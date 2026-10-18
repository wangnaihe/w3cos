//! SWC AST rules for `leak-lint` (see the module docs for the pattern table).

use super::{Edit, FileResult, Finding, Severity};
use std::collections::{HashMap, HashSet};
use swc_common::{FileName, SourceMap, Span, Spanned, sync::Lrc};
use swc_ecma_ast::*;
use swc_ecma_parser::{EsSyntax, Parser, StringInput, Syntax, TsSyntax};
use swc_ecma_visit::{Visit, VisitWith};

pub(super) const FIX_WEAK_BACK_POINTER: &str = "weak-back-pointer";
pub(super) const FIX_WEAK_MAP: &str = "weakmap";
pub(super) const FIX_REGISTER: &str = "register-disposable";

pub(super) fn is_typescript(file: &str) -> bool {
    let path = file.split(['?', '#']).next().unwrap_or(file);
    [".ts", ".tsx", ".mts", ".cts"]
        .iter()
        .any(|ext| path.ends_with(ext))
}

pub(super) fn parse(source: &str, file: &str) -> Option<(Program, u32)> {
    let source_map: Lrc<SourceMap> = Default::default();
    let fm = source_map.new_source_file(
        Lrc::new(FileName::Custom(file.to_string())),
        source.to_string(),
    );
    let syntax = if is_typescript(file) {
        Syntax::Typescript(TsSyntax {
            tsx: file.ends_with(".tsx"),
            decorators: true,
            ..Default::default()
        })
    } else {
        Syntax::Es(EsSyntax {
            jsx: true,
            decorators: true,
            ..Default::default()
        })
    };
    let mut parser = Parser::new(syntax, StringInput::from(&*fm), None);
    let program = parser.parse_program().ok()?;
    Some((program, fm.start_pos.0))
}

/// Maps SWC byte positions to 1-based line/column.
pub(super) struct Lines {
    base: u32,
    starts: Vec<usize>,
}

impl Lines {
    pub(super) fn new(source: &str, base: u32) -> Self {
        let mut starts = vec![0];
        starts.extend(source.match_indices('\n').map(|(i, _)| i + 1));
        Self { base, starts }
    }

    pub(super) fn offset(&self, pos: swc_common::BytePos) -> usize {
        pos.0.saturating_sub(self.base) as usize
    }

    pub(super) fn line_col(&self, offset: usize) -> (usize, usize) {
        let line = self.starts.partition_point(|&s| s <= offset);
        (line, offset - self.starts[line - 1] + 1)
    }

    fn line_start(&self, offset: usize) -> usize {
        let line = self.starts.partition_point(|&s| s <= offset);
        self.starts[line - 1]
    }
}

fn wtf8(value: &Str) -> String {
    value.value.to_string_lossy().into_owned()
}

fn unparen(mut e: &Expr) -> &Expr {
    loop {
        match e {
            Expr::Paren(p) => e = &p.expr,
            Expr::TsNonNull(n) => e = &n.expr,
            Expr::TsAs(a) => e = &a.expr,
            _ => return e,
        }
    }
}

fn member_name(prop: &MemberProp) -> Option<String> {
    match prop {
        MemberProp::Ident(i) => Some(i.sym.to_string()),
        MemberProp::PrivateName(p) => Some(format!("#{}", p.name)),
        MemberProp::Computed(c) => match unparen(&c.expr) {
            Expr::Lit(Lit::Str(s)) => Some(wtf8(s)),
            _ => None,
        },
    }
}

/// `this.x` -> "x"
fn this_member(e: &Expr) -> Option<String> {
    match unparen(e) {
        Expr::Member(m) if matches!(unparen(&m.obj), Expr::This(_)) => member_name(&m.prop),
        _ => None,
    }
}

/// Usage key for a receiver: `this.x` -> "this.x", `x` -> "x".
fn target_key(e: &Expr) -> Option<String> {
    match unparen(e) {
        Expr::Ident(i) => Some(i.sym.to_string()),
        e => this_member(e).map(|name| format!("this.{name}")),
    }
}

fn callee_name(call: &CallExpr) -> Option<String> {
    match &call.callee {
        Callee::Expr(e) => match unparen(e) {
            Expr::Member(m) => member_name(&m.prop),
            Expr::Ident(i) => Some(i.sym.to_string()),
            _ => None,
        },
        _ => None,
    }
}

fn is_bind_this(e: &Expr) -> bool {
    let Expr::Call(call) = unparen(e) else {
        return false;
    };
    callee_name(call).as_deref() == Some("bind")
        && call
            .args
            .first()
            .is_some_and(|a| matches!(unparen(&a.expr), Expr::This(_)))
}

fn is_nullish(e: &Expr) -> bool {
    match unparen(e) {
        Expr::Lit(Lit::Null(_)) => true,
        Expr::Ident(i) => i.sym == *"undefined",
        Expr::Unary(u) => u.op == UnaryOp::Void,
        _ => false,
    }
}

const BACK_NAMES: &[&str] = &[
    "parent",
    "owner",
    "container",
    "host",
    "outer",
    "back",
    "prev",
    "previous",
];

