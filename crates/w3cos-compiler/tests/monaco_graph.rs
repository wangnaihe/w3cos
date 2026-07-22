//! Monaco Editor dependency-graph sweep.
//!
//! Requires `npm install` to have been run in `examples/monaco-editor/`.
//! Ignored by default (depends on node_modules); run explicitly with:
//!
//! ```sh
//! cargo test -p w3cos-compiler --test monaco_graph -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};

fn monaco_example_dir() -> PathBuf {
    // Tests run with CWD = crates/w3cos-compiler.
    let dir = Path::new("../../examples/monaco-editor");
    dir.canonicalize()
        .unwrap_or_else(|e| panic!("monaco example not found (run npm install there first): {e}"))
}

#[test]
#[ignore = "requires examples/monaco-editor/node_modules"]
fn monaco_editor_api_graph_resolves() {
    let example_dir = monaco_example_dir();
    let entry = example_dir.join("app.ts");
    assert!(entry.is_file(), "missing {}", entry.display());

    let resolver = w3cos_compiler::esm_resolver::EsmResolver::new(example_dir.clone());
    let graph = resolver
        .build_graph_from_entry(&entry)
        .expect("graph build must not fail on monaco editor.api");

    println!("[monaco] modules resolved: {}", graph.nodes.len());
    let total_imports: usize = graph.nodes.iter().map(|n| n.imports.len()).sum();
    println!("[monaco] static import edges: {total_imports}");
    let packages = graph.package_names();
    println!("[monaco] external packages: {packages:?}");

    // The full parse is the expensive part — it SWC-parses every module.
    let parsed = resolver
        .parse_graph_from_entry(&entry)
        .expect("parse must not fail on monaco editor.api");
    println!("[monaco] parsed modules: {}", parsed.modules.len());
    println!("[monaco] parsed imports: {}", parsed.total_imports());
    println!("[monaco] parsed exports: {}", parsed.total_exports());

    let bundle = w3cos_compiler::esm_resolver::EsmBundle::build(&parsed, &resolver, &entry);
    println!("[monaco] bundle symbols: {}", bundle.symbol_count());
    println!("[monaco] fully resolved: {}", bundle.is_fully_resolved());
    if !bundle.unresolved.is_empty() {
        println!("[monaco] unresolved bindings: {}", bundle.unresolved.len());
        for item in bundle.unresolved.iter().take(400) {
            println!("  - {item}");
        }
    }

    assert_eq!(packages, vec!["monaco-editor".to_string()]);
    assert!(
        graph.nodes.len() > 100,
        "monaco core should pull in hundreds of modules"
    );
}
