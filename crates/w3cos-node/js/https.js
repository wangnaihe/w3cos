'use strict';
// `https`: the http shapes with TLS defaults. Requests are not implemented
// yet (M3 item 23).

const http = require('http');
const { unsupported } = require('node:internal/w3cos_unsupported');

class Agent extends http.Agent {
  constructor(options) {
    super(options);
    this.defaultPort = 443;
    this.protocol = 'https:';
  }
}

module.exports = {
  Agent,
  globalAgent: new Agent(),
  Server: http.Server,
  createServer: function createServer() { throw unsupported('https.createServer'); },
  request: function request() { throw unsupported('https.request'); },
  get: function get() { throw unsupported('https.get'); },
};
