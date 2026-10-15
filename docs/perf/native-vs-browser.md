# Native vs headless Chromium

This compares the w3cos AOT native runtime with headless Chromium on the same
page source: `tools/perf-native-browser/workloads.ts`. Reproduce with:

```sh
python3 tools/perf-native-browser/run.py --self-check
python3 tools/perf-native-browser/run.py --runs 5
```

The runner builds three stripped `cpu-render` binaries of that source and
times each workload five times against one shared Chromium series.

| Profile | Settings |
| --- | --- |
| size | The profile `w3cos build --release` ships: `opt-level = "z"`, fat LTO, `codegen-units = 1`, panic unwind, symbols stripped. |
| speed | `opt-level = 3`, LTO off, `codegen-units = 16`, panic unwind. Earlier tables on this page measured this profile only. |
| max | `opt-level = 3`, fat LTO, `codegen-units = 1`, panic unwind, symbols stripped, plus PGO. |

`panic = "abort"` is not used. JavaScript exceptions and host callbacks unwind
through `catch_unwind`, so an abort profile is not a compatible application
setting. PGO is rustc `-Cprofile-generate` / `-Cprofile-use` merged with
`llvm-profdata`. It is not a Cargo.toml field. The instrumented binary is a
separate compile of `workloads.ts` with half the node counts and stride 13.
The measured max binary is a profile-use rebuild of the published source. Runtime crates are the same source in both compiles, so
their counters can apply. Application counters that do not match the
published control flow are ignored. The measured trip counts are not the
training set.

`--target-cpu-native` adds a fourth column: the speed profile with
`-Ctarget-cpu=native`. That column is not a shipped profile. It was not
built for the table below.

Chromium is the host Google Chrome in headless mode, sandbox left on,
viewport 1024×768, DPR 1.

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

## Profile comparison (`e7351a2`)

Pinned host: Linux 6.12.94+, 4 vCPU Intel Xeon, no `/dev/dri`. Rustc 1.99.0
(`b940084d7`, 2026-09-28). Google Chrome 154.0.8037.92. The binaries are
commit `e7351a261ceae790af0231dd2430301677375da7`. `workloads.ts` sha256 is
`dba1b9a0327b08e0ac269aa010f268bc7ad442213238b597e8f4d0f73ad88cb9`. Five
fresh processes per workload. One Chromium series is shared by all three
profiles. Paired checksums agreed on every run.

A win means the native maximum is strictly below the Chromium minimum. A
loss means the native minimum is strictly above the Chromium maximum.
Anything else overlaps, and the ratio of medians is not a win or a loss.
The code gap is a profile compared with Chromium on this source. The
profile gap is size, speed, and max compared with each other. They ran the
same published script.

### Binary size

Stripped `cpu-render` binaries. The instrumented max binary was used only
to collect the profile.

| profile | bytes | sha256 |
| --- | ---: | --- |
| size | 17859048 | `ed35b4cd4f05525d4d3f679d3f0fe230ac48c24ee2d9d534215c1c670ce82c9f` |
| speed | 31045928 | `82548a06ae6c1b38d57ef0c28ed431ebf6c24d337d08eec7a8cdb899f1dbc9e2` |
| max | 23136616 | `0099b7a0bb2e7ef95fa18cfbfb439c44e6964abb35e34c23e9e0d1a09e31e5dc` |

The merged profdata is 23158520 bytes. The instrumented binary is 74313376
bytes, sha256
`e8253495a4672520091443b67d8f535d0a01537a6658ba4b54b231cc062316c5`. Speed is
the largest measured binary: LTO is off and there are 16 codegen units.
Size is the smallest. Max sits between them.

### Five-run table

Times are milliseconds, median (min–max). RSS is `VmRSS` in MB. Checksums
are the common value; every sample matched. The compute checksum is
`-362647.901806622` on every native and Chromium sample. The harness
printer rounds that with six significant digits.

