'use strict';
// Worker boot: which worker module this thread runs (null on the main
// thread), and the worker files the compiler linked into the program entry.
// The entry's CommonJS record calls `compiled()` and `entry()` first (see
// the compiler's `worker_boot_prelude`).

const host = __w3cos_node_host;

const context = host.workerContext();
let compiledPaths = null;

exports.context = context;

exports.compiled = function compiled(paths) {
  if (compiledPaths === null) compiledPaths = new Set();
  for (const path of paths) compiledPaths.add(path);
};

exports.isCompiled = function isCompiled(path) {
  // CommonJS workers are listed by the entry record; ES-module workers
  // (package.json "w3cos": { "workers": [...] }) by the generated entry.
  return (compiledPaths !== null && compiledPaths.has(path)) || host.workerModuleCompiled(path) === true;
};

// Electron `utilityProcess` children see `process.type === 'utility'`,
// their fork arguments in argv, and `process.parentPort` (message events
// carry `{ data, ports }`).
function setUpUtilityProcess() {
  const proc = require('process');
  const EventEmitter = require('events');
  const threads = require('worker_threads');
  const data = threads.workerData || {};
  proc.type = 'utility';
  proc.argv = [proc.execPath, context.path].concat(Array.isArray(data.args) ? data.args : []);
  const { setImmediate } = require('timers');
  const port = new EventEmitter();
  port.postMessage = function postMessage(message) {
    threads.parentPort.postMessage(message);
  };
  // Ports the main process transferred (`child.postMessage(msg, [port])`):
  // MessagePortMain-shaped ends whose traffic is multiplexed over the
  // parent channel as `{ __w3cosPort, id, data }` envelopes.
  const transferred = new Map();
  class TransferredPort extends EventEmitter {
    constructor(id) {
      super();
      this._id = id;
      this._queue = [];
      this._started = false;
      this._closed = false;
      this._scheduled = false;
      transferred.set(id, this);
    }
    postMessage(data) {
      if (this._closed) return;
      threads.parentPort.postMessage({ __w3cosPort: 'message', id: this._id, data });
    }
    start() {
      if (this._started || this._closed) return;
      this._started = true;
      this._schedule();
    }
    close() {
      if (this._closed) return;
      this._closed = true;
      transferred.delete(this._id);
      threads.parentPort.postMessage({ __w3cosPort: 'close', id: this._id });
      setImmediate(() => this.emit('close'));
    }
    _deliver(data) {
      this._queue.push(data);
      this._schedule();
    }
    _remoteClosed() {
      if (this._closed) return;
      this._closed = true;
      transferred.delete(this._id);
      setImmediate(() => this.emit('close'));
    }
    _schedule() {
      if (!this._started || this._scheduled || this._queue.length === 0) return;
      this._scheduled = true;
      setImmediate(() => {
        this._scheduled = false;
        if (this._closed || this._queue.length === 0) return;
        this.emit('message', { data: this._queue.shift(), ports: [] });
        this._schedule();
      });
    }
  }
  threads.parentPort.on('message', (message) => {
    const envelope = message !== null && typeof message === 'object' ? message.__w3cosPort : undefined;
    if (envelope === undefined) {
      port.emit('message', { data: message, ports: [] });
    } else if (envelope === 'transfer') {
      port.emit('message', { data: message.message, ports: message.ids.map((id) => new TransferredPort(id)) });
    } else {
      const target = transferred.get(message.id);
      if (target === undefined) return;
      if (envelope === 'message') target._deliver(message.data);
      else if (envelope === 'close') target._remoteClosed();
    }
  });
  proc.parentPort = port;
}

exports.entry = function entry() {
  if (context === null) return undefined;
  // The environment the parent gave this worker (`new Worker(file, { env })`,
  // `utilityProcess.fork(path, args, { env })`), before the entry module reads it.
  if (typeof context.env === 'string' && context.env !== '') {
    require('process').env = JSON.parse(context.env);
  }
  if (context.kind === 'utility') setUpUtilityProcess();
  return context.path;
};

exports.missing = function missing(path) {
  const error = new Error("Cannot find module '" + path + "'");
  error.code = 'MODULE_NOT_FOUND';
  throw error;
};
