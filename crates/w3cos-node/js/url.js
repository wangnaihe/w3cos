'use strict';
// `url`: WHATWG URL / URLSearchParams from the runtime plus Node's file URL
// helpers and the legacy `format` for URL objects.

const path = require('path');

function invalidArg(name, expected, value) {
  const error = new TypeError('The "' + name + '" argument must be ' + expected + '. Received ' + (value === null ? 'null' : typeof value));
  error.code = 'ERR_INVALID_ARG_TYPE';
  return error;
}

function fileURLToPath(input) {
  let url = input;
  if (typeof input === 'string') url = new URL(input);
  else if (input === null || typeof input !== 'object' || typeof input.href !== 'string') {
    throw invalidArg('path', 'of type string or an instance of URL', input);
  }
  if (url.protocol !== 'file:') {
    const error = new TypeError('The URL must be of scheme file');
    error.code = 'ERR_INVALID_URL_SCHEME';
    throw error;
  }
  if (url.hostname !== '') {
    const error = new TypeError('File URL host must be "localhost" or empty on linux');
    error.code = 'ERR_INVALID_FILE_URL_HOST';
    throw error;
  }
  const pathname = url.pathname;
  for (let i = 0; i < pathname.length; i++) {
    if (pathname.charAt(i) === '%') {
      const third = pathname.charAt(i + 2).toLowerCase();
      if (pathname.charAt(i + 1) === '2' && third === 'f') {
        const error = new TypeError('File URL path must not include encoded / characters');
        error.code = 'ERR_INVALID_FILE_URL_PATH';
        throw error;
      }
    }
  }
  return decodeURIComponent(pathname);
}

function pathToFileURL(filepath) {
  let resolved = path.resolve(filepath);
  if (filepath.charCodeAt(filepath.length - 1) === 47 && resolved.charCodeAt(resolved.length - 1) !== 47) {
    resolved += '/';
  }
  let encoded = '';
  for (let i = 0; i < resolved.length; i++) {
    const ch = resolved.charAt(i);
    if (ch === '%') encoded += '%25';
    else if (ch === '\\') encoded += '%5C';
    else if (ch === '\n') encoded += '%0A';
    else if (ch === '\r') encoded += '%0D';
    else if (ch === '\t') encoded += '%09';
    else if (ch === '#') encoded += '%23';
    else if (ch === '?') encoded += '%3F';
    else if (ch === ' ') encoded += '%20';
    else if (ch === '"') encoded += '%22';
    else if (ch === '<') encoded += '%3C';
    else if (ch === '>') encoded += '%3E';
    else if (ch === '`') encoded += '%60';
    else if (ch === '{') encoded += '%7B';
    else if (ch === '}') encoded += '%7D';
    else encoded += ch;
  }
  return new URL('file://' + encoded);
}

function urlToHttpOptions(url) {
  const options = {
    protocol: url.protocol,
    hostname: url.hostname.charAt(0) === '[' ? url.hostname.slice(1, -1) : url.hostname,
    hash: url.hash,
    search: url.search,
    pathname: url.pathname,
    path: (url.pathname || '') + (url.search || ''),
    href: url.href,
  };
  if (url.port !== '') options.port = Number(url.port);
  if (url.username || url.password) options.auth = decodeURIComponent(url.username) + ':' + decodeURIComponent(url.password);
  return options;
}

