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

## Android

- **Shell:** `templates/android/` — NativeActivity + `libw3cos_mobile_app.so`
- **Build:** `w3cos mobile build --platform android`
- **Needs:** Android SDK 34+, NDK, `cargo install cargo-ndk`, `rustup target add aarch64-linux-android`

See [templates/android/README.md](../templates/android/README.md).

## iOS

- **Shell:** `templates/ios/` — Xcode + `libw3cos_mobile_app.a`
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

## Status

| Milestone | Item | Status |
|-----------|------|--------|
| M1 | Shared mobile crate, manifest, and demo | ✅ implemented |
| M2 | Android/iOS init, build, and dev shell paths | ✅ implemented; device gates remain |
| M3 | HarmonyOS ArkUI/XComponent scaffold and fail-closed build | 🚧 scaffold/build path; device runtime open |
| M4 | Stable incremental AOT module generation | ✅ implemented and covered by codegen tests |
| M5 | Native input and viewport bridges | 🚧 iOS text/file/keyboard path; Android and device parity open |
| M6 | W3C Geolocation / getUserMedia host adapters | 📋 planned |
| M7 | Signed device/App Store/Play Store pipelines | 📋 planned |

The authoritative open gates are maintained in
[ROADMAP.md](../ROADMAP.md#r3--mobile-production-runtime); per-API platform
status is in [WEB_API_CAPABILITIES.md](../WEB_API_CAPABILITIES.md).

## Downstream integration

```bash
cd path/to/application
w3cos mobile build --platform both --release
```

Pin and update the application's W3COS dependency when mobile APIs change.
