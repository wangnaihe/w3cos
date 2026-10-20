'use strict';
// Worker boot: which worker module this thread runs (null on the main
// thread), and the worker files the compiler linked into the program entry.
// The entry's CommonJS record calls `compiled()` and `entry()` first (see
// the compiler's `worker_boot_prelude`).

const host = __w3cos_node_host;

const context = host.workerContext();
let compiledPaths = null;

exports.context = context;

exports.compiled = function compiled(paths) {
  if (compiledPaths === null) compiledPaths = new Set();
  for (const path of paths) compiledPaths.add(path);
};

exports.isCompiled = function isCompiled(path) {
  return compiledPaths !== null && compiledPaths.has(path);
};

// Electron `utilityProcess` children see `process.type === 'utility'`,
// their fork arguments in argv, and `process.parentPort` (message events
// carry `{ data, ports }`).
function setUpUtilityProcess() {
  const proc = require('process');
  const EventEmitter = require('events');
  const threads = require('worker_threads');
  const data = threads.workerData || {};
  proc.type = 'utility';
  proc.argv = [proc.execPath, context.path].concat(Array.isArray(data.args) ? data.args : []);
  const port = new EventEmitter();
  port.postMessage = function postMessage(message) {
    threads.parentPort.postMessage(message);
  };
  threads.parentPort.on('message', (message) => port.emit('message', { data: message, ports: [] }));
  proc.parentPort = port;
}

exports.entry = function entry() {
  if (context === null) return undefined;
  if (context.kind === 'utility') setUpUtilityProcess();
  return context.path;
};

exports.missing = function missing(path) {
  const error = new Error("Cannot find module '" + path + "'");
  error.code = 'MODULE_NOT_FOUND';
  throw error;
};
