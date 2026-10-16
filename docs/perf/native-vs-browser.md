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
| dom-list | Create 1000 nodes, then 8000 keyed text updates and moves. |
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

## Profile comparison (`ffbd97c`)

Pinned host: Linux 6.12.94+, 4 vCPU Intel Xeon, no `/dev/dri`. Rustc 1.99.0
(`b940084d7`, 2026-09-28). Google Chrome 154.0.8037.92. The binaries are
commit `ffbd97c89cf9f159bdbd60d53a8ea35093442466`. `workloads.ts` sha256 is
`ec367da79305f2a65333e7b4408d1c57ec2bdcfe563a5022b549986b4139aceb`, the same
script as `9a87534`. Five fresh processes per workload. One Chromium series
is shared by all three profiles. Paired checksums agreed on every run. The
`9a87534` table below is unchanged.

Release host UI font lookup asks fontconfig for the existing family list
(PingFang SC, Microsoft YaHei, Noto Sans CJK SC, Noto Sans SC, Noto Sans,
Roboto, then sans-serif) at regular weight and loads only the matched file.
The test build keeps the full fontdb query: the pinned test fontconfig
prefers the CJK subset, while that test database still returns Noto Sans
Regular. On this host the fontconfig match and a full scan both select
`/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf` (512672 bytes). Glyph
outlines compile on first rasterize. The startup frame checksum stays
73058730294 and the marker stays `w3cos-perf-ready`.

The first page reserves 16384 nodes and 256 KB of page strings. The id
index starts at 2048 slots. The atom table starts at 8192 slots and 8 spare
chunks. Four thousand text rows stay inside that first page
(`four_thousand_text_rows_stay_inside_the_first_page`).

`table-repaint`'s inner loop (`cells[i].textContent = "v " + pass + "-" + i`,
then the length sum) is one guarded fill when every cell is still a single
text child and `textContent` is the builtin. An extra property write stays
on the ordinary path. `native_sequential_text_fill_loop_is_rewritten` and
`sequential_text_fill_matches_prefix_pair_lengths` cover that shape.
Checksum 5704 matched.

Compute stays flat because the timed loop is already an outlined native
`f64` leaf with an FMA variant. Each of the 400000 iterations is two
dependent multiply-adds (`a = a + b * 1e-6`, `b = b + a * 1e-6`) plus
`s = s + (a - b)`. On this host that chain is about 2.0 ns per iteration
(400000 / 0.810 ms). opt-z, opt-3, and the PGO rebuild emit that same leaf,
so the three profiles stay within 0.810–0.816 ms. The checksum remains
`-362647.901806622`.

`layout_microbench` on this host was 2283 µs, inside the 9 ms budget.

A win means the native maximum is strictly below the Chromium minimum.
Every comparable lower-is-better row in this table is a win (165 wins).
Native first raster has no Chromium sample.

### Binary size

Stripped `cpu-render` binaries. The instrumented max binary was used only
to collect the profile.

| profile | bytes | sha256 |
| --- | ---: | --- |
| size | 18274984 | `cf8aec8023932936805b699732a4446a01ce30cd4d16b9eb1b91b09fe31353af` |
| speed | 31731160 | `2f4a5768758d4b7f8a3e6cd266ea3a78b7011efce6901cf770cb515380e95db3` |
| max | 23937320 | `b99bfca0ef747c48a1b5da2ed93c16d76b47280d63cfc35f40db0b7b52205f55` |

The merged profdata is 23365608 bytes. The instrumented binary is 75469840
bytes, sha256
`9cec5cf5ce47d0258700608d51108b91547fcf026aa0bf68d56be584d5a56566`.

### Five-run table

Times are milliseconds, median (min–max) of the five process results. RSS
is `VmRSS` in MB. Checksums are the common value. The compute checksum is
`-362647.901806622` on every native and Chromium sample.

