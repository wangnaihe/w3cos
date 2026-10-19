'use strict';
// `@vscode/sqlite3` / `@w3cos/sqlite3` (M3 item 20): the node-sqlite3 API
// over the runtime's SQLite (`__w3cos_node_host.sqlite*`, see
// w3cos-runtime/src/node_sqlite.rs). Statements run on the calling thread.
// Callbacks are asynchronous and queued per database in call order, as with
// node-sqlite3, so `serialize()` and `parallelize()` behave the same.

const EventEmitter = require('events');
const { Buffer } = require('buffer');

const host = __w3cos_node_host;

const OPEN_READONLY = 0x00000001;
const OPEN_READWRITE = 0x00000002;
const OPEN_CREATE = 0x00000004;
const OPEN_FULLMUTEX = 0x00010000;
const OPEN_URI = 0x00000040;
const OPEN_SHAREDCACHE = 0x00020000;
const OPEN_PRIVATECACHE = 0x00040000;

function makeError(info) {
  const error = new Error(info.message);
  error.errno = info.errno;
  error.code = info.code;
  return error;
}

function isPlainObject(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value) &&
    !Buffer.isBuffer(value) && !ArrayBuffer.isView(value) && !(value instanceof Date);
}

function normalize(value) {
  if (value === undefined || value === null) return null;
  if (typeof value === 'boolean') return value ? 1 : 0;
  if (typeof value === 'number' || typeof value === 'string') return value;
  if (value instanceof Date) return value.getTime();
  if (Buffer.isBuffer(value) || ArrayBuffer.isView(value)) return value;
  if (typeof value === 'bigint') return Number(value);
  return String(value);
}

// node-sqlite3 parameter forms: (array), (object of names), or a list.
function splitParams(params) {
  if (params.length === 1 && Array.isArray(params[0])) {
    return { positional: params[0].map(normalize), named: null };
  }
  if (params.length === 1 && isPlainObject(params[0])) {
    const named = Object.keys(params[0]).map((key) => [key, normalize(params[0][key])]);
    return { positional: null, named };
  }
  return { positional: params.map(normalize), named: null };
}

function decodeRows(result) {
  const columns = result.columns;
  return result.rows.map((values) => {
    const row = {};
    for (let i = 0; i < columns.length; i++) {
      const value = values[i];
      row[columns[i]] = value !== null && typeof value === 'object' && value.blob
        ? Buffer.from(value.blob)
        : value;
    }
    return row;
  });
}

// Split trailing callbacks off an argument list.
function takeCallbacks(args, count) {
  const list = Array.prototype.slice.call(args);
  const callbacks = [];
  while (callbacks.length < count && list.length && typeof list[list.length - 1] === 'function') {
    callbacks.unshift(list.pop());
  }
  return { params: list, callbacks };
}

class Database extends EventEmitter {
  constructor(filename, mode, callback) {
    super();
    if (typeof mode === 'function') {
      callback = mode;
      mode = undefined;
    }
    if (typeof filename !== 'string') throw new TypeError('Argument 0 must be a string');
    this.filename = filename;
    this.mode = typeof mode === 'number' ? mode : (OPEN_READWRITE | OPEN_CREATE | OPEN_FULLMUTEX);
    this.open = false;
    this._id = 0;
    this._queue = [];
    this._scheduled = false;
    this._closed = false;
    this._enqueue(() => {
      const result = host.sqliteOpen(filename, this.mode);
      if (result.error) {
        const error = makeError(result.error);
        if (callback) callback.call(this, error);
        else this.emit('error', error);
        return;
      }
      this._id = result.id;
      this.open = true;
      if (callback) callback.call(this, null);
      this.emit('open');
    });
  }

  _enqueue(job) {
    this._queue.push(job);
    if (!this._scheduled) {
      this._scheduled = true;
      setImmediate(() => this._pump());
    }
  }

  _pump() {
    this._scheduled = false;
    // Jobs queued by these callbacks wait for the next turn, as they would
    // behind node-sqlite3's thread pool.
    const jobs = this._queue;
    this._queue = [];
    for (const job of jobs) job();
  }

  _fail(error, callback, target) {
    if (callback) callback.call(target || this, error);
    else this.emit('error', error);
  }

