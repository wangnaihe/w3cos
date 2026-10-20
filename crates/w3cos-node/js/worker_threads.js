'use strict';
// `worker_threads` over host threads (`__w3cos_node_host` workerSpawn/...):
// each Worker is an OS thread with its own realm running a module the
// compiler linked into the program (name it statically:
// `new Worker(path.join(__dirname, 'worker.js'))`). Messages are
// structured-cloned (`node:internal/w3cos_clone`); MessageChannel ports
// live in one thread. Starting a worker asks the host permission policy
// (`ERR_ACCESS_DENIED`, permission 'WorkerThreads').

const EventEmitter = require('events');
const path = require('path');
const poll = require('node:internal/w3cos_poll');
const clone = require('node:internal/w3cos_clone');
const boot = require('node:internal/w3cos_worker');
const { setImmediate } = require('timers');
const process = require('process');

const host = __w3cos_node_host;
const context = boot.context;
const isMainThread = context === null;
const threadId = isMainThread ? 0 : context.threadId;
const workerData = isMainThread ? null : clone.deserialize(context.data);
const SHARE_ENV = Symbol.for('nodejs.worker_threads.SHARE_ENV');
const environmentData = new Map();

function unsupported(message) {
  const error = new Error(message);
  error.code = 'ERR_W3COS_UNSUPPORTED';
  return error;
}

function checkTransfer(transferList) {
  if (transferList === undefined || transferList === null) return;
  const list = Array.isArray(transferList) ? transferList : transferList.transfer;
  if (!Array.isArray(list)) return;
  for (const item of list) {
    if (item instanceof MessagePort) throw unsupported('Transferring a MessagePort is not supported by w3cos yet');
  }
}

// One end of a MessageChannel. Messages wait until the port starts (a
// 'message' listener or start()), then arrive one per macrotask. A started
// port keeps the thread alive until it is closed or unref()'d.
class MessagePort extends EventEmitter {
  constructor() {
    super();
    this._other = null;
    this._queue = [];
    this._started = false;
    this._closed = false;
    this._scheduled = false;
    this._pollRef = true;
    this.on('newListener', (event) => {
      if (event === 'message') this.start();
    });
  }

  postMessage(value, transferList) {
    checkTransfer(transferList);
    const data = clone.serialize(value);
    const other = this._other;
    if (this._closed || other === null || other._closed) return;
    other._queue.push(data);
    other._schedule();
  }

  _schedule() {
    if (!this._started || this._scheduled || this._queue.length === 0) return;
    this._scheduled = true;
    setImmediate(() => {
      this._scheduled = false;
      if (!this._started || this._closed || this._queue.length === 0) return;
      const data = this._queue.shift();
      this.emit('message', clone.deserialize(data));
      this._schedule();
    });
  }

  _poll() { return false; }

  start() {
    if (this._started || this._closed) return;
    this._started = true;
    poll.add(this);
    this._schedule();
  }

  close() {
    if (this._closed) return;
    this._closed = true;
    poll.remove(this);
    setImmediate(() => this.emit('close'));
    const other = this._other;
    if (other !== null && !other._closed) other.close();
  }

  ref() { this._pollRef = true; poll.wake(); return this; }
  unref() { this._pollRef = false; return this; }
  hasRef() { return this._started && !this._closed && this._pollRef !== false; }
}

class MessageChannel {
  constructor() {
    this.port1 = new MessagePort();
    this.port2 = new MessagePort();
    this.port1._other = this.port2;
    this.port2._other = this.port1;
  }
}

function receiveMessageOnPort(port) {
  if (!(port instanceof MessagePort)) {
    const error = new TypeError('The "port" argument must be a MessagePort instance');
    error.code = 'ERR_INVALID_ARG_TYPE';
    throw error;
  }
  if (port._queue.length === 0) return undefined;
  return { message: clone.deserialize(port._queue.shift()) };
}

// `parentPort` inside a worker: messages come from the host queue.
class ParentPort extends MessagePort {
  postMessage(value, transferList) {
    checkTransfer(transferList);
    if (this._closed) return;
    host.workerParentPost(clone.serialize(value));
  }
  _schedule() {}
  _poll() {
    if (this._closed) return false;
    const messages = host.workerParentPoll();
    for (const data of messages) this.emit('message', clone.deserialize(data));
    return messages.length > 0;
  }
  close() {
    if (this._closed) return;
    this._closed = true;
    poll.remove(this);
    setImmediate(() => this.emit('close'));
  }
}

const parentPort = isMainThread ? null : new ParentPort();

