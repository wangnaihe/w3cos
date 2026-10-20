'use strict';
// `net` over the host's non-blocking sockets (`__w3cos_node_host`
// netListen/netConnect/netRead/..., std::net and Unix domain sockets
// underneath). Opening a listener or a connection asks the host permission
// policy first (`ERR_ACCESS_DENIED`). The shared poll loop accepts
// connections and moves bytes without blocking the event loop.

const EventEmitter = require('events');
const { Duplex } = require('stream');
const { Buffer } = require('buffer');
const process = require('process');
const poll = require('node:internal/w3cos_poll');
const { setImmediate } = require('timers');

const host = __w3cos_node_host;

function isIPv4(input) {
  const parts = String(input).split('.');
  return parts.length === 4 && parts.every((part) => /^(25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)$/.test(part));
}

function isIPv6(input) {
  const text = String(input);
  if (!/^[0-9a-fA-F:.]+$/.test(text) || text.indexOf(':') < 0) return false;
  const doubles = text.split('::').length - 1;
  if (doubles > 1) return false;
  let groups = text.split(':').filter((group) => group !== '');
  let max = 8;
  const last = groups[groups.length - 1];
  if (last !== undefined && last.indexOf('.') >= 0) {
    if (!isIPv4(last)) return false;
    groups = groups.slice(0, -1);
    max = 6;
  }
  if (!groups.every((group) => /^[0-9a-fA-F]{1,4}$/.test(group))) return false;
  return doubles === 1 ? groups.length < max : groups.length === max;
}

function isIP(input) {
  if (isIPv4(input)) return 4;
  if (isIPv6(input)) return 6;
  return 0;
}

// Node's message shape for socket errors: `connect ECONNREFUSED 127.0.0.1:80`,
// `listen EADDRINUSE: address already in use 127.0.0.1:80`.
function socketError(error, syscall, address, port) {
  if (!error || typeof error.errno !== 'number') return error;
  const where = address === undefined ? '' : ' ' + address + (port === undefined ? '' : ':' + port);
  const description = syscall === 'listen' && error.code === 'EADDRINUSE' ? ': address already in use' : '';
  error.message = syscall + ' ' + error.code + description + where;
  error.syscall = syscall;
  if (address !== undefined) error.address = address;
  if (port !== undefined) error.port = port;
  return error;
}

function toPort(value) {
  const port = Number(value);
  if (!Number.isInteger(port) || port < 0 || port > 65535) {
    const error = new RangeError('options.port should be >= 0 and < 65536. Received ' + value + '.');
    error.code = 'ERR_SOCKET_BAD_PORT';
    throw error;
  }
  return port;
}

// (path[, cb]) | (port[, host][, cb]) | (options[, cb])
function connectArgs(args) {
  let options = {};
  let callback;
  if (args.length > 0 && typeof args[args.length - 1] === 'function') callback = args[args.length - 1];
  const first = args[0];
  if (first !== null && typeof first === 'object') options = Object.assign({}, first);
  else if (typeof first === 'string' && !/^\d+$/.test(first)) options.path = first;
  else {
    options.port = first;
    if (typeof args[1] === 'string') options.host = args[1];
  }
  return { options, callback };
}

class Socket extends Duplex {
  constructor(options) {
    const opts = options || {};
    // 'close' carries `hadError` here, so the stream's own one is off.
    super({ allowHalfOpen: opts.allowHalfOpen === true, emitClose: false, autoDestroy: true });
    this._id = null;
    this._server = null;
    this._pollRef = true;
    this._eof = false;
    this._waiting = [];
    this._timeout = 0;
    this._timer = null;
    this.connecting = false;
    this.pending = true;
    this.bytesRead = 0;
    this._bytesDispatched = 0;
    this._addresses = null;
  }

