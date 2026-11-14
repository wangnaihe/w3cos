//! Per-pass layout result and intrinsic-size caches.
//!
//! Blink keeps the last `NGLayoutResult` on each `LayoutBox` and reuses it
//! when the new `NGConstraintSpace` matches (`LayoutBox::CachedLayoutResult`),
//! and caches `MinMaxSizes` per box (`LayoutBox::IntrinsicLogicalWidths`).
//! Without them a flex item is laid out once to measure its content and
//! again at its flexed size, at every nesting level, which is exponential
//! in the depth of nested flex containers.
//!
//! This first cut keys entries on the box's index in one pass's flat tree
//! and the full constraint space, so a hit replays the exact same inputs.
//! Subtrees with grid containers are not cached: grid layout publishes
//! subgrid tracks through thread-local side channels a replay would skip.

use std::cell::RefCell;

use super::ConstraintSpace;
use super::LayoutResult;
use super::block::MinMax;
use crate::layout::FlatNodeInfo;

type Sized = Option<(f32, Option<f32>)>;

struct LayoutEntry {
    space: ConstraintSpace,
    force_bfc_root: bool,
    sized: Sized,
    result: LayoutResult,
}

struct MinMaxEntry {
    space: ConstraintSpace,
    block: Option<f32>,
    result: MinMax,
}

struct Pass {
    flat_ptr: usize,
    flat_len: usize,
    /// The box's subtree holds no grid container (cacheable).
    cacheable: Vec<bool>,
    /// Nothing in the subtree resolves a percentage against the
    /// containing block's block size (Blink's
    /// `DependsOnPercentageBlockSize`), so `percentage_block` and
    /// `fixed_block_indefinite` cannot change the result.
    block_free: Vec<bool>,
    /// Nothing in the subtree resolves a percentage against the inline
    /// size, so min/max-content sizes do not depend on `available_inline`
    /// (Blink's `MinMaxSizes` cache ignores the available inline size).
    inline_free: Vec<bool>,
    layouts: Vec<Vec<LayoutEntry>>,
    min_max: Vec<Vec<MinMaxEntry>>,
    /// Cross-flush identity of each box's layout inputs (empty when the
    /// retained cache is off): see [`fingerprints`].
    fp: Vec<u64>,
}

/// Layout results kept across flushes (Blink keeps the last
/// `NGLayoutResult` on the `LayoutBox` until `NeedsLayout` is set). A box
/// is found again by the fingerprint of its subtree's inputs, so a box
/// that moved (a streamed message pushes later ones down) still hits.
#[derive(Default)]
struct Retained {
    layouts: std::collections::HashMap<u64, (usize, Vec<LayoutEntry>)>,
    min_max: std::collections::HashMap<u64, Vec<MinMaxEntry>>,
}

thread_local! {
    static PASSES: RefCell<Vec<Pass>> = const { RefCell::new(Vec::new()) };
    static RETAINED: RefCell<Retained> = RefCell::new(Retained::default());
}

/// `W3COS_LAYOUT_NG_RETAIN=1` keeps layout results across flushes.
fn retain_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        matches!(
            std::env::var("W3COS_LAYOUT_NG_RETAIN").ok().as_deref(),
            Some("1") | Some("true")
        )
    })
}

fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        !matches!(
            std::env::var("W3COS_LAYOUT_NG_CACHE").ok().as_deref(),
            Some("0") | Some("false")
        )
    })
}

/// `W3COS_LAYOUT_NG_CACHE_STATS` prints boxes with many cache entries.
fn stats_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("W3COS_LAYOUT_NG_CACHE_STATS").is_some())
}

/// Caching is active while the returned guard lives.
pub(super) struct PassGuard(bool);

