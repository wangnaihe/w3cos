# W3C OS Roadmap

## Phase 0 — Skeleton ✅
- [x] Cargo workspace (9 crates)
- [x] w3cos-std: Component, Style, Color, Dimension (rem/em/vw/vh)
- [x] w3cos-std: BoxShadow, Transform2D, Transition, Easing
- [x] w3cos-dom: Document, Element, Node arena, CSSStyleDeclaration
- [x] w3cos-dom: Events (click/mouse/key/focus/scroll)
- [x] w3cos-dom: querySelector, classList, setAttribute
- [x] w3cos-a11y: DOM → ARIA tree, flatten for AI
- [x] w3cos-ai-bridge: DOM access + a11y API + screenshot + permissions
- [x] w3cos-compiler: JSON + TS parsing → Rust codegen
- [x] w3cos-runtime: Taffy 0.9 (Flex/Grid/Block/position) + Vello GPU / tiny-skia CPU + winit
- [x] w3cos-runtime: Mouse events, hover, click, hit-testing
- [x] w3cos-cli: `w3cos build`, `w3cos run`, `w3cos dev`, `w3cos init`
- [x] CSS: Flexbox, Grid, Block, position relative/absolute/fixed/sticky, overflow, z-index
- [x] CSS: rem, em, vw, vh, box-shadow, transform, transition, opacity
- [x] 9 example apps (hello, counter, dashboard, showcase, calculator, weather, settings-panel, etc.)
- [x] Dockerfile + .devcontainer
- [x] Buildroot config + QEMU scripts + INSTALL.md
- [x] ARCHITECTURE.md, README.md, CONTRIBUTING.md, ISSUES.md

