# Typed-array write synchronization: cross-PR review

Review date: 2026-10-23. Baseline main: `09d981226c7373d8ae5100274a8878d28da60655`.
This is a Core micro-optimization, not an end-to-end renderer or application speedup.

## Why this change, rather than stacking existing percentages?

| PR | Inspected head / relationship | Assessment |
| --- | --- | --- |
| [#126](https://github.com/wangnaihe/w3cos/pull/126) | `bcbb8aa29556e4e08205b2fc08c6b9e2b15173d5`, includes baseline main | LayoutNG migration, default off; not equivalent work in all reported incremental text cases. Legacy final geometry 243.20001 vs NG 75.1875 in its report precludes calling the corresponding timing a speedup. Run modes in separate processes; VERIFY does both engines. |
| [#180](https://github.com/wangnaihe/w3cos/pull/180) | `2676e043a4ccc4cde08fc5fb74ed6ebfd1d78857`, historical main base `ecd54c7` | DOM class-selector/group-allocation/index maintenance changes. Author's 7.35% class-match claim is from an older aarch64 AOT opt-z build, not independently reproduced here and not additive. |
| [#189](https://github.com/wangnaihe/w3cos/pull/189) | Already merged via `6964c0f` and present in baseline | Lifecycle negative-prototype-proof cache. Do not reapply the corresponding #175 work. Its 8.17% historical append/remove claim is not an additional gain on current main. |
| [#170](https://github.com/wangnaihe/w3cos/pull/170), [#175](https://github.com/wangnaihe/w3cos/pull/175), [#179](https://github.com/wangnaihe/w3cos/pull/179) | Cumulative stack targeting `aot-port`, not three independent main changes | #179 includes restricted same-module boxed direct calls. Its [test262 run](https://github.com/wangnaihe/w3cos/actions/runs/39558085160) reports 57 regressions (56 runtime PRINT cases, one optional-chaining call-expression SameValue mismatch), despite other green checks. Root cause not established; no compiler port in this change. |
| [#196](https://github.com/wangnaihe/w3cos/pull/196), [#197](https://github.com/wangnaihe/w3cos/pull/197), [#198](https://github.com/wangnaihe/w3cos/pull/198) | Already in baseline main | Keep as baseline. #198 removes quadratic fresh typed-array initialization, but subsequent writes still refresh every element of every alias. |

The open DOM/layout changes do not alter `binary.rs`. #189 touches `value.rs`; our additional change there is narrowly confined to `ArrayStorage` mutation tracking.

## Implementation and compatibility

A completed byte write refreshes only complete materialized elements intersecting its byte range, including wider and unaligned aliases. Synchronization remains immediate after each successful write: this is not deferred batching. Resize/grow retain full synchronization.

Generic inherited array operations can mutate a typed array's materialized cache independently of its backing bytes (including same-length splice and delete). A dirty flag preserves the old full-sync repair behavior for these cases, including empty byte fills. All mutable storage entry points mark dirty; successful synchronization clears it. Unwinding extension/resize callbacks mark dirty before mutation.

For clean views, work is proportional to registered views plus intersecting elements; the registry scan remains. N indexed writes into one N-element view decode N elements rather than N². Dirty-view repair intentionally remains linear in the view length.

Metadata is kept beside HeapAllocation’s kind field in existing alignment space rather than adding eight bytes to every ArrayStorage. On this measured x86_64 target, ArrayStorage remains 48 bytes and HeapAllocation remains 24 bytes; a unit test checks compactness on each tested target. This is not a Rust ABI/layout guarantee. Ordinary-array mutators gain a dirty-bit store, so ordinary-array push and fresh typed-array construction are negative controls. Heap accounting and lifetimes are unchanged.

## Correctness validation

- Original implementation with the added complexity regression fails: 32 writes decode 1,024 elements, expected 32.
- Candidate core suite: 333 passed.
- Tests cover repeated writes; mixed-width aliases and offsets; unaligned DataView writes; disjoint buffers/views; bounds; partial BigInt conversion failure; reentrant sort comparator observing aliases immediately; resize/shrink/grow repair; dropped views; fill/set/copyWithin/reverse/sort; Atomics; same-length splice/delete/pop+push; empty fills.
- The engine's current numeric conversion does not execute object valueOf hooks. No claim is made that unsupported coercion-hook semantics were validated.
- Isolated **local only** composition of inspected #126 + #180 + this change: merge clean, core 333 passed, DOM 764 passed / 2 ignored. This is compatibility evidence, not a combined performance claim.

## Benchmark protocol

The standalone harness in `scripts/perf/typed-array-alias-sync` uses Rust 1.99.1, release opt-level 3, thin LTO, one codegen unit, unwind panic strategy. Build the identical harness against each worktree's core crate. Baseline and candidate are separate binaries. No compilation runs concurrently with the accepted timing collection.

Cases: indexed writes and fill at N=512/2048/8192, one view or three overlapping views (Uint8 plus two Uint16 aliases); construction and ordinary-array push at N=8192 as negative controls. Eight fresh-process pairs, alternating baseline/candidate execution order; eight rounds per process, first three discarded. Per-process median of five samples, then median of eight paired speedup ratios. Target-byte checksums are asserted for indexed/fill; alias-cache correctness is covered by unit tests. Negative controls use black_box, but their printed zero checksum belongs to the untouched setup array, not their produced output. The case order is deterministically shuffled.

The harness intentionally does not reset the page arena between rounds. Registered views can accumulate across the eight rounds; one/three views means views of the current target buffer, not the entire registry. This population and its scan cost are matched between baseline and candidate. Ordinary-array push also includes method classification with live typed-array views and is not representative of every ordinary-array operation. The reported min/max paired ratios are observed ranges, not confidence intervals.

A preliminary collection interrupted by an accidentally overlapping runtime build is discarded, not reported. Results are local CPU microbenchmarks; no AOT, GPU, browser workload, LayoutNG timing or sum of prior PR claims is implied.

## AOT failure interpretation

At pinned test262 commit d53ebc8b1931d9f1541e43d59650776e4b766248, [call-expression.js lines 51–58](https://github.com/tc39/test262/blob/d53ebc8b1931d9f1541e43d59650776e4b766248/test/language/expressions/optional-chaining/call-expression.js#L51-L58) defines fn4 returning [{a:77}] and fn5 returning []. The assertion arguments are reversed: the error is consistent with fn4()[0]?.a yielding undefined, not proof that fn5()[0]?.a yields 77. Exact failing-site attribution requires reproduction.

The 56 PRINT cases are a runner classification, not 56 proven semantic faults: the #179 runner (examples/test262.rs lines 573–588) checks __t262_print before __t262_ran, while synchronous acceptance (lines 718–759) requires T262 OK. Whether changed prints originate in tests, harness behavior, or compilation remains unresolved. #179 does not modify that runner. Its direct-call eligibility (esm_codegen.rs around 2799) and lowering (w3ir_aot.rs around 494, 7531, 8160) can affect zero-argument fn4/fn5 calls, but this is only a hypothesis until toggle/bisect evidence.

## Combined-runtime limitations (same-head control)

The no-default-features runtime builds against the local #126+#180 composition. Both **without this patch** and with the final candidate produce exactly the same named failures:
- jsdom: 218 passed, 1 failed, 1 ignored; canvas_2d_context fails the canvas capture track class assertion.
- LayoutNG: 67 passed, 21 failed; primarily Ahem text geometry differences under this feature configuration, plus generated inline ext/wrap cases missing checked-in Chrome golden entries.

These failures are not introduced by this optimization in that controlled comparison. They also prevent claiming all combined runtime paths are green. Renderer/backend support and the missing golden cases need separate triage; no speculative fixes or changed goldens are included here.

## Accepted local timing results

Times are medians of per-process medians; speedup is the median of paired ratios (not the quotient of the two table medians).

| Mode | N | Target views | Baseline ms | Candidate ms | Paired speedup | Observed ratio range |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| array_push | 8192 | 1 | 1.225 | 1.162 | 1.05× | 0.92–1.18× |
| construct | 8192 | 1 | 0.417 | 0.390 | 1.09× | 0.83–1.22× |
| fill | 512 | 1 | 2.497 | 0.080 | 33.66× | 24.91–39.78× |
| fill | 512 | 3 | 5.273 | 0.192 | 27.34× | 21.84–33.25× |
| fill | 2048 | 1 | 40.902 | 0.323 | 126.13× | 115.44–145.28× |
| fill | 2048 | 3 | 82.821 | 0.804 | 102.99× | 93.85–116.12× |
| fill | 8192 | 1 | 683.884 | 1.334 | 495.92× | 424.99–573.47× |
| fill | 8192 | 3 | 1359.410 | 3.558 | 382.19× | 335.23–454.82× |
| indexed | 512 | 1 | 2.630 | 0.090 | 28.91× | 20.30–40.10× |
| indexed | 512 | 3 | 5.284 | 0.199 | 26.10× | 20.24–29.88× |
| indexed | 2048 | 1 | 41.291 | 0.295 | 139.66× | 105.37–155.83× |
| indexed | 2048 | 3 | 81.293 | 0.811 | 102.27× | 79.22–110.72× |
| indexed | 8192 | 1 | 650.388 | 1.334 | 493.30× | 369.50–534.29× |
| indexed | 8192 | 3 | 1326.788 | 3.277 | 397.03× | 334.02–480.41× |

Controls span parity and are noisy; no reliable improvement is claimed for construction or ordinary-array push. Large write gains reflect removal of repeated full-view decoding, not improvements to the JS compiler, rendering, or end-to-end applications.
