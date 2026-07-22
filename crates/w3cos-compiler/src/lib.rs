pub mod codegen;
pub mod css_parser;
pub mod css_values;
pub mod esm_codegen;
pub mod esm_css;
pub mod esm_lowering;
pub mod esm_resolver;
pub mod media_query;
pub mod mobile_codegen;
pub mod npm_bridge;
pub mod parser;
pub mod scope_analysis;
pub mod style_matcher;
pub mod ts_transpiler;
pub mod ts_types;
pub mod web_codegen;

use anyhow::{Context, Result};

/// Compile TypeScript/JSON source to Rust code string (no file I/O).
///
/// Auto-detects whether the source is:
/// - **UI DSL** (Column/Row/Text/Button components) → existing UI pipeline
/// - **General TypeScript** (functions, variables, logic) → SWC-based transpiler
pub fn compile_to_rust(ts_source: &str) -> Result<String> {
    if is_ui_dsl(ts_source) {
        let tree = parser::parse(ts_source)?;
        codegen::generate(&tree, &css_parser::Stylesheet::empty())
    } else {
        ts_transpiler::transpile(ts_source)
    }
}

pub use codegen::CompileOptions;

/// Collect entry + CSS import paths for dev file watching.
pub fn collect_watch_paths(source_path: &std::path::Path) -> Result<Vec<std::path::PathBuf>> {
    let ts_source = std::fs::read_to_string(source_path)
        .with_context(|| format!("Could not read {}", source_path.display()))?;
    if !is_ui_dsl(&ts_source) {
        return Ok(vec![source_path.to_path_buf()]);
    }
    let source_dir = source_path.parent();
    let tree = parser::parse(&ts_source)?;
    let mut paths = vec![source_path.to_path_buf()];
    if let Some(dir) = source_dir {
        for import_path in &tree.css_imports {
            paths.push(dir.join(import_path));
        }
    }
    Ok(paths)
}

/// Compile flags that influence Cargo.toml / import generation.
pub struct CompileFlags {
    pub needs_hashmap: bool,
    pub needs_async: bool,
    pub needs_rc: bool,
    pub needs_core: bool,
    pub needs_fetch: bool,
    pub needs_history: bool,
    pub needs_runtime: bool,
    pub needs_dom: bool,
    pub needs_std: bool,
}

/// Compile a TypeScript source file into a standalone Rust project.
///
/// For UI apps: links against w3cos-runtime and produces a native GUI binary.
/// For general TS: produces a standalone CLI binary.
pub fn compile(ts_source: &str, output_dir: &std::path::Path) -> Result<()> {
    compile_with_source_dir(
        ts_source,
        output_dir,
        None,
        None,
        &CompileOptions::default(),
    )
}

/// Compile a TSX UI app into a mobile project directory.
pub fn compile_mobile_from_file(
    source_path: &std::path::Path,
    output_dir: &std::path::Path,
    platform: &str,
    safe_area: bool,
    interactive_widget: &str,
) -> Result<()> {
    compile_mobile_from_file_with_options(
        source_path,
        output_dir,
        platform,
        safe_area,
        interactive_widget,
        &CompileOptions::default(),
    )
}

/// Mobile compile with runtime flags (e.g. Chrome DevTools on device/simulator).
pub fn compile_mobile_from_file_with_options(
    source_path: &std::path::Path,
    output_dir: &std::path::Path,
    platform: &str,
    safe_area: bool,
    interactive_widget: &str,
    options: &CompileOptions,
) -> Result<()> {
    let ts_source = std::fs::read_to_string(source_path)
        .with_context(|| format!("Could not read {}", source_path.display()))?;
    if !is_ui_dsl(&ts_source) {
        let artifacts = build_esm_artifacts(source_path)?;
        let bundle = artifacts.bundle_code.ok_or_else(|| {
            anyhow::anyhow!("React AOT mobile entry did not produce an ESM bundle")
        })?;
        return mobile_codegen::write_mobile_dom_project(
            &bundle,
            output_dir,
            platform,
            safe_area,
            interactive_widget,
            options,
        );
    }
    let source_dir = source_path.parent();
    let tree = parser::parse(&ts_source)?;
    let stylesheet = resolve_css_imports(&tree.css_imports, source_dir)?;
    mobile_codegen::write_mobile_project(
        &tree,
        &stylesheet,
        output_dir,
        platform,
        safe_area,
        interactive_widget,
        options,
    )
}

/// Compile the same TSX UI app to static HTML/CSS/JS for browser preview.
pub fn compile_web_from_file(
    source_path: &std::path::Path,
    output_dir: &std::path::Path,
) -> Result<()> {
    let ts_source = std::fs::read_to_string(source_path)
        .with_context(|| format!("Could not read {}", source_path.display()))?;
    let source_dir = source_path.parent();
    let tree = parser::parse(&ts_source)?;
    let stylesheet = resolve_css_imports(&tree.css_imports, source_dir)?;
    web_codegen::write_web_project(&tree, &stylesheet, output_dir)
}

/// Compile from a source file path, enabling CSS/SCSS import resolution.
pub fn compile_from_file(
    source_path: &std::path::Path,
    output_dir: &std::path::Path,
) -> Result<()> {
    compile_from_file_with_options(source_path, output_dir, &CompileOptions::default())
}

/// Compile from a source file with runtime feature flags (e.g. DevTools).
pub fn compile_from_file_with_options(
    source_path: &std::path::Path,
    output_dir: &std::path::Path,
    options: &CompileOptions,
) -> Result<()> {
    let ts_source = std::fs::read_to_string(source_path)
        .with_context(|| format!("Could not read {}", source_path.display()))?;
    let source_dir = source_path.parent().map(|p| p.to_path_buf());
    compile_with_source_dir(
        &ts_source,
        output_dir,
        source_dir.as_deref(),
        Some(source_path),
        options,
    )
}

