# Native/browser performance validation

## Current status

The baseline reviewed for this work is commit
`480741e9a50a075a293a0a6c533e383c2e27f51b`. It has native CPU-frame telemetry,
correctness tests, and a small layout microbenchmark, but no reproducible paired
native/browser performance gate. This document specifies the broader missing gate. A runnable two-scenario DOM
script comparison now exists in `tools/perf-paired`; no native/browser comparison
has passed or even completed in the cloud environment.

The lazy ancestor-filter patch removes unnecessary ancestor traversal and
hashing when no surviving stylesheet candidate needs ancestor features. It
retains the existing full selector matcher, candidate selection, cascade order,
and ancestor Bloom filter. The lazily initialized filter lives for one matching
operation, so it cannot survive a DOM mutation. Focused before/after measurements are recorded below. End-to-end application
and browser-comparison effects remain unmeasured.

## Validate the focused patch

After installing a Rust toolchain and resolving the same dependency versions
for both revisions:

```sh
cargo test -p w3cos-dom --lib
cargo clippy -p w3cos-dom --all-targets --no-deps -- -D warnings
cargo fmt --all -- --check
cargo test -p w3cos-dom --release ancestor_filter_microbenchmark -- --ignored --nocapture
```

The ignored microbenchmark reports all five repetitions for each of four
candidate-filter scenarios at depths 0, 32, and 256. Run the same benchmark-only
addition on the unoptimized baseline, as well as on the patched revision. Use
the same lockfile, toolchain, machine, build profile, and CPU settings. Alternate
baseline and patched runs after warmup and retain every result. Inspect all
scenarios, including ancestor-dependent hits and misses, for regressions.

This diagnostic measures an internal selector/cascade operation. Its values
are neither browser comparison results nor frame timing. Run the repository's
required workspace/CI checks separately before accepting the patch. A focused
test pass does not establish a full workspace pass.

## Paired browser gate contract

Define each scenario before running it, with the same authored application,
fixture data, DOM/CSS, fonts/assets, viewport in CSS and physical pixels, DPR,
input sequence, final state, and output correctness expectations. Record the
native commit and compiler settings, browser version and flags, OS, CPU/GPU,
driver, renderer/backend, power/thermal state, and dependency lockfile digest.
Browser and native runs must share the measurement host.

An initial explicit scenario matrix should cover:

- Cold start and first correct rendered frame, separately from warm start
- Static flex/grid layout and text shaping, including CJK/RTL and font fallback
- Simple-selector cascade on deep trees and sparse/no-candidate stylesheets
- Ancestor, sibling, relational, and pseudo-element selectors with mutations
- Large-list scrolling, incremental updates, insertion/removal, and reorder
- Keyboard/text input and pointer interactions through resulting presentation
- Continuous animation and canvas/image-heavy rendering
- Peak and steady-state memory for the same completed workloads

Select concrete fixture sizes and expected outputs for each case. Add other
supported application scenarios explicitly; an unlisted or unsupported scenario
is untested, never implicitly passing. Conformance failures and mismatched
outputs block performance comparisons rather than counting omitted work as a
speedup.

For every scenario and metric:

1. Validate outputs/state and confirm all scripted actions were consumed.
2. Separate warmup from measurement and alternate paired run order.
3. Save raw per-run samples, time units, repetitions, sample counts, and errors.
4. Report medians, tail latency, and uncertainty for the paired native/browser
   ratio. Use a predeclared run count and statistical method; do not stop as soon
   as a favorable sample appears.
5. Require a predeclared margin below 1 for the ratio's upper confidence bound
   to declare native faster. An interval crossing the threshold is inconclusive.
6. Evaluate every required scenario independently. A geometric mean or an
   overall score must never hide a slower, missing, or incorrect scenario.

Keep `pass`, `fail`, `inconclusive`, `blocked`, and `not-run` distinct. Only call
the specified matrix faster when every required comparison passes. Finite
coverage cannot establish superiority for every possible application.

## Existing telemetry limitations

`w3cos-runtime/src/perf.rs` measures continuous main-thread CPU wall time from
the redraw rendering opportunity through post-paint work. It excludes time in
the input queue before that opportunity. `phase_sum` is a diagnostic subtotal,
not frame latency; paint timing includes CPU submission, not GPU completion.
Display presentation timestamps are explicitly unavailable. Do not compare
these values with browser rAF interval or input-to-display latency.

The existing summary retains only the last 240 samples, does not export raw
samples, and labels desktop viewport as `1200x800` rather than observing actual
dimensions/DPR. A summary requested during a frame can contain its unfinished
zero-valued CPU sample. Before implementing the paired frame gate, use completed
samples, instrument actual viewport metadata, retain the measurement window,
and define equivalent intervals on both sides. The existing 16.7 ms budget is
a 60 Hz-oriented diagnostic, not proof of performance at other refresh rates.

