//! ESM/npm resolver for the w3cos compile pipeline.
//!
//! This module does not interpret JavaScript. It resolves npm/ESM modules at
//! compile time so the compiler can load real package sources, build a module
//! graph, and then lower the combined SWC AST into w3cos/Rust.

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
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
    pub imports: Vec<ImportBinding>,
    pub exports: Vec<ExportBinding>,
    pub top_level_classes: Vec<String>,
    pub top_level_functions: Vec<String>,
    pub top_level_variables: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ParsedModuleGraph {
    pub modules: Vec<ParsedModuleInfo>,
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
        self.modules.iter().find(|m| m.path == path)
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

            return SymbolResolution::Resolved(ResolvedSymbol {
                defining_module: module_path.to_path_buf(),
                local_name: export
                    .local
                    .clone()
                    .unwrap_or_else(|| export_name.to_string()),
                exported_name: export_name.to_string(),
                kind: if module_info
                    .top_level_classes
                    .contains(&export.local.clone().unwrap_or_default())
                {
                    SymbolKind::Class
                } else if module_info
                    .top_level_functions
                    .contains(&export.local.clone().unwrap_or_default())
                {
                    SymbolKind::Function
                } else {
                    SymbolKind::Variable
                },
            });
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
    /// source module (for example React hooks and JSX runtime entry points).
    pub host_imports: Vec<(String, String)>,
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
}

