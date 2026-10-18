// Leak CI scenarios. Every `export function scenario_<name>()` is run by
// `cargo run --release -p w3cos-compiler --example leak_ci`: warm up, take a
// baseline, run N iterations, and require live arena objects / arrays /
// functions to return exactly to the baseline (zero residual per scenario).
// Async scenarios return a promise; the harness drains microtasks (and with
// them FinalizationRegistry callbacks) after every call.

// ---- realistic AOT sample (bench.ts without the DOM output) ----
class Vec2 {
  x: number;
  y: number;
  constructor(x: number, y: number) {
    this.x = x;
    this.y = y;
  }
  add(o: Vec2): Vec2 {
    return new Vec2(this.x + o.x, this.y + o.y);
  }
  len2(): number {
    return this.x * this.x + this.y * this.y;
  }
}

function particles(n: number): number {
  let acc = new Vec2(0, 0);
  const items: any[] = [];
  for (let i = 0; i < n; i++) {
    items.push({ id: i, pos: new Vec2(i, i * 2), tag: "p" + (i % 10) });
  }
  for (let r = 0; r < 2; r++) {
    for (const it of items) acc = acc.add(it.pos);
  }
  return acc.len2() + items.length;
}

function pipeline(n: number): number {
  const xs: any[] = [];
  for (let i = 0; i < n; i++) xs.push({ v: i, odd: i % 2 === 1 });
  return xs
    .filter((o: any) => o.odd)
    .map((o: any) => ({ w: o.v * 2 }))
    .reduce((s: number, o: any) => s + o.w, 0);
}

function lookup(n: number): number {
  const table: any = {};
  for (let i = 0; i < 100; i++) table["k" + i] = { n: i };
  let s = 0;
  for (let i = 0; i < n; i++) s += table["k" + (i % 100)].n;
  return s;
}

function tree(depth: number): any {
  if (depth === 0) return null;
  return { l: tree(depth - 1), r: tree(depth - 1), d: depth };
}

function sumTree(t: any): number {
  if (t === null) return 0;
  return t.d + sumTree(t.l) + sumTree(t.r);
}

export function scenario_realistic_sample(): number {
  return particles(500) + pipeline(500) + lookup(2000) + sumTree(tree(8));
}

// ---- probe (non-escaping and escaping-then-dropped allocations) ----
function makePoint(i: number): any {
  return { x: i, y: i + 1 };
}
export function scenario_probe(): number {
  let sum = 0;
  for (let i = 0; i < 1000; i++) {
    const p = { x: i, y: i + 1 };
    const pair = [p.x, p.y];
    sum += pair[0] + pair[1];
    const q = makePoint(i);
    sum += q.x;
  }
  return sum;
}

// ---- language features ----
export function scenario_for_of(): number {
  const rows: any[] = [];
  for (let i = 0; i < 100; i++) rows.push([i, { v: i }]);
  let sum = 0;
  for (const row of rows) for (const cell of row) sum += typeof cell === "number" ? cell : cell.v;
  for (const [k, v] of new Map<string, number>([["a", 1], ["b", 2]])) sum += v + k.length;
  for (const ch of "abc") sum += ch.length;
  return sum;
}

function* range(n: number) {
  for (let i = 0; i < n; i++) yield { i };
}
function* nested(n: number) {
  yield* range(n);
  yield { i: -1 };
}
export function scenario_generators(): number {
  let sum = 0;
  for (const item of nested(50)) sum += item.i;
  const it = range(10);
  it.next();
  it.return(undefined);
  return sum + [...range(5)].length;
}

class Shape {
  name: string;
  #secret: any;
  constructor(name: string) {
    this.name = name;
    this.#secret = { name };
  }
  area(): number {
    return 0;
  }
  secretName(): string {
    return this.#secret.name;
  }
}
class Rect extends Shape {
  w: number;
  h: number;
  constructor(w: number, h: number) {
    super("rect");
    this.w = w;
    this.h = h;
  }
  area(): number {
    return this.w * this.h;
  }
  static unit(): Rect {
    return new Rect(1, 1);
  }
}
export function scenario_classes(): number {
  let sum = 0;
  for (let i = 0; i < 100; i++) {
    const r = new Rect(i, 2);
    sum += r.area() + r.secretName().length + Rect.unit().area();
  }
  return sum;
}

function counter() {
  let n = 0;
  return { inc: () => ++n, get: () => n };
}
function makeAdder(base: any) {
  return (x: number) => base.v + x;
}
export function scenario_closures(): number {
  let sum = 0;
  for (let i = 0; i < 100; i++) {
    const c = counter();
    c.inc();
    c.inc();
    sum += c.get();
    const add = makeAdder({ v: i });
    sum += add(1);
    const handlers: any[] = [];
    const owner: any = { items: [] as any[] };
    handlers.push(() => owner.items.length);
    owner.items.push({ i });
    sum += handlers[0]();
  }
  return sum;
}

let asyncSink = 0;
async function step(v: any): Promise<number> {
  await null;
  return v.n;
}
export async function scenario_async(): Promise<number> {
  let sum = 0;
  for (let i = 0; i < 20; i++) sum += await step({ n: i });
  const all = await Promise.all([step({ n: 1 }), step({ n: 2 })]);
  try {
    await Promise.reject(new Error("x"));
  } catch (e) {
    sum += 1;
  }
  asyncSink = sum + all[0] + all[1];
  return asyncSink;
}

export function scenario_weakref(): number {
  let alive = 0;
  for (let i = 0; i < 100; i++) {
    const target: any = { i };
    const ref = new WeakRef(target);
    if (ref.deref() !== undefined) alive++;
    const wm = new WeakMap<any, any>();
    wm.set(target, { payload: i });
    if (wm.has(target)) alive++;
  }
  return alive;
}

let finalized = 0;
const registry = new FinalizationRegistry((held: any) => {
  finalized += held;
});
export function scenario_finalization_registry(): number {
  for (let i = 0; i < 50; i++) {
    const obj: any = { i };
    registry.register(obj, 1);
  }
  return finalized;
}

export function scenario_collections_json_strings(): number {
  const m = new Map<string, any>();
  const s = new Set<any>();
  for (let i = 0; i < 100; i++) {
    m.set("k" + i, { i });
    s.add({ i });
  }
  const json = JSON.stringify({ list: [...m.values()].slice(0, 10) });
  const back = JSON.parse(json);
  let text = "";
  for (let i = 0; i < 50; i++) text += String(i);
  m.clear();
  return back.list.length + s.size + text.length;
}

export function scenario_exceptions_destructuring(): number {
  let sum = 0;
  for (let i = 0; i < 100; i++) {
    try {
      if (i % 3 === 0) throw new Error("e" + i);
      const { a, ...rest } = { a: i, b: { c: i }, d: [i] } as any;
      const [first, ...others] = [a, rest.b, rest.d];
      sum += first + others.length;
    } catch (e: any) {
      sum += e.message.length;
    } finally {
      sum += 1;
    }
  }
  return sum;
}
