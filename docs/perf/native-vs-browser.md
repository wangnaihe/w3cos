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
binary commit `9ae8569`, profile `opt-level=3`, LTO off, `codegen-units=16`,
`cpu-render` (tiny-skia). Five processes per workload. Checksums matched on
every paired run except `layout-css` (DOM 230, layout 20016, frames 11000,
memory 72000, compute `-362647.901806622`). `layout-css` is native 8160 and
Chromium 6240, outside the relative 0.02 / absolute 8 tolerance, so that row
is not a win or a loss. Ratio is w3cos / Chromium; below 1 is a w3cos win
only when the ranges do not overlap.

"Before" is the five-run table at `f5b6581`. That build fused
`createElement` / `appendChild` / property stores into host calls that did
not run own getters, prototype overrides, expandos, or Proxy traps. Those
emitters were removed when observable lookup was restored (`a307750`
merged in `ba72b95`). "After" is `9ae8569`: ordinary lookup stays, and a
node stays an immediate handle only until authored state appears.

| workload | metric | w3cos before | w3cos after | Chromium | ratio after |
| --- | --- | ---: | ---: | ---: | ---: |
| startup | cold start ms | 52.3 (51.8–53.2) | 52.3 (51.4–53.2) | 204 (201–1125) | 0.26 |
| dom-list | wall ms | 0.585 (0.566–0.611) | 3.556 (3.515–3.655) | 0.900 (0.800–0.900) | 3.95 |
| dom-list | create ms | 0.524 (0.509–0.551) | 3.438 (3.413–3.549) | 0.900 (0.700–0.900) | 3.82 |
| dom-list | steady RSS | 20.8 MB | 21.0 MB | 921 MB | 0.023 |
| layout | wall ms | 0.699 (0.689–1.207) | 0.759 (0.732–0.780) | 11.0 (10.4–14.9) | 0.069 |
| layout | steady RSS | 24.5 MB | 26.2 MB | 977 MB | 0.027 |
| layout-css | wall ms | 3.75 (3.65–3.78) | 3.71 (3.66–6.02) | 0.900 (0.900–1.100) | n/a |
| frames | work p50 ms | 0.132 (0.127–0.134) | 0.188 (0.187–0.193) | 0.200 (0.200–0.250) | 0.94 |
| frames | work p95 ms | 0.136 (0.129–0.151) | 0.204 (0.195–0.223) | 0.305 (0.300–0.400) | 0.67 |
| compute | wall ms | 1.406 (1.405–1.413) | 1.405 (1.405–1.415) | 1.500 (1.400–1.800) | overlap |
| memory | wall ms | 8.10 (8.07–8.22) | 18.6 (18.5–19.2) | 23.1 (19.8–31.4) | 0.81 |
| memory | steady RSS | 25.1 MB | 27.5 MB | 941 MB | 0.029 |

Frame p50/p95 are the median of the five per-run percentiles. The `layout-css`
max of 6.02 ms is one slow process; the other four sit next to the 3.71 ms
median.

Cold start, RSS, the px flex/grid layout, frame work, and the 4000-node build
are wins: each w3cos max is below the Chromium min. DOM create is a loss.
The numeric loop still overlaps. `layout-css` stays incomparable.

### DOM create

AOT emits ordinary `createElement`, `appendChild`, and `id` / `className` /
`textContent` stores. Own getters, prototype methods, expandos, and Proxy
get / defineProperty traps run before any cached tree method. A fresh node
with no memoized wrapper is an immediate handle. Gets still walk the live
prototype, then expandos, then the computed element value. `defineProperty`
and `setPrototypeOf` pin a collectable wrapper so authored properties and an
authored prototype stay visible on the next read. A callable prototype setter
materializes that wrapper and runs.

The page still reserves 32768 node slots and a 1 MiB string arena. Two
attributes stay in an inline buffer. Sibling links are `u32`. Short text stays
in the 22-byte inline string, and the single text child remains a real text
node. The create loop is no longer one fused host call. That fusion is what
produced the 0.524 ms create at `f5b6581`, and it skipped the checks above.

A `perf cpu-clock` sample of the checked path is dominated by interning the
unique `"row-N"` and `"item N"` strings and by hashing property names
(`page_arena::intern`, siphash, `PropertyMap::get`). Those strings are the
values a user setter observes, so the path does not drop them.

Create is 3.438 ms for 1000 elements: 3.44 µs/element, range 3.41–3.55 µs.
Chromium is 0.70–0.90 µs. The ranges do not overlap, and w3cos is slower.
Checksum 230 matches on both sides (`item 1`, `upd 39`). The 40 text updates
take 0.107 ms (0.105–0.116). Chromium reports 0.000–0.100 ms, which is the
timer quantum, so the update ratio is not meaningful.

The 4000-node build moved from 8.10 ms, on the fused path, to 18.6 ms
(18.5–19.2). Chromium is 23.1 ms (19.8–31.4). The ranges still do not
overlap, so memory remains a win, with a much smaller margin. Steady RSS is
27.5 MB against 941 MB.

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
samples are 1.405–1.415 ms. Chromium's median is 1.500 ms, but its fastest
sample is 1.400 ms, which is under the w3cos minimum. The ranges overlap.
That is not a separated win.

### Layout

The px flex/grid workload is 0.759 ms (0.732–0.780) versus 11.0 ms
(10.4–14.9), checksum 20016 on both sides. There is no author stylesheet and
every item has a px border box, so flush writes those boxes directly and does
not build a Taffy tree.

`layout-css` is the author-CSS workload: `.wrap{width:100%}`,
`.card{display:block;width:50%}`, `.copy{font-size:16px;line-height:20px}`,
40 cards, six reflows alternating 800 px and 420 px. It stays on Taffy.
A width edit patches the cached tree when the cascade change is geometry
only. Definite block widths seed text line heights before the first
`compute_layout`, and a tree that only needs that measure skips the float
and table projection walk. Wall time is 3.71 ms (3.66–6.02). Chromium is
0.900 ms (0.900–1.100).

The checksums are not comparable. Native 8160 is three passes at 1120 px plus
three at 1600 px: with Noto Sans, `"measure "` is about 70 px at 16 px, so
five- and six-word cards wrap on the 800 px pass and more of them wrap on
the 420 px pass. Chromium's 6240 matches three passes at 800 px plus three
at 1280 px, which is what a narrower UI font produces for the same source.
A block solver that measured text at the CSS content box changed the native
checksum (7680), so it was not kept. The Taffy result is the one reported.

Frame callback work is 0.188 ms p50 (0.187–0.193) and 0.204 ms p95
(0.195–0.223), checksum 11000. Chromium is 0.200–0.250 ms p50 and
0.300–0.400 ms p95. Both native ranges sit under the Chromium minimum, so
frame work is a win. The p50 edge gap is 0.007 ms. The native pipeline p50
of 9.47 ms adds the tiny-skia raster and is not compared with the browser
rAF gap (16.7 ms).
