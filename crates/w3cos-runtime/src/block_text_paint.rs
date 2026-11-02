//! Replay folded block text as block decoration followed by inline foreground.
//! DOM lowering may combine a paragraph box and its text into one client; this
//! must not combine their distinct CSS2 Appendix E paint phases.
use crate::{layout::LayoutRect, paint_artifact::PaintArtifact, table_paint::ReplayNode};
use std::borrow::Cow;
use w3cos_std::style::Display;
use w3cos_std::{ComponentKind, Style};

/// Style marker of a text piece to paint rotated (see `is_vertical_text`).
pub(crate) const VERTICAL_TEXT: &str = "--w3cos-internal-vertical-text";

pub(crate) fn replay<'a>(
    nodes: &[(usize, LayoutRect, &'a ComponentKind, &'a Style)],
    artifact: &PaintArtifact,
) -> Vec<ReplayNode<'a>> {
    let mut output = Vec::with_capacity(nodes.len());
    let mut foregrounds = Vec::new();
    let mut expanded = Vec::new();
    for &(index, rect, kind, style) in nodes {
        let pieces = artifact.text_fragments_of(index);
        if !pieces.is_empty() && matches!(kind, ComponentKind::Text { .. }) {
            if expanded.contains(&index) {
                // An earlier replay split this client; its pieces are out.
                continue;
            }
            expanded.push(index);
            // Blink paints each NGPhysicalTextFragment of a LayoutText in
            // fragment order at the text's own paint position. The pieces
            // keep the client identity; the frame may have offset the node
            // (scroll, animation), so carry that delta onto each piece.
            let principal = artifact.rect_by_index.get(index).copied().flatten();
            let (dx, dy) = principal.map_or((0.0, 0.0), |p| (rect.x - p.x, rect.y - p.y));
            let mut piece_style = style.clone();
            // A piece is exactly one line's run: never re-break it.
            if piece_style.white_space == w3cos_std::style::WhiteSpace::Normal {
                piece_style.white_space = w3cos_std::style::WhiteSpace::NoWrap;
            }
            if matches!(
                piece_style.display,
                Display::TableCell | Display::TableCaption
            ) {
                // The fragment is already the font content box. Leaving the
                // cell's display and padding would center and inset the
                // glyphs inside that box a second time.
                piece_style.display = Display::Inline;
                piece_style.padding = w3cos_std::style::Edges::ZERO;
                piece_style.border_width = 0.0;
                piece_style.border_top_width = None;
                piece_style.border_right_width = None;
                piece_style.border_bottom_width = None;
                piece_style.border_left_width = None;
            }
            if let Some(properties) = piece_style.custom_properties.as_mut() {
                properties.remove("--w3cos-internal-float-line-bands");
            }
            if artifact.is_vertical_text(index) {
                // A piece of a vertical flow was laid out as a horizontal
                // line in transposed space: paint turns it a quarter turn
                // clockwise inside its physical rect.
                piece_style.writing_mode = w3cos_std::style::WritingMode::HorizontalTb;
                piece_style
                    .custom_properties
                    .get_or_insert_with(Default::default)
                    .insert(VERTICAL_TEXT.to_string(), String::new());
            }
            for (piece, text) in pieces {
                output.push(ReplayNode {
                    index,
                    rect: LayoutRect {
                        x: piece.x + dx,
                        y: piece.y + dy,
                        ..*piece
                    },
                    kind: Cow::Owned(ComponentKind::Text {
                        content: text.clone(),
                    }),
                    style: Cow::Owned(piece_style.clone()),
                });
            }
            continue;
        }
        let mut key = artifact.paint_order_key(index).to_vec();
        // Float, atomic-inline and positioned participants already have an
        // atomic group key. Do not lift their text into the enclosing context.
        let split = matches!(kind, ComponentKind::Text { .. })
            && style.display == Display::Block
            && key.last().is_some_and(|level| level.0 == 1);
        if split {
            let mut decoration = style.clone();
            decoration.outline_width = 0.0;
            output.push(ReplayNode {
                index,
                rect,
                kind: Cow::Owned(ComponentKind::Box),
                style: Cow::Owned(decoration),
            });
            let mut text = style.clone();
            text.background = w3cos_std::Color::TRANSPARENT;
            text.background_image = None;
            text.box_shadow = None;
            text.box_shadows.clear();
            text.backdrop_blur = None;
            // Widths remain: text inset and background origin use the box.
            text.border_color = w3cos_std::Color::TRANSPARENT;
            text.border_top_color = Some(w3cos_std::Color::TRANSPARENT);
            text.border_right_color = Some(w3cos_std::Color::TRANSPARENT);
            text.border_bottom_color = Some(w3cos_std::Color::TRANSPARENT);
            text.border_left_color = Some(w3cos_std::Color::TRANSPARENT);
            key.last_mut().unwrap().0 = 3;
            foregrounds.push((
                key,
                ReplayNode {
                    index,
                    rect,
                    kind: Cow::Borrowed(kind),
                    style: Cow::Owned(text),
                },
            ));
        } else {
            output.push(ReplayNode {
                index,
                rect,
                kind: Cow::Borrowed(kind),
                style: Cow::Borrowed(style),
            });
        }
    }
    foregrounds.sort_by(|a, b| a.0.cmp(&b.0));
    // Preserve existing table/bidi replay order. Only insert the separated
    // foregrounds at their effective inline phase, retaining the same client
    // identity, geometry, property tree and hit-test source.
    for (key, node) in foregrounds {
        let position = output
            .iter()
            .position(|other| artifact.paint_order_key(other.index) > key.as_slice())
            .unwrap_or(output.len());
        output.insert(position, node);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paint_artifact::PaintNode;

    #[test]
    fn broken_text_paints_its_pieces_in_place() {
        let inline = Style {
            display: Display::Inline,
            ..Style::default()
        };
        let text = |content: &str| ComponentKind::Text {
            content: content.into(),
        };
        let nodes = [
            PaintNode {
                kind: ComponentKind::Box,
                style: Style::default(),
                parent: None,
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: text("XXX-XXX"),
                style: inline.clone(),
                parent: Some(0),
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: text("after"),
                style: inline.clone(),
                parent: Some(0),
                sticky_counter_signal: None,
            },
        ];
        let r = |x: f32, y: f32, width: f32| LayoutRect {
            x,
            y,
            width,
            height: 20.0,
        };
        let union = LayoutRect {
            x: 0.0,
            y: 0.0,
            width: 120.0,
            height: 40.0,
        };
        let cache = [
            (r(0.0, 0.0, 200.0), 0),
            (union, 1),
            (r(60.0, 20.0, 40.0), 2),
        ];
        let mut artifact = PaintArtifact::build(nodes, &cache, 1);
        let piece = |start, end, rect, text: &str| crate::layout_ng::TextFragment {
            index: 1,
            start,
            end,
            rect,
            text: text.into(),
            vertical: false,
        };
        artifact.set_text_fragments(&[
            piece(0, 4, r(40.0, 0.0, 80.0), "XXX-"),
            piece(4, 7, r(0.0, 20.0, 60.0), "XXX"),
        ]);
        let frame: Vec<_> = (0..3)
            .map(|i| {
                (
                    i,
                    artifact.rect_by_index[i].unwrap(),
                    &artifact.nodes[i].kind,
                    &artifact.nodes[i].style,
                )
            })
            .collect();
        let out = replay(&frame, &artifact);
        let got: Vec<_> = out
            .iter()
            .map(|n| {
                let content = match n.kind.as_ref() {
                    ComponentKind::Text { content } => content.clone(),
                    _ => String::new(),
                };
                (n.index, content, n.rect.x, n.rect.y, n.style.white_space)
            })
            .collect();
        use w3cos_std::style::WhiteSpace::{NoWrap, Normal};
        assert_eq!(
            got,
            vec![
                (0, String::new(), 0.0, 0.0, Normal),
                (1, "XXX-".into(), 40.0, 0.0, NoWrap),
                (1, "XXX".into(), 0.0, 20.0, NoWrap),
                (2, "after".into(), 60.0, 20.0, Normal),
            ]
        );
        // A scrolled/animated frame rect moves every piece with the node.
        let moved = [(1, LayoutRect { y: -5.0, ..union }, frame[1].2, frame[1].3)];
        let out = replay(&moved, &artifact);
        assert_eq!(
            out.iter().map(|n| n.rect.y).collect::<Vec<_>>(),
            vec![-5.0, 15.0]
        );
    }
}
