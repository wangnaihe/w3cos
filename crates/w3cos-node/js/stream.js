'use strict';
// node:stream — Readable / Writable / Duplex / Transform / PassThrough,
// pipeline and finished. Follows Node's state machine closely enough for
// ordinary producers/consumers (buffering, highWaterMark backpressure,
// flowing vs paused mode, objectMode, destroy/autoDestroy, 'close').
const EE = require('events');
const { Buffer } = require('buffer');

function nextTick(fn, a, b) {
  process.nextTick(function () { fn(a, b); });
}

function codedError(Ctor, code, message) {
  const err = new Ctor(message);
  err.code = code;
  return err;
}

function Stream(opts) {
  EE.call(this, opts);
}
Object.setPrototypeOf(Stream.prototype, EE.prototype);
Object.setPrototypeOf(Stream, EE);

Stream.prototype.pipe = function pipe(dest, options) {
  return Readable.prototype.pipe.call(this, dest, options);
};

function getHighWaterMark(options, key, objectMode) {
  const hwm = options && (options.highWaterMark !== undefined ? options.highWaterMark : options[key]);
  if (hwm !== undefined && hwm !== null) {
    if (!(hwm >= 0) || Math.floor(hwm) !== hwm) {
      throw codedError(TypeError, 'ERR_INVALID_ARG_VALUE', "The property 'options.highWaterMark' is invalid. Received " + hwm);
    }
    return hwm;
  }
  return objectMode ? 16 : 16384;
}

function destroyImpl(stream, err, cb, rState, wState) {
  const states = [rState, wState].filter(Boolean);
  if (states.some(function (s) { return s.destroyed; })) {
    if (typeof cb === 'function') cb(err);
    return stream;
  }
  states.forEach(function (s) { s.destroyed = true; if (err && !s.errored) s.errored = err; });
  const finish = function (er) {
    if (typeof cb === 'function') cb(er);
    states.forEach(function (s) { s.closed = true; });
    nextTick(function () {
      if (er && !states.some(function (s) { return s.errorEmitted; })) {
        states.forEach(function (s) { s.errorEmitted = true; });
        stream.emit('error', er);
      }
      if (states.every(function (s) { return s.emitClose !== false; })) stream.emit('close');
    });
  };
  if (typeof stream._destroy === 'function') {
    let called = false;
    stream._destroy(err || null, function (er) { if (!called) { called = true; finish(er || err); } });
  } else {
    finish(err);
  }
  return stream;
}

// ── Readable ──

function ReadableState(options, stream, isDuplex) {
  options = options || {};
  this.objectMode = !!(options.objectMode || (isDuplex && options.readableObjectMode));
  this.highWaterMark = getHighWaterMark(options, 'readableHighWaterMark', this.objectMode);
  this.buffer = [];
  this.length = 0;
  this.pipes = [];
  this.flowing = null;
  this.ended = false;
  this.endEmitted = false;
  this.reading = false;
  this.constructed = true;
  this.sync = true;
  this.needReadable = false;
  this.emittedReadable = false;
  this.readableListening = false;
  this.resumeScheduled = false;
  this.errorEmitted = false;
  this.emitClose = options.emitClose !== false;
  this.autoDestroy = options.autoDestroy !== false;
  this.destroyed = false;
  this.errored = null;
  this.closed = false;
  this.defaultEncoding = options.defaultEncoding || 'utf8';
  this.decoder = null;
  this.encoding = null;
  this.dataEmitted = false;
  if (options.encoding) {
    const { StringDecoder } = require('string_decoder');
    this.decoder = new StringDecoder(options.encoding);
    this.encoding = options.encoding;
  }
}

function Readable(options) {
  if (!(this instanceof Readable)) return new Readable(options);
  const isDuplex = this instanceof Duplex;
  this._readableState = new ReadableState(options, this, isDuplex);
  if (options) {
    if (typeof options.read === 'function') this._read = options.read;
    if (typeof options.destroy === 'function') this._destroy = options.destroy;
  }
  Stream.call(this, options);
}
Object.setPrototypeOf(Readable.prototype, Stream.prototype);
Object.setPrototypeOf(Readable, Stream);

Readable.prototype._read = function _read() {
  throw codedError(Error, 'ERR_METHOD_NOT_IMPLEMENTED', 'The _read() method is not implemented');
};

Readable.prototype.push = function push(chunk, encoding) {
  return readableAddChunk(this, chunk, encoding, false);
};

Readable.prototype.unshift = function unshift(chunk, encoding) {
  return readableAddChunk(this, chunk, encoding, true);
};