## Phase 1 — Interactive Apps ✅
- [x] Reactive state system (signal/create_signal/get_signal/set_signal)
- [x] Event handlers in TSX (onClick compiled to EventAction)
- [x] TSX syntax support (built-in parser, SWC integration planned #10)
- [x] Text input component (TextInput with keyboard input)
- [x] display: inline / inline-block (Taffy flex approximation)
- [x] position: fixed / sticky
- [x] CSS transitions (animated with 60fps frame loop)
- [x] @keyframes animation support (Animation struct, keyframe types) (#11)
- [x] Scroll support (overflow: scroll with mouse wheel)
- [x] Image component (placeholder rendering, full decode #2)
- [x] Focus management + keyboard navigation (Tab/Shift+Tab)

## Phase 1.5 — TypeScript → Rust Transpiler ✅
- [x] General TS → Rust transpilation (SWC parser → Rust codegen)
- [x] Closures with Rc<RefCell<T>> capture + move semantics
- [x] async/await → async fn + .await + tokio runtime
- [x] Promise.all/race → tokio::join!/select!
- [x] GC → Reference Counting (conservative Rc<RefCell<T>> strategy)
- [x] w3cos-core crate: Value dynamic type system (JS-compatible)
- [x] w3cos-core: JsObject with HashMap properties + prototype chain
- [x] w3cos-core: Proxy with all 13 ECMAScript handler traps + ProxyBuilder
- [x] w3cos-core: Signal<T> / Computed<T> / Effect / watch() / batch()
- [x] Compiler: new Proxy(target, handler) → ProxyBuilder codegen
- [x] Compiler: reactive() → Signal expansion (compile-time optimization)
- [x] Compiler: watch()/computed()/effect() → w3cos-core API calls
- [x] Compiler: reactive property access/assignment → signal.get()/set()
- [x] Dynamic dependency generation (needs_core/needs_async/needs_rc/needs_fetch flags)
- [x] Compiler: fetch() → w3cos_runtime::fetch bridge codegen
- [ ] Escape analysis optimization for Rc<RefCell<T>> elision (P5)
- [ ] typeof operator runtime support via Value::type_of()

## Phase 2 — System APIs & Production Quality ✅ (core APIs done)
- [x] GPU rendering (Vello + wgpu — replace tiny-skia, CPU fallback via feature flag) (#12)
- [x] System bridge: File System Access API → Linux FS (#16)
- [x] System bridge: Fetch API → native HTTP client (ureq) (#15)
- [x] System bridge: Clipboard API (#17 — arboard integration)
- [x] System bridge: Notifications API (#18 — notify-rust)
- [x] System bridge: setTimeout / setInterval / requestAnimationFrame (#33)
- [x] System bridge: Child Process API (spawn/exec/pipe) (#35)
- [x] System bridge: Pseudo Terminal (PTY) API (#36)
- [x] System bridge: Path utilities + Environment variables
- [x] CSS Text properties: text-align, white-space, line-height, letter-spacing, text-decoration, text-overflow, font-family, font-style, word-break (#31)
- [x] CSS Custom Properties: var(--x) support in Style struct (#34)
- [x] CSS Containment: `contain` property (None/Layout/Size/Content/Strict) for layout isolation
- [x] Hot reload during development (`w3cos dev` with file watcher) (#13)
- [x] Live demo infrastructure (Docker + noVNC remote desktop)
- [ ] Multiple windows (#21)
- [ ] React hooks compatibility layer (@w3cos/react-compat)
- [ ] React Native API mapping (@w3cos/rn-compat) (#19)
- [ ] Wire up AI Bridge to runtime (end-to-end AI agent demo) (#14)

## Phase 2.5 — Dynamic DOM & Performance ✅ (core done)

### Dynamic DOM (#30) ✅
- [x] Thread-local Document in runtime (`w3cos_runtime::dom`)
- [x] W3C DOM API wrappers: createElement, createTextNode, appendChild, removeChild, insertBefore
- [x] DOM attributes: setAttribute, getAttribute, setTextContent
- [x] DOM style: setStyleProperty via CSSStyleDeclaration
- [x] DOM events: addEventListener with EventAction integration
- [x] DOM queries: querySelector, querySelectorAll, getElementById
- [x] DOM-driven rendering: `run_app_dom(setup)` entry point
- [x] Dual dirty tracking: signal dirty + DOM dirty → to_component_tree() → layout → render
- [x] Compiler DOM codegen backend: `generate_dom()` emits createElement/appendChild calls
- [x] Improved to_component_tree(): flex-direction → Row/Column, img, input, h1-h6 defaults
- [x] Event bubbling: dispatch_event_bubbling walks parent chain
- [x] Backward compatible: existing Component-mode apps unchanged

### DOM Performance (Chrome/Blink Algorithms) ✅
- [x] Interned Atoms: O(1) string comparison, 45 pre-interned common tags/attrs
- [x] LCRS Tree: first_child/last_child/next_sibling/prev_sibling — O(1) tree mutations
- [x] HashMap indexes: O(1) getElementById, querySelector by class/tag
- [x] Direct event bubbling: walk parent pointers, no HashMap per event
- [x] Node freelist: arena slot recycling for bounded memory
- [x] Scoped dirty propagation: mark_dirty walks to nearest `contain` boundary
- [x] CSS `contain` property: layout isolation for incremental re-layout

## Phase 2.75 — VS Code Compatibility (see docs/vscode-compat.md)
- [ ] Canvas 2D API (CanvasRenderingContext2D) (#32)
- [ ] Selection API (window.getSelection, Range) (#37)
- [ ] CSS Selectors engine (:hover, :focus, .class, [attr])
- [ ] Web Workers
- [ ] WebSocket API
- [ ] localStorage / IndexedDB
- [ ] w3cos.window (multi-window management) (#21)
- [ ] w3cos.dialog (open/save/message dialogs)
- [ ] w3cos.ipc (inter-process communication)
- [ ] w3cos.menu (application/context menus)
- [ ] RegExp (full JS spec)
- [ ] TextEncoder / TextDecoder

## Phase 3 — Compatibility & Migration
- [ ] React Native app auto-migration tool (`w3cos migrate --from rn`)
- [ ] Electron app AST transpiler (strip Chromium, map APIs)
- [ ] PWA manifest support
- [ ] npm package compatibility (pure-logic packages)
- [ ] Cross-compilation: Linux x86/ARM, macOS

## Phase 4 — Operating System
- [ ] Bootable ISO (Buildroot) available on GitHub Releases (#20)
- [ ] W3C OS as system shell (replaces desktop environment)
- [ ] AI system agent with privileged APIs
- [ ] Package manager for W3C OS applications
- [ ] Multi-device sync protocol
- [ ] App store / registry
