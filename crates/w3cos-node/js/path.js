'use strict';
// POSIX `path`, following Node's lib/path.js (MIT) behaviour.

function assertPath(path, name) {
  if (typeof path !== 'string') {
    const error = new TypeError('The "' + (name || 'path') + '" argument must be of type string. Received ' + describe(path));
    error.code = 'ERR_INVALID_ARG_TYPE';
    throw error;
  }
}

function describe(value) {
  if (value === null) return 'null';
  if (value === undefined) return 'undefined';
  if (typeof value === 'function') return 'function ' + (value.name || '<anonymous>');
  if (typeof value === 'object') return 'an instance of ' + ((value.constructor && value.constructor.name) || 'Object');
  return 'type ' + typeof value + ' (' + String(value) + ')';
}

// Resolves . and .. elements in a path with directory names.
function normalizeString(path, allowAboveRoot) {
  let res = '';
  let lastSegmentLength = 0;
  let lastSlash = -1;
  let dots = 0;
  let code = 0;
  for (let i = 0; i <= path.length; ++i) {
    if (i < path.length) code = path.charCodeAt(i);
    else if (code === 47) break;
    else code = 47;
    if (code === 47) {
      if (lastSlash === i - 1 || dots === 1) {
        // NOOP
      } else if (dots === 2) {
        if (res.length < 2 || lastSegmentLength !== 2 ||
            res.charCodeAt(res.length - 1) !== 46 ||
            res.charCodeAt(res.length - 2) !== 46) {
          if (res.length > 2) {
            const lastSlashIndex = res.lastIndexOf('/');
            if (lastSlashIndex === -1) {
              res = '';
              lastSegmentLength = 0;
            } else {
              res = res.slice(0, lastSlashIndex);
              lastSegmentLength = res.length - 1 - res.lastIndexOf('/');
            }
            lastSlash = i;
            dots = 0;
            continue;
          } else if (res.length !== 0) {
            res = '';
            lastSegmentLength = 0;
            lastSlash = i;
            dots = 0;
            continue;
          }
        }
        if (allowAboveRoot) {
          res += res.length > 0 ? '/..' : '..';
          lastSegmentLength = 2;
        }
      } else {
        if (res.length > 0) res += '/' + path.slice(lastSlash + 1, i);
        else res = path.slice(lastSlash + 1, i);
        lastSegmentLength = i - lastSlash - 1;
      }
      lastSlash = i;
      dots = 0;
    } else if (code === 46 && dots !== -1) {
      ++dots;
    } else {
      dots = -1;
    }
  }
  return res;
}

function cwd() {
  const proc = typeof process === 'object' && process !== null ? process : null;
  if (proc && typeof proc.cwd === 'function') {
    const dir = proc.cwd();
    if (typeof dir === 'string' && dir.length > 0) return dir;
  }
  return '/';
}

