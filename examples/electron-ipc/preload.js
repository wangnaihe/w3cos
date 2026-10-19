const { contextBridge, ipcRenderer } = require('electron')

contextBridge.exposeInMainWorld('bridge', {
  ping: (n) => ipcRenderer.invoke('ping', n),
  report: (report) => ipcRenderer.send('report', report),
  done: (summary) => ipcRenderer.send('done', summary),
  onGreeting: (callback) => ipcRenderer.on('greeting', (_event, message) => callback(message)),
  onAck: (callback) => ipcRenderer.on('ack', (_event, step) => callback(step)),
  platform: process.platform
})