| workload | metric | size | speed | max | Chromium |
| --- | --- | ---: | ---: | ---: | ---: |
| startup | cold start | 78.107 (72.250–236.689) | 59.170 (56.211–157.668) | 48.794 (41.549–62.894) | 320.784 (288.228–386.012) |
| startup | native first raster | 48.477 (46.270–149.493) | 40.977 (38.748–63.749) | 35.436 (30.224–44.867) | n/a |
| startup | CPU | 60 (60–170) | 50 (50–70) | 30 (30–50) | 150 (120–170) |
| startup | peak RSS | 30.3 (29.8–30.4) | 25.7 (24.7–33.1) | 19.5 (19.3–27.7) | 920.1 (919.7–922.6) |
| startup | steady RSS | 30.4 (30.3–30.5) | 33.3 (33.1–33.5) | 27.2 (27.1–27.4) | 920.1 (919.7–922.6) |
| dom-list | wall | 9.480 (9.435–9.977) | 7.392 (7.334–7.452) | 3.794 (3.758–3.858) | 0.800 (0.700–1.400) |
| dom-list | create | 9.237 (9.196–9.739) | 7.197 (7.139–7.253) | 3.693 (3.655–3.754) | 0.800 (0.700–1.400) |
| dom-list | update | 0.241 (0.238–0.242) | 0.196 (0.196–0.199) | 0.102 (0.102–0.103) | 0.000 (0.000–0.000) |
| dom-list | CPU | 120 (120–140) | 100 (90–100) | 40 (40–50) | 90 (90–110) |
| dom-list | peak RSS | 25.8 (24.2–25.9) | 26.1 (25.8–26.3) | 21.6 (21.3–21.9) | 919.3 (917.8–922.4) |
| dom-list | steady RSS | 25.7 (25.6–25.9) | 27.7 (27.6–27.9) | 23.0 (22.9–23.2) | 919.6 (917.8–922.6) |
| dom-list | checksum | 230 | 230 | 230 | 230 |
| layout | wall | 1.013 (1.004–1.018) | 0.757 (0.748–0.765) | 0.533 (0.529–0.539) | 11.000 (10.500–12.800) |
| layout | CPU | 70 (60–70) | 50 (40–50) | 20 (20–30) | 270 (250–310) |
| layout | peak RSS | 26.3 (26.1–26.5) | 26.4 (26.3–26.6) | 21.1 (21.0–21.2) | 978.5 (975.6–979.0) |
| layout | steady RSS | 27.5 (27.4–27.5) | 29.3 (29.1–29.4) | 24.6 (24.6–24.6) | 978.5 (975.6–979.0) |
| layout | checksum | 20016 | 20016 | 20016 | 20016 |
| layout-css | wall | 0.549 (0.540–0.558) | 0.447 (0.436–0.450) | 0.368 (0.358–0.371) | 1.100 (1.000–1.200) |
| layout-css | CPU | 120 (120–120) | 90 (90–100) | 70 (70–70) | 100 (80–110) |
| layout-css | peak RSS | 36.5 (36.3–36.9) | 38.6 (38.6–38.7) | 33.4 (33.3–33.6) | 922.2 (919.7–923.5) |
| layout-css | steady RSS | 36.8 (36.6–36.9) | 38.9 (38.8–39.0) | 33.8 (33.7–33.9) | 922.5 (920.0–923.5) |
| layout-css | checksum | 7680 | 7680 | 7680 | 7680 |
| frames | work p50 | 0.182 (0.181–0.187) | 0.157 (0.156–0.162) | 0.093 (0.092–0.095) | 0.200 (0.200–0.300) |
| frames | work p95 | 0.191 (0.189–0.267) | 0.172 (0.159–0.226) | 0.100 (0.098–0.158) | 0.300 (0.300–0.425) |
| frames | native pipeline p50 | 18.798 (18.777–21.203) | 9.976 (9.898–11.445) | 7.940 (7.897–9.028) | n/a |
| frames | browser rAF gap p50 | n/a | n/a | n/a | 16.700 (16.700–16.700) |
| frames | CPU | 460 (460–530) | 260 (260–310) | 210 (200–240) | 290 (280–340) |
| frames | peak RSS | 34.9 (34.8–35.0) | 37.7 (37.6–37.9) | 31.8 (31.6–31.8) | 925.2 (924.9–926.8) |
| frames | steady RSS | 34.9 (34.9–35.1) | 37.7 (37.6–38.1) | 31.8 (31.7–31.9) | 925.2 (924.9–926.8) |
| frames | checksum | 11000 | 11000 | 11000 | 11000 |
| compute | wall | 0.816 (0.816–0.819) | 0.812 (0.810–0.816) | 0.815 (0.807–0.816) | 1.400 (1.400–1.500) |
| compute | CPU | 10 (0–10) | 10 (10–10) | 0 (0–10) | 100 (100–130) |
| compute | peak RSS | 8.9 (8.7–9.1) | 9.5 (9.4–9.7) | 7.8 (7.4–7.9) | 920.6 (918.5–926.0) |
| compute | steady RSS | 15.4 (15.1–15.6) | 17.2 (16.9–17.3) | 12.6 (12.5–12.7) | 920.9 (918.5–926.2) |
| compute | checksum | -362647.901806622 | -362647.901806622 | -362647.901806622 | -362647.901806622 |
| memory | wall | 33.365 (33.119–33.867) | 25.897 (25.755–26.410) | 14.638 (14.527–14.749) | 28.500 (18.300–33.600) |
| memory | CPU | 40 (40–40) | 30 (20–30) | 10 (10–20) | 140 (120–150) |
| memory | peak RSS | 24.9 (24.8–25.1) | 22.3 (22.0–22.5) | 7.8 (7.6–8.2) | 950.0 (939.3–954.1) |
| memory | steady RSS | 25.3 (25.2–25.5) | 27.3 (26.8–27.4) | 22.6 (22.4–22.7) | 950.0 (939.6–954.1) |
| memory | checksum | 72000 | 72000 | 72000 | 72000 |
| css-list | wall | 5.020 (4.963–5.062) | 4.119 (4.048–4.159) | 2.247 (2.221–2.273) | 6.900 (5.300–8.800) |
| css-list | CPU | 10 (10–10) | 10 (0–10) | 0 (0–0) | 100 (90–120) |
| css-list | peak RSS | 8.9 (8.8–9.1) | 9.5 (6.4–9.5) | 7.7 (7.6–7.8) | 922.0 (919.7–926.0) |
| css-list | steady RSS | 16.6 (16.6–16.9) | 18.0 (18.0–18.2) | 13.6 (13.4–13.7) | 922.0 (919.7–926.3) |
| css-list | checksum | 18000 | 18000 | 18000 | 18000 |
| flex-list | wall | 3.910 (3.889–3.928) | 3.146 (3.109–3.167) | 1.766 (1.731–1.865) | 4.300 (3.300–5.700) |
| flex-list | CPU | 10 (10–10) | 10 (10–10) | 0 (0–0) | 110 (100–120) |
| flex-list | peak RSS | 9.1 (8.9–9.6) | 9.3 (6.2–9.5) | 7.7 (7.2–7.9) | 920.7 (919.9–922.5) |
| flex-list | steady RSS | 17.0 (16.8–17.1) | 18.6 (18.6–18.8) | 14.1 (13.9–14.1) | 920.7 (920.2–922.8) |
| flex-list | checksum | 7200 | 7200 | 7200 | 7200 |
| events | wall | 0.230 (0.228–0.242) | 0.210 (0.206–0.229) | 0.097 (0.097–0.100) | 0.400 (0.300–0.600) |
| events | CPU | 10 (0–10) | 10 (10–10) | 0 (0–0) | 110 (90–120) |
| events | peak RSS | 8.9 (8.5–9.6) | 9.5 (9.2–9.6) | 7.7 (7.1–7.9) | 925.3 (922.9–927.2) |
| events | steady RSS | 16.3 (16.2–16.6) | 18.3 (18.1–18.7) | 13.5 (13.4–13.7) | 925.3 (923.2–927.2) |
| events | checksum | 400 | 400 | 400 | 400 |