fn compile_with_source_dir(
    ts_source: &str,
    output_dir: &std::path::Path,
    source_dir: Option<&std::path::Path>,
    source_path: Option<&std::path::Path>,
    options: &CompileOptions,
) -> Result<()> {
    let is_ui = is_ui_dsl(ts_source);

    std::fs::create_dir_all(output_dir.join("src"))?;

    if is_ui {
        let tree = parser::parse(ts_source)?;
        let stylesheet = resolve_css_imports(&tree.css_imports, source_dir)?;
        let rust_code = codegen::generate(&tree, &stylesheet)?;
        let cargo_toml = codegen::generate_cargo_toml(output_dir, options)?;
        std::fs::write(output_dir.join("Cargo.toml"), cargo_toml)?;
        std::fs::write(output_dir.join("src/main.rs"), rust_code)?;
    } else {
        let output = ts_transpiler::transpile_with_flags(ts_source)?;
        let (esm_diagnostics, esm_bundle_code, _has_entry_main) =
            if let Some(entry_path) = source_path {
                match build_esm_artifacts(entry_path) {
                    Ok(artifacts) => (
                        artifacts.diagnostics,
                        artifacts.bundle_code,
                        artifacts.has_entry_main,
                    ),
                    Err(err) => (
                        format!("//! ESM graph: unresolved ({err})\n\n"),
                        None,
                        false,
                    ),
                }
            } else {
                (String::new(), None, false)
            };
        let has_esm_bundle = esm_bundle_code.is_some();
        let esm_needs_core = esm_bundle_code
            .as_ref()
            .is_some_and(|code| code.contains("w3cos_core::"));
        // Generated ESM bundles reference the jsdom bridge for JS globals
        // (document/window/timers/navigator/...), so they need the runtime.
        let esm_uses_jsdom = esm_bundle_code
            .as_ref()
            .is_some_and(|code| code.contains("w3cos_runtime::jsdom"));
        let flags = CompileFlags {
            needs_hashmap: output.needs_hashmap,
            // The legacy transpiler still scans the source before the ESM
            // pipeline takes ownership. Its async flag must not pull Tokio
            // into an ESM-only artifact whose generated bundle contains no
            // Tokio code.
            needs_async: output.needs_async && !has_esm_bundle,
            needs_rc: output.needs_rc,
            needs_core: output.needs_core || esm_needs_core,
            needs_fetch: output.needs_fetch,
            needs_history: output.needs_history,
            needs_runtime: output.code.contains("use w3cos_runtime")
                || has_esm_bundle
                || esm_uses_jsdom,
            needs_dom: output.code.contains("use w3cos_dom"),
            needs_std: output.code.contains("use w3cos_std") || has_esm_bundle,
        };
        let cargo_toml = generate_standalone_cargo_toml(&flags);
        std::fs::write(output_dir.join("Cargo.toml"), cargo_toml)?;

        let mut main_rs = format!("{esm_diagnostics}");
        if esm_bundle_code.is_some() {
            main_rs.push_str("mod esm_bundle;\n\n");
        }
        if !has_esm_bundle {
            main_rs.push_str(&output.code);
        }
        if has_esm_bundle || !output.code.contains("fn main(") {
            if has_esm_bundle {
                // DOM-mode bundle (no React host imports): run the entry main,
                // which registers baked-in stylesheet rules and builds the DOM.
                // W3COS_DOM_DUMP=1 additionally prints the body's outer HTML
                // (truncated) — the headless smoke signal for DOM apps.
                main_rs.push_str(
                    "\nfn setup_dom() {\n    if w3cos_runtime::dom::get_element_by_id(\"root\").is_none() {\n        let root = w3cos_runtime::dom::create_element(\"div\");\n        w3cos_runtime::dom::set_attribute(root, \"id\", \"root\");\n        w3cos_runtime::dom::append_child(w3cos_runtime::dom::body_id(), root);\n    }\n    esm_bundle::run_entry();\n}\n\nfn main() {\n    if std::env::var_os(\"W3COS_AOT_WINDOW\").is_some() {\n        if let Err(error) = w3cos_runtime::run_app_dom(setup_dom) {\n            eprintln!(\"W3COS_AOT_WINDOW_ERROR {error:#}\");\n        }\n    } else {\n        setup_dom();\n        for _ in 0..16 {\n            w3cos_runtime::jsdom::drain_microtasks();\n            w3cos_runtime::jsdom::tick_timers();\n        }\n        println!(\"W3COS_DOM_OK nodes={}\", w3cos_runtime::dom::node_count());\n        if std::env::var_os(\"W3COS_DOM_DUMP\").is_some() {\n            let html = w3cos_runtime::dom::outer_html(w3cos_runtime::dom::body_id());\n            let truncated: String = html.chars().take(8000).collect();\n            println!(\"W3COS_DOM_DUMP_BEGIN\\n{truncated}\\nW3COS_DOM_DUMP_END\");\n        }\n    }\n}\n",
                );
            } else {
                main_rs.push_str("\nfn main() {}\n");
            }
        }
        std::fs::write(output_dir.join("src/main.rs"), main_rs)?;

        if let Some(bundle_code) = esm_bundle_code {
            std::fs::write(output_dir.join("src/esm_bundle.rs"), bundle_code)?;
        }
    }

    Ok(())
}

struct EsmArtifacts {
    diagnostics: String,
    bundle_code: Option<String>,
    /// True when the bundle exports a `main` function from the entry module,
    /// i.e. the generated `esm_bundle.rs` runs real app code via `run_entry()`.
    has_entry_main: bool,
}

fn build_esm_artifacts(entry_path: &std::path::Path) -> Result<EsmArtifacts> {
    if let Some(prebundled_entry) = prebundle_web_entry(entry_path)? {
        let mut artifacts = build_esm_artifacts(&prebundled_entry)?;
        artifacts.diagnostics.insert_str(
            0,
            "//! Web dependency graph: bundled with vite.config.w3cos.ts\n",
        );
        return Ok(artifacts);
    }

    let project_root = find_project_root(
        entry_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new(".")),
    );
    let resolver = esm_resolver::EsmResolver::new(project_root);
    let graph = resolver.build_graph_from_entry(entry_path)?;
    let parsed = resolver.parse_graph_from_entry(entry_path)?;
    // The manifest may reference an entry through `../` segments. Use the
    // resolver's normalized identity for bundle ordering as well; otherwise
    // the raw spelling is emitted as a phantom entry module before the real
    // normalized module and its imports are left unbound.
    let resolved_entry = resolver.resolve_entry(entry_path)?.path;
    let bundle = esm_resolver::EsmBundle::build(&parsed, &resolver, &resolved_entry);

    // Stylesheet collection: `.css` asset imports stay in the graph's import
    // lists, so this is a walk over already-resolved modules. CSS problems
    // degrade to diagnostics, never build errors.
    let css = esm_css::collect_esm_css(&graph, &resolver);

    let mut diagnostics = String::new();
    diagnostics.push_str("//! ESM graph: resolved at compile time\n");
    diagnostics.push_str(&format!("//! ESM modules: {}\n", graph.nodes.len()));
    diagnostics.push_str(&format!("//! ESM imports: {}\n", parsed.total_imports()));
    diagnostics.push_str(&format!("//! ESM exports: {}\n", parsed.total_exports()));
    let packages = graph.package_names();
    if !packages.is_empty() {
        diagnostics.push_str(&format!("//! ESM packages: {}\n", packages.join(", ")));
    }
    let exports = parsed.exported_names();
    if !exports.is_empty() {
        diagnostics.push_str(&format!("//! ESM exported names: {}\n", exports.join(", ")));
    }
    diagnostics.push_str(&format!(
        "//! ESM bundle symbols: {}\n",
        bundle.symbol_count()
    ));
    diagnostics.push_str(&format!(
        "//! ESM bundle resolved: {}\n",
        if bundle.is_fully_resolved() {
            "yes"
        } else {
            "no"
        }
    ));
    if !bundle.unresolved.is_empty() {
        diagnostics.push_str(&format!(
            "//! ESM unresolved bindings: {}\n",
            bundle.unresolved.len()
        ));
    }
    diagnostics.push_str(&format!("//! ESM css files: {}\n", css.files));
    diagnostics.push_str(&format!("//! ESM css rules: {}\n", css.rules.len()));
    for warning in &css.warnings {
        diagnostics.push_str(&format!("//! WARNING {warning}\n"));
    }
    diagnostics.push('\n');

    // Only generate bundle code if there are symbols to compile.
    let bundle_code = if bundle.symbol_count() > 0 {
        Some(esm_codegen::generate_with_bodies_and_css(
            &bundle, &css.rules,
        ))
    } else {
        None
    };
    let has_entry_main = bundle.symbol_count() > 0
        && bundle
            .symbols
            .iter()
            .any(|s| s.module == bundle.entry && s.original_name == "main");

    Ok(EsmArtifacts {
        diagnostics,
        bundle_code,
        has_entry_main,
    })
}