/// `parent`, `_owner`, `parentWidget`, ... (not DOM-owned `parentNode`/`ownerDocument`).
fn is_back_name(name: &str) -> bool {
    let bare = name.trim_start_matches('_');
    if matches!(bare, "parentNode" | "parentElement" | "ownerDocument") {
        return false;
    }
    BACK_NAMES.contains(&bare)
        || ["parent", "owner"].iter().any(|p| {
            bare.strip_prefix(p).is_some_and(|rest| {
                rest.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                    && OWNER_SUFFIXES.iter().any(|s| rest.ends_with(s))
            })
        })
}

/// `parentEditor`, `_parentContextKeyService`, ... (not `parentBounds`, `parentData`).
const OWNER_SUFFIXES: &[&str] = &[
    "Editor",
    "Widget",
    "View",
    "Session",
    "Thread",
    "Node",
    "Service",
    "Item",
    "Model",
    "Group",
    "Part",
    "Pane",
    "Controller",
    "Instance",
    "Tree",
    "Scope",
    "Context",
    "Window",
    "Host",
];

/// Back pointers to DOM nodes or plain values do not close an ownership
/// cycle the lint can reason about (the DOM tree is owned by the runtime).
fn is_non_owner_type(t: &TsType, local_types: &HashSet<String>) -> bool {
    match t {
        TsType::TsUnionOrIntersectionType(TsUnionOrIntersectionType::TsUnionType(u)) => {
            let rest: Vec<_> = u
                .types
                .iter()
                .filter(|t| {
                    !matches!(&***t, TsType::TsKeywordType(k) if matches!(k.kind, TsKeywordTypeKind::TsUndefinedKeyword | TsKeywordTypeKind::TsNullKeyword))
                })
                .collect();
            !rest.is_empty() && rest.iter().all(|t| is_non_owner_type(t, local_types))
        }
        TsType::TsTypeRef(r) => match &r.type_name {
            TsEntityName::Ident(i) => {
                let name = i.sym.as_ref();
                matches!(
                    name,
                    "Node"
                        | "Element"
                        | "Document"
                        | "Window"
                        | "URI"
                        | "Event"
                        | "ShadowRoot"
                        | "DocumentFragment"
                ) || name.starts_with("HTML")
                    || name.starts_with("SVG")
                    || name.ends_with("Element")
            }
            _ => false,
        },
        t => key_kind(t, local_types) == KeyKind::Primitive,
    }
}

fn is_disposable_ctor(name: &str) -> bool {
    matches!(
        name,
        "Emitter"
            | "PauseableEmitter"
            | "DebounceEmitter"
            | "MicrotaskEmitter"
            | "AsyncEmitter"
            | "EventMultiplexer"
            | "Relay"
            | "DisposableStore"
            | "MutableDisposable"
            | "DisposableMap"
            | "DisposableSet"
    ) || (name.ends_with("Disposable") && name != "Disposable")
}

fn is_event_registration(name: &str) -> bool {
    name.starts_with("onDid") || name.starts_with("onWill")
}

#[derive(PartialEq, Eq, Debug, Clone, Copy, Hash)]
enum KeyKind {
    Object,
    Primitive,
    Unknown,
}

fn key_kind(t: &TsType, local_types: &HashSet<String>) -> KeyKind {
    match t {
        TsType::TsKeywordType(k) => match k.kind {
            TsKeywordTypeKind::TsObjectKeyword => KeyKind::Object,
            TsKeywordTypeKind::TsAnyKeyword | TsKeywordTypeKind::TsUnknownKeyword => {
                KeyKind::Unknown
            }
            _ => KeyKind::Primitive,
        },
        TsType::TsTypeLit(_) | TsType::TsFnOrConstructorType(_) => KeyKind::Object,
        TsType::TsLitType(_) => KeyKind::Primitive,
        TsType::TsParenthesizedType(p) => key_kind(&p.type_ann, local_types),
        TsType::TsUnionOrIntersectionType(TsUnionOrIntersectionType::TsUnionType(u)) => {
            let kinds: HashSet<_> = u.types.iter().map(|t| key_kind(t, local_types)).collect();
            if kinds.len() == 1 {
                *kinds.iter().next().unwrap()
            } else {
                KeyKind::Unknown
            }
        }
        TsType::TsTypeRef(r) => match &r.type_name {
            TsEntityName::Ident(i) => {
                let name = i.sym.as_ref();
                let interface_like = name.len() > 1
                    && name.starts_with('I')
                    && name[1..]
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_uppercase());
                let dom = matches!(
                    name,
                    "HTMLElement"
                        | "Element"
                        | "Node"
                        | "Window"
                        | "Document"
                        | "Function"
                        | "Object"
                        | "SVGElement"
                        | "Event"
                ) || (name.starts_with("HTML") && name.ends_with("Element"));
                if local_types.contains(name) || interface_like || dom {
                    KeyKind::Object
                } else {
                    KeyKind::Unknown
                }
            }
            _ => KeyKind::Unknown,
        },
        _ => KeyKind::Unknown,
    }
}

