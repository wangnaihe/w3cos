# Android shell template (M2)

Hosts `libw3cos_mobile_app.so` via **NativeActivity** (`android.app.lib_name=w3cos_mobile_app`).

## Prerequisites

- Android Studio / SDK 34+
- NDK (SDK Manager)
- `cargo install cargo-ndk`
- `rustup target add aarch64-linux-android`

## Build (automated)

From your mobile project root (contains `app.tsx` + `android/`):

```bash
w3cos mobile build --platform android --release
```

This compiles TSX → Rust cdylib, runs `cargo ndk`, copies to `jniLibs/`, and runs `gradlew assembleRelease` when wrapper exists.

## Build (Android Studio)

1. `w3cos mobile build --platform android` (native lib only)
2. Open `android/` in Android Studio → Run on emulator

## Customize

- `applicationId` in `app/build.gradle.kts` ↔ `bundle_id` in `w3cos.app.json`
