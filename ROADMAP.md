# W3C OS Roadmap

Last reconciled with implementation: **2026-08-22**
Baseline before this milestone: `main` @ `06bc454`

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

## Current implementation checkpoint

The `23808bc` checkpoint advances native Web UI behavior without changing the
release exit definitions below:

- DOM-to-component lowering now preserves more browser control semantics,
  stylesheet selector context, custom properties, replaced images, and SVG
  children.
- CSS/layout work covers additional declaration shorthands, `inline-flex`,
  Grid placement/stretching, wrapped text constraints, and intrinsic image
  sizing.
- Fetch responses can expose native byte delivery through `ReadableStream`;
  object URLs participate in Fetch and image decoding without enabling the
  dynamic-JavaScript runtime.
- Window interaction ordering now prefers the deepest live DOM control after
  framework rebuilds, respects disabled/read-only states, and keeps native
  focus, submit, text, composition, and viewport updates on the shared DOM
  path.
- Mobile code generation writes stable incremental module files for Android,
  iOS, and HarmonyOS. The iOS host includes native text-input/keyboard handling
  and a document-picker bridge for file inputs.

These are source and focused-test milestones. They do not close the physical
Android/iOS device, full gesture, lifecycle, signing, or downstream-product
gates in R3.

---

## R0 — Restore a Trustworthy Main Branch

No new API should be declared complete while the corresponding conformance
suite is red.

### Green baseline

- [x] Add a pinned raw-WPT runner with an exact-clean-checkout gate, upstream
  `testharness.js`, isolated per-document workers, deterministic offscreen
  Skia reftests, WPT fuzzy comparison, JSON results, and PNG diff artifacts.
- [x] Establish a fail-closed two-case raw-WPT smoke gate and a broader
  ten-case baseline. The initial 2-pass/3-fail snapshot on 2026-08-22 was
  closed to 5 pass and 0 fail without expected-result exemptions.
- [x] Close the three recorded raw-WPT failures: HTML attribute-name
  normalization, empty-value attribute selectors plus Window named access,
  and block-in-inline opacity positioning.
- [x] Complete the first raw-WPT expansion from 5 to 10 cases. The 8-pass /
  2-fail discovery snapshot was closed without exemptions by fixing same-name
  namespaced attributes, quoted attribute-value selector parsing, and indexed
  NodeList own-property descriptors; both added CSS reftests are pixel-exact.
- [x] Inventory the full pinned `dom/nodes` and `css/CSS2` document range and
  execute all 6,548 cases supported by the static runner in isolated,
  resumable release batches. Keep the 2,368 pass / 4,180 red result as
  structured discovery evidence rather than weakening the ten-case gate.
- [x] Prevent stale page-arena handles from aliasing newly allocated values
  after a Realm reset. The serialized full `w3cos-runtime --lib` run moved
  from 730 pass / 79 fail / 1 ignored to 773 pass / 36 fail / 1 ignored; the
  remaining Realm-lifetime assertions and IndexedDB cascade stay visible as
  blocking runtime debt rather than being hidden by the WPT runner.
- [ ] Add the 112 classified WPT server/metadata capabilities required by the
  remaining entries: print, multi-reference graphs, `.headers`, generated JS
  wrappers, fuzzy metadata, testdriver, server handlers, and `.sub` expansion.
- [ ] Add an independently pinned Test262 runner for ECMAScript language
  semantics; do not infer Test262 coverage from WPT or direct Rust tests.
- [x] Fix `w3cos-runtime --test w3c_feature_matrix`
  `dom_to_component_tree_smoke`.
- [x] Fix `w3cos-compiler` `generated_bundle_runs_jsdom_globals`.
- [x] Make the CodeMirror diagnostic compile reuse the workspace target with
  offline dependency resolution and a renderer-free runtime dependency, so
  registry/Skia availability cannot replace the expected ESM-lowering
  diagnostic with a CI infrastructure failure.
- [x] Make the required compiler/runtime suites explicit blocking steps in the
  default CI gate (`w3cos-compiler --lib --tests` and
  `w3cos-runtime --lib --tests`).
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
  reader locking/delivery. Supplied BYOB views are filled from queued chunks
  or `ReadableStreamBYOBRequest.respond()` / `respondWithNewView()`, sharing
  the caller buffer. Exact queuing backpressure remains an explicit partial.
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
  in the same change that lands an API. The `23808bc` reconciliation updates
  the current baseline; future capability changes still need this enforced as
  a landing rule.

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
  controller/BYOB reader/request identities, fill supplied views from queued
  chunks or `byobRequest.respond()` / `respondWithNewView()`, and keep leftover
  bytes queued. Compiler W3IR/AOT lowering for `for await...of` is covered by
  compiled-JS tests. Exact stream backpressure remains an explicit partial.
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
  change events, and a shared persistent-capable backend with `document.cookie`.
  Cookie
  selection now matches request host/domain, path and secure scheme; network
  `HttpOnly` cookies remain request-visible but hidden from script, and
  `Max-Age`/RFC `Expires` support expiry/deletion. Domain attributes are checked
  against the Mozilla Public Suffix List, and module subresource requests enforce
  schemeful-site Strict/Lax/None delivery. Storage quotas, the encrypted
  profile-partitioning contract, and an Apple Keychain protector are implemented;
  the remaining platform credential-store protectors, full Cookie Store option
  fidelity and service-worker delivery remain pending with a warning.
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
- [x] Interrupt an already-running native request when asynchronous Promise
  execution aborts concurrently. The AOT `fetch` facade now runs native I/O on
  a worker and pumps timers/microtasks, so `AbortController.abort()` from
  `Promise.then` / `queueMicrotask` / timers returns AbortError without waiting
  for the transport deadline. A platform I/O call may still finish in the
  background.
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
  - [x] Execute `blob:` and `data:` Worker scripts in an isolated W3VM realm
    when the `dynamic-js` feature is enabled. The parent thread captures the
    source (object URLs are thread-local) and the worker OS thread lowers it
    through SWC → W3IR → W3VM with its own `self` / `postMessage` globals.
    Top-level `onmessage = …` is still rejected by W3IR as an undeclared
    assignment; worker scripts should set `self.onmessage`. HTTP, file, and
    dummy URLs keep the existing structured-clone echo host.
    Ordinary AOT builds do not link the compiler or W3VM for this path.
  - [ ] Fetch and execute HTTP/file Worker scripts, compile Worker scripts on
    the ordinary AOT path, run SharedWorker realms, and transfer MessagePort
    objects into a worker thread. MessagePort transfer into a Worker still
    warns once and raises `DataCloneError` without detaching the source.

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
  surface. Authored `<style>`/`<link>` discovery and population is completed by
  the Browser subresource loader in Phase 3.6.
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
  identity, and reset behavior. Dynamic Browser documents route network font
  URLs through the shared Browser transport; ordinary AOT without an active
  Browser document rejects them with an explicit one-time host-adapter warning.
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
  - [x] Animation-aware subtree invalidation and 32px tile-granular
    rerasterization when SVG topology and geometry stay the same. Paint-only
    source mutations (typical of DOM `setAttribute` / presentation animation)
    copy clean tiles and rerasterize only dirty bounding boxes. Topology or
    geometry changes still take a full tiled raster. SMIL and Web Animations
    still do not write computed values into this raster path.
  - [ ] Optional direct GPU vector path for SVG (bypass CPU pixmap tiles).
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
- [x] Select a feature-minimal Skia mobile runtime, generate the size-oriented
  release profile, and emit reproducible unsigned iOS device-slice timing/size
  reports without claiming signing or App Store completion.
- [x] `w3cos mobile dev` with debug DevTools plumbing.
- [x] Safe-area inset storage and native setter.
- [x] HarmonyOS ArkUI/XComponent shell scaffold with fail-closed build.
- [x] Split generated ESM modules into stable per-module Rust sources and avoid
  rewriting unchanged generated files, enabling incremental mobile rebuilds.

### R3.2 Touch and pointer input

- [x] Map native window touch input through runtime hit testing into paired
  `PointerEvent` and `TouchEvent` lifecycles, including stable identifiers,
  active/target/changed `TouchList` snapshots, pressure, cancel, primary-touch
  selection, and `preventDefault()` feedback.
- [x] Replace the standalone `w3cos-mobile::touch::TouchEvent::dispatch()`
  compatibility placeholder with hit-tested dispatch into the shared jsdom
  PointerEvent/TouchEvent path (CSSOM boxes, same geometry as
  `document.elementFromPoint`). A miss on `Start` is ignored; active contacts
  keep their target through later phases.
- [ ] Wire Android MotionEvent / iOS UITouch direct surface adapters; the
  shared DTO path does not replace those host contact adapters. Exact hardware
  simultaneous-contact reporting and gesture arbitration remain pending.
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

- [x] Connect the shared runtime focus path to native text controls on iOS,
  including first-responder ownership and DOM value synchronization for
  `<input>` and `<textarea>`.
- [ ] Extend the native bridge to contenteditable on Android and iOS and prove
  parity on physical devices.
- [x] Implement text-control `select()`, `setSelectionRange()`, and
  `setRangeText()` with UTF-16 offsets, selection direction, replacement
  modes, and `IndexSizeError`.
- [x] Bridge iOS native text/marked-text state into DOM `beforeinput`, `input`,
  and `composition*` events and synchronize keyboard viewport insets.
- [ ] Complete exact caret geometry, selection-range round trips, Android IME,
  and physical-device UTF-8 commit/delete validation.
- [ ] Implement `inputmode`, `enterkeyhint`, secure input, and
  EditContext-compatible geometry.
- [ ] Add CJK, emoji, RTL, paste, autocorrect, and hardware-keyboard device
  tests.
- [x] Route iOS `<input type="file">` activation through a native document
  picker and return selected files through the DOM `FileList` bridge.

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

- 📋 `eval()` and arbitrary runtime code generation remain disabled in the
  AOT path; they may be enabled only inside the future capability-scoped W3VM
  after heap limits, execution budgets, cancellation, and page isolation land.
- ⚠️ Writable `innerHTML` and explicit unsafe parsing create inert markup and
  never execute scripts; use the implemented Sanitizer / `setHTML()` /
  `Document.parseHTML()` path for active-content and unsafe-attribute removal.
- ⛔ Runtime CommonJS `require()`: dependencies must be statically resolved,
  bundled, or migrated to the standard ESM loader.
- 📋 Service Worker execution is deferred until the W3VM background execution,
  lifecycle, storage, and permission model is designed.
- 📋 Real WebRTC networking remains deferred until the native ICE/DTLS/SRTP/
  SCTP adapters and their permission/security gates exist.
- 📋 Dynamic `import()` remains statically resolved in the AOT path and is
  planned for arbitrary runtime modules through W3IR/W3VM.
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

## Phase 3.5 — Unified AOT + Dynamic JavaScript Runtime

### Architecture invariants
- [ ] One JavaScript semantic core for both execution modes: `Value`, object/array semantics, prototype chain, property access, calls, exceptions, Promise, and Web API coercions must not be implemented twice
- [ ] One object heap / GC and stable handle model shared by AOT code, dynamically loaded code, Host functions, and DOM `NodeId` wrappers
- [x] One `Callable` ABI covering native AOT functions, W3 bytecode functions,
  and Rust Host functions, with calls allowed in every direction. W3VM
  callables use the existing `Value::Function` ABI and conformance tests cover
  Host → VM and VM → Host calls plus Host exceptions entering VM catch regions.
- [ ] One Realm, module registry, microtask queue, timer/event loop, DOM implementation, and Web API implementation per page context
- [x] Two execution backends only: native AOT for build-time-known modules and
  W3VM for runtime-loaded modules. CI rejects compiler/W3IR/W3VM linkage in
  ordinary AOT, confines runtime lowering to the single script adapter, and
  rejects file-source interpretation or runtime compilation fallback.
- [x] Route browser JavaScript by resolved URL protocol inside the single
  `ScriptLoader`: `http:`/`https:` and inline/runtime-created sources use the
  existing SWC → W3IR → W3VM path, while `file:` resolves only to a
  build-time-compiled native AOT record in the shared Core module registry.
  File URLs automatically alias their decoded local build path, so generated
  modules do not need a second registration format. Missing AOT records fail
  explicitly instead of reading source, invoking `rustc`, or falling back to
  W3VM. Classic scripts, ESM entries, static dependencies, and dynamic imports
  share this protocol gate; focused tests cover routing, one-time AOT
  evaluation, namespace exports, invalid source bypass, and missing-record
  rejection, while CI pins the boundary.
- [x] Ordinary AOT applications do not ship SWC, the compiler, or W3VM;
  `w3cos-runtime` links them only behind the explicit `dynamic-js` feature,
  verified by default/no-default-feature dependency-tree checks.

### W3IR semantic layer
- [x] Define versioned W3IR for constants, lexical scopes, closures, control flow, property operations, function/class construction, exceptions, async/await, and module operations
  - [x] Land the dependency-light `w3cos-ir` format-v1 foundation with typed
    function/block/register identities, constants, property/call/construct/
    await/control-flow instructions, and pre-execution structural validation.
  - [x] Add lexical environments, closures/classes, exception regions,
    module records/imports/exports, source locations, and async suspension
    metadata, with validation of references, control-flow termination,
    registers, captures, exception handlers and suspension points.
  - [x] Advance the serialized format through v17 for backend-neutral unary
    `typeof`/negation/bitwise-not, arithmetic/comparison/shift/bitwise/
    exponentiation/`in` operators, explicit property deletion, a validated
    rest-parameter call-frame binding, and array/object destructuring-rest
    plus lexical-cell refresh, `CopyDataProperties`, incremental
    array-element/iterable append, and materialized call/method/construct
    argument instructions.
    Persistent compiled-cache keys carry the format version, so older artifacts
    cannot execute as current modules.
- [x] Extract shared runtime intrinsics (`add`, `get_property`, `set_property`,
  `call`, `construct`, `await_value`, etc.) from the former direct Rust
  lowering path
  - [x] Establish the backend-neutral `w3cos_core::intrinsics` ABI for addition,
    arithmetic/comparison coercion, dynamic property reads/writes, aggregate
    creation, calls and construction; migrate ordinary AOT binary addition
    through the shared entry point.
  - [x] Migrate remaining arithmetic/comparison, property/update, call,
    construct and Promise/await lowering, then prevent backends from bypassing
    the intrinsic layer for JavaScript semantics.
    - [x] Route every binary operator emitted by the remaining direct-AST
      module/class initialization path through `w3cos_core::intrinsics`.
      W3VM and the native W3IR emitter now also share the same `logical_not`
      and `instance_of` entry points for inequality and `instanceof`, with a
      compiler guard that rejects reintroducing direct `Value::js_*` binary
      calls in module initialization.
    - [x] Route the remaining direct-AST module/class peripheral property
      reads, writes and deletion, updates, calls, construction, destructuring,
      fields and private-member installation through `w3cos_core::intrinsics`.
      Compiler guards reject direct `Value` and class semantic bypasses in
      module initialization and class assembly.
    - [x] Put `super` dispatch, Promise construction/combinators and
      PromiseResolve-based await assimilation behind the same Core intrinsic
      ABI. Direct-AST ESM, native W3IR async/generator state machines and W3VM
      now reuse those entry points; generated-code and VM source guards reject
      direct Promise/class bypasses.
