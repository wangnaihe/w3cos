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
- [x] w3cos-compiler: JSON + TS parsing → Rust codegen (Component + DOM backends)
- [x] w3cos-runtime: Taffy 0.9 (Flex/Grid/Block/position) + Vello GPU / tiny-skia CPU + winit
- [x] w3cos-runtime: Mouse events, hover, click, hit-testing
- [x] w3cos-cli: `w3cos build`, `w3cos run`, `w3cos dev`, `w3cos init`
- [x] CSS: Flexbox, Grid, Block, position relative/absolute/fixed/sticky, overflow, z-index
- [x] CSS: rem, em, vw, vh, box-shadow, transform, transition, opacity
- [x] 13 example apps (hello, counter, dashboard, showcase, calculator, weather, settings-panel, chat-ui, css-demo, scss-demo, desktop-shell, file-manager, terminal, ai-agent)
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
- [x] typeof operator runtime support via Value::type_of() + standalone type_of() function

## Phase 2 — System APIs & Production Quality ✅
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
- [x] Multiple windows: W3C-standard window.open/close/focus/moveTo/resizeTo (#21)
- [x] Multi-window: w3cos:// URL scheme with app manifest + registry
- [x] Multi-window: postMessage cross-window communication
- [x] Multi-window: WindowManager with focus stack (z-order)
- [ ] React hooks compatibility layer (@w3cos/react-compat)
- [ ] React Native API mapping (@w3cos/rn-compat) (#19)
- [ ] Wire up AI Bridge to runtime (end-to-end AI agent demo) (#14)

## Phase 2.5 — Dynamic DOM & Performance ✅

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

### CSS Ecosystem ✅
- [x] External CSS file parsing: selector + property → `Stylesheet` / `CssRule` / `Selector`
- [x] CSS selectors: universal (`*`), element (DOM tags: `div`, `span`, `h1`–`h6`, etc.), class (`.name`), compound (`div.name`)
- [x] DOM selector mapping: W3COS component names → standard HTML tag aliases (Column → div, Text → span, etc.)
- [x] `className` attribute in TSX parser + codegen
- [x] `import "./styles.css"` / `import "./theme.scss"` in TSX
- [x] Style matching: `resolve_style()` merges CSS rules with inline styles (inline wins)
- [x] CSS Cascade Layers (`@layer`): explicit ordering, named/anonymous/nested layers, layer precedence in cascade
- [x] SCSS preprocessor: `grass` crate integration (feature-gated `scss`)
- [x] Compiler integration: `compile_from_file()` resolves CSS/SCSS imports, parses stylesheets, feeds to codegen

### DOM Performance (Chrome/Blink Algorithms) ✅
- [x] Interned Atoms: O(1) string comparison, 45 pre-interned common tags/attrs
- [x] LCRS Tree: first_child/last_child/next_sibling/prev_sibling — O(1) tree mutations
- [x] HashMap indexes: O(1) getElementById, querySelector by class/tag
- [x] Direct event bubbling: walk parent pointers, no HashMap per event
- [x] Node freelist: arena slot recycling for bounded memory
- [x] Scoped dirty propagation: mark_dirty walks to nearest `contain` boundary
- [x] CSS `contain` property: layout isolation for incremental re-layout

### Rendering Pipeline Optimizations ✅
- [x] Pre-flatten architecture: `FlatNodeInfo` array replaces all `flatten_tree` / `get_*_at_index` calls — O(n²) → O(n)
- [x] Viewport culling: `render_frame` skips nodes entirely outside visible area — 80%+ draw call reduction in scroll
- [x] Glyph cache: `GlyphCache` HashMap keyed by `(char, quantized_font_size)` — eliminates repeated charmap lookup + fontdue rasterize
- [x] Zero-copy animation/hover: `HashMap<usize, Style>` override table replaces `root.clone()` deep copy — only 0–2 styles cloned per frame
- [x] Scroll info precomputation: top-down `scroll_ancestor` propagation in `collect_layouts` — O(n × depth) → O(n)
- [x] Incremental layout: persistent `LayoutEngine` with cached `TaffyTree` — resize skips tree rebuild, Taffy internal caching applies
- [x] Spatial index: `SpatialGrid` (64px grid hash) for hit testing — O(n) → O(k) per `CursorMoved` (k ≈ 1–5)
- [x] Buffer reuse: `LayoutEngine`, `GlyphCache`, `SpatialGrid` persist in `App` — eliminates per-frame heap allocations
- [x] Dirty generation tracking: `paint_generation` / `layout_generation` + `needs_tree_rebuild` flag — distinguishes tree change from resize-only

### Multi-Device Adaptive Layout ✅
- [x] @media query engine: min-width, max-width, orientation, resolution, prefers-color-scheme
- [x] Compound media queries: And, Or, Not conditions
- [x] Media query string parser: "(min-width: 600px) and (max-width: 1024px)"
- [x] CSS Container Queries: component-level responsive (min-width, max-width, And)
- [x] Viewport helpers: orientation(), size_class() (Compact/Medium/Expanded)
- [x] Adaptive layout example: flex-wrap + minWidth for phone/tablet/desktop

### System GUI Examples ✅
- [x] Desktop Shell: taskbar + app launcher + system tray + desktop icons
- [x] File Manager: split-pane with directory tree + file list + toolbar
- [x] Terminal: multi-tab + colored output + input + status bar
- [x] AI Agent Hub: agent list + permissions + DOM API conversation view
- [x] Adaptive Layout: responsive dashboard that works on any screen size

## Phase 2.75 — VS Code Compatibility (see docs/vscode-compat.md)

### DOM Core ✅
- [x] replaceChild, cloneNode(deep), DocumentFragment, Comment node types
- [x] nextSibling/previousSibling/firstChild/lastChild accessors on Element
- [x] is_connected, node_type, node_name, child_element_count
- [x] getElementsByTagName, getElementsByClassName
- [x] dataset (DOMStringMap), inner_text, outer_html (read-only)
- [x] Runtime DOM bridge: all new APIs exposed via w3cos_runtime::dom

### DOM Events ✅
- [x] Full event sub-types: MouseEventData, KeyboardEventData, PointerEventData, WheelEventData
- [x] 40+ EventType variants (pointer, touch, composition, custom, etc.)
- [x] Event phases: Capturing → AtTarget → Bubbling (W3C 3-phase propagation)
- [x] Listener options: capture, once, passive
- [x] Single listener removal by ID
- [x] stop_immediate_propagation support
- [x] CustomEvent with detail

### CSS Engine ✅
- [x] 30+ new CSS properties in StyleDecl/apply_css_property (cursor, visibility, pointer-events, user-select, outline-*, text-*, flex-basis, order, align-self, align-content, etc.)
- [x] CSS shorthand parsing: flex, outline, border, margin/padding multi-value
- [x] CSSStyleDeclaration: text-align, white-space, line-height, letter-spacing, text-decoration, text-overflow, font-family/style, word-break, cursor, visibility, pointer-events, user-select, outline-*, align-self/content
- [x] Style struct: cursor, visibility, pointer_events, user_select, outline, flex_basis, order, align_self, align_content
- [x] CSS Custom Properties: var(--name, fallback) resolution with inheritance
- [x] @keyframes parsing: keyframe stops with percentage/from/to
- [x] @media parsing: rules inside @media blocks now parsed (not skipped)
- [x] @font-face parsing: font-family, src, weight, style extraction
- [x] CSS pseudo-class selectors (:hover, :focus, :active, :first-child, :last-child, :nth-child, :only-child, :empty, :not(), :disabled, :enabled, :checked)
- [x] CSS attribute selectors ([attr], [attr=value], [attr^=value], [attr$=value], [attr*=value], [attr~=value], [attr|=value])

### Canvas 2D ✅
- [x] Path: quadraticCurveTo, bezierCurveTo, ellipse, rect, clip
- [x] Image: drawImage, putImageData
- [x] Line styles: setLineCap, setLineJoin, setMiterLimit, setLineDash/getLineDash
- [x] Transform: setTransform, resetTransform
- [x] TextMetrics: width + boundingBox ascent/descent

### Selection API ✅
- [x] Range: collapse, selectNode, selectNodeContents, cloneRange, setStartBefore/After, setEndBefore/After
- [x] Selection: removeRange, containsNode, selectionType

### Observer APIs ✅
- [x] ResizeObserver (observe/unobserve/disconnect/checkForChanges)
- [x] MutationObserver (MutationType/MutationRecord)
- [x] IntersectionObserver (observe/unobserve/disconnect/checkForIntersections)
- [x] Window.matchMedia (min-width, max-width, prefers-color-scheme)

### Remaining
- [ ] Web Workers
- [ ] WebSocket API
- [x] localStorage (Web Storage API with JSON file persistence)
- [ ] IndexedDB
- [ ] w3cos.dialog (open/save/message dialogs)
- [ ] w3cos.ipc (inter-process communication)
- [ ] w3cos.menu (application/context menus)
- [ ] RegExp (full JS spec)
- [x] TextEncoder / TextDecoder (UTF-8, UTF-16LE/BE, ASCII, BOM handling)

## Phase 3 — Compatibility & Migration
- [ ] React Native app auto-migration tool (`w3cos migrate --from rn`)
- [ ] Electron app AST transpiler (strip Chromium, map APIs)
- [ ] PWA manifest support
- [ ] npm package compatibility (pure-logic packages)
- [ ] Cross-compilation: Linux x86/ARM, macOS

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
