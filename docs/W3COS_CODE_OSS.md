# Code-OSS on w3cos: AOT milestone (M3 item 23)

Item 23 of `docs/roadmap/aot-port.md` is the M3 acceptance test: Code-OSS
(`--disable-extensions`) is AOT-compiled and shows the workbench. This page
tracks how far the compiler gets through Code-OSS and what blocks the next
step.

## Reproducing

Use the commit locked in `tools/vscode-baseline/CODE_OSS.lock.json`. Its
`build/next` transpiler needs Node 24 (`.nvmrc`).

```sh
git init vscode && cd vscode
git remote add origin https://github.com/microsoft/vscode.git
git fetch --depth 1 origin e97039dd9293ef2b9df8876d0bc8c0c8ff87f871 && git checkout FETCH_HEAD
npm ci --ignore-scripts && (cd build && npm ci --ignore-scripts)
npm run transpile-client          # src → out (esbuild, ~10k modules)
```

`--ignore-scripts` is enough, because no native addon or Electron download is needed. w3cos
replaces the native packages (`docs/W3COS_NATIVE_MODULES.md`), and the
Electron API comes from `w3cos-node`'s `electron` glue (`docs/W3COS_ELECTRON.md`).

The sweep walks each process entry through the resolver, the bundler and
the code generator, then writes a JSON report:

```sh
W3COS_CODE_OSS=/path/to/vscode \
W3COS_CODE_OSS_REPORT=/tmp/code-oss-aot.json \
  cargo test -p w3cos-compiler --test code_oss_graph -- --ignored --nocapture
```

Entries: `out/main.js` (Electron main),
`out/vs/base/parts/sandbox/electron-browser/preload.js` and
`out/vs/code/electron-browser/workbench/workbench.js` (renderer).

## What the first slice adds

- **`createRequire` in ES modules** (`w3cos-compiler/src/create_require.rs`).
  Code-OSS bootstraps load `product.json`, `package.json` and builtins through
  `createRequire(import.meta.url)`. The compiler turns every call with a
  literal specifier on such a binding into a static import of that module.
  - A JSON file or CommonJS module imports its value.
  - An ES module imports its namespace.
  - A specifier that does not resolve becomes an expression that throws
    `MODULE_NOT_FOUND`, so `try { require('../product.overrides.json') } catch {}`
    still works.
  - A computed specifier calls the runtime `require`, which throws
    `ERR_W3COS_DYNAMIC_REQUIRE`.
- **`node:module`** (`w3cos-node/js/module.js`):
  - Implemented: `createRequire`, `isBuiltin`, `builtinModules`, and the
    `Module` statics Code-OSS patches (`_resolveLookupPaths`,
    `_nodeModulePaths`, `globalPaths`, `_cache`, `_extensions`).
  - No-ops: `register` and `registerHooks` (nothing to hook in a linked app).
  - `enableCompileCache` reports `DISABLED`.
- **Builtins the Electron main graph loads**. Each one evaluates and exposes
  Node's shapes. An operation that is not implemented throws
  `ERR_W3COS_UNSUPPORTED` when called, not at link time:

  | Module | Status |
  |---|---|
  | `v8` | heap statistics from `process.memoryUsage()`; snapshots and `serialize` unsupported |
  | `perf_hooks` | global `performance`, `PerformanceObserver` (no entries delivered), histograms |
  | `async_hooks` | `AsyncLocalStorage` / `AsyncResource` for synchronous scopes; context does not follow `await` or timers yet |
  | `diagnostics_channel` | channels, `subscribe`/`publish`, `tracingChannel` |
  | `readline` | `createInterface` line splitting, `question`, async iteration; no terminal editing |
  | `dns` | IP literals and `localhost`; name lookups unsupported |
  | `http`, `https`, `http2`, `tls` | constants, `STATUS_CODES`/`METHODS`, `Agent`, classes; requests, servers and TLS connections unsupported |
  | `tty`, `console` | `isatty` from `process.std*.isTTY`; `Console` over the global console |
  | `node:sqlite` | `DatabaseSync` throws (the `@vscode/sqlite3` replacement is wired) |
  | `original-fs` | Electron's unpatched `fs`, mapped to `fs` (w3cos has no asar) |

- Compiler fixes found on the way:
  - ESM JSON imports emitted integers as `Value::Number(1)`, which does not
    compile. They now emit float literals, and every import of a JSON file
    now sees the same object.
  - `const y = x`, where `x` is the default import of a CommonJS module,
    read `undefined`. It now resolves to that module's export.

`fixtures/code_oss_boot/` reproduces these bootstrap patterns.
`code_oss_bootstrap_builtins_match_node` must print exactly what Node 20.19
prints for it.

## Status of the sweep (Code-OSS e97039dd)

| Entry | Modules linked | Unresolved bindings | Codegen |
|---|---|---|---|
| `out/main.js` | 45 | 0 | ok |
| preload | 1 | 0 | ok |
| workbench | 2 | 0 | ok |

Before this slice, `out/main.js` stopped at the resolver: `node:module` was
missing, then `node:v8`.

## Next blockers

1. **Literal dynamic `import()`**. `main.js` runs `await import("./mainImpl.js")`,
   which runs `await import("./vs/code/electron-main/main.js")`. Graph discovery
   only follows static imports, so the Electron main graph (about 600 modules
   according to an esbuild metafile of the same `out/`) is not linked yet.
   Literal dynamic imports must join the graph as lazily evaluated modules.
2. **Workbench renderer**. `workbench.js` imports
   `vs/workbench/workbench.desktop.main.js` through a computed URL built from
   the `vscode-file://` root. The compiler must learn this entry, via the
   page-script and Electron `loadURL` mapping from item 21.
3. **Networking builtins** (`http`/`https`/`tls`/`dns`) need host-backed
   implementations before the proxy agent and telemetry paths do any real
   work. Under `--disable-extensions`, they only have to load.
4. **Compiler**: an arrow function that captures `this` inside a class
   constructor lost its effect when the instance was built from a timer
   callback (seen in `readline`; the glue avoids the pattern). This needs a
   minimal repro and a fix.
