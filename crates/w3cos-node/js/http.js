'use strict';
// `http`: protocol tables, `Agent`, the message classes and an HTTP/1.1
// server over `net.Server` (TCP or Unix socket): requests with
// Content-Length or chunked bodies, keep-alive, and `upgrade` hand-off of the
// raw socket (what `ws.WebSocketServer({ server })` needs). Client requests
// are not implemented yet (M3 item 23) and throw ERR_W3COS_UNSUPPORTED.

const EventEmitter = require('events');
const stream = require('stream');
const { unsupported } = require('node:internal/w3cos_unsupported');
const net = require('net');
const { Buffer } = require('buffer');
const process = require('process');

const METHODS = [
  'ACL', 'BIND', 'CHECKOUT', 'CONNECT', 'COPY', 'DELETE', 'GET', 'HEAD', 'LINK', 'LOCK',
  'M-SEARCH', 'MERGE', 'MKACTIVITY', 'MKCALENDAR', 'MKCOL', 'MOVE', 'NOTIFY', 'OPTIONS',
  'PATCH', 'POST', 'PROPFIND', 'PROPPATCH', 'PURGE', 'PUT', 'QUERY', 'REBIND', 'REPORT',
  'SEARCH', 'SOURCE', 'SUBSCRIBE', 'TRACE', 'UNBIND', 'UNLINK', 'UNLOCK', 'UNSUBSCRIBE',
];

const STATUS_CODES = {
  100: 'Continue', 101: 'Switching Protocols', 102: 'Processing', 103: 'Early Hints',
  200: 'OK', 201: 'Created', 202: 'Accepted', 203: 'Non-Authoritative Information', 204: 'No Content',
  205: 'Reset Content', 206: 'Partial Content', 207: 'Multi-Status', 208: 'Already Reported', 226: 'IM Used',
  300: 'Multiple Choices', 301: 'Moved Permanently', 302: 'Found', 303: 'See Other', 304: 'Not Modified',
  305: 'Use Proxy', 307: 'Temporary Redirect', 308: 'Permanent Redirect',
  400: 'Bad Request', 401: 'Unauthorized', 402: 'Payment Required', 403: 'Forbidden', 404: 'Not Found',
  405: 'Method Not Allowed', 406: 'Not Acceptable', 407: 'Proxy Authentication Required', 408: 'Request Timeout',
  409: 'Conflict', 410: 'Gone', 411: 'Length Required', 412: 'Precondition Failed', 413: 'Payload Too Large',
  414: 'URI Too Long', 415: 'Unsupported Media Type', 416: 'Range Not Satisfiable', 417: 'Expectation Failed',
  418: "I'm a Teapot", 421: 'Misdirected Request', 422: 'Unprocessable Entity', 423: 'Locked',
  424: 'Failed Dependency', 425: 'Too Early', 426: 'Upgrade Required', 428: 'Precondition Required',
  429: 'Too Many Requests', 431: 'Request Header Fields Too Large', 451: 'Unavailable For Legal Reasons',
  500: 'Internal Server Error', 501: 'Not Implemented', 502: 'Bad Gateway', 503: 'Service Unavailable',
  504: 'Gateway Timeout', 505: 'HTTP Version Not Supported', 506: 'Variant Also Negotiates',
  507: 'Insufficient Storage', 508: 'Loop Detected', 509: 'Bandwidth Limit Exceeded', 510: 'Not Extended',
  511: 'Network Authentication Required',
};

const maxHeaderSize = 16384;

class Agent extends EventEmitter {
  constructor(options) {
    super();
    this.options = Object.assign({ path: null }, options);
    this.defaultPort = 80;
    this.protocol = 'http:';
    this.keepAlive = !!this.options.keepAlive;
    this.maxSockets = this.options.maxSockets || Infinity;
    this.maxFreeSockets = this.options.maxFreeSockets || 256;
    this.requests = Object.create(null);
    this.sockets = Object.create(null);
    this.freeSockets = Object.create(null);
  }
  createConnection() { throw unsupported('http.Agent.createConnection'); }
  getName(options) {
    options = options || {};
    return (options.host || 'localhost') + ':' + (options.port || '') + ':' + (options.localAddress || '');
  }
  addRequest() { throw unsupported('http.Agent.addRequest'); }
  destroy() {}
}

class OutgoingMessage extends stream.Writable {}
class ClientRequest extends OutgoingMessage {}

