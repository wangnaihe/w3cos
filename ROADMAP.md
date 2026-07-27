# W3C OS Roadmap

Last replanned: **2026-07-25**
Baseline: `main` @ `ae6e458`

## North Star

Compile a standards-oriented Web application — TypeScript/JavaScript, DOM, CSS,
and npm dependencies — into a native desktop or mobile application without a
browser or JavaScript VM.

The primary compatibility target is the **formal ESM application path**.
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
| **R1** | Native Web App P0 | Formal app has localization, network streams, voice, location, and media capture |
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
- [x] Add a compiled-JavaScript API-surface test that checks `typeof`,
  constructor calls, callbacks/events, and failure behavior.
- [x] Add a `.d.ts`-driven `web-api-skeleton` tool that generates reviewable
  Rust facades with named `todo!()` placeholders without wiring them into the
  production runtime.
- [x] Add `web-api-audit`, which launches a local headless Chromium, inventories
  live Web API globals/prototypes, compares them with the instantiated w3cos
  window, and emits human-readable or JSON differences with an optional CI
  failure gate.
- [x] Use Chrome 150 as a live surface baseline: w3cos exposes 1057 runtime
  globals and covers every audited Web API global with zero missing prototype
  or static members. The audit explicitly excludes five ECMAScript language
  built-ins (`Iterator`, Explicit Resource Management and `Temporal`) from the
  browser API inventory.
- [x] Add the `BarcodeDetector` compatibility surface with validated format
  options, Promise-shaped queries/detection, and truthful empty results plus a
  warning until a native image-analysis adapter is available.
- [x] Align Screen, ScreenOrientation and Window Management identities with
  live single-display metrics and a warning-backed `getScreenDetails()` facade.
- [x] Add Compute Pressure observation with standard records, queued callback
  delivery and a host CPU-pressure injection boundary.
- [x] Expose the URL Fragment Text Directives identity and stable document
  entry point, with unsupported parsing/scroll/highlight behavior warned.
- [x] Complete the Navigation event destination surface with the standard
  `NavigationDestination` identity, metadata and state accessor.
- [x] Extend Performance Timeline with long-task records, task attribution,
  buffered observer delivery and a host scheduler injection boundary.
- [x] Make document visibility host-updatable and publish standard
  `VisibilityStateEntry` records through Performance Timeline.
- [x] Add Chrome-shaped Element/Event/Paint/Resource/Navigation/Script/Long
  Animation Frame/LCP/Layout Shift timing identities, their exact inheritance
  and nested ServerTiming/TimingConfidence/LayoutShiftAttribution brands, plus
  a common host-entry injection boundary and buffered observer delivery.
- [x] Bind the existing Canvas 2D engine to standard
  `CanvasRenderingContext2D`, `CanvasGradient`, `CanvasPattern`, `TextMetrics`
  and `ImageBitmap` identities; add stable bitmap-renderer contexts and
  canvas-capture tracks, with explicit warnings for renderer/media effects
  that still require host adapters.
- [x] Brand enumerated microphone/camera devices as `InputDeviceInfo`, expose
  the WebCrypto `CryptoKey` record identity without weakening unsupported
  cryptography failures, and retain constructible legacy `DOMError`.
- [x] Normalize accessor-backed public properties in the Chrome audit, expose
  the live `window.document`/`devicePixelRatio` entries, and complete Web
  Speech synthesis identities with queue, pause/resume/cancel state,
  utterance events and warning-backed native audio output.
- [x] Add DOM-backed XPath evaluation and layout-backed `CaretPosition`,
  `caretRangeFromPoint()` and element hit-testing compatibility surfaces.
- [x] Extend Generic Sensor with gravity, linear acceleration and
  absolute/relative orientation sensors, including permission-aware lifecycle,
  quaternion/matrix state and host injection; add stateful Web Speech grammar
  lists and contextual recognition phrases.
- [x] Connect `StylePropertyMap`/`StylePropertyMapReadOnly` to live inline and
  computed element styles, including Typed OM parsing, iteration and mutable
  set/append/delete/clear behavior.
- [x] Add structured CSP/integrity report-body identities and JSON snapshots,
  warning-backed Gamepad haptic completion, and nested BFCache
  `NotRestoredReasons` diagnostic records.
- [x] Complete the legacy/core compatibility family (`External`, `Origin`,
  `FeaturePolicy`, `QuotaExceededError`, `WebSocketError`, `TimeRanges`,
  `MediaError`, `PictureInPictureWindow`, `RadioNodeList`, `ReportBody` and
  the `WebKitMutationObserver` alias), with neutral values and explicit host
  warnings where browser integration is unavailable.
- [x] Add mutable text-track/cue lists, constructible `VTTCue`, media-element
  `addTextTrack()` integration and neutral `VideoPlaybackQuality` records.
- [x] Expose byte-stream controller/BYOB identities and compatible byte-stream
  reader locking/delivery. Supplied BYOB views are not filled yet and emit a
  warning instead of silently claiming zero-copy semantics.
- [x] Back File System Access handles and `navigator.storage.getDirectory()`
  with a runtime-local OPFS directory, including file read/write streams,
  directory creation/traversal/removal/resolve and permission-compatible
  results. Native picker UI, quotas and cross-process observation remain
  warning-backed host-adapter work.
- [x] Add Web Animations timing/effect/playback identities and state, wire
  `Element.animate()` plus element/document `getAnimations()`, and retain an
  explicit warning until arbitrary keyframes feed the native compositor.
- [x] Complete UA Client Hints neutral/high-entropy results, media
  `RemotePlayback`, the dedicated Offscreen Canvas 2D context identity and
  legacy `WebKitCSSMatrix` alias with browser-compatible failure behavior.
- [x] Add MathML namespace element identity, the standard `Window` constructor,
  WebAuthn/OTP response and credential types with conservative authenticator
  capability results, and warning-backed media recording/image/display-capture
  lifecycles.
- [x] Add in-memory Media Source/SourceBuffer lifecycle and byte storage,
  Document Picture-in-Picture rejection semantics, Payment Request capability
  queries, Push discovery, and Service Worker background companion managers.
- [x] Add stateful XSLT parameters plus an explicit identity-transform fallback,
  and byte-preserving `EncodedAudioChunk`/`EncodedVideoChunk` plus
  `VideoColorSpace` WebCodecs data identities. Add `AudioData` construction,
  allocation, copying, cloning and independent close lifecycle, plus raw
  `VideoFrame` geometry, plane-copy Promise, metadata, clone and close.
- [x] Connect `MediaStreamTrackGenerator` writable streams to
  `MediaStreamTrackProcessor` readable streams for local AudioData/VideoFrame
  flow, with live total/discarded counters and an explicit warning-backed empty
  stream for native capture tracks lacking a decoded-frame adapter.
- [x] Implement IndexedDB `getAllRecords()` for object stores and indexes with
  branded `IDBRecord` key/primaryKey/value results, count filtering and
  forward/reverse ordering.
