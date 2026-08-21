use crate::codegen::{
    CompileOptions, GENERATED_DEV_PROFILE, GENERATED_RELEASE_PROFILE, find_workspace_root, gen_node,
};
use crate::css_parser::Stylesheet;
use crate::parser::{AppTree, SignalDecl};
use anyhow::{Context, Result, bail};
use std::path::Path;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct MobileRuntimeCapabilities {
    pub(crate) web_graphics_advanced: bool,
    pub(crate) web_media_advanced: bool,
}

fn write_if_changed(path: &Path, contents: impl AsRef<[u8]>) -> Result<()> {
    let contents = contents.as_ref();
    if std::fs::read(path).is_ok_and(|existing| existing == contents) {
        return Ok(());
    }
    std::fs::write(path, contents)?;
    Ok(())
}

fn canonicalize_embedded_bundle_root(source: &str) -> String {
    const ENTRY_MARKER: &str = "// bundle entry: ";
    let Some(entry_start) = source
        .find(ENTRY_MARKER)
        .map(|index| index + ENTRY_MARKER.len())
    else {
        return source.to_string();
    };
    let entry_end = source[entry_start..]
        .find('\n')
        .map(|offset| entry_start + offset)
        .unwrap_or(source.len());
    let entry = Path::new(&source[entry_start..entry_end]);
    let Some(root) = entry.parent().and_then(Path::to_str) else {
        return source.to_string();
    };
    if root.is_empty() || root == "/" {
        return source.to_string();
    }
    source.replace(root, "/__w3cos_bundle__")
}

fn write_bundle_if_changed(path: &Path, contents: &str) -> Result<()> {
    if std::fs::read_to_string(path)
        .is_ok_and(|existing| canonicalize_embedded_bundle_root(&existing) == contents)
    {
        return Ok(());
    }
    std::fs::write(path, contents)?;
    Ok(())
}

fn split_bundle_modules(bundle: &str) -> Result<(String, Vec<(String, String)>)> {
    const MODULE_MARKER: &str = "/// ESM module: ";
    const STYLES_MARKER: &str = "/// Stylesheet rules collected from ESM `.css` imports";
    let starts = bundle
        .match_indices(MODULE_MARKER)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if starts.is_empty() {
        return Ok((bundle.to_string(), Vec::new()));
    }
    let tail_start = bundle
        .find(STYLES_MARKER)
        .context("generated ESM bundle is missing its stylesheet tail")?;
    let mut root = bundle[..starts[0]].to_string();
    let mut modules = Vec::with_capacity(starts.len());
    for (position, start) in starts.iter().copied().enumerate() {
        let end = starts.get(position + 1).copied().unwrap_or(tail_start);
        let section = bundle[start..end].trim_end();
        let declaration_start = section
            .find("\nmod m")
            .map(|index| index + 1)
            .context("generated ESM module is missing its Rust declaration")?;
        let declaration_end = section[declaration_start..]
            .find(" {\n")
            .map(|index| declaration_start + index)
            .context("generated ESM module declaration is malformed")?;
        let name = &section[declaration_start + "mod ".len()..declaration_end];
        if !name.starts_with('m')
            || !name[1..]
                .chars()
                .all(|character| character.is_ascii_digit())
        {
            bail!("generated ESM module has an unsafe name: {name}");
        }
        let body_start = declaration_end + " {\n".len();
        let body_end = section
            .rfind("\n}")
            .context("generated ESM module is missing its closing brace")?;
        root.push_str(&section[..declaration_start]);
        root.push_str(&format!(
            "#[path = \"esm_bundle/{name}.rs\"]\nmod {name};\n\n"
        ));
        modules.push((
            format!("{name}.rs"),
            format!("#![allow(warnings)]\n{}\n", &section[body_start..body_end]),
        ));
    }
    root.push_str(&bundle[tail_start..]);
    Ok((root, modules))
}

