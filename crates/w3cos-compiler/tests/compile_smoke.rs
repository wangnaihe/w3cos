//! Compile-smoke: every `examples/*` app and the LogiDesk native app must transpile.

use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn discover_apps(dir: &Path) -> Vec<PathBuf> {
    let mut apps = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return apps;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        for name in ["app.tsx", "app.ts"] {
            let app = path.join(name);
            if app.is_file() {
                apps.push(app);
            }
        }
    }
    apps.sort();
    apps
}

fn assert_ui_rust_output(rust: &str, app: &Path) {
    assert!(
        rust.contains("fn build_ui()"),
        "{} missing build_ui",
        app.display()
    );
    assert!(
        rust.contains("w3cos_runtime::run_app"),
        "{} missing run_app entry",
        app.display()
    );
}

fn assert_general_ts_output(rust: &str, app: &Path) {
    assert!(rust.contains("fn main()"), "{} missing main", app.display());
}

/// Examples whose TSX parser currently fails (tracked gaps).
const KNOWN_PARSE_GAPS: &[&str] = &["adaptive-layout"];

#[test]
fn all_examples_compile_to_rust() {
    let root = workspace_root();
    let examples = root.join("examples");
    let apps = discover_apps(&examples);
    assert!(
        apps.len() >= 19,
        "expected >=19 example apps under examples/, found {}",
        apps.len()
    );

    let mut compiled = 0usize;
    let mut gaps = 0usize;

    for app in &apps {
        let example_name = app
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let source =
            std::fs::read_to_string(app).unwrap_or_else(|e| panic!("read {}: {e}", app.display()));
        let result = w3cos_compiler::compile_to_rust(&source);

        if KNOWN_PARSE_GAPS.contains(&example_name.as_str()) {
            if result.is_err() {
                gaps += 1;
                eprintln!("known parse gap (JSX comments): {}", app.display());
                continue;
            }
        }

        let rust = result.unwrap_or_else(|e| panic!("compile {}: {e}", app.display()));
        if app.extension().is_some_and(|e| e == "ts") {
            assert_general_ts_output(&rust, app);
        } else {
            assert_ui_rust_output(&rust, app);
        }
        compiled += 1;
    }

    assert!(
        compiled >= 15,
        "expected at least 15 compiled examples, got {compiled}"
    );
    assert_eq!(
        gaps,
        KNOWN_PARSE_GAPS.len(),
        "expected all known-gap examples to fail parse until JSX comment support lands"
    );
}

#[test]
fn css_and_scss_examples_emit_standalone_projects() {
    let root = workspace_root();
    for name in ["css-demo", "scss-demo"] {
        let path = root.join("examples").join(name).join("app.tsx");
        assert!(path.is_file(), "missing {name}");
        let out = tempfile::tempdir().expect("tempdir");
        w3cos_compiler::compile_from_file(&path, out.path())
            .unwrap_or_else(|e| panic!("compile_from_file {name}: {e}"));
        assert!(
            out.path().join("src/main.rs").is_file(),
            "{name} should emit src/main.rs"
        );
        let main_rs = std::fs::read_to_string(out.path().join("src/main.rs")).unwrap();
        assert!(main_rs.contains("build_ui"));
    }
}

#[test]
fn logidesk_native_app_compiles_when_present() {
    let app =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../apps/logidesk-native/app.tsx");
    if !app.is_file() {
        eprintln!(
            "skip logidesk_native_app_compiles_when_present: {}",
            app.display()
        );
        return;
    }
    let source = std::fs::read_to_string(&app).unwrap();
    let rust = w3cos_compiler::compile_to_rust(&source).unwrap();
    assert_ui_rust_output(&rust, &app);
    assert!(
        rust.contains("ensure_signals"),
        "reactive native app should register signals once"
    );
    assert!(
        rust.contains("button_with_click"),
        "native driver buttons should wire onClick"
    );
}