- [x] Add Background Fetch record/registration identities while preserving
  truthful warning-backed rejection without isolated Service Worker execution.
- [x] Brand audio/video track statistics and audio playback statistics, update
  local generator frame counters, and expose neutral resettable host snapshots.
- [x] Add the full WebUSB descriptor/alternate/interface/endpoint and transfer
  result record family with nested browser identities.
- [x] Extend Credential Management with DigitalCredential, IdentityCredential,
  IdentityCredentialError and IdentityProvider static capability/failure
  behavior; wallet/IdP operations remain explicit host-adapter boundaries.
- [x] Implement `ElementInternals`, `CustomStateSet` and `CSSPseudoElement`
  with form ownership/value state, validity events, ARIA reflection, iterable
  dashed states and pseudo-element parent/origin chains.
- [x] Add `NavigatorManagedData` host-injected configuration queries/change
  events and validated `NavigatorLogin` status state, with warnings for missing
  enterprise-policy and host-account adapters.
- [x] Implement `EditContext` and `TextFormat` with UTF-16 text/selection
  mutation, TextUpdateEvent delivery, character/control/selection geometry and
  live element attachment.
- [x] Add branded `ChapterInformation` records and normalize
  `MediaMetadata.chapterInfo` entries alongside artwork metadata.
- [x] Add `AudioDecoder`, `AudioEncoder`, `VideoDecoder` and `VideoEncoder`
  controller identities with configuration validation, EventTarget/dequeue
  behavior, queue/reset/close lifecycles and Promise-shaped support/flush
  results. Until a native codec adapter is registered, support queries return
  false and queued processing closes through the standard error callback with
  an explicit warning instead of reporting fake encoded output.
- [x] Add Chrome's current `AnimationTrigger`/`TimelineTrigger` surface with
  iterable activation/active range records and lists, validated entry/exit
  actions, unique animation association/update/removal, and explicit renderer
  warnings until scroll-timeline sampling drives native playback actions.
- [x] Add the complete current Chrome Web Audio constructor family and
  inheritance graph, with mutable Float32 PCM buffers, AudioParam automation
  state, node connection/disconnection, source lifecycle/events, analyser and
  frequency-response output, AudioContext clock/state, media-node facades,
  AudioWorklet ports and deterministic OfflineAudioContext silent rendering.
  Real-time output, compressed decoding, sink selection and worklet processor
  execution remain explicit warning-backed native-adapter boundaries.
- [x] Implement `ImageDecoder`, `ImageTrack` and `ImageTrackList` over the
  existing native image codecs, including MIME capability queries, static
  PNG/JPEG-family decode, GIF/WebP/APNG frame extraction, repetition/frame
  metadata, desired-size resampling and RGBA `VideoFrame` results. Incremental
  ReadableStream input remains an explicit warning-backed adapter boundary.
- [x] Add the current Chrome WebRTC constructor family with stateful
  offer/answer and local/remote description transitions, ICE candidate/SDP
  records, media sender/receiver/transceiver graphs, legacy stream methods,
  data-channel configuration/closing, DTMF and encoded-stream facades,
  iterable stats reports and RTP capability shapes. ICE gathering, certificate
  generation, DTLS/SRTP/SCTP and actual network delivery remain explicit
  warning-backed native-adapter boundaries and never report fake connectivity.
- [x] Add Chrome's independent experimental browser-service interfaces,
  including process-local Shared Storage modifiers, live viewport segments,
  inactive `fetchLater` results, empty local-font discovery and truthful
  unavailable AI/Ink/Profiler/Privacy Sandbox capabilities with one-time
  compatibility warnings.
- [x] Add `WebSocketStream` and the WebTransport object/stream/error family
  with URL validation and Promise-shaped lifecycle. WebSocket-to-Streams and
  HTTP/3/QUIC delivery reject explicitly until native adapters are connected.
- [x] Add the complete WebXR constructor/prototype family and `navigator.xr`
  capability surface, with fully computed `XRRigidTransform` inverse matrices
  and normalized `XRRay` geometry. Hardware capability queries return false
  and session creation rejects without an XR device/compositor adapter.
- [x] Connect WebGPU to Vello's native `wgpu` backend for adapter/device
  discovery, mapped GPU buffers, queue writes, WGSL shader modules and basic
  command encoder/submission, while retaining explicit warnings for pending
  bind-group/pipeline/pass/canvas descriptor translation.
- [x] Add WebGL 1/2 contexts, resource identities, shader/program lifecycle,
  state queries and current Chrome method/constant surfaces. GLSL-to-wgpu
  translation and draw/upload execution remain a warning-backed renderer
  boundary.
- [x] Brand the host-backed Web Bluetooth device/GATT server/service/
  characteristic graph with all Chrome identities, capability records,
  descriptor and notification facades, plus standard `BluetoothUUID`
  canonicalization. Host-unsupported enumeration/I/O rejects explicitly.
- [ ] Separate test labels for:
  - direct Rust engine API;
  - ESM/JavaScript Web surface;
  - desktop integration;
  - Android/iOS integration.

### Status integrity

- [x] Generate or maintain one Web API capability matrix with the columns
  `engine`, `esm_surface`, `desktop`, `android`, `ios`, and `conformance`.
- [ ] Remove DONE claims where only a Rust module exists.
- [ ] Keep roadmap, README capability claims, and mobile documentation aligned
  in the same change that lands an API.

**R0 exit:** required CI is green and every claimed Web API has an ESM-level
test.

---

## R1 — Native Web App P0

These APIs block the current formal downstream application and therefore
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
- [x] Bundle the IANA timezone database and resolve offsets per formatted
  instant, including DST transitions, while retaining deterministic fixed
  `UTC±HH:MM` offsets and invalid-timezone errors.
- [x] Expand the initial locale profiles with `de-DE`, `fr-FR`, and `ja-JP`
  decimal/grouping, currency placement, date/time conventions, and JPY/KRW
  minor-unit defaults.
- [ ] Replace the selected application profiles with broad CLDR-backed locale,
  numbering-system, calendar, plural, and currency-name coverage.

### R1.2 Network primitives

- [x] Expose the existing WebSocket engine as the standard `WebSocket`
  constructor.
  - `CONNECTING`, `OPEN`, `CLOSING`, `CLOSED`;
  - `onopen`, `onmessage`, `onerror`, `onclose`;
  - text/binary send and close code/reason;
  - event-loop polling and cleanup.
- [x] Complete the initial Fetch companion surface used by application code:
  `Request`, `Response`, `Headers`, and `AbortController`.