fn write_split_bundle_if_changed(src_dir: &Path, bundle: &str) -> Result<()> {
    let (root, modules) = split_bundle_modules(bundle)?;
    write_bundle_if_changed(&src_dir.join("esm_bundle.rs"), &root)?;
    let module_dir = src_dir.join("esm_bundle");
    if modules.is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(&module_dir)?;
    let expected = modules
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<std::collections::HashSet<_>>();
    for entry in std::fs::read_dir(&module_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('m') && name.ends_with(".rs") && !expected.contains(name.as_ref()) {
            std::fs::remove_file(entry.path())?;
        }
    }
    for (name, contents) in modules {
        write_if_changed(&module_dir.join(name), contents)?;
    }
    Ok(())
}

pub fn write_mobile_project(
    tree: &AppTree,
    stylesheet: &Stylesheet,
    output_dir: &Path,
    platform: &str,
    safe_area: bool,
    interactive_widget: &str,
    options: &CompileOptions,
) -> Result<()> {
    let runtime_capabilities = MobileRuntimeCapabilities::default();
    std::fs::create_dir_all(output_dir.join("src"))?;
    let body = generate_app_body(tree, stylesheet)?;
    if platform == "ios" {
        write_if_changed(&output_dir.join("src/app_ui.rs"), &body)?;
        write_if_changed(
            &output_dir.join("src/layout_export.rs"),
            generate_layout_export(tree, safe_area)?,
        )?;
        let main = generate_ios_main(safe_area, interactive_widget, options)?;
        write_if_changed(&output_dir.join("src/main.rs"), main)?;
        write_if_changed(
            &output_dir.join("Cargo.toml"),
            generate_ios_cargo_toml_with_capabilities(options, runtime_capabilities)?,
        )?;
    } else if platform == "harmony" {
        write_if_changed(
            &output_dir.join("src/lib.rs"),
            generate_harmony_component_lib(&body, interactive_widget, options)?,
        )?;
        write_if_changed(
            &output_dir.join("Cargo.toml"),
            generate_harmony_cargo_toml(options)?,
        )?;
    } else {
        write_if_changed(
            &output_dir.join("src/lib.rs"),
            generate_android_lib(&body, interactive_widget, options)?,
        )?;
        write_if_changed(
            &output_dir.join("Cargo.toml"),
            generate_android_cargo_toml_with_capabilities(options, runtime_capabilities)?,
        )?;
    }
    Ok(())
}

pub fn write_mobile_dom_project(
    bundle: &str,
    output_dir: &Path,
    platform: &str,
    safe_area: bool,
    interactive_widget: &str,
    options: &CompileOptions,
) -> Result<()> {
    write_mobile_dom_project_with_capabilities(
        bundle,
        output_dir,
        platform,
        safe_area,
        interactive_widget,
        options,
        MobileRuntimeCapabilities::default(),
    )
}

