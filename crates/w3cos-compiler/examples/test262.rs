//! test262 conformance runner for the AOT pipeline (M1 item 14).
//!
//! Every test goes through the same path as an app: ESM front end
//! (resolver + bundle + W3IR lowering + Rust codegen), rustc, native run.
//!
//! ```text
//! cargo run --release -p w3cos-compiler --example test262 -- \
//!     --test262 <checkout> [--subset tests/test262/subset.txt | <path-prefix>...] \
//!     [--jobs 8] [--batch 150] [--timeout 10] [--report out/test262.json] \
//!     [--summary out/test262.md] [--expected tests/test262/expected-failures.txt] \
//!     [--update-expected]
//! ```
//!
//! Design: compiling one crate per test is far too slow (the harness alone
//! lowers to ~330 KB of Rust), so tests are batched. Each batch is one ESM
//! bundle: a harness module (assert.js, sta.js, a `$262`/`print` prelude
//! and the batch's `includes`) exporting its top-level names, plus one
//! module per test that imports them. The batch binary takes a test index
//! and runs only the harness init and that test's module init, so every test
//! still runs in its own process with a timeout.
//!
//! Tests are compiled as module code (strict), which is how w3cos compiles
//! everything; `noStrict`-only tests are reported as failures with reason
//! `unsupported: sloppy mode`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use swc_common::{FileName, SourceMap, sync::Lrc};
use swc_ecma_ast::{Decl, ModuleItem, Pat, Stmt};
use swc_ecma_parser::{EsSyntax, Parser, StringInput, Syntax};
use w3cos_compiler::esm_codegen::try_generate_with_bodies;
use w3cos_compiler::esm_resolver::{EsmBundle, EsmResolver};

/// Directories that make up the conformance score (intl402 and staging are
/// reported only when selected explicitly).
const DEFAULT_ROOTS: &[&str] = &["test/built-ins", "test/language", "test/annexB"];

const PRELUDE: &str = r#"
var __t262_global = globalThis;
function print(message) { __t262_global.__t262_print = String(message); }
var $262 = {
  global: __t262_global,
  gc: function () {},
  detachArrayBuffer: function (buffer) {
    if (typeof buffer.transfer === "function") { buffer.transfer(); return null; }
    throw new Test262Error("detachArrayBuffer is not supported");
  },
  evalScript: function () { throw new Test262Error("evalScript is not supported"); },
  createRealm: function () { throw new Test262Error("createRealm is not supported"); },
  agent: {},
};
"#;

#[derive(Clone, Debug, Default)]
struct Meta {
    includes: Vec<String>,
    flags: Vec<String>,
    features: Vec<String>,
    negative: Option<(String, String)>,
}

#[derive(Clone, Debug)]
struct TestCase {
    rel: String,
    path: PathBuf,
    meta: Meta,
    source: String,
}