- [ ] Make the native AOT backend generate code from W3IR and call the same runtime intrinsics used by W3VM
- [x] Migrate existing SWC → Rust lowering incrementally to SWC → W3IR → Rust;
  remove long-lived direct AST → Rust semantic paths. Native code generation
  still reads AST declarations for module/class structure and symbol naming,
  but executable JavaScript expressions and bodies enter validated W3IR.
  - [x] Compile supported synchronous ESM evaluation statements directly from
    their parsed SWC AST into W3IR and then native Core-only AOT helpers.
    Existing ESM cells/imports enter W3IR as explicit external live bindings;
    getter/setter adapters propagate through nested closures, verified by a
    generated module-init callback that mutates a live export from `1` to `6`.
    - [x] Represent object/array module-level destructuring as W3IR assignments
      to the already-linked live bindings, eliminating the init-local plus
      codegen-only write-back path. A generated native fixture covers nested
      defaults, object/array rest and seven live cells with result `36`.
  - [x] Represent asynchronous module evaluation with the existing W3IR
    ordinary-async native state machine and Core Promise/module-registry ABI.
    Native modules cache their evaluation Promise; the bundle entry sequences
    modules in topological order before `main`. Executed fixtures cover
    identifier initializers containing top-level await across two modules
    (`SDA4`) and rejection propagation that skips later statements and `main`.
  - [x] Coalesce adjacent synchronous identifier/destructuring initialization
    and executable statements into bounded W3IR AOT segments. Segments contain
    at most 32 statements, prune unused external bindings before AOT emission,
    and therefore avoid both per-declarator helpers and quadratic capture
    adapters; a 65-initializer fixture produces exactly three frames.
  - [x] Replace pre-evaluation lazy identifier getter initializers with
    Core-owned module binding cells. Native registration now performs a
    distinct declaration-instantiation phase: `var` begins as `undefined`,
    lexical declarations remain uninitialized until their source-ordered W3IR
    assignment, live getters never initialize on read, and cyclic TDZ reads
    throw a standard `ReferenceError`. An executed A ↔ B native fixture covers
    the cycle and the shared JavaScript exception ABI.
  - [x] Remove the direct-AST semantic fallback and empty diagnostic
    placeholders for residual module-init segments. Native generation now has
    a fallible W3IR-only API; failures identify the module, phase, bounded
    chunk and lowering cause, propagate through the production compiler, and
    cannot downgrade to the legacy transpiler or emit a partial bundle. CI
    guards the boundary and executed tests cover both supported pure-W3IR
    modules and an explicitly rejected tagged-template initializer.
  - [x] Remove the residual direct-AST function-body emitter and generated
    runtime-error stubs for ordinary synchronous, async and generator symbols.
    All three callable shapes now either emit from validated module W3IR or
    fail native generation immediately with module, callable kind, symbol and
    backend cause; production compilation propagates the same typed diagnostic.
    TypeScript ambient function/class/variable declarations are erased during
    resolver and codegen collection instead of becoming phantom runtime
    symbols. CI forbids restoring the old emitters/stub strings, and focused
    tests cover all three failure shapes plus ambient erasure.
  - [x] Remove class method/getter/setter/constructor runtime W3IR failure
    stubs. Explicit class callables now require matching sync, async or
    generator W3IR plus supported native capture mapping, and fail native
    compilation with module/class/member context instead of deferring an
    unsupported path to application startup. Spec-defined default constructors
    remain generated directly.
  - [x] Generate top-level native class public/private field values and static
    blocks from the same W3IR synthetic initializer functions consumed by
    W3VM. Field keys are evaluated once at class definition and cached by their
    verified W3IR capture identities; instance initialization still runs
    through the shared Core class scheduler, while static fields and blocks run
    in source order after the class binding becomes visible. Unsupported field
    or static-block lowering now fails native compilation instead of returning
    to direct AST bodies. This left `extends` and computed member/key evaluation
    as the final executable class expressions outside W3IR.
  - [x] Move the remaining top-level native class `extends`, public field keys
    and computed public method/accessor keys into a source-ordered W3IR
    definition-values function. Native class assembly consumes and caches
    those results instead of calling the AST expression emitter. W3VM now also
    prepares field and method keys in their shared source order before class
    creation, fixing the former field-first ordering. A source-identical
    executed AOT/W3VM fixture covers inheritance, interleaved field/method
    keys, static initialization, private fields and observable ordering.
    Remaining AST ownership is structural class/member assembly only.
- [x] Add differential conformance tests: the same fixture must produce equivalent results under AOT and W3VM
  - [x] Add the first executable differential fixture covering Host calls,
    dynamic property writes/reads and ECMAScript addition through the exact
    same intrinsic functions.
  - [x] Expand the fixture corpus across coercion, control flow, closures,
    exceptions, classes, Promise/async and modules.
    - [x] Execute one source-identical ESM fixture through generated native AOT
      and W3VM, covering captured nested closures, local class construction
      and method dispatch, branch/strict comparison, `throw`/`catch`, `typeof`
      and chained ECMAScript addition. Both backends produce `number:5`.
    - [x] Generate ordinary async AOT functions from validated W3IR suspension
      metadata, as already done for generators. Core-only native frames retain
      registers and lexical cells across fulfillment/rejection, resume the
      recorded blocks, recursively create captured nested async functions, and
      settle through the shared Promise/microtask engine. A source-identical
      two-`await` fixture now returns `5` in both generated AOT and W3VM and
      propagates `throw` as the same rejected Promise; a second differential
      exercises rejected awaited input and a captured nested async closure.
    - [x] Execute a source-identical two-module graph through generated AOT and
      W3VM. The async importer suspends between two reads while a dependency
      function mutates an exported live cell; both backends fulfill with
      `1:3:3`, proving import capture, Promise resumption and post-mutation live
      binding reads share semantics.
    - [x] Execute a source-identical timer, microtask and DOM Host-call fixture
      through generated AOT and W3VM. An injected VM Host and the real AOT
      jsdom host both preserve synchronous `S`, microtask `M`, following task
      `T`, captured lexical mutation and `document.body` attribute
      round-tripping as `S:SMT|SMT`.
- [x] Version W3IR/bytecode and reject incompatible modules before execution;
  current-format serialization round-trips and unknown versions fail
  validation.

### W3VM dynamic execution
- [ ] Implement bytecode generation, verifier, operand stack, call frames, lexical environments, closures, `this`, and prototype lookup
  - [x] Implement the initial register interpreter, structural verifier, call
    frames, lexical cells, live closure captures, `this` plumbing, branches,
    calls and property operations.
  - [x] Lower initial SWC function expressions and arrow callbacks with
    identifier parameters, return/expression bodies, and transitive live
    lexical captures into `CreateClosure`.
  - [x] Lower initial real CFG for `if`/`else`, `while`, unlabeled
    `break`/`continue`, block shadowing, and function-scoped `var` hoisting
    into W3IR `Branch`/`Jump`.
  - [x] Extend that CFG to classic `for` and `do...while`, prefix/postfix
    identifier/member updates, arithmetic/exponent/logical compound
    assignments, parenthesized and comma expressions. `&&=`, `||=` and `??=`
    reuse the same backend-neutral branches as their expression forms, evaluate
    identifier/public/private targets once, and skip the right operand when
    required; an immutable binding throws only if its write branch is reached.
    `++`/`--` use numeric coercion rather than addition
    concatenation, while `typeof` calls the shared Core value semantic through
    W3IR and W3VM.
  - [x] Lower `switch` with strict case comparison, default selection and
    source-order fall-through. One ordered control-target stack preserves
    switch `break`, loop `break`, and loop `continue` when the constructs are
    nested. Bitwise/shift expressions and compound assignments use shared Core
    Int32/Uint32 coercion through W3IR v4.
  - [x] Hoist direct function declarations before script, module and nested
    function-body evaluation; exported declarations use the same W3IR module
    path, and identifier parameters support left-to-right default evaluation.
  - [x] Lower fixed object/array destructured parameters, nested/default
    patterns and final rest parameters. W3IR v5 carries the rest binding in the
    function ABI and W3VM creates its array in the shared call frame.
  - [x] Lower array rest elements and object rest properties through W3IR v6
    `ArrayRest`/`ObjectRest`; W3VM delegates slicing and excluded-key copying to
    the same Core intrinsics available to AOT code.
  - [x] Implement `for (let/const ...)` per-iteration environments through
    W3IR v7 `RefreshBinding`. W3VM replaces the current frame cell before the
    first condition and each update, so initializer closures retain the
    declaration cell while body closures retain their own iteration cell;
    `continue` follows the same refresh/update block.
  - [x] Route direct computed and named member calls through W3IR v8
    `CallMethod`. W3VM delegates Array/String built-ins and ordinary Host
    receiver calls to the existing Core method semantic, so dynamic scripts
    do not need a second prototype-method implementation.
  - [x] Refresh nested block and switch lexical cells on every entry using the
    existing W3IR `RefreshBinding`, so closures retain values from repeated
    loop/switch evaluations. Block/switch function declarations are
    predeclared as lexical bindings and initialized at scope entry.
  - [x] Lower nested array/object destructuring for `var`, `let`, `const`,
    classic `for` initializers and exported declarations using the same
    `GetProperty`, `ArrayRest` and `ObjectRest` instructions as parameter
    patterns. Defaults initialize left-to-right against predeclared cells, so
    later bindings retain TDZ behavior, and destructured exports create one
    live module binding per bound name.
  - [x] Route array/object destructuring reassignment expressions through those
    same W3IR pattern writes. Nested defaults/rest, computed keys, identifier,
    public-member and private-field targets preserve source order and return
    the original right-hand value in W3VM and generated native AOT.
  - [x] Lower ordinary template literals as ordered W3IR `Add` chains so cooked
    escapes and interpolation coercion use the same Core semantics in W3VM and
    native AOT. Runtime-erased TypeScript `as`, angle-bracket assertion,
    non-null, const assertion, `satisfies`, and instantiation wrappers lower
    their inner expression through that same path.
  - [x] Erase TypeScript `interface`, type aliases, ambient
    function/class/variable declarations, and ambient enum/namespace
    declarations before W3IR binding and export construction. Declarations
    with runtime behavior (`enum`, namespace and `using`) remain explicit
    lowering errors instead of silently diverging between W3VM and AOT.
    Type-only imports, exports and re-exports also stay out of the runtime
    module-request graph.
  - [x] Lower synchronous `for...of` over the shared Core iterator protocol
    into existing W3IR `CallMethod`/property/branch instructions. Declaration
    and assignment heads support nested patterns and member targets;
    `let`/`const` cells refresh per iteration; Unicode strings use the same
    immutable iterator snapshot as AOT, while arrays, typed arrays, Map and Set
    share Core's live `ValueIterator` across AOT and W3IR. Array length changes
    and typed-array writes to unvisited indices are observed at each step;
    Map/Set deletions are skipped, later additions are visited, and
    delete-then-reinsert entries are visited again; and
    `break`, explicit `return`, and explicit `throw` perform the iterator
    `return()` hook when present. Nested abrupt completion closes iterators
    from inner to outer; an existing throw completion wins over close-hook
    failures, while a close failure replaces a return completion. Expression
    and Host-call failures inside the loop body now enter existing W3IR
    `ExceptionRegion` handlers and follow the same close-and-rethrow path;
    return cleanup runs in an unprotected block so hooks execute exactly once.
    Shared Core protocol bridges now require callable iterator, `next`, and
    `return` methods plus object-valued iterator/step/close results. Iterator
    step failures do not spuriously close; a non-callable `return` GetMethod
    failure overrides an existing throw, while call/result failures preserve
    that throw as required by IteratorClose completion priority. AOT
    `Value::iter` and W3IR iterator acquisition now use the same custom
    `Symbol.iterator` validation and step driver, and Core-created iterator
    objects are themselves iterable. TypedArray default iteration and explicit
    `values` / `keys` / `entries` now also use that shared live iterator-object
    path instead of maintaining a separate snapshot implementation.
  - [x] Lower `for-await-of` through the same W3IR loop CFG and W3VM async
    frames used by ordinary `await`: Core performs validated async-iterator
    acquisition with synchronous-iterator fallback, W3IR awaits both `next`
    results and yielded values, and break/return/throw paths await the shared
    async IteratorClose chain with JavaScript completion priority.
  - [x] Lower labeled blocks, multi-label loops, and labeled `break` /
    `continue` to explicit W3IR CFG targets. Targets record their iterator
    depth so transfers across nested synchronous or asynchronous `for-of`
    loops close exactly the iterators they leave while keeping the destination
    iterator open for `continue`.
  - [x] Lower Annex B branch-level function declarations for non-strict
    classic scripts through the existing function-scoped `var`,
    `CreateClosure`, and `StoreBinding` path. Bindings hoist as `undefined` and
    only the selected `if` / `else` branch installs its closure; strict scripts,
    strict nested functions, and ESM reject the legacy form explicitly.
  - [x] Preserve parameter-list TDZ and left-to-right initialization by routing
    raw call arguments into hidden ABI cells, then initializing visible
    identifier, default, destructured, and rest bindings through the existing
    lexical-cell instructions. Earlier defaults and closures can observe later
    initialization, while reads or writes of a later parameter during an
    earlier default raise `ReferenceError`.
  - [x] Lower `for-in` through a shared Core `for_in_keys` snapshot and the
    same W3IR iterator CFG used by `for-of`; AOT calls the same intrinsic.
    `var`, per-iteration `let` / `const`, assignment/member heads, closures,
    and abrupt cleanup share one binding/iterator implementation. Core walks
    the prototype chain once, suppresses shadowed duplicate keys, and protects
    against malformed prototype cycles for both execution modes.
  - [x] Execute W3IR v11 `CreateClass` and both AOT class emitters through the
    same Core class builder instead of backend-owned object models. Classic
    scripts and named/default ESM exports support class declarations and
    expressions, captured constructors, instance/static methods, ordinary and
    arrow lexical `this`, `new`, `instanceof`, prototype/static inheritance,
    explicit/default derived constructors, `super(...)`, and instance `super`
    method/property reads. Instance/static getters and setters, including
    computed names, use the same Core accessor convention as AOT. Static
    `super` method/property access preserves the derived receiver through
    ordinary methods and lexical arrow closures. Instance/static `super`
    property writes, arithmetic/logical compound assignments, and prefix/
    postfix updates now use one W3IR assignment target plus Core get/set
    bridges; computed keys evaluate once and setters retain the derived
    receiver in W3VM and native AOT.
  - [x] Schedule public instance fields through a separate initializer closure
    passed by W3IR/W3VM and both AOT emitters to the same Core class builder.
    Base fields run before the constructor body; derived fields run immediately
    after their own `super(...)` returns, including default constructors and
    multilevel inheritance. `DefineField` bypasses prototype setters, computed
    keys are captured at class definition, and static fields initialize the
    class object once. Static blocks share the ordered class-initialization
    sequence with static fields and preserve block lexical scope, class-bound
    `this`, and static `super`.
  - [x] Give runtime-parsed classes real unobservable private brands and slots
    in the shared Core object model. W3IR v12 defines private field, method,
    accessor, get/set and brand-check operations; W3VM delegates every one to
    Core. Base/derived brands install at the same `super(...)` boundaries as
    public fields, private calls preserve their receiver, and private state is
    absent from ordinary properties, proxy traps and reflection keys.
  - [x] Migrate both AOT emitters from legacy string-mangled private properties
    to the same Core private operations. Top-level and captured class
    expressions share field/method/accessor/brand-check behavior with W3VM;
    generated Rust runs the same compound-update, receiver and accessor
    fixture, while Core tests cover inheritance, wrong-brand failure and
    reflection invisibility.
  - [x] Complete generators and remaining control flow.
    - [x] Advance W3IR to v13 with validated generator suspension metadata and
      `Yield` / `YieldDelegate` operations. W3VM now creates lazy resumable
      generator frames using the shared Core `Value` and iterator protocol;
      `next(value)`, `throw`, `return`, `yield*`, nested `try` / `catch` /
      `finally`, iterator-close on loop exit and finalizers that yield preserve
      JavaScript completion behavior. Browser-loaded scripts execute the same
      path without runtime Rust compilation.
    - [x] Run async generators on the same W3IR v13 suspension metadata and
      W3VM Promise/microtask queue. `next` / `throw` / `return` requests queue
      in order; awaited expressions, yielded thenables and return completions
      resume through one preserved frame; async `yield*` forwards delegated
      completions with synchronous-iterator fallback; and `for-await-of`
      awaits values plus IteratorClose. Compiler, direct-VM and browser-loaded
      script fixtures cover these paths.
    - [x] Generate native AOT generator state machines from the same W3IR v13
      suspension metadata, then add AOT/W3VM differential fixtures before
      declaring generator support complete.
      - [x] Emit top-level synchronous ESM `function*` bodies as native Rust
        state machines directly from validated W3IR blocks and suspension
        records. Generated applications link Core only, use live getter/setter
        adapters for captured ESM cells, and preserve `next` / `throw` /
        `return`, yielding finalizers, Host exception re-entry and synchronous
        `yield*`. An executed AOT/W3VM differential fixture covers completion
        injection, while the full ESM AOT fixture covers live captures,
        delegated throws and Core-only linkage.
      - [x] Route top-level function-valued generator variables, including
        anonymous `const value = function* () {}` forms, and public
        literal-named instance/static class generator methods through the same
        W3IR AOT emitter. Stable W3IR identities distinguish static and
        instance members; executed Core-only fixtures cover live module
        captures, dynamic `this`, parameters and same-key static/instance
        methods.
      - [x] Emit nested generator factories created inside an AOT generator
        from W3IR `CreateClosure`, with `Rc<RefCell>` binding cells shared
        through the existing capture-adapter ABI. A Core-only differential
        fixture proves bidirectional outer/inner local writes across
        suspension against W3VM; unsupported ordinary nested closures are
        rejected rather than approximated.
      - [x] Emit computed, private and `super`-capturing class generator
        methods. Computed members receive stable source-order W3IR identities;
        class brand and parent cells use explicit immutable capture adapters;
        private field get/set/brand checks use the same Core intrinsics as
        W3VM. A Core-only execution fixture covers computed lookup, private
        mutation, and instance `super` method/getter dispatch.
      - [x] Emit non-async ordinary W3IR functions that create and return
        nested generators through a synchronous Core-only block runner. The
        escaping generator retains the host invocation's live local cells;
        modules are lowered once opportunistically so selection does not
        require a second syntax heuristic.
      - [x] Generalize the closure graph emitter to mixed synchronous and
        generator functions. Escaping ordinary helpers and nested generators
        share the same factory ABI and live cells; an executed Core-only
        fixture covers a generator calling an ordinary captured helper after
        its host invocation has returned. Async closure nodes remain explicit
        failures.
      - [x] Complete the Core-backed W3IR object/class surface in the AOT
        runner: rest parameters, array/object rest, class creation and
        initializer wiring, private method/accessor definition, fields and
        brand operations. Executed fixtures cover destructuring/rest and a
        local class whose ordinary method closure is called from a generator.
      - [x] Emit `import.meta.url` from the validated W3IR module identity; a
        dedicated small Core-only fixture verifies the generated URL without
        inflating the larger state-machine fixture's test-thread stack.
      - [x] Route AOT W3IR `DynamicImport` through the optional
        `w3cos/module::dynamicImport` Core host adapter with `(specifier,
        referrer)` arguments. Missing adapters return a rejected Promise;
        configured adapters preserve Core-only linkage and are verified by the
        same generated fixture as `import.meta`.
      - [x] Emit async-generator Promise request queues directly from the same
        W3IR suspension records. Core-only generated frames serialize
        `next`/`return`/`throw`, adopt awaited and yielded thenables, resume
        rejection blocks, await completion values, and forward async `yield*`
        including delegated return/finalizer paths. Direct AOT/W3VM
        differential fixtures cover queued await/yield/completion and nested
        async delegation; an executed ESM fixture verifies selection of the
        native W3IR factory without linking W3VM or W3IR.
      - [x] Route every top-level ordinary synchronous ESM function
        declaration, function-valued variable and arrow-valued variable
        through the Core-only W3IR AOT runner. Missing module/function
        lowering is an explicit generated failure and never silently selects
        the legacy direct-AST semantic backend.
      - [x] Route explicit top-level class constructors plus public/private,
        static/instance methods and accessors through W3IR AOT. Accessor and
        method identities encode kind/static/private distinctions; only
        specification-defined default constructors remain synthesized
        directly.
      - [x] Extend the shared W3IR expression surface used by ordinary AOT and
        W3VM with optional member/call chains (including receiver preservation
        and skipped argument evaluation), RegExp and BigInt literals, BigInt
        unary negation/bitwise-not, exponentiation, `in`, `delete`, named
        default function declarations, object/array/call/construct spread,
        sparse array holes across property presence, enumeration, callbacks,
        JSON and spread iteration,
        object-literal methods/getters/setters (including generator methods),
        BigInt property keys, TypeScript constructor parameter properties with
        base/derived initializer ordering, short-circuit logical assignments,
        array/object destructuring reassignment, instance/static `super`
        property assignment and update targets, ordinary template literals,
        runtime-erased TypeScript expressions and type-only/ambient
        declarations, and JSX element/fragment values including ordered
        spread attributes.
        Compiler, VM and generated browser/Core fixtures exercise the same
        `CopyDataProperties`/other intrinsics and Promise constructor facade.
        Dynamic Realms also resolve the standard Object, Array, Math and JSON
        facades from Core when the host window does not override them, keeping
        browser globals on the same semantics as ordinary AOT.
