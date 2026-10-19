'use strict';
// `async_hooks`: AsyncLocalStorage and AsyncResource for synchronous scopes.
// The AOT runtime does not propagate async context across awaits or timers
// yet: a store is visible inside `run()` until it returns.

class AsyncLocalStorage {
  constructor() { this._store = undefined; this._enabled = false; }
  getStore() { return this._enabled ? this._store : undefined; }
  enterWith(store) { this._enabled = true; this._store = store; }
  run(store, callback, ...args) {
    const previous = this._store;
    const wasEnabled = this._enabled;
    this._enabled = true;
    this._store = store;
    try { return callback(...args); } finally { this._store = previous; this._enabled = wasEnabled; }
  }
  exit(callback, ...args) {
    const previous = this._store;
    const wasEnabled = this._enabled;
    this._enabled = false;
    try { return callback(...args); } finally { this._store = previous; this._enabled = wasEnabled; }
  }
  disable() { this._enabled = false; this._store = undefined; }
  static bind(fn) { return fn; }
  static snapshot() { return function runInSnapshot(fn, ...args) { return fn(...args); }; }
}

let nextId = 2;
class AsyncResource {
  constructor(type) { this.type = type; this._id = nextId++; }
  runInAsyncScope(fn, thisArg, ...args) { return fn.apply(thisArg, args); }
  bind(fn, thisArg) { const self = this; return function () { return self.runInAsyncScope(fn, thisArg === undefined ? this : thisArg, ...arguments); }; }
  emitDestroy() { return this; }
  asyncId() { return this._id; }
  triggerAsyncId() { return 1; }
  static bind(fn, type, thisArg) { return new AsyncResource(type || 'bound-anonymous-fn').bind(fn, thisArg); }
}

module.exports = {
  AsyncLocalStorage,
  AsyncResource,
  createHook: function createHook() { return { enable() { return this; }, disable() { return this; } }; },
  executionAsyncId: function executionAsyncId() { return 1; },
  triggerAsyncId: function triggerAsyncId() { return 0; },
  executionAsyncResource: function executionAsyncResource() { return {}; },
  asyncWrapProviders: {},
};
