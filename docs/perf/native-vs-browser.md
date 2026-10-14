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
| layout-css | Author stylesheet, percentage widths, and text-driven auto heights. Six width changes on one wrap, each followed by `offsetHeight`. Simple author blocks are solved directly. Other flex, grid, and floats stay on Taffy. |
| frames | 20 animation frames after 2 warmup frames: scroll plus 80 absolute boxes, then `offsetHeight`. A definite overflow box and absolute children of a relative parent are placed without a Taffy rebuild. |
| compute | 400000 iterations of a scalar numeric loop. |
| memory | Build 4000 nodes, force one layout, then sample RSS. |
| css-list | Author rule `#css-list > div { height: 18px }` on 1000 text rows, then one `offsetHeight`. |
| flex-list | 400 text rows in a column flex parent, each `height: 18px` and `flex: 0 0 auto`, then one `offsetHeight`. Uniform definite rows are stacked from the specified height. |
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
Rustc 1.99.0 (`b940084d7`, 2026-09-28). Google Chrome 154.0.8037.92. The
JSON `host.git` is `d9bc7fe`. The native binary is the speed build of that
commit (`opt-level=3`, LTO off, `codegen-units=16`, `cpu-render` / tiny-skia).
Five processes per workload. Paired checksums agreed on every run. The
scripts still require DOM 230, layout 20016, layout-css 7680, frames 11000,
memory 72000, css-list 18000 (1000 nodes), flex-list 7200 (400 nodes), and
events 400 hits. Compute checksums agreed under the harness relative
tolerance, and every sample is `-362647.901806622`. Ratio is w3cos /
Chromium; below 1 is a w3cos win only when the ranges do not overlap.

"Before" is the five-run table at `f5b6581`. That build fused
`createElement` / `appendChild` / property stores into host calls that did
not run own getters, prototype overrides, expandos, or Proxy traps. Those
unguarded emitters were removed when observable lookup was restored
(`a307750`, merged in `ba72b95`). "After" is `d9bc7fe`. The fused create
still runs only after a shape-epoch check. A column flex of uniform definite
rows, a definite overflow box, and absolute children of a relative parent
are stacked from specified px sizes and do not rebuild Taffy. Repeated
`top` / `left` px writes reuse the existing declaration and an interned
style-cache key. Click dispatch reuses one Event object, stays on the
target when the event does not bubble and no capture listener exists, and
calls a numeric listener through a shared cell. `window` property reads no
longer clone the constructor map. The published `e63a6d3` table is the
previous after column. `840063f` is the one before that, and `fd45422` is
the one before `840063f`.

| workload | metric | w3cos before | w3cos after | Chromium | ratio after |
| --- | --- | ---: | ---: | ---: | ---: |
| startup | cold start ms | 52.3 (51.8–53.2) | 52.1 (51.6–52.5) | 211 (196–224) | 0.25 |
| dom-list | wall ms | 0.585 (0.566–0.611) | 0.470 (0.469–0.486) | 0.800 (0.800–1.200) | 0.59 |
| dom-list | create ms | 0.524 (0.509–0.551) | 0.375 (0.375–0.378) | 0.800 (0.800–1.100) | 0.47 |
| dom-list | steady RSS | 20.8 MB | 20.4 MB | 919 MB | 0.022 |
| layout | wall ms | 0.699 (0.689–1.207) | 0.806 (0.783–0.820) | 10.9 (10.7–11.1) | 0.074 |
| layout | steady RSS | 24.5 MB | 25.9 MB | 974 MB | 0.027 |
| layout-css | wall ms | 3.75 (3.65–3.78) | 0.897 (0.885–0.916) | 1.200 (1.100–1.200) | 0.75 |
| frames | work p50 ms | 0.132 (0.127–0.134) | 0.158 (0.154–0.159) | 0.200 (0.200–0.300) | 0.79 |
| frames | work p95 ms | 0.136 (0.129–0.151) | 0.170 (0.166–0.222) | 0.305 (0.300–0.310) | 0.56 |
| compute | wall ms | 1.406 (1.405–1.413) | 0.816 (0.814–0.817) | 1.400 (1.400–1.600) | 0.58 |
| memory | wall ms | 8.10 (8.07–8.22) | 2.45 (2.36–4.04) | 21.2 (20.5–29.8) | 0.12 |
| memory | steady RSS | 25.1 MB | 19.5 MB | 939 MB | 0.021 |
| css-list | wall ms | — | 2.31 (2.27–2.36) | 6.70 (4.90–8.00) | 0.34 |
| flex-list | wall ms | — | 2.26 (2.21–2.40) | 4.10 (3.70–6.70) | 0.55 |
| events | wall ms | — | 0.201 (0.192–0.273) | 0.300 (0.300–0.400) | 0.67 |

Frame p50/p95 are the median of the five per-run percentiles. Cold start's
52.5 ms sample is the slowest process; that maximum is still under Chromium's
196 ms minimum.

Cold start, DOM wall, DOM create, the px flex/grid layout, author-CSS layout,
frame p50, frame p95, the numeric loop, the 4000-node build, the author-rule
list, the column-flex list, click dispatch, and steady RSS are wins on this
run: each w3cos max is below the Chromium min. DOM text-update time is not a
win: 0.095 ms (0.094–0.099) meets Chromium's 0.000–0.100 ms timer quantum, so
the ranges overlap and the ratio is not meaningful. Click dispatch is 0.201 ms
(0.192–0.273) against 0.300–0.400 ms, checksum 400. Frame p50 is
0.154–0.159 ms. Chromium's minimum is 0.200 ms, and the w3cos maximum is
under it. Frame p95 maximum is 0.222 ms, under Chromium's 0.300 ms minimum.

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

