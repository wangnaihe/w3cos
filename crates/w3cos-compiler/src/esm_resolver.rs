//! ESM/npm resolver for the w3cos compile pipeline.
//!
//! This module does not interpret JavaScript. It resolves npm/ESM modules at
//! compile time so the compiler can load real package sources, build a module
//! graph, and then lower the combined SWC AST into w3cos/Rust.

use crate::cjs::{CjsAnalysis, RequireTarget};
use crate::module_source;
use crate::node_resolution::{
    PackageJson, ResolveKind, TargetResolution, is_node_builtin, resolve_package_exports,
    resolve_package_imports,
};
use anyhow::{Context, Result, anyhow};
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use swc_common::{FileName, SourceMap, sync::Lrc};
use swc_ecma_ast::*;
use swc_ecma_parser::{Parser, StringInput, Syntax, TsSyntax};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleKind {
    Entry,
    Relative,
    Package,
}

#[derive(Debug, Clone)]
pub struct ResolvedModule {
    pub specifier: String,
    pub path: PathBuf,
    pub kind: ModuleKind,
    pub package_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ModuleGraphNode {
    pub module: ResolvedModule,
    pub imports: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ModuleGraph {
    pub nodes: Vec<ModuleGraphNode>,
}

impl ModuleGraph {
    pub fn contains_path(&self, path: &Path) -> bool {
        self.nodes.iter().any(|node| node.module.path == path)
    }

    pub fn package_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        for node in &self.nodes {
            if let Some(name) = &node.module.package_name {
                if !names.contains(name) {
                    names.push(name.clone());
                }
            }
        }
        names
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportBinding {
    pub imported: String,
    pub local: String,
    pub source: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportBinding {
    pub exported: String,
    pub local: Option<String>,
    pub source: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ParsedModuleInfo {
    pub path: PathBuf,
    /// Every static import source, including bare side-effect imports whose
    /// declarations have no local bindings.
    pub dependency_sources: Vec<String>,
    pub imports: Vec<ImportBinding>,
    pub exports: Vec<ExportBinding>,
    pub top_level_classes: Vec<String>,
    pub top_level_functions: Vec<String>,
    pub top_level_variables: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ParsedModuleGraph {
    pub modules: Vec<ParsedModuleInfo>,
    /// Lazily built path → index map so `find_module` is O(1) on large graphs
    /// (Monaco-scale graphs have hundreds of modules and thousands of lookups).
    path_index: RefCell<HashMap<PathBuf, usize>>,
}

impl ParsedModuleGraph {
    pub fn total_imports(&self) -> usize {
        self.modules.iter().map(|m| m.imports.len()).sum()
    }

    pub fn total_exports(&self) -> usize {
        self.modules.iter().map(|m| m.exports.len()).sum()
    }

    pub fn exported_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        for module in &self.modules {
            for export in &module.exports {
                if !names.contains(&export.exported) {
                    names.push(export.exported.clone());
                }
            }
        }
        names
    }

    pub fn find_module(&self, path: &Path) -> Option<&ParsedModuleInfo> {
        if self.path_index.borrow().len() != self.modules.len() {
            let mut index = self.path_index.borrow_mut();
            index.clear();
            for (i, module) in self.modules.iter().enumerate() {
                index.insert(module.path.clone(), i);
            }
        }
        let i = *self.path_index.borrow().get(path)?;
        self.modules.get(i)
    }

    pub fn resolve_binding(
        &self,
        from_module: &Path,
        imported_name: &str,
        resolver: &EsmResolver,
    ) -> SymbolResolution {
        self.resolve_binding_inner(from_module, imported_name, resolver, &mut HashSet::new())
    }

    fn resolve_binding_inner(
        &self,
        from_module: &Path,
        imported_name: &str,
        resolver: &EsmResolver,
        visited: &mut HashSet<(PathBuf, String)>,
    ) -> SymbolResolution {
        let key = (from_module.to_path_buf(), imported_name.to_string());
        if !visited.insert(key) {
            return SymbolResolution::Circular;
        }

        let module_info = match self.find_module(from_module) {
            Some(info) => info,
            None => {
                return SymbolResolution::Unresolved {
                    name: imported_name.to_string(),
                    reason: format!("module {} not in graph", from_module.display()),
                };
            }
        };

        let from_dir = from_module.parent().unwrap_or_else(|| Path::new("."));

        let import_binding = module_info
            .imports
            .iter()
            .find(|imp| imp.local == imported_name);
        if let Some(binding) = import_binding {
            let target_path = match resolver.resolve(&binding.source, from_dir) {
                Ok(resolved) => resolved.path,
                Err(e) => {
                    return SymbolResolution::Unresolved {
                        name: imported_name.to_string(),
                        reason: format!("could not resolve source `{}`: {e}", binding.source),
                    };
                }
            };
            return self.resolve_export_from(&target_path, &binding.imported, resolver, visited);
        }

        SymbolResolution::Unresolved {
            name: imported_name.to_string(),
            reason: format!(
                "`{}` not found as import in {}",
                imported_name,
                from_module.display()
            ),
        }
    }

    fn resolve_export_from(
        &self,
        module_path: &Path,
        export_name: &str,
        resolver: &EsmResolver,
        visited: &mut HashSet<(PathBuf, String)>,
    ) -> SymbolResolution {
        let key = (module_path.to_path_buf(), export_name.to_string());
        if !visited.insert(key) {
            return SymbolResolution::Circular;
        }

        let module_info = match self.find_module(module_path) {
            Some(info) => info,
            None => {
                return SymbolResolution::Unresolved {
                    name: export_name.to_string(),
                    reason: format!("module {} not in graph", module_path.display()),
                };
            }
        };

        for export in &module_info.exports {
            if export.exported != export_name {
                continue;
            }

            if let Some(source) = &export.source {
                let from_dir = module_path.parent().unwrap_or_else(|| Path::new("."));
                let target_path = match resolver.resolve(source, from_dir) {
                    Ok(resolved) => resolved.path,
                    Err(_) => {
                        return SymbolResolution::Unresolved {
                            name: export_name.to_string(),
                            reason: format!("re-export source `{source}` unresolvable"),
                        };
                    }
                };
                let orig_name = export.local.as_deref().unwrap_or(export_name);
                return self.resolve_export_from(&target_path, orig_name, resolver, visited);
            }

            // `import { X } from "./a.js"; export { X };` — the local name is
            // an import binding whose live cell lives in `./a.js`, not here.
            // Namespace imports (`import * as ns` / `export * as ns`) stay local:
            // their cell is the module's synthetic namespace object, not an
            // export named `*` on the target.
            let local_name = export
                .local
                .clone()
                .unwrap_or_else(|| export_name.to_string());
            if let Some(binding) = module_info
                .imports
                .iter()
                .find(|imp| imp.local == local_name && imp.imported != "*")
            {
                let from_dir = module_path.parent().unwrap_or_else(|| Path::new("."));
                match resolver.resolve(&binding.source, from_dir) {
                    Ok(resolved) => {
                        return self.resolve_export_from(
                            &resolved.path,
                            &binding.imported,
                            resolver,
                            visited,
                        );
                    }
                    Err(error) => {
                        return SymbolResolution::Unresolved {
                            name: export_name.to_string(),
                            reason: format!(
                                "export `{}` re-exports import from `{}`: {error}",
                                local_name, binding.source
                            ),
                        };
                    }
                }
            }

            return SymbolResolution::Resolved(ResolvedSymbol {
                defining_module: module_path.to_path_buf(),
                local_name,
                exported_name: export_name.to_string(),
                kind: if module_info.top_level_classes.contains(
                    &export
                        .local
                        .clone()
                        .unwrap_or_else(|| export_name.to_string()),
                ) {
                    SymbolKind::Class
                } else if module_info.top_level_functions.contains(
                    &export
                        .local
                        .clone()
                        .unwrap_or_else(|| export_name.to_string()),
                ) {
                    SymbolKind::Function
                } else {
                    SymbolKind::Variable
                },
            });
        }

        // No direct export matched — forward through `export * from` chains
        // in declaration order (direct exports shadow star exports, checked
        // above; first star hit wins, matching ESM ambiguity-tolerant usage).
        for export in &module_info.exports {
            if export.exported != "*" {
                continue;
            }
            let Some(source) = &export.source else {
                continue;
            };
            let from_dir = module_path.parent().unwrap_or_else(|| Path::new("."));
            let Ok(resolved) = resolver.resolve(source, from_dir) else {
                continue;
            };
            match self.resolve_export_from(&resolved.path, export_name, resolver, visited) {
                SymbolResolution::Unresolved { .. } => continue,
                other => return other,
            }
        }

        SymbolResolution::Unresolved {
            name: export_name.to_string(),
            reason: format!(
                "`{}` not exported from {}",
                export_name,
                module_path.display()
            ),
        }
    }

    /// Enumerate every export a module exposes, following `export * from`
    /// chains. Used to build `import * as ns` namespace objects: each name is
    /// resolved through the normal precedence rules (direct exports shadow
    /// star exports).
    pub fn all_exports(&self, module_path: &Path, resolver: &EsmResolver) -> Vec<ResolvedSymbol> {
        let mut names: Vec<String> = Vec::new();
        let mut queue: Vec<(PathBuf, bool)> = vec![(module_path.to_path_buf(), true)];
        let mut seen_modules: HashSet<PathBuf> = HashSet::new();
        while let Some((path, include_default)) = queue.pop() {
            if !seen_modules.insert(path.clone()) {
                continue;
            }
            let Some(info) = self.find_module(&path) else {
                continue;
            };
            for export in &info.exports {
                if export.exported == "*" {
                    if let Some(source) = &export.source {
                        let from_dir = path.parent().unwrap_or_else(|| Path::new("."));
                        if let Ok(resolved) = resolver.resolve(source, from_dir) {
                            queue.push((resolved.path, false));
                        }
                    }
                } else if (include_default || export.exported != "default")
                    && !names.contains(&export.exported)
                {
                    names.push(export.exported.clone());
                }
            }
        }
        let mut symbols = Vec::new();
        for name in names {
            if let SymbolResolution::Resolved(mut sym) =
                self.resolve_export_from(module_path, &name, resolver, &mut HashSet::new())
            {
                // Resolution identifies the defining cell, while the
                // namespace must retain the name exposed by this module
                // (`export { value as alias } from ...`).
                sym.exported_name = name;
                symbols.push(sym);
            }
        }
        symbols
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolKind {
    Class,
    Function,
    Variable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSymbol {
    pub defining_module: PathBuf,
    pub local_name: String,
    pub exported_name: String,
    pub kind: SymbolKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolResolution {
    Resolved(ResolvedSymbol),
    Unresolved { name: String, reason: String },
    Circular,
}

/// A single top-level definition lifted into the flattened bundle namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundledSymbol {
    /// Unique, collision-free name inside the bundle, e.g. `m1_EditorView`.
    pub bundled_name: String,
    /// Original local name inside its defining module.
    pub original_name: String,
    pub module: PathBuf,
    pub kind: SymbolKind,
}

/// An `import * as ns from "..."` binding, resolved at bundle-build time.
///
/// The namespace object is materialized by codegen as a lazily built
/// `Value::object` whose properties are the target module's exports (star
/// re-export chains followed by `ParsedModuleGraph::all_exports`).
#[derive(Debug, Clone)]
pub struct NamespaceImport {
    /// Local identifier as written in the importing module (e.g. `monaco`).
    pub local: String,
    /// Module the namespace object is built from.
    pub target: PathBuf,
    /// Exports of the target module, resolved to their defining symbols.
    pub exports: Vec<ResolvedSymbol>,
}

#[derive(Debug, Clone)]
pub struct RuntimeImport {
    pub local: String,
    pub imported: String,
    pub specifier: String,
}

/// A module after flattening: dependency order + per-module namespace.
#[derive(Debug, Clone)]
pub struct BundledModule {
    pub path: PathBuf,
    pub index: usize,
    /// Namespace prefix that isolates this module's symbols, e.g. `m1`.
    pub namespace: String,
    /// Local identifier (as written in this module) -> bundled name it refers to.
    /// Covers both this module's own definitions and its imported bindings.
    pub local_to_bundled: Vec<(String, String)>,
    /// Imports implemented by a native AOT host ABI rather than another ESM
    /// source module (for example framework hooks and JSX runtime entry points).
    pub host_imports: Vec<(String, String)>,
    /// JSON/default asset imports embedded as compile-time literals.
    pub literal_imports: Vec<(String, String)>,
    /// `import * as ns` bindings: the local name evaluates to a lazily built
    /// namespace object exposing the target module's exports.
    pub namespace_imports: Vec<NamespaceImport>,
    /// Imports supplied at runtime through the Core module registry.
    pub runtime_imports: Vec<RuntimeImport>,
    /// Runtime-only side-effect and binding dependencies in source order.
    pub runtime_dependencies: Vec<String>,
    /// Public ESM surface of this module after resolving named and star
    /// re-exports to their defining live bindings.
    pub shared_exports: Vec<ResolvedSymbol>,
}

impl BundledModule {
    pub fn lookup(&self, local: &str) -> Option<&str> {
        self.local_to_bundled
            .iter()
            .find(|(name, _)| name == local)
            .map(|(_, bundled)| bundled.as_str())
    }
}

/// Flattened module graph: dependency-ordered modules with import/export edges
///消解为 bundle 内的直接引用。这是 Rust codegen 的输入。
#[derive(Debug, Clone, Default)]
pub struct EsmBundle {
    pub entry: PathBuf,
    /// Modules in dependency order (dependencies before dependents).
    pub modules: Vec<BundledModule>,
    /// All top-level definitions across every module, with unique names.
    pub symbols: Vec<BundledSymbol>,
    /// Imports that could not be bound to a definition.
    pub unresolved: Vec<String>,
    /// Index of the first module that is only reachable through literal
    /// `import()` calls (`None`: every module is evaluated at startup).
    /// Modules from this index on are evaluated when first imported.
    pub lazy_start: Option<usize>,
    /// Literal `import()` edges, with the lazy modules each one evaluates.
    pub dynamic_imports: Vec<BundledDynamicImport>,
}

/// A literal `import("specifier")` linked into the bundle.
#[derive(Debug, Clone, Default)]
pub struct BundledDynamicImport {
    pub referrer: PathBuf,
    pub specifier: String,
    /// Bundle index of the imported module.
    pub target: usize,
    /// Lazy modules in the target's static closure, dependencies first
    /// (bundle indices); evaluating the import runs their initializers.
    pub evaluation_order: Vec<usize>,
}

impl EsmBundle {
    /// Flatten a parsed graph into a single bundle with collision-free names.
    pub fn build(parsed: &ParsedModuleGraph, resolver: &EsmResolver, entry: &Path) -> Self {
        let (order, lazy_start) = topo_order(parsed, resolver, entry);

        let mut bundle = EsmBundle {
            entry: entry.to_path_buf(),
            lazy_start,
            ..Default::default()
        };
        if lazy_start.is_some() {
            let index_of: HashMap<&Path, usize> = order
                .iter()
                .enumerate()
                .map(|(index, path)| (path.as_path(), index))
                .collect();
            let lazy_from = lazy_start.unwrap_or(order.len());
            for edge in resolver.dynamic_imports() {
                let Some(&target) = index_of.get(edge.target.as_path()) else {
                    continue;
                };
                let mut visited = HashSet::new();
                let mut closure = Vec::new();
                visit_module(&edge.target, parsed, resolver, &mut visited, &mut closure);
                let evaluation_order = closure
                    .iter()
                    .filter_map(|path| index_of.get(path.as_path()).copied())
                    .filter(|&index| index >= lazy_from)
                    .collect();
                bundle.dynamic_imports.push(BundledDynamicImport {
                    referrer: edge.referrer,
                    specifier: edge.specifier,
                    target,
                    evaluation_order,
                });
            }
        }

        // Pass 1: assign namespaces and lift each module's own definitions.
        for (index, path) in order.iter().enumerate() {
            let namespace = format!("m{index}");
            let mut local_to_bundled = Vec::new();

            if let Some(info) = parsed.find_module(path) {
                // `var x; var x;` and `var f; function f() {}` declare one
                // binding: keep the first (functions before variables).
                let mut declared = HashSet::new();
                for name in info
                    .top_level_classes
                    .iter()
                    .chain(info.top_level_functions.iter())
                    .chain(info.top_level_variables.iter())
                {
                    if !declared.insert(name.as_str()) {
                        continue;
                    }
                    let bundled_name =
                        format!("{namespace}_{}", crate::esm_lowering::sanitize_ident(name));
                    let kind = if info.top_level_classes.contains(name) {
                        SymbolKind::Class
                    } else if info.top_level_variables.contains(name) {
                        SymbolKind::Variable
                    } else {
                        SymbolKind::Function
                    };
                    bundle.symbols.push(BundledSymbol {
                        bundled_name: bundled_name.clone(),
                        original_name: name.clone(),
                        module: path.clone(),
                        kind,
                    });
                    local_to_bundled.push((name.clone(), bundled_name));
                }
            }
            let runtime_dependencies = parsed
                .find_module(path)
                .map(|info| {
                    info.dependency_sources
                        .iter()
                        .filter(|source| resolver.is_runtime_module(source))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();

            bundle.modules.push(BundledModule {
                path: path.clone(),
                index,
                namespace,
                local_to_bundled,
                host_imports: Vec::new(),
                literal_imports: Vec::new(),
                namespace_imports: Vec::new(),
                runtime_imports: Vec::new(),
                runtime_dependencies,
                shared_exports: parsed.all_exports(path, resolver),
            });
        }

        // Pass 2: resolve each module's imports to the defining symbol's bundled name.
        // Pre-index symbols by (module, original name) — a linear scan per import
        // is O(imports × symbols) and dominates on Monaco-scale graphs.
        let mut symbol_lookup: HashMap<(PathBuf, String), String> = HashMap::new();
        for sym in &bundle.symbols {
            symbol_lookup.insert(
                (sym.module.clone(), sym.original_name.clone()),
                sym.bundled_name.clone(),
            );
        }
        for module_index in 0..bundle.modules.len() {
            let path = bundle.modules[module_index].path.clone();
            let info = match parsed.find_module(&path) {
                Some(info) => info.clone(),
                None => continue,
            };

            for import in &info.imports {
                if resolver.is_runtime_module(&import.source) {
                    bundle.modules[module_index]
                        .runtime_imports
                        .push(RuntimeImport {
                            local: import.local.clone(),
                            imported: import.imported.clone(),
                            specifier: import.source.clone(),
                        });
                    continue;
                }
                if Path::new(&import.source)
                    .extension()
                    .and_then(|ext| ext.to_str())
                    == Some("json")
                {
                    let from_dir = path.parent().unwrap_or_else(|| Path::new("."));
                    match resolver
                        .resolve(&import.source, from_dir)
                        .and_then(|resolved| {
                            std::fs::read_to_string(&resolved.path).with_context(|| {
                                format!("Could not read JSON import {}", resolved.path.display())
                            })
                        }) {
                        Ok(source) => bundle.modules[module_index]
                            .literal_imports
                            .push((import.local.clone(), source)),
                        Err(error) => bundle.unresolved.push(format!(
                            "{}: `{}` -> could not embed `{}`: {error}",
                            path.display(),
                            import.local,
                            import.source
                        )),
                    }
                    continue;
                }
                if let Some(host_path) = host_import_path(&import.source, &import.imported) {
                    bundle.modules[module_index]
                        .host_imports
                        .push((import.local.clone(), host_path.to_string()));
                    continue;
                }
                // `import * as ns from "..."` — not a symbol binding; the local
                // name denotes the target module's namespace object, built by
                // codegen from the target's full export list.
                if import.imported == "*" {
                    let from_dir = path.parent().unwrap_or_else(|| Path::new("."));
                    match resolver.resolve(&import.source, from_dir) {
                        Ok(resolved) => {
                            let exports = parsed.all_exports(&resolved.path, resolver);
                            bundle.modules[module_index]
                                .namespace_imports
                                .push(NamespaceImport {
                                    local: import.local.clone(),
                                    target: resolved.path,
                                    exports,
                                });
                        }
                        Err(e) => {
                            bundle.unresolved.push(format!(
                                "{}: `{name}` -> could not resolve namespace source `{}`: {e}",
                                path.display(),
                                import.source,
                                name = import.local
                            ));
                        }
                    }
                    continue;
                }
                match parsed.resolve_binding(&path, &import.local, resolver) {
                    SymbolResolution::Resolved(sym) => {
                        let bundled = symbol_lookup
                            .get(&(sym.defining_module.clone(), sym.local_name.clone()))
                            .cloned();
                        if let Some(bundled) = bundled {
                            bundle.modules[module_index]
                                .local_to_bundled
                                .push((import.local.clone(), bundled));
                        } else {
                            bundle.unresolved.push(format!(
                                "{}: `{}` resolved but no bundled symbol",
                                path.display(),
                                import.local
                            ));
                        }
                    }
                    SymbolResolution::Unresolved { name, reason } => {
                        bundle
                            .unresolved
                            .push(format!("{}: `{name}` -> {reason}", path.display()));
                    }
                    SymbolResolution::Circular => {
                        bundle.unresolved.push(format!(
                            "{}: `{}` -> circular import",
                            path.display(),
                            import.local
                        ));
                    }
                }
            }
        }

        bundle
    }

    pub fn symbol_count(&self) -> usize {
        self.symbols.len()
    }

    pub fn is_fully_resolved(&self) -> bool {
        self.unresolved.is_empty()
    }
}

fn host_import_path(source: &str, imported: &str) -> Option<&'static str> {
    match (source, imported) {
        ("w3cos/native", "invoke") => Some("w3cos/native::invoke"),
        _ => None,
    }
}

/// Depth-first topological order: each module appears after its dependencies.
fn topo_order(
    parsed: &ParsedModuleGraph,
    resolver: &EsmResolver,
    entry: &Path,
) -> (Vec<PathBuf>, Option<usize>) {
    let mut visited: HashSet<PathBuf> = HashSet::new();
    let mut order: Vec<PathBuf> = Vec::new();
    visit_module(entry, parsed, resolver, &mut visited, &mut order);
    // Modules reachable only through literal `import()` are evaluated lazily;
    // order each dynamic target's static closure dependencies-first.
    let mut lazy: Vec<PathBuf> = Vec::new();
    for edge in resolver.dynamic_imports() {
        visit_module(&edge.target, parsed, resolver, &mut visited, &mut lazy);
    }
    // Include any modules not reachable from entry (defensive), preserving graph order.
    for module in &parsed.modules {
        if !visited.contains(&module.path) {
            visited.insert(module.path.clone());
            order.push(module.path.clone());
        }
    }
    let lazy_start = (!lazy.is_empty()).then_some(order.len());
    order.extend(lazy);
    (order, lazy_start)
}

fn visit_module(
    path: &Path,
    parsed: &ParsedModuleGraph,
    resolver: &EsmResolver,
    visited: &mut HashSet<PathBuf>,
    order: &mut Vec<PathBuf>,
) {
    if !visited.insert(path.to_path_buf()) {
        return;
    }
    if let Some(info) = parsed.find_module(path) {
        let from_dir = path.parent().unwrap_or_else(|| Path::new("."));
        let mut sources = info.dependency_sources.clone();
        for export in &info.exports {
            if let Some(source) = &export.source {
                sources.push(source.clone());
            }
        }
        for source in sources {
            if is_asset_import(&source) || resolver.is_runtime_module(&source) {
                continue;
            }
            if let Ok(resolved) = resolver.resolve(&source, from_dir) {
                visit_module(&resolved.path, parsed, resolver, visited, order);
            }
        }
    }
    order.push(path.to_path_buf());
}

/// Extensions probed (by appending) for extensionless paths, in order.
const SOURCE_EXTENSIONS: [&str; 7] = ["ts", "tsx", "js", "mjs", "cjs", "jsx", "json"];

#[derive(Debug, Clone)]
pub struct EsmResolver {
    project_root: PathBuf,
    runtime_modules: HashSet<String>,
    /// Memoizes `resolve(specifier, from_dir)` — Monaco-scale graphs re-resolve
    /// the same specifier thousands of times through re-export chains, and each
    /// uncached call costs filesystem syscalls.
    resolve_cache: RefCell<HashMap<(String, PathBuf, ResolveKind), ResolvedModule>>,
    /// Parsed `package.json` per directory (`None`: absent or invalid).
    package_cache: RefCell<HashMap<PathBuf, Option<Rc<PackageJson>>>>,
    /// CommonJS classification per file (`None`: ESM). See [`crate::cjs`].
    commonjs: RefCell<HashMap<PathBuf, Option<Rc<CjsAnalysis>>>>,
    /// Named imports ESM importers request from CommonJS facades.
    commonjs_requested: RefCell<HashMap<PathBuf, BTreeSet<String>>>,
    /// Literal `require()`s that did not resolve (they throw when called).
    commonjs_diagnostics: RefCell<Vec<String>>,
    /// ES modules served as `createRequire`-rewritten text: still ES
    /// modules for facade-name collection.
    create_require_rewritten: RefCell<HashSet<PathBuf>>,
    /// Literal `import("...")` edges found while building the graph, in
    /// discovery order. Their targets are linked as lazily evaluated modules.
    dynamic_imports: RefCell<Vec<DynamicImportEdge>>,
    /// Electron renderer-side scripts (preloads, `<script src>` files): classic
    /// scripts loaded on demand, so marker-free files become CommonJS records
    /// too (see [`crate::cjs::classify_as`]).
    classic_scripts: RefCell<HashSet<PathBuf>>,
    /// The graph entry: its CommonJS record links every worker module (see
    /// [`crate::cjs::worker_boot_prelude`]).
    entry: RefCell<Option<PathBuf>>,
    /// Statically named worker modules found so far, in discovery order.
    worker_entries: RefCell<Vec<PathBuf>>,
}

impl EsmResolver {
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
            runtime_modules: HashSet::new(),
            resolve_cache: RefCell::new(HashMap::new()),
            package_cache: RefCell::new(HashMap::new()),
            commonjs: RefCell::new(HashMap::new()),
            commonjs_requested: RefCell::new(HashMap::new()),
            commonjs_diagnostics: RefCell::new(Vec::new()),
            create_require_rewritten: RefCell::new(HashSet::new()),
            dynamic_imports: RefCell::new(Vec::new()),
            entry: RefCell::new(None),
            worker_entries: RefCell::new(Vec::new()),
            classic_scripts: RefCell::new(HashSet::new()),
        }
    }

    pub fn with_runtime_modules(mut self, specifiers: impl IntoIterator<Item = String>) -> Self {
        self.runtime_modules.extend(specifiers);
        self
    }

    pub fn is_runtime_module(&self, specifier: &str) -> bool {
        self.runtime_modules.contains(specifier)
    }

    pub fn resolve_entry(&self, entry: &Path) -> Result<ResolvedModule> {
        let path = self.resolve_path_like(entry)?;
        Ok(ResolvedModule {
            specifier: path.to_string_lossy().to_string(),
            path,
            kind: ModuleKind::Entry,
            package_name: None,
        })
    }

    pub fn resolve(&self, specifier: &str, from_dir: &Path) -> Result<ResolvedModule> {
        self.resolve_with(specifier, from_dir, ResolveKind::Import)
    }

    /// Resolve `specifier` the way Node resolves an `import` or a `require()`
    /// of it from a file in `from_dir`.
    pub fn resolve_with(
        &self,
        specifier: &str,
        from_dir: &Path,
        kind: ResolveKind,
    ) -> Result<ResolvedModule> {
        let from_dir = normalize_path(from_dir);
        let key = (specifier.to_string(), from_dir.clone(), kind);
        if let Some(hit) = self.resolve_cache.borrow().get(&key) {
            return Ok(hit.clone());
        }
        let resolved = self.resolve_uncached(specifier, &from_dir, kind)?;
        self.resolve_cache
            .borrow_mut()
            .insert(key, resolved.clone());
        Ok(resolved)
    }

    fn resolve_uncached(
        &self,
        specifier: &str,
        from_dir: &Path,
        kind: ResolveKind,
    ) -> Result<ResolvedModule> {
        if specifier.starts_with("./")
            || specifier.starts_with("../")
            || specifier.starts_with('/')
            || specifier == "."
            || specifier == ".."
        {
            let candidate = if specifier.starts_with('/') {
                PathBuf::from(specifier)
            } else {
                from_dir.join(specifier)
            };
            let path = match self.resolve_path_like(&candidate) {
                Ok(path) => self.prefer_transpiled_out(&path),
                Err(error) => self
                    .resolve_src_js_via_out(&candidate)
                    .ok_or(error)
                    .with_context(|| {
                        format!(
                            "Could not resolve relative import `{specifier}` from {}",
                            from_dir.display()
                        )
                    })?,
            };
            return Ok(ResolvedModule {
                specifier: specifier.to_string(),
                path,
                kind: ModuleKind::Relative,
                package_name: None,
            });
        }
        if specifier.starts_with('#') {
            return self.resolve_package_imports(specifier, from_dir, kind);
        }
        if let Some(module) = w3cos_node::native_replacement(specifier) {
            let mut resolved = node_builtin_module(module, &self.project_root)?
                .expect("native replacement glue is a w3cos-node module");
            resolved.specifier = specifier.to_string();
            resolved.package_name = Some(specifier.to_string());
            return Ok(resolved);
        }
        if crate::electron_app::is_electron_specifier(specifier) {
            let (resolved, scripts) = electron_module(specifier, &self.project_root)?;
            self.classic_scripts.borrow_mut().extend(scripts);
            return Ok(resolved);
        }
        if specifier.starts_with("node:") {
            if let Some(resolved) = node_builtin_module(specifier, &self.project_root)? {
                return Ok(resolved);
            }
            return Err(anyhow!(
                "`{specifier}` is a Node builtin module that w3cos-node does not provide yet"
            ));
        }
        // Node: a core module wins over a same-named package (LOAD_CORE_MODULE
        // runs before LOAD_NODE_MODULES). Code-OSS ships the browserify `util`,
        // `events` and `buffer` polyfills in node_modules; resolving
        // `import { promisify } from "util"` to them breaks pfs.realpath.
        if is_node_builtin(specifier) && w3cos_node::is_available(specifier) {
            return Ok(
                node_builtin_module(specifier, &self.project_root)?.expect("available builtin")
            );
        }
        match self.resolve_package(specifier, from_dir, kind) {
            Ok(resolved) => Ok(resolved),
            // Builtins w3cos-node lacks fall back to an installed package.
            Err(error) if is_node_builtin(specifier) => Err(error.context(format!(
                "`{specifier}` is a Node builtin module that w3cos-node does not provide yet"
            ))),
            Err(error) => Err(error),
        }
    }

    pub fn build_graph_from_entry(&self, entry: &Path) -> Result<ModuleGraph> {
        let entry = self.resolve_entry(entry)?;
        *self.entry.borrow_mut() = Some(entry.path.clone());
        self.dynamic_imports.borrow_mut().clear();
        let mut graph = ModuleGraph::default();
        let mut visited = HashSet::new();
        self.visit_module(entry, &mut visited, &mut graph)?;
        Ok(graph)
    }

    pub fn parse_graph_from_entry(&self, entry: &Path) -> Result<ParsedModuleGraph> {
        let graph = self.build_graph_from_entry(entry)?;
        let mut parsed = ParsedModuleGraph::default();
        for node in graph.nodes {
            let source = module_source::read(&node.module.path).with_context(|| {
                format!("Could not read ESM module {}", node.module.path.display())
            })?;
            let module = parse_esm_module(&source, &node.module.path)?;
            parsed
                .modules
                .push(collect_module_info(&node.module.path, &module));
        }
        Ok(parsed)
    }

    fn visit_module(
        &self,
        module: ResolvedModule,
        visited: &mut HashSet<PathBuf>,
        graph: &mut ModuleGraph,
    ) -> Result<()> {
        if !visited.insert(module.path.clone()) {
            return Ok(());
        }
        if self.should_skip_graph_path(&module.path) {
            return Ok(());
        }

        if !self.prepare_commonjs(&module.path)? {
            self.prepare_create_require(&module.path)?;
        }
        let source = module_source::read(&module.path)
            .with_context(|| format!("Could not read ESM module {}", module.path.display()))?;
        let imports = module_source::synthesized_imports(&module.path)
            .unwrap_or_else(|| collect_static_imports(&source));
        let from_dir = module.path.parent().unwrap_or_else(|| Path::new("."));

        graph.nodes.push(ModuleGraphNode {
            module: module.clone(),
            imports: imports.clone(),
        });

        let mut importer_info: Option<ParsedModuleInfo> = None;
        for import in imports {
            // CSS and other non-code assets are part of the asset pipeline, not this JS graph.
            if is_asset_import(&import)
                || is_host_module(&import)
                || self.is_runtime_module(&import)
            {
                continue;
            }
            let resolved = match self.resolve(&import, from_dir) {
                Ok(resolved) => resolved,
                Err(error) => {
                    self.commonjs_diagnostics.borrow_mut().push(format!(
                        "{}: import('{import}') does not resolve: {error:#}",
                        module.path.display()
                    ));
                    continue;
                }
            };
            if (!module_source::is_synthesized(&module.path)
                || self
                    .create_require_rewritten
                    .borrow()
                    .contains(&module.path))
                && self.prepare_commonjs(&resolved.path)?
            {
                // ESM → CommonJS: the facade also exports the names this
                // importer asks for, beyond the lexer's names.
                if importer_info.is_none() {
                    importer_info = parse_esm_module(&source, &module.path)
                        .ok()
                        .map(|parsed| collect_module_info(&module.path, &parsed));
                }
                if let Some(info) = &importer_info {
                    // `import * as fs from "original-fs"`: the facade
                    // snapshots named exports, so request every `fs.member`
                    // the importer reads (members assigned dynamically, like
                    // w3cos-node's `fs[name + 'Sync']`, escape the lexer).
                    let namespace_members: Vec<String> = info
                        .imports
                        .iter()
                        .filter(|binding| binding.source == import && binding.imported == "*")
                        .flat_map(|binding| namespace_member_names(&source, &binding.local))
                        .collect();
                    let imported = info
                        .imports
                        .iter()
                        .filter(|binding| binding.source == import)
                        .map(|binding| binding.imported.clone())
                        .chain(namespace_members);
                    let reexported = info
                        .exports
                        .iter()
                        .filter(|export| export.source.as_deref() == Some(import.as_str()))
                        .map(|export| {
                            export
                                .local
                                .clone()
                                .unwrap_or_else(|| export.exported.clone())
                        });
                    let names: Vec<String> = imported
                        .chain(reexported)
                        .filter(|name| name != "default" && name != "*")
                        .collect();
                    if !names.is_empty() {
                        self.commonjs_requested
                            .borrow_mut()
                            .entry(resolved.path.clone())
                            .or_default()
                            .extend(names);
                        self.write_commonjs_facade(&resolved.path);
                    }
                }
            }
            self.visit_module(resolved, visited, graph)?;
        }

        if !module_source::is_synthesized(&module.path)
            || self
                .create_require_rewritten
                .borrow()
                .contains(&module.path)
            || is_electron_app_record(&module.path)
        {
            for specifier in collect_dynamic_import_literals(&source) {
                self.link_dynamic_import(&module.path, &specifier, from_dir, visited, graph);
            }
            // Code-OSS workbench: `new URL(\`vs/workbench/....js\`, baseUrl)` then
            // `import(workbenchUrl)` — seed the URL's JS path as a lazy module.
            for specifier in collect_new_url_js_literals(&source) {
                self.link_dynamic_import(&module.path, &specifier, from_dir, visited, graph);
            }
        }

        Ok(())
    }

    /// Link the target of a literal `import("specifier")` as a lazily
    /// evaluated module. A target that fails to resolve or link stays out of
    /// the graph (the import rejects at runtime, as in Node when the file is
    /// missing) and is reported through [`Self::commonjs_diagnostics`].
    fn link_dynamic_import(
        &self,
        referrer: &Path,
        specifier: &str,
        from_dir: &Path,
        visited: &mut HashSet<PathBuf>,
        graph: &mut ModuleGraph,
    ) {
        if is_asset_import(specifier)
            || is_host_module(specifier)
            || self.is_runtime_module(specifier)
        {
            return;
        }
        let resolved = match self.resolve(specifier, from_dir) {
            Ok(resolved) => resolved,
            Err(error) => match self.resolve_out_rooted_url(specifier, referrer) {
                Some(resolved) => resolved,
                None => {
                    self.commonjs_diagnostics.borrow_mut().push(format!(
                        "{}: import('{specifier}') does not resolve: {error}",
                        referrer.display()
                    ));
                    return;
                }
            },
        };
        let target = resolved.path.clone();
        let nodes_before = graph.nodes.len();
        if let Err(error) = self.visit_module(resolved, visited, graph) {
            for node in graph.nodes.drain(nodes_before..) {
                visited.remove(&node.module.path);
            }
            self.commonjs_diagnostics.borrow_mut().push(format!(
                "{}: import('{specifier}') does not link: {error:#}",
                referrer.display()
            ));
            return;
        }
        self.dynamic_imports.borrow_mut().push(DynamicImportEdge {
            referrer: referrer.to_path_buf(),
            specifier: specifier.to_string(),
            target,
        });
    }

    /// Resolve a path-like specifier (e.g. `vs/workbench/workbench.desktop.main.js`)
    /// against `<project>/out/` when the referrer lives under that tree. Used for
    /// Code-OSS `new URL(path, vscode-file://…/out/)` seeds.
    fn resolve_out_rooted_url(&self, specifier: &str, referrer: &Path) -> Option<ResolvedModule> {
        if specifier.starts_with('.')
            || specifier.starts_with('/')
            || specifier.starts_with("node:")
            || specifier.contains("://")
            || !specifier.contains('/')
        {
            return None;
        }
        let out_root = self.project_root.join("out");
        if !referrer.starts_with(&out_root) {
            return None;
        }
        let candidate = out_root.join(specifier);
        let path = self.resolve_path_like(&candidate).ok()?;
        let path = self.prefer_transpiled_out(&path);
        Some(ResolvedModule {
            specifier: specifier.to_string(),
            path,
            kind: ModuleKind::Relative,
            package_name: None,
        })
    }

    /// Literal dynamic-import edges of the last built graph.
    pub fn dynamic_imports(&self) -> Vec<DynamicImportEdge> {
        self.dynamic_imports.borrow().clone()
    }

    /// Classify `path` and, for a CommonJS file, synthesize its module record
    /// and ESM facade (see [`crate::cjs`]). Returns whether it is CommonJS.
    /// Dependencies are classified recursively; the memo entry is written
    /// before recursing, so `require` cycles terminate.
    pub fn prepare_commonjs(&self, path: &Path) -> Result<bool> {
        if crate::cjs::is_virtual(path) {
            return Ok(false);
        }
        if let Some(hit) = self.commonjs.borrow().get(path) {
            return Ok(hit.is_some());
        }
        // Always classify the file on disk: a previous build in this process
        // may have left synthesized text under the same path.
        let Ok(source) = std::fs::read_to_string(path) else {
            return Ok(false);
        };
        let analysis = crate::cjs::classify_as(
            path,
            &source,
            self.is_in_type_module_scope(path),
            self.classic_scripts.borrow().contains(path),
        )?;
        let Some(analysis) = analysis.map(Rc::new) else {
            self.commonjs.borrow_mut().insert(path.to_path_buf(), None);
            module_source::clear(path);
            return Ok(false);
        };
        self.commonjs
            .borrow_mut()
            .insert(path.to_path_buf(), Some(analysis.clone()));

        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        let mut targets = Vec::new();
        for specifier in &analysis.requires {
            match self.resolve_with(specifier, dir, ResolveKind::Require) {
                Ok(resolved) => {
                    let target = if resolved.path.extension().is_some_and(|ext| ext == "json") {
                        RequireTarget::Json(resolved.path)
                    } else if self.prepare_commonjs(&resolved.path)? {
                        RequireTarget::CommonJs(resolved.path)
                    } else {
                        RequireTarget::Esm(resolved.path)
                    };
                    targets.push((specifier.clone(), target));
                }
                Err(_) if is_node_builtin(specifier) => {
                    targets.push((specifier.clone(), RequireTarget::Builtin(specifier.clone())));
                }
                Err(error) => self.commonjs_diagnostics.borrow_mut().push(format!(
                    "{}: require(`{specifier}`) is unresolved and throws when called: {error:#}",
                    path.display()
                )),
            }
        }
        for specifier in &analysis.worker_entries {
            match self.resolve_with(specifier, dir, ResolveKind::Require) {
                Ok(resolved) => {
                    let known = self.worker_entries.borrow().contains(&resolved.path);
                    if !known {
                        // Node runs a worker file as a script: a marker-free
                        // file is CommonJS too (not an eagerly evaluated ESM
                        // module of the main program).
                        self.classic_scripts
                            .borrow_mut()
                            .insert(resolved.path.clone());
                        if self.prepare_commonjs(&resolved.path)? {
                            self.worker_entries.borrow_mut().push(resolved.path.clone());
                        } else {
                            self.commonjs_diagnostics.borrow_mut().push(format!(
                                "{}: worker module `{specifier}` is an ES module; w3cos runs CommonJS worker files only",
                                path.display()
                            ));
                        }
                    }
                }
                Err(error) => self.commonjs_diagnostics.borrow_mut().push(format!(
                    "{}: worker module `{specifier}` is unresolved: {error:#}",
                    path.display()
                )),
            }
        }
        let mut prelude = String::new();
        let is_entry = self.entry.borrow().as_deref() == Some(path);
        if is_entry && !self.worker_entries.borrow().is_empty() {
            // The entry's require closure has been classified by now, so
            // every CommonJS-reachable worker module is known.
            let workers = self.worker_entries.borrow().clone();
            if let Ok(boot) =
                self.resolve_with("node:internal/w3cos_worker", dir, ResolveKind::Require)
            {
                targets.push((
                    "node:internal/w3cos_worker".to_string(),
                    RequireTarget::CommonJs(boot.path.clone()),
                ));
                self.prepare_commonjs(&boot.path)?;
                for worker in workers.iter().filter(|worker| worker.as_path() != path) {
                    let target = RequireTarget::CommonJs(worker.clone());
                    targets.push((
                        format!(
                            "{}{}",
                            crate::cjs::WORKER_TARGET_PREFIX,
                            worker.to_string_lossy()
                        ),
                        target,
                    ));
                }
                prelude = crate::cjs::worker_boot_prelude(path, &workers);
            }
        }
        let runtime = crate::cjs::runtime_path(&self.project_root);
        module_source::set_synthesized(
            runtime.clone(),
            crate::cjs::RUNTIME_SOURCE.to_string(),
            Vec::new(),
        );
        let (record, imports) = crate::cjs::record_source_with_prelude(
            path,
            &source,
            &targets,
            &runtime,
            &analysis.implicit_globals,
            &prelude,
        );
        module_source::set_synthesized(crate::cjs::record_path(path), record, imports);
        self.write_commonjs_facade(path);
        Ok(true)
    }

    /// Serve an ES module that requires through `createRequire` as text
    /// whose literal `require` calls are static imports (see
    /// [`crate::create_require`]).
    fn prepare_create_require(&self, path: &Path) -> Result<()> {
        if crate::cjs::is_virtual(path) || self.create_require_rewritten.borrow().contains(path) {
            return Ok(());
        }
        let Ok(source) = std::fs::read_to_string(path) else {
            return Ok(());
        };
        let calls = crate::create_require::find_calls(path, &source);
        // Node implicit globals an ES module reads (`Buffer`) become static
        // imports of their builtin, as CommonJS modules get via the prelude.
        let implicit_imports = crate::cjs::esm_implicit_global_imports(path, &source);
        if calls.is_empty() {
            if !implicit_imports.is_empty() {
                let text = prepend_after_hashbang(&implicit_imports, &source);
                let imports = collect_static_imports(&text);
                module_source::set_synthesized(path.to_path_buf(), text, imports);
                self.create_require_rewritten
                    .borrow_mut()
                    .insert(path.to_path_buf());
            }
            return Ok(());
        }
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        let mut targets = Vec::with_capacity(calls.len());
        for call in &calls {
            use crate::create_require::Target;
            targets.push(match self.resolve_with(&call.specifier, dir, ResolveKind::Require) {
                Ok(resolved) => {
                    if resolved.path.extension().is_some_and(|ext| ext == "json")
                        || self.prepare_commonjs(&resolved.path)?
                    {
                        Target::Default
                    } else {
                        Target::Namespace
                    }
                }
                Err(error) => {
                    self.commonjs_diagnostics.borrow_mut().push(format!(
                        "{}: require(`{}`) via createRequire is unresolved and throws when called: {error:#}",
                        path.display(),
                        call.specifier
                    ));
                    Target::Missing
                }
            });
        }
        let text = prepend_after_hashbang(
            &implicit_imports,
            &crate::create_require::rewrite(&source, &calls, &targets),
        );
        let imports = collect_static_imports(&text);
        module_source::set_synthesized(path.to_path_buf(), text, imports);
        self.create_require_rewritten
            .borrow_mut()
            .insert(path.to_path_buf());
        Ok(())
    }

    fn write_commonjs_facade(&self, path: &Path) {
        let mut names = BTreeSet::new();
        self.collect_commonjs_export_names(path, &mut names, &mut HashSet::new());
        if let Some(requested) = self.commonjs_requested.borrow().get(path) {
            names.extend(requested.iter().cloned());
        }
        let (facade, imports) = crate::cjs::facade_source(path, &names);
        module_source::set_synthesized(path.to_path_buf(), facade, imports);
    }

    /// Lexer export names of a CommonJS file, following wholesale re-exports
    /// (`module.exports = require(x)`, `__exportStar(require(x), exports)`).
    fn collect_commonjs_export_names(
        &self,
        path: &Path,
        names: &mut BTreeSet<String>,
        visited: &mut HashSet<PathBuf>,
    ) {
        if !visited.insert(path.to_path_buf()) {
            return;
        }
        let Some(Some(analysis)) = self.commonjs.borrow().get(path).cloned() else {
            return;
        };
        names.extend(analysis.export_names.iter().cloned());
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        for specifier in &analysis.reexports {
            if let Ok(resolved) = self.resolve_with(specifier, dir, ResolveKind::Require) {
                self.collect_commonjs_export_names(&resolved.path, names, visited);
            }
        }
    }

    /// Whether `path` was classified as CommonJS by this resolver.
    pub fn is_commonjs(&self, path: &Path) -> bool {
        self.commonjs
            .borrow()
            .get(path)
            .is_some_and(|analysis| analysis.is_some())
    }

    /// Unresolved literal `require()` specifiers (they throw at runtime).
    pub fn commonjs_diagnostics(&self) -> Vec<String> {
        self.commonjs_diagnostics.borrow().clone()
    }

    fn resolve_package(
        &self,
        specifier: &str,
        from_dir: &Path,
        kind: ResolveKind,
    ) -> Result<ResolvedModule> {
        let (package_name, subpath) = split_package_specifier(specifier)?;
        // Self-reference: a module inside a package with `exports` may import
        // that package by its own name.
        let package_dir = match self.find_package_scope(from_dir) {
            Some((scope, package))
                if package.name() == Some(package_name.as_str()) && package.exports.is_some() =>
            {
                scope
            }
            _ => self.find_package_dir(&package_name, from_dir)?,
        };
        let entry = self.resolve_in_package_dir(&package_dir, subpath.as_deref(), kind)?;

        Ok(ResolvedModule {
            specifier: specifier.to_string(),
            path: entry,
            kind: ModuleKind::Package,
            package_name: Some(package_name),
        })
    }

    /// PACKAGE_RESOLVE once the package directory is known: `exports` first,
    /// then (leniently, also when `exports` has no matching key) the legacy
    /// subpath / entry-field lookup. A `null` export target is an error.
    fn resolve_in_package_dir(
        &self,
        package_dir: &Path,
        subpath: Option<&str>,
        kind: ResolveKind,
    ) -> Result<PathBuf> {
        let package = self.package_json(package_dir);
        let export_key = match subpath {
            Some(subpath) => format!("./{subpath}"),
            None => ".".to_string(),
        };
        if let Some(exports) = package
            .as_ref()
            .and_then(|package| package.exports.as_ref())
        {
            match resolve_package_exports(exports, &export_key, kind.conditions()) {
                TargetResolution::Found(target) => {
                    return self
                        .resolve_path_like(&package_dir.join(target.trim_start_matches("./")))
                        .with_context(|| {
                            format!(
                                "Could not resolve package export `{export_key}` -> `{target}` in {}",
                                package_dir.display()
                            )
                        });
                }
                TargetResolution::Blocked => {
                    return Err(anyhow!(
                        "Package subpath `{export_key}` is not exported (null target) by {}",
                        package_dir.display()
                    ));
                }
                TargetResolution::NoMatch => {}
            }
        }
        match subpath {
            Some(subpath) => self.resolve_path_like(&package_dir.join(subpath)),
            None => self.resolve_package_entry(package_dir, package.as_deref(), kind),
        }
    }

    /// `#name` specifiers: PACKAGE_IMPORTS_RESOLVE against the `imports` field
    /// of the nearest package scope.
    fn resolve_package_imports(
        &self,
        specifier: &str,
        from_dir: &Path,
        kind: ResolveKind,
    ) -> Result<ResolvedModule> {
        let (scope, package) = self.find_package_scope(from_dir).ok_or_else(|| {
            anyhow!(
                "Package import `{specifier}` used outside a package scope ({})",
                from_dir.display()
            )
        })?;
        let imports = package.imports.as_ref().ok_or_else(|| {
            anyhow!(
                "Package import `{specifier}`: {} has no `imports` field",
                scope.display()
            )
        })?;
        match resolve_package_imports(imports, specifier, kind.conditions()) {
            TargetResolution::Found(target) if target.starts_with("./") => {
                let path = self
                    .resolve_path_like(&scope.join(target.trim_start_matches("./")))
                    .with_context(|| {
                        format!(
                            "Could not resolve package import `{specifier}` -> `{target}` in {}",
                            scope.display()
                        )
                    })?;
                Ok(ResolvedModule {
                    specifier: specifier.to_string(),
                    path,
                    kind: ModuleKind::Relative,
                    package_name: package.name().map(str::to_string),
                })
            }
            TargetResolution::Found(target) => {
                let mut resolved = self.resolve_with(&target, &scope, kind)?;
                resolved.specifier = specifier.to_string();
                Ok(resolved)
            }
            TargetResolution::Blocked | TargetResolution::NoMatch => Err(anyhow!(
                "Package import `{specifier}` is not defined in {}",
                scope.join("package.json").display()
            )),
        }
    }

    /// Parsed `package.json` of `dir`, cached.
    fn package_json(&self, dir: &Path) -> Option<Rc<PackageJson>> {
        if let Some(hit) = self.package_cache.borrow().get(dir) {
            return hit.clone();
        }
        let parsed = std::fs::read_to_string(dir.join("package.json"))
            .ok()
            .and_then(|source| PackageJson::parse(&source).ok())
            .map(Rc::new);
        self.package_cache
            .borrow_mut()
            .insert(dir.to_path_buf(), parsed.clone());
        parsed
    }

    /// Nearest directory at or above `dir` with a `package.json` (Node's
    /// "package scope"). The search stops at a `node_modules` directory.
    pub fn find_package_scope(&self, dir: &Path) -> Option<(PathBuf, Rc<PackageJson>)> {
        let mut dir = normalize_path(dir);
        loop {
            if dir.file_name().is_some_and(|name| name == "node_modules") {
                return None;
            }
            if let Some(package) = self.package_json(&dir) {
                return Some((dir, package));
            }
            if !dir.pop() {
                return None;
            }
        }
    }

    /// Whether `.js` files next to `path` are ESM by `"type": "module"`.
    pub fn is_in_type_module_scope(&self, path: &Path) -> bool {
        path.parent()
            .and_then(|dir| self.find_package_scope(dir))
            .is_some_and(|(_, package)| package.is_type_module())
    }

    fn find_package_dir(&self, package_name: &str, from_dir: &Path) -> Result<PathBuf> {
        let mut dir = from_dir.to_path_buf();
        loop {
            // `node_modules/node_modules/x` is never searched.
            if !dir.file_name().is_some_and(|name| name == "node_modules") {
                let candidate = dir.join("node_modules").join(package_name);
                if candidate.is_dir() {
                    return Ok(candidate);
                }
            }
            if !dir.pop() {
                break;
            }
        }

        let candidate = self.project_root.join("node_modules").join(package_name);
        if candidate.is_dir() {
            return Ok(candidate);
        }

        Err(anyhow!(
            "Package `{package_name}` not found from {}",
            from_dir.display()
        ))
    }

    /// Package root without a matching `exports` entry. ESM imports prefer the
    /// bundler `module` field and a string `browser` field; `require()` uses
    /// `main` like Node. Both end at `index`.
    fn resolve_package_entry(
        &self,
        package_dir: &Path,
        package: Option<&PackageJson>,
        kind: ResolveKind,
    ) -> Result<PathBuf> {
        let mut fields: Vec<&str> = Vec::new();
        if let Some(package) = package {
            if kind == ResolveKind::Import {
                fields.extend(package.module_field());
                fields.extend(package.browser_string());
            }
            fields.extend(package.main());
        }
        for field in &fields {
            if let Ok(path) =
                self.resolve_path_like(&package_dir.join(field.trim_start_matches("./")))
            {
                return Ok(path);
            }
        }
        self.resolve_index(package_dir).ok_or_else(|| {
            anyhow!(
                "Could not resolve package entry ({}) in {}",
                if fields.is_empty() {
                    "index".to_string()
                } else {
                    fields.join(", ")
                },
                package_dir.display()
            )
        })
    }

    /// LOAD_AS_FILE then LOAD_AS_DIRECTORY, with the extension set widened to
    /// the TypeScript and ESM sources the compiler accepts.
    /// Prefer a TypeScript project's `out/` transpile over its `src/` when
    /// both exist (Code-OSS: `npm run transpile-client` writes `out/vs/...js`
    /// beside `src/vs/...ts`). Keeps Electron-main graphs out of the editor
    /// sources that `build/vite` still imports by path.
    /// `import '../../src/vs/foo.js'` from build tooling: the `.js` file lives
    /// under `out/`, not `src/`.
    /// Build/test tooling and TypeScript sources are not part of the Electron
    /// `out/` runtime graph; following them pulls thousands of dead modules.
    fn should_skip_graph_path(&self, path: &Path) -> bool {
        let Some(entry) = self.entry.borrow().clone() else {
            return false;
        };
        if !entry.starts_with(self.project_root.join("out")) {
            return false;
        }
        let rel = match path.strip_prefix(&self.project_root) {
            Ok(rel) => rel,
            Err(_) => return false,
        };
        let first = rel
            .components()
            .next()
            .and_then(|c| c.as_os_str().to_str())
            .unwrap_or("");
        if matches!(
            first,
            "build" | "test" | "src" | "extensions" | ".eslintplugin" | "scripts"
        ) {
            return true;
        }
        // Unit-test trees live under out/vs/**/test/** (and *.test.js). Following
        // them pulls sinon (~1MB ESM) and OOMs codegen (~13GB).
        if rel
            .components()
            .any(|c| c.as_os_str().to_str() == Some("test"))
        {
            return true;
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.contains(".test.") {
                return true;
            }
        }
        // Belt-and-suspenders: known test-only packages under node_modules.
        let rel_s = rel.to_string_lossy();
        for pkg in [
            "node_modules/sinon/",
            "node_modules/mocha/",
            "node_modules/nyc/",
            "node_modules/@vscode/test-electron/",
            "node_modules/@vscode/test-web/",
        ] {
            if rel_s.contains(pkg) {
                return true;
            }
        }
        false
    }

    fn resolve_src_js_via_out(&self, candidate: &Path) -> Option<PathBuf> {
        let normalized = normalize_path(candidate);
        let src_root = self.project_root.join("src");
        let rel = normalized.strip_prefix(&src_root).ok()?;
        let mut out = self.project_root.join("out").join(rel);
        if out
            .extension()
            .is_none_or(|ext| ext == "ts" || ext == "tsx")
        {
            out.set_extension("js");
        }
        out.is_file().then_some(out)
    }

    fn prefer_transpiled_out(&self, path: &Path) -> PathBuf {
        let Ok(src_root) = path.strip_prefix(self.project_root.join("src")) else {
            return path.to_path_buf();
        };
        let ext = path.extension().and_then(|ext| ext.to_str());
        if !matches!(ext, Some("ts" | "tsx" | "js" | "jsx" | "mts" | "cts")) {
            return path.to_path_buf();
        }
        let mut out = self.project_root.join("out").join(src_root);
        out.set_extension("js");
        if out.is_file() {
            out
        } else {
            path.to_path_buf()
        }
    }

    fn resolve_path_like(&self, candidate: &Path) -> Result<PathBuf> {
        if let Some(path) = self.resolve_file(candidate) {
            return Ok(path);
        }
        if candidate.is_dir() {
            if let Some(main) = self
                .package_json(candidate)
                .as_ref()
                .and_then(|p| p.main().map(str::to_string))
            {
                let main_path = candidate.join(main.trim_start_matches("./"));
                if let Some(path) = self.resolve_file(&main_path).or_else(|| {
                    main_path
                        .is_dir()
                        .then(|| self.resolve_index(&main_path))
                        .flatten()
                }) {
                    return Ok(path);
                }
            }
            if let Some(index) = self.resolve_index(candidate) {
                return Ok(index);
            }
        }

        Err(anyhow!("Could not resolve path {}", candidate.display()))
    }

    fn resolve_file(&self, candidate: &Path) -> Option<PathBuf> {
        if crate::cjs::is_virtual(candidate) && module_source::is_synthesized(candidate) {
            return Some(normalize_path(candidate));
        }
        if candidate.is_file() {
            return Some(normalize_path(candidate));
        }

        // Probe source extensions by *appending* to the file name. This must
        // also run when `candidate` already has an extension: dotted basenames
        // like `editor.api` are common (monaco-editor), and the real file is
        // `editor.api.js` — `Path::extension` would wrongly treat `.api` as
        // the source extension and skip probing.
        for ext in SOURCE_EXTENSIONS {
            let mut appended = candidate.as_os_str().to_owned();
            appended.push(".");
            appended.push(ext);
            let with_ext = PathBuf::from(appended);
            if with_ext.is_file() {
                return Some(normalize_path(&with_ext));
            }
        }
        None
    }

    fn resolve_index(&self, dir: &Path) -> Option<PathBuf> {
        SOURCE_EXTENSIONS
            .iter()
            .map(|ext| dir.join(format!("index.{ext}")))
            .find(|index| index.is_file())
            .map(|index| normalize_path(&index))
    }
}

/// Identifiers read as `local.<name>` in `source` (namespace member reads).
/// Lexical scan: occurrences inside strings or comments only add names the
/// CommonJS facade then exports as `undefined`, which is harmless.
fn namespace_member_names(source: &str, local: &str) -> Vec<String> {
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$';
    let needle = format!("{local}.");
    let mut names = BTreeSet::new();
    let mut from = 0;
    while let Some(offset) = source[from..].find(&needle) {
        let start = from + offset;
        from = start + needle.len();
        let preceded = source[..start]
            .chars()
            .next_back()
            .is_some_and(|c| is_ident(c) || c == '.');
        if preceded {
            continue;
        }
        let name: String = source[from..]
            .chars()
            .take_while(|c| is_ident(*c))
            .collect();
        if !name.is_empty() && !name.starts_with(|c: char| c.is_ascii_digit()) {
            names.insert(name);
        }
    }
    names.into_iter().collect()
}

/// `prefix` + `source`, keeping a leading `#!` line first.
fn prepend_after_hashbang(prefix: &str, source: &str) -> String {
    if prefix.is_empty() {
        return source.to_string();
    }
    if source.starts_with("#!") {
        let split = source.find('\n').map_or(source.len(), |index| index + 1);
        let (hashbang, rest) = source.split_at(split);
        let newline = if hashbang.ends_with('\n') { "" } else { "\n" };
        return format!("{hashbang}{newline}{prefix}{rest}");
    }
    format!("{prefix}{source}")
}

fn is_host_module(specifier: &str) -> bool {
    matches!(specifier, "w3cos/native")
}

/// Lexically normalize a path: fold `.` and `..` components without touching
/// the filesystem. Critical for module-graph dedup — `a/b/../c.js` and
/// `a/c.js` must be the SAME graph node, otherwise Monaco-scale graphs
/// explode (each `../..` spelling chain duplicates the whole subtree).
fn normalize_path(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match out.components().last() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir) => {}
                _ => out.push(".."),
            },
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn split_package_specifier(specifier: &str) -> Result<(String, Option<String>)> {
    if specifier.starts_with('@') {
        let mut parts = specifier.splitn(3, '/');
        let scope = parts.next().unwrap_or_default();
        let name = parts
            .next()
            .ok_or_else(|| anyhow!("Invalid scoped package specifier `{specifier}`"))?;
        let package_name = format!("{scope}/{name}");
        let subpath = parts.next().map(|s| s.to_string());
        Ok((package_name, subpath))
    } else {
        let mut parts = specifier.splitn(2, '/');
        let package_name = parts.next().unwrap_or_default().to_string();
        let subpath = parts.next().map(|s| s.to_string());
        Ok((package_name, subpath))
    }
}

/// A literal `import("specifier")` in `referrer` that resolved to `target`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DynamicImportEdge {
    pub referrer: PathBuf,
    pub specifier: String,
    pub target: PathBuf,
}

/// String-literal specifiers of `import("...")` calls, in source order and
/// deduplicated. A light lexer skips comments, strings and template text so
/// `import(` inside them is ignored; computed specifiers are not collected.
pub fn collect_dynamic_import_literals(source: &str) -> Vec<String> {
    let bytes = source.as_bytes();
    let mut found: Vec<String> = Vec::new();
    let mut index = 0;
    let is_ident = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$';
    while index < bytes.len() {
        match bytes[index] {
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/')
                {
                    index += 1;
                }
                index += 2;
            }
            quote @ (b'\'' | b'"' | b'`') => {
                index += 1;
                while index < bytes.len() && bytes[index] != quote {
                    if bytes[index] == b'\\' {
                        index += 1;
                    }
                    index += 1;
                }
                index += 1;
            }
            b'i' if bytes[index..].starts_with(b"import")
                && (index == 0 || !(is_ident(bytes[index - 1]) || bytes[index - 1] == b'.'))
                && !bytes.get(index + 6).copied().is_some_and(is_ident) =>
            {
                let mut cursor = index + 6;
                while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                    cursor += 1;
                }
                index += 6;
                if bytes.get(cursor) != Some(&b'(') {
                    continue;
                }
                cursor += 1;
                while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                    cursor += 1;
                }
                let Some(&quote @ (b'\'' | b'"')) = bytes.get(cursor) else {
                    continue;
                };
                let start = cursor + 1;
                let Some(length) = bytes[start..]
                    .iter()
                    .position(|&byte| byte == quote || byte == b'\\' || byte == b'\n')
                else {
                    continue;
                };
                if bytes[start + length] != quote {
                    continue;
                }
                let mut after = start + length + 1;
                while bytes.get(after).is_some_and(u8::is_ascii_whitespace) {
                    after += 1;
                }
                if !matches!(bytes.get(after), Some(b')') | Some(b',')) {
                    continue;
                }
                let specifier = source[start..start + length].to_string();
                if !found.contains(&specifier) {
                    found.push(specifier);
                }
                index = after;
            }
            _ => index += 1,
        }
    }
    found
}

/// Collect JS module paths from `new URL("….js", base)` / `new URL('….js', base)` /
/// `` new URL(`….js`, base) `` (no `${…}` interpolations). Code-OSS builds the
/// workbench entry as `new URL(\`vs/workbench/workbench.desktop.main.js\`, baseUrl)`
/// then `import(workbenchUrl)`; seeding those paths links the renderer graph.
pub fn collect_new_url_js_literals(source: &str) -> Vec<String> {
    let bytes = source.as_bytes();
    let mut found: Vec<String> = Vec::new();
    let mut index = 0;
    let is_ident = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$';
    while index < bytes.len() {
        match bytes[index] {
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/')
                {
                    index += 1;
                }
                index += 2;
            }
            quote @ (b'\'' | b'"' | b'`') => {
                index += 1;
                while index < bytes.len() && bytes[index] != quote {
                    if bytes[index] == b'\\' {
                        index += 1;
                    }
                    index += 1;
                }
                index += 1;
            }
            b'n' if bytes[index..].starts_with(b"new")
                && (index == 0 || !is_ident(bytes[index - 1]))
                && !bytes.get(index + 3).copied().is_some_and(is_ident) =>
            {
                let mut cursor = index + 3;
                while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                    cursor += 1;
                }
                if !bytes[cursor..].starts_with(b"URL")
                    || bytes.get(cursor + 3).copied().is_some_and(is_ident)
                {
                    index += 1;
                    continue;
                }
                cursor += 3;
                while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                    cursor += 1;
                }
                if bytes.get(cursor) != Some(&b'(') {
                    index += 1;
                    continue;
                }
                cursor += 1;
                while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                    cursor += 1;
                }
                let Some(&quote @ (b'\'' | b'"' | b'`')) = bytes.get(cursor) else {
                    index += 1;
                    continue;
                };
                let start = cursor + 1;
                let end_rel = match bytes[start..].iter().position(|&b| b == quote) {
                    Some(n) => n,
                    None => {
                        index += 1;
                        continue;
                    }
                };
                let lit = &source[start..start + end_rel];
                // Skip template interpolations and non-JS assets (css, etc.).
                if !lit.contains("${")
                    && (lit.ends_with(".js") || lit.ends_with(".mjs") || lit.ends_with(".cjs"))
                    && !found.contains(&lit.to_string())
                {
                    found.push(lit.to_string());
                }
                index = start + end_rel + 1;
            }
            _ => index += 1,
        }
    }
    found
}

