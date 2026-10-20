const port = process.parentPort
port.on('message', (event) => {
  const message = event.data
  if (message.cmd === 'add') port.postMessage({ step: 'sum', value: message.a + message.b, ports: event.ports.length })
})
port.postMessage({ step: 'ready', args: process.argv.slice(-2), type: process.type })
