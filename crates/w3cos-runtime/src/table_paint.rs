//! CSS2 Appendix E table replay: backgrounds, borders, then cell content.
use std::borrow::Cow;
use std::collections::HashMap;

use w3cos_std::color::Color;
use w3cos_std::component::ComponentKind;
use w3cos_std::style::{Display, Float, Position, Style};

use crate::layout::LayoutRect;
use crate::paint_artifact::PaintArtifact;

type InputNode<'a> = (usize, LayoutRect, &'a ComponentKind, &'a Style);

pub(crate) struct ReplayNode<'a> {
    pub index: usize,
    pub rect: LayoutRect,
    pub kind: Cow<'a, ComponentKind>,
    pub style: Cow<'a, Style>,
}

fn layer(display: Display) -> Option<u8> {
    Some(match display {
        Display::Table | Display::InlineTable => 0,
        Display::TableColumnGroup => 1,
        Display::TableColumn => 2,
        Display::TableRowGroup | Display::TableHeaderGroup | Display::TableFooterGroup => 3,
        Display::TableRow => 4,
        Display::TableCell => 5,
        _ => return None,
    })
}

fn atomic(style: &Style) -> bool {
    style.position != Position::Static
        || style.float != Float::None
        || style.opacity < 1.0
        || !style.transform.is_identity()
        || style
            .filter
            .as_deref()
            .is_some_and(|value| !value.trim().eq_ignore_ascii_case("none"))
}

fn owner(artifact: &PaintArtifact, index: usize, style: &Style) -> Option<usize> {
    layer(style.display)?;
    if !style.border_collapse {
        return None;
    }
    if matches!(style.display, Display::Table | Display::InlineTable) {
        return Some(index);
    }
    if atomic(style) {
        return None;
    }
    let mut parent = artifact.nodes.get(index)?.parent;
    while let Some(index) = parent {
        let node = artifact.nodes.get(index)?;
        if matches!(node.style.display, Display::Table | Display::InlineTable) {
            return node.style.border_collapse.then_some(index);
        }
        if atomic(&node.style) {
            return None;
        }
        parent = node.parent;
    }
    None
}

fn clear_borders(style: &mut Style) {
    // Retain border widths: background origin and text offsets still need them.
    style.border_color = Color::TRANSPARENT;
    style.border_top_color = Some(Color::TRANSPARENT);
    style.border_right_color = Some(Color::TRANSPARENT);
    style.border_bottom_color = Some(Color::TRANSPARENT);
    style.border_left_color = Some(Color::TRANSPARENT);
}

fn clear_background(style: &mut Style) {
    style.background = Color::TRANSPARENT;
    style.background_image = None;
    style.box_shadow = None;
}

