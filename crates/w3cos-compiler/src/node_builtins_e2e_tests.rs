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
