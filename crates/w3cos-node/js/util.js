'use strict';
// `util`: format / inspect (common cases), inherits, promisify, callbackify,
// deprecate, isDeepStrictEqual and `util.types`.

const types = require('util/types');

const hasSymbolFor = typeof Symbol !== 'undefined' && Symbol !== null && typeof Symbol.for === 'function';
const customInspect = hasSymbolFor ? Symbol.for('nodejs.util.inspect.custom') : '__w3cos_inspect_custom';
const customPromisify = hasSymbolFor ? Symbol.for('nodejs.util.promisify.custom') : '__w3cos_promisify_custom';

function isIdentifier(key) {
  return /^[A-Za-z_$][A-Za-z0-9_$]*$/.test(key);
}

function quote(str) {
  let quoteChar = "'";
  if (str.indexOf("'") !== -1) {
    if (str.indexOf('"') === -1) quoteChar = '"';
    else if (str.indexOf('`') === -1 && str.indexOf('${') === -1) quoteChar = '`';
  }
  let out = '';
  for (let i = 0; i < str.length; i++) {
    const ch = str.charAt(i);
    if (ch === quoteChar || ch === '\\') out += '\\' + ch;
    else if (ch === '\n') out += '\\n';
    else if (ch === '\t') out += '\\t';
    else if (ch === '\r') out += '\\r';
    else if (ch === '\b') out += '\\b';
    else if (ch === '\f') out += '\\f';
    else if (ch === '\v') out += '\\v';
    else out += ch;
  }
  return quoteChar + out + quoteChar;
}

function formatNumber(n) {
  if (n === 0 && 1 / n < 0) return '-0';
  return String(n);
}

function functionLabel(fn) {
  const source = Function.prototype.toString.call(fn);
  const isClass = typeof source === 'string' && source.slice(0, 5) === 'class';
  if (isClass) return '[class ' + (fn.name || '(anonymous)') + ']';
  return fn.name ? '[Function: ' + fn.name + ']' : '[Function (anonymous)]';
}

// The prototype of `{}`: `Object.prototype`, or `null` on hosts whose plain
// objects carry no prototype (then plain and null-prototype objects look
// alike and both print as plain).
const plainProto = Object.getPrototypeOf({});

function constructorName(value) {
  const proto = Object.getPrototypeOf(value);
  if (proto === plainProto && plainProto === null) return 'Object';
  if (proto === null) return null;
  const ctor = proto.constructor;
  return typeof ctor === 'function' && ctor.name ? ctor.name : '';
}

function inspectValue(value, ctx, depth) {
  if (value === null) return 'null';
  const type = typeof value;
  if (type === 'undefined') return 'undefined';
  if (type === 'string') return depth === 0 && ctx.topString ? value : quote(value);
  if (type === 'number') return formatNumber(value);
  if (type === 'boolean') return String(value);
  if (type === 'bigint') return String(value) + 'n';
  if (type === 'symbol') return value.toString();
  if (ctx.customInspect && typeof value[customInspect] === 'function' && value[customInspect] !== inspect) {
    const result = value[customInspect](ctx.depth - depth, ctx, inspect);
    return typeof result === 'string' ? result : inspectValue(result, ctx, depth);
  }
  if (ctx.seen.indexOf(value) !== -1) return '[Circular *1]';
  if (type === 'function') {
    const label = functionLabel(value);
    const keys = Object.keys(value);
    if (keys.length === 0) return label;
    return formatProperties(value, keys, ctx, depth, label + ' {', '}', false);
  }
  if (types.isDate(value)) return isNaN(value.getTime()) ? 'Invalid Date' : value.toISOString();
  if (types.isRegExp(value)) return '/' + value.source + '/' + value.flags;
  if (value instanceof Error) {
    const stack = typeof value.stack === 'string' && value.stack ? value.stack : null;
    const head = (value.name || 'Error') + (value.message ? ': ' + value.message : '');
    return stack || '[' + head + ']';
  }
  if (types.isBoxedPrimitive(value)) {
    const inner = value.valueOf();
    const kind = typeof inner === 'string' ? 'String' : typeof inner === 'number' ? 'Number' : typeof inner === 'boolean' ? 'Boolean' : 'Object';
    return '[' + kind + ': ' + inspectValue(inner, ctx, depth + 1) + ']';
  }
  if (types.isPromise(value)) return 'Promise { <unknown> }';
  if (depth > ctx.depth) {
    if (Array.isArray(value)) return '[Array]';
    const name = constructorName(value);
    return '[' + (name || 'Object') + ']';
  }
  ctx.seen.push(value);
  let out;
  if (Array.isArray(value)) {
    out = formatArray(value, ctx, depth);
  } else if (types.isMap(value)) {
    const parts = [];
    value.forEach(function (v, k) {
      parts.push(inspectValue(k, ctx, depth + 1) + ' => ' + inspectValue(v, ctx, depth + 1));
    });
    out = wrap('Map(' + value.size + ') {', parts, '}', ctx, depth);
  } else if (types.isSet(value)) {
    const parts = [];
    value.forEach(function (v) { parts.push(inspectValue(v, ctx, depth + 1)); });
    out = wrap('Set(' + value.size + ') {', parts, '}', ctx, depth);
  } else {
    const name = constructorName(value);
    let prefix = '{';
    if (name === null) prefix = '[Object: null prototype] {';
    else if (name !== 'Object' && name !== '') prefix = name + ' {';
    out = formatProperties(value, Object.keys(value), ctx, depth, prefix, '}', true);
  }
  ctx.seen.pop();
  return out;
}