impl TestCase {
    fn has_flag(&self, flag: &str) -> bool {
        self.meta.flags.iter().any(|f| f == flag)
    }
    fn is_async(&self) -> bool {
        self.has_flag("async")
    }
    fn harness_key(&self) -> String {
        if self.has_flag("raw") {
            return "raw".into();
        }
        let mut includes = self.meta.includes.clone();
        if self.is_async() {
            includes.push("doneprintHandle.js".into());
        }
        includes.sort();
        includes.dedup();
        includes.join(",")
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Outcome {
    Pass,
    Fail(String),
}

// ── frontmatter ─────────────────────────────────────────────────────────

fn parse_list(value: &str) -> Vec<String> {
    let value = value.trim();
    let inner = value
        .strip_prefix('[')
        .and_then(|v| v.strip_suffix(']'))
        .unwrap_or(value);
    inner
        .split(',')
        .map(|s| s.trim().trim_matches(|c| c == '\'' || c == '"').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Tests above this size are reported unsupported instead of compiled.
const MAX_TEST_SOURCE: usize = 64 * 1024;

fn parse_meta(source: &str) -> Meta {
    let mut meta = Meta::default();
    let Some(start) = source.find("/*---") else {
        return meta;
    };
    let Some(end) = source[start..].find("---*/") else {
        return meta;
    };
    let yaml = &source[start + 5..start + end];
    let mut current: Option<String> = None;
    let mut negative_phase = String::new();
    let mut negative_type = String::new();
    for line in yaml.lines() {
        let indented = line.starts_with(' ') || line.starts_with('\t');
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !indented {
            if let Some((key, value)) = trimmed.split_once(':') {
                let key = key.trim().to_string();
                let value = value.trim();
                match key.as_str() {
                    "includes" if !value.is_empty() => meta.includes = parse_list(value),
                    "flags" if !value.is_empty() => meta.flags = parse_list(value),
                    "features" if !value.is_empty() => meta.features = parse_list(value),
                    _ => {}
                }
                current = Some(key);
            } else {
                current = None;
            }
            continue;
        }
        match current.as_deref() {
            Some(list @ ("includes" | "flags" | "features")) => {
                if let Some(item) = trimmed.strip_prefix("- ") {
                    let item = item.trim().to_string();
                    match list {
                        "includes" => meta.includes.push(item),
                        "flags" => meta.flags.push(item),
                        _ => meta.features.push(item),
                    }
                }
            }
            Some("negative") => {
                if let Some(v) = trimmed.strip_prefix("phase:") {
                    negative_phase = v.trim().to_string();
                } else if let Some(v) = trimmed.strip_prefix("type:") {
                    negative_type = v.trim().to_string();
                }
            }
            _ => {}
        }
    }
    if !negative_phase.is_empty() {
        meta.negative = Some((negative_phase, negative_type));
    }
    meta
}

// ── discovery ───────────────────────────────────────────────────────────

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else if path.extension().is_some_and(|e| e == "js")
            && !path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.contains("_FIXTURE"))
        {
            out.push(path);
        }
    }
}

fn read_list_file(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| l.split('#').next().unwrap_or("").trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

// ── harness ─────────────────────────────────────────────────────────────

fn top_level_names(source: &str) -> Vec<String> {
    let cm: Lrc<SourceMap> = Default::default();
    let fm = cm.new_source_file(Lrc::new(FileName::Anon), source.to_string());
    let mut parser = Parser::new(
        Syntax::Es(EsSyntax::default()),
        StringInput::from(&*fm),
        None,
    );
    let Ok(script) = parser.parse_script() else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for stmt in script.body {
        if let Stmt::Decl(decl) = stmt {
            match decl {
                Decl::Fn(f) => names.push(f.ident.sym.to_string()),
                Decl::Class(c) => names.push(c.ident.sym.to_string()),
                Decl::Var(v) => {
                    for d in v.decls {
                        if let Pat::Ident(id) = d.name {
                            names.push(id.id.sym.to_string());
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let mut seen = BTreeSet::new();
    names.retain(|n| seen.insert(n.clone()));
    names
}

/// Harness module source for an include set, plus its exported names.
fn harness_module(test262: &Path, key: &str) -> (String, Vec<String>) {
    let mut source = String::new();
    for file in ["assert.js", "sta.js"] {
        source.push_str(&std::fs::read_to_string(test262.join("harness").join(file)).unwrap());
        source.push('\n');
    }
    source.push_str(PRELUDE);
    for include in key.split(',').filter(|s| !s.is_empty()) {
        if let Ok(text) = std::fs::read_to_string(test262.join("harness").join(include)) {
            source.push_str(&text);
            source.push('\n');
        }
    }
    let names = top_level_names(&source);
    source.push_str(&format!("\nexport {{ {} }};\n", names.join(", ")));
    (source, names)
}

fn relative_import(from_dir: &Path, to: &Path) -> String {
    let from: Vec<_> = from_dir.components().collect();
    let to_c: Vec<_> = to.components().collect();
    let common = from.iter().zip(&to_c).take_while(|(a, b)| a == b).count();
    let mut parts = vec!["..".to_string(); from.len() - common];
    for c in &to_c[common..] {
        parts.push(c.as_os_str().to_string_lossy().into_owned());
    }
    let joined = parts.join("/");
    if joined.starts_with("..") {
        joined
    } else {
        format!("./{joined}")
    }
}

// ── front end ───────────────────────────────────────────────────────────

struct Workspace {
    test262: PathBuf,
    work: PathBuf,
    harness: Mutex<HashMap<String, (PathBuf, Vec<String>)>>,
}

impl Workspace {
    fn harness_for(&self, key: &str) -> (PathBuf, Vec<String>) {
        let mut map = self.harness.lock().unwrap();
        if let Some(found) = map.get(key) {
            return found.clone();
        }
        let index = map.len();
        let (source, names) = harness_module(&self.test262, key);
        let path = self.work.join(format!("tree/__harness/h{index}.js"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, source).unwrap();
        map.insert(key.to_string(), (path.clone(), names.clone()));
        (path, names)
    }

    /// Mirror location of a test (fixtures are copied next to it).
    fn mirror(&self, test: &TestCase) -> PathBuf {
        let path = self.work.join("tree").join(&test.rel);
        let dir = path.parent().unwrap();
        if !dir.exists() {
            std::fs::create_dir_all(dir).unwrap();
            if let Ok(entries) = std::fs::read_dir(test.path.parent().unwrap()) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    if name.to_string_lossy().contains("_FIXTURE") {
                        let _ = std::fs::copy(entry.path(), dir.join(name));
                    }
                }
            }
        }
        path
    }

    fn write_test_module(&self, test: &TestCase) -> PathBuf {
        let path = self.mirror(test);
        let mut text = String::new();
        if !test.has_flag("raw") {
            let (harness, names) = self.harness_for(&test.harness_key());
            let spec = relative_import(path.parent().unwrap(), &harness);
            // Tests that redeclare a harness name keep their own binding.
            let names: Vec<_> = names
                .into_iter()
                .filter(|n| !declares(&test.source, n))
                .collect();
            text.push_str(&format!(
                "import {{ {} }} from {spec:?};\n",
                names.join(", ")
            ));
        }
        text.push_str(&test.source);
        // Completion marker: a sync test passes only if its body ran to the
        // end (guards against a test silently not being executed).
        text.push_str("\n;globalThis.__t262_ran = true;\n");
        std::fs::write(&path, text).unwrap();
        path
    }
}

fn declares(source: &str, name: &str) -> bool {
    ["function ", "var ", "let ", "const ", "class "]
        .iter()
        .any(|kw| {
            source.contains(&format!("{kw}{name} "))
                || source.contains(&format!("{kw}{name}("))
                || source.contains(&format!("{kw}{name};"))
                || source.contains(&format!("{kw}{name}="))
        })
}

fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_else(|| "panic".into())
}

/// Front end for one entry; `Err` carries the stage and message.
fn front_end(root: &Path, entry: &Path) -> Result<String, String> {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let resolver = EsmResolver::new(root);
        let parsed = resolver
            .parse_graph_from_entry(entry)
            .map_err(|e| format!("parse: {}", first_line(&format!("{e:#}"))))?;
        let bundle = EsmBundle::build(&parsed, &resolver, entry);
        if !bundle.unresolved.is_empty() {
            return Err(format!(
                "link: {} unresolved binding(s)",
                bundle.unresolved.len()
            ));
        }
        try_generate_with_bodies(&bundle)
            .map_err(|e| format!("codegen: {}", first_line(&e.to_string())))
    }));
    match result {
        Ok(r) => r,
        Err(payload) => Err(format!(
            "frontend panic: {}",
            first_line(&panic_text(&*payload))
        )),
    }
}

/// Front end in a child process (`--frontend-one`), so a compiler hang or
/// stack overflow on one test cannot stall the run.
fn front_end_isolated(root: &Path, entry: &Path, timeout: Duration) -> Result<String, String> {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.arg("--frontend-one").arg(root).arg(entry);
    match run_with_timeout(command, timeout) {
        Ok(output) => match output.lines().last() {
            Some("FE OK") => Ok(String::new()),
            Some(line) if line.starts_with("FE ERR ") => Err(line[7..].to_string()),
            _ => Err("frontend crash".into()),
        },
        Err(error) if error == "timeout" => Err("frontend: timeout".into()),
        Err(error) => Err(format!("frontend: {error}")),
    }
}

fn first_line(s: &str) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    line.chars().take(200).collect()
}

// ── batch crates ────────────────────────────────────────────────────────

struct Batch {
    tests: Vec<usize>,
}

struct ModuleMap {
    /// Module path (relative to the tree root) -> `mN`.
    by_path: HashMap<String, String>,
    /// Topological init order.
    order: Vec<String>,
    /// Line ranges of each module in the generated code.
    lines: Vec<(usize, String)>,
    register: String,
}

fn module_map(code: &str, tree: &Path) -> ModuleMap {
    let tree_prefix = format!("{}/", tree.display());
    let mut by_path = HashMap::new();
    let mut lines = Vec::new();
    let mut pending: Option<String> = None;
    let mut order = Vec::new();
    let mut register = String::new();
    for (index, line) in code.lines().enumerate() {
        if let Some(path) = line.strip_prefix("/// ESM module: ") {
            let path = path.trim();
            pending = Some(path.strip_prefix(&tree_prefix).unwrap_or(path).to_string());
        } else if let Some(rest) = line.strip_prefix("mod m") {
            if let Some(path) = pending.take() {
                let id: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                let name = format!("m{id}");
                by_path.insert(path.clone(), name.clone());
                lines.push((index + 1, path));
            }
        } else if line.starts_with("fn __w3cos_run_entry_body()") {
            for token in line.split(';') {
                let token = token.trim();
                if let Some(call) = token.strip_suffix("__init()") {
                    if let Some((module, _)) = call.rsplit_once("::") {
                        let module =
                            module.trim_start_matches(|c: char| c == '{' || c.is_whitespace());
                        let module = module.rsplit(' ').next().unwrap_or(module);
                        order.push(module.to_string());
                    }
                }
            }
        } else if let Some(body) = line.strip_prefix("pub fn run_entry() -> w3cos_core::Value {") {
            register = body
                .trim_end()
                .trim_end_matches('}')
                .replace("__w3cos_run_entry_body()", "");
        }
    }
    ModuleMap {
        by_path,
        order,
        lines,
        register,
    }
}

const MAIN_TAIL: &str = r#"
fn __t262_report(test: usize) {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        __t262_register();
        __t262_run(test);
        for _ in 0..64 {
            if w3cos_core::promise::drain_microtasks() == 0 { break; }
        }
    }));
    let printed = w3cos_core::intrinsics::get_property(
        &w3cos_runtime::jsdom::window_value(),
        &w3cos_core::Value::string("__t262_print"),
    );
    match outcome {
        Ok(()) => {
            let ran = w3cos_core::intrinsics::get_property(
                &w3cos_runtime::jsdom::window_value(),
                &w3cos_core::Value::string("__t262_ran"),
            );
            if !printed.is_undefined() {
                println!("T262 PRINT {}", printed.to_js_string());
            } else if ran.to_bool() {
                println!("T262 OK");
            } else {
                println!("T262 NORUN test body did not complete");
            }
        }
        Err(payload) => {
            if let Some(value) = payload.downcast_ref::<w3cos_core::PanicValue>() {
                let error = &value.0;
                let ctor = error.get_property("constructor").get_property("name").to_js_string();
                let message = error.get_property("message");
                let text = if message.is_undefined() { error.to_js_string() } else { message.to_js_string() };
                println!("T262 THROW {ctor} {}", text.replace('\n', " "));
            } else {
                let text = payload.downcast_ref::<String>().cloned()
                    .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_default();
                println!("T262 PANIC {}", text.replace('\n', " "));
            }
        }
    }
}

