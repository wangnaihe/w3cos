'use strict';
// node:fs — synchronous calls go straight to native host functions
// (`__w3cos_node_host`, std::fs underneath); callback and promise APIs are
// layered on top and complete asynchronously.
const host = __w3cos_node_host;
const { Buffer } = require('buffer');

const constants = {
  F_OK: 0, R_OK: 4, W_OK: 2, X_OK: 1,
  O_RDONLY: 0, O_WRONLY: 1, O_RDWR: 2, O_CREAT: 64, O_EXCL: 128, O_TRUNC: 512, O_APPEND: 1024,
  O_NOCTTY: 256, O_DIRECTORY: 65536, O_NOATIME: 262144, O_NOFOLLOW: 131072, O_SYNC: 1052672,
  O_DSYNC: 4096, O_DIRECT: 16384, O_NONBLOCK: 2048,
  S_IFMT: 61440, S_IFREG: 32768, S_IFDIR: 16384, S_IFCHR: 8192, S_IFBLK: 24576,
  S_IFIFO: 4096, S_IFLNK: 40960, S_IFSOCK: 49152,
  COPYFILE_EXCL: 1, COPYFILE_FICLONE: 2, COPYFILE_FICLONE_FORCE: 4,
  UV_FS_COPYFILE_EXCL: 1,
};

function invalidArgType(name, expected, actual) {
  const err = new TypeError('The "' + name + '" argument must be ' + expected + '. Received ' +
    (actual === null ? 'null' : typeof actual === 'object' ? 'an instance of ' + ((actual.constructor && actual.constructor.name) || 'Object') : 'type ' + typeof actual + ' (' + String(actual) + ')'));
  err.code = 'ERR_INVALID_ARG_TYPE';
  return err;
}

function toPath(path, name) {
  if (typeof path === 'string') return path;
  if (Buffer.isBuffer(path)) return path.toString();
  if (path && typeof path === 'object' && path.protocol === 'file:' && typeof path.href === 'string') {
    return require('url').fileURLToPath(path);
  }
  throw invalidArgType(name || 'path', 'of type string or an instance of Buffer or URL', path);
}

function getOptions(options, defaults) {
  if (options === undefined || options === null || typeof options === 'function') return Object.assign({}, defaults);
  if (typeof options === 'string') return Object.assign({}, defaults, { encoding: options });
  return Object.assign({}, defaults, options);
}

function encode(buffer, encoding) {
  return encoding && encoding !== 'buffer' ? buffer.toString(encoding) : buffer;
}

// ── Stats / Dirent ──

function dateOf(ms) { return new Date(Math.round(ms)); }

function Stats(raw) {
  this.dev = raw.dev; this.mode = raw.mode; this.nlink = raw.nlink; this.uid = raw.uid;
  this.gid = raw.gid; this.rdev = raw.rdev; this.blksize = raw.blksize; this.ino = raw.ino;
  this.size = raw.size; this.blocks = raw.blocks;
  this.atimeMs = raw.atimeMs; this.mtimeMs = raw.mtimeMs; this.ctimeMs = raw.ctimeMs; this.birthtimeMs = raw.birthtimeMs;
  this.atime = dateOf(raw.atimeMs); this.mtime = dateOf(raw.mtimeMs);
  this.ctime = dateOf(raw.ctimeMs); this.birthtime = dateOf(raw.birthtimeMs);
}
function modeIs(type) { return function () { return (this.mode & constants.S_IFMT) === type; }; }
Stats.prototype.isFile = modeIs(constants.S_IFREG);
Stats.prototype.isDirectory = modeIs(constants.S_IFDIR);
Stats.prototype.isSymbolicLink = modeIs(constants.S_IFLNK);
Stats.prototype.isBlockDevice = modeIs(constants.S_IFBLK);
Stats.prototype.isCharacterDevice = modeIs(constants.S_IFCHR);
Stats.prototype.isFIFO = modeIs(constants.S_IFIFO);
Stats.prototype.isSocket = modeIs(constants.S_IFSOCK);

