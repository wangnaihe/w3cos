'use strict';
// `readline`: line splitting over a readable stream (`createInterface`,
// `question`, async iteration). No terminal editing.

const EventEmitter = require('events');

class Interface extends EventEmitter {
  constructor(input, output) {
    super();
    const options = input && input.input ? input : { input, output };
    this.input = options.input;
    this.output = options.output;
    this.terminal = false;
    this.line = '';
    this.closed = false;
    this._buffer = '';
    this._prompt = options.prompt === undefined ? '> ' : options.prompt;
    this._questions = [];
    const self = this;
    this._onData = function onData(chunk) { self._push(String(chunk)); };
    this._onEnd = function onEnd() {
      if (self._buffer) { const rest = self._buffer; self._buffer = ''; self._line(rest); }
      self.close();
    };
    if (this.input && typeof this.input.on === 'function') {
      this.input.on('data', this._onData);
      this.input.on('end', this._onEnd);
    }
  }
  _push(text) {
    this._buffer += text;
    let index;
    while ((index = this._buffer.search(/\r?\n/)) !== -1) {
      const line = this._buffer.slice(0, index);
      this._buffer = this._buffer.slice(this._buffer[index] === '\r' ? index + 2 : index + 1);
      this._line(line);
    }
  }
  _line(line) {
    const question = this._questions.shift();
    if (question) question(line); else this.emit('line', line);
  }
  setPrompt(prompt) { this._prompt = prompt; }
  getPrompt() { return this._prompt; }
  prompt() { if (this.output && this.output.write) this.output.write(this._prompt); }
  question(query, options, callback) {
    if (typeof options === 'function') callback = options;
    if (this.output && this.output.write) this.output.write(query);
    this._questions.push(callback);
  }
  write(data) { if (this.output && this.output.write) this.output.write(data); }
  pause() { if (this.input && this.input.pause) this.input.pause(); this.emit('pause'); return this; }
  resume() { if (this.input && this.input.resume) this.input.resume(); this.emit('resume'); return this; }
  close() {
    if (this.closed) return;
    this.closed = true;
    if (this.input && typeof this.input.removeListener === 'function') {
      this.input.removeListener('data', this._onData);
      this.input.removeListener('end', this._onEnd);
    }
    this.emit('close');
  }
  [Symbol.asyncIterator]() {
    const lines = [];
    const waiting = [];
    let done = false;
    this.on('line', (line) => { const next = waiting.shift(); if (next) next({ value: line, done: false }); else lines.push(line); });
    this.on('close', () => { done = true; while (waiting.length) waiting.shift()({ value: undefined, done: true }); });
    return {
      next: () => {
        if (lines.length) return Promise.resolve({ value: lines.shift(), done: false });
        if (done) return Promise.resolve({ value: undefined, done: true });
        return new Promise((resolve) => waiting.push(resolve));
      },
      return: () => { this.close(); return Promise.resolve({ value: undefined, done: true }); },
      [Symbol.asyncIterator]() { return this; },
    };
  }
}

module.exports = {
  Interface,
  createInterface: function createInterface(input, output) { return new Interface(input, output); },
  clearLine: function clearLine(stream, dir, callback) { if (callback) callback(); return true; },
  clearScreenDown: function clearScreenDown(stream, callback) { if (callback) callback(); return true; },
  cursorTo: function cursorTo(stream, x, y, callback) { if (typeof y === 'function') y(); else if (callback) callback(); return true; },
  moveCursor: function moveCursor(stream, dx, dy, callback) { if (callback) callback(); return true; },
  emitKeypressEvents: function emitKeypressEvents() {},
};
