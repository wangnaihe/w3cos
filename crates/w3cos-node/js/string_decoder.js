'use strict';
// node:string_decoder — incremental decoding that holds back partial
// multi-byte characters between writes.
const { Buffer } = require('buffer');

function normalize(enc) {
  const e = String(enc === undefined || enc === null ? 'utf8' : enc).toLowerCase();
  switch (e) {
    case 'utf8': case 'utf-8': return 'utf8';
    case 'ucs2': case 'ucs-2': case 'utf16le': case 'utf-16le': return 'utf16le';
    case 'latin1': case 'binary': return 'latin1';
    case 'base64': case 'base64url': case 'hex': case 'ascii': return e;
    default: {
      const err = new TypeError('Unknown encoding: ' + enc);
      err.code = 'ERR_UNKNOWN_ENCODING';
      throw err;
    }
  }
}

// Bytes a UTF-8 sequence starting with `byte` needs (0 = continuation/invalid).
function utf8Length(byte) {
  if (byte <= 0x7f) return 1;
  if (byte >> 5 === 0x06) return 2;
  if (byte >> 4 === 0x0e) return 3;
  if (byte >> 3 === 0x1e) return 4;
  return 0;
}

function StringDecoder(encoding) {
  if (!(this instanceof StringDecoder)) return new StringDecoder(encoding);
  this.encoding = normalize(encoding);
  this.pending = [];
}

StringDecoder.prototype._complete = function _complete(bytes) {
  // How many trailing bytes form an incomplete character.
  const enc = this.encoding;
  const n = bytes.length;
  if (enc === 'utf8') {
    for (let back = 1; back <= Math.min(3, n); back++) {
      const b = bytes[n - back];
      if ((b & 0xc0) === 0x80) continue;
      const need = utf8Length(b);
      return need > back ? back : 0;
    }
    return 0;
  }
  if (enc === 'utf16le') {
    let keep = n % 2;
    if (keep === 0 && n >= 2) {
      const unit = bytes[n - 2] | (bytes[n - 1] << 8);
      if (unit >= 0xd800 && unit <= 0xdbff) keep = 2;
    }
    return keep;
  }
  if (enc === 'base64' || enc === 'base64url') return n % 3;
  return 0;
};

StringDecoder.prototype.write = function write(buf) {
  if (typeof buf === 'string') return buf;
  const bytes = this.pending.concat(Array.prototype.slice.call(buf));
  const keep = this._complete(bytes);
  this.pending = bytes.slice(bytes.length - keep);
  const body = bytes.slice(0, bytes.length - keep);
  return body.length ? Buffer.from(body).toString(this.encoding) : '';
};

StringDecoder.prototype.end = function end(buf) {
  let out = buf === undefined ? '' : this.write(buf);
  if (this.pending.length) {
    out += Buffer.from(this.pending).toString(this.encoding);
    this.pending = [];
  }
  return out;
};

StringDecoder.prototype.text = function text(buf, offset) {
  return this.write(Buffer.from(buf).subarray(offset || 0));
};

module.exports = { StringDecoder };