| workload | metric | size | speed | max | Chromium |
| --- | --- | ---: | ---: | ---: | ---: |
| startup | cold start | 25.286 (24.441–40.004) | 22.824 (22.244–41.292) | 17.519 (17.282–17.860) | 211.757 (190.163–218.537) |
| startup | native first raster | 12.295 (11.864–14.450) | 11.113 (10.649–15.339) | 10.098 (10.009–10.484) | n/a |
| startup | CPU | 10 (10–20) | 10 (10–20) | 10 (0–10) | 90 (80–100) |
| startup | peak RSS | 24.0 (15.2–24.5) | 9.4 (8.9–17.0) | 8.3 (8.1–8.4) | 920.8 (917.9–922.6) |
| startup | steady RSS | 24.5 (24.2–24.6) | 26.7 (26.7–26.9) | 21.3 (21.2–21.4) | 920.9 (917.9–922.9) |
| dom-list | wall | 2.165 (2.157–2.191) | 1.454 (1.433–1.469) | 0.760 (0.751–0.766) | 5.900 (5.500–6.100) |
| dom-list | create | 0.460 (0.451–0.465) | 0.372 (0.359–0.390) | 0.217 (0.216–0.237) | 0.700 (0.700–0.800) |
| dom-list | update | 1.708 (1.692–1.732) | 1.077 (1.067–1.082) | 0.538 (0.531–0.553) | 5.100 (4.800–5.400) |
| dom-list | CPU | 40 (30–40) | 20 (20–20) | 10 (0–10) | 190 (180–200) |
| dom-list | peak RSS | 16.6 (16.3–16.9) | 19.4 (19.3–19.5) | 8.2 (8.2–8.2) | 940.8 (937.7–941.8) |
| dom-list | steady RSS | 18.6 (18.2–18.6) | 20.4 (20.3–20.5) | 15.8 (15.8–15.9) | 940.8 (937.7–941.8) |
| dom-list | checksum | 62890 | 62890 | 62890 | 62890 |
| layout | wall | 1.019 (1.017–1.029) | 0.768 (0.762–0.772) | 0.548 (0.547–0.563) | 10.600 (10.400–10.700) |
| layout | CPU | 40 (40–50) | 40 (40–40) | 20 (20–20) | 290 (250–290) |
| layout | peak RSS | 22.0 (21.6–22.0) | 24.2 (21.0–25.4) | 20.6 (20.5–21.0) | 976.9 (974.8–978.0) |
| layout | steady RSS | 23.9 (23.7–24.0) | 25.8 (25.6–26.0) | 21.3 (21.1–21.4) | 977.0 (974.8–977.9) |
| layout | checksum | 20016 | 20016 | 20016 | 20016 |
| layout-css | wall | 0.465 (0.458–0.470) | 0.345 (0.337–0.348) | 0.256 (0.253–0.261) | 1.200 (1.100–1.300) |
| layout-css | CPU | 40 (40–40) | 30 (30–30) | 20 (20–30) | 100 (80–120) |
| layout-css | peak RSS | 22.3 (21.6–24.5) | 18.8 (18.7–20.8) | 18.6 (17.9–18.8) | 921.0 (919.7–923.2) |
| layout-css | steady RSS | 22.0 (21.9–22.1) | 24.0 (23.9–24.1) | 19.0 (18.9–19.1) | 921.0 (919.9–923.5) |
| layout-css | checksum | 7680 | 7680 | 7680 | 7680 |
| frames | work p50 | 0.157 (0.156–0.162) | 0.135 (0.133–0.136) | 0.076 (0.075–0.077) | 0.200 (0.200–0.300) |
| frames | work p95 | 0.167 (0.164–0.266) | 0.145 (0.140–0.147) | 0.080 (0.079–0.083) | 0.300 (0.300–0.400) |
| frames | native pipeline p50 | 4.777 (4.763–4.851) | 3.696 (3.662–3.743) | 3.033 (3.000–3.038) | n/a |
| frames | browser rAF gap p50 | n/a | n/a | n/a | 16.700 (16.700–16.700) |
| frames | CPU | 140 (130–140) | 110 (100–110) | 80 (70–80) | 300 (290–310) |
| frames | peak RSS | 28.9 (28.8–29.1) | 31.5 (31.3–31.6) | 26.0 (25.9–26.2) | 928.6 (925.1–929.4) |
| frames | steady RSS | 29.1 (28.8–29.1) | 31.5 (31.4–31.6) | 26.1 (25.9–26.3) | 928.6 (925.1–929.4) |
| frames | checksum | 11000 | 11000 | 11000 | 11000 |
| compute | wall | 0.810 (0.810–0.810) | 0.816 (0.814–0.817) | 0.815 (0.810–0.815) | 1.400 (1.400–1.400) |
| compute | CPU | 10 (10–10) | 10 (10–10) | 10 (0–10) | 110 (110–120) |
| compute | peak RSS | 9.1 (9.0–9.1) | 10.0 (9.6–10.0) | 8.4 (8.1–8.5) | 921.3 (919.9–923.0) |
| compute | steady RSS | 15.0 (14.9–15.0) | 17.0 (16.9–17.0) | 12.6 (12.6–12.7) | 921.3 (920.2–923.2) |
| compute | checksum | -362647.901806622 | -362647.901806622 | -362647.901806622 | -362647.901806622 |
| memory | wall | 2.971 (2.902–3.016) | 2.346 (2.314–2.390) | 1.426 (1.417–1.486) | 20.000 (18.700–32.400) |
| memory | CPU | 0 (0–10) | 10 (0–10) | 0 (0–0) | 120 (110–160) |
| memory | peak RSS | 9.0 (8.9–9.2) | 9.7 (9.7–10.1) | 8.1 (8.1–8.4) | 943.4 (937.9–951.7) |
| memory | steady RSS | 17.3 (17.1–17.5) | 19.3 (19.2–19.6) | 14.6 (14.4–14.8) | 943.7 (938.1–951.7) |
| memory | checksum | 72000 | 72000 | 72000 | 72000 |
| css-list | wall | 1.279 (1.255–1.295) | 1.024 (1.013–1.072) | 0.675 (0.665–0.699) | 4.800 (4.700–5.900) |
| css-list | CPU | 10 (10–10) | 0 (0–0) | 0 (0–0) | 80 (80–100) |
| css-list | peak RSS | 9.2 (9.1–9.3) | 9.8 (9.6–10.0) | 8.2 (8.1–8.4) | 922.5 (921.5–923.6) |
| css-list | steady RSS | 16.4 (16.1–16.6) | 18.2 (18.0–18.3) | 13.4 (13.2–13.5) | 922.5 (921.8–923.9) |
| css-list | checksum | 18000 | 18000 | 18000 | 18000 |
| flex-list | wall | 0.562 (0.536–0.624) | 0.478 (0.451–0.496) | 0.321 (0.316–0.341) | 3.500 (2.900–4.300) |
| flex-list | CPU | 0 (0–10) | 10 (10–10) | 0 (0–0) | 110 (80–110) |
| flex-list | peak RSS | 8.8 (7.5–9.2) | 9.8 (9.7–10.1) | 8.4 (8.1–8.6) | 921.1 (919.1–921.8) |
| flex-list | steady RSS | 15.9 (15.7–15.9) | 17.9 (17.8–18.0) | 13.2 (13.1–13.2) | 921.1 (919.4–921.8) |
| flex-list | checksum | 7200 | 7200 | 7200 | 7200 |
| events | wall | 0.241 (0.237–0.243) | 0.201 (0.198–0.212) | 0.101 (0.099–0.103) | 0.400 (0.300–0.400) |
| events | CPU | 10 (0–10) | 10 (10–10) | 0 (0–0) | 90 (70–100) |
| events | peak RSS | 9.0 (8.8–9.3) | 10.0 (9.8–10.3) | 8.2 (7.8–8.4) | 924.3 (924.0–926.5) |
| events | steady RSS | 16.2 (15.8–16.4) | 18.2 (18.1–18.5) | 13.3 (13.2–13.3) | 924.3 (924.0–926.5) |
| events | checksum | 400 | 400 | 400 | 400 |
| table-repaint | wall | 1.245 (1.231–1.280) | 0.950 (0.946–0.976) | 0.597 (0.567–0.651) | 5.000 (4.500–5.400) |
| table-repaint | CPU | 40 (40–50) | 30 (30–30) | 20 (20–30) | 100 (90–140) |
| table-repaint | peak RSS | 25.0 (24.8–25.1) | 23.1 (21.9–23.1) | 22.1 (21.3–22.3) | 929.0 (927.1–930.4) |
| table-repaint | steady RSS | 25.0 (24.9–25.3) | 27.7 (27.5–27.9) | 22.6 (22.6–22.7) | 929.3 (927.4–930.7) |
| table-repaint | checksum | 5704 | 5704 | 5704 | 5704 |
| styled-scroll | wall | 1.314 (1.307–1.325) | 1.046 (1.016–1.145) | 0.725 (0.710–0.739) | 5.100 (4.000–5.400) |
| styled-scroll | CPU | 0 (0–10) | 10 (10–10) | 0 (0–0) | 80 (70–100) |
| styled-scroll | peak RSS | 9.2 (9.0–9.2) | 10.0 (9.7–10.1) | 8.2 (8.1–8.3) | 927.2 (924.3–928.4) |
| styled-scroll | steady RSS | 16.4 (16.3–16.7) | 18.1 (18.0–18.3) | 13.4 (13.2–13.4) | 927.5 (924.3–928.7) |
| styled-scroll | checksum | 650115 | 650115 | 650115 | 650115 |
| text-layout | wall | 0.173 (0.169–0.182) | 0.137 (0.135–0.144) | 0.081 (0.079–0.084) | 2.200 (2.000–3.200) |
| text-layout | CPU | 40 (40–50) | 40 (30–40) | 20 (20–20) | 110 (70–130) |
| text-layout | peak RSS | 23.4 (21.6–23.9) | 20.4 (18.8–29.0) | 18.4 (17.9–18.8) | 924.0 (921.8–924.8) |
| text-layout | steady RSS | 22.0 (21.7–22.1) | 24.3 (24.1–24.4) | 19.2 (19.1–19.3) | 924.0 (922.1–925.0) |
| text-layout | checksum | 34320 | 34320 | 34320 | 34320 |

