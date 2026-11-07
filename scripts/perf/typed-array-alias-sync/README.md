# Alias synchronization microbenchmark

Requires Python 3 and the repository's pinned Rust toolchain. No third-party Python packages.

1. Create baseline and candidate worktrees. Copy this entire harness directory into the same relative location in both (the baseline predates the harness).
2. Build each with `cargo build --release --manifest-path scripts/perf/typed-array-alias-sync/Cargo.toml`. Preserve/reuse the same generated Cargo.lock across both builds. No default native renderer dependencies are needed.
3. Copy their `target/release/alias-sync-bench` binaries into an empty evidence directory as `alias-sync-baseline` and `alias-sync-candidate`.
4. Stop all builds and CPU-heavy tasks, then run `python3 scripts/perf/typed-array-alias-sync/compare.py /absolute/path/to/evidence` from any directory.

The comparison script writes raw CSV, per-variant ArrayStorage size metadata, and summary JSON. It alternates order across eight pairs of fresh processes, discards the first three of eight rounds per process, and reports medians. The benchmark asserts target-byte checksums; correctness/alias verification belongs in the Core unit tests. Negative controls cover fresh construction and ordinary-array push. Baseline large-array cases are deliberately quadratic and can take several minutes.

Standalone harness profile: opt-level 3, thin LTO, codegen-units 1, panic unwind (not the main workspace's panic-abort/stripped artifact). Timings must not be described as AOT or application results.
