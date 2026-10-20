'use strict';
const path = require('path');
const { Worker, isMainThread, threadId, parentPort, workerData, MessageChannel, receiveMessageOnPort } = require('worker_threads');

const workerFile = path.join(__dirname, 'worker.js');
const base = { n: 0, list: [], when: new Date(0), m: new Map() };

function t1(next) {
  const w = new Worker(workerFile, { workerData: { n: 6, list: [1, 2], when: new Date(1000), m: new Map([['a', 1]]) } });
  const events = [];
  w.on('online', () => events.push('online'));
  w.on('message', (msg) => {
    events.push(msg.type);
    if (msg.type === 'ready') {
      console.log('t1 ready', msg.isMainThread, typeof msg.threadId, msg.threadId > 0, JSON.stringify(msg.data));
      w.postMessage({ cmd: 'square', n: 7 });
    } else if (msg.type === 'square') {
      console.log('t1 square', msg.value);
      w.postMessage({ cmd: 'clone', value: { s: new Set([1, 'two']), big: 10n, u: undefined, nan: NaN, neg: -0, inf: -Infinity,
        buf: Buffer.from('hi'), f64: new Float64Array([1.5, -2]), err: new RangeError('bad'), re: /a+b/gi, nested: { deep: [[1], { x: null }] } } });
    } else if (msg.type === 'clone') {
      const v = msg.value;
      console.log('t1 clone', JSON.stringify([...v.s]), typeof v.big, v.big === 10n, 'u' in v, v.u, Number.isNaN(v.nan), Object.is(v.neg, -0), v.inf);
      console.log('t1 clone2', v.buf instanceof Uint8Array, Buffer.isBuffer(v.buf), String.fromCharCode(...v.buf), v.f64 instanceof Float64Array, Array.from(v.f64).join(','),
        v.err instanceof RangeError, v.err.message, v.re instanceof RegExp, v.re.source, v.re.flags, v.self === v, JSON.stringify(v.nested));
      w.postMessage({ cmd: 'exit', code: 3 });
    }
  });
  w.on('exit', (code) => {
    console.log('t1 exit', code, events.join(','), w.threadId);
    next();
  });
}

function t2(next) {
  const w = new Worker(workerFile, { workerData: base });
  w.on('message', (msg) => { if (msg.type === 'ready') w.postMessage({ cmd: 'throw' }); });
  w.on('error', (e) => console.log('t2 error', e instanceof TypeError, e.name, e.message));
  w.on('exit', (code) => { console.log('t2 exit', code); next(); });
}

function t3(next) {
  const w = new Worker(workerFile, { workerData: Object.assign({}, base, { mode: 'busy' }) });
  w.on('exit', (code) => console.log('t3 exit', code));
  w.once('message', (msg) => {
    console.log('t3 first', msg.type);
    w.terminate().then((code) => { console.log('t3 terminate', code); next(); });
  });
}

function t4(next) {
  const { port1, port2 } = new MessageChannel();
  port1.postMessage({ a: 1 });
  console.log('t4 receive', JSON.stringify(receiveMessageOnPort(port2)), receiveMessageOnPort(port2));
  port2.on('close', () => { console.log('t4 closed'); next(); });
  port2.on('message', (m) => { console.log('t4 message', JSON.stringify(m)); port2.close(); });
  port1.postMessage(['hello', new Map([[1, 2]]).size]);
}

function t5(next) {
  const w = new Worker(__filename, { workerData: 'self' });
  w.on('message', (msg) => console.log('t5 message', msg));
  w.on('exit', (code) => { console.log('t5 exit', code); next(); });
}

function t6(next) {
  try {
    new Worker('worker.js');
  } catch (e) {
    console.log('t6 path', e.code, e.name);
  }
  next();
}

if (isMainThread) {
  console.log('main', isMainThread, threadId, workerData, parentPort);
  t1(() => t2(() => t3(() => t4(() => t5(() => t6(() => console.log('done')))))));
} else {
  parentPort.postMessage('from self ' + workerData + ' ' + (threadId > 0));
}
