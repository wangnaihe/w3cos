'use strict';
// node:crypto — digests, HMAC, PBKDF2 and randomness over native host
// functions (`__w3cos_node_host`: RustCrypto md-5/sha1/sha2/hmac and the OS
// random source). Ciphers, keys and signatures are not provided.
const host = __w3cos_node_host;
const { Buffer } = require('buffer');

const HASHES = ['RSA-MD5', 'RSA-SHA1', 'RSA-SHA224', 'RSA-SHA256', 'RSA-SHA384', 'RSA-SHA512',
  'md5', 'sha1', 'sha224', 'sha256', 'sha384', 'sha512'];

function codedError(Ctor, code, message) {
  const err = new Ctor(message);
  err.code = code;
  return err;
}

function describe(value) {
  if (value === null) return 'null';
  if (value === undefined) return 'undefined';
  if (typeof value === 'object') {
    return 'an instance of ' + ((value.constructor && value.constructor.name) || 'Object');
  }
  return 'type ' + typeof value + ' (' + String(value) + ')';
}

function invalidArgType(name, expected, actual) {
  return codedError(TypeError, 'ERR_INVALID_ARG_TYPE',
    'The "' + name + '" argument must be ' + expected + '. Received ' + describe(actual));
}

function toBuffer(data, encoding, name) {
  if (typeof data === 'string') return Buffer.from(data, encoding || 'utf8');
  if (Buffer.isBuffer(data)) return data;
  if (ArrayBuffer.isView(data)) return Buffer.from(data.buffer, data.byteOffset, data.byteLength);
  if (data instanceof ArrayBuffer) return Buffer.from(data);
  throw invalidArgType(name || 'data',
    'of type string or an instance of Buffer, TypedArray, or DataView', data);
}

function encodeOutput(bytes, encoding) {
  const buf = Buffer.from(bytes);
  return encoding === undefined || encoding === 'buffer' ? buf : buf.toString(encoding);
}

function checkAlgorithm(algorithm) {
  if (typeof algorithm !== 'string') throw invalidArgType('algorithm', 'of type string', algorithm);
  if (host.hash(algorithm, '') === undefined) throw new Error('Digest method not supported');
  return algorithm;
}

class Hash {
  constructor(algorithm, options) {
    this._algorithm = checkAlgorithm(algorithm);
    this._chunks = [];
    this._finalized = false;
    void options;
  }
  update(data, inputEncoding) {
    if (this._finalized) throw codedError(Error, 'ERR_CRYPTO_HASH_FINALIZED', 'Digest already called');
    this._chunks.push(toBuffer(data, inputEncoding));
    return this;
  }
  _bytes() {
    if (this._finalized) throw codedError(Error, 'ERR_CRYPTO_HASH_FINALIZED', 'Digest already called');
    this._finalized = true;
    return host.hash(this._algorithm, Buffer.concat(this._chunks));
  }
  digest(outputEncoding) {
    return encodeOutput(this._bytes(), outputEncoding);
  }
  copy() {
    if (this._finalized) throw codedError(Error, 'ERR_CRYPTO_HASH_FINALIZED', 'Digest already called');
    const copy = new Hash(this._algorithm);
    copy._chunks = this._chunks.slice();
    return copy;
  }
}

class Hmac {
  constructor(algorithm, key, options) {
    this._algorithm = checkAlgorithm(algorithm);
    if (key && typeof key === 'object' && key.type === 'secret' && typeof key.export === 'function') {
      key = key.export();
    }
    this._key = toBuffer(key, undefined, 'key');
    this._chunks = [];
    this._finalized = false;
    void options;
  }
  update(data, inputEncoding) {
    if (this._finalized) throw codedError(Error, 'ERR_CRYPTO_HASH_FINALIZED', 'Digest already called');
    this._chunks.push(toBuffer(data, inputEncoding));
    return this;
  }
  digest(outputEncoding) {
    // Node returns an empty digest after the first call instead of throwing.
    if (this._finalized) return encodeOutput(new ArrayBuffer(0), outputEncoding);
    this._finalized = true;
    return encodeOutput(host.hmac(this._algorithm, this._key, Buffer.concat(this._chunks)), outputEncoding);
  }
}

function createHash(algorithm, options) {
  return new Hash(algorithm, options);
}

function createHmac(algorithm, key, options) {
  return new Hmac(algorithm, key, options);
}

function getHashes() {
  return HASHES.slice();
}

function pbkdf2Sync(password, salt, iterations, keylen, digest) {
  password = toBuffer(password, undefined, 'password');
  salt = toBuffer(salt, undefined, 'salt');
  if (typeof iterations !== 'number' || iterations < 1) {
    throw codedError(RangeError, 'ERR_OUT_OF_RANGE',
      'The value of "iterations" is out of range. It must be >= 1 && <= 2147483647. Received ' + iterations);
  }
  if (typeof keylen !== 'number' || keylen < 0) {
    throw codedError(RangeError, 'ERR_OUT_OF_RANGE',
      'The value of "keylen" is out of range. It must be >= 0 && <= 2147483647. Received ' + keylen);
  }
  if (typeof digest !== 'string') throw invalidArgType('digest', 'of type string', digest);
  const bytes = host.pbkdf2(password, salt, iterations, keylen, digest);
  if (bytes === undefined) {
    throw codedError(TypeError, 'ERR_CRYPTO_INVALID_DIGEST', 'Invalid digest: ' + digest);
  }
  return Buffer.from(bytes);
}

