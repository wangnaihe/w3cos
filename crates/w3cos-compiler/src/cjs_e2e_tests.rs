//! End-to-end CommonJS linking: compile a mixed CommonJS/ESM project with
//! the AOT pipeline, run it, and compare the console output with Node's
//! (recorded with Node 20 for the same fixture).

use crate::esm_codegen::try_generate_with_bodies;
use crate::esm_resolver::{EsmBundle, EsmResolver};
use std::path::{Path, PathBuf};

pub(crate) fn write_fixture(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let root = std::env::temp_dir().join(name);
    let _ = std::fs::remove_dir_all(&root);
    for (path, contents) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    root
}

/// Compile `entry` under `root`, run it, and return the `console.*` lines
/// (and an `UNCAUGHT ...` line if evaluation threw).
pub(crate) fn compile_and_run(root: &Path, entry: &Path, crate_name: &str) -> Vec<String> {
    compile_and_run_with_env(root, entry, crate_name, &[])
}

/// [`compile_and_run`] with extra environment variables for the app.
pub(crate) fn compile_and_run_with_env(
    root: &Path,
    entry: &Path,
    crate_name: &str,
    env: &[(&str, &str)],
) -> Vec<String> {
    let resolver = EsmResolver::new(root);
    let parsed = resolver
        .parse_graph_from_entry(entry)
        .unwrap_or_else(|error| panic!("graph: {error:#}"));
    let bundle = EsmBundle::build(
        &parsed,
        &resolver,
        &resolver.resolve_entry(entry).unwrap().path,
    );
    assert!(
        bundle.unresolved.is_empty(),
        "unresolved bindings: {:#?}",
        bundle.unresolved
    );
    let code = try_generate_with_bodies(&bundle).unwrap_or_else(|error| panic!("codegen: {error}"));
    let crate_dir = root.join("bundle_run");
    std::fs::create_dir_all(crate_dir.join("src")).unwrap();
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let core_path = manifest_dir.join("../w3cos-core");
    let runtime_path = manifest_dir.join("../w3cos-runtime");
    // Compiled module code reaches the DOM host (`console` inside functions
    // goes through the jsdom window), so the fixture links w3cos-runtime
    // without renderer defaults, like the other generated-bundle tests.
    std::fs::write(
        crate_dir.join("Cargo.toml"),
        format!(
            "[package]\nname = {crate_name:?}\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\nw3cos-core = {{ path = {core_path:?} }}\nw3cos-runtime = {{ path = {runtime_path:?}, default-features = false, features = [\"web-graphics-advanced\", \"web-media-advanced\"] }}\n\n[workspace]\n"
        ),
    )
    .unwrap();
    std::fs::write(
        crate_dir.join("src/main.rs"),
        format!(
            r#"#![allow(warnings)]
{code}
fn main() {{
    // Like `node main.js`: run the entry, then the event loop (microtasks,
    // nextTick-style jobs and timers) until no work is left (10 s cap).
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {{
        run_entry();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {{
            let ran = w3cos_runtime::jsdom::drain_microtasks() + w3cos_runtime::jsdom::tick_timers();
            if ran > 0 {{
                continue;
            }}
            match w3cos_runtime::jsdom::next_timer_deadline() {{
                Some(at) => std::thread::sleep(at.saturating_duration_since(std::time::Instant::now())),
                None => break,
            }}
        }}
    }}));
    if let Err(payload) = outcome {{
        let message = payload
            .downcast_ref::<w3cos_core::PanicValue>()
            .map(|value| {{
                let error = &value.0;
                let message = error.get_property("message");
                if message.is_undefined() {{
                    error.to_js_string()
                }} else {{
                    format!("{{}}: {{}}", error.get_property("name").to_js_string(), message.to_js_string())
                }}
            }})
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default();
        eprintln!("[js.console.log] UNCAUGHT {{message}}");
    }}
}}
"#
        ),
    )
    .unwrap();
    // Share the workspace target dir: w3cos-runtime's dependency tree is
    // large and already built for the other generated-bundle tests.
    let target_dir = manifest_dir.join("../../target");
    let output = std::process::Command::new("cargo")
        .args(["run", "--quiet", "--offline"])
        .current_dir(&crate_dir)
        .env("CARGO_TARGET_DIR", &target_dir)
        .env("W3COS_JS_CONSOLE", "1")
        .envs(env.iter().copied())
        .output()
        .expect("cargo should be available");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "generated CommonJS bundle should build and run.\n--- stderr ---\n{stderr}\n--- code ---\n{code}"
    );
    stderr
        .lines()
        .filter_map(|line| {
            line.strip_prefix("[js.console.log] ")
                .or_else(|| line.strip_prefix("[js.console.error] "))
        })
        .map(str::to_string)
        .collect()
}