function decodeError(text) {
  const info = JSON.parse(text);
  if (info.message === undefined && 'value' in info) return info.value;
  const kinds = { TypeError, RangeError, SyntaxError, ReferenceError, EvalError, URIError };
  const Ctor = kinds[info.name] || Error;
  const error = new Ctor(info.message === null ? '' : info.message);
  if (info.name && !kinds[info.name]) error.name = info.name;
  if (info.stack !== null && info.stack !== undefined) error.stack = info.stack;
  if (info.code !== null && info.code !== undefined) error.code = info.code;
  return error;
}

function toFilename(filename) {
  if (filename !== null && typeof filename === 'object' && typeof filename.href === 'string') {
    return require('url').fileURLToPath(filename);
  }
  return String(filename);
}

class Worker extends EventEmitter {
  constructor(filename, options) {
    super();
    const opts = options || {};
    if (opts.eval) throw unsupported('new Worker(code, { eval: true }) needs a JS engine; w3cos compiles workers ahead of time');
    const file = toFilename(filename);
    if (!path.isAbsolute(file) && !file.startsWith('./') && !file.startsWith('../')) {
      const error = new TypeError('The worker script or module filename must be an absolute path or a relative path starting with \'./\' or \'../\'. Received "' + file + '"');
      error.code = 'ERR_WORKER_PATH';
      throw error;
    }
    const resolved = path.resolve(file);
    if (!boot.isCompiled(resolved)) {
      throw unsupported("Worker module '" + resolved + "' was not compiled into this w3cos binary; name it statically, e.g. new Worker(path.join(__dirname, 'worker.js'))");
    }
    checkTransfer(opts.transferList);
    const spawned = host.workerSpawn(resolved, clone.serialize(opts.workerData),
      opts.name === undefined ? null : String(opts.name), opts.w3cosKind === undefined ? null : String(opts.w3cosKind), null);
    this._id = spawned.id;
    this.threadId = spawned.threadId;
    this._pollRef = true;
    this._exited = false;
    this._terminating = [];
    this.resourceLimits = {};
    this.stdin = null;
    this.stdout = null;
    this.stderr = null;
    poll.add(this);
  }

  postMessage(value, transferList) {
    checkTransfer(transferList);
    const data = clone.serialize(value);
    if (this._exited) return;
    host.workerPost(this._id, data);
    poll.wake();
  }

  _poll() {
    const state = host.workerPoll(this._id);
    if (!state) return false;
    let progressed = false;
    if (state.online) {
      progressed = true;
      this.emit('online');
    }
    for (const data of state.messages) {
      progressed = true;
      this.emit('message', clone.deserialize(data));
    }
    for (const text of state.errors) {
      progressed = true;
      this.emit('error', decodeError(text));
    }
    if (state.exited) {
      progressed = true;
      this._exited = true;
      poll.remove(this);
      host.workerRelease(this._id);
      this.threadId = -1;
      this.emit('exit', state.code);
      const waiting = this._terminating;
      this._terminating = [];
      for (const resolve of waiting) resolve(state.code);
    }
    return progressed;
  }

  terminate(callback) {
    const done = new Promise((resolve) => {
      if (this._exited) {
        resolve(undefined);
        return;
      }
      this._terminating.push(resolve);
      host.workerTerminate(this._id);
      poll.wake();
    });
    if (typeof callback === 'function') done.then((code) => callback(null, code));
    return done;
  }

  ref() { this._pollRef = true; poll.wake(); return this; }
  unref() { this._pollRef = false; return this; }
  getHeapSnapshot() { return Promise.reject(unsupported('getHeapSnapshot is not available in w3cos')); }
}

function setEnvironmentData(key, value) {
  if (value === undefined) environmentData.delete(key);
  else environmentData.set(key, value);
}

function getEnvironmentData(key) {
  return environmentData.get(key);
}

function markAsUntransferable() {}
function isMarkedAsUntransferable() { return false; }
function moveMessagePortToContext() { throw unsupported('vm contexts are not available in w3cos'); }

module.exports = {
  isMainThread,
  threadId,
  workerData,
  parentPort,
  resourceLimits: {},
  SHARE_ENV,
  Worker,
  MessageChannel,
  MessagePort,
  BroadcastChannel: typeof globalThis.BroadcastChannel === 'function' ? globalThis.BroadcastChannel : undefined,
  receiveMessageOnPort,
  setEnvironmentData,
  getEnvironmentData,
  markAsUntransferable,
  isMarkedAsUntransferable,
  moveMessagePortToContext,
};