/// `Map<K, V>` type reference -> (span of `Map`, K)
fn map_type(t: &TsType) -> Option<(Span, &TsType)> {
    let TsType::TsTypeRef(r) = t else { return None };
    let TsEntityName::Ident(i) = &r.type_name else {
        return None;
    };
    if i.sym != *"Map" {
        return None;
    }
    let key = r.type_params.as_ref()?.params.first()?;
    Some((i.span, key))
}

/// `new Map<K, V>(...)` -> (span of `Map`, K if given)
fn new_map(e: &Expr) -> Option<(Span, Option<&TsType>)> {
    let Expr::New(n) = unparen(e) else {
        return None;
    };
    let Expr::Ident(i) = unparen(&n.callee) else {
        return None;
    };
    if i.sym != *"Map" {
        return None;
    }
    let key = n
        .type_args
        .as_ref()
        .and_then(|a| a.params.first())
        .map(|t| &**t);
    Some((i.span, key))
}

fn new_ctor_name(e: &Expr) -> Option<String> {
    let Expr::New(n) = unparen(e) else {
        return None;
    };
    match unparen(&n.callee) {
        Expr::Ident(i) => Some(i.sym.to_string()),
        _ => None,
    }
}

// ── prepass: file facts ──────────────────────────────────────────────────

#[derive(Default, Debug)]
struct Usage {
    methods: HashSet<String>,
    other: bool,
}

#[derive(Default)]
struct Facts {
    removed_events: HashSet<String>,
    dynamic_remove: bool,
    usage: HashMap<String, Usage>,
    external_back_writes: HashSet<String>,
    local_types: HashSet<String>,
    registered: HashSet<String>,
    /// Setters that store into a `WeakRef` (e.g. produced by `--fix weak-back-pointer`).
    weak_setters: HashSet<String>,
    /// Names re-exported via `export { a }`.
    exported: HashSet<String>,
}

