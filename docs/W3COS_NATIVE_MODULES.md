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
| `@parcel/watcher`, `@w3cos/watcher` | `js/watcher.js` | `w3cos-runtime/src/node_watcher.rs` over `fs_watch.rs` |
| `native-keymap`, `@w3cos/native-keymap` | `js/native_keymap.js` | none (static US layout) |
| `@vscode/policy-watcher`, `@w3cos/policy-watcher` | `js/policy_watcher.js` | none |

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

## `@parcel/watcher`

The glue covers `subscribe(dir, fn, { ignore })`, which resolves to
`{ unsubscribe() }`, plus `unsubscribe`, `writeSnapshot` and
`getEventsSince`.

- Each subscription is a recursive `FileSystemObserver` from `fs_watch.rs`.
  It is the portable polling backend: it scans every 100 ms, and this PR
  changes its diff to look entries up by path. The glue drains it on the
  shared host poll loop.
- `ignore` takes paths (prefix match), globs (`**`, `*`, `?`, `{a,b}`,
  `[...]`, relative to the watched dir) or RegExps.
- Each batch is coalesced the way parcel coalesces it: create+delete →
  nothing, create+update → create, delete+create → update. Directory mtime
  changes are dropped, as in parcel.
- A missing directory rejects with parcel's Linux message (`Bad file
  descriptor`). Snapshots are a JSON listing of mtimes and sizes.

## `native-keymap`

`getKeyMap()` returns the X11/xkb US (pc105) table exactly as the native
module reports it on Linux: 183 codes, `value` / `withShift` /
`withAltGr` / `withShiftAltGr` / `withLevel5` / `withLevel3Level5`.
`getCurrentKeyboardLayout()` returns `{ model: 'pc105', layout: 'us', … }`,
and `isISOKeyboard()` returns `undefined` (the native module only answers
on macOS). The layout never changes. A host keyboard-layout adapter
(xkbcommon / winit) is the follow-up.

## `@vscode/policy-watcher`

On Linux the native module has no backend, and `createWatcher` throws
`TypeError: Unsupported platform`. Code-OSS falls back to file-based
policy in that case. w3cos does the same on every platform for now:
Windows registry and macOS managed-preferences policy are not read.

## Tests

`crates/w3cos-compiler/src/node_native_e2e_tests.rs` AOT-compiles
`fixtures/node_native/{sqlite3,spdlog,watcher,keymap_policy}.js`. The
output is compared line by line with Node 20.19 running the real
`@vscode/sqlite3` 5.1.14-vscode, `@vscode/spdlog` 0.15.8,
`@parcel/watcher` 2.6.0, `native-keymap` 3.3.9 and
`@vscode/policy-watcher` 1.6.0.

## Not yet

- The file watcher polls. An inotify/FSEvents backend (the `notify`
  crate is already a dependency) would scale better to large workspaces.
- `native-keymap` has only the static US layout; there is no layout
  detection or change events.
- sqlite3: `Backup`, `loadExtension`, and `interrupt()` (a no-op) are not
  done.
