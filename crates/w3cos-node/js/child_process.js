'use strict';
// `child_process` over the host's process layer (`__w3cos_node_host`
// cpSpawn/cpPoll/..., std::process underneath). Every spawn asks the host
// permission policy first (`ERR_ACCESS_DENIED`, as Node's permission model),
// so a shell or agent host can gate and record process launches.
// Pipes are read and written on host threads; the shared poll loop delivers
// output and exit to the JS side without blocking the event loop.

const EventEmitter = require('events');
const { Readable, Writable } = require('stream');
const { Buffer } = require('buffer');
const os = require('os');
const util = require('util');
const process = require('process');
const poll = require('node:internal/w3cos_poll');
const { setImmediate } = require('timers');

const host = __w3cos_node_host;
const signals = os.constants.signals;

function invalidArgType(name, expected, actual) {
  const error = new TypeError('The "' + name + '" argument must be ' + expected +
    '. Received ' + (actual === null ? 'null' : typeof actual));
  error.code = 'ERR_INVALID_ARG_TYPE';
  return error;
}

function signalNumber(signal) {
  if (signal === undefined || signal === null) return signals.SIGTERM;
  if (typeof signal === 'number') return signal;
  const number = signals[String(signal).toUpperCase()];
  if (number === undefined) {
    const error = new TypeError('Unknown signal: ' + signal);
    error.code = 'ERR_UNKNOWN_SIGNAL';
    throw error;
  }
  return number;
}

function signalName(number) {
  if (number === null || number === undefined) return null;
  for (const name of Object.keys(signals)) if (signals[name] === number) return name;
  return 'SIG' + number;
}

function envPairs(env) {
  const source = env || process.env || {};
  const pairs = [];
  for (const key of Object.keys(source)) {
    if (source[key] !== undefined) pairs.push(key + '=' + source[key]);
  }
  return pairs;
}

function stdioModes(stdio) {
  const list = stdio === undefined || stdio === null
    ? ['pipe', 'pipe', 'pipe']
    : typeof stdio === 'string'
      ? [stdio, stdio, stdio]
      : Array.from(stdio);
  const modes = [];
  for (let i = 0; i < 3; i++) {
    const entry = list[i];
    if (entry === undefined || entry === null || entry === 'pipe' || entry === 'overlapped') modes.push('pipe');
    else if (entry === 'ignore') modes.push('ignore');
    else if (entry === 'inherit' || typeof entry === 'number' || typeof entry === 'object') modes.push('inherit');
    else if (entry === 'ipc') {
      const error = new Error("stdio 'ipc' (fork channels) is not supported by this w3cos runtime yet");
      error.code = 'ERR_W3COS_UNSUPPORTED';
      throw error;
    } else {
      const error = new TypeError("The argument 'stdio' is invalid. Received " + String(entry));
      error.code = 'ERR_INVALID_ARG_VALUE';
      throw error;
    }
  }
  return modes;
}

// Node's argument shapes: (file), (file, args), (file, options), (file, args, options).
function normalize(file, args, options) {
  if (typeof file !== 'string') throw invalidArgType('file', 'of type string', file);
  if (file.length === 0) {
    const error = new Error("The argument 'file' cannot be empty. Received ''");
    error.code = 'ERR_INVALID_ARG_VALUE';
    throw error;
  }
  if (!Array.isArray(args)) {
    if (args !== undefined && args !== null && typeof args !== 'object') {
      throw invalidArgType('args', 'an instance of Array', args);
    }
    options = args;
    args = [];
  }
  options = Object.assign({}, options || {});
  args = args.map(String);
  let spawnfile = file;
  let spawnargs = [file].concat(args);
  if (options.shell) {
    const command = [file].concat(args).join(' ');
    const shell = typeof options.shell === 'string' ? options.shell : '/bin/sh';
    spawnfile = shell;
    spawnargs = [shell, '-c', command];
  }
  return { file: spawnfile, args: spawnargs.slice(1), spawnargs, options };
}

