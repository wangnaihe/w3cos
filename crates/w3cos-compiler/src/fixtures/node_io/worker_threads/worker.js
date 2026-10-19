'use strict';
const { parentPort, workerData, isMainThread, threadId } = require('worker_threads');

if (workerData.mode === 'busy') {
  setInterval(() => {}, 5);
  parentPort.postMessage({ type: 'busy' });
} else {
  parentPort.postMessage({ type: 'ready', isMainThread, threadId, data: [workerData.n, workerData.list, workerData.when.getTime(), workerData.m.get('a')] });
  parentPort.on('message', (msg) => {
    if (msg.cmd === 'square') parentPort.postMessage({ type: 'square', value: msg.n * msg.n });
    else if (msg.cmd === 'clone') {
      msg.value.self = msg.value;
      parentPort.postMessage({ type: 'clone', value: msg.value });
    } else if (msg.cmd === 'exit') process.exit(msg.code);
    else if (msg.cmd === 'throw') throw new TypeError('boom from worker');
  });
}
