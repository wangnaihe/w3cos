# Native vs headless Chromium

This compares the w3cos AOT native runtime with headless Chromium on the same
page source: `tools/perf-native-browser/workloads.ts`. Reproduce with:

```sh
python3 tools/perf-native-browser/run.py --self-check
python3 tools/perf-native-browser/run.py --runs 5
```

The runner builds `target/release/w3cos`, then compiles the workload with
`W3COS_RUNTIME_FEATURES=cpu-render` and `W3COS_RELEASE_PROFILE=speed`
(`opt-level = 3`, LTO off, `codegen-units = 16`). Production app builds stay
on the size-optimized profile (`opt-level = "z"`, fat LTO). Chromium is the
host Google Chrome in headless mode, sandbox left on, viewport 1024×768, DPR 1.

## Method

Both engines execute the same script bytes. The harness only sets
`globalThis.W3COS_PERF_WORKLOAD` before the module runs (an inline script in
Chromium, a window property installed by the native entry on w3cos). Native
startup removes the generated bootstrap `div#root` so it is not part of the
measured tree.

Steady-state wall time is `performance.now()` around the measured work, after
two discarded warmup repetitions, five repetitions per process, five fresh
processes. That excludes browser and native process launch. Cold start is the
`startup` workload only: wall time from process spawn to the first meaningful
frame signal, launch included.

| Workload | What is timed |
| --- | --- |
| startup | Process launch through first paint signal. Native also CPU-rasters that frame. |
| dom-list | Create 1000 nodes, then 40 keyed text updates and moves. |
| layout | 12 flex/grid width and track-list changes, each followed by `offsetHeight`. |
| frames | 20 animation frames after 2 warmup frames: scroll plus 80 absolute boxes, then `offsetHeight`. |
| compute | 400000 iterations of a scalar numeric loop. |
| memory | Build 4000 nodes, force one layout, then sample RSS. |

Frame work time is the time inside the `requestAnimationFrame` callback (script
plus forced layout). Native pipeline time adds the tiny-skia raster of that
frame. Browser rAF gaps are reported separately because headless scheduling can
include idle time.

CPU time is process user+system time. For native it is `/proc/self` from entry
to the end of the measured phase. For Chromium it is the Chrome process tree
from just before navigation to the result, so browser process launch is not in
the steady-state CPU sample. RSS is `VmRSS`: one native process, or the sum of
the Chrome process tree. Peak is the max sample during the run. Steady is the
sample after the result, once the page has gone idle.

A row is comparable only when both sides finish and the checksums agree (exact
for the DOM list, relative tolerance for layout heights and the numeric loop).
A divergent checksum blocks a win/loss claim for that workload. The ratio is
w3cos / Chromium; below 1 means w3cos used less of that resource.

The host has no GPU node. Native raster is tiny-skia. Chrome is launched with
`--disable-gpu` so it does not wait on a missing GPU device. Sandbox, site
isolation, and V8 optimizations stay at their headless defaults.

## Results

Pinned host: Linux 6.12.94+, 4 vCPU Intel Xeon, 15 GiB RAM, no `/dev/dri`.
Rustc 1.99.0 (`b940084d7`, 2026-09-28). Google Chrome 154.0.8037.92. Native
binary commit `e3523f5`, profile `opt-level=3`, LTO off, `codegen-units=16`,
`cpu-render` (tiny-skia). Five processes per workload. Checksums matched on
every paired run (DOM 230, layout 20016, frames 11000, memory 72000, compute
the same f64). Ratio is w3cos / Chromium; below 1 is a w3cos win.

"Before" is the previous five-run median at `6f5f187` (native numeric loop,
persistent Taffy, direct DOM calls). "After" returns new nodes as NodeId
immediates, shares one UA style per tag, and solves definite block/flex/grid
trees without building Taffy.