Startup CPU and RSS are the five process samples stored with this run
(median, minimum, maximum). The harness comparison rows at the time of the
run omitted them; the same samples are what a later run prints. Native first
raster is inside native cold start. Chromium cold start stops at the rAF
signal and does not encode the frame, so that row is not a comparison. The
native pipeline is the rAF callback plus tiny-skia. The browser rAF gap is
scheduling around the callback. Those two rows are not a ratio.

### Code gap and profile gap

DOM create is a code loss on every profile. Size is 9.237 ms
(9.196–9.739), speed is 7.197 ms (7.139–7.253), and max is 3.693 ms
(3.655–3.754). Chromium is 0.800 ms (0.700–1.400). Max is about 2.5× the
size profile and about 1.9× the speed profile, and its fastest sample,
3.655 ms, is still above Chromium's slowest, 1.400 ms. The 0.375 ms create
in the `d9bc7fe` section below is that commit, not this tree. DOM wall
follows create: 9.480 / 7.392 / 3.794 ms against 0.800 ms (0.700–1.400),
also a loss on every profile.

DOM update is 0.241 / 0.196 / 0.102 ms. Every Chromium sample is 0.000 ms,
so the ratio is undefined. The native minima are above that recorded
maximum. `performance.now()` on this Chromium does not resolve a
sub-quantum update, so the magnitude past the quantum is not known.

