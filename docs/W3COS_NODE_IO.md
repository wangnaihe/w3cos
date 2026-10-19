# w3cos-node: `child_process`, `net` and the permission layer

M3 item 19, first slice (`docs/roadmap/aot-port.md`). Glue:
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
  path or `host:port`) and `worker` (reserved for slice 2). This is where
  a shell's permission sheet or the runtime capability gate plugs in.
- With no policy installed, everything is allowed except the scopes listed
  in `W3COS_NODE_DENY` (comma-separated: `child`, `net`, `worker`, `*`).
- A refusal throws synchronously with the error shape of Node's permission
  model: `code: 'ERR_ACCESS_DENIED'`, `permission: 'ChildProcess' | 'Net'`,
  `resource`, message `Access to this API has been restricted`.
  `process.permission.has('child')` reports the same decision.

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
- ES5-constructor classes: `class X extends F` running F's body, which
  `net.Socket extends Duplex` needs.

## Not yet

- `fork` and `stdio: 'ipc'` throw `ERR_W3COS_UNSUPPORTED`. They need the
  AOT child-process IPC channel (slice 2, with `utilityProcess`).
- TCP `connect` resolves and connects synchronously. That is fine for
  loopback and LAN, but slow remote hosts stall the loop. A host-thread
  connect is the follow-up.
- Named pipes on Windows, `dgram`, `tls`, and `http` on top of `net` are
  not done yet.
