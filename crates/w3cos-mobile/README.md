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

## SpeechRecognition

The declarative runtime action mirrors the Web Speech API result model:

```tsx
const transcript = signal("")
const isFinal = signal(0)
const confidence = signal(0)
const status = signal(0)

<Button onClick="speech:start:transcript:isFinal:confidence:status:zh-CN:1:1:1">
  Start
</Button>
<Button onClick="speech:stop">Stop</Button>
<Button onClick="speech:stop:panelOpen:0">Stop and close</Button>
<Text>{transcript}</Text>
```

The last three start flags are `processLocally`, `continuous`, and
`interimResults`. iOS uses `SFSpeechRecognizer` + `AVAudioEngine`; when
`processLocally` is enabled it requires `supportsOnDeviceRecognition` and never
silently falls back to a remote recognizer. Declare
`"permissions": ["speech-recognition"]` in `w3cos.app.json` so the generated
iOS bundle contains microphone and speech usage descriptions.

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
| iOS host | ✅ winit/UIKit runtime |
| iOS on-device `SpeechRecognition` | 🚧 zh-CN adapter implemented; device validation required |
