# Combined range synchronization and identity lookup

## Scope

Integrates #200 (`bf3cf66d1b4e2988377fd9042a89558e55d5ad5e`) and #201
(`062b910d2622485d730c66188ae0488a9e7fc540`) on main
`09d981226c7373d8ae5100274a8878d28da60655`. The integration commit retains
both original heads as parents; their PRs and branches remain unchanged.
No #126/#180 changes or new optimization is included.

The only reconciliation of the two implementations adapts range synchronization
and registry test access to the ordered registry: `retain_live()` and
`entries.iter()`. Compaction rebuilds the index; weak-reference identity checks,
epoch cleanup, dirty-cache repair, empty-write repair and resize full refresh
remain intact. An independent source review found no blocking issue; it was not
a substitute for tests. A direct range-sync-compaction-followed-by-lookup test
would further strengthen the existing separate compaction/epoch coverage.

The complete `binary.rs` SHA-256 is
`deba0e631cf39c534d4e061362cd188799cca65eec5b1c1f569eddbfcfe8fe15`,
matching the previously archived, tested composition exactly. `heap.rs` and
`value.rs` are unchanged from #200. The original reports are historical
single-PR measurements, not measurements of this combined candidate.

## Fresh correctness validation

Rust 1.99.1, identical dependency lockfile, separate candidate/main targets:

- Formatting passed; Core **341 passed**; DOM **760 passed, 2 ignored**.
- `w3cos-runtime --no-default-features` jsdom: both actual main and combination
  **221 passed, 1 failed, 1 ignored**, with exactly the same failure:
  `jsdom::tests::canvas_2d_context` (`canvas_capture_media_stream_track_class`
  instanceof assertion). This limited local suite is not fully green.
- `layout_ng` filter selected **0 tests on both sides** because this candidate
  does not include #126. This is not LayoutNG coverage and the prior broader
  combination's 21 failures must not be attributed to this candidate.
- Workspace CI must be checked on the published combined head, not inherited
  from either original PR. See the integration PR for its current result.

## Actual combined performance

Linux x86_64 cloud executor, pinned Rust 1.99.1, release opt-level 3, thin LTO,
one codegen unit, default unwind. The current candidate was independently built
in dedicated targets. The registry main baseline reuses the previously isolated,
audited byte-identical binary; alias main and candidate were both rebuilt fresh.
CSV line endings are normalized to LF for repository storage; values are unchanged.
Source/lock/binary hashes and raw samples are in
[the integration data](typed-array-integration-data/). Timing began only after
both correctness builds/tests finished; affinity and paired ordering are saved.
These are **engine API microbenchmarks, not browser or application speedups**.

Five alternating fresh-process pairs per registry case:

| Mixed workload | Iterations | Main median ms | Combined median ms | Median paired speedup |
|---|---:|---:|---:|---:|
| 0 live views | 200000 | 54.270 | 41.872 | 1.271× |
| 1 live view | 200000 | 78.918 | 66.104 | 1.207× |
| 64 live views | 20000 | 129.799 | 7.581 | 16.130× |
| 1024 live views | 20000 | 1694.040 | 7.498 | 213.330× |

All five pairs are faster for each row. Ordinary-array controls at 0/1 views
are 47.901 → 37.105 ms and 67.534 → 51.635 ms, each 5/5 faster. These values
measure the combined executable against main; they are not added or multiplied
single-PR gains.

The deliberately quadratic alias-write harness uses eight alternating pairs,
eight rounds per process, dropping the first three rounds. At 2048 elements,
indexed writes take 42.633 → 0.177 ms (one view), 83.366 → 0.336 ms (three
aliases); fill takes 41.532 → 0.181 and 87.525 → 0.362 ms. These stress cases
validate range-sync benefits, not real application-scale multipliers.

### Controls and costs remain visible

- Constructing 8192 views with no/1024 preexisting views: 95.467 → 94.044 ms
  and 98.217 → 86.246 ms, only 3/5 and 4/5 faster pairs respectively.
- Pure lifecycle, 16 epochs: 64 views **10.955 → 11.753 ms**, 1024 views
  **214.539 → 221.275 ms**. Only 2/5 and 3/5 pairs are faster; ranges overlap
  and paired directions differ. Do not claim lifecycle improvement or zero
  regression. No favorable re-run was selected to replace these controls.
- Alias-harness fresh construction at 8192 elements is 0.425 → 0.432 ms;
  paired ratio range 0.907–1.293. No reliable construction improvement claimed.
- Separate allocation-counting builds: after subtracting each zero-view
  control, holding 1/64/1024/16384 views adds **84/2192/34832/557072 requested
  live bytes** versus main. This is not RSS, allocator metadata or fixed TLS.
  Maintenance remains occasionally linear; delayed weak-wrapper cleanup and
  the existing global backing-buffer retention policy remain unchanged.

No merge, deployment, source PR closure, or branch deletion is implied by this
integration validation. Review the tradeoffs alongside the combined-head CI.
