# W3C OS Roadmap

Last replanned: **2026-07-23**
Baseline: `main` @ `ae6e458`

## North Star

Compile a standards-oriented Web application — TypeScript/JavaScript, DOM, CSS,
and npm dependencies — into a native desktop or mobile application without a
browser or JavaScript VM.

The primary compatibility target is the **formal ESM/React application path**.
A Rust module existing in `w3cos-runtime` is necessary, but it does not make a
Web API complete until compiled application code can call the standard
JavaScript surface.

## Definition of Done

An API is marked complete only when all applicable layers pass:

1. **Engine** — the generic Rust implementation exists.
2. **Web surface** — the standard JavaScript global, constructor, properties,
   events, and errors are exposed through the ESM/jsdom path.
3. **Conformance** — behavior tests execute compiled JavaScript, not only direct
   Rust calls.
4. **Platform** — required desktop/mobile adapters pass on their target
   platform.
5. **Downstream gate** — at least one real application exercises the API when
   the capability is product-critical.

Status:

- ✅ complete under the definition above
- 🚧 engine exists, but the Web surface or a platform adapter is incomplete
- 📋 planned
- ⛔ intentionally unsupported

## Release Order

| Release | Outcome | Exit gate |
|---------|---------|-----------|
| **R0** | Trustworthy `main` | Required tests are green and API status cannot overclaim Rust-only modules |
| **R1** | Native Web App P0 | Formal React app has localization, network streams, voice, location, and media capture |
| **R2** | Web Platform Facade | Common browser constructors and events work from compiled ESM |
| **R3** | Mobile Production Runtime | Android/iOS touch, IME, viewport, lifecycle, and device validation pass |
| **R4** | npm Compatibility | Package support is driven by repeatable compatibility gates |
| **R5** | W3C OS Distribution | Shell, package lifecycle, permissions, updates, and system agent are production-ready |

---

## R0 — Restore a Trustworthy Main Branch

No new API should be declared complete while the corresponding conformance
suite is red.

### Green baseline

- [x] Fix `w3cos-runtime --test w3c_feature_matrix`
  `dom_to_component_tree_smoke`.
- [x] Fix `w3cos-compiler` `generated_bundle_runs_jsdom_globals`.
- [ ] Make the required compiler/runtime suites part of the default CI gate.
- [ ] Add a compiled-JavaScript API-surface test that checks `typeof`,
  constructor calls, callbacks/events, and failure behavior.
- [x] Add a `.d.ts`-driven `web-api-skeleton` tool that generates reviewable
  Rust facades with named `todo!()` placeholders without wiring them into the
  production runtime.
- [ ] Separate test labels for:
  - direct Rust engine API;
  - ESM/JavaScript Web surface;
  - desktop integration;
  - Android/iOS integration.

### Status integrity

- [ ] Generate or maintain one Web API capability matrix with the columns
  `engine`, `esm_surface`, `desktop`, `android`, `ios`, and `conformance`.
- [ ] Remove DONE claims where only a Rust module exists.
- [ ] Keep roadmap, README capability claims, and mobile documentation aligned
  in the same change that lands an API.

**R0 exit:** required CI is green and every claimed Web API has an ESM-level
test.

---

## R1 — Native Web App P0

These APIs block the current formal downstream React application and therefore
precede ecosystem breadth or migration tooling.

### R1.1 Internationalization

- [x] Implement the initial `Intl.NumberFormat` application profile.
  - locale-aware decimal/grouping;
  - currency style and ISO 4217 currency codes;
  - stable behavior for unsupported locales.
- [x] Implement the initial `Intl.DateTimeFormat` application profile.
  - locale-aware date/time fields;
  - UTC, fixed-offset, and selected non-DST IANA timezones;
  - deterministic invalid-date and invalid-timezone behavior.
- [x] Expose both constructors through the ESM global `Intl`.
- [x] Add engine and compiled-JavaScript tests for `zh-CN` currency, `en-US`
  decimal formatting, UTC input, and `Asia/Shanghai`.
