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
    /// After a `Rebuild` sync: per layer, the offset (CSS px) its bounds,
    /// chunks and clips all moved by since the recordings were made, when
    /// that move is the layer's only change besides its clients' paint.
    translated: Option<Vec<Option<(f32, f32)>>>,
    /// The clip tree's rects when the recordings were made.
    clip_rects: Vec<Option<LayoutRect>>,
    /// The chunk paint order `layers` were grouped from
    /// (`W3COS_LAYER_REUSE`).
    chunk_order: Vec<usize>,
    /// `serial` of the artifact `layers` were last synced from.
    artifact_serial: u64,
    /// Per chunk, what grouping reads (`W3COS_LAYER_PARTITION_KEYS`), from
    /// the artifact `layers` were last grouped from.
    partition_keys: Vec<ChunkGroupKey>,
    /// Every display item's client, from that artifact.
    partition_clients: Vec<usize>,
    /// Each chunk's paint-order class in that artifact.
    partition_classes: Vec<usize>,
}

/// What `build_layers_in_order` reads from one chunk to decide whether it
/// joins the current layer: properties, sticky owner, whether its first
/// client is viewport-attached, and its item count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ChunkGroupKey {
    properties: PaintProperties,
    sticky: Option<usize>,
    viewport: bool,
    items: usize,
}

impl RetainedLayerTree {
    pub fn sync(
        &mut self,
        artifact: &PaintArtifact,
        scroll_offsets: &HashMap<usize, (f32, f32)>,
        overrides: &CompositorOverrides,
    ) -> LayerPaintAction {
        self.sync_known_changed(artifact, scroll_offsets, overrides, false)
    }

    /// `sync` when the caller already knows a paint client changed since the
    /// recordings (a paint diff marked one dirty): the frame re-records, so
    /// the whole-artifact content fingerprint that would only confirm it is
    /// skipped (Blink's PaintArtifactCompositor takes its repaint path from
    /// the paint invalidation, not from rehashing the artifact). The stored
    /// fingerprint is cleared, so the next frame without that knowledge
    /// rebuilds once.
    pub fn sync_known_changed(
        &mut self,
        artifact: &PaintArtifact,
        scroll_offsets: &HashMap<usize, (f32, f32)>,
        overrides: &CompositorOverrides,
        known_changed: bool,
    ) -> LayerPaintAction {
        let reuse_mode = layer_reuse_mode();
        let keys_mode = if reuse_mode != 0 {
            layer_partition_keys_mode()
        } else {
            0
        };
        let (classes, keys, clients) = if keys_mode != 0 {
            partition_inputs(artifact)
        } else {
            (Vec::new(), Vec::new(), Vec::new())
        };
        // Same classes as last frame give the same stable order: keep it
        // instead of sorting again.
        let same_classes = keys_mode != 0
            && !self.layers.is_empty()
            && classes == self.partition_classes
            && self.chunk_order.len() == classes.len();
        let order = if same_classes {
            self.chunk_order.clone()
        } else if keys_mode != 0 {
            crate::paint_artifact::stable_order_by_class(&classes)
        } else {
            chunk_paint_order(artifact)
        };
        let mut reused = false;
        let old_bounds: Vec<LayoutRect> = self.layers.iter().map(|layer| layer.bounds).collect();
        let runs = artifact.append_runs_since(self.artifact_serial);
        let holds = if reuse_mode == 0 {
            false
        } else if keys_mode != 0 {
            // Blink's PaintArtifactCompositor regroups only when the chunks'
            // grouping inputs changed: compare them flat against the inputs
            // the kept layers were grouped from, in one pass with no per-layer
            // walk.
            let holds = !self.layers.is_empty()
                && same_classes
                && keys == self.partition_keys
                && clients == self.partition_clients;
            if keys_mode == 2 {
                let checked =
                    partition_still_holds(artifact, &order, &self.chunk_order, &self.layers, runs);
                if holds && !checked {
                    eprintln!(
                        "[layer-partition-keys] mismatch: keys hold, the full check does not"
                    );
                    LAYER_PARTITION_KEYS_MISMATCHES.with(|count| count.set(count.get() + 1));
                }
                holds && checked
            } else {
                holds
            }
        } else {
            partition_still_holds(artifact, &order, &self.chunk_order, &self.layers, runs)
        };
        if keys_mode != 0 {
            self.partition_keys = keys;
            self.partition_clients = clients;
            self.partition_classes = classes;
        } else if !self.partition_keys.is_empty() {
            self.partition_keys.clear();
            self.partition_clients.clear();
            self.partition_classes.clear();
        }
        let layers = if holds {
            // Blink's PaintArtifactCompositor keeps its pending layers when
            // the chunks group the same way; only bounds and owners refresh.
            let mut layers = std::mem::take(&mut self.layers);
            refresh_layers(artifact, &mut layers);
            reused = true;
            if reuse_mode == 2 {
                let full = build_layers_in_order(artifact, &order);
                if !layers_equal(&layers, &full) {
                    eprintln!("[layer-reuse] mismatch against the full layer build");
                    LAYER_REUSE_MISMATCHES.with(|count| count.set(count.get() + 1));
                    reused = false;
                    layers = full;
                }
            }
            layers
        } else {
            build_layers_in_order(artifact, &order)
        };
        if reused {
            LAYER_REUSE_HITS.with(|count| count.set(count.get() + 1));
        }
        let content = if known_changed {
            if layer_sync_fast_verify()
                && content_fingerprint(artifact, &layers) | 1 == self.content_fingerprint
                && self.recordings_valid
            {
                eprintln!("[layer-sync-fast] content unchanged under a dirty client");
            }
            0
        } else {
            // Non-zero: 0 marks a skipped fingerprint.
            content_fingerprint(artifact, &layers) | 1
        };
        let compositor = compositor_fingerprint(artifact, &layers, scroll_offsets, overrides);
        let (global, chunk_hashes) = if self.track_partial {
            (
                global_fingerprint(artifact, &layers),
                layer_chunk_hashes(artifact, &layers),
            )
        } else {
            (0, Vec::new())
        };
        let clip_rects: Vec<Option<LayoutRect>> = if self.track_partial {
            artifact
                .properties
                .clips
                .iter()
                .map(|clip| clip.rect)
                .collect()
        } else {
            Vec::new()
        };
        // A reused partition has the same clients per layer by construction.
        // A reuse that verify rejected took the old layers: rebuild.
        let structure_changed = if reuse_mode != 0 && self.layers.is_empty() {
            !reused
        } else {
            self.layers.len() != layers.len()
                || self
                    .layers
                    .iter()
                    .zip(&layers)
                    .any(|(old, new)| old.client_indices != new.client_indices)
        };
        // Under the paint cull band, boxes crossing the band's edge change
        // some layers' clients every few frames: compare layer by layer
        // (a layer whose clients changed re-records) instead of treating
        // the whole tree as changed, as cc re-rasters only changed layers.
        let per_layer_clients = structure_changed
            && crate::paint_artifact::paint_cull_rect_mode() != 0
            && self.layers.len() == layers.len()
            && !(reuse_mode != 0 && self.layers.is_empty());
        let comparable = self.track_partial
            && self.recordings_valid
            && !old_bounds.is_empty()
            && (!structure_changed || per_layer_clients)
            && global == self.global_fingerprint
            && self.clip_rects.len() == clip_rects.len();
        let moves: Option<Vec<LayerMove>> = comparable.then(|| {
            let mut chain = ClipChainMoves::new(artifact, &self.clip_rects, &clip_rects);
            old_bounds
                .iter()
                .zip(&layers)
                .zip(self.layer_chunk_hashes.iter().zip(&chunk_hashes))
                .enumerate()
                .map(|(layer_index, ((old, new), (old_chunks, new_chunks)))| {
                    if per_layer_clients
                        && self.layers[layer_index].client_indices != new.client_indices
                    {
                        return LayerMove::Changed;
                    }
                    if old_chunks != new_chunks
                        || old.width.to_bits() != new.bounds.width.to_bits()
                        || old.height.to_bits() != new.bounds.height.to_bits()
                    {
                        return LayerMove::Changed;
                    }
                    let delta = (new.bounds.x - old.x, new.bounds.y - old.y);
                    let clips_moved = new
                        .client_indices
                        .iter()
                        .all(|&client| chain.client_moved_by(client, delta));
                    match (clips_moved, delta == (0.0, 0.0)) {
                        (false, _) => LayerMove::Changed,
                        (true, true) => LayerMove::Same,
                        (true, false) => LayerMove::Moved(delta),
                    }
                })
                .collect()
        });
        self.partial = moves
            .as_ref()
            .map(|moves| moves.iter().map(|m| matches!(m, LayerMove::Same)).collect());
        self.translated = moves.map(|moves| {
            moves
                .iter()
                .map(|m| match m {
                    LayerMove::Moved(delta) => Some(*delta),
                    _ => None,
                })
                .collect()
        });
        self.clip_rects = clip_rects;
        self.global_fingerprint = global;
        self.layer_chunk_hashes = chunk_hashes;
        self.layers = layers;
        self.chunk_order = order;
        self.artifact_serial = artifact.serial();

        if known_changed
            || !self.recordings_valid
            || structure_changed
            || content != self.content_fingerprint
        {
            // After a skipped fingerprint, the next computed one cannot match.
            self.content_fingerprint = if known_changed { 0 } else { content };
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

    /// Per layer, the offset it moved by as a whole (see `translated`).
    pub fn translated_layers(&self) -> Option<&[Option<(f32, f32)>]> {
        self.translated.as_deref()
    }
}

/// On by default (`W3COS_LAYER_SYNC_FAST=0` turns it off): the Skia replay
/// passes the pending paint invalidation to `sync_known_changed`. `=verify`
/// also computes the fingerprint and reports a dirty frame whose content
/// hashed the same.
pub(crate) fn layer_sync_fast_enabled() -> bool {
    #[cfg(test)]
    if let Some(on) = LAYER_SYNC_FAST_OVERRIDE.with(std::cell::Cell::get) {
        return on;
    }
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var("W3COS_LAYER_SYNC_FAST").map_or(true, |v| v != "0" && v != "false")
    })
}

/// `W3COS_LAYER_REUSE=1`: keep last frame's layer partition when every
/// chunk still groups the same way (same paint order, properties, sticky
/// owner, viewport attachment and clients); `=verify` also builds the
/// layers in full and reports a difference. Off by default.
pub(crate) fn layer_reuse_mode() -> u8 {
    #[cfg(test)]
    if let Some(mode) = LAYER_REUSE_OVERRIDE.with(std::cell::Cell::get) {
        return mode;
    }
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| match std::env::var("W3COS_LAYER_REUSE").as_deref() {
        Ok("1") | Ok("true") => 1,
        Ok("verify") => 2,
        _ => 0,
    })
}

#[cfg(test)]
thread_local! {
    pub(crate) static LAYER_REUSE_OVERRIDE: std::cell::Cell<Option<u8>> =
        const { std::cell::Cell::new(None) };
}

