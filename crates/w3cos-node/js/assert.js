'use strict';
// node:assert (legacy + strict modes). Messages follow node for the simple
// one-line cases; deep-equality failures carry a compact message instead of
// node's diff.
const util = require('util');

function AssertionError(options) {
  options = options || {};
  const message = options.message !== undefined ? String(options.message) : defaultMessage(options);
  const err = new Error(message);
  Object.setPrototypeOf(err, AssertionError.prototype);
  err.generatedMessage = options.message === undefined;
  err.code = 'ERR_ASSERTION';
  err.actual = options.actual;
  err.expected = options.expected;
  err.operator = options.operator;
  return err;
}
AssertionError.prototype = Object.create(Error.prototype);
Object.defineProperty(AssertionError.prototype, 'constructor', {
  value: AssertionError, writable: true, configurable: true, enumerable: false,
});
Object.defineProperty(AssertionError.prototype, 'name', {
  value: 'AssertionError', writable: true, configurable: true, enumerable: false,
});

const HEADERS = {
  strictEqual: 'Expected values to be strictly equal:',
  deepStrictEqual: 'Expected values to be strictly deep-equal:',
  deepEqual: 'Expected values to be loosely deep-equal:',
  notStrictEqual: 'Expected "actual" to be strictly unequal to:',
  notDeepStrictEqual: 'Expected "actual" not to be strictly deep-equal to:',
};

function defaultMessage(o) {
  const a = util.inspect(o.actual);
  const e = util.inspect(o.expected);
  switch (o.operator) {
    case 'strictEqual':
      return HEADERS.strictEqual + '\n\n' + a + ' !== ' + e + '\n';
    case 'deepStrictEqual':
    case 'deepEqual':
      return HEADERS[o.operator] + '\n\n' + a + '\n\nshould equal\n\n' + e;
    case 'notStrictEqual':
    case 'notDeepStrictEqual':
      return HEADERS[o.operator] + ' ' + e;
    case 'fail':
      return 'Failed';
    default:
      return a + ' ' + o.operator + ' ' + e;
  }
}

function fail(actual, expected, message, operator) {
  if (arguments.length === 0) message = 'Failed';
  if (arguments.length === 1) { message = actual; actual = undefined; }
  if (message instanceof Error) throw message;
  throw new AssertionError({ actual, expected, message, operator: operator || 'fail' });
}

function innerFail(obj) {
  if (obj.message instanceof Error) throw obj.message;
  throw new AssertionError(obj);
}

function innerOk(argLen, value, message) {
  if (!value) {
    let generated = false;
    if (argLen === 0) {
      message = 'No value argument passed to `assert.ok()`';
    } else if (message === undefined) {
      generated = true;
      message = 'The expression evaluated to a falsy value:\n\n  assert.ok(' + util.inspect(value) + ')\n';
    }
    const err = new AssertionError({ actual: value, expected: true, message, operator: '==' });
    err.generatedMessage = generated;
    throw err;
  }
}

function ok(...args) { innerOk(args.length, ...args); }

function looseDeepEqual(a, b, seen) {
  if (a == b) return true; // eslint-disable-line eqeqeq
  if (typeof a !== 'object' || typeof b !== 'object' || a === null || b === null) {
    return a !== a && b !== b; // NaN
  }
  for (let i = 0; i < seen.length; i++) {
    if (seen[i][0] === a && seen[i][1] === b) return true;
  }
  seen = seen.concat([[a, b]]);
  if (util.types.isDate(a) && util.types.isDate(b)) return a.getTime() === b.getTime();
  if (util.types.isRegExp(a) && util.types.isRegExp(b)) return String(a) === String(b);
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  const ka = Object.keys(a);
  const kb = Object.keys(b);
  if (ka.length !== kb.length) return false;
  for (let i = 0; i < ka.length; i++) {
    if (!Object.prototype.hasOwnProperty.call(b, ka[i])) return false;
    if (!looseDeepEqual(a[ka[i]], b[ka[i]], seen)) return false;
  }
  return true;
}

