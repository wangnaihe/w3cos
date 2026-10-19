const { contextBridge, ipcRenderer } = require('electron')
contextBridge.exposeInMainWorld('probe', {
  line: (text) => ipcRenderer.send('line', text),
  done: () => ipcRenderer.send('done')
})
