const { app, BrowserWindow, ipcMain } = require('electron')
const path = require('node:path')

let win
ipcMain.on('line', (_event, line) => console.log(line))
ipcMain.on('done', () => win.close())
app.whenReady().then(() => {
  win = new BrowserWindow({
    width: 800,
    height: 600,
    show: false,
    webPreferences: { preload: path.join(__dirname, 'preload.js') }
  })
  win.loadFile('index.html')
})
app.on('window-all-closed', () => app.quit())