  _query(sql, params, wantRows) {
    if (this.listenerCount('trace')) this.emit('trace', sql);
    const started = Date.now();
    const { positional, named } = splitParams(params);
    const result = this.open
      ? host.sqliteQuery(this._id, sql, positional, named, wantRows)
      : { error: { message: 'SQLITE_MISUSE: Database is closed', code: 'SQLITE_MISUSE', errno: 21 } };
    if (!result.error && this.listenerCount('profile')) this.emit('profile', sql, Date.now() - started);
    return result;
  }

  run(sql, ...rest) {
    const { params, callbacks } = takeCallbacks(rest, 1);
    const statement = new Statement(this, sql, null, true);
    this._enqueue(() => statement._run(params, callbacks[0]));
    return this;
  }

  get(sql, ...rest) {
    const { params, callbacks } = takeCallbacks(rest, 1);
    this._enqueue(() => {
      const result = this._query(sql, params, true);
      if (result.error) return this._fail(makeError(result.error), callbacks[0]);
      if (callbacks[0]) callbacks[0].call(this, null, decodeRows(result)[0]);
    });
    return this;
  }

  all(sql, ...rest) {
    const { params, callbacks } = takeCallbacks(rest, 1);
    this._enqueue(() => {
      const result = this._query(sql, params, true);
      if (result.error) return this._fail(makeError(result.error), callbacks[0]);
      if (callbacks[0]) callbacks[0].call(this, null, decodeRows(result));
    });
    return this;
  }

  each(sql, ...rest) {
    const { params, callbacks } = takeCallbacks(rest, 2);
    const [onRow, onComplete] = callbacks;
    this._enqueue(() => {
      const result = this._query(sql, params, true);
      if (result.error) return this._fail(makeError(result.error), onRow);
      const rows = decodeRows(result);
      for (const row of rows) if (onRow) onRow.call(this, null, row);
      if (onComplete) onComplete.call(this, null, rows.length);
    });
    return this;
  }

  exec(sql, callback) {
    this._enqueue(() => {
      if (this.listenerCount('trace')) this.emit('trace', sql);
      const result = this.open
        ? host.sqliteExec(this._id, sql)
        : { error: { message: 'SQLITE_MISUSE: Database is closed', code: 'SQLITE_MISUSE', errno: 21 } };
      if (result.error) return this._fail(makeError(result.error), callback);
      if (callback) callback.call(this, null);
    });
    return this;
  }

  prepare(sql, ...rest) {
    const { params, callbacks } = takeCallbacks(rest, 1);
    return new Statement(this, sql, params.length ? params : null, false, callbacks[0]);
  }

  map(sql, ...rest) {
    const { params, callbacks } = takeCallbacks(rest, 1);
    return this.all(sql, ...params, (error, rows) => {
      if (error) return callbacks[0] && callbacks[0].call(this, error);
      const out = {};
      for (const row of rows) {
        const keys = Object.keys(row);
        out[row[keys[0]]] = keys.length === 2 ? row[keys[1]] : row;
      }
      if (callbacks[0]) callbacks[0].call(this, null, out);
    });
  }

  serialize(callback) {
    if (callback) callback();
    return this;
  }

  parallelize(callback) {
    if (callback) callback();
    return this;
  }

  configure(option, value) {
    if (option === 'busyTimeout') {
      this._enqueue(() => { if (this.open) host.sqliteBusyTimeout(this._id, value); });
    }
    return this;
  }

  interrupt() {}

  wait(callback) {
    this._enqueue(() => { if (callback) callback.call(this, null); });
    return this;
  }

  loadExtension(path, callback) {
    this._enqueue(() => {
      const error = new Error('SQLITE_ERROR: extension loading is not available in w3cos');
      error.code = 'SQLITE_ERROR';
      error.errno = 1;
      this._fail(error, callback);
    });
    return this;
  }

  close(callback) {
    this._enqueue(() => {
      if (!this.open) {
        const error = makeError({ message: 'SQLITE_MISUSE: Database is closed', code: 'SQLITE_MISUSE', errno: 21 });
        return this._fail(error, callback);
      }
      const result = host.sqliteClose(this._id);
      if (result.error) return this._fail(makeError(result.error), callback);
      this.open = false;
      if (callback) callback.call(this, null);
      this.emit('close');
    });
    return this;
  }
}

class Statement extends EventEmitter {
  constructor(db, sql, params, internal, callback) {
    super();
    if (!(db instanceof Database)) throw new TypeError('Database object expected');
    this.db = db;
    this.sql = sql;
    this._params = params;
    this._finalized = false;
    if (!internal) {
      db._enqueue(() => {
        if (callback) callback.call(this, null);
      });
    }
  }

