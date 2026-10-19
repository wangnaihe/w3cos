'use strict';
// `buffer`: Node's Buffer as Uint8Arrays whose prototype is Buffer.prototype
// (installed with Object.setPrototypeOf, which the runtime supports on typed
// arrays). Views share memory with their ArrayBuffer like Node's.

const kMaxLength = 0x7fffffff;
const kStringMaxLength = 0x1fffffe8;
const hasSymbolFor = typeof Symbol !== 'undefined' && Symbol !== null && typeof Symbol.for === 'function';
const customInspect = hasSymbolFor ? Symbol.for('nodejs.util.inspect.custom') : '__w3cos_inspect_custom';

function typeError(code, message) {
  const error = new TypeError(message);
  error.code = code;
  return error;
}

function rangeError(code, message) {
  const error = new RangeError(message);
  error.code = code;
  return error;
}

function Buffer(arg, encodingOrOffset, length) {
  if (typeof arg === 'number') return Buffer.alloc(arg);
  return Buffer.from(arg, encodingOrOffset, length);
}

const proto = Buffer.prototype;
if (typeof Uint8Array === 'function' && Uint8Array.prototype && typeof Uint8Array.prototype === 'object') {
  Object.setPrototypeOf(proto, Uint8Array.prototype);
}

function wrap(bytes) {
  Object.setPrototypeOf(bytes, proto);
  return bytes;
}

function normalizeEncoding(encoding) {
  if (encoding === undefined || encoding === null || encoding === '') return 'utf8';
  const lower = String(encoding).toLowerCase();
  switch (lower) {
    case 'utf8': case 'utf-8': return 'utf8';
    case 'hex': return 'hex';
    case 'base64': return 'base64';
    case 'base64url': return 'base64url';
    case 'latin1': case 'binary': return 'latin1';
    case 'ascii': return 'ascii';
    case 'ucs2': case 'ucs-2': case 'utf16le': case 'utf-16le': return 'utf16le';
    default: return undefined;
  }
}

function checkEncoding(encoding) {
  const normalized = normalizeEncoding(encoding);
  if (normalized === undefined) throw typeError('ERR_UNKNOWN_ENCODING', 'Unknown encoding: ' + encoding);
  return normalized;
}

// ── encoders: string -> array of byte values ──

function utf8Bytes(string) {
  const out = [];
  for (let i = 0; i < string.length; i++) {
    let code = string.charCodeAt(i);
    if (code >= 0xd800 && code <= 0xdbff && i + 1 < string.length) {
      const next = string.charCodeAt(i + 1);
      if (next >= 0xdc00 && next <= 0xdfff) {
        code = 0x10000 + ((code - 0xd800) << 10) + (next - 0xdc00);
        i++;
      } else {
        code = 0xfffd;
      }
    } else if (code >= 0xd800 && code <= 0xdfff) {
      code = 0xfffd;
    }
    if (code < 0x80) out.push(code);
    else if (code < 0x800) out.push(0xc0 | (code >> 6), 0x80 | (code & 63));
    else if (code < 0x10000) out.push(0xe0 | (code >> 12), 0x80 | ((code >> 6) & 63), 0x80 | (code & 63));
    else out.push(0xf0 | (code >> 18), 0x80 | ((code >> 12) & 63), 0x80 | ((code >> 6) & 63), 0x80 | (code & 63));
  }
  return out;
}

function hexBytes(string) {
  const out = [];
  for (let i = 0; i + 1 < string.length; i += 2) {
    const byte = parseInt(string.substr(i, 2), 16);
    if (byte !== byte) break;
    out.push(byte);
  }
  return out;
}

const B64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
const B64URL = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_';

function base64Value(code) {
  if (code >= 65 && code <= 90) return code - 65;
  if (code >= 97 && code <= 122) return code - 71;
  if (code >= 48 && code <= 57) return code + 4;
  if (code === 43 || code === 45) return 62;
  if (code === 47 || code === 95) return 63;
  return -1;
}

function base64Bytes(string) {
  const out = [];
  let bits = 0;
  let count = 0;
  for (let i = 0; i < string.length; i++) {
    const code = string.charCodeAt(i);
    if (code === 61) break;
    const value = base64Value(code);
    if (value < 0) continue;
    bits = (bits << 6) | value;
    count += 6;
    if (count >= 8) {
      count -= 8;
      out.push((bits >> count) & 255);
    }
  }
  return out;
}

function latin1Bytes(string) {
  const out = [];
  for (let i = 0; i < string.length; i++) out.push(string.charCodeAt(i) & 255);
  return out;
}

