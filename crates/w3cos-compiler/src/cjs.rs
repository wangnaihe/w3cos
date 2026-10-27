//! CommonJS static linking.
//!
//! A CommonJS file is compiled as two synthesized ESM modules (served through
//! [`crate::module_source`]) so the rest of the pipeline (graph, bundle,
//! W3IR lowering, AOT) only ever sees ESM:
//!
//! - **record** (`<file>.w3cos-cjs.js`): the file's body wrapped in
//!   `function (exports, require, module, __filename, __dirname)`, plus a
//!   per-file `require` whose cases are the file's literal `require("...")`
//!   specifiers, resolved at compile time with the Node algorithm
//!   (`require` conditions). Its default export is a load-once thunk that
//!   returns `module.exports`. Dependencies are imported statically: a
//!   CommonJS dependency's record thunk, an ESM dependency's namespace, a JSON
//!   file's value.
//! - **facade** (the file's own path): what ESM importers see, with Node's
//!   interop semantics: `default` is `module.exports`, and named exports are
//!   the names found by a cjs-module-lexer style scan (plus the names the
//!   graph's importers ask for), read from `module.exports` when the facade
//!   evaluates.
//!
//! All record bindings are function declarations, so records can reference
//! each other in cycles without TDZ hazards; the shared runtime puts a module
//! in its cache before running the body, so a circular `require` sees the
//! partial `module.exports`, as in Node.

use anyhow::{Result, anyhow};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use swc_common::{FileName, SourceMap, sync::Lrc};
use swc_ecma_ast::*;
use swc_ecma_parser::{EsSyntax, Parser, StringInput, Syntax};
use swc_ecma_visit::{Visit, VisitWith};

/// What a CommonJS file needs from the linker.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CjsAnalysis {
    /// Literal `require("...")` / `require.resolve("...")` specifiers in
    /// first-seen order.
    pub requires: Vec<String>,
    /// Statically detectable export names (`exports.x =`, ...).
    pub export_names: Vec<String>,
    /// Specifiers whose exports are re-exported wholesale
    /// (`module.exports = require(x)`, `__exportStar(require(x), exports)`).
    pub reexports: Vec<String>,
    /// Node globals the file reads without declaring (`Buffer`): the record
    /// binds them from their builtin module, as Node's globals would.
    pub implicit_globals: Vec<String>,
    /// Statically named worker modules (`new Worker(path.join(__dirname,
    /// 'w.js'))`, `utilityProcess.fork(...)`), as specifiers relative to the
    /// file: each runs as a worker entry (see [`WORKER_TARGET_PREFIX`]).
    pub worker_entries: Vec<String>,
}

/// `require()` key under which the program entry links a worker module.
pub const WORKER_TARGET_PREFIX: &str = "w3cos-worker:";

/// Free Node globals backed by a builtin: (global, specifier, member).
pub const IMPLICIT_GLOBALS: &[(&str, &str, &str)] = &[("Buffer", "node:buffer", "Buffer")];

/// Classify `source` at `path`. Returns `Some` for CommonJS:
/// - `.cjs` files;
/// - `.js` / `.jsx` / extensionless files outside a `"type": "module"`
///   scope that contain no ESM syntax and reference `require`, `module` or
///   `exports`.
///
/// TypeScript, `.mjs` and JSON files are never CommonJS here.
pub fn classify(
    path: &Path,
    source: &str,
    in_type_module_scope: bool,
) -> Result<Option<CjsAnalysis>> {
    classify_as(path, source, in_type_module_scope, false)
}

/// [`classify`]; a `classic_script` (an Electron preload or page script,
/// evaluated when a window loads it) is CommonJS unless it has module
/// syntax, even without `require`/`module`/`exports`, so it gets a lazily
/// evaluated record instead of running at startup like an ES module.
pub fn classify_as(
    path: &Path,
    source: &str,
    in_type_module_scope: bool,
    classic_script: bool,
) -> Result<Option<CjsAnalysis>> {
    let extension = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");
    let forced = extension == "cjs";
    if !forced && !classic_script {
        if !matches!(extension, "js" | "jsx" | "") || in_type_module_scope {
            return Ok(None);
        }
        // Cheap pre-filter before parsing.
        if !(source.contains("require") || source.contains("exports") || source.contains("module"))
        {
            return Ok(None);
        }
    }
    let body = strip_hashbang(source);
    let cm: Lrc<SourceMap> = Default::default();
    let fm = cm.new_source_file(
        Lrc::new(FileName::Real(path.to_path_buf())),
        body.to_string(),
    );
    let mut parser = Parser::new(
        Syntax::Es(EsSyntax {
            jsx: extension == "jsx",
            ..Default::default()
        }),
        StringInput::from(&*fm),
        None,
    );
    let program = match parser.parse_program() {
        Ok(program) => program,
        Err(error) if forced => {
            return Err(anyhow!(
                "CommonJS parse error in {}: {:?}",
                path.display(),
                error
            ));
        }
        // Leave the diagnostic to the ESM parser.
        Err(_) => return Ok(None),
    };
    let statements: Vec<Stmt> = match program {
        Program::Script(script) => script.body,
        Program::Module(module) => {
            if module
                .body
                .iter()
                .any(|item| matches!(item, ModuleItem::ModuleDecl(_)))
            {
                return Ok(None);
            }
            module
                .body
                .into_iter()
                .filter_map(|item| match item {
                    ModuleItem::Stmt(statement) => Some(statement),
                    ModuleItem::ModuleDecl(_) => None,
                })
                .collect()
        }
    };
    let mut scan = RequireScan::default();
    for statement in &statements {
        statement.visit_with(&mut scan);
    }
    if !forced && !classic_script && !scan.uses_commonjs {
        return Ok(None);
    }
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut worker_entries: Vec<String> = Vec::new();
    let named = scan.worker_idents.iter().filter_map(|name| {
        scan.path_consts
            .iter()
            .find(|(binding, _)| binding == name)
            .map(|(_, path)| path)
    });
    for entry in scan
        .worker_args
        .iter()
        .chain(named)
        .filter_map(|arg| arg.relative_specifier(&file_name))
    {
        if !worker_entries.contains(&entry) {
            worker_entries.push(entry);
        }
    }
    let mut analysis = CjsAnalysis {
        requires: scan.requires,
        worker_entries,
        ..Default::default()
    };
    for (global, specifier, _) in IMPLICIT_GLOBALS {
        if scan.globals_read.contains(*global) && !declares_top_level(&statements, global) {
            analysis.implicit_globals.push((*global).to_string());
            if !analysis
                .requires
                .iter()
                .any(|existing| existing == specifier)
            {
                analysis.requires.push((*specifier).to_string());
            }
        }
    }
    let mut lexer = ExportLexer::default();
    for statement in &statements {
        lexer.statement(statement);
    }
    analysis.export_names = lexer.names.into_iter().collect();
    analysis.reexports = lexer.reexports;
    Ok(Some(analysis))
}

