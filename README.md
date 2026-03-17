# W3C OS

**An AI-native operating system built on W3C standards. TypeScript + DOM compiled to native binaries. No browser. No V8.**

![W3C OS Demo](docs/screenshot.png)

```
app.ts  →  w3cos build  →  native binary (2.4 MB)
```

## What is this?

W3C OS is a Linux-based operating system where:
- Applications use **standard W3C DOM + CSS** (the same APIs as the Web)
- TypeScript is compiled to **native machine code** via Rust/LLVM (not interpreted)
- AI agents can **read and operate every UI element** directly through the DOM — no screenshot guessing
- The system boots from a **minimal Linux kernel** directly into the W3C OS Shell

Write Web-standard code. Get native performance. Give AI full visibility.

## Quick Start

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Clone and build
git clone https://github.com/anthropic-ai/w3cos.git
cd w3cos
cargo build --release

# Compile a TypeScript app to a native binary
./target/release/w3cos build examples/showcase/app.ts -o showcase --release
./showcase    # Opens a native window — no browser involved
```

## Example

```typescript
// app.ts — a native application written in TypeScript
import { Column, Text, Button } from "@w3cos/std"

export default Column({
  style: { gap: 20, padding: 48, alignItems: "center", background: "#0f0f1a" },
  children: [
    Text("W3C OS", { style: { fontSize: 42, color: "#e94560" } }),
    Text("Native binary from TypeScript.", { style: { fontSize: 20, color: "#a0a0b0" } }),
    Button("Get Started", { style: { background: "#e94560", borderRadius: 8 } }),
  ]
})
```

```bash
$ w3cos build app.ts -o myapp --release
⚡ Transpiling TS → Rust... done
🔨 Compiling native binary... done
✅ Output: ./myapp (2.4 MB)
```

## Why?

| | Electron | React Native | Flutter | **W3C OS** |
|---|---------|-------------|---------|-----------|
| Binary Size | 90+ MB | 30+ MB | 15+ MB | **2.4 MB** |
| RAM Usage | 200+ MB | 100+ MB | 80+ MB | **~15 MB** |
| Startup | 2-5 sec | 1-3 sec | 0.5-2 sec | **< 100ms** |
| Language | JS (V8 JIT) | JS (Hermes) | Dart (AOT) | **TS (native AOT)** |
| Runtime | Chromium | Bridge + Native | Dart VM | **None** |
| DOM API | ✅ (browser only) | ❌ | ❌ | **✅ (system-wide)** |
| AI reads UI | Screenshot | Screenshot | Screenshot | **DOM tree (< 1ms)** |
| Standard | Proprietary | Proprietary | Proprietary | **W3C** |
| Installable OS | ❌ | ❌ | ❌ | **✅** |

## AI-Native: Why This Matters

Traditional operating systems are **opaque to AI** — an AI agent must take screenshots and guess what's on screen (slow, expensive, fragile).

W3C OS applications are built with the DOM. AI agents **read the DOM tree directly**:

```
Traditional OS:  AI sees pixels → vision model → guess UI → click coordinates (1-3 sec, $$$)
W3C OS:          AI reads DOM  → structured tree → precise action (< 1ms, free)
```

Three access levels for AI agents:
- **Layer 1 — DOM Access**: Read/write any element, trigger events. 100% precise. < 1ms.
- **Layer 2 — Accessibility Tree**: ARIA-compliant summary. Minimal tokens for LLMs.
- **Layer 3 — Annotated Screenshot**: For Claude Computer Use / UI-TARS compatibility.

## Install as an OS

W3C OS can boot as a standalone operating system — directly into the W3C OS Shell.

### Build the bootable ISO

```bash
# Prerequisites (Linux): build-essential, ncurses-dev, wget, python3
# On macOS, the script will automatically use Docker for cross-compilation.

