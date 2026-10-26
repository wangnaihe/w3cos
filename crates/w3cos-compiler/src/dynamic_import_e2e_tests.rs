//! Literal `import("./x.js")` is linked at compile time and evaluated on demand.

use crate::cjs_e2e_tests::{compile_and_run, write_fixture};

const FILES: &[(&str, &str)] = &[
    (
        "package.json",
        include_str!("fixtures/dynamic_import/package.json"),
    ),
    ("lazy.js", include_str!("fixtures/dynamic_import/lazy.js")),
    ("main.js", include_str!("fixtures/dynamic_import/main.js")),
];

#[test]
fn literal_dynamic_import_evaluates_lazy_module() {
    let root = write_fixture("w3cos_dynamic_import", FILES);
    let lines = compile_and_run(&root, &root.join("main.js"), "w3cos_dynamic_import");
    assert_eq!(lines, vec!["dyn 41 42".to_string()]);
    std::fs::remove_dir_all(root).ok();
}

/// `"w3cos": { "exclude": [...] }` keeps a lazily imported package out of the
/// build; importing it (or any module missing from the graph) rejects with
/// `ERR_MODULE_NOT_FOUND` instead of resolving to `undefined`.
const EXCLUDED: &[(&str, &str)] = &[
    (
        "package.json",
        r#"{ "name": "w3cos-dyn-exclude", "version": "0.1.0", "type": "module", "main": "main.js",
  "w3cos": { "exclude": ["heavy"] } }"#,
    ),
    (
        "node_modules/heavy/package.json",
        r#"{ "name": "heavy", "version": "1.0.0", "main": "index.js" }"#,
    ),
    (
        "node_modules/heavy/index.js",
        "console.log('heavy evaluated'); exports.big = 1;\n",
    ),
    ("lazy.js", "export const lazy = 'lazy';\n"),
    (
        "main.js",
        r#"const report = (label) => [
  (ns) => console.log(label, 'loaded', typeof ns),
  (error) => console.log(label, error instanceof Error, error.code, error.message),
];
await import('./lazy.js').then(...report('linked'));
await import('heavy').then(...report('excluded'));
const name = ['missing', 'js'].join('.');
await import('./' + name).then(...report('computed'));
console.log('done');
"#,
    ),
];

#[test]
fn excluded_and_missing_dynamic_imports_reject_with_module_not_found() {
    let root = write_fixture("w3cos_dynamic_import_exclude", EXCLUDED);
    let lines = compile_and_run(&root, &root.join("main.js"), "w3cos_dynamic_import_exclude");
    assert_eq!(
        lines,
        [
            "linked loaded object",
            "excluded true ERR_MODULE_NOT_FOUND Cannot find module 'heavy'",
            "computed true ERR_MODULE_NOT_FOUND Cannot find module './missing.js'",
            "done",
        ],
        "{lines:#?}"
    );
    std::fs::remove_dir_all(root).ok();
}
