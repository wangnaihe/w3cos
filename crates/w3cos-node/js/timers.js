'use strict';
// `timers`: the runtime's timer globals (Node's event loop phases map onto
// the W3C OS loop: timers, then immediates after I/O).

function immediate(fn, args) {
  return setTimeout(function () { fn.apply(undefined, args); }, 0);
}

module.exports = {
  setTimeout: function (fn, ms) {
    const args = [];
    for (let i = 2; i < arguments.length; i++) args.push(arguments[i]);
    return setTimeout(function () { fn.apply(undefined, args); }, ms);
  },
  clearTimeout: function (id) { return clearTimeout(id); },
  setInterval: function (fn, ms) {
    const args = [];
    for (let i = 2; i < arguments.length; i++) args.push(arguments[i]);
    return setInterval(function () { fn.apply(undefined, args); }, ms);
  },
  clearInterval: function (id) { return clearInterval(id); },
  setImmediate: function (fn) {
    const args = [];
    for (let i = 1; i < arguments.length; i++) args.push(arguments[i]);
    return immediate(fn, args);
  },
  clearImmediate: function (id) { return clearTimeout(id); },
  promises: undefined,
};

module.exports.promises = require('timers/promises');