Size cold start is 25.286 ms (24.441–40.004) against Chromium 211.757 ms
(190.163–218.537). On `9a87534` the size median was 52.836 ms. Native first
raster is 12.295 ms (11.864–14.450); on `9a87534` it was 39.974 ms. Startup
steady RSS is 24.5 MB (24.2–24.6) against 920.9 MB (917.9–922.9). On
`9a87534` the size steady RSS was 30.6 MB. Compute steady RSS is 15.0 MB
(14.9–15.0); on `9a87534` it was 15.5 MB.

Frame CPU on the size profile is 140 ms (130–140) against Chromium 300 ms
(290–310). The size maximum, 140 ms, is under 290 ms. On `9a87534` that
maximum was 170 ms. Checksum 11000 matched. Size pipeline p50 is 4.777 ms
(4.763–4.851).

`table-repaint` wall on the size profile is 1.245 ms (1.231–1.280) against
Chromium 5.000 ms (4.500–5.400). On `9a87534` the size median was 2.301 ms.
Size CPU is 40 ms (40–50) against 100 ms (90–140). On `9a87534` the size
CPU maximum was 70 ms. Checksum 5704 matched.

## Profile comparison (`9a87534`)

Pinned host: Linux 6.12.94+, 4 vCPU Intel Xeon, no `/dev/dri`. Rustc 1.99.0
(`b940084d7`, 2026-09-28). Google Chrome 154.0.8037.92. The binaries are
commit `9a875348c29a55a7f11c11d18ba2193e1d176594`. `workloads.ts` sha256 is
`ec367da79305f2a65333e7b4408d1c57ec2bdcfe563a5022b549986b4139aceb`. Five
fresh processes per workload. One Chromium series is shared by all three
profiles. Paired checksums agreed on every run. The `09c29fc` table below
is the previous script and is unchanged.

Three workloads were added on both engines. `table-repaint` is a 24×6
fixed table with collapsed borders, zero spacing, and clipped 22px cells.
Four passes rewrite every cell, then read `offsetHeight`. Checksum 5704.
`styled-scroll` is 800 class-styled rows in an `overflow:auto` box, 30
`scrollTop` steps, and the last row's `offsetTop`. Checksum 650115.
`text-layout` is 80 paragraphs in the same `w3perf` face as `layout-css`,
with the column width toggled between 640px and 360px. Both widths are
laid out before the timer. Checksum 34320.