- [x] Implement exceptions, Promise/microtask integration, async/await suspension, and Host-call re-entry
  - [x] Implement `throw`, protected catch regions and Host-call exception
    re-entry.
  - [x] Expose the shared core `Promise` constructor/static methods to the page
    Realm; W3VM closures run as `then` reactions on the existing microtask
    queue, including `Promise.resolve` and `new Promise`.
  - [x] Run the shared microtask checkpoint after dynamic script evaluation;
    native window task turns already drain the same Promise/queueMicrotask
    queues.
  - [x] Implement W3IR `Await` suspension with resumable W3VM frames, preserved
    registers/lexical cells, shared Promise adoption, and async rejection
    routing. Dynamic-script tests cover multiple awaits and rejected awaits.
  - [x] Materialize `try` / `catch` / `finally` completion paths in W3IR and
    expand exception-region coverage across suspended W3VM frames. Normal
    completion, `return`, `throw`, labeled/unlabeled `break` / `continue`,
    Promise rejection and Host-call exceptions execute finalizers in order;
    a finalizer's own abrupt completion overrides the pending one. Browser
    dynamic-script tests cover `await` in both the protected body and
    finalizer, while compiler fixtures cover nested finalizers and captured
    compound/update assignments.
  - [x] Lower identifier, array and object catch bindings through the same
    W3IR lexical cells and destructuring operations used by declarations and
    parameters. Nested defaults, computed keys and rest bindings preserve
    catch-scope TDZ and execute unchanged in W3VM, generated native AOT and
    browser-loaded scripts. Binding TDZ and immutable-write failures use
    Core-created standard Error values and enter the same catch regions in
    both backends instead of escaping W3VM as host errors.
- [x] Implement mixed-module linking: AOT modules may import/call bytecode modules and bytecode modules may import/call AOT modules
  - [x] Implement the initial runtime ESM linker for bytecode module graphs:
    URL resolution, source/module caches, named/default/namespace imports,
    local and named re-exports, live lexical cells, live namespace getters,
    dependency-order evaluation, and cycle-safe instantiation all execute
    through the same W3VM and Core `Value` ABI.
  - [x] Add a Core-owned module-record ABI whose exports are live
    getter/setter `Value` pairs, keeping compiler, W3IR and W3VM types out of
    the boundary. Generated AOT modules register direct function/class/variable
    exports before evaluation; W3VM import slots can read registered AOT cells;
    runtime bytecode records register their own live exports and evaluator in
    the same registry. Graph discovery, linking and evaluation skip network
    fetches for registered AOT records. Executed fixtures cover generated AOT
    live mutation plus bytecode → AOT and AOT-style → bytecode calls.
  - [x] Extend the shared record adapter across named aliases, named default
    declarations and star re-exports. The AOT bundle now retains each module's
    resolved public surface and registers cross-module live cells; bytecode
    star records enumerate/read native Core exports without forwarding
    `default`. Executed fixtures cover aliased/star AOT registration and a
    bytecode barrel forwarding a mutating AOT binding. Core also owns chained
    specifier aliases, so a deployment/CDN URL resolves to the same registered
    live record instead of duplicating module state; the mixed linker fixture
    exercises that path. Anonymous default expressions and anonymous
    function/class declarations now receive stable synthetic AOT cells;
    executed coverage verifies default-expression snapshot semantics across an
    actual default import and the shared registry. Namespace re-exports use
    immutable synthetic cells whose namespace properties remain live; the same
    executed barrel fixture covers `export * as ns`.
  - [x] Route native and bytecode entry evaluation through one Core record
    state machine. Successful, rejected and pending evaluations are cached;
    active mixed-graph back edges reuse the already-instantiated live cells
    without awaiting their own Promise. A real W3VM module and registered AOT
    evaluator form an executed AOT → bytecode → AOT SCC and each evaluates
    exactly once.
  - [x] Populate canonical aliases from deployment metadata and redirect-final
    URLs. `AppManifest.module_aliases` is deserialized and installed when an
    app enters the registry; HTTP request/final module URLs now alias one Core
    record rather than registering duplicate evaluation state. The redirect
    fixture proves both URLs evaluate once.
  - [x] Lower manifest-declared runtime-only ESM imports in generated AOT
    modules into Core registry live slots. The compiler reads
    `runtime_modules` from `w3cos.app.json`/`w3cos.json`, excludes those
    specifiers from the static source graph, and generates named/default/
    namespace accessors plus side-effect evaluation through the public Core
    ABI. An executed generated-AOT fixture calls a runtime function, observes
    its mutated live export, and proves Core evaluates the runtime record once
    without linking W3VM or W3IR into the AOT binary.
  - [x] Add a Promise-shaped generated AOT entry that starts every declared
    runtime dependency through the existing Core dynamic-import adapter,
    waits for their cached Core evaluation Promises, and only then runs AOT
    module initialization and `main`. Generated desktop and mobile DOM
    launchers use this entry; ordinary all-AOT bundles still execute
    synchronously inside an already-fulfilled Promise. `ScriptLoader`
    installs the weak page-scoped adapter to reuse its existing fetch,
    redirect/import-map, SWC → W3IR → W3VM and live-module path. Executed
    fixtures cover pending top-level `await`, live export visibility,
    rejection propagation, automatic adapter loading and missing-loader
    failure without introducing another evaluator or scheduler.
- [x] Add runtime bytecode cache keyed by resolved URL, ETag/Last-Modified or
  content hash, W3IR version, and compile options
  - [x] Add the shared in-memory classic-script/ESM cache keyed by resolved
    URL/specifier, verified source digest/length, W3IR format version, and
    compile mode.
    It removes duplicate module lowering between graph discovery and
    instantiation and survives page Realm teardown without preserving module
    evaluation state.
  - [x] Bound the in-memory cache by configurable entry and estimated resident
    byte budgets, evict least-recently-used entries, and expose hit/miss,
    eviction, entry, and byte counters.
  - [x] Persist compiled W3IR in an embedder-owned application-private cache
    directory using atomically replaced artifacts, exact source/key checks,
    W3IR structural validation, bounded cleanup, and persistent-cache
    telemetry. Invalid artifacts fall back to the same lowering path.
  - [x] Integrate ETag/Last-Modified conditional requests for synchronous and
    task-pump classic scripts plus ESM graphs. The script adapter now consumes
    an atomically persisted, application-private generic browser response entry
    containing status, binary body, safe headers, validators and `Vary`
    metadata; 304 responses merge current metadata, re-run existing
    redirect/CORS/MIME/source-limit checks, and reuse the same W3IR/W3VM path
    without downloading the body. Changed 200 responses replace source and
    W3IR, corrupt sidecars fall back to an unconditional fetch, same-origin
    redirects revalidate only when the final URL is unchanged, cross-origin
    redirect validators are not forwarded, sensitive response headers are not
    persisted, `Cache-Control: no-store` responses remove any prior artifact,
    and the script adapter conservatively bypasses responses with `Vary`.
    Generic consumers can key and verify the named request-header dimensions.
    Shared disk budgets prune response and W3IR artifacts together, and public
    counters expose candidates, misses, 304 hits, refreshes, writes, evictions,
    and errors.
