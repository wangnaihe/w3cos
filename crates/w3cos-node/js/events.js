'use strict';
// `events`: EventEmitter with Node's ordering and error semantics.

function EventEmitter(options) {
  EventEmitter.init.call(this, options);
}

EventEmitter.prototype._events = undefined;
EventEmitter.prototype._eventsCount = 0;
EventEmitter.prototype._maxListeners = undefined;

let defaultMaxListeners = 10;

Object.defineProperty(EventEmitter, 'defaultMaxListeners', {
  enumerable: true,
  get: function () { return defaultMaxListeners; },
  set: function (value) {
    if (typeof value !== 'number' || value < 0 || value !== value) {
      throw rangeError('defaultMaxListeners', value);
    }
    defaultMaxListeners = value;
  },
});

// Hosts without accessor properties on functions keep a plain data property.
if (EventEmitter.defaultMaxListeners !== defaultMaxListeners) {
  delete EventEmitter.defaultMaxListeners;
  EventEmitter.defaultMaxListeners = defaultMaxListeners;
}

function currentDefaultMaxListeners() {
  const value = EventEmitter.defaultMaxListeners;
  return typeof value === 'number' ? value : defaultMaxListeners;
}

EventEmitter.errorMonitor = typeof Symbol === 'function' ? Symbol('events.errorMonitor') : '__w3cos_errorMonitor';

EventEmitter.init = function init() {
  if (this._events === undefined || this._events === Object.getPrototypeOf(this)._events) {
    this._events = Object.create(null);
    this._eventsCount = 0;
  }
  this._maxListeners = this._maxListeners || undefined;
};

function rangeError(name, value) {
  const error = new RangeError('The value of "' + name + '" is out of range. It must be a non-negative number. Received ' + String(value));
  error.code = 'ERR_OUT_OF_RANGE';
  return error;
}

function checkListener(listener) {
  if (typeof listener !== 'function') {
    const error = new TypeError('The "listener" argument must be of type function. Received ' + (listener === null ? 'null' : typeof listener === 'object' ? 'an instance of Object' : 'type ' + typeof listener));
    error.code = 'ERR_INVALID_ARG_TYPE';
    throw error;
  }
}

EventEmitter.prototype.setMaxListeners = function setMaxListeners(n) {
  if (typeof n !== 'number' || n < 0 || n !== n) throw rangeError('n', n);
  this._maxListeners = n;
  return this;
};

function maxListeners(that) {
  return that._maxListeners === undefined ? currentDefaultMaxListeners() : that._maxListeners;
}

EventEmitter.prototype.getMaxListeners = function getMaxListeners() {
  return maxListeners(this);
};

EventEmitter.prototype.emit = function emit(type) {
  const args = [];
  for (let i = 1; i < arguments.length; i++) args.push(arguments[i]);
  const events = this._events;
  if (type === 'error' && events !== undefined && events[EventEmitter.errorMonitor] !== undefined) {
    this.emit.apply(this, [EventEmitter.errorMonitor].concat(args));
  }
  const handler = events === undefined ? undefined : events[type];
  if (handler === undefined) {
    if (type === 'error') {
      const er = args[0];
      if (er instanceof Error) throw er;
      let shown;
      try {
        shown = require('util').inspect(er);
      } catch (inspectError) {
        shown = String(er);
      }
      const error = new Error('Unhandled error. (' + shown + ')');
      error.code = 'ERR_UNHANDLED_ERROR';
      error.context = er;
      throw error;
    }
    return false;
  }
  if (typeof handler === 'function') {
    handler.apply(this, args);
  } else {
    const listeners = handler.slice();
    for (let i = 0; i < listeners.length; ++i) listeners[i].apply(this, args);
  }
  return true;
};

function addListener(target, type, listener, prepend) {
  checkListener(listener);
  let events = target._events;
  if (events === undefined) {
    events = target._events = Object.create(null);
    target._eventsCount = 0;
  } else if (events.newListener !== undefined) {
    target.emit('newListener', type, listener.listener ? listener.listener : listener);
    events = target._events;
  }
  let existing = events[type];
  if (existing === undefined) {
    events[type] = listener;
    ++target._eventsCount;
  } else {
    if (typeof existing === 'function') {
      existing = events[type] = prepend ? [listener, existing] : [existing, listener];
    } else if (prepend) {
      existing.unshift(listener);
    } else {
      existing.push(listener);
    }
    const m = maxListeners(target);
    if (m > 0 && existing.length > m && !existing.warned) {
      existing.warned = true;
      const message = 'Possible EventEmitter memory leak detected. ' + existing.length + ' ' + String(type) +
        ' listeners added to ' + (target.constructor && target.constructor.name ? '[' + target.constructor.name + ']' : 'EventEmitter') +
        '. MaxListeners is ' + m + '. Use emitter.setMaxListeners() to increase limit';
      if (typeof process === 'object' && process && typeof process.emitWarning === 'function') {
        process.emitWarning(message, 'MaxListenersExceededWarning');
      } else if (typeof console === 'object') {
        console.warn('(node) ' + message);
      }
    }
  }
  return target;
}

EventEmitter.prototype.addListener = function addListener_(type, listener) {
  return addListener(this, type, listener, false);
};
EventEmitter.prototype.on = EventEmitter.prototype.addListener;

EventEmitter.prototype.prependListener = function prependListener(type, listener) {
  return addListener(this, type, listener, true);
};

function onceWrapper(state) {
  return function wrapped() {
    if (!state.fired) {
      state.target.removeListener(state.type, state.wrapFn);
      state.fired = true;
      return state.listener.apply(state.target, arguments);
    }
  };
}

