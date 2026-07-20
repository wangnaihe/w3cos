# Android shell template (M2)

Hosts `libw3cos_mobile_app.so` via **NativeActivity** (`android.app.lib_name=w3cos_mobile_app`).

## Prerequisites

- Android Studio / SDK 34+
- NDK 27+ (SDK Manager)
- Android 8.0 / API 26+ device with Vulkan 1.0
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

## Rendering backend

The `skia` build defaults to direct Skia Ganesh + Vulkan presentation. W3COS
creates a `VK_KHR_android_surface` from the NativeActivity `ANativeWindow` and
presents its swapchain without a CPU readback. `W3COS_RENDERER=gpu` and
`W3COS_RENDERER=cpu` remain explicit diagnostic fallbacks.

## Customize

- `applicationId` in `app/build.gradle.kts` ↔ `bundle_id` in `w3cos.app.json`
