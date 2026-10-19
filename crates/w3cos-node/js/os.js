'use strict';
// `os`: static facts of a W3C OS host plus host-provided values when the
// runtime exposes them on `process`.

const proc = require('process');

function env(name) {
  const value = proc.env[name];
  return typeof value === 'string' && value.length > 0 ? value : undefined;
}

const constants = {
  signals: {
    SIGHUP: 1, SIGINT: 2, SIGQUIT: 3, SIGILL: 4, SIGTRAP: 5, SIGABRT: 6, SIGBUS: 7, SIGFPE: 8,
    SIGKILL: 9, SIGUSR1: 10, SIGSEGV: 11, SIGUSR2: 12, SIGPIPE: 13, SIGALRM: 14, SIGTERM: 15,
    SIGCHLD: 17, SIGCONT: 18, SIGSTOP: 19, SIGTSTP: 20, SIGTTIN: 21, SIGTTOU: 22, SIGWINCH: 28,
  },
  errno: {
    E2BIG: 7, EACCES: 13, EADDRINUSE: 98, EAGAIN: 11, EBADF: 9, EBUSY: 16, ECONNREFUSED: 111,
    ECONNRESET: 104, EEXIST: 17, EINTR: 4, EINVAL: 22, EIO: 5, EISDIR: 21, EMFILE: 24,
    ENOENT: 2, ENOSPC: 28, ENOTDIR: 20, ENOTEMPTY: 39, EPERM: 1, EPIPE: 32, ETIMEDOUT: 110,
  },
  priority: {
    PRIORITY_LOW: 19, PRIORITY_BELOW_NORMAL: 10, PRIORITY_NORMAL: 0,
    PRIORITY_ABOVE_NORMAL: -7, PRIORITY_HIGH: -14, PRIORITY_HIGHEST: -20,
  },
  UV_UDP_REUSEADDR: 4,
};

module.exports = {
  EOL: '\n',
  devNull: '/dev/null',
  constants: constants,
  platform: function platform() { return 'linux'; },
  type: function type() { return 'Linux'; },
  release: function release() { return env('W3COS_KERNEL_RELEASE') || '6.0.0-w3cos'; },
  version: function version() { return '#1 SMP W3C OS'; },
  machine: function machine() { return proc.arch === 'arm64' ? 'aarch64' : 'x86_64'; },
  arch: function arch() { return proc.arch; },
  endianness: function endianness() { return 'LE'; },
  hostname: function hostname() { return env('HOSTNAME') || 'w3cos'; },
  homedir: function homedir() { return env('HOME') || '/home/user'; },
  tmpdir: function tmpdir() {
    let dir = env('TMPDIR') || env('TMP') || env('TEMP') || '/tmp';
    if (dir.length > 1 && dir.charAt(dir.length - 1) === '/') dir = dir.slice(0, -1);
    return dir;
  },
  userInfo: function userInfo() {
    const username = env('USER') || env('LOGNAME') || 'user';
    return { uid: proc.getuid(), gid: proc.getgid(), username: username, homedir: module.exports.homedir(), shell: env('SHELL') || null };
  },
  cpus: function cpus() { return []; },
  availableParallelism: function availableParallelism() { return 1; },
  loadavg: function loadavg() { return [0, 0, 0]; },
  uptime: function uptime() { return Math.floor(proc.uptime()); },
  freemem: function freemem() { return 0; },
  totalmem: function totalmem() { return 0; },
  networkInterfaces: function networkInterfaces() { return {}; },
  getPriority: function getPriority() { return 0; },
  setPriority: function setPriority() {},
};
