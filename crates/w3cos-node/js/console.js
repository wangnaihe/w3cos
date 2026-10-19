'use strict';
// `console`: the global console; `Console` writes through it.

const base = globalThis.console;

function Console() {
  if (!(this instanceof Console)) return new Console();
}
for (const name of ['log', 'info', 'warn', 'error', 'debug', 'trace', 'dir', 'table', 'time', 'timeEnd', 'timeLog', 'group', 'groupEnd', 'count', 'countReset', 'assert']) {
  Console.prototype[name] = function () {
    if (base && typeof base[name] === 'function') return base[name].apply(base, arguments);
  };
}

module.exports = base;
module.exports.Console = Console;
