use crate::codegen::{CompileOptions, find_workspace_root, gen_node};
use crate::css_parser::Stylesheet;
use crate::parser::{AppTree, SignalDecl};
use anyhow::Result;
use std::path::Path;

pub fn write_mobile_project(
    tree: &AppTree,
    stylesheet: &Stylesheet,
    output_dir: &Path,
    platform: &str,
    safe_area: bool,
    interactive_widget: &str,
    options: &CompileOptions,
) -> Result<()> {
    std::fs::create_dir_all(output_dir.join("src"))?;
    let body = generate_app_body(tree, stylesheet)?;
    if platform == "ios" {
        std::fs::write(output_dir.join("src/app_ui.rs"), &body)?;
        std::fs::write(
            output_dir.join("src/layout_export.rs"),
            generate_layout_export(tree, safe_area)?,
        )?;
        let main = generate_ios_main(safe_area, interactive_widget)?;
        std::fs::write(output_dir.join("src/main.rs"), main)?;
        std::fs::write(
            output_dir.join("Cargo.toml"),
            generate_ios_cargo_toml(options)?,
        )?;
    } else {
        std::fs::write(
            output_dir.join("src/lib.rs"),
            generate_android_lib(&body, interactive_widget)?,
        )?;
        std::fs::write(
            output_dir.join("Cargo.toml"),
            generate_android_cargo_toml(options)?,
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
    std::fs::create_dir_all(output_dir.join("src"))?;
    std::fs::write(output_dir.join("src/esm_bundle.rs"), bundle)?;
    let body = r#"pub fn setup() {
    if w3cos_runtime::dom::get_element_by_id("root").is_none() {
        let root = w3cos_runtime::dom::create_element("div");
        w3cos_runtime::dom::set_attribute(root, "id", "root");
        w3cos_runtime::dom::append_child(w3cos_runtime::dom::body_id(), root);
    }
    crate::esm_bundle::run_entry();
}
"#;
    if platform == "ios" {
        std::fs::write(output_dir.join("src/app_ui.rs"), body)?;
        std::fs::write(
            output_dir.join("src/layout_export.rs"),
            generate_dom_layout_export(safe_area),
        )?;
        let safe_init = if safe_area {
            "    w3cos_std::safe_area::set_enabled(true);\n"
        } else {
            ""
        };
        let viewport_init = gen_viewport_init(interactive_widget);
        let main = format!(
            "//! Auto-generated iOS DOM app — do not edit.\nmod esm_bundle;\nmod app_ui;\n\nfn main() {{\n{safe_init}{viewport_init}    if let Err(error) = w3cos_mobile::run_mobile_app_dom(app_ui::setup) {{\n        eprintln!(\"w3cos iOS DOM app failed: {{error:#}}\");\n    }}\n}}\n"
        );
        std::fs::write(output_dir.join("src/main.rs"), main)?;
        std::fs::write(
            output_dir.join("Cargo.toml"),
            generate_ios_cargo_toml(options)?,
        )?;
    } else {
        let viewport_init = gen_viewport_init(interactive_widget);
        let lib = format!(
            "//! Auto-generated Android DOM app — do not edit.\nmod esm_bundle;\n{body}\n#[unsafe(no_mangle)]\npub extern \"C\" fn w3cos_app_run() -> i32 {{\n    match w3cos_mobile::run_mobile_app_dom(setup) {{\n        Ok(()) => 0,\n        Err(error) => {{ eprintln!(\"w3cos_app_run failed: {{error:#}}\"); 1 }}\n    }}\n}}\n\n#[cfg(target_os = \"android\")]\n#[unsafe(no_mangle)]\nfn android_main(app: winit::platform::android::activity::AndroidApp) {{\n    android_logger::init_once(android_logger::Config::default().with_max_level(log::LevelFilter::Info));\n{viewport_init}    if let Err(error) = w3cos_runtime::run_app_on_android_dom(app, setup) {{\n        log::error!(\"android_main failed: {{error:#}}\");\n    }}\n}}\n"
        );
        std::fs::write(output_dir.join("src/lib.rs"), lib)?;
        std::fs::write(
            output_dir.join("Cargo.toml"),
            generate_android_cargo_toml(options)?,
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

fn generate_ios_main(safe_area: bool, interactive_widget: &str) -> Result<String> {
    let safe_init = if safe_area {
        r#"    w3cos_std::safe_area::set_enabled(true);
"#
    } else {
        ""
    };
    let viewport_init = gen_viewport_init(interactive_widget);
    Ok(format!(
        r#"//! Auto-generated iOS app — do not edit.
mod app_ui;
use app_ui::build_ui;

fn main() {{
{safe_init}{viewport_init}    if let Err(e) = w3cos_mobile::run_mobile_app(build_ui) {{
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

fn generate_android_lib(body: &str, interactive_widget: &str) -> Result<String> {
    let viewport_init = gen_viewport_init(interactive_widget);
    Ok(format!(
        r#"//! Auto-generated Android lib — do not edit.
{body}
#[unsafe(no_mangle)]
pub extern "C" fn w3cos_app_run() -> i32 {{
    match w3cos_mobile::run_mobile_app(build_ui) {{
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
{viewport_init}    if let Err(e) = w3cos_runtime::run_app_on_android(app, build_ui) {{
        log::error!("android_main failed: {{e:#}}");
    }}
}}
"#
    ))
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

fn deps_block(root: &Path, options: &CompileOptions) -> String {
    let runtime_features = if options.devtools {
        r#", features = ["devtools"]"#
    } else {
        ""
    };
    format!(
        r#"w3cos-mobile = {{ path = "{mobile}" }}
w3cos-runtime = {{ path = "{runtime}"{runtime_features} }}
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

[dependencies]
{deps}
serde_json = "1"
"#,
        deps = deps_block(&root, options),
    ))
}

pub fn generate_android_cargo_toml(options: &CompileOptions) -> Result<String> {
    let root = find_workspace_root()?;
    Ok(format!(
        r#"[package]
name = "w3cos-mobile-app"
version = "0.1.0"
edition = "2024"

[lib]
name = "w3cos_mobile_app"
crate-type = ["cdylib"]

[dependencies]
{deps}

[target.'cfg(target_os = "android")'.dependencies]
android_logger = "0.14"
winit = {{ version = "0.30", features = ["android-native-activity"] }}
"#,
        deps = deps_block(&root, options),
    ))
}