- [ ] Add locale data and DST-aware IANA timezone transitions beyond the
  initial application profile.

### R1.2 Network primitives

- [x] Expose the existing WebSocket engine as the standard `WebSocket`
  constructor.
  - `CONNECTING`, `OPEN`, `CLOSING`, `CLOSED`;
  - `onopen`, `onmessage`, `onerror`, `onclose`;
  - text/binary send and close code/reason;
  - event-loop polling and cleanup.
- [x] Complete the initial Fetch companion surface used by application code:
  `Request`, `Response`, `Headers`, and `AbortController`.
- [ ] Support request cancellation and timeout cleanup without leaking native
  work.
- [x] Add an ESM integration test against a local WebSocket fixture.
- [x] Add an ESM integration test against a local HTTP fixture for Fetch and
  its companion constructors.

### R1.3 Web Speech

- [x] iOS native speech engine prototype (`SFSpeechRecognizer`).
- [ ] Expose `window.SpeechRecognition` and the compatibility alias
  `window.webkitSpeechRecognition`.
- [ ] Implement browser-shaped results, alternatives, confidence, finality,
  lifecycle events, and error events.
- [ ] Add Android speech recognition adapter.
- [ ] Define desktop behavior explicitly: supported adapter or standards-shaped
  `not-supported` failure.
- [ ] Validate permissions, denial, restart, continuous mode, and cancellation
  on physical iOS and Android devices.

### R1.4 Geolocation

- [ ] Implement `navigator.geolocation`.
- [ ] Support `getCurrentPosition`, `watchPosition`, and `clearWatch`.
- [ ] Implement timeout, maximum age, accuracy fields, permission denial, and
  platform-disabled errors.
- [ ] Add iOS Core Location and Android location adapters with manifest/plist
  generation.

### R1.5 MediaDevices

- [ ] Implement `navigator.mediaDevices`.
- [ ] Implement `getUserMedia()` for camera and microphone.
- [ ] Provide `MediaStream` and track lifecycle sufficient for preview,
  capture, stop, and permission handling.
- [ ] Add photo/evidence capture without product-specific native modules.
- [ ] Validate camera/microphone denial and interruption on physical devices.

### R1.6 Formal downstream conformance

- [ ] Compile the formal downstream Vite production graph without a parallel
  native UI or bootstrap.
- [ ] Pass localization formatting.
- [ ] Pass authenticated Fetch and IndexedDB/local-first startup.
- [ ] Pass live WebSocket capture stream.
- [ ] Pass voice capability detection and transcript delivery.
- [ ] Pass location and camera evidence flows.

**R1 exit:** the formal React application completes these flows on native
desktop and the applicable mobile targets using standard Web APIs.

---

## R2 — Web Platform Facade

R2 turns existing engine modules and partial shims into coherent browser-facing
APIs. Work is ordered by common npm usage, not by number of Rust modules.

### R2.1 Binary data and files

- [ ] Expose working `TextEncoder` alongside `TextDecoder`.
- [ ] Implement `ArrayBuffer`, `DataView`, and typed-array buffer/view
  semantics.
- [ ] Implement `Blob`, `File`, and `FileReader`.
- [ ] Implement `FormData`, including Fetch request integration.
- [ ] Implement `ImageData`, `Path2D`, and `OffscreenCanvas` where supported by
  the existing Canvas engine.

### R2.2 Events and DOM constructors

- [ ] Implement callable `Event`, `CustomEvent`, and `EventTarget`.
- [ ] Expose DOM constructors with useful identity and `instanceof` behavior:
  `Node`, `Element`, `HTMLElement`, common HTML elements, `Range`, and
  `Selection`.
- [ ] Expose standard event subclasses: keyboard, pointer, input, clipboard,
  drag, touch, animation, and transition events.
