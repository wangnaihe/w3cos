# W3C OS — Mobile (Android / iOS)

RN-like **shell + AOT app** for iOS and Android. **Generic platform only** — product apps are built in downstream repos.

## Layout

```
crates/w3cos-mobile/       # touch, safe area, lifecycle, Android JNI
templates/android/         # Gradle shell (W3cosActivity)
templates/shared/          # starter app.tsx + w3cos.app.json
examples/mobile-demo/      # generic CI / docs example
```

## Quick start (desktop dev)

Mobile apps compile with the same TSX pipeline as desktop:

```bash
w3cos build examples/mobile-demo/app.tsx -o mobile-demo --release
./mobile-demo
```

## Scaffold a mobile project

```bash
w3cos mobile init MyApp --platform android
cd MyApp
w3cos build app.tsx -o myapp --release   # desktop test
# Android APK: see templates/android/README.md (M1 pipeline)
```

## Android APK (M1 skeleton)

1. Install [Rust NDK](https://github.com/bbqsrc/cargo-ndk): `cargo install cargo-ndk`
2. Build `w3cos-mobile` + your app as `cdylib` for `aarch64-linux-android`
3. Copy `.so` into `templates/android/app/src/main/jniLibs/`
4. Open `templates/android/` in Android Studio → Run on emulator

Full automation (`w3cos mobile build`) lands in M2.

## iOS

Planned (M5). `templates/ios/` placeholder TBD.

## Status

| Milestone | Item | Status |
|-----------|------|--------|
| M1 | `w3cos-mobile` crate | 🚧 skeleton |
| M1 | `examples/mobile-demo` | ✅ |
| M1 | `templates/android` | 🚧 skeleton |
| M2 | `w3cos mobile build` → APK | 📋 |
| M3 | `w3cos-mobile-shell` chrome | 📋 |
| M4 | W3C Geolocation / getUserMedia | 📋 |
| M5 | iOS template | 📋 |

See also: [ROADMAP.md](../ROADMAP.md) Phase 3 cross-compilation.