pub fn strip_hashbang(source: &str) -> &str {
    if source.starts_with("#!") {
        // Keep the newline so line numbers stay stable.
        &source[source.find('\n').unwrap_or(source.len())..]
    } else {
        source
    }
}

#[derive(Default)]
struct RequireScan {
    requires: Vec<String>,
    /// First arguments of worker constructors, when statically known.
    worker_args: Vec<WorkerPath>,
    /// Worker constructor arguments that name a binding (`new Worker(file)`).
    worker_idents: Vec<String>,
    /// `const file = path.join(__dirname, 'w.js')` bindings.
    path_consts: Vec<(String, WorkerPath)>,
    uses_commonjs: bool,
    /// Implicit-global names that appear as identifiers.
    globals_read: BTreeSet<String>,
}

/// Imports an ES module needs for Node implicit globals it reads without
/// declaring (`typeof Buffer !== "undefined"` in Code-OSS `buffer.js`), as
/// source text to prepend: `import { Buffer } from "node:buffer";`. Empty
/// when the module declares, imports, or never reads them.
pub fn esm_implicit_global_imports(path: &Path, source: &str) -> String {
    if !IMPLICIT_GLOBALS
        .iter()
        .any(|(global, _, _)| source.contains(global))
    {
        return String::new();
    }
    let cm: Lrc<SourceMap> = Default::default();
    let fm = cm.new_source_file(
        Lrc::new(FileName::Real(path.to_path_buf())),
        strip_hashbang(source).to_string(),
    );
    let jsx = path.extension().is_some_and(|ext| ext == "jsx");
    let mut parser = Parser::new(
        Syntax::Es(EsSyntax {
            jsx,
            ..Default::default()
        }),
        StringInput::from(&*fm),
        None,
    );
    let Ok(module) = parser.parse_module() else {
        return String::new();
    };
    let mut statements = Vec::new();
    let mut imported = BTreeSet::new();
    let mut scan = RequireScan::default();
    for item in module.body {
        match item {
            ModuleItem::Stmt(statement) => statements.push(statement),
            ModuleItem::ModuleDecl(ModuleDecl::Import(import)) => {
                for specifier in &import.specifiers {
                    let local = match specifier {
                        ImportSpecifier::Named(named) => &named.local,
                        ImportSpecifier::Default(default) => &default.local,
                        ImportSpecifier::Namespace(namespace) => &namespace.local,
                    };
                    imported.insert(local.sym.to_string());
                }
            }
            ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => {
                statements.push(Stmt::Decl(export.decl));
            }
            ModuleItem::ModuleDecl(declaration) => {
                // `export default <expr>` and friends still read identifiers.
                if let ModuleDecl::ExportDefaultDecl(default) = &declaration {
                    let ident = match &default.decl {
                        DefaultDecl::Class(class) => class.ident.as_ref(),
                        DefaultDecl::Fn(function) => function.ident.as_ref(),
                        _ => None,
                    };
                    if let Some(ident) = ident {
                        imported.insert(ident.sym.to_string());
                    }
                }
                declaration.visit_with(&mut scan);
            }
        }
    }
    for statement in &statements {
        statement.visit_with(&mut scan);
    }
    IMPLICIT_GLOBALS
        .iter()
        .filter(|(global, _, _)| {
            scan.globals_read.contains(*global)
                && !imported.contains(*global)
                && !declares_top_level(&statements, global)
        })
        .map(|(global, specifier, member)| {
            format!(
                "import {{ {member} as {global} }} from {};\n",
                js_string(specifier)
            )
        })
        .collect()
}

