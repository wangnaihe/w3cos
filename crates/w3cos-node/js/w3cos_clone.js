'use strict';
// Structured clone across worker threads: the HTML structured-serialize
// algorithm over a JSON string, so only strings cross the thread boundary
// (each thread has its own heap). Covers primitives incl. undefined / NaN /
// -0 / BigInt, plain objects and arrays with cycles and shared references,
// Date, RegExp, Map, Set, Error, ArrayBuffer and typed arrays (a Buffer
// arrives as a Uint8Array, as in Node). Functions and symbols throw
// DataCloneError.

const TYPED = ['Int8Array', 'Uint8Array', 'Uint8ClampedArray', 'Int16Array', 'Uint16Array',
  'Int32Array', 'Uint32Array', 'Float32Array', 'Float64Array', 'BigInt64Array', 'BigUint64Array'];

function dataCloneError(value) {
  const text = typeof value === 'function' ? String(value) : String(value && value.toString ? value.toString() : value);
  const error = new Error(text + ' could not be cloned.');
  error.name = 'DataCloneError';
  error.code = 25;
  return error;
}

const HEX = '0123456789abcdef';
function toHex(bytes) {
  let out = '';
  for (let i = 0; i < bytes.length; i++) out += HEX[bytes[i] >> 4] + HEX[bytes[i] & 15];
  return out;
}
function fromHex(text) {
  const bytes = new Uint8Array(text.length / 2);
  for (let i = 0; i < bytes.length; i++) bytes[i] = parseInt(text.substr(i * 2, 2), 16);
  return bytes;
}

function typedName(value) {
  for (const name of TYPED) {
    const Ctor = globalThis[name];
    if (typeof Ctor === 'function' && value instanceof Ctor) return name;
  }
  return null;
}

function encodeValue(value, seen) {
  switch (typeof value) {
    case 'string':
    case 'boolean':
      return value;
    case 'undefined':
      return { $: 'u' };
    case 'number':
      if (Number.isNaN(value)) return { $: 'n', v: 'NaN' };
      if (value === Infinity) return { $: 'n', v: 'Infinity' };
      if (value === -Infinity) return { $: 'n', v: '-Infinity' };
      if (value === 0 && 1 / value < 0) return { $: 'n', v: '-0' };
      return value;
    case 'bigint':
      return { $: 'b', v: value.toString() };
    case 'function':
    case 'symbol':
      throw dataCloneError(value);
    default:
      break;
  }
  if (value === null) return null;
  if (seen.has(value)) return { $: 'r', i: seen.get(value) };
  const id = seen.size;
  seen.set(value, id);
  if (value instanceof Date) return { $: 'd', i: id, v: value.getTime() };
  if (value instanceof RegExp) return { $: 're', i: id, s: value.source, f: value.flags };
  if (value instanceof Map) {
    const entries = [];
    for (const [k, v] of value) entries.push([encodeValue(k, seen), encodeValue(v, seen)]);
    return { $: 'm', i: id, v: entries };
  }
  if (value instanceof Set) {
    const items = [];
    for (const v of value) items.push(encodeValue(v, seen));
    return { $: 's', i: id, v: items };
  }
  if (value instanceof ArrayBuffer) return { $: 'ab', i: id, v: toHex(new Uint8Array(value)) };
  const typed = typedName(value);
  if (typed !== null) {
    const bytes = new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
    return { $: 'ta', i: id, t: typed, v: toHex(bytes) };
  }
  if (typeof DataView === 'function' && value instanceof DataView) {
    return { $: 'dv', i: id, v: toHex(new Uint8Array(value.buffer, value.byteOffset, value.byteLength)) };
  }
  // After the typed-array checks: w3cos reports typed arrays as arrays.
  if (Array.isArray(value)) {
    const items = [];
    for (let i = 0; i < value.length; i++) items.push(encodeValue(value[i], seen));
    return { $: 'a', i: id, v: items };
  }
  if (value instanceof Error) {
    const out = { $: 'e', i: id, n: String(value.name), m: String(value.message) };
    if (value.stack !== undefined) out.st = String(value.stack);
    if (value.cause !== undefined) out.c = encodeValue(value.cause, seen);
    return out;
  }
  const keys = Object.keys(value);
  const entries = [];
  for (const key of keys) entries.push([key, encodeValue(value[key], seen)]);
  return { $: 'o', i: id, v: entries };
}

function serialize(value) {
  return JSON.stringify(encodeValue(value, new Map()));
}

const ERRORS = { Error, EvalError, RangeError, ReferenceError, SyntaxError, TypeError, URIError };

function decodeValue(node, refs) {
  if (node === null || typeof node !== 'object') return node;
  switch (node.$) {
    case 'u': return undefined;
    case 'n': return node.v === '-0' ? -0 : Number(node.v);
    case 'b': return BigInt(node.v);
    case 'r': return refs[node.i];
    case 'a': {
      const out = [];
      refs[node.i] = out;
      for (const item of node.v) out.push(decodeValue(item, refs));
      return out;
    }
    case 'd': return (refs[node.i] = new Date(node.v));
    case 're': return (refs[node.i] = new RegExp(node.s, node.f));
    case 'm': {
      const out = new Map();
      refs[node.i] = out;
      for (const [k, v] of node.v) out.set(decodeValue(k, refs), decodeValue(v, refs));
      return out;
    }
    case 's': {
      const out = new Set();
      refs[node.i] = out;
      for (const v of node.v) out.add(decodeValue(v, refs));
      return out;
    }
    case 'ab': return (refs[node.i] = fromHex(node.v).buffer);
    case 'ta': {
      const bytes = fromHex(node.v);
      const Ctor = globalThis[node.t] || Uint8Array;
      const out = node.t === 'Uint8Array' ? bytes : new Ctor(bytes.buffer, 0, bytes.byteLength / (Ctor.BYTES_PER_ELEMENT || 1));
      return (refs[node.i] = out);
    }
    case 'dv': return (refs[node.i] = new DataView(fromHex(node.v).buffer));
    case 'e': {
      const Ctor = ERRORS[node.n] || Error;
      const out = new Ctor(node.m);
      refs[node.i] = out;
      if (!ERRORS[node.n]) out.name = node.n;
      if (node.st !== undefined) out.stack = node.st;
      if (node.c !== undefined) out.cause = decodeValue(node.c, refs);
      return out;
    }
    case 'o': {
      const out = {};
      refs[node.i] = out;
      for (const [key, v] of node.v) out[key] = decodeValue(v, refs);
      return out;
    }
    default:
      return undefined;
  }
}

function deserialize(text) {
  return decodeValue(JSON.parse(text), []);
}

module.exports = { serialize, deserialize, dataCloneError };