const BASIC_FILES: &[(&str, &str)] = &[
    ("package.json", r##"{"name":"app"}"##),
    (
        "src/main.mjs",
        r##"import cjsDefault, { add, counter, named } from "./lib/math.js";
import * as ns from "./lib/math.js";
import data from "./lib/data.cjs";
import dual from "dual";
import { greet } from "legacy-cjs";
import fn from "./lib/fn.js";
import tsMod, { tsNamed } from "./lib/ts.js";
import umd from "./lib/umd.js";
import "./lib/cycle.js";
import "./lib/semantics.js";
console.log("default.add", cjsDefault.add(2, 3));
console.log("named.add", add(4, 5));
console.log("live counter", cjsDefault.counter, "snapshot", counter, named);
console.log("ns.default===default", ns.default === cjsDefault, ns.add === add);
console.log("dual(import)", dual);
console.log("greet", greet("w3cos"));
console.log("data", data.kind, data.json.name);
console.log("fn", fn(), fn.extra);
console.log("ts interop", typeof tsMod, tsMod.default, tsNamed, tsMod.__esModule);
console.log("umd", umd.umd);
"##,
    ),
    (
        "src/lib/math.js",
        r##""use strict";
const helper = require("./helper");
let counter = 0;
exports.add = function (a, b) { counter++; exports.counter = counter; return helper.twice(a + b) / 2; };
exports.counter = counter;
exports.named = "n";
exports.dual = require("dual");
console.log("math.this===exports", this === module.exports, typeof __filename, __filename.endsWith("math.js"), __dirname.endsWith("lib"));
console.log("dual(require)", exports.dual);
"##,
    ),
    (
        "src/lib/helper.js",
        r##"module.exports = { twice(x) { return x * 2; } };
"##,
    ),
    (
        "src/lib/data.cjs",
        r##"module.exports = { kind: "cjs-file", json: require("./info.json") };
"##,
    ),
    ("src/lib/info.json", r##"{"name": "info"}"##),
    (
        "src/lib/a.js",
        r##"exports.done = false;
const b = require('./b.js');
console.log('in a, b.done =', b.done);
exports.done = true;
"##,
    ),
    (
        "src/lib/b.js",
        r##"exports.done = false;
const a = require('./a.js');
console.log('in b, a.done =', a.done);
exports.done = true;
"##,
    ),
    (
        "src/lib/cycle.js",
        r##"const a = require('./a.js');
const b = require('./b.js');
console.log('in cycle, a.done =', a.done, 'b.done =', b.done);
"##,
    ),
    (
        "src/lib/count.js",
        r##"module.exports = { n: 0 };
"##,
    ),
    (
        "src/lib/throws.js",
        r##"const c = require('./count');
c.n++;
throw new Error('boom ' + c.n);
"##,
    ),
    (
        "src/lib/early.js",
        r##"exports.before = 1;
if (exports.before) return;
exports.after = 2;
"##,
    ),
    (
        "src/lib/reassign.js",
        r##"exports = { lost: true };
module.exports.kept = true;
"##,
    ),
    (
        "src/lib/semantics.js",
        r##"const h1 = require("./helper");
const h2 = require("./helper.js");
console.log("cached identity", h1 === h2, require.cache[require.resolve("./helper")].exports === h1);
console.log("module fields", module.loaded, typeof module.id, module.exports === exports, typeof require.resolve);
for (let i = 0; i < 2; i++) {
  try { require("./throws"); } catch (e) { console.log("caught", e.message); }
}
try { require("optional-missing-dep"); } catch (e) { console.log("missing", e.code); }
const name = "./" + "helper";
console.log("dynamic require of a known specifier", require(name) === h1);
const early = require("./early");
console.log("early return", early.before, early.after);
console.log("exports reassignment", JSON.stringify(require("./reassign")));
"##,
    ),
    (
        "src/lib/fn.js",
        r##"module.exports = function f() { return 'fn'; };
module.exports.extra = 1;
"##,
    ),
    (
        "src/lib/ts.js",
        r##""use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.tsNamed = exports.default = void 0;
exports.default = "ts-default";
exports.tsNamed = "ts-named";
"##,
    ),
    (
        "src/lib/umd.js",
        r##"(function (root, factory) {
  if (typeof module === "object" && module.exports) module.exports = factory();
  else root.Umd = factory();
})(this, function () { return { umd: true }; });
"##,
    ),
    (
        "node_modules/dual/package.json",
        r##"{"name":"dual","exports":{".":{"import":"./esm.mjs","require":"./cjs.cjs"}}}"##,
    ),
    (
        "node_modules/dual/esm.mjs",
        r##"export default "dual-esm";
"##,
    ),
    (
        "node_modules/dual/cjs.cjs",
        r##"module.exports = "dual-cjs";
"##,
    ),
    (
        "node_modules/legacy-cjs/package.json",
        r##"{"main":"lib/index"}"##,
    ),
    (
        "node_modules/legacy-cjs/lib/index.js",
        r##"exports.greet = (n) => "hi " + n;
"##,
    ),
];

