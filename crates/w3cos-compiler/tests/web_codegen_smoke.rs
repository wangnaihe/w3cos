//! Web emit smoke — same TSX/CSS as native/mobile must compile to HTML.

use std::path::Path;

fn repo_native_shell() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../apps/logidesk-native")
}

#[test]
fn test_ios_web_emit_has_actions_and_binds() {
    let demo = repo_native_shell();
    let tsx = demo.join("test-ios.tsx");
    if !tsx.is_file() {
        eprintln!("skip: {} not in checkout", tsx.display());
        return;
    }
    let out = tempfile::tempdir().expect("tempdir");
    w3cos_compiler::compile_web_from_file(&tsx, out.path())
        .unwrap_or_else(|e| panic!("web compile: {e}"));

    let html = std::fs::read_to_string(out.path().join("index.html")).unwrap();
    let js = std::fs::read_to_string(out.path().join("app.js")).unwrap();
    let css = std::fs::read_to_string(out.path().join("styles.css")).unwrap();

    assert!(html.contains("W3C OS iOS Lab"), "title text in html");
    assert!(
        html.contains("data-action=\"increment:taps\""),
        "tap action"
    );
    assert!(html.contains("data-action=\"fetch:GET:"), "fetch action");
    assert!(html.contains("data-bind=\"route\""), "route bind");
    assert!(js.contains("function executeAction"), "runtime");
    assert!(js.contains("history.pushState"), "history in runtime");
    assert!(css.contains("box-sizing"), "base reset css");
}

#[test]
fn native_and_web_share_merged_gap_padding() {
    let src = r#"import { Column, Text } from "@w3cos/std"
import "./theme.css"
export default
<Column className="box" style={{ gap: 8 }}>
  <Text>Hi</Text>
</Column>"#;
    let css = ".box { padding: 20; gap: 16; background: #112233; }";
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("theme.css"), css).unwrap();
    std::fs::write(dir.path().join("app.tsx"), src).unwrap();

    let out_web = tempfile::tempdir().unwrap();
    w3cos_compiler::compile_web_from_file(&dir.path().join("app.tsx"), out_web.path()).unwrap();
    let html = std::fs::read_to_string(out_web.path().join("index.html")).unwrap();
    assert!(
        html.contains("padding:20px") || html.contains("padding: 20px"),
        "merged padding in inline: {html}"
    );
    assert!(
        html.contains("gap:16px") || html.contains("gap: 16px") || html.contains("gap:8px"),
        "gap in inline: {html}"
    );

    let out_native = tempfile::tempdir().unwrap();
    w3cos_compiler::compile_from_file(&dir.path().join("app.tsx"), out_native.path()).unwrap();
    let main_rs = std::fs::read_to_string(out_native.path().join("src/main.rs")).unwrap();
    assert!(
        main_rs.contains("padding: Edges::all(20_f32)"),
        "native merged padding: {main_rs}"
    );
    assert!(
        main_rs.contains("gap: 16_f32") || main_rs.contains("gap: 8_f32"),
        "native gap: {main_rs}"
    );
}

#[test]
fn web_emit_preserves_overlay_position_and_visual_styles() {
    let src = r#"import { Column, Text } from "@w3cos/std"
import "./theme.css"
export default
<Column className="screen">
  <Column className="drawer">
    <Text className="eyebrow">WORKFLOWS</Text>
  </Column>
</Column>"#;
    let css = r#"
.screen { position: relative; }
.drawer {
  position: absolute;
  top: 0;
  right: 0;
  bottom: 0;
  left: 0;
  z-index: 100;
  box-shadow: 0 3 7 #dce3ec;
}
.eyebrow { letter-spacing: 3; }
"#;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("theme.css"), css).unwrap();
    std::fs::write(dir.path().join("app.tsx"), src).unwrap();

    let out = tempfile::tempdir().unwrap();
    w3cos_compiler::compile_web_from_file(&dir.path().join("app.tsx"), out.path()).unwrap();
    let html = std::fs::read_to_string(out.path().join("index.html")).unwrap();

    assert!(
        html.contains("position:absolute"),
        "absolute positioning: {html}"
    );
    assert!(html.contains("top:0px"), "top inset: {html}");
    assert!(html.contains("right:0px"), "right inset: {html}");
    assert!(html.contains("bottom:0px"), "bottom inset: {html}");
    assert!(html.contains("left:0px"), "left inset: {html}");
    assert!(html.contains("z-index:100"), "stacking order: {html}");
    assert!(html.contains("box-shadow:0 3 7 #dce3ec"), "shadow: {html}");
    assert!(
        html.contains("letter-spacing:3px"),
        "letter spacing: {html}"
    );
}
