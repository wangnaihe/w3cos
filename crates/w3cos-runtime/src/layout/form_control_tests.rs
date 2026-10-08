use super::*;

#[test]
fn button_border_box_intrinsic_includes_its_border_edges() {
    for sizing in [WBoxSizing::BorderBox, WBoxSizing::ContentBox] {
        let mut style = w3cos_dom::user_agent::html_default_style("button");
        style.box_sizing = sizing;
        let text = text_intrinsic_size("Apply", &style);
        let border = if sizing == WBoxSizing::BorderBox { 4.0 } else { 0.0 };
        let actual = button_intrinsic_size("Apply", &style);
        assert_eq!(actual, (text.0 + border, text.1 + border), "{sizing:?}");
    }
}

#[test]
fn html_text_input_intrinsic_size_uses_control_font_metrics() {
    let mut style = w3cos_dom::user_agent::html_default_style("input");
    style.custom_properties.get_or_insert_with(Default::default)
        .insert("--w3cos-internal-input-size".into(), "20".into());
    let input = Component::text_input("", "", style.clone());
    let face = skia_safe::FontMgr::default().match_family_style(
        "Arial", skia_safe::FontStyle::normal()).unwrap();
    let (_, metrics) = crate::skia_text_run::css_font(&face, style.font_size).metrics();
    assert_eq!(leaf_intrinsic_size(&input.kind, &style), (145.0, 15.0),
        "HTML size=20 intrinsic content box, metrics={metrics:?}");
    style.custom_properties.as_mut().unwrap()
        .insert("--w3cos-internal-input-size".into(), "5".into());
    assert!(leaf_intrinsic_size(&input.kind, &style).0 < 60.0,
        "HTML size must alter intrinsic width");
}

#[test]
fn text_control_exports_inner_font_baseline_not_bottom_border() {
    let text_style = w3cos_std::Style { display: WDisplay::Inline,
        font_family: Some("Arial".into()), font_size: 16.0,
        line_height: 1.2, line_height_is_normal: true, ..w3cos_std::Style::default() };
    let control_style = w3cos_dom::user_agent::html_default_style("input");
    let root = Component::row(w3cos_std::Style { display: WDisplay::Block,
        ..text_style.clone() }, vec![Component::text("Text", text_style.clone()),
        Component::text_input("", "", control_style.clone())]);
    let mut layouts = vec![
        (LayoutRect { x: 0.0, y: 0.0, width: 300.0, height: 30.0 }, 0),
        (LayoutRect { x: 0.0, y: 5.0, width: 40.0, height: 19.0 }, 1),
        (LayoutRect { x: 40.0, y: 0.0, width: 151.0, height: 19.0 }, 2),
    ];
    align_inline_block_last_line_baselines(&mut layouts, &root);
    let text_baseline = layouts[1].0.y + inline_font_content_ascent(&text_style);
    let control_baseline = layouts[2].0.y + control_style.padding_lengths().top
        + control_style.border_width + inline_font_content_ascent(&control_style);
    assert!((text_baseline - control_baseline).abs() < 0.01,
        "text={text_baseline}, control={control_baseline}; {layouts:?}");
}