function later(callback, err, value) {
  require('timers').setImmediate(() => callback(err, value));
}

function pbkdf2(password, salt, iterations, keylen, digest, callback) {
  if (typeof callback !== 'function') throw invalidArgType('callback', 'of type function', callback);
  const key = pbkdf2Sync(password, salt, iterations, keylen, digest);
  later(callback, null, key);
}

function randomBytes(size, callback) {
  if (typeof size !== 'number' || size < 0 || size > 2147483647 || size !== Math.floor(size)) {
    throw codedError(RangeError, 'ERR_OUT_OF_RANGE',
      'The value of "size" is out of range. It must be >= 0 && <= 2147483647. Received ' + size);
  }
  const buf = Buffer.from(host.randomBytes(size));
  if (typeof callback === 'function') {
    later(callback, null, buf);
    return undefined;
  }
  return buf;
}

function randomFillSync(buf, offset, size) {
  if (!ArrayBuffer.isView(buf) && !(buf instanceof ArrayBuffer)) {
    throw invalidArgType('buf', 'an instance of ArrayBuffer or ArrayBufferView', buf);
  }
  const bytes = buf instanceof ArrayBuffer
    ? new Uint8Array(buf)
    : new Uint8Array(buf.buffer, buf.byteOffset, buf.byteLength);
  offset = offset === undefined ? 0 : offset;
  size = size === undefined ? bytes.length - offset : size;
  const random = new Uint8Array(host.randomBytes(size));
  for (let i = 0; i < size; i++) bytes[offset + i] = random[i];
  return buf;
}

function randomFill(buf, offset, size, callback) {
  if (typeof offset === 'function') { callback = offset; offset = undefined; size = undefined; }
  else if (typeof size === 'function') { callback = size; size = undefined; }
  randomFillSync(buf, offset, size);
  later(callback, null, buf);
}

function randomInt(min, max, callback) {
  if (typeof max === 'function' || max === undefined) {
    callback = max;
    max = min;
    min = 0;
  }
  if (!Number.isSafeInteger(min)) throw invalidArgType('min', 'a safe integer', min);
  if (!Number.isSafeInteger(max)) throw invalidArgType('max', 'a safe integer', max);
  if (max <= min) {
    throw codedError(RangeError, 'ERR_OUT_OF_RANGE',
      'The value of "max" is out of range. It must be greater than the value of "min" (' + min + '). Received ' + max);
  }
  const range = max - min;
  const bytes = new Uint8Array(host.randomBytes(6));
  let n = 0;
  for (let i = 0; i < 6; i++) n = n * 256 + bytes[i];
  const value = min + (n % range);
  if (typeof callback === 'function') {
    later(callback, null, value);
    return undefined;
  }
  return value;
}

function randomUUID() {
  const b = new Uint8Array(host.randomBytes(16));
  b[6] = (b[6] & 0x0f) | 0x40;
  b[8] = (b[8] & 0x3f) | 0x80;
  const hex = Buffer.from(b).toString('hex');
  return hex.slice(0, 8) + '-' + hex.slice(8, 12) + '-' + hex.slice(12, 16) + '-' +
    hex.slice(16, 20) + '-' + hex.slice(20);
}

function getRandomValues(array) {
  if (!ArrayBuffer.isView(array) || array instanceof Float32Array || array instanceof Float64Array) {
    throw codedError(TypeError, 'ERR_INVALID_ARG_TYPE',
      'The "typedArray" argument must be an instance of Int8Array, Int16Array, Int32Array, Uint8Array, Uint16Array, Uint32Array, or Uint8ClampedArray.');
  }
  if (array.byteLength > 65536) {
    const err = new Error("Failed to execute 'getRandomValues' on 'Crypto': The ArrayBufferView's byte length (" +
      array.byteLength + ') exceeds the number of bytes of entropy available via this API (65536)');
    err.name = 'QuotaExceededError';
    throw err;
  }
  randomFillSync(array);
  return array;
}

function timingSafeEqual(a, b) {
  a = toBuffer(a, undefined, 'buf1');
  b = toBuffer(b, undefined, 'buf2');
  if (a.length !== b.length) {
    throw codedError(RangeError, 'ERR_CRYPTO_TIMING_SAFE_EQUAL_LENGTH',
      'Input buffers must have the same byte length');
  }
  let diff = 0;
  for (let i = 0; i < a.length; i++) diff |= a[i] ^ b[i];
  return diff === 0;
}

function unsupported(name) {
  return function () {
    throw codedError(Error, 'ERR_FEATURE_UNAVAILABLE_ON_PLATFORM',
      'crypto.' + name + ' is not supported by w3cos-node');
  };
}

const webcrypto = { getRandomValues, randomUUID };

module.exports = {
  Hash,
  Hmac,
  createHash,
  createHmac,
  getHashes,
  getCiphers: () => [],
  getCurves: () => [],
  pbkdf2,
  pbkdf2Sync,
  randomBytes,
  pseudoRandomBytes: randomBytes,
  randomFill,
  randomFillSync,
  randomInt,
  randomUUID,
  getRandomValues,
  timingSafeEqual,
  webcrypto,
  constants: {},
  createCipheriv: unsupported('createCipheriv'),
  createDecipheriv: unsupported('createDecipheriv'),
  createSign: unsupported('createSign'),
  createVerify: unsupported('createVerify'),
  generateKeyPairSync: unsupported('generateKeyPairSync'),
  scryptSync: unsupported('scryptSync'),
};
