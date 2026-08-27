//! Retained SVG parsing and raster cache.
//!
//! SVG content is normalized by usvg and rasterized by resvg. The cache key
//! deliberately excludes CSS transform and opacity: those are compositor
//! properties and must not force SVG parsing/rasterization on every frame.
//!
//! Paint-only mutations of an unchanged document topology (typical of SVG
//! presentation animation) reuse 32px tiles whose dirty bounding boxes do
//! not overlap. Topology or geometry changes still take a full tiled raster.
//! Direct GPU vector tessellation is not implemented.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::image_loader::DecodedImage;
use w3cos_std::SvgEventTarget;

const SVG_CACHE_BUDGET_BYTES: usize = 64 * 1024 * 1024;
const SVG_PARSE_CACHE_ENTRIES: usize = 128;
const SVG_RASTER_TILE_SIZE: u32 = 32;
const SVG_DIRTY_PAD_PX: f32 = 8.0;
const SVG_SESSION_LIMIT: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct RasterKey {
    revision: u64,
    width: u32,
    height: u32,
}

struct ParsedEntry {
    revision: u64,
    tree: Option<Arc<resvg::usvg::Tree>>,
    hit_tree: Option<Arc<resvg::usvg::Tree>>,
    use_ids: Arc<HashSet<String>>,
    last_used: u64,
}

#[derive(Clone)]
struct RasterEntry {
    image: Option<DecodedImage>,
    bytes: usize,
    last_used: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct HitMaskKey {
    revision: u64,
    target_index: u32,
    width: u32,
    height: u32,
    mode: HitMaskMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum HitMaskMode {
    Painted,
    Fill,
    Stroke,
    FillAndStroke,
}

#[derive(Clone, Copy)]
enum PointerHitMode {
    None,
    BoundingBox,
    Mask {
        mode: HitMaskMode,
        requires_visible: bool,
    },
}

struct HitMaskEntry {
    left: i32,
    top: i32,
    width: u32,
    height: u32,
    alpha: Arc<[u8]>,
    last_used: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct SessionKey {
    shape: u64,
    width: u32,
    height: u32,
}

#[derive(Clone)]
struct NodeRecord {
    key: String,
    paint: u64,
    geometry: u64,
    bounds: [f32; 4],
}

#[derive(Clone)]
struct TilePixels {
    width: u32,
    height: u32,
    data: Arc<[u8]>,
}

struct TiledSession {
    records: Vec<NodeRecord>,
    tiles: HashMap<(u32, u32), TilePixels>,
    last_used: u64,
}

#[derive(Default)]
struct SvgCache {
    parsed: HashMap<String, ParsedEntry>,
    rasters: HashMap<RasterKey, RasterEntry>,
    hit_masks: HashMap<HitMaskKey, HitMaskEntry>,
    sessions: HashMap<SessionKey, TiledSession>,
    clock: u64,
    next_revision: u64,
    parse_hits: u64,
    parse_misses: u64,
    hits: u64,
    misses: u64,
    mask_hits: u64,
    mask_misses: u64,
    evictions: u64,
    tile_hits: u64,
    tile_misses: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SvgCacheStats {
    pub entries: usize,
    pub resident_bytes: usize,
    pub hit_masks: usize,
    pub mask_hits: u64,
    pub mask_misses: u64,
    pub parsed_entries: usize,
    pub parse_hits: u64,
    pub parse_misses: u64,
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub tile_hits: u64,
    pub tile_misses: u64,
}

thread_local! {
    static CACHE: RefCell<SvgCache> = RefCell::new(SvgCache::default());
}

pub fn get_or_render(source: &str, width: u32, height: u32) -> Option<DecodedImage> {
    let width = width.max(1);
    let height = height.max(1);
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.clock = cache.clock.wrapping_add(1);
        let clock = cache.clock;
        let (revision, tree, _, _) = cache.get_or_parse(source, clock)?;
        let key = RasterKey {
            revision,
            width,
            height,
        };
        if let Some(entry) = cache.rasters.get_mut(&key) {
            entry.last_used = clock;
            let image = entry.image.clone();
            cache.hits = cache.hits.wrapping_add(1);
            return image;
        }

        cache.misses = cache.misses.wrapping_add(1);
        let image = cache.rasterize_document(&tree, width, height, clock);
        let bytes = image
            .as_ref()
            .map(|image| image.data.len())
            .unwrap_or_default();
        cache.rasters.insert(
            key,
            RasterEntry {
                image: image.clone(),
                bytes,
                last_used: clock,
            },
        );
        cache.evict_to_budget();
        image
    })
}

/// Returns the deepest SVG DOM target at raster-space coordinates.
pub fn hit_test(
    source: &str,
    width: u32,
    height: u32,
    event_targets: &[SvgEventTarget],
    x: f32,
    y: f32,
) -> Option<Vec<u64>> {
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.clock = cache.clock.wrapping_add(1);
        let clock = cache.clock;
        let (revision, display_tree, hit_tree, use_ids) = cache.get_or_parse(source, clock)?;
        let size = hit_tree.size();
        let scale_x = width.max(1) as f32 / size.width();
        let scale_y = height.max(1) as f32 / size.height();
        let mut render_nodes = Vec::new();
        collect_render_nodes(hit_tree.root(), &[], &mut render_nodes);
        event_targets
            .iter()
            .enumerate()
            .rev()
            .find_map(|(target_index, target)| {
                let pointer_mode = pointer_hit_mode(&target.pointer_events);
                if matches!(pointer_mode, PointerHitMode::None) {
                    return None;
                }
                let use_display_tree = matches!(
                    pointer_mode,
                    PointerHitMode::Mask {
                        mode: HitMaskMode::Painted,
                        ..
                    }
                ) && use_ids.contains(&target.svg_id);
                let resolved = if use_display_tree {
                    find_node_by_id(display_tree.root(), &target.svg_id, &[])
                } else {
                    resolve_target_node(&hit_tree, &render_nodes, target)
                }?;
                let node = resolved.node;
                if matches!(
                    pointer_mode,
                    PointerHitMode::Mask {
                        requires_visible: true,
                        ..
                    }
                ) && !node_is_visible(node)
                {
                    return None;
                }
                let bounds = node_hit_bounds(node, &pointer_mode);
                let left = bounds.x() * scale_x;
                let top = bounds.y() * scale_y;
                let right = left + bounds.width() * scale_x;
                let bottom = top + bounds.height() * scale_y;
                if x < left || x > right || y < top || y > bottom {
                    return None;
                }
                if matches!(pointer_mode, PointerHitMode::BoundingBox) {
                    return Some(target.host_chain.clone());
                }
                let PointerHitMode::Mask { mode, .. } = pointer_mode else {
                    return None;
                };
                cache
                    .hit_mask_contains(
                        revision,
                        target_index as u32,
                        node,
                        &resolved.clips,
                        width.max(1),
                        height.max(1),
                        mode,
                        scale_x,
                        scale_y,
                        x,
                        y,
                        clock,
                    )
                    .then(|| target.host_chain.clone())
            })
    })
}

pub fn clear_cache() {
    CACHE.with(|cache| *cache.borrow_mut() = SvgCache::default());
}

pub fn cache_stats() -> SvgCacheStats {
    CACHE.with(|cache| {
        let cache = cache.borrow();
        SvgCacheStats {
            entries: cache.rasters.len(),
            resident_bytes: cache.resident_bytes(),
            hit_masks: cache.hit_masks.len(),
            mask_hits: cache.mask_hits,
            mask_misses: cache.mask_misses,
            parsed_entries: cache.parsed.len(),
            parse_hits: cache.parse_hits,
            parse_misses: cache.parse_misses,
            hits: cache.hits,
            misses: cache.misses,
            evictions: cache.evictions,
            tile_hits: cache.tile_hits,
            tile_misses: cache.tile_misses,
        }
    })
}

