'use strict';
// `node-pty` (M3 item 20). The native addon opens a pseudo-terminal and runs
// a program on it; w3cos does the same through the host's `ptySpawn`
// (posix_openpt + a session of its own, see `w3cos-core` node_host_io), and
// reads the master like a child's stdout through the shared poll loop. The
// package's `spawn` / `IPty` surface is what Code-OSS's pty host uses
// (terminalProcess.ts): onData, onExit, write, resize, clear, kill, pause,
// resume, pid, process, cols, rows, handleFlowControl.

const { Buffer } = require('buffer');
const { StringDecoder } = require('string_decoder');
const path = require('path');
const os = require('os');
const poll = require('node:internal/w3cos_poll');
const { setImmediate } = require('timers');

const host = __w3cos_node_host;

const SIGNALS = {
  SIGHUP: 1, SIGINT: 2, SIGQUIT: 3, SIGKILL: 9, SIGUSR1: 10, SIGUSR2: 12,
  SIGPIPE: 13, SIGALRM: 14, SIGTERM: 15, SIGCONT: 18, SIGSTOP: 19,
};

function signalNumber(signal) {
  if (signal === undefined || signal === null) return 1; // node-pty kills with SIGHUP
  if (typeof signal === 'number') return signal;
  const number = SIGNALS[String(signal)];
  if (number === undefined) throw new Error('Unknown signal: ' + signal);
  return number;
}

class Event {
  constructor() {
    this._listeners = [];
  }

  get event() {
    return (listener) => {
      this._listeners.push(listener);
      return {
        dispose: () => {
          const index = this._listeners.indexOf(listener);
          if (index >= 0) this._listeners.splice(index, 1);
        },
      };
    };
  }

  fire(value) {
    for (const listener of this._listeners.slice()) listener(value);
  }
}

class Terminal {
  constructor(file, args, options) {
    options = options || {};
    if (typeof file !== 'string' || file.length === 0) {
      throw new Error('file must be a non-empty string');
    }
    const argv = typeof args === 'string' ? [args] : Array.isArray(args) ? args.map(String) : [];
    this._file = file;
    this._cols = options.cols > 0 ? Math.floor(options.cols) : 80;
    this._rows = options.rows > 0 ? Math.floor(options.rows) : 24;
    this._encoding = options.encoding === undefined ? 'utf8' : options.encoding;
    this._decoder = this._encoding === null ? null : new StringDecoder(this._encoding);
    this._data = new Event();
    this._exit = new Event();
    this._paused = false;
    this._held = [];
    this._exited = false;
    this._closed = false;
    this._outDone = false;
    this._pollRef = true;
    this.handleFlowControl = !!options.handleFlowControl;
    this._flowPause = options.flowControlPause === undefined ? '\x13' : options.flowControlPause;
    this._flowResume = options.flowControlResume === undefined ? '\x11' : options.flowControlResume;
    const env = Object.assign({}, options.env || process.env);
    if (options.name) env.TERM = options.name;
    else if (env.TERM === undefined) env.TERM = 'xterm';
    env.COLORTERM = env.COLORTERM || 'truecolor';
    const pairs = [];
    for (const key of Object.keys(env)) {
      if (env[key] !== undefined) pairs.push(key + '=' + env[key]);
    }
    const spawned = host.ptySpawn(file, argv, options.cwd === undefined ? null : String(options.cwd),
      pairs, this._cols, this._rows);
    this._id = spawned.id;
    this.pid = spawned.pid;
    poll.add(this);
  }

  get cols() { return this._cols; }
  get rows() { return this._rows; }
  get process() { return path.basename(this._file); }
  get onData() { return this._data.event; }
  get onExit() { return this._exit.event; }

  on(event, listener) {
    if (event === 'data') this._data.event(listener);
    else if (event === 'exit') this._exit.event((e) => listener(e.exitCode, e.signal));
    return this;
  }

  _deliver(chunk) {
    const text = this._decoder === null ? Buffer.from(chunk) : this._decoder.write(Buffer.from(chunk));
    if (text.length > 0) this._data.fire(text);
  }

  _poll() {
    if (this._closed) return false;
    const state = host.cpPoll(this._id);
    if (state === undefined) return false;
    let progressed = false;
    if (!this._paused) {
      for (const chunk of this._held.splice(0)) {
        this._deliver(chunk);
        progressed = true;
      }
    }
    for (const chunk of state.stdout) {
      if (this._paused) this._held.push(chunk);
      else this._deliver(chunk);
      progressed = true;
    }
    if (state.stdoutDone && !this._outDone) {
      this._outDone = true;
      progressed = true;
    }
    if (state.exited && this._outDone && !this._exited && this._held.length === 0) {
      this._exited = true;
      this._closed = true;
      poll.remove(this);
      host.cpRelease(this._id);
      const signal = state.signal === null ? undefined : state.signal;
      // After the output queued before the exit, like node-pty's close.
      setImmediate(() => this._exit.fire({ exitCode: state.code === null ? 0 : state.code, signal }));
      progressed = true;
    }
    return progressed;
  }

  write(data) {
    if (this._closed) throw new Error('Cannot write to a closed terminal');
    host.cpWrite(this._id, typeof data === 'string' ? Buffer.from(data, 'utf8') : data);
    poll.wake();
  }

  resize(cols, rows) {
    if (!(cols > 0) || !(rows > 0) || !isFinite(cols) || !isFinite(rows)) {
      throw new Error('resizing must be done using positive cols and rows');
    }
    if (this._closed) throw new Error('Cannot resize a pty that has already exited');
    this._cols = Math.floor(cols);
    this._rows = Math.floor(rows);
    host.ptyResize(this._id, this._cols, this._rows);
  }

  clear() {}

  kill(signal) {
    if (this._closed) throw new Error('Cannot kill a closed terminal');
    host.cpKill(this._id, signalNumber(signal));
    poll.wake();
  }

  pause() { this._paused = true; }

  resume() {
    this._paused = false;
    poll.wake();
  }
}

function spawn(file, args, options) {
  return new Terminal(file, args, options);
}

module.exports = {
  spawn,
  fork: spawn,
  createTerminal: spawn,
  native: undefined,
};