/// `node src/main.mjs` on the fixture above.
const BASIC_NODE_OUTPUT: &str = r##"math.this===exports true string true true
dual(require) dual-cjs
in b, a.done = false
in a, b.done = true
in cycle, a.done = true b.done = true
cached identity true true
module fields false string true function
caught boom 1
caught boom 2
missing MODULE_NOT_FOUND
dynamic require of a known specifier true
early return 1 undefined
exports reassignment {"kept":true}
default.add 5
named.add 9
live counter 2 snapshot 0 n
ns.default===default true true
dual(import) dual-esm
greet hi w3cos
data cjs-file info
fn fn 1
ts interop object ts-default ts-named true
umd true
"##;

#[test]
fn commonjs_linking_matches_node() {
    let root = write_fixture("w3cos_cjs_e2e_basic", BASIC_FILES);
    let lines = compile_and_run(&root, &root.join("src/main.mjs"), "w3cos_cjs_e2e_basic");
    let expected: Vec<&str> = BASIC_NODE_OUTPUT.lines().collect();
    assert_eq!(lines, expected);
    std::fs::remove_dir_all(root).ok();
}

const ADVANCED_FILES: &[(&str, &str)] = &[
    (
        "package.json",
        r##"{"name":"app2","imports":{"#cfg":{"w3cos":"./src/cfg-w3cos.js","node":"./src/cfg-node.js","default":"./src/cfg.js"}}}"##,
    ),
    (
        "src/main.cjs",
        r##"const esm = require("./esm.mjs");
console.log("require(esm)", esm.value, typeof esm.default, esm.default());
try { require("dgram"); } catch (e) { console.log("builtin", e.code); }
try { require("node:repl"); } catch (e) { console.log("node: builtin", e.code); }
console.log("#cfg", require("#cfg").which);
console.log("w3cos condition", require("cond-pkg"));
const reexp = require("./reexport-user.mjs");
console.log("export * from cjs", reexp.alpha, reexp.beta);
console.log("hidden name", reexp.hidden);
console.log("require.resolve", require.resolve("./esm.mjs").endsWith("/src/esm.mjs"));
module.exports = "entry-done";
"##,
    ),
    (
        "src/esm.mjs",
        r##"export const value = 42;
export default function hello() { return "hello"; }
"##,
    ),
    (
        "src/cfg.js",
        r##"exports.which = "default";
"##,
    ),
    (
        "src/cfg-node.js",
        r##"exports.which = "node";
"##,
    ),
    (
        "src/cfg-w3cos.js",
        r##"exports.which = "w3cos";
"##,
    ),
    (
        "src/lib.js",
        r##"exports.alpha = "a";
exports.beta = "b";
const key = "hid" + "den";
exports[key] = "found-by-importer";
"##,
    ),
    (
        "src/reexport-user.mjs",
        r##"export * from "./lib.js";
export { hidden } from "./lib.js";
"##,
    ),
    (
        "node_modules/cond-pkg/package.json",
        r##"{"name":"cond-pkg","exports":{"types":"./i.d.ts","w3cos":"./w3cos.js","node":"./node.js","default":"./default.js"}}"##,
    ),
    (
        "node_modules/cond-pkg/w3cos.js",
        r##"module.exports = "w3cos build";
"##,
    ),
    (
        "node_modules/cond-pkg/node.js",
        r##"module.exports = "node build";
"##,
    ),
    (
        "node_modules/cond-pkg/default.js",
        r##"module.exports = "default build";
"##,
    ),
];

