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

module.exports = {
  URL: typeof URL === 'function' ? URL : undefined,
  URLSearchParams: typeof URLSearchParams === 'function' ? URLSearchParams : undefined,
  fileURLToPath: fileURLToPath,
  pathToFileURL: pathToFileURL,
  urlToHttpOptions: urlToHttpOptions,
  format: format,
  domainToASCII: function (domain) { return String(domain).toLowerCase(); },
  domainToUnicode: function (domain) { return String(domain); },
};