- [x] Expose JavaScript `ReadableStream`, `WritableStream`, `TransformStream`,
  `TextEncoderStream`, `TextDecoderStream`, count/byte-length queuing
  strategies, and their default reader/writer/controller interfaces with controller
  enqueue/close/error, promise-based reads/writes, locking/release/cancel,
  pending pulls, `ReadableStream.from()`, `tee()`, `pipeTo()`/`pipeThrough()`,
  preventClose/preventAbort/preventCancel and AbortSignal propagation,
  transform/flush callbacks, gzip/deflate compression streams, and Fetch
  response-body integration. Tee branches coordinate cancellation, combine
  reasons and release the original reader only after both branches cancel.
  `values()` and `Symbol.asyncIterator` expose promise-based `next()`/`return()`,
  `preventCancel`, source cancellation and deterministic reader-lock release.
  Compression currently buffers until close. Byte streams expose the standard
  controller/BYOB reader/request identities and compatible locking/delivery;
  BYOB reads warn because supplied views are not filled yet. Compiler lowering
  for `for await...of` syntax and exact backpressure remain explicit partials.
- [x] Expose `CustomElementRegistry` / `customElements` with autonomous
  element definition lookup, `whenDefined()`, explicit/subsequent creation
  upgrades, and connected/disconnected callbacks. Customized built-ins and
  exact reaction-queue ordering remain explicit partials with a warning.
- [x] Align `TextEncoder` / `TextDecoder` constructor identities and expose
  UTF-8/UTF-16 BOM handling plus `fatal`/`ignoreBOM`. Stateful incremental
  decoding preserves split BOMs, UTF-8 sequences, UTF-16 code units and
  surrogate pairs until flush; unsupported encoding labels warn and use the
  UTF-8 compatibility fallback.
- [x] Expose the Cache API (`caches`, `CacheStorage`, `Cache`) with
  Promise-returning `open`, `match`, `matchAll`, `put`, `add`, `addAll`,
  `delete`, and `keys`. The compatibility backend is process-local memory;
  persistence, quotas, and cross-process coordination remain pending with a
  warning.
- [x] Expose the Web Locks API (`navigator.locks`, `LockManager`, `Lock`) with
  Promise-based shared/exclusive FIFO acquisition, `ifAvailable`, `steal`,
  queued-request abort signals, and `query()` snapshots. Arbitration currently
  spans this runtime process; cross-process and cross-device locks remain
  pending with a warning.
- [x] Expose the Prioritized Task Scheduling API (`scheduler.postTask()`,
  `scheduler.yield()`, `TaskController`, `TaskSignal`) with delay, promise
  result adoption, abort handling, mutable priorities, and `prioritychange`
  events. Native priority-aware execution remains pending with a warning.
- [x] Expose the Reporting API (`ReportingObserver`, `Report`) with type
  filtering, buffered reports, microtask delivery, `takeRecords()`, and
  `disconnect()`, plus standards-compatible `reportError()` delivery through
  a global `ErrorEvent` and structured `CSPViolationReportBody` /
  `IntegrityViolationReportBody` records. Host crash/native-console reporting
  remains pending with a warning.
- [x] Expose the Cookie Store API (`cookieStore`, `CookieStore`,
  `CookieChangeEvent`) with Promise-based get/getAll/set/delete, asynchronous
  change events, and a shared session backend with `document.cookie`.
  Persistence, partitioning, expiry and service-worker delivery remain
  pending with a warning.
- [x] Expose the Sanitizer API (`Sanitizer`, `Element.setHTML()`,
  `Document.parseHTML()`) over an inert parser that removes active elements,
  inline event handlers, and `javascript:` URLs. Explicit `setHTMLUnsafe()` /
  `parseHTMLUnsafe()` retain inert unsanitized markup and warn that scripts and
  declarative shadow roots do not activate; exhaustive configurable allow/
  remove lists remain pending.
- [x] Expose Trusted Types (`trustedTypes`, `TrustedTypePolicyFactory`,
  `TrustedTypePolicy`, `TrustedHTML`, `TrustedScript`, `TrustedScriptURL`) with
  policy callbacks, branded string conversion, duplicate-name checks, default
  policy tracking, brand predicates and sink type introspection. Compatible
  inert HTML sinks accept the branded values; CSP policy-name directives and
  executable script sinks remain pending with a warning.
- [x] Expose the Web Share API (`navigator.canShare()` / `share()`) with
  browser-compatible payload, URL and File validation. Until a user-activation
  and native share-sheet adapter exists, valid shares warn and reject with
  `NotAllowedError`; invalid payloads reject with `TypeError` rather than
  reporting false success.
- [x] Expose the Screen Wake Lock API (`navigator.wakeLock`,
  `WakeLock`, `WakeLockSentinel`) with Promise-based screen requests,
  standards-shaped EventTarget sentinels, and idempotent release events.
  Requests warn that preventing host display sleep still requires a platform
  power adapter.
- [x] Expose the Badging API (`navigator.setAppBadge()` /
  `clearAppBadge()`) with Promise lifecycle, compatible flag/numeric state,
  and input validation. Calls warn that displaying the state on the host
  application icon still requires a platform adapter.
- [x] Expose the Permissions API (`navigator.permissions.query()` and
  `PermissionStatus`) with conservative host-aware snapshots, Promise
  rejection for unknown permission names, and EventTarget-compatible status
  objects. Live operating-system policy change delivery remains pending with
  a warning.
- [x] Add standard illegal manager identities for `Permissions`,
  `MediaDevices`, `Bluetooth`, and `Scheduler`, including EventTarget
  inheritance where specified and warning-only capture-handle configuration.
- [x] Expose the Network Information API (`navigator.connection` plus legacy
  aliases and `NetworkInformation`) as an EventTarget-compatible static
  snapshot. Live connection type, quality sampling and change delivery remain
  pending on a host adapter and emit a warning.
- [x] Expose the Storage Manager API (`navigator.storage`, `StorageManager`)
  with Promise-shaped estimates and persistence checks. Until a platform
  quota/persistence/OPFS adapter exists, estimates return explicit zero values,
  persistence returns `false`, and `getDirectory()` warns and rejects with
  `NotSupportedError`.
- [x] Expose Storage Buckets (`navigator.storageBuckets`,
  `StorageBucketManager`, `StorageBucket`) with validated names, process-local
  open/keys/delete lifecycle, expiry metadata, storage facades and Promise
  methods. Durable namespace isolation, quota enforcement and bucket-scoped
  OPFS remain runtime-storage work and emit a compatibility warning.
- [x] Expose User Activation (`navigator.userActivation`, `UserActivation`)
  with transient state during trusted native keyboard/mouse/touch/pointer
  handlers, sticky `hasBeenActive` state until runtime reset, and nested-event
  activation tracking.
- [x] Expose Media Session (`navigator.mediaSession`, `MediaSession`,
  `MediaMetadata`) with metadata/playback state, validated position state,
  action registration/removal and a host action-dispatch entry point.
  Publishing to platform media centers and receiving media-key actions require
  an adapter and emit a warning.