function equal(actual, expected, message) {
  // eslint-disable-next-line eqeqeq
  if (!(actual == expected || (actual !== actual && expected !== expected))) {
    innerFail({ actual, expected, message, operator: '==' });
  }
}
function notEqual(actual, expected, message) {
  // eslint-disable-next-line eqeqeq
  if (actual == expected || (actual !== actual && expected !== expected)) {
    innerFail({ actual, expected, message, operator: '!=' });
  }
}
function strictEqual(actual, expected, message) {
  if (!Object.is(actual, expected)) innerFail({ actual, expected, message, operator: 'strictEqual' });
}
function notStrictEqual(actual, expected, message) {
  if (Object.is(actual, expected)) innerFail({ actual, expected, message, operator: 'notStrictEqual' });
}
function deepEqual(actual, expected, message) {
  if (!looseDeepEqual(actual, expected, [])) innerFail({ actual, expected, message, operator: 'deepEqual' });
}
function notDeepEqual(actual, expected, message) {
  if (looseDeepEqual(actual, expected, [])) innerFail({ actual, expected, message, operator: 'notDeepEqual' });
}
function deepStrictEqual(actual, expected, message) {
  if (!util.isDeepStrictEqual(actual, expected)) {
    innerFail({ actual, expected, message, operator: 'deepStrictEqual' });
  }
}
function notDeepStrictEqual(actual, expected, message) {
  if (util.isDeepStrictEqual(actual, expected)) {
    innerFail({ actual, expected, message, operator: 'notDeepStrictEqual' });
  }
}

function matchesExpected(err, expected, message, fnName) {
  if (expected === undefined) return;
  if (typeof expected === 'function') {
    if (expected.prototype !== undefined && err instanceof expected) return;
    if (Error.isPrototypeOf && Error.isPrototypeOf(expected)) throw err;
    if (expected === Error || (expected.prototype instanceof Error)) throw err;
    if (expected.call({}, err) === true) return;
    innerFail({
      actual: err, expected, operator: fnName,
      message: message || 'The ' + (expected.name ? '"' + expected.name + '" ' : '') +
        'validation function is expected to return "true". Received ' + util.inspect(expected.call({}, err)),
    });
  }
  if (expected instanceof RegExp) {
    if (expected.test(String(err))) return;
    innerFail({
      actual: err, expected, operator: fnName,
      message: message || 'The input did not match the regular expression ' + util.inspect(expected) +
        '. Input:\n\n' + util.inspect(String(err)) + '\n',
    });
  }
  if (typeof expected === 'object' && expected !== null) {
    const keys = Object.keys(expected);
    if (expected instanceof Error) keys.push('name', 'message');
    for (let i = 0; i < keys.length; i++) {
      const k = keys[i];
      const want = expected[k];
      const got = err == null ? undefined : err[k];
      if (want instanceof RegExp && typeof got === 'string') {
        if (!want.test(got)) innerFail({ actual: err, expected, operator: fnName, message: message || 'The input did not match the regular expression ' + util.inspect(want) + '. Input:\n\n' + util.inspect(got) + '\n' });
      } else if (!util.isDeepStrictEqual(got, want)) {
        innerFail({ actual: err, expected, operator: fnName, message: message || 'Expected values to be strictly deep-equal:\n\n' + util.inspect(got) + '\n\nshould equal\n\n' + util.inspect(want) });
      }
    }
  }
}

function throws(fn, expected, message) {
  if (typeof expected === 'string') { message = expected; expected = undefined; }
  let threw = false;
  let err;
  try { fn(); } catch (e) { threw = true; err = e; }
  if (!threw) {
    let details = '';
    if (expected && expected.name) details += ' (' + expected.name + ')';
    details += message ? ': ' + message : '.';
    innerFail({ actual: undefined, expected, operator: 'throws', message: 'Missing expected exception' + details });
  }
  matchesExpected(err, expected, message, 'throws');
}

