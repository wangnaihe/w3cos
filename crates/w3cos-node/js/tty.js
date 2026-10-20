'use strict';
// `tty`: W3C OS apps have no controlling terminal unless the host says so
// through `process.stdout.isTTY`.

const proc = require('process');
const net = require('net');

function isatty(fd) {
  const stream = fd === 0 ? proc.stdin : fd === 1 ? proc.stdout : fd === 2 ? proc.stderr : undefined;
  return !!(stream && stream.isTTY);
}

class ReadStream extends net.Socket {
  constructor() { super(); this.isRaw = false; this.isTTY = true; }
  setRawMode(mode) { this.isRaw = !!mode; return this; }
}
class WriteStream extends net.Socket {
  constructor() { super(); this.isTTY = true; this.columns = 80; this.rows = 24; }
  getColorDepth() { return 8; }
  hasColors(count) { return (count === undefined ? 16 : count) <= 256; }
  getWindowSize() { return [this.columns, this.rows]; }
}

module.exports = { isatty, ReadStream, WriteStream };
