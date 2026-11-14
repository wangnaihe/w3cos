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
}

thread_local! {
    static PASSES: RefCell<Vec<Pass>> = const { RefCell::new(Vec::new()) };
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
                let pass = passes.borrow_mut().pop();
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
    PASSES.with(|passes| {
        passes.borrow_mut().push(Pass {
            flat_ptr: flat.as_ptr() as usize,
            flat_len: flat.len(),
            cacheable,
            block_free,
            inline_free,
            layouts: (0..flat.len()).map(|_| Vec::new()).collect(),
            min_max: (0..flat.len()).map(|_| Vec::new()).collect(),
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
            let result = compute()?;
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
            let result = compute()?;
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
