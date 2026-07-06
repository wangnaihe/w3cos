use crate::codegen::{find_workspace_root, gen_node};
use crate::css_parser::Stylesheet;
use crate::parser::{AppTree, SignalDecl};
use anyhow::Result;
use std::path::Path;

pub fn write_mobile_project(tree: &AppTree, stylesheet: &Stylesheet, output_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(output_dir.join("src"))?;
    std::fs::write(
        output_dir.join("src/lib.rs"),
        generate_mobile_lib(tree, stylesheet)?,
    )?;
    std::fs::write(output_dir.join("Cargo.toml"), generate_mobile_cargo_toml()?)?;
    Ok(())
}

pub fn generate_mobile_lib(tree: &AppTree, stylesheet: &Stylesheet) -> Result<String> {
    let is_reactive = !tree.signals.is_empty();
    let signal_names: Vec<&str> = tree.signals.iter().map(|s| s.name.as_str()).collect();
    let component_code = gen_node(&tree.root, 0, &signal_names, stylesheet);

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
        r#"//! Auto-generated mobile lib — do not edit.
{uses}

fn build_ui() -> Component {{
{signal_inits}{component_code}
}}

/// C ABI entry used by iOS shell and Android JNI fallback.
#[no_mangle]
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
#[no_mangle]
fn android_main(app: winit::platform::android::activity::AndroidApp) {{
    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Info),
    );
    if let Err(e) = w3cos_runtime::run_app_on_android(app, build_ui) {{
        log::error!("android_main failed: {{e:#}}");
    }}
}}
"#,
    ))
}

fn gen_signal_inits(signals: &[SignalDecl]) -> String {
    signals
        .iter()
        .enumerate()
        .map(|(i, sig)| {
            format!(
                "    let _ = w3cos_runtime::state::create_signal({initial});\n    let {name} = w3cos_runtime::state::get_signal({i});\n",
                initial = sig.initial,
                name = sig.name,
            )
        })
        .collect::<Vec<_>>()
        .join("")
}

pub fn generate_mobile_cargo_toml() -> Result<String> {
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
w3cos-mobile = {{ path = "{mobile}" }}
w3cos-runtime = {{ path = "{runtime}", features = ["gpu"] }}
w3cos-std = {{ path = "{std}" }}
log = "0.4"

[target.'cfg(target_os = "android")'.dependencies]
android_logger = "0.14"
winit = {{ version = "0.30", features = ["android-game-activity"] }}
"#,
        mobile = root.join("crates/w3cos-mobile").display(),
        runtime = root.join("crates/w3cos-runtime").display(),
        std = root.join("crates/w3cos-std").display(),
    ))
}
