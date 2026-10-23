# Typed-array identity registry

Base: main `09d981226c7373d8ae5100274a8878d28da60655`.

## Bottleneck and implementation

Ordinary array property reads, indexed writes and method dispatch ask the binary
module whether an array is a typed view. Previously each lookup retained and
searched the complete global view vector. Unrelated live typed views therefore
imposed linear work even on ordinary-array operations. A deterministic regression
test records **16,384 weak-entry visits for 128 ordinary reads with 128 unrelated
live views** on the baseline.

The registry keeps the existing ordered vector and a private identity-to-index
hash map. Hits still upgrade the weak reference and check array identity:
addresses alone are not sufficient across allocation reuse or arena epochs.
Compaction rebuilds indices. Alias synchronization/detachment retain their
previous traversal order and semantics.

The final implementation also has a small inline empty-registry guard, before
array-handle resolution. Registered lookups use a separate helper. This targets
empty-path frame and array-resolution overhead when no typed view exists;
the benchmark, not an assumed inlining guarantee, establishes its effect.

Maintenance runs after max(64, live-view count) lookup/registration operations,
and on the first observed arena epoch change. Lookup/registration is therefore
**amortized** constant, not worst-case constant: an individual cleanup still
scans the vector. Dead host-backed views may remain until the maintenance budget
is consumed; page-arena teardown is recognized on the next lookup/registration.
Substantial teardown also shrinks excess registry capacity. The global
backing-buffer table's existing lifetime policy is unchanged.

## Measurement protocol

The harness in `scripts/perf/typed-array-registry` uses public engine APIs:
64 ordinary rows, indexed read/write/read and push/pop, with distributed reads
from unrelated 32-byte typed payloads in the mixed case. Checksums are computed
independently and validated. This is **not** a browser/app end-to-end measurement.

Linux x86_64 cloud executor, AMD EPYC 9V74, pinned Rust 1.99.1, opt-level 3,
thin LTO, one codegen unit, default unwind. Identical standalone dependency
lockfiles: SHA256
`c7a34e89154cf9be64671c4bcac9751ed600e2ed4c10bf6180a4d97ef91f1025`.

Final timing runs are pinned to permitted CPU 0. Each sample uses a fresh process
so arena-retained values do not accumulate across samples. Baseline/candidate
order alternates; no concurrent builds run during measurement. Ordinary/mixed
no-view investigation additionally alternates case order. Five pairs per case,
except the 11-pair million-iteration no-view comparison. Median paired speedup
and ratio of medians are distinct; raw summaries retain both. No-view and
1/4/16-view controls, construction and pure lifecycle are included.

Allocation counting is a separate opt-in build, never used for timing claims.
It reports requested live allocator bytes, not fixed TLS metadata, allocator
overhead, RSS, or transient physical realloc peaks.

## Final candidate measurements

The following results use the final empty-registry wrapper, not the earlier
index-only experiment. Raw samples, executable/source hashes and summaries are
under [final data](typed-array-registry-data/final/). Superseded experiment data
remain separately under [initial data](typed-array-registry-data/initial/).

| Mixed row workload | Iterations | Main median ms | Final median ms | Median paired speedup |
|---|---:|---:|---:|---:|
| 0 views | 200000 | 47.632 | 39.736 | 1.205× |
| 1 views | 200000 | 81.726 | 67.952 | 1.199× |
| 4 views | 200000 | 114.372 | 58.897 | 1.939× |
| 16 views | 200000 | 326.720 | 60.829 | 5.347× |
| 64 views | 20000 | 108.863 | 6.532 | 16.790× |
| 1024 views | 20000 | 1716.915 | 6.766 | 252.539× |

All five pairs are faster for each row above. The separate 11-pair, million-
iteration no-view check reports ordinary 239.91 → 183.29 ms and mixed
246.83 → 184.53 ms; every pair is faster, paired medians 1.289× / 1.308×.
These are engine API gains, not an application throughput guarantee.

### Construction and lifetime tradeoffs

Keep the unfavorable control batch visible: allocating 8,192 views with 1,024
already live takes 90.94 → 103.01 ms median; pure lifecycle (1,024 views ×16
allocation/reset/first-plain-read epochs) takes 192.08 → 243.65 ms. Candidate is
faster in only 1/5 pairs for each. Do not dismiss this just because ranges overlap.

A single bounded attribution pass then rotates **main, index-only, final wrapper**
order and alternates case order, pinned CPU 0, for 11 rounds:

| Control | Main median ms | Index-only median ms | Final wrapper median ms |
|---|---:|---:|---:|
| Construct 8,192 with 1,024 existing | 89.789 | 89.164 | 88.298 |
| Pure lifecycle 1,024 ×16 | 180.464 | 173.091 | 172.581 |

Main→final paired median is 1.000× for construction (6/11 faster, range
0.854–1.244) and 1.051× for lifecycle (8/11 faster, range 0.970–1.179).
Index-only→final lifecycle paired median is 1.003×. The negative batch is not
reproduced and no wrapper-specific lifecycle penalty is identified, but these
data do **not** prove universal construction/lifetime gains or zero regressions.
Index insertion/maintenance and capacity shrink/regrowth remain real work.
No further repeats were selected to manufacture a favorable conclusion.

The final wrapper is ~8% slower than the intermediate index-only version in the
one-view mixed comparison, while remaining faster than main. It favors the
common empty-registry path; this is a tradeoff versus an intermediate experiment,
not a regression claim against the requested main baseline.

### Memory