A definite px width and height does not relayout when its text changes.
Auto height still grows. Setting that height back to `auto` after the
text change matches a fresh layout. A clipped table cell keeps its
specified height. Scroll size is remembered until a layout rect is stored
again, and the walk follows sibling links. An authored column of definite
blocks may use one-sided px margins and a definite overflow box. Adjacent
non-zero vertical margins stay on Taffy. The row stack matches
`compute_with_scroll_details`.

A win means the native maximum is strictly below the Chromium minimum.
Every comparable row in this table is a win. Native first raster has no
Chromium sample.

### Binary size

Stripped `cpu-render` binaries. The instrumented max binary was used only
to collect the profile.

| profile | bytes | sha256 |
| --- | ---: | --- |
| size | 18272600 | `3d30b0e09431589ba5cf2fb7890b405511b95977bd1ed3f25e5aab2d77185d1b` |
| speed | 31720400 | `04a24f2d476e142b4009e8883ba42e0189fbe5240dba5b4ad21ded6f1c7759f2` |
| max | 23910232 | `e59928f50c5283c298f4882e41ba090c2b986b07b1954b3339bb0b4369a50330` |

The merged profdata is 23385472 bytes. The instrumented binary is 75448912
bytes, sha256
`aaa84b988d6386d6e9293b912589050899b84e728b4be687222fc7570646d99a`.

### Five-run table

Times are milliseconds, median (min–max) of the five process results. RSS
is `VmRSS` in MB. Checksums are the common value. The compute checksum is
`-362647.901806622` on every native and Chromium sample.

