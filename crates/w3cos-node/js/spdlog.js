'use strict';
// `@vscode/spdlog` / `@w3cos/spdlog` (M3 item 20): spdlog's rotating file
// logger in plain JS over `fs`, with the same API, level names, pattern
// flags and rotation (`main.log` -> `main.1.log` -> ... `main.N.log`, a
// rotation whenever a record would push the file past `maxFileSize`).
// Writes are synchronous, so `flush()` has nothing left to do and the async
// logger types behave like the sync one.

const fs = require('fs');
const path = require('path');

const LEVELS = ['trace', 'debug', 'info', 'warning', 'error', 'critical', 'off'];
const SHORT = ['T', 'D', 'I', 'W', 'E', 'C', 'O'];
const DEFAULT_PATTERN = '[%Y-%m-%d %H:%M:%S.%e] [%n] [%l] %v';
const DAYS = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];
const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];

const registry = new Map();
let globalLevel = 2;

function pad(value, width) {
  return String(value).padStart(width, '0');
}

function offset(date) {
  const minutes = -date.getTimezoneOffset();
  const sign = minutes < 0 ? '-' : '+';
  const abs = Math.abs(minutes);
  return sign + pad(Math.floor(abs / 60), 2) + ':' + pad(abs % 60, 2);
}

function format(pattern, name, level, message) {
  const date = new Date();
  let out = '';
  for (let i = 0; i < pattern.length; i++) {
    const ch = pattern[i];
    if (ch !== '%' || i + 1 >= pattern.length) {
      out += ch;
      continue;
    }
    const flag = pattern[++i];
    switch (flag) {
      case 'v': out += message; break;
      case 'n': out += name; break;
      case 'l': out += LEVELS[level]; break;
      case 'L': out += SHORT[level]; break;
      case 'Y': out += date.getFullYear(); break;
      case 'y': out += pad(date.getFullYear() % 100, 2); break;
      case 'm': out += pad(date.getMonth() + 1, 2); break;
      case 'd': out += pad(date.getDate(), 2); break;
      case 'H': out += pad(date.getHours(), 2); break;
      case 'I': out += pad(date.getHours() % 12 || 12, 2); break;
      case 'M': out += pad(date.getMinutes(), 2); break;
      case 'S': out += pad(date.getSeconds(), 2); break;
      case 'e': out += pad(date.getMilliseconds(), 3); break;
      case 'f': out += pad(date.getMilliseconds() * 1000, 6); break;
      case 'F': out += pad(date.getMilliseconds() * 1000000, 9); break;
      case 'p': out += date.getHours() < 12 ? 'AM' : 'PM'; break;
      case 'a': out += DAYS[date.getDay()]; break;
      case 'b': case 'h': out += MONTHS[date.getMonth()]; break;
      case 'D': out += pad(date.getMonth() + 1, 2) + '/' + pad(date.getDate(), 2) + '/' + pad(date.getFullYear() % 100, 2); break;
      case 'T': case 'X': out += pad(date.getHours(), 2) + ':' + pad(date.getMinutes(), 2) + ':' + pad(date.getSeconds(), 2); break;
      case 'z': out += offset(date); break;
      case 'E': out += Math.floor(date.getTime() / 1000); break;
      case 'P': out += process.pid; break;
      case 't': out += 1; break;
      case '^': case '$': break;
      case '%': out += '%'; break;
      default: out += '%' + flag;
    }
  }
  return out + '\n';
}

// main.log -> main.1.log, like spdlog's rotating_file_sink::calc_filename.
function rotatedName(base, index) {
  if (index === 0) return base;
  const ext = path.extname(base);
  const stem = ext && path.basename(base) !== ext ? base.slice(0, -ext.length) : base;
  return stem + '.' + index + (stem === base ? '' : ext);
}

class Logger {
  constructor(loggerType, name, filename, maxFileSize, maxFiles) {
    if (registry.has(name)) {
      throw new Error("logger with name '" + name + "' already exists");
    }
    this._type = loggerType;
    this._name = name;
    this._filename = filename;
    this._maxSize = Number(maxFileSize) || Infinity;
    this._maxFiles = Math.max(0, Number(maxFiles) | 0);
    this._level = globalLevel;
    this._pattern = DEFAULT_PATTERN;
    this._dropped = false;
    if (loggerType !== 'stdout_async') {
      fs.mkdirSync(path.dirname(filename), { recursive: true });
      fs.closeSync(fs.openSync(filename, 'a'));
      this._size = fs.statSync(filename).size;
    }
    registry.set(name, this);
  }

  _log(level, message) {
    if (this._dropped || level < this._level || this._level >= 6) return;
    const record = format(this._pattern, this._name, level, String(message));
    if (this._type === 'stdout_async') {
      process.stdout.write(record);
      return;
    }
    const bytes = Buffer.byteLength(record);
    let size = this._size + bytes;
    if (size > this._maxSize && this._size > 0) {
      this._rotate();
      size = bytes;
    }
    fs.appendFileSync(this._filename, record);
    this._size = size;
  }

  _rotate() {
    for (let i = this._maxFiles; i > 0; i--) {
      const src = rotatedName(this._filename, i - 1);
      if (!fs.existsSync(src)) continue;
      const target = rotatedName(this._filename, i);
      fs.rmSync(target, { force: true });
      fs.renameSync(src, target);
    }
    if (this._maxFiles === 0) fs.truncateSync(this._filename, 0);
  }

  trace(message) { this._log(0, message); }
  debug(message) { this._log(1, message); }
  info(message) { this._log(2, message); }
  warn(message) { this._log(3, message); }
  error(message) { this._log(4, message); }
  critical(message) { this._log(5, message); }
  getLevel() { return this._level; }
  setLevel(level) { this._level = Number(level); }
  setPattern(pattern) { this._pattern = String(pattern); }
  clearFormatters() { this._pattern = '%v'; }
  flush() {}
  drop() {
    this._dropped = true;
    if (registry.get(this._name) === this) registry.delete(this._name);
  }
}

async function createLogger(loggerType, name, filepath, maxFileSize, maxFiles) {
  return new Logger(loggerType, name, filepath, maxFileSize, maxFiles);
}

module.exports = {
  version: 11200,
  setLevel(level) {
    globalLevel = Number(level);
    for (const logger of registry.values()) logger.setLevel(globalLevel);
  },
  setFlushOn() {},
  Logger,
  LogLevel: { Trace: 0, Debug: 1, Info: 2, Warning: 3, Error: 4, Critical: 5, Off: 6 },
  createRotatingLogger(name, filepath, maxFileSize, maxFiles) {
    return createLogger('rotating', name, filepath, maxFileSize, maxFiles);
  },
  createAsyncRotatingLogger(name, filepath, maxFileSize, maxFiles) {
    return createLogger('rotating_async', name, filepath, maxFileSize, maxFiles);
  },
};
