# Runtime speed

`w3cos-runtime-bench` measures style, layout, paint, and host-call cost on
one machine. Numbers below are from that harness. They are not a budget.

## Run

```sh
cargo build --profile runtime-bench -p w3cos-runtime --bin w3cos-runtime-bench --no-default-features --features cpu-render
target/runtime-bench/w3cos-runtime-bench --output runtime-bench.json --samples 3
```

The `runtime-bench` profile matches release optimization (`opt-level` 3)
with LTO off, 16 codegen units, and symbols kept. CPU raster (`cpu-render`,
no Skia) is what CI runs. `--samples` repeats the in-process scenarios.
Startup is a child process, so its wall time includes process spawn and the
first fontconfig load.

Scenarios:

- cold startup through the first painted frame of a small page
- style recalc and layout of 8201 flex/grid/text nodes
- one width edit, then one background edit, against the retained layout
- CPU raster of a 240-row scrolling list
- JS `Value` calls into `createElement`, `setAttribute`, `textContent`,
  `appendChild`, and `getAttribute`

The process prints one JSON object and exits non-zero if a scenario does
not change the DOM or layout it claims to measure.

## CI

`.github/workflows/ci.yml` job `runtime-bench` builds the same binary, runs
`--samples 3`, checks that the tree has at least 5000 nodes, that medians
are positive, that the width edit changes used width, and that a one-node
edit is faster than four times the same run's full style-plus-layout median.
The job uploads `runtime-bench.json` as the `runtime-bench` artifact. Absolute
microseconds are not a gate.

## Profile

A flat `perf` sample of the harness (999 Hz, `runtime-bench` profile) put
the largest share of style time in `Document::computed_style_impl`, string
map lookups, `Style` clones, and `Atom::as_static`. Layout time sat in text
leaf heights, Taffy flexbox, and edge resolution. Host calls were already
about 0.5 µs and were not the top cost. `/usr/bin/perf` on this kernel may
refuse to start; `/usr/lib/linux-tools/6.8.0-31-generic/perf` sampled the
binary.

## Before / after

Same binary name, profile, flags, and viewport (`1280×720` for style and
layout, `800×600` for the scrolling list). Three samples. Medians.

The before run is the first successful harness run on this machine. The
after run is a later warm repeat after the changes below. A second warm
repeat landed in the same band (style 28.7 ms, style+layout 81.6 ms).
Layout checksum `18294915160932`, scroll checksum `1786374846`, and startup
paint checksum `150112521` matched before and after.

| Scenario | Before | After |
| --- | ---: | ---: |
| Style recalc, 8201 nodes | 76.3 ms | 29.6 ms |
| Layout, same tree | 55.5 ms | 55.2 ms |
| Style + layout | 131.8 ms | 84.7 ms |
| One-node width edit | 4.42 ms | 4.57 ms |
| One-node background edit | 0.272 ms | 0.232 ms |
| Scroll-list first frame | 6.80 ms | 7.90 ms |
| Scroll-list later frame | 2.26 ms | 2.29 ms |
| JS to DOM host call | 462 ns | 452 ns |
| Startup wall (child process) | 333 ms | 42.0 ms |

The width edit changed the target's used width from 1260 px to 202 px in
both runs. The background edit did not move the sibling box.

What the table supports:

- Style recalc dropped because identical elements no longer miss the
  matched-properties cache solely due to a unique `id`. Selector matches
  that depend on `id` stay in the matched-rule list. `dir` is still part
  of the key, and a unit test checks that `dir="rtl"` does not share
  direction with an otherwise identical element.
- Layout time and the layout checksum did not move. The width edit still
  relayouts.
- Background, scroll, and host-call deltas sit inside run noise. The
  background path does skip a Taffy solve when every dirty style differs
  only by paint fields (background, opacity, border colors, radii,
  shadows, outlines, `z-index`). Color and font still rebuild, because
  descendants copy them when the component tree is built.
- Startup wall time is the first fontconfig load. A warm child on the
  before binary was already tens of milliseconds. This pass does not skip
  system font setup.

Other changes on the same paths, kept because they remove work the profile
showed and because the tests lock the old behavior:

- Declarations without `var()` are not copied into a new `String` before
  `set_property`.
- `::before`, `::after`, `::first-line`, and `::first-letter` matching
  returns immediately when no rule uses that pseudo-element.
- Tag rule buckets are visited in place. A tag is lowercased only when it
  contains an uppercase byte.
- The text paint cache looks up by borrowed text, so a hit does not
  allocate the cache key.
- A local inline edit (width, background, and the other non-inherited
  properties) no longer drops every element's matched-rule memo, unless a
  container query or `[style]` selector is registered.

## Still open

Layout of the 8201-node tree is about 55 ms and is mostly text measurement
and Taffy. Scroll-list paint and host-call throughput did not move in this
harness. Escape analysis and JIT remain deferred until correctness and Web
API conformance land.
