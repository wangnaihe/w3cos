'use strict';
// node:querystring

const UNESCAPED = /[A-Za-z0-9\-_.!~*'()]/;

function qsEscape(str) {
  if (typeof str !== 'string') {
    if (typeof str === 'object') str = String(str);
    else str += '';
  }
  let out = '';
  for (let i = 0; i < str.length; i++) {
    const ch = str[i];
    if (UNESCAPED.test(ch)) { out += ch; continue; }
    const code = str.charCodeAt(i);
    if (code >= 0xd800 && code <= 0xdbff && i + 1 < str.length) {
      out += encodeURIComponent(str[i] + str[i + 1]);
      i++;
      continue;
    }
    if (code >= 0xd800 && code <= 0xdfff) {
      const err = new URIError('URI malformed');
      err.code = 'ERR_INVALID_URI';
      throw err;
    }
    out += encodeURIComponent(ch);
  }
  return out;
}

function hexValue(c) {
  if (c >= 48 && c <= 57) return c - 48;
  if (c >= 65 && c <= 70) return c - 55;
  if (c >= 97 && c <= 102) return c - 87;
  return -1;
}

function qsUnescape(s) {
  try {
    return decodeURIComponent(s);
  } catch (e) {
    // Like node's unescapeBuffer: decode valid %XX bytes, keep the rest.
    const bytes = [];
    for (let i = 0; i < s.length; i++) {
      const c = s.charCodeAt(i);
      if (c === 37 && i + 2 < s.length) {
        const hi = hexValue(s.charCodeAt(i + 1));
        const lo = hexValue(s.charCodeAt(i + 2));
        if (hi >= 0 && lo >= 0) { bytes.push(hi * 16 + lo); i += 2; continue; }
      }
      const enc = new TextEncoder().encode(s[i]);
      for (let j = 0; j < enc.length; j++) bytes.push(enc[j]);
    }
    return new TextDecoder().decode(new Uint8Array(bytes));
  }
}

function stringifyPrimitive(v) {
  if (typeof v === 'string') return v;
  if (typeof v === 'number' && isFinite(v)) return '' + v;
  if (typeof v === 'bigint') return '' + v;
  if (typeof v === 'boolean') return v ? 'true' : 'false';
  return '';
}

function stringify(obj, sep, eq, options) {
  sep = sep || '&';
  eq = eq || '=';
  const encode = (options && typeof options.encodeURIComponent === 'function')
    ? options.encodeURIComponent : api.escape;
  if (obj === null || typeof obj !== 'object') return '';
  const parts = [];
  const keys = Object.keys(obj);
  for (let i = 0; i < keys.length; i++) {
    const k = keys[i];
    const v = obj[k];
    const ks = encode(stringifyPrimitive(k)) + eq;
    if (Array.isArray(v)) {
      for (let j = 0; j < v.length; j++) parts.push(ks + encode(stringifyPrimitive(v[j])));
    } else {
      parts.push(ks + encode(stringifyPrimitive(v)));
    }
  }
  return parts.join(sep);
}

function parse(qs, sep, eq, options) {
  const obj = Object.create(null);
  if (typeof qs !== 'string' || qs.length === 0) return obj;
  sep = sep || '&';
  eq = eq || '=';
  let maxKeys = 1000;
  if (options && typeof options.maxKeys === 'number') maxKeys = options.maxKeys;
  const decode = (options && typeof options.decodeURIComponent === 'function')
    ? options.decodeURIComponent : api.unescape;
  let pairs = qs.split(sep);
  if (maxKeys > 0 && pairs.length > maxKeys) pairs = pairs.slice(0, maxKeys);
  for (let i = 0; i < pairs.length; i++) {
    const pair = pairs[i];
    if (pair.length === 0 && pairs.length > 1) continue;
    const idx = pair.indexOf(eq);
    let k, v;
    if (idx >= 0) { k = pair.slice(0, idx); v = pair.slice(idx + eq.length); }
    else { k = pair; v = ''; }
    k = decode(k.split('+').join(' '));
    v = decode(v.split('+').join(' '));
    if (!(k in obj)) obj[k] = v;
    else if (Array.isArray(obj[k])) obj[k].push(v);
    else obj[k] = [obj[k], v];
  }
  return obj;
}

const api = module.exports;
api.escape = qsEscape;
api.unescape = qsUnescape;
api.unescapeBuffer = function unescapeBuffer(s) { return require('buffer').Buffer.from(qsUnescape(s)); };
api.stringify = stringify;
api.encode = stringify;
api.parse = parse;
api.decode = parse;
