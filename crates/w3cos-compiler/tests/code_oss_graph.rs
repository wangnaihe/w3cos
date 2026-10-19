//! Code-OSS AOT sweep (M3 item 23).
//!
//! Point `W3COS_CODE_OSS` at a Code-OSS checkout whose `out/` was produced by
//! `npm run transpile-client` (see `tools/vscode-baseline/README.md`). The
//! test walks each process entry through the resolver, the bundler and the
//! code generator, and writes a JSON report (`W3COS_CODE_OSS_REPORT`, default
//! `target/code-oss-aot.json`) with the module counts and every blocker.
//!
//! ```sh
//! W3COS_CODE_OSS=/path/to/vscode cargo test -p w3cos-compiler --test code_oss_graph -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};

/// Process entries in `out/`: Electron main, the workbench renderer and
/// its preload.
const ENTRIES: &[(&str, &str)] = &[
    ("main", "out/main.js"),
    (
        "preload",
        "out/vs/base/parts/sandbox/electron-browser/preload.js",
    ),
    (
        "workbench",
        "out/vs/code/electron-browser/workbench/workbench.js",
    ),
];

fn sweep(root: &Path, name: &str, entry: &Path) -> serde_json::Value {
    use w3cos_compiler::esm_resolver::{EsmBundle, EsmResolver};
    let started = std::time::Instant::now();
    let resolver = EsmResolver::new(root.to_path_buf());
    let parsed = match resolver.parse_graph_from_entry(entry) {
        Ok(parsed) => parsed,
        Err(error) => {
            return serde_json::json!({ "entry": name, "stage": "graph", "error": format!("{error:#}") });
        }
    };
    let modules = parsed.modules.len();
    let bundle = EsmBundle::build(&parsed, &resolver, entry);
    let unresolved: Vec<String> = bundle.unresolved.iter().map(|u| u.to_string()).collect();
    let codegen = if unresolved.is_empty() {
        match w3cos_compiler::esm_codegen::try_generate_with_bodies(&bundle) {
            Ok(code) => serde_json::json!({ "ok": true, "rust_bytes": code.len() }),
            Err(error) => serde_json::json!({ "ok": false, "error": error.to_string() }),
        }
    } else {
        serde_json::json!({ "ok": false, "error": "skipped: unresolved bindings" })
    };
    let report = serde_json::json!({
        "entry": name,
        "modules": modules,
        "imports": parsed.total_imports(),
        "exports": parsed.total_exports(),
        "symbols": bundle.symbol_count(),
        "unresolved_count": unresolved.len(),
        "unresolved": unresolved,
        "codegen": codegen,
        "seconds": started.elapsed().as_secs_f64(),
    });
    println!(
        "[code-oss] {name}: modules={modules} unresolved={} codegen_ok={}",
        report["unresolved_count"], report["codegen"]["ok"]
    );
    report
}

#[test]
#[ignore = "requires a transpiled Code-OSS checkout in W3COS_CODE_OSS"]
fn code_oss_entries_compile() {
    let root = PathBuf::from(std::env::var("W3COS_CODE_OSS").expect("set W3COS_CODE_OSS"));
    let mut reports = Vec::new();
    for (name, rel) in ENTRIES {
        let entry = root.join(rel);
        assert!(entry.is_file(), "missing {}", entry.display());
        reports.push(sweep(&root, name, &entry));
    }
    let out = std::env::var("W3COS_CODE_OSS_REPORT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("../../target/code-oss-aot.json"));
    std::fs::write(&out, serde_json::to_string_pretty(&reports).unwrap()).unwrap();
    println!("[code-oss] report: {}", out.display());
}