function Dirent(name, kind, dir) {
  this.name = name;
  this.parentPath = dir;
  this.path = dir;
  Object.defineProperty(this, '_kind', { value: kind, enumerable: false, writable: true, configurable: true });
}
Dirent.prototype.isFile = function () { return this._kind === 1; };
Dirent.prototype.isDirectory = function () { return this._kind === 2; };
Dirent.prototype.isSymbolicLink = function () { return this._kind === 3; };
Dirent.prototype.isBlockDevice = function () { return false; };
Dirent.prototype.isCharacterDevice = function () { return false; };
Dirent.prototype.isFIFO = function () { return false; };
Dirent.prototype.isSocket = function () { return false; };

// ── synchronous API ──

function readFileSync(path, options) {
  options = getOptions(options, { flag: 'r' });
  let buffer;
  if (typeof path === 'number') {
    const chunks = [];
    for (;;) {
      const chunk = Buffer.from(host.read(path, 65536, null));
      if (chunk.length === 0) break;
      chunks.push(chunk);
    }
    buffer = Buffer.concat(chunks);
  } else {
    const p = toPath(path);
    if (options.encoding === 'utf8' || options.encoding === 'utf-8') return host.readFileUtf8(p);
    buffer = Buffer.from(host.readFile(p));
  }
  return encode(buffer, options.encoding);
}

function dataFor(data, encoding) {
  if (typeof data === 'string') return encoding && encoding !== 'utf8' && encoding !== 'utf-8' ? Buffer.from(data, encoding) : data;
  if (data instanceof Uint8Array || ArrayBuffer.isView(data)) return data;
  throw invalidArgType('data', 'of type string or an instance of Buffer, TypedArray, or DataView', data);
}

function writeFileSync(path, data, options) {
  options = getOptions(options, { encoding: 'utf8', flag: 'w', mode: 0o666 });
  const bytes = dataFor(data, options.encoding);
  if (typeof path === 'number') { host.write(path, bytes, null); return; }
  host.writeFile(toPath(path), bytes, options.flag, options.mode);
}

function appendFileSync(path, data, options) {
  options = getOptions(options, { encoding: 'utf8', flag: 'a', mode: 0o666 });
  if (!options.flag || options.flag === 'w') options.flag = 'a';
  writeFileSync(path, data, options);
}

function existsSync(path) {
  try { return host.stat(toPath(path), true, true) !== undefined; } catch (e) { return false; }
}

function statSync(path, options) {
  const raw = host.stat(toPath(path), true, !!(options && options.throwIfNoEntry === false));
  return raw === undefined ? undefined : new Stats(raw);
}
function lstatSync(path, options) {
  const raw = host.stat(toPath(path), false, !!(options && options.throwIfNoEntry === false));
  return raw === undefined ? undefined : new Stats(raw);
}
function fstatSync(fd) { return new Stats(host.fstat(fd)); }

function readdirSync(path, options) {
  options = getOptions(options, {});
  const dir = toPath(path);
  const out = [];
  const walk = function (abs, rel) {
    const entries = host.readdir(abs);
    for (let i = 0; i < entries.length; i++) {
      const name = entries[i][0];
      const kind = entries[i][1];
      const relName = rel ? rel + '/' + name : name;
      if (options.withFileTypes) out.push(new Dirent(name, kind, abs));
      else out.push(options.encoding === 'buffer' ? Buffer.from(relName) : relName);
      if (options.recursive && kind === 2) walk(abs + '/' + name, relName);
    }
  };
  walk(dir, '');
  return out;
}