| workload | metric | size | speed | max | Chromium |
| --- | --- | ---: | ---: | ---: | ---: |
| startup | cold start | 52.836 (51.432–73.417) | 47.234 (44.296–68.998) | 35.573 (33.783–42.890) | 202.317 (194.003–322.253) |
| startup | native first raster | 39.974 (38.751–44.237) | 35.032 (32.478–39.912) | 28.088 (26.272–35.201) | n/a |
| startup | CPU | 40 (40–50) | 40 (40–40) | 30 (20–40) | 90 (80–120) |
| startup | peak RSS | 24.7 (22.7–26.8) | 33.0 (28.6–33.1) | 19.5 (15.9–19.6) | 918.8 (908.8–920.9) |
| startup | steady RSS | 30.6 (30.3–30.8) | 33.0 (33.0–33.2) | 27.5 (27.4–27.6) | 918.8 (909.1–921.2) |
| dom-list | wall | 2.120 (2.113–2.147) | 1.424 (1.415–1.428) | 0.757 (0.754–0.784) | 5.900 (5.700–6.000) |
| dom-list | create | 0.440 (0.432–0.446) | 0.353 (0.341–0.360) | 0.218 (0.208–0.240) | 0.700 (0.700–0.800) |
| dom-list | update | 1.687 (1.678–1.714) | 1.067 (1.065–1.070) | 0.544 (0.534–0.550) | 5.100 (5.000–5.300) |
| dom-list | CPU | 30 (30–30) | 20 (20–30) | 10 (10–10) | 210 (180–210) |
| dom-list | peak RSS | 17.2 (17.1–17.5) | 19.9 (19.8–20.0) | 7.9 (7.9–8.2) | 937.3 (937.0–939.7) |
| dom-list | steady RSS | 19.1 (18.7–19.2) | 20.9 (20.7–21.1) | 16.3 (16.1–16.4) | 937.3 (937.0–939.7) |
| dom-list | checksum | 62890 | 62890 | 62890 | 62890 |
| layout | wall | 1.012 (1.008–1.022) | 0.763 (0.758–0.773) | 0.545 (0.538–0.548) | 10.500 (10.300–10.800) |
| layout | CPU | 50 (40–50) | 40 (40–40) | 20 (10–20) | 270 (260–280) |
| layout | peak RSS | 22.4 (22.2–22.4) | 21.7 (21.4–21.8) | 21.0 (20.8–21.0) | 977.2 (975.6–979.3) |
| layout | steady RSS | 24.2 (24.2–24.3) | 26.5 (26.1–26.6) | 21.6 (21.6–21.7) | 977.2 (975.6–979.3) |
| layout | checksum | 20016 | 20016 | 20016 | 20016 |
| layout-css | wall | 0.477 (0.468–0.486) | 0.366 (0.363–0.369) | 0.273 (0.269–0.277) | 1.100 (1.100–1.200) |
| layout-css | CPU | 50 (50–50) | 40 (40–40) | 30 (30–40) | 110 (80–120) |
| layout-css | peak RSS | 25.8 (24.5–26.6) | 30.6 (29.1–31.1) | 18.5 (18.1–18.8) | 922.8 (921.1–923.9) |
| layout-css | steady RSS | 29.1 (28.9–29.1) | 31.0 (30.8–31.2) | 26.0 (25.9–26.1) | 922.8 (921.4–924.2) |
| layout-css | checksum | 7680 | 7680 | 7680 | 7680 |
| frames | work p50 | 0.156 (0.154–0.157) | 0.130 (0.129–0.130) | 0.076 (0.074–0.078) | 0.200 (0.200–0.250) |
| frames | work p95 | 0.163 (0.162–0.168) | 0.137 (0.134–0.143) | 0.082 (0.080–0.090) | 0.300 (0.300–0.300) |
| frames | native pipeline p50 | 5.012 (4.965–5.044) | 3.601 (3.593–3.607) | 2.921 (2.900–2.930) | n/a |
| frames | browser rAF gap p50 | n/a | n/a | n/a | 16.700 (16.650–16.700) |
| frames | CPU | 170 (160–170) | 130 (120–130) | 100 (90–110) | 300 (270–310) |
| frames | peak RSS | 35.2 (35.1–35.3) | 37.9 (37.7–38.0) | 32.4 (32.3–32.5) | 925.8 (922.1–927.3) |
| frames | steady RSS | 35.3 (35.2–35.4) | 37.9 (37.8–38.1) | 32.5 (32.4–32.6) | 925.8 (922.1–927.3) |
| frames | checksum | 11000 | 11000 | 11000 | 11000 |
| compute | wall | 0.808 (0.808–0.816) | 0.810 (0.809–0.815) | 0.815 (0.813–0.816) | 1.400 (1.400–1.500) |
| compute | CPU | 10 (0–10) | 10 (10–10) | 0 (0–10) | 120 (100–140) |
| compute | peak RSS | 9.0 (8.4–9.3) | 10.1 (9.7–10.1) | 8.0 (7.9–8.1) | 918.8 (918.7–921.9) |
| compute | steady RSS | 15.5 (15.4–15.8) | 17.5 (17.2–17.7) | 13.0 (12.8–13.3) | 919.1 (919.0–922.2) |
| compute | checksum | -362647.901806622 | -362647.901806622 | -362647.901806622 | -362647.901806622 |
| memory | wall | 2.836 (2.816–2.973) | 2.257 (2.175–2.361) | 1.458 (1.435–1.468) | 19.300 (17.900–20.200) |
| memory | CPU | 10 (10–10) | 10 (0–10) | 0 (0–0) | 130 (110–150) |
| memory | peak RSS | 9.2 (9.0–9.4) | 9.9 (9.8–10.2) | 8.0 (8.0–8.2) | 940.0 (936.0–942.1) |
| memory | steady RSS | 18.0 (17.8–18.2) | 19.9 (19.8–20.0) | 15.1 (14.9–15.2) | 940.2 (936.0–942.4) |
| memory | checksum | 72000 | 72000 | 72000 | 72000 |
| css-list | wall | 1.209 (1.193–1.238) | 1.024 (0.990–1.063) | 0.685 (0.669–0.725) | 7.100 (4.600–8.500) |
| css-list | CPU | 10 (0–10) | 10 (0–10) | 0 (0–0) | 100 (80–110) |
| css-list | peak RSS | 9.2 (9.0–9.2) | 10.1 (9.9–10.2) | 8.2 (8.0–8.3) | 925.0 (920.9–926.5) |
| css-list | steady RSS | 16.9 (16.7–17.2) | 18.6 (18.4–18.7) | 14.0 (13.9–14.1) | 925.0 (920.9–926.5) |
| css-list | checksum | 18000 | 18000 | 18000 | 18000 |
| flex-list | wall | 0.575 (0.551–0.623) | 0.481 (0.471–0.499) | 0.358 (0.347–0.364) | 3.100 (3.000–5.000) |
| flex-list | CPU | 10 (10–10) | 10 (0–10) | 0 (0–0) | 90 (60–120) |
| flex-list | peak RSS | 9.2 (9.0–9.3) | 10.1 (9.8–10.3) | 7.9 (7.7–8.1) | 920.3 (919.7–923.3) |
| flex-list | steady RSS | 16.6 (16.3–16.8) | 18.5 (18.3–18.6) | 13.8 (13.6–13.9) | 920.3 (919.7–923.3) |
| flex-list | checksum | 7200 | 7200 | 7200 | 7200 |
| events | wall | 0.238 (0.236–0.253) | 0.212 (0.211–0.216) | 0.103 (0.102–0.117) | 0.400 (0.300–0.400) |
| events | CPU | 10 (10–10) | 10 (10–10) | 0 (0–0) | 90 (80–110) |
| events | peak RSS | 9.2 (9.1–9.2) | 10.0 (9.9–10.2) | 7.9 (7.8–8.0) | 924.9 (924.3–926.5) |
| events | steady RSS | 16.7 (16.4–16.9) | 18.5 (18.5–18.7) | 13.6 (13.6–13.8) | 924.9 (924.3–926.2) |
| events | checksum | 400 | 400 | 400 | 400 |
| table-repaint | wall | 2.301 (2.196–2.308) | 1.855 (1.819–1.901) | 1.147 (1.079–1.210) | 4.900 (4.500–5.000) |
| table-repaint | CPU | 70 (60–70) | 60 (50–60) | 40 (40–50) | 100 (100–140) |
| table-repaint | peak RSS | 30.5 (30.3–31.0) | 28.7 (28.3–32.6) | 28.3 (28.1–28.5) | 928.0 (926.2–929.5) |
| table-repaint | steady RSS | 30.8 (30.8–31.2) | 33.9 (33.8–33.9) | 28.7 (28.5–28.9) | 928.2 (926.4–929.7) |
| table-repaint | checksum | 5704 | 5704 | 5704 | 5704 |
| styled-scroll | wall | 1.313 (1.298–1.324) | 1.065 (1.046–1.096) | 0.737 (0.730–0.771) | 4.400 (4.000–6.400) |
| styled-scroll | CPU | 10 (0–10) | 10 (10–10) | 0 (0–0) | 100 (80–110) |
| styled-scroll | peak RSS | 9.1 (8.9–9.3) | 10.0 (9.7–10.2) | 8.0 (7.9–8.3) | 925.8 (924.3–927.8) |
| styled-scroll | steady RSS | 16.9 (16.9–17.2) | 18.6 (18.5–18.8) | 13.8 (13.8–14.0) | 926.1 (924.3–928.0) |
| styled-scroll | checksum | 650115 | 650115 | 650115 | 650115 |
| text-layout | wall | 0.171 (0.170–0.177) | 0.137 (0.132–0.142) | 0.084 (0.082–0.089) | 2.000 (1.900–2.300) |
| text-layout | CPU | 50 (50–60) | 50 (40–50) | 30 (30–40) | 110 (90–120) |
| text-layout | peak RSS | 25.2 (24.5–26.0) | 29.5 (29.3–30.0) | 18.4 (18.2–18.5) | 924.1 (923.3–926.3) |
| text-layout | steady RSS | 28.5 (28.4–28.7) | 30.8 (30.7–31.0) | 25.9 (25.7–25.9) | 924.4 (923.3–926.6) |
| text-layout | checksum | 34320 | 34320 | 34320 | 34320 |

