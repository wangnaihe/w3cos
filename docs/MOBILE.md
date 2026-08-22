# W3C OS — Mobile (Android / iOS / HarmonyOS)

Native **shell + AOT app** targets for Android, iOS, and HarmonyOS. **Generic platform only** —
product applications and business-specific integrations live in downstream
repositories.

## Quick start

```bash
# Desktop smoke test (same TSX pipeline)
w3cos build examples/mobile-demo/app.tsx -o mobile-demo --release
./mobile-demo

# Scaffold
w3cos mobile init MyApp --platform all
cd MyApp
# `both` intentionally means Android + iOS.
w3cos mobile build --platform both --release
```

### Packaged document origin

Packaged modules keep their `w3cos://` module identity, but applications that
use browser-relative network URLs can declare an HTTP(S) document base in
`w3cos.app.json`:

```json
{
  "entry": "app.tsx",
  "document_base_url": "https://app.example.com/"
}
```

W3COS configures `window.location`, relative `fetch()`/resource resolution,
cookies, CORS and same-origin checks from that URL before application code
runs. The value must be an absolute `http` or `https` URL without credentials,
a query or a fragment. `W3COS_DOCUMENT_BASE_URL` provides a build-time override
for local and CI validation without changing the application manifest.

## Android

- **Shell:** `templates/android/` — NativeActivity + `libw3cos_mobile_app.so`
- **Build:** `w3cos mobile build --platform android`
- **Needs:** Android SDK 34+, NDK, `cargo install cargo-ndk`, `rustup target add aarch64-linux-android`

See [templates/android/README.md](../templates/android/README.md).

## iOS

- **Shell:** `templates/ios/` — generated app bundle + Xcode packaging shell
- **Build:** `w3cos mobile build --platform ios`
- **Needs:** Full Xcode, `rustup target add aarch64-apple-ios-sim`

See [templates/ios/README.md](../templates/ios/README.md).

The current iOS runtime bridge includes native first-responder text input,
marked-text/composition polling, keyboard-layout-guide viewport insets, and a
document picker for `<input type="file">`. These are implementation milestones;
physical-device IME, accessibility, lifecycle, archive/signing, and App Store
validation remain release gates.

## HarmonyOS

- **Shell:** `templates/harmony/` — ArkUI + XComponent scaffold
- **Build:** `w3cos mobile build --platform harmony`
- **Needs:** DevEco/OpenHarmony SDK and native toolchain configuration

See [templates/harmony/README.md](../templates/harmony/README.md). The build is
fail-closed when the SDK, native surface, or generated ABI is unavailable. A
successful scaffold/build does not yet prove real-device rendering, IME,
lifecycle, or safe-area behavior.

## Generated-code and rebuild model

Mobile AOT builds use the same compiler and DOM/Web API semantics as desktop.
Generated ESM modules are split into stable `src/esm_bundle/m*.rs` files and
unchanged files are not rewritten, so Cargo can reuse incremental work.

Set `W3COS_PRESERVE_GENERATED_SOURCES=1` only for controlled fast-build
workflows that intentionally preserve the generated source directory. Without
it, the build regenerates that directory from the application graph.

Generated iOS and Android applications disable the runtime's default feature
set. The normal mobile closure contains only the Skia presenter and its current
window owner; Vello/WGPU and advanced graphics/media groups are restored only
when the compiled application references those capabilities. Debug builds keep
LTO off and incremental code generation on. `--release` uses the generated
size-oriented profile (`opt-level=z`, fat LTO, one codegen unit and symbol
stripping); do not use it for the daily edit loop.

The ordinary raster closure also excludes the AVIF encoder. In `image 0.25`,
the `avif` feature is backed by `ravif`/`rav1e` and does not decode AVIF; W3COS
does not currently expose an AVIF encoding API. Native embeddings that provide
one must opt in explicitly with the `w3cos-runtime/image-avif-encode` feature.
AVIF decoding remains a separate native-decoder capability and must not be
inferred from this encoder feature.

## Reproducible iOS build and size evidence

The simulator artifact is diagnostic. Build an unsigned App Store device slice
and emit machine-readable timing and executable-size evidence with:

```bash
w3cos mobile build . \
  --platform ios \
  --ios-target device \
  --release \
  --report target/mobile-ios-device.json
```

The JSON separates source generation from Native build/package time and records
the exact `aarch64-apple-ios` executable bytes with `device_slice: true`. Signing,
archiving and App Store validation remain downstream release gates.

For comparable cold, no-change warm and one-source-change measurements, use a
dedicated target directory instead of deleting the daily incremental cache:

```bash
W3COS_PRESERVE_GENERATED_SOURCES=1 \
CARGO_TARGET_DIR="$PWD/target/mobile-benchmark" \
  w3cos mobile build . --platform ios --ios-target device --release \
  --report target/mobile-cold.json

# Run the same command again with mobile-warm.json, then change one real
# application source and run it once more with mobile-incremental.json.
```

Enforce the standalone one-renderer runtime budget against that same device
executable with the existing size auditor:

```bash
cargo build -p w3cos-size-audit --profile release-size
target/release-size/w3cos-size-audit --profile runtime \
  --component executable=path/to/application/ios/W3cosApp.app/W3cosApp \
  --output target/mobile-ios-size.json
```

## Status

| Milestone | Item | Status |
|-----------|------|--------|
| M1 | Shared mobile crate, manifest, and demo | ✅ implemented |
| M2 | Android/iOS init, build, and dev shell paths | ✅ implemented; device gates remain |
| M3 | HarmonyOS ArkUI/XComponent scaffold and fail-closed build | 🚧 scaffold/build path; device runtime open |
| M4 | Stable incremental AOT module generation | ✅ implemented and covered by codegen tests |
| M5 | Feature-minimal mobile linkage and iOS build/size evidence | ✅ generated profiles, device slice and JSON evidence entry |
| M6 | Native input and viewport bridges | 🚧 iOS text/file/keyboard path; Android and device parity open |
| M7 | W3C Geolocation / getUserMedia host adapters | 📋 planned |
| M8 | Signed device/App Store/Play Store pipelines | 📋 planned |

The authoritative open gates are maintained in
[ROADMAP.md](../ROADMAP.md#r3--mobile-production-runtime); per-API platform
status is in [WEB_API_CAPABILITIES.md](../WEB_API_CAPABILITIES.md).

## Downstream integration

```bash
cd path/to/application
w3cos mobile build --platform both --release
```

Pin and update the application's W3COS dependency when mobile APIs change.