fn main() {
    std::panic::set_hook(Box::new(|_| {}));
    let test: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(0);
    __t262_report(test);
}
"#;

fn crate_source(code: &str, map: &ModuleMap, tests: &[(usize, Vec<String>)]) -> String {
    let mut run = String::from("fn __t262_run(test: usize) {\n    match test {\n");
    for (slot, modules) in tests {
        let wanted: BTreeSet<&String> = modules.iter().collect();
        let calls: String = map
            .order
            .iter()
            .filter(|m| wanted.contains(m))
            .map(|m| format!("{m}::{m}__init(); "))
            .collect();
        run.push_str(&format!("        {slot} => {{ {calls}}}\n"));
    }
    run.push_str("        _ => {}\n    }\n}\n");
    format!(
        "{code}\nfn __t262_register() {{ {} }}\n{run}{MAIN_TAIL}",
        map.register.trim().trim_end_matches(';')
    )
}

fn manifest(workspace: &Path) -> String {
    let core = workspace.join("crates/w3cos-core");
    let runtime = workspace.join("crates/w3cos-runtime");
    format!(
        "[package]\nname = \"t262-batch\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\nw3cos-core = {{ path = {core:?} }}\nw3cos-runtime = {{ path = {runtime:?}, default-features = false, features = [\"web-graphics-advanced\", \"web-media-advanced\"] }}\n\n[profile.dev]\nopt-level = 0\ndebug = 0\nincremental = false\ncodegen-units = 64\n\n[profile.dev.package.\"*\"]\nopt-level = 1\n\n[workspace]\n"
    )
}