function utf16Bytes(string) {
  const out = [];
  for (let i = 0; i < string.length; i++) {
    const code = string.charCodeAt(i);
    out.push(code & 255, code >> 8);
  }
  return out;
}

function stringBytes(string, encoding) {
  switch (encoding) {
    case 'utf8': return utf8Bytes(string);
    case 'hex': return hexBytes(string);
    case 'base64': case 'base64url': return base64Bytes(string);
    case 'latin1': case 'ascii': return latin1Bytes(string);
    case 'utf16le': return utf16Bytes(string);
    default: return utf8Bytes(string);
  }
}

// ── decoders: bytes [start, end) -> string ──

function utf8Slice(buf, start, end) {
  // WHATWG UTF-8 decode: one U+FFFD per maximal invalid subpart.
  let out = '';
  let i = start;
  while (i < end) {
    const byte = buf[i++];
    if (byte < 0x80) { out += String.fromCharCode(byte); continue; }
    let need = 0;
    let code = 0;
    let lower = 0x80;
    let upper = 0xbf;
    if (byte >= 0xc2 && byte <= 0xdf) { need = 1; code = byte & 31; }
    else if (byte >= 0xe0 && byte <= 0xef) {
      need = 2; code = byte & 15;
      if (byte === 0xe0) lower = 0xa0;
      if (byte === 0xed) upper = 0x9f;
    } else if (byte >= 0xf0 && byte <= 0xf4) {
      need = 3; code = byte & 7;
      if (byte === 0xf0) lower = 0x90;
      if (byte === 0xf4) upper = 0x8f;
    } else { out += '\ufffd'; continue; }
    let ok = true;
    for (let k = 0; k < need; k++) {
      const next = buf[i];
      if (i >= end || next < lower || next > upper) { ok = false; break; }
      code = (code << 6) | (next & 63);
      lower = 0x80; upper = 0xbf;
      i++;
    }
    if (!ok) { out += '\ufffd'; continue; }
    if (code > 0xffff) {
      code -= 0x10000;
      out += String.fromCharCode(0xd800 + (code >> 10), 0xdc00 + (code & 1023));
    } else {
      out += String.fromCharCode(code);
    }
  }
  return out;
}

function hexSlice(buf, start, end) {
  let out = '';
  for (let i = start; i < end; i++) out += (buf[i] < 16 ? '0' : '') + buf[i].toString(16);
  return out;
}

function base64Slice(buf, start, end, alphabet, pad) {
  let out = '';
  let i = start;
  for (; i + 2 < end; i += 3) {
    const n = (buf[i] << 16) | (buf[i + 1] << 8) | buf[i + 2];
    out += alphabet.charAt((n >> 18) & 63) + alphabet.charAt((n >> 12) & 63) +
      alphabet.charAt((n >> 6) & 63) + alphabet.charAt(n & 63);
  }
  const rest = end - i;
  if (rest === 1) {
    const n = buf[i] << 16;
    out += alphabet.charAt((n >> 18) & 63) + alphabet.charAt((n >> 12) & 63) + (pad ? '==' : '');
  } else if (rest === 2) {
    const n = (buf[i] << 16) | (buf[i + 1] << 8);
    out += alphabet.charAt((n >> 18) & 63) + alphabet.charAt((n >> 12) & 63) +
      alphabet.charAt((n >> 6) & 63) + (pad ? '=' : '');
  }
  return out;
}

function latin1Slice(buf, start, end, mask) {
  let out = '';
  for (let i = start; i < end; i++) out += String.fromCharCode(buf[i] & mask);
  return out;
}

function utf16Slice(buf, start, end) {
  let out = '';
  for (let i = start; i + 1 < end; i += 2) out += String.fromCharCode(buf[i] | (buf[i + 1] << 8));
  return out;
}

// ── construction ──

function fromBytes(values) {
  const bytes = new Uint8Array(values.length);
  for (let i = 0; i < values.length; i++) bytes[i] = values[i] & 255;
  return wrap(bytes);
}

Buffer.alloc = function alloc(size, fill, encoding) {
  if (typeof size !== 'number' || size < 0 || size > kMaxLength || size !== size) {
    throw rangeError('ERR_OUT_OF_RANGE', 'The value of "size" is out of range. Received ' + size);
  }
  const buf = wrap(new Uint8Array(Math.floor(size)));
  if (fill !== undefined && fill !== 0 && size > 0) buf.fill(fill, encoding);
  return buf;
};
Buffer.allocUnsafe = function allocUnsafe(size) { return Buffer.alloc(size); };
Buffer.allocUnsafeSlow = function allocUnsafeSlow(size) { return Buffer.alloc(size); };

