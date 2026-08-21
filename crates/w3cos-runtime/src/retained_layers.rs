//! Retained compositor layers over a [`PaintArtifact`].
//!
//! Blink splits paint (display items) from compositor property trees. This
//! module does the same for W3COS: backends record each layer once, then a
//! scroll / opacity / transform frame replays those recordings instead of
//! flattening a new Vello or Skia scene.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use w3cos_std::component::ComponentKind;
use w3cos_std::style::{Style, Transform2D};

use crate::layout::LayoutRect;
use crate::paint_artifact::{PaintArtifact, PaintChunkId, PaintProperties};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayerPaintAction {
    /// Display items or non-compositor style changed; backends must re-record.
    Rebuild,
    /// Only scroll offset / opacity / transform changed; replay recordings.
    Replay,
}

#[derive(Clone, Debug)]
pub struct CompositorLayer {
    pub id: usize,
    pub bounds: LayoutRect,
    pub properties: PaintProperties,
    pub chunk_ids: Vec<PaintChunkId>,
    pub client_indices: Vec<usize>,
    pub sticky_owner: Option<usize>,
    pub effect_owner: Option<usize>,
    pub transform_owner: Option<usize>,
    pub scroll_host: Option<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct CompositorOverrides {
    pub opacity: HashMap<usize, f32>,
    pub transform: HashMap<usize, Transform2D>,
}

impl CompositorOverrides {
    pub fn from_style_map(overrides: &HashMap<usize, Style>) -> Self {
        let mut out = Self::default();
        for (&index, style) in overrides {
            if (style.opacity - 1.0).abs() > f32::EPSILON {
                out.opacity.insert(index, style.opacity);
            }
            if !style.transform.is_identity() {
                out.transform.insert(index, style.transform);
            }
        }
        out
    }
}

#[derive(Clone, Debug, Default)]
pub struct RetainedLayerTree {
    pub layers: Vec<CompositorLayer>,
    content_fingerprint: u64,
    compositor_fingerprint: u64,
    recordings_valid: bool,
    pub full_scene_rebuilds: u64,
    pub compositor_replays: u64,
}

impl RetainedLayerTree {
    pub fn sync(
        &mut self,
        artifact: &PaintArtifact,
        scroll_offsets: &HashMap<usize, (f32, f32)>,
        overrides: &CompositorOverrides,
    ) -> LayerPaintAction {
        let layers = build_layers(artifact);
        let content = content_fingerprint(artifact, &layers);
        let compositor = compositor_fingerprint(artifact, &layers, scroll_offsets, overrides);
        let structure_changed = self.layers.len() != layers.len()
            || self
                .layers
                .iter()
                .zip(&layers)
                .any(|(old, new)| old.client_indices != new.client_indices);
        self.layers = layers;

        if !self.recordings_valid || structure_changed || content != self.content_fingerprint {
            self.content_fingerprint = content;
            self.compositor_fingerprint = compositor;
            self.recordings_valid = false;
            LayerPaintAction::Rebuild
        } else if compositor != self.compositor_fingerprint {
            self.compositor_fingerprint = compositor;
            LayerPaintAction::Replay
        } else {
            LayerPaintAction::Replay
        }
    }

    pub fn note_rebuild(&mut self) {
        self.recordings_valid = true;
        self.full_scene_rebuilds = self.full_scene_rebuilds.saturating_add(1);
    }

    pub fn note_replay(&mut self) {
        self.compositor_replays = self.compositor_replays.saturating_add(1);
    }

    pub fn invalidate_recordings(&mut self) {
        self.recordings_valid = false;
    }

