//! Differential harness: Cranelift backend B vs the rustc backend.
//!
//! ```text
//! cargo run --profile wpt -p w3cos-cranelift --example cranelift_diff -- \
//!     [--test262 <checkout> --prefix test/language/expressions ... --limit N] \
//!     [--work target/cranelift-diff]
//! ```
//!
//! Every case is lowered once to W3IR, then run twice: JIT-compiled by
//! Cranelift in this process, and through `w3ir_aot` + rustc in one batch
//! fixture crate. Outcomes (normal completion or the thrown value) must match.
//! Cases are a built-in micro / bench corpus plus test262 files that only
//! need a minimal operator-only `assert` prelude plus the host globals that
//! `w3cos-core` provides alone ([`CORE_GLOBALS`]), passed to both backends as
//! identical captures, so both see exactly the same program.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use w3cos_core::Value;

const PRELUDE: &str = r#"
function Test262Error(message) { this.message = message === undefined ? "" : message; }
Test262Error.prototype.name = "Test262Error";
var assert = function (mustBeTrue, message) {
  if (mustBeTrue === true) { return; }
  throw new Test262Error(message);
};
assert._isSameValue = function (a, b) {
  if (a === b) { return a !== 0 || 1 / a === 1 / b; }
  return a !== a && b !== b;
};
assert.sameValue = function (actual, expected, message) {
  if (!assert._isSameValue(actual, expected)) { throw new Test262Error(message); }
};
assert.notSameValue = function (actual, unexpected, message) {
  if (assert._isSameValue(actual, unexpected)) { throw new Test262Error(message); }
};
assert.throws = function (expectedErrorConstructor, func, message) {
  try { func(); } catch (thrown) { return; }
  throw new Test262Error(message);
};
"#;

/// Divergences where the Cranelift backend follows the spec and the rustc
/// backend does not. Reported, but they do not fail the run.
const KNOWN_DIVERGENCES: &[(&str, &str)] = &[(
    "corpus/tdz",
    "rustc backend reads an uninitialized function-local `let` without the TDZ ReferenceError",
)];