function spawnFailure(error, syscall, file, spawnargs) {
  if (error && error.code !== 'ERR_ACCESS_DENIED' && typeof error.errno === 'number') {
    error.syscall = syscall + ' ' + file;
    error.path = file;
    error.spawnargs = spawnargs.slice(1);
    error.message = syscall + ' ' + file + ' ' + error.code;
  }
  return error;
}

class ChildProcess extends EventEmitter {
  constructor() {
    super();
    this.pid = undefined;
    this.stdin = null;
    this.stdout = null;
    this.stderr = null;
    this.stdio = [null, null, null];
    this.exitCode = null;
    this.signalCode = null;
    this.killed = false;
    this.connected = false;
    this.spawnfile = undefined;
    this.spawnargs = [];
    this._id = null;
    this._exited = false;
    this._closed = false;
    this._outDone = false;
    this._errDone = false;
    this._pollRef = true;
  }

  spawn(options) {
    const { file, args, spawnargs } = options;
    this.spawnfile = file;
    this.spawnargs = spawnargs;
    const modes = stdioModes(options.stdio);
    if (modes[0] === 'pipe') {
      const child = this;
      this.stdin = new Writable({
        write(chunk, encoding, callback) {
          if (child._id !== null) host.cpWrite(child._id, chunk);
          callback();
        },
        final(callback) {
          if (child._id !== null) host.cpEndStdin(child._id);
          callback();
        },
      });
    }
    if (modes[1] === 'pipe') this.stdout = new Readable({ read() {} });
    if (modes[2] === 'pipe') this.stderr = new Readable({ read() {} });
    this.stdio = [this.stdin, this.stdout, this.stderr];
    this._outDone = this.stdout === null;
    this._errDone = this.stderr === null;
    let spawned;
    try {
      spawned = host.cpSpawn(file, args, options.cwd === undefined ? null : String(options.cwd),
        envPairs(options.env), modes);
    } catch (error) {
      if (error && error.code === 'ERR_ACCESS_DENIED') throw error;
      const failure = spawnFailure(error, 'spawn', file, spawnargs);
      for (const stream of this.stdio) if (stream) stream.destroy();
      process.nextTick(() => {
        this.emit('error', failure);
        this._closed = true;
        this.emit('close', failure.errno, null);
      });
      return failure.errno;
    }
    this._id = spawned.id;
    this.pid = spawned.pid;
    poll.add(this);
    process.nextTick(() => this.emit('spawn'));
    return 0;
  }

  _poll() {
    const state = host.cpPoll(this._id);
    if (state === undefined) return false;
    let progressed = false;
    for (const chunk of state.stdout) {
      if (this.stdout) this.stdout.push(Buffer.from(chunk));
      progressed = true;
    }
    for (const chunk of state.stderr) {
      if (this.stderr) this.stderr.push(Buffer.from(chunk));
      progressed = true;
    }
    if (state.stdoutDone && !this._outDone) {
      this._outDone = true;
      if (this.stdout) this.stdout.push(null);
      progressed = true;
    }
    if (state.stderrDone && !this._errDone) {
      this._errDone = true;
      if (this.stderr) this.stderr.push(null);
      progressed = true;
    }
    if (state.exited && !this._exited) {
      this._exited = true;
      this.exitCode = state.code;
      this.signalCode = signalName(state.signal);
      if (this.stdin) this.stdin.destroy();
      this.emit('exit', this.exitCode, this.signalCode);
      progressed = true;
    }
    if (this._exited && this._outDone && this._errDone && !this._closed) {
      this._closed = true;
      poll.remove(this);
      host.cpRelease(this._id);
      // After the streams' queued 'data' / 'end' events, as in Node.
      setImmediate(() => this.emit('close', this.exitCode, this.signalCode));
      progressed = true;
    }
    return progressed;
  }

  kill(signal) {
    const number = signalNumber(signal);
    if (this._id === null || this._exited) return false;
    const sent = host.cpKill(this._id, number);
    if (sent) this.killed = true;
    poll.wake();
    return sent;
  }

