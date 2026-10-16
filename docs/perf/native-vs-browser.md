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

## Profile comparison (`d709bf9`)

Pinned host: Linux 6.12.94+, 4 vCPU Intel Xeon, no `/dev/dri`. Rustc 1.99.0
(`b940084d7`, 2026-09-28). Google Chrome 154.0.8037.92. The binaries are
commit `d709bf9b68f2ecc2423abfca8e1b982d6d9ff17f`. `workloads.ts` sha256 is
`dba1b9a0327b08e0ac269aa010f268bc7ad442213238b597e8f4d0f73ad88cb9`. Five
fresh processes per workload. One Chromium series is shared by all three
profiles. Paired checksums agreed on every run.

A win means the native maximum is strictly below the Chromium minimum. A
loss means the native minimum is strictly above the Chromium maximum.
Anything else overlaps, and the ratio of medians is not a win or a loss.
The code gap is a profile compared with Chromium on this source. The
profile gap is size, speed, and max compared with each other. They ran the
same published script.

Column-flex rows with a definite height and `flex: 0 0 auto` now take
`dom_build_flex_row_guarded` after the loop gate. The parent has to be a
nowrap column flex container, and the child has to stay a simple block with
a px height, zero grow, zero shrink, and an automatic basis. A block
parent, a row flex parent, `flex: 1 0 auto`, `height: auto`, and a second
style other than `flex` stay on the ordinary setters.
`native_column_flex_row_loop_is_guarded` requires that intrinsic for the
flex-list loop. `column_flex_uniform_rows_take_fused_create` checks the
fused row against an ordinary row and still sums eight rows to 144 px.

On `06fe305` that loop was unfused. Size flex-list was 3.964 ms
(3.932–4.125) and lost to Chromium 3.200 ms (2.900–3.400). Speed was
3.045 ms (3.015–3.069) and overlapped. Those numbers are that commit.

### Binary size

Stripped `cpu-render` binaries. The instrumented max binary was used only
to collect the profile.

| profile | bytes | sha256 |
| --- | ---: | --- |
| size | 17926272 | `84f0ecfd48a2b026819847de3a175516111f725a3d007c5b9664de974b158c8d` |
| speed | 31114688 | `9aad7ef42b2783442b9c61f937361e13aef1a9c5b13aeb29f6a3493e912ba399` |
| max | 23229184 | `e23c45f9ebc0788c7729a699e349878f3c14cff7392b1af8ed95c0d2ae2f4c73` |

The merged profdata is 23186512 bytes. The instrumented binary is 74456256
bytes, sha256
`c59e55fb46378354f10c6658979073818ac5396173640c6e2f1b40877e035e5d`. Speed is
the largest measured binary: LTO is off and there are 16 codegen units.
Size is the smallest. Max sits between them.

### Five-run table

Times are milliseconds, median (min–max). RSS is `VmRSS` in MB. Checksums
are the common value; every sample matched. The compute checksum is
`-362647.901806622` on every native and Chromium sample. The harness
printer rounds that with six significant digits.