thread_local! {
    pub(crate) static LAYER_REUSE_HITS: std::cell::Cell<u64> =
        const { std::cell::Cell::new(0) };
    pub(crate) static LAYER_REUSE_MISMATCHES: std::cell::Cell<u64> =
        const { std::cell::Cell::new(0) };
}

fn layer_sync_fast_verify() -> bool {
    #[cfg(test)]
    if LAYER_SYNC_FAST_QUIET.with(std::cell::Cell::get) {
        return false;
    }
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("W3COS_LAYER_SYNC_FAST").is_ok_and(|v| v == "verify"))
}

#[cfg(test)]
thread_local! {
    pub(crate) static LAYER_SYNC_FAST_OVERRIDE: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
    /// A test that reports a known change on purpose silences the verify
    /// note (`=verify` gates treat it as a finding).
    static LAYER_SYNC_FAST_QUIET: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

enum LayerMove {
    Same,
    Moved((f32, f32)),
    Changed,
}

/// Per clip node, the offset its rect and every recorded ancestor clip
/// moved by (`None`: they moved apart or changed). Scrollport clips are
/// applied when compositing, not recorded, so they do not count.
struct ClipChainMoves<'a> {
    artifact: &'a PaintArtifact,
    old: &'a [Option<LayoutRect>],
    new: &'a [Option<LayoutRect>],
    scrollport: Vec<bool>,
    memo: Vec<Option<Option<ClipMove>>>,
    /// The last (self clip, contents clip, wanted move) and its answer.
    last: Option<((usize, usize, ClipMove), bool)>,
}

#[derive(Clone, Copy, PartialEq)]
enum ClipMove {
    /// No recorded clip, or only clips without a rect.
    Any,
    By(u32, u32),
}

impl<'a> ClipChainMoves<'a> {
    fn new(
        artifact: &'a PaintArtifact,
        old: &'a [Option<LayoutRect>],
        new: &'a [Option<LayoutRect>],
    ) -> Self {
        let mut scrollport = vec![false; new.len()];
        for scroll in &artifact.properties.scrolls {
            if let Some(clip) = scroll.clip
                && let Some(slot) = scrollport.get_mut(clip)
            {
                *slot = true;
            }
        }
        Self {
            artifact,
            old,
            new,
            scrollport,
            memo: vec![None; new.len()],
            last: None,
        }
    }

    fn chain(&mut self, clip: usize) -> Option<ClipMove> {
        if clip == 0 || clip >= self.new.len() {
            return Some(ClipMove::Any);
        }
        if let Some(known) = self.memo[clip] {
            return known;
        }
        let own = if self.scrollport[clip] {
            Some(ClipMove::Any)
        } else {
            match (self.old[clip], self.new[clip]) {
                (None, None) => Some(ClipMove::Any),
                (Some(o), Some(n))
                    if o.width.to_bits() == n.width.to_bits()
                        && o.height.to_bits() == n.height.to_bits() =>
                {
                    Some(ClipMove::By((n.x - o.x).to_bits(), (n.y - o.y).to_bits()))
                }
                _ => None,
            }
        };
        let parent = self.artifact.properties.clips[clip].parent;
        let up = if parent == clip {
            Some(ClipMove::Any)
        } else {
            self.chain(parent)
        };
        let result = match (own, up) {
            (Some(ClipMove::Any), up) => up,
            (own, Some(ClipMove::Any)) => own,
            (Some(a), Some(b)) if a == b => Some(a),
            _ => None,
        };
        self.memo[clip] = Some(result);
        result
    }

    fn client_moved_by(&mut self, client: usize, delta: (f32, f32)) -> bool {
        let want = ClipMove::By(delta.0.to_bits(), delta.1.to_bits());
        let zero = delta == (0.0, 0.0);
        let self_clip = self
            .artifact
            .self_clip
            .get(client)
            .copied()
            .unwrap_or_default();
        let contents_clip = self
            .artifact
            .node_properties
            .get(client)
            .map_or(0, |properties| properties.clip);
        // Neighbouring clients mostly share both clips.
        let key = (self_clip, contents_clip, want);
        if let Some((last, moved)) = self.last
            && last == key
        {
            return moved;
        }
        let moved = [self_clip, contents_clip]
            .into_iter()
            .all(|clip| match self.chain(clip) {
                Some(ClipMove::Any) => true,
                Some(ClipMove::By(x, y)) => {
                    ClipMove::By(x, y) == want
                        || (zero && f32::from_bits(x) == 0.0 && f32::from_bits(y) == 0.0)
                }
                None => false,
            });
        self.last = Some((key, moved));
        moved
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
                // Relative to the layer, so a layer that moved as a whole
                // keeps its hashes.
                hash_rect(
                    LayoutRect {
                        x: chunk.bounds.x - layer.bounds.x,
                        y: chunk.bounds.y - layer.bounds.y,
                        ..chunk.bounds
                    },
                    &mut hasher,
                );
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
    // Clip rects are compared per layer (`ClipChainMoves`); only the clip
    // tree's shape is global.
    let clip_parents: Vec<usize> = artifact
        .properties
        .clips
        .iter()
        .map(|clip| clip.parent)
        .collect();
    // Scrollport rects are read when compositing (from the current
    // artifact), never by a recording.
    let scroll_shape: Vec<_> = artifact
        .properties
        .scrolls
        .iter()
        .map(|scroll| (scroll.parent, scroll.host_index, scroll.clip))
        .collect();
    let _ = write!(
        text,
        "{:?}{:?}{:?}{:?}",
        artifact.properties.transforms, clip_parents, artifact.properties.effects, scroll_shape
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
    paint_diff_clients(prev, cur).map(|diff| {
        diff.dirty
            .iter()
            .zip(&diff.moved)
            .map(|(dirty, moved)| *dirty || moved.is_some())
            .collect()
    })
}

/// `paint_dirty_clients`, with clients that only moved kept apart: a moved
/// client paints exactly what it painted before, offset by its move (its
/// display items, text fragments and every other paint input are equal
/// relative to its box), so a layer whose clients all moved by one offset
/// can keep its recording under that offset, as cc keeps a layer's raster
/// when only its position changes.
pub struct PaintDiff {
    /// Clients whose paint changed (not counting moves).
    pub dirty: Vec<bool>,
    /// Clients that only moved, with the move in CSS px.
    pub moved: Vec<Option<(f32, f32)>>,
}

/// Raster invalidation for a [`PaintDiff`] (Blink's
/// `RasterInvalidator`): per changed or moved client, a conservative rect
/// (document CSS px) covering what it painted in `prev` and what it paints
/// in `cur`. Pixels outside the union of these rects are the same in both
/// frames. `None`: some change cannot be bounded this way (a property-tree
/// or sticky change, shadows, filters, transforms, or a client that stopped
/// painting), so the whole frame must be composited.
/// One raster invalidation rect for the clients `first..end` (document CSS
/// px; see [`paint_damage`]).
pub type ClientDamage = (usize, usize, LayoutRect);

/// A damage entry that is a run of clients which only moved, by one offset
/// (`W3COS_PAINT_DAMAGE_BLIT`): its position in the damage list, the move
/// (CSS px) and the union of the clients' old ink.
pub type MovedSpan = (usize, (f32, f32), LayoutRect);

thread_local! {
    /// The moved spans of the last bounded damage, with that damage's length.
    static DAMAGE_MOVES: std::cell::RefCell<Option<(usize, Vec<MovedSpan>)>> =
        const { std::cell::RefCell::new(None) };
}

/// `W3COS_PAINT_DAMAGE_BLIT=1|verify` (needs `W3COS_PAINT_DAMAGE=1`): a run
/// of clients that only moved by a whole device pixel offset is copied from
/// the previous frame's pixels instead of composited again (WebKit's
/// blit-on-scroll); only changed clients, uncovered strips and whatever
/// overlaps the copied pixels are composited. `verify` also composites the
/// full frame off-screen and compares.
pub fn paint_damage_blit_mode() -> u8 {
    #[cfg(test)]
    if let Some(mode) = PAINT_DAMAGE_BLIT_OVERRIDE.with(std::cell::Cell::get) {
        return mode;
    }
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(
        || match std::env::var("W3COS_PAINT_DAMAGE_BLIT").ok().as_deref() {
            Some("1") => 1,
            Some("verify") => 2,
            _ => 0,
        },
    )
}

#[cfg(test)]
thread_local! {
    pub(crate) static PAINT_DAMAGE_SPANS_OVERRIDE: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
    pub(crate) static PAINT_DAMAGE_BLIT_OVERRIDE: std::cell::Cell<Option<u8>> =
        const { std::cell::Cell::new(None) };
}

/// `W3COS_PAINT_NARROW_ROOTS=1`: a changed text client does not dirty
/// every descendant of its containing block when those are only plain text
/// clients (see `paint_diff_clients_with`). Off by default.
pub(crate) fn paint_narrow_roots_enabled() -> bool {
    #[cfg(test)]
    if let Some(on) = PAINT_NARROW_ROOTS_OVERRIDE.with(std::cell::Cell::get) {
        return on;
    }
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("W3COS_PAINT_NARROW_ROOTS").is_ok_and(|v| v == "1"))
}

#[cfg(test)]
thread_local! {
    pub(crate) static PAINT_NARROW_ROOTS_OVERRIDE: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

thread_local! {
    /// Changed clients whose containing block was not repainted whole.
    pub(crate) static PAINT_NARROW_ROOTS_HITS: std::cell::Cell<u64> =
        const { std::cell::Cell::new(0) };
}

/// `W3COS_PAINT_RESIZE_STRIPS=1`: a resized plain box whose background
/// image list is only `none` layers (what `background: <color>` sets) takes
/// the resized-box strips instead of its whole old and new ink. Off by
/// default.
pub(crate) fn resize_strips_enabled() -> bool {
    #[cfg(test)]
    if let Some(on) = RESIZE_STRIPS_OVERRIDE.with(std::cell::Cell::get) {
        return on;
    }
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("W3COS_PAINT_RESIZE_STRIPS").is_ok_and(|v| v == "1"))
}

thread_local! {
    /// Resized-box checks that passed a `none`-only image list.
    pub(crate) static RESIZE_STRIPS_NONE_IMAGES: std::cell::Cell<u64> =
        const { std::cell::Cell::new(0) };
}

#[cfg(test)]
thread_local! {
    pub(crate) static RESIZE_STRIPS_OVERRIDE: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

/// The moved spans recorded with a damage list of `len` entries.
pub fn take_damage_moves(len: usize) -> Option<Vec<MovedSpan>> {
    DAMAGE_MOVES
        .with(|slot| slot.borrow_mut().take())
        .filter(|(recorded, _)| *recorded == len)
        .map(|(_, moves)| moves)
}

/// `W3COS_PAINT_DAMAGE_SPANS=0` turns it off: consecutive clients that only
/// moved, by one offset, share one damage rect covering their old and new
/// paint (Blink invalidates a subtree that moved as a whole by its visual
/// rect, not box by box).
fn paint_damage_spans_enabled() -> bool {
    #[cfg(test)]
    if let Some(on) = PAINT_DAMAGE_SPANS_OVERRIDE.with(std::cell::Cell::get) {
        return on;
    }
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("W3COS_PAINT_DAMAGE_SPANS").map_or(true, |v| v != "0"))
}

/// Per client of the artifact whose serial is `serial`, its
/// `client_ink_bounds` (`W3COS_PAINT_DAMAGE_INK_CACHE`).
struct InkCache {
    serial: u64,
    /// `None`: not measured yet (measure in the previous artifact).
    ink: Vec<Option<Ink>>,
}

type Ink = Result<Option<LayoutRect>, ()>;

/// Where `paint_damage_with` reads a client's old ink and records its new.
trait InkSource {
    fn old(&mut self, prev: &PaintArtifact, index: usize) -> Ink;
    fn store(&mut self, index: usize, ink: Ink);
}

/// Measure every time, keep nothing.
struct MeasureInk;

impl InkSource for MeasureInk {
    #[inline(always)]
    fn old(&mut self, prev: &PaintArtifact, index: usize) -> Ink {
        client_ink_bounds(prev, index)
    }
    #[inline(always)]
    fn store(&mut self, _index: usize, _ink: Ink) {}
}

impl InkSource for Vec<Option<Ink>> {
    #[inline]
    fn old(&mut self, prev: &PaintArtifact, index: usize) -> Ink {
        match self[index] {
            Some(ink) => ink,
            None => client_ink_bounds(prev, index),
        }
    }
    #[inline]
    fn store(&mut self, index: usize, ink: Ink) {
        self[index] = Some(ink);
    }
}

thread_local! {
    static INK_CACHE: std::cell::RefCell<Option<InkCache>> = const { std::cell::RefCell::new(None) };
    static INK_CACHE_MISMATCHES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// `W3COS_PAINT_DAMAGE_INK_CACHE=1`: keep each client's ink bounds from
/// the previous frame, so a moved client's old ink is a lookup and its new
/// ink is the old one shifted by the move (Blink keeps each fragment's
/// visual rect the same way), instead of measuring every moved box twice.
/// `=verify` also measures and compares (within 0.01 px: a shifted rect may
/// round differently from a measured one).
fn ink_cache_mode() -> u8 {
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(
        || match std::env::var("W3COS_PAINT_DAMAGE_INK_CACHE").as_deref() {
            Ok("1") => 1,
            Ok("verify") => 2,
            _ => 0,
        },
    )
}

fn ink_close(a: Result<Option<LayoutRect>, ()>, b: Result<Option<LayoutRect>, ()>) -> bool {
    match (a, b) {
        (Ok(Some(a)), Ok(Some(b))) => {
            (a.x - b.x).abs() <= 0.01
                && (a.y - b.y).abs() <= 0.01
                && (a.width - b.width).abs() <= 0.01
                && (a.height - b.height).abs() <= 0.01
        }
        (a, b) => a == b,
    }
}

pub fn paint_damage(
    prev: &PaintArtifact,
    cur: &PaintArtifact,
    diff: &PaintDiff,
) -> Option<Vec<ClientDamage>> {
    paint_damage_cached(prev, cur, (&diff.dirty, &diff.moved), None)
}

/// `paint_damage` (through the ink cache when it is on) that gives up with
/// `None` as soon as `stop` accepts the rects added since its last call.
pub(crate) fn paint_damage_cached(
    prev: &PaintArtifact,
    cur: &PaintArtifact,
    diff: (&[bool], &[Option<(f32, f32)>]),
    mut stop: Option<&mut dyn FnMut(&[ClientDamage]) -> bool>,
) -> Option<Vec<ClientDamage>> {
    let mode = ink_cache_mode();
    if mode == 0 {
        return paint_damage_with(prev, cur, diff, &mut MeasureInk, stop.take());
    }
    INK_CACHE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let mut cache = slot
            .take()
            .filter(|cache| cache.serial == prev.serial() && cache.ink.len() == prev.nodes.len())
            .unwrap_or_else(|| InkCache {
                serial: prev.serial(),
                ink: vec![None; prev.nodes.len()],
            });
        if mode == 2 {
            let mut bad = 0;
            for (index, ink) in cache.ink.iter_mut().enumerate() {
                let measured = client_ink_bounds(prev, index);
                if let Some(kept) = *ink
                    && !ink_close(kept, measured)
                {
                    bad += 1;
                    *ink = Some(measured);
                }
            }
            if bad > 0 {
                eprintln!("[ink-cache-verify] mismatch: {bad} clients' ink bounds differ");
                INK_CACHE_MISMATCHES.with(|count| count.set(count.get() + bad));
            }
        }
        // Clients past `prev`'s painted nothing there.
        cache.ink.resize(cur.nodes.len(), Some(Ok(None)));
        let damage = paint_damage_with(prev, cur, diff, &mut cache.ink, stop.take());
        // Bounded: every changed client's entry now holds its `cur` ink.
        // Unbounded: the walk stopped part way, so drop the cache.
        if damage.is_some() {
            cache.serial = cur.serial();
            *slot = Some(cache);
        }
        damage
    })
}

