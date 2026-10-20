'use strict';
// node:zlib — deflate/inflate, raw and gzip over native host functions
// (`__w3cos_node_host.zlib`, flate2/miniz_oxide underneath). One-shot APIs
// run synchronously; the callback API completes asynchronously and the
// stream classes buffer their input and emit the result on end. Brotli is
// not provided.
const host = __w3cos_node_host;
const { Buffer } = require('buffer');

const codes = {
  Z_OK: 0, Z_STREAM_END: 1, Z_NEED_DICT: 2, Z_ERRNO: -1, Z_STREAM_ERROR: -2,
  Z_DATA_ERROR: -3, Z_MEM_ERROR: -4, Z_BUF_ERROR: -5, Z_VERSION_ERROR: -6,
};

const constants = {
  Z_NO_FLUSH: 0, Z_PARTIAL_FLUSH: 1, Z_SYNC_FLUSH: 2, Z_FULL_FLUSH: 3, Z_FINISH: 4,
  Z_BLOCK: 5, Z_OK: 0, Z_STREAM_END: 1, Z_NEED_DICT: 2, Z_ERRNO: -1,
  Z_STREAM_ERROR: -2, Z_DATA_ERROR: -3, Z_MEM_ERROR: -4, Z_BUF_ERROR: -5,
  Z_VERSION_ERROR: -6, Z_NO_COMPRESSION: 0, Z_BEST_SPEED: 1, Z_BEST_COMPRESSION: 9,
  Z_DEFAULT_COMPRESSION: -1, Z_FILTERED: 1, Z_HUFFMAN_ONLY: 2, Z_RLE: 3, Z_FIXED: 4,
  Z_DEFAULT_STRATEGY: 0, ZLIB_VERNUM: 4865, DEFLATE: 1, INFLATE: 2, GZIP: 3,
  GUNZIP: 4, DEFLATERAW: 5, INFLATERAW: 6, UNZIP: 7, BROTLI_DECODE: 8, BROTLI_ENCODE: 9,
  Z_MIN_WINDOWBITS: 8, Z_MAX_WINDOWBITS: 15, Z_DEFAULT_WINDOWBITS: 15,
  Z_MIN_CHUNK: 64, Z_MAX_CHUNK: Infinity, Z_DEFAULT_CHUNK: 16384,
  Z_MIN_MEMLEVEL: 1, Z_MAX_MEMLEVEL: 9, Z_DEFAULT_MEMLEVEL: 8,
  Z_MIN_LEVEL: -1, Z_MAX_LEVEL: 9, Z_DEFAULT_LEVEL: -1,
};

function invalidArgType(name, actual) {
  const err = new TypeError('The "' + name + '" argument must be of type string or an instance of ' +
    'Buffer, TypedArray, DataView, or ArrayBuffer. Received ' +
    (actual === null ? 'null' : actual === undefined ? 'undefined' : 'type ' + typeof actual));
  err.code = 'ERR_INVALID_ARG_TYPE';
  return err;
}

function toBuffer(data) {
  if (typeof data === 'string') return Buffer.from(data);
  if (Buffer.isBuffer(data)) return data;
  if (ArrayBuffer.isView(data)) return Buffer.from(data.buffer, data.byteOffset, data.byteLength);
  if (data instanceof ArrayBuffer) return Buffer.from(data);
  throw invalidArgType('buffer', data);
}

function level(options) {
  return options && typeof options.level === 'number' ? options.level : -1;
}

function run(op, data, options) {
  const out = Buffer.from(host.zlib(op, toBuffer(data), level(options)));
  return options && options.info ? { buffer: out, engine: null } : out;
}

function syncFn(op) {
  return function (buffer, options) { return run(op, buffer, options); };
}

function asyncFn(op) {
  return function (buffer, options, callback) {
    if (typeof options === 'function') { callback = options; options = {}; }
    if (typeof callback !== 'function') {
      const err = new TypeError('The "callback" argument must be of type function. Received ' +
        (callback === undefined ? 'undefined' : 'type ' + typeof callback));
      err.code = 'ERR_INVALID_ARG_TYPE';
      throw err;
    }
    const data = toBuffer(buffer);
    require('timers').setImmediate(() => {
      let result;
      try {
        result = run(op, data, options);
      } catch (err) {
        callback(err);
        return;
      }
      callback(null, result);
    });
  };
}

const OPS = {
  Deflate: 'deflate', Inflate: 'inflate', DeflateRaw: 'deflateRaw', InflateRaw: 'inflateRaw',
  Gzip: 'gzip', Gunzip: 'gunzip', Unzip: 'unzip',
};

// Stream classes extend stream.Transform, linked on first construction so
// requiring zlib does not pull in the stream module.
const classes = {};
let linked = false;

function linkStreams() {
  if (linked) return;
  linked = true;
  const { Transform } = require('stream');
  for (const name of Object.keys(classes)) {
    Object.setPrototypeOf(classes[name].prototype, Transform.prototype);
    Object.setPrototypeOf(classes[name], Transform);
  }
}

function makeClass(name, op) {
  const Ctor = function (options) {
    if (!(this instanceof Ctor)) return new Ctor(options);
    linkStreams();
    require('stream').Transform.call(this, options);
    this._op = op;
    this._options = options || {};
    this._chunks = [];
    this.bytesWritten = 0;
  };
  Object.defineProperty(Ctor, 'name', { value: name });
  Ctor.prototype._transform = function (chunk, encoding, callback) {
    const buf = typeof chunk === 'string' ? Buffer.from(chunk, encoding) : toBuffer(chunk);
    this._chunks.push(buf);
    this.bytesWritten += buf.length;
    callback();
  };
  Ctor.prototype._flush = function (callback) {
    let out;
    try {
      out = run(this._op, Buffer.concat(this._chunks), this._options);
    } catch (err) {
      callback(err);
      return;
    }
    this._chunks = [];
    if (out.length) this.push(out);
    callback();
  };
  Ctor.prototype.flush = function (kind, callback) {
    if (typeof kind === 'function') callback = kind;
    if (typeof callback === 'function') require('timers').setImmediate(callback);
  };
  Ctor.prototype.params = function (lvl, strategy, callback) {
    this._options = Object.assign({}, this._options, { level: lvl });
    if (typeof callback === 'function') require('timers').setImmediate(callback);
  };
  Ctor.prototype.reset = function () { this._chunks = []; };
  Ctor.prototype.close = function (callback) {
    if (typeof callback === 'function') this.once('close', callback);
    this.destroy();
  };
  classes[name] = Ctor;
  return Ctor;
}

function brotliUnsupported() {
  const err = new Error('Brotli is not supported by w3cos-node');
  err.code = 'ERR_FEATURE_UNAVAILABLE_ON_PLATFORM';
  throw err;
}

const zlib = { constants, codes };
for (const name of Object.keys(OPS)) {
  const op = OPS[name];
  const Ctor = makeClass(name, op);
  zlib[name] = Ctor;
  zlib['create' + name] = (options) => new Ctor(options);
  zlib[op] = asyncFn(op);
  zlib[op + 'Sync'] = syncFn(op);
}
for (const key of Object.keys(constants)) {
  if (key.startsWith('Z_')) zlib[key] = constants[key];
}
zlib.brotliCompress = brotliUnsupported;
zlib.brotliCompressSync = brotliUnsupported;
zlib.brotliDecompress = brotliUnsupported;
zlib.brotliDecompressSync = brotliUnsupported;
zlib.createBrotliCompress = brotliUnsupported;
zlib.createBrotliDecompress = brotliUnsupported;

module.exports = zlib;