    pub fn recordings_valid(&self) -> bool {
        self.recordings_valid && !self.layers.is_empty()
    }
}

pub fn build_layers(artifact: &PaintArtifact) -> Vec<CompositorLayer> {
    let mut layers = Vec::new();
    for (chunk_id, chunk) in artifact.chunks.iter().enumerate() {
        let sticky = artifact
            .display_items
            .get(chunk.begin)
            .and_then(|item| artifact.sticky_owner.get(item.client_index).copied())
            .flatten();
        let same_as_current = layers.last().is_some_and(|layer: &CompositorLayer| {
            layer.properties == chunk.properties && layer.sticky_owner == sticky
        });
        if same_as_current {
            let layer = layers.last_mut().expect("current layer");
            layer.chunk_ids.push(chunk_id);
            layer.bounds = union_rect(layer.bounds, chunk.bounds);
            for item in &artifact.display_items[chunk.begin..chunk.end] {
                layer.client_indices.push(item.client_index);
            }
            continue;
        }
        let mut client_indices = Vec::with_capacity(chunk.end.saturating_sub(chunk.begin));
        for item in &artifact.display_items[chunk.begin..chunk.end] {
            client_indices.push(item.client_index);
        }
        let scroll_host = artifact
            .properties
            .scrolls
            .get(chunk.properties.scroll)
            .and_then(|node| node.host_index);
        let effect_owner = property_owner(artifact, |props| props.effect, chunk.properties.effect);
        let transform_owner = property_owner(
            artifact,
            |props| props.transform,
            chunk.properties.transform,
        );
        layers.push(CompositorLayer {
            id: layers.len(),
            bounds: chunk.bounds,
            properties: chunk.properties,
            chunk_ids: vec![chunk_id],
            client_indices,
            sticky_owner: sticky,
            effect_owner,
            transform_owner,
            scroll_host,
        });
    }
    layers
}

fn property_owner(
    artifact: &PaintArtifact,
    slot: impl Fn(&PaintProperties) -> usize,
    id: usize,
) -> Option<usize> {
    if id == 0 {
        return None;
    }
    artifact
        .node_properties
        .iter()
        .enumerate()
        .find(|(index, props)| {
            slot(props) == id
                && artifact
                    .nodes
                    .get(*index)
                    .and_then(|node| node.parent)
                    .map_or(true, |parent| {
                        artifact
                            .node_properties
                            .get(parent)
                            .map(|parent_props| slot(parent_props) != id)
                            .unwrap_or(true)
                    })
        })
        .map(|(index, _)| index)
}

fn union_rect(a: LayoutRect, b: LayoutRect) -> LayoutRect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    LayoutRect {
        x,
        y,
        width: (a.x + a.width).max(b.x + b.width) - x,
        height: (a.y + a.height).max(b.y + b.height) - y,
    }
}

fn content_fingerprint(artifact: &PaintArtifact, layers: &[CompositorLayer]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    layers.len().hash(&mut hasher);
    for layer in layers {
        layer.properties.transform.hash(&mut hasher);
        layer.properties.clip.hash(&mut hasher);
        layer.properties.effect.hash(&mut hasher);
        layer.properties.scroll.hash(&mut hasher);
        layer.sticky_owner.hash(&mut hasher);
        layer.client_indices.hash(&mut hasher);
        hash_rect(layer.bounds, &mut hasher);
    }
    for item in &artifact.display_items {
        item.client_index.hash(&mut hasher);
        hash_rect(item.visual_rect, &mut hasher);
        item.chunk_id.hash(&mut hasher);
    }
    for (index, node) in artifact.nodes.iter().enumerate() {
        index.hash(&mut hasher);
        node.parent.hash(&mut hasher);
        hash_kind(&node.kind, &mut hasher);
        hash_paint_style(&node.style, &mut hasher);
        artifact
            .z_order
            .get(index)
            .copied()
            .unwrap_or(0)
            .hash(&mut hasher);
        artifact.sticky_owner.get(index).copied().hash(&mut hasher);
    }
    for clip in &artifact.properties.clips {
        clip.parent.hash(&mut hasher);
        match clip.rect {
            Some(rect) => {
                1u8.hash(&mut hasher);
                hash_rect(rect, &mut hasher);
            }
            None => 0u8.hash(&mut hasher),
        }
    }
    for effect in &artifact.properties.effects {
        effect.parent.hash(&mut hasher);
        effect.filter.hash(&mut hasher);
    }
    hasher.finish()
}