function formatKey(key) {
  if (typeof key === 'symbol') return '[' + key.toString() + ']';
  return isIdentifier(key) ? key : quote(key);
}

function formatProperties(value, keys, ctx, depth, open, close, emptyShort) {
  const parts = [];
  for (let i = 0; i < keys.length; i++) {
    const key = keys[i];
    const desc = Object.getOwnPropertyDescriptor(value, key);
    let rendered;
    if (desc && (desc.get || desc.set)) {
      rendered = desc.get && desc.set ? '[Getter/Setter]' : desc.get ? '[Getter]' : '[Setter]';
    } else {
      rendered = inspectValue(value[key], ctx, depth + 1);
    }
    parts.push(formatKey(key) + ': ' + rendered);
  }
  if (parts.length === 0 && emptyShort) return open === '{' ? '{}' : open + '}';
  return wrap(open, parts, close, ctx, depth);
}

function formatArray(value, ctx, depth) {
  const parts = [];
  let holes = 0;
  const limit = Math.min(value.length, 100);
  for (let i = 0; i < limit; i++) {
    if (!Object.prototype.hasOwnProperty.call(value, i)) {
      holes++;
      continue;
    }
    if (holes > 0) {
      parts.push('<' + holes + ' empty item' + (holes > 1 ? 's' : '') + '>');
      holes = 0;
    }
    parts.push(inspectValue(value[i], ctx, depth + 1));
  }
  if (holes > 0) parts.push('<' + holes + ' empty item' + (holes > 1 ? 's' : '') + '>');
  if (value.length > limit) parts.push('... ' + (value.length - limit) + ' more item' + (value.length - limit > 1 ? 's' : ''));
  const keys = Object.keys(value);
  for (let i = 0; i < keys.length; i++) {
    if (String(Number(keys[i])) === keys[i] && Number(keys[i]) < value.length) continue;
    parts.push(formatKey(keys[i]) + ': ' + inspectValue(value[keys[i]], ctx, depth + 1));
  }
  if (parts.length === 0) return '[]';
  return wrap('[', parts, ']', ctx, depth);
}

function wrap(open, parts, close, ctx, depth) {
  if (parts.length === 0) return open + close;
  let total = open.length + close.length;
  let multiline = false;
  for (let i = 0; i < parts.length; i++) {
    total += parts[i].length + 2;
    if (parts[i].indexOf('\n') !== -1) multiline = true;
  }
  if (!multiline && total + depth * 2 <= ctx.breakLength) {
    return open + ' ' + parts.join(', ') + ' ' + close;
  }
  const indent = '  '.repeat(depth + 1);
  return open + '\n' + indent + parts.map(function (part) {
    return part.split('\n').join('\n' + indent);
  }).join(',\n' + indent) + '\n' + '  '.repeat(depth) + close;
}

function inspect(value, options) {
  const ctx = { depth: 2, breakLength: 80, customInspect: true, seen: [], topString: false };
  if (typeof options === 'object' && options !== null) {
    if (options.depth === null) ctx.depth = Infinity;
    else if (typeof options.depth === 'number') ctx.depth = options.depth;
    if (typeof options.breakLength === 'number') ctx.breakLength = options.breakLength;
    if (options.customInspect === false) ctx.customInspect = false;
  }
  return inspectValue(value, ctx, 0);
}
inspect.custom = customInspect;
inspect.defaultOptions = { depth: 2, breakLength: 80, customInspect: true };