## Focused measurement results (2026-10-12)

Measured on shared cloud Linux x86-64 using Rust 1.99.0, release optimization,
the same dependency lockfile, eight alternating paired process runs and five
samples per case per process (40 samples per revision/case). Each sample uses
1,000,000 iterations at depth 0, 200,000 at depth 32, or 30,000 at depth 256.
The table gives medians across those samples, not a confidence-based pass/fail
gate. CPU frequency and host contention were not controlled.

| Scenario | Ancestor depth | Baseline ns/op | Patched ns/op | Baseline / patched |
| --- | ---: | ---: | ---: | ---: |
| ancestor-hit | 0 | 98.13 | 97.30 | 1.01x |
| ancestor-miss | 0 | 100.46 | 97.34 | 1.03x |
| no-candidates | 0 | 76.64 | 75.90 | 1.01x |
| simple | 0 | 177.03 | 176.81 | 1.00x |
| ancestor-hit | 32 | 635.58 | 650.82 | 0.98x |
| ancestor-miss | 32 | 551.09 | 546.50 | 1.01x |
| no-candidates | 32 | 532.07 | 74.94 | 7.10x |
| simple | 32 | 632.82 | 170.93 | 3.70x |
| ancestor-hit | 256 | 3850.55 | 3769.04 | 1.02x |
| ancestor-miss | 256 | 3721.10 | 3705.03 | 1.00x |
| no-candidates | 256 | 3760.03 | 72.34 | 51.98x |
| simple | 256 | 3895.10 | 165.12 | 23.59x |

The clear gains are in deep context cascades with simple selectors or no
candidates. The depth-32 ancestor-hit median is about 2.4% slower; its paired
ratios straddle 1. Other small differences also have substantial run-to-run
variation. This is not an all-scenario non-regression certificate. These cases
exercise the context entrypoint only: do not extrapolate these speedups to
whole applications, node/pseudo cascades, rendering, or browser comparisons.

Verification on the initial lazy-filter patch (before the later build recovery):

- All four added correctness tests pass (empty filters, lazy/eager equivalence,
  single construction, class mutations and reparenting including pseudo styles).
- DOM suite: 715 passed, 8 failed, 2 ignored. The same eight failures reproduce
  on the baseline comparison checkout; the suite is not clean.
- Strict DOM clippy: 102 errors, with diagnostic-title counts matching baseline.
- Workspace formatting reports 853 existing differences; added code is formatted.
- `git diff --check` passes.
- At that checkpoint, workspace check was blocked at `yeslogic-fontconfig-sys`
  by missing fontconfig development files. Later authorized workspace-only
  dependency extraction resolved this blocker; see the subsequent results below.
- No browser comparison, full workspace test pass, GPU/display timing, or
  native-all-scenarios superiority has been established.

The accompanying evidence bundle retains the raw paired samples, toolchain and
lockfile metadata, diagnostic logs, exact patch, baseline benchmark additions,
and scripts for repeating and summarizing the process runs. No remote push or
publication was performed.


## Native runtime follow-up (2026-10-12)

Official Rust and Debian build dependencies were subsequently installed/extracted
inside the cloud workspace with user approval. `cargo check --workspace --locked`
passed with the lazy-filter and console-bridge changes. This is a type/build
check, not a passing full test suite or display benchmark.

The first generated native fixture exposed a real observability defect: the ESM
route obtains `window.console`, which was absent. A console bridge now forwards
log/info/warn/error/debug to the existing opt-in core sink. A subprocess regression
test verifies enabled output and disabled silence without mutating process-wide
environment variables in parallel tests. Native runs require the actual fixture
JSON with correctness checks; bootstrap `W3COS_DOM_OK` alone remains invalid.

An isolated live-childNodes candidate avoids materializing every node wrapper
for each indexed lookup, traversing current sibling links instead. It does not
cache mutation state; general collections keep their old provider. Mixed node,
live mutation, reparenting, fragment, wrapper-materialization and realm-reset
regressions were tested. The full no-default-feature runtime candidate reproduced
the pristine baseline's exact 108 failing test names, with five additional
passing tests. This is explicitly not a clean-suite claim.

A real **native-only** comparison used the shared two-scenario fixture, identical
release compiler/CLI/lockfile, eight alternating fresh-process pairs, three
excluded warmups and seven measured samples per scenario/process (56 measured
samples per revision/scenario). Before is lazy-filter plus console bridge;
after adds only childNodes traversal. Workload timings include correctness checks:

| Scenario | Before process-median ms | After process-median ms | Paired geometric after/before | Adjusted estimated interval |
| --- | ---: | ---: | ---: | --- |
| Append/remove pool | 348.845 | 288.282 | 0.82794 | 0.80177–0.85361 |
| Class mutation/matches | 901.783 | 886.864 | 0.97481 | 0.94616–1.00357 |