Native first raster is inside native cold start and has no Chromium sample.
The native pipeline is the rAF callback plus tiny-skia. The browser rAF gap
includes headless scheduling, so those two rows stay separate.

Frame CPU on the size profile is 170 ms (160–170) against Chromium 300 ms
(270–310). The size maximum, 170 ms, is under 270 ms. On `09c29fc` that
maximum was 190 ms. Checksum 11000 matched. Size pipeline p50 is 5.012 ms
(4.965–5.044), against 6.233 ms on `09c29fc`. Speed is 3.601 ms
(3.593–3.607) and max is 2.921 ms (2.900–2.930).

Author-CSS CPU on the size profile is flat at 50 ms against Chromium 110 ms
(80–120). The size maximum is under this series' 80 ms minimum. On
`09c29fc` the size range was 50–60 ms. Checksum 7680 matched.

DOM update on the size profile is 1.687 ms (1.678–1.714) against Chromium
5.100 ms (5.000–5.300). On `09c29fc` the size median was 2.147 ms. Checksum
62890 matched. DOM wall is 2.120 ms (2.113–2.147) against 5.900 ms
(5.700–6.000).

Startup cold start on the size profile is 52.836 ms (51.432–73.417) against
202.317 ms (194.003–322.253). Steady RSS is 30.6 MB (30.3–30.8) against
918.8 MB (909.1–921.2). The `09c29fc` size median was 54.301 ms and steady
RSS was 30.4 MB.

`table-repaint` wall is 2.301 / 1.855 / 1.147 ms against 4.900 ms
(4.500–5.000). The size maximum, 2.308 ms, is under 4.500 ms. Checksum
5704 matched. `styled-scroll` wall is 1.313 / 1.065 / 0.737 ms against
4.400 ms (4.000–6.400). The size maximum, 1.324 ms, is under 4.000 ms.
Checksum 650115 matched. `text-layout` wall is 0.171 / 0.137 / 0.084 ms
against 2.000 ms (1.900–2.300). The size maximum, 0.177 ms, is under
1.900 ms. Checksum 34320 matched.

## Profile comparison (`09c29fc`)

Pinned host: Linux 6.12.94+, 4 vCPU Intel Xeon, no `/dev/dri`. Rustc 1.99.0
(`b940084d7`, 2026-09-28). Google Chrome 154.0.8037.92. The binaries are
commit `09c29fc6d4fba30214d5bd70c93a5b747963896b`. `workloads.ts` sha256 is
`7d002a9c0d5c880a03af4a5dee06efaae40680a0dd42c1718cfb868d1788b05e`. Five
fresh processes per workload. One Chromium series is shared by all three
profiles. Paired checksums agreed on every run.

Headless Chrome 154 resolves `performance.now()` in 0.1 ms steps. A probe
that spun until the clock moved saw only deltas of 0.1 ms. The `d709bf9`
DOM update did 40 text writes and moves. Every Chromium sample of that
loop was 0.000 ms, which is the timer bucket, not zero work. The same
Chrome script at 8000 updates took about 5.9–6.6 ms and summed to checksum
62890. `DOM_UPDATES` is now 8000 in the shared script, so both engines time
the same longer loop. The `d709bf9` section below is that older script
(`DOM_UPDATES = 40`, checksum 230).

The 8000-update loop is one guarded rewrite when the nodes are still a
dense element array, the text prefix is ASCII, and no mutation observer,
custom element, own `textContent` or `appendChild`, or tree-order selector
is present. Anything else runs the ordinary property path and does not
mutate early. `native_dom_update_loop_is_rewritten` requires the intrinsic
and the fallback. `indexed_text_rewrite_matches_the_ordinary_move` checks
the moved text against the unfused loop. `indexed_text_rewrite_refuses_without_mutating`
covers an own `textContent`, an own `appendChild`, a `:nth-child` rule,
and a non-ASCII prefix.

Author-rule text rows (`#css-list > div`) use `dom_build_text_guarded`.
The row height still comes from the stylesheet. Checksum 18000 is the
1000-row `offsetHeight`. `author_rule_text_rows_keep_stylesheet_height`
checks eight fused rows at 144 px.

Headless frames keep the component tree and the Taffy tree when only
scroll offsets and used-box geometry change. Inherited color, an appended
node, and a rejected patch rebuild. Absolute `top` on a relative parent
matches a fresh `compute_with_scroll_details`. Inset-only edits do not
remeasure text. Trees without floats, tables, or inline-block skip the
post-Taffy projection walk. The pixel checksum still reads every byte and
matches the previous modulo formula. Frame checksum 11000 is
`offsetHeight`, unchanged.