/// (name, source, bench) — sources report their result by throwing a string.
const CORPUS: &[(&str, &str, bool)] = &[
    (
        "arith",
        "throw 'R:' + (1 + 2 * 3 - 4 / 8 % 3) + ',' + (2 ** 10) + ',' + (-7 % 3);",
        false,
    ),
    (
        "bitwise",
        "var a = 0x7fffffff; throw 'R:' + (a | 1) + ',' + (a & 0xff) + ',' + (a ^ -1) + ',' + (a << 3) + ',' + (-16 >> 2) + ',' + (-16 >>> 28) + ',' + ~a;",
        false,
    ),
    (
        "compare",
        "throw 'R:' + (1 < 2) + (2 <= 2) + (3 > 4) + (NaN === NaN) + (0 === -0) + ('a' < 'b') + (null == undefined) + (1 == '1') + (1 === '1');",
        false,
    ),
    (
        "strings",
        "var s = 'ab'; s = s + 1 + 2; s += 'cd'; throw 'R:' + s + typeof s + s.length + s.toUpperCase();",
        false,
    ),
    (
        "typeof",
        "throw 'R:' + typeof 1 + typeof 'x' + typeof undefined + typeof null + typeof {} + typeof function(){} + typeof true;",
        false,
    ),
    (
        "loop-let",
        "let t = 0; for (let i = 0; i < 100; i++) { if (i % 3 === 0) continue; if (i > 90) break; t += i; } throw 'R:' + t;",
        false,
    ),
    (
        "while",
        "var i = 0, t = 1; while (i < 20) { t = (t * 3) % 1000003; i++; } do { t--; } while (t % 7 !== 0); throw 'R:' + t;",
        false,
    ),
    (
        "switch",
        "function f(x) { switch (x) { case 1: return 'one'; case 'a': return 'A'; default: return 'other'; case 2: return 'two'; } } throw 'R:' + f(1) + f(2) + f('a') + f(3);",
        false,
    ),
    (
        "closures",
        "function counter() { var n = 0; return function () { n += 1; return n; }; } var c = counter(); c(); c(); var d = counter(); throw 'R:' + c() + ',' + d();",
        false,
    ),
    (
        "let-per-iteration",
        "var fs = []; for (let i = 0; i < 3; i++) { fs.push(function () { return i; }); } throw 'R:' + fs[0]() + fs[1]() + fs[2]();",
        false,
    ),
    (
        "recursion",
        "function fact(n) { return n <= 1 ? 1 : n * fact(n - 1); } throw 'R:' + fact(10) + ',' + fact(25);",
        false,
    ),
    (
        "objects",
        "var o = { a: 1, b: { c: 'x' } }; o.d = o.a + 1; o['e'] = o.b.c + o.d; delete o.a; var ks = []; for (var k in o) ks.push(k); throw 'R:' + ks.join('|') + ',' + ('a' in o) + ',' + o.e;",
        false,
    ),
    (
        "arrays",
        "var a = [3, 1, 2]; a.push(5); a[10] = 1; var b = a.slice(0, 3).map(function (x) { return x * 2; }); throw 'R:' + a.length + ',' + b.join('-') + ',' + a.indexOf(2);",
        false,
    ),
    (
        "spread-rest",
        "function f(a, ...rest) { return a + rest.length; } var xs = [1, 2, 3]; var o = { ...{ p: 1 }, q: 2 }; var { p, ...others } = o; var [h, ...t] = xs; throw 'R:' + f(...xs) + ',' + p + ',' + others.q + ',' + h + t.length;",
        false,
    ),
    (
        "try-catch",
        "function f(x) { try { if (x) throw new Error('boom'); return 'ok'; } catch (e) { return 'caught:' + e.message; } } throw 'R:' + f(0) + ',' + f(1);",
        false,
    ),
    (
        "try-finally",
        "var log = ''; function f() { try { log += 'a'; return 'r'; } finally { log += 'f'; } } function g() { try { throw 'x'; } catch (e) { log += e; } finally { log += 'F'; } } var r = f(); g(); throw 'R:' + r + log;",
        false,
    ),
    ("uncaught-type-error", "var o = null; o.x;", false),
    (
        "tdz",
        "function f() { try { x; } catch (e) { return 'tdz'; } let x = 1; return 'no'; } throw 'R:' + f();",
        false,
    ),
    (
        "this-call-apply",
        "var o = { v: 4, m: function (a) { return this.v + a; } }; var m = o.m; throw 'R:' + o.m(1) + ',' + m.call({ v: 10 }, 2) + ',' + m.apply({ v: 20 }, [3]);",
        false,
    ),
    (
        "constructor",
        "function P(x) { this.x = x; } P.prototype.get = function () { return this.x * 2; }; var p = new P(21); throw 'R:' + p.get() + ',' + (p instanceof P);",
        false,
    ),
    (
        "class",
        "class A { constructor(v) { this.v = v; } get double() { return this.v * 2; } static make() { return new A(5); } } class B extends A { constructor() { super(7); } } throw 'R:' + A.make().double + ',' + new B().v;",
        false,
    ),
    (
        "arguments",
        "function f() { return arguments.length + ':' + arguments[1]; } throw 'R:' + f(1, 'two', 3);",
        false,
    ),
    (
        "number-format",
        "throw 'R:' + (0.1 + 0.2) + ',' + (1 / 0) + ',' + (-1 / 0) + ',' + (0 / 0) + ',' + 1e21 + ',' + (123.456).toFixed(1);",
        false,
    ),
    // Benchmarks (runtime comparison).
    (
        "bench-fib",
        "function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); } throw 'R:' + fib(25);",
        true,
    ),
    (
        "bench-loop",
        "function sum(n) { var t = 0; for (var i = 0; i < n; i++) { t = (t + i * 3) % 1000003; } return t; } throw 'R:' + sum(5000000);",
        true,
    ),
    (
        "bench-sieve",
        "function sieve(n) { var flags = []; for (var i = 0; i <= n; i++) flags.push(true); var count = 0; for (var p = 2; p <= n; p++) { if (flags[p]) { count++; for (var m = p * p; m <= n; m += p) flags[m] = false; } } return count; } throw 'R:' + sieve(200000);",
        true,
    ),
    (
        "bench-objects",
        "function run(n) { var t = 0; for (var i = 0; i < n; i++) { var o = { x: i, y: i * 2 }; t += o.x + o.y; } return t; } throw 'R:' + run(300000);",
        true,
    ),
    (
        "bench-strings",
        "function run(n) { var s = ''; for (var i = 0; i < n; i++) { s += String.fromCharCode(97 + i % 26); } return s.length; } throw 'R:' + run(100000);",
        true,
    ),
];

