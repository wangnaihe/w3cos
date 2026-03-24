# VS Code Compatibility — Gap Analysis

This document maps the full gap between W3C OS platform capabilities and what is required to run VS Code (Code-OSS) as a native W3C OS application. It separates responsibilities into **platform provides** (W3C OS) and **application adapts** (VS Code).

## 1. VS Code Architecture

VS Code is built on Electron, which bundles Chromium + Node.js. It runs as three cooperating processes:

```
┌─────────────────────────────────────────────────────────┐
│  Electron                                               │
│                                                         │
│  ┌──────────────┐  IPC  ┌────────────────────────────┐  │
│  │ Main Process │◄─────►│ Renderer Process            │  │
│  │ (Node.js)    │       │ (Chromium: DOM + CSS + V8)  │  │
│  │              │       │ Monaco Editor, UI panels,   │  │
│  │ File system  │       │ tree views, tabs, status    │  │
│  │ Processes    │  IPC  │ bar, terminal (xterm.js)    │  │
│  │ Menus/Dialog │◄─────►├────────────────────────────┤  │
│  │ Window mgmt  │       │ Extension Host              │  │
│  │ OS bridge    │       │ (Node.js sandbox)           │  │
│  │              │       │ LSP clients, DAP clients,   │  │
│  └──────────────┘       │ 40K+ extensions             │  │
│                         └────────────────────────────┘  │
└─────────────────────────────────────────────────────────┘
```

### Replacement Mapping

| VS Code dependency | Role | W3C OS replacement |
|--------------------|------|--------------------|
| Chromium | DOM, CSS, rendering, V8 | w3cos-runtime (Taffy + Vello + winit) + w3cos-dom |
| Node.js | File system, processes, network | W3C standard APIs + w3cos system APIs |
| Electron | Window management, IPC, system dialogs, menus | w3cos platform APIs |
| V8 JIT | JavaScript execution | w3cos TS-to-Rust AOT compiler + w3cos-core runtime |

---

## 2. W3C OS Platform Must Provide

These are capabilities the **platform** must implement. They are not VS Code-specific; any complex web application expects them.

### 2.1 DOM / CSS / Rendering (replacing Chromium)

