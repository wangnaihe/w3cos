//! Deterministic offscreen rendering for conformance and embedding tools.

use anyhow::{Result, bail};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use crate::layout::{self, LayoutRect};
use crate::paint_artifact::{PaintArtifact, PaintNode};
#[cfg(feature = "skia")]
use crate::render_skia::SkiaRasterizer;

/// Pixel checksum used to prove a headless frame was actually painted.
///
/// Equivalent to `len + Σ byte[i] * ((i % 251) + 1)` over every byte, with
/// wrapping arithmetic. The factor walks 1..=251 so the hot loop does not
/// divide once per byte.
pub(crate) fn frame_byte_checksum(rgba: &[u8]) -> u64 {
    let mut checksum = rgba.len() as u64;
    let mut index = 0usize;
    while index < rgba.len() {
        // The weight is `(index % 251) + 1` and wraps to 1 after 251.
        // Each run stays inside one cycle so the inner loop does not branch.
        let factor0 = (index % 251) + 1;
        let run = (252 - factor0).min(rgba.len() - index);
        let slice = &rgba[index..index + run];
        let mut factor = factor0 as u64;
        let mut offset = 0usize;
        while offset + 4 <= slice.len() {
            let factor1 = factor + 1;
            let factor2 = factor + 2;
            let factor3 = factor + 3;
            checksum = checksum
                .wrapping_add(u64::from(slice[offset]).wrapping_mul(factor))
                .wrapping_add(u64::from(slice[offset + 1]).wrapping_mul(factor1))
                .wrapping_add(u64::from(slice[offset + 2]).wrapping_mul(factor2))
                .wrapping_add(u64::from(slice[offset + 3]).wrapping_mul(factor3));
            factor += 4;
            offset += 4;
        }
        while offset < slice.len() {
            checksum = checksum.wrapping_add(u64::from(slice[offset]).wrapping_mul(factor));
            factor += 1;
            offset += 1;
        }
        index += run;
    }
    checksum
}

/// One fully rasterized document frame in premultiplied RGBA byte order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadlessFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

struct RetainedPaint {
    structure: u64,
    /// Paint styles do not depend on the layout rects, so an inset or scroll
    /// update can reuse the artifact.
    rect_independent: bool,
    artifact: PaintArtifact,
    /// The paint layout (`paint_layout_generation`) the artifact shows.
    layout_generation: u64,
    /// `W3COS_PAINT_QUIET_FRAME`: the artifact's diff against itself (dirty,
    /// moved), kept while the artifact, its rects and its scroll stay as
    /// they are, so a run of quiet frames diffs once.
    /// `W3COS_PAINT_QUIET_EXIT`: after a quiet frame with nothing dirty, the
    /// frame size and the nested scrollers' offsets it showed (node id,
    /// offset); a later frame with the same ones returns the last pixels.
    #[cfg_attr(not(feature = "skia"), allow(dead_code))]
    quiet_exit: Option<((u32, u32), Vec<(u32, (f32, f32))>)>,
    #[cfg_attr(not(feature = "skia"), allow(dead_code))]
    quiet_diff: Option<(Vec<bool>, Vec<Option<(f32, f32)>>)>,
}

/// The damage of `diff` against the retained artifact, measured in
/// `replay_frame` where the layers place it, or `None` as soon as `stop`
/// accepts part of it (the caller then needs none of the rest): for the
/// off-surface skip without `W3COS_PAINT_DAMAGE` (measured, not cached) and
/// for the damage early-out (`cached`: through the ink cache).
#[cfg(feature = "skia")]
pub(crate) fn lazy_damage(
    cur: &PaintArtifact,
    diff: (&[bool], &[Option<(f32, f32)>]),
    cached: bool,
    mut stop: impl FnMut(&[crate::retained_layers::ClientDamage]) -> bool,
) -> Option<Vec<crate::retained_layers::ClientDamage>> {
    RETAINED_PAINT.with(|slot| {
        let slot = slot.try_borrow().ok()?;
        let prev = &slot.as_ref()?.artifact;
        if cached {
            crate::retained_layers::paint_damage_cached(
                prev,
                cur,
                diff,
                Some(&mut |rects| stop(rects)),
            )
        } else {
            crate::retained_layers::paint_damage_until(prev, cur, diff, &mut |rects| stop(rects))
        }
    })
}

thread_local! {
    static CHECKSUM_ONLY: Cell<bool> = const { Cell::new(false) };
    static LAST_FRAME_CHECKSUM: Cell<u64> = const { Cell::new(0) };
    static RETAINED_PAINT: RefCell<Option<RetainedPaint>> = const { RefCell::new(None) };
    /// The node kinds of the artifact the last frame replaced, by node index.
    /// The next build copies into them (`refill_kind`), so an unchanged text
    /// keeps its buffer instead of being allocated and freed every frame.
    static RECYCLED_KINDS: RefCell<Vec<w3cos_std::ComponentKind>> = const { RefCell::new(Vec::new()) };
    /// With `RECYCLED_KINDS`: the box count and the changed ranges of the
    /// frame that replaced their artifact, when that frame used a touched set
    /// (`W3COS_PAINT_KIND_KEEP`).
    static RECYCLED_CHANGED: RefCell<Option<(usize, Vec<(u32, u32)>)>> = const { RefCell::new(None) };
    /// `W3COS_PAINT_PERSIST_INPLACE`: the whole nodes of the artifact the
    /// last frame replaced (instead of `RECYCLED_KINDS`), and its trimmed
    /// boxes.
    static RECYCLED_NODES: RefCell<(Vec<Option<PaintNode>>, Option<std::sync::Arc<Vec<u32>>>)> =
        const { RefCell::new((Vec::new(), None)) };
    /// `W3COS_PAINT_NODES_SPLICE`: the nodes of the artifact the last frame
    /// replaced, kept whole (instead of `RECYCLED_NODES`' per-box slots).
    static RECYCLED_NODE_VEC: RefCell<Vec<PaintNode>> = const { RefCell::new(Vec::new()) };
    /// (frames spliced, boxes rebuilt, verify mismatches).
    pub(crate) static PAINT_NODES_SPLICE_STATS: Cell<(u64, u64, u64)> = const { Cell::new((0, 0, 0)) };
    /// (kinds kept without a copy, kinds copied, verify mismatches).
    static PAINT_KIND_KEEP_STATS: Cell<(u64, u64, u64)> = const { Cell::new((0, 0, 0)) };
}

#[cfg(test)]
thread_local! {
    pub(crate) static PAINT_KIND_KEEP_OVERRIDE: Cell<Option<u8>> = const { Cell::new(None) };
}

/// `W3COS_PAINT_NODES_SPLICE=1|verify` (off by default; with
/// `W3COS_PAINT_PERSIST_INPLACE`): the paint nodes of the artifact two
/// frames back are kept as one array and updated in place over the boxes
/// either of the last two layout splices touched, as Blink keeps its paint
/// chunks and invalidates only the dirty ones, instead of a node array
/// rebuilt box by box every frame. A box outside those ranges takes its
/// recycled node exactly as the per-box path does. `verify` also recomputes
/// each box's keep decision the per-box way and reports a difference.
fn paint_nodes_splice_mode() -> u8 {
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| {
        if crate::paint_artifact::persist_inplace_mode() == 0 {
            return 0;
        }
        match std::env::var("W3COS_PAINT_NODES_SPLICE").as_deref() {
            Ok("1" | "true") => 1,
            Ok("verify") => 2,
            _ => 0,
        }
    })
}

/// `W3COS_PAINT_KIND_KEEP=1|verify`: a recycled kind comes from the artifact
/// two frames back. A box that neither of the last two layout splices
/// touched (the `PAINT_TOUCHED` rule: an untouched box keeps its inputs, its
/// kind among them) already holds this frame's kind, so the paint node takes
/// it as it is instead of copying the text again. `verify` compares each such
/// kind and copies on a difference. On by default (`=0` turns it off).
fn paint_kind_keep_mode() -> u8 {
    #[cfg(test)]
    if let Some(mode) = PAINT_KIND_KEEP_OVERRIDE.with(Cell::get) {
        return mode;
    }
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(
        || match std::env::var("W3COS_PAINT_KIND_KEEP").ok().as_deref() {
            Some("0" | "false") => 0,
            Some("verify") => 2,
            _ => 1,
        },
    )
}

#[cfg(test)]
pub(crate) fn paint_kind_keep_stats() -> (u64, u64, u64) {
    PAINT_KIND_KEEP_STATS.with(Cell::get)
}

#[cfg(feature = "cpu-render")]
thread_local! {
    static CPU_PIXMAP: RefCell<Option<tiny_skia::Pixmap>> = const { RefCell::new(None) };
}

#[cfg(test)]
thread_local! {
    static RETAINED_REUSES: Cell<u32> = const { Cell::new(0) };
}

pub(crate) fn invalidate_retained_frame() {
    RETAINED_PAINT.with(|slot| *slot.borrow_mut() = None);
    #[cfg(feature = "skia")]
    HEADLESS_SKIA_CURRENT.with(|current| current.set(false));
}

#[cfg(feature = "skia")]
thread_local! {
    /// One headless rasterizer kept across frames when layer reuse is on.
    static HEADLESS_SKIA: RefCell<Option<SkiaRasterizer>> = const { RefCell::new(None) };
    /// The kept rasterizer painted the last frame, so its recordings match
    /// the artifact in `RETAINED_PAINT`.
    static HEADLESS_SKIA_CURRENT: Cell<bool> = const { Cell::new(false) };
    /// Layers the kept rasterizer reused in its last frame, and the count.
    static HEADLESS_SKIA_REUSED: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
    /// Font registry revision when the kept recordings were made.
    static HEADLESS_SKIA_FONTS: Cell<u64> = const { Cell::new(u64::MAX) };
}

#[cfg(all(test, feature = "skia"))]
thread_local! {
    static PAINT_LAYER_REUSE_OVERRIDE: Cell<Option<bool>> = const { Cell::new(None) };
}

/// Keep the headless rasterizer and repaint only the compositor layers
/// holding changed paint clients; `W3COS_PAINT_LAYER_REUSE=0` repaints
/// every layer with a fresh rasterizer each frame.
#[cfg(feature = "skia")]
fn paint_layer_reuse_enabled() -> bool {
    #[cfg(test)]
    if let Some(enabled) = PAINT_LAYER_REUSE_OVERRIDE.with(Cell::get) {
        return enabled;
    }
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED
        .get_or_init(|| std::env::var("W3COS_PAINT_LAYER_REUSE").map_or(true, |value| value != "0"))
}

/// `W3COS_PAINT_LAYER_REUSE_VERIFY=1`: also paint each frame from scratch
/// and report differing bytes.
#[cfg(feature = "skia")]
fn paint_layer_reuse_verify() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("W3COS_PAINT_LAYER_REUSE_VERIFY").is_ok_and(|value| value == "1")
    })
}

#[cfg(test)]
pub(crate) fn retained_paint_reuses() -> u32 {
    RETAINED_REUSES.with(Cell::get)
}

#[cfg(test)]
fn note_retained_reuse() {
    RETAINED_REUSES.with(|count| count.set(count.get().saturating_add(1)));
}

#[cfg(not(test))]
fn note_retained_reuse() {}

/// Render the current live DOM through the same component, layout and Skia
/// stages used by native windows, without creating an operating-system window.
pub fn render_document_rgba(width: u32, height: u32) -> Result<HeadlessFrame> {
    render_document_rgba_inner(width, height, false)
}

/// Rasterize and checksum the pixmap in place.
///
/// The CPU renderer keeps one viewport buffer and does not copy it into a
/// second `Vec` when the caller only needs the checksum.
pub(crate) fn render_document_checksum(width: u32, height: u32) -> Result<u64> {
    CHECKSUM_ONLY.with(|cell| cell.set(true));
    let rendered = render_document_rgba(width, height);
    CHECKSUM_ONLY.with(|cell| cell.set(false));
    let frame = rendered?;
    if frame.rgba.is_empty() {
        Ok(LAST_FRAME_CHECKSUM.with(Cell::get))
    } else {
        Ok(frame_byte_checksum(&frame.rgba))
    }
}

/// Rasterize with tiny-skia even when Skia is also compiled in.
#[cfg(feature = "cpu-render")]
pub fn render_document_rgba_cpu(width: u32, height: u32) -> Result<HeadlessFrame> {
    render_document_rgba_inner(width, height, true)
}

fn render_document_rgba_inner(width: u32, height: u32, force_cpu: bool) -> Result<HeadlessFrame> {
    if width == 0 || height == 0 {
        bail!("headless viewport dimensions must be positive");
    }

    crate::incremental_layout::with_paint_layout_flat(
        width as f32,
        height as f32,
        |root, layout_results, known_flat| {
            finish_headless_frame(width, height, force_cpu, root, layout_results, known_flat)
        },
    )
}

