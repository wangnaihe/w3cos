'use strict';
// `util/types`: brand checks via Object.prototype.toString.

const objectToString = Object.prototype && Object.prototype.toString;

function tag(value) {
  return typeof objectToString === 'function' ? objectToString.call(value) : '';
}

// Hosts without brand tags or realm prototypes (the compact w3cos runtime)
// fall back to the shape of the built-in objects.
function looksLikeDate(value) {
  return isObjectLike(value) && typeof value.getTime === 'function' &&
    typeof value.toISOString === 'function';
}

function looksLikePromise(value) {
  return isObjectLike(value) && typeof value.then === 'function' &&
    typeof value.catch === 'function' && typeof value.finally === 'function';
}

function isObjectLike(value) {
  return value !== null && (typeof value === 'object' || typeof value === 'function');
}

function check(name) {
  return function (value) {
    return isObjectLike(value) && tag(value) === '[object ' + name + ']';
  };
}

module.exports = {
  isDate: function (value) { return value instanceof Date || check('Date')(value) || looksLikeDate(value); },
  isRegExp: function (value) { return value instanceof RegExp || check('RegExp')(value); },
  isMap: function (value) { return (typeof Map === 'function' && value instanceof Map) || check('Map')(value); },
  isSet: function (value) { return (typeof Set === 'function' && value instanceof Set) || check('Set')(value); },
  isWeakMap: function (value) { return (typeof WeakMap === 'function' && value instanceof WeakMap) || check('WeakMap')(value); },
  isWeakSet: function (value) { return (typeof WeakSet === 'function' && value instanceof WeakSet) || check('WeakSet')(value); },
  isPromise: function (value) { return (typeof Promise === 'function' && value instanceof Promise) || check('Promise')(value) || looksLikePromise(value); },
  isNativeError: function (value) { return value instanceof Error; },
  isArrayBuffer: function (value) { return (typeof ArrayBuffer === 'function' && value instanceof ArrayBuffer) || check('ArrayBuffer')(value); },
  isAnyArrayBuffer: function (value) { return (typeof ArrayBuffer === 'function' && value instanceof ArrayBuffer) || check('ArrayBuffer')(value) || check('SharedArrayBuffer')(value); },
  isTypedArray: function (value) { return typeof ArrayBuffer === 'function' && ArrayBuffer.isView(value) && !check('DataView')(value); },
  isUint8Array: check('Uint8Array'),
  isDataView: check('DataView'),
  isArrayBufferView: function (value) { return typeof ArrayBuffer === 'function' && ArrayBuffer.isView(value); },
  isAsyncFunction: check('AsyncFunction'),
  isGeneratorFunction: function (value) { return check('GeneratorFunction')(value) || check('AsyncGeneratorFunction')(value); },
  isGeneratorObject: check('Generator'),
  isBoxedPrimitive: function (value) {
    return value instanceof Number || value instanceof String || value instanceof Boolean ||
      check('Number')(value) || check('String')(value) || check('Boolean')(value);
  },
  isNumberObject: function (value) { return value instanceof Number || check('Number')(value); },
  isStringObject: function (value) { return value instanceof String || check('String')(value); },
  isBooleanObject: function (value) { return value instanceof Boolean || check('Boolean')(value); },
  isSymbolObject: function (value) { return typeof value === 'object' && check('Symbol')(value); },
  isProxy: function () { return false; },
};
