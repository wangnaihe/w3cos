# w3cos-node: `child_process`, `net`, workers and the permission layer

M3 item 19 (`docs/roadmap/aot-port.md`). Slice 1 (`child_process`, `net`,
permissions) glue:
`crates/w3cos-node/js/{child_process,net,w3cos_poll}.js`. Host half:
`crates/w3cos-core/src/node_host_io.rs` (functions on
`__w3cos_node_host`, next to the fs ones in `node_host.rs`).

## Model

- Host functions never block the event loop. Child pipes are read and
  written on host threads. Sockets are non-blocking. One timer loop,
  `node:internal/w3cos_poll`, polls open handles. It polls every 1 ms while
  there is traffic and backs off to 16 ms when idle. Only referenced handles
  (`ref()` / `unref()`) keep the process alive.
- Sync APIs (`spawnSync`, `execSync`, `execFileSync`) run on the calling
  thread. They support `input`, `timeout` and `killSignal`, as Node does.
- Errors have Node's shape: `spawn X ENOENT` with `errno` / `syscall` /
  `path` / `spawnargs`, `connect ECONNREFUSED 127.0.0.1:port`,
  `listen EADDRINUSE: address already in use host:port`.

## Permission layer (local-first / sandbox)

Every `spawn*` / `exec*` call, every `listen` and every `connect` first asks
the host:

- `w3cos_core::node_host_io::set_permission_policy(Some(Arc::new(|scope,
  resource| ...)))` installs the embedder's decision function, once per
  process. Scopes are `child` (resource = the executable), `net` (a UDS
  path or `host:port`) and `worker` (resource = the worker module path). This is where
  a shell's permission sheet or the runtime capability gate plugs in.
- With no policy installed, everything is allowed except the scopes listed
  in `W3COS_NODE_DENY` (comma-separated: `child`, `net`, `worker`, `*`).
- A refusal throws synchronously with the error shape of Node's permission
  model: `code: 'ERR_ACCESS_DENIED'`, `permission: 'ChildProcess' | 'Net' | 'WorkerThreads'`,
  `resource`, message `Access to this API has been restricted`.
  `process.permission.has('child')` reports the same decision.

## Workers (slice 2): `worker_threads`, `utilityProcess`, `MessageChannelMain`

Glue: `crates/w3cos-node/js/{worker_threads,w3cos_worker,w3cos_clone}.js`
and `electron.js`. Host half: `crates/w3cos-core/src/node_workers.rs`.

- A worker is an OS thread with its own JS realm. Module state is
  thread-local, so the thread runs the program's `run_entry` again. The
  entry's CommonJS record starts with a boot prelude: on the main thread it
  does nothing, and on a worker thread it requires the worker module and
  returns. The worker thread then runs its own microtask/timer loop until
  nothing is pending, `process.exit()` is called, or the parent calls
  `terminate()`.
- The compiler links worker modules it can name statically:
  `new Worker(X)` / `new x.Worker(X)` / `utilityProcess.fork(X)`, where X
  is `__filename`, `path.join|resolve(__dirname, 'lit', ...)`,
  `__dirname + '/lit'`, `` `${__dirname}/lit` ``, or a `const` bound to one
  of those. Worker files run as CommonJS, as in Node, even without
  `require`/`module`. A worker path the compiler cannot see, `eval: true`,
  or an ES-module worker file throws `ERR_W3COS_UNSUPPORTED` at
  `new Worker()`.
- Messages are structured-cloned (`node:internal/w3cos_clone`): primitives,
  bigint, Date, RegExp, Map, Set, ArrayBuffer, typed arrays, DataView,
  errors, cycles. Functions and symbols throw `DataCloneError`.
- `worker_threads`: `isMainThread`, `threadId`, `workerData`, `parentPort`,
  `Worker` (`online` / `message` / `error` / `exit`, `terminate()`
  resolving to the exit code, `ref`/`unref`), `MessageChannel` /
  `MessagePort` within a thread, `receiveMessageOnPort`,
  `setEnvironmentData` / `getEnvironmentData`.
- Electron: `utilityProcess.fork(modulePath, args, { serviceName })` runs
  the module as a worker with `process.type === 'utility'` and
  `process.parentPort` (`message` events carry `{ data, ports }`).
  `UtilityProcess` emits `spawn` / `message` / `exit`, has `postMessage`,
  `kill()` and `pid`. `MessageChannelMain` / `MessagePortMain` work within
  the main process. `app.quit()` kills live utility processes.
- Every `new Worker()` / `utilityProcess.fork()` asks the permission layer
  (scope `worker`). A refusal throws `ERR_ACCESS_DENIED` with
  `permission: 'WorkerThreads'`, as `node --experimental-permission` does.

## Tests

`crates/w3cos-compiler/src/node_io_e2e_tests.rs` AOT-compiles each fixture
in `fixtures/node_io/` and compares the output line by line with Node
20.19:

- `child_process`: spawn, exit/close order, stdin, ENOENT, kill, exec,
  execFile, promisify, and the sync APIs.
- `net`: UDS and TCP echo, half-close order, addresses, getConnections,
  and the ENOENT / EADDRINUSE / ECONNREFUSED errors.
- `permission`: `W3COS_NODE_DENY=child` compared with `node
  --experimental-permission`.
- `worker_threads`: workerData, round trips with clone-heavy payloads,
  worker errors, `process.exit` codes, `terminate()`, MessageChannel,
  `receiveMessageOnPort`.
- `worker_permission`: `W3COS_NODE_DENY=worker` compared with `node
  --experimental-permission`.
- `electron_e2e_tests.rs` (`examples/electron-utility`): `utilityProcess`
  and `MessageChannelMain`, compared with Electron 44.9.0.
- ES5-constructor classes: `class X extends F` running F's body, which
  `net.Socket extends Duplex` needs.

## Not yet

- `fork` and `stdio: 'ipc'` throw `ERR_W3COS_UNSUPPORTED`. They need the
  binary to re-exec itself with a selected entry and an IPC socket.
- Workers stop cooperatively: `terminate()` cannot interrupt a busy
  synchronous JS loop. `SharedArrayBuffer`/`Atomics` sharing, transferring
  `MessagePort`s across threads, and `resourceLimits` are not done.
  A utility process shares the main process's pid, and `stdout`/`stderr`
  are `null` (output is inherited).
- TCP `connect` resolves and connects synchronously. That is fine for
  loopback and LAN, but slow remote hosts stall the loop. A host-thread
  connect is the follow-up.
- Named pipes on Windows, `dgram`, `tls`, and `http` on top of `net` are
  not done yet.
