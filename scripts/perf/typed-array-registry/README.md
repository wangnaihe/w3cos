# Typed-array registry engine API benchmark

For the recorded dependency versions, first copy
`docs/performance/typed-array-registry-data/benchmark-Cargo.lock` to
`scripts/perf/typed-array-registry/Cargo.lock`.
Build with `cargo build --locked --release --manifest-path scripts/perf/typed-array-registry/Cargo.toml`.
Use a separate `CARGO_TARGET_DIR` for each worktree/configuration; inspect build
logs/source paths and save each executable before comparison. Shared workspace
target directories can accidentally reuse another worktree's test artifacts.
Run the resulting `typed-array-registry-bench MODE VIEWS ITERATIONS` executable.
Each invocation is **one independent sample/process**. CSV output has no header:
`mode,views,iterations,nanoseconds,checksum`.

Modes:
- `read`: ordinary array indexed reads while holding unrelated typed views.
- `method`: ordinary array push/pop pairs, with fixed final length.
- `ordinary`: 64 ordinary rows; indexed read/write/read plus push/pop, while
  unrelated 32-byte typed views stay live.
- `mixed`: the same row processing, also reading distributed typed payloads.
  This checks the complementary ordinary miss / typed hit paths.
- `construct`: allocate ITERATIONS 32-byte typed views with VIEWS preexisting
  unrelated views. Startup fixtures and result validation are outside timing.
- `churn`: allocate and read VIEWS typed arrays, drop all local page values,
  reset the page arena, then read an ordinary array; repeat ITERATIONS epochs.
  Timing includes all of those steps. The engine's preexisting global backing
  buffer retention is not fixed or hidden by this benchmark. Churn gains include
  faster per-view reads, not only teardown.
- `lifecycle`: same allocate/drop/reset/first-ordinary-read cycle, but only count
  allocated views instead of reading each typed view. This isolates allocation
  and teardown from the per-view read benefit.

Examples: run ordinary/mixed/read/method for 0, 1, 64, and 1024 views with 20000
iterations. Run construct with 0 or 1024 background views and 1024 allocations;
run churn with 64 or 1024 views and 16 epochs. Tune iteration counts identically
between compared revisions, use several alternating baseline/candidate fresh
processes, and report medians plus range rather than a single sample.

Every sample validates an independently computed checksum. Ordinary rows must
also retain length 2. Do not run repeated samples in one process without reset:
interned page values remain alive until arena reset, changing the live-view
count. This measures public engine API calls, not parsed JavaScript, browser
page load, rendering, or an end-to-end application speedup. Report no/few-view
controls and construction/churn costs alongside any large-registry gains.

## Allocation controls (separate build)

Build with `--features allocation-counting` into a **different target directory**.
Run `typed-array-registry-bench memory VIEWS 1`. Output is
`memory,views,before_live,held_live,peak_live,after_reset_live,held_delta,after_reset_delta`
in bytes. The counters wrap Rust's System allocator and measure requested live
allocation bytes, not RSS, allocator metadata, or fixed thread-local registry
metadata. Compare identical main/candidate
builds at 0, 1, 64, 1024, and optionally 16384 views. Subtracting the 0-view control
helps separate fixed thread-local initialization costs. Held memory includes all
engine state, not just registry keys. After-reset measurements perform one plain
array read to trigger lazy cleanup and include that tiny new array; existing
buffer retention remains included. Allocation instrumentation is disabled in the
default build; do not use allocation-counting builds for timing comparisons.

## Paired runner

`python run_pairs.py /path/to/baseline /path/to/candidate /path/to/output`
executes five alternating-order pairs per default case, never concurrently,
and records binary SHA-256, raw samples, median/min/max, and speedup ratios.
Use `--controls-only --steady-iterations 200000` for longer no/few-view controls.
Use `--lifecycle-controls` for 8192-view construction and 16-epoch lifecycle
controls. Use `--composition-controls` for longer no/few-view controls plus
64/1024-view mixed workloads when checking another PR combination.
`--mixed-composition-controls` selects only mixed 0/1 views at 200000 iterations
and mixed 64/1024 views at 20000 iterations for a focused composition check.
`--registered-controls` selects mixed 1/4/16/64/1024 views at 20000 iterations
to compare two candidate designs without hiding registered-path tradeoffs.
Use `--small-controls` for 4/16-view 200000-iteration workloads, or
`--construction-controls --pairs 11` for the 1024-background/8192-new-view
construction control. The summary also reports within-pair speedup median/range
and number of candidate-faster pairs, avoiding ratio-of-medians alone. Keep output outside the repository and compare the same harness
source/compiler flags on both revisions. Tiny no-view differences with strongly
overlapping sample ranges should not be described as a reliable improvement or
regression.

For a bounded no-view investigation, `python run_no_view.py BASELINE CANDIDATE
OUTPUT` runs ordinary/mixed with one million iterations and eleven pairs. It
pins the runner and both child variants to the same first permitted CPU where
supported, records the actual affinity (or failure), alternates both case and
variant order, and saves every paired direction. This does not reserve a CPU or
eliminate other host activity; use the ranges and paired outcomes, not only
ratios of aggregate medians.

`python run_lifecycle_investigation.py MAIN INITIAL_INDEX FINAL_WRAPPER OUTPUT`
is a single bounded eleven-round investigation for lifecycle/constructor costs.
It pins all three existing binaries to one permitted CPU, rotates variant order
(including reverse rotations), alternates case order, and reports all three
pairwise comparisons. The cases are 1024 live background views plus 8192 new
allocations, and sixteen lifecycle epochs of 1024 views. Preserve earlier
negative controls; this is attribution evidence, not a repeat-until-green loop.
