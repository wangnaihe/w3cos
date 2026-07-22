//! ESM statement/expression lowering: JS AST → Rust code strings.
//!
//! This module takes SWC `Stmt` and `Expr` nodes and produces equivalent Rust
//! source text. It is intentionally a "best effort" structural lowering: JS
//! semantics that have no Rust equivalent emit a `todo!()` with a comment.

use std::collections::HashSet;
use swc_ecma_ast::*;

/// Class-body context carried while lowering a single function/method body.
///
/// Set when lowering inside a class member (constructor, method, getter,
/// setter, static block, field initializer) so `this`, `super`, and private
/// `#name` references lower correctly.
#[derive(Clone, Debug, Default)]
pub struct ClassScope {
    /// Sanitized class name used to mangle private members:
    /// `__w3cos_priv_{class_name}_{name}`.
    pub class_name: String,
    /// Rust expression evaluating to the parent class `Value` (for `extends`).
    pub parent: Option<String>,
    /// Whether the current member is static (`super` then reads the parent
    /// class object instead of its prototype).
    pub is_static: bool,
}

/// Context carried while lowering a single function/method body.
pub struct LowerCtx {
    indent: usize,
    /// local name → bundled name (for cross-module references).
    pub renames: Vec<(String, String)>,
    dynamic_values: bool,
    temp_index: usize,
    value_bindings: HashSet<String>,
    known_values: HashSet<String>,
    /// Local names that refer to classes (own or imported). References lower
    /// to a call of the class accessor (`X()`), which yields the class Value.
    class_names: HashSet<String>,
    /// Local names bound by `import * as ns from ...`. References lower to a
    /// call of the namespace accessor (`ns()`), which yields the lazily built
    /// namespace object Value.
    namespace_names: HashSet<String>,
    /// What `this` lowers to in the current scope (`__this` inside classes).
    this_name: Option<String>,
    /// Active class-body scope, if any.
    class_scope: Option<ClassScope>,
    /// Stack of try-flow enum names for enclosing `try` blocks (innermost
    /// last). While non-empty, `return` lowers to an early-return variant of
    /// the innermost try's flow enum so `finally` still runs; the enum value
    /// is re-thrown/propagated after the finally block. Cleared when entering
    /// a nested function closure (returns there target the closure itself).
    try_flow_stack: Vec<String>,
    /// Function-scoped `var` names hoisted to the top of the fn body being
    /// lowered (JS var hoisting): pre-declared there as `Undefined`, and
    /// `var x = ...` lowers to `x = ...` (assignment, not shadowing).
    hoisted_vars: HashSet<String>,
    /// Names of nested fn declarations emitted as Rust fn items taking
    /// `(__args: Vec<Value>)`. Value positions wrap them as
    /// `Value::function(...)`; call positions invoke them with `vec![args]`.
    fn_item_names: HashSet<String>,
    /// Locals lowered as `Rc<RefCell<Value>>` (captured by a closure AND
    /// assigned somewhere — see `scope_analysis::CaptureInfo::boxed_names`).
    /// Declarations, reads, and writes of these names all go through the
    /// cell, so `Fn` closures can mutate captures with JS live-binding
    /// semantics. Inherited by child (closure) contexts; over-approximated by
    /// name, which is sound (all uses of a boxed name are consistent).
    boxed: HashSet<String>,
    /// Rust labels of enclosing loops (innermost last) for `break`/`continue`
    /// emission. Cleared when crossing a fn/closure boundary.
    loop_labels: Vec<String>,
    /// Rust labels of enclosing breakable constructs (loops and switch
    /// blocks, innermost last) for `break` emission.
    break_labels: Vec<String>,
    /// JS label name → Rust label for labeled statements in scope.
    named_labels: Vec<(String, String)>,
    /// Subset of `named_labels` that target loops and are therefore legal
    /// `continue` destinations in Rust as well as JavaScript.
    named_loop_labels: Vec<(String, String)>,
    /// A JS label to attach to the next loop lowered (set by Stmt::Labeled).
    pending_loop_label: Option<String>,
}

impl LowerCtx {
    pub fn new(renames: Vec<(String, String)>) -> Self {
        Self {
            indent: 2,
            renames,
            dynamic_values: false,
            temp_index: 0,
            value_bindings: HashSet::new(),
            known_values: HashSet::new(),
            class_names: HashSet::new(),
            namespace_names: HashSet::new(),
            this_name: None,
            class_scope: None,
            try_flow_stack: Vec::new(),
            hoisted_vars: HashSet::new(),
            fn_item_names: HashSet::new(),
            boxed: HashSet::new(),
            loop_labels: Vec::new(),
            break_labels: Vec::new(),
            named_labels: Vec::new(),
            named_loop_labels: Vec::new(),
            pending_loop_label: None,
        }
    }

    pub fn new_dynamic(renames: Vec<(String, String)>) -> Self {
        Self {
            indent: 2,
            renames,
            dynamic_values: true,
            temp_index: 0,
            value_bindings: HashSet::new(),
            known_values: HashSet::new(),
            class_names: HashSet::new(),
            namespace_names: HashSet::new(),
            this_name: None,
            class_scope: None,
            try_flow_stack: Vec::new(),
            hoisted_vars: HashSet::new(),
            fn_item_names: HashSet::new(),
            boxed: HashSet::new(),
            loop_labels: Vec::new(),
            break_labels: Vec::new(),
            named_labels: Vec::new(),
            named_loop_labels: Vec::new(),
            pending_loop_label: None,
        }
    }

    pub fn new_dynamic_with_bindings(
        renames: Vec<(String, String)>,
        value_bindings: HashSet<String>,
    ) -> Self {
        Self {
            indent: 2,
            renames,
            dynamic_values: true,
            temp_index: 0,
            value_bindings,
            known_values: HashSet::new(),
            class_names: HashSet::new(),
            namespace_names: HashSet::new(),
            this_name: None,
            class_scope: None,
            try_flow_stack: Vec::new(),
            hoisted_vars: HashSet::new(),
            fn_item_names: HashSet::new(),
            boxed: HashSet::new(),
            loop_labels: Vec::new(),
            break_labels: Vec::new(),
            named_labels: Vec::new(),
            named_loop_labels: Vec::new(),
            pending_loop_label: None,
        }
    }

    /// Mark local names that refer to classes (references lower to `X()`).
    pub fn with_classes(mut self, class_names: HashSet<String>) -> Self {
        self.class_names = class_names;
        self
    }

    /// Mark local names bound by `import * as ns` (references lower to the
    /// namespace accessor call `ns_fn()`, yielding the namespace object).
    pub fn with_namespaces(mut self, namespace_names: HashSet<String>) -> Self {
        self.namespace_names = namespace_names;
        self
    }

    /// Enter a normal JavaScript function call scope with dynamic `this`.
    pub fn with_function_this(mut self) -> Self {
        self.this_name = Some("__this".to_string());
        self
    }

    /// Enter a class member scope: `this` becomes `__this`, `super` and
    /// private `#name` references resolve per the given scope.
    pub fn with_class_scope(mut self, scope: ClassScope) -> Self {
        self.this_name = Some("__this".to_string());
        self.class_scope = Some(scope);
        self
    }

    /// A dynamic child context inheriting names, class bindings, and class
    /// scope (used for closures, class-expression members, etc.).
    fn child_dynamic_ctx(&self) -> Self {
        let mut ctx =
            LowerCtx::new_dynamic_with_bindings(self.renames.clone(), self.value_bindings.clone());
        ctx.class_names = self.class_names.clone();
        ctx.namespace_names = self.namespace_names.clone();
        ctx.this_name = self.this_name.clone();
        ctx.class_scope = self.class_scope.clone();
        ctx.fn_item_names = self.fn_item_names.clone();
        // Boxed (Rc<RefCell>) bindings keep their cell-based form across the
        // closure boundary — the capture prologue clones the Rc, so the
        // closure shares the same cell as the enclosing scope.
        ctx.boxed = self.boxed.clone();
        // break/continue labels do not cross fn/closure boundaries (the child
        // body is a new Rust closure).
        ctx
    }
    fn pad(&self) -> String {
        " ".repeat(self.indent)
    }

    /// Take the pending JS loop label (set by Stmt::Labeled) or mint a fresh
    /// Rust label for a loop/switch being lowered. Loop labels make
    /// `break`/`continue` inside labeled blocks (notably `switch`) legal
    /// Rust and give JS labeled statements a target.
    fn take_loop_label(&mut self) -> String {
        if let Some(label) = self.pending_loop_label.take() {
            return label;
        }
        let index = self.temp_index;
        self.temp_index += 1;
        format!("__lp{index}")
    }

    /// Lower a statement as a loop body with the given labels active for
    /// `break`/`continue` emission. For `for`/`do-while` loops the continue
    /// target is a labeled block wrapping the body (so `continue` still runs
    /// the update/test, matching JS); for `while`/`for-in`/`for-of` it is the
    /// loop's own label.
    fn lower_loop_body(&mut self, body: &Stmt, break_label: &str, continue_label: &str) -> String {
        self.break_labels.push(break_label.to_string());
        self.loop_labels.push(continue_label.to_string());
        let out = self.lower_stmt(body);
        self.break_labels.pop();
        self.loop_labels.pop();
        out
    }

    fn resolve_name(&self, name: &str) -> String {
        // A local binding (param/hoisted/local/capture) shadows any import or
        // module-level mapping with the same name — never route those through
        // the `{bundled}_get` cell accessor.
        if self.dynamic_values && self.known_values.contains(name) {
            return name.to_string();
        }
        let has_mapping = self.renames.iter().any(|(local, _)| local == name);
        let resolved = self
            .renames
            .iter()
            .find(|(local, _)| local == name)
            .map(|(_, bundled)| {
                if !self.dynamic_values || self.value_bindings.contains(name) {
                    bundled.clone()
                } else {
                    sanitize_ident(name)
                }
            })
            .unwrap_or_else(|| name.to_string());
        if self.dynamic_values && self.value_bindings.contains(name) && has_mapping {
            format!("{resolved}_get()")
        } else {
            resolved
        }
    }

    fn resolve_value(&self, name: &str) -> String {
        let resolved = self.resolve_name(name);
        if self.dynamic_values && self.known_values.contains(name) {
            if self.boxed.contains(name) {
                // Rc<RefCell<Value>> local: read through the shared cell.
                Self::boxed_read(&resolved)
            } else {
                format!("{resolved}.clone()")
            }
        } else if self.class_names.contains(name) {
            // Class reference: call the class accessor to get the class Value.
            format!("{resolved}()")
        } else if self.namespace_names.contains(name) {
            // `import * as ns` reference: call the namespace accessor to get
            // the lazily built namespace object Value.
            format!("{resolved}()")
        } else if self.fn_item_names.contains(name) {
            // Nested fn item (`fn name(__args)`): wrap as a callable Value.
            format!("w3cos_core::Value::function(move |_this, __args| {name}(__args))")
        } else if self.dynamic_values
            && self.renames.iter().any(|(local, _)| local == name)
            && !self.value_bindings.contains(name)
        {
            // Top-level JS function declarations are objects with stable
            // identity: properties written through `Fn.prototype` must be
            // visible to a later `new Fn()`. The generated `_value()`
            // accessor interns that callable instead of allocating a fresh
            // Value for every reference.
            format!("{resolved}_value()")
        } else if self.dynamic_values
            && !self.renames.iter().any(|(local, _)| local == name)
            && let Some(global) = global_value_expr(name)
        {
            // Unshadowed JS global: jsdom bridge or a core builtin facade.
            global
        } else {
            resolved
        }
    }

    /// True when `name` is bound locally (parameter/local binding) or via an
    /// import/module-level symbol — i.e. it must NOT resolve to a JS global.
    fn is_name_shadowed(&self, name: &str) -> bool {
        self.known_values.contains(name) || self.renames.iter().any(|(local, _)| local == name)
    }

    /// Lower a bare identifier reference (used by codegen for alias targets
    /// that are not module-local bindings, e.g. globals).
    pub fn lower_ident(&self, name: &str) -> String {
        self.resolve_value(&sanitize_ident(name))
    }

    /// Mangle a private `#name` into its property key for a class.
    pub(crate) fn mangle_private(class_name: &str, name: &PrivateName) -> String {
        format!("__w3cos_priv_{class_name}_{}", atom_str(&name.name))
    }

    /// The property key for a private `#name` in the current class scope.
    fn private_key(&self, name: &PrivateName) -> String {
        match &self.class_scope {
            Some(scope) => Self::mangle_private(&scope.class_name, name),
            None => atom_str(&name.name),
        }
    }

    /// The literal form of a property key, when statically known.
    pub(crate) fn key_literal(&self, key: &PropName) -> Option<String> {
        match key {
            PropName::Ident(ident) => Some(ident.sym.to_string()),
            PropName::Str(value) => Some(wtf8_to_string(&value.value)),
            PropName::Num(value) => Some(value.value.to_string()),
            PropName::BigInt(value) => Some(value.value.to_string()),
            PropName::Computed(_) => None,
        }
    }

    /// A `&str`-compatible key argument for `get_property`/`set_property`,
    /// with an optional convention prefix (`__w3cos_getter_` etc.).
    pub(crate) fn key_arg(&self, prefix: &str, key: &PropName) -> String {
        match self.key_literal(key) {
            Some(name) => format!("{:?}", format!("{prefix}{name}")),
            None => {
                let PropName::Computed(computed) = key else {
                    unreachable!("key_literal covers all non-computed keys")
                };
                let expr = self.lower_expr(&computed.expr);
                if prefix.is_empty() {
                    format!("&{expr}.to_js_string()")
                } else {
                    format!("&format!(\"{prefix}{{}}\", {expr}.to_js_string())")
                }
            }
        }
    }

    /// The Rust expression `this` lowers to in the current scope.
    fn this_expr(&self) -> String {
        self.this_name
            .clone()
            .unwrap_or_else(|| "w3cos_core::Value::Undefined".to_string())
    }

    /// True when a nested closure emitted here must also capture `__parent`:
    /// we're inside a class-expression member (`__parent` is a captured
    /// local there) and the closure may call `super.*`/`super(...)`.
    fn needs_parent_capture(&self) -> bool {
        self.known_values.contains("__parent")
            && matches!(
                self.class_scope.as_ref().and_then(|s| s.parent.as_deref()),
                Some("__parent")
            )
    }

    /// Append `__parent` to a closure capture list when [`Self::needs_parent_capture`].
    fn push_parent_capture(&self, captures: &mut Vec<String>) {
        if self.needs_parent_capture() && !captures.iter().any(|name| name == "__parent") {
            captures.push("__parent".to_string());
            captures.sort();
        }
    }

    pub fn bind_patterns(&mut self, patterns: &[Pat]) {
        for pattern in patterns {
            collect_pattern_names(pattern, &mut self.known_values);
        }
    }

    /// Enter a function-body scope: compute which of the body's locals are
    /// captured by a closure AND assigned (see `scope_analysis`) and union
    /// them into `self.boxed`. Returns the previous set — pass it to
    /// [`Self::leave_fn_scope`] when the body is lowered on `self` directly
    /// (nested fn-item form); child-context users can ignore it.
    pub fn enter_fn_scope(&mut self, params: &[Pat], body: &[Stmt]) -> HashSet<String> {
        let info = crate::scope_analysis::analyze_fn_body(params, body);
        // The analysis reports raw JS names; the lowering tracks sanitized
        // Rust identifiers (e.g. `x$1` → `x_d1`) — align before storing.
        let own: HashSet<String> = info
            .boxed_names()
            .into_iter()
            .map(|name| sanitize_ident(&name))
            .collect();
        let union: HashSet<String> = self.boxed.union(&own).cloned().collect();
        std::mem::replace(&mut self.boxed, union)
    }

    /// Restore the boxed set saved by [`Self::enter_fn_scope`].
    pub fn leave_fn_scope(&mut self, saved: HashSet<String>) {
        self.boxed = saved;
    }

    /// True when `name` is lowered as `Rc<RefCell<Value>>` in this scope.
    fn is_boxed(&self, name: &str) -> bool {
        self.dynamic_values && self.boxed.contains(name)
    }

    /// Read a boxed local (`Rc<RefCell<Value>>`) as a `Value`.
    pub(crate) fn boxed_read(name: &str) -> String {
        format!("(*{name}.borrow()).clone()")
    }

    /// Write a boxed local through its cell.
    pub(crate) fn boxed_write(name: &str, value: &str) -> String {
        format!("*{name}.borrow_mut() = {value};")
    }

    /// `let` binding text for a (possibly boxed) local: plain locals get
    /// `let mut` (JS bindings are all reassignable); boxed ones get the
    /// `Rc<RefCell<Value>>` cell shared with capturing closures.
    fn bind_local(&self, name: &str, value: &str) -> String {
        if self.is_boxed(name) {
            format!("let {name} = std::rc::Rc::new(std::cell::RefCell::new({value}));")
        } else {
            format!("let mut {name} = {value};")
        }
    }

    /// The `x.is_undefined()` test for default-value fixups — reads through
    /// the cell for boxed (Rc<RefCell>) names.
    fn undefined_check(&self, name: &str) -> String {
        if self.is_boxed(name) {
            format!("{name}.borrow().is_undefined()")
        } else {
            format!("{name}.is_undefined()")
        }
    }

    /// Statement assigning an already-declared local — writes through the
    /// cell for boxed (Rc<RefCell>) names.
    fn assign_local(&self, name: &str, source: &str) -> String {
        if self.is_boxed(name) {
            Self::boxed_write(name, source)
        } else {
            format!("{name} = {source};")
        }
    }

    /// Pre-declare function-scoped `var` bindings at the top of a fn body
    /// (JS var hoisting) and return the prologue code. Every `var` name found
    /// anywhere in the body (including nested blocks/loops/try bodies, but
    /// not nested fns) becomes `let mut name = Undefined;` here, and the
    /// actual `var x = ...` statements then lower to assignments, so closures
    /// created before the declaration line capture the same binding.
    pub fn hoist_fn_body_vars(&mut self, body: &[Stmt]) -> String {
        if !self.dynamic_values {
            return String::new();
        }
        let mut names = HashSet::new();
        collect_hoisted_var_names(body, &mut names);
        // Fn declarations that will take the Rust fn-item form (self-
        // recursive but capture-free) must NOT be shadowed by a predeclared
        // local — the fn item is already visible throughout the block.
        for stmt in body {
            if let Stmt::Decl(Decl::Fn(f)) = stmt {
                let name = sanitize_ident(&f.ident.sym.to_string());
                let fbody: &[Stmt] = f
                    .function
                    .body
                    .as_ref()
                    .map(|b| b.stmts.as_slice())
                    .unwrap_or_default();
                let pats: Vec<Pat> = f.function.params.iter().map(|p| p.pat.clone()).collect();
                let param_names = pattern_names(&pats);
                let outer: HashSet<String> = self
                    .known_values
                    .difference(&param_names)
                    .cloned()
                    .collect();
                if stmts_reference_ident(fbody, &name)
                    && !stmts_reference_any_ident(fbody, &outer)
                    && !stmts_reference_ident(fbody, "arguments")
                {
                    names.remove(&name);
                }
            }
        }
        let mut names: Vec<String> = names.into_iter().collect();
        names.sort();
        let mut prologue = String::new();
        for name in names {
            // Params and already-bound locals are not re-declared.
            if self.known_values.contains(&name) {
                continue;
            }
            if self.is_boxed(&name) {
                prologue.push_str(&format!(
                    "let {name} = std::rc::Rc::new(std::cell::RefCell::new(w3cos_core::Value::Undefined)); "
                ));
            } else {
                prologue.push_str(&format!("let mut {name} = w3cos_core::Value::Undefined; "));
            }
            self.known_values.insert(name.clone());
            self.hoisted_vars.insert(name);
        }
        prologue
    }

