//! LayoutNG-style layout core.
//!
//! Structure follows Blink LayoutNG (third_party/blink/renderer/core/layout/,
//! BSD-style license, Copyright The Chromium Authors): one layout algorithm
//! per formatting context, each a function
//! `layout(node, &ConstraintSpace) -> LayoutResult` producing a box fragment
//! with child offsets; a margin strut carries collapsible margins between
//! siblings and through parents (NGMarginStrut).  Positions are made absolute
//! only when the fragment tree is flattened into the engine's
//! `(LayoutRect, flat index)` output, so paint, hit-testing and the CSSOM
//! geometry getters are unchanged.
//!
//! Rollout: LayoutNG is the default. An unset `W3COS_LAYOUT_NG` uses this
//! engine for trees it fully supports and falls back to the legacy Taffy +
//! projection engine otherwise. `W3COS_LAYOUT_NG=1` is the same mode.
//! `W3COS_LAYOUT_NG=0` is the legacy escape hatch. `W3COS_LAYOUT_NG_VERIFY=1`
//! (any value other than `0`) runs both on supported trees, keeps the legacy
//! output, and reports every rect that differs. The choice is read once per
//! process and then latched.
//!
//! Stage S1: block flow without floats -- block-level boxes in normal flow,
//! margin collapsing (siblings, parent/first-child, parent/last-child,
//! empty boxes), auto/px/percent widths and heights, min/max sizes,
//! box-sizing, auto horizontal margins.  Stage S3b adds block-level floats
//! and clearance (exclusion.rs); text beside a float is not supported yet.
//! Anything else makes
//! `unsupported_tree` report a reason and the legacy engine runs.

mod block;
mod box_scan;
mod cache;
mod columns;
mod exclusion;
mod flex;
mod grid;
mod inline;
mod oof;
mod orthogonal;
mod table;

use crate::layout::{FlatNodeInfo, LayoutRect};
use std::sync::OnceLock;
use w3cos_std::{Component, ComponentKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Off,
    On,
    Verify,
}

/// Resolve the layout engine from the two environment values.
///
/// `verify` other than `0` wins (including when `ng` is `0`). An unset `ng`
/// selects NG, the same as `1` or any value other than `0`. Only `ng=0`
/// with verify unset or `0` stays on the legacy engine. This is the single
/// decision `mode` latches; `w3cos_dom::document::layout_ng_requested_from_env`
/// must accept the same inputs.
pub(crate) fn mode_from_env(
    ng: Option<&std::ffi::OsStr>,
    verify: Option<&std::ffi::OsStr>,
) -> Mode {
    let enabled = |value: &std::ffi::OsStr| value != "0";
    if verify.is_some_and(enabled) {
        Mode::Verify
    } else if ng.is_none_or(enabled) {
        Mode::On
    } else {
        Mode::Off
    }
}

/// A node's style was patched in place (see `cache::note_restyled`).
pub(crate) fn note_restyled(id: u32) {
    cache::note_restyled(id);
    if cache::box_tree_enabled() {
        crate::list_marker::note_restyled(id);
        crate::incremental_layout::note_client_restyled(id);
        box_scan::note_restyled(id);
    }
}

/// The tree was edited other than by an in-place restyle.
pub(crate) fn forget_patch_memo() {
    cache::forget_patch_memo();
    crate::list_marker::forget_retained();
    crate::incremental_layout::forget_client_plan();
    crate::incremental_layout::forget_style_paths();
    box_scan::forget();
    forget_flat_reuse();
    crate::layout::forget_flat_skeleton();
    crate::layout::forget_scroll_plan();
}

/// Box-tree mode: the restyle-log position a memo kept under
/// `box_tree_key` records.
pub(crate) fn restyle_position() -> u64 {
    box_scan::log_position()
}

/// Box-tree mode: the boxes a restyle since `seq` can have changed.
pub(crate) fn restyled_boxes_since(
    flat: &[crate::layout::FlatNodeInfo],
    key: (u64, u64),
    seq: u64,
) -> Option<Vec<usize>> {
    box_scan::changed_since(flat, key, seq)
}

pub(crate) fn box_tree_enabled() -> bool {
    cache::box_tree_enabled()
}

/// Box-tree mode: the structure key of the patch flush laying out `flat`
/// (per-box data kept under it stays valid but for restyled boxes).
pub(crate) fn box_tree_key(flat: &[crate::layout::FlatNodeInfo]) -> Option<(u64, u64)> {
    cache::box_tree_enabled()
        .then(|| cache::patch_key(flat))
        .flatten()
}

/// Mark (or with `None` end) the flush laying out patched `flat` under
/// structure `key`.
pub(crate) fn set_patch_hint(flat: Option<(&[crate::layout::FlatNodeInfo], (u64, u64))>) {
    cache::set_patch_hint(flat);
}

pub(crate) fn mode() -> Mode {
    static MODE: OnceLock<Mode> = OnceLock::new();
    *MODE.get_or_init(|| {
        let ng = std::env::var_os("W3COS_LAYOUT_NG");
        let verify = std::env::var_os("W3COS_LAYOUT_NG_VERIFY");
        mode_from_env(ng.as_deref(), verify.as_deref())
    })
}

/// Stable name for harness reports: `off`, `on`, or `verify`.
pub(crate) fn mode_name() -> &'static str {
    match mode() {
        Mode::Off => "off",
        Mode::On => "on",
        Mode::Verify => "verify",
    }
}

/// `W3COS_LAYOUT_NG_TRACE=1`: log whether each compute used NG or fell back
/// (coverage accounting for the WPT gate).
///
/// Compiled only with the `layout-ng-trace` feature (the WPT runner enables
/// it) and in unit tests; other builds drop the trace code and the fallback
/// reason strings.
#[cfg(any(test, feature = "layout-ng-trace"))]
fn trace() -> bool {
    static TRACE: OnceLock<bool> = OnceLock::new();
    *TRACE.get_or_init(|| std::env::var_os("W3COS_LAYOUT_NG_TRACE").is_some_and(|v| v != "0"))
}