const posix = {
  sep: '/',
  delimiter: ':',

  resolve: function resolve() {
    let resolvedPath = '';
    let resolvedAbsolute = false;
    for (let i = arguments.length - 1; i >= -1 && !resolvedAbsolute; i--) {
      const path = i >= 0 ? arguments[i] : cwd();
      assertPath(path, 'paths[' + i + ']');
      if (path.length === 0) continue;
      resolvedPath = path + '/' + resolvedPath;
      resolvedAbsolute = path.charCodeAt(0) === 47;
    }
    resolvedPath = normalizeString(resolvedPath, !resolvedAbsolute);
    if (resolvedAbsolute) return '/' + resolvedPath;
    return resolvedPath.length > 0 ? resolvedPath : '.';
  },

  normalize: function normalize(path) {
    assertPath(path);
    if (path.length === 0) return '.';
    const isAbsolute = path.charCodeAt(0) === 47;
    const trailingSeparator = path.charCodeAt(path.length - 1) === 47;
    path = normalizeString(path, !isAbsolute);
    if (path.length === 0) {
      if (isAbsolute) return '/';
      return trailingSeparator ? './' : '.';
    }
    if (trailingSeparator) path += '/';
    return isAbsolute ? '/' + path : path;
  },

  isAbsolute: function isAbsolute(path) {
    assertPath(path);
    return path.length > 0 && path.charCodeAt(0) === 47;
  },

  join: function join() {
    if (arguments.length === 0) return '.';
    let joined;
    for (let i = 0; i < arguments.length; ++i) {
      const arg = arguments[i];
      assertPath(arg, 'paths[' + i + ']');
      if (arg.length > 0) {
        if (joined === undefined) joined = arg;
        else joined += '/' + arg;
      }
    }
    if (joined === undefined) return '.';
    return posix.normalize(joined);
  },

  relative: function relative(from, to) {
    assertPath(from, 'from');
    assertPath(to, 'to');
    if (from === to) return '';
    from = posix.resolve(from);
    to = posix.resolve(to);
    if (from === to) return '';
    const fromStart = 1;
    const fromEnd = from.length;
    const fromLen = fromEnd - fromStart;
    const toStart = 1;
    const toLen = to.length - toStart;
    const length = fromLen < toLen ? fromLen : toLen;
    let lastCommonSep = -1;
    let i = 0;
    for (; i < length; i++) {
      const fromCode = from.charCodeAt(fromStart + i);
      if (fromCode !== to.charCodeAt(toStart + i)) break;
      else if (fromCode === 47) lastCommonSep = i;
    }
    if (i === length) {
      if (toLen > length) {
        if (to.charCodeAt(toStart + i) === 47) return to.slice(toStart + i + 1);
        if (i === 0) return to.slice(toStart + i);
      } else if (fromLen > length) {
        if (from.charCodeAt(fromStart + i) === 47) lastCommonSep = i;
        else if (i === 0) lastCommonSep = 0;
      }
    }
    let out = '';
    for (i = fromStart + lastCommonSep + 1; i <= fromEnd; ++i) {
      if (i === fromEnd || from.charCodeAt(i) === 47) {
        out += out.length === 0 ? '..' : '/..';
      }
    }
    return out + to.slice(toStart + lastCommonSep);
  },

  toNamespacedPath: function toNamespacedPath(path) {
    return path;
  },

  dirname: function dirname(path) {
    assertPath(path);
    if (path.length === 0) return '.';
    const hasRoot = path.charCodeAt(0) === 47;
    let end = -1;
    let matchedSlash = true;
    for (let i = path.length - 1; i >= 1; --i) {
      if (path.charCodeAt(i) === 47) {
        if (!matchedSlash) {
          end = i;
          break;
        }
      } else {
        matchedSlash = false;
      }
    }
    if (end === -1) return hasRoot ? '/' : '.';
    if (hasRoot && end === 1) return '//';
    return path.slice(0, end);
  },

  basename: function basename(path, suffix) {
    if (suffix !== undefined) assertPath(suffix, 'suffix');
    assertPath(path);
    let start = 0;
    let end = -1;
    let matchedSlash = true;
    let i;
    if (suffix !== undefined && suffix.length > 0 && suffix.length <= path.length) {
      if (suffix === path) return '';
      let extIdx = suffix.length - 1;
      let firstNonSlashEnd = -1;
      for (i = path.length - 1; i >= 0; --i) {
        const code = path.charCodeAt(i);
        if (code === 47) {
          if (!matchedSlash) {
            start = i + 1;
            break;
          }
        } else {
          if (firstNonSlashEnd === -1) {
            matchedSlash = false;
            firstNonSlashEnd = i + 1;
          }
          if (extIdx >= 0) {
            if (code === suffix.charCodeAt(extIdx)) {
              if (--extIdx === -1) end = i;
            } else {
              extIdx = -1;
              end = firstNonSlashEnd;
            }
          }
        }
      }
      if (start === end) end = firstNonSlashEnd;
      else if (end === -1) end = path.length;
      return path.slice(start, end);
    }
    for (i = path.length - 1; i >= 0; --i) {
      if (path.charCodeAt(i) === 47) {
        if (!matchedSlash) {
          start = i + 1;
          break;
        }
      } else if (end === -1) {
        matchedSlash = false;
        end = i + 1;
      }
    }
    if (end === -1) return '';
    return path.slice(start, end);
  },

  extname: function extname(path) {
    assertPath(path);
    let startDot = -1;
    let startPart = 0;
    let end = -1;
    let matchedSlash = true;
    let preDotState = 0;
    for (let i = path.length - 1; i >= 0; --i) {
      const code = path.charCodeAt(i);
      if (code === 47) {
        if (!matchedSlash) {
          startPart = i + 1;
          break;
        }
        continue;
      }
      if (end === -1) {
        matchedSlash = false;
        end = i + 1;
      }
      if (code === 46) {
        if (startDot === -1) startDot = i;
        else if (preDotState !== 1) preDotState = 1;
      } else if (startDot !== -1) {
        preDotState = -1;
      }
    }
    if (startDot === -1 || end === -1 || preDotState === 0 ||
        (preDotState === 1 && startDot === end - 1 && startDot === startPart + 1)) {
      return '';
    }
    return path.slice(startDot, end);
  },

  format: function format(pathObject) {
    if (pathObject === null || typeof pathObject !== 'object') {
      const error = new TypeError('The "pathObject" argument must be of type object. Received ' + describe(pathObject));
      error.code = 'ERR_INVALID_ARG_TYPE';
      throw error;
    }
    const dir = pathObject.dir || pathObject.root;
    const base = pathObject.base ||
      ((pathObject.name || '') + (pathObject.ext ? (pathObject.ext.charAt(0) === '.' ? '' : '.') + pathObject.ext : ''));
    if (!dir) return base;
    return dir === pathObject.root ? dir + base : dir + '/' + base;
  },

  parse: function parse(path) {
    assertPath(path);
    const ret = { root: '', dir: '', base: '', ext: '', name: '' };
    if (path.length === 0) return ret;
    const isAbsolute = path.charCodeAt(0) === 47;
    let start;
    if (isAbsolute) {
      ret.root = '/';
      start = 1;
    } else {
      start = 0;
    }
    let startDot = -1;
    let startPart = 0;
    let end = -1;
    let matchedSlash = true;
    let i = path.length - 1;
    let preDotState = 0;
    for (; i >= start; --i) {
      const code = path.charCodeAt(i);
      if (code === 47) {
        if (!matchedSlash) {
          startPart = i + 1;
          break;
        }
        continue;
      }
      if (end === -1) {
        matchedSlash = false;
        end = i + 1;
      }
      if (code === 46) {
        if (startDot === -1) startDot = i;
        else if (preDotState !== 1) preDotState = 1;
      } else if (startDot !== -1) {
        preDotState = -1;
      }
    }
    if (end !== -1) {
      const start2 = startPart === 0 && isAbsolute ? 1 : startPart;
      if (startDot === -1 || preDotState === 0 ||
          (preDotState === 1 && startDot === end - 1 && startDot === startPart + 1)) {
        ret.base = ret.name = path.slice(start2, end);
      } else {
        ret.name = path.slice(start2, startDot);
        ret.base = path.slice(start2, end);
        ret.ext = path.slice(startDot, end);
      }
    }
    if (startPart > 0) ret.dir = path.slice(0, startPart - 1);
    else if (isAbsolute) ret.dir = '/';
    return ret;
  },
};

posix.posix = posix;
// Windows paths are not supported on W3C OS; `path.win32` aliases POSIX.
posix.win32 = posix;

module.exports = posix;
