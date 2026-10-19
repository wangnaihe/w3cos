'use strict';
// `diagnostics_channel`: named publish/subscribe channels.

const channels = new Map();

class Channel {
  constructor(name) { this.name = name; this._subscribers = []; }
  get hasSubscribers() { return this._subscribers.length > 0; }
  subscribe(fn) { this._subscribers.push(fn); }
  unsubscribe(fn) {
    const index = this._subscribers.indexOf(fn);
    if (index === -1) return false;
    this._subscribers.splice(index, 1);
    return true;
  }
  publish(message) {
    for (const fn of this._subscribers.slice()) {
      try { fn(message, this.name); } catch (error) { setTimeout(() => { throw error; }, 0); }
    }
  }
  bindStore() {}
  unbindStore() { return false; }
  runStores(data, fn, thisArg, ...args) { this.publish(data); return fn.apply(thisArg, args); }
}

function channel(name) {
  let found = channels.get(name);
  if (!found) { found = new Channel(name); channels.set(name, found); }
  return found;
}

function tracingChannel(nameOrChannels) {
  const make = (event) => typeof nameOrChannels === 'string' ? channel('tracing:' + nameOrChannels + ':' + event) : nameOrChannels[event];
  const tc = {
    start: make('start'), end: make('end'), asyncStart: make('asyncStart'), asyncEnd: make('asyncEnd'), error: make('error'),
  };
  Object.defineProperty(tc, 'hasSubscribers', { get() { return [tc.start, tc.end, tc.asyncStart, tc.asyncEnd, tc.error].some((c) => c.hasSubscribers); } });
  tc.subscribe = function (handlers) { for (const key in handlers) if (tc[key]) tc[key].subscribe(handlers[key]); };
  tc.unsubscribe = function (handlers) { let ok = true; for (const key in handlers) if (tc[key]) ok = tc[key].unsubscribe(handlers[key]) && ok; return ok; };
  tc.traceSync = function (fn, context, thisArg, ...args) {
    context = context || {};
    tc.start.publish(context);
    try { const result = fn.apply(thisArg, args); context.result = result; return result; }
    catch (error) { context.error = error; tc.error.publish(context); throw error; }
    finally { tc.end.publish(context); }
  };
  tc.tracePromise = function (fn, context, thisArg, ...args) {
    context = context || {};
    tc.start.publish(context);
    try {
      return Promise.resolve(fn.apply(thisArg, args)).then(
        (result) => { context.result = result; tc.asyncStart.publish(context); tc.asyncEnd.publish(context); return result; },
        (error) => { context.error = error; tc.error.publish(context); tc.asyncStart.publish(context); tc.asyncEnd.publish(context); throw error; });
    } finally { tc.end.publish(context); }
  };
  tc.traceCallback = function (fn, position, context, thisArg, ...args) { return tc.traceSync(fn, context, thisArg, ...args); };
  return tc;
}

module.exports = {
  channel,
  hasSubscribers: function hasSubscribers(name) { const found = channels.get(name); return !!found && found.hasSubscribers; },
  subscribe: function subscribe(name, fn) { channel(name).subscribe(fn); },
  unsubscribe: function unsubscribe(name, fn) { return channel(name).unsubscribe(fn); },
  tracingChannel,
  Channel,
};