pub fn collect_static_imports(source: &str) -> Vec<String> {
    let mut imports = Vec::new();
    let mut pending = String::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if pending.is_empty() {
            let starts_import = trimmed.starts_with("import ");
            let starts_reexport =
                trimmed.starts_with("export {") || trimmed.starts_with("export *");
            if !starts_import && !starts_reexport {
                continue;
            }
            pending.push_str(trimmed);
        } else {
            pending.push(' ');
            pending.push_str(trimmed);
        }

        if pending.ends_with(';')
            || extract_from_clause(&pending).is_some()
            || extract_bare_import(&pending).is_some()
        {
            if let Some(spec) =
                extract_from_clause(&pending).or_else(|| extract_bare_import(&pending))
                && !imports.contains(&spec)
            {
                imports.push(spec);
            }
            pending.clear();
        }
    }
    imports
}

fn extract_from_clause(line: &str) -> Option<String> {
    let from_idx = line.find(" from ")?;
    extract_quoted_specifier(&line[from_idx + " from ".len()..])
}

fn extract_bare_import(line: &str) -> Option<String> {
    if !line.starts_with("import ") {
        return None;
    }
    extract_quoted_specifier(line.trim_start_matches("import ").trim())
}

fn extract_quoted_specifier(input: &str) -> Option<String> {
    let input = input.trim().trim_end_matches(';').trim();
    let mut chars = input.chars();
    let quote = chars.next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let rest = &input[quote.len_utf8()..];
    let end = rest.find(quote)?;
    Some(rest[..end].to_string())
}

