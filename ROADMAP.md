# W3C OS Roadmap

## Phase 0 — Skeleton ✅
- [x] Cargo workspace (7 crates)
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
- [x] w3cos-cli: `w3cos build` and `w3cos run`
- [x] CSS: Flexbox, Grid, Block, position relative/absolute, overflow, z-index
- [x] CSS: rem, em, vw, vh, box-shadow, transform, transition, opacity
- [x] 4 example apps (hello, counter, dashboard, showcase)
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
- [ ] @keyframes animation (#11)
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
- [x] Dynamic dependency generation (needs_core/needs_async/needs_rc flags)
- [ ] Escape analysis optimization for Rc<RefCell<T>> elision (P5)
- [ ] typeof operator runtime support via Value::type_of()

## Phase 2 — Production Quality
- [x] GPU rendering (Vello + wgpu — replace tiny-skia, CPU fallback via feature flag)
- [ ] System bridge: File System Access API → Linux FS
- [ ] System bridge: Fetch API → native HTTP client
- [ ] System bridge: Clipboard API
- [ ] System bridge: Notifications API
- [ ] Multiple windows
- [ ] Hot reload during development (`w3cos dev` with file watcher)
- [ ] React hooks compatibility layer (@w3cos/react-compat)
- [ ] React Native API mapping (@w3cos/rn-compat)

## Phase 3 — Compatibility & Migration
- [ ] React Native app auto-migration tool (`w3cos migrate --from rn`)
- [ ] Electron app AST transpiler (strip Chromium, map APIs)
- [ ] PWA manifest support
- [ ] npm package compatibility (pure-logic packages)
- [ ] Cross-compilation: Linux x86/ARM, macOS
## Phase 4 — Operating System
- [ ] Bootable ISO (Buildroot) available on GitHub Releases
- [ ] W3C OS as system shell (replaces desktop environment)
- [ ] AI system agent with privileged APIs
- [ ] Package manager for W3C OS applications
- [ ] Multi-device sync protocol
- [ ] App store / registry
