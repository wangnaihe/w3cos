'use strict';
// `process`: one shared object over the host `process` global (env, argv,
// cwd, exit come from the runtime), plus EventEmitter methods, nextTick,
// hrtime and the static facts of a W3C OS host.

const EventEmitter = require('events');

const host = typeof process === 'object' && process !== null ? process : {};
const hostCwd = host.cwd;
const hostChdir = host.chdir;
const hostExit = host.exit;
const hostAbort = host.abort;

// `require('process') === process`, as in Node: the host object itself gains
// the EventEmitter methods and the members below.
const proc = host;
for (const key of Object.getOwnPropertyNames(EventEmitter.prototype)) {
  if (key !== 'constructor' && proc[key] === undefined) proc[key] = EventEmitter.prototype[key];
}
EventEmitter.call(proc);

function hostValue(name, fallback) {
  const value = host[name];
  return value === undefined ? fallback : value;
}

proc.title = 'w3cos';
proc.env = hostValue('env', {});
proc.argv = hostValue('argv', ['w3cos']);
proc.execArgv = [];
proc.argv0 = proc.argv.length > 0 ? proc.argv[0] : 'w3cos';
proc.execPath = hostValue('execPath', '/usr/bin/w3cos');
proc.pid = hostValue('pid', 1);
proc.ppid = hostValue('ppid', 0);
proc.platform = hostValue('platform', 'linux');
proc.arch = hostValue('arch', 'x64');
proc.version = 'v20.0.0';
proc.versions = { node: '20.0.0', w3cos: '0.1.0' };
proc.release = { name: 'node' };
proc.exitCode = undefined;
proc.config = { variables: {} };
proc.features = {};

// Node's permission model API over the host policy (`child`, `worker`,
// `net`; file system access is not gated by this layer).
proc.permission = {
  has(scope, reference) {
    const name = String(scope);
    if (name === 'fs' || name.startsWith('fs.')) return true;
    const host = typeof __w3cos_node_host === 'object' ? __w3cos_node_host : null;
    if (host === null || typeof host.permissionHas !== 'function') return true;
    return host.permissionHas(name, reference === undefined ? null : String(reference));
  },
};

proc.cwd = function cwd() {
  if (typeof hostCwd === 'function') return hostCwd();
  return '/';
};
proc.chdir = function chdir(directory) {
  if (typeof hostChdir === 'function') return hostChdir(directory);
  const error = new Error('process.chdir() is not supported');
  error.code = 'ERR_W3COS_UNSUPPORTED';
  throw error;
};
proc.umask = function umask() { return 18; };
proc.getuid = function getuid() { return hostValue('uid', 1000); };
proc.getgid = function getgid() { return hostValue('gid', 1000); };
proc.geteuid = proc.getuid;
proc.getegid = proc.getgid;

proc.exit = function exit(code) {
  if (code !== undefined) proc.exitCode = code;
  proc.emit('exit', proc.exitCode === undefined ? 0 : proc.exitCode);
  if (typeof hostExit === 'function') return hostExit(proc.exitCode === undefined ? 0 : proc.exitCode);
};

proc.abort = function abort() {
  if (typeof hostAbort === 'function') return hostAbort();
  proc.exit(134);
};

proc.kill = function kill() {
  const error = new Error('process.kill() is not supported');
  error.code = 'ERR_W3COS_UNSUPPORTED';
  throw error;
};

// nextTick callbacks run before promise jobs queued after them; a dedicated
// queue drained from one microtask keeps their relative order.
const tickQueue = [];
let tickScheduled = false;
function runTicks() {
  tickScheduled = false;
  while (tickQueue.length > 0) {
    const entry = tickQueue.shift();
    entry.fn.apply(undefined, entry.args);
  }
}
proc.nextTick = function nextTick(fn) {
  if (typeof fn !== 'function') {
    const error = new TypeError('The "callback" argument must be of type function');
    error.code = 'ERR_INVALID_ARG_TYPE';
    throw error;
  }
  const args = [];
  for (let i = 1; i < arguments.length; i++) args.push(arguments[i]);
  tickQueue.push({ fn: fn, args: args });
  if (!tickScheduled) {
    tickScheduled = true;
    if (typeof queueMicrotask === 'function') queueMicrotask(runTicks);
    else Promise.resolve().then(runTicks);
  }
};

function nowMs() {
  if (typeof performance === 'object' && performance && typeof performance.now === 'function') {
    return (typeof performance.timeOrigin === 'number' ? performance.timeOrigin : 0) + performance.now();
  }
  return Date.now();
}

proc.hrtime = function hrtime(previous) {
  const ms = nowMs();
  let seconds = Math.floor(ms / 1000);
  let nanos = Math.floor((ms - seconds * 1000) * 1e6);
  if (previous) {
    seconds -= previous[0];
    nanos -= previous[1];
    if (nanos < 0) {
      seconds -= 1;
      nanos += 1e9;
    }
  }
  return [seconds, nanos];
};
proc.hrtime.bigint = function bigint() {
  return BigInt(Math.floor(nowMs() * 1e6));
};

proc.uptime = (function () {
  const start = nowMs();
  return function uptime() { return (nowMs() - start) / 1000; };
})();

proc.memoryUsage = function memoryUsage() {
  return { rss: 0, heapTotal: 0, heapUsed: 0, external: 0, arrayBuffers: 0 };
};
proc.memoryUsage.rss = function rss() { return 0; };
proc.cpuUsage = function cpuUsage() { return { user: 0, system: 0 }; };
proc.resourceUsage = function resourceUsage() { return {}; };

proc.emitWarning = function emitWarning(warning, type, code) {
  if (typeof type === 'object' && type !== null) {
    code = type.code;
    type = type.type;
  }
  const name = typeof type === 'string' ? type : 'Warning';
  const message = warning instanceof Error ? warning.message : String(warning);
  const object = warning instanceof Error ? warning : new Error(message);
  object.name = name;
  if (code !== undefined) object.code = code;
  if (proc.listenerCount('warning') > 0) {
    proc.nextTick(function () { proc.emit('warning', object); });
    return;
  }
  if (typeof console === 'object') {
    console.error('(node:' + proc.pid + ') ' + (code ? '[' + code + '] ' : '') + name + ': ' + message);
  }
};

proc.binding = function binding(name) {
  const error = new Error('No such module: ' + name);
  error.code = 'ERR_INVALID_MODULE';
  throw error;
};

proc.stdout = hostValue('stdout', {
  isTTY: false,
  write: function write(chunk) {
    console.log(String(chunk).replace(/\n$/, ''));
    return true;
  },
});
proc.stderr = hostValue('stderr', {
  isTTY: false,
  write: function write(chunk) {
    console.error(String(chunk).replace(/\n$/, ''));
    return true;
  },
});
proc.stdin = hostValue('stdin', { isTTY: false });

module.exports = proc;