./system/scripts/build-iso.sh    # Output: w3cos.iso (~50-100 MB)
```

### Run

```bash
# Option 1: QEMU virtual machine
qemu-system-x86_64 -cdrom w3cos.iso -m 2G -vga virtio

# Option 2: Flash to USB and boot real hardware
sudo dd if=w3cos.iso of=/dev/sdX bs=4M status=progress

# Option 3: Docker (compile apps, no GUI)
docker build -t w3cos . && docker run w3cos --help

# Option 4: GitHub Codespaces (one-click dev environment)
# Click "Open in Codespaces" on the GitHub repo page
```

See [system/INSTALL.md](system/INSTALL.md) for the full installation guide.

## How It Works

```
TypeScript (W3C DOM + CSS)            ← You write this
        ↓  w3cos-compiler
Rust source code (auto-generated)     ← AST transform
        ↓  rustc + LLVM
Native ELF/Mach-O binary              ← Machine code
        ↓  Linux kernel
Runs directly on hardware             ← No runtime
```

### Technology Stack

| Layer | Technology | What it does |
|-------|-----------|-------------|
| CSS Layout | [Taffy](https://github.com/DioxusLabs/taffy) 0.9 | Flexbox, Grid, Block, position |
| Text Layout | [Parley](https://github.com/linebender/parley) | Line-breaking, shaping, bidi |
| 2D Rendering | [tiny-skia](https://github.com/nickel-org/tiny-skia) → [Vello](https://github.com/linebender/vello) (Phase 2) | Vector graphics |
| Windowing | [winit](https://github.com/rust-windowing/winit) | Cross-platform native windows |
| OS Base | Linux kernel (Debian Minimal / Buildroot) | Drivers, processes, filesystem |

## CSS Support

| Feature | Status |
|---------|--------|
| Flexbox / Grid | ✅ Full |
| Block layout | ✅ |
| `position: relative / absolute` | ✅ |
| `position: fixed / sticky` | 🔜 |
| `overflow: hidden / scroll` | ✅ |
| `z-index` | ✅ |
| Units: `px, %, rem, em, vw, vh` | ✅ |
| `border-radius`, `opacity` | ✅ |
| `box-shadow` | ✅ |
| `transform: translate / scale / rotate` | ✅ |
| `transition` (easing functions) | ✅ |
| `display: inline / inline-block` | 🔜 |
| Mouse events (hover, click) | ✅ |

## Project Structure

```
w3cos/
├── crates/
│   ├── w3cos-std/         # Type definitions (Style, Component, Color)
│   ├── w3cos-dom/         # W3C DOM API (Document, Element, Events)
│   ├── w3cos-a11y/        # Accessibility tree (ARIA, for AI + screen readers)
│   ├── w3cos-ai-bridge/   # AI agent interface (3-layer access + permissions)
│   ├── w3cos-compiler/    # TS → Rust transpiler
│   ├── w3cos-runtime/     # Layout + Rendering + Window + Events
│   └── w3cos-cli/         # CLI: w3cos build / w3cos run
├── system/
│   ├── buildroot/         # Bootable ISO config
│   ├── rootfs_overlay/    # System init scripts
│   ├── scripts/           # build-iso.sh, run-qemu.sh
│   └── INSTALL.md         # Installation guide
├── examples/              # 4 example applications
├── .devcontainer/         # One-click dev environment
├── Dockerfile             # Container build
├── ARCHITECTURE.md        # Full architecture document
├── ROADMAP.md             # Phased development plan
└── CONTRIBUTING.md        # How to contribute (AI + humans)
```

## Contributing

This project is designed for **AI-assisted development**. See [CONTRIBUTING.md](CONTRIBUTING.md).

- **Humans**: File Issues, architecture decisions, security review
- **AI agents**: Pick up `ai-ready` Issues, submit PRs, write tests
- No AI compute or API keys provided — bring your own

## License

Apache 2.0 — open, neutral, not controlled by any single corporation.
