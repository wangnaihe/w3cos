# Pre-written Issues for GitHub Launch

Create these as GitHub Issues after pushing the repo.

---

## Core — Reactive State System

### 1. [ai-ready][P0] Implement signal/memo/effect reactive primitives
**Module**: w3cos-std + w3cos-runtime
**Difficulty**: Hard

Implement `signal<T>()`, `memo()`, and `effect()` reactive primitives. When a signal value changes, all dependent memos recompute and effects re-execute, triggering a UI re-render. This is the foundation for any interactive application.

**Acceptance Criteria**:
- `Signal::new(0)` creates a reactive value
- `Signal::set(1)` triggers re-render of dependent UI
- No virtual DOM diffing — direct subscription model

---

### 2. [ai-ready][P0] Implement onClick/onInput event handlers in TS compilation
**Module**: w3cos-compiler + w3cos-runtime
**Difficulty**: Medium

Currently events are handled at runtime via hit-testing. Compile TS event handlers (`onClick`, `onInput`) into Rust callback functions that are registered with the event system.

---

### 3. [ai-ready][P1] Integrate SWC for full TypeScript + TSX parsing
**Module**: w3cos-compiler
**Difficulty**: Hard

Replace the hand-written TS parser with SWC (`swc_ecma_parser`) for full TypeScript + JSX/TSX syntax support.

---

## CSS & Layout

### 4. [ai-ready] Implement `position: fixed` and `position: sticky`
**Module**: w3cos-runtime/layout.rs
**Difficulty**: Medium

Fixed: position relative to viewport. Sticky: position relative to scroll container until threshold.

---

### 5. [ai-ready] Implement `display: inline` and `display: inline-block`
**Module**: w3cos-runtime + parley integration
**Difficulty**: Hard

Integrate Parley text layout with Taffy block layout for inline content flow and word-wrap.

---

### 6. [good first issue] Implement CSS `transition` animation playback
**Module**: w3cos-runtime/window.rs
**Difficulty**: Medium

The transition data structure and easing functions exist. Implement the frame loop that interpolates style values over time when they change.

---

### 7. [good first issue] Implement `@keyframes` animation support
**Module**: w3cos-std/style.rs + w3cos-compiler + w3cos-runtime
**Difficulty**: Medium

Parse `@keyframes` declarations, store as animation data, play back with frame loop.

---

## Rendering

### 8. [ai-ready] GPU rendering via Vello + wgpu
**Module**: w3cos-runtime/render.rs
**Difficulty**: Hard

Replace tiny-skia CPU rendering with Vello GPU rendering for 5-10x performance improvement.

---

### 9. [good first issue] Implement Image component (PNG/JPEG)
**Module**: w3cos-std + w3cos-runtime/render.rs
**Difficulty**: Low-Medium

Add `Image { src }` component kind. Decode PNG/JPEG at compile time or runtime. Render with tiny-skia.

---

## AI Integration

### 10. [ai-ready] Wire up AI Bridge to runtime (end-to-end AI agent demo)
**Module**: w3cos-ai-bridge + w3cos-runtime
**Difficulty**: Medium

Connect the AI agent interface to the live runtime so an external AI can observe the UI, click buttons, and modify elements in a running application. Expose via HTTP or Unix socket.

---

### 11. [good first issue] Add annotated screenshot rendering
**Module**: w3cos-ai-bridge/screenshot.rs
**Difficulty**: Medium

Draw numbered circles on interactive elements in the screenshot PNG. Map numbers to DOM node IDs.

---

## System

### 12. [ai-ready] Build first bootable ISO with Buildroot
**Module**: system/
**Difficulty**: Hard

Use the Buildroot config to produce a working x86_64 ISO that boots to the W3C OS Shell. Test in QEMU.

---

### 13. [good first issue] Add `--strip` and `--lto` flags for smaller binaries
**Module**: w3cos-cli/main.rs
**Difficulty**: Low

Pass strip/LTO/codegen-units options to cargo build for smaller release binaries (2.4MB → ~1MB).

---

## Compatibility

### 14. [ai-ready] React Native compatibility layer (View/Text/TouchableOpacity mapping)
**Module**: new crate `w3cos-rn-compat`
**Difficulty**: Medium

Map React Native components to W3C OS equivalents: `View→Column`, `Text→Text`, `TouchableOpacity→Button`, `StyleSheet.create→inline style`.

---

### 15. [ai-ready] Electron app AST transpiler (proof of concept)
**Module**: new crate `w3cos-electron-compat`
**Difficulty**: Very Hard

Extract JS/HTML/CSS from Electron .asar, transpile Electron API calls to W3C OS system bridge.

---

## Developer Experience

### 16. [good first issue] Add more example applications
**Module**: examples/
**Difficulty**: Low

Create examples: settings panel, login form, music player, file browser, calculator, weather app, chat UI.

---

### 17. [ai-ready] Implement `w3cos dev` with hot reload
**Module**: w3cos-cli
**Difficulty**: Medium

File watcher that re-compiles and restarts the app on source changes. Sub-second iteration cycle.

---

### 18. [good first issue] Add `w3cos init` scaffolding command
**Module**: w3cos-cli
**Difficulty**: Low

Generate a new W3C OS project with template app.ts, Cargo.toml, and README.