| workload | metric | size | speed | max | Chromium |
| --- | --- | ---: | ---: | ---: | ---: |
| startup | cold start | 63.508 (62.232–82.029) | 53.491 (49.458–75.982) | 43.817 (38.699–45.885) | 203.999 (202.366–229.913) |
| startup | native first raster | 41.211 (40.620–44.950) | 37.838 (35.274–45.264) | 33.543 (28.998–35.360) | n/a |
| startup | CPU | 60 (50–60) | 40 (40–50) | 30 (30–40) | 100 (80–120) |
| startup | peak RSS | 22.6 (22.3–25.3) | 27.9 (25.9–30.9) | 16.9 (16.4–27.9) | 919.6 (917.8–922.1) |
| startup | steady RSS | 30.6 (30.3–30.7) | 33.3 (33.2–33.6) | 27.4 (27.3–27.5) | 919.6 (917.8–922.4) |
| dom-list | wall | 0.703 (0.702–0.727) | 0.579 (0.569–0.588) | 0.325 (0.322–0.366) | 0.700 (0.600–0.800) |
| dom-list | create | 0.454 (0.451–0.473) | 0.371 (0.367–0.384) | 0.220 (0.219–0.258) | 0.700 (0.600–0.800) |
| dom-list | update | 0.251 (0.248–0.253) | 0.206 (0.201–0.207) | 0.104 (0.103–0.107) | 0.000 (0.000–0.000) |
| dom-list | CPU | 60 (60–70) | 50 (40–50) | 20 (20–20) | 100 (90–110) |
| dom-list | peak RSS | 24.9 (24.8–25.1) | 25.7 (25.5–25.7) | 21.5 (21.0–21.6) | 920.1 (918.7–924.8) |
| dom-list | steady RSS | 25.5 (25.5–25.7) | 27.5 (27.4–27.6) | 23.0 (22.9–23.1) | 920.1 (918.7–925.0) |
| dom-list | checksum | 230 | 230 | 230 | 230 |
| layout | wall | 1.029 (1.003–1.034) | 0.775 (0.773–0.779) | 0.540 (0.528–0.553) | 10.700 (10.500–10.900) |
| layout | CPU | 70 (60–70) | 50 (50–50) | 20 (20–30) | 270 (260–310) |
| layout | peak RSS | 26.1 (26.0–26.4) | 26.4 (26.2–26.4) | 21.2 (21.0–21.4) | 979.9 (976.1–981.2) |
| layout | steady RSS | 27.6 (27.4–27.7) | 29.2 (29.1–29.4) | 24.8 (24.5–24.9) | 979.9 (976.1–981.2) |
| layout | checksum | 20016 | 20016 | 20016 | 20016 |
| layout-css | wall | 0.557 (0.541–0.567) | 0.449 (0.444–0.452) | 0.361 (0.355–0.367) | 1.100 (1.100–1.200) |
| layout-css | CPU | 120 (120–130) | 100 (90–100) | 80 (70–90) | 100 (100–120) |
| layout-css | peak RSS | 36.6 (36.4–36.7) | 38.7 (38.4–38.8) | 33.5 (33.1–33.8) | 923.3 (920.3–924.1) |
| layout-css | steady RSS | 36.9 (36.7–36.9) | 38.9 (38.8–39.1) | 33.8 (33.6–34.0) | 923.6 (920.5–924.3) |
| layout-css | checksum | 7680 | 7680 | 7680 | 7680 |
| frames | work p50 | 0.185 (0.183–0.186) | 0.159 (0.153–0.160) | 0.094 (0.092–0.095) | 0.200 (0.200–0.250) |
| frames | work p95 | 0.195 (0.188–0.196) | 0.170 (0.163–0.192) | 0.097 (0.094–0.103) | 0.300 (0.300–0.300) |
| frames | native pipeline p50 | 18.788 (18.747–18.824) | 9.908 (9.820–10.029) | 7.965 (7.874–7.983) | n/a |
| frames | browser rAF gap p50 | n/a | n/a | n/a | 16.700 (16.700–16.700) |
| frames | CPU | 460 (460–470) | 260 (260–270) | 210 (200–210) | 300 (270–310) |
| frames | peak RSS | 34.8 (34.7–34.9) | 37.6 (37.6–38.0) | 31.9 (31.7–31.9) | 926.2 (924.3–928.0) |
| frames | steady RSS | 34.9 (34.7–35.0) | 37.7 (37.6–38.0) | 31.9 (31.8–32.0) | 926.2 (924.3–928.0) |
| frames | checksum | 11000 | 11000 | 11000 | 11000 |
| compute | wall | 0.816 (0.814–0.990) | 0.810 (0.808–0.815) | 0.814 (0.814–0.815) | 1.400 (1.400–1.500) |
| compute | CPU | 10 (10–20) | 10 (10–10) | 10 (0–10) | 110 (90–130) |
| compute | peak RSS | 9.0 (5.8–9.3) | 9.6 (9.5–9.9) | 7.9 (7.8–8.1) | 919.2 (916.8–923.0) |
| compute | steady RSS | 15.4 (15.3–15.6) | 17.3 (17.2–17.5) | 12.8 (12.7–12.9) | 919.5 (917.1–923.2) |
| compute | checksum | -362647.901806622 | -362647.901806622 | -362647.901806622 | -362647.901806622 |
| memory | wall | 2.786 (2.750–2.836) | 2.221 (2.197–2.310) | 1.433 (1.411–1.566) | 18.800 (18.400–20.100) |
| memory | CPU | 10 (10–10) | 10 (0–10) | 0 (0–0) | 130 (110–130) |
| memory | peak RSS | 8.9 (8.7–9.1) | 9.9 (9.6–9.9) | 7.5 (7.2–7.8) | 940.4 (936.7–943.6) |
| memory | steady RSS | 17.7 (17.5–17.9) | 19.3 (19.2–19.5) | 14.9 (14.9–15.0) | 940.7 (937.1–943.6) |
| memory | checksum | 72000 | 72000 | 72000 | 72000 |
| css-list | wall | 5.091 (5.017–5.130) | 4.207 (4.111–4.234) | 2.253 (2.196–2.279) | 6.100 (4.900–7.800) |
| css-list | CPU | 10 (10–10) | 10 (0–10) | 0 (0–0) | 100 (90–130) |
| css-list | peak RSS | 8.8 (8.6–9.1) | 9.7 (9.4–9.8) | 8.0 (7.8–8.2) | 923.9 (922.4–925.2) |
| css-list | steady RSS | 16.8 (16.6–16.9) | 18.2 (18.0–18.3) | 13.7 (13.6–13.9) | 924.1 (922.4–925.4) |
| css-list | checksum | 18000 | 18000 | 18000 | 18000 |
| flex-list | wall | 0.573 (0.564–0.574) | 0.478 (0.469–0.499) | 0.357 (0.344–0.359) | 4.300 (3.000–6.200) |
| flex-list | CPU | 0 (0–10) | 10 (10–10) | 0 (0–0) | 100 (80–130) |
| flex-list | peak RSS | 8.8 (8.6–9.0) | 9.7 (9.5–10.1) | 7.8 (7.4–8.1) | 922.2 (921.4–924.4) |
| flex-list | steady RSS | 16.3 (16.1–16.3) | 18.1 (17.9–18.3) | 13.4 (13.3–13.5) | 922.2 (921.4–924.3) |
| flex-list | checksum | 7200 | 7200 | 7200 | 7200 |
| events | wall | 0.233 (0.232–0.245) | 0.205 (0.205–0.207) | 0.097 (0.097–0.129) | 0.400 (0.400–1.200) |
| events | CPU | 10 (10–10) | 10 (0–10) | 0 (0–0) | 80 (70–100) |
| events | peak RSS | 9.0 (8.8–9.1) | 9.7 (9.6–9.7) | 7.9 (7.4–8.0) | 926.2 (923.5–926.5) |
| events | steady RSS | 16.3 (16.2–16.4) | 18.4 (18.2–18.7) | 13.7 (13.5–13.8) | 926.2 (923.5–926.5) |
| events | checksum | 400 | 400 | 400 | 400 |