/// CommonJS entry; `require(esm)` (Node 22 semantics: the namespace), Node
/// builtins w3cos-node does not provide yet (they throw a coded error), `#imports` and
/// package `exports` picking the `w3cos` condition first, `export *` from a
/// CommonJS facade, and a name only the importer knows about.
const ADVANCED_OUTPUT: &str = r##"require(esm) 42 function hello
builtin ERR_W3COS_BUILTIN_UNAVAILABLE
node: builtin ERR_W3COS_BUILTIN_UNAVAILABLE
#cfg w3cos
w3cos condition w3cos build
export * from cjs a b
hidden name found-by-importer
require.resolve true
"##;

#[test]
fn commonjs_entry_conditions_and_interop() {
    let root = write_fixture("w3cos_cjs_e2e_advanced", ADVANCED_FILES);
    let lines = compile_and_run(&root, &root.join("src/main.cjs"), "w3cos_cjs_e2e_advanced");
    let expected: Vec<&str> = ADVANCED_OUTPUT.lines().collect();
    assert_eq!(lines, expected);
    std::fs::remove_dir_all(root).ok();
}

/// Real-world CommonJS packages (VS Code dependencies), compared with Node.
///
/// Opt-in: `scripts/cjs-real-packages.sh` installs the pinned packages and
/// entry scripts into a directory and runs this test with
/// `W3COS_CJS_REAL_DIR` pointing at it (optionally `W3COS_CJS_REAL_ONLY` =
/// comma-separated entry names). Each `src/*.cjs|*.mjs` entry is compiled,
/// run, and compared line by line with `node <entry>`.
#[test]
#[ignore]
fn real_world_commonjs_packages_match_node() {
    let Some(dir) = std::env::var_os("W3COS_CJS_REAL_DIR").map(PathBuf::from) else {
        eprintln!("W3COS_CJS_REAL_DIR not set; skipping");
        return;
    };
    let only: Option<Vec<String>> = std::env::var("W3COS_CJS_REAL_ONLY")
        .ok()
        .map(|list| list.split(',').map(str::to_string).collect());
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir.join("src"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("cjs" | "mjs")
            )
        })
        .filter(|path| {
            only.as_ref().is_none_or(|only| {
                only.iter()
                    .any(|name| path.file_stem().is_some_and(|stem| stem == name.as_str()))
            })
        })
        .collect();
    entries.sort();
    let mut failures = Vec::new();
    for entry in &entries {
        let name = entry.file_stem().unwrap().to_string_lossy().to_string();
        let node = std::process::Command::new("node")
            .arg(entry)
            .output()
            .expect("node is required for the real-package comparison");
        let expected: Vec<String> = String::from_utf8_lossy(&node.stdout)
            .lines()
            .map(str::to_string)
            .collect();
        let crate_name = format!("w3cos_cjs_real_{}", name.replace('-', "_"));
        let actual = std::panic::catch_unwind(|| compile_and_run(&dir, entry, &crate_name));
        match actual {
            Ok(lines) if lines == expected => eprintln!("PASS {name} ({} lines)", lines.len()),
            Ok(lines) => {
                eprintln!("FAIL {name}\n  node: {expected:?}\n  w3cos: {lines:?}");
                failures.push(name);
            }
            Err(_) => {
                eprintln!("FAIL {name}: build or run failed (see panic above)");
                failures.push(name);
            }
        }
    }
    assert!(failures.is_empty(), "real packages failing: {failures:?}");
}