function mkdirSync(path, options) {
  if (typeof options === 'number' || typeof options === 'string') options = { mode: options };
  options = options || {};
  return host.mkdir(toPath(path), !!options.recursive, options.mode);
}
function rmdirSync(path, options) { host.rmdir(toPath(path), !!(options && options.recursive)); }
function rmSync(path, options) {
  options = options || {};
  host.rm(toPath(path), !!options.recursive, !!options.force);
}
function unlinkSync(path) { host.unlink(toPath(path)); }
function renameSync(from, to) { host.rename(toPath(from, 'oldPath'), toPath(to, 'newPath')); }
function copyFileSync(src, dest, mode) {
  host.copyFile(toPath(src, 'src'), toPath(dest, 'dest'), !!((mode || 0) & constants.COPYFILE_EXCL));
}
function cpSync(src, dest, options) {
  host.cp(toPath(src, 'src'), toPath(dest, 'dest'), !!(options && options.recursive));
}
function realpathSync(path, options) {
  const real = host.realpath(toPath(path));
  return options && (options === 'buffer' || options.encoding === 'buffer') ? Buffer.from(real) : real;
}
realpathSync.native = realpathSync;
function readlinkSync(path) { return host.readlink(toPath(path)); }
function symlinkSync(target, path) { host.symlink(toPath(target, 'target'), toPath(path)); }
function linkSync(existing, path) { host.link(toPath(existing, 'existingPath'), toPath(path, 'newPath')); }
function chmodSync(path, mode) { host.chmod(toPath(path), mode); }
function toSeconds(time) {
  if (time instanceof Date) return time.getTime() / 1000;
  if (typeof time === 'string') return Number(time);
  return time;
}
function utimesSync(path, atime, mtime) { host.utimes(toPath(path), toSeconds(atime), toSeconds(mtime)); }
function accessSync(path, mode) { host.access(toPath(path), mode === undefined ? 0 : mode); }
function truncateSync(path, len) {
  if (typeof path === 'number') { host.ftruncate(path, len || 0); return; }
  host.truncate(toPath(path), len || 0);
}
function mkdtempSync(prefix) { return host.mkdtemp(String(prefix)); }
function openSync(path, flags, mode) {
  return host.open(toPath(path), flags === undefined ? 'r' : flags, mode);
}
function closeSync(fd) { host.close(fd); }
function readSync(fd, buffer, offsetOrOptions, length, position) {
  let offset = offsetOrOptions;
  if (offsetOrOptions && typeof offsetOrOptions === 'object') {
    offset = offsetOrOptions.offset; length = offsetOrOptions.length; position = offsetOrOptions.position;
  }
  offset = offset || 0;
  if (length === undefined) length = buffer.byteLength - offset;
  if (position === undefined) position = null;
  const bytes = new Uint8Array(host.read(fd, length, typeof position === 'bigint' ? Number(position) : position));
  const target = new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength);
  for (let i = 0; i < bytes.length; i++) target[offset + i] = bytes[i];
  return bytes.length;
}
function writeSync(fd, buffer, offsetOrPosition, lengthOrEncoding, position) {
  if (typeof buffer === 'string') {
    const position2 = offsetOrPosition === undefined ? null : offsetOrPosition;
    return host.write(fd, dataFor(buffer, lengthOrEncoding), position2);
  }
  const offset = offsetOrPosition || 0;
  const length = lengthOrEncoding === undefined ? buffer.byteLength - offset : lengthOrEncoding;
  const view = new Uint8Array(buffer.buffer, buffer.byteOffset + offset, length);
  return host.write(fd, view, position === undefined ? null : position);
}
function fsyncSync(fd) { host.fsync(fd); }
function fdatasyncSync(fd) { host.fdatasync(fd); }
function ftruncateSync(fd, len) { host.ftruncate(fd, len || 0); }

// ── callback API ──

const later = require('timers').setImmediate;

function callbackify(sync, name) {
  return function () {
    const args = Array.prototype.slice.call(arguments);
    const cb = args.pop();
    if (typeof cb !== 'function') {
      throw invalidArgType('cb', 'of type function', cb);
    }
    let result;
    let error = null;
    try { result = sync.apply(null, args); } catch (e) { error = e; }
    later(function () { if (error) cb(error); else cb(null, result); });
  };
}

const fs = module.exports;
fs.constants = constants;
Object.assign(fs, constants);
fs.Stats = Stats;
fs.Dirent = Dirent;