| Standard | API | Current Status | Priority | Notes |
|----------|-----|----------------|----------|-------|
| DOM Core | document.createElement, appendChild, removeChild, replaceChild, cloneNode, insertBefore | Partial (w3cos-dom) | P0 | Foundation for everything |
| DOM Core | querySelector, querySelectorAll, getElementById, getElementsByClassName | Partial | P0 | VS Code uses extensively |
| DOM Core | textContent, innerHTML (read-only), parentNode, childNodes, nextSibling | Partial | P0 | Tree traversal |
| DOM Core | setAttribute, getAttribute, removeAttribute, dataset | Partial | P0 | |
| DOM Core | classList (add, remove, toggle, contains) | Have | Done | |
| DOM Events | addEventListener, removeEventListener, dispatchEvent | Partial | P0 | |
| DOM Events | MouseEvent, KeyboardEvent, FocusEvent, InputEvent, WheelEvent, PointerEvent | Partial | P0 | PointerEvent needed for Monaco |
| DOM Events | CompositionEvent (IME input) | Partial (winit IME) | P1 | CJK input support |
| DOM Events | CustomEvent | None | P1 | VS Code internal events |
| CSSOM | element.style read/write, getComputedStyle() | Partial | P0 | |
| CSSOM | CSSStyleSheet, adoptedStyleSheets | None | P2 | Theme system |
| CSS Flexbox | Full spec | Have (Taffy) | Done | |
| CSS Grid | Full spec | Have (Taffy) | Done | |
| CSS Position | relative, absolute, fixed, sticky | Have | Done | |
| CSS Text | text-align, text-decoration, white-space, word-break, text-overflow, line-height, letter-spacing | Partial | P0 | Code rendering depends on this |
| CSS Text | font-family, font-weight, font-style, @font-face | Partial | P0 | Monospace font critical for editor |
| CSS Overflow | overflow: hidden/scroll/auto with native scrollbars | Basic | P1 | Virtual scrolling for large files |
| CSS Color | rgba(), hsla(), currentColor, CSS variables (var(--x)) | Partial | P1 | VS Code theme system uses CSS vars |
| CSS Selectors | :hover, :focus, :active, :focus-within, :first-child, :last-child, :nth-child, [attr] selectors | None | P1 | CSS-based hover/focus states |
| CSS Transitions | transition property | Have | Done | |
| CSS Animations | @keyframes, animation property | None | P2 | Loading spinners, cursor blink |
| CSS calc() | calc(100% - 20px) | None | P2 | Layout calculations |
| CSS Custom Props | var(--color), :root definitions | None | P1 | VS Code theme engine |
| CSS z-index | Stacking context | Have | Done | |
| CSS box-shadow | Multi-layer | Have | Done | |
| CSS border | border-width, border-style, border-color (per-side) | Partial | P1 | |
| CSS transform | translate, scale, rotate | Have | Done | |
| Canvas 2D | CanvasRenderingContext2D (fillRect, strokeRect, drawImage, fillText, measureText) | None | P1 | Monaco minimap, diff decorations |
| Selection API | window.getSelection(), Range, Selection | None | P0 | Text selection in editor |
| Clipboard API | navigator.clipboard.readText/writeText | Code exists (arboard) | P1 | Copy/paste |
| Drag and Drop | dragstart, dragover, drop, DataTransfer | None | P2 | File drag, tab reorder |
| ResizeObserver | Element resize monitoring | None | P1 | Panel/editor resize |
| MutationObserver | DOM change monitoring | None | P2 | Extension DOM patches |
| IntersectionObserver | Visibility detection | None | P2 | Virtual list optimization |
| requestAnimationFrame | Frame-synced callbacks | Implicit (frame loop) | P1 | Needs explicit JS API |
| matchMedia | Media query matching | None | P3 | Responsive layout |
| window.scrollTo | Programmatic scrolling | None | P1 | |

### 2.2 JavaScript / TypeScript Runtime (replacing V8)

| Capability | Specifics | Current Status | Priority |
|------------|-----------|----------------|----------|
| Primitive types | number, string, boolean, null, undefined, bigint, symbol | AOT compile | Done |
| Object model | Object, Array, Map, Set, WeakMap, WeakSet, WeakRef | w3cos-core Value | Partial |
| Proxy / Reflect | All 13 handler traps | w3cos-core Proxy | Done |
| async/await | async fn, Promise, Promise.all/race/allSettled/any | Compiled to tokio | Done |
| Error handling | try/catch/finally, Error/TypeError/RangeError | Compiled | Done |
| Closures | Captured variables with Rc<RefCell<T>> | Compiled | Done |
| Module system | import/export (ESM static) | Compile-time | P0 |
| Dynamic import | import("./module") | None | P2 |
| RegExp | Full JS regex spec (named groups, lookbehind, unicode) | None | P1 |
| Timers | setTimeout, setInterval, clearTimeout, clearInterval | None | P0 |
| JSON | JSON.parse, JSON.stringify | None (serde at compile) | P0 |
| console | console.log, console.error, console.warn, console.time | Compiled to println! | Partial |
| TextEncoder/Decoder | UTF-8/UTF-16 encoding | None | P1 |
| URL / URLSearchParams | URL parsing and manipulation | None | P2 |
| Intl | NumberFormat, DateTimeFormat, Collator | None | P3 |
| structuredClone | Deep clone | None | P3 |
| queueMicrotask | Microtask scheduling | None | P2 |
| EventTarget | Base class for event dispatching | None | P1 |
| AbortController | Request/operation cancellation | None | P2 |

### 2.3 System APIs (replacing Node.js + Electron)

#### W3C / WHATWG Standard APIs