/// Host globals provided by `w3cos-core` alone (same expressions the ESM
/// codegen uses). Both backends receive them as identical binding captures;
/// cases needing any other global are skipped.
const CORE_GLOBALS: &[(&str, &str)] = &[
    ("undefined", "w3cos_core::Value::Undefined"),
    ("NaN", "w3cos_core::Value::Number(f64::NAN)"),
    ("Infinity", "w3cos_core::Value::Number(f64::INFINITY)"),
    ("Error", "w3cos_core::error_class(\"Error\")"),
    ("EvalError", "w3cos_core::error_class(\"EvalError\")"),
    ("RangeError", "w3cos_core::error_class(\"RangeError\")"),
    (
        "ReferenceError",
        "w3cos_core::error_class(\"ReferenceError\")",
    ),
    ("SyntaxError", "w3cos_core::error_class(\"SyntaxError\")"),
    ("TypeError", "w3cos_core::error_class(\"TypeError\")"),
    ("URIError", "w3cos_core::error_class(\"URIError\")"),
    ("Math", "w3cos_core::math_value()"),
    ("Map", "w3cos_core::collections::map_class()"),
    ("RegExp", "w3cos_core::regexp::regexp_class()"),
    ("BigInt", "w3cos_core::bigint::bigint_class()"),
    ("Array", "w3cos_core::array_value()"),
    ("Object", "w3cos_core::object_value()"),
];

fn core_global(name: &str) -> Value {
    match name {
        "undefined" => Value::Undefined,
        "NaN" => Value::Number(f64::NAN),
        "Infinity" => Value::Number(f64::INFINITY),
        "Math" => w3cos_core::math_value(),
        "Map" => w3cos_core::collections::map_class(),
        "RegExp" => w3cos_core::regexp::regexp_class(),
        "BigInt" => w3cos_core::bigint::bigint_class(),
        "Array" => w3cos_core::array_value(),
        "Object" => w3cos_core::object_value(),
        error => w3cos_core::error_class(error),
    }
}

fn global_captures(globals: &[(u32, String)]) -> w3cos_core::AotCaptureMap {
    globals
        .iter()
        .map(|(binding, name)| {
            (
                *binding,
                w3cos_core::AotCapture::Cell(std::rc::Rc::new(std::cell::RefCell::new((
                    core_global(name),
                    true,
                )))),
            )
        })
        .collect()
}

struct Case {
    globals: Vec<(u32, String)>,
    name: String,
    source: String,
    bench: bool,
    module: w3cos_ir::Module,
    rust: String,
}

#[derive(Default)]
struct Outcomes {
    cranelift: Option<String>,
    cranelift_compile: Duration,
    cranelift_run: Duration,
    rustc: Option<String>,
    rustc_run: Duration,
}