function readableAddChunk(stream, chunk, encoding, addToFront) {
  const state = stream._readableState;
  if (chunk === null) {
    state.reading = false;
    onEofChunk(stream, state);
    return false;
  }
  if (!state.objectMode) {
    if (typeof chunk === 'string') {
      encoding = encoding || state.defaultEncoding;
      if (state.encoding !== encoding) {
        chunk = Buffer.from(chunk, encoding);
        encoding = '';
      }
    } else if (chunk instanceof Uint8Array) {
      if (!Buffer.isBuffer(chunk)) chunk = Buffer.from(chunk.buffer, chunk.byteOffset, chunk.byteLength);
      encoding = '';
    } else if (chunk !== undefined) {
      const err = codedError(TypeError, 'ERR_INVALID_ARG_TYPE',
        'The "chunk" argument must be of type string or an instance of Buffer or Uint8Array');
      errorOrDestroy(stream, err);
      return false;
    }
  }
  if (state.objectMode || (chunk && chunk.length > 0)) {
    if (addToFront) {
      if (state.endEmitted) {
        errorOrDestroy(stream, codedError(Error, 'ERR_STREAM_UNSHIFT_AFTER_END_EVENT', 'stream.unshift() after end event'));
        return false;
      }
      addChunk(stream, state, chunk, true);
    } else if (state.ended) {
      errorOrDestroy(stream, codedError(Error, 'ERR_STREAM_PUSH_AFTER_EOF', 'stream.push() after EOF'));
      return false;
    } else if (state.destroyed || state.errored) {
      return false;
    } else {
      state.reading = false;
      if (state.decoder && !encoding) {
        chunk = state.decoder.write(chunk);
        if (state.objectMode || chunk.length !== 0) addChunk(stream, state, chunk, false);
        else maybeReadMore(stream, state);
      } else {
        addChunk(stream, state, chunk, false);
      }
    }
  } else if (!addToFront) {
    state.reading = false;
    maybeReadMore(stream, state);
  }
  return !state.ended && (state.length < state.highWaterMark || state.length === 0);
}

function addChunk(stream, state, chunk, addToFront) {
  if (state.flowing && state.length === 0 && !state.sync && stream.listenerCount('data') > 0) {
    state.dataEmitted = true;
    stream.emit('data', chunk);
  } else {
    state.length += state.objectMode ? 1 : chunk.length;
    if (addToFront) state.buffer.unshift(chunk);
    else state.buffer.push(chunk);
    if (state.needReadable) emitReadable(stream);
  }
  maybeReadMore(stream, state);
}

function onEofChunk(stream, state) {
  if (state.ended) return;
  if (state.decoder) {
    const chunk = state.decoder.end();
    if (chunk && chunk.length) {
      state.buffer.push(chunk);
      state.length += state.objectMode ? 1 : chunk.length;
    }
  }
  state.ended = true;
  if (state.sync) {
    emitReadable(stream);
  } else {
    state.needReadable = false;
    state.emittedReadable = true;
    emitReadable_(stream);
  }
}

function emitReadable(stream) {
  const state = stream._readableState;
  state.needReadable = false;
  if (!state.emittedReadable) {
    state.emittedReadable = true;
    nextTick(emitReadable_, stream);
  }
}

function emitReadable_(stream) {
  const state = stream._readableState;
  if (!state.destroyed && !state.errored && (state.length || state.ended)) {
    stream.emit('readable');
    state.emittedReadable = false;
  }
  state.needReadable = !state.flowing && !state.ended && state.length <= state.highWaterMark;
  flow(stream);
}

function maybeReadMore(stream, state) {
  if (!state.readingMore && state.constructed) {
    state.readingMore = true;
    nextTick(maybeReadMore_, stream, state);
  }
}

function maybeReadMore_(stream, state) {
  while (!state.reading && !state.ended &&
      (state.length < state.highWaterMark || (state.flowing && state.length === 0))) {
    const len = state.length;
    stream.read(0);
    if (len === state.length) break;
  }
  state.readingMore = false;
}

function howMuchToRead(n, state) {
  if (n <= 0 || (state.length === 0 && state.ended)) return 0;
  if (state.objectMode) return 1;
  if (n !== n || n === undefined) {
    if (state.flowing && state.length) return state.objectMode ? 1 : state.buffer[0].length;
    return state.length;
  }
  if (n <= state.length) return n;
  return state.ended ? state.length : 0;
}

Readable.prototype.read = function read(n) {
  const state = this._readableState;
  if (n === undefined) n = NaN;
  else if (typeof n !== 'number') n = parseInt(n, 10);
  const nOrig = n;
  if (n > state.highWaterMark) {
    let hwm = 1;
    while (hwm < n) hwm *= 2;
    state.highWaterMark = hwm;
  }
  if (n !== 0) state.emittedReadable = false;
  if (n === 0 && state.needReadable &&
      ((state.highWaterMark !== 0 ? state.length >= state.highWaterMark : state.length > 0) || state.ended)) {
    if (state.length === 0 && state.ended) endReadable(this);
    else emitReadable(this);
    return null;
  }
  n = howMuchToRead(n, state);
  if (n === 0 && state.ended) {
    if (state.length === 0) endReadable(this);
    return null;
  }
  let doRead = state.needReadable;
  if (state.length === 0 || state.length - n < state.highWaterMark) doRead = true;
  if (state.ended || state.reading || state.destroyed || state.errored || !state.constructed) doRead = false;
  else if (doRead) {
    state.reading = true;
    state.sync = true;
    if (state.length === 0) state.needReadable = true;
    try {
      this._read(state.highWaterMark);
    } catch (err) {
      errorOrDestroy(this, err);
    }
    state.sync = false;
    if (!state.reading) n = howMuchToRead(nOrig, state);
  }
  let ret = n > 0 ? fromList(n, state) : null;
  if (ret === null) {
    state.needReadable = state.length <= state.highWaterMark;
    n = 0;
  } else {
    state.length -= n;
  }
  if (state.length === 0) {
    if (!state.ended) state.needReadable = true;
    if (nOrig !== n && state.ended) endReadable(this);
  }
  if (ret !== null && !state.errorEmitted && !state.closeEmitted) {
    state.dataEmitted = true;
    this.emit('data', ret);
  }
  return ret;
};