const SYNC = {
  readFile: readFileSync, writeFile: writeFileSync, appendFile: appendFileSync,
  stat: statSync, lstat: lstatSync, fstat: fstatSync, readdir: readdirSync, mkdir: mkdirSync,
  rmdir: rmdirSync, rm: rmSync, unlink: unlinkSync, rename: renameSync, copyFile: copyFileSync,
  cp: cpSync, realpath: realpathSync, readlink: readlinkSync, symlink: symlinkSync, link: linkSync,
  chmod: chmodSync, utimes: utimesSync, access: accessSync, truncate: truncateSync,
  mkdtemp: mkdtempSync, open: openSync, close: closeSync, fsync: fsyncSync, fdatasync: fdatasyncSync, ftruncate: ftruncateSync,
};
Object.keys(SYNC).forEach(function (name) {
  fs[name + 'Sync'] = SYNC[name];
  fs[name] = callbackify(SYNC[name], name);
});
fs.realpath.native = fs.realpath;
fs.existsSync = existsSync;
fs.readSync = readSync;
fs.writeSync = writeSync;
fs.lchmodSync = chmodSync;
fs.exists = function exists(path, cb) {
  const result = existsSync(path);
  later(function () { cb(result); });
};
fs.read = function read(fd, buffer, offset, length, position, cb) {
  if (typeof offset === 'function') { cb = offset; offset = 0; length = buffer.byteLength; position = null; }
  let n; let error = null;
  try { n = readSync(fd, buffer, offset, length, position); } catch (e) { error = e; }
  later(function () { if (error) cb(error); else cb(null, n, buffer); });
};
fs.write = function write(fd, buffer) {
  const args = Array.prototype.slice.call(arguments, 1);
  const cb = args.pop();
  let n; let error = null;
  try { n = writeSync.apply(null, [fd].concat(args)); } catch (e) { error = e; }
  later(function () { if (error) cb(error); else cb(null, n, buffer); });
};
fs.watch = function watch() {
  const EE = require('events');
  const watcher = new EE();
  watcher.close = function () {};
  watcher.ref = watcher.unref = function () { return watcher; };
  return watcher;
};
fs.watchFile = function watchFile() {};
fs.unwatchFile = function unwatchFile() {};

// ── streams ──

function ReadStream(path, options) {
  if (!(this instanceof ReadStream)) return new ReadStream(path, options);
  const { Readable } = require('stream');
  options = getOptions(options, { highWaterMark: 64 * 1024 });
  Readable.call(this, options);
  this.path = typeof path === 'number' ? undefined : toPath(path);
  this.fd = options.fd !== undefined ? options.fd : typeof path === 'number' ? path : null;
  this.flags = options.flags || 'r';
  this.start = options.start;
  this.end = options.end === undefined ? Infinity : options.end;
  this.pos = this.start;
  this.bytesRead = 0;
  this.autoClose = options.autoClose !== false;
  if (options.encoding) this.setEncoding(options.encoding);
  const self = this;
  if (this.fd === null) {
    try {
      this.fd = host.open(this.path, this.flags, options.mode);
      process.nextTick(function () { self.emit('open', self.fd); self.emit('ready'); });
    } catch (e) {
      process.nextTick(function () { self.destroy(e); });
    }
  }
}
function WriteStream(path, options) {
  if (!(this instanceof WriteStream)) return new WriteStream(path, options);
  const { Writable } = require('stream');
  options = getOptions(options, {});
  Writable.call(this, options);
  this.path = typeof path === 'number' ? undefined : toPath(path);
  this.fd = options.fd !== undefined ? options.fd : typeof path === 'number' ? path : null;
  this.flags = options.flags || 'w';
  this.pos = options.start;
  this.bytesWritten = 0;
  this.autoClose = options.autoClose !== false;
  const self = this;
  if (this.fd === null) {
    try {
      this.fd = host.open(this.path, this.flags, options.mode);
      process.nextTick(function () { self.emit('open', self.fd); self.emit('ready'); });
    } catch (e) {
      process.nextTick(function () { self.destroy(e); });
    }
  }
}