fn compositor_fingerprint(
    artifact: &PaintArtifact,
    layers: &[CompositorLayer],
    scroll_offsets: &HashMap<usize, (f32, f32)>,
    overrides: &CompositorOverrides,
) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut scrolls: Vec<_> = scroll_offsets.iter().collect();
    scrolls.sort_by_key(|(id, _)| *id);
    scrolls.len().hash(&mut hasher);
    for (id, (x, y)) in scrolls {
        id.hash(&mut hasher);
        x.to_bits().hash(&mut hasher);
        y.to_bits().hash(&mut hasher);
    }
    for transform in &artifact.properties.transforms {
        transform.parent.hash(&mut hasher);
        hash_transform(transform.transform, &mut hasher);
    }
    for effect in &artifact.properties.effects {
        effect.opacity.to_bits().hash(&mut hasher);
    }
    let mut opacity_over: Vec<_> = overrides.opacity.iter().collect();
    opacity_over.sort_by_key(|(id, _)| *id);
    for (id, opacity) in opacity_over {
        id.hash(&mut hasher);
        opacity.to_bits().hash(&mut hasher);
    }
    let mut transform_over: Vec<_> = overrides.transform.iter().collect();
    transform_over.sort_by_key(|(id, _)| *id);
    for (id, transform) in transform_over {
        id.hash(&mut hasher);
        hash_transform(*transform, &mut hasher);
    }
    for layer in layers {
        hash_transform(layer_css_transform(layer, artifact, overrides), &mut hasher);
        layer_opacity(layer, artifact, overrides)
            .to_bits()
            .hash(&mut hasher);
    }
    hasher.finish()
}

fn hash_rect(rect: LayoutRect, hasher: &mut impl Hasher) {
    rect.x.to_bits().hash(hasher);
    rect.y.to_bits().hash(hasher);
    rect.width.to_bits().hash(hasher);
    rect.height.to_bits().hash(hasher);
}

fn hash_transform(transform: Transform2D, hasher: &mut impl Hasher) {
    transform.translate_x.to_bits().hash(hasher);
    transform.translate_y.to_bits().hash(hasher);
    transform.scale_x.to_bits().hash(hasher);
    transform.scale_y.to_bits().hash(hasher);
    transform.rotate_deg.to_bits().hash(hasher);
}

fn hash_kind(kind: &ComponentKind, hasher: &mut impl Hasher) {
    std::mem::discriminant(kind).hash(hasher);
    match kind {
        ComponentKind::Text { content } => content.hash(hasher),
        ComponentKind::Button { label } => label.hash(hasher),
        ComponentKind::Image { src } => src.hash(hasher),
        ComponentKind::TextInput {
            value,
            placeholder,
            secure,
        } => {
            value.hash(hasher);
            placeholder.hash(hasher);
            secure.hash(hasher);
        }
        ComponentKind::Canvas { width, height } => {
            width.hash(hasher);
            height.hash(hasher);
        }
        ComponentKind::SvgDocument {
            source,
            width,
            height,
            ..
        } => {
            source.hash(hasher);
            width.hash(hasher);
            height.hash(hasher);
        }
        _ => {}
    }
}

fn hash_paint_style(style: &Style, hasher: &mut impl Hasher) {
    style.background.r.hash(hasher);
    style.background.g.hash(hasher);
    style.background.b.hash(hasher);
    style.background.a.hash(hasher);
    style.color.r.hash(hasher);
    style.color.g.hash(hasher);
    style.color.b.hash(hasher);
    style.color.a.hash(hasher);
    style.font_size.to_bits().hash(hasher);
    style.font_weight.hash(hasher);
    style.font_family.hash(hasher);
    style.border_radius.to_bits().hash(hasher);
    style.border_width.to_bits().hash(hasher);
    style.border_color.r.hash(hasher);
    style.border_color.g.hash(hasher);
    style.border_color.b.hash(hasher);
    style.border_color.a.hash(hasher);
    style.filter.hash(hasher);
    style.background_image.hash(hasher);
    std::mem::discriminant(&style.visibility).hash(hasher);
    std::mem::discriminant(&style.overflow).hash(hasher);
    match style.overflow_x {
        Some(value) => {
            1u8.hash(hasher);
            std::mem::discriminant(&value).hash(hasher);
        }
        None => 0u8.hash(hasher),
    }
    match style.overflow_y {
        Some(value) => {
            1u8.hash(hasher);
            std::mem::discriminant(&value).hash(hasher);
        }
        None => 0u8.hash(hasher),
    }
    style.box_shadow.is_some().hash(hasher);
    style.will_change.transform.hash(hasher);
    style.will_change.opacity.hash(hasher);
    style.will_change.filter.hash(hasher);
    style.will_change.scroll_position.hash(hasher);
    std::mem::discriminant(&style.contain).hash(hasher);
    std::mem::discriminant(&style.text_align).hash(hasher);
    std::mem::discriminant(&style.white_space).hash(hasher);
    style.line_height.to_bits().hash(hasher);
    style.letter_spacing.to_bits().hash(hasher);
    std::mem::discriminant(&style.text_decoration).hash(hasher);
    style.outline_width.to_bits().hash(hasher);
}