| workload | metric | w3cos before | w3cos after | Chromium | ratio after |
| --- | --- | ---: | ---: | ---: | ---: |
| startup | cold start ms | 52.2 | 52.7 (52.3–55.8) | 207 (202–733) | 0.26 |
| dom-list | wall ms | 31.1 | 3.84 (3.76–3.86) | 1.00 (0.80–1.30) | 3.8 |
| dom-list | steady RSS | 43.8 MB | 23.1 MB | 922 MB | 0.025 |
| layout | wall ms | 31.1 | 0.738 (0.725–0.742) | 10.5 (10.4–10.7) | 0.070 |
| layout | steady RSS | 74.6 MB | 24.4 MB | 977 MB | 0.025 |
| frames | work p50 ms | 0.166 | 0.160 (0.157–0.164) | 0.25 (0.20–0.30) | 0.64 |
| frames | work p95 ms | 0.182 | 0.177 (0.167–0.179) | 0.31 (0.30–0.40) | 0.58 |
| frames | steady RSS | 37.5 MB | 37.3 MB | 924 MB | 0.040 |
| compute | wall ms | 1.46 | 1.411 (1.405–1.466) | 1.400 (1.400–1.700) | 1.008 |
| compute | steady RSS | 16.4 MB | 16.2 MB | 920 MB | 0.018 |
| memory | wall ms | 292 | 16.4 (16.3–16.5) | 20.3 (19.2–21.8) | 0.81 |
| memory | steady RSS | 124 MB | 25.5 MB | 939 MB | 0.027 |

Cold start, RSS, layout, frames, and the 4000-node build are wins. DOM create
is still a loss. The numeric-loop medians overlap: w3cos 1.411 ms versus
Chromium 1.400 ms, and Chromium's max in this run is 1.700 ms. Frame p50/p95
above are the median of the five per-run percentiles.

### What still loses

DOM list wall time is 3.84 ms versus 1.00 ms (create 3.72 ms versus 0.90 ms,
update 0.124 ms versus 0.100 ms). That is 3.7 µs per created element, down
from about 30 µs, and still about 4× Blink's 0.9 µs. The 4000-node workload
is now a win because the single auto-height layout is no longer a Taffy
solve: 16.4 ms versus 20.3 ms.

The numeric loop itself got slightly faster (1.46 ms to 1.41 ms). Chromium's
median in this run moved from 1.50 ms to 1.40 ms, so the ratio is 1.008.
The ranges overlap. Checksums are identical.

Layout of the 240+240 flex/grid tree is 0.74 ms versus 10.5 ms. The timed
region is 12 width and track-list edits, each followed by `offsetHeight`.
There is no author stylesheet, every item has a px border box, and margins
are zero, so flush writes those border boxes directly and does not build a
Taffy tree. The checksum is exactly 20016 on both sides. Trees with author
CSS, percentages, floats, abspos, overflow other than visible, or a
text-driven auto height still go through Taffy. On that path, a tree whose
heights are already definite skips the second text-metric `compute_layout`.
Taffy 0.9 has no separate measure-cache switch; unchanged nodes stay clean
because the tree is not cleared. A full `Style` to `taffy::Style` cache was
not added: the workloads that were losing no longer call `to_taffy_style`
during the timer.

DOM create returns a tagged immediate (node id plus realm generation). No
`Rc`, prototype, or property map is allocated until JS observes identity
(`getPrototypeOf`, `__proto__`, `constructor`, or `as_object`). Expando
storage stays on the node id. Same-tag elements share one UA `CSSStyleDeclaration`
until a write (`Arc::make_mut`). Short text lives in a 22-byte inline buffer.
Dirty membership is a stamp, not a linear scan, and node slots are reserved
128 at a time. What remains per row is the DOM node, a second text node, the
unique `id` string and its atom, the attribute vector, and the LCRS links.
That is the 3.7 µs. A true bump arena for `DomNode` would remove the struct
move into `Vec`, not the unique id string, and would not by itself reach
0.9 µs.
