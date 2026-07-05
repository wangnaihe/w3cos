# Android shell template (M1 skeleton)

Gradle project that hosts the W3C OS native library — similar to React Native's `android/` folder.

## Prerequisites

- Android Studio / SDK 34+
- NDK (via SDK Manager)
- `cargo-ndk` for building Rust `cdylib`

## Structure

```
android/
├── app/
│   ├── build.gradle.kts
│   └── src/main/
│       ├── AndroidManifest.xml
│       └── java/com/example/w3cos/W3cosActivity.kt
├── build.gradle.kts
└── settings.gradle.kts
```

## Build flow (manual M1)

1. Build your app + `w3cos-mobile` for Android:

```bash
cargo ndk -t arm64-v8a build --release -p w3cos-mobile
```

2. Copy `target/aarch64-linux-android/release/libw3cos_mobile.so` to:

```
app/src/main/jniLibs/arm64-v8a/libw3cos_mobile.so
```

3. Open this directory in Android Studio → Run on emulator.

## M2

`w3cos mobile build` will automate steps 1–3 and produce `app-release.apk`.

## Customization

- Change `applicationId` in `app/build.gradle.kts` to match `w3cos.app.json` `bundle_id`
- Replace `com.example.w3cos` package if needed (update manifest + Kotlin path)
