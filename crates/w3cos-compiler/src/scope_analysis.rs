//! Scope tree and capture analysis for closures.
//!
//! Walks the SWC AST to determine which variables are captured by closures
//! and whether they're mutated, so codegen can wrap them in `Rc<RefCell<T>>`.

use std::collections::{HashMap, HashSet};
use swc_ecma_ast::*;

pub type ScopeId = usize;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScopeKind {
    Module,
    Function,
    Block,
    Closure,
}

#[derive(Debug)]
struct Scope {
    #[allow(dead_code)]
    id: ScopeId,
    parent: Option<ScopeId>,
    kind: ScopeKind,
    declarations: HashSet<String>,
}

#[derive(Debug)]
pub struct CaptureDetail {
    pub captured_by: Vec<ScopeId>,
    pub is_mutated_in_closure: bool,
}

/// Result of capture analysis — which variables need Rc<RefCell<T>> wrapping.
#[derive(Debug, Default)]
pub struct CaptureInfo {
    pub captures: HashMap<String, CaptureDetail>,
    /// Every name that appears as a plain-identifier assignment/update target
    /// anywhere in the analyzed body (any scope, closure or not). Used to
    /// decide Rc<RefCell> boxing: a captured binding that is reassigned
    /// anywhere must be shared, not clone-captured.
    pub assigned_names: HashSet<String>,
}

impl CaptureInfo {
    pub fn is_captured(&self, name: &str) -> bool {
        self.captures.contains_key(name)
    }

    #[allow(dead_code)]
    pub fn is_mutated_in_closure(&self, name: &str) -> bool {
        self.captures
            .get(name)
            .map(|d| d.is_mutated_in_closure)
            .unwrap_or(false)
    }

    /// Names that must be boxed as `Rc<RefCell<Value>>`: captured by a
    /// closure AND (mutated inside a closure OR assigned anywhere). Boxing is
    /// what lets `Fn` closures mutate captures and preserves JS live-binding
    /// semantics for writes that happen after closure creation.
    pub fn boxed_names(&self) -> HashSet<String> {
        self.captures
            .iter()
            .filter(|(name, detail)| {
                detail.is_mutated_in_closure || self.assigned_names.contains(*name)
            })
            .map(|(name, _)| name.clone())
            .collect()
    }
}

struct ScopeBuilder {
    scopes: Vec<Scope>,
    current: ScopeId,
    /// When true, `Function` scopes count as closure boundaries for capture
    /// detection. Used by `analyze_fn_body`: a nested fn *declaration*
    /// inside a fn body is lowered as a capturing closure value, so names
    /// from the outer body referenced inside it are captures. Module-level
    /// analysis keeps this off (fn declarations there do not capture).
    fn_scopes_capture: bool,
}

impl ScopeBuilder {
    fn new() -> Self {
        let root = Scope {
            id: 0,
            parent: None,
            kind: ScopeKind::Module,
            declarations: HashSet::new(),
        };
        Self {
            scopes: vec![root],
            current: 0,
            fn_scopes_capture: false,
        }
    }

    fn push_scope(&mut self, kind: ScopeKind) -> ScopeId {
        let id = self.scopes.len();
        self.scopes.push(Scope {
            id,
            parent: Some(self.current),
            kind,
            declarations: HashSet::new(),
        });
        self.current = id;
        id
    }

    fn pop_scope(&mut self) {
        if let Some(parent) = self.scopes[self.current].parent {
            self.current = parent;
        }
    }

    fn declare(&mut self, name: &str) {
        self.scopes[self.current]
            .declarations
            .insert(name.to_string());
    }

    /// Find which scope a variable is declared in, walking up the scope chain.
    fn find_declaration(&self, name: &str) -> Option<ScopeId> {
        let mut scope_id = self.current;
        loop {
            if self.scopes[scope_id].declarations.contains(name) {
                return Some(scope_id);
            }
            match self.scopes[scope_id].parent {
                Some(parent) => scope_id = parent,
                None => return None,
            }
        }
    }

    /// Check if the current scope is inside a closure.
    fn is_inside_closure(&self) -> bool {
        let mut scope_id = self.current;
        loop {
            let kind = self.scopes[scope_id].kind;
            if kind == ScopeKind::Closure || (self.fn_scopes_capture && kind == ScopeKind::Function)
            {
                return true;
            }
            match self.scopes[scope_id].parent {
                Some(parent) => scope_id = parent,
                None => return false,
            }
        }
    }