fn finish_headless_frame(
    width: u32,
    height: u32,
    force_cpu: bool,
    root: &w3cos_std::Component,
    layout_results: &layout::LayoutResults,
    known_flat: Option<&[layout::FlatNodeInfo<'_>]>,
) -> Result<HeadlessFrame> {
    if crate::webgl_draw::has_live_contexts() {
        let flat = layout::pre_flatten(root);
        crate::webgl_draw::publish_drawing_buffers(flat.iter().enumerate().filter_map(
            |(index, node)| match &node.on_click {
                w3cos_std::EventAction::NativeHost { id, .. } => {
                    u32::try_from(*id).ok().map(|id| (index, id))
                }
                _ => None,
            },
        ));
    }
    let structure = crate::dom::with_document(|document| document.structure_generation());
    let reuse = crate::incremental_layout::take_paint_reuse();
    if reuse != crate::incremental_layout::PaintReuse::None
        && layout_results.text_fragments.is_empty()
        && let Some(frame) = reuse_retained_paint(
            structure,
            reuse,
            width,
            height,
            force_cpu,
            root,
            layout_results,
        )?
    {
        return Ok(frame);
    }
    // A paint splice already flattened this tree (`W3COS_SHARED_PAINT_FLAT`).
    let flattened;
    let flat: &[layout::FlatNodeInfo<'_>] = match known_flat {
        Some(known) => {
            if crate::incremental_layout::shared_paint_flat_mode() == 2 {
                let fresh = layout::pre_flatten(root);
                if !layout::same_flat(known, &fresh) {
                    SHARED_FLAT_MISMATCHES.with(|count| count.set(count.get() + 1));
                    eprintln!(
                        "[shared-paint-flat-verify] mismatch: the splice's flat tree differs"
                    );
                }
            }
            SHARED_FLAT_USES.with(|count| count.set(count.get() + 1));
            known
        }
        None => {
            flattened = layout::pre_flatten(root);
            &flattened
        }
    };
    let layout_cache = &layout_results.layout_cache;
    if std::env::var_os("W3COS_DUMP_HEADLESS_LAYOUT").is_some() {
        for (rect, index) in layout_cache {
            if let Some(node) = flat.get(*index) {
                eprintln!(
                    "W3COS_HEADLESS_LAYOUT index={index} parent={:?} display={:?} position={:?} left={:?} top={:?} float={:?} kind={:?} width={:?} height={:?} margin={:?} padding={:?} align_self={:?} align_items={:?} text_align={:?} justify_content={:?} rect={rect:?} font_family={:?} font_size={} word_spacing={} white_space={:?} line_height={} normal={} direction={:?} rendered_text={:?} font_metric={:?}",
                    node.parent,
                    node.style.display,
                    node.style.position,
                    node.style.left,
                    node.style.top,
                    node.style.float,
                    node.kind,
                    node.style.width,
                    node.style.height,
                    node.style.margin,
                    node.style.padding,
                    node.style.align_self,
                    node.style.align_items,
                    node.style.text_align,
                    node.style.justify_content,
                    node.style.font_family,
                    node.style.font_size,
                    node.style.word_spacing,
                    node.style.white_space,
                    node.style.line_height,
                    node.style.line_height_is_normal,
                    node.style.direction,
                    match node.kind {
                        w3cos_std::ComponentKind::Text { content } => Some(
                            crate::text_layout::font_render_text_for_style(content, node.style)
                        ),
                        _ => None,
                    },
                    crate::font_face::FontRegistry::global().normal_line_height(&node.style),
                );
            }
        }
    }
    let body_index = flat.iter().position(|node| {
        matches!(
            node.on_click,
            w3cos_std::EventAction::NativeHost { id, .. }
                if *id == u64::from(crate::dom::body_id())
        )
    });
    let viewport = Some((width as f32, height as f32));
    // What the layout splice of this frame touched (if it replayed one).
    #[cfg_attr(not(feature = "skia"), allow(unused_variables))]
    let changes = crate::incremental_layout::take_paint_changes();
    let mut frame_changed: Option<Vec<(u32, u32)>> = None;
    let touched_on = crate::paint_artifact::paint_touched_mode() != 0;
    if touched_on {
        // Paint-only UA boxes come after every layout box; a set that does
        // not cover the layout boxes is not used.
        // Only a set for this frame's paint layout, made on top of the one
        // the retained artifact shows, describes what changed since it.
        let now = crate::incremental_layout::paint_layout_generation();
        let base = RETAINED_PAINT.with(|slot| {
            slot.borrow()
                .as_ref()
                .map(|retained| retained.layout_generation)
        });
        let touched = changes
            .as_ref()
            .filter(|changes| changes.generation == now && base.map(|base| base + 1) == Some(now))
            .filter(|changes| changes.len == flat.len())
            .cloned();
        frame_changed = touched.as_ref().map(|touched| touched.changed.clone());
        crate::paint_artifact::set_paint_touched(touched);
    }
    let frame_len = flat.len();
    let keep_mode = paint_kind_keep_mode();
    let mut recycled = RECYCLED_KINDS.with(|kinds| std::mem::take(&mut *kinds.borrow_mut()));
    let inplace = crate::paint_artifact::persist_inplace_mode() != 0;
    let splice_mode = paint_nodes_splice_mode();
    let (mut recycled_nodes, stale_trimmed) = if inplace {
        RECYCLED_NODES.with(|slot| std::mem::take(&mut *slot.borrow_mut()))
    } else {
        (Vec::new(), None)
    };
    let mut recycled_vec = if splice_mode != 0 {
        RECYCLED_NODE_VEC.with(|slot| std::mem::take(&mut *slot.borrow_mut()))
    } else {
        Vec::new()
    };
    let recycled_len = if splice_mode != 0 {
        recycled_vec.len()
    } else if inplace {
        recycled_nodes.len()
    } else {
        recycled.len()
    };
    let previous_changed = RECYCLED_CHANGED.with(|slot| slot.borrow_mut().take());
    // Boxes either of the last two splices touched; the rest keep their kind.
    let dirty = (keep_mode != 0 && recycled_len == frame_len)
        .then_some(())
        .and_then(|()| {
            let (len, previous) = previous_changed.as_ref()?;
            let now = frame_changed.as_ref()?;
            (*len == frame_len).then(|| {
                let mut dirty = previous.clone();
                dirty.extend_from_slice(now);
                crate::incremental_layout::normalize_ranges(&mut dirty);
                dirty
            })
        });
    let mut cursor = 0;
    let (mut kept, mut copied, mut mismatched) = (0u64, 0u64, 0u64);
    let moved = std::cell::RefCell::new(if inplace {
        vec![false; frame_len]
    } else {
        Vec::new()
    });
    // `W3COS_PAINT_NODES_SPLICE`: with the dirty ranges known and the box
    // count unchanged, update the kept array over those ranges only.
    let spliced: Option<Vec<PaintNode>> = match dirty.as_deref() {
        Some(ranges) if splice_mode != 0 && recycled_vec.len() == frame_len => {
            let mut nodes = std::mem::take(&mut recycled_vec);
            let mut rebuilt = 0u64;
            {
                let mut moved = moved.borrow_mut();
                moved.fill(true);
                for (slot, node) in nodes.iter_mut().zip(flat) {
                    slot.parent = node.parent;
                    slot.sticky_counter_signal = node.sticky_counter_signal;
                }
                for &(start, end) in ranges {
                    for index in start as usize..(end as usize).min(frame_len) {
                        let node = &flat[index];
                        moved[index] = false;
                        rebuilt += 1;
                        let slot = &mut nodes[index];
                        let kind =
                            crate::paint_artifact::refill_kind(Some(&mut slot.kind), node.kind);
                        *slot = PaintNode {
                            kind,
                            style: crate::paint_artifact::shared_paint_style(
                                node.component,
                                node.style,
                            ),
                            parent: node.parent,
                            sticky_counter_signal: node.sticky_counter_signal,
                        };
                    }
                }
            }
            let mut mismatched = 0u64;
            if splice_mode == 2 {
                // The per-box rule (`keep` below): outside every range.
                let moved = moved.borrow();
                let mut at = 0usize;
                for index in 0..frame_len {
                    while ranges
                        .get(at)
                        .is_some_and(|&(_, end)| end as usize <= index)
                    {
                        at += 1;
                    }
                    let keep = ranges
                        .get(at)
                        .is_none_or(|&(start, _)| index < start as usize);
                    if keep != moved[index] {
                        mismatched += 1;
                        eprintln!(
                            "[paint-nodes-splice-verify] mismatch: box {index} keep {keep} spliced {}",
                            moved[index]
                        );
                    }
                }
            }
            if splice_mode == 2 {
                eprintln!(
                    "[paint-nodes-splice-verify] rebuilt {rebuilt} of {frame_len}, {mismatched} mismatched"
                );
            }
            kept += frame_len as u64 - rebuilt;
            copied += rebuilt;
            PAINT_NODES_SPLICE_STATS.with(|stats| {
                let (f, r, m) = stats.get();
                stats.set((f + 1, r + rebuilt, m + mismatched));
            });
            Some(nodes)
        }
        _ => {
            if splice_mode != 0 && !recycled_vec.is_empty() {
                recycled_nodes = std::mem::take(&mut recycled_vec)
                    .into_iter()
                    .map(Some)
                    .collect();
            }
            None
        }
    };
    let paint_nodes = flat.iter().enumerate().map(|(index, node)| {
        if inplace {
            let keep = dirty.as_deref().is_some_and(|dirty| {
                while dirty
                    .get(cursor)
                    .is_some_and(|&(_, end)| end as usize <= index)
                {
                    cursor += 1;
                }
                dirty
                    .get(cursor)
                    .is_none_or(|&(start, _)| index < start as usize)
            });
            let slot = recycled_nodes.get_mut(index);
            if keep
                && let Some(slot) = slot
                && let Some(mut recycled) = slot.take()
            {
                kept += 1;
                moved.borrow_mut()[index] = true;
                recycled.parent = node.parent;
                recycled.sticky_counter_signal = node.sticky_counter_signal;
                return recycled;
            }
            copied += 1;
            let kind = crate::paint_artifact::refill_kind(
                recycled_nodes
                    .get_mut(index)
                    .and_then(Option::as_mut)
                    .map(|recycled| &mut recycled.kind),
                node.kind,
            );
            return PaintNode {
                kind,
                style: crate::paint_artifact::shared_paint_style(node.component, node.style),
                parent: node.parent,
                sticky_counter_signal: node.sticky_counter_signal,
            };
        }
        let slot = recycled.get_mut(index);
        let keep = dirty.as_deref().is_some_and(|dirty| {
            while dirty
                .get(cursor)
                .is_some_and(|&(_, end)| end as usize <= index)
            {
                cursor += 1;
            }
            dirty
                .get(cursor)
                .is_none_or(|&(start, _)| index < start as usize)
        });
        let kind = match slot {
            Some(slot) if keep && (keep_mode == 1 || *slot == *node.kind) => {
                kept += 1;
                std::mem::replace(slot, w3cos_std::ComponentKind::Root)
            }
            slot => {
                if keep {
                    mismatched += 1;
                    eprintln!("[paint-kind-keep-verify] mismatch: box {index} kept a stale kind");
                }
                copied += 1;
                crate::paint_artifact::refill_kind(slot, node.kind)
            }
        };
        PaintNode {
            kind,
            style: crate::paint_artifact::shared_paint_style(node.component, node.style),
            parent: node.parent,
            sticky_counter_signal: node.sticky_counter_signal,
        }
    });
    let paint_nodes: Vec<PaintNode> = match spliced {
        Some(nodes) => nodes,
        None => paint_nodes.collect(),
    };
    let cull_mode = crate::paint_artifact::paint_cull_rect_mode();
    if cull_mode != 0 {
        crate::paint_artifact::PAINT_CULL_BAND.with(|band| {
            band.set(Some(headless_cull_band(height as f32)));
        });
    }
    let mut artifact = if crate::paint_artifact::paint_append_reuse_enabled() {
        RETAINED_PAINT.with(|slot| {
            let retained = slot.borrow();
            let raw = |index: usize| {
                let node = &flat[index];
                PaintNode {
                    kind: node.kind.clone(),
                    style: crate::paint_artifact::shared_paint_style(node.component, node.style),
                    parent: node.parent,
                    sticky_counter_signal: node.sticky_counter_signal,
                }
            };
            PaintArtifact::build_reusing_with(
                paint_nodes,
                &layout_cache,
                1,
                body_index,
                viewport,
                retained.as_ref().map(|retained| &retained.artifact),
                inplace.then(|| crate::paint_artifact::PersistRaw {
                    moved: &moved,
                    raw: &raw,
                    stale_trimmed: stale_trimmed.clone(),
                }),
            )
        })
    } else {
        PaintArtifact::build_with_body_background_and_viewport(
            paint_nodes,
            &layout_cache,
            1,
            body_index,
            viewport,
        )
    };
    if touched_on {
        crate::paint_artifact::set_paint_touched(None);
    }
    if keep_mode != 0 {
        PAINT_KIND_KEEP_STATS.with(|stats| {
            let (k, c, m) = stats.get();
            stats.set((k + kept, c + copied, m + mismatched));
        });
    }
    if cull_mode != 0 {
        crate::paint_artifact::PAINT_CULL_BAND.with(|band| band.set(None));
    }
    artifact.set_text_fragments(&layout_results.text_fragments);
    artifact.viewport_scroll = crate::jsdom::window_scroll_offset();
    if cull_mode == 2 {
        verify_paint_cull(&artifact, width, height);
    }
    let skip_culled = culled_paint_nodes(&artifact);
    let nodes = artifact
        .rect_by_index
        .iter()
        .enumerate()
        .filter_map(|(index, rect)| {
            let rect = (*rect)?;
            if skip_culled.as_ref().is_some_and(|skip| skip[index]) {
                return None;
            }
            let node = artifact.nodes.get(index)?;
            Some((index, rect, &node.kind, node.style.as_style()))
        })
        .collect::<Vec<(usize, LayoutRect, _, _)>>();
    let nodes = crate::paint_artifact::sort_by_paint_order(&artifact, nodes, |node| node.0);

    let table_replay = crate::table_paint::replay(&nodes, &artifact);
    let nodes = table_replay
        .iter()
        .map(|node| {
            (
                node.index,
                node.rect,
                node.kind.as_ref(),
                node.style.as_ref(),
            )
        })
        .collect::<Vec<_>>();
    let bidi_replay = crate::bidi_paint::replay(&nodes, &artifact);
    let nodes = bidi_replay
        .iter()
        .map(|node| {
            (
                node.index,
                node.rect,
                node.kind.as_ref(),
                node.style.as_ref(),
            )
        })
        .collect::<Vec<_>>();
    if skip_culled.is_some() && skip_culled_paint_nodes_mode() == 2 {
        verify_skip_culled_paint_nodes(&artifact, &nodes);
    }
    if std::env::var_os("W3COS_DUMP_HEADLESS_LAYOUT").is_some() {
        for (index, rect, kind, style) in &nodes {
            eprintln!(
                "W3COS_HEADLESS_REPLAY index={index} rect={rect:?} kind={kind:?} color={:?} background={:?} opacity={} properties={:?} text_decoration={:?} applied_decorations={:?}",
                style.color,
                style.background,
                style.opacity,
                style.custom_properties,
                style.text_decoration,
                artifact
                    .ancestor_text_decorations(*index)
                    .iter()
                    .map(|owner| (
                        owner.style.text_decoration,
                        owner.style.color,
                        owner.style.font_size,
                        owner.style.font_family.as_deref(),
                        owner.baseline_shift
                    ))
                    .collect::<Vec<_>>()
            );
        }
    }
    let scroll_offsets = layout_results
        .scrollable_nodes
        .iter()
        .filter_map(|(index, _, extent)| {
            let w3cos_std::EventAction::NativeHost { id, .. } = flat[*index].on_click else {
                return None;
            };
            u32::try_from(*id).ok().map(|node| {
                let (x, y) = crate::dom::get_scroll_offset(node);
                (*index, clamp_scroll_offset_ng(x, y, extent))
            })
        })
        .collect::<HashMap<_, _>>();
    let scroll_info = crate::window::build_headless_scroll_info(
        &layout_results.scroll_ancestor,
        &layout_results.scrollable_nodes,
        &layout_results.clip_only_nodes,
        &scroll_offsets,
        layout_cache,
        &flat,
        width as f32,
        height as f32,
    );
    // What the layout splice of this frame touched (if it was one splice).
    #[cfg(feature = "skia")]
    if paint_layer_reuse_enabled() {
        // Diff against the artifact the retained recordings were made from.
        // Loaded fonts change glyphs without changing the artifact.
        let same_fonts = HEADLESS_SKIA_FONTS.with(Cell::get)
            == crate::font_face::FontRegistry::global().revision();
        let dirty = (same_fonts && HEADLESS_SKIA_CURRENT.with(Cell::get))
            .then(|| {
                RETAINED_PAINT.with(|slot| {
                    slot.borrow().as_ref().and_then(|retained| {
                        let diff = crate::retained_layers::paint_diff_clients(
                            &retained.artifact,
                            &artifact,
                        )?;
                        if paint_changes_verify() {
                            verify_paint_changes(
                                changes.as_ref(),
                                &diff,
                                &retained.artifact,
                                &artifact,
                            );
                        }
                        // The off-surface skip needs the damage even under a
                        // full composite: only to see it is all off surface.
                        // Skip only: the composite stays full, so the damage
                        // is computed in `replay_frame` only once its first
                        // client is known to lie off the surface.
                        let skip_only = !crate::render_skia::paint_damage_enabled()
                            && crate::render_skia::paint_offscreen_skip_mode() != 0;
                        // The early-out measures the damage where the layers
                        // place it, stopping once most of the frame is damaged.
                        let early_out = crate::render_skia::paint_damage_early_out_active();
                        let damage = (crate::render_skia::paint_damage_enabled() && !early_out)
                            .then(|| {
                                crate::retained_layers::paint_damage(
                                    &retained.artifact,
                                    &artifact,
                                    &diff,
                                )
                            })
                            .flatten();
                        if damage.is_none()
                            && crate::render_skia::paint_damage_enabled()
                            && !early_out
                        {
                            crate::render_skia::note_unbounded_damage();
                        }
                        Some((diff, damage, skip_only || early_out))
                    })
                })
            })
            .flatten();
        let (dirty, moved, damage, lazy) = dirty
            .map_or((None, None, None, false), |(diff, damage, lazy)| {
                (Some(diff.dirty), Some(diff.moved), damage, lazy)
            });
        crate::render_skia::set_pending_paint_invalidation(dirty);
        crate::render_skia::set_pending_paint_moved(moved);
        crate::render_skia::set_pending_paint_damage(damage);
        crate::render_skia::set_pending_lazy_damage(lazy);
    }
    let rgba = rasterize_prepared_frame(width, height, &nodes, &scroll_info, &artifact, force_cpu)?;
    let rect_independent = component_rect_independent(root);
    let replaced = RETAINED_PAINT.with(|slot| {
        slot.borrow_mut().replace(RetainedPaint {
            structure,
            rect_independent,
            artifact,
            layout_generation: crate::incremental_layout::paint_layout_generation(),
            quiet_diff: None,
            quiet_exit: None,
        })
    });
    if let Some(replaced) = replaced {
        if splice_mode != 0 {
            let trimmed = replaced.artifact.persist_trimmed.clone();
            RECYCLED_NODE_VEC.with(|slot| *slot.borrow_mut() = replaced.artifact.nodes);
            RECYCLED_NODES.with(|slot| *slot.borrow_mut() = (Vec::new(), trimmed));
        } else if inplace {
            let trimmed = replaced.artifact.persist_trimmed.clone();
            recycled_nodes.clear();
            recycled_nodes.extend(replaced.artifact.nodes.into_iter().map(Some));
            RECYCLED_NODES.with(|slot| *slot.borrow_mut() = (recycled_nodes, trimmed));
        } else {
            recycled.clear();
            recycled.extend(replaced.artifact.nodes.into_iter().map(|node| node.kind));
            RECYCLED_KINDS.with(|kinds| *kinds.borrow_mut() = recycled);
        }
        if keep_mode != 0
            && let Some(changed) = frame_changed
        {
            RECYCLED_CHANGED.with(|slot| *slot.borrow_mut() = Some((frame_len, changed)));
        }
    }

    Ok(HeadlessFrame {
        width,
        height,
        rgba,
    })
}

/// Persistent paint nodes on this thread so far: (builds on the persistent
/// path, clean boxes taken, verify mismatches). For multi-frame checks.
pub fn persistent_paint_stats() -> (u64, u64, u64) {
    let (frames, clean) = crate::paint_artifact::PERSISTENT_NODES_STATS.with(Cell::get);
    let mismatches = crate::paint_artifact::PERSISTENT_NODES_MISMATCHES.with(Cell::get);
    (frames, clean, mismatches)
}

thread_local! {
    /// (frames painted from a splice's flat tree, verify mismatches).
    static SHARED_FLAT_USES: Cell<u64> = const { Cell::new(0) };
    static SHARED_FLAT_MISMATCHES: Cell<u64> = const { Cell::new(0) };
}

#[cfg(test)]
fn shared_flat_stats() -> (u64, u64) {
    (
        SHARED_FLAT_USES.with(Cell::get),
        SHARED_FLAT_MISMATCHES.with(Cell::get),
    )
}

/// The cull band for this frame: the kept artifact's band while it still
/// covers the viewport with Blink's "changed enough" distance to spare,
/// else the viewport expanded by 4000px vertically.
fn headless_cull_band(height: f32) -> (f32, f32) {
    use crate::paint_artifact::{PAINT_CULL_CHANGED_ENOUGH, PAINT_CULL_EXPAND};
    let scroll_y = crate::jsdom::window_scroll_offset().1;
    let kept = RETAINED_PAINT.with(|slot| {
        slot.borrow()
            .as_ref()
            .and_then(|retained| retained.artifact.cull_band)
    });
    let fresh = (
        scroll_y - PAINT_CULL_EXPAND,
        scroll_y + height + PAINT_CULL_EXPAND,
    );
    match kept {
        Some((top, bottom))
            if top <= scroll_y - (PAINT_CULL_EXPAND - PAINT_CULL_CHANGED_ENOUGH)
                && bottom
                    >= scroll_y + height + (PAINT_CULL_EXPAND - PAINT_CULL_CHANGED_ENOUGH) =>
        {
            (top, bottom)
        }
        _ => fresh,
    }
}

/// `W3COS_PAINT_CULL_RECT=verify`: no box the band dropped may reach the
/// viewport at the scroll this frame paints with.
fn verify_paint_cull(artifact: &PaintArtifact, width: u32, height: u32) {
    let reaching = artifact.culled_in_viewport(width as f32, height as f32);
    if reaching > 0 {
        crate::paint_artifact::PAINT_CULL_DIFFS.with(|count| count.set(count.get() + 1));
        eprintln!(
            "[paint-cull-verify] DIFF {reaching} culled boxes reach the viewport ({} culled)",
            artifact.culled_nodes.len()
        );
    }
}

fn reuse_retained_paint(
    structure: u64,
    reuse: crate::incremental_layout::PaintReuse,
    width: u32,
    height: u32,
    force_cpu: bool,
    root: &w3cos_std::Component,
    layout_results: &layout::LayoutResults,
) -> Result<Option<HeadlessFrame>> {
    RETAINED_PAINT.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(retained) = slot.as_mut() else {
            return Ok(None);
        };
        if retained.structure != structure || !retained.artifact.text_fragments.is_empty() {
            return Ok(None);
        }
        let scroll = crate::jsdom::window_scroll_offset();
        // A kept artifact culled for another scroll position may lack boxes
        // the viewport now shows: rebuild with a band around it.
        if !retained
            .artifact
            .cull_band_covers(scroll.1, height as f32, 0.0)
        {
            return Ok(None);
        }
        let scroll_same = retained.artifact.viewport_scroll == scroll;
        if !scroll_same || reuse != crate::incremental_layout::PaintReuse::Scroll {
            retained.quiet_diff = None;
            retained.quiet_exit = None;
        }
        retained.artifact.viewport_scroll = scroll;
        if crate::paint_artifact::paint_cull_rect_mode() == 2 {
            verify_paint_cull(&retained.artifact, width, height);
        }
        retained.layout_generation = crate::incremental_layout::paint_layout_generation();
        if reuse == crate::incremental_layout::PaintReuse::Insets {
            if !retained.rect_independent {
                return Ok(None);
            }
            if !assign_retained_rects(
                &mut retained.artifact.rect_by_index,
                &layout_results.layout_cache,
            ) {
                return Ok(None);
            }
        }
        // `W3COS_PAINT_QUIET_EXIT`: the last frame was quiet with no dirty or
        // moved client, and nothing it showed changed since: return its
        // pixels without flattening, painting, syncing or compositing (cc
        // draws no frame without damage).
        #[cfg(feature = "skia")]
        let exit_mode = quiet_exit_mode();
        #[cfg(feature = "skia")]
        let exit_rgba = (exit_mode != 0
            && quiet_frame_mode() != 0
            && reuse == crate::incremental_layout::PaintReuse::Scroll
            && scroll_same
            && !force_cpu)
            .then(|| quiet_exit_frame(retained, width, height))
            .flatten();
        #[cfg(feature = "skia")]
        if let Some(rgba) = exit_rgba.as_ref()
            && exit_mode == 1
        {
            QUIET_EXIT_STATS.with(|stats| {
                let mut value = stats.get();
                value.0 += 1;
                stats.set(value);
            });
            note_retained_reuse();
            return Ok(Some(HeadlessFrame {
                width,
                height,
                rgba: rgba.clone(),
            }));
        }
        let flat = layout::pre_flatten(root);
        // `W3COS_PAINT_QUIET_FRAME`: nothing was invalidated since the kept
        // recordings were made, so the paint diff is taken against the same
        // artifact (only volatile clients repaint) and the damage path can
        // skip raster and composite when that damage is empty.
        #[cfg(feature = "skia")]
        let quiet_mode = quiet_frame_mode();
        #[cfg(feature = "skia")]
        let quiet = quiet_mode != 0
            && reuse == crate::incremental_layout::PaintReuse::Scroll
            && scroll_same
            && !force_cpu
            && quiet_frame_invalidation(&retained.artifact, &mut retained.quiet_diff, quiet_mode);
        #[cfg(not(feature = "skia"))]
        let _ = scroll_same;
        let frame = paint_retained_artifact(
            width,
            height,
            force_cpu,
            &flat,
            layout_results,
            &retained.artifact,
        )?;
        // A sync that did not run must not carry the hint to a later frame.
        #[cfg(feature = "skia")]
        crate::retained_layers::set_quiet_content(0);
        #[cfg(feature = "skia")]
        if quiet {
            let skipped = crate::render_skia::paint_offscreen_skip_stats();
            QUIET_FRAME_STATS.with(|stats| {
                let mut value = stats.get();
                value.0 += 1;
                stats.set(value);
            });
            if quiet_mode == 2 {
                // A frame painted without the diff re-records every layer
                // and composites in full.
                crate::render_skia::set_pending_paint_invalidation(None);
                crate::retained_layers::set_quiet_content(0);
                let full = paint_retained_artifact(
                    width,
                    height,
                    force_cpu,
                    &flat,
                    layout_results,
                    &retained.artifact,
                )?;
                let differing = frame
                    .rgba
                    .chunks_exact(4)
                    .zip(full.rgba.chunks_exact(4))
                    .filter(|(a, b)| a != b)
                    .count()
                    + frame.rgba.len().abs_diff(full.rgba.len());
                let kept = crate::render_skia::paint_offscreen_skip_stats().0 > skipped.0;
                if differing == 0 {
                    eprintln!("[paint-quiet-frame-verify] same (raster skipped: {kept})");
                } else {
                    QUIET_FRAME_STATS.with(|stats| {
                        let mut value = stats.get();
                        value.1 += 1;
                        stats.set(value);
                    });
                    eprintln!("[paint-quiet-frame-verify] mismatch: DIFF {differing} pixels");
                    note_retained_reuse();
                    return Ok(Some(full));
                }
            }
        }
        #[cfg(feature = "skia")]
        {
            if let Some(rgba) = exit_rgba {
                QUIET_EXIT_STATS.with(|stats| {
                    let mut value = stats.get();
                    value.0 += 1;
                    if rgba != frame.rgba {
                        value.1 += 1;
                    }
                    stats.set(value);
                });
                if rgba == frame.rgba {
                    eprintln!("[paint-quiet-exit-verify] same");
                } else {
                    eprintln!("[paint-quiet-exit-verify] mismatch against the painted frame");
                }
            }
            let clean = quiet
                && retained.quiet_diff.as_ref().is_some_and(|(dirty, moved)| {
                    !dirty.iter().any(|client| *client) && moved.iter().all(Option::is_none)
                });
            retained.quiet_exit = (exit_mode != 0 && clean)
                .then(|| quiet_scroll_state(&flat, layout_results))
                .flatten()
                .map(|scrolls| ((width, height), scrolls));
        }
        note_retained_reuse();
        Ok(Some(frame))
    })
}