function format(urlObject, options) {
  if (typeof urlObject === 'string') return urlObject;
  if (typeof URL === 'function' && urlObject instanceof URL) {
    let out = urlObject.href;
    if (options && options.fragment === false) out = out.split('#')[0];
    if (options && options.search === false) out = out.replace(/\?[^#]*/, '');
    return out;
  }
  if (urlObject === null || typeof urlObject !== 'object') throw invalidArg('urlObject', 'one of type object or string', urlObject);
  let protocol = urlObject.protocol || '';
  if (protocol && protocol.charAt(protocol.length - 1) !== ':') protocol += ':';
  let auth = urlObject.auth ? encodeURIComponent(urlObject.auth).replace(/%3A/i, ':') + '@' : '';
  let host = '';
  if (urlObject.host) host = auth + urlObject.host;
  else if (urlObject.hostname) host = auth + urlObject.hostname + (urlObject.port ? ':' + urlObject.port : '');
  let pathname = urlObject.pathname || '';
  let search = urlObject.search || '';
  if (!search && urlObject.query && typeof urlObject.query === 'object') {
    // `querystring.stringify` encoding (%20, not URLSearchParams' '+').
    const keys = Object.keys(urlObject.query);
    const pairs = [];
    for (let i = 0; i < keys.length; i++) {
      const value = urlObject.query[keys[i]];
      const values = Array.isArray(value) ? value : [value];
      for (let j = 0; j < values.length; j++) {
        const item = values[j];
        const text = typeof item === 'string' ? item
          : (typeof item === 'number' && isFinite(item)) || typeof item === 'boolean' || typeof item === 'bigint' ? String(item) : '';
        pairs.push(encodeURIComponent(keys[i]) + '=' + encodeURIComponent(text));
      }
    }
    const query = pairs.join('&');
    if (query) search = '?' + query;
  }
  if (search && search.charAt(0) !== '?') search = '?' + search;
  let hash = urlObject.hash || '';
  if (hash && hash.charAt(0) !== '#') hash = '#' + hash;
  const slashes = urlObject.slashes || /^(https?|ftp|gopher|file|wss?):$/.test(protocol);
  if (slashes || host) {
    host = '//' + host;
    if (pathname && pathname.charAt(0) !== '/') pathname = '/' + pathname;
  }
  return protocol + host + pathname + search + hash;
}

// Legacy `url.parse` / `url.resolve` (Node's `Url`), still used by
// @vscode/proxy-agent and older packages. Mirrors Node's lib/url.js for
// the common shapes: protocol, auth, host/port, path, query, hash.
const SLASHED_PROTOCOLS = { 'http:': true, 'https:': true, 'ftp:': true, 'gopher:': true, 'file:': true, 'ws:': true, 'wss:': true };
const HOSTLESS_PROTOCOLS = { 'javascript:': true };
const AUTO_ESCAPE = { ' ': '%20', '\t': '%09', '\n': '%0A', '\r': '%0D', '"': '%22', "'": '%27', '<': '%3C', '>': '%3E', '\\': '%5C', '^': '%5E', '`': '%60', '{': '%7B', '|': '%7C', '}': '%7D' };

function Url() {
  this.protocol = null;
  this.slashes = null;
  this.auth = null;
  this.host = null;
  this.port = null;
  this.hostname = null;
  this.hash = null;
  this.search = null;
  this.query = null;
  this.pathname = null;
  this.path = null;
  this.href = null;
}

function autoEscape(text) {
  let out = '';
  for (let i = 0; i < text.length; i++) {
    const ch = text.charAt(i);
    out += AUTO_ESCAPE[ch] !== undefined ? AUTO_ESCAPE[ch] : ch;
  }
  return out;
}

Url.prototype.parse = function parse(input, parseQueryString, slashesDenoteHost) {
  if (typeof input !== 'string') throw invalidArg('url', 'of type string', input);
  let rest = input.trim();
  const hashAt = rest.indexOf('#');
  let hash = null;
  if (hashAt !== -1) {
    hash = rest.slice(hashAt);
    rest = rest.slice(0, hashAt);
  }
  const queryAt = rest.indexOf('?');
  let search = null;
  if (queryAt !== -1) {
    search = rest.slice(queryAt);
    rest = rest.slice(0, queryAt);
  }
  const protoMatch = /^[a-zA-Z0-9.+-]+:/.exec(rest);
  let lowerProto = null;
  if (protoMatch) {
    lowerProto = protoMatch[0].toLowerCase();
    this.protocol = lowerProto;
    rest = rest.slice(protoMatch[0].length);
  }
  let slashes = false;
  if (slashesDenoteHost || lowerProto || /^\/\/[^@/]+@[^@/]+/.test(rest)) {
    slashes = rest.slice(0, 2) === '//';
    if (slashes && !(lowerProto && HOSTLESS_PROTOCOLS[lowerProto])) {
      rest = rest.slice(2);
      this.slashes = true;
    }
  }
  if (!(lowerProto && HOSTLESS_PROTOCOLS[lowerProto]) && (slashes || (lowerProto && !SLASHED_PROTOCOLS[lowerProto]))) {
    let hostEnd = rest.length;
    for (let i = 0; i < rest.length; i++) {
      const ch = rest.charAt(i);
      if (ch === '/' || ch === '\\' || ch === ' ' || ch === '\t' || ch === '\n' || ch === '\r' || ch === '<' || ch === '>' || ch === '"' || ch === '`' || ch === '{' || ch === '}' || ch === '|' || ch === '^' || ch === "'") {
        hostEnd = i;
        break;
      }
    }
    const atSign = rest.lastIndexOf('@', hostEnd);
    if (atSign !== -1) {
      this.auth = decodeURIComponent(rest.slice(0, atSign));
      rest = rest.slice(atSign + 1);
      hostEnd -= atSign + 1;
    }
    let host = rest.slice(0, hostEnd);
    rest = rest.slice(hostEnd);
    const portMatch = /:[0-9]*$/.exec(host);
    if (portMatch) {
      if (portMatch[0] !== ':') this.port = portMatch[0].slice(1);
      host = host.slice(0, host.length - portMatch[0].length);
    }
    let hostname = host.toLowerCase();
    const ipv6 = hostname.charAt(0) === '[' && hostname.charAt(hostname.length - 1) === ']';
    if (ipv6) hostname = hostname.slice(1, -1);
    this.hostname = hostname;
    this.host = (ipv6 ? '[' + hostname + ']' : hostname) + (this.port ? ':' + this.port : '');
  }
  rest = autoEscape(rest);
  if (hash !== null) this.hash = autoEscape(hash);
  if (search !== null) {
    this.search = autoEscape(search);
    this.query = this.search.slice(1);
    if (parseQueryString) this.query = require('querystring').parse(this.query);
  } else if (parseQueryString) {
    this.query = Object.create(null);
  }
  if (rest) this.pathname = rest;
  if (lowerProto && SLASHED_PROTOCOLS[lowerProto] && this.hostname && !this.pathname) this.pathname = '/';
  if (this.pathname || this.search) this.path = (this.pathname || '') + (this.search || '');
  this.href = this.format();
  return this;
};

Url.prototype.format = function () {
  let auth = this.auth || '';
  if (auth) auth = encodeURIComponent(auth).replace(/%3A/i, ':') + '@';
  const protocol = this.protocol || '';
  let pathname = this.pathname || '';
  let hash = this.hash || '';
  let host = '';
  if (this.host) host = auth + this.host;
  else if (this.hostname) host = auth + (this.hostname.indexOf(':') !== -1 ? '[' + this.hostname + ']' : this.hostname) + (this.port ? ':' + this.port : '');
  let search = this.search || '';
  if (!search && this.query && typeof this.query === 'object') {
    const query = require('querystring').stringify(this.query);
    if (query) search = '?' + query;
  }
  if (this.slashes || ((!protocol || SLASHED_PROTOCOLS[protocol]) && host)) {
    host = '//' + host;
    if (pathname && pathname.charAt(0) !== '/') pathname = '/' + pathname;
  }
  if (hash && hash.charAt(0) !== '#') hash = '#' + hash;
  if (search && search.charAt(0) !== '?') search = '?' + search;
  return protocol + host + pathname.replace(/[?#]/g, encodeURIComponent) + search.replace('#', '%23') + hash;
};

Url.prototype.resolve = function (relative) {
  return resolve(this.href, relative);
};

function parse(input, parseQueryString, slashesDenoteHost) {
  if (input instanceof Url) return input;
  return new Url().parse(input, parseQueryString, slashesDenoteHost);
}

function resolve(from, to) {
  // Node's documented WHATWG equivalent of the legacy resolver.
  const base = new URL(from, 'resolve://');
  const resolved = new URL(to, base);
  if (resolved.protocol === 'resolve:') {
    const { pathname, search, hash } = resolved;
    return pathname + search + hash;
  }
  return resolved.toString();
}

module.exports = {
  URL: typeof URL === 'function' ? URL : undefined,
  URLSearchParams: typeof URLSearchParams === 'function' ? URLSearchParams : undefined,
  fileURLToPath: fileURLToPath,
  pathToFileURL: pathToFileURL,
  urlToHttpOptions: urlToHttpOptions,
  format: function (urlObject, options) {
    if (typeof urlObject === 'string') return parse(urlObject).format();
    if (urlObject instanceof Url) return urlObject.format();
    return format(urlObject, options);
  },
  parse: parse,
  resolve: resolve,
  Url: Url,
  domainToASCII: function (domain) { return String(domain).toLowerCase(); },
  domainToUnicode: function (domain) { return String(domain); },
};