class IncomingMessage extends stream.Readable {
  constructor(socket) {
    super();
    this.socket = socket;
    this.connection = socket;
    this.httpVersion = '1.1';
    this.httpVersionMajor = 1;
    this.httpVersionMinor = 1;
    this.method = null;
    this.url = '';
    this.headers = {};
    this.rawHeaders = [];
    this.trailers = {};
    this.rawTrailers = [];
    this.complete = false;
    this.aborted = false;
    this.upgrade = false;
    this.statusCode = null;
    this.statusMessage = null;
  }
  _read() {}
  setTimeout(ms, callback) { if (this.socket) this.socket.setTimeout(ms, callback); return this; }
}

// Node folds repeated headers: set-cookie stays an array, a few singletons
// keep the first value, everything else joins with ', '.
const SINGLE_HEADERS = new Set(['age', 'authorization', 'content-length', 'content-type', 'etag',
  'expires', 'from', 'host', 'if-modified-since', 'if-unmodified-since', 'last-modified', 'location',
  'max-forwards', 'proxy-authorization', 'referer', 'retry-after', 'server', 'user-agent']);

function addHeader(headers, name, value) {
  const key = name.toLowerCase();
  if (key === 'set-cookie') {
    (headers[key] || (headers[key] = [])).push(value);
  } else if (headers[key] === undefined) {
    headers[key] = value;
  } else if (!SINGLE_HEADERS.has(key)) {
    headers[key] += (key === 'cookie' ? '; ' : ', ') + value;
  }
}

class ServerResponse extends OutgoingMessage {
  constructor(req, socket) {
    super();
    this.req = req;
    this.socket = socket;
    this.connection = socket;
    this.statusCode = 200;
    this.statusMessage = undefined;
    this.headersSent = false;
    this.sendDate = true;
    this.finished = false;
    this.shouldKeepAlive = req.httpVersionMinor >= 1 && !/\bclose\b/i.test(req.headers.connection || '')
      || /\bkeep-alive\b/i.test(req.headers.connection || '');
    this._headers = Object.create(null);
    this._chunked = false;
    this._noBody = req.method === 'HEAD';
  }
  setHeader(name, value) {
    if (this.headersSent) {
      const error = new Error('Cannot set headers after they are sent to the client');
      error.code = 'ERR_HTTP_HEADERS_SENT';
      throw error;
    }
    validateHeaderName(name);
    validateHeaderValue(name, value);
    this._headers[String(name).toLowerCase()] = [String(name), value];
    return this;
  }
  appendHeader(name, value) {
    const prev = this.getHeader(name);
    if (prev === undefined) return this.setHeader(name, value);
    return this.setHeader(name, [].concat(prev, value));
  }
  getHeader(name) { const entry = this._headers[String(name).toLowerCase()]; return entry && entry[1]; }
  hasHeader(name) { return String(name).toLowerCase() in this._headers; }
  removeHeader(name) { delete this._headers[String(name).toLowerCase()]; }
  getHeaderNames() { return Object.keys(this._headers); }
  getHeaders() {
    const out = Object.create(null);
    for (const key of Object.keys(this._headers)) out[key] = this._headers[key][1];
    return out;
  }
  writeHead(statusCode, statusMessage, headers) {
    if (typeof statusMessage !== 'string') { headers = statusMessage; statusMessage = undefined; }
    this.statusCode = statusCode;
    if (statusMessage !== undefined) this.statusMessage = statusMessage;
    if (Array.isArray(headers)) {
      for (let i = 0; i + 1 < headers.length; i += 2) this.appendHeader(headers[i], headers[i + 1]);
    } else if (headers) {
      for (const key of Object.keys(headers)) this.setHeader(key, headers[key]);
    }
    this._sendHeaders();
    return this;
  }
  flushHeaders() { this._sendHeaders(); }
  _sendHeaders(lastChunk) {
    if (this.headersSent) return;
    this.headersSent = true;
    const status = this.statusCode;
    const noBody = this._noBody || status === 204 || status === 304 || (status >= 100 && status < 200);
    if (this.sendDate && !this.hasHeader('date')) this._headers.date = ['Date', new Date().toUTCString()];
    if (!this.hasHeader('connection')) {
      this._headers.connection = ['Connection', this.shouldKeepAlive ? 'keep-alive' : 'close'];
      const timeout = this._keepAliveTimeout;
      if (this.shouldKeepAlive && timeout && !this.hasHeader('keep-alive')) {
        this._headers['keep-alive'] = ['Keep-Alive', 'timeout=' + Math.floor(timeout / 1000)];
      }
    } else if (/\bclose\b/i.test(String(this.getHeader('connection')))) {
      this.shouldKeepAlive = false;
    }
    if (!noBody && !this.hasHeader('content-length') && !this.hasHeader('transfer-encoding')) {
      if (lastChunk !== undefined) {
        this._headers['content-length'] = ['Content-Length', String(lastChunk.length)];
      } else {
        this._headers['transfer-encoding'] = ['Transfer-Encoding', 'chunked'];
        this._chunked = true;
      }
    } else if (/\bchunked\b/i.test(String(this.getHeader('transfer-encoding') || ''))) {
      this._chunked = true;
    }
    let head = 'HTTP/1.1 ' + status + ' ' + (this.statusMessage || STATUS_CODES[status] || 'unknown') + '\r\n';
    for (const key of Object.keys(this._headers)) {
      const [name, value] = this._headers[key];
      for (const one of [].concat(value)) head += name + ': ' + one + '\r\n';
    }
    this.socket.write(head + '\r\n');
  }
  _rawWrite(chunk) {
    if (this._noBody || chunk.length === 0) return;
    if (this._chunked) this.socket.write(chunk.length.toString(16) + '\r\n');
    this.socket.write(chunk);
    if (this._chunked) this.socket.write('\r\n');
  }
  _write(chunk, encoding, callback) {
    this._sendHeaders();
    this._rawWrite(chunk);
    callback();
  }
  end(chunk, encoding, callback) {
    if (typeof chunk === 'function') { callback = chunk; chunk = undefined; }
    else if (typeof encoding === 'function') { callback = encoding; encoding = undefined; }
    if (this.finished) { if (callback) process.nextTick(callback); return this; }
    if (chunk !== undefined && chunk !== null) {
      const buf = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk, encoding);
      if (!this.headersSent) this._sendHeaders(buf); else this._sendHeaders();
      this._rawWrite(buf);
    } else {
      this._sendHeaders(Buffer.alloc(0));
    }
    if (this._chunked && !this._noBody) this.socket.write('0\r\n\r\n');
    this.finished = true;
    this.once('finish', () => process.nextTick(() => this.emit('close')));
    super.end(callback);
    if (this._onDone) this._onDone(this.shouldKeepAlive);
    return this;
  }
}