/// Does a top-level statement declare `name` (var/let/const/function/class)?
fn declares_top_level(statements: &[Stmt], name: &str) -> bool {
    fn pat_binds(pat: &Pat, name: &str) -> bool {
        match pat {
            Pat::Ident(ident) => ident.id.sym == *name,
            Pat::Array(array) => array
                .elems
                .iter()
                .flatten()
                .any(|elem| pat_binds(elem, name)),
            Pat::Object(object) => object.props.iter().any(|prop| match prop {
                ObjectPatProp::KeyValue(kv) => pat_binds(&kv.value, name),
                ObjectPatProp::Assign(assign) => assign.key.id.sym == *name,
                ObjectPatProp::Rest(rest) => pat_binds(&rest.arg, name),
            }),
            Pat::Rest(rest) => pat_binds(&rest.arg, name),
            Pat::Assign(assign) => pat_binds(&assign.left, name),
            _ => false,
        }
    }
    statements.iter().any(|statement| match statement {
        Stmt::Decl(Decl::Var(var)) => var.decls.iter().any(|decl| pat_binds(&decl.name, name)),
        Stmt::Decl(Decl::Fn(function)) => function.ident.sym == *name,
        Stmt::Decl(Decl::Class(class)) => class.ident.sym == *name,
        _ => false,
    })
}

fn literal_string(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Lit(Lit::Str(s)) => Some(s.value.to_string_lossy().into_owned()),
        Expr::Tpl(tpl) if tpl.exprs.is_empty() && tpl.quasis.len() == 1 => tpl.quasis[0]
            .cooked
            .as_ref()
            .map(|cooked| cooked.to_string_lossy().into_owned()),
        Expr::Paren(paren) => literal_string(&paren.expr),
        _ => None,
    }
}

fn is_ident(expr: &Expr, name: &str) -> bool {
    matches!(expr, Expr::Ident(ident) if ident.sym == *name)
}

/// `require("x")` → `Some("x")`.
fn require_call(expr: &Expr) -> Option<String> {
    let Expr::Call(call) = expr else { return None };
    let Callee::Expr(callee) = &call.callee else {
        return None;
    };
    if !is_ident(callee, "require") || call.args.len() != 1 || call.args[0].spread.is_some() {
        return None;
    }
    literal_string(&call.args[0].expr)
}

fn member_prop_name(prop: &MemberProp) -> Option<String> {
    match prop {
        MemberProp::Ident(ident) => Some(ident.sym.to_string()),
        MemberProp::Computed(computed) => literal_string(&computed.expr),
        _ => None,
    }
}

/// A worker module path the compiler can name: `__filename`, or segments
/// below `__dirname`.
#[derive(Clone)]
enum WorkerPath {
    SelfFile,
    BelowDir(Vec<String>),
}

impl WorkerPath {
    fn relative_specifier(&self, file_name: &str) -> Option<String> {
        match self {
            WorkerPath::SelfFile => Some(format!("./{file_name}")),
            WorkerPath::BelowDir(segments) => {
                let joined = segments
                    .iter()
                    .flat_map(|segment| segment.split('/'))
                    .filter(|part| !part.is_empty() && *part != ".")
                    .collect::<Vec<_>>()
                    .join("/");
                (!joined.is_empty()).then(|| format!("./{joined}"))
            }
        }
    }
}

/// `__filename`, `path.join(__dirname, 'a', 'b')` / `path.resolve(...)`,
/// `__dirname + '/w.js'` and `` `${__dirname}/w.js` ``.
fn worker_path(expr: &Expr) -> Option<WorkerPath> {
    match expr {
        Expr::Paren(paren) => worker_path(&paren.expr),
        Expr::Ident(ident) if ident.sym == *"__filename" => Some(WorkerPath::SelfFile),
        Expr::Call(call) => {
            let Callee::Expr(callee) = &call.callee else {
                return None;
            };
            let name = match &**callee {
                Expr::Ident(ident) => ident.sym.to_string(),
                Expr::Member(member) => member_prop_name(&member.prop)?,
                _ => return None,
            };
            if !matches!(name.as_str(), "join" | "resolve") || call.args.len() < 2 {
                return None;
            }
            if call.args.iter().any(|arg| arg.spread.is_some())
                || !is_ident(&call.args[0].expr, "__dirname")
            {
                return None;
            }
            let segments = call.args[1..]
                .iter()
                .map(|arg| literal_string(&arg.expr))
                .collect::<Option<Vec<_>>>()?;
            if segments.iter().any(|segment| segment.starts_with('/')) {
                return None;
            }
            Some(WorkerPath::BelowDir(segments))
        }
        Expr::Bin(bin) if bin.op == BinaryOp::Add && is_ident(&bin.left, "__dirname") => {
            let rest = literal_string(&bin.right)?;
            rest.starts_with('/')
                .then(|| WorkerPath::BelowDir(vec![rest]))
        }
        Expr::Tpl(tpl)
            if tpl.exprs.len() == 1
                && is_ident(&tpl.exprs[0], "__dirname")
                && tpl.quasis.len() == 2
                && tpl.quasis[0].raw.is_empty() =>
        {
            let rest = tpl.quasis[1]
                .cooked
                .as_ref()?
                .to_string_lossy()
                .into_owned();
            rest.starts_with('/')
                .then(|| WorkerPath::BelowDir(vec![rest]))
        }
        _ => None,
    }
}