/// Upper bound for one batch's `cargo build`.
const BUILD_TIMEOUT: Duration = Duration::from_secs(20 * 60);

/// `Command::output` with a deadline; `None` when the child was killed.
fn output_with_timeout(mut command: Command, timeout: Duration) -> Option<std::process::Output> {
    use std::process::Stdio;
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("cargo");
    // Drain pipes on threads so a chatty child cannot block on a full pipe.
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stdout, &mut buf);
        buf
    });
    let err = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stderr, &mut buf);
        buf
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(_) => return None,
        }
    };
    Some(std::process::Output {
        status,
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
    })
}

fn run_with_timeout(mut command: Command, timeout: Duration) -> Result<String, String> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn: {e}"))?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("timeout".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(e) => return Err(format!("wait: {e}")),
        }
    }
    let mut out = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_string(&mut out);
    }
    Ok(out)
}

fn judge(test: &TestCase, output: &str) -> Outcome {
    let line = output
        .lines()
        .rev()
        .find(|l| l.starts_with("T262 "))
        .unwrap_or("T262 CRASH");
    let negative_runtime = test
        .meta
        .negative
        .as_ref()
        .filter(|(phase, _)| phase == "runtime")
        .map(|(_, ty)| ty.as_str());
    if let Some(expected) = negative_runtime {
        return match line.strip_prefix("T262 THROW ") {
            Some(rest) if rest.split(' ').next() == Some(expected) => Outcome::Pass,
            Some(rest) => Outcome::Fail(format!(
                "runtime: expected {expected}, got {}",
                first_line(rest)
            )),
            None => Outcome::Fail(format!("runtime: expected {expected} to be thrown")),
        };
    }
    if test.is_async() {
        return match line.strip_prefix("T262 PRINT ") {
            Some("Test262:AsyncTestComplete") => Outcome::Pass,
            Some(other) => Outcome::Fail(format!("async: {}", first_line(other))),
            None if line.starts_with("T262 OK") => {
                Outcome::Fail("async: $DONE was not called".into())
            }
            None => Outcome::Fail(format!(
                "runtime: {}",
                first_line(line.trim_start_matches("T262 "))
            )),
        };
    }
    if line == "T262 OK" {
        Outcome::Pass
    } else {
        Outcome::Fail(format!(
            "runtime: {}",
            first_line(line.trim_start_matches("T262 "))
        ))
    }
}