function format(first) {
  const args = arguments;
  let out = '';
  let argIndex = 1;
  if (typeof first === 'string') {
    let last = 0;
    for (let i = 0; i < first.length - 1; i++) {
      if (first.charCodeAt(i) !== 37) continue;
      const next = first.charAt(i + 1);
      let replacement;
      if (next === '%') {
        out += first.slice(last, i) + '%';
        last = i + 2;
        i++;
        continue;
      }
      if (argIndex >= args.length) continue;
      const arg = args[argIndex];
      if (next === 's') {
        replacement = typeof arg === 'string' ? arg
          : typeof arg === 'bigint' ? String(arg) + 'n'
          : typeof arg === 'object' && arg !== null ? inspect(arg, { depth: 0 })
          : typeof arg === 'number' ? formatNumber(arg) : String(arg);
      } else if (next === 'd' || next === 'i') {
        if (typeof arg === 'bigint') replacement = String(arg) + 'n';
        else if (typeof arg === 'symbol') replacement = 'NaN';
        else {
          const n = Number(arg);
          replacement = formatNumber(next === 'i' ? (n < 0 ? Math.ceil(n) : Math.floor(n)) : n);
        }
      } else if (next === 'f') {
        replacement = typeof arg === 'symbol' ? 'NaN' : formatNumber(parseFloat(arg));
      } else if (next === 'j') {
        try {
          replacement = JSON.stringify(arg);
        } catch (error) {
          replacement = '[Circular]';
        }
      } else if (next === 'o') {
        replacement = inspect(arg, { depth: 4 });
      } else if (next === 'O') {
        replacement = inspect(arg);
      } else if (next === 'c') {
        replacement = '';
      } else {
        continue;
      }
      out += first.slice(last, i) + replacement;
      last = i + 2;
      i++;
      argIndex++;
    }
    out += first.slice(last);
  } else {
    argIndex = 0;
  }
  for (; argIndex < args.length; argIndex++) {
    const arg = args[argIndex];
    if (out.length > 0 || argIndex > 0) out += ' ';
    out += typeof arg === 'string' ? arg : inspect(arg);
  }
  return out;
}

function inherits(ctor, superCtor) {
  if (typeof ctor !== 'function' || typeof superCtor !== 'function' || superCtor.prototype === undefined) {
    const error = new TypeError('The "ctor" and "superCtor" arguments must be functions with a prototype');
    error.code = 'ERR_INVALID_ARG_TYPE';
    throw error;
  }
  ctor.super_ = superCtor;
  Object.setPrototypeOf(ctor.prototype, superCtor.prototype);
}

function promisify(original) {
  if (typeof original !== 'function') {
    const error = new TypeError('The "original" argument must be of type function');
    error.code = 'ERR_INVALID_ARG_TYPE';
    throw error;
  }
  if (original[customPromisify]) {
    const fn = original[customPromisify];
    return fn;
  }
  function fn() {
    const self = this;
    const args = [];
    for (let i = 0; i < arguments.length; i++) args.push(arguments[i]);
    return new Promise(function (resolve, reject) {
      args.push(function (err, value) {
        if (err) reject(err);
        else resolve(value);
      });
      original.apply(self, args);
    });
  }
  Object.setPrototypeOf(fn, Object.getPrototypeOf(original));
  Object.defineProperty(fn, customPromisify, { value: fn, configurable: true });
  return fn;
}
promisify.custom = customPromisify;

function callbackify(original) {
  if (typeof original !== 'function') {
    const error = new TypeError('The "original" argument must be of type function');
    error.code = 'ERR_INVALID_ARG_TYPE';
    throw error;
  }
  return function callbackified() {
    const args = [];
    for (let i = 0; i < arguments.length; i++) args.push(arguments[i]);
    const callback = args.pop();
    const self = this;
    original.apply(this, args).then(function (value) {
      nextTick(function () { callback.call(self, null, value); });
    }, function (reason) {
      if (!reason) {
        const error = new Error('Promise was rejected with a falsy value');
        error.code = 'ERR_FALSY_VALUE_REJECTION';
        error.reason = reason;
        reason = error;
      }
      nextTick(function () { callback.call(self, reason); });
    });
  };
}