  ref() { this._pollRef = true; poll.wake(); }
  unref() { this._pollRef = false; }
  disconnect() {
    const error = new Error('IPC channel is already disconnected');
    error.code = 'ERR_IPC_DISCONNECTED';
    throw error;
  }
}

function spawn(file, args, options) {
  const normalized = normalize(file, args, options);
  const child = new ChildProcess();
  child.spawn(Object.assign({}, normalized.options, normalized));
  if (normalized.options.timeout > 0) {
    const timer = setTimeout(() => child.kill(normalized.options.killSignal), normalized.options.timeout);
    child.once('exit', () => clearTimeout(timer));
  }
  return child;
}

function decode(chunks, encoding) {
  const buffer = Buffer.concat(chunks);
  return encoding && encoding !== 'buffer' && Buffer.isEncoding(encoding) ? buffer.toString(encoding) : buffer;
}

function execFile(file, args, options, callback) {
  if (typeof args === 'function') { callback = args; args = []; options = {}; }
  else if (typeof options === 'function') {
    callback = options;
    if (!Array.isArray(args)) { options = args; args = []; } else options = {};
  }
  if (!Array.isArray(args)) { options = args; args = []; }
  const opts = Object.assign({ encoding: 'utf8', timeout: 0, maxBuffer: 1024 * 1024, killSignal: 'SIGTERM' }, options || {});
  const cmd = opts._command !== undefined ? opts._command : [file].concat(args).join(' ');
  const child = spawn(file, args, {
    cwd: opts.cwd, env: opts.env, shell: opts.shell, stdio: ['pipe', 'pipe', 'pipe'],
  });
  const out = [];
  const err = [];
  let outLength = 0;
  let errLength = 0;
  let killed = false;
  let failure = null;
  let timer = null;
  let done = false;
  const kill = () => { killed = true; child.kill(opts.killSignal); };
  if (opts.timeout > 0) timer = setTimeout(kill, opts.timeout);
  const finish = (code, signal) => {
    if (done) return;
    done = true;
    if (timer) clearTimeout(timer);
    const stdout = decode(out, opts.encoding);
    const stderr = decode(err, opts.encoding);
    if (!callback) return;
    if (failure) {
      failure.cmd = cmd;
      callback(failure, stdout, stderr);
      return;
    }
    if (code === 0 && signal === null) {
      callback(null, stdout, stderr);
      return;
    }
    const error = new Error('Command failed: ' + cmd + '\n' + stderr);
    error.code = code;
    error.killed = child.killed || killed;
    error.signal = signal;
    error.cmd = cmd;
    callback(error, stdout, stderr);
  };
  const collect = (list, which) => (chunk) => {
    if (which === 'out') outLength += chunk.length; else errLength += chunk.length;
    list.push(chunk);
    if (opts.maxBuffer && (outLength > opts.maxBuffer || errLength > opts.maxBuffer) && !failure) {
      failure = new RangeError((which === 'out' ? 'stdout' : 'stderr') + ' maxBuffer length exceeded');
      failure.code = 'ERR_CHILD_PROCESS_STDIO_MAXBUFFER';
      kill();
    }
  };
  if (child.stdout) child.stdout.on('data', collect(out, 'out'));
  if (child.stderr) child.stderr.on('data', collect(err, 'err'));
  child.on('error', (error) => { failure = error; finish(null, null); });
  child.on('close', finish);
  return child;
}

function exec(command, options, callback) {
  if (typeof options === 'function') { callback = options; options = {}; }
  const opts = Object.assign({}, options || {});
  const shell = typeof opts.shell === 'string' ? opts.shell : '/bin/sh';
  opts.shell = false;
  opts._command = command;
  return execFile(shell, ['-c', String(command)], opts, callback);
}