/// Class elements are non-enumerable: Code-OSS's `ProxyChannel.fromService`
/// walks a service with `for-in` and must not see prototype methods such as
/// `onDynamicBrowserViewEvent` (Node 20 output).
#[test]
fn class_methods_accessors_and_constructor_are_not_enumerable() {
    let root = write_fixture(
        "w3cos_class_method_enumerability",
        &[
            (
                "package.json",
                r#"{ "name": "p", "version": "0.1.0", "type": "module" }"#,
            ),
            (
                "main.js",
                r#"class Base { baseMethod() {} get g() { return 1 } static s() {} }
class Svc extends Base { constructor() { super(); this.own = 1; this.onDidX = 2 } onDynamicFoo(a) { return this.own } }
const keys = []; for (const k in new Svc()) keys.push(k)
console.log(keys.join(','))
console.log(JSON.stringify(Object.getOwnPropertyDescriptor(Svc.prototype, 'onDynamicFoo')))
console.log(Object.keys(Svc.prototype).join(','), Object.keys(Svc).join(','))
const o = { m() {}, get a() { return 1 } }; const ok = []; for (const k in o) ok.push(k); console.log(ok.join(','))
"#,
            ),
        ],
    );
    let lines = compile_and_run(
        &root,
        &root.join("main.js"),
        "w3cos_class_method_enumerability",
    );
    assert_eq!(
        lines,
        vec![
            "own,onDidX",
            "{\"configurable\":true,\"enumerable\":false,\"writable\":true}",
            " ",
            "m,a",
        ]
    );
}

