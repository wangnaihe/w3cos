# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **Web Standard APIs** — completes Phase 2.75 platform layer:
  - **WebSocket** (`w3cos_runtime::websocket`) — RFC 6455 client over `tungstenite`. Browser-style `WebSocket::connect`/`send_text`/`send_binary`/`close`/`poll_events`, `ReadyState` enum, queued events for reactive frame loops.
  - **IndexedDB** (`w3cos_runtime::indexed_db`) — object stores with key paths, auto-increment, indexes, and transactions. Backed by `~/.w3cos/indexeddb/<name>.json` so data survives restarts. Mirrors `IDBDatabase`/`IDBTransaction`/`IDBObjectStore`.
- **w3cos Platform APIs** — bridges previously missing Electron-class capabilities:
  - **`w3cos.dialog`** (`w3cos_runtime::dialog`) — native open / open-multi / open-directory / save / message dialogs via `rfd` (XDG Portal / GTK / Cocoa / Win32). Non-blocking `DialogReceiver<T>`.
  - **`w3cos.ipc`** (`w3cos_runtime::ipc`) — typed length-prefixed JSON message bus over Unix Domain Sockets (Linux/macOS) or TCP loopback (Windows). Multi-client `IpcServer` with `broadcast` / `send_to`, `IpcClient` with reader+writer worker threads.
  - **`w3cos.menu`** (`w3cos_runtime::menu`) — application menu bar + context menu data model with `MenuItem`/`MenuItemKind` (Normal/Separator/Checkbox/Radio), accelerators, roles, and a global `MenuEvent` queue.
- **AI Bridge end-to-end** (#14) — runtime now installs a `ScreenshotProvider` backed by the new `frame_cache` module. The CPU renderer caches each frame; the AI Bridge `/screenshot` endpoint returns a PNG-encoded snapshot of the latest frame instead of a stub error response.
- **`w3cos-react-compat` crate** — React hooks compatibility on top of `w3cos-core` signals: `use_state` / `use_effect` / `use_memo` / `use_callback` / `use_ref` / `use_reducer` / `use_context` / `provide_context` / `flush_sync` with a slot-table render lifecycle (`begin_render` / `end_render` / `mark_dirty` / `take_dirty` / `unmount`).
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
