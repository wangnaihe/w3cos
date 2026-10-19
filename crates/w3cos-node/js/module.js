'use strict';
// `module` / `node:module` for AOT apps (M3 item 23).
//
// An AOT app is linked at compile time, so there is no module loader to
// hook. The compiler turns literal calls of a `createRequire` result into
// static imports (`w3cos-compiler/src/create_require.rs`); what reaches this
// glue at run time is a computed specifier, which only a loader could serve,
// so it throws a coded error. The resolution hooks Code-OSS installs
// (`registerHooks`, `_resolveLookupPaths`, the compile cache) are accepted
// and have nothing to act on.

const builtinModules = [
  '_http_agent', '_http_client', '_http_common', '_http_incoming', '_http_outgoing',
  '_http_server', '_stream_duplex', '_stream_passthrough', '_stream_readable',
  '_stream_transform', '_stream_wrap', '_stream_writable', '_tls_common', '_tls_wrap',
  'assert', 'assert/strict', 'async_hooks', 'buffer', 'child_process', 'cluster', 'console',
  'constants', 'crypto', 'dgram', 'diagnostics_channel', 'dns', 'dns/promises', 'domain',
  'events', 'fs', 'fs/promises', 'http', 'http2', 'https', 'inspector', 'inspector/promises',
  'module', 'net', 'os', 'path', 'path/posix', 'path/win32', 'perf_hooks', 'process',
  'punycode', 'querystring', 'readline', 'readline/promises', 'repl', 'stream',
  'stream/consumers', 'stream/promises', 'stream/web', 'string_decoder', 'sys', 'timers',
  'timers/promises', 'tls', 'trace_events', 'tty', 'url', 'util', 'util/types', 'v8', 'vm',
  'wasi', 'worker_threads', 'zlib',
];
// Only reachable through the `node:` scheme.
const schemeOnly = ['node:sea', 'node:sqlite', 'node:test', 'node:test/reporters'];

function isBuiltin(name) {
  if (typeof name !== 'string') return false;
  if (schemeOnly.indexOf(name) !== -1) return true;
  const bare = name.indexOf('node:') === 0 ? name.slice(5) : name;
  return builtinModules.indexOf(bare) !== -1;
}

function dynamicRequireError(specifier) {
  const error = new Error(
    "Cannot require '" + specifier + "' at run time: an AOT app links its modules at compile time (use a literal specifier)"
  );
  error.code = 'ERR_W3COS_DYNAMIC_REQUIRE';
  return error;
}

function createRequire(filename) {
  if (typeof filename !== 'string' && !(filename && typeof filename.href === 'string')) {
    const error = new TypeError(
      "The argument 'filename' must be a file URL object, file URL string, or absolute path string. Received " + String(filename)
    );
    error.code = 'ERR_INVALID_ARG_VALUE';
    throw error;
  }
  function require(specifier) {
    throw dynamicRequireError(specifier);
  }
  require.resolve = function resolve(specifier) {
    if (isBuiltin(specifier)) return specifier;
    throw dynamicRequireError(specifier);
  };
  require.resolve.paths = function paths(specifier) {
    return isBuiltin(specifier) ? null : [];
  };
  require.cache = Module._cache;
  require.extensions = Module._extensions;
  require.main = undefined;
  return require;
}

function Module(id, parent) {
  this.id = id === undefined ? '' : id;
  this.path = '.';
  this.exports = {};
  this.filename = null;
  this.loaded = false;
  this.children = [];
  this.paths = [];
  this.parent = parent;
}

function nodeModulePaths(from) {
  const parts = String(from).split('/');
  const paths = [];
  for (let i = parts.length; i > 0; i--) {
    if (parts[i - 1] === 'node_modules') continue;
    const dir = parts.slice(0, i).join('/');
    paths.push((dir === '' ? '' : dir) + '/node_modules');
  }
  return paths;
}

Module.builtinModules = builtinModules;
Module.isBuiltin = isBuiltin;
Module.createRequire = createRequire;
Module.globalPaths = [];
Module._cache = Object.create(null);
Module._extensions = Object.create(null);
Module._pathCache = Object.create(null);
Module._nodeModulePaths = nodeModulePaths;
Module._resolveLookupPaths = function _resolveLookupPaths(request, parent) {
  if (isBuiltin(request)) return null;
  return parent && parent.paths ? parent.paths.slice() : [];
};
Module._resolveFilename = function _resolveFilename(request) {
  if (isBuiltin(request)) return request;
  throw dynamicRequireError(request);
};
Module._load = function _load(request) {
  throw dynamicRequireError(request);
};
Module.register = function register() {};
Module.registerHooks = function registerHooks() {
  return { deregister: function deregister() {} };
};
Module.syncBuiltinESMExports = function syncBuiltinESMExports() {};
Module.findSourceMap = function findSourceMap() { return undefined; };
Module.constants = { compileCacheStatus: { FAILED: 0, ENABLED: 1, ALREADY_ENABLED: 2, DISABLED: 3 } };
// AOT code is already native: there is no compile cache to enable.
Module.enableCompileCache = function enableCompileCache() {
  return { status: Module.constants.compileCacheStatus.DISABLED, message: 'AOT-compiled app' };
};
Module.flushCompileCache = function flushCompileCache() {};
Module.getCompileCacheDir = function getCompileCacheDir() { return undefined; };
Module.Module = Module;

module.exports = Module;