/// A worker constructor's first argument: a path expression, or a binding
/// to resolve against `const` path declarations.
fn note_worker_arg(scan: &mut RequireScan, expr: &Expr) {
    if let Some(path) = worker_path(expr) {
        scan.worker_args.push(path);
    } else if let Expr::Ident(ident) = expr {
        scan.worker_idents.push(ident.sym.to_string());
    }
}

impl Visit for RequireScan {
    fn visit_var_decl(&mut self, decl: &VarDecl) {
        if decl.kind == VarDeclKind::Const {
            for declarator in &decl.decls {
                if let Pat::Ident(binding) = &declarator.name
                    && let Some(init) = &declarator.init
                    && let Some(path) = worker_path(init)
                {
                    self.path_consts.push((binding.id.sym.to_string(), path));
                }
            }
        }
        decl.visit_children_with(self);
    }

    fn visit_new_expr(&mut self, new: &NewExpr) {
        let is_worker = match &*new.callee {
            Expr::Ident(ident) => ident.sym == *"Worker",
            Expr::Member(member) => member_prop_name(&member.prop).as_deref() == Some("Worker"),
            _ => false,
        };
        if is_worker
            && let Some(args) = &new.args
            && let Some(first) = args.first()
            && first.spread.is_none()
        {
            note_worker_arg(self, &first.expr);
        }
        new.visit_children_with(self);
    }

    fn visit_call_expr(&mut self, call: &CallExpr) {
        // Electron's `utilityProcess.fork(modulePath)` runs a worker entry too.
        if let Callee::Expr(callee) = &call.callee
            && let Expr::Member(member) = &**callee
            && member_prop_name(&member.prop).as_deref() == Some("fork")
            && match &*member.obj {
                Expr::Ident(ident) => ident.sym == *"utilityProcess",
                Expr::Member(inner) => {
                    member_prop_name(&inner.prop).as_deref() == Some("utilityProcess")
                }
                _ => false,
            }
            && let Some(first) = call.args.first()
            && first.spread.is_none()
        {
            note_worker_arg(self, &first.expr);
        }
        if let Callee::Expr(callee) = &call.callee {
            let is_require = is_ident(callee, "require")
                || matches!(&**callee, Expr::Member(member)
                    if is_ident(&member.obj, "require")
                        && member_prop_name(&member.prop).as_deref() == Some("resolve"));
            if is_require && call.args.len() == 1 && call.args[0].spread.is_none() {
                if let Some(specifier) = literal_string(&call.args[0].expr) {
                    if !self.requires.contains(&specifier) {
                        self.requires.push(specifier);
                    }
                }
            }
        }
        call.visit_children_with(self);
    }

    fn visit_ident(&mut self, ident: &Ident) {
        if matches!(&*ident.sym, "require" | "module" | "exports") {
            self.uses_commonjs = true;
        }
        if IMPLICIT_GLOBALS
            .iter()
            .any(|(global, _, _)| ident.sym == **global)
        {
            self.globals_read.insert(ident.sym.to_string());
        }
    }
}

/// cjs-module-lexer style detection of export names on top-level statements.
#[derive(Default)]
struct ExportLexer {
    names: BTreeSet<String>,
    reexports: Vec<String>,
}

/// `exports` or `module.exports`.
fn is_exports_object(expr: &Expr) -> bool {
    match expr {
        Expr::Ident(ident) => ident.sym == *"exports",
        Expr::Member(member) => {
            is_ident(&member.obj, "module")
                && member_prop_name(&member.prop).as_deref() == Some("exports")
        }
        Expr::Paren(paren) => is_exports_object(&paren.expr),
        _ => false,
    }
}

impl ExportLexer {
    fn statement(&mut self, statement: &Stmt) {
        match statement {
            Stmt::Expr(expr) => self.expr(&expr.expr),
            // TS: `if (k !== "default") __createBinding(...)`, esbuild and
            // UMD guards: look one level into blocks and ifs.
            Stmt::Block(block) => block.stmts.iter().for_each(|s| self.statement(s)),
            Stmt::If(stmt) => {
                self.statement(&stmt.cons);
                if let Some(alt) = &stmt.alt {
                    self.statement(alt);
                }
            }
            _ => {}
        }
    }

    fn add_reexport(&mut self, specifier: String) {
        if !self.reexports.contains(&specifier) {
            self.reexports.push(specifier);
        }
    }

