//! `createRequire(import.meta.url)` in ES modules (M3 item 23).
//!
//! Code-OSS's bootstrap modules load JSON and builtins through a `require`
//! made by `node:module`'s `createRequire`. An AOT app has no loader at run
//! time, so every call with a literal specifier becomes a static import of
//! that module, served as synthesized text under the module's own path.
//! The imported value matches what Node's `require` returns: a JSON file's
//! value, a CommonJS `module.exports`, an ES module's namespace. A specifier
//! that does not resolve becomes an expression that throws Node's
//! `MODULE_NOT_FOUND` error when evaluated, so `try { require(x) } catch {}`
//! keeps working. Calls with a computed specifier keep calling the runtime
//! `require`, which only serves builtins.

use std::collections::HashSet;
use std::path::Path;
use swc_common::{FileName, SourceMap, Spanned, sync::Lrc};
use swc_ecma_ast::*;
use swc_ecma_parser::{Parser, StringInput, Syntax, TsSyntax};
use swc_ecma_visit::{Visit, VisitWith};

/// A `require("x")` call made through a `createRequire` result: the byte
/// range of the call in the source and its specifier.
#[derive(Debug, Clone, PartialEq)]
pub struct RequireCall {
    pub start: usize,
    pub end: usize,
    pub specifier: String,
}

/// How a specifier resolved.
pub enum Target {
    /// JSON file or CommonJS module: the default import is the value.
    Default,
    /// ES module: `require(esm)` returns the namespace.
    Namespace,
    /// Unresolved: throws `MODULE_NOT_FOUND` when evaluated.
    Missing,
}

/// Literal `require` calls made through `createRequire` bindings, in source
/// order. Empty when the module does not import `createRequire`.
pub fn find_calls(path: &Path, source: &str) -> Vec<RequireCall> {
    if !source.contains("createRequire") {
        return Vec::new();
    }
    let cm: Lrc<SourceMap> = Default::default();
    let fm = cm.new_source_file(
        Lrc::new(FileName::Real(path.to_path_buf())),
        source.to_string(),
    );
    let mut parser = Parser::new(
        Syntax::Typescript(TsSyntax::default()),
        StringInput::from(&*fm),
        None,
    );
    let Ok(module) = parser.parse_module() else {
        return Vec::new();
    };
    let mut factories = HashSet::new();
    for item in &module.body {
        if let ModuleItem::ModuleDecl(ModuleDecl::Import(import)) = item
            && matches!(
                &*import.src.value.to_string_lossy(),
                "node:module" | "module"
            )
        {
            for specifier in &import.specifiers {
                if let ImportSpecifier::Named(named) = specifier {
                    let imported = match &named.imported {
                        Some(ModuleExportName::Ident(ident)) => ident.sym.to_string(),
                        Some(ModuleExportName::Str(name)) => name.value.to_string_lossy().into(),
                        None => named.local.sym.to_string(),
                    };
                    if imported == "createRequire" {
                        factories.insert(named.local.sym.to_string());
                    }
                }
            }
        }
    }
    if factories.is_empty() {
        return Vec::new();
    }
    let mut bindings = Bindings {
        factories,
        requires: HashSet::new(),
    };
    module.visit_with(&mut bindings);
    if bindings.requires.is_empty() {
        return Vec::new();
    }
    let base = fm.start_pos.0 as usize;
    let mut calls = Calls {
        requires: bindings.requires,
        base,
        calls: Vec::new(),
    };
    module.visit_with(&mut calls);
    calls.calls.sort_by_key(|call| call.start);
    calls.calls
}

struct Bindings {
    factories: HashSet<String>,
    requires: HashSet<String>,
}

