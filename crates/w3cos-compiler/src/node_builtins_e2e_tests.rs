//! Node builtins from `w3cos-node` (M2 slice 1): a CommonJS app using
//! path / events / util / url / os / process / timers is AOT-compiled with
//! the builtin glue and must print what Node 20 prints for the same file.

use crate::cjs_e2e_tests::{compile_and_run, write_fixture};

const MAIN: &str = r##"const path = require("path");
const posix = require("node:path/posix");
console.log("join", path.join("/a/b", "../c", "./d.txt"), path.join("", ""), path.join("a", "", "b/"));
console.log("normalize", path.normalize("/foo/bar//baz/asdf/quux/.."), path.normalize("./x/../../y"), path.normalize(""));
console.log("resolve", path.resolve("/foo/bar", "./baz"), path.resolve("/foo/bar", "/tmp/file/"));
console.log("relative", path.relative("/data/orandea/test/aaa", "/data/orandea/impl/bbb"), JSON.stringify(path.relative("/a", "/a")));
console.log("dirname", path.dirname("/a/b/c"), path.dirname("a"), path.dirname("/"), path.dirname("//x"));
console.log("basename", path.basename("/a/b/c.html"), path.basename("/a/b/c.html", ".html"), path.basename("/a/b/"), path.basename("aaa", "a"));
console.log("extname", path.extname("index.html"), path.extname("index.coffee.md"), JSON.stringify(path.extname(".index")), path.extname("index."));
console.log("parse", JSON.stringify(path.parse("/home/user/dir/file.txt")), JSON.stringify(path.parse("./rel.tar.gz")));
console.log("format", path.format({ root: "/ignored", dir: "/home/user/dir", base: "file.txt" }), path.format({ name: "file", ext: "txt" }));
console.log("misc", path.sep, path.delimiter, path.isAbsolute("/x"), path.isAbsolute("x"), posix === path.posix, path.posix.join("a", "b"));
try { path.join("a", 1); } catch (e) { console.log("path err", e.code, e.name); }

const EventEmitter = require("events");
const { once } = require("node:events");
class Job extends EventEmitter {}
const job = new Job();
const order = [];
job.on("newListener", (name) => order.push("new:" + String(name)));
job.on("data", (x) => order.push("a" + x));
job.prependListener("data", (x) => order.push("p" + x));
job.once("data", (x) => order.push("o" + x));
console.log("emit", job.emit("data", 1), job.emit("data", 2), job.emit("nothing"));
console.log("order", order.join(","));
console.log("counts", job.listenerCount("data"), job.eventNames().join("|"), EventEmitter.defaultMaxListeners, job.getMaxListeners());
const handler = () => {};
job.on("x", handler).off("x", handler);
console.log("off", job.listenerCount("x"), job.rawListeners("data").length);
try { job.emit("error", new Error("boom")); } catch (e) { console.log("error event", e.message); }
try { job.emit("error", "plain"); } catch (e) { console.log("error event2", e.code, e.message); }
job.removeAllListeners("data");
console.log("removed", job.listenerCount("data"), EventEmitter.EventEmitter === EventEmitter, job instanceof EventEmitter);

const util = require("util");
console.log(util.format("%s=%d %i %f %j %%", "n", 42.5, 42.5, "1.5", { a: [1, 2] }), util.format("a", 1, "b", { c: 2 }));
console.log(util.format("%O", { x: { y: { z: { w: 1 } } } }));
console.log(util.inspect({ a: 1, b: "two", c: [1, 2, 3], d: { e: { f: { g: 1 } } }, fn: function named() {}, arrow: () => 1 }).split("\n").join("|"));
console.log(util.inspect([1, "x", null, undefined, true, -0, [[[[1]]]]]), util.inspect("it's"), util.inspect(new Map([["k", 1]])), util.inspect(new Set([1, 2])));
console.log(util.inspect(Object.create(null)), util.inspect({}), util.inspect([]), util.inspect(new Date(0)), util.inspect(/re/g));
function Base() {}
Base.prototype.hello = function () { return "hi"; };
function Derived() { Base.call(this); }
util.inherits(Derived, Base);
console.log("inherits", new Derived().hello(), Derived.super_ === Base);
console.log("deepEqual", util.isDeepStrictEqual({ a: [1, { b: 2 }] }, { a: [1, { b: 2 }] }), util.isDeepStrictEqual([1], ["1"]), util.isDeepStrictEqual(new Map([[1, 2]]), new Map([[1, 2]])));
console.log("types", util.types.isDate(new Date()), util.types.isRegExp(/x/), util.types.isPromise(Promise.resolve()), util.types.isMap(new Map()), util.isArray([]));
const custom = { [util.inspect.custom]() { return "CUSTOM"; } };
console.log("custom", util.inspect(custom), util.inspect.custom === Symbol.for("nodejs.util.inspect.custom"));

const url = require("url");
console.log("fileURLToPath", url.fileURLToPath("file:///tmp/a%20b.txt"), url.pathToFileURL("/tmp/x y#1.txt").href);
console.log("url format", url.format({ protocol: "https", hostname: "example.com", pathname: "/p", query: { q: "1 2" } }));
console.log("URL", new url.URL("https://a.b/c?d=1").searchParams.get("d"), url.URL === URL);
try { url.fileURLToPath("https://x/y"); } catch (e) { console.log("url err", e.code); }

const os = require("os");
console.log("os", JSON.stringify(os.EOL), os.platform(), os.type(), os.endianness(), typeof os.tmpdir(), typeof os.homedir(), os.constants.signals.SIGTERM, os.constants.errno.ENOENT);

const proc = require("process");
console.log("process", proc.platform, typeof proc.nextTick, typeof proc.hrtime(), proc.hrtime().length, typeof proc.env, Array.isArray(proc.argv), typeof proc.cwd());

