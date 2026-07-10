//! Mobile codegen smoke — iOS/Android project scaffolding from TSX.

use w3cos_compiler::{css_parser, mobile_codegen, parser};

const COUNTER_TSX: &str = r#"
const count = signal(0)

export default
<Column style={{ flexGrow: 1, padding: 32 }}>
  <Text style={{ fontSize: 24 }}>{count}</Text>
  <Button onClick="increment:count">+</Button>
</Column>
"#;

#[test]
fn mobile_ios_writes_main_rs() {
    let tree = parser::parse(COUNTER_TSX).expect("parse counter tsx");
    let dir = tempfile::tempdir().unwrap();
    mobile_codegen::write_mobile_project(
        &tree,
        &css_parser::Stylesheet::empty(),
        dir.path(),
        "ios",
        false,
        "resizes-content",
    )
    .expect("write ios project");

    let main_rs = std::fs::read_to_string(dir.path().join("src/main.rs")).unwrap();
    assert!(main_rs.contains("run_mobile_app"));
    let app_ui = std::fs::read_to_string(dir.path().join("src/app_ui.rs")).unwrap();
    assert!(app_ui.contains("ensure_signals"));
    assert!(app_ui.contains("button_with_click"));
    let cargo = std::fs::read_to_string(dir.path().join("Cargo.toml")).unwrap();
    assert!(cargo.contains("w3cos-mobile"));
}

#[test]
fn mobile_android_writes_lib_rs() {
    let tree = parser::parse(COUNTER_TSX).expect("parse counter tsx");
    let dir = tempfile::tempdir().unwrap();
    mobile_codegen::write_mobile_project(
        &tree,
        &css_parser::Stylesheet::empty(),
        dir.path(),
        "android",
        false,
        "resizes-content",
    )
    .expect("write android project");

    let lib_rs = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
    assert!(lib_rs.contains("w3cos_app_run"));
    assert!(lib_rs.contains("android_main"));
    assert!(lib_rs.contains("set_interactive_widget"));
}