function fromList(n, state) {
  if (state.length === 0) return null;
  if (state.objectMode) return state.buffer.shift();
  if (!n || n >= state.length) {
    let ret;
    if (state.decoder) ret = state.buffer.join('');
    else if (state.buffer.length === 1) ret = state.buffer[0];
    else ret = Buffer.concat(state.buffer, state.length);
    state.buffer = [];
    return ret;
  }
  // Take exactly n units from the front.
  const first = state.buffer[0];
  if (n < first.length) {
    state.buffer[0] = first.slice(n);
    return first.slice(0, n);
  }
  if (n === first.length) return state.buffer.shift();
  const parts = [];
  let need = n;
  while (need > 0) {
    const head = state.buffer[0];
    if (head.length <= need) {
      parts.push(state.buffer.shift());
      need -= head.length;
    } else {
      parts.push(head.slice(0, need));
      state.buffer[0] = head.slice(need);
      need = 0;
    }
  }
  return state.decoder ? parts.join('') : Buffer.concat(parts, n);
}

function endReadable(stream) {
  const state = stream._readableState;
  if (!state.endEmitted) {
    state.ended = true;
    nextTick(endReadableNT, state, stream);
  }
}

function endReadableNT(state, stream) {
  if (!state.errored && !state.closeEmitted && !state.endEmitted && state.length === 0) {
    state.endEmitted = true;
    stream.emit('end');
    const wState = stream._writableState;
    if (wState && !stream.allowHalfOpen && !wState.ending && !wState.destroyed) {
      stream.end();
    }
    if (state.autoDestroy) {
      const autoDestroy = !wState || (wState.autoDestroy && (wState.finished || wState.writable === false));
      if (autoDestroy) stream.destroy();
    }
  }
}

function flow(stream) {
  const state = stream._readableState;
  while (state.flowing && stream.read() !== null);
}

Readable.prototype.on = function on(ev, fn) {
  const res = Stream.prototype.on.call(this, ev, fn);
  const state = this._readableState;
  if (ev === 'data') {
    state.readableListening = this.listenerCount('readable') > 0;
    if (state.flowing !== false) this.resume();
  } else if (ev === 'readable') {
    if (!state.endEmitted && !state.readableListening) {
      state.readableListening = state.needReadable = true;
      state.flowing = false;
      state.emittedReadable = false;
      if (state.length) emitReadable(this);
      else if (!state.reading) nextTick(function (self) { self.read(0); }, this);
    }
  }
  return res;
};
Readable.prototype.addListener = Readable.prototype.on;

Readable.prototype.removeListener = function removeListener(ev, fn) {
  const res = Stream.prototype.removeListener.call(this, ev, fn);
  if (ev === 'readable') {
    nextTick(updateReadableListening, this);
  }
  return res;
};
Readable.prototype.off = Readable.prototype.removeListener;

function updateReadableListening(self) {
  const state = self._readableState;
  state.readableListening = self.listenerCount('readable') > 0;
  if (state.resumeScheduled && state.paused === false) state.flowing = true;
  else if (self.listenerCount('data') > 0) self.resume();
  else if (!state.readableListening) state.flowing = null;
}

Readable.prototype.resume = function resume() {
  const state = this._readableState;
  if (!state.flowing) {
    state.flowing = !state.readableListening;
    if (!state.resumeScheduled) {
      state.resumeScheduled = true;
      nextTick(resume_, this, state);
    }
  }
  state.paused = false;
  return this;
};

function resume_(stream, state) {
  if (!state.reading) stream.read(0);
  state.resumeScheduled = false;
  stream.emit('resume');
  flow(stream);
  if (state.flowing && !state.reading) stream.read(0);
}

Readable.prototype.pause = function pause() {
  const state = this._readableState;
  if (state.flowing !== false) {
    state.flowing = false;
    this.emit('pause');
  }
  state.paused = true;
  return this;
};

Readable.prototype.isPaused = function isPaused() {
  const state = this._readableState;
  return state.paused === true || state.flowing === false;
};

Readable.prototype.setEncoding = function setEncoding(enc) {
  const { StringDecoder } = require('string_decoder');
  const decoder = new StringDecoder(enc);
  const state = this._readableState;
  state.decoder = decoder;
  state.encoding = decoder.encoding;
  let content = '';
  for (let i = 0; i < state.buffer.length; i++) content += decoder.write(state.buffer[i]);
  state.buffer = content ? [content] : [];
  state.length = content.length;
  return this;
};