- [ ] Replace silent empty-object fallbacks with standards-shaped exceptions or
  explicit unsupported errors.

### R2.3 Observers and background work

- [x] ResizeObserver engine and compiler special case.
- [ ] Expose standard `ResizeObserver` constructor behavior through ESM.
- [ ] Expose `MutationObserver` and `IntersectionObserver` through ESM.
- [ ] Implement `PerformanceObserver`.
- [ ] Expose the Worker engine as `Worker`, `SharedWorker`, `MessageChannel`,
  `MessagePort`, and structured message events.

### R2.4 Remaining network/browser services

- [ ] Expose `EventSource`.
- [ ] Decide whether `XMLHttpRequest` is a compatibility shim over Fetch or an
  explicitly unsupported legacy API.
- [ ] Expose the Notifications API on supported desktop/mobile platforms.
- [ ] Complete Clipboard item/data-transfer APIs beyond text-only clipboard.
- [ ] Add secure randomness backed by the OS; do not use the current
  deterministic fallback for security-sensitive APIs.

### R2.5 DOM, viewport, and display

- [ ] Implement Fullscreen API and `fullscreenchange` lifecycle.
- [ ] Implement Screen Orientation state, `lock()`, `unlock()`, and events.
- [ ] Make `VisualViewport` geometry and resize/scroll listeners live.
- [ ] Complete computed style beyond inline-style reflection.
- [ ] Complete SVG namespace/rendering support required by application gates.
- [ ] Define cookie behavior: real store with policy or explicit unsupported
  errors instead of inert assignment.

**R2 exit:** supported APIs behave consistently when reached from compiled ESM;
Rust-only modules are no longer advertised as browser APIs.

---

## R3 — Mobile Production Runtime

### R3.1 Existing foundation

- [x] `w3cos-mobile` crate and generic mobile demo.
- [x] Android/iOS project templates.
- [x] `w3cos mobile init`.
- [x] `w3cos mobile build` for Android and iOS simulator artifacts.
- [x] `w3cos mobile dev` with debug DevTools plumbing.
- [x] Safe-area inset storage and native setter.
- [x] HarmonyOS ArkUI/XComponent shell scaffold with fail-closed build.

### R3.2 Touch and pointer input

- [ ] Replace `TouchEvent::dispatch()` no-op with runtime hit-test dispatch.
- [ ] Map Android MotionEvent and iOS touch input to Pointer Events and Touch
  Events.
- [ ] Support multi-touch identity, cancel, capture, pressure where available,
  scrolling arbitration, and gesture interruption.
- [ ] Report real `navigator.maxTouchPoints`.

### R3.3 IME and editable text

- [ ] Connect native focus to `<input>`, `<textarea>`, and contenteditable.
- [ ] Implement UTF-8 commit/delete, caret geometry, selection ranges, and
  keyboard viewport resize.
- [ ] Complete `beforeinput`, `input`, and `composition*` lifecycle with marked
  text.
- [ ] Implement `inputmode`, `enterkeyhint`, secure input, and
  EditContext-compatible geometry.
- [ ] Add CJK, emoji, RTL, paste, autocorrect, and hardware-keyboard device
  tests.

### R3.4 Immersive viewport and shell

- [ ] Implement edge-to-edge viewport and native system-bar integration.
- [ ] Support `viewport-fit=cover`, CSS safe-area `env()`, and
  `svh`/`lvh`/`dvh`.
- [ ] Implement keyboard insets through live `VisualViewport`.
- [ ] Replace RN-compat `StatusBar` and `ActivityIndicator` placeholders.
- [ ] Add generic mobile-shell chrome hooks without application-specific UI.

### R3.5 Platform completion

- [ ] Run Android rendering on the real NativeActivity surface without desktop
  fallback assumptions.
- [ ] Produce and validate physical-device Android APKs.
- [ ] Add iOS device archive/signing pipeline in addition to simulator builds.
- [ ] Add lifecycle, background/foreground, rotation, memory pressure, and
  interruption tests.
