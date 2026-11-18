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
    /// Fingerprint of everything a layer recording reads that is not owned
    /// by one paint client (layer list, property trees, self clips).
    global_fingerprint: u64,
    layer_chunk_hashes: Vec<u64>,
    /// Compute `partial` on sync (off unless a backend keeps recordings
    /// across content changes).
    track_partial: bool,
    /// After a `Rebuild` sync: per layer, whether its bounds and the global
    /// inputs are unchanged since the recordings were made, so a backend may
    /// keep that layer's recording when none of its clients was invalidated.
    partial: Option<Vec<bool>>,
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
        let (global, chunk_hashes) = if self.track_partial {
            (
                global_fingerprint(artifact, &layers),
                layer_chunk_hashes(artifact, &layers),
            )
        } else {
            (0, Vec::new())
        };
        let structure_changed = self.layers.len() != layers.len()
            || self
                .layers
                .iter()
                .zip(&layers)
                .any(|(old, new)| old.client_indices != new.client_indices);
        self.partial = (self.track_partial
            && self.recordings_valid
            && !self.layers.is_empty()
            && !structure_changed
            && global == self.global_fingerprint)
            .then(|| {
                self.layers
                    .iter()
                    .zip(&layers)
                    .zip(self.layer_chunk_hashes.iter().zip(&chunk_hashes))
                    .map(|((old, new), (old_chunks, new_chunks))| {
                        rects_equal(old.bounds, new.bounds) && old_chunks == new_chunks
                    })
                    .collect()
            });
        self.global_fingerprint = global;
        self.layer_chunk_hashes = chunk_hashes;
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

    pub fn set_track_partial(&mut self, track: bool) {
        self.track_partial = track;
    }

    /// Per layer, whether its previous recording may be kept if none of its
    /// clients changed (see `partial`). `None` when every layer re-records.
    pub fn partial_layers(&self) -> Option<&[bool]> {
        self.partial.as_deref()
    }
}

/// What a layer's recording reads from its own paint chunks.
fn layer_chunk_hashes(artifact: &PaintArtifact, layers: &[CompositorLayer]) -> Vec<u64> {
    layers
        .iter()
        .map(|layer| {
            // Every display item feeds this each frame: FxHash, not SipHash.
            let mut hasher = w3cos_std::fxhash::FxHasher::default();
            layer.chunk_ids.len().hash(&mut hasher);
            for &chunk_id in &layer.chunk_ids {
                let Some(chunk) = artifact.chunks.get(chunk_id) else {
                    u64::MAX.hash(&mut hasher);
                    continue;
                };
                hash_rect(chunk.bounds, &mut hasher);
                chunk.properties.transform.hash(&mut hasher);
                chunk.properties.clip.hash(&mut hasher);
                chunk.properties.effect.hash(&mut hasher);
                chunk.properties.scroll.hash(&mut hasher);
                chunk.z_order.hash(&mut hasher);
                for item in artifact
                    .display_items
                    .get(chunk.begin..chunk.end)
                    .unwrap_or(&[])
                {
                    item.client_index.hash(&mut hasher);
                }
            }
            hasher.finish()
        })
        .collect()
}

fn rects_equal(a: LayoutRect, b: LayoutRect) -> bool {
    a.x.to_bits() == b.x.to_bits()
        && a.y.to_bits() == b.y.to_bits()
        && a.width.to_bits() == b.width.to_bits()
        && a.height.to_bits() == b.height.to_bits()
}

/// Everything a layer recording reads that no single paint client or layer
/// owns: the layer list, the property trees and self clips.
fn global_fingerprint(artifact: &PaintArtifact, layers: &[CompositorLayer]) -> u64 {
    use std::fmt::Write as _;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    layers.len().hash(&mut hasher);
    for layer in layers {
        layer.properties.transform.hash(&mut hasher);
        layer.properties.clip.hash(&mut hasher);
        layer.properties.effect.hash(&mut hasher);
        layer.properties.scroll.hash(&mut hasher);
        layer.sticky_owner.hash(&mut hasher);
        layer.effect_owner.hash(&mut hasher);
        layer.transform_owner.hash(&mut hasher);
        layer.scroll_host.hash(&mut hasher);
    }
    artifact.nodes.len().hash(&mut hasher);
    artifact.self_clip.hash(&mut hasher);
    // Property nodes carry floats without `Hash`; their `Debug` form is
    // exact (shortest round-trip) and these trees are small.
    let mut text = String::new();
    let _ = write!(
        text,
        "{:?}{:?}{:?}{:?}",
        artifact.properties.transforms,
        artifact.properties.clips,
        artifact.properties.effects,
        artifact.properties.scrolls
    );
    text.hash(&mut hasher);
    artifact.viewport_scroll.0.to_bits().hash(&mut hasher);
    artifact.viewport_scroll.1.to_bits().hash(&mut hasher);
    hasher.finish()
}

