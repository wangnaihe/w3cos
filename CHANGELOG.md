# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **SVG paint-only tile invalidation** — retained SVG rasters reuse 32px tiles when document topology and geometry stay the same and only paint (fill/stroke/opacity/visibility/text chunks) changes. Dirty bounding boxes are rerasterized with an 8px pad; topology or geometry changes still take a full tiled raster. SMIL/Web Animations still do not write computed values into this path, and there is no GPU vector tessellation.
- **Isolated dedicated Worker realms (dynamic-js)** — `new Worker(blob:)` / `new Worker(data:)` capture script bytes on the parent thread and execute them on the worker OS thread with a fresh W3VM (`self`, `postMessage`, `self.onmessage`). W3IR still rejects top-level undeclared `onmessage = …`. HTTP/file/dummy URLs still echo structured-clone messages. SharedWorker, AOT-compiled worker scripts, and MessagePort transfer into a worker are still unimplemented.
- **AOT Fetch in-flight abort** — without a page document URL, `fetch` with an AbortSignal runs native I/O on a worker and pumps timers/microtasks, so `Promise.then` / timer abort returns AbortError without waiting for the transport deadline. Background ureq I/O may still finish.

### Fixed
- **Main compiler CI** — `cargo test -p w3cos-compiler --lib --tests` now matches current ESM lowering (unbound names through the window intrinsic, `GetValue` via `get_property_checked`) and `for-in` skips non-enumerable `Object.prototype` methods. These suites were unreachable on `main` until #40 unblocked `cargo check`.
- **Runtime CI** — script-fetch cancel no longer races a 1 MiB kernel write burst; inline-block/flex shrink-wrap tests compare against the host font instead of assuming CJK em metrics.
- **ReadableStream BYOB filling** — `ReadableStreamBYOBReader.read(view)` now copies queued bytes into the supplied ArrayBufferView and returns a same-buffer prefix view. Byte-stream controllers expose a live `byobRequest`; `respond()` / `respondWithNewView()` complete in-flight BYOB reads. Leftover queued bytes stay available for the next read.
- **Compiled `for await...of`** — W3IR/AOT lowering of `for await...of` over async iterables and `ReadableStream` is covered by compiled-JS and AOT/W3VM differential tests.
- **Web Workers** (`w3cos_runtime::worker`) — W3C-standard background execution mapped onto native OS threads:
  - `Worker::spawn(opts, body)` runs a Rust closure on a dedicated thread; the closure receives a `WorkerScope` with browser-equivalent `recv` / `try_recv` / `post_message` / `report_error` methods.
  - `Worker::post_message` / `try_recv` / `poll_events` mirror the parent-side `MessageEvent` / `ErrorEvent` queue.
  - Cooperative `Worker::terminate()` drops the inbound channel and joins the thread; `WorkerScope::is_terminated` plus a polling `recv_timeout` ensures workers always exit cleanly.
  - `SharedWorker::spawn` keeps one thread alive across many `SharedWorkerPort`s (W3C `MessagePort` semantics) — `send_to(port_id, ...)`, `broadcast(...)`, per-port `poll_events`, and graceful disconnect when ports drop.
  - Examples: `cargo run -p w3cos-runtime --example worker_prime_sieve`, `cargo run -p w3cos-runtime --example pwa_install`.
