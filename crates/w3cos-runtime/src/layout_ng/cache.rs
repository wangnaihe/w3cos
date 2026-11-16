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

type EntryTables = (Vec<Vec<LayoutEntry>>, Vec<Vec<MinMaxEntry>>);

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
    /// Per box, filled on first read: `MARKERS_READ` plus style-marker bits
    /// (styles do not change within a pass).
    markers: Vec<u8>,
    /// The boxes holding `layouts` or `min_max` entries, each once (the
    /// rest of both tables is empty).
    touched: Vec<usize>,
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
    /// Emptied per-box entry tables of finished passes, reused by the next
    /// pass instead of allocating and dropping one slot per box.
    static TABLES: RefCell<Vec<EntryTables>> = const { RefCell::new(Vec::new()) };
    static RETAINED: RefCell<Retained> = RefCell::new(Retained::default());
}

/// Layout results are kept across flushes (Blink's CachedLayoutResult on
/// the box); `W3COS_LAYOUT_NG_RETAIN=0` turns retention off.
fn retain_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        !matches!(
            std::env::var("W3COS_LAYOUT_NG_RETAIN").ok().as_deref(),
            Some("0") | Some("false")
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
                if let Some(pass) = pass.as_ref()
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
                if let Some(pass) = pass {
                    recycle_tables(pass);
                }
            });
        }
    }
}

/// Empty `pass`'s entry tables (only its touched slots hold entries) and
/// keep them for the next pass.
fn recycle_tables(pass: Pass) {
    let Pass {
        mut layouts,
        mut min_max,
        touched,
        ..
    } = pass;
    for index in touched {
        layouts[index].clear();
        min_max[index].clear();
    }
    TABLES.with(|tables| {
        let mut tables = tables.borrow_mut();
        if tables.len() < 4 {
            tables.push((layouts, min_max));
        }
    });
}

fn entry_tables(len: usize) -> EntryTables {
    let (mut layouts, mut min_max) = TABLES
        .with(|tables| tables.borrow_mut().pop())
        .unwrap_or_default();
    layouts.truncate(len);
    layouts.resize_with(len, Vec::new);
    min_max.truncate(len);
    min_max.resize_with(len, Vec::new);
    (layouts, min_max)
}