| Standard | API | Replaces | Current Status | Priority |
|----------|-----|----------|----------------|----------|
| Fetch API | fetch(), Request, Response, Headers | Node.js http/https | None | P0 |
| File System Access | showOpenFilePicker, FileSystemFileHandle, read/write | Node.js fs | None | P0 |
| Web Workers | new Worker(), postMessage, SharedWorker | Node.js worker_threads | None | P1 |
| WebSocket | new WebSocket(), onmessage, send | Node.js ws / net | None | P1 |
| Storage | localStorage, sessionStorage | Electron Store | None | P1 |
| IndexedDB | Structured data storage | Node.js sqlite / json files | None | P2 |
| Notifications | new Notification() | Electron Notification | Have (notify-rust) | P2 |
| WHATWG Streams | ReadableStream, WritableStream, TransformStream | Node.js stream | None | P1 |
| WHATWG Encoding | TextEncoder, TextDecoder | Node.js Buffer | None | P1 |
| WHATWG URL | URL, URLSearchParams | Node.js url | None | P2 |

#### w3cos Platform APIs (non-standard, custom)

These have no W3C equivalent but are necessary for desktop applications. Designed as `w3cos.*` namespace APIs.

| API | Purpose | Replaces | Current Status | Priority |
|-----|---------|----------|----------------|----------|
| w3cos.process.spawn() | Launch child processes, pipe stdin/stdout | Node.js child_process | None | P0 |
| w3cos.process.exec() | Run command and capture output | Node.js child_process.exec | None | P0 |
| w3cos.pty.create() | Pseudo-terminal for interactive shells | node-pty | None | P0 |
| w3cos.window.create() | Open new application windows | Electron BrowserWindow | Single window only | P1 |
| w3cos.window.close() | Close windows programmatically | Electron BrowserWindow.close | None | P1 |
| w3cos.dialog.showOpen() | Native file open dialog | Electron dialog.showOpenDialog | None | P1 |
| w3cos.dialog.showSave() | Native file save dialog | Electron dialog.showSaveDialog | None | P1 |
| w3cos.dialog.showMessage() | Message box | Electron dialog.showMessageBox | None | P2 |
| w3cos.shell.openExternal() | Open URL in default browser | Electron shell.openExternal | None | P2 |
| w3cos.shell.showInFolder() | Reveal file in file manager | Electron shell.showItemInFolder | None | P2 |
| w3cos.ipc.send() | Inter-process communication | Electron ipcMain/ipcRenderer | None | P1 |
| w3cos.ipc.on() | IPC event listener | Electron ipcMain.on | None | P1 |
| w3cos.menu.setApp() | Application menu bar | Electron Menu.setApplicationMenu | None | P2 |
| w3cos.menu.showContext() | Context menu | Electron Menu.popup | None | P2 |
| w3cos.tray.create() | System tray icon | Electron Tray | None | P3 |
| w3cos.env.get() | Environment variables | Node.js process.env | None | P1 |
| w3cos.path.join() | Path manipulation | Node.js path | None | P1 |
| w3cos.os.platform() | OS information | Node.js os | None | P2 |
| w3cos.fs.watch() | File system watcher | Node.js fs.watch / chokidar | None | P1 |

---

## 3. VS Code Must Adapt

These are changes **VS Code's codebase** needs to make. They are application-level concerns, not platform standards.

