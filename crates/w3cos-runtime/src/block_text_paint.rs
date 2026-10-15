//! Replay folded block text as block decoration followed by inline foreground.
//! DOM lowering may combine a paragraph box and its text into one client; this
//! must not combine their distinct CSS2 Appendix E paint phases.
use crate::{layout::LayoutRect, paint_artifact::PaintArtifact, table_paint::ReplayNode};
use std::borrow::Cow;
use w3cos_std::style::Display;
use w3cos_std::{ComponentKind, Style};

pub(crate) fn replay<'a>(
    nodes: &[(usize, LayoutRect, &'a ComponentKind, &'a Style)],
    artifact: &PaintArtifact,
) -> Vec<ReplayNode<'a>> {
    let mut output = Vec::with_capacity(nodes.len());
    let mut foregrounds = Vec::new();
    for &(index, rect, kind, style) in nodes {
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
