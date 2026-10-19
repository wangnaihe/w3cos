const steps = []
window.bridge.onAck((step) => {
  steps.push('ack:' + step)
  if (steps.length === 2) window.bridge.done(steps.join(','))
})
window.bridge.onGreeting(async (message) => {
  const pong = await window.bridge.ping(message.n)
  window.bridge.report({
    step: 'ping',
    pong,
    from: message.from,
    title: document.title,
    h1: document.querySelector('h1').textContent
  })
  window.bridge.report({
    step: 'bridge',
    keys: Object.keys(window.bridge).sort().join(','),
    platform: typeof window.bridge.platform,
    process: typeof window.process,
    require: typeof window.require
  })
})