/// Paint clients whose recording may differ from the previous artifact's,
/// for keeping unchanged compositor layer recordings across frames (Blink
/// keeps display items of clients that were not invalidated; cc re-rasters
/// only invalidated layers). A client is marked when its own paint inputs
/// changed, or when it shares a containing block (inline shaping, bidi
/// reordering, decorations and inline backgrounds read across that block's
/// inline content) or a table with a changed client. `None` means repaint
/// every layer: a different node count, a different canvas background, or
/// a changed client under a filter or with a resize indicator (effects that
/// read across clients).
pub fn paint_dirty_clients(prev: &PaintArtifact, cur: &PaintArtifact) -> Option<Vec<bool>> {
    use w3cos_std::component::ComponentKind;
    use w3cos_std::style::Display;
    let n = cur.nodes.len();
    if prev.nodes.len() != n
        || prev.canvas_background != cur.canvas_background
        || prev.canvas_background_style != cur.canvas_background_style
        || prev.canvas_background_source != cur.canvas_background_source
        || prev.body_index != cur.body_index
        || prev.canvas_background_positioning_rect != cur.canvas_background_positioning_rect
        || prev.viewport_scroll != cur.viewport_scroll
    {
        return None;
    }
    let item_hashes = |artifact: &PaintArtifact| {
        let mut hashes = vec![0u64; n];
        for item in &artifact.display_items {
            if let Some(slot) = hashes.get_mut(item.client_index) {
                let mut hasher = w3cos_std::fxhash::FxHasher::default();
                slot.hash(&mut hasher);
                hash_rect(item.visual_rect, &mut hasher);
                item.chunk_id.hash(&mut hasher);
                *slot = hasher.finish() | 1;
            }
        }
        hashes
    };
    let (prev_items, cur_items) = (item_hashes(prev), item_hashes(cur));
    let column_equal = |a: &[crate::paint_artifact::ColumnFragment],
                        b: &[crate::paint_artifact::ColumnFragment]| {
        a.len() == b.len()
            && a.iter().zip(b).all(|(a, b)| {
                rects_equal(a.visual_rect, b.visual_rect)
                    && a.translate_x.to_bits() == b.translate_x.to_bits()
                    && a.translate_y.to_bits() == b.translate_y.to_bits()
                    && a.clip_top.to_bits() == b.clip_top.to_bits()
                    && a.clip_bottom.to_bits() == b.clip_bottom.to_bits()
            })
    };
    let text_equal = |a: &[(LayoutRect, crate::layout_ng::FragmentText)],
                      b: &[(LayoutRect, crate::layout_ng::FragmentText)]| {
        a.len() == b.len()
            && a.iter()
                .zip(b)
                .all(|(a, b)| rects_equal(a.0, b.0) && a.1 == b.1)
    };
    let rect_equal = |a: Option<LayoutRect>, b: Option<LayoutRect>| match (a, b) {
        (Some(a), Some(b)) => rects_equal(a, b),
        (None, None) => true,
        _ => false,
    };
    let inline_level = |display: Display| {
        matches!(
            display,
            Display::Inline | Display::InlineBlock | Display::InlineFlex
        ) || display == Display::Contents
    };
    let volatile = |b: &crate::paint_artifact::PaintNode| {
        matches!(
            b.kind,
            ComponentKind::Image { .. }
                | ComponentKind::Canvas { .. }
                | ComponentKind::SvgDocument { .. }
                | ComponentKind::TextInput { .. }
        ) || b
            .style
            .background_image
            .as_deref()
            .is_some_and(|value| value.contains("url("))
            || b.style
                .custom_properties
                .as_ref()
                .is_some_and(|properties| properties.values().any(|value| value.contains("url(")))
    };
    let get = |v: &[bool], i: usize| v.get(i).copied().unwrap_or(false);
    let mut changed = Vec::new();
    for index in 0..n {
        let (a, b) = (&prev.nodes[index], &cur.nodes[index]);
        // Geometry first: it is the cheap check and the common change.
        let same = rect_equal(
            prev.rect_by_index.get(index).copied().flatten(),
            cur.rect_by_index.get(index).copied().flatten(),
        ) && prev_items[index] == cur_items[index]
            && a.parent == b.parent
            && a.sticky_counter_signal == b.sticky_counter_signal
            && a.kind == b.kind
            && a.style == b.style
            && prev.node_properties.get(index) == cur.node_properties.get(index)
            && prev.self_clip.get(index) == cur.self_clip.get(index)
            && prev.z_order.get(index) == cur.z_order.get(index)
            && prev.sticky_owner.get(index) == cur.sticky_owner.get(index)
            && prev.paint_order.get(index) == cur.paint_order.get(index)
            && column_equal(
                prev.column_fragments
                    .get(index)
                    .map_or(&[][..], Vec::as_slice),
                cur.column_fragments
                    .get(index)
                    .map_or(&[][..], Vec::as_slice),
            )
            && text_equal(
                prev.text_fragments
                    .get(index)
                    .map_or(&[][..], Vec::as_slice),
                cur.text_fragments.get(index).map_or(&[][..], Vec::as_slice),
            )
            && get(&prev.vertical_text, index) == get(&cur.vertical_text, index);
        // Clients painting resources the artifact does not describe
        // (decoded `url()` images, canvas bitmaps, SVG documents, form
        // control values) repaint every frame; their geometry did not change, so
        // their containing block keeps its recording.
        if same {
            if volatile(b) {
                changed.push((index, true));
            }
        } else {
            // A block-level box whose own paint inputs are unchanged and only
            // moved does not change what its siblings paint; each moved
            // descendant is compared on its own.
            let moved_only = a.parent == b.parent
                && a.sticky_counter_signal == b.sticky_counter_signal
                && a.kind == b.kind
                && a.style == b.style
                && prev.node_properties.get(index) == cur.node_properties.get(index)
                && prev.self_clip.get(index) == cur.self_clip.get(index)
                && prev.z_order.get(index) == cur.z_order.get(index)
                && prev.sticky_owner.get(index) == cur.sticky_owner.get(index)
                && prev.paint_order.get(index) == cur.paint_order.get(index)
                && get(&prev.vertical_text, index) == get(&cur.vertical_text, index)
                && !matches!(b.kind, ComponentKind::Text { .. })
                && !inline_level(b.style.display)
                && prev.text_fragments.get(index).is_none_or(Vec::is_empty)
                && cur.text_fragments.get(index).is_none_or(Vec::is_empty);
            changed.push((index, moved_only));
            // Past this many changed clients most layers re-record anyway;
            // stop diffing and repaint everything.
            if changed.len() > n / 8 + 64 {
                return None;
            }
        }
    }
    if cur
        .nodes
        .iter()
        .enumerate()
        .any(|(index, node)| node.parent.is_some_and(|parent| parent >= index))
    {
        return None;
    }
    let mut dirty = vec![false; n];
    if changed.is_empty() {
        return Some(dirty);
    }
    let reads_across_clients = |artifact: &PaintArtifact, index: usize| {
        let node = &artifact.nodes[index];
        if node
            .style
            .custom_properties
            .as_ref()
            .is_some_and(|properties| {
                properties.contains_key(w3cos_dom::css_style::RESIZE_PROPERTY)
            })
        {
            return true;
        }
        let mut effect = artifact
            .node_properties
            .get(index)
            .map_or(0, |properties| properties.effect);
        while effect != 0 {
            let Some(node) = artifact.properties.effects.get(effect) else {
                break;
            };
            if node.filter.is_some() {
                return true;
            }
            if node.parent == effect {
                break;
            }
            effect = node.parent;
        }
        false
    };
    let table_like = |display: Display| matches!(display, Display::Table | Display::InlineTable);
    let mut roots = Vec::new();
    for &(index, moved_only) in &changed {
        if reads_across_clients(prev, index) || reads_across_clients(cur, index) {
            return None;
        }
        dirty[index] = true;
        if !moved_only {
            // The nearest containing block above the changed client.
            let mut block = cur.nodes[index].parent;
            while let Some(candidate) = block {
                if !inline_level(cur.nodes[candidate].style.display) {
                    break;
                }
                block = cur.nodes[candidate].parent;
            }
            roots.push(block.unwrap_or(index));
        }
        let mut ancestor = cur.nodes[index].parent;
        while let Some(candidate) = ancestor {
            if table_like(cur.nodes[candidate].style.display) {
                roots.push(candidate);
            }
            ancestor = cur.nodes[candidate].parent;
        }
        dirty[index] = true;
    }
    // Mark every descendant of a root; nodes are in tree order, so a node's
    // parent is visited before it.
    let mut root_mark = vec![false; n];
    for root in roots {
        root_mark[root] = true;
    }
    for index in 0..n {
        let inherited = cur.nodes[index]
            .parent
            .is_some_and(|parent| root_mark[parent]);
        if root_mark[index] || inherited {
            root_mark[index] = true;
            dirty[index] = true;
        }
    }
    Some(dirty)
}