pub(crate) fn write_mobile_dom_project_with_capabilities(
    bundle: &str,
    output_dir: &Path,
    platform: &str,
    safe_area: bool,
    interactive_widget: &str,
    options: &CompileOptions,
    runtime_capabilities: MobileRuntimeCapabilities,
) -> Result<()> {
    std::fs::create_dir_all(output_dir.join("src"))?;
    write_split_bundle_if_changed(&output_dir.join("src"), bundle)?;
    let body = r#"pub fn setup() {
    if w3cos_runtime::dom::get_element_by_id("root").is_none() {
        let root = w3cos_runtime::dom::create_element("div");
        w3cos_runtime::dom::set_attribute(root, "id", "root");
        w3cos_runtime::dom::append_child(w3cos_runtime::dom::body_id(), root);
    }
    let _ = crate::esm_bundle::run_entry_async();
}
"#;
    if platform == "ios" {
        write_if_changed(&output_dir.join("src/app_ui.rs"), body)?;
        write_if_changed(
            &output_dir.join("src/layout_export.rs"),
            generate_dom_layout_export(safe_area),
        )?;
        let safe_init = if safe_area {
            "    w3cos_std::safe_area::set_enabled(true);\n"
        } else {
            ""
        };
        let viewport_init = gen_viewport_init(interactive_widget);
        let document_base_init = gen_document_base_init(options);
        let main = format!(
            "//! Auto-generated iOS DOM app — do not edit.\nmod esm_bundle;\nmod app_ui;\n\nfn main() {{\n{safe_init}{viewport_init}{document_base_init}    if let Err(error) = w3cos_mobile::run_mobile_app_dom(app_ui::setup) {{\n        eprintln!(\"w3cos iOS DOM app failed: {{error:#}}\");\n    }}\n}}\n"
        );
        write_if_changed(&output_dir.join("src/main.rs"), main)?;
        write_if_changed(
            &output_dir.join("Cargo.toml"),
            generate_ios_cargo_toml_with_capabilities(options, runtime_capabilities)?,
        )?;
    } else if platform == "harmony" {
        let viewport_init = gen_viewport_init(interactive_widget);
        let document_base_init = gen_document_base_init(options);
        let lib = format!(
            "//! Auto-generated HarmonyOS DOM app — do not edit.\nmod esm_bundle;\n{body}\n#[unsafe(no_mangle)]\npub extern \"C\" fn w3cos_harmony_surface_created(window: *mut core::ffi::c_void, width: u32, height: u32) -> i32 {{\n{viewport_init}{document_base_init}    w3cos_mobile::harmony::surface_created_dom(window, width, height, setup)\n}}\n\n{harmony_common}",
            harmony_common = generate_harmony_common_exports(),
        );
        write_if_changed(&output_dir.join("src/lib.rs"), lib)?;
        write_if_changed(
            &output_dir.join("Cargo.toml"),
            generate_harmony_cargo_toml(options)?,
        )?;
    } else {
        let viewport_init = gen_viewport_init(interactive_widget);
        let document_base_init = gen_document_base_init(options);
        let lib = format!(
            "//! Auto-generated Android DOM app — do not edit.\nmod esm_bundle;\n{body}\n#[unsafe(no_mangle)]\npub extern \"C\" fn w3cos_app_run() -> i32 {{\n{document_base_init}    match w3cos_mobile::run_mobile_app_dom(setup) {{\n        Ok(()) => 0,\n        Err(error) => {{ eprintln!(\"w3cos_app_run failed: {{error:#}}\"); 1 }}\n    }}\n}}\n\n#[cfg(target_os = \"android\")]\n#[unsafe(no_mangle)]\nfn android_main(app: winit::platform::android::activity::AndroidApp) {{\n    android_logger::init_once(android_logger::Config::default().with_max_level(log::LevelFilter::Info));\n{viewport_init}{document_base_init}    if let Err(error) = w3cos_runtime::run_app_on_android_dom(app, setup) {{\n        log::error!(\"android_main failed: {{error:#}}\");\n    }}\n}}\n"
        );
        write_if_changed(&output_dir.join("src/lib.rs"), lib)?;
        write_if_changed(
            &output_dir.join("Cargo.toml"),
            generate_android_cargo_toml_with_capabilities(options, runtime_capabilities)?,
        )?;
    }
    Ok(())
}

fn generate_dom_layout_export(safe_area: bool) -> String {
    let safe_init = if safe_area {
        "    w3cos_std::safe_area::set_enabled(true);\n"
    } else {
        ""
    };
    format!(
        "mod esm_bundle;\nmod app_ui;\nfn main() {{\n{safe_init}    app_ui::setup();\n    println!(\"{{}}\", serde_json::json!({{\"nodes\": w3cos_runtime::dom::node_count()}}));\n}}\n"
    )
}