Buffer.from = function from(value, encodingOrOffset, length) {
  if (typeof value === 'string') {
    return fromBytes(stringBytes(value, checkEncoding(encodingOrOffset)));
  }
  if (value instanceof ArrayBuffer) {
    const offset = encodingOrOffset === undefined ? 0 : Number(encodingOrOffset);
    const count = length === undefined ? value.byteLength - offset : Number(length);
    if (offset > value.byteLength) throw rangeError('ERR_BUFFER_OUT_OF_BOUNDS', '"offset" is outside of buffer bounds');
    return wrap(new Uint8Array(value, offset, count));
  }
  if (value !== null && typeof value === 'object') {
    if (typeof value.length === 'number' || Array.isArray(value)) {
      const values = [];
      for (let i = 0; i < value.length; i++) values.push(Number(value[i]) || 0);
      return fromBytes(values);
    }
    if (value.type === 'Buffer' && Array.isArray(value.data)) return fromBytes(value.data);
    if (typeof value.valueOf === 'function') {
      const primitive = value.valueOf();
      if (primitive !== value && primitive !== null && primitive !== undefined) {
        return Buffer.from(primitive, encodingOrOffset, length);
      }
    }
  }
  throw typeError('ERR_INVALID_ARG_TYPE',
    'The first argument must be of type string or an instance of Buffer, ArrayBuffer, or Array or an Array-like Object.');
};

Buffer.isBuffer = function isBuffer(value) {
  return value !== null && value !== undefined && typeof value === 'object' &&
    Object.getPrototypeOf(value) === proto;
};

Buffer.isEncoding = function isEncoding(encoding) {
  return typeof encoding === 'string' && encoding !== '' && normalizeEncoding(encoding) !== undefined;
};

Buffer.byteLength = function byteLength(value, encoding) {
  if (typeof value !== 'string') {
    if (value && typeof value.byteLength === 'number') return value.byteLength;
    throw typeError('ERR_INVALID_ARG_TYPE', 'The "string" argument must be of type string or an instance of Buffer or ArrayBuffer.');
  }
  const normalized = normalizeEncoding(encoding) || 'utf8';
  if (normalized === 'latin1' || normalized === 'ascii') return value.length;
  if (normalized === 'utf16le') return value.length * 2;
  if (normalized === 'hex') return value.length >>> 1;
  return stringBytes(value, normalized).length;
};

Buffer.concat = function concat(list, totalLength) {
  if (!Array.isArray(list)) throw typeError('ERR_INVALID_ARG_TYPE', 'The "list" argument must be an instance of Array.');
  if (totalLength === undefined) {
    totalLength = 0;
    for (let i = 0; i < list.length; i++) totalLength += list[i].length;
  }
  const out = Buffer.alloc(totalLength);
  let offset = 0;
  for (let i = 0; i < list.length && offset < totalLength; i++) {
    const item = list[i];
    for (let j = 0; j < item.length && offset < totalLength; j++) out[offset++] = item[j];
  }
  return out;
};

function compareBytes(a, aStart, aEnd, b, bStart, bEnd) {
  const n = Math.min(aEnd - aStart, bEnd - bStart);
  for (let i = 0; i < n; i++) {
    if (a[aStart + i] !== b[bStart + i]) return a[aStart + i] < b[bStart + i] ? -1 : 1;
  }
  const la = aEnd - aStart;
  const lb = bEnd - bStart;
  return la < lb ? -1 : la > lb ? 1 : 0;
}

Buffer.compare = function compare(a, b) { return compareBytes(a, 0, a.length, b, 0, b.length); };

Buffer.poolSize = 8192;

// ── prototype ──

function clampIndex(value, length, fallback) {
  if (value === undefined) return fallback;
  let index = Math.trunc(Number(value)) || 0;
  if (index < 0) index = Math.max(length + index, 0);
  return Math.min(index, length);
}

proto.toString = function toString(encoding, start, end) {
  const length = this.length;
  start = start === undefined ? 0 : Math.max(0, Math.trunc(start) || 0);
  end = end === undefined ? length : Math.min(length, Math.trunc(end) || 0);
  if (end <= start) return '';
  switch (checkEncoding(encoding)) {
    case 'hex': return hexSlice(this, start, end);
    case 'base64': return base64Slice(this, start, end, B64, true);
    case 'base64url': return base64Slice(this, start, end, B64URL, false);
    case 'latin1': return latin1Slice(this, start, end, 255);
    case 'ascii': return latin1Slice(this, start, end, 127);
    case 'utf16le': return utf16Slice(this, start, end);
    default: return utf8Slice(this, start, end);
  }
};
proto.toLocaleString = proto.toString;