/// `paint_damage` (measured, not cached) that gives up with `None` as soon
/// as `stop` accepts the rects added since its last call: the off-surface
/// check needs every rect only when none of them is on the surface.
pub(crate) fn paint_damage_until(
    prev: &PaintArtifact,
    cur: &PaintArtifact,
    diff: (&[bool], &[Option<(f32, f32)>]),
    stop: &mut dyn FnMut(&[ClientDamage]) -> bool,
) -> Option<Vec<ClientDamage>> {
    paint_damage_with(prev, cur, diff, &mut MeasureInk, Some(stop))
}

/// The first client at or after `from` that changed or moved (`len` when
/// none does). Clients past `diff`'s tables count as changed.
#[inline(never)]
fn next_changed_client(
    dirty: &[bool],
    moved: &[Option<(f32, f32)>],
    from: usize,
    len: usize,
) -> usize {
    let known = len.min(dirty.len()).min(moved.len());
    let (dirty, moved) = (&dirty[..known], &moved[..known]);
    let mut index = from;
    while index < known {
        if dirty[index] || moved[index].is_some() {
            return index;
        }
        index += 1;
    }
    index.max(from).min(len)
}

/// `paint_damage`, reading old ink from `ink` (`prev`'s per client) when
/// given and advancing it to `cur`'s: changed clients are measured, moved
/// ones shift, the rest keep theirs.
fn paint_damage_with<S: InkSource>(
    prev: &PaintArtifact,
    cur: &PaintArtifact,
    (dirty_by_client, moved_by_client): (&[bool], &[Option<(f32, f32)>]),
    ink: &mut S,
    mut stop: Option<&mut dyn FnMut(&[ClientDamage]) -> bool>,
) -> Option<Vec<ClientDamage>> {
    let mut checked = 0;
    let spans = paint_damage_spans_enabled();
    let record_moves = paint_damage_blit_mode() != 0;
    DAMAGE_MOVES.with(|slot| *slot.borrow_mut() = None);
    let mut moves: Vec<MovedSpan> = Vec::new();
    let mut damage = Vec::new();
    // The open run of moved clients: first, end, offset bits, old ink union.
    let mut span: Option<(usize, usize, (u32, u32), (f32, f32), LayoutRect)> = None;
    let mut flush = |span: &mut Option<(usize, usize, (u32, u32), (f32, f32), LayoutRect)>,
                     damage: &mut Vec<ClientDamage>| {
        if let Some((first, end, _, (dx, dy), old)) = span.take() {
            let new = LayoutRect {
                x: old.x + dx,
                y: old.y + dy,
                ..old
            };
            if record_moves {
                moves.push((damage.len(), (dx, dy), old));
            }
            damage.push((first, end, union_rect(old, new)));
        }
    };
    let shifted = |old: Option<LayoutRect>, (dx, dy): (f32, f32)| {
        old.map(|old| LayoutRect {
            x: old.x + dx,
            y: old.y + dy,
            ..old
        })
    };
    let len = cur.nodes.len();
    let mut next = 0;
    while next < len {
        // Skip the clients that neither changed nor moved in one tight scan.
        let index = next_changed_client(dirty_by_client, moved_by_client, next, len);
        if index >= len {
            break;
        }
        next = index + 1;
        // Checked in doubling batches: each check has a fixed cost.
        if let Some(stop) = stop.as_mut()
            && damage.len() > checked
            && damage.len() >= 2 * checked
        {
            if stop(&damage[checked..]) {
                return None;
            }
            checked = damage.len();
        }
        let dirty = dirty_by_client.get(index).copied().unwrap_or(true);
        // A client that only moved (`moved_by`: same style, properties,
        // size and display items relative to its box) paints its old ink
        // shifted by the move.
        let shift = if dirty {
            None
        } else {
            moved_by_client.get(index).copied().flatten()
        };
        if prev.sticky_owner.get(index).copied().flatten().is_some()
            || cur.sticky_owner.get(index).copied().flatten().is_some()
            || (shift.is_none()
                && prev.node_properties.get(index) != cur.node_properties.get(index))
        {
            return None;
        }
        if shift.is_none()
            && let Some(strips) = resized_box_damage(prev, cur, index)
        {
            ink.store(index, client_ink_bounds(cur, index));
            flush(&mut span, &mut damage);
            damage.extend(strips.into_iter().map(|strip| (index, index + 1, strip)));
            continue;
        }
        let old = ink.old(prev, index).ok()?;
        if spans && let Some(delta) = shift {
            ink.store(index, Ok(shifted(old, delta)));
            let Some(old) = old else {
                continue;
            };
            let bits = (delta.0.to_bits(), delta.1.to_bits());
            match &mut span {
                Some((_, end, offset, _, union)) if *end == index && *offset == bits => {
                    *end = index + 1;
                    *union = union_rect(*union, old);
                }
                _ => {
                    flush(&mut span, &mut damage);
                    span = Some((index, index + 1, bits, delta, old));
                }
            }
            continue;
        }
        flush(&mut span, &mut damage);
        let new = match shift {
            Some(delta) => shifted(old, delta),
            None => client_ink_bounds(cur, index).ok()?,
        };
        ink.store(index, Ok(new));
        match (old, new) {
            (None, None) => {}
            // It painted and now does not: no current layer places it.
            (Some(_), None) => return None,
            (None, Some(new)) => damage.push((index, index + 1, new)),
            (Some(old), Some(new)) => damage.push((index, index + 1, union_rect(old, new))),
        }
    }
    flush(&mut span, &mut damage);
    if let Some(stop) = stop
        && damage.len() > checked
        && stop(&damage[checked..])
    {
        return None;
    }
    if record_moves {
        DAMAGE_MOVES.with(|slot| *slot.borrow_mut() = Some((damage.len(), moves)));
    }
    Some(damage)
}

