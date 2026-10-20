const { app, BrowserWindow, ipcMain } = require('electron')
const path = require('node:path')

let win

ipcMain.handle('ping', (event, n) => `pong:${n}`)
ipcMain.on('report', (event, report) => {
  console.log('report', JSON.stringify(report))
  event.reply('ack', report.step)
})
ipcMain.on('done', (event, summary) => {
  console.log('done', summary, event.sender === win.webContents)
  win.close()
})

app.on('browser-window-created', (_event, created) => {
  console.log('browser-window-created', created.getSize().join('x'), created === win)
})

app.whenReady().then(() => {
  console.log('ready', app.isReady(), app.getName(), app.getVersion())
  win = new BrowserWindow({
    width: 640,
    height: 480,
    show: false,
    webPreferences: { preload: path.join(__dirname, 'preload.js') }
  })
  console.log('windows', BrowserWindow.getAllWindows().length, win.id === BrowserWindow.getAllWindows()[0].id)
  win.webContents.on('did-finish-load', () => {
    console.log('did-finish-load', win.getTitle())
    win.webContents.send('greeting', { from: 'main', n: 1 })
  })
  win.on('closed', () => console.log('closed', BrowserWindow.getAllWindows().length))
  win.loadFile('index.html')
})

app.on('window-all-closed', () => {
  console.log('window-all-closed')
  app.quit()
})
app.on('will-quit', () => console.log('will-quit'))