fn describe(completion: &Result<Value, Value>) -> String {
    match completion {
        Ok(_) => "OK".into(),
        Err(e) if e.is_object() || e.is_function() => {
            format!(
                "THROW {}: {}",
                e.get_property("name").to_js_string(),
                e.get_property("message").to_js_string()
            )
        }
        Err(e) => format!("THROW {}", e.to_js_string()),
    }
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn test262_cases(
    root: &Path,
    prefixes: &[String],
    limit: usize,
    skipped: &mut BTreeMap<String, usize>,
) -> Vec<(String, String)> {
    let mut files = Vec::new();
    for prefix in prefixes {
        collect(&root.join(prefix), &mut files);
    }
    files.sort();
    let mut cases = Vec::new();
    for file in files {
        if cases.len() >= limit {
            break;
        }
        let Ok(source) = std::fs::read_to_string(&file) else {
            continue;
        };
        let meta = source
            .split("/*---")
            .nth(1)
            .and_then(|m| m.split("---*/").next())
            .unwrap_or("");
        let reason = if meta.contains("negative:") {
            Some("negative")
        } else if meta.contains("includes:") && !meta.contains("includes: []") {
            Some("includes")
        } else if [
            "onlyStrict",
            "noStrict",
            "module",
            "async",
            "raw",
            "CanBlock",
        ]
        .iter()
        .any(|f| meta.contains(f))
        {
            Some("flags")
        } else if source.len() > 16 * 1024 {
            Some("oversized")
        } else {
            None
        };
        if let Some(reason) = reason {
            *skipped.entry(format!("test262 {reason}")).or_default() += 1;
            continue;
        }
        let rel = file
            .strip_prefix(root)
            .unwrap_or(&file)
            .display()
            .to_string();
        cases.push((rel, format!("{PRELUDE}\n{source}")));
    }
    cases
}

fn collect(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_file() {
        if path.extension().is_some_and(|e| e == "js")
            && !path.to_string_lossy().contains("_FIXTURE")
        {
            out.push(path.to_path_buf());
        }
        return;
    }
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            collect(&entry.path(), out);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let work =
        PathBuf::from(arg(&args, "--work").unwrap_or_else(|| "target/cranelift-diff".into()));
    let limit: usize = arg(&args, "--limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(400);
    let prefixes: Vec<String> = args
        .iter()
        .enumerate()
        .filter(|(i, _)| *i > 0 && args[*i - 1] == "--prefix")
        .map(|(_, v)| v.clone())
        .collect();
    let mut skipped: BTreeMap<String, usize> = BTreeMap::new();
    let mut missing_globals: BTreeMap<String, usize> = BTreeMap::new();

    let mut sources: Vec<(String, String, bool)> = CORPUS
        .iter()
        .map(|(n, s, b)| (format!("corpus/{n}"), s.to_string(), *b))
        .collect();
    if let Some(root) = arg(&args, "--test262") {
        for (name, source) in test262_cases(Path::new(&root), &prefixes, limit, &mut skipped) {
            sources.push((name, source, false));
        }
    }

    // Lower + filter: both backends must accept the case.
    let mut cases = Vec::new();
    let mut op_coverage: BTreeMap<&'static str, usize> = BTreeMap::new();
    for (name, source, bench) in sources {
        let module = match w3cos_compiler::w3ir_lowering::lower_script(&source, "app:///case.js") {
            Ok(module) => module,
            Err(_) => {
                *skipped.entry("lowering error".into()).or_default() += 1;
                continue;
            }
        };
        let globals: Vec<(u32, String)> = module
            .imports
            .iter()
            .filter(|i| i.specifier == "w3cos:global")
            .map(|i| (i.local.0, i.imported.clone()))
            .collect();
        if let Some((_, name)) = globals
            .iter()
            .find(|(_, name)| !CORE_GLOBALS.iter().any(|(g, _)| g == name))
        {
            *skipped.entry("needs host globals".into()).or_default() += 1;
            *missing_globals.entry(name.clone()).or_default() += 1;
            continue;
        }
        if let Err(e) = w3cos_cranelift::check_module(&module) {
            *skipped.entry(format!("cranelift {}", e.0)).or_default() += 1;
            continue;
        }
        let entry = module
            .functions
            .iter()
            .find(|f| f.id == module.entry)
            .expect("entry");
        let rust_name = format!("case_{}", cases.len());
        let rust = match w3cos_compiler::w3ir_aot::generate_sync_function_from_module(
            &module, entry, &rust_name,
        ) {
            Ok(rust) => rust,
            Err(_) => {
                *skipped.entry("rustc backend codegen".into()).or_default() += 1;
                continue;
            }
        };
        for function in &module.functions {
            for block in &function.blocks {
                for instruction in &block.instructions {
                    *op_coverage
                        .entry(w3cos_cranelift::op_name(instruction))
                        .or_default() += 1;
                }
            }
        }
        cases.push(Case {
            globals,
            name,
            source,
            bench,
            module,
            rust,
        });
    }
    eprintln!(
        "cranelift_diff: {} cases ({} skipped)",
        cases.len(),
        skipped.values().sum::<usize>()
    );

    // Cranelift: compile + run in process.
    let mut outcomes: Vec<Outcomes> = (0..cases.len()).map(|_| Outcomes::default()).collect();
    let mut cranelift_total_compile = Duration::ZERO;
    let mut native_numeric = 0;
    let mut instructions = 0;
    for (case, outcome) in cases.iter().zip(outcomes.iter_mut()) {
        if std::env::var_os("W3COS_DIFF_TRACE").is_some() {
            eprintln!("cranelift: {}", case.name);
        }
        let started = Instant::now();
        let program = match w3cos_cranelift::compile_module(&case.module) {
            Ok(p) => p,
            Err(e) => {
                outcome.cranelift = Some(format!("COMPILE {e}"));
                continue;
            }
        };
        outcome.cranelift_compile = started.elapsed();
        cranelift_total_compile += outcome.cranelift_compile;
        native_numeric += program.stats.native_numeric;
        instructions += program.stats.instructions;
        let started = Instant::now();
        let completion = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            program.run_entry_with(&global_captures(&case.globals))
        }));
        outcome.cranelift_run = started.elapsed();
        outcome.cranelift = Some(match completion {
            Ok(c) => describe(&c),
            Err(_) => "PANIC".into(),
        });
    }

    // rustc: one batch fixture crate.
    let crate_dir = work.join("fixture");
    std::fs::create_dir_all(crate_dir.join("src")).unwrap();
    let core = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../w3cos-core")
        .canonicalize()
        .unwrap();
    std::fs::write(
        crate_dir.join("Cargo.toml"),
        format!(
            "[package]\nname = \"cranelift_diff_fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[dependencies]\nw3cos-core = {{ path = {:?} }}\n\n[workspace]\n",
            core
        ),
    )
    .unwrap();
    let lock = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock");
    if !crate_dir.join("Cargo.lock").exists() {
        let _ = std::fs::copy(&lock, crate_dir.join("Cargo.lock"));
    }
    let mut main_rs = String::from("#![allow(warnings)]\n");
    for (index, case) in cases.iter().enumerate() {
        main_rs.push_str(&format!("mod c{index} {{\n{}\n}}\n", case.rust));
    }
    main_rs.push_str(
        "fn describe(c: &Result<w3cos_core::Value, w3cos_core::Value>) -> String {\n    match c {\n        Ok(_) => \"OK\".into(),\n        Err(e) if e.is_object() || e.is_function() => format!(\"THROW {}: {}\", e.get_property(\"name\").to_js_string(), e.get_property(\"message\").to_js_string()),\n        Err(e) => format!(\"THROW {}\", e.to_js_string()),\n    }\n}\nfn main() {\n",
    );
    for (index, case) in cases.iter().enumerate() {
        main_rs.push_str("    { let mut caps = w3cos_core::AotCaptureMap::new();\n");
        for (binding, name) in &case.globals {
            let expr = CORE_GLOBALS.iter().find(|(g, _)| g == name).unwrap().1;
            main_rs.push_str(&format!(
                "      caps.insert({binding}, w3cos_core::AotCapture::Cell(std::rc::Rc::new(std::cell::RefCell::new(({expr}, true)))));\n"
            ));
        }
        main_rs.push_str(&format!(
            "    {{ let t = std::time::Instant::now(); let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| w3cos_core::catch_js(|| c{index}::case_{index}(w3cos_core::Value::Undefined, &[], &caps)))); let d = t.elapsed().as_nanos(); println!(\"{index}\\t{{}}\\t{{d}}\", match r {{ Ok(c) => describe(&c), Err(_) => \"PANIC\".into() }}); }} }}\n"
        ));
    }
    main_rs.push_str("}\n");
    // Build dependencies first so the timed build is the cases only.
    let target = work.join("target");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let build = |label: &str| {
        let started = Instant::now();
        let output = Command::new(&cargo)
            .args(["build", "--release", "--quiet", "--manifest-path"])
            .arg(crate_dir.join("Cargo.toml"))
            .env("CARGO_TARGET_DIR", &target)
            .output()
            .unwrap();
        if !output.status.success() {
            eprintln!(
                "rustc fixture build failed ({label}):\n{}",
                String::from_utf8_lossy(&output.stderr)
                    .chars()
                    .take(4000)
                    .collect::<String>()
            );
            std::process::exit(2);
        }
        started.elapsed()
    };
    std::fs::write(
        crate_dir.join("src/main.rs"),
        "fn main() { let _ = w3cos_core::Value::Undefined; }\n",
    )
    .unwrap();
    build("deps");
    std::fs::write(crate_dir.join("src/main.rs"), &main_rs).unwrap();
    let rustc_compile = build("cases");
    let output = Command::new(target.join("release/cranelift_diff_fixture"))
        .output()
        .unwrap();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut parts = line.splitn(3, '\t');
        let (Some(i), Some(outcome), Some(nanos)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let Ok(i) = i.parse::<usize>() else { continue };
        outcomes[i].rustc = Some(outcome.to_string());
        outcomes[i].rustc_run = Duration::from_nanos(nanos.parse().unwrap_or(0));
    }

    // Compare.
    let mut matched = 0;
    let mut mismatches = Vec::new();
    let mut known = Vec::new();
    for (case, outcome) in cases.iter().zip(&outcomes) {
        if outcome.cranelift == outcome.rustc && outcome.cranelift.is_some() {
            matched += 1;
        } else if let Some((_, why)) = KNOWN_DIVERGENCES.iter().find(|(n, _)| *n == case.name) {
            known.push(format!(
                "{}: cranelift={:?} rustc={:?} ({why})",
                case.name, outcome.cranelift, outcome.rustc
            ));
        } else {
            mismatches.push(format!(
                "{}: cranelift={:?} rustc={:?}",
                case.name, outcome.cranelift, outcome.rustc
            ));
        }
    }
    let t262: Vec<_> = cases
        .iter()
        .zip(&outcomes)
        .filter(|(c, _)| c.name.starts_with("test/"))
        .collect();
    let t262_pass = t262
        .iter()
        .filter(|(_, o)| o.cranelift.as_deref() == Some("OK"))
        .count();

    println!("# Cranelift backend B vs rustc backend\n");
    println!(
        "cases: {} (matched {matched}, known divergences {}, mismatched {})",
        cases.len(),
        known.len(),
        mismatches.len()
    );
    println!(
        "test262 cases: {} (OK under both backends: {t262_pass})",
        t262.len()
    );
    println!("skipped: {skipped:?}");
    let mut missing: Vec<_> = missing_globals.into_iter().collect();
    missing.sort_by_key(|a| std::cmp::Reverse(a.1));
    missing.truncate(12);
    println!("top missing host globals: {missing:?}");
    println!(
        "compile: cranelift {:.1} ms total ({:.2} ms/case) vs rustc batch {:.1} s ({:.1} ms/case, deps prebuilt)",
        cranelift_total_compile.as_secs_f64() * 1e3,
        cranelift_total_compile.as_secs_f64() * 1e3 / cases.len().max(1) as f64,
        rustc_compile.as_secs_f64(),
        rustc_compile.as_secs_f64() * 1e3 / cases.len().max(1) as f64,
    );
    println!("native f64 instructions: {native_numeric} / {instructions}");
    println!(
        "\n| bench | cranelift run (ms) | rustc run (ms) | ratio | outcome |\n|---|---|---|---|---|"
    );
    for (case, outcome) in cases.iter().zip(&outcomes).filter(|(c, _)| c.bench) {
        let (c, r) = (
            outcome.cranelift_run.as_secs_f64() * 1e3,
            outcome.rustc_run.as_secs_f64() * 1e3,
        );
        println!(
            "| {} | {c:.2} | {r:.2} | {:.2}x | {} |",
            case.name,
            c / r.max(1e-9),
            outcome.cranelift.clone().unwrap_or_default()
        );
    }
    println!("\nW3IR op coverage (instructions in accepted cases): {op_coverage:?}");
    for k in &known {
        println!("KNOWN-DIVERGENCE {k}");
    }
    for m in &mismatches {
        println!("MISMATCH {m}");
    }
    let _ = &cases.iter().map(|c| c.source.len()).sum::<usize>();
    if !mismatches.is_empty() {
        std::process::exit(1);
    }
}