- **PWA Web App Manifest support** (`w3cos_runtime::pwa`) — installs Progressive Web Apps as first-class W3C OS apps:
  - `PwaManifest::from_json` / `from_file` parse the W3C Web App Manifest (`name`, `short_name`, `id`, `start_url`, `scope`, `display`, `display_override`, `orientation`, `theme_color`, `background_color`, `icons`, `screenshots`, `shortcuts`, `categories`).
  - `PwaManifest::pick_icon(target_px)` selects the icon closest to a given square size (handles `sizes: any`, `purpose` filtering).
  - `PwaManifest::effective_display()` honours `display_override` per the spec.
  - `PwaManifest::into_app_manifest(fallback_id)` adapts a parsed manifest to a W3C OS `AppManifest` (frameless for `display: fullscreen`, derives a stable `id` from `start_url` when one isn't declared).
- **Web Standard APIs** — completes Phase 2.75 platform layer:
  - **WebSocket** (`w3cos_runtime::websocket`) — RFC 6455 client over `tungstenite`. Browser-style `WebSocket::connect`/`send_text`/`send_binary`/`close`/`poll_events`, `ReadyState` enum, queued events for reactive frame loops.
  - **IndexedDB** (`w3cos_runtime::indexed_db`) — object stores with key paths, auto-increment, indexes, and transactions. Backed by `~/.w3cos/indexeddb/<name>.json` so data survives restarts. Mirrors `IDBDatabase`/`IDBTransaction`/`IDBObjectStore`.
- **w3cos Platform APIs** — bridges previously missing Electron-class capabilities:
  - **`w3cos.dialog`** (`w3cos_runtime::dialog`) — native open / open-multi / open-directory / save / message dialogs via `rfd` (XDG Portal / GTK / Cocoa / Win32). Non-blocking `DialogReceiver<T>`.
  - **`w3cos.ipc`** (`w3cos_runtime::ipc`) — typed length-prefixed JSON message bus over Unix Domain Sockets (Linux/macOS) or TCP loopback (Windows). Multi-client `IpcServer` with `broadcast` / `send_to`, `IpcClient` with reader+writer worker threads.
  - **`w3cos.menu`** (`w3cos_runtime::menu`) — application menu bar + context menu data model with `MenuItem`/`MenuItemKind` (Normal/Separator/Checkbox/Radio), accelerators, roles, and a global `MenuEvent` queue.
- **AI Bridge end-to-end** (#14) — runtime now installs a `ScreenshotProvider` backed by the new `frame_cache` module. The CPU renderer caches each frame; the AI Bridge `/screenshot` endpoint returns a PNG-encoded snapshot of the latest frame instead of a stub error response.
- **Framework-neutral AOT path** — npm/CJS dependencies are bundled before W3COS compiles the application through its generic JavaScript and DOM runtime.
- **`w3cos-rn-compat` crate updates** — React Native mapping now exports `View` / `Text` / `TouchableOpacity` / `Pressable` / `ScrollView` / `SafeAreaView` / `Image` / `TextInput` / `FlatList` / `StatusBar` / `ActivityIndicator` / `Button` / `Switch` plus `StyleSheet.create` and `use_state`, fulfilling issue #19.
- README badges (CI, License, Rust version)
- CODE_OF_CONDUCT.md (Contributor Covenant v2.1)
- SECURITY.md (vulnerability reporting policy)
- PR template and Issue templates (Bug Report, Feature Request)
- GitHub Actions workflow for ISO builds (manual + tag trigger)
- ISO build instructions in README

### Changed
- `w3cos-runtime` no longer treats `tungstenite` as feature-gated; it is now a base dependency shared between the WebSocket client and the DevTools server.
- `w3cos-ai-bridge::server::start` retained for backwards compatibility; new `start_with_provider(port, Arc<dyn ScreenshotProvider>)` lets hosts plug in custom screenshot capture (the runtime supplies a `FrameCacheScreenshot` provider automatically when the `ai-bridge` feature is enabled).

### Fixed
- Linux `rfd` 0.15.4 `xdg-portal` builds enable the required `tokio` feature so `cargo check --workspace` compiles on current crates.io.
- ReadableStream / FileSystem async iterators publish `__w3cos_symbol_async_iterator` so compiled `for await...of` matches the W3IR protocol (the camelCase alias remains).
- Compiled jsdom SubtleCrypto smoke now exercises an unimplemented operation (`sign`); `digest` is implemented and no longer rejects.
- AI PR Review workflow writes a single `test_count=` line when `grep -c` finds zero tests (avoids GitHub Actions `Invalid format '0'`).
- `libc::mq_attr` initialization no longer names the removed `__pad` field (current `libc` / rustc 1.97).
- README screenshot now renders as inline image instead of text link

## [0.1.0] - 2025-03-17

### Added
- **w3cos-std**: Component, Style, Color, Dimension (rem/em/vw/vh), BoxShadow, Transform2D, Transition, Easing
- **w3cos-dom**: W3C DOM API — Document, Element, Node arena, Events (click/mouse/key/focus/scroll), querySelector, classList, CSSStyleDeclaration
- **w3cos-a11y**: Accessibility tree generation from DOM (ARIA roles, AI-friendly flatten)
- **w3cos-ai-bridge**: AI agent interface — DOM access, a11y API, annotated screenshot, permission system
- **w3cos-compiler**: TypeScript/JSON parser with Rust code generation (Column, Row, Text, Button, Box)
- **w3cos-runtime**: Layout engine (Taffy 0.9 — Flexbox, Grid, Block, position), 2D rendering (tiny-skia), native windowing (winit), mouse event handling
- **w3cos-cli**: `w3cos build` and `w3cos run` commands
- 4 example applications: hello, counter, dashboard, showcase
- Buildroot configuration for bootable x86_64 ISO
- QEMU run script
- Dockerfile (multi-stage build)
- DevContainer configuration (Codespaces support)
- ARCHITECTURE.md, ROADMAP.md, CONTRIBUTING.md, ISSUES.md
- CI workflow (cargo check, clippy, test, fmt)
