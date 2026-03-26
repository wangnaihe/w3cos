# W3C OS — Architecture Document

## One-line Definition

**A Linux-based AI-native operating system that compiles TypeScript + CSS (W3C standard DOM) to native binaries. No browser. No interpreter. No V8.**

---

## 1. Why This Exists

| Existing System | Problem |
|-----------------|---------|
| ChromeOS | Browser pretending to be an OS — killed by Google, merging into Android |
| Electron | Ships 90 MB Chromium per app, 200 MB+ RAM per window |
| React Native / Flutter | Still interpreted or VM-based, not truly native |
| Traditional OS (Win/macOS) | Closed ecosystem, AI is an afterthought, can't read UI structure |

**W3C OS fills the gap:** W3C standard DOM + CSS as the universal application framework, compiled to native machine code, with AI as a first-class system citizen that can read and operate every element in every application.

---

## 2. Architecture Overview

```
┌──────────────────────────────────────────────────────────┐
│  Applications (TypeScript + W3C DOM + CSS)                │  ← Developers write this
├──────────────────────────────────────────────────────────┤
│  w3cos-ai-bridge                                         │
│  ┌──────────┐ ┌──────────────┐ ┌──────────────────────┐ │
│  │ DOM      │ │ Accessibility│ │ Annotated Screenshot │ │
│  │ Access   │ │ Tree (ARIA)  │ │ (Claude/UI-TARS)     │ │
│  │ Layer 1  │ │ Layer 2      │ │ Layer 3              │ │
│  └──────────┘ └──────────────┘ └──────────────────────┘ │
├──────────────────────────────────────────────────────────┤
│  w3cos-dom (W3C DOM API)                                 │
│  Document · Element · Events · CSSStyleDeclaration       │
│  querySelector · classList · addEventListener             │
├──────────────────────────────────────────────────────────┤
│  w3cos-compiler: TS → Rust Transpiler                    │  ← AST transform
├──────────────────────────────────────────────────────────┤
│  w3cos-runtime                                           │
│  ┌──────────┐ ┌──────────┐ ┌───────────────────┐       │
│  │  Taffy    │ │  Vello   │ │  winit + wgpu     │       │
│  │ (CSS      │ │ (GPU 2D  │ │  (native window   │       │
│  │  layout)  │ │  render) │ │   + GPU surface)  │       │
│  │ Flex/Grid │ │ +skrifa  │ │                   │       │
│  │ Block/Pos │ │ +fontdue │ │  CPU fallback:    │       │
│  │           │ │          │ │  tiny-skia+softbuf│       │
│  └──────────┘ └──────────┘ └───────────────────┘       │
├──────────────────────────────────────────────────────────┤
│  rustc / LLVM (compilation to native machine code)       │
├──────────────────────────────────────────────────────────┤
│  Linux Kernel (Debian Minimal / Buildroot)                │
│  Drivers · Firmware · Process mgmt · Filesystem · Network │
└──────────────────────────────────────────────────────────┘
```

---

## 3. Key Design Decisions

### TS → Rust → Native (not interpreted)

We do NOT interpret TypeScript. We do NOT run a JavaScript engine.

1. **Parse** TypeScript (W3C Modern Subset) into AST
2. **Transpile** AST into Rust source code
3. **Compile** Rust → native ELF/Mach-O binary via rustc/LLVM
4. **Run** as a standard process — no runtime, no VM, no GC

### W3C DOM as the universal API

Every application uses the standard W3C DOM API (`document.createElement`, `element.style`, `addEventListener`). This means:

- Existing Web/React/Vue code can migrate with minimal changes
- AI agents read the DOM tree directly — no screenshot guessing
- Every UI element is structured, typed, and accessible

### AI-Native: DOM is the accessibility layer

Traditional OS: AI sees pixels → guesses what's on screen (slow, expensive, fragile).
W3C OS: AI reads the DOM tree → knows exactly what every element is (instant, free, precise).

Three access layers for AI agents:
- **Layer 1 — DOM Access**: Direct read/write of the DOM tree (< 1ms, zero tokens)
- **Layer 2 — Accessibility Tree**: ARIA-compliant structured summary (minimal tokens)
- **Layer 3 — Annotated Screenshot**: Visual capture with element markers (Claude Computer Use compatible)

### Linux kernel as the base (not custom kernel)

We use the stock Linux kernel for hardware support. The innovation is above the kernel:
- **Buildroot** for minimal bootable images (~50-100 MB ISO)
- **Debian Minimal** as an alternative base for maximum driver coverage
- Same drivers as Ubuntu/Debian — WiFi, GPU, Bluetooth, USB all work