/// Whether an import specifier points at a non-code asset (css, images, ...).
/// Asset imports stay in the module graph's import lists but are not recursed
/// into as JS modules; `.css` assets feed the stylesheet pipeline instead.
pub fn is_asset_import(specifier: &str) -> bool {
    matches!(
        Path::new(specifier).extension().and_then(|e| e.to_str()),
        Some("css" | "scss" | "sass" | "less" | "json" | "wasm" | "png" | "jpg" | "jpeg" | "svg")
    )
}

fn parse_esm_module(source: &str, path: &Path) -> Result<Module> {
    let cm: Lrc<SourceMap> = Default::default();
    let fm = cm.new_source_file(
        Lrc::new(FileName::Real(path.to_path_buf())),
        source.to_string(),
    );
    let mut parser = Parser::new(
        Syntax::Typescript(TsSyntax {
            tsx: matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("tsx" | "jsx")
            ),
            ..Default::default()
        }),
        StringInput::from(&*fm),
        None,
    );
    parser
        .parse_module()
        .map_err(|err| anyhow!("ESM parse error in {}: {:?}", path.display(), err))
}

fn collect_module_info(path: &Path, module: &Module) -> ParsedModuleInfo {
    let mut info = ParsedModuleInfo {
        path: path.to_path_buf(),
        ..Default::default()
    };

    for (item_index, item) in module.body.iter().enumerate() {
        match item {
            ModuleItem::ModuleDecl(ModuleDecl::Import(import)) => {
                let source = atom_to_string(&import.src.value);
                if !info.dependency_sources.contains(&source) {
                    info.dependency_sources.push(source.clone());
                }
                for spec in &import.specifiers {
                    match spec {
                        ImportSpecifier::Named(named) => {
                            let imported = named
                                .imported
                                .as_ref()
                                .map(module_export_name)
                                .unwrap_or_else(|| named.local.sym.to_string());
                            info.imports.push(ImportBinding {
                                imported,
                                local: named.local.sym.to_string(),
                                source: source.clone(),
                            });
                        }
                        ImportSpecifier::Default(default) => {
                            info.imports.push(ImportBinding {
                                imported: "default".to_string(),
                                local: default.local.sym.to_string(),
                                source: source.clone(),
                            });
                        }
                        ImportSpecifier::Namespace(ns) => {
                            info.imports.push(ImportBinding {
                                imported: "*".to_string(),
                                local: ns.local.sym.to_string(),
                                source: source.clone(),
                            });
                        }
                    }
                }
            }
            ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => {
                collect_decl_exports(&export.decl, &mut info);
            }
            ModuleItem::ModuleDecl(ModuleDecl::ExportNamed(named)) => {
                let source = named.src.as_ref().map(|src| atom_to_string(&src.value));
                for (specifier_index, spec) in named.specifiers.iter().enumerate() {
                    match spec {
                        ExportSpecifier::Named(named_spec) => {
                            let local = module_export_name(&named_spec.orig);
                            let exported = named_spec
                                .exported
                                .as_ref()
                                .map(module_export_name)
                                .unwrap_or_else(|| local.clone());
                            info.exports.push(ExportBinding {
                                exported,
                                local: Some(local),
                                source: source.clone(),
                            });
                        }
                        ExportSpecifier::Default(default_spec) => {
                            info.exports.push(ExportBinding {
                                exported: "default".to_string(),
                                local: Some(default_spec.exported.sym.to_string()),
                                source: source.clone(),
                            });
                        }
                        ExportSpecifier::Namespace(ns) => {
                            if let Some(source) = &source {
                                let local =
                                    synthetic_namespace_export_name(item_index, specifier_index);
                                if !info.dependency_sources.contains(source) {
                                    info.dependency_sources.push(source.clone());
                                }
                                info.top_level_variables.push(local.clone());
                                info.imports.push(ImportBinding {
                                    imported: "*".to_string(),
                                    local: local.clone(),
                                    source: source.clone(),
                                });
                                info.exports.push(ExportBinding {
                                    exported: module_export_name(&ns.name),
                                    local: Some(local),
                                    source: None,
                                });
                                continue;
                            }
                            info.exports.push(ExportBinding {
                                exported: module_export_name(&ns.name),
                                local: None,
                                source: source.clone(),
                            });
                        }
                    }
                }
            }
            ModuleItem::ModuleDecl(ModuleDecl::ExportAll(export_all)) => {
                // `export * from "./x.js"` — recorded as a star re-export;
                // resolve_export_from forwards lookups through it.
                info.exports.push(ExportBinding {
                    exported: "*".to_string(),
                    local: None,
                    source: Some(atom_to_string(&export_all.src.value)),
                });
            }
            ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultDecl(default_decl)) => {
                match &default_decl.decl {
                    DefaultDecl::Class(class) => {
                        let local = class
                            .ident
                            .as_ref()
                            .map(|ident| ident.sym.to_string())
                            .unwrap_or_else(|| synthetic_default_export_name(item_index));
                        info.top_level_classes.push(local.clone());
                        info.exports.push(ExportBinding {
                            exported: "default".to_string(),
                            local: Some(local),
                            source: None,
                        });
                    }
                    DefaultDecl::Fn(function) => {
                        let local = function
                            .ident
                            .as_ref()
                            .map(|ident| ident.sym.to_string())
                            .unwrap_or_else(|| synthetic_default_export_name(item_index));
                        info.top_level_functions.push(local.clone());
                        info.exports.push(ExportBinding {
                            exported: "default".to_string(),
                            local: Some(local),
                            source: None,
                        });
                    }
                    _ => {
                        info.exports.push(ExportBinding {
                            exported: "default".to_string(),
                            local: None,
                            source: None,
                        });
                    }
                }
            }
            ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultExpr(_)) => {
                // Default expressions are snapshot bindings, including the
                // identifier form (`export default local`).
                let local = synthetic_default_export_name(item_index);
                info.top_level_variables.push(local.clone());
                info.exports.push(ExportBinding {
                    exported: "default".to_string(),
                    local: Some(local),
                    source: None,
                });
            }
            ModuleItem::Stmt(Stmt::Decl(decl)) => collect_top_level_decl(decl, &mut info),
            _ => {}
        }
    }

    info
}

