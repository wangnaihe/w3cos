//! ESM statement/expression lowering: JS AST → Rust code strings.
//!
//! This module takes SWC `Stmt` and `Expr` nodes and produces equivalent Rust
//! source text. It is intentionally a "best effort" structural lowering: JS
//! semantics that have no Rust equivalent emit a `todo!()` with a comment.

use std::collections::HashSet;
use swc_ecma_ast::*;

/// Context carried while lowering a single function/method body.
pub struct LowerCtx {
    indent: usize,
    /// local name → bundled name (for cross-module references).
    pub renames: Vec<(String, String)>,
    dynamic_values: bool,
    temp_index: usize,
    value_bindings: HashSet<String>,
    known_values: HashSet<String>,
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
        }
    }

    fn pad(&self) -> String {
        " ".repeat(self.indent)
    }

    fn resolve_name(&self, name: &str) -> String {
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
            format!("{resolved}.clone()")
        } else if self.dynamic_values
            && self.renames.iter().any(|(local, _)| local == name)
            && !self.value_bindings.contains(name)
        {
            format!("w3cos_core::Value::function(move |_this, __args| {resolved}(__args))")
        } else {
            resolved
        }
    }

    pub fn bind_patterns(&mut self, patterns: &[Pat]) {
        for pattern in patterns {
            collect_pattern_names(pattern, &mut self.known_values);
        }
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
                Some(expr) => format!("{}return {};", self.pad(), self.lower_expr(expr)),
                None if self.dynamic_values => {
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
                let body = self.lower_stmt(&while_stmt.body);
                let test = if self.dynamic_values {
                    format!("{test}.to_bool()")
                } else {
                    test
                };
                format!(
                    "{}while {} {{\n{}\n{}}}",
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
                format!("{}panic!(\"{{}}\", {});", self.pad(), arg)
            }
            Stmt::Switch(switch) => self.lower_switch(switch),
            Stmt::Break(_) => format!("{}break;", self.pad()),
            Stmt::Continue(_) => format!("{}continue;", self.pad()),
            Stmt::DoWhile(do_while) => {
                self.indent += 4;
                let body = self.lower_stmt(&do_while.body);
                self.indent -= 4;
                let test = self.lower_expr(&do_while.test);
                format!(
                    "{}loop {{\n{}\n{}if !({}) {{ break; }}\n{}}}",
                    self.pad(),
                    body,
                    " ".repeat(self.indent + 4),
                    test,
                    self.pad()
                )
            }
            Stmt::ForIn(for_in) => {
                let right = self.lower_expr(&for_in.right);
                let left = match &for_in.left {
                    ForHead::VarDecl(vd) => vd
                        .decls
                        .first()
                        .map(|d| self.lower_pat(&d.name))
                        .unwrap_or_else(|| "_".to_string()),
                    ForHead::Pat(p) => self.lower_pat(p),
                    _ => "_".to_string(),
                };
                self.indent += 4;
                let body = self.lower_stmt(&for_in.body);
                self.indent -= 4;
                format!(
                    "{}for {left} in Object.call_method(\"keys\", vec![{right}]).iter() {{\n{}\n{}}}",
                    self.pad(),
                    body,
                    self.pad()
                )
            }
            Stmt::ForOf(for_of) => {
                let right = self.lower_expr(&for_of.right);
                let left = match &for_of.left {
                    ForHead::VarDecl(vd) => vd
                        .decls
                        .first()
                        .map(|d| self.lower_pat(&d.name))
                        .unwrap_or_else(|| "_".to_string()),
                    ForHead::Pat(p) => self.lower_pat(p),
                    _ => "_".to_string(),
                };
                self.indent += 4;
                let body = self.lower_stmt(&for_of.body);
                self.indent -= 4;
                format!(
                    "{}for {left} in {right}.iter() {{\n{}\n{}}}",
                    self.pad(),
                    body,
                    self.pad()
                )
            }
            Stmt::Labeled(labeled) => {
                let label = atom_str(&labeled.label.sym);
                let body = self.lower_stmt(&labeled.body);
                format!("{}// label: {label}\n{body}", self.pad())
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
                    let kw = if var_decl.kind == VarDeclKind::Const {
                        "let"
                    } else {
                        "let mut"
                    };
                    lines.push(format!("{}{kw} {name} = {val};", self.pad()));
                    collect_pattern_names(&d.name, &mut self.known_values);
                }
                lines.join("\n")
            }
            Decl::Fn(fn_decl) => {
                let name = atom_str(&fn_decl.ident.sym);
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
                let body = fn_decl
                    .function
                    .body
                    .as_ref()
                    .map(|b| self.lower_stmts(&b.stmts))
                    .unwrap_or_default();
                if self.dynamic_values {
                    format!(
                        "{}fn {name}({params}) -> w3cos_core::Value {{\n{body}\n{}w3cos_core::Value::Undefined\n{}}}",
                        self.pad(),
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
            _ => format!("{}/* unsupported decl */", self.pad()),
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
        self.indent += 4;
        let body = self.lower_stmt(&for_stmt.body);
        self.indent -= 4;

        let inner_pad = " ".repeat(self.indent + 4);
        let mut out = String::new();
        // Emit init before the loop
        if !init.is_empty() {
            out.push_str(&format!("{}{}\n", self.pad(), init));
        }
        out.push_str(&format!("{}loop {{\n", self.pad()));
        // Emit break condition
        if test != "true" {
            let condition = if self.dynamic_values {
                format!("{test}.to_bool()")
            } else {
                test
            };
            out.push_str(&format!("{inner_pad}if !({condition}) {{ break; }}\n"));
        }
        out.push_str(&body);
        out.push('\n');
        // Emit update
        if !update.is_empty() {
            out.push_str(&format!("{inner_pad}{update};\n"));
        }
        out.push_str(&format!("{}}}", self.pad()));
        out
    }

    fn lower_try(&mut self, try_stmt: &TryStmt) -> String {
        // JS try/catch → Rust closure returning Result, then handle Err.
        // Simplified: emit the try block inline with a comment, then catch as fallback.
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
            let pad = self.pad();
            let mut out = format!("{pad}'__switch: {{\n{pad}    let __disc = {disc};\n");
            let mut default_body = None;
            self.indent += 4;
            for case in &switch.cases {
                let body = self
                    .lower_stmts(&case.cons)
                    .replace("break;", "break '__switch;");
                if let Some(test) = &case.test {
                    let test = self.lower_expr(test);
                    out.push_str(&format!(
                        "{}if __disc.strict_eq(&{test}) {{\n{body}\n{}    break '__switch;\n{}}}\n",
                        self.pad(),
                        self.pad(),
                        self.pad()
                    ));
                } else {
                    default_body = Some(body);
                }
            }
            if let Some(body) = default_body {
                out.push_str(&body);
                out.push('\n');
            }
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
            Expr::Ident(ident) => self.resolve_value(&atom_str(&ident.sym)),
            Expr::Lit(lit) => self.lower_lit(lit),
            Expr::New(new_expr) => {
                let callee = match new_expr.callee.as_ref() {
                    Expr::Ident(identifier) => self.resolve_name(&atom_str(&identifier.sym)),
                    expression => self.lower_expr(expression),
                };
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
                    if matches!(callee.as_str(), "Error" | "Map" | "ResizeObserver") {
                        format!("{callee}::new(vec![{args}])")
                    } else {
                        format!("{callee}::new()")
                    }
                } else {
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
                        MemberProp::Ident(ident) => format!("{:?}", atom_str(&ident.sym)),
                        MemberProp::Computed(computed) => {
                            format!("{}.to_js_string()", self.lower_expr(&computed.expr))
                        }
                        MemberProp::PrivateName(name) => {
                            format!("{:?}", atom_str(&name.name))
                        }
                    };
                    let right = self.lower_expr(&assign.right);
                    return format!(
                        "{{ let value = {right}; {object}.set_property(&{key}, value.clone()); value }}"
                    );
                }
                if self.dynamic_values
                    && let AssignTarget::Simple(SimpleAssignTarget::Ident(identifier)) =
                        &assign.left
                {
                    let local = atom_str(&identifier.id.sym);
                    if self.value_bindings.contains(&local) {
                        let bundled = self
                            .renames
                            .iter()
                            .find(|(name, _)| name == &local)
                            .map(|(_, bundled)| bundled.as_str())
                            .unwrap_or(&local);
                        let right = self.lower_expr(&assign.right);
                        return format!("{bundled}_set({right})");
                    }
                }
                let left = match &assign.left {
                    AssignTarget::Simple(simple) => match simple {
                        SimpleAssignTarget::Ident(i) => self.resolve_name(&atom_str(&i.id.sym)),
                        SimpleAssignTarget::Member(m) => self.lower_member(m),
                        _ => "/* assign target */".to_string(),
                    },
                    AssignTarget::Pat(_) => "/* pattern assign */".to_string(),
                };
                let right = self.lower_expr(&assign.right);
                if self.dynamic_values {
                    format!("{{ let value = {right}; {left} = value.clone(); value }}")
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
                let arg = self.lower_expr(&unary.arg);
                if self.dynamic_values {
                    match unary.op {
                        UnaryOp::Bang => format!("{arg}.js_not()"),
                        UnaryOp::Minus => format!("{arg}.js_neg()"),
                        UnaryOp::TypeOf => format!("w3cos_core::type_of(&{arg})"),
                        UnaryOp::Void => "w3cos_core::Value::Undefined".to_string(),
                        _ => format!("{arg}"),
                    }
                } else {
                    let op = lower_unary_op(unary.op);
                    format!("{op}{arg}")
                }
            }
            Expr::Update(update) => {
                let arg = if self.dynamic_values {
                    match update.arg.as_ref() {
                        Expr::Ident(identifier) => self.resolve_name(&atom_str(&identifier.sym)),
                        expression => self.lower_expr(expression),
                    }
                } else {
                    self.lower_expr(&update.arg)
                };
                if self.dynamic_values {
                    let delta = if update.op == UpdateOp::PlusPlus {
                        "js_add"
                    } else {
                        "js_sub"
                    };
                    return format!(
                        "{{ let previous = {arg}.clone(); {arg} = {arg}.{delta}(&w3cos_core::Value::Number(1.0)); previous }}"
                    );
                }
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
                    let bindings =
                        lower_closure_params(&arrow.params, &self.renames, &self.value_bindings);
                    let parameter_names = pattern_names(&arrow.params);
                    let captures = self
                        .known_values
                        .difference(&parameter_names)
                        .cloned()
                        .collect::<Vec<_>>();
                    let capture_bindings = captures
                        .iter()
                        .map(|name| format!("let mut {name} = {name}.clone(); "))
                        .collect::<String>();
                    let body = match arrow.body.as_ref() {
                        BlockStmtOrExpr::Expr(expression) => {
                            let mut ctx = LowerCtx::new_dynamic_with_bindings(
                                self.renames.clone(),
                                self.value_bindings.clone(),
                            );
                            ctx.known_values.extend(captures.iter().cloned());
                            ctx.bind_patterns(&arrow.params);
                            format!("return {};", ctx.lower_expr(expression))
                        }
                        BlockStmtOrExpr::BlockStmt(block) => {
                            let mut ctx = LowerCtx::new_dynamic_with_bindings(
                                self.renames.clone(),
                                self.value_bindings.clone(),
                            );
                            ctx.known_values.extend(captures.iter().cloned());
                            ctx.bind_patterns(&arrow.params);
                            ctx.indent = self.indent + 4;
                            ctx.lower_stmts(&block.stmts)
                        }
                    };
                    return format!(
                        "{{ {capture_bindings} w3cos_core::Value::function(move |_this, __args| {{ {bindings}{body} w3cos_core::Value::Undefined }}) }}"
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
                                    let name = atom_str(&identifier.sym);
                                    Some(format!(
                                        "w3cos_core::js_object! {{ {name:?} => {} }}",
                                        self.resolve_name(&name)
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
                                let name = atom_str(&ident.sym);
                                Some(format!("{name:?} => {}", self.resolve_name(&name)))
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
            Expr::This(_) => "self".to_string(),
            Expr::Cond(cond) => {
                let mut test = self.lower_expr(&cond.test);
                if self.dynamic_values {
                    test = format!("{test}.to_bool()");
                }
                let cons = self.lower_expr(&cond.cons);
                let alt = self.lower_expr(&cond.alt);
                format!("if {test} {{ {cons} }} else {{ {alt} }}")
            }
            Expr::Await(await_expr) => {
                let arg = self.lower_expr(&await_expr.arg);
                format!("{arg}.await")
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
                            MemberProp::Ident(id) => format!("{:?}", atom_str(&id.sym)),
                            MemberProp::Computed(computed) => {
                                format!("&{}.to_js_string()", self.lower_expr(&computed.expr))
                            }
                            MemberProp::PrivateName(name) => {
                                format!("{:?}", atom_str(&name.name))
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
                    let callee = self.lower_expr(&call.callee);
                    let args = call
                        .args
                        .iter()
                        .map(|a| self.lower_expr(&a.expr))
                        .collect::<Vec<_>>()
                        .join(", ");
                    if self.dynamic_values {
                        format!(
                            "if {callee}.is_nullish() {{ w3cos_core::Value::Undefined }} else {{ {callee}.call(w3cos_core::Value::Undefined, vec![{args}]) }}"
                        )
                    } else {
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
                    let bindings =
                        lower_closure_params(&params, &self.renames, &self.value_bindings);
                    let parameter_names = pattern_names(&params);
                    let captures = self
                        .known_values
                        .difference(&parameter_names)
                        .cloned()
                        .collect::<Vec<_>>();
                    let capture_bindings = captures
                        .iter()
                        .map(|name| format!("let mut {name} = {name}.clone(); "))
                        .collect::<String>();
                    let body = fn_expr
                        .function
                        .body
                        .as_ref()
                        .map(|block| {
                            let mut ctx = LowerCtx::new_dynamic_with_bindings(
                                self.renames.clone(),
                                self.value_bindings.clone(),
                            );
                            ctx.known_values.extend(captures.iter().cloned());
                            ctx.bind_patterns(&params);
                            ctx.indent = self.indent + 4;
                            ctx.lower_stmts(&block.stmts)
                        })
                        .unwrap_or_default();
                    return format!(
                        "{{ {capture_bindings} w3cos_core::Value::function(move |_this, __args| {{ {bindings}{body} w3cos_core::Value::Undefined }}) }}"
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
            Expr::Class(_) => "/* class expr */ Default::default()".to_string(),
            Expr::TaggedTpl(tagged) => {
                let tag = self.lower_expr(&tagged.tag);
                let quasi = self.lower_expr(&Expr::Tpl(*tagged.tpl.clone()));
                format!("{tag}({quasi})")
            }
            _ => format!("todo!(\"lower: {:?}\")", expr_kind_name(expr)),
        }
    }

    fn lower_call(&self, call: &CallExpr) -> String {
        if self.dynamic_values
            && let Callee::Expr(callee) = &call.callee
            && let Expr::Member(member) = callee.as_ref()
            && matches!(&member.prop, MemberProp::Ident(identifier) if identifier.sym == *"forEach")
            && let Some(first) = call.args.first()
            && let Expr::Arrow(arrow) = first.expr.as_ref()
        {
            let object = self.lower_expr(&member.obj);
            let mut ctx = LowerCtx::new_dynamic_with_bindings(
                self.renames.clone(),
                self.value_bindings.clone(),
            );
            ctx.known_values = self.known_values.clone();
            ctx.bind_patterns(&arrow.params);
            let mut bindings = String::new();
            if let Some(pattern) = arrow.params.first() {
                lower_closure_pattern(
                    pattern,
                    "__item",
                    &mut bindings,
                    &self.renames,
                    &self.value_bindings,
                );
            }
            let body = match arrow.body.as_ref() {
                BlockStmtOrExpr::Expr(expression) => {
                    format!("{};", ctx.lower_expr(expression))
                }
                BlockStmtOrExpr::BlockStmt(block) => ctx.lower_stmts(&block.stmts),
            };
            return format!(
                "{{ for __item in {object}.iter() {{ {bindings}{body} }} w3cos_core::Value::Undefined }}"
            );
        }
        if self.dynamic_values
            && let Callee::Expr(callee) = &call.callee
            && let Expr::Member(member) = callee.as_ref()
        {
            let object = self.lower_expr(&member.obj);
            let key = match &member.prop {
                MemberProp::Ident(id) => format!("{:?}", atom_str(&id.sym)),
                MemberProp::Computed(computed) => {
                    format!("&{}.to_js_string()", self.lower_expr(&computed.expr))
                }
                MemberProp::PrivateName(name) => format!("{:?}", atom_str(&name.name)),
            };
            let args = call
                .args
                .iter()
                .map(|arg| self.lower_argument(&arg.expr))
                .collect::<Vec<_>>()
                .join(", ");
            return format!("{object}.call_method({key}, vec![{args}])");
        }
        let callee = match &call.callee {
            Callee::Expr(e) => self.lower_expr(e),
            _ => "/* super/import call */".to_string(),
        };
        let args = call
            .args
            .iter()
            .map(|arg| self.lower_argument(&arg.expr))
            .collect::<Vec<_>>()
            .join(", ");
        if self.dynamic_values
            && let Callee::Expr(expression) = &call.callee
            && let Expr::Ident(identifier) = expression.as_ref()
        {
            let name = atom_str(&identifier.sym);
            let is_static = self.renames.iter().any(|(local, _)| local == &name)
                || matches!(
                    name.as_str(),
                    "parseInt" | "parseFloat" | "RangeError" | "Error"
                );
            if !is_static {
                return format!("{callee}.call(w3cos_core::Value::Undefined, vec![{args}])");
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
                    _ => callee,
                },
                _ => callee,
            };
            format!("{callee}(vec![{args}])")
        } else {
            format!("{callee}({args})")
        }
    }

    fn lower_member(&self, member: &MemberExpr) -> String {
        let obj = self.lower_expr(&member.obj);
        if self.dynamic_values {
            return match &member.prop {
                MemberProp::Ident(id) => {
                    format!("{obj}.get_property({:?})", atom_str(&id.sym))
                }
                MemberProp::Computed(computed) => format!(
                    "{obj}.get_property(&{}.to_js_string())",
                    self.lower_expr(&computed.expr)
                ),
                MemberProp::PrivateName(name) => {
                    format!("{obj}.get_property({:?})", atom_str(&name.name))
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
            PropName::Ident(id) => format!("{:?}", atom_str(&id.sym)),
            PropName::Str(value) => format!("{:?}", wtf8_to_string(&value.value)),
            PropName::Num(value) => format!("{:?}", value.value.to_string()),
            PropName::Computed(value) => {
                format!("{}.to_js_string()", self.lower_expr(&value.expr))
            }
            PropName::BigInt(value) => format!("{:?}", value.value.to_string()),
        }
    }

    fn lower_dynamic_function_value(&self, params: &[Pat], body: &[Stmt]) -> String {
        let bindings = lower_closure_params(params, &self.renames, &self.value_bindings);
        let parameter_names = pattern_names(params);
        let captures = self
            .known_values
            .difference(&parameter_names)
            .cloned()
            .collect::<Vec<_>>();
        let capture_bindings = captures
            .iter()
            .map(|name| format!("let mut {name} = {name}.clone(); "))
            .collect::<String>();
        let mut ctx =
            LowerCtx::new_dynamic_with_bindings(self.renames.clone(), self.value_bindings.clone());
        ctx.known_values.extend(captures.iter().cloned());
        ctx.bind_patterns(params);
        let body = ctx.lower_stmts(body);
        format!(
            "{{ {capture_bindings} w3cos_core::Value::function(move |_this, __args| {{ {bindings}{body} w3cos_core::Value::Undefined }}) }}"
        )
    }

    fn lower_dynamic_local_pattern(
        &mut self,
        pattern: &Pat,
        source: &str,
        lines: &mut Vec<String>,
        indent: usize,
    ) {
        let pad = " ".repeat(indent);
        match pattern {
            Pat::Ident(ident) => lines.push(format!(
                "{pad}let {} = {source};",
                sanitize_ident(&ident.id.sym.to_string())
            )),
            Pat::Array(array) => {
                for (index, element) in array.elems.iter().enumerate() {
                    if let Some(element) = element {
                        let nested = format!("{source}.get_property({:?})", index.to_string());
                        self.lower_dynamic_local_pattern(element, &nested, lines, indent);
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
                            if let Some(default) = &assign.value {
                                let fallback = self.lower_expr(default);
                                lines.push(format!(
                                    "{pad}let {name} = {{ let value = {value}; if value.is_undefined() {{ {fallback} }} else {{ value }} }};"
                                ));
                            } else {
                                lines.push(format!("{pad}let {name} = {value};"));
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
                            self.lower_dynamic_local_pattern(
                                &key_value.value,
                                &nested,
                                lines,
                                indent,
                            );
                        }
                        ObjectPatProp::Rest(rest) => {
                            self.lower_dynamic_local_pattern(&rest.arg, source, lines, indent)
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
                        lower_closure_pattern(&rest.arg, source, output, renames, value_bindings)
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

fn atom_str(atom: &impl ToString) -> String {
    sanitize_ident(&atom.to_string())
}

/// Wtf8Atom (string literal values) has no Display; recover via Debug + trim.
fn wtf8_to_string(atom: &impl std::fmt::Debug) -> String {
    format!("{:?}", atom).trim_matches('"').to_string()
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
        | "true" | "false" => {
            out.push('_');
            out
        }
        _ => out,
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
            format!("if {left}.to_bool() {{ {right} }} else {{ {left}.clone() }}")
        }
        BinaryOp::LogicalOr => {
            format!("if {left}.to_bool() {{ {left}.clone() }} else {{ {right} }}")
        }
        BinaryOp::NullishCoalescing => {
            format!("if {left}.is_nullish() {{ {right} }} else {{ {left}.clone() }}")
        }
        BinaryOp::BitAnd => format!("{left}.js_bitand(&{right})"),
        BinaryOp::BitOr => format!("{left}.js_bitor(&{right})"),
        BinaryOp::BitXor => format!("{left}.js_bitxor(&{right})"),
        BinaryOp::LShift => format!("{left}.js_shl(&{right})"),
        BinaryOp::RShift => format!("{left}.js_shr(&{right})"),
        BinaryOp::ZeroFillRShift => format!("{left}.js_ushr(&{right})"),
        BinaryOp::In => format!("{left}.js_in(&{right})"),
        BinaryOp::InstanceOf => "w3cos_core::Value::Bool(false)".to_string(),
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
        let stmts =
            parse_stmts("const [value, setValue] = state; const {height: h, width = 10} = size;");
        let mut ctx = LowerCtx::new_dynamic(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(
            code.contains("let value = __binding0.get_property(\"0\")")
                && code.contains("let setValue = __binding0.get_property(\"1\")"),
            "array destructuring: {code}"
        );
        assert!(
            code.contains("let h = __binding1.get_property(\"height\")")
                && code.contains("let width = { let value = __binding1.get_property(\"width\")"),
            "object destructuring: {code}"
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
                && code.contains("let current = __args.get(0)")
                && code.contains("set_property(&\"value\"")
                && code.contains("is_nullish()"),
            "dynamic closure lowering: {code}"
        );
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
}
