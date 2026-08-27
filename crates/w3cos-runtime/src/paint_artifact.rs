//! Retained paint output shared by every raster backend.
//!
//! This follows Blink's split between layout, PaintArtifact construction and
//! compositor consumption. The artifact owns immutable snapshots so scrolling
//! and raster scheduling never need to walk the application component tree.

use w3cos_std::color::Color;
use w3cos_std::component::ComponentKind;
use w3cos_std::style::{Overflow, Position, Style, Transform2D};

use crate::layout::LayoutRect;

pub type PropertyNodeId = usize;
pub type PaintChunkId = usize;

#[derive(Clone)]
pub struct PaintNode {
    pub kind: ComponentKind,
    pub style: Style,
    pub parent: Option<usize>,
    pub sticky_counter_signal: Option<usize>,
}

pub fn effective_z_order(style: &Style, inherited: i32) -> i32 {
    if style.z_index != 0 {
        style.z_index
    } else if matches!(
        style.position,
        Position::Relative | Position::Absolute | Position::Fixed | Position::Sticky
    ) {
        inherited.saturating_add(1)
    } else {
        inherited
    }
}

/// Rebuild paint nodes, cloning `Style` only for slots that actually changed.
///
/// Returns the node list and the number of Style clones performed. A clean
/// subtree (same length, same parent/kind/style) reuses the previous
/// allocation and reports 0 clones.
pub fn reuse_or_clone_paint_nodes<'a>(
    existing: Vec<PaintNode>,
    incoming: impl IntoIterator<
        Item = (&'a ComponentKind, &'a Style, Option<usize>, Option<usize>),
    >,
) -> (Vec<PaintNode>, usize) {
    let incoming: Vec<_> = incoming.into_iter().collect();
    if existing.len() != incoming.len() {
        let clones = incoming.len();
        let nodes = incoming
            .into_iter()
            .map(|(kind, style, parent, sticky)| PaintNode {
                kind: kind.clone(),
                style: style.clone(),
                parent,
                sticky_counter_signal: sticky,
            })
            .collect();
        return (nodes, clones);
    }
    let mut existing: Vec<Option<PaintNode>> = existing.into_iter().map(Some).collect();
    let mut out = Vec::with_capacity(incoming.len());
    let mut clones = 0;
    for (i, (kind, style, parent, sticky)) in incoming.into_iter().enumerate() {
        let reusable = existing[i].as_ref().is_some_and(|old| {
            old.parent == parent
                && old.sticky_counter_signal == sticky
                && old.kind == *kind
                && (std::ptr::eq(&old.style, style) || old.style == *style)
        });
        if reusable {
            out.push(existing[i].take().expect("paint node slot"));
        } else {
            clones += 1;
            out.push(PaintNode {
                kind: kind.clone(),
                style: style.clone(),
                parent,
                sticky_counter_signal: sticky,
            });
        }
    }
    (out, clones)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PaintProperties {
    pub transform: PropertyNodeId,
    pub clip: PropertyNodeId,
    pub effect: PropertyNodeId,
    pub scroll: PropertyNodeId,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransformNode {
    pub parent: PropertyNodeId,
    pub transform: Transform2D,
}

#[derive(Clone, Copy, Debug)]
pub struct ClipNode {
    pub parent: PropertyNodeId,
    pub rect: Option<LayoutRect>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EffectNode {
    pub parent: PropertyNodeId,
    pub opacity: f32,
    pub filter: Option<String>,
}

#[derive(Clone, Copy, Debug)]
pub struct ScrollNode {
    pub parent: PropertyNodeId,
    pub host_index: Option<usize>,
    pub scrollport: Option<LayoutRect>,
}

#[derive(Clone, Debug)]
pub struct PropertyTrees {
    pub transforms: Vec<TransformNode>,
    pub clips: Vec<ClipNode>,
    pub effects: Vec<EffectNode>,
    pub scrolls: Vec<ScrollNode>,
}

impl Default for PropertyTrees {
    fn default() -> Self {
        Self {
            transforms: vec![TransformNode {
                parent: 0,
                transform: Transform2D::IDENTITY,
            }],
            clips: vec![ClipNode {
                parent: 0,
                rect: None,
            }],
            effects: vec![EffectNode {
                parent: 0,
                opacity: 1.0,
                filter: None,
            }],
            scrolls: vec![ScrollNode {
                parent: 0,
                host_index: None,
                scrollport: None,
            }],
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DisplayItem {
    pub client_index: usize,
    pub visual_rect: LayoutRect,
    pub chunk_id: PaintChunkId,
}

#[derive(Clone, Copy, Debug)]
pub struct PaintChunk {
    pub begin: usize,
    pub end: usize,
    pub bounds: LayoutRect,
    pub properties: PaintProperties,
    pub z_order: i32,
}

#[derive(Clone)]
pub struct PaintArtifact {
    pub nodes: Vec<PaintNode>,
    pub canvas_background: Color,
    pub canvas_background_style: Option<Style>,
    pub canvas_background_source: Option<usize>,
    pub canvas_background_positioning_rect: Option<LayoutRect>,
    pub display_items: Vec<DisplayItem>,
    pub chunks: Vec<PaintChunk>,
    pub properties: PropertyTrees,
    pub node_properties: Vec<PaintProperties>,
    pub z_order: Vec<i32>,
    pub sticky_owner: Vec<Option<usize>>,
    pub rect_by_index: Vec<Option<LayoutRect>>,
    pub generation: u64,
}

impl Default for PaintArtifact {
    fn default() -> Self {
        Self {
            nodes: Vec::new(),
            canvas_background: Color::WHITE,
            canvas_background_style: None,
            canvas_background_source: None,
            canvas_background_positioning_rect: None,
            display_items: Vec::new(),
            chunks: Vec::new(),
            properties: PropertyTrees::default(),
            node_properties: Vec::new(),
            z_order: Vec::new(),
            sticky_owner: Vec::new(),
            rect_by_index: Vec::new(),
            generation: 0,
        }
    }
}

impl PaintArtifact {
    pub fn build(
        nodes: impl IntoIterator<Item = PaintNode>,
        layout_cache: &[(LayoutRect, usize)],
        generation: u64,
    ) -> Self {
        Self::build_with_body_background(nodes, layout_cache, generation, None)
    }

    pub fn build_with_body_background(
        nodes: impl IntoIterator<Item = PaintNode>,
        layout_cache: &[(LayoutRect, usize)],
        generation: u64,
        body_index: Option<usize>,
    ) -> Self {
        let mut nodes: Vec<_> = nodes.into_iter().collect();
        let has_background_image = |style: &Style| {
            style.background_image.as_deref().is_some_and(|value| {
                value
                    .split(',')
                    .any(|layer| !layer.trim().eq_ignore_ascii_case("none"))
            })
        };
        let canvas_background_source = nodes
            .first()
            .filter(|node| node.style.background.a > 0 || has_background_image(&node.style))
            .map(|_| 0)
            .or_else(|| {
                body_index.filter(|index| {
                    *index != 0
                        && nodes.get(*index).is_some_and(|node| {
                            node.parent == Some(0)
                                && (node.style.background.a > 0
                                    || has_background_image(&node.style))
                        })
                })
            });
        let root_rect = layout_cache
            .iter()
            .find_map(|(rect, index)| (*index == 0).then_some(*rect));
        let canvas_background_style = canvas_background_source.map(|index| {
            let mut style = nodes[index].style.clone();
            let root = &nodes[0].style;
            let root_border_x = root.border_left_width.unwrap_or(root.border_width);
            let root_border_y = root.border_top_width.unwrap_or(root.border_width);
            style.border_width = 0.0;
            style.border_top_width = None;
            style.border_right_width = None;
            style.border_bottom_width = None;
            style.border_left_width = None;
            if style.background_position.as_deref().is_none_or(|position| {
                matches!(
                    position.trim().to_ascii_lowercase().as_str(),
                    "" | "0% 0%" | "top left" | "left top"
                )
            }) {
                let (x, y) = if index == 0 {
                    // Root backgrounds cover the canvas from the visible
                    // inner border edge rather than retaining the root box.
                    (
                        (root_border_x - 1.0).max(0.0),
                        (root_border_y - 1.0).max(0.0),
                    )
                } else {
                    // A propagated body background uses the document element's
                    // padding edge as its canvas positioning origin.
                    (
                        root_rect.map_or(0.0, |rect| rect.x) + root_border_x,
                        root_rect.map_or(0.0, |rect| rect.y) + root_border_y,
                    )
                };
                style.background_position = Some(format!("{x}px {y}px"));
            }
            style
        });
        let canvas_background = canvas_background_source
            .map(|index| nodes[index].style.background)
            .filter(|color| color.a > 0)
            .unwrap_or(Color::WHITE);
        let canvas_background_positioning_rect = canvas_background_source
            .filter(|index| *index == 0)
            .and(root_rect);
        if let Some(index) = canvas_background_source {
            nodes[index].style.background = Color::TRANSPARENT;
            nodes[index].style.background_image = None;
        }
        let mut artifact = Self {
            rect_by_index: vec![None; nodes.len()],
            node_properties: vec![PaintProperties::default(); nodes.len()],
            z_order: vec![0; nodes.len()],
            sticky_owner: vec![None; nodes.len()],
            nodes,
            canvas_background,
            canvas_background_style,
            canvas_background_source,
            canvas_background_positioning_rect,
            generation,
            ..Self::default()
        };
        for &(rect, index) in layout_cache {
            if let Some(slot) = artifact.rect_by_index.get_mut(index) {
                *slot = Some(rect);
            }
        }

        for index in 0..artifact.nodes.len() {
            artifact.append_node(index);
        }
        artifact
    }

    fn append_node(&mut self, index: usize) {
        let node = &self.nodes[index];
        let inherited = node
            .parent
            .and_then(|parent| self.node_properties.get(parent).copied())
            .unwrap_or_default();
        let inherited_z = node
            .parent
            .and_then(|parent| self.z_order.get(parent).copied())
            .unwrap_or_default();
        self.z_order[index] = effective_z_order(&node.style, inherited_z);
        self.sticky_owner[index] = if matches!(node.style.position, Position::Sticky) {
            Some(index)
        } else {
            node.parent.and_then(|parent| self.sticky_owner[parent])
        };

        let mut properties = inherited;
        if !node.style.transform.is_identity() {
            properties.transform = self.properties.transforms.len();
            self.properties.transforms.push(TransformNode {
                parent: inherited.transform,
                transform: node.style.transform,
            });
        }
        let overflow_x = node.style.resolved_overflow_x();
        let overflow_y = node.style.resolved_overflow_y();
        if matches!(
            overflow_x,
            Overflow::Hidden | Overflow::Scroll | Overflow::Auto
        ) || matches!(
            overflow_y,
            Overflow::Hidden | Overflow::Scroll | Overflow::Auto
        ) {
            properties.clip = self.properties.clips.len();
            self.properties.clips.push(ClipNode {
                parent: inherited.clip,
                rect: self.rect_by_index[index],
            });
        }
        if node.style.opacity < 0.999 || node.style.filter.is_some() {
            properties.effect = self.properties.effects.len();
            self.properties.effects.push(EffectNode {
                parent: inherited.effect,
                opacity: node.style.opacity,
                filter: node.style.filter.clone(),
            });
        }
        if matches!(overflow_x, Overflow::Scroll | Overflow::Auto)
            || matches!(overflow_y, Overflow::Scroll | Overflow::Auto)
        {
            properties.scroll = self.properties.scrolls.len();
            self.properties.scrolls.push(ScrollNode {
                parent: inherited.scroll,
                host_index: Some(index),
                scrollport: self.rect_by_index[index],
            });
        }
        self.node_properties[index] = properties;

        let Some(bounds) = self.rect_by_index[index] else {
            return;
        };
        let item_index = self.display_items.len();
        let chunk_id = self.chunks.len();
        self.display_items.push(DisplayItem {
            client_index: index,
            visual_rect: bounds,
            chunk_id,
        });
        self.chunks.push(PaintChunk {
            begin: item_index,
            end: item_index + 1,
            bounds,
            properties,
            z_order: self.z_order[index],
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(y: f32) -> LayoutRect {
        LayoutRect {
            x: 0.0,
            y,
            width: 320.0,
            height: 80.0,
        }
    }

    #[test]
    fn propagates_root_background_to_the_canvas_without_repainting_the_root_box() {
        let mut style = Style::default();
        style.background = Color::rgb(255, 255, 0);
        style.background_image = Some("url(square-white.png)".into());
        style.border_width = 3.0;
        let artifact = PaintArtifact::build(
            [PaintNode {
                kind: ComponentKind::Column,
                style,
                parent: None,
                sticky_counter_signal: None,
            }],
            &[(rect(0.0), 0)],
            1,
        );

        assert_eq!(artifact.canvas_background, Color::rgb(255, 255, 0));
        assert_eq!(artifact.canvas_background_source, Some(0));
        assert_eq!(artifact.canvas_background_positioning_rect, Some(rect(0.0)));
        let canvas_style = artifact.canvas_background_style.as_ref().unwrap();
        assert_eq!(canvas_style.border_width, 0.0);
        assert_eq!(canvas_style.background_position.as_deref(), Some("2px 2px"));
        assert_eq!(artifact.nodes[0].style.background, Color::TRANSPARENT);
        assert!(artifact.nodes[0].style.background_image.is_none());
    }

    #[test]
    fn propagates_body_background_image_to_the_canvas() {
        let mut root_style = Style::default();
        root_style.background_image = Some("none".into());
        root_style.border_width = 3.0;
        let root = PaintNode {
            kind: ComponentKind::Column,
            style: root_style,
            parent: None,
            sticky_counter_signal: None,
        };
        let mut body_style = Style::default();
        body_style.background_image = Some("url(square-white.png)".into());
        body_style.background_position = Some("top left".into());
        let body = PaintNode {
            kind: ComponentKind::Column,
            style: body_style,
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let artifact = PaintArtifact::build_with_body_background(
            [root, body],
            &[
                (
                    LayoutRect {
                        x: 16.0,
                        ..rect(16.0)
                    },
                    0,
                ),
                (rect(0.0), 1),
            ],
            1,
            Some(1),
        );

        assert_eq!(artifact.canvas_background_source, Some(1));
        assert_eq!(artifact.canvas_background, Color::WHITE);
        assert_eq!(artifact.canvas_background_positioning_rect, None);
        let canvas_style = artifact.canvas_background_style.as_ref().unwrap();
        assert_eq!(canvas_style.border_width, 0.0);
        assert_eq!(
            canvas_style.background_position.as_deref(),
            Some("19px 19px")
        );
        assert!(artifact.nodes[1].style.background_image.is_none());
    }

    #[test]
    fn builds_independent_property_trees_and_display_chunks() {
        let root = PaintNode {
            kind: ComponentKind::Column,
            style: Style::default(),
            parent: None,
            sticky_counter_signal: None,
        };
        let mut scroll_style = Style::default();
        scroll_style.overflow = Overflow::Scroll;
        let scroll = PaintNode {
            kind: ComponentKind::Column,
            style: scroll_style,
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let mut child_style = Style::default();
        child_style.opacity = 0.5;
        child_style.transform.translate_y = 4.0;
        let child = PaintNode {
            kind: ComponentKind::Text {
                content: "row".into(),
            },
            style: child_style,
            parent: Some(1),
            sticky_counter_signal: None,
        };

        let artifact = PaintArtifact::build(
            [root, scroll, child],
            &[(rect(0.0), 0), (rect(0.0), 1), (rect(80.0), 2)],
            7,
        );

        assert_eq!(artifact.generation, 7);
        assert_eq!(artifact.display_items.len(), 3);
        assert_eq!(artifact.chunks.len(), 3);
        assert_eq!(artifact.properties.scrolls.len(), 2);
        assert_eq!(artifact.properties.clips.len(), 2);
        assert_eq!(artifact.properties.effects.len(), 2);
        assert_eq!(artifact.properties.transforms.len(), 2);
        assert_ne!(artifact.node_properties[2].scroll, 0);
        assert_ne!(artifact.node_properties[2].effect, 0);
        assert_ne!(artifact.node_properties[2].transform, 0);
    }

    #[test]
    fn sticky_owner_and_z_order_are_retained() {
        let mut sticky_style = Style::default();
        sticky_style.position = Position::Sticky;
        sticky_style.z_index = 3;
        let nodes = [
            PaintNode {
                kind: ComponentKind::Column,
                style: Style::default(),
                parent: None,
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Column,
                style: sticky_style,
                parent: Some(0),
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Text {
                    content: "inside".into(),
                },
                style: Style::default(),
                parent: Some(1),
                sticky_counter_signal: None,
            },
        ];
        let artifact =
            PaintArtifact::build(nodes, &[(rect(0.0), 0), (rect(0.0), 1), (rect(20.0), 2)], 1);

        assert_eq!(artifact.sticky_owner, vec![None, Some(1), Some(1)]);
        assert_eq!(artifact.z_order, vec![0, 3, 3]);
    }

    #[test]
    fn auto_positioned_subtree_paints_after_later_normal_flow_content() {
        let root = PaintNode {
            kind: ComponentKind::Column,
            style: Style::default(),
            parent: None,
            sticky_counter_signal: None,
        };
        let mut absolute_style = Style::default();
        absolute_style.position = Position::Absolute;
        let absolute = PaintNode {
            kind: ComponentKind::Column,
            style: absolute_style,
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let normal = PaintNode {
            kind: ComponentKind::Column,
            style: Style::default(),
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let artifact = PaintArtifact::build(
            [root, absolute, normal],
            &[(rect(0.0), 0), (rect(0.0), 1), (rect(0.0), 2)],
            1,
        );

        assert_eq!(artifact.z_order, vec![0, 1, 0]);
    }

    #[test]
    fn clean_subtree_does_not_clone_style() {
        let mut style = Style::default();
        style.filter = Some("blur(2px)".into());
        let kind = ComponentKind::Column;
        let incoming = [(&kind, &style, None, None)];
        let (first, clones) = reuse_or_clone_paint_nodes(Vec::new(), incoming);
        assert_eq!(clones, 1);
        let first_ptr = first[0].style.filter.as_ref().map(|s| s.as_ptr());
        let (second, clones) = reuse_or_clone_paint_nodes(first, incoming);
        assert_eq!(clones, 0);
        assert_eq!(
            second[0].style.filter.as_ref().map(|s| s.as_ptr()),
            first_ptr
        );
        let mut dirty = style.clone();
        dirty.opacity = 0.5;
        let incoming_dirty = [(&kind, &dirty, None, None)];
        let (_, dirty_clones) = reuse_or_clone_paint_nodes(second, incoming_dirty);
        assert_eq!(dirty_clones, 1);
    }
}
