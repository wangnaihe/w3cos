# W3C OS — Mobile (Android / iOS)

RN-like **shell + AOT app** for iOS and Android. **Generic platform only** — product apps live in downstream repos (e.g. aiNativeTms `apps/logidesk-native/`).

## Quick start

```bash
# Desktop smoke test (same TSX pipeline)
w3cos build examples/mobile-demo/app.tsx -o mobile-demo --release
./mobile-demo

# Scaffold
w3cos mobile init MyApp --platform both
cd MyApp
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

## Status

| Milestone | Item | Status |
|-----------|------|--------|
| M1 | `w3cos-mobile` crate | ✅ skeleton |
| M2 | `w3cos mobile build` | ✅ android + ios lib + shell |
| M3 | `w3cos-mobile-shell` chrome | 📋 |
| M4 | W3C Geolocation / getUserMedia | 📋 |
| M5 | Device IPA + Play Store pipeline | 📋 |

## Downstream (aiNativeTms)

```bash
cd apps/logidesk-native
./build-mobile.sh both    # after w3cos CLI built
```

Bump `vendor/w3cos` submodule when mobile APIs change.