let streamsLinked = false;
function linkStreams() {
  if (streamsLinked) return;
  streamsLinked = true;
  const { Readable, Writable } = require('stream');
  Object.setPrototypeOf(ReadStream.prototype, Readable.prototype);
  Object.setPrototypeOf(WriteStream.prototype, Writable.prototype);
  ReadStream.prototype._read = function (n) {
    if (this.fd === null || this.destroyed) return;
    let want = n;
    if (this.end !== Infinity) {
      const pos = this.pos === undefined ? this.bytesRead + (this.start || 0) : this.pos;
      want = Math.min(n, this.end - pos + 1);
    }
    if (want <= 0) { this.push(null); return; }
    let chunk;
    try {
      chunk = Buffer.from(host.read(this.fd, want, this.pos === undefined ? null : this.pos));
    } catch (e) { this.destroy(e); return; }
    if (this.pos !== undefined) this.pos += chunk.length;
    this.bytesRead += chunk.length;
    if (chunk.length === 0) this.push(null);
    else this.push(chunk);
  };
  ReadStream.prototype._destroy = function (err, cb) {
    if (this.fd !== null && this.autoClose) {
      try { host.close(this.fd); } catch (e) { /* already closed */ }
      this.fd = null;
    }
    cb(err);
  };
  ReadStream.prototype.close = function (cb) {
    if (typeof cb === 'function') require('stream').finished(this, cb);
    this.destroy();
  };
  WriteStream.prototype._write = function (chunk, encoding, cb) {
    try {
      const n = host.write(this.fd, chunk, this.pos === undefined ? null : this.pos);
      if (this.pos !== undefined) this.pos += n;
      this.bytesWritten += n;
    } catch (e) { cb(e); return; }
    cb();
  };
  WriteStream.prototype._destroy = ReadStream.prototype._destroy;
  WriteStream.prototype.close = function (cb) {
    if (cb) this.once('close', cb);
    this.end();
  };
  WriteStream.prototype.destroySoon = WriteStream.prototype.end;
}

fs.ReadStream = ReadStream;
fs.WriteStream = WriteStream;
fs.createReadStream = function createReadStream(path, options) { linkStreams(); return new ReadStream(path, options); };
fs.createWriteStream = function createWriteStream(path, options) { linkStreams(); return new WriteStream(path, options); };

// ── promises ──

function promisify(sync) {
  return function () {
    const args = arguments;
    return new Promise(function (resolve, reject) {
      later(function () {
        try { resolve(sync.apply(null, args)); } catch (e) { reject(e); }
      });
    });
  };
}

function FileHandle(fd) { this.fd = fd; }
FileHandle.prototype.close = function () { const fd = this.fd; this.fd = -1; return promisify(closeSync)(fd); };
FileHandle.prototype.stat = function () { return promisify(fstatSync)(this.fd); };
FileHandle.prototype.readFile = function (options) { return promisify(readFileSync)(this.fd, options); };
FileHandle.prototype.writeFile = function (data, options) { return promisify(writeFileSync)(this.fd, data, options); };
FileHandle.prototype.appendFile = FileHandle.prototype.writeFile;
FileHandle.prototype.truncate = function (len) { return promisify(ftruncateSync)(this.fd, len); };
FileHandle.prototype.sync = function () { return promisify(fsyncSync)(this.fd); };
FileHandle.prototype.datasync = function () { return promisify(fdatasyncSync)(this.fd); };
FileHandle.prototype.read = function (buffer, offset, length, position) {
  const fd = this.fd;
  if (buffer === undefined || (buffer && !(buffer instanceof Uint8Array) && typeof buffer === 'object')) {
    const o = buffer || {};
    buffer = o.buffer || Buffer.alloc(16384); offset = o.offset; length = o.length; position = o.position;
  }
  return promisify(function () {
    return { bytesRead: readSync(fd, buffer, offset, length, position), buffer: buffer };
  })();
};
FileHandle.prototype.write = function (buffer, a, b, c) {
  const fd = this.fd;
  return promisify(function () {
    return { bytesWritten: writeSync(fd, buffer, a, b, c), buffer: buffer };
  })();
};

const promises = {};
Object.keys(SYNC).forEach(function (name) {
  if (name === 'open' || name === 'close' || name === 'fstat' || name === 'fsync' || name === 'fdatasync' || name === 'ftruncate') return;
  promises[name] = promisify(SYNC[name]);
});
promises.open = function open(path, flags, mode) {
  return promisify(openSync)(path, flags, mode).then(function (fd) { return new FileHandle(fd); });
};
promises.constants = constants;
promises.FileHandle = FileHandle;
fs.promises = promises;