  get bytesWritten() { return this._bytesDispatched; }
  get readyState() {
    if (this.connecting) return 'opening';
    if (this.readable && this.writable) return 'open';
    if (this.readable) return 'readOnly';
    if (this.writable) return 'writeOnly';
    return 'closed';
  }
  _addr() {
    if (this._addresses === null && this._id !== null) this._addresses = host.netSocketAddresses(this._id);
    return this._addresses || {};
  }
  get remoteAddress() { const a = this._addr().remote; return a ? a.address : undefined; }
  get remotePort() { const a = this._addr().remote; return a ? a.port : undefined; }
  get remoteFamily() { const a = this._addr().remote; return a ? a.family : undefined; }
  get localAddress() { const a = this._addr().local; return a ? a.address : undefined; }
  get localPort() { const a = this._addr().local; return a ? a.port : undefined; }
  address() {
    const local = this._addr().local;
    return local ? { address: local.address, family: local.family, port: local.port } : {};
  }

  connect(...args) {
    const { options, callback } = connectArgs(args);
    if (callback) this.once('connect', callback);
    const path = options.path;
    const hostName = options.host || 'localhost';
    const port = path === undefined ? toPort(options.port) : undefined;
    this.connecting = true;
    let id;
    try {
      // Permission refusals throw here, synchronously, as in Node.
      id = host.netConnect(path === undefined ? null : String(path), port === undefined ? 0 : port, hostName);
    } catch (error) {
      if (error && error.code === 'ERR_ACCESS_DENIED') throw error;
      const failure = path === undefined
        ? socketError(error, 'connect', error && error.address !== undefined ? error.address : hostName, port)
        : socketError(error, 'connect', String(path));
      setImmediate(() => {
        this.connecting = false;
        this.destroy(failure);
      });
      return this;
    }
    setImmediate(() => {
      if (this.destroyed) {
        host.netClose(id);
        return;
      }
      this._attach(id);
      this.connecting = false;
      this.emit('connect');
      this.emit('ready');
      this._flushWaiting();
    });
    return this;
  }

  _attach(id) {
    this._id = id;
    this.pending = false;
    poll.add(this);
  }

  _poll() {
    if (this._id === null) return false;
    let progressed = false;
    try {
      for (let i = 0; i < 16 && !this._eof; i++) {
        const chunk = host.netRead(this._id);
        if (chunk === undefined) break;
        progressed = true;
        if (chunk === null) {
          this._eof = true;
          this.push(null);
          break;
        }
        this.bytesRead += chunk.byteLength;
        this._touch();
        this.push(Buffer.from(chunk));
      }
      if (this._waiting.length > 0 && host.netPending(this._id) === 0) {
        const callbacks = this._waiting;
        this._waiting = [];
        for (const callback of callbacks) callback();
        progressed = true;
      }
    } catch (error) {
      this.destroy(socketError(error, error.syscall || 'read'));
      return true;
    }
    if (this._eof && this._waiting.length === 0) poll.remove(this);
    return progressed;
  }

  _read() {}

  _write(chunk, encoding, callback) {
    if (this._id === null) {
      if (!this.connecting) {
        callback(Object.assign(new Error('This socket is closed'), { code: 'ERR_SOCKET_CLOSED' }));
        return;
      }
      this._waiting.push(() => this._write(chunk, encoding, callback));
      return;
    }
    let queued;
    try {
      queued = host.netWrite(this._id, chunk);
    } catch (error) {
      callback(socketError(error, 'write'));
      return;
    }
    this._bytesDispatched += chunk.length;
    this._touch();
    if (queued === 0) callback();
    else {
      this._waiting.push(callback);
      poll.add(this);
      poll.wake();
    }
  }

  _flushWaiting() {
    const waiting = this._waiting;
    this._waiting = [];
    for (const run of waiting) run();
  }

  _final(callback) {
    if (this._id === null) {
      if (this.connecting) this._waiting.push(() => this._final(callback));
      else callback();
      return;
    }
    host.netEnd(this._id);
    callback();
  }

  _destroy(error, callback) {
    if (this._timer) clearTimeout(this._timer);
    if (this._id !== null) {
      poll.remove(this);
      host.netClose(this._id);
    }
    callback(error);
    process.nextTick(() => this.emit('close', !!error));
  }

  _touch() {
    if (this._timeout > 0) this._arm();
  }

  _arm() {
    if (this._timer) clearTimeout(this._timer);
    this._timer = setTimeout(() => { this._timer = null; this.emit('timeout'); }, this._timeout);
  }

