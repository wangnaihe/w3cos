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
- one width edit, one background edit, one text edit, one geometry
  class toggle, and one child insert and remove, against the retained layout
- CPU raster of a 240-row scrolling list
- JS `Value` calls into `createElement`, `setAttribute`, `textContent`,
  `appendChild`, and `getAttribute`

The process prints one JSON object and exits non-zero if a scenario does
not change the DOM or layout it claims to measure.

## CI

`.github/workflows/ci.yml` job `runtime-bench` builds the same binary, runs
`--samples 3`, checks that the tree has at least 5000 nodes, that medians
are positive, that each edit changes the box it claims to change, that a
one-node edit is faster than four times the same run's full style-plus-layout
median, and that the width, text, class, insert, and remove edits lay out a
subtree (`solved_nodes * 4 < nodes`) rather than the whole tree.
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
- Layout time and the layout checksum did not move. The width edit in this
  table still walked the retained tree. The subtree path below is a later
  measurement.
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

## Incremental subtree relayout

A later pass stopped walking the whole 8201-node tree after a local edit.
Numbers below are five samples on one machine, `runtime-bench` profile,
viewport `1280×720`. The before column for width and background is the
harness binary from before this pass (`/tmp/runtime-bench-before.json`).
Text, class, insert, and remove were not in that binary. Their before
column is the same new binary with `W3COS_SUBTREE_RELAYOUT=0`, which keeps
the previous flush. Medians, then the five samples.

`LayoutEngine::compute` on the retained tree was the width cost. With
`W3COS_LAYOUT_PHASES=1` on that fallback, five width edits of 8201 nodes
spent about 175–192 µs flattening, 3900–3991 µs in `compute`, and about
65 µs writing rects, one compute pass. Taffy already skips clean subtrees.
The walk that still touched every node was the text-height update and the
layout collect inside that compute.

| Scenario | Before | After |
| --- | ---: | ---: |
| One-node width edit | 4.02 ms | 0.254 ms |
| One-node background edit | 0.275 ms | 0.241 ms |
| Text edit in a column cell | 50.5 ms | 0.220 ms |
| `.wide` class toggle | 4.44 ms | 0.231 ms |
| Insert a cell in a column | 52.4 ms | 0.385 ms |
| Remove that cell | 53.0 ms | 0.362 ms |

Samples, microseconds:

- Width before `[4392, 4017, 3930, 3946, 4031]`, after `[603, 276, 254, 246, 244]`.
  Used width 1260 px to 236 px in both. After solves 2 nodes (`style-subtree`).
  Height did not change, so no follower was translated.
- Background before `[275, 246, 245, 322, 290]`, after `[246, 241, 237, 241, 242]`.
  Kind `paint`. The sibling box did not move.
- Text before `[54533, 51048, 49996, 50500, 50407]` (kind `rebuild`, 8201 nodes),
  after `[296, 221, 210, 213, 220]` (kind `splice`, 2 solved, 4156 shifted).
  Cell height 24.8 px to 360.8 px.
- Class before `[5892, 4544, 273, 4441, 260]`. The 273 µs and 260 µs samples
  are full-tree width-memo hits of the repeated 180 px value. The other three
  are retained computes of 8201 nodes. After `[322, 231, 224, 251, 228]`
  (kind `style-subtree`, 2 solved). Used width 1260 px to 188 px. The last
  sample is the wide state because the run has an odd sample count.
- Insert before `[50790, 52031, 57554, 52422, 52724]` (rebuild, 8203 nodes),
  after `[465, 398, 385, 380, 384]` (splice, 43 solved, 4137 shifted).
- Remove before `[52278, 52522, 53029, 52966, 53630]` (rebuild, 8201 nodes),
  after `[368, 359, 360, 384, 362]` (splice, 41 solved, 4137 shifted).

Cold style-plus-layout on this machine stayed in the same band (before
median 75.8 ms, layout 48.4 ms; after-run median 76.3 ms, layout 49.4 ms).
Layout checksum `18294915160932` matched. Scroll checksum `2683200085`
matched. This pass does not change the cold layout.

What changed:

- A geometry edit whose parent is a block or a safe column flex (no wrap,
  no gap, flex-start, every item `flex-grow: 0`) lays out that element in
  the parent's content box and pins the border-box origin. Following
  siblings move only when the border-box height changes. A flex or grid
  parent that is not a vertical stack is solved as a container when its
  used width is definite and the container is at most 512 nodes.
- A text edit or a child insert/remove under that column flex uses the
  existing block splice. The same edit under a definite-width flex or grid
  re-solves that container and shifts what follows. `ancestor_levels` used
  to reject every flex and grid parent, so those edits rebuilt the document.
- Splice no longer walks all 8201 nodes to decide whether text must be
  measured. A definite-height ancestor still suppresses that measure, which
  is what the chat-log splice tests require.
- The retained Taffy tree is invalidated after a subtree write, and the
  whole-tree width memo is cleared, so a later full compute cannot restore
  stale rects.

Unit tests lay out a fresh tree after a column-flex width, text, append,
and remove; a grid text, append, and remove; a width class toggle; and a
row-flex width edit. Every host rect must match within 0.05 px.

## Still open

Cold layout of the 8201-node tree is still about 49 ms on this machine and
is mostly text measurement and Taffy. A flex or grid container above 512
nodes, a wrapping or gapped column, center or flex-end alignment, and any
tree with a float, absolute, fixed, or sticky box still takes the retained
or full rebuild path. A height change still adds `dy` to every following
host; that translation is counted in the text and insert numbers above.
Scroll-list paint and host-call throughput did not move in this harness.
Escape analysis and JIT remain deferred until correctness and Web API
conformance land.