impl Visit for Bindings {
    fn visit_var_declarator(&mut self, declarator: &VarDeclarator) {
        if let (Pat::Ident(name), Some(init)) = (&declarator.name, &declarator.init)
            && let Expr::Call(call) = &**init
            && let Callee::Expr(callee) = &call.callee
            && let Expr::Ident(ident) = &**callee
            && self.factories.contains(&*ident.sym)
        {
            self.requires.insert(name.id.sym.to_string());
        }
        declarator.visit_children_with(self);
    }
}

struct Calls {
    requires: HashSet<String>,
    base: usize,
    calls: Vec<RequireCall>,
}

impl Visit for Calls {
    fn visit_call_expr(&mut self, call: &CallExpr) {
        if let Callee::Expr(callee) = &call.callee
            && let Expr::Ident(ident) = &**callee
            && self.requires.contains(&*ident.sym)
            && call.args.len() == 1
            && call.args[0].spread.is_none()
            && let Expr::Lit(Lit::Str(specifier)) = &*call.args[0].expr
        {
            let span = call.span();
            self.calls.push(RequireCall {
                start: span.lo.0 as usize - self.base,
                end: span.hi.0 as usize - self.base,
                specifier: specifier.value.to_string_lossy().into(),
            });
            return;
        }
        call.visit_children_with(self);
    }
}

/// Rewrite `source`: each call becomes a binding of a prepended static
/// import (or a throwing expression for [`Target::Missing`]). Returns the new
/// text; `targets[i]` belongs to `calls[i]`.
pub fn rewrite(source: &str, calls: &[RequireCall], targets: &[Target]) -> String {
    let mut imports = String::new();
    let mut body = String::with_capacity(source.len());
    let mut cursor = 0;
    let mut bound: Vec<(String, String)> = Vec::new();
    for (call, target) in calls.iter().zip(targets) {
        body.push_str(&source[cursor..call.start]);
        let literal = serde_json::to_string(&call.specifier).unwrap();
        match target {
            Target::Missing => body.push_str(&format!(
                "(() => {{ const error = new Error(\"Cannot find module '\" + {literal} + \"'\"); error.code = \"MODULE_NOT_FOUND\"; throw error; }})()"
            )),
            Target::Default | Target::Namespace => {
                let local = match bound.iter().find(|(spec, _)| *spec == call.specifier) {
                    Some((_, local)) => local.clone(),
                    None => {
                        let local = format!("__w3cos_require_{}", bound.len());
                        if matches!(target, Target::Namespace) {
                            imports.push_str(&format!("import * as {local} from {literal};\n"));
                        } else {
                            imports.push_str(&format!("import {local} from {literal};\n"));
                        }
                        bound.push((call.specifier.clone(), local.clone()));
                        local
                    }
                };
                body.push_str(&local);
            }
        }
        cursor = call.end;
    }
    body.push_str(&source[cursor..]);
    imports + body.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_literal_calls_through_create_require_bindings() {
        let source = "import { createRequire as cr } from \"node:module\";\nconst req = cr(import.meta.url);\nlet a = req(\"../product.json\");\nconst b = req(name);\nfunction f() { return req('node:fs'); }\nconst other = require(\"x\");\n";
        let calls = find_calls(Path::new("a.js"), source);
        let specs: Vec<&str> = calls.iter().map(|c| c.specifier.as_str()).collect();
        assert_eq!(specs, ["../product.json", "node:fs"]);
        assert_eq!(
            &source[calls[0].start..calls[0].end],
            "req(\"../product.json\")"
        );
        let out = rewrite(source, &calls, &[Target::Default, Target::Missing]);
        assert!(out.starts_with("import __w3cos_require_0 from \"../product.json\";\n"));
        assert!(out.contains("let a = __w3cos_require_0;"));
        assert!(out.contains("MODULE_NOT_FOUND"));
        assert!(out.contains("const b = req(name);"));
    }

    #[test]
    fn modules_without_create_require_are_untouched() {
        assert!(find_calls(Path::new("a.js"), "const r = createRequire(x); r('y');").is_empty());
    }
}
