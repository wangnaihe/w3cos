'use strict';
// `dns`: IP literals and `localhost` resolve locally; name lookups are not
// implemented yet (M3 item 23) and fail with ERR_W3COS_UNSUPPORTED.

const net = require('net');
const { unsupported } = require('node:internal/w3cos_unsupported');

function local(hostname) {
  const family = net.isIP(hostname);
  if (family) return [{ address: hostname, family }];
  if (hostname === 'localhost') return [{ address: '127.0.0.1', family: 4 }, { address: '::1', family: 6 }];
  return null;
}

function lookup(hostname, options, callback) {
  if (typeof options === 'function') { callback = options; options = {}; }
  if (typeof options === 'number') options = { family: options };
  options = options || {};
  const found = local(String(hostname));
  setTimeout(() => {
    if (!found) return callback(unsupported('dns.lookup(' + JSON.stringify(hostname) + ')'));
    const matches = options.family ? found.filter((entry) => entry.family === options.family) : found;
    if (options.all) return callback(null, matches);
    callback(null, matches[0].address, matches[0].family);
  }, 0);
}

const promises = {
  lookup: function (hostname, options) {
    return new Promise((resolve, reject) => {
      lookup(hostname, Object.assign({}, typeof options === 'number' ? { family: options } : options), (error, address, family) => {
        if (error) reject(error); else resolve(Array.isArray(address) ? address : { address, family });
      });
    });
  },
  resolve: function () { return Promise.reject(unsupported('dns.promises.resolve')); },
};

module.exports = {
  lookup,
  promises,
  ADDRCONFIG: 1024, V4MAPPED: 2048, ALL: 256,
  NOTFOUND: 'ENOTFOUND', NODATA: 'ENODATA',
  getServers: function getServers() { return []; },
  setServers: function setServers() {},
  setDefaultResultOrder: function setDefaultResultOrder() {},
  getDefaultResultOrder: function getDefaultResultOrder() { return 'verbatim'; },
  resolve: function resolve(name, type, callback) {
    if (typeof type === 'function') callback = type;
    setTimeout(() => callback(unsupported('dns.resolve')), 0);
  },
};