    /// Check if `decl_scope` is an outer scope relative to the current closure.
    fn is_captured_from_outer(&self, decl_scope: ScopeId) -> bool {
        let mut scope_id = self.current;
        loop {
            if scope_id == decl_scope {
                return false;
            }
            if self.scopes[scope_id].kind == ScopeKind::Closure
                || self.scopes[scope_id].kind == ScopeKind::Function
            {
                return true;
            }
            match self.scopes[scope_id].parent {
                Some(parent) => scope_id = parent,
                None => return false,
            }
        }
    }

    fn current_closure_id(&self) -> Option<ScopeId> {
        let mut scope_id = self.current;
        loop {
            if self.scopes[scope_id].kind == ScopeKind::Closure {
                return Some(scope_id);
            }
            match self.scopes[scope_id].parent {
                Some(parent) => scope_id = parent,
                None => return None,
            }
        }
    }
}

/// Analyze a parsed TypeScript module and return capture information.
pub fn analyze(module: &Module) -> CaptureInfo {
    let mut builder = ScopeBuilder::new();
    let mut info = CaptureInfo::default();

    for item in &module.body {
        match item {
            ModuleItem::Stmt(stmt) => analyze_stmt(stmt, &mut builder, &mut info),
            ModuleItem::ModuleDecl(decl) => analyze_module_decl(decl, &mut builder, &mut info),
        }
    }

    info
}

/// Analyze a single function body (with its parameters pre-declared in the
/// root scope) and return capture information. Used by the ESM lowering to
/// decide which locals of the body need `Rc<RefCell<Value>>` boxing.
/// Nested fn declarations count as capture boundaries here (they are lowered
/// as closure values when they reference outer locals).
pub fn analyze_fn_body(params: &[Pat], body: &[Stmt]) -> CaptureInfo {
    let mut builder = ScopeBuilder::new();
    builder.fn_scopes_capture = true;
    for param in params {
        declare_pat(param, &mut builder);
    }
    let mut info = CaptureInfo::default();
    for stmt in body {
        analyze_stmt(stmt, &mut builder, &mut info);
    }
    info
}

fn analyze_module_decl(decl: &ModuleDecl, builder: &mut ScopeBuilder, info: &mut CaptureInfo) {
    match decl {
        ModuleDecl::ExportDecl(export) => analyze_decl(&export.decl, builder, info),
        ModuleDecl::ExportDefaultExpr(export) => analyze_expr(&export.expr, builder, info, false),
        _ => {}
    }
}