- [x] Expose Battery Status (`navigator.getBattery()`, `BatteryManager`) with a
  Promise singleton, dynamic telemetry getters, EventTarget change lifecycle,
  and a host update entry point. The default full/charging snapshot warns until
  a platform power adapter supplies live telemetry.
- [x] Expose the Credential Management base surface (`navigator.credentials`,
  `CredentialsContainer`, `Credential`, `PasswordCredential`,
  `FederatedCredential`) with typed object construction, null retrieval and
  Promise lifecycle. Secure persistence and authenticator-backed credential
  types explicitly warn/reject until user-consent and secure-vault adapters
  exist; HTML form constructor overloads remain pending.
- [x] Expose Web MIDI (`navigator.requestMIDIAccess`, `MIDIAccess`, port/map
  identities and MIDI events) with Promise access, live host-injectable input
  and output maps, connection/message dispatch, port open/close lifecycle and
  validated output recording. Physical discovery, system-exclusive permission
  and hardware I/O remain platform-adapter work and emit compatibility
  warnings.
- [x] Expose the Encrypted Media Extensions base surface
  (`navigator.requestMediaKeySystemAccess`, media-key identities and
  `MediaKeyMessageEvent`) with request validation and Promise-compatible
  failures. Valid key-system requests warn and reject `NotSupportedError`
  until a platform CDM, secure decoder and license-policy adapter exist.
- [x] Expose the Service Worker container base (`navigator.serviceWorker`,
  `ServiceWorkerContainer`, `ServiceWorker`, `ServiceWorkerRegistration`) with
  EventTarget identity, truthful empty discovery and explicit
  `NotSupportedError` registration/ready promises. Isolated worker realms,
  persistent origin storage and fetch interception remain a runtime-subsystem
  milestone and emit a compatibility warning.
- [x] Expose Web Serial, WebHID and WebUSB compatibility bases with navigator
  manager identity, EventTarget behavior, standard device/event constructors,
  truthful empty enumeration and Promise chooser rejection. Native discovery,
  permission prompts and device I/O remain host-adapter work and emit one-time
  warnings instead of reporting false success.
- [x] Expose the Web NFC base with constructible `NDEFReader`, `NDEFMessage`,
  `NDEFRecord` and `NDEFReadingEvent`, usable message/record data, AbortSignal
  pre-abort handling and explicit Promise failures. Scanning, permissions and
  tag I/O remain platform-adapter work and emit a one-time warning.
- [x] Align Chromium window-environment entry points with `navigator.keyboard`,
  `virtualKeyboard`, `devicePosture` and `windowControlsOverlay`, including
  standard constructor identity, empty layout-map iteration, EventTarget
  snapshots and host update hooks. Exclusive keyboard/IME control remains
  platform-adapter work and emits one-time compatibility warnings.
- [x] Expose `navigator.scheduling` / `Scheduling.isInputPending()` with exact
  Chromium prototype shape and a host-settable pending-input snapshot.
- [x] Expose the Presentation API base (`navigator.presentation`,
  `PresentationRequest`, availability/connection/receiver identities and
  connection events) with truthful unavailable snapshots, request validation
  and explicit Promise rejection. Display discovery, picker UI and transport
  remain platform-adapter work and emit a one-time warning.
- [x] Expose `IdleDetector` and `EyeDropper` with Chromium prototype shape,
  threshold/AbortSignal validation, conservative default permission, explicit
  host-unavailable errors and host-injectable idle/screen state. Native idle
  sampling and screen-color picking emit one-time compatibility warnings.
- [x] Expose Gamepad (`navigator.getGamepads()`, `Gamepad`, `GamepadButton`,
  `GamepadEvent`) with stable indexed snapshots, axes/buttons, trusted
  connect/disconnect events and a host update entry point. Physical controller
  discovery requires a platform gamepad adapter and emits a warning.
- [x] Expose Device Orientation and Motion (`DeviceOrientationEvent`,
  `DeviceMotionEvent`, acceleration and rotation-rate value identities) with
  standard fields/identity, permission-gated trusted window events, Permissions
  API synchronization and host sensor injection.
  Platform sensor-consent and telemetry adapters remain pending with warning.
- [x] Expose the Generic Sensor core (`Sensor`, `SensorErrorEvent`,
  `Accelerometer`, `Gyroscope`, `Magnetometer`) with asynchronous
  start/error/activate lifecycle, permission integration, reading events and
  host vector injection. Live telemetry requires platform adapters and emits a
  warning.
- [x] Expose Media Capabilities (`navigator.mediaCapabilities`,
  `MediaCapabilities`) with validated decode/encode Promise queries backed by
  a host-registerable MIME codec table. Unknown formats report unsupported;
  DRM key-system access remains `null` and missing adapters emit a warning.
- [x] Complete Navigator identity and legacy compatibility fields, including
  `Navigator`, empty standard-shaped `PluginArray`/`MimeTypeArray`, deprecated
  ID values, `javaEnabled()`, and validated `registerProtocolHandler()`.
  Protocol handlers are recorded process-locally; OS association and user
  consent require a platform adapter and emit a warning.
- [x] Propagate request signals, reject pre-aborted fetches before native I/O,
  bound native work with request/`AbortSignal.timeout()` deadlines, and expose
  `AbortSignal.abort()`, `any()`, `timeout()`, and `throwIfAborted()`.
- [ ] Interrupt an already-running native request when asynchronous Promise
  execution permits JavaScript to abort concurrently; the synchronous AOT
  facade currently reports a warning and relies on its native deadline.
- [x] Add an ESM integration test against a local WebSocket fixture.
- [x] Add an ESM integration test against a local HTTP fixture for Fetch and
  its companion constructors.

### R1.3 Web Speech

- [x] iOS native speech engine prototype (`SFSpeechRecognizer`).
- [x] Expose `window.SpeechRecognition` and the compatibility alias
  `window.webkitSpeechRecognition`.
- [x] Implement browser-shaped results, alternatives, confidence, finality,
  lifecycle events, and error events.
- [ ] Add Android speech recognition adapter.
- [x] Define desktop behavior explicitly: supported adapter or standards-shaped
  `not-supported` failure.
- [ ] Validate permissions, denial, restart, continuous mode, and cancellation
  on physical iOS and Android devices.

### R1.4 Geolocation

- [x] Implement `navigator.geolocation`.
- [x] Support `getCurrentPosition`, `watchPosition`, and `clearWatch`.
- [x] Implement timeout, maximum age, accuracy fields, permission denial, and
  platform-disabled errors.
- [ ] Add iOS Core Location and Android location adapters with manifest/plist
  generation.

### R1.5 MediaDevices

- [x] Implement `navigator.mediaDevices`.
- [x] Implement `getUserMedia()` for camera and microphone through the
  host-device adapter boundary.
- [x] Expose `getSupportedConstraints()`, `getDisplayMedia()` and
  `selectAudioOutput()` with truthful constraint reporting, validation and
  explicit Promise failures plus one-time warnings until host screen-capture
  and audio-output picker adapters exist.