/// `W3COS_PAINT_QUIET_EXIT=1|verify` (off by default, needs
/// `W3COS_PAINT_QUIET_FRAME`): a frame after a quiet frame that had no
/// dirty or moved client, at the same size, window scroll and nested
/// scroll offsets, returns the kept rasterizer's last pixels without
/// flattening, painting, layer sync or composite. `verify` paints the
/// frame anyway and compares every pixel.
#[cfg(feature = "skia")]
fn quiet_exit_mode() -> u8 {
    #[cfg(test)]
    if let Some(mode) = QUIET_EXIT_OVERRIDE.with(Cell::get) {
        return mode;
    }
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(
        || match std::env::var("W3COS_PAINT_QUIET_EXIT").as_deref() {
            Ok("1") => 1,
            Ok("verify") => 2,
            _ => 0,
        },
    )
}

#[cfg(feature = "skia")]
thread_local! {
    #[cfg(test)]
    static QUIET_EXIT_OVERRIDE: Cell<Option<u8>> = const { Cell::new(None) };
    /// (frames that took the exit or were verified, verify mismatches).
    static QUIET_EXIT_STATS: Cell<(u64, u64)> = const { Cell::new((0, 0)) };
}

/// Quiet exits on this thread so far: (frames, verify mismatches).
#[cfg(feature = "skia")]
pub fn quiet_exit_stats() -> (u64, u64) {
    QUIET_EXIT_STATS.with(Cell::get)
}

/// The nested scrollers' node ids and raw offsets, as `paint_retained_artifact`
/// reads them; `None` if one is not a DOM node.
#[cfg(feature = "skia")]
fn quiet_scroll_state(
    flat: &[layout::FlatNodeInfo<'_>],
    layout_results: &layout::LayoutResults,
) -> Option<Vec<(u32, (f32, f32))>> {
    layout_results
        .scrollable_nodes
        .iter()
        .map(|(index, _, _)| {
            let w3cos_std::EventAction::NativeHost { id, .. } = flat.get(*index)?.on_click else {
                return None;
            };
            let node = u32::try_from(*id).ok()?;
            Some((node, crate::dom::get_scroll_offset(node)))
        })
        .collect()
}

/// The last frame's pixels when the kept quiet-exit state still holds.
#[cfg(feature = "skia")]
fn quiet_exit_frame(retained: &RetainedPaint, width: u32, height: u32) -> Option<Vec<u8>> {
    let (size, scrolls) = retained.quiet_exit.as_ref()?;
    if *size != (width, height)
        || !paint_layer_reuse_enabled()
        || !HEADLESS_SKIA_CURRENT.with(Cell::get)
        || HEADLESS_SKIA_FONTS.with(Cell::get)
            != crate::font_face::FontRegistry::global().revision()
        || scrolls
            .iter()
            .any(|(node, offset)| crate::dom::get_scroll_offset(*node) != *offset)
    {
        return None;
    }
    HEADLESS_SKIA.with(|slot| {
        slot.borrow()
            .as_ref()
            .and_then(|rasterizer| rasterizer.last_rgba(width, height))
            .map(<[u8]>::to_vec)
    })
}

/// `W3COS_PAINT_QUIET_FRAME=1|verify` (off by default): a headless frame
/// with no layout or style change since the last one (the paint cache is
/// reused with no dirty node, the scroll position is the painted one, the
/// kept rasterizer painted that artifact with the same fonts) hands the
/// rasterizer a paint diff of the artifact against itself instead of none.
/// Only volatile clients (decoded images, canvases, SVG documents, form
/// values) then count as changed, the kept layer recordings stay, and with
/// no damage on the surface the off-surface skip (C94) returns the last
/// frame's pixels without raster or composite (Blink: no invalidation, no
/// paint, no raster, and cc draws nothing for a frame without damage).
/// `verify` also paints each such frame in full and compares every pixel.
#[cfg(feature = "skia")]
fn quiet_frame_mode() -> u8 {
    #[cfg(test)]
    if let Some(mode) = QUIET_FRAME_OVERRIDE.with(Cell::get) {
        return mode;
    }
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(
        || match std::env::var("W3COS_PAINT_QUIET_FRAME").ok().as_deref() {
            Some("1") => 1,
            Some("verify") => 2,
            _ => 0,
        },
    )
}

#[cfg(all(test, feature = "skia"))]
thread_local! {
    static QUIET_FRAME_OVERRIDE: Cell<Option<u8>> = const { Cell::new(None) };
}

#[cfg(feature = "skia")]
thread_local! {
    /// (quiet frames, verify mismatches).
    static QUIET_FRAME_STATS: Cell<(u64, u64)> = const { Cell::new((0, 0)) };
}

/// Quiet frames on this thread so far: (frames, verify mismatches).
#[cfg(feature = "skia")]
pub fn quiet_frame_stats() -> (u64, u64) {
    QUIET_FRAME_STATS.with(Cell::get)
}

/// Set the pending paint invalidation of a quiet frame: `artifact` diffed
/// against itself. False (nothing set) when the kept recordings may not be
/// current or the damage cannot be bounded.
#[cfg(feature = "skia")]
fn quiet_frame_invalidation(
    artifact: &PaintArtifact,
    kept: &mut Option<(Vec<bool>, Vec<Option<(f32, f32)>>)>,
    mode: u8,
) -> bool {
    if !paint_layer_reuse_enabled()
        || !HEADLESS_SKIA_CURRENT.with(Cell::get)
        || HEADLESS_SKIA_FONTS.with(Cell::get)
            != crate::font_face::FontRegistry::global().revision()
    {
        return false;
    }
    // The diff of an unchanged artifact against itself depends only on the
    // artifact, so a run of quiet frames reuses the first one's.
    let diff = match kept.as_ref() {
        Some((dirty, moved)) => {
            let diff = crate::retained_layers::PaintDiff {
                dirty: dirty.clone(),
                moved: moved.clone(),
            };
            if mode == 2 {
                let fresh = crate::retained_layers::paint_diff_clients(artifact, artifact);
                if fresh
                    .as_ref()
                    .is_none_or(|fresh| fresh.dirty != diff.dirty || fresh.moved != diff.moved)
                {
                    eprintln!("[paint-quiet-frame-verify] mismatch: kept self-diff differs");
                    QUIET_FRAME_STATS.with(|stats| {
                        let mut value = stats.get();
                        value.1 += 1;
                        stats.set(value);
                    });
                    *kept = None;
                    return false;
                }
            }
            diff
        }
        None => {
            let Some(diff) = crate::retained_layers::paint_diff_clients(artifact, artifact) else {
                return false;
            };
            *kept = Some((diff.dirty.clone(), diff.moved.clone()));
            diff
        }
    };
    let skip_only = !crate::render_skia::paint_damage_enabled()
        && crate::render_skia::paint_offscreen_skip_mode() != 0;
    let early_out = crate::render_skia::paint_damage_early_out_active();
    let damage = (crate::render_skia::paint_damage_enabled() && !early_out)
        .then(|| crate::retained_layers::paint_damage(artifact, artifact, &diff))
        .flatten();
    if damage.is_none() && crate::render_skia::paint_damage_enabled() && !early_out {
        return false;
    }
    crate::render_skia::set_pending_paint_invalidation(Some(diff.dirty));
    crate::render_skia::set_pending_paint_moved(Some(diff.moved));
    crate::render_skia::set_pending_paint_damage(damage);
    crate::render_skia::set_pending_lazy_damage(skip_only || early_out);
    crate::retained_layers::set_quiet_content(mode);
    true
}

fn assign_retained_rects(
    rects: &mut [Option<LayoutRect>],
    layouts: &[(LayoutRect, usize)],
) -> bool {
    for &(rect, index) in layouts {
        let Some(slot) = rects.get_mut(index) else {
            return false;
        };
        *slot = Some(rect);
    }
    true
}

fn component_rect_independent(component: &w3cos_std::Component) -> bool {
    fn style_ok(style: &w3cos_std::style::Style) -> bool {
        use w3cos_std::style::{Dimension, Display, Float, Position, Spacing};
        let padding_percent = [
            style.padding.top,
            style.padding.right,
            style.padding.bottom,
            style.padding.left,
        ]
        .into_iter()
        .any(|edge| matches!(edge, Spacing::Percent(_) | Spacing::PaddingCalc { .. }));
        let background_image = style.background_image.as_deref().is_some_and(|value| {
            value.split(',').any(|layer| {
                let layer = layer.trim();
                !layer.is_empty() && !layer.eq_ignore_ascii_case("none")
            })
        });
        style.float == Float::None
            && !matches!(
                style.display,
                Display::Table
                    | Display::InlineTable
                    | Display::TableRow
                    | Display::TableRowGroup
                    | Display::TableHeaderGroup
                    | Display::TableFooterGroup
                    | Display::TableColumn
                    | Display::TableColumnGroup
                    | Display::TableCell
                    | Display::TableCaption
            )
            && style.column_count.is_none()
            && matches!(style.column_width, Dimension::Auto)
            && style.position != Position::Sticky
            && style.filter.is_none()
            && style.transform.is_identity()
            && style.backdrop_blur.is_none()
            && style.clip.is_none()
            && !background_image
            && !padding_percent
    }
    fn walk(component: &w3cos_std::Component) -> bool {
        style_ok(&component.style) && component.children.iter().all(walk)
    }
    walk(component)
}

/// `W3COS_PAINT_SKIP_CULLED_NODES` (`=verify` checks it): leave out of
/// the paint-order sort and the table and bidi replays the boxes the paint
/// cull band dropped (outside the band, no display items), which no layer
/// draws (Blink does not walk a box outside the cull rect). Boxes under
/// `border-collapse` stay: a collapsed table paints its cells as one group.
/// A bidi run's boxes share one line box, so the band drops all or none of
/// them. `=verify` also replays every box and compares what reaches the
/// layers (every box with display items, in order). On by default (`=0` turns it off).
fn skip_culled_paint_nodes_mode() -> u8 {
    #[cfg(test)]
    if let Some(mode) = SKIP_CULLED_PAINT_NODES_OVERRIDE.with(std::cell::Cell::get) {
        return mode;
    }
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| {
        match std::env::var("W3COS_PAINT_SKIP_CULLED_NODES")
            .ok()
            .as_deref()
        {
            Some("0" | "false") => 0,
            Some("verify") => 2,
            _ => 1,
        }
    })
}

#[cfg(test)]
thread_local! {
    pub(crate) static SKIP_CULLED_PAINT_NODES_OVERRIDE: std::cell::Cell<Option<u8>> =
        const { std::cell::Cell::new(None) };
}

thread_local! {
    /// (boxes left out, verify mismatches) since the thread started.
    pub(crate) static SKIP_CULLED_PAINT_NODES_STATS: std::cell::Cell<(u64, u64)> =
        const { std::cell::Cell::new((0, 0)) };
}

/// Per box, whether display items of `artifact` name it.
fn painted_clients(artifact: &PaintArtifact) -> Vec<bool> {
    let mut painted = vec![false; artifact.nodes.len()];
    for item in &artifact.display_items {
        if let Some(slot) = painted.get_mut(item.client_index) {
            *slot = true;
        }
    }
    painted
}

/// The boxes [`skip_culled_paint_nodes_mode`] leaves out, when it is on and
/// the artifact has a cull band.
fn culled_paint_nodes(artifact: &PaintArtifact) -> Option<Vec<bool>> {
    if skip_culled_paint_nodes_mode() == 0 {
        return None;
    }
    let (top, bottom) = artifact.cull_band?;
    let painted = painted_clients(artifact);
    let mut skipped = 0u64;
    let skip = artifact
        .rect_by_index
        .iter()
        .enumerate()
        .map(|(index, rect)| {
            let skip = rect.is_some_and(|rect| rect.y + rect.height < top || rect.y > bottom)
                && !painted[index]
                && artifact
                    .nodes
                    .get(index)
                    .is_some_and(|node| !node.style.border_collapse);
            skipped += u64::from(skip);
            skip
        })
        .collect();
    SKIP_CULLED_PAINT_NODES_STATS.with(|stats| {
        let (count, mismatches) = stats.get();
        stats.set((count + skipped, mismatches));
    });
    Some(skip)
}