pub(super) fn begin_pass(flat: &[FlatNodeInfo]) -> PassGuard {
    if !enabled() {
        return PassGuard(false);
    }
    let mut cacheable = vec![true; flat.len()];
    let mut block_free = vec![true; flat.len()];
    let mut inline_free = vec![true; flat.len()];
    // Subtree flags: a box is clean when it and every box below it is.
    // Flags of boxes not restyled since the last patch pass carry over.
    let hint = PATCH_HINT
        .with(|hint| hint.get())
        .filter(|h| h.flat_ptr == flat.as_ptr() as usize && h.flat_len == flat.len());
    let mut memo = hint.and_then(|h| {
        OWN_MEMO.with(|memo| {
            let mut memo = memo.borrow_mut();
            if memo
                .as_ref()
                .is_some_and(|m| m.epoch == h.epoch && m.flags.len() == flat.len())
            {
                memo.take()
            } else {
                None
            }
        })
    });
    // Box-tree mode: the subtree bits of the last pass over this tree hold
    // for every box but the reset ones and their ancestors.
    if let Some(m) = memo.as_mut()
        && m.sub.len() == flat.len()
    {
        let mut sub = std::mem::take(&mut m.sub);
        let mut chain = Vec::new();
        for index in std::mem::take(&mut m.flag_dirty) {
            let mut at = Some(index);
            while let Some(i) = at {
                chain.push(i);
                at = flat[i].parent;
            }
        }
        chain.sort_unstable_by(|a, b| b.cmp(a));
        chain.dedup();
        for index in chain {
            let flags = own_flags(flat, index, &mut m.flags[index]);
            let mut bits = (flags & 1 == 0) as u8 * SUB_CACHEABLE
                | (flags & 2 == 0) as u8 * SUB_BLOCK_FREE
                | (flags & 4 == 0) as u8 * SUB_INLINE_FREE;
            let end = flat[index].subtree_end.max(index + 1).min(flat.len());
            let mut child = index + 1;
            while child < end {
                bits &= sub[child];
                child = flat[child].subtree_end.max(child + 1);
            }
            sub[index] = bits;
        }
        for (index, bits) in sub.iter().enumerate() {
            cacheable[index] = bits & SUB_CACHEABLE != 0;
            block_free[index] = bits & SUB_BLOCK_FREE != 0;
            inline_free[index] = bits & SUB_INLINE_FREE != 0;
        }
        m.sub = sub;
    } else {
        begin_pass_flags(
            flat,
            memo.as_mut(),
            &mut cacheable,
            &mut block_free,
            &mut inline_free,
        );
    }
    if let Some(memo) = memo {
        OWN_MEMO.with(|slot| *slot.borrow_mut() = Some(memo));
    }
    let nested = PASSES.with(|passes| !passes.borrow().is_empty());
    let fp = if retain_enabled() && !nested {
        take_fingerprints(flat)
            .or_else(|| fingerprints(flat))
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let (layouts, min_max) = entry_tables(flat.len());
    PASSES.with(|passes| {
        passes.borrow_mut().push(Pass {
            flat_ptr: flat.as_ptr() as usize,
            flat_len: flat.len(),
            cacheable,
            block_free,
            inline_free,
            layouts,
            min_max,
            markers: Vec::new(),
            touched: Vec::new(),
            fp,
        })
    });
    PassGuard(true)
}

/// One box's own cache-dependence flags (`FLAGS_VALID` plus grid,
/// block-percent and inline-percent bits), kept in `slot` once read.
fn own_flags(flat: &[FlatNodeInfo], index: usize, slot: &mut u8) -> u8 {
    if *slot & FLAGS_VALID != 0 {
        return *slot;
    }
    let (block_dep, inline_dep, grid) = percent_dependence(&flat[index]);
    let flags = FLAGS_VALID | grid as u8 | (block_dep as u8) << 1 | (inline_dep as u8) << 2;
    *slot = flags;
    flags
}

/// Subtree cacheability bits of every box, bottom up (also recorded in
/// the memo, when there is one, for the next pass over the same tree).
fn begin_pass_flags(
    flat: &[FlatNodeInfo],
    mut memo: Option<&mut OwnMemo>,
    cacheable: &mut [bool],
    block_free: &mut [bool],
    inline_free: &mut [bool],
) {
    let record = memo
        .as_ref()
        .is_some_and(|m| m.flags.len() == flat.len() && box_tree_enabled());
    let mut sub = if record {
        vec![0u8; flat.len()]
    } else {
        Vec::new()
    };
    for index in (0..flat.len()).rev() {
        let own = match memo.as_mut() {
            Some(m) if m.flags.len() == flat.len() => own_flags(flat, index, &mut m.flags[index]),
            _ => own_flags(flat, index, &mut 0),
        };
        let mut ok = (own & 1 == 0, own & 2 == 0, own & 4 == 0);
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
        if record {
            sub[index] = ok.0 as u8 * SUB_CACHEABLE
                | ok.1 as u8 * SUB_BLOCK_FREE
                | ok.2 as u8 * SUB_INLINE_FREE;
        }
    }
    if let Some(m) = memo {
        m.sub = sub;
        m.flag_dirty.clear();
    }
}

const MARKERS_READ: u8 = 0x80;

/// Box `index`'s style-marker bits (`read` computes them), kept for the
/// rest of the innermost pass over `flat`.
pub(super) fn marker_bits(flat: &[FlatNodeInfo], index: usize, read: impl FnOnce() -> u8) -> u8 {
    PASSES.with(|passes| {
        let mut passes = passes.borrow_mut();
        let Some(pass) = passes
            .last_mut()
            .filter(|p| p.flat_ptr == flat.as_ptr() as usize && p.flat_len == flat.len())
        else {
            return read();
        };
        if pass.markers.len() != flat.len() {
            pass.markers = vec![0; flat.len()];
        }
        let bits = pass.markers[index];
        if bits & MARKERS_READ != 0 {
            return bits & !MARKERS_READ;
        }
        let bits = read() & !MARKERS_READ;
        pass.markers[index] = bits | MARKERS_READ;
        bits
    })
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
                if pass.layouts[index].is_empty() && pass.min_max[index].is_empty() {
                    pass.touched.push(index);
                }
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
                if pass.layouts[index].is_empty() && pass.min_max[index].is_empty() {
                    pass.touched.push(index);
                }
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
/// percentage basis, is an author grid) for one box. Conservative: anything
/// not known to be free counts as dependent. The grid and lanes markers are
/// not size-free, so with only size-free markers a grid is `display: grid`.
fn percent_dependence(info: &FlatNodeInfo) -> (bool, bool, bool) {
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
    let grid = if markers_ok {
        style.display == Display::Grid
    } else {
        super::block::is_author_grid(style)
    };
    (block, inline, grid)
}

/// The fingerprint of `index`'s subtree inputs in this top-level pass, if
/// it has one (for inputs kept across flushes outside this module).
pub(super) fn retained_fp(flat: &[FlatNodeInfo], index: usize) -> Option<u64> {
    pass_fp(flat, index)
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

/// Keep the results this top-level pass used for the next pass.
fn retain(pass: &mut Pass) {
    let mut layouts = std::collections::HashMap::new();
    let mut min_max = std::collections::HashMap::new();
    // In index order, as a scan of every box would insert them (a later
    // box with the same fingerprint replaces an earlier one).
    let mut touched = pass.touched.clone();
    touched.sort_unstable();
    for index in touched {
        let Some(&fp) = pass.fp.get(index) else {
            continue;
        };
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
    RETAINED.with(|retained| {
        let mut retained = retained.borrow_mut();
        // A pass over a small subtree (a streamed token's splice) adds its
        // results to the kept ones instead of replacing them, as
        // `retire_items` does for inline items: the rest of the document's
        // results stay valid, and dropping them freed the whole document's
        // fragments on the flush after each paint pass. A pass over the
        // whole tree used every result the next one can reuse, so it
        // replaces them. A fingerprint names its inputs, so a kept entry no
        // box has any more is only unused.
        let kept = retained.layouts.len() + retained.min_max.len();
        let fresh = layouts.len() + min_max.len();
        if pass.flat_len * 4 < kept && kept + fresh <= RETAINED_MERGE_MAX {
            retained.layouts.extend(layouts);
            retained.min_max.extend(min_max);
        } else {
            *retained = Retained { layouts, min_max };
        }
    });
    super::inline::retire_items();
}

/// Past this many retained entries a small pass replaces them instead of
/// adding to them (bounds what unused entries can hold on to).
const RETAINED_MERGE_MAX: usize = 1 << 18;

/// Drop every retained result.
#[allow(dead_code)]
pub(super) fn clear_retained() {
    RETAINED.with(|retained| *retained.borrow_mut() = Retained::default());
    super::inline::clear_items();
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

/// The flush that lays out a patched tree in place: its flat tree and the
/// structure generation it was lowered under. Only then do per-box inputs
/// carry over from the last pass (`OWN_MEMO`).
#[derive(Clone, Copy)]
struct PatchHint {
    key: (u64, u64),
    epoch: u64,
    flat_ptr: usize,
    flat_len: usize,
}

/// Per-box fingerprint inputs and percent/grid flags from the last pass
/// over a patched structure.
struct OwnMemo {
    key: (u64, u64),
    epoch: u64,
    salt: u64,
    host: Vec<Option<u32>>,
    owner: Vec<Option<u32>>,
    text: Vec<(usize, usize)>,
    own: Vec<u64>,
    known: Vec<bool>,
    /// `FLAGS_VALID` plus grid / block-percent / inline-percent bits.
    flags: Vec<u8>,
    /// Box-tree mode: subtree hashes, subtree retainability, fingerprints.
    subtree: Vec<u64>,
    known_sub: Vec<bool>,
    fp: Vec<u64>,
    /// Box-tree mode: per box, the subtree bits `begin_pass` derived from
    /// `flags` (`SUB_*`), and the boxes whose `flags` were reset since.
    sub: Vec<u8>,
    flag_dirty: Vec<usize>,
}

const FLAGS_VALID: u8 = 0x80;
const SUB_CACHEABLE: u8 = 1;
const SUB_BLOCK_FREE: u8 = 2;
const SUB_INLINE_FREE: u8 = 4;

thread_local! {
    static PATCH_HINT: std::cell::Cell<Option<PatchHint>> = const { std::cell::Cell::new(None) };
    static PATCH_EPOCH: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static OWN_MEMO: RefCell<Option<OwnMemo>> = const { RefCell::new(None) };
    /// Nodes restyled in place since the last pass recorded `OWN_MEMO`.
    static RESTYLED: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
}

/// A node's style changed in place (no re-lower).
pub(super) fn note_restyled(id: u32) {
    RESTYLED.with(|ids| {
        let mut ids = ids.borrow_mut();
        if ids.len() >= 4096 {
            ids.clear();
            OWN_MEMO.with(|memo| *memo.borrow_mut() = None);
        }
        ids.push(id);
    });
}

/// Forget carried-over per-box inputs (the tree changed some other way).
pub(super) fn forget_patch_memo() {
    OWN_MEMO.with(|memo| *memo.borrow_mut() = None);
}

/// The structure key of the patch flush over `flat`, if this is one.
pub(super) fn patch_key(flat: &[FlatNodeInfo]) -> Option<(u64, u64)> {
    PATCH_HINT
        .with(|hint| hint.get())
        .filter(|h| h.flat_ptr == flat.as_ptr() as usize && h.flat_len == flat.len())
        .map(|h| h.key)
}

/// Mark (or with `None` end) the patch flush over `flat`.
pub(super) fn set_patch_hint(flat: Option<(&[FlatNodeInfo], (u64, u64))>) {
    let hint = flat.map(|(flat, key)| {
        let epoch = PATCH_EPOCH.with(|epoch| {
            epoch.set(epoch.get() + 1);
            epoch.get()
        });
        PatchHint {
            key,
            epoch,
            flat_ptr: flat.as_ptr() as usize,
            flat_len: flat.len(),
        }
    });
    PATCH_HINT.with(|slot| slot.set(hint));
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
            mix(
                mix(0x5157_4e47, document.serial()),
                w3cos_dom::stylesheet::generation(),
            ),
            // Web fonts that finish loading change text metrics.
            crate::font_face::FontRegistry::global().revision(),
        );
        let revision = |id: u32| document.style_revision(w3cos_dom::NodeId::from_u32(id));
        // A box with neither a DOM node nor a split-inline owner (and not a
        // text leaf, whose style is its parent's) has a style no revision
        // tracks: it and its ancestors are not retained.
        let hint = PATCH_HINT.with(|hint| hint.get());
        let hinted =
            hint.filter(|h| h.flat_ptr == flat.as_ptr() as usize && h.flat_len == flat.len());
        let mut memo = if hinted.is_some() {
            OWN_MEMO.with(|memo| memo.borrow_mut().take())
        } else {
            None
        }
        .filter(|memo| {
            Some(memo.key) == hinted.map(|h| h.key)
                && memo.salt == salt
                && memo.own.len() == flat.len()
        });
        if let (Some(m), Some(h)) = (memo.as_mut(), hinted)
            && box_tree_enabled()
            && m.fp.len() == flat.len()
        {
            let ids: Vec<u32> = RESTYLED.with(|ids| ids.borrow().clone());
            if ids.len() <= 8 {
                // Same structure key: the tree is the one the memo saw, with
                // only the restyled nodes' styles patched in place.
                let fp = patch_fingerprints(flat, m, &ids, salt, &revision);
                m.epoch = h.epoch;
                RESTYLED.with(|ids| ids.borrow_mut().clear());
                let memo = memo.take();
                OWN_MEMO.with(|slot| *slot.borrow_mut() = memo);
                return fp;
            }
        }
        let dirty: std::collections::HashSet<u32, w3cos_std::fxhash::FxBuildHasher> =
            if memo.is_some() {
                RESTYLED.with(|ids| ids.borrow().iter().copied().collect())
            } else {
                Default::default()
            };
        let text_key = |info: &FlatNodeInfo| match info.kind {
            w3cos_std::ComponentKind::Text { content } => {
                (content.as_ptr() as usize, content.len())
            }
            _ => (0, 0),
        };
        let mut known = vec![true; flat.len()];
        let mut own = vec![0u64; flat.len()];
        let mut next = OwnMemo {
            key: hinted.map_or((0, 0), |h| h.key),
            epoch: hinted.map_or(0, |h| h.epoch),
            salt,
            host: Vec::with_capacity(flat.len()),
            owner: Vec::with_capacity(flat.len()),
            text: Vec::with_capacity(flat.len()),
            own: Vec::new(),
            known: Vec::new(),
            flags: memo
                .as_mut()
                .map(|m| std::mem::take(&mut m.flags))
                .unwrap_or_else(|| vec![0; flat.len()]),
            subtree: Vec::new(),
            known_sub: Vec::new(),
            fp: Vec::new(),
            sub: Vec::new(),
            flag_dirty: Vec::new(),
        };
        for (index, info) in flat.iter().enumerate() {
            let host = host_id(info);
            let text = text_key(info);
            // Blink's NeedsLayout, tracked instead of recomputed: a box
            // whose node was not restyled since the last pass over this
            // structure keeps its inputs.
            if let Some(m) = memo.as_ref()
                && m.host[index] == host
                && m.text[index] == text
                && host.is_none_or(|id| !dirty.contains(&id))
                && m.owner[index].is_none_or(|id| !dirty.contains(&id))
            {
                own[index] = m.own[index];
                known[index] = m.known[index];
                next.owner.push(m.owner[index]);
            } else {
                let (h, ok, owner) = own_input(flat, index, salt, &revision);
                own[index] = h;
                known[index] = ok;
                next.owner.push(owner);
                next.flags[index] = 0;
            }
            next.host.push(host);
            next.text.push(text);
        }
        let keep = hinted.is_some();
        if keep {
            next.own = own.clone();
            next.known = known.clone();
        }
        let (subtree, known_sub, fp) = subtree_fingerprints(flat, &own, known);
        if keep {
            RESTYLED.with(|ids| ids.borrow_mut().clear());
            if box_tree_enabled() {
                next.subtree = subtree;
                next.known_sub = known_sub;
                next.fp = fp.clone();
            }
            OWN_MEMO.with(|slot| *slot.borrow_mut() = Some(next));
        } else if hint.is_none() {
            RESTYLED.with(|ids| ids.borrow_mut().clear());
            OWN_MEMO.with(|slot| *slot.borrow_mut() = None);
        }
        fp
    })
}

/// Owned box tree (LayoutNG on; `W3COS_NG_BOX_TREE=0` opts out): per-box
/// derived data kept across patch flushes and updated only on restyled
/// boxes and their ancestors, as Blink keeps its LayoutObject tree.
pub(crate) fn box_tree_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        super::mode() == super::Mode::On
            && !matches!(
                std::env::var("W3COS_NG_BOX_TREE").ok().as_deref(),
                Some("0") | Some("false")
            )
    })
}