- [x] Start with an interpreter; defer JIT until profiling proves it necessary.
- [ ] Add heap limits, instruction/time budgets, cancellation, and deterministic cleanup for untrusted page code
  - [x] Add instruction and call-depth limits plus a reusable cancellation
    token.
  - [x] Add shared Core heap accounting for objects, arrays and functions,
    including container-capacity growth, per-page ownership, live/peak and
    allocation diagnostics. W3VM enters the same owner scope for synchronous
    execution, async continuations and generator resumes, exposes the snapshot,
    and enforces a configurable estimated live-byte cap (64 MiB by default).
    Native AOT and Host code use these same allocation tickets when entering a
    page owner; opaque Rust closure captures and external native resources
    remain explicitly outside the estimate.
  - [x] Add a configurable cumulative active wall-clock deadline shared by
    synchronous frames, async continuations and sync/async generator resumes.
    The default budget is five seconds, embedders can disable it with `None`,
    and time suspended at `await`/`yield` boundaries is excluded so network,
    timer and host waits do not consume page execution time. Exhaustion remains
    an uncatchable VM termination and is enforced by `ScriptPolicy`.
  - [x] On navigation and loader destruction, cooperatively cancel every
    retained runtime-module VM, unregister its requested/final URL records and
    aliases from the shared Core module registry, and preserve native/AOT
    providers. Suspended top-level-await continuations now reject instead of
    resuming stale page code.
  - [x] Advance the shared Core Promise microtask Realm generation during
    bridge reset, after cancelling retained W3VMs. Navigation now drops queued
    microtasks, timers and animation-frame callbacks, and old pending Promise
    subscriptions cannot re-enqueue stale page callbacks when they settle
    later. A new Realm can still subscribe to a cached settled native/AOT
    module Promise, preserving the shared module cache and single scheduler.
  - [x] Treat memoized `document`, `window` and `Selection` wrappers as
    Realm-owned rather than process-thread singletons. Reset now releases their
    object graphs after subsystem teardown, rebuilds fresh identities on
    demand, clears scroll/fullscreen state, and recreates mutable nested
    Navigation, Screen, window-environment, FragmentDirective and active
    ViewTransition wrappers without carrying authored expandos into the next
    document.
  - [x] Advance a DOM-bridge Realm generation before recycling document node
    ids. Externally retained element proxies, live collections, node-owned
    style/dataset/canvas facades and previously retrieved DOM methods now
    become inert instead of reading or mutating a same-numbered node in the
    next document. Reset also rebuilds the core DOM constructor/prototype graph
    and local jsdom class caches so authored prototype mutations do not cross
    navigation.
  - [x] Rebuild the Custom Elements registry, `ElementInternals`,
    `CustomStateSet` and `CSSPseudoElement` constructor/prototype graphs on
    Realm reset. Definitions and pending `whenDefined()` waiters are released,
    while externally retained registry methods and constructors are guarded by
    the shared DOM-bridge generation so they cannot query or register elements
    in the next document.
  - [x] Rebuild the process-local Cache API and Prioritized Task Scheduling
    singleton/constructor graphs on Realm reset. The shared DOM-bridge
    generation now makes externally retained `Cache`, `CacheStorage`,
    `Scheduler`, `TaskController` and `TaskSignal` methods inert, preventing old
    pages from mutating the next page's cache namespace or injecting work into
    its timer and Promise queues.
  - [x] Rebuild Web Locks and CSS Custom Highlight singleton/constructor
    graphs on Realm reset. Lock request ids and asynchronous release/abort
    paths now carry the shared DOM-bridge generation, so stale signals and
    completions cannot cancel or release next-page locks; retained Highlight
    and Registry methods cannot preserve or act on old Range graphs.
  - [x] Make Clipboard and Launch Handler state Realm-safe. Clipboard payloads
    now survive navigation as typed byte snapshots and are rehydrated into
    fresh `ClipboardItem`/`Blob` wrappers instead of retaining old JS values;
    stale Clipboard and LaunchQueue methods are generation-guarded, old
    consumers and pending launch values are released on reset, and
    `LaunchParams` no longer writes per-instance fields onto its shared
    prototype.
  - [x] Rebuild Permissions, Credential Management and CloseWatcher
    constructor/prototype graphs on Realm reset. Retained entry methods,
    watcher lifecycle callbacks and abort listeners are guarded by the shared
    generation, so old documents cannot query current permission state,
    construct credentials in the next Realm or dispatch stale close/cancel
    handlers.
  - [x] Rebuild StorageManager, Storage Buckets, Wake Lock and Network
    Information JS object graphs on Realm reset. Storage bucket metadata keeps
    its storage lifetime while stale managers and bucket wrappers cannot
    enumerate, delete or mutate it; old wake-lock sentinels cannot dispatch
    release events, and authored constructor/prototype mutations do not cross
    navigation.
  - [x] Rebuild Compute Pressure, Presentation and Barcode Detection
    constructor/prototype graphs on Realm reset. Page teardown drops pressure
    observers and queued delivery, while generation-guarded constructors,
    request methods and detector entry points keep retained objects from
    invoking callbacks or creating work in a later document.
  - [x] Rebuild Notification, User Activation and EditContext object graphs on
    Realm reset. Retained notification constructors and permission callbacks
    cannot trigger host work, activation getters cannot observe a later
    document's trusted-input state, and stale editing models cannot mutate text
    or dispatch events after navigation.
  - [x] Rebuild IdleDetector, EyeDropper, Observable and Subscriber object
    graphs on Realm reset. Host idle-state delivery releases old detectors,
    retained screen-sampling and subscription entry points become inert, and
    teardown clears subscriber callback/teardown references so old producers
    cannot re-enter a later page.
  - [x] Make ResizeObserver, MutationObserver and IntersectionObserver delivery
    Realm-owned. Navigation rebuilds constructors and entry prototypes, cancels
    pending records, releases callback/target graphs, and makes retained
    observer methods inert before a later document becomes active.
  - [x] Make PerformanceObserver and performance-timeline object graphs
    Realm-owned. Navigation cancels pending entry delivery, releases observer
    callbacks and buffered records, rebuilds timeline/entry/diagnostic classes,
    and prevents retained entry-list or serialization functions from entering
    the next page.
  - [x] Make Worker, SharedWorker, MessagePort, MessageChannel and
    BroadcastChannel resources Realm-owned. Navigation terminates native
    workers, closes and detaches port/channel graphs, cancels queued broadcast
    delivery, releases event callbacks, and rebuilds all exposed constructors.
  - [x] Make Canvas Web object graphs Realm-owned. Navigation rebuilds
    OffscreenCanvas, Path2D, gradient, pattern, bitmap, context, capture-track
    and text-metrics classes, clears retained path storage, and makes stale
    drawing/resource methods inert.
  - [x] Make XPath and Sanitizer objects Realm-owned. Navigation rebuilds
    evaluator/expression/result and sanitizer classes, and prevents retained
    namespace resolvers, DOM result iterators, configurations or fragment
    creation methods from operating on the next document.
  - [x] Make Web Bluetooth object graphs Realm-owned. Navigation rebuilds the
    Bluetooth, device, UUID and GATT interface classes, while retained
    discovery, connection, service, characteristic, descriptor and
    notification methods can no longer invoke the platform adapter.
  - [x] Make WHATWG Streams object graphs Realm-owned. Navigation rebuilds the
    readable, writable, transform, text-codec, compression and queuing-strategy
    classes; clears queued chunks, pending reads, sources, sinks and tee
    coordination; and disables retained readers, writers, controllers, async
    iterators and pipe/tee Promise pumps.
  - [x] Make WebSocket, EventSource, and XMLHttpRequest network object graphs
    Realm-owned. Navigation requests WebSocket closure, marks EventSource
    handles closed, clears network listeners and XHR response/upload graphs,
    rebuilds their constructors, and makes retained network methods inert in
    later Realms. EventSource also participates in pending-work/deadline polling.
  - [x] Make the shared Web Events graph Realm-owned. Navigation rebuilds
    Event, CustomEvent, EventTarget, Touch and all event-subclass constructors,
    clears registered listeners and callable `on*` handlers, and makes retained
    event/target/observable methods inert before the next document runs.
  - [x] Make the shared Fetch object graph Realm-owned without duplicating its
    transport. Navigation rebuilds stable Headers, Request, Response,
    AbortController and AbortSignal constructors, releases abort listeners and
    handlers, cancels stale signal state, and makes retained body/header/request
    methods inert while AOT, XHR and W3VM continue sharing one Fetch pipeline.
  - [x] Make media-capture facades Realm-owned while retaining one native-adapter
    boundary. Navigation rebuilds MediaDevices, MediaStream, MediaStreamTrack,
    processor/generator, device-info, constraint-error and stats constructors,
    releases page callbacks and generator registrations, and makes retained
    capture/stream/track methods inert.
  - [x] Make WebRTC signaling and media graphs Realm-owned without adding a
    second transport. Navigation rebuilds peer/data-channel/RTP/ICE/DTLS/SCTP
    constructors, releases EventTarget handlers, queued negotiation work,
    signaling descriptions, tracks, streams, workers and encoded-stream
    references, disconnects the complete constructor/prototype graph, and makes
    retained connection, channel, sender, receiver, transceiver and stats
    methods inert while preserving the single native-adapter boundary.
  - [x] Make Service Worker compatibility and companion-manager objects
    Realm-owned without introducing a placeholder execution backend.
    Navigation rebuilds the container, registration, worker, Background Fetch,
    Cookie Store, navigation-preload and sync classes, releases container event
    handlers plus manager promise/method references, disconnects all ten
    constructor/prototype graphs, and makes retained registration/discovery
    methods inert.
  - [x] Make WebCodecs controllers and media containers Realm-owned while
    preserving one native codec-adapter boundary. Navigation cancels queued
    codec work, releases output/error/dequeue callbacks, closes registered
    controllers, rebuilds codec/data/chunk classes, and makes stale frame,
    chunk and controller methods inert without retaining completed media
    buffers until navigation.
  - [x] Make Web Audio constructors, contexts and graph operations Realm-owned
    while preserving one native audio-device/decoder/worklet adapter boundary.
    Navigation closes registered contexts, releases context and processor/source
    callbacks, rebuilds the constructor graph, and makes stale buffer, context
    and node methods inert without retaining ordinary buffers or graph nodes
    until navigation.
  - [x] Make ImageDecoder, ImageTrack and ImageTrackList Realm-owned while
    preserving the existing native image-codec boundary. Navigation closes
    weakly registered decoders, releases decoded frame storage, rebuilds the
    constructor graph, and makes stale decoder and track methods inert without
    extending decoder or image-buffer lifetimes through the registry.
  - [x] Make MediaSource, SourceBuffer, SourceBufferList and handles Realm-owned
    while preserving one host media-pipeline boundary. Navigation weakly finds
    live objects, clears appended bytes and event callbacks, closes sources,
    empties buffer lists, breaks self-capturing method cycles, rebuilds the
    constructor graph, and makes stale mutation methods inert.
  - [x] Make MediaRecorder, ImageCapture, CaptureController and capture-target
    constructors Realm-owned while preserving one host codec/capture-adapter
    boundary. Navigation weakly finds live recorders and controllers, returns
    recorders to inactive, releases streams/tracks and event callbacks, breaks
    self-capturing methods, rebuilds classes, and makes stale capture operations
    inert through shared Realm teardown helpers.
  - [x] Make Animation, Effect, Timeline, Trigger and range-list objects
    Realm-owned while preserving one compositor integration boundary.
    Navigation cancels active registrations, releases target/source references
    and playback callbacks, breaks method/list/promise cycles, rebuilds classes,
    and makes stale animation operations inert. The Promise state index is weak,
    so settled values are retained only by live promises, resolvers or reactions.
  - [x] Make Custom Element registries, ElementInternals, CustomStateSet and
    CSSPseudoElement wrappers Realm-owned. Navigation releases definitions,
    pending `whenDefined()` resolvers, DOM/form/pseudo references and
    self-capturing methods, disconnects constructor/prototype graphs, and makes
    every retained operation from the old page inert.
  - [x] Make XSLTProcessor instances Realm-owned while retaining one explicit
    host-adapter boundary for real stylesheet execution. Navigation releases
    stylesheet and parameter values, breaks instance-method cycles, disconnects
    the class graph, and makes retained processors inert.
  - [x] Make WebGL 1/2 contexts and resource wrappers Realm-owned while
    preserving one future GLSL-to-wgpu compositor adapter boundary. Navigation
    releases canvas/state closures, marks resources deleted, clears shader
    source, disconnects every WebGL constructor/prototype graph, and makes stale
    context operations inert.
  - [x] Make XRSystem, XRRigidTransform and XRRay wrappers Realm-owned while
    preserving one native XR device/compositor adapter boundary. Navigation
    releases capability callbacks and geometry object graphs, disconnects the
    WebXR class hierarchy, and prevents retained page objects from issuing
    session or geometry operations.
  - [x] Make WebSocketStream and WebTransport lifecycle/stream wrappers
    Realm-owned while preserving one native streaming-network adapter boundary.
    Navigation releases stream and promise references, invalidates transport
    methods, disconnects the constructor/prototype graphs, and prevents retained
    page objects from initiating transport operations.
  - [x] Make WebGPU roots, adapters, devices, queues, buffers, encoders, shader
    modules and support objects Realm-owned while retaining the existing single
    `wgpu` host adapter. Navigation invalidates every GPU method, releases
    descriptor/object graphs, explicitly unmaps and destroys registered native
    buffers, drops pending command resources, and disconnects the complete
    WebGPU constructor/prototype graph.
  - [x] Make File System Access handles, writable streams, observers and
    directory iterators Realm-owned while retaining one runtime-local OPFS
    storage implementation. Navigation invalidates every filesystem operation,
    releases captured paths/write buffers/iterator entries, disconnects all
    five constructor/prototype graphs, and prevents stale page handles from
    mutating files after a Realm transition.
  - [x] Make Payment Request and Push capability objects Realm-owned while
    retaining one future platform-adapter boundary for each service. Navigation
    invalidates payment/show/abort and push subscription operations, releases
    event callbacks, disconnects all seven constructor/prototype graphs, and
    prevents retained page objects from initiating user- or platform-mediated
    actions.
  - [x] Make Text Track, cue, cue-list, track-list and playback-quality objects
    Realm-owned. Navigation clears indexed entries, breaks track/cue/list
    cycles, releases cue-rendering closures and event handlers, and disconnects
    all six constructor/prototype graphs.
  - [x] Make experimental and small compatibility surfaces Realm-owned without
    inventing privileged browser services. Navigation releases Shared Storage,
    worklet, viewport, WGSL, AI-capability, Origin, UA-data, remote-playback and
    picture-in-picture object graphs, clears process-local page state, and
    disconnects all cached constructors while retaining explicit host-adapter
    rejection boundaries.
  - [ ] Complete Realm-owned teardown for remaining mutable Web API
    constructor/prototype graphs and page-scoped host resources, then replace
    the current `Rc` lifetime accounting with the planned cycle-collecting
    shared heap/stable-handle model.

### Delivery gates and provisional effort
- [x] **Gate A — Dynamic JS proof:** fetch an external script over HTTP, parse
  with SWC, lower to validated W3IR, execute in W3VM without rustc, and mutate
  the real W3COS jsdom document. The route is capability-scoped behind
  `dynamic-js` and covered by the default CI workflow.
- [ ] **Gate B — Unified runtime MVP (6–10 cumulative person-months):** W3IR + W3VM + native backend share semantics; closures, exceptions, Promise, timers, ESM, DOM Host calls, and bytecode cache pass differential tests
- [ ] **Gate C — Real map SDK (10–16 cumulative person-months):** a dynamically loaded map SDK initializes, loads chunks/resources, renders, and handles pointer/touch/zoom interactions
- [ ] Re-estimate Gate C if the selected SDK path requires production WebGL, Blob Worker, or other missing browser subsystems (provisional cumulative range: 16–24 person-months)

## Phase 3.6 — Native Browser and Dynamic Web Loading

### Static parse / reader mode
- [ ] Replace the trusted-fragment parser with a standards-oriented HTML5 document parser and tree builder
  - [x] Add an initial incremental Browser document tokenizer/tree builder that
    accepts arbitrary chunk boundaries across tags, quoted attributes,
    comments, entities and raw/RCDATA elements; reuses the live DOM; applies
    basic head/body insertion, implied paragraph/list end tags, table row/body
    insertion and foster parenting; and exposes explicit EOF/resume progress
    to the browser task pump.
  - [x] Add the next tree-builder compatibility layer: active-formatting
    reconstruction and the common misnested adoption-agency path; inert
    `template.content` fragments; SVG/MathML namespace propagation, integration
    points and HTML breakouts; and doctype-driven standards/quirks selection.
  - [x] Scope table insertion and foster parenting to the active template,
    maintain nested template insertion-mode state, prevent end tags from
    escaping template boundaries, add implicit `colgroup` plus cell/caption
    formatting markers, and apply the HTML foreign-content SVG/MathML tag and
    attribute adjustments with foreign CDATA support.
  - [x] Implement the adoption-agency furthest-block path: bounded outer and
    inner loops, special-node discovery, active/open-list replacement,
    intermediate and formatting clones, child reparenting, bookmark updates,
    and foster/template-aware insertion of the repaired subtree.
  - [x] Route Browser fragment parsing through the same incremental tree
    builder used by document navigation. `innerHTML`, `insertAdjacentHTML`,
    `setHTML`, `setHTMLUnsafe`, DOMParser fragments and range-created markup now
    share formatting repair, templates, table modes and foreign-content rules;
    fragment scripts remain inert and the sanitized entry points filter active
    elements, event handlers and `javascript:` URL attributes in that path.
  - [x] Complete doctype compatibility selection with the current HTML
    standards/limited-quirks/quirks public- and system-identifier table,
    preserve the three-state result for parser diagnostics, and expose a
    recovery-error counter with checkpoints for malformed/late doctypes,
    missing doctypes, unmatched end tags, invalid HTML self-closing flags,
    ignored declarations and unterminated comments/foreign CDATA.
  - [x] Start the feature-neutral parser extraction with always-compiled
    doctype token interpretation/compatibility selection plus a shared inert
    fragment policy for active elements, unsafe attributes and HTML void
    elements. The tree builder now talks to script execution only through a
    four-method `ParserScriptHost`; Browser adapts the existing `ScriptLoader`
    and fragments use an inert host. Parser progress, insertion/template modes,
    namespaces, active formatting entries and the complete streaming parser
    state are now also always-compiled types. Ordinary AOT and Browser builds
    compile and test this foundation without pulling the dynamic compiler or
    W3VM into AOT. The single parser implementation's constructors,
    write/finish/resume lifecycle, tokenizer drive loop, raw/RCDATA handling,
    text insertion, doctype checkpoints, start/end-tag paths, table/template
    scope, SVG/MathML integration, active formatting/adoption repair, foster
    insertion and script checkpoints now all live in the always-compiled
    `html_tree_builder`. All inert fragment entry points use that same parser in
    Browser and ordinary AOT builds; the former non-`dynamic-js` fragment
    fallback has been removed. Feature-neutral regression tests now execute the
    same document parser in both configurations for compatibility modes,
    table/template scoping, adoption repair and SVG/MathML adjustment. Direct
    template `col`, `tr` and `td`/`th` tokens also create their required implicit
    `colgroup`, `tbody` and `tr` wrappers. SVG/MathML parsing now retains the
    required XLink, XML and XMLNS namespace/prefix/local-name identity in the
    shared DOM attribute store, and the same metadata backs `get`, `set`, `has`
    and `removeAttributeNS` plus `NamedNodeMap` lookup.
  - [ ] Complete the HTML5 insertion-mode surface (remaining table,
    adoption-agency scope/error edge cases, the remaining template-mode token
    rules, and complete tokenizer/tree-builder parse-error coverage).