Append/remove was faster in all eight observed pairs, about 17.2% by the paired
geometric ratio. Class/matches is inconclusive and two pairs were slower. These
are batch durations for 256 nodes × 32 rounds, not frame times. No browser,
renderer, presentation or allocation-throughput comparison has been made.

A separate candidate avoids allocating all siblings merely to determine whether
an element is first. Focused semantic tests preserve text/comment siblings,
reordering, reparenting and detached behavior. Its first combined benchmark was
rejected before measurement because fresh dependency resolution changed
unicode-ident 1.0.26 to 1.0.27. `rebuild_native_locked.py` now records an explicit
retained-project rebuild with the supplied lockfile, preserving original CLI
provenance and requiring --locked --offline; it does not waive dependency parity.

After the explicit pinned rebuild, a second uncontended eight-pair native-only
run compared lazy-filter plus console against childNodes plus sibling changes.
Append/remove process medians were 346.393ms before and 280.326ms after; the
paired geometric ratio was 0.81214 (estimated adjusted interval 0.79332–0.83618),
with all eight pairs faster. Class/matches medians were 899.535ms and 854.744ms;
ratio 0.94620 (0.90457–0.98293), but one observed pair was slower. Therefore the
strict 5% margin plus every-pair-faster rule still does **not** accept all cases.
The second run is not an isolated measurement of the sibling change and does
not include the independent shared-font change below. Both experiments and
all raw per-process samples are retained.

An additional main-checkout change returns the already immutable, process-static
host font directly instead of retaining a second deep clone of its glyph vectors
and maps. The pointer-sharing regression test failed before and passed after the
change. This verifies removal of the duplicate ownership/allocation; no precise
memory-byte saving or startup speedup has yet been measured.

The unchanged debug 8ms layout gate is sensitive to initialization and execution
context. Four alternating isolated cold runs per pristine/candidate binary all
failed (~86–94ms average); the candidate observations were slower. Sequential
layout suites passed that budget (4.083ms pristine, 4.458ms candidate), while
sharing the same 70 other failing tests. An exploratory release probe of the
same 40-row tree found first compute ~978ms and later computes ~0.8ms. This
localizes initialization overhead but does not identify its cause or dismiss the
candidate regression concern. All failures remain in evidence; no gate was
relaxed, warmed silently, or removed.

Browser paired collection remains blocked by this executor's required socket
restrictions with Chromium sandbox enabled. A suitable authorized common host
is still needed. Startup, cold/warm layout, allocation-heavy memory behavior,
scrolling, input latency, animation, GPU and display presentation remain outside
the two-scenario comparison. Universal native superiority remains unverified.


The final main checkout's `cargo check --workspace --locked` passed with the
shared-font change and corrected workspace include paths. The full Skia-enabled
runtime test executable builds with default features plus `dynamic-js`.
Default features alone still reproduce three upstream test-compilation errors
from unconditionally referenced dynamic modules. The broader 2,050-test run is
separate from the headless baseline; its failures must not automatically be
called pre-existing without a matched pristine run in that configuration.

## Matched graphics baseline and cold-layout attribution

The matched pristine default-plus-dynamic-js suite completed with 1,915 passed,
131 failed and 2 ignored; the main shared-font checkpoint had 1,918 passed,
130 failed and 2 ignored. All 130 shared panic/assertion payloads match exactly,
including values, geometry and errors. Only narrowly verified path/line offsets,
libtest thread IDs and one map-entry order differ. There are no main-only
failures in these runs. The additional pristine realm-reset identity failure
passes four isolated alternating repetitions on each binary, so it is not
credited as a fixed defect. Existing early failures may hide later regressions.

A separate debug/no-default-feature diagnostic exports three controlled source
variants from the pristine commit: original, shared-font only, and the frozen
DOM candidate retaining the old font clone. Every variant uses the same ignored
diagnostic, lockfile, compiler, features and geometry checks. An initial build
attempt was rejected because a shared Cargo target reused an old executable.
The corrected workflow archives rejected attempts, cleans only local workspace
packages, requires freshly compiled local artifacts, and verifies source bytes
before and after compilation. Runtime pointer checks independently verify which
font-ownership implementation ran.

Nine smoke processes and the complete predeclared 96-process paired series all
passed font identity and exact 121-node geometry checks. They selected the same
19,484,784-byte Noto Sans CJK SC collection, index 2. Eight alternating pairs for
each comparison and each of three prewarm modes produced these descriptive
phase results (milliseconds; no acceptance gate was defined for this diagnostic):