- [x] Provide browser-shaped `MediaStream` and `MediaStreamTrack` lifecycle:
  track enumeration/filtering, add/remove events, clone, stop/ended, live
  `active`, constraints facades, and permission/not-found failures.
- [ ] Connect host media payloads to preview and capture sinks; the current
  adapter boundary exposes device/track lifecycle but not frame/audio data.
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

**R1 exit:** the formal application completes these flows on native
desktop and the applicable mobile targets using standard Web APIs.

---

## R2 — Web Platform Facade

R2 turns existing engine modules and partial shims into coherent browser-facing
APIs. Work is ordered by common npm usage, not by number of Rust modules.

### R2.1 Binary data and files

- [x] Expose working `TextEncoder` alongside `TextDecoder`.
- [x] Implement `ArrayBuffer`, `DataView`, and typed-array buffer/view
  semantics, including same-type `slice`/`map`/`filter` and change-by-copy
  methods, shared-buffer `subarray`, backing-store-safe mutations,
  `keys`/`values`/`entries`, aligned offsets, `ToIndex` length conversion, and
  browser-shaped constructor/access `TypeError`/`RangeError` bounds errors.
- [x] Add the Chromium 135+/ECMAScript `Float16Array`, DataView
  `getFloat16`/`setFloat16`, and `Math.f16round` surface with nearest-even
  IEEE-754 binary16 conversion.
- [x] Implement `Blob`, `File`, and `FileReader`.
- [x] Implement `FormData`, including Fetch request integration.
- [x] Implement `ImageData`, `Path2D`, and `OffscreenCanvas` where supported by
  the existing Canvas engine.
- [x] Implement Canvas 2D line-dash state, odd-pattern normalization,
  `lineDashOffset`, save/restore, and dashed path/rectangle rasterization.

### R2.2 Events and DOM constructors

- [x] Implement callable `Event`, `CustomEvent`, and `EventTarget`.
- [x] Expose DOM constructors with useful identity and `instanceof` behavior:
  `Node`, `Element`, `HTMLElement`, common HTML elements, `Range`, and
  `Selection`.
- [x] Expand constructor identity and tag mapping across the current Chromium
  HTML element family and the SVG element hierarchy, including inherited
  geometry/text/animation/filter identities and their standard enum constants.
- [x] Expose legacy SVG animated/list/value identities and constants, mapping
  SVGPoint/Rect/Matrix to the working DOM geometry implementation.
- [x] Implement the data-oriented CSS Typed OM subset: keyword/unit/numeric/math,
  variable, position and transform values with parsing, arithmetic, iteration
  and serialization; connect `attributeStyleMap`/`computedStyleMap()` to live
  DOM styles; connect constructable stylesheet results to CSSRule and
  CSSStyleRule identities and expose the current CSSOM rule hierarchy.
- [x] Extend the DOM arena with real CDATA, processing-instruction and document
  type nodes, including cloning/serialization, XML document factories and a
  working `DOMImplementation` surface.
- [x] Implement CSS Custom Highlight `Highlight`/`HighlightRegistry` set/map
  semantics, `CSS.highlights`, point queries and range identity. Registry
  painting remains a compositor boundary and emits a one-time warning.
- [x] Expose standard event subclasses: keyboard, pointer, input, clipboard,
  drag, touch, animation, transition, close, blob, submit/form-data, toggle,
  command, page-transition, promise-rejection, CSP violation, media track and
  storage events, including required dictionary validation and inheritance.
- [x] Add the remaining Chrome event constructor identities as dictionary-backed
  generic Events, preserving their standard fields while subsystem-specific
  producers (WebRTC, WebGPU, WebXR, payments and audio graphs) remain separate
  implementation milestones.
- [x] Align `Event`, `EventTarget`, `CustomEvent`, and all implemented event
  subclass prototypes with Chromium; expose experimental `EventTarget.when()`
  through standard `Observable`/`Subscriber` identities.
- [x] Add the Chromium Observable surface with iterable `from()`, subscription
  lifecycle, map/filter/take/drop/inspect transforms, collection and predicate
  Promise operators, and warning-compatible advanced composition placeholders
  pending exact cancellation/flattening semantics.
- [x] Close Chromium prototype gaps across existing parser/serializer,
  AbortController/Signal, Performance entry/list, WakeLock, Permissions,
  Web Locks, MediaCapabilities, Scheduler task signals/controllers, and
  UserActivation implementations without changing their established behavior.
- [x] Give TouchEvent touch collections the standard illegal `TouchList`
  constructor identity with indexed/item access.
- [x] Dispatch live matchMedia changes as standard `MediaQueryListEvent`
  instances with `matches` and `media` fields.
- [x] Expose `CloseWatcher` with Chromium prototype shape, cancelable
  `requestClose()`, close-once/destroy behavior and AbortSignal teardown.
- [x] Replace silent empty-object fallbacks with standards-shaped exceptions or
  explicit unsupported errors.

### R2.3 Observers and background work

- [x] ResizeObserver engine and compiler special case.
- [x] Expose standard `ResizeObserver` constructor behavior through ESM,
  including DOM and native-host targets, content/border/device-pixel box
  entries, box-selective change detection, callback observer identity,
  unobserve/disconnect, and a compatibility warning when host DPR metadata is
  unavailable.
- [x] Give delivered resize records and box-size values the standard illegal
  `ResizeObserverEntry` and `ResizeObserverSize` constructor identities.
- [x] Give delivered intersection records the standard illegal
  `IntersectionObserverEntry` constructor identity.
- [x] Give delivered mutation records the standard illegal `MutationRecord`
  constructor identity and complete record-field prototype.
- [x] Expose `MutationObserver` and `IntersectionObserver` through ESM.
- [x] Align `ResizeObserver`, `MutationObserver`, `IntersectionObserver`, and
  `PerformanceObserver` prototype members with Chromium, including
  IntersectionObserver v2 fields and the compatible `scrollMargin` default.
- [x] Implement the User Timing subset of the Performance Timeline:
  `mark()`, `measure()`, entry queries/clearing, mark-name resolution, detail
  values, buffered asynchronous `PerformanceObserver` delivery, and standard
  `PerformanceEntry`/mark/measure/observer-entry-list prototype identities.
- [x] Extend Performance Timeline with the current Chrome rendering,
  interaction, navigation and resource entry classes, exact prototype members,
  nested value identities, neutral defaults and a warning-backed host
  instrumentation boundary.
