# `@w3cos/electron`: Electron apps, AOT-compiled

Status: M2 slice (roadmap `docs/roadmap/aot-port.md` §6, items 17–18).
Glue: `crates/w3cos-node/js/electron.js`. Compile-time half:
`crates/w3cos-compiler/src/electron_app.rs`. Tests:
`crates/w3cos-compiler/src/electron_e2e_tests.rs` (in CI through
`cargo test -p w3cos-compiler --lib`).

An Electron app is compiled like any CommonJS app (`w3cos build main.js`).
`require('electron')`, `electron/main`, `electron/renderer`,
`electron/common` and `@w3cos/electron` all resolve to the adapter, even
when the `electron` npm package is installed. That package is only the
Chromium launcher, which the compiled app replaces. The adapter is a module,
not a global: there is no `electron.*` or `w3cos` object on `window`.

## How an app maps onto one w3cos process

| Electron | w3cos |
|---|---|
| main process | the AOT-compiled entry and its modules |
| renderer of the first `BrowserWindow` | the process's own DOM (`document`, `window`); in GUI mode (`W3COS_AOT_WINDOW=1`) that is the w3cos window |
| `loadFile('index.html')` | the page compiled in: title, `<head>` (local stylesheets inlined as `<style>`) and `<body>` are split at compile time; `<script src>` files are compiled and run in document order after the markup, then `DOMContentLoaded` and `load` fire |
| `webPreferences.preload` | compiled in; runs before the page scripts |
| `contextBridge.exposeInMainWorld` | a copy of the API on `window` (functions wrapped, objects frozen) |
| `ipcMain` / `ipcRenderer` | in-process messages, one task per message (Electron's ordering), arguments and results structured-cloned; `invoke`/`handle`, `send`/`on`, `event.reply`, `sendSync` |
| `app` lifecycle | `ready` / `whenReady` after the main script's synchronous part; `browser-window-created`, `window-all-closed` (or quit when nobody listens), `before-quit`, `will-quit`, `quit`, then the process exits |

### Compile-time discovery

When a module requires `electron`, the compiler scans the project (not
`node_modules`, `target` or dot directories) and writes a generated
`electron_app.js` next to the glue:

- every `.html` page, split as above;
- every page's local `<script src>`;
- preloads: files named `preload*`, or files that use `contextBridge` or
  `ipcRenderer` and do not create windows.

These renderer-side scripts are classic scripts loaded on demand, so a file
without `require`/`module`/`exports` still becomes a lazily evaluated
CommonJS record instead of running at startup.

## Native affordances: standards first

Following `docs/W3COS_EXTENSIONS.md`, the adapter uses a W3C standard or
draft wherever one exists:

| Electron | standard object | headless / refused |
|---|---|---|
| `dialog.showOpenDialog` (`openFile`, `multiSelections`, `openDirectory`) | `window.showOpenFilePicker()` / `showDirectoryPicker()` (File System Access) | `{ canceled: true, filePaths: [] }` when the picker rejects `AbortError` / `NotAllowedError` / `SecurityError` / `NotSupportedError` |
| `dialog.showSaveDialog` | `window.showSaveFilePicker()` | `{ canceled: true, filePath: '' }` |
| `dialog.showMessageBox(Sync)` | `window.alert()` (one button) / `window.confirm()` | `confirm` refused → `cancelId` |
| `dialog.showErrorBox` | `window.alert()` | `console.error` without a DOM |
| `shell.openExternal` | `window.open(url, '_blank', 'noopener')` | rejects `NotSupportedError` without a DOM |
| `BrowserWindow#setTitle` / `getTitle` | `document.title` | |

There is no W3C API for `shell.openPath`, `showItemInFolder` or
`trashItem`; they report failure (`openPath` resolves with an error
message as Electron does, `trashItem` rejects `NotSupportedError`) rather
than pretending to succeed. A future `navigator.w3cos.shell` capability
would back them.

File System Access handles do not expose host paths. `filePaths` holds the
handle's `path` when the runtime provides one, otherwise its `name`; the
handles themselves are returned as `handles` / `handle`.

## Headless runs and tracing

- `W3COS_ELECTRON_TRACE=1` prints `[electron] window-created WxH`,
  `[electron] page-loaded "<title>" "<body text>"`,
  `[electron] window-all-closed` and `[electron] will-quit`.
- `W3COS_ELECTRON_EXIT_AFTER_LOAD=1` closes each window once its page has
  loaded, so a headless CI run ends through the app's own quit path.

The CI test compiles `examples/electron-quick-start` (the upstream
quick-start, CC0, unmodified) and `examples/electron-ipc` and compares the
output with Electron 44.9.0. Version numbers are normalized:
`process.versions.node` is w3cos-node's level, `process.versions.electron`
is the API level the adapter is tested against (44.9.0), and there is no
`process.versions.chrome`.

## Known gaps

- One renderer realm per process. A second `BrowserWindow` exists (ids,
  events, `getAllWindows`) but `loadFile` on it fails `ERR_FAILED`;
  multi-window is multiple runtime instances (roadmap §6, later).
- Renderer scripts run as compiled modules, so `require`/`process` are
  visible inside them (`window.require` / `window.process` stay
  `undefined`). Preloads and page scripts share the realm with the main
  process; there is no context isolation boundary yet.
- No `eval`: `webContents.executeJavaScript` rejects `NotSupportedError`.
  Inline `<script>` blocks are dropped; page scripts with `import`/`export`
  are linked as ES modules and therefore evaluate at startup.
- Remote pages (`loadURL('https://…')`) fail `ERR_DISALLOWED_URL_SCHEME`.
- Window geometry (`setSize`, `setPosition`, …) and `show`/`hide` are
  tracked but do not resize or hide the native window yet; the w3cos window
  keeps the shell viewport size.
- `Menu` / `MenuItem` keep the model only (no native menu bar);
  `nativeTheme.shouldUseDarkColors` reads `prefers-color-scheme`.
- Not provided yet: `utilityProcess`, `MessageChannelMain`, `protocol`,
  `session`/`net`, `clipboard`, `screen`, `Tray`, `safeStorage`,
  `autoUpdater`, `crashReporter`.