Native first raster is inside native cold start. Chromium cold start stops
at the rAF signal and does not encode the frame, so that row has no
Chromium sample. The native pipeline is the rAF callback plus tiny-skia.
The browser rAF gap is scheduling around the callback. Those two rows stay
separate from the frame-work comparison.

### Code gap and profile gap

Flex-list is a win on every profile. Size is 0.573 ms (0.564–0.574), speed
is 0.478 ms (0.469–0.499), and max is 0.357 ms (0.344–0.359). Chromium is
4.300 ms (3.000–6.200). The median ratios are 0.133, 0.111, and 0.083.
Every native maximum is under Chromium's 3.000 ms minimum. The size
maximum, 0.574 ms, is also under the 2.900 ms Chromium minimum recorded on
`06fe305`, so the call does not depend on this series running slower. The
drop from 3.964 / 3.045 / 1.771 ms is the fused row path. Checksum 7200
matched on every sample. Size is 1.60× max and speed is 1.34× max, a
profile gap on top of a win the shipping profile already has.

DOM create stays a win on every profile. Size is 0.454 ms (0.451–0.473),
speed is 0.371 ms (0.367–0.384), and max is 0.220 ms (0.219–0.258).
Chromium is 0.700 ms (0.600–0.800). The median ratios are 0.649, 0.529, and
0.314. Size create is 2.06× max and speed create is 1.69× max. Every create
maximum is under Chromium's 0.600 ms minimum.