A win means the native maximum is strictly below the Chromium minimum.
Every comparable row in this table is a win, including the rows that
overlapped or lost on `d709bf9`.

### Binary size

Stripped `cpu-render` binaries. The instrumented max binary was used only
to collect the profile.

| profile | bytes | sha256 |
| --- | ---: | --- |
| size | 17959056 | `2a7aa34c30cc86da199b02cd8135d911a014119fc21a09003cb99b1cf1ca1583` |
| speed | 31191504 | `e51240ae7e0abc3e6457a88b05116b8b5ec769323c4afc8c242d7ba512371f6e` |
| max | 23010448 | `4d7c33f36f1b7efe84ea7b4c2bcacd0a85ee8366b86e2db23986159e46ed6396` |

The merged profdata is 23209528 bytes. The instrumented binary is 74557408
bytes, sha256
`3385a227dd3481b826c27f63a541fb4c68092a554ced9121e9d379de8e88601b`.

### Five-run table

Times are milliseconds, median (min–max) of the five process results. RSS
is `VmRSS` in MB. Checksums are the common value. The compute checksum is
`-362647.901806622` on every native and Chromium sample.

| workload | metric | size | speed | max | Chromium |
| --- | --- | ---: | ---: | ---: | ---: |
| startup | cold start | 54.301 (52.394–72.085) | 47.376 (46.163–69.448) | 35.331 (34.409–37.446) | 199.446 (193.615–338.676) |
| startup | native first raster | 39.971 (38.022–44.102) | 34.256 (33.141–40.647) | 26.900 (25.970–28.514) | n/a |
| startup | CPU | 50 (40–50) | 30 (30–40) | 20 (20–30) | 90 (60–90) |
| startup | peak RSS | 23.3 (22.7–27.8) | 32.8 (32.8–33.2) | 19.3 (19.3–19.5) | 918.3 (908.9–921.0) |
| startup | steady RSS | 30.4 (30.1–30.6) | 33.0 (32.9–33.2) | 27.0 (26.9–27.1) | 918.0 (908.9–921.4) |
| dom-list | wall | 2.568 (2.551–2.601) | 1.765 (1.738–1.782) | 0.912 (0.907–0.924) | 5.900 (5.700–6.200) |
| dom-list | create | 0.440 (0.435–0.450) | 0.366 (0.355–0.371) | 0.218 (0.213–0.223) | 0.700 (0.700–0.800) |
| dom-list | update | 2.147 (2.108–2.159) | 1.394 (1.385–1.406) | 0.694 (0.689–0.699) | 5.200 (4.900–5.400) |
| dom-list | CPU | 30 (30–40) | 20 (20–30) | 10 (10–10) | 190 (170–220) |
| dom-list | peak RSS | 16.8 (16.7–17.1) | 19.8 (19.4–19.9) | 8.1 (7.9–8.3) | 939.4 (938.7–940.6) |
| dom-list | steady RSS | 18.8 (18.7–19.1) | 20.8 (20.8–21.1) | 15.8 (15.7–15.9) | 939.4 (938.7–940.6) |
| dom-list | checksum | 62890 | 62890 | 62890 | 62890 |
| layout | wall | 1.019 (1.003–1.021) | 0.767 (0.765–0.787) | 0.540 (0.535–0.573) | 10.500 (10.400–10.600) |
| layout | CPU | 50 (50–50) | 40 (30–40) | 20 (10–20) | 260 (250–290) |
| layout | peak RSS | 22.5 (22.3–22.6) | 25.0 (21.6–25.9) | 20.9 (20.2–21.1) | 977.5 (975.9–978.2) |
| layout | steady RSS | 24.3 (24.1–24.5) | 26.3 (26.1–26.6) | 21.5 (21.3–21.5) | 977.5 (975.9–978.2) |
| layout | checksum | 20016 | 20016 | 20016 | 20016 |
| layout-css | wall | 0.479 (0.477–0.498) | 0.366 (0.363–0.376) | 0.278 (0.275–0.286) | 1.200 (1.100–1.500) |
| layout-css | CPU | 50 (50–60) | 40 (40–50) | 30 (30–40) | 120 (90–140) |
| layout-css | peak RSS | 23.9 (23.2–24.7) | 29.2 (28.4–30.4) | 18.0 (17.8–18.1) | 922.1 (920.6–923.1) |
| layout-css | steady RSS | 28.7 (28.6–28.9) | 31.1 (31.0–31.3) | 25.4 (25.3–25.6) | 922.3 (920.6–923.1) |
| layout-css | checksum | 7680 | 7680 | 7680 | 7680 |
| frames | work p50 | 0.169 (0.169–0.172) | 0.137 (0.134–0.138) | 0.082 (0.082–0.084) | 0.200 (0.200–0.200) |
| frames | work p95 | 0.183 (0.174–0.187) | 0.149 (0.142–0.154) | 0.092 (0.088–0.099) | 0.300 (0.300–0.300) |
| frames | native pipeline p50 | 6.233 (6.209–6.249) | 4.308 (4.270–4.331) | 3.393 (3.375–3.430) | n/a |
| frames | browser rAF gap p50 | n/a | n/a | n/a | 16.700 (16.650–16.700) |
| frames | CPU | 190 (190–190) | 140 (130–140) | 110 (110–110) | 290 (270–290) |
| frames | peak RSS | 34.7 (34.5–35.1) | 37.4 (37.3–37.6) | 31.5 (31.2–31.8) | 926.7 (922.7–928.4) |
| frames | steady RSS | 34.7 (34.6–35.1) | 37.6 (37.4–37.8) | 31.6 (31.4–31.9) | 926.7 (922.7–928.4) |
| frames | checksum | 11000 | 11000 | 11000 | 11000 |
| compute | wall | 0.810 (0.810–0.816) | 0.816 (0.815–0.816) | 0.815 (0.810–0.816) | 1.400 (1.400–1.400) |
| compute | CPU | 10 (0–10) | 10 (0–10) | 0 (0–10) | 100 (90–130) |
| compute | peak RSS | 9.1 (8.8–9.2) | 9.9 (9.4–9.9) | 7.9 (7.9–8.4) | 920.5 (918.9–923.5) |
| compute | steady RSS | 15.6 (15.3–15.6) | 17.6 (17.3–17.6) | 12.7 (12.6–13.0) | 920.8 (919.2–923.8) |
| compute | checksum | -362647.901806622 | -362647.901806622 | -362647.901806622 | -362647.901806622 |
| memory | wall | 2.829 (2.713–2.855) | 2.194 (2.172–2.333) | 1.452 (1.435–1.491) | 19.300 (18.100–20.600) |
| memory | CPU | 10 (0–10) | 10 (0–10) | 0 (0–0) | 130 (100–140) |
| memory | peak RSS | 9.0 (8.8–9.0) | 9.7 (9.5–9.9) | 8.1 (7.9–8.4) | 940.9 (940.1–942.3) |
| memory | steady RSS | 17.6 (17.5–17.9) | 19.7 (19.6–19.9) | 14.8 (14.7–14.9) | 941.2 (940.3–942.5) |
| memory | checksum | 72000 | 72000 | 72000 | 72000 |
| css-list | wall | 1.237 (1.236–1.277) | 1.058 (1.017–1.079) | 0.718 (0.715–0.727) | 5.200 (4.600–5.700) |
| css-list | CPU | 0 (0–10) | 10 (0–10) | 0 (0–0) | 110 (100–110) |
| css-list | peak RSS | 8.9 (8.7–9.7) | 9.7 (9.5–10.0) | 8.0 (7.9–8.0) | 922.8 (920.5–923.5) |
| css-list | steady RSS | 16.8 (16.7–17.0) | 18.9 (18.4–18.9) | 13.7 (13.6–13.9) | 923.1 (920.5–923.5) |
| css-list | checksum | 18000 | 18000 | 18000 | 18000 |
| flex-list | wall | 0.577 (0.564–0.600) | 0.503 (0.479–0.541) | 0.363 (0.354–0.370) | 3.200 (3.000–3.900) |
| flex-list | CPU | 0 (0–10) | 10 (0–10) | 0 (0–0) | 110 (80–130) |
| flex-list | peak RSS | 8.8 (8.8–9.1) | 9.7 (9.6–10.0) | 8.0 (7.8–8.1) | 919.0 (917.3–920.2) |
| flex-list | steady RSS | 16.3 (16.2–16.4) | 18.6 (18.2–18.6) | 13.3 (13.2–13.5) | 919.0 (917.3–920.4) |
| flex-list | checksum | 7200 | 7200 | 7200 | 7200 |
| events | wall | 0.233 (0.231–0.245) | 0.201 (0.197–0.204) | 0.099 (0.099–0.137) | 0.400 (0.300–0.400) |
| events | CPU | 10 (10–10) | 10 (0–10) | 0 (0–0) | 90 (80–100) |
| events | peak RSS | 8.9 (8.9–9.2) | 9.7 (9.6–10.0) | 8.0 (7.9–8.3) | 926.1 (923.9–926.3) |
| events | steady RSS | 16.4 (16.4–16.7) | 18.5 (18.5–18.7) | 13.5 (13.3–13.6) | 926.1 (923.9–926.3) |
| events | checksum | 400 | 400 | 400 | 400 |