/// Build a batch crate; returns the binary or marks tests whose generated
/// code rustc rejected and retries without them.
fn build_batch(
    ws: &Workspace,
    workspace: &Path,
    slot: usize,
    batch_index: usize,
    tests: &[TestCase],
    members: Vec<usize>,
    results: &Mutex<BTreeMap<usize, Outcome>>,
) -> Option<(PathBuf, Vec<usize>)> {
    let mut members = members;
    for _attempt in 0..4 {
        if members.is_empty() {
            return None;
        }
        let dir = ws.work.join(format!("batches/b{batch_index}"));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let tree = ws.work.join("tree");
        let mut entry = String::new();
        let mut rel_paths = Vec::new();
        for &index in &members {
            let path = ws.mirror(&tests[index]);
            let rel = path
                .strip_prefix(&tree)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            entry.push_str(&format!("import {:?};\n", format!("./{rel}")));
            rel_paths.push(rel);
        }
        let entry_path = tree.join(format!("__batch_{batch_index}.js"));
        std::fs::write(&entry_path, entry).unwrap();
        let code_path = dir.join("bundle.rs");
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .arg("--frontend-one")
            .arg(&tree)
            .arg(&entry_path)
            .arg(&code_path);
        let fe = match run_with_timeout(command, Duration::from_secs(600)) {
            Ok(out) if out.lines().last() == Some("FE OK") => {
                Ok(std::fs::read_to_string(&code_path).unwrap())
            }
            Ok(out) => Err(out.lines().last().unwrap_or("frontend crash").to_string()),
            Err(error) => Err(error),
        };
        let code = match fe {
            Ok(code) => code,
            Err(error) => {
                // Should not happen (each test passed the front end alone);
                // fall back to failing the whole batch.
                let mut r = results.lock().unwrap();
                for &index in &members {
                    r.insert(index, Outcome::Fail(format!("batch {error}")));
                }
                return None;
            }
        };
        let map = module_map(&code, &tree);
        let mut slots = Vec::new();
        for (slot_index, rel) in rel_paths.iter().enumerate() {
            let own_dir = Path::new(rel)
                .parent()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let mut modules: Vec<String> = map
                .by_path
                .iter()
                .filter(|(path, _)| {
                    *path == rel
                        || path.starts_with("__harness/")
                        || (path.contains("_FIXTURE")
                            && Path::new(path)
                                .parent()
                                .map(|p| p.to_string_lossy().into_owned())
                                == Some(own_dir.clone()))
                })
                .map(|(_, m)| m.clone())
                .collect();
            // Only this test's harness module, not every harness in the batch.
            let harness = harness_module_name(ws, &tests[members[slot_index]], &map);
            modules.retain(|m| !is_harness(&map, m) || Some(m) == harness.as_ref());
            if !map.by_path.contains_key(rel) {
                results.lock().unwrap().insert(
                    members[slot_index],
                    Outcome::Fail("runner: test module missing from batch".into()),
                );
                modules.clear();
            }
            slots.push((slot_index, modules));
        }
        std::fs::write(dir.join("src/main.rs"), crate_source(&code, &map, &slots)).unwrap();
        std::fs::write(dir.join("Cargo.toml"), manifest(workspace)).unwrap();
        // Pin the batch crate to the workspace's locked versions so `--offline`
        // resolves against what `cargo fetch` already downloaded.
        let lock = dir.join("Cargo.lock");
        if !lock.exists() {
            let _ = std::fs::copy(workspace.join("Cargo.lock"), &lock);
        }
        let target = ws.work.join(format!("target/slot{slot}"));
        let mut build = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
        build
            .args(["build", "--offline", "--quiet", "--message-format=short"])
            .current_dir(&dir)
            .env("CARGO_TARGET_DIR", &target)
            .env_remove("RUSTFLAGS");
        let Some(output) = output_with_timeout(build, BUILD_TIMEOUT) else {
            let mut r = results.lock().unwrap();
            for &index in &members {
                r.entry(index)
                    .or_insert_with(|| Outcome::Fail("build: timeout".into()));
            }
            return None;
        };
        if output.status.success() {
            let bin = dir.join("t262-batch");
            let _ = std::fs::copy(target.join("debug/t262-batch"), &bin);
            // Generated sources run to ~100 MB per batch; a full run would
            // otherwise fill a CI runner's disk. W3COS_T262_KEEP=1 keeps them.
            if std::env::var_os("W3COS_T262_KEEP").is_none() {
                let _ = std::fs::remove_file(&code_path);
                let _ = std::fs::remove_file(dir.join("src/main.rs"));
            }
            return Some((bin, members));
        }
        // Map rustc errors back to modules (tests) by line.
        let stderr = String::from_utf8_lossy(&output.stderr);
        let mut bad = BTreeSet::new();
        let mut first_error = String::new();
        for line in stderr.lines() {
            if let Some(rest) = line.strip_prefix("src/main.rs:") {
                if !line.contains("error") {
                    continue;
                }
                if first_error.is_empty() {
                    first_error = first_line(line);
                }
                let number: usize = rest
                    .split(':')
                    .next()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0);
                let owner = map
                    .lines
                    .iter()
                    .rev()
                    .find(|(start, _)| *start <= number)
                    .map(|(_, p)| p.clone());
                if let Some(owner) = owner {
                    if let Some(pos) = rel_paths.iter().position(|p| *p == owner) {
                        bad.insert(members[pos]);
                    } else if owner.contains("_FIXTURE") {
                        let dir = Path::new(&owner)
                            .parent()
                            .unwrap()
                            .to_string_lossy()
                            .into_owned();
                        for (pos, p) in rel_paths.iter().enumerate() {
                            if p.starts_with(&dir) {
                                bad.insert(members[pos]);
                            }
                        }
                    }
                }
            }
        }
        let mut r = results.lock().unwrap();
        if bad.is_empty() {
            if first_error.is_empty() {
                // Not a per-test rustc error (dependency resolution, linker,
                // ...): surface cargo's own first error line.
                first_error = stderr
                    .lines()
                    .find(|line| line.contains("error"))
                    .or_else(|| stderr.lines().next())
                    .map(first_line)
                    .unwrap_or_default();
                eprintln!("test262: batch {batch_index} build failed:\n{stderr}");
            }
            for &index in &members {
                r.insert(
                    index,
                    Outcome::Fail(format!("rustc (batch): {first_error}")),
                );
            }
            return None;
        }
        for &index in &bad {
            r.insert(index, Outcome::Fail(format!("rustc: {first_error}")));
        }
        members.retain(|m| !bad.contains(m));
    }
    None
}