/// A box's own fingerprint input: (hash, retainable, the node other than
/// its own whose style revision it reads: a split-inline owner, or the
/// list item of an outside marker).
fn own_input(
    flat: &[FlatNodeInfo],
    index: usize,
    salt: u64,
    revision: &impl Fn(u32) -> u64,
) -> (u64, bool, Option<u32>) {
    let info = &flat[index];
    let mut h = salt;
    let mut ok = true;
    let properties = info.style.custom_properties.as_ref();
    let owner = properties
        .and_then(|p| p.get(w3cos_dom::document::SPLIT_INLINE_FRAGMENT_OWNER))
        .and_then(|v| v.parse::<u32>().ok())
        .or_else(|| {
            // An outside `::marker` box has no node of its own. Its style is
            // generated from its list item's computed style and the
            // `list-style` declarations the item's style revision tracks
            // (an ancestor's style edit invalidates the item's subtree).
            let kind = properties?.get("--w3cos-internal-outside-list-marker")?;
            if host_id(info).is_some()
                || !matches!(info.kind, w3cos_std::ComponentKind::Box)
                || kind == "image"
            {
                return None;
            }
            host_id(&flat[info.parent?])
        });
    // Replaced and form content reads state no style revision
    // tracks (an image's natural size arrives when it decodes).
    if !matches!(
        info.kind,
        w3cos_std::ComponentKind::Root
            | w3cos_std::ComponentKind::Column
            | w3cos_std::ComponentKind::Row
            | w3cos_std::ComponentKind::Box
            | w3cos_std::ComponentKind::Text { .. }
    ) {
        ok = false;
    }
    match host_id(info) {
        Some(id) => {
            h = mix(h, id as u64);
            h = mix(h, revision(id));
        }
        None => {
            h = mix(h, u64::MAX);
            ok &= owner.is_some() || matches!(info.kind, w3cos_std::ComponentKind::Text { .. });
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
        let mut kind = w3cos_std::fxhash::FxHasher::default();
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
    (h, ok, owner)
}

/// One box's subtree hash and whether its whole subtree is retainable.
fn subtree_entry(
    flat: &[FlatNodeInfo],
    index: usize,
    own: &[u64],
    known: &[bool],
    subtree: &[u64],
    known_sub: &[bool],
) -> (u64, bool) {
    let end = flat[index].subtree_end.max(index + 1).min(flat.len());
    let mut h = mix(own[index], (end - index) as u64);
    let mut ok = known[index];
    let mut child = index + 1;
    while child < end {
        h = mix(h, subtree[child]);
        ok &= known_sub[child];
        child = flat[child].subtree_end.max(child + 1);
    }
    (h, ok)
}

fn fingerprint_entry(
    flat: &[FlatNodeInfo],
    index: usize,
    own: &[u64],
    subtree: &[u64],
    known_sub: &[bool],
) -> u64 {
    let parent = flat[index].parent.map_or(0, |p| own[p]);
    if known_sub[index] {
        mix(subtree[index], parent).max(1)
    } else {
        0
    }
}

/// Subtree hashes, subtree retainability, and fingerprints of every box.
fn subtree_fingerprints(
    flat: &[FlatNodeInfo],
    own: &[u64],
    known: Vec<bool>,
) -> (Vec<u64>, Vec<bool>, Vec<u64>) {
    let mut subtree = vec![0u64; flat.len()];
    let mut known_sub = known;
    for index in (0..flat.len()).rev() {
        let (h, ok) = subtree_entry(flat, index, own, &known_sub, &subtree, &known_sub);
        subtree[index] = h;
        known_sub[index] = ok;
    }
    let fp = (0..flat.len())
        .map(|index| fingerprint_entry(flat, index, own, &subtree, &known_sub))
        .collect();
    (subtree, known_sub, fp)
}

/// Fingerprints after restyling `ids` in place: recompute the own inputs of
/// their boxes, the subtree entries of those boxes' ancestors, and the
/// fingerprints that read either (Blink's containing-block chain marking).
fn patch_fingerprints(
    flat: &[FlatNodeInfo],
    m: &mut OwnMemo,
    ids: &[u32],
    salt: u64,
    revision: &impl Fn(u32) -> u64,
) -> Vec<u64> {
    let mut dirty = Vec::new();
    if !ids.is_empty() {
        for index in 0..flat.len() {
            if m.host[index].is_some_and(|id| ids.contains(&id))
                || m.owner[index].is_some_and(|id| ids.contains(&id))
            {
                dirty.push(index);
            }
        }
    }
    let mut chain = Vec::new();
    let mut refresh = Vec::new();
    for &index in &dirty {
        let (h, ok, owner) = own_input(flat, index, salt, revision);
        m.owner[index] = owner;
        m.flags[index] = 0;
        m.flag_dirty.push(index);
        if m.own[index] == h && m.known[index] == ok {
            continue;
        }
        m.own[index] = h;
        m.known[index] = ok;
        let mut at = Some(index);
        while let Some(i) = at {
            chain.push(i);
            at = flat[i].parent;
        }
        // Children read their parent's own input.
        let end = flat[index].subtree_end.max(index + 1).min(flat.len());
        let mut child = index + 1;
        while child < end {
            refresh.push(child);
            child = flat[child].subtree_end.max(child + 1);
        }
    }
    chain.sort_unstable_by(|a, b| b.cmp(a));
    chain.dedup();
    for &index in &chain {
        let (h, ok) = subtree_entry(flat, index, &m.own, &m.known, &m.subtree, &m.known_sub);
        m.subtree[index] = h;
        m.known_sub[index] = ok;
    }
    for &index in chain.iter().chain(&refresh) {
        m.fp[index] = fingerprint_entry(flat, index, &m.own, &m.subtree, &m.known_sub);
    }
    m.fp.clone()
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
    let mut before = SUPPORTED.with(|set| std::mem::take(&mut *set.borrow_mut()));
    let mut reason = None;
    let mut index = 0;
    // Block-level boxes this walk checked itself (not skipped as accepted).
    let mut walked = Vec::new();
    // `node_reason` reads subtree fingerprints through `walk_fingerprint`.
    let fp_slot = WalkFingerprints::install(flat, fp);
    let fp = &fp_slot;
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
        if fp[index] != 0 && block_level(index) {
            walked.push(fp[index]);
        }
        index += 1;
    }
    // A fingerprint names a subtree's inputs, so an accepted one stays
    // accepted: add what this walk checked instead of re-inserting every
    // box. Rebuild when stale entries outgrow the tree.
    if reason.is_none() && !before.is_empty() && before.len() + walked.len() <= 2 * flat.len() {
        before.extend(walked);
        SUPPORTED.with(|set| *set.borrow_mut() = before);
    } else if reason.is_none() {
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
    let fp = fp_slot.take();
    LAST_FP.with(|slot| *slot.borrow_mut() = Some((flat.as_ptr() as usize, flat.len(), fp)));
    Some(reason)
}

thread_local! {
    /// The fingerprints of the flat tree an eligibility walk is checking.
    static WALK_FP: RefCell<Option<(usize, usize, std::rc::Rc<Vec<u64>>)>> =
        const { RefCell::new(None) };
    /// Subtree fingerprints known to hold no rendered float.
    static FLOAT_FREE: RefCell<std::collections::HashSet<u64, w3cos_std::fxhash::FxBuildHasher>> =
        RefCell::new(Default::default());
}

/// The walk's fingerprints, published to `walk_fingerprint` while it lives.
struct WalkFingerprints(std::rc::Rc<Vec<u64>>);

impl WalkFingerprints {
    fn install(flat: &[FlatNodeInfo], fp: Vec<u64>) -> Self {
        let fp = std::rc::Rc::new(fp);
        WALK_FP.with(|slot| {
            *slot.borrow_mut() = Some((flat.as_ptr() as usize, flat.len(), fp.clone()))
        });
        Self(fp)
    }

    fn take(self) -> Vec<u64> {
        WALK_FP.with(|slot| *slot.borrow_mut() = None);
        let mut this = self;
        let fp = std::mem::take(&mut this.0);
        std::rc::Rc::try_unwrap(fp).unwrap_or_else(|fp| (*fp).clone())
    }
}

impl std::ops::Deref for WalkFingerprints {
    type Target = [u64];
    fn deref(&self) -> &[u64] {
        &self.0
    }
}

impl Drop for WalkFingerprints {
    fn drop(&mut self) {
        WALK_FP.with(|slot| {
            if let Ok(mut slot) = slot.try_borrow_mut() {
                *slot = None;
            }
        });
    }
}

/// Box `index`'s subtree fingerprint during an eligibility walk over
/// `flat` (it names every input of the subtree, its floats included).
fn walk_fingerprint(flat: &[FlatNodeInfo], index: usize) -> Option<u64> {
    WALK_FP.with(|slot| {
        let slot = slot.borrow();
        let (ptr, len, fp) = slot.as_ref()?;
        (*ptr == flat.as_ptr() as usize && *len == flat.len())
            .then(|| fp.get(index).copied())
            .flatten()
            .filter(|fp| *fp != 0)
    })
}

/// `scan(flat, index)` (whether the subtree holds a rendered float),
/// remembered by subtree fingerprint across eligibility walks: a retained
/// walk re-checks an ancestor flex container whose items did not change.
pub(super) fn subtree_has_float(
    flat: &[FlatNodeInfo],
    index: usize,
    scan: impl FnOnce() -> bool,
) -> bool {
    let fp = walk_fingerprint(flat, index);
    if let Some(fp) = fp
        && FLOAT_FREE.with(|set| set.borrow().contains(&fp))
    {
        return false;
    }
    let has = scan();
    if !has && let Some(fp) = fp {
        FLOAT_FREE.with(|set| {
            let mut set = set.borrow_mut();
            if set.len() > 2 * flat.len() + 1024 {
                set.clear();
            }
            set.insert(fp);
        });
    }
    has
}