/// `super(message)` from a class extending a builtin error installs the
/// error's own fields on the derived instance, and classes own their
/// `name` (Node: validateString's ERR_INVALID_ARG_TYPE message).
#[test]
fn error_subclass_super_sets_message_and_classes_own_name() {
    let root = write_fixture(
        "w3cos_error_subclass_super",
        &[
            (
                "package.json",
                r#"{ "name": "p", "version": "0.1.0", "type": "module" }"#,
            ),
            (
                "main.js",
                r#"class A extends Error { constructor(m) { super(m) } }
class B extends Error { constructor(m) { const x = m + "!"; super(x) } }
class C extends Error { constructor(m) { let msg = `a${m}`; super(msg) } }
class D extends Error { constructor(n) { let d; if (n) { d = 1 } else { d = 2 } super("d" + d) } }
class F extends Error { constructor(m) { let msg = "x"; msg += m; super(msg) } }
class G extends Error { constructor(m, e) { e = e.replace(/^not /, ""); super(m + e) } }
class H extends Error { constructor(m) { super(m); this.code = 1 } }
for (const K of [A, B, C, D, F, G, H]) console.log(K.name, JSON.stringify(new K("m", "not y").message))
class Plain {}
const e = new A("boom");
class K2 extends Error {}
console.log(new K2("k", { cause: 7 }).cause, new K2("k").message)
console.log(Plain.name, A.name, e instanceof A, e instanceof Error, String(e), e.stack.split("\n")[0], Object.keys(e).length)
class T extends TypeError { constructor(m) { super(m); this.name = "T" } }
console.log(String(new T("bad")), new T("x") instanceof TypeError, e.cause)
function mk() { class Inner extends RangeError { constructor(m) { super(m) } } return Inner }
const I = mk();
console.log(I.name, new I("r").message, new I("r") instanceof RangeError)
class S { static name = "custom" }
class N { static get name() { return "getter" } }
console.log(S.name, N.name, Object.keys(A).length, Object.keys(Plain).length)
"#,
            ),
        ],
    );
    let lines = compile_and_run(&root, &root.join("main.js"), "w3cos_error_subclass_super");
    assert_eq!(
        lines,
        vec![
            "A \"m\"",
            "B \"m!\"",
            "C \"am\"",
            "D \"d1\"",
            "F \"xm\"",
            "G \"my\"",
            "H \"m\"",
            "7 k",
            "Plain A true true Error: boom Error: boom 0",
            "T: bad true undefined",
            "Inner r true",
            "custom getter 0 0",
        ]
    );
}

/// `new.target` in a class constructor (and arrows inside it) is the class
/// that was constructed, also through `super()` (zod's `ZodError` does
/// `Object.setPrototypeOf(this, new.target.prototype)` after `super()`).
#[test]
fn class_constructor_new_target_is_the_constructed_class() {
    let root = write_fixture(
        "w3cos_class_new_target",
        &[
            (
                "package.json",
                r#"{ "name": "p", "version": "0.1.0", "type": "module" }"#,
            ),
            (
                "main.js",
                r#"class Base { constructor() { this.kind = new.target.name; const arrow = () => new.target; this.same = arrow() === new.target } }
class Derived extends Base { constructor() { super(); this.proto = new.target.prototype === Derived.prototype } }
class ZodError extends Error { constructor(issues) { super(); const actualProto = new.target.prototype; Object.setPrototypeOf(this, actualProto); this.issues = issues } }
const b = new Base();
const d = new Derived();
const z = new ZodError([1]);
console.log(b.kind, b.same, d.kind, d.same, d.proto)
console.log(z instanceof ZodError, z instanceof Error, z.issues.length)
"#,
            ),
        ],
    );
    let lines = compile_and_run(&root, &root.join("main.js"), "w3cos_class_new_target");
    assert_eq!(lines, vec!["Base true Derived true true", "true true 1"]);
    std::fs::remove_dir_all(root).ok();
}

#[test]
/// A primitive string inherits `String.prototype` methods by value, so
/// `s.startsWith` is a function and `s?.startsWith(x)` calls it (Code-OSS
/// `validatedIpcMain` checks `channel?.startsWith("vscode:")`).
fn string_methods_read_through_string_prototype() {
    let root = write_fixture(
        "w3cos_string_proto_read",
        &[
            ("package.json", r#"{"name":"sp","type":"module"}"#),
            (
                "main.mjs",
                r#"const c = "vscode:x";
const f = c.startsWith; console.log("1", typeof f);
console.log("2", c?.startsWith("vscode:"), c.startsWith("vscode:"));
console.log("3", typeof c?.startsWith, c?.length);
const o = { m() { return 7; } }; console.log("4", o?.m());
const n = 5; console.log("5", n?.toFixed(1));
function g(ch) { return ch?.startsWith("vs"); } console.log("6", g("vsx"));
"#,
            ),
        ],
    );
    let lines = compile_and_run(&root, &root.join("main.mjs"), "w3cos_string_proto_read");
    assert_eq!(
        lines,
        vec![
            "1 function",
            "2 true true",
            "3 function 8",
            "4 7",
            "5 5.0",
            "6 true",
        ]
    );
}