proto.toJSON = function toJSON() {
  const data = [];
  for (let i = 0; i < this.length; i++) data.push(this[i]);
  return { type: 'Buffer', data: data };
};

proto.equals = function equals(other) {
  if (!other || typeof other.length !== 'number') {
    throw typeError('ERR_INVALID_ARG_TYPE', 'The "otherBuffer" argument must be an instance of Buffer or Uint8Array.');
  }
  return compareBytes(this, 0, this.length, other, 0, other.length) === 0;
};

proto.compare = function compare(target, targetStart, targetEnd, sourceStart, sourceEnd) {
  return compareBytes(
    this,
    sourceStart === undefined ? 0 : sourceStart,
    sourceEnd === undefined ? this.length : sourceEnd,
    target,
    targetStart === undefined ? 0 : targetStart,
    targetEnd === undefined ? target.length : targetEnd);
};

proto.subarray = function subarray(start, end) {
  const length = this.length;
  const from = clampIndex(start, length, 0);
  const to = Math.max(from, clampIndex(end, length, length));
  return wrap(new Uint8Array(this.buffer, this.byteOffset + from, to - from));
};
proto.slice = proto.subarray;

proto.copy = function copy(target, targetStart, sourceStart, sourceEnd) {
  targetStart = targetStart === undefined ? 0 : targetStart;
  sourceStart = sourceStart === undefined ? 0 : sourceStart;
  sourceEnd = sourceEnd === undefined ? this.length : Math.min(sourceEnd, this.length);
  let count = 0;
  const values = [];
  for (let i = sourceStart; i < sourceEnd && targetStart + count < target.length; i++, count++) values.push(this[i]);
  for (let i = 0; i < values.length; i++) target[targetStart + i] = values[i];
  return count;
};

proto.write = function write(string, offset, length, encoding) {
  if (typeof offset === 'string') {
    encoding = offset;
    offset = 0;
    length = undefined;
  } else if (typeof length === 'string') {
    encoding = length;
    length = undefined;
  }
  offset = offset === undefined ? 0 : offset >>> 0;
  const remaining = this.length - offset;
  length = length === undefined ? remaining : Math.min(length >>> 0, remaining);
  const bytes = stringBytes(String(string), checkEncoding(encoding));
  const count = Math.min(bytes.length, length);
  for (let i = 0; i < count; i++) this[offset + i] = bytes[i];
  return count;
};

proto.fill = function fill(value, offset, end, encoding) {
  if (typeof offset === 'string') {
    encoding = offset;
    offset = 0;
    end = this.length;
  } else if (typeof end === 'string') {
    encoding = end;
    end = this.length;
  }
  offset = offset === undefined ? 0 : offset;
  end = end === undefined ? this.length : end;
  let pattern;
  if (typeof value === 'number') pattern = [value & 255];
  else if (typeof value === 'string') pattern = stringBytes(value, checkEncoding(encoding));
  else if (value && typeof value.length === 'number') pattern = Array.prototype.slice.call(value);
  else pattern = [0];
  if (pattern.length === 0) pattern = [0];
  for (let i = offset, j = 0; i < end; i++, j++) this[i] = pattern[j % pattern.length];
  return this;
};

function needleBytes(value, encoding) {
  if (typeof value === 'number') return [value & 255];
  if (typeof value === 'string') return stringBytes(value, checkEncoding(encoding));
  return Array.prototype.slice.call(value);
}

proto.indexOf = function indexOf(value, byteOffset, encoding) {
  if (typeof byteOffset === 'string') {
    encoding = byteOffset;
    byteOffset = 0;
  }
  const needle = needleBytes(value, encoding);
  let start = byteOffset === undefined ? 0 : Math.trunc(byteOffset) || 0;
  if (start < 0) start = Math.max(this.length + start, 0);
  if (needle.length === 0) return Math.min(start, this.length);
  outer: for (let i = start; i + needle.length <= this.length; i++) {
    for (let j = 0; j < needle.length; j++) if (this[i + j] !== needle[j]) continue outer;
    return i;
  }
  return -1;
};

proto.lastIndexOf = function lastIndexOf(value, byteOffset, encoding) {
  if (typeof byteOffset === 'string') {
    encoding = byteOffset;
    byteOffset = undefined;
  }
  const needle = needleBytes(value, encoding);
  let start = byteOffset === undefined ? this.length - needle.length : Math.trunc(byteOffset) || 0;
  if (start < 0) start = this.length + start;
  start = Math.min(start, this.length - needle.length);
  outer: for (let i = start; i >= 0; i--) {
    for (let j = 0; j < needle.length; j++) if (this[i + j] !== needle[j]) continue outer;
    return i;
  }
  return -1;
};