const customPromisify = util.promisify && util.promisify.custom;
function promisified(fn) {
  return function (...args) {
    let child;
    const promise = new Promise((resolve, reject) => {
      child = fn(...args, (error, stdout, stderr) => {
        if (error) {
          error.stdout = stdout;
          error.stderr = stderr;
          reject(error);
        } else resolve({ stdout, stderr });
      });
    });
    promise.child = child;
    return promise;
  };
}
if (customPromisify) {
  Object.defineProperty(exec, customPromisify, { value: promisified(exec) });
  Object.defineProperty(execFile, customPromisify, { value: promisified(execFile) });
}

function spawnSync(file, args, options) {
  const normalized = normalize(file, args, options);
  const opts = normalized.options;
  const modes = stdioModes(opts.stdio);
  let result;
  try {
    result = host.cpSpawnSync(normalized.file, normalized.args,
      opts.cwd === undefined ? null : String(opts.cwd), envPairs(opts.env), modes,
      opts.input === undefined ? null : opts.input,
      opts.timeout > 0 ? opts.timeout : 0, signalNumber(opts.killSignal));
  } catch (error) {
    if (error && error.code === 'ERR_ACCESS_DENIED') throw error;
    return {
      error: spawnFailure(error, 'spawnSync', normalized.file, normalized.spawnargs),
      status: null, signal: null, output: null, pid: 0, stdout: null, stderr: null,
    };
  }
  const convert = (value) => {
    if (value === null) return null;
    const buffer = Buffer.from(value);
    return opts.encoding && opts.encoding !== 'buffer' ? buffer.toString(opts.encoding) : buffer;
  };
  const stdout = convert(result.stdout);
  const stderr = convert(result.stderr);
  const out = {
    status: result.status,
    signal: signalName(result.signal),
    output: [null, stdout, stderr],
    pid: result.pid,
    stdout,
    stderr,
  };
  if (result.timedOut) {
    const error = new Error('spawnSync ' + normalized.file + ' ETIMEDOUT');
    error.code = 'ETIMEDOUT';
    error.errno = -110;
    error.syscall = 'spawnSync ' + normalized.file;
    error.path = normalized.file;
    error.spawnargs = normalized.args;
    out.error = error;
  }
  return out;
}

function checkSync(result, cmd, options) {
  // execSync: the child's stderr goes to ours unless stdio was given.
  if (options.stdio === undefined && result.stderr && result.stderr.length > 0 && process.stderr) {
    process.stderr.write(result.stderr);
  }
  if (result.error) {
    result.error.status = result.status;
    throw result.error;
  }
  if (result.status !== 0) {
    let message = 'Command failed: ' + cmd;
    if (result.stderr && result.stderr.length > 0) message += '\n' + result.stderr.toString();
    const error = new Error(message);
    error.status = result.status;
    error.signal = result.signal;
    error.output = result.output;
    error.pid = result.pid;
    error.stdout = result.stdout;
    error.stderr = result.stderr;
    throw error;
  }
  return result.stdout;
}

function execSync(command, options) {
  const opts = Object.assign({}, options || {});
  const shell = typeof opts.shell === 'string' ? opts.shell : '/bin/sh';
  const result = spawnSync(shell, ['-c', String(command)], Object.assign({}, opts, { shell: false }));
  return checkSync(result, command, opts);
}

function execFileSync(file, args, options) {
  if (!Array.isArray(args)) { options = args; args = []; }
  const opts = Object.assign({}, options || {});
  const result = spawnSync(file, args, opts);
  return checkSync(result, [file].concat(args || []).join(' '), opts);
}

function fork() {
  // fork() starts a new AOT process for a separately compiled module; that
  // needs multi-entry builds (utilityProcess slice).
  const error = new Error('child_process.fork() is not supported by this w3cos runtime yet');
  error.code = 'ERR_W3COS_UNSUPPORTED';
  throw error;
}

module.exports = {
  ChildProcess,
  spawn,
  exec,
  execFile,
  spawnSync,
  execSync,
  execFileSync,
  fork,
};