    fn expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Paren(paren) => self.expr(&paren.expr),
            Expr::Seq(seq) => seq.exprs.iter().for_each(|e| self.expr(e)),
            Expr::Unary(unary) => self.expr(&unary.arg),
            Expr::Assign(assign) => {
                if let AssignTarget::Simple(SimpleAssignTarget::Member(member)) = &assign.left {
                    if is_exports_object(&member.obj) {
                        if let Some(name) = member_prop_name(&member.prop) {
                            self.names.insert(name);
                        }
                    } else if is_ident(&member.obj, "module")
                        && member_prop_name(&member.prop).as_deref() == Some("exports")
                    {
                        self.module_exports_value(&assign.right);
                    }
                }
                // `exports.b = exports.a = void 0;`
                self.expr(&assign.right);
            }
            Expr::Call(call) => self.call(call),
            _ => {}
        }
    }

    fn module_exports_value(&mut self, value: &Expr) {
        match value {
            Expr::Paren(paren) => self.module_exports_value(&paren.expr),
            Expr::Object(object) => {
                for prop in &object.props {
                    match prop {
                        PropOrSpread::Spread(spread) => {
                            if let Some(specifier) = require_call(&spread.expr) {
                                self.add_reexport(specifier);
                            }
                        }
                        PropOrSpread::Prop(prop) => {
                            let key = match &**prop {
                                Prop::Shorthand(ident) => Some(ident.sym.to_string()),
                                Prop::KeyValue(kv) => prop_name(&kv.key),
                                Prop::Method(method) => prop_name(&method.key),
                                Prop::Getter(getter) => prop_name(&getter.key),
                                Prop::Setter(setter) => prop_name(&setter.key),
                                Prop::Assign(_) => None,
                            };
                            if let Some(key) = key {
                                self.names.insert(key);
                            }
                        }
                    }
                }
            }
            other => {
                if let Some(specifier) = require_call(other) {
                    self.add_reexport(specifier);
                }
            }
        }
    }

    fn call(&mut self, call: &CallExpr) {
        let Callee::Expr(callee) = &call.callee else {
            return;
        };
        let callee_name = match &**callee {
            Expr::Ident(ident) => Some(ident.sym.to_string()),
            Expr::Member(member) => {
                if is_ident(&member.obj, "Object")
                    && member_prop_name(&member.prop).as_deref() == Some("defineProperty")
                {
                    if let (Some(target), Some(key)) = (call.args.first(), call.args.get(1)) {
                        if is_exports_object(&target.expr) {
                            if let Some(name) = literal_string(&key.expr) {
                                self.names.insert(name);
                            }
                        }
                    }
                    return;
                }
                member_prop_name(&member.prop)
            }
            _ => None,
        };
        if matches!(
            callee_name.as_deref(),
            Some("__exportStar" | "__export" | "__reExport")
        ) {
            for arg in &call.args {
                if let Some(specifier) = require_call(&arg.expr) {
                    self.add_reexport(specifier);
                }
            }
        }
    }
}

fn prop_name(key: &PropName) -> Option<String> {
    match key {
        PropName::Ident(ident) => Some(ident.sym.to_string()),
        PropName::Str(s) => Some(s.value.to_string_lossy().into_owned()),
        _ => None,
    }
}

/// Virtual path of a CommonJS file's module record.
pub fn record_path(file: &Path) -> PathBuf {
    let mut path = file.as_os_str().to_owned();
    path.push(".w3cos-cjs.js");
    PathBuf::from(path)
}

/// Synthesized-only paths (records and the loader); never files on disk.
pub fn is_virtual(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    name.ends_with(".w3cos-cjs.js") || name == RUNTIME_FILE_NAME
}

const RUNTIME_FILE_NAME: &str = "__w3cos_cjs_runtime__.js";

/// Virtual path of the shared CommonJS loader module.
pub fn runtime_path(project_root: &Path) -> PathBuf {
    project_root.join(RUNTIME_FILE_NAME)
}

/// The shared loader: module cache (inserted before the body runs, removed
/// if it throws), `module` objects, and `require` functions.
pub const RUNTIME_SOURCE: &str = r#"var __w3cos_cjs_cache;
export function __w3cos_cjs_load(filename, dirname, factory, resolveRequire, resolvePath) {
  if (__w3cos_cjs_cache === undefined) __w3cos_cjs_cache = {};
  const cached = __w3cos_cjs_cache[filename];
  if (cached !== undefined) return cached.exports;
  const module = { id: filename, filename: filename, path: dirname, loaded: false, exports: {}, children: [], paths: [] };
  const require = function require(specifier) {
    return resolveRequire(String(specifier), module);
  };
  require.resolve = function resolve(specifier) {
    const resolved = resolvePath(String(specifier));
    if (resolved === undefined) return __w3cos_cjs_missing(String(specifier), filename);
    return resolved;
  };
  require.cache = __w3cos_cjs_cache;
  module.require = require;
  __w3cos_cjs_cache[filename] = module;
  try {
    factory.call(module.exports, module.exports, require, module, filename, dirname);
  } catch (error) {
    delete __w3cos_cjs_cache[filename];
    throw error;
  }
  module.loaded = true;
  return module.exports;
}
export function __w3cos_cjs_missing(specifier, from, module) {
  // Not linked at compile time: a run-time module loader (Node's `module`
  // glue, `Module._load` and whatever patched it, e.g. the Code-OSS extension
  // host's `vscode` API interceptor) may still serve it.
  const loader = globalThis.__w3cos_cjs_runtime_load;
  if (module !== undefined && typeof loader === "function") {
    const loaded = loader(specifier, module);
    if (loaded !== undefined) return loaded.value;
  }
  const error = new Error("Cannot find module '" + specifier + "' from '" + from + "'");
  error.code = "MODULE_NOT_FOUND";
  throw error;
}
export function __w3cos_cjs_builtin(specifier) {
  const error = new Error("Node builtin module '" + specifier + "' is not available in this w3cos runtime");
  error.code = "ERR_W3COS_BUILTIN_UNAVAILABLE";
  throw error;
}
"#;