| VS Code Module | Current Dependency | Adaptation Strategy | Effort |
|----------------|--------------------|---------------------|--------|
| `vs/base/parts/ipc` | Electron ipcMain / ipcRenderer | Replace with w3cos.ipc API calls | Medium |
| `vs/code/electron-main` | Electron app, BrowserWindow, Menu, dialog | Replace with w3cos.window, w3cos.dialog, w3cos.menu | Large |
| `vs/platform/files` | Node.js fs, path | Replace with W3C File System Access API + w3cos.path | Large |
| `vs/platform/terminal` | node-pty + xterm.js | xterm.js renders via DOM (keep); node-pty replaced by w3cos.pty | Medium |
| `vs/platform/request` | Node.js http/https | Replace with W3C Fetch API | Small |
| `vs/workbench/services/search` | ripgrep via child_process | Call rg via w3cos.process.spawn() | Small |
| `vs/editor/browser` (Monaco) | DOM + Canvas 2D + requestAnimationFrame | No change needed if platform DOM/Canvas is complete | Small |
| `vs/workbench/contrib/scm` (Git) | git CLI via child_process | Call git via w3cos.process.spawn() | Small |
| `vs/workbench/services/extensions` | Extension host (separate Node.js process) | Run in w3cos Worker or w3cos.process with TS runtime | Very Large |
| `vs/platform/native` | Native modules (.node via node-gyp): spdlog, native-keymap, nsfw | Replace with Rust FFI equivalents or pure-TS alternatives | Medium |
| `vs/base/browser/dom` | Direct DOM manipulation | No change if platform DOM is W3C-compliant | None |
| `vs/platform/configuration` | Node.js fs reading JSON settings | Replace with File System Access API | Small |
| `vs/platform/contextkey` | Context key service for keybindings | Pure TS logic, no platform dependency | None |
| Title bar / System tray | Electron-specific frameless window APIs | Use w3cos.window title/frame APIs or custom title bar | Small |
| Auto-update | Electron autoUpdater | Replace with w3cos package manager or remove | Small |
| Crash reporter | Electron crashReporter | Replace with w3cos error reporting or remove | Small |

### Adaptation Layer Strategy

Rather than forking VS Code and modifying every import, the practical approach is a **shim layer**:

```
┌─────────────────────────────────────────┐
│  VS Code Source (mostly unchanged)      │
├─────────────────────────────────────────┤
│  @electron/shim → w3cos platform APIs   │  ← Thin adapter
│  @node/shim    → W3C standard APIs      │  ← Thin adapter
├─────────────────────────────────────────┤
│  W3C OS Platform                        │
└─────────────────────────────────────────┘
```

Two shim packages intercept Electron and Node.js imports and redirect them to w3cos equivalents. This minimizes VS Code fork divergence.

---

## 4. Phased Roadmap

### Phase A — Render VS Code UI (3-6 months)

Goal: VS Code's workbench layout renders correctly as a static UI.