struct IdentFinder(&'static str, bool);

impl Visit for IdentFinder {
    fn visit_ident(&mut self, n: &Ident) {
        if n.sym == *self.0 {
            self.1 = true;
        }
    }
}

impl Visit for Facts {
    fn visit_class_decl(&mut self, n: &ClassDecl) {
        self.local_types.insert(n.ident.sym.to_string());
        n.visit_children_with(self);
    }

    fn visit_class_method(&mut self, n: &ClassMethod) {
        if n.kind == MethodKind::Setter
            && let Some(name) = prop_name(&n.key)
        {
            let mut finder = IdentFinder("WeakRef", false);
            n.function.visit_with(&mut finder);
            if finder.1 {
                self.weak_setters.insert(name);
            }
        }
        n.visit_children_with(self);
    }

    fn visit_export_named_specifier(&mut self, n: &ExportNamedSpecifier) {
        if let ModuleExportName::Ident(i) = &n.orig {
            self.exported.insert(i.sym.to_string());
        }
    }

    fn visit_ts_interface_decl(&mut self, n: &TsInterfaceDecl) {
        self.local_types.insert(n.id.sym.to_string());
        n.visit_children_with(self);
    }

    fn visit_call_expr(&mut self, n: &CallExpr) {
        let name = callee_name(n);
        if name.as_deref() == Some("removeEventListener") {
            match n.args.first().map(|a| unparen(&a.expr)) {
                Some(Expr::Lit(Lit::Str(s))) => {
                    self.removed_events.insert(wtf8(s));
                }
                _ => self.dynamic_remove = true,
            }
        }
        if matches!(
            name.as_deref(),
            Some("_register" | "register" | "add" | "push" | "toDisposable")
        ) {
            for arg in &n.args {
                if let Some(member) = this_member(&arg.expr) {
                    self.registered.insert(member);
                }
            }
        }
        if let Callee::Expr(callee) = &n.callee
            && let Expr::Member(m) = unparen(callee)
            && let (Some(key), Some(method)) = (target_key(&m.obj), member_name(&m.prop))
        {
            self.usage.entry(key).or_default().methods.insert(method);
            n.args.visit_with(self);
            return;
        }
        n.visit_children_with(self);
    }

    fn visit_member_expr(&mut self, n: &MemberExpr) {
        if let Some(name) = this_member(&Expr::Member(n.clone())) {
            self.usage.entry(format!("this.{name}")).or_default().other = true;
        }
        n.visit_children_with(self);
    }

    fn visit_expr(&mut self, n: &Expr) {
        if let Expr::Ident(i) = n {
            self.usage.entry(i.sym.to_string()).or_default().other = true;
        }
        n.visit_children_with(self);
    }

    fn visit_assign_expr(&mut self, n: &AssignExpr) {
        if let AssignTarget::Simple(SimpleAssignTarget::Member(m)) = &n.left {
            let this_obj = matches!(unparen(&m.obj), Expr::This(_));
            if this_obj && n.op == AssignOp::Assign && new_map(&n.right).is_some() {
                n.right.visit_with(self);
                return;
            }
            if !this_obj
                && let Some(name) = member_name(&m.prop)
                && is_back_name(&name)
            {
                self.external_back_writes.insert(name);
            }
        }
        n.visit_children_with(self);
    }
}

// ── helpers over subtrees ────────────────────────────────────────────────

/// Does `node` use the enclosing `this` (not descending into non-arrow functions or classes)?
#[derive(Default)]
struct ThisFinder(bool);

impl Visit for ThisFinder {
    fn visit_this_expr(&mut self, _: &ThisExpr) {
        self.0 = true;
    }
    fn visit_function(&mut self, _: &Function) {}
    fn visit_class(&mut self, _: &Class) {}
}

fn uses_this<N: VisitWith<ThisFinder>>(node: &N) -> bool {
    let mut finder = ThisFinder::default();
    node.visit_with(&mut finder);
    finder.0
}

/// Listener / callback argument that keeps `this` alive.
fn handler_captures_this(e: &Expr) -> bool {
    match unparen(e) {
        Expr::Arrow(a) => uses_this(&*a.body),
        e => is_bind_this(e),
    }
}

/// `this.x` names referenced and `this.x = <non-null>` writes in a class body
/// (nested classes excluded).
#[derive(Default)]
struct ClassScan {
    refs: HashSet<String>,
    writes: HashSet<String>,
}

impl Visit for ClassScan {
    fn visit_class(&mut self, _: &Class) {}
    fn visit_member_expr(&mut self, n: &MemberExpr) {
        if let Some(name) = this_member(&Expr::Member(n.clone())) {
            self.refs.insert(name);
        }
        n.visit_children_with(self);
    }
    fn visit_assign_expr(&mut self, n: &AssignExpr) {
        if let AssignTarget::Simple(SimpleAssignTarget::Member(m)) = &n.left
            && matches!(unparen(&m.obj), Expr::This(_))
            && let Some(name) = member_name(&m.prop)
            && !is_nullish(&n.right)
        {
            self.writes.insert(name);
        }
        n.visit_children_with(self);
    }
}

struct ClassInfo {
    register_available: bool,
    declares_dispose: bool,
    dispose_refs: HashSet<String>,
    writes: HashSet<String>,
    member_names: HashSet<String>,
    field_names: HashSet<String>,
    private_fields: HashSet<String>,
    /// Map-typed field annotation: name -> span of `Map` in the annotation
    map_fields: HashMap<String, (Span, KeyKind)>,
    reported_back_writes: HashSet<String>,
}

fn prop_name(key: &PropName) -> Option<String> {
    match key {
        PropName::Ident(i) => Some(i.sym.to_string()),
        PropName::Str(s) => Some(wtf8(s)),
        _ => None,
    }
}

// ── lint pass ────────────────────────────────────────────────────────────

struct Linter<'a> {
    source: &'a str,
    file: &'a str,
    ts: bool,
    lines: Lines,
    facts: Facts,
    classes: Vec<ClassInfo>,
    /// Is `this` the instance of `classes.last()`?
    instance: Vec<bool>,
    /// Inside `export const ...`
    exporting: bool,
    findings: Vec<Finding>,
    edits: Vec<Edit>,
}

