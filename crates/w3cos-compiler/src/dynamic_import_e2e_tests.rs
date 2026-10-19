//! Literal `import("./x.js")` is linked at compile time and evaluated on demand.

use crate::cjs_e2e_tests::{compile_and_run, write_fixture};

const FILES: &[(&str, &str)] = &[
    (
        "package.json",
        include_str!("fixtures/dynamic_import/package.json"),
    ),
    (
        "lazy.js",
        include_str!("fixtures/dynamic_import/lazy.js"),
    ),
    ("main.js", include_str!("fixtures/dynamic_import/main.js")),
];

#[test]
fn literal_dynamic_import_evaluates_lazy_module() {
    let root = write_fixture("w3cos_dynamic_import", FILES);
    let lines = compile_and_run(&root, &root.join("main.js"), "w3cos_dynamic_import");
    assert_eq!(lines, vec!["dyn 41 42".to_string()]);
    std::fs::remove_dir_all(root).ok();
}
