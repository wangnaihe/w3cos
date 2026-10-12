# Paired DOM API benchmark foundation

This is a runnable collection/validation foundation, **not evidence that native
is faster than Chromium**. It deliberately cannot consume existing native
`cpu_frame` summaries: those have a different timing interval.

## Run locally on macOS

See [LOCAL-RUN.zh-CN.md](LOCAL-RUN.zh-CN.md) for prerequisites, exact commands,
lockfiles, outputs and the limits of this two-scenario comparison.

## Scope and shared work

`scenarios.ts` is JavaScript-compatible TypeScript. The exact same bytes run as
an ES module in Chromium and via the real-DOM ESM/AOT native compilation path.
It has two scenarios:

- Append/remove a fixed pool of 256 nodes for 32 rounds, checking return values,
  parent transitions, exact child order/count, and the final empty subtree.
- Alternate two classes on 256 attached nodes for 32 rounds, checking className
  and both positive and negative simple-selector matches every iteration.

The metric is synchronous DOM/JavaScript script wall time in milliseconds,
including validation, API calls, invalidation, allocations and any GC that
happens during the timed work. There is no forced layout or wait for rendering
completion. These results must never stand in for startup, CPU-frame, scrolling,
input-to-display, GPU, presentation, memory, or whole-application performance.

Each process runs three excluded warmup samples then seven measured samples per
scenario. Eight process pairs alternate native/browser and browser/native order.
Each arm starts a fresh process/document. Preallocated pools avoid progressively
retaining detached native arena nodes across allocation trials; **creation and
allocation throughput are not measured**. No native-only cleanup is inserted.

Window dimensions are recorded as 1024×768, DPR 1, matching the native default
headless DOM environment. These are DOM environment values, not evidence of a
physical display or presented pixels. The fixture owns its subtrees; native's
additional bootstrap `div#root` does not affect its correctness assertions.

## Run verification without native rendering dependencies

```sh
python3 -m unittest discover -s tools/perf-paired -p 'test_*.py' -v
node --check --input-type=module < tools/perf-paired/scenarios.ts
node tools/perf-paired/test_fixture.mjs
```

The Python tests use clearly synthetic durations to test acceptance/rejection.
The Node test uses a small mock only to verify that the fixture detects broken
append/remove, child ordering, class mutation and selector matches. Neither is
a real browser or native timing run. The foundation's tests need Python 3.11+
and a recent Node with ECMAScript modules; they install nothing.

## Collect an actual browser arm

Collection additionally requires installed Python Playwright and Chromium. The
runner keeps Chromium's sandbox enabled and never downloads a browser or
installs packages. It uses an isolated, offline browser context with no account
profile. Its test viewport is explicit.

```sh
python3 tools/perf-paired/run.py browser \
  --chromium /usr/bin/chromium --output browser-only.json
```

Browser-only evidence intentionally returns a nonzero comparison status. A
browser result cannot establish native superiority. Launch/runtime errors are
saved as `collection_status: blocked`, with no fabricated samples. If the host
blocks Chromium's required sockets or sandbox setup, use an authorized suitable
benchmark host; do not disable security or change the metric to make it pass.

## Build and collect native/browser pairs

First obtain a working release w3cos CLI and its normal build dependencies using
the repository's documented build process. This helper invokes that existing
CLI; it does not install anything. Set the Rust toolchain environment as needed.

```sh
python3 tools/perf-paired/build_native.py \
  --w3cos /absolute/path/to/w3cos --output-directory /absolute/path/to/native-bench
python3 tools/perf-paired/run.py paired --chromium /usr/bin/chromium \
  --native-binary /absolute/path/to/native-bench/w3cos-dom-api-bench \
  --native-provenance /absolute/path/to/native-bench/build-provenance.json \
  --output paired-evidence.json
python3 tools/perf-paired/validate.py paired-evidence.json
```

The build helper uses a dedicated temporary build directory, verifies the
real-runtime ESM bundle rather than accepting legacy fake-document fallback,
verifies the generated runtime dependency points to the selected checkout,
and records exact source, generated code, executable, CLI, lockfile and compiler
metadata. Its build directory and log are retained. It hashes tracked and
non-ignored untracked repository files, retaining a source snapshot and tracked patch; HEAD and source hashes
must remain unchanged through the build. Symlinked repository files are rejected
by this provenance helper. Use an output directory outside the repository or
inside an ignored directory. Starting a rebuild invalidates any previous success
provenance in that output directory; a failed build cannot silently reuse the
earlier binary as new evidence. The runner requires that
provenance and binary/source digests match, retains full provenance with each
native run, and rejects any build/configuration change between pairs. The Git
revision describes the runtime checkout; the supplied CLI is separately bound
by its binary hash, not assumed to come from that revision. Native execution
explicitly sets
`W3COS_JS_CONSOLE=1`, clears `W3COS_AOT_WINDOW`, captures stderr and requires
exactly one complete fixture JSON line. Process exit or `W3COS_DOM_OK` is not
accepted as correctness evidence. Provenance is an operational record, not a
cryptographic attestation against intentionally falsified files.

## Rebuild a retained native project with a pinned lockfile

