# Observable binding regression check

The PR55 AOT DOM calls and fused style writes could bypass authored methods,
accessors, exceptions, and evaluation order. Tagged node reads also ignored
properties and prototypes added after materialization. This repair uses the
ordinary call/set path and memoized collectable node wrappers. Own getters
resolve before inherited data, run with the actual Element receiver, and can
reenter after the proxy borrow is released. Dynamic Proxy default definitions
update both the original target and its existing reflection storage without
holding the proxy alive.

The check runs the original 31 cases plus nine getter/reentry/order boundaries
in `semantic-regressions.ts`. Each native case gets a fresh process, so an old
baseline crash cannot hide later results. `semantic-const.ts` remains separate:
the old immutable-assignment compile rejection is recorded rather than fixed.
Five/six explicitly listed old baseline differences remain visible in the JSON;
no new boundary failure is accepted as an unnamed baseline debt.

With already-installed Rust, the CLI, Python/Playwright, and Chrome, build one
independent test program per pinned checkout using its corresponding CLI:

```sh
python3 tools/perf-native-browser/build_semantics.py \
  --cli /path/to/checkout/target/release/w3cos \
  --output /path/to/new-independent-test-directory \
  --lock /path/to/retained/native.Cargo.lock
```

The output must be new. The helper is offline, declines the CLI's automatic
build in a temporary shim, then builds the retained source with the supplied
lock and production profile. The generated entry's environment selector is
test-only. It does not install toolchains or change the checkout.

Run the same source against Chrome, old `4bbbfd0`, frozen PR55 `820786bb`, and
the repaired binary:

```sh
python3 tools/perf-native-browser/check_semantics.py \
  --baseline /path/to/4bbb-test/semantic-test \
  --control /path/to/820786bb-test/semantic-test \
  --candidate /path/to/repaired-test/semantic-test \
  --chrome /path/to/installed/chrome \
  --output /path/to/new-evidence-directory
```

Normal Chromium sandbox and identical script bytes are required. All native
stdout/stderr, values, exit states, and binary/source hashes are retained.
Performance uses the separate unchanged `tools/perf-paired` fixture and
production profile only after this correctness gate passes. The rollback may
cost time or allocations; the change makes no browser-speed promise and does
not repair the complete realm/WebIDL contract.