/// Opt-in web dependency bundling for framework applications. A colocated
/// `vite.config.w3cos.ts` is the contract: Vite resolves npm/CJS packages
/// (including the official React runtime), while native host modules remain
/// external for the W3COS ESM resolver.
fn prebundle_web_entry(entry_path: &std::path::Path) -> Result<Option<std::path::PathBuf>> {
    if !matches!(
        entry_path
            .extension()
            .and_then(|extension| extension.to_str()),
        Some("ts" | "tsx" | "jsx")
    ) {
        return Ok(None);
    }
    let Some(source_dir) = entry_path.parent() else {
        return Ok(None);
    };
    let Some(app_dir) = source_dir.parent() else {
        return Ok(None);
    };
    let config = app_dir.join("vite.config.w3cos.ts");
    if !config.is_file() {
        return Ok(None);
    }
    let vite = app_dir.join("node_modules/.bin/vite");
    if !vite.is_file() {
        anyhow::bail!(
            "{} requires the local Vite dependency; install workspace dependencies first",
            config.display()
        );
    }

    let output_dir = std::env::temp_dir().join(format!(
        "w3cos-vite-{}-{}",
        std::process::id(),
        entry_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("entry")
    ));
    if output_dir.exists() {
        std::fs::remove_dir_all(&output_dir)?;
    }
    let status = std::process::Command::new(&vite)
        .current_dir(app_dir)
        .args(["build", "--config"])
        .arg(&config)
        .args(["--outDir"])
        .arg(&output_dir)
        .arg("--emptyOutDir")
        .status()
        .with_context(|| format!("Could not run {}", vite.display()))?;
    if !status.success() {
        anyhow::bail!("W3COS Vite dependency bundle failed with {status}");
    }

    let assets_dir = output_dir.join("assets");
    let mut javascript = Vec::new();
    let mut stylesheets = Vec::new();
    for entry in std::fs::read_dir(&assets_dir)
        .with_context(|| format!("Could not read {}", assets_dir.display()))?
    {
        let path = entry?.path();
        match path.extension().and_then(|extension| extension.to_str()) {
            Some("js") => javascript.push(path),
            Some("css") => stylesheets.push(path),
            _ => {}
        }
    }
    javascript.sort();
    stylesheets.sort();
    let bundle = javascript
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("Vite emitted no JavaScript bundle"))?;
    let wrapper = output_dir.join("w3cos-entry.js");
    let relative_asset = |path: &std::path::Path| {
        format!(
            "./assets/{}",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
        )
    };
    let mut source = format!("import {:?};\n", relative_asset(&bundle));
    for stylesheet in stylesheets {
        source.push_str(&format!("import {:?};\n", relative_asset(&stylesheet)));
    }
    std::fs::write(&wrapper, source)?;
    Ok(Some(wrapper))
}

fn find_project_root(start: &std::path::Path) -> std::path::PathBuf {
    let mut dir = start.to_path_buf();
    loop {
        if dir.join("package.json").exists() || dir.join("node_modules").exists() {
            return dir;
        }
        if !dir.pop() {
            return start.to_path_buf();
        }
    }
}

fn resolve_css_imports(
    imports: &[String],
    source_dir: Option<&std::path::Path>,
) -> Result<css_parser::Stylesheet> {
    let mut stylesheet = css_parser::Stylesheet::empty();

    if imports.is_empty() {
        return Ok(stylesheet);
    }

    let source_dir = source_dir.ok_or_else(|| {
        anyhow::anyhow!(
            "CSS imports found but source directory is unknown. \
             Use compile_from_file() instead of compile()."
        )
    })?;

    for import_path in imports {
        let full_path = source_dir.join(import_path);
        let css_source = if full_path
            .extension()
            .map_or(false, |e| e == "scss" || e == "sass")
        {
            compile_scss(&full_path)?
        } else if full_path.extension().map_or(false, |e| e == "less") {
            anyhow::bail!(
                "Less is not yet supported. Use CSS or SCSS instead: {}",
                full_path.display()
            );
        } else {
            std::fs::read_to_string(&full_path)
                .with_context(|| format!("Could not read CSS file: {}", full_path.display()))?
        };
        stylesheet.merge(css_parser::parse_css(&css_source));
    }

    Ok(stylesheet)
}

#[cfg(feature = "scss")]
fn compile_scss(path: &std::path::Path) -> Result<String> {
    let source = std::fs::read_to_string(path)
        .with_context(|| format!("Could not read SCSS file: {}", path.display()))?;
    grass::from_string(source, &grass::Options::default())
        .map_err(|e| anyhow::anyhow!("SCSS compilation failed for {}: {e}", path.display()))
}

#[cfg(not(feature = "scss"))]
fn compile_scss(path: &std::path::Path) -> Result<String> {
    anyhow::bail!(
        "SCSS support is not enabled. Rebuild w3cos with the 'scss' feature to use {}: \
         cargo build --features scss",
        path.display()
    )
}

