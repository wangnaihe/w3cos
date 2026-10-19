'use strict';
// `@parcel/watcher` / `@w3cos/watcher` (M3 item 20): parcel's API over the
// runtime's recursive file observer (`__w3cos_node_host.watcher*`, see
// w3cos-runtime/src/node_watcher.rs and fs_watch.rs). Changes are polled
// on the shared host poll loop, filtered by `ignore`, coalesced per batch
// the way parcel coalesces them, and delivered as one callback per batch.
// Snapshots are a JSON listing written with `fs`.

const fs = require('fs');
const path = require('path');
const poll = require('node:internal/w3cos_poll');

const host = __w3cos_node_host;

function escapeRegExp(text) {
  return text.replace(/[.+^${}()|[\]\\]/g, '\\$&');
}

// Glob -> RegExp over '/'-separated paths relative to the watched dir.
function globToRegExp(glob) {
  let out = '';
  for (let i = 0; i < glob.length; i++) {
    const ch = glob[i];
    if (ch === '*') {
      if (glob[i + 1] === '*') {
        const slash = glob[i + 2] === '/';
        out += slash ? '(?:.*/)?' : '.*';
        i += slash ? 2 : 1;
      } else {
        out += '[^/]*';
      }
    } else if (ch === '?') {
      out += '[^/]';
    } else if (ch === '{') {
      const end = glob.indexOf('}', i);
      if (end < 0) { out += '\\{'; continue; }
      out += '(?:' + glob.slice(i + 1, end).split(',').map(escapeRegExp).join('|') + ')';
      i = end;
    } else if (ch === '[') {
      const end = glob.indexOf(']', i);
      if (end < 0) { out += '\\['; continue; }
      out += glob.slice(i, end + 1);
      i = end;
    } else {
      out += escapeRegExp(ch);
    }
  }
  return new RegExp('^' + out + '$');
}

const GLOB_CHARS = /[*?{[]/;

function makeIgnore(dir, ignore) {
  const prefixes = [];
  const patterns = [];
  for (const item of ignore || []) {
    if (item instanceof RegExp) patterns.push(item);
    else if (typeof item === 'string' && GLOB_CHARS.test(item)) patterns.push(globToRegExp(item));
    else if (typeof item === 'string') prefixes.push(path.resolve(dir, item));
  }
  return (file) => {
    for (const prefix of prefixes) {
      if (file === prefix || file.startsWith(prefix + path.sep)) return true;
    }
    if (patterns.length) {
      const rel = path.relative(dir, file).split(path.sep).join('/');
      for (const pattern of patterns) if (pattern.test(rel)) return true;
    }
    return false;
  };
}

// parcel's per-batch coalescing: create+delete -> nothing,
// create+update -> create, delete+create -> update.
function coalesce(events) {
  const byPath = new Map();
  for (const event of events) {
    const prev = byPath.get(event.path);
    if (!prev) {
      byPath.set(event.path, event.type);
    } else if (prev === 'create' && event.type === 'delete') {
      byPath.delete(event.path);
    } else if (prev === 'create') {
      // stays a create
    } else if (prev === 'delete' && event.type === 'create') {
      byPath.set(event.path, 'update');
    } else {
      byPath.set(event.path, event.type);
    }
  }
  return Array.from(byPath, ([file, type]) => ({ path: file, type }));
}

const subscriptions = new Set();

class Subscription {
  constructor(dir, fn, opts, id) {
    this.dir = dir;
    this.fn = fn;
    this.opts = opts;
    this._id = id;
    this._ignored = makeIgnore(dir, opts && opts.ignore);
    this._pollRef = true;
    this._closed = false;
    subscriptions.add(this);
    poll.add(this);
  }

  _poll() {
    if (this._closed) return false;
    const raw = host.watcherPoll(this._id);
    if (!raw.length) return false;
    const events = [];
    for (const [type, file, isDirectory] of raw) {
      // parcel reports content changes of files, not directory mtimes.
      if (type === 'update' && isDirectory) continue;
      if (this._ignored(file)) continue;
      events.push({ path: file, type });
    }
    const batch = coalesce(events);
    if (batch.length) {
      try {
        this.fn(null, batch);
      } catch (error) {
        process.nextTick(() => { throw error; });
      }
    }
    return true;
  }

  unsubscribe() {
    if (!this._closed) {
      this._closed = true;
      subscriptions.delete(this);
      poll.remove(this);
      host.watcherUnsubscribe(this._id);
    }
    return Promise.resolve();
  }
}

function subscribe(dir, fn, opts) {
  return new Promise((resolve, reject) => {
    if (typeof dir !== 'string') throw new TypeError('Expected a string');
    if (typeof fn !== 'function') throw new TypeError('Expected a function');
    const resolved = path.resolve(dir);
    const id = host.watcherSubscribe(resolved);
    if (typeof id === 'string') {
      reject(new Error(id));
      return;
    }
    const subscription = new Subscription(resolved, fn, opts || {}, id);
    resolve({ unsubscribe: () => subscription.unsubscribe() });
  });
}

function unsubscribe(dir, fn, opts) {
  const resolved = path.resolve(dir);
  for (const subscription of Array.from(subscriptions)) {
    if (subscription.dir === resolved && subscription.fn === fn) subscription.unsubscribe();
  }
  return Promise.resolve();
}

function listTree(dir, ignored, out) {
  let entries;
  try {
    entries = fs.readdirSync(dir, { withFileTypes: true });
  } catch (error) {
    return;
  }
  for (const entry of entries) {
    const file = path.join(dir, entry.name);
    if (ignored(file)) continue;
    let stat;
    try {
      stat = fs.statSync(file);
    } catch (error) {
      continue;
    }
    const isDirectory = stat.isDirectory();
    out[file] = [stat.mtimeMs, stat.size, isDirectory ? 1 : 0];
    if (isDirectory) listTree(file, ignored, out);
  }
}

function writeSnapshot(dir, snapshot, opts) {
  return new Promise((resolve) => {
    const resolved = path.resolve(dir);
    const files = {};
    listTree(resolved, makeIgnore(resolved, opts && opts.ignore), files);
    fs.writeFileSync(snapshot, JSON.stringify({ w3cosWatcherSnapshot: 1, dir: resolved, files }));
    resolve(null);
  });
}

function getEventsSince(dir, snapshot, opts) {
  return new Promise((resolve) => {
    const resolved = path.resolve(dir);
    const before = JSON.parse(fs.readFileSync(snapshot, 'utf8')).files || {};
    const after = {};
    listTree(resolved, makeIgnore(resolved, opts && opts.ignore), after);
    const events = [];
    for (const file of Object.keys(after)) {
      const old = before[file];
      if (!old) events.push({ path: file, type: 'create' });
      else if (!after[file][2] && (old[0] !== after[file][0] || old[1] !== after[file][1])) {
        events.push({ path: file, type: 'update' });
      }
    }
    for (const file of Object.keys(before)) {
      if (!after[file]) events.push({ path: file, type: 'delete' });
    }
    resolve(events);
  });
}

module.exports = { subscribe, unsubscribe, writeSnapshot, getEventsSince };