fn generate_app_body(tree: &AppTree, stylesheet: &Stylesheet) -> Result<String> {
    let is_reactive = !tree.signals.is_empty();
    let signal_names: Vec<&str> = tree.signals.iter().map(|s| s.name.as_str()).collect();
    let component_code = gen_node(&tree.root, 0, &signal_names, stylesheet, 1, 1);
    let signal_inits = if is_reactive {
        gen_signal_inits(&tree.signals)
    } else {
        String::new()
    };
    let uses = if is_reactive {
        "use w3cos_std::{Component, EventAction, Style};\nuse w3cos_std::style::*;\nuse w3cos_std::color::Color;"
    } else {
        "use w3cos_std::{Component, Style};\nuse w3cos_std::style::*;\nuse w3cos_std::color::Color;"
    };
    Ok(format!(
        r#"{uses}

pub fn build_ui() -> Component {{
{signal_inits}{component_code}
}}
"#,
    ))
}

fn gen_viewport_init(interactive_widget: &str) -> String {
    let mode = match interactive_widget {
        "resizes-visual" => "w3cos_std::viewport::InteractiveWidget::ResizesVisual",
        "overlays-content" => "w3cos_std::viewport::InteractiveWidget::OverlaysContent",
        _ => "w3cos_std::viewport::InteractiveWidget::ResizesContent",
    };
    format!("    w3cos_std::viewport::set_interactive_widget({mode});\n",)
}

fn gen_document_base_init(options: &CompileOptions) -> String {
    options
        .document_base_url
        .as_deref()
        .map(|url| {
            format!(
                "    w3cos_runtime::document_context::configure_document_base_url({url:?}).expect(\"invalid document_base_url\");\n"
            )
        })
        .unwrap_or_default()
}

fn generate_ios_main(
    safe_area: bool,
    interactive_widget: &str,
    options: &CompileOptions,
) -> Result<String> {
    let safe_init = if safe_area {
        r#"    w3cos_std::safe_area::set_enabled(true);
"#
    } else {
        ""
    };
    let viewport_init = gen_viewport_init(interactive_widget);
    let document_base_init = gen_document_base_init(options);
    Ok(format!(
        r#"//! Auto-generated iOS app — do not edit.
mod app_ui;
use app_ui::build_ui;

fn main() {{
{safe_init}{viewport_init}{document_base_init}    if let Err(e) = w3cos_mobile::run_mobile_app(build_ui) {{
        eprintln!("w3cos iOS app failed: {{e:#}}");
    }}
}}
"#
    ))
}

fn generate_layout_export(tree: &AppTree, safe_area: bool) -> Result<String> {
    let signal_inits = gen_signal_inits(&tree.signals);
    let safe_init = if safe_area {
        r#"    w3cos_std::safe_area::set_enabled(true);
    w3cos_std::safe_area::set_insets(w3cos_std::safe_area::SafeAreaInsets {
        top: 59.0,
        right: 0.0,
        bottom: 34.0,
        left: 0.0,
    });
"#
    } else {
        ""
    };
    Ok(format!(
        r#"//! Auto-generated layout metrics export — do not edit.
mod app_ui;
use app_ui::build_ui;

fn main() {{
{signal_inits}{safe_init}
    let root = build_ui();
    let layout =
        w3cos_runtime::layout::compute(&root, 402.0, 874.0).expect("layout compute");
    let flat = w3cos_runtime::layout::pre_flatten(&root);

    let mut nodes = serde_json::Map::new();
    for (i, node) in flat.iter().enumerate() {{
        let key = match node.kind {{
            w3cos_std::ComponentKind::Text {{ content }} => Some(format!("text:{{}}", content)),
            w3cos_std::ComponentKind::Button {{ label }} => Some(format!("btn:{{}}", label)),
            _ => None,
        }};
        if let Some(key) = key {{
            if let Some((rect, _)) = layout.iter().find(|(_, idx)| *idx == i) {{
                nodes.insert(
                    key,
                    serde_json::json!({{
                        "x": rect.x,
                        "y": rect.y,
                        "w": rect.width,
                        "h": rect.height,
                    }}),
                );
            }}
        }}
    }}
    println!("{{}}", serde_json::Value::Object(nodes));
}}
"#,
        signal_inits = signal_inits,
        safe_init = safe_init,
    ))
}