/// Blink's incremental invalidation for a box that only changed size: when
/// a plain block box (solid background, square corners, no scrollbars, no
/// images) kept its paint inputs and its origin, only the strips along the
/// right and bottom edges that it grew into or shrank from, plus its borders
/// and outline there, can change. `None`: not such a change.
fn resized_box_damage(
    prev: &PaintArtifact,
    cur: &PaintArtifact,
    index: usize,
) -> Option<Vec<LayoutRect>> {
    use w3cos_std::component::ComponentKind;
    use w3cos_std::style::{Display, Overflow, Visibility};
    let (a, b) = (prev.nodes.get(index)?, cur.nodes.get(index)?);
    let old = prev.rect_by_index.get(index).copied().flatten()?;
    let new = cur.rect_by_index.get(index).copied().flatten()?;
    if old.x.to_bits() != new.x.to_bits() || old.y.to_bits() != new.y.to_bits() {
        return None;
    }
    let style = b.style.as_style();
    let plain_kind = matches!(
        b.kind,
        ComponentKind::Root | ComponentKind::Column | ComponentKind::Row | ComponentKind::Box
    );
    let visible = |overflow: Option<Overflow>| overflow.is_none_or(|o| o == Overflow::Visible);
    // The inherited list-style-image is read by the DOM when it builds
    // marker boxes, which are clients of their own; it paints nothing here.
    let painted_by_name = |name: &str| {
        name != "--w3cos-internal-list-style-image"
            && [
                "radius",
                "background",
                "border",
                "outline",
                "image",
                "mask",
                "clip",
                "shadow",
                "gradient",
            ]
            .iter()
            .any(|word| name.contains(word))
    };
    // `background: <color>` leaves a `none` image layer: it paints no image
    // (`W3COS_PAINT_RESIZE_STRIPS=1`; off by default it counts as one).
    let no_image = style.background_image.as_deref().is_none_or(|value| {
        resize_strips_enabled()
            && value.split(',').all(|layer| {
                let layer = layer.trim();
                layer.is_empty() || layer.eq_ignore_ascii_case("none")
            })
    });
    if style.background_image.is_some() && no_image {
        RESIZE_STRIPS_NONE_IMAGES.with(|count| count.set(count.get() + 1));
    }
    let plain = plain_kind
        && a.kind == b.kind
        && a.style == b.style
        && a.parent == b.parent
        && style.visibility == Visibility::Visible
        && !matches!(
            style.display,
            Display::Inline
                | Display::InlineBlock
                | Display::InlineFlex
                | Display::Contents
                | Display::ListItem
        )
        && style.overflow == Overflow::Visible
        && visible(style.overflow_x)
        && visible(style.overflow_y)
        && style.border_radius == 0.0
        && [
            style.border_top_left_radius,
            style.border_top_right_radius,
            style.border_bottom_right_radius,
            style.border_bottom_left_radius,
        ]
        .iter()
        .all(|radius| radius.is_none_or(|radius| radius == 0.0))
        && no_image
        && style
            .custom_properties
            .as_ref()
            .is_none_or(|properties| !properties.keys().any(|name| painted_by_name(name)))
        && prev.self_clip.get(index) == cur.self_clip.get(index)
        && prev.z_order.get(index) == cur.z_order.get(index)
        && prev.paint_order.get(index) == cur.paint_order.get(index)
        && [prev, cur].iter().all(|artifact| {
            artifact.text_fragments.get(index).is_none_or(Vec::is_empty)
                && artifact
                    .column_fragments
                    .get(index)
                    .is_none_or(|f| f.len() <= 1)
        })
        && old.x.to_bits() == new.x.to_bits()
        && old.y.to_bits() == new.y.to_bits();
    if !plain {
        if crate::render_skia::paint_damage_trace() && !matches!(b.kind, ComponentKind::Text { .. })
        {
            let checks = [
                ("plain kind", plain_kind),
                ("same kind", a.kind == b.kind),
                ("same style", a.style == b.style),
                ("same parent", a.parent == b.parent),
                ("visible", style.visibility == Visibility::Visible),
                (
                    "block display",
                    !matches!(
                        style.display,
                        Display::Inline
                            | Display::InlineBlock
                            | Display::InlineFlex
                            | Display::Contents
                            | Display::ListItem
                    ),
                ),
                (
                    "overflow visible",
                    style.overflow == Overflow::Visible
                        && visible(style.overflow_x)
                        && visible(style.overflow_y),
                ),
                ("no radius", style.border_radius == 0.0),
                ("no image", no_image),
                (
                    "no painted custom property",
                    style.custom_properties.as_ref().is_none_or(|properties| {
                        !properties.keys().any(|name| painted_by_name(name))
                    }),
                ),
                (
                    "same clip",
                    prev.self_clip.get(index) == cur.self_clip.get(index),
                ),
                ("same z", prev.z_order.get(index) == cur.z_order.get(index)),
                (
                    "same paint order",
                    prev.paint_order.get(index) == cur.paint_order.get(index),
                ),
                (
                    "no own fragments",
                    [prev, cur].iter().all(|artifact| {
                        artifact.text_fragments.get(index).is_none_or(Vec::is_empty)
                            && artifact
                                .column_fragments
                                .get(index)
                                .is_none_or(|f| f.len() <= 1)
                    }),
                ),
            ];
            let failed: Vec<&str> = checks
                .iter()
                .filter(|(_, ok)| !ok)
                .map(|(name, _)| *name)
                .collect();
            let names: Vec<String> = style
                .custom_properties
                .as_ref()
                .map(|properties| {
                    properties
                        .keys()
                        .take(8)
                        .map(|name| name.to_string())
                        .collect()
                })
                .unwrap_or_default();
            eprintln!(
                "[paint-damage-trace]   resized box {index} not plain: failed {failed:?}; rect {old:?} -> {new:?}; custom properties {names:?}"
            );
        }
        return None;
    }
    let border = [
        style.border_width,
        style.border_top_width.unwrap_or(0.0),
        style.border_right_width.unwrap_or(0.0),
        style.border_bottom_width.unwrap_or(0.0),
        style.border_left_width.unwrap_or(0.0),
    ]
    .into_iter()
    .fold(0.0f32, f32::max);
    let reach = 16.0 + border + 2.0 * style.outline_width.max(0.0);
    if !reach.is_finite() || !old.width.is_finite() || !old.height.is_finite() {
        return None;
    }
    let (left, top) = (new.x - reach, new.y - reach);
    let right = old.x + old.width.max(new.width) + reach;
    let bottom = old.y + old.height.max(new.height) + reach;
    let mut strips = Vec::new();
    if old.width.to_bits() != new.width.to_bits() {
        let x = old.x + old.width.min(new.width) - reach;
        strips.push(LayoutRect {
            x,
            y: top,
            width: right - x,
            height: bottom - top,
        });
    }
    if old.height.to_bits() != new.height.to_bits() {
        let y = old.y + old.height.min(new.height) - reach;
        strips.push(LayoutRect {
            x: left,
            y,
            width: right - left,
            height: bottom - y,
        });
    }
    Some(strips)
}

/// A rect (document CSS px) holding everything client `index` paints, or
/// `Ok(None)` when it paints nothing. Its border box, column fragments and
/// text fragments, grown by a margin for glyph overhang, outlines and
/// anti-aliasing. `Err`: paint that can reach farther (shadows, filters,
/// transforms, backdrop effects).
pub(crate) fn client_ink_bounds(
    artifact: &PaintArtifact,
    index: usize,
) -> Result<Option<LayoutRect>, ()> {
    let Some(node) = artifact.nodes.get(index) else {
        return Ok(None);
    };
    let Some(rect) = artifact.rect_by_index.get(index).copied().flatten() else {
        return Ok(None);
    };
    let style = node.style.as_style();
    // As `append_node`: a hidden box has no display items.
    if style.visibility != w3cos_std::style::Visibility::Visible {
        return Ok(None);
    }
    if style.filter.is_some()
        || style.box_shadow.is_some()
        || !style.box_shadows.is_empty()
        || style.backdrop_blur.is_some()
        || !style.text_shadows.is_empty()
        || !style.transform.is_identity()
    {
        return Err(());
    }
    let mut bounds = rect;
    for fragment in artifact.column_fragments.get(index).into_iter().flatten() {
        bounds = union_rect(bounds, fragment.visual_rect);
    }
    for (fragment, _) in artifact.text_fragments.get(index).into_iter().flatten() {
        bounds = union_rect(bounds, *fragment);
    }
    let margin = 16f32.max(style.font_size) + 2.0 * style.outline_width.max(0.0);
    if !margin.is_finite() || !bounds.x.is_finite() || !bounds.y.is_finite() {
        return Err(());
    }
    Ok(Some(LayoutRect {
        x: bounds.x - margin,
        y: bounds.y - margin,
        width: bounds.width.max(0.0) + 2.0 * margin,
        height: bounds.height.max(0.0) + 2.0 * margin,
    }))
}

/// On by default (`W3COS_PAINT_DIFF_RUNS=0` turns it off): clients inside an aligned run the artifact
/// copied from `prev` (same index, rect, inputs and display items; see
/// `PaintArtifact::append_runs_against`) skip the display-item hashing and
/// the input comparison (Blink's PaintController skips a cached
/// subsequence the same way). `=verify` also diffs in full and compares.
fn paint_diff_runs_mode() -> u8 {
    #[cfg(test)]
    if let Some(mode) = PAINT_DIFF_RUNS_OVERRIDE.with(std::cell::Cell::get) {
        return mode;
    }
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(
        || match std::env::var("W3COS_PAINT_DIFF_RUNS").ok().as_deref() {
            Some("0" | "false") => 0,
            Some("verify") => 2,
            _ => 1,
        },
    )
}

#[cfg(test)]
thread_local! {
    pub(crate) static PAINT_DIFF_RUNS_OVERRIDE: std::cell::Cell<Option<u8>> =
        const { std::cell::Cell::new(None) };
}

thread_local! {
    /// (diffs that used runs, verify mismatches) since the thread started.
    pub(crate) static PAINT_DIFF_RUNS_STATS: std::cell::Cell<(u64, u64)> =
        const { std::cell::Cell::new((0, 0)) };
}

fn diffs_equal(a: &Option<PaintDiff>, b: &Option<PaintDiff>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            a.dirty == b.dirty
                && a.moved.len() == b.moved.len()
                && a.moved.iter().zip(&b.moved).all(|(x, y)| match (x, y) {
                    (None, None) => true,
                    (Some(x), Some(y)) => {
                        x.0.to_bits() == y.0.to_bits() && x.1.to_bits() == y.1.to_bits()
                    }
                    _ => false,
                })
        }
        _ => false,
    }
}

/// `W3COS_PAINT_DIFF_SPARSE=1`: with runs, the diff visits inside a copied
/// run only the clients the artifact build listed as volatile or that have
/// text fragments, instead of every client of the run. `=verify` also diffs
/// densely and compares. Off by default; the build records the volatile
/// list only when this is on.
pub(crate) fn paint_diff_sparse_mode() -> u8 {
    #[cfg(test)]
    if let Some(mode) = PAINT_DIFF_SPARSE_OVERRIDE.with(std::cell::Cell::get) {
        return mode;
    }
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(
        || match std::env::var("W3COS_PAINT_DIFF_SPARSE").ok().as_deref() {
            Some("1") => 1,
            Some("verify") => 2,
            _ => 0,
        },
    )
}