    pub fn lower_stmts(&mut self, stmts: &[Stmt]) -> String {
        stmts
            .iter()
            .map(|s| self.lower_stmt(s))
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn lower_stmt(&mut self, stmt: &Stmt) -> String {
        match stmt {
            Stmt::Expr(expr_stmt) => {
                let e = self.lower_expr(&expr_stmt.expr);
                format!("{}{};", self.pad(), e)
            }
            Stmt::Return(ret) => match &ret.arg {
                Some(expr) => {
                    if self.dynamic_values
                        && let Some(flow) = self.try_flow_stack.last()
                    {
                        // Early return inside a `try`: wrap in the try's flow
                        // enum so the finally block runs before propagating.
                        return format!(
                            "{}return {}::Return({});",
                            self.pad(),
                            flow,
                            self.lower_expr(expr)
                        );
                    }
                    format!("{}return {};", self.pad(), self.lower_expr(expr))
                }
                None if self.dynamic_values => {
                    if let Some(flow) = self.try_flow_stack.last() {
                        return format!(
                            "{}return {}::Return(w3cos_core::Value::Undefined);",
                            self.pad(),
                            flow
                        );
                    }
                    format!("{}return w3cos_core::Value::Undefined;", self.pad())
                }
                None => format!("{}return;", self.pad()),
            },
            Stmt::Decl(decl) => self.lower_decl(decl),
            Stmt::Block(block) => {
                let outer_values = self.known_values.clone();
                let mut out = format!("{}{{\n", self.pad());
                self.indent += 4;
                for s in &block.stmts {
                    out.push_str(&self.lower_stmt(s));
                    out.push('\n');
                }
                self.indent -= 4;
                self.known_values = outer_values;
                out.push_str(&format!("{}}}", self.pad()));
                out
            }
            Stmt::If(if_stmt) => self.lower_if(if_stmt),
            Stmt::For(for_stmt) => self.lower_for(for_stmt),
            Stmt::While(while_stmt) => {
                let test = self.lower_expr(&while_stmt.test);
                let label = if self.dynamic_values {
                    self.take_loop_label()
                } else {
                    String::new()
                };
                let body = if self.dynamic_values {
                    self.lower_loop_body(&while_stmt.body, &label.clone(), &label.clone())
                } else {
                    self.lower_stmt(&while_stmt.body)
                };
                let test = if self.dynamic_values {
                    format!("{test}.to_bool()")
                } else {
                    test
                };
                let prefix = if label.is_empty() {
                    label
                } else {
                    format!("'{label}: ")
                };
                format!(
                    "{}{prefix}while {} {{\n{}\n{}}}",
                    self.pad(),
                    test,
                    body,
                    self.pad()
                )
            }
            // PLACEHOLDER_REMAINING_STMTS
            Stmt::Try(try_stmt) => self.lower_try(try_stmt),
            Stmt::Throw(throw_stmt) => {
                let arg = self.lower_expr(&throw_stmt.arg);
                if self.dynamic_values {
                    // Thrown JS values travel as panic payloads via the shared
                    // `PanicValue` wrapper (Value is not Send); catch_unwind in
                    // compiled try/catch and in the promise reaction runner
                    // both recover it.
                    format!("{}w3cos_core::throw_value({});", self.pad(), arg)
                } else {
                    format!("{}panic!(\"{{}}\", {});", self.pad(), arg)
                }
            }
            Stmt::Switch(switch) => self.lower_switch(switch),
            Stmt::Break(break_stmt) => {
                if !self.dynamic_values {
                    return format!("{}break;", self.pad());
                }
                match &break_stmt.label {
                    Some(label) => {
                        let js = atom_str(&label.sym);
                        let rust = self
                            .named_labels
                            .iter()
                            .rev()
                            .find(|(name, _)| name == &js)
                            .map(|(_, rust)| rust.clone());
                        match rust {
                            Some(rust) => format!("{}break '{rust};", self.pad()),
                            None if !self.try_flow_stack.is_empty() => format!(
                                "{}return {}::Break({js:?});",
                                self.pad(),
                                self.try_flow_stack.last().expect("checked above")
                            ),
                            None => format!("{}break; /* unresolved label {js} */", self.pad()),
                        }
                    }
                    None => match self.break_labels.last() {
                        Some(rust) => format!("{}break '{rust};", self.pad()),
                        None if !self.try_flow_stack.is_empty() => format!(
                            "{}return {}::Break(\"\");",
                            self.pad(),
                            self.try_flow_stack.last().expect("checked above")
                        ),
                        None => format!("{}break;", self.pad()),
                    },
                }
            }
            Stmt::Continue(continue_stmt) => {
                if !self.dynamic_values {
                    return format!("{}continue;", self.pad());
                }
                match &continue_stmt.label {
                    Some(label) => {
                        let js = atom_str(&label.sym);
                        let rust = self
                            .named_loop_labels
                            .iter()
                            .rev()
                            .find(|(name, _)| name == &js)
                            .map(|(_, rust)| rust.clone());
                        match rust {
                            Some(rust) => format!("{}continue '{rust};", self.pad()),
                            None if !self.try_flow_stack.is_empty() => format!(
                                "{}return {}::Continue({js:?});",
                                self.pad(),
                                self.try_flow_stack.last().expect("checked above")
                            ),
                            None => {
                                format!("{}continue; /* unresolved label {js} */", self.pad())
                            }
                        }
                    }
                    None => match self.loop_labels.last() {
                        Some(rust) => format!("{}continue '{rust};", self.pad()),
                        None if !self.try_flow_stack.is_empty() => format!(
                            "{}return {}::Continue(\"\");",
                            self.pad(),
                            self.try_flow_stack.last().expect("checked above")
                        ),
                        None => format!("{}continue;", self.pad()),
                    },
                }
            }
            Stmt::DoWhile(do_while) => {
                let label = if self.dynamic_values {
                    self.take_loop_label()
                } else {
                    String::new()
                };
                self.indent += 4;
                let body = if self.dynamic_values {
                    self.lower_loop_body(&do_while.body, &label.clone(), &label.clone())
                } else {
                    self.lower_stmt(&do_while.body)
                };
                self.indent -= 4;
                let test = self.lower_expr(&do_while.test);
                let test = if self.dynamic_values {
                    format!("{test}.to_bool()")
                } else {
                    test
                };
                let (prefix, break_) = if label.is_empty() {
                    (String::new(), "break;".to_string())
                } else {
                    (format!("'{label}: "), format!("break '{label};"))
                };
                if self.dynamic_values {
                    // `continue` in a do-while must run the test: the test
                    // lives at the loop head, guarded so the first iteration
                    // always runs the body (JS do-while semantics).
                    let inner_pad = " ".repeat(self.indent + 4);
                    format!(
                        "{}let mut __w3cos_first = true;\n{}{prefix}loop {{\n{inner_pad}if !__w3cos_first {{ if !({test}) {{ {break_} }} }}\n{inner_pad}__w3cos_first = false;\n{}\n{}}}",
                        self.pad(),
                        self.pad(),
                        body,
                        self.pad()
                    )
                } else {
                    format!(
                        "{}{prefix}loop {{\n{}\n{}if !({}) {{ {} }}\n{}}}",
                        self.pad(),
                        body,
                        " ".repeat(self.indent + 4),
                        test,
                        break_,
                        self.pad()
                    )
                }
            }
            Stmt::ForIn(for_in) => {
                let right = self.lower_expr(&for_in.right);
                let label = if self.dynamic_values {
                    self.take_loop_label()
                } else {
                    String::new()
                };
                let (left, prelude) = self.lower_for_head(&for_in.left);
                // Register the loop variable(s) so the body (and closures
                // capturing them) can reference them.
                let saved_known = self.known_values.clone();
                match &for_in.left {
                    ForHead::VarDecl(vd) => {
                        if let Some(d) = vd.decls.first() {
                            collect_pattern_names(&d.name, &mut self.known_values);
                        }
                    }
                    ForHead::Pat(p) => collect_pattern_names(p, &mut self.known_values),
                    _ => {}
                }
                self.indent += 4;
                let body = if self.dynamic_values {
                    self.lower_loop_body(&for_in.body, &label.clone(), &label.clone())
                } else {
                    self.lower_stmt(&for_in.body)
                };
                self.indent -= 4;
                self.known_values = saved_known;
                let body = if prelude.is_empty() {
                    body
                } else {
                    format!("{prelude}\n{body}")
                };
                let prefix = if label.is_empty() {
                    label
                } else {
                    format!("'{label}: ")
                };
                format!(
                    "{}{prefix}for {left} in Object.call_method(\"keys\", vec![{right}]).iter() {{\n{}\n{}}}",
                    self.pad(),
                    body,
                    self.pad()
                )
            }
            Stmt::ForOf(for_of) => {
                let right = self.lower_expr(&for_of.right);
                let label = if self.dynamic_values {
                    self.take_loop_label()
                } else {
                    String::new()
                };
                let (left, prelude) = self.lower_for_head(&for_of.left);
                let saved_known = self.known_values.clone();
                match &for_of.left {
                    ForHead::VarDecl(vd) => {
                        if let Some(d) = vd.decls.first() {
                            collect_pattern_names(&d.name, &mut self.known_values);
                        }
                    }
                    ForHead::Pat(p) => collect_pattern_names(p, &mut self.known_values),
                    _ => {}
                }
                self.indent += 4;
                let body = if self.dynamic_values {
                    self.lower_loop_body(&for_of.body, &label.clone(), &label.clone())
                } else {
                    self.lower_stmt(&for_of.body)
                };
                self.indent -= 4;
                self.known_values = saved_known;
                let body = if prelude.is_empty() {
                    body
                } else {
                    format!("{prelude}\n{body}")
                };
                let prefix = if label.is_empty() {
                    label
                } else {
                    format!("'{label}: ")
                };
                format!(
                    "{}{prefix}for {left} in {right}.iter() {{\n{}\n{}}}",
                    self.pad(),
                    body,
                    self.pad()
                )
            }
            Stmt::Labeled(labeled) => {
                let label = atom_str(&labeled.label.sym);
                if !self.dynamic_values {
                    let body = self.lower_stmt(&labeled.body);
                    return format!("{}// label: {label}\n{body}", self.pad());
                }
                let rust = format!("__js_{label}");
                match labeled.body.as_ref() {
                    // Labeled loop: attach the label to the loop itself so
                    // `break lbl` / `continue lbl` both work.
                    Stmt::For(_)
                    | Stmt::ForIn(_)
                    | Stmt::ForOf(_)
                    | Stmt::While(_)
                    | Stmt::DoWhile(_) => {
                        self.named_labels.push((label.clone(), rust.clone()));
                        self.named_loop_labels.push((label.clone(), rust.clone()));
                        self.pending_loop_label = Some(rust);
                        let out = self.lower_stmt(&labeled.body);
                        self.named_loop_labels.pop();
                        self.named_labels.pop();
                        out
                    }
                    // Labeled block (or any other statement): wrap in a Rust
                    // labeled block so `break lbl` targets it.
                    _ => {
                        self.named_labels.push((label, rust.clone()));
                        self.break_labels.push(rust.clone());
                        let body = self.lower_stmt(&labeled.body);
                        self.break_labels.pop();
                        self.named_labels.pop();
                        format!("{}'{rust}: {{\n{}\n{}}}", self.pad(), body, self.pad())
                    }
                }
            }
            Stmt::Empty(_) => String::new(),
            _ => format!("{}/* unsupported stmt */", self.pad()),
        }
    }

    fn lower_decl(&mut self, decl: &Decl) -> String {
        match decl {
            Decl::Var(var_decl) => {
                let mut lines = Vec::new();
                for d in &var_decl.decls {
                    let val = d
                        .init
                        .as_ref()
                        .map(|e| self.lower_expr(e))
                        .unwrap_or_else(|| {
                            if self.dynamic_values {
                                "w3cos_core::Value::Undefined".to_string()
                            } else {
                                "Default::default()".to_string()
                            }
                        });
                    if self.dynamic_values && !matches!(d.name, Pat::Ident(_)) {
                        let temporary = format!("__binding{}", self.temp_index);
                        self.temp_index += 1;
                        lines.push(format!("{}let {temporary} = {val};", self.pad()));
                        self.lower_dynamic_local_pattern(
                            &d.name,
                            &temporary,
                            &mut lines,
                            self.indent,
                        );
                        collect_pattern_names(&d.name, &mut self.known_values);
                        continue;
                    }
                    let name = self.lower_pat(&d.name);
                    if self.dynamic_values
                        && matches!(&d.name, Pat::Ident(_))
                        && self.hoisted_vars.contains(&name)
                    {
                        // Hoisted binding: assign into the slot pre-declared
                        // at the fn body's top (see hoist_fn_body_vars).
                        if self.is_boxed(&name) {
                            lines.push(format!("{}{}", self.pad(), Self::boxed_write(&name, &val)));
                        } else {
                            lines.push(format!("{}{name} = {val};", self.pad()));
                        }
                        continue;
                    }
                    let binding = if self.dynamic_values {
                        // JS bindings are all reassignable → `let mut`; boxed
                        // (captured+assigned) ones get the shared cell.
                        self.bind_local(&name, &val)
                    } else {
                        let kw = if var_decl.kind == VarDeclKind::Const {
                            "let"
                        } else {
                            "let mut"
                        };
                        format!("{kw} {name} = {val};")
                    };
                    lines.push(format!("{}{binding}", self.pad()));
                    collect_pattern_names(&d.name, &mut self.known_values);
                }
                lines.join("\n")
            }
            Decl::Fn(fn_decl) => {
                let name = atom_str(&fn_decl.ident.sym);
                let body_stmts: &[Stmt] = fn_decl
                    .function
                    .body
                    .as_ref()
                    .map(|b| b.stmts.as_slice())
                    .unwrap_or_default();
                if self.dynamic_values {
                    let self_refs = stmts_reference_ident(body_stmts, &name);
                    let pats: Vec<Pat> = fn_decl
                        .function
                        .params
                        .iter()
                        .map(|p| p.pat.clone())
                        .collect();
                    let param_names = pattern_names(&pats);
                    let outer_candidates: HashSet<String> = self
                        .known_values
                        .difference(&param_names)
                        .cloned()
                        .collect();
                    let needs_capture = stmts_reference_any_ident(body_stmts, &outer_candidates);
                    if !self_refs {
                        // Nested fn declaration → a closure value so it can
                        // capture enclosing locals (Rust fn items cannot).
                        let value = self.lower_dynamic_function_value(&pats, body_stmts);
                        if self.hoisted_vars.contains(&name) {
                            // Fn declarations hoist: assign the hoisted slot.
                            if self.is_boxed(&name) {
                                return format!(
                                    "{}{}",
                                    self.pad(),
                                    Self::boxed_write(&name, &value)
                                );
                            }
                            return format!("{}{name} = {value};", self.pad());
                        }
                        self.known_values.insert(name.clone());
                        return format!("{}{}", self.pad(), self.bind_local(&name, &value));
                    }
                    if needs_capture || stmts_reference_ident(body_stmts, "arguments") {
                        // Self-recursive AND capturing: the binding is
                        // pre-declared (hoisted) so the closure can capture
                        // it; the recursive reference sees the value captured
                        // at creation time (the pre-declared Undefined), so
                        // recursion degrades — recorded limitation.
                        // (`arguments` also forces the closure form: fn items
                        // have no `__args` in scope.)
                        self.known_values.insert(name.clone());
                        let value = self.lower_dynamic_function_value(&pats, body_stmts);
                        if self.hoisted_vars.contains(&name) {
                            if self.is_boxed(&name) {
                                return format!(
                                    "{}{}",
                                    self.pad(),
                                    Self::boxed_write(&name, &value)
                                );
                            }
                            return format!("{}{name} = {value};", self.pad());
                        }
                        if self.is_boxed(&name) {
                            // Boxed (captured+assigned): the cell is shared
                            // with the closure, so recursion stays live.
                            return format!(
                                "{}let {name} = std::rc::Rc::new(std::cell::RefCell::new(w3cos_core::Value::Undefined));\n{}{}",
                                self.pad(),
                                self.pad(),
                                Self::boxed_write(&name, &value)
                            );
                        }
                        return format!(
                            "{}let mut {name} = w3cos_core::Value::Undefined;\n{}{name} = {value};",
                            self.pad(),
                            self.pad()
                        );
                    }
                    // Self-recursive but capture-free: keep the fn-item form
                    // below (its name is in scope there).
                    // Register the name BEFORE lowering the body — recursive
                    // calls inside must already resolve to the direct
                    // fn-item call form `name(vec![...])`.
                    self.fn_item_names.insert(name.clone());
                }
                let params = fn_decl
                    .function
                    .params
                    .iter()
                    .map(|p| {
                        let name = self.lower_pat(&p.pat);
                        if self.dynamic_values {
                            format!("{name}: w3cos_core::Value")
                        } else {
                            name
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                // A nested fn has its own return type; try-flow wrapping from
                // an enclosing try block must not leak into its body.
                let saved_flow = std::mem::take(&mut self.try_flow_stack);
                // Bind the params for the body (and restore afterwards so
                // they don't leak into the enclosing scope).
                let saved_known = self.known_values.clone();
                // break/continue labels do not cross the fn boundary.
                let saved_loop_labels = std::mem::take(&mut self.loop_labels);
                let saved_break_labels = std::mem::take(&mut self.break_labels);
                let saved_named_labels = std::mem::take(&mut self.named_labels);
                let saved_named_loop_labels = std::mem::take(&mut self.named_loop_labels);
                let pats: Vec<Pat> = fn_decl
                    .function
                    .params
                    .iter()
                    .map(|p| p.pat.clone())
                    .collect();
                self.bind_patterns(&pats);
                let saved_boxed = self.enter_fn_scope(&pats, body_stmts);
                // Param bindings are part of the fn scope: they must see the
                // same boxed set as the body (a param captured by a nested
                // closure and assigned is emitted as a cell).
                let bindings = if self.dynamic_values {
                    Some(self.lower_closure_params(&pats))
                } else {
                    None
                };
                let body = fn_decl
                    .function
                    .body
                    .as_ref()
                    .map(|b| {
                        let prologue = self.hoist_fn_body_vars(&b.stmts);
                        format!("{prologue}{}", self.lower_stmts(&b.stmts))
                    })
                    .unwrap_or_default();
                self.try_flow_stack = saved_flow;
                self.known_values = saved_known;
                self.loop_labels = saved_loop_labels;
                self.break_labels = saved_break_labels;
                self.named_labels = saved_named_labels;
                self.named_loop_labels = saved_named_loop_labels;
                self.leave_fn_scope(saved_boxed);
                if self.dynamic_values {
                    // Nested fn items use the bundle-wide `(__args)` calling
                    // convention so they can be wrapped as Values and called
                    // uniformly (see fn_item_names).
                    self.fn_item_names.insert(name.clone());
                    let bindings = bindings.unwrap_or_default();
                    format!(
                        "{}fn {name}(__args: Vec<w3cos_core::Value>) -> w3cos_core::Value {{\n{}  {bindings}{body}\n{}w3cos_core::Value::Undefined\n{}}}",
                        self.pad(),
                        " ".repeat(self.indent + 4),
                        " ".repeat(self.indent + 4),
                        self.pad()
                    )
                } else {
                    format!(
                        "{}fn {name}({params}) {{\n{body}\n{}}}",
                        self.pad(),
                        self.pad()
                    )
                }
            }
            Decl::Class(class_decl) => {
                if self.dynamic_values {
                    // Nested class declaration → local binding holding the
                    // eagerly-built class value.
                    let name = atom_str(&class_decl.ident.sym);
                    self.known_values.insert(name.clone());
                    let class_expr = ClassExpr {
                        ident: Some(class_decl.ident.clone()),
                        class: class_decl.class.clone(),
                    };
                    let value = self.lower_class_value(&class_expr);
                    if self.hoisted_vars.contains(&name) {
                        // Hoisted class: assign the pre-declared slot so
                        // forward references and method self-references see
                        // the live binding (boxed when captured).
                        if self.is_boxed(&name) {
                            return format!("{}{}", self.pad(), Self::boxed_write(&name, &value));
                        }
                        return format!("{}{name} = {value};", self.pad());
                    }
                    format!("{}{}", self.pad(), self.bind_local(&name, &value))
                } else {
                    format!("{}/* unsupported decl */", self.pad())
                }
            }
            _ => format!("{}/* unsupported decl */", self.pad()),
        }
    }

    /// The `for`-loop pattern plus a destructuring prelude for a
    /// for-in/for-of head: plain idents bind directly in the loop pattern;
    /// destructuring heads bind `__item` and destructure inside the body.
    /// Dynamic mode handles JS scoping faithfully: `var` heads write through
    /// to the hoisted fn-scope slot, `let`/`const` heads get a fresh
    /// per-iteration binding (boxed as a cell when captured+assigned), and
    /// declaration-free heads write through to the existing binding.
    fn lower_for_head(&self, head: &ForHead) -> (String, String) {
        let pat = match head {
            ForHead::VarDecl(vd) => vd.decls.first().map(|d| &d.name),
            ForHead::Pat(p) => Some(p.as_ref()),
            _ => None,
        };
        if !self.dynamic_values {
            return match pat {
                Some(Pat::Ident(_)) | None => {
                    let name = pat
                        .map(|p| self.lower_pat(p))
                        .unwrap_or_else(|| "_".to_string());
                    (name, String::new())
                }
                Some(p) => {
                    let mut lines = Vec::new();
                    self.lower_dynamic_local_pattern(p, "__item", &mut lines, self.indent + 4);
                    ("__item".to_string(), lines.join("\n"))
                }
            };
        }
        let pad = " ".repeat(self.indent + 4);
        match head {
            ForHead::VarDecl(vd) => {
                let Some(declarator) = vd.decls.first() else {
                    return ("_".to_string(), String::new());
                };
                match &declarator.name {
                    Pat::Ident(ident) => {
                        let name = sanitize_ident(&ident.id.sym.to_string());
                        let fn_scoped =
                            vd.kind == VarDeclKind::Var || self.hoisted_vars.contains(&name);
                        if fn_scoped {
                            // `var` loop heads share one fn-scope binding:
                            // write through to the hoisted slot.
                            let write = if self.is_boxed(&name) {
                                Self::boxed_write(&name, "__it")
                            } else {
                                format!("{name} = __it;")
                            };
                            ("__it".to_string(), format!("{pad}{write}"))
                        } else if self.is_boxed(&name) {
                            // Captured+assigned loop variable: per-iteration
                            // cell shared with closures created in the body.
                            (
                                "__it".to_string(),
                                format!(
                                    "{pad}let {name} = std::rc::Rc::new(std::cell::RefCell::new(__it));"
                                ),
                            )
                        } else {
                            // Fresh per-iteration binding; `mut` because JS
                            // loop variables are reassignable in the body.
                            (format!("mut {name}"), String::new())
                        }
                    }
                    pattern => {
                        let mut lines = Vec::new();
                        self.lower_dynamic_local_pattern(
                            pattern,
                            "__item",
                            &mut lines,
                            self.indent + 4,
                        );
                        ("__item".to_string(), lines.join("\n"))
                    }
                }
            }
            ForHead::Pat(p) => match p.as_ref() {
                Pat::Ident(ident) => {
                    let name = sanitize_ident(&ident.id.sym.to_string());
                    let write = if self.is_boxed(&name) {
                        Self::boxed_write(&name, "__it")
                    } else {
                        format!("{name} = __it;")
                    };
                    ("__it".to_string(), format!("{pad}{write}"))
                }
                pattern => {
                    let mut lines = Vec::new();
                    self.lower_dynamic_assign_pattern(pattern, "__item", &mut lines);
                    let prelude = lines
                        .iter()
                        .map(|line| format!("{pad}{line}"))
                        .collect::<Vec<_>>()
                        .join("\n");
                    ("__item".to_string(), prelude)
                }
            },
            _ => ("_".to_string(), String::new()),
        }
    }

    fn lower_if(&mut self, if_stmt: &IfStmt) -> String {
        let mut test = self.lower_expr(&if_stmt.test);
        if self.dynamic_values {
            test = format!("{test}.to_bool()");
        }
        self.indent += 4;
        let cons = self.lower_stmt(&if_stmt.cons);
        self.indent -= 4;
        let mut out = format!("{}if {} {{\n{}\n{}}}", self.pad(), test, cons, self.pad());
        if let Some(alt) = &if_stmt.alt {
            self.indent += 4;
            let alt_code = self.lower_stmt(alt);
            self.indent -= 4;
            out.push_str(&format!(" else {{\n{}\n{}}}", alt_code, self.pad()));
        }
        out
    }

    fn lower_for(&mut self, for_stmt: &ForStmt) -> String {
        let init = for_stmt
            .init
            .as_ref()
            .map(|v| match v {
                VarDeclOrExpr::VarDecl(vd) => {
                    let d = Decl::Var(vd.clone());
                    self.lower_decl(&d).trim().to_string()
                }
                VarDeclOrExpr::Expr(e) => format!("{};", self.lower_expr(e)),
            })
            .unwrap_or_default();
        let test = for_stmt
            .test
            .as_ref()
            .map(|e| self.lower_expr(e))
            .unwrap_or_else(|| "true".to_string());
        let update = for_stmt
            .update
            .as_ref()
            .map(|e| self.lower_expr(e))
            .unwrap_or_default();
        let label = if self.dynamic_values {
            self.take_loop_label()
        } else {
            String::new()
        };
        self.indent += 4;
        let body = if self.dynamic_values {
            self.lower_loop_body(&for_stmt.body, &label.clone(), &label.clone())
        } else {
            self.lower_stmt(&for_stmt.body)
        };
        self.indent -= 4;

        let inner_pad = " ".repeat(self.indent + 4);
        let mut out = String::new();
        // Emit init before the loop
        if !init.is_empty() {
            out.push_str(&format!("{}{}\n", self.pad(), init));
        }
        let (prefix, break_) = if label.is_empty() {
            (String::new(), "break;".to_string())
        } else {
            (format!("'{label}: "), format!("break '{label};"))
        };
        // JS `continue` must run the update before re-testing. Rust
        // `continue` jumps to the loop head, so the update lives at the head
        // guarded by a first-iteration flag (emitted only when there IS an
        // update; otherwise plain `continue` is already correct).
        let first_flag = self.dynamic_values && !update.is_empty();
        if first_flag {
            out.push_str(&format!("{}let mut __w3cos_first = true;\n", self.pad()));
        }
        out.push_str(&format!("{}{prefix}loop {{\n", self.pad()));
        if first_flag {
            out.push_str(&format!(
                "{inner_pad}if !__w3cos_first {{ {update}; }}\n{inner_pad}__w3cos_first = false;\n"
            ));
        }
        // Emit break condition
        if test != "true" {
            let condition = if self.dynamic_values {
                format!("{test}.to_bool()")
            } else {
                test
            };
            out.push_str(&format!("{inner_pad}if !({condition}) {{ {break_} }}\n"));
        }
        out.push_str(&body);
        out.push('\n');
        // Emit update at the tail when there is no continue-flag to run it.
        if !first_flag && !update.is_empty() {
            out.push_str(&format!("{inner_pad}{update};\n"));
        }
        out.push_str(&format!("{}}}", self.pad()));
        out
    }

    fn lower_try(&mut self, try_stmt: &TryStmt) -> String {
        if !self.dynamic_values {
            return self.lower_try_static(try_stmt);
        }
        // JS try/catch/finally on top of panic unwinding:
        //
        // ```text
        // enum __FlowN { Done, Return(Value), Break(&str), Continue(&str), Throw(...) }
        // let __flowN = (|| -> __FlowN {
        //     match catch_unwind(|| { <try body>; __FlowN::Done }) {
        //         Ok(flow) => flow,
        //         Err(payload) => { bind catch param; catch body as flow }
        //     }
        // })();
        // <finally body>                        // always runs
        // match __flowN { Done => {}, Return(v) => propagate, Throw(p) => resume_unwind(p) }
        // ```
        //
        // `return` inside try/catch lowers to `return __FlowN::Return(v)` from
        // the flow closure (see Stmt::Return), so finally runs on every path;
        // the epilogue then propagates the early return (or re-throws, which
        // an enclosing try catches as its own payload — nested try works).
        let index = self.temp_index;
        self.temp_index += 1;
        let flow = format!("__Flow{index}");
        let pad0 = self.pad();
        let pad1 = " ".repeat(self.indent + 4);
        let pad2 = " ".repeat(self.indent + 8);
        let pad3 = " ".repeat(self.indent + 12);
        let pad4 = " ".repeat(self.indent + 16);
        let outer_break_label = self.break_labels.last().cloned();
        let outer_continue_label = self.loop_labels.last().cloned();
        let outer_named_labels = self.named_labels.clone();
        let outer_named_loop_labels = self.named_loop_labels.clone();

        let mut out = String::new();
        if try_has_await(try_stmt) {
            // AssertUnwindSafe across .await is unsound (a future may be held
            // across the unwind boundary); sync try/catch is sound.
            out.push_str(&format!(
                "{pad0}// compile_warning: try/catch around `await` is best-effort only\n"
            ));
        }
        out.push_str(&format!(
            "{pad0}{{\n{pad1}enum {flow} {{ Done, Return(w3cos_core::Value), Break(&'static str), Continue(&'static str), Throw(std::boxed::Box<dyn std::any::Any + Send>) }}\n{pad1}let __flow{index}: {flow} = (|| -> {flow} {{\n{pad2}let __caught{index} = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> {flow} {{\n"
        ));

        self.try_flow_stack.push(flow.clone());
        self.indent += 12;
        // The try body runs inside the flow closure: break/continue labels
        // from enclosing loops do not cross the closure boundary.
        let saved_loop_labels = std::mem::take(&mut self.loop_labels);
        let saved_break_labels = std::mem::take(&mut self.break_labels);
        let saved_named_labels = std::mem::take(&mut self.named_labels);
        let saved_named_loop_labels = std::mem::take(&mut self.named_loop_labels);
        let try_body = self.lower_stmts(&try_stmt.block.stmts);
        self.loop_labels = saved_loop_labels;
        self.break_labels = saved_break_labels;
        self.named_labels = saved_named_labels;
        self.named_loop_labels = saved_named_loop_labels;
        self.indent -= 12;
        out.push_str(&try_body);
        out.push_str(&format!(
            "\n{pad3}{flow}::Done\n{pad2}}}));\n{pad2}match __caught{index} {{\n{pad3}Ok(flow) => flow,\n{pad3}Err(__payload{index}) => {{\n"
        ));

        if let Some(handler) = &try_stmt.handler {
            // Mirror w3cos_core's payload_to_value: JS exceptions thrown by
            // compiled code or builtins arrive as PanicValue; native Rust
            // panics degrade to a string value.
            let payload_value = format!(
                "{{ if let Some(__v) = __payload{index}.downcast_ref::<w3cos_core::Value>() {{ __v.clone() }} else if let Some(__w) = __payload{index}.downcast_ref::<w3cos_core::PanicValue>() {{ __w.0.clone() }} else if let Some(__s) = __payload{index}.downcast_ref::<&'static str>() {{ w3cos_core::Value::from(*__s) }} else if let Some(__s) = __payload{index}.downcast_ref::<String>() {{ w3cos_core::Value::from(__s.clone()) }} else {{ w3cos_core::Value::from(\"native panic\") }} }}"
            );
            let saved_known = self.known_values.clone();
            match &handler.param {
                Some(Pat::Ident(ident)) => {
                    let name = sanitize_ident(&ident.id.sym.to_string());
                    out.push_str(&format!(
                        "{pad4}{}\n",
                        self.bind_local(&name, &payload_value)
                    ));
                    self.known_values.insert(name);
                }
                Some(pattern) => {
                    let err = format!("__err{index}");
                    out.push_str(&format!("{pad4}let {err} = {payload_value};\n"));
                    let mut lines = Vec::new();
                    self.lower_dynamic_local_pattern(pattern, &err, &mut lines, self.indent + 16);
                    for line in lines {
                        out.push_str(&line);
                        out.push('\n');
                    }
                    collect_pattern_names(pattern, &mut self.known_values);
                }
                None => {}
            }
            out.push_str(&format!(
                "{pad4}let __caught2{index} = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> {flow} {{\n"
            ));
            self.indent += 20;
            // The catch body also runs inside a flow closure.
            let saved_loop_labels = std::mem::take(&mut self.loop_labels);
            let saved_break_labels = std::mem::take(&mut self.break_labels);
            let saved_named_labels = std::mem::take(&mut self.named_labels);
            let saved_named_loop_labels = std::mem::take(&mut self.named_loop_labels);
            let catch_body = self.lower_stmts(&handler.body.stmts);
            self.loop_labels = saved_loop_labels;
            self.break_labels = saved_break_labels;
            self.named_labels = saved_named_labels;
            self.named_loop_labels = saved_named_loop_labels;
            self.indent -= 20;
            self.known_values = saved_known;
            out.push_str(&catch_body);
            out.push_str(&format!(
                "\n{}__Flow{}::Done\n{pad4}}}));\n{pad4}match __caught2{index} {{\n{}Ok(flow) => flow,\n{}Err(__rethrown{index}) => {flow}::Throw(__rethrown{index}),\n{pad4}}}\n",
                " ".repeat(self.indent + 20),
                index,
                " ".repeat(self.indent + 20),
                " ".repeat(self.indent + 20),
            ));
        } else {
            // No catch: finally runs, then the panic resumes.
            out.push_str(&format!("{pad4}{flow}::Throw(__payload{index})\n"));
        }
        out.push_str(&format!("{pad3}}}\n{pad2}}}\n{pad1}}})();\n"));
        self.try_flow_stack.pop();

        // Finally runs on every path (its own `return`/throw wins naturally:
        // it is emitted outside the flow closure).
        if let Some(finalizer) = &try_stmt.finalizer {
            self.indent += 4;
            let finally_body = self.lower_stmts(&finalizer.stmts);
            self.indent -= 4;
            out.push_str(&finally_body);
            out.push('\n');
        }

        let propagate = match self.try_flow_stack.last() {
            Some(outer) => format!("return {outer}::Return(v);"),
            None => "return v;".to_string(),
        };
        let propagate_break = match self.try_flow_stack.last() {
            Some(outer) => lower_flow_control_target_or(
                "break",
                outer_break_label.as_deref(),
                &outer_named_labels,
                &format!("return {outer}::Break(label);"),
            ),
            None => lower_flow_control_target(
                "break",
                outer_break_label.as_deref(),
                &outer_named_labels,
            ),
        };
        let propagate_continue = match self.try_flow_stack.last() {
            Some(outer) => lower_flow_control_target_or(
                "continue",
                outer_continue_label.as_deref(),
                &outer_named_loop_labels,
                &format!("return {outer}::Continue(label);"),
            ),
            None => lower_flow_control_target(
                "continue",
                outer_continue_label.as_deref(),
                &outer_named_loop_labels,
            ),
        };
        out.push_str(&format!(
            "{pad1}match __flow{index} {{\n{pad2}{flow}::Done => {{}}\n{pad2}{flow}::Return(v) => {{ {propagate} }}\n{pad2}{flow}::Break(label) => {{ {propagate_break} }}\n{pad2}{flow}::Continue(label) => {{ {propagate_continue} }}\n{pad2}{flow}::Throw(p) => {{ std::panic::resume_unwind(p); }}\n{pad1}}}\n{pad0}}}"
        ));
        out
    }

    /// Static (non-dynamic-Value) fallback: try/catch has no Rust equivalent
    /// in that mode; emit the bodies inline with markers.
    fn lower_try_static(&mut self, try_stmt: &TryStmt) -> String {
        self.indent += 4;
        let try_body = self.lower_stmts(&try_stmt.block.stmts);
        self.indent -= 4;
        let mut out = format!(
            "{}// try\n{}{{\n{}\n{}}}",
            self.pad(),
            self.pad(),
            try_body,
            self.pad()
        );
        if let Some(handler) = &try_stmt.handler {
            let param = handler
                .param
                .as_ref()
                .map(|p| self.lower_pat(p))
                .unwrap_or_else(|| "_e".to_string());
            self.indent += 4;
            let catch_body = self.lower_stmts(&handler.body.stmts);
            self.indent -= 4;
            out.push_str(&format!(
                "\n{}// catch ({param})\n{}{{\n{}\n{}}}",
                self.pad(),
                self.pad(),
                catch_body,
                self.pad()
            ));
        }
        if let Some(finalizer) = &try_stmt.finalizer {
            self.indent += 4;
            let fin_body = self.lower_stmts(&finalizer.stmts);
            self.indent -= 4;
            out.push_str(&format!(
                "\n{}// finally\n{}{{\n{}\n{}}}",
                self.pad(),
                self.pad(),
                fin_body,
                self.pad()
            ));
        }
        out
    }

    fn lower_switch(&mut self, switch: &SwitchStmt) -> String {
        let disc = self.lower_expr(&switch.discriminant);
        if self.dynamic_values {
            let index = self.temp_index;
            self.temp_index += 1;
            let sw = format!("__sw{index}");
            let selected = format!("__case{index}");
            let pad = self.pad();
            let mut out = format!(
                "{pad}'{sw}: {{\n{pad}    let __disc = {disc};\n{pad}    let mut {selected}: i32 = -1;\n"
            );
            let default_index = switch.cases.iter().position(|case| case.test.is_none());
            for (case_index, case) in switch.cases.iter().enumerate() {
                if let Some(test) = &case.test {
                    let test = self.lower_expr(test);
                    out.push_str(&format!(
                        "{pad}    if {selected} < 0 && __disc.strict_eq(&{test}) {{ {selected} = {case_index}; }}\n"
                    ));
                }
            }
            if let Some(default_index) = default_index {
                out.push_str(&format!(
                    "{pad}    if {selected} < 0 {{ {selected} = {default_index}; }}\n"
                ));
            }
            self.indent += 4;
            // A `break` inside a case targets the switch; a `continue`
            // targets the enclosing loop (its label stays on the loop stack).
            self.break_labels.push(sw.clone());
            for (case_index, case) in switch.cases.iter().enumerate() {
                let body = self.lower_stmts(&case.cons);
                out.push_str(&format!(
                    "{}if {selected} >= 0 && {selected} <= {case_index} {{\n{body}\n{}}}\n",
                    self.pad(),
                    self.pad()
                ));
            }
            self.break_labels.pop();
            self.indent -= 4;
            out.push_str(&format!("{pad}}}"));
            return out;
        }
        let mut out = format!("{}match {} {{\n", self.pad(), disc);
        self.indent += 4;
        for case in &switch.cases {
            let pat = match &case.test {
                Some(test) => self.lower_expr(test),
                None => "_".to_string(),
            };
            let body = self.lower_stmts(&case.cons);
            out.push_str(&format!(
                "{}{} => {{\n{}\n{}}}\n",
                self.pad(),
                pat,
                body,
                self.pad()
            ));
        }
        self.indent -= 4;
        out.push_str(&format!("{}}}", self.pad()));
        out
    }

    pub fn lower_expr(&self, expr: &Expr) -> String {
        match expr {
            Expr::Ident(ident) => {
                // `self` sanitizes to `self_` (Rust keyword), so the globals
                // table in resolve_value would miss it; resolve the raw name.
                if self.dynamic_values
                    && ident.sym == *"self"
                    && !self.is_name_shadowed("self")
                    && !self.is_name_shadowed("self_")
                {
                    return "w3cos_runtime::jsdom::window_value()".to_string();
                }
                self.resolve_value(&atom_str(&ident.sym))
            }
            Expr::Lit(lit) => self.lower_lit(lit),
            Expr::New(new_expr) => {
                let args = new_expr
                    .args
                    .as_ref()
                    .map(|a| {
                        a.iter()
                            .map(|arg| self.lower_argument(&arg.expr))
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();
                if self.dynamic_values {
                    let callee_name = match new_expr.callee.as_ref() {
                        Expr::Ident(identifier) => Some(atom_str(&identifier.sym)),
                        _ => None,
                    };
                    // Keep builtin special-cases (native Rust constructors).
                    if let Some(name) = &callee_name {
                        let resolved = self.resolve_name(name);
                        // Map/Set/WeakMap/WeakSet are NOT special-cased: when
                        // unshadowed they resolve (below) to the
                        // w3cos_core::collections class values and go through
                        // class::construct like any other class.
                        if matches!(resolved.as_str(), "Error" | "ResizeObserver") {
                            // `Error::new` returns the struct — unwrap the Value.
                            let unwrap = if resolved == "Error" { ".0" } else { "" };
                            return format!("{resolved}::new(vec![{args}]){unwrap}");
                        }
                        // Unshadowed globals with dedicated core constructors.
                        if !self.is_name_shadowed(name) {
                            match name.as_str() {
                                "Promise" => {
                                    return format!("w3cos_core::promise::new(vec![{args}])");
                                }
                                // `new Array(n)` (length) vs `new Array(a,b)`
                                // (elements) per JS semantics.
                                "Array" => {
                                    return format!(
                                        "{{ let __args: Vec<w3cos_core::Value> = vec![{args}]; if __args.len() == 1 && __args[0].is_number() {{ let n = __args[0].to_number() as usize; w3cos_core::Value::array(vec![w3cos_core::Value::Undefined; n]) }} else {{ w3cos_core::Value::array(__args) }} }}"
                                    );
                                }
                                "URL" => {
                                    return format!("w3cos_core::web::url_new(vec![{args}])");
                                }
                                "URLSearchParams" => {
                                    return format!(
                                        "w3cos_core::web::url_search_params_new(vec![{args}])"
                                    );
                                }
                                // Error family maps onto the Error builtin
                                // (type tags are not modeled); `Error::new`
                                // returns the struct, so unwrap the Value.
                                "TypeError" | "SyntaxError" | "ReferenceError" | "EvalError"
                                | "URIError" | "AggregateError" => {
                                    return format!("Error::new(vec![{args}]).0");
                                }
                                _ => {}
                            }
                        }
                    }
                    // Everything else — classes (own or cross-module), class
                    // expressions, constructor functions — goes through the
                    // class runtime's construct().
                    let callee_value = match &callee_name {
                        Some(name) => self.resolve_value(name),
                        None => self.lower_expr(&new_expr.callee),
                    };
                    format!("w3cos_core::class::construct(&{callee_value}, vec![{args}])")
                } else {
                    let callee = match new_expr.callee.as_ref() {
                        Expr::Ident(identifier) => self.resolve_name(&atom_str(&identifier.sym)),
                        expression => self.lower_expr(expression),
                    };
                    format!("{callee}::new({args})")
                }
            }
            Expr::Call(call) => self.lower_call(call),
            Expr::Member(member) => self.lower_member(member),
            Expr::Assign(assign) => {
                if self.dynamic_values
                    && let AssignTarget::Simple(SimpleAssignTarget::Member(member)) = &assign.left
                {
                    let object = self.lower_expr(&member.obj);
                    let key = match &member.prop {
                        MemberProp::Ident(ident) => format!("{:?}", ident.sym.to_string()),
                        MemberProp::Computed(computed) => {
                            format!("{}.to_js_string()", self.lower_expr(&computed.expr))
                        }
                        MemberProp::PrivateName(name) => {
                            format!("{:?}", self.private_key(name))
                        }
                    };
                    let right = self.lower_expr(&assign.right);
                    if let Some(method) = compound_assign_op(assign.op) {
                        return format!(
                            "{{ let __obj = {object}; let __w3cos_av = __obj.get_property(&{key}).{method}(&{right}); __obj.set_property(&{key}, __w3cos_av.clone()); __w3cos_av }}"
                        );
                    }
                    return format!(
                        "{{ let __w3cos_av = {right}; {object}.set_property(&{key}, __w3cos_av.clone()); __w3cos_av }}"
                    );
                }
                if self.dynamic_values
                    && let AssignTarget::Simple(SimpleAssignTarget::Ident(identifier)) =
                        &assign.left
                {
                    let local = atom_str(&identifier.id.sym);
                    if self.value_bindings.contains(&local) && !self.known_values.contains(&local) {
                        let bundled = self
                            .renames
                            .iter()
                            .find(|(name, _)| name == &local)
                            .map(|(_, bundled)| bundled.as_str())
                            .unwrap_or(&local);
                        let right = self.lower_expr(&assign.right);
                        if let Some(method) = compound_assign_op(assign.op) {
                            return format!("{bundled}_set({bundled}_get().{method}(&{right}))");
                        }
                        return format!("{bundled}_set({right})");
                    }
                    if self.is_boxed(&local) && self.known_values.contains(&local) {
                        // Rc<RefCell> local: write through the shared cell.
                        let target = self.resolve_name(&local);
                        let right = self.lower_expr(&assign.right);
                        if let Some(method) = compound_assign_op(assign.op) {
                            return format!(
                                "{{ let __w3cos_av = (*{target}.borrow()).{method}(&{right}); *{target}.borrow_mut() = __w3cos_av.clone(); __w3cos_av }}"
                            );
                        }
                        return format!(
                            "{{ let __w3cos_av = {right}; *{target}.borrow_mut() = __w3cos_av.clone(); __w3cos_av }}"
                        );
                    }
                    if !self.known_values.contains(&local) {
                        // Assignment to a non-local, non-cell binding: a
                        // module-level fn/class/namespace accessor (not
                        // assignable in Rust), an import (read-only), or an
                        // implicit global (sloppy-mode UMD guards). Evaluate
                        // the RHS (and the current value for compound ops)
                        // and drop the write — documented degradation that
                        // keeps emission total.
                        let right = self.lower_expr(&assign.right);
                        if let Some(method) = compound_assign_op(assign.op) {
                            let target = self.resolve_value(&local);
                            return format!(
                                "{{ let __w3cos_av = {target}.{method}(&{right}); __w3cos_av }} /* write dropped: non-assignable binding */"
                            );
                        }
                        return format!(
                            "{{ let __w3cos_av = {right}; __w3cos_av }} /* write dropped: non-assignable binding */"
                        );
                    }
                    if let Some(method) = compound_assign_op(assign.op) {
                        let target = self.resolve_name(&local);
                        let right = self.lower_expr(&assign.right);
                        return format!(
                            "{{ let __w3cos_av = {target}.{method}(&{right}); {target} = __w3cos_av.clone(); __w3cos_av }}"
                        );
                    }
                }
                // `super.x = v` — define on the parent prototype chain is not
                // meaningful here; keep total by evaluating both sides.
                if self.dynamic_values
                    && let AssignTarget::Simple(SimpleAssignTarget::SuperProp(_)) = &assign.left
                {
                    let right = self.lower_expr(&assign.right);
                    return format!("{{ let value = {right}; value }} /* super.x = unsupported */");
                }
                let left = match &assign.left {
                    AssignTarget::Simple(simple) => match simple {
                        SimpleAssignTarget::Ident(i) => self.resolve_name(&atom_str(&i.id.sym)),
                        SimpleAssignTarget::Member(m) => self.lower_member(m),
                        _ => {
                            if self.dynamic_values {
                                let right = self.lower_expr(&assign.right);
                                return format!(
                                    "{{ let value = {right}; value }} /* unsupported assign target */"
                                );
                            }
                            "/* assign target */".to_string()
                        }
                    },
                    AssignTarget::Pat(_) => {
                        if self.dynamic_values {
                            // Destructuring reassignment (`[a, b] = arr`):
                            // evaluate the rhs once, then assign each element
                            // to its (already declared) binding.
                            let right = self.lower_expr(&assign.right);
                            let mut lines = Vec::new();
                            self.lower_dynamic_assign_target(
                                &assign.left,
                                "__assign_value",
                                &mut lines,
                            );
                            return format!(
                                "{{ let __assign_value = {right}; {} __assign_value }}",
                                lines.join(" ")
                            );
                        }
                        "/* pattern assign */".to_string()
                    }
                };
                let right = self.lower_expr(&assign.right);
                if self.dynamic_values {
                    format!(
                        "{{ let __w3cos_av = {right}; {left} = __w3cos_av.clone(); __w3cos_av }}"
                    )
                } else {
                    format!("{left} = {right}")
                }
            }
            Expr::Bin(bin) => {
                let left = self.lower_expr(&bin.left);
                let right = self.lower_expr(&bin.right);
                if self.dynamic_values {
                    lower_dynamic_bin_op(bin.op, &left, &right)
                } else {
                    let op = lower_bin_op(bin.op);
                    format!("{left} {op} {right}")
                }
            }
            Expr::Unary(unary) => {
                if self.dynamic_values && unary.op == UnaryOp::TypeOf {
                    if let Expr::Ident(identifier) = unary.arg.as_ref() {
                        match atom_str(&identifier.sym).as_str() {
                            "ResizeObserver" => {
                                return "w3cos_core::Value::from(\"function\")".to_string();
                            }
                            // Native W3COS is a browser host, not an SSR environment.
                            // Libraries such as react-window select useLayoutEffect
                            // through `typeof window !== "undefined"`.
                            "window" => {
                                return "w3cos_core::Value::from(\"object\")".to_string();
                            }
                            _ => {}
                        }
                    }
                }
                if self.dynamic_values {
                    if unary.op == UnaryOp::Delete {
                        if let Expr::Member(member) = unary.arg.as_ref() {
                            let object = self.lower_expr(&member.obj);
                            let key = match &member.prop {
                                MemberProp::Ident(ident) => {
                                    format!("{:?}", ident.sym.to_string())
                                }
                                MemberProp::Computed(computed) => {
                                    format!("{}.to_js_string()", self.lower_expr(&computed.expr))
                                }
                                MemberProp::PrivateName(name) => {
                                    format!("{:?}", self.private_key(name))
                                }
                            };
                            return format!("{object}.delete_property(&{key})");
                        }
                        return "w3cos_core::Value::Bool(true)".to_string();
                    }
                    let arg = self.lower_expr(&unary.arg);
                    match unary.op {
                        UnaryOp::Bang => format!("{arg}.js_not()"),
                        UnaryOp::Minus => format!("{arg}.js_neg()"),
                        UnaryOp::Tilde => format!("{arg}.js_bitnot()"),
                        UnaryOp::TypeOf => format!("w3cos_core::type_of(&{arg})"),
                        UnaryOp::Void => "w3cos_core::Value::Undefined".to_string(),
                        _ => format!("{arg}"),
                    }
                } else {
                    let arg = self.lower_expr(&unary.arg);
                    let op = lower_unary_op(unary.op);
                    format!("{op}{arg}")
                }
            }
            Expr::Update(update) => {
                if self.dynamic_values {
                    let delta = if update.op == UpdateOp::PlusPlus {
                        "js_add"
                    } else {
                        "js_sub"
                    };
                    if let Expr::Member(member) = update.arg.as_ref() {
                        let object = self.lower_expr(&member.obj);
                        let key = match &member.prop {
                            MemberProp::Ident(ident) => format!("{:?}", ident.sym.to_string()),
                            MemberProp::Computed(computed) => {
                                format!("{}.to_js_string()", self.lower_expr(&computed.expr))
                            }
                            MemberProp::PrivateName(name) => {
                                format!("{:?}", self.private_key(name))
                            }
                        };
                        return format!(
                            "{{ let __obj = {object}; let __w3cos_prev = __obj.get_property(&{key}); __obj.set_property(&{key}, __w3cos_prev.{delta}(&w3cos_core::Value::Number(1.0))); __w3cos_prev }}"
                        );
                    }
                    let arg = match update.arg.as_ref() {
                        Expr::Ident(identifier) => {
                            let local = atom_str(&identifier.sym);
                            if self.value_bindings.contains(&local)
                                && !self.known_values.contains(&local)
                            {
                                // x++ on an imported/module-level variable:
                                // go through the `{bundled}_get`/`_set`
                                // accessors (can't assign to a call result).
                                let bundled = self
                                    .renames
                                    .iter()
                                    .find(|(name, _)| name == &local)
                                    .map(|(_, bundled)| bundled.as_str())
                                    .unwrap_or(&local);
                                return format!(
                                    "{{ let __w3cos_prev = {bundled}_get(); {bundled}_set(__w3cos_prev.{delta}(&w3cos_core::Value::Number(1.0))); __w3cos_prev }}"
                                );
                            }
                            if self.is_boxed(&local) && self.known_values.contains(&local) {
                                // Rc<RefCell> local: read/write through the cell.
                                let target = self.resolve_name(&local);
                                return format!(
                                    "{{ let __w3cos_prev = (*{target}.borrow()).clone(); *{target}.borrow_mut() = __w3cos_prev.{delta}(&w3cos_core::Value::Number(1.0)); __w3cos_prev }}"
                                );
                            }
                            self.resolve_name(&local)
                        }
                        expression => self.lower_expr(expression),
                    };
                    return format!(
                        "{{ let __w3cos_prev = {arg}.clone(); {arg} = {arg}.{delta}(&w3cos_core::Value::Number(1.0)); __w3cos_prev }}"
                    );
                }
                let arg = self.lower_expr(&update.arg);
                let op = if update.op == UpdateOp::PlusPlus {
                    "+= 1"
                } else {
                    "-= 1"
                };
                format!("{arg} {op}")
            }
            Expr::Paren(paren) => {
                let inner = self.lower_expr(&paren.expr);
                format!("({inner})")
            }
            Expr::Arrow(arrow) => {
                if self.dynamic_values {
                    let parameter_names = pattern_names(&arrow.params);
                    let referenced = match arrow.body.as_ref() {
                        BlockStmtOrExpr::Expr(expression) => expr_referenced_names(expression),
                        BlockStmtOrExpr::BlockStmt(block) => stmts_referenced_names(&block.stmts),
                    };
                    let mut captures =
                        capture_names(&self.known_values, &parameter_names, &referenced);
                    self.push_parent_capture(&mut captures);
                    let lower_body = |captures: &[String]| match arrow.body.as_ref() {
                        BlockStmtOrExpr::Expr(expression) => {
                            let mut ctx = self.child_dynamic_ctx();
                            ctx.known_values.extend(captures.iter().cloned());
                            ctx.bind_patterns(&arrow.params);
                            // Analyze the expression body so params captured
                            // by a nested closure and assigned get boxed.
                            let analysis_body = [Stmt::Return(ReturnStmt {
                                span: arrow.span,
                                arg: Some(expression.clone()),
                            })];
                            ctx.enter_fn_scope(&arrow.params, &analysis_body);
                            let bindings = ctx.lower_closure_params(&arrow.params);
                            (bindings, format!("return {};", ctx.lower_expr(expression)))
                        }
                        BlockStmtOrExpr::BlockStmt(block) => {
                            let mut ctx = self.child_dynamic_ctx();
                            ctx.known_values.extend(captures.iter().cloned());
                            ctx.bind_patterns(&arrow.params);
                            ctx.indent = self.indent + 4;
                            ctx.enter_fn_scope(&arrow.params, &block.stmts);
                            let bindings = ctx.lower_closure_params(&arrow.params);
                            let prologue = ctx.hoist_fn_body_vars(&block.stmts);
                            (
                                bindings,
                                format!("{prologue}{}", ctx.lower_stmts(&block.stmts)),
                            )
                        }
                    };
                    let (mut bindings, mut body) = lower_body(&captures);
                    // The AST reference walker intentionally stays lightweight and
                    // can miss identifiers nested in newer TS/JSX wrapper nodes.
                    // Reconcile against the emitted Rust, then lower once more so
                    // newly discovered captures use `.clone()` at value positions.
                    let candidates = self
                        .known_values
                        .difference(&parameter_names)
                        .cloned()
                        .collect::<Vec<_>>();
                    let lowered = format!("{bindings}{body}");
                    for capture in referenced_captures(candidates, &lowered) {
                        if !captures.contains(&capture) {
                            captures.push(capture);
                        }
                    }
                    captures.sort();
                    (bindings, body) = lower_body(&captures);
                    let capture_bindings = captures
                        .iter()
                        .map(|name| format!("let mut {name} = {name}.clone(); "))
                        .collect::<String>();
                    // Arrows capture the lexical `this` of the enclosing class
                    // member when (and only when) the body references it.
                    let this_capture = if self.this_name.is_some() && body.contains("__this") {
                        "let mut __this = __this.clone(); "
                    } else {
                        ""
                    };
                    return format!(
                        "{{ {capture_bindings}{this_capture} w3cos_core::Value::function(move |_this, __args| {{ {bindings}{body} w3cos_core::Value::Undefined }}) }}"
                    );
                }
                let params = arrow
                    .params
                    .iter()
                    .map(|p| self.lower_pat(p))
                    .collect::<Vec<_>>()
                    .join(", ");
                let body = match arrow.body.as_ref() {
                    BlockStmtOrExpr::Expr(e) => self.lower_expr(e),
                    BlockStmtOrExpr::BlockStmt(b) => {
                        let mut ctx = LowerCtx::new(self.renames.clone());
                        ctx.indent = self.indent + 4;
                        ctx.lower_stmts(&b.stmts)
                    }
                };
                format!("|{params}| {{ {body} }}")
            }
            Expr::Object(obj) => {
                if obj.props.is_empty() {
                    return "w3cos_core::js_object! {}".to_string();
                }
                if obj
                    .props
                    .iter()
                    .any(|property| matches!(property, PropOrSpread::Spread(_)))
                {
                    let parts = obj
                        .props
                        .iter()
                        .filter_map(|property| match property {
                            PropOrSpread::Spread(spread) => Some(self.lower_expr(&spread.expr)),
                            PropOrSpread::Prop(property) => match property.as_ref() {
                                Prop::KeyValue(key_value) => Some(format!(
                                    "w3cos_core::js_object! {{ {} => {} }}",
                                    self.lower_object_key(&key_value.key),
                                    self.lower_expr(&key_value.value)
                                )),
                                Prop::Shorthand(identifier) => {
                                    let key = identifier.sym.to_string();
                                    let name = atom_str(&identifier.sym);
                                    Some(format!(
                                        "w3cos_core::js_object! {{ {key:?} => {} }}",
                                        self.resolve_value(&name)
                                    ))
                                }
                                _ => None,
                            },
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    return format!("w3cos_core::Value::object_from_parts(vec![{parts}])");
                }
                let fields = obj
                    .props
                    .iter()
                    .filter_map(|prop| match prop {
                        PropOrSpread::Prop(p) => match p.as_ref() {
                            Prop::KeyValue(kv) => {
                                let key = self.lower_object_key(&kv.key);
                                let val = self.lower_expr(&kv.value);
                                Some(format!("{key} => {val}"))
                            }
                            Prop::Shorthand(ident) => {
                                let key = ident.sym.to_string();
                                let name = atom_str(&ident.sym);
                                Some(format!("{key:?} => {}", self.resolve_value(&name)))
                            }
                            Prop::Method(method) => {
                                let key = self.lower_object_key(&method.key);
                                let params = method
                                    .function
                                    .params
                                    .iter()
                                    .map(|parameter| parameter.pat.clone())
                                    .collect::<Vec<_>>();
                                let body = method
                                    .function
                                    .body
                                    .as_ref()
                                    .map(|body| body.stmts.as_slice())
                                    .unwrap_or_default();
                                Some(format!(
                                    "{key} => {}",
                                    self.lower_dynamic_function_value(&params, body)
                                ))
                            }
                            Prop::Getter(getter) => {
                                let key = match &getter.key {
                                    PropName::Ident(identifier) => {
                                        format!(
                                            "{:?}",
                                            format!("__w3cos_getter_{}", identifier.sym)
                                        )
                                    }
                                    _ => return None,
                                };
                                let body = getter
                                    .body
                                    .as_ref()
                                    .map(|body| body.stmts.as_slice())
                                    .unwrap_or_default();
                                Some(format!(
                                    "{key} => {}",
                                    self.lower_dynamic_function_value(&[], body)
                                ))
                            }
                            _ => None,
                        },
                        PropOrSpread::Spread(_) => None,
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("w3cos_core::js_object! {{ {fields} }}")
            }
            Expr::Array(arr) => {
                if arr
                    .elems
                    .iter()
                    .flatten()
                    .any(|element| element.spread.is_some())
                {
                    let mut statements = Vec::new();
                    for element in arr.elems.iter().flatten() {
                        let value = self.lower_expr(&element.expr);
                        if element.spread.is_some() {
                            if self.dynamic_values {
                                statements
                                    .push(format!("__w3cos_array_items.extend(({value}).iter());"));
                            } else {
                                statements.push(format!(
                                    "__w3cos_array_items.extend(({value}).into_iter());"
                                ));
                            }
                        } else {
                            statements.push(format!("__w3cos_array_items.push({value});"));
                        }
                    }
                    let finish = if self.dynamic_values {
                        "w3cos_core::Value::array(__w3cos_array_items)"
                    } else {
                        "__w3cos_array_items"
                    };
                    return format!(
                        "{{ let mut __w3cos_array_items = Vec::new(); {} {finish} }}",
                        statements.join(" ")
                    );
                }
                let items = arr
                    .elems
                    .iter()
                    .filter_map(|e| e.as_ref().map(|el| self.lower_expr(&el.expr)))
                    .collect::<Vec<_>>()
                    .join(", ");
                if self.dynamic_values {
                    format!("w3cos_core::Value::array(vec![{items}])")
                } else {
                    format!("vec![{items}]")
                }
            }
            Expr::Tpl(tpl) => {
                let mut value = "w3cos_core::Value::from(\"\")".to_string();
                for (i, quasi) in tpl.quasis.iter().enumerate() {
                    let raw = quasi.raw.to_string();
                    if !raw.is_empty() {
                        value = format!("{value}.js_add(&w3cos_core::Value::from({raw:?}))");
                    }
                    if i < tpl.exprs.len() {
                        value = format!("{value}.js_add(&{})", self.lower_expr(&tpl.exprs[i]));
                    }
                }
                if self.dynamic_values {
                    value
                } else {
                    format!("{value}.to_js_string()")
                }
            }
            Expr::This(_) => {
                if self.this_name.is_none() && self.dynamic_values {
                    // Top-level/module-scope `this` (undefined in ESM).
                    "w3cos_core::Value::Undefined".to_string()
                } else if self.dynamic_values {
                    // Value position: clone (`__this` is frequently a captured
                    // variable in an `Fn` closure, where moving out is an
                    // error; Value is Rc-cheap to clone).
                    format!(
                        "{}.clone()",
                        self.this_name.clone().unwrap_or_else(|| "self".to_string())
                    )
                } else {
                    self.this_name.clone().unwrap_or_else(|| "self".to_string())
                }
            }
            Expr::SuperProp(super_prop) => {
                if self.dynamic_values {
                    let this = self.this_expr();
                    return match (&self.class_scope, &super_prop.prop) {
                        (Some(scope), _) if scope.parent.is_none() => {
                            "w3cos_core::Value::Undefined /* super without parent */".to_string()
                        }
                        (Some(scope), SuperProp::Ident(ident)) => {
                            let parent = scope.parent.clone().unwrap_or_default();
                            let key = atom_str(&ident.sym);
                            if scope.is_static {
                                // `super.x` in a static member: parent class object.
                                format!("{parent}.get_property({key:?})")
                            } else {
                                format!("w3cos_core::class::super_get(&{this}, &{parent}, {key:?})")
                            }
                        }
                        (Some(scope), SuperProp::Computed(computed)) => {
                            let parent = scope.parent.clone().unwrap_or_default();
                            let key =
                                format!("&{}.to_js_string()", self.lower_expr(&computed.expr));
                            if scope.is_static {
                                format!("{parent}.get_property({key})")
                            } else {
                                format!("w3cos_core::class::super_get(&{this}, &{parent}, {key})")
                            }
                        }
                        (None, _) => {
                            "w3cos_core::Value::Undefined /* super outside class */".to_string()
                        }
                    };
                }
                "/* super.prop */".to_string()
            }
            Expr::PrivateName(name) => {
                // `#x in obj` — the private brand as a (mangled) string key.
                if self.dynamic_values {
                    format!("w3cos_core::Value::from({:?})", self.private_key(name))
                } else {
                    format!("{:?}", self.private_key(name))
                }
            }
            Expr::Cond(cond) => {
                let mut test = self.lower_expr(&cond.test);
                if self.dynamic_values {
                    test = format!("{test}.to_bool()");
                }
                let cons = self.lower_expr(&cond.cons);
                let alt = self.lower_expr(&cond.alt);
                format!("if {test} {{ {cons} }} else {{ {alt} }}")
            }
            Expr::TsTypeAssertion(assertion) => self.lower_expr(&assertion.expr),
            Expr::TsConstAssertion(assertion) => self.lower_expr(&assertion.expr),
            Expr::TsNonNull(non_null) => self.lower_expr(&non_null.expr),
            Expr::TsAs(as_expr) => self.lower_expr(&as_expr.expr),
            Expr::TsInstantiation(instantiation) => self.lower_expr(&instantiation.expr),
            Expr::TsSatisfies(satisfies) => self.lower_expr(&satisfies.expr),
            Expr::MetaProp(meta) if meta.kind == MetaPropKind::ImportMeta => {
                if self.dynamic_values {
                    "w3cos_core::js_object! { \"env\" => w3cos_core::js_object! { \"DEV\" => false, \"PROD\" => true, \"MODE\" => \"production\" } }".to_string()
                } else {
                    "Default::default() /* import.meta */".to_string()
                }
            }
            Expr::Await(await_expr) => {
                let arg = self.lower_expr(&await_expr.arg);
                if self.dynamic_values {
                    // Generated code is synchronous: `await` degrades to the
                    // operand itself (the promise value flows through; its
                    // resolution is not awaited — recorded limitation).
                    format!("{arg} /* await */")
                } else {
                    format!("{arg}.await")
                }
            }
            Expr::Seq(seq) => {
                let exprs: Vec<String> = seq.exprs.iter().map(|e| self.lower_expr(e)).collect();
                if let Some(last) = exprs.last() {
                    // In Rust, only the last value matters; emit others as statements.
                    let setup: Vec<&String> = exprs.iter().take(exprs.len() - 1).collect();
                    if setup.is_empty() {
                        last.clone()
                    } else {
                        format!(
                            "{{ {}; {} }}",
                            setup
                                .iter()
                                .map(|s| s.as_str())
                                .collect::<Vec<_>>()
                                .join("; "),
                            last
                        )
                    }
                } else {
                    "()".to_string()
                }
            }
            Expr::OptChain(opt) => match opt.base.as_ref() {
                OptChainBase::Member(member) => {
                    let obj = self.lower_expr(&member.obj);
                    if self.dynamic_values {
                        let property = match &member.prop {
                            MemberProp::Ident(id) => format!("{:?}", id.sym.to_string()),
                            MemberProp::Computed(computed) => {
                                format!("&{}.to_js_string()", self.lower_expr(&computed.expr))
                            }
                            MemberProp::PrivateName(name) => {
                                format!("{:?}", self.private_key(name))
                            }
                        };
                        return if property.starts_with('&') {
                            format!(
                                "if {obj}.is_nullish() {{ w3cos_core::Value::Undefined }} else {{ {obj}.get_property({property}) }}"
                            )
                        } else {
                            format!(
                                "if {obj}.is_nullish() {{ w3cos_core::Value::Undefined }} else {{ {obj}.get_property({property}) }}"
                            )
                        };
                    }
                    let prop = match &member.prop {
                        MemberProp::Ident(id) => format!(".{}", atom_str(&id.sym)),
                        MemberProp::Computed(c) => format!("[{}]", self.lower_expr(&c.expr)),
                        MemberProp::PrivateName(p) => format!(".{}", atom_str(&p.name)),
                    };
                    format!("{obj}.as_ref().map(|v| v{prop})")
                }
                OptChainBase::Call(call) => {
                    let args = call
                        .args
                        .iter()
                        .map(|a| self.lower_argument(&a.expr))
                        .collect::<Vec<_>>()
                        .join(", ");
                    if self.dynamic_values {
                        // Optional calls on a member must retain the base as
                        // the JavaScript receiver. Lowering `obj?.method()` as
                        // `method.call(undefined, ...)` loses `this` (and in
                        // Monaco breaks BracketPairsTree methods). Cache the
                        // receiver so chained getters are also evaluated once.
                        if let Some(member) = optional_call_member(&call.callee) {
                            let receiver = self.lower_expr(&member.obj);
                            let method = match &member.prop {
                                MemberProp::Ident(id) => format!(
                                    "__w3cos_receiver.get_property({:?})",
                                    id.sym.to_string()
                                ),
                                MemberProp::Computed(computed) => format!(
                                    "{{ let __w3cos_key = {}.to_js_string(); __w3cos_receiver.get_property(&__w3cos_key) }}",
                                    self.lower_expr(&computed.expr)
                                ),
                                MemberProp::PrivateName(name) => format!(
                                    "__w3cos_receiver.get_property({:?})",
                                    self.private_key(name)
                                ),
                            };
                            return format!(
                                "{{ let __w3cos_receiver = {receiver}; if __w3cos_receiver.is_nullish() {{ w3cos_core::Value::Undefined }} else {{ let __w3cos_method = {method}; if __w3cos_method.is_nullish() {{ w3cos_core::Value::Undefined }} else {{ __w3cos_method.call(__w3cos_receiver.clone(), vec![{args}]) }} }} }}"
                            );
                        }
                        let callee = self.lower_expr(&call.callee);
                        format!(
                            "if {callee}.is_nullish() {{ w3cos_core::Value::Undefined }} else {{ {callee}.call(w3cos_core::Value::Undefined, vec![{args}]) }}"
                        )
                    } else {
                        let callee = self.lower_expr(&call.callee);
                        format!("{callee}.map(|f| f({args}))")
                    }
                }
            },
            Expr::Yield(yield_expr) => {
                let arg = yield_expr
                    .arg
                    .as_ref()
                    .map(|a| self.lower_expr(a))
                    .unwrap_or_else(|| "()".to_string());
                format!("/* yield */ {arg}")
            }
            Expr::Fn(fn_expr) => {
                if self.dynamic_values {
                    let params = fn_expr
                        .function
                        .params
                        .iter()
                        .map(|param| param.pat.clone())
                        .collect::<Vec<_>>();
                    let parameter_names = pattern_names(&params);
                    let referenced = fn_expr
                        .function
                        .body
                        .as_ref()
                        .map(|block| stmts_referenced_names(&block.stmts))
                        .unwrap_or_default();
                    let mut captures =
                        capture_names(&self.known_values, &parameter_names, &referenced);
                    self.push_parent_capture(&mut captures);
                    let capture_bindings = captures
                        .iter()
                        .map(|name| format!("let mut {name} = {name}.clone(); "))
                        .collect::<String>();
                    let (bindings, body) = fn_expr
                        .function
                        .body
                        .as_ref()
                        .map(|block| {
                            let mut ctx = self.child_dynamic_ctx();
                            // A function expression has its own dynamic `this`:
                            // rebind it to the closure's receiver parameter.
                            ctx.this_name = Some("__this".to_string());
                            ctx.known_values.extend(captures.iter().cloned());
                            ctx.bind_patterns(&params);
                            ctx.indent = self.indent + 4;
                            ctx.enter_fn_scope(&params, &block.stmts);
                            let bindings = ctx.lower_closure_params(&params);
                            let prologue = ctx.hoist_fn_body_vars(&block.stmts);
                            (
                                bindings,
                                format!("{prologue}{}", ctx.lower_stmts(&block.stmts)),
                            )
                        })
                        .unwrap_or_default();
                    return format!(
                        "{{ {capture_bindings} w3cos_core::Value::function(move |__this, __args| {{ {bindings}{body} w3cos_core::Value::Undefined }}) }}"
                    );
                }
                let params = fn_expr
                    .function
                    .params
                    .iter()
                    .map(|p| self.lower_pat(&p.pat))
                    .collect::<Vec<_>>()
                    .join(", ");
                let body = fn_expr
                    .function
                    .body
                    .as_ref()
                    .map(|b| {
                        let mut ctx = LowerCtx::new(self.renames.clone());
                        ctx.indent = self.indent + 4;
                        ctx.lower_stmts(&b.stmts)
                    })
                    .unwrap_or_default();
                format!("|{params}| {{ {body} }}")
            }
            Expr::Class(class_expr) => {
                if self.dynamic_values {
                    self.lower_class_value(class_expr)
                } else {
                    "/* class expr */ Default::default()".to_string()
                }
            }
            Expr::TaggedTpl(tagged) => {
                let tag = self.lower_expr(&tagged.tag);
                let quasi = self.lower_expr(&Expr::Tpl(*tagged.tpl.clone()));
                format!("{tag}({quasi})")
            }
            Expr::JSXElement(element) => self.lower_jsx_element(element),
            Expr::JSXFragment(fragment) => self.lower_jsx_children(&fragment.children),
            _ => format!("todo!(\"lower: {}\")", expr_kind_name(expr)),
        }
    }

    fn lower_jsx_element(&self, element: &JSXElement) -> String {
        let element_type = self.lower_jsx_name(&element.opening.name);
        let props = self.lower_jsx_props(&element.opening.attrs);
        let children = element
            .children
            .iter()
            .filter_map(|child| self.lower_jsx_child(child))
            .collect::<Vec<_>>();
        let props = if children.is_empty() {
            props
        } else {
            let children = format!("w3cos_core::Value::array(vec![{}])", children.join(", "));
            format!(
                "w3cos_core::Value::object_from_parts(vec![{props}, w3cos_core::js_object! {{ \"children\" => {children} }}])"
            )
        };
        format!("w3cos_core::js_object! {{ \"type\" => {element_type}, \"props\" => {props} }}")
    }

    fn lower_jsx_children(&self, children: &[JSXElementChild]) -> String {
        let children = children
            .iter()
            .filter_map(|child| self.lower_jsx_child(child))
            .collect::<Vec<_>>()
            .join(", ");
        format!("w3cos_core::Value::array(vec![{children}])")
    }

    fn lower_jsx_name(&self, name: &JSXElementName) -> String {
        match name {
            JSXElementName::Ident(identifier) => {
                let name = atom_str(&identifier.sym);
                if name.chars().next().is_some_and(char::is_lowercase) {
                    format!("w3cos_core::Value::from({name:?})")
                } else {
                    self.resolve_value(&name)
                }
            }
            JSXElementName::JSXMemberExpr(member) => self.lower_jsx_member_name(member),
            JSXElementName::JSXNamespacedName(name) => format!(
                "w3cos_core::Value::from({:?})",
                format!("{}:{}", name.ns.sym, name.name.sym)
            ),
        }
    }

    fn lower_jsx_member_name(&self, member: &JSXMemberExpr) -> String {
        let object = match &member.obj {
            JSXObject::Ident(identifier) => self.resolve_value(&atom_str(&identifier.sym)),
            JSXObject::JSXMemberExpr(member) => self.lower_jsx_member_name(member),
        };
        format!("{object}.get_property({:?})", atom_str(&member.prop.sym))
    }

    fn lower_jsx_props(&self, attributes: &[JSXAttrOrSpread]) -> String {
        if attributes.is_empty() {
            return "w3cos_core::js_object! {}".to_string();
        }
        let parts = attributes
            .iter()
            .filter_map(|attribute| match attribute {
                JSXAttrOrSpread::SpreadElement(spread) => Some(self.lower_expr(&spread.expr)),
                JSXAttrOrSpread::JSXAttr(attribute) => {
                    let key = match &attribute.name {
                        JSXAttrName::Ident(identifier) => atom_str(&identifier.sym),
                        JSXAttrName::JSXNamespacedName(name) => {
                            format!("{}:{}", name.ns.sym, name.name.sym)
                        }
                    };
                    let value = match attribute.value.as_ref() {
                        None => "w3cos_core::Value::Bool(true)".to_string(),
                        Some(JSXAttrValue::Str(value)) => {
                            format!(
                                "w3cos_core::Value::from({:?})",
                                wtf8_to_string(&value.value)
                            )
                        }
                        Some(JSXAttrValue::JSXExprContainer(container)) => match &container.expr {
                            JSXExpr::Expr(expression) => self.lower_expr(expression),
                            JSXExpr::JSXEmptyExpr(_) => "w3cos_core::Value::Undefined".to_string(),
                        },
                        Some(JSXAttrValue::JSXElement(element)) => self.lower_jsx_element(element),
                        Some(JSXAttrValue::JSXFragment(fragment)) => {
                            self.lower_jsx_children(&fragment.children)
                        }
                    };
                    Some(format!("w3cos_core::js_object! {{ {key:?} => {value} }}"))
                }
            })
            .collect::<Vec<_>>();
        if parts.len() == 1 {
            parts[0].clone()
        } else {
            format!(
                "w3cos_core::Value::object_from_parts(vec![{}])",
                parts.join(", ")
            )
        }
    }

    fn lower_jsx_child(&self, child: &JSXElementChild) -> Option<String> {
        match child {
            JSXElementChild::JSXText(text) => {
                let value = text.value.split_whitespace().collect::<Vec<_>>().join(" ");
                (!value.is_empty()).then(|| format!("w3cos_core::Value::from({value:?})"))
            }
            JSXElementChild::JSXExprContainer(container) => match &container.expr {
                JSXExpr::Expr(expression) => Some(self.lower_expr(expression)),
                JSXExpr::JSXEmptyExpr(_) => None,
            },
            JSXElementChild::JSXSpreadChild(spread) => Some(self.lower_expr(&spread.expr)),
            JSXElementChild::JSXElement(element) => Some(self.lower_jsx_element(element)),
            JSXElementChild::JSXFragment(fragment) => {
                Some(self.lower_jsx_children(&fragment.children))
            }
        }
    }

    fn lower_call(&self, call: &CallExpr) -> String {
        // `super(...)` — parent constructor call inside a derived ctor.
        if self.dynamic_values && matches!(call.callee, Callee::Super(_)) {
            let args = call
                .args
                .iter()
                .map(|arg| self.lower_argument(&arg.expr))
                .collect::<Vec<_>>()
                .join(", ");
            let this = self.this_expr();
            return match self.class_scope.as_ref().and_then(|s| s.parent.clone()) {
                Some(parent) => {
                    format!("w3cos_core::class::super_ctor(&{this}, &{parent}, vec![{args}])")
                }
                None => {
                    "w3cos_core::Value::Undefined /* super() outside derived ctor */".to_string()
                }
            };
        }
        // `super.method(...)` — parent prototype (or class object) dispatch.
        if self.dynamic_values
            && let Callee::Expr(callee) = &call.callee
            && let Expr::SuperProp(super_prop) = callee.as_ref()
        {
            let args = call
                .args
                .iter()
                .map(|arg| self.lower_argument(&arg.expr))
                .collect::<Vec<_>>()
                .join(", ");
            return self.lower_super_call(super_prop, &args);
        }
        if self.dynamic_values
            && let Callee::Expr(callee) = &call.callee
            && let Expr::Member(member) = callee.as_ref()
            && matches!(&member.prop, MemberProp::Ident(identifier) if identifier.sym == *"forEach")
            && let Some(first) = call.args.first()
            && let Expr::Arrow(arrow) = first.expr.as_ref()
        {
            let object = self.lower_expr(&member.obj);
            let mut ctx = self.child_dynamic_ctx();
            ctx.known_values = self.known_values.clone();
            ctx.bind_patterns(&arrow.params);
            let analysis_body: Vec<Stmt> = match arrow.body.as_ref() {
                BlockStmtOrExpr::Expr(expression) => vec![Stmt::Return(ReturnStmt {
                    span: arrow.span,
                    arg: Some(expression.clone()),
                })],
                BlockStmtOrExpr::BlockStmt(block) => block.stmts.clone(),
            };
            ctx.enter_fn_scope(&arrow.params, &analysis_body);
            let mut bindings = String::new();
            let mut fixups = Vec::new();
            // forEach callbacks receive (item, index, array); the loop drives
            // the first two, anything further is Undefined.
            for (index, pattern) in arrow.params.iter().enumerate() {
                let source = match index {
                    0 => "__item".to_string(),
                    1 => "w3cos_core::Value::Number(__index as f64)".to_string(),
                    _ => "w3cos_core::Value::Undefined".to_string(),
                };
                ctx.lower_closure_pattern(pattern, &source, &mut bindings, &mut fixups);
            }
            for fixup in fixups {
                bindings.push_str(&fixup);
                bindings.push(' ');
            }
            let body = match arrow.body.as_ref() {
                BlockStmtOrExpr::Expr(expression) => {
                    format!("{};", ctx.lower_expr(expression))
                }
                BlockStmtOrExpr::BlockStmt(block) => {
                    let prologue = ctx.hoist_fn_body_vars(&block.stmts);
                    format!("{prologue}{}", ctx.lower_stmts(&block.stmts))
                }
            };
            return format!(
                "{{ for (__index, __item) in {object}.iter().enumerate() {{ {bindings}{body} }} w3cos_core::Value::Undefined }}"
            );
        }
        if self.dynamic_values
            && let Callee::Expr(callee) = &call.callee
            && let Expr::Member(member) = callee.as_ref()
        {
            let object = self.lower_expr(&member.obj);
            let key = match &member.prop {
                MemberProp::Ident(id) => format!("{:?}", id.sym.to_string()),
                MemberProp::Computed(computed) => {
                    format!("&{}.to_js_string()", self.lower_expr(&computed.expr))
                }
                MemberProp::PrivateName(name) => format!("{:?}", self.private_key(name)),
            };
            let args = self.lower_dynamic_argument_vec(&call.args);
            return format!("{object}.call_method({key}, {args})");
        }
        let callee = match &call.callee {
            Callee::Expr(e) => self.lower_expr(e),
            Callee::Import(_) if self.dynamic_values => {
                // Dynamic import(): all modules are statically bundled, so
                // degrade to a resolved promise of Undefined (the namespace
                // object cannot be recovered from a dynamic specifier).
                let spec = call
                    .args
                    .first()
                    .map(|a| self.lower_expr(&a.expr))
                    .unwrap_or_else(|| "w3cos_core::Value::Undefined".to_string());
                return format!(
                    "{{ let _ = {spec}; w3cos_core::promise::resolve(vec![w3cos_core::Value::Undefined]) }} /* dynamic import */"
                );
            }
            _ => "/* super/import call */".to_string(),
        };
        let args = call
            .args
            .iter()
            .map(|arg| self.lower_argument(&arg.expr))
            .collect::<Vec<_>>()
            .join(", ");
        let dynamic_args = self
            .dynamic_values
            .then(|| self.lower_dynamic_argument_vec(&call.args));
        if self.dynamic_values
            && let Callee::Expr(expression) = &call.callee
            && let Expr::Ident(identifier) = expression.as_ref()
        {
            let name = atom_str(&identifier.sym);
            if self.class_names.contains(&name) && !self.known_values.contains(&name) {
                // Calling a class without `new` (a TypeError in JS) —
                // approximate via construct() to stay total.
                return format!(
                    "w3cos_core::class::construct(&{}, {})",
                    self.resolve_value(&name),
                    dynamic_args.as_ref().expect("dynamic argument vector")
                );
            }
            if self.value_bindings.contains(&name) {
                // Function-valued variable: call the Value, not a Rust fn.
                let callee = self.resolve_value(&name);
                return format!(
                    "{callee}.call(w3cos_core::Value::Undefined, {})",
                    dynamic_args.as_ref().expect("dynamic argument vector")
                );
            }
            if self.fn_item_names.contains(&name) {
                // Nested fn item: direct Rust call with the args vector.
                return format!(
                    "{name}({})",
                    dynamic_args.as_ref().expect("dynamic argument vector")
                );
            }
            let is_static = !self.known_values.contains(&name)
                && (self.renames.iter().any(|(local, _)| local == &name)
                    || matches!(
                        name.as_str(),
                        "parseInt" | "parseFloat" | "RangeError" | "Error"
                    ));
            if !is_static {
                return format!(
                    "{callee}.call(w3cos_core::Value::Undefined, {})",
                    dynamic_args.as_ref().expect("dynamic argument vector")
                );
            }
        }
        if self.dynamic_values {
            let callee = match &call.callee {
                Callee::Expr(expression) => match expression.as_ref() {
                    Expr::Ident(identifier) => {
                        let name = atom_str(&identifier.sym);
                        if name == "Error" {
                            "ErrorValue".to_string()
                        } else {
                            self.resolve_name(&name)
                        }
                    }
                    // Non-identifier callees (IIFEs, parenthesized fns) are
                    // Value expressions: invoke through Value::call.
                    _ => {
                        return format!(
                            "{callee}.call(w3cos_core::Value::Undefined, {})",
                            dynamic_args.as_ref().expect("dynamic argument vector")
                        );
                    }
                },
                _ => callee,
            };
            format!(
                "{callee}({})",
                dynamic_args.as_ref().expect("dynamic argument vector")
            )
        } else {
            format!("{callee}({args})")
        }
    }

    fn lower_member(&self, member: &MemberExpr) -> String {
        let obj = self.lower_expr(&member.obj);
        if self.dynamic_values {
            return match &member.prop {
                MemberProp::Ident(id) => {
                    format!("{obj}.get_property({:?})", id.sym.to_string())
                }
                MemberProp::Computed(computed) => format!(
                    "{obj}.get_property(&{}.to_js_string())",
                    self.lower_expr(&computed.expr)
                ),
                MemberProp::PrivateName(name) => {
                    format!("{obj}.get_property({:?})", self.private_key(name))
                }
            };
        }
        let prop = match &member.prop {
            MemberProp::Ident(id) => format!(".{}", atom_str(&id.sym)),
            MemberProp::Computed(c) => format!("[{}]", self.lower_expr(&c.expr)),
            MemberProp::PrivateName(p) => format!(".{}", atom_str(&p.name)),
        };
        format!("{obj}{prop}")
    }

    fn lower_argument(&self, expression: &Expr) -> String {
        let value = self.lower_expr(expression);
        if self.dynamic_values {
            format!("{value}.clone()")
        } else {
            value
        }
    }

    fn lower_dynamic_argument_vec(&self, arguments: &[ExprOrSpread]) -> String {
        if !arguments.iter().any(|argument| argument.spread.is_some()) {
            let arguments = arguments
                .iter()
                .map(|argument| self.lower_argument(&argument.expr))
                .collect::<Vec<_>>()
                .join(", ");
            return format!("vec![{arguments}]");
        }

        let statements = arguments
            .iter()
            .map(|argument| {
                let value = self.lower_expr(&argument.expr);
                if argument.spread.is_some() {
                    format!("__w3cos_call_args.extend(({value}).iter());")
                } else {
                    format!("__w3cos_call_args.push({value}.clone());")
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        format!("{{ let mut __w3cos_call_args = Vec::new(); {statements} __w3cos_call_args }}")
    }

    fn lower_lit(&self, lit: &Lit) -> String {
        if self.dynamic_values {
            return match lit {
                Lit::Str(value) => format!(
                    "w3cos_core::Value::from({:?})",
                    wtf8_to_string(&value.value)
                ),
                Lit::Num(value) => {
                    format!("w3cos_core::Value::Number({:?})", value.value)
                }
                Lit::Bool(value) => format!("w3cos_core::Value::Bool({})", value.value),
                Lit::Null(_) => "w3cos_core::Value::Null".to_string(),
                Lit::Regex(value) => format!(
                    "w3cos_core::regexp::create({:?}, {:?})",
                    wtf8_to_string(&value.exp),
                    wtf8_to_string(&value.flags)
                ),
                _ => "w3cos_core::Value::Undefined".to_string(),
            };
        }
        match lit {
            Lit::Str(s) => {
                let raw = wtf8_to_string(&s.value);
                format!("\"{}\"", raw.replace('\\', "\\\\").replace('"', "\\\""))
            }
            Lit::Num(n) => {
                if n.value.fract() == 0.0 && n.value.abs() < i64::MAX as f64 {
                    format!("{}", n.value as i64)
                } else {
                    format!("{}f64", n.value)
                }
            }
            Lit::Bool(b) => format!("{}", b.value),
            Lit::Null(_) => "None".to_string(),
            Lit::BigInt(b) => format!("/* BigInt({}) */0", b.value),
            Lit::Regex(r) => {
                let exp = wtf8_to_string(&r.exp);
                let flags = wtf8_to_string(&r.flags);
                format!("/* regex /{exp}/{flags} */\"\"")
            }
            Lit::JSXText(t) => {
                let val = wtf8_to_string(&t.value);
                format!("\"{val}\"")
            }
        }
    }

    fn lower_pat(&self, pat: &Pat) -> String {
        match pat {
            Pat::Ident(i) => atom_str(&i.id.sym),
            Pat::Array(arr) => {
                let elems = arr
                    .elems
                    .iter()
                    .map(|e| match e {
                        Some(p) => self.lower_pat(p),
                        None => "_".to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("[{elems}]")
            }
            Pat::Object(obj) => {
                let props = obj
                    .props
                    .iter()
                    .map(|p| match p {
                        ObjectPatProp::Assign(a) => atom_str(&a.key.sym),
                        ObjectPatProp::KeyValue(kv) => self.lower_pat(&kv.value),
                        ObjectPatProp::Rest(r) => format!("..{}", self.lower_pat(&r.arg)),
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("/* {{ {props} }} */_{}", props.len())
            }
            Pat::Rest(r) => format!("/* ...{} */", self.lower_pat(&r.arg)),
            _ => "_".to_string(),
        }
    }

    /// Bind fn/closure parameters from `__args` at the top of a lowered body.
    /// Default values lower with the CURRENT ctx (enclosing locals, classes,
    /// namespaces, and boxed cells all resolve correctly); params whose name
    /// is boxed get the shared `Rc<RefCell<Value>>` cell.
    ///
    /// Binding is two-phase: all params are bound from their raw `__args`
    /// source first, then defaults are applied as `if x.is_undefined()`
    /// fixups in declaration order. A default value may reference (or close
    /// over) a LATER parameter — deferring application keeps that legal.
    pub(crate) fn lower_closure_params(&self, params: &[Pat]) -> String {
        let mut output = String::new();
        let mut fixups = Vec::new();
        for (index, pattern) in params.iter().enumerate() {
            let source = if matches!(pattern, Pat::Rest(_)) {
                format!("w3cos_core::Value::array(__args.iter().skip({index}).cloned().collect())")
            } else {
                format!("__args.get({index}).cloned().unwrap_or(w3cos_core::Value::Undefined)")
            };
            self.lower_closure_pattern(pattern, &source, &mut output, &mut fixups);
        }
        for fixup in fixups {
            output.push_str(&fixup);
            output.push(' ');
        }
        output
    }

    fn lower_closure_pattern(
        &self,
        pattern: &Pat,
        source: &str,
        output: &mut String,
        fixups: &mut Vec<String>,
    ) {
        match pattern {
            Pat::Ident(ident) => {
                let name = sanitize_ident(&ident.id.sym.to_string());
                output.push_str(&self.bind_local(&name, source));
                output.push(' ');
            }
            Pat::Array(array) => {
                for (index, element) in array.elems.iter().enumerate() {
                    if let Some(element) = element {
                        let nested = format!("{source}.get_property({:?})", index.to_string());
                        self.lower_closure_pattern(element, &nested, output, fixups);
                    }
                }
            }
            Pat::Object(object) => {
                for property in &object.props {
                    match property {
                        ObjectPatProp::Assign(assign) => {
                            let name = sanitize_ident(&assign.key.sym.to_string());
                            let value =
                                format!("{source}.get_property({:?})", assign.key.sym.to_string());
                            output.push_str(&self.bind_local(&name, &value));
                            output.push(' ');
                            if let Some(default) = &assign.value {
                                let fallback = self.lower_expr(default);
                                let check = self.undefined_check(&name);
                                fixups.push(format!(
                                    "if {check} {{ {} }}",
                                    self.assign_local(&name, &fallback)
                                ));
                            }
                        }
                        ObjectPatProp::KeyValue(key_value) => {
                            let key = match &key_value.key {
                                PropName::Ident(ident) => ident.sym.to_string(),
                                PropName::Str(value) => wtf8_to_string(&value.value),
                                PropName::Num(value) => value.value.to_string(),
                                _ => continue,
                            };
                            let nested = format!("{source}.get_property({key:?})");
                            self.lower_closure_pattern(&key_value.value, &nested, output, fixups);
                        }
                        ObjectPatProp::Rest(rest) => {
                            self.lower_closure_pattern(&rest.arg, source, output, fixups)
                        }
                    }
                }
            }
            Pat::Assign(assign) => match assign.left.as_ref() {
                Pat::Ident(ident) => {
                    // Defer the default to the fixup phase (see above).
                    let name = sanitize_ident(&ident.id.sym.to_string());
                    output.push_str(&self.bind_local(&name, source));
                    output.push(' ');
                    let fallback = self.lower_expr(&assign.right);
                    let check = self.undefined_check(&name);
                    fixups.push(format!(
                        "if {check} {{ {} }}",
                        self.assign_local(&name, &fallback)
                    ));
                }
                left => {
                    // Whole-pattern default (`[a, b] = x ?? []`): the default
                    // selects the source as a whole — keep the inline form.
                    let fallback = self.lower_expr(&assign.right);
                    let nested = format!(
                        "{{ let value = {source}; if value.is_undefined() {{ {fallback} }} else {{ value }} }}"
                    );
                    self.lower_closure_pattern(left, &nested, output, fixups);
                }
            },
            Pat::Rest(rest) => self.lower_closure_pattern(&rest.arg, source, output, fixups),
            _ => {}
        }
    }

    fn lower_prop_name(&self, name: &PropName) -> String {
        match name {
            PropName::Ident(id) => atom_str(&id.sym),
            PropName::Str(s) => {
                let raw = wtf8_to_string(&s.value);
                format!("\"{raw}\"")
            }
            PropName::Num(n) => format!("{}", n.value),
            PropName::Computed(c) => self.lower_expr(&c.expr),
            PropName::BigInt(b) => format!("{}", b.value),
        }
    }

    fn lower_object_key(&self, name: &PropName) -> String {
        match name {
            PropName::Ident(id) => format!("{:?}", id.sym.to_string()),
            PropName::Str(value) => format!("{:?}", wtf8_to_string(&value.value)),
            PropName::Num(value) => format!("{:?}", value.value.to_string()),
            PropName::Computed(value) => {
                format!("{}.to_js_string()", self.lower_expr(&value.expr))
            }
            PropName::BigInt(value) => format!("{:?}", value.value.to_string()),
        }
    }

    fn lower_dynamic_function_value(&self, params: &[Pat], body: &[Stmt]) -> String {
        let parameter_names = pattern_names(params);
        let referenced = stmts_referenced_names(body);
        let mut captures = capture_names(&self.known_values, &parameter_names, &referenced);
        self.push_parent_capture(&mut captures);
        let capture_bindings = captures
            .iter()
            .map(|name| format!("let mut {name} = {name}.clone(); "))
            .collect::<String>();
        let mut ctx = self.child_dynamic_ctx();
        // Object-literal methods have their own dynamic `this` (the receiver).
        ctx.this_name = Some("__this".to_string());
        ctx.known_values.extend(captures.iter().cloned());
        ctx.bind_patterns(params);
        ctx.enter_fn_scope(params, body);
        let bindings = ctx.lower_closure_params(params);
        let prologue = ctx.hoist_fn_body_vars(body);
        let body = format!("{prologue}{}", ctx.lower_stmts(body));
        format!(
            "{{ {capture_bindings} w3cos_core::Value::function(move |__this, __args| {{ {bindings}{body} w3cos_core::Value::Undefined }}) }}"
        )
    }

    /// `super.method(args)` — instance: parent prototype chain dispatch with
    /// the current receiver; static: parent class object access (best effort).
    fn lower_super_call(&self, super_prop: &SuperPropExpr, args: &str) -> String {
        let this = self.this_expr();
        match (&self.class_scope, &super_prop.prop) {
            (Some(scope), _) if scope.parent.is_none() => {
                "w3cos_core::Value::Undefined /* super without parent */".to_string()
            }
            (Some(scope), SuperProp::Ident(ident)) => {
                let parent = scope.parent.clone().unwrap_or_default();
                let key = atom_str(&ident.sym);
                if scope.is_static {
                    format!(
                        "{{ let __super_fn = {parent}.get_property({key:?}); __super_fn.call({this}.clone(), vec![{args}]) }}"
                    )
                } else {
                    format!(
                        "w3cos_core::class::super_method(&{this}, &{parent}, {key:?}, vec![{args}])"
                    )
                }
            }
            (Some(scope), SuperProp::Computed(computed)) => {
                let parent = scope.parent.clone().unwrap_or_default();
                let key = format!("&{}.to_js_string()", self.lower_expr(&computed.expr));
                if scope.is_static {
                    format!(
                        "{{ let __super_fn = {parent}.get_property({key}); __super_fn.call({this}.clone(), vec![{args}]) }}"
                    )
                } else {
                    format!(
                        "w3cos_core::class::super_method(&{this}, &{parent}, {key}, vec![{args}])"
                    )
                }
            }
            (None, _) => "w3cos_core::Value::Undefined /* super outside class */".to_string(),
        }
    }

    /// Lower a class *expression* (named or anonymous, optionally with
    /// `extends <expr>`) to a block expression that eagerly builds the class
    /// object. Method/ctor bodies become inline closures so they can capture
    /// surrounding locals (unlike top-level class declarations, which emit
    /// free functions — see `esm_codegen::emit_class`).
    fn lower_class_value(&self, class_expr: &ClassExpr) -> String {
        let class = &class_expr.class;
        let class_name = class_expr
            .ident
            .as_ref()
            .map(|ident| sanitize_ident(&ident.sym.to_string()))
            .unwrap_or_else(|| format!("anon{}", class.span.lo.0));
        // The parent expression is evaluated once, in the enclosing scope.
        let parent = class.super_class.as_ref().map(|expr| self.lower_expr(expr));
        let parent_ref = parent.as_ref().map(|_| "__parent".to_string());
        let instance_scope = ClassScope {
            class_name: class_name.clone(),
            parent: parent_ref.clone(),
            is_static: false,
        };
        let static_scope = ClassScope {
            class_name: class_name.clone(),
            parent: parent_ref,
            is_static: true,
        };

        let mut ctor: Option<(Vec<Pat>, Vec<Stmt>)> = None;
        let mut proto_installs: Vec<String> = Vec::new();
        let mut static_installs: Vec<String> = Vec::new();
        let mut static_inits: Vec<String> = Vec::new();
        let mut field_inits: Vec<String> = Vec::new();
        // Names referenced by instance-field initializers (they inline into
        // the ctor closure, which must capture them).
        let mut field_refs: HashSet<String> = HashSet::new();

        for member in &class.body {
            match member {
                ClassMember::Constructor(constructor) => {
                    let params = constructor
                        .params
                        .iter()
                        .filter_map(|param| match param {
                            ParamOrTsParamProp::Param(param) => Some(param.pat.clone()),
                            ParamOrTsParamProp::TsParamProp(_) => None,
                        })
                        .collect();
                    let body = constructor
                        .body
                        .as_ref()
                        .map(|block| block.stmts.clone())
                        .unwrap_or_default();
                    ctor = Some((params, body));
                }
                ClassMember::Method(method) => {
                    let params = method
                        .function
                        .params
                        .iter()
                        .map(|param| param.pat.clone())
                        .collect::<Vec<_>>();
                    let body = method
                        .function
                        .body
                        .as_ref()
                        .map(|block| block.stmts.clone())
                        .unwrap_or_default();
                    let scope = if method.is_static {
                        &static_scope
                    } else {
                        &instance_scope
                    };
                    let closure = self.lower_method_closure(&params, &body, scope);
                    let (target, installs) = if method.is_static {
                        ("__class", &mut static_installs)
                    } else {
                        ("__proto", &mut proto_installs)
                    };
                    let prefix = match method.kind {
                        MethodKind::Method => "",
                        MethodKind::Getter => "__w3cos_getter_",
                        MethodKind::Setter => "__w3cos_setter_",
                    };
                    let key = self.key_arg(prefix, &method.key);
                    installs.push(format!("{target}.set_property({key}, {closure});"));
                }
                ClassMember::PrivateMethod(method) => {
                    let params = method
                        .function
                        .params
                        .iter()
                        .map(|param| param.pat.clone())
                        .collect::<Vec<_>>();
                    let body = method
                        .function
                        .body
                        .as_ref()
                        .map(|block| block.stmts.clone())
                        .unwrap_or_default();
                    let scope = if method.is_static {
                        &static_scope
                    } else {
                        &instance_scope
                    };
                    let closure = self.lower_method_closure(&params, &body, scope);
                    let (target, installs) = if method.is_static {
                        ("__class", &mut static_installs)
                    } else {
                        ("__proto", &mut proto_installs)
                    };
                    let mangled = Self::mangle_private(&class_name, &method.key);
                    let key = match method.kind {
                        MethodKind::Method => mangled,
                        MethodKind::Getter => format!("__w3cos_getter_{mangled}"),
                        MethodKind::Setter => format!("__w3cos_setter_{mangled}"),
                    };
                    installs.push(format!("{target}.set_property({key:?}, {closure});"));
                }
                ClassMember::ClassProp(prop) => {
                    let scope = if prop.is_static {
                        &static_scope
                    } else {
                        &instance_scope
                    };
                    if let Some(value) = &prop.value {
                        field_refs.extend(expr_referenced_names(value));
                    }
                    let init = self.lower_field_value(prop.value.as_deref(), scope);
                    let key = self.key_arg("", &prop.key);
                    if prop.is_static {
                        static_inits.push(format!(
                            "w3cos_core::class::define_field(&__this, {key}, {init});"
                        ));
                    } else {
                        field_inits.push(format!(
                            "w3cos_core::class::define_field(&__this, {key}, {init});"
                        ));
                    }
                }
                ClassMember::PrivateProp(prop) => {
                    let scope = if prop.is_static {
                        &static_scope
                    } else {
                        &instance_scope
                    };
                    if let Some(value) = &prop.value {
                        field_refs.extend(expr_referenced_names(value));
                    }
                    let init = self.lower_field_value(prop.value.as_deref(), scope);
                    let key = format!("{:?}", Self::mangle_private(&class_name, &prop.key));
                    if prop.is_static {
                        static_inits.push(format!(
                            "w3cos_core::class::define_field(&__this, {key}, {init});"
                        ));
                    } else {
                        field_inits.push(format!(
                            "w3cos_core::class::define_field(&__this, {key}, {init});"
                        ));
                    }
                }
                ClassMember::StaticBlock(block) => {
                    let mut ctx = self.child_dynamic_ctx();
                    ctx.class_scope = Some(static_scope.clone());
                    ctx.this_name = Some("__this".to_string());
                    // Static blocks run inline in the class-value block:
                    // enclosing locals stay visible here.
                    ctx.known_values = self.known_values.clone();
                    ctx.enter_fn_scope(&[], &block.body.stmts);
                    let body = ctx.lower_stmts(&block.body.stmts);
                    static_inits.push(format!("{{ {body} }}"));
                }
                // Index signatures, empty members, accessors: not lowered.
                _ => {}
            }
        }

        let derived = parent.is_some();
        let ctor_closure = match &ctor {
            Some((params, body)) => self.lower_ctor_closure(
                params,
                body,
                &field_inits,
                &instance_scope,
                derived,
                &field_refs,
            ),
            None if derived => {
                // Derived class without a ctor: forward all args to super.
                let mut captures = capture_names(&self.known_values, &HashSet::new(), &field_refs);
                if !captures.iter().any(|name| name == "__parent") {
                    captures.push("__parent".to_string());
                }
                captures.sort();
                let capture_bindings = captures
                    .iter()
                    .map(|name| format!("let mut {name} = {name}.clone(); "))
                    .collect::<String>();
                let fields = field_inits.join(" ");
                format!(
                    "{{ {capture_bindings} w3cos_core::Value::function(move |__this: w3cos_core::Value, __args: Vec<w3cos_core::Value>| -> w3cos_core::Value {{ w3cos_core::class::super_ctor(&__this, &__parent, __args); {fields} __this }}) }}"
                )
            }
            None => {
                let captures = capture_names(&self.known_values, &HashSet::new(), &field_refs);
                let capture_bindings = captures
                    .iter()
                    .map(|name| format!("let mut {name} = {name}.clone(); "))
                    .collect::<String>();
                let fields = field_inits.join(" ");
                format!(
                    "{{ {capture_bindings} w3cos_core::Value::function(move |__this: w3cos_core::Value, __args: Vec<w3cos_core::Value>| -> w3cos_core::Value {{ let _ = &__args; {fields} __this }}) }}"
                )
            }
        };

        let mut out = String::from("{ ");
        if let Some(parent_expr) = &parent {
            out.push_str(&format!("let __parent = {parent_expr}; "));
        }
        out.push_str("let __proto = w3cos_core::Value::object(std::collections::HashMap::new()); ");
        for install in &proto_installs {
            out.push_str(install);
            out.push(' ');
        }
        if derived {
            out.push_str(
                "w3cos_core::class::set_prototype_of(&__proto, &__parent.get_property(\"prototype\")); ",
            );
        }
        out.push_str(&format!("let __ctor = {ctor_closure}; "));
        out.push_str("let __ctor_c = __ctor.clone(); ");
        out.push_str("let __proto_c = __proto.clone(); ");
        out.push_str(
            "let __class = w3cos_core::Value::callable(std::collections::HashMap::new(), move |_this, __args| { let __instance = w3cos_core::Value::object(std::collections::HashMap::new()); w3cos_core::class::set_prototype_of(&__instance, &__proto_c); let __ret = __ctor_c.call(__instance.clone(), __args); if __ret.is_object() { __ret } else { __instance } }); ",
        );
        out.push_str("__proto.set_property(\"constructor\", __class.clone()); ");
        out.push_str("__class.set_property(\"prototype\", __proto); ");
        out.push_str("__class.set_property(\"__w3cos_ctor\", __ctor); ");
        for install in &static_installs {
            out.push_str(install);
            out.push(' ');
        }
        if derived {
            out.push_str("w3cos_core::class::set_prototype_of(&__class, &__parent); ");
        }
        if !static_inits.is_empty() {
            out.push_str("{ let __this = __class.clone(); ");
            for init in &static_inits {
                out.push_str(init);
                out.push(' ');
            }
            out.push_str("} ");
        }
        out.push_str("__class }");
        out
    }

    /// Lower a field initializer expression with the given class scope
    /// (`this` is available: the instance, or the class object when static).
    /// Field initializers run inside the ctor/init closures, so enclosing
    /// locals stay visible (and captured) here.
    fn lower_field_value(&self, value: Option<&Expr>, scope: &ClassScope) -> String {
        let Some(value) = value else {
            return "w3cos_core::Value::Undefined".to_string();
        };
        let mut ctx = self.child_dynamic_ctx();
        ctx.class_scope = Some(scope.clone());
        ctx.this_name = Some("__this".to_string());
        ctx.known_values = self.known_values.clone();
        ctx.lower_expr(value)
    }

    /// Lower a class-expression method/getter/setter body to an inline
    /// closure value. Captures surrounding locals (cloned) and `__parent`
    /// when the class extends something.
    fn lower_method_closure(&self, params: &[Pat], body: &[Stmt], scope: &ClassScope) -> String {
        let parameter_names = pattern_names(params);
        let referenced = stmts_referenced_names(body);
        let mut captures = capture_names(&self.known_values, &parameter_names, &referenced);
        if scope.parent.is_some() && !captures.iter().any(|name| name == "__parent") {
            captures.push("__parent".to_string());
        }
        captures.sort();
        let capture_bindings = captures
            .iter()
            .map(|name| format!("let mut {name} = {name}.clone(); "))
            .collect::<String>();
        let mut ctx = self.child_dynamic_ctx();
        ctx.class_scope = Some(scope.clone());
        ctx.this_name = Some("__this".to_string());
        ctx.known_values.extend(captures.iter().cloned());
        ctx.bind_patterns(params);
        ctx.enter_fn_scope(params, body);
        let bindings = ctx.lower_closure_params(params);
        let prologue = ctx.hoist_fn_body_vars(body);
        let body = format!("{prologue}{}", ctx.lower_stmts(body));
        format!(
            "{{ {capture_bindings} w3cos_core::Value::function(move |__this: w3cos_core::Value, __args: Vec<w3cos_core::Value>| -> w3cos_core::Value {{ {bindings}{body} w3cos_core::Value::Undefined }}) }}"
        )
    }

    /// Lower a class-expression constructor. Field initializers run at the
    /// top for base classes and immediately after the top-level `super(...)`
    /// call for derived classes (at the end when no such call exists).
    /// `field_refs` carries the names field initializers reference (they
    /// inline into the ctor closure and must be captured).
    fn lower_ctor_closure(
        &self,
        params: &[Pat],
        body: &[Stmt],
        field_inits: &[String],
        scope: &ClassScope,
        derived: bool,
        field_refs: &HashSet<String>,
    ) -> String {
        let parameter_names = pattern_names(params);
        let mut referenced = stmts_referenced_names(body);
        referenced.extend(field_refs.iter().cloned());
        let mut captures = capture_names(&self.known_values, &parameter_names, &referenced);
        if scope.parent.is_some() && !captures.iter().any(|name| name == "__parent") {
            captures.push("__parent".to_string());
        }
        captures.sort();
        let capture_bindings = captures
            .iter()
            .map(|name| format!("let mut {name} = {name}.clone(); "))
            .collect::<String>();
        let mut ctx = self.child_dynamic_ctx();
        ctx.class_scope = Some(scope.clone());
        ctx.this_name = Some("__this".to_string());
        ctx.known_values.extend(captures.iter().cloned());
        ctx.bind_patterns(params);
        ctx.enter_fn_scope(params, body);
        let bindings = ctx.lower_closure_params(params);
        let prologue = ctx.hoist_fn_body_vars(body);
        let fields = field_inits.join(" ");
        let body_code = if derived {
            let mut code = String::new();
            let mut injected = false;
            for stmt in body {
                code.push_str(&ctx.lower_stmt(stmt));
                code.push(' ');
                if !injected && is_super_call_stmt(stmt) {
                    code.push_str(&fields);
                    code.push(' ');
                    injected = true;
                }
            }
            if !injected {
                code.push_str(&fields);
            }
            code
        } else {
            format!("{fields} {}", ctx.lower_stmts(body))
        };
        format!(
            "{{ {capture_bindings} w3cos_core::Value::function(move |__this: w3cos_core::Value, __args: Vec<w3cos_core::Value>| -> w3cos_core::Value {{ {bindings}{prologue}{body_code} __this }}) }}"
        )
    }

    fn lower_dynamic_local_pattern(
        &self,
        pattern: &Pat,
        source: &str,
        lines: &mut Vec<String>,
        indent: usize,
    ) {
        let pad = " ".repeat(indent);
        match pattern {
            Pat::Ident(ident) => {
                let name = sanitize_ident(&ident.id.sym.to_string());
                lines.push(format!("{pad}{}", self.bind_local(&name, source)));
            }
            Pat::Array(array) => {
                for (index, element) in array.elems.iter().enumerate() {
                    if let Some(element) = element {
                        let nested = format!("{source}.get_property({:?})", index.to_string());
                        self.lower_dynamic_local_pattern(element, &nested, lines, indent);
                    }
                }
            }
            Pat::Object(object) => {
                let excluded = object_pattern_excluded_keys(object);
                for property in &object.props {
                    match property {
                        ObjectPatProp::Assign(assign) => {
                            let name = sanitize_ident(&assign.key.sym.to_string());
                            let value =
                                format!("{source}.get_property({:?})", assign.key.sym.to_string());
                            let bound = if let Some(default) = &assign.value {
                                let fallback = self.lower_expr(default);
                                format!(
                                    "{{ let value = {value}; if value.is_undefined() {{ {fallback} }} else {{ value }} }}"
                                )
                            } else {
                                value
                            };
                            lines.push(format!("{pad}{}", self.bind_local(&name, &bound)));
                        }
                        ObjectPatProp::KeyValue(key_value) => {
                            let key = match &key_value.key {
                                PropName::Ident(ident) => ident.sym.to_string(),
                                PropName::Str(value) => wtf8_to_string(&value.value),
                                PropName::Num(value) => value.value.to_string(),
                                _ => continue,
                            };
                            let nested = format!("{source}.get_property({key:?})");
                            self.lower_dynamic_local_pattern(
                                &key_value.value,
                                &nested,
                                lines,
                                indent,
                            );
                        }
                        ObjectPatProp::Rest(rest) => {
                            let rest_source = format!("{source}.object_rest(&[{excluded}])");
                            self.lower_dynamic_local_pattern(&rest.arg, &rest_source, lines, indent)
                        }
                    }
                }
            }
            Pat::Assign(assign) => {
                let fallback = self.lower_expr(&assign.right);
                let value = format!(
                    "{{ let value = {source}; if value.is_undefined() {{ {fallback} }} else {{ value }} }}"
                );
                self.lower_dynamic_local_pattern(&assign.left, &value, lines, indent);
            }
            Pat::Rest(rest) => self.lower_dynamic_local_pattern(&rest.arg, source, lines, indent),
            _ => {}
        }
    }

    /// Assignment-form destructuring for `[a, b] = arr` / `({x, y} = obj)`
    /// (no `let`: the bindings already exist in the enclosing scope).
    fn lower_dynamic_assign_target(
        &self,
        target: &AssignTarget,
        source: &str,
        lines: &mut Vec<String>,
    ) {
        match target {
            AssignTarget::Simple(simple) => match simple {
                SimpleAssignTarget::Ident(ident) => {
                    let name = sanitize_ident(&ident.id.sym.to_string());
                    lines.push(self.assign_local(&name, source));
                }
                SimpleAssignTarget::Member(member) => {
                    let object = self.lower_expr(&member.obj);
                    let key = match &member.prop {
                        MemberProp::Ident(ident) => format!("{:?}", ident.sym.to_string()),
                        MemberProp::Computed(computed) => {
                            format!("{}.to_js_string()", self.lower_expr(&computed.expr))
                        }
                        MemberProp::PrivateName(name) => {
                            format!("{:?}", self.private_key(name))
                        }
                    };
                    lines.push(format!("{object}.set_property(&{key}, {source});"));
                }
                _ => {}
            },
            AssignTarget::Pat(pat) => match pat {
                AssignTargetPat::Array(array) => {
                    for (index, element) in array.elems.iter().enumerate() {
                        if let Some(element) = element {
                            let nested = format!("{source}.get_property({:?})", index.to_string());
                            self.lower_dynamic_assign_pattern(element, &nested, lines);
                        }
                    }
                }
                AssignTargetPat::Object(object) => {
                    for property in &object.props {
                        match property {
                            ObjectPatProp::Assign(assign) => {
                                let name = sanitize_ident(&assign.key.sym.to_string());
                                let value = format!(
                                    "{source}.get_property({:?})",
                                    assign.key.sym.to_string()
                                );
                                let assigned = if let Some(default) = &assign.value {
                                    let fallback = self.lower_expr(default);
                                    format!(
                                        "{{ let value = {value}; if value.is_undefined() {{ {fallback} }} else {{ value }} }}"
                                    )
                                } else {
                                    value
                                };
                                lines.push(self.assign_local(&name, &assigned));
                            }
                            ObjectPatProp::KeyValue(key_value) => {
                                let key = match &key_value.key {
                                    PropName::Ident(ident) => ident.sym.to_string(),
                                    PropName::Str(value) => wtf8_to_string(&value.value),
                                    PropName::Num(value) => value.value.to_string(),
                                    _ => continue,
                                };
                                let nested = format!("{source}.get_property({key:?})");
                                self.lower_dynamic_assign_pattern(&key_value.value, &nested, lines);
                            }
                            ObjectPatProp::Rest(rest) => {
                                self.lower_dynamic_assign_pattern(&rest.arg, source, lines)
                            }
                        }
                    }
                }
                AssignTargetPat::Invalid(_) => {}
            },
        }
    }

    /// The `Pat`-based recursion backing [`Self::lower_dynamic_assign_target`].
    fn lower_dynamic_assign_pattern(&self, pattern: &Pat, source: &str, lines: &mut Vec<String>) {
        match pattern {
            Pat::Ident(ident) => {
                let name = sanitize_ident(&ident.id.sym.to_string());
                lines.push(self.assign_local(&name, source));
            }
            Pat::Array(array) => {
                for (index, element) in array.elems.iter().enumerate() {
                    if let Some(element) = element {
                        let nested = format!("{source}.get_property({:?})", index.to_string());
                        self.lower_dynamic_assign_pattern(element, &nested, lines);
                    }
                }
            }
            Pat::Object(object) => {
                for property in &object.props {
                    match property {
                        ObjectPatProp::Assign(assign) => {
                            let name = sanitize_ident(&assign.key.sym.to_string());
                            let value =
                                format!("{source}.get_property({:?})", assign.key.sym.to_string());
                            let assigned = if let Some(default) = &assign.value {
                                let fallback = self.lower_expr(default);
                                format!(
                                    "{{ let value = {value}; if value.is_undefined() {{ {fallback} }} else {{ value }} }}"
                                )
                            } else {
                                value
                            };
                            lines.push(self.assign_local(&name, &assigned));
                        }
                        ObjectPatProp::KeyValue(key_value) => {
                            let key = match &key_value.key {
                                PropName::Ident(ident) => ident.sym.to_string(),
                                PropName::Str(value) => wtf8_to_string(&value.value),
                                PropName::Num(value) => value.value.to_string(),
                                _ => continue,
                            };
                            let nested = format!("{source}.get_property({key:?})");
                            self.lower_dynamic_assign_pattern(&key_value.value, &nested, lines);
                        }
                        ObjectPatProp::Rest(rest) => {
                            self.lower_dynamic_assign_pattern(&rest.arg, source, lines)
                        }
                    }
                }
            }
            Pat::Assign(assign) => {
                let fallback = self.lower_expr(&assign.right);
                let value = format!(
                    "{{ let value = {source}; if value.is_undefined() {{ {fallback} }} else {{ value }} }}"
                );
                self.lower_dynamic_assign_pattern(&assign.left, &value, lines);
            }
            Pat::Rest(rest) => self.lower_dynamic_assign_pattern(&rest.arg, source, lines),
            _ => {}
        }
    }
}

/// The dynamic lowerer used to clone every value visible in the surrounding
/// function into every generated Rust closure. JavaScript engines trace only
/// actual lexical captures; reproducing the all-values behavior with `Rc`
/// both bloats callback environments and can create otherwise-unreachable
/// cycles. The body is lowered with all candidates in scope first, then the
/// emitted Rust identifiers provide an exact conservative capture filter.
fn referenced_captures(mut candidates: Vec<String>, body: &str) -> Vec<String> {
    candidates.retain(|candidate| contains_rust_identifier(body, candidate));
    candidates.sort_unstable();
    candidates
}

fn contains_rust_identifier(source: &str, identifier: &str) -> bool {
    source.match_indices(identifier).any(|(start, _)| {
        let end = start + identifier.len();
        let before = source[..start].chars().next_back();
        let after = source[end..].chars().next();
        !before.is_some_and(|ch| ch == '_' || ch.is_ascii_alphanumeric())
            && !after.is_some_and(|ch| ch == '_' || ch.is_ascii_alphanumeric())
    })
}

fn lower_flow_control_target(
    keyword: &str,
    default_target: Option<&str>,
    named_targets: &[(String, String)],
) -> String {
    lower_flow_control_target_or(
        keyword,
        default_target,
        named_targets,
        &format!("panic!(\"{keyword} escaped a non-{keyword}able try block\");"),
    )
}

fn lower_flow_control_target_or(
    keyword: &str,
    default_target: Option<&str>,
    named_targets: &[(String, String)],
    fallback: &str,
) -> String {
    let mut arms = named_targets
        .iter()
        .rev()
        .map(|(js, rust)| format!("{js:?} => {keyword} '{rust},"))
        .collect::<Vec<_>>();
    arms.push(match default_target {
        Some(rust) => format!("_ => {keyword} '{rust},"),
        None => format!("_ => {{ {fallback} }}"),
    });
    format!("match label {{ {} }}", arms.join(" "))
}

fn lower_closure_params(
    params: &[Pat],
    renames: &[(String, String)],
    value_bindings: &HashSet<String>,
) -> String {
    let mut output = String::new();
    for (index, pattern) in params.iter().enumerate() {
        let source =
            format!("__args.get({index}).cloned().unwrap_or(w3cos_core::Value::Undefined)");
        lower_closure_pattern(pattern, &source, &mut output, renames, value_bindings);
    }
    output
}

/// Collect fn-body-level declaration names for hoisting-style
/// predeclaration: `let`/`const`/`var` declarators AND fn declarations (JS
/// fn declarations hoist to the enclosing fn body's top). Recurses into
/// blocks, loops, if/switch/try bodies, but not into nested functions or
/// classes (a nested fn has its own hoisting scope). Block-scoping shadowing
/// subtleties are traded for totality (closures may reference a binding
/// before its declaration line, e.g. `const d = toDisposable(() => d)`).
fn collect_hoisted_var_names(stmts: &[Stmt], names: &mut HashSet<String>) {
    for stmt in stmts {
        match stmt {
            Stmt::Decl(Decl::Var(var)) => {
                for declarator in &var.decls {
                    collect_pattern_names(&declarator.name, names);
                }
            }
            Stmt::Decl(Decl::Fn(f)) => {
                names.insert(sanitize_ident(&f.ident.sym.to_string()));
            }
            Stmt::Decl(Decl::Class(c)) => {
                // Class declarations hoist too: forward references (a fn
                // declared before the class but called after it) and class
                // self-references inside methods resolve through the
                // pre-declared slot.
                names.insert(sanitize_ident(&c.ident.sym.to_string()));
            }
            Stmt::Block(block) => collect_hoisted_var_names(&block.stmts, names),
            Stmt::If(s) => {
                collect_hoisted_var_names(std::slice::from_ref(&s.cons), names);
                if let Some(alt) = &s.alt {
                    collect_hoisted_var_names(std::slice::from_ref(alt), names);
                }
            }
            Stmt::For(s) => {
                if let Some(VarDeclOrExpr::VarDecl(var)) = &s.init {
                    for declarator in &var.decls {
                        collect_pattern_names(&declarator.name, names);
                    }
                }
                collect_hoisted_var_names(std::slice::from_ref(&s.body), names);
            }
            Stmt::ForIn(s) => {
                if let ForHead::VarDecl(var) = &s.left {
                    for declarator in &var.decls {
                        collect_pattern_names(&declarator.name, names);
                    }
                }
                collect_hoisted_var_names(std::slice::from_ref(&s.body), names);
            }
            Stmt::ForOf(s) => {
                if let ForHead::VarDecl(var) = &s.left {
                    for declarator in &var.decls {
                        collect_pattern_names(&declarator.name, names);
                    }
                }
                collect_hoisted_var_names(std::slice::from_ref(&s.body), names);
            }
            Stmt::While(s) => collect_hoisted_var_names(std::slice::from_ref(&s.body), names),
            Stmt::DoWhile(s) => collect_hoisted_var_names(std::slice::from_ref(&s.body), names),
            Stmt::Switch(s) => {
                for case in &s.cases {
                    collect_hoisted_var_names(&case.cons, names);
                }
            }
            Stmt::Try(s) => {
                collect_hoisted_var_names(&s.block.stmts, names);
                if let Some(handler) = &s.handler {
                    collect_hoisted_var_names(&handler.body.stmts, names);
                }
                if let Some(finalizer) = &s.finalizer {
                    collect_hoisted_var_names(&finalizer.stmts, names);
                }
            }
            Stmt::Labeled(s) => collect_hoisted_var_names(std::slice::from_ref(&s.body), names),
            _ => {}
        }
    }
}

/// Return the member reference that supplies an optional call's receiver.
/// SWC wraps chained forms such as `a?.b?.method()` in nested OptChain nodes.
fn optional_call_member(expr: &Expr) -> Option<&MemberExpr> {
    match expr {
        Expr::Member(member) => Some(member),
        Expr::OptChain(chain) => match chain.base.as_ref() {
            OptChainBase::Member(member) => Some(member),
            OptChainBase::Call(_) => None,
        },
        Expr::Paren(paren) => optional_call_member(&paren.expr),
        _ => None,
    }
}

fn pattern_names(patterns: &[Pat]) -> HashSet<String> {
    let mut names = HashSet::new();
    for pattern in patterns {
        collect_pattern_names(pattern, &mut names);
    }
    names
}

fn collect_pattern_names(pattern: &Pat, names: &mut HashSet<String>) {
    match pattern {
        Pat::Ident(identifier) => {
            names.insert(sanitize_ident(&identifier.id.sym.to_string()));
        }
        Pat::Array(array) => {
            for element in array.elems.iter().flatten() {
                collect_pattern_names(element, names);
            }
        }
        Pat::Object(object) => {
            for property in &object.props {
                match property {
                    ObjectPatProp::Assign(assign) => {
                        names.insert(sanitize_ident(&assign.key.sym.to_string()));
                    }
                    ObjectPatProp::KeyValue(key_value) => {
                        collect_pattern_names(&key_value.value, names)
                    }
                    ObjectPatProp::Rest(rest) => collect_pattern_names(&rest.arg, names),
                }
            }
        }
        Pat::Assign(assign) => collect_pattern_names(&assign.left, names),
        Pat::Rest(rest) => collect_pattern_names(&rest.arg, names),
        _ => {}
    }
}

fn lower_closure_pattern(
    pattern: &Pat,
    source: &str,
    output: &mut String,
    renames: &[(String, String)],
    value_bindings: &HashSet<String>,
) {
    match pattern {
        Pat::Ident(ident) => output.push_str(&format!(
            "let {} = {source}; ",
            sanitize_ident(&ident.id.sym.to_string())
        )),
        Pat::Array(array) => {
            for (index, element) in array.elems.iter().enumerate() {
                if let Some(element) = element {
                    let nested = format!("{source}.get_property({:?})", index.to_string());
                    lower_closure_pattern(element, &nested, output, renames, value_bindings);
                }
            }
        }
        Pat::Object(object) => {
            let excluded = object_pattern_excluded_keys(object);
            for property in &object.props {
                match property {
                    ObjectPatProp::Assign(assign) => {
                        let name = sanitize_ident(&assign.key.sym.to_string());
                        let value =
                            format!("{source}.get_property({:?})", assign.key.sym.to_string());
                        if let Some(default) = &assign.value {
                            let ctx = LowerCtx::new_dynamic_with_bindings(
                                renames.to_vec(),
                                value_bindings.clone(),
                            );
                            let fallback = ctx.lower_expr(default);
                            output.push_str(&format!(
                                "let {name} = {{ let value = {value}; if value.is_undefined() {{ {fallback} }} else {{ value }} }}; "
                            ));
                        } else {
                            output.push_str(&format!("let {name} = {value}; "));
                        }
                    }
                    ObjectPatProp::KeyValue(key_value) => {
                        let key = match &key_value.key {
                            PropName::Ident(ident) => ident.sym.to_string(),
                            PropName::Str(value) => wtf8_to_string(&value.value),
                            PropName::Num(value) => value.value.to_string(),
                            _ => continue,
                        };
                        let nested = format!("{source}.get_property({key:?})");
                        lower_closure_pattern(
                            &key_value.value,
                            &nested,
                            output,
                            renames,
                            value_bindings,
                        );
                    }
                    ObjectPatProp::Rest(rest) => {
                        let rest_source = format!("{source}.object_rest(&[{excluded}])");
                        lower_closure_pattern(
                            &rest.arg,
                            &rest_source,
                            output,
                            renames,
                            value_bindings,
                        )
                    }
                }
            }
        }
        Pat::Assign(assign) => {
            let ctx = LowerCtx::new_dynamic_with_bindings(renames.to_vec(), value_bindings.clone());
            let fallback = ctx.lower_expr(&assign.right);
            let nested = format!(
                "{{ let value = {source}; if value.is_undefined() {{ {fallback} }} else {{ value }} }}"
            );
            lower_closure_pattern(&assign.left, &nested, output, renames, value_bindings);
        }
        Pat::Rest(rest) => {
            lower_closure_pattern(&rest.arg, source, output, renames, value_bindings)
        }
        _ => {}
    }
}

fn object_pattern_excluded_keys(object: &ObjectPat) -> String {
    object
        .props
        .iter()
        .filter_map(|property| match property {
            ObjectPatProp::Assign(assign) => Some(assign.key.sym.to_string()),
            ObjectPatProp::KeyValue(key_value) => match &key_value.key {
                PropName::Ident(ident) => Some(ident.sym.to_string()),
                PropName::Str(value) => Some(wtf8_to_string(&value.value)),
                PropName::Num(value) => Some(value.value.to_string()),
                _ => None,
            },
            ObjectPatProp::Rest(_) => None,
        })
        .map(|key| format!("{key:?}"))
        .collect::<Vec<_>>()
        .join(", ")
}
fn atom_str(atom: &impl ToString) -> String {
    sanitize_ident(&atom.to_string())
}

/// Is this statement a top-level `super(...)` call (derived-ctor marker)?
pub(crate) fn is_super_call_stmt(stmt: &Stmt) -> bool {
    matches!(
        stmt,
        Stmt::Expr(expr_stmt)
            if matches!(
                expr_stmt.expr.as_ref(),
                Expr::Call(call) if matches!(call.callee, Callee::Super(_))
            )
    )
}

/// Map a compound assignment operator (`+=`, `-=`, ...) to the `Value`
/// method implementing it. Plain `=` and logical-assign ops return `None`.
fn compound_assign_op(op: AssignOp) -> Option<&'static str> {
    match op {
        AssignOp::AddAssign => Some("js_add"),
        AssignOp::SubAssign => Some("js_sub"),
        AssignOp::MulAssign => Some("js_mul"),
        AssignOp::DivAssign => Some("js_div"),
        AssignOp::ModAssign => Some("js_rem"),
        AssignOp::ExpAssign => Some("js_pow"),
        AssignOp::BitAndAssign => Some("js_bitand"),
        AssignOp::BitOrAssign => Some("js_bitor"),
        AssignOp::BitXorAssign => Some("js_bitxor"),
        AssignOp::LShiftAssign => Some("js_shl"),
        AssignOp::RShiftAssign => Some("js_shr"),
        AssignOp::ZeroFillRShiftAssign => Some("js_ushr"),
        _ => None,
    }
}

/// Wtf8Atom (string literal values) has no Display. Its Debug form is a
/// quoted, escaped string, so decode that representation instead of trimming
/// quotes: `trim_matches` also removed a real trailing `"` from values such
/// as Monaco's `class="` fragment.
fn wtf8_to_string(atom: &impl std::fmt::Debug) -> String {
    let debug = format!("{:?}", atom);
    serde_json::from_str(&debug).unwrap_or_else(|_| {
        debug
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .unwrap_or(&debug)
            .to_string()
    })
}

/// Sanitize a JS identifier to be valid Rust: replace `$` with `_d`, leading
/// digits get prefixed with `_`, and Rust keywords get suffixed with `_`.
pub fn sanitize_ident(name: &str) -> String {
    if name.is_empty() || name == "_" {
        return "_unused".to_string();
    }
    let mut out = String::with_capacity(name.len());
    for (i, ch) in name.chars().enumerate() {
        if ch == '$' {
            out.push_str("_d");
        } else if i == 0 && ch.is_ascii_digit() {
            out.push('_');
            out.push(ch);
        } else if ch.is_alphanumeric() || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    // Avoid Rust reserved keywords
    match out.as_str() {
        "self" | "Self" | "super" | "crate" | "type" | "fn" | "mod" | "pub" | "let" | "mut"
        | "ref" | "use" | "impl" | "trait" | "struct" | "enum" | "match" | "if" | "else"
        | "for" | "while" | "loop" | "break" | "continue" | "return" | "where" | "as" | "in"
        | "move" | "async" | "await" | "dyn" | "static" | "const" | "unsafe" | "extern"
        | "true" | "false" | "override" | "final" | "try" | "yield" | "abstract" | "become"
        | "box" | "do" | "macro" | "priv" | "typeof" | "unsized" | "virtual" | "gen" => {
            out.push('_');
            out
        }
        _ => out,
    }
}

/// Best-effort scan for `await` inside a try statement (any of the three
/// bodies). Used to flag the unsound `AssertUnwindSafe`-across-await case.
fn try_has_await(try_stmt: &TryStmt) -> bool {
    stmts_have_await(&try_stmt.block.stmts)
        || try_stmt
            .handler
            .as_ref()
            .is_some_and(|handler| stmts_have_await(&handler.body.stmts))
        || try_stmt
            .finalizer
            .as_ref()
            .is_some_and(|finalizer| stmts_have_await(&finalizer.stmts))
}

fn stmts_have_await(stmts: &[Stmt]) -> bool {
    stmts.iter().any(stmt_has_await)
}

fn stmt_has_await(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Expr(s) => expr_has_await(&s.expr),
        Stmt::Return(s) => s.arg.as_deref().is_some_and(expr_has_await),
        Stmt::Throw(s) => expr_has_await(&s.arg),
        Stmt::Decl(decl) => decl_has_await(decl),
        Stmt::Block(b) => stmts_have_await(&b.stmts),
        Stmt::If(s) => {
            expr_has_await(&s.test)
                || stmt_has_await(&s.cons)
                || s.alt.as_deref().is_some_and(stmt_has_await)
        }
        Stmt::For(s) => {
            s.init.as_ref().is_some_and(|init| match init {
                VarDeclOrExpr::VarDecl(decl) => decl
                    .decls
                    .iter()
                    .any(|d| d.init.as_deref().is_some_and(expr_has_await)),
                VarDeclOrExpr::Expr(expr) => expr_has_await(expr),
            }) || s.test.as_deref().is_some_and(expr_has_await)
                || s.update.as_deref().is_some_and(expr_has_await)
                || stmt_has_await(&s.body)
        }
        Stmt::While(s) => expr_has_await(&s.test) || stmt_has_await(&s.body),
        Stmt::DoWhile(s) => stmt_has_await(&s.body) || expr_has_await(&s.test),
        Stmt::ForIn(s) => expr_has_await(&s.right) || stmt_has_await(&s.body),
        Stmt::ForOf(s) => expr_has_await(&s.right) || stmt_has_await(&s.body),
        Stmt::Switch(s) => {
            expr_has_await(&s.discriminant)
                || s.cases.iter().any(|case| {
                    case.test.as_deref().is_some_and(expr_has_await) || stmts_have_await(&case.cons)
                })
        }
        Stmt::Try(s) => try_has_await(s),
        Stmt::Labeled(s) => stmt_has_await(&s.body),
        _ => false,
    }
}

fn decl_has_await(decl: &Decl) -> bool {
    match decl {
        Decl::Var(var) => var
            .decls
            .iter()
            .any(|d| d.init.as_deref().is_some_and(expr_has_await)),
        Decl::Fn(f) => f
            .function
            .body
            .as_ref()
            .is_some_and(|b| stmts_have_await(&b.stmts)),
        Decl::Class(c) => class_has_await(&c.class),
        _ => false,
    }
}

fn class_has_await(class: &Class) -> bool {
    class.body.iter().any(|member| match member {
        ClassMember::Constructor(c) => c.body.as_ref().is_some_and(|b| stmts_have_await(&b.stmts)),
        ClassMember::Method(m) => m
            .function
            .body
            .as_ref()
            .is_some_and(|b| stmts_have_await(&b.stmts)),
        ClassMember::PrivateMethod(m) => m
            .function
            .body
            .as_ref()
            .is_some_and(|b| stmts_have_await(&b.stmts)),
        ClassMember::ClassProp(p) => p.value.as_deref().is_some_and(expr_has_await),
        ClassMember::PrivateProp(p) => p.value.as_deref().is_some_and(expr_has_await),
        ClassMember::StaticBlock(b) => stmts_have_await(&b.body.stmts),
        _ => false,
    })
}

fn expr_has_await(expr: &Expr) -> bool {
    match expr {
        Expr::Await(_) => true,
        Expr::Call(call) => {
            call.args.iter().any(|arg| expr_has_await(&arg.expr))
                || matches!(&call.callee, Callee::Expr(callee) if expr_has_await(callee))
        }
        Expr::New(new_expr) => {
            expr_has_await(&new_expr.callee)
                || new_expr
                    .args
                    .as_ref()
                    .is_some_and(|args| args.iter().any(|arg| expr_has_await(&arg.expr)))
        }
        Expr::Member(member) => expr_has_await(&member.obj),
        Expr::Bin(bin) => expr_has_await(&bin.left) || expr_has_await(&bin.right),
        Expr::Unary(unary) => expr_has_await(&unary.arg),
        Expr::Update(update) => expr_has_await(&update.arg),
        Expr::Assign(assign) => expr_has_await(&assign.right),
        Expr::Cond(cond) => {
            expr_has_await(&cond.test) || expr_has_await(&cond.cons) || expr_has_await(&cond.alt)
        }
        Expr::Paren(paren) => expr_has_await(&paren.expr),
        Expr::Seq(seq) => seq.exprs.iter().any(|e| expr_has_await(e)),
        Expr::Tpl(tpl) => tpl.exprs.iter().any(|e| expr_has_await(e)),
        Expr::Arrow(arrow) => match arrow.body.as_ref() {
            BlockStmtOrExpr::BlockStmt(block) => stmts_have_await(&block.stmts),
            BlockStmtOrExpr::Expr(expr) => expr_has_await(expr),
        },
        Expr::Fn(f) => f
            .function
            .body
            .as_ref()
            .is_some_and(|b| stmts_have_await(&b.stmts)),
        Expr::Object(obj) => obj.props.iter().any(|prop| match prop {
            PropOrSpread::Spread(spread) => expr_has_await(&spread.expr),
            PropOrSpread::Prop(prop) => match prop.as_ref() {
                Prop::KeyValue(kv) => expr_has_await(&kv.value),
                Prop::Method(method) => method
                    .function
                    .body
                    .as_ref()
                    .is_some_and(|b| stmts_have_await(&b.stmts)),
                Prop::Getter(getter) => getter
                    .body
                    .as_ref()
                    .is_some_and(|b| stmts_have_await(&b.stmts)),
                Prop::Setter(setter) => setter
                    .body
                    .as_ref()
                    .is_some_and(|b| stmts_have_await(&b.stmts)),
                _ => false,
            },
        }),
        Expr::Array(arr) => arr
            .elems
            .iter()
            .flatten()
            .any(|elem| expr_has_await(&elem.expr)),
        Expr::OptChain(opt) => match opt.base.as_ref() {
            OptChainBase::Member(member) => expr_has_await(&member.obj),
            OptChainBase::Call(call) => {
                expr_has_await(&call.callee)
                    || call.args.iter().any(|arg| expr_has_await(&arg.expr))
            }
        },
        Expr::TaggedTpl(tagged) => {
            expr_has_await(&tagged.tag) || tagged.tpl.exprs.iter().any(|e| expr_has_await(e))
        }
        Expr::Class(class_expr) => class_has_await(&class_expr.class),
        Expr::Yield(yield_expr) => yield_expr.arg.as_deref().is_some_and(expr_has_await),
        _ => false,
    }
}

/// Map an unshadowed bare JS identifier to the Rust expression producing its
/// global value in generated ESM modules. `document`/`window` and friends go
/// to the real jsdom bridge (the fake `w3cos_core::builtins::document` is not
/// used in ESM modules); Promise/JSON/atob and friends map to w3cos-core
/// builtin facades.
fn global_value_expr(name: &str) -> Option<String> {
    let expr = match name {
        "undefined" => "w3cos_core::Value::Undefined".to_string(),
        "NaN" => "w3cos_core::Value::Number(f64::NAN)".to_string(),
        "Infinity" => "w3cos_core::Value::Number(f64::INFINITY)".to_string(),
        // `Error` as a value (`class X extends Error`, `e instanceof Error` —
        // instanceof degrades to false): a constructor function value wrapping
        // the builtin. `new Error(...)` and `Error(...)` are special-cased
        // elsewhere and unaffected.
        "Error" => {
            "w3cos_core::Value::function(|_this, __args| w3cos_core::Error::new(__args).0)"
                .to_string()
        }
        "RangeError" => {
            "w3cos_core::Value::function(|_this, __args| w3cos_core::RangeError(__args))"
                .to_string()
        }
        // `Map` as a value: the real ES6 Map class (SameValueZero identity
        // keys, insertion order, prototype-linked instances so
        // `x instanceof Map` works). `new Map(...)` resolves to the same
        // class value and goes through class::construct.
        "Map" => "w3cos_core::collections::map_class()".to_string(),
        "RegExp" => "w3cos_core::regexp::regexp_class()".to_string(),
        // `Array` as a value: callable facade — calling it mirrors the
        // `new Array` semantics (single numeric arg = length); the statics
        // implemented by the core builtin (currently `from`) are installed as
        // properties so `Array.from(x)` keeps working.
        "Array" => "w3cos_core::Value::callable(::std::collections::HashMap::from([(\"from\".to_string(), w3cos_core::Value::function(|_this, __args| w3cos_core::Array.call_method(\"from\", __args))), (\"isArray\".to_string(), w3cos_core::Value::function(|_this, __args| w3cos_core::Value::Bool(matches!(__args.first(), Some(w3cos_core::Value::Array(_))))))]), |_this, __args| { if __args.len() == 1 && __args[0].is_number() { let n = __args[0].to_number() as usize; w3cos_core::Value::array(vec![w3cos_core::Value::Undefined; n]) } else { w3cos_core::Value::array(__args) } })"
            .to_string(),
        // `Object` as a value (`x.constructor === Object`, `Object(x)`):
        // callable facade with the core builtin's statics as properties, so
        // `Object.keys(x)` / `Object.values(x)` / `Object.is(a, b)` keep
        // working through plain member calls. `create` ignores the prototype
        // argument (fresh empty object); `assign` merges own enumerable
        // properties; `entries`/`getOwnPropertyNames` mirror `keys`;
        // `freeze` is a pass-through.
        "Object" => "w3cos_core::Value::callable(::std::collections::HashMap::from([(\"keys\".to_string(), w3cos_core::Value::function(|_this, __args| w3cos_core::Object.call_method(\"keys\", __args))), (\"values\".to_string(), w3cos_core::Value::function(|_this, __args| w3cos_core::Object.call_method(\"values\", __args))), (\"is\".to_string(), w3cos_core::Value::function(|_this, __args| w3cos_core::Object.call_method(\"is\", __args))), (\"create\".to_string(), w3cos_core::Value::function(|_this, __args| w3cos_core::Value::object(::std::collections::HashMap::new()))), (\"getPrototypeOf\".to_string(), w3cos_core::Value::function(|_this, __args| w3cos_core::class::get_prototype_of(&__args.first().cloned().unwrap_or(w3cos_core::Value::Undefined)))), (\"getOwnPropertyDescriptor\".to_string(), w3cos_core::Value::function(|_this, __args| { let obj = __args.first().cloned().unwrap_or(w3cos_core::Value::Undefined); let key = __args.get(1).cloned().unwrap_or(w3cos_core::Value::Undefined).to_js_string(); w3cos_core::class::get_own_property_descriptor(&obj, &key) })), (\"defineProperty\".to_string(), w3cos_core::Value::function(|_this, __args| { let obj = __args.first().cloned().unwrap_or(w3cos_core::Value::Undefined); let key = __args.get(1).cloned().unwrap_or(w3cos_core::Value::Undefined).to_js_string(); let descriptor = __args.get(2).cloned().unwrap_or(w3cos_core::Value::Undefined); w3cos_core::class::define_property(&obj, &key, &descriptor) })), (\"freeze\".to_string(), w3cos_core::Value::function(|_this, __args| __args.first().cloned().unwrap_or(w3cos_core::Value::Undefined))), (\"getOwnPropertyNames\".to_string(), w3cos_core::Value::function(|_this, __args| w3cos_core::Object.call_method(\"keys\", __args))), (\"assign\".to_string(), w3cos_core::Value::function(|_this, __args| { let target = __args.first().cloned().unwrap_or(w3cos_core::Value::Undefined); for source in __args.iter().skip(1) { for key in w3cos_core::Object.call_method(\"keys\", vec![source.clone()]).iter() { let k = key.to_js_string(); let v = source.get_property(&k); target.set_property(&k, v); } } target })), (\"entries\".to_string(), w3cos_core::Value::function(|_this, __args| { let obj = __args.first().cloned().unwrap_or(w3cos_core::Value::Undefined); let mut out = Vec::new(); for key in w3cos_core::Object.call_method(\"keys\", vec![obj.clone()]).iter() { let k = key.to_js_string(); out.push(w3cos_core::Value::array(vec![w3cos_core::Value::from(k.clone()), obj.get_property(&k)])); } w3cos_core::Value::array(out) }))]), |_this, __args| __args.first().cloned().unwrap_or_else(|| w3cos_core::Value::object(::std::collections::HashMap::new())))"
            .to_string(),
        "document" => "w3cos_runtime::jsdom::document_value()".to_string(),
        "window" | "self" | "globalThis" => {
            "w3cos_runtime::jsdom::window_value()".to_string()
        }
        // Property globals: read off the jsdom window singleton.
        "navigator" | "localStorage" | "sessionStorage" | "indexedDB" | "IDBKeyRange" | "performance" | "location"
        | "screen" | "crypto" | "navigation" | "reportError" | "setImmediate"
        | "MessageChannel" | "__REACT_DEVTOOLS_GLOBAL_HOOK__" => {
            format!("w3cos_runtime::jsdom::window_value().get_property({name:?})")
        }
        // Scheduling/utility globals: the jsdom window holds them as function
        // values, so a bare reference is just the property read (calling it
        // goes through the normal `Value::call` path).
        "setTimeout" | "setInterval" | "clearTimeout" | "clearInterval" | "checkDCE"
        | "requestAnimationFrame" | "cancelAnimationFrame" | "queueMicrotask"
        | "matchMedia" | "getComputedStyle" | "getSelection" => {
            format!("w3cos_runtime::jsdom::window_value().get_property({name:?})")
        }
        "atob" => {
            "w3cos_core::Value::function(|_this, __args| w3cos_core::web::atob(__args))"
                .to_string()
        }
        "btoa" => {
            "w3cos_core::Value::function(|_this, __args| w3cos_core::web::btoa(__args))"
                .to_string()
        }
        "structuredClone" => {
            "w3cos_core::Value::function(|_this, __args| w3cos_core::web::structured_clone(__args))"
                .to_string()
        }
        // Facade objects exposing the builtin entry points as properties, so
        // `Promise.resolve(x)` / `JSON.parse(s)` work through plain member
        // calls (and the facades can be passed around as values).
        "Promise" => "w3cos_core::Value::object(::std::collections::HashMap::from([(\"resolve\".to_string(), w3cos_core::Value::function(|_this, __args| w3cos_core::promise::resolve(__args))), (\"reject\".to_string(), w3cos_core::Value::function(|_this, __args| w3cos_core::promise::reject(__args))), (\"all\".to_string(), w3cos_core::Value::function(|_this, __args| w3cos_core::promise::all(__args))), (\"race\".to_string(), w3cos_core::Value::function(|_this, __args| w3cos_core::promise::race(__args)))]))".to_string(),
        "JSON" => "w3cos_core::Value::object(::std::collections::HashMap::from([(\"parse\".to_string(), w3cos_core::Value::function(|_this, __args| w3cos_core::json::parse(__args))), (\"stringify\".to_string(), w3cos_core::Value::function(|_this, __args| w3cos_core::json::stringify(__args)))]))".to_string(),
        // Intl is not implemented; a harmless empty object keeps references
        // total (member access yields Undefined).
        "Intl" => "w3cos_core::Value::object(::std::collections::HashMap::new()) /* Intl stub */"
            .to_string(),
        // `arguments` — the current fn's argument list as an array value.
        // Only valid where `__args` is in scope (lowered fns/closures).
        "arguments" => "w3cos_core::Value::array(__args.clone())".to_string(),
        // Conversion globals as function values: `String(x)`/`Number(x)`/
        // `Boolean(x)`/`isNaN(x)`/`isFinite(x)` work through the normal call
        // path; static members (e.g. `String.fromCharCode`) degrade to
        // Undefined via property access on the function value.
        "String" => "w3cos_core::Value::function(|_this, __args| w3cos_core::Value::from(__args.first().cloned().unwrap_or(w3cos_core::Value::Undefined).to_js_string()))".to_string(),
        "Number" => "w3cos_core::Value::function(|_this, __args| w3cos_core::Value::Number(__args.first().cloned().unwrap_or(w3cos_core::Value::Undefined).to_number()))".to_string(),
        "Boolean" => "w3cos_core::Value::function(|_this, __args| w3cos_core::Value::Bool(__args.first().cloned().unwrap_or(w3cos_core::Value::Undefined).to_bool()))".to_string(),
        "isNaN" => "w3cos_core::Value::function(|_this, __args| w3cos_core::Value::Bool(__args.first().cloned().unwrap_or(w3cos_core::Value::Undefined).to_number().is_nan()))".to_string(),
        "isFinite" => "w3cos_core::Value::function(|_this, __args| w3cos_core::Value::Bool(__args.first().cloned().unwrap_or(w3cos_core::Value::Undefined).to_number().is_finite()))".to_string(),
        // `Set` as a value: the real ES6 Set class (see Map above);
        // `new Set(...)` routes through class::construct on this class value.
        "Set" => "w3cos_core::collections::set_class()".to_string(),
        // Weak collections: no weak semantics in v1 — they alias Map/Set.
        "WeakMap" => "w3cos_core::collections::weak_map_class()".to_string(),
        "WeakSet" => "w3cos_core::collections::weak_set_class()".to_string(),
        // Dynamic Proxy constructor backed by w3cos-core's proxy traps.
        "Proxy" => "w3cos_core::proxy_class()".to_string(),
        "TextDecoder" => "w3cos_core::web::text_decoder_class()".to_string(),
        "Date" => "w3cos_core::web::date_class()".to_string(),
        "Uint8Array" | "Uint8ClampedArray" | "Int8Array" | "Uint16Array" | "Int16Array"
        | "Uint32Array" | "Int32Array" | "Float32Array" | "Float64Array" | "BigInt64Array"
        | "BigUint64Array" => "w3cos_core::collections::typed_array_class()".to_string(),
        // `Reflect` facade: `Reflect.construct(target, args)` routes through
        // the class runtime (Monaco's InstantiationService._createInstance
        // builds every service through it). The optional newTarget argument
        // is ignored; all other Reflect members degrade to Undefined.
        "Reflect" => "w3cos_core::Value::object(::std::collections::HashMap::from([(\"construct\".to_string(), w3cos_core::Value::function(|_this, __args| { let __target = __args.first().cloned().unwrap_or(w3cos_core::Value::Undefined); let __ctor_args: Vec<w3cos_core::Value> = __args.get(1).map(|__a| __a.iter().collect()).unwrap_or_default(); w3cos_core::class::construct(&__target, __ctor_args) }))]))".to_string(),
        // Symbols use collision-resistant string sentinels in the compact
        // runtime. `Symbol.for(key)` must be stable because libraries such as
        // React use the global registry for element type identity.
        "Symbol" => "w3cos_core::Value::object(::std::collections::HashMap::from([(\"iterator\".to_string(), w3cos_core::Value::from(\"__w3cos_symbol_iterator\")), (\"for\".to_string(), w3cos_core::Value::function(|_this, __args| w3cos_core::Value::from(format!(\"__w3cos_symbol_for:{}\", __args.first().cloned().unwrap_or(w3cos_core::Value::Undefined).to_js_string()))))]))".to_string(),
        // Unimplemented builtin globals: harmless empty-object stubs keep
        // references total (`new X()` yields Undefined via construct on a
        // non-callable; `X.y` yields Undefined).
        "BigInt" | "ArrayBuffer" | "SharedArrayBuffer" | "DataView"
        | "WeakRef" | "FinalizationRegistry" | "Atomics" | "eval"
        | "encodeURI" | "encodeURIComponent" | "decodeURI" | "decodeURIComponent" | "escape"
        | "unescape" | "TextEncoder" | "fetch" | "Request" | "Response"
        | "Headers" | "FormData" | "AbortController" | "AbortSignal" | "Event"
        | "EventTarget" | "CustomEvent" | "MessagePort" | "Worker"
        | "ImageData" | "OffscreenCanvas" | "Path2D" | "DOMRect" | "DOMPoint" | "DOMMatrix"
        | "MutationObserver" | "IntersectionObserver" | "PerformanceObserver" | "Report"
        // DOM constructors / event types (instanceof degrades to false).
        | "Function" | "Node" | "Element" | "HTMLElement" | "HTMLAnchorElement"
        | "HTMLDivElement" | "HTMLSpanElement" | "HTMLButtonElement" | "HTMLInputElement"
        | "HTMLTextAreaElement" | "HTMLSelectElement" | "HTMLFormElement" | "HTMLImageElement"
        | "HTMLVideoElement" | "HTMLCanvasElement" | "SVGElement" | "DocumentFragment"
        | "ShadowRoot" | "NodeList" | "CSSStyleDeclaration" | "MouseEvent" | "KeyboardEvent"
        | "PointerEvent" | "WheelEvent" | "FocusEvent" | "InputEvent" | "ClipboardEvent"
        | "DragEvent" | "TouchEvent" | "AnimationEvent" | "TransitionEvent" | "ErrorEvent"
        | "EventSource" | "WebSocket" | "XMLHttpRequest" | "Blob" | "File" | "FileReader"
        | "ClipboardItem" | "DataTransfer" | "DOMException" | "Range" | "Selection"
        | "DOMParser" | "XMLSerializer" | "CSS" | "CSSStyleSheet"
        // URL constructors are handled at `new` sites; bare values are stubs.
        | "URL" | "URLSearchParams"
        // CommonJS/Node artifacts that appear in UMD-wrapped sources.
        | "require" | "module" | "exports" | "process" | "global" | "Buffer" | "__dirname"
        | "__filename"
        // AMD (`define`/`define.amd`) and Worker (`importScripts`) globals:
        // object stubs make `typeof define === "function"` guards take the
        // false branch (the correct non-AMD/non-Worker path).
        | "define" | "importScripts" => {
            "w3cos_core::Value::object(::std::collections::HashMap::new()) /* builtin stub */"
                .to_string()
        }
        // Error family as bare values (`instanceof` etc. evaluates to false).
        "TypeError" | "SyntaxError" | "ReferenceError" | "EvalError" | "URIError"
        | "AggregateError" => {
            "w3cos_core::Value::object(::std::collections::HashMap::new()) /* error stub */"
                .to_string()
        }
        _ => return None,
    };
    Some(expr)
}

/// Does any statement reference the given identifier (free or bound)? Used to
/// detect self-recursion in nested fn declarations.
fn stmts_reference_ident(stmts: &[Stmt], name: &str) -> bool {
    stmts_reference_matching(stmts, &|n| n == name)
}

/// Does any statement reference any identifier in the set?
fn stmts_reference_any_ident(stmts: &[Stmt], names: &HashSet<String>) -> bool {
    !names.is_empty() && stmts_reference_matching(stmts, &|n| names.contains(n))
}

fn stmts_reference_matching(stmts: &[Stmt], matches: &dyn Fn(&str) -> bool) -> bool {
    stmts.iter().any(|s| stmt_references_ident(s, matches))
}

/// Collect every identifier name referenced in the statements (sanitized).
/// Used to restrict closure capture lists to names the body actually uses:
/// the coarse `known_values − params` set breaks inside nested Rust fn items
/// (which cannot capture) and wastes clones elsewhere.
fn stmts_referenced_names(stmts: &[Stmt]) -> HashSet<String> {
    let names = std::cell::RefCell::new(HashSet::new());
    stmts_reference_matching(stmts, &|name| {
        names.borrow_mut().insert(sanitize_ident(name));
        false
    });
    names.into_inner()
}

/// The [`stmts_referenced_names`] equivalent for a single expression.
fn expr_referenced_names(expr: &Expr) -> HashSet<String> {
    let names = std::cell::RefCell::new(HashSet::new());
    let _ = expr_references_ident(expr, &|name| {
        names.borrow_mut().insert(sanitize_ident(name));
        false
    });
    names.into_inner()
}

/// Capture list for a closure body: in-scope names the body references.
fn capture_names(
    known_values: &HashSet<String>,
    parameter_names: &HashSet<String>,
    referenced: &HashSet<String>,
) -> Vec<String> {
    let mut captures = known_values
        .difference(parameter_names)
        .filter(|name| referenced.contains(*name))
        .cloned()
        .collect::<Vec<_>>();
    captures.sort();
    captures
}

fn stmt_references_ident(stmt: &Stmt, matches: &dyn Fn(&str) -> bool) -> bool {
    match stmt {
        Stmt::Expr(s) => expr_references_ident(&s.expr, matches),
        Stmt::Return(s) => s
            .arg
            .as_deref()
            .is_some_and(|e| expr_references_ident(e, matches)),
        Stmt::Throw(s) => expr_references_ident(&s.arg, matches),
        Stmt::Decl(decl) => match decl {
            Decl::Var(var) => var.decls.iter().any(|d| {
                d.init
                    .as_deref()
                    .is_some_and(|e| expr_references_ident(e, matches))
            }),
            Decl::Fn(f) => f
                .function
                .body
                .as_ref()
                .is_some_and(|b| stmts_reference_matching(&b.stmts, matches)),
            Decl::Class(c) => class_references_ident(&c.class, matches),
            _ => false,
        },
        Stmt::Block(b) => stmts_reference_matching(&b.stmts, matches),
        Stmt::If(s) => {
            expr_references_ident(&s.test, matches)
                || stmt_references_ident(&s.cons, matches)
                || s.alt
                    .as_deref()
                    .is_some_and(|a| stmt_references_ident(a, matches))
        }
        Stmt::For(s) => {
            s.init.as_ref().is_some_and(|init| match init {
                VarDeclOrExpr::VarDecl(decl) => decl.decls.iter().any(|d| {
                    d.init
                        .as_deref()
                        .is_some_and(|e| expr_references_ident(e, matches))
                }),
                VarDeclOrExpr::Expr(expr) => expr_references_ident(expr, matches),
            }) || s
                .test
                .as_deref()
                .is_some_and(|e| expr_references_ident(e, matches))
                || s.update
                    .as_deref()
                    .is_some_and(|e| expr_references_ident(e, matches))
                || stmt_references_ident(&s.body, matches)
        }
        Stmt::While(s) => {
            expr_references_ident(&s.test, matches) || stmt_references_ident(&s.body, matches)
        }
        Stmt::DoWhile(s) => {
            stmt_references_ident(&s.body, matches) || expr_references_ident(&s.test, matches)
        }
        Stmt::ForIn(s) => {
            expr_references_ident(&s.right, matches) || stmt_references_ident(&s.body, matches)
        }
        Stmt::ForOf(s) => {
            expr_references_ident(&s.right, matches) || stmt_references_ident(&s.body, matches)
        }
        Stmt::Switch(s) => {
            expr_references_ident(&s.discriminant, matches)
                || s.cases.iter().any(|case| {
                    case.test
                        .as_deref()
                        .is_some_and(|e| expr_references_ident(e, matches))
                        || stmts_reference_matching(&case.cons, matches)
                })
        }
        Stmt::Try(s) => {
            stmts_reference_matching(&s.block.stmts, matches)
                || s.handler
                    .as_ref()
                    .is_some_and(|h| stmts_reference_matching(&h.body.stmts, matches))
                || s.finalizer
                    .as_ref()
                    .is_some_and(|f| stmts_reference_matching(&f.stmts, matches))
        }
        Stmt::Labeled(s) => stmt_references_ident(&s.body, matches),
        _ => false,
    }
}

fn class_references_ident(class: &Class, matches: &dyn Fn(&str) -> bool) -> bool {
    class.body.iter().any(|member| match member {
        ClassMember::Constructor(c) => c
            .body
            .as_ref()
            .is_some_and(|b| stmts_reference_matching(&b.stmts, matches)),
        ClassMember::Method(m) => m
            .function
            .body
            .as_ref()
            .is_some_and(|b| stmts_reference_matching(&b.stmts, matches)),
        ClassMember::PrivateMethod(m) => m
            .function
            .body
            .as_ref()
            .is_some_and(|b| stmts_reference_matching(&b.stmts, matches)),
        ClassMember::ClassProp(p) => p
            .value
            .as_deref()
            .is_some_and(|e| expr_references_ident(e, matches)),
        ClassMember::PrivateProp(p) => p
            .value
            .as_deref()
            .is_some_and(|e| expr_references_ident(e, matches)),
        ClassMember::StaticBlock(b) => stmts_reference_matching(&b.body.stmts, matches),
        _ => false,
    })
}

fn expr_references_ident(expr: &Expr, matches: &dyn Fn(&str) -> bool) -> bool {
    match expr {
        Expr::Ident(ident) => matches(&ident.sym),
        Expr::Call(call) => {
            call.args
                .iter()
                .any(|arg| expr_references_ident(&arg.expr, matches))
                || matches!(&call.callee, Callee::Expr(callee) if expr_references_ident(callee, matches))
        }
        Expr::New(new_expr) => {
            expr_references_ident(&new_expr.callee, matches)
                || new_expr.args.as_ref().is_some_and(|args| {
                    args.iter()
                        .any(|arg| expr_references_ident(&arg.expr, matches))
                })
        }
        Expr::Member(member) => {
            expr_references_ident(&member.obj, matches)
                || match &member.prop {
                    MemberProp::Computed(computed) => {
                        expr_references_ident(&computed.expr, matches)
                    }
                    _ => false,
                }
        }
        Expr::Bin(bin) => {
            expr_references_ident(&bin.left, matches) || expr_references_ident(&bin.right, matches)
        }
        Expr::Unary(unary) => expr_references_ident(&unary.arg, matches),
        Expr::Update(update) => expr_references_ident(&update.arg, matches),
        Expr::Assign(assign) => {
            expr_references_ident(&assign.right, matches)
                || match &assign.left {
                    AssignTarget::Simple(SimpleAssignTarget::Ident(ident)) => {
                        matches(&ident.id.sym)
                    }
                    AssignTarget::Simple(SimpleAssignTarget::Member(member)) => {
                        expr_references_ident(&member.obj, matches)
                            || match &member.prop {
                                MemberProp::Computed(computed) => {
                                    expr_references_ident(&computed.expr, matches)
                                }
                                _ => false,
                            }
                    }
                    _ => false,
                }
        }
        Expr::Cond(cond) => {
            expr_references_ident(&cond.test, matches)
                || expr_references_ident(&cond.cons, matches)
                || expr_references_ident(&cond.alt, matches)
        }
        Expr::Paren(paren) => expr_references_ident(&paren.expr, matches),
        Expr::Seq(seq) => seq.exprs.iter().any(|e| expr_references_ident(e, matches)),
        Expr::Tpl(tpl) => tpl.exprs.iter().any(|e| expr_references_ident(e, matches)),
        Expr::Arrow(arrow) => match arrow.body.as_ref() {
            BlockStmtOrExpr::BlockStmt(block) => stmts_reference_matching(&block.stmts, matches),
            BlockStmtOrExpr::Expr(expr) => expr_references_ident(expr, matches),
        },
        Expr::Fn(f) => f
            .function
            .body
            .as_ref()
            .is_some_and(|b| stmts_reference_matching(&b.stmts, matches)),
        Expr::Object(obj) => obj.props.iter().any(|prop| match prop {
            PropOrSpread::Spread(spread) => expr_references_ident(&spread.expr, matches),
            PropOrSpread::Prop(prop) => match prop.as_ref() {
                Prop::KeyValue(kv) => expr_references_ident(&kv.value, matches),
                Prop::Shorthand(ident) => matches(&ident.sym),
                Prop::Method(method) => method
                    .function
                    .body
                    .as_ref()
                    .is_some_and(|b| stmts_reference_matching(&b.stmts, matches)),
                Prop::Getter(getter) => getter
                    .body
                    .as_ref()
                    .is_some_and(|b| stmts_reference_matching(&b.stmts, matches)),
                Prop::Setter(setter) => setter
                    .body
                    .as_ref()
                    .is_some_and(|b| stmts_reference_matching(&b.stmts, matches)),
                _ => false,
            },
        }),
        Expr::Array(arr) => arr
            .elems
            .iter()
            .flatten()
            .any(|elem| expr_references_ident(&elem.expr, matches)),
        Expr::OptChain(opt) => match opt.base.as_ref() {
            OptChainBase::Member(member) => {
                expr_references_ident(&member.obj, matches)
                    || match &member.prop {
                        MemberProp::Computed(computed) => {
                            expr_references_ident(&computed.expr, matches)
                        }
                        _ => false,
                    }
            }
            OptChainBase::Call(call) => {
                expr_references_ident(&call.callee, matches)
                    || call
                        .args
                        .iter()
                        .any(|arg| expr_references_ident(&arg.expr, matches))
            }
        },
        Expr::TaggedTpl(tagged) => {
            expr_references_ident(&tagged.tag, matches)
                || tagged
                    .tpl
                    .exprs
                    .iter()
                    .any(|e| expr_references_ident(e, matches))
        }
        Expr::Class(class_expr) => class_references_ident(&class_expr.class, matches),
        Expr::Await(await_expr) => expr_references_ident(&await_expr.arg, matches),
        Expr::Yield(yield_expr) => yield_expr
            .arg
            .as_deref()
            .is_some_and(|e| expr_references_ident(e, matches)),
        _ => false,
    }
}

fn lower_bin_op(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Mul => "*",
        BinaryOp::Div => "/",
        BinaryOp::Mod => "%",
        BinaryOp::EqEq | BinaryOp::EqEqEq => "==",
        BinaryOp::NotEq | BinaryOp::NotEqEq => "!=",
        BinaryOp::Lt => "<",
        BinaryOp::LtEq => "<=",
        BinaryOp::Gt => ">",
        BinaryOp::GtEq => ">=",
        BinaryOp::LogicalAnd => "&&",
        BinaryOp::LogicalOr => "||",
        BinaryOp::BitAnd => "&",
        BinaryOp::BitOr => "|",
        BinaryOp::BitXor => "^",
        BinaryOp::LShift => "<<",
        BinaryOp::RShift => ">>",
        BinaryOp::ZeroFillRShift => ">>",
        BinaryOp::NullishCoalescing => "/* ?? */||",
        BinaryOp::Exp => "/* ** */",
        BinaryOp::In => "/* in */==",
        BinaryOp::InstanceOf => "/* instanceof */==",
    }
}

fn lower_dynamic_bin_op(op: BinaryOp, left: &str, right: &str) -> String {
    match op {
        BinaryOp::Add => format!("{left}.js_add(&{right})"),
        BinaryOp::Sub => format!("{left}.js_sub(&{right})"),
        BinaryOp::Mul => format!("{left}.js_mul(&{right})"),
        BinaryOp::Div => format!("{left}.js_div(&{right})"),
        BinaryOp::Mod => format!("{left}.js_rem(&{right})"),
        BinaryOp::Exp => format!("{left}.js_pow(&{right})"),
        BinaryOp::EqEqEq => format!("w3cos_core::Value::Bool({left}.strict_eq(&{right}))"),
        BinaryOp::NotEqEq => format!("w3cos_core::Value::Bool(!{left}.strict_eq(&{right}))"),
        BinaryOp::EqEq => format!("w3cos_core::Value::Bool({left}.abstract_eq(&{right}))"),
        BinaryOp::NotEq => format!("w3cos_core::Value::Bool(!{left}.abstract_eq(&{right}))"),
        BinaryOp::Lt => format!("w3cos_core::Value::Bool({left}.js_lt(&{right}))"),
        BinaryOp::LtEq => format!("w3cos_core::Value::Bool({left}.js_le(&{right}))"),
        BinaryOp::Gt => format!("w3cos_core::Value::Bool({left}.js_gt(&{right}))"),
        BinaryOp::GtEq => format!("w3cos_core::Value::Bool({left}.js_ge(&{right}))"),
        BinaryOp::LogicalAnd => {
            // Bind the left operand once: emitting its text twice (as a naive
            // `if l { r } else { l }` does) doubles per `&&` in a chain and
            // blows up exponentially on real-world condition chains.
            format!("{{ let __l = {left}; if __l.to_bool() {{ {right} }} else {{ __l }} }}")
        }
        BinaryOp::LogicalOr => {
            format!("{{ let __l = {left}; if __l.to_bool() {{ __l }} else {{ {right} }} }}")
        }
        BinaryOp::NullishCoalescing => {
            format!("{{ let __l = {left}; if __l.is_nullish() {{ {right} }} else {{ __l }} }}")
        }
        BinaryOp::BitAnd => format!("{left}.js_bitand(&{right})"),
        BinaryOp::BitOr => format!("{left}.js_bitor(&{right})"),
        BinaryOp::BitXor => format!("{left}.js_bitxor(&{right})"),
        BinaryOp::LShift => format!("{left}.js_shl(&{right})"),
        BinaryOp::RShift => format!("{left}.js_shr(&{right})"),
        BinaryOp::ZeroFillRShift => format!("{left}.js_ushr(&{right})"),
        BinaryOp::In => format!("{left}.js_in(&{right})"),
        BinaryOp::InstanceOf => {
            format!("w3cos_core::Value::Bool(w3cos_core::class::instance_of(&{left}, &{right}))")
        }
    }
}

fn lower_unary_op(op: UnaryOp) -> &'static str {
    match op {
        UnaryOp::Minus => "-",
        UnaryOp::Plus => "",
        UnaryOp::Bang => "!",
        UnaryOp::Tilde => "!",
        UnaryOp::TypeOf => "/* typeof */",
        UnaryOp::Void => "/* void */",
        UnaryOp::Delete => "/* delete */",
    }
}

fn expr_kind_name(expr: &Expr) -> &'static str {
    match expr {
        Expr::Fn(_) => "fn_expr",
        Expr::Class(_) => "class_expr",
        Expr::Yield(_) => "yield",
        Expr::Await(_) => "await",
        Expr::Seq(_) => "seq",
        Expr::TaggedTpl(_) => "tagged_tpl",
        Expr::OptChain(_) => "opt_chain",
        _ => "unknown_expr",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use swc_common::{FileName, SourceMap, sync::Lrc};
    use swc_ecma_parser::{Parser, StringInput, Syntax, TsSyntax, lexer::Lexer};

    fn parse_stmts(code: &str) -> Vec<Stmt> {
        let cm: Lrc<SourceMap> = Default::default();
        let fm = cm.new_source_file(Lrc::new(FileName::Anon), code.to_string());
        let lexer = Lexer::new(
            Syntax::Typescript(TsSyntax::default()),
            Default::default(),
            StringInput::from(&*fm),
            None,
        );
        let mut parser = Parser::new_from(lexer);
        let module = parser.parse_module().expect("parse failed");
        module
            .body
            .into_iter()
            .filter_map(|item| match item {
                ModuleItem::Stmt(s) => Some(s),
                _ => None,
            })
            .collect()
    }

    fn parse_tsx_stmts(code: &str) -> Vec<Stmt> {
        let cm: Lrc<SourceMap> = Default::default();
        let fm = cm.new_source_file(Lrc::new(FileName::Anon), code.to_string());
        let lexer = Lexer::new(
            Syntax::Typescript(TsSyntax {
                tsx: true,
                ..Default::default()
            }),
            Default::default(),
            StringInput::from(&*fm),
            None,
        );
        let mut parser = Parser::new_from(lexer);
        let module = parser.parse_module().expect("parse failed");
        module
            .body
            .into_iter()
            .filter_map(|item| match item {
                ModuleItem::Stmt(statement) => Some(statement),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn lowers_standard_tsx_elements_to_generic_values() {
        let statements = parse_tsx_stmts(
            r#"const view = <List rowCount={1000} {...props}><span onClick={open}>你好</span></List>;"#,
        );
        let mut context = LowerCtx::new_dynamic(vec![]);
        let code = context.lower_stmts(&statements);

        assert!(
            code.contains("js_object! { \"type\"") && code.contains("\"props\""),
            "tsx value: {code}"
        );
        assert!(
            code.contains("Value::from(\"span\")"),
            "intrinsic tag: {code}"
        );
        assert!(code.contains("rowCount"), "component prop: {code}");
        assert!(code.contains("object_from_parts"), "spread props: {code}");
        assert!(code.contains("你好"), "text child: {code}");
    }

    #[test]
    fn erases_typescript_expression_wrappers() {
        let statements = parse_stmts(
            "const a = value as string; const b = value!; const c = value satisfies string;",
        );
        let mut context = LowerCtx::new_dynamic(vec![]);
        let code = context.lower_stmts(&statements);

        assert!(
            !code.contains("unknown_expr"),
            "typescript wrappers: {code}"
        );
        assert!(code.matches("value").count() >= 3, "wrapped values: {code}");
    }

    #[test]
    fn lowers_import_meta_env_to_native_build_metadata() {
        let statements = parse_stmts("const dev = import.meta.env.DEV;");
        let mut context = LowerCtx::new_dynamic(vec![]);
        let code = context.lower_stmts(&statements);

        assert!(code.contains("\"env\""), "import.meta env object: {code}");
        assert!(code.contains("\"DEV\" => false"), "native DEV flag: {code}");
        assert!(
            !code.contains("unknown_expr"),
            "import.meta lowering: {code}"
        );
    }

    #[test]
    fn lowers_dynamic_array_spread_by_flattening_iterables() {
        let statements = parse_stmts("const result = [first, ...rest, last];");
        let mut context = LowerCtx::new_dynamic(vec![]);
        let code = context.lower_stmts(&statements);

        assert!(
            code.contains("__w3cos_array_items.push(first)"),
            "leading item: {code}"
        );
        assert!(
            code.contains("__w3cos_array_items.extend((rest).iter())"),
            "spread item: {code}"
        );
        assert!(
            code.contains("__w3cos_array_items.push(last)"),
            "trailing item: {code}"
        );
        assert!(
            code.contains("Value::array(__w3cos_array_items)"),
            "dynamic array result: {code}"
        );
    }

    #[test]
    fn lowers_dynamic_call_spread_by_flattening_arguments() {
        let statements =
            parse_stmts("const invoke = (fn, accessor, args) => fn(accessor, ...args);");
        let mut context = LowerCtx::new_dynamic(vec![]);
        let code = context.lower_stmts(&statements);

        assert!(
            code.contains("__w3cos_call_args.push(accessor.clone().clone())"),
            "leading argument: {code}"
        );
        assert!(
            code.contains("__w3cos_call_args.extend((args.clone()).iter())"),
            "spread arguments: {code}"
        );
        assert!(
            code.contains("fn_.clone().call(w3cos_core::Value::Undefined"),
            "dynamic function call: {code}"
        );
    }

    #[test]
    fn lowers_variable_declarations() {
        let stmts = parse_stmts("const x = 42; let y = \"hello\";");
        let mut ctx = LowerCtx::new(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(code.contains("let x = 42"), "const → let: {code}");
        assert!(
            code.contains("let mut y = \"hello\""),
            "let → let mut: {code}"
        );
    }

    #[test]
    fn lowers_new_expr_to_struct_new() {
        let stmts = parse_stmts("const v = new EditorView({});");
        let renames = vec![("EditorView".to_string(), "m1_EditorView".to_string())];
        let mut ctx = LowerCtx::new(renames);
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains("m1_EditorView::new("),
            "new X() → X::new(): {code}"
        );
    }

    #[test]
    fn lowers_method_calls() {
        let stmts = parse_stmts("state.create({doc: \"hi\"});");
        let mut ctx = LowerCtx::new(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(code.contains("state.create("), "method call: {code}");
        assert!(
            code.contains("w3cos_core::js_object! { \"doc\" => \"hi\" }"),
            "object arg: {code}"
        );
    }

    #[test]
    fn lowers_object_literals_to_dynamic_js_values() {
        let stmts = parse_stmts("const props = { rowCount: 1000, 'aria-label': \"rows\" };");
        let mut ctx = LowerCtx::new(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains("\"rowCount\" => 1000"),
            "identifier key: {code}"
        );
        assert!(
            code.contains("\"aria-label\" => \"rows\""),
            "string key: {code}"
        );
    }

    #[test]
    fn dynamic_lowering_binds_local_destructuring_patterns() {
        let stmts = parse_stmts(
            "const [value, setValue] = state; const {height: h, width = 10, ...rest} = size;",
        );
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains("let mut value = __binding0.get_property(\"0\")")
                && code.contains("let mut setValue = __binding0.get_property(\"1\")"),
            "array destructuring: {code}"
        );
        assert!(
            code.contains("let mut h = __binding1.get_property(\"height\")")
                && code
                    .contains("let mut width = { let value = __binding1.get_property(\"width\")"),
            "object destructuring: {code}"
        );
        assert!(
            code.contains("let mut rest = __binding1.object_rest(&[\"height\", \"width\"]);"),
            "object rest must exclude prior bindings: {code}"
        );
    }

    #[test]
    fn dynamic_lowering_wraps_arrow_functions_and_member_assignment() {
        let stmts = parse_stmts(
            "const update = ({current}, value) => { current.value = value; return current?.value; };",
        );
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains("Value::function(move |_this, __args|")
                && code.contains("let mut current = __args.get(0)")
                && code.contains("set_property(&\"value\"")
                && code.contains("is_nullish()"),
            "dynamic closure lowering: {code}"
        );
    }

    #[test]
    fn dynamic_closures_capture_only_referenced_values() {
        let stmts =
            parse_stmts("const used = 1; const unused = 2; const callback = () => used + 1;");
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);

        assert!(
            code.contains("let mut used = used.clone();"),
            "referenced capture missing: {code}"
        );
        assert!(
            !code.contains("let mut unused = unused.clone();"),
            "unreferenced capture retained: {code}"
        );
    }

    #[test]
    fn dynamic_jsx_callbacks_clone_nested_captures() {
        let stmts = parse_tsx_stmts(
            "const columns = []; const ctx = {}; const view = () => <div>{columns.map((column) => <span>{ctx[column]}</span>)}</div>;",
        );
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);

        assert!(
            code.matches("let mut columns = columns.clone();").count() >= 1,
            "outer JSX callback must retain columns: {code}"
        );
        assert!(
            code.contains("let mut ctx = ctx.clone();")
                && code.contains("ctx.clone().get_property"),
            "nested JSX callback must clone ctx before use: {code}"
        );
    }

    #[test]
    fn dynamic_lowering_exposes_resize_observer_as_a_constructor() {
        let stmts = parse_stmts("const supported = typeof ResizeObserver < \"u\";");
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains("Value::from(\"function\")"),
            "ResizeObserver typeof: {code}"
        );
        assert!(
            !code.contains("type_of(&ResizeObserver)"),
            "must not expose the placeholder value: {code}"
        );
    }

    #[test]
    fn dynamic_lowering_identifies_native_runtime_as_a_window_host() {
        let stmts =
            parse_stmts("const effect = typeof window < \"u\" ? useLayoutEffect : useEffect;");
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains("Value::from(\"object\")"),
            "window typeof: {code}"
        );
        assert!(
            !code.contains("type_of(&window)"),
            "must not lower the native window host as an unresolved value: {code}"
        );
    }

    #[test]
    fn dynamic_lowering_reads_optional_browser_globals_from_window() {
        let stmts = parse_stmts(
            "typeof __REACT_DEVTOOLS_GLOBAL_HOOK__; typeof navigation; typeof reportError; typeof setImmediate; typeof MessageChannel;",
        );
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);

        for name in [
            "__REACT_DEVTOOLS_GLOBAL_HOOK__",
            "navigation",
            "reportError",
            "setImmediate",
            "MessageChannel",
        ] {
            assert!(
                code.contains(&format!("window_value().get_property({name:?})")),
                "optional global {name}: {code}"
            );
        }
    }

    #[test]
    fn lowers_if_else() {
        let stmts = parse_stmts("if (x > 0) { return x; } else { return 0; }");
        let mut ctx = LowerCtx::new(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(code.contains("if x > 0"), "if condition: {code}");
        assert!(code.contains("return x"), "then branch: {code}");
        assert!(code.contains("else"), "else branch: {code}");
        assert!(code.contains("return 0"), "else body: {code}");
    }

    #[test]
    fn lowers_for_loop() {
        let stmts = parse_stmts("for (let i = 0; i < 10; i++) { console.log(i); }");
        let mut ctx = LowerCtx::new(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(code.contains("loop"), "for → loop: {code}");
        assert!(code.contains("break"), "break condition: {code}");
        // init should appear before loop, not inside a comment
        assert!(
            code.contains("let mut i = 0;"),
            "init declared before loop: {code}"
        );
        // no double semicolons
        assert!(!code.contains(";;"), "no double semicolons: {code}");
        // update should appear
        assert!(code.contains("i += 1"), "update increment: {code}");
    }

    #[test]
    fn dynamic_lowering_preserves_bitwise_not() {
        let stmts = parse_stmts("const available = pending & ~suspended;");
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains("suspended.js_bitnot()"),
            "bitwise not must not degrade to the operand: {code}"
        );
    }

    #[test]
    fn dynamic_lowering_preserves_switch_fallthrough() {
        let stmts = parse_stmts(
            "switch (tag) { case 7: case 8: bubble(); return null; case 9: stop(); break; default: fail(); }",
        );
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);

        assert!(
            code.contains("if __case0 >= 0 && __case0 <= 0")
                && code.contains("if __case0 >= 0 && __case0 <= 1"),
            "case bodies execute from the selected case onward: {code}"
        );
        assert!(
            !code.contains("strict_eq(&w3cos_core::Value::Number(7.0)) {\n\n")
                || !code.contains("break '__sw0;"),
            "an empty case must not implicitly break the switch: {code}"
        );
    }

    #[test]
    fn dynamic_lowering_preserves_shorthand_property_key() {
        let stmts = parse_stmts("const type = 'main'; const element = { type };");
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);

        assert!(
            code.contains("\"type\" => type_.clone()"),
            "the JavaScript key stays `type` while only the Rust binding is escaped: {code}"
        );
        assert!(!code.contains("\"type_\" =>"), "escaped key leaked: {code}");
    }

    #[test]
    fn dynamic_lowering_selects_middle_default_only_when_no_case_matches() {
        let stmts = parse_stmts(
            "switch (value) { case 1: one(); break; default: fallback(); case 2: two(); }",
        );
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);

        assert!(
            code.contains("if __case0 < 0 { __case0 = 1; }") && code.contains("__case0 <= 2"),
            "default selection still falls through to following cases: {code}"
        );
    }

    #[test]
    fn lowers_arrow_function() {
        let stmts = parse_stmts("const fn1 = (a, b) => a + b;");
        let mut ctx = LowerCtx::new(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(code.contains("|a, b|"), "arrow params: {code}");
        assert!(code.contains("a + b"), "arrow body: {code}");
    }

    #[test]
    fn lowers_try_catch() {
        let stmts = parse_stmts("try { doThing(); } catch (e) { handle(e); }");
        let mut ctx = LowerCtx::new(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(code.contains("// try"), "try block marker: {code}");
        assert!(code.contains("doThing("), "try body: {code}");
        assert!(code.contains("// catch (e)"), "catch marker: {code}");
        assert!(code.contains("handle(e)"), "catch body: {code}");
    }

    #[test]
    fn lowers_throw() {
        let stmts = parse_stmts("throw new Error(\"boom\");");
        let mut ctx = LowerCtx::new(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(code.contains("panic!("), "throw → panic!: {code}");
    }

    #[test]
    fn dynamic_lowering_throw_panics_with_value_payload() {
        let stmts = parse_stmts(r#"throw { code: 42 };"#);
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains("w3cos_core::throw_value("),
            "dynamic throw → throw_value: {code}"
        );
    }

    #[test]
    fn dynamic_lowering_try_catch_finally_shape() {
        let stmts = parse_stmts("try { risky(); } catch (e) { report(e); } finally { cleanup(); }");
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);

        assert!(
            code.contains("enum __Flow0 { Done, Return(w3cos_core::Value), Break(&'static str), Continue(&'static str), Throw(std::boxed::Box<dyn std::any::Any + Send>) }"),
            "flow enum: {code}"
        );
        assert!(
            code.contains("let __caught0 = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> __Flow0 {"),
            "try body wrapped in catch_unwind: {code}"
        );
        assert!(
            code.contains("__payload0.downcast_ref::<w3cos_core::PanicValue>()"),
            "payload downcast to thrown value: {code}"
        );
        assert!(
            code.contains("__payload0.downcast_ref::<&'static str>()")
                && code.contains("__payload0.downcast_ref::<String>()"),
            "string payload fallbacks: {code}"
        );
        assert!(code.contains("let mut e = {"), "catch param bound: {code}");
        assert!(
            code.contains("let __caught2_0") || code.contains("__caught2"),
            "catch body itself guarded (catch throwing still runs finally): {code}"
        );
        // finally body appears after the flow closure, before the epilogue.
        let finally_pos = code.find("cleanup").expect("finally body present");
        let closure_pos = code.find("})();").expect("flow closure ends");
        let epilogue_pos = code.find("std::panic::resume_unwind(p)").expect("rethrow");
        assert!(
            closure_pos < finally_pos && finally_pos < epilogue_pos,
            "finally runs after try/catch, before propagation: {code}"
        );
    }

    #[test]
    fn dynamic_lowering_try_finally_without_catch_rethrows() {
        let stmts = parse_stmts("try { risky(); } finally { cleanup(); }");
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains("__Flow0::Throw(__payload0)"),
            "no catch → payload deferred until after finally: {code}"
        );
        assert!(
            code.contains("std::panic::resume_unwind(p)"),
            "finally without catch resumes the panic: {code}"
        );
    }

    #[test]
    fn dynamic_lowering_propagates_loop_control_through_try_finally() {
        let stmts = parse_stmts(
            "outer: while (ready) { try { if (done) break outer; continue; } finally { cleanup(); } }",
        );
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);

        assert!(
            code.contains("return __Flow") && code.contains("::Break(\"outer\")"),
            "labeled break becomes a flow value: {code}"
        );
        assert!(
            code.contains("::Continue(\"\")"),
            "continue becomes a flow value: {code}"
        );
        assert!(
            code.contains("break '__js_outer") && code.contains("continue '__js_outer"),
            "flow epilogue targets the enclosing loop: {code}"
        );
    }

    #[test]
    fn dynamic_lowering_consumes_nested_try_break_at_inner_label() {
        let stmts = parse_stmts(
            "try { outer: { try { inner: { break inner; } break outer; } finally { cleanup(); } unreachable(); } } finally { finish(); }",
        );
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);

        assert!(
            code.contains("\"outer\" => break '__js_outer"),
            "a label declared inside an outer try must consume a break propagated by an inner try: {code}"
        );
        assert!(
            !code.contains("__Flow0::Break(label) => { return"),
            "the inner label must be resolved before flow escapes the labeled block: {code}"
        );
    }

    #[test]
    fn dynamic_lowering_return_inside_try_wraps_flow_and_finally_runs() {
        let stmts = parse_stmts("function f() { try { return 1; } finally { mark(); } }");
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains("return __Flow0::Return("),
            "return inside try wraps into the flow enum: {code}"
        );
        assert!(
            code.contains("__Flow0::Return(v) => { return v; }"),
            "epilogue propagates the early return after finally: {code}"
        );
    }

    #[test]
    fn dynamic_lowering_nested_try_propagates_to_outer_flow() {
        let stmts = parse_stmts(
            "function f() { try { try { return 1; } catch (e) { return 2; } } finally { mark(); } }",
        );
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        // Inner try's returns target its own enum; its epilogue re-wraps into
        // the outer try's enum so the outer finally still runs.
        assert!(
            code.contains("return __Flow1::Return("),
            "inner try return wraps inner flow: {code}"
        );
        assert!(
            code.contains("__Flow1::Return(v) => { return __Flow0::Return(v); }"),
            "inner epilogue re-wraps into outer flow: {code}"
        );
        assert!(
            code.contains("enum __Flow0") && code.contains("enum __Flow1"),
            "distinct flow enums per try: {code}"
        );
    }

    #[test]
    fn dynamic_lowering_try_around_await_emits_warning() {
        let stmts = parse_stmts("async function f() { try { await g(); } catch (e) { h(e); } }");
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains("// compile_warning: try/catch around `await` is best-effort only"),
            "await inside try flagged: {code}"
        );
    }

    #[test]
    fn dynamic_lowering_maps_dom_globals_to_jsdom_bridge() {
        let stmts = parse_stmts(
            r#"const el = document.getElementById("app");
const w = window.innerWidth;
const g = globalThis.location;
const s = self.closed;
const ua = navigator.userAgent;
const saved = localStorage.getItem("k");
const openRequest = indexedDB.open("app", 1);
const now = performance.now();
const scr = screen.width;"#,
        );
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains("w3cos_runtime::jsdom::document_value().call_method(\"getElementById\""),
            "document.* → jsdom document: {code}"
        );
        assert!(
            code.contains("w3cos_runtime::jsdom::window_value().get_property(\"innerWidth\")"),
            "window.innerWidth: {code}"
        );
        assert!(
            code.contains("w3cos_runtime::jsdom::window_value().get_property(\"location\")"),
            "globalThis.location: {code}"
        );
        assert!(
            code.contains("w3cos_runtime::jsdom::window_value().get_property(\"closed\")"),
            "self.closed: {code}"
        );
        assert!(
            code.contains(
                "w3cos_runtime::jsdom::window_value().get_property(\"navigator\").get_property(\"userAgent\")"
            ),
            "navigator.userAgent: {code}"
        );
        assert!(
            code.contains(
                "w3cos_runtime::jsdom::window_value().get_property(\"localStorage\").call_method(\"getItem\""
            ),
            "localStorage.getItem: {code}"
        );
        assert!(
            code.contains(
                "w3cos_runtime::jsdom::window_value().get_property(\"indexedDB\").call_method(\"open\""
            ),
            "indexedDB.open: {code}"
        );
        assert!(
            code.contains(
                "w3cos_runtime::jsdom::window_value().get_property(\"performance\").call_method(\"now\""
            ),
            "performance.now: {code}"
        );
        assert!(
            code.contains(
                "w3cos_runtime::jsdom::window_value().get_property(\"screen\").get_property(\"width\")"
            ),
            "screen.width: {code}"
        );
    }

    #[test]
    fn dynamic_lowering_maps_timer_globals_to_window_methods() {
        let stmts = parse_stmts(
            r#"setTimeout(cb, 10);
const id = setInterval(cb, 5);
clearTimeout(id);
requestAnimationFrame(frame);
queueMicrotask(job);
const mq = matchMedia("(min-width: 100px)");
const cs = getComputedStyle(el);
const sel = getSelection();"#,
        );
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        for name in [
            "setTimeout",
            "setInterval",
            "clearTimeout",
            "requestAnimationFrame",
            "queueMicrotask",
            "matchMedia",
            "getComputedStyle",
            "getSelection",
        ] {
            assert!(
                code.contains(&format!(
                    "w3cos_runtime::jsdom::window_value().get_property(\"{name}\")"
                )),
                "{name} → window function value: {code}"
            );
        }
    }

    #[test]
    fn dynamic_lowering_maps_promise_json_and_web_globals() {
        let stmts = parse_stmts(
            r#"const p = new Promise((resolve) => resolve(1));
const q = Promise.resolve(2);
const all = Promise.all([p, q]);
const obj = JSON.parse("{\"a\":1}");
const text = JSON.stringify(obj);
const bin = atob("aGk=");
const enc = btoa(bin);
const copy = structuredClone(obj);
const u = new URL("https://example.com/x");
const params = new URLSearchParams("a=1");"#,
        );
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains("w3cos_core::promise::new(vec!["),
            "new Promise: {code}"
        );
        assert!(
            code.contains("w3cos_core::promise::resolve(__args)"),
            "Promise.resolve facade: {code}"
        );
        assert!(
            code.contains("w3cos_core::promise::all(__args)"),
            "Promise.all facade: {code}"
        );
        assert!(
            code.contains("w3cos_core::json::parse(__args)"),
            "JSON.parse facade: {code}"
        );
        assert!(
            code.contains("w3cos_core::json::stringify(__args)"),
            "JSON.stringify facade: {code}"
        );
        assert!(
            code.contains("w3cos_core::web::atob(__args)"),
            "atob: {code}"
        );
        assert!(
            code.contains("w3cos_core::web::btoa(__args)"),
            "btoa: {code}"
        );
        assert!(
            code.contains("w3cos_core::web::structured_clone(__args)"),
            "structuredClone: {code}"
        );
        assert!(
            code.contains("w3cos_core::web::url_new(vec!["),
            "new URL: {code}"
        );
        assert!(
            code.contains("w3cos_core::web::url_search_params_new(vec!["),
            "new URLSearchParams: {code}"
        );
    }

    #[test]
    fn dynamic_lowering_intl_is_a_harmless_stub() {
        let stmts = parse_stmts("const nf = Intl.NumberFormat;");
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains(
                "w3cos_core::Value::object(::std::collections::HashMap::new()) /* Intl stub */"
            ),
            "Intl stub object: {code}"
        );
    }

    #[test]
    fn dynamic_lowering_destructuring_reassignment() {
        let stmts = parse_stmts("[a, b] = arr; ({x, y: {z}} = obj);");
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains("let __assign_value = arr")
                && code.contains("a = __assign_value.get_property(\"0\");")
                && code.contains("b = __assign_value.get_property(\"1\");"),
            "array destructuring assignment: {code}"
        );
        assert!(
            code.contains("x = __assign_value.get_property(\"x\");")
                && code.contains("z = __assign_value.get_property(\"y\").get_property(\"z\");"),
            "object destructuring assignment: {code}"
        );
        assert!(
            !code.contains("/* pattern assign */"),
            "no placeholder left: {code}"
        );
    }

    #[test]
    fn dynamic_lowering_hoists_var_declarations_for_closures() {
        // A closure created before the `var` line must still see the binding:
        // the var is pre-declared at the fn top and the declaration lowers to
        // an assignment.
        let stmts =
            parse_stmts("function f() { const g = () => result; var result = 42; return g(); }");
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        let decl_pos = code
            .find("let mut result = w3cos_core::Value::Undefined;")
            .expect("hoisted predecl: {code}");
        let closure_pos = code
            .find("let mut result = result.clone();")
            .expect("capture: {code}");
        let assign_pos = code
            .find("result = w3cos_core::Value::Number(42.0);")
            .expect("var as assignment: {code}");
        assert!(
            decl_pos < closure_pos && closure_pos < assign_pos,
            "predecl → closure capture → assignment: {code}"
        );
        // `var` in a nested block hoists to the fn top as well.
        let stmts = parse_stmts("function f() { if (x) { var y = 1; } return y; }");
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains("let mut y = w3cos_core::Value::Undefined;"),
            "block-nested var hoisted: {code}"
        );
    }

    #[test]
    fn dynamic_lowering_boxes_self_referential_initializer() {
        let stmts = parse_stmts(
            "function createId() { const id = function () { return id; }; return id; }",
        );
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains(
                "let id = std::rc::Rc::new(std::cell::RefCell::new(w3cos_core::Value::Undefined));"
            ),
            "self-referential binding gets a shared cell: {code}"
        );
        assert!(
            code.contains("let mut id = id.clone();")
                && code.contains("return (*id.borrow()).clone();")
                && code.contains("*id.borrow_mut() ="),
            "closure captures the cell and initialization writes through it: {code}"
        );
    }