- [x] Expose the Worker engine as `Worker`, `SharedWorker`, `MessageChannel`,
  `MessagePort`, and structured message events.
  - [x] Expose `BroadcastChannel` with same-name fan-out, per-recipient
    structured cloning, asynchronous message delivery, close semantics,
    standard events, and constructor/prototype identity.
  - [x] Queue `MessagePort` traffic until `start()` or `onmessage` assignment,
    clone messages at `postMessage()` time, and stop delivery after `close()`.
  - [x] Preserve structured-clone cycles/shared references and Date, RegExp,
    Map, Set, Error/AggregateError, Blob/File, ImageData,
    ArrayBuffer/SharedArrayBuffer, TypedArray, and DataView types; reject
    functions; implement ArrayBuffer transfer/detach through `structuredClone()` and
    `MessagePort.postMessage()`.
  - [x] Transfer MessagePort objects through same-thread structured messages,
    preserve entanglement, detach the source wrapper only after a successful
    clone, and reject duplicate transfer-list entries.
  - [x] Replace Worker JSON.stringify transport with the graph
    structured-clone codec for cycles, collections, Error, Blob/File and other
    supported platform values, including exact ArrayBuffer/SharedArrayBuffer,
    TypedArray/DataView types, ranges, and shared backing-buffer topology.
  - [ ] Execute referenced Worker scripts and create isolated worker realms.
    The current Worker host remains an explicit echo profile; MessagePort
    transfer into that missing realm warns once and raises `DataCloneError`
    without detaching the source.

### R2.4 Remaining network/browser services

- [x] Expose `EventSource`.
- [x] Provide `XMLHttpRequest` as a compatibility shim over Fetch.
- [x] Complete the XMLHttpRequest event-target hierarchy with standard
  `XMLHttpRequestEventTarget`/`XMLHttpRequestUpload` identities, ProgressEvent
  delivery and a live upload target.
- [x] Add browser legacy factories (`Image`, `Audio`, `Option`) backed by real
  typed DOM elements, plus truthful hidden `BarProp` window-chrome snapshots.
- [x] Expose the Notifications API on supported desktop/mobile platforms.
- [x] Complete Clipboard item/data-transfer APIs beyond text-only clipboard.
- [x] Give `navigator.clipboard` the standard illegal `Clipboard` constructor
  identity, EventTarget inheritance, `onclipboardchange` surface, and the
  existing text/item read-write behavior.
- [x] Replace DataTransfer's plain collection arrays with live `FileList`,
  `DataTransferItem`, and `DataTransferItemList` identities, indexed access,
  file/string filtering, add/remove/clear behavior, and explicit warning
  fallbacks for unavailable file-system entries and native drag images.
- [x] Add secure randomness backed by the OS; do not use the current
  deterministic fallback for security-sensitive APIs.

### R2.5 DOM, viewport, and display

- [x] Give the live `visualViewport` singleton its standard illegal
  `VisualViewport` identity, EventTarget inheritance and resize/scroll handler
  surface while preserving viewport, scroll and keyboard-inset synchronization.
- [x] Add live `ValidityState` identities and control/form
  `checkValidity()`/`reportValidity()`/`setCustomValidity()` behavior for
  required, email/URL, range, step, length and custom constraints, including
  synchronous invalid events. Pattern parsing and native report UI remain
  explicit warning boundaries.
- [x] Give the host-injectable Geolocation facade standard `Geolocation`,
  position, coordinates and error identities, JSON methods and error constants.
- [x] Add `StyleSheet`, `StyleSheetList`, and `MediaList` identities,
  CSSStyleSheet inheritance, live media text/item mutation, indexed empty
  document sheet discovery, and complete the existing CSSStyleSheet prototype
  surface. Discovering authored `<style>`/`<link>` sheets remains parser work.
- [x] Make writable `Location` properties and `assign()`/`replace()` update
  session history, and dispatch trusted `HashChangeEvent`/`PopStateEvent`
  subclasses during navigation. `reload()` remains a warning-only host hook
  until document reconstruction is available.
- [x] Implement detached deep/shallow `Document.importNode()`,
  single-document `adoptNode()` detachment, and indexed/named `NamedNodeMap`
  attribute lookup.
- [x] Implement Canvas 2D `drawImage()` for HTML/Offscreen canvas sources,
  including 3/5/9 argument forms, source cropping, destination scaling,
  translation, and global-alpha composition. Undecoded image/video sources
  emit a one-time compatibility warning instead of silently succeeding.
- [x] Make `requestIdleCallback()` deliver an `IdleDeadline` with live
  `timeRemaining()`/`didTimeout`, support cancellation, and expose both APIs
  as bare ESM globals.
- [x] Connect the CSS Font Loading API to the native registry: `FontFace`
  descriptors/status/`load()`/`loaded`, `FontFaceSet` add/delete/clear/check/
  load/iteration/ready, ArrayBuffer and local/file sources, constructor
  identity, and reset behavior. Network font URLs reject with an explicit
  one-time host-adapter warning.
- [x] Replace the `scrollIntoView()` no-op with scroll-container discovery,
  boolean/options overloads, block/inline start/center/end/nearest alignment,
  `container: nearest`, viewport fallback, and scroll events. Smooth behavior
  applies the final position immediately with a one-time warning.
- [x] Replace unsupported `DOMParser`/`XMLSerializer` constructors with
  detached, queryable document facades for HTML/XML/XHTML/SVG MIME types and
  DOM-backed serialization. XML modes warn once that DTD, namespace
  validation, and strict well-formedness diagnostics remain unavailable.
- [x] Brand parsed documents as `HTMLDocument`/`XMLDocument`, UI event source
  capabilities as `InputDeviceCapabilities`, and exact media-device constraint
  failures as `OverconstrainedError`.
- [x] Implement open/closed `ShadowRoot` tree identity, querying, connectivity,
  `getRootNode({ composed })`, composed event propagation/path, and boundary
  retargeting. Slot distribution, declarative shadow DOM, and composed
  rendering remain explicit compatibility warnings.
- [x] Replace unsupported DOM Geometry constructors with mutable/read-only
  `DOMRect`, `DOMPoint`, `DOMMatrix`, `DOMQuad`, and `DOMRectList`
  interfaces, including constructor identity, JSON conversion, 2D/3D matrix
  fields, transforms, chaining, mutable `*Self()` operations, quad
  construction/copy/bounds, and correctly typed `getClientRects()` results.
- [x] Replace unsupported CSSOM globals with `CSS.supports()`/`CSS.escape()`,
  real `CSSStyleDeclaration` identity for inline/computed styles, and
  constructable `CSSStyleSheet` rule insertion/deletion/replacement plus
  `document.adoptedStyleSheets`.
- [x] Replace unsupported collection placeholders with browser-shaped
  `NodeList` and `HTMLCollection` identities, indexed/item/named access,
  iteration/`forEach`, static selector snapshots, and live tree/tag/class
  collections.
- [x] Remove the final explicit unsupported-global placeholders: implement
  legacy UTF-16 `escape()`/`unescape()`, expose the non-constructible Reporting
  `Report` interface, and make `eval(string)`/`Function(source)` preserve their
  callable browser shapes while warning once at the AOT dynamic-code boundary.