- [ ] Implement HarmonyOS OHNativeWindow rendering, input, lifecycle, IME, and
  safe-area adapters before enabling Harmony builds.

**R3 exit:** one formal Web application passes the same input, layout,
local-first, and device-capability flows on physical Android and iOS devices.

---

## R4 — npm and JavaScript Compatibility

Package compatibility is validation-based. W3COS does not add
framework-specific runtime paths to make individual packages pass.

### R4.1 JavaScript semantics

- [ ] Complete RegExp semantics required by package gates.
- [ ] Implement `BigInt`.
- [ ] Implement real `WeakMap`, `WeakSet`, `WeakRef`, and
  `FinalizationRegistry` semantics where feasible.
- [ ] Implement `ArrayBuffer`, shared-memory, and Atomics semantics selected by
  the supported security model.
- [ ] Complete URI encode/decode globals.
- [ ] Remove reachable `todo!()` and silent unsupported-expression lowering
  from production compiler paths.

### R4.2 Package gates

- [x] Official React and react-dom formal application gate.
- [x] Monaco/CodeMirror-oriented compiler and DOM milestones.
- [ ] Define a versioned compatibility suite for representative package
  classes:
  - pure logic;
  - state/data;
  - UI/component;
  - editor/visualization;
  - networking/storage.
- [ ] Publish tested package versions and the Web APIs each gate requires.
- [ ] Add CSS/Web API failures as generic platform issues, not package-specific
  hard-coded bridges.
- [ ] Claim broad npm compatibility only after the selected suite passes in CI.

### R4.3 Migration tooling

- [ ] React Native application analysis/migration command.
- [ ] Electron application analysis and standards-oriented migration report.
- [ ] Keep runtime compatibility work separate from source migration tooling.

**R4 exit:** package support is reproducible, versioned, and explained by
generic JavaScript/Web-platform coverage.

---

## R5 — W3C OS Distribution

### Completed foundation

- [x] Desktop shell and multi-window foundations.
- [x] Buildroot boot pipeline and QEMU tooling.
- [x] Bootable ISO release workflow.
- [x] AI Bridge DOM/a11y/query/click/type/screenshot foundation.
- [x] File system, process, PTY, IPC, menu, and dialog engine modules.

### Remaining

- [ ] Capability-based application permissions and user-facing consent.
- [ ] Signed application package format, installer, updater, and rollback.
- [ ] Package registry/store and dependency policy.
- [ ] AI system agent with privileged APIs and auditable authorization.
- [ ] Multi-device sync protocol with identity, encryption, and conflict
  handling.
- [ ] Recovery, crash reporting, diagnostics, and upgrade compatibility.
- [ ] Hardware/driver support matrix and real-device release qualification.

---

## Intentionally Unsupported or Deferred

- ⛔ `eval()` and arbitrary runtime code generation: incompatible with the AOT
  and security model.
- ⛔ Writable `innerHTML` as an unrestricted script execution path. A safe,
  inert markup subset may be supported for compatibility.
- ⛔ Runtime CommonJS `require()`: dependencies must be statically resolved or
  bundled.
- ⛔ Service Workers until an offline/background execution and permission model
  is designed. Local-first storage does not require Service Workers.
- ⛔ WebRTC until a real product/package gate justifies the media, networking,
  permission, and security surface.
- 📋 Dynamic `import()` may be considered as statically known AOT chunks; fully
  arbitrary runtime module loading is out of scope.
- 📋 Escape-analysis optimization is performance work and must not precede
  correctness or Web API conformance.

## Change Policy

When completing a roadmap item:

1. land the generic implementation;
2. expose the standard ESM/Web surface;
3. add conformance tests at the appropriate layers;
4. validate required platforms;
5. update capability claims and this roadmap in the same change.

Downstream applications may supply conformance cases, but product names,
business semantics, and application-specific native modules do not belong in
W3COS.