pub(crate) fn synthetic_default_export_name(item_index: usize) -> String {
    format!("__w3cos_default_export_{item_index}")
}

pub(crate) fn synthetic_namespace_export_name(item_index: usize, specifier_index: usize) -> String {
    format!("__w3cos_namespace_export_{item_index}_{specifier_index}")
}

fn collect_decl_exports(decl: &Decl, info: &mut ParsedModuleInfo) {
    match decl {
        Decl::Class(class) if !class.declare => {
            let name = class.ident.sym.to_string();
            info.top_level_classes.push(name.clone());
            info.exports.push(ExportBinding {
                exported: name.clone(),
                local: Some(name),
                source: None,
            });
        }
        Decl::Fn(function) if !function.declare => {
            let name = function.ident.sym.to_string();
            info.top_level_functions.push(name.clone());
            info.exports.push(ExportBinding {
                exported: name.clone(),
                local: Some(name),
                source: None,
            });
        }
        Decl::Var(var) if !var.declare => {
            for decl in &var.decls {
                // Handles plain `const x = ...` AND destructured exports like
                // `export const { getWindowId, ... } = (function(){...})()`
                // (monaco's base/browser/dom.js pattern).
                let mut names = Vec::new();
                collect_pat_names(&decl.name, &mut names);
                for name in names {
                    info.top_level_variables.push(name.clone());
                    info.exports.push(ExportBinding {
                        exported: name.clone(),
                        local: Some(name),
                        source: None,
                    });
                }
            }
        }
        _ => {}
    }
}