  _fail(error, callback) {
    if (callback) callback.call(this, error);
    else if (this.listenerCount('error')) this.emit('error', error);
    else this.db.emit('error', error);
  }

  _paramsFor(params) {
    if (params.length) return params;
    return this._params || [];
  }

  bind(...rest) {
    const { params, callbacks } = takeCallbacks(rest, 1);
    this._params = params;
    this.db._enqueue(() => { if (callbacks[0]) callbacks[0].call(this, null); });
    return this;
  }

  reset(callback) {
    this.db._enqueue(() => { if (callback) callback.call(this, null); });
    return this;
  }

  _run(params, callback) {
    const result = this.db._query(this.sql, this._paramsFor(params), false);
    if (result.error) return this._fail(makeError(result.error), callback);
    this.lastID = result.lastID;
    this.changes = result.changes;
    if (callback) callback.call(this, null);
  }

  run(...rest) {
    const { params, callbacks } = takeCallbacks(rest, 1);
    this.db._enqueue(() => this._run(params, callbacks[0]));
    return this;
  }

  get(...rest) {
    const { params, callbacks } = takeCallbacks(rest, 1);
    this.db._enqueue(() => {
      const result = this.db._query(this.sql, this._paramsFor(params), true);
      if (result.error) return this._fail(makeError(result.error), callbacks[0]);
      if (callbacks[0]) callbacks[0].call(this, null, decodeRows(result)[0]);
    });
    return this;
  }

  all(...rest) {
    const { params, callbacks } = takeCallbacks(rest, 1);
    this.db._enqueue(() => {
      const result = this.db._query(this.sql, this._paramsFor(params), true);
      if (result.error) return this._fail(makeError(result.error), callbacks[0]);
      if (callbacks[0]) callbacks[0].call(this, null, decodeRows(result));
    });
    return this;
  }

  each(...rest) {
    const { params, callbacks } = takeCallbacks(rest, 2);
    const [onRow, onComplete] = callbacks;
    this.db._enqueue(() => {
      const result = this.db._query(this.sql, this._paramsFor(params), true);
      if (result.error) return this._fail(makeError(result.error), onRow);
      const rows = decodeRows(result);
      for (const row of rows) if (onRow) onRow.call(this, null, row);
      if (onComplete) onComplete.call(this, null, rows.length);
    });
    return this;
  }

  finalize(callback) {
    this._finalized = true;
    this.db._enqueue(() => { if (callback) callback.call(this); });
    return this.db;
  }
}

const sqlite3 = {
  Database,
  Statement,
  Backup: class Backup {
    constructor() {
      throw new Error('sqlite3.Backup is not available in w3cos');
    }
  },
  OPEN_READONLY,
  OPEN_READWRITE,
  OPEN_CREATE,
  OPEN_FULLMUTEX,
  OPEN_URI,
  OPEN_SHAREDCACHE,
  OPEN_PRIVATECACHE,
  VERSION: '3 (w3cos)',
  SOURCE_ID: 'w3cos',
  VERSION_NUMBER: 3000000,
  OK: 0, ERROR: 1, INTERNAL: 2, PERM: 3, ABORT: 4, BUSY: 5, LOCKED: 6, NOMEM: 7,
  READONLY: 8, INTERRUPT: 9, IOERR: 10, CORRUPT: 11, NOTFOUND: 12, FULL: 13,
  CANTOPEN: 14, PROTOCOL: 15, EMPTY: 16, SCHEMA: 17, TOOBIG: 18, CONSTRAINT: 19,
  MISMATCH: 20, MISUSE: 21, NOLFS: 22, AUTH: 23, FORMAT: 24, RANGE: 25, NOTADB: 26,
  cached: {
    objects: {},
    Database(file, mode, callback) {
      if (typeof mode === 'function') { callback = mode; mode = undefined; }
      const key = file;
      let db = sqlite3.cached.objects[key];
      if (!db || !db.open && db._id) {
        db = sqlite3.cached.objects[key] = new Database(file, mode, callback);
      } else if (callback) {
        db._enqueue(() => callback.call(db, null));
      }
      return db;
    },
  },
  verbose() {
    return sqlite3;
  },
};

module.exports = sqlite3;
// `import sqlite3 from '@vscode/sqlite3'` through ESM interop.
module.exports.default = sqlite3;
