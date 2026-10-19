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

## Platform services: protocol, nativeTheme, screen, safeStorage

These are the main-process services Code-OSS needs before it can show its
workbench (roadmap M3 item 21).

- **`protocol` / `net` / `session`.** Code-OSS registers `vscode-file` as a
  privileged scheme and serves the workbench from
  `vscode-file://vscode-app/<absolute path>`. Handlers run in-process.
  `protocol.handle(scheme, handler)` takes a Request and returns a Response,
  usually `net.fetch('file://…')`. `registerFileProtocol` /
  `interceptFileProtocol` take the legacy `callback({ path })` form.
  `loadURL()` of a handled scheme asks the handler which file backs the URL
  and shows that compiled page; `getURL()` keeps the `vscode-file://` URL.
  `net.fetch()` and renderer `fetch()` of a privileged scheme with
  `supportFetchAPI` go through the handler too. A Response that did not
  come from a file (for example a 403 from the handler) cannot become a
  page, because pages are compiled in, so that load fails `ERR_FAILED`.
  Registering a scheme twice, or a built-in scheme, throws
  `Failed to register protocol: <scheme>`, as Electron does.
  `registerSchemesAsPrivileged` must run before `ready`.
  `session.defaultSession` (also `webContents.session`) carries `protocol`,
  `fetch` and no-op cache, proxy and permission hooks.
- **`nativeTheme`.** `themeSource` accepts `system` / `light` / `dark` and
  rejects other values with Electron's conversion `TypeError`.
  `shouldUseDarkColors` follows the source, or `prefers-color-scheme` for
  `system`. Changes made within one turn emit a single `updated` event.
  High-contrast, inverted, forced-colors and reduced-transparency report
  `false`.
- **`screen`.** One display: the surface the window system gives the app
  (`window.screen`, `devicePixelRatio`). It has Electron's full `Display`
  shape (bounds, workArea, size, scaleFactor, rotation, …).
  `getDisplayNearestPoint` / `getDisplayMatching` return that display, and
  `getCursorScreenPoint` is the last pointer position. Like Electron, the
  module throws when used before `ready`.
- **`safeStorage`.** Uses the runtime's local keyring instead of the OS
  keychain, libsecret or kwallet, so encryption is always available, works
  offline, and the secret never leaves the machine
  (`w3cos-runtime/src/node_safe_storage.rs`). Blobs are sealed with
  AES-256-GCM: a `w3c1` prefix, a random nonce, then the ciphertext and
  tag. The key is 32 random bytes in `<keyring>/safe-storage.key`, file mode
  0600 inside a 0700 directory, created on first use. `<keyring>` is
  `$W3COS_KEYRING_DIR`, otherwise `$XDG_DATA_HOME/w3cos/keyring`.
  `getSelectedStorageBackend()` is `w3cos_keyring`.
  `setUsePlainTextEncryption` is a no-op. Errors match Electron's messages:
  a non-Buffer argument, data that is not a w3cos blob ("does not appear to
  be encrypted"), or a tampered blob ("Decryption failed."). Blobs written
  by Electron's own backends cannot be read.

The CI test compiles `examples/electron-platform` and compares its output
line for line with Electron 44.9.0, run under xvfb with the Linux
`basic_text` backend (`password-store=basic` plus
`setUsePlainTextEncryption(true)`). The test uses a throwaway
`W3COS_KEYRING_DIR`.

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
  `nativeTheme.themeSource` does not yet override the renderer's
  `prefers-color-scheme`.
- `screen` reports one display and emits no `display-*` events yet.
- `net.fetch` of http(s) uses the runtime's `fetch`; `session.webRequest`
  hooks are accepted but never called.
- Not provided yet: `clipboard`, `Tray`, `autoUpdater`, `crashReporter`.
