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
binary commit `ce74c90`, profile `opt-level=3`, LTO off, `codegen-units=16`,
`cpu-render` (tiny-skia). Five processes per workload. Checksums matched on
every paired run. Ratio is w3cos / Chromium after optimization; below 1 is a
w3cos win.

"Before" is the first five-run pass, already including the sibling-context
cache, inline-declaration replacement, and forced-layout rect cache. The
single-process smoke before those three changes is called out underneath,
because layout and memory had not been through five runs yet. "After" adds
allocation-free CSS property compares and skipping sibling restyles when no
structural selector is registered.

| workload | metric | w3cos before | w3cos after | Chromium | ratio after |
| --- | --- | ---: | ---: | ---: | ---: |
| startup | cold start ms | 62.5 | 52.0 (50.8–53.0) | 210 (196–240) | 0.25 |
| startup | steady RSS | — | see other rows | ~940 MB tree | — |
| dom-list | wall ms | 46.7 | 25.3 (25.2–25.8) | 0.90 (0.70–1.50) | 28 |
| dom-list | steady RSS | 50.0 MB | 49.8 MB | 941 MB | 0.053 |
| layout | wall ms | 955 | 306 (304–310) | 11.7 (10.8–14.2) | 26 |
| layout | steady RSS | 79.0 MB | 79.3 MB | 1000 MB | 0.079 |
| frames | work p50 ms | 40.7 | 5.63 (5.54–5.74) | 0.20 (0.20–0.30) | 28 |
| frames | work p95 ms | 43.8 | 6.06 (5.72–6.58) | 0.30 (0.30–0.40) | 20 |
| frames | steady RSS | 37.5 MB | 37.6 MB | 947 MB | 0.040 |
| compute | wall ms | 194 | 191 (188–195) | 1.40 (1.40–1.50) | 136 |
| compute | steady RSS | 16.8 MB | 16.6 MB | 941 MB | 0.018 |
| memory | wall ms | 1023 | 358 (337–383) | 21.8 (19.7–25.6) | 16 |
| memory | steady RSS | 133 MB | 133 MB | 961 MB | 0.14 |

Cold start and every RSS sample are wins. DOM update time is 0.47 ms versus a
Chromium median of 0.0 ms (max 0.1 ms), so the ratio is undefined and w3cos is
slower. Frame p50/p95 above are the median of the five per-run percentiles.

Before the first three optimizations, one native process measured layout at
about 2.7 s per sample, frame work growing from 70 ms to 130 ms, and the 4000
node memory workload still inside `to_component_tree` after 90 s. Compute was
already about 200 ms. Startup's first process spent 421 ms in the first
tiny-skia raster; the five-run medians above are after font caches are warm.

### What still loses

DOM creation (25 ms vs 0.9 ms), layout (306 ms vs 12 ms), frame work (5.6 ms
vs 0.2 ms), the memory build (358 ms vs 22 ms), and the numeric loop (191 ms
vs 1.4 ms) are still losses. Checksums match, so this is not skipped work.

Layout and frames are the same cost. `offsetHeight` still rebuilds a component
tree after every style write. Samples of that rebuild land in `computed_style`,
`reorder_explicit_bidi_*` cloning `Component` trees, and
`layout::compute_with_scroll_details` cloning the tree again before Taffy.
Caching the rects removes the second read in a frame, which is why layout fell
from 2.7 s to 306 ms and frame work from ~100 ms to 5.6 ms. It cannot remove
the flush that the style write itself requires. Closing the rest of the gap
needs an incremental style and layout tree; a full rebuild of a few hundred
boxes is still about 25 ms on this host, and Blink does the same reflow in
about 1 ms.

DOM creation fell from 47 ms to 25 ms by not restyling every sibling on append
when the stylesheet has no structural selector. The remaining ~25 µs per node
is host-object construction through boxed values (element, text node, style
declaration). Memory is that cost at 4000 nodes plus one full layout.

Compute did not move. The 400000-iteration loop lowers to environment bindings
(`RefCell<(Value, bool)>`) and a block-dispatch state machine. Straight-line
`1 + 2 + 3` already uses `num_regs`, but `LoadBinding` is typed `Unknown`, and
a loop is more than one block so it cannot use the direct-body emitter. At
191 ms that is about 480 ns per iteration. V8 measures 3.5 ns. Unboxing the
bindings would remove the `RefCell`, but the state machine would still sit far
above a native `for` loop, which is what would be required to go under 1.4 ms.