/// Detect whether TS source is a UI DSL (component tree) or general TypeScript.
///
/// Heuristics:
/// - JSON input → UI DSL
/// - Imports from `@w3cos/std` → UI DSL
/// - Top-level expression is Column/Row/Text/Button call → UI DSL
/// - TSX with `<Column>`, `<Row>`, etc. → UI DSL
/// - Everything else → general TypeScript
fn is_ui_dsl(source: &str) -> bool {
    let trimmed = source.trim();

    // JSON format
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return true;
    }

    // Quick scan for W3C OS UI patterns (including React Native compat)
    let has_ui_import = trimmed.contains("@w3cos/std") || trimmed.contains("react-native");
    let has_component_call = [
        "Column(",
        "Row(",
        "Text(",
        "Button(",
        "View(",
        "ScrollView(",
        "TouchableOpacity(",
        "FlatList(",
    ]
    .iter()
    .any(|pat| trimmed.contains(pat));
    let has_tsx_component = [
        "<Column",
        "<Row",
        "<Text",
        "<Button",
        "<View",
        "<ScrollView",
        "<TouchableOpacity",
        "<FlatList",
    ]
    .iter()
    .any(|pat| trimmed.contains(pat));

    // If it imports @w3cos/std or react-native or directly uses component constructors
    if has_ui_import && (has_component_call || has_tsx_component) {
        return true;
    }

    // Check if the main expression (after stripping imports/export default) is a component
    let lines: Vec<&str> = trimmed.lines().collect();
    let body: String = lines
        .iter()
        .filter(|l| {
            let t = l.trim();
            !t.starts_with("import ") && !t.starts_with("export default")
        })
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    let body = body.trim();

    let component_starts = [
        "Column(",
        "Row(",
        "Text(",
        "Button(",
        "Image(",
        "TextInput(",
        "Box(",
        "<Column",
        "<Row",
        "<Text",
        "<Button",
        "<Image",
        "<TextInput",
        "<Box",
        "View(",
        "ScrollView(",
        "TouchableOpacity(",
        "FlatList(",
        "<View",
        "<ScrollView",
        "<TouchableOpacity",
        "<FlatList",
    ];
    let starts_with_component = component_starts.iter().any(|pat| body.starts_with(pat));

    // Standalone component expression (no functions, no variable logic)
    if starts_with_component
        && !body.contains("function ")
        && !body.contains("const ")
        && !body.contains("let ")
    {
        return true;
    }

    false
}

