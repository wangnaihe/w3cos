# w3cos-mobile

Mobile platform layer for W3C OS — touch, safe area, lifecycle, and Android/iOS shell integration.

**Generic platform code only.** Product apps live in downstream repositories.

## Desktop dev

```rust
use w3cos_mobile::run_mobile_app;

fn main() {
    run_mobile_app(build_ui).expect("app crashed");
}
```

On non-Android targets, `run_mobile_app` calls `w3cos_runtime::run_app` (same as `w3cos build`).

## Android (M1 skeleton)

1. Build app + this crate as `cdylib` with `cargo-ndk`
2. Copy `libw3cos_mobile.so` into `templates/android/` Gradle project
3. `W3cosActivity` loads the library and calls `w3cos_mobile_run()`

See [docs/MOBILE.md](../../docs/MOBILE.md) and [examples/mobile-demo](../../examples/mobile-demo/).

## Status

| Feature | Status |
|---------|--------|
| `w3cos.app.json` manifest | ✅ parse |
| Desktop dev fallback | ✅ |
| Android JNI entry | 🚧 skeleton |
| Touch → DOM | 🚧 stub |
| iOS host | 📋 planned |
