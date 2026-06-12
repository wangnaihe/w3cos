//! ESM statement/expression lowering: JS AST → Rust code strings.
//!
//! This module takes SWC `Stmt` and `Expr` nodes and produces equivalent Rust
//! source text. It is intentionally a "best effort" structural lowering: JS
//! semantics that have no Rust equivalent emit a `todo!()` with a comment.

use swc_ecma_ast::*;

/// Context carried while lowering a single function/method body.
pub struct LowerCtx {
    indent: usize,
    /// local name → bundled name (for cross-module references).
    pub renames: Vec<(String, String)>,
}

impl LowerCtx {
    pub fn new(renames: Vec<(String, String)>) -> Self {
        Self { indent: 2, renames }
    }

    fn pad(&self) -> String {
        " ".repeat(self.indent)
    }

    fn resolve_name(&self, name: &str) -> String {
        self.renames
            .iter()
            .find(|(local, _)| local == name)
            .map(|(_, bundled)| bundled.clone())
            .unwrap_or_else(|| name.to_string())
    }

    pub fn lower_stmts(&mut self, stmts: &[Stmt]) -> String {
        stmts.iter().map(|s| self.lower_stmt(s)).collect::<Vec<_>>().join("\n")
    }

    pub fn lower_stmt(&mut self, stmt: &Stmt) -> String {
        match stmt {
            Stmt::Expr(expr_stmt) => {
                let e = self.lower_expr(&expr_stmt.expr);
                format!("{}{};", self.pad(), e)
            }
            Stmt::Return(ret) => match &ret.arg {
                Some(expr) => format!("{}return {};", self.pad(), self.lower_expr(expr)),
                None => format!("{}return;", self.pad()),
            },
            Stmt::Decl(decl) => self.lower_decl(decl),
            Stmt::Block(block) => {
                let mut out = format!("{}{{\n", self.pad());
                self.indent += 4;
                for s in &block.stmts {
                    out.push_str(&self.lower_stmt(s));
                    out.push('\n');
                }
                self.indent -= 4;
                out.push_str(&format!("{}}}", self.pad()));
                out
            }
            Stmt::If(if_stmt) => self.lower_if(if_stmt),
            Stmt::For(for_stmt) => self.lower_for(for_stmt),
            Stmt::While(while_stmt) => {
                let test = self.lower_expr(&while_stmt.test);
                let body = self.lower_stmt(&while_stmt.body);
                format!("{}while {} {{\n{}\n{}}}", self.pad(), test, body, self.pad())
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
                format!("{}loop {{\n{}\n{}if !({}) {{ break; }}\n{}}}", self.pad(), body, " ".repeat(self.indent + 4), test, self.pad())
            }
            Stmt::ForIn(for_in) => {
                let right = self.lower_expr(&for_in.right);
                let left = match &for_in.left {
                    ForHead::VarDecl(vd) => vd.decls.first().map(|d| self.lower_pat(&d.name)).unwrap_or_else(|| "_".to_string()),
                    ForHead::Pat(p) => self.lower_pat(p),
                    _ => "_".to_string(),
                };
                self.indent += 4;
                let body = self.lower_stmt(&for_in.body);
                self.indent -= 4;
                format!("{}for {left} in {right} {{\n{}\n{}}}", self.pad(), body, self.pad())
            }
            Stmt::ForOf(for_of) => {
                let right = self.lower_expr(&for_of.right);
                let left = match &for_of.left {
                    ForHead::VarDecl(vd) => vd.decls.first().map(|d| self.lower_pat(&d.name)).unwrap_or_else(|| "_".to_string()),
                    ForHead::Pat(p) => self.lower_pat(p),
                    _ => "_".to_string(),
                };
                self.indent += 4;
                let body = self.lower_stmt(&for_of.body);
                self.indent -= 4;
                format!("{}for {left} in {right}.iter() {{\n{}\n{}}}", self.pad(), body, self.pad())
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
                    let name = self.lower_pat(&d.name);
                    let val = d
                        .init
                        .as_ref()
                        .map(|e| self.lower_expr(e))
                        .unwrap_or_else(|| "Default::default()".to_string());
                    let kw = if var_decl.kind == VarDeclKind::Const {
                        "let"
                    } else {
                        "let mut"
                    };
                    lines.push(format!("{}{kw} {name} = {val};", self.pad()));
                }
                lines.join("\n")
            }
            Decl::Fn(fn_decl) => {
                let name = atom_str(&fn_decl.ident.sym);
                let params = fn_decl
                    .function
                    .params
                    .iter()
                    .map(|p| self.lower_pat(&p.pat))
                    .collect::<Vec<_>>()
                    .join(", ");
                let body = fn_decl
                    .function
                    .body
                    .as_ref()
                    .map(|b| self.lower_stmts(&b.stmts))
                    .unwrap_or_default();
                format!("{}fn {name}({params}) {{\n{body}\n{}}}", self.pad(), self.pad())
            }
            _ => format!("{}/* unsupported decl */", self.pad()),
        }
    }

    fn lower_if(&mut self, if_stmt: &IfStmt) -> String {
        let test = self.lower_expr(&if_stmt.test);
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
            out.push_str(&format!("{inner_pad}if !({test}) {{ break; }}\n"));
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
        let mut out = format!("{}// try\n{}{{\n{}\n{}}}", self.pad(), self.pad(), try_body, self.pad());
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
                self.pad(), self.pad(), catch_body, self.pad()
            ));
        }
        if let Some(finalizer) = &try_stmt.finalizer {
            self.indent += 4;
            let fin_body = self.lower_stmts(&finalizer.stmts);
            self.indent -= 4;
            out.push_str(&format!(
                "\n{}// finally\n{}{{\n{}\n{}}}",
                self.pad(), self.pad(), fin_body, self.pad()
            ));
        }
        out
    }

    fn lower_switch(&mut self, switch: &SwitchStmt) -> String {
        let disc = self.lower_expr(&switch.discriminant);
        let mut out = format!("{}match {} {{\n", self.pad(), disc);
        self.indent += 4;
        for case in &switch.cases {
            let pat = match &case.test {
                Some(test) => self.lower_expr(test),
                None => "_".to_string(),
            };
            let body = self.lower_stmts(&case.cons);
            out.push_str(&format!("{}{} => {{\n{}\n{}}}\n", self.pad(), pat, body, self.pad()));
        }
        self.indent -= 4;
        out.push_str(&format!("{}}}", self.pad()));
        out
    }

    pub fn lower_expr(&self, expr: &Expr) -> String {
        match expr {
            Expr::Ident(ident) => self.resolve_name(&atom_str(&ident.sym)),
            Expr::Lit(lit) => self.lower_lit(lit),
            Expr::New(new_expr) => {
                let callee = self.lower_expr(&new_expr.callee);
                let args = new_expr
                    .args
                    .as_ref()
                    .map(|a| a.iter().map(|arg| self.lower_expr(&arg.expr)).collect::<Vec<_>>().join(", "))
                    .unwrap_or_default();
                format!("{callee}::new({args})")
            }
            Expr::Call(call) => self.lower_call(call),
            Expr::Member(member) => self.lower_member(member),
            Expr::Assign(assign) => {
                let left = match &assign.left {
                    AssignTarget::Simple(simple) => match simple {
                        SimpleAssignTarget::Ident(i) => self.resolve_name(&atom_str(&i.id.sym)),
                        SimpleAssignTarget::Member(m) => self.lower_member(m),
                        _ => "/* assign target */".to_string(),
                    },
                    AssignTarget::Pat(_) => "/* pattern assign */".to_string(),
                };
                let right = self.lower_expr(&assign.right);
                format!("{left} = {right}")
            }
            Expr::Bin(bin) => {
                let left = self.lower_expr(&bin.left);
                let right = self.lower_expr(&bin.right);
                let op = lower_bin_op(bin.op);
                format!("{left} {op} {right}")
            }
            Expr::Unary(unary) => {
                let arg = self.lower_expr(&unary.arg);
                let op = lower_unary_op(unary.op);
                format!("{op}{arg}")
            }
            Expr::Update(update) => {
                let arg = self.lower_expr(&update.arg);
                let op = if update.op == UpdateOp::PlusPlus { "+= 1" } else { "-= 1" };
                format!("{arg} {op}")
            }
            Expr::Paren(paren) => {
                let inner = self.lower_expr(&paren.expr);
                format!("({inner})")
            }
            Expr::Arrow(arrow) => {
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
                    return "Default::default()".to_string();
                }
                let fields = obj
                    .props
                    .iter()
                    .filter_map(|prop| match prop {
                        PropOrSpread::Prop(p) => match p.as_ref() {
                            Prop::KeyValue(kv) => {
                                let key = self.lower_prop_name(&kv.key);
                                let val = self.lower_expr(&kv.value);
                                Some(format!("{key}: {val}"))
                            }
                            Prop::Shorthand(ident) => {
                                let name = atom_str(&ident.sym);
                                Some(format!("{name}: {}", self.resolve_name(&name)))
                            }
                            _ => None,
                        },
                        PropOrSpread::Spread(_) => None,
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{{ {fields} }}")
            }
            Expr::Array(arr) => {
                let items = arr
                    .elems
                    .iter()
                    .filter_map(|e| e.as_ref().map(|el| self.lower_expr(&el.expr)))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("vec![{items}]")
            }
            Expr::Tpl(tpl) => {
                let mut parts = Vec::new();
                for (i, quasi) in tpl.quasis.iter().enumerate() {
                    let raw = quasi.raw.to_string();
                    if !raw.is_empty() {
                        parts.push(format!("\"{raw}\""));
                    }
                    if i < tpl.exprs.len() {
                        parts.push(format!("&format!(\"{{}}\", {})", self.lower_expr(&tpl.exprs[i])));
                    }
                }
                if parts.is_empty() {
                    "String::new()".to_string()
                } else {
                    format!("format!(\"{{}}\"{})", parts.iter().map(|p| format!(", {p}")).collect::<String>())
                }
            }
            Expr::This(_) => "self".to_string(),
            Expr::Cond(cond) => {
                let test = self.lower_expr(&cond.test);
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
                        format!("{{ {}; {} }}", setup.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("; "), last)
                    }
                } else {
                    "()".to_string()
                }
            }
            Expr::OptChain(opt) => {
                match opt.base.as_ref() {
                    OptChainBase::Member(member) => {
                        let obj = self.lower_expr(&member.obj);
                        let prop = match &member.prop {
                            MemberProp::Ident(id) => format!(".{}", atom_str(&id.sym)),
                            MemberProp::Computed(c) => format!("[{}]", self.lower_expr(&c.expr)),
                            MemberProp::PrivateName(p) => format!(".{}", atom_str(&p.name)),
                        };
                        format!("{obj}.as_ref().map(|v| v{prop})")
                    }
                    OptChainBase::Call(call) => {
                        let callee = self.lower_expr(&call.callee);
                        let args = call.args.iter().map(|a| self.lower_expr(&a.expr)).collect::<Vec<_>>().join(", ");
                        format!("{callee}.map(|f| f({args}))")
                    }
                }
            }
            Expr::Yield(yield_expr) => {
                let arg = yield_expr
                    .arg
                    .as_ref()
                    .map(|a| self.lower_expr(a))
                    .unwrap_or_else(|| "()".to_string());
                format!("/* yield */ {arg}")
            }
            Expr::Fn(fn_expr) => {
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
        let callee = match &call.callee {
            Callee::Expr(e) => self.lower_expr(e),
            _ => "/* super/import call */".to_string(),
        };
        let args = call
            .args
            .iter()
            .map(|arg| self.lower_expr(&arg.expr))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{callee}({args})")
    }

    fn lower_member(&self, member: &MemberExpr) -> String {
        let obj = self.lower_expr(&member.obj);
        let prop = match &member.prop {
            MemberProp::Ident(id) => format!(".{}", atom_str(&id.sym)),
            MemberProp::Computed(c) => format!("[{}]", self.lower_expr(&c.expr)),
            MemberProp::PrivateName(p) => format!(".{}", atom_str(&p.name)),
        };
        format!("{obj}{prop}")
    }

    fn lower_lit(&self, lit: &Lit) -> String {
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
    if name.is_empty() {
        return "_".to_string();
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
        "self" | "Self" | "super" | "crate" | "type" | "fn" | "mod" | "pub"
        | "let" | "mut" | "ref" | "use" | "impl" | "trait" | "struct" | "enum"
        | "match" | "if" | "else" | "for" | "while" | "loop" | "break"
        | "continue" | "return" | "where" | "as" | "in" | "move" | "async"
        | "await" | "dyn" | "static" | "const" | "unsafe" | "extern" | "true"
        | "false" => {
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
    use swc_common::{sync::Lrc, FileName, SourceMap};
    use swc_ecma_parser::{lexer::Lexer, Parser, StringInput, Syntax, TsSyntax};

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
        assert!(code.contains("let mut y = \"hello\""), "let → let mut: {code}");
    }

    #[test]
    fn lowers_new_expr_to_struct_new() {
        let stmts = parse_stmts("const v = new EditorView({});");
        let renames = vec![("EditorView".to_string(), "m1_EditorView".to_string())];
        let mut ctx = LowerCtx::new(renames);
        let code = ctx.lower_stmts(&stmts);
        assert!(code.contains("m1_EditorView::new("), "new X() → X::new(): {code}");
    }

    #[test]
    fn lowers_method_calls() {
        let stmts = parse_stmts("state.create({doc: \"hi\"});");
        let mut ctx = LowerCtx::new(vec![]);
        let code = ctx.lower_stmts(&stmts);
        assert!(code.contains("state.create("), "method call: {code}");
        assert!(code.contains("doc: \"hi\""), "object arg: {code}");
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
        assert!(code.contains("let mut i = 0;"), "init declared before loop: {code}");
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