pub fn layer_scroll_translation(
    layer: &CompositorLayer,
    scroll_info: &[Option<(f32, f32, LayoutRect)>],
) -> (f32, f32, Option<LayoutRect>) {
    let Some(&first) = layer.client_indices.first() else {
        return (0.0, 0.0, None);
    };
    match scroll_info.get(first).copied().flatten() {
        Some((sx, sy, clip)) => (-sx, -sy, Some(clip)),
        None => (0.0, 0.0, None),
    }
}

pub fn layer_css_transform(
    layer: &CompositorLayer,
    artifact: &PaintArtifact,
    overrides: &CompositorOverrides,
) -> Transform2D {
    if let Some(owner) = layer.transform_owner
        && let Some(&transform) = overrides.transform.get(&owner)
    {
        return transform;
    }
    artifact
        .properties
        .transforms
        .get(layer.properties.transform)
        .map(|node| node.transform)
        .unwrap_or(Transform2D::IDENTITY)
}

pub fn layer_opacity(
    layer: &CompositorLayer,
    artifact: &PaintArtifact,
    overrides: &CompositorOverrides,
) -> f32 {
    let mut opacity = 1.0;
    let mut effect_id = layer.properties.effect;
    while effect_id != 0 {
        let Some(node) = artifact.properties.effects.get(effect_id) else {
            break;
        };
        let mut local = node.opacity;
        if let Some(owner) = layer.effect_owner
            && artifact
                .node_properties
                .get(owner)
                .is_some_and(|props| props.effect == effect_id)
            && let Some(&overridden) = overrides.opacity.get(&owner)
        {
            local = overridden;
        }
        opacity *= local;
        if node.parent == effect_id {
            break;
        }
        effect_id = node.parent;
    }
    opacity.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use w3cos_std::style::{Overflow, Style, Transform2D};

    use crate::paint_artifact::PaintNode;

    fn rect(y: f32) -> LayoutRect {
        LayoutRect {
            x: 0.0,
            y,
            width: 320.0,
            height: 80.0,
        }
    }

    fn scroll_tree() -> PaintArtifact {
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
        PaintArtifact::build(
            [root, scroll, child],
            &[(rect(0.0), 0), (rect(0.0), 1), (rect(80.0), 2)],
            1,
        )
    }

    fn note(tree: &mut RetainedLayerTree, action: LayerPaintAction) {
        match action {
            LayerPaintAction::Rebuild => tree.note_rebuild(),
            LayerPaintAction::Replay => tree.note_replay(),
        }
    }

    #[test]
    fn scroll_only_skips_full_scene_rebuild() {
        let artifact = scroll_tree();
        let mut tree = RetainedLayerTree::default();
        let mut scrolls = HashMap::new();
        scrolls.insert(1, (0.0, 0.0));
        let first = tree.sync(&artifact, &scrolls, &CompositorOverrides::default());
        assert_eq!(first, LayerPaintAction::Rebuild);
        note(&mut tree, first);
        assert_eq!(tree.full_scene_rebuilds, 1);

        scrolls.insert(1, (0.0, 48.0));
        let second = tree.sync(&artifact, &scrolls, &CompositorOverrides::default());
        assert_eq!(second, LayerPaintAction::Replay);
        note(&mut tree, second);
        assert_eq!(tree.full_scene_rebuilds, 1);
        assert_eq!(tree.compositor_replays, 1);
    }

    #[test]
    fn opacity_only_skips_full_scene_rebuild() {
        let artifact = scroll_tree();
        let mut tree = RetainedLayerTree::default();
        let scrolls = HashMap::from([(1, (0.0, 0.0))]);
        let first = tree.sync(&artifact, &scrolls, &CompositorOverrides::default());
        note(&mut tree, first);
        assert_eq!(tree.full_scene_rebuilds, 1);

        let mut overrides = CompositorOverrides::default();
        overrides.opacity.insert(2, 0.2);
        let second = tree.sync(&artifact, &scrolls, &overrides);
        assert_eq!(second, LayerPaintAction::Replay);
        note(&mut tree, second);
        assert_eq!(tree.full_scene_rebuilds, 1);
        assert_eq!(tree.compositor_replays, 1);
    }

    #[test]
    fn transform_only_skips_full_scene_rebuild() {
        let artifact = scroll_tree();
        let mut tree = RetainedLayerTree::default();
        let scrolls = HashMap::from([(1, (0.0, 0.0))]);
        let first = tree.sync(&artifact, &scrolls, &CompositorOverrides::default());
        note(&mut tree, first);

        let mut overrides = CompositorOverrides::default();
        overrides.transform.insert(
            2,
            Transform2D {
                translate_y: 24.0,
                ..Transform2D::IDENTITY
            },
        );
        let action = tree.sync(&artifact, &scrolls, &overrides);
        assert_eq!(action, LayerPaintAction::Replay);
        note(&mut tree, action);
        assert_eq!(tree.full_scene_rebuilds, 1);
        assert_eq!(tree.compositor_replays, 1);
    }

    #[test]
    fn dirty_paint_rebuilds_the_affected_layer_tree() {
        let artifact = scroll_tree();
        let mut tree = RetainedLayerTree::default();
        let scrolls = HashMap::from([(1, (0.0, 0.0))]);
        let first = tree.sync(&artifact, &scrolls, &CompositorOverrides::default());
        note(&mut tree, first);

        let mut dirty_style = Style::default();
        dirty_style.overflow = Overflow::Scroll;
        let mut child_style = Style::default();
        child_style.opacity = 0.5;
        child_style.transform.translate_y = 4.0;
        let dirty = PaintArtifact::build(
            [
                PaintNode {
                    kind: ComponentKind::Column,
                    style: Style::default(),
                    parent: None,
                    sticky_counter_signal: None,
                },
                PaintNode {
                    kind: ComponentKind::Column,
                    style: dirty_style,
                    parent: Some(0),
                    sticky_counter_signal: None,
                },
                PaintNode {
                    kind: ComponentKind::Text {
                        content: "dirty".into(),
                    },
                    style: child_style,
                    parent: Some(1),
                    sticky_counter_signal: None,
                },
            ],
            &[(rect(0.0), 0), (rect(0.0), 1), (rect(80.0), 2)],
            2,
        );
        let action = tree.sync(&dirty, &scrolls, &CompositorOverrides::default());
        assert_eq!(action, LayerPaintAction::Rebuild);
        note(&mut tree, action);
        assert_eq!(tree.full_scene_rebuilds, 2);
        assert_eq!(tree.compositor_replays, 0);
    }

    #[cfg(feature = "skia")]
    fn skia_nodes(artifact: &PaintArtifact) -> Vec<(usize, LayoutRect, &ComponentKind, &Style)> {
        artifact
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(idx, node)| {
                let rect = artifact.rect_by_index.get(idx).copied().flatten()?;
                Some((idx, rect, &node.kind, &node.style))
            })
            .collect()
    }

    #[cfg(feature = "skia")]
    fn paint_skia(
        rasterizer: &mut crate::render_skia::SkiaRasterizer,
        artifact: &PaintArtifact,
        scroll_info: &[Option<(f32, f32, LayoutRect)>],
        overrides: Option<&CompositorOverrides>,
    ) -> Vec<u8> {
        let nodes = skia_nodes(artifact);
        let font = fontdue::Font::from_bytes(
            include_bytes!("../assets/Inter-Regular.ttf").as_slice(),
            fontdue::FontSettings::default(),
        )
        .unwrap();
        rasterizer
            .render_frame(
                8,
                8,
                &nodes,
                &font,
                scroll_info,
                &std::collections::HashMap::new(),
                None,
                w3cos_std::color::Color::WHITE,
                Some(artifact),
                overrides,
                1.0,
            )
            .unwrap()
            .to_vec()
    }

    #[cfg(feature = "skia")]
    fn red_layer_tree() -> PaintArtifact {
        let mut child_style = Style::default();
        child_style.background = w3cos_std::color::Color::rgb(255, 0, 0);
        child_style.opacity = 0.5;
        child_style.transform.translate_y = 1.0;
        PaintArtifact::build(
            [
                PaintNode {
                    kind: ComponentKind::Column,
                    style: Style::default(),
                    parent: None,
                    sticky_counter_signal: None,
                },
                PaintNode {
                    kind: ComponentKind::Box,
                    style: child_style,
                    parent: Some(0),
                    sticky_counter_signal: None,
                },
            ],
            &[
                (
                    LayoutRect {
                        x: 0.0,
                        y: 0.0,
                        width: 8.0,
                        height: 8.0,
                    },
                    0,
                ),
                (
                    LayoutRect {
                        x: 0.0,
                        y: 0.0,
                        width: 8.0,
                        height: 8.0,
                    },
                    1,
                ),
            ],
            1,
        )
    }

    #[cfg(feature = "skia")]
    #[test]
    fn opacity_only_skips_skia_picture_rerecord() {
        let artifact = red_layer_tree();
        let mut rasterizer =
            crate::render_skia::SkiaRasterizer::new(include_bytes!("../assets/Inter-Regular.ttf"))
                .unwrap();
        let first = paint_skia(&mut rasterizer, &artifact, &[], None);
        assert_eq!(rasterizer.retained_rebuilds(), 1);
        assert_eq!(rasterizer.retained_replays(), 0);

        let mut overrides = CompositorOverrides::default();
        overrides.opacity.insert(1, 0.2);
        let second = paint_skia(&mut rasterizer, &artifact, &[], Some(&overrides));
        assert_eq!(rasterizer.retained_rebuilds(), 1);
        assert_eq!(rasterizer.retained_replays(), 1);
        assert_ne!(first, second, "opacity replay must still change pixels");
        let center = (4 * 8 + 4) * 4;
        let first_px = &first[center..center + 4];
        let second_px = &second[center..center + 4];
        assert!(
            second_px[1] > first_px[1] && second_px[3] == 255 && first_px[3] == 255,
            "lower opacity over white should lighten red: first={first_px:?} second={second_px:?}"
        );
    }

    #[cfg(feature = "skia")]
    #[test]
    fn transform_only_skips_skia_picture_rerecord() {
        let artifact = red_layer_tree();
        let mut rasterizer =
            crate::render_skia::SkiaRasterizer::new(include_bytes!("../assets/Inter-Regular.ttf"))
                .unwrap();
        let first = paint_skia(&mut rasterizer, &artifact, &[], None);
        assert_eq!(rasterizer.retained_rebuilds(), 1);

        let mut overrides = CompositorOverrides::default();
        overrides.transform.insert(
            1,
            Transform2D {
                translate_y: 4.0,
                ..Transform2D::IDENTITY
            },
        );
        let second = paint_skia(&mut rasterizer, &artifact, &[], Some(&overrides));
        assert_eq!(rasterizer.retained_rebuilds(), 1);
        assert_eq!(rasterizer.retained_replays(), 1);
        assert_ne!(first, second, "transform replay must still move pixels");
        assert_eq!(&second[0..4], &[255, 255, 255, 255]);
    }

    #[cfg(feature = "skia")]
    #[test]
    fn scroll_only_skips_skia_picture_rerecord() {
        let artifact = scroll_tree();
        let mut rasterizer =
            crate::render_skia::SkiaRasterizer::new(include_bytes!("../assets/Inter-Regular.ttf"))
                .unwrap();
        let clip = rect(0.0);
        let mut scroll_info = vec![None, None, Some((0.0, 0.0, clip))];
        paint_skia(&mut rasterizer, &artifact, &scroll_info, None);
        assert_eq!(rasterizer.retained_rebuilds(), 1);

        scroll_info[2] = Some((0.0, 48.0, clip));
        paint_skia(&mut rasterizer, &artifact, &scroll_info, None);
        assert_eq!(rasterizer.retained_rebuilds(), 1);
        assert_eq!(rasterizer.retained_replays(), 1);
    }
}