pub fn build_layers(artifact: &PaintArtifact) -> Vec<CompositorLayer> {
    let mut layers = Vec::new();
    let mut chunk_ids = (0..artifact.chunks.len()).collect::<Vec<_>>();
    chunk_ids.sort_by(|left, right| {
        let client_index = |chunk_id: &usize| {
            artifact
                .chunks
                .get(*chunk_id)
                .and_then(|chunk| artifact.display_items.get(chunk.begin))
                .map(|item| item.client_index)
                .unwrap_or(usize::MAX)
        };
        artifact
            .paint_order_key(client_index(left))
            .cmp(artifact.paint_order_key(client_index(right)))
    });
    for chunk_id in chunk_ids {
        let chunk = &artifact.chunks[chunk_id];
        let sticky = artifact
            .display_items
            .get(chunk.begin)
            .and_then(|item| artifact.sticky_owner.get(item.client_index).copied())
            .flatten();
        let same_as_current = layers.last().is_some_and(|layer: &CompositorLayer| {
            layer.properties == chunk.properties
                && layer.sticky_owner == sticky
                && layer
                    .client_indices
                    .first()
                    .zip(artifact.display_items.get(chunk.begin))
                    .is_some_and(|(previous, item)| {
                        artifact.viewport_attached(*previous)
                            == artifact.viewport_attached(item.client_index)
                    })
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
    for (index, pieces) in artifact.text_fragments.iter().enumerate() {
        for (rect, text) in pieces {
            index.hash(&mut hasher);
            hash_rect(*rect, &mut hasher);
            text.hash(&mut hasher);
        }
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
    // Which chain a box paints its own border and background under is not
    // derivable from `node_properties` alone once an overflow clip is in play,
    // so hash it explicitly rather than trusting the clip tree to imply it.
    for clip in &artifact.self_clip {
        clip.hash(&mut hasher);
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
        effect.isolates_surface.hash(&mut hasher);
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
    style.has_box_shadow().hash(hasher);
    style.backdrop_blur.is_some().hash(hasher);
    style.text_shadows.len().hash(hasher);
    style.will_change.transform.hash(hasher);
    style.will_change.opacity.hash(hasher);
    style.will_change.filter.hash(hasher);
    style.will_change.scroll_position.hash(hasher);
    std::mem::discriminant(&style.contain).hash(hasher);
    std::mem::discriminant(&style.text_align).hash(hasher);
    std::mem::discriminant(&style.white_space).hash(hasher);
    style.line_height.to_bits().hash(hasher);
    style.letter_spacing.to_bits().hash(hasher);
    style.word_spacing.to_bits().hash(hasher);
    std::mem::discriminant(&style.text_decoration).hash(hasher);
    style.outline_width.to_bits().hash(hasher);
    style.outline_color.r.hash(hasher);
    style.outline_color.g.hash(hasher);
    style.outline_color.b.hash(hasher);
    style.outline_color.a.hash(hasher);
    std::mem::discriminant(&style.outline_style).hash(hasher);
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
    use w3cos_std::style::{Float, Overflow, Position, Style, Transform2D};

    use crate::paint_artifact::PaintNode;

    #[test]
    fn outline_color_and_line_style_invalidate_paint_fingerprint() {
        let fingerprint = |style: &Style| {
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            hash_paint_style(style, &mut hash);
            hash.finish()
        };
        let base = Style {
            outline_width: 4.0,
            outline_style: w3cos_std::style::OutlineStyle::Solid,
            outline_color: w3cos_std::Color::rgb(128, 0, 128),
            ..Style::default()
        };
        let recolored = Style {
            outline_color: w3cos_std::Color::BLACK,
            ..base.clone()
        };
        let dashed = Style {
            outline_style: w3cos_std::style::OutlineStyle::Dashed,
            ..base.clone()
        };
        assert_ne!(fingerprint(&base), fingerprint(&recolored));
        assert_ne!(fingerprint(&base), fingerprint(&dashed));
    }

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
    fn layers_preserve_css_paint_order_across_scroll_properties() {
        let mut positioned_style = Style::default();
        positioned_style.position = Position::Absolute;
        let mut float_style = Style::default();
        float_style.float = Float::Left;
        float_style.overflow = Overflow::Scroll;
        let artifact = PaintArtifact::build(
            [
                PaintNode {
                    kind: ComponentKind::Column,
                    style: Style::default(),
                    parent: None,
                    sticky_counter_signal: None,
                },
                PaintNode {
                    kind: ComponentKind::Column,
                    style: positioned_style,
                    parent: Some(0),
                    sticky_counter_signal: None,
                },
                PaintNode {
                    kind: ComponentKind::Column,
                    style: float_style,
                    parent: Some(0),
                    sticky_counter_signal: None,
                },
                PaintNode {
                    kind: ComponentKind::Text {
                        content: "float".into(),
                    },
                    style: Style::default(),
                    parent: Some(2),
                    sticky_counter_signal: None,
                },
            ],
            &[
                (rect(0.0), 0),
                (rect(0.0), 1),
                (rect(0.0), 2),
                (rect(0.0), 3),
            ],
            1,
        );

        let painted_clients = build_layers(&artifact)
            .into_iter()
            .flat_map(|layer| layer.client_indices)
            .collect::<Vec<_>>();
        assert_eq!(painted_clients, vec![0, 2, 3, 1]);
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

    #[cfg(feature = "skia")]
    #[test]
    fn presenter_cache_injection_replays_opacity() {
        let artifact = red_layer_tree();
        let nodes = skia_nodes(&artifact);
        let font = fontdue::Font::from_bytes(
            include_bytes!("../assets/Inter-Regular.ttf").as_slice(),
            fontdue::FontSettings::default(),
        )
        .unwrap();
        let typeface = crate::font_face::pinned_font_mgr()
            .new_from_data(include_bytes!("../assets/Inter-Regular.ttf"), None)
            .expect("test typeface");
        let mut cache = crate::render_skia::RetainedSkiaCache::default();
        let mut surface = skia_safe::Surface::new_raster_n32_premul((8, 8)).unwrap();
        let empty = std::collections::HashMap::new();
        crate::render_skia::replay_frame(
            surface.canvas(),
            &typeface,
            crate::render_skia::ReplayFrame {
                nodes: &nodes,
                metrics_font: &font,
                scroll_info: &[],
                text_input_values: &empty,
                focused_index: None,
                background: w3cos_std::color::Color::WHITE,
                artifact: Some(&artifact),
                retained: Some(&mut cache),
                compositor_overrides: None,
                scale_factor: 1.0,
            },
        );
        assert_eq!(cache.full_scene_rebuilds(), 1);
        assert_eq!(cache.compositor_replays(), 0);

        let mut overrides = CompositorOverrides::default();
        overrides.opacity.insert(1, 0.2);
        crate::render_skia::replay_frame(
            surface.canvas(),
            &typeface,
            crate::render_skia::ReplayFrame {
                nodes: &nodes,
                metrics_font: &font,
                scroll_info: &[],
                text_input_values: &empty,
                focused_index: None,
                background: w3cos_std::color::Color::WHITE,
                artifact: Some(&artifact),
                retained: Some(&mut cache),
                compositor_overrides: Some(&overrides),
                scale_factor: 1.0,
            },
        );
        assert_eq!(cache.full_scene_rebuilds(), 1);
        assert_eq!(cache.compositor_replays(), 1);

        let unused = crate::render_skia::RetainedSkiaCache::default();
        crate::render_skia::replay_frame(
            surface.canvas(),
            &typeface,
            crate::render_skia::ReplayFrame {
                nodes: &nodes,
                metrics_font: &font,
                scroll_info: &[],
                text_input_values: &empty,
                focused_index: None,
                background: w3cos_std::color::Color::WHITE,
                artifact: Some(&artifact),
                retained: None,
                compositor_overrides: Some(&overrides),
                scale_factor: 1.0,
            },
        );
        assert_eq!(unused.full_scene_rebuilds(), 0);
        assert_eq!(unused.compositor_replays(), 0);
    }
}
