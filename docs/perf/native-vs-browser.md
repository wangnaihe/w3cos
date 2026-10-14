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
| css-list | Author rule `#css-list > div { height: 18px }` on 1000 text rows, then one `offsetHeight`. |
| flex-list | 400 text rows in a column flex parent, each `height: 18px` and `flex: 0 0 auto`, then one `offsetHeight`. |
| events | 400 buttons with click listeners already attached. Time is only `dispatchEvent(new Event("click"))`. Checksum is the handler hit count. |

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
binary commit `c2d46f0`, profile `opt-level=3`, LTO off, `codegen-units=16`,
`cpu-render` (tiny-skia). Five processes per workload. Checksums matched on
every paired run: DOM 230, layout 20016, layout-css 7680, frames 11000,
memory 72000, compute `-362647.901806622`. Ratio is w3cos / Chromium; below
1 is a w3cos win only when the ranges do not overlap.

"Before" is the five-run table at `f5b6581`. That build fused
`createElement` / `appendChild` / property stores into host calls that did
not run own getters, prototype overrides, expandos, or Proxy traps. Those
unguarded emitters were removed when observable lookup was restored
(`a307750`, merged in `ba72b95`). "After" is `c2d46f0`: the fused create
runs only after a shape-epoch check, and a mismatch takes the ordinary
property path. Tag and attribute names are static `u32`s. Identical
interned attribute sets share one immutable record and copy on write. A
repeated fixed-height block (one text child, one constant inline
declaration) uses the same stamp. Definite rows that share that style are
stacked from the specified height and do not build a Taffy box.

| workload | metric | w3cos before | w3cos after | Chromium | ratio after |
| --- | --- | ---: | ---: | ---: | ---: |
| startup | cold start ms | 52.3 (51.8–53.2) | 58.9 (55.5–79.5) | 234 (223–921) | 0.25 |
| dom-list | wall ms | 0.585 (0.566–0.611) | 0.529 (0.515–0.550) | 0.900 (0.800–1.000) | 0.59 |
| dom-list | create ms | 0.524 (0.509–0.551) | 0.409 (0.406–0.438) | 0.900 (0.800–1.000) | 0.45 |
| dom-list | steady RSS | 20.8 MB | 20.6 MB | 922 MB | 0.022 |
| layout | wall ms | 0.699 (0.689–1.207) | 0.846 (0.826–0.885) | 11.2 (11.0–13.0) | 0.076 |
| layout | steady RSS | 24.5 MB | 26.1 MB | 976 MB | 0.027 |
| layout-css | wall ms | 3.75 (3.65–3.78) | 0.970 (0.891–1.036) | 1.300 (1.200–1.400) | 0.75 |
| frames | work p50 ms | 0.132 (0.127–0.134) | 0.196 (0.190–0.200) | 0.200 (0.200–0.300) | meet |
| frames | work p95 ms | 0.136 (0.129–0.151) | 0.221 (0.202–0.226) | 0.300 (0.300–0.505) | 0.74 |
| compute | wall ms | 1.406 (1.405–1.413) | 0.863 (0.840–0.901) | 1.600 (1.500–2.100) | 0.54 |
| memory | wall ms | 8.10 (8.07–8.22) | 2.71 (2.66–2.74) | 31.3 (23.7–33.7) | 0.086 |
| memory | steady RSS | 25.1 MB | 19.3 MB | 950 MB | 0.020 |

Frame p50/p95 are the median of the five per-run percentiles. Cold start's
79 ms sample is one slow process; the other four sit next to the 59 ms
median, and that maximum is still under Chromium's 223 ms minimum.
Chromium's 921 ms cold start is one slow process as well.

Cold start, DOM create, the px flex/grid layout, author-CSS layout, frame
p95, the numeric loop, the 4000-node build, and steady RSS are wins: each
w3cos max is below the Chromium min. Frame p50 meets Chromium at 0.200 ms,
so that row is not a win.

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

Tag and attribute names come from a build-time qualified-name table and
compare as `u32`. The AOT prologue interns a dynamic class literal once and
passes that id into the loop, so the create path does not hash it. Unique
id bytes are an arena slice with no hash slot. The id and class maps are
built on the first lookup, which is when those values are atomized.
Elements whose interned attribute sets match share one immutable record;
the first mutation copies it. One or two attributes stay in the element.
A third allocates the spill vector. This is the ShareableElementData /
UniqueElementData split, without Oilpan, compositor tiles, or another
process. The page still reserves 32768 node slots and a 1 MiB string arena.
Sibling links are `u32`. Short text stays in the 22-byte inline string, and
the single text child remains a real text node. The unguarded
`dom_build_appended` emitter is not used. The 0.524 ms create at `f5b6581`
skipped the checks above; this path does not.

Create is 0.409 ms for 1000 elements: 0.409 µs/element, range 0.406–0.438 µs.
Chromium is 0.800–1.000 µs (median 0.900). The ranges do not overlap.
Checksum 230 matches on both sides (`item 1`, `upd 39`). The 40 text updates
take 0.111 ms (0.109–0.116). Chromium's samples are 0.000–0.100 ms, which
is the timer quantum, so the update ratio is not meaningful.

The 4000-node build is `createElement("div")`, `textContent`, one constant
`style.height`, and `appendChild`. That shape is fused behind the same
stamp as the id/class create. A mismatch, an author rule, a mutation
observer, a custom element, a flex or grid parent, or a declaration that
does not stick returns undefined and the loop runs the ordinary strict
stores. Identical `(tag, property, value)` rows share one style record and
copy on write. The host reserves element and text slots together and does
not walk the subtree to invalidate style on each append. At `offsetHeight`,
rows that share that style, have a definite px height, and contain one text
node are stacked as border boxes. Glyph measurement and a per-row Taffy
node are skipped. Any child that differs leaves the rect list untouched and
the ordinary block walk runs. Wall time is 2.71 ms (2.66–2.74), checksum
72000 on both sides. Chromium is 31.3 ms (23.7–33.7). The ranges do not
overlap. Steady RSS is 19.3 MB against 950 MB.

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

This run's w3cos samples are 0.840–0.901 ms. Chromium is 1.500–2.100 ms.
The ranges do not overlap.

### Layout

The px flex/grid workload is 0.846 ms (0.826–0.885) versus 11.2 ms
(11.0–13.0), checksum 20016 on both sides. There is no author stylesheet and
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
pass is 1600 px tall, so six reflows sum to 7680. Wall time is 0.970 ms
(0.891–1.036). Chromium is 1.200–1.400 ms (median 1.300). The ranges do not
overlap.

Frame callback work is 0.196 ms p50 (0.190–0.200) and 0.221 ms p95
(0.202–0.226), checksum 11000. Chromium p50 is 0.200–0.300 ms and p95 is
0.300–0.505 ms. The p95 maximum is under Chromium's p95 minimum. The p50
maximum equals Chromium's p50 minimum, so that range meets and is not a
win. The native pipeline p50 of 10.2 ms adds the tiny-skia raster and is
not compared with the browser rAF gap (16.7 ms).
