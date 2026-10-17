# W3C OS

[CI](https://github.com/wangnaihe/w3cos/actions/workflows/ci.yml)
[Build ISO](https://github.com/wangnaihe/w3cos/actions/workflows/build-iso.yml)
[License](LICENSE)
[Rust](https://www.rust-lang.org)

**An AI-native operating system and application runtime built on W3C standards. TypeScript + DOM run as native code, without embedding Chromium or V8.**

![Desktop home: icons, the intent bar, and L2 off on the taskbar](docs/screenshot.png)

![Phone home of the same shell](docs/screenshot-phone.png)

```
app.ts  →  w3cos build  →  native binary (2.4 MB)
```

## What is this?

W3C OS is a native Web application platform and Linux-based operating system where:

- Applications use **standard W3C DOM + CSS** (the same APIs as the Web)
- TypeScript can be compiled **ahead of time to native machine code** via
  Rust/LLVM, while dynamic browser-style documents use the shared W3IR/W3VM
  semantic core
- AI agents can **read and operate every UI element** directly through the DOM — no screenshot guessing
- The system boots from a **minimal Linux kernel** directly into the W3C OS Shell

Write Web-standard code. Get native performance. Give AI full visibility.

Implementation status is tracked per engine and target in the
[Web API capability matrix](WEB_API_CAPABILITIES.md); planned work and exit
gates live in the [roadmap](ROADMAP.md).

## Current milestone

The current `main` milestone is **native Web UI semantic parity**. The latest
baseline expands the real DOM/CSSOM path, stylesheet matching, Flexbox/Grid
layout, Fetch response streams, object-URL images, SVG rendering, native
hit-testing and form-control behavior. Mobile generation now covers Android,
iOS, and HarmonyOS, with incremental generated modules and an iOS native text
and file-input bridge.

This is active implementation, not a browser-conformance claim. Physical-device
lifecycle, complete mobile gesture arbitration, host-backed device APIs, and
the formal downstream application gates remain open. See the
[capability matrix](WEB_API_CAPABILITIES.md) for exact per-target boundaries.

## Quick Start

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Clone and build
git clone https://github.com/wangnaihe/w3cos.git
cd w3cos
cargo build --release

# Compile a TypeScript app to a native binary
./target/release/w3cos build examples/showcase/app.tsx -o showcase --release
./showcase    # Opens a native window — no browser involved
```

## AI Bridge

The shell can listen on loopback for an agent. Permissions come from
`W3COS_AI_PERMISSIONS` (`observer` when unset). The bearer token is
`W3COS_AI_TOKEN` or `--ai-token`.

```bash
cargo build -p w3cos-shell --no-default-features --features cpu-render,ai-bridge
W3COS_AI_TOKEN=demo-token ./target/debug/w3cos-shell --ai-port 9222 --ai-token demo-token

# stdio MCP adapter. It only forwards to that listener.
W3COS_AI_PORT=9222 W3COS_AI_TOKEN=demo-token cargo run -p w3cos-ai-bridge --bin w3cos-ai-mcp
```

The shell intent bar uses the L0 rule table first. Phrases that miss it can
fall through to a local L1 synonym matcher (no model, no network).
`W3COS_L1=0` keeps the rule table only. An L1 hit shows an `L1 本地 Local`
badge on the confirm card. The task center starts locked; the orange
`解锁才能执行` button unlocks the local stub.

Free-form text can go to an L2 planner when `W3COS_L2=1`. That planner is off
by default and runs on a background thread. It calls the same MCP tools the
bridge already exposes. Mutating steps still wait for confirm, the capability
gate, and a receipt. See [docs/AI_HARNESS.md](docs/AI_HARNESS.md). Model
weights are not in the repo:

```bash
./scripts/fetch-l2-models.sh
./scripts/w3cos-inferd.sh llama-1.5b
export W3COS_L2=1 W3COS_L2_BASE_URL=http://127.0.0.1:8081/v1 W3COS_L2_MODEL=qwen2.5-1.5b-instruct
```

### Xvfb demo (Codespaces or any Linux VM)

The shell is a native window, not a browser page. Rust 1.85+ is required
(`edition = "2024"`). In a Codespace:

```bash
sudo apt-get update
sudo apt-get install -y xvfb xdotool libfontconfig1-dev libxkbcommon-dev libxkbcommon-x11-0 libwayland-dev fonts-droid-fallback libclang-dev cmake g++ libasound2-dev
cargo build -p w3cos-shell --no-default-features --features cpu-render,ai-bridge
Xvfb :99 -screen 0 1280x800x24 -ac &
export DISPLAY=:99 WINIT_UNIX_BACKEND=x11
mkdir -p /tmp/w3cos-receipts
W3COS_RECEIPT_DIR=/tmp/w3cos-receipts ./target/debug/w3cos-shell --ai-port 9222 --ai-token demo-token
```

Type `file manager` and press Propose. That phrase misses the rule table, so
the confirm card is the L1 badge. Leave `W3COS_AI_PERMISSIONS` unset; the HTTP
listener stays `observer`.

`a11y_index` and `screenshot_annotated` are readable by an observer.
`propose_intent`, `confirm_intent`, `click`, and `type` use the bridge's
interaction permission, so an observer token is rejected before the shell runs
the command.

## Mobile (Android / iOS / HarmonyOS)

Native **shell + AOT app** — generic platform, no product-specific examples.

```bash
# Desktop dev (same TSX pipeline)
w3cos build examples/mobile-demo/app.tsx -o mobile-demo --release

# Scaffold all platform shells
w3cos mobile init MyApp --platform all
```

See [docs/MOBILE.md](docs/MOBILE.md).

## Example

### TSX + npm ESM

```tsx
import { useState } from "react"

function App() {
  const [count, setCount] = useState(0)
  return (
    <div style={{ gap: 20, padding: 48, alignItems: "center", backgroundColor: "#0f0f1a" }}>
      <span style={{ fontSize: 42, color: "#e94560" }}>W3C OS</span>
      <span style={{ fontSize: 20, color: "#a0a0b0" }}>Native TSX, compiled AOT.</span>
      <button onClick={() => setCount(count + 1)}>Count: {count}</button>
    </div>
  )
}

export function main() {
  return <App />
}
```

The compiler lowers standard JSX and npm ESM dependencies directly to Rust. Hooks are ordinary package imports; there is no framework-specific runtime path.

```bash
$ w3cos build app.tsx -o myapp --release
⚡ Transpiling TS → Rust... done
🔨 Compiling native binary... done
✅ Output: ./myapp (2.4 MB)
```

## Why?


|                | Electron         | React Native    | Flutter     | **W3C OS**           |
| -------------- | ---------------- | --------------- | ----------- | -------------------- |
| Binary Size    | 90+ MB           | 30+ MB          | 15+ MB      | **2.4 MB**           |
| RAM Usage      | 200+ MB          | 100+ MB         | 80+ MB      | **~15 MB**           |
| Startup        | 2-5 sec          | 1-3 sec         | 0.5-2 sec   | **< 100ms**          |
| Language       | JS (V8 JIT)      | JS (Hermes)     | Dart (AOT)  | **TS (native AOT)**  |
| Runtime        | Chromium         | Bridge + Native | Dart VM     | **Native Rust (AOT/W3VM)** |
| DOM API        | ✅ (browser only) | ❌               | ❌           | **✅ (system-wide)**  |
| AI reads UI    | Screenshot       | Screenshot      | Screenshot  | **DOM tree (< 1ms)** |
| Standard       | Proprietary      | Proprietary     | Proprietary | **W3C**              |
| Installable OS | ❌                | ❌               | ❌           | **✅**                |


## Memory model: no GC, reference counting + WeakRef

> **w3cos has no garbage collector.** Heap values are reference-counted and freed deterministically. A reference cycle is never collected, so **cycles must be broken with `WeakRef` / `WeakMap` / `WeakSet` / `FinalizationRegistry`.**

**Why:** with no GC pauses, latency and memory stay predictable (no collector stalls while typing), and the runtime stays close to native Rust (`Rc`/`Weak`) behavior and cost.

**What you must do:** make back-references weak and dispose what you subscribe to.
- Back-references should be weak: child → parent, listener → emitter, cache → value, DOM node → JS wrapper.
- A closure that captures `this` and is registered on a longer-lived object should hold `this` through a `WeakRef`, or be removed in `dispose()`.
- Pair every `addEventListener` / `onDidX` subscription with a removal (`Disposable` / `DisposableStore` pattern).

```ts
class Child {
  #parent: WeakRef<Parent>;               // back-edge is weak
  constructor(parent: Parent) { this.#parent = new WeakRef(parent); }
  get parent() { return this.#parent.deref(); }
}

class View {
  #off: () => void;
  constructor(emitter: Emitter) {
    const self = new WeakRef(this);       // listener does not own the view
    const onChange = () => self.deref()?.render();
    emitter.on("change", onChange);
    this.#off = () => emitter.off("change", onChange);
  }
  render() {}
  dispose() { this.#off(); }               // explicit teardown
}

const meta = new WeakMap<object, Info>(); // side tables keyed by object: WeakMap
```

**What the compiler does for you** (`crates/w3cos-compiler/src/unique_back_edges.rs`). AOT automatically rewrites three unambiguous closure back-edges to `WeakRef`:
1. self-install: `obj.fn = () => obj.x`, `this.fn = () => this.x`
2. promise reactions that capture their own promise binding: `p.then(() => p...)`
3. iterator-on-collection: `coll.iter = { next: () => coll.pop() }`

For ambiguous cases it **only warns** and never rewrites, because guessing could free live data. These cases are mutual properties (`a.b = b; b.a = a`), computed or dynamic keys (`obj[k] = ...`), and cross-file cycles.

**FinalizationRegistry is deterministic.** The cleanup callback runs at the first job checkpoint after the target's last strong reference is dropped, once the microtask queue is empty. It never runs inside `Drop` or during the job that released the target. `WeakMap` and `WeakSet` are O(1), and an entry is removed (and its value released) when its key dies. See [`crates/w3cos-core/src/weak.rs`](crates/w3cos-core/src/weak.rs) for ordering details.

> **Current limitation:** ordinary AOT object literals, arrays, and closures are allocated in a page arena and freed on page reset, not at their last reference. Making arena values reference-counted too is roadmap item M1-4a (planned). Host values (DOM wrappers, native objects) are already freed immediately.

**Tooling (planned):**
- A cycle detector for debug builds only, which reports the full retaining chain with source locations. It is not compiled into release builds.
- `w3cos leak-lint` with codemods for `this`-capturing closures, listeners that are never removed, parent ↔ child strong references, and un-disposed `Disposable`s.
- Leak CI: per-scenario heap diffs plus a nightly 8-hour soak test.

**Upstream policy:** when a cycle leaks in upstream code (e.g. VS Code, extensions, npm packages), we fork it and fix it with `WeakRef` or an explicit `dispose`, keeping the fix in a patch queue. Fixes that make sense upstream are sent upstream as PRs. Details are in [`docs/roadmap/aot-port.md`](docs/roadmap/aot-port.md) §3.2 and M1.

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
TypeScript / JavaScript / TSX + DOM + CSS
        ├─ AOT: SWC → generated Rust → rustc/LLVM → native binary
        └─ Dynamic document: SWC → W3IR → W3VM
                         ↓
          Shared Value / Promise / DOM / Web API semantics
                         ↓
       Native layout, rendering, input, and platform hosts
```

### Generate Web API skeletons

Generate reviewable Rust facade scaffolding from a TypeScript declaration file:

```bash
w3cos web-api-skeleton path/to/lib.dom.d.ts \
  --interface Event \
  --interface Navigator \
  --output web_api_stubs.rs
```

Inherited interface members are included by default; pass `--own-only` to emit
only directly declared members. Generated constructors, methods, getters, and
setters contain named `todo!()` placeholders and are not wired into the runtime
automatically.

### Audit the live Chromium Web API surface

Compare the Web API globals and constructor prototypes exposed by a locally
installed Chrome/Chromium with the current w3cos `window`:

```bash
w3cos web-api-audit
w3cos web-api-audit --json
w3cos web-api-audit --fail-on-missing
```

Use `--chrome /path/to/chrome` when auto-detection is unavailable. The audit
filters ECMAScript built-ins and event-handler properties, but intentionally
keeps experimental browser interfaces visible so roadmap decisions do not
mistake an omitted subsystem for conformance.

### Run raw Web Platform Tests

Run a pinned, unmodified WPT smoke subset through the dynamic document and
native rendering paths:

```bash
cargo run -p w3cos-wpt-runner -- \
  --wpt-root ../wpt \
  --suite tests/wpt/w3cos-smoke.json
```

Checkout preparation, the broader fail-closed baseline, artifacts, and current
runner boundaries are documented in
[Raw Web Platform Tests](docs/wpt-conformance.md).

### Technology Stack


| Layer        | Technology                                                                                                    | What it does                   |
| ------------ | ------------------------------------------------------------------------------------------------------------- | ------------------------------ |
| CSS Layout   | [Taffy](https://github.com/DioxusLabs/taffy) 0.9                                                              | Flexbox, Grid, Block, position |
| Text Layout  | [Parley](https://github.com/linebender/parley)                                                                | Line-breaking, shaping, bidi   |
| 2D Rendering | Skia (primary), Vello/tiny-skia compatibility backends                                                  | Raster and vector graphics     |
| Windowing    | [winit](https://github.com/rust-windowing/winit)                                                              | Cross-platform native windows  |
| OS Base      | Linux kernel (Debian Minimal / Buildroot)                                                                     | Drivers, processes, filesystem |


## CSS support snapshot

The table below is a high-level native rendering snapshot. It does not replace
the per-surface and per-target qualifications in
[WEB_API_CAPABILITIES.md](WEB_API_CAPABILITIES.md).


| Feature                                 | Status |
| --------------------------------------- | ------ |
| Flexbox / Grid                          | ✅ Full |
| Block layout                            | ✅      |
| `position: relative / absolute`         | ✅      |
| `position: fixed / sticky`              | ✅      |
| `overflow: hidden / scroll`             | ✅      |
| `z-index`                               | ✅      |
| Units: `px, %, rem, em, vw, vh`         | ✅      |
| `border-radius`, `opacity`              | ✅      |
| `box-shadow`                            | ✅      |
| `transform: translate / scale / rotate` | ✅      |
| `transition` (easing functions)         | ✅      |
| `@keyframes` animation                  | ✅      |
| `display: inline / inline-block`        | ✅      |
| `@layer` cascade layers                 | ✅      |
| `@media` queries                        | ✅      |
| Container Queries                       | ✅      |
| Pseudo-classes (`:hover`, `:focus`, `:nth-child`, etc.) | ✅ |
| Attribute selectors (`[attr=value]`)    | ✅      |
| CSS Custom Properties (`var(--x)`)      | ✅      |
| Mouse events (hover, click)             | ✅      |


## Project Structure

```
w3cos/
├── crates/
│   ├── w3cos-core/        # JS-compatible Value type, reactive system, Proxy
│   ├── w3cos-ir/          # Versioned shared JavaScript IR
│   ├── w3cos-vm/          # Dynamic W3IR execution backend
│   ├── w3cos-std/         # Type definitions (Style, Component, Color)
│   ├── w3cos-dom/         # W3C DOM API (Document, Element, Events)
│   ├── w3cos-a11y/        # Accessibility tree (ARIA, for AI + screen readers)
│   ├── w3cos-ai-bridge/   # AI agent interface (3-layer access + permissions)
│   ├── w3cos-compiler/    # TS → Rust transpiler (SWC parser + CSS/SCSS)
│   ├── w3cos-runtime/     # Layout + Rendering + Window + System APIs
│   ├── w3cos-cli/         # CLI: w3cos build / run / dev / init / mobile
│   ├── w3cos-mobile/      # Mobile host contracts and platform bridges
│   ├── w3cos-ffi/         # Native library and C ABI bridge
│   ├── w3cos-shell/       # System-level desktop shell binary
│   ├── w3cos-demo/        # Showcase demo binary
│   └── w3cos-rn-compat/   # React Native API compatibility layer
├── templates/
│   ├── android/           # Gradle + NativeActivity shell
│   ├── ios/               # Xcode/UIKit shell
│   ├── harmony/           # ArkUI/XComponent shell scaffold
│   └── shared/            # app.tsx + w3cos.app.json starter
├── docs/MOBILE.md         # Mobile build guide
├── system/
│   ├── buildroot/         # Bootable ISO config
│   ├── rootfs_overlay/    # System init scripts
│   ├── scripts/           # build-iso.sh, run-qemu.sh
│   └── INSTALL.md         # Installation guide
├── examples/              # 18+ example applications
├── .openclaw/             # OpenClaw + Lobster AI workflow configs
├── .devcontainer/         # One-click dev environment
├── Dockerfile             # Container build
├── ARCHITECTURE.md        # Full architecture document
├── AI_DEVELOPMENT.md      # AI-driven development model
├── ROADMAP.md             # Phased development plan
└── CONTRIBUTING.md        # How to contribute (AI + humans)
```

## AI-Driven Development

W3C OS is built by AI agents, directed by humans.

```
Humans file Issues  →  Management AI triages  →  Contributor AI codes  →  Human approves
```

- **Humans**: File Issues, review PRs, make architecture decisions, sponsor tokens
- **AI (Management)**: Triage issues, review PRs, run CI, manage releases
- **AI (Contributor)**: Pick up `ai-ready` issues, implement features, write tests, submit PRs

AI tokens are funded by community sponsors. Every dollar goes to AI compute.

See [AI_DEVELOPMENT.md](AI_DEVELOPMENT.md) for the full model, and [CONTRIBUTING.md](CONTRIBUTING.md) to get involved.

## Sponsor

AI agents need tokens. Your sponsorship keeps development moving.

[Sponsor](https://github.com/sponsors/wangnaihe)


| Tier     | Amount  | Impact                            |
| -------- | ------- | --------------------------------- |
| Byte     | $5/mo   | ~1 AI-implemented issue/month     |
| Kilobyte | $25/mo  | ~5 AI-implemented issues/month    |
| Megabyte | $100/mo | ~20 AI-implemented issues/month   |
| Gigabyte | $500/mo | Sustained AI development capacity |


100% goes to AI compute. No human salaries. Fully transparent.

## License

Apache 2.0 — open, neutral, not controlled by any single corporation.