Native first raster is inside native cold start and has no Chromium sample.
The native pipeline is the rAF callback plus tiny-skia. The browser rAF gap
includes headless scheduling, so those two rows stay separate.

DOM update is 2.147 / 1.394 / 0.694 ms against Chromium 5.200 ms
(4.900–5.400). The size maximum, 2.159 ms, is under 4.900 ms. Checksum
62890 matched. DOM wall follows: size 2.568 ms (2.551–2.601) against
5.900 ms (5.700–6.200). Create stays a win at 0.440 ms (0.435–0.450)
against 0.700 ms (0.700–0.800).

Frame CPU is 190 / 140 / 110 ms against 290 ms (270–290). Size is flat at
190 ms, under Chromium's 270 ms minimum. Checksum 11000 matched. Work p50
and p95 stay wins. Size pipeline p50 fell from 18.788 ms on `d709bf9` to
6.233 ms.

CSS-list wall is 1.237 / 1.058 / 0.718 ms against 5.200 ms (4.600–5.700).
The size maximum, 1.277 ms, is under 4.600 ms. Checksum 18000 matched.

Author-CSS CPU is 50 / 40 / 30 ms against 120 ms (90–140). The size
maximum, 60 ms, is under this series' 90 ms Chromium minimum and under the
100 ms minimum recorded on `d709bf9`. Checksum 7680 matched. Wall stays a
win at 0.479 ms (0.477–0.498) against 1.200 ms (1.100–1.500).

## Profile comparison (`d709bf9`)

This run's script set `DOM_UPDATES` to 40 (checksum 230). The section above
is the current script.

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