impl EsmBundle {
    /// Flatten a parsed graph into a single bundle with collision-free names.
    pub fn build(parsed: &ParsedModuleGraph, resolver: &EsmResolver, entry: &Path) -> Self {
        let order = topo_order(parsed, resolver, entry);

        let mut bundle = EsmBundle {
            entry: entry.to_path_buf(),
            ..Default::default()
        };

        // Pass 1: assign namespaces and lift each module's own definitions.
        for (index, path) in order.iter().enumerate() {
            let namespace = format!("m{index}");
            let mut local_to_bundled = Vec::new();

            if let Some(info) = parsed.find_module(path) {
                for name in info
                    .top_level_classes
                    .iter()
                    .chain(info.top_level_functions.iter())
                    .chain(info.top_level_variables.iter())
                {
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

            bundle.modules.push(BundledModule {
                path: path.clone(),
                index,
                namespace,
                local_to_bundled,
                host_imports: Vec::new(),
            });
        }

        // Pass 2: resolve each module's imports to the defining symbol's bundled name.
        for module_index in 0..bundle.modules.len() {
            let path = bundle.modules[module_index].path.clone();
            let info = match parsed.find_module(&path) {
                Some(info) => info.clone(),
                None => continue,
            };

            for import in &info.imports {
                if let Some(host_path) = host_import_path(&import.source, &import.imported) {
                    bundle.modules[module_index]
                        .host_imports
                        .push((import.local.clone(), host_path.to_string()));
                    continue;
                }
                match parsed.resolve_binding(&path, &import.local, resolver) {
                    SymbolResolution::Resolved(sym) => {
                        let bundled = bundle
                            .bundled_name_for(&sym.defining_module, &sym.local_name)
                            .map(str::to_string);
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

    fn bundled_name_for(&self, module: &Path, original: &str) -> Option<&str> {
        self.symbols
            .iter()
            .find(|sym| sym.module == module && sym.original_name == original)
            .map(|sym| sym.bundled_name.as_str())
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
        ("react", "useState") => Some("w3cos_react_compat::aot::useState"),
        ("react", "useMemo") => Some("w3cos_react_compat::aot::useMemo"),
        ("react", "useCallback") => Some("w3cos_react_compat::aot::useCallback"),
        ("react", "useRef") => Some("w3cos_react_compat::aot::useRef"),
        ("react", "useEffect") => Some("w3cos_react_compat::aot::useEffect"),
        ("react", "useLayoutEffect") => Some("w3cos_react_compat::aot::useLayoutEffect"),
        ("react", "useImperativeHandle") => Some("w3cos_react_compat::aot::useImperativeHandle"),
        ("react", "memo") => Some("w3cos_react_compat::aot::memo"),
        ("react", "createElement") => Some("w3cos_react_compat::aot::createElement"),
        ("react/jsx-runtime", "jsx") => Some("w3cos_react_compat::aot::jsx"),
        ("react/jsx-runtime", "jsxs") => Some("w3cos_react_compat::aot::jsxs"),
        ("react/jsx-runtime", "Fragment") => Some("w3cos_react_compat::aot::Fragment"),
        ("w3cos/native", "invoke") => Some("w3cos_core::host::invoke"),
        _ => None,
    }
}

/// Depth-first topological order: each module appears after its dependencies.
fn topo_order(parsed: &ParsedModuleGraph, resolver: &EsmResolver, entry: &Path) -> Vec<PathBuf> {
    let mut visited: HashSet<PathBuf> = HashSet::new();
    let mut order: Vec<PathBuf> = Vec::new();
    visit_module(entry, parsed, resolver, &mut visited, &mut order);
    // Include any modules not reachable from entry (defensive), preserving graph order.
    for module in &parsed.modules {
        if !order.contains(&module.path) {
            order.push(module.path.clone());
        }
    }
    order
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
        let mut sources: Vec<String> = info.imports.iter().map(|i| i.source.clone()).collect();
        for export in &info.exports {
            if let Some(source) = &export.source {
                sources.push(source.clone());
            }
        }
        for source in sources {
            if let Ok(resolved) = resolver.resolve(&source, from_dir) {
                visit_module(&resolved.path, parsed, resolver, visited, order);
            }
        }
    }
    order.push(path.to_path_buf());
}

#[derive(Debug, Deserialize)]
struct PackageJson {
    #[serde(default)]
    main: Option<String>,
    #[serde(default)]
    module: Option<String>,
    #[serde(default)]
    browser: Option<Value>,
    #[serde(default)]
    exports: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct EsmResolver {
    project_root: PathBuf,
}

impl EsmResolver {
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
        }
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
        if specifier.starts_with("./") || specifier.starts_with("../") || specifier.starts_with('/')
        {
            let candidate = if specifier.starts_with('/') {
                PathBuf::from(specifier)
            } else {
                from_dir.join(specifier)
            };
            let path = self.resolve_path_like(&candidate).with_context(|| {
                format!(
                    "Could not resolve relative import `{specifier}` from {}",
                    from_dir.display()
                )
            })?;
            return Ok(ResolvedModule {
                specifier: specifier.to_string(),
                path,
                kind: ModuleKind::Relative,
                package_name: None,
            });
        }

        self.resolve_package(specifier, from_dir)
    }

    pub fn build_graph_from_entry(&self, entry: &Path) -> Result<ModuleGraph> {
        let entry = self.resolve_entry(entry)?;
        let mut graph = ModuleGraph::default();
        let mut visited = HashSet::new();
        self.visit_module(entry, &mut visited, &mut graph)?;
        Ok(graph)
    }

    pub fn parse_graph_from_entry(&self, entry: &Path) -> Result<ParsedModuleGraph> {
        let graph = self.build_graph_from_entry(entry)?;
        let mut parsed = ParsedModuleGraph::default();
        for node in graph.nodes {
            let source = std::fs::read_to_string(&node.module.path).with_context(|| {
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

        let source = std::fs::read_to_string(&module.path)
            .with_context(|| format!("Could not read ESM module {}", module.path.display()))?;
        let imports = collect_static_imports(&source);
        let from_dir = module.path.parent().unwrap_or_else(|| Path::new("."));

        graph.nodes.push(ModuleGraphNode {
            module: module.clone(),
            imports: imports.clone(),
        });

        for import in imports {
            // CSS and other non-code assets are part of the asset pipeline, not this JS graph.
            if is_asset_import(&import) || is_host_module(&import) {
                continue;
            }
            let resolved = self.resolve(&import, from_dir)?;
            self.visit_module(resolved, visited, graph)?;
        }

        Ok(())
    }

    fn resolve_package(&self, specifier: &str, from_dir: &Path) -> Result<ResolvedModule> {
        let (package_name, subpath) = split_package_specifier(specifier)?;
        let package_dir = self.find_package_dir(&package_name, from_dir)?;

        let entry = if let Some(subpath) = subpath {
            self.resolve_path_like(&package_dir.join(subpath))?
        } else {
            self.resolve_package_entry(&package_dir)?
        };

        Ok(ResolvedModule {
            specifier: specifier.to_string(),
            path: entry,
            kind: ModuleKind::Package,
            package_name: Some(package_name),
        })
    }

    fn find_package_dir(&self, package_name: &str, from_dir: &Path) -> Result<PathBuf> {
        let mut dir = from_dir.to_path_buf();
        loop {
            let candidate = dir.join("node_modules").join(package_name);
            if candidate.join("package.json").exists() || candidate.exists() {
                return Ok(candidate);
            }
            if !dir.pop() {
                break;
            }
        }

        let candidate = self.project_root.join("node_modules").join(package_name);
        if candidate.join("package.json").exists() || candidate.exists() {
            return Ok(candidate);
        }

        Err(anyhow!(
            "Package `{package_name}` not found from {}",
            from_dir.display()
        ))
    }

    fn resolve_package_entry(&self, package_dir: &Path) -> Result<PathBuf> {
        let package_json_path = package_dir.join("package.json");
        let package_json = std::fs::read_to_string(&package_json_path)
            .with_context(|| format!("Could not read {}", package_json_path.display()))?;
        let package: PackageJson = serde_json::from_str(&package_json)
            .with_context(|| format!("Invalid package.json at {}", package_json_path.display()))?;

        let entry = package
            .exports
            .as_ref()
            .and_then(resolve_exports_root)
            .or(package.module)
            .or_else(|| resolve_browser_string(package.browser.as_ref()))
            .or(package.main)
            .unwrap_or_else(|| "index.js".to_string());

        let entry = entry.trim_start_matches("./");
        self.resolve_path_like(&package_dir.join(entry))
            .with_context(|| {
                format!(
                    "Could not resolve package entry `{}` in {}",
                    entry,
                    package_dir.display()
                )
            })
    }

    fn resolve_path_like(&self, candidate: &Path) -> Result<PathBuf> {
        if candidate.is_file() {
            return Ok(candidate.to_path_buf());
        }

        if candidate.extension().is_none() {
            for ext in ["ts", "tsx", "js", "mjs", "jsx"] {
                let with_ext = candidate.with_extension(ext);
                if with_ext.is_file() {
                    return Ok(with_ext);
                }
            }
        }

        if candidate.is_dir() {
            for name in [
                "index.ts",
                "index.tsx",
                "index.js",
                "index.mjs",
                "index.jsx",
            ] {
                let index = candidate.join(name);
                if index.is_file() {
                    return Ok(index);
                }
            }
        }

        Err(anyhow!("Could not resolve path {}", candidate.display()))
    }
}

fn is_host_module(specifier: &str) -> bool {
    matches!(specifier, "react" | "react/jsx-runtime" | "w3cos/native")
}

fn resolve_exports_root(exports: &Value) -> Option<String> {
    match exports {
        Value::String(s) => Some(s.clone()),
        Value::Object(map) => {
            if let Some(root) = map.get(".") {
                return resolve_exports_root(root);
            }
            for key in ["import", "module", "browser", "default"] {
                if let Some(value) = map.get(key).and_then(Value::as_str) {
                    return Some(value.to_string());
                }
            }
            None
        }
        _ => None,
    }
}

fn resolve_browser_string(browser: Option<&Value>) -> Option<String> {
    match browser {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    }
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

pub fn collect_static_imports(source: &str) -> Vec<String> {
    let mut imports = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("import ") || trimmed.starts_with("export ") {
            if let Some(spec) =
                extract_from_clause(trimmed).or_else(|| extract_bare_import(trimmed))
            {
                if !imports.contains(&spec) {
                    imports.push(spec);
                }
            }
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

fn is_asset_import(specifier: &str) -> bool {
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

    for item in &module.body {
        match item {
            ModuleItem::ModuleDecl(ModuleDecl::Import(import)) => {
                let source = atom_to_string(&import.src.value);
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
                for spec in &named.specifiers {
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
                            info.exports.push(ExportBinding {
                                exported: module_export_name(&ns.name),
                                local: None,
                                source: source.clone(),
                            });
                        }
                    }
                }
            }
            ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultDecl(default_decl)) => {
                match &default_decl.decl {
                    DefaultDecl::Class(class) => {
                        if let Some(ident) = &class.ident {
                            info.top_level_classes.push(ident.sym.to_string());
                            info.exports.push(ExportBinding {
                                exported: "default".to_string(),
                                local: Some(ident.sym.to_string()),
                                source: None,
                            });
                        } else {
                            info.exports.push(ExportBinding {
                                exported: "default".to_string(),
                                local: None,
                                source: None,
                            });
                        }
                    }
                    DefaultDecl::Fn(function) => {
                        let local = function.ident.as_ref().map(|ident| ident.sym.to_string());
                        if let Some(name) = &local {
                            info.top_level_functions.push(name.clone());
                        }
                        info.exports.push(ExportBinding {
                            exported: "default".to_string(),
                            local,
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
            ModuleItem::Stmt(Stmt::Decl(decl)) => collect_top_level_decl(decl, &mut info),
            _ => {}
        }
    }

    info
}

fn collect_decl_exports(decl: &Decl, info: &mut ParsedModuleInfo) {
    match decl {
        Decl::Class(class) => {
            let name = class.ident.sym.to_string();
            info.top_level_classes.push(name.clone());
            info.exports.push(ExportBinding {
                exported: name.clone(),
                local: Some(name),
                source: None,
            });
        }
        Decl::Fn(function) => {
            let name = function.ident.sym.to_string();
            info.top_level_functions.push(name.clone());
            info.exports.push(ExportBinding {
                exported: name.clone(),
                local: Some(name),
                source: None,
            });
        }
        Decl::Var(var) => {
            for decl in &var.decls {
                if let Pat::Ident(binding) = &decl.name {
                    let name = binding.id.sym.to_string();
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

fn collect_top_level_decl(decl: &Decl, info: &mut ParsedModuleInfo) {
    match decl {
        Decl::Class(class) => info.top_level_classes.push(class.ident.sym.to_string()),
        Decl::Fn(function) => info
            .top_level_functions
            .push(function.ident.sym.to_string()),
        Decl::Var(var) => {
            for declaration in &var.decls {
                if let Pat::Ident(binding) = &declaration.name {
                    info.top_level_variables.push(binding.id.sym.to_string());
                }
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
    fn collect_imports_from_esm_source() {
        let imports = collect_static_imports(
            r#"
import { EditorView } from "@codemirror/view";
import "./theme.css";
export { EditorState } from '@codemirror/state';
"#,
        );
        assert_eq!(
            imports,
            vec!["@codemirror/view", "./theme.css", "@codemirror/state"]
        );
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
            vec![("invoke".into(), "w3cos_core::host::invoke".into())]
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
}
