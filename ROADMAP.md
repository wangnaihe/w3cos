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
- [x] w3cos-runtime: Taffy 0.9 (Flex/Grid/Block/position) + tiny-skia + winit
- [x] w3cos-runtime: Mouse events, hover, click, hit-testing
- [x] w3cos-cli: `w3cos build` and `w3cos run`
- [x] CSS: Flexbox, Grid, Block, position relative/absolute, overflow, z-index
- [x] CSS: rem, em, vw, vh, box-shadow, transform, transition, opacity
- [x] 4 example apps (hello, counter, dashboard, showcase)
- [x] Dockerfile + .devcontainer
- [x] Buildroot config + QEMU scripts + INSTALL.md
- [x] ARCHITECTURE.md, README.md, CONTRIBUTING.md, ISSUES.md

## Phase 1 — Interactive Apps
- [ ] Reactive state system (signal/memo/effect)
- [ ] Event handlers in TS (onClick, onInput compiled to Rust callbacks)
- [ ] TSX syntax support (via SWC integration)
- [ ] Text input component
- [ ] display: inline / inline-block (Parley text reflow integration)
- [ ] position: fixed / sticky
- [ ] CSS transitions (animated with frame loop)
- [ ] @keyframes animation
- [ ] Scroll support (overflow: scroll with mouse wheel)
- [ ] Image component (PNG/JPEG decoding)
- [ ] Focus management + keyboard navigation

## Phase 2 — Production Quality
- [ ] GPU rendering (Vello + wgpu — replace tiny-skia)
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