- [x] Replace `Range` mutation no-ops with DOM-backed `cloneContents()`,
  `extractContents()`, `deleteContents()`, `insertNode()`,
  `surroundContents()`, contextual fragments, and before/after boundary
  setters. Same-container text/child ranges preserve nodes; cross-container
  partial selections warn and use a documented text fallback.
- [x] Add `AbstractRange`/`StaticRange` constructor identities, standard
  inheritance, immutable boundary accessors, and required-init validation.
- [x] Add `CharacterData`, `Text`, and `Comment` identities with UTF-16
  substring/insert/delete/replace operations, adjacent `wholeText`, and
  node-preserving `splitText()`.
- [x] Add `DOMTokenList`/`DOMStringMap` identities and make `classList` and
  `dataset` stable live views, including class iteration and camelCase
  `data-*` reads, writes, and deletion.
- [x] Add `Attr`/`NamedNodeMap` identities around element attribute lookup,
  namespace aliases, mutation methods, owner-element links, and Node
  inheritance.
- [x] Add `Location`/`History` constructor identities and prototype shapes
  around the existing live URL, session-history, state, and navigation-event
  implementation.
- [x] Add the standard `Storage` identity and prototype shape around the
  existing local/session storage backends.
- [x] Add the `Performance` constructor identity and EventTarget inheritance
  around User Timing/PerformanceObserver, with explicit warning fallbacks for
  unavailable resource timing and allocator-memory telemetry.
- [x] Give `performance.navigation` and `performance.timing` standard legacy
  `PerformanceNavigation`/`PerformanceTiming` identities, constants,
  timestamps and JSON snapshots.
- [x] Add `Crypto`/`SubtleCrypto` identities around secure random generation;
  preserve every SubtleCrypto Promise method with explicit warning and
  `NotSupportedError` rejection until a native cryptographic provider lands.
- [x] Add the `DOMStringList` identity and list methods used by
  `Location.ancestorOrigins`.
- [x] Add the read-only maplike `EventCounts` identity with Chromium's 36
  interaction-event keys, excluding synthetic dispatch and incrementing
  trusted native input delivery.
- [x] Replace `MediaQueryList` listener no-ops with live viewport/DPR
  reevaluation, `change` events, `onchange`, legacy listener aliases, and
  browser-shaped illegal-constructor and EventTarget prototype identity.
- [x] Expose the standard illegal `MediaDeviceInfo` identity, prototype fields,
  and `toJSON()` surface for enumerated device records.
- [x] Make unavailable host window/dialog controls, deprecated
  `document.execCommand()`, beacon, and vibration APIs emit one-time warnings
  while preserving browser-compatible fallback return shapes.
- [x] Replace simplified Web API thenables with the shared Promise engine so
  async DOM APIs preserve microtask scheduling and chainable
  `then()`/`catch()`/`finally()` results.
- [x] Replace fixed-zero Range geometry with selected-node client rects and
  bounding unions backed by current layout data; collapsed ranges return an
  empty rect list and cross-container geometry uses the warning fallback.
- [x] Extend `DOMMatrix.inverse()`/`invertSelf()` from the 2D shortcut to a
  pivoted general 4×4 inverse with browser-compatible NaN matrices for
  singular inputs.
- [x] Extend `CSS.supports(conditionText)` with nested parentheses and
  `not`/`and`/`or` condition composition on top of declaration and selector
  queries.
- [x] Replace the `MutationObserver.observe()` placeholder with DOM write-path
  integration for attributes, character data, child lists, subtree matching,
  filters, old values, record draining, disconnect, and batched microtask
  delivery.
- [x] Replace the synchronous always-visible `IntersectionObserver` placeholder
  with layout/viewport-backed intersection geometry, normalized thresholds and
  `rootMargin`, threshold-crossing delivery, live refresh, queued records,
  unobserve/disconnect, DOMRect entry fields, and a warning-based geometric
  fallback for occlusion visibility tracking.
- [x] Extend `ResizeObserver` beyond native host-tree ids to ordinary DOM
  elements with layout-backed content/border geometry, DPR-aware
  `devicePixelContentBoxSize`, validated box options, change deduplication, and
  compiler-generated behavior coverage.
- [x] Implement `NodeFilter`, `TreeWalker`, and `NodeIterator` with
  `whatToShow`, callable/object filters, skip/reject behavior, directional
  traversal, constructor identity, Document roots, live tree reads, and
  reference-pointer repair after subtree removal.
- [x] Replace bare `URL`/`URLSearchParams` compiler stubs with real global
  constructors, prototype identity, `window` aliases, relative resolution,
  iterable query operations, live URL/query synchronization, `canParse()` /
  `parse()`, and Blob/File object URL registration with byte-exact Fetch
  resolution, MIME propagation and revocation.
- [x] Add `URLPattern` as a standard ESM/window constructor with string and
  object initialization, base URL resolution, component getters,
  `test()`/`exec()`, wildcard/named capture groups and `ignoreCase`; retain a
  compatibility warning/default-segment fallback for custom regexp groups
  until the complete URLPattern grammar is available.
- [x] Add the Chromium Navigation API surface (`navigation`,
  `NavigationHistoryEntry`, navigation events/transition/activation identities)
  with stable entry ids/keys, same-document navigate/replace/reload/traversal,
  state updates, cancelation, result promises and compiled-JavaScript coverage;
  direct legacy History/location writes retain an explicit warning until both
  APIs share one keyed backing store.
- [x] Add the Launch Handler API (`launchQueue`, `LaunchQueue`, `LaunchParams`)
  with queued-before-consumer delivery, asynchronous ordered callbacks,
  standard identities and a host launch injection entry point.
- [x] Add View Transition API lifecycle (`document.startViewTransition`,
  active transition, transition/type-set identities, page reveal/swap events,
  update/ready/finished promises, `waitUntil()` and `skipTransition()`), with a
  compatibility warning while renderer snapshot capture and pseudo-element
  compositing remain pending.
- [x] Replace empty Error-family globals with browser-shaped `Error`,
  `TypeError`, `SyntaxError`, `RangeError`, `ReferenceError`, `EvalError`,
  `URIError`, and `AggregateError` constructors, subclass prototype chains,
  `cause`, aggregate `errors`, stack/message/name fields, and ESM/window
  identity.
- [x] Unify `DOMException` across core and window globals with all legacy code
  constants, name-derived `code`, browser string form, prototype identity, and
  structured-clone preservation.
- [x] Add standard IndexedDB interface globals and prototype identity for
  factory, key ranges, requests/open requests, databases, transactions, object
  stores, indexes, cursor/cursor-with-value objects, and
  `IDBVersionChangeEvent`.
- [x] Preserve IndexedDB storage-clone identity, cycles, bytes, and metadata
  for BigInt, Map, Set, RegExp, Error, DOMException, Blob, File, and ImageData
  values; preserve exact ArrayBuffer/SharedArrayBuffer and TypedArray/DataView
  types, view ranges, and shared backing-buffer topology while retaining
  sortable array and binary index keys, with Rust codec and
  compiler-generated JavaScript behavior coverage.