/// Replay every box as without the skip and compare the boxes with display
/// items (those layers draw), in order, with `kept`.
fn verify_skip_culled_paint_nodes(
    artifact: &PaintArtifact,
    kept: &[(
        usize,
        LayoutRect,
        &w3cos_std::component::ComponentKind,
        &w3cos_std::style::Style,
    )],
) {
    let all = artifact
        .rect_by_index
        .iter()
        .enumerate()
        .filter_map(|(index, rect)| {
            let rect = (*rect)?;
            let node = artifact.nodes.get(index)?;
            Some((index, rect, &node.kind, node.style.as_style()))
        })
        .collect::<Vec<(usize, LayoutRect, _, _)>>();
    let all = crate::paint_artifact::sort_by_paint_order(artifact, all, |node| node.0);
    let table = crate::table_paint::replay(&all, artifact);
    let all = table
        .iter()
        .map(|node| {
            (
                node.index,
                node.rect,
                node.kind.as_ref(),
                node.style.as_ref(),
            )
        })
        .collect::<Vec<_>>();
    let bidi = crate::bidi_paint::replay(&all, artifact);
    let painted = painted_clients(artifact);
    let drawn = |nodes: &mut dyn Iterator<
        Item = (
            usize,
            LayoutRect,
            &w3cos_std::component::ComponentKind,
            &w3cos_std::style::Style,
        ),
    >| {
        nodes
            .filter(|node| painted.get(node.0).copied().unwrap_or(true))
            .map(|(index, rect, kind, style)| {
                (
                    index,
                    [rect.x, rect.y, rect.width, rect.height].map(f32::to_bits),
                    kind.clone(),
                    style.clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    let full = drawn(&mut bidi.iter().map(|node| {
        (
            node.index,
            node.rect,
            node.kind.as_ref(),
            node.style.as_ref(),
        )
    }));
    let skipped = drawn(&mut kept.iter().copied());
    if full != skipped {
        eprintln!(
            "[skip-culled-paint-nodes-verify] mismatch: {} boxes reach the layers, {} without the skip",
            skipped.len(),
            full.len()
        );
        SKIP_CULLED_PAINT_NODES_STATS.with(|stats| {
            let (count, mismatches) = stats.get();
            stats.set((count, mismatches + 1));
        });
    }
}

fn paint_retained_artifact(
    width: u32,
    height: u32,
    force_cpu: bool,
    flat: &[layout::FlatNodeInfo<'_>],
    layout_results: &layout::LayoutResults,
    artifact: &PaintArtifact,
) -> Result<HeadlessFrame> {
    let skip_culled = culled_paint_nodes(&artifact);
    let nodes = artifact
        .rect_by_index
        .iter()
        .enumerate()
        .filter_map(|(index, rect)| {
            let rect = (*rect)?;
            if skip_culled.as_ref().is_some_and(|skip| skip[index]) {
                return None;
            }
            let node = artifact.nodes.get(index)?;
            Some((index, rect, &node.kind, node.style.as_style()))
        })
        .collect::<Vec<(usize, LayoutRect, _, _)>>();
    let nodes = crate::paint_artifact::sort_by_paint_order(&artifact, nodes, |node| node.0);
    let table_replay = crate::table_paint::replay(&nodes, artifact);
    let nodes = table_replay
        .iter()
        .map(|node| {
            (
                node.index,
                node.rect,
                node.kind.as_ref(),
                node.style.as_ref(),
            )
        })
        .collect::<Vec<_>>();
    let bidi_replay = crate::bidi_paint::replay(&nodes, artifact);
    let nodes = bidi_replay
        .iter()
        .map(|node| {
            (
                node.index,
                node.rect,
                node.kind.as_ref(),
                node.style.as_ref(),
            )
        })
        .collect::<Vec<_>>();
    if skip_culled.is_some() && skip_culled_paint_nodes_mode() == 2 {
        verify_skip_culled_paint_nodes(artifact, &nodes);
    }
    let scroll_offsets = layout_results
        .scrollable_nodes
        .iter()
        .filter_map(|(index, _, extent)| {
            let w3cos_std::EventAction::NativeHost { id, .. } = flat[*index].on_click else {
                return None;
            };
            u32::try_from(*id).ok().map(|node| {
                let (x, y) = crate::dom::get_scroll_offset(node);
                (*index, clamp_scroll_offset_ng(x, y, extent))
            })
        })
        .collect::<HashMap<_, _>>();
    let scroll_info = crate::window::build_headless_scroll_info(
        &layout_results.scroll_ancestor,
        &layout_results.scrollable_nodes,
        &layout_results.clip_only_nodes,
        &scroll_offsets,
        &layout_results.layout_cache,
        flat,
        width as f32,
        height as f32,
    );
    let rgba = rasterize_prepared_frame(width, height, &nodes, &scroll_info, artifact, force_cpu)?;
    Ok(HeadlessFrame {
        width,
        height,
        rgba,
    })
}

/// Write a headless frame as a PNG. Used by the native-gaps demo.
pub fn write_frame_png(path: &std::path::Path, frame: &HeadlessFrame) -> Result<()> {
    image::save_buffer(
        path,
        &frame.rgba,
        frame.width,
        frame.height,
        image::ExtendedColorType::Rgba8,
    )
    .map_err(|err| anyhow::anyhow!("failed to write {}: {err}", path.display()))
}

#[cfg(feature = "cpu-render")]
fn cpu_pixmap_bytes(
    width: u32,
    height: u32,
    nodes: &[(
        usize,
        LayoutRect,
        &w3cos_std::component::ComponentKind,
        &w3cos_std::style::Style,
    )],
    scroll_info: &[Option<(f32, f32, LayoutRect)>],
) -> Result<Vec<u8>> {
    CPU_PIXMAP.with(|slot| -> Result<()> {
        let mut slot = slot.borrow_mut();
        let same_size = slot
            .as_ref()
            .is_some_and(|pixmap| pixmap.width() == width && pixmap.height() == height);
        if !same_size {
            *slot =
                Some(tiny_skia::Pixmap::new(width, height).ok_or_else(|| {
                    anyhow::anyhow!("viewport is too large to rasterize on the CPU")
                })?);
        }
        let pixmap = slot.as_mut().expect("cpu pixmap");
        let mut clips = crate::render_cpu::ClipMaskCache::default();
        crate::render_cpu::render_frame(
            pixmap,
            nodes,
            layout::layout_font(),
            scroll_info,
            &HashMap::new(),
            None,
            &mut clips,
        );
        LAST_FRAME_CHECKSUM.with(|cell| cell.set(frame_byte_checksum(pixmap.data())));
        Ok(())
    })?;
    if CHECKSUM_ONLY.with(Cell::get) {
        return Ok(Vec::new());
    }
    CPU_PIXMAP.with(|slot| Ok(slot.borrow().as_ref().expect("cpu pixmap").data().to_vec()))
}

fn rasterize_prepared_frame(
    width: u32,
    height: u32,
    nodes: &[(
        usize,
        LayoutRect,
        &w3cos_std::component::ComponentKind,
        &w3cos_std::style::Style,
    )],
    scroll_info: &[Option<(f32, f32, LayoutRect)>],
    artifact: &PaintArtifact,
    force_cpu: bool,
) -> Result<Vec<u8>> {
    if force_cpu {
        #[cfg(feature = "skia")]
        {
            crate::render_skia::set_pending_paint_invalidation(None);
            HEADLESS_SKIA_CURRENT.with(|current| current.set(false));
        }
        #[cfg(feature = "cpu-render")]
        {
            let _ = artifact;
            return cpu_pixmap_bytes(width, height, nodes, scroll_info);
        }
        #[cfg(not(feature = "cpu-render"))]
        {
            let _ = (width, height, nodes, scroll_info, artifact);
            bail!("cpu renderer is not compiled in");
        }
    }
    // Skia software raster is the conformance path. Headless machines without
    // that feature use the tiny-skia CPU renderer instead of opening a GPU
    // window. Prefer Skia when both renderers are compiled in so existing
    // pixel tests keep a single backend.
    #[cfg(feature = "skia")]
    {
        let render = |rasterizer: &mut SkiaRasterizer| {
            rasterizer
                .render_frame(
                    width,
                    height,
                    nodes,
                    layout::layout_font(),
                    scroll_info,
                    &HashMap::new(),
                    None,
                    artifact.canvas_background,
                    Some(artifact),
                    None,
                    1.0,
                )
                .map(|rgba| rgba.to_vec())
                .ok_or_else(|| anyhow::anyhow!("Skia failed to rasterize the headless document"))
        };
        let new_rasterizer = || {
            SkiaRasterizer::new_host()
                .ok_or_else(|| anyhow::anyhow!("host W3COS font is unavailable to Skia"))
        };
        if !paint_layer_reuse_enabled() {
            crate::render_skia::set_pending_paint_invalidation(None);
            crate::render_skia::set_track_partial_layers(false);
            return render(&mut new_rasterizer()?);
        }
        crate::render_skia::set_track_partial_layers(true);
        HEADLESS_SKIA_CURRENT.with(|current| current.set(false));
        // Keep one rasterizer, and with it the retained layer recordings,
        // across headless frames (cc keeps its layers between commits).
        let rgba = HEADLESS_SKIA.with(|slot| -> Result<(Vec<u8>, (usize, usize))> {
            let mut slot = slot.borrow_mut();
            if slot.is_none() {
                *slot = Some(new_rasterizer()?);
            }
            let rasterizer = slot.as_mut().expect("headless rasterizer");
            if !crate::render_skia::has_pending_paint_invalidation() {
                // Without a diff against the recordings' artifact, nothing
                // proves a kept recording (or its images and fonts) current.
                rasterizer.invalidate_recordings();
            }
            let rgba = render(rasterizer);
            crate::render_skia::set_pending_paint_invalidation(None);
            Ok((rgba?, rasterizer.last_reused_layers()))
        })?;
        HEADLESS_SKIA_CURRENT.with(|current| current.set(true));
        HEADLESS_SKIA_REUSED.with(|reused| reused.set(rgba.1));
        HEADLESS_SKIA_FONTS
            .with(|fonts| fonts.set(crate::font_face::FontRegistry::global().revision()));
        if paint_layer_reuse_verify() {
            crate::render_skia::set_pending_paint_invalidation(None);
            crate::render_skia::set_track_partial_layers(false);
            let fresh = render(&mut new_rasterizer()?)?;
            let differing = rgba.0.iter().zip(&fresh).filter(|(a, b)| a != b).count()
                + rgba.0.len().abs_diff(fresh.len());
            eprintln!(
                "[paint-verify] kept {}/{} layers, {} differing bytes",
                rgba.1.0, rgba.1.1, differing
            );
        }
        return Ok(rgba.0);
    }
    #[cfg(all(feature = "cpu-render", not(feature = "skia")))]
    {
        let _ = artifact;
        return cpu_pixmap_bytes(width, height, nodes, scroll_info);
    }
    #[cfg(not(any(feature = "skia", all(feature = "cpu-render", not(feature = "skia")))))]
    {
        let _ = (width, height, nodes, scroll_info, artifact);
        bail!("headless rasterizer is not compiled in");
    }
}

#[cfg(test)]
mod checksum_tests {
    use super::frame_byte_checksum;

    fn naive(rgba: &[u8]) -> u64 {
        let mut checksum = rgba.len() as u64;
        for (index, byte) in rgba.iter().enumerate() {
            checksum =
                checksum.wrapping_add(u64::from(*byte).wrapping_mul((index as u64 % 251) + 1));
        }
        checksum
    }

    #[test]
    fn frame_checksum_matches_the_modulo_formula() {
        for len in [0usize, 1, 250, 251, 252, 503, 4096] {
            let rgba = (0..len)
                .map(|index| (index.wrapping_mul(17)) as u8)
                .collect::<Vec<_>>();
            assert_eq!(frame_byte_checksum(&rgba), naive(&rgba), "len {len}");
        }
    }
}

#[cfg(all(test, feature = "skia"))]
mod tests {
    use super::*;

    #[test]
    fn rejects_zero_sized_viewports() {
        assert!(render_document_rgba(0, 600).is_err());
        assert!(render_document_rgba(800, 0).is_err());
    }

    #[test]
    fn renders_the_live_dom_at_the_requested_size() {
        crate::dom::reset_document();
        let box_node = crate::dom::create_element("div");
        crate::dom::set_style_property(box_node, "width", "20px");
        crate::dom::set_style_property(box_node, "height", "10px");
        crate::dom::set_style_property(box_node, "background", "#00ff00");
        crate::dom::append_child(crate::dom::body_id(), box_node);

        let frame = render_document_rgba(64, 32).expect("headless frame");
        assert_eq!((frame.width, frame.height), (64, 32));
        assert_eq!(frame.rgba.len(), 64 * 32 * 4);
        assert!(
            frame
                .rgba
                .chunks_exact(4)
                .any(|pixel| pixel != [255, 255, 255, 255]),
            "the document box must change at least one white background pixel"
        );
    }

    #[test]
    fn block_in_inline_collapsible_whitespace_matches_the_direct_block() {
        crate::dom::reset_document();
        let span = crate::dom::create_element("span");
        crate::dom::set_style_property(span, "opacity", "0.5");
        crate::dom::append_child(span, crate::dom::create_text_node("\n  "));
        let nested = crate::dom::create_element("div");
        crate::dom::set_style_property(nested, "width", "100px");
        crate::dom::set_style_property(nested, "height", "100px");
        crate::dom::set_style_property(nested, "background", "green");
        crate::dom::append_child(span, nested);
        crate::dom::append_child(span, crate::dom::create_text_node("\n"));
        crate::dom::append_child(crate::dom::body_id(), span);
        let actual = render_document_rgba(160, 120).expect("render block in inline");

        crate::dom::reset_document();
        let direct = crate::dom::create_element("div");
        crate::dom::set_style_property(direct, "width", "100px");
        crate::dom::set_style_property(direct, "height", "100px");
        crate::dom::set_style_property(direct, "background", "green");
        crate::dom::set_style_property(direct, "opacity", "0.5");
        crate::dom::append_child(crate::dom::body_id(), direct);
        let expected = render_document_rgba(160, 120).expect("render direct block");

        assert_eq!(actual, expected);
    }

    #[test]
    fn window_scroll_to_changes_the_captured_viewport() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::jsdom::set_viewport(64.0, 64.0);
        let body = crate::dom::body_id();
        crate::dom::set_style_property(body, "margin", "0");
        crate::dom::set_style_property(body, "height", "200px");
        let child = crate::dom::create_element("div");
        for (name, value) in [
            ("width", "20px"),
            ("height", "20px"),
            ("position", "absolute"),
            ("top", "20px"),
            ("left", "0"),
            ("background", "green"),
        ] {
            crate::dom::set_style_property(child, name, value);
        }
        crate::dom::append_child(body, child);
        let before = render_document_rgba(64, 64).unwrap();
        let window = crate::jsdom::window_value();
        window.call_method(
            "scrollTo",
            vec![
                w3cos_core::Value::Number(0.0),
                w3cos_core::Value::Number(20.0),
            ],
        );
        assert_eq!(window.get_property("scrollY").to_number(), 20.0);
        let after = render_document_rgba(64, 64).unwrap();
        let pixel = (10 * 64 + 10) * 4;
        assert_eq!(&before.rgba[pixel..pixel + 4], &[255, 255, 255, 255]);
        assert_eq!(
            &after.rgba[pixel..pixel + 4],
            &[0, 128, 0, 255],
            "window scrolling must move document content into the captured viewport"
        );
        crate::jsdom::reset_bridge();
    }

    /// An inline element's inline padding edit, undone by
    /// `removeAttribute('style')`, must paint like a cold build of the
    /// unedited tree (Blink: an inline box style change marks the
    /// containing block `NeedsCollectInlines`).
    #[test]
    fn inline_span_padding_undone_by_remove_attribute_matches_cold_build() {
        use w3cos_core::Value;
        fn build() -> u32 {
            crate::dom::reset_document();
            crate::jsdom::reset_bridge();
            crate::jsdom::set_viewport(120.0, 60.0);
            let document = crate::jsdom::document_value();
            let sheet = document.call_method("createElement", vec![Value::string("style")]);
            sheet.set_property(
                "textContent",
                Value::string("body{margin:0} div{font-size:20px} .s{border:3px solid orange}"),
            );
            document
                .get_property("head")
                .call_method("appendChild", vec![sheet]);
            let body = crate::dom::body_id();
            let p = crate::dom::create_element("div");
            crate::dom::append_child(body, p);
            let mut last = 0;
            for text in ["A", "D"] {
                crate::dom::append_child(p, crate::dom::create_text_node(text));
                let span = crate::dom::create_element("span");
                crate::dom::set_attribute(span, "class", "s");
                crate::dom::append_child(p, span);
                last = span;
            }
            last
        }
        let differing = |a: &HeadlessFrame, b: &HeadlessFrame| {
            a.rgba
                .chunks(4)
                .zip(b.rgba.chunks(4))
                .filter(|(x, y)| x != y)
                .count()
        };
        build();
        let cold = render_document_rgba(120, 60).unwrap();
        let span = build();
        crate::jsdom::element_value(span)
            .get_property("style")
            .set_property("paddingBottom", Value::string("13px"));
        let cold_padded = render_document_rgba(120, 60).unwrap();
        let mut failures = Vec::new();
        for undo in [
            "remove-property",
            "remove-attribute",
            "set-attribute",
            "css-text-empty",
        ] {
            let span = build();
            let first = render_document_rgba(120, 60).unwrap();
            assert_eq!(differing(&first, &cold), 0, "two cold builds must agree");
            let style = crate::jsdom::element_value(span).get_property("style");
            style.set_property("paddingBottom", Value::string("13px"));
            let edited = render_document_rgba(120, 60).unwrap();
            assert!(differing(&edited, &cold) > 0, "the padding edit must paint");
            let count = differing(&edited, &cold_padded);
            if count != 0 {
                failures.push(format!(
                    "{undo} edit: {count} pixels vs a cold padded build"
                ));
            }
            match undo {
                "remove-property" => {
                    style.call_method("removeProperty", vec![Value::string("padding-bottom")]);
                }
                "remove-attribute" => crate::dom::remove_attribute(span, "style"),
                "set-attribute" => crate::jsdom::apply_html_attribute(span, "style", ""),
                _ => style.set_property("cssText", Value::string("")),
            }
            let restored = render_document_rgba(120, 60).unwrap();
            let count = differing(&restored, &cold);
            if count != 0 {
                failures.push(format!("{undo}: {count} pixels"));
            }
        }
        assert!(
            failures.is_empty(),
            "restored frames differ from a cold build: {failures:?}"
        );
        crate::jsdom::reset_bridge();
    }

    /// The last element of each page gets an inline padding edit that is
    /// undone; the restored frame must equal a cold build. Cases from WPT
    /// pages whose restored frame differed: an atomic inline image in an
    /// anonymous inline formatting context, a percent-height canvas in a
    /// table cell, and a block after floats and inline-blocks.
    #[test]
    fn padding_edit_undone_on_replaced_and_float_neighbours_matches_cold_build() {
        use w3cos_core::Value;
        fn build(html: &str) -> u32 {
            crate::dom::reset_document();
            crate::jsdom::reset_bridge();
            crate::jsdom::set_viewport(300.0, 200.0);
            let document = crate::jsdom::document_value();
            document
                .get_property("body")
                .set_property("innerHTML", Value::string(html));
            let target = document.call_method("getElementById", vec![Value::string("t")]);
            crate::jsdom::node_id_of(&target).expect("target element")
        }
        let differing = |a: &HeadlessFrame, b: &HeadlessFrame| {
            a.rgba
                .chunks(4)
                .zip(b.rgba.chunks(4))
                .filter(|(x, y)| x != y)
                .count()
        };
        let cases = [
            (
                "image",
                "<div style='font: 40px/1 monospace'>X<span style='font-size:60px'>X</span><span>X</span><img id=t width=60 height=60 style='background:green'></div>",
            ),
            (
                "canvas",
                "<div style='display:table-cell;height:100px;line-height:0'><canvas id=t width=200 height=200 style='background:green;max-height:100%'></canvas></div>",
            ),
            (
                "floats",
                "<style>.sample{border:solid 1px green;font-family:monospace;font-size:20px;line-height:24px;width:230px}.float{background:cyan;clear:left;float:left;height:50px;width:50px}.fake{background:lime;display:inline-block;height:80px;margin-top:2px;width:80px}.sur{display:inline-block;margin-left:-20px;vertical-align:top}.sur2{display:block;margin-left:50px;margin-top:0px}</style><div class=sample><div class=float></div><div class=float></div>\n<div class=fake></div>\n<div class=sur>bc<br>def<br>ghi<br>jkl<br></div>\n<div id=t class=sur2>mno</div>\n</div>",
            ),
        ];
        let mut failures = Vec::new();
        for (name, html) in cases {
            build(html);
            let cold = render_document_rgba(300, 200).unwrap();
            let span = build(html);
            let first = render_document_rgba(300, 200).unwrap();
            assert_eq!(
                differing(&first, &cold),
                0,
                "{name}: two cold builds must agree"
            );
            let style = crate::jsdom::element_value(span).get_property("style");
            style.set_property("paddingBottom", Value::string("3px"));
            let _ = render_document_rgba(300, 200).unwrap();
            style.call_method("removeProperty", vec![Value::string("padding-bottom")]);
            let restored = render_document_rgba(300, 200).unwrap();
            let count = differing(&restored, &cold);
            if count != 0 {
                failures.push(format!("{name}: {count} pixels"));
            }
        }
        assert!(
            failures.is_empty(),
            "restored frames differ from a cold build: {failures:?}"
        );
        crate::jsdom::reset_bridge();
    }

    /// The multi-frame WPT check's order on
    /// css/css-inline/initial-letter/initial-letter-float-002-ref: a text
    /// edit and its undo, the last element removed and reinserted, then a
    /// padding edit on that block (beside floats and inline-blocks) and its
    /// undo. Each restored frame must equal a cold build.
    #[test]
    fn float_neighbour_block_edits_in_check_order_match_cold_build() {
        use w3cos_core::Value;
        const CSS: &str = r#"
    .sample {
        border: solid 1px green;
        font-family: Ahem;
        font-size: 20px;
        line-height: 24px;
        width: 230px;
    }

    .float {
        background: cyan;
        clear: left;
        float: left;
        height: 50px;
        width: 50px;
    }

    .fake-initial-letter {
        background: lime;
        display: inline-block;
        height: 80px;
        margin-top: 2px;
        width: 80px;
    }

    .surrounding {
        display: inline-block;
        margin-left: -20px;
        vertical-align: top;
    }

    .surrounding2 {
        display: block;
        margin-left: 50px;
        margin-top: 0px;
    }
"#;
        const BODY: &str = r#"
<div class="sample"><div class="float"></div><div class="float"></div>
<div class="fake-initial-letter"></div>
<div class="surrounding">bc<br>def<br>ghi<br>jkl<br></div>
<div class="surrounding2">mno</div>
</div>
"#;
        fn build() -> u32 {
            crate::dom::reset_document();
            crate::jsdom::reset_bridge();
            crate::jsdom::set_viewport(800.0, 600.0);
            let document = crate::jsdom::document_value();
            let sheet = document.call_method("createElement", vec![Value::string("style")]);
            sheet.set_property("textContent", Value::string(CSS));
            document
                .get_property("head")
                .call_method("appendChild", vec![sheet]);
            document
                .get_property("body")
                .set_property("innerHTML", Value::string(BODY));
            let target =
                document.call_method("querySelector", vec![Value::string(".surrounding2")]);
            crate::jsdom::node_id_of(&target).expect("target element")
        }
        let differing = |a: &HeadlessFrame, b: &HeadlessFrame| {
            a.rgba
                .chunks(4)
                .zip(b.rgba.chunks(4))
                .filter(|(x, y)| x != y)
                .count()
        };
        // The page's Ahem web font, registered from the fixed-width fixture.
        let owner = u64::MAX - 513;
        struct ClearOwner(u64);
        impl Drop for ClearOwner {
            fn drop(&mut self) {
                crate::font_face::FontRegistry::global().clear_owner(self.0);
            }
        }
        let _clear = ClearOwner(owner);
        crate::font_face::FontRegistry::global()
            .register_for_owner(
                owner,
                crate::font_face::FontFace {
                    family: "Ahem".into(),
                    src: crate::font_face::FontSource::Bytes(
                        include_bytes!("layout_ng/testdata/Ahem.ttf").to_vec(),
                    ),
                    ..Default::default()
                },
            )
            .expect("register Ahem");
        build();
        let cold = render_document_rgba(800, 600).unwrap();
        let target = build();
        let first = render_document_rgba(800, 600).unwrap();
        assert_eq!(differing(&first, &cold), 0, "two cold builds must agree");
        let text = crate::dom::children(target)[0];
        let mut failures = Vec::new();
        for round in 0..2 {
            crate::dom::set_text_content(text, "mno x");
            let _ = render_document_rgba(800, 600).unwrap();
            crate::dom::set_text_content(text, "mno");
            let count = differing(&render_document_rgba(800, 600).unwrap(), &cold);
            if count != 0 {
                failures.push(format!("round {round} text: {count} pixels"));
            }
            let parent = crate::dom::parent_node(target).unwrap();
            let next = crate::dom::next_sibling(target);
            crate::dom::remove_child(parent, target);
            let _ = render_document_rgba(800, 600).unwrap();
            match next {
                Some(next) => crate::dom::insert_before(parent, target, next),
                None => crate::dom::append_child(parent, target),
            }
            let count = differing(&render_document_rgba(800, 600).unwrap(), &cold);
            if count != 0 {
                failures.push(format!("round {round} reinsert: {count} pixels"));
            }
            let style = crate::jsdom::element_value(target).get_property("style");
            style.set_property("paddingBottom", Value::string("3px"));
            let _ = render_document_rgba(800, 600).unwrap();
            style.call_method("removeProperty", vec![Value::string("padding-bottom")]);
            let count = differing(&render_document_rgba(800, 600).unwrap(), &cold);
            if count != 0 {
                failures.push(format!("round {round} padding: {count} pixels"));
            }
        }
        assert!(
            failures.is_empty(),
            "restored frames differ from a cold build: {failures:?}"
        );
        crate::jsdom::reset_bridge();
    }

    #[test]
    fn window_scroll_keeps_fixed_descendants_in_the_viewport() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::jsdom::set_viewport(64.0, 64.0);
        let body = crate::dom::body_id();
        crate::dom::set_style_property(body, "margin", "0");
        crate::dom::set_style_property(body, "height", "200px");
        let fixed = crate::dom::create_element("div");
        for (name, value) in [
            ("position", "fixed"),
            ("top", "10px"),
            ("left", "10px"),
            ("width", "20px"),
            ("height", "20px"),
            ("background", "red"),
        ] {
            crate::dom::set_style_property(fixed, name, value);
        }
        let child = crate::dom::create_element("div");
        for (name, value) in [
            ("width", "10px"),
            ("height", "10px"),
            ("background", "green"),
        ] {
            crate::dom::set_style_property(child, name, value);
        }
        crate::dom::append_child(fixed, child);
        crate::dom::append_child(body, fixed);
        let before = render_document_rgba(64, 64).unwrap();
        crate::jsdom::window_value().call_method(
            "scrollTo",
            vec![
                w3cos_core::Value::Number(0.0),
                w3cos_core::Value::Number(20.0),
            ],
        );
        let after = render_document_rgba(64, 64).unwrap();
        crate::jsdom::reset_bridge();
        assert_eq!(
            before.rgba, after.rgba,
            "fixed subtree must not scroll with the document"
        );
        let pixel = (15 * 64 + 15) * 4;
        assert_eq!(&after.rgba[pixel..pixel + 4], &[0, 128, 0, 255]);
    }

    #[test]
    fn window_scroll_moves_nested_overflow_clips_with_the_document() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::jsdom::set_viewport(64.0, 64.0);
        let body = crate::dom::body_id();
        crate::dom::set_style_property(body, "margin", "0");
        crate::dom::set_style_property(body, "height", "200px");
        let container = crate::dom::create_element("div");
        for (name, value) in [
            ("position", "absolute"),
            ("top", "30px"),
            ("left", "0"),
            ("width", "20px"),
            ("height", "20px"),
            ("overflow", "hidden"),
        ] {
            crate::dom::set_style_property(container, name, value);
        }
        let child = crate::dom::create_element("div");
        for (name, value) in [
            ("width", "20px"),
            ("height", "40px"),
            ("background", "green"),
        ] {
            crate::dom::set_style_property(child, name, value);
        }
        crate::dom::append_child(container, child);
        crate::dom::append_child(body, container);
        crate::jsdom::window_value().call_method(
            "scrollTo",
            vec![
                w3cos_core::Value::Number(0.0),
                w3cos_core::Value::Number(20.0),
            ],
        );
        let frame = render_document_rgba(64, 64).unwrap();
        crate::jsdom::reset_bridge();
        for (y, expected) in [
            (5, [255, 255, 255, 255]),
            (15, [0, 128, 0, 255]),
            (25, [0, 128, 0, 255]),
            (35, [255, 255, 255, 255]),
        ] {
            let pixel = (y * 64 + 10) * 4;
            assert_eq!(&frame.rgba[pixel..pixel + 4], &expected, "clip at y={y}");
        }
    }

    /// Run `run` again when the process-wide font registry moved while it
    /// ran (tests on other threads register and clear fonts): a font change
    /// correctly re-records every layer, which these tests cannot tell from
    /// lost reuse. `run` must set up the document state it renders.
    fn with_stable_fonts<T>(mut run: impl FnMut() -> T) -> T {
        let registry = crate::font_face::FontRegistry::global();
        for _ in 0..8 {
            let before = registry.revision();
            let result = run();
            if registry.revision() == before {
                return result;
            }
        }
        panic!("the font registry kept changing under the test");
    }

    fn with_layer_reuse<T>(enabled: bool, run: impl FnOnce() -> T) -> T {
        PAINT_LAYER_REUSE_OVERRIDE.with(|value| value.set(Some(enabled)));
        let result = run();
        PAINT_LAYER_REUSE_OVERRIDE.with(|value| value.set(None));
        result
    }

    #[test]
    fn layer_reuse_keeps_unchanged_layers_and_matches_a_fresh_frame() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        invalidate_retained_frame();
        let body = crate::dom::body_id();
        let mut texts = Vec::new();
        for index in 0..4 {
            let card = crate::dom::create_element("div");
            for (name, value) in [
                ("will-change", "transform"),
                ("width", "120px"),
                ("background", "#eeeeff"),
                ("border-bottom", "2px solid #3366cc"),
            ] {
                crate::dom::set_style_property(card, name, value);
            }
            let paragraph = crate::dom::create_element("p");
            let text = crate::dom::create_text_node(&format!("card {index} says hello"));
            crate::dom::append_child(paragraph, text);
            crate::dom::append_child(card, paragraph);
            crate::dom::append_child(body, card);
            texts.push(text);
        }
        let (first, edited, reused) = with_stable_fonts(|| {
            with_layer_reuse(true, || {
                crate::dom::set_text_content(texts[3], "card 3 says hello");
                render_document_rgba(160, 360).expect("settled frame");
                let first = render_document_rgba(160, 360).expect("first frame");
                crate::dom::set_text_content(texts[3], "card 3 says goodbye");
                let edited = render_document_rgba(160, 360).expect("edited frame");
                (first, edited, HEADLESS_SKIA_REUSED.with(Cell::get))
            })
        });
        assert_ne!(first.rgba, edited.rgba, "the edit must change pixels");
        assert!(reused.1 > 1, "the cards must paint in several layers");
        assert!(
            reused.0 > 0 && reused.0 < reused.1,
            "only the edited card's layers repaint, got {reused:?}"
        );
        let fresh = with_layer_reuse(false, || {
            crate::incremental_layout::invalidate();
            invalidate_retained_frame();
            render_document_rgba(160, 360).expect("fresh frame")
        });
        assert_eq!(
            edited.rgba, fresh.rgba,
            "kept layer recordings must match a frame painted from scratch"
        );
        // Wrapping the first card moves every card below it.
        let (wrapped, moved_reuse) = with_stable_fonts(|| {
            with_layer_reuse(true, || {
                crate::dom::set_text_content(texts[0], "card 0 says hello");
                render_document_rgba(160, 360).expect("warm frame");
                crate::dom::set_text_content(texts[0], "card 0 now says a much longer hello");
                let wrapped = render_document_rgba(160, 360).expect("wrapped frame");
                (wrapped, HEADLESS_SKIA_REUSED.with(Cell::get))
            })
        });
        // The cards below only moved: their layers keep their recordings
        // under the offset (the first card's layers re-record).
        assert!(
            moved_reuse.0 > 0 && moved_reuse.0 < moved_reuse.1,
            "moved cards keep their recordings, got {moved_reuse:?}"
        );
        let fresh = with_layer_reuse(false, || {
            crate::incremental_layout::invalidate();
            invalidate_retained_frame();
            render_document_rgba(160, 360).expect("fresh wrapped frame")
        });
        assert_eq!(
            wrapped.rgba, fresh.rgba,
            "moved layers must match a frame painted from scratch"
        );
    }

    #[test]
    fn node_paint_reuse_matches_frames_painted_from_scratch() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        invalidate_retained_frame();
        let body = crate::dom::body_id();
        let mut texts = Vec::new();
        for index in 0..6 {
            let card = crate::dom::create_element("div");
            crate::dom::set_style_property(card, "width", "140px");
            let paragraph = crate::dom::create_element("p");
            let text = crate::dom::create_text_node(&format!("card {index} says hello"));
            crate::dom::append_child(paragraph, text);
            crate::dom::append_child(card, paragraph);
            crate::dom::append_child(body, card);
            texts.push(text);
        }
        struct Off;
        impl Drop for Off {
            fn drop(&mut self) {
                crate::render_skia::NODE_PAINT_REUSE_OVERRIDE.with(|value| value.set(None));
            }
        }
        let _off = Off;
        let edits: [(usize, &str); 4] = [
            (5, "card 5 says goodbye"),
            (2, "card 2 now says a much longer hello that wraps"),
            (5, "card 5"),
            (2, "card 2"),
        ];
        for mode in [2u8, 1] {
            crate::render_skia::NODE_PAINT_REUSE_OVERRIDE.with(|value| value.set(Some(mode)));
            let (frames, (reused, mismatched)) = with_stable_fonts(|| {
                with_layer_reuse(true, || {
                    invalidate_retained_frame();
                    for (index, text) in texts.iter().enumerate() {
                        crate::dom::set_text_content(*text, &format!("card {index} says hello"));
                    }
                    let before = crate::render_skia::node_paint_stats();
                    render_document_rgba(160, 480).expect("first frame");
                    let mut frames = Vec::new();
                    for (card, text) in edits {
                        crate::dom::set_text_content(texts[card], text);
                        frames.push(render_document_rgba(160, 480).expect("edited frame").rgba);
                    }
                    let after = crate::render_skia::node_paint_stats();
                    (frames, (after.0 - before.0, after.2 - before.2))
                })
            });
            assert!(reused > 0, "mode {mode}: unchanged text clients must reuse");
            assert_eq!(mismatched, 0, "mode {mode}: a reused drawing differs");
            crate::render_skia::NODE_PAINT_REUSE_OVERRIDE.with(|value| value.set(Some(0)));
            for index in 0..6 {
                crate::dom::set_text_content(texts[index], &format!("card {index} says hello"));
            }
            for (step, (card, text)) in edits.into_iter().enumerate() {
                crate::dom::set_text_content(texts[card], text);
                let fresh = with_layer_reuse(false, || {
                    crate::incremental_layout::invalidate();
                    invalidate_retained_frame();
                    render_document_rgba(160, 480).expect("fresh frame")
                });
                assert!(
                    frames[step] == fresh.rgba,
                    "mode {mode} step {step}: a frame reusing text drawings differs"
                );
            }
        }
    }

    #[test]
    fn paint_scroll_plan_kept_across_splices_matches_a_built_plan() {
        struct Off;
        impl Drop for Off {
            fn drop(&mut self) {
                crate::layout::PAINT_SCROLL_PLAN_OVERRIDE.with(|value| value.set(None));
                crate::incremental_layout::force_paint_splice(None);
            }
        }
        let _off = Off;
        let run = |mode: u8| {
            crate::layout::PAINT_SCROLL_PLAN_OVERRIDE.with(|value| value.set(Some(mode)));
            crate::dom::reset_document();
            crate::jsdom::reset_bridge();
            crate::incremental_layout::invalidate();
            invalidate_retained_frame();
            crate::incremental_layout::force_paint_splice(Some(true));
            // Paint splices follow a flush laid out at the painted size.
            crate::jsdom::set_viewport(160.0, 480.0);
            let body = crate::dom::body_id();
            let log = crate::dom::create_element("div");
            crate::dom::set_style_property(log, "width", "150px");
            // A closed bottom edge lets the streamed text splice as a tail.
            crate::dom::set_style_property(log, "padding-bottom", "2px");
            let mut messages = Vec::new();
            for index in 0..5 {
                let message = crate::dom::create_element("div");
                crate::dom::set_style_property(message, "padding", "4px");
                if index == 3 {
                    crate::dom::set_style_property(message, "transform", "translateX(6px)");
                }
                let paragraph = crate::dom::create_element("p");
                let text = crate::dom::create_text_node(&format!("message {index} says hi"));
                crate::dom::append_child(paragraph, text);
                crate::dom::append_child(message, paragraph);
                crate::dom::append_child(log, message);
                messages.push(message);
            }
            let scroller = crate::dom::create_element("div");
            crate::dom::set_style_property(scroller, "height", "40px");
            crate::dom::set_style_property(scroller, "overflow", "auto");
            for line in 0..4 {
                let row = crate::dom::create_element("p");
                crate::dom::append_child(row, crate::dom::create_text_node(&format!("row {line}")));
                crate::dom::append_child(scroller, row);
            }
            crate::dom::append_child(body, log);
            crate::dom::append_child(body, scroller);
            let streamed = crate::dom::create_element("p");
            let text = crate::dom::create_text_node("streamed");
            crate::dom::append_child(streamed, text);
            crate::dom::append_child(messages[2], streamed);
            assert!(crate::incremental_layout::flush());
            let before = crate::layout::paint_scroll_plan_stats();
            let mut frames = vec![render_document_rgba(160, 480).expect("first frame").rgba];
            let mut content = String::from("streamed");
            for step in 0..6 {
                content.push_str(" more words");
                crate::dom::set_text_content(text, &content);
                if step == 3 {
                    crate::dom::set_scroll_offset(scroller, None, Some(20.0));
                }
                frames.push(render_document_rgba(160, 480).expect("stream frame").rgba);
            }
            let after = crate::layout::paint_scroll_plan_stats();
            (frames, (after.0 - before.0, after.2 - before.2))
        };
        let (verified, (kept, mismatched)) = run(2);
        if crate::layout_ng::mode() == crate::layout_ng::Mode::On {
            assert!(kept > 0, "stream splices must keep the paint scroll plan");
        }
        assert_eq!(
            mismatched, 0,
            "a kept paint scroll plan differs from a built one"
        );
        let (reused, _) = run(1);
        let (fresh, (off_kept, _)) = run(0);
        assert_eq!(off_kept, 0);
        assert!(verified == fresh, "verify-mode frames differ");
        assert!(
            reused == fresh,
            "frames with a kept paint scroll plan differ"
        );
    }

    /// Scroll info shared per scroll container (`W3COS_SCROLL_INFO_SHARED`)
    /// equals the per-box computation, with sticky boxes, a nested scroller
    /// and a clip-only box, before and after scrolling; the frames match.
    #[test]
    fn scroll_info_shared_matches_per_box_scroll_info() {
        struct Off;
        impl Drop for Off {
            fn drop(&mut self) {
                crate::window::SCROLL_INFO_SHARED_OVERRIDE.with(|value| value.set(None));
            }
        }
        let _off = Off;
        let run = |mode: u8| {
            crate::window::SCROLL_INFO_SHARED_OVERRIDE.with(|value| value.set(Some(mode)));
            crate::dom::reset_document();
            crate::jsdom::reset_bridge();
            crate::incremental_layout::invalidate();
            invalidate_retained_frame();
            crate::jsdom::set_viewport(160.0, 240.0);
            let body = crate::dom::body_id();
            let scroller = crate::dom::create_element("div");
            crate::dom::set_style_property(scroller, "height", "80px");
            crate::dom::set_style_property(scroller, "overflow", "auto");
            let header = crate::dom::create_element("div");
            crate::dom::set_style_property(header, "position", "sticky");
            crate::dom::set_style_property(header, "top", "0px");
            crate::dom::set_style_property(header, "background", "yellow");
            let label = crate::dom::create_element("span");
            crate::dom::append_child(label, crate::dom::create_text_node("sticky header"));
            crate::dom::append_child(header, label);
            crate::dom::append_child(scroller, header);
            for line in 0..8 {
                let row = crate::dom::create_element("p");
                crate::dom::set_style_property(
                    row,
                    "color",
                    if line % 2 == 0 { "red" } else { "blue" },
                );
                crate::dom::append_child(row, crate::dom::create_text_node(&format!("row {line}")));
                crate::dom::append_child(scroller, row);
            }
            let nested = crate::dom::create_element("div");
            crate::dom::set_style_property(nested, "height", "30px");
            crate::dom::set_style_property(nested, "overflow", "auto");
            for line in 0..4 {
                let row = crate::dom::create_element("p");
                crate::dom::append_child(
                    row,
                    crate::dom::create_text_node(&format!("inner {line}")),
                );
                crate::dom::append_child(nested, row);
            }
            crate::dom::append_child(scroller, nested);
            let clip = crate::dom::create_element("div");
            crate::dom::set_style_property(clip, "height", "20px");
            crate::dom::set_style_property(clip, "overflow", "hidden");
            for line in 0..3 {
                let row = crate::dom::create_element("p");
                crate::dom::append_child(
                    row,
                    crate::dom::create_text_node(&format!("clipped {line}")),
                );
                crate::dom::append_child(clip, row);
            }
            crate::dom::append_child(body, scroller);
            crate::dom::append_child(body, clip);
            assert!(crate::incremental_layout::flush());
            let before = crate::window::SCROLL_INFO_SHARED_STATS.with(std::cell::Cell::get);
            let mut frames = vec![render_document_rgba(160, 240).expect("first frame").rgba];
            for (target, offset) in [(scroller, 20.0), (nested, 10.0), (scroller, 45.0)] {
                crate::dom::set_scroll_offset(target, None, Some(offset));
                frames.push(render_document_rgba(160, 240).expect("scrolled frame").rgba);
            }
            let after = crate::window::SCROLL_INFO_SHARED_STATS.with(std::cell::Cell::get);
            (frames, (after.0 - before.0, after.1 - before.1))
        };
        let (verified, (hits, mismatched)) = run(2);
        assert!(hits > 0, "descendants of a scroller must share its entry");
        assert_eq!(
            mismatched, 0,
            "shared scroll info differs from per-box scroll info"
        );
        let (shared, _) = run(1);
        let (fresh, (off_hits, _)) = run(0);
        assert_eq!(off_hits, 0);
        assert!(verified == fresh, "verify-mode frames differ");
        assert!(shared == fresh, "frames with shared scroll info differ");
    }

    /// Kinds kept from two frames back for boxes no splice touched equal the
    /// copied ones, and the frames are the same.
    #[test]
    fn paint_kind_keep_matches_copied_kinds() {
        struct Off;
        impl Drop for Off {
            fn drop(&mut self) {
                PAINT_KIND_KEEP_OVERRIDE.with(|value| value.set(None));
                crate::incremental_layout::force_paint_splice(None);
                crate::paint_artifact::ARTIFACT_SPLICE_OVERRIDE.with(|value| value.set(None));
                crate::paint_artifact::PAINT_TOUCHED_OVERRIDE.with(|value| value.set(None));
                crate::incremental_layout::WARM_REBUILD_OVERRIDE.with(|value| value.set(None));
            }
        }
        let _off = Off;
        let run = |mode: u8| {
            PAINT_KIND_KEEP_OVERRIDE.with(|value| value.set(Some(mode)));
            // Default-on flags the kept kinds rely on, pinned.
            crate::paint_artifact::ARTIFACT_SPLICE_OVERRIDE.with(|value| value.set(Some(true)));
            crate::paint_artifact::PAINT_TOUCHED_OVERRIDE.with(|value| value.set(Some(1)));
            crate::incremental_layout::WARM_REBUILD_OVERRIDE.with(|value| value.set(Some(true)));
            crate::dom::reset_document();
            crate::jsdom::reset_bridge();
            crate::incremental_layout::invalidate();
            invalidate_retained_frame();
            crate::incremental_layout::force_paint_splice(Some(true));
            crate::jsdom::set_viewport(160.0, 480.0);
            let body = crate::dom::body_id();
            let log = crate::dom::create_element("div");
            crate::dom::set_style_property(log, "width", "150px");
            let mut texts = Vec::new();
            for index in 0..6 {
                let message = crate::dom::create_element("p");
                let text = crate::dom::create_text_node(&format!("message {index} says hi"));
                crate::dom::append_child(message, text);
                crate::dom::append_child(log, message);
                texts.push(text);
            }
            crate::dom::append_child(body, log);
            assert!(crate::incremental_layout::flush());
            let before = paint_kind_keep_stats();
            let mut frames = vec![render_document_rgba(160, 480).expect("first frame").rgba];
            let mut contents: Vec<String> = (0..6)
                .map(|index| format!("message {index} says hi"))
                .collect();
            for step in 0..6 {
                // Alternate two messages so a kind changed two frames back is
                // in the older touched set.
                let at = 2 + step % 2;
                contents[at].push_str(" more");
                crate::dom::set_text_content(texts[at], &contents[at]);
                frames.push(render_document_rgba(160, 480).expect("stream frame").rgba);
            }
            let after = paint_kind_keep_stats();
            (
                frames,
                (after.0 - before.0, after.1 - before.1, after.2 - before.2),
            )
        };
        let (verified, (_, _, mismatched)) = run(2);
        assert_eq!(mismatched, 0, "a kept kind differs from the copied one");
        let (kept_frames, (kept, _, _)) = run(1);
        if crate::layout_ng::mode() == crate::layout_ng::Mode::On {
            assert!(
                kept > 0,
                "boxes untouched for two frames must keep their kind"
            );
        }
        let (fresh, (off_kept, _, _)) = run(0);
        assert_eq!(off_kept, 0);
        assert!(
            verified == fresh && kept_frames == fresh,
            "kept kinds changed the frames"
        );
    }

    #[test]
    fn paint_range_rebuild_matches_full_passes() {
        struct Off;
        impl Drop for Off {
            fn drop(&mut self) {
                crate::paint_artifact::PAINT_RANGE_REBUILD_OVERRIDE.with(|value| value.set(None));
                crate::incremental_layout::force_paint_splice(None);
                crate::paint_artifact::ARTIFACT_SPLICE_OVERRIDE.with(|value| value.set(None));
                crate::incremental_layout::WARM_REBUILD_OVERRIDE.with(|value| value.set(None));
            }
        }
        let _off = Off;
        let run = |mode: u8| {
            crate::paint_artifact::PAINT_RANGE_REBUILD_OVERRIDE.with(|value| value.set(Some(mode)));
            // The record is kept across spliced frames only: pin the splice
            // flags so the lib-test modes that turn them off still check it.
            crate::paint_artifact::ARTIFACT_SPLICE_OVERRIDE.with(|value| value.set(Some(true)));
            crate::incremental_layout::WARM_REBUILD_OVERRIDE.with(|value| value.set(Some(true)));
            crate::dom::reset_document();
            crate::jsdom::reset_bridge();
            crate::incremental_layout::invalidate();
            invalidate_retained_frame();
            crate::incremental_layout::force_paint_splice(Some(true));
            crate::jsdom::set_viewport(160.0, 480.0);
            let body = crate::dom::body_id();
            let log = crate::dom::create_element("div");
            crate::dom::set_style_property(log, "width", "150px");
            crate::dom::set_style_property(log, "padding-bottom", "2px");
            let mut messages = Vec::new();
            for index in 0..5 {
                let message = crate::dom::create_element("div");
                crate::dom::set_style_property(message, "padding", "4px");
                // Leading white space at a line start is trimmed in paint.
                let span = crate::dom::create_element("span");
                crate::dom::set_style_property(span, "color", "red");
                let inner = crate::dom::create_text_node(&format!("  message {index}"));
                crate::dom::append_child(span, inner);
                crate::dom::append_child(message, span);
                let text = crate::dom::create_text_node(" says hi");
                crate::dom::append_child(message, text);
                crate::dom::append_child(log, message);
                messages.push(message);
            }
            crate::dom::append_child(body, log);
            let streamed = crate::dom::create_element("p");
            let text = crate::dom::create_text_node(" streamed");
            crate::dom::append_child(streamed, text);
            crate::dom::append_child(messages[2], streamed);
            assert!(crate::incremental_layout::flush());
            let before = crate::paint_artifact::paint_range_stats();
            let mut frames = vec![render_document_rgba(160, 480).expect("first frame").rgba];
            let mut content = String::from(" streamed");
            for _ in 0..5 {
                content.push_str(" more words");
                crate::dom::set_text_content(text, &content);
                frames.push(render_document_rgba(160, 480).expect("stream frame").rgba);
            }
            let after = crate::paint_artifact::paint_range_stats();
            (
                frames,
                (after.0 - before.0, after.1 - before.1, after.2 - before.2),
            )
        };
        let (verified, (aligned, copied, mismatched)) = run(2);
        if crate::layout_ng::mode() == crate::layout_ng::Mode::On {
            assert!(
                aligned > 0,
                "spliced frames must reuse the previous build's record"
            );
            // With `W3COS_PAINT_PERSISTENT_NODES=1` clean boxes take the
            // persistent node instead, so no trim decision is copied.
            if crate::paint_artifact::persistent_nodes_mode() != 1 {
                assert!(
                    copied > 0,
                    "untouched boxes must take the previous trim decision"
                );
            }
        }
        assert_eq!(
            mismatched, 0,
            "range-scoped passes differ from the full passes"
        );
        let (ranged, _) = run(1);
        let (full, (off_aligned, _, _)) = run(0);
        assert_eq!(off_aligned, 0);
        assert!(verified == full, "verify-mode frames differ");
        assert!(ranged == full, "frames from range-scoped passes differ");
    }

    #[test]
    fn shared_paint_flat_matches_a_fresh_flatten() {
        struct Off;
        impl Drop for Off {
            fn drop(&mut self) {
                crate::incremental_layout::SHARED_PAINT_FLAT_OVERRIDE.with(|value| value.set(None));
                crate::incremental_layout::force_paint_splice(None);
            }
        }
        let _off = Off;
        let run = |mode: u8| {
            crate::incremental_layout::SHARED_PAINT_FLAT_OVERRIDE
                .with(|value| value.set(Some(mode)));
            crate::dom::reset_document();
            crate::jsdom::reset_bridge();
            crate::incremental_layout::invalidate();
            invalidate_retained_frame();
            crate::incremental_layout::force_paint_splice(Some(true));
            crate::jsdom::set_viewport(160.0, 480.0);
            let body = crate::dom::body_id();
            let log = crate::dom::create_element("div");
            crate::dom::set_style_property(log, "width", "150px");
            crate::dom::set_style_property(log, "padding-bottom", "2px");
            let mut messages = Vec::new();
            for index in 0..5 {
                let message = crate::dom::create_element("div");
                crate::dom::set_style_property(message, "padding", "4px");
                let paragraph = crate::dom::create_element("p");
                let text = crate::dom::create_text_node(&format!("message {index} says hi"));
                crate::dom::append_child(paragraph, text);
                crate::dom::append_child(message, paragraph);
                crate::dom::append_child(log, message);
                messages.push(message);
            }
            crate::dom::append_child(body, log);
            let streamed = crate::dom::create_element("p");
            let text = crate::dom::create_text_node("streamed");
            crate::dom::append_child(streamed, text);
            crate::dom::append_child(messages[2], streamed);
            assert!(crate::incremental_layout::flush());
            let before = shared_flat_stats();
            let mut frames = vec![render_document_rgba(160, 480).expect("first frame").rgba];
            let mut content = String::from("streamed");
            for _ in 0..5 {
                content.push_str(" more words");
                crate::dom::set_text_content(text, &content);
                frames.push(render_document_rgba(160, 480).expect("stream frame").rgba);
            }
            let after = shared_flat_stats();
            (frames, (after.0 - before.0, after.1 - before.1))
        };
        let (verified, (used, mismatched)) = run(2);
        if crate::layout_ng::mode() == crate::layout_ng::Mode::On {
            assert!(
                used > 0,
                "spliced frames must paint from the splice's flat tree"
            );
        }
        assert_eq!(
            mismatched, 0,
            "a shared flat tree differs from a fresh flatten"
        );
        let (shared, _) = run(1);
        let (fresh, (off_used, _)) = run(0);
        assert_eq!(off_used, 0);
        assert!(verified == fresh, "verify-mode frames differ");
        assert!(
            shared == fresh,
            "frames painted from a shared flat tree differ"
        );
    }

    #[test]
    fn paint_flat_keep_matches_a_fresh_flatten() {
        struct Off;
        impl Drop for Off {
            fn drop(&mut self) {
                crate::incremental_layout::PAINT_FLAT_KEEP_OVERRIDE.with(|value| value.set(None));
                crate::incremental_layout::force_paint_splice(None);
            }
        }
        let _off = Off;
        let run = |mode: u8| {
            crate::incremental_layout::PAINT_FLAT_KEEP_OVERRIDE.with(|value| value.set(Some(mode)));
            crate::dom::reset_document();
            crate::jsdom::reset_bridge();
            crate::incremental_layout::invalidate();
            invalidate_retained_frame();
            crate::incremental_layout::force_paint_splice(Some(true));
            crate::jsdom::set_viewport(160.0, 480.0);
            let body = crate::dom::body_id();
            let log = crate::dom::create_element("div");
            crate::dom::set_style_property(log, "width", "150px");
            let mut messages = Vec::new();
            for index in 0..5 {
                let message = crate::dom::create_element("div");
                crate::dom::set_style_property(message, "padding", "4px");
                let paragraph = crate::dom::create_element("p");
                let text = crate::dom::create_text_node(&format!("message {index} says hi"));
                crate::dom::append_child(paragraph, text);
                crate::dom::append_child(message, paragraph);
                crate::dom::append_child(log, message);
                messages.push(message);
            }
            crate::dom::append_child(body, log);
            let streamed = crate::dom::create_element("p");
            let text = crate::dom::create_text_node("streamed");
            crate::dom::append_child(streamed, text);
            crate::dom::append_child(messages[2], streamed);
            assert!(crate::incremental_layout::flush());
            let before = crate::incremental_layout::paint_flat_keep_stats();
            let mut frames = vec![render_document_rgba(160, 480).expect("first frame").rgba];
            let mut content = String::from("streamed");
            for step in 0..8 {
                match step % 4 {
                    // Text edits inside a kept message.
                    0 | 2 => {
                        content.push_str(" more words");
                        crate::dom::set_text_content(text, &content);
                    }
                    // A new message at the end (the log's child list grows,
                    // and at times moves).
                    1 => {
                        let message = crate::dom::create_element("div");
                        let span = crate::dom::create_element("span");
                        let words = crate::dom::create_text_node(&format!("new {step}"));
                        crate::dom::append_child(span, words);
                        crate::dom::append_child(message, span);
                        crate::dom::append_child(log, message);
                    }
                    // A word inside an earlier message.
                    _ => {
                        let word = crate::dom::create_element("b");
                        let words = crate::dom::create_text_node("bold");
                        crate::dom::append_child(word, words);
                        crate::dom::append_child(messages[1], word);
                    }
                }
                frames.push(render_document_rgba(160, 480).expect("stream frame").rgba);
            }
            let after = crate::incremental_layout::paint_flat_keep_stats();
            (frames, (after.0 - before.0, after.2 - before.2))
        };
        let (verified, (patched, mismatched)) = run(2);
        if crate::layout_ng::mode() == crate::layout_ng::Mode::On {
            assert!(patched > 0, "paint splices must patch the kept flat tree");
        }
        assert_eq!(
            mismatched, 0,
            "a patched flat tree differs from a fresh flatten"
        );
        let (kept, (_, kept_mismatched)) = run(1);
        assert_eq!(kept_mismatched, 0);
        let (fresh, (off_patched, _)) = run(0);
        assert_eq!(off_patched, 0);
        assert!(verified == fresh, "verify-mode frames differ");
        assert!(
            kept == fresh,
            "frames painted from a patched flat tree differ"
        );
    }

    #[test]
    fn paint_feature_skip_matches_the_full_passes() {
        struct Off;
        impl Drop for Off {
            fn drop(&mut self) {
                crate::paint_artifact::PAINT_FEATURE_SKIP_OVERRIDE.with(|value| value.set(None));
                crate::incremental_layout::force_paint_splice(None);
            }
        }
        let _off = Off;
        let run = |mode: u8| {
            crate::paint_artifact::PAINT_FEATURE_SKIP_OVERRIDE.with(|value| value.set(Some(mode)));
            crate::dom::reset_document();
            crate::jsdom::reset_bridge();
            crate::incremental_layout::invalidate();
            invalidate_retained_frame();
            crate::incremental_layout::force_paint_splice(Some(true));
            crate::jsdom::set_viewport(160.0, 480.0);
            let body = crate::dom::body_id();
            let log = crate::dom::create_element("div");
            crate::dom::set_style_property(log, "width", "150px");
            crate::dom::append_child(body, log);
            let first = crate::dom::create_element("p");
            let text = crate::dom::create_text_node("plain");
            crate::dom::append_child(first, text);
            crate::dom::append_child(log, first);
            assert!(crate::incremental_layout::flush());
            let before = crate::paint_artifact::paint_feature_skip_stats();
            let mut frames = vec![render_document_rgba(160, 480).expect("first frame").rgba];
            for step in 0..6 {
                match step {
                    // A box with a percentage translate arrives.
                    1 => {
                        let moved = crate::dom::create_element("div");
                        for (name, value) in [
                            ("width", "40px"),
                            ("height", "20px"),
                            ("background", "#3366cc"),
                            ("transform", "translate(50%, 25%)"),
                        ] {
                            crate::dom::set_style_property(moved, name, value);
                        }
                        crate::dom::append_child(log, moved);
                    }
                    // Then a table.
                    3 => {
                        let table = crate::dom::create_element("table");
                        crate::dom::set_style_property(table, "border-collapse", "collapse");
                        let row = crate::dom::create_element("tr");
                        let cell = crate::dom::create_element("td");
                        crate::dom::set_style_property(cell, "border", "2px solid #000");
                        let words = crate::dom::create_text_node("cell");
                        crate::dom::append_child(cell, words);
                        crate::dom::append_child(row, cell);
                        crate::dom::append_child(table, row);
                        crate::dom::append_child(log, table);
                    }
                    _ => {
                        let line = crate::dom::create_element("p");
                        let words = crate::dom::create_text_node(&format!("line {step}"));
                        crate::dom::append_child(line, words);
                        crate::dom::append_child(log, line);
                    }
                }
                frames.push(render_document_rgba(160, 480).expect("stream frame").rgba);
            }
            let after = crate::paint_artifact::paint_feature_skip_stats();
            (frames, (after.0 - before.0, after.1 - before.1))
        };
        let (verified, (skipped, mismatched)) = run(2);
        assert!(skipped > 0, "passes for absent features must be skipped");
        assert_eq!(mismatched, 0, "a skipped pass was needed");
        let (skip, _) = run(1);
        let (full, (off_skipped, _)) = run(0);
        assert_eq!(off_skipped, 0);
        assert!(verified == full, "verify-mode frames differ");
        assert!(skip == full, "frames with skipped passes differ");
    }

    #[test]
    fn persist_dirty_scan_matches_the_full_scan() {
        struct Off;
        impl Drop for Off {
            fn drop(&mut self) {
                crate::paint_artifact::PERSIST_DIRTY_SCAN_OVERRIDE.with(|value| value.set(None));
                crate::incremental_layout::force_paint_splice(None);
            }
        }
        let _off = Off;
        let run = |mode: u8| {
            crate::paint_artifact::PERSIST_DIRTY_SCAN_OVERRIDE.with(|value| value.set(Some(mode)));
            crate::dom::reset_document();
            crate::jsdom::reset_bridge();
            crate::incremental_layout::invalidate();
            invalidate_retained_frame();
            crate::incremental_layout::force_paint_splice(Some(true));
            crate::jsdom::set_viewport(160.0, 480.0);
            let body = crate::dom::body_id();
            let log = crate::dom::create_element("div");
            crate::dom::set_style_property(log, "width", "150px");
            crate::dom::append_child(body, log);
            // A flex row whose static items carry z-index (painted like
            // positioned boxes), and inline text with line-leading spaces.
            let row = crate::dom::create_element("div");
            crate::dom::set_style_property(row, "display", "flex");
            for (index, color) in ["#cc3333", "#33cc33"].iter().enumerate() {
                let item = crate::dom::create_element("div");
                for (name, value) in [
                    ("width", "30px"),
                    ("height", "20px"),
                    ("background", color),
                    ("margin-left", if index == 1 { "-10px" } else { "0px" }),
                    ("z-index", if index == 0 { "2" } else { "1" }),
                ] {
                    crate::dom::set_style_property(item, name, value);
                }
                crate::dom::append_child(row, item);
            }
            crate::dom::append_child(log, row);
            let mut texts = Vec::new();
            for index in 0..3 {
                let message = crate::dom::create_element("p");
                let span = crate::dom::create_element("span");
                let words = crate::dom::create_text_node(&format!("  message {index} words"));
                crate::dom::append_child(span, words);
                crate::dom::append_child(message, span);
                crate::dom::append_child(log, message);
                texts.push(words);
            }
            assert!(crate::incremental_layout::flush());
            let before = crate::paint_artifact::persist_dirty_scan_stats();
            let persistent_before = persistent_paint_stats().0;
            let mut frames = vec![render_document_rgba(160, 480).expect("first frame").rgba];
            for step in 0..6 {
                match step % 3 {
                    0 => crate::dom::set_text_content(
                        texts[2],
                        &format!("  message 2 words and {step} more words that wrap"),
                    ),
                    1 => crate::dom::set_text_content(texts[0], &format!(" first {step}")),
                    _ => {
                        let line = crate::dom::create_element("p");
                        let words = crate::dom::create_text_node(&format!("   line {step}"));
                        crate::dom::append_child(line, words);
                        crate::dom::append_child(log, line);
                    }
                }
                frames.push(render_document_rgba(160, 480).expect("stream frame").rgba);
            }
            let after = crate::paint_artifact::persist_dirty_scan_stats();
            let persistent = persistent_paint_stats().0 - persistent_before;
            (frames, (after.0 - before.0, after.1 - before.1), persistent)
        };
        let (verified, (walked, mismatched), persistent) = run(2);
        // The dirty list exists only on the persistent path
        // (`W3COS_PAINT_PERSISTENT_NODES`, on in the gate's verify mode).
        if persistent > 0 {
            assert!(walked > 0, "persistent builds must walk the dirty list");
        }
        assert_eq!(
            mismatched, 0,
            "the dirty list walk differs from the full scan"
        );
        let (walk, _, _) = run(1);
        let (full, (off_walked, _), _) = run(0);
        assert_eq!(off_walked, 0);
        assert!(verified == full, "verify-mode frames differ");
        assert!(walk == full, "frames from the dirty list walk differ");
    }

    #[cfg(feature = "skia")]
    #[test]
    fn quiet_frames_match_a_full_paint() {
        struct Off;
        impl Drop for Off {
            fn drop(&mut self) {
                super::QUIET_FRAME_OVERRIDE.with(|value| value.set(None));
                super::QUIET_EXIT_OVERRIDE.with(|value| value.set(None));
            }
        }
        let _off = Off;
        let run = |mode: u8, exit: u8| {
            super::QUIET_FRAME_OVERRIDE.with(|value| value.set(Some(mode)));
            super::QUIET_EXIT_OVERRIDE.with(|value| value.set(Some(exit)));
            crate::dom::reset_document();
            crate::jsdom::reset_bridge();
            crate::incremental_layout::invalidate();
            invalidate_retained_frame();
            crate::jsdom::set_viewport(160.0, 240.0);
            let body = crate::dom::body_id();
            let scroller = crate::dom::create_element("div");
            for (name, value) in [
                ("width", "150px"),
                ("height", "120px"),
                ("overflow", "auto"),
                ("background", "#ffffee"),
            ] {
                crate::dom::set_style_property(scroller, name, value);
            }
            for index in 0..12 {
                let line = crate::dom::create_element("p");
                crate::dom::set_style_property(
                    line,
                    "color",
                    if index % 2 == 0 { "#223388" } else { "#882233" },
                );
                let words = crate::dom::create_text_node(&format!("line {index} words"));
                crate::dom::append_child(line, words);
                crate::dom::append_child(scroller, line);
            }
            crate::dom::append_child(body, scroller);
            let tail = crate::dom::create_element("div");
            for (name, value) in [
                ("width", "40px"),
                ("height", "30px"),
                ("background", "#3366cc"),
            ] {
                crate::dom::set_style_property(tail, name, value);
            }
            crate::dom::append_child(body, tail);
            let before = super::quiet_frame_stats();
            let mut frames = vec![render_document_rgba(160, 240).expect("first frame").rgba];
            for step in 0..6 {
                // Nothing changes, except a nested scroll on step 3.
                if step == 3 {
                    crate::dom::set_scroll_offset(scroller, None, Some(40.0));
                }
                frames.push(render_document_rgba(160, 240).expect("frame").rgba);
            }
            let after = super::quiet_frame_stats();
            (frames, (after.0 - before.0, after.1 - before.1))
        };
        let content_before = crate::retained_layers::quiet_content_mismatches();
        let (verified, (quiet, mismatched)) = run(2, 0);
        assert_eq!(
            crate::retained_layers::quiet_content_mismatches(),
            content_before,
            "a quiet frame's kept content fingerprint is stale"
        );
        assert!(
            quiet > 0,
            "frames without a change must take the quiet path"
        );
        assert_eq!(mismatched, 0, "a quiet frame differs from a full paint");
        let (kept, (kept_quiet, _)) = run(1, 0);
        assert_eq!(kept_quiet, quiet);
        let (full, (off_quiet, _)) = run(0, 0);
        assert_eq!(off_quiet, 0);
        assert!(
            frames_differ_somewhere(&full),
            "the scroll must change the frame"
        );
        assert!(verified == full, "verify-mode frames differ");
        assert!(kept == full, "quiet frames differ from full paints");
        let exits = super::quiet_exit_stats();
        let (exit_verified, _) = run(1, 2);
        let verified_exits = super::quiet_exit_stats();
        assert!(
            verified_exits.0 > exits.0,
            "frames after a clean quiet frame must take the quiet exit"
        );
        assert_eq!(
            verified_exits.1, exits.1,
            "a quiet exit differs from the painted frame"
        );
        let (exited, _) = run(1, 1);
        assert!(super::quiet_exit_stats().0 > verified_exits.0);
        assert!(exit_verified == full, "quiet-exit verify frames differ");
        assert!(exited == full, "quiet-exit frames differ from full paints");
    }

    #[cfg(feature = "skia")]
    fn frames_differ_somewhere(frames: &[Vec<u8>]) -> bool {
        frames.windows(2).any(|pair| pair[0] != pair[1])
    }

    #[test]
    fn damage_composite_matches_a_full_composite() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        invalidate_retained_frame();
        let body = crate::dom::body_id();
        let mut texts = Vec::new();
        for index in 0..6 {
            let card = crate::dom::create_element("div");
            for (name, value) in [
                ("width", "140px"),
                ("background", "#eeeeff"),
                ("border-bottom", "2px solid #3366cc"),
                ("outline", "3px solid #cc3366"),
            ] {
                crate::dom::set_style_property(card, name, value);
            }
            let paragraph = crate::dom::create_element("p");
            let text = crate::dom::create_text_node(&format!("card {index} says hello"));
            crate::dom::append_child(paragraph, text);
            crate::dom::append_child(card, paragraph);
            crate::dom::append_child(body, card);
            texts.push(text);
        }
        struct Off;
        impl Drop for Off {
            fn drop(&mut self) {
                crate::render_skia::PAINT_DAMAGE_OVERRIDE.with(|value| value.set(None));
            }
        }
        let _off = Off;
        let edits: [(usize, &str); 4] = [
            (5, "card 5 says goodbye"),
            (4, "card 4 now says a much longer hello that wraps"),
            (5, "card 5"),
            (4, "card 4"),
        ];
        let (frames, damaged) = with_stable_fonts(|| {
            with_layer_reuse(true, || {
                crate::render_skia::PAINT_DAMAGE_OVERRIDE.with(|value| value.set(Some(true)));
                for (index, text) in texts.iter().enumerate() {
                    crate::dom::set_text_content(*text, &format!("card {index} says hello"));
                }
                let before = crate::render_skia::paint_damage_stats().0;
                render_document_rgba(160, 480).expect("first frame");
                let mut frames = Vec::new();
                for (card, text) in edits {
                    crate::dom::set_text_content(texts[card], text);
                    frames.push(render_document_rgba(160, 480).expect("edited frame").rgba);
                }
                (frames, crate::render_skia::paint_damage_stats().0 - before)
            })
        });
        assert!(
            damaged > 0,
            "an edit near the bottom must composite under damage"
        );
        // The same document states painted from scratch.
        crate::render_skia::PAINT_DAMAGE_OVERRIDE.with(|value| value.set(Some(false)));
        for index in 0..6 {
            crate::dom::set_text_content(texts[index], &format!("card {index} says hello"));
        }
        for (step, (card, text)) in edits.into_iter().enumerate() {
            crate::dom::set_text_content(texts[card], text);
            let fresh = with_layer_reuse(false, || {
                crate::incremental_layout::invalidate();
                invalidate_retained_frame();
                render_document_rgba(160, 480).expect("fresh frame")
            });
            assert!(
                frames[step] == fresh.rgba,
                "step {step}: a damage composite differs from a full one"
            );
        }
    }

    /// Boxes the cull band dropped are left out of the paint-order sort and
    /// the table and bidi replays: the frames equal those that replay every
    /// box, and verify mode finds the boxes reaching the layers the same.
    #[test]
    fn skip_culled_paint_nodes_matches_the_full_replay() {
        struct Off;
        impl Drop for Off {
            fn drop(&mut self) {
                SKIP_CULLED_PAINT_NODES_OVERRIDE.with(|value| value.set(None));
                crate::paint_artifact::PAINT_CULL_RECT_OVERRIDE.with(|value| value.set(None));
            }
        }
        let _off = Off;
        crate::paint_artifact::PAINT_CULL_RECT_OVERRIDE.with(|value| value.set(Some(1)));
        let run = |mode: u8| {
            SKIP_CULLED_PAINT_NODES_OVERRIDE.with(|value| value.set(Some(mode)));
            crate::dom::reset_document();
            crate::jsdom::reset_bridge();
            crate::incremental_layout::invalidate();
            invalidate_retained_frame();
            let body = crate::dom::body_id();
            let top = crate::dom::create_element("p");
            let words = crate::dom::create_text_node("top");
            crate::dom::append_child(top, words);
            crate::dom::append_child(body, top);
            // Lines of inline words (bidi runs) and a collapsed table, far
            // past the band below a 480px view.
            for index in 0..300 {
                let line = crate::dom::create_element("p");
                for word in 0..3 {
                    let span = crate::dom::create_element("span");
                    let text = crate::dom::create_text_node(&format!("w{index}.{word} "));
                    crate::dom::append_child(span, text);
                    crate::dom::append_child(line, span);
                }
                crate::dom::append_child(body, line);
            }
            let table = crate::dom::create_element("table");
            crate::dom::set_style_property(table, "border-collapse", "collapse");
            let row = crate::dom::create_element("tr");
            let cell = crate::dom::create_element("td");
            crate::dom::set_style_property(cell, "border", "2px solid #000");
            crate::dom::append_child(cell, crate::dom::create_text_node("cell"));
            crate::dom::append_child(row, cell);
            crate::dom::append_child(table, row);
            crate::dom::append_child(body, table);
            let before = SKIP_CULLED_PAINT_NODES_STATS.with(std::cell::Cell::get);
            let mut frames = vec![render_document_rgba(160, 480).expect("first frame").rgba];
            for text in ["top grows over a longer line of words", "top"] {
                crate::dom::set_text_content(words, text);
                frames.push(render_document_rgba(160, 480).expect("edited frame").rgba);
            }
            let after = SKIP_CULLED_PAINT_NODES_STATS.with(std::cell::Cell::get);
            (frames, (after.0 - before.0, after.1 - before.1))
        };
        let (verified, (skipped, mismatches)) = run(2);
        assert!(skipped > 0, "boxes outside the band must be left out");
        assert_eq!(mismatches, 0, "the boxes reaching the layers differ");
        let (skip, _) = run(1);
        let (full, (none, _)) = run(0);
        assert_eq!(none, 0);
        assert!(verified == full, "verify-mode frames differ");
        assert!(skip == full, "frames without the culled boxes differ");
    }

    /// A long column under the paint cull rect: an edit at the top moves
    /// every card below it, most of them outside the cull band, where no
    /// layer draws them. Their damage entries are placed by their property
    /// tree state, or (`W3COS_PAINT_DAMAGE_QUIET`) left out since they paint
    /// nothing in either frame; both composite under damage and equal a full
    /// paint.
    #[test]
    fn damage_of_culled_boxes_matches_a_full_composite() {
        struct Off;
        impl Drop for Off {
            fn drop(&mut self) {
                crate::render_skia::PAINT_DAMAGE_OVERRIDE.with(|value| value.set(None));
                crate::retained_layers::PAINT_DAMAGE_BLIT_OVERRIDE.with(|value| value.set(None));
                crate::retained_layers::PAINT_DAMAGE_SPANS_OVERRIDE.with(|value| value.set(None));
                crate::retained_layers::PAINT_DAMAGE_QUIET_OVERRIDE.with(|value| value.set(None));
                crate::paint_artifact::ARTIFACT_SPLICE_OVERRIDE.with(|value| value.set(None));
                crate::paint_artifact::PAINT_CULL_RECT_OVERRIDE.with(|value| value.set(None));
                crate::paint_artifact::PAINT_CULL_RUNS_OVERRIDE.with(|value| value.set(None));
                crate::incremental_layout::WARM_REBUILD_OVERRIDE.with(|value| value.set(None));
            }
        }
        let _off = Off;
        crate::retained_layers::PAINT_DAMAGE_SPANS_OVERRIDE.with(|value| value.set(Some(true)));
        crate::paint_artifact::ARTIFACT_SPLICE_OVERRIDE.with(|value| value.set(Some(true)));
        crate::paint_artifact::PAINT_CULL_RECT_OVERRIDE.with(|value| value.set(Some(1)));
        crate::paint_artifact::PAINT_CULL_RUNS_OVERRIDE.with(|value| value.set(Some(1)));
        crate::incremental_layout::WARM_REBUILD_OVERRIDE.with(|value| value.set(Some(true)));
        for quiet in [false, true] {
            crate::retained_layers::PAINT_DAMAGE_QUIET_OVERRIDE
                .with(|value| value.set(Some(quiet)));
            crate::dom::reset_document();
            crate::jsdom::reset_bridge();
            invalidate_retained_frame();
            let body = crate::dom::body_id();
            // The first card grows (and shrinks back), moving all the others.
            let first = crate::dom::create_element("div");
            crate::dom::set_style_property(first, "width", "140px");
            let words = crate::dom::create_text_node("top");
            crate::dom::append_child(first, words);
            crate::dom::append_child(body, first);
            let mut texts = Vec::new();
            // 300 cards of 24px: far past the cull band below a 480px view.
            for index in 0..300 {
                let card = crate::dom::create_element("div");
                for (name, value) in [
                    ("width", "140px"),
                    ("height", "24px"),
                    ("background", "#eeeeff"),
                ] {
                    crate::dom::set_style_property(card, name, value);
                }
                let text = crate::dom::create_text_node(&format!("card {index}"));
                crate::dom::append_child(card, text);
                crate::dom::append_child(body, card);
                texts.push(text);
            }
            let edits = [
                "top now says a much longer hello that wraps onto more lines",
                "top",
                "top grows again over two lines",
            ];
            let quiet_skips =
                || crate::retained_layers::PAINT_DAMAGE_QUIET_SKIPS.with(std::cell::Cell::get);
            let placed = || crate::render_skia::DAMAGE_CULLED_PLACED.with(std::cell::Cell::get);
            let (frames, (damaged, blit_diff, skipped, culled)) = with_stable_fonts(|| {
                with_layer_reuse(true, || {
                    crate::render_skia::PAINT_DAMAGE_OVERRIDE.with(|value| value.set(Some(true)));
                    crate::retained_layers::PAINT_DAMAGE_BLIT_OVERRIDE
                        .with(|value| value.set(Some(2)));
                    render_document_rgba(160, 480).expect("first frame");
                    let before = (
                        crate::render_skia::paint_damage_stats().0
                            + crate::render_skia::paint_damage_blit_stats().0,
                        crate::render_skia::paint_damage_blit_stats().2,
                        quiet_skips(),
                        placed(),
                    );
                    let mut frames = Vec::new();
                    for text in edits {
                        crate::dom::set_text_content(words, text);
                        frames.push(render_document_rgba(160, 480).expect("edited frame").rgba);
                    }
                    (
                        frames,
                        (
                            crate::render_skia::paint_damage_stats().0
                                + crate::render_skia::paint_damage_blit_stats().0
                                - before.0,
                            crate::render_skia::paint_damage_blit_stats().2 - before.1,
                            quiet_skips() - before.2,
                            placed() - before.3,
                        ),
                    )
                })
            });
            assert!(
                damaged > 0,
                "quiet {quiet}: no frame composited under damage"
            );
            assert_eq!(blit_diff, 0, "quiet {quiet}: a blitted frame differs");
            if quiet {
                assert!(skipped > 0, "culled moved cards must be left out");
            } else {
                assert_eq!(skipped, 0);
                assert!(culled > 0, "culled moved cards must be placed by state");
            }
            crate::render_skia::PAINT_DAMAGE_OVERRIDE.with(|value| value.set(Some(false)));
            crate::retained_layers::PAINT_DAMAGE_BLIT_OVERRIDE.with(|value| value.set(Some(0)));
            crate::dom::set_text_content(words, "top");
            for (step, text) in edits.into_iter().enumerate() {
                crate::dom::set_text_content(words, text);
                let fresh = with_layer_reuse(false, || {
                    crate::incremental_layout::invalidate();
                    invalidate_retained_frame();
                    render_document_rgba(160, 480).expect("fresh frame")
                });
                assert!(
                    frames[step] == fresh.rgba,
                    "quiet {quiet} step {step}: a damage composite differs from a full one"
                );
            }
            let _ = &texts;
        }
    }

    /// Cards below an edited top card move by whole pixels: the blit copies
    /// them from the previous frame and the result equals a full paint; with
    /// a gradient behind them the blit must give up and stay correct.
    #[test]
    fn damage_blit_matches_a_full_composite() {
        struct Off;
        impl Drop for Off {
            fn drop(&mut self) {
                crate::render_skia::PAINT_DAMAGE_OVERRIDE.with(|value| value.set(None));
                crate::retained_layers::PAINT_DAMAGE_BLIT_OVERRIDE.with(|value| value.set(None));
                crate::retained_layers::PAINT_DAMAGE_SPANS_OVERRIDE.with(|value| value.set(None));
                crate::paint_artifact::ARTIFACT_SPLICE_OVERRIDE.with(|value| value.set(None));
                crate::incremental_layout::WARM_REBUILD_OVERRIDE.with(|value| value.set(None));
            }
        }
        let _off = Off;
        // Moved spans need the damage spans and the splice (default on):
        // pin them so the lib-test modes that turn them off still check it.
        crate::retained_layers::PAINT_DAMAGE_SPANS_OVERRIDE.with(|value| value.set(Some(true)));
        crate::paint_artifact::ARTIFACT_SPLICE_OVERRIDE.with(|value| value.set(Some(true)));
        crate::incremental_layout::WARM_REBUILD_OVERRIDE.with(|value| value.set(Some(true)));
        for gradient in [false, true] {
            crate::dom::reset_document();
            crate::jsdom::reset_bridge();
            invalidate_retained_frame();
            let body = crate::dom::body_id();
            if gradient {
                crate::dom::set_style_property(
                    body,
                    "background-image",
                    "linear-gradient(#ffffff, #3366cc)",
                );
            }
            let mut texts = Vec::new();
            for index in 0..6 {
                let card = crate::dom::create_element("div");
                for (name, value) in [("width", "140px"), ("background", "#eeeeff")] {
                    crate::dom::set_style_property(card, name, value);
                }
                let paragraph = crate::dom::create_element("p");
                let text = crate::dom::create_text_node(&format!("card {index} says hello"));
                crate::dom::append_child(paragraph, text);
                crate::dom::append_child(card, paragraph);
                crate::dom::append_child(body, card);
                texts.push(text);
            }
            let edits: [(usize, &str); 4] = [
                (
                    0,
                    "card 0 now says a much longer hello that wraps onto more lines",
                ),
                (0, "card 0"),
                (1, "card 1 now says a much longer hello that wraps"),
                (1, "card 1"),
            ];
            let (frames, (blitted, gave_up, differed)) = with_stable_fonts(|| {
                with_layer_reuse(true, || {
                    crate::render_skia::PAINT_DAMAGE_OVERRIDE.with(|value| value.set(Some(true)));
                    crate::retained_layers::PAINT_DAMAGE_BLIT_OVERRIDE
                        .with(|value| value.set(Some(2)));
                    let before = crate::render_skia::paint_damage_blit_stats();
                    render_document_rgba(160, 480).expect("first frame");
                    let mut frames = Vec::new();
                    for (card, text) in edits {
                        crate::dom::set_text_content(texts[card], text);
                        frames.push(render_document_rgba(160, 480).expect("edited frame").rgba);
                    }
                    let after = crate::render_skia::paint_damage_blit_stats();
                    (
                        frames,
                        (after.0 - before.0, after.1 - before.1, after.2 - before.2),
                    )
                })
            });
            assert_eq!(differed, 0, "a blitted frame differs from a full composite");
            if gradient {
                assert_eq!(blitted, 0, "a blit over a gradient must give up");
            } else {
                assert!(
                    blitted > 0,
                    "cards moved by an edit above them must be copied"
                );
            }
            let _ = gave_up;
            crate::render_skia::PAINT_DAMAGE_OVERRIDE.with(|value| value.set(Some(false)));
            crate::retained_layers::PAINT_DAMAGE_BLIT_OVERRIDE.with(|value| value.set(Some(0)));
            for (index, text) in texts.iter().enumerate() {
                crate::dom::set_text_content(*text, &format!("card {index} says hello"));
            }
            for (step, (card, text)) in edits.into_iter().enumerate() {
                crate::dom::set_text_content(texts[card], text);
                let fresh = with_layer_reuse(false, || {
                    crate::incremental_layout::invalidate();
                    invalidate_retained_frame();
                    render_document_rgba(160, 480).expect("fresh frame")
                });
                assert!(
                    frames[step] == fresh.rgba,
                    "gradient {gradient} step {step}: a blitted frame differs from a full one"
                );
            }
        }
    }

    /// Edits to cards below the surface skip the frame (its pixels equal
    /// the previous frame's); an on-surface edit after them re-records and
    /// every frame equals a full paint. Verify mode composites the skipped
    /// frames in full and compares them with the previous frame.
    /// A card with a background color grows as its text does: with
    /// `W3COS_PAINT_RESIZE_STRIPS` its `none` image layer no longer blocks
    /// the resized-box strips, and every damage frame equals a full paint.
    #[test]
    fn resize_strips_match_a_full_composite() {
        struct Off;
        impl Drop for Off {
            fn drop(&mut self) {
                crate::render_skia::PAINT_DAMAGE_OVERRIDE.with(|value| value.set(None));
                crate::retained_layers::RESIZE_STRIPS_OVERRIDE.with(|value| value.set(None));
                crate::retained_layers::PAINT_DAMAGE_BLIT_OVERRIDE.with(|value| value.set(None));
                crate::retained_layers::PAINT_DAMAGE_SPANS_OVERRIDE.with(|value| value.set(None));
                crate::paint_artifact::ARTIFACT_SPLICE_OVERRIDE.with(|value| value.set(None));
                crate::incremental_layout::WARM_REBUILD_OVERRIDE.with(|value| value.set(None));
            }
        }
        let _off = Off;
        crate::retained_layers::PAINT_DAMAGE_SPANS_OVERRIDE.with(|value| value.set(Some(true)));
        crate::paint_artifact::ARTIFACT_SPLICE_OVERRIDE.with(|value| value.set(Some(true)));
        crate::incremental_layout::WARM_REBUILD_OVERRIDE.with(|value| value.set(Some(true)));
        let edits: [&str; 4] = [
            "card 0 says hello and keeps on talking",
            "card 0 says hello and keeps on talking until it wraps onto more lines",
            "card 0 says hello and keeps on talking until it wraps onto more lines again and again",
            "card 0",
        ];
        for blit in [0u8, 2] {
            crate::dom::reset_document();
            crate::jsdom::reset_bridge();
            invalidate_retained_frame();
            let body = crate::dom::body_id();
            let card = crate::dom::create_element("div");
            for (name, value) in [
                ("width", "140px"),
                ("padding", "12px"),
                ("background", "#eeeeff"),
            ] {
                crate::dom::set_style_property(card, name, value);
            }
            let paragraph = crate::dom::create_element("p");
            let text = crate::dom::create_text_node("card 0 says hello");
            crate::dom::append_child(paragraph, text);
            crate::dom::append_child(card, paragraph);
            crate::dom::append_child(body, card);
            // A card below moves by whole pixels as the first one grows.
            let below = crate::dom::create_element("div");
            for (name, value) in [("width", "140px"), ("background", "#ffeeee")] {
                crate::dom::set_style_property(below, name, value);
            }
            let below_text = crate::dom::create_text_node("the card below");
            crate::dom::append_child(below, below_text);
            crate::dom::append_child(body, below);
            let (frames, hits, blit_diffs) = with_stable_fonts(|| {
                crate::render_skia::PAINT_DAMAGE_OVERRIDE.with(|value| value.set(Some(true)));
                crate::retained_layers::RESIZE_STRIPS_OVERRIDE.with(|value| value.set(Some(true)));
                crate::retained_layers::PAINT_DAMAGE_BLIT_OVERRIDE
                    .with(|value| value.set(Some(blit)));
                let blit_before = crate::render_skia::paint_damage_blit_stats().2;
                let before = crate::retained_layers::RESIZE_STRIPS_NONE_IMAGES.with(Cell::get);
                render_document_rgba(180, 240).expect("first frame");
                let mut frames = Vec::new();
                for edit in edits {
                    crate::dom::set_text_content(text, edit);
                    frames.push(render_document_rgba(180, 240).expect("edited frame").rgba);
                }
                (
                    frames,
                    crate::retained_layers::RESIZE_STRIPS_NONE_IMAGES.with(Cell::get) - before,
                    crate::render_skia::paint_damage_blit_stats().2 - blit_before,
                )
            });
            assert!(
                hits > 0,
                "the card's none image layer must pass the strips check"
            );
            crate::render_skia::PAINT_DAMAGE_OVERRIDE.with(|value| value.set(Some(false)));
            crate::retained_layers::RESIZE_STRIPS_OVERRIDE.with(|value| value.set(Some(false)));
            crate::retained_layers::PAINT_DAMAGE_BLIT_OVERRIDE.with(|value| value.set(Some(0)));
            crate::dom::set_text_content(text, "card 0 says hello");
            for (step, edit) in edits.into_iter().enumerate() {
                crate::dom::set_text_content(text, edit);
                let fresh = with_stable_fonts(|| {
                    crate::incremental_layout::invalidate();
                    invalidate_retained_frame();
                    render_document_rgba(180, 240).expect("fresh frame")
                });
                assert!(
                    frames[step] == fresh.rgba,
                    "blit {blit} step {step}: a strips damage frame differs from a full paint"
                );
            }
        }
    }

    /// A paragraph whose text grows by a word per frame: with
    /// `W3COS_PAINT_NARROW_ROOTS` the unchanged words stay clean, and every
    /// damage frame (blit off and verified) equals a full paint.
    #[test]
    fn narrow_roots_match_a_full_composite() {
        struct Off;
        impl Drop for Off {
            fn drop(&mut self) {
                crate::render_skia::PAINT_DAMAGE_OVERRIDE.with(|value| value.set(None));
                crate::retained_layers::PAINT_NARROW_ROOTS_OVERRIDE.with(|value| value.set(None));
                crate::retained_layers::PAINT_DAMAGE_BLIT_OVERRIDE.with(|value| value.set(None));
                crate::retained_layers::PAINT_DAMAGE_SPANS_OVERRIDE.with(|value| value.set(None));
                crate::paint_artifact::ARTIFACT_SPLICE_OVERRIDE.with(|value| value.set(None));
                crate::incremental_layout::WARM_REBUILD_OVERRIDE.with(|value| value.set(None));
            }
        }
        let _off = Off;
        crate::retained_layers::PAINT_DAMAGE_SPANS_OVERRIDE.with(|value| value.set(Some(true)));
        crate::paint_artifact::ARTIFACT_SPLICE_OVERRIDE.with(|value| value.set(Some(true)));
        crate::incremental_layout::WARM_REBUILD_OVERRIDE.with(|value| value.set(Some(true)));
        let edits: [&str; 4] = [
            "the words of this paragraph grow",
            "the words of this paragraph grow one",
            "the words of this paragraph grow one by",
            "the words of this paragraph grow one by one",
        ];
        for blit in [0u8, 2] {
            crate::dom::reset_document();
            crate::jsdom::reset_bridge();
            invalidate_retained_frame();
            let body = crate::dom::body_id();
            let card = crate::dom::create_element("div");
            for (name, value) in [
                ("width", "160px"),
                ("padding", "12px"),
                ("background", "#eeeeff"),
            ] {
                crate::dom::set_style_property(card, name, value);
            }
            let paragraph = crate::dom::create_element("p");
            // Earlier text nodes of the paragraph stay as they are.
            for word in ["some ", "earlier ", "words "] {
                crate::dom::append_child(paragraph, crate::dom::create_text_node(word));
            }
            let text = crate::dom::create_text_node("the words of this paragraph");
            crate::dom::append_child(paragraph, text);
            crate::dom::append_child(card, paragraph);
            crate::dom::append_child(body, card);
            let below = crate::dom::create_element("div");
            crate::dom::set_style_property(below, "background", "#ffeeee");
            crate::dom::append_child(below, crate::dom::create_text_node("below"));
            crate::dom::append_child(body, below);
            let (frames, hits, blit_diffs) = with_stable_fonts(|| {
                crate::render_skia::PAINT_DAMAGE_OVERRIDE.with(|value| value.set(Some(true)));
                crate::retained_layers::PAINT_NARROW_ROOTS_OVERRIDE
                    .with(|value| value.set(Some(true)));
                crate::retained_layers::PAINT_DAMAGE_BLIT_OVERRIDE
                    .with(|value| value.set(Some(blit)));
                let blit_before = crate::render_skia::paint_damage_blit_stats().2;
                let before = crate::retained_layers::PAINT_NARROW_ROOTS_HITS.with(Cell::get);
                render_document_rgba(200, 240).expect("first frame");
                let mut frames = Vec::new();
                for edit in edits {
                    crate::dom::set_text_content(text, edit);
                    frames.push(render_document_rgba(200, 240).expect("edited frame").rgba);
                }
                (
                    frames,
                    crate::retained_layers::PAINT_NARROW_ROOTS_HITS.with(Cell::get) - before,
                    crate::render_skia::paint_damage_blit_stats().2 - blit_before,
                )
            });
            assert!(
                hits > 0,
                "a grown paragraph must keep its unchanged words clean"
            );
            assert_eq!(
                blit_diffs, 0,
                "blit {blit}: a blitted frame differs from a full one"
            );
            crate::render_skia::PAINT_DAMAGE_OVERRIDE.with(|value| value.set(Some(false)));
            crate::retained_layers::PAINT_NARROW_ROOTS_OVERRIDE
                .with(|value| value.set(Some(false)));
            crate::retained_layers::PAINT_DAMAGE_BLIT_OVERRIDE.with(|value| value.set(Some(0)));
            crate::dom::set_text_content(text, "the words of this paragraph");
            for (step, edit) in edits.into_iter().enumerate() {
                crate::dom::set_text_content(text, edit);
                let fresh = with_stable_fonts(|| {
                    crate::incremental_layout::invalidate();
                    invalidate_retained_frame();
                    render_document_rgba(200, 240).expect("fresh frame")
                });
                assert!(
                    frames[step] == fresh.rgba,
                    "blit {blit} step {step}: a narrow-roots frame differs from a full paint"
                );
            }
        }
    }

    #[test]
    fn offscreen_skip_matches_a_full_composite() {
        struct Off;
        impl Drop for Off {
            fn drop(&mut self) {
                crate::render_skia::PAINT_DAMAGE_OVERRIDE.with(|value| value.set(None));
                crate::render_skia::PAINT_OFFSCREEN_SKIP_OVERRIDE.with(|value| value.set(None));
                crate::retained_layers::PAINT_DAMAGE_SPANS_OVERRIDE.with(|value| value.set(None));
                crate::paint_artifact::ARTIFACT_SPLICE_OVERRIDE.with(|value| value.set(None));
                crate::incremental_layout::WARM_REBUILD_OVERRIDE.with(|value| value.set(None));
            }
        }
        let _off = Off;
        crate::retained_layers::PAINT_DAMAGE_SPANS_OVERRIDE.with(|value| value.set(Some(true)));
        crate::paint_artifact::ARTIFACT_SPLICE_OVERRIDE.with(|value| value.set(Some(true)));
        crate::incremental_layout::WARM_REBUILD_OVERRIDE.with(|value| value.set(Some(true)));
        let edits: [(usize, &str); 5] = [
            (10, "card 10 is edited far below the surface"),
            (11, "card 11 too"),
            (10, "card 10 again"),
            (0, "card 0 is edited on the surface"),
            (11, "card 11 once more"),
        ];
        // Also without PAINT_DAMAGE: the damage then only serves the check.
        for (mode, damage) in [(1u8, true), (2, true), (1, false), (2, false)] {
            crate::dom::reset_document();
            crate::jsdom::reset_bridge();
            invalidate_retained_frame();
            let body = crate::dom::body_id();
            let mut texts = Vec::new();
            for index in 0..12 {
                let card = crate::dom::create_element("div");
                for (name, value) in [
                    ("width", "140px"),
                    ("height", "100px"),
                    ("background", "#eeeeff"),
                ] {
                    crate::dom::set_style_property(card, name, value);
                }
                let paragraph = crate::dom::create_element("p");
                let text = crate::dom::create_text_node(&format!("card {index} says hello"));
                crate::dom::append_child(paragraph, text);
                crate::dom::append_child(card, paragraph);
                crate::dom::append_child(body, card);
                texts.push(text);
            }
            let (frames, (skipped, verified, differed)) = with_stable_fonts(|| {
                crate::render_skia::PAINT_DAMAGE_OVERRIDE.with(|value| value.set(Some(damage)));
                crate::render_skia::PAINT_OFFSCREEN_SKIP_OVERRIDE
                    .with(|value| value.set(Some(mode)));
                let before = crate::render_skia::paint_offscreen_skip_stats();
                render_document_rgba(160, 240).expect("first frame");
                let mut frames = Vec::new();
                for (card, text) in edits {
                    crate::dom::set_text_content(texts[card], text);
                    frames.push(render_document_rgba(160, 240).expect("edited frame").rgba);
                }
                let after = crate::render_skia::paint_offscreen_skip_stats();
                (
                    frames,
                    (after.0 - before.0, after.1 - before.1, after.2 - before.2),
                )
            });
            assert_eq!(
                differed, 0,
                "an off-surface frame differs from the previous one"
            );
            if mode == 1 {
                assert!(
                    skipped >= 3,
                    "off-surface edits must skip, skipped {skipped}"
                );
            } else {
                assert!(
                    verified >= 3,
                    "off-surface edits must verify, verified {verified}"
                );
            }
            crate::render_skia::PAINT_DAMAGE_OVERRIDE.with(|value| value.set(Some(false)));
            crate::render_skia::PAINT_OFFSCREEN_SKIP_OVERRIDE.with(|value| value.set(Some(0)));
            for (index, text) in texts.iter().enumerate() {
                crate::dom::set_text_content(*text, &format!("card {index} says hello"));
            }
            for (step, (card, text)) in edits.into_iter().enumerate() {
                crate::dom::set_text_content(texts[card], text);
                let fresh = with_stable_fonts(|| {
                    crate::incremental_layout::invalidate();
                    invalidate_retained_frame();
                    render_document_rgba(160, 240).expect("fresh frame")
                });
                assert!(
                    frames[step] == fresh.rgba,
                    "mode {mode} damage {damage} step {step}: a frame differs from a full paint"
                );
            }
        }
    }

    #[test]
    fn damage_early_out_matches_a_full_composite() {
        struct Off;
        impl Drop for Off {
            fn drop(&mut self) {
                crate::render_skia::PAINT_DAMAGE_OVERRIDE.with(|value| value.set(None));
                crate::render_skia::PAINT_DAMAGE_EARLY_OUT_OVERRIDE.with(|value| value.set(None));
                crate::retained_layers::PAINT_DAMAGE_BLIT_OVERRIDE.with(|value| value.set(None));
                crate::retained_layers::PAINT_DAMAGE_SPANS_OVERRIDE.with(|value| value.set(None));
                crate::paint_artifact::ARTIFACT_SPLICE_OVERRIDE.with(|value| value.set(None));
                crate::incremental_layout::WARM_REBUILD_OVERRIDE.with(|value| value.set(None));
            }
        }
        let _off = Off;
        crate::retained_layers::PAINT_DAMAGE_BLIT_OVERRIDE.with(|value| value.set(Some(0)));
        crate::retained_layers::PAINT_DAMAGE_SPANS_OVERRIDE.with(|value| value.set(Some(true)));
        crate::paint_artifact::ARTIFACT_SPLICE_OVERRIDE.with(|value| value.set(Some(true)));
        crate::incremental_layout::WARM_REBUILD_OVERRIDE.with(|value| value.set(Some(true)));
        // Card 0 growing moves every card below it: most of the frame.
        let edits: [(usize, Option<&str>, &str); 5] = [
            (1, None, "card 1 is edited"),
            (0, Some("160px"), "card 0 grows"),
            (2, None, "card 2 is edited"),
            (0, Some("100px"), "card 0 shrinks"),
            (1, None, "card 1 again"),
        ];
        for mode in [1u8, 2] {
            crate::dom::reset_document();
            crate::jsdom::reset_bridge();
            invalidate_retained_frame();
            let body = crate::dom::body_id();
            let mut cards = Vec::new();
            let mut texts = Vec::new();
            for index in 0..6 {
                let card = crate::dom::create_element("div");
                for (name, value) in [
                    ("width", "140px"),
                    ("height", "100px"),
                    ("background", "#eeeeff"),
                ] {
                    crate::dom::set_style_property(card, name, value);
                }
                let paragraph = crate::dom::create_element("p");
                let text = crate::dom::create_text_node(&format!("card {index} says hello"));
                crate::dom::append_child(paragraph, text);
                crate::dom::append_child(card, paragraph);
                crate::dom::append_child(body, card);
                cards.push(card);
                texts.push(text);
            }
            let (frames, (stopped, differed)) = with_stable_fonts(|| {
                crate::render_skia::PAINT_DAMAGE_OVERRIDE.with(|value| value.set(Some(true)));
                crate::render_skia::PAINT_DAMAGE_EARLY_OUT_OVERRIDE
                    .with(|value| value.set(Some(mode)));
                let before = crate::render_skia::paint_damage_early_out_stats();
                render_document_rgba(160, 240).expect("first frame");
                let mut frames = Vec::new();
                for (card, height, text) in edits {
                    if let Some(height) = height {
                        crate::dom::set_style_property(cards[card], "height", height);
                    }
                    crate::dom::set_text_content(texts[card], text);
                    frames.push(render_document_rgba(160, 240).expect("edited frame").rgba);
                }
                let after = crate::render_skia::paint_damage_early_out_stats();
                (frames, (after.0 - before.0, after.1 - before.1))
            });
            assert_eq!(
                differed, 0,
                "a stopped frame's damage was not most of the frame"
            );
            assert!(
                stopped >= 2,
                "growing card 0 must stop the walk, stopped {stopped}"
            );
            crate::render_skia::PAINT_DAMAGE_OVERRIDE.with(|value| value.set(Some(false)));
            crate::render_skia::PAINT_DAMAGE_EARLY_OUT_OVERRIDE.with(|value| value.set(Some(0)));
            crate::dom::set_style_property(cards[0], "height", "100px");
            for (index, text) in texts.iter().enumerate() {
                crate::dom::set_text_content(*text, &format!("card {index} says hello"));
            }
            for (step, (card, height, text)) in edits.into_iter().enumerate() {
                if let Some(height) = height {
                    crate::dom::set_style_property(cards[card], "height", height);
                }
                crate::dom::set_text_content(texts[card], text);
                let fresh = with_stable_fonts(|| {
                    crate::incremental_layout::invalidate();
                    invalidate_retained_frame();
                    render_document_rgba(160, 240).expect("fresh frame")
                });
                assert!(
                    frames[step] == fresh.rgba,
                    "mode {mode} step {step}: an early-out frame differs from a full paint"
                );
            }
        }
    }

    #[test]
    fn layer_reuse_keeps_text_overflowing_a_zero_width_block() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        invalidate_retained_frame();
        let body = crate::dom::body_id();
        let note = crate::dom::create_element("p");
        let note_text = crate::dom::create_text_node("edit me");
        crate::dom::append_child(note, note_text);
        crate::dom::append_child(body, note);
        for content in ["12345", "a b"] {
            let block = crate::dom::create_element("div");
            crate::dom::set_style_property(block, "width", "0");
            crate::dom::set_style_property(block, "font-size", "32px");
            crate::dom::append_child(block, crate::dom::create_text_node(content));
            crate::dom::append_child(body, block);
        }
        let kept = with_layer_reuse(true, || {
            render_document_rgba(200, 200).expect("first frame");
            crate::dom::set_text_content(note_text, "edited");
            render_document_rgba(200, 200).expect("second frame")
        });
        let fresh = with_layer_reuse(false, || {
            crate::incremental_layout::invalidate();
            invalidate_retained_frame();
            render_document_rgba(200, 200).expect("fresh frame")
        });
        assert_eq!(
            kept.rgba, fresh.rgba,
            "text overflowing a zero-width block must not be culled"
        );
    }

    #[test]
    fn layer_reuse_culls_far_content_and_recovers_it_on_scroll() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        invalidate_retained_frame();
        let body = crate::dom::body_id();
        crate::dom::set_style_property(body, "margin", "0");
        let scroller = crate::dom::create_element("div");
        for (name, value) in [
            ("overflow", "auto"),
            ("width", "120px"),
            ("height", "100px"),
            ("will-change", "transform"),
        ] {
            crate::dom::set_style_property(scroller, name, value);
        }
        for index in 0..120 {
            let row = crate::dom::create_element("div");
            for (name, value) in [
                ("height", "100px"),
                (
                    "background",
                    if index % 2 == 0 { "#cc3333" } else { "#3333cc" },
                ),
            ] {
                crate::dom::set_style_property(row, name, value);
            }
            crate::dom::append_child(row, crate::dom::create_text_node(&format!("row {index}")));
            crate::dom::append_child(scroller, row);
        }
        crate::dom::append_child(body, scroller);
        let note = crate::dom::create_element("p");
        let note_text = crate::dom::create_text_node("edit me");
        crate::dom::append_child(note, note_text);
        crate::dom::append_child(body, note);
        let scrolled = with_layer_reuse(true, || {
            render_document_rgba(160, 200).expect("first frame");
            crate::dom::set_scroll_offset(scroller, None, Some(9000.0));
            crate::dom::set_text_content(note_text, "edited");
            render_document_rgba(160, 200).expect("scrolled frame")
        });
        let fresh = with_layer_reuse(false, || {
            crate::incremental_layout::invalidate();
            invalidate_retained_frame();
            render_document_rgba(160, 200).expect("fresh frame")
        });
        assert_eq!(
            scrolled.rgba, fresh.rgba,
            "content culled at the first frame must be recorded once scrolled into view"
        );
    }

    #[test]
    fn absolute_top_reuses_the_paint_artifact_and_matches_a_fresh_frame() {
        crate::dom::reset_document();
        let layer = crate::dom::create_element("div");
        crate::dom::set_style_property(layer, "position", "relative");
        crate::dom::set_style_property(layer, "width", "80px");
        crate::dom::set_style_property(layer, "height", "80px");
        let box_node = crate::dom::create_element("div");
        for (name, value) in [
            ("position", "absolute"),
            ("width", "16px"),
            ("height", "16px"),
            ("left", "4px"),
            ("top", "0px"),
            ("background", "#0000ff"),
        ] {
            crate::dom::set_style_property(box_node, name, value);
        }
        crate::dom::append_child(layer, box_node);
        crate::dom::append_child(crate::dom::body_id(), layer);
        let first = render_document_rgba(64, 64).expect("first frame");
        let reuses = retained_paint_reuses();
        crate::dom::set_style_property(box_node, "top", "24px");
        let shifted = render_document_rgba(64, 64).expect("shifted frame");
        if crate::layout_ng::mode() == crate::layout_ng::Mode::Off {
            assert_eq!(
                retained_paint_reuses(),
                reuses + 1,
                "an absolute inset frame reuses the paint artifact"
            );
        }
        assert_ne!(
            first.rgba, shifted.rgba,
            "moving the box must change pixels before the artifact is reused"
        );
        crate::incremental_layout::invalidate();
        let fresh = render_document_rgba(64, 64).expect("fresh frame");
        assert_eq!(
            shifted.rgba, fresh.rgba,
            "reused artifact pixels must match a rebuilt frame"
        );
    }

    #[test]
    fn hidden_overflow_honors_programmatic_scroll_offset() {
        crate::dom::reset_document();
        let container = crate::dom::create_element("div");
        for (name, value) in [
            ("overflow", "hidden"),
            ("width", "20px"),
            ("height", "20px"),
            ("background", "red"),
        ] {
            crate::dom::set_style_property(container, name, value);
        }
        let child = crate::dom::create_element("div");
        for (name, value) in [
            ("width", "20px"),
            ("height", "20px"),
            ("margin-top", "20px"),
            ("background", "green"),
        ] {
            crate::dom::set_style_property(child, name, value);
        }
        crate::dom::append_child(container, child);
        crate::dom::append_child(crate::dom::body_id(), container);
        let before = render_document_rgba(64, 64).unwrap();
        crate::dom::set_scroll_offset(container, None, Some(20.0));
        let after = render_document_rgba(64, 64).unwrap();
        let pixel = (16 * 64 + 16) * 4;
        assert!(
            before.rgba[pixel] > before.rgba[pixel + 1],
            "unscrolled clip shows its red background"
        );
        assert!(
            after.rgba[pixel + 1] > after.rgba[pixel],
            "scrolling the hidden overflow container reveals the green child: before={:?}, after={:?}",
            &before.rgba[pixel..pixel + 4],
            &after.rgba[pixel..pixel + 4]
        );
    }
}

