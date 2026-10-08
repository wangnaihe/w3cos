use super::*;

#[cfg(feature = "skia")]
#[test]
fn paragraph_inline_margin_restores_continuation_width_for_height() {
    let content = "This is a long piece of text that will wrap to multiple lines. ".repeat(12);
    let mut inline = paragraph(&content, 0.0).style;
    inline.margin.left = w3cos_std::style::Spacing::Px(100.0);
    let mut block = inline.clone();
    block.display = WDisplay::Block;
    block.margin.left = w3cos_std::style::Spacing::Px(0.0);
    block.text_indent = WDim::Px(100.0);
    assert_eq!(
        crate::render_skia::measure_skia_wrapped_text_height(&content, 684.0, &inline),
        crate::render_skia::measure_skia_wrapped_text_height(&content, 784.0, &block),
        "a first-fragment margin cannot constrain every continuation line"
    );
}

fn paragraph(content: &str, indent: f32) -> Component {
    Component::text(
        content,
        w3cos_std::Style {
            display: WDisplay::Inline,
            text_indent: WDim::Px(indent),
            custom_properties: Some(HashMap::from([(
                "--w3cos-internal-text-line-width".into(),
                "1".into(),
            )])),
            ..w3cos_std::Style::default()
        },
    )
}

#[test]
fn paragraph_max_content_reserves_first_line_indent_once() {
    let plain = paragraph("First cell", 0.0);
    let indented = paragraph("First cell", 20.0);
    assert_eq!(
        component_max_content_width(&indented),
        component_max_content_width(&plain) + 20.0
    );
    let mut inline = indented.clone();
    inline.style.custom_properties = None;
    assert_eq!(
        component_max_content_width(&inline),
        component_max_content_width(&plain),
        "an ordinary inline does not own its inherited line indent"
    );
    let mut margin = plain.clone();
    margin.style.margin.left = w3cos_std::style::Spacing::Px(20.0);
    assert_eq!(
        component_max_content_width(&margin),
        component_max_content_width(&indented),
        "an already-lowered anonymous-cell margin is counted only once"
    );
}

#[test]
fn paragraph_min_content_adds_indent_only_to_first_segment() {
    let first = paragraph("First", 0.0);
    let last = paragraph("cell", 0.0);
    let expected =
        (component_min_content_width(&first) + 20.0).max(component_min_content_width(&last));
    assert_eq!(
        component_min_content_width(&paragraph("First cell", 20.0)),
        expected
    );
}

#[test]
fn paragraph_hard_break_does_not_repeat_or_subtract_later_line_indent() {
    for indent in [-200.0, 20.0] {
        let mut text = paragraph("X\nA much longer second line", indent);
        text.style.white_space = WWhiteSpace::Pre;
        let mut later = paragraph("A much longer second line", 0.0);
        later.style.white_space = WWhiteSpace::Pre;
        assert_eq!(
            component_max_content_width(&text),
            component_max_content_width(&later)
        );
    }
    let mut text = paragraph("\nFirst cell", 20.0);
    text.style.white_space = WWhiteSpace::PreLine;
    let plain = paragraph("First cell", 0.0);
    assert_eq!(
        component_min_content_width(&text),
        component_min_content_width(&plain)
    );
}
