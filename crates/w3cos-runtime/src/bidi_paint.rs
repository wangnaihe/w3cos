//! Preserve inline backgrounds while shaping compatible bidi fragments as one run.
use std::borrow::Cow;

use crate::{layout::LayoutRect, paint_artifact::PaintArtifact, table_paint::ReplayNode};
use w3cos_std::{
    Color,
    component::ComponentKind,
    style::{Position, Style},
};

type InputNode<'a> = (usize, LayoutRect, &'a ComponentKind, &'a Style);
const LOGICAL_ORDER: &str = "--w3cos-internal-bidi-logical-order";

fn foreground(style: &Style) -> Style {
    let mut style = style.clone();
    style.background = Color::TRANSPARENT;
    style.background_image = None;
    if let Some(properties) = &mut style.custom_properties {
        properties.remove(LOGICAL_ORDER);
    }
    style
}

fn eligible(kind: &ComponentKind, style: &Style) -> bool {
    matches!(kind, ComponentKind::Text { .. })
        && style.position == Position::Static
        && style.float == w3cos_std::style::Float::None
        && style.opacity == 1.0
        && style.transform.is_identity()
        && style.box_shadow.is_none()
        && style
            .filter
            .as_deref()
            .is_none_or(|value| value.trim().eq_ignore_ascii_case("none"))
        && style.padding == w3cos_std::style::Edges::ZERO
        && style.margin == w3cos_std::style::Edges::ZERO
        && style.letter_spacing == 0.0
        && style.word_spacing == 0.0
        && style.text_indent == w3cos_std::style::Dimension::Px(0.0)
        && style.outline_width == 0.0
        && style.border_width == 0.0
        && [
            style.border_top_width,
            style.border_right_width,
            style.border_bottom_width,
            style.border_left_width,
        ]
        .into_iter()
        .all(|width| width.unwrap_or(0.0) == 0.0)
        && style
            .custom_properties
            .as_ref()
            .is_some_and(|properties| properties.contains_key(LOGICAL_ORDER))
}

pub(crate) fn replay<'a>(nodes: &[InputNode<'a>], artifact: &PaintArtifact) -> Vec<ReplayNode<'a>> {
    let mut output = Vec::with_capacity(nodes.len());
    let mut cursor = 0;
    while cursor < nodes.len() {
        let (index, rect, kind, style) = nodes[cursor];
        if !eligible(kind, style) {
            output.push(ReplayNode {
                index,
                rect,
                kind: Cow::Borrowed(kind),
                style: Cow::Borrowed(style),
            });
            cursor += 1;
            continue;
        }
        let normalized = foreground(style);
        let parent = artifact.nodes[index].parent;
        let mut end = cursor + 1;
        if eligible(kind, style) {
            while end < nodes.len() {
                let (next, next_rect, next_kind, next_style) = nodes[end];
                if !eligible(next_kind, next_style)
                    || artifact.nodes[next].parent != parent
                    || artifact.node_properties[next] != artifact.node_properties[index]
                    || next_rect.y != rect.y
                    || next_rect.height != rect.height
                    || foreground(next_style) != normalized
                {
                    break;
                }
                end += 1;
            }
        }
        let mut visual: Vec<_> = (cursor..end).collect();
        visual.sort_by(|left, right| nodes[*left].1.x.total_cmp(&nodes[*right].1.x));
        let contiguous = visual.windows(2).all(|pair| {
            let left = nodes[pair[0]].1;
            let right = nodes[pair[1]].1;
            (left.x + left.width - right.x).abs() <= f32::EPSILON * right.x.abs().max(1.0)
        });
        if end - cursor > 1 && contiguous {
            // Backgrounds retain logical tree order and their individual boxes.
            for &(index, rect, _, style) in &nodes[cursor..end] {
                let mut background = style.clone();
                background.text_decoration = w3cos_std::style::TextDecoration::None;
                output.push(ReplayNode {
                    index,
                    rect,
                    kind: Cow::Owned(ComponentKind::Box),
                    style: Cow::Owned(background),
                });
            }
            let mut content = String::new();
            let (first, mut union, _, _) = nodes[visual[0]];
            for member in visual {
                let (_, rect, kind, _) = nodes[member];
                if let ComponentKind::Text { content: fragment } = kind {
                    content.push_str(fragment);
                }
                union.width = rect.x + rect.width - union.x;
            }
            output.push(ReplayNode {
                index: first,
                rect: union,
                kind: Cow::Owned(ComponentKind::Text { content }),
                style: Cow::Owned(normalized),
            });
        } else {
            for &(index, rect, kind, style) in &nodes[cursor..end] {
                output.push(ReplayNode {
                    index,
                    rect,
                    kind: Cow::Borrowed(kind),
                    style: Cow::Borrowed(style),
                });
            }
        }
        cursor = end;
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paint_artifact::PaintNode;

    fn fixture() -> (PaintArtifact, Vec<LayoutRect>) {
        let mut nodes = vec![PaintNode {
            kind: ComponentKind::Row,
            style: Style::default(),
            parent: None,
            sticky_counter_signal: None,
        }];
        for (rank, content) in ["a", "b", "c"].into_iter().enumerate() {
            let mut style = Style::default();
            style.display = w3cos_std::style::Display::Inline;
            style.custom_properties = Some(std::collections::HashMap::from([(
                LOGICAL_ORDER.to_string(),
                rank.to_string(),
            )]));
            if rank == 1 {
                style.background = Color::WHITE;
            }
            nodes.push(PaintNode {
                kind: ComponentKind::Text {
                    content: content.to_string(),
                },
                style,
                parent: Some(0),
                sticky_counter_signal: None,
            });
        }
        let rects: Vec<_> = [0.0, 10.0, 5.0, 0.0]
            .into_iter()
            .map(|x| LayoutRect {
                x,
                y: 0.0,
                width: 5.0,
                height: 10.0,
            })
            .collect();
        let layouts: Vec<_> = rects
            .iter()
            .copied()
            .enumerate()
            .map(|(index, rect)| (rect, index))
            .collect();
        (PaintArtifact::build(nodes, &layouts, 1), rects)
    }

    #[test]
    fn backgrounds_remain_logical_and_compatible_text_shapes_in_visual_order() {
        let (artifact, rects) = fixture();
        let input: Vec<_> = (1..4)
            .map(|index| {
                (
                    index,
                    rects[index],
                    &artifact.nodes[index].kind,
                    &artifact.nodes[index].style,
                )
            })
            .collect();
        let output = replay(&input, &artifact);
        assert_eq!(output.len(), 4);
        assert_eq!(
            output[..3]
                .iter()
                .map(|node| node.index)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(output[1].style.background, Color::WHITE);
        assert!(
            matches!(output[3].kind.as_ref(), ComponentKind::Text { content } if content == "cba")
        );
        assert_eq!(output[3].rect.width, 15.0);
        assert_eq!(output[3].style.background, Color::TRANSPARENT);
        assert_eq!(artifact.nodes[2].style.background, Color::WHITE);
    }

    #[test]
    fn different_foreground_styles_are_not_merged() {
        let (mut artifact, rects) = fixture();
        artifact.nodes[2].style.color = Color::rgb(255, 0, 0);
        let input: Vec<_> = (1..4)
            .map(|index| {
                (
                    index,
                    rects[index],
                    &artifact.nodes[index].kind,
                    &artifact.nodes[index].style,
                )
            })
            .collect();
        let output = replay(&input, &artifact);
        assert_eq!(output.len(), 3);
        assert!(
            output
                .iter()
                .all(|node| matches!(&node.kind, Cow::Borrowed(_)))
        );
    }
}