fn analyze_stmt(stmt: &Stmt, builder: &mut ScopeBuilder, info: &mut CaptureInfo) {
    match stmt {
        Stmt::Decl(decl) => analyze_decl(decl, builder, info),
        Stmt::Expr(expr_stmt) => analyze_expr(&expr_stmt.expr, builder, info, false),
        Stmt::Return(ret) => {
            if let Some(arg) = &ret.arg {
                analyze_expr(arg, builder, info, false);
            }
        }
        Stmt::If(if_stmt) => {
            analyze_expr(&if_stmt.test, builder, info, false);
            analyze_stmt(&if_stmt.cons, builder, info);
            if let Some(alt) = &if_stmt.alt {
                analyze_stmt(alt, builder, info);
            }
        }
        Stmt::For(for_stmt) => {
            builder.push_scope(ScopeKind::Block);
            if let Some(init) = &for_stmt.init {
                match init {
                    VarDeclOrExpr::VarDecl(vd) => analyze_var_decl(vd, builder, info),
                    VarDeclOrExpr::Expr(e) => analyze_expr(e, builder, info, false),
                }
            }
            if let Some(test) = &for_stmt.test {
                analyze_expr(test, builder, info, false);
            }
            if let Some(update) = &for_stmt.update {
                analyze_expr(update, builder, info, true);
            }
            analyze_stmt(&for_stmt.body, builder, info);
            builder.pop_scope();
        }
        Stmt::ForIn(fi) => {
            builder.push_scope(ScopeKind::Block);
            match &fi.left {
                ForHead::VarDecl(vd) => analyze_var_decl(vd, builder, info),
                ForHead::Pat(pat) => analyze_assign_pat(pat, builder, info),
                ForHead::UsingDecl(_) => {}
            }
            analyze_expr(&fi.right, builder, info, false);
            analyze_stmt(&fi.body, builder, info);
            builder.pop_scope();
        }
        Stmt::ForOf(fo) => {
            builder.push_scope(ScopeKind::Block);
            match &fo.left {
                ForHead::VarDecl(vd) => analyze_var_decl(vd, builder, info),
                ForHead::Pat(pat) => analyze_assign_pat(pat, builder, info),
                ForHead::UsingDecl(_) => {}
            }
            analyze_expr(&fo.right, builder, info, false);
            analyze_stmt(&fo.body, builder, info);
            builder.pop_scope();
        }
        Stmt::While(w) => {
            analyze_expr(&w.test, builder, info, false);
            analyze_stmt(&w.body, builder, info);
        }
        Stmt::DoWhile(dw) => {
            analyze_stmt(&dw.body, builder, info);
            analyze_expr(&dw.test, builder, info, false);
        }
        Stmt::Switch(switch) => {
            analyze_expr(&switch.discriminant, builder, info, false);
            builder.push_scope(ScopeKind::Block);
            for case in &switch.cases {
                if let Some(test) = &case.test {
                    analyze_expr(test, builder, info, false);
                }
                for s in &case.cons {
                    analyze_stmt(s, builder, info);
                }
            }
            builder.pop_scope();
        }
        Stmt::Try(t) => {
            builder.push_scope(ScopeKind::Block);
            for s in &t.block.stmts {
                analyze_stmt(s, builder, info);
            }
            builder.pop_scope();
            if let Some(handler) = &t.handler {
                builder.push_scope(ScopeKind::Block);
                if let Some(param) = &handler.param {
                    declare_pat(param, builder);
                }
                for s in &handler.body.stmts {
                    analyze_stmt(s, builder, info);
                }
                builder.pop_scope();
            }
            if let Some(finalizer) = &t.finalizer {
                builder.push_scope(ScopeKind::Block);
                for s in &finalizer.stmts {
                    analyze_stmt(s, builder, info);
                }
                builder.pop_scope();
            }
        }
        Stmt::Labeled(labeled) => analyze_stmt(&labeled.body, builder, info),
        Stmt::Block(block) => {
            builder.push_scope(ScopeKind::Block);
            for s in &block.stmts {
                analyze_stmt(s, builder, info);
            }
            builder.pop_scope();
        }
        _ => {}
    }
}

fn analyze_decl(decl: &Decl, builder: &mut ScopeBuilder, info: &mut CaptureInfo) {
    match decl {
        Decl::Var(var_decl) => analyze_var_decl(var_decl, builder, info),
        Decl::Fn(fn_decl) => {
            builder.declare(&fn_decl.ident.sym);
            builder.push_scope(ScopeKind::Function);
            for param in &fn_decl.function.params {
                declare_pat(&param.pat, builder);
            }
            if let Some(body) = &fn_decl.function.body {
                for s in &body.stmts {
                    analyze_stmt(s, builder, info);
                }
            }
            builder.pop_scope();
        }
        Decl::Class(class_decl) => {
            builder.declare(&class_decl.ident.sym);
            // A nested class declaration lowers to an assignment into the
            // hoisted slot: record it so a class name captured by its own
            // methods (or by forward-referencing closures) is boxed and the
            // binding stays live.
            info.assigned_names.insert(class_decl.ident.sym.to_string());
            analyze_class(&class_decl.class, builder, info);
        }
        _ => {}
    }
}

fn analyze_var_decl(var_decl: &VarDecl, builder: &mut ScopeBuilder, info: &mut CaptureInfo) {
    for decl in &var_decl.decls {
        declare_pat(&decl.name, builder);
        if let Some(init) = &decl.init {
            analyze_expr(init, builder, info, false);
            // `const id = function () { use(id) }` creates the closure before
            // the declaration's initialization write completes. A plain
            // clone capture therefore freezes the pre-declared Undefined
            // value. Treat a binding captured by its own initializer as
            // assigned so lowering gives it an Rc<RefCell> live binding.
            let mut declared = HashSet::new();
            collect_declared_names(&decl.name, &mut declared);
            for name in declared {
                if info.captures.contains_key(&name) {
                    info.assigned_names.insert(name);
                }
            }
        }
    }
}