| Original versus shared-font phase | Original median | Shared median |
| --- | ---: | ---: |
| Cold first compute | 4556.047 | 4171.781 |
| First compute after host-font initialization | 209.225 | 7.746 |
| Explicit layout-font getter initialization after host prewarm | 197.382 | 0.004 |
| First compute after both font getters are initialized | 7.682 | 7.621 |

The cold paired geometric ratio was 0.90852, but unchanged host-font initialization
also differed between binaries. Thus the entire 9.1% difference must not be
attributed to sharing. The directly timed getter and pointer checks establish
removal of the redundant one-time clone. Host initialization still costs about
four seconds in these debug processes and includes discovery, byte copying and
font parsing. No precise memory saving or release startup claim is made.

The controlled DOM-candidate cold ratio was 0.97596, with two of eight pairs
slower. The earlier 4–6% adverse cold result therefore did not reproduce
consistently; it is retained, not erased or declared fixed. Identical-source host
initialization varied between executable variants. Embedded same-binary temporal
controls also show variation, and cannot isolate link-layout effects. CPU and
wall measurements track closely for the long phases; this does not uniquely
identify instruction counts, cache behavior, page faults or frequency effects.

These diagnostic intervals exclude verification, result dropping and input-tree
construction; checks between samples perturb caches. First compute still builds
internal layout state, while later computes reuse engine/text caches. Modes
cannot be subtracted as an exact cost decomposition. The unchanged real 8ms gate
was separately run in four alternating fresh-process pairs: all eight processes
failed, including shared-font. No threshold or warmup was altered.

The main working checkout subsequently consolidates the previously tested
childNodes/sibling production delta with shared-font and the latest comparison
tools. Earlier performance runs remain attributed to their exact frozen binaries;
they do not automatically validate the consolidated build.

The consolidated release fixture subsequently compiled and completed an explicit
original-lock rebuild. A new uncontended eight-pair native-only experiment used
the same CLI, compiler, fixture and lockfile as the before arm (lazy filter plus
console). All eight pairs were faster for both script scenarios:

| Consolidated scenario | Before median ms | After median ms | Paired ratio | Adjusted estimated interval |
| --- | ---: | ---: | ---: | --- |
| Append/remove pool | 356.306 | 290.922 | 0.81009 | 0.78146–0.83960 |
| Class mutation/matches | 921.751 | 884.792 | 0.95634 | 0.92698–0.98032 |

This is about 19.0% and 4.4% respectively by paired geometric ratio. Class/matches
still misses the strict 5% margin (upper bound must be below 0.95), so the all-case
acceptance rule does not pass. It is not a browser result or evidence of additive
individual-patch effects. The consolidated headless correctness run completed
with 1,449 passed and 108 failed, exactly the pristine failure-name set; this
comparison does not assert equivalence after those existing failure points.


## Location facade cache (2026-10-12, incremental to consolidated main)

One live top-level Location facade is retained per page realm and cleared before
its arena reset. Getters still read current history, authored property overrides
still execute, and frame-specific explicit Location objects stay independent.
This removes repeated construction from selector `:target`/hash access without
caching the current URL. Stable identity intentionally preserves mutations to the
facade; existing descriptor/unforgeable compatibility gaps remain.

Eight alternating fresh-process native-only pairs used identical fixture bytes,
CLI, compiler and pinned generated lock, with three warmups and seven measured
samples per scenario per process:

| Scenario | Before median ms | After median ms | Paired ratio | Adjusted estimated interval |
| --- | ---: | ---: | ---: | --- |
| Class mutation/matches | 932.359 | 192.033 | 0.20984 | 0.20250–0.21694 |
| Append/remove pool | 298.551 | 300.734 | 0.98663 | 0.95742–1.01400 |

All eight class/matches pairs improved; four append/remove pairs were slower.
The strict all-scenario gate therefore does not pass. These are native-versus-
native script measurements, never browser/rendering/startup results. Allocation
attribution is estimated arena storage, not measured RSS or proof of a leak.

Seven new focused Location tests passed headless and with graphics/dynamic-js.
Headless before/after: 1,449/1,456 passed and the same 108 failure names. Graphics
plus dynamic-js before/after: 1,921/1,929 passed, 131/130 failed, two ignored each;
all 130 shared assertion/error payloads reconciled after nondeterministic path,
thread-ID and map-order normalization. The remaining baseline-only retained-node
identity assertion is intermittent, not fixed: isolated four-pair checks passed
4/4 before and 3/4 after. Full CI is not green. These failure counts include
failures outside the seven added focused tests and are not acceptance waivers.

The optional local-font-registration candidate is **not included** in this branch.
The branch contains the consolidated main changes plus Location and local-run
utilities. Historical measurements remain bound to their original binaries and
source snapshots; new packaging/docs do not constitute a new performance run.