fn generate_android_lib(
    body: &str,
    interactive_widget: &str,
    options: &CompileOptions,
) -> Result<String> {
    let viewport_init = gen_viewport_init(interactive_widget);
    let document_base_init = gen_document_base_init(options);
    Ok(format!(
        r#"//! Auto-generated Android lib — do not edit.
{body}
#[unsafe(no_mangle)]
pub extern "C" fn w3cos_app_run() -> i32 {{
{document_base_init}    match w3cos_mobile::run_mobile_app(build_ui) {{
        Ok(()) => 0,
        Err(e) => {{
            eprintln!("w3cos_app_run failed: {{e:#}}");
            1
        }}
    }}
}}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: winit::platform::android::activity::AndroidApp) {{
    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Info),
    );
{viewport_init}{document_base_init}    if let Err(e) = w3cos_runtime::run_app_on_android(app, build_ui) {{
        log::error!("android_main failed: {{e:#}}");
    }}
}}
"#
    ))
}

fn generate_harmony_component_lib(
    body: &str,
    interactive_widget: &str,
    options: &CompileOptions,
) -> Result<String> {
    let viewport_init = gen_viewport_init(interactive_widget);
    let document_base_init = gen_document_base_init(options);
    Ok(format!(
        r#"//! Auto-generated HarmonyOS component app — do not edit.
{body}
#[unsafe(no_mangle)]
pub extern "C" fn w3cos_harmony_surface_created(
    window: *mut core::ffi::c_void,
    width: u32,
    height: u32,
) -> i32 {{
{viewport_init}{document_base_init}    w3cos_mobile::harmony::surface_created_component(window, width, height, build_ui)
}}

{common}
"#,
        common = generate_harmony_common_exports(),
    ))
}

fn generate_harmony_common_exports() -> &'static str {
    r#"#[unsafe(no_mangle)]
pub extern "C" fn w3cos_harmony_surface_changed(width: u32, height: u32) -> i32 {
    w3cos_mobile::harmony::surface_changed(width, height)
}

#[unsafe(no_mangle)]
pub extern "C" fn w3cos_harmony_surface_destroyed() {
    w3cos_mobile::harmony::surface_destroyed();
}

#[unsafe(no_mangle)]
pub extern "C" fn w3cos_harmony_frame() -> i32 {
    w3cos_mobile::harmony::frame()
}

#[unsafe(no_mangle)]
pub extern "C" fn w3cos_harmony_touch(
    phase: i32,
    x: f32,
    y: f32,
    pointer_id: i64,
    pressure: f32,
) {
    w3cos_mobile::harmony::touch(phase, x, y, pointer_id, pressure);
}"#
}

fn gen_signal_inits(signals: &[SignalDecl]) -> String {
    if signals.is_empty() {
        return String::new();
    }
    let register: Vec<String> = signals
        .iter()
        .map(|sig| {
            let initializer = sig.initial.rust_initializer();
            format!(
                "        w3cos_runtime::state::register_signal_name({name:?});\n        let _ = {initializer};",
                name = sig.name,
            )
        })
        .collect();
    format!(
        "    w3cos_runtime::state::ensure_signals(|| {{\n{register}\n    }});\n",
        register = register.join("\n"),
    )
}

fn mobile_runtime_features(
    options: &CompileOptions,
    capabilities: MobileRuntimeCapabilities,
) -> Vec<&'static str> {
    // The mobile window path presents through Skia Metal/Vulkan. The current
    // presenter is hosted by the cpu-render window owner, so both features are
    // required while Vello/WGPU remains opt-in for WebGPU/WebGL/WebXR usage.
    let mut features = vec!["cpu-render", "skia"];
    if capabilities.web_graphics_advanced {
        features.extend(["gpu", "web-graphics-advanced"]);
    }
    if capabilities.web_media_advanced {
        features.push("web-media-advanced");
    }
    if options.devtools {
        features.push("devtools");
    }
    features
}