impl SvgCache {
    #[allow(clippy::too_many_arguments)]
    fn hit_mask_contains(
        &mut self,
        revision: u64,
        target_index: u32,
        node: &resvg::usvg::Node,
        clips: &[ClipContext<'_>],
        width: u32,
        height: u32,
        mode: HitMaskMode,
        scale_x: f32,
        scale_y: f32,
        x: f32,
        y: f32,
        clock: u64,
    ) -> bool {
        let key = HitMaskKey {
            revision,
            target_index,
            width,
            height,
            mode,
        };
        if let Some(mask) = self.hit_masks.get_mut(&key) {
            mask.last_used = clock;
            self.mask_hits = self.mask_hits.wrapping_add(1);
            return mask.contains(x, y);
        }
        self.mask_misses = self.mask_misses.wrapping_add(1);
        let Some(mask) = rasterize_hit_mask(node, clips, mode, scale_x, scale_y, clock) else {
            return false;
        };
        let contains = mask.contains(x, y);
        self.hit_masks.insert(key, mask);
        self.evict_to_budget();
        contains
    }

    fn get_or_parse(
        &mut self,
        source: &str,
        clock: u64,
    ) -> Option<(
        u64,
        Arc<resvg::usvg::Tree>,
        Arc<resvg::usvg::Tree>,
        Arc<HashSet<String>>,
    )> {
        if let Some(entry) = self.parsed.get_mut(source) {
            entry.last_used = clock;
            let revision = entry.revision;
            let tree = entry.tree.clone();
            let hit_tree = entry.hit_tree.clone();
            let use_ids = entry.use_ids.clone();
            self.parse_hits = self.parse_hits.wrapping_add(1);
            return tree
                .zip(hit_tree)
                .map(|(tree, hit_tree)| (revision, tree, hit_tree, use_ids));
        }

        self.parse_misses = self.parse_misses.wrapping_add(1);
        self.next_revision = self.next_revision.wrapping_add(1).max(1);
        let revision = self.next_revision;
        let tree = parse(source).map(Arc::new);
        let (use_ids, geometry_references) =
            collect_use_metadata(source).unwrap_or_else(|| (HashSet::new(), HashSet::new()));
        let use_ids = Arc::new(use_ids);
        let hit_tree = tree.as_ref().and_then(|tree| {
            sanitize_svg_for_hit_testing(source, &geometry_references)
                .and_then(|source| parse_quiet(&source))
                .map(Arc::new)
                .or_else(|| Some(tree.clone()))
        });
        self.parsed.insert(
            source.to_string(),
            ParsedEntry {
                revision,
                tree: tree.clone(),
                hit_tree: hit_tree.clone(),
                use_ids: use_ids.clone(),
                last_used: clock,
            },
        );
        self.evict_parsed_entries();
        tree.zip(hit_tree)
            .map(|(tree, hit_tree)| (revision, tree, hit_tree, use_ids))
    }

    fn rasterize_document(
        &mut self,
        tree: &resvg::usvg::Tree,
        width: u32,
        height: u32,
        clock: u64,
    ) -> Option<DecodedImage> {
        let records = collect_node_records(tree.root(), "root");
        let shape = shape_identity(tree, &records);
        let session_key = SessionKey {
            shape,
            width,
            height,
        };
        let previous = self.sessions.remove(&session_key);
        let dirty_tiles = previous
            .as_ref()
            .map(|session| dirty_tile_origins(&session.records, &records, width, height, tree))
            .unwrap_or_else(|| all_tile_origins(width, height));
        let mut tiles = previous.map(|session| session.tiles).unwrap_or_default();
        let mut copied = 0_u64;
        tiles.retain(|&origin, _| {
            if dirty_tiles.contains(&origin) {
                false
            } else if origin.0 < width && origin.1 < height {
                copied = copied.saturating_add(1);
                true
            } else {
                false
            }
        });
        let mut rerastered = 0_u64;
        for origin in &dirty_tiles {
            if let Some(tile) = rasterize_tile(tree, width, height, origin.0, origin.1) {
                tiles.insert(*origin, tile);
                rerastered = rerastered.saturating_add(1);
            }
        }
        self.tile_hits = self.tile_hits.wrapping_add(copied);
        self.tile_misses = self.tile_misses.wrapping_add(rerastered);
        let image = compose_tiles(&tiles, width, height)?;
        self.sessions.insert(
            session_key,
            TiledSession {
                records,
                tiles,
                last_used: clock,
            },
        );
        self.evict_sessions();
        Some(image)
    }

    fn evict_sessions(&mut self) {
        while self.sessions.len() > SVG_SESSION_LIMIT {
            let Some(oldest) = self
                .sessions
                .iter()
                .min_by_key(|(_, session)| session.last_used)
                .map(|(key, _)| *key)
            else {
                break;
            };
            self.sessions.remove(&oldest);
        }
    }

    fn evict_parsed_entries(&mut self) {
        while self.parsed.len() > SVG_PARSE_CACHE_ENTRIES {
            let Some(oldest_source) = self
                .parsed
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(source, _)| source.clone())
            else {
                break;
            };
            if let Some(entry) = self.parsed.remove(&oldest_source) {
                self.rasters.retain(|key, _| key.revision != entry.revision);
                self.hit_masks
                    .retain(|key, _| key.revision != entry.revision);
            }
        }
    }

    fn resident_bytes(&self) -> usize {
        self.rasters
            .values()
            .map(|entry| entry.bytes)
            .sum::<usize>()
            + self
                .hit_masks
                .values()
                .map(|entry| entry.alpha.len())
                .sum::<usize>()
    }

    fn evict_to_budget(&mut self) {
        let mut resident = self.resident_bytes();
        while resident > SVG_CACHE_BUDGET_BYTES && self.rasters.len() + self.hit_masks.len() > 1 {
            let oldest_raster = self
                .rasters
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, entry)| (*key, entry.last_used));
            let oldest_mask = self
                .hit_masks
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, entry)| (*key, entry.last_used));
            if oldest_mask.is_some_and(|(_, used)| {
                oldest_raster.is_none_or(|(_, raster_used)| used <= raster_used)
            }) {
                let (key, _) = oldest_mask.unwrap();
                if let Some(entry) = self.hit_masks.remove(&key) {
                    resident = resident.saturating_sub(entry.alpha.len());
                    self.evictions = self.evictions.wrapping_add(1);
                }
            } else if let Some((key, _)) = oldest_raster
                && let Some(entry) = self.rasters.remove(&key)
            {
                resident = resident.saturating_sub(entry.bytes);
                self.evictions = self.evictions.wrapping_add(1);
            } else {
                break;
            }
        }
    }
}

impl HitMaskEntry {
    fn contains(&self, x: f32, y: f32) -> bool {
        let local_x = x.floor() as i32 - self.left;
        let local_y = y.floor() as i32 - self.top;
        if local_x < 0
            || local_y < 0
            || local_x >= self.width as i32
            || local_y >= self.height as i32
        {
            return false;
        }
        self.alpha[local_y as usize * self.width as usize + local_x as usize] != 0
    }
}

#[derive(Clone, Copy)]
struct ClipContext<'a> {
    clip: &'a resvg::usvg::ClipPath,
    group_transform: resvg::tiny_skia::Transform,
}

struct ResolvedNode<'a> {
    node: &'a resvg::usvg::Node,
    clips: Vec<ClipContext<'a>>,
}

fn resolve_target_node<'a>(
    tree: &'a resvg::usvg::Tree,
    render_nodes: &[ResolvedNode<'a>],
    target: &SvgEventTarget,
) -> Option<ResolvedNode<'a>> {
    if !target.svg_id.is_empty() {
        return find_node_by_id(tree.root(), &target.svg_id, &[]);
    }
    let index = target.render_index? as usize;
    render_nodes.get(index).map(|resolved| ResolvedNode {
        node: resolved.node,
        clips: resolved.clips.clone(),
    })
}

fn group_clips<'a>(
    group: &'a resvg::usvg::Group,
    inherited: &[ClipContext<'a>],
) -> Vec<ClipContext<'a>> {
    let mut clips = inherited.to_vec();
    if let Some(clip) = group.clip_path() {
        clips.push(ClipContext {
            clip,
            group_transform: group.abs_transform(),
        });
    }
    clips
}

fn find_node_by_id<'a>(
    group: &'a resvg::usvg::Group,
    id: &str,
    inherited: &[ClipContext<'a>],
) -> Option<ResolvedNode<'a>> {
    for node in group.children() {
        match node {
            resvg::usvg::Node::Group(child) => {
                let clips = group_clips(child, inherited);
                if node.id() == id {
                    return Some(ResolvedNode { node, clips });
                }
                if let Some(resolved) = find_node_by_id(child, id, &clips) {
                    return Some(resolved);
                }
            }
            _ if node.id() == id => {
                return Some(ResolvedNode {
                    node,
                    clips: inherited.to_vec(),
                });
            }
            _ => {}
        }
    }
    None
}

fn collect_render_nodes<'a>(
    group: &'a resvg::usvg::Group,
    inherited: &[ClipContext<'a>],
    nodes: &mut Vec<ResolvedNode<'a>>,
) {
    for node in group.children() {
        match node {
            resvg::usvg::Node::Group(group) => {
                let clips = group_clips(group, inherited);
                collect_render_nodes(group, &clips, nodes);
            }
            _ => nodes.push(ResolvedNode {
                node,
                clips: inherited.to_vec(),
            }),
        }
    }
}

fn pointer_hit_mode(value: &str) -> PointerHitMode {
    match value.trim().to_ascii_lowercase().as_str() {
        "none" => PointerHitMode::None,
        "bounding-box" => PointerHitMode::BoundingBox,
        "fill" => PointerHitMode::Mask {
            mode: HitMaskMode::Fill,
            requires_visible: false,
        },
        "visiblefill" | "visible-fill" => PointerHitMode::Mask {
            mode: HitMaskMode::Fill,
            requires_visible: true,
        },
        "stroke" => PointerHitMode::Mask {
            mode: HitMaskMode::Stroke,
            requires_visible: false,
        },
        "visiblestroke" | "visible-stroke" => PointerHitMode::Mask {
            mode: HitMaskMode::Stroke,
            requires_visible: true,
        },
        "all" => PointerHitMode::Mask {
            mode: HitMaskMode::FillAndStroke,
            requires_visible: false,
        },
        "visible" => PointerHitMode::Mask {
            mode: HitMaskMode::FillAndStroke,
            requires_visible: true,
        },
        "painted" => PointerHitMode::Mask {
            mode: HitMaskMode::Painted,
            requires_visible: false,
        },
        _ => PointerHitMode::Mask {
            mode: HitMaskMode::Painted,
            requires_visible: true,
        },
    }
}

