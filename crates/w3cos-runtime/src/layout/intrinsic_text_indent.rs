//! Intrinsic contributions for a text leaf that owns a paragraph's line box.
//! Ordinary inline descendants inherit text-indent but must not reserve it.
use super::*;

pub(super) fn indent(style: &w3cos_std::Style) -> f32 {
    if !style
        .custom_properties
        .as_ref()
        .is_some_and(|properties| properties.contains_key("--w3cos-internal-text-line-width"))
        && !matches!(
            style.display,
            WDisplay::Block
                | WDisplay::FlowRoot
                | WDisplay::ListItem
                | WDisplay::TableCell
                | WDisplay::TableCaption
        )
    {
        return 0.0;
    }
    // Percentage contributions have an indefinite basis during intrinsic
    // sizing. Viewport-relative dimensions are resolved by the layout tree.
    match style.text_indent {
        WDim::Ch(value) => value * css_ch_advance(style),
        _ => style.resolved_text_indent(0.0, 0.0, 0.0),
    }
}

pub(super) fn max_content(content: &str, style: &w3cos_std::Style, width: f32) -> f32 {
    let indent = indent(style);
    if indent == 0.0 || !text_establishes_inline_line(content, style) {
        return width;
    }
    let lines =
        text_layout::wrap_text_with_run_width(content, f32::MAX / 4.0, style.white_space, |line| {
            text_intrinsic_size(line, style).0
        });
    if lines.len() <= 1 {
        return (width + indent).max(0.0);
    }
    let authored = text_layout::authored_line_styles(content, style, &lines);
    lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let style = authored.as_ref().map_or(style, |styles| &styles[index]);
            let padding = style.padding_lengths();
            let advance = text_intrinsic_size(line, style).0 - padding.left - padding.right;
            advance + if index == 0 { indent } else { 0.0 }
        })
        .fold(0.0_f32, f32::max)
}

pub(super) fn first_segment_indent(content: &str, style: &w3cos_std::Style) -> f32 {
    let normalized = text_layout::prepare_text_for_white_space(content, style.white_space);
    let prefix = normalized
        .split(|character: char| !character.is_whitespace())
        .next()
        .unwrap_or("");
    if prefix.contains(['\n', '\r', '\u{2028}']) {
        0.0
    } else {
        indent(style)
    }
}