#[cfg(test)]
thread_local! {
    pub(crate) static PAINT_DIFF_SPARSE_OVERRIDE: std::cell::Cell<Option<u8>> =
        const { std::cell::Cell::new(None) };
}

thread_local! {
    pub(crate) static PAINT_DIFF_SPARSE_MISMATCHES: std::cell::Cell<u64> =
        const { std::cell::Cell::new(0) };
}

pub fn paint_diff_clients(prev: &PaintArtifact, cur: &PaintArtifact) -> Option<PaintDiff> {
    let mode = paint_diff_runs_mode();
    let runs = (mode != 0).then(|| cur.append_runs_against(prev)).flatten();
    let Some(runs) = runs else {
        return paint_diff_clients_with(prev, cur, None, false);
    };
    let sparse_mode = paint_diff_sparse_mode();
    let sparse = sparse_mode != 0 && cur.volatile_nodes().is_some();
    if sparse_mode == 2 {
        eprintln!(
            "[paint-diff-sparse] sparse {sparse} volatile {:?} runs {} covering {} of {}",
            cur.volatile_nodes().map(<[u32]>::len),
            runs.len(),
            runs.iter()
                .map(|run| run[1].saturating_sub(run[0]) as usize)
                .sum::<usize>(),
            cur.nodes.len()
        );
    }
    let diff = paint_diff_clients_with(prev, cur, Some(runs), sparse);
    if sparse && sparse_mode == 2 {
        let dense = paint_diff_clients_with(prev, cur, Some(runs), false);
        if !diffs_equal(&diff, &dense) {
            eprintln!("[paint-diff-sparse] mismatch against the dense diff");
            PAINT_DIFF_SPARSE_MISMATCHES.with(|count| count.set(count.get() + 1));
            return dense;
        }
    }
    PAINT_DIFF_RUNS_STATS.with(|stats| {
        let (used, mismatched) = stats.get();
        stats.set((used + 1, mismatched));
    });
    if mode == 2 {
        let full = paint_diff_clients_with(prev, cur, None, false);
        let equal = diffs_equal(&diff, &full);
        if !equal {
            eprintln!("[paint-diff-runs] mismatch against the full diff");
            PAINT_DIFF_RUNS_STATS.with(|stats| {
                let (used, mismatched) = stats.get();
                stats.set((used, mismatched + 1));
            });
            return full;
        }
    }
    diff
}

