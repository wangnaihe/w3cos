use super::component_max_content_width;
use w3cos_std::style::{Dimension, Display, Float, Position, TextDirection, WhiteSpace};
use w3cos_std::{Component, ComponentKind};

/// Tab positions belong to the entire preformatted line, including preceding
/// atomic inlines. The returned content advances are shared by intrinsic
/// sizing and the Taffy child constraints, not a paint-only correction.
pub(super) fn advances(component: &Component) -> Option<Vec<Option<f32>>> {
    if component.style.white_space != WhiteSpace::Pre
        || component.style.direction != TextDirection::Ltr
        || !component.children.iter().any(|child|
            matches!(&child.kind, ComponentKind::Text { content } if content.contains('\t'))) {
        return None;
    }
    let mut widths = vec![None; component.children.len()];
    let mut cursor = 0.0;
    for (index, child) in component.children.iter().enumerate() {
        let style = &child.style;
        if style.display == Display::None
            || matches!(style.position, Position::Absolute | Position::Fixed)
        {
            continue;
        }
        if style.position != Position::Static || style.float != Float::None {
            return None;
        }
        let plain_text = child.children.is_empty() && style.display == Display::Inline;
        if let ComponentKind::Text { content } = &child.kind {
            if !plain_text
                || style.white_space != WhiteSpace::Pre
                || content.contains(['\n', '\r', '\u{2028}'])
            {
                return None;
            }
            if content.contains('\t') {
                if style.white_space != WhiteSpace::Pre
                    || style.width != Dimension::Auto
                    || style.min_width != Dimension::Auto
                    || style.max_width != Dimension::Auto
                {
                    return None;
                }
                let padding = style.padding_lengths();
                let margin = style.margin_lengths();
                let left_border = style.border_left_width.unwrap_or(style.border_width);
                let right_border = style.border_right_width.unwrap_or(style.border_width);
                let offset = cursor + margin.left + left_border + padding.left;
                let advance = crate::render_skia::preserved_tab_advance_in_block(
                    content,
                    style,
                    &component.style,
                    offset,
                )?;
                widths[index] = Some(
                    advance
                        + if style.box_sizing == w3cos_std::style::BoxSizing::BorderBox {
                            padding.left + padding.right
                        } else {
                            0.0
                        },
                );
                cursor += advance
                    + margin.left
                    + margin.right
                    + padding.left
                    + padding.right
                    + left_border
                    + right_border;
                continue;
            }
        } else if !matches!(
            style.display,
            Display::InlineBlock | Display::InlineFlex | Display::InlineTable
        ) {
            return None;
        }
        cursor += component_max_content_width(child);
    }
    Some(widths)
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use w3cos_std::Style;

    fn line() -> Component {
        let text_style = Style {
            display: WDisplay::Inline,
            white_space: WWhiteSpace::Pre,
            ..Style::default()
        };
        let atomic = Style {
            display: WDisplay::InlineBlock,
            width: WDim::Px(15.0),
            height: WDim::Px(15.0),
            ..text_style.clone()
        };
        Component::row(
            Style {
                display: WDisplay::Flex,
                white_space: WWhiteSpace::Pre,
                custom_properties: Some(HashMap::from([(
                    "--w3cos-internal-inline-formatting-context".into(),
                    "1".into(),
                )])),
                ..Style::default()
            },
            vec![
                Component::text(" ", text_style.clone()),
                Component::boxed(atomic.clone(), vec![]),
                Component::text("\t ", text_style.clone()),
                Component::boxed(atomic, vec![]),
                Component::text("   ", text_style),
            ],
        )
    }

    #[test]
    fn preformatted_tab_fragment_uses_shared_inline_origin() {
        let root = line();
        let prefix = component_max_content_width(&root.children[0]) + 15.0;
        let expected = crate::render_skia::preserved_tab_advance_in_block(
            "\t ",
            &root.children[2].style,
            &root.style,
            prefix,
        )
        .unwrap();
        assert_eq!(inline_fragment_shaped_advances(&root)[2], Some(expected));
    }

    #[test]
    fn preformatted_tab_max_content_includes_prior_atomic_advance() {
        let root = line();
        let space = component_max_content_width(&root.children[0]);
        let tab = crate::render_skia::preserved_tab_advance_in_block(
            "\t ",
            &root.children[2].style,
            &root.style,
            space + 15.0,
        )
        .unwrap();
        // Separate authored fragments retain their own LayoutUnit rounding;
        // three spaces need not equal three individually rounded space boxes.
        let trailing = component_max_content_width(&root.children[4]);
        let expected = text_layout::inline_layout_advance(space + trailing + 30.0 + tab);
        assert_eq!(component_max_content_width(&root), expected);
    }

    #[test]
    fn preformatted_tab_mixed_or_multiline_packets_keep_general_layout() {
        let mut root = line();
        root.children[0].style.white_space = WWhiteSpace::Normal;
        assert!(super::advances(&root).is_none());
        root = line();
        root.children[0].kind = ComponentKind::Text {
            content: "first\nsecond".into(),
        };
        assert!(super::advances(&root).is_none());
    }
}
