'use strict';
// `http2`: constants only. Sessions are not implemented yet (M3 item 23).

const { unsupported } = require('node:internal/w3cos_unsupported');

const constants = {
  NGHTTP2_CANCEL: 8, NGHTTP2_NO_ERROR: 0, NGHTTP2_PROTOCOL_ERROR: 1, NGHTTP2_INTERNAL_ERROR: 2,
  NGHTTP2_REFUSED_STREAM: 7, NGHTTP2_SESSION_CLIENT: 0, NGHTTP2_SESSION_SERVER: 1,
  HTTP2_HEADER_STATUS: ':status', HTTP2_HEADER_METHOD: ':method', HTTP2_HEADER_AUTHORITY: ':authority',
  HTTP2_HEADER_SCHEME: ':scheme', HTTP2_HEADER_PATH: ':path', HTTP2_HEADER_PROTOCOL: ':protocol',
  HTTP2_HEADER_CONTENT_TYPE: 'content-type', HTTP2_HEADER_CONTENT_LENGTH: 'content-length',
  HTTP2_HEADER_EXPECT: 'expect', HTTP2_HEADER_TE: 'te',
  HTTP2_METHOD_GET: 'GET', HTTP2_METHOD_POST: 'POST', HTTP2_METHOD_CONNECT: 'CONNECT',
  HTTP_STATUS_OK: 200,
};

module.exports = {
  constants,
  sensitiveHeaders: Symbol('nodejs.http2.sensitiveHeaders'),
  getDefaultSettings: function getDefaultSettings() { return {}; },
  connect: function connect() { throw unsupported('http2.connect'); },
  createServer: function createServer() { throw unsupported('http2.createServer'); },
  createSecureServer: function createSecureServer() { throw unsupported('http2.createSecureServer'); },
};