function doesNotThrow(fn, expected, message) {
  if (typeof expected === 'string') { message = expected; expected = undefined; }
  try { fn(); } catch (e) {
    const details = message ? ': ' + message : '.';
    innerFail({ actual: e, expected, operator: 'doesNotThrow', message: 'Got unwanted exception' + details + '\nActual message: "' + (e && e.message) + '"' });
  }
}

async function rejects(promiseFn, expected, message) {
  if (typeof expected === 'string') { message = expected; expected = undefined; }
  const p = typeof promiseFn === 'function' ? promiseFn() : promiseFn;
  let threw = false;
  let err;
  try { await p; } catch (e) { threw = true; err = e; }
  if (!threw) {
    let details = '';
    if (expected && expected.name) details += ' (' + expected.name + ')';
    details += message ? ': ' + message : '.';
    innerFail({ actual: undefined, expected, operator: 'rejects', message: 'Missing expected rejection' + details });
  }
  matchesExpected(err, expected, message, 'rejects');
}

async function doesNotReject(promiseFn, expected, message) {
  if (typeof expected === 'string') { message = expected; expected = undefined; }
  const p = typeof promiseFn === 'function' ? promiseFn() : promiseFn;
  try { await p; } catch (e) {
    const details = message ? ': ' + message : '.';
    innerFail({ actual: e, expected, operator: 'doesNotReject', message: 'Got unwanted rejection' + details + '\nActual message: "' + (e && e.message) + '"' });
  }
}

function ifError(err) {
  if (err !== null && err !== undefined) {
    let message = 'ifError got unwanted exception: ';
    message += (typeof err === 'object' && typeof err.message === 'string') ? err.message : util.inspect(err);
    innerFail({ actual: err, expected: null, operator: 'ifError', message });
  }
}

function match(string, regexp, message) {
  if (typeof string !== 'string' || !regexp.test(string)) {
    innerFail({
      actual: string, expected: regexp, operator: 'match',
      message: message || (typeof string !== 'string'
        ? 'The "string" argument must be of type string. Received type ' + typeof string + ' (' + util.inspect(string) + ')'
        : 'The input did not match the regular expression ' + util.inspect(regexp) + '. Input:\n\n' + util.inspect(string) + '\n'),
    });
  }
}

function doesNotMatch(string, regexp, message) {
  if (typeof string !== 'string' || regexp.test(string)) {
    innerFail({
      actual: string, expected: regexp, operator: 'doesNotMatch',
      message: message || 'The input was expected to not match the regular expression ' + util.inspect(regexp) + '. Input:\n\n' + util.inspect(string) + '\n',
    });
  }
}

function assert(...args) { innerOk(args.length, ...args); }
assert.ok = ok;
assert.fail = fail;
assert.AssertionError = AssertionError;
assert.equal = equal;
assert.notEqual = notEqual;
assert.strictEqual = strictEqual;
assert.notStrictEqual = notStrictEqual;
assert.deepEqual = deepEqual;
assert.notDeepEqual = notDeepEqual;
assert.deepStrictEqual = deepStrictEqual;
assert.notDeepStrictEqual = notDeepStrictEqual;
assert.throws = throws;
assert.doesNotThrow = doesNotThrow;
assert.rejects = rejects;
assert.doesNotReject = doesNotReject;
assert.ifError = ifError;
assert.match = match;
assert.doesNotMatch = doesNotMatch;

function strict(...args) { innerOk(args.length, ...args); }
Object.keys(assert).forEach(function (k) { strict[k] = assert[k]; });
strict.equal = strictEqual;
strict.notEqual = notStrictEqual;
strict.deepEqual = deepStrictEqual;
strict.notDeepEqual = notDeepStrictEqual;
strict.strict = strict;
assert.strict = strict;

module.exports = assert;