/// A compile-time resolved `require()` target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequireTarget {
    /// Another CommonJS file; the path is the file (not its record).
    CommonJs(PathBuf),
    /// An ESM module: `require()` returns its namespace object.
    Esm(PathBuf),
    /// A JSON file: `require()` returns the parsed value.
    Json(PathBuf),
    /// A Node builtin (throws until the runtime provides it).
    Builtin(String),
}

fn js_string(value: &str) -> String {
    serde_json::to_string(value).expect("string serialization cannot fail")
}

/// Module record source and its static imports.
pub fn record_source(
    file: &Path,
    body: &str,
    targets: &[(String, RequireTarget)],
    runtime: &Path,
) -> (String, Vec<String>) {
    record_source_with_globals(file, body, targets, runtime, &[])
}

/// [`record_source`] plus bindings for `implicit_globals` (see
/// [`CjsAnalysis::implicit_globals`]) at the top of the module body.
pub fn record_source_with_globals(
    file: &Path,
    body: &str,
    targets: &[(String, RequireTarget)],
    runtime: &Path,
    implicit_globals: &[String],
) -> (String, Vec<String>) {
    record_source_with_prelude(file, body, targets, runtime, implicit_globals, "")
}

/// Statements the program entry runs first on a worker thread: hand over to
/// the worker module the thread was started for (linked under
/// [`WORKER_TARGET_PREFIX`]); a worker started on the entry file itself
/// (`new Worker(__filename)`) keeps running the entry.
pub fn worker_boot_prelude(entry: &Path, workers: &[PathBuf]) -> String {
    if workers.is_empty() {
        return String::new();
    }
    let paths = workers
        .iter()
        .map(|path| js_string(&path.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(", ");
    let cases = workers
        .iter()
        .filter(|path| path.as_path() != entry)
        .map(|path| {
            let path = path.to_string_lossy();
            format!(
                "    case {}: require({}); break;\n",
                js_string(&path),
                js_string(&format!("{WORKER_TARGET_PREFIX}{path}"))
            )
        })
        .collect::<String>();
    format!(
        "var __w3cos_worker_boot = require(\"node:internal/w3cos_worker\");\n\
__w3cos_worker_boot.compiled([{paths}]);\n\
var __w3cos_worker_entry = __w3cos_worker_boot.entry();\n\
if (__w3cos_worker_entry !== undefined && __w3cos_worker_entry !== __filename) {{\n\
  switch (__w3cos_worker_entry) {{\n{cases}    default: __w3cos_worker_boot.missing(__w3cos_worker_entry);\n  }}\n\
  return;\n}}\n"
    )
}

/// [`record_source_with_globals`] plus `extra_prelude` statements before the
/// module body (see [`worker_boot_prelude`]).
pub fn record_source_with_prelude(
    file: &Path,
    body: &str,
    targets: &[(String, RequireTarget)],
    runtime: &Path,
    implicit_globals: &[String],
    extra_prelude: &str,
) -> (String, Vec<String>) {
    let prelude: String = IMPLICIT_GLOBALS
        .iter()
        .filter(|(global, _, _)| implicit_globals.iter().any(|name| name == global))
        .map(|(global, specifier, member)| {
            format!(
                "var {global} = require({}).{member};\n",
                js_string(specifier)
            )
        })
        .collect::<String>()
        + extra_prelude;
    // Keep a leading "use strict" directive first in the function body.
    let leading = strip_hashbang(body).trim_start();
    let prelude = if !prelude.is_empty()
        && (leading.starts_with("'use strict'") || leading.starts_with("\"use strict\""))
    {
        format!("\"use strict\";\n{prelude}")
    } else {
        prelude
    };
    let filename = file.to_string_lossy();
    let dirname = file
        .parent()
        .map(|dir| dir.to_string_lossy().into_owned())
        .unwrap_or_default();
    let runtime = runtime.to_string_lossy().into_owned();
    let mut imports = vec![runtime.clone()];
    let mut out = format!(
        "import {{ __w3cos_cjs_load, __w3cos_cjs_missing, __w3cos_cjs_builtin }} from {};\n",
        js_string(&runtime)
    );
    let mut require_cases = String::new();
    let mut resolve_cases = String::new();
    for (index, (specifier, target)) in targets.iter().enumerate() {
        let local = format!("__w3cos_cjs_dep{index}");
        let (value, resolved) = match target {
            RequireTarget::CommonJs(path) => {
                let record = record_path(path).to_string_lossy().into_owned();
                out.push_str(&format!("import {local} from {};\n", js_string(&record)));
                imports.push(record);
                (format!("{local}()"), path.to_string_lossy().into_owned())
            }
            RequireTarget::Esm(path) => {
                let path = path.to_string_lossy().into_owned();
                out.push_str(&format!("import * as {local} from {};\n", js_string(&path)));
                imports.push(path.clone());
                (local, path)
            }
            RequireTarget::Json(path) => {
                let path = path.to_string_lossy().into_owned();
                out.push_str(&format!("import {local} from {};\n", js_string(&path)));
                imports.push(path.clone());
                (local, path)
            }
            RequireTarget::Builtin(name) => (
                format!("__w3cos_cjs_builtin({})", js_string(name)),
                name.clone(),
            ),
        };
        require_cases.push_str(&format!(
            "    case {}: return {value};\n",
            js_string(specifier)
        ));
        resolve_cases.push_str(&format!(
            "    case {}: return {};\n",
            js_string(specifier),
            js_string(&resolved)
        ));
    }
    out.push_str(&format!(
        "function __w3cos_cjs_require(specifier, module) {{\n  switch (specifier) {{\n{require_cases}  }}\n  return __w3cos_cjs_missing(specifier, {file}, module);\n}}\n\
function __w3cos_cjs_resolve(specifier) {{\n  switch (specifier) {{\n{resolve_cases}  }}\n  return undefined;\n}}\n\
export default function __w3cos_cjs_record() {{\n  return __w3cos_cjs_load({file}, {dir}, __w3cos_cjs_factory, __w3cos_cjs_require, __w3cos_cjs_resolve);\n}}\n\
function __w3cos_cjs_factory(exports, require, module, __filename, __dirname) {{\n{prelude}{body}\n}}\n",
        file = js_string(&filename),
        dir = js_string(&dirname),
        body = strip_hashbang(body),
    ));
    (out, imports)
}

const RESERVED: &[&str] = &[
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "implements",
    "import",
    "in",
    "instanceof",
    "interface",
    "let",
    "new",
    "null",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
    "arguments",
    "eval",
    "undefined",
];

fn is_exportable_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first == '$' || first.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c == '$' || c.is_ascii_alphanumeric())
        && !RESERVED.contains(&name)
        && !name.starts_with("__w3cos_cjs")
        && name != "__esModule"
}