Create is 0.375 ms for 1000 elements: 0.375 µs/element, range 0.375–0.378 µs.
Chromium is 0.800–1.100 µs (median 0.800). The ranges do not overlap.
Checksum 230 matches on both sides (`item 1`, `upd 39`). Whole-loop wall
time is 0.470 ms (0.469–0.486). Chromium's wall minimum on this run is
0.800 ms, so the ranges do not overlap. The 40 text updates take 0.095 ms
(0.094–0.099). Chromium's samples are 0.000–0.100 ms, which is the timer
quantum, so the ranges overlap and the update ratio is not meaningful.

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
the ordinary block walk runs. Wall time is 2.45 ms (2.36–4.04), checksum
72000 on both sides. Chromium is 21.2 ms (20.5–29.8). One of the five
processes measured 4.04 ms; the other four were 2.36–2.45 ms. That maximum
is still under Chromium's 20.5 ms minimum, so the ranges do not overlap.
Steady RSS is 19.5 MB against 939 MB.

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

This run's w3cos samples are 0.814–0.817 ms. Chromium is 1.400–1.600 ms.
The ranges do not overlap.

### Layout

The px flex/grid workload is 0.806 ms (0.783–0.820) versus 10.9 ms
(10.7–11.1), checksum 20016 on both sides. There is no author stylesheet and
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
pass is 1600 px tall, so six reflows sum to 7680. Wall time is 0.897 ms
(0.885–0.916). Chromium is 1.100–1.200 ms (median 1.200). The ranges do not
overlap.

Frame callback work is 0.158 ms p50 (0.154–0.159) and 0.170 ms p95
(0.166–0.222), checksum 11000. Chromium p50 is 0.200–0.300 ms and p95 is
0.300–0.310 ms. Both w3cos maxima are under Chromium's minima on this run.
The p50 maximum, 0.159 ms, is under the Chromium minimum of 0.200 ms. The
native pipeline p50 of 9.91 ms (9.80–10.0) adds the
tiny-skia raster and is not compared with the browser rAF gap (16.7 ms).
CSSOM `top` writes in this callback skip serializing the `style` attribute
when no mutation observer is registered, reuse the existing px declaration,
and do not allocate a new style-cache key. The callback still writes all 80
positions and still reads both `offsetHeight` values. A definite overflow
scroller and the absolute boxes under the relative layer are placed by the
definite walk when geometry is flushed.

### Author-rule list, column flex, and clicks

`css-list` is 1000 text rows under `#css-list > div { height: 18px }`.
Author rules disable the fixed-block fusion, so each row is an ordinary
create, strict `textContent` store, and append. Both sides report height
18000 and count 1000. Wall time is 2.31 ms (2.27–2.36) versus 6.70 ms
(4.90–8.00). The ranges do not overlap.

`flex-list` is 400 text rows in a column flex parent (`flex: 0 0 auto`,
`height: 18px`). Both sides report height 7200 and count 400. Wall time is
2.26 ms (2.21–2.40) versus 4.10 ms (3.70–6.70). The w3cos maximum is under
Chromium's minimum. The guarded fusion still refuses a flex parent. The
column is definite: nowrap, flex-start, stretch, auto container height, and
every child is a simple block with one text node and a px height, so the
border boxes are the sum of the row heights and Taffy is not built. The
published run on `840063f` was 2.47 ms (2.43–2.57) against 3.40 ms
(3.10–5.40). The run on `fd45422` was 59.6 ms (58.7–63.8).

`events` attaches a click listener to each of 400 buttons, then times only
`dispatchEvent(new Event("click"))`. The script checks that all 400
listeners ran. Both sides report 400 hits and 400 nodes. Wall time is
0.201 ms (0.192–0.273) versus 0.300 ms (0.300–0.400). The w3cos maximum,
0.273 ms, is under Chromium's minimum, 0.300 ms, so the ranges do not
overlap. The ratio of medians is 0.67. Four of the five processes were
0.192–0.212 ms; one was 0.273 ms. That is 0.503 µs per dispatch at the
median and 0.683 µs on the slow sample. Chromium's minimum is 0.750 µs per
dispatch. Steady RSS is 18.5 MB against 923 MB. The published run on
`e63a6d3` was 0.486 ms (0.471–0.496), a loss against 0.400 ms
(0.300–0.400). The run on `840063f` was 16.0 ms (15.9–16.4). The run on
`fd45422` was 38.2 ms (37.4–41.9).

Each iteration still re-reads `nodes.length`, because a listener can
append, and still re-reads `window.Event`, because a listener can replace
the constructor. `new Event("click")` still fills a slot (type, bubbles,
cancelable, composed, phase) and the listener still runs. The previous
0.47 ms figure was not that cost. The pool's uniqueness check cloned the
`Rc`, so the strong count was never 1 and every click allocated a new
object. This build checks the count on the pooled object itself. A
non-bubbling event with no own properties and no capture listener stays on
the target: it does not walk ancestors or build `composedPath`. A node with
one non-capture listener of that type is called directly. `hits = hits + 1`
is a bump of the shared number cell and does not allocate a frame. The AOT
loop interns the `"click"` literal once and compares a numeric index with
`nodes.length` as f64, falling back to `less_than` for any other length.
`childNodes` length and index skip the Proxy unless an own or inherited
`length` has been written. Bubbling, capture, re-entry, and an event that
carries own properties still use the full path.
