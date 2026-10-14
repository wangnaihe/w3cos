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
| layout | 12 flex/grid width and track-list changes, each followed by `offsetHeight`. No author stylesheet; every item has a px border box. |
| layout-css | Author stylesheet, percentage widths, and text-driven auto heights. Six width changes on one wrap, each followed by `offsetHeight`. Simple author blocks are solved directly; flex, grid, floats, and positioned boxes stay on Taffy. |
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
binary commit `6989059`, profile `opt-level=3`, LTO off, `codegen-units=16`,
`cpu-render` (tiny-skia). Five processes per workload. Checksums matched on
every paired run: DOM 230, layout 20016, layout-css 7680, frames 11000,
memory 72000, compute `-362647.901806622`. Ratio is w3cos / Chromium; below
1 is a w3cos win only when the ranges do not overlap.

"Before" is the five-run table at `f5b6581`. That build fused
`createElement` / `appendChild` / property stores into host calls that did
not run own getters, prototype overrides, expandos, or Proxy traps. Those
unguarded emitters were removed when observable lookup was restored
(`a307750`, merged in `ba72b95`). "After" is `6989059`: the fused create
runs only after a shape-epoch check, and a mismatch takes the ordinary
property path.

| workload | metric | w3cos before | w3cos after | Chromium | ratio after |
| --- | --- | ---: | ---: | ---: | ---: |
| startup | cold start ms | 52.3 (51.8–53.2) | 53.2 (52.0–166.9) | 210 (206–1349) | 0.25 |
| dom-list | wall ms | 0.585 (0.566–0.611) | 0.526 (0.518–0.556) | 0.800 (0.700–0.800) | 0.66 |
| dom-list | create ms | 0.524 (0.509–0.551) | 0.423 (0.416–0.443) | 0.700 (0.700–0.800) | 0.60 |
| dom-list | steady RSS | 20.8 MB | 20.9 MB | 920 MB | 0.023 |
| layout | wall ms | 0.699 (0.689–1.207) | 0.750 (0.734–0.765) | 10.4 (10.4–10.7) | 0.072 |
| layout | steady RSS | 24.5 MB | 26.0 MB | 976 MB | 0.027 |
| layout-css | wall ms | 3.75 (3.65–3.78) | 0.881 (0.871–0.896) | 1.100 (1.100–1.100) | 0.80 |
| frames | work p50 ms | 0.132 (0.127–0.134) | 0.183 (0.182–0.189) | 0.200 (0.200–0.300) | 0.92 |
| frames | work p95 ms | 0.136 (0.129–0.151) | 0.193 (0.185–0.219) | 0.300 (0.300–0.305) | 0.64 |
| compute | wall ms | 1.406 (1.405–1.413) | 0.820 (0.812–0.923) | 1.500 (1.400–2.200) | 0.55 |
| memory | wall ms | 8.10 (8.07–8.22) | 18.7 (18.5–25.9) | 19.9 (19.5–30.4) | overlap |
| memory | steady RSS | 25.1 MB | 27.2 MB | 942 MB | 0.029 |

Frame p50/p95 are the median of the five per-run percentiles. Cold start's
167 ms sample is one slow process; the other four sit next to the 53 ms
median, and that maximum is still under Chromium's 206 ms minimum.

Cold start, DOM create, the px flex/grid layout, author-CSS layout, frame
work, the numeric loop, and steady RSS are wins: each w3cos max is below
the Chromium min. The 4000-node build overlaps, so it is not a win.

### DOM create

The create loop samples a shape epoch once in the native-loop prologue.
`defineProperty`, `setPrototypeOf`, and a prototype mutation bump that
epoch. While the stamp matches, AOT builds the element, the unhashed id,
the class, and the text child in one host call. A mismatch returns undefined
and the same call site runs ordinary `createElement`, `appendChild`, and
strict `id` / `className` / `textContent` stores, so own getters, prototype
methods, expandos, and Proxy traps still run. A fresh node with no memoized
wrapper is an immediate handle. `defineProperty` and `setPrototypeOf` pin a
collectable wrapper so authored state stays visible on the next read.