Readable.prototype.pipe = function pipe(dest, pipeOpts) {
  const src = this;
  const state = this._readableState;
  state.pipes.push(dest);
  const doEnd = !pipeOpts || pipeOpts.end !== false;
  let ondrain = null;
  let cleanedUp = false;

  function onend() { dest.end(); }
  function unpipe() { cleanup(); }
  function cleanup() {
    if (cleanedUp) return;
    cleanedUp = true;
    dest.removeListener('close', onclose);
    dest.removeListener('finish', onfinish);
    if (ondrain) dest.removeListener('drain', ondrain);
    dest.removeListener('error', onerror);
    dest.removeListener('unpipe', onunpipe);
    src.removeListener('end', onend);
    src.removeListener('end', unpipe);
    src.removeListener('data', ondata);
  }
  function ondata(chunk) {
    const ret = dest.write(chunk);
    if (ret === false) {
      if (!ondrain) {
        ondrain = function () {
          src._readableState.awaitDrain = 0;
          if (src.listenerCount('data')) { src._readableState.flowing = true; flow(src); }
        };
        dest.on('drain', ondrain);
      }
      src.pause();
    }
  }
  function onerror(er) {
    unpipeDest();
    dest.removeListener('error', onerror);
    if (dest.listenerCount('error') === 0) {
      const s = dest._writableState || dest._readableState;
      if (s && !s.errorEmitted) errorOrDestroy(dest, er);
      else dest.emit('error', er);
    }
  }
  function onclose() { dest.removeListener('finish', onfinish); unpipeDest(); }
  function onfinish() { dest.removeListener('close', onclose); unpipeDest(); }
  function onunpipe(readable) { if (readable === src) cleanup(); }
  function unpipeDest() { src.unpipe(dest); }

  src.on('data', ondata);
  dest.prependListener ? dest.prependListener('error', onerror) : dest.on('error', onerror);
  dest.once('close', onclose);
  dest.once('finish', onfinish);
  dest.on('unpipe', onunpipe);
  if (doEnd) src.once('end', onend);
  else src.once('end', unpipe);
  dest.emit('pipe', src);
  if (dest.writableNeedDrain === true) {
    src.pause();
  } else if (!state.flowing) {
    src.resume();
  }
  return dest;
};

Readable.prototype.unpipe = function unpipe(dest) {
  const state = this._readableState;
  if (state.pipes.length === 0) return this;
  const dests = dest ? [dest] : state.pipes.slice();
  for (let i = 0; i < dests.length; i++) {
    const index = state.pipes.indexOf(dests[i]);
    if (index === -1) continue;
    state.pipes.splice(index, 1);
    if (state.pipes.length === 0) this.pause();
    dests[i].emit('unpipe', this, { hasUnpiped: false });
  }
  return this;
};

Readable.prototype.destroy = function destroy(err, cb) {
  return destroyImpl(this, err, cb, this._readableState, this._writableState);
};

Readable.prototype.wrap = function wrap(stream) {
  const self = this;
  let paused = false;
  stream.on('data', function (chunk) {
    if (!self.push(chunk) && stream.pause) { paused = true; stream.pause(); }
  });
  stream.on('end', function () { self.push(null); });
  stream.on('error', function (err) { errorOrDestroy(self, err); });
  stream.on('close', function () { self.destroy(); });
  this._read = function () { if (paused && stream.resume) { paused = false; stream.resume(); } };
  return this;
};

function createAsyncIterator(stream) {
  const queue = [];
  const waiting = [];
  let done = false;
  let error = null;
  function settle() {
    while (waiting.length) {
      const w = waiting.shift();
      if (queue.length) w.resolve({ value: queue.shift(), done: false });
      else if (error) w.reject(error);
      else if (done) w.resolve({ value: undefined, done: true });
      else { waiting.unshift(w); break; }
    }
  }
  stream.on('data', function (chunk) { queue.push(chunk); settle(); });
  stream.on('end', function () { done = true; settle(); });
  stream.on('error', function (err) { error = err; settle(); });
  stream.on('close', function () {
    if (!done && !error && !stream._readableState.endEmitted) {
      error = codedError(Error, 'ERR_STREAM_PREMATURE_CLOSE', 'Premature close');
    }
    done = true;
    settle();
  });
  const iterator = {
    next: function () {
      return new Promise(function (resolve, reject) { waiting.push({ resolve, reject }); settle(); });
    },
    return: function () {
      done = true;
      queue.length = 0;
      stream.destroy();
      return Promise.resolve({ value: undefined, done: true });
    },
  };
  iterator[Symbol.asyncIterator] = function () { return iterator; };
  return iterator;
}

Readable.prototype[Symbol.asyncIterator] = function () { return createAsyncIterator(this); };

Readable.from = function from(iterable, opts) {
  if (typeof iterable === 'string' || Buffer.isBuffer(iterable)) {
    return new Readable(Object.assign({ objectMode: true }, opts, {
      read: function () { this.push(iterable); this.push(null); },
    }));
  }
  let iterator;
  let isAsync = false;
  if (iterable && typeof iterable[Symbol.asyncIterator] === 'function') {
    iterator = iterable[Symbol.asyncIterator]();
    isAsync = true;
  } else if (iterable && typeof iterable[Symbol.iterator] === 'function') {
    iterator = iterable[Symbol.iterator]();
  } else {
    throw codedError(TypeError, 'ERR_INVALID_ARG_TYPE', 'The "iterable" argument must be an instance of Iterable.');
  }
  let reading = false;
  const readable = new Readable(Object.assign({ objectMode: true, highWaterMark: 1 }, opts));
  readable._read = function () {
    if (reading) return;
    reading = true;
    const step = function () {
      let result;
      try { result = iterator.next(); } catch (err) { readable.destroy(err); return; }
      Promise.resolve(result).then(function (r) {
        if (r.done) { readable.push(null); return; }
        Promise.resolve(r.value).then(function (value) {
          if (value === null) {
            readable.destroy(codedError(TypeError, 'ERR_STREAM_NULL_VALUES', 'May not write null values to stream'));
            return;
          }
          if (readable.push(value)) step();
          else reading = false;
        }, function (err) { readable.destroy(err); });
      }, function (err) { readable.destroy(err); });
    };
    if (isAsync || true) step();
  };
  return readable;
};

function defineGetters(proto, getters) {
  Object.keys(getters).forEach(function (key) {
    Object.defineProperty(proto, key, { get: getters[key], configurable: true, enumerable: false });
  });
}