fn collect_declared_names(pat: &Pat, names: &mut HashSet<String>) {
    match pat {
        Pat::Ident(ident) => {
            names.insert(ident.id.sym.to_string());
        }
        Pat::Array(array) => {
            for element in array.elems.iter().flatten() {
                collect_declared_names(element, names);
            }
        }
        Pat::Object(object) => {
            for property in &object.props {
                match property {
                    ObjectPatProp::Assign(assign) => {
                        names.insert(assign.key.sym.to_string());
                    }
                    ObjectPatProp::KeyValue(key_value) => {
                        collect_declared_names(&key_value.value, names)
                    }
                    ObjectPatProp::Rest(rest) => collect_declared_names(&rest.arg, names),
                }
            }
        }
        Pat::Assign(assign) => collect_declared_names(&assign.left, names),
        Pat::Rest(rest) => collect_declared_names(&rest.arg, names),
        _ => {}
    }
}

fn declare_pat(pat: &Pat, builder: &mut ScopeBuilder) {
    match pat {
        Pat::Ident(ident) => builder.declare(&ident.sym),
        Pat::Array(arr) => {
            for elem in arr.elems.iter().flatten() {
                declare_pat(elem, builder);
            }
        }
        Pat::Object(obj) => {
            for prop in &obj.props {
                match prop {
                    ObjectPatProp::Assign(a) => builder.declare(&a.key.sym),
                    ObjectPatProp::KeyValue(kv) => declare_pat(&kv.value, builder),
                    ObjectPatProp::Rest(r) => declare_pat(&r.arg, builder),
                }
            }
        }
        Pat::Rest(rest) => declare_pat(&rest.arg, builder),
        Pat::Assign(assign) => declare_pat(&assign.left, builder),
        _ => {}
    }
}

