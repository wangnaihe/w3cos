'use strict';
// `http`: protocol tables, `Agent` and the message classes. Client requests
// and servers are not implemented yet (M3 item 23); they throw
// ERR_W3COS_UNSUPPORTED when called.

const EventEmitter = require('events');
const stream = require('stream');
const { unsupported } = require('node:internal/w3cos_unsupported');

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
class IncomingMessage extends stream.Readable {}
class ClientRequest extends OutgoingMessage {}
class ServerResponse extends OutgoingMessage {}
class Server extends EventEmitter {
  listen() { throw unsupported('http.Server.listen'); }
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
  maxHeaderSize: 16384,
  validateHeaderName,
  validateHeaderValue,
  setMaxIdleHTTPParsers: function setMaxIdleHTTPParsers() {},
  createServer: function createServer() { throw unsupported('http.createServer'); },
  request: function request() { throw unsupported('http.request'); },
  get: function get() { throw unsupported('http.get'); },
};