- [x] Implement Fullscreen API and `fullscreenchange` lifecycle.
- [x] Implement Screen Orientation state, `lock()`, `unlock()`, and events.
- [x] Make `VisualViewport` geometry and resize/scroll listeners live.
- [x] Complete computed style beyond inline-style reflection.
- [ ] Complete SVG namespace/rendering support required by application gates.
  - [x] SVG namespace identity, concrete element constructors, animated
    lengths, geometry methods, and basic `rect`/`circle`/`ellipse`/`line`/text
    presentation attributes.
  - [x] SVG path/polyline/polygon rasterization (including arcs and shorthand
    commands), fill/stroke paint, and basic translate/scale/rotate transforms.
  - [x] Retained SVG subtree normalization/rasterization through `usvg`/`resvg`,
    including static gradients, clipping, masks, `<use>`, and viewBox scaling.
    Raster revisions exclude compositor-only transform and opacity changes.
  - [x] Retain parsed usvg trees across raster sizes and route pointer/click/wheel
    events through DOM bubbling chains. Author-ID nodes use direct lookup;
    anonymous basic shapes use paint-order mapping and cached per-node alpha
    masks. Path hit testing covers `auto`/`visiblePainted`, `painted`, `fill`,
    `visibleFill`, `stroke`, `visibleStroke`, `all`, `visible`, `none`, and
    `bounding-box`; `<defs>` nodes no longer shift anonymous paint ordinals,
    and both author-ID and anonymous `<use>` instances route to their DOM
    bubbling chain. Text uses character-cell hit geometry; raster images use
    per-pixel alpha for `painted` and their full rectangle for
    `fill`/`stroke`/`all`. A separately cached hit tree removes `mask` and
    `filter` effects while retaining transformed `clip-path` ancestry, matching
    SVG pointer-event processing. Geometry-only hit modes inject paint only
    into that hit tree, preserving otherwise-pruned unpainted shapes and text;
    inherited `pointer-events` values are retained in DOM event metadata.
    Unpainted `<use>` shadow content, including transitive nested references,
    is retained only for geometry hit modes; painted instances continue to use
    the display tree. Nested clip-path children and clip-path-on-clip-path
    chains are intersected on isolated local-coordinate layers.
  - [ ] Complete animation-aware subtree invalidation, tile-granular
    rerasterization, and an optional direct GPU vector path.
- [x] Define cookie behavior: real per-origin session store instead of inert
  assignment.

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

- [x] Map native window touch input through runtime hit testing into paired
  `PointerEvent` and `TouchEvent` lifecycles, including stable identifiers,
  active/target/changed `TouchList` snapshots, pressure, cancel, primary-touch
  selection, and `preventDefault()` feedback.
- [ ] Replace the standalone `w3cos-mobile::touch::TouchEvent::dispatch()`
  compatibility placeholder and wire Android MotionEvent / iOS UITouch direct
  surface adapters; the placeholder now emits a one-time warning instead of
  silently succeeding.
- [x] Implement explicit pointer capture by pointer id, event retargeting,
  `gotpointercapture` / `lostpointercapture`, implicit release, and
  `NotFoundError` for inactive pointers.
- [ ] Add native per-contact geometry/pressure where available, scrolling
  arbitration, and gesture interruption across direct mobile adapters.
- [x] Expose live host-configurable `navigator.maxTouchPoints`; mobile startup
  reports a conservative value of 5 with a compatibility warning.
- [ ] Replace the mobile fallback with exact Android/iOS/Harmony hardware
  simultaneous-contact reporting.

### R3.3 IME and editable text

- [ ] Connect native focus to `<input>`, `<textarea>`, and contenteditable.
- [x] Implement text-control `select()`, `setSelectionRange()`, and
  `setRangeText()` with UTF-16 offsets, selection direction, replacement
  modes, and `IndexSizeError`.
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
- [x] Synchronize runtime layout size, device-pixel ratio, and Android/iOS
  keyboard inset changes through live `innerWidth`/`innerHeight` and
  `VisualViewport`, including `resize` events.
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

- [x] Complete RegExp semantics required by package gates.
  - [x] Constructor metadata, compiled literals, `exec`/`test`, global and
    sticky `lastIndex`, named captures, `match`/`search`/`replace`, UTF-16
    indices, and syntax errors.
  - [x] Replacement callbacks and JavaScript substitution tokens.
  - [x] `matchAll` iteration and regexp-aware `split`.
  - [x] Match indices (`d`) with UTF-16 ranges and named groups.
  - [x] Unicode sets (`v`), look-around, and backreferences required by
    package gates.
- [x] Implement `BigInt` literals, construction, arbitrary-precision
  arithmetic, comparisons, bitwise operations, shifts, and radix formatting.
- [x] Implement `WeakMap`, `WeakSet`, `WeakRef`, and `FinalizationRegistry`
  semantics where feasible.
  - [x] Weak object/array/function targets, weak-key collections, `deref`,
    constructor identity, and primitive-target `TypeError`s.
  - [x] `FinalizationRegistry.register`, `unregister`, and explicit
    `cleanupSome` delivery for dead targets.
  - [x] When tracing-GC timing is unavailable, retain the compatible API and
    return shapes, emit one runtime warning, and expose explicit cleanup
    instead of failing construction. Ephemeron tracing and automatic callback
    timing are not claimed.
- [x] Implement `ArrayBuffer`, shared-memory, and Atomics semantics selected by
  the supported security model.
  - [x] Expose resizable `ArrayBuffer` and growable `SharedArrayBuffer`
    construction, capacity getters, `resize()` / `grow()`, detached-state
    reporting, and `transfer()` / `transferToFixedLength()` with bounds and
    fixed-buffer errors. Views currently retain their construction-time length
    and emit a compatibility warning instead of claiming automatic
    length-tracking semantics.
  - [x] Shared backing storage for integer and BigInt typed arrays with
    `load`, `store`, arithmetic, bitwise, exchange, and compare-exchange.
  - [x] Bounds/type errors, `isLockFree`, `notify`, and browser-compatible
    `wait`/`waitAsync` return shapes.
  - [x] Use a warning plus non-blocking `timed-out` fallback where the native
    host cannot safely block; cross-thread Worker execution is not claimed.
- [x] Complete URI encode/decode globals.
- [x] Remove reachable `todo!()` and silent unsupported-expression lowering
  from production compiler paths.

### R4.2 Package gates

- [x] Formal application gate using standard npm ESM dependencies.
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
- ⚠️ Writable `innerHTML` and explicit unsafe parsing create inert markup and
  never execute scripts; use the implemented Sanitizer / `setHTML()` /
  `Document.parseHTML()` path for active-content and unsafe-attribute removal.
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