impl Drop for PassGuard {
    fn drop(&mut self) {
        if self.0 {
            PASSES.with(|passes| {
                let mut passes = passes.borrow_mut();
                let mut pass = passes.pop();
                let top_level = passes.is_empty();
                drop(passes);
                if top_level
                    && let Some(pass) = pass.as_mut()
                    && !pass.fp.is_empty()
                {
                    retain(pass);
                }
                if let Some(pass) = pass
                    && stats_enabled()
                {
                    for (i, (l, m)) in pass.layouts.iter().zip(pass.min_max.iter()).enumerate() {
                        if l.len() + m.len() > 4 {
                            eprintln!("[ng-cache] #{i} layouts {} minmax {}", l.len(), m.len());
                            for e in l.iter().take(6) {
                                eprintln!(
                                    "   L sized={:?} force={} {:?}",
                                    e.sized, e.force_bfc_root, e.space
                                );
                            }
                            for e in m.iter().take(6) {
                                eprintln!("   M block={:?} {:?}", e.block, e.space);
                            }
                        }
                    }
                }
            });
        }
    }
}

pub(super) fn begin_pass(flat: &[FlatNodeInfo]) -> PassGuard {
    if !enabled() {
        return PassGuard(false);
    }
    let mut cacheable = vec![true; flat.len()];
    let mut block_free = vec![true; flat.len()];
    let mut inline_free = vec![true; flat.len()];
    // Subtree flags: a box is clean when it and every box below it is.
    for index in (0..flat.len()).rev() {
        let (block_dep, inline_dep) = percent_dependence(&flat[index]);
        let mut ok = (
            !super::block::is_author_grid(flat[index].style),
            !block_dep,
            !inline_dep,
        );
        let end = flat[index].subtree_end.max(index + 1).min(flat.len());
        let mut child = index + 1;
        while child < end {
            ok.0 &= cacheable[child];
            ok.1 &= block_free[child];
            ok.2 &= inline_free[child];
            child = flat[child].subtree_end.max(child + 1);
        }
        cacheable[index] = ok.0;
        block_free[index] = ok.1;
        inline_free[index] = ok.2;
    }
    let nested = PASSES.with(|passes| !passes.borrow().is_empty());
    let fp = if retain_enabled() && !nested {
        take_fingerprints(flat)
            .or_else(|| fingerprints(flat))
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    PASSES.with(|passes| {
        passes.borrow_mut().push(Pass {
            flat_ptr: flat.as_ptr() as usize,
            flat_len: flat.len(),
            cacheable,
            block_free,
            inline_free,
            layouts: (0..flat.len()).map(|_| Vec::new()).collect(),
            min_max: (0..flat.len()).map(|_| Vec::new()).collect(),
            fp,
        })
    });
    PassGuard(true)
}

fn with_pass<R>(flat: &[FlatNodeInfo], index: usize, f: impl FnOnce(&mut Pass) -> R) -> Option<R> {
    PASSES.with(|passes| {
        let mut passes = passes.borrow_mut();
        let pass = passes.last_mut()?;
        if pass.flat_ptr != flat.as_ptr() as usize
            || pass.flat_len != flat.len()
            || !pass.cacheable.get(index).copied().unwrap_or(false)
        {
            return None;
        }
        Some(f(pass))
    })
}

pub(super) fn layout(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    force_bfc_root: bool,
    sized: Sized,
    compute: impl FnOnce() -> Option<LayoutResult>,
) -> Option<LayoutResult> {
    let mut key = *space;
    let hit = with_pass(flat, index, |pass| {
        key = normalize(pass, index, space, false);
        // A box laid out at a fixed inline size reads the available inline
        // size only through percentages of it (none in an inline-free
        // subtree) and its own auto inline margins (Blink
        // CalculateSizeBasedLayoutCacheStatus compares the resolved inline
        // size, not the available one).
        if sized.is_some() && pass.inline_free[index] && !auto_inline_margin(flat, index) {
            key.available_inline = 0.0;
        }
        let space = &key;
        pass.layouts[index]
            .iter()
            .find(|entry| {
                entry.force_bfc_root == force_bfc_root
                    && entry.sized == sized
                    && entry.space == *space
            })
            .map(|entry| entry.result.clone())
    });
    match hit {
        Some(Some(result)) => Some(result),
        Some(None) => {
            let result = match retained_layout(flat, index, &key, force_bfc_root, sized) {
                Some(result) => result,
                None => compute()?,
            };
            with_pass(flat, index, |pass| {
                pass.layouts[index].push(LayoutEntry {
                    space: key,
                    force_bfc_root,
                    sized,
                    result: result.clone(),
                })
            });
            Some(result)
        }
        None => compute(),
    }
}

pub(super) fn min_max(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    block: Option<f32>,
    compute: impl FnOnce() -> Option<MinMax>,
) -> Option<MinMax> {
    let mut key = *space;
    let hit = with_pass(flat, index, |pass| {
        key = normalize(pass, index, space, true);
        let space = &key;
        pass.min_max[index]
            .iter()
            .find(|entry| entry.block == block && entry.space == *space)
            .map(|entry| entry.result)
    });
    match hit {
        Some(Some(result)) => Some(result),
        Some(None) => {
            let result = match retained_min_max(flat, index, &key, block) {
                Some(result) => result,
                None => compute()?,
            };
            with_pass(flat, index, |pass| {
                pass.min_max[index].push(MinMaxEntry {
                    space: key,
                    block,
                    result,
                })
            });
            Some(result)
        }
        None => compute(),
    }
}

/// The cache key: inputs the subtree provably ignores are reset, so
/// layouts that differ only there share one entry (Blink's
/// `CalculateSizeBasedLayoutCacheStatus`).
fn normalize(pass: &Pass, index: usize, space: &ConstraintSpace, min_max: bool) -> ConstraintSpace {
    let mut key = *space;
    if pass.block_free[index] {
        key.percentage_block = None;
        key.fixed_block_indefinite = false;
    }
    if min_max && pass.inline_free[index] {
        key.available_inline = 0.0;
    }
    key
}

fn auto_inline_margin(flat: &[FlatNodeInfo], index: usize) -> bool {
    use w3cos_std::style::Spacing;
    let margin = &flat[index].style.margin;
    matches!(margin.left, Spacing::Auto) || matches!(margin.right, Spacing::Auto)
}

/// Internal markers that carry no size or percentage information.
const SIZE_FREE_MARKERS: &[&str] = &[
    "--w3cos-internal-font-size-origin",
    "--w3cos-internal-font-specified-size",
    "--w3cos-internal-list-style-image",
    "--w3cos-internal-list-item",
    "--w3cos-internal-text-fragment-ends",
    "--w3cos-internal-text-source-run",
    "--w3cos-internal-cssom-owner-ranges",
    "--w3cos-internal-inline-formatting-context",
];

/// (depends on the block-size percentage basis, depends on the inline-size
/// percentage basis) for one box. Conservative: anything not known to be
/// free counts as dependent.
fn percent_dependence(info: &FlatNodeInfo) -> (bool, bool) {
    use w3cos_std::style::{Dimension, Display, Position, Spacing, WritingMode};
    let style = info.style;
    let pct = |d: Dimension| matches!(d, Dimension::Percent(_) | Dimension::Calc { .. });
    let spacing_ok = |s: Spacing| {
        matches!(
            s,
            Spacing::Px(_)
                | Spacing::Rem(_)
                | Spacing::Em(_)
                | Spacing::Ch(_)
                | Spacing::Vw(_)
                | Spacing::Vh(_)
                | Spacing::Auto
        )
    };
    let edges_ok = |e: &w3cos_std::style::Edges| {
        spacing_ok(e.top) && spacing_ok(e.right) && spacing_ok(e.bottom) && spacing_ok(e.left)
    };
    let markers_ok = style.custom_properties.as_ref().is_none_or(|props| {
        props.keys().all(|key| {
            !key.starts_with("--w3cos-internal") || SIZE_FREE_MARKERS.contains(&key.as_str())
        })
    });
    let common = !markers_ok
        || style.logical_overrides.is_some()
        || style.writing_mode != WritingMode::HorizontalTb
        || matches!(
            style.position,
            Position::Absolute | Position::Fixed | Position::Sticky
        )
        || super::block::is_replaced(info.kind)
        || !matches!(
            style.display,
            Display::Block
                | Display::FlowRoot
                | Display::Flex
                | Display::Inline
                | Display::InlineBlock
                | Display::InlineFlex
                | Display::ListItem
                | Display::None
        );
    let block = common
        || pct(style.height)
        || pct(style.min_height)
        || pct(style.max_height)
        || pct(style.flex_basis)
        || pct(style.top)
        || pct(style.bottom);
    let inline = common
        || pct(style.width)
        || pct(style.min_width)
        || pct(style.max_width)
        || pct(style.flex_basis)
        || pct(style.left)
        || pct(style.right)
        || pct(style.text_indent)
        || style.column_count.is_some()
        || !matches!(style.column_width, Dimension::Auto)
        || !edges_ok(&style.padding)
        || !edges_ok(&style.margin);
    (block, inline)
}

/// The pass fingerprint of `index` (0 when the retained cache is off).
fn pass_fp(flat: &[FlatNodeInfo], index: usize) -> Option<u64> {
    with_pass(flat, index, |pass| pass.fp.get(index).copied())
        .flatten()
        .filter(|&fp| fp != 0)
}

/// Spaces equal for a retained replay. A result that never read the
/// parent's exclusion space (`bfc_dependent == false`) does not depend on
/// its BFC offset, so a box that only moved still matches.
fn retained_space_matches(entry: &LayoutEntry, space: &ConstraintSpace) -> bool {
    if entry.space == *space {
        return true;
    }
    if entry.result.bfc_dependent || entry.result.float_overlap {
        return false;
    }
    let mut moved = *space;
    moved.bfc_x = entry.space.bfc_x;
    moved.bfc_y = entry.space.bfc_y;
    entry.space == moved
}

fn retained_layout(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    force_bfc_root: bool,
    sized: Sized,
) -> Option<LayoutResult> {
    let fp = pass_fp(flat, index)?;
    RETAINED.with(|retained| {
        let retained = retained.borrow();
        let (base, entries) = retained.layouts.get(&fp)?;
        let entry = entries.iter().find(|entry| {
            entry.force_bfc_root == force_bfc_root
                && entry.sized == sized
                && retained_space_matches(entry, space)
        })?;
        let mut result = entry.result.clone();
        let delta = index as isize - *base as isize;
        if delta != 0 {
            shift_indices(&mut result, delta);
        }
        Some(result)
    })
}

fn retained_min_max(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    block: Option<f32>,
) -> Option<MinMax> {
    let fp = pass_fp(flat, index)?;
    RETAINED.with(|retained| {
        retained
            .borrow()
            .min_max
            .get(&fp)?
            .iter()
            .find(|entry| entry.block == block && entry.space == *space)
            .map(|entry| entry.result)
    })
}

/// Flat indices in a result taken from another flush's tree, where the box
/// sat `delta` slots away (its subtree is identical, so every index moves
/// by the same amount).
fn shift_indices(result: &mut LayoutResult, delta: isize) {
    fn shift(index: &mut usize, delta: isize) {
        *index = (*index as isize + delta) as usize;
    }
    fn walk(fragment: &mut super::BoxFragment, delta: isize) {
        shift(&mut fragment.index, delta);
        for (_, _, child) in &mut fragment.children {
            walk(child, delta);
        }
    }
    walk(&mut result.fragment, delta);
    for candidate in &mut result.oof {
        shift(&mut candidate.index, delta);
    }
}

/// Replace the retained results with the ones this top-level pass used.
fn retain(pass: &mut Pass) {
    let mut layouts = std::collections::HashMap::new();
    let mut min_max = std::collections::HashMap::new();
    for index in 0..pass.fp.len() {
        let fp = pass.fp[index];
        if fp == 0 {
            continue;
        }
        let entries = std::mem::take(&mut pass.layouts[index]);
        if !entries.is_empty() {
            layouts.insert(fp, (index, entries));
        }
        let entries = std::mem::take(&mut pass.min_max[index]);
        if !entries.is_empty() {
            min_max.insert(fp, entries);
        }
    }
    RETAINED.with(|retained| *retained.borrow_mut() = Retained { layouts, min_max });
}

/// Drop every retained result.
#[allow(dead_code)]
pub(super) fn clear_retained() {
    RETAINED.with(|retained| *retained.borrow_mut() = Retained::default());
}

fn mix(h: u64, v: u64) -> u64 {
    (h ^ v)
        .wrapping_mul(0x100_0000_01b3)
        .rotate_left(29)
        .wrapping_add(0x9e37_79b9_7f4a_7c15)
}

fn hash_bytes(mut h: u64, bytes: &[u8]) -> u64 {
    for chunk in bytes.chunks(8) {
        let mut word = [0u8; 8];
        word[..chunk.len()].copy_from_slice(chunk);
        h = mix(h, u64::from_le_bytes(word));
    }
    mix(h, bytes.len() as u64)
}

fn host_id(info: &FlatNodeInfo) -> Option<u32> {
    match info.on_click {
        w3cos_std::EventAction::NativeHost { id, .. } => u32::try_from(*id).ok(),
        _ => None,
    }
}

/// Per-box fingerprint of everything a box's layout reads besides its
/// constraint space (Blink's NeedsLayout bits, computed instead of
/// tracked): for every box of the subtree its DOM node and that node's
/// style revision, the style revision of the split inline it belongs to,
/// its kind and text, the subtree shape; plus its parent's node and style
/// revision (an item reads its container), the document, and the style
/// sheet generation. `None` when the document is borrowed mutably.
fn fingerprints(flat: &[FlatNodeInfo]) -> Option<Vec<u64>> {
    crate::dom::try_with_document(|document| {
        let salt = mix(
            mix(0x5157_4e47, document.serial()),
            w3cos_dom::stylesheet::generation(),
        );
        let revision = |id: u32| document.style_revision(w3cos_dom::NodeId::from_u32(id));
        // A box with neither a DOM node nor a split-inline owner (and not a
        // text leaf, whose style is its parent's) has a style no revision
        // tracks: it and its ancestors are not retained.
        let mut known = vec![true; flat.len()];
        let own: Vec<u64> = flat
            .iter()
            .enumerate()
            .map(|(index, info)| {
                let mut h = salt;
                let owner = info
                    .style
                    .custom_properties
                    .as_ref()
                    .and_then(|p| p.get(w3cos_dom::document::SPLIT_INLINE_FRAGMENT_OWNER))
                    .and_then(|v| v.parse::<u32>().ok());
                match host_id(info) {
                    Some(id) => {
                        h = mix(h, id as u64);
                        h = mix(h, revision(id));
                    }
                    None => {
                        h = mix(h, u64::MAX);
                        known[index] = owner.is_some()
                            || matches!(info.kind, w3cos_std::ComponentKind::Text { .. });
                    }
                }
                if let Some(owner) = owner {
                    h = mix(h, owner as u64);
                    h = mix(h, revision(owner));
                }
                let style = info.style;
                h = mix(h, style.display as u64);
                h = mix(h, style.position as u64);
                h = mix(h, style.float as u64);
                h = mix(h, {
                    use std::hash::{Hash, Hasher};
                    let mut kind = std::collections::hash_map::DefaultHasher::new();
                    std::mem::discriminant(info.kind).hash(&mut kind);
                    kind.finish()
                });
                if let w3cos_std::ComponentKind::Text { content } = info.kind {
                    h = hash_bytes(h, content.as_bytes());
                    // (A lowered text leaf carries its parent's style; a
                    // leaf built with its own keeps the fields text
                    // measure reads.)
                    h = mix(h, style.font_size.to_bits() as u64);
                    h = mix(h, style.line_height.to_bits() as u64);
                    h = mix(h, style.line_height_is_normal as u64);
                    h = mix(h, style.writing_mode as u64);
                    h = mix(h, style.white_space as u64);
                    h = mix(h, style.direction as u64);
                    h = hash_bytes(h, style.font_family.as_deref().unwrap_or("").as_bytes());
                }
                h
            })
            .collect();
        let mut subtree = vec![0u64; flat.len()];
        for index in (0..flat.len()).rev() {
            let end = flat[index].subtree_end.max(index + 1).min(flat.len());
            let mut h = mix(own[index], (end - index) as u64);
            let mut child = index + 1;
            while child < end {
                h = mix(h, subtree[child]);
                known[index] &= known[child];
                child = flat[child].subtree_end.max(child + 1);
            }
            subtree[index] = h;
        }
        (0..flat.len())
            .map(|index| {
                let parent = flat[index].parent.map_or(0, |p| own[p]);
                if known[index] {
                    mix(subtree[index], parent).max(1)
                } else {
                    0
                }
            })
            .collect()
    })
}

thread_local! {
    /// Fingerprints computed by the eligibility walk for the flat tree that
    /// `begin_pass` lays out next: (flat pointer, length, fingerprints).
    static LAST_FP: RefCell<Option<(usize, usize, Vec<u64>)>> = const { RefCell::new(None) };
    /// Fingerprints of block-level subtrees the last fully supported
    /// eligibility walk accepted.
    static SUPPORTED: RefCell<std::collections::HashSet<u64>> = RefCell::new(Default::default());
}

fn take_fingerprints(flat: &[FlatNodeInfo]) -> Option<Vec<u64>> {
    LAST_FP.with(|slot| {
        let (ptr, len, fp) = slot.borrow_mut().take()?;
        (ptr == flat.as_ptr() as usize && len == flat.len()).then_some(fp)
    })
}

/// The eligibility walk for the retained path: a block-level subtree whose
/// fingerprint the last fully supported walk accepted is skipped. `None`
/// when the retained cache is off (the caller walks every box).
pub(super) fn unsupported_tree_retained(
    flat: &[FlatNodeInfo],
    node_reason: impl Fn(usize) -> Option<&'static str>,
) -> Option<Option<&'static str>> {
    use w3cos_std::style::Display;
    if !enabled() || !retain_enabled() || PASSES.with(|p| !p.borrow().is_empty()) {
        return None;
    }
    let fp = fingerprints(flat)?;
    let block_level = |index: usize| {
        flat[index].style.display != Display::Inline
            && !matches!(flat[index].kind, w3cos_std::ComponentKind::Text { .. })
    };
    let before = SUPPORTED.with(|set| std::mem::take(&mut *set.borrow_mut()));
    let mut reason = None;
    let mut index = 0;
    while index < flat.len() {
        let info = &flat[index];
        let end = info.subtree_end.max(index + 1);
        if info.style.display == Display::None {
            index = end;
            continue;
        }
        if fp[index] != 0 && block_level(index) && before.contains(&fp[index]) {
            index = end;
            continue;
        }
        reason = node_reason(index);
        if reason.is_some() {
            break;
        }
        index += 1;
    }
    if reason.is_none() {
        let mut now = before;
        now.clear();
        let mut index = 0;
        while index < flat.len() {
            let info = &flat[index];
            if info.style.display == Display::None {
                index = info.subtree_end.max(index + 1);
                continue;
            }
            if fp[index] != 0 && block_level(index) {
                now.insert(fp[index]);
            }
            index += 1;
        }
        SUPPORTED.with(|set| *set.borrow_mut() = now);
    }
    LAST_FP.with(|slot| *slot.borrow_mut() = Some((flat.as_ptr() as usize, flat.len(), fp)));
    Some(reason)
}