// One parser per connection: request line + headers, then a Content-Length
// or chunked body; `upgrade`/CONNECT detach the socket with the bytes read
// past the headers.
function attachParser(server, socket) {
  let buffer = Buffer.alloc(0);
  let req = null;
  let bodyLeft = 0;
  let chunked = false;
  let chunkLeft = -1;
  let busy = false;
  const queue = [];

  const fail = () => {
    if (!socket.destroyed) socket.end('HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n');
  };
  const finishBody = () => {
    const done = req;
    req = null;
    done.complete = true;
    done.push(null);
  };
  const dispatch = (incoming) => {
    const res = new ServerResponse(incoming, socket);
    res._keepAliveTimeout = server.keepAliveTimeout;
    res._onDone = (keepAlive) => {
      busy = false;
      if (!keepAlive) { socket.end(); return; }
      if (queue.length) dispatch(queue.shift());
    };
    busy = true;
    server.emit('request', incoming, res);
  };
  const onData = (data) => {
    buffer = buffer.length ? Buffer.concat([buffer, data]) : Buffer.from(data);
    for (;;) {
      if (req === null) {
        const end = buffer.indexOf('\r\n\r\n');
        if (end < 0) {
          if (buffer.length > maxHeaderSize) fail();
          return;
        }
        const lines = buffer.subarray(0, end).toString('latin1').split('\r\n');
        buffer = buffer.subarray(end + 4);
        const match = /^([A-Z-]+) (\S+) HTTP\/(\d)\.(\d)$/.exec(lines[0]);
        if (!match) { fail(); return; }
        const incoming = new IncomingMessage(socket);
        incoming.method = match[1];
        incoming.url = match[2];
        incoming.httpVersionMajor = Number(match[3]);
        incoming.httpVersionMinor = Number(match[4]);
        incoming.httpVersion = match[3] + '.' + match[4];
        for (let i = 1; i < lines.length; i++) {
          const colon = lines[i].indexOf(':');
          if (colon <= 0) { fail(); return; }
          const name = lines[i].slice(0, colon).trim();
          const value = lines[i].slice(colon + 1).trim();
          incoming.rawHeaders.push(name, value);
          addHeader(incoming.headers, name, value);
        }
        const headers = incoming.headers;
        const wantsUpgrade = incoming.method === 'CONNECT'
          || (headers.upgrade !== undefined && /\bupgrade\b/i.test(headers.connection || ''));
        if (wantsUpgrade) {
          const event = incoming.method === 'CONNECT' ? 'connect' : 'upgrade';
          if (server.listenerCount(event) > 0) {
            incoming.upgrade = true;
            incoming.complete = true;
            socket.removeListener('data', onData);
            const head = buffer;
            buffer = Buffer.alloc(0);
            server.emit(event, incoming, socket, head);
            return;
          }
        }
        chunked = /\bchunked\b/i.test(headers['transfer-encoding'] || '');
        bodyLeft = chunked ? 0 : Number(headers['content-length'] || 0);
        chunkLeft = -1;
        req = incoming;
        if (busy) queue.push(incoming); else dispatch(incoming);
        if (!chunked && bodyLeft === 0) { finishBody(); continue; }
      }
      if (chunked) {
        if (chunkLeft < 0) {
          const eol = buffer.indexOf('\r\n');
          if (eol < 0) return;
          chunkLeft = parseInt(buffer.subarray(0, eol).toString('latin1'), 16);
          if (Number.isNaN(chunkLeft)) { fail(); return; }
          buffer = buffer.subarray(eol + 2);
          if (chunkLeft === 0) {
            const end = buffer.indexOf('\r\n');
            if (end < 0) { chunkLeft = -1; buffer = Buffer.concat([Buffer.from('0\r\n'), buffer]); return; }
            buffer = buffer.subarray(end + 2);
            finishBody();
            continue;
          }
        }
        if (buffer.length < chunkLeft + 2) return;
        req.push(buffer.subarray(0, chunkLeft));
        buffer = buffer.subarray(chunkLeft + 2);
        chunkLeft = -1;
        continue;
      }
      if (buffer.length === 0) return;
      const take = Math.min(bodyLeft, buffer.length);
      req.push(buffer.subarray(0, take));
      buffer = buffer.subarray(take);
      bodyLeft -= take;
      if (bodyLeft === 0) finishBody();
    }
  };
  socket.on('data', onData);
  socket.on('error', (error) => { if (server.listenerCount('clientError') > 0) server.emit('clientError', error, socket); });
  socket.on('close', () => { if (req && !req.complete) { req.aborted = true; req.emit('aborted'); req.destroy(); } });
}