fn analyze_expr(expr: &Expr, builder: &mut ScopeBuilder, info: &mut CaptureInfo, is_write: bool) {
    match expr {
        Expr::Ident(ident) => {
            let name = ident.sym.to_string();
            if is_write {
                info.assigned_names.insert(name.clone());
            }
            if let Some(decl_scope) = builder.find_declaration(&name) {
                if builder.is_inside_closure() && builder.is_captured_from_outer(decl_scope) {
                    let closure_id = builder.current_closure_id().unwrap_or(0);
                    let detail = info.captures.entry(name).or_insert_with(|| CaptureDetail {
                        captured_by: Vec::new(),
                        is_mutated_in_closure: false,
                    });
                    if !detail.captured_by.contains(&closure_id) {
                        detail.captured_by.push(closure_id);
                    }
                    if is_write {
                        detail.is_mutated_in_closure = true;
                    }
                }
            }
        }
        Expr::Arrow(arrow) => {
            builder.push_scope(ScopeKind::Closure);
            for param in &arrow.params {
                declare_pat(param, builder);
            }
            match &*arrow.body {
                BlockStmtOrExpr::Expr(e) => analyze_expr(e, builder, info, false),
                BlockStmtOrExpr::BlockStmt(block) => {
                    for s in &block.stmts {
                        analyze_stmt(s, builder, info);
                    }
                }
            }
            builder.pop_scope();
        }
        Expr::Fn(fn_expr) => {
            builder.push_scope(ScopeKind::Closure);
            if let Some(ident) = &fn_expr.ident {
                builder.declare(&ident.sym);
            }
            for param in &fn_expr.function.params {
                declare_pat(&param.pat, builder);
            }
            if let Some(body) = &fn_expr.function.body {
                for s in &body.stmts {
                    analyze_stmt(s, builder, info);
                }
            }
            builder.pop_scope();
        }
        Expr::Assign(assign) => {
            analyze_assign_target(&assign.left, builder, info);
            analyze_expr(&assign.right, builder, info, false);
        }
        Expr::Update(update) => {
            if let Expr::Ident(ident) = update.arg.as_ref() {
                info.assigned_names.insert(ident.sym.to_string());
            }
            analyze_expr(&update.arg, builder, info, true);
        }
        Expr::Bin(bin) => {
            analyze_expr(&bin.left, builder, info, false);
            analyze_expr(&bin.right, builder, info, false);
        }
        Expr::Unary(unary) => {
            analyze_expr(&unary.arg, builder, info, false);
        }
        Expr::Call(call) => {
            if let Callee::Expr(callee) = &call.callee {
                analyze_expr(callee, builder, info, false);
            }
            for arg in &call.args {
                analyze_expr(&arg.expr, builder, info, false);
            }
        }
        Expr::Member(member) => {
            analyze_expr(&member.obj, builder, info, false);
            if let MemberProp::Computed(c) = &member.prop {
                analyze_expr(&c.expr, builder, info, false);
            }
        }
        Expr::Array(arr) => {
            for elem in arr.elems.iter().flatten() {
                analyze_expr(&elem.expr, builder, info, false);
            }
        }
        Expr::Object(obj) => {
            for prop in &obj.props {
                match prop {
                    PropOrSpread::Spread(spread) => {
                        analyze_expr(&spread.expr, builder, info, false)
                    }
                    PropOrSpread::Prop(p) => match p.as_ref() {
                        Prop::KeyValue(kv) => analyze_expr(&kv.value, builder, info, false),
                        Prop::Method(method) => {
                            analyze_function_like(&method.function, builder, info)
                        }
                        Prop::Getter(getter) => {
                            builder.push_scope(ScopeKind::Closure);
                            if let Some(body) = &getter.body {
                                for s in &body.stmts {
                                    analyze_stmt(s, builder, info);
                                }
                            }
                            builder.pop_scope();
                        }
                        Prop::Setter(setter) => {
                            builder.push_scope(ScopeKind::Closure);
                            declare_pat(&setter.param, builder);
                            if let Some(body) = &setter.body {
                                for s in &body.stmts {
                                    analyze_stmt(s, builder, info);
                                }
                            }
                            builder.pop_scope();
                        }
                        _ => {}
                    },
                }
            }
        }
        Expr::Paren(paren) => analyze_expr(&paren.expr, builder, info, is_write),
        Expr::Cond(cond) => {
            analyze_expr(&cond.test, builder, info, false);
            analyze_expr(&cond.cons, builder, info, false);
            analyze_expr(&cond.alt, builder, info, false);
        }
        Expr::Tpl(tpl) => {
            for e in &tpl.exprs {
                analyze_expr(e, builder, info, false);
            }
        }
        Expr::Seq(seq) => {
            for e in &seq.exprs {
                analyze_expr(e, builder, info, false);
            }
        }
        Expr::New(new_expr) => {
            analyze_expr(&new_expr.callee, builder, info, false);
            if let Some(args) = &new_expr.args {
                for arg in args {
                    analyze_expr(&arg.expr, builder, info, false);
                }
            }
        }
        Expr::Await(await_expr) => {
            analyze_expr(&await_expr.arg, builder, info, false);
        }
        Expr::OptChain(opt) => match opt.base.as_ref() {
            OptChainBase::Member(member) => {
                analyze_expr(&member.obj, builder, info, false);
                if let MemberProp::Computed(c) = &member.prop {
                    analyze_expr(&c.expr, builder, info, false);
                }
            }
            OptChainBase::Call(call) => {
                analyze_expr(&call.callee, builder, info, false);
                for arg in &call.args {
                    analyze_expr(&arg.expr, builder, info, false);
                }
            }
        },
        Expr::TaggedTpl(tagged) => {
            analyze_expr(&tagged.tag, builder, info, false);
            for e in &tagged.tpl.exprs {
                analyze_expr(e, builder, info, false);
            }
        }
        Expr::Class(class_expr) => analyze_class(&class_expr.class, builder, info),
        _ => {}
    }
}

/// Analyze the params + body of a function-like node (method, fn expr) as a
/// new closure scope.
fn analyze_function_like(function: &Function, builder: &mut ScopeBuilder, info: &mut CaptureInfo) {
    builder.push_scope(ScopeKind::Closure);
    for param in &function.params {
        declare_pat(&param.pat, builder);
    }
    if let Some(body) = &function.body {
        for s in &body.stmts {
            analyze_stmt(s, builder, info);
        }
    }
    builder.pop_scope();
}