defineGetters(Readable.prototype, {
  readable: function () {
    const s = this._readableState;
    return !!s && s.readable !== false && !s.destroyed && !s.errorEmitted && !s.endEmitted;
  },
  readableEnded: function () { return this._readableState.endEmitted; },
  readableFlowing: function () { return this._readableState.flowing; },
  readableLength: function () { return this._readableState.length; },
  readableHighWaterMark: function () { return this._readableState.highWaterMark; },
  readableObjectMode: function () { return this._readableState.objectMode; },
  readableEncoding: function () { return this._readableState.encoding; },
  destroyed: function () {
    const r = this._readableState;
    const w = this._writableState;
    return !!((r && r.destroyed) || (w && w.destroyed));
  },
  closed: function () { return this._readableState.closed; },
  errored: function () { return this._readableState.errored; },
});

function errorOrDestroy(stream, err) {
  const r = stream._readableState;
  const w = stream._writableState;
  if ((r && r.destroyed) || (w && w.destroyed)) return;
  if ((r && r.autoDestroy) || (w && w.autoDestroy)) stream.destroy(err);
  else if (err) {
    if (r) r.errored = err;
    if (w) w.errored = err;
    nextTick(function () { stream.emit('error', err); });
  }
}

// ── Writable ──

function WritableState(options, stream, isDuplex) {
  options = options || {};
  this.objectMode = !!(options.objectMode || (isDuplex && options.writableObjectMode));
  this.highWaterMark = getHighWaterMark(options, 'writableHighWaterMark', this.objectMode);
  this.finalCalled = false;
  this.needDrain = false;
  this.ending = false;
  this.ended = false;
  this.finished = false;
  this.destroyed = false;
  this.decodeStrings = options.decodeStrings !== false;
  this.defaultEncoding = options.defaultEncoding || 'utf8';
  this.length = 0;
  this.writing = false;
  this.corked = 0;
  this.sync = true;
  this.buffered = [];
  this.pendingcb = 0;
  this.prefinished = false;
  this.errorEmitted = false;
  this.emitClose = options.emitClose !== false;
  this.autoDestroy = options.autoDestroy !== false;
  this.errored = null;
  this.closed = false;
  this.constructed = true;
  this.writecb = null;
  this.writelen = 0;
  this.afterWriteTickInfo = null;
}

function Writable(options) {
  const isDuplex = this instanceof Duplex;
  if (!isDuplex && !(this instanceof Writable)) return new Writable(options);
  this._writableState = new WritableState(options, this, isDuplex);
  if (options) {
    if (typeof options.write === 'function') this._write = options.write;
    if (typeof options.writev === 'function') this._writev = options.writev;
    if (typeof options.destroy === 'function') this._destroy = options.destroy;
    if (typeof options.final === 'function') this._final = options.final;
  }
  Stream.call(this, options);
}
Object.setPrototypeOf(Writable.prototype, Stream.prototype);
Object.setPrototypeOf(Writable, Stream);

Writable.prototype._write = function _write(chunk, encoding, cb) {
  if (this._writev) this._writev([{ chunk, encoding }], cb);
  else throw codedError(Error, 'ERR_METHOD_NOT_IMPLEMENTED', 'The _write() method is not implemented');
};

function nop() {}

Writable.prototype.write = function write(chunk, encoding, cb) {
  const state = this._writableState;
  if (typeof encoding === 'function') { cb = encoding; encoding = null; }
  if (!encoding) encoding = state.defaultEncoding;
  if (typeof cb !== 'function') cb = nop;
  if (chunk === null) {
    throw codedError(TypeError, 'ERR_STREAM_NULL_VALUES', 'May not write null values to stream');
  }
  if (!state.objectMode) {
    if (typeof chunk === 'string') {
      if (state.decodeStrings !== false) { chunk = Buffer.from(chunk, encoding); encoding = 'buffer'; }
    } else if (chunk instanceof Uint8Array) {
      if (!Buffer.isBuffer(chunk)) chunk = Buffer.from(chunk.buffer, chunk.byteOffset, chunk.byteLength);
      encoding = 'buffer';
    } else {
      throw codedError(TypeError, 'ERR_INVALID_ARG_TYPE',
        'The "chunk" argument must be of type string or an instance of Buffer or Uint8Array');
    }
  }
  let err;
  if (state.ending) err = codedError(Error, 'ERR_STREAM_WRITE_AFTER_END', 'write after end');
  else if (state.destroyed) err = codedError(Error, 'ERR_STREAM_DESTROYED', 'Cannot call write after a stream was destroyed');
  if (err) {
    nextTick(cb, err);
    errorOrDestroy(this, err);
    return false;
  }
  state.pendingcb++;
  return writeOrBuffer(this, state, chunk, encoding, cb);
};

function writeOrBuffer(stream, state, chunk, encoding, callback) {
  const len = state.objectMode ? 1 : chunk.length;
  state.length += len;
  const ret = state.length < state.highWaterMark;
  if (!ret) state.needDrain = true;
  if (state.writing || state.corked || state.errored || !state.constructed) {
    state.buffered.push({ chunk, encoding, callback });
  } else {
    state.writelen = len;
    state.writecb = callback;
    state.writing = true;
    state.sync = true;
    stream._write(chunk, encoding, function (er) { onwrite(stream, er); });
    state.sync = false;
  }
  return ret && !state.errored && !state.destroyed;
}