fn deps_block(
    root: &Path,
    options: &CompileOptions,
    capabilities: MobileRuntimeCapabilities,
) -> String {
    let runtime_features = mobile_runtime_features(options, capabilities)
        .into_iter()
        .map(|feature| format!(r#""{feature}""#))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        r#"w3cos-mobile = {{ path = "{mobile}", default-features = false }}
w3cos-runtime = {{ path = "{runtime}", default-features = false, features = [{runtime_features}] }}
w3cos-std = {{ path = "{std}" }}
w3cos-core = {{ path = "{core}" }}
log = "0.4""#,
        mobile = root.join("crates/w3cos-mobile").display(),
        runtime = root.join("crates/w3cos-runtime").display(),
        std = root.join("crates/w3cos-std").display(),
        core = root.join("crates/w3cos-core").display(),
    )
}

pub fn generate_ios_cargo_toml(options: &CompileOptions) -> Result<String> {
    generate_ios_cargo_toml_with_capabilities(options, MobileRuntimeCapabilities::default())
}

fn generate_ios_cargo_toml_with_capabilities(
    options: &CompileOptions,
    capabilities: MobileRuntimeCapabilities,
) -> Result<String> {
    let root = find_workspace_root()?;
    Ok(format!(
        r#"[package]
name = "w3cos-mobile-app"
version = "0.1.0"
edition = "2024"

[[bin]]
name = "W3cosApp"
path = "src/main.rs"

[[bin]]
name = "layout-export"
path = "src/layout_export.rs"

{dev_profile}
{release_profile}
[dependencies]
{deps}
serde_json = "1"
"#,
        deps = deps_block(&root, options, capabilities),
        dev_profile = GENERATED_DEV_PROFILE,
        release_profile = GENERATED_RELEASE_PROFILE,
    ))
}

pub fn generate_android_cargo_toml(options: &CompileOptions) -> Result<String> {
    generate_android_cargo_toml_with_capabilities(options, MobileRuntimeCapabilities::default())
}

fn generate_android_cargo_toml_with_capabilities(
    options: &CompileOptions,
    capabilities: MobileRuntimeCapabilities,
) -> Result<String> {
    let root = find_workspace_root()?;
    Ok(format!(
        r#"[package]
name = "w3cos-mobile-app"
version = "0.1.0"
edition = "2024"

[lib]
name = "w3cos_mobile_app"
crate-type = ["cdylib"]

{dev_profile}
{release_profile}
[dependencies]
{deps}

[target.'cfg(target_os = "android")'.dependencies]
android_logger = "0.14"
winit = {{ version = "0.30", features = ["android-native-activity"] }}
"#,
        deps = deps_block(&root, options, capabilities),
        dev_profile = GENERATED_DEV_PROFILE,
        release_profile = GENERATED_RELEASE_PROFILE,
    ))
}

pub fn generate_harmony_cargo_toml(options: &CompileOptions) -> Result<String> {
    let root = find_workspace_root()?;
    let runtime_features = if options.devtools {
        r#"["skia", "devtools"]"#
    } else {
        r#"["skia"]"#
    };
    Ok(format!(
        r#"[package]
name = "w3cos-mobile-app"
version = "0.1.0"
edition = "2024"

[lib]
name = "w3cos_mobile_app"
crate-type = ["cdylib"]

{dev_profile}
[dependencies]
w3cos-mobile = {{ path = "{mobile}", default-features = false }}
w3cos-runtime = {{ path = "{runtime}", default-features = false, features = {runtime_features} }}
w3cos-std = {{ path = "{std}" }}
w3cos-core = {{ path = "{core}" }}
log = "0.4"
"#,
        mobile = root.join("crates/w3cos-mobile").display(),
        runtime = root.join("crates/w3cos-runtime").display(),
        std = root.join("crates/w3cos-std").display(),
        core = root.join("crates/w3cos-core").display(),
        dev_profile = GENERATED_DEV_PROFILE,
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        MobileRuntimeCapabilities, canonicalize_embedded_bundle_root,
        generate_android_cargo_toml_with_capabilities, generate_ios_cargo_toml_with_capabilities,
        split_bundle_modules,
    };
    use crate::codegen::CompileOptions;

    #[test]
    fn generated_bundle_root_is_stable_across_vite_processes() {
        let first = "// bundle entry: /tmp/w3cos-vite-101-main/w3cos-entry.js\n\
/// ESM module: /tmp/w3cos-vite-101-main/assets/index.js\n\
register(\"/tmp/w3cos-vite-101-main/assets/index.js\");\n";
        let second = "// bundle entry: /tmp/w3cos-vite-202-main/w3cos-entry.js\n\
/// ESM module: /tmp/w3cos-vite-202-main/assets/index.js\n\
register(\"/tmp/w3cos-vite-202-main/assets/index.js\");\n";

        assert_eq!(
            canonicalize_embedded_bundle_root(first),
            canonicalize_embedded_bundle_root(second)
        );
    }

    #[test]
    fn generated_bundle_modules_are_split_into_incremental_source_files() {
        let bundle = "#![allow(warnings)]\n// bundle entry: /entry.js\n\
/// ESM module: /vendor.js\n\
mod m0 {\nfn init() {}\n}\n\n\
/// ESM module: /app.js\n\
mod m1 {\nfn init() { let value = \"}\"; }\n}\n\n\
/// Stylesheet rules collected from ESM `.css` imports (baked in at compile time).\n\
pub fn register_styles() {}\n\
pub fn run_entry() { m0::init(); m1::init(); }\n";

        let (root, modules) = split_bundle_modules(bundle).unwrap();

        assert!(root.contains("#[path = \"esm_bundle/m0.rs\"]\nmod m0;"));
        assert!(root.contains("#[path = \"esm_bundle/m1.rs\"]\nmod m1;"));
        assert!(root.contains("pub fn register_styles() {}"));
        assert_eq!(modules.len(), 2);
        assert!(modules[0].1.contains("fn init() {}"));
        assert!(modules[1].1.contains("let value = \"}\";"));
    }

    #[test]
    fn mobile_manifests_link_only_the_skia_renderer_and_release_profile() {
        let options = CompileOptions {
            devtools: true,
            ..CompileOptions::default()
        };
        for manifest in [
            generate_ios_cargo_toml_with_capabilities(
                &options,
                MobileRuntimeCapabilities::default(),
            )
            .unwrap(),
            generate_android_cargo_toml_with_capabilities(
                &options,
                MobileRuntimeCapabilities::default(),
            )
            .unwrap(),
        ] {
            assert!(manifest.contains(r#"w3cos-runtime = { path = "#));
            assert!(manifest.contains(
                r#"default-features = false, features = ["cpu-render", "skia", "devtools"]"#
            ));
            assert!(!manifest.contains(r#"features = ["gpu""#));
            assert!(manifest.contains("[profile.release]"));
            assert!(manifest.contains("opt-level = \"z\""));
            assert!(manifest.contains("lto = \"fat\""));
            assert!(manifest.contains("strip = \"symbols\""));
        }
    }

    #[test]
    fn mobile_manifest_restores_advanced_runtime_capabilities_when_referenced() {
        let manifest = generate_ios_cargo_toml_with_capabilities(
            &CompileOptions::default(),
            MobileRuntimeCapabilities {
                web_graphics_advanced: true,
                web_media_advanced: true,
            },
        )
        .unwrap();

        assert!(manifest.contains(
            r#"features = ["cpu-render", "skia", "gpu", "web-graphics-advanced", "web-media-advanced"]"#
        ));
    }
}
