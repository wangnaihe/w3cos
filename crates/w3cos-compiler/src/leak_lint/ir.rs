//! W3IR pass: lower the file and run the compiler's `unique_back_edges`
//! analysis on every function. Many real-world TS files use constructs the
//! lowering does not support yet (parameter decorators, namespaces, ...);
//! those are skipped and counted, never reported as findings.

use super::rules::{Lines, parse};
use super::{Finding, Severity};
use crate::unique_back_edges::analyze_unique_back_edges;
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitWith};

/// First line of each named function / method / function-valued binding.
#[derive(Default)]
struct FunctionLines {
    lines: Vec<(String, swc_common::BytePos)>,
}

impl Visit for FunctionLines {
    fn visit_fn_decl(&mut self, n: &FnDecl) {
        self.lines.push((n.ident.sym.to_string(), n.ident.span.lo));
        n.visit_children_with(self);
    }
    fn visit_class_method(&mut self, n: &ClassMethod) {
        if let PropName::Ident(i) = &n.key {
            self.lines.push((i.sym.to_string(), i.span.lo));
        }
        n.visit_children_with(self);
    }
    fn visit_constructor(&mut self, n: &Constructor) {
        self.lines.push(("constructor".into(), n.span.lo));
        n.visit_children_with(self);
    }
    fn visit_var_declarator(&mut self, n: &VarDeclarator) {
        if let Pat::Ident(i) = &n.name
            && matches!(n.init.as_deref(), Some(Expr::Arrow(_) | Expr::Fn(_)))
        {
            self.lines.push((i.id.sym.to_string(), i.id.span.lo));
        }
        n.visit_children_with(self);
    }
}

pub(super) fn lint(source: &str, file: &str) -> (bool, Vec<Finding>) {
    let module = catch_unwind(AssertUnwindSafe(|| {
        crate::w3ir_lowering::lower_module(source, file)
            .or_else(|_| crate::w3ir_lowering::lower_script(source, file))
            .ok()
    }))
    .ok()
    .flatten();
    let Some(module) = module else {
        return (false, Vec::new());
    };
    let mut positions: HashMap<String, (usize, usize)> = HashMap::new();
    if let Some((program, base)) = parse(source, file) {
        let lines = Lines::new(source, base);
        let mut collect = FunctionLines::default();
        program.visit_with(&mut collect);
        for (name, pos) in collect.lines {
            positions
                .entry(name)
                .or_insert_with(|| lines.line_col(lines.offset(pos)));
        }
    }
    let names: HashMap<_, _> = module
        .functions
        .iter()
        .map(|f| (f.id, f.name.clone().unwrap_or_else(|| "<anonymous>".into())))
        .collect();
    let mut findings = Vec::new();
    for function in &module.functions {
        let plan = catch_unwind(AssertUnwindSafe(|| {
            analyze_unique_back_edges(function, Some(file))
        }));
        let Ok(plan) = plan else { continue };
        let owner = names.get(&function.id).cloned().unwrap_or_default();
        // Methods lower as `Class.method`.
        let short = owner.rsplit('.').next().unwrap_or(&owner);
        let (line, column) = positions.get(short).copied().unwrap_or((1, 1));
        let mut captures: Vec<_> = plan.weak_captures.iter().collect();
        captures.sort_by_key(|(f, b)| (f.0, b.0));
        for (nested, binding) in captures {
            let binding_name = function
                .bindings
                .iter()
                .find(|b| b.id == *binding)
                .map(|b| b.name.clone())
                .unwrap_or_else(|| format!("binding {}", binding.0));
            if binding_name.starts_with('*') {
                // `this` captures are reported (with a line) by the AST rule.
                continue;
            }
            let nested_name = names.get(nested).cloned().unwrap_or_default();
            findings.push(Finding {
                file: file.to_string(),
                line,
                column,
                pattern: "closure-back-edge",
                severity: Severity::Info,
                message: format!(
                    "in `{owner}`: closure `{nested_name}` captures `{binding_name}` and is stored back onto it (W3IR unique back-edge)"
                ),
                suggestion: "AOT already boxes this capture as a WeakRef; no source change needed for w3cos builds".into(),
                fix: None,
            });
        }
        for warning in &plan.warnings {
            let pattern = if warning.contains("mutual properties") {
                "mutual-properties"
            } else if warning.contains("cross-file") {
                "cross-file-cycle"
            } else {
                // "dynamic property assignment" fires for any computed store and is too noisy for a lint.
                continue;
            };
            findings.push(Finding {
                file: file.to_string(),
                line,
                column,
                pattern,
                severity: Severity::Warning,
                message: format!("in `{owner}`: {warning}"),
                suggestion: if pattern == "mutual-properties" {
                    "two objects point at each other strongly; make the non-owning side a WeakRef".into()
                } else {
                    "a closure captured from another module is stored on a different object; break the cycle with a WeakRef or explicit dispose".into()
                },
                fix: None,
            });
        }
    }
    (true, findings)
}
