# Code-OSS patch queue (M1 item 9)

w3cos keeps reference counting and breaks cycles with `WeakRef` (no tracing GC), so cycles in Code-OSS that
V8 would collect become permanent leaks here. Fixes live in this quilt-style queue and are replayed on the
pinned upstream commit. The queue lives in this repo; there is no GitHub fork yet.

- **Pin**: `tools/vscode-baseline/CODE_OSS.lock.json` (the same commit as the M0 baseline).
- **Queue**: `patches/series` lists `patches/NNNN-*.patch` (git format-patch) in order.
- **Labels**: every patch has a `W3cos-Upstream: yes|no` trailer. `yes` means the fix is valid for stock VS
  Code too and should go to microsoft/vscode as a PR. `no` means it only matters for w3cos.

```sh
ports/vscode/scripts/vscode-patches.sh fetch     # shallow-fetch the pin into ports/vscode/.work/vscode
ports/vscode/scripts/vscode-patches.sh apply     # reset to the pin, git am the series
# ... edit + commit in .work/vscode (add the W3cos-Upstream trailer) ...
ports/vscode/scripts/vscode-patches.sh refresh   # rewrite patches/ + series from pin..HEAD
ports/vscode/scripts/vscode-patches.sh lint      # w3cos leak-lint over src/ (excluding tests) -> JSON, text, ranked markdown
ports/vscode/scripts/vscode-patches.sh check     # trailers present + the whole series applies (CI)
```

`VSCODE_DIR`, `W3COS_BIN` and `LEAK_LINT_REPORT_DIR` override the checkout, the w3cos binary and the report
directory.

**Moving the pin**: update the lock, run `apply`, and resolve conflicts with `git am --3way`. Then run
`refresh`, and re-run `lint` to regenerate `docs/perf/code-oss-leak-lint.md`.

**Choosing patches**: start from the ranked report and only take findings where the lifetime change is
obviously correct:
- registering a disposable on an owner that is already disposed;
- a WeakMap for a private cache that is never iterated;
- a WeakRef back pointer whose target is owned elsewhere.

A wrong WeakRef frees an object early, which is worse than a leak. When unsure, leave the finding for the
runtime leak detector and soak to confirm. Hot data-structure back pointers (red-black tree / interval tree
`parent`, linked-list `prev`) are deliberately not patched: a `deref()` on every step would cost too much,
and they need a compiler-level weak field instead.

## Patches

| patch | upstream | what |
| --- | --- | --- |
| 0001-extHostTerminalService-register-terminal-emitters | yes | 12 Emitters in `ExtHostPseudoterminal` / `BaseExtHostTerminalService` were never disposed; wrapped in `this._register(...)` (generated with `w3cos leak-lint --fix`) |