- [x] Add `DocumentLoader`: navigation lifecycle, redirects, MIME/charset handling, cancellation, relative URL resolution, and error pages
  - Top-level GET runs on the background Fetch transport with a bounded body,
    manually followed redirects, final-URL/history propagation, sensitive
    header stripping and redirect-hop Cookie Store rematching under safe
    top-level SameSite rules.
  - HTML/XHTML and escaped plain-text documents feed the incremental live-DOM
    parser; transport charset, BOM and early `<meta charset>` select UTF-8,
    UTF-16 or Windows-1252 decoding. Unsupported MIME/charset and network/HTTP
    failures render a script-free error document.
  - Response headers and bounded body chunks cross the worker channel
    independently. After at most the 1024-byte charset sniff window, incremental
    UTF-8/UTF-16/Windows-1252 decoding feeds the live parser before network EOF;
    split code points and cancellation remain safe.
  - Navigation replacement cooperatively cancels both the document request and
    its page script graph, so a late stale response cannot mutate the new
    document. Relative scripts resolve from the final redirected document URL,
    then reuse the shared Cookie/Fetch/W3IR/W3VM path.
- [ ] Load `<style>`, `<link rel="stylesheet">`, images, fonts, and other supported subresources into the existing DOM/CSS/layout/render pipeline
  - [x] Load authored and dynamically inserted `<style>` plus HTTP(S)
    `<link rel="stylesheet">` through one Browser subresource path. Runtime
    source reuses the compiler's tolerant ESM CSS parser/normalizer and the
    existing DOM stylesheet registry; page-owner ids let navigation remove
    Browser rules without deleting native AOT rules.
  - [x] Preserve parser DOM source order, expose each installed sheet through
    the element's `sheet` property and the live `document.styleSheets` list,
    update computed style through the existing layout/render registry, dispatch
    `load`/`error`, and hold the document `load` lifecycle until external CSS
    settles. Dynamic insertion/mutation and removal use the existing DOM pump,
    cancellation and navigation teardown.
  - [x] Reuse the existing background Fetch transport, Cookie Store snapshot
    and response updates, referrer policy, CORS credentials modes, SRI
    validation, source-size bound, strict `text/css` MIME validation and
    final-redirect URL propagation. `crossorigin` sheets validate final CORS
    permission before installation.
  - [x] Participate in the shared persistent Browser HTTP response cache with
    request-origin/credentials/mode partitioning, exact Fetch-generated request
    headers for `Vary`, ETag/Last-Modified conditional requests, safe `304`
    merging, `no-store`, bounded/pruned storage and the existing cache counters.
    No second network stack, cache, CSS parser, DOM registry, layout engine or
    execution engine is introduced.
  - [x] Replace loaded `<style>` text and `link.href` without retaining stale
    rules, cancel superseded requests, honor live `disabled`/re-enable and
    removal, keep `sheet` plus `document.styleSheets` synchronized, and rebuild
    the same page-owned CSS registry in current DOM order so mutation time
    cannot change cascade order.
  - [x] Preserve enclosing and nested `@media` conditions through the shared
    tolerant compiler parser, combine them on flattened rules, and activate both
    rule-level conditions and element `media` attributes against the existing
    live viewport/`matchMedia` state. Viewport and DPR changes rebuild the same
    page-owned registry while inactive sheets remain visible through
    `document.styleSheets`.
  - [x] Resolve leading `@import` dependencies from external and inline
    stylesheets through the same background Fetch, Cookie, CORS, integrity,
    referrer and persistent-cache path. Recursive graphs preserve depth-first
    cascade order and inherited media conditions, hold document load completion,
    skip failed branches, and bound cycles, depth, import count and aggregate
    source bytes without introducing another CSS parser or loader.
  - [x] Parse ordered `@font-face` sources and descriptors in the shared
    compiler CSS pipeline, then load HTTP(S) TTF/OTF/WOFF/WOFF2 sources through the same
    Browser Fetch, Cookie, CORS, referrer and persistent-cache path. Unsupported
    formats and unavailable `local()` candidates fall through in authored
    order; MIME, source-size and font decoding are validated before registration.
    Page/stylesheet owner ids release registered bytes on mutation, removal,
    navigation and loader teardown, and font work holds document load completion.
    Compressed web fonts are decoded once at the shared `FontRegistry` boundary;
    the HTTP cache retains the original representation while layout and all
    renderers consume the same canonical sfnt bytes. The pure-Rust decoder is
    linked only by `dynamic-js`, so ordinary AOT artifacts do not inherit its
    Brotli/WOFF code.
  - [x] Resolve registered CSS family stacks, weights and styles through the
    existing `FontRegistry` in shared text measurement plus CPU, Vello/GPU and
    Skia rendering. Font identity participates in retained layout, shaped-text,
    glyph and display-chunk cache keys; owner teardown restores fallback metrics
    and releases parsed/typeface data instead of retaining a second font cache.
  - [x] Enforce CSS `unicode-range` bounds and actual cmap coverage while
    resolving each character through the authored family/style/weight stack.
    Sibling subset faces from one stylesheet remain registered together; shared
    layout, wrapping, ink bounds, input cursor advancement, CPU rasterization,
    Vello glyph runs and Skia typeface runs consume the same resolved spans.
    Retained measurement and Vello display-chunk identities hash only the subset
    faces used by the text, so adding an unrelated subset does not invalidate
    existing runs. Browser coverage loads disjoint WOFF2/TTF subsets over the
    shared HTTP path, verifies mixed-run metrics and releases both with their
    stylesheet owner.
  - [x] Defer `@font-face` work while its nested media conditions or owning
    `<style>`/`<link>` `media` attribute do not match. Viewport and DPR changes
    activate newly eligible faces through the existing stylesheet graph, Fetch,
    cache and `FontRegistry` owner path without replaying stylesheet load events
    or blocking the document's initial completion; removal and navigation also
    discard deferred faces.
  - [x] Make `document.fonts` a real `EventTarget` and drive truthful
    `loading`/`loaded` status, per-cycle `ready` promises and
    `loading`/`loadingdone`/`loadingerror` events from both programmatic
    `FontFace.load()` and the shared stylesheet graph. Concurrent loads keep
    native and JS readiness pending until the final cycle settles; cancellation
    closes the cycle as an error instead of leaving readiness stuck.
  - [x] Expose stylesheet-backed `@font-face` rules as stable, CSS-connected
    `FontFace` identities in `document.fonts` and loading-event `fontfaces`.
    Parsed descriptors and per-face status follow the same stylesheet
    fetch/registry graph; `delete()`/`clear()` preserve CSS-connected faces,
    while owner removal/navigation releases them and cancelled loads become
    failed event entries.
  - [x] Route programmatic HTTP(S) `FontFace` sources through the shared
    Browser Fetch/CORS/Cookie/cache transport. Relative URLs resolve against
    the active document, font requests omit credentials, enforce response MIME
    and decoded-size limits, reuse ETag revalidation, then register canonical
    sfnt bytes through the same `FontRegistry` used by CSS and native callers.
  - [x] Add text/glyph-demand font loading. Stylesheet `@font-face`
    declarations and stable `document.fonts` identities are registered without
    downloading their sources. The shared `FontRegistry` requests only faces
    matching the text's family, weight, style and `unicode-range` when layout
    first resolves font runs; `FontFace.load()` and
    `FontFaceSet.load(font, text)` enter the same demand gate. Media-inactive
    demand remains deferred, unused subsets stay `unloaded`, and activated
    faces retain the existing Browser Fetch/CORS/cache, lifecycle, owner
    cleanup and native font registry path.
  - [x] Load HTTP(S) raster `<img>` resources through the shared Browser
    Fetch/Cookie/CORS/referrer/cache transport, decode them with the existing
    `image` codecs at the Browser task checkpoint, and publish the result into the one
    `image_loader` cache already consumed by CPU, GPU and Skia renderers.
    Parser-authored images hold the document `load` lifecycle; dynamic and
    detached `Image()` sources use the same cancellable loader without becoming
    document blockers. `complete`, `currentSrc`, `naturalWidth`,
    `naturalHeight`, `decode()`, load/error EventTarget delivery, decoded-size
    layout invalidation, HTML width/height hints and auto-size aspect-ratio
    propagation are covered by end-to-end tests; source mutation and navigation
    cleanup reuse the loader's cancellation path. Paint is reserved while
    Browser fetch is pending or failed, preventing a second renderer-owned
    synchronous network request.
  - [x] Add responsive raster `srcset`/`sizes`/`<picture>` selection on the
    existing image pipeline. Density and width descriptors use the live
    viewport/DPR plus ordered `sizes` media conditions; `<source>` selection
    honors tree order, media and decoder-supported MIME types before falling
    back to the `<img>`. Attribute, source-tree, viewport and DPR changes queue
    reselection through the same cancellable Browser loader. The selected
    renderer source is internal Document state, so reflected `src` remains the
    author fallback while `currentSrc` exposes the fetched URL. Physical pixels
    stay in the shared decoder cache and density-corrected intrinsic dimensions
    drive `naturalWidth`/`naturalHeight` and layout. Unit and HTTP end-to-end
    tests cover candidate choice, property reflection, mutation scheduling,
    shared caching and final component rendering.
  - [x] Load raster CSS `background-image: url(...)` layers through the same
    Browser Fetch/Cookie/referrer/HTTP-cache transport and the same decoded
    `image_loader` cache used by `<img>` and all CPU, GPU and Skia renderers.
    Inline declarations resolve from the document; every external or imported
    stylesheet fragment absolutizes its own relative URLs before entering the
    shared CSS registry. Dynamic style changes are discovered by the regular
    document pump, pending/failed sources remain reserved against synchronous
    renderer fetch fallback, navigation cancels outstanding work, and unit plus
    HTTP end-to-end tests cover layered URL parsing, fragment-base resolution,
    decoding and cache publication.
  - [x] Keep `<img>` and CSS raster backgrounds on one Browser image-request
    implementation for Fetch headers, Cookie updates, CORS, HTTP revalidation,
    MIME/source limits, decoding and cache publication. Their coordinators only
    retain consumer-specific DOM lifecycle behavior. Both request classes now
    participate in pump deadlines and navigation/cancellation cleanup; removing
    the final CSS consumer cancels its pending request and permits a later
    re-added URL to retry.
  - [x] Add the shared raster-background placement model used by CPU, GPU and
    Skia: multiple URL layers, intrinsic/explicit/percentage size, `cover`,
    `contain`, common position forms, `repeat`/`no-repeat`/`repeat-x`/
    `repeat-y`/`round`/`space`, and border/padding/content origin plus
    rectangular clip boxes. DOM CSSOM and the compiler expand the same shared
    `background` shorthand parser into the same Style fields, avoiding
    renderer- or mode-specific parsing.
  - [x] Complete the shared advanced background geometry subset: rounded
    `border-radius` clipping for border/padding/content clips, edge-offset
    three/four-token positions, and linear/radial gradient layer
    size/position/repeat/origin/clip parity. Gradient parsing and geometry now
    live beside raster placement and feed CPU, GPU and Skia paint adapters
    without a renderer-specific CSS model.
  - [x] Complete `background-attachment`, blend modes, repeating-gradient and
    directional/shape gradient syntax, and the remaining shorthand/cascade
    edge cases. Attachment, per-layer blend selection, double-position stops,
    repeating expansion and direction/shape normalization now live in the one
    shared background model. CPU, Vello and Skia consume that model; shorthand
    resets attachment and blend longhands according to CSS cascade semantics.
  - [x] Add SVG image documents, lazy-loading policy, animation scheduling and
    remaining supported subresources. SVG bytes rasterize through resvg into
    the shared image cache, GIF frames advance through the normal window frame
    scheduler, and native `loading=lazy` uses a viewport prefetch band without
    delaying document load. Images, CSS images, stylesheets/imports and fonts
    continue to share the Browser Fetch/Cookie/cache cancellation pipeline.

### Current unattended implementation batch
- [x] Step 1 — Extend the one serialized Style model and one shared shorthand
  parser with raster background image/size/position/repeat/origin/clip fields.
- [x] Step 2 — Route DOM CSSOM and ahead-of-time compiler declarations through
  that shared model and parser.
- [x] Step 3 — Compute raster layer geometry once and consume it from CPU, GPU
  and Skia renderers.
- [x] Step 4 — Consolidate Browser `<img>` and CSS background network
  validation/cache/decode behavior while retaining separate consumer lifecycle
  notifications.
- [x] Step 5 — Handle active-source mutation, cancellation, navigation teardown
  and retry eligibility without renderer-owned network fallback.
- [x] Step 6 — At the batch boundary, run one unified acceptance pass: format
  and diff checks; DOM, compiler and default/dynamic runtime suites; targeted
  HTTP Browser coverage; AOT/dynamic dependency boundaries; release-size audit.
  The pass completed with DOM 131/131, compiler 438/438 plus all available
  integration suites, default runtime 727/727, dynamic runtime 901/901 and the
  map-style loader fixture 1/1. The two dependency boundaries and size policy
  also passed; only pre-existing environment-specific tests remain ignored.

### Current unattended advanced-background batch
- [x] Step 1 — Promote background clip from a rectangle to shared rounded
  border/padding/content geometry and consume it from CPU, GPU and Skia.
- [x] Step 2 — Resolve CSS three/four-token named-edge positions centrally,
  including inward pixel and percentage offsets.
- [x] Step 3 — Move linear/radial gradient parsing, normalized stops and tile
  geometry into the same background layer model used by raster images.
- [x] Step 4 — Paint the shared gradient description through software CPU
  sampling, Vello gradient brushes and Skia shaders while preserving CSS layer
  order and shared size/position/repeat/origin/clip behavior.
- [x] Step 5 — At the batch boundary, run the unified default/dynamic runtime,
  DOM/compiler, map compatibility, dependency-boundary and release-size
  acceptance pass. The final pass completed with DOM 131/131, compiler 438/438
  plus all available integration suites, default runtime 732/732 and dynamic
  runtime 906/906 (one environment-specific unit remains ignored in each),
  and the map-style fixture 1/1. Both dependency boundaries passed. Stripped
  probes remain 6,083,264 bytes for AOT and 7,148,896 bytes for Browser:
  a 1,065,632-byte (17.52%) increment, within the fixed and regression budgets.

- [x] Implement anchors, URL bar, reload, back/forward history, downloads, and
  a visible script-disabled reader mode. `BrowserController` is UI-neutral and
  reuses DocumentLoader/session history; same-document fragments scroll without
  reloading, downloads return sanitized names plus bytes to the shell, and
  reader mode keeps CSS/images networking while policy-disabling all scripts.
- [x] Run parsing, image decoding, and page DOM work outside the privileged
  shell capability domain. `BrowserPageDomain` constructs the loader, parser,
  decoders, Realm and thread-local DOM inside a dedicated page worker; the
  shell receives only typed navigation/download events and immutable Component
  snapshots, with no live DOM, VM or arbitrary filesystem handle crossing the
  boundary.