impl Linter<'_> {
    fn in_instance(&self) -> bool {
        self.instance.last().copied().unwrap_or(false) && !self.classes.is_empty()
    }

    fn class(&self) -> Option<&ClassInfo> {
        if self.in_instance() {
            self.classes.last()
        } else {
            None
        }
    }

    fn range(&self, span: Span) -> (usize, usize) {
        (self.lines.offset(span.lo), self.lines.offset(span.hi))
    }

    fn report(
        &mut self,
        span: Span,
        pattern: &'static str,
        severity: Severity,
        message: String,
        suggestion: impl Into<String>,
        fix: Option<&'static str>,
    ) {
        let (line, column) = self.lines.line_col(self.lines.offset(span.lo));
        self.findings.push(Finding {
            file: self.file.to_string(),
            line,
            column,
            pattern,
            severity,
            message,
            suggestion: suggestion.into(),
            fix,
        });
    }

    fn snippet(&self, span: Span) -> String {
        let (start, end) = self.range(span);
        let text = self.source.get(start..end).unwrap_or("");
        let one_line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if one_line.chars().count() > 70 {
            format!("{}…", one_line.chars().take(69).collect::<String>())
        } else {
            one_line
        }
    }

    fn wrap_register(&mut self, span: Span) {
        let (start, end) = self.range(span);
        self.edits.push(Edit {
            start,
            end: start,
            text: "this._register(".into(),
        });
        self.edits.push(Edit {
            start: end,
            end,
            text: ")".into(),
        });
    }

    fn scan_class(&self, class: &Class) -> ClassInfo {
        let mut scan = ClassScan::default();
        class.body.visit_with(&mut scan);
        let mut dispose_refs = HashSet::new();
        let mut declares_dispose = false;
        let mut member_names = HashSet::new();
        let mut field_names = HashSet::new();
        let mut private_fields = HashSet::new();
        let mut map_fields = HashMap::new();
        let mut has_register = false;
        for member in &class.body {
            match member {
                ClassMember::Method(m) => {
                    if let Some(name) = prop_name(&m.key) {
                        if name == "dispose" {
                            declares_dispose = true;
                            let mut refs = ClassScan::default();
                            m.function.visit_with(&mut refs);
                            dispose_refs = refs.refs;
                        }
                        if name == "_register" {
                            has_register = true;
                        }
                        if matches!(m.kind, MethodKind::Getter | MethodKind::Setter) {
                            field_names.insert(name.clone());
                        }
                        member_names.insert(name);
                    }
                }
                ClassMember::ClassProp(p) => {
                    if let Some(name) = prop_name(&p.key) {
                        if let Some((span, key)) =
                            p.type_ann.as_ref().and_then(|t| map_type(&t.type_ann))
                        {
                            map_fields.insert(
                                name.clone(),
                                (span, key_kind(key, &self.facts.local_types)),
                            );
                        }
                        if p.accessibility == Some(Accessibility::Private) {
                            private_fields.insert(name.clone());
                        }
                        member_names.insert(name.clone());
                        field_names.insert(name);
                    }
                }
                ClassMember::Constructor(c) => {
                    for param in &c.params {
                        if let ParamOrTsParamProp::TsParamProp(p) = param
                            && let TsParamPropParam::Ident(i) = &p.param
                        {
                            member_names.insert(i.id.sym.to_string());
                            field_names.insert(i.id.sym.to_string());
                        }
                    }
                }
                _ => {}
            }
        }
        let extends_disposable = class
            .super_class
            .as_deref()
            .is_some_and(|s| matches!(unparen(s), Expr::Ident(i) if i.sym == *"Disposable"));
        ClassInfo {
            register_available: extends_disposable || has_register,
            declares_dispose,
            dispose_refs,
            writes: scan.writes,
            member_names,
            field_names,
            private_fields,
            map_fields,
            reported_back_writes: HashSet::new(),
        }
    }

    // ── rule: object-keyed Map ──
    #[allow(clippy::too_many_arguments)]
    fn check_map(
        &mut self,
        span: Span,
        usage_key: &str,
        label: &str,
        ctor_span: Span,
        key: KeyKind,
        annotation: Option<Span>,
        private: bool,
    ) {
        if key != KeyKind::Object {
            return;
        }
        let usage = self.facts.usage.get(usage_key);
        let methods = usage.map(|u| u.methods.clone()).unwrap_or_default();
        let other = usage.is_some_and(|u| u.other);
        let cache_only = !other
            && methods
                .iter()
                .all(|m| matches!(m.as_str(), "get" | "set" | "has" | "delete"));
        if cache_only && !private {
            self.report(
                span,
                "object-keyed-map",
                Severity::Warning,
                format!("`{label}` is a Map keyed by objects and only used via get/set/has/delete in this file: every key (and value) stays alive until removed"),
                "use a WeakMap if no other module iterates it (not auto-fixed: the map is visible outside this file)",
                None,
            );
        } else if cache_only {
            let (start, end) = self.range(ctor_span);
            self.edits.push(Edit {
                start,
                end,
                text: "WeakMap".into(),
            });
            if let Some(annotation) = annotation {
                let (start, end) = self.range(annotation);
                self.edits.push(Edit {
                    start,
                    end,
                    text: "WeakMap".into(),
                });
            }
            self.report(
                span,
                "object-keyed-map",
                Severity::Warning,
                format!("`{label}` is a Map keyed by objects and only used via get/set/has/delete: every key (and value) stays alive until removed"),
                "use a WeakMap so entries die with their key objects",
                Some(FIX_WEAK_MAP),
            );
        } else if !methods.contains("delete") && !methods.contains("clear") {
            self.report(
                span,
                "object-keyed-map",
                Severity::Warning,
                format!("`{label}` is a Map keyed by objects with no delete()/clear(): entries are never evicted"),
                "evict entries when their key is disposed, or use a WeakMap if iteration isn't needed",
                None,
            );
        }
    }

    // ── rule: disposables ──
    fn check_disposable(&mut self, name: &str, value: &Expr) {
        let Some(ctor) = new_ctor_name(value) else {
            return;
        };
        if !is_disposable_ctor(&ctor) {
            return;
        }
        let Some(class) = self.class() else { return };
        if class.dispose_refs.contains(name) || self.facts.registered.contains(name) {
            return;
        }
        if self
            .facts
            .usage
            .get(&format!("this.{name}"))
            .is_some_and(|u| u.methods.contains("dispose"))
        {
            return;
        }
        let fixable = class.register_available;
        if !fixable && !class.declares_dispose {
            // Not a disposable class: nothing to tie the lifetime to.
            return;
        }
        if fixable {
            self.wrap_register(value.span());
        }
        self.report(
            value.span(),
            "disposable-not-registered",
            Severity::Warning,
            format!("`this.{name} = new {ctor}(...)` is never registered or disposed: its listeners keep their targets alive"),
            if fixable { "wrap it in this._register(...)" } else { "dispose it in dispose()" },
            fixable.then_some(FIX_REGISTER),
        );
    }

    // ── rule: back-pointer field ──
    fn check_back_field(&mut self, p: &ClassProp) {
        let Some(name) = prop_name(&p.key) else {
            return;
        };
        if p.is_static || !is_back_name(&name) || name.ends_with("Ref") {
            return;
        }
        if p.type_ann.as_ref().is_some_and(|t| {
            let mut finder = IdentFinder("WeakRef", false);
            t.visit_with(&mut finder);
            finder.1
        }) {
            return;
        }
        let Some(class) = self.classes.last() else {
            return;
        };
        let written =
            class.writes.contains(&name) || self.facts.external_back_writes.contains(&name);
        if !written {
            return;
        }
        let type_text = p.type_ann.as_ref().map(|t| {
            let (start, end) = self.range(t.type_ann.span());
            self.source[start..end].to_string()
        });
        if p.type_ann
            .as_ref()
            .is_some_and(|t| is_non_owner_type(&t.type_ann, &self.facts.local_types))
        {
            return;
        }
        let ref_name = if name.starts_with('_') {
            format!("{name}Ref")
        } else {
            format!("_{name}Ref")
        };
        let fixable = self.ts
            && type_text.is_some()
            && p.value.as_deref().is_none_or(is_nullish)
            && !p.readonly
            && !p.declare
            && !p.is_abstract
            && !p.is_override
            && p.decorators.is_empty()
            && matches!(p.key, PropName::Ident(_))
            && !class.member_names.contains(&ref_name);
        if fixable {
            let mut ty = type_text.unwrap();
            if p.is_optional {
                ty = format!("{ty} | undefined");
            }
            let acc = match p.accessibility {
                Some(Accessibility::Private) => "private ",
                Some(Accessibility::Protected) => "protected ",
                Some(Accessibility::Public) => "public ",
                None => "",
            };
            let (start, end) = self.range(p.span);
            let line_start = self.lines.line_start(start);
            let indent: String = self.source[line_start..start]
                .chars()
                .take_while(|c| c.is_whitespace())
                .collect();
            let text = format!(
                "private {ref_name}: WeakRef<object> | null | undefined; // w3cos leak-lint: weak back pointer\n\
                 {indent}{acc}get {name}(): {ty} {{ const ref = this.{ref_name}; return (ref instanceof WeakRef ? ref.deref() : ref) as {ty}; }}\n\
                 {indent}{acc}set {name}(value: {ty}) {{ this.{ref_name} = value === null || value === undefined ? value : new WeakRef(value as object); }}"
            );
            // Replace the whole member including a trailing `;`.
            let end = if self.source[end..].starts_with(';') {
                end + 1
            } else {
                end
            };
            self.edits.push(Edit { start, end, text });
        }
        self.report(
            p.span,
            "back-pointer",
            Severity::Warning,
            format!("`{name}` is a strong back pointer: the object it points to usually owns this one, closing a cycle"),
            "store it as a WeakRef and read it through deref() (the owner must keep its own strong reference)",
            fixable.then_some(FIX_WEAK_BACK_POINTER),
        );
    }

    fn visit_class_members(&mut self, class: &Class) {
        for member in &class.body {
            match member {
                ClassMember::Constructor(c) => {
                    for param in &c.params {
                        if let ParamOrTsParamProp::TsParamProp(p) = param
                            && let TsParamPropParam::Ident(i) = &p.param
                            && is_back_name(&i.id.sym)
                            && i.type_ann.as_ref().is_none_or(|t| {
                                !is_non_owner_type(&t.type_ann, &self.facts.local_types)
                            })
                        {
                            self.report(
                                p.span,
                                "back-pointer",
                                Severity::Warning,
                                format!("constructor parameter property `{}` is a strong back pointer to the owner", i.id.sym),
                                "take a WeakRef instead (or a plain parameter + WeakRef field with a deref() getter)",
                                None,
                            );
                        }
                    }
                    self.instance.push(true);
                    c.params.visit_with(self);
                    c.body.visit_with(self);
                    self.instance.pop();
                }
                ClassMember::Method(m) => {
                    self.instance.push(!m.is_static);
                    m.function.params.visit_with(self);
                    m.function.body.visit_with(self);
                    self.instance.pop();
                }
                ClassMember::PrivateMethod(m) => {
                    self.instance.push(!m.is_static);
                    m.function.params.visit_with(self);
                    m.function.body.visit_with(self);
                    self.instance.pop();
                }
                ClassMember::ClassProp(p) => {
                    self.instance.push(!p.is_static);
                    self.check_back_field(p);
                    if let (Some(name), Some(value)) = (prop_name(&p.key), p.value.as_deref())
                        && !p.is_static
                    {
                        self.check_field_value(p.span, &name, value, p.type_ann.as_deref());
                    }
                    p.value.visit_with(self);
                    self.instance.pop();
                }
                ClassMember::PrivateProp(p) => {
                    self.instance.push(!p.is_static);
                    if let Some(value) = p.value.as_deref()
                        && !p.is_static
                    {
                        let name = format!("#{}", p.key.name);
                        self.check_this_capture(p.span, &name, value);
                    }
                    p.value.visit_with(self);
                    self.instance.pop();
                }
                ClassMember::StaticBlock(b) => {
                    self.instance.push(false);
                    b.visit_with(self);
                    self.instance.pop();
                }
                other => other.visit_children_with(self),
            }
        }
    }

    /// Instance field initializer / `this.name = value` checks.
    fn check_field_value(
        &mut self,
        span: Span,
        name: &str,
        value: &Expr,
        annotation: Option<&TsTypeAnn>,
    ) {
        self.check_this_capture(span, name, value);
        self.check_disposable(name, value);
        if let Some((ctor, key)) = new_map(value) {
            let from_annotation = annotation.and_then(|t| map_type(&t.type_ann));
            let class_field = self
                .classes
                .last()
                .and_then(|c| c.map_fields.get(name))
                .copied();
            let kind = key
                .map(|k| key_kind(k, &self.facts.local_types))
                .or_else(|| from_annotation.map(|(_, k)| key_kind(k, &self.facts.local_types)))
                .or(class_field.map(|(_, k)| k))
                .unwrap_or(KeyKind::Unknown);
            let annotation_span = from_annotation
                .map(|(s, _)| s)
                .or(class_field.map(|(s, _)| s));
            self.check_map(
                span,
                &format!("this.{name}"),
                &format!("this.{name}"),
                ctor,
                kind,
                annotation_span,
                self.classes
                    .last()
                    .is_some_and(|c| c.private_fields.contains(name)),
            );
        }
    }

    fn check_this_capture(&mut self, span: Span, name: &str, value: &Expr) {
        match unparen(value) {
            Expr::Arrow(a) if uses_this(&*a.body) => self.report(
                span,
                "this-capture-stored-on-this",
                Severity::Info,
                format!("`this.{name}` holds an arrow function that captures `this` (object -> closure -> object)"),
                "AOT rewrites this unique self-install to a WeakRef capture (unique_back_edges); prefer a method if the closure is passed elsewhere",
                None,
            ),
            e if is_bind_this(e) => self.report(
                span,
                "this-capture-stored-on-this",
                Severity::Warning,
                format!("`this.{name} = ….bind(this)` stores a bound function that holds `this` strongly (object -> bound fn -> object); AOT does not weaken bind()"),
                "call the method through an arrow at the use site, or keep the bound function off the instance",
                None,
            ),
            _ => {}
        }
    }
}