/// Recursively collect binding identifiers from a destructuring pattern.
fn collect_pat_names(pat: &Pat, names: &mut Vec<String>) {
    match pat {
        Pat::Ident(binding) => names.push(binding.id.sym.to_string()),
        Pat::Object(object) => {
            for prop in &object.props {
                match prop {
                    ObjectPatProp::KeyValue(kv) => collect_pat_names(&kv.value, names),
                    ObjectPatProp::Assign(assign) => names.push(assign.key.id.sym.to_string()),
                    ObjectPatProp::Rest(rest) => collect_pat_names(&rest.arg, names),
                }
            }
        }
        Pat::Array(array) => {
            for elem in array.elems.iter().flatten() {
                collect_pat_names(elem, names);
            }
        }
        Pat::Assign(assign) => collect_pat_names(&assign.left, names),
        Pat::Rest(rest) => collect_pat_names(&rest.arg, names),
        _ => {}
    }
}

fn collect_top_level_decl(decl: &Decl, info: &mut ParsedModuleInfo) {
    match decl {
        Decl::Class(class) if !class.declare => {
            info.top_level_classes.push(class.ident.sym.to_string())
        }
        Decl::Fn(function) if !function.declare => info
            .top_level_functions
            .push(function.ident.sym.to_string()),
        Decl::Var(var) if !var.declare => {
            for declaration in &var.decls {
                collect_pat_names(&declaration.name, &mut info.top_level_variables);
            }
        }
        _ => {}
    }
}