### Current unattended Browser-completion batch
- [x] Step 1 — Extend shared CSS/style/compiler/DOM inputs for attachment and
  blend longhands plus shorthand reset semantics.
- [x] Step 2 — Normalize repeating/directional/shape gradients and blend each
  background layer through the common CPU/Vello/Skia paint description.
- [x] Step 3 — Decode SVG image documents and animated GIF frames in the shared
  cache, schedule animated repaint, and add non-load-blocking lazy images.
- [x] Step 4 — Add the shared Browser controller for address/navigation,
  anchors, downloads and explicit script-disabled reader mode.
- [x] Step 5 — Add the typed per-page capability domain so privileged shells do
  not own page parser, decoder, Realm or live DOM state.
- [x] Step 6 — Run the consolidated test, dependency-boundary and stripped-size
  acceptance pass and record its exact evidence here. Formatting and diff
  checks passed; DOM completed 132/132 and compiler completed 438/438. The
  default runtime completed 735/735 plus API 28/28, CodeMirror 11/11 and the
  feature matrix 21/21; the dynamic Browser runtime completed 913/913 plus the
  same integration suites and the map-style loader fixture 1/1. One
  environment-specific native-wgpu test remains explicitly ignored in each
  runtime configuration. Both AOT and dynamic-JavaScript dependency boundaries
  passed. The stripped probes are 6,083,280 bytes for ordinary AOT and
  8,162,944 bytes for Browser, a 2,079,664-byte (34.19%) increment; both the
  fixed product budget and the 50% regression gate passed.

### Dynamic script and module loading
- [x] Support parser-inserted `<script src>`, inline scripts, `async`, `defer`, and ordered execution
  - [x] Fetch classic external scripts on background workers without blocking
    the browser task pump. Parser-discovered scripts fetch concurrently but
    enter W3IR/W3VM in document order; explicit parser `async` and dynamically
    inserted scripts execute in fetch-completion order, while
    `script.async = false` joins the ordered queue.
  - [x] Add streaming-parser lifecycle checkpoints (`begin_document_parse`,
    incremental script scans and `finish_document_parse`) without introducing
    another execution engine. `readyState` now advances through loading,
    interactive and complete; parser `defer` scripts use a distinct ordered
    EOF queue, non-async modules start at EOF, `DOMContentLoaded` waits for
    defer/module evaluation including top-level await, and `load` additionally
    waits for async scripts. Failures and removed elements release their
    lifecycle blockers.
  - [x] Pause token-by-token tree building immediately after a parser-inserted,
    non-async/non-defer external classic script and resume at the exact buffered
    token after its shared transport and W3IR/W3VM execution settle. Parser DOM
    insertions are distinguished from dynamically inserted scripts without a
    second loader or evaluator.
- [x] Support dynamically created `<script>` elements, `load`/`error` events,
  JSONP callbacks, and script removal/cancellation semantics
  - The standard `HTMLScriptElement` loading properties (`src`, `type`,
    `async`, `defer`, `noModule`, `crossOrigin`, `integrity`,
    `referrerPolicy`, and `text`) reflect through the shared DOM attribute/text
    store. Connecting an empty script no longer marks it started: a later
    `src` or inline-text mutation reschedules the existing document script pump
    and enters the same SWC → W3IR → W3VM path exactly once.
  - [x] Apply the module-capable script preparation rules to `nomodule` and
    JavaScript MIME types. Classic `nomodule` elements are claimed without
    fetching or evaluating, removing the attribute later cannot restart them,
    module scripts ignore `nomodule`, and script-element MIME essence matching
    handles ASCII case, surrounding whitespace, and legacy JavaScript aliases.
    Parameterized element `type` values remain inert data blocks, while fetched
    response `Content-Type` parsing accepts parameters before the same alias
    table and W3VM path.
- [ ] Support ESM module graphs, dynamic `import()`, import maps, module namespaces, circular dependencies, and top-level await
  - [x] Add the initial static ESM graph path for inline/fetched
    `<script type="module">`, exact and prefix import-map entries, resolved URL
    caching, live imports/re-exports and module namespaces, and cycle-safe
    instantiation/evaluation.
  - [x] Lower dynamic `import()` to W3IR and resolve it through the same module
    registry; the existing Core Promise delivers the live namespace on the
    shared microtask queue. `import.meta.url` is populated from the canonical
    module record URL.
  - [x] Lower top-level await through the same W3IR suspension metadata and
    make dependency/module evaluation Promise-based. Importers and dynamic
    `import()` wait for settlement, rejected awaits fail the graph, and
    strongly connected graphs do not await their own evaluation promise.
  - [x] Implement `export *` as W3IR module metadata resolved by the same live
    binding registry. Default exports are excluded, direct exports override
    star exports, ambiguous names fail named imports and are omitted from
    module namespaces.
  - [x] Resolve global and scoped import maps with canonical URL-like keys,
    longest matching scope, parent-scope fallback, and deterministic longest
    specifier-prefix selection.
  - [x] Preserve `null` import-map targets as blocking mappings so they stop
    scope/global fallback, and merge multiple pre-instantiation DOM import maps
    without overriding entries registered by an earlier map.
  - [x] Expose Promise-returning source/URL module entry points and make DOM
    module-script `load`/`error` follow final graph evaluation, including
    pending and rejected top-level await. Synchronous embedding calls are now
    adapters over that same evaluation Promise.
  - [x] Fetch and buffer complete external module graphs on background workers,
    deduplicate source requests by canonical URL, advance graph acquisition
    from the browser task pump, and instantiate/evaluate only through the
    existing W3IR/W3VM module registry. Releasing the final loader handle
    cooperatively rejects pending graph Promises and discards worker results.
  - [x] Merge import maps installed after module resolution begins while
    preserving every successful `(referrer, specifier) → URL` decision already
    observed by the loader. New bare names and scopes become available to later
    graphs, but an incoming exact/prefix/blocking rule is skipped when it would
    change a prior resolution; earlier map entries still win deterministically.
  - [x] Bind module graph fetches to the attached document origin, or the first
    standalone module origin, and require a matching or wildcard
    `Access-Control-Allow-Origin` response for cross-origin ESM sources.
  - [x] Emit the initiating page's `Origin` header for every ESM CORS request
    and for classic scripts that opt into `crossorigin`, including redirect
    hops; classic no-CORS GETs omit it. Cross-origin redirect responses must
    pass the same credential-aware CORS check as final responses before their
    `Location` is followed.
  - [x] Propagate the Fetch transport's final redirect URL and redirected flag,
    run module CORS checks against that final origin, and alias requested/final
    URLs in the shared source and module registries. Relative imports,
    `import.meta.url`, and module identity therefore use the final URL.
  - [x] Partition the shared Cookie Store by page URL scope and implement the
    module default `same-origin` credentials baseline: same-origin module
    requests send matching Cookie headers and accept `Set-Cookie`, including
    request-only `HttpOnly`; cross-origin module requests receive no page
    cookies.
  - [x] Add graph-inherited ESM `omit` / `same-origin` / `include` credential
    modes on the same module loader. DOM module scripts map
    `crossorigin="use-credentials"` to `include`; static dependencies and W3VM
    dynamic imports inherit the first module-map fetch mode. `omit` neither
    sends nor stores cookies, while `include` rematches and updates the shared
    Cookie Store across origins.
  - [x] Enforce credentialed module CORS: cross-origin `include` responses
    require an exact `Access-Control-Allow-Origin` plus
    `Access-Control-Allow-Credentials: true`, so wildcard origins cannot
    authorize credentialed modules. Persistent HTTP validator/body entries are
    partitioned by credentials mode and retain both CORS response headers.
  - [x] Match session cookies by host-only/validated Domain, Path and Secure
    rules, derive default paths from response URLs, order request cookies by
    path specificity, and implement `Max-Age` expiry/deletion without adding a
    second loader-side cookie implementation.
  - [x] Parse RFC 6265 cookie dates across IMF-fixdate, obsolete RFC 850, and
    ANSI C `asctime` forms; apply `Max-Age` precedence over `Expires`, delete
    already-expired cookies, expose absolute expiry and parsed Strict/Lax/None
    values through Cookie Store, and reject `SameSite=None` without `Secure`.
    Page writes, response `Set-Cookie`, and redirect-chain snapshots all use the
    same parser.
  - [x] Add an embedder-owned persistent Cookie jar without forking the browser
    loader backend. Future-expiry cookies are stored in a versioned, atomically
    replaced JSON file; session cookies remain memory-only, expired/invalid
    records are pruned on load, deletion is durable, and corrupt files fail
    closed without replacing the live jar. Navigation now resets only the
    document URL context and rematches the same jar for the next Realm. Android
    binds this jar to its internal application data directory; other embedders
    opt in by supplying their profile directory.
  - [x] Enforce Cookie SameSite request context in the shared module transport.
    Schemeful sites use Mozilla PSL registrable domains (with IP/localhost
    handling); Strict and Lax cookies stay on same-site subresources, cross-site
    safe top-level navigation admits Lax, and `None` still requires `Secure`.
    The initiating document site remains fixed across redirects and W3VM
    dynamic imports. Domain attributes targeting public suffixes such as
    `co.uk` are rejected, and invalid non-host-only persisted records are
    pruned on load.
  - [x] Route DOM classic scripts through the same manual-redirect Cookie
    transport as ESM. Classic scripts without `crossorigin` use credentialed
    no-CORS semantics; `anonymous` uses same-origin credentials plus CORS, and
    `use-credentials` requires exact credentialed CORS. Redirect and final
    `Set-Cookie` values—including repeated headers on one response—update the
    shared jar before a retry, while in-memory deduplication and persistent HTTP
    cache entries are partitioned by classic fetch mode.
  - [x] Enforce `Cross-Origin-Resource-Policy` for classic no-CORS responses.
    `same-origin`, PSL-based `same-site` (including the secure-response rule),
    and `cross-origin` are evaluated against the initiating page; CORS-mode
    classic scripts continue to use CORS permission instead of opaque-response
    CORP checks.
  - [x] Enforce initial-fetch Subresource Integrity for classic and module
    script elements with SHA-256/384/512, strongest-algorithm selection,
    multiple candidate digests, base64/base64url forms, and forward-compatible
    unsupported-token handling. Classic cache/dedup keys include integrity
    metadata; module consumers validate independently while sharing fetched
    source bytes and the same W3IR/W3VM graph path. Cross-origin classic SRI
    requires a CORS-enabled response.
  - [x] Propagate script-element `referrerpolicy` through classic requests,
    static module dependencies, dynamic imports, retries and redirects. The
    shared transport implements all eight standard policy values, strips URL
    credentials/fragments, defaults to `strict-origin-when-cross-origin`, lets
    redirect responses tighten the next hop, and partitions in-flight
    deduplication where request referrers differ.
  - [x] Follow module redirects in the background module transport with a
    per-chain snapshot of the same Cookie Store. Each same-page-origin hop
    regenerates its Cookie header for the target URL, cross-origin hops strip
    Cookie/Authorization, and same-origin redirect `Set-Cookie` uses the same
    parser to affect the immediately following hop before returning to the
    authoritative Store.
  - [x] Enforce strict JavaScript MIME validation for fetched ESM sources,
    including case-insensitive parameterized MIME values, before parsing or
    inserting source into the shared module cache.
  - [x] Enforce Fetch `X-Content-Type-Options: nosniff` for classic external
    scripts on the existing response-validation path. A missing or non-
    JavaScript `Content-Type` dispatches `error` without compiling, executing,
    or caching the body; a new element can retry the same URL, while classic
    responses without `nosniff` preserve the web-compatible MIME-sniffing path.
    Synchronous embedding calls use the same check, 304 revalidation retains
    the security header, and the HTTP source-cache schema invalidates
    pre-enforcement artifacts.
  - [x] Move classic external script fetches onto the same browser task pump,
    deduplicate concurrent requests by URL, dispatch per-element `load`/`error`,
    and leave failed responses uncached so a newly inserted element can retry.
  - [x] Bind document reset/navigation to the active loader lifecycle: discard
    pending classic-script responses without firing stale element callbacks,
    reject pending module-graph Promises through the shared microtask queue, and
    clear page-scoped source/module/import-map state before a new Realm attaches.
  - [x] Add one configurable bounded retry state machine shared by classic and
    ESM fetches. Default behavior remains one attempt; opt-in retries cover
    transport failures and 408/425/429/500/502/503/504, apply capped
    exponential backoff plus delta-seconds/IMF-fixdate `Retry-After`, preserve
    URL deduplication, refresh module cookies per attempt, expose outcome
    counters, publish retry deadlines to the event loop, and cancel scheduled
    retries on navigation without executing stale code.
  - [x] Cancel claimed script elements when direct removal, subtree removal,
    replacement, text-content clearing, or navigation disconnects them.
    Classic subscribers are pruned independently from URL-deduplicated fetches,
    cancelled ordered entries leave explicit tombstones so later scripts cannot
    deadlock, and guarded ESM graph reactions suppress evaluation and callbacks.
    Reinserted claimed elements remain one-shot.
  - [x] Discover scripts when a detached subtree or `DocumentFragment` becomes
    connected. Detached scripts are never claimed or executed by the document
    scan; attaching their containing subtree reschedules the same loader pump
    and preserves dynamically inserted script ordering semantics.
  - [x] Cooperatively interrupt the existing classic/ESM fetch workers without
    introducing a second network stack. One shared atomic cancellation token is
    checked before requests, after response headers, around every 16 KiB body
    read, and at every manually followed ESM redirect hop. Navigation, loader
    release, the last classic subscriber, and an orphaned module graph signal
    the same task; shared URL/graph consumers retain their transport. Blocking
    system calls observe cancellation when I/O returns or the configured timeout
    expires, so a platform-specific hard socket abort remains an optional
    latency optimization rather than a correctness dependency.
  - [x] Model lexical initialization and temporal-dead-zone state in the shared
    W3IR/W3VM binding cells. Declaration-time `InitializeBinding` is distinct
    from assignment, reads and writes before `let`/`const` initialization raise
    `ReferenceError`, module namespace getters preserve the same guard, and
    cyclic ESM graphs reject rather than exposing a synthetic `undefined`.
    The W3IR format version invalidates older persistent bytecode artifacts.
  - [x] Cache failed ESM evaluation outcomes on the shared module record.
    Repeated static loads and later dynamic imports now reuse the original
    rejected evaluation promise without executing module bodies again. Cyclic
    graphs retain depth-first dependency order, and an async cyclic dependency
    does not prevent a later sibling dependency from starting evaluation.
  - [x] Track ESM strongly connected component cycle roots through weak module
    record links. Future static loads and dynamic imports of any cycle member
    adopt the root settlement and original rejection even when that member's
    body finished first; the cycle-root links themselves do not add strong
    ownership cycles. Evaluation settlement is projected separately onto the
    specifically requested module namespace, so successful member loads do not
    expose the root namespace.
  - [x] Preserve `InnerModuleEvaluation` readiness order across shared async
    dependencies and cycles. Modules whose dependencies completed
    synchronously now enter W3VM immediately instead of taking an extra
    `Promise.all` microtask hop; an evaluating cycle member retains its own
    in-DFS Promise while later external evaluation still adopts the cycle-root
    settlement. The ECMAScript asynchronous cyclic graph shape
    `A → {B,C}`, `B → D`, `C → {D,E}`, `D → A` is covered with independently
    controlled top-level-await settlement.
  - [x] Preserve the synchronous-abrupt versus asynchronous-rejection boundary
    during module evaluation. A synchronously rejected dependency now stops DFS
    before later sibling modules execute and W3VM-thrown JavaScript values
    reach the graph rejection unchanged. In the asynchronous cyclic graph
    above, a rejecting `C` still rejects root `A` immediately while sibling `B`
    remains pending; later settlement of `B` cannot execute `A` or replace the
    cycle root's original error, and evaluating any cycle member reuses it.
  - [x] Integrate streaming-parser EOF with the ordered deferred-script queue.
    Parser-inserted non-async module graphs now begin fetching while parsing,
    but evaluation waits for `interactive` and only the ready head of the
    document-order queue may start; a later inline or cached module cannot
    overtake an earlier network graph. Graph settlement and element removal
    wake the same queue, while module evaluation and top-level await remain
    `DOMContentLoaded`/`load` blockers through the existing lifecycle sets.
  - [x] Harden CORS singleton-header and redirect validation on the shared
    classic/ESM transport. Repeated HTTP response fields are combined before
    permission checks, so duplicate `Access-Control-Allow-Origin` or
    `Access-Control-Allow-Credentials` cannot be accepted by last-value
    overwrite. Document, classic-script and ESM URLs and redirect targets are
    restricted to credential-free HTTP(S); ambiguous duplicate `Location`
    fields are rejected before a target is contacted. Once a redirect crosses
    an origin boundary, an author `Authorization` header stays stripped even if
    a later hop returns to the original origin.
  - [x] Bound the shared session/persistent Cookie Store without introducing a
    loader-private jar. Name/value pairs over 4096 bytes are rejected; the
    oldest entries are evicted above 180 cookies per registrable site or 3000
    globally. The same limits apply while loading persisted profiles, mutating
    the authoritative jar, and following redirect-chain snapshots.
  - [x] Add an encrypted multi-profile persistence contract to that same jar.
    Profile identifiers map to isolated path-safe directories and are bound to
    a versioned protected envelope; the embedder supplies a
    `CookiePersistenceProtector` backed by its platform credential store.
    Plaintext downgrade, profile-envelope substitution, oversized files and
    corrupt/unauthenticated ciphertext fail before replacing live Cookie state.
  - [x] Provide an Apple Keychain protector for macOS/iOS. Each profile receives
    a random AES-256-GCM key stored as a generic-password item; cookie files use
    random nonces and authenticate the profile identifier as associated data.
  - [ ] Wire platform protectors to Android Keystore, Windows DPAPI and Linux
    Secret Service, and complete remaining redirect/CORS edge cases.
