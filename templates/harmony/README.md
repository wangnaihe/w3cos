# HarmonyOS NEXT shell template (P0)

This is the generic ArkUI `XComponent` host for the future W3COS OpenHarmony
runtime. It intentionally does not embed an Android APK or use an Android
compatibility layer.

## Prerequisites

- DevEco Studio with a HarmonyOS NEXT / OpenHarmony SDK
- `ohpm` and `hvigorw`
- Rust target `aarch64-unknown-linux-ohos`
- `OHOS_SDK_HOME` or `local.properties`

## Current boundary

The ArkUI shell and native surface contract are present. `w3cos mobile build
--platform harmony` remains fail-closed until W3COS can drive its renderer and
input loop from `OHNativeWindow`. This prevents an empty HAP from being reported
as a working native renderer.

The host contract is:

- ArkUI owns application lifecycle and safe-area chrome.
- `XComponent` owns the content surface.
- Native code forwards surface creation, size changes, destruction, and touch
  input to `libw3cos_mobile_app.so`.
- Product UI still comes from the manifest `entry`; no Harmony-only business
  page is allowed.
