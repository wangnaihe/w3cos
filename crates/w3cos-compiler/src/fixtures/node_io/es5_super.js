'use strict';
// Classes extending ES5 constructor functions (how Node's stream / events
// classes are often written) and Array.from over native collections: the
// runtime pieces net.Socket and the I/O poll loop rely on.
function Base(value) { this.base = value; }
Base.prototype.describe = function () { return 'base:' + this.base; };
class Derived extends Base {
  constructor() { super(7); this.own = 1; }
}
const d = new Derived();
console.log('explicit', d.base, d.own, d.describe(), d instanceof Base, Object.keys(d).join(','));

function Guarded(value) {
  if (!(this instanceof Guarded)) return new Guarded(value);
  this.guarded = value;
}
class FromGuarded extends Guarded { constructor() { super(8); } }
const g = new FromGuarded();
console.log('guarded', g.guarded, g instanceof FromGuarded, g instanceof Guarded);

function Plain(value) { this.plain = value; }
class Implicit extends Plain {}
console.log('implicit', new Implicit(3).plain);

const { Duplex } = require('stream');
const seen = [];
class Echo extends Duplex {
  _write(chunk, encoding, callback) { seen.push(String(chunk)); callback(); }
  _read() {}
}
const echo = new Echo();
echo.on('finish', () => console.log('duplex finish', seen.join('+'), typeof echo._writableState));
echo.write('a');
echo.end('b');

const set = new Set([{ n: 1 }, 'two', 3]);
console.log('from set', Array.from(set).length, JSON.stringify([...set]));
const map = new Map([['k', 1], ['j', 2]]);
console.log('from map', JSON.stringify(Array.from(map)), JSON.stringify(Array.from(map.keys())));