fn module_export_name(name: &ModuleExportName) -> String {
    match name {
        ModuleExportName::Ident(ident) => ident.sym.to_string(),
        ModuleExportName::Str(s) => atom_to_string(&s.value),
    }
}

fn atom_to_string(atom: &impl std::fmt::Debug) -> String {
    format!("{:?}", atom).trim_matches('"').to_string()
}

/// A `w3cos-node` builtin as an ordinary CommonJS file: the glue is written
/// once per content hash (and project) under the temp dir and linked like
/// package code.
fn node_builtin_module(specifier: &str, project_root: &Path) -> Result<Option<ResolvedModule>> {
    let Some(source) = w3cos_node::source(specifier) else {
        return Ok(None);
    };
    let name = w3cos_node::module_name(specifier);
    let path = node_builtin_dir(project_root)?.join(format!("{}.js", name.replace('/', "__")));
    install_glue_file(&path, source)?;
    Ok(Some(ResolvedModule {
        specifier: specifier.to_string(),
        path,
        kind: ModuleKind::Package,
        package_name: Some(format!("node:{name}")),
    }))
}

/// The CommonJS record of the generated `electron_app.js`: its `load()`
/// switch `import()`s ES module page scripts, which must link lazily.
fn is_electron_app_record(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name == "electron_app.js.w3cos-cjs.js")
}

/// Write `source` at `path` unless it is already there.
fn install_glue_file(path: &Path, source: &str) -> Result<()> {
    if std::fs::read_to_string(path).ok().as_deref() != Some(source) {
        // Unique per writer: parallel resolutions in one process (tests,
        // threaded builds) must not share a temp file.
        static WRITER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let writer = WRITER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let temp = path.with_extension(format!("js.{}.{writer}", std::process::id()));
        std::fs::write(&temp, source)
            .with_context(|| format!("writing glue module {}", temp.display()))?;
        std::fs::rename(&temp, path)
            .with_context(|| format!("installing glue module {}", path.display()))?;
    }
    Ok(())
}

/// `electron` / `@w3cos/electron`: the Electron glue from `w3cos-node`, next
/// to this project's generated `electron_app.js` (pages and renderer-side
/// scripts, see `electron_app.rs`), which the glue requires.
fn electron_module(specifier: &str, project_root: &Path) -> Result<(ResolvedModule, Vec<PathBuf>)> {
    let dir = node_builtin_dir(project_root)?;
    let (source, scripts) = crate::electron_app::app_module(project_root);
    install_glue_file(&dir.join("electron_app.js"), &source)?;
    let path = dir.join("electron.js");
    install_glue_file(&path, w3cos_node::ELECTRON)?;
    let resolved = ResolvedModule {
        specifier: specifier.to_string(),
        path,
        kind: ModuleKind::Package,
        package_name: Some("electron".to_string()),
    };
    Ok((resolved, scripts))
}