class Server extends net.Server {
  constructor(options, requestListener) {
    if (typeof options === 'function') { requestListener = options; options = {}; }
    super(options || {});
    this.timeout = 0;
    this.keepAliveTimeout = 5000;
    this.headersTimeout = 60000;
    this.requestTimeout = 300000;
    this.maxHeadersCount = null;
    if (requestListener) this.on('request', requestListener);
    this.on('connection', (socket) => attachParser(this, socket));
  }
  setTimeout(ms, callback) {
    this.timeout = ms;
    if (callback) this.on('timeout', callback);
    return this;
  }
  closeAllConnections() {}
  closeIdleConnections() {}
}

function validateHeaderName(name) {
  if (typeof name !== 'string' || !/^[\^_`a-zA-Z\-0-9!#$%&'*+.|~]+$/.test(name)) {
    const error = new TypeError('Header name must be a valid HTTP token ["' + name + '"]');
    error.code = 'ERR_INVALID_HTTP_TOKEN';
    throw error;
  }
}
function validateHeaderValue(name, value) {
  if (value === undefined) {
    const error = new TypeError('Invalid value "undefined" for header "' + name + '"');
    error.code = 'ERR_HTTP_INVALID_HEADER_VALUE';
    throw error;
  }
}

module.exports = {
  METHODS,
  STATUS_CODES,
  Agent,
  globalAgent: new Agent(),
  OutgoingMessage,
  IncomingMessage,
  ClientRequest,
  ServerResponse,
  Server,
  maxHeaderSize,
  validateHeaderName,
  validateHeaderValue,
  setMaxIdleHTTPParsers: function setMaxIdleHTTPParsers() {},
  createServer: function createServer(options, requestListener) { return new Server(options, requestListener); },
  request: function request() { throw unsupported('http.request'); },
  get: function get() { throw unsupported('http.get'); },
};
