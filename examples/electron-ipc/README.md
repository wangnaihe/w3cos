# electron-ipc

A minimal Electron app that exercises the `@w3cos/electron` adapter
(`docs/W3COS_ELECTRON.md`): app lifecycle, a `BrowserWindow` with a preload,
`contextBridge.exposeInMainWorld`, `ipcRenderer.invoke` → `ipcMain.handle`,
`ipcRenderer.send` → `ipcMain.on` → `event.reply`, `webContents.send`, and
the close → `window-all-closed` → quit chain.

It runs unchanged under Electron (`npx electron .`) and is AOT-compiled by
w3cos. `crates/w3cos-compiler/src/electron_e2e_tests.rs` compares both
outputs line by line (reference recorded with Electron 44.9.0).
