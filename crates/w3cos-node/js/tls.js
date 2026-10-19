'use strict';
// `tls`: constants and socket classes. TLS connections are not implemented
// yet (M3 item 23).

const net = require('net');
const { unsupported } = require('node:internal/w3cos_unsupported');

class TLSSocket extends net.Socket {
  constructor(socket, options) { super(); this.encrypted = true; this.authorized = false; this._options = options; }
  getPeerCertificate() { return {}; }
  getProtocol() { return null; }
}
class Server extends net.Server {}

module.exports = {
  CLIENT_RENEG_LIMIT: 3,
  CLIENT_RENEG_WINDOW: 600,
  DEFAULT_CIPHERS: 'TLS_AES_256_GCM_SHA384:TLS_CHACHA20_POLY1305_SHA256:TLS_AES_128_GCM_SHA256',
  DEFAULT_ECDH_CURVE: 'auto',
  DEFAULT_MIN_VERSION: 'TLSv1.2',
  DEFAULT_MAX_VERSION: 'TLSv1.3',
  rootCertificates: Object.freeze([]),
  getCiphers: function getCiphers() { return ['tls_aes_128_gcm_sha256', 'tls_aes_256_gcm_sha384', 'tls_chacha20_poly1305_sha256']; },
  getCACertificates: function getCACertificates() { return []; },
  checkServerIdentity: function checkServerIdentity() { return undefined; },
  createSecureContext: function createSecureContext(options) { return { context: {}, options: options || {} }; },
  SecureContext: function SecureContext() {},
  TLSSocket,
  Server,
  connect: function connect() { throw unsupported('tls.connect'); },
  createServer: function createServer() { throw unsupported('tls.createServer'); },
};