fn node_is_visible(node: &resvg::usvg::Node) -> bool {
    match node {
        resvg::usvg::Node::Group(group) => group.children().iter().any(node_is_visible),
        resvg::usvg::Node::Path(path) => path.is_visible(),
        resvg::usvg::Node::Image(image) => image.is_visible(),
        resvg::usvg::Node::Text(text) => text
            .chunks()
            .iter()
            .flat_map(|chunk| chunk.spans())
            .any(|span| span.is_visible()),
    }
}

fn is_image_geometry_node(node: &resvg::usvg::Node) -> bool {
    match node {
        resvg::usvg::Node::Image(_) => true,
        resvg::usvg::Node::Group(group) if group.children().len() == 1 => {
            is_image_geometry_node(&group.children()[0])
        }
        _ => false,
    }
}

fn node_hit_bounds(node: &resvg::usvg::Node, mode: &PointerHitMode) -> resvg::usvg::Rect {
    match (node, mode) {
        (
            resvg::usvg::Node::Path(path),
            PointerHitMode::Mask {
                mode: HitMaskMode::Fill,
                ..
            },
        ) => path.abs_bounding_box(),
        (resvg::usvg::Node::Text(text), _) => text.abs_bounding_box(),
        (resvg::usvg::Node::Image(image), _) => image.abs_bounding_box(),
        (node, _) if is_image_geometry_node(node) => {
            node.abs_layer_bounding_box().unwrap().to_rect()
        }
        _ => node.abs_stroke_bounding_box(),
    }
}

fn draw_clip_group(
    group: &resvg::usvg::Group,
    transform: resvg::tiny_skia::Transform,
    pixmap: &mut resvg::tiny_skia::Pixmap,
) {
    for node in group.children() {
        match node {
            resvg::usvg::Node::Path(path) if path.is_visible() => {
                let Some(fill) = path.fill() else {
                    continue;
                };
                let fill_rule = match fill.rule() {
                    resvg::usvg::FillRule::NonZero => resvg::tiny_skia::FillRule::Winding,
                    resvg::usvg::FillRule::EvenOdd => resvg::tiny_skia::FillRule::EvenOdd,
                };
                let paint = resvg::tiny_skia::Paint::default();
                pixmap
                    .as_mut()
                    .fill_path(path.data(), &paint, fill_rule, transform, None);
            }
            resvg::usvg::Node::Text(text) => {
                draw_clip_group(text.flattened(), transform, pixmap);
            }
            resvg::usvg::Node::Group(group) => {
                let transform = transform.pre_concat(group.transform());
                if let Some(clip) = group.clip_path() {
                    let Some(mut layer) =
                        resvg::tiny_skia::Pixmap::new(pixmap.width(), pixmap.height())
                    else {
                        continue;
                    };
                    draw_clip_group(group, transform, &mut layer);
                    apply_clip_path(&mut layer, clip, transform);
                    pixmap.draw_pixmap(
                        0,
                        0,
                        layer.as_ref(),
                        &resvg::tiny_skia::PixmapPaint::default(),
                        resvg::tiny_skia::Transform::identity(),
                        None,
                    );
                } else {
                    draw_clip_group(group, transform, pixmap);
                }
            }
            _ => {}
        }
    }
}

fn apply_clip_path(
    pixmap: &mut resvg::tiny_skia::Pixmap,
    clip: &resvg::usvg::ClipPath,
    transform: resvg::tiny_skia::Transform,
) {
    let Some(mut clip_pixmap) = resvg::tiny_skia::Pixmap::new(pixmap.width(), pixmap.height())
    else {
        return;
    };
    draw_clip_group(
        clip.root(),
        transform.pre_concat(clip.transform()),
        &mut clip_pixmap,
    );
    if let Some(parent_clip) = clip.clip_path() {
        apply_clip_path(&mut clip_pixmap, parent_clip, transform);
    }
    let mask = resvg::tiny_skia::Mask::from_pixmap(
        clip_pixmap.as_ref(),
        resvg::tiny_skia::MaskType::Alpha,
    );
    pixmap.apply_mask(&mask);
}