/// A scroll offset cannot exceed the scrollable overflow. LayoutNG computes
/// that extent itself, so an offset script set past it (`scrollLeft = 100` on
/// a box with no overflow) is clamped as a browser does. The legacy path keeps
/// applying offsets unclamped, as it always has.
fn clamp_scroll_offset_ng(x: f32, y: f32, extent: &crate::layout::ScrollExtent) -> (f32, f32) {
    if crate::layout_ng::mode() != crate::layout_ng::Mode::On {
        return (x, y);
    }
    (
        x.clamp(0.0, extent.max_x.max(0.0)),
        y.clamp(0.0, extent.max_y.max(0.0)),
    )
}

/// `W3COS_PAINT_CHANGES_VERIFY=1`: report paint clients the full paint diff
/// marks dirty or moved that the layout splice's change set does not cover.
fn paint_changes_verify() -> bool {
    // Only the check itself: `W3COS_PAINT_TOUCHED` also records the changes
    // but must not pay for this full comparison.
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("W3COS_PAINT_CHANGES_VERIFY").is_ok_and(|v| v == "1"))
}

#[cfg(feature = "skia")]
fn verify_paint_changes(
    changes: Option<&crate::incremental_layout::PaintChanges>,
    diff: &crate::retained_layers::PaintDiff,
    prev: &crate::paint_artifact::PaintArtifact,
    cur: &crate::paint_artifact::PaintArtifact,
) {
    let len = cur.nodes.len();
    let Some(changes) = changes else {
        eprintln!("[paint-changes-verify] no change set");
        return;
    };
    if changes.len != len {
        eprintln!("[paint-changes-verify] size {} vs {len}", changes.len);
        return;
    }
    let touched = &changes.touched_bitmap();
    // What the range consumers assume: a box in neither range kept its
    // rect and inputs; a box only moved kept its inputs.
    let same_inputs = |i: usize| {
        prev.nodes.get(i).is_some_and(|n| {
            n.kind == cur.nodes[i].kind
                && n.style == cur.nodes[i].style
                && n.parent == cur.nodes[i].parent
        })
    };
    let changed_box = |i: usize| crate::incremental_layout::ranges_contain(&changes.changed, i);
    let untouched_diff = (0..len)
        .filter(|&i| {
            !touched[i]
                && !(same_inputs(i) && prev.rect_by_index.get(i) == cur.rect_by_index.get(i))
        })
        .count();
    let moved_diff = (0..len)
        .filter(|&i| touched[i] && !changed_box(i) && !same_inputs(i))
        .count();
    eprintln!(
        "[paint-changes-verify] ranges changed {} moved {} untouched-diff {untouched_diff} moved-inputs-diff {moved_diff}",
        changes.changed.len(),
        changes.moved.len()
    );
    let changed: Vec<usize> = (0..len)
        .filter(|&i| {
            diff.dirty.get(i).copied().unwrap_or(true)
                || diff.moved.get(i).copied().flatten().is_some()
        })
        .collect();
    let uncovered: Vec<usize> = changed.iter().copied().filter(|&i| !touched[i]).collect();
    if std::env::var_os("W3COS_PAINT_CHANGES_DUMP").is_some() {
        for &i in uncovered.iter().take(6) {
            eprintln!(
                "[paint-changes-dump] {i} dirty {} moved {:?} parent {:?} kind_same {} style_same {} rect {:?} -> {:?} kind {:?}",
                diff.dirty[i],
                diff.moved[i],
                cur.nodes[i].parent,
                prev.nodes
                    .get(i)
                    .is_some_and(|n| n.kind == cur.nodes[i].kind),
                prev.nodes
                    .get(i)
                    .is_some_and(|n| n.style == cur.nodes[i].style),
                prev.rect_by_index.get(i),
                cur.rect_by_index.get(i),
                cur.nodes[i].kind
            );
        }
    }
    // Clients whose own inputs moved (the rest are marked by the diff's
    // propagation to their containing block or table, which paint applies
    // to the change set the same way).
    let own: Vec<usize> = uncovered
        .iter()
        .copied()
        .filter(|&i| {
            !(prev.nodes.get(i).is_some_and(|n| {
                n.kind == cur.nodes[i].kind
                    && n.style == cur.nodes[i].style
                    && n.parent == cur.nodes[i].parent
            }) && prev.rect_by_index.get(i) == cur.rect_by_index.get(i)
                && prev.text_fragments.get(i) == cur.text_fragments.get(i))
        })
        .collect();
    eprintln!(
        "[paint-changes-verify] nodes {len} touched {} changed {} uncovered {} own {} first {:?}",
        touched.iter().filter(|t| **t).count(),
        changed.len(),
        uncovered.len(),
        own.len(),
        &own[..own.len().min(8)]
    );
}