Separate final allocation-counting builds confirm extra requested-live allocation
of **34,832 bytes at 1,024 held views**, total fixture held delta
9,292,704 → 9,327,536 bytes (+0.375%). Extra held bytes at 1 / 64 / 16,384 views:
84 / 2,192 / 557,072. Normalize each process by its own initial live count
(argv lengths differ). This excludes fixed TLS metadata and allocator overhead.

After reset plus a plain read, final retains 59,248 fewer bytes at 1,024 views
because excess registry capacity is released. This is not a claim that all
backing buffers are freed. Shrinking can cause regrowth on a later page.

## Investigation history (not final candidate claims)

The first index-only version had no-view mixed medians ~5–9% slower in three
short collections. A stricter pinned, interleaved 11-pair million-iteration check
did not reproduce that slowdown (mixed paired median 1.028×, 8/11 faster).
Assembly showed both baseline and intermediate lookup remained out-of-line and
resolved array handles before the empty check. The small wrapper was then
measured separately and retained for its common-case benefit.

Earlier constructor and lifecycle samples also vary in direction. All initial
raw data remain available separately; headline final results are not copied from
a superseded binary. There is no additive-percentage claim across PRs.

## Correctness and cross-PR composition

Independent read-only review approved the index design, adversarial tests,
benchmark design and final wrapper's safety. Tests cover dead/live mismatched
identity hints, compaction, arena handle reuse, a live host array across reset,
amortized lookup/registration cleanup, churn and capacity reclamation.
The original ordinary-read complexity regression fails on main and passes here.

Final-code local results:

| Composition | Core | DOM |
|---|---:|---:|
| main + final registry | 333 passed | 760 passed, 2 ignored |
| #200 + final registry | 341 passed | 760 passed, 2 ignored |
| #126 + #180 + #200 + final registry | 341 passed | 764 passed, 2 ignored |

Exact combined heads: #126 `01c56cbf564737b6c8e926829a53b1568904fa10`,
#180 `2676e043a4ccc4cde08fc5fb74ed6ebfd1d78857`,
#200 `bf3cf66d1b4e2988377fd9042a89558e55d5ad5e`.
#126 advanced during investigation; the comparison uses its new legacy-table
geometry head, not the previous `bcbb8aa` snapshot.

Fresh combined **without/with** final registry, runtime `--no-default-features`:
both jsdom **221 passed / 1 failed / 1 ignored** and LayoutNG
**68 passed / 21 failed**, with exactly identical named failure sets. The canvas
capture-class assertion and text/Ahem/golden cases are not claimed green; no
golden changes or unrelated layout fixes are included.
[Named comparison](typed-array-registry-data/composition-results.json).

#200 reduces decoded alias elements, whereas this change indexes classification.
It still scans aliases for synchronization. Composition required mechanical
`retain_live()` / `entries.iter()` adaptations in #200's range-sync helper and
one registry-inspection test, not write-range/dirty-cache/coercion changes.
An initial composed compile caught the test access; after adaptation, final
composed suites pass. No existing remote PR was merged or altered.

#180 changes DOM indexing; #126 is default-off LayoutNG, not a claimed layout
speedup. #189 is already merged on main. The divergent #170 → #175 → #179
AOT-port stack is not silently transplanted into this main-based change.

Final workspace formatting, standalone harness formatting and diff checks pass.
These focused/local checks are not a full workspace pass. Exact-head remote CI
remains a separate gate, and the PR is draft pending maintainer review.

## Final performance atop #200

Same harness, pinned CPU 0, five alternating fresh-process pairs. This compares
**#200 versus #200 + final index/wrapper**, not main versus both changes:

| Mixed views | Iterations | #200 median ms | + final registry ms | Median paired speedup |
|---|---:|---:|---:|---:|
| 0 | 200000 | 43.673 | 35.119 | 1.244× |
| 1 | 200000 | 72.024 | 60.577 | 1.169× |
| 64 | 20000 | 106.292 | 6.336 | 16.775× |
| 1024 | 20000 | 1691.670 | 6.725 | 245.774× |

All four cases are faster in all five pairs. The classification bottleneck
persists atop #200; these gains are complementary, not percentages to add to
historical PR results, and do not claim faster alias-write traversal.

## Build provenance

Core source SHA256 for the final main candidate:
`fe739d95345907fdcf93a9b7719ac2c94f103e72870faaaa0b012a5c6afdfb57`.

A shared-target final PR200-only test invocation incorrectly reused a 333-test
main binary. That log is **invalidated**, not counted as composed validation.
A dedicated target directory with core/DOM cleaned and rebuilt explicitly from
the PR200 worktree passes the expected **341 core / 760 DOM** tests. Main and
combined final build logs explicitly compile their corresponding worktree paths;
runtime comparison executes separately saved immutable baseline/final binaries.

Benchmark executable hashes, source snapshots, matching lockfiles, build logs
and raw data are preserved. Final provenance audit results are recorded alongside
the data; cached build success alone is not treated as proof of the correct code.

The audit is closed: all **four timing binaries** were rebuilt after cleaning
core and harness in **separate per-variant target directories**, and are
byte-for-byte identical to the measured binaries. Explicit compilation paths,
Rust 1.99.1 / LLVM 23.1.1, profile flags and absence of RUSTFLAGS/profile
overrides are recorded. No accepted timing measurements were invalidated.

Fresh separate allocation-counting builds reproduced all 14 held/post-reset
deltas exactly; [verified memory CSV](typed-array-registry-data/final/provenance/verified-memory.csv)
is authoritative. The original memory-baseline binary predated the added
lifecycle mode; it was not assumed identical. Main timing harness source hashes
differ only by the allocator SAFETY comment, not executable workload code.
See [audit and compilation evidence](typed-array-registry-data/final/provenance/finalaudit.json).