DOM wall is 0.703 / 0.579 / 0.325 ms against 0.700 ms (0.600–0.800). Speed
and max win. Size overlaps: the size minimum, 0.702 ms, is above Chromium's
0.600 ms sample, and the size maximum, 0.727 ms, is inside Chromium's
0.800 ms maximum. Create itself still wins on size; the wall overlap is the
update added to a create that is already under the Chromium wall minimum.

DOM update is 0.251 / 0.206 / 0.104 ms. Every Chromium sample is 0.000 ms,
so the ratio is undefined. The native minima are above that recorded
maximum. `performance.now()` on this Chromium does not resolve a
sub-quantum update, so the magnitude past the quantum is not known.

Memory wall is 2.786 ms (2.750–2.836) on size, 2.221 ms (2.197–2.310) on
speed, and 1.433 ms (1.411–1.566) on max, against Chromium 18.800 ms
(18.400–20.100). All three win. That is the styled-block fusion for the
4000-node shape.

Cold start, the px flex/grid layout, author-CSS layout wall, frame-callback
p50 and p95, the numeric loop, and click dispatch are code wins on the size
profile already. Each size maximum is under the Chromium minimum. Max is
faster on those rows, and that is a profile gap on top of a win the
shipping profile already has. Compute is nearly flat: 0.816 / 0.810 /
0.814 ms against 1.400–1.500 ms, checksum `-362647.901806622`. One size
sample reached 0.990 ms and is still under Chromium's 1.400 ms minimum.

CSS-list wall overlaps on size: 5.091 ms (5.017–5.130) against Chromium
6.100 ms (4.900–7.800). Speed's maximum, 4.234 ms, and max's maximum,
2.279 ms, are under Chromium's 4.900 ms minimum. Frame CPU is a loss on
size (460–470 ms against 270–310 ms). Speed is 260–270 ms and overlaps,
because the speed maximum and the Chromium minimum are both 270 ms. Max is
200–210 ms, under 270 ms. Author-CSS CPU overlaps on size (120–130 ms) and
speed (90–100 ms) against Chromium 100–120 ms, and wins on max (70–90 ms,
maximum under the 100 ms minimum).

Frame-callback work already wins on size (p50 maximum 0.186 ms under
Chromium's 0.200 ms minimum; p95 maximum 0.196 ms under 0.300 ms). The
native pipeline p50 is 18.788 / 9.908 / 7.965 ms. The browser rAF gap p50
is 16.700 ms on every sample.

DOM CPU wins on all three profiles (60–70 / 40–50 / 20 ms against 90–110
ms). Layout CPU wins on all three (size maximum 70 ms, Chromium minimum
260 ms). Startup CPU wins on all three (size maximum 60 ms, Chromium
minimum 80 ms). CPU samples on compute, memory, css-list, flex-list, and
events sit on a 10 ms native quantum, so those ratios are coarse.

Steady RSS is about 13–39 MB native and about 910–980 MB Chromium on every
workload, including startup. Size, speed, and max differ by a few
megabytes. That gap is the process. The compiler profile moves it by those
few megabytes.

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