function nextTick(fn) {
  if (typeof process === 'object' && process && typeof process.nextTick === 'function') process.nextTick(fn);
  else Promise.resolve().then(fn);
}

const warned = {};
function deprecate(fn, msg, code) {
  let didWarn = false;
  return function deprecated() {
    if (!didWarn && !(code && warned[code])) {
      didWarn = true;
      if (code) warned[code] = true;
      if (typeof process === 'object' && process && typeof process.emitWarning === 'function') {
        process.emitWarning(msg, 'DeprecationWarning', code);
      }
    }
    return fn.apply(this, arguments);
  };
}

function isDeepStrictEqual(a, b) {
  return deepEqual(a, b, []);
}

function deepEqual(a, b, seen) {
  if (Object.is(a, b)) return true;
  if (typeof a !== 'object' || typeof b !== 'object' || a === null || b === null) return false;
  if (Object.getPrototypeOf(a) !== Object.getPrototypeOf(b)) return false;
  for (let i = 0; i < seen.length; i++) {
    if (seen[i][0] === a && seen[i][1] === b) return true;
  }
  seen = seen.concat([[a, b]]);
  if (types.isDate(a)) return a.getTime() === b.getTime();
  if (types.isRegExp(a)) return String(a) === String(b);
  if (types.isMap(a)) {
    if (a.size !== b.size) return false;
    let ok = true;
    a.forEach(function (v, k) {
      if (ok && (!b.has(k) || !deepEqual(v, b.get(k), seen))) ok = false;
    });
    return ok;
  }
  if (types.isSet(a)) {
    if (a.size !== b.size) return false;
    let ok = true;
    a.forEach(function (v) { if (ok && !b.has(v)) ok = false; });
    return ok;
  }
  if (Array.isArray(a) && a.length !== b.length) return false;
  const ka = Object.keys(a);
  const kb = Object.keys(b);
  if (ka.length !== kb.length) return false;
  for (let i = 0; i < ka.length; i++) {
    if (!Object.prototype.hasOwnProperty.call(b, ka[i])) return false;
    if (!deepEqual(a[ka[i]], b[ka[i]], seen)) return false;
  }
  return true;
}

function legacyIs(check) {
  return function (value) { return check(value); };
}

module.exports = {
  format: format,
  formatWithOptions: function formatWithOptions(options) {
    const args = [];
    for (let i = 1; i < arguments.length; i++) args.push(arguments[i]);
    return format.apply(null, args);
  },
  inspect: inspect,
  inherits: inherits,
  promisify: promisify,
  callbackify: callbackify,
  deprecate: deprecate,
  isDeepStrictEqual: isDeepStrictEqual,
  types: types,
  isArray: Array.isArray,
  isBoolean: legacyIs(function (v) { return typeof v === 'boolean'; }),
  isNull: legacyIs(function (v) { return v === null; }),
  isNullOrUndefined: legacyIs(function (v) { return v === null || v === undefined; }),
  isNumber: legacyIs(function (v) { return typeof v === 'number'; }),
  isString: legacyIs(function (v) { return typeof v === 'string'; }),
  isSymbol: legacyIs(function (v) { return typeof v === 'symbol'; }),
  isUndefined: legacyIs(function (v) { return v === undefined; }),
  isRegExp: types.isRegExp,
  isObject: legacyIs(function (v) { return v !== null && typeof v === 'object'; }),
  isDate: types.isDate,
  isError: legacyIs(function (v) { return v instanceof Error; }),
  isFunction: legacyIs(function (v) { return typeof v === 'function'; }),
  isPrimitive: legacyIs(function (v) { return v === null || (typeof v !== 'object' && typeof v !== 'function'); }),
  TextEncoder: typeof TextEncoder === 'function' ? TextEncoder : undefined,
  TextDecoder: typeof TextDecoder === 'function' ? TextDecoder : undefined,
  stripVTControlCharacters: function (str) {
    return String(str).replace(/[\u001B\u009B][[\]()#;?]*(?:(?:(?:(?:;[-a-zA-Z\d\/#&.:=?%@~_]+)*|[a-zA-Z\d]+(?:;[-a-zA-Z\d\/#&.:=?%@~_]*)*)?\u0007)|(?:(?:\d{1,4}(?:;\d{0,4})*)?[\dA-PR-TZcf-ntqry=><~]))/g, '');
  },
  toUSVString: function (str) { return String(str); },
};