/// Generate a minimal Cargo.toml for standalone (non-UI) Rust programs.
fn generate_standalone_cargo_toml(flags: &CompileFlags) -> String {
    let mut toml = String::from(
        r#"[package]
name = "w3cos-app"
version = "0.1.0"
edition = "2024"

[dependencies]
"#,
    );

    let workspace_root = codegen::find_workspace_root().ok();
    let dependency = |name: &str| {
        workspace_root
            .as_ref()
            .map(|root| {
                format!(
                    "{name} = {{ path = {:?} }}\n",
                    root.join(format!("crates/{name}"))
                )
            })
            .unwrap_or_else(|| format!("{name} = {{ path = \"../../crates/{name}\" }}\n"))
    };

    if flags.needs_core {
        toml.push_str(&dependency("w3cos-core"));
    }
    if flags.needs_async {
        toml.push_str("tokio = { version = \"1\", features = [\"full\"] }\n");
    }
    if flags.needs_fetch || flags.needs_history || flags.needs_runtime {
        toml.push_str(&dependency("w3cos-runtime"));
    }
    if flags.needs_dom {
        toml.push_str(&dependency("w3cos-dom"));
    }
    if flags.needs_std {
        toml.push_str(&dependency("w3cos-std"));
    }
    toml
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_to_rust_simple_text() {
        let rust = compile_to_rust(r##"Text("hello", { style: { color: "#fff" } })"##).unwrap();
        assert!(rust.contains("Component::text(\"hello\""));
        assert!(rust.contains("fn build_ui()"));
        assert!(rust.contains("w3cos_runtime::run_app"));
    }

    #[test]
    fn compile_to_rust_column_with_children() {
        let rust = compile_to_rust(r#"Column({ children: [Text("a"), Text("b")] })"#).unwrap();
        assert!(rust.contains("Component::column"));
        assert!(rust.contains("Component::text(\"a\""));
        assert!(rust.contains("Component::text(\"b\""));
    }

    #[test]
    fn compile_to_rust_repeat_uses_runtime_loop() {
        let rust = compile_to_rust(r#"<Column repeat="1000"><Text>row</Text></Column>"#).unwrap();
        assert!(rust.contains("Vec::with_capacity(__repeat_template.len() * 1000)"));
        assert!(rust.contains("for _ in 0..1000"));
        assert_eq!(rust.matches("Component::text(\"row\"").count(), 1);
    }

    #[test]
    fn compile_to_rust_sticky_counter_binds_signal() {
        let rust =
            compile_to_rust(r#"<Column stickyCounter="todoCount"><Text>task</Text></Column>"#)
                .unwrap();
        assert!(
            rust.contains("__comp.sticky_counter_signal = Some(0)"),
            "generated Rust did not bind sticky counter:\n{rust}"
        );
    }

    #[test]
    fn compile_to_rust_scroll_initial_target() {
        let rust = compile_to_rust(
            r#"<Column style={{ scrollInitialTarget: "nearest" }}><Text>latest</Text></Column>"#,
        )
        .unwrap();
        assert!(rust.contains("scroll_initial_target: ScrollInitialTarget::Nearest"));
    }

    #[test]
    fn compile_to_rust_overscroll_behavior() {
        let rust = compile_to_rust(
            r#"<Column style={{ overscrollBehavior: "none" }}><Text>feed</Text></Column>"#,
        )
        .unwrap();
        assert!(rust.contains("overscroll_behavior: OverscrollBehavior::None"));
    }

    #[test]
    fn compile_to_rust_full_pipeline() {
        let input = r##"Column({
            style: { gap: 8, padding: 16 },
            children: [
                Text("Title", { style: { font_size: 24 } }),
                Button("Submit", { style: { background: "#e94560" } })
            ]
        })"##;
        let rust = compile_to_rust(input).unwrap();
        assert!(rust.contains("Component::column"));
        assert!(rust.contains("Component::text(\"Title\""));
        assert!(rust.contains("Component::button(\"Submit\""));
        assert!(rust.contains("gap: 8_f32"));
        assert!(rust.contains("padding: Edges::all(16_f32)"));
        assert!(rust.contains("Color::from_hex(\"#e94560\")"));
    }

    #[test]
    fn compile_general_ts() {
        let input = r#"
function greet(name: string): string {
    return "Hello, " + name;
}
console.log(greet("W3C OS"));
"#;
        let rust = compile_to_rust(input).unwrap();
        assert!(rust.contains("fn greet("), "got: {rust}");
        assert!(rust.contains("fn main()"), "got: {rust}");
        assert!(rust.contains("println!"), "got: {rust}");
    }

    #[test]
    fn compile_general_ts_showcase() {
        let input = r#"
interface User {
    name: string;
    age: number;
    email?: string;
}

function greet(name: string): string {
    return "Hello, " + name + "!";
}

function fibonacci(n: number): number {
    if (n <= 1) { return n; }
    let a: number = 0;
    let b: number = 1;
    for (let i = 2; i < n; i++) {
        let temp = b;
        b = a + b;
        a = temp;
    }
    return b;
}

let message = greet("W3C OS");
console.log(message);

let numbers: number[] = [1, 2, 3, 4, 5];

let items: number[] = [];
items.push(10);
items.push(20);
console.log("Items:", items);
console.log("Length:", items.length);

for (let i = 0; i < 10; i++) {
    console.log("fib:", fibonacci(i));
}

let score: number = 85;
if (score >= 90) {
    console.log("Grade: A");
} else if (score >= 80) {
    console.log("Grade: B");
} else {
    console.log("Grade: F");
}

let countdown: number = 5;
while (countdown > 0) {
    console.log("Countdown:", countdown);
    countdown -= 1;
}
console.log("Done!");
"#;
        let rust = compile_to_rust(input).unwrap();
        eprintln!("=== Generated Rust ===\n{rust}\n=== End ===");

        // Verify key constructs
        assert!(rust.contains("struct User"), "missing struct: {rust}");
        assert!(rust.contains("fn greet("), "missing greet: {rust}");
        assert!(rust.contains("fn fibonacci("), "missing fibonacci: {rust}");
        assert!(rust.contains("fn main()"), "missing main: {rust}");
        assert!(rust.contains("for i in"), "missing range for: {rust}");
        assert!(rust.contains("while countdown"), "missing while: {rust}");
        assert!(rust.contains("else if"), "missing else if: {rust}");
        assert!(rust.contains(".push("), "missing push: {rust}");
        assert!(rust.contains(".len()"), "missing len: {rust}");
        assert!(rust.contains("println!"), "missing println: {rust}");
    }

    #[test]
    fn standalone_cargo_toml_no_deps() {
        let flags = CompileFlags {
            needs_hashmap: false,
            needs_async: false,
            needs_rc: false,
            needs_core: false,
            needs_fetch: false,
            needs_history: false,
            needs_runtime: false,
            needs_dom: false,
            needs_std: false,
        };
        let toml = generate_standalone_cargo_toml(&flags);
        assert!(!toml.contains("tokio"), "should not include tokio: {toml}");
    }

    #[test]
    fn standalone_cargo_toml_with_tokio() {
        let flags = CompileFlags {
            needs_hashmap: false,
            needs_async: true,
            needs_rc: false,
            needs_core: false,
            needs_fetch: false,
            needs_history: false,
            needs_runtime: false,
            needs_dom: false,
            needs_std: false,
        };
        let toml = generate_standalone_cargo_toml(&flags);
        assert!(toml.contains("tokio"), "should include tokio: {toml}");
        assert!(toml.contains("features"), "should have features: {toml}");
    }

    #[test]
    fn compile_async_to_dir() {
        let input = r#"
            async function fetchData(): Promise<string> {
                return await fetch("http://example.com");
            }
            let data = await fetchData();
            console.log(data);
        "#;

        let dir = std::env::temp_dir().join("w3cos_test_async_compile");
        let _ = std::fs::remove_dir_all(&dir);
        compile(input, &dir).expect("compile failed");

        let main_rs = std::fs::read_to_string(dir.join("src/main.rs")).unwrap();
        assert!(main_rs.contains("async fn"), "missing async: {main_rs}");
        assert!(
            main_rs.contains("#[tokio::main]"),
            "missing tokio::main: {main_rs}"
        );

        let cargo_toml = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();
        assert!(
            cargo_toml.contains("tokio"),
            "missing tokio dep: {cargo_toml}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn compile_closure_to_dir() {
        let input = r#"
            function makeCounter(): () => number {
                let count = 0;
                return () => { count += 1; return count; };
            }
            let c = makeCounter();
            console.log(c());
        "#;

        let dir = std::env::temp_dir().join("w3cos_test_closure_compile");
        let _ = std::fs::remove_dir_all(&dir);
        compile(input, &dir).expect("compile failed");

        let main_rs = std::fs::read_to_string(dir.join("src/main.rs")).unwrap();
        assert!(
            main_rs.contains("use std::rc::Rc;"),
            "missing Rc import: {main_rs}"
        );
        assert!(
            main_rs.contains("use std::cell::RefCell;"),
            "missing RefCell import: {main_rs}"
        );
        assert!(
            main_rs.contains("Rc::new(RefCell::new("),
            "missing Rc wrapping: {main_rs}"
        );

        let cargo_toml = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();
        assert!(
            !cargo_toml.contains("tokio"),
            "should not have tokio: {cargo_toml}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn detect_ui_vs_general() {
        assert!(is_ui_dsl(
            r#"import { Column } from "@w3cos/std"; Column({ children: [] })"#
        ));
        assert!(is_ui_dsl(r#"Column({ children: [Text("hi")] })"#));
        assert!(is_ui_dsl(r#"<Column><Text>hi</Text></Column>"#));
        assert!(!is_ui_dsl(r#"function main() { console.log("hello"); }"#));
        assert!(!is_ui_dsl(r#"let x = 42; console.log(x);"#));
    }

    #[test]
    fn compile_from_file_with_css() {
        let dir = std::env::temp_dir().join("w3cos_test_css_compile");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Write a CSS file
        std::fs::write(
            dir.join("styles.css"),
            ".title { font-size: 32; color: #e94560; font-weight: bold; }\n\
             .container { padding: 20; background: #1a1a2e; gap: 16; }",
        )
        .unwrap();

        // Write the TSX source
        let tsx = r#"import { Column, Text } from "@w3cos/std"
import "./styles.css"

export default
<Column className="container">
  <Text className="title">Hello CSS</Text>
</Column>
"#;
        let tsx_path = dir.join("app.tsx");
        std::fs::write(&tsx_path, tsx).unwrap();

        let build_dir = dir.join("build");
        compile_from_file(&tsx_path, &build_dir).expect("compile_from_file failed");

        let main_rs = std::fs::read_to_string(build_dir.join("src/main.rs")).unwrap();
        assert!(
            main_rs.contains("font_size: 32_f32"),
            "CSS font-size not applied: {main_rs}"
        );
        assert!(
            main_rs.contains("Color::from_hex(\"#e94560\")"),
            "CSS color not applied: {main_rs}"
        );
        assert!(
            main_rs.contains("padding: Edges::all(20_f32)"),
            "CSS padding not applied: {main_rs}"
        );
        assert!(
            main_rs.contains("gap: 16_f32"),
            "CSS gap not applied: {main_rs}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn compile_css_inline_override() {
        let dir = std::env::temp_dir().join("w3cos_test_css_override");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(dir.join("s.css"), ".t { color: red; font-size: 20; }").unwrap();

        let tsx = r##"import { Text } from "@w3cos/std"
import "./s.css"

export default <Text className="t" style={{ color: "#fff" }}>Hi</Text>
"##;
        let tsx_path = dir.join("app.tsx");
        std::fs::write(&tsx_path, tsx).unwrap();

        let build_dir = dir.join("build");
        compile_from_file(&tsx_path, &build_dir).expect("compile failed");

        let main_rs = std::fs::read_to_string(build_dir.join("src/main.rs")).unwrap();
        // Inline #fff should override CSS red
        assert!(
            main_rs.contains("Color::from_hex(\"#fff\")"),
            "inline override failed: {main_rs}"
        );
        // CSS font-size should still apply
        assert!(
            main_rs.contains("font_size: 20_f32"),
            "CSS font-size missing: {main_rs}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    // ── npm bridge integration tests ──────────────────────────────────────

    #[test]
    fn codemirror_import_injects_use_stmts() {
        let ts = r#"
import { EditorView } from "@codemirror/view";
import { EditorState } from "@codemirror/state";

function createEditor(): void {
    console.log("editor created");
}
createEditor();
"#;
        let rust = compile_to_rust(ts).unwrap();
        eprintln!("=== codemirror import output ===\n{rust}\n=== end ===");

        assert!(
            rust.contains("w3cos_dom"),
            "should inject w3cos_dom use: {rust}"
        );
        assert!(
            rust.contains("w3cos_runtime"),
            "should inject w3cos_runtime use: {rust}"
        );
        assert!(
            rust.contains("fn createEditor"),
            "should transpile function: {rust}"
        );
    }

    #[test]
    fn unknown_npm_package_emits_warning_comment() {
        let ts = r#"
import { something } from "some-unknown-package";
let x = 42;
console.log(x);
"#;
        let rust = compile_to_rust(ts).unwrap();
        eprintln!("=== unknown npm output ===\n{rust}\n=== end ===");

        assert!(
            rust.contains("some-unknown-package"),
            "should warn about unknown package: {rust}"
        );
        assert!(
            rust.contains("WARNING"),
            "should emit WARNING comment: {rust}"
        );
    }

    #[test]
    fn relative_import_is_silently_ignored() {
        let ts = r#"
import { helper } from "./utils";
let y = 10;
console.log(y);
"#;
        let rust = compile_to_rust(ts).unwrap();
        // Relative imports should not produce warnings or use stmts
        assert!(
            !rust.contains("WARNING"),
            "relative import should not warn: {rust}"
        );
        assert!(
            rust.contains("fn main"),
            "should still produce main: {rust}"
        );
    }

    #[test]
    fn codemirror_full_bundle_import() {
        let ts = r#"
import { EditorView, ViewPlugin, Decoration } from "@codemirror/view";
import { EditorState, Transaction, Extension } from "@codemirror/state";
import { javascript } from "@codemirror/lang-javascript";

function setupEditor(): void {
    let state = 0;
    console.log("setup", state);
}
setupEditor();
"#;
        let rust = compile_to_rust(ts).unwrap();
        eprintln!("=== full bundle output ===\n{rust}\n=== end ===");

        // @codemirror/view and @codemirror/state are bridged
        assert!(rust.contains("w3cos_dom"), "missing w3cos_dom: {rust}");
        assert!(
            rust.contains("w3cos_runtime"),
            "missing w3cos_runtime: {rust}"
        );

        // @codemirror/lang-javascript has no bridge → warning
        assert!(
            rust.contains("lang-javascript") || rust.contains("WARNING"),
            "unknown lang package should warn: {rust}"
        );

        assert!(rust.contains("fn setupEditor"), "missing fn: {rust}");
    }

    #[test]
    fn style_mod_and_w3c_keyname_bridge() {
        let ts = r#"
import { StyleModule } from "style-mod";
import { keyName } from "w3c-keyname";

function test(): void {
    console.log("ok");
}
test();
"#;
        let rust = compile_to_rust(ts).unwrap();
        eprintln!("=== style-mod + w3c-keyname output ===\n{rust}\n=== end ===");

        assert!(
            rust.contains("w3cos_dom::css_style"),
            "style-mod should map to css_style: {rust}"
        );
        assert!(
            !rust.contains("WARNING"),
            "known packages should not warn: {rust}"
        );
    }

    #[test]
    fn real_codemirror_editorview_integration_currently_requires_esm_compile_pipeline() {
        let ts = r#"
import { EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";

const state = EditorState.create({ doc: "hello w3cos" });
const view = new EditorView({ state, parent: document.body });
console.log("view", view);
"#;

        let rust = compile_to_rust(ts).unwrap();
        eprintln!("=== real CodeMirror EditorView diagnostic output ===\n{rust}\n=== end ===");

        assert!(
            rust.contains("w3cos_dom"),
            "CodeMirror import should expose W3C DOM APIs: {rust}"
        );
        assert!(
            rust.contains("w3cos_runtime"),
            "CodeMirror import should expose W3C runtime APIs: {rust}"
        );
        assert!(
            !rust.contains("w3cos_codemirror"),
            "must not generate non-standard w3cos_codemirror shims: {rust}"
        );
        assert!(
            rust.contains("EditorView::new") || rust.contains("EditorView"),
            "real EditorView should still come from the npm JS package, not a Rust shim: {rust}"
        );

        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/w3cos_real_codemirror_editorview_probe");
        let _ = std::fs::remove_dir_all(&dir);
        compile(ts, &dir).expect("source should transpile to a diagnostic Rust project");
        let cargo_toml_path = dir.join("Cargo.toml");
        let mut cargo_toml = std::fs::read_to_string(&cargo_toml_path).unwrap();
        cargo_toml.push_str("\n[workspace]\n");
        std::fs::write(&cargo_toml_path, cargo_toml).unwrap();

        let output = std::process::Command::new("cargo")
            .arg("check")
            .current_dir(&dir)
            .output()
            .expect("cargo check should run");

        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        eprintln!(
            "=== real CodeMirror cargo check stdout ===\n{stdout}\n=== stderr ===\n{stderr}\n=== end ==="
        );

        assert!(
            !output.status.success(),
            "real CodeMirror should not be reported runnable until the ESM compile pipeline lowers npm modules"
        );
        assert!(
            stderr.contains("EditorView")
                || stderr.contains("EditorState")
                || stderr.contains("w3cos_dom")
                || stderr.contains("w3cos_runtime"),
            "failure should point at missing ESM compile lowering or generated deps, got: {stderr}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn esm_bundle_with_jsdom_globals_adds_runtime_dependency() {
        let root = std::env::temp_dir().join("w3cos_esm_jsdom_cargo_wiring");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();

        let app = root.join("src/app.ts");
        std::fs::write(
            &app,
            r#"export function boot() {
  const el = document.createElement("div");
  setTimeout(() => {}, 0);
  return el;
}"#,
        )
        .unwrap();

        let out = root.join("build");
        compile_from_file(&app, &out).expect("compile_from_file should succeed");
        let bundle_rs = std::fs::read_to_string(out.join("src/esm_bundle.rs"))
            .expect("esm_bundle.rs should be generated");
        assert!(
            bundle_rs.contains("w3cos_runtime::jsdom::document_value()"),
            "document should map to the jsdom bridge: {bundle_rs}"
        );
        assert!(
            bundle_rs.contains("w3cos_runtime::jsdom::window_value().get_property(\"setTimeout\")"),
            "setTimeout should map to the jsdom window: {bundle_rs}"
        );
        // The fake builtin document must not be imported in generated modules.
        assert!(
            !bundle_rs.contains("console, document, parseFloat"),
            "fake w3cos_core document must not be imported: {bundle_rs}"
        );

        let cargo_toml = std::fs::read_to_string(out.join("Cargo.toml")).unwrap();
        assert!(
            cargo_toml.contains("w3cos-runtime"),
            "bundle referencing jsdom needs w3cos-runtime: {cargo_toml}"
        );

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn compile_from_file_builds_codemirror_esm_graph() {
        let root = std::env::temp_dir().join("w3cos_compile_codemirror_esm_graph");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();

        let app = root.join("src/app.ts");
        std::fs::write(
            &app,
            r#"import { EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";

const state = EditorState.create({ doc: "hello" });
const view = new EditorView({ state, parent: document.body });
console.log("view", view);"#,
        )
        .unwrap();

        let view = root.join("node_modules/@codemirror/view");
        std::fs::create_dir_all(view.join("dist")).unwrap();
        std::fs::write(
            view.join("package.json"),
            r#"{"exports":{".":{"import":"./dist/index.js"}}}"#,
        )
        .unwrap();
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

        let out = root.join("build");
        compile_from_file(&app, &out).expect("compile_from_file should build ESM diagnostics");
        let main_rs = std::fs::read_to_string(out.join("src/main.rs")).unwrap();

        assert!(
            main_rs.contains("ESM graph: resolved at compile time"),
            "missing ESM graph diagnostics: {main_rs}"
        );
        assert!(
            main_rs.contains("ESM modules: 4"),
            "entry + 3 package modules expected: {main_rs}"
        );
        assert!(
            main_rs.contains("ESM imports:"),
            "missing ESM import metadata: {main_rs}"
        );
        assert!(
            main_rs.contains("ESM exports:"),
            "missing ESM export metadata: {main_rs}"
        );
        assert!(
            main_rs.contains("@codemirror/view"),
            "missing CodeMirror view package: {main_rs}"
        );
        assert!(
            main_rs.contains("@codemirror/state"),
            "missing CodeMirror state package: {main_rs}"
        );
        assert!(
            main_rs.contains("style-mod"),
            "missing transitive dependency: {main_rs}"
        );

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn compile_from_file_generates_esm_bundle_module() {
        let root = std::env::temp_dir().join("w3cos_e2e_esm_bundle");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();

        std::fs::write(
            root.join("src/app.ts"),
            r#"import { EditorView } from "@codemirror/view";
import { EditorState } from "@codemirror/state";

export function boot() {
  const state = EditorState.create({doc: "hello"});
  const view = new EditorView({state});
  return view;
}"#,
        )
        .unwrap();

        let view = root.join("node_modules/@codemirror/view");
        std::fs::create_dir_all(view.join("dist")).unwrap();
        std::fs::write(view.join("package.json"), r#"{"module":"dist/index.js"}"#).unwrap();
        std::fs::write(
            view.join("dist/index.js"),
            r#"export class EditorView {
  mount() {
    const el = document.createElement("div");
    return el;
  }
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
  static create(config) { return new EditorState(); }
}"#,
        )
        .unwrap();

        let out = root.join("build");
        compile_from_file(&root.join("src/app.ts"), &out).expect("e2e compile should succeed");

        // Verify main.rs
        let main_rs = std::fs::read_to_string(out.join("src/main.rs")).unwrap();
        assert!(
            main_rs.contains("mod esm_bundle;"),
            "main.rs should include esm_bundle module: {main_rs}"
        );
        assert!(
            main_rs.contains("ESM bundle symbols:"),
            "main.rs should have bundle diagnostics: {main_rs}"
        );
        assert!(
            main_rs.contains("ESM bundle resolved: yes"),
            "bundle should be fully resolved: {main_rs}"
        );

        // Verify esm_bundle.rs is generated
        let bundle_rs = std::fs::read_to_string(out.join("src/esm_bundle.rs"))
            .expect("esm_bundle.rs should be generated");
        assert!(
            bundle_rs.contains("__build_class"),
            "classes should use the runtime class-factory pattern: {bundle_rs}"
        );
        assert!(
            bundle_rs.contains("w3cos_core::class::construct(&EditorView()"),
            "new EditorView() should lower to class::construct: {bundle_rs}"
        );
        assert!(
            bundle_rs.contains("EditorView"),
            "should contain EditorView: {bundle_rs}"
        );
        assert!(
            bundle_rs.contains("EditorState"),
            "should contain EditorState: {bundle_rs}"
        );
        assert!(
            bundle_rs.contains("pub fn"),
            "should contain function definitions: {bundle_rs}"
        );
        // Function bodies should be lowered, not todo!()
        assert!(
            bundle_rs
                .contains("w3cos_runtime::jsdom::document_value().call_method(\"createElement\""),
            "method body should be lowered: {bundle_rs}"
        );
        assert!(
            !bundle_rs.contains("todo!(\"lower ESM body: keymap\")"),
            "keymap body should be lowered: {bundle_rs}"
        );

        // Verify Cargo.toml
        let cargo = std::fs::read_to_string(out.join("Cargo.toml")).unwrap();
        assert!(
            cargo.contains("[package]"),
            "should have valid Cargo.toml: {cargo}"
        );

        std::fs::remove_dir_all(root).ok();
    }

    /// Integration test with the real `@codemirror/state` npm package.
    /// Requires: `npm install @codemirror/state` in /tmp/cm_real_test
    #[test]
    fn real_codemirror_state_npm_package_compile() {
        let project = std::path::Path::new("/tmp/cm_real_test");
        if !project.join("node_modules/@codemirror/state").exists() {
            eprintln!("SKIP: /tmp/cm_real_test not set up (npm install @codemirror/state)");
            return;
        }

        let entry = project.join("src/app.ts");
        let out = project.join("build");
        let _ = std::fs::remove_dir_all(&out);

        compile_from_file(&entry, &out)
            .expect("compile_from_file should handle real @codemirror/state");

        let main_rs = std::fs::read_to_string(out.join("src/main.rs")).unwrap();
        eprintln!("=== main.rs (first 40 lines) ===");
        for line in main_rs.lines().take(40) {
            eprintln!("  {line}");
        }

        assert!(
            main_rs.contains("ESM graph: resolved at compile time"),
            "diagnostics: {}",
            &main_rs[..200.min(main_rs.len())]
        );
        assert!(
            main_rs.contains("@codemirror/state"),
            "should detect package"
        );
        assert!(
            main_rs.contains("mod esm_bundle;"),
            "should generate esm_bundle module"
        );

        let bundle_rs = std::fs::read_to_string(out.join("src/esm_bundle.rs"))
            .expect("esm_bundle.rs should be generated");
        eprintln!("=== esm_bundle.rs stats ===");
        eprintln!("  lines: {}", bundle_rs.lines().count());
        eprintln!(
            "  class factories: {}",
            bundle_rs.matches("__build_class").count()
        );
        eprintln!("  fns: {}", bundle_rs.matches("pub fn").count());

        assert!(
            bundle_rs.contains("__build_class"),
            "should use the runtime class-factory pattern"
        );
        assert!(bundle_rs.contains("pub fn"), "should have fn defs");
        assert!(
            bundle_rs.contains("EditorState"),
            "should export EditorState"
        );

        eprintln!("=== real @codemirror/state compilation: SUCCESS ===");
    }

    #[test]
    fn esm_css_import_baked_into_bundle_and_dom_main() {
        let root = std::env::temp_dir().join("w3cos_esm_css_dom_main");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();

        std::fs::write(
            root.join("src/app.ts"),
            r#"import "./style.css";
export function main() {
  const el = document.createElement("div");
  return el;
}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("src/style.css"),
            ".app { color: #ff0000; width: 10px; }\n.outer .inner { position: absolute; }",
        )
        .unwrap();

        let out = root.join("build");
        compile_from_file(&root.join("src/app.ts"), &out).expect("compile should succeed");

        let main_rs = std::fs::read_to_string(out.join("src/main.rs")).unwrap();
        assert!(
            main_rs.contains("ESM css files: 1"),
            "missing css diagnostics: {main_rs}"
        );
        assert!(
            main_rs.contains("ESM css rules: 2"),
            "missing css rule count: {main_rs}"
        );
        // Non-React bundle → DOM-mode main, not the React one.
        assert!(
            main_rs.contains("W3COS_DOM_OK nodes="),
            "missing DOM-mode main: {main_rs}"
        );
        assert!(
            main_rs.contains("w3cos_runtime::run_app_dom"),
            "missing run_app_dom glue: {main_rs}"
        );
        assert!(
            !main_rs.contains("W3COS_AOT_RENDER_OK"),
            "non-React bundle must not get the React main: {main_rs}"
        );

        let bundle_rs = std::fs::read_to_string(out.join("src/esm_bundle.rs")).unwrap();
        assert!(
            bundle_rs.contains("pub fn register_styles()"),
            "missing register_styles: {bundle_rs}"
        );
        assert!(
            bundle_rs.contains(
                "w3cos_runtime::stylesheet::register_rule(\".app\", &[(\"color\", \"#ff0000\"), (\"width\", \"10px\")]);"
            ),
            "missing baked rule: {bundle_rs}"
        );
        assert!(
            bundle_rs.contains("register_rule(\".outer .inner\""),
            "descendant selector text must survive: {bundle_rs}"
        );
        assert!(
            bundle_rs.contains("pub fn run_entry() -> w3cos_core::Value { register_styles();"),
            "run_entry must register styles first: {bundle_rs}"
        );

        let cargo_toml = std::fs::read_to_string(out.join("Cargo.toml")).unwrap();
        assert!(
            cargo_toml.contains("w3cos-runtime"),
            "DOM-mode bundle needs w3cos-runtime: {cargo_toml}"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn framework_package_bundle_uses_dom_main() {
        let root = std::env::temp_dir().join("w3cos_esm_react_main");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();

        std::fs::write(
            root.join("src/app.ts"),
            r#"import { useState } from "react";
export function main() {
  const state = useState();
  return state;
}"#,
        )
        .unwrap();

        let react = root.join("node_modules/react");
        std::fs::create_dir_all(&react).unwrap();
        std::fs::write(react.join("package.json"), r#"{"main":"index.js"}"#).unwrap();
        std::fs::write(react.join("index.js"), "export function useState() {}").unwrap();

        let out = root.join("build");
        compile_from_file(&root.join("src/app.ts"), &out).expect("compile should succeed");

        let main_rs = std::fs::read_to_string(out.join("src/main.rs")).unwrap();
        assert!(
            main_rs.contains("W3COS_DOM_OK"),
            "framework bundle must use the DOM main: {main_rs}"
        );
        assert!(
            !main_rs.contains("install_host_modules"),
            "framework bundle must not install a framework host runtime: {main_rs}"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn esm_framework_bundle_runs_standard_create_root_entry_without_exported_main() {
        let root = std::env::temp_dir().join("w3cos_esm_react_create_root_entry");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("src/nested")).unwrap();
        std::fs::write(
            root.join("src/main.tsx"),
            r#"import { createRoot } from "react-dom/client";
import App from "./App";
createRoot(document.getElementById("root")!).render(App());"#,
        )
        .unwrap();
        std::fs::write(
            root.join("src/App.tsx"),
            r#"export default function App() { return "ready"; }"#,
        )
        .unwrap();
        let react_dom = root.join("node_modules/react-dom");
        std::fs::create_dir_all(&react_dom).unwrap();
        std::fs::write(
            react_dom.join("package.json"),
            r#"{"exports":{"./client":"./client.js"}}"#,
        )
        .unwrap();
        std::fs::write(
            react_dom.join("client.js"),
            "export function createRoot() { return { render() {} }; }",
        )
        .unwrap();

        let out = root.join("build");
        let manifest_style_entry = root.join("src/nested/../main.tsx");
        compile_from_file(&manifest_style_entry, &out).expect("compile should succeed");

        let main_rs = std::fs::read_to_string(out.join("src/main.rs")).unwrap();
        assert!(
            main_rs.contains("W3COS_DOM_OK"),
            "standard framework entry must use the DOM runtime: {main_rs}"
        );
        let bundle_rs = std::fs::read_to_string(out.join("src/esm_bundle.rs")).unwrap();
        assert!(
            bundle_rs.contains("createRoot(vec!") && bundle_rs.contains("m0__init();"),
            "top-level Web bootstrap must execute through module init: {bundle_rs}"
        );
        assert!(
            !bundle_rs.contains("host_modules::call(\"react"),
            "framework packages must compile from their real sources: {bundle_rs}"
        );
        let main_marker = format!("/// ESM module: {}", root.join("src/main.tsx").display());
        assert_eq!(
            bundle_rs.matches(&main_marker).count(),
            1,
            "a path containing `..` must not create a phantom duplicate entry module: {bundle_rs}"
        );
        std::fs::remove_dir_all(&root).ok();
    }
}