fn is_harness(map: &ModuleMap, module: &str) -> bool {
    map.by_path
        .iter()
        .any(|(path, m)| m == module && path.starts_with("__harness/"))
}

fn harness_module_name(ws: &Workspace, test: &TestCase, map: &ModuleMap) -> Option<String> {
    if test.has_flag("raw") {
        return None;
    }
    let (path, _) = ws.harness_for(&test.harness_key());
    let rel = path
        .strip_prefix(ws.work.join("tree"))
        .ok()?
        .to_string_lossy()
        .into_owned();
    map.by_path.get(&rel).cloned()
}

// ── reporting ───────────────────────────────────────────────────────────

fn area(rel: &str) -> String {
    let parts: Vec<&str> = rel.split('/').collect();
    let depth = if parts.get(1) == Some(&"annexB") {
        3
    } else {
        3
    };
    parts[..depth.min(parts.len() - 1)].join("/")
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--frontend-one") {
        std::panic::set_hook(Box::new(|_| {}));
        match front_end(Path::new(&args[2]), Path::new(&args[3])) {
            Ok(code) => {
                if let Some(out) = args.get(4) {
                    std::fs::write(out, code).unwrap();
                }
                println!("FE OK")
            }
            Err(error) => println!("FE ERR {}", error.replace('\n', " ")),
        }
        return;
    }
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir.join("../..").canonicalize().unwrap();
    let test262 = PathBuf::from(
        arg(&args, "--test262")
            .or_else(|| std::env::var("W3COS_TEST262_DIR").ok())
            .expect("--test262 <checkout> or W3COS_TEST262_DIR"),
    );
    let jobs: usize = arg(&args, "--jobs")
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
        });
    let build_jobs: usize = arg(&args, "--build-jobs")
        .and_then(|v| v.parse().ok())
        .unwrap_or((jobs / 2).max(1));
    let batch_size: usize = arg(&args, "--batch")
        .and_then(|v| v.parse().ok())
        .unwrap_or(150);
    let timeout = Duration::from_secs(
        arg(&args, "--timeout")
            .and_then(|v| v.parse().ok())
            .unwrap_or(10),
    );
    let work = PathBuf::from(arg(&args, "--work").unwrap_or_else(|| {
        workspace
            .join("target/test262-work")
            .to_string_lossy()
            .into_owned()
    }));
    let limit: Option<usize> = arg(&args, "--limit").and_then(|v| v.parse().ok());
    let fe_timeout = Duration::from_secs(
        arg(&args, "--frontend-timeout")
            .and_then(|v| v.parse().ok())
            .unwrap_or(30),
    );

    // Selection: --subset file of prefixes, positional prefixes, or the
    // default conformance roots.
    let mut prefixes: Vec<String> = arg(&args, "--subset")
        .map(|f| read_list_file(Path::new(&f)))
        .unwrap_or_default();
    let mut skip_next = false;
    for a in args.iter().skip(1) {
        if skip_next {
            skip_next = false;
            continue;
        }
        if a.starts_with("--") {
            skip_next = !matches!(a.as_str(), "--update-expected");
            continue;
        }
        prefixes.push(a.clone());
    }
    if prefixes.is_empty() {
        prefixes = DEFAULT_ROOTS.iter().map(|s| s.to_string()).collect();
    }
    let mut files = Vec::new();
    for prefix in &prefixes {
        let path = test262.join(prefix);
        if path.is_file() {
            files.push(path);
        } else {
            walk(&path, &mut files);
        }
    }
    files.sort();
    files.dedup();
    if let Some(limit) = limit {
        files.truncate(limit);
    }
    let tests: Vec<TestCase> = files
        .into_iter()
        .map(|path| {
            let source = std::fs::read_to_string(&path).unwrap_or_default();
            let rel = path
                .strip_prefix(&test262)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            TestCase {
                meta: parse_meta(&source),
                rel,
                path,
                source,
            }
        })
        .collect();
    eprintln!(
        "test262: {} tests, jobs {jobs}, batch {batch_size}",
        tests.len()
    );
    let _ = std::fs::remove_dir_all(work.join("tree"));
    let _ = std::fs::remove_dir_all(work.join("batches"));
    std::fs::create_dir_all(&work).unwrap();
    let ws = Arc::new(Workspace {
        test262: test262.clone(),
        work: work.clone(),
        harness: Mutex::new(HashMap::new()),
    });
    let results: Arc<Mutex<BTreeMap<usize, Outcome>>> = Arc::new(Mutex::new(BTreeMap::new()));
    let started = Instant::now();

    // Phase 1: classify + per-test front end (parallel).
    let runnable: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
    let next = Arc::new(AtomicUsize::new(0));
    let tests = Arc::new(tests);
    std::thread::scope(|scope| {
        for _ in 0..jobs {
            let (ws, results, runnable, next, tests) = (&ws, &results, &runnable, &next, &tests);
            scope.spawn(move || {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(test) = tests.get(index) else { break };
                    let outcome = if test.has_flag("noStrict") {
                        Some(Outcome::Fail("unsupported: sloppy mode".into()))
                    } else if test.source.len() > MAX_TEST_SOURCE {
                        // Generated Unicode tables (language/identifiers/*unicode*,
                        // property escapes) expand to 100+ MB of Rust and stall a
                        // batch build for most of an hour.
                        Some(Outcome::Fail("unsupported: oversized test source".into()))
                    } else {
                        let path = ws.write_test_module(test);
                        let early = test
                            .meta
                            .negative
                            .as_ref()
                            .filter(|(phase, _)| phase == "parse" || phase == "resolution");
                        match (
                            front_end_isolated(&ws.work.join("tree"), &path, fe_timeout),
                            early,
                        ) {
                            (Err(_), Some(_)) => Some(Outcome::Pass),
                            (Ok(_), Some((phase, ty))) => {
                                Some(Outcome::Fail(format!("{phase}: expected {ty}, compiled")))
                            }
                            (Err(error), None) => Some(Outcome::Fail(error)),
                            (Ok(_), None) => None,
                        }
                    };
                    match outcome {
                        Some(outcome) => {
                            results.lock().unwrap().insert(index, outcome);
                        }
                        None => runnable.lock().unwrap().push(index),
                    }
                }
            });
        }
    });
    let mut runnable = Arc::try_unwrap(runnable).unwrap().into_inner().unwrap();
    runnable.sort();
    eprintln!(
        "test262: front end done in {:.0?}: {} runnable, {} decided",
        started.elapsed(),
        runnable.len(),
        results.lock().unwrap().len()
    );

    // Phase 2: batches grouped by harness key, built and run in parallel.
    let mut by_key: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for index in runnable {
        by_key
            .entry(tests[index].harness_key())
            .or_default()
            .push(index);
    }
    let mut batches = Vec::new();
    for (_, members) in by_key {
        for chunk in members.chunks(batch_size) {
            batches.push(Batch {
                tests: chunk.to_vec(),
            });
        }
    }
    let total_batches = batches.len();
    eprintln!("test262: {total_batches} batches");
    let batches = Arc::new(Mutex::new(
        batches.into_iter().enumerate().collect::<Vec<_>>(),
    ));
    let done_batches = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for slot in 0..build_jobs {
            let (ws, results, batches, tests, workspace, done_batches) =
                (&ws, &results, &batches, &tests, &workspace, &done_batches);
            scope.spawn(move || {
                loop {
                    let Some((batch_index, batch)) = batches.lock().unwrap().pop() else {
                        break;
                    };
                    let built = build_batch(
                        ws,
                        workspace,
                        slot,
                        batch_index,
                        tests,
                        batch.tests,
                        results,
                    );
                    if let Some((bin, members)) = built {
                        // Run this batch's tests with a small per-batch pool.
                        let per = (jobs / build_jobs).max(1);
                        let cursor = AtomicUsize::new(0);
                        std::thread::scope(|inner| {
                            for _ in 0..per {
                                inner.spawn(|| {
                                    loop {
                                        let pos = cursor.fetch_add(1, Ordering::Relaxed);
                                        let Some(&index) = members.get(pos) else {
                                            break;
                                        };
                                        if results.lock().unwrap().contains_key(&index) {
                                            continue;
                                        }
                                        let mut command = Command::new(&bin);
                                        command.arg(pos.to_string());
                                        let outcome = match run_with_timeout(command, timeout) {
                                            Ok(output) => judge(&tests[index], &output),
                                            Err(error) => Outcome::Fail(error),
                                        };
                                        results.lock().unwrap().insert(index, outcome);
                                    }
                                });
                            }
                        });
                        if std::env::var_os("W3COS_T262_KEEP").is_none() {
                            let _ = std::fs::remove_file(&bin);
                        }
                    }
                    let done = done_batches.fetch_add(1, Ordering::Relaxed) + 1;
                    eprintln!(
                        "test262: batch {done}/{total_batches} ({:.0?})",
                        started.elapsed()
                    );
                }
            });
        }
    });

    // Report.
    let results = results.lock().unwrap();
    let mut passed = BTreeSet::new();
    let mut by_area: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut by_feature: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    let mut json_tests = serde_json::Map::new();
    for (index, test) in tests.iter().enumerate() {
        let outcome = results
            .get(&index)
            .cloned()
            .unwrap_or(Outcome::Fail("not run".into()));
        let ok = outcome == Outcome::Pass;
        if ok {
            passed.insert(test.rel.clone());
        }
        let entry = by_area.entry(area(&test.rel)).or_default();
        entry.1 += 1;
        entry.0 += ok as usize;
        let features = if test.meta.features.is_empty() {
            vec!["(none)".to_string()]
        } else {
            test.meta.features.clone()
        };
        for feature in features {
            let entry = by_feature.entry(feature).or_default();
            entry.1 += 1;
            entry.0 += ok as usize;
        }
        let status = match &outcome {
            Outcome::Pass => "pass".to_string(),
            Outcome::Fail(reason) => {
                let category = reason.split(':').next().unwrap_or("").to_string();
                *reasons.entry(category).or_default() += 1;
                format!("fail: {reason}")
            }
        };
        json_tests.insert(test.rel.clone(), serde_json::Value::String(status));
    }
    let total = tests.len();
    let pct = |p: usize, t: usize| {
        if t == 0 {
            0.0
        } else {
            100.0 * p as f64 / t as f64
        }
    };
    let commit = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&test262)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let mut summary = format!(
        "# test262 (AOT pipeline)\n\n- test262: `{commit}`\n- selection: {}\n- passed: **{} / {} ({:.2}%)**\n- elapsed: {:.0?}\n\n## Failure stages\n\n| stage | tests |\n|---|---:|\n",
        prefixes.join(", "),
        passed.len(),
        total,
        pct(passed.len(), total),
        started.elapsed()
    );
    for (reason, count) in &reasons {
        summary.push_str(&format!("| {reason} | {count} |\n"));
    }
    summary.push_str("\n## By area\n\n| area | pass | total | % |\n|---|---:|---:|---:|\n");
    for (name, (p, t)) in &by_area {
        summary.push_str(&format!("| {name} | {p} | {t} | {:.1} |\n", pct(*p, *t)));
    }
    summary.push_str("\n## By feature\n\n| feature | pass | total | % |\n|---|---:|---:|---:|\n");
    for (name, (p, t)) in &by_feature {
        summary.push_str(&format!("| {name} | {p} | {t} | {:.1} |\n", pct(*p, *t)));
    }
    println!(
        "test262: passed {} / {} ({:.2}%) in {:.0?}",
        passed.len(),
        total,
        pct(passed.len(), total),
        started.elapsed()
    );
    if let Some(path) = arg(&args, "--summary") {
        std::fs::write(path, &summary).unwrap();
    }
    if let Some(path) = arg(&args, "--report") {
        let report = serde_json::json!({
            "commit": commit,
            "passed": passed.len(),
            "total": total,
            "areas": by_area.iter().map(|(k, (p, t))| (k.clone(), serde_json::json!([p, t]))).collect::<serde_json::Map<_, _>>(),
            "features": by_feature.iter().map(|(k, (p, t))| (k.clone(), serde_json::json!([p, t]))).collect::<serde_json::Map<_, _>>(),
            "tests": json_tests,
        });
        std::fs::write(path, serde_json::to_string_pretty(&report).unwrap()).unwrap();
    }

    // Gate: expected-failures list for the selection.
    if let Some(expected_path) = arg(&args, "--expected") {
        let expected: BTreeSet<String> = read_list_file(Path::new(&expected_path))
            .into_iter()
            .collect();
        let failing: BTreeSet<String> = tests
            .iter()
            .map(|t| t.rel.clone())
            .filter(|r| !passed.contains(r))
            .collect();
        if args.iter().any(|a| a == "--update-expected") {
            let body: String = failing.iter().map(|r| format!("{r}\n")).collect();
            std::fs::write(
                &expected_path,
                format!("# test262 expected failures for the CI subset (regenerate with --update-expected)\n{body}"),
            )
            .unwrap();
            println!("test262: wrote {} expected failures", failing.len());
            return;
        }
        let regressions: Vec<_> = failing.difference(&expected).collect();
        let fixed: Vec<_> = expected.iter().filter(|r| passed.contains(*r)).collect();
        for rel in &fixed {
            println!("FIXED (remove from expected failures): {rel}");
        }
        for rel in &regressions {
            let index = tests.iter().position(|t| &&t.rel == rel).unwrap();
            println!("REGRESSION: {rel}: {:?}", results.get(&index));
        }
        if !regressions.is_empty() {
            println!(
                "test262: {} regression(s) against {expected_path}",
                regressions.len()
            );
            std::process::exit(1);
        }
        println!("test262: no regressions ({} newly passing)", fixed.len());
    }
}