const timers = require("timers");
const timersPromises = require("node:timers/promises");
const seq = [];
proc.nextTick(() => seq.push("tick1"));
Promise.resolve().then(() => seq.push("micro"));
proc.nextTick(() => seq.push("tick2"));
timers.setTimeout((a, b) => {
  seq.push("timeout:" + a + b);
  timersPromises.setTimeout(1, "val").then((v) => {
    seq.push("promise:" + v);
    console.log("seq", seq.join(","));
  });
}, 0, "x", "y");
console.log("object model", ({}) instanceof Object, Object.getPrototypeOf({}) === Object.prototype, ({}).constructor === Object, new Date(0) instanceof Date, Object.prototype.toString.call(new Date(0)), Object.prototype.toString.call([]), typeof ({}).hasOwnProperty);
"##;

/// `node main.cjs` (v20.19.2).
const NODE_OUTPUT: &str = r##"join /a/c/d.txt . a/b/
normalize /foo/bar/baz/asdf ../y .
resolve /foo/bar/baz /tmp/file
relative ../../impl/bbb ""
dirname /a/b . / //
basename c.html c b aa
extname .html .md "" .
parse {"root":"/","dir":"/home/user/dir","base":"file.txt","ext":".txt","name":"file"} {"root":"","dir":".","base":"rel.tar.gz","ext":".gz","name":"rel.tar"}
format /home/user/dir/file.txt file.txt
misc / : true false true a/b
path err ERR_INVALID_ARG_TYPE TypeError
emit true true false
order new:data,new:data,new:data,p1,a1,o1,p2,a2
counts 2 newListener|data 10 10
off 0 2
error event boom
error event2 ERR_UNHANDLED_ERROR Unhandled error. ('plain')
removed 0 true true
n=42.5 42 1.5 {"a":[1,2]} % a 1 b { c: 2 }
{ x: { y: { z: [Object] } } }
{|  a: 1,|  b: 'two',|  c: [ 1, 2, 3 ],|  d: { e: { f: [Object] } },|  fn: [Function: named],|  arrow: [Function: arrow]|}
[ 1, 'x', null, undefined, true, -0, [ [ [Array] ] ] ] "it's" Map(1) { 'k' => 1 } Set(2) { 1, 2 }
[Object: null prototype] {} {} [] 1970-01-01T00:00:00.000Z /re/g
inherits hi true
deepEqual true false true
types true true true true true
custom CUSTOM true
fileURLToPath /tmp/a b.txt file:///tmp/x%20y%231.txt
url format https://example.com/p?q=1%202
URL 1 true
url err ERR_INVALID_URL_SCHEME
os "\n" linux Linux LE string string 15 2
process linux function object 2 object true string
object model true true true true [object Date] [object Array] function
seq tick1,tick2,micro,timeout:xy,promise:val
"##;

