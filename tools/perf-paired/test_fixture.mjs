// Structural fixture tests with a deliberately tiny mock. NOT browser/native performance evidence.
import {readFileSync} from 'node:fs';
import assert from 'node:assert/strict';
const source = readFileSync(new URL('./scenarios.ts', import.meta.url), 'utf8');
const originalConsole = console;
for (const fault of ['none', 'noop-append', 'noop-remove', 'reverse-order', 'always-true-matches', 'always-false-matches', 'stale-class']) {
  class Node {
    constructor() { this.parentNode = null; this.children = []; this.classValue = ''; }
    get childNodes() { return fault === 'reverse-order' ? [...this.children].reverse() : this.children; }
    appendChild(node) {
      if (fault !== 'noop-append') { this.children.push(node); node.parentNode = this; }
      return node;
    }
    removeChild(node) {
      if (fault !== 'noop-remove') { this.children.splice(this.children.indexOf(node), 1); node.parentNode = null; }
      return node;
    }
    set className(value) { if (fault !== 'stale-class') this.classValue = value; }
    get className() { return this.classValue; }
    matches(selector) { return fault !== 'always-false-matches' && (fault === 'always-true-matches' || this.classValue.split(' ').includes(selector.slice(1))); }
  }
  globalThis.document = {body: new Node(), createElement() { return new Node(); }};
  globalThis.window = {innerWidth: 1024, innerHeight: 768, devicePixelRatio: 1};
  const lines = [];
  globalThis.console = {log(line) { lines.push(line); }};
  await import(`data:text/javascript;base64,${Buffer.from(source).toString('base64')}#${fault}`);
  assert.equal(lines.length, 1);
  const result = JSON.parse(lines[0].slice('W3COS_PERF_RESULT '.length));
  assert.equal(result.status, fault === 'none' ? 'ok' : 'error', fault);
}
globalThis.console = originalConsole;
console.log('7 fixture behavior tests passed (mock-only; no performance claim).');