#[cfg(not(any(test, feature = "layout-ng-trace")))]
#[inline(always)]
fn trace() -> bool {
    false
}

/// NGConstraintSpace: the input of every layout algorithm.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ConstraintSpace {
    /// Available inline size (the containing block's content width).
    pub available_inline: f32,
    /// Definite containing-block block size, if any (percent heights).
    pub percentage_block: Option<f32>,
    pub viewport_w: f32,
    pub viewport_h: f32,
    /// The containing block's direction is rtl: CSS2 10.3.3 resolves auto
    /// and over-constrained inline margins from its inline-start (right)
    /// edge (Blink ResolveInlineAutoMargins with the container style).
    pub rtl: bool,
    /// NGBfcOffset: the estimated offset of the box's border box from its
    /// block formatting context root's border box (S3b).  Floats and
    /// clearance are resolved in these coordinates.
    pub bfc_x: f32,
    pub bfc_y: f32,
    /// NGConstraintSpace::MarginStrut: the margins adjoining our top edge
    /// from before us (excluding our own), and the BFC block offset they
    /// start at -- the hypothetical position of a first child whose
    /// margins collapse through us (CSS2 9.5.2 clearance).
    pub margin_base: f32,
    pub margin_strut: MarginStrut,
    /// NGConstraintSpace::ForcedBfcBlockOffset: the box was placed past
    /// clearance, so its BFC block offset is resolved at its border box
    /// (`bfc_y`) and nothing collapses through its top any more.
    pub forced_bfc: bool,
    /// Blink's adjoining object types: the exclusion-space index from
    /// which floats were placed while our BFC block offset is still
    /// unresolved (the margin strut that started at `margin_base`).
    pub adjoining_from: usize,
    /// Blink's `IsFixedBlockSizeIndefinite`: the box is laid out at a
    /// fixed block size (a column flex item's flexed main size) that is
    /// not definite for its children's percentages, because the flex
    /// container's main size is indefinite (css-flexbox "definite sizes").
    pub fixed_block_indefinite: bool,
    /// An intrinsic-size measurement (min/max-content of an inline
    /// formatting context): `available_inline` is a probe (0 or
    /// indefinite), not a containing-block size, so inline percentages of
    /// the boxes laid out against it are cyclic. They resolve as zero
    /// (margins, padding), auto (width) and none (min/max-width), CSS
    /// Sizing 3 5.2.1; the final layout re-resolves them against the real
    /// containing block.
    pub percent_unresolved: bool,
    /// Grid track sizing (and the layout of grid items in their areas): a
    /// replaced box's percentage block size with no definite basis is
    /// cyclic and behaves as `auto` (CSS Grid 6.6, CSS Sizing 3 5.2.2).
    /// Inherited by descendants, but reset by flex layout, whose
    /// flex-basis measurement resolves such percentages later.
    pub cyclic_percent_auto: bool,
    /// Remaining line budget for `line-clamp` / `continue: collapse` in
    /// this BFC, propagated to same-BFC block children (CSS Overflow 4).
    /// Independent formatting-context children do not inherit it.
    pub line_clamp: Option<LineClampState>,
}

/// Remaining clamp budget in the current line-clamp BFC, and whether the
/// last kept line gets a block-ellipsis. Integer `line-clamp` uses a
/// line-box count; `line-clamp: auto` / `continue: collapse` uses a
/// remaining content-box block size so descendant padding, border, and
/// margin consume the cap (CSS Overflow 4).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct LineClampState {
    pub remaining: Option<usize>,
    pub remaining_block: Option<f32>,
    pub ellipsis: bool,
}

/// NGMarginStrut: the positive and negative extremes of adjoining margins.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct MarginStrut {
    positive: f32,
    negative: f32,
}

impl MarginStrut {
    pub fn append(&mut self, margin: f32) {
        if margin >= 0.0 {
            self.positive = self.positive.max(margin);
        } else {
            self.negative = self.negative.min(margin);
        }
    }

    pub fn merge(&mut self, other: MarginStrut) {
        self.positive = self.positive.max(other.positive);
        self.negative = self.negative.min(other.negative);
    }

    pub fn sum(&self) -> f32 {
        self.positive + self.negative
    }
}

/// NGPhysicalBoxFragment (logical == physical in S1: horizontal-tb, ltr).
#[derive(Debug, Clone)]
pub(crate) struct BoxFragment {
    pub index: usize,
    pub width: f32,
    pub height: f32,
    /// A text fragment covering part of its leaf's text: the byte range
    /// (NGPhysicalTextFragment start/end offsets).  `None` is a whole node.
    pub text_range: Option<(usize, usize)>,
    /// The piece ends a `-webkit-line-clamp` box: paint draws its text with
    /// a block ellipsis (U+2026) after it, trailing breaks removed.
    pub ellipsis: bool,
    /// A text leaf of an orthogonal (vertical) flow: its rect is physical
    /// and paint rotates the glyphs a quarter turn clockwise (sideways
    /// glyphs of `text-orientation: mixed`).
    pub vertical: bool,
    /// Children with their border-box offset from this border box.
    pub children: FragmentChildren,
}

/// A fragment's children, shared between copies (Blink's
/// NGPhysicalFragment is ref-counted and immutable once built: a cached
/// NGLayoutResult hands out the same fragment tree). Copy-on-write: a
/// mutable borrow of a shared list copies one level.
#[derive(Debug, Clone)]
pub(crate) struct FragmentChildren(std::rc::Rc<Vec<(f32, f32, BoxFragment)>>);

