# w3cos-node: native-module replacements

M3 item 20 (`docs/roadmap/aot-port.md`). Code-OSS depends on npm packages
that ship a `.node` addon. An AOT app cannot load those, so `w3cos-node`
provides glue with the same API. The glue is AOT-compiled with the app like
the Node builtins.

The compiler resolves these specifiers to the glue even when the package is
installed (`w3cos_node::NATIVE_REPLACEMENTS`):

| Package | Glue | Host half |
|---|---|---|
| `@vscode/sqlite3`, `@w3cos/sqlite3` | `js/sqlite3.js` | `w3cos-runtime/src/node_sqlite.rs` (the runtime's SQLite) |
| `@vscode/spdlog`, `@w3cos/spdlog` | `js/spdlog.js` | none (plain `fs`) |

## `@vscode/sqlite3`

This is node-sqlite3's API as Code-OSS storage uses it
(`src/vs/base/parts/storage/node/storage.ts`):

- `Database` (mode flags, open callback, `'open'` / `'close'` / `'error'` /
  `'trace'` / `'profile'` events).
- `exec`, `run` (`this.lastID` / `this.changes`), `get`, `all`, `each`,
  `map`, `prepare` → `Statement` (`bind`, `run`, `get`, `all`, `each`,
  `reset`, `finalize`).
- `serialize` / `parallelize`, `configure('busyTimeout')`, `close`, and
  `verbose()` / `cached.Database`.

Parameters can be positional (`?`), an array, or named (`$k`, `:k`,
`@k`). Values convert as node-sqlite3 converts them: blobs come back as
`Buffer`, and booleans and Dates are bound as numbers. Errors have
node-sqlite3's shape (`SQLITE_ERROR: no such table: x`, `code`, `errno`).

Statements run synchronously on the calling thread, on connections owned by
that thread. Callbacks are queued per database in call order and delivered
asynchronously.

The host functions live on `__w3cos_node_host`. The runtime adds them
through `w3cos_core::node_host::register_extension`, which is the hook
other runtime-backed replacements use too.

## `@vscode/spdlog`

The glue is spdlog's rotating file logger: `createRotatingLogger`,
`createAsyncRotatingLogger`, `Logger` (`trace` … `critical`,
`getLevel` / `setLevel`, `setPattern`, `clearFormatters`, `flush`,
`drop`), and the module-level `setLevel`, `setFlushOn`, and `version`.

- Level names and pattern flags (`%Y-%m-%d %H:%M:%S.%e`, `%n`, `%l`, `%L`,
  `%v`, `%z`, `%P`, …) match spdlog.
- Rotation matches spdlog: `main.log` → `main.1.log` → … `main.N.log`. A
  rotation happens whenever a record would push the file past
  `maxFileSize`.
- Writes are synchronous, so `flush()` has nothing left to write.

## Tests

`crates/w3cos-compiler/src/node_native_e2e_tests.rs` AOT-compiles
`fixtures/node_native/{sqlite3,spdlog}.js`. The output is compared line by
line with Node 20.19 running the real `@vscode/sqlite3` 5.1.14-vscode and
`@vscode/spdlog` 0.15.8.

## Not yet

- `@parcel/watcher` (→ `fs_watch.rs`), `native-keymap`,
  `@vscode/policy-watcher`, and other stubs are the next slice.
- sqlite3: `Backup`, `loadExtension`, and `interrupt()` (a no-op) are not
  done.