The CLI's original build resolves dependencies without `--locked` and recreates
its temporary generated project. Independently built variants can therefore
have different `Cargo.lock` bytes. Do not waive the lock-equality check or edit
successful provenance to disguise that difference. Instead, explicitly select
the baseline lockfile and rebuild the retained generated project:

```sh
python3 /path/to/helper-checkout/tools/perf-paired/rebuild_native_locked.py \
  --build-directory /absolute/path/to/candidate-native-bench \
  --repo-root /absolute/path/to/unchanged-candidate-checkout \
  --lockfile /absolute/path/to/baseline-native-bench/compiler-temp-UUID/w3cos-build/Cargo.lock
```

This helper runs installed Cargo directly with `build --release --locked
--offline`; it never calls the CLI, regenerates the fixture, executes a benchmark,
or fetches missing dependencies. Use the same Rust/native-dependency environment
as the original successful build. `--cargo` optionally selects an installed
executable; otherwise Cargo is found on `PATH`. Missing offline dependencies or
an incompatible lockfile are errors, not permission to resolve a different lock.

Preflight requires successful provenance and exact retained binary, generated
main/ESM, lockfile, CLI, fixture, source snapshot, tracked patch, revision, and
compiler identity. The selected checkout must still match the complete original
snapshot, including nonignored untracked files. Run a new helper from a different
checkout if adding it to the selected checkout would change that snapshot.
The helper accepts only this fixture's exact generated Cargo manifest shape,
including all three local dependency paths and the release profile. This explicit
check matters because original v1 provenance did not hash `Cargo.toml`. New
generated files, symlinked retained inputs, Cargo configuration files, generated
ancestor toolchain files, and compiler/profile/target overrides are rejected.
Inherited target-directory/build-target settings are replaced by the verified
host-native output location. This remains operational provenance, not a complete
reproducible-build attestation for native system libraries or the entire host.

Before changing the lockfile, the helper creates a unique
`locked-rebuild-history/<id>/` with the original provenance bytes, binary, lock,
manifest, source records, and a copy of the explicitly pinned lock. Original
CLI logs and earlier history directories remain untouched. Active provenance is
then marked incomplete. Cargo's old target executable is archived so a missing
new output cannot reuse it. After Cargo succeeds, source identities and pinned
lock bytes are checked again; only then is the new binary copied and successful
provenance published atomically last. A separate `locked_rebuild` record retains
the exact previous provenance, actual Cargo command/cwd, input paths, generated
input hashes, Cargo identity, final binary/lock hashes and rebuild log hash.
The original `build_command` continues to describe the original, unpinned CLI
invocation. Repeated successful rebuilds retain the complete provenance chain.
The validator accepts legacy successful v1 builds and strictly checks this
optional record when present. Final lock digests must still match between any
variants whose comparison requires matched dependencies.

Preflight rejection leaves the original build untouched. Failure after the
archive/incomplete transition retains diagnostics and intentionally leaves
unusable provenance, even if the old binary still exists. The helper will not
silently retry an incomplete build. To retry, inspect the failure first, then
explicitly restore the archived original binary, `Cargo.lock`, and provenance
to their original paths; source and compiler checks must still pass. Do not
restore only provenance over a changed binary/lock. A stale
`.locked-rebuild-in-progress` after a killed process also requires inspection
before removal. Exclude concurrent source edits, CLI/Cargo builds, tests and
measurements; the helper's exclusive marker only coordinates other instances
of this helper for the same output directory.

## Fail-closed gate

The versioned manifest fixes the workload, matrix, sample counts, eight pairs,
5% native speed margin and statistical settings before collection. Unknown or
weakened manifests are rejected. A new workload requires an explicitly reviewed
new schema, not runtime tuning of only one engine.

Validation rejects missing or duplicate arms/scenarios/runs, source or host
mismatch, changed engine/build versions, unsupported modes, mismatched viewport,
units or timing semantics, reused result records, bad warmup/sample ordering,
zero/nonfinite durations and incorrect state/operation evidence.

The independent statistical unit is each **process pair**; within-process
samples produce a median, not seven extra independent pairs. Per scenario, a
deterministic paired bootstrap estimates the geometric mean native/browser
ratio, using Bonferroni-adjusted two-sided percentile intervals across the two
fixed scenarios. Pass requires the upper interval below 0.95 and no observed
pair median ratio at or above 1. A lower interval above 1 fails; remaining valid
outcomes are inconclusive. Missing data is blocked. Every required scenario
must pass; an aggregate cannot hide a slower case. Bootstrap intervals from
eight pairs are estimates, not universal guarantees. Record host contention,
power/thermal state and other limits when interpreting real results.

## Current verification limits (2026-10-12)

Python schema/build-helper tests and mock fixture correctness checks pass in the
cloud executor. The native fixture was compiled and executed on Linux x86-64;
the consolidated main changes plus Location cache were tested with the recorded
locks. Native-only improvements and remaining correctness failures are documented
in [performance-validation.md](../../docs/performance-validation.md).

Actual Chromium launch is blocked in that cloud execution environment by denied
process-singleton Unix sockets. No paired native/browser result has been produced.
The macOS runner has unit/contract validation only; a real macOS end-to-end build
and measurement remain unverified. The broader startup/rendering/input/memory
matrix is not implemented by this DOM-script runner. Full existing CI is not
green, and the comparison has not been installed as a passing CI requirement.