impl Default for FragmentChildren {
    /// Leaf fragments share one empty list (no allocation per leaf; the
    /// first push copies it like any shared list).
    fn default() -> Self {
        thread_local! {
            static EMPTY: std::rc::Rc<Vec<(f32, f32, BoxFragment)>> =
                std::rc::Rc::new(Vec::new());
        }
        Self(EMPTY.with(std::rc::Rc::clone))
    }
}

impl std::ops::Deref for FragmentChildren {
    type Target = Vec<(f32, f32, BoxFragment)>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for FragmentChildren {
    fn deref_mut(&mut self) -> &mut Self::Target {
        std::rc::Rc::make_mut(&mut self.0)
    }
}

impl From<Vec<(f32, f32, BoxFragment)>> for FragmentChildren {
    fn from(children: Vec<(f32, f32, BoxFragment)>) -> Self {
        Self(std::rc::Rc::new(children))
    }
}

impl FromIterator<(f32, f32, BoxFragment)> for FragmentChildren {
    fn from_iter<I: IntoIterator<Item = (f32, f32, BoxFragment)>>(iter: I) -> Self {
        Self(std::rc::Rc::new(iter.into_iter().collect()))
    }
}

impl FragmentChildren {
    /// The list by value (moved out when unshared).
    pub fn into_vec(self) -> Vec<(f32, f32, BoxFragment)> {
        std::rc::Rc::try_unwrap(self.0).unwrap_or_else(|shared| (*shared).clone())
    }
}

impl IntoIterator for FragmentChildren {
    type Item = (f32, f32, BoxFragment);
    type IntoIter = std::vec::IntoIter<(f32, f32, BoxFragment)>;
    fn into_iter(self) -> Self::IntoIter {
        self.into_vec().into_iter()
    }
}

impl<'a> IntoIterator for &'a FragmentChildren {
    type Item = &'a (f32, f32, BoxFragment);
    type IntoIter = std::slice::Iter<'a, (f32, f32, BoxFragment)>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<'a> IntoIterator for &'a mut FragmentChildren {
    type Item = &'a mut (f32, f32, BoxFragment);
    type IntoIter = std::slice::IterMut<'a, (f32, f32, BoxFragment)>;
    fn into_iter(self) -> Self::IntoIter {
        std::rc::Rc::make_mut(&mut self.0).iter_mut()
    }
}

/// The last line box's baseline (NGBoxFragment::LastBaseline), as an
/// offset from the border-box top.  `Unknown` where paint lays out the
/// lines (a wrapped paragraph leaf): an inline-block depending on it falls
/// back.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum LastBaseline {
    NoLines,
    At(f32),
    Unknown,
}

impl LastBaseline {
    pub fn offset(self, by: f32) -> Self {
        match self {
            LastBaseline::At(b) => LastBaseline::At(b + by),
            other => other,
        }
    }
}

/// NGLayoutResult.
#[derive(Debug, Clone)]
pub(crate) struct LayoutResult {
    pub fragment: BoxFragment,
    pub last_baseline: LastBaseline,
    /// First in-flow line box (CSS 2.1 17.5.3). Table cells align on this,
    /// not on the last line. `NoLines` when the box has no line box.
    pub first_baseline: LastBaseline,
    /// Collapsible margins adjoining the top border edge (own margin-top,
    /// plus first-child margins when nothing separates them).  The parent
    /// places the border box after this strut has been resolved.
    pub start_strut: MarginStrut,
    /// Collapsible margins adjoining the bottom border edge.
    pub end_strut: MarginStrut,
    /// Top and bottom margins adjoin each other (CSS2 8.3.1 "collapse
    /// through"): start and end strut belong to one collapsed margin.
    pub self_collapsing: bool,
    /// Out-of-flow descendants whose containing block is further up
    /// (static positions relative to this border box).
    pub oof: Vec<oof::OofCandidate>,
    /// The layout read or wrote the parent's exclusion space, so a
    /// different BFC offset may change it (Blink relayouts when the
    /// estimated BFC block offset was wrong).
    pub bfc_dependent: bool,
    /// Line boxes or a BFC root would have to avoid a float
    /// (NGLayoutOpportunity per line; not implemented yet).
    pub float_overlap: bool,
    /// Line boxes this box contributes to an ancestor line-clamp budget.
    pub lines_used: usize,
}

#[cfg(any(test, feature = "layout-ng-trace"))]
thread_local! {
    /// Why the last layout pass gave up at runtime (trace accounting).
    static RUNTIME_REASON: std::cell::Cell<&'static str> = const { std::cell::Cell::new("") };
}

/// A runtime fallback with its reason, reported by
/// `W3COS_LAYOUT_NG_TRACE` as `fallback runtime:<reason>`.
#[cfg_attr(any(test, feature = "layout-ng-trace"), cold)]
#[cfg_attr(not(any(test, feature = "layout-ng-trace")), inline(always))]
pub(crate) fn bail<T>(reason: &'static str) -> Option<T> {
    #[cfg(any(test, feature = "layout-ng-trace"))]
    RUNTIME_REASON.with(|r| r.set(reason));
    #[cfg(not(any(test, feature = "layout-ng-trace")))]
    let _ = reason;
    None
}