  setTimeout(ms, callback) {
    this._timeout = ms;
    if (callback) this.once('timeout', callback);
    if (ms > 0) this._arm();
    else if (this._timer) { clearTimeout(this._timer); this._timer = null; }
    return this;
  }

  setNoDelay() { return this; }
  setKeepAlive() { return this; }
  ref() { this._pollRef = true; poll.wake(); return this; }
  unref() { this._pollRef = false; return this; }
  resetAndDestroy() { return this.destroy(); }
}

class Server extends EventEmitter {
  constructor(options, connectionListener) {
    super();
    if (typeof options === 'function') { connectionListener = options; options = {}; }
    this._options = options || {};
    this._id = null;
    this._connections = 0;
    this._pollRef = true;
    this._closing = false;
    this.maxConnections = undefined;
    if (connectionListener) this.on('connection', connectionListener);
  }

  get listening() { return this._id !== null; }

  listen(...args) {
    let callback;
    if (args.length > 0 && typeof args[args.length - 1] === 'function') callback = args.pop();
    let options = {};
    const first = args[0];
    if (first !== null && typeof first === 'object') options = Object.assign({}, first);
    else if (typeof first === 'string' && !/^\d+$/.test(first)) options.path = first;
    else {
      options.port = first === undefined ? 0 : first;
      if (typeof args[1] === 'string') options.host = args[1];
    }
    if (callback) this.once('listening', callback);
    const path = options.path;
    const port = path === undefined ? toPort(options.port === undefined ? 0 : options.port) : undefined;
    const hostName = options.host;
    try {
      this._id = host.netListen(path === undefined ? null : String(path), port === undefined ? 0 : port,
        hostName === undefined ? null : String(hostName));
    } catch (error) {
      if (error && error.code === 'ERR_ACCESS_DENIED') throw error;
      const failure = path === undefined
        ? socketError(error, 'listen', hostName || '0.0.0.0', port)
        : socketError(error, 'listen', String(path));
      process.nextTick(() => this.emit('error', failure));
      return this;
    }
    poll.add(this);
    process.nextTick(() => this.emit('listening'));
    return this;
  }

  _poll() {
    if (this._id === null) return false;
    let progressed = false;
    for (let i = 0; i < 16; i++) {
      const id = host.netAccept(this._id);
      if (id === undefined) break;
      progressed = true;
      const socket = new Socket({ allowHalfOpen: this._options.allowHalfOpen === true });
      socket._server = this;
      socket._attach(id);
      this._connections++;
      socket.once('close', () => {
        this._connections--;
        this._maybeClosed();
      });
      if (this._options.pauseOnConnect) socket.pause();
      this.emit('connection', socket);
    }
    return progressed;
  }

  address() {
    if (this._id === null) return null;
    return host.netAddress(this._id);
  }

  close(callback) {
    if (this._id === null) {
      if (callback) {
        const error = new Error('Server is not running.');
        error.code = 'ERR_SERVER_NOT_RUNNING';
        process.nextTick(() => callback(error));
      }
      return this;
    }
    if (callback) this.once('close', callback);
    poll.remove(this);
    host.netClose(this._id);
    this._id = null;
    this._closing = true;
    this._maybeClosed();
    return this;
  }

  _maybeClosed() {
    if (this._closing && this._connections === 0) {
      this._closing = false;
      process.nextTick(() => this.emit('close'));
    }
  }

  getConnections(callback) {
    const count = this._connections;
    process.nextTick(() => callback(null, count));
    return this;
  }

  ref() { this._pollRef = true; poll.wake(); return this; }
  unref() { this._pollRef = false; return this; }
}

function createServer(options, connectionListener) {
  return new Server(options, connectionListener);
}

function connect(...args) {
  const { options } = connectArgs(args);
  const socket = new Socket(options);
  if (options.timeout) socket.setTimeout(options.timeout);
  return socket.connect(...args);
}

module.exports = {
  Server,
  Socket,
  Stream: Socket,
  createServer,
  connect,
  createConnection: connect,
  isIP,
  isIPv4,
  isIPv6,
};