/// Analyze class members (ctor, methods, fields, static blocks) for captures.
fn analyze_class(class: &Class, builder: &mut ScopeBuilder, info: &mut CaptureInfo) {
    if let Some(super_class) = &class.super_class {
        analyze_expr(super_class, builder, info, false);
    }
    for member in &class.body {
        match member {
            ClassMember::Constructor(ctor) => {
                builder.push_scope(ScopeKind::Closure);
                for param in &ctor.params {
                    if let ParamOrTsParamProp::Param(param) = param {
                        declare_pat(&param.pat, builder);
                    }
                }
                if let Some(body) = &ctor.body {
                    for s in &body.stmts {
                        analyze_stmt(s, builder, info);
                    }
                }
                builder.pop_scope();
            }
            ClassMember::Method(method) => analyze_function_like(&method.function, builder, info),
            ClassMember::PrivateMethod(method) => {
                analyze_function_like(&method.function, builder, info)
            }
            ClassMember::ClassProp(prop) => {
                if let Some(value) = &prop.value {
                    analyze_expr(value, builder, info, false);
                }
            }
            ClassMember::PrivateProp(prop) => {
                if let Some(value) = &prop.value {
                    analyze_expr(value, builder, info, false);
                }
            }
            ClassMember::StaticBlock(block) => {
                builder.push_scope(ScopeKind::Closure);
                for s in &block.body.stmts {
                    analyze_stmt(s, builder, info);
                }
                builder.pop_scope();
            }
            _ => {}
        }
    }
}

/// Record the writes performed by a destructuring assignment pattern
/// (`[a, {b}] = ...`): every bound identifier is an assignment target.
fn analyze_assign_pat(pat: &Pat, builder: &mut ScopeBuilder, info: &mut CaptureInfo) {
    match pat {
        Pat::Ident(ident) => {
            let name = ident.id.sym.to_string();
            info.assigned_names.insert(name.clone());
            if let Some(decl_scope) = builder.find_declaration(&name) {
                if builder.is_inside_closure() && builder.is_captured_from_outer(decl_scope) {
                    let closure_id = builder.current_closure_id().unwrap_or(0);
                    let detail = info.captures.entry(name).or_insert_with(|| CaptureDetail {
                        captured_by: Vec::new(),
                        is_mutated_in_closure: false,
                    });
                    if !detail.captured_by.contains(&closure_id) {
                        detail.captured_by.push(closure_id);
                    }
                    detail.is_mutated_in_closure = true;
                }
            }
        }
        Pat::Array(arr) => {
            for elem in arr.elems.iter().flatten() {
                analyze_assign_pat(elem, builder, info);
            }
        }
        Pat::Object(obj) => {
            for prop in &obj.props {
                match prop {
                    ObjectPatProp::Assign(assign) => {
                        info.assigned_names.insert(assign.key.sym.to_string());
                    }
                    ObjectPatProp::KeyValue(kv) => analyze_assign_pat(&kv.value, builder, info),
                    ObjectPatProp::Rest(rest) => analyze_assign_pat(&rest.arg, builder, info),
                }
            }
        }
        Pat::Rest(rest) => analyze_assign_pat(&rest.arg, builder, info),
        Pat::Assign(assign) => {
            analyze_expr(&assign.right, builder, info, false);
            analyze_assign_pat(&assign.left, builder, info);
        }
        _ => {}
    }
}

