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
| layout-css | Author stylesheet, percentage widths, and text-driven auto heights. Six width changes on one wrap, each followed by `offsetHeight`. This is the Taffy path. |
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
binary commit `f5b6581`, profile `opt-level=3`, LTO off, `codegen-units=16`,
`cpu-render` (tiny-skia). Five processes per workload. Checksums matched on
every paired run except `layout-css` (DOM 230, layout 20016, frames 11000,
memory 72000, compute `-362647.901806622`). `layout-css` is native 8160 and
Chromium 6240, outside the relative 0.02 / absolute 8 tolerance, so that row
is not a win or a loss. Ratio is w3cos / Chromium; below 1 is a w3cos win.

"Before" is the five-run median at `e3523f5` (lazy DOM immediates, shared UA
style, definite px layout). "After" is `f5b6581`.

| workload | metric | w3cos before | w3cos after | Chromium | ratio after |
| --- | --- | ---: | ---: | ---: | ---: |
| startup | cold start ms | 52.7 (52.3–55.8) | 52.3 (51.8–53.2) | 220 (203–1047) | 0.24 |
| dom-list | wall ms | 3.84 (3.76–3.86) | 0.585 (0.566–0.611) | 0.800 (0.700–1.000) | 0.73 |
| dom-list | create ms | 3.72 | 0.524 (0.509–0.551) | 0.800 (0.700–0.900) | 0.66 |
| dom-list | steady RSS | 23.1 MB | 20.8 MB | 921 MB | 0.023 |
| layout | wall ms | 0.738 (0.725–0.742) | 0.699 (0.689–1.207) | 10.9 (10.7–11.0) | 0.064 |
| layout | steady RSS | 24.4 MB | 24.5 MB | 978 MB | 0.025 |
| layout-css | wall ms | 6.49 (6.41–7.24) | 3.75 (3.65–3.78) | 0.900 (0.900–1.000) | n/a |
| frames | work p50 ms | 0.160 (0.157–0.164) | 0.132 (0.127–0.134) | 0.200 (0.200–0.300) | 0.66 |
| frames | work p95 ms | 0.177 (0.167–0.179) | 0.136 (0.129–0.151) | 0.305 (0.300–0.400) | 0.45 |
| compute | wall ms | 1.411 (1.405–1.466) | 1.406 (1.405–1.413) | 1.500 (1.400–2.200) | overlap |
| memory | wall ms | 16.4 (16.3–16.5) | 8.10 (8.07–8.22) | 19.3 (18.6–20.2) | 0.42 |
| memory | steady RSS | 25.5 MB | 25.1 MB | 941 MB | 0.027 |

`layout-css` before is the five-run median at `b53be99`, the first commit
that ran this workload. It was not in the `e3523f5` table. Frame p50/p95 are
the median of the five per-run percentiles.

Cold start, RSS, DOM create, the px flex/grid layout, frames, and the
4000-node build are wins. DOM create is 0.524 µs/element (range
0.509–0.551 µs). Chromium's create samples are 0.700–0.900 µs, so the ranges
do not overlap.

### DOM create

The 1000-element loop is one host call per iteration: tag, id, class, text,
and append. The page reserves 32768 node slots and a 1 MiB string arena up
front. An element with at most two attributes keeps them in an inline buffer,
so `id` plus `class` does not allocate a `Vec`. The id is an interned atom;
the atom table owns the bytes, and a unique string is bump-allocated in the
page arena instead of a second `String`. Sibling links are `u32` indexes.
Short text stays in the 22-byte inline string. The single text child remains
a real text node, so `childNodes` and layout still observe it. Folding that
text into an invisible tail of the parent would hide it from those APIs.

A `perf cpu-clock` profile of the old create path was dominated by per-node
`malloc`. The fused call removes the argument vector, the per-step document
borrow, and the style walk when the page has no author rules. The steady
create time is under the 0.9 µs target. Update of 40 text nodes is 0.059 ms.
Chromium reports that update as 0.000–0.100 ms, which is the timer quantum,
so the update ratio is not meaningful. Checksum 230 matches (`item 1`,
`upd 39`).

### Numeric loop

The lowered loop is an outlined `#[inline(never)]` function. The trip count
is converted from f64 to i32 once, with `cvttsd2si`, before the loop. The hot
body is 4× unrolled `mulsd`/`addsd`/`subsd` on xmm registers, then
`add $4, %eax` / `cmp` / `jle`. There is no bounds check, no `call`, and no
`Value` boxing in the body. The exit stores the f64 accumulators with
`movsd`. FMA is not used: it would change the checksum
`-362647.901806622`, which matches V8 exactly.

At 2.4 GHz this is about 8 cycles per iteration, and the `a`/`b` multiply-add
chain is serial. Unrolling does not shorten that chain. This run's w3cos
samples are 1.405–1.413 ms. Chromium's median is 1.500 ms, but its fastest
sample is 1.400 ms, so the ranges still overlap. That is not a separated win.

### Layout

The px flex/grid workload is 0.699 ms versus 10.9 ms, checksum 20016 on both
sides. There is no author stylesheet and every item has a px border box, so
flush writes those boxes directly and does not build a Taffy tree.

`layout-css` is the author-CSS workload: `.wrap{width:100%}`,
`.card{display:block;width:50%}`, `.copy{font-size:16px;line-height:20px}`,
40 cards, six reflows alternating 800 px and 420 px. It stays on Taffy.
A width edit patches the cached tree when the cascade change is geometry
only. Definite block widths seed text line heights before the first
`compute_layout`, and a tree that only needs that measure skips the float
and table projection walk. Wall time moved from 6.49 ms at `b53be99` to
3.75 ms (3.65–3.78). Chromium is 0.900 ms (0.900–1.000).

The checksums are not comparable. Native 8160 is three passes at 1120 px plus
three at 1600 px: with Noto Sans, `"measure "` is about 70 px at 16 px, so
five- and six-word cards wrap on the 800 px pass and more of them wrap on
the 420 px pass. Chromium's 6240 matches three passes at 800 px plus three
at 1280 px, which is what a narrower UI font produces for the same source.
A block solver that measured text at the CSS content box changed the native
checksum (7680), so it was not kept. The Taffy result is the one reported.