#[test]
fn node_builtins_match_node() {
    let root = write_fixture(
        "w3cos_node_builtins_e2e",
        &[
            ("package.json", r#"{"name":"node-builtins"}"#),
            ("main.cjs", MAIN),
        ],
    );
    let lines = compile_and_run(&root, &root.join("main.cjs"), "w3cos_node_builtins_e2e");
    let expected: Vec<&str> = NODE_OUTPUT.lines().collect();
    assert_eq!(lines, expected);
    std::fs::remove_dir_all(root).ok();
}

const BUFFER_MAIN: &str = r##"const { Buffer } = require("buffer");
const b = Buffer.from("héllo wörld");
console.log("from", b.length, b.toString(), b.toString("hex"), b.toString("base64"), b.toString("base64url"), b.toString("latin1").length);
console.log("is", Buffer.isBuffer(b), b instanceof Uint8Array, b instanceof Buffer, Buffer.isBuffer(new Uint8Array(2)), Buffer.isEncoding("UTF-8"), Buffer.isEncoding("nope"));
console.log("decode", Buffer.from("68656c6c6f", "hex").toString(), Buffer.from("aGVsbG8=", "base64").toString(), Buffer.from("aGVsbG8", "base64url").toString(), Buffer.from("hi", "utf16le").toString("hex"), Buffer.from([0xe2, 0x82, 0xac, 0xff]).toString());
console.log("bytes", Buffer.byteLength("héllo"), Buffer.byteLength("abcd", "hex"), Buffer.byteLength("€"), Buffer.alloc(4).toString("hex"), Buffer.alloc(5, "ab").toString(), Buffer.alloc(3, 7).toString("hex"));
const c = Buffer.concat([Buffer.from("ab"), Buffer.from("cd"), Buffer.from("ef")]);
console.log("concat", c.toString(), Buffer.concat([Buffer.from("abc")], 2).toString(), Buffer.compare(Buffer.from("a"), Buffer.from("b")), c.equals(Buffer.from("abcdef")), c.compare(Buffer.from("abd")));
const s = c.subarray(1, 4); s[0] = 0x42;
console.log("slice", s.toString(), c.toString(), c.slice(-2).toString(), Buffer.isBuffer(s), s.length, c.subarray(4, 2).length);
const w = Buffer.alloc(8); const n = w.write("hey", 2);
console.log("write", n, w.toString("hex"), w.write("zz", "latin1"), w.toString("utf8", 0, 2));
console.log("index", c.indexOf("cd"), c.indexOf(0x65), c.lastIndexOf("e"), c.includes("Bc"), c.indexOf("zz"), c.indexOf(Buffer.from("ef")));
const r = Buffer.alloc(16);
r.writeUInt16LE(0x1234, 0); r.writeUInt32BE(0xdeadbeef, 2); r.writeInt8(-2, 6); r.writeDoubleLE(1.5, 8);
console.log("rw", r.readUInt16LE(0), r.readUInt16BE(0), r.readUInt32BE(2).toString(16), r.readInt8(6), r.readUInt8(6), r.readDoubleLE(8), r.readUint32LE(2), r.toString("hex"));
try { r.readUInt32LE(14); } catch (e) { console.log("range", e.code, e.name); }
try { Buffer.from({}); } catch (e) { console.log("type", e.code); }
try { b.toString("bogus"); } catch (e) { console.log("enc", e.code); }
console.log("json", JSON.stringify(Buffer.from("hi")), Buffer.from(JSON.parse(JSON.stringify(Buffer.from("ok")))).toString());
const cp = Buffer.alloc(6, "."); console.log("copy", Buffer.from("xyz").copy(cp, 2), cp.toString(), Buffer.from("abcd").swap16().toString(), Buffer.from("abcd").swap32().toString());
console.log("inspect", require("util").inspect(Buffer.from("hi")), Buffer.from("hi").inspect());
console.log("ab", Buffer.from(new Uint8Array([1, 2, 3]).buffer, 1).toString("hex"), Buffer.from(Buffer.from("x")).toString(), Object.prototype.toString.call(b));
console.log("fill", Buffer.alloc(4).fill("ab").toString(), Buffer.alloc(4).fill(0x41, 1, 3).toString("hex"), typeof Buffer.poolSize, require("node:buffer").kMaxLength > 0);
"##;

/// `node main.cjs` (v20.19.2).
const BUFFER_NODE_OUTPUT: &str = r##"from 13 héllo wörld 68c3a96c6c6f2077c3b6726c64 aMOpbGxvIHfDtnJsZA== aMOpbGxvIHfDtnJsZA 13
is true true true false true false
decode hello hello hello 68006900 €�
bytes 6 2 3 00000000 ababa 070707
concat abcdef ab -1 true -1
slice Bcd aBcdef ef true 3 0
write 3 0000686579000000 2 zz
index 2 4 4 true -1 4
rw 4660 13330 deadbeef -2 254 1.5 4022250974 3412deadbeeffe00000000000000f83f
range ERR_OUT_OF_RANGE RangeError
type ERR_INVALID_ARG_TYPE
enc ERR_UNKNOWN_ENCODING
json {"type":"Buffer","data":[104,105]} ok
copy 3 ..xyz. badc dcba
inspect <Buffer 68 69> <Buffer 68 69>
ab 0203 x [object Uint8Array]
fill abab 00414100 number true
"##;

#[test]
fn node_buffer_matches_node() {
    let root = write_fixture(
        "w3cos_node_buffer_e2e",
        &[
            ("package.json", r#"{"name":"node-buffer"}"#),
            ("main.cjs", BUFFER_MAIN),
        ],
    );
    let lines = compile_and_run(&root, &root.join("main.cjs"), "w3cos_node_buffer_e2e");
    let expected: Vec<&str> = BUFFER_NODE_OUTPUT.lines().collect();
    assert_eq!(lines, expected);
    std::fs::remove_dir_all(root).ok();
}

const MODULES_MAIN: &str = r##"const { StringDecoder } = require("string_decoder");
const qs = require("node:querystring");
const assert = require("assert");
const strict = require("assert/strict");

const euro = Buffer.from("€");
const d = new StringDecoder("utf8");
console.log("sd", JSON.stringify(d.write(euro.subarray(0, 1))), JSON.stringify(d.write(euro.subarray(1))), JSON.stringify(d.end()), d.encoding);
const d2 = new StringDecoder("utf8");
console.log("sd-end", JSON.stringify(d2.write(Buffer.from([0x61, 0xe2, 0x82]))), JSON.stringify(d2.end()), new StringDecoder("hex").write(Buffer.from("hi")), new StringDecoder("utf16le").end(Buffer.from([0x68, 0, 0x69, 0])));
try { new StringDecoder("nope"); } catch (e) { console.log("sd-err", e.code, e.name); }

console.log("qs", qs.stringify({ a: 1, b: [2, "x y"], c: "é&=", d: true, e: null, f: undefined }), qs.escape("a b/ü"), qs.unescape("a%20b%zz+"));
const parsed = qs.parse("a=1&b=2&a=3&c=x+y%21&d&=e&f=%E2%82%AC");
console.log("qs-parse", JSON.stringify(parsed), Object.getPrototypeOf(parsed) === null, JSON.stringify(qs.parse("a:1;b:2", ";", ":")), qs.decode === qs.parse, JSON.stringify(qs.parse("a=1&b=2", null, null, { maxKeys: 1 })));

function code(fn) { try { fn(); return "no-throw"; } catch (e) { return e.code + "/" + e.name + "/" + (e instanceof assert.AssertionError) + "/" + e.operator; } }
console.log("assert-ok", code(() => assert(true)), code(() => assert.ok(0)), code(() => assert.equal(1, "1")), code(() => assert.equal(1, 2)), code(() => assert.notEqual(1, 2)));
console.log("assert-strict", code(() => assert.strictEqual(1, "1")), code(() => assert.strictEqual(NaN, NaN)), code(() => strict.equal(1, "1")), strict.strict === strict, assert.strict === strict);
console.log("assert-deep", code(() => assert.deepEqual({ a: [1, 2] }, { a: ["1", 2] })), code(() => assert.deepStrictEqual({ a: [1, 2] }, { a: ["1", 2] })), code(() => assert.deepStrictEqual({ a: [1, { b: 2 }] }, { a: [1, { b: 2 }] })), code(() => assert.notDeepStrictEqual([1], [1])));
try { assert.strictEqual(1, 2); } catch (e) { console.log("msg1", JSON.stringify(e.message), e.actual, e.expected, e.generatedMessage); }
try { assert.equal(1, 2, "custom"); } catch (e) { console.log("msg2", e.message, e.generatedMessage); }
try { assert.fail("boom"); } catch (e) { console.log("msg3", e.message, e.code); }
try { assert.notStrictEqual(1, 1); } catch (e) { console.log("msg4", JSON.stringify(e.message)); }
console.log("throws", code(() => assert.throws(() => { throw new TypeError("bad"); }, TypeError)), code(() => assert.throws(() => { throw new Error("abc"); }, /abc/)), code(() => assert.throws(() => { throw new Error("abc"); }, { message: "abc" })), code(() => assert.throws(() => {})), code(() => assert.doesNotThrow(() => { throw new Error("x"); })));
try { assert.throws(() => {}); } catch (e) { console.log("msg5", e.message); }
try { assert.throws(() => {}, TypeError, "need it"); } catch (e) { console.log("msg6", e.message); }
console.log("match", code(() => assert.match("abc", /b/)), code(() => assert.match("abc", /z/)), code(() => assert.ifError(null)), code(() => assert.ifError(new Error("e1"))));
try { assert.ifError(new Error("e1")); } catch (e) { console.log("msg7", e.message); }
// Chained: the AOT backend resumes `await` on an already-fulfilled promise
// synchronously, so independent chains may interleave differently.
assert.rejects(Promise.reject(new Error("r")), /r/)
  .then(() => { console.log("rejects ok"); return assert.rejects(Promise.resolve(1)); })
  .catch((e) => console.log("rejects missing", e.message, e.code));
console.log("misc", Object.is(NaN, NaN), Object.is(0, -0), Object.keys(new Error("x")).length, new Error("x").hasOwnProperty("name"), JSON.stringify(new TypeError("t")), Promise.resolve(1) instanceof Promise, (async () => 1)() instanceof Promise);
const nodeConstants = require("constants");
console.log("constants", nodeConstants.O_RDONLY, nodeConstants.ENOENT, nodeConstants.SIGTERM, nodeConstants.S_IFMT, nodeConstants.PRIORITY_HIGH, nodeConstants === require("node:constants"));
"##;

/// `node main.cjs` (v20.19.2).
const MODULES_NODE_OUTPUT: &str = r##"sd "" "€" "" utf8
sd-end "a" "�" 6869 hi
sd-err ERR_UNKNOWN_ENCODING TypeError
qs a=1&b=2&b=x%20y&c=%C3%A9%26%3D&d=true&e=&f= a%20b%2F%C3%BC a b%zz+
qs-parse {"a":["1","3"],"b":"2","c":"x y!","d":"","":"e","f":"€"} true {"a":"1","b":"2"} true {"a":"1"}
assert-ok no-throw ERR_ASSERTION/AssertionError/true/== no-throw ERR_ASSERTION/AssertionError/true/== no-throw
assert-strict ERR_ASSERTION/AssertionError/true/strictEqual no-throw ERR_ASSERTION/AssertionError/true/strictEqual true true
assert-deep no-throw ERR_ASSERTION/AssertionError/true/deepStrictEqual no-throw ERR_ASSERTION/AssertionError/true/notDeepStrictEqual
msg1 "Expected values to be strictly equal:\n\n1 !== 2\n" 1 2 true
msg2 custom false
msg3 boom ERR_ASSERTION
msg4 "Expected \"actual\" to be strictly unequal to: 1"
throws no-throw no-throw no-throw ERR_ASSERTION/AssertionError/true/throws ERR_ASSERTION/AssertionError/true/doesNotThrow
msg5 Missing expected exception.
msg6 Missing expected exception (TypeError): need it
match no-throw ERR_ASSERTION/AssertionError/true/match no-throw ERR_ASSERTION/AssertionError/true/ifError
msg7 ifError got unwanted exception: e1
misc true false 0 false {} true true
constants 0 2 15 61440 -14 true
rejects ok
rejects missing Missing expected rejection. ERR_ASSERTION
"##;

#[test]
/// Also covers the implicit global `Buffer` (no `require("buffer")`).
fn node_string_decoder_querystring_assert_match_node() {
    let root = write_fixture(
        "w3cos_node_modules_e2e",
        &[
            ("package.json", r#"{"name":"node-modules"}"#),
            ("main.cjs", MODULES_MAIN),
        ],
    );
    let lines = compile_and_run(&root, &root.join("main.cjs"), "w3cos_node_modules_e2e");
    let expected: Vec<&str> = MODULES_NODE_OUTPUT.lines().collect();
    assert_eq!(lines, expected);
    std::fs::remove_dir_all(root).ok();
}

const STREAM_MAIN: &str = r##"const { Readable, Writable, Transform, PassThrough, Duplex, pipeline, finished } = require("stream");
const streamPromises = require("stream/promises");
const log = [];
// Event interleaving across independent streams depends on tick
// scheduling details; the lines are compared sorted.
const out = (...a) => log.push(a.join(" "));

// Readable push + data events
const r1 = new Readable({ read() {} });
const got1 = [];
r1.on("data", (c) => got1.push(c.toString()));
r1.on("end", () => out("r1 end", got1.join("|"), r1.readableEnded));
r1.push("ab"); r1.push("cd"); r1.push(null);

// Readable.from + objectMode + async iteration
(async () => {
  const items = [];
  for await (const x of Readable.from([1, 2, 3])) items.push(x * 10);
  out("from", items.join(","));
})();

// Writable with backpressure and drain
const chunks = [];
const w = new Writable({
  highWaterMark: 4,
  write(chunk, enc, cb) { chunks.push(chunk.toString()); setTimeout(cb, 1); },
});
const ok1 = w.write("abc");
const ok2 = w.write("defg");
out("write ret", ok1, ok2, w.writableLength, w.writableNeedDrain);
w.on("drain", () => out("drain", chunks.join("")));
w.end("!", () => out("w finished", chunks.join(""), w.writableFinished));

// Transform upper + pipeline into collecting writable
const upper = new Transform({ transform(chunk, enc, cb) { cb(null, chunk.toString().toUpperCase()); } });
const sink = [];
pipeline(
  Readable.from(["x", "y", "z"]),
  upper,
  new Writable({ write(c, e, cb) { sink.push(c.toString()); cb(); } }),
  (err) => out("pipeline", err === undefined || err === null ? "ok" : err.message, sink.join("")),
);

// pipe with PassThrough and setEncoding
const pt = new PassThrough();
pt.setEncoding("utf8");
let ptOut = "";
pt.on("data", (s) => { ptOut += typeof s + ":" + s + ";"; });
pt.on("end", () => out("passthrough", ptOut));
Readable.from([Buffer.from("he"), Buffer.from("llo")], { objectMode: false }).pipe(pt);

// errors propagate through pipeline
pipeline(
  new Readable({ read() { this.destroy(new Error("boom")); } }),
  new PassThrough(),
  (err) => out("pipeline err", err && err.message),
);

// finished on writable
const fw = new Writable({ write(c, e, cb) { cb(); } });
finished(fw, (err) => out("finished", err === undefined ? "undefined" : String(err)));
fw.end("x");

// readable event / paused mode read()
const r2 = new Readable({ read() {} });
r2.push("hello"); r2.push(" world"); r2.push(null);
r2.once("readable", () => {
  const a = r2.read(5);
  out("read5", a && a.toString(), r2.readableLength);
  r2.resume();
});

// Duplex
const d = new Duplex({
  read() { this.push("pong"); this.push(null); },
  write(c, e, cb) { out("duplex got", c.toString()); cb(); },
});
d.on("data", (c) => out("duplex data", c.toString()));
d.write("ping"); d.end();

// objectMode transform with flush
const objs = [];
const tx = new Transform({
  objectMode: true,
  transform(o, e, cb) { cb(null, { n: o.n * 2 }); },
  flush(cb) { this.push({ n: -1 }); cb(); },
});
tx.on("data", (o) => objs.push(o.n));
tx.on("end", () => out("objmode", objs.join(",")));
tx.write({ n: 1 }); tx.write({ n: 2 }); tx.end();

// write after end
const we = new Writable({ write(c, e, cb) { cb(); } });
we.on("error", (e) => out("write-after-end", e.code));
we.end(); we.write("late");

streamPromises.pipeline(Readable.from(["p", "q"]), new Writable({ objectMode: true, write(c, e, cb) { cb(); } }))
  .then(() => out("promises.pipeline ok"));

setTimeout(() => {
  out("done", typeof require("stream").Stream, require("stream") === require("stream").Stream);
  for (const line of log.sort()) console.log(line);
}, 50);
"##;

/// `node main.cjs` (v20.19.2).
const STREAM_NODE_OUTPUT: &str = r##"done function true
duplex data pong
duplex got ping
finished undefined
from 10,20,30
objmode 2,4,-1
passthrough string:he;string:llo;
pipeline err boom
pipeline ok XYZ
promises.pipeline ok
r1 end ab|cd true
read5 hello 6
w finished abcdefg! true
write ret true false 7 true
write-after-end ERR_STREAM_WRITE_AFTER_END
"##;

#[test]
fn node_stream_matches_node() {
    let root = write_fixture(
        "w3cos_node_stream_e2e",
        &[
            ("package.json", r#"{"name":"node-stream"}"#),
            ("main.cjs", STREAM_MAIN),
        ],
    );
    let lines = compile_and_run(&root, &root.join("main.cjs"), "w3cos_node_stream_e2e");
    let expected: Vec<&str> = STREAM_NODE_OUTPUT.lines().collect();
    assert_eq!(lines, expected);
    std::fs::remove_dir_all(root).ok();
}

const FS_MAIN: &str = r##"const fs = require("fs");
const fsp = require("node:fs/promises");
const path = require("path");
const os = require("os");

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "w3fs-"));
const rel = (s) => String(s).split(tmp).join("<tmp>");
const out = (...a) => console.log(...a.map(rel));
const err = (fn) => { try { fn(); return "ok"; } catch (e) { return e.code + "/" + e.syscall + "/" + e.errno + "/" + rel(e.message); } };

const f = path.join(tmp, "a.txt");
fs.writeFileSync(f, "héllo\n");
fs.appendFileSync(f, Buffer.from("world"));
out("read", JSON.stringify(fs.readFileSync(f, "utf8")), fs.readFileSync(f).length, fs.readFileSync(f, { encoding: "hex" }).slice(0, 6), Buffer.isBuffer(fs.readFileSync(f)));
const st = fs.statSync(f);
out("stat", st.isFile(), st.isDirectory(), st.size, typeof st.mtimeMs, st.mtime instanceof Date, (st.mode & 0o170000) === fs.constants.S_IFREG);
out("exists", fs.existsSync(f), fs.existsSync(path.join(tmp, "nope")), fs.statSync(path.join(tmp, "nope"), { throwIfNoEntry: false }));
out("enoent", err(() => fs.readFileSync(path.join(tmp, "nope"))));
out("mkdir", rel(fs.mkdirSync(path.join(tmp, "d1/d2/d3"), { recursive: true })), err(() => fs.mkdirSync(path.join(tmp, "d1"))));
fs.writeFileSync(path.join(tmp, "d1/b.bin"), new Uint8Array([1, 2, 3]));
out("readdir", fs.readdirSync(tmp).join(","), fs.readdirSync(path.join(tmp, "d1"), { withFileTypes: true }).map((d) => d.name + ":" + d.isDirectory()).join(","));
out("recursive", fs.readdirSync(tmp, { recursive: true }).sort().join(","));
fs.renameSync(f, path.join(tmp, "c.txt"));
fs.copyFileSync(path.join(tmp, "c.txt"), path.join(tmp, "e.txt"));
out("copy", err(() => fs.copyFileSync(path.join(tmp, "c.txt"), path.join(tmp, "e.txt"), fs.constants.COPYFILE_EXCL)), fs.readFileSync(path.join(tmp, "e.txt"), "utf8").length);
out("rmdir-notempty", err(() => fs.rmdirSync(path.join(tmp, "d1"))).split("/")[0], err(() => fs.rmSync(path.join(tmp, "d1"))).split("/")[0]);
fs.rmSync(path.join(tmp, "d1"), { recursive: true });
out("rm", fs.existsSync(path.join(tmp, "d1")), err(() => fs.rmSync(path.join(tmp, "zz"), { force: true })), err(() => fs.unlinkSync(path.join(tmp, "zz"))));
fs.symlinkSync(path.join(tmp, "c.txt"), path.join(tmp, "link"));
out("symlink", fs.lstatSync(path.join(tmp, "link")).isSymbolicLink(), fs.statSync(path.join(tmp, "link")).isFile(), rel(fs.readlinkSync(path.join(tmp, "link"))), rel(fs.realpathSync(path.join(tmp, "link"))) === rel(fs.realpathSync(path.join(tmp, "c.txt"))));
const fd = fs.openSync(path.join(tmp, "fd.txt"), "w+");
out("fd-write", fs.writeSync(fd, "abcdef"), fs.writeSync(fd, Buffer.from("XY"), 0, 2, 1));
const buf = Buffer.alloc(4);
out("fd-read", fs.readSync(fd, buf, 0, 4, 0), buf.toString(), fs.fstatSync(fd).size);
fs.ftruncateSync(fd, 3);
fs.closeSync(fd);
out("fd-closed", fs.readFileSync(path.join(tmp, "fd.txt"), "utf8"), err(() => fs.closeSync(fd)).split("/")[0]);
fs.chmodSync(path.join(tmp, "c.txt"), 0o600);
out("chmod", (fs.statSync(path.join(tmp, "c.txt")).mode & 0o777).toString(8), err(() => fs.accessSync(path.join(tmp, "c.txt"), fs.constants.R_OK)));
fs.utimesSync(path.join(tmp, "c.txt"), 1000, 2000);
out("utimes", fs.statSync(path.join(tmp, "c.txt")).mtimeMs);
fs.truncateSync(path.join(tmp, "c.txt"), 2);
out("truncate", fs.readFileSync(path.join(tmp, "c.txt"), "latin1").length);

fs.readFile(path.join(tmp, "e.txt"), "utf8", (e, data) => {
  out("cb readFile", e, data.length);
  fs.stat(path.join(tmp, "missing"), (e2) => {
    out("cb stat err", e2.code);
    (async () => {
      await fsp.writeFile(path.join(tmp, "p.txt"), "promised");
      out("promises", await fsp.readFile(path.join(tmp, "p.txt"), "utf8"), (await fsp.stat(path.join(tmp, "p.txt"))).size);
      const fh = await fsp.open(path.join(tmp, "p.txt"), "r");
      const { bytesRead, buffer } = await fh.read(Buffer.alloc(3), 0, 3, 0);
      out("filehandle", bytesRead, buffer.toString(), typeof fh.fd);
      await fh.close();
      try { await fsp.readFile(path.join(tmp, "missing")); } catch (e3) { out("promises err", e3.code, e3.syscall); }
      const ws = fs.createWriteStream(path.join(tmp, "s.txt"));
      ws.write("line1\n"); ws.end("line2\n");
      ws.on("finish", () => {
        const chunks = [];
        fs.createReadStream(path.join(tmp, "s.txt"), { encoding: "utf8", highWaterMark: 4 })
          .on("data", (c) => chunks.push(c))
          .on("end", () => {
            out("streams", chunks.length, JSON.stringify(chunks.join("")));
            const rs = fs.createReadStream(path.join(tmp, "s.txt"), { start: 2, end: 4 });
            let part = "";
            rs.on("data", (c) => { part += c; }).on("close", () => {
              out("range", part);
              fs.rmSync(tmp, { recursive: true });
              out("cleanup", fs.existsSync(tmp));
            });
          });
      });
    })();
  });
});
"##;

/// `node main.cjs` (v20.19.2).
const FS_NODE_OUTPUT: &str = r##"read "héllo\nworld" 12 68c3a9 true
stat true false 12 number true true
exists true false undefined
enoent ENOENT/open/-2/ENOENT: no such file or directory, open '<tmp>/nope'
mkdir <tmp>/d1 EEXIST/mkdir/-17/EEXIST: file already exists, mkdir '<tmp>/d1'
readdir a.txt,d1 b.bin:false,d2:true
recursive a.txt,d1,d1/b.bin,d1/d2,d1/d2/d3
copy EEXIST/copyfile/-17/EEXIST: file already exists, copyfile '<tmp>/c.txt' -> '<tmp>/e.txt' 11
rmdir-notempty ENOTEMPTY ERR_FS_EISDIR
rm false ok ENOENT/unlink/-2/ENOENT: no such file or directory, unlink '<tmp>/zz'
symlink true true <tmp>/c.txt true
fd-write 6 2
fd-read 4 aXYd 6
fd-closed aXY EBADF
chmod 600 ok
utimes 2000000
truncate 2
cb readFile null 11
cb stat err ENOENT
promises promised 8
filehandle 3 pro number
promises err ENOENT open
streams 3 "line1\nline2\n"
range ne1
cleanup false
"##;

#[test]
fn node_fs_matches_node() {
    let root = write_fixture(
        "w3cos_node_fs_e2e",
        &[
            ("package.json", r#"{"name":"node-fs"}"#),
            ("main.cjs", FS_MAIN),
        ],
    );
    let lines = compile_and_run(&root, &root.join("main.cjs"), "w3cos_node_fs_e2e");
    let expected: Vec<&str> = FS_NODE_OUTPUT.lines().collect();
    assert_eq!(lines, expected);
    std::fs::remove_dir_all(root).ok();
}

const CRYPTO_MAIN: &str = r##"'use strict';
const crypto = require('crypto');
const nc = require('node:crypto');
console.log('same', crypto === nc);
for (const alg of ['md5', 'sha1', 'sha224', 'sha256', 'sha384', 'sha512']) {
  console.log(alg, crypto.createHash(alg).update('hello ').update('world').digest('hex'));
}
console.log('empty', crypto.createHash('sha256').digest('hex'));
console.log('b64', crypto.createHash('sha1').update('abc').digest('base64'));
console.log('b64url', crypto.createHash('sha256').update('abc').digest('base64url'));
const d = crypto.createHash('md5').update(Buffer.from([1, 2, 3])).digest();
console.log('buf', Buffer.isBuffer(d), d.length, d.toString('hex'));
console.log('enc', crypto.createHash('sha1').update('68656c6c6f', 'hex').digest('hex'));
console.log('u8', crypto.createHash('sha1').update(new Uint8Array([104, 105])).digest('hex'));
console.log('upper', crypto.createHash('SHA256').update('x').digest('hex'));
console.log('dashed', crypto.createHash('sha-256').update('x').digest('hex') === crypto.createHash('sha256').update('x').digest('hex'));
const long = 'a'.repeat(1000);
console.log('long', crypto.createHash('sha512').update(long).digest('hex').slice(0, 32));
const h = crypto.createHash('sha256').update('part1');
const c = h.copy();
c.update('part2');
console.log('copy', h.digest('hex').slice(0, 16), c.digest('hex').slice(0, 16));
try { h.update('x'); } catch (e) { console.log('finalized', e.code, e.message); }
try { h.digest(); } catch (e) { console.log('finalized2', e.code, e.message); }
try { crypto.createHash('nope'); } catch (e) { console.log('unsupported', e.message); }
console.log('hmac', crypto.createHmac('sha256', 'key').update('The quick brown fox jumps over the lazy dog').digest('hex'));
console.log('hmac-md5', crypto.createHmac('md5', 'key').update('msg').digest('hex'));
console.log('hmac-long-key', crypto.createHmac('sha1', 'k'.repeat(100)).update('m').digest('hex'));
console.log('hmac512', crypto.createHmac('sha512', Buffer.from('secret')).update('a').update('b').digest('base64'));
console.log('pbkdf2', crypto.pbkdf2Sync('password', 'salt', 1000, 32, 'sha256').toString('hex'));
console.log('pbkdf2-sha1', crypto.pbkdf2Sync('password', 'salt', 2, 20, 'sha1').toString('hex'));
const r = crypto.randomBytes(16);
console.log('random', Buffer.isBuffer(r), r.length);
const u = crypto.randomUUID();
console.log('uuid', /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(u));
const arr = new Uint32Array(4);
console.log('grv', crypto.getRandomValues(arr) === arr, arr.length);
console.log('webcrypto', typeof crypto.webcrypto.getRandomValues, typeof crypto.webcrypto.randomUUID);
const fill = Buffer.alloc(8);
console.log('fill', crypto.randomFillSync(fill, 2, 4) === fill, fill[0], fill[1], fill[6], fill[7]);
let okInt = true;
for (let i = 0; i < 200; i++) { const n = crypto.randomInt(3, 7); if (n < 3 || n >= 7 || n !== Math.floor(n)) okInt = false; }
console.log('randomInt', okInt, crypto.randomInt(1) === 0);
console.log('tse', crypto.timingSafeEqual(Buffer.from('abc'), Buffer.from('abc')), crypto.timingSafeEqual(Buffer.from('abc'), Buffer.from('abd')));
try { crypto.timingSafeEqual(Buffer.from('a'), Buffer.from('ab')); } catch (e) { console.log('tse-len', e.code, e.name, e.message); }
const hashes = crypto.getHashes();
console.log('hashes', ['md5', 'sha1', 'sha256', 'sha512'].every((x) => hashes.includes(x)));
crypto.randomBytes(4, (err, buf) => {
  console.log('random-cb', err, buf.length);
  crypto.pbkdf2('pw', 'salt', 1, 8, 'sha512', (err2, key) => {
    console.log('pbkdf2-cb', err2, key.toString('hex'));
  });
});
"##;

/// `node main.cjs` (v20.19.2) output for `CRYPTO_MAIN`.
const CRYPTO_NODE_OUTPUT: &str = r##"same true
md5 5eb63bbbe01eeed093cb22bb8f5acdc3
sha1 2aae6c35c94fcfb415dbe95f408b9ce91ee846ed
sha224 2f05477fc24bb4faefd86517156dafdecec45b8ad3cf2522a563582b
sha256 b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9
sha384 fdbd8e75a67f29f701a4e040385e2e23986303ea10239211af907fcbb83578b3e417cb71ce646efd0819dd8c088de1bd
sha512 309ecc489c12d6eb4cc40f50c902f2b4d0ed77ee511a7c7a9bcd3ca86d4cd86f989dd35bc5ff499670da34255b45b0cfd830e81f605dcf7dc5542e93ae9cd76f
empty e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
b64 qZk+NkcGgWq6PiVxeFDCbJzQ2J0=
b64url ungWv48Bz-pBQUDeXa4iI7ADYaOWF3qctBD_YfIAFa0
buf true 16 5289df737df57326fcdd22597afb1fac
enc aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d
u8 c22b5f9178342609428d6f51b2c5af4c0bde6a42
upper 2d711642b726b04401627ca9fbac32f5c8530fb1903cc4db02258717921a4881
dashed true
long 67ba5535a46e3f86dbfbed8cbbaf0125
copy 59e0a3cc7a0c45f9 4986fc0feee88f7e
finalized ERR_CRYPTO_HASH_FINALIZED Digest already called
finalized2 ERR_CRYPTO_HASH_FINALIZED Digest already called
unsupported Digest method not supported
hmac f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8
hmac-md5 18e3548c59ad40dd03907b7aeee71d67
hmac-long-key 014f08e9aabf7980d07a205a4c638743915ff915
hmac512 ZNyU2swDduB7NCka3KCRHI+QM4YW4etuGJN/wUQzN40zUwsrnddcXuptyH3cGddTBiU31g9gHbw3jVs7ltNClQ==
pbkdf2 632c2812e46d4604102ba7618e9d6d7d2f8128f6266b4a03264d2a0460b7dcb3
pbkdf2-sha1 ea6c014dc72d6f8ccd1ed92ace1d41f0d8de8957
random true 16
uuid true
grv true 4
webcrypto function function
fill true 0 0 0 0
randomInt true true
tse true false
tse-len ERR_CRYPTO_TIMING_SAFE_EQUAL_LENGTH RangeError Input buffers must have the same byte length
hashes true
random-cb null 4
pbkdf2-cb null 9bbcdf4f0b03e358
"##;

#[test]
fn node_crypto_matches_node() {
    let root = write_fixture(
        "w3cos_node_crypto_e2e",
        &[
            ("package.json", r#"{"name":"node-crypto"}"#),
            ("main.cjs", CRYPTO_MAIN),
        ],
    );
    let lines = compile_and_run(&root, &root.join("main.cjs"), "w3cos_node_crypto_e2e");
    let expected: Vec<&str> = CRYPTO_NODE_OUTPUT.lines().collect();
    assert_eq!(lines, expected);
    std::fs::remove_dir_all(root).ok();
}

const ZLIB_MAIN: &str = r##"'use strict';
const zlib = require('zlib');
const { promisify } = require('util');
const { pipeline, Readable, Writable } = require('stream');
const text = 'hello zlib! '.repeat(50);
const input = Buffer.from(text);
const pairs = [
  ['deflateSync', 'inflateSync'], ['deflateRawSync', 'inflateRawSync'],
  ['gzipSync', 'gunzipSync'], ['gzipSync', 'unzipSync'], ['deflateSync', 'unzipSync'],
];
for (const [enc, dec] of pairs) {
  const packed = zlib[enc](input);
  const out = zlib[dec](packed);
  console.log(enc, dec, Buffer.isBuffer(packed), packed.length < input.length, out.toString() === text);
}
console.log('gzip header', zlib.gzipSync('x').subarray(0, 10).toString('hex'));
console.log('zlib header', zlib.deflateSync('x').subarray(0, 2).toString('hex'),
  zlib.deflateSync('x', { level: 1 }).subarray(0, 2).toString('hex'),
  zlib.deflateSync('x', { level: 9 }).subarray(0, 2).toString('hex'));
console.log('level0', zlib.deflateRawSync('abc', { level: 0 }).toString('hex'));
console.log('string input', zlib.inflateSync(zlib.deflateSync('héllo')).toString());
console.log('u8 input', zlib.inflateRawSync(zlib.deflateRawSync(new Uint8Array([1, 2, 3]))).toString('hex'));
const fromNode = Buffer.from('H4sIAAAAAAAAA8tIzcnJVyjPL8pJAQCFEUoNCwAAAA==', 'base64');
console.log('node gzip', zlib.gunzipSync(fromNode).toString());
const fromNodeDeflate = Buffer.from('eJzLSM3JyVcozy/KSQEAGgsEXQ==', 'base64');
console.log('node deflate', zlib.inflateSync(fromNodeDeflate).toString());
console.log('empty', zlib.inflateSync(zlib.deflateSync(Buffer.alloc(0))).length, zlib.gunzipSync(zlib.gzipSync('')).length);
try { zlib.inflateSync(Buffer.from('not compressed')); } catch (e) {
  console.log('bad', e.name, e.code, e.errno, e.message);
}
try { zlib.gunzipSync(Buffer.from('nope nope')); } catch (e) {
  console.log('bad gzip', e.code, e.errno, e.message);
}
const truncated = zlib.deflateSync(input).subarray(0, 10);
try { zlib.inflateSync(truncated); } catch (e) {
  console.log('truncated', e.code, e.errno, e.message);
}
console.log('constants', zlib.constants.Z_BEST_COMPRESSION, zlib.constants.Z_DEFAULT_COMPRESSION, zlib.Z_BEST_SPEED, zlib.constants.Z_SYNC_FLUSH);
console.log('types', typeof zlib.createGzip, typeof zlib.createGunzip, typeof zlib.createDeflate, typeof zlib.createInflate);
zlib.deflate(input, (err, packed) => {
  console.log('cb deflate', err, packed.length < input.length);
  zlib.inflate(packed, { }, (err2, out) => {
    console.log('cb inflate', err2, out.toString() === text);
    zlib.inflate(Buffer.from('zzz'), (err3, out3) => {
      console.log('cb error', err3.code, out3);
      promisify(zlib.gzip)('promised').then((gz) => promisify(zlib.gunzip)(gz)).then((buf) => {
        console.log('promisify', buf.toString());
        const chunks = [];
        pipeline(
          Readable.from([Buffer.from('stream '), Buffer.from('data '), Buffer.from('here')]),
          zlib.createGzip(),
          zlib.createGunzip(),
          new Writable({ write(chunk, enc, cb) { chunks.push(chunk); cb(); } }),
          (err4) => {
            console.log('pipeline', err4, Buffer.concat(chunks).toString());
          },
        );
      });
    });
  });
});
"##;

/// `node main.cjs` (v20.19.2) output for `ZLIB_MAIN`. Compressed bytes are
/// only compared where zlib and miniz_oxide agree (headers, stored blocks).
const ZLIB_NODE_OUTPUT: &str = r##"deflateSync inflateSync true true true
deflateRawSync inflateRawSync true true true
gzipSync gunzipSync true true true
gzipSync unzipSync true true true
deflateSync unzipSync true true true
gzip header 1f8b0800000000000003
zlib header 789c 7801 78da
level0 010300fcff616263
string input héllo
u8 input 010203
node gzip hello world
node deflate hello world
empty 0 0
bad Error Z_DATA_ERROR -3 incorrect header check
bad gzip Z_DATA_ERROR -3 incorrect header check
truncated Z_BUF_ERROR -5 unexpected end of file
constants 9 -1 1 2
types function function function function
cb deflate null true
cb inflate null true
cb error Z_DATA_ERROR undefined
promisify promised
pipeline undefined stream data here
"##;

#[test]
fn node_zlib_matches_node() {
    let root = write_fixture(
        "w3cos_node_zlib_e2e",
        &[
            ("package.json", r#"{"name":"node-zlib"}"#),
            ("main.cjs", ZLIB_MAIN),
        ],
    );
    let lines = compile_and_run(&root, &root.join("main.cjs"), "w3cos_node_zlib_e2e");
    let expected: Vec<&str> = ZLIB_NODE_OUTPUT.lines().collect();
    assert_eq!(lines, expected);
    std::fs::remove_dir_all(root).ok();
}