---

## 4. Crate Structure

| Crate | Purpose | Lines |
|-------|---------|-------|
| `w3cos-std` | Type definitions: Component, Style, Color, Dimension (rem/em/vw/vh), BoxShadow, Transform2D, Transition, Easing | ~400 |
| `w3cos-dom` | W3C DOM API: Document, Element, Node tree, Events, CSSStyleDeclaration, querySelector | ~600 |
| `w3cos-a11y` | Accessibility tree: DOM → ARIA roles, flatten for AI consumption | ~250 |
| `w3cos-ai-bridge` | AI agent interface: DOM access + a11y API + screenshot + permissions (observer/interactive/system) | ~400 |
| `w3cos-compiler` | TS/JSON parser + Rust code generator. Handles TSX components + CSS-in-JS styles | ~500 |
| `w3cos-runtime` | Layout (Taffy 0.9) + rendering (Vello GPU / tiny-skia CPU fallback) + window (winit + wgpu) + mouse events + hover/click | ~900 |
| `w3cos-cli` | CLI tool: `w3cos build app.ts -o binary --release` | ~100 |

---

## 5. CSS Support

### Layout
- Flexbox (full) ✅
- CSS Grid (full) ✅
- Block layout ✅
- `position: relative / absolute` ✅
- `position: fixed / sticky` (planned)
- `z-index` ✅
- `overflow: hidden / scroll` ✅

### Units
- `px`, `%`, `rem`, `em`, `vw`, `vh`, `auto` ✅

### Visual
- `background`, `color`, `opacity` ✅
- `border-radius`, `border-width`, `border-color` ✅
- `box-shadow` (multi-layer blur) ✅
- `transform: translate / scale / rotate` ✅
- `transition` (parsing + easing functions) ✅

### Excluded (intentionally — legacy)
- `float` — use Flexbox/Grid instead
- `innerHTML` — XSS risk, not AOT-compatible
- `document.write`, `eval()` — not AOT-compatible

---

## 6. AI Agent Permission Model

| Level | Read DOM | Write DOM | Click | Files | Network | Processes |
|-------|----------|-----------|-------|-------|---------|-----------|
| `observer` | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ |
| `interactive` | ✅ | ✅ | ✅ | ❌ | ❌ | ❌ |
| `system` | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |

Additional controls: selector blacklist/whitelist, rate limiting, action audit log.

---

## 7. System Layer (OS Boot)

```
Power On → BIOS/UEFI → syslinux → Linux kernel → init
→ S99w3cos → /usr/bin/w3cos-shell (fullscreen)
→ SSH on port 22 (AI agent remote access)
```

Build options:
- **Buildroot**: ~50-100 MB minimal ISO, boots in seconds
- **Debian Minimal**: ~200 MB, maximum driver coverage
- **Docker**: Container mode for CI/CD and app compilation
- **DevContainer**: One-click dev environment for GitHub Codespaces

---

## 8. Application Compatibility (Roadmap)

| App Source | Strategy | Status |
|------------|----------|--------|
| W3C OS native (.ts) | Direct compilation | ✅ Working |
| Web/PWA | Same DOM API | Planned |
| React (web) | DOM API compatible — most code works | Planned |
| React Native | API mapping (View→Column, etc.) — 80%+ auto-migrate | Planned |
| Electron apps | AST transpilation: strip Chromium, map APIs | Planned |
| Linux native | Direct execution (it's Linux) | Inherent |
| Android APK | Waydroid container | Planned |
| Windows .exe | Wine compatibility layer | Planned |

---

## 9. Comparison

| | ChromeOS | Electron | React Native | **W3C OS** |
|---|---------|----------|-------------|-----------|
| Kernel | Linux | Host OS | Host OS | Linux |
| App Language | JS (interpreted) | JS (V8 JIT) | JS (Hermes) | **TS (AOT native)** |
| Binary Size | N/A | 90+ MB | 30+ MB | **2.4 MB** |
| Startup | 1-3s | 2-5s | 1-3s | **< 100ms** |
| RAM/App | 200+ MB | 200+ MB | 100+ MB | **~15 MB** |
| DOM API | Browser only | Browser only | None | **System-wide** |
| AI sees UI as | Pixels | Pixels | Pixels | **DOM tree** |
| AI access speed | 1-3s (vision) | 1-3s (vision) | 1-3s (vision) | **< 1ms** |
| Open Standard | Partial | No | No | **W3C** |
| Installable OS | Yes | No | No | **Yes (ISO)** |
