# IndexedDB 3.0 conformance plan

> Status: implementation baseline · 2026-07-22
> Normative target: <https://www.w3.org/TR/IndexedDB/>
> Backend choice: SQLite is private to the user agent and is not an application API.

## Public contract

Applications use the standard global and interfaces:

```ts
const request = indexedDB.open('app-local', 1)

request.onupgradeneeded = () => {
  const db = request.result
  db.createObjectStore('items', { keyPath: 'id' })
}

request.onsuccess = () => {
  const transaction = request.result.transaction('items', 'readwrite', {
    durability: 'strict',
  })
  transaction.objectStore('items').put({ id: '1', value: 'offline' })
}
```

W3COS must not require `w3cos.indexedDB`, an upgrade callback argument to `open`, Promise-returning object-store methods, SQL strings, or a native-extension import. A convenience Promise wrapper may live in application code, but is not part of the platform contract.

## Required semantics

- `globalThis.indexedDB` is an `IDBFactory`; operations return `IDBRequest` / `IDBOpenDBRequest` immediately and complete through EventTarget dispatch.
- Database identity is scoped by the W3C storage key and installed application identity, not only by a caller-controlled database name.
- Values cross the storage boundary through the HTML structured clone algorithm. Unsupported values fail with `DataCloneError`.
- Keys, compound keys, key paths and `IDBKeyRange` follow the specification ordering and validation algorithms.
- Schema mutation is only allowed inside the exclusive `versionchange` transaction created by `open`.
- Requests inside one transaction execute in order. Overlapping `readwrite` scopes are scheduled in creation order and never observe partial writes.
- `abort()` rolls back records, indexes and schema changes atomically. A `complete` event is emitted only after the selected durability boundary has been reached.
- Connections participate in `versionchange` / `blocked`; closing or terminating a context releases its locks.
- Failures use standard `DOMException` names, including `AbortError`, `ConstraintError`, `DataCloneError`, `DataError`, `InvalidStateError`, `NotFoundError`, `ReadOnlyError`, `TransactionInactiveError` and `VersionError`.
- `indexedDB.databases()`, stores, indexes, unique/multiEntry behavior, cursors, `getAll`, `getAllKeys`, `count`, `deleteDatabase`, explicit `commit` and durability hints are covered by conformance tests.

## SQLite mapping

SQLite implements persistence, not the API surface. The backend owns metadata for databases, object stores, indexes, connections and schema versions. Records store a canonical encoded key plus structured-clone bytes. A W3C `readwrite` or `versionchange` transaction maps to one real SQLite transaction; no record is committed per request.

Platform defaults:

- WAL where supported by the target filesystem.
- Parameterized internal SQL only.
- `strict` durability reaches an fsync-equivalent boundary before `complete`; `relaxed` may use the operating-system cache.
- Database paths are derived from the storage key and application identity and cannot be selected as arbitrary filesystem paths by script.
- Quota, disk-full, corruption and migration failures abort the active IDB transaction without exposing a partially upgraded database.
- Encryption keys come from the platform credential store. Encryption is an implementation policy and does not change the IndexedDB API.

## Current implementation

`crates/w3cos-runtime/src/indexed_db.rs` is backed by private SQLite files and remains hidden behind `indexed_db_web.rs`. Transactions stage mutations in an isolated snapshot, publish the complete scope on commit, discard it on abort, order overlapping scopes, remove aborted queued transactions, roll back failed schema upgrades, honor `preventDefault()` on cancelable request errors, and propagate uncanceled errors through request, transaction and database targets. Open and delete notify live connections with `versionchange`, remain pending with one `blocked` event, and resume after the final blocker closes.

The SQLite adapter uses WAL, `synchronous=FULL`, parameter binding and `BEGIN IMMEDIATE`/`COMMIT`. Persistence is normalized across metadata, object-store, index, record and index-entry tables; the former singleton snapshot table is read only for migration and then cleared. Storage paths are derived from application/storage identity, caller database names are hashed before becoming filenames, quota is checked before publication, and a real subprocess-abort test proves that committed data survives while an uncommitted SQLite transaction disappears.

## Delivery gates

1. **IDB surface — baseline landed**: `globalThis.indexedDB`, asynchronous request/open-request events, version handling, upgrade transactions, object stores, indexes, ranges, cursors, connection blocking, explicit commit, durability hints, `cmp` and `databases()` execute without W3COS imports. Complete constructor/prototype identity remains open.
2. **Atomic engine — baseline landed**: commit/abort, automatic completion, overlapping-scope creation order, queued-abort removal, request-error cancellation, failed-upgrade rollback, error propagation and open/delete `versionchange → blocked → close → resume` are tested. Exact HTML task active/inactive boundaries remain open.
3. **SQLite backend — baseline landed**: normalized tables, WAL/FULL durability, legacy migration, storage-scope path isolation, quota fail-closed behavior, fresh-registry reopen and subprocess abrupt-termination proof are covered. Platform credential-store encryption and disk-full/corruption recovery remain open.
4. **Keys/query/clone — baseline landed**: number, string, Date, binary and compound keys retain type and order; compound key paths, `IDBKeyRange`, unique/multiEntry indexes and request-reusing cursors are covered. Structured clone preserves undefined, special numbers, Date, binary, cyclic and shared Array/Object graphs. Map, Set, Blob, File, RegExp and cursor update/delete remain open.
5. **Conformance — pinned adapted subset landed**: `tests/wpt/indexeddb-subset.json` pins WPT revision `f64f3e13f0c456553639fd5c30a438204cc5dfe3` and maps 12 upstream files to JavaScript-visible Rust assertions: 12 covered, 0 failed, 0 skipped. The gate rejects missing mappings and every non-`covered` status. This is an adapted assertion subset, not a claim that the raw upstream WPT harness or the full IndexedDB suite passes.
6. **Production hardening — in progress**: iOS build is a release gate; Android build/device, OS-level background termination, power-loss injection, encrypted-at-rest policy and disk-full/corruption recovery remain platform-specific gates.

The existing Rust unit tests remain backend tests. They are not substitutes for Web Platform Tests against the JavaScript-visible standard API.
