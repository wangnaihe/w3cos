# HarmonyOS NEXT native template

This is the generic ArkUI `XComponent` host for the W3COS OpenHarmony runtime.
It intentionally does not embed an Android APK or use an Android compatibility
layer.

## Prerequisites

- DevEco Studio with a HarmonyOS NEXT / OpenHarmony SDK
- `ohpm` and `hvigorw`
- Rust target `aarch64-unknown-linux-ohos`
- `OHOS_SDK_HOME`, `OHOS_NDK_HOME`, `ohos_sdk_native`, or a DevEco-generated
  `local.properties` containing `sdk.dir`

## Build

```bash
w3cos mobile build --platform harmony
```

The build cross-compiles `libw3cos_mobile_app.so`, packages it under
`entry/libs/arm64-v8a`, and invokes `hvigorw assembleHap`. It fails closed when
the OHOS SDK, Rust target, native library, or HAP toolchain is absent.

The host contract is:

- ArkUI owns application lifecycle and safe-area chrome.
- `XComponent` owns the content surface.
- Native code forwards surface creation, size changes, destruction, and touch
  input to `libw3cos_mobile_app.so`.
- W3COS creates EGL/GLES3 on `OHNativeWindow` and replays the shared component
  tree through Skia Ganesh.
- Product UI still comes from the manifest `entry`; no Harmony-only business
  page is allowed.

Keyboard/IME composition, ArkUI safe-area insets, accessibility semantics,
signed-HAP configuration, and device certification remain follow-up work.