- [x] Route the initial runtime `ScriptLoader` through SWC → W3IR → W3VM and
  never invoke `rustc` during loading. Unsupported syntax fails explicitly
  while the lowering surface is expanded.
  - [x] Enforce that architecture in CI: ordinary AOT excludes compiler/W3IR/
    W3VM/SWC; the dynamic browser feature requires compiler, W3IR and W3VM;
    only `dynamic_script.rs` may consume them; classic and module sources retain
    one lowering entry each and exactly the shared W3VM construction sites.
- [x] Share Fetch/cache/module state with the page Realm while enforcing origin and credential rules
  - [x] Expose the same runtime `fetch_value` implementation to W3VM through
    the live page `window`; relative Request URLs resolve against the loader's
    active document URL instead of requiring a separate VM-side network API.
  - [x] Route page Fetch and script loading through the same manual redirect
    transport, URL-matched Cookie snapshot, credential modes and CORS
    validator. Redirect cookies are rematched on the next hop, accepted
    response cookies update the shared Cookie Store, forbidden cookie headers
    stay hidden, cross-origin response headers are CORS-filtered, and
    POST-to-GET redirects discard body-specific headers.
  - [x] Enforce page Fetch `cors`, `same-origin` and `no-cors` modes plus
    `follow`, `error` and `manual` redirect modes on that shared transport.
    Non-simple cross-origin requests perform and validate OPTIONS preflight;
    failed preflight never sends the actual request, while no-CORS responses
    use opaque filtering.
  - [x] Add a request-origin, target-origin and credentials-partitioned CORS
    preflight cache. `Access-Control-Max-Age` is capped at two hours, zero-age
    responses are not retained, expired entries are removed on lookup, and an
    LRU ceiling of 128 entries prevents unbounded page-controlled growth.
  - [x] Route page Fetch HTTP cache policy through the loader's persistent
    cache state and complete in-flight Abort cancellation semantics without
    adding a second transport.
    - [x] Replace the UTF-8/script-mode-specific sidecar with one generic binary
      browser response cache carrying status, validators, sanitized headers,
      `Vary` request metadata and caller-defined partition keys. Script loading
      consumes it through an adapter, and the shared budget prunes response and
      compiled W3IR artifacts without creating a second cache implementation.
    - [x] Connect page Fetch to that cache for safe GET/follow responses using
      request-origin, target-origin, credentials and request-mode partitions.
      Default/no-cache requests conditionally revalidate validators;
      no-store/reload/force-cache/only-if-cached select their corresponding
      read/write behavior. `Vary` compares the effective request headers while
      persisting only SHA-256 value digests, cache failures fall back to the
      network response, redirects and cross-origin opaque responses bypass
      storage, and CORS is rechecked before either cached or refreshed bytes
      become visible. The Response bridge now preserves binary bodies instead
      of forcing network bytes through UTF-8.
    - [x] Complete in-flight Abort cancellation through the shared sender.
      Page Fetch runs the existing redirect/Cookie/CORS sender on a cancellable
      worker while its synchronous Realm facade pumps timers and microtasks.
      `AbortController.abort()` and `AbortSignal.timeout()` therefore return an
      AbortError/TimeoutError while waiting for response headers or body bytes;
      the shared cancellation token stops later redirect and body work. A
      platform I/O call may still finish at its configured transport timeout,
      but it no longer blocks the page Realm from observing cancellation.
      The completion path runs the same timer/microtask checkpoint before
      classifying a transport result, so an AbortSignal and transport timeout
      becoming ready in one turn deterministically report the signal reason.
      The CORS preflight cache is process-shared and remains partitioned by
      request origin, target origin and credentials across those workers.

### Browser compatibility and security
- [ ] Implement origin model, CORS, CSP baseline, Cookie jar, storage partitioning, secure-context checks, and URL scheme policy
- [x] Isolate tabs/pages into sandboxed processes or equivalent capability
  domains; `BrowserPageDomain` creates a dedicated worker-owned Realm/DOM and
  exposes only typed commands and immutable render snapshots, so the shared
  W3IR/W3VM implementation does not imply a shared global VM instance.
- [ ] Add process/Realm crash containment, memory limits, navigation cancellation, and watchdog termination
- [ ] Add compatibility suites for DOM/HTML/CSS/ECMAScript plus real-site smoke tests
- [ ] Keep W3COS applications native-AOT by default; document the browser as the isolated dynamic-content exception

### Real map SDK acceptance matrix
- [x] Dynamic loader: script injection, JSONP, chunk loading, module caching, and retry behavior
  - CI now runs a standalone map-style compatibility fixture that serves an
    external bootstrap, a dotted-name JSONP callback and a secondary chunk.
    The bootstrap is evaluated only through SWC → W3IR → W3VM, performs its own
    DOM script injection, receives load/error lifecycle events and exposes an
    initialized SDK factory on the real page window.
  - Runtime W3IR lowering now represents `&&`, `||`, `??`, conditional
    expressions and `!`/unary plus/minus/void with existing backend-neutral
    branches, moves and arithmetic. It also covers classic `for`/`do...while`,
    `++`/`--`, arithmetic/exponent compound assignments, short-circuit
    `&&=`/`||=`/`??=` with single-evaluation member targets,
    parentheses/comma expressions and shared-Core `typeof`, plus `switch`
    fall-through and signed/unsigned bitwise/shift operations. Direct function
    declarations hoist through the same path, while object/array destructuring,
    defaults, inner rest patterns, reassignment targets and final rest
    parameters use the W3IR/W3VM function ABI and shared Core intrinsics.
    Ordinary template interpolation also uses shared W3IR addition/coercion;
    derived-class `super` writes/updates use the same Core receiver-aware
    accessor bridges as native AOT, including computed targets and logical
    short-circuiting. `debugger;` is a backend-neutral no-op when no debugging
    transport is attached, so development-flavored third-party chunks do not
    require a browser-only evaluator.
    `for (let ...)` loader closures receive W3VM-managed per-iteration cells.
    This expands common minified loader syntax without adding an evaluator or
    browser-only semantic path.
  - Initial `ScriptLoader::execute_pending_document_scripts` scans newly inserted classic scripts in document order, resolves relative URLs, shares the SWC → W3IR → W3VM path, invokes load/error callbacks, and reuses fetched source
  - An attached loader automatically schedules direct `<script>` insertions on the shared microtask queue, claims each element before evaluation to make nested script injection re-entrant safe, and requires no second execution engine
  - External JSONP can invoke a callback registered on the real window with nested object/array payloads; aggregate creation uses the same core intrinsics in AOT and W3VM
  - W3VM closures can escape into the shared host timer and microtask queues and later re-enter with live lexical captures
  - Static ESM graphs now share resolved URL/source/module caches, live lexical
    cells, cycle-safe instantiation, namespace getters, and exact/prefix import
    maps; dynamically inserted `type="module"` scripts use this path.
  - Module evaluation now adopts W3VM top-level-await Promises, delays
    importers, propagates rejection, and lets dynamic import expose a namespace
    only after asynchronous evaluation succeeds. The synchronous embedding API
    explicitly reports a still-pending host await instead of marking the module
    evaluated.
  - DOM module scripts subscribe to that evaluation Promise: `load` waits for
    the whole graph and top-level await, while parse/link/evaluation rejection
    dispatches `error`.
  - Barrel modules can forward non-default live bindings with `export *`;
    direct-export precedence and ambiguous-star behavior are resolved inside
    the shared module registry.
  - Import-map resolution now supports scoped maps, parent-scope fallback,
    URL-like remapping, deterministic longest-prefix matching, blocking `null`
    targets, and first-registration-wins merging for multiple maps installed
    before module instantiation.
  - External ESM graphs now fetch without blocking the DOM task pump, share
    canonical-URL in-flight requests, handle cyclic discovery, and delay
    script `load` until graph evaluation. Loader release provides cooperative
    cancellation without maintaining a second execution path.
  - Import maps merge both before and after graph resolution starts. The loader
    records successful referrer/specifier resolutions and admits each late
    mapping only when replaying those decisions produces the same URLs, while
    still making previously unresolved names available to later graphs.
  - External ESM responses now enforce a page-origin CORS baseline: same-origin
    loads pass directly and cross-origin sources require a matching or wildcard
    allow-origin header.
  - CORS-mode script requests now send the initiating page `Origin` across the
    redirect chain. The shared transport rejects an unauthorized cross-origin
    redirect before opening its target, while classic no-CORS scripts retain
    browser-shaped GET behavior without an `Origin` header.
  - Redirected Fetch responses expose their final URL. Module graphs check CORS
    against the final origin and use that URL for caching, relative dependency
    resolution, and `import.meta.url`.
  - The page Cookie Store is URL-matched. External ESM uses the default
    same-origin credentials behavior, including Domain/Path/Secure/HttpOnly/
    Max-Age-aware Cookie/Set-Cookie flow without leaking page cookies to
    cross-origin module requests.
  - Module graphs also support explicit omit/include credentials.
    `crossorigin="use-credentials"` selects include for DOM modules, and the
    graph mode is inherited by static dependencies and W3VM dynamic imports.
    Credentialed cross-origin responses require exact allow-origin and
    allow-credentials headers; wildcard authorization is rejected. Module-map
    consumers share the first fetch mode, while persistent HTTP cache artifacts
    are partitioned by mode.
  - Cookie expiry now accepts RFC 6265's current and legacy HTTP-date shapes,
    observes `Max-Age` precedence, and is shared by page state and redirect
    snapshots. Cookie Store exposes expiry and SameSite values, while insecure
    `SameSite=None` assignments are rejected.
  - Module subresource cookies now use the initiating page's fixed schemeful
    site across static dependencies, redirects, retries and W3VM dynamic
    imports. Mozilla PSL eTLD+1 matching distinguishes same-site cross-origin
    requests from true cross-site requests; Strict/Lax cookies are suppressed
    in the latter, and Domain attributes cannot target a public suffix.
  - DOM classic scripts now share the ESM Cookie/redirect transport rather than
    maintaining a second client path. Default, anonymous and use-credentials
    modes apply their distinct credential/CORS rules, consume response cookies,
    preserve repeated final/redirect `Set-Cookie` headers across retries, and
    cannot share source-cache entries across credential modes.
  - Classic no-CORS responses now honor `Cross-Origin-Resource-Policy`,
    including registrable-domain `same-site` matching and secure transport
    constraints; classic CORS requests remain governed by CORS.
  - External classic and module elements now enforce Subresource Integrity
    before execution. Mismatched consumers dispatch `error`; a later consumer
    with a valid digest can reuse already fetched source without bypassing its
    own integrity check.
  - Classic and module graph requests now compute `Referer` from the initiating
    document or importing module under the element's inherited referrer policy;
    redirect-provided policy is applied before following the next location.
  - Module redirect hops now rematch cookies from an immutable page-store
    snapshot instead of trusting ureq's header forwarding. The per-chain copy
    applies same-origin redirect `Set-Cookie` before the next request through
    the same parser used by the main Store. A target-path cookie
    can appear only after the redirect reaches that path, while a cross-origin
    target receives no page Cookie or Authorization header.
  - Network ESM accepts recognized JavaScript MIME types and rejects missing,
    plain-text, HTML, or other response types before W3IR lowering.
  - Classic external scripts fetch off-thread through the browser task pump.
    Parser-discovered scripts preserve document order despite out-of-order
    responses, dynamic scripts default to completion-order execution, concurrent
    identical URLs share one response, and failures dispatch `error` without
    poisoning a later element retry.
  - Document reset/navigation now cancels the page loader cooperatively. Late
    classic responses cannot execute against the replacement document, pending
    module graph Promises reject, and the old page's loader registries are
    cleared.
  - Classic and ESM transport/status retries now share an opt-in bounded policy,
    deduplicated request chain, Retry-After/backoff scheduling, event-loop
    deadlines, navigation cancellation, and telemetry. Every successful retry
    still lowers and executes only through W3IR/W3VM.
  - Removing a claimed classic or module script, including through an ancestor
    subtree, now cancels only that element subscription. Shared classic URL
    fetches retain live subscribers, ordered-script cancellation cannot block
    later execution, and a removed module element cannot enter W3VM or dispatch
    stale lifecycle callbacks.
  - Scripts assembled inside a detached subtree or `DocumentFragment` stay
    inert until that subtree is connected. The insertion hook then discovers
    descendant scripts and schedules them through the same document loader.
  - Classic and ESM fetches now use cancellation handles on the existing
    background transport. Body buffering and manually followed ESM redirects
    stop cooperatively, orphaned graphs release unreferenced fetches, shared
    consumers remain live, and dynamic classic scripts retain true network
    completion order even when several worker results are collected in one
    event-loop turn. A blocking platform I/O call still exits at its normal
    completion or timeout boundary.
  - Persistent response cookies now survive process reload through the same
    URL-matched Cookie Store used by document, ESM and redirect requests.
    Session cookies do not reach disk, navigation preserves the jar, and
    deletion remains deleted after reload.
  - W3VM page code now resolves `fetch` from the same live `window` surface as
    AOT code and can issue document-relative requests through the existing
    runtime Fetch implementation. Page Fetch and script loads now also share
    redirect, Cookie credential and CORS response-filtering primitives; no
    VM-specific network client or duplicate CORS policy is introduced. Page
    Request modes, redirect modes and CORS preflight layer policy onto the same
    sender instead of forking another HTTP implementation. Page Fetch also
    consumes the same generic binary response/validator cache as script loads,
    partitioned by origin, credentials and mode while preserving `Vary`.
    In-flight page requests now use that sender on a cancellable worker while
    the Realm pumps timers/microtasks, so AbortController and
    AbortSignal.timeout interrupt both header and body waits without a second
    network stack.
  - Full HTML5 insertion modes, descendant-module integrity metadata,
    platform credential-store hookups and remaining CORS edge cases remain.