/// ESM facade source and its static imports.
pub fn facade_source<'a>(
    file: &Path,
    names: impl IntoIterator<Item = &'a String>,
) -> (String, Vec<String>) {
    let record = record_path(file).to_string_lossy().into_owned();
    let mut out = format!(
        "import __w3cos_cjs_record from {};\nconst __w3cos_cjs_exports = __w3cos_cjs_record();\nexport default __w3cos_cjs_exports;\n",
        js_string(&record)
    );
    let mut seen = BTreeSet::new();
    for name in names {
        if is_exportable_identifier(name) && seen.insert(name.as_str()) {
            out.push_str(&format!(
                "export const {name} = __w3cos_cjs_exports.{name};\n"
            ));
        }
    }
    (out, vec![record])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esm_modules_reading_buffer_import_it_from_node_buffer() {
        let path = Path::new("/p/buffer.js");
        let reads = "import { Lazy } from './lazy.js';\nconst hasBuffer = typeof Buffer !== 'undefined';\nexport class VSBuffer { static alloc(n) { return Buffer.allocUnsafe(n); } }\n";
        assert_eq!(
            esm_implicit_global_imports(path, reads),
            "import { Buffer as Buffer } from \"node:buffer\";\n"
        );
        let imports = "import { Buffer } from 'node:buffer';\nexport const b = Buffer.from('x');\n";
        assert_eq!(esm_implicit_global_imports(path, imports), "");
        let declares = "export class Buffer {}\nexport const b = new Buffer();\n";
        assert_eq!(esm_implicit_global_imports(path, declares), "");
        let member_only = "export const size = view.Buffer;\n";
        assert_eq!(esm_implicit_global_imports(path, member_only), "");
    }

    fn analyze(path: &str, source: &str) -> Option<CjsAnalysis> {
        classify(Path::new(path), source, false).unwrap()
    }

    #[test]
    fn detects_commonjs_and_leaves_esm_alone() {
        assert!(analyze("/a.js", "module.exports = 1;").is_some());
        assert!(analyze("/a.js", "#!/usr/bin/env node\nconst x = require('y');").is_some());
        assert!(analyze("/a.cjs", "var x = 1;").is_some());
        assert!(analyze("/a.js", "export const a = require('x');").is_none());
        assert!(analyze("/a.js", "import x from 'y'; module.exports = x;").is_none());
        assert!(analyze("/a.js", "var x = 1;").is_none());
        assert!(analyze("/a.ts", "module.exports = 1;").is_none());
        assert!(analyze("/a.mjs", "module.exports = 1;").is_none());
        assert!(
            classify(Path::new("/a.js"), "module.exports = 1;", true)
                .unwrap()
                .is_none()
        );
        // Property names are not references.
        assert!(analyze("/a.js", "var o = {}; o.module = o.exports; o.require();").is_none());
    }

    #[test]
    fn names_statically_known_worker_modules() {
        let analysis = analyze(
            "/app/main.js",
            r#"const path = require('path');
const { Worker } = require('worker_threads');
const file = path.join(__dirname, 'workers', 'a.js');
new Worker(file);
new Worker(path.resolve(__dirname, './b.js'));
new threads.Worker(__dirname + '/c.js');
new Worker(`${__dirname}/d.js`, { workerData: 1 });
new Worker(__filename);
utilityProcess.fork(path.join(__dirname, 'child.js'), []);
electron.utilityProcess.fork(file);
new Worker(someRuntimePath);
new Worker('./relative-to-cwd.js');
new Worker(path.join(__dirname, '/abs.js'));
"#,
        )
        .unwrap();
        assert_eq!(
            analysis.worker_entries,
            [
                "./b.js",
                "./c.js",
                "./d.js",
                "./main.js",
                "./child.js",
                "./workers/a.js"
            ]
        );
    }

    #[test]
    fn worker_boot_prelude_dispatches_to_linked_workers() {
        let entry = Path::new("/app/main.js");
        let prelude =
            worker_boot_prelude(entry, &[PathBuf::from("/app/w.js"), entry.to_path_buf()]);
        assert!(
            prelude.contains(r#"__w3cos_worker_boot.compiled(["/app/w.js", "/app/main.js"]);"#)
        );
        assert!(prelude.contains(r#"case "/app/w.js": require("w3cos-worker:/app/w.js"); break;"#));
        assert!(!prelude.contains(r#"case "/app/main.js""#));
        assert!(worker_boot_prelude(entry, &[]).is_empty());
    }

    #[test]
    fn collects_literal_requires_in_order() {
        let a = analyze(
            "/a.js",
            "const a = require('./a'); function f() { return require(`b`) + require('./a'); }\nif (x) require.resolve('c'); require(dynamic);",
        )
        .unwrap();
        assert_eq!(a.requires, ["./a", "b", "c"]);
    }

    #[test]
    fn lexes_export_names_and_reexports() {
        let a = analyze(
            "/a.js",
            r#""use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.b = exports.a = void 0;
exports["c-d"] = 1;
module.exports.e = function () {};
Object.defineProperty(exports, "f", { enumerable: true, get: function () { return g; } });
__exportStar(require("./star"), exports);
tslib_1.__exportStar(require("./star2"), exports);
"#,
        )
        .unwrap();
        assert_eq!(a.export_names, ["__esModule", "a", "b", "c-d", "e", "f"]);
        assert_eq!(a.reexports, ["./star", "./star2"]);
        let b = analyze(
            "/b.js",
            "module.exports = { x, y: 1, 'z': 2, m() {}, get g() { return 1; }, ...require('./more') };",
        )
        .unwrap();
        assert_eq!(b.export_names, ["g", "m", "x", "y", "z"]);
        assert_eq!(b.reexports, ["./more"]);
        let c = analyze("/c.js", "module.exports = require('./other');").unwrap();
        assert_eq!(c.reexports, ["./other"]);
    }

    #[test]
    fn facade_exports_only_valid_names() {
        let names = vec![
            "a".to_string(),
            "c-d".to_string(),
            "default".to_string(),
            "__esModule".to_string(),
            "a".to_string(),
        ];
        let (source, imports) = facade_source(Path::new("/p/x.js"), &names);
        assert!(source.contains("export const a = __w3cos_cjs_exports.a;"));
        assert_eq!(source.matches("export const").count(), 1);
        assert_eq!(imports, ["/p/x.js.w3cos-cjs.js"]);
    }

    #[test]
    fn free_buffer_reads_bind_the_builtin() {
        let free = analyze("/p/a.js", "module.exports = Buffer.from('x');").unwrap();
        assert_eq!(free.implicit_globals, ["Buffer"]);
        assert!(free.requires.contains(&"node:buffer".to_string()));
        for declared in [
            "const { Buffer } = require('buffer'); module.exports = Buffer;",
            "const Buffer = require('safe-buffer').Buffer; module.exports = Buffer;",
            "function Buffer() {} module.exports = Buffer;",
        ] {
            let analysis = analyze("/p/b.js", declared).unwrap();
            assert!(analysis.implicit_globals.is_empty(), "{declared}");
        }
        let (source, _) = record_source_with_globals(
            Path::new("/p/a.js"),
            "'use strict';\nmodule.exports = Buffer;",
            &[],
            Path::new("/p/__w3cos_cjs_runtime__.js"),
            &["Buffer".to_string()],
        );
        assert!(source.contains(
            "__dirname) {\n\"use strict\";\nvar Buffer = require(\"node:buffer\").Buffer;\n'use strict';"
        ));
    }

    #[test]
    fn record_links_each_target_kind() {
        let (source, imports) = record_source(
            Path::new("/p/x.js"),
            "#!/bin/node\nmodule.exports = require('./y');",
            &[
                ("./y".into(), RequireTarget::CommonJs("/p/y.js".into())),
                (
                    "esm".into(),
                    RequireTarget::Esm("/p/node_modules/esm/i.mjs".into()),
                ),
                ("./d.json".into(), RequireTarget::Json("/p/d.json".into())),
                ("fs".into(), RequireTarget::Builtin("fs".into())),
            ],
            Path::new("/p/__w3cos_cjs_runtime__.js"),
        );
        assert_eq!(
            imports,
            [
                "/p/__w3cos_cjs_runtime__.js",
                "/p/y.js.w3cos-cjs.js",
                "/p/node_modules/esm/i.mjs",
                "/p/d.json"
            ]
        );
        assert!(source.contains("case \"./y\": return __w3cos_cjs_dep0();"));
        assert!(source.contains("case \"esm\": return __w3cos_cjs_dep1;"));
        assert!(source.contains("case \"fs\": return __w3cos_cjs_builtin(\"fs\");"));
        assert!(source.contains("\nmodule.exports = require('./y');"));
        assert!(!source.contains("#!"));
    }
}