fn apply_clip_contexts(
    pixmap: &mut resvg::tiny_skia::Pixmap,
    clips: &[ClipContext<'_>],
    left: i32,
    top: i32,
    scale_x: f32,
    scale_y: f32,
) {
    let crop_transform = resvg::tiny_skia::Transform::from_translate(-(left as f32), -(top as f32))
        .pre_concat(resvg::tiny_skia::Transform::from_scale(scale_x, scale_y));
    for context in clips {
        apply_clip_path(
            pixmap,
            context.clip,
            crop_transform.pre_concat(context.group_transform),
        );
    }
}

fn rasterize_hit_mask(
    node: &resvg::usvg::Node,
    clips: &[ClipContext<'_>],
    mode: HitMaskMode,
    scale_x: f32,
    scale_y: f32,
    clock: u64,
) -> Option<HitMaskEntry> {
    let bounds = match (node, mode) {
        (resvg::usvg::Node::Path(path), HitMaskMode::Fill) => path.abs_bounding_box(),
        (resvg::usvg::Node::Path(path), HitMaskMode::Stroke | HitMaskMode::FillAndStroke) => {
            path.abs_stroke_bounding_box()
        }
        (resvg::usvg::Node::Text(text), _) => text.abs_bounding_box(),
        (resvg::usvg::Node::Image(image), mode) if mode != HitMaskMode::Painted => {
            image.abs_bounding_box()
        }
        (node, mode) if is_image_geometry_node(node) && mode != HitMaskMode::Painted => {
            node.abs_layer_bounding_box()?.to_rect()
        }
        _ => node.abs_layer_bounding_box()?.to_rect(),
    };
    let left = (bounds.x() * scale_x).floor() as i32 - 1;
    let top = (bounds.y() * scale_y).floor() as i32 - 1;
    let width = ((bounds.right() * scale_x).ceil() as i32 + 1 - left).max(1) as u32;
    let height = ((bounds.bottom() * scale_y).ceil() as i32 + 1 - top).max(1) as u32;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)?;
    if let resvg::usvg::Node::Path(path) = node {
        let transform = resvg::tiny_skia::Transform::from_translate(-(left as f32), -(top as f32))
            .pre_concat(
                resvg::tiny_skia::Transform::from_scale(scale_x, scale_y)
                    .pre_concat(path.abs_transform()),
            );
        let mut paint = resvg::tiny_skia::Paint::default();
        paint.anti_alias = true;
        let draw_fill = matches!(mode, HitMaskMode::Fill | HitMaskMode::FillAndStroke)
            || (mode == HitMaskMode::Painted && path.fill().is_some());
        let draw_stroke = matches!(mode, HitMaskMode::Stroke | HitMaskMode::FillAndStroke)
            || (mode == HitMaskMode::Painted && path.stroke().is_some());
        if draw_fill {
            let fill_rule = path
                .fill()
                .map(|fill| match fill.rule() {
                    resvg::usvg::FillRule::NonZero => resvg::tiny_skia::FillRule::Winding,
                    resvg::usvg::FillRule::EvenOdd => resvg::tiny_skia::FillRule::EvenOdd,
                })
                .unwrap_or(resvg::tiny_skia::FillRule::Winding);
            pixmap.fill_path(path.data(), &paint, fill_rule, transform, None);
        }
        if draw_stroke && let Some(stroke) = path.stroke() {
            pixmap.stroke_path(path.data(), &paint, &stroke.to_tiny_skia(), transform, None);
        }
    } else if let resvg::usvg::Node::Text(text) = node {
        let is_painted = text
            .chunks()
            .iter()
            .flat_map(|chunk| chunk.spans())
            .any(|span| span.fill().is_some() || span.stroke().is_some());
        if mode != HitMaskMode::Painted || is_painted {
            pixmap.fill(resvg::tiny_skia::Color::BLACK);
        }
    } else if is_image_geometry_node(node) && mode != HitMaskMode::Painted {
        pixmap.fill(resvg::tiny_skia::Color::BLACK);
    } else {
        let transform = resvg::tiny_skia::Transform::from_scale(scale_x, scale_y);
        resvg::render_node(node, transform, &mut pixmap.as_mut())?;
    }
    apply_clip_contexts(&mut pixmap, clips, left, top, scale_x, scale_y);
    let alpha = pixmap
        .data()
        .chunks_exact(4)
        .map(|pixel| pixel[3])
        .collect::<Vec<_>>();
    Some(HitMaskEntry {
        left,
        top,
        width,
        height,
        alpha: alpha.into(),
        last_used: clock,
    })
}

fn parse(source: &str) -> Option<resvg::usvg::Tree> {
    parse_impl(source, true)
}

fn parse_quiet(source: &str) -> Option<resvg::usvg::Tree> {
    parse_impl(source, false)
}

fn parse_impl(source: &str, warn: bool) -> Option<resvg::usvg::Tree> {
    let mut options = resvg::usvg::Options::default();
    #[cfg(not(test))]
    {
        options.font_family = "sans-serif".to_string();
        options
            .fontdb_mut()
            .load_font_data(crate::font_face::host_ui_font().data.as_ref().clone());
    }
    #[cfg(test)]
    {
        options.font_family = "Geneva".to_string();
    }
    #[cfg(test)]
    options
        .fontdb_mut()
        .load_font_data(include_bytes!("../assets/Inter-Regular.ttf").to_vec());
    #[cfg(test)]
    options
        .fontdb_mut()
        .load_font_data(include_bytes!("../assets/CJK-Subset.ttf").to_vec());

    match resvg::usvg::Tree::from_str(source, &options) {
        Ok(tree) => Some(tree),
        Err(error) => {
            if warn {
                eprintln!("W3COS warning: SVG subtree could not be parsed: {error}");
            }
            None
        }
    }
}

fn collect_use_metadata(source: &str) -> Option<(HashSet<String>, HashSet<String>)> {
    use quick_xml::Reader;
    use quick_xml::events::{BytesStart, Event};

    fn style_property(value: &str, name: &str) -> Option<String> {
        value
            .split(';')
            .filter_map(|declaration| declaration.split_once(':'))
            .filter(|(property, _)| property.trim().eq_ignore_ascii_case(name))
            .map(|(_, value)| value.trim().to_string())
            .next_back()
    }

    fn use_metadata(
        event: &BytesStart<'_>,
        decoder: quick_xml::encoding::Decoder,
        inherited_pointer_events: &str,
    ) -> Option<(String, bool, Option<String>, Option<String>)> {
        let name = String::from_utf8(event.name().as_ref().to_vec()).ok()?;
        let mut id = None;
        let mut href = None;
        let mut style = String::new();
        let mut attribute_pointer_events = None;
        for attribute in event.attributes().with_checks(false) {
            let attribute = attribute.ok()?;
            let key = String::from_utf8(attribute.key.as_ref().to_vec()).ok()?;
            let value = attribute.decode_and_unescape_value(decoder).ok()?;
            match key.as_str() {
                "id" => id = Some(value.to_string()),
                "href" | "xlink:href" => href = Some(value.to_string()),
                "style" => style = value.to_string(),
                "pointer-events" => attribute_pointer_events = Some(value.to_string()),
                _ => {}
            }
        }
        let pointer_events = style_property(&style, "pointer-events")
            .or(attribute_pointer_events)
            .unwrap_or_else(|| inherited_pointer_events.to_string());
        Some((pointer_events, name == "use", id, href))
    }

    fn needs_geometry(pointer_events: &str) -> bool {
        matches!(
            pointer_events.trim().to_ascii_lowercase().as_str(),
            "fill"
                | "visiblefill"
                | "visible-fill"
                | "stroke"
                | "visiblestroke"
                | "visible-stroke"
                | "all"
                | "visible"
                | "bounding-box"
        )
    }

    let mut reader = Reader::from_str(source);
    let mut pointer_events_stack = vec!["auto".to_string()];
    let mut id_stack: Vec<Option<String>> = Vec::new();
    let mut use_ids = HashSet::new();
    let mut geometry_references = HashSet::new();
    let mut reference_edges: HashMap<String, HashSet<String>> = HashMap::new();
    loop {
        match reader.read_event().ok()? {
            Event::Start(event) => {
                let (pointer_events, is_use, id, href) = use_metadata(
                    &event,
                    reader.decoder(),
                    pointer_events_stack
                        .last()
                        .map(String::as_str)
                        .unwrap_or("auto"),
                )?;
                if is_use {
                    if let Some(use_id) = id.as_ref() {
                        use_ids.insert(use_id.clone());
                    }
                    let reference = href
                        .as_deref()
                        .and_then(|href| href.strip_prefix('#'))
                        .map(str::to_string);
                    if needs_geometry(&pointer_events)
                        && let Some(reference) = reference.as_ref()
                    {
                        geometry_references.insert(reference.clone());
                    }
                    if let Some(reference) = reference {
                        for owner_id in id_stack.iter().filter_map(Option::as_ref).chain(id.iter())
                        {
                            reference_edges
                                .entry(owner_id.clone())
                                .or_default()
                                .insert(reference.clone());
                        }
                    }
                }
                pointer_events_stack.push(pointer_events);
                id_stack.push(id);
            }
            Event::Empty(event) => {
                let (pointer_events, is_use, id, href) = use_metadata(
                    &event,
                    reader.decoder(),
                    pointer_events_stack
                        .last()
                        .map(String::as_str)
                        .unwrap_or("auto"),
                )?;
                if is_use {
                    if let Some(use_id) = id.as_ref() {
                        use_ids.insert(use_id.clone());
                    }
                    let reference = href
                        .as_deref()
                        .and_then(|href| href.strip_prefix('#'))
                        .map(str::to_string);
                    if needs_geometry(&pointer_events)
                        && let Some(reference) = reference.as_ref()
                    {
                        geometry_references.insert(reference.clone());
                    }
                    if let Some(reference) = reference {
                        for owner_id in id_stack.iter().filter_map(Option::as_ref).chain(id.iter())
                        {
                            reference_edges
                                .entry(owner_id.clone())
                                .or_default()
                                .insert(reference.clone());
                        }
                    }
                }
            }
            Event::End(_) => {
                if pointer_events_stack.len() > 1 {
                    pointer_events_stack.pop();
                }
                id_stack.pop();
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let mut pending = geometry_references.iter().cloned().collect::<Vec<_>>();
    while let Some(reference) = pending.pop() {
        if let Some(nested) = reference_edges.get(&reference) {
            for nested_reference in nested {
                if geometry_references.insert(nested_reference.clone()) {
                    pending.push(nested_reference.clone());
                }
            }
        }
    }
    Some((use_ids, geometry_references))
}

fn sanitize_svg_for_hit_testing(
    source: &str,
    geometry_references: &HashSet<String>,
) -> Option<String> {
    use quick_xml::events::{BytesStart, Event};
    use quick_xml::{Reader, Writer};

    fn clean_style(value: &str) -> String {
        value
            .split(';')
            .filter(|declaration| {
                let property = declaration
                    .split_once(':')
                    .map(|(property, _)| property.trim().to_ascii_lowercase());
                !matches!(property.as_deref(), Some("filter" | "mask"))
            })
            .collect::<Vec<_>>()
            .join(";")
    }

    fn style_property(value: &str, name: &str) -> Option<String> {
        value
            .split(';')
            .filter_map(|declaration| declaration.split_once(':'))
            .filter(|(property, _)| property.trim().eq_ignore_ascii_case(name))
            .map(|(_, value)| value.trim().to_string())
            .next_back()
    }

    fn geometry_style(tag: &str, pointer_events: &str) -> Option<&'static str> {
        if !matches!(
            tag,
            "path"
                | "rect"
                | "circle"
                | "ellipse"
                | "line"
                | "polyline"
                | "polygon"
                | "text"
                | "use"
        ) {
            return None;
        }
        match pointer_events.trim().to_ascii_lowercase().as_str() {
            "fill" | "visiblefill" | "visible-fill" => Some("fill:#000;fill-opacity:1;opacity:1"),
            "stroke" | "visiblestroke" | "visible-stroke" => {
                Some("stroke:#000;stroke-opacity:1;opacity:1")
            }
            "all" | "visible" | "bounding-box" => {
                Some("fill:#000;stroke:#000;fill-opacity:1;stroke-opacity:1;opacity:1")
            }
            _ => None,
        }
    }

    fn clean_start(
        event: &BytesStart<'_>,
        decoder: quick_xml::encoding::Decoder,
        inherited_pointer_events: &str,
        force_geometry: bool,
        geometry_references: &HashSet<String>,
    ) -> Option<(BytesStart<'static>, String, bool)> {
        let name = String::from_utf8(event.name().as_ref().to_vec()).ok()?;
        let mut clean = BytesStart::new(name.clone());
        let mut attributes = Vec::new();
        let mut style = String::new();
        let mut attribute_pointer_events = None;
        let mut id = None;
        for attribute in event.attributes().with_checks(false) {
            let attribute = attribute.ok()?;
            let key = String::from_utf8(attribute.key.as_ref().to_vec()).ok()?;
            if matches!(key.as_str(), "filter" | "mask") {
                continue;
            }
            let value = attribute.decode_and_unescape_value(decoder).ok()?;
            if key == "style" {
                style = clean_style(&value);
            } else {
                if key == "id" {
                    id = Some(value.to_string());
                }
                if key == "pointer-events" {
                    attribute_pointer_events = Some(value.to_string());
                }
                attributes.push((key, value.to_string()));
            }
        }
        let pointer_events = style_property(&style, "pointer-events")
            .or(attribute_pointer_events)
            .unwrap_or_else(|| inherited_pointer_events.to_string());
        let force_geometry =
            force_geometry || id.is_some_and(|id| geometry_references.contains(&id));
        let geometry_pointer_events = if force_geometry {
            "all"
        } else {
            &pointer_events
        };
        if let Some(geometry_style) = geometry_style(&name, geometry_pointer_events) {
            if !style.is_empty() && !style.trim_end().ends_with(';') {
                style.push(';');
            }
            style.push_str(geometry_style);
        }
        for (key, value) in &attributes {
            clean.push_attribute((key.as_str(), value.as_str()));
        }
        if !style.is_empty() {
            clean.push_attribute(("style", style.as_str()));
        }
        Some((clean.into_owned(), pointer_events, force_geometry))
    }

    let mut reader = Reader::from_str(source);
    reader.config_mut().trim_text(false);
    let mut writer = Writer::new(Vec::with_capacity(source.len()));
    let mut pointer_events_stack = vec!["auto".to_string()];
    let mut force_geometry_stack = vec![false];
    loop {
        let event = reader.read_event().ok()?;
        match event {
            Event::Start(event) => {
                let (event, pointer_events, force_geometry) = clean_start(
                    &event,
                    reader.decoder(),
                    pointer_events_stack
                        .last()
                        .map(String::as_str)
                        .unwrap_or("auto"),
                    force_geometry_stack.last().copied().unwrap_or(false),
                    geometry_references,
                )?;
                pointer_events_stack.push(pointer_events);
                force_geometry_stack.push(force_geometry);
                writer.write_event(Event::Start(event)).ok()?;
            }
            Event::Empty(event) => {
                let (event, _, _) = clean_start(
                    &event,
                    reader.decoder(),
                    pointer_events_stack
                        .last()
                        .map(String::as_str)
                        .unwrap_or("auto"),
                    force_geometry_stack.last().copied().unwrap_or(false),
                    geometry_references,
                )?;
                writer.write_event(Event::Empty(event)).ok()?;
            }
            Event::End(event) => {
                if pointer_events_stack.len() > 1 {
                    pointer_events_stack.pop();
                }
                if force_geometry_stack.len() > 1 {
                    force_geometry_stack.pop();
                }
                writer.write_event(Event::End(event.into_owned())).ok()?;
            }
            Event::Eof => break,
            event => writer.write_event(event.into_owned()).ok()?,
        }
    }
    String::from_utf8(writer.into_inner()).ok()
}

fn rasterize_tile(
    tree: &resvg::usvg::Tree,
    width: u32,
    height: u32,
    tile_x: u32,
    tile_y: u32,
) -> Option<TilePixels> {
    let tile_w = (tile_x + SVG_RASTER_TILE_SIZE)
        .min(width)
        .saturating_sub(tile_x);
    let tile_h = (tile_y + SVG_RASTER_TILE_SIZE)
        .min(height)
        .saturating_sub(tile_y);
    if tile_w == 0 || tile_h == 0 {
        return None;
    }
    let mut pixmap = resvg::tiny_skia::Pixmap::new(tile_w, tile_h)?;
    let size = tree.size();
    let transform = resvg::tiny_skia::Transform::from_row(
        width as f32 / size.width(),
        0.0,
        0.0,
        height as f32 / size.height(),
        -(tile_x as f32),
        -(tile_y as f32),
    );
    resvg::render(tree, transform, &mut pixmap.as_mut());
    Some(TilePixels {
        width: tile_w,
        height: tile_h,
        data: Arc::from(unpremultiply_rgba(pixmap.data())),
    })
}

fn unpremultiply_rgba(data: &[u8]) -> Vec<u8> {
    let mut rgba = data.to_vec();
    for pixel in rgba.chunks_exact_mut(4) {
        let alpha = pixel[3];
        if alpha == 0 || alpha == 255 {
            continue;
        }
        for channel in &mut pixel[..3] {
            *channel = ((*channel as u32 * 255 + alpha as u32 / 2) / alpha as u32).min(255) as u8;
        }
    }
    rgba
}

fn compose_tiles(
    tiles: &HashMap<(u32, u32), TilePixels>,
    width: u32,
    height: u32,
) -> Option<DecodedImage> {
    let mut rgba = vec![0_u8; width as usize * height as usize * 4];
    for ((tile_x, tile_y), tile) in tiles {
        for row in 0..tile.height {
            let dest_y = *tile_y + row;
            if dest_y >= height {
                continue;
            }
            let dest_start = ((dest_y * width + *tile_x) * 4) as usize;
            let src_start = (row * tile.width * 4) as usize;
            let copy_w = ((*tile_x + tile.width).min(width) - *tile_x) as usize * 4;
            let dest_end = dest_start + copy_w;
            let src_end = src_start + copy_w;
            if dest_end <= rgba.len() && src_end <= tile.data.len() {
                rgba[dest_start..dest_end].copy_from_slice(&tile.data[src_start..src_end]);
            }
        }
    }
    Some(DecodedImage {
        width,
        height,
        intrinsic_width: width,
        intrinsic_height: height,
        svg_intrinsic_size: None,
        data: Arc::new(rgba),
    })
}

fn all_tile_origins(width: u32, height: u32) -> HashSet<(u32, u32)> {
    let mut origins = HashSet::new();
    let mut y = 0;
    while y < height {
        let mut x = 0;
        while x < width {
            origins.insert((x, y));
            x = x.saturating_add(SVG_RASTER_TILE_SIZE);
        }
        y = y.saturating_add(SVG_RASTER_TILE_SIZE);
    }
    origins
}

fn dirty_tile_origins(
    previous: &[NodeRecord],
    next: &[NodeRecord],
    width: u32,
    height: u32,
    tree: &resvg::usvg::Tree,
) -> HashSet<(u32, u32)> {
    let previous_map: HashMap<&str, &NodeRecord> = previous
        .iter()
        .map(|record| (record.key.as_str(), record))
        .collect();
    let next_map: HashMap<&str, &NodeRecord> = next
        .iter()
        .map(|record| (record.key.as_str(), record))
        .collect();
    let mut dirty = HashSet::new();
    let scale_x = width as f32 / tree.size().width();
    let scale_y = height as f32 / tree.size().height();
    let mark = |dirty: &mut HashSet<(u32, u32)>, bounds: [f32; 4]| {
        let left = ((bounds[0] * scale_x) - SVG_DIRTY_PAD_PX).floor().max(0.0) as u32;
        let top = ((bounds[1] * scale_y) - SVG_DIRTY_PAD_PX).floor().max(0.0) as u32;
        let right = ((bounds[0] + bounds[2]) * scale_x + SVG_DIRTY_PAD_PX)
            .ceil()
            .min(width as f32) as u32;
        let bottom = ((bounds[1] + bounds[3]) * scale_y + SVG_DIRTY_PAD_PX)
            .ceil()
            .min(height as f32) as u32;
        let mut y = top / SVG_RASTER_TILE_SIZE * SVG_RASTER_TILE_SIZE;
        while y < bottom.max(top.saturating_add(1)) && y < height {
            let mut x = left / SVG_RASTER_TILE_SIZE * SVG_RASTER_TILE_SIZE;
            while x < right.max(left.saturating_add(1)) && x < width {
                dirty.insert((x, y));
                x = x.saturating_add(SVG_RASTER_TILE_SIZE);
            }
            y = y.saturating_add(SVG_RASTER_TILE_SIZE);
        }
    };
    for record in next {
        match previous_map.get(record.key.as_str()) {
            Some(old) if old.paint == record.paint => {}
            Some(old) => {
                mark(&mut dirty, old.bounds);
                mark(&mut dirty, record.bounds);
            }
            None => mark(&mut dirty, record.bounds),
        }
    }
    for record in previous {
        if !next_map.contains_key(record.key.as_str()) {
            mark(&mut dirty, record.bounds);
        }
    }
    if dirty.is_empty() {
        return HashSet::new();
    }
    dirty
}

fn shape_identity(tree: &resvg::usvg::Tree, records: &[NodeRecord]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hash_debug(&mut hasher, &tree.size());
    for record in records {
        record.key.hash(&mut hasher);
        record.geometry.hash(&mut hasher);
    }
    hasher.finish()
}

fn hash_debug(
    hasher: &mut std::collections::hash_map::DefaultHasher,
    value: &impl std::fmt::Debug,
) {
    format!("{value:?}").hash(hasher);
}

fn collect_node_records(group: &resvg::usvg::Group, prefix: &str) -> Vec<NodeRecord> {
    let mut records = Vec::new();
    collect_node_records_into(group, prefix, &mut records);
    records
}

fn collect_node_records_into(
    group: &resvg::usvg::Group,
    prefix: &str,
    records: &mut Vec<NodeRecord>,
) {
    records.push(node_record(
        prefix,
        "group",
        &format!(
            "{:?}:{:?}:{:?}:{}",
            group.abs_transform(),
            group.opacity(),
            group.blend_mode(),
            group.isolate()
        ),
        &format!(
            "{:?}:{:?}:{}",
            group.clip_path().is_some(),
            group.mask().is_some(),
            group.filters().len()
        ),
        group_bounds(group),
    ));
    for (index, node) in group.children().iter().enumerate() {
        let key = if node.id().is_empty() {
            format!("{prefix}/{index}")
        } else {
            node.id().to_string()
        };
        match node {
            resvg::usvg::Node::Group(child) => {
                collect_node_records_into(child, &key, records);
            }
            resvg::usvg::Node::Path(path) => records.push(node_record(
                &key,
                "path",
                &format!(
                    "{:?}:{:?}:{:?}:{}",
                    path.fill(),
                    path.stroke(),
                    path.abs_transform(),
                    path.is_visible()
                ),
                &format!("{:?}", path.data()),
                path.abs_stroke_bounding_box(),
            )),
            resvg::usvg::Node::Image(image) => records.push(node_record(
                &key,
                "image",
                &format!(
                    "{:?}:{}:{:?}",
                    image.abs_transform(),
                    image.is_visible(),
                    image.rendering_mode()
                ),
                &format!("{:?}", image.abs_bounding_box()),
                image.abs_bounding_box(),
            )),
            resvg::usvg::Node::Text(text) => records.push(node_record(
                &key,
                "text",
                &format!(
                    "{:?}:{:?}:{:?}",
                    text.abs_transform(),
                    text.chunks(),
                    text.flattened()
                ),
                &format!("{:?}", text.abs_bounding_box()),
                text.abs_bounding_box(),
            )),
        }
    }
}

fn group_bounds(group: &resvg::usvg::Group) -> resvg::usvg::Rect {
    group.abs_layer_bounding_box().to_rect()
}

fn node_record(
    key: &str,
    kind: &str,
    paint: &str,
    geometry: &str,
    bounds: resvg::usvg::Rect,
) -> NodeRecord {
    let mut paint_hasher = std::collections::hash_map::DefaultHasher::new();
    kind.hash(&mut paint_hasher);
    paint.hash(&mut paint_hasher);
    let mut geometry_hasher = std::collections::hash_map::DefaultHasher::new();
    kind.hash(&mut geometry_hasher);
    geometry.hash(&mut geometry_hasher);
    NodeRecord {
        key: format!("{kind}:{key}"),
        paint: paint_hasher.finish(),
        geometry: geometry_hasher.finish(),
        bounds: [bounds.x(), bounds.y(), bounds.width(), bounds.height()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rasterizes_current_color_replacement_on_stroke_only_icons() {
        clear_cache();
        let source = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="rgba(41, 93, 167, 1)" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
            <path d="m9.5 12.5 5.7-5.7a3 3 0 0 1 4.2 4.2l-7.8 7.8a5 5 0 0 1-7.1-7.1l7.4-7.4"/>
            <path d="m7.4 14.6 7.1-7.1"/>
        </svg>"#;

        let image = get_or_render(source, 21, 21).expect("stroke-only SVG should rasterize");
        assert!(
            image.data.chunks_exact(4).any(|pixel| pixel[3] != 0),
            "stroke-only SVG should produce visible pixels"
        );
    }

    const COMPLEX_SVG: &str = r##"
        <svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 32 32">
          <defs>
            <linearGradient id="g"><stop stop-color="#ff0000"/><stop offset="1" stop-color="#0000ff"/></linearGradient>
            <clipPath id="c"><circle cx="16" cy="16" r="12"/></clipPath>
          </defs>
          <rect width="32" height="32" fill="url(#g)" clip-path="url(#c)"/>
        </svg>
    "##;

    #[test]
    fn renders_viewbox_gradient_and_clip() {
        clear_cache();
        let image = get_or_render(COMPLEX_SVG, 64, 64).unwrap();
        assert_eq!((image.width, image.height), (64, 64));
        let center = (32 * 64 + 32) * 4;
        assert!(image.data[center] > 0);
        assert!(image.data[center + 2] > 0);
        assert_eq!(image.data[3], 0);
    }

    #[test]
    fn reuses_raster_for_identical_content_and_size() {
        clear_cache();
        assert!(get_or_render(COMPLEX_SVG, 64, 64).is_some());
        assert!(get_or_render(COMPLEX_SVG, 64, 64).is_some());
        let stats = cache_stats();
        assert_eq!(stats.entries, 1);
        assert_eq!(stats.parsed_entries, 1);
        assert_eq!(stats.parse_misses, 1);
        assert_eq!(stats.parse_hits, 1);
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.hits, 1);
    }

    #[test]
    fn content_or_intrinsic_size_creates_new_revision() {
        clear_cache();
        assert!(get_or_render(COMPLEX_SVG, 32, 32).is_some());
        assert!(get_or_render(COMPLEX_SVG, 64, 64).is_some());
        let changed = COMPLEX_SVG.replace("#ff0000", "#00ff00");
        assert!(get_or_render(&changed, 64, 64).is_some());
        let stats = cache_stats();
        assert_eq!(stats.entries, 3);
        assert_eq!(stats.parsed_entries, 2);
        assert_eq!(stats.parse_misses, 2);
        assert_eq!(stats.parse_hits, 1);
        assert_eq!(stats.misses, 3);
    }

    fn pixel_at(image: &DecodedImage, x: u32, y: u32) -> [u8; 4] {
        let index = ((y * image.width + x) * 4) as usize;
        image.data[index..index + 4].try_into().unwrap()
    }

    #[test]
    fn paint_only_mutation_reuses_tiles_outside_the_dirty_bounds() {
        clear_cache();
        let red = r##"<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128">
            <rect id="stable" x="80" y="80" width="40" height="40" fill="#0000ff"/>
            <rect id="anim" x="0" y="0" width="24" height="24" fill="#ff0000"/>
        </svg>"##;
        let green = red.replace("#ff0000", "#00ff00");
        let first = get_or_render(red, 128, 128).unwrap();
        let after_first = cache_stats();
        let second = get_or_render(&green, 128, 128).unwrap();
        let stats = cache_stats();
        assert!(
            stats.tile_hits > after_first.tile_hits,
            "unchanged tiles should be copied, hits {} -> {}",
            after_first.tile_hits,
            stats.tile_hits
        );
        let new_tile_misses = stats.tile_misses - after_first.tile_misses;
        let new_tile_hits = stats.tile_hits - after_first.tile_hits;
        assert!(
            new_tile_misses < new_tile_hits,
            "dirty tiles ({new_tile_misses}) should be fewer than reused tiles ({new_tile_hits})"
        );
        assert_eq!(pixel_at(&first, 100, 100), pixel_at(&second, 100, 100));
        assert_ne!(pixel_at(&first, 8, 8), pixel_at(&second, 8, 8));
        assert!(pixel_at(&second, 8, 8)[1] > pixel_at(&second, 8, 8)[0]);
    }

    #[test]
    fn topology_change_does_not_reuse_stale_tiles() {
        clear_cache();
        let base = r##"<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128">
            <rect id="stable" x="80" y="80" width="40" height="40" fill="#0000ff"/>
        </svg>"##;
        let extra = r##"<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128">
            <rect id="stable" x="80" y="80" width="40" height="40" fill="#0000ff"/>
            <circle id="extra" cx="16" cy="16" r="12" fill="#ff0000"/>
        </svg>"##;
        let without = get_or_render(base, 128, 128).unwrap();
        let after_first = cache_stats();
        let with_circle = get_or_render(extra, 128, 128).unwrap();
        let stats = cache_stats();
        assert_eq!(
            stats.tile_hits, after_first.tile_hits,
            "a new node must not copy tiles from a different document topology"
        );
        assert_ne!(pixel_at(&without, 16, 16), pixel_at(&with_circle, 16, 16));
        assert_eq!(
            pixel_at(&without, 100, 100),
            pixel_at(&with_circle, 100, 100)
        );
    }

    #[test]
    fn text_fill_mutation_reuses_tiles_outside_the_dirty_bounds() {
        clear_cache();
        let red = r##"<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128">
            <rect id="stable" x="80" y="80" width="40" height="40" fill="#0000ff"/>
            <text id="anim" x="4" y="24" font-size="20" fill="#ff0000">Hi</text>
        </svg>"##;
        let green = red.replace("#ff0000", "#00ff00");
        let first = get_or_render(red, 128, 128).unwrap();
        let after_first = cache_stats();
        let second = get_or_render(&green, 128, 128).unwrap();
        let stats = cache_stats();
        assert!(
            stats.tile_hits > after_first.tile_hits,
            "unchanged tiles should be copied after a text fill change"
        );
        assert!(
            stats.tile_misses > after_first.tile_misses,
            "text fill change should rerasterize the glyph tiles"
        );
        assert_eq!(pixel_at(&first, 100, 100), pixel_at(&second, 100, 100));
        let changed = (0..48)
            .flat_map(|y| (0..48).map(move |x| (x, y)))
            .any(|(x, y)| pixel_at(&first, x, y) != pixel_at(&second, x, y));
        assert!(
            changed,
            "text fill change should alter pixels in the glyph region"
        );
    }

    #[test]
    fn invalid_source_is_parsed_only_once() {
        clear_cache();
        assert!(get_or_render("<svg><", 32, 32).is_none());
        assert!(get_or_render("<svg><", 64, 64).is_none());
        let stats = cache_stats();
        assert_eq!(stats.parsed_entries, 1);
        assert_eq!(stats.parse_misses, 1);
        assert_eq!(stats.parse_hits, 1);
        assert_eq!(stats.entries, 0);
    }

    #[test]
    fn hit_test_returns_deepest_dom_chain() {
        clear_cache();
        let source = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50">
            <rect id="back" x="0" y="0" width="100" height="50"/>
            <circle id="front" cx="50" cy="25" r="10"/>
        </svg>"#;
        let targets = vec![
            SvgEventTarget {
                svg_id: "back".into(),
                render_index: Some(0),
                pointer_events: "auto".into(),
                host_chain: vec![2, 1],
            },
            SvgEventTarget {
                svg_id: "front".into(),
                render_index: Some(1),
                pointer_events: "auto".into(),
                host_chain: vec![3, 1],
            },
        ];
        assert_eq!(
            hit_test(source, 200, 100, &targets, 100.0, 50.0),
            Some(vec![3, 1])
        );
        assert_eq!(
            hit_test(source, 200, 100, &targets, 10.0, 10.0),
            Some(vec![2, 1])
        );
        assert_eq!(
            hit_test(source, 200, 100, &targets, 81.0, 31.0),
            Some(vec![2, 1]),
            "transparent circle corner must fall through to the painted rect"
        );
        let stats = cache_stats();
        assert_eq!(stats.hit_masks, 2);
        assert!(stats.mask_hits >= 1);
    }

    #[test]
    fn anonymous_shapes_use_render_order_mapping() {
        clear_cache();
        let source = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50">
            <rect x="0" y="0" width="100" height="50"/>
            <circle cx="50" cy="25" r="10"/>
        </svg>"#;
        let targets = vec![
            SvgEventTarget {
                svg_id: String::new(),
                render_index: Some(0),
                pointer_events: "auto".into(),
                host_chain: vec![2, 1],
            },
            SvgEventTarget {
                svg_id: String::new(),
                render_index: Some(1),
                pointer_events: "auto".into(),
                host_chain: vec![3, 1],
            },
        ];
        assert_eq!(
            hit_test(source, 100, 50, &targets, 50.0, 25.0),
            Some(vec![3, 1])
        );
        assert_eq!(
            hit_test(source, 100, 50, &targets, 40.0, 15.0),
            Some(vec![2, 1])
        );
        let mut pointer_none = targets.clone();
        pointer_none[1].pointer_events = "none".into();
        assert_eq!(
            hit_test(source, 100, 50, &pointer_none, 50.0, 25.0),
            Some(vec![2, 1])
        );
        let mut bounding_box = targets;
        bounding_box[1].pointer_events = "bounding-box".into();
        assert_eq!(
            hit_test(source, 100, 50, &bounding_box, 40.0, 15.0),
            Some(vec![3, 1])
        );
    }

    #[test]
    fn pointer_events_distinguishes_fill_stroke_and_painted() {
        clear_cache();
        let source = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50">
            <rect id="back" width="100" height="50"/>
            <circle id="ring" cx="50" cy="25" r="10" fill="none" stroke="red" stroke-width="2"/>
        </svg>"#;
        let make_targets = |pointer_events: &str| {
            vec![
                SvgEventTarget {
                    svg_id: "back".into(),
                    render_index: Some(0),
                    pointer_events: "auto".into(),
                    host_chain: vec![2, 1],
                },
                SvgEventTarget {
                    svg_id: "ring".into(),
                    render_index: Some(1),
                    pointer_events: pointer_events.into(),
                    host_chain: vec![3, 1],
                },
            ]
        };

        assert_eq!(
            hit_test(source, 100, 50, &make_targets("painted"), 50.0, 25.0),
            Some(vec![2, 1])
        );
        assert_eq!(
            hit_test(source, 100, 50, &make_targets("fill"), 50.0, 25.0),
            Some(vec![3, 1])
        );
        assert_eq!(
            hit_test(source, 100, 50, &make_targets("stroke"), 50.0, 25.0),
            Some(vec![2, 1])
        );
        assert_eq!(
            hit_test(source, 100, 50, &make_targets("stroke"), 60.0, 25.0),
            Some(vec![3, 1])
        );
    }

    #[test]
    fn unpainted_geometry_is_retained_for_geometry_pointer_modes() {
        clear_cache();
        let fill_source = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50">
            <rect id="back" width="100" height="50"/>
            <g pointer-events="fill">
                <circle id="front" cx="50" cy="25" r="10" fill="none" stroke="none"/>
            </g>
        </svg>"#;
        let targets = |pointer_events: &str| {
            vec![
                SvgEventTarget {
                    svg_id: "back".into(),
                    render_index: Some(0),
                    pointer_events: "auto".into(),
                    host_chain: vec![2, 1],
                },
                SvgEventTarget {
                    svg_id: "front".into(),
                    render_index: Some(1),
                    pointer_events: pointer_events.into(),
                    host_chain: vec![4, 3, 1],
                },
            ]
        };
        assert_eq!(
            hit_test(fill_source, 100, 50, &targets("fill"), 50.0, 25.0),
            Some(vec![4, 3, 1])
        );

        let painted_source =
            fill_source.replace("pointer-events=\"fill\"", "pointer-events=\"painted\"");
        assert_eq!(
            hit_test(&painted_source, 100, 50, &targets("painted"), 50.0, 25.0),
            Some(vec![2, 1])
        );

        let stroke_source = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50">
            <rect id="back" width="100" height="50"/>
            <line id="front" x1="20" y1="25" x2="80" y2="25"
                fill="none" stroke="none" pointer-events="stroke"/>
        </svg>"#;
        assert_eq!(
            hit_test(stroke_source, 100, 50, &targets("stroke"), 50.0, 24.5),
            Some(vec![4, 3, 1])
        );

        let text_source = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50">
            <rect id="back" width="100" height="50"/>
            <text id="label" x="10" y="30" font-size="20"
                fill="none" stroke="none" pointer-events="fill">A</text>
        </svg>"#;
        let text_targets = vec![
            targets("fill")[0].clone(),
            SvgEventTarget {
                svg_id: "label".into(),
                render_index: Some(1),
                pointer_events: "fill".into(),
                host_chain: vec![3, 1],
            },
        ];
        assert_eq!(
            hit_test(text_source, 100, 50, &text_targets, 10.5, 11.0),
            Some(vec![3, 1])
        );
    }

    #[test]
    fn author_id_use_element_routes_to_context_group() {
        clear_cache();
        let source = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50">
            <defs><circle id="shape" cx="10" cy="10" r="8"/></defs>
            <use id="instance" href="#shape" x="20"/>
        </svg>"##;
        let targets = vec![SvgEventTarget {
            svg_id: "instance".into(),
            render_index: None,
            pointer_events: "auto".into(),
            host_chain: vec![4, 1],
        }];
        assert_eq!(
            hit_test(source, 100, 50, &targets, 30.0, 10.0),
            Some(vec![4, 1])
        );
        assert_eq!(hit_test(source, 100, 50, &targets, 5.0, 5.0), None);
    }

    #[test]
    fn internal_id_routes_anonymous_use_to_context_group() {
        clear_cache();
        let source = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50">
            <defs><circle id="shape" cx="10" cy="10" r="8"/></defs>
            <use id="__w3cos_internal_use_4" href="#shape" x="20"/>
        </svg>"##;
        let targets = vec![SvgEventTarget {
            svg_id: "__w3cos_internal_use_4".into(),
            render_index: None,
            pointer_events: "auto".into(),
            host_chain: vec![4, 1],
        }];
        assert_eq!(
            hit_test(source, 100, 50, &targets, 30.0, 10.0),
            Some(vec![4, 1])
        );
        assert_eq!(hit_test(source, 100, 50, &targets, 5.0, 5.0), None);
    }

    #[test]
    fn geometry_pointer_modes_retain_unpainted_use_shadow_content() {
        clear_cache();
        let source = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50">
            <defs>
                <circle id="leaf" cx="10" cy="10" r="8" fill="none" stroke="none"/>
                <g id="shape"><use href="#leaf"/></g>
            </defs>
            <rect id="back" width="100" height="50"/>
            <use id="geometry" href="#shape" x="20" pointer-events="fill"/>
            <use id="painted" href="#shape" x="50" pointer-events="painted"/>
        </svg>"##;
        let targets = vec![
            SvgEventTarget {
                svg_id: "back".into(),
                render_index: Some(0),
                pointer_events: "auto".into(),
                host_chain: vec![2, 1],
            },
            SvgEventTarget {
                svg_id: "geometry".into(),
                render_index: None,
                pointer_events: "fill".into(),
                host_chain: vec![4, 1],
            },
            SvgEventTarget {
                svg_id: "painted".into(),
                render_index: None,
                pointer_events: "painted".into(),
                host_chain: vec![5, 1],
            },
        ];

        assert_eq!(
            hit_test(source, 100, 50, &targets, 30.0, 10.0),
            Some(vec![4, 1])
        );
        assert_eq!(
            hit_test(source, 100, 50, &targets, 60.0, 10.0),
            Some(vec![2, 1]),
            "synthetic hit-tree paint must not make a painted use interactive"
        );
    }

    #[test]
    fn visible_pointer_modes_honor_visibility_but_painted_does_not() {
        clear_cache();
        let source = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50">
            <rect id="back" width="100" height="50"/>
            <circle id="hidden" cx="50" cy="25" r="10" visibility="hidden"/>
        </svg>"#;
        let targets = |pointer_events: &str| {
            vec![
                SvgEventTarget {
                    svg_id: "back".into(),
                    render_index: Some(0),
                    pointer_events: "auto".into(),
                    host_chain: vec![2, 1],
                },
                SvgEventTarget {
                    svg_id: "hidden".into(),
                    render_index: Some(1),
                    pointer_events: pointer_events.into(),
                    host_chain: vec![3, 1],
                },
            ]
        };
        assert_eq!(
            hit_test(source, 100, 50, &targets("visiblePainted"), 50.0, 25.0),
            Some(vec![2, 1])
        );
        assert_eq!(
            hit_test(source, 100, 50, &targets("painted"), 50.0, 25.0),
            Some(vec![3, 1])
        );
    }

    #[test]
    fn text_pointer_modes_use_character_cell_geometry() {
        clear_cache();
        let source = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50">
            <rect id="back" width="100" height="50"/>
            <text id="label" x="10" y="30" font-size="20" fill="black">A</text>
        </svg>"#;
        let targets = |pointer_events: &str| {
            vec![
                SvgEventTarget {
                    svg_id: "back".into(),
                    render_index: Some(0),
                    pointer_events: "auto".into(),
                    host_chain: vec![2, 1],
                },
                SvgEventTarget {
                    svg_id: "label".into(),
                    render_index: Some(1),
                    pointer_events: pointer_events.into(),
                    host_chain: vec![3, 1],
                },
            ]
        };
        for pointer_events in ["fill", "stroke", "all", "painted"] {
            assert_eq!(
                hit_test(source, 100, 50, &targets(pointer_events), 10.5, 11.0),
                Some(vec![3, 1]),
                "{pointer_events} should use the complete text character cell"
            );
        }
    }

    #[test]
    fn image_fill_and_stroke_modes_use_image_rectangle() {
        clear_cache();
        let source = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50">
            <rect id="back" width="100" height="50"/>
            <image id="pixel" x="20" y="10" width="20" height="20"
                href="data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNgYAAAAAMAASsJTYQAAAAASUVORK5CYII="/>
        </svg>"#;
        let targets = |pointer_events: &str| {
            vec![
                SvgEventTarget {
                    svg_id: "back".into(),
                    render_index: Some(0),
                    pointer_events: "auto".into(),
                    host_chain: vec![2, 1],
                },
                SvgEventTarget {
                    svg_id: "pixel".into(),
                    render_index: Some(1),
                    pointer_events: pointer_events.into(),
                    host_chain: vec![3, 1],
                },
            ]
        };
        for pointer_events in ["fill", "stroke", "all"] {
            assert_eq!(
                hit_test(source, 100, 50, &targets(pointer_events), 30.0, 20.0),
                Some(vec![3, 1]),
                "{pointer_events} should use the complete image rectangle"
            );
        }
        assert_eq!(
            hit_test(source, 100, 50, &targets("painted"), 30.0, 20.0),
            Some(vec![2, 1])
        );
        let hidden_source = source.replace("<image id=", "<image visibility=\"hidden\" id=");
        assert_eq!(
            hit_test(&hidden_source, 100, 50, &targets("fill"), 30.0, 20.0),
            Some(vec![3, 1])
        );
        assert_eq!(
            hit_test(&hidden_source, 100, 50, &targets("visibleFill"), 30.0, 20.0),
            Some(vec![2, 1])
        );
    }

    #[test]
    fn mask_transparency_does_not_remove_pointer_hits() {
        clear_cache();
        let source = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50">
            <defs>
                <mask id="hide"><rect width="100" height="50" fill="black"/></mask>
            </defs>
            <rect id="back" width="100" height="50"/>
            <rect id="front" x="20" y="10" width="20" height="20"
                style="fill:red;mask:url(#hide)"/>
        </svg>"##;
        let targets = vec![
            SvgEventTarget {
                svg_id: "back".into(),
                render_index: Some(0),
                pointer_events: "auto".into(),
                host_chain: vec![2, 1],
            },
            SvgEventTarget {
                svg_id: "front".into(),
                render_index: Some(1),
                pointer_events: "auto".into(),
                host_chain: vec![3, 1],
            },
        ];
        assert_eq!(
            hit_test(source, 100, 50, &targets, 30.0, 20.0),
            Some(vec![3, 1])
        );
    }

    #[test]
    fn filter_output_does_not_expand_pointer_hits() {
        clear_cache();
        let source = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50">
            <defs>
                <filter id="blur" x="-100%" y="-100%" width="300%" height="300%">
                    <feGaussianBlur stdDeviation="8"/>
                </filter>
            </defs>
            <rect id="back" width="100" height="50"/>
            <circle id="front" cx="50" cy="25" r="5" fill="red" filter="url(#blur)"/>
        </svg>"##;
        let targets = vec![
            SvgEventTarget {
                svg_id: "back".into(),
                render_index: Some(0),
                pointer_events: "auto".into(),
                host_chain: vec![2, 1],
            },
            SvgEventTarget {
                svg_id: "front".into(),
                render_index: Some(1),
                pointer_events: "auto".into(),
                host_chain: vec![3, 1],
            },
        ];
        assert_eq!(
            hit_test(source, 100, 50, &targets, 50.0, 25.0),
            Some(vec![3, 1])
        );
        assert_eq!(
            hit_test(source, 100, 50, &targets, 62.0, 25.0),
            Some(vec![2, 1])
        );
    }

    #[test]
    fn clip_path_still_limits_pointer_hits() {
        clear_cache();
        let source = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50">
            <defs>
                <clipPath id="left-half"><rect x="20" y="10" width="10" height="20"/></clipPath>
            </defs>
            <rect id="back" width="100" height="50"/>
            <g transform="translate(10)" clip-path="url(#left-half)">
                <rect id="front" x="20" y="10" width="20" height="20" fill="red"/>
            </g>
        </svg>"##;
        let targets = vec![
            SvgEventTarget {
                svg_id: "back".into(),
                render_index: Some(0),
                pointer_events: "auto".into(),
                host_chain: vec![2, 1],
            },
            SvgEventTarget {
                svg_id: "front".into(),
                render_index: Some(1),
                pointer_events: "auto".into(),
                host_chain: vec![3, 1],
            },
        ];
        assert_eq!(
            hit_test(source, 100, 50, &targets, 35.0, 20.0),
            Some(vec![3, 1])
        );
        assert_eq!(
            hit_test(source, 100, 50, &targets, 45.0, 20.0),
            Some(vec![2, 1])
        );
    }

    #[test]
    fn nested_group_clip_paths_intersect_in_local_coordinates() {
        clear_cache();
        let source = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50">
            <defs>
                <clipPath id="ancestor">
                    <rect x="22" y="10" width="6" height="20"/>
                </clipPath>
                <clipPath id="inner" clip-path="url(#ancestor)">
                    <rect x="20" y="10" width="10" height="20"/>
                </clipPath>
                <clipPath id="outer">
                    <rect x="10" y="10" width="30" height="20"
                        clip-path="url(#inner)"/>
                </clipPath>
            </defs>
            <rect id="back" width="100" height="50"/>
            <g clip-path="url(#outer)">
                <rect id="front" x="0" y="0" width="50" height="40"/>
            </g>
        </svg>"##;
        let targets = vec![
            SvgEventTarget {
                svg_id: "back".into(),
                render_index: Some(0),
                pointer_events: "auto".into(),
                host_chain: vec![2, 1],
            },
            SvgEventTarget {
                svg_id: "front".into(),
                render_index: Some(1),
                pointer_events: "auto".into(),
                host_chain: vec![3, 1],
            },
        ];

        assert_eq!(
            hit_test(source, 100, 50, &targets, 25.0, 20.0),
            Some(vec![3, 1])
        );
        assert_eq!(
            hit_test(source, 100, 50, &targets, 29.0, 20.0),
            Some(vec![2, 1]),
            "the nested clip must be applied before its group joins the outer clip"
        );
    }
}
