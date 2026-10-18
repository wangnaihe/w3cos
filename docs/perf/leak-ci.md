# Leak CI (M1 item 7)

Workflow: `.github/workflows/leak-ci.yml`. Harness: `crates/w3cos-compiler/examples/leak_ci.rs`.
Scenarios: `crates/w3cos-compiler/leak-scenarios/scenarios.ts`. Allowlist: `leak-scenarios/known-leaks.txt`.

```
cargo run --release -p w3cos-compiler --example leak_ci -- \
    [--iterations N] [--soak-secs S] [--report-dir DIR] [--scenarios FILE]
```

The harness AOT-compiles every `export (async) function scenario_*` into a runner that links
`w3cos-core` with `--features leak-detector`. For each scenario it:

1. runs 2 warm-up calls (draining microtasks after each one);
2. records the live arena object/array/function counts and calls `leak_detector::mark_baseline()`;
3. runs N iterations;
4. requires the counts to return to the baseline exactly.

If a scenario leaves residuals, the run fails and prints the leak-detector report (cycles + retainer
chains). Allowlisted scenarios are reported as `known` and don't fail the run.

Gates (docs/roadmap/aot-port.md §3 M1):

| Job | When | Gate |
| --- | --- | --- |
| `scenarios` | every aot-port PR / push | residual = 0 per scenario; release build contains no detector code |
| `soak` | nightly 18:00 UTC (02:00 Asia/Shanghai), or manual | default 8h (configurable): RSS growth ≤ 5% after warm-up, zero residual |

GitHub-hosted jobs stop at 6h, so the soak is clamped to 5.5h unless the repo variable
`LEAK_SOAK_RUNNER` names a self-hosted runner. Reports (`leak-summary.json`,
`leak-report.txt`) are uploaded as artifacts.

## Results (2026-10-18, 20 iterations)

| Scenario | Covers | Residual |
| --- | --- | --- |
| scenario_realistic_sample | bench.ts compute (particles, pipeline, lookup, tree), no DOM | 0 |
| scenario_probe | non-escaping and escaping-then-dropped allocations | 0 |
| scenario_for_of | arrays, strings, iterators, early break | 0 |
| scenario_generators | generators, return/throw | 0 |
| scenario_classes | inheritance, private fields, statics | 0 |
| scenario_closures | captures, counters, callbacks | 0 |
| scenario_async | await, Promise.all, reject/catch | 0 |
| scenario_weakref | WeakRef, WeakMap | 0 |
| scenario_finalization_registry | FinalizationRegistry | 0 |
| scenario_collections_json_strings | Map, Set, JSON, string concat | 0 (was +200 objects per call) |
| scenario_exceptions_destructuring | try/catch/finally, destructuring | 0 |

Local soak smoke (130 s, about 4.9k rounds): live counts stayed flat at (139, 0, 102).

DOM/jsdom paths aren't covered yet: the runner links only `w3cos-core`, and the runtime/DOM crates
need the full native toolchain. That's a follow-up.

### Leaks found

- **Map/Set backing stores never freed.** `collections.rs` kept every instance's store in a
  thread-local id registry (`MAP_STATES`/`SET_STATES`) forever, along with every key and value.
  `clear()` tombstones kept values too. Fix: the registry id is now the instance's weak identity
  (`watch_id`), so the instance's death notice removes its store at the next job checkpoint
  (`weak::process_deaths`). The store is dropped after all tables are unborrowed. Regression test:
  `collections::tests::dead_map_and_set_release_their_backing_stores`.