Platform work:
- [ ] Complete DOM Core API (createElement, appendChild, removeChild, insertBefore, cloneNode)
- [ ] Complete DOM Events (full MouseEvent, KeyboardEvent, PointerEvent, InputEvent)
- [ ] CSS Text properties (text-align, white-space, line-height, text-overflow, font-family)
- [ ] CSS Selectors engine (:hover, :focus, .class, #id, [attr])
- [ ] CSS Custom Properties (var(--color)) — VS Code themes depend on this
- [ ] Selection API (window.getSelection, Range)
- [ ] Canvas 2D context (fillRect, fillText, measureText, drawImage)
- [ ] GPU rendering (Vello + wgpu) — CPU cannot handle VS Code's DOM volume
- [ ] setTimeout / setInterval / requestAnimationFrame
- [ ] JSON.parse / JSON.stringify
- [ ] ESM module system (static import/export)

VS Code work:
- [ ] Create @electron/shim stub (window creation, basic lifecycle)
- [ ] Identify and stub Node.js fs calls for initial render

### Phase B — Code Editing Works (+6 months)

Goal: open a file, edit code with syntax highlighting, save, use terminal.

Platform work:
- [ ] W3C File System Access API (read, write, directory listing)
- [ ] w3cos.process.spawn() / exec() (run git, ripgrep, LSP servers)
- [ ] w3cos.pty.create() (terminal emulator backend)
- [ ] W3C Fetch API (extension marketplace, remote connections)
- [ ] Web Workers (extension host process v1)
- [ ] ResizeObserver
- [ ] RegExp (full JS spec)
- [ ] TextEncoder / TextDecoder
- [ ] localStorage
- [ ] w3cos.fs.watch() (file change detection)
- [ ] w3cos.env / w3cos.path

VS Code work:
- [ ] Wire file service to File System Access API via @node/shim
- [ ] Wire terminal to w3cos.pty
- [ ] Wire search service to w3cos.process (ripgrep)
- [ ] Wire SCM to w3cos.process (git)
- [ ] Extension host running in Worker with limited API surface

### Phase C — Extension Ecosystem (+6-12 months)

Goal: popular extensions (Prettier, ESLint, GitLens, language packs) work.

Platform work:
- [ ] w3cos.ipc (multi-process communication)
- [ ] w3cos.window (multi-window support)
- [ ] w3cos.dialog (open/save/message dialogs)
- [ ] w3cos.menu (application menu, context menu)
- [ ] WebSocket API (LSP over WebSocket, live share)
- [ ] Dynamic import()
- [ ] IntersectionObserver (virtual list performance)
- [ ] Drag and Drop API
- [ ] CSS @keyframes animation
- [ ] CSS calc()
- [ ] IndexedDB (extension storage)
- [ ] AbortController

VS Code work:
- [ ] Full @electron/shim (dialog, menu, shell, tray)
- [ ] Extension host with complete VS Code Extension API surface
- [ ] Native module replacements (spdlog → Rust logger, nsfw → w3cos.fs.watch)
- [ ] Marketplace integration via Fetch API

---

## 5. Architecture Boundary

```
┌─────────────────────────────────────────────────────────────┐
│  VS Code Application Layer                                  │
│  (VS Code adapts this layer to W3C OS APIs)                 │
│                                                             │
│  ┌───────────┐ ┌───────────┐ ┌───────────┐ ┌────────────┐  │
│  │ Monaco    │ │ Workbench │ │ Extension │ │ @electron/ │  │
│  │ Editor    │ │ UI        │ │ Host      │ │ shim       │  │
│  └─────┬─────┘ └─────┬─────┘ └─────┬─────┘ └──────┬─────┘  │
├────────┼─────────────┼─────────────┼──────────────┼────────┤
│  W3C OS Platform Layer                                      │
│  (W3C OS provides everything below this line)               │
│                                                             │
│  ┌─────────────────────────────────────────────────────┐    │
│  │ W3C Standard APIs                                   │    │
│  │ DOM · CSS · Events · Canvas · Fetch · WebSocket     │    │
│  │ Workers · Storage · File System Access · Clipboard  │    │
│  │ Selection · Observers · Streams · Encoding          │    │
│  ├─────────────────────────────────────────────────────┤    │
│  │ w3cos Platform APIs (non-standard)                  │    │
│  │ w3cos.process · w3cos.pty · w3cos.window            │    │
│  │ w3cos.dialog · w3cos.ipc · w3cos.menu · w3cos.env   │    │
│  │ w3cos.fs.watch · w3cos.path · w3cos.shell           │    │
│  ├─────────────────────────────────────────────────────┤    │
│  │ w3cos Runtime Engine                                │    │
│  │ TS/JS AOT Compiler · Taffy Layout · Vello Renderer  │    │
│  │ winit Windowing · fontdue/Parley Text               │    │
│  ├─────────────────────────────────────────────────────┤    │
│  │ Linux Kernel                                        │    │
│  │ Drivers · FS · Network · Process · PTY              │    │
│  └─────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────┘
```

---

## 6. What We Intentionally Exclude

These Chromium/Web features are **not** needed and will not be implemented:

| Feature | Reason |
|---------|--------|
| `document.write()` | Not AOT-compatible, security risk |
| `eval()` / `new Function()` | Not AOT-compatible |
| `innerHTML` (write) | XSS risk, not needed with DOM API |
| `<iframe>` | No cross-origin isolation needed |
| `float` layout | Legacy; Flexbox/Grid covers all use cases |
| Service Workers | No browser cache layer |
| WebRTC | Not needed for desktop IDE |
| WebGL / WebGPU (full spec) | Canvas 2D sufficient; rendering uses Vello directly |
| Cookie API | No HTTP document model |
| History API / Navigation | Single-page app, no URL routing |
| `<form>` submission | No HTTP form model |

---

## References

- [ARCHITECTURE.md](../ARCHITECTURE.md) — W3C OS system architecture
- [ROADMAP.md](../ROADMAP.md) — Current development roadmap
- [VS Code source](https://github.com/microsoft/vscode) — Code-OSS repository
- [Electron API docs](https://www.electronjs.org/docs/latest/api/app) — APIs that need shimming