/// Labels a `None` from a helper that has no reason of its own; a reason
/// recorded deeper (`bail`) is kept.
pub(crate) trait Why {
    fn why(self, reason: &'static str) -> Self;
}

impl<T> Why for Option<T> {
    #[inline]
    fn why(self, reason: &'static str) -> Self {
        if self.is_none() {
            note(reason);
        }
        self
    }
}

#[cfg_attr(any(test, feature = "layout-ng-trace"), cold)]
#[cfg_attr(not(any(test, feature = "layout-ng-trace")), inline(always))]
fn note(reason: &'static str) {
    #[cfg(any(test, feature = "layout-ng-trace"))]
    RUNTIME_REASON.with(|r| {
        if r.get().is_empty() {
            r.set(reason);
        }
    });
    #[cfg(not(any(test, feature = "layout-ng-trace")))]
    let _ = reason;
}

/// A text fragment's characters, shared by every copy of the fragment
/// (paint's, and a retained subtree's on the next flush) instead of
/// copied (Blink's NGPhysicalTextFragment shares its text content).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FragmentText(std::rc::Rc<str>);

impl FragmentText {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::ops::Deref for FragmentText {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl From<&str> for FragmentText {
    fn from(text: &str) -> Self {
        Self(text.into())
    }
}

impl From<String> for FragmentText {
    fn from(text: String) -> Self {
        Self(text.into())
    }
}

impl PartialEq<str> for FragmentText {
    fn eq(&self, other: &str) -> bool {
        &*self.0 == other
    }
}

impl PartialEq<&str> for FragmentText {
    fn eq(&self, other: &&str) -> bool {
        &*self.0 == *other
    }
}

impl std::fmt::Display for FragmentText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One line-box piece of a text node broken inside its content, like
/// Blink's NGPhysicalTextFragment: the node, its [start, end) byte offsets
/// into the node's text (NGInlineItem start/end offsets) and the piece's
/// physical rect. `text` is that slice, captured at layout time so later
/// paint-side whitespace trimming cannot shift the offsets.
#[derive(Clone, Debug, PartialEq)]
pub struct TextFragment {
    pub index: usize,
    pub start: usize,
    pub end: usize,
    pub rect: LayoutRect,
    pub text: FragmentText,
    /// Sideways glyphs of a vertical flow (see `BoxFragment::vertical`).
    pub vertical: bool,
}

/// Entry point used by tests: the per-node rects only.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn layout_tree(
    root: &Component,
    flat: &[FlatNodeInfo],
    viewport_w: f32,
    viewport_h: f32,
) -> Option<Vec<(LayoutRect, usize)>> {
    layout_tree_with_text(root, flat, viewport_w, viewport_h).map(|(rects, _)| rects)
}

/// Entry point used by `LayoutEngine::compute`.  Returns `None` when the tree
/// is outside the supported stage, so the legacy engine runs.  A text node
/// broken across lines gets one rect per node (the union of its pieces, like
/// LayoutText's bounding box) plus one `TextFragment` per piece, in fragment
/// order, which paint draws in place of the node.
pub(crate) fn layout_tree_with_text(
    root: &Component,
    flat: &[FlatNodeInfo],
    viewport_w: f32,
    viewport_h: f32,
) -> Option<(Vec<(LayoutRect, usize)>, Vec<TextFragment>)> {
    let _ = root;
    // A generated `::before`/`::after` with `content: ""` is lowered to an
    // empty text leaf that keeps the pseudo-element's own display. A
    // block-level or atomic one is an empty box here, as in the box tree.
    // An empty inline text node generates no box at all (CSS Text 3 §4.1:
    // collapsed away; it adds nothing to a line, and a flex or grid parent
    // makes no item of it).
    let hidden_entry = |index: usize| -> Option<(usize, w3cos_std::style::Style)> {
        let node = &flat[index];
        {
            if is_empty_inline_text(node)
                && !node
                    .parent
                    .is_some_and(|parent| parent_has_break_beside_boxes(flat, parent))
            {
                let mut style = node.style.clone();
                style.display = w3cos_std::style::Display::None;
                return Some((index, style));
            }
            // The DOM's fallback `px` size of an svg grid item is `auto`
            // for layout (the legacy engine keeps reading the `px`).
            // (Only a box with a specified size has a fallback one; read
            // its markers, then its parent's, only then.)
            if node.style.width == w3cos_std::style::Dimension::Auto
                && node.style.height == w3cos_std::style::Dimension::Auto
            {
                return None;
            }
            let style = block::svg_fallback_auto_style(node.style)?;
            node.parent
                .is_some_and(|parent| block::is_author_grid(flat[parent].style))
                .then_some((index, style))
        }
    };
    let hidden: Vec<(usize, w3cos_std::style::Style)> =
        match box_scan::retained(box_scan::Scan::Hidden, flat, |index| {
            hidden_entry(index).is_some()
        }) {
            Some(hits) => hits.into_iter().filter_map(hidden_entry).collect(),
            None => (0..flat.len()).filter_map(hidden_entry).collect(),
        };
    if !hidden.is_empty() {
        let flat_hidden = run_in_patched(flat, &hidden);
        let patched: Vec<FlatNodeInfo> = flat_hidden
            .iter()
            .map(|node| FlatNodeInfo {
                kind: if is_empty_box_leaf(node) {
                    &EMPTY_BOX
                } else {
                    node.kind
                },
                ..*node
            })
            .collect();
        return layout_tree_balanced(&patched, viewport_w, viewport_h);
    }
    let empty_leaf = box_scan::retained(box_scan::Scan::EmptyLeaf, flat, |index| {
        is_empty_box_leaf(&flat[index])
    })
    .map_or_else(
        || flat.iter().any(is_empty_box_leaf),
        |hits| !hits.is_empty(),
    );
    if empty_leaf {
        let patched: Vec<FlatNodeInfo> = flat
            .iter()
            .map(|node| FlatNodeInfo {
                kind: if is_empty_box_leaf(node) {
                    &EMPTY_BOX
                } else {
                    node.kind
                },
                ..*node
            })
            .collect();
        return layout_tree_balanced(&patched, viewport_w, viewport_h);
    }
    layout_tree_balanced(flat, viewport_w, viewport_h)
}

static EMPTY_BOX: ComponentKind = ComponentKind::Box;

fn is_empty_box_leaf(node: &FlatNodeInfo) -> bool {
    matches!(node.kind, ComponentKind::Text { content } if content.is_empty())
        && matches!(
            node.style.display,
            w3cos_std::style::Display::Block | w3cos_std::style::Display::InlineBlock
        )
}

/// A parent holding both a forced break (`<br>`, U+2028) and a non-text
/// child. Its line boxes put the break after an atomic inline, which this
/// engine stacks differently from the legacy layout, so the empty text
/// nodes around such a break keep the container on the legacy path.
fn parent_has_break_beside_boxes(flat: &[FlatNodeInfo], parent: usize) -> bool {
    let end = flat[parent].subtree_end;
    let mut breaks = false;
    let mut boxes = false;
    let mut child = parent + 1;
    while child < end {
        match flat[child].kind {
            ComponentKind::Text { content } if content.contains('\u{2028}') => breaks = true,
            ComponentKind::Text { .. } => {}
            _ => boxes = true,
        }
        child = flat[child].subtree_end.max(child + 1);
    }
    breaks && boxes
}

fn is_empty_inline_text(node: &FlatNodeInfo) -> bool {
    matches!(node.kind, ComponentKind::Text { content } if content.is_empty())
        && node.style.display == w3cos_std::style::Display::Inline
}

fn layout_tree_balanced(
    flat: &[FlatNodeInfo],
    viewport_w: f32,
    viewport_h: f32,
) -> Option<(Vec<(LayoutRect, usize)>, Vec<TextFragment>)> {
    // `flex-wrap: balance` is stored by the cascade as `nowrap` plus a
    // marker (legacy keeps it unrecognized); this engine reads it as a
    // balanced wrap.
    let balanced = balanced_wrap_styles(flat);
    if balanced.is_empty() {
        return layout_logical(flat, viewport_w, viewport_h);
    }
    let patched = run_in_patched(flat, &balanced);
    layout_logical(&patched, viewport_w, viewport_h)
}

/// The styles of flex containers with `flex-wrap: balance`, as `wrap` (or
/// `wrap-reverse`) with the balance marker, keyed by flat index.
fn balanced_wrap_styles(flat: &[FlatNodeInfo]) -> Vec<(usize, w3cos_std::style::Style)> {
    use w3cos_std::style::{Display, FlexWrap};
    let entry = |index: usize| -> Option<(usize, w3cos_std::style::Style)> {
        {
            let style = flat[index].style;
            if !matches!(style.display, Display::Flex) {
                return None;
            }
            let properties = style.custom_properties.as_ref()?;
            // A `-webkit-box` is a legacy flex box that ignores `flex-wrap`.
            if properties
                .get("--w3cos-internal-unrecognized-display")
                .is_some()
            {
                return None;
            }
            let value = properties.get("--w3cos-internal-unrecognized-flex-wrap")?;
            let mut wrap = FlexWrap::Wrap;
            let mut balance = false;
            for word in value.split_ascii_whitespace() {
                match word {
                    "balance" => balance = true,
                    "wrap" => {}
                    "wrap-reverse" => wrap = FlexWrap::WrapReverse,
                    _ => return None,
                }
            }
            if !balance {
                return None;
            }
            let mut style = style.clone();
            style.flex_wrap = wrap;
            if let Some(properties) = style.custom_properties.as_mut() {
                properties.remove("--w3cos-internal-unrecognized-flex-wrap");
                properties.insert(flex::BALANCE_MARKER.to_string(), "1".to_string());
            }
            Some((index, style))
        }
    };
    match box_scan::retained(box_scan::Scan::Balanced, flat, |index| {
        entry(index).is_some()
    }) {
        Some(hits) => hits.into_iter().filter_map(entry).collect(),
        None => (0..flat.len()).filter_map(entry).collect(),
    }
}

fn layout_logical(
    flat: &[FlatNodeInfo],
    viewport_w: f32,
    viewport_h: f32,
) -> Option<(Vec<(LayoutRect, usize)>, Vec<TextFragment>)> {
    // Logical properties (`inline-size`, `block-size`, block-axis margins and
    // padding) resolved per writing mode apply to this engine only; the
    // declared style stays what the legacy engine reads.
    let logical_entry = |index: usize| -> Option<(usize, w3cos_std::style::Style)> {
        let node = &flat[index];
        let overrides = node.style.logical_overrides.as_ref()?;
        let mut style = node.style.clone();
        overrides.apply(&mut style);
        Some((index, style))
    };
    let logical: Vec<(usize, w3cos_std::style::Style)> =
        match box_scan::retained(box_scan::Scan::Logical, flat, |index| {
            flat[index].style.logical_overrides.is_some()
        }) {
            Some(hits) => hits.into_iter().filter_map(logical_entry).collect(),
            None => (0..flat.len()).filter_map(logical_entry).collect(),
        };
    // `text-spacing-trim` (default `normal`) halves fullwidth punctuation at
    // line ends and between adjacent marks; it is not laid out yet, so a page
    // that sets text in such punctuation keeps ignoring the logical sizes on
    // both sides of a comparison instead of exposing the missing trim.
    if logical.is_empty() || has_spacing_trim_punctuation(flat) {
        return layout_with_run_in(flat, viewport_w, viewport_h);
    }
    // Which engine lays a page out is decided on the declared style, as it
    // is for pages without logical properties: a reftest's test and
    // reference keep choosing the same engine they chose before the
    // logical axes were modelled, and only pages this engine lays out see
    // the resolved values.
    if declared_tree_unsupported(flat) {
        return layout_with_run_in(flat, viewport_w, viewport_h);
    }
    let logical_flat = run_in_patched(flat, &logical);
    layout_with_run_in(&logical_flat, viewport_w, viewport_h)
}

/// Whether any text holds fullwidth punctuation that `text-spacing-trim`
/// trims (CJK brackets, stops and commas, and their fullwidth forms).
fn has_spacing_trim_punctuation(flat: &[FlatNodeInfo]) -> bool {
    let trimmed = |c: char| {
        matches!(
            c,
            '\u{3001}'..='\u{3003}'
                | '\u{3008}'..='\u{3011}'
                | '\u{3014}'..='\u{301F}'
                | '\u{FF01}'
                | '\u{FF08}'
                | '\u{FF09}'
                | '\u{FF0C}'
                | '\u{FF0E}'
                | '\u{FF1A}'
                | '\u{FF1B}'
                | '\u{FF1F}'
                | '\u{FF3B}'
                | '\u{FF3D}'
                | '\u{FF5B}'
                | '\u{FF5D}'
                | '\u{FF5F}'
                | '\u{FF60}'
        )
    };
    flat.iter().any(|node| match node.kind {
        ComponentKind::Text { content } => content.chars().any(trimmed),
        _ => false,
    })
}

fn run_in_patched<'a>(
    flat: &[FlatNodeInfo<'a>],
    run_in: &'a [(usize, w3cos_std::style::Style)],
) -> Vec<FlatNodeInfo<'a>> {
    let mut overrides = run_in.iter().peekable();
    flat.iter()
        .enumerate()
        .map(|(index, node)| FlatNodeInfo {
            style: overrides
                .next_if(|(at, _)| *at == index)
                .map_or(node.style, |(_, style)| style),
            ..*node
        })
        .collect()
}

fn declared_tree_unsupported(flat: &[FlatNodeInfo]) -> bool {
    if flat.is_empty() {
        return true;
    }
    let run_in = block::run_in_block_styles(flat);
    if run_in.is_empty() {
        return block::unsupported_tree(flat).is_some();
    }
    block::unsupported_tree(&run_in_patched(flat, &run_in)).is_some()
}

fn layout_with_run_in(
    flat: &[FlatNodeInfo],
    viewport_w: f32,
    viewport_h: f32,
) -> Option<(Vec<(LayoutRect, usize)>, Vec<TextFragment>)> {
    let run_in = block::run_in_block_styles(flat);
    let run_in_flat;
    let base: &[FlatNodeInfo] = if run_in.is_empty() {
        flat
    } else {
        run_in_flat = run_in_patched(flat, &run_in);
        &run_in_flat
    };
    // Vertical-writing-mode flex and grid containers lay out with transposed
    // axes.
    // Both read only boxes in a vertical writing mode: a tree without one
    // has nothing to transpose (box-tree mode keeps that set).
    let no_vertical = box_scan::retained(box_scan::Scan::Vertical, base, |index| {
        !base[index].style.writing_mode.is_horizontal()
    })
    .is_some_and(|hits| hits.is_empty());
    let mut vertical = if no_vertical {
        Vec::new()
    } else {
        grid::vertical_grid_styles(base)
    };
    if !no_vertical {
        let taken: Vec<usize> = vertical.iter().map(|(index, _)| *index).collect();
        vertical.extend(
            flex::vertical_flex_styles(base)
                .into_iter()
                .filter(|(index, _)| !taken.contains(index)),
        );
    }
    if vertical.is_empty() {
        return layout_flat_with_text(base, viewport_w, viewport_h);
    }
    let patched = run_in_patched(base, &vertical);
    layout_flat_with_text(&patched, viewport_w, viewport_h)
}

fn layout_flat_with_text(
    flat: &[FlatNodeInfo],
    viewport_w: f32,
    viewport_h: f32,
) -> Option<(Vec<(LayoutRect, usize)>, Vec<TextFragment>)> {
    let reason = if flat.is_empty() {
        Some("empty")
    } else {
        block::unsupported_tree(flat)
    };
    if trace() {
        match reason {
            None => eprintln!("[layout-ng] ng ({} boxes)", flat.len()),
            Some(_) => eprintln!(
                "[layout-ng] fallback {} ({} boxes)",
                block::unsupported_reasons(flat).join(","),
                flat.len()
            ),
        }
    }
    if trace() && std::env::var_os("W3COS_LAYOUT_NG_TRACE").is_some_and(|v| v == "2") {
        // Tree dump for diagnosing pages whose scripts the unit probe cannot run.
        for (i, node) in flat.iter().enumerate() {
            let s = node.style;
            let kind = match node.kind {
                w3cos_std::ComponentKind::Text { content } => format!("Text {content:?}"),
                other => format!("{:?}", std::mem::discriminant(other)),
            };
            eprintln!(
                "[layout-ng tree] #{i} end={} {kind} disp={:?} ws={:?} fs={} lh={:?} ff={:?} h={:?} minh={:?} m=({:?},{:?}) p=({:?},{:?}) markers={:?}",
                node.subtree_end,
                s.display,
                s.white_space,
                s.font_size,
                s.line_height,
                s.font_family,
                s.height,
                s.min_height,
                s.margin.top,
                s.margin.bottom,
                s.padding.top,
                s.padding.bottom,
                s.custom_properties.as_ref().map(|p| p
                    .keys()
                    .filter(|k| k.starts_with("--w3cos-internal"))
                    .collect::<Vec<_>>())
            );
        }
    }
    if reason.is_some() {
        return None;
    }
    #[cfg(any(test, feature = "layout-ng-trace"))]
    RUNTIME_REASON.with(|r| r.set(""));
    let pass = cache::begin_pass(flat);
    let laid = block::layout_root(flat, viewport_w, viewport_h);
    drop(pass);
    let Some((x, y, fragment)) = laid else {
        #[cfg(any(test, feature = "layout-ng-trace"))]
        if trace() {
            let reason = RUNTIME_REASON.with(|r| r.get());
            let sep = if reason.is_empty() { "" } else { ":" };
            eprintln!(
                "[layout-ng] fallback runtime{sep}{reason} ({} boxes)",
                flat.len()
            );
        }
        return None;
    };
    let mut out = Vec::with_capacity(flat.len());
    let mut texts = Vec::new();
    match box_tree_key(flat) {
        Some(key) => flatten_retained(&fragment, x, y, flat, key, &mut out, &mut texts),
        None => flatten(&fragment, x, y, flat, &mut out, &mut texts),
    }
    if !texts.is_empty() {
        // Paint draws pieces as inline text runs; a block-level text leaf
        // also paints box decorations from its single node rect.
        // Bidi paint reorders whole text clients; keep that on legacy too.
        if texts.iter().any(|t| {
            let style = flat[t.index].style;
            // A table-cell text leaf (no child list) carries the cell's
            // display. Its line box is one fragment so paint can place the
            // glyphs where an inline child would sit.
            let line_box = matches!(
                style.display,
                w3cos_std::style::Display::Inline
                    | w3cos_std::style::Display::TableCell
                    | w3cos_std::style::Display::TableCaption
            );
            !line_box
                || style
                    .custom_properties
                    .as_ref()
                    .is_some_and(|p| p.contains_key("--w3cos-internal-bidi-logical-order"))
        }) {
            if trace() {
                eprintln!(
                    "[layout-ng] fallback runtime:internal-break-paint ({} boxes)",
                    flat.len()
                );
            }
            return None;
        }
        out = union_text_pieces(out, &texts, flat.len());
    }
    crate::list_marker::project_retained(&mut out, flat, viewport_w, viewport_h);
    Some((out, texts))
}

/// Collapse the pieces of each broken text node into one entry (at the first
/// piece's position) holding their union.
fn union_text_pieces(
    out: Vec<(LayoutRect, usize)>,
    texts: &[TextFragment],
    len: usize,
) -> Vec<(LayoutRect, usize)> {
    // Flat index -> its merged entry (dense: `UNSEEN` for a broken text
    // node not yet met, `u32::MAX` for an unbroken box).
    const UNSEEN: u32 = u32::MAX - 1;
    let len = len.max(texts.iter().map(|t| t.index + 1).max().unwrap_or(0));
    let mut slot = vec![u32::MAX; len];
    for text in texts {
        slot[text.index] = UNSEEN;
    }
    let mut merged: Vec<(LayoutRect, usize)> = Vec::with_capacity(out.len());
    for (rect, index) in out {
        if let Some(entry) = slot.get_mut(index)
            && *entry != u32::MAX
        {
            if *entry != UNSEEN {
                let u = &mut merged[*entry as usize].0;
                let x0 = u.x.min(rect.x);
                let y0 = u.y.min(rect.y);
                let x1 = (u.x + u.width).max(rect.x + rect.width);
                let y1 = (u.y + u.height).max(rect.y + rect.height);
                *u = LayoutRect {
                    x: x0,
                    y: y0,
                    width: x1 - x0,
                    height: y1 - y0,
                };
                continue;
            }
            *entry = merged.len() as u32;
        }
        merged.push((rect, index));
    }
    merged
}

fn flatten(
    fragment: &BoxFragment,
    x: f32,
    y: f32,
    flat: &[FlatNodeInfo],
    out: &mut Vec<(LayoutRect, usize)>,
    texts: &mut Vec<TextFragment>,
) {
    flatten_own(fragment, x, y, flat, out, texts);
    for (dx, dy, child) in &fragment.children {
        flatten(child, x + dx, y + dy, flat, out, texts);
    }
}

/// A fragment's own rect entry and its text piece, if any.
fn flatten_own(
    fragment: &BoxFragment,
    x: f32,
    y: f32,
    flat: &[FlatNodeInfo],
    out: &mut Vec<(LayoutRect, usize)>,
    texts: &mut Vec<TextFragment>,
) {
    let rect = LayoutRect {
        x,
        y,
        width: fragment.width,
        height: fragment.height,
    };
    let whole = fragment
        .vertical
        .then(|| match flat[fragment.index].kind {
            w3cos_std::ComponentKind::Text { content } => (0, content.len()),
            _ => (0, 0),
        })
        .filter(|_| fragment.text_range.is_none());
    if let Some((start, end)) = fragment.text_range.or(whole) {
        let content = match flat[fragment.index].kind {
            w3cos_std::ComponentKind::Text { content } => &content[start..end],
            _ => "",
        };
        // One allocation for the shared text (a `String` first would be
        // copied again into the `Rc<str>`).
        let text: FragmentText = if !fragment.ellipsis {
            content.into()
        } else {
            let mut text = content.to_string();
            // (The line's collapsible trailing space goes before the ellipsis.)
            let keep_spaces =
                flat[fragment.index].style.white_space == w3cos_std::style::WhiteSpace::Pre;
            text.truncate(
                text.trim_end_matches(|c| {
                    matches!(c, '\n' | '\u{2028}') || (c == ' ' && !keep_spaces)
                })
                .len(),
            );
            text.push('\u{2026}');
            text.into()
        };
        texts.push(TextFragment {
            index: fragment.index,
            start,
            end,
            rect,
            text,
            vertical: fragment.vertical,
        });
    }
    out.push((rect, fragment.index));
}

/// Box-tree mode: the last patch flush's flattened output, and where each
/// large retained child list's part of it sits (keyed by the list's
/// address; the list is pinned so the address stays its own).
struct FlatReuse {
    key: (u64, u64),
    len: usize,
    out: Vec<(LayoutRect, usize)>,
    texts: Vec<TextFragment>,
    spans: w3cos_std::fxhash::FxHashMap<usize, FlatSpan>,
}

struct FlatSpan {
    children: FragmentChildren,
    x: f32,
    y: f32,
    out: (usize, usize),
    texts: (usize, usize),
}

thread_local! {
    static FLAT_REUSE: std::cell::RefCell<Option<FlatReuse>> = const { std::cell::RefCell::new(None) };
}

/// See [`cache::forget_walk_fingerprints`].
pub(crate) fn forget_walk_fingerprints() {
    cache::forget_walk_fingerprints();
}

pub(crate) fn forget_flat_reuse() {
    FLAT_REUSE.with(|slot| *slot.borrow_mut() = None);
}

/// [`flatten`] reusing the output of a child list laid out at the same
/// position in the last patch flush (the same list, so the same boxes and
/// offsets; an exact position keeps the sums bit-identical).
fn flatten_retained(
    fragment: &BoxFragment,
    x: f32,
    y: f32,
    flat: &[FlatNodeInfo],
    key: (u64, u64),
    out: &mut Vec<(LayoutRect, usize)>,
    texts: &mut Vec<TextFragment>,
) {
    let prev = FLAT_REUSE
        .with(|slot| slot.borrow_mut().take())
        .filter(|prev| prev.key == key && prev.len == flat.len());
    let mut spans = w3cos_std::fxhash::FxHashMap::default();
    flatten_reusing(fragment, x, y, flat, prev.as_ref(), out, texts, &mut spans);
    FLAT_REUSE.with(|slot| {
        *slot.borrow_mut() = Some(FlatReuse {
            key,
            len: flat.len(),
            out: out.clone(),
            texts: texts.clone(),
            spans,
        })
    });
}

#[allow(clippy::too_many_arguments)]
fn flatten_reusing(
    fragment: &BoxFragment,
    x: f32,
    y: f32,
    flat: &[FlatNodeInfo],
    prev: Option<&FlatReuse>,
    out: &mut Vec<(LayoutRect, usize)>,
    texts: &mut Vec<TextFragment>,
    spans: &mut w3cos_std::fxhash::FxHashMap<usize, FlatSpan>,
) {
    // This box's own entries, exactly as `flatten` makes them.
    flatten_own(fragment, x, y, flat, out, texts);
    if fragment.children.is_empty() {
        return;
    }
    let address = std::rc::Rc::as_ptr(&fragment.children.0) as usize;
    if let Some(span) = prev.and_then(|prev| prev.spans.get(&address))
        && std::rc::Rc::ptr_eq(&span.children.0, &fragment.children.0)
        && span.x.to_bits() == x.to_bits()
        && span.y.to_bits() == y.to_bits()
    {
        let prev = prev.unwrap();
        let (out_start, text_start) = (out.len(), texts.len());
        out.extend_from_slice(&prev.out[span.out.0..span.out.1]);
        texts.extend_from_slice(&prev.texts[span.texts.0..span.texts.1]);
        spans.insert(
            address,
            FlatSpan {
                children: fragment.children.clone(),
                x,
                y,
                out: (out_start, out.len()),
                texts: (text_start, texts.len()),
            },
        );
        return;
    }
    let (out_start, text_start) = (out.len(), texts.len());
    for (dx, dy, child) in fragment.children.iter() {
        flatten_reusing(child, x + dx, y + dy, flat, prev, out, texts, spans);
    }
    if out.len() - out_start >= 16 {
        spans.insert(
            address,
            FlatSpan {
                children: fragment.children.clone(),
                x,
                y,
                out: (out_start, out.len()),
                texts: (text_start, texts.len()),
            },
        );
    }
}

/// Verify mode: compare NG output with the legacy engine's.  Returns the
/// number of differing entries and logs the first few.
pub(crate) fn verify(
    flat: &[FlatNodeInfo],
    legacy: &[(LayoutRect, usize)],
    ng: &[(LayoutRect, usize)],
) -> usize {
    use std::collections::HashMap;
    let legacy_map: HashMap<usize, LayoutRect> = legacy.iter().map(|(r, i)| (*i, *r)).collect();
    let ng_map: HashMap<usize, LayoutRect> = ng.iter().map(|(r, i)| (*i, *r)).collect();
    let mut indices: Vec<usize> = legacy_map.keys().chain(ng_map.keys()).copied().collect();
    indices.sort_unstable();
    indices.dedup();
    let close = |a: f32, b: f32| (a - b).abs() <= 1.0 / 64.0;
    let mut diffs = 0;
    // W3COS_LAYOUT_NG_VERIFY=all also logs matching boxes.
    let all = std::env::var_os("W3COS_LAYOUT_NG_VERIFY").is_some_and(|v| v == "all");
    for index in indices {
        let (l, n) = (legacy_map.get(&index), ng_map.get(&index));
        let same = match (l, n) {
            (Some(l), Some(n)) => {
                close(l.x, n.x)
                    && close(l.y, n.y)
                    && close(l.width, n.width)
                    && close(l.height, n.height)
            }
            _ => false,
        };
        if same && all {
            eprintln!("[layout-ng verify] = #{index} {l:?}");
        }
        if !same {
            diffs += 1;
            if diffs <= 8 || all {
                let tag = flat
                    .get(index)
                    .map(|info| format!("{:?} {:?}", info.kind, info.style.display))
                    .unwrap_or_default();
                eprintln!("[layout-ng verify] #{index} {tag}: legacy {l:?} ng {n:?}");
            }
        }
    }
    if diffs > 0 {
        eprintln!(
            "[layout-ng verify] {diffs} differing boxes of {}",
            flat.len()
        );
    }
    diffs
}

#[cfg(test)]
mod inline_tests;
#[cfg(test)]
mod tests;