fn node_builtin_dir(project_root: &Path) -> Result<PathBuf> {
    // FNV-1a over every builtin, so edits never reuse stale glue.
    let fnv = |bytes: &mut dyn Iterator<Item = u8>| {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in bytes {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        hash
    };
    let glue = fnv(&mut w3cos_node::MODULES
        .iter()
        .flat_map(|(name, source)| name.bytes().chain(source.bytes()))
        .chain(w3cos_node::ELECTRON.bytes()));
    // Per project too: a CommonJS file's synthesized record is keyed by its
    // path process-wide and names the project's loader, so two projects
    // compiled in one process must not share builtin paths.
    let project = fnv(&mut project_root.to_string_lossy().bytes());
    let dir = std::env::temp_dir()
        .join("w3cos-node")
        .join(format!("{glue:016x}"))
        .join(format!("{project:016x}"));
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating Node builtin dir {}", dir.display()))?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn typescript_ambient_declarations_do_not_create_runtime_symbols() {
        let root = fixture_root("w3cos_esm_resolver_ambient_erasure");
        std::fs::create_dir_all(root.join("src")).unwrap();
        let entry = root.join("src/app.ts");
        std::fs::write(
            &entry,
            r#"declare function ambientFunction(value: number): number;
declare class AmbientClass { value: number }
declare const ambientValue: number;
export function main() { return 42; }"#,
        )
        .unwrap();

        let resolver = EsmResolver::new(&root);
        let parsed = resolver.parse_graph_from_entry(&entry).unwrap();
        let bundle = EsmBundle::build(&parsed, &resolver, &entry);
        assert_eq!(bundle.symbol_count(), 1);
        assert_eq!(bundle.symbols[0].original_name, "main");
        assert_eq!(bundle.symbols[0].kind, SymbolKind::Function);

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn namespace_member_names_collects_reads_on_the_local() {
        let source = "import * as fs from 'original-fs';\nconst a = fs.readFileSync(p);\nif (fs.existsSync(d)) x.fs.ignored(); myfs.no(); fs.promises.readFile;";
        assert_eq!(
            namespace_member_names(source, "fs"),
            ["existsSync", "promises", "readFileSync"]
        );
    }

    #[test]
    fn collect_imports_from_esm_source() {
        let imports = collect_static_imports(
            r#"
import { EditorView } from "@codemirror/view";
import "./theme.css";
export { EditorState } from '@codemirror/state';
import {
  Metric,
  DataTable,
} from './components/DisplayComponents';
"#,
        );
        assert_eq!(
            imports,
            vec![
                "@codemirror/view",
                "./theme.css",
                "@codemirror/state",
                "./components/DisplayComponents",
            ]
        );
    }

    #[test]
    fn embeds_default_json_import_as_a_literal_binding() {
        let root = fixture_root("w3cos_esm_json_import");
        std::fs::write(root.join("catalog.json"), r#"{"ready":"Ready"}"#).unwrap();
        std::fs::write(
            root.join("app.ts"),
            r#"import catalog from "./catalog.json";
export function message() { return catalog.ready; }"#,
        )
        .unwrap();

        let resolver = EsmResolver::new(&root);
        let parsed = resolver
            .parse_graph_from_entry(&root.join("app.ts"))
            .unwrap();
        let bundle = EsmBundle::build(&parsed, &resolver, &root.join("app.ts"));
        let entry = bundle
            .modules
            .iter()
            .find(|module| module.path.ends_with("app.ts"))
            .unwrap();
        assert_eq!(
            entry.literal_imports,
            vec![("catalog".to_string(), r#"{"ready":"Ready"}"#.to_string())]
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn resolves_native_invoke_as_a_host_import_without_an_npm_package() {
        let root = fixture_root("w3cos_esm_native_host_import");
        let entry = root.join("app.ts");
        std::fs::write(
            &entry,
            r#"import { invoke } from "w3cos/native";
export function main() { return invoke("example", "ping"); }"#,
        )
        .unwrap();

        let resolver = EsmResolver::new(&root);
        let parsed = resolver.parse_graph_from_entry(&entry).unwrap();
        let bundle = EsmBundle::build(&parsed, &resolver, &entry);
        assert!(bundle.is_fully_resolved(), "{:?}", bundle.unresolved);
        assert_eq!(bundle.modules.len(), 1);
        assert_eq!(
            bundle.modules[0].host_imports,
            vec![("invoke".into(), "w3cos/native::invoke".into())]
        );

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn resolves_scoped_package_exports_import() {
        let root = fixture_root("w3cos_esm_resolver_scoped_package");
        let pkg = root.join("node_modules/@codemirror/view");
        std::fs::create_dir_all(pkg.join("dist")).unwrap();
        std::fs::write(
            pkg.join("package.json"),
            r#"{"exports":{".":{"import":"./dist/index.js","default":"./dist/index.cjs"}}}"#,
        )
        .unwrap();
        std::fs::write(pkg.join("dist/index.js"), "export class EditorView {}").unwrap();

        let resolver = EsmResolver::new(&root);
        let resolved = resolver.resolve("@codemirror/view", &root).unwrap();
        assert_eq!(resolved.package_name.as_deref(), Some("@codemirror/view"));
        assert!(
            resolved
                .path
                .ends_with("node_modules/@codemirror/view/dist/index.js")
        );

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn resolves_scoped_package_subpath_export() {
        let root = fixture_root("w3cos_esm_resolver_scoped_package_subpath");
        let pkg = root.join("node_modules/@logidesk/contract");
        std::fs::create_dir_all(pkg.join("src/generated")).unwrap();
        std::fs::write(
            pkg.join("package.json"),
            r#"{"exports":{".":"./src/index.ts","./generated/composer-rules":"./src/generated/composer-rules.ts"}}"#,
        )
        .unwrap();
        std::fs::write(pkg.join("src/index.ts"), "export const root = true;").unwrap();
        std::fs::write(
            pkg.join("src/generated/composer-rules.ts"),
            "export const rules = {};",
        )
        .unwrap();

        let resolver = EsmResolver::new(&root);
        let resolved = resolver
            .resolve("@logidesk/contract/generated/composer-rules", &root)
            .unwrap();
        assert!(resolved.path.ends_with("src/generated/composer-rules.ts"));

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn resolves_package_subpath_with_dotted_basename() {
        // monaco-editor style: `monaco-editor/esm/vs/editor/editor.api` where
        // the real file is `editor.api.js` — `.api` is NOT the source extension.
        let root = fixture_root("w3cos_esm_resolver_dotted_basename");
        let pkg = root.join("node_modules/monaco-editor");
        std::fs::create_dir_all(pkg.join("esm/vs/editor")).unwrap();
        std::fs::write(
            pkg.join("package.json"),
            r#"{"module":"./esm/vs/editor/editor.main.js"}"#,
        )
        .unwrap();
        std::fs::write(
            pkg.join("esm/vs/editor/editor.api.js"),
            "export const api = true;",
        )
        .unwrap();

        let resolver = EsmResolver::new(&root);
        let resolved = resolver
            .resolve("monaco-editor/esm/vs/editor/editor.api", &root)
            .unwrap();
        assert!(
            resolved
                .path
                .ends_with("node_modules/monaco-editor/esm/vs/editor/editor.api.js"),
            "resolved to {}",
            resolved.path.display()
        );

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn normalizes_parent_dir_components() {
        assert_eq!(
            normalize_path(Path::new("a/b/../c.js")),
            PathBuf::from("a/c.js")
        );
        assert_eq!(normalize_path(Path::new("a/./b")), PathBuf::from("a/b"));
        assert_eq!(
            normalize_path(Path::new("../a/../b")),
            PathBuf::from("../b")
        );
        assert_eq!(
            normalize_path(Path::new("a/../../b")),
            PathBuf::from("../b")
        );
        assert_eq!(normalize_path(Path::new("/a/b/..")), PathBuf::from("/a"));
        assert_eq!(
            normalize_path(Path::new("/a/../b")),
            PathBuf::from("/b"),
            "parent of root stays at root"
        );
    }

    #[test]
    fn graph_dedupes_parent_dir_path_spellings() {
        // a/one.js and b/two.js both import ../shared.js — shared must appear
        // exactly once in the graph despite `..` in the resolved path.
        let root = fixture_root("w3cos_esm_resolver_normalize_graph");
        std::fs::create_dir_all(root.join("src/a")).unwrap();
        std::fs::create_dir_all(root.join("src/b")).unwrap();
        std::fs::write(
            root.join("src/app.ts"),
            "import \"./a/one.js\";\nimport \"./b/two.js\";\n",
        )
        .unwrap();
        std::fs::write(
            root.join("src/a/one.js"),
            r#"import { s } from "../shared.js"; export const one = s;"#,
        )
        .unwrap();
        std::fs::write(
            root.join("src/b/two.js"),
            r#"import { s } from "../shared.js"; export const two = s;"#,
        )
        .unwrap();
        std::fs::write(root.join("src/shared.js"), r#"export const s = 1;"#).unwrap();

        let resolver = EsmResolver::new(&root);
        let graph = resolver
            .build_graph_from_entry(&root.join("src/app.ts"))
            .unwrap();
        assert_eq!(graph.nodes.len(), 4, "entry + one + two + shared (deduped)");

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn export_star_forwards_to_source_module() {
        let root = fixture_root("w3cos_esm_resolver_export_star");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/app.ts"),
            r#"import { foo } from "./barrel.js"; foo();"#,
        )
        .unwrap();
        std::fs::write(root.join("src/barrel.js"), r#"export * from "./impl.js";"#).unwrap();
        std::fs::write(
            root.join("src/impl.js"),
            r#"export function foo() {}
export default function hiddenDefault() {}"#,
        )
        .unwrap();

        let resolver = EsmResolver::new(&root);
        let parsed = resolver
            .parse_graph_from_entry(&root.join("src/app.ts"))
            .unwrap();
        let impl_path = normalize_path(&root.join("src/impl.js"));
        match parsed.resolve_binding(&normalize_path(&root.join("src/app.ts")), "foo", &resolver) {
            SymbolResolution::Resolved(sym) => {
                assert_eq!(sym.defining_module, impl_path);
                assert_eq!(sym.local_name, "foo");
                assert_eq!(sym.kind, SymbolKind::Function);
            }
            other => panic!("expected resolution through export *, got {other:?}"),
        }

        // all_exports (namespace objects) also follows the star chain.
        let exports = parsed.all_exports(&normalize_path(&root.join("src/barrel.js")), &resolver);
        assert!(
            exports
                .iter()
                .any(|s| s.exported_name == "foo" && s.defining_module == impl_path)
        );
        assert!(
            exports
                .iter()
                .all(|symbol| symbol.exported_name != "default"),
            "export * must not forward the dependency's default export"
        );

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn collects_destructured_exports() {
        let root = fixture_root("w3cos_esm_resolver_destructured_exports");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/app.ts"),
            "import { getWindowId, registerWindow } from \"./dom.js\";\ngetWindowId(registerWindow);\n",
        )
        .unwrap();
        std::fs::write(
            root.join("src/dom.js"),
            "export const { registerWindow, getWindowId } = (function () { return {}; })();\n",
        )
        .unwrap();

        let resolver = EsmResolver::new(&root);
        let parsed = resolver
            .parse_graph_from_entry(&root.join("src/app.ts"))
            .unwrap();
        for name in ["getWindowId", "registerWindow"] {
            match parsed.resolve_binding(&normalize_path(&root.join("src/app.ts")), name, &resolver)
            {
                SymbolResolution::Resolved(sym) => {
                    assert_eq!(sym.kind, SymbolKind::Variable, "{name} is a variable")
                }
                other => panic!("expected {name} to resolve, got {other:?}"),
            }
        }

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn bare_imports_participate_in_bundle_dependency_order() {
        let root = fixture_root("w3cos_esm_resolver_bare_import_order");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/app.js"),
            "import './register.js';\nexport function main() {}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("src/register.js"),
            "import './foundation.js';\nglobalThis.registered = true;\n",
        )
        .unwrap();
        std::fs::write(root.join("src/foundation.js"), "globalThis.ready = true;\n").unwrap();

        let resolver = EsmResolver::new(&root);
        let entry = normalize_path(&root.join("src/app.js"));
        let parsed = resolver.parse_graph_from_entry(&entry).unwrap();
        let bundle = EsmBundle::build(&parsed, &resolver, &entry);
        let paths: Vec<_> = bundle.modules.iter().map(|module| &module.path).collect();

        assert_eq!(paths[0], &normalize_path(&root.join("src/foundation.js")));
        assert_eq!(paths[1], &normalize_path(&root.join("src/register.js")));
        assert_eq!(paths[2], &entry);

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn builds_codemirror_like_dependency_graph() {
        let root = fixture_root("w3cos_esm_resolver_codemirror_graph");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/app.ts"),
            r#"import { EditorView } from "@codemirror/view";
import { EditorState } from "@codemirror/state";
new EditorView({ state: EditorState.create({ doc: "hi" }), parent: document.body });"#,
        )
        .unwrap();

        let view = root.join("node_modules/@codemirror/view");
        std::fs::create_dir_all(view.join("dist")).unwrap();
        std::fs::write(view.join("package.json"), r#"{"module":"dist/index.js"}"#).unwrap();
        std::fs::write(
            view.join("dist/index.js"),
            r#"import { EditorState } from "@codemirror/state";
import { StyleModule } from "style-mod";
export class EditorView {}"#,
        )
        .unwrap();

        let state = root.join("node_modules/@codemirror/state");
        std::fs::create_dir_all(state.join("dist")).unwrap();
        std::fs::write(state.join("package.json"), r#"{"module":"dist/index.js"}"#).unwrap();
        std::fs::write(state.join("dist/index.js"), "export class EditorState {}").unwrap();

        let style = root.join("node_modules/style-mod");
        std::fs::create_dir_all(&style).unwrap();
        std::fs::write(style.join("package.json"), r#"{"main":"index.js"}"#).unwrap();
        std::fs::write(style.join("index.js"), "export class StyleModule {}").unwrap();

        let resolver = EsmResolver::new(&root);
        let graph = resolver
            .build_graph_from_entry(&root.join("src/app.ts"))
            .unwrap();
        let packages = graph.package_names();

        assert_eq!(graph.nodes.len(), 4, "entry + view + state + style-mod");
        assert!(packages.contains(&"@codemirror/view".to_string()));
        assert!(packages.contains(&"@codemirror/state".to_string()));
        assert!(packages.contains(&"style-mod".to_string()));

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn parses_codemirror_like_modules_with_swc_metadata() {
        let root = fixture_root("w3cos_esm_resolver_codemirror_parse_graph");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/app.ts"),
            r#"import { EditorView } from "@codemirror/view";
import { EditorState } from "@codemirror/state";
export function boot() {
  return new EditorView({ state: EditorState.create({ doc: "hi" }), parent: document.body });
}"#,
        )
        .unwrap();

        let view = root.join("node_modules/@codemirror/view");
        std::fs::create_dir_all(view.join("dist")).unwrap();
        std::fs::write(view.join("package.json"), r#"{"module":"dist/index.js"}"#).unwrap();
        std::fs::write(
            view.join("dist/index.js"),
            r#"import { EditorState } from "@codemirror/state";
export class EditorView {
  constructor(config) { this.state = config.state }
}
export function keymap() {}"#,
        )
        .unwrap();

        let state = root.join("node_modules/@codemirror/state");
        std::fs::create_dir_all(state.join("dist")).unwrap();
        std::fs::write(state.join("package.json"), r#"{"module":"dist/index.js"}"#).unwrap();
        std::fs::write(
            state.join("dist/index.js"),
            r#"export class EditorState {
  static create(config) { return new EditorState(config) }
}"#,
        )
        .unwrap();

        let resolver = EsmResolver::new(&root);
        let parsed = resolver
            .parse_graph_from_entry(&root.join("src/app.ts"))
            .unwrap();
        let exports = parsed.exported_names();

        assert_eq!(parsed.modules.len(), 3, "entry + view + state modules");
        assert_eq!(
            parsed.total_imports(),
            3,
            "entry has 2 imports, view has 1 import"
        );
        assert!(
            exports.contains(&"boot".to_string()),
            "entry export missing: {exports:?}"
        );
        assert!(
            exports.contains(&"EditorView".to_string()),
            "EditorView export missing: {exports:?}"
        );
        assert!(
            exports.contains(&"EditorState".to_string()),
            "EditorState export missing: {exports:?}"
        );
        assert!(
            exports.contains(&"keymap".to_string()),
            "keymap export missing: {exports:?}"
        );

        let view_info = parsed
            .modules
            .iter()
            .find(|module| {
                module
                    .path
                    .ends_with("node_modules/@codemirror/view/dist/index.js")
            })
            .expect("view module should exist");
        assert_eq!(view_info.top_level_classes, vec!["EditorView"]);
        assert_eq!(view_info.top_level_functions, vec!["keymap"]);

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn resolves_cross_module_symbol_bindings() {
        let root = fixture_root("w3cos_esm_resolver_symbol_binding");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/app.ts"),
            r#"import { EditorView } from "@codemirror/view";
import { EditorState } from "@codemirror/state";
export function main() {}"#,
        )
        .unwrap();

        let view = root.join("node_modules/@codemirror/view");
        std::fs::create_dir_all(view.join("dist")).unwrap();
        std::fs::write(view.join("package.json"), r#"{"module":"dist/index.js"}"#).unwrap();
        std::fs::write(
            view.join("dist/index.js"),
            r#"export class EditorView {}
export function keymap() {}"#,
        )
        .unwrap();

        let state = root.join("node_modules/@codemirror/state");
        std::fs::create_dir_all(state.join("dist")).unwrap();
        std::fs::write(state.join("package.json"), r#"{"module":"dist/index.js"}"#).unwrap();
        std::fs::write(state.join("dist/index.js"), "export class EditorState {}").unwrap();

        let resolver = EsmResolver::new(&root);
        let parsed = resolver
            .parse_graph_from_entry(&root.join("src/app.ts"))
            .unwrap();

        let editor_view = parsed.resolve_binding(&root.join("src/app.ts"), "EditorView", &resolver);
        match &editor_view {
            SymbolResolution::Resolved(sym) => {
                assert_eq!(sym.exported_name, "EditorView");
                assert_eq!(sym.local_name, "EditorView");
                assert_eq!(sym.kind, SymbolKind::Class);
                assert!(
                    sym.defining_module
                        .ends_with("node_modules/@codemirror/view/dist/index.js")
                );
            }
            other => panic!("EditorView should resolve, got: {other:?}"),
        }

        let editor_state =
            parsed.resolve_binding(&root.join("src/app.ts"), "EditorState", &resolver);
        match &editor_state {
            SymbolResolution::Resolved(sym) => {
                assert_eq!(sym.exported_name, "EditorState");
                assert_eq!(sym.kind, SymbolKind::Class);
                assert!(
                    sym.defining_module
                        .ends_with("node_modules/@codemirror/state/dist/index.js")
                );
            }
            other => panic!("EditorState should resolve, got: {other:?}"),
        }

        let unknown = parsed.resolve_binding(&root.join("src/app.ts"), "NonExistent", &resolver);
        assert!(matches!(unknown, SymbolResolution::Unresolved { .. }));

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn resolves_re_export_chain() {
        let root = fixture_root("w3cos_esm_resolver_re_export");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/app.ts"),
            r#"import { Theme } from "my-lib";"#,
        )
        .unwrap();

        let lib = root.join("node_modules/my-lib");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(lib.join("package.json"), r#"{"main":"index.js"}"#).unwrap();
        std::fs::write(lib.join("index.js"), r#"export { Theme } from "./theme";"#).unwrap();
        std::fs::write(lib.join("theme.js"), r#"export class Theme {}"#).unwrap();

        let resolver = EsmResolver::new(&root);
        let parsed = resolver
            .parse_graph_from_entry(&root.join("src/app.ts"))
            .unwrap();

        let theme = parsed.resolve_binding(&root.join("src/app.ts"), "Theme", &resolver);
        match &theme {
            SymbolResolution::Resolved(sym) => {
                assert_eq!(sym.exported_name, "Theme");
                assert_eq!(sym.kind, SymbolKind::Class);
                assert!(
                    sym.defining_module.ends_with("theme.js"),
                    "should resolve through re-export to theme.js, got: {:?}",
                    sym.defining_module
                );
            }
            other => panic!("Theme should resolve through re-export chain, got: {other:?}"),
        }

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn bundles_codemirror_like_graph() {
        let root = fixture_root("w3cos_esm_bundler_codemirror");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/app.ts"),
            r#"import { EditorView } from "@codemirror/view";
import { EditorState } from "@codemirror/state";
export function main() { return new EditorView(EditorState.create({})); }"#,
        )
        .unwrap();

        let view = root.join("node_modules/@codemirror/view");
        std::fs::create_dir_all(view.join("dist")).unwrap();
        std::fs::write(view.join("package.json"), r#"{"module":"dist/index.js"}"#).unwrap();
        std::fs::write(
            view.join("dist/index.js"),
            r#"import { EditorState } from "@codemirror/state";
export class EditorView {}
export function keymap() {}"#,
        )
        .unwrap();

        let state = root.join("node_modules/@codemirror/state");
        std::fs::create_dir_all(state.join("dist")).unwrap();
        std::fs::write(state.join("package.json"), r#"{"module":"dist/index.js"}"#).unwrap();
        std::fs::write(
            state.join("dist/index.js"),
            r#"export class EditorState {}"#,
        )
        .unwrap();

        let resolver = EsmResolver::new(&root);
        let parsed = resolver
            .parse_graph_from_entry(&root.join("src/app.ts"))
            .unwrap();
        let bundle = EsmBundle::build(&parsed, &resolver, &root.join("src/app.ts"));

        // Dependency order: state before view before app (entry last)
        assert_eq!(bundle.modules.len(), 3);
        let state_idx = bundle
            .modules
            .iter()
            .position(|m| m.path.to_string_lossy().contains("@codemirror/state"))
            .unwrap();
        let view_idx = bundle
            .modules
            .iter()
            .position(|m| m.path.to_string_lossy().contains("@codemirror/view"))
            .unwrap();
        let app_idx = bundle
            .modules
            .iter()
            .position(|m| m.path.to_string_lossy().contains("src/app.ts"))
            .unwrap();
        assert!(state_idx < view_idx, "state should come before view");
        assert!(view_idx < app_idx, "view should come before entry");

        // Symbols are lifted with namespace prefixes
        assert!(
            bundle
                .symbols
                .iter()
                .any(|s| s.original_name == "EditorView" && s.kind == SymbolKind::Class)
        );
        assert!(
            bundle
                .symbols
                .iter()
                .any(|s| s.original_name == "EditorState" && s.kind == SymbolKind::Class)
        );
        assert!(
            bundle
                .symbols
                .iter()
                .any(|s| s.original_name == "keymap" && s.kind == SymbolKind::Function)
        );
        assert!(
            bundle
                .symbols
                .iter()
                .any(|s| s.original_name == "main" && s.kind == SymbolKind::Function)
        );

        // Entry module's "EditorView" local should map to the view module's bundled name
        let app_module = &bundle.modules[app_idx];
        let ev_bundled = app_module
            .lookup("EditorView")
            .expect("EditorView should be bound in app");
        assert!(
            ev_bundled.contains("EditorView"),
            "bundled name should contain original: {ev_bundled}"
        );
        let es_bundled = app_module
            .lookup("EditorState")
            .expect("EditorState should be bound in app");
        assert!(
            es_bundled.contains("EditorState"),
            "bundled name should contain original: {es_bundled}"
        );

        // View module's "EditorState" import should also be resolved
        let view_module = &bundle.modules[view_idx];
        let view_es = view_module
            .lookup("EditorState")
            .expect("EditorState should be bound in view");
        assert_eq!(
            view_es, es_bundled,
            "view and app should reference the same bundled EditorState"
        );

        assert!(
            bundle.is_fully_resolved(),
            "all imports should be resolved, unresolved: {:?}",
            bundle.unresolved
        );

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn bundles_re_export_flattened() {
        let root = fixture_root("w3cos_esm_bundler_reexport");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/app.ts"),
            r#"import { Theme } from "my-lib";"#,
        )
        .unwrap();

        let lib = root.join("node_modules/my-lib");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(lib.join("package.json"), r#"{"main":"index.js"}"#).unwrap();
        std::fs::write(lib.join("index.js"), r#"export { Theme } from "./theme";"#).unwrap();
        std::fs::write(lib.join("theme.js"), r#"export class Theme {}"#).unwrap();

        let resolver = EsmResolver::new(&root);
        let parsed = resolver
            .parse_graph_from_entry(&root.join("src/app.ts"))
            .unwrap();
        let bundle = EsmBundle::build(&parsed, &resolver, &root.join("src/app.ts"));

        // Theme should have a unique bundled name, defined in theme.js
        let theme_sym = bundle
            .symbols
            .iter()
            .find(|s| s.original_name == "Theme")
            .unwrap();
        assert!(theme_sym.module.ends_with("theme.js"));

        // app's local "Theme" should map to the same bundled name
        let app_module = bundle
            .modules
            .iter()
            .find(|m| m.path.to_string_lossy().contains("src/app.ts"))
            .unwrap();
        let theme_bundled = app_module.lookup("Theme").expect("Theme should be bound");
        assert_eq!(theme_bundled, &theme_sym.bundled_name);

        assert!(
            bundle.is_fully_resolved(),
            "unresolved: {:?}",
            bundle.unresolved
        );

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn bundles_namespace_import() {
        let root = fixture_root("w3cos_esm_bundler_namespace");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/app.ts"),
            r#"import * as ns from "./nsmod.js";
export function main() { return ns; }"#,
        )
        .unwrap();
        std::fs::write(
            root.join("src/nsmod.js"),
            r#"export class Widget {}
export function make() {}
export const version = "1.0";
export * from "./extra.js";"#,
        )
        .unwrap();
        std::fs::write(root.join("src/extra.js"), r#"export const extra = 1;"#).unwrap();

        let resolver = EsmResolver::new(&root);
        let parsed = resolver
            .parse_graph_from_entry(&root.join("src/app.ts"))
            .unwrap();
        let bundle = EsmBundle::build(&parsed, &resolver, &root.join("src/app.ts"));

        let app_module = bundle
            .modules
            .iter()
            .find(|m| m.path.to_string_lossy().contains("src/app.ts"))
            .unwrap();
        assert_eq!(
            app_module.namespace_imports.len(),
            1,
            "namespace import should be recorded"
        );
        let ns = &app_module.namespace_imports[0];
        assert_eq!(ns.local, "ns");
        assert!(ns.target.ends_with("nsmod.js"));
        // Star re-export chain is followed into extra.js.
        for name in ["Widget", "make", "version", "extra"] {
            assert!(
                ns.exports.iter().any(|s| s.exported_name == name),
                "namespace exports should include {name}: {:?}",
                ns.exports
            );
        }
        assert!(
            bundle.is_fully_resolved(),
            "namespace import must not land in unresolved: {:?}",
            bundle.unresolved
        );

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn manifest_runtime_modules_stay_out_of_static_graph_and_become_live_imports() {
        let root = fixture_root("w3cos_esm_runtime_module");
        std::fs::create_dir_all(root.join("src")).unwrap();
        let entry = root.join("src/app.ts");
        let specifier = "https://cdn.example.test/maps/plugin.js";
        std::fs::write(
            &entry,
            format!(
                r#"import {{ value, bump }} from "{specifier}";
import * as plugin from "{specifier}";
export function main() {{ bump(); return value + plugin.value; }}"#
            ),
        )
        .unwrap();

        let resolver = EsmResolver::new(&root).with_runtime_modules([specifier.to_string()]);
        let graph = resolver.build_graph_from_entry(&entry).unwrap();
        assert_eq!(graph.nodes.len(), 1, "runtime module must not be fetched");
        let parsed = resolver.parse_graph_from_entry(&entry).unwrap();
        let bundle = EsmBundle::build(&parsed, &resolver, &entry);
        assert!(bundle.is_fully_resolved(), "{:?}", bundle.unresolved);
        let app = bundle
            .modules
            .iter()
            .find(|module| module.path == entry)
            .unwrap();
        assert_eq!(app.runtime_dependencies, [specifier]);
        assert_eq!(app.runtime_imports.len(), 3);
        assert!(app.runtime_imports.iter().any(|import| {
            import.local == "value" && import.imported == "value" && import.specifier == specifier
        }));
        assert!(app.runtime_imports.iter().any(|import| {
            import.local == "plugin" && import.imported == "*" && import.specifier == specifier
        }));

        std::fs::remove_dir_all(root).ok();
    }

    fn write_tree(root: &Path, files: &[(&str, &str)]) {
        for (path, contents) in files {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
    }

    #[test]
    fn node_resolution_algorithm_end_to_end() {
        use crate::node_resolution::ResolveKind::{Import, Require};
        let root = fixture_root("w3cos_node_resolution_algorithm");
        write_tree(
            &root,
            &[
                (
                    "package.json",
                    r##"{"name":"app","exports":{".":"./src/main.js","./util":"./src/util.js"},"imports":{"#cfg":{"w3cos":"./src/cfg.w3cos.js","default":"./src/cfg.js"},"#dep":"dual","#lib/*":"./src/lib/*.js","#gone":null}}"##,
                ),
                ("src/main.js", ""),
                ("src/util.js", ""),
                ("src/cfg.js", ""),
                ("src/cfg.w3cos.js", ""),
                ("src/lib/a.js", ""),
                ("src/deep/x/y.js", ""),
                ("src/data.json", "{}"),
                ("src/legacy.cjs", ""),
                ("src/dir/package.json", r#"{"main":"./entry"}"#),
                ("src/dir/entry.js", ""),
                ("src/idx/index.js", ""),
                // Dual package: ordered conditions, nested conditions,
                // patterns, `null` block.
                (
                    "node_modules/dual/package.json",
                    r#"{"name":"dual","main":"./legacy-main.js","module":"./legacy-module.js","exports":{".":{"types":"./i.d.ts","node":{"import":"./esm/node.mjs","require":"./cjs/node.cjs"},"default":"./esm/index.js"},"./features/*":{"require":"./cjs/features/*.cjs","default":"./esm/features/*.js"},"./features/internal/*":null,"./package.json":"./package.json"}}"#,
                ),
                ("node_modules/dual/esm/node.mjs", ""),
                ("node_modules/dual/cjs/node.cjs", ""),
                ("node_modules/dual/esm/features/a.js", ""),
                ("node_modules/dual/cjs/features/a.cjs", ""),
                ("node_modules/dual/esm/features/internal/s.js", ""),
                // Legacy package: no exports.
                (
                    "node_modules/legacy/package.json",
                    r#"{"name":"legacy","main":"lib/main.js","module":"es/index.js"}"#,
                ),
                ("node_modules/legacy/lib/main.js", ""),
                ("node_modules/legacy/es/index.js", ""),
                ("node_modules/legacy/lib/sub.js", ""),
                ("node_modules/noentry/index.js", ""),
                // A nested node_modules shadows the outer one for files
                // below it (walk-up order).
                (
                    "src/deep/node_modules/legacy/package.json",
                    r#"{"main":"near.js"}"#,
                ),
                ("src/deep/node_modules/legacy/near.js", ""),
                (
                    "node_modules/events/package.json",
                    r#"{"main":"events.js"}"#,
                ),
                ("node_modules/events/events.js", ""),
                ("src/deep/node_modules/dgram/index.js", ""),
            ],
        );
        let resolver = EsmResolver::new(&root);
        let src = root.join("src");
        let path = |spec: &str, from: &Path, kind| {
            resolver
                .resolve_with(spec, from, kind)
                .unwrap_or_else(|e| panic!("{spec}: {e:#}"))
                .path
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .to_string()
        };

        assert_eq!(path("dual", &src, Import), "node_modules/dual/esm/node.mjs");
        assert_eq!(
            path("dual", &src, Require),
            "node_modules/dual/cjs/node.cjs"
        );
        assert_eq!(
            path("dual/features/a", &src, Import),
            "node_modules/dual/esm/features/a.js"
        );
        assert_eq!(
            path("dual/features/a", &src, Require),
            "node_modules/dual/cjs/features/a.cjs"
        );
        let blocked = resolver
            .resolve_with("dual/features/internal/s", &src, Import)
            .unwrap_err();
        assert!(
            format!("{blocked:#}").contains("not exported"),
            "{blocked:#}"
        );

        assert_eq!(
            path("legacy", &src, Import),
            "node_modules/legacy/es/index.js"
        );
        assert_eq!(
            path("legacy", &src, Require),
            "node_modules/legacy/lib/main.js"
        );
        assert_eq!(
            path("legacy/lib/sub", &src, Require),
            "node_modules/legacy/lib/sub.js"
        );
        assert_eq!(
            path("noentry", &src, Require),
            "node_modules/noentry/index.js"
        );
        assert_eq!(
            path("legacy", &src.join("deep/x"), Require),
            "src/deep/node_modules/legacy/near.js"
        );

        // Self-reference through the package's own `exports`.
        assert_eq!(path("app", &src.join("deep"), Require), "src/main.js");
        assert_eq!(path("app/util", &src, Import), "src/util.js");

        // `imports`: conditions (w3cos first), bare package target, patterns, null.
        assert_eq!(
            path("#cfg", &src.join("deep/x"), Import),
            "src/cfg.w3cos.js"
        );
        assert_eq!(
            path("#dep", &src, Require),
            "node_modules/dual/cjs/node.cjs"
        );
        assert_eq!(path("#lib/a", &src, Import), "src/lib/a.js");
        assert!(resolver.resolve_with("#gone", &src, Import).is_err());
        assert!(resolver.resolve_with("#missing", &src, Import).is_err());

        // File and directory completion.
        assert_eq!(path("./data", &src, Require), "src/data.json");
        assert_eq!(path("./legacy", &src, Require), "src/legacy.cjs");
        assert_eq!(path("./dir", &src, Require), "src/dir/entry.js");
        assert_eq!(path("./idx", &src, Require), "src/idx/index.js");
        assert_eq!(
            path("../dir", &src.join("idx"), Require),
            "src/dir/entry.js"
        );

        // Builtins: `node:` always; bare names win over an installed
        // package of the same name, as in Node. Builtins w3cos-node lacks
        // fall back to the package, else a coded resolution error.
        let fs = resolver.resolve_with("dgram", &src, Require).unwrap_err();
        assert!(format!("{fs:#}").contains("Node builtin"), "{fs:#}");
        let events = resolver.resolve_with("node:events", &src, Require).unwrap();
        assert_eq!(events.package_name.as_deref(), Some("node:events"));
        assert!(events.path.ends_with("events.js"));
        let bare_path = resolver.resolve_with("path", &src, Require).unwrap();
        assert_eq!(bare_path.package_name.as_deref(), Some("node:path"));
        let bare_events = resolver.resolve_with("events", &src, Require).unwrap();
        assert_eq!(bare_events.package_name.as_deref(), Some("node:events"));
        assert!(!bare_events.path.ends_with("node_modules/events/events.js"));
        let bare_events = resolver.resolve_with("events", &src, Import).unwrap();
        assert_eq!(bare_events.package_name.as_deref(), Some("node:events"));
        assert_eq!(
            path("dgram", &src.join("deep/x"), Require),
            "src/deep/node_modules/dgram/index.js"
        );

        assert!(!resolver.is_in_type_module_scope(&src.join("main.js")));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn collect_new_url_js_literals_picks_js_paths() {
        let source = r#"
const baseUrl = new URL(`${root}/out/`);
const workbenchUrl = new URL(`vs/workbench/workbench.desktop.main.js`, baseUrl).href;
const cssUrl = new URL("skin.css", baseUrl).href;
const other = new URL("./lazy.js", import.meta.url);
const skip = new URL(`vs/${name}.js`, baseUrl);
import("./literal.js");
"#;
        assert_eq!(
            collect_new_url_js_literals(source),
            vec![
                "vs/workbench/workbench.desktop.main.js".to_string(),
                "./lazy.js".to_string(),
            ]
        );
    }

    #[test]
    fn collect_dynamic_import_literals_skips_comments_and_strings() {
        let source = r#"
import { x } from "./static.js";
const a = import("./lazy.js");
const b = import ( './lazy2.js' );
const c = import("./lazy.js"); // dedupe
const s = "import(\"./nope.js\")";
const t = `import("./nope2.js")`;
// import("./comment.js")
/* import("./block.js") */
foo.import("./method.js");
importing("./ident.js");
const d = import(computed);
"#;
        assert_eq!(
            collect_dynamic_import_literals(source),
            vec!["./lazy.js".to_string(), "./lazy2.js".to_string(),]
        );
    }

    #[test]
    fn literal_dynamic_import_is_lazy_in_bundle() {
        let root = std::env::temp_dir().join(format!(
            "w3cos-dyn-import-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("package.json"), r#"{"type":"module"}"#).unwrap();
        std::fs::write(
            root.join("entry.js"),
            "export async function main() { return import('./lazy.js'); }
",
        )
        .unwrap();
        std::fs::write(
            root.join("lazy.js"),
            "export const value = 42;
",
        )
        .unwrap();
        let resolver = EsmResolver::new(&root);
        let parsed = resolver
            .parse_graph_from_entry(&root.join("entry.js"))
            .unwrap();
        let bundle = EsmBundle::build(&parsed, &resolver, &root.join("entry.js"));
        assert_eq!(bundle.modules.len(), 2);
        assert_eq!(bundle.lazy_start, Some(1));
        assert_eq!(bundle.dynamic_imports.len(), 1);
        assert_eq!(bundle.dynamic_imports[0].specifier, "./lazy.js");
        assert_eq!(bundle.dynamic_imports[0].evaluation_order, vec![1]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn export_imported_name_resolves_to_defining_module() {
        let root = std::env::temp_dir().join(format!(
            "w3cos-reexport-import-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("facade")).unwrap();
        std::fs::write(root.join("package.json"), r#"{"type":"module"}"#).unwrap();
        std::fs::write(
            root.join("lifecycle.js"),
            "export class DisposableStore { constructor() { this.n = 1; } }
",
        )
        .unwrap();
        std::fs::write(
            root.join("facade/deps.js"),
            "import { DisposableStore } from '../lifecycle.js';
export { DisposableStore };
",
        )
        .unwrap();
        std::fs::write(
            root.join("entry.js"),
            "import { DisposableStore } from './facade/deps.js';
export function main() { return new DisposableStore().n; }
",
        )
        .unwrap();
        let resolver = EsmResolver::new(&root);
        let parsed = resolver
            .parse_graph_from_entry(&root.join("entry.js"))
            .unwrap();
        let bundle = EsmBundle::build(&parsed, &resolver, &root.join("entry.js"));
        assert!(
            bundle.unresolved.is_empty(),
            "unresolved: {:#?}",
            bundle.unresolved
        );
        let entry = bundle
            .modules
            .iter()
            .find(|m| m.path.ends_with("entry.js"))
            .unwrap();
        assert!(
            entry
                .local_to_bundled
                .iter()
                .any(|(local, _)| local == "DisposableStore"),
            "entry bindings: {:#?}",
            entry.local_to_bundled
        );
        std::fs::remove_dir_all(&root).ok();
    }
}