function onwrite(stream, er) {
  const state = stream._writableState;
  const sync = state.sync;
  const cb = state.writecb;
  if (typeof cb !== 'function') {
    errorOrDestroy(stream, codedError(Error, 'ERR_MULTIPLE_CALLBACK', 'Callback called multiple times'));
    return;
  }
  state.writing = false;
  state.writecb = null;
  state.length -= state.writelen;
  state.writelen = 0;
  if (er) {
    if (!state.errored) state.errored = er;
    if (sync) nextTick(onwriteError, stream, { state, cb, er });
    else onwriteError(stream, { state, cb, er });
  } else {
    if (state.buffered.length > 0) clearBuffer(stream, state);
    if (sync) {
      nextTick(afterWrite, stream, cb);
    } else {
      afterWrite(stream, cb);
    }
  }
}

function onwriteError(stream, info) {
  info.state.pendingcb--;
  info.cb(info.er);
  errorOrDestroy(stream, info.er);
}

function afterWrite(stream, cb) {
  const state = stream._writableState;
  const needDrain = !state.ending && !stream.destroyed && state.length === 0 && state.needDrain;
  if (needDrain) {
    state.needDrain = false;
    stream.emit('drain');
  }
  state.pendingcb--;
  cb();
  finishMaybe(stream, state);
}

function clearBuffer(stream, state) {
  if (state.corked || state.destroyed || !state.constructed || state.writing) return;
  const buffered = state.buffered;
  if (buffered.length > 1 && stream._writev) {
    state.buffered = [];
    const callbacks = buffered.map(function (b) { return b.callback; });
    state.pendingcb -= callbacks.length - 1;
    const chunks = buffered.map(function (b) { return { chunk: b.chunk, encoding: b.encoding }; });
    state.writelen = state.objectMode ? buffered.length : buffered.reduce(function (n, b) { return n + b.chunk.length; }, 0);
    state.writecb = function (err) { for (let i = 0; i < callbacks.length; i++) callbacks[i](err); };
    state.writing = true;
    state.sync = true;
    stream._writev(chunks, function (er) { onwrite(stream, er); });
    state.sync = false;
  } else if (buffered.length > 0) {
    const b = buffered.shift();
    state.writelen = state.objectMode ? 1 : b.chunk.length;
    state.writecb = b.callback;
    state.writing = true;
    state.sync = true;
    stream._write(b.chunk, b.encoding, function (er) { onwrite(stream, er); });
    state.sync = false;
  }
}

Writable.prototype.cork = function cork() { this._writableState.corked++; };

Writable.prototype.uncork = function uncork() {
  const state = this._writableState;
  if (state.corked) {
    state.corked--;
    if (!state.writing) clearBuffer(this, state);
  }
};

Writable.prototype.setDefaultEncoding = function setDefaultEncoding(encoding) {
  if (typeof encoding === 'string') encoding = encoding.toLowerCase();
  if (!Buffer.isEncoding(encoding)) {
    throw codedError(TypeError, 'ERR_UNKNOWN_ENCODING', 'Unknown encoding: ' + encoding);
  }
  this._writableState.defaultEncoding = encoding;
  return this;
};

Writable.prototype.end = function end(chunk, encoding, cb) {
  const state = this._writableState;
  if (typeof chunk === 'function') { cb = chunk; chunk = null; encoding = null; }
  else if (typeof encoding === 'function') { cb = encoding; encoding = null; }
  if (chunk !== null && chunk !== undefined) this.write(chunk, encoding);
  if (state.corked) { state.corked = 1; this.uncork(); }
  let err;
  if (!state.errored && !state.ending) {
    state.ending = true;
    finishMaybe(this, state, true);
    state.ended = true;
  } else if (state.finished) {
    err = codedError(Error, 'ERR_STREAM_ALREADY_FINISHED', 'Cannot call end after a stream was finished');
  } else if (state.destroyed) {
    err = codedError(Error, 'ERR_STREAM_DESTROYED', 'Cannot call end after a stream was destroyed');
  }
  if (typeof cb === 'function') {
    if (err || state.finished) nextTick(cb, err);
    else {
      const self = this;
      const onfinish = function () { self.removeListener('error', onerror); cb(); };
      const onerror = function (e) { self.removeListener('finish', onfinish); cb(e); };
      this.once('finish', onfinish);
      this.once('error', onerror);
    }
  }
  return this;
};

function needFinish(state) {
  return state.ending && !state.destroyed && state.constructed && state.length === 0 &&
    !state.errored && state.buffered.length === 0 && !state.finished && !state.writing;
}

function prefinish(stream, state) {
  if (!state.prefinished && !state.finalCalled) {
    if (typeof stream._final === 'function' && !state.destroyed) {
      state.finalCalled = true;
      state.sync = true;
      state.pendingcb++;
      let called = false;
      const done = function (err) {
        if (called) return;
        called = true;
        state.pendingcb--;
        if (err) {
          errorOrDestroy(stream, err);
        } else if (needFinish(state)) {
          state.prefinished = true;
          stream.emit('prefinish');
          state.pendingcb++;
          nextTick(finish, stream, state);
        }
      };
      try {
        stream._final(done);
      } catch (err) {
        done(err);
      }
      state.sync = false;
    } else {
      state.prefinished = true;
      stream.emit('prefinish');
    }
  }
}

function finishMaybe(stream, state, sync) {
  if (needFinish(state)) {
    prefinish(stream, state);
    if (state.pendingcb === 0 && needFinish(state)) {
      state.pendingcb++;
      if (sync) nextTick(finish, stream, state);
      else finish(stream, state);
    }
  }
}

