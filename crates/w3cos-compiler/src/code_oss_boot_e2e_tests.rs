//! Code-OSS bootstrap patterns (M3 item 23): `createRequire` from
//! `node:module` (JSON, CommonJS, a missing file, builtins), the module
//! hooks Code-OSS installs, and the builtins its Electron main graph loads
//! (`v8`, `perf_hooks`, `async_hooks`, `diagnostics_channel`, `readline`,
//! `dns`, `http`/`https`/`tls`, `tty`, `console`). The fixture in
//! `fixtures/code_oss_boot/` must print what Node 20.19 prints.

use crate::cjs_e2e_tests::{compile_and_run, write_fixture};

const FILES: &[(&str, &str)] = &[
    (
        "package.json",
        include_str!("fixtures/code_oss_boot/package.json"),
    ),
    (
        "data.json",
        include_str!("fixtures/code_oss_boot/data.json"),
    ),
    (
        "helper.cjs",
        include_str!("fixtures/code_oss_boot/helper.cjs"),
    ),
    ("main.js", include_str!("fixtures/code_oss_boot/main.js")),
];

/// `node main.js` (Node 20.19) in the fixture directory.
const NODE: &str = r#"json Code - OSS 2 true
cjs 1 object true
missing MODULE_NOT_FOUND
module function function true true
lookup function true /a/b/node_modules
builtin true true true false false true
hooks function
require builtin function a/b
v8 number number function true
perf number function function
als undefined 7 undefined
resource 5 number
dc false false
dc2 true w3cos:test:1 true false true
http Not Found true function true 443 https:
tls TLSv1.2 auto function function
tty false function
console function
dns null 127.0.0.1 4
dns2 null 127.0.0.1
readline ["alpha","beta","gamma"]
"#;

#[test]
fn code_oss_bootstrap_builtins_match_node() {
    let root = write_fixture("w3cos_code_oss_boot", FILES);
    let lines = compile_and_run(&root, &root.join("main.js"), "w3cos_code_oss_boot");
    let expected: Vec<&str> = NODE.lines().collect();
    assert_eq!(lines, expected);
    std::fs::remove_dir_all(root).ok();
}