pub(crate) fn replay<'a>(nodes: &[InputNode<'a>], artifact: &PaintArtifact) -> Vec<ReplayNode<'a>> {
    let owners = nodes
        .iter()
        .map(|(index, _, _, style)| owner(artifact, *index, style))
        .collect::<Vec<_>>();
    let mut groups = HashMap::<usize, Vec<usize>>::new();
    for (position, owner) in owners.iter().enumerate() {
        if let Some(owner) = owner {
            groups.entry(*owner).or_default().push(position);
        }
    }
    let mut output =
        Vec::with_capacity(nodes.len() + groups.values().map(|g| g.len() * 2).sum::<usize>());
    for (position, &(index, rect, kind, style)) in nodes.iter().enumerate() {
        if let Some(owner) = owners[position] {
            let group = &groups[&owner];
            if group[0] == position {
                let mut backgrounds = group.clone();
                backgrounds.sort_by_key(|position| layer(nodes[*position].3.display));
                for member in backgrounds {
                    let (index, rect, _, style) = nodes[member];
                    let mut style = style.clone();
                    clear_borders(&mut style);
                    style.outline_width = 0.0;
                    output.push(ReplayNode {
                        index,
                        rect,
                        kind: Cow::Owned(ComponentKind::Box),
                        style: Cow::Owned(style),
                    });
                }
                for member in group {
                    let (index, rect, _, style) = nodes[*member];
                    let mut style = style.clone();
                    clear_background(&mut style);
                    style.outline_width = 0.0;
                    output.push(ReplayNode {
                        index,
                        rect,
                        kind: Cow::Owned(ComponentKind::Box),
                        style: Cow::Owned(style),
                    });
                }
            }
            let mut style = style.clone();
            clear_background(&mut style);
            clear_borders(&mut style);
            output.push(ReplayNode {
                index,
                rect,
                kind: Cow::Borrowed(kind),
                style: Cow::Owned(style),
            });
        } else {
            output.push(ReplayNode {
                index,
                rect,
                kind: Cow::Borrowed(kind),
                style: Cow::Borrowed(style),
            });
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paint_artifact::PaintNode;

    fn artifact(positioned: bool) -> PaintArtifact {
        let nodes = [
            Display::Table,
            Display::TableRow,
            Display::TableCell,
            Display::TableCell,
        ]
        .into_iter()
        .enumerate()
        .map(|(index, display)| PaintNode {
            kind: ComponentKind::Box,
            style: Style {
                display,
                border_collapse: true,
                background: if index == 2 {
                    Color::rgb(255, 0, 0)
                } else {
                    Color::rgb(0, 128, 0)
                },
                border_width: 2.0,
                outline_width: if index == 2 { 3.0 } else { 0.0 },
                position: if positioned && index == 3 {
                    Position::Relative
                } else {
                    Position::Static
                },
                ..Style::default()
            },
            parent: match index {
                0 => None,
                1 => Some(0),
                _ => Some(1),
            },
            sticky_counter_signal: None,
        })
        .collect::<Vec<_>>();
        let rect = LayoutRect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        };
        PaintArtifact::build(
            nodes,
            &(0..4).map(|index| (rect, index)).collect::<Vec<_>>(),
            1,
        )
    }

    #[test]
    fn all_cell_backgrounds_precede_borders_and_content() {
        let artifact = artifact(false);
        let input = artifact
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| {
                (
                    index,
                    artifact.rect_by_index[index].unwrap(),
                    &node.kind,
                    &node.style,
                )
            })
            .collect::<Vec<_>>();
        let output = replay(&input, &artifact);
        assert_eq!(output.len(), 12);
        for node in &output[..4] {
            assert_eq!(node.style.border_color, Color::TRANSPARENT);
            assert_eq!(node.style.outline_width, 0.0);
        }
        assert_eq!(output[2].style.background, Color::rgb(255, 0, 0));
        assert_eq!(output[3].style.background, Color::rgb(0, 128, 0));
        for node in &output[4..8] {
            assert_eq!(node.style.background, Color::TRANSPARENT);
            assert_eq!(node.style.outline_width, 0.0);
        }
        assert_eq!(output[10].style.outline_width, 3.0);
        for node in &output[8..] {
            assert_eq!(node.style.background, Color::TRANSPARENT);
            assert_eq!(node.style.border_color, Color::TRANSPARENT);
        }
        assert_eq!(artifact.nodes[2].style.background, Color::rgb(255, 0, 0));
    }

    #[test]
    fn positioned_cells_keep_their_atomic_replay() {
        let artifact = artifact(true);
        let input = artifact
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| {
                (
                    index,
                    artifact.rect_by_index[index].unwrap(),
                    &node.kind,
                    &node.style,
                )
            })
            .collect::<Vec<_>>();
        let output = replay(&input, &artifact);
        let cell = output
            .iter()
            .filter(|node| node.index == 3)
            .collect::<Vec<_>>();
        assert_eq!(cell.len(), 1);
        assert!(matches!(cell[0].style, Cow::Borrowed(_)));
    }
}