function onceWrap(target, type, listener) {
  const state = { fired: false, wrapFn: undefined, target: target, type: type, listener: listener };
  const wrapped = onceWrapper(state);
  wrapped.listener = listener;
  state.wrapFn = wrapped;
  return wrapped;
}

EventEmitter.prototype.once = function once(type, listener) {
  checkListener(listener);
  this.on(type, onceWrap(this, type, listener));
  return this;
};

EventEmitter.prototype.prependOnceListener = function prependOnceListener(type, listener) {
  checkListener(listener);
  this.prependListener(type, onceWrap(this, type, listener));
  return this;
};

EventEmitter.prototype.removeListener = function removeListener(type, listener) {
  checkListener(listener);
  const events = this._events;
  if (events === undefined) return this;
  const list = events[type];
  if (list === undefined) return this;
  if (list === listener || list.listener === listener) {
    if (--this._eventsCount === 0) this._events = Object.create(null);
    else delete events[type];
    if (events.removeListener) this.emit('removeListener', type, list.listener || listener);
  } else if (typeof list !== 'function') {
    let position = -1;
    for (let i = list.length - 1; i >= 0; i--) {
      if (list[i] === listener || list[i].listener === listener) {
        position = i;
        break;
      }
    }
    if (position < 0) return this;
    const removed = list[position];
    list.splice(position, 1);
    if (list.length === 1) events[type] = list[0];
    if (events.removeListener !== undefined) this.emit('removeListener', type, removed.listener || listener);
  }
  return this;
};
EventEmitter.prototype.off = EventEmitter.prototype.removeListener;

EventEmitter.prototype.removeAllListeners = function removeAllListeners(type) {
  const events = this._events;
  if (events === undefined) return this;
  if (events.removeListener === undefined) {
    if (arguments.length === 0) {
      this._events = Object.create(null);
      this._eventsCount = 0;
    } else if (events[type] !== undefined) {
      if (--this._eventsCount === 0) this._events = Object.create(null);
      else delete events[type];
    }
    return this;
  }
  if (arguments.length === 0) {
    const keys = Object.keys(events);
    for (let i = 0; i < keys.length; ++i) {
      if (keys[i] === 'removeListener') continue;
      this.removeAllListeners(keys[i]);
    }
    this.removeAllListeners('removeListener');
    this._events = Object.create(null);
    this._eventsCount = 0;
    return this;
  }
  const listeners = events[type];
  if (typeof listeners === 'function') {
    this.removeListener(type, listeners);
  } else if (listeners !== undefined) {
    for (let i = listeners.length - 1; i >= 0; i--) this.removeListener(type, listeners[i]);
  }
  return this;
};

function listeners(target, type, unwrap) {
  const events = target._events;
  if (events === undefined) return [];
  const listener = events[type];
  if (listener === undefined) return [];
  if (typeof listener === 'function') return unwrap ? [listener.listener || listener] : [listener];
  const out = [];
  for (let i = 0; i < listener.length; i++) out.push(unwrap ? listener[i].listener || listener[i] : listener[i]);
  return out;
}

EventEmitter.prototype.listeners = function listeners_(type) {
  return listeners(this, type, true);
};

EventEmitter.prototype.rawListeners = function rawListeners(type) {
  return listeners(this, type, false);
};

function listenerCount(type, listener) {
  const events = this._events;
  if (events === undefined) return 0;
  const value = events[type];
  if (value === undefined) return 0;
  if (typeof value === 'function') {
    return listener === undefined || value === listener || value.listener === listener ? 1 : 0;
  }
  if (listener === undefined) return value.length;
  let count = 0;
  for (let i = 0; i < value.length; i++) {
    if (value[i] === listener || value[i].listener === listener) count++;
  }
  return count;
}

EventEmitter.prototype.listenerCount = listenerCount;
EventEmitter.listenerCount = function (emitter, type) {
  if (typeof emitter.listenerCount === 'function') return emitter.listenerCount(type);
  return listenerCount.call(emitter, type);
};

EventEmitter.prototype.eventNames = function eventNames() {
  if (this._eventsCount === 0 || this._events === undefined) return [];
  const names = Object.keys(this._events);
  if (typeof Object.getOwnPropertySymbols === 'function') {
    return names.concat(Object.getOwnPropertySymbols(this._events));
  }
  return names;
};

// events.once(emitter, name): a promise for the next event's arguments.
function once(emitter, name) {
  return new Promise(function (resolve, reject) {
    function errorListener(err) {
      emitter.removeListener(name, resolver);
      reject(err);
    }
    function resolver() {
      if (name !== 'error') emitter.removeListener('error', errorListener);
      const args = [];
      for (let i = 0; i < arguments.length; i++) args.push(arguments[i]);
      resolve(args);
    }
    emitter.once(name, resolver);
    if (name !== 'error') emitter.once('error', errorListener);
  });
}

function getEventListeners(emitter, type) {
  if (typeof emitter.listeners === 'function') return emitter.listeners(type);
  return [];
}

function setMaxListeners(n) {
  const targets = [];
  for (let i = 1; i < arguments.length; i++) targets.push(arguments[i]);
  if (targets.length === 0) {
    EventEmitter.defaultMaxListeners = n;
    return;
  }
  for (let i = 0; i < targets.length; i++) {
    if (typeof targets[i].setMaxListeners === 'function') targets[i].setMaxListeners(n);
  }
}

module.exports = EventEmitter;
EventEmitter.EventEmitter = EventEmitter;
EventEmitter.once = once;
EventEmitter.getEventListeners = getEventListeners;
EventEmitter.setMaxListeners = setMaxListeners;
EventEmitter.usingDomains = false;