function finish(stream, state) {
  state.pendingcb--;
  if (state.finished || state.errored || state.destroyed) return;
  state.finished = true;
  stream.emit('finish');
  if (state.autoDestroy) {
    const rState = stream._readableState;
    const autoDestroy = !rState || (rState.autoDestroy && (rState.endEmitted || rState.readable === false));
    if (autoDestroy) stream.destroy();
  }
}

Writable.prototype.destroy = function destroy(err, cb) {
  const state = this._writableState;
  if (!state.destroyed && state.buffered.length) {
    const buffered = state.buffered;
    state.buffered = [];
    const e = codedError(Error, 'ERR_STREAM_DESTROYED', 'Cannot call write after a stream was destroyed');
    buffered.forEach(function (b) { nextTick(b.callback, e); });
  }
  return destroyImpl(this, err, cb, this._readableState, state);
};

defineGetters(Writable.prototype, {
  writable: function () {
    const w = this._writableState;
    return !!w && w.writable !== false && !w.destroyed && !w.errored && !w.ending && !w.ended;
  },
  writableEnded: function () { return this._writableState.ending; },
  writableFinished: function () { return this._writableState.finished; },
  writableLength: function () { return this._writableState.length; },
  writableHighWaterMark: function () { return this._writableState.highWaterMark; },
  writableObjectMode: function () { return this._writableState.objectMode; },
  writableCorked: function () { return this._writableState.corked; },
  writableNeedDrain: function () {
    const w = this._writableState;
    return !w.destroyed && !w.ending && w.needDrain;
  },
  destroyed: function () { return this._writableState.destroyed; },
  closed: function () { return this._writableState.closed; },
  errored: function () { return this._writableState.errored; },
});

// ── Duplex / Transform / PassThrough ──

function Duplex(options) {
  if (!(this instanceof Duplex)) return new Duplex(options);
  Readable.call(this, options);
  Writable.call(this, options);
  this.allowHalfOpen = !options || options.allowHalfOpen !== false;
  if (options) {
    if (options.readable === false) {
      this._readableState.readable = false;
      this._readableState.ended = true;
      this._readableState.endEmitted = true;
    }
    if (options.writable === false) {
      this._writableState.writable = false;
      this._writableState.ending = true;
      this._writableState.ended = true;
      this._writableState.finished = true;
    }
  }
}
Object.setPrototypeOf(Duplex.prototype, Readable.prototype);
Object.setPrototypeOf(Duplex, Readable);
['write', 'cork', 'uncork', 'setDefaultEncoding', 'end', '_write'].forEach(function (method) {
  if (!Object.prototype.hasOwnProperty.call(Duplex.prototype, method)) {
    Duplex.prototype[method] = Writable.prototype[method];
  }
});
Duplex.prototype.destroy = Readable.prototype.destroy;
(function copyWritableGetters() {
  const getters = {};
  ['writable', 'writableEnded', 'writableFinished', 'writableLength', 'writableHighWaterMark',
    'writableObjectMode', 'writableCorked', 'writableNeedDrain'].forEach(function (key) {
    const desc = Object.getOwnPropertyDescriptor(Writable.prototype, key);
    if (desc && desc.get) getters[key] = desc.get;
  });
  defineGetters(Duplex.prototype, getters);
})();

function Transform(options) {
  if (!(this instanceof Transform)) return new Transform(options);
  const readableHighWaterMark = options ? getHighWaterMark(options, 'readableHighWaterMark', !!(options.objectMode || options.readableObjectMode)) : null;
  if (readableHighWaterMark === 0) {
    options = Object.assign({}, options, { readableHighWaterMark: null, highWaterMark: undefined });
  }
  Duplex.call(this, options);
  this._readableState.sync = false;
  this._transformCallback = null;
  if (options) {
    if (typeof options.transform === 'function') this._transform = options.transform;
    if (typeof options.flush === 'function') this._flush = options.flush;
  }
  this.on('prefinish', transformPrefinish);
}
Object.setPrototypeOf(Transform.prototype, Duplex.prototype);
Object.setPrototypeOf(Transform, Duplex);

function transformFinal(cb) {
  const self = this;
  if (typeof this._flush === 'function' && !this.destroyed) {
    this._flush(function (er, data) {
      if (er) {
        if (cb) cb(er); else self.destroy(er);
        return;
      }
      if (data !== null && data !== undefined) self.push(data);
      self.push(null);
      if (cb) cb();
    });
  } else {
    this.push(null);
    if (cb) cb();
  }
}

function transformPrefinish() {
  if (this._final !== transformFinal) transformFinal.call(this);
}

Transform.prototype._final = transformFinal;

Transform.prototype._transform = function _transform() {
  throw codedError(Error, 'ERR_METHOD_NOT_IMPLEMENTED', 'The _transform() method is not implemented');
};

Transform.prototype._write = function _write(chunk, encoding, callback) {
  const self = this;
  const rState = this._readableState;
  const wState = this._writableState;
  const length = rState.length;
  this._transform(chunk, encoding, function (err, val) {
    if (err) { callback(err); return; }
    if (val !== null && val !== undefined) self.push(val);
    if (wState.ended || length === rState.length || rState.length < rState.highWaterMark) {
      callback();
    } else {
      self._transformCallback = callback;
    }
  });
};