fn analyze_assign_target(
    target: &AssignTarget,
    builder: &mut ScopeBuilder,
    info: &mut CaptureInfo,
) {
    match target {
        AssignTarget::Simple(simple) => match simple {
            SimpleAssignTarget::Ident(ident) => {
                let name = ident.sym.to_string();
                info.assigned_names.insert(name.clone());
                if let Some(decl_scope) = builder.find_declaration(&name) {
                    if builder.is_inside_closure() && builder.is_captured_from_outer(decl_scope) {
                        let closure_id = builder.current_closure_id().unwrap_or(0);
                        let detail = info.captures.entry(name).or_insert_with(|| CaptureDetail {
                            captured_by: Vec::new(),
                            is_mutated_in_closure: false,
                        });
                        if !detail.captured_by.contains(&closure_id) {
                            detail.captured_by.push(closure_id);
                        }
                        detail.is_mutated_in_closure = true;
                    }
                }
            }
            SimpleAssignTarget::Member(member) => {
                analyze_expr(&Expr::Member(member.clone()), builder, info, true);
            }
            _ => {}
        },
        AssignTarget::Pat(pat) => match pat {
            AssignTargetPat::Array(arr) => {
                for elem in arr.elems.iter().flatten() {
                    analyze_assign_pat(elem, builder, info);
                }
            }
            AssignTargetPat::Object(obj) => {
                for prop in &obj.props {
                    match prop {
                        ObjectPatProp::Assign(assign) => {
                            info.assigned_names.insert(assign.key.sym.to_string());
                        }
                        ObjectPatProp::KeyValue(kv) => analyze_assign_pat(&kv.value, builder, info),
                        ObjectPatProp::Rest(rest) => analyze_assign_pat(&rest.arg, builder, info),
                    }
                }
            }
            AssignTargetPat::Invalid(_) => {}
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_and_analyze(source: &str) -> CaptureInfo {
        use swc_common::{FileName, SourceMap, sync::Lrc};
        use swc_ecma_parser::{Parser, StringInput, Syntax, TsSyntax};

        let cm: Lrc<SourceMap> = Default::default();
        let fm = cm.new_source_file(
            Lrc::new(FileName::Custom("test.ts".into())),
            source.to_string(),
        );
        let mut parser = Parser::new(
            Syntax::Typescript(TsSyntax {
                tsx: true,
                ..Default::default()
            }),
            StringInput::from(&*fm),
            None,
        );
        let module = parser.parse_module().expect("parse failed");
        analyze(&module)
    }

    #[test]
    fn no_capture_pure_lambda() {
        let info = parse_and_analyze("let nums = [1,2,3]; nums.map((x) => x * 2);");
        assert!(!info.is_captured("nums"), "nums should not be captured");
        assert!(!info.is_captured("x"), "x is a param, not captured");
    }

    #[test]
    fn capture_mutable_var() {
        let info = parse_and_analyze(
            r#"
            function makeCounter() {
                let count = 0;
                return () => { count += 1; return count; };
            }
            "#,
        );
        assert!(info.is_captured("count"), "count should be captured");
        assert!(
            info.captures["count"].is_mutated_in_closure,
            "count mutated in closure"
        );
    }

    #[test]
    fn capture_readonly_var() {
        let info = parse_and_analyze(
            r#"
            function greet() {
                let name = "world";
                return () => console.log(name);
            }
            "#,
        );
        assert!(info.is_captured("name"), "name should be captured");
        assert!(
            !info.captures["name"].is_mutated_in_closure,
            "name is read-only"
        );
    }

    #[test]
    fn self_referential_initializer_uses_live_binding() {
        let info = parse_and_analyze(
            r#"
            function createId() {
                const id = function () { return id; };
                return id;
            }
            "#,
        );
        assert!(info.is_captured("id"), "initializer closure captures id");
        assert!(
            info.boxed_names().contains("id"),
            "the initialization write must be visible inside its own closure"
        );
    }

    #[test]
    fn multiple_closures_share_var() {
        let info = parse_and_analyze(
            r#"
            function shared() {
                let x = 0;
                let inc = () => { x += 1; };
                let get = () => x;
            }
            "#,
        );
        assert!(info.is_captured("x"), "x should be captured");
        assert_eq!(
            info.captures["x"].captured_by.len(),
            2,
            "captured by 2 closures"
        );
        assert!(info.captures["x"].is_mutated_in_closure, "x is mutated");
    }

    #[test]
    fn nested_closure() {
        let info = parse_and_analyze(
            r#"
            function outer() {
                let x = 1;
                return () => {
                    let y = 2;
                    return () => x + y;
                };
            }
            "#,
        );
        assert!(info.is_captured("x"), "x captured by inner closures");
        assert!(info.is_captured("y"), "y captured by innermost closure");
    }

    #[test]
    fn function_param_not_captured() {
        let info = parse_and_analyze(
            r#"
            function foo(x: number) {
                return x + 1;
            }
            "#,
        );
        assert!(
            !info.is_captured("x"),
            "param x is not captured (used in own scope)"
        );
    }

    #[test]
    fn top_level_closure_captures() {
        let info = parse_and_analyze(
            r#"
            let counter = 0;
            let inc = () => { counter += 1; };
            "#,
        );
        assert!(
            info.is_captured("counter"),
            "top-level counter captured by closure"
        );
        assert!(info.captures["counter"].is_mutated_in_closure);
    }
}
