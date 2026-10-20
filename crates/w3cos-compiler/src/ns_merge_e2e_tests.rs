//! TypeScript `function f` + namespace merge emit: `f || (f = {})`.
use crate::cjs_e2e_tests::{compile_and_run, write_fixture};

const FILES: &[(&str, &str)] = &[
    (
        "package.json",
        include_str!("fixtures/ns_merge/package.json"),
    ),
    ("main.js", include_str!("fixtures/ns_merge/main.js")),
];

#[test]
fn typescript_namespace_merge_assigns_function_binding() {
    let root = write_fixture("w3cos_ns_merge", FILES);
    let lines = compile_and_run(&root, &root.join("main.js"), "w3cos_ns_merge");
    assert_eq!(lines, vec!["ns 2 7".to_string()]);
    std::fs::remove_dir_all(root).ok();
}
