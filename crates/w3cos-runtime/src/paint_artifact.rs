//! Retained paint output shared by every raster backend.
//!
//! This follows Blink's split between layout, PaintArtifact construction and
//! compositor consumption. The artifact owns immutable snapshots so scrolling
//! and raster scheduling never need to walk the application component tree.

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

#[derive(Clone, Default)]
pub struct PaintArtifact {
    pub nodes: Vec<PaintNode>,
    pub display_items: Vec<DisplayItem>,
    pub chunks: Vec<PaintChunk>,
    pub properties: PropertyTrees,
    pub node_properties: Vec<PaintProperties>,
    pub z_order: Vec<i32>,
    pub sticky_owner: Vec<Option<usize>>,
    pub rect_by_index: Vec<Option<LayoutRect>>,
    pub generation: u64,
}

impl PaintArtifact {
    pub fn build(
        nodes: impl IntoIterator<Item = PaintNode>,
        layout_cache: &[(LayoutRect, usize)],
        generation: u64,
    ) -> Self {
        let nodes: Vec<_> = nodes.into_iter().collect();
        let mut artifact = Self {
            rect_by_index: vec![None; nodes.len()],
            node_properties: vec![PaintProperties::default(); nodes.len()],
            z_order: vec![0; nodes.len()],
            sticky_owner: vec![None; nodes.len()],
            nodes,
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
        self.z_order[index] = if node.style.z_index == 0 {
            inherited_z
        } else {
            node.style.z_index
        };
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
        if matches!(
            node.style.overflow,
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
        if matches!(node.style.overflow, Overflow::Scroll | Overflow::Auto) {
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
    fn builds_independent_property_trees_and_display_chunks() {
        let root = PaintNode {
            kind: ComponentKind::Column,
            style: Style::default(),
            parent: None,
        };
        let mut scroll_style = Style::default();
        scroll_style.overflow = Overflow::Scroll;
        let scroll = PaintNode {
            kind: ComponentKind::Column,
            style: scroll_style,
            parent: Some(0),
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
            },
            PaintNode {
                kind: ComponentKind::Column,
                style: sticky_style,
                parent: Some(0),
            },
            PaintNode {
                kind: ComponentKind::Text {
                    content: "inside".into(),
                },
                style: Style::default(),
                parent: Some(1),
            },
        ];
        let artifact =
            PaintArtifact::build(nodes, &[(rect(0.0), 0), (rect(0.0), 1), (rect(20.0), 2)], 1);

        assert_eq!(artifact.sticky_owner, vec![None, Some(1), Some(1)]);
        assert_eq!(artifact.z_order, vec![0, 3, 3]);
    }
}