- [ ] Web platform: Promise/microtasks, Fetch/CORS, URL APIs, observers, timers, storage, and DOM mutation
- [ ] Rendering/input: Canvas 2D, CSS transforms/positioning/z-index, image tiles, Pointer/Wheel/Touch events, resize, and high-DPI scaling
- [ ] Evaluate WebGL and Worker/Blob URL requirements against the chosen SDK configuration; implement only from measured blockers
- [x] Define three acceptance levels with evidence requirements:
  - **Level 1 — loader succeeds:** bootstrap, JSONP/chunks, cache/retry and
    lifecycle events complete without a second engine. The hermetic CI fixture
    passes this level; the selected vendor SDK remains a separate Gate C run.
  - **Level 2 — SDK initializes:** the loaded graph publishes its documented
    factory and can create an instance against a real DOM container. The
    hermetic CI fixture passes this level; vendor API/configuration acceptance
    is still pending.
  - **Level 3 — fully interactive:** rendered tiles plus pointer, touch, wheel
    zoom, resize and high-DPI behavior pass screenshot/input assertions on the
    selected vendor SDK. This level remains pending and must not be inferred
    from the loader fixture.

## Phase 3.7 — Runtime Distribution and Binary Size

### W3COS OS shared-runtime model
- [ ] Define a stable versioned W3COS application ABI for DOM, layout/render, W3 core semantics, W3VM, networking, storage, windowing, and standard components
- [ ] Install one system copy of the runtime, renderer, fonts, decoders, compiler service (if present), and dynamic JS support
- [ ] Package OS-native applications as AOT business code + metadata + resources + shared-runtime references
- [ ] Support ABI compatibility negotiation and side-by-side runtime versions where an in-place upgrade is unsafe
- [ ] Initial size targets excluding application assets: simple OS-linked app ≤1 MB; normal OS-linked app ≤5 MB

### Standalone iOS/Android model
- [ ] Produce capability-driven runtime feature sets; applications link only the APIs and subsystems reachable from their manifest/build graph
- [ ] Select exactly one primary render backend per mobile artifact; do not ship Skia, Vello/wgpu, and tiny-skia/softbuffer together
- [ ] Use system fonts where possible; do not embed the 9.6 MB CJK font in every standalone application
- [ ] Disable default image-codec features and enable only required formats (for example PNG/JPEG/WebP)
  - [x] Remove the unused AVIF encoder (`ravif`/`rav1e`) from the ordinary
    raster closure and retain it as explicit `image-avif-encode` capability.
    AVIF decoding is a separate native-decoder boundary.
- [x] Keep compiler/SWC/W3VM out of ordinary AOT applications; include parser + W3VM only for Browser or explicitly dynamic targets
  - `scripts/check-aot-dependency-boundary.sh` enforces the default runtime boundary in CI
  - `dynamic-js` is an explicit opt-in runtime feature
  - Browser-only WOFF/WOFF2 and Brotli decoding follows the same feature
    boundary and is absent from the ordinary AOT dependency graph
- [ ] Use Android App Bundles / ABI splits and measure the iOS App Store device slice rather than universal simulator artifacts
- [ ] Initial stripped, uncompressed, per-ABI targets excluding application assets: minimal AOT UI ≤20 MB; complete single-renderer runtime ≤60 MB; Browser dynamic runtime ≤120 MB

### Release-size engineering
- [x] Add size-focused release profile (`opt-level = "z"`, LTO, one codegen unit, `panic = "abort"`, symbol stripping)
  - Platform-specific linker dead-code elimination still requires validation in each Apple/Android packaging pipeline
- [x] Measure strip separately from feature removal: stripping removes symbols but not renderer code, decoders, embedded fonts, or application assets
  - The policy and distinction are documented in `docs/distribution-size.md`
- [ ] Add CI size reports for executable text/data, embedded resources, native libraries, per-ABI package size, and compressed download size
  - [x] `w3cos-size-audit` emits the common versioned JSON report. CI now builds,
    runs and audits real stripped ordinary-AOT and dynamic-browser linkage
    probes instead of measuring the audit tool itself, then uploads both reports.
    The Browser probe uses the same-runner AOT size as a baseline and fails when
    dynamic linkage adds more than 50%, avoiding cross-architecture absolute
    baseline noise.
  - [x] The current arm64 macOS linkage probes measure 6,083,264 bytes for
    ordinary DOM/jsdom AOT, including the shared page `fetch` implementation,
    and 7,148,896 bytes for a probe that incrementally
    parses HTML and executes SWC → W3IR → W3VM while retaining the network
    loader, WOFF/WOFF2 decoder and configurable persistent-cache/retry path, a
    1,065,632-byte (17.52%) dynamic increment.
  - Product gates still need Android ABI-split and iOS App Store device-slice artifacts
- [ ] Add dependency/symbol attribution (`cargo bloat` or platform equivalents) and fail CI on unexplained size regressions above an agreed threshold
- [ ] Track the current full-runtime Monaco artifact as a baseline (about 172 MB unstripped / 120 MB fully stripped on arm64 macOS); replace it with reproducible per-feature baselines
- [ ] Verify that standalone W3COS applications remain materially smaller than shipping a complete Chromium stack; treat a regression toward Chromium-class size as an architecture issue, not something `strip` alone will fix

## Phase 4 — Operating System ✅ (core done)
- [x] w3cos-shell crate: native desktop shell binary (taskbar, icons, system tray)
- [x] Boot pipeline: S99w3cos init → framebuffer detect → w3cos-shell fullscreen
- [x] GitHub Actions build-iso.yml: auto-build ISO on version tag push
- [x] Buildroot post-build: installs w3cos-shell + CLI + example apps
- [x] QEMU script: --download flag, KVM detect, SSH forwarding
- [x] Bootable ISO (Buildroot) available on GitHub Releases (#20)
- [x] W3C OS as system shell (replaces desktop environment)
- [ ] AI system agent with privileged APIs
- [ ] Package manager for W3C OS applications
- [ ] Multi-device sync protocol
- [ ] App store / registry

## Phase 4.1 — Standard Native Shell

Detailed template and host delivery plan:
[`docs/SHELL_TEMPLATES.md`](docs/SHELL_TEMPLATES.md).

### Current-shell hardening
- [ ] Replace the current single-signal, in-process app switcher with the real `AppRegistry`, `WindowManager`, compositor, and process/application lifecycle
- [ ] Move Files, Terminal, Settings, Browser, Editor, and AI Agent out of static shell demo builders into registered system applications
- [ ] Make title-bar controls functional: close, minimize, maximize/restore, move, resize, focus, z-order, modal ownership, and fullscreen
- [ ] Add window snapping, multi-workspace/virtual-desktop support, task switching, app grouping, launch activation, and session restore
- [ ] Add process supervision: launch, readiness, crash UI, restart, hang detection, resource limits, and clean shutdown
- [ ] Persist shell state separately from application state; recover safely after shell or app crashes

### Standard shell service protocol
- [ ] Define a versioned `w3cos.shell` protocol over typed IPC; the Shell UI consumes services and must not own authoritative app/process/window state
- [ ] Define stable models for `AppIdentity`, `WindowRef`, `LaunchRequest`, `DeepLink`, `FileOpenRequest`, `ShellCommand`, `Notification`, `Progress`, and `ActionOutcome`
- [ ] Add command registry and global shortcut routing; applications declare commands, labels, accelerators, availability, and permission requirements
- [ ] Implement command palette/global search over installed apps, windows, files, settings, commands, and explicitly exposed application content
- [ ] Implement app/file associations, default applications, `openExternal`, `showItemInFolder`, share/open-with, and drag/drop routing
- [ ] Turn the existing menu, dialog, notification, manifest, IPC, and multi-window data models into Shell-consumed services with end-to-end tests
- [ ] Add notification center with grouped history, action buttons, progress updates, quiet mode, badge counts, and per-app preferences
- [ ] Add real system tray/status services for time, locale, network, battery/power, audio, input method, accessibility, and background activity

### Desktop session and system UX
- [ ] Add login/session bootstrap, lock screen, screen blanking, suspend, restart, shutdown, and privileged confirmation surfaces
- [ ] Add persistent Settings for display/scale, theme, locale, keyboard, accessibility, notifications, privacy, permissions, networking, and default apps
- [ ] Add clipboard history as an opt-in protected service; redact secrets and allow applications to disable history for sensitive content
- [ ] Add accessibility-first keyboard navigation, focus rings, screen-reader announcements, high contrast, reduced motion, and scalable shell chrome
- [ ] Add multi-display geometry, per-display scale, work areas, hot-plug handling, and window placement recovery
- [ ] Keep desktop Shell, mobile Shell, and ordinary app chrome separate while sharing protocol models, permissions, notification, command, context, and AI surfaces

### Platform Shell hosts
- [ ] Extract a platform-neutral `w3cos-shell-core` containing Shell state machines, typed protocols, command/notification/context models, session persistence, and AI surfaces without direct Win32, Cocoa, UIKit, Android, Wayland, or X11 dependencies
- [ ] **Linux system Shell:** own the full desktop session from login to compositor, application processes, taskbar/workspaces, notifications, power/session controls, Wayland-first input/output, and X11 compatibility where required
- [ ] **Windows desktop host:** implement Win32 window lifecycle, taskbar/tray, jump lists, notifications, file associations, protocol activation, global shortcuts, multi-display/DPI, accessibility, and optional kiosk/custom-shell mode; do not require replacing Explorer for normal applications
- [ ] **macOS desktop host:** implement NSApplication/NSWindow lifecycle, global menu bar, Dock, notifications, file/protocol activation, Spaces/fullscreen behavior, permissions, Retina scaling, accessibility, and standard app sandbox integration; treat it as an application host rather than replacing Finder/loginwindow
- [ ] **Android mobile host:** keep a single-application Activity shell with Surface lifecycle, edge-to-edge/safe areas, system bars, back/navigation intents, deep links/share, runtime permissions, notifications, IME, accessibility, background/foreground lifecycle, and process recreation
- [ ] **iOS/iPadOS mobile host:** keep a single-application UIKit shell with Scene lifecycle, safe areas, status bar/home indicator, deep links/share, permissions, notifications, IME, accessibility, background/foreground lifecycle, state restoration, and iPad multi-window where the app opts in
- [ ] Keep platform hosts thin: native code owns OS lifecycle/surfaces/permissions, while layout, DOM, commands, context, AI sessions, and portable Shell presentation remain in shared W3COS code
- [ ] Define a versioned Host ABI for surface creation, input, insets, lifecycle, notifications, permissions, dialogs, clipboard, file/protocol activation, power/session capabilities, and accessibility
- [ ] Add per-platform capability discovery so unsupported operations fail explicitly instead of being rendered as working Shell controls
- [ ] Add build and smoke-test matrices for Linux x86_64/ARM64, Windows x86_64/ARM64, macOS arm64/x86_64 where supported, Android ABI splits, and iOS device/simulator slices

### Standard Shell delivery gates
- [ ] **Shell M1:** launch two separately registered applications, move/resize/focus/minimize/close them, and restore the session after Shell restart
- [ ] **Shell M2:** command palette, notifications, menus/dialogs, deep links, file associations, system tray, and Settings operate through typed Shell services
- [ ] **Shell M3:** lock/power/session recovery, accessibility, multi-display, crash containment, and process supervision pass ISO/QEMU and native desktop tests
- [ ] **Shell M4:** the same neutral command, notification, context, permission, and AI-session fixtures pass through Linux, Windows, macOS, Android, and iOS Host adapters with declared capability differences
- [ ] Remove static/mocked CPU, memory, network, battery, clock, and browser content from the production Shell path once their real services land

## Phase 4.2 — AI-Native Shell Services

### Upstream/downstream boundary
- [ ] Keep W3COS generic: upstream only neutral Shell/AI contracts, services, surfaces, and fixtures; product scenarios, business cards, policies, and authoritative write paths remain downstream
- [ ] Promote a downstream pattern upstream only after it has a neutral name, no domain fields, a versioned contract, permission semantics, and a generic conformance fixture
- [ ] Provide adapters so downstream products can keep their existing Portable UI/action contracts while targeting standard W3COS Shell services
- [ ] Do not copy a product Shell into `w3cos-shell`; extract reusable context, input, action, feedback, task, and notification primitives

### Unified context, intent, and action contracts
- [ ] Define `ShellContextSnapshot`: active app/window, route, focused control, selection, compact accessibility tree, user-visible object references, locale, device posture, and freshness
- [ ] Make context capability-scoped, redacted, user-inspectable, and revocable; applications explicitly declare what may be exposed to agents
- [ ] Define one `ShellIntent`/command path used by humans, keyboard shortcuts, application UI, voice input, and AI agents
- [ ] Define structured action lifecycle: propose → preflight → permission check → impact summary → confirmation → execute → outcome → feedback/transition
- [ ] Carry stable action/operation IDs, idempotency keys, cancellation, retry policy, progress, source/target references, and human-readable failure information
- [ ] Distinguish in-place updates, result feedback, related-object creation, app/window navigation, and explicit handoff; AI must not silently change the user's primary focus
- [ ] Route authoritative writes back to the owning application/service; Shell and AI surfaces orchestrate but do not become a second business write path

### Global AI interaction surfaces
- [ ] Add a summonable global AI bar with text input plus capability-gated voice, clipboard, file, screenshot/OCR, camera, and selection attachments
- [ ] Provide a neutral, inspectable Shell context header with source/freshness indicators
- [ ] Add an Agent panel/task center for active and background sessions, progress, pause/resume/cancel, pending questions, failures, results, and cross-app handoffs
- [ ] Add a reusable Portable result/action surface for neutral cards, confirmations, impact summaries, progress, retry, and auditable outcomes
- [ ] Add recommendation surfaces with accept/dismiss/why controls; recommendations never execute privileged or destructive actions without policy and confirmation
- [ ] Unify transient Toast, inline feedback, result cards, notification-center entries, and persistent task history through an explicit presentation policy
- [ ] Make the same AI session reachable from desktop panel, mobile full-screen/sheet presentation, notification action, and application-embedded surface without duplicating session state

### Permission, approval, evidence, and audit
- [ ] Replace coarse global AI booleans with capability grants scoped by agent, app, window/document, selector/resource, operation, duration, and data sensitivity
- [ ] Add a system permission/approval broker with allow-once, allow-for-session, persistent grant, deny, revoke, and administrator policy
- [ ] Require impact preview and explicit confirmation for destructive, financial, identity, credential, process, filesystem, device, and cross-app actions
- [ ] Record an append-only action receipt containing actor, intent, target, permission decision, confirmation, operation ID, outcome, and user-visible evidence references
- [ ] Never expose hidden chain-of-thought, raw secrets, unrestricted DOM dumps, or cross-application data in Shell UI, notifications, logs, or agent context
- [ ] Add rate limits, budgets, cancellation, background-execution indicators, emergency stop, and visible control whenever an agent is acting

### AI-Native Shell delivery gates
- [ ] **AI Shell M1:** summon the AI bar, inspect/revoke shared context, propose a neutral read-only command, and render its structured result
- [ ] **AI Shell M2:** complete a permissioned cross-app flow with preflight, confirmation, progress, cancellation, outcome, notification, and audit receipt
- [ ] **AI Shell M3:** run the same neutral fixture through desktop Shell, mobile Shell, and a downstream adapter with equivalent action and permission semantics
- [ ] Add adversarial tests for prompt/content injection, stale context, confused deputy, hidden-window access, permission escalation, replay, duplicate execution, and sensitive-data leakage
