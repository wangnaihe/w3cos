'use strict';
// `module` / `node:module` for AOT apps (M3 item 23).
//
// An AOT app is linked at compile time, so there is no module loader to
// hook. The compiler turns literal calls of a `createRequire` result into
// static imports (`w3cos-compiler/src/create_require.rs`); what reaches this
// glue at run time is a computed specifier. An absolute or relative path to a
// module of the compiled graph (an app's run-time roots, `package.json`
// `"w3cos": { "modules": [...] }`, e.g. Code-OSS extension entry points
// loaded through `createRequire(import.meta.url)(fsPath)`) is served from
// it; anything else would need a loader, so it throws a coded error. As in
// Node, `require` goes through `Module._load`, so loaders patched over it
// (the Code-OSS extension host's `vscode` interceptor) see every request.
// The other resolution hooks (`registerHooks`, `_resolveLookupPaths`, the
// compile cache) are accepted and have nothing to act on.

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

const host = typeof __w3cos_node_host === 'undefined' ? undefined : __w3cos_node_host;

function filePath(value) {
  let path = String(value && typeof value.href === 'string' ? value.href : value);
  if (path.indexOf('file://') === 0) {
    path = path.slice(7);
    const hash = path.search(/[?#]/);
    if (hash !== -1) path = path.slice(0, hash);
    try {
      path = decodeURIComponent(path);
    } catch (_) {}
  }
  return path;
}

function normalizePath(path) {
  const segments = [];
  const parts = path.split('/');
  for (let i = 0; i < parts.length; i++) {
    const part = parts[i];
    if (part === '' || part === '.') continue;
    if (part === '..') segments.pop();
    else segments.push(part);
  }
  return '/' + segments.join('/');
}

function dirnameOf(path) {
  const slash = path.lastIndexOf('/');
  return slash <= 0 ? '/' : path.slice(0, slash);
}

const compiledSuffixes = ['', '.js', '.cjs', '.mjs', '.json', '/index.js', '/index.cjs', '/index.mjs', '/index.json'];

// The compiled module path `request` names from `parentFilename`, or undefined.
function resolveCompiled(request, parentFilename) {
  if (!host || typeof host.compiledModule !== 'function' || typeof request !== 'string') return undefined;
  let base;
  const spec = request.indexOf('file://') === 0 ? filePath(request) : request;
  if (spec.charAt(0) === '/') {
    base = spec;
  } else if (spec === '.' || spec === '..' || spec.indexOf('./') === 0 || spec.indexOf('../') === 0) {
    if (typeof parentFilename !== 'string' || parentFilename.charAt(0) !== '/') return undefined;
    base = dirnameOf(parentFilename) + '/' + spec;
  } else {
    return undefined;
  }
  base = normalizePath(base);
  for (let i = 0; i < compiledSuffixes.length; i++) {
    const candidate = base + compiledSuffixes[i];
    if (host.compiledModule(candidate)) return candidate;
  }
  return undefined;
}

function parentFilenameOf(parent) {
  return parent && typeof parent.filename === 'string' ? parent.filename : undefined;
}

function createRequire(filename) {
  if (typeof filename !== 'string' && !(filename && typeof filename.href === 'string')) {
    const error = new TypeError(
      "The argument 'filename' must be a file URL object, file URL string, or absolute path string. Received " + String(filename)
    );
    error.code = 'ERR_INVALID_ARG_VALUE';
    throw error;
  }
  const parent = new Module(filePath(filename), undefined);
  parent.filename = parent.id;
  parent.path = dirnameOf(parent.id);
  parent.paths = nodeModulePaths(parent.path);
  function require(specifier) {
    return Module._load(specifier, parent, false);
  }
  require.resolve = function resolve(specifier) {
    return Module._resolveFilename(specifier, parent, false);
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
Module._resolveFilename = function _resolveFilename(request, parent) {
  if (isBuiltin(request)) return request;
  const resolved = resolveCompiled(request, parentFilenameOf(parent));
  if (resolved !== undefined) return resolved;
  throw dynamicRequireError(request);
};
// The request a compiled CommonJS module could not link (see below): if it
// reaches the default loader unserved, the module's own "Cannot find module"
// error is thrown instead of a dynamic-require one.
let unlinkedRequest;
const unlinked = {};
Module._load = function _load(request, parent) {
  const resolved = resolveCompiled(request, parentFilenameOf(parent));
  if (resolved === undefined) {
    if (unlinkedRequest !== undefined && unlinkedRequest.request === request && unlinkedRequest.parent === parent) {
      throw unlinked;
    }
    throw dynamicRequireError(request);
  }
  return host.requireCompiled(resolved);
};
// A compiled CommonJS module's `require()` of a specifier its build could not
// link (`require("vscode")` in a Code-OSS extension) goes through
// `Module._load` like in Node, so a loader patched over it can serve it.
// Returns `{ value }`, or undefined when nothing did.
Object.defineProperty(globalThis, '__w3cos_cjs_runtime_load', {
  configurable: true,
  writable: true,
  value: function runtimeLoad(request, parent) {
    const previous = unlinkedRequest;
    unlinkedRequest = { request: request, parent: parent };
    try {
      return { value: Module._load(request, parent, false) };
    } catch (error) {
      if (error === unlinked) return undefined;
      throw error;
    } finally {
      unlinkedRequest = previous;
    }
  },
});
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
