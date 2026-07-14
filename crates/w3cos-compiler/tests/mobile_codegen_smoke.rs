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

const SHOW_OVERLAY_TSX: &str = r#"
const open = signal(0)

export default
<Column>
  <Show when="open:1">
    <Row style={{ position: "absolute", width: "100%", height: "100%" }}>
      <Text>Drawer</Text>
    </Row>
  </Show>
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
        &w3cos_compiler::CompileOptions::default(),
    )
    .expect("write ios project");

    let main_rs = std::fs::read_to_string(dir.path().join("src/main.rs")).unwrap();
    assert!(main_rs.contains("run_mobile_app"));
    assert!(main_rs.contains("w3cos_std::viewport::InteractiveWidget::ResizesContent"));
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
        &w3cos_compiler::CompileOptions::default(),
    )
    .expect("write android project");

    let lib_rs = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
    assert!(lib_rs.contains("w3cos_app_run"));
    assert!(lib_rs.contains("android_main"));
    assert!(lib_rs.contains("set_interactive_widget"));
    let cargo = std::fs::read_to_string(dir.path().join("Cargo.toml")).unwrap();
    assert!(cargo.contains("w3cos-runtime"));
}

#[test]
fn mobile_android_devtools_cargo_feature() {
    let tree = parser::parse(COUNTER_TSX).expect("parse counter tsx");
    let dir = tempfile::tempdir().unwrap();
    mobile_codegen::write_mobile_project(
        &tree,
        &css_parser::Stylesheet::empty(),
        dir.path(),
        "android",
        false,
        "resizes-content",
        &w3cos_compiler::CompileOptions { devtools: true },
    )
    .expect("write android project");

    let cargo = std::fs::read_to_string(dir.path().join("Cargo.toml")).unwrap();
    assert!(
        cargo.contains(r#"features = ["devtools"]"#),
        "devtools feature missing: {cargo}"
    );
}

#[test]
fn mobile_show_single_child_preserves_overlay_geometry() {
    let tree = parser::parse(SHOW_OVERLAY_TSX).expect("parse Show overlay tsx");
    let dir = tempfile::tempdir().unwrap();
    mobile_codegen::write_mobile_project(
        &tree,
        &css_parser::Stylesheet::empty(),
        dir.path(),
        "ios",
        false,
        "resizes-content",
        &w3cos_compiler::CompileOptions::default(),
    )
    .expect("write ios project");

    let app_ui = std::fs::read_to_string(dir.path().join("src/app_ui.rs")).unwrap();
    assert!(app_ui.contains("let mut __show_comp"));
    assert!(app_ui.contains("__show_comp.style.display = Display::None"));
    assert!(!app_ui.contains("Component::column(__show_style"));
}
