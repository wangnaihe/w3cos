const { app, utilityProcess, MessageChannelMain } = require('electron')
const path = require('node:path')

app.whenReady().then(() => {
  const { port1, port2 } = new MessageChannelMain()
  port2.on('message', (event) => {
    console.log('channel', JSON.stringify(event.data), event.ports.length)
    startChild()
  })
  port2.start()
  port1.postMessage({ hello: 'port', list: [1, 2] })

  function startChild () {
    const child = utilityProcess.fork(path.join(__dirname, 'child.js'), ['--mode', 'sum'], { serviceName: 'adder' })
    console.log('forked', typeof child.pid, child.stdout, child.stderr)
    child.on('spawn', () => console.log('spawn', typeof child.pid, child.pid > 0))
    child.on('message', (message) => {
      console.log('message', JSON.stringify(message))
      if (message.step === 'ready') child.postMessage({ cmd: 'add', a: 2, b: 3 })
      else if (message.step === 'sum') console.log('kill', child.kill())
    })
    child.on('exit', (code) => {
      console.log('exit', typeof code, child.pid)
      app.quit()
    })
  }
})

app.on('will-quit', () => console.log('will-quit'))