impl Visit for Linter<'_> {
    fn visit_class(&mut self, n: &Class) {
        n.decorators.visit_with(self);
        n.super_class.visit_with(self);
        let info = self.scan_class(n);
        self.classes.push(info);
        self.visit_class_members(n);
        self.classes.pop();
    }

    fn visit_export_decl(&mut self, n: &ExportDecl) {
        self.exporting = true;
        n.visit_children_with(self);
        self.exporting = false;
    }

    fn visit_function(&mut self, n: &Function) {
        self.instance.push(false);
        n.visit_children_with(self);
        self.instance.pop();
    }

    fn visit_var_declarator(&mut self, n: &VarDeclarator) {
        if let (Pat::Ident(binding), Some(init)) = (&n.name, n.init.as_deref())
            && let Some((ctor, key)) = new_map(init)
        {
            let annotation = binding
                .type_ann
                .as_ref()
                .and_then(|t| map_type(&t.type_ann));
            let kind = key
                .or(annotation.map(|(_, k)| k))
                .map(|k| key_kind(k, &self.facts.local_types))
                .unwrap_or(KeyKind::Unknown);
            let name = binding.id.sym.to_string();
            let local = !self.exporting && !self.facts.exported.contains(&name);
            self.check_map(
                n.span,
                &name,
                &name,
                ctor,
                kind,
                annotation.map(|(s, _)| s),
                local,
            );
        }
        n.visit_children_with(self);
    }

    fn visit_assign_expr(&mut self, n: &AssignExpr) {
        if n.op == AssignOp::Assign
            && let AssignTarget::Simple(SimpleAssignTarget::Member(m)) = &n.left
            && let Some(name) = member_name(&m.prop)
        {
            let this_obj = matches!(unparen(&m.obj), Expr::This(_));
            if this_obj && self.in_instance() {
                self.check_field_value(n.span, &name, &n.right, None);
                let undeclared = self.class().is_some_and(|c| !c.field_names.contains(&name));
                if is_back_name(&name)
                    && undeclared
                    && !is_nullish(&n.right)
                    && !matches!(
                        unparen(&n.right),
                        Expr::This(_)
                            | Expr::Lit(_)
                            | Expr::Arrow(_)
                            | Expr::Fn(_)
                            | Expr::Object(_)
                            | Expr::Array(_)
                            | Expr::New(_)
                    )
                    && self
                        .classes
                        .last_mut()
                        .is_some_and(|c| c.reported_back_writes.insert(name.clone()))
                {
                    self.report(
                        n.span,
                        "back-pointer",
                        Severity::Warning,
                        format!("`this.{name} = …` stores a strong back pointer (the target usually owns this object)"),
                        "store a WeakRef and read it through deref()",
                        None,
                    );
                }
            } else if !this_obj
                && is_back_name(&name)
                && matches!(unparen(&n.right), Expr::This(_))
                && !self.facts.weak_setters.contains(&name)
            {
                let target = self.snippet(m.obj.span());
                self.report(
                    n.span,
                    "back-pointer",
                    Severity::Warning,
                    format!("`{target}.{name} = this`: the child points back to its owner strongly (parent <-> child cycle)"),
                    "make the child's back pointer a WeakRef (see --fix weak-back-pointer on the child's class)",
                    None,
                );
            }
        }
        n.visit_children_with(self);
    }

    fn visit_expr_stmt(&mut self, n: &ExprStmt) {
        // Discarded IDisposable from an event registration: `x.onDidY(() => this.z())`.
        if let Expr::Call(call) = unparen(&n.expr)
            && let Callee::Expr(callee) = &call.callee
            && let Expr::Member(m) = unparen(callee)
            && let Some(name) = member_name(&m.prop)
            && let Some(handler) = call.args.first()
        {
            let via_event = name == "event"
                && matches!(unparen(&m.obj), Expr::Member(inner) if member_name(&inner.prop).is_some_and(|p| is_event_registration(p.trim_start_matches('_'))));
            let handler_like = matches!(unparen(&handler.expr), Expr::Arrow(_) | Expr::Fn(_))
                || is_bind_this(&handler.expr);
            if (is_event_registration(&name) || via_event)
                && handler_like
                && handler.spread.is_none()
                // `event(listener, thisArgs, disposables)` registers through its third argument.
                && call.args.len() < 3
            {
                let captures = handler_captures_this(&handler.expr);
                let fixable = captures && self.class().is_some_and(|c| c.register_available);
                if fixable {
                    self.wrap_register(call.span);
                }
                let call_text = self.snippet(callee.span());
                self.report(
                    call.span,
                    "listener-without-dispose",
                    if captures {
                        Severity::Warning
                    } else {
                        Severity::Info
                    },
                    format!(
                        "`{call_text}(…)` returns an IDisposable that is discarded{}",
                        if captures {
                            "; the listener captures `this`, so the emitter keeps this object alive"
                        } else {
                            ""
                        }
                    ),
                    if fixable {
                        "register it: this._register(…)"
                    } else {
                        "keep the IDisposable and dispose it (DisposableStore / this._register)"
                    },
                    fixable.then_some(FIX_REGISTER),
                );
            }
        }
        n.visit_children_with(self);
    }

    fn visit_call_expr(&mut self, n: &CallExpr) {
        if callee_name(n).as_deref() == Some("addEventListener") && n.args.len() >= 2 {
            let options_release = n.args.get(2).is_some_and(|a| match unparen(&a.expr) {
                Expr::Object(o) => o.props.iter().any(|p| match p {
                    PropOrSpread::Prop(p) => match &**p {
                        Prop::KeyValue(kv) => {
                            matches!(prop_name(&kv.key).as_deref(), Some("once" | "signal"))
                        }
                        Prop::Shorthand(i) => i.sym == *"signal",
                        _ => false,
                    },
                    _ => false,
                }),
                _ => false,
            });
            let event = match unparen(&n.args[0].expr) {
                Expr::Lit(Lit::Str(s)) => Some(wtf8(s)),
                _ => None,
            };
            let removed = match &event {
                Some(e) => self.facts.removed_events.contains(e) || self.facts.dynamic_remove,
                None => !self.facts.removed_events.is_empty() || self.facts.dynamic_remove,
            };
            if !options_release && !removed {
                let captures = handler_captures_this(&n.args[1].expr);
                let label = event
                    .map(|e| format!("'{e}'"))
                    .unwrap_or_else(|| "a dynamic event".into());
                self.report(
                    n.span,
                    "listener-without-dispose",
                    if captures { Severity::Warning } else { Severity::Info },
                    format!(
                        "addEventListener({label}, …) has no matching removeEventListener in this file{}",
                        if captures { "; the handler captures `this`" } else { "" }
                    ),
                    "use addDisposableListener(...) and register the IDisposable, pass { signal }, or remove it in dispose()",
                    None,
                );
            }
        }
        n.visit_children_with(self);
    }
}

pub(super) fn lint(source: &str, file: &str) -> FileResult {
    let Some((program, base)) = parse(source, file) else {
        return FileResult::default();
    };
    let mut facts = Facts::default();
    program.visit_with(&mut facts);
    let mut linter = Linter {
        source,
        file,
        ts: is_typescript(file),
        lines: Lines::new(source, base),
        facts,
        classes: Vec::new(),
        instance: Vec::new(),
        exporting: false,
        findings: Vec::new(),
        edits: Vec::new(),
    };
    program.visit_with(&mut linter);
    FileResult {
        findings: linter.findings,
        edits: linter.edits,
        parsed: true,
        ir_lowered: false,
    }
}