Transform.prototype._read = function _read() {
  if (this._transformCallback) {
    const callback = this._transformCallback;
    this._transformCallback = null;
    callback();
  }
};

function PassThrough(options) {
  if (!(this instanceof PassThrough)) return new PassThrough(options);
  Transform.call(this, options);
}
Object.setPrototypeOf(PassThrough.prototype, Transform.prototype);
Object.setPrototypeOf(PassThrough, Transform);
PassThrough.prototype._transform = function (chunk, encoding, cb) { cb(null, chunk); };

// ── finished / pipeline ──

function once(fn) {
  let called = false;
  return function () {
    if (called) return;
    called = true;
    return fn.apply(this, arguments);
  };
}

function eos(stream, options, callback) {
  if (typeof options === 'function') { callback = options; options = {}; }
  options = options || {};
  callback = once(callback);
  const rState = stream._readableState;
  const wState = stream._writableState;
  const readable = options.readable !== undefined ? options.readable : !!rState && rState.readable !== false;
  const writable = options.writable !== undefined ? options.writable : !!wState && wState.writable !== false;
  let readableFinished = !!(rState && rState.endEmitted);
  let writableFinished = !!(wState && wState.finished);
  const onfinish = function () {
    writableFinished = true;
    if (!readable || readableFinished) callback.call(stream);
  };
  const onend = function () {
    readableFinished = true;
    if (!writable || writableFinished) callback.call(stream);
  };
  const onerror = function (err) { callback.call(stream, err); };
  const onclose = function () {
    if (readable && !readableFinished) {
      return callback.call(stream, codedError(Error, 'ERR_STREAM_PREMATURE_CLOSE', 'Premature close'));
    }
    if (writable && !writableFinished) {
      return callback.call(stream, codedError(Error, 'ERR_STREAM_PREMATURE_CLOSE', 'Premature close'));
    }
    callback.call(stream);
  };
  stream.on('end', onend);
  stream.on('finish', onfinish);
  if (options.error !== false) stream.on('error', onerror);
  stream.on('close', onclose);
  const cleanup = function () {
    stream.removeListener('end', onend);
    stream.removeListener('finish', onfinish);
    stream.removeListener('error', onerror);
    stream.removeListener('close', onclose);
  };
  if ((rState && rState.closed) || (wState && wState.closed)) {
    nextTick(onclose);
  } else if ((!readable || readableFinished) && (!writable || writableFinished)) {
    nextTick(function () { callback.call(stream); });
  }
  return cleanup;
}

function pipeline() {
  const streams = Array.prototype.slice.call(arguments);
  let callback = typeof streams[streams.length - 1] === 'function' ? streams.pop() : null;
  if (Array.isArray(streams[0]) && streams.length === 1) {
    const inner = streams.pop();
    for (let i = 0; i < inner.length; i++) streams.push(inner[i]);
  }
  if (!callback) {
    throw codedError(TypeError, 'ERR_INVALID_ARG_TYPE', 'The "streams[stream.length - 1]" argument must be of type function');
  }
  if (streams.length < 2) {
    throw codedError(TypeError, 'ERR_MISSING_ARGS', 'The "streams" argument must be specified');
  }
  callback = once(callback);
  let error = null;
  let remaining = streams.length;
  const destroys = [];
  function finishOne(err) {
    if (err && (!error || error.code === 'ERR_STREAM_PREMATURE_CLOSE')) error = err;
    if (err) destroys.forEach(function (d) { d(error); });
    remaining--;
    if (remaining === 0 || (err && remaining >= 0 && error === err)) {
      if (remaining === 0 || err) nextTick(function () { callback(error, last); });
    }
  }
  const last = streams[streams.length - 1];
  for (let i = 0; i < streams.length; i++) {
    const stream = streams[i];
    const reading = i < streams.length - 1;
    const writing = i > 0;
    eos(stream, { readable: reading, writable: writing }, function (err) { finishOne(err); });
    destroys.push(function (err) {
      if (!stream.destroyed && typeof stream.destroy === 'function') stream.destroy(err);
    });
    if (reading) stream.pipe(streams[i + 1]);
  }
  return last;
}

Stream.Stream = Stream;
Stream.Readable = Readable;
Stream.Writable = Writable;
Stream.Duplex = Duplex;
Stream.Transform = Transform;
Stream.PassThrough = PassThrough;
Stream.finished = eos;
Stream.pipeline = pipeline;
Stream.addAbortSignal = function addAbortSignal(signal, stream) { return stream; };
Stream.isReadable = function isReadable(s) { return !!(s && s._readableState && s.readable); };
Stream.isErrored = function isErrored(s) { return !!(s && ((s._readableState && s._readableState.errored) || (s._writableState && s._writableState.errored))); };
Stream.isDisturbed = function isDisturbed(s) { return !!(s && s._readableState && (s._readableState.dataEmitted || s._readableState.endEmitted)); };
Stream.getDefaultHighWaterMark = function getDefaultHighWaterMark(objectMode) { return objectMode ? 16 : 16384; };
Stream.promises = {
  pipeline: function () {
    const args = Array.prototype.slice.call(arguments);
    return new Promise(function (resolve, reject) {
      args.push(function (err, value) { if (err) reject(err); else resolve(value); });
      pipeline.apply(null, args);
    });
  },
  finished: function (stream, opts) {
    return new Promise(function (resolve, reject) {
      eos(stream, opts, function (err) { if (err) reject(err); else resolve(); });
    });
  },
};

module.exports = Stream;