Unique attribute strings are copied into the page arena without a hash
slot and are atomized on lookup. Static names (`div`, `id`, `class`) are
hashed at AOT time and interned with that hash. The page still reserves
32768 node slots and a 1 MiB string arena. Two attributes stay in an inline
buffer. Sibling links are `u32`. Short text stays in the 22-byte inline
string, and the single text child remains a real text node. The unguarded
`dom_build_appended` emitter is not used. The 0.524 ms create at `f5b6581`
skipped the checks above; this path does not.

Create is 0.423 ms for 1000 elements: 0.423 µs/element, range 0.416–0.443 µs.
Chromium is 0.700–0.800 µs. The ranges do not overlap. Checksum 230 matches
on both sides (`item 1`, `upd 39`). The 40 text updates take 0.102 ms
(0.102–0.106). Chromium reports 0.000 ms on every sample, which is the timer
quantum, so the update ratio is not meaningful.

The 4000-node build does not match the fused create shape. It is 18.7 ms
(18.5–25.9). Chromium is 19.9 ms (19.5–30.4). The ranges overlap, so this
is not a win. Steady RSS is 27.2 MB against 942 MB, and that range does not
overlap.

### Numeric loop

The lowered loop is an outlined `#[inline(never)]` function. The trip count
is converted from f64 to i32 once, with `cvttsd2si`, before the loop. The
body stays the dependent recurrence `a = a + b*1e-6`, `b = b + a*1e-6`,
`s = s + (a-b)`. Partial sums are not reassociated. On x86, that `* 1e-6`
self-add lowers to `mul_add` inside `#[target_feature(enable = "fma")]`,
taken only when `is_x86_feature_detected!("fma")` is true. The scalar
fallback keeps separate multiply and add. For this recurrence the FMA form
is bitwise identical to the separate operations, and every sample's
checksum is `-362647.901806622`, the same value V8 reports. FMA is not
enabled globally.

This run's w3cos samples are 0.812–0.923 ms. Chromium is 1.400–2.200 ms.
The ranges do not overlap.

### Layout

The px flex/grid workload is 0.750 ms (0.734–0.765) versus 10.4 ms
(10.4–10.7), checksum 20016 on both sides. There is no author stylesheet and
every item has a px border box, so flush writes those boxes directly and does
not build a Taffy tree.

`layout-css` is the author-CSS workload: 40 cards, six reflows alternating
an 800 px and a 420 px wrap, each card `width:50%`, copy at 16 px with a
20 px line height. Both engines load the same face. The stylesheet is

`@font-face { font-family: w3perf; src: local("Noto Sans Regular"), local("Noto Sans"), url("/NotoSans-Regular.ttf") }`.

Chromium does not resolve `local("Noto Sans")`. `local("Noto Sans Regular")`
resolves to `/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf` and is
available for the timed reflows. An earlier pair, native 8160 versus
Chromium 6240, compared Noto Sans with a fallback UI font because that
shorter local name never became a loaded face before `offsetHeight`.

Simple author blocks (percent or px widths, one text run, no flex, grid,
float, or positioned box) are solved directly and remembered. Two elements
with the same attribute and inline-style signature share a computed style
only when the sheet has no sibling, structural, or relational selector.
Anything else still goes through Taffy. The shared font wraps to the same
line counts on both sides: the 800 px pass is 960 px tall and the 420 px
pass is 1600 px tall, so six reflows sum to 7680. Wall time is 0.881 ms
(0.871–0.896). Chromium is 1.100 ms on every sample. The ranges do not
overlap.

Frame callback work is 0.183 ms p50 (0.182–0.189) and 0.193 ms p95
(0.185–0.219), checksum 11000. Chromium is 0.200–0.300 ms p50 and
0.300–0.305 ms p95. Both native ranges sit under the Chromium minimum.
The p50 edge gap is 0.011 ms. The native pipeline p50 of 9.35 ms adds the
tiny-skia raster and is not compared with the browser rAF gap (16.7 ms).