Cold start, the px flex/grid layout, author-CSS layout, frame-callback
p50 and p95, the numeric loop, the author-rule list, and click dispatch
are code wins on the size profile already. Each size maximum is under the
Chromium minimum. Max is faster on those rows, and that is a profile gap
on top of a win the shipping profile already has. Compute is the flat
case: 0.816 / 0.812 / 0.815 ms against 1.400–1.500 ms, checksum
`-362647.901806622`. The three profiles differ by a few microseconds, inside
the ranges, so the profile does not move it.

Two wall-time rows change the win call when the profile changes. Memory is
33.365 ms (33.119–33.867) on size and 25.897 ms (25.755–26.410) on speed.
Both overlap Chromium 28.500 ms (18.300–33.600). Max is 14.638 ms
(14.527–14.749), under Chromium's 18.300 ms minimum, so only max wins.
Flex-list is 3.910 ms (3.889–3.928) on size, which overlaps Chromium
4.300 ms (3.300–5.700). Speed's maximum, 3.167 ms, is under Chromium's
3.300 ms minimum, so speed and max win and size does not.

Frame-callback work already wins on size (p50 maximum 0.187 ms under
Chromium's 0.200 ms minimum; p95 maximum 0.267 ms under 0.300 ms). Frame
CPU does not. Size is 460–530 ms against Chromium 280–340 ms, a loss.
Speed is 260–310 ms and overlaps. Max is 200–240 ms, under Chromium's
280 ms minimum. The native pipeline p50 is 18.798 / 9.976 / 7.940 ms. That
drop is a profile gap, and it is not compared with the 16.700 ms browser
rAF gap.

DOM CPU is a loss on size (120–140 ms vs 90–110 ms), an overlap on speed
(90–100 ms), and a win on max (40–50 ms vs 90–110 ms). Author-CSS CPU is
the same shape: size 120 ms loses to 80–110 ms, speed 90–100 ms overlaps,
and max 70 ms wins. Layout CPU wins on all three profiles (size maximum
70 ms, Chromium minimum 250 ms). CPU samples on compute, css-list,
flex-list, and events sit on a 10 ms native quantum, so those ratios are
coarse.

Steady RSS is about 13–39 MB native and about 920–980 MB Chromium on every
workload, including startup. Size, speed, and max differ by a few
megabytes. That gap is the process, not the compiler profile.

## Previous speed-only run (`d9bc7fe`)

The table in this section is the earlier speed-only measurement. It is not
the three-profile run above.

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
