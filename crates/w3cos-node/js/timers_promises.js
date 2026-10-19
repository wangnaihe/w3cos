'use strict';
// `timers/promises`.

module.exports = {
  setTimeout: function (ms, value) {
    return new Promise(function (resolve) {
      setTimeout(function () { resolve(value); }, ms);
    });
  },
  setImmediate: function (value) {
    return new Promise(function (resolve) {
      setTimeout(function () { resolve(value); }, 0);
    });
  },
  scheduler: {
    wait: function (ms) {
      return new Promise(function (resolve) { setTimeout(resolve, ms); });
    },
    yield: function () {
      return new Promise(function (resolve) { setTimeout(resolve, 0); });
    },
  },
};