fn paint_diff_clients_with(
    prev: &PaintArtifact,
    cur: &PaintArtifact,
    runs: Option<&[[u32; 4]]>,
    sparse: bool,
) -> Option<PaintDiff> {
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
    // Display item rects relative to their client's box: with equal boxes
    // this is the absolute comparison, and a moved client compares equal.
    // Clients inside a copied run: equal rects, inputs and display items.
    let mut in_run = Vec::new();
    // Display items outside the runs, as index ranges (the same in both).
    let mut item_spans = Vec::new();
    if let Some(runs) = runs {
        in_run = vec![false; n];
        let mut next = 0usize;
        for &[start, end, items_start, items_end] in runs {
            let (start, end) = (start as usize, end.min(n as u32) as usize);
            if start < end {
                in_run[start..end].fill(true);
            }
            if items_start as usize > next {
                item_spans.push((next, items_start as usize));
            }
            next = next.max(items_end as usize);
        }
        item_spans.push((next, usize::MAX));
    }
    let item_hashes = |artifact: &PaintArtifact| {
        let mut hashes = vec![0u64; n];
        let all = [(0usize, usize::MAX)];
        let spans: &[(usize, usize)] = if runs.is_some() { &item_spans } else { &all };
        let items = &artifact.display_items;
        for item in spans.iter().flat_map(|&(a, b)| {
            items
                .get(a.min(items.len())..b.min(items.len()))
                .into_iter()
                .flatten()
        }) {
            if let Some(slot) = hashes.get_mut(item.client_index) {
                let (ox, oy) = artifact
                    .rect_by_index
                    .get(item.client_index)
                    .copied()
                    .flatten()
                    .map_or((0.0, 0.0), |rect| (rect.x, rect.y));
                let mut hasher = w3cos_std::fxhash::FxHasher::default();
                slot.hash(&mut hasher);
                hash_rect(
                    LayoutRect {
                        x: item.visual_rect.x - ox,
                        y: item.visual_rect.y - oy,
                        ..item.visual_rect
                    },
                    &mut hasher,
                );
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
    // Inherited custom properties share one map across many boxes: scan
    // each map once per diff.
    let url_maps = std::cell::RefCell::new(w3cos_std::fxhash::FxHashMap::<usize, bool>::default());
    // Siblings usually share their parent's map: check the last one first.
    let last_url_map = std::cell::Cell::new((usize::MAX, false));
    let has_url = |properties: &w3cos_std::style::CustomProperties| {
        let key = properties.identity();
        let (last_key, last_found) = last_url_map.get();
        if last_key == key {
            return last_found;
        }
        let found = match url_maps.borrow().get(&key) {
            Some(&known) => Some(known),
            None => None,
        };
        let found = found.unwrap_or_else(|| {
            let found = properties.values().any(|value| value.contains("url("));
            url_maps.borrow_mut().insert(key, found);
            found
        });
        last_url_map.set((key, found));
        found
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
            || b.style.custom_properties.as_ref().is_some_and(has_url)
    };
    let get = |v: &[bool], i: usize| v.get(i).copied().unwrap_or(false);
    // The build already compared these inputs and outputs per node against
    // this `prev` (see `PaintArtifact::append_unchanged_against`).
    let unchanged = cur.append_unchanged_against(prev);
    let paint_inputs_equal = |index: usize,
                              a: &crate::paint_artifact::PaintNode,
                              b: &crate::paint_artifact::PaintNode| {
        unchanged.is_some_and(|unchanged| unchanged[index])
            || (a.parent == b.parent
                && a.sticky_counter_signal == b.sticky_counter_signal
                && a.kind == b.kind
                && a.style == b.style
                && prev.node_properties.get(index) == cur.node_properties.get(index)
                && prev.self_clip.get(index) == cur.self_clip.get(index)
                && prev.z_order.get(index) == cur.z_order.get(index)
                && prev.sticky_owner.get(index) == cur.sticky_owner.get(index)
                && prev.paint_order.get(index) == cur.paint_order.get(index))
    };
    // Remembered per box when the artifact was built.
    let viewport_attached = |index: usize| cur.viewport_attached(index);
    let mut changed = Vec::new();
    let mut moved: Vec<Option<(f32, f32)>> = vec![None; n];
    // One client; false: too many changed clients, give up the diff.
    let visit = |index: usize,
                 changed: &mut Vec<(usize, bool)>,
                 moved: &mut [Option<(f32, f32)>]|
     -> bool {
        let (a, b) = (&prev.nodes[index], &cur.nodes[index]);
        if in_run.get(index).copied().unwrap_or(false) {
            // Copied with its display items from `prev` at this index.
            let same = text_equal(
                prev.text_fragments
                    .get(index)
                    .map_or(&[][..], Vec::as_slice),
                cur.text_fragments.get(index).map_or(&[][..], Vec::as_slice),
            ) && get(&prev.vertical_text, index) == get(&cur.vertical_text, index);
            if same {
                if volatile(b) {
                    changed.push((index, true));
                }
                return true;
            }
        }
        // Geometry first: it is the cheap check and the common change.
        let same = rect_equal(
            prev.rect_by_index.get(index).copied().flatten(),
            cur.rect_by_index.get(index).copied().flatten(),
        ) && prev_items[index] == cur_items[index]
            && paint_inputs_equal(index, a, b)
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
        } else if let Some(delta) = moved_by(
            prev,
            cur,
            index,
            prev_items[index] == cur_items[index],
            &volatile,
            &viewport_attached,
        ) {
            moved[index] = Some(delta);
        } else {
            // A block-level box whose own paint inputs are unchanged and only
            // moved does not change what its siblings paint; each moved
            // descendant is compared on its own.
            let moved_only = paint_inputs_equal(index, a, b)
                && get(&prev.vertical_text, index) == get(&cur.vertical_text, index)
                && !matches!(b.kind, ComponentKind::Text { .. })
                && !inline_level(b.style.display)
                && prev.text_fragments.get(index).is_none_or(Vec::is_empty)
                && cur.text_fragments.get(index).is_none_or(Vec::is_empty);
            changed.push((index, moved_only));
            // Past this many changed clients most layers re-record anyway;
            // stop diffing and repaint everything.
            if changed.len() > n / 8 + 64 {
                return false;
            }
        }
        true
    };
    // The clients to visit: all, or (sparse) the gaps between copied runs
    // plus, inside a run, only a volatile client or one with text fragments
    // in either frame (the rest of a run cannot differ). One loop below, so
    // the visit stays inlined.
    let sparse_list = match (sparse, runs, cur.volatile_nodes()) {
        (true, Some(runs), Some(volatile)) => {
            let within = |list: &[u32], start: usize, end: usize| {
                let a = list.partition_point(|&i| (i as usize) < start);
                let b = list.partition_point(|&i| (i as usize) < end);
                a..b
            };
            let mut list: Vec<usize> = Vec::new();
            let mut index = 0usize;
            for &[start, end, _, _] in runs {
                let (start, end) = (start as usize, (end as usize).min(n));
                list.extend(index..start.min(n).max(index));
                let start = start.max(index);
                if start < end {
                    let from = list.len();
                    for picks in [volatile, prev.fragment_nodes(), cur.fragment_nodes()] {
                        list.extend(picks[within(picks, start, end)].iter().map(|&i| i as usize));
                    }
                    list[from..].sort_unstable();
                    let mut kept = from;
                    for at in from..list.len() {
                        if at == from || list[at] != list[kept - 1] {
                            list[kept] = list[at];
                            kept += 1;
                        }
                    }
                    list.truncate(kept);
                }
                index = index.max(end);
            }
            list.extend(index..n);
            Some(list)
        }
        _ => None,
    };
    let count = sparse_list.as_ref().map_or(n, Vec::len);
    for at in 0..count {
        let index = sparse_list.as_ref().map_or(at, |list| list[at]);
        // The common case, kept out of the (not inlined) visit: a client of
        // a copied run with equal text pieces.
        if in_run.get(index).copied().unwrap_or(false)
            && prev.text_fragments.get(index).is_none_or(Vec::is_empty)
            && cur.text_fragments.get(index).is_none_or(Vec::is_empty)
            && get(&prev.vertical_text, index) == get(&cur.vertical_text, index)
        {
            if volatile(&cur.nodes[index]) {
                changed.push((index, true));
            }
            continue;
        }
        if !visit(index, &mut changed, &mut moved) {
            return None;
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
    let (prev_filters, cur_filters) = (effects_under_filter(prev), effects_under_filter(cur));
    let under_filter = |artifact: &PaintArtifact, filters: &[bool], index: usize| {
        artifact
            .node_properties
            .get(index)
            .is_some_and(|properties| filters.get(properties.effect).copied().unwrap_or(false))
    };
    for index in 0..n {
        // A moved box paints the same pixels offset, resize indicator
        // included; only a filter above it reads across clients.
        if moved[index].is_some()
            && (under_filter(prev, &prev_filters, index) || under_filter(cur, &cur_filters, index))
        {
            return None;
        }
    }
    let mut dirty = vec![false; n];
    if changed.is_empty() {
        return Some(PaintDiff { dirty, moved });
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
    // `W3COS_PAINT_NARROW_ROOTS=1`: a containing block whose descendants
    // are only text clients that paint nothing across each other is not
    // repainted whole for one changed text; each descendant was compared on
    // its own above (Blink invalidates only the objects whose geometry or
    // style changed).
    let narrow = paint_narrow_roots_enabled();
    let mut narrow_memo = w3cos_std::fxhash::FxHashMap::<usize, bool>::default();
    let mut text_only_block = |root: usize| -> bool {
        *narrow_memo.entry(root).or_insert_with(|| {
            let plain_text = |artifact: &PaintArtifact, index: usize| {
                let node = &artifact.nodes[index];
                let style = node.style.as_style();
                let paints_nothing_itself = matches!(
                    node.kind,
                    ComponentKind::Column | ComponentKind::Row | ComponentKind::Box
                ) && !inline_level(style.display)
                    && style.border_width == 0.0
                    && [
                        style.border_top_width,
                        style.border_right_width,
                        style.border_bottom_width,
                        style.border_left_width,
                    ]
                    .iter()
                    .all(|width| width.is_none_or(|width| width == 0.0))
                    && style.outline_width == 0.0
                    && style.overflow == w3cos_std::style::Overflow::Visible
                    && style.clip.is_none()
                    && style.backdrop_blur.is_none()
                    && style.transform.is_identity();
                // A text run's own background paints inside its own
                // fragments, which it compares on its own.
                (matches!(node.kind, ComponentKind::Text { .. })
                    || (paints_nothing_itself && style.background.a == 0))
                    && style.background_image.as_deref().is_none_or(|value| {
                        value.split(',').all(|layer| {
                            let layer = layer.trim();
                            layer.is_empty() || layer.eq_ignore_ascii_case("none")
                        })
                    })
                    && style.box_shadow.is_none()
                    && style.box_shadows.is_empty()
                    && style.opacity >= 1.0
                    && style.filter.is_none()
                    // Text-run and font bookkeeping is the client's own;
                    // anything that paints (or reads) across clients is not.
                    && style.custom_properties.as_ref().is_none_or(|properties| {
                        properties.keys().all(|name| {
                            ![
                                "background",
                                "border",
                                "decoration",
                                "outline",
                                "shadow",
                                "vertical",
                                "float-line-bands",
                                "first-line",
                                "highlight",
                                "selection",
                                "resize",
                                "url",
                            ]
                            .iter()
                            .any(|word| name.contains(word))
                        })
                    })
            };
            let descends = |artifact: &PaintArtifact, mut index: usize| {
                while let Some(parent) = artifact.nodes[index].parent {
                    if parent == root {
                        return true;
                    }
                    if parent < root {
                        return false;
                    }
                    index = parent;
                }
                false
            };
            let root_plain = [prev, cur].iter().all(|artifact| {
                artifact.text_fragments.get(root).is_none_or(Vec::is_empty)
                    && !matches!(artifact.nodes[root].kind, ComponentKind::Text { .. })
            });
            let mut any = false;
            let mut index = root + 1;
            while root_plain && index < n {
                let inside = descends(cur, index);
                if inside != descends(prev, index) {
                    return false;
                }
                if !inside {
                    break;
                }
                if !plain_text(prev, index) || !plain_text(cur, index) {
                    return false;
                }
                any = true;
                index += 1;
            }
            root_plain && any
        })
    };
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
            let root = block.unwrap_or(index);
            if !(narrow && root != index && text_only_block(root)) {
                roots.push(root);
            } else {
                PAINT_NARROW_ROOTS_HITS.with(|count| count.set(count.get() + 1));
            }
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
    for (moved, dirty) in moved.iter_mut().zip(&dirty) {
        if *dirty {
            *moved = None;
        }
    }
    Some(PaintDiff { dirty, moved })
}

/// Per effect node, whether it or an ancestor effect has a filter.
fn effects_under_filter(artifact: &PaintArtifact) -> Vec<bool> {
    fn visit(
        effects: &[crate::paint_artifact::EffectNode],
        memo: &mut [Option<bool>],
        id: usize,
    ) -> bool {
        if let Some(known) = memo[id] {
            return known;
        }
        // Provisional entry guards a malformed cycle while recursing.
        memo[id] = Some(effects[id].filter.is_some());
        let parent = effects[id].parent;
        let result = effects[id].filter.is_some()
            || (id != 0 && parent != id && parent < effects.len() && visit(effects, memo, parent));
        memo[id] = Some(result);
        result
    }
    let effects = &artifact.properties.effects;
    let mut memo: Vec<Option<bool>> = vec![None; effects.len()];
    (0..effects.len())
        .map(|id| visit(effects, &mut memo, id))
        .collect()
}

/// The offset a client moved by when that is its only paint change: same
/// box size, same paint inputs, and its display items and text fragments
/// equal relative to its box. Clients whose paint reads the viewport (fixed
/// backgrounds, sticky or viewport-attached boxes), columns, or resources
/// the artifact does not describe never count as moved.
fn moved_by(
    prev: &PaintArtifact,
    cur: &PaintArtifact,
    index: usize,
    items_equal: bool,
    volatile: &dyn Fn(&crate::paint_artifact::PaintNode) -> bool,
    viewport_attached: &dyn Fn(usize) -> bool,
) -> Option<(f32, f32)> {
    if !items_equal {
        return None;
    }
    let (a, b) = (&prev.nodes[index], &cur.nodes[index]);
    let old = prev.rect_by_index.get(index).copied().flatten()?;
    let new = cur.rect_by_index.get(index).copied().flatten()?;
    if old.width.to_bits() != new.width.to_bits() || old.height.to_bits() != new.height.to_bits() {
        return None;
    }
    let delta = (new.x - old.x, new.y - old.y);
    if !delta.0.is_finite() || !delta.1.is_finite() || delta == (0.0, 0.0) {
        return None;
    }
    let get = |v: &[bool], i: usize| v.get(i).copied().unwrap_or(false);
    let same_inputs = (cur
        .append_unchanged_against(prev)
        .is_some_and(|unchanged| unchanged[index])
        || (a.parent == b.parent
            && a.sticky_counter_signal == b.sticky_counter_signal
            && a.kind == b.kind
            && a.style == b.style
            && prev.node_properties.get(index) == cur.node_properties.get(index)
            && prev.self_clip.get(index) == cur.self_clip.get(index)
            && prev.z_order.get(index) == cur.z_order.get(index)
            && prev.sticky_owner.get(index) == cur.sticky_owner.get(index)
            && prev.paint_order.get(index) == cur.paint_order.get(index)))
        && get(&prev.vertical_text, index) == get(&cur.vertical_text, index);
    if !same_inputs
        || cur.sticky_owner.get(index).copied().flatten().is_some()
        || prev.column_fragments.get(index).is_some_and(|f| !f.is_empty())
        || cur.column_fragments.get(index).is_some_and(|f| !f.is_empty())
        || volatile(b)
        // Gradients sit in the box's own positioning area and move with it;
        // fixed backgrounds read the viewport.
        || b.style
            .background_attachment
            .as_deref()
            .is_some_and(|value| value.contains("fixed"))
        || viewport_attached(index)
    {
        return None;
    }
    let empty: &[(LayoutRect, crate::layout_ng::FragmentText)] = &[];
    let old_text = prev.text_fragments.get(index).map_or(empty, Vec::as_slice);
    let new_text = cur.text_fragments.get(index).map_or(empty, Vec::as_slice);
    let text_moved = old_text.len() == new_text.len()
        && old_text.iter().zip(new_text).all(|(o, n)| {
            o.1 == n.1
                && (o.0.x - old.x).to_bits() == (n.0.x - new.x).to_bits()
                && (o.0.y - old.y).to_bits() == (n.0.y - new.y).to_bits()
                && o.0.width.to_bits() == n.0.width.to_bits()
                && o.0.height.to_bits() == n.0.height.to_bits()
        });
    text_moved.then_some(delta)
}

pub fn build_layers(artifact: &PaintArtifact) -> Vec<CompositorLayer> {
    build_layers_in_order(artifact, &chunk_paint_order(artifact))
}

/// Chunk ids in paint order. Stable: chunks of equal paint order keep their
/// order. Paint-order classes are dense ranks below the node count, so a
/// counting sort orders the chunks in linear time.
fn chunk_paint_order(artifact: &PaintArtifact) -> Vec<usize> {
    let classes = artifact
        .chunks
        .iter()
        .map(|chunk| {
            let client_index = artifact
                .display_items
                .get(chunk.begin)
                .map(|item| item.client_index)
                .unwrap_or(usize::MAX);
            artifact.paint_order_class(client_index) as usize
        })
        .collect::<Vec<_>>();
    crate::paint_artifact::stable_order_by_class(&classes)
}

/// In one pass over the chunks: each chunk's paint-order class (what
/// `chunk_paint_order` sorts by) and grouping key, and every display item's
/// client.
fn partition_inputs(artifact: &PaintArtifact) -> (Vec<usize>, Vec<ChunkGroupKey>, Vec<usize>) {
    let mut classes = Vec::with_capacity(artifact.chunks.len());
    let mut keys = Vec::with_capacity(artifact.chunks.len());
    for chunk in &artifact.chunks {
        let first = artifact
            .display_items
            .get(chunk.begin)
            .map(|item| item.client_index);
        classes.push(artifact.paint_order_class(first.unwrap_or(usize::MAX)) as usize);
        keys.push(ChunkGroupKey {
            properties: chunk.properties,
            sticky: first
                .and_then(|client| artifact.sticky_owner.get(client).copied())
                .flatten(),
            viewport: first.is_some_and(|client| artifact.viewport_attached(client)),
            items: chunk.end.saturating_sub(chunk.begin),
        });
    }
    let clients = artifact
        .display_items
        .iter()
        .map(|item| item.client_index)
        .collect();
    (classes, keys, clients)
}

/// `W3COS_LAYER_PARTITION_KEYS=1` (with `W3COS_LAYER_REUSE`): decide that the
/// kept layers still group the chunks by comparing each chunk's grouping
/// inputs and every item's client with the ones the layers were grouped
/// from, instead of walking the layers chunk by chunk. `=verify` also runs
/// the walk and reports a keys match the walk rejects. Off by default.
pub(crate) fn layer_partition_keys_mode() -> u8 {
    #[cfg(test)]
    if let Some(mode) = LAYER_PARTITION_KEYS_OVERRIDE.with(std::cell::Cell::get) {
        return mode;
    }
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(
        || match std::env::var("W3COS_LAYER_PARTITION_KEYS").ok().as_deref() {
            Some("1") => 1,
            Some("verify") => 2,
            _ => 0,
        },
    )
}

#[cfg(test)]
thread_local! {
    pub(crate) static LAYER_PARTITION_KEYS_OVERRIDE: std::cell::Cell<Option<u8>> =
        const { std::cell::Cell::new(None) };
}

thread_local! {
    static LAYER_PARTITION_KEYS_MISMATCHES: std::cell::Cell<u32> =
        const { std::cell::Cell::new(0) };
}

#[allow(dead_code)]
pub(crate) fn layer_partition_keys_mismatches() -> u32 {
    LAYER_PARTITION_KEYS_MISMATCHES.with(std::cell::Cell::get)
}

fn build_layers_in_order(artifact: &PaintArtifact, chunk_ids: &[usize]) -> Vec<CompositorLayer> {
    let mut layers = Vec::new();
    for &chunk_id in chunk_ids {
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
            layer.client_indices.extend(
                artifact.display_items[chunk.begin..chunk.end]
                    .iter()
                    .map(|item| item.client_index),
            );
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

/// Whether `build_layers_in_order(artifact, order)` would group the chunks
/// exactly as `layers` (built from `previous_order`) are: same order, and
/// every chunk joins or starts a layer as before with the same clients.
fn partition_still_holds(
    artifact: &PaintArtifact,
    order: &[usize],
    previous_order: &[usize],
    layers: &[CompositorLayer],
    runs: Option<&[[u32; 4]]>,
) -> bool {
    if layers.is_empty() || order != previous_order {
        return false;
    }
    // Per chunk: it lies inside a run the build copied from the artifact
    // `layers` came from, so its items, properties, sticky owners and
    // viewport attachment are last frame's (runs align every array, chunk
    // ids included).
    let mut copied = vec![false; artifact.chunks.len()];
    if let Some(runs) = runs {
        let mut run = 0;
        for (id, chunk) in artifact.chunks.iter().enumerate() {
            while run < runs.len() && (runs[run][3] as usize) < chunk.end {
                run += 1;
            }
            copied[id] = runs
                .get(run)
                .is_some_and(|r| r[2] as usize <= chunk.begin && chunk.end <= r[3] as usize);
        }
    }
    let mut position = 0;
    let mut previous_layer: Option<&CompositorLayer> = None;
    let mut previous_chunk_copied = true;
    let mut previous_first_copied = true;
    for layer in layers {
        let count = layer.chunk_ids.len();
        if count == 0 || order.get(position..position + count) != Some(layer.chunk_ids.as_slice()) {
            return false;
        }
        position += count;
        let mut offset = 0;
        let first_copied = copied.get(layer.chunk_ids[0]).copied().unwrap_or(false);
        for (nth, &chunk_id) in layer.chunk_ids.iter().enumerate() {
            let Some(chunk) = artifact.chunks.get(chunk_id) else {
                return false;
            };
            let chunk_copied = copied.get(chunk_id).copied().unwrap_or(false);
            // This chunk, the one before it in paint order and the layer's
            // first chunk are all unchanged: the join decision and the
            // clients are last frame's.
            // A layer's first chunk is also tested against the previous
            // layer, whose first client decides viewport attachment.
            if chunk_copied
                && previous_chunk_copied
                && first_copied
                && (nth > 0 || previous_first_copied)
            {
                offset += chunk.end - chunk.begin;
                if offset > layer.client_indices.len() {
                    return false;
                }
                continue;
            }
            previous_chunk_copied = chunk_copied;
            let Some(items) = artifact.display_items.get(chunk.begin..chunk.end) else {
                return false;
            };
            let first_item = artifact.display_items.get(chunk.begin);
            let sticky = first_item
                .and_then(|item| artifact.sticky_owner.get(item.client_index).copied())
                .flatten();
            // build_layers' same-layer test against a layer whose first
            // client so far is `first_client`.
            let joins = |properties: &PaintProperties,
                         sticky_owner: Option<usize>,
                         first_client: Option<usize>| {
                *properties == chunk.properties
                    && sticky_owner == sticky
                    && first_client.zip(first_item).is_some_and(|(client, item)| {
                        artifact.viewport_attached(client)
                            == artifact.viewport_attached(item.client_index)
                    })
            };
            if nth == 0 {
                if previous_layer.is_some_and(|previous| {
                    joins(
                        &previous.properties,
                        previous.sticky_owner,
                        previous.client_indices.first().copied(),
                    )
                }) || layer.properties != chunk.properties
                    || layer.sticky_owner != sticky
                {
                    return false;
                }
            } else if !joins(
                &layer.properties,
                layer.sticky_owner,
                (offset > 0).then(|| layer.client_indices[0]),
            ) {
                return false;
            }
            let end = offset + items.len();
            let Some(clients) = layer.client_indices.get(offset..end) else {
                return false;
            };
            if !clients
                .iter()
                .zip(items)
                .all(|(client, item)| *client == item.client_index)
            {
                return false;
            }
            offset = end;
        }
        if offset != layer.client_indices.len() {
            return false;
        }
        previous_layer = Some(layer);
        previous_first_copied = first_copied;
    }
    position == order.len()
}

/// Recomputes what a kept partition does not fix: bounds and owners.
fn refresh_layers(artifact: &PaintArtifact, layers: &mut [CompositorLayer]) {
    for layer in layers {
        let mut chunks = layer.chunk_ids.iter().map(|&id| &artifact.chunks[id]);
        if let Some(first) = chunks.next() {
            layer.bounds = chunks.fold(first.bounds, |bounds, chunk| {
                union_rect(bounds, chunk.bounds)
            });
        }
        layer.scroll_host = artifact
            .properties
            .scrolls
            .get(layer.properties.scroll)
            .and_then(|node| node.host_index);
        layer.effect_owner =
            property_owner(artifact, |props| props.effect, layer.properties.effect);
        layer.transform_owner = property_owner(
            artifact,
            |props| props.transform,
            layer.properties.transform,
        );
    }
}

fn layers_equal(a: &[CompositorLayer], b: &[CompositorLayer]) -> bool {
    let rect_bits = |r: &LayoutRect| {
        [
            r.x.to_bits(),
            r.y.to_bits(),
            r.width.to_bits(),
            r.height.to_bits(),
        ]
    };
    a.len() == b.len()
        && a.iter().zip(b).all(|(a, b)| {
            a.id == b.id
                && rect_bits(&a.bounds) == rect_bits(&b.bounds)
                && a.properties == b.properties
                && a.chunk_ids == b.chunk_ids
                && a.client_indices == b.client_indices
                && a.sticky_owner == b.sticky_owner
                && a.effect_owner == b.effect_owner
                && a.transform_owner == b.transform_owner
                && a.scroll_host == b.scroll_host
        })
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
    // Every node and display item feeds this each frame: FxHash, not SipHash.
    let mut hasher = w3cos_std::fxhash::FxHasher::default();
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
        hasher.write_usize(index);
        hasher.write_usize(node.parent.map_or(usize::MAX, |parent| parent));
        hash_kind(&node.kind, &mut hasher);
        hash_paint_style(&node.style, &mut hasher);
        let z_order = artifact.z_order.get(index).copied().unwrap_or(0);
        let sticky = artifact.sticky_owner.get(index).copied().flatten();
        hasher.write_u64(
            u64::from(z_order as u32) | (sticky.map_or(u64::MAX, |owner| owner as u64) << 32),
        );
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
    hasher.write_u64(u64::from(rect.x.to_bits()) | (u64::from(rect.y.to_bits()) << 32));
    hasher.write_u64(u64::from(rect.width.to_bits()) | (u64::from(rect.height.to_bits()) << 32));
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
    // Runs for every node each frame: pack the scalar inputs into a few
    // words instead of one hasher write per field. The value is only ever
    // compared with this function's value for another frame.
    let color = |c: w3cos_std::Color| u64::from(u32::from_le_bytes([c.r, c.g, c.b, c.a]));
    let pair = |a: f32, b: f32| u64::from(a.to_bits()) | (u64::from(b.to_bits()) << 32);
    let overflow = |value: Option<w3cos_std::style::Overflow>| value.map_or(0, |v| v as u64 + 1);
    hasher.write_u64(color(style.background) | (color(style.color) << 32));
    hasher.write_u64(color(style.border_color) | (color(style.outline_color) << 32));
    hasher.write_u64(pair(style.font_size, style.border_radius));
    hasher.write_u64(pair(style.border_width, style.line_height));
    hasher.write_u64(pair(style.letter_spacing, style.word_spacing));
    hasher
        .write_u64(u64::from(style.outline_width.to_bits()) | (u64::from(style.font_weight) << 32));
    let flags = style.visibility as u64
        | (style.overflow as u64) << 4
        | overflow(style.overflow_x) << 8
        | overflow(style.overflow_y) << 12
        | (style.contain as u64) << 16
        | (style.text_align as u64) << 20
        | (style.white_space as u64) << 24
        | (style.text_decoration as u64) << 28
        | (style.outline_style as u64) << 32
        | u64::from(style.has_box_shadow()) << 40
        | u64::from(style.backdrop_blur.is_some()) << 41
        | u64::from(style.will_change.transform) << 42
        | u64::from(style.will_change.opacity) << 43
        | u64::from(style.will_change.filter) << 44
        | u64::from(style.will_change.scroll_position) << 45;
    hasher.write_u64(flags);
    hasher.write_usize(style.text_shadows.len());
    style.font_family.hash(hasher);
    style.filter.hash(hasher);
    style.background_image.hash(hasher);
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
    fn counting_order_matches_a_stable_sort_by_class() {
        let classes = [3usize, 0, 2, 3, 1, 0, 2, 2, 5, 0];
        let mut expected = (0..classes.len()).collect::<Vec<_>>();
        expected.sort_by_key(|&index| classes[index]);
        assert_eq!(
            crate::paint_artifact::stable_order_by_class(&classes),
            expected
        );
        assert!(crate::paint_artifact::stable_order_by_class(&[]).is_empty());
    }

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
            style: Style::default().into(),
            parent: None,
            sticky_counter_signal: None,
        };
        let mut scroll_style = Style::default();
        scroll_style.overflow = Overflow::Scroll;
        let scroll = PaintNode {
            kind: ComponentKind::Column,
            style: scroll_style.into(),
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
            style: child_style.into(),
            parent: Some(1),
            sticky_counter_signal: None,
        };
        PaintArtifact::build(
            [root, scroll, child],
            &[(rect(0.0), 0), (rect(0.0), 1), (rect(80.0), 2)],
            1,
        )
    }

    #[test]
    fn paint_damage_covers_old_and_new_paint_and_gives_up_on_filters() {
        let node = |style: Style, parent| PaintNode {
            kind: ComponentKind::Column,
            style: style.into(),
            parent,
            sticky_counter_signal: None,
        };
        let build = |y: f32, shadow: bool| {
            let mut moved = Style::default();
            if shadow {
                moved.filter = Some("blur(2px)".into());
            }
            PaintArtifact::build(
                [node(Style::default(), None), node(moved, Some(0))],
                &[(rect(0.0), 0), (rect(y), 1)],
                1,
            )
        };
        let (prev, cur) = (build(80.0, false), build(120.0, false));
        let diff = PaintDiff {
            dirty: vec![false, true],
            moved: vec![None, None],
        };
        let damage = paint_damage(&prev, &cur, &diff).expect("bounded");
        assert_eq!(damage.len(), 1);
        let (client, end, area) = damage[0];
        assert_eq!((client, end), (1, 2));
        // Both the old (y 80..160) and new (y 120..200) boxes, with margin.
        assert!(area.y <= 80.0 - 16.0 && area.y + area.height >= 200.0 + 16.0);
        assert!(area.x <= -16.0 && area.x + area.width >= 320.0 + 16.0);
        let shadowed = build(120.0, true);
        assert!(paint_damage(&prev, &shadowed, &diff).is_none());
    }

    #[test]
    fn consecutive_clients_moved_by_one_offset_share_one_damage_rect() {
        let node = |parent| PaintNode {
            kind: ComponentKind::Column,
            style: Style::default().into(),
            parent,
            sticky_counter_signal: None,
        };
        let build = |dy: f32| {
            PaintArtifact::build(
                [node(None), node(Some(0)), node(Some(0))],
                &[(rect(0.0), 0), (rect(80.0 + dy), 1), (rect(160.0 + dy), 2)],
                1,
            )
        };
        let (prev, cur) = (build(0.0), build(40.0));
        let diff = PaintDiff {
            dirty: vec![false, false, false],
            moved: vec![None, Some((0.0, 40.0)), Some((0.0, 40.0))],
        };
        let damage = paint_damage(&prev, &cur, &diff).expect("bounded");
        assert_eq!(damage.len(), 1);
        let (first, end, area) = damage[0];
        assert_eq!((first, end), (1, 3));
        // Old (80..240) and new (120..280) paint of both, with margin.
        assert!(area.y <= 80.0 - 16.0 && area.y + area.height >= 280.0 + 16.0);
        // A different offset starts its own rect.
        let diff = PaintDiff {
            dirty: vec![false, false, false],
            moved: vec![None, Some((0.0, 40.0)), Some((0.0, 41.0))],
        };
        assert_eq!(paint_damage(&prev, &cur, &diff).expect("bounded").len(), 2);
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
                    style: Style::default().into(),
                    parent: None,
                    sticky_counter_signal: None,
                },
                PaintNode {
                    kind: ComponentKind::Column,
                    style: positioned_style.into(),
                    parent: Some(0),
                    sticky_counter_signal: None,
                },
                PaintNode {
                    kind: ComponentKind::Column,
                    style: float_style.into(),
                    parent: Some(0),
                    sticky_counter_signal: None,
                },
                PaintNode {
                    kind: ComponentKind::Text {
                        content: "float".into(),
                    },
                    style: Style::default().into(),
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
    fn a_kept_layer_partition_refreshes_bounds_and_matches_a_full_build() {
        super::LAYER_REUSE_OVERRIDE.with(|mode| mode.set(Some(2)));
        let mismatches = || super::LAYER_REUSE_MISMATCHES.with(std::cell::Cell::get);
        let hits = || super::LAYER_REUSE_HITS.with(std::cell::Cell::get);
        let (mismatches_before, hits_before) = (mismatches(), hits());
        let mut tree = RetainedLayerTree::default();
        let scrolls = HashMap::new();
        let overrides = CompositorOverrides::default();
        let first = scroll_tree();
        tree.sync(&first, &scrolls, &overrides);
        assert_eq!(hits(), hits_before, "nothing to keep on the first frame");
        let mut moved = scroll_tree();
        for chunk in &mut moved.chunks {
            chunk.bounds.y += 7.0;
        }
        tree.sync(&moved, &scrolls, &overrides);
        assert_eq!(hits(), hits_before + 1);
        assert!(layers_equal(&tree.layers, &build_layers(&moved)));
        // A chunk that changes its clients regroups in full.
        let mut regrouped = scroll_tree();
        let last = regrouped.display_items.len() - 1;
        regrouped.display_items[last].client_index += 1;
        tree.sync(&regrouped, &scrolls, &overrides);
        assert_eq!(hits(), hits_before + 1);
        assert!(layers_equal(&tree.layers, &build_layers(&regrouped)));
        assert_eq!(mismatches(), mismatches_before);
        super::LAYER_REUSE_OVERRIDE.with(|mode| mode.set(None));
    }

    #[test]
    fn partition_keys_keep_layers_only_when_grouping_inputs_match() {
        super::LAYER_REUSE_OVERRIDE.with(|mode| mode.set(Some(2)));
        super::LAYER_PARTITION_KEYS_OVERRIDE.with(|mode| mode.set(Some(2)));
        let mismatches = || super::LAYER_REUSE_MISMATCHES.with(std::cell::Cell::get);
        let key_mismatches = super::layer_partition_keys_mismatches;
        let hits = || super::LAYER_REUSE_HITS.with(std::cell::Cell::get);
        let (mismatches_before, key_mismatches_before, hits_before) =
            (mismatches(), key_mismatches(), hits());
        let mut tree = RetainedLayerTree::default();
        let scrolls = HashMap::new();
        let overrides = CompositorOverrides::default();
        tree.sync(&scroll_tree(), &scrolls, &overrides);
        assert_eq!(hits(), hits_before, "nothing to keep on the first frame");
        let mut moved = scroll_tree();
        for chunk in &mut moved.chunks {
            chunk.bounds.y += 3.0;
        }
        tree.sync(&moved, &scrolls, &overrides);
        assert_eq!(hits(), hits_before + 1, "only geometry moved");
        assert!(layers_equal(&tree.layers, &build_layers(&moved)));
        let mut regrouped = scroll_tree();
        let last = regrouped.display_items.len() - 1;
        regrouped.display_items[last].client_index += 1;
        tree.sync(&regrouped, &scrolls, &overrides);
        assert_eq!(hits(), hits_before + 1, "a changed client regroups");
        assert!(layers_equal(&tree.layers, &build_layers(&regrouped)));
        tree.sync(&regrouped, &scrolls, &overrides);
        assert_eq!(hits(), hits_before + 2, "the regrouped inputs are kept");
        assert_eq!(mismatches(), mismatches_before);
        assert_eq!(key_mismatches(), key_mismatches_before);
        super::LAYER_PARTITION_KEYS_OVERRIDE.with(|mode| mode.set(None));
        super::LAYER_REUSE_OVERRIDE.with(|mode| mode.set(None));
    }

    #[test]
    fn cull_band_client_changes_re_record_only_the_changed_layer() {
        let tree_with_root_row = |band: Option<(f32, f32)>| {
            let mut nodes: Vec<PaintNode> = scroll_tree().nodes;
            for _ in 0..2 {
                nodes.push(PaintNode {
                    kind: ComponentKind::Column,
                    style: Style::default().into(),
                    parent: Some(0),
                    sticky_counter_signal: None,
                });
            }
            crate::paint_artifact::PAINT_CULL_BAND.with(|cell| cell.set(band));
            let artifact = PaintArtifact::build_reusing(
                nodes,
                &[
                    (rect(0.0), 0),
                    (rect(0.0), 1),
                    (rect(80.0), 2),
                    (rect(200.0), 3),
                    (rect(10000.0), 4),
                ],
                1,
                None,
                None,
                None,
            );
            crate::paint_artifact::PAINT_CULL_BAND.with(|cell| cell.set(None));
            artifact
        };
        crate::paint_artifact::PAINT_CULL_RECT_OVERRIDE.with(|mode| mode.set(Some(1)));
        let mut tree = RetainedLayerTree::default();
        tree.set_track_partial(true);
        let scrolls = HashMap::new();
        let overrides = CompositorOverrides::default();
        let full = tree_with_root_row(None);
        tree.sync(&full, &scrolls, &overrides);
        tree.note_rebuild();
        // The cull band drops the far root row's item; its node stays and
        // the layer count holds.
        let culled = tree_with_root_row(Some((-4000.0, 4720.0)));
        assert!(culled.display_items.len() < full.display_items.len());
        tree.sync(&culled, &scrolls, &overrides);
        crate::paint_artifact::PAINT_CULL_RECT_OVERRIDE.with(|mode| mode.set(None));
        assert!(layers_equal(&tree.layers, &build_layers(&culled)));
        let partial = tree.partial_layers().expect("compared layer by layer");
        let before = build_layers(&full);
        let changed: Vec<bool> = before
            .iter()
            .zip(&tree.layers)
            .map(|(old, new)| old.client_indices != new.client_indices)
            .collect();
        assert_eq!(changed.iter().filter(|c| **c).count(), 1, "{changed:?}");
        for (same, changed) in partial.iter().zip(&changed) {
            assert_eq!(*same, !changed, "partial {partial:?}");
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
    fn a_known_change_rebuilds_without_the_fingerprint_and_the_next_frame_rebuilds_once() {
        let artifact = scroll_tree();
        let mut tree = RetainedLayerTree::default();
        let scrolls = HashMap::new();
        let overrides = CompositorOverrides::default();
        let first = tree.sync(&artifact, &scrolls, &overrides);
        note(&mut tree, first);
        assert_eq!(
            tree.sync(&artifact, &scrolls, &overrides),
            LayerPaintAction::Replay
        );
        // The artifact is unchanged on purpose: that is what `=verify` notes.
        super::LAYER_SYNC_FAST_QUIET.with(|quiet| quiet.set(true));
        let known = tree.sync_known_changed(&artifact, &scrolls, &overrides, true);
        super::LAYER_SYNC_FAST_QUIET.with(|quiet| quiet.set(false));
        assert_eq!(known, LayerPaintAction::Rebuild);
        note(&mut tree, known);
        // The skipped fingerprint cannot vouch for the recordings.
        let after = tree.sync(&artifact, &scrolls, &overrides);
        assert_eq!(after, LayerPaintAction::Rebuild);
        note(&mut tree, after);
        assert_eq!(
            tree.sync(&artifact, &scrolls, &overrides),
            LayerPaintAction::Replay
        );
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
                    style: Style::default().into(),
                    parent: None,
                    sticky_counter_signal: None,
                },
                PaintNode {
                    kind: ComponentKind::Column,
                    style: dirty_style.into(),
                    parent: Some(0),
                    sticky_counter_signal: None,
                },
                PaintNode {
                    kind: ComponentKind::Text {
                        content: "dirty".into(),
                    },
                    style: child_style.into(),
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
                Some((idx, rect, &node.kind, node.style.as_style()))
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
                    style: Style::default().into(),
                    parent: None,
                    sticky_counter_signal: None,
                },
                PaintNode {
                    kind: ComponentKind::Box,
                    style: child_style.into(),
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
