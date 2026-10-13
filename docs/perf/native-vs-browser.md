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
binary commit `6f5f187`, profile `opt-level=3`, LTO off, `codegen-units=16`,
`cpu-render` (tiny-skia). Five processes per workload. Checksums matched on
every paired run. Ratio is w3cos / Chromium; below 1 is a w3cos win.

"Before" is the previous five-run median at `ce74c90` (sibling restyle skip,
inline declaration replacement, rect cache). "After" unboxes numeric locals
into native loops, flushes layout through a persistent Taffy tree, and calls
create/append/style directly from AOT.

| workload | metric | w3cos before | w3cos after | Chromium | ratio after |
| --- | --- | ---: | ---: | ---: | ---: |
| startup | cold start ms | 52.0 | 52.2 (50.9–54.7) | 228 (212–249) | 0.23 |
| dom-list | wall ms | 25.3 | 31.1 (30.8–31.9) | 0.80 (0.70–1.20) | 39 |
| dom-list | steady RSS | 49.8 MB | 43.8 MB | 921 MB | 0.048 |
| layout | wall ms | 306 | 31.1 (30.5–31.8) | 11.2 (11.1–12.2) | 2.8 |
| layout | steady RSS | 79.3 MB | 74.6 MB | 976 MB | 0.076 |
| frames | work p50 ms | 5.63 | 0.166 (0.163–0.177) | 0.20 (0.20–0.30) | 0.83 |
| frames | work p95 ms | 6.06 | 0.182 (0.180–0.218) | 0.31 (0.30–0.40) | 0.59 |
| frames | steady RSS | 37.6 MB | 37.5 MB | 927 MB | 0.041 |
| compute | wall ms | 191 | 1.46 (1.41–1.55) | 1.50 (1.40–1.50) | 0.98 |
| compute | steady RSS | 16.6 MB | 16.4 MB | 919 MB | 0.018 |
| memory | wall ms | 358 | 292 (286–306) | 22.9 (21.2–34.8) | 13 |
| memory | steady RSS | 133 MB | 124 MB | 941 MB | 0.13 |

Cold start, RSS, the numeric loop, and frame work are wins. DOM update time
is 0.61 ms versus a Chromium median of 0.0 ms (max 0.1 ms). Frame p50/p95
above are the median of the five per-run percentiles. The earlier pass
(46.7 ms DOM, 955 ms layout, 194 ms compute) is the "before" column of the
previous revision of this table.

### What still loses

DOM creation (31 ms vs 0.8 ms) and the 4000-node build (292 ms vs 23 ms) are
still losses. Layout is 31 ms vs 11 ms, about 2.8×, down from 26×. Checksums
match.

The 400000-iteration loop now lives in `f64` slots inside a Rust `loop`.
Median 1.46 ms versus V8 1.50 ms (about 3.7 ns per dependent iteration).

Frame work reads `offsetHeight` on boxes with a definite px height. Those
reads return the specified border box when no dirty ancestor can move them, so
the 80 `style.top` writes no longer rebuild the tree. p50 is 0.166 ms versus
Chromium 0.20 ms.

Layout keeps one component tree and one Taffy tree. A width or
`grid-template-columns` edit patches those nodes and calls `compute_layout`
again. The first timed `offsetHeight` still builds the ~500-node tree (the
dominant share of the 31 ms). The next eleven reflows are incremental. Taffy
on this tree is still slower than Blink's flex/grid: 12 reflows cost 31 ms
here and 11 ms in Chrome. Skipping the post-Taffy projection passes is limited
to trees whose heights do not depend on text measurement, floats, or tables,
and the checksum stayed within the harness tolerance.

DOM create is a direct `dom_create_element` / `dom_set_data` / `dom_append_child`
call, and the update loop is 0.61 ms. Creating 1000 elements plus their text
nodes is still ~30 µs each: a DOM arena node, a UA style declaration, a
collectable JS wrapper (host `Rc`, because arena handles are not refcounted
and the weak wrapper cache must observe the drop), prototype install, and
insertion invalidation. Blink does the same work at ~0.4 µs/node. At 4000
nodes that construction, plus one real layout of the auto-height root, is the
292 ms memory result. Closing it means a bump allocator for repeated `div`
nodes and a wrapper that does not allocate a property map per element, without
giving up the weak-cache drop.