proto.includes = function includes(value, byteOffset, encoding) {
  return this.indexOf(value, byteOffset, encoding) !== -1;
};

function checkOffset(buf, offset, size) {
  if (offset === undefined) offset = 0;
  if (typeof offset !== 'number' || offset !== Math.floor(offset)) {
    throw typeError('ERR_INVALID_ARG_TYPE', 'The "offset" argument must be of type number. Received ' + offset);
  }
  if (offset < 0 || offset + size > buf.length) {
    throw rangeError('ERR_OUT_OF_RANGE',
      'The value of "offset" is out of range. It must be >= 0 and <= ' + (buf.length - size) + '. Received ' + offset);
  }
  return offset;
}

function view(buf) {
  return new DataView(buf.buffer, buf.byteOffset, buf.length);
}

const accessors = [
  ['UInt8', 1, 'getUint8', 'setUint8'],
  ['Int8', 1, 'getInt8', 'setInt8'],
  ['UInt16', 2, 'getUint16', 'setUint16'],
  ['Int16', 2, 'getInt16', 'setInt16'],
  ['UInt32', 4, 'getUint32', 'setUint32'],
  ['Int32', 4, 'getInt32', 'setInt32'],
  ['Float', 4, 'getFloat32', 'setFloat32'],
  ['Double', 8, 'getFloat64', 'setFloat64'],
];

function defineAccessor(name, size, getter, setter, suffix, little) {
  proto['read' + name + suffix] = function (offset) {
    offset = checkOffset(this, offset, size);
    return view(this)[getter](offset, little);
  };
  proto['write' + name + suffix] = function (value, offset) {
    offset = checkOffset(this, offset, size);
    const dv = view(this);
    dv[setter](offset, Number(value), little);
    for (let i = 0; i < size; i++) this[offset + i] = dv.getUint8(offset + i);
    return offset + size;
  };
}

for (let i = 0; i < accessors.length; i++) {
  const entry = accessors[i];
  if (entry[1] === 1) {
    defineAccessor(entry[0], 1, entry[2], entry[3], '', false);
  } else {
    defineAccessor(entry[0], entry[1], entry[2], entry[3], 'LE', true);
    defineAccessor(entry[0], entry[1], entry[2], entry[3], 'BE', false);
  }
}
// Lower-case `Uint` aliases (Node >= 14.9).
const aliasNames = Object.keys(proto);
for (let i = 0; i < aliasNames.length; i++) {
  const name = aliasNames[i];
  if (name.indexOf('UInt') !== -1) proto[name.replace('UInt', 'Uint')] = proto[name];
}

proto.swap16 = function swap16() {
  if (this.length % 2 !== 0) throw rangeError('ERR_INVALID_BUFFER_SIZE', 'Buffer size must be a multiple of 16-bits');
  for (let i = 0; i < this.length; i += 2) {
    const a = this[i];
    this[i] = this[i + 1];
    this[i + 1] = a;
  }
  return this;
};

proto.swap32 = function swap32() {
  if (this.length % 4 !== 0) throw rangeError('ERR_INVALID_BUFFER_SIZE', 'Buffer size must be a multiple of 32-bits');
  for (let i = 0; i < this.length; i += 4) {
    const a = this[i];
    const b = this[i + 1];
    this[i] = this[i + 3];
    this[i + 1] = this[i + 2];
    this[i + 2] = b;
    this[i + 3] = a;
  }
  return this;
};

proto.inspect = function inspect() {
  let out = '';
  const max = Math.min(this.length, api.INSPECT_MAX_BYTES);
  for (let i = 0; i < max; i++) out += ' ' + (this[i] < 16 ? '0' : '') + this[i].toString(16);
  if (this.length > max) out += ' ... ' + (this.length - max) + ' more byte' + (this.length - max > 1 ? 's' : '');
  return '<Buffer' + out + '>';
};
proto[customInspect] = proto.inspect;

const api = module.exports;
api.Buffer = Buffer;
api.SlowBuffer = Buffer;
api.INSPECT_MAX_BYTES = 50;
api.kMaxLength = kMaxLength;
api.kStringMaxLength = kStringMaxLength;
api.constants = { MAX_LENGTH: kMaxLength, MAX_STRING_LENGTH: kStringMaxLength };
api.atob = typeof atob === 'function' ? atob : undefined;
api.btoa = typeof btoa === 'function' ? btoa : undefined;
api.isUtf8 = function isUtf8(input) {
  const bytes = input;
  return utf8Slice(bytes, 0, bytes.length).indexOf('\ufffd') === -1;
};