    #[test]
    fn dynamic_lowering_foreach_binds_item_and_index() {
        let stmts = parse_stmts("items.forEach((value, key) => { seen = key; });");
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains("for (__index, __item) in items.iter().enumerate()"),
            "enumerate loop: {code}"
        );
        assert!(
            code.contains("let mut value = __item;")
                && code.contains("let mut key = w3cos_core::Value::Number(__index as f64);"),
            "item + index params bound: {code}"
        );
    }

    #[test]
    fn dynamic_lowering_nested_fn_decl_captures_as_closure() {
        let stmts = parse_stmts(
            "function outer() { const x = 1; function inner() { return x + 1; } return inner(); }",
        );
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains("let mut inner = w3cos_core::Value::Undefined;"),
            "nested fn name hoisted: {code}"
        );
        assert!(
            code.contains("inner = {")
                && code.contains("let mut x = x.clone();")
                && code.contains("w3cos_core::Value::function(move |__this, __args|"),
            "nested fn → capturing closure value: {code}"
        );
        // Self-recursive nested fns keep the fn-item form (name must be in scope).
        let stmts = parse_stmts(
            "function outer() { function fib(n) { if (n < 2) { return n; } return fib(n-1) + fib(n-2); } return fib(10); }",
        );
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains("fn fib(__args: Vec<w3cos_core::Value>) -> w3cos_core::Value"),
            "self-recursive nested fn keeps fn item: {code}"
        );
        assert!(
            code.contains("fib(vec![n.clone().js_sub(&w3cos_core::Value::Number(1.0)).clone()])")
                || code.contains("fib(vec![n.js_sub(&w3cos_core::Value::Number(1.0))"),
            "recursive call uses the direct fn-item form: {code}"
        );
        assert!(
            !code.contains("let mut fib = w3cos_core::Value::Undefined;"),
            "fn-item names are not hoisted (would shadow the item): {code}"
        );
    }

    #[test]
    fn sanitize_ident_covers_rust_reserved_keywords() {
        for kw in [
            "override", "final", "try", "yield", "gen", "typeof", "macro",
        ] {
            assert_eq!(sanitize_ident(kw), format!("{kw}_"));
        }
    }

    #[test]
    fn dynamic_lowering_local_bindings_shadow_globals() {
        let stmts = parse_stmts(
            r#"const document = { title: "mine" };
const t = document.title;
function f(window) { return window.x; }"#,
        );
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(
            !code.contains("w3cos_runtime::jsdom::document_value()"),
            "local `document` shadows the global: {code}"
        );
        assert!(
            code.contains("document.clone().get_property(\"title\")"),
            "shadowed member read: {code}"
        );
        assert!(
            code.contains("return window.get_property(\"x\");")
                || code.contains("return window.clone().get_property(\"x\");"),
            "fn param `window` shadows the global: {code}"
        );
    }

    #[test]
    fn dynamic_lowering_collects_rest_parameters() {
        let stmts =
            parse_stmts("function createInstance(ctor, ...rest) { return [ctor, rest.length]; }");
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains(
                "let mut rest = w3cos_core::Value::array(__args.iter().skip(1).cloned().collect());"
            ),
            "rest parameter collects every remaining argument: {code}"
        );
    }

    #[test]
    fn lowers_await() {
        let stmts = parse_stmts("async function f() { const x = await fetchData(); return x; }");
        let mut ctx = LowerCtx::new(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(code.contains("fetchData().await"), "await expr: {code}");
    }

    #[test]
    fn lowers_for_of() {
        let stmts = parse_stmts("for (const item of items) { process(item); }");
        let mut ctx = LowerCtx::new(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(code.contains("for item in items.iter()"), "for-of: {code}");
        assert!(code.contains("process(item)"), "for-of body: {code}");
    }

    #[test]
    fn lowers_switch() {
        let stmts = parse_stmts("switch (x) { case 1: a(); break; default: b(); }");
        let mut ctx = LowerCtx::new(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(code.contains("match x"), "switch → match: {code}");
        assert!(code.contains("1 =>"), "case arm: {code}");
        assert!(code.contains("_ =>"), "default arm: {code}");
    }

    #[test]
    fn lowers_optional_chaining() {
        let stmts = parse_stmts("const v = obj?.prop;");
        let mut ctx = LowerCtx::new(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(code.contains("as_ref().map("), "optional chain: {code}");
    }

    fn class_names(names: &[&str]) -> HashSet<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn lowers_new_expr_to_class_construct() {
        let stmts = parse_stmts("const v = new EditorView({});");
        let mut ctx = LowerCtx::new_dynamic(vec![]).with_classes(class_names(&["EditorView"]));
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains("w3cos_core::class::construct(&EditorView(), vec!["),
            "new on a class → construct(&EditorView(), ...): {code}"
        );
    }

    #[test]
    fn lowers_new_expr_for_plain_functions_through_construct() {
        // Not a class and not a builtin special-case: still routed through
        // construct() so constructor-functions-as-objects work.
        let stmts = parse_stmts("const w = new makeWidget(1);");
        let renames = vec![("makeWidget".to_string(), "m0_makeWidget".to_string())];
        let mut ctx = LowerCtx::new_dynamic(renames);
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains("w3cos_core::class::construct(&makeWidget_value()"),
            "new on a function value → construct: {code}"
        );
        // The Error special-case stays intact; Map is no longer special-cased
        // (it routes through construct on the collections class value).
        let stmts = parse_stmts("const m = new Map([\"a\", 1]); const e = new Error(\"x\");");
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains(
                "w3cos_core::class::construct(&w3cos_core::collections::map_class(), vec!["
            ),
            "new Map → construct on the collections class: {code}"
        );
        assert!(
            code.contains("Error::new(vec!["),
            "Error special-case: {code}"
        );
    }

    #[test]
    fn lowers_map_set_globals_to_collection_classes() {
        let stmts = parse_stmts(
            "const m = new Map(); const s = new Set([1]); const wm = new WeakMap(); const ws = new WeakSet(); const ok = m instanceof Map; const M = Map; const S = Set;",
        );
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        for (ctor, class_fn) in [
            ("new Map()", "map_class"),
            ("new Set()", "set_class"),
            ("new WeakMap()", "weak_map_class"),
            ("new WeakSet()", "weak_set_class"),
        ] {
            assert!(
                code.contains(&format!(
                    "w3cos_core::class::construct(&w3cos_core::collections::{class_fn}(), vec!["
                )),
                "{ctor} → construct on the collections class: {code}"
            );
        }
        assert!(
            code.contains(
                "w3cos_core::class::instance_of(&m.clone(), &w3cos_core::collections::map_class())"
            ),
            "instanceof Map → instance_of on the collections class: {code}"
        );
        // Bare `Map`/`Set` references resolve to the class singletons.
        assert!(
            code.contains("let mut M = w3cos_core::collections::map_class();"),
            "Map as a value: {code}"
        );
        assert!(
            code.contains("let mut S = w3cos_core::collections::set_class();"),
            "Set as a value: {code}"
        );
    }

    #[test]
    fn lowers_proxy_to_dynamic_runtime_constructor() {
        let stmts =
            parse_stmts("const p = new Proxy({}, { get(target, key) { return target[key]; } });");
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains("w3cos_core::class::construct(&w3cos_core::proxy_class(), vec!["),
            "new Proxy → dynamic proxy runtime: {code}"
        );
    }

    #[test]
    fn array_facade_exposes_is_array() {
        let stmts = parse_stmts("const result = Array.isArray([]);");
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains("\"isArray\".to_string()")
                && code.contains("Some(w3cos_core::Value::Array(_))"),
            "Array.isArray must inspect the runtime Value variant: {code}"
        );
    }

    #[test]
    fn lowers_regexp_literals_to_runtime_values() {
        let stmts = parse_stmts(
            "const color = /^#?([0-9A-Fa-f]{6})$/i; const result = value.match(color);",
        );
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains("w3cos_core::regexp::create(\"^#?([0-9A-Fa-f]{6})$\", \"i\")"),
            "regexp literal → runtime value: {code}"
        );
        assert!(
            code.contains("call_method(\"match\""),
            "reserved Rust words must remain unchanged as JS property keys: {code}"
        );
    }

    #[test]
    fn dynamic_delete_removes_member_property() {
        let stmts = parse_stmts("delete options.model; delete options[key];");
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains("options.delete_property(&\"model\")")
                && code.contains("options.delete_property(&key.to_js_string())"),
            "delete must perform a runtime property mutation: {code}"
        );
    }

    #[test]
    fn typed_array_globals_use_runtime_storage() {
        let stmts = parse_stmts("const lines = new Uint16Array(4); lines.set([0, 2], 0);");
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains("w3cos_core::collections::typed_array_class()")
                && code.contains("call_method(\"set\""),
            "typed arrays need indexed runtime storage: {code}"
        );
    }

    #[test]
    fn class_method_property_keys_keep_js_reserved_words() {
        let statements = parse_stmts("const Matcher = class { match(value) { return value; } };");
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&statements);
        assert!(
            code.contains("__proto.set_property(\"match\"")
                && !code.contains("__proto.set_property(\"match_\""),
            "Rust identifier sanitization must not alter JS property keys: {code}"
        );
    }

    #[test]
    fn lowers_reflect_construct_to_class_runtime() {
        // Reflect.construct(target, args) → class::construct on the facade
        // (Monaco's DI instantiates services through it).
        let stmts = parse_stmts("const o = Reflect.construct(Target, [1, 2]);");
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains("w3cos_core::class::construct(&__target, __ctor_args)"),
            "Reflect.construct → class::construct facade: {code}"
        );
    }

    #[test]
    fn lowers_symbol_iterator_to_runtime_key() {
        let statements = parse_stmts("const iterator = value[Symbol.iterator]();");
        let mut context = LowerCtx::new_dynamic(vec![]);
        let code = context.lower_stmts(&statements);

        assert!(
            code.contains("__w3cos_symbol_iterator"),
            "well-known iterator symbol: {code}"
        );
    }

    #[test]
    fn lowers_symbol_for_to_stable_registry_key() {
        let statements = parse_stmts("const element = Symbol.for('react.element');");
        let mut context = LowerCtx::new_dynamic(vec![]);
        let code = context.lower_stmts(&statements);

        assert!(
            code.contains("__w3cos_symbol_for:{}") && code.contains("call_method(\"for\""),
            "Symbol.for registry facade: {code}"
        );
    }

    #[test]
    fn lowers_reflect_feature_detection_with_balanced_facade() {
        // DOMPurify destructures this expression at module scope. Keep the
        // `&&` value semantics and, importantly, the inline Reflect facade's
        // HashMap/array delimiters balanced so the generated Rust parses.
        let stmts =
            parse_stmts("let { apply, construct } = typeof Reflect !== 'undefined' && Reflect;");
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains("if __l.to_bool()") && code.contains("else { __l }"),
            "logical-and preserves the selected operand: {code}"
        );
        assert!(
            code.contains("w3cos_core::class::construct(&__target, __ctor_args) }))]))"),
            "Reflect facade closes function, tuple, array, HashMap, and object: {code}"
        );
        assert!(
            !code.contains("w3cos_core::class::construct(&__target, __ctor_args) })))]))"),
            "Reflect facade must not contain the old extra tuple delimiter: {code}"
        );
    }

    #[test]
    fn lowers_instanceof_through_class_runtime() {
        let stmts = parse_stmts("const ok = dog instanceof Animal;");
        let mut ctx = LowerCtx::new_dynamic(vec![]).with_classes(class_names(&["Animal"]));
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains("w3cos_core::class::instance_of(&dog, &Animal())"),
            "instanceof → class::instance_of: {code}"
        );
    }

    #[test]
    fn lowers_class_call_without_new_via_construct() {
        let stmts = parse_stmts("const o = Widget(1);");
        let mut ctx = LowerCtx::new_dynamic(vec![]).with_classes(class_names(&["Widget"]));
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains("w3cos_core::class::construct(&Widget(), vec!["),
            "class call without new → construct: {code}"
        );
    }

    #[test]
    fn lowers_class_expression_with_extends_super_and_privates() {
        let stmts = parse_stmts(
            r#"const Dog = class Dog extends Animal {
  #tag = "dog";
  constructor(name, bark) { super(name); this.bark = bark; }
  get label() { return this.name; }
  set label(v) { this.name = v; }
  describe() { return super.kind() + super.sound + this.#tag; }
  static make(name) { return new Animal(name); }
  static { this.count = 0; }
};"#,
        );
        let mut ctx = LowerCtx::new_dynamic(vec![]).with_classes(class_names(&["Animal"]));
        let code = ctx.lower_stmts(&stmts);

        // Eagerly built class value with call slot.
        assert!(
            code.contains("w3cos_core::Value::callable(std::collections::HashMap::new()"),
            "class object with call slot: {code}"
        );
        // Parent evaluated once, prototype chain wired.
        assert!(code.contains("let __parent = Animal();"), "parent: {code}");
        assert!(
            code.contains(
                "w3cos_core::class::set_prototype_of(&__proto, &__parent.get_property(\"prototype\"));"
            ),
            "proto chain: {code}"
        );
        assert!(
            code.contains("w3cos_core::class::set_prototype_of(&__class, &__parent);"),
            "static inheritance: {code}"
        );
        // super(...) in ctor → super_ctor; field init `this.bark` afterwards.
        assert!(
            code.contains("w3cos_core::class::super_ctor(&__this, &__parent, vec![name.clone()"),
            "super ctor: {code}"
        );
        assert!(
            code.contains("__this.clone().set_property(&\"bark\", __w3cos_av.clone())"),
            "this.bark assignment: {code}"
        );
        // Getter/setter conventions on the prototype.
        assert!(
            code.contains("__proto.set_property(\"__w3cos_getter_label\""),
            "getter install: {code}"
        );
        assert!(
            code.contains("__proto.set_property(\"__w3cos_setter_label\""),
            "setter install: {code}"
        );
        // super.method() / super.prop
        assert!(
            code.contains("w3cos_core::class::super_method(&__this, &__parent, \"kind\", vec![])"),
            "super method: {code}"
        );
        assert!(
            code.contains("w3cos_core::class::super_get(&__this, &__parent, \"sound\")"),
            "super get: {code}"
        );
        // Private field mangled with the class name.
        assert!(
            code.contains("__w3cos_priv_Dog_tag"),
            "private mangle: {code}"
        );
        // Static method installed on the class object; static block runs in place.
        assert!(
            code.contains("__class.set_property(\"make\""),
            "static method: {code}"
        );
        assert!(
            code.contains("__this.clone().set_property(&\"count\""),
            "static block assignment via this = class object: {code}"
        );
        // constructor / prototype / raw-ctor wiring.
        assert!(
            code.contains("__class.set_property(\"prototype\", __proto);")
                && code.contains("__class.set_property(\"__w3cos_ctor\", __ctor);")
                && code.contains("__proto.set_property(\"constructor\", __class.clone());"),
            "wiring: {code}"
        );
    }

    #[test]
    fn lowers_static_super_via_parent_class_object() {
        let stmts = parse_stmts(
            "const S = class extends Base { static go() { return super.run() + super.speed; } };",
        );
        let mut ctx = LowerCtx::new_dynamic(vec![]).with_classes(class_names(&["Base"]));
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains("__parent.get_property(\"run\").call(__this.clone(), vec![])")
                || code.contains("{ let __super_fn = __parent.get_property(\"run\");"),
            "static super call → parent class object: {code}"
        );
        assert!(
            code.contains("__parent.get_property(\"speed\")"),
            "static super read → parent class object: {code}"
        );
    }

    #[test]
    fn lowers_private_brand_check_via_js_in() {
        let stmts = parse_stmts("const P = class P { #x = 1; has(o) { return #x in o; } };");
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains("w3cos_core::Value::from(\"__w3cos_priv_P_x\").js_in(&o.clone())"),
            "#x in o → mangled js_in: {code}"
        );
    }

    #[test]
    fn lowers_nested_class_declaration_statement() {
        let stmts = parse_stmts(
            "function f() { class Local { constructor() { this.ok = true; } } return new Local(); }",
        );
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains("let mut Local = w3cos_core::Value::Undefined;")
                && code.contains("Local = { let __proto = "),
            "nested class → hoisted local class value: {code}"
        );
        assert!(
            code.contains("w3cos_core::class::construct(&Local.clone(), vec![])"),
            "new on the local class: {code}"
        );
    }

    #[test]
    fn lowers_derived_class_expression_without_ctor_forwards_to_super() {
        let stmts = parse_stmts("const B = class extends A { };");
        let mut ctx = LowerCtx::new_dynamic(vec![]).with_classes(class_names(&["A"]));
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains("w3cos_core::class::super_ctor(&__this, &__parent, __args)"),
            "synthesized forwarding ctor: {code}"
        );
    }

    #[test]
    fn lowers_this_and_member_compound_assignment_in_class_scope() {
        let stmts =
            parse_stmts("const C = class C { bump() { this.x += 2; this.y++; return this.x; } };");
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains("__obj.get_property(&\"x\").js_add(&w3cos_core::Value::Number(2.0))"),
            "compound member assign: {code}"
        );
        assert!(
            code.contains("let __w3cos_prev = __obj.get_property(&\"y\");"),
            "member update: {code}"
        );
        assert!(
            code.contains("return __this.clone().get_property(\"x\");"),
            "this read: {code}"
        );
    }

    #[test]
    fn preserves_quotes_at_string_literal_boundaries() {
        let stmts = parse_stmts(r#"const open = 'class="'; const close = '"done';"#);
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains(r#"w3cos_core::Value::from("class=\"")"#),
            "trailing quote: {code}"
        );
        assert!(
            code.contains(r#"w3cos_core::Value::from("\"done")"#),
            "leading quote: {code}"
        );
    }
}
