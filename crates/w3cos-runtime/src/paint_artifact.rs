//! Retained paint output shared by every raster backend.
//!
//! This follows Blink's split between layout, PaintArtifact construction and
//! compositor consumption. The artifact owns immutable snapshots so scrolling
//! and raster scheduling never need to walk the application component tree.

use w3cos_std::color::Color;
use w3cos_std::component::ComponentKind;
use w3cos_std::style::{
    Contain, Display, Overflow, Position, Spacing, Style, Transform2D, Visibility, WhiteSpace,
};

use crate::layout::LayoutRect;

#[path = "paint_artifact/media_controls.rs"]
mod media_controls;

pub type PropertyNodeId = usize;
pub type PaintChunkId = usize;
pub type PaintOrderLevel = (u8, i32, usize);

/// Every node's paint-order key, stored back to back: a key is its paint
/// context owner's key prefix plus one or two levels, so one buffer per
/// artifact replaces one allocation per node.
#[derive(Debug, Default, PartialEq)]
pub struct PaintOrderKeys {
    levels: Vec<PaintOrderLevel>,
    /// Per node: start and length in `levels`.
    spans: Vec<(u32, u32)>,
}

impl Clone for PaintOrderKeys {
    fn clone(&self) -> Self {
        Self {
            levels: self.levels.clone(),
            spans: self.spans.clone(),
        }
    }

    fn clone_from(&mut self, source: &Self) {
        self.levels.clone_from(&source.levels);
        self.spans.clone_from(&source.spans);
    }
}

impl PaintOrderKeys {
    pub const fn new() -> Self {
        Self {
            levels: Vec::new(),
            spans: Vec::new(),
        }
    }

    pub fn with_len(len: usize) -> Self {
        Self {
            levels: Vec::with_capacity(len * 4),
            spans: vec![(0, 0); len],
        }
    }

    pub fn len(&self) -> usize {
        self.spans.len()
    }

    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<&[PaintOrderLevel]> {
        let &(start, len) = self.spans.get(index)?;
        Some(&self.levels[start as usize..(start + len) as usize])
    }

    /// The key of `index`; empty when it has none.
    pub fn key(&self, index: usize) -> &[PaintOrderLevel] {
        self.get(index).unwrap_or(&[])
    }

    /// Sets the key of `index` to the first `prefix.1` levels of node
    /// `prefix.0`'s key followed by `tail`.
    pub fn set(&mut self, index: usize, prefix: Option<(usize, usize)>, tail: &[PaintOrderLevel]) {
        let start = self.levels.len();
        if let Some((owner, len)) = prefix {
            let owner_start = self.spans[owner].0 as usize;
            self.levels
                .extend_from_within(owner_start..owner_start + len);
        }
        self.levels.extend_from_slice(tail);
        self.spans[index] = (start as u32, (self.levels.len() - start) as u32);
    }

    #[cfg(test)]
    pub fn from_keys(keys: &[Vec<PaintOrderLevel>]) -> Self {
        let mut out = Self::with_len(keys.len());
        for (index, key) in keys.iter().enumerate() {
            out.set(index, None, key);
        }
        out
    }
}

#[derive(Clone)]
pub struct PaintNode {
    pub kind: ComponentKind,
    /// Shared with the component tree's style until build adjusts it
    /// (copy-on-write), so an unchanged node's style is not copied per frame
    /// and compares by pointer.
    pub style: w3cos_std::component::ComponentStyle,
    pub parent: Option<usize>,
    pub sticky_counter_signal: Option<usize>,
}

impl PaintNode {
    /// UA paint children are appended after all DOM/layout source nodes.
    pub(crate) fn is_paint_only(&self) -> bool {
        self.style.custom_properties.as_ref().is_some_and(|p| {
            const KEY: &str = "--w3cos-internal-media-controls-layer";
            p.may_contain(KEY) && p.contains_key(KEY)
        })
    }
}

pub(crate) struct AppliedTextDecoration<'a> {
    pub style: std::borrow::Cow<'a, Style>,
    pub baseline_shift: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct InlineLineContext {
    pub line_box: LayoutRect,
    pub first_line_box: LayoutRect,
    pub direction: w3cos_std::style::TextDirection,
    pub text_align: w3cos_std::style::TextAlign,
    pub line_advance: Option<f32>,
    #[cfg(feature = "skia")]
    pub tab_stops: crate::render_skia::TabStops,
}

impl InlineLineContext {
    pub fn fragment_box(&self, style: &Style, first: bool, last: bool) -> LayoutRect {
        let line = if first {
            self.first_line_box
        } else {
            self.line_box
        };
        let padding = style.padding_lengths();
        let margin = style.margin_lengths();
        let widths = crate::text_layout::inline_fragment_border_widths(style, first, last);
        let rtl = style.direction == w3cos_std::style::TextDirection::Rtl;
        let left = if (first && !rtl) || (last && rtl) {
            margin.left + padding.left + widths[3]
        } else {
            0.0
        };
        let right = if (last && !rtl) || (first && rtl) {
            margin.right + padding.right + widths[1]
        } else {
            0.0
        };
        LayoutRect {
            x: line.x + left,
            width: (line.width - left - right).max(1.0),
            ..line
        }
    }

    pub fn alignment(&self) -> w3cos_std::style::TextAlign {
        use w3cos_std::style::{TextAlign, TextDirection};
        match (self.text_align, self.direction) {
            (TextAlign::Start, TextDirection::Rtl) | (TextAlign::End, TextDirection::Ltr) => {
                TextAlign::Right
            }
            (TextAlign::Start, TextDirection::Ltr) | (TextAlign::End, TextDirection::Rtl) => {
                TextAlign::Left
            }
            (align, _) => align,
        }
    }
}

pub fn effective_z_order(style: &Style, inherited: i32) -> i32 {
    if style.z_index != 0 {
        style.z_index
    } else if matches!(
        style.position,
        Position::Relative | Position::Absolute | Position::Fixed | Position::Sticky
    ) {
        inherited.saturating_add(1)
    } else {
        inherited
    }
}

/// Paint style for a flattened node. When the node paints its component's
/// own style, share that `Arc` instead of deep-copying the `Style`, so an
/// unchanged node keeps the same allocation across frames and the dirty
/// comparison short-circuits on pointer equality.
pub fn shared_paint_style(
    component: Option<&w3cos_std::Component>,
    style: &Style,
) -> w3cos_std::component::ComponentStyle {
    match component {
        Some(component) if std::ptr::eq(component.style.as_style(), style) => {
            component.style.clone()
        }
        _ => style.clone().into(),
    }
}

/// `source` as an owned kind, built in `slot`'s storage when there is one:
/// a text kind copies into the old buffer instead of allocating a new one.
/// Inlined: it runs once per box per frame and is a match on two kinds.
#[inline]
pub(crate) fn refill_kind(
    slot: Option<&mut ComponentKind>,
    source: &ComponentKind,
) -> ComponentKind {
    let Some(slot) = slot else {
        return source.clone();
    };
    let mut kind = std::mem::replace(slot, ComponentKind::Root);
    match (&mut kind, source) {
        (ComponentKind::Text { content }, ComponentKind::Text { content: source }) => {
            content.clone_from(source);
        }
        (kind, source) => *kind = source.clone(),
    }
    kind
}

/// Rebuild paint nodes, cloning `Style` only for slots that actually changed.
///
/// Returns the node list and the number of Style clones performed. A clean
/// subtree (same length, same parent/kind/style) reuses the previous
/// allocation and reports 0 clones.
pub fn reuse_or_clone_paint_nodes<'a>(
    existing: Vec<PaintNode>,
    incoming: impl IntoIterator<Item = (&'a ComponentKind, &'a Style, Option<usize>, Option<usize>)>,
) -> (Vec<PaintNode>, usize) {
    let incoming: Vec<_> = incoming.into_iter().collect();
    if existing.len() != incoming.len() {
        let clones = incoming.len();
        let nodes = incoming
            .into_iter()
            .map(|(kind, style, parent, sticky)| PaintNode {
                kind: kind.clone(),
                style: style.clone().into(),
                parent,
                sticky_counter_signal: sticky,
            })
            .collect();
        return (nodes, clones);
    }
    let mut existing: Vec<Option<PaintNode>> = existing.into_iter().map(Some).collect();
    let mut out = Vec::with_capacity(incoming.len());
    let mut clones = 0;
    for (i, (kind, style, parent, sticky)) in incoming.into_iter().enumerate() {
        let reusable = existing[i].as_ref().is_some_and(|old| {
            old.parent == parent
                && old.sticky_counter_signal == sticky
                && old.kind == *kind
                && (std::ptr::eq(old.style.as_style(), style) || *old.style == *style)
        });
        if reusable {
            out.push(existing[i].take().expect("paint node slot"));
        } else {
            clones += 1;
            out.push(PaintNode {
                kind: kind.clone(),
                style: style.clone().into(),
                parent,
                sticky_counter_signal: sticky,
            });
        }
    }
    (out, clones)
}

/// `transform: translate*(<percentage>)` resolves against the border box
/// (CSS Transforms 1, 13.1). The DOM keeps the percentages in
/// `--w3cos-internal-translate-percent` (LayoutNG only) as `"x,y"`; add them
/// to the pixel translation once the box is laid out.
fn resolve_translate_percent(
    nodes: &mut [PaintNode],
    layout_cache: &[(LayoutRect, usize)],
    // Boxes that already hold last build's resolved translate
    // (`W3COS_PAINT_PERSISTENT_NODES` clean boxes).
    skip: Option<&[bool]>,
) {
    for (rect, index) in layout_cache {
        if skip.is_some_and(|skip| skip.get(*index).copied().unwrap_or(false)) {
            continue;
        }
        let Some(node) = nodes.get_mut(*index) else {
            continue;
        };
        let Some((x, y)) = node
            .style
            .custom_properties
            .as_ref()
            .and_then(|properties| properties.get("--w3cos-internal-translate-percent"))
            .and_then(|value| value.split_once(','))
            .and_then(|(x, y)| Some((x.parse::<f32>().ok()?, y.parse::<f32>().ok()?)))
        else {
            continue;
        };
        node.style.transform.translate_x += rect.width * x / 100.0;
        node.style.transform.translate_y += rect.height * y / 100.0;
    }
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClipNode {
    pub parent: PropertyNodeId,
    pub rect: Option<LayoutRect>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EffectNode {
    pub parent: PropertyNodeId,
    pub opacity: f32,
    pub filter: Option<String>,
    /// A compositor hint isolates this subtree's raster surface even when
    /// its opacity is one and its transform is currently the identity.
    pub isolates_surface: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollNode {
    pub parent: PropertyNodeId,
    pub host_index: Option<usize>,
    pub scrollport: Option<LayoutRect>,
    pub clip: Option<PropertyNodeId>,
}

#[derive(Clone, Debug, PartialEq)]
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
                isolates_surface: false,
            }],
            scrolls: vec![ScrollNode {
                parent: 0,
                host_index: None,
                scrollport: None,
                clip: None,
            }],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DisplayItem {
    pub client_index: usize,
    pub visual_rect: LayoutRect,
    pub chunk_id: PaintChunkId,
}

/// A physical slice of one logical source box. Vertical clipping slices the
/// box decoration rather than repeating top/bottom borders at column breaks.
/// Inline-axis overflow is not clipped to the nominal column width.
#[derive(Clone, Copy, Debug)]
pub struct ColumnFragment {
    pub visual_rect: LayoutRect,
    pub translate_x: f32,
    pub translate_y: f32,
    pub clip_top: f32,
    pub clip_bottom: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaintChunk {
    pub begin: usize,
    pub end: usize,
    pub bounds: LayoutRect,
    pub properties: PaintProperties,
    pub z_order: i32,
}

#[derive(Clone)]
pub struct PaintArtifact {
    pub nodes: Vec<PaintNode>,
    /// Root viewport scroll in CSS pixels; layout and recordings stay in
    /// document coordinates so scrolling can replay retained pictures.
    pub viewport_scroll: (f32, f32),
    /// Document-space vertical band (top, bottom) outside of which root
    /// content emits no display items (`W3COS_PAINT_CULL_RECT`).
    pub(crate) cull_band: Option<(f32, f32)>,
    /// Boxes the cull band dropped (kept only under `=verify`).
    pub(crate) culled_nodes: Vec<u32>,
    pub body_index: Option<usize>,
    pub canvas_background: Color,
    pub canvas_background_style: Option<Style>,
    pub canvas_background_source: Option<usize>,
    pub canvas_background_positioning_rect: Option<LayoutRect>,
    pub display_items: Vec<DisplayItem>,
    pub chunks: Vec<PaintChunk>,
    pub properties: PropertyTrees,
    pub node_properties: Vec<PaintProperties>,
    /// The clip chain tip the node's own background and border paint under.
    ///
    /// This is `node_properties[index].clip` except for a box that clips its
    /// own overflow: CSS 2.1 11.1.1 scopes an `overflow` clip to "the contents
    /// of an element", so the box's own border and background stay outside its
    /// clip. The `clip` property and inline fragment clips do apply to the box
    /// itself, so they appear here as well. Kept beside `node_properties`
    /// rather than inside `PaintProperties` because the latter is the
    /// compositor's layer identity: the overflow box's own paint remains
    /// stationary while its contents can scroll in a separate layer.
    pub self_clip: Vec<PropertyNodeId>,
    pub z_order: Vec<i32>,
    pub paint_order: PaintOrderKeys,
    /// Per node, a rank that orders nodes as `paint_order_key` does and is
    /// equal exactly for equal keys; 0 is the empty key's (built on first
    /// use; see [`PaintArtifact::paint_order_class`]).
    pub paint_classes: std::sync::OnceLock<Vec<u32>>,
    logical_paint_ordinals: Vec<usize>,
    /// Per appended node: it or an ancestor is a fixed box (see
    /// [`PaintArtifact::viewport_attached`]).
    viewport_attached_by_index: Vec<bool>,
    pub sticky_owner: Vec<Option<usize>>,
    pub rect_by_index: Vec<Option<LayoutRect>>,
    pub column_fragments: Vec<Vec<ColumnFragment>>,
    /// Per node: the line pieces of a LayoutNG text node broken inside its
    /// content (NGPhysicalTextFragment rect + text slice), in fragment
    /// order. Empty for every node when nothing was broken. Paint draws the
    /// pieces in place of the node's single rect, at the node's own position
    /// in paint order.
    pub text_fragments: Vec<Vec<(LayoutRect, crate::layout_ng::FragmentText)>>,
    /// Per node: its text pieces are sideways glyphs of a vertical flow
    /// (`TextFragment::vertical`); paint rotates them a quarter turn.
    pub vertical_text: Vec<bool>,
    pub generation: u64,
    /// What `append_node` decided per node, kept (with
    /// `W3COS_PAINT_APPEND_REUSE=1`) so the next frame's build can copy the
    /// nodes whose inputs and context did not change.
    append_record: Option<std::sync::Arc<AppendRecord>>,
    /// Identifies this build for the next frame's `append_record.base`.
    serial: u64,
    /// `W3COS_PAINT_PERSISTENT_NODES`: the boxes whose text the line-start
    /// trim shortened in this build (sorted), for the next build's clean
    /// boxes, which take this build's adjusted node.
    pub(crate) persist_trimmed: Option<std::sync::Arc<Vec<u32>>>,
    /// That build found percentage padding (its clean boxes' used padding
    /// depends on their containing blocks: no persistent path from it).
    persist_percent_padding: bool,
    /// The viewport that build adjusted its used styles for.
    persist_viewport: Option<(f32, f32)>,
    /// `W3COS_PAINT_PERSIST_TABLE_SPANS`: that build's boxes in table
    /// subtrees (sorted), for the next plan's ranged marks.
    persist_tables: Option<std::sync::Arc<Vec<u32>>>,
    /// `W3COS_PAINT_PERSIST_PREFILL_SPANS`: that build's boxes that were not
    /// clean (sorted), when its plan kept every box's index.
    persist_dirty: Option<std::sync::Arc<Vec<u32>>>,
    /// During `append_all_reusing` only: the boxes this frame's layout
    /// splice touched ([`paint_touched_mode`]).
    touched: Option<crate::incremental_layout::PaintChanges>,
    /// `touched` is set and no box has column fragments: untouched gaps
    /// may be jumped in one step.
    touched_gaps: bool,
    /// Sorted indices of the boxes that repaint every frame
    /// ([`paint_node_volatile`]), recorded by the reusing build when
    /// `W3COS_PAINT_DIFF_SPARSE` is on.
    volatile_nodes: Option<Vec<u32>>,
    /// Sorted indices of the boxes with text fragments.
    fragment_nodes: Vec<u32>,
    /// What the line-start trim and the ordinal pass decided, kept (with
    /// `W3COS_PAINT_RANGE_REBUILD`) so the next frame re-runs them only for
    /// the boxes its layout splice touched.
    range_record: Option<std::sync::Arc<RangeRecord>>,
    /// [`FEATURE_TRANSLATE_PERCENT`] and the other rare-feature bits some
    /// box of `nodes` may carry (a superset), kept with
    /// `W3COS_PAINT_FEATURE_SKIP` so the next build skips whole-tree passes
    /// for features no box has.
    feature_presence: Option<u8>,
}

/// Per build, for [`paint_range_rebuild_mode`].
#[derive(Debug, Default)]
struct RangeRecord {
    /// Layout boxes (before paint-only UA boxes).
    len: usize,
    /// Per layout box: the line-start trim removed leading white space.
    trimmed: Vec<bool>,
    /// No box had a bidi rank or a split positioned-inline owner: the
    /// ordinals were the identity.
    ordinals_plain: bool,
    /// Percentage padding was resolved (trim geometry then depends on
    /// ancestors' widths): the record is not reused.
    percent_padding: bool,
}

impl Default for PaintArtifact {
    fn default() -> Self {
        Self {
            nodes: Vec::new(),
            viewport_scroll: (0.0, 0.0),
            cull_band: None,
            culled_nodes: Vec::new(),
            body_index: None,
            canvas_background: Color::WHITE,
            canvas_background_style: None,
            canvas_background_source: None,
            canvas_background_positioning_rect: None,
            display_items: Vec::new(),
            chunks: Vec::new(),
            properties: PropertyTrees::default(),
            node_properties: Vec::new(),
            self_clip: Vec::new(),
            z_order: Vec::new(),
            paint_order: PaintOrderKeys::default(),
            paint_classes: std::sync::OnceLock::new(),
            logical_paint_ordinals: Vec::new(),
            viewport_attached_by_index: Vec::new(),
            sticky_owner: Vec::new(),
            rect_by_index: Vec::new(),
            column_fragments: Vec::new(),
            text_fragments: Vec::new(),
            vertical_text: Vec::new(),
            generation: 0,
            append_record: None,
            serial: 0,
            persist_trimmed: None,
            persist_percent_padding: false,
            persist_viewport: None,
            persist_tables: None,
            persist_dirty: None,
            touched: None,
            touched_gaps: false,
            volatile_nodes: None,
            fragment_nodes: Vec::new(),
            range_record: None,
            feature_presence: None,
        }
    }
}

const BIDI_LOGICAL_ORDER: &str = "--w3cos-internal-bidi-logical-order";

#[inline(always)]
fn bidi_logical_rank(node: &PaintNode) -> Option<usize> {
    node.style
        .custom_properties
        .as_ref()
        .filter(|properties| properties.may_contain(BIDI_LOGICAL_ORDER))
        .and_then(|properties| properties.get(BIDI_LOGICAL_ORDER))
        .and_then(|value| value.parse::<usize>().ok())
}

fn logical_paint_ordinals(nodes: &[PaintNode]) -> Vec<usize> {
    let rank = |index: usize| bidi_logical_rank(&nodes[index]);
    let mut ordinals: Vec<_> = (0..nodes.len()).collect();
    if !(0..nodes.len()).any(|index| rank(index).is_some()) {
        return ordinals;
    }
    // Recover logical tree traversal only for normalized bidi sibling runs.
    // Subtrees remain contiguous; indices and layout stay in visual order.
    let mut children = vec![Vec::new(); nodes.len() + 1];
    for (index, node) in nodes.iter().enumerate() {
        children[node
            .parent
            .filter(|parent| *parent < nodes.len())
            .unwrap_or(nodes.len())]
        .push(index);
    }
    for siblings in &mut children {
        if siblings.iter().all(|index| rank(*index).is_some()) {
            siblings.sort_by_key(|index| rank(*index));
        }
    }
    let mut stack: Vec<_> = children[nodes.len()].iter().rev().copied().collect();
    let mut ordinal = 0;
    while let Some(index) = stack.pop() {
        ordinals[index] = ordinal;
        ordinal += 1;
        stack.extend(children[index].iter().rev().copied());
    }
    ordinals
}

/// Per node, the property tree lengths before `append_node` ran for it and
/// the scroll node its own chunks paint under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AppendMark {
    transforms: u32,
    clips: u32,
    effects: u32,
    scrolls: u32,
    self_scroll: u32,
    /// Display items, chunks and paint-order levels before the node.
    items: u32,
    chunks: u32,
    levels: u32,
}

/// The previous build's per-node property-tree bookkeeping. Blink's
/// PaintController keeps each display item client's cached output and
/// copies it for a client that is not invalidated; the artifact build keeps
/// `append_node`'s per-node output the same way.
struct AppendRecord {
    marks: Vec<AppendMark>,
    memo: AppendMemo,
    /// `serial` of the artifact this build copied from (0: none).
    base: u64,
    /// Per node: its paint inputs other than geometry (parent, kind, style,
    /// sticky signal) and its property / paint-order output equal `base`'s.
    unchanged: Vec<bool>,
    /// Aligned runs copied from `base`: `[node start, node end, display
    /// item start, display item end)`, at the same indices in both.
    runs: Vec<[u32; 4]>,
    /// `W3COS_PAINT_CULL_RUN_GAPS`: sorted disjoint `[start, end)` boxes
    /// this build copied as culled runs (or inherited them through aligned
    /// runs): each pushed no property node and no display item, sat under
    /// the root's properties and was not viewport attached or sticky.
    cull_spans: Vec<CullSpan>,
}

/// One culled span of an append record (see `AppendRecord::cull_spans`).
/// With `W3COS_PAINT_CULL_SUBTREES` it also carries what Blink keeps as a
/// subtree's visual overflow: the vertical extent of its boxes relative to
/// its first box, so a span that only moved is culled with one test.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CullSpan {
    pub(crate) start: u32,
    pub(crate) end: u32,
    /// Lowest box top and highest box bottom, minus the first box's top.
    top: f32,
    bottom: f32,
    /// Visible boxes (those the band culled rather than hid).
    visible: u32,
    /// Parents before `start` of boxes in the span; `parent_count` 3: more
    /// than two (the span is not taken whole).
    parents: [u32; 2],
    parent_count: u8,
    /// The root properties every box sat under when checked.
    root: PaintProperties,
}

/// `W3COS_PAINT_APPEND_REUSE=1`: copy `append_node` output for nodes whose
/// inputs and ancestors' output are unchanged since the previous frame.
/// `W3COS_PAINT_PERSISTENT_NODES` (off by default; design in
/// ngtools/design-persistent-paint-nodes.md). Step P1, `=probe`: after the
/// full build, every box the layout splice did not change must have the
/// same adjusted paint node as last frame; counts and reports the boxes
/// that do not (the dependency rules the persistent path will need).
pub(crate) fn persistent_nodes_mode() -> u8 {
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(
        || match std::env::var("W3COS_PAINT_PERSISTENT_NODES").as_deref() {
            Ok("1" | "true") => 1,
            Ok("verify") => 2,
            Ok("probe") => 3,
            _ => 0,
        },
    )
}

thread_local! {
    /// (frames probed, clean boxes compared, clean boxes that differ).
    pub(crate) static PERSISTENT_NODES_PROBE: std::cell::Cell<(u64, u64, u64)> =
        const { std::cell::Cell::new((0, 0, 0)) };
}

/// `W3COS_PAINT_PERSIST_INPLACE=1|verify` (with the persistent path on):
/// a box clean in this splice and the last one keeps the whole node of the
/// artifact the last frame replaced (its kind and adjusted style) instead of
/// a node made from the flat tree; `=verify` also builds the nodes in full
/// and compares, like `W3COS_PAINT_PERSISTENT_NODES=verify`.
pub(crate) fn persist_inplace_mode() -> u8 {
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| {
        if !matches!(persistent_nodes_mode(), 1 | 2) {
            return 0;
        }
        match std::env::var("W3COS_PAINT_PERSIST_INPLACE").as_deref() {
            Ok("1" | "true") => 1,
            Ok("verify") => 2,
            _ => 0,
        }
    })
}

/// The in-place inputs (`persist_inplace_mode`): which boxes took a
/// recycled node, the flat-tree node of a box, and the trimmed boxes of the
/// artifact the recycled nodes come from (their kinds hold trimmed text).
pub(crate) struct PersistRaw<'a> {
    pub moved: &'a std::cell::RefCell<Vec<bool>>,
    pub raw: &'a dyn Fn(usize) -> PaintNode,
    pub stale_trimmed: Option<std::sync::Arc<Vec<u32>>>,
    /// The nodes were spliced (`W3COS_PAINT_NODES_SPLICE`): every box
    /// outside the last two frames' change ranges took its recycled node.
    pub spliced: bool,
}

thread_local! {
    /// (frames built on the persistent path, clean boxes taken).
    pub(crate) static PERSISTENT_NODES_STATS: std::cell::Cell<(u64, u64)> =
        const { std::cell::Cell::new((0, 0)) };
    pub(crate) static PERSISTENT_NODES_MISMATCHES: std::cell::Cell<u64> =
        const { std::cell::Cell::new(0) };
}

/// The persistent path's inputs: last build and its clean boxes (not in
/// this frame's layout changes; the root and body always rebuild, since
/// the canvas background reads their unadjusted style).
struct PersistPlan<'a> {
    previous: &'a PaintArtifact,
    clean: Vec<bool>,
    clean_count: usize,
    /// Last build's trimmed boxes, in this build's indices.
    trimmed: std::sync::Arc<Vec<u32>>,
    /// `W3COS_PAINT_PERSIST_REMAP`: where each box was in the previous
    /// build when the splices inserted or removed boxes (`None`: same index).
    moved_from: Option<Vec<u32>>,
    /// `W3COS_PAINT_PERSIST_DIRTY_SCAN`: the boxes that are not clean, in
    /// order, so the per-box passes visit only them.
    dirty: Option<Vec<u32>>,
    /// `W3COS_PAINT_PERSIST_TABLE_SPANS`: the boxes in table subtrees.
    tables: Option<std::sync::Arc<Vec<u32>>>,
}

/// `W3COS_PAINT_PERSIST_DIRTY_SCAN=1|verify` (off by default; with the
/// persistent path on): the per-box passes of the reusing build that skip
/// clean boxes walk the list of boxes that are not clean instead of every
/// box (Blink: paint invalidation visits only the objects layout marked).
/// `verify` also runs each pass over every box, skipping clean ones, and
/// compares.
pub(crate) fn persist_dirty_scan_mode() -> u8 {
    #[cfg(test)]
    if let Some(mode) = PERSIST_DIRTY_SCAN_OVERRIDE.with(std::cell::Cell::get) {
        return mode;
    }
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| {
        match std::env::var("W3COS_PAINT_PERSIST_DIRTY_SCAN")
            .ok()
            .as_deref()
        {
            Some("1") => 1,
            Some("verify") => 2,
            _ => 0,
        }
    })
}

#[cfg(test)]
thread_local! {
    pub(crate) static PERSIST_DIRTY_SCAN_OVERRIDE: std::cell::Cell<Option<u8>> =
        const { std::cell::Cell::new(None) };
}

thread_local! {
    /// (builds that walked the dirty list, verify mismatches).
    static PERSIST_DIRTY_SCAN_STATS: std::cell::Cell<(u64, u64)> =
        const { std::cell::Cell::new((0, 0)) };
}

#[cfg(test)]
pub(crate) fn persist_dirty_scan_stats() -> (u64, u64) {
    PERSIST_DIRTY_SCAN_STATS.with(std::cell::Cell::get)
}

/// `W3COS_PAINT_PERSIST_TABLE_SPANS=1|verify` (off by default; with the
/// persistent path on, and no boxes inserted or removed): the plan's
/// table-subtree marks (table boxes always rebuild) come from the previous
/// build's list, recomputed only over the splice-changed ranges and their
/// descendants, instead of reading every box's style (Blink: invalidation
/// walks only what layout marked). The plan's dirty list then comes from
/// the same ranges. `verify` also marks every box and compares.
fn persist_table_spans_mode() -> u8 {
    #[cfg(test)]
    if let Some(mode) = PERSIST_TABLE_SPANS_OVERRIDE.with(std::cell::Cell::get) {
        return mode;
    }
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| {
        match std::env::var("W3COS_PAINT_PERSIST_TABLE_SPANS")
            .ok()
            .as_deref()
        {
            Some("1") => 1,
            Some("verify") => 2,
            _ => 0,
        }
    })
}

#[cfg(test)]
thread_local! {
    pub(crate) static PERSIST_TABLE_SPANS_OVERRIDE: std::cell::Cell<Option<u8>> =
        const { std::cell::Cell::new(None) };
}

thread_local! {
    /// (plans that took the ranged marks, verify mismatches).
    static PERSIST_TABLE_SPANS_STATS: std::cell::Cell<(u64, u64)> =
        const { std::cell::Cell::new((0, 0)) };
}

#[cfg(test)]
pub(crate) fn persist_table_spans_stats() -> (u64, u64) {
    PERSIST_TABLE_SPANS_STATS.with(std::cell::Cell::get)
}

/// `W3COS_PAINT_TABLE_PASS_SPANS=1|verify` (off by default; with the
/// persistent plan's table list, `W3COS_PAINT_PERSIST_TABLE_SPANS`): the
/// table passes find this frame's tables from the plan's list of boxes in
/// table subtrees instead of reading every box's display. `verify` also
/// scans every box and compares.
fn table_pass_spans_mode() -> u8 {
    #[cfg(test)]
    if let Some(mode) = TABLE_PASS_SPANS_OVERRIDE.with(std::cell::Cell::get) {
        return mode;
    }
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| {
        match std::env::var("W3COS_PAINT_TABLE_PASS_SPANS")
            .ok()
            .as_deref()
        {
            Some("1") => 1,
            Some("verify") => 2,
            _ => 0,
        }
    })
}

#[cfg(test)]
thread_local! {
    pub(crate) static TABLE_PASS_SPANS_OVERRIDE: std::cell::Cell<Option<u8>> =
        const { std::cell::Cell::new(None) };
}

thread_local! {
    /// (table passes that took the plan's list, verify mismatches).
    static TABLE_PASS_SPANS_STATS: std::cell::Cell<(u64, u64)> =
        const { std::cell::Cell::new((0, 0)) };
}

#[cfg(test)]
pub(crate) fn table_pass_spans_stats() -> (u64, u64) {
    TABLE_PASS_SPANS_STATS.with(std::cell::Cell::get)
}

/// `W3COS_PAINT_PERSIST_PREFILL_SPANS=1|verify` (off by default; with the
/// persistent path in place, spliced nodes and the dirty list): the clean
/// boxes take last build's style by visiting only the boxes last build did
/// not keep clean. A box clean in both builds holds the recycled node of the
/// build before (outside the last two frames' change ranges), whose style
/// last build took as it was. `verify` also visits every clean box and
/// compares.
fn persist_prefill_spans_mode() -> u8 {
    #[cfg(test)]
    if let Some(mode) = PERSIST_PREFILL_SPANS_OVERRIDE.with(std::cell::Cell::get) {
        return mode;
    }
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| {
        match std::env::var("W3COS_PAINT_PERSIST_PREFILL_SPANS")
            .ok()
            .as_deref()
        {
            Some("1") => 1,
            Some("verify") => 2,
            _ => 0,
        }
    })
}

#[cfg(test)]
thread_local! {
    pub(crate) static PERSIST_PREFILL_SPANS_OVERRIDE: std::cell::Cell<Option<u8>> =
        const { std::cell::Cell::new(None) };
}

thread_local! {
    /// (prefills over last build's dirty list, verify mismatches).
    static PERSIST_PREFILL_SPANS_STATS: std::cell::Cell<(u64, u64)> =
        const { std::cell::Cell::new((0, 0)) };
}

#[cfg(test)]
pub(crate) fn persist_prefill_spans_stats() -> (u64, u64) {
    PERSIST_PREFILL_SPANS_STATS.with(std::cell::Cell::get)
}

/// A table-internal or table box (the table passes add to its properties).
#[inline]
fn table_display(node: &PaintNode) -> bool {
    matches!(
        node.style.display,
        Display::Table
            | Display::InlineTable
            | Display::TableRowGroup
            | Display::TableHeaderGroup
            | Display::TableFooterGroup
            | Display::TableRow
            | Display::TableColumnGroup
            | Display::TableColumn
            | Display::TableCell
            | Display::TableCaption
    )
}

/// The boxes in a table subtree (sorted), over every box.
fn table_subtree_boxes(nodes: &[PaintNode]) -> Vec<u32> {
    let mut in_table = vec![false; nodes.len()];
    let mut list = Vec::new();
    for (index, node) in nodes.iter().enumerate() {
        if table_display(node)
            || node
                .parent
                .is_some_and(|parent| parent < index && in_table[parent])
        {
            in_table[index] = true;
            list.push(index as u32);
        }
    }
    list
}

/// `table_subtree_boxes` from the previous build's list: only the changed
/// ranges are marked again, and the subtree of a box whose mark flipped
/// (a box's mark depends only on its own display and its ancestors').
fn table_subtree_boxes_ranged(
    nodes: &[PaintNode],
    previous: &[u32],
    changed: &[(u32, u32)],
) -> Vec<u32> {
    let len = nodes.len();
    let mut ranges: Vec<(usize, usize)> = changed
        .iter()
        .map(|&(a, b)| (a as usize, (b as usize).min(len)))
        .filter(|(a, b)| a < b)
        .collect();
    ranges.sort_unstable();
    let mut in_table = vec![false; len];
    for &index in previous {
        if let Some(slot) = in_table.get_mut(index as usize) {
            *slot = true;
        }
    }
    // The regions marked again (sorted, disjoint).
    let mut redone: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
    let mut flipped = false;
    for (a, b) in ranges {
        let start = redone.last().map_or(a, |last| a.max(last.1));
        let mut until = b.max(start);
        let mut index = start;
        while index < until {
            let node = &nodes[index];
            let mark = table_display(node)
                || node
                    .parent
                    .is_some_and(|parent| parent < index && in_table[parent]);
            if mark != in_table[index] {
                in_table[index] = mark;
                flipped = true;
                // Preorder: its subtree runs while parents are at or after it.
                let mut end = index + 1;
                while end < len && nodes[end].parent.is_some_and(|parent| parent >= index) {
                    end += 1;
                }
                until = until.max(end);
            }
            index += 1;
        }
        match redone.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(until),
            _ if start < until => redone.push((start, until)),
            _ => {}
        }
    }
    if !flipped {
        return previous
            .iter()
            .copied()
            .filter(|&index| (index as usize) < len)
            .collect();
    }
    let mut list = Vec::with_capacity(previous.len());
    let mut kept = previous.iter().map(|&index| index as usize).peekable();
    for &(a, end) in &redone {
        while let Some(&index) = kept.peek().filter(|&&index| index < a) {
            list.push(index as u32);
            kept.next();
        }
        while kept.peek().is_some_and(|&index| index < end) {
            kept.next();
        }
        list.extend(
            (a..end)
                .filter(|&index| in_table[index])
                .map(|index| index as u32),
        );
    }
    list.extend(kept.filter(|&index| index < len).map(|index| index as u32));
    list
}

fn persist_dirty_scan_mismatch(what: &str) {
    PERSIST_DIRTY_SCAN_STATS.with(|stats| {
        let (builds, mismatches) = stats.get();
        stats.set((builds, mismatches + 1));
    });
    eprintln!("[paint-persist-dirty-scan-verify] mismatch: {what}");
}

/// The boxes a per-box pass visits: every box not in `skip`, or the dirty
/// list (the same boxes, in the same order) when there is one.
enum BoxScan<'a> {
    All(std::ops::Range<usize>, Option<&'a [bool]>),
    Dirty(std::slice::Iter<'a, u32>),
}

impl<'a> BoxScan<'a> {
    fn new(len: usize, skip: Option<&'a [bool]>, dirty: Option<&'a [u32]>) -> Self {
        match dirty {
            Some(dirty) => BoxScan::Dirty(dirty.iter()),
            None => BoxScan::All(0..len, skip),
        }
    }
}

impl Iterator for BoxScan<'_> {
    type Item = usize;
    #[inline]
    fn next(&mut self) -> Option<usize> {
        match self {
            BoxScan::All(range, skip) => {
                for index in range.by_ref() {
                    if !skip.is_some_and(|skip| skip[index]) {
                        return Some(index);
                    }
                }
                None
            }
            BoxScan::Dirty(iter) => iter.next().map(|&index| index as usize),
        }
    }
}

/// `W3COS_PAINT_PERSIST_REMAP=1` (with the persistent path on; off by
/// default): splices that insert or remove boxes keep the persistent path,
/// clean boxes taking the previous node at their old index (the splices'
/// index pieces). Checked by the persistent path's `=verify`.
fn persist_remap_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("W3COS_PAINT_PERSIST_REMAP").is_ok_and(|value| value == "1"))
}

impl PersistPlan<'_> {
    /// Where clean box `index` was in the previous build.
    #[inline]
    fn previous_index(&self, index: usize) -> usize {
        self.moved_from
            .as_ref()
            .map_or(index, |map| map[index] as usize)
    }

    /// Clean boxes take last build's adjusted style, and its trimmed text.
    /// `visit`: the only boxes whose style may differ from last build's
    /// (`W3COS_PAINT_PERSIST_PREFILL_SPANS`).
    fn prefill(&self, nodes: &mut [PaintNode], visit: Option<&[u32]>) {
        let take = |index: usize, node: &mut PaintNode| {
            let previous = &self.previous.nodes[self.previous_index(index)].style;
            if !node.style.ptr_eq(previous) {
                node.style = previous.clone();
            }
        };
        match visit {
            Some(visit) => {
                for &index in visit {
                    let index = index as usize;
                    if self.clean.get(index).copied().unwrap_or(false) {
                        take(index, &mut nodes[index]);
                    }
                }
            }
            None => {
                for (index, (node, clean)) in nodes.iter_mut().zip(&self.clean).enumerate() {
                    if *clean {
                        take(index, node);
                    }
                }
            }
        }
        for &index in self.trimmed.iter() {
            let index = index as usize;
            if self.clean[index] {
                nodes[index].kind = self.previous.nodes[self.previous_index(index)].kind.clone();
            }
        }
    }
}

fn persistent_nodes_plan<'a>(
    nodes: &[PaintNode],
    previous: Option<&'a PaintArtifact>,
    body_index: Option<usize>,
    viewport: Option<(f32, f32)>,
) -> Option<PersistPlan<'a>> {
    // `=verify` says why a build takes the full path (coverage checks).
    let full = |why: &str| {
        if persistent_nodes_mode() == 2 || persist_inplace_mode() == 2 {
            eprintln!("[paint-persistent-nodes-verify] full: {why}");
        }
        None
    };
    let Some(previous) = previous else {
        return full("no previous build");
    };
    let Some(trimmed) = previous.persist_trimmed.clone() else {
        return full("previous build not persistent");
    };
    // Used styles depend on the viewport (fixed backgrounds).
    if previous.persist_viewport != viewport {
        return full("viewport");
    }
    if previous.persist_percent_padding {
        return full("percentage padding");
    }
    let remap = persist_remap_enabled();
    if nodes.is_empty() || (!remap && previous.nodes.len() != nodes.len()) {
        return full("box count");
    }
    let Some((changed, segments, base_len)) = PAINT_TOUCHED.with(|touched| {
        touched
            .borrow()
            .as_ref()
            .filter(|changes| changes.len == nodes.len())
            .map(|changes| {
                (
                    changes.changed.clone(),
                    changes.segments.clone(),
                    changes.base_len,
                )
            })
    }) else {
        return full("no touched set");
    };
    let shifted = segments.iter().any(|&(_, offset)| offset != 0);
    if shifted && !remap {
        return full("boxes inserted or removed");
    }
    if (shifted || previous.nodes.len() != nodes.len()) && base_len != previous.nodes.len() {
        return full("box count");
    }
    let mut clean = vec![true; nodes.len()];
    for &(a, b) in &changed {
        clean[a as usize..(b as usize).min(nodes.len())].fill(false);
    }
    // New index -> previous index, from the splices' pieces.
    let moved_from = shifted.then(|| {
        let mut map = vec![u32::MAX; nodes.len()];
        for (k, &(from, offset)) in segments.iter().enumerate() {
            let to = segments
                .get(k + 1)
                .map_or(nodes.len(), |&(next, _)| next as usize)
                .min(nodes.len());
            for (index, slot) in map.iter_mut().enumerate().take(to).skip(from as usize) {
                let old = index as i64 + offset;
                if (0..previous.nodes.len() as i64).contains(&old) {
                    *slot = old as u32;
                }
            }
        }
        map
    });
    if let Some(map) = moved_from.as_ref() {
        for (clean, &old) in clean.iter_mut().zip(map) {
            if old == u32::MAX {
                *clean = false;
            }
        }
    }
    // Last build's trimmed boxes in this build's indices.
    let trimmed = match moved_from.as_ref() {
        None => trimmed,
        Some(map) => {
            let mut at_old = vec![false; previous.nodes.len()];
            for &old in trimmed.iter() {
                if let Some(slot) = at_old.get_mut(old as usize) {
                    *slot = true;
                }
            }
            std::sync::Arc::new(
                map.iter()
                    .enumerate()
                    .filter(|&(index, &old)| {
                        clean[index] && old != u32::MAX && at_old[old as usize]
                    })
                    .map(|(index, _)| index as u32)
                    .collect(),
            )
        }
    };
    // The table passes add to their boxes' properties (not idempotent on
    // an adjusted node): table subtrees always rebuild.
    let tables_mode = persist_table_spans_mode();
    let ranged = (tables_mode != 0 && moved_from.is_none())
        .then(|| previous.persist_tables.as_deref())
        .flatten()
        .map(|kept| table_subtree_boxes_ranged(nodes, kept, &changed));
    let tables = match (ranged, tables_mode) {
        (Some(ranged), 2) => {
            let full = table_subtree_boxes(nodes);
            let same = ranged == full;
            PERSIST_TABLE_SPANS_STATS.with(|stats| {
                let (plans, mismatches) = stats.get();
                stats.set((plans + 1, mismatches + u64::from(!same)));
            });
            if !same {
                eprintln!(
                    "[paint-persist-table-spans-verify] mismatch: {} ranged vs {} full table boxes",
                    ranged.len(),
                    full.len()
                );
            }
            (full, true)
        }
        (Some(ranged), _) => {
            PERSIST_TABLE_SPANS_STATS.with(|stats| {
                let (plans, mismatches) = stats.get();
                stats.set((plans + 1, mismatches));
            });
            (ranged, true)
        }
        (None, _) => (table_subtree_boxes(nodes), false),
    };
    let (tables, tables_ranged) = tables;
    for &index in &tables {
        clean[index as usize] = false;
    }
    clean[0] = false;
    if let Some(body) = body_index.filter(|body| *body < clean.len()) {
        clean[body] = false;
    }
    let (clean_count, dirty) = if tables_ranged && tables_mode == 1 {
        // Not clean: the changed ranges, table subtrees, the root and body.
        let mut dirty: Vec<u32> = changed
            .iter()
            .flat_map(|&(a, b)| a..b.min(nodes.len() as u32))
            .chain(tables.iter().copied())
            .chain(std::iter::once(0))
            .chain(
                body_index
                    .filter(|body| *body < clean.len())
                    .map(|body| body as u32),
            )
            .collect();
        dirty.sort_unstable();
        dirty.dedup();
        let count = clean.len() - dirty.len();
        (count, (persist_dirty_scan_mode() != 0).then_some(dirty))
    } else if persist_dirty_scan_mode() != 0 {
        let dirty: Vec<u32> = clean
            .iter()
            .enumerate()
            .filter(|(_, clean)| !**clean)
            .map(|(index, _)| index as u32)
            .collect();
        (clean.len() - dirty.len(), Some(dirty))
    } else {
        (clean.iter().filter(|clean| **clean).count(), None)
    };
    Some(PersistPlan {
        previous,
        clean,
        clean_count,
        trimmed,
        moved_from,
        dirty,
        tables: (tables_mode != 0).then(|| std::sync::Arc::new(tables)),
    })
}

type AdjustedNodes = (
    Vec<PaintNode>,
    Vec<Option<LayoutRect>>,
    Vec<usize>,
    Option<std::sync::Arc<RangeRecord>>,
    Option<u8>,
    Color,
    Option<Style>,
    Option<usize>,
    Option<LayoutRect>,
    usize,
    Option<std::sync::Arc<Vec<u32>>>,
    bool,
);

/// `=verify`: the persistent build equals the full one.
fn persistent_nodes_same(fast: &AdjustedNodes, full: &AdjustedNodes) -> bool {
    let nodes = fast.0.len() == full.0.len()
        && fast.0.iter().zip(&full.0).all(|(a, b)| {
            a.kind == b.kind
                && a.style.as_style() == b.style.as_style()
                && a.parent == b.parent
                && a.sticky_counter_signal == b.sticky_counter_signal
        });
    let rest = fast.1 == full.1
        && fast.2 == full.2
        && fast.4 == full.4
        && fast.5 == full.5
        && fast.6 == full.6
        && fast.7 == full.7
        && fast.8 == full.8
        && fast.9 == full.9
        && fast.10 == full.10
        && fast.11 == full.11;
    if !(nodes && rest) {
        let first = fast
            .0
            .iter()
            .zip(&full.0)
            .position(|(a, b)| a.kind != b.kind || a.style.as_style() != b.style.as_style());
        if let Some(first) = first {
            let (a, b) = (&fast.0[first], &full.0[first]);
            let fa = format!("{:#?}", a.style.as_style());
            let fb = format!("{:#?}", b.style.as_style());
            let diff: Vec<String> = fa
                .lines()
                .zip(fb.lines())
                .filter(|(x, y)| x != y)
                .take(6)
                .map(|(x, y)| format!("{} | {}", x.trim(), y.trim()))
                .collect();
            eprintln!(
                "[paint-persistent-nodes-verify] box {first}: kind same {} kind {:?} style diff (fast | full): {diff:?}",
                a.kind == b.kind,
                std::mem::discriminant(&b.kind)
            );
        }
        eprintln!(
            "[paint-persistent-nodes-verify] mismatch: nodes {nodes} rest {rest} first box {first:?} rects {} ordinals {} trimmed {}",
            fast.1 == full.1,
            fast.2 == full.2,
            fast.10 == full.10
        );
    }
    nodes && rest
}

fn persistent_nodes_probe(artifact: &PaintArtifact, previous: &PaintArtifact, source_len: usize) {
    if previous.nodes.len() != artifact.nodes.len() {
        eprintln!("[paint-persistent-nodes-probe] skip: node count");
        return;
    }
    let Some((changed, moved)) = PAINT_TOUCHED.with(|touched| {
        touched
            .borrow()
            .as_ref()
            .filter(|changes| changes.len == source_len)
            .map(|changes| (changes.changed.clone(), changes.moved.clone()))
    }) else {
        eprintln!("[paint-persistent-nodes-probe] skip: no layout changes");
        return;
    };
    let in_ranges = |ranges: &[(u32, u32)], index: usize| {
        ranges
            .iter()
            .any(|&(a, b)| index >= a as usize && index < b as usize)
    };
    let (mut compared, mut differ) = (0u64, 0u64);
    let mut shown = 0;
    for index in 0..artifact.nodes.len() {
        if index < source_len && in_ranges(&changed, index) {
            continue;
        }
        compared += 1;
        let (a, b) = (&previous.nodes[index], &artifact.nodes[index]);
        let kind = a.kind != b.kind;
        let style = a.style.as_style() != b.style.as_style();
        let tree = a.parent != b.parent || a.sticky_counter_signal != b.sticky_counter_signal;
        if kind || style || tree {
            differ += 1;
            if shown < 4 {
                shown += 1;
                let moved = index < source_len && in_ranges(&moved, index);
                let props = |node: &PaintNode| {
                    node.style
                        .custom_properties
                        .as_ref()
                        .map(|p| format!("{p:?}"))
                        .unwrap_or_default()
                };
                eprintln!(
                    "[paint-persistent-nodes-probe] box {index}/{len} differs: kind {kind} style {style} tree {tree} moved {moved} paint_only {po} kind {k:?}{extra}",
                    len = artifact.nodes.len(),
                    po = index >= source_len,
                    k = std::mem::discriminant(&b.kind),
                    extra = if style && props(a) != props(b) {
                        format!(" props {} -> {}", props(a), props(b))
                    } else {
                        String::new()
                    },
                );
            }
        }
    }
    PERSISTENT_NODES_PROBE.with(|stats| {
        let (f, c, d) = stats.get();
        stats.set((f + 1, c + compared, d + differ));
    });
    eprintln!(
        "[paint-persistent-nodes-probe] frame: {differ} of {compared} unchanged boxes differ (changed {changed:?})"
    );
}

/// A fresh, nonzero [`PaintArtifact::serial`] for a new build.
fn next_artifact_serial() -> u64 {
    static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

pub(crate) fn paint_append_reuse_enabled() -> bool {
    #[cfg(test)]
    if let Some(enabled) = APPEND_REUSE_OVERRIDE.with(std::cell::Cell::get) {
        return enabled;
    }
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("W3COS_PAINT_APPEND_REUSE").is_some_and(|v| v == "1"))
        || artifact_splice_enabled()
}

/// `W3COS_PAINT_ARTIFACT_SPLICE`: with append reuse, copy each run of
/// unchanged, aligned nodes as slices of last frame's output (`=verify`
/// also builds in full and compares, like `W3COS_PAINT_APPEND_REUSE_VERIFY`).
/// On by default (last-message stream frame -9.5% wall, 13/16 rounds);
/// `=0` turns it off.
pub(crate) fn artifact_splice_enabled() -> bool {
    #[cfg(test)]
    if let Some(enabled) = ARTIFACT_SPLICE_OVERRIDE.with(std::cell::Cell::get) {
        return enabled;
    }
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        !std::env::var_os("W3COS_PAINT_ARTIFACT_SPLICE").is_some_and(|v| v == "0" || v == "false")
    })
}

thread_local! {
    /// Nodes copied in aligned runs since the thread started.
    static ARTIFACT_SPLICE_RUN_NODES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
thread_local! {
    pub(crate) static ARTIFACT_SPLICE_OVERRIDE: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

/// `W3COS_PAINT_APPEND_REUSE_VERIFY=1`: also run the full build and compare.
fn paint_append_reuse_verify() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("W3COS_PAINT_APPEND_REUSE_VERIFY").is_some_and(|v| v == "1")
            || std::env::var_os("W3COS_PAINT_ARTIFACT_SPLICE").is_some_and(|v| v == "verify")
    })
}

#[cfg(test)]
thread_local! {
    static APPEND_REUSE_OVERRIDE: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

thread_local! {
    /// (nodes copied, nodes appended, verify mismatches) since start.
    static APPEND_REUSE_STATS: std::cell::Cell<(u64, u64, u64)> =
        const { std::cell::Cell::new((0, 0, 0)) };
}

#[cfg(test)]
pub(crate) fn append_reuse_stats() -> (u64, u64, u64) {
    APPEND_REUSE_STATS.with(std::cell::Cell::get)
}

/// Per-build ancestor lookups for `append_node`, filled in preorder.
struct AppendMemo {
    /// Nearest ancestor-or-self that is a fixed box with `z-index: auto`.
    fixed_owner: Vec<Option<usize>>,
    /// Nearest ancestor-or-self whose paint order key children extend.
    context_owner: Vec<Option<usize>>,
    /// Per node: a split positioned inline fragment (looked up once; the
    /// paint order key reads it several times per node).
    split_owner: Vec<bool>,
    /// Per appended node: it owns a paint context (its key ends with its
    /// own background level).
    owns_context: Vec<bool>,
    /// Per appended node: it establishes a stacking context.
    stacking: Vec<bool>,
    /// Per appended node: the local paint phase its descendants inherit
    /// from the ancestor walk in `local_paint_order_level` when that walk
    /// reaches it (`ANCESTOR_PHASE_NONE` when the walk stops there or runs
    /// out of ancestors without deciding).
    ancestor_phase: Vec<u8>,
}

const ANCESTOR_PHASE_NONE: u8 = 0;

impl AppendMemo {
    fn new(nodes: &[PaintNode]) -> Self {
        let len = nodes.len();
        Self {
            fixed_owner: vec![None; len],
            context_owner: vec![None; len],
            split_owner: nodes
                .iter()
                .map(|node| split_positioned_inline_owner(node).is_some())
                .collect(),
            owns_context: vec![false; len],
            stacking: vec![false; len],
            ancestor_phase: vec![ANCESTOR_PHASE_NONE; len],
        }
    }

    /// [`AppendMemo::new`] on top of last build's memo: with
    /// `W3COS_PAINT_SPLIT_OWNER_REUSE`, a box outside the layout's `changed`
    /// ranges kept its inputs (the same rule as `inputs_match`), so its split
    /// positioned-inline owner flag is last build's and only the changed
    /// boxes look at their custom properties. Taken only when the boxes kept
    /// their indices (no splice offset). `verify` scans every box too.
    fn new_reusing(
        nodes: &[PaintNode],
        previous: &AppendMemo,
        touched: Option<&crate::incremental_layout::PaintChanges>,
    ) -> Self {
        let mode = paint_split_owner_reuse_mode();
        let len = nodes.len();
        let Some(touched) = touched.filter(|touched| {
            mode != 0
                && touched.len == len
                && previous.split_owner.len() == len
                && touched.segments.iter().all(|&(_, offset)| offset == 0)
        }) else {
            return Self::new(nodes);
        };
        let mut split_owner = previous.split_owner.clone();
        for &(start, end) in &touched.changed {
            for index in start as usize..(end as usize).min(len) {
                split_owner[index] = split_positioned_inline_owner(&nodes[index]).is_some();
            }
        }
        split_owner_reuse_count(0);
        if mode == 2 {
            let full: Vec<bool> = nodes
                .iter()
                .map(|node| split_positioned_inline_owner(node).is_some())
                .collect();
            if full != split_owner {
                eprintln!(
                    "[paint-split-owner-reuse-verify] mismatch: an untouched box changed its split owner"
                );
                split_owner_reuse_count(1);
                split_owner = full;
            }
        }
        Self {
            fixed_owner: vec![None; len],
            context_owner: vec![None; len],
            split_owner,
            owns_context: vec![false; len],
            stacking: vec![false; len],
            ancestor_phase: vec![ANCESTOR_PHASE_NONE; len],
        }
    }
}

/// `W3COS_PAINT_SPLIT_OWNER_REUSE=1|verify` (default off): the append memo
/// takes last build's split positioned-inline owner flags for boxes the
/// layout did not touch instead of reading every box's custom properties.
pub(crate) fn paint_split_owner_reuse_mode() -> u8 {
    #[cfg(test)]
    if let Some(mode) = PAINT_SPLIT_OWNER_REUSE_OVERRIDE.with(std::cell::Cell::get) {
        return mode;
    }
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| {
        match std::env::var("W3COS_PAINT_SPLIT_OWNER_REUSE")
            .ok()
            .as_deref()
        {
            Some("1") => 1,
            Some("verify") => 2,
            _ => 0,
        }
    })
}

#[cfg(test)]
thread_local! {
    pub(crate) static PAINT_SPLIT_OWNER_REUSE_OVERRIDE: std::cell::Cell<Option<u8>> =
        const { std::cell::Cell::new(None) };
}

thread_local! {
    /// (memos built on last build's flags, verify mismatches).
    pub(crate) static PAINT_SPLIT_OWNER_REUSE_STATS: std::cell::Cell<(u64, u64)> =
        const { std::cell::Cell::new((0, 0)) };
}

fn split_owner_reuse_count(field: usize) {
    PAINT_SPLIT_OWNER_REUSE_STATS.with(|stats| {
        let mut value = stats.get();
        if field == 0 {
            value.0 += 1;
        } else {
            value.1 += 1;
        }
        stats.set(value);
    });
}

// Inlined: almost every node has no custom properties, so the check is a
// pointer test at the call site instead of a call per node.
#[inline(always)]
fn split_positioned_inline_owner(node: &PaintNode) -> Option<&str> {
    const KEY: &str = "--w3cos-internal-positioned-inline-owner";
    node.style
        .custom_properties
        .as_ref()
        .filter(|properties| properties.may_contain(KEY))?
        .get(KEY)
        .map(String::as_str)
        .filter(|owner| !owner.is_empty())
}

/// [`positioned_inline_fragment_ordinals`] when only `scan` can hold a
/// bidi rank or a split positioned-inline owner (every other box kept the
/// inputs of a build that had none): the identity unless one is found.
/// Returns the ordinals and whether they are the identity.
fn positioned_inline_fragment_ordinals_ranged(
    nodes: &[PaintNode],
    scan: Option<&[(u32, u32)]>,
    tail: usize,
) -> (Vec<usize>, bool) {
    let special = |node: &PaintNode| {
        bidi_logical_rank(node).is_some() || split_positioned_inline_owner(node).is_some()
    };
    let found = match scan {
        Some(ranges) => ranges
            .iter()
            .flat_map(|&(a, b)| a as usize..(b as usize).min(nodes.len()))
            .chain(tail.min(nodes.len())..nodes.len())
            .any(|index| special(&nodes[index])),
        None => nodes.iter().any(special),
    };
    if !found {
        return ((0..nodes.len()).collect(), true);
    }
    (positioned_inline_fragment_ordinals(nodes), false)
}

fn positioned_inline_fragment_ordinals(nodes: &[PaintNode]) -> Vec<usize> {
    let mut ordinals = logical_paint_ordinals(nodes);
    // One owner lookup per node; the grouping below only visits the split
    // fragments it found.
    let mut split: Vec<(usize, &str)> = Vec::new();
    for (index, node) in nodes.iter().enumerate() {
        if let Some(owner) = split_positioned_inline_owner(node) {
            split.push((index, owner));
        }
    }
    if split.is_empty() {
        return ordinals;
    }
    let mut groups = std::collections::HashMap::new();
    for &(index, owner) in &split {
        groups
            .entry((nodes[index].parent, owner))
            .and_modify(|first: &mut usize| *first = (*first).min(ordinals[index]))
            .or_insert(ordinals[index]);
    }
    for &(index, owner) in &split {
        ordinals[index] = groups[&(nodes[index].parent, owner)];
    }
    ordinals
}

fn background_position_uses_relative_basis(position: &str) -> bool {
    position.split_ascii_whitespace().any(|token| {
        token.contains('%')
            || matches!(
                token.to_ascii_lowercase().as_str(),
                "center" | "right" | "bottom"
            )
    })
}

fn inline_fragment_clip_rect(
    kind: &ComponentKind,
    style: &Style,
    rect: LayoutRect,
) -> Option<LayoutRect> {
    let internal_clip = style
        .custom_properties
        .as_ref()
        .and_then(|properties| properties.get("--w3cos-internal-inline-fragment-clip"))
        .and_then(|value| {
            let mut parts = value.split_ascii_whitespace();
            let alignment = parts.next()?;
            let height = parts.next()?.parse::<f32>().ok()?;
            Some((alignment, height))
        });
    if matches!(
        kind,
        ComponentKind::Image { .. }
            | ComponentKind::Canvas { .. }
            | ComponentKind::SvgDocument { .. }
    ) {
        return None;
    }
    // Vertical alignment positions an inline box; it does not clip its
    // background, borders, or overflowing descendants. Only an explicit
    // fragment boundary from inline splitting creates a paint clip.
    let (alignment, height) = internal_clip?;
    let height = height.clamp(0.0, rect.height);
    if height >= rect.height {
        return None;
    }
    Some(LayoutRect {
        x: rect.x,
        y: if alignment.eq_ignore_ascii_case("bottom") {
            rect.y + rect.height - height
        } else {
            rect.y
        },
        width: rect.width,
        height,
    })
}

fn css2_clip_rect(style: &Style, rect: LayoutRect) -> Option<LayoutRect> {
    if !matches!(style.position, Position::Absolute | Position::Fixed) {
        return None;
    }
    let clip = style.clip?;
    let resolve = |side: Option<w3cos_std::style::Dimension>, auto: f32| match side {
        Some(w3cos_std::style::Dimension::Px(value)) => value,
        Some(w3cos_std::style::Dimension::Rem(value)) => value * 16.0,
        Some(w3cos_std::style::Dimension::Em(value)) => value * style.font_size,
        Some(w3cos_std::style::Dimension::Ch(value)) => value * style.font_size * 0.5,
        Some(w3cos_std::style::Dimension::Vw(value)) => value * rect.width / 100.0,
        Some(w3cos_std::style::Dimension::Vh(value)) => value * rect.height / 100.0,
        Some(w3cos_std::style::Dimension::Lh(value)) => value * style.used_line_height_px(),
        Some(w3cos_std::style::Dimension::Percent(value)) => value * auto / 100.0,
        Some(w3cos_std::style::Dimension::Auto | w3cos_std::style::Dimension::Calc { .. })
        | None => auto,
    };
    let top = resolve(clip.top, 0.0);
    let right = resolve(clip.right, rect.width);
    let bottom = resolve(clip.bottom, rect.height);
    let left = resolve(clip.left, 0.0);
    Some(LayoutRect {
        x: rect.x + left,
        y: rect.y + top,
        width: (right - left).max(0.0),
        height: (bottom - top).max(0.0),
    })
}

/// The table paint fix-ups (collapsed border resolution, track projection,
/// joints, empty-row extension, hidden empty cells, separated column
/// backgrounds), with a per-table memo: like Blink's `NGTableBorders`,
/// cached on the table and recomputed only when the table changes, a table
/// whose subtree inputs equal the last build's reuses that build's results
/// (on by default, `W3COS_PAINT_TABLE_MEMO=0` turns it off;
/// `W3COS_PAINT_TABLE_MEMO_VERIFY=1` also runs the passes and reports
/// differing styles).
mod table_passes {
    use super::*;
    use std::cell::RefCell;

    fn passes(nodes: &mut [PaintNode], rect_by_index: &[Option<LayoutRect>]) {
        suppress_improper_nested_table_part_backgrounds(nodes);
        suppress_hidden_empty_cell_paint(nodes);
        project_collapsed_table_tracks_to_cells(nodes);
        resolve_collapsed_cell_border_conflicts(nodes);
        annotate_collapsed_border_joints(nodes, rect_by_index);
        extend_collapsed_borders_across_empty_rows(nodes, rect_by_index);
        annotate_separated_table_background_fragments(nodes, rect_by_index);
    }

    fn flag(name: &str) -> bool {
        std::env::var(name).is_ok_and(|value| value == "1")
    }

    fn enabled() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| {
            std::env::var("W3COS_PAINT_TABLE_MEMO").map_or(true, |value| value != "0")
        })
    }

    fn verify() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| flag("W3COS_PAINT_TABLE_MEMO_VERIFY"))
    }

    pub(super) fn is_table_part(display: Display) -> bool {
        table_part(display)
    }

    fn table_part(display: Display) -> bool {
        matches!(
            display,
            Display::TableRowGroup
                | Display::TableHeaderGroup
                | Display::TableFooterGroup
                | Display::TableRow
                | Display::TableColumnGroup
                | Display::TableColumn
                | Display::TableCell
                | Display::TableCaption
        )
    }

    /// Outermost tables as preorder ranges, or `None` when a table part
    /// sits outside every table (the passes then read across the tree).
    pub(super) fn tables(nodes: &[PaintNode]) -> Option<Vec<std::ops::Range<usize>>> {
        let mut ranges = Vec::new();
        let mut index = 0;
        while index < nodes.len() {
            let display = nodes[index].style.display;
            if matches!(display, Display::Table | Display::InlineTable) {
                let mut end = index + 1;
                while end < nodes.len() && nodes[end].parent.is_some_and(|parent| parent >= index) {
                    end += 1;
                }
                ranges.push(index..end);
                index = end;
                continue;
            }
            if table_part(display) {
                return None;
            }
            index += 1;
        }
        Some(ranges)
    }

    /// `tables` from the boxes in table subtrees (sorted): the outermost
    /// tables are the listed table boxes outside an earlier table's range,
    /// and a listed box outside every table range is a stray table part.
    pub(super) fn tables_from_boxes(
        nodes: &[PaintNode],
        boxes: &[u32],
    ) -> Option<Vec<std::ops::Range<usize>>> {
        let mut ranges: Vec<std::ops::Range<usize>> = Vec::new();
        for &index in boxes {
            let index = index as usize;
            if index >= nodes.len() {
                return None;
            }
            if ranges.last().is_some_and(|last| index < last.end) {
                continue;
            }
            let display = nodes[index].style.display;
            if matches!(display, Display::Table | Display::InlineTable) {
                let mut end = index + 1;
                while end < nodes.len() && nodes[end].parent.is_some_and(|parent| parent >= index) {
                    end += 1;
                }
                ranges.push(index..end);
                continue;
            }
            // A table part, or a box under one, outside every table.
            return None;
        }
        Some(ranges)
    }

    /// This frame's tables: from the plan's list when given
    /// (`W3COS_PAINT_TABLE_PASS_SPANS`), else over every box.
    fn frame_tables(
        nodes: &[PaintNode],
        boxes: Option<&[u32]>,
    ) -> Option<Vec<std::ops::Range<usize>>> {
        let mode = table_pass_spans_mode();
        let Some(boxes) = boxes.filter(|_| mode != 0) else {
            return tables(nodes);
        };
        let listed = tables_from_boxes(nodes, boxes);
        let mut mismatch = false;
        let listed = if mode == 2 {
            let full = tables(nodes);
            if full != listed {
                mismatch = true;
                eprintln!(
                    "[paint-table-pass-spans-verify] mismatch: {:?} listed vs {:?} full tables",
                    listed.as_ref().map(Vec::len),
                    full.as_ref().map(Vec::len)
                );
            }
            full
        } else {
            listed
        };
        TABLE_PASS_SPANS_STATS.with(|stats| {
            let (passes, mismatches) = stats.get();
            stats.set((passes + 1, mismatches + u64::from(mismatch)));
        });
        listed
    }

    struct Input {
        kind: ComponentKind,
        style: w3cos_std::component::ComponentStyle,
        parent: Option<usize>,
        rect: Option<[u32; 4]>,
    }

    struct Entry {
        root_rect: Option<LayoutRect>,
        inputs: Vec<Input>,
        outputs: Vec<w3cos_std::component::ComponentStyle>,
        /// No result holds absolute coordinates (separated column background
        /// fragments do), so a moved table keeps its results.
        translation_safe: bool,
    }

    thread_local! {
        static MEMO: RefCell<Vec<Entry>> = const { RefCell::new(Vec::new()) };
    }

    fn relative(rect: Option<LayoutRect>, root: Option<LayoutRect>) -> Option<[u32; 4]> {
        let rect = rect?;
        let (x, y) = root.map_or((0.0, 0.0), |root| (root.x, root.y));
        Some([
            (rect.x - x).to_bits(),
            (rect.y - y).to_bits(),
            rect.width.to_bits(),
            rect.height.to_bits(),
        ])
    }

    fn rect_bits(rect: Option<LayoutRect>) -> Option<[u32; 4]> {
        rect.map(|rect| {
            [
                rect.x.to_bits(),
                rect.y.to_bits(),
                rect.width.to_bits(),
                rect.height.to_bits(),
            ]
        })
    }

    fn input(
        nodes: &[PaintNode],
        rects: &[Option<LayoutRect>],
        range: &std::ops::Range<usize>,
        index: usize,
    ) -> Input {
        let root = rects.get(range.start).copied().flatten();
        let node = &nodes[index];
        Input {
            kind: node.kind.clone(),
            style: node.style.clone(),
            parent: node
                .parent
                .filter(|parent| *parent >= range.start)
                .map(|parent| parent - range.start),
            rect: relative(rects.get(index).copied().flatten(), root),
        }
    }

    fn hit(
        entry: &Entry,
        nodes: &[PaintNode],
        rects: &[Option<LayoutRect>],
        range: &std::ops::Range<usize>,
    ) -> bool {
        let root = rects.get(range.start).copied().flatten();
        entry.inputs.len() == range.len()
            && (entry.translation_safe || rect_bits(entry.root_rect) == rect_bits(root))
            && range.clone().zip(&entry.inputs).all(|(index, input)| {
                let node = &nodes[index];
                node.parent
                    .filter(|parent| *parent >= range.start)
                    .map(|parent| parent - range.start)
                    == input.parent
                    && relative(rects.get(index).copied().flatten(), root) == input.rect
                    && node.style == input.style
                    && node.kind == input.kind
            })
    }

    pub(super) fn run(
        nodes: &mut [PaintNode],
        rects: &[Option<LayoutRect>],
        boxes: Option<&[u32]>,
    ) {
        if !enabled() {
            passes(nodes, rects);
            return;
        }
        let Some(ranges) = frame_tables(nodes, boxes) else {
            MEMO.with(|memo| memo.borrow_mut().clear());
            passes(nodes, rects);
            return;
        };
        let hits: Vec<bool> = MEMO.with(|memo| {
            let memo = memo.borrow();
            if memo.len() != ranges.len() {
                return vec![false; ranges.len()];
            }
            ranges
                .iter()
                .zip(memo.iter())
                .map(|(range, entry)| hit(entry, nodes, rects, range))
                .collect()
        });
        if hits.iter().all(|hit| *hit) && !verify() {
            MEMO.with(|memo| {
                for (range, entry) in ranges.iter().zip(memo.borrow().iter()) {
                    for (index, output) in range.clone().zip(&entry.outputs) {
                        nodes[index].style = output.clone();
                    }
                }
            });
            return;
        }
        let inputs: Vec<Option<Vec<Input>>> = ranges
            .iter()
            .zip(&hits)
            .map(|(range, hit)| {
                (!hit).then(|| {
                    range
                        .clone()
                        .map(|index| input(nodes, rects, range, index))
                        .collect()
                })
            })
            .collect();
        passes(nodes, rects);
        MEMO.with(|memo| {
            let mut memo = memo.borrow_mut();
            if verify() {
                let mut differing = 0usize;
                let mut reused = 0usize;
                for ((range, hit), entry) in ranges.iter().zip(&hits).zip(memo.iter()) {
                    if !hit {
                        continue;
                    }
                    reused += 1;
                    differing += range
                        .clone()
                        .zip(&entry.outputs)
                        .filter(|(index, output)| nodes[*index].style != **output)
                        .count();
                }
                eprintln!(
                    "[table-memo-verify] tables {} reused {reused} differing styles {differing}",
                    ranges.len()
                );
            }
            if memo.len() != ranges.len() {
                memo.clear();
            }
            for (k, (range, inputs)) in ranges.iter().zip(inputs).enumerate() {
                let Some(inputs) = inputs else {
                    continue;
                };
                let outputs: Vec<_> = range
                    .clone()
                    .map(|index| nodes[index].style.clone())
                    .collect();
                let translation_safe = outputs.iter().all(|style| {
                    style.custom_properties.as_ref().is_none_or(|properties| {
                        !properties.contains_key(TABLE_BACKGROUND_FRAGMENTS)
                    })
                });
                let entry = Entry {
                    root_rect: rects.get(range.start).copied().flatten(),
                    inputs,
                    outputs,
                    translation_safe,
                };
                if k < memo.len() {
                    memo[k] = entry;
                } else {
                    memo.push(entry);
                }
            }
        });
    }
}

fn suppress_improper_nested_table_part_backgrounds(nodes: &mut [PaintNode]) {
    for index in 0..nodes.len() {
        if !matches!(
            nodes[index].style.display,
            Display::TableRowGroup
                | Display::TableHeaderGroup
                | Display::TableFooterGroup
                | Display::TableRow
                | Display::TableColumnGroup
                | Display::TableColumn
        ) {
            continue;
        }
        let mut parent = nodes[index].parent;
        let mut nested_in_cell = false;
        while let Some(parent_index) = parent {
            match nodes[parent_index].style.display {
                Display::TableCell => {
                    nested_in_cell = true;
                    break;
                }
                Display::Table | Display::InlineTable => break,
                _ => parent = nodes[parent_index].parent,
            }
        }
        if nested_in_cell {
            // CSS table fixup may place an improper internal table part inside
            // an anonymous cell. Row/row-group/column backgrounds are table
            // layer backgrounds, not independent box fills, so without cells
            // of their own they contribute no painted area.
            nodes[index].style.background = Color::TRANSPARENT;
            nodes[index].style.background_image = None;
        }
    }
}

fn suppress_hidden_empty_cell_paint(nodes: &mut [PaintNode]) {
    let has_child = nodes
        .iter()
        .filter_map(|node| node.parent)
        .collect::<std::collections::HashSet<_>>();
    for (index, node) in nodes.iter_mut().enumerate() {
        if node.style.display != Display::TableCell
            || node.style.border_collapse
            || !node.style.empty_cells_hide
            || has_child.contains(&index)
        {
            continue;
        }
        node.style.background = Color::TRANSPARENT;
        node.style.background_image = None;
        node.style.border_width = 0.0;
        node.style.border_top_width = None;
        node.style.border_right_width = None;
        node.style.border_bottom_width = None;
        node.style.border_left_width = None;
    }
}

/// Per-node used-style adjustments of [`PaintArtifact::build_reusing`]
/// (fixed-background viewport, used border widths); returns whether any
/// padding needs a percentage basis. They read the style alone (and the
/// viewport), so with `W3COS_STYLE_ADJUST_MEMO` a node whose shared style
/// is the allocation seen at its index last frame reuses that result.
fn adjust_used_styles(
    nodes: &mut [PaintNode],
    viewport: Option<(f32, f32)>,
    // Boxes that already hold last build's used style
    // (`W3COS_PAINT_PERSISTENT_NODES` clean boxes; none had percentage
    // padding).
    skip: Option<&[bool]>,
    // The boxes not in `skip`, in order (`W3COS_PAINT_PERSIST_DIRTY_SCAN`).
    dirty: Option<&[u32]>,
) -> bool {
    let mode = style_adjust_memo_mode();
    if mode == 0 {
        let mut has_percentage_padding = false;
        for index in BoxScan::new(nodes.len(), skip, dirty) {
            has_percentage_padding |= adjust_used_style(&mut nodes[index], viewport);
        }
        return has_percentage_padding;
    }
    STYLE_ADJUST_MEMO.with(|memo| {
        let mut memo = memo.borrow_mut();
        if memo.viewport != viewport {
            memo.entries.clear();
            memo.viewport = viewport;
        }
        memo.entries.resize_with(nodes.len(), || None);
        let mut has_percentage_padding = false;
        for index in BoxScan::new(nodes.len(), skip, dirty) {
            let node = &mut nodes[index];
            let entry = &mut memo.entries[index];
            if let Some(known) = entry
                && known.source.ptr_eq(&node.style)
            {
                if mode == 2 {
                    let mut fresh = node.clone();
                    let percent = adjust_used_style(&mut fresh, viewport);
                    let adjusted = known.adjusted.as_ref().unwrap_or(&known.source);
                    if percent != known.percent_padding || *fresh.style != **adjusted {
                        eprintln!("[style-adjust-memo] mismatch");
                        STYLE_ADJUST_MEMO_MISMATCHES.with(|count| count.set(count.get() + 1));
                    }
                }
                if let Some(adjusted) = &known.adjusted {
                    node.style = adjusted.clone();
                }
                has_percentage_padding |= known.percent_padding;
                continue;
            }
            let source = node.style.clone();
            let percent_padding = adjust_used_style(node, viewport);
            let adjusted = (!node.style.ptr_eq(&source)).then(|| node.style.clone());
            *entry = Some(StyleAdjustEntry {
                source,
                adjusted,
                percent_padding,
            });
            has_percentage_padding |= percent_padding;
        }
        has_percentage_padding
    })
}

fn adjust_used_style(node: &mut PaintNode, viewport: Option<(f32, f32)>) -> bool {
    // Write through the shared style only when a value changes.
    if crate::background_image::fixed_background_viewport_touches(&node.style) {
        crate::background_image::annotate_fixed_background_viewport(
            node.style.make_mut(),
            viewport,
        );
    }
    if node.style.needs_used_border_widths() {
        node.style.make_mut().resolve_used_border_widths();
    }
    [
        node.style.padding.top,
        node.style.padding.right,
        node.style.padding.bottom,
        node.style.padding.left,
    ]
    .into_iter()
    .any(|edge| matches!(edge, Spacing::Percent(_) | Spacing::PaddingCalc { .. }))
}

struct StyleAdjustEntry {
    /// Held so the allocation (and its address) stays this style's.
    source: w3cos_std::component::ComponentStyle,
    adjusted: Option<w3cos_std::component::ComponentStyle>,
    percent_padding: bool,
}

#[derive(Default)]
struct StyleAdjustMemo {
    viewport: Option<(f32, f32)>,
    entries: Vec<Option<StyleAdjustEntry>>,
}

thread_local! {
    static STYLE_ADJUST_MEMO: std::cell::RefCell<StyleAdjustMemo> = Default::default();
    static STYLE_ADJUST_MEMO_MISMATCHES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// `W3COS_STYLE_ADJUST_MEMO` (default on; `=0` off); `=verify` recomputes each
/// reused result and reports a difference.
fn style_adjust_memo_mode() -> u8 {
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(
        || match std::env::var("W3COS_STYLE_ADJUST_MEMO").as_deref() {
            Ok("0") => 0,
            Ok("verify") => 2,
            _ => 1,
        },
    )
}

fn trim_collapsible_inline_whitespace_at_line_start(
    nodes: &mut [PaintNode],
    rect_by_index: &[Option<LayoutRect>],
) {
    trim_line_start_white_space_ranged(nodes, rect_by_index, None, None, None, None);
}

/// [`trim_collapsible_inline_whitespace_at_line_start`], recording per box
/// whether it trimmed. With `aligned` (this frame's touched boxes and the
/// previous build's record, for the same boxes), a box whose own and whose
/// inline ancestors' and line-start container's inputs and geometry the
/// layout splice did not touch takes the previous decision instead of
/// re-testing its line-start geometry.
fn trim_line_start_white_space_ranged(
    nodes: &mut [PaintNode],
    rect_by_index: &[Option<LayoutRect>],
    aligned: Option<(&[bool], &[bool])>,
    mut record: Option<&mut Vec<bool>>,
    // Boxes left as they are (`W3COS_PAINT_PERSISTENT_NODES` clean boxes,
    // which already hold last build's trimmed text).
    skip: Option<&[bool]>,
    // The boxes not in `skip`, in order (`W3COS_PAINT_PERSIST_DIRTY_SCAN`).
    dirty: Option<&[u32]>,
) -> u32 {
    const COLLAPSIBLE: [char; 5] = [' ', '\t', '\n', '\r', '\u{000c}'];
    let trim = |node: &mut PaintNode| {
        let ComponentKind::Text { content } = &mut node.kind else {
            return false;
        };
        let trimmed = content.trim_start_matches(COLLAPSIBLE);
        if trimmed.len() == content.len() {
            return false;
        }
        let removed = content.len() - trimmed.len();
        let ends = w3cos_std::inline_text::fragment_ends(content, &node.style);
        *content = trimmed.to_string();
        if let Some(ends) = ends {
            // Anonymous text may contain independently authored DOM or
            // pseudo-element runs. Rebase their byte boundaries and text
            // fingerprint when discarding a line-leading separator.
            let ends = ends
                .into_iter()
                .filter(|end| *end > removed)
                .map(|end| end - removed)
                .collect::<Vec<_>>();
            w3cos_std::inline_text::set_fragment_ends(&mut node.style, content, &ends);
        }
        true
    };
    let mut copied = 0;
    for index in BoxScan::new(nodes.len(), skip, dirty) {
        let node = &nodes[index];
        if node.style.display != Display::Inline
            || matches!(
                node.style.white_space,
                WhiteSpace::Pre | WhiteSpace::PreWrap
            )
        {
            continue;
        }
        if !matches!(&node.kind, ComponentKind::Text { content } if content.starts_with(COLLAPSIBLE))
        {
            continue;
        }
        if let Some((touched, before)) = aligned
            && let Some(mut parent) = node.parent
            && !touched[index]
        {
            let mut clean = !touched[parent];
            while clean && nodes[parent].style.display == Display::Inline {
                let Some(ancestor) = nodes[parent].parent else {
                    break;
                };
                parent = ancestor;
                clean = !touched[parent];
            }
            if clean {
                copied += 1;
                if before[index] && trim(&mut nodes[index]) {
                    if let Some(record) = record.as_deref_mut() {
                        record[index] = true;
                    }
                }
                continue;
            }
        }
        let (Some(mut parent), Some(rect)) =
            (node.parent, rect_by_index.get(index).copied().flatten())
        else {
            continue;
        };
        while nodes[parent].style.display == Display::Inline {
            let Some(ancestor) = nodes[parent].parent else {
                break;
            };
            parent = ancestor;
        }
        let Some(parent_rect) = rect_by_index.get(parent).copied().flatten() else {
            continue;
        };
        let Some(parent_style) = nodes.get(parent).map(|parent| &parent.style) else {
            continue;
        };
        let line_start = parent_rect.x
            + parent_style
                .border_left_width
                .unwrap_or(parent_style.border_width)
            + parent_style.padding_left_length();
        if (rect.x - line_start).abs() > 0.01 {
            continue;
        }
        if trim(&mut nodes[index])
            && let Some(record) = record.as_deref_mut()
        {
            record[index] = true;
        }
    }
    copied
}

const TABLE_BACKGROUND_FRAGMENTS: &str = "--w3cos-internal-table-background-fragments";

fn annotate_separated_table_background_fragments(
    nodes: &mut [PaintNode],
    rect_by_index: &[Option<LayoutRect>],
) {
    fn nearest_table(nodes: &[PaintNode], index: usize) -> Option<usize> {
        let mut parent = nodes[index].parent;
        while let Some(parent_index) = parent {
            if matches!(
                nodes[parent_index].style.display,
                Display::Table | Display::InlineTable
            ) {
                return Some(parent_index);
            }
            parent = nodes[parent_index].parent;
        }
        None
    }
    fn column_span(style: &Style) -> usize {
        style
            .custom_properties
            .as_ref()
            .and_then(|properties| properties.get("--w3cos-internal-table-column-span"))
            .and_then(|span| span.parse::<usize>().ok())
            .filter(|span| *span > 0)
            .unwrap_or(1)
            .min(1000)
    }
    fn descendant_of(nodes: &[PaintNode], mut index: usize, ancestor: usize) -> bool {
        while let Some(parent) = nodes[index].parent {
            if parent == ancestor {
                return true;
            }
            index = parent;
        }
        false
    }

    let original_len = nodes.len();
    for source in 0..original_len {
        if !matches!(
            nodes[source].style.display,
            Display::TableColumnGroup
                | Display::TableColumn
                | Display::TableRow
                | Display::TableRowGroup
                | Display::TableHeaderGroup
                | Display::TableFooterGroup
        ) || (nodes[source].style.background.a == 0
            && nodes[source].style.background_image.is_none())
        {
            continue;
        }
        let Some(table) = nearest_table(nodes, source) else {
            continue;
        };
        let table_has_cells = (0..original_len).any(|index| {
            nodes[index].style.display == Display::TableCell
                && nearest_table(nodes, index) == Some(table)
        });
        if !table_has_cells {
            nodes[source].style.background = Color::TRANSPARENT;
            nodes[source].style.background_image = None;
            continue;
        }
        if nodes[table].style.border_collapse {
            continue;
        }
        let columns = (0..original_len)
            .filter(|index| {
                nodes[*index].style.display == Display::TableColumn
                    && nearest_table(nodes, *index) == Some(table)
            })
            .collect::<Vec<_>>();
        let covered = columns
            .iter()
            .enumerate()
            .filter(|(_, column)| {
                source == **column
                    || (nodes[source].style.display == Display::TableColumnGroup
                        && descendant_of(nodes, **column, source))
            })
            .map(|(column, _)| column)
            .collect::<std::collections::HashSet<_>>();
        let row_source = matches!(
            nodes[source].style.display,
            Display::TableRow
                | Display::TableRowGroup
                | Display::TableHeaderGroup
                | Display::TableFooterGroup
        );
        if covered.is_empty() && !row_source {
            continue;
        }
        let rows = (0..original_len)
            .filter(|index| {
                nodes[*index].style.display == Display::TableRow
                    && nearest_table(nodes, *index) == Some(table)
            })
            .collect::<Vec<_>>();
        let mut fragments = Vec::new();
        for row in rows {
            if row_source && row != source && !descendant_of(nodes, row, source) {
                continue;
            }
            let cells = (0..original_len)
                .filter(|index| {
                    nodes[*index].parent == Some(row)
                        && nodes[*index].style.display == Display::TableCell
                })
                .collect::<Vec<_>>();
            let mut column = 0usize;
            for cell in cells {
                let span = column_span(&nodes[cell].style);
                if (row_source
                    || (column..column.saturating_add(span)).any(|index| covered.contains(&index)))
                    && let Some(rect) = rect_by_index.get(cell).copied().flatten()
                {
                    fragments.push(format!(
                        "{} {} {} {}",
                        rect.x, rect.y, rect.width, rect.height
                    ));
                }
                column += span;
            }
        }
        if !fragments.is_empty() {
            nodes[source]
                .style
                .custom_properties
                .get_or_insert_with(Default::default)
                .insert(TABLE_BACKGROUND_FRAGMENTS.to_string(), fragments.join(";"));
        }
    }

    // Row and row-group backgrounds paint over their continuous row boxes,
    // including the separated-border spacing gutters. Only column layers
    // need per-cell fragments because columns have no principal CSS box.
}

fn project_collapsed_table_tracks_to_cells(nodes: &mut [PaintNode]) {
    fn column_span(style: &Style) -> usize {
        style
            .custom_properties
            .as_ref()
            .and_then(|properties| properties.get("--w3cos-internal-table-column-span"))
            .and_then(|span| span.parse::<usize>().ok())
            .filter(|span| *span > 0)
            .unwrap_or(1)
            .min(1000)
    }
    fn nearest_table(nodes: &[PaintNode], index: usize) -> Option<usize> {
        let mut parent = nodes[index].parent;
        while let Some(parent_index) = parent {
            if matches!(
                nodes[parent_index].style.display,
                Display::Table | Display::InlineTable
            ) {
                return Some(parent_index);
            }
            parent = nodes[parent_index].parent;
        }
        None
    }
    // A table projects its columns only when one of them is collapsed; with
    // no collapsed column anywhere every table below would be skipped.
    let any_collapsed_column = nodes.iter().any(|node| {
        node.style.display == Display::TableColumn && node.style.visibility == Visibility::Collapse
    });
    let tables = nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| {
            any_collapsed_column
                && matches!(node.style.display, Display::Table | Display::InlineTable)
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    for table in tables {
        let collapsed_columns = nodes
            .iter()
            .enumerate()
            .filter(|(index, node)| {
                node.style.display == Display::TableColumn
                    && nearest_table(nodes, *index) == Some(table)
            })
            .map(|(_, node)| node.style.visibility == Visibility::Collapse)
            .collect::<Vec<_>>();
        if !collapsed_columns.iter().any(|collapsed| *collapsed) {
            continue;
        }
        let rows = nodes
            .iter()
            .enumerate()
            .filter(|(index, node)| {
                node.style.display == Display::TableRow
                    && nearest_table(nodes, *index) == Some(table)
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        for row in rows {
            let cells = nodes
                .iter()
                .enumerate()
                .filter(|(_, node)| {
                    node.parent == Some(row) && node.style.display == Display::TableCell
                })
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            let mut column: usize = 0;
            for cell in cells {
                let span = column_span(&nodes[cell].style);
                let covered_columns = collapsed_columns
                    .get(column..column.saturating_add(span))
                    .unwrap_or_default();
                let all_collapsed = !covered_columns.is_empty()
                    && covered_columns.iter().all(|collapsed| *collapsed);
                if all_collapsed {
                    nodes[cell].style.visibility = Visibility::Collapse;
                    nodes[cell].style.overflow_x = Some(Overflow::Hidden);
                    nodes[cell].style.overflow_y = Some(Overflow::Hidden);
                }
                column = column.saturating_add(span);
            }
        }
    }
    let collapsed_rows = nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| {
            node.style.display == Display::TableRow && node.style.visibility == Visibility::Collapse
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    for row in collapsed_rows {
        for node in nodes
            .iter_mut()
            .filter(|node| node.parent == Some(row) && node.style.display == Display::TableCell)
        {
            node.style.visibility = Visibility::Collapse;
        }
    }
}

/// Each table row's cell children, in paint-tree order.
fn table_cells_by_row(nodes: &[PaintNode]) -> std::collections::HashMap<usize, Vec<usize>> {
    let mut cells: std::collections::HashMap<usize, Vec<usize>> = std::collections::HashMap::new();
    for (index, node) in nodes.iter().enumerate() {
        if node.style.display == Display::TableCell
            && let Some(row) = node.parent
        {
            cells.entry(row).or_default().push(index);
        }
    }
    cells
}

fn resolve_collapsed_cell_border_conflicts(nodes: &mut [PaintNode]) {
    use w3cos_std::style::TextDirection;

    fn edge(style: &Style, side: usize) -> (f32, Color) {
        let width = match side {
            0 => style.border_top_width,
            1 => style.border_right_width,
            2 => style.border_bottom_width,
            _ => style.border_left_width,
        }
        .unwrap_or(style.border_width);
        let color = match side {
            0 => style.border_top_color,
            1 => style.border_right_color,
            2 => style.border_bottom_color,
            _ => style.border_left_color,
        }
        .unwrap_or(style.border_color);
        (width, color)
    }
    fn set_edge(style: &mut Style, side: usize, width: f32, color: Color) {
        match side {
            0 => {
                style.border_top_width = Some(width);
                style.border_top_color = Some(color);
            }
            1 => {
                style.border_right_width = Some(width);
                style.border_right_color = Some(color);
            }
            2 => {
                style.border_bottom_width = Some(width);
                style.border_bottom_color = Some(color);
            }
            _ => {
                style.border_left_width = Some(width);
                style.border_left_color = Some(color);
            }
        }
    }
    fn hidden(style: &Style, side: usize) -> bool {
        style.border_styles[side] == Some(w3cos_std::style::BorderLineStyle::Hidden)
    }
    fn adopt_edge(nodes: &mut [PaintNode], owner: usize, cell: usize, side: usize) {
        let (width, color) = edge(&nodes[owner].style, side);
        let line_style = nodes[owner].style.border_styles[side];
        let current_color = nodes[owner]
            .style
            .border_current_color
            .is_some_and(|mask| mask[side]);
        set_edge(&mut nodes[cell].style, side, width, color);
        nodes[cell].style.border_styles[side] = line_style;
        if current_color || nodes[cell].style.border_current_color.is_some() {
            nodes[cell]
                .style
                .border_current_color
                .get_or_insert([false; 4])[side] = current_color;
        }
    }
    fn line_priority(style: &Style, side: usize) -> u8 {
        use w3cos_std::style::BorderLineStyle;
        // Compare authored styles before color/owner precedence. Outset's
        // groove-like painting does not change its conflict priority.
        match style.border_styles[side].unwrap_or(BorderLineStyle::Solid) {
            BorderLineStyle::Hidden => 10,
            BorderLineStyle::Double => 9,
            BorderLineStyle::Solid => 8,
            BorderLineStyle::Dashed => 7,
            BorderLineStyle::Dotted => 6,
            BorderLineStyle::Ridge => 5,
            BorderLineStyle::Outset => 4,
            BorderLineStyle::Groove => 3,
            BorderLineStyle::Inset => 2,
            BorderLineStyle::None => 0,
        }
    }
    fn hide_edge(style: &mut Style, side: usize) {
        style.border_styles[side] = Some(w3cos_std::style::BorderLineStyle::Hidden);
        set_edge(style, side, 0.0, Color::TRANSPARENT);
    }
    fn suppress_edge(style: &mut Style, side: usize, width: f32) {
        const NAMES: [&str; 4] = ["top", "right", "bottom", "left"];
        set_edge(style, side, width, Color::TRANSPARENT);
        style
            .custom_properties
            .get_or_insert_with(Default::default)
            .entry(COLLAPSED_BORDER_SUPPRESSED.to_string())
            .and_modify(|value| {
                value.push(' ');
                value.push_str(NAMES[side]);
            })
            .or_insert_with(|| NAMES[side].to_string());
    }

    // Every pass below starts from a box in the collapsing border model.
    if !nodes.iter().any(|node| node.style.border_collapse) {
        return;
    }
    let rows = nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| node.style.display == Display::TableRow && node.style.border_collapse)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    // Cell membership reads only parent and display, which neither pass
    // changes; index it once rather than scanning every node per row.
    let cells_by_row = table_cells_by_row(nodes);
    let row_cells = |row: usize| cells_by_row.get(&row).cloned().unwrap_or_default();

    fn nearest_table(nodes: &[PaintNode], index: usize) -> Option<usize> {
        let mut parent = nodes[index].parent;
        while let Some(parent_index) = parent {
            if matches!(
                nodes[parent_index].style.display,
                Display::Table | Display::InlineTable
            ) {
                return Some(parent_index);
            }
            parent = nodes[parent_index].parent;
        }
        None
    }
    let mut parts = nodes
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, node)| {
            node.style.border_collapse
                && matches!(
                    node.style.display,
                    Display::TableRow
                        | Display::TableRowGroup
                        | Display::TableHeaderGroup
                        | Display::TableFooterGroup
                        | Display::TableColumn
                        | Display::TableColumnGroup
                )
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    // Equal-width/color conflicts retain the higher-priority owner. Cells
    // already own their authored edges; resolve parts in CSS precedence
    // order, independently of their order in the flattened paint tree.
    parts.sort_by_key(|index| match nodes[*index].style.display {
        Display::TableRow => 0,
        Display::TableRowGroup | Display::TableHeaderGroup | Display::TableFooterGroup => 1,
        Display::TableColumn => 2,
        _ => 3,
    });
    for part in parts {
        let column_part = matches!(
            nodes[part].style.display,
            Display::TableColumn | Display::TableColumnGroup
        );
        let part_rows = if nodes[part].style.display == Display::TableRow {
            vec![part]
        } else {
            rows.iter()
                .copied()
                .filter(|row| {
                    if nearest_table(nodes, *row) != nearest_table(nodes, part) {
                        return false;
                    }
                    if column_part {
                        return true;
                    }
                    let mut parent = nodes[*row].parent;
                    while let Some(index) = parent {
                        if index == part {
                            return true;
                        }
                        parent = nodes[index].parent;
                    }
                    false
                })
                .collect()
        };
        let populated_rows = part_rows
            .iter()
            .copied()
            .filter(|row| !row_cells(*row).is_empty())
            .collect::<Vec<_>>();
        let Some(first) = populated_rows.first().copied() else {
            continue;
        };
        let last = populated_rows.last().copied().unwrap_or(first);
        let mut boundary_cells = [row_cells(first), Vec::new(), row_cells(last), Vec::new()];
        for row in populated_rows {
            let cells = row_cells(row);
            if let Some(first) = cells.first() {
                boundary_cells[3].push(*first);
            }
            if let Some(last) = cells.last() {
                boundary_cells[1].push(*last);
            }
        }
        if column_part {
            fn descendant(nodes: &[PaintNode], mut index: usize, ancestor: usize) -> bool {
                while let Some(parent) = nodes[index].parent {
                    if parent == ancestor {
                        return true;
                    }
                    index = parent;
                }
                false
            }
            fn span(style: &Style) -> usize {
                style
                    .custom_properties
                    .as_ref()
                    .and_then(|properties| properties.get("--w3cos-internal-table-column-span"))
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(1)
                    .clamp(1, 1000)
            }
            let mut columns = Vec::new();
            for (index, node) in nodes.iter().enumerate() {
                if nearest_table(nodes, index) != nearest_table(nodes, part) {
                    continue;
                }
                let implicit_group = node.style.display == Display::TableColumnGroup
                    && !nodes.iter().enumerate().any(|(child, node)| {
                        node.style.display == Display::TableColumn
                            && descendant(nodes, child, index)
                    });
                if node.style.display == Display::TableColumn || implicit_group {
                    columns.extend(std::iter::repeat_n(index, span(&node.style)));
                }
            }
            let covered = columns
                .iter()
                .enumerate()
                .filter(|(_, column)| **column == part || descendant(nodes, **column, part))
                .map(|(column, _)| column)
                .collect::<Vec<_>>();
            let Some(start) = covered.first().copied() else {
                continue;
            };
            let end = covered.last().copied().unwrap_or(start) + 1;
            boundary_cells = std::array::from_fn(|_| Vec::new());
            for row in &part_rows {
                let mut column = 0usize;
                for cell in row_cells(*row) {
                    let next = column + span(&nodes[cell].style);
                    if column < end && next > start {
                        if *row == first {
                            boundary_cells[0].push(cell);
                        }
                        if *row == last {
                            boundary_cells[2].push(cell);
                        }
                    }
                    if column == start {
                        boundary_cells[3].push(cell);
                    }
                    if next == end {
                        boundary_cells[1].push(cell);
                    }
                    column = next;
                }
            }
        }
        for side in 0..4 {
            let part_edge = edge(&nodes[part].style, side);
            for cell in &boundary_cells[side] {
                if hidden(&nodes[part].style, side) || hidden(&nodes[*cell].style, side) {
                    hide_edge(&mut nodes[*cell].style, side);
                } else if part_edge.0 > edge(&nodes[*cell].style, side).0
                    || (part_edge.0 == edge(&nodes[*cell].style, side).0
                        && line_priority(&nodes[part].style, side)
                            > line_priority(&nodes[*cell].style, side))
                {
                    adopt_edge(nodes, part, *cell, side);
                }
            }
            if !boundary_cells[side].is_empty() {
                // Cells now own the winning edge. Keep the part's background
                // on its grid box without painting a second border ring.
                set_edge(&mut nodes[part].style, side, 0.0, Color::TRANSPARENT);
            }
        }
    }

    let grid_nodes = nodes
        .iter()
        .map(|node| (node.style.as_style(), node.parent))
        .collect::<Vec<_>>();
    let spanning_grids = crate::table_grid::calculate(&grid_nodes)
        .into_iter()
        .filter(|grid| {
            nodes[grid.table].style.border_collapse
                && grid.cells.iter().any(|cell| cell.row_span > 1)
        })
        .collect::<Vec<_>>();
    let mut occupied_rows = std::collections::HashMap::new();
    for grid in &spanning_grids {
        for (row_number, row) in grid.rows.iter().enumerate() {
            let mut cells = grid
                .cells
                .iter()
                .filter(|cell| cell.row <= row_number && row_number < cell.row + cell.row_span)
                .copied()
                .collect::<Vec<_>>();
            cells.sort_by_key(|cell| cell.column);
            occupied_rows.insert(*row, cells);
        }
    }
    #[derive(Default)]
    struct HorizontalVotes {
        covered: usize,
        span: usize,
        won: usize,
        hidden: usize,
        width: f32,
    }
    let mut horizontal_votes = std::collections::HashMap::<(usize, usize), HorizontalVotes>::new();
    for row in &rows {
        let pairs = if let Some(cells) = occupied_rows.get(row) {
            cells
                .windows(2)
                .filter(|pair| pair[0].column + pair[0].column_span == pair[1].column)
                .map(|pair| {
                    (
                        pair[0].index,
                        pair[1].index,
                        pair[0].row_span,
                        pair[1].row_span,
                    )
                })
                .collect::<Vec<_>>()
        } else {
            row_cells(*row)
                .windows(2)
                .map(|pair| (pair[0], pair[1], 1, 1))
                .collect()
        };
        for (left, right, left_span, right_span) in pairs {
            let left_edge = edge(&nodes[left].style, 1);
            let right_edge = edge(&nodes[right].style, 3);
            let hidden_conflict = hidden(&nodes[left].style, 1) || hidden(&nodes[right].style, 3);
            let left_collapsed = nodes[left].style.visibility == Visibility::Collapse;
            let right_collapsed = nodes[right].style.visibility == Visibility::Collapse;
            let left_wins = if left_collapsed != right_collapsed {
                !left_collapsed
            } else if left_edge.0 > right_edge.0 {
                true
            } else if right_edge.0 > left_edge.0 {
                false
            } else if line_priority(&nodes[left].style, 1) != line_priority(&nodes[right].style, 3)
            {
                line_priority(&nodes[left].style, 1) > line_priority(&nodes[right].style, 3)
            } else {
                nodes[*row].style.direction != TextDirection::Rtl
            };
            for (cell, side, span, won, other_width) in [
                (left, 1, left_span, left_wins, right_edge.0),
                (right, 3, right_span, !left_wins, left_edge.0),
            ] {
                let vote = horizontal_votes.entry((cell, side)).or_default();
                vote.span = span;
                vote.covered += 1;
                vote.won += usize::from(won);
                vote.hidden += usize::from(hidden_conflict);
                vote.width = vote.width.max(other_width);
            }
        }
    }
    for ((cell, side), vote) in horizontal_votes {
        // A neighbor on one row cannot suppress an entire rowspan edge.
        // Mixed ownership needs per-segment painting, just as for colspan.
        if vote.covered != vote.span {
            continue;
        }
        if vote.hidden == vote.covered {
            hide_edge(&mut nodes[cell].style, side);
        } else if vote.hidden == 0 && vote.won == 0 {
            suppress_edge(&mut nodes[cell].style, side, vote.width);
        } else if vote.hidden == 0 && vote.won == vote.covered {
            let winner = edge(&nodes[cell].style, side);
            set_edge(&mut nodes[cell].style, side, winner.0, winner.1);
        }
    }

    let tables = nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| {
            matches!(node.style.display, Display::Table | Display::InlineTable)
                && node.style.border_collapse
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    for table in tables {
        let table_rows = rows
            .iter()
            .copied()
            .filter(|row| nearest_table(nodes, *row) == Some(table))
            .collect::<Vec<_>>();
        let visible_rows = table_rows
            .iter()
            .copied()
            .filter(|row| nodes[*row].style.visibility != Visibility::Collapse)
            .collect::<Vec<_>>();
        let boundary_rows = if visible_rows.is_empty() {
            &table_rows
        } else {
            &visible_rows
        };
        let Some(first_row) = boundary_rows.first().copied() else {
            continue;
        };
        let last_row = boundary_rows.last().copied().unwrap_or(first_row);
        let mut boundary_cells = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        boundary_cells[0] = row_cells(first_row)
            .into_iter()
            .filter(|cell| nodes[*cell].style.visibility != Visibility::Collapse)
            .collect();
        boundary_cells[2] = row_cells(last_row)
            .into_iter()
            .filter(|cell| nodes[*cell].style.visibility != Visibility::Collapse)
            .collect();
        for row in boundary_rows {
            let cells = row_cells(*row)
                .into_iter()
                .filter(|cell| nodes[*cell].style.visibility != Visibility::Collapse)
                .collect::<Vec<_>>();
            if let Some(first) = cells.first() {
                boundary_cells[3].push(*first);
            }
            if let Some(last) = cells.last() {
                boundary_cells[1].push(*last);
            }
        }
        for side in 0..4 {
            let table_edge = edge(&nodes[table].style, side);
            for cell in &boundary_cells[side] {
                if hidden(&nodes[table].style, side) || hidden(&nodes[*cell].style, side) {
                    hide_edge(&mut nodes[*cell].style, side);
                    continue;
                }
                let cell_edge = edge(&nodes[*cell].style, side);
                if table_edge.0 > cell_edge.0
                    || (table_edge.0 == cell_edge.0
                        && line_priority(&nodes[table].style, side)
                            > line_priority(&nodes[*cell].style, side))
                {
                    adopt_edge(nodes, table, *cell, side);
                } else {
                    set_edge(&mut nodes[*cell].style, side, cell_edge.0, cell_edge.1);
                }
            }
            // Boundary cells paint the resolved collapsed edge on the grid.
            // Leaving the table wrapper border active would inset and paint a
            // second ring around that same edge.
            // Keep the resolved table padding-box geometry while cells own
            // its ink. CSS2 takes inline outer edges from the first row;
            // block outer edges use the maximum across the boundary row.
            let used_width = if matches!(side, 1 | 3) {
                boundary_cells[side]
                    .first()
                    .map(|cell| edge(&nodes[*cell].style, side).0)
            } else {
                boundary_cells[side]
                    .iter()
                    .map(|cell| edge(&nodes[*cell].style, side).0)
                    .reduce(f32::max)
            }
            .unwrap_or(table_edge.0);
            suppress_edge(&mut nodes[table].style, side, used_width);
        }
    }
    for pair in rows.windows(2) {
        if nearest_table(nodes, pair[0]) != nearest_table(nodes, pair[1]) {
            continue;
        }
        let ranges = |row| {
            let mut column = 0usize;
            row_cells(row)
                .into_iter()
                .map(|cell| {
                    let span = nodes[cell]
                        .style
                        .custom_properties
                        .as_ref()
                        .and_then(|properties| properties.get("--w3cos-internal-table-column-span"))
                        .and_then(|span| span.parse::<usize>().ok())
                        .unwrap_or(1)
                        .clamp(1, 1000);
                    let start = column;
                    column += span;
                    (cell, start, column)
                })
                .collect::<Vec<_>>()
        };
        let top_cells = ranges(pair[0]);
        let bottom_cells = ranges(pair[1]);
        #[derive(Default)]
        struct Votes {
            covered: usize,
            span: usize,
            won: usize,
            hidden: usize,
            width: f32,
        }
        let mut votes = std::collections::HashMap::<(usize, usize), Votes>::new();
        let (mut top_index, mut bottom_index) = (0, 0);
        while top_index < top_cells.len() && bottom_index < bottom_cells.len() {
            let (top, top_start, top_end) = top_cells[top_index];
            let (bottom, bottom_start, bottom_end) = bottom_cells[bottom_index];
            let overlap = top_end
                .min(bottom_end)
                .saturating_sub(top_start.max(bottom_start));
            let top_edge = edge(&nodes[top].style, 2);
            let bottom_edge = edge(&nodes[bottom].style, 0);
            let hidden_conflict = hidden(&nodes[top].style, 2) || hidden(&nodes[bottom].style, 0);
            let top_collapsed = nodes[top].style.visibility == Visibility::Collapse;
            let bottom_collapsed = nodes[bottom].style.visibility == Visibility::Collapse;
            let bottom_wins = (top_collapsed && !bottom_collapsed)
                || bottom_edge.0 > top_edge.0
                || (bottom_edge.0 == top_edge.0
                    && line_priority(&nodes[bottom].style, 0)
                        > line_priority(&nodes[top].style, 2));
            for (cell, side, span, won, other_width) in [
                (top, 2, top_end - top_start, !bottom_wins, bottom_edge.0),
                (
                    bottom,
                    0,
                    bottom_end - bottom_start,
                    bottom_wins,
                    top_edge.0,
                ),
            ] {
                let vote = votes.entry((cell, side)).or_default();
                vote.span = span;
                vote.covered += overlap;
                vote.won += if won { overlap } else { 0 };
                vote.hidden += if hidden_conflict { overlap } else { 0 };
                vote.width = vote.width.max(other_width);
            }
            if top_end <= bottom_end {
                top_index += 1;
            }
            if bottom_end <= top_end {
                bottom_index += 1;
            }
        }
        for ((cell, side), vote) in votes {
            // Never suppress an entire spanning edge on the strength of a
            // single track. Mixed winners require separate ink segments.
            if vote.covered != vote.span {
                continue;
            }
            if vote.hidden == vote.covered {
                hide_edge(&mut nodes[cell].style, side);
            } else if vote.hidden == 0 && vote.won == 0 {
                suppress_edge(&mut nodes[cell].style, side, vote.width);
            }
        }
    }
}

const COLLAPSED_BORDER_SUPPRESSED: &str = "--w3cos-internal-collapsed-border-suppressed";
const COLLAPSED_BORDER_BOTTOM_EXTENSION: &str =
    "--w3cos-internal-collapsed-border-bottom-extension";

const COLLAPSED_BORDER_JOINTS: &str = "--w3cos-internal-collapsed-border-joints";

fn annotate_collapsed_border_joints(nodes: &mut [PaintNode], rects: &[Option<LayoutRect>]) {
    use std::collections::HashMap;
    use w3cos_std::style::BorderLineStyle;
    struct Edge {
        cell: usize,
        side: usize,
        width: f32,
        priority: u8,
    }
    let mut edges = Vec::<Edge>::new();
    let mut joints = HashMap::<(usize, i64, i64), Vec<(usize, usize)>>::new();
    let mut lines = HashMap::<(usize, usize, i64), Vec<(i64, i64, usize)>>::new();
    let mut offsets = HashMap::<usize, [[f32; 2]; 4]>::new();
    for (cell, node) in nodes.iter().enumerate() {
        if node.style.display != Display::TableCell
            || !node.style.border_collapse
            || node.style.visibility == Visibility::Collapse
        {
            continue;
        }
        let Some(rect) = rects.get(cell).and_then(|rect| *rect) else {
            continue;
        };
        let mut parent = node.parent;
        let mut table = None;
        while let Some(index) = parent {
            if matches!(
                nodes[index].style.display,
                Display::Table | Display::InlineTable
            ) {
                table = Some(index);
                break;
            }
            parent = nodes[index].parent;
        }
        let Some(table) = table else { continue };
        let widths = [
            node.style.border_top_width,
            node.style.border_right_width,
            node.style.border_bottom_width,
            node.style.border_left_width,
        ]
        .map(|width| width.unwrap_or(node.style.border_width));
        let endpoints = [
            [(rect.x, rect.y), (rect.x + rect.width, rect.y)],
            [
                (rect.x + rect.width, rect.y),
                (rect.x + rect.width, rect.y + rect.height),
            ],
            [
                (rect.x, rect.y + rect.height),
                (rect.x + rect.width, rect.y + rect.height),
            ],
            [(rect.x, rect.y), (rect.x, rect.y + rect.height)],
        ];
        for side in 0..4 {
            if widths[side] <= 0.0
                || collapsed_border_suppressed(
                    &node.style,
                    ["top", "right", "bottom", "left"][side],
                )
            {
                continue;
            }
            let priority = match node.style.border_styles[side].unwrap_or(BorderLineStyle::Solid) {
                BorderLineStyle::None | BorderLineStyle::Hidden => continue,
                BorderLineStyle::Double => 9,
                BorderLineStyle::Solid => 8,
                BorderLineStyle::Dashed => 7,
                BorderLineStyle::Dotted => 6,
                BorderLineStyle::Ridge => 5,
                BorderLineStyle::Outset => 4,
                BorderLineStyle::Groove => 3,
                BorderLineStyle::Inset => 2,
            };
            let edge_index = edges.len();
            edges.push(Edge {
                cell,
                side,
                width: widths[side],
                priority,
            });
            offsets.entry(cell).or_insert([[0.0; 2]; 4]);
            let points = endpoints[side]
                .map(|(x, y)| ((x * 64.0).round() as i64, (y * 64.0).round() as i64));
            let (line, start, end) = if side % 2 == 0 {
                (points[0].1, points[0].0, points[1].0)
            } else {
                (points[0].0, points[0].1, points[1].1)
            };
            lines
                .entry((table, side % 2, line))
                .or_default()
                .push((start, end, edge_index));
            for (endpoint, (x, y)) in endpoints[side].into_iter().enumerate() {
                // Layout coordinates are CSS layout units, not snapped paint
                // pixels. Keep nearby but distinct joints separate.
                joints
                    .entry((table, (x * 64.0).round() as i64, (y * 64.0).round() as i64))
                    .or_default()
                    .push((edge_index, endpoint));
            }
        }
    }
    // Include edges passing through T-joints, not just edges ending there.
    // Row/column indexes and prefix maximum endpoints keep ordinary joints
    // local rather than scanning every edge in the table.
    let indexed_lines: HashMap<_, _> = lines
        .into_iter()
        .map(|(key, mut segments)| {
            segments.sort_by_key(|segment| segment.0);
            let mut maximum = i64::MIN;
            let maxima = segments
                .iter()
                .map(|segment| {
                    maximum = maximum.max(segment.1);
                    maximum
                })
                .collect::<Vec<_>>();
            (key, (segments, maxima))
        })
        .collect();
    for (&(table, x, y), incident) in &mut joints {
        for (axis, line, coordinate) in [(0, y, x), (1, x, y)] {
            let Some((segments, maxima)) = indexed_lines.get(&(table, axis, line)) else {
                continue;
            };
            let mut index = segments.partition_point(|segment| segment.0 < coordinate);
            while index > 0 && maxima[index - 1] > coordinate {
                index -= 1;
                let (_, end, edge) = segments[index];
                if end > coordinate {
                    incident.push((edge, usize::MAX));
                }
            }
        }
    }
    let compare = |a: &Edge, b: &Edge| {
        a.width
            .total_cmp(&b.width)
            .then(a.priority.cmp(&b.priority))
            .then(b.cell.cmp(&a.cell))
    };
    for incident in joints.values() {
        let Some((winner, _)) = incident
            .iter()
            .max_by(|(a, _), (b, _)| compare(&edges[*a], &edges[*b]))
        else {
            continue;
        };
        for &(edge_index, endpoint) in incident {
            if endpoint == usize::MAX {
                continue;
            }
            let edge = &edges[edge_index];
            let perpendicular_width = incident
                .iter()
                .filter(|(other, _)| edges[*other].side % 2 != edge.side % 2)
                .map(|(other, _)| edges[*other].width)
                .fold(0.0, f32::max);
            let wins = !compare(edge, &edges[*winner]).is_lt();
            let signed_half = if wins {
                perpendicular_width / 2.0
            } else {
                -perpendicular_width / 2.0
            };
            offsets.get_mut(&edge.cell).unwrap()[edge.side][endpoint] = if endpoint == 0 {
                -signed_half
            } else {
                signed_half
            };
        }
    }
    for (cell, offsets) in offsets {
        nodes[cell]
            .style
            .custom_properties
            .get_or_insert_with(Default::default)
            .insert(
                COLLAPSED_BORDER_JOINTS.into(),
                offsets
                    .into_iter()
                    .flatten()
                    .map(|value| value.to_string())
                    .collect::<Vec<_>>()
                    .join(" "),
            );
    }
}

fn extend_collapsed_borders_across_empty_rows(
    nodes: &mut [PaintNode],
    rect_by_index: &[Option<LayoutRect>],
) {
    let rows = nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| node.style.display == Display::TableRow && node.style.border_collapse)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let cells_by_row = table_cells_by_row(nodes);
    let row_cells = |row: usize| cells_by_row.get(&row).cloned().unwrap_or_default();
    for rows in rows.windows(3) {
        let top_cells = row_cells(rows[0]);
        let empty_cells = row_cells(rows[1]);
        let bottom_cells = row_cells(rows[2]);
        if top_cells.is_empty() || !empty_cells.is_empty() || bottom_cells.is_empty() {
            continue;
        }
        let extension = rect_by_index
            .get(rows[1])
            .and_then(|rect| *rect)
            .map_or(0.0, |rect| rect.height.max(0.0));
        if extension == 0.0 {
            continue;
        }
        for cell in top_cells {
            let bottom_width = nodes[cell]
                .style
                .border_bottom_width
                .unwrap_or(nodes[cell].style.border_width);
            let extension = extension.min(bottom_width);
            if extension > 0.0 {
                nodes[cell]
                    .style
                    .custom_properties
                    .get_or_insert_with(Default::default)
                    .insert(
                        COLLAPSED_BORDER_BOTTOM_EXTENSION.to_string(),
                        extension.to_string(),
                    );
            }
        }
    }
}

pub(crate) fn border_edge_paint_rects(
    style: &Style,
    rect: LayoutRect,
    widths: [f32; 4],
) -> [LayoutRect; 4] {
    let grid = rect;
    let rect = if style.border_collapse && style.display == Display::TableCell {
        LayoutRect {
            y: rect.y - widths[0] / 2.0,
            height: rect.height + widths[0] / 2.0 + widths[2] / 2.0,
            ..rect
        }
    } else {
        rect
    };
    if style.border_collapse
        && matches!(
            style.display,
            Display::TableColumn | Display::TableColumnGroup
        )
    {
        // Column boxes describe grid tracks, not inset border boxes. A
        // collapsed border is centered on the corresponding grid line.
        // Block edges include the adjacent inline half-borders so the
        // outer corner quadrants are covered, not four disconnected strips.
        return [
            LayoutRect {
                x: rect.x - widths[3] / 2.0,
                y: rect.y - widths[0] / 2.0,
                width: rect.width + (widths[3] + widths[1]) / 2.0,
                height: widths[0],
            },
            LayoutRect {
                x: rect.x + rect.width - widths[1] / 2.0,
                y: rect.y,
                width: widths[1],
                height: rect.height,
            },
            LayoutRect {
                x: rect.x - widths[3] / 2.0,
                y: rect.y + rect.height - widths[2] / 2.0,
                width: rect.width + (widths[3] + widths[1]) / 2.0,
                height: widths[2],
            },
            LayoutRect {
                x: rect.x - widths[3] / 2.0,
                y: rect.y,
                width: widths[3],
                height: rect.height,
            },
        ];
    }
    let cell_inline_half = if style.border_collapse && style.display == Display::TableCell {
        0.5
    } else {
        0.0
    };
    let suppressed = |name: &str| collapsed_border_suppressed(style, name);
    let top = if suppressed("top") { widths[0] } else { 0.0 };
    let right = if suppressed("right") {
        widths[1] * (1.0 - cell_inline_half)
    } else {
        0.0
    };
    let bottom = if suppressed("bottom") { widths[2] } else { 0.0 };
    let left = if suppressed("left") {
        widths[3] * (1.0 - cell_inline_half)
    } else {
        0.0
    };
    let bottom_extension = style
        .custom_properties
        .as_ref()
        .and_then(|properties| properties.get(COLLAPSED_BORDER_BOTTOM_EXTENSION))
        .and_then(|value| value.parse::<f32>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(0.0);
    if style.border_collapse && style.display == Display::TableCell {
        let joints = style
            .custom_properties
            .as_ref()
            .and_then(|properties| properties.get(COLLAPSED_BORDER_JOINTS))
            .and_then(|value| {
                value
                    .split_ascii_whitespace()
                    .map(str::parse::<f32>)
                    .collect::<Result<Vec<_>, _>>()
                    .ok()
            })
            .filter(|values| values.len() == 8 && values.iter().all(|value| value.is_finite()));
        if let Some(joints) = joints {
            return [
                LayoutRect {
                    x: grid.x + joints[0],
                    y: grid.y - widths[0] / 2.0,
                    width: (grid.width + joints[1] - joints[0]).max(0.0),
                    height: widths[0],
                },
                LayoutRect {
                    x: grid.x + grid.width - widths[1] / 2.0,
                    y: grid.y + joints[2],
                    width: widths[1],
                    height: (grid.height + joints[3] - joints[2]).max(0.0),
                },
                LayoutRect {
                    x: grid.x + joints[4],
                    y: grid.y + grid.height - widths[2] / 2.0,
                    width: (grid.width + joints[5] - joints[4]).max(0.0),
                    height: widths[2] + bottom_extension,
                },
                LayoutRect {
                    x: grid.x - widths[3] / 2.0,
                    y: grid.y + joints[6],
                    width: widths[3],
                    height: (grid.height + joints[7] - joints[6]).max(0.0),
                },
            ];
        }
    }
    let horizontal_insets = |width: f32| {
        // A wider collapsed inline edge owns the junction, including its
        // inner half. Horizontal ink must not paint over that winning edge.
        let wider_left = if cell_inline_half > 0.0 && widths[3] > width {
            widths[3] * cell_inline_half
        } else {
            0.0
        };
        let wider_right = if cell_inline_half > 0.0 && widths[1] > width {
            widths[1] * cell_inline_half
        } else {
            0.0
        };
        (left.max(wider_left), right.max(wider_right))
    };
    let (top_left, top_right) = horizontal_insets(widths[0]);
    let (bottom_left, bottom_right) = horizontal_insets(widths[2]);
    [
        LayoutRect {
            x: rect.x + top_left,
            y: rect.y,
            width: (rect.width - top_left - top_right).max(0.0),
            height: widths[0],
        },
        LayoutRect {
            x: rect.x + rect.width - widths[1] * (1.0 - cell_inline_half),
            y: rect.y + top,
            width: widths[1],
            height: (rect.height - top - bottom).max(0.0),
        },
        LayoutRect {
            x: rect.x + bottom_left,
            y: rect.y + rect.height - widths[2],
            width: (rect.width - bottom_left - bottom_right).max(0.0),
            height: widths[2] + bottom_extension,
        },
        LayoutRect {
            x: rect.x - widths[3] * cell_inline_half,
            y: rect.y + top,
            width: widths[3],
            height: (rect.height - top - bottom).max(0.0),
        },
    ]
}

pub(crate) fn paint_inline_border_widths(style: &Style) -> (f32, f32) {
    let scale = if style.border_collapse && style.display == Display::TableCell {
        0.5
    } else {
        1.0
    };
    (
        style.border_left_width.unwrap_or(style.border_width) * scale,
        style.border_right_width.unwrap_or(style.border_width) * scale,
    )
}

fn collapsed_border_suppressed(style: &Style, name: &str) -> bool {
    style
        .custom_properties
        .as_ref()
        .and_then(|properties| properties.get(COLLAPSED_BORDER_SUPPRESSED))
        .is_some_and(|value| value.split_ascii_whitespace().any(|side| side == name))
}

/// `overflow` applies to block containers (CSS 2.1 11.1.1). An inline box and
/// the internal table boxes that are not block containers therefore never clip
/// their overflowing content, however `overflow: hidden` is authored.
fn establishes_overflow_clip(display: Display) -> bool {
    display.establishes_overflow_clip()
}

/// The overflow clipping region is the element's padding box (CSS 2.1 11.1.1),
/// so the border widths come off the border box. A collapsed-border box owns
/// only half of each shared edge, the same convention as
/// `paint_inline_border_widths` and `box_background_positioning_rect`.
pub(crate) fn overflow_clip_rect(style: &Style, rect: LayoutRect) -> LayoutRect {
    let scale = if style.border_collapse
        && matches!(
            style.display,
            Display::TableCell | Display::Table | Display::InlineTable
        ) {
        0.5
    } else {
        1.0
    };
    let top = style.border_top_width.unwrap_or(style.border_width) * scale;
    let right = style.border_right_width.unwrap_or(style.border_width) * scale;
    let bottom = style.border_bottom_width.unwrap_or(style.border_width) * scale;
    let left = style.border_left_width.unwrap_or(style.border_width) * scale;
    LayoutRect {
        x: rect.x + left,
        y: rect.y + top,
        width: (rect.width - left - right).max(0.0),
        height: (rect.height - top - bottom).max(0.0),
    }
}

pub(crate) fn box_background_paint_rect(style: &Style, rect: LayoutRect) -> LayoutRect {
    if style.border_collapse && style.display == Display::TableCell {
        // Cell rects end on shared grid-line centers on both axes.
        return rect;
    }
    if style.border_collapse && matches!(style.display, Display::Table | Display::InlineTable) {
        return rect;
    }
    let top = collapsed_border_suppressed(style, "top")
        .then(|| style.border_top_width.unwrap_or(style.border_width))
        .unwrap_or(0.0);
    let right = collapsed_border_suppressed(style, "right")
        .then(|| style.border_right_width.unwrap_or(style.border_width))
        .unwrap_or(0.0);
    let bottom = collapsed_border_suppressed(style, "bottom")
        .then(|| style.border_bottom_width.unwrap_or(style.border_width))
        .unwrap_or(0.0);
    let left = collapsed_border_suppressed(style, "left")
        .then(|| style.border_left_width.unwrap_or(style.border_width))
        .unwrap_or(0.0);
    LayoutRect {
        x: rect.x + left,
        y: rect.y + top,
        width: (rect.width - left - right).max(0.0),
        height: (rect.height - top - bottom).max(0.0),
    }
}

pub(crate) fn box_background_paint_rects(style: &Style, rect: LayoutRect) -> Vec<LayoutRect> {
    let Some(fragments) = style
        .custom_properties
        .as_ref()
        .and_then(|properties| properties.get(TABLE_BACKGROUND_FRAGMENTS))
    else {
        return vec![box_background_paint_rect(style, rect)];
    };
    let rects = fragments
        .split(';')
        .filter_map(|fragment| {
            let mut values = fragment
                .split_ascii_whitespace()
                .filter_map(|value| value.parse::<f32>().ok());
            Some(LayoutRect {
                x: values.next()?,
                y: values.next()?,
                width: values.next()?.max(0.0),
                height: values.next()?.max(0.0),
            })
        })
        .collect::<Vec<_>>();
    if rects.is_empty() {
        vec![box_background_paint_rect(style, rect)]
    } else {
        rects
    }
}

pub(crate) fn box_background_positioning_rect(
    style: &Style,
    rect: LayoutRect,
) -> Option<LayoutRect> {
    if !style.border_collapse || !matches!(style.display, Display::Table | Display::InlineTable) {
        return None;
    }
    let top = style.border_top_width.unwrap_or(style.border_width) / 2.0;
    let right = style.border_right_width.unwrap_or(style.border_width) / 2.0;
    let bottom = style.border_bottom_width.unwrap_or(style.border_width) / 2.0;
    let left = style.border_left_width.unwrap_or(style.border_width) / 2.0;
    Some(LayoutRect {
        x: rect.x + left,
        y: rect.y + top,
        width: (rect.width - left - right).max(0.0),
        height: (rect.height - top - bottom).max(0.0),
    })
}

const TABLE_CAPTION_INSETS: &str = "--w3cos-internal-table-caption-insets";

pub(crate) fn table_grid_paint_rect(style: &Style, mut rect: LayoutRect) -> LayoutRect {
    let Some(value) = style
        .custom_properties
        .as_ref()
        .and_then(|properties| properties.get(TABLE_CAPTION_INSETS))
    else {
        return rect;
    };
    let mut parts = value.split_ascii_whitespace();
    let Some(top) = parts.next().and_then(|value| value.parse::<f32>().ok()) else {
        return rect;
    };
    let Some(bottom) = parts.next().and_then(|value| value.parse::<f32>().ok()) else {
        return rect;
    };
    let scale_y = style.transform.scale_y;
    let top = top * scale_y;
    let bottom = bottom * scale_y;
    rect.y += top;
    rect.height = (rect.height - top - bottom).max(0.0);
    if let Some(right) = parts.next().and_then(|value| value.parse::<f32>().ok()) {
        rect.width = (rect.width - right * style.transform.scale_x).max(0.0);
    }
    rect
}

fn annotate_table_caption_paint_insets(
    nodes: &mut [PaintNode],
    rect_by_index: &[Option<LayoutRect>],
) {
    // Without a caption every inset is zero and nothing is annotated.
    if !nodes
        .iter()
        .any(|node| node.style.display == Display::TableCaption)
    {
        return;
    }
    let mut insets = vec![(0.0_f32, 0.0_f32); nodes.len()];
    let mut has_top_caption = vec![false; nodes.len()];
    let mut grid_widths = vec![None::<f32>; nodes.len()];
    let mut grid_tops = vec![None::<f32>; nodes.len()];
    for (index, node) in nodes.iter().enumerate() {
        if matches!(
            node.style.display,
            Display::TableRow
                | Display::TableRowGroup
                | Display::TableHeaderGroup
                | Display::TableFooterGroup
        ) && !matches!(node.style.position, Position::Absolute | Position::Fixed)
            && let Some(parent) = node.parent
            && matches!(
                nodes[parent].style.display,
                Display::Table | Display::InlineTable
            )
            && let Some(rect) = rect_by_index.get(index).copied().flatten()
        {
            grid_widths[parent] = Some(grid_widths[parent].unwrap_or(0.0).max(rect.width));
            grid_tops[parent] = Some(grid_tops[parent].unwrap_or(rect.y).min(rect.y));
        }
        if node.style.display != Display::TableCaption {
            continue;
        }
        let Some(parent) = node.parent else {
            continue;
        };
        if !matches!(
            nodes[parent].style.display,
            Display::Table | Display::InlineTable
        ) {
            continue;
        }
        let height = rect_by_index
            .get(index)
            .copied()
            .flatten()
            .map_or(0.0, |rect| rect.height);
        if node.style.caption_side_bottom {
            insets[parent].1 += height;
        } else {
            has_top_caption[parent] = true;
            insets[parent].0 += height;
        }
    }
    for (index, (caption_top, bottom)) in insets.into_iter().enumerate() {
        let style = &nodes[index].style;
        let top = if has_top_caption[index] {
            grid_tops[index]
                .zip(rect_by_index.get(index).copied().flatten())
                .map(|(row_top, table_rect)| {
                    let border_top = style.border_top_width.unwrap_or(style.border_width);
                    let before_row = if style.border_collapse {
                        border_top * 0.5
                    } else {
                        border_top + style.padding_lengths().top + style.border_spacing_y
                    };
                    (row_top - table_rect.y - before_row).max(0.0)
                })
                .unwrap_or(caption_top)
        } else {
            caption_top
        };
        if top <= 0.0 && bottom <= 0.0 {
            continue;
        }
        let right = if !style.border_collapse {
            grid_widths[index]
                .zip(rect_by_index.get(index).copied().flatten())
                .map_or(0.0, |(grid_width, rect)| {
                    let padding = style.padding_lengths();
                    let edges = padding.left
                        + padding.right
                        + style.border_left_width.unwrap_or(style.border_width)
                        + style.border_right_width.unwrap_or(style.border_width)
                        + 2.0 * style.border_spacing_x;
                    // The wrapper includes the caption, but table background
                    // paints only the grid and its own edges.
                    (rect.width - grid_width - edges).max(0.0)
                })
        } else {
            0.0
        };
        nodes[index]
            .style
            .custom_properties
            .get_or_insert_with(Default::default)
            .insert(
                TABLE_CAPTION_INSETS.to_string(),
                format!("{top} {bottom} {right}"),
            );
    }
}

impl PaintArtifact {
    /// No box of this artifact carries a `resize` value: the build kept a
    /// feature superset (`W3COS_PAINT_FEATURE_SKIP`) without
    /// [`FEATURE_RESIZE`]. `false` when no superset was kept.
    pub(crate) fn resize_absent(&self) -> bool {
        self.feature_presence
            .is_some_and(|bits| bits & FEATURE_RESIZE == 0)
    }

    /// No box of this artifact carries an inline background group: the
    /// kept feature superset lacks [`FEATURE_INLINE_BACKGROUND_GROUP`].
    /// `false` when no superset was kept.
    pub(crate) fn inline_background_groups_absent(&self) -> bool {
        self.feature_presence
            .is_some_and(|bits| bits & FEATURE_INLINE_BACKGROUND_GROUP == 0)
    }

    /// Applied decoration belongs to the originating box, not the child's
    /// computed property. Atomic inline and out-of-flow boxes isolate their
    /// contents, but an isolating box's own decoration still reaches them.
    pub(crate) fn ancestor_text_decorations(&self, index: usize) -> Vec<AppliedTextDecoration<'_>> {
        use w3cos_std::style::{Float, TextDecoration};
        let isolates = |style: &Style| {
            matches!(
                style.display,
                Display::InlineBlock | Display::InlineFlex | Display::InlineTable
            ) || style.float != Float::None
                || matches!(style.position, Position::Absolute | Position::Fixed)
        };
        let Some(source) = self.nodes.get(index) else {
            return Vec::new();
        };
        if !matches!(source.kind, ComponentKind::Text { .. }) || isolates(&source.style) {
            return Vec::new();
        }
        let anonymous = source
            .style
            .custom_properties
            .as_ref()
            .is_some_and(|properties| {
                properties
                    .get(w3cos_std::inline_text::SOURCE_RUN)
                    .is_some_and(|source| {
                        source.starts_with("text:") || source.starts_with("pseudo:")
                    })
            });
        let baseline_offset = |style: &Style| {
            style
                .custom_properties
                .as_ref()
                .and_then(|properties| properties.get("--w3cos-internal-vertical-align-length"))
                .and_then(|value| value.split_ascii_whitespace().next())
                .and_then(|value| value.parse::<f32>().ok())
                .unwrap_or(0.0)
        };
        let mut baseline_shift = baseline_offset(&source.style);
        let mut parent = source.parent;
        let mut owners = w3cos_std::inline_text::decoration_owners(&source.style)
            .into_iter()
            .rev()
            .map(|style| AppliedTextDecoration {
                style: std::borrow::Cow::Owned(style),
                baseline_shift: 0.0,
            })
            .collect::<Vec<_>>();
        while let Some(owner) = parent {
            let node = &self.nodes[owner];
            // The immediate anonymous text already paints its parent's used
            // decoration. Do not paint the same line twice at fractional edges.
            let represented_locally = anonymous
                && source.parent == Some(owner)
                && source.style.text_decoration == node.style.text_decoration
                && source.style.color == node.style.color
                && source.style.font_size == node.style.font_size
                && source.style.font_family == node.style.font_family
                && source.style.font_weight == node.style.font_weight
                && source.style.font_style == node.style.font_style
                && source.style.font_variant == node.style.font_variant;
            if node.style.text_decoration != TextDecoration::None
                && node.style.display != Display::Contents
                && !represented_locally
            {
                owners.push(AppliedTextDecoration {
                    style: std::borrow::Cow::Borrowed(&node.style),
                    baseline_shift,
                });
            }
            if isolates(&node.style) {
                break;
            }
            owners.extend(
                w3cos_std::inline_text::decoration_owners(&node.style)
                    .into_iter()
                    .rev()
                    .map(|style| AppliedTextDecoration {
                        style: std::borrow::Cow::Owned(style),
                        baseline_shift,
                    }),
            );
            baseline_shift += baseline_offset(&node.style);
            parent = node.parent;
        }
        owners.reverse();
        owners
    }

    /// The paragraph's available line geometry is distinct from the inline
    /// owner's intrinsic decoration box and its own bidi direction.
    /// Install LayoutNG's text pieces (see `text_fragments`).
    pub(crate) fn set_text_fragments(&mut self, fragments: &[crate::layout_ng::TextFragment]) {
        self.text_fragments.clear();
        self.vertical_text.clear();
        self.fragment_nodes.clear();
        if fragments.is_empty() {
            return;
        }
        let len = self.nodes.len();
        self.fragment_nodes.extend(
            fragments
                .iter()
                .filter(|fragment| fragment.index < len)
                .map(|fragment| fragment.index as u32),
        );
        self.fragment_nodes.sort_unstable();
        self.fragment_nodes.dedup();
        self.text_fragments = vec![Vec::new(); self.nodes.len()];
        for fragment in fragments {
            if let Some(slot) = self.text_fragments.get_mut(fragment.index) {
                slot.push((fragment.rect, fragment.text.clone()));
            }
            if fragment.vertical {
                if self.vertical_text.is_empty() {
                    self.vertical_text = vec![false; self.nodes.len()];
                }
                if let Some(flag) = self.vertical_text.get_mut(fragment.index) {
                    *flag = true;
                }
            }
        }
    }

    /// See `volatile_nodes`.
    pub(crate) fn volatile_nodes(&self) -> Option<&[u32]> {
        self.volatile_nodes.as_deref()
    }

    /// Sorted indices of the boxes with text fragments.
    pub(crate) fn fragment_nodes(&self) -> &[u32] {
        &self.fragment_nodes
    }

    pub(crate) fn is_vertical_text(&self, index: usize) -> bool {
        self.vertical_text.get(index).copied().unwrap_or(false)
    }

    pub(crate) fn text_fragments_of(
        &self,
        index: usize,
    ) -> &[(LayoutRect, crate::layout_ng::FragmentText)] {
        self.text_fragments
            .get(index)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub(crate) fn inline_line_context(&self, index: usize) -> Option<InlineLineContext> {
        if !self.text_fragments_of(index).is_empty() {
            // Each piece is already one positioned line box run.
            return None;
        }
        let node = self.nodes.get(index)?;
        let ComponentKind::Text { content } = &node.kind else {
            return None;
        };
        let has_preserved_break = content.contains('\u{2028}')
            || (matches!(
                node.style.white_space,
                WhiteSpace::Pre | WhiteSpace::PreWrap | WhiteSpace::PreLine
            ) && content.contains(['\n', '\r']));
        let has_preserved_tab = node.style.white_space == WhiteSpace::Pre && content.contains('\t');
        let has_float_lines = node
            .style
            .custom_properties
            .as_ref()
            .is_some_and(|properties| properties.contains_key("--w3cos-internal-float-line-bands"));
        if node.style.display != Display::Inline
            || !(has_preserved_break || has_preserved_tab || has_float_lines)
        {
            return None;
        }
        let mut owner = index;
        let mut parent = node.parent?;
        loop {
            let ancestor = self.nodes.get(parent)?;
            if matches!(ancestor.style.display, Display::Inline | Display::Contents) {
                owner = parent;
                parent = ancestor.parent?;
                continue;
            }
            let anonymous = ancestor
                .style
                .custom_properties
                .as_ref()
                .is_some_and(|properties| {
                    properties.contains_key("--w3cos-internal-inline-formatting-context")
                });
            if !anonymous
                && !matches!(
                    ancestor.style.display,
                    Display::Block
                        | Display::FlowRoot
                        | Display::ListItem
                        | Display::TableCell
                        | Display::InlineBlock
                )
            {
                return None;
            }
            let rect = self.rect_by_index.get(parent).copied().flatten()?;
            let padding = ancestor.style.padding_lengths();
            let left = ancestor
                .style
                .border_left_width
                .unwrap_or(ancestor.style.border_width)
                + padding.left;
            let right = ancestor
                .style
                .border_right_width
                .unwrap_or(ancestor.style.border_width)
                + padding.right;
            let line_box = LayoutRect {
                x: rect.x + left,
                width: (rect.width - left - right).max(1.0),
                ..rect
            };
            let mut first_line_box = line_box;
            if let Some((previous_index, previous)) = self.nodes[..owner]
                .iter()
                .enumerate()
                .rev()
                .find(|(_, previous)| {
                    previous.parent == Some(parent)
                        && previous.style.display != Display::None
                        && previous.style.float == w3cos_std::style::Float::None
                        && !matches!(
                            previous.style.position,
                            Position::Absolute | Position::Fixed
                        )
                })
                && matches!(
                    previous.style.display,
                    Display::Inline
                        | Display::InlineBlock
                        | Display::InlineFlex
                        | Display::InlineTable
                )
                && !matches!(&previous.kind, ComponentKind::Text { content } if content == "\u{2028}")
                && let Some(previous_rect) =
                    self.rect_by_index.get(previous_index).copied().flatten()
                && !self
                    .rect_by_index
                    .get(index)
                    .copied()
                    .flatten()
                    .is_some_and(|current| {
                        // A multiline fragment can have a union x at the line
                        // start even when its first line continues a sibling.
                        // Reset only when both axes prove a later, rewound line.
                        ancestor.style.direction == w3cos_std::style::TextDirection::Ltr
                            && current.x + 0.01 < previous_rect.x + previous_rect.width
                            && current.y >= previous_rect.y + previous_rect.height - 0.01
                    })
            {
                let margin = previous.style.margin_lengths();
                match ancestor.style.direction {
                    w3cos_std::style::TextDirection::Ltr => {
                        first_line_box.x =
                            (previous_rect.x + previous_rect.width + margin.right).max(line_box.x);
                        first_line_box.width =
                            (line_box.x + line_box.width - first_line_box.x).max(1.0);
                    }
                    w3cos_std::style::TextDirection::Rtl => {
                        first_line_box.width =
                            (previous_rect.x - margin.left - line_box.x).max(1.0);
                    }
                }
            }
            return Some(InlineLineContext {
                line_box,
                first_line_box,
                direction: ancestor.style.direction,
                text_align: ancestor.style.text_align,
                line_advance: Some(
                    crate::inline_line_metrics::baseline_text_metrics(&ancestor.style, &node.style)
                        .0,
                ),
                #[cfg(feature = "skia")]
                tab_stops: crate::render_skia::tab_stops_for_style(&ancestor.style),
            });
        }
    }

    pub fn paint_order_key(&self, index: usize) -> &[PaintOrderLevel] {
        self.paint_order.get(index).unwrap_or(&[])
    }

    /// `paint_order_key` as one integer: comparing two classes compares the
    /// two keys, so a stable sort by class equals a stable sort by key. The
    /// keys are sorted once per artifact instead of on every sort.
    pub fn paint_order_class(&self, index: usize) -> u32 {
        let classes = self
            .paint_classes
            .get_or_init(|| paint_classes_for(&self.paint_order));
        // Indices without a node have the empty key.
        classes.get(index).copied().unwrap_or(0)
    }

    /// CSS2 paint phase within one effective stack level. In-flow block
    /// backgrounds paint before floats, and inline content paints after
    /// floats. A float's descendants stay in the float phase as one atomic
    /// paint group.
    pub fn css2_paint_phase(&self, index: usize) -> u8 {
        let mut cursor = Some(index);
        while let Some(current) = cursor {
            let Some(node) = self.nodes.get(current) else {
                break;
            };
            if matches!(
                node.style.position,
                Position::Relative | Position::Absolute | Position::Fixed | Position::Sticky
            ) {
                // Positioned subtrees are atomic at their effective stack
                // level. Do not lift inline descendants above a later
                // positioned sibling merely because they are inline content.
                return 0;
            }
            if node.style.float != w3cos_std::style::Float::None {
                return 1;
            }
            if current != index
                && matches!(
                    node.style.display,
                    Display::InlineBlock | Display::InlineFlex | Display::InlineTable
                )
            {
                // Atomic inline-level boxes paint their entire subtree in the
                // inline phase. A block child must not be globally sorted
                // ahead of its inline-block parent's background.
                return 2;
            }
            cursor = node.parent;
        }
        self.nodes.get(index).map_or(0, |node| {
            u8::from(
                unadorned_block_text(node)
                    || matches!(
                        node.style.display,
                        Display::Inline
                            | Display::InlineBlock
                            | Display::InlineFlex
                            | Display::InlineTable
                    ),
            ) * 2
        })
    }

    pub fn build(
        nodes: impl IntoIterator<Item = PaintNode>,
        layout_cache: &[(LayoutRect, usize)],
        generation: u64,
    ) -> Self {
        Self::build_with_body_background(nodes, layout_cache, generation, None)
    }

    /// Fixed boxes and their descendants are attached to the viewport rather
    /// than the scrolling document. This mirrors layout's fixed-box owner.
    pub(crate) fn viewport_attached(&self, index: usize) -> bool {
        if self.viewport_attached_by_index.len() == self.nodes.len() {
            return self
                .viewport_attached_by_index
                .get(index)
                .copied()
                .unwrap_or(false);
        }
        let mut current = Some(index);
        while let Some(index) = current {
            let Some(node) = self.nodes.get(index) else {
                break;
            };
            if node.style.position == Position::Fixed {
                return true;
            }
            current = node.parent;
        }
        false
    }

    pub(crate) fn viewport_scroll_for(&self, index: usize) -> (f32, f32) {
        if self.viewport_attached(index) {
            (0.0, 0.0)
        } else {
            self.viewport_scroll
        }
    }

    pub fn build_with_body_background(
        nodes: impl IntoIterator<Item = PaintNode>,
        layout_cache: &[(LayoutRect, usize)],
        generation: u64,
        body_index: Option<usize>,
    ) -> Self {
        Self::build_with_body_background_and_viewport(
            nodes,
            layout_cache,
            generation,
            body_index,
            None,
        )
    }

    pub(crate) fn build_with_body_background_and_viewport(
        nodes: impl IntoIterator<Item = PaintNode>,
        layout_cache: &[(LayoutRect, usize)],
        generation: u64,
        body_index: Option<usize>,
        viewport: Option<(f32, f32)>,
    ) -> Self {
        Self::build_reusing(nodes, layout_cache, generation, body_index, viewport, None)
    }

    /// `build_with_body_background_and_viewport`, copying per-node output
    /// from `previous` (the last frame's artifact) where it still holds; see
    /// [`paint_append_reuse_enabled`].
    pub(crate) fn build_reusing(
        nodes: impl IntoIterator<Item = PaintNode>,
        layout_cache: &[(LayoutRect, usize)],
        generation: u64,
        body_index: Option<usize>,
        viewport: Option<(f32, f32)>,
        previous: Option<&PaintArtifact>,
    ) -> Self {
        Self::build_reusing_with(
            nodes,
            layout_cache,
            generation,
            body_index,
            viewport,
            previous,
            None,
        )
    }

    pub(crate) fn build_reusing_with(
        nodes: impl IntoIterator<Item = PaintNode>,
        layout_cache: &[(LayoutRect, usize)],
        generation: u64,
        body_index: Option<usize>,
        viewport: Option<(f32, f32)>,
        previous: Option<&PaintArtifact>,
        persist_raw: Option<PersistRaw<'_>>,
    ) -> Self {
        let mut nodes: Vec<_> = nodes.into_iter().collect();
        let collected = nodes.len();
        // Reused snapshots include paint-only UA children; reconstruct them
        // from their live hosts rather than accumulating another copy.
        nodes.retain(|node| !node.is_paint_only());
        // `z-index` applies to flex and grid items even when they are not
        // positioned: such an item paints like a positioned box at that level
        // and forms a stacking context. Paint reads only layout rects, so
        // modelling it as a zero-offset relative box changes ordering alone.
        let persist_mode = persistent_nodes_mode();
        let persist_on = matches!(persist_mode, 1 | 2);
        // The persistent path (see `persistent_nodes_plan`): boxes the
        // layout splice did not change take last build's adjusted node.
        let plan = persist_on
            .then(|| persistent_nodes_plan(&nodes, previous, body_index, viewport))
            .flatten();
        // The next plan's ranged table marks start from this build's.
        let persist_tables = (persist_on && persist_table_spans_mode() != 0).then(|| {
            plan.as_ref()
                .and_then(|plan| plan.tables.clone())
                .unwrap_or_else(|| std::sync::Arc::new(table_subtree_boxes(&nodes)))
        });
        // In place: a recycled node stays only where the plan keeps the box
        // clean; elsewhere (and on a box whose recycled kind may hold text
        // trimmed two builds ago) the box takes its flat-tree node again.
        let mut moved: Option<Vec<bool>> = None;
        if let Some(persist_raw) = persist_raw.as_ref() {
            let taken = std::mem::take(&mut *persist_raw.moved.borrow_mut());
            if taken.iter().any(|moved| *moved) {
                let aligned = collected == nodes.len() && taken.len() == nodes.len();
                // Recycled nodes sit at last-but-one build's indices: only
                // without inserted or removed boxes.
                let clean = |index: usize| {
                    aligned
                        && plan
                            .as_ref()
                            .is_some_and(|plan| plan.moved_from.is_none() && plan.clean[index])
                };
                for (index, was_moved) in taken.iter().enumerate() {
                    if *was_moved && !clean(index) && index < nodes.len() {
                        nodes[index] = (persist_raw.raw)(index);
                    }
                }
                match (&persist_raw.stale_trimmed, aligned) {
                    (Some(stale), true) => {
                        for &index in stale.iter() {
                            let index = index as usize;
                            if taken.get(index).copied().unwrap_or(false) && clean(index) {
                                nodes[index].kind = (persist_raw.raw)(index).kind;
                            }
                        }
                    }
                    _ => {
                        for (index, was_moved) in taken.iter().enumerate() {
                            if *was_moved && clean(index) {
                                nodes[index].kind = (persist_raw.raw)(index).kind;
                            }
                        }
                    }
                }
                moved = aligned.then_some(taken);
            }
        }
        // `W3COS_PAINT_PERSIST_PREFILL_SPANS`: in place on spliced nodes,
        // with last build's dirty list (same indices as this build's).
        let prefill_mode = persist_prefill_spans_mode();
        let prefill_visit = (prefill_mode != 0
            && moved.is_some()
            && persist_raw.as_ref().is_some_and(|raw| raw.spliced)
            && plan.as_ref().is_some_and(|plan| plan.moved_from.is_none()))
        .then(|| previous.and_then(|previous| previous.persist_dirty.clone()))
        .flatten();
        let persist_dirty = (prefill_mode != 0)
            .then(|| {
                plan.as_ref()
                    .filter(|plan| plan.moved_from.is_none())
                    .and_then(|plan| plan.dirty.clone())
                    .map(std::sync::Arc::new)
            })
            .flatten();
        let adjust = |mut nodes: Vec<PaintNode>, plan: Option<&PersistPlan>| {
            if let Some(plan) = plan {
                match prefill_visit.as_deref() {
                    Some(visit) if prefill_mode == 2 => {
                        let mut full = nodes.clone();
                        plan.prefill(&mut full, None);
                        plan.prefill(&mut nodes, Some(visit));
                        let same = full.iter().zip(&nodes).all(|(a, b)| {
                            a.style.ptr_eq(&b.style) || a.style.as_style() == b.style.as_style()
                        });
                        PERSIST_PREFILL_SPANS_STATS.with(|stats| {
                            let (prefills, mismatches) = stats.get();
                            stats.set((prefills + 1, mismatches + u64::from(!same)));
                        });
                        if !same {
                            eprintln!(
                                "[paint-persist-prefill-spans-verify] mismatch: {} visited",
                                visit.len()
                            );
                            nodes = full;
                        }
                    }
                    Some(visit) => {
                        PERSIST_PREFILL_SPANS_STATS.with(|stats| {
                            let (prefills, mismatches) = stats.get();
                            stats.set((prefills + 1, mismatches));
                        });
                        plan.prefill(&mut nodes, Some(visit));
                    }
                    None => plan.prefill(&mut nodes, None),
                }
            }
            // Clean boxes the per-box passes leave as they are.
            let skip = plan.map(|plan| plan.clean.as_slice());
            let skipped = |index: usize| skip.is_some_and(|skip| skip[index]);
            // `W3COS_PAINT_PERSIST_DIRTY_SCAN`: the passes below visit only
            // the boxes that are not clean.
            let dirty = plan.and_then(|plan| plan.dirty.as_deref());
            let dirty_verify = dirty.is_some() && persist_dirty_scan_mode() == 2;
            if let Some(dirty) = dirty {
                if dirty_verify {
                    // Coverage line for the multi-frame checks (verify only).
                    eprintln!(
                        "[paint-persist-dirty-scan-verify] walked {} of {}",
                        dirty.len(),
                        nodes.len()
                    );
                }
                PERSIST_DIRTY_SCAN_STATS.with(|stats| {
                    let (builds, mismatches) = stats.get();
                    stats.set((builds + 1, mismatches));
                });
            }
            let z_item = |index: usize, nodes: &[PaintNode]| {
                let node = &nodes[index];
                node.style.z_index != 0
                    && node.style.position == Position::Static
                    && node.parent.is_some_and(|parent| {
                        matches!(
                            nodes[parent].style.display,
                            Display::Flex | Display::Grid | Display::InlineFlex
                        )
                    })
            };
            let z_items: Vec<usize> = BoxScan::new(nodes.len(), skip, dirty)
                .filter(|&index| z_item(index, &nodes))
                .collect();
            if dirty_verify {
                let full: Vec<usize> = (0..nodes.len())
                    .filter(|&index| !skipped(index) && z_item(index, &nodes))
                    .collect();
                if full != z_items {
                    persist_dirty_scan_mismatch("z items");
                }
            }
            for index in z_items {
                nodes[index].style.position = Position::Relative;
            }
            let feature_mode = paint_feature_skip_mode();
            let features = (feature_mode != 0).then(|| feature_presence(&nodes, previous));
            let has_feature = |bit: u8| features.is_none_or(|features| features & bit != 0);
            let full_adjusted = dirty_verify.then(|| {
                let mut full = nodes.clone();
                let percent = adjust_used_styles(&mut full, viewport, skip, None);
                (full, percent)
            });
            let has_percentage_padding = adjust_used_styles(&mut nodes, viewport, skip, dirty);
            if let Some((full, percent)) = full_adjusted
                && (percent != has_percentage_padding || !same_paint_nodes(&full, &nodes))
            {
                persist_dirty_scan_mismatch("used styles");
                nodes = full;
            }
            if crate::layout_ng::mode() != crate::layout_ng::Mode::Off {
                run_feature_pass(
                    feature_mode,
                    has_feature(FEATURE_TRANSLATE_PERCENT),
                    &mut nodes,
                    |a, b| same_paint_nodes(a, b),
                    Clone::clone,
                    |nodes| resolve_translate_percent(nodes, layout_cache, skip),
                    "translate-percent",
                );
            }
            let has_background_image = |style: &Style| {
                style.background_image.as_deref().is_some_and(|value| {
                    value
                        .split(',')
                        .any(|layer| !layer.trim().eq_ignore_ascii_case("none"))
                })
            };
            let canvas_background_source = nodes
                .first()
                .filter(|node| node.style.background.a > 0 || has_background_image(&node.style))
                .map(|_| 0)
                .or_else(|| {
                    body_index.filter(|index| {
                        *index != 0
                            && nodes.get(*index).is_some_and(|node| {
                                node.style.display != Display::None
                                    && (node.style.background.a > 0
                                        || has_background_image(&node.style))
                            })
                    })
                })
                // A display:none document element has no box to paint on the
                // canvas. Its suppressed body cannot supply a fallback either.
                .filter(|_| nodes[0].style.display != Display::None);
            let root_rect = layout_cache
                .iter()
                .find_map(|(rect, index)| (*index == 0).then_some(*rect));
            if has_percentage_padding {
                let mut rects = vec![None; nodes.len()];
                for (rect, index) in layout_cache {
                    if let Some(slot) = rects.get_mut(*index) {
                        *slot = Some(*rect);
                    }
                }
                let root_basis = viewport
                    .map_or_else(|| root_rect.map_or(0.0, |rect| rect.width), |size| size.0);
                let mut child_bases = vec![root_basis; nodes.len()];
                for index in 0..nodes.len() {
                    let basis = nodes[index]
                        .parent
                        .map_or(root_basis, |parent| child_bases[parent]);
                    let percent_padding = {
                        let padding = &nodes[index].style.padding;
                        [padding.top, padding.right, padding.bottom, padding.left]
                            .iter()
                            .any(|edge| {
                                matches!(edge, Spacing::Percent(_) | Spacing::PaddingCalc { .. })
                            })
                    };
                    // Copy a shared style only when an edge resolves.
                    if percent_padding {
                        let style = nodes[index].style.make_mut();
                        for edge in [
                            &mut style.padding.top,
                            &mut style.padding.right,
                            &mut style.padding.bottom,
                            &mut style.padding.left,
                        ] {
                            match *edge {
                                Spacing::Percent(percent) => {
                                    *edge = Spacing::Px(basis * percent / 100.0);
                                }
                                Spacing::PaddingCalc { percent, px } => {
                                    *edge = Spacing::Px((basis * percent / 100.0 + px).max(0.0));
                                }
                                _ => {}
                            }
                        }
                    }
                    let style = nodes[index].style.as_style();
                    child_bases[index] =
                        if matches!(style.display, Display::Inline | Display::Contents) {
                            basis
                        } else {
                            let padding = style.padding_lengths();
                            let borders = style.border_left_width.unwrap_or(style.border_width)
                                + style.border_right_width.unwrap_or(style.border_width);
                            (rects[index].map_or(basis, |rect| rect.width)
                                - padding.left
                                - padding.right
                                - borders)
                                .max(0.0)
                        };
                }
            }
            let root_style = &nodes[0].style;
            let root_borders = (
                root_style
                    .border_left_width
                    .unwrap_or(root_style.border_width),
                root_style
                    .border_top_width
                    .unwrap_or(root_style.border_width),
                root_style
                    .border_right_width
                    .unwrap_or(root_style.border_width),
                root_style
                    .border_bottom_width
                    .unwrap_or(root_style.border_width),
            );
            let root_positioning_rect = root_rect.map(|rect| LayoutRect {
                x: rect.x + root_borders.0,
                y: rect.y + root_borders.1,
                width: (rect.width - root_borders.0 - root_borders.2).max(0.0),
                height: (rect.height - root_borders.1 - root_borders.3).max(0.0),
            });
            let canvas_background_style = canvas_background_source.map(|index| {
                let mut style = nodes[index].style.as_style().clone();
                style.border_width = 0.0;
                style.border_top_width = None;
                style.border_right_width = None;
                style.border_bottom_width = None;
                style.border_left_width = None;
                if style.background_position.as_deref().is_none_or(|position| {
                    matches!(
                        position.trim().to_ascii_lowercase().as_str(),
                        "" | "0% 0%" | "top left" | "left top"
                    )
                }) {
                    let (x, y) = if index == 0 {
                        // The positioning override already is the root padding box.
                        (0.0, 0.0)
                    } else {
                        // A propagated body background uses the document element's
                        // padding edge as its canvas positioning origin.
                        (
                            root_positioning_rect.map_or(0.0, |rect| rect.x),
                            root_positioning_rect.map_or(0.0, |rect| rect.y),
                        )
                    };
                    style.background_position = Some(format!("{x}px {y}px"));
                }
                style
            });
            let canvas_background = canvas_background_source
                .map(|index| nodes[index].style.background)
                .filter(|color| color.a > 0)
                .unwrap_or(Color::WHITE);
            // CSS paints a propagated body background on the canvas as if it
            // were specified on the root element. Relative positions need the
            // root box as their percentage basis; absolute positions have already
            // been converted to canvas coordinates above.
            let canvas_background_positioning_rect = canvas_background_source
                .filter(|index| {
                    *index == 0
                        || nodes[*index]
                            .style
                            .background_position
                            .as_deref()
                            .is_some_and(background_position_uses_relative_basis)
                })
                .and(root_positioning_rect);
            if let Some(index) = canvas_background_source {
                nodes[index].style.background = Color::TRANSPARENT;
                nodes[index].style.background_image = None;
            }
            let mut rect_by_index = vec![None; nodes.len()];
            for &(rect, index) in layout_cache {
                if let Some(slot) = rect_by_index.get_mut(index) {
                    *slot = Some(rect);
                }
            }
            let float_bands_full =
                dirty_verify.then(|| {
                    (0..nodes.len())
                        .filter(|&index| !skipped(index))
                        .filter(|&index| {
                            nodes[index].style.custom_properties.as_ref().is_some_and(
                                |properties| {
                                    properties.contains_key("--w3cos-internal-float-line-bands")
                                },
                            )
                        })
                        .collect::<Vec<_>>()
                });
            let mut float_bands_seen = Vec::new();
            for index in BoxScan::new(nodes.len(), skip, dirty) {
                let node = &mut nodes[index];
                if node
                    .style
                    .custom_properties
                    .as_ref()
                    .is_some_and(|properties| {
                        properties.contains_key("--w3cos-internal-float-line-bands")
                    })
                {
                    if float_bands_full.is_some() {
                        float_bands_seen.push(index);
                    }
                    if let Some(properties) = node.style.make_mut().custom_properties.as_mut() {
                        properties.remove("--w3cos-internal-float-line-bands");
                    }
                }
            }
            if float_bands_full.is_some_and(|full| full != float_bands_seen) {
                persist_dirty_scan_mismatch("float line bands");
            }
            if let Some((width, height)) = viewport {
                let refs: Vec<_> = nodes
                    .iter()
                    .map(|node| (&node.kind, node.style.as_style(), node.parent))
                    .collect();
                let flows =
                    crate::layout::resolve_float_text_layouts(&refs, &rect_by_index, width, height);
                for flow in flows {
                    let encoded = flow
                        .bands
                        .iter()
                        .map(|band| {
                            format!(
                                "{} {} {}",
                                band.x - flow.content.x,
                                band.y - flow.content.y,
                                band.width
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(";");
                    nodes[flow.text_index]
                        .style
                        .custom_properties
                        .get_or_insert_with(Default::default)
                        .insert("--w3cos-internal-float-line-bands".into(), encoded);
                }
            }
            let range_mode = paint_range_rebuild_mode();
            let source_len = nodes.len();
            // The boxes this frame's layout splice touched, when the previous
            // build's record covers the same boxes (see `PAINT_TOUCHED`).
            let range_touched = (range_mode != 0 && !has_percentage_padding)
                .then(|| {
                    PAINT_TOUCHED.with(|touched| {
                        touched
                            .borrow()
                            .as_ref()
                            .filter(|changes| changes.len == source_len)
                            .map(|changes| (changes.touched_bitmap(), changes.changed.clone()))
                    })
                })
                .flatten();
            let range_before = previous
                .and_then(|previous| previous.range_record.as_deref())
                .filter(|record| record.len == source_len && !record.percent_padding);
            let range_aligned = range_touched.as_ref().zip(range_before);
            let mut trimmed = (range_mode != 0 || persist_on).then(|| vec![false; source_len]);
            let verify_nodes = (range_mode == 2 && range_aligned.is_some()).then(|| nodes.clone());
            let trim_aligned = range_aligned
                .map(|((touched, _), before)| (touched.as_slice(), before.trimmed.as_slice()));
            let trim_full = dirty_verify.then(|| {
                let mut full = nodes.clone();
                let mut full_trimmed = trimmed.clone();
                let copied = trim_line_start_white_space_ranged(
                    &mut full,
                    &rect_by_index,
                    trim_aligned,
                    full_trimmed.as_mut(),
                    skip,
                    None,
                );
                (full, full_trimmed, copied)
            });
            let copied = trim_line_start_white_space_ranged(
                &mut nodes,
                &rect_by_index,
                trim_aligned,
                trimmed.as_mut(),
                skip,
                dirty,
            );
            if let Some((full, full_trimmed, full_copied)) = trim_full
                && (full_copied != copied
                    || full_trimmed != trimmed
                    || !same_paint_nodes(&full, &nodes))
            {
                persist_dirty_scan_mismatch("line-start trim");
                nodes = full;
                trimmed = full_trimmed;
            }
            if let Some(mut fresh) = verify_nodes {
                trim_collapsible_inline_whitespace_at_line_start(&mut fresh, &rect_by_index);
                let same = fresh
                    .iter()
                    .zip(&nodes)
                    .all(|(a, b)| a.kind == b.kind && a.style.as_style() == b.style.as_style());
                if !same {
                    PAINT_RANGE_STATS.with(|stats| {
                        let mut value = stats.get();
                        value.2 += 1;
                        stats.set(value);
                    });
                    eprintln!("[paint-range-rebuild-verify] trim mismatch ({copied} copied)");
                    nodes = fresh;
                    trimmed = None;
                }
            }
            run_feature_pass(
                feature_mode,
                has_feature(FEATURE_TABLE),
                &mut nodes,
                |a, b| same_paint_nodes(a, b),
                Clone::clone,
                |nodes| {
                    table_passes::run(
                        nodes,
                        &rect_by_index,
                        plan.and_then(|plan| plan.tables.as_deref())
                            .map(Vec::as_slice),
                    )
                },
                "table",
            );
            {
                let mut both = (
                    std::mem::take(&mut nodes),
                    std::mem::take(&mut rect_by_index),
                );
                run_feature_pass(
                    feature_mode,
                    has_feature(FEATURE_VIDEO_CONTROLS),
                    &mut both,
                    |a, b| same_paint_nodes(&a.0, &b.0) && a.1 == b.1,
                    Clone::clone,
                    |(nodes, rects)| media_controls::append(nodes, rects),
                    "media-controls",
                );
                (nodes, rect_by_index) = both;
            }
            let ordinal_scan = range_aligned
                .filter(|(_, before)| before.ordinals_plain)
                .map(|((_, changed), _)| changed.as_slice());
            let (logical_paint_ordinals, ordinals_plain) =
                positioned_inline_fragment_ordinals_ranged(&nodes, ordinal_scan, source_len);
            if range_mode == 2
                && ordinal_scan.is_some()
                && logical_paint_ordinals != positioned_inline_fragment_ordinals(&nodes)
            {
                PAINT_RANGE_STATS.with(|stats| {
                    let mut value = stats.get();
                    value.2 += 1;
                    stats.set(value);
                });
                eprintln!("[paint-range-rebuild-verify] ordinal mismatch");
            }
            if range_aligned.is_some() {
                PAINT_RANGE_STATS.with(|stats| {
                    let mut value = stats.get();
                    value.0 += 1;
                    value.1 += u64::from(copied);
                    stats.set(value);
                });
            }
            let persist_trimmed = persist_on.then(|| {
                let all = |bits: &[bool]| -> Vec<u32> {
                    bits.iter()
                        .enumerate()
                        .filter(|(_, trimmed)| **trimmed)
                        .map(|(index, _)| index as u32)
                        .collect()
                };
                // Only boxes the trim visited can be marked.
                let mut list: Vec<u32> = trimmed
                    .as_deref()
                    .map(|bits| match dirty {
                        Some(dirty) => {
                            let list: Vec<u32> = dirty
                                .iter()
                                .copied()
                                .filter(|&index| bits[index as usize])
                                .collect();
                            if dirty_verify && list != all(bits) {
                                persist_dirty_scan_mismatch("trimmed list");
                                return all(bits);
                            }
                            list
                        }
                        None => all(bits),
                    })
                    .unwrap_or_default();
                if let Some(plan) = plan {
                    list.extend(
                        plan.trimmed
                            .iter()
                            .filter(|&&index| plan.clean[index as usize]),
                    );
                    list.sort_unstable();
                }
                std::sync::Arc::new(list)
            });
            let trimmed = trimmed.filter(|_| range_mode != 0);

            let range_record = trimmed.map(|trimmed| {
                std::sync::Arc::new(RangeRecord {
                    len: source_len,
                    trimmed,
                    ordinals_plain,
                    percent_padding: has_percentage_padding,
                })
            });
            (
                nodes,
                rect_by_index,
                logical_paint_ordinals,
                range_record,
                features,
                canvas_background,
                canvas_background_style,
                canvas_background_source,
                canvas_background_positioning_rect,
                source_len,
                persist_trimmed,
                has_percentage_padding,
            )
        };
        let verify = persist_mode == 2 || persist_inplace_mode() == 2;
        let source_nodes = (verify && plan.is_some()).then(|| {
            let mut source = nodes.clone();
            // The full build starts from flat-tree nodes, not recycled ones.
            if let (Some(moved), Some(persist_raw)) = (moved.as_ref(), persist_raw.as_ref()) {
                for (index, was_moved) in moved.iter().enumerate() {
                    if *was_moved {
                        source[index] = (persist_raw.raw)(index);
                    }
                }
            }
            source
        });
        let (
            nodes,
            rect_by_index,
            logical_paint_ordinals,
            range_record,
            features,
            canvas_background,
            canvas_background_style,
            canvas_background_source,
            canvas_background_positioning_rect,
            source_len,
            persist_trimmed,
            has_percentage_padding,
        ) = {
            let fast = adjust(nodes, plan.as_ref());
            match source_nodes {
                Some(source) => {
                    let full = adjust(source, None);
                    if persistent_nodes_same(&fast, &full) {
                        // Coverage line for the WPT check (verify only).
                        if let Some(plan) = plan.as_ref() {
                            eprintln!(
                                "[paint-persistent-nodes-verify] same clean {} of {}",
                                plan.clean_count,
                                fast.0.len()
                            );
                        }
                        fast
                    } else {
                        PERSISTENT_NODES_MISMATCHES.with(|count| count.set(count.get() + 1));
                        full
                    }
                }
                None => fast,
            }
        };
        if let Some(plan) = plan.as_ref() {
            PERSISTENT_NODES_STATS.with(|stats| {
                let (frames, clean) = stats.get();
                stats.set((frames + 1, clean + plan.clean_count as u64));
            });
        }
        let mut artifact = Self {
            cull_band: PAINT_CULL_BAND.with(std::cell::Cell::get),
            logical_paint_ordinals,
            range_record,
            feature_presence: features,
            body_index,
            rect_by_index,
            node_properties: vec![PaintProperties::default(); nodes.len()],
            self_clip: vec![0; nodes.len()],
            z_order: vec![0; nodes.len()],
            paint_order: PaintOrderKeys::with_len(nodes.len()),
            sticky_owner: vec![None; nodes.len()],
            viewport_attached_by_index: vec![false; nodes.len()],
            nodes,
            canvas_background,
            canvas_background_style,
            canvas_background_source,
            canvas_background_positioning_rect,
            generation,
            persist_trimmed,
            persist_percent_padding: has_percentage_padding,
            persist_viewport: viewport,
            persist_tables,
            persist_dirty,
            ..Self::default()
        };
        let presence_mode = paint_caption_column_presence_mode();
        let absent = |bit: u8| {
            presence_mode != 0
                && artifact
                    .feature_presence
                    .is_some_and(|features| features & bit == 0)
        };
        let has_columns = |nodes: &[PaintNode]| {
            nodes.iter().any(|node| {
                node.style.column_count.is_some()
                    || !matches!(node.style.column_width, w3cos_std::style::Dimension::Auto)
            })
        };
        // A caption is a table part: without one, the caption pass would
        // find no caption and annotate nothing.
        let captions = if absent(FEATURE_TABLE) {
            caption_column_presence_count(0);
            presence_mode == 2
                && artifact
                    .nodes
                    .iter()
                    .any(|node| node.style.display == Display::TableCaption)
                && {
                    caption_column_presence_mismatch("table caption");
                    true
                }
        } else {
            true
        };
        if captions {
            annotate_table_caption_paint_insets(&mut artifact.nodes, &artifact.rect_by_index);
        }

        let columns = if absent(FEATURE_COLUMNS) {
            caption_column_presence_count(1);
            presence_mode == 2 && has_columns(&artifact.nodes) && {
                caption_column_presence_mismatch("multicol");
                true
            }
        } else {
            has_columns(&artifact.nodes)
        };
        if columns {
            let column_nodes: Vec<_> = artifact
                .nodes
                .iter()
                .map(|node| (node.style.as_style(), node.parent))
                .collect();
            let fragment_plans = crate::layout::resolve_column_fragment_plans(
                &column_nodes,
                &artifact.rect_by_index,
                viewport,
            );
            for (index, plan) in fragment_plans.iter().enumerate() {
                if let Some(plan) = plan.as_ref().filter(|plan| plan.auto_height)
                    && let Some(rect) = artifact.rect_by_index[index].as_mut()
                {
                    rect.height = plan.principal_height;
                }
            }
            artifact.column_fragments = (0..artifact.nodes.len())
                .map(|index| artifact.build_column_fragments(index, viewport, &fragment_plans))
                .collect();
        }

        if persistent_nodes_mode() == 3
            && let Some(previous) = previous
        {
            persistent_nodes_probe(&artifact, previous, source_len);
        }
        if paint_append_reuse_enabled() {
            artifact.append_all_reusing(previous);
            return artifact;
        }
        let mut memo = AppendMemo::new(&artifact.nodes);
        for index in 0..artifact.nodes.len() {
            artifact.append_node(index, &mut memo);
        }
        // Every build has its own serial: per-frame caches keyed by it (the
        // damage ink cache) must not take another build's entries.
        artifact.serial = next_artifact_serial();
        artifact
    }

    /// Per node, whether its non-geometric paint inputs and property output
    /// equal `previous`'s, when this artifact was built against `previous`.
    pub(crate) fn append_unchanged_against(&self, previous: &PaintArtifact) -> Option<&[bool]> {
        let record = self.append_record.as_deref()?;
        (record.base != 0
            && record.base == previous.serial
            && record.unchanged.len() == self.nodes.len())
        .then_some(record.unchanged.as_slice())
    }

    /// The culled spans this build copied or carried over from `previous`
    /// (sorted, disjoint): their boxes kept their inputs and pushed no
    /// property node and no display item in either build.
    pub(crate) fn culled_spans_against(&self, previous: &PaintArtifact) -> Option<&[CullSpan]> {
        let record = self.append_record.as_deref()?;
        (record.base != 0
            && record.base == previous.serial
            && record.unchanged.len() == self.nodes.len())
        .then_some(record.cull_spans.as_slice())
    }

    /// Whether this build recorded `index` pushing no property node (and
    /// so no rect-carrying clip or scroll node). `false` without a record.
    #[inline]
    pub(crate) fn pushed_no_property_nodes(&self, index: usize) -> bool {
        let Some(record) = self.append_record.as_deref() else {
            return false;
        };
        let (Some(at), next) = (record.marks.get(index), record.marks.get(index + 1)) else {
            return false;
        };
        let next = next.map_or(
            (
                self.properties.transforms.len() as u32,
                self.properties.clips.len() as u32,
                self.properties.effects.len() as u32,
                self.properties.scrolls.len() as u32,
            ),
            |next| (next.transforms, next.clips, next.effects, next.scrolls),
        );
        record.marks.len() == self.nodes.len()
            && next == (at.transforms, at.clips, at.effects, at.scrolls)
    }

    /// Identifies this build (see `append_runs_since`).
    pub(crate) fn serial(&self) -> u64 {
        self.serial
    }

    /// `append_runs_against` for the build whose `serial` is `base`.
    pub(crate) fn append_runs_since(&self, base: u64) -> Option<&[[u32; 4]]> {
        let record = self.append_record.as_deref()?;
        (base != 0 && record.base == base && record.unchanged.len() == self.nodes.len())
            .then_some(record.runs.as_slice())
    }

    /// The aligned runs this artifact copied from `previous` (see
    /// `AppendRecord::runs`), when it was built against `previous`.
    pub(crate) fn append_runs_against(&self, previous: &PaintArtifact) -> Option<&[[u32; 4]]> {
        let record = self.append_record.as_deref()?;
        (record.base != 0
            && record.base == previous.serial
            && record.unchanged.len() == self.nodes.len())
        .then_some(record.runs.as_slice())
    }

    fn mark(&self) -> AppendMark {
        AppendMark {
            transforms: self.properties.transforms.len() as u32,
            clips: self.properties.clips.len() as u32,
            effects: self.properties.effects.len() as u32,
            scrolls: self.properties.scrolls.len() as u32,
            self_scroll: 0,
            items: self.display_items.len() as u32,
            chunks: self.chunks.len() as u32,
            levels: self.paint_order.levels.len() as u32,
        }
    }

    /// Copies the previous build's output for the run of nodes from
    /// `start` whose inputs and rects equal last frame's, whose parents'
    /// output is the same, and whose every appended array is aligned with
    /// last frame's at `start`: slices instead of per-node work (Blink's
    /// PaintController copies a cached subsequence wholesale). Returns the
    /// run's end (`start` when there is no run).
    #[allow(clippy::too_many_arguments)]
    fn copy_aligned_run(
        &mut self,
        start: usize,
        previous: &PaintArtifact,
        record: &AppendRecord,
        same: &mut [bool],
        unchanged: &mut [bool],
        memo: &mut AppendMemo,
        marks: &mut Vec<AppendMark>,
        runs: &mut Vec<[u32; 4]>,
        cull_spans: &mut Vec<CullSpan>,
    ) -> usize {
        let len = self.nodes.len();
        let mark = self.mark();
        let old = record.marks[start];
        if (
            mark.transforms,
            mark.clips,
            mark.effects,
            mark.scrolls,
            mark.items,
            mark.chunks,
            mark.levels,
        ) != (
            old.transforms,
            old.clips,
            old.effects,
            old.scrolls,
            old.items,
            old.chunks,
            old.levels,
        ) {
            return start;
        }
        let mut end = start;
        // Boxes in neither touched range kept their rects and inputs: jump
        // over such a gap at once when its outside parents are the same.
        while end < len {
            if self.touched_gaps
                && let Some(touched) = self.touched.as_ref()
            {
                let gap =
                    crate::incremental_layout::ranges_gap_end(&touched.changed, end, len).min(
                        crate::incremental_layout::ranges_gap_end(&touched.moved, end, len),
                    );
                if gap > end + 1 && self.gap_parents_same(start, end, same) {
                    let verify = paint_touched_mode() == 2;
                    if !verify || self.gap_matches(start, end, gap, previous, same) {
                        end = gap;
                        continue;
                    }
                    eprintln!("[paint-touched-verify] mismatch: gap {end}..{gap} differs");
                    PAINT_TOUCHED_MISMATCHES.with(|count| count.set(count.get() + 1));
                }
            }
            let parent_same = self.nodes[end]
                .parent
                .is_none_or(|parent| parent >= start || same[parent]);
            if !parent_same
                || self.rect_by_index[end] != previous.rect_by_index[end]
                || !self.column_fragments.get(end).is_none_or(Vec::is_empty)
                || !previous.column_fragments.get(end).is_none_or(Vec::is_empty)
                || !self.inputs_match(end, previous)
            {
                break;
            }
            end += 1;
        }
        // A parent inside the run is copied with it; one at or after `end`
        // cannot precede its child.
        if end - start < 8 {
            return start;
        }
        let stop = record.marks.get(end).copied().unwrap_or(AppendMark {
            transforms: previous.properties.transforms.len() as u32,
            clips: previous.properties.clips.len() as u32,
            effects: previous.properties.effects.len() as u32,
            scrolls: previous.properties.scrolls.len() as u32,
            self_scroll: 0,
            items: previous.display_items.len() as u32,
            chunks: previous.chunks.len() as u32,
            levels: previous.paint_order.levels.len() as u32,
        });
        runs.push([start as u32, end as u32, old.items, stop.items]);
        // Last frame's culled spans in the run hold again: same indices,
        // same output (and so no item, no property node).
        let old_spans = &record.cull_spans;
        let mut at = old_spans.partition_point(|span| (span.start as usize) < start);
        while let Some(span) = old_spans.get(at)
            && span.end as usize <= end
        {
            cull_spans.push(*span);
            at += 1;
        }
        let range = |a: u32, b: u32| a as usize..b as usize;
        self.properties.transforms.extend_from_slice(
            &previous.properties.transforms[range(old.transforms, stop.transforms)],
        );
        self.properties
            .clips
            .extend_from_slice(&previous.properties.clips[range(old.clips, stop.clips)]);
        self.properties
            .effects
            .extend_from_slice(&previous.properties.effects[range(old.effects, stop.effects)]);
        self.properties
            .scrolls
            .extend_from_slice(&previous.properties.scrolls[range(old.scrolls, stop.scrolls)]);
        self.display_items
            .extend_from_slice(&previous.display_items[range(old.items, stop.items)]);
        self.chunks
            .extend_from_slice(&previous.chunks[range(old.chunks, stop.chunks)]);
        self.paint_order
            .levels
            .extend_from_slice(&previous.paint_order.levels[range(old.levels, stop.levels)]);
        let nodes = start..end;
        self.paint_order.spans[nodes.clone()]
            .copy_from_slice(&previous.paint_order.spans[nodes.clone()]);
        self.node_properties[nodes.clone()]
            .copy_from_slice(&previous.node_properties[nodes.clone()]);
        self.self_clip[nodes.clone()].copy_from_slice(&previous.self_clip[nodes.clone()]);
        self.z_order[nodes.clone()].copy_from_slice(&previous.z_order[nodes.clone()]);
        self.viewport_attached_by_index[nodes.clone()]
            .copy_from_slice(&previous.viewport_attached_by_index[nodes.clone()]);
        self.sticky_owner[nodes.clone()].copy_from_slice(&previous.sticky_owner[nodes.clone()]);
        let old_memo = &record.memo;
        memo.fixed_owner[nodes.clone()].copy_from_slice(&old_memo.fixed_owner[nodes.clone()]);
        memo.context_owner[nodes.clone()].copy_from_slice(&old_memo.context_owner[nodes.clone()]);
        memo.owns_context[nodes.clone()].copy_from_slice(&old_memo.owns_context[nodes.clone()]);
        memo.stacking[nodes.clone()].copy_from_slice(&old_memo.stacking[nodes.clone()]);
        memo.ancestor_phase[nodes.clone()].copy_from_slice(&old_memo.ancestor_phase[nodes.clone()]);
        marks.extend_from_slice(&record.marks[nodes.clone()]);
        same[nodes.clone()].fill(true);
        unchanged[nodes].fill(true);
        end
    }

    /// `W3COS_PAINT_CULL_RUNS`: copies the previous build's output for the
    /// run of boxes from `start` that the cull band dropped last frame and
    /// drops again: each pushed no property node and no display item last
    /// frame, kept its inputs, size and inline position (only its block
    /// position may differ: everything below a growing message shifts), and
    /// its new rect is still outside the band. Blink does not walk a box
    /// outside the cull rect at all; here the per-box work shrinks to these
    /// checks and slice copies, without the display items' alignment an
    /// aligned run needs. Returns the run's end (`start` when there is none).
    #[allow(clippy::too_many_arguments)]
    /// One box of `copy_culled_run`: `None` ends the run, `Some(visible)`
    /// takes the box (a visible one was culled by the band).
    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    fn culled_box_check(
        &self,
        end: usize,
        start: usize,
        previous: &PaintArtifact,
        record: &AppendRecord,
        same: &[bool],
        tail: AppendMark,
        root: PaintProperties,
        top: f32,
        bottom: f32,
    ) -> Option<bool> {
        let at = record.marks[end];
        let next = record.marks.get(end + 1).copied().unwrap_or(tail);
        if (next.transforms, next.clips, next.effects, next.scrolls)
            != (at.transforms, at.clips, at.effects, at.scrolls)
            || next.items != at.items
            || next.chunks != at.chunks
        {
            return None;
        }
        let node = &self.nodes[end];
        let parent_same = node.parent.is_none_or(|parent| {
            (start..end).contains(&parent) || (parent < start && same[parent])
        });
        if !parent_same
            || !self.column_fragments.get(end).is_none_or(Vec::is_empty)
            || !previous.column_fragments.get(end).is_none_or(Vec::is_empty)
        {
            return None;
        }
        let (Some(rect), Some(old_rect)) = (self.rect_by_index[end], previous.rect_by_index[end])
        else {
            return None;
        };
        if rect.x != old_rect.x || rect.width != old_rect.width || rect.height != old_rect.height {
            return None;
        }
        if !self.inputs_match(end, previous) {
            return None;
        }
        if node.style.visibility != Visibility::Visible {
            return Some(false);
        }
        let cullable = end != 0
            && previous.node_properties[end] == root
            && at.self_scroll as usize == root.scroll
            && !matches!(node.style.position, Position::Fixed | Position::Sticky)
            && !previous
                .viewport_attached_by_index
                .get(end)
                .copied()
                .unwrap_or(true)
            && previous.sticky_owner.get(end).copied().flatten().is_none();
        (cullable && self.outside_band(node, rect, top, bottom)).then_some(true)
    }

    /// `append_display_items`' cull test, on the copied output.
    #[inline(always)]
    fn outside_band(&self, node: &PaintNode, rect: LayoutRect, top: f32, bottom: f32) -> bool {
        let bounds = if node.style.border_collapse && node.style.display == Display::TableCell {
            let (left, right) = paint_inline_border_widths(&node.style);
            LayoutRect {
                x: rect.x - left,
                width: rect.width + left + right,
                ..rect
            }
        } else {
            rect
        };
        bounds.y + bounds.height < top || bounds.y > bottom
    }

    /// `W3COS_PAINT_CULL_RUN_GAPS`: `culled_box_check` for a box in last
    /// frame's culled spans whose inputs the layout did not change. Its
    /// marks, columns, viewport / sticky attachment and style held when the
    /// span was copied; what may differ is its parent's output, its geometry
    /// and the root's properties.
    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    fn culled_box_still_culled(
        &self,
        end: usize,
        start: usize,
        previous: &PaintArtifact,
        record: &AppendRecord,
        same: &[bool],
        root: PaintProperties,
        top: f32,
        bottom: f32,
    ) -> Option<bool> {
        let node = &self.nodes[end];
        let parent_same = node.parent.is_none_or(|parent| {
            (start..end).contains(&parent) || (parent < start && same[parent])
        });
        if !parent_same || !self.column_fragments.get(end).is_none_or(Vec::is_empty) {
            return None;
        }
        let (Some(rect), Some(old_rect)) = (self.rect_by_index[end], previous.rect_by_index[end])
        else {
            return None;
        };
        if rect.x != old_rect.x || rect.width != old_rect.width || rect.height != old_rect.height {
            return None;
        }
        if node.style.visibility != Visibility::Visible {
            return Some(false);
        }
        let cullable = end != 0
            && previous.node_properties[end] == root
            && record.marks[end].self_scroll as usize == root.scroll;
        (cullable && self.outside_band(node, rect, top, bottom)).then_some(true)
    }

    /// `W3COS_PAINT_CULL_SUBTREES`: last frame's culled span starting at
    /// `at`, when it may be taken whole: its boxes kept their inputs (a gap
    /// of the changed ranges), its outside parents have the same output, its
    /// first box kept its size and inline position, and its extent moved
    /// with that box lies past the band (with a pixel to spare for rounding).
    #[allow(clippy::too_many_arguments)]
    fn whole_culled_span(
        &self,
        at: usize,
        start: usize,
        previous: &PaintArtifact,
        record: &AppendRecord,
        same: &[bool],
        changed: &[(u32, u32)],
        root: PaintProperties,
        top: f32,
        bottom: f32,
        cursor: Option<&mut CullSpanCursor>,
    ) -> Option<CullSpan> {
        let spans = &record.cull_spans;
        let index = match cursor {
            Some(cursor) => cursor.starting(spans, at),
            None => spans.partition_point(|span| (span.start as usize) < at),
        };
        let span = *spans.get(index).filter(|span| span.start as usize == at)?;
        let end = span.end as usize;
        if span.parent_count > 2 || span.root != root {
            return None;
        }
        let parents_same = span.parents[..span.parent_count as usize]
            .iter()
            .all(|&parent| {
                let parent = parent as usize;
                (start..at).contains(&parent) || (parent < start && same[parent])
            });
        if !parents_same
            || crate::incremental_layout::ranges_gap_end(changed, at, self.nodes.len()) < end
        {
            return None;
        }
        let (Some(rect), Some(old_rect)) = (self.rect_by_index[at], previous.rect_by_index[at])
        else {
            return None;
        };
        if rect.x != old_rect.x || rect.width != old_rect.width || rect.height != old_rect.height {
            return None;
        }
        let (low, high) = (rect.y + span.top, rect.y + span.bottom);
        (low > bottom + 1.0 || high < top - 1.0).then_some(span)
    }

    /// Records boxes `segment..end` of a culled run, checked box by box, as
    /// one span with its extent, visible count and outside parents.
    fn close_cull_segment(
        &self,
        segment: usize,
        end: usize,
        root: PaintProperties,
        cull_spans: &mut Vec<CullSpan>,
    ) {
        // Only the span readers need them; skip the walk otherwise.
        if segment >= end || !cull_spans_wanted() {
            return;
        }
        let first = self.rect_by_index[segment].map_or(0.0, |rect| rect.y);
        let mut span = CullSpan {
            start: segment as u32,
            end: end as u32,
            top: f32::INFINITY,
            bottom: f32::NEG_INFINITY,
            visible: 0,
            parents: [0; 2],
            parent_count: 0,
            root,
        };
        for index in segment..end {
            let node = &self.nodes[index];
            if let Some(rect) = self.rect_by_index[index] {
                span.top = span.top.min(rect.y - first);
                span.bottom = span.bottom.max(rect.y + rect.height - first);
            } else {
                // A box without a rect is never in a run; keep it unusable.
                span.parent_count = 3;
            }
            if node.style.visibility == Visibility::Visible {
                span.visible += 1;
            }
            if let Some(parent) = node.parent
                && parent < segment
                && span.parent_count <= 2
                && !span.parents[..span.parent_count as usize].contains(&(parent as u32))
            {
                if span.parent_count == 2 {
                    span.parent_count = 3;
                } else {
                    span.parents[span.parent_count as usize] = parent as u32;
                    span.parent_count += 1;
                }
            }
        }
        cull_spans.push(span);
    }

    fn copy_culled_run(
        &mut self,
        start: usize,
        previous: &PaintArtifact,
        record: &AppendRecord,
        same: &mut [bool],
        unchanged: &mut [bool],
        memo: &mut AppendMemo,
        marks: &mut Vec<AppendMark>,
        cull_spans: &mut Vec<CullSpan>,
        span_cursor: &mut CullSpanCursor,
    ) -> usize {
        let Some((top, bottom)) = self.cull_band else {
            return start;
        };
        let len = self.nodes.len();
        let mark = self.mark();
        let old = record.marks[start];
        if (mark.transforms, mark.clips, mark.effects, mark.scrolls)
            != (old.transforms, old.clips, old.effects, old.scrolls)
        {
            return start;
        }
        let tail = AppendMark {
            transforms: previous.properties.transforms.len() as u32,
            clips: previous.properties.clips.len() as u32,
            effects: previous.properties.effects.len() as u32,
            scrolls: previous.properties.scrolls.len() as u32,
            self_scroll: 0,
            items: previous.display_items.len() as u32,
            chunks: previous.chunks.len() as u32,
            levels: previous.paint_order.levels.len() as u32,
        };
        let root = previous
            .node_properties
            .first()
            .copied()
            .unwrap_or_default();
        let (mut end, mut culled) = (start, 0u64);
        let gaps = paint_cull_run_gaps_mode();
        // The fast check needs the layout's changed ranges (an untouched box
        // kept its inputs) and last frame's spans.
        let changed = (gaps != 0 && paint_touched_mode() == 1)
            .then(|| self.touched.as_ref().map(|touched| &touched.changed))
            .flatten();
        let spans = &record.cull_spans;
        let (mut span_end, mut gap_end) = (start, start);
        let mut fast_count = 0u64;
        let subtrees = paint_cull_subtrees_mode();
        let whole = (subtrees != 0 && paint_touched_mode() == 1 && self.touched_gaps)
            .then(|| self.touched.as_ref().map(|touched| &touched.changed))
            .flatten();
        let spans_before = cull_spans.len();
        let mut segment = start;
        let (mut whole_spans, mut whole_boxes, mut whole_mismatch) = (0u64, 0u64, 0u64);
        // `W3COS_PAINT_CULL_SPAN_CURSOR`: boxes only move forward.
        let use_cursor = paint_cull_span_cursor_mode() != 0;
        while end < len {
            if let Some(changed) = whole
                && let Some(span) = self.whole_culled_span(
                    end,
                    start,
                    previous,
                    record,
                    same,
                    changed,
                    root,
                    top,
                    bottom,
                    use_cursor.then_some(&mut *span_cursor),
                )
            {
                let span_end = span.end as usize;
                let agrees = subtrees != 2
                    || (end..span_end).all(|index| {
                        self.culled_box_check(
                            index, start, previous, record, same, tail, root, top, bottom,
                        )
                        .is_some()
                    });
                if agrees {
                    self.close_cull_segment(segment, end, root, cull_spans);
                    cull_spans.push(span);
                    culled += span.visible as u64;
                    whole_spans += 1;
                    whole_boxes += (span_end - end) as u64;
                    end = span_end;
                    segment = end;
                    continue;
                }
                eprintln!("[paint-cull-subtrees-verify] mismatch: span {end}..{span_end}");
                whole_mismatch += 1;
            }
            if let Some(changed) = changed {
                if end >= span_end {
                    let at = if use_cursor {
                        span_cursor.ending(spans, end)
                    } else {
                        spans.partition_point(|span| span.end as usize <= end)
                    };
                    span_end = spans
                        .get(at)
                        .filter(|span| span.start as usize <= end)
                        .map_or(end, |span| span.end as usize);
                }
                if end >= gap_end {
                    gap_end = crate::incremental_layout::ranges_gap_end(changed, end, len);
                }
                if end < span_end && end < gap_end {
                    let fast = self.culled_box_still_culled(
                        end, start, previous, record, same, root, top, bottom,
                    );
                    if gaps == 2 {
                        let full = self.culled_box_check(
                            end, start, previous, record, same, tail, root, top, bottom,
                        );
                        if fast != full {
                            eprintln!(
                                "[paint-cull-run-gaps-verify] mismatch: box {end} fast {fast:?} full {full:?}"
                            );
                            PAINT_CULL_RUN_GAPS_STATS.with(|stats| {
                                let (a, b) = stats.get();
                                stats.set((a, b + 1));
                            });
                        }
                    }
                    match fast {
                        None => break,
                        Some(counted) => {
                            culled += counted as u64;
                            fast_count += 1;
                            end += 1;
                            continue;
                        }
                    }
                }
            }
            match self.culled_box_check(end, start, previous, record, same, tail, root, top, bottom)
            {
                None => break,
                Some(counted) => culled += counted as u64,
            }
            end += 1;
        }
        if fast_count > 0 {
            PAINT_CULL_RUN_GAPS_STATS.with(|stats| {
                let (a, b) = stats.get();
                stats.set((a + fast_count, b));
            });
        }
        if whole_spans + whole_mismatch > 0 {
            PAINT_CULL_SUBTREE_STATS.with(|stats| {
                let (a, b, c) = stats.get();
                stats.set((a + whole_spans, b + whole_boxes, c + whole_mismatch));
            });
        }
        if end - start < 8 {
            cull_spans.truncate(spans_before);
            return start;
        }
        self.close_cull_segment(segment, end, root, cull_spans);
        let nodes = start..end;
        self.node_properties[nodes.clone()]
            .copy_from_slice(&previous.node_properties[nodes.clone()]);
        self.self_clip[nodes.clone()].copy_from_slice(&previous.self_clip[nodes.clone()]);
        self.z_order[nodes.clone()].copy_from_slice(&previous.z_order[nodes.clone()]);
        self.viewport_attached_by_index[nodes.clone()]
            .copy_from_slice(&previous.viewport_attached_by_index[nodes.clone()]);
        self.sticky_owner[nodes.clone()].copy_from_slice(&previous.sticky_owner[nodes.clone()]);
        let old_memo = &record.memo;
        memo.fixed_owner[nodes.clone()].copy_from_slice(&old_memo.fixed_owner[nodes.clone()]);
        memo.context_owner[nodes.clone()].copy_from_slice(&old_memo.context_owner[nodes.clone()]);
        memo.owns_context[nodes.clone()].copy_from_slice(&old_memo.owns_context[nodes.clone()]);
        memo.stacking[nodes.clone()].copy_from_slice(&old_memo.stacking[nodes.clone()]);
        memo.ancestor_phase[nodes.clone()].copy_from_slice(&old_memo.ancestor_phase[nodes.clone()]);
        let (items, chunks) = (mark.items, mark.chunks);
        let stop = record.marks.get(end).copied().unwrap_or(tail);
        if self.paint_order.levels.len() as u32 == old.levels {
            // Levels line up with last frame's: the keys copy as slices.
            self.paint_order.levels.extend_from_slice(
                &previous.paint_order.levels[old.levels as usize..stop.levels as usize],
            );
            self.paint_order.spans[nodes.clone()]
                .copy_from_slice(&previous.paint_order.spans[nodes.clone()]);
            marks.extend(record.marks[nodes.clone()].iter().map(|at| AppendMark {
                items,
                chunks,
                ..*at
            }));
        } else {
            for index in nodes.clone() {
                let levels = self.paint_order.levels.len() as u32;
                self.paint_order
                    .set(index, None, previous.paint_order.key(index));
                marks.push(AppendMark {
                    items,
                    chunks,
                    levels,
                    ..record.marks[index]
                });
            }
        }
        if paint_cull_rect_mode() == 2 {
            for index in nodes.clone() {
                if self.nodes[index].style.visibility == Visibility::Visible {
                    self.culled_nodes.push(index as u32);
                }
            }
        }
        PAINT_CULL_COUNT.with(|count| count.set(count.get() + culled));
        same[nodes.clone()].fill(true);
        unchanged[nodes].fill(true);
        end
    }

    /// `append_node` for every node, copying the previous build's output for
    /// a node whose own inputs equal last frame's, whose property trees so
    /// far line up with last frame's, and whose ancestors' output is the
    /// same. A copied node's display items are emitted from its new rect.
    fn append_all_reusing(&mut self, previous: Option<&PaintArtifact>) {
        if paint_touched_mode() != 0 {
            self.touched = PAINT_TOUCHED
                .with(|touched| touched.borrow_mut().take())
                .filter(|touched| touched.len == self.nodes.len());
            self.touched_gaps = self.touched.is_some()
                && previous.is_some_and(|previous| {
                    !self.column_fragments.iter().any(|c| !c.is_empty())
                        && !previous.column_fragments.iter().any(|c| !c.is_empty())
                });
        }
        self.append_all_reusing_with(previous);
        self.touched = None;
        self.touched_gaps = false;
    }

    #[inline(always)]
    fn append_all_reusing_with(&mut self, previous: Option<&PaintArtifact>) {
        let len = self.nodes.len();
        let base = previous.and_then(|previous| {
            let record = previous.append_record.as_deref()?;
            (previous.nodes.len() == len
                && record.marks.len() == len
                && previous.cull_band == self.cull_band
                && previous.body_index == self.body_index
                && previous.column_fragments.len() == self.column_fragments.len())
            .then_some((previous, record))
        });
        let verify = base.is_some() && paint_append_reuse_verify();
        let fresh = verify.then(|| {
            let mut fresh = self.clone();
            let mut memo = AppendMemo::new(&fresh.nodes);
            for index in 0..len {
                fresh.append_node(index, &mut memo);
            }
            fresh
        });
        let mut memo = match base {
            Some((_, record)) => {
                AppendMemo::new_reusing(&self.nodes, &record.memo, self.touched.as_ref())
            }
            None => AppendMemo::new(&self.nodes),
        };
        let mut marks = Vec::with_capacity(len);
        // Per node: its inputs (other than the rect) and its output equal
        // last frame's, so a descendant reading them may be copied.
        let mut same = vec![false; len];
        let mut unchanged = vec![false; len];
        let mut runs = Vec::new();
        let mut cull_spans: Vec<CullSpan> = Vec::new();
        let (mut copied, mut appended, mut run_copied) = (0u64, 0u64, 0u64);
        let mut cull_copied = 0u64;
        let splice = artifact_splice_enabled();
        let cull_runs = self.cull_band.is_some() && paint_cull_runs_enabled();
        // Volatile boxes, for the sparse paint diff: a copied run takes the
        // previous build's entries in its range (same inputs, same answer).
        let record_volatile = crate::retained_layers::paint_diff_sparse_mode() != 0;
        let previous_volatile = base.and_then(|(previous, _)| previous.volatile_nodes.as_deref());
        let mut volatile_ok = record_volatile && (base.is_none() || previous_volatile.is_some());
        let mut volatile = Vec::new();
        let mut url_memo = UrlMemo::default();
        let mut span_cursor = CullSpanCursor::default();
        let mut index = 0;
        while index < len {
            if splice && let Some((previous, record)) = base {
                let end = self.copy_aligned_run(
                    index,
                    previous,
                    record,
                    &mut same,
                    &mut unchanged,
                    &mut memo,
                    &mut marks,
                    &mut runs,
                    &mut cull_spans,
                );
                if end > index {
                    if volatile_ok && let Some(old) = previous_volatile {
                        let a = old.partition_point(|&i| (i as usize) < index);
                        let b = old.partition_point(|&i| (i as usize) < end);
                        volatile.extend_from_slice(&old[a..b]);
                    }
                    run_copied += (end - index) as u64;
                    index = end;
                    continue;
                }
            }
            if cull_runs && let Some((previous, record)) = base {
                let end = self.copy_culled_run(
                    index,
                    previous,
                    record,
                    &mut same,
                    &mut unchanged,
                    &mut memo,
                    &mut marks,
                    &mut cull_spans,
                    &mut span_cursor,
                );
                if end > index {
                    if volatile_ok && let Some(old) = previous_volatile {
                        let a = old.partition_point(|&i| (i as usize) < index);
                        let b = old.partition_point(|&i| (i as usize) < end);
                        volatile.extend_from_slice(&old[a..b]);
                    }
                    PAINT_CULL_RUN_NODES
                        .with(|count| count.set(count.get() + (end - index) as u64));
                    copied += (end - index) as u64;
                    cull_copied += (end - index) as u64;
                    index = end;
                    continue;
                }
            }
            let mut mark = self.mark();
            let reused = base.is_some_and(|(previous, record)| {
                self.copy_append_output(index, previous, record, &mark, &same, &mut memo)
            });
            if reused {
                mark.self_scroll = base.unwrap().1.marks[index].self_scroll;
                same[index] = true;
                unchanged[index] = true;
                copied += 1;
            } else {
                mark.self_scroll = self.append_node_with_scroll(index, &mut memo) as u32;
                appended += 1;
                if let Some((previous, record)) = base {
                    unchanged[index] = self.inputs_match(index, previous)
                        && self.output_matches(index, previous, &memo, &record.memo);
                    same[index] = unchanged[index]
                        && self.nodes[index].parent.is_none_or(|parent| same[parent]);
                }
            }
            if volatile_ok && paint_node_volatile(&self.nodes[index], &mut url_memo) {
                volatile.push(index as u32);
            }
            marks.push(mark);
            index += 1;
        }
        volatile_ok &= record_volatile;
        self.volatile_nodes = volatile_ok.then_some(volatile);
        copied += run_copied;
        ARTIFACT_SPLICE_RUN_NODES.with(|count| count.set(count.get() + run_copied));
        self.paint_classes = std::sync::OnceLock::new();
        APPEND_REUSE_STATS.with(|stats| {
            let (c, a, m) = stats.get();
            stats.set((c + copied, a + appended, m));
        });
        if verify {
            eprintln!(
                "[append-reuse] copied {copied} of {len} ({run_copied} in aligned runs, {cull_copied} in culled runs; whole culled spans so far {:?})",
                PAINT_CULL_SUBTREE_STATS.with(std::cell::Cell::get)
            );
        }
        if let Some(fresh) = fresh
            && !self.append_output_eq(&fresh)
        {
            eprintln!(
                "[append-reuse-verify] mismatch: {len} nodes, {copied} copied; using the full build"
            );
            APPEND_REUSE_STATS.with(|stats| {
                let (c, a, m) = stats.get();
                stats.set((c, a, m + 1));
            });
            // The next frame builds in full and records again.
            *self = fresh;
            self.serial = next_artifact_serial();
            return;
        }
        self.serial = next_artifact_serial();
        let base = base.map_or(0, |(previous, _)| previous.serial);
        self.append_record = Some(std::sync::Arc::new(AppendRecord {
            marks,
            memo,
            base,
            unchanged,
            runs,
            cull_spans,
        }));
    }

    #[inline]
    fn inputs_match(&self, index: usize, previous: &PaintArtifact) -> bool {
        // A box the layout splice did not touch kept its inputs (Blink:
        // layout marks the objects that need a paint invalidation).
        // Moved boxes kept their inputs too; only `changed` ones compare.
        if let Some(touched) = self.touched.as_ref()
            && !crate::incremental_layout::ranges_contain(&touched.changed, index)
        {
            if paint_touched_mode() == 2 && !self.inputs_match_compared(index, previous) {
                eprintln!("[paint-touched-verify] mismatch: untouched box {index} changed inputs");
                PAINT_TOUCHED_MISMATCHES.with(|count| count.set(count.get() + 1));
                return false;
            }
            return true;
        }
        self.inputs_match_compared(index, previous)
    }

    /// Every ancestor of `at` before the run `start..` has the same output:
    /// a box after `at` whose parent precedes the run has one of them.
    fn gap_parents_same(&self, start: usize, at: usize, same: &[bool]) -> bool {
        let mut parent = self.nodes[at].parent;
        while let Some(index) = parent {
            if index < start && !same[index] {
                return false;
            }
            parent = self.nodes[index].parent;
        }
        true
    }

    /// Verify: the per-box run checks hold over `at..gap`.
    fn gap_matches(
        &self,
        start: usize,
        at: usize,
        gap: usize,
        previous: &PaintArtifact,
        same: &[bool],
    ) -> bool {
        (at..gap).all(|index| {
            self.nodes[index]
                .parent
                .is_none_or(|parent| parent >= start || same[parent])
                && self.rect_by_index[index] == previous.rect_by_index[index]
                && self.inputs_match_compared(index, previous)
        })
    }

    #[inline(always)]
    fn inputs_match_compared(&self, index: usize, previous: &PaintArtifact) -> bool {
        let (node, old) = (&self.nodes[index], &previous.nodes[index]);
        node.parent == old.parent
            && node.sticky_counter_signal == old.sticky_counter_signal
            && self.logical_paint_ordinals.get(index) == previous.logical_paint_ordinals.get(index)
            && node.kind == old.kind
            && node.style == old.style
    }

    fn output_matches(
        &self,
        index: usize,
        previous: &PaintArtifact,
        memo: &AppendMemo,
        old: &AppendMemo,
    ) -> bool {
        self.node_properties[index] == previous.node_properties[index]
            && self.self_clip[index] == previous.self_clip[index]
            && self.z_order[index] == previous.z_order[index]
            && self.viewport_attached_by_index[index] == previous.viewport_attached_by_index[index]
            && self.sticky_owner[index] == previous.sticky_owner[index]
            && self.paint_order.key(index) == previous.paint_order.key(index)
            && memo.fixed_owner[index] == old.fixed_owner[index]
            && memo.context_owner[index] == old.context_owner[index]
            && memo.owns_context[index] == old.owns_context[index]
            && memo.stacking[index] == old.stacking[index]
            && memo.ancestor_phase[index] == old.ancestor_phase[index]
    }

    /// Copies `index`'s `append_node` output from `previous` when that is
    /// exactly what `append_node` would produce now.
    fn copy_append_output(
        &mut self,
        index: usize,
        previous: &PaintArtifact,
        record: &AppendRecord,
        mark: &AppendMark,
        same: &[bool],
        memo: &mut AppendMemo,
    ) -> bool {
        let old_mark = record.marks[index];
        let next = record.marks.get(index + 1).copied().unwrap_or(AppendMark {
            transforms: previous.properties.transforms.len() as u32,
            clips: previous.properties.clips.len() as u32,
            effects: previous.properties.effects.len() as u32,
            scrolls: previous.properties.scrolls.len() as u32,
            self_scroll: 0,
            items: previous.display_items.len() as u32,
            chunks: previous.chunks.len() as u32,
            levels: previous.paint_order.levels.len() as u32,
        });
        // Property node ids assigned so far line up with last frame's.
        if (mark.transforms, mark.clips, mark.effects, mark.scrolls)
            != (
                old_mark.transforms,
                old_mark.clips,
                old_mark.effects,
                old_mark.scrolls,
            )
        {
            return false;
        }
        if let Some(parent) = self.nodes[index].parent
            && !same[parent]
        {
            return false;
        }
        // Overflow, `clip` and fragment clip nodes and scroll nodes carry the
        // rect; any other output is rect-independent. A node that pushed
        // none of them may move.
        let geometry_free = next.clips == old_mark.clips && next.scrolls == old_mark.scrolls;
        let rect = self.rect_by_index[index];
        let old_rect = previous.rect_by_index[index];
        let rect_ok = rect == old_rect
            || (geometry_free
                && match (rect, old_rect) {
                    (Some(rect), Some(old)) => {
                        rect.x == old.x && rect.width == old.width && rect.height == old.height
                    }
                    _ => false,
                });
        let no_columns = self.column_fragments.get(index).is_none_or(Vec::is_empty)
            && previous
                .column_fragments
                .get(index)
                .is_none_or(Vec::is_empty);
        if !rect_ok || !no_columns || !self.inputs_match(index, previous) {
            return false;
        }
        let range = |start: u32, end: u32| start as usize..end as usize;
        if next.transforms != old_mark.transforms {
            self.properties.transforms.extend_from_slice(
                &previous.properties.transforms[range(old_mark.transforms, next.transforms)],
            );
        }
        if next.clips != old_mark.clips {
            self.properties
                .clips
                .extend_from_slice(&previous.properties.clips[range(old_mark.clips, next.clips)]);
        }
        if next.effects != old_mark.effects {
            self.properties.effects.extend_from_slice(
                &previous.properties.effects[range(old_mark.effects, next.effects)],
            );
        }
        if next.scrolls != old_mark.scrolls {
            self.properties.scrolls.extend_from_slice(
                &previous.properties.scrolls[range(old_mark.scrolls, next.scrolls)],
            );
        }
        self.node_properties[index] = previous.node_properties[index];
        self.self_clip[index] = previous.self_clip[index];
        self.z_order[index] = previous.z_order[index];
        self.viewport_attached_by_index[index] = previous.viewport_attached_by_index[index];
        self.sticky_owner[index] = previous.sticky_owner[index];
        self.paint_order
            .set(index, None, previous.paint_order.key(index));
        let old = &record.memo;
        memo.fixed_owner[index] = old.fixed_owner[index];
        memo.context_owner[index] = old.context_owner[index];
        memo.owns_context[index] = old.owns_context[index];
        memo.stacking[index] = old.stacking[index];
        memo.ancestor_phase[index] = old.ancestor_phase[index];
        let properties = self.node_properties[index];
        self.append_display_items(index, properties, old_mark.self_scroll as usize);
        true
    }

    fn append_output_eq(&self, other: &PaintArtifact) -> bool {
        self.node_properties == other.node_properties
            && self.self_clip == other.self_clip
            && self.z_order == other.z_order
            && self.paint_order == other.paint_order
            && self.viewport_attached_by_index == other.viewport_attached_by_index
            && self.sticky_owner == other.sticky_owner
            && self.properties == other.properties
            && self.display_items == other.display_items
            && self.chunks == other.chunks
    }

    fn build_column_fragments(
        &self,
        index: usize,
        viewport: Option<(f32, f32)>,
        plans: &[Option<crate::layout::ColumnFragmentPlan>],
    ) -> Vec<ColumnFragment> {
        use w3cos_std::style::TextDirection;
        let Some(rect) = self.rect_by_index[index] else {
            return Vec::new();
        };
        if rect.height <= 0.0 || !rect.height.is_finite() {
            return Vec::new();
        }
        let mut ancestor = self.nodes[index].parent;
        let mut fragments = vec![ColumnFragment {
            visual_rect: rect,
            translate_x: 0.0,
            translate_y: 0.0,
            clip_top: f32::NEG_INFINITY,
            clip_bottom: f32::INFINITY,
        }];
        let mut fragmented = false;
        if matches!(
            self.nodes[index].style.position,
            Position::Absolute | Position::Fixed
        ) {
            return Vec::new();
        }
        while let Some(owner) = ancestor {
            let style = &self.nodes[owner].style;
            let Some(principal) = self.rect_by_index[owner] else {
                return Vec::new();
            };
            let padding = style.padding_lengths();
            let left = style.border_left_width.unwrap_or(style.border_width) + padding.left;
            let right = style.border_right_width.unwrap_or(style.border_width) + padding.right;
            let top = style.border_top_width.unwrap_or(style.border_width) + padding.top;
            let content_width = (principal.width - left - right).max(0.0);
            let (vw, vh) = viewport.unwrap_or((principal.width, principal.height));
            if let Some(column_width) =
                crate::layout::used_column_width(style, content_width, vw, vh)
            {
                let Some(plan) = plans[owner].as_ref() else {
                    ancestor = self.nodes[owner].parent;
                    continue;
                };
                let height = plan.height;
                if !height.is_finite() || height <= 0.0 {
                    return Vec::new();
                }
                let gap = style.column_gap.unwrap_or(style.font_size).max(0.0);
                let stride = column_width + gap;
                let origin_y = principal.y + top;
                let rtl = style.direction == TextDirection::Rtl;
                let first_offset = if rtl {
                    content_width - column_width
                } else {
                    0.0
                };
                let mut projected = Vec::new();
                for fragment in &fragments {
                    for (column, band) in plan.bands.iter().enumerate() {
                        let rect = fragment.visual_rect;
                        let start = rect.y.max(band.start);
                        // A deferred complete line leaves a blank tail in its
                        // block decoration. Do not paint clipped glyphs there,
                        // but retain the box fragment up to fragmentainer capacity.
                        let band_end = if plan.continues_in_parent
                            && !matches!(self.nodes[index].kind, ComponentKind::Text { .. })
                        {
                            band.start + height
                        } else {
                            band.end
                        };
                        let end = (rect.y + rect.height)
                            .min(band_end)
                            .min(band.start + height);
                        if end <= start {
                            continue;
                        }
                        let (column, set) = if plan.continues_in_parent {
                            (column % plan.columns_per_set, column / plan.columns_per_set)
                        } else {
                            (column, 0)
                        };
                        let set_origin = origin_y + set as f32 * height;
                        let translate_x =
                            first_offset + column as f32 * stride * if rtl { -1.0 } else { 1.0 };
                        let translate_y = set_origin - band.start;
                        projected.push(ColumnFragment {
                            visual_rect: LayoutRect {
                                x: rect.x + translate_x,
                                y: start + translate_y,
                                width: rect.width,
                                height: end - start,
                            },
                            translate_x: fragment.translate_x + translate_x,
                            translate_y: fragment.translate_y + translate_y,
                            clip_top: (fragment.clip_top + translate_y).max(set_origin),
                            clip_bottom: (fragment.clip_bottom + translate_y)
                                .min(set_origin + height),
                        });
                    }
                }
                fragments = projected;
                fragmented = true;
            }
            if matches!(style.position, Position::Absolute | Position::Fixed) {
                break;
            }
            ancestor = self.nodes[owner].parent;
        }
        if fragmented { fragments } else { Vec::new() }
    }

    #[inline]
    fn append_node(&mut self, index: usize, memo: &mut AppendMemo) {
        self.append_node_with_scroll(index, memo);
    }

    /// `append_node`, returning the scroll node the box's own chunks use.
    fn append_node_with_scroll(&mut self, index: usize, memo: &mut AppendMemo) -> usize {
        let node = &self.nodes[index];
        debug_assert!(node.parent.is_none_or(|parent| parent < index));
        let mut inherited = node
            .parent
            .and_then(|parent| self.node_properties.get(parent).copied())
            .unwrap_or_default();
        if node.style.display == Display::TableCaption
            && let Some(parent) = node.parent
            && matches!(
                self.nodes[parent].style.display,
                Display::Table | Display::InlineTable
            )
            && matches!(
                self.nodes[parent].style.resolved_overflow_x(),
                Overflow::Hidden
            )
        {
            inherited.clip = self.self_clip[parent];
        }
        if matches!(node.style.position, Position::Absolute | Position::Fixed) {
            // Overflow clips between an out-of-flow box and its containing
            // block do not clip that box. Inherit the clip chain from the
            // nearest positioned containing-block ancestor, not blindly from
            // the DOM parent; fixed boxes use the viewport chain.
            let positioned_ancestor = if matches!(node.style.position, Position::Fixed) {
                None
            } else {
                let mut ancestor = node.parent;
                let mut owner = None;
                while let Some(index) = ancestor {
                    if !matches!(self.nodes[index].style.position, Position::Static) {
                        owner = Some(index);
                        break;
                    }
                    ancestor = self.nodes[index].parent;
                }
                owner
            };
            let containing_properties = positioned_ancestor
                .and_then(|owner| self.node_properties.get(owner))
                .copied()
                .unwrap_or_default();
            inherited.clip = containing_properties.clip;
            // An out-of-flow box escapes intermediate overflow scrollports
            // together with their clips. Keep both property chains rooted at
            // its actual positioned containing block.
            inherited.scroll = containing_properties.scroll;
        }
        let inherited_z = node
            .parent
            .and_then(|parent| {
                if self.nodes[parent].parent.is_none() {
                    Some(0)
                } else {
                    self.z_order.get(parent).copied()
                }
            })
            .unwrap_or_default();
        // The root box establishes the root stacking context. Its own
        // background and border paint below every descendant, including a
        // positioned descendant with a negative z-index.
        self.z_order[index] = if node.parent.is_none() {
            i32::MIN
        } else if matches!(node.style.position, Position::Fixed) && node.style.z_index == 0 {
            // A fixed auto box participates at stack level zero in the root
            // context. Positioned ancestors with z-index:auto do not create a
            // context, so their synthetic paint rank must not accumulate.
            1
        } else {
            effective_z_order(&node.style, inherited_z)
        };
        // A fixed-positioned box establishes a stacking context even when
        // z-index is auto. Negative descendants therefore paint above the
        // fixed box's own background instead of escaping behind it.
        // Nodes append in preorder, so the nearest fixed auto ancestor is the
        // parent's (or the parent itself); no per-node ancestor walk.
        let fixed_context_floor = node
            .parent
            .and_then(|parent| memo.fixed_owner[parent])
            .and_then(|ancestor| self.z_order.get(ancestor).copied())
            .map(|z| z.saturating_add(1));
        if let Some(floor) = fixed_context_floor {
            self.z_order[index] = self.z_order[index].max(floor);
        }
        memo.fixed_owner[index] =
            if matches!(node.style.position, Position::Fixed) && node.style.z_index == 0 {
                Some(index)
            } else {
                node.parent.and_then(|parent| memo.fixed_owner[parent])
            };
        let stacking = establishes_stacking_context(node);
        memo.stacking[index] = stacking;
        // What the local paint phase walk decides when it reaches this node
        // as an ancestor; descendants read it instead of walking up.
        memo.ancestor_phase[index] = if is_positioned(&node.style) && !stacking {
            ANCESTOR_PHASE_NONE
        } else if node.style.float != w3cos_std::style::Float::None {
            2
        } else if matches!(
            node.style.display,
            Display::InlineBlock | Display::InlineFlex | Display::InlineTable
        ) {
            3
        } else if stacking {
            ANCESTOR_PHASE_NONE
        } else {
            node.parent
                .map_or(ANCESTOR_PHASE_NONE, |parent| memo.ancestor_phase[parent])
        };
        let owns_context = node.parent.is_none()
            || stacking
            || is_positioned(&node.style)
            || memo.split_owner[index];
        memo.owns_context[index] = owns_context;
        memo.context_owner[index] = if owns_context {
            Some(index)
        } else {
            node.parent.and_then(|parent| memo.context_owner[parent])
        };
        let (prefix, tail, tail_len) = self.hierarchical_paint_order(index, memo);
        self.paint_order.set(index, prefix, &tail[..tail_len]);
        self.paint_classes = std::sync::OnceLock::new();
        self.viewport_attached_by_index[index] = node.style.position == Position::Fixed
            || node
                .parent
                .is_some_and(|parent| self.viewport_attached_by_index[parent]);
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
        let overflow_x = node.style.resolved_overflow_x();
        let overflow_y = node.style.resolved_overflow_y();
        // Snapshot the chain the box's own background and border paint under.
        // `overflow` clips the box's contents, not the box: a
        // `border: 10px solid black; overflow: auto` box keeps all ten pixels
        // of its border, and a negative-margin child must not erase the
        // parent's border either. The `clip` property and the inline fragment
        // clip below do apply to the box itself, so each refreshes this value.
        let mut self_clip = properties.clip;
        let mut overflow_clip = None;
        // `hidden`/`scroll`/`auto` on either axis clips both (the other axis
        // computes to a scroll container too); `clip` bounds only its axis.
        let clips_content = |overflow| {
            matches!(
                overflow,
                Overflow::Hidden | Overflow::Scroll | Overflow::Auto
            )
        };
        let scroll_overflow = clips_content(overflow_x) || clips_content(overflow_y);
        // A document element's `clip` and the `clip` of a body that
        // propagates to the viewport apply to the viewport, not to the box.
        let propagates_to_viewport = index == 0
            || (Some(index) == self.body_index
                && node.style.contain == Contain::None
                && self.nodes.first().is_some_and(|root| {
                    root.style.resolved_overflow_x() == Overflow::Visible
                        && root.style.resolved_overflow_y() == Overflow::Visible
                        && !root.style.overflow_clip_x
                        && !root.style.overflow_clip_y
                }));
        // The overflow of the document element (and of a propagating body)
        // belongs to the viewport: no clip at the box (CSS Overflow 3 3.3).
        let scroll_clip = scroll_overflow && !(propagates_to_viewport && self.body_index.is_some());
        let clips_x = (node.style.overflow_clip_x && !propagates_to_viewport) || scroll_clip;
        let clips_y = (node.style.overflow_clip_y && !propagates_to_viewport) || scroll_clip;
        if establishes_overflow_clip(node.style.display) && (clips_x || clips_y) {
            properties.clip = self.properties.clips.len();
            overflow_clip = Some(properties.clip);
            self.properties.clips.push(ClipNode {
                parent: inherited.clip,
                // The clipping region is the padding box, not the border box
                // (CSS 2.1 11.1.1). Without the border inset a border lets
                // exactly its own width of overflowing content show through:
                // `css/CSS2/ui/overflow-applies-to-009.xht` leaks the 5 px of
                // `border: 5px solid transparent`, and a 20 px border leaks 20.
                rect: self.rect_by_index[index].map(|rect| {
                    // Caption insets delimit table background/border paint,
                    // not the wrapper's descendant overflow clip. Chrome clips
                    // grid descendants against the wrapper padding box while
                    // captions retain their separate inherited clip chain.
                    let mut clip = overflow_clip_rect(&node.style, rect);
                    // `overflow-clip-margin` moves the edge of an
                    // `overflow: clip` axis outward from its reference box.
                    let (visual_box, margin) = node.style.overflow_clip_margin;
                    if !scroll_clip
                        && (node.style.overflow_clip_x || node.style.overflow_clip_y)
                        && (visual_box != 0 || margin > 0.0)
                    {
                        let base = match visual_box {
                            2 => rect,
                            1 => {
                                let padding = node.style.padding_lengths();
                                LayoutRect {
                                    x: clip.x + padding.left,
                                    y: clip.y + padding.top,
                                    width: (clip.width - padding.left - padding.right).max(0.0),
                                    height: (clip.height - padding.top - padding.bottom).max(0.0),
                                }
                            }
                            _ => clip,
                        };
                        if node.style.overflow_clip_x {
                            clip.x = base.x - margin;
                            clip.width = base.width + 2.0 * margin;
                        }
                        if node.style.overflow_clip_y {
                            clip.y = base.y - margin;
                            clip.height = base.height + 2.0 * margin;
                        }
                    }
                    // A single-axis `overflow: clip` leaves the other axis
                    // unbounded.
                    if !clips_x {
                        clip.x = -1.0e7;
                        clip.width = 2.0e7;
                    }
                    if !clips_y {
                        clip.y = -1.0e7;
                        clip.height = 2.0e7;
                    }
                    clip
                }),
            });
        }
        if let Some(rect) = self.rect_by_index[index]
            && let Some(css_clip) = css2_clip_rect(&node.style, rect)
        {
            let parent = properties.clip;
            properties.clip = self.properties.clips.len();
            self.properties.clips.push(ClipNode {
                parent,
                rect: Some(css_clip),
            });
            self_clip = properties.clip;
        }
        if let Some(rect) = self.rect_by_index[index]
            && let Some(fragment_clip) = inline_fragment_clip_rect(&node.kind, &node.style, rect)
        {
            let parent = properties.clip;
            properties.clip = self.properties.clips.len();
            self.properties.clips.push(ClipNode {
                parent,
                rect: Some(fragment_clip),
            });
            self_clip = properties.clip;
        }
        if node.style.opacity < 0.999
            || node.style.filter.is_some()
            || node.style.will_change.promotes_layer()
        {
            properties.effect = self.properties.effects.len();
            self.properties.effects.push(EffectNode {
                parent: inherited.effect,
                opacity: node.style.opacity,
                filter: node.style.filter.clone(),
                isolates_surface: node.style.will_change.promotes_layer(),
            });
        }
        let self_scroll = properties.scroll;
        // `overflow: hidden` is also a scroll container: it has no user
        // scrollbar, but CSSOM may move its scroll offset programmatically.
        if establishes_overflow_clip(node.style.display)
            && (matches!(
                overflow_x,
                Overflow::Hidden | Overflow::Scroll | Overflow::Auto
            ) || matches!(
                overflow_y,
                Overflow::Hidden | Overflow::Scroll | Overflow::Auto
            ))
        {
            properties.scroll = self.properties.scrolls.len();
            self.properties.scrolls.push(ScrollNode {
                parent: inherited.scroll,
                host_index: Some(index),
                scrollport: self.rect_by_index[index],
                clip: overflow_clip,
            });
        }
        self.node_properties[index] = properties;
        self.self_clip[index] = self_clip;
        self.append_display_items(index, properties, self_scroll);
        self_scroll
    }

    /// The display items and chunks of `index`'s own box.
    #[inline(always)]
    fn append_display_items(
        &mut self,
        index: usize,
        properties: PaintProperties,
        self_scroll: PropertyNodeId,
    ) {
        let node = &self.nodes[index];
        if node.style.visibility != Visibility::Visible {
            return;
        }

        let Some(bounds) = self.rect_by_index[index] else {
            return;
        };
        let bounds = if node.style.border_collapse && node.style.display == Display::TableCell {
            let (left, right) = paint_inline_border_widths(&node.style);
            LayoutRect {
                x: bounds.x - left,
                width: bounds.width + left + right,
                ..bounds
            }
        } else {
            bounds
        };
        let fragments = self
            .column_fragments
            .get(index)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if let Some((top, bottom)) = self.cull_band
            && fragments.is_empty()
            && (bounds.y + bounds.height < top || bounds.y > bottom)
            && self.cullable(index, properties, self_scroll)
        {
            // Blink's CullRect: root content outside the viewport expanded
            // by 4000px is not painted; the band moves only when the scroll
            // gets near its edge, and a box entering it paints as new.
            PAINT_CULL_COUNT.with(|count| count.set(count.get() + 1));
            if paint_cull_rect_mode() == 2 {
                self.culled_nodes.push(index as u32);
            }
            return;
        }
        let physical_bounds = fragments
            .iter()
            .map(|fragment| fragment.visual_rect)
            .chain(fragments.is_empty().then_some(bounds));
        for bounds in physical_bounds {
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
                properties: PaintProperties {
                    scroll: self_scroll,
                    ..properties
                },
                z_order: self.z_order[index],
            });
        }
    }

    /// The cull band may drop this box's items: it paints under the root's
    /// property nodes (no transform, clip, effect or scroller of its own or
    /// from an ancestor) and scrolls with the document (not fixed, sticky or
    /// viewport-attached).
    fn cullable(
        &self,
        index: usize,
        properties: PaintProperties,
        self_scroll: PropertyNodeId,
    ) -> bool {
        let root = self.node_properties.first().copied().unwrap_or_default();
        let node = &self.nodes[index];
        index != 0
            && properties == root
            && self_scroll == root.scroll
            && !matches!(node.style.position, Position::Fixed | Position::Sticky)
            && !self
                .viewport_attached_by_index
                .get(index)
                .copied()
                .unwrap_or(true)
            && self.sticky_owner.get(index).copied().flatten().is_none()
    }

    /// `=verify`: boxes the cull band dropped whose painted rect (document
    /// rect minus the viewport scroll, grown by a generous ink outset)
    /// reaches the viewport `width` x `height`. Must be 0.
    pub(crate) fn culled_in_viewport(&self, width: f32, height: f32) -> usize {
        self.culled_nodes
            .iter()
            .filter(|&&index| {
                let index = index as usize;
                let Some(rect) = self.rect_by_index.get(index).copied().flatten() else {
                    return false;
                };
                let (_, sy) = self.viewport_scroll_for(index);
                let outset = paint_cull_ink_outset(&self.nodes[index].style);
                let top = rect.y - sy - outset;
                let bottom = rect.y + rect.height - sy + outset;
                bottom > 0.0 && top < height && width > 0.0
            })
            .count()
    }

    /// The band covers the viewport at `scroll_y` with `margin` to spare.
    pub(crate) fn cull_band_covers(&self, scroll_y: f32, height: f32, margin: f32) -> bool {
        self.cull_band.is_none_or(|(top, bottom)| {
            top <= scroll_y - margin && bottom >= scroll_y + height + margin
        })
    }

    /// A node's paint-order key: a prefix of its paint context owner's key
    /// (owner, length) followed by `tail[..len]`.
    fn hierarchical_paint_order(
        &self,
        index: usize,
        memo: &AppendMemo,
    ) -> (Option<(usize, usize)>, [PaintOrderLevel; 2], usize) {
        let node = &self.nodes[index];
        let background = (0, i32::MIN, index);
        if node.parent.is_none() {
            return (None, [background, background], 1);
        }

        // Auto positioned boxes group their normal contents, but positioned
        // descendants participate in the nearest real stacking context.
        let mut parent = node.parent;
        if is_positioned(&node.style) {
            while let Some(current) = parent {
                if self.nodes[current].parent.is_none() || memo.stacking[current] {
                    break;
                }
                parent = self.nodes[current].parent;
            }
        }
        let prefix = parent.and_then(|parent| self.paint_context_prefix(parent, memo));
        let local = self.local_paint_order_level(index, memo);
        if memo.owns_context[index] {
            // A context's own background is the first item inside that
            // context. Descendant keys extend the prefix before adding their
            // local CSS2 paint phase.
            (prefix, [local, background], 2)
        } else {
            (prefix, [local, background], 1)
        }
    }

    /// The key prefix a child of `index` extends: the key of the nearest
    /// ancestor-or-self that owns a paint context, without that owner's own
    /// background level, as the owner and the prefix length.
    fn paint_context_prefix(&self, index: usize, memo: &AppendMemo) -> Option<(usize, usize)> {
        let owner = (*memo.context_owner.get(index)?)?;
        let owner_key = self.paint_order.get(owner)?;
        let len = owner_key.len() - usize::from(memo.owns_context[owner] && !owner_key.is_empty());
        Some((owner, len))
    }

    fn local_paint_order_level(&self, index: usize, memo: &AppendMemo) -> PaintOrderLevel {
        let node = &self.nodes[index];
        let split_owner = memo.split_owner[index];
        let ordinal = self
            .logical_paint_ordinals
            .get(index)
            .copied()
            .unwrap_or(index);
        if split_owner {
            // A lowered static float can have a zero visual offset. It still
            // belongs to its positioned inline owner, without changing flow.
            return (4, 0, ordinal);
        }
        if is_positioned(&node.style) {
            return match node.style.z_index.cmp(&0) {
                std::cmp::Ordering::Less => (0, node.style.z_index, ordinal),
                std::cmp::Ordering::Equal => (4, 0, ordinal),
                std::cmp::Ordering::Greater => (5, node.style.z_index, ordinal),
            };
        }
        if node.parent.is_some() && memo.stacking[index] {
            // Transform/opacity contexts participate at stack level zero,
            // after normal inline ink, even when their box is a static float.
            // A non-positioned box does not gain authored z-index semantics.
            return (4, 0, ordinal);
        }

        let collapsed_table_part_border = node.style.border_collapse
            && node.style.background.a == 0
            && matches!(
                node.style.display,
                Display::TableRowGroup
                    | Display::TableHeaderGroup
                    | Display::TableFooterGroup
                    | Display::TableRow
                    | Display::TableColumnGroup
                    | Display::TableColumn
            )
            && (node.style.border_width > 0.0
                || node.style.border_top_width.is_some_and(|width| width > 0.0)
                || node
                    .style
                    .border_right_width
                    .is_some_and(|width| width > 0.0)
                || node
                    .style
                    .border_bottom_width
                    .is_some_and(|width| width > 0.0)
                || node
                    .style
                    .border_left_width
                    .is_some_and(|width| width > 0.0));
        if collapsed_table_part_border {
            // Collapsed table borders paint over cell contents. Transparent
            // table-part backgrounds can therefore use a late display item
            // without disturbing the table background-layer ordering.
            return (4, 0, ordinal);
        }

        // The walk from this node up to its nearest positioned, floated,
        // atomic inline or stacking-context ancestor; every ancestor's
        // outcome is remembered when it is appended (preorder).
        if node.style.float != w3cos_std::style::Float::None {
            return (2, 0, ordinal);
        }
        match node
            .parent
            .map_or(ANCESTOR_PHASE_NONE, |parent| memo.ancestor_phase[parent])
        {
            ANCESTOR_PHASE_NONE => {}
            phase => return (phase, 0, ordinal),
        }

        let split_inline_edge = node
            .style
            .custom_properties
            .as_ref()
            .is_some_and(|properties| {
                properties.contains_key("--w3cos-internal-split-inline-edge")
            });
        let normal_inline_border = node.style.display == Display::Inline
            && node.style.line_height_is_normal
            && matches!(node.kind, ComponentKind::Row | ComponentKind::Box)
            && (node.style.border_width > 0.0
                || node.style.border_top_width.is_some_and(|width| width > 0.0)
                || node
                    .style
                    .border_right_width
                    .is_some_and(|width| width > 0.0)
                || node
                    .style
                    .border_bottom_width
                    .is_some_and(|width| width > 0.0)
                || node
                    .style
                    .border_left_width
                    .is_some_and(|width| width > 0.0));
        let phase = if split_inline_edge || normal_inline_border {
            // Inline decoration and in-flow foreground share tree order:
            // a later fragment covers preceding block glyph overflow, while
            // remaining behind its own/following text. Moving every border
            // before all foreground loses that ordering across split lines.
            3
        } else if unadorned_block_text(node)
            || matches!(
                node.style.display,
                Display::Inline | Display::InlineBlock | Display::InlineFlex | Display::InlineTable
            )
        {
            3
        } else {
            1
        };
        (phase, 0, ordinal)
    }
}

fn unadorned_block_text(node: &PaintNode) -> bool {
    matches!(node.kind, ComponentKind::Text { .. })
        && node.style.display == Display::Block
        && node.style.background.a == 0
        && node
            .style
            .background_image
            .as_deref()
            .is_none_or(|image| image == "none")
        && node.style.border_width == 0.0
        && [
            node.style.border_top_width,
            node.style.border_right_width,
            node.style.border_bottom_width,
            node.style.border_left_width,
        ]
        .into_iter()
        .all(|width| width.unwrap_or(0.0) == 0.0)
        && !node.style.has_box_shadow()
        && node.style.backdrop_blur.is_none()
        && node.style.text_shadows.is_empty()
}

fn is_positioned(style: &Style) -> bool {
    matches!(
        style.position,
        Position::Relative | Position::Absolute | Position::Fixed | Position::Sticky
    )
}

fn establishes_stacking_context(node: &PaintNode) -> bool {
    // The custom property lookup is a string hash: only positioned boxes
    // without a nonzero z-index need it.
    let specifies_z_index = || {
        node.style.z_index != 0
            || node
                .style
                .custom_properties
                .as_ref()
                .is_some_and(|properties| {
                    properties.contains_key("--w3cos-internal-z-index-specified")
                })
    };
    node.parent.is_none()
        || node.style.opacity < 1.0
        || !node.style.transform.is_identity()
        || (is_positioned(&node.style)
            && (matches!(node.style.position, Position::Fixed | Position::Sticky)
                || specifies_z_index()))
}

/// Indices of `classes` ordered by class, equal classes in index order.
thread_local! {
    /// Per box: whether this frame's layout splice touched it (see
    /// `incremental_layout::PaintChanges`), for the artifact build only.
    static PAINT_TOUCHED: std::cell::RefCell<Option<crate::incremental_layout::PaintChanges>> =
        const { std::cell::RefCell::new(None) };
    static PAINT_TOUCHED_MISMATCHES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
thread_local! {
    pub(crate) static PAINT_TOUCHED_OVERRIDE: std::cell::Cell<Option<u8>> =
        const { std::cell::Cell::new(None) };
}

/// `W3COS_PAINT_TOUCHED` (on by default, `=0` turns it off): the reusing
/// artifact build trusts the layout splice's change set and skips the input
/// comparison for boxes it did not touch. `=verify` also compares and
/// reports (and honours) a difference.
pub(crate) fn paint_touched_mode() -> u8 {
    #[cfg(test)]
    if let Some(mode) = PAINT_TOUCHED_OVERRIDE.with(std::cell::Cell::get) {
        return mode;
    }
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(
        || match std::env::var("W3COS_PAINT_TOUCHED").ok().as_deref() {
            Some("0" | "false") => 0,
            Some("verify") => 2,
            _ => 1,
        },
    )
}

thread_local! {
    /// (aligned builds, trim decisions copied, verify mismatches).
    static PAINT_RANGE_STATS: std::cell::Cell<(u64, u64, u64)> =
        const { std::cell::Cell::new((0, 0, 0)) };
}

#[cfg(test)]
thread_local! {
    pub(crate) static PAINT_RANGE_REBUILD_OVERRIDE: std::cell::Cell<Option<u8>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn paint_range_stats() -> (u64, u64, u64) {
    PAINT_RANGE_STATS.with(std::cell::Cell::get)
}

/// `W3COS_PAINT_RANGE_REBUILD=1|verify` (off by default): the reusing
/// artifact build re-runs the line-start white-space trim and the logical
/// ordinal scan only for the boxes this frame's layout splice touched,
/// taking the previous build's decisions for the rest. `verify` also runs
/// the full passes, reports a difference and uses the full result.
pub(crate) fn paint_range_rebuild_mode() -> u8 {
    #[cfg(test)]
    if let Some(mode) = PAINT_RANGE_REBUILD_OVERRIDE.with(std::cell::Cell::get) {
        return mode;
    }
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(
        || match std::env::var("W3COS_PAINT_RANGE_REBUILD").ok().as_deref() {
            Some("1" | "true") => 1,
            Some("verify") => 2,
            _ => 0,
        },
    )
}

const FEATURE_TRANSLATE_PERCENT: u8 = 1;
const FEATURE_VIDEO_CONTROLS: u8 = 2;
const FEATURE_TABLE: u8 = 4;
/// A multicol box (`column-count` or a non-auto `column-width`).
const FEATURE_COLUMNS: u8 = 32;

/// `W3COS_PAINT_CAPTION_COLUMN_PRESENCE=1|verify` (default off; needs
/// `W3COS_PAINT_FEATURE_SKIP`): when the kept feature superset has no table
/// part, the build skips the table caption inset pass (its own full scan
/// for a caption), and without a multicol box it skips the multicol scan.
/// `verify` scans anyway and runs the pass on anything the superset missed.
pub(crate) fn paint_caption_column_presence_mode() -> u8 {
    #[cfg(test)]
    if let Some(mode) = PAINT_CAPTION_COLUMN_PRESENCE_OVERRIDE.with(std::cell::Cell::get) {
        return mode;
    }
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| {
        match std::env::var("W3COS_PAINT_CAPTION_COLUMN_PRESENCE")
            .ok()
            .as_deref()
        {
            Some("1") => 1,
            Some("verify") => 2,
            _ => 0,
        }
    })
}

#[cfg(test)]
thread_local! {
    pub(crate) static PAINT_CAPTION_COLUMN_PRESENCE_OVERRIDE: std::cell::Cell<Option<u8>> =
        const { std::cell::Cell::new(None) };
}

thread_local! {
    /// (caption passes skipped, multicol scans skipped, verify mismatches).
    pub(crate) static PAINT_CAPTION_COLUMN_PRESENCE_STATS: std::cell::Cell<(u64, u64, u64)> =
        const { std::cell::Cell::new((0, 0, 0)) };
}

fn caption_column_presence_count(field: usize) {
    PAINT_CAPTION_COLUMN_PRESENCE_STATS.with(|stats| {
        let mut value = stats.get();
        match field {
            0 => value.0 += 1,
            1 => value.1 += 1,
            _ => value.2 += 1,
        }
        stats.set(value);
    });
}

fn caption_column_presence_mismatch(what: &str) {
    eprintln!("[paint-caption-column-presence-verify] mismatch: {what} the superset missed");
    caption_column_presence_count(2);
}
/// A box carries a `resize` value (see [`PaintArtifact::resize_absent`]).
const FEATURE_RESIZE: u8 = 8;
/// A box carries an inline background group (see
/// [`PaintArtifact::inline_background_groups_absent`]).
const FEATURE_INLINE_BACKGROUND_GROUP: u8 = 16;

/// `W3COS_PAINT_FEATURE_SKIP=1|verify` (off by default): the reusing build
/// skips its whole-tree passes for percentage translates, media controls
/// and tables when no box can carry them: the previous build's set plus
/// the boxes this frame's layout splice touched (every other box kept its
/// style). `verify` also runs each skipped pass and compares.
pub(crate) fn paint_feature_skip_mode() -> u8 {
    #[cfg(test)]
    if let Some(mode) = PAINT_FEATURE_SKIP_OVERRIDE.with(std::cell::Cell::get) {
        return mode;
    }
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(
        || match std::env::var("W3COS_PAINT_FEATURE_SKIP").ok().as_deref() {
            Some("1") => 1,
            Some("verify") => 2,
            _ => 0,
        },
    )
}

#[cfg(test)]
thread_local! {
    pub(crate) static PAINT_FEATURE_SKIP_OVERRIDE: std::cell::Cell<Option<u8>> =
        const { std::cell::Cell::new(None) };
}

thread_local! {
    /// (passes skipped, verify mismatches).
    static PAINT_FEATURE_SKIP_STATS: std::cell::Cell<(u64, u64)> =
        const { std::cell::Cell::new((0, 0)) };
}

#[cfg(test)]
pub(crate) fn paint_feature_skip_stats() -> (u64, u64) {
    PAINT_FEATURE_SKIP_STATS.with(std::cell::Cell::get)
}

/// The rare-feature bits of one box.
fn node_features(node: &PaintNode) -> u8 {
    let style = &node.style;
    let mut bits = 0;
    if matches!(style.display, Display::Table | Display::InlineTable)
        || table_passes::is_table_part(style.display)
    {
        bits |= FEATURE_TABLE;
    }
    if style.column_count.is_some()
        || !matches!(style.column_width, w3cos_std::style::Dimension::Auto)
    {
        bits |= FEATURE_COLUMNS;
    }
    if let Some(properties) = style.custom_properties.as_ref() {
        const TRANSLATE: &str = "--w3cos-internal-translate-percent";
        const VIDEO: &str = "--w3cos-internal-video-controls";
        if properties.may_contain(TRANSLATE) && properties.contains_key(TRANSLATE) {
            bits |= FEATURE_TRANSLATE_PERCENT;
        }
        if properties.may_contain(VIDEO) && properties.contains_key(VIDEO) {
            bits |= FEATURE_VIDEO_CONTROLS;
        }
        const RESIZE: &str = w3cos_dom::css_style::RESIZE_PROPERTY;
        if properties.may_contain(RESIZE) && properties.contains_key(RESIZE) {
            bits |= FEATURE_RESIZE;
        }
        const GROUP: &str = "--w3cos-internal-inline-background-group";
        if properties.may_contain(GROUP) && properties.contains_key(GROUP) {
            bits |= FEATURE_INLINE_BACKGROUND_GROUP;
        }
    }
    bits
}

/// The features some box of `nodes` may carry: the previous build's set
/// and this frame's touched boxes when the splice's set covers the same
/// boxes, else every box.
fn feature_presence(nodes: &[PaintNode], previous: Option<&PaintArtifact>) -> u8 {
    let kept = previous.and_then(|previous| previous.feature_presence);
    let incremental = kept.and_then(|kept| {
        PAINT_TOUCHED.with(|touched| {
            let touched = touched.borrow();
            let changes = touched
                .as_ref()
                .filter(|changes| changes.len == nodes.len())?;
            let mut bits = kept;
            for &(start, end) in &changes.changed {
                for node in &nodes[start as usize..(end as usize).min(nodes.len())] {
                    bits |= node_features(node);
                }
            }
            Some(bits)
        })
    });
    incremental.unwrap_or_else(|| {
        nodes
            .iter()
            .fold(0, |bits, node| bits | node_features(node))
    })
}

/// Run `pass` on `nodes` unless `present` says no box needs it; `verify`
/// runs it on a copy and compares.
fn run_feature_pass<T>(
    mode: u8,
    present: bool,
    nodes: &mut T,
    same: impl Fn(&T, &T) -> bool,
    copy: impl Fn(&T) -> T,
    pass: impl Fn(&mut T),
    name: &str,
) {
    if mode == 0 || present {
        pass(nodes);
        return;
    }
    let mut stats = PAINT_FEATURE_SKIP_STATS.with(std::cell::Cell::get);
    stats.0 += 1;
    if mode == 2 {
        let mut fresh = copy(nodes);
        pass(&mut fresh);
        if !same(&fresh, nodes) {
            stats.1 += 1;
            eprintln!("[paint-feature-skip-verify] {name} pass was needed");
            *nodes = fresh;
        }
    }
    PAINT_FEATURE_SKIP_STATS.with(|cell| cell.set(stats));
}

fn same_paint_nodes(a: &[PaintNode], b: &[PaintNode]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            x.kind == y.kind && x.parent == y.parent && x.style.as_style() == y.style.as_style()
        })
}

/// Install (or clear) the touched set for the next artifact build.
pub(crate) fn set_paint_touched(touched: Option<crate::incremental_layout::PaintChanges>) {
    PAINT_TOUCHED.with(|slot| *slot.borrow_mut() = touched);
}

/// Untouched boxes whose inputs differed (verify mode) on this thread.
#[allow(dead_code)]
pub(crate) fn paint_touched_mismatches() -> u32 {
    PAINT_TOUCHED_MISMATCHES.with(std::cell::Cell::get)
}

/// A box whose paint reads resources the artifact does not describe
/// (decoded `url()` images, canvas bitmaps, SVG documents, form control
/// values): it repaints every frame. The paint diff's rule.
pub(crate) fn paint_node_volatile(node: &PaintNode, memo: &mut UrlMemo) -> bool {
    matches!(
        node.kind,
        ComponentKind::Image { .. }
            | ComponentKind::Canvas { .. }
            | ComponentKind::SvgDocument { .. }
            | ComponentKind::TextInput { .. }
    ) || node
        .style
        .background_image
        .as_deref()
        .is_some_and(|value| value.contains("url("))
        || node
            .style
            .custom_properties
            .as_ref()
            .is_some_and(|properties| memo.has_url(properties))
}

/// Inherited custom properties share one map across many boxes: scan each
/// map for `url(` once (the paint diff's memo).
#[derive(Default)]
pub(crate) struct UrlMemo {
    last: Option<(usize, bool)>,
    known: w3cos_std::fxhash::FxHashMap<usize, bool>,
}

impl UrlMemo {
    fn has_url(&mut self, properties: &w3cos_std::style::CustomProperties) -> bool {
        let key = properties.identity();
        if let Some((last, found)) = self.last
            && last == key
        {
            return found;
        }
        let found = *self
            .known
            .entry(key)
            .or_insert_with(|| properties.values().any(|value| value.contains("url(")));
        self.last = Some((key, found));
        found
    }
}

pub(crate) fn stable_order_by_class(classes: &[usize]) -> Vec<usize> {
    let buckets = classes.iter().copied().max().map_or(0, |max| max + 1);
    let mut starts = vec![0usize; buckets + 1];
    for &class in classes {
        starts[class + 1] += 1;
    }
    for bucket in 0..buckets {
        starts[bucket + 1] += starts[bucket];
    }
    let mut order = vec![0usize; classes.len()];
    for (index, &class) in classes.iter().enumerate() {
        order[starts[class]] = index;
        starts[class] += 1;
    }
    order
}

/// `items` in paint order (stable), by the paint-order class of each item's
/// node; a counting sort, since classes are dense ranks.
pub(crate) fn sort_by_paint_order<T: Copy>(
    artifact: &PaintArtifact,
    items: Vec<T>,
    node_index: impl Fn(&T) -> usize,
) -> Vec<T> {
    let classes = items
        .iter()
        .map(|item| artifact.paint_order_class(node_index(item)))
        .collect::<Vec<u32>>();
    if classes.is_sorted() {
        return items;
    }
    // The order depends only on the class sequence, which a frame that did
    // not restack repeats; comparing it is far cheaper than sorting again.
    PAINT_SORT_MEMO.with(|memo| {
        let mut memo = memo.borrow_mut();
        if memo.0 != classes {
            let wide = classes
                .iter()
                .map(|&class| class as usize)
                .collect::<Vec<_>>();
            memo.1 = stable_order_by_class(&wide)
                .into_iter()
                .map(|index| index as u32)
                .collect();
            memo.0 = classes;
        }
        memo.1.iter().map(|&index| items[index as usize]).collect()
    })
}

thread_local! {
    /// The last class sequence `sort_by_paint_order` sorted, and its order.
    static PAINT_SORT_MEMO: std::cell::RefCell<(Vec<u32>, Vec<u32>)> =
        const { std::cell::RefCell::new((Vec::new(), Vec::new())) };
}

thread_local! {
    /// The last paint-order keys this thread ranked and their classes. A
    /// frame whose stacking structure did not change has the same keys as
    /// the frame before it, and comparing the keys is far cheaper than
    /// sorting them again (like Blink keeping paint order until a paint
    /// layer's z-order lists are dirtied).
    static PAINT_CLASS_MEMO: std::cell::RefCell<(PaintOrderKeys, Vec<u32>)> =
        const { std::cell::RefCell::new((PaintOrderKeys::new(), Vec::new())) };
}

fn paint_class_memo_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("W3COS_PAINT_CLASS_MEMO").map_or(true, |v| v != "0"))
}

/// Per node, a rank that orders nodes as their paint-order keys do and is
/// equal exactly for equal keys; 0 is the empty key's.
fn paint_classes_for(paint_order: &PaintOrderKeys) -> Vec<u32> {
    if !paint_class_memo_enabled() {
        return rank_paint_order(paint_order);
    }
    PAINT_CLASS_MEMO.with(|memo| {
        let mut memo = memo.borrow_mut();
        if memo.0 != *paint_order || memo.1.len() != paint_order.len() {
            memo.1 = rank_paint_order(paint_order);
            // Reuses the per-node key allocations of the previous frame.
            memo.0.clone_from(paint_order);
        }
        memo.1.clone()
    })
}

fn rank_paint_order(paint_order: &PaintOrderKeys) -> Vec<u32> {
    let key = |index: u32| paint_order.key(index as usize);
    let mut order: Vec<u32> = (0..paint_order.len() as u32).collect();
    order.sort_by(|left, right| key(*left).cmp(key(*right)));
    let mut classes = vec![0u32; order.len()];
    // Class 0 is kept for the empty key (indices without a key).
    let mut class = 0u32;
    let mut previous: Option<&[PaintOrderLevel]> = None;
    for &index in &order {
        let key = key(index);
        if previous != Some(key) {
            if !(previous.is_none() && key.is_empty()) {
                class += 1;
            }
            previous = Some(key);
        }
        classes[index as usize] = class;
    }
    classes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranged_table_marks_match_the_full_marks() {
        // A small deterministic generator (no rand dependency).
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = |bound: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % bound as u64) as usize
        };
        let displays = [
            Display::Block,
            Display::Inline,
            Display::Table,
            Display::TableRow,
            Display::TableCell,
            Display::Flex,
        ];
        let node = |display: Display, parent: Option<usize>| PaintNode {
            kind: ComponentKind::Column,
            style: Style {
                display,
                ..Style::default()
            }
            .into(),
            parent,
            sticky_counter_signal: None,
        };
        for _ in 0..200 {
            // Preorder: each box's parent is an earlier box on the current
            // ancestor chain.
            let len = 2 + next(60);
            let mut nodes = vec![node(Display::Block, None)];
            let mut chain = vec![0usize];
            for index in 1..len {
                chain.truncate(1 + next(chain.len()));
                let parent = *chain.last().unwrap();
                let display = if next(4) == 0 {
                    displays[next(displays.len())]
                } else {
                    Display::Block
                };
                nodes.push(node(display, Some(parent)));
                chain.push(index);
            }
            let previous = table_subtree_boxes(&nodes);
            let mut changed = Vec::new();
            for _ in 0..1 + next(3) {
                let a = next(len);
                let b = (a + 1 + next(6)).min(len);
                for index in a..b {
                    let parent = nodes[index].parent;
                    nodes[index] = node(displays[next(displays.len())], parent);
                }
                changed.push((a as u32, b as u32));
            }
            assert_eq!(
                table_subtree_boxes_ranged(&nodes, &previous, &changed),
                table_subtree_boxes(&nodes)
            );
        }
    }

    #[test]
    fn tables_from_listed_boxes_match_the_full_scan() {
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = |bound: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % bound as u64) as usize
        };
        let displays = [
            Display::Block,
            Display::Inline,
            Display::Table,
            Display::InlineTable,
            Display::TableRowGroup,
            Display::TableRow,
            Display::TableCell,
            Display::TableCaption,
        ];
        let node = |display: Display, parent: Option<usize>| PaintNode {
            kind: ComponentKind::Column,
            style: Style {
                display,
                ..Style::default()
            }
            .into(),
            parent,
            sticky_counter_signal: None,
        };
        let mut stray = 0;
        let mut found = 0;
        for _ in 0..300 {
            let len = 2 + next(50);
            let mut nodes = vec![node(Display::Block, None)];
            let mut chain = vec![0usize];
            for index in 1..len {
                chain.truncate(1 + next(chain.len()));
                let parent = *chain.last().unwrap();
                let display = if next(3) == 0 {
                    displays[next(displays.len())]
                } else {
                    Display::Block
                };
                nodes.push(node(display, Some(parent)));
                chain.push(index);
            }
            let full = table_passes::tables(&nodes);
            match &full {
                None => stray += 1,
                Some(ranges) => found += ranges.len(),
            }
            assert_eq!(
                table_passes::tables_from_boxes(&nodes, &table_subtree_boxes(&nodes)),
                full
            );
        }
        assert!(stray > 0 && found > 0, "both outcomes must be covered");
    }

    #[test]
    fn refilled_kinds_equal_their_source_and_reuse_text_buffers() {
        let long = "x".repeat(64);
        let mut slots = vec![
            ComponentKind::Text {
                content: long.clone(),
            },
            ComponentKind::Text {
                content: "old".into(),
            },
            ComponentKind::Box,
        ];
        let buffer = match &slots[0] {
            ComponentKind::Text { content } => content.as_ptr(),
            _ => unreachable!(),
        };
        let sources = [
            ComponentKind::Text {
                content: "short".into(),
            },
            ComponentKind::Column,
            ComponentKind::Text {
                content: "from a box".into(),
            },
            ComponentKind::Text {
                content: "no slot".into(),
            },
        ];
        let refilled = sources
            .iter()
            .enumerate()
            .map(|(index, source)| refill_kind(slots.get_mut(index), source))
            .collect::<Vec<_>>();
        assert!(refilled.iter().zip(&sources).all(|(a, b)| a == b));
        match &refilled[0] {
            ComponentKind::Text { content } => assert_eq!(content.as_ptr(), buffer),
            _ => unreachable!(),
        }
    }

    #[test]
    fn paint_order_keys_extend_an_owner_prefix() {
        let mut keys = PaintOrderKeys::with_len(4);
        keys.set(0, None, &[(0, i32::MIN, 0)]);
        keys.set(1, Some((0, 1)), &[(4, 0, 1), (0, i32::MIN, 1)]);
        keys.set(3, Some((1, 2)), &[(2, 0, 3)]);
        assert_eq!(keys.key(0), &[(0, i32::MIN, 0)]);
        assert_eq!(
            keys.key(1),
            &[(0, i32::MIN, 0), (4, 0, 1), (0, i32::MIN, 1)]
        );
        assert_eq!(keys.key(2), &[] as &[PaintOrderLevel]);
        assert_eq!(keys.key(3), &[(0, i32::MIN, 0), (4, 0, 1), (2, 0, 3)]);
        assert_eq!(keys.get(4), None);
        let mut copy = PaintOrderKeys::new();
        copy.clone_from(&keys);
        assert_eq!(copy, keys);
    }

    #[test]
    fn remembered_paint_classes_match_a_fresh_ranking() {
        let first: Vec<Vec<PaintOrderLevel>> = vec![
            vec![(0, 0, 0)],
            vec![],
            vec![(0, 0, 0), (1, 2, 3)],
            vec![(0, -1, 4)],
            vec![(0, 0, 0)],
        ];
        let mut second = first.clone();
        second[3] = vec![(0, 5, 4)];
        let first = PaintOrderKeys::from_keys(&first);
        let second = PaintOrderKeys::from_keys(&second);
        for keys in [&first, &first, &second, &second, &first] {
            assert_eq!(paint_classes_for(keys), rank_paint_order(keys));
        }
        assert_eq!(rank_paint_order(&first), vec![2, 0, 3, 1, 2]);
        assert_eq!(rank_paint_order(&second), vec![1, 0, 2, 3, 1]);
    }

    #[test]
    fn plain_paint_nodes_share_component_styles() {
        let mut child_style = Style::default();
        child_style.border_width = 2.0;
        child_style.border_styles = [Some(w3cos_std::style::BorderLineStyle::Solid); 4];
        let mut hidden = Style::default();
        hidden.border_width = 2.0;
        hidden.border_styles = [Some(w3cos_std::style::BorderLineStyle::None); 4];
        let mut root = w3cos_std::Component::column(Style::default(), vec![]);
        root.children
            .push(w3cos_std::Component::column(child_style, vec![]));
        root.children
            .push(w3cos_std::Component::column(hidden, vec![]));
        let flat = crate::layout::pre_flatten(&root);
        let layouts = (0..flat.len())
            .map(|index| {
                (
                    LayoutRect {
                        x: 0.0,
                        y: 0.0,
                        width: 10.0,
                        height: 10.0,
                    },
                    index,
                )
            })
            .collect::<Vec<_>>();
        let build = || {
            PaintArtifact::build_with_body_background_and_viewport(
                flat.iter().map(|node| PaintNode {
                    kind: node.kind.clone(),
                    style: shared_paint_style(node.component, node.style),
                    parent: node.parent,
                    sticky_counter_signal: node.sticky_counter_signal,
                }),
                &layouts,
                1,
                None,
                Some((100.0, 100.0)),
            )
        };
        let (first, second) = (build(), build());
        for index in 0..2 {
            assert!(std::ptr::eq(
                first.nodes[index].style.as_style(),
                flat[index].style
            ));
            assert!(std::ptr::eq(
                first.nodes[index].style.as_style(),
                second.nodes[index].style.as_style()
            ));
        }
        // A used-value adjustment copies only the adjusted node.
        assert!(!std::ptr::eq(
            first.nodes[2].style.as_style(),
            flat[2].style
        ));
        assert_eq!(first.nodes[2].style.border_width, 0.0);
        assert_eq!(flat[2].style.border_width, 2.0);
    }

    #[test]
    fn float_line_bands_export_shared_advance_without_preserved_breaks() {
        let parent = Style {
            display: Display::Block,
            font_size: 50.0,
            line_height: 0.2,
            line_height_is_normal: false,
            custom_properties: Some(w3cos_std::style::CustomProperties::from_iter([(
                w3cos_dom::user_agent::HTML_STANDARD_FONT_PROPERTY.into(),
                "1".into(),
            )])),
            ..Style::default()
        };
        let text = Style {
            display: Display::Inline,
            font_size: 10.0,
            line_height: 1.0,
            ..parent.clone()
        };
        let nodes = [
            PaintNode {
                kind: ComponentKind::Row,
                style: parent.into(),
                parent: None,
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Text {
                    content: "wrapped text without BR".into(),
                },
                style: text.into(),
                parent: Some(0),
                sticky_counter_signal: None,
            },
        ];
        let mut artifact = PaintArtifact::build(
            nodes,
            &[
                (
                    LayoutRect {
                        x: 8.0,
                        y: 8.0,
                        width: 150.0,
                        height: 96.0,
                    },
                    0,
                ),
                (
                    LayoutRect {
                        x: 8.0,
                        y: 20.0,
                        width: 150.0,
                        height: 14.0,
                    },
                    1,
                ),
            ],
            1,
        );
        // The flow generator and prepaint attach this retained line geometry;
        // its consumer must not require an authored BR or preserved tab.
        artifact.nodes[1]
            .style
            .custom_properties
            .get_or_insert_with(Default::default)
            .insert(
                "--w3cos-internal-float-line-bands".into(),
                "30.953125 0 119.046875".into(),
            );
        let context = artifact
            .inline_line_context(1)
            .expect("float lines need shared metrics");
        assert_eq!(context.line_advance, Some(25.0));
    }

    #[test]
    fn inline_line_context_resets_only_after_a_rewound_later_line() {
        for (x, y, expected_x) in [(8.0, 28.0, 8.0), (88.0, 8.0, 88.0), (8.0, 8.0, 88.0)] {
            let mut parent = Style::default();
            parent.display = Display::Block;
            parent.font_size = 20.0;
            parent.line_height = 1.0;
            let mut text = parent.clone();
            text.display = Display::Inline;
            text.white_space = WhiteSpace::Pre;
            let nodes = [
                PaintNode {
                    kind: ComponentKind::Row,
                    style: parent.into(),
                    parent: None,
                    sticky_counter_signal: None,
                },
                PaintNode {
                    kind: ComponentKind::Text {
                        content: "\u{200b}".into(),
                    },
                    style: text.clone().into(),
                    parent: Some(0),
                    sticky_counter_signal: None,
                },
                PaintNode {
                    kind: ComponentKind::Text {
                        content: "XX\nXX".into(),
                    },
                    style: text.into(),
                    parent: Some(0),
                    sticky_counter_signal: None,
                },
            ];
            let artifact = PaintArtifact::build(
                nodes,
                &[
                    (
                        LayoutRect {
                            x: 8.0,
                            y: 8.0,
                            width: 100.0,
                            height: 60.0,
                        },
                        0,
                    ),
                    (
                        LayoutRect {
                            x: 88.0,
                            y: 8.0,
                            width: 0.0,
                            height: 20.0,
                        },
                        1,
                    ),
                    (
                        LayoutRect {
                            x,
                            y,
                            width: 100.0,
                            height: 20.0,
                        },
                        2,
                    ),
                ],
                1,
            );
            let context = artifact.inline_line_context(2).unwrap();
            assert_eq!(context.first_line_box.x, expected_x, "x={x}, y={y}");
            assert_eq!(
                context.first_line_box.width,
                108.0 - expected_x,
                "x={x}, y={y}"
            );
        }
    }

    #[test]
    fn paint_snapshot_resolves_percentage_padding_from_containing_block() {
        let mut text_style = Style::default();
        text_style.display = Display::Block;
        text_style.padding.right = w3cos_std::style::Spacing::Percent(45.3);
        let nodes = vec![
            PaintNode {
                kind: ComponentKind::Row,
                style: Style::default().into(),
                parent: None,
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Row,
                style: (Style {
                    display: Display::Block,
                    ..Style::default()
                })
                .into(),
                parent: Some(0),
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Text {
                    content: "x".into(),
                },
                style: text_style.into(),
                parent: Some(1),
                sticky_counter_signal: None,
            },
        ];
        let layouts = [
            (
                LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: 800.0,
                    height: 600.0,
                },
                0,
            ),
            (
                LayoutRect {
                    x: 8.0,
                    y: 0.0,
                    width: 106.0,
                    height: 100.0,
                },
                1,
            ),
            (
                LayoutRect {
                    x: 8.0,
                    y: 0.0,
                    width: 106.0,
                    height: 10.0,
                },
                2,
            ),
        ];
        let artifact = PaintArtifact::build(nodes, &layouts, 1);
        assert!((artifact.nodes[2].style.padding_lengths().right - 48.018).abs() < 0.01);
    }

    /// Every build gets its own nonzero serial, with or without append
    /// reuse: caches keyed by it (the damage ink cache) must never take
    /// another build's entries.
    #[test]
    fn every_build_has_its_own_serial() {
        let mut serials = Vec::new();
        for reuse in [false, false, true, true] {
            APPEND_REUSE_OVERRIDE.with(|flag| flag.set(Some(reuse)));
            let root = PaintNode {
                kind: ComponentKind::Root,
                style: Style::default().into(),
                parent: None,
                sticky_counter_signal: None,
            };
            let rect = LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            };
            let artifact = PaintArtifact::build([root], &[(rect, 0)], 1);
            serials.push(artifact.serial());
        }
        APPEND_REUSE_OVERRIDE.with(|flag| flag.set(None));
        assert!(serials.iter().all(|serial| *serial != 0), "{serials:?}");
        let mut unique = serials.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), serials.len(), "{serials:?}");
    }

    /// Root content outside the cull band emits no display items; boxes in
    /// it, and a fixed box anywhere, still paint; none of the dropped boxes
    /// reaches the viewport.
    #[test]
    fn paint_cull_band_drops_only_root_content_outside_it() {
        PAINT_CULL_RECT_OVERRIDE.with(|mode| mode.set(Some(2)));
        PAINT_CULL_BAND.with(|band| band.set(Some((-4000.0, 720.0 + 4000.0))));
        let mut block = Style::default();
        block.display = Display::Block;
        let mut fixed = block.clone();
        fixed.position = Position::Fixed;
        let node = |style: &Style, parent: Option<usize>| PaintNode {
            kind: ComponentKind::Root,
            style: style.clone().into(),
            parent,
            sticky_counter_signal: None,
        };
        let nodes = vec![
            node(&block, None),
            node(&block, Some(0)),
            node(&block, Some(0)),
            node(&fixed, Some(0)),
        ];
        let rect = |y: f32| LayoutRect {
            x: 0.0,
            y,
            width: 100.0,
            height: 20.0,
        };
        let layouts = vec![
            (
                LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: 100.0,
                    height: 20000.0,
                },
                0,
            ),
            (rect(100.0), 1),
            (rect(10000.0), 2),
            (rect(10000.0), 3),
        ];
        let artifact = PaintArtifact::build_reusing(nodes, &layouts, 1, None, None, None);
        PAINT_CULL_BAND.with(|band| band.set(None));
        PAINT_CULL_RECT_OVERRIDE.with(|mode| mode.set(None));
        let clients: Vec<usize> = artifact
            .display_items
            .iter()
            .map(|item| item.client_index)
            .collect();
        assert_eq!(clients, vec![0, 1, 3]);
        assert_eq!(artifact.culled_nodes, vec![2]);
        assert_eq!(artifact.culled_in_viewport(1280.0, 720.0), 0);
        assert!(artifact.cull_band_covers(0.0, 720.0, 512.0));
        assert!(!artifact.cull_band_covers(9000.0, 720.0, 0.0));
    }

    #[test]
    fn cull_span_cursor_matches_the_binary_search() {
        PAINT_CULL_SPAN_CURSOR_OVERRIDE.with(|mode| mode.set(Some(1)));
        let mut seed = 0x51ed_270b_27b6_37d1_u64;
        let mut next = |bound: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % bound as u64) as usize
        };
        for _ in 0..200 {
            let mut spans = Vec::new();
            let mut at = next(4);
            for _ in 0..next(12) {
                let start = at + next(5);
                let end = start + 1 + next(9);
                spans.push(CullSpan {
                    start: start as u32,
                    end: end as u32,
                    top: 0.0,
                    bottom: 0.0,
                    visible: 0,
                    parents: [0; 2],
                    parent_count: 0,
                    root: PaintProperties::default(),
                });
                at = end;
            }
            let mut cursor = CullSpanCursor::default();
            let mut box_index = 0usize;
            for _ in 0..60 {
                // Mostly forward, sometimes a few boxes back.
                box_index = if next(5) == 0 {
                    box_index.saturating_sub(next(8))
                } else {
                    box_index + next(4)
                };
                assert_eq!(
                    cursor.starting(&spans, box_index),
                    spans.partition_point(|span| (span.start as usize) < box_index)
                );
                assert_eq!(
                    cursor.ending(&spans, box_index),
                    spans.partition_point(|span| span.end as usize <= box_index)
                );
            }
        }
        PAINT_CULL_SPAN_CURSOR_OVERRIDE.with(|mode| mode.set(None));
    }

    /// `W3COS_PAINT_SPLIT_OWNER_REUSE`: a second build over the same boxes
    /// with only the first row changed takes last build's split owner flags
    /// (verify agrees), and its output equals the build without it.
    #[test]
    fn split_owner_reuse_takes_last_flags_and_matches() {
        APPEND_REUSE_OVERRIDE.with(|flag| flag.set(Some(true)));
        PAINT_TOUCHED_OVERRIDE.with(|mode| mode.set(Some(1)));
        let mut block = Style::default();
        block.display = Display::Block;
        let mut owner = block.clone();
        owner
            .custom_properties
            .get_or_insert_with(Default::default)
            .insert(
                "--w3cos-internal-positioned-inline-owner".into(),
                "inline:7".into(),
            );
        let nodes = || {
            (0..20)
                .map(|index| PaintNode {
                    kind: ComponentKind::Column,
                    style: if index == 9 {
                        owner.clone()
                    } else {
                        block.clone()
                    }
                    .into(),
                    parent: (index > 0).then_some(0),
                    sticky_counter_signal: None,
                })
                .collect::<Vec<_>>()
        };
        let layouts = |grow: f32| {
            (0..20)
                .map(|index| {
                    let height = if index == 0 { 1000.0 } else { 20.0 };
                    let y = if index == 0 {
                        0.0
                    } else {
                        30.0 * index as f32 + grow
                    };
                    let rect = LayoutRect {
                        x: 0.0,
                        y,
                        width: 100.0,
                        height: if index == 1 { height + grow } else { height },
                    };
                    (rect, index)
                })
                .collect::<Vec<_>>()
        };
        let build = |grow: f32, previous: Option<&PaintArtifact>| {
            set_paint_touched(Some(crate::incremental_layout::PaintChanges {
                len: 20,
                changed: vec![(0, 2)],
                moved: vec![(2, 20)],
                ..Default::default()
            }));
            let artifact =
                PaintArtifact::build_reusing(nodes(), &layouts(grow), 1, None, None, previous);
            set_paint_touched(None);
            artifact
        };
        PAINT_SPLIT_OWNER_REUSE_OVERRIDE.with(|mode| mode.set(Some(0)));
        let first = build(0.0, None);
        let full = build(8.0, Some(&first));
        for mode in [2, 1] {
            PAINT_SPLIT_OWNER_REUSE_OVERRIDE.with(|value| value.set(Some(mode)));
            let before = PAINT_SPLIT_OWNER_REUSE_STATS.with(std::cell::Cell::get);
            let reused = build(8.0, Some(&first));
            let after = PAINT_SPLIT_OWNER_REUSE_STATS.with(std::cell::Cell::get);
            assert!(after.0 > before.0, "the memo takes last build's flags");
            assert_eq!(after.1, before.1, "verify finds an untouched flag changed");
            assert!(reused.append_output_eq(&full));
            let record = reused.append_record.as_deref().expect("append record");
            assert!(record.memo.split_owner[9], "the owner box keeps its flag");
            assert_eq!(
                record.memo.split_owner.iter().filter(|flag| **flag).count(),
                1
            );
        }
        PAINT_SPLIT_OWNER_REUSE_OVERRIDE.with(|mode| mode.set(None));
        PAINT_TOUCHED_OVERRIDE.with(|mode| mode.set(None));
        APPEND_REUSE_OVERRIDE.with(|flag| flag.set(None));
    }

    /// `W3COS_PAINT_CULL_RUN_GAPS`: boxes far below the band shift down
    /// twice; in the second shift they sit in last build's culled spans and
    /// outside the layout's changed range, so the short check takes them.
    /// Verify finds it agrees with the full check, and the build equals the
    /// one without it.
    #[test]
    fn culled_run_gaps_take_last_spans_and_match_the_full_check() {
        APPEND_REUSE_OVERRIDE.with(|flag| flag.set(Some(true)));
        ARTIFACT_SPLICE_OVERRIDE.with(|flag| flag.set(Some(true)));
        PAINT_CULL_RECT_OVERRIDE.with(|mode| mode.set(Some(1)));
        PAINT_CULL_RUNS_OVERRIDE.with(|mode| mode.set(Some(1)));
        PAINT_TOUCHED_OVERRIDE.with(|mode| mode.set(Some(1)));
        // Subtree culling stays off outside its own loop below (whatever
        // `W3COS_PAINT_CULL_SUBTREES` is), so the short checks are counted.
        PAINT_CULL_SUBTREES_OVERRIDE.with(|value| value.set(Some(0)));
        let mut block = Style::default();
        block.display = Display::Block;
        let nodes = || {
            (0..40)
                .map(|index| PaintNode {
                    kind: ComponentKind::Column,
                    style: block.clone().into(),
                    parent: (index > 0).then_some(0),
                    sticky_counter_signal: None,
                })
                .collect::<Vec<_>>()
        };
        let layouts = |grow: f32| {
            (0..40)
                .map(|index| {
                    let (y, height) = match index {
                        0 => (0.0, 40000.0),
                        1 => (0.0, 20.0 + grow),
                        2..8 => (30.0 * index as f32 + grow, 20.0),
                        _ => (9000.0 + 30.0 * index as f32 + grow, 20.0),
                    };
                    let rect = LayoutRect {
                        x: 0.0,
                        y,
                        width: 100.0,
                        height,
                    };
                    (rect, index)
                })
                .collect::<Vec<_>>()
        };
        let build = |grow: f32, previous: Option<&PaintArtifact>, gaps: u8| {
            PAINT_CULL_RUN_GAPS_OVERRIDE.with(|mode| mode.set(Some(gaps)));
            // The first row grew; every box after it only moved.
            set_paint_touched(Some(crate::incremental_layout::PaintChanges {
                len: 40,
                changed: vec![(0, 2)],
                moved: vec![(2, 40)],
                ..Default::default()
            }));
            PAINT_CULL_BAND.with(|band| band.set(Some((-4000.0, 720.0 + 4000.0))));
            let artifact =
                PaintArtifact::build_reusing(nodes(), &layouts(grow), 1, None, None, previous);
            PAINT_CULL_BAND.with(|band| band.set(None));
            set_paint_touched(None);
            artifact
        };
        let first = build(0.0, None, 1);
        let second = build(16.0, Some(&first), 1);
        // `W3COS_PAINT_CULL_SUBTREES`: the moved span is culled whole by its
        // kept extent; verify checks every box agrees.
        // `W3COS_PAINT_CULL_SPAN_CURSOR`: the spans are found by cursor
        // (verify searches too and compares); the build is the same.
        for (mode, cursor) in [(2, 0), (1, 0), (1, 2), (1, 1)] {
            PAINT_CULL_SPAN_CURSOR_OVERRIDE.with(|value| value.set(Some(cursor)));
            PAINT_CULL_SUBTREES_OVERRIDE.with(|value| value.set(Some(mode)));
            let before = PAINT_CULL_SUBTREE_STATS.with(std::cell::Cell::get);
            let lookups_before = PAINT_CULL_SPAN_CURSOR_STATS.with(std::cell::Cell::get);
            let whole = build(32.0, Some(&second), 1);
            let lookups = PAINT_CULL_SPAN_CURSOR_STATS.with(std::cell::Cell::get);
            PAINT_CULL_SPAN_CURSOR_OVERRIDE.with(|value| value.set(None));
            if cursor != 0 {
                assert!(
                    lookups.0 > lookups_before.0,
                    "the spans are found by cursor"
                );
            }
            assert_eq!(
                lookups.1, lookups_before.1,
                "a cursor span differs from the search"
            );
            let after = PAINT_CULL_SUBTREE_STATS.with(std::cell::Cell::get);
            PAINT_CULL_SUBTREES_OVERRIDE.with(|value| value.set(Some(0)));
            assert!(after.0 > before.0, "the moved span is taken whole");
            assert!(after.1 - before.1 >= 30);
            assert_eq!(
                after.2, before.2,
                "a box of a whole span fails the full check"
            );
            PAINT_CULL_RUN_GAPS_OVERRIDE.with(|value| value.set(Some(0)));
            let reference = {
                set_paint_touched(Some(crate::incremental_layout::PaintChanges {
                    len: 40,
                    changed: vec![(0, 2)],
                    moved: vec![(2, 40)],
                    ..Default::default()
                }));
                PAINT_CULL_BAND.with(|band| band.set(Some((-4000.0, 720.0 + 4000.0))));
                let artifact = PaintArtifact::build_reusing(
                    nodes(),
                    &layouts(32.0),
                    1,
                    None,
                    None,
                    Some(&second),
                );
                PAINT_CULL_BAND.with(|band| band.set(None));
                set_paint_touched(None);
                artifact
            };
            assert!(whole.append_output_eq(&reference));
        }
        // `W3COS_PAINT_DIFF_CULLED_SPANS`: the diff takes the culled span's
        // boxes as quiet from the build and marks the moved ones, as the
        // full test does.
        {
            use crate::retained_layers::{
                PAINT_DIFF_CULLED_SPANS_OVERRIDE, PAINT_DIFF_CULLED_SPANS_STATS, paint_diff_clients,
            };
            let third = build(32.0, Some(&second), 1);
            let diff = |mode: u8| {
                PAINT_DIFF_CULLED_SPANS_OVERRIDE.with(|value| value.set(Some(mode)));
                let diff = paint_diff_clients(&second, &third).expect("paint diff");
                PAINT_DIFF_CULLED_SPANS_OVERRIDE.with(|value| value.set(None));
                (diff.dirty, diff.moved)
            };
            let before = PAINT_DIFF_CULLED_SPANS_STATS.with(std::cell::Cell::get);
            let verified = diff(2);
            let after = PAINT_DIFF_CULLED_SPANS_STATS.with(std::cell::Cell::get);
            assert!(
                after.0 - before.0 >= 30,
                "the culled boxes are taken by span"
            );
            assert_eq!(
                after.1, before.1,
                "the span test disagrees with the full one"
            );
            let full = diff(0);
            assert!(verified == full);
            assert!(diff(1) == full);
            assert!(full.0[39], "a moved culled box is still marked");
        }
        let before = PAINT_CULL_RUN_GAPS_STATS.with(std::cell::Cell::get);
        let verified = build(32.0, Some(&second), 2);
        let after = PAINT_CULL_RUN_GAPS_STATS.with(std::cell::Cell::get);
        let fast = build(32.0, Some(&second), 1);
        let full = build(32.0, Some(&second), 0);
        PAINT_CULL_RUN_GAPS_OVERRIDE.with(|mode| mode.set(None));
        PAINT_CULL_SUBTREES_OVERRIDE.with(|value| value.set(None));
        PAINT_TOUCHED_OVERRIDE.with(|mode| mode.set(None));
        PAINT_CULL_RUNS_OVERRIDE.with(|mode| mode.set(None));
        PAINT_CULL_RECT_OVERRIDE.with(|mode| mode.set(None));
        ARTIFACT_SPLICE_OVERRIDE.with(|flag| flag.set(None));
        APPEND_REUSE_OVERRIDE.with(|flag| flag.set(None));
        assert!(
            after.0 - before.0 >= 30,
            "the shifted boxes take the short check"
        );
        assert_eq!(
            after.1, before.1,
            "the short check disagrees with the full one"
        );
        assert!(verified.append_output_eq(&full));
        assert!(fast.append_output_eq(&full));
        assert_eq!(fast.display_items.len(), full.display_items.len());
    }

    /// Boxes far below the band that shift down with a growing first row
    /// are copied as a culled run and match a build without the run copy.
    #[test]
    fn culled_run_copies_shifted_boxes_and_matches_a_per_box_build() {
        APPEND_REUSE_OVERRIDE.with(|flag| flag.set(Some(true)));
        ARTIFACT_SPLICE_OVERRIDE.with(|flag| flag.set(Some(true)));
        PAINT_CULL_RECT_OVERRIDE.with(|mode| mode.set(Some(2)));
        let mut block = Style::default();
        block.display = Display::Block;
        let nodes = || {
            (0..40)
                .map(|index| PaintNode {
                    kind: ComponentKind::Column,
                    style: block.clone().into(),
                    parent: (index > 0).then_some(0),
                    sticky_counter_signal: None,
                })
                .collect::<Vec<_>>()
        };
        let layouts = |grow: f32| {
            let mut layouts = vec![(
                LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: 100.0,
                    height: 40000.0,
                },
                0,
            )];
            layouts.push((
                LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: 100.0,
                    height: 20.0 + grow,
                },
                1,
            ));
            for index in 2..40 {
                let y = if index < 8 {
                    30.0 * index as f32
                } else {
                    9000.0 + 30.0 * index as f32
                };
                layouts.push((
                    LayoutRect {
                        x: 0.0,
                        y: y + grow,
                        width: 100.0,
                        height: 20.0,
                    },
                    index,
                ));
            }
            layouts
        };
        let build = |grow: f32, previous: Option<&PaintArtifact>| {
            PAINT_CULL_BAND.with(|band| band.set(Some((-4000.0, 720.0 + 4000.0))));
            let artifact =
                PaintArtifact::build_reusing(nodes(), &layouts(grow), 1, None, None, previous);
            PAINT_CULL_BAND.with(|band| band.set(None));
            artifact
        };
        PAINT_CULL_RUNS_OVERRIDE.with(|mode| mode.set(Some(1)));
        let first = build(0.0, None);
        let before = PAINT_CULL_RUN_NODES.with(std::cell::Cell::get);
        let culled_before = PAINT_CULL_COUNT.with(std::cell::Cell::get);
        let runs = build(16.0, Some(&first));
        let copied = PAINT_CULL_RUN_NODES.with(std::cell::Cell::get) - before;
        let culled_runs = PAINT_CULL_COUNT.with(std::cell::Cell::get) - culled_before;
        PAINT_CULL_RUNS_OVERRIDE.with(|mode| mode.set(Some(0)));
        let culled_before = PAINT_CULL_COUNT.with(std::cell::Cell::get);
        let per_box = build(16.0, Some(&first));
        let culled_per_box = PAINT_CULL_COUNT.with(std::cell::Cell::get) - culled_before;
        PAINT_CULL_RUNS_OVERRIDE.with(|mode| mode.set(None));
        PAINT_CULL_RECT_OVERRIDE.with(|mode| mode.set(None));
        ARTIFACT_SPLICE_OVERRIDE.with(|flag| flag.set(None));
        APPEND_REUSE_OVERRIDE.with(|flag| flag.set(None));
        assert_eq!(copied, 32, "boxes 8..40 lie below the band in both frames");
        assert!(runs.append_output_eq(&per_box));
        assert_eq!(runs.culled_nodes, per_box.culled_nodes);
        assert_eq!(culled_runs, culled_per_box);
        assert_eq!(
            runs.append_record
                .as_ref()
                .map(|record| record.marks.clone()),
            per_box
                .append_record
                .as_ref()
                .map(|record| record.marks.clone())
        );
    }

    /// A frame whose text changed and whose later siblings moved down
    /// copies the unchanged nodes' output and matches a full build.
    #[test]
    fn append_reuse_copies_clean_nodes_and_matches_a_full_build() {
        APPEND_REUSE_OVERRIDE.with(|flag| flag.set(Some(true)));
        let block = |overflow: bool, opacity: f32| {
            let mut style = Style::default();
            style.display = Display::Block;
            if overflow {
                style.overflow_x = Some(Overflow::Hidden);
                style.overflow_y = Some(Overflow::Hidden);
            }
            style.opacity = opacity;
            style
        };
        let node = |kind: ComponentKind, style: &Style, parent: Option<usize>| PaintNode {
            kind,
            style: style.clone().into(),
            parent,
            sticky_counter_signal: None,
        };
        let text = |content: &str| ComponentKind::Text {
            content: content.into(),
        };
        let (plain, clipped, faded) = (block(false, 1.0), block(true, 1.0), block(false, 0.5));
        let nodes = |first: &str| {
            vec![
                node(ComponentKind::Root, &plain, None),
                node(ComponentKind::Root, &clipped, Some(0)),
                node(text(first), &plain, Some(1)),
                node(ComponentKind::Root, &faded, Some(1)),
                node(text("second"), &plain, Some(3)),
                node(ComponentKind::Root, &plain, Some(0)),
                node(text("third"), &plain, Some(5)),
            ]
        };
        let rect = |y: f32, height: f32| LayoutRect {
            x: 0.0,
            y,
            width: 200.0,
            height,
        };
        let layouts = |grow: f32| {
            vec![
                (rect(0.0, 300.0 + grow), 0),
                (rect(0.0, 100.0 + grow), 1),
                (rect(0.0, 20.0 + grow), 2),
                (rect(20.0 + grow, 20.0), 3),
                (rect(20.0 + grow, 20.0), 4),
                (rect(100.0 + grow, 50.0), 5),
                (rect(100.0 + grow, 20.0), 6),
            ]
        };
        let first =
            PaintArtifact::build_reusing(nodes("first"), &layouts(0.0), 1, None, None, None);
        let (copied_before, _, mismatches_before) = append_reuse_stats();
        let next = PaintArtifact::build_reusing(
            nodes("first, longer"),
            &layouts(20.0),
            2,
            None,
            None,
            Some(&first),
        );
        APPEND_REUSE_OVERRIDE.with(|flag| flag.set(Some(false)));
        let fresh = PaintArtifact::build(nodes("first, longer"), &layouts(20.0), 2);
        let (copied, _, mismatches) = append_reuse_stats();
        assert!(next.append_output_eq(&fresh));
        assert_eq!(mismatches, mismatches_before);
        // The moved faded box and its text, and the moved last block and
        // its text, are copied; the grown clip container and the edited
        // text are rebuilt.
        assert!(
            copied - copied_before >= 4,
            "copied {}",
            copied - copied_before
        );
        let unchanged = next
            .append_unchanged_against(&first)
            .expect("built against first");
        assert!(!unchanged[2] && unchanged[4] && unchanged[6]);
        APPEND_REUSE_OVERRIDE.with(|flag| flag.set(None));
    }

    /// `W3COS_PAINT_TOUCHED`: with the splice's ranges (the edited text and
    /// its grown ancestors changed, the later boxes moved) the build matches
    /// a full build; ranges that miss the edit and the moves are caught by
    /// verify.
    #[test]
    fn a_touched_set_skips_the_input_compare_and_verify_catches_a_wrong_one() {
        APPEND_REUSE_OVERRIDE.with(|flag| flag.set(Some(true)));
        let block = |overflow: bool, opacity: f32| {
            let mut style = Style::default();
            style.display = Display::Block;
            if overflow {
                style.overflow_x = Some(Overflow::Hidden);
                style.overflow_y = Some(Overflow::Hidden);
            }
            style.opacity = opacity;
            style
        };
        let node = |kind: ComponentKind, style: &Style, parent: Option<usize>| PaintNode {
            kind,
            style: style.clone().into(),
            parent,
            sticky_counter_signal: None,
        };
        let text = |content: &str| ComponentKind::Text {
            content: content.into(),
        };
        let (plain, clipped, faded) = (block(false, 1.0), block(true, 1.0), block(false, 0.5));
        let nodes = |first: &str| {
            vec![
                node(ComponentKind::Root, &plain, None),
                node(ComponentKind::Root, &clipped, Some(0)),
                node(text(first), &plain, Some(1)),
                node(ComponentKind::Root, &faded, Some(1)),
                node(text("second"), &plain, Some(3)),
                node(ComponentKind::Root, &plain, Some(0)),
                node(text("third"), &plain, Some(5)),
            ]
        };
        let rect = |y: f32, height: f32| LayoutRect {
            x: 0.0,
            y,
            width: 200.0,
            height,
        };
        let layouts = |grow: f32| {
            vec![
                (rect(0.0, 300.0 + grow), 0),
                (rect(0.0, 100.0 + grow), 1),
                (rect(0.0, 20.0 + grow), 2),
                (rect(20.0 + grow, 20.0), 3),
                (rect(20.0 + grow, 20.0), 4),
                (rect(100.0 + grow, 50.0), 5),
                (rect(100.0 + grow, 20.0), 6),
            ]
        };
        let first =
            PaintArtifact::build_reusing(nodes("first"), &layouts(0.0), 1, None, None, None);
        let build = |mode: u8, changed: Vec<(u32, u32)>, moved: Vec<(u32, u32)>| {
            PAINT_TOUCHED_OVERRIDE.with(|flag| flag.set(Some(mode)));
            set_paint_touched(Some(crate::incremental_layout::PaintChanges {
                len: 7,
                changed,
                moved,
                ..Default::default()
            }));
            let next = PaintArtifact::build_reusing(
                nodes("first, longer"),
                &layouts(20.0),
                2,
                None,
                None,
                Some(&first),
            );
            set_paint_touched(None);
            PAINT_TOUCHED_OVERRIDE.with(|flag| flag.set(None));
            next
        };
        APPEND_REUSE_OVERRIDE.with(|flag| flag.set(Some(false)));
        let fresh = PaintArtifact::build(nodes("first, longer"), &layouts(20.0), 2);
        APPEND_REUSE_OVERRIDE.with(|flag| flag.set(Some(true)));
        // The edited text and its ancestors changed; the rest moved.
        let right = build(1, vec![(0, 3)], vec![(3, 7)]);
        assert!(right.append_output_eq(&fresh));
        let before = paint_touched_mismatches();
        let verified = build(2, vec![(0, 1)], Vec::new());
        assert!(verified.append_output_eq(&fresh));
        assert!(paint_touched_mismatches() > before);
        APPEND_REUSE_OVERRIDE.with(|flag| flag.set(None));
    }

    /// `W3COS_PAINT_DIFF_SPARSE`: inside copied runs only the listed
    /// volatile clients are visited, and the diff equals the dense one.
    #[test]
    fn sparse_paint_diff_visits_volatile_clients_in_runs_and_matches_the_dense_diff() {
        use crate::retained_layers::{PAINT_DIFF_SPARSE_OVERRIDE, paint_diff_clients};
        ARTIFACT_SPLICE_OVERRIDE.with(|flag| flag.set(Some(true)));
        APPEND_REUSE_OVERRIDE.with(|flag| flag.set(None));
        PAINT_DIFF_SPARSE_OVERRIDE.with(|mode| mode.set(Some(1)));
        let mut plain = Style::default();
        plain.display = Display::Block;
        let node = |kind: ComponentKind, parent: Option<usize>| PaintNode {
            kind,
            style: plain.clone().into(),
            parent,
            sticky_counter_signal: None,
        };
        let text = |content: &str| ComponentKind::Text {
            content: content.into(),
        };
        let rect = |y: f32, height: f32| LayoutRect {
            x: 0.0,
            y,
            width: 200.0,
            height,
        };
        let blocks = 12;
        // The edited block sits in the middle: runs before and after it.
        let nodes = |edited: &str| {
            let mut nodes = vec![node(ComponentKind::Root, None)];
            for index in 0..blocks {
                nodes.push(node(ComponentKind::Root, Some(0)));
                let content = if index == blocks / 2 {
                    edited.to_string()
                } else {
                    format!("block {index}")
                };
                // An image inside the first run repaints every frame.
                let kind = if index == 1 {
                    ComponentKind::Image {
                        src: "a.png".into(),
                    }
                } else {
                    text(&content)
                };
                nodes.push(node(kind, Some(2 * index + 1)));
            }
            nodes
        };
        let layouts = || {
            let mut layouts = vec![(rect(0.0, 20.0 * blocks as f32), 0)];
            for index in 0..blocks {
                let y = 20.0 * index as f32;
                layouts.push((rect(y, 20.0), 2 * index + 1));
                layouts.push((rect(y, 20.0), 2 * index + 2));
            }
            layouts
        };
        let first = PaintArtifact::build_reusing(nodes("middle"), &layouts(), 1, None, None, None);
        let next = PaintArtifact::build_reusing(
            nodes("middle, edited"),
            &layouts(),
            2,
            None,
            None,
            Some(&first),
        );
        ARTIFACT_SPLICE_OVERRIDE.with(|flag| flag.set(None));
        assert_eq!(next.volatile_nodes(), Some(&[4u32][..]));
        let runs = next.append_runs_against(&first).expect("runs");
        assert!(
            runs.iter().any(|run| run[0] <= 4 && 4 < run[1]),
            "the image is in a run"
        );
        let sparse = paint_diff_clients(&first, &next).expect("sparse diff");
        // The item hashes in a map (forced: few boxes here) give the same diff.
        use crate::retained_layers::{SPARSE_ITEM_HASHES_FORCE, SPARSE_ITEM_HASHES_USED};
        let used = SPARSE_ITEM_HASHES_USED.with(std::cell::Cell::get);
        SPARSE_ITEM_HASHES_FORCE.with(|flag| flag.set(true));
        let mapped = paint_diff_clients(&first, &next).expect("sparse diff, mapped hashes");
        SPARSE_ITEM_HASHES_FORCE.with(|flag| flag.set(false));
        assert!(SPARSE_ITEM_HASHES_USED.with(std::cell::Cell::get) > used);
        PAINT_DIFF_SPARSE_OVERRIDE.with(|mode| mode.set(Some(0)));
        let dense = paint_diff_clients(&first, &next).expect("dense diff");
        PAINT_DIFF_SPARSE_OVERRIDE.with(|mode| mode.set(None));
        assert_eq!(sparse.dirty, dense.dirty);
        assert_eq!(sparse.moved, dense.moved);
        assert_eq!(mapped.dirty, dense.dirty);
        assert_eq!(mapped.moved, dense.moved);
        assert!(dense.dirty[4], "the image repaints");
        assert!(
            dense.dirty[2 * (blocks / 2) + 2],
            "the edited text is dirty"
        );
    }

    /// A frame whose last block grew copies the unchanged blocks before it
    /// as one aligned run and matches a full build.
    #[test]
    fn paint_diff_skips_copied_runs_and_matches_the_full_diff() {
        use crate::retained_layers::{PAINT_DIFF_RUNS_OVERRIDE, paint_diff_clients};
        ARTIFACT_SPLICE_OVERRIDE.with(|flag| flag.set(Some(true)));
        APPEND_REUSE_OVERRIDE.with(|flag| flag.set(None));
        let mut plain = Style::default();
        plain.display = Display::Block;
        let node = |kind: ComponentKind, parent: Option<usize>| PaintNode {
            kind,
            style: plain.clone().into(),
            parent,
            sticky_counter_signal: None,
        };
        let text = |content: &str| ComponentKind::Text {
            content: content.into(),
        };
        let rect = |y: f32, height: f32| LayoutRect {
            x: 0.0,
            y,
            width: 200.0,
            height,
        };
        let blocks = 12;
        // The edited block sits in the middle: runs before and after it.
        let nodes = |edited: &str| {
            let mut nodes = vec![node(ComponentKind::Root, None)];
            for index in 0..blocks {
                nodes.push(node(ComponentKind::Root, Some(0)));
                let content = if index == blocks / 2 {
                    edited.to_string()
                } else {
                    format!("block {index}")
                };
                nodes.push(node(text(&content), Some(2 * index + 1)));
            }
            nodes
        };
        let layouts = || {
            let mut layouts = vec![(rect(0.0, 20.0 * blocks as f32), 0)];
            for index in 0..blocks {
                let y = 20.0 * index as f32;
                layouts.push((rect(y, 20.0), 2 * index + 1));
                layouts.push((rect(y, 20.0), 2 * index + 2));
            }
            layouts
        };
        let first = PaintArtifact::build_reusing(nodes("middle"), &layouts(), 1, None, None, None);
        let next = PaintArtifact::build_reusing(
            nodes("middle, edited"),
            &layouts(),
            2,
            None,
            None,
            Some(&first),
        );
        ARTIFACT_SPLICE_OVERRIDE.with(|flag| flag.set(None));
        let runs = next
            .append_runs_against(&first)
            .expect("built against first");
        assert!(runs.len() >= 1, "unchanged blocks copy as runs");
        assert!(next.append_runs_against(&next).is_none());
        PAINT_DIFF_RUNS_OVERRIDE.with(|mode| mode.set(Some(0)));
        let full = paint_diff_clients(&first, &next).expect("full diff");
        PAINT_DIFF_RUNS_OVERRIDE.with(|mode| mode.set(Some(1)));
        let skipped = paint_diff_clients(&first, &next).expect("diff with runs");
        PAINT_DIFF_RUNS_OVERRIDE.with(|mode| mode.set(None));
        assert_eq!(full.dirty, skipped.dirty);
        assert_eq!(full.moved, skipped.moved);
        assert!(full.dirty[2 * (blocks / 2) + 2], "the edited text is dirty");
        assert_eq!(full.dirty.iter().filter(|&&d| d).count() >= 1, true);
    }

    #[test]
    fn artifact_splice_copies_aligned_runs_and_matches_a_full_build() {
        ARTIFACT_SPLICE_OVERRIDE.with(|flag| flag.set(Some(true)));
        APPEND_REUSE_OVERRIDE.with(|flag| flag.set(None));
        let mut plain = Style::default();
        plain.display = Display::Block;
        let mut clipped = plain.clone();
        clipped.overflow_x = Some(Overflow::Hidden);
        clipped.overflow_y = Some(Overflow::Hidden);
        let node = |kind: ComponentKind, style: &Style, parent: Option<usize>| PaintNode {
            kind,
            style: style.clone().into(),
            parent,
            sticky_counter_signal: None,
        };
        let text = |content: &str| ComponentKind::Text {
            content: content.into(),
        };
        let rect = |y: f32, height: f32| LayoutRect {
            x: 0.0,
            y,
            width: 200.0,
            height,
        };
        let blocks = 10;
        let nodes = |last: &str| {
            let mut nodes = vec![node(ComponentKind::Root, &clipped, None)];
            for index in 0..blocks {
                nodes.push(node(ComponentKind::Root, &plain, Some(0)));
                nodes.push(node(
                    text(&format!("block {index}")),
                    &plain,
                    Some(2 * index + 1),
                ));
            }
            nodes.push(node(ComponentKind::Root, &plain, Some(0)));
            nodes.push(node(text(last), &plain, Some(2 * blocks + 1)));
            nodes
        };
        let layouts = |grow: f32| {
            let mut layouts = vec![(rect(0.0, 20.0 * blocks as f32 + 20.0 + grow), 0)];
            for index in 0..blocks {
                let y = 20.0 * index as f32;
                layouts.push((rect(y, 20.0), 2 * index + 1));
                layouts.push((rect(y, 20.0), 2 * index + 2));
            }
            let y = 20.0 * blocks as f32;
            layouts.push((rect(y, 20.0 + grow), 2 * blocks + 1));
            layouts.push((rect(y, 20.0 + grow), 2 * blocks + 2));
            layouts
        };
        let first = PaintArtifact::build_reusing(nodes("last"), &layouts(0.0), 1, None, None, None);
        let runs_before = ARTIFACT_SPLICE_RUN_NODES.with(std::cell::Cell::get);
        let next = PaintArtifact::build_reusing(
            nodes("last, and longer"),
            &layouts(20.0),
            2,
            None,
            None,
            Some(&first),
        );
        let runs = ARTIFACT_SPLICE_RUN_NODES.with(std::cell::Cell::get) - runs_before;
        ARTIFACT_SPLICE_OVERRIDE.with(|flag| flag.set(Some(false)));
        APPEND_REUSE_OVERRIDE.with(|flag| flag.set(Some(false)));
        let fresh = PaintArtifact::build(nodes("last, and longer"), &layouts(20.0), 2);
        assert!(next.append_output_eq(&fresh));
        assert_eq!(
            runs,
            2 * blocks as u64,
            "the unchanged blocks copy as one run"
        );
        // A third frame copies from a build that itself copied a run.
        ARTIFACT_SPLICE_OVERRIDE.with(|flag| flag.set(Some(true)));
        APPEND_REUSE_OVERRIDE.with(|flag| flag.set(None));
        let third = PaintArtifact::build_reusing(
            nodes("last, and longer still"),
            &layouts(40.0),
            3,
            None,
            None,
            Some(&next),
        );
        ARTIFACT_SPLICE_OVERRIDE.with(|flag| flag.set(Some(false)));
        APPEND_REUSE_OVERRIDE.with(|flag| flag.set(Some(false)));
        let fresh = PaintArtifact::build(nodes("last, and longer still"), &layouts(40.0), 3);
        assert!(third.append_output_eq(&fresh));
        ARTIFACT_SPLICE_OVERRIDE.with(|flag| flag.set(None));
        APPEND_REUSE_OVERRIDE.with(|flag| flag.set(None));
    }

    #[test]
    fn inline_ancestor_does_not_replace_percentage_padding_basis() {
        let mut parent_style = Style::default();
        parent_style.display = Display::Block;
        parent_style.padding.left = Spacing::Px(10.0);
        let mut inline_style = Style::default();
        inline_style.display = Display::Inline;
        let mut text_style = Style::default();
        text_style.display = Display::Inline;
        text_style.padding.right = Spacing::Percent(50.0);
        let nodes = vec![
            PaintNode {
                kind: ComponentKind::Row,
                style: Style::default().into(),
                parent: None,
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Row,
                style: parent_style.into(),
                parent: Some(0),
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Row,
                style: inline_style.into(),
                parent: Some(1),
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Text {
                    content: "x".into(),
                },
                style: text_style.into(),
                parent: Some(2),
                sticky_counter_signal: None,
            },
        ];
        let layouts = [
            (
                LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: 800.0,
                    height: 600.0,
                },
                0,
            ),
            (
                LayoutRect {
                    x: 8.0,
                    y: 0.0,
                    width: 106.0,
                    height: 100.0,
                },
                1,
            ),
            (
                LayoutRect {
                    x: 18.0,
                    y: 0.0,
                    width: 20.0,
                    height: 10.0,
                },
                2,
            ),
            (
                LayoutRect {
                    x: 18.0,
                    y: 0.0,
                    width: 20.0,
                    height: 10.0,
                },
                3,
            ),
        ];
        let artifact = PaintArtifact::build(nodes, &layouts, 1);
        assert_eq!(artifact.nodes[3].style.padding_lengths().right, 48.0);
    }

    #[test]
    fn collapsed_column_edges_cover_outer_corner_quadrants() {
        for display in [Display::TableColumn, Display::TableColumnGroup] {
            let style = Style {
                display,
                border_collapse: true,
                ..Style::default()
            };
            let edges = border_edge_paint_rects(
                &style,
                LayoutRect {
                    x: 10.0,
                    y: 53.0,
                    width: 100.0,
                    height: 100.0,
                },
                [4.0; 4],
            );
            for (x, y) in [(8.5, 51.5), (111.5, 51.5), (8.5, 154.5), (111.5, 154.5)] {
                assert!(
                    edges.iter().any(|rect| x >= rect.x
                        && x < rect.x + rect.width
                        && y >= rect.y
                        && y < rect.y + rect.height),
                    "{display:?} leaves the outer corner ({x}, {y}) uncovered"
                );
            }
        }
    }

    fn rect(y: f32) -> LayoutRect {
        LayoutRect {
            x: 0.0,
            y,
            width: 320.0,
            height: 80.0,
        }
    }

    #[test]
    fn overflow_clips_only_block_containers() {
        // `overflow` applies to block containers (CSS 2.1 11.1.1), so an inline
        // box and the internal table boxes must leave their overflowing content
        // visible however the author writes `overflow: hidden`
        // (css/CSS2/ui/overflow-applies-to-008.xht and -001..-004).
        for display in [
            Display::Inline,
            Display::TableRow,
            Display::TableRowGroup,
            Display::TableHeaderGroup,
            Display::TableFooterGroup,
            Display::TableColumn,
            Display::TableColumnGroup,
        ] {
            assert!(
                !establishes_overflow_clip(display),
                "{display:?} is not a block container and must not clip"
            );
        }
        for display in [
            Display::Block,
            Display::FlowRoot,
            Display::Flex,
            Display::Grid,
            Display::InlineBlock,
            Display::InlineFlex,
            Display::ListItem,
            Display::Table,
            Display::InlineTable,
            Display::TableCell,
            Display::TableCaption,
        ] {
            assert!(
                establishes_overflow_clip(display),
                "{display:?} is a block container and must clip"
            );
        }
    }

    #[test]
    fn overflow_clip_region_is_the_padding_box() {
        // The clipping region is the padding box, not the border box, so a
        // border must not let its own width of overflowing content show
        // through: css/CSS2/ui/overflow-applies-to-009.xht leaks exactly the
        // 5 px of `border: 5px solid transparent` without this inset.
        let style = Style {
            border_width: 5.0,
            ..Style::default()
        };
        let border_box = LayoutRect {
            x: 8.0,
            y: 51.2,
            width: 110.0,
            height: 30.0,
        };
        assert_eq!(
            overflow_clip_rect(&style, border_box),
            LayoutRect {
                x: 13.0,
                y: 56.2,
                width: 100.0,
                height: 20.0
            }
        );
    }

    #[test]
    fn overflow_clip_applies_to_the_contents_and_not_to_the_box() {
        // CSS 2.1 11.1.1 scopes an `overflow` clip to "the contents of an
        // element", so a box keeps painting its own border and background while
        // its descendants are clipped. `css/CSS2/normal-flow/negative-margin-001.html`
        // (a `border: 10px solid orange` BFC whose negative margins push it past
        // its parent) and `css/CSS2/positioning/absolute-non-replaced-height-006.xht`
        // (`border: 10px solid black; overflow: auto`) both lost exactly the
        // area of their own border when the box was clipped with its contents.
        let outer = PaintNode {
            kind: ComponentKind::Box,
            style: Style::default().into(),
            parent: None,
            sticky_counter_signal: None,
        };
        let clipping = PaintNode {
            kind: ComponentKind::Box,
            style: (Style {
                overflow: Overflow::Hidden,
                border_width: 10.0,
                ..Style::default()
            })
            .into(),
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let child = PaintNode {
            kind: ComponentKind::Box,
            style: Style::default().into(),
            parent: Some(1),
            sticky_counter_signal: None,
        };
        let artifact = PaintArtifact::build(
            vec![outer, clipping, child],
            &[(rect(0.0), 0), (rect(0.0), 1), (rect(20.0), 2)],
            1,
        );

        let contents = artifact.node_properties[1].clip;
        assert_ne!(contents, 0, "the clipping box hands a clip to its contents");
        assert_eq!(
            artifact.node_properties[2].clip, contents,
            "the contents inherit the overflow clip"
        );
        assert_eq!(
            artifact.self_clip[1], 0,
            "the box paints its own border outside its overflow clip"
        );
        assert_eq!(
            artifact.self_clip[2], contents,
            "a descendant still paints under its parent's overflow clip"
        );
    }

    #[test]
    fn collapsible_inline_whitespace_is_trimmed_at_a_soft_line_start() {
        let mut parent_style = Style::default();
        parent_style.padding.left = w3cos_std::style::Spacing::Px(10.0);
        parent_style.border_left_width = Some(2.0);
        let mut inline_style = Style::default();
        inline_style.display = Display::Inline;
        w3cos_std::inline_text::set_fragment_ends(&mut inline_style, "  next line", &[7, 11]);
        let mut nodes = vec![
            PaintNode {
                kind: ComponentKind::Row,
                style: parent_style.into(),
                parent: None,
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Text {
                    content: "  next line".to_string(),
                },
                style: inline_style.into(),
                parent: Some(0),
                sticky_counter_signal: None,
            },
        ];
        let rects = vec![
            Some(LayoutRect {
                x: 20.0,
                y: 0.0,
                width: 200.0,
                height: 40.0,
            }),
            Some(LayoutRect {
                x: 32.0,
                y: 20.0,
                width: 80.0,
                height: 20.0,
            }),
        ];

        trim_collapsible_inline_whitespace_at_line_start(&mut nodes, &rects);
        assert!(matches!(
            &nodes[1].kind,
            ComponentKind::Text { content } if content == "next line"
        ));
        assert_eq!(
            w3cos_std::inline_text::fragment_ends("next line", &nodes[1].style),
            Some(vec![5, 9]),
            "line-start trimming must rebase authored shaping boundaries"
        );

        nodes[1].style.white_space = WhiteSpace::Pre;
        nodes[1].kind = ComponentKind::Text {
            content: "  preserved".to_string(),
        };
        trim_collapsible_inline_whitespace_at_line_start(&mut nodes, &rects);
        assert!(matches!(
            &nodes[1].kind,
            ComponentKind::Text { content } if content == "  preserved"
        ));
    }

    #[test]
    fn nested_inline_leading_space_is_not_trimmed_mid_line() {
        let mut inline_style = Style::default();
        inline_style.display = Display::Inline;
        let mut nodes = vec![
            PaintNode {
                kind: ComponentKind::Row,
                style: Style::default().into(),
                parent: None,
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Row,
                style: inline_style.clone().into(),
                parent: Some(0),
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Text {
                    content: " X".into(),
                },
                style: inline_style.into(),
                parent: Some(1),
                sticky_counter_signal: None,
            },
        ];
        let mut rects = vec![
            Some(LayoutRect {
                x: 28.0,
                y: 0.0,
                width: 744.0,
                height: 20.0,
            }),
            Some(LayoutRect {
                x: 208.0,
                y: 0.0,
                width: 40.0,
                height: 20.0,
            }),
            Some(LayoutRect {
                x: 208.0,
                y: 0.0,
                width: 40.0,
                height: 20.0,
            }),
        ];

        trim_collapsible_inline_whitespace_at_line_start(&mut nodes, &rects);
        assert!(matches!(&nodes[2].kind, ComponentKind::Text { content } if content == " X"));

        for rect in &mut rects[1..] {
            rect.as_mut().unwrap().x = 28.0;
        }
        trim_collapsible_inline_whitespace_at_line_start(&mut nodes, &rects);
        assert!(matches!(&nodes[2].kind, ComponentKind::Text { content } if content == "X"));
    }

    #[test]
    fn explicit_inline_fragment_clip_keeps_layout_rect_and_clips_only_paint() {
        let mut style = Style::default();
        style.display = w3cos_std::style::Display::Inline;
        style.font_size = 20.0;
        style.line_height = 1.0;
        style
            .custom_properties
            .get_or_insert_with(Default::default)
            .insert(
                "--w3cos-internal-inline-fragment-clip".into(),
                "bottom 20".into(),
            );
        let layout = LayoutRect {
            x: 12.0,
            y: 40.0,
            width: 100.0,
            height: 80.0,
        };
        let text = ComponentKind::Text {
            content: "fragment".to_string(),
        };

        assert_eq!(
            inline_fragment_clip_rect(&text, &style, layout),
            Some(LayoutRect {
                x: 12.0,
                y: 100.0,
                width: 100.0,
                height: 20.0,
            })
        );
        assert_eq!(
            inline_fragment_clip_rect(
                &ComponentKind::Image {
                    src: "100x100.png".to_string(),
                },
                &style,
                layout,
            ),
            None,
            "vertical alignment must not crop a replaced element to the line-height strut"
        );
    }

    #[test]
    fn top_aligned_decorated_inline_keeps_its_vertical_border_paint() {
        let style = Style {
            display: Display::Inline,
            align_self: w3cos_std::style::AlignSelf::FlexStart,
            border_top_width: Some(4.0),
            border_bottom_width: Some(4.0),
            font_size: 16.0,
            line_height: 1.2,
            ..Style::default()
        };
        let rect = LayoutRect {
            x: 8.0,
            y: 5.6,
            width: 16.0,
            height: 24.0,
        };
        assert_eq!(
            inline_fragment_clip_rect(&ComponentKind::Row, &style, rect),
            None
        );
    }

    #[test]
    fn css2_clip_rect_uses_positioned_box_local_coordinates() {
        let style = Style {
            position: Position::Absolute,
            clip: Some(w3cos_std::style::CssClipRect {
                top: Some(w3cos_std::style::Dimension::Px(10.0)),
                right: Some(w3cos_std::style::Dimension::Px(70.0)),
                bottom: Some(w3cos_std::style::Dimension::Px(50.0)),
                left: Some(w3cos_std::style::Dimension::Px(20.0)),
            }),
            ..Style::default()
        };
        assert_eq!(
            css2_clip_rect(
                &style,
                LayoutRect {
                    x: 100.0,
                    y: 200.0,
                    width: 90.0,
                    height: 80.0,
                }
            ),
            Some(LayoutRect {
                x: 120.0,
                y: 210.0,
                width: 50.0,
                height: 40.0,
            })
        );
    }

    #[test]
    fn improper_table_part_nested_in_cell_has_no_independent_background() {
        let mut nodes = vec![
            PaintNode {
                kind: ComponentKind::Box,
                style: (Style {
                    display: Display::Table,
                    ..Style::default()
                })
                .into(),
                parent: None,
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Box,
                style: (Style {
                    display: Display::TableCell,
                    ..Style::default()
                })
                .into(),
                parent: Some(0),
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Box,
                style: (Style {
                    display: Display::TableRowGroup,
                    background: Color::rgb(255, 0, 0),
                    ..Style::default()
                })
                .into(),
                parent: Some(1),
                sticky_counter_signal: None,
            },
        ];
        suppress_improper_nested_table_part_backgrounds(&mut nodes);
        assert_eq!(nodes[2].style.background, Color::TRANSPARENT);
    }

    #[test]
    fn empty_cells_hide_suppresses_separate_cell_background_and_border() {
        let mut nodes = vec![PaintNode {
            kind: ComponentKind::Box,
            style: (Style {
                display: Display::TableCell,
                empty_cells_hide: true,
                background: Color::rgb(255, 0, 0),
                border_width: 5.0,
                ..Style::default()
            })
            .into(),
            parent: None,
            sticky_counter_signal: None,
        }];
        suppress_hidden_empty_cell_paint(&mut nodes);
        assert_eq!(nodes[0].style.background, Color::TRANSPARENT);
        assert_eq!(nodes[0].style.border_width, 0.0);
    }

    #[test]
    fn collapsed_column_color_defers_to_cell_and_row_owners() {
        let green = Color::rgb(0, 128, 0);
        let red = Color::rgb(255, 0, 0);
        let node = |display, width, color, parent| PaintNode {
            kind: ComponentKind::Box,
            style: (Style {
                display,
                border_collapse: true,
                border_width: width,
                border_color: color,
                ..Style::default()
            })
            .into(),
            parent,
            sticky_counter_signal: None,
        };
        for (cell_owner, display) in [
            (false, Display::TableColumn),
            (true, Display::TableColumn),
            (false, Display::TableColumnGroup),
            (true, Display::TableColumnGroup),
        ] {
            let mut nodes = vec![
                node(Display::Table, 0.0, red, None),
                node(display, 25.0, red, Some(0)),
                node(
                    Display::TableRow,
                    if cell_owner { 0.0 } else { 25.0 },
                    green,
                    Some(0),
                ),
                node(
                    Display::TableCell,
                    if cell_owner { 25.0 } else { 0.0 },
                    green,
                    Some(2),
                ),
            ];
            resolve_collapsed_cell_border_conflicts(&mut nodes);
            assert_eq!(
                [
                    nodes[1].style.border_top_width,
                    nodes[1].style.border_right_width,
                    nodes[1].style.border_bottom_width,
                    nodes[1].style.border_left_width
                ],
                [Some(0.0); 4],
                "cell owner: {cell_owner}"
            );
            assert_eq!(
                [
                    nodes[3].style.border_top_color,
                    nodes[3].style.border_right_color,
                    nodes[3].style.border_bottom_color,
                    nodes[3].style.border_left_color
                ],
                [Some(green); 4],
                "cell owner: {cell_owner}"
            );
        }
    }

    #[test]
    fn hidden_table_parts_suppress_solid_cell_paint_edges() {
        use w3cos_std::style::BorderLineStyle;
        let node = |display, parent| PaintNode {
            kind: ComponentKind::Box,
            style: (Style {
                display,
                border_collapse: true,
                ..Style::default()
            })
            .into(),
            parent,
            sticky_counter_signal: None,
        };
        for display in [
            Display::TableRow,
            Display::TableRowGroup,
            Display::TableColumn,
            Display::TableColumnGroup,
        ] {
            let mut nodes = vec![node(Display::Table, None)];
            let cell;
            if display == Display::TableRow {
                nodes.push(node(display, Some(0)));
                cell = 2;
                nodes.push(node(Display::TableCell, Some(1)));
            } else if display == Display::TableRowGroup {
                nodes.push(node(display, Some(0)));
                nodes.push(node(Display::TableRow, Some(1)));
                cell = 3;
                nodes.push(node(Display::TableCell, Some(2)));
            } else {
                nodes.push(node(display, Some(0)));
                nodes.push(node(Display::TableRow, Some(0)));
                cell = 3;
                nodes.push(node(Display::TableCell, Some(2)));
            }
            nodes[1].style.border_styles = [Some(BorderLineStyle::Hidden); 4];
            nodes[cell].style.border_styles = [Some(BorderLineStyle::Solid); 4];
            nodes[cell].style.border_width = 3.0;
            nodes[cell].style.border_color = Color::rgb(255, 0, 0);
            resolve_collapsed_cell_border_conflicts(&mut nodes);
            let style = &nodes[cell].style;
            assert_eq!(
                [
                    style.border_top_width,
                    style.border_right_width,
                    style.border_bottom_width,
                    style.border_left_width
                ],
                [Some(0.0); 4],
                "{display:?}"
            );
        }
    }

    #[test]
    fn collapsed_part_edges_preserve_owner_priority_and_nested_table_boundary() {
        let green = Color::rgb(0, 128, 0);
        let red = Color::rgb(255, 0, 0);
        let blue = Color::rgb(0, 0, 255);
        let node = |display, border_width, border_color, parent| PaintNode {
            kind: ComponentKind::Box,
            style: (Style {
                display,
                border_collapse: true,
                border_width,
                border_color,
                ..Style::default()
            })
            .into(),
            parent,
            sticky_counter_signal: None,
        };
        let mut nodes = vec![
            node(Display::Table, 0.0, green, None),
            node(Display::TableRowGroup, 20.0, green, Some(0)),
            node(Display::TableRow, 20.0, red, Some(1)),
            node(Display::TableCell, 10.0, blue, Some(2)),
            node(Display::Table, 0.0, green, Some(1)),
            node(Display::TableRow, 0.0, green, Some(4)),
            node(Display::TableCell, 10.0, blue, Some(5)),
        ];
        nodes[3].style.border_top_width = Some(20.0);
        nodes[3].style.border_top_color = Some(blue);
        resolve_collapsed_cell_border_conflicts(&mut nodes);
        assert_eq!(nodes[3].style.border_top_color, Some(blue));
        assert_eq!(nodes[3].style.border_bottom_color, Some(red));
        assert_eq!(nodes[3].style.border_left_color, Some(red));
        assert_eq!(nodes[3].style.border_left_width, Some(20.0));
        assert_eq!(nodes[1].style.border_left_width, Some(0.0));
        assert_eq!(nodes[2].style.border_bottom_width, Some(0.0));
        assert_eq!(nodes[6].style.border_left_width, Some(10.0));
        assert_eq!(nodes[6].style.border_left_color, Some(blue));
    }

    #[test]
    fn collapsed_colspan_border_resolves_every_overlapping_track() {
        use w3cos_std::style::BorderLineStyle;
        let node = |display, parent, line_style| PaintNode {
            kind: ComponentKind::Box,
            style: (Style {
                display,
                border_collapse: true,
                border_width: if display == Display::TableCell {
                    10.0
                } else {
                    0.0
                },
                border_color: Color::rgb(0, 128, 0),
                border_styles: [Some(line_style); 4],
                ..Style::default()
            })
            .into(),
            parent,
            sticky_counter_signal: None,
        };
        let mut nodes = vec![
            node(Display::Table, None, BorderLineStyle::Solid),
            node(Display::TableRow, Some(0), BorderLineStyle::Solid),
            node(Display::TableCell, Some(1), BorderLineStyle::Outset),
            node(Display::TableCell, Some(1), BorderLineStyle::Outset),
            node(Display::TableCell, Some(1), BorderLineStyle::Outset),
            node(Display::TableRow, Some(0), BorderLineStyle::Solid),
            node(Display::TableCell, Some(5), BorderLineStyle::Solid),
            node(Display::TableRow, Some(0), BorderLineStyle::Solid),
            node(Display::TableCell, Some(7), BorderLineStyle::Outset),
            node(Display::TableCell, Some(7), BorderLineStyle::Outset),
            node(Display::TableCell, Some(7), BorderLineStyle::Outset),
        ];
        nodes[6]
            .style
            .custom_properties
            .get_or_insert_with(Default::default)
            .insert("--w3cos-internal-table-column-span".into(), "3".into());
        resolve_collapsed_cell_border_conflicts(&mut nodes);
        for cell in [2, 3, 4] {
            assert_eq!(
                nodes[cell].style.border_bottom_color,
                Some(Color::TRANSPARENT),
                "upper track cell {cell} yields to the solid spanning cell"
            );
        }
        for cell in [8, 9, 10] {
            assert_eq!(
                nodes[cell].style.border_top_color,
                Some(Color::TRANSPARENT),
                "lower track cell {cell} yields to the solid spanning cell"
            );
        }
        assert_ne!(nodes[6].style.border_bottom_color, Some(Color::TRANSPARENT));
    }

    #[test]
    fn collapsed_rowspan_border_owns_neighbors_in_every_occupied_row() {
        use w3cos_std::style::BorderLineStyle;
        let node = |display, parent, line_style| PaintNode {
            kind: ComponentKind::Box,
            style: (Style {
                display,
                border_collapse: true,
                border_width: if display == Display::TableCell {
                    10.0
                } else {
                    0.0
                },
                border_color: Color::rgb(0, 128, 0),
                border_styles: [Some(line_style); 4],
                ..Style::default()
            })
            .into(),
            parent,
            sticky_counter_signal: None,
        };
        let mut nodes = vec![
            node(Display::Table, None, BorderLineStyle::Solid),
            node(Display::TableRow, Some(0), BorderLineStyle::Solid),
            node(Display::TableCell, Some(1), BorderLineStyle::Outset),
            node(Display::TableCell, Some(1), BorderLineStyle::Solid),
            node(Display::TableCell, Some(1), BorderLineStyle::Outset),
            node(Display::TableRow, Some(0), BorderLineStyle::Solid),
            node(Display::TableCell, Some(5), BorderLineStyle::Outset),
            node(Display::TableCell, Some(5), BorderLineStyle::Outset),
            node(Display::TableRow, Some(0), BorderLineStyle::Solid),
            node(Display::TableCell, Some(8), BorderLineStyle::Outset),
            node(Display::TableCell, Some(8), BorderLineStyle::Outset),
        ];
        nodes[3]
            .style
            .custom_properties
            .get_or_insert_with(Default::default)
            .insert(crate::table_grid::ROW_SPAN.into(), "3".into());
        resolve_collapsed_cell_border_conflicts(&mut nodes);
        for cell in [2, 6, 9] {
            assert_eq!(
                nodes[cell].style.border_right_color,
                Some(Color::TRANSPARENT),
                "left neighbor {cell} yields to the solid rowspan edge"
            );
        }
        for cell in [4, 7, 10] {
            assert_eq!(
                nodes[cell].style.border_left_color,
                Some(Color::TRANSPARENT)
            );
        }
        assert_ne!(nodes[3].style.border_left_color, Some(Color::TRANSPARENT));
        assert_ne!(nodes[3].style.border_right_color, Some(Color::TRANSPARENT));
    }

    #[test]
    fn collapsed_part_style_priority_transfers_the_winning_line_style() {
        use w3cos_std::style::BorderLineStyle;
        let node = |display, parent, width, line_style| PaintNode {
            kind: ComponentKind::Box,
            style: (Style {
                display,
                border_collapse: true,
                border_width: width,
                border_color: Color::rgb(0, 128, 0),
                border_styles: [Some(line_style); 4],
                ..Style::default()
            })
            .into(),
            parent,
            sticky_counter_signal: None,
        };
        for part_display in [
            Display::Table,
            Display::TableRow,
            Display::TableRowGroup,
            Display::TableColumn,
            Display::TableColumnGroup,
        ] {
            let mut nodes = vec![node(Display::Table, None, 0.0, BorderLineStyle::Solid)];
            let cell;
            if part_display == Display::Table {
                nodes[0].style.border_width = 10.0;
                nodes.push(node(
                    Display::TableRow,
                    Some(0),
                    0.0,
                    BorderLineStyle::Solid,
                ));
                cell = 2;
                nodes.push(node(
                    Display::TableCell,
                    Some(1),
                    10.0,
                    BorderLineStyle::Outset,
                ));
            } else if part_display == Display::TableRow {
                nodes.push(node(part_display, Some(0), 10.0, BorderLineStyle::Solid));
                cell = 2;
                nodes.push(node(
                    Display::TableCell,
                    Some(1),
                    10.0,
                    BorderLineStyle::Outset,
                ));
            } else {
                nodes.push(node(part_display, Some(0), 10.0, BorderLineStyle::Solid));
                let parent = if part_display == Display::TableRowGroup {
                    1
                } else {
                    0
                };
                nodes.push(node(
                    Display::TableRow,
                    Some(parent),
                    0.0,
                    BorderLineStyle::Solid,
                ));
                cell = 3;
                nodes.push(node(
                    Display::TableCell,
                    Some(2),
                    10.0,
                    BorderLineStyle::Outset,
                ));
            }
            resolve_collapsed_cell_border_conflicts(&mut nodes);
            assert_eq!(
                nodes[cell].style.border_styles,
                [Some(BorderLineStyle::Solid); 4],
                "same-width solid {part_display:?} wins over cell outset and supplies the ink style"
            );
        }
    }

    #[test]
    fn collapsed_grid_joints_use_neighbor_owner_and_extend_winning_edges() {
        use w3cos_std::style::BorderLineStyle;
        let node = |display, parent| PaintNode {
            kind: ComponentKind::Box,
            style: (Style {
                display,
                border_collapse: true,
                border_width: if display == Display::TableCell {
                    10.0
                } else {
                    0.0
                },
                border_color: Color::rgb(0, 128, 0),
                border_styles: [Some(BorderLineStyle::Groove); 4],
                ..Style::default()
            })
            .into(),
            parent,
            sticky_counter_signal: None,
        };
        let mut nodes = vec![
            node(Display::Table, None),
            node(Display::TableRow, Some(0)),
            node(Display::TableCell, Some(1)),
            node(Display::TableCell, Some(1)),
        ];
        let rects = vec![
            None,
            None,
            Some(LayoutRect {
                x: 20.0,
                y: 30.0,
                width: 40.0,
                height: 30.0,
            }),
            Some(LayoutRect {
                x: 60.0,
                y: 30.0,
                width: 40.0,
                height: 30.0,
            }),
        ];
        resolve_collapsed_cell_border_conflicts(&mut nodes);
        annotate_collapsed_border_joints(&mut nodes, &rects);
        let first = border_edge_paint_rects(&nodes[2].style, rects[2].unwrap(), [10.0; 4]);
        let second = border_edge_paint_rects(&nodes[3].style, rects[3].unwrap(), [10.0; 4]);
        assert_eq!(
            (first[0].x, first[0].width),
            (15.0, 50.0),
            "winning horizontal edge fills both joint quadrants"
        );
        assert_eq!((first[1].y, first[1].height), (25.0, 40.0));
        assert_eq!(
            (second[0].x, second[0].width),
            (65.0, 40.0),
            "later cell's horizontal edge yields its start joint to the earlier owner"
        );
    }

    #[test]
    fn collapsed_wider_inline_edge_owns_the_block_edge_junction() {
        let style = Style {
            display: Display::TableCell,
            border_collapse: true,
            ..Style::default()
        };
        let rect = LayoutRect {
            x: 20.0,
            y: 30.0,
            width: 40.0,
            height: 30.0,
        };
        let edges = border_edge_paint_rects(&style, rect, [10.0, 11.0, 10.0, 11.0]);
        // The wider inline borders win at both horizontal junctions, rather
        // than letting the later-painted bottom edge cover their inner halves.
        for horizontal in [edges[0], edges[2]] {
            assert_eq!(horizontal.x, 25.5);
            assert_eq!(horizontal.width, 29.0);
        }
    }

    #[test]
    fn collapsed_equal_width_border_prefers_solid_over_outset() {
        use w3cos_std::style::BorderLineStyle;
        let node = |display, parent, line_style| PaintNode {
            kind: ComponentKind::Box,
            style: (Style {
                display,
                border_collapse: true,
                border_width: 10.0,
                border_color: Color::rgb(0, 128, 0),
                border_styles: [Some(line_style); 4],
                ..Style::default()
            })
            .into(),
            parent,
            sticky_counter_signal: None,
        };
        let mut nodes = vec![
            node(Display::TableRow, None, BorderLineStyle::None),
            node(Display::TableCell, Some(0), BorderLineStyle::Outset),
            node(Display::TableCell, Some(0), BorderLineStyle::Solid),
        ];
        nodes[0].style.border_width = 0.0;
        resolve_collapsed_cell_border_conflicts(&mut nodes);
        assert_eq!(
            nodes[1].style.border_right_color,
            Some(Color::TRANSPARENT),
            "equal-width solid must win before directional cell-owner precedence"
        );
        assert_eq!(
            nodes[2].style.border_left_color,
            Some(Color::rgb(0, 128, 0))
        );
    }

    #[test]
    fn collapsed_equal_inline_borders_prefer_the_start_cell() {
        let start_color = Color::rgb(0, 128, 0);
        let end_color = Color::rgb(255, 0, 0);
        let mut nodes = vec![
            PaintNode {
                kind: ComponentKind::Box,
                style: (Style {
                    display: Display::TableRow,
                    border_collapse: true,
                    ..Style::default()
                })
                .into(),
                parent: None,
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Box,
                style: (Style {
                    display: Display::TableCell,
                    border_right_width: Some(20.0),
                    border_right_color: Some(start_color),
                    ..Style::default()
                })
                .into(),
                parent: Some(0),
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Box,
                style: (Style {
                    display: Display::TableCell,
                    border_left_width: Some(20.0),
                    border_left_color: Some(end_color),
                    ..Style::default()
                })
                .into(),
                parent: Some(0),
                sticky_counter_signal: None,
            },
        ];

        resolve_collapsed_cell_border_conflicts(&mut nodes);

        assert_eq!(nodes[1].style.border_right_color, Some(start_color));
        assert_eq!(nodes[2].style.border_left_width, Some(20.0));
        assert_eq!(nodes[2].style.border_left_color, Some(Color::TRANSPARENT));
        assert_eq!(
            box_background_paint_rect(
                &nodes[2].style,
                LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: 100.0,
                    height: 40.0,
                },
            ),
            LayoutRect {
                x: 20.0,
                y: 0.0,
                width: 80.0,
                height: 40.0,
            }
        );
    }

    #[test]
    fn collapsed_explicit_transparent_border_remains_transparent() {
        let mut nodes = vec![
            PaintNode {
                kind: ComponentKind::Box,
                style: (Style {
                    display: Display::TableRow,
                    border_collapse: true,
                    ..Style::default()
                })
                .into(),
                parent: None,
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Box,
                style: (Style {
                    display: Display::TableCell,
                    color: Color::BLACK,
                    border_right_width: Some(2.0),
                    border_color: Color::TRANSPARENT,
                    ..Style::default()
                })
                .into(),
                parent: Some(0),
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Box,
                style: (Style {
                    display: Display::TableCell,
                    color: Color::BLACK,
                    border_left_width: Some(2.0),
                    border_color: Color::TRANSPARENT,
                    ..Style::default()
                })
                .into(),
                parent: Some(0),
                sticky_counter_signal: None,
            },
        ];

        resolve_collapsed_cell_border_conflicts(&mut nodes);

        assert_eq!(nodes[1].style.border_right_color, Some(Color::TRANSPARENT));
        assert_eq!(nodes[2].style.border_left_color, Some(Color::TRANSPARENT));
    }

    #[test]
    fn collapsed_cell_visual_bounds_include_centered_inline_borders() {
        let node = PaintNode {
            kind: ComponentKind::Box,
            style: (Style {
                display: Display::TableCell,
                border_collapse: true,
                border_left_width: Some(25.0),
                border_right_width: Some(25.0),
                ..Style::default()
            })
            .into(),
            parent: None,
            sticky_counter_signal: None,
        };
        let rect = LayoutRect {
            x: 12.5,
            y: 0.0,
            width: 75.0,
            height: 100.0,
        };
        let artifact = PaintArtifact::build(vec![node], &[(rect, 0)], 1);
        assert_eq!(
            artifact.display_items[0].visual_rect,
            LayoutRect {
                x: 0.0,
                width: 100.0,
                ..rect
            }
        );
        assert_eq!(artifact.rect_by_index[0], Some(rect));
    }

    #[test]
    fn collapsed_table_background_origin_uses_resolved_outer_edges() {
        let node = |display, border_width, parent| PaintNode {
            kind: ComponentKind::Box,
            style: (Style {
                display,
                border_collapse: true,
                border_width,
                border_color: Color::rgb(0, 128, 0),
                ..Style::default()
            })
            .into(),
            parent,
            sticky_counter_signal: None,
        };
        let mut nodes = vec![
            node(Display::Table, 2.0, None),
            node(Display::TableRow, 0.0, Some(0)),
            node(Display::TableCell, 6.0, Some(1)),
        ];
        resolve_collapsed_cell_border_conflicts(&mut nodes);
        let rect = LayoutRect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        };
        assert_eq!(
            box_background_positioning_rect(&nodes[0].style, rect),
            Some(LayoutRect {
                x: 3.0,
                y: 3.0,
                width: 94.0,
                height: 94.0
            })
        );
        assert_eq!(nodes[0].style.border_left_color, Some(Color::TRANSPARENT));
        assert_eq!(
            nodes[2].style.border_left_color,
            Some(Color::rgb(0, 128, 0))
        );
    }

    #[test]
    fn collapsed_cell_background_retains_centered_inline_grid_bounds() {
        let style = Style {
            display: Display::TableCell,
            border_collapse: true,
            border_top_width: Some(4.0),
            border_right_width: Some(2.0),
            border_bottom_width: Some(4.0),
            border_left_width: Some(2.0),
            ..Style::default()
        };

        assert_eq!(
            box_background_paint_rect(
                &style,
                LayoutRect {
                    // Both axes now use shared grid-line centers.
                    x: 138.0,
                    y: 55.0,
                    width: 57.0,
                    height: 19.0,
                },
            ),
            LayoutRect {
                x: 138.0,
                y: 55.0,
                width: 57.0,
                height: 19.0,
            }
        );
    }

    #[test]
    fn collapsed_row_projects_visibility_to_its_cells() {
        let mut nodes = vec![
            PaintNode {
                kind: ComponentKind::Box,
                style: (Style {
                    display: Display::TableRow,
                    visibility: Visibility::Collapse,
                    ..Style::default()
                })
                .into(),
                parent: None,
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Box,
                style: (Style {
                    display: Display::TableCell,
                    visibility: Visibility::Visible,
                    ..Style::default()
                })
                .into(),
                parent: Some(0),
                sticky_counter_signal: None,
            },
        ];

        project_collapsed_table_tracks_to_cells(&mut nodes);

        assert_eq!(nodes[1].style.visibility, Visibility::Collapse);
    }

    #[test]
    fn collapsed_columns_preserve_partial_span_overflow_and_hide_full_cells() {
        let node = |display, visibility, parent, span: Option<&str>| PaintNode {
            kind: ComponentKind::Box,
            style: (Style {
                display,
                visibility,
                custom_properties: span.map(|span| {
                    w3cos_std::style::CustomProperties::from_iter([(
                        "--w3cos-internal-table-column-span".to_string(),
                        span.to_string(),
                    )])
                }),
                ..Style::default()
            })
            .into(),
            parent,
            sticky_counter_signal: None,
        };
        let mut nodes = vec![
            node(Display::Table, Visibility::Visible, None, None),
            node(Display::TableColumn, Visibility::Visible, Some(0), None),
            node(Display::TableColumn, Visibility::Collapse, Some(0), None),
            node(Display::TableColumn, Visibility::Visible, Some(0), None),
            node(Display::TableRow, Visibility::Visible, Some(0), None),
            node(Display::TableCell, Visibility::Visible, Some(4), Some("2")),
            node(Display::TableCell, Visibility::Visible, Some(4), None),
            node(Display::TableRow, Visibility::Visible, Some(0), None),
            node(Display::TableCell, Visibility::Visible, Some(7), None),
            node(Display::TableCell, Visibility::Visible, Some(7), None),
            node(Display::TableCell, Visibility::Visible, Some(7), None),
        ];

        project_collapsed_table_tracks_to_cells(&mut nodes);

        assert_eq!(nodes[5].style.visibility, Visibility::Visible);
        assert_eq!(
            nodes[5].style.overflow_x, None,
            "Chromium preserves authored overflow for partially collapsed spans"
        );
        assert_eq!(nodes[9].style.visibility, Visibility::Collapse);
        assert_eq!(nodes[9].style.overflow_x, Some(Overflow::Hidden));
        assert_eq!(nodes[9].style.overflow_y, Some(Overflow::Hidden));
        nodes[5].style.overflow_x = Some(Overflow::Hidden);
        project_collapsed_table_tracks_to_cells(&mut nodes);
        assert_eq!(
            nodes[5].style.overflow_x,
            Some(Overflow::Hidden),
            "explicit author clipping is not removed"
        );
    }

    #[test]
    fn table_background_paint_rect_uses_grid_width_not_wider_caption() {
        for bottom in [false, true] {
            let mut nodes = vec![
                PaintNode {
                    kind: ComponentKind::Box,
                    style: (Style {
                        display: Display::Table,
                        ..Style::default()
                    })
                    .into(),
                    parent: None,
                    sticky_counter_signal: None,
                },
                PaintNode {
                    kind: ComponentKind::Box,
                    style: (Style {
                        display: Display::TableCaption,
                        caption_side_bottom: bottom,
                        ..Style::default()
                    })
                    .into(),
                    parent: Some(0),
                    sticky_counter_signal: None,
                },
                PaintNode {
                    kind: ComponentKind::Box,
                    style: (Style {
                        display: Display::TableRowGroup,
                        ..Style::default()
                    })
                    .into(),
                    parent: Some(0),
                    sticky_counter_signal: None,
                },
            ];
            let grid_y = if bottom { 0.0 } else { 30.0 };
            let rects = vec![
                Some(LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: 192.0,
                    height: 60.0,
                }),
                Some(LayoutRect {
                    x: 0.0,
                    y: if bottom { 30.0 } else { 0.0 },
                    width: 192.0,
                    height: 30.0,
                }),
                Some(LayoutRect {
                    x: 0.0,
                    y: grid_y,
                    width: 100.0,
                    height: 30.0,
                }),
            ];
            annotate_table_caption_paint_insets(&mut nodes, &rects);
            assert_eq!(
                table_grid_paint_rect(&nodes[0].style, rects[0].unwrap()),
                LayoutRect {
                    x: 0.0,
                    y: grid_y,
                    width: 100.0,
                    height: 30.0
                }
            );
        }
    }

    #[test]
    fn empty_top_caption_margin_is_inside_table_wrapper_overflow_clip() {
        let node = |display, parent: Option<usize>| PaintNode {
            kind: ComponentKind::Box,
            style: (Style {
                display,
                overflow: if parent.is_none() {
                    Overflow::Hidden
                } else {
                    Overflow::Visible
                },
                ..Style::default()
            })
            .into(),
            parent,
            sticky_counter_signal: None,
        };
        let artifact = PaintArtifact::build(
            vec![
                node(Display::Table, None),
                node(Display::TableCaption, Some(0)),
                node(Display::TableRow, Some(0)),
                node(Display::TableCell, Some(2)),
                node(Display::Block, Some(3)),
            ],
            &[
                (
                    LayoutRect {
                        x: 8.0,
                        y: 51.0,
                        width: 20.0,
                        height: 30.0,
                    },
                    0,
                ),
                (
                    LayoutRect {
                        x: 8.0,
                        y: 51.0,
                        width: 20.0,
                        height: 0.0,
                    },
                    1,
                ),
                (
                    LayoutRect {
                        x: 8.0,
                        y: 61.0,
                        width: 20.0,
                        height: 20.0,
                    },
                    2,
                ),
                (
                    LayoutRect {
                        x: 8.0,
                        y: 61.0,
                        width: 20.0,
                        height: 20.0,
                    },
                    3,
                ),
                (
                    LayoutRect {
                        x: 8.0,
                        y: 46.0,
                        width: 20.0,
                        height: 35.0,
                    },
                    4,
                ),
            ],
            1,
        );
        let clip = artifact.node_properties[0].clip;
        // Chrome155 V4149 clips contents at the wrapper even when an empty
        // caption's margin moves the row down. Background still uses the grid.
        assert_eq!(
            artifact.properties.clips[clip].rect,
            artifact.rect_by_index[0]
        );
        assert_eq!(
            table_grid_paint_rect(&artifact.nodes[0].style, artifact.rect_by_index[0].unwrap()).y,
            61.0
        );
        assert_eq!(
            artifact.node_properties[1].clip, 0,
            "caption lives outside table grid"
        );
        assert_eq!(artifact.node_properties[4].clip, clip);
    }

    #[test]
    fn table_grid_border_tracks_a_row_overlapped_by_the_top_caption() {
        let mut nodes = vec![
            PaintNode {
                kind: ComponentKind::Box,
                style: (Style {
                    display: Display::Table,
                    border_top_width: Some(16.0),
                    ..Style::default()
                })
                .into(),
                parent: None,
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Box,
                style: (Style {
                    display: Display::TableCaption,
                    ..Style::default()
                })
                .into(),
                parent: Some(0),
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Box,
                style: (Style {
                    display: Display::TableRow,
                    ..Style::default()
                })
                .into(),
                parent: Some(0),
                sticky_counter_signal: None,
            },
        ];
        let rects = vec![
            Some(LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 200.0,
                height: 119.2,
            }),
            Some(LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 200.0,
                height: 22.4,
            }),
            Some(LayoutRect {
                x: 0.0,
                y: 19.2,
                width: 200.0,
                height: 100.0,
            }),
        ];
        annotate_table_caption_paint_insets(&mut nodes, &rects);
        let grid = table_grid_paint_rect(&nodes[0].style, rects[0].unwrap());
        assert!(
            (grid.y - 3.2).abs() < 0.01,
            "grid border precedes the first row: {grid:?}"
        );
    }

    #[test]
    fn table_background_paint_rect_excludes_bottom_caption() {
        let nodes = vec![
            PaintNode {
                kind: ComponentKind::Box,
                style: (Style {
                    display: Display::Table,
                    background: Color::rgb(0, 0, 255),
                    ..Style::default()
                })
                .into(),
                parent: None,
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Box,
                style: (Style {
                    display: Display::TableCaption,
                    caption_side_bottom: true,
                    ..Style::default()
                })
                .into(),
                parent: Some(0),
                sticky_counter_signal: None,
            },
        ];
        let artifact = PaintArtifact::build(
            nodes,
            &[
                (
                    LayoutRect {
                        x: 0.0,
                        y: 0.0,
                        width: 192.0,
                        height: 115.0,
                    },
                    0,
                ),
                (
                    LayoutRect {
                        x: 0.0,
                        y: 96.0,
                        width: 192.0,
                        height: 19.0,
                    },
                    1,
                ),
            ],
            1,
        );
        assert_eq!(
            table_grid_paint_rect(&artifact.nodes[0].style, artifact.rect_by_index[0].unwrap()),
            LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 192.0,
                height: 96.0
            }
        );
    }

    #[test]
    fn separated_row_group_background_excludes_cell_spacing() {
        let mut nodes = vec![
            PaintNode {
                kind: ComponentKind::Box,
                style: (Style {
                    display: Display::Table,
                    border_collapse: false,
                    ..Style::default()
                })
                .into(),
                parent: None,
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Box,
                style: (Style {
                    display: Display::TableRowGroup,
                    background_image: Some("blue.png".into()),
                    ..Style::default()
                })
                .into(),
                parent: Some(0),
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Box,
                style: (Style {
                    display: Display::TableRow,
                    ..Style::default()
                })
                .into(),
                parent: Some(1),
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Box,
                style: (Style {
                    display: Display::TableCell,
                    ..Style::default()
                })
                .into(),
                parent: Some(2),
                sticky_counter_signal: None,
            },
        ];
        let row_group = LayoutRect {
            x: 3.0,
            y: 3.0,
            width: 96.0,
            height: 96.0,
        };
        let rects = vec![
            Some(LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 102.0,
                height: 102.0,
            }),
            Some(row_group),
            Some(row_group),
            Some(LayoutRect {
                x: 3.0,
                y: 3.0,
                width: 90.0,
                height: 96.0,
            }),
        ];

        annotate_separated_table_background_fragments(&mut nodes, &rects);

        assert!(
            nodes[1]
                .style
                .custom_properties
                .as_ref()
                .is_some_and(|properties| properties.contains_key(TABLE_BACKGROUND_FRAGMENTS))
        );
        assert_eq!(
            box_background_paint_rects(&nodes[1].style, row_group),
            vec![LayoutRect {
                width: 90.0,
                ..row_group
            }]
        );
    }

    #[test]
    fn first_line_internal_clip_uses_the_originating_line_height() {
        let mut style = Style::default();
        style.display = w3cos_std::style::Display::Inline;
        style.font_size = 100.0;
        style
            .custom_properties
            .get_or_insert_with(Default::default)
            .insert(
                "--w3cos-internal-inline-fragment-clip".to_string(),
                "top 60".to_string(),
            );
        let layout = LayoutRect {
            x: 0.0,
            y: 10.0,
            width: 80.0,
            height: 100.0,
        };
        let text = ComponentKind::Text {
            content: "fragment".to_string(),
        };

        assert_eq!(
            inline_fragment_clip_rect(&text, &style, layout),
            Some(LayoutRect {
                x: 0.0,
                y: 10.0,
                width: 80.0,
                height: 60.0,
            })
        );
    }

    #[test]
    fn atomic_inline_level_box_is_not_clipped_to_the_parent_line_height() {
        let style = Style {
            display: Display::InlineBlock,
            align_self: w3cos_std::style::AlignSelf::FlexStart,
            font_size: 16.0,
            line_height: 1.2,
            ..Style::default()
        };

        assert_eq!(
            inline_fragment_clip_rect(&ComponentKind::Box, &style, rect(0.0)),
            None
        );
    }

    #[test]
    fn display_none_root_suppresses_root_and_body_canvas_backgrounds() {
        for background in [Color::rgb(0, 128, 0), Color::TRANSPARENT] {
            let root = PaintNode {
                kind: ComponentKind::Column,
                style: (Style {
                    display: Display::None,
                    background,
                    ..Style::default()
                })
                .into(),
                parent: None,
                sticky_counter_signal: None,
            };
            let body = PaintNode {
                kind: ComponentKind::Column,
                style: (Style {
                    background: Color::rgb(255, 0, 0),
                    background_image: Some("url(square-white.png)".into()),
                    ..Style::default()
                })
                .into(),
                parent: Some(0),
                sticky_counter_signal: None,
            };
            let artifact = PaintArtifact::build_with_body_background([root, body], &[], 1, Some(1));
            assert_eq!(artifact.canvas_background, Color::WHITE);
            assert_eq!(artifact.canvas_background_source, None);
            assert!(artifact.canvas_background_style.is_none());
        }
    }

    #[test]
    fn display_none_body_does_not_propagate_its_canvas_background() {
        let node = |parent, style: Style| PaintNode {
            kind: ComponentKind::Column,
            style: style.into(),
            parent,
            sticky_counter_signal: None,
        };
        let artifact = PaintArtifact::build_with_body_background(
            [
                node(None, Style::default()),
                node(
                    Some(0),
                    Style {
                        display: Display::None,
                        background: Color::rgb(0, 128, 0),
                        background_image: Some("url(square-white.png)".into()),
                        ..Style::default()
                    },
                ),
            ],
            &[(rect(0.0), 0)],
            1,
            Some(1),
        );
        assert_eq!(artifact.canvas_background, Color::WHITE);
        assert_eq!(artifact.canvas_background_source, None);
        assert!(artifact.canvas_background_style.is_none());
    }

    #[test]
    fn propagates_root_background_to_the_canvas_without_repainting_the_root_box() {
        let mut style = Style::default();
        style.background = Color::rgb(255, 255, 0);
        style.background_image = Some("url(square-white.png)".into());
        style.border_width = 3.0;
        let artifact = PaintArtifact::build(
            [PaintNode {
                kind: ComponentKind::Column,
                style: style.into(),
                parent: None,
                sticky_counter_signal: None,
            }],
            &[(rect(0.0), 0)],
            1,
        );

        assert_eq!(artifact.canvas_background, Color::rgb(255, 255, 0));
        assert_eq!(artifact.canvas_background_source, Some(0));
        assert_eq!(
            artifact.canvas_background_positioning_rect,
            Some(LayoutRect {
                x: 3.0,
                y: 3.0,
                width: 314.0,
                height: 74.0,
            })
        );
        let canvas_style = artifact.canvas_background_style.as_ref().unwrap();
        assert_eq!(canvas_style.border_width, 0.0);
        assert_eq!(canvas_style.background_position.as_deref(), Some("0px 0px"));
        assert_eq!(artifact.nodes[0].style.background, Color::TRANSPARENT);
        assert!(artifact.nodes[0].style.background_image.is_none());
    }

    #[test]
    fn root_sentinel_does_not_raise_negative_descendants_above_normal_flow() {
        let root = PaintNode {
            kind: ComponentKind::Column,
            style: Style::default().into(),
            parent: None,
            sticky_counter_signal: None,
        };
        let mut negative_style = Style::default();
        negative_style.position = Position::Absolute;
        negative_style.z_index = -1;
        let negative = PaintNode {
            kind: ComponentKind::Box,
            style: negative_style.into(),
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let normal = PaintNode {
            kind: ComponentKind::Box,
            style: Style::default().into(),
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let artifact = PaintArtifact::build(
            [root, negative, normal],
            &[(rect(0.0), 0), (rect(0.0), 1), (rect(0.0), 2)],
            1,
        );
        assert_eq!(artifact.z_order, [i32::MIN, -1, 0]);
        assert!(
            artifact.paint_order_key(0) < artifact.paint_order_key(1),
            "the root background and border paint below negative descendants"
        );
        assert!(artifact.paint_order_key(1) < artifact.paint_order_key(2));
    }

    #[test]
    fn transparent_block_text_paints_after_later_in_flow_block_background() {
        let root = PaintNode {
            kind: ComponentKind::Column,
            style: Style::default().into(),
            parent: None,
            sticky_counter_signal: None,
        };
        let mut text_style = Style::default();
        text_style.display = Display::Block;
        let text = PaintNode {
            kind: ComponentKind::Text {
                content: "instruction".into(),
            },
            style: text_style.into(),
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let mut bar_style = Style::default();
        bar_style.background = Color::rgb(255, 165, 0);
        let bar = PaintNode {
            kind: ComponentKind::Box,
            style: bar_style.into(),
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let artifact = PaintArtifact::build(
            [root, text, bar],
            &[(rect(0.0), 0), (rect(0.0), 1), (rect(0.0), 2)],
            1,
        );

        assert!(artifact.paint_order_key(2) < artifact.paint_order_key(1));
    }

    #[test]
    fn absolute_box_inherits_clip_chain_from_its_containing_block() {
        let nodes = vec![
            PaintNode {
                kind: ComponentKind::Box,
                style: Style::default().into(),
                parent: None,
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Box,
                style: (Style {
                    overflow: Overflow::Hidden,
                    ..Style::default()
                })
                .into(),
                parent: Some(0),
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Box,
                style: (Style {
                    position: Position::Absolute,
                    ..Style::default()
                })
                .into(),
                parent: Some(1),
                sticky_counter_signal: None,
            },
        ];
        let artifact =
            PaintArtifact::build(nodes, &[(rect(0.0), 0), (rect(0.0), 1), (rect(80.0), 2)], 1);

        assert_ne!(artifact.node_properties[1].clip, 0);
        assert_eq!(artifact.node_properties[2].clip, 0);
        assert_ne!(artifact.node_properties[1].scroll, 0);
        assert_eq!(artifact.node_properties[2].scroll, 0);
    }

    #[test]
    fn overflow_clip_clips_paint_without_becoming_a_scroll_container() {
        let node = |parent, style: Style| PaintNode {
            kind: ComponentKind::Box,
            style: style.into(),
            parent,
            sticky_counter_signal: None,
        };
        let nodes = vec![
            node(None, Style::default()),
            node(
                Some(0),
                Style {
                    overflow_clip_x: true,
                    ..Style::default()
                },
            ),
        ];
        let artifact = PaintArtifact::build(nodes, &[(rect(0.0), 0), (rect(0.0), 1)], 1);

        let clip = artifact.node_properties[1].clip;
        assert_ne!(clip, 0);
        assert_eq!(artifact.node_properties[1].scroll, 0);
        let rect = artifact.properties.clips[clip].rect.unwrap();
        assert!(
            rect.y <= -1.0e6 && rect.height >= 1.0e6,
            "y stays unbounded"
        );
        assert!(rect.width < 1.0e6, "x is clipped");
    }

    #[test]
    fn overflow_clip_margin_moves_the_clip_edge_and_body_clip_propagates() {
        let node = |parent, style: Style| PaintNode {
            kind: ComponentKind::Box,
            style: style.into(),
            parent,
            sticky_counter_signal: None,
        };
        let nodes = vec![
            node(None, Style::default()),
            node(Some(0), Style::default()),
            node(
                Some(1),
                Style {
                    overflow_clip_x: true,
                    overflow_clip_y: true,
                    overflow_clip_margin: (2, 10.0),
                    ..Style::default()
                },
            ),
        ];
        let artifact =
            PaintArtifact::build(nodes, &[(rect(0.0), 0), (rect(0.0), 1), (rect(50.0), 2)], 1);
        let clip = artifact.node_properties[2].clip;
        assert_ne!(clip, 0);
        let clip_rect = artifact.properties.clips[clip].rect.unwrap();
        let border = artifact.rect_by_index[2].unwrap();
        assert_eq!(clip_rect.x, border.x - 10.0);
        assert_eq!(clip_rect.width, border.width + 20.0);

        // A body whose overflow propagates to the viewport does not clip.
        let nodes = vec![
            node(None, Style::default()),
            node(
                Some(0),
                Style {
                    overflow_clip_x: true,
                    overflow_clip_y: true,
                    ..Style::default()
                },
            ),
        ];
        let artifact = PaintArtifact::build_with_body_background(
            nodes,
            &[(rect(0.0), 0), (rect(0.0), 1)],
            1,
            Some(1),
        );
        assert_eq!(artifact.node_properties[1].clip, 0);
    }

    #[test]
    fn body_background_propagates_through_anonymous_table_wrappers() {
        let node = |parent, display, background| PaintNode {
            kind: ComponentKind::Box,
            style: (Style {
                display,
                background,
                ..Style::default()
            })
            .into(),
            parent,
            sticky_counter_signal: None,
        };
        let nodes = [
            node(None, Display::Table, Color::TRANSPARENT),
            node(Some(0), Display::TableRow, Color::TRANSPARENT),
            node(Some(1), Display::TableCell, Color::TRANSPARENT),
            node(Some(2), Display::Block, Color::rgb(255, 255, 0)),
        ];
        let artifact = PaintArtifact::build_with_body_background(
            nodes,
            &[
                (rect(0.0), 0),
                (rect(0.0), 1),
                (rect(0.0), 2),
                (rect(0.0), 3),
            ],
            1,
            Some(3),
        );
        assert_eq!(artifact.canvas_background_source, Some(3));
        assert_eq!(artifact.canvas_background, Color::rgb(255, 255, 0));
        assert_eq!(artifact.nodes[3].style.background, Color::TRANSPARENT);
    }

    #[test]
    fn propagates_body_background_image_to_the_canvas() {
        let mut root_style = Style::default();
        root_style.background_image = Some("none".into());
        root_style.border_width = 3.0;
        let root = PaintNode {
            kind: ComponentKind::Column,
            style: root_style.into(),
            parent: None,
            sticky_counter_signal: None,
        };
        let mut body_style = Style::default();
        body_style.background_image = Some("url(square-white.png)".into());
        body_style.background_position = Some("top left".into());
        let body = PaintNode {
            kind: ComponentKind::Column,
            style: body_style.into(),
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let artifact = PaintArtifact::build_with_body_background(
            [root, body],
            &[
                (
                    LayoutRect {
                        x: 16.0,
                        ..rect(16.0)
                    },
                    0,
                ),
                (rect(0.0), 1),
            ],
            1,
            Some(1),
        );

        assert_eq!(artifact.canvas_background_source, Some(1));
        assert_eq!(artifact.canvas_background, Color::WHITE);
        assert_eq!(artifact.canvas_background_positioning_rect, None);
        let canvas_style = artifact.canvas_background_style.as_ref().unwrap();
        assert_eq!(canvas_style.border_width, 0.0);
        assert_eq!(
            canvas_style.background_position.as_deref(),
            Some("19px 19px")
        );
        assert!(artifact.nodes[1].style.background_image.is_none());
    }

    #[test]
    fn propagated_body_percentage_position_uses_the_root_box() {
        let root = PaintNode {
            kind: ComponentKind::Column,
            style: Style::default().into(),
            parent: None,
            sticky_counter_signal: None,
        };
        let mut body_style = Style::default();
        body_style.background_image = Some("url(square-purple.png)".into());
        body_style.background_position = Some("50% 50%".into());
        let body = PaintNode {
            kind: ComponentKind::Column,
            style: body_style.into(),
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let root_rect = LayoutRect {
            x: 16.0,
            ..rect(16.0)
        };
        let artifact = PaintArtifact::build_with_body_background(
            [root, body],
            &[(root_rect, 0), (rect(0.0), 1)],
            1,
            Some(1),
        );

        assert_eq!(artifact.canvas_background_positioning_rect, Some(root_rect));
    }

    #[test]
    fn nonvisible_css_borders_use_zero_width_paint_snapshots() {
        use w3cos_std::style::BorderLineStyle;
        for line_style in [
            Some(BorderLineStyle::None),
            Some(BorderLineStyle::Hidden),
            Some(BorderLineStyle::Solid),
            None,
        ] {
            let source = Style {
                border_width: 32.0,
                border_styles: [line_style; 4],
                ..Style::default()
            };
            let artifact = PaintArtifact::build(
                [PaintNode {
                    kind: ComponentKind::Column,
                    style: source.clone().into(),
                    parent: None,
                    sticky_counter_signal: None,
                }],
                &[(rect(0.0), 0)],
                1,
            );
            let used = &artifact.nodes[0].style;
            let expected = if line_style.is_some_and(|style| !style.is_visible()) {
                0.0
            } else {
                32.0
            };
            for width in [
                used.border_top_width,
                used.border_right_width,
                used.border_bottom_width,
                used.border_left_width,
            ] {
                assert_eq!(
                    width.unwrap_or(used.border_width),
                    expected,
                    "{line_style:?}"
                );
            }
            assert_eq!(
                used.border_styles, source.border_styles,
                "hidden identity retained"
            );
            assert_eq!(source.border_width, 32.0, "computed width retained");
        }
    }

    #[test]
    fn translate_percent_marker_resolves_against_the_border_box() {
        let mut style = Style::default();
        style.transform.translate_x = 4.0;
        style.custom_properties = Some(Default::default());
        style
            .custom_properties
            .as_mut()
            .unwrap()
            .insert("--w3cos-internal-translate-percent".into(), "-50,10".into());
        let mut nodes = vec![
            PaintNode {
                kind: ComponentKind::Box,
                style: style.into(),
                parent: None,
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Box,
                style: Style::default().into(),
                parent: Some(0),
                sticky_counter_signal: None,
            },
        ];
        let rect = |width, height| LayoutRect {
            x: 0.0,
            y: 0.0,
            width,
            height,
        };
        resolve_translate_percent(
            &mut nodes,
            &[(rect(60.0, 20.0), 0), (rect(8.0, 8.0), 1)],
            None,
        );
        assert_eq!(nodes[0].style.transform.translate_x, 4.0 - 30.0);
        assert_eq!(nodes[0].style.transform.translate_y, 2.0);
        assert!(nodes[1].style.transform.is_identity());
    }

    #[test]
    fn builds_independent_property_trees_and_display_chunks() {
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

        let artifact = PaintArtifact::build(
            [root, scroll, child],
            &[(rect(0.0), 0), (rect(0.0), 1), (rect(80.0), 2)],
            7,
        );

        assert_eq!(artifact.generation, 7);
        assert!(
            artifact.column_fragments.is_empty(),
            "ordinary pages do not allocate per-node column slices"
        );
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
    fn fixed_auto_multicol_artifact_retains_physical_fragments_and_source_identity() {
        use w3cos_std::style::{ColumnFill, Dimension};
        let node = |style: Style, parent| PaintNode {
            kind: ComponentKind::Column,
            style: style.into(),
            parent,
            sticky_counter_signal: None,
        };
        let artifact = PaintArtifact::build(
            [
                node(
                    Style {
                        display: Display::Block,
                        width: Dimension::Px(300.0),
                        height: Dimension::Px(100.0),
                        column_width: Dimension::Px(100.0),
                        column_gap: Some(0.0),
                        column_fill: ColumnFill::Auto,
                        ..Style::default()
                    },
                    None,
                ),
                node(
                    Style {
                        display: Display::Block,
                        ..Style::default()
                    },
                    Some(0),
                ),
                node(
                    Style {
                        display: Display::Block,
                        position: Position::Absolute,
                        ..Style::default()
                    },
                    Some(0),
                ),
            ],
            &[
                (
                    LayoutRect {
                        x: 20.0,
                        y: 30.0,
                        width: 300.0,
                        height: 100.0,
                    },
                    0,
                ),
                (
                    LayoutRect {
                        x: 20.0,
                        y: 30.0,
                        width: 100.0,
                        height: 250.0,
                    },
                    1,
                ),
                (
                    LayoutRect {
                        x: 20.0,
                        y: 30.0,
                        width: 20.0,
                        height: 250.0,
                    },
                    2,
                ),
            ],
            1,
        );
        assert_eq!(
            artifact.nodes.len(),
            3,
            "fragments do not duplicate source nodes"
        );
        let fragments: Vec<_> = artifact
            .display_items
            .iter()
            .filter(|item| item.client_index == 1)
            .map(|item| item.visual_rect)
            .collect();
        assert_eq!(
            fragments,
            vec![
                LayoutRect {
                    x: 20.0,
                    y: 30.0,
                    width: 100.0,
                    height: 100.0
                },
                LayoutRect {
                    x: 120.0,
                    y: 30.0,
                    width: 100.0,
                    height: 100.0
                },
                LayoutRect {
                    x: 220.0,
                    y: 30.0,
                    width: 100.0,
                    height: 50.0
                },
            ]
        );
        assert_eq!(
            artifact
                .display_items
                .iter()
                .filter(|item| item.client_index == 2)
                .count(),
            1,
            "absolute children do not flow through columns"
        );
        assert_eq!(
            artifact.rect_by_index[1].unwrap().height,
            250.0,
            "logical source geometry remains available"
        );
    }

    #[test]
    fn balanced_multicol_artifact_uses_shorter_fragmentainers_with_fixed_principal_height() {
        use w3cos_std::style::{ColumnFill, Dimension};
        let node = |style: Style, parent| PaintNode {
            kind: ComponentKind::Column,
            style: style.into(),
            parent,
            sticky_counter_signal: None,
        };
        let artifact = PaintArtifact::build(
            [
                node(
                    Style {
                        display: Display::Block,
                        width: Dimension::Px(300.0),
                        height: Dimension::Px(100.0),
                        column_width: Dimension::Px(100.0),
                        column_gap: Some(0.0),
                        column_fill: ColumnFill::Balance,
                        ..Style::default()
                    },
                    None,
                ),
                node(
                    Style {
                        display: Display::Block,
                        ..Style::default()
                    },
                    Some(0),
                ),
            ],
            &[
                (
                    LayoutRect {
                        x: 20.0,
                        y: 30.0,
                        width: 300.0,
                        height: 100.0,
                    },
                    0,
                ),
                (
                    LayoutRect {
                        x: 20.0,
                        y: 30.0,
                        width: 100.0,
                        height: 255.0,
                    },
                    1,
                ),
            ],
            1,
        );
        let fragments: Vec<_> = artifact
            .display_items
            .iter()
            .filter(|item| item.client_index == 1)
            .map(|item| item.visual_rect)
            .collect();
        assert_eq!(
            fragments,
            vec![
                LayoutRect {
                    x: 20.0,
                    y: 30.0,
                    width: 100.0,
                    height: 85.0
                },
                LayoutRect {
                    x: 120.0,
                    y: 30.0,
                    width: 100.0,
                    height: 85.0
                },
                LayoutRect {
                    x: 220.0,
                    y: 30.0,
                    width: 100.0,
                    height: 85.0
                },
            ]
        );
        assert_eq!(
            artifact.rect_by_index[0].unwrap().height,
            100.0,
            "balancing shortens fragmentainers, not an explicit principal height"
        );
    }

    /// The paint phase walk as it was before ancestors' outcomes were
    /// remembered: every node walks up its ancestor chain.
    fn reference_local_level(
        this: &PaintArtifact,
        index: usize,
        split_owner: bool,
    ) -> PaintOrderLevel {
        let node = &this.nodes[index];
        let ordinal = this
            .logical_paint_ordinals
            .get(index)
            .copied()
            .unwrap_or(index);
        if split_owner {
            // A lowered static float can have a zero visual offset. It still
            // belongs to its positioned inline owner, without changing flow.
            return (4, 0, ordinal);
        }
        if is_positioned(&node.style) {
            return match node.style.z_index.cmp(&0) {
                std::cmp::Ordering::Less => (0, node.style.z_index, ordinal),
                std::cmp::Ordering::Equal => (4, 0, ordinal),
                std::cmp::Ordering::Greater => (5, node.style.z_index, ordinal),
            };
        }
        if node.parent.is_some() && establishes_stacking_context(node) {
            // Transform/opacity contexts participate at stack level zero,
            // after normal inline ink, even when their box is a static float.
            // A non-positioned box does not gain authored z-index semantics.
            return (4, 0, ordinal);
        }

        let collapsed_table_part_border = node.style.border_collapse
            && node.style.background.a == 0
            && matches!(
                node.style.display,
                Display::TableRowGroup
                    | Display::TableHeaderGroup
                    | Display::TableFooterGroup
                    | Display::TableRow
                    | Display::TableColumnGroup
                    | Display::TableColumn
            )
            && (node.style.border_width > 0.0
                || node.style.border_top_width.is_some_and(|width| width > 0.0)
                || node
                    .style
                    .border_right_width
                    .is_some_and(|width| width > 0.0)
                || node
                    .style
                    .border_bottom_width
                    .is_some_and(|width| width > 0.0)
                || node
                    .style
                    .border_left_width
                    .is_some_and(|width| width > 0.0));
        if collapsed_table_part_border {
            // Collapsed table borders paint over cell contents. Transparent
            // table-part backgrounds can therefore use a late display item
            // without disturbing the table background-layer ordering.
            return (4, 0, ordinal);
        }

        let mut cursor = Some(index);
        while let Some(current) = cursor {
            let current_node = &this.nodes[current];
            if current != index
                && is_positioned(&current_node.style)
                && !establishes_stacking_context(current_node)
            {
                // The hierarchical prefix keeps this normal subtree in the
                // positioned participant; retain its local CSS paint phases.
                break;
            }
            if current_node.style.float != w3cos_std::style::Float::None {
                return (2, 0, ordinal);
            }
            if current != index
                && matches!(
                    current_node.style.display,
                    Display::InlineBlock | Display::InlineFlex | Display::InlineTable
                )
            {
                return (3, 0, ordinal);
            }
            if current != index && establishes_stacking_context(current_node) {
                break;
            }
            cursor = current_node.parent;
        }

        let split_inline_edge = node
            .style
            .custom_properties
            .as_ref()
            .is_some_and(|properties| {
                properties.contains_key("--w3cos-internal-split-inline-edge")
            });
        let normal_inline_border = node.style.display == Display::Inline
            && node.style.line_height_is_normal
            && matches!(node.kind, ComponentKind::Row | ComponentKind::Box)
            && (node.style.border_width > 0.0
                || node.style.border_top_width.is_some_and(|width| width > 0.0)
                || node
                    .style
                    .border_right_width
                    .is_some_and(|width| width > 0.0)
                || node
                    .style
                    .border_bottom_width
                    .is_some_and(|width| width > 0.0)
                || node
                    .style
                    .border_left_width
                    .is_some_and(|width| width > 0.0));
        let phase = if split_inline_edge || normal_inline_border {
            // Inline decoration and in-flow foreground share tree order:
            // a later fragment covers preceding block glyph overflow, while
            // remaining behind its own/following text. Moving every border
            // before all foreground loses that ordering across split lines.
            3
        } else if unadorned_block_text(node)
            || matches!(
                node.style.display,
                Display::Inline | Display::InlineBlock | Display::InlineFlex | Display::InlineTable
            )
        {
            3
        } else {
            1
        };
        (phase, 0, ordinal)
    }

    #[test]
    fn remembered_ancestor_walks_match_a_fresh_walk() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n) as usize
        };
        let mut nodes = Vec::new();
        let mut layout = Vec::new();
        for index in 0..400 {
            let mut style = Style::default();
            style.position = [
                Position::Static,
                Position::Static,
                Position::Relative,
                Position::Absolute,
                Position::Fixed,
                Position::Sticky,
            ][next(6)];
            style.z_index = [0, 0, 1, -1][next(4)];
            style.float = [
                w3cos_std::style::Float::None,
                w3cos_std::style::Float::None,
                w3cos_std::style::Float::Left,
            ][next(3)];
            style.display = [
                Display::Block,
                Display::Inline,
                Display::InlineBlock,
                Display::InlineFlex,
                Display::Flex,
            ][next(5)];
            if next(5) == 0 {
                style.opacity = 0.5;
            }
            let parent = (index > 0).then(|| next(index as u64));
            nodes.push(PaintNode {
                kind: ComponentKind::Column,
                style: style.into(),
                parent,
                sticky_counter_signal: None,
            });
            layout.push((rect(index as f32), index));
        }
        let artifact = PaintArtifact::build(nodes, &layout, 1);
        let mut replay = artifact.clone();
        let mut memo = AppendMemo::new(&replay.nodes);
        for index in 0..replay.nodes.len() {
            replay.append_node(index, &mut memo);
        }
        let mut walked = artifact.clone();
        walked.viewport_attached_by_index.clear();
        for index in 0..artifact.nodes.len() {
            assert_eq!(
                replay.local_paint_order_level(index, &memo),
                reference_local_level(&replay, index, memo.split_owner[index]),
                "node {index}"
            );
            assert_eq!(
                artifact.viewport_attached(index),
                walked.viewport_attached(index),
                "node {index}"
            );
        }
    }

    #[test]
    fn sticky_owner_and_z_order_are_retained() {
        let mut sticky_style = Style::default();
        sticky_style.position = Position::Sticky;
        sticky_style.z_index = 3;
        let nodes = [
            PaintNode {
                kind: ComponentKind::Column,
                style: Style::default().into(),
                parent: None,
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Column,
                style: sticky_style.into(),
                parent: Some(0),
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Text {
                    content: "inside".into(),
                },
                style: Style::default().into(),
                parent: Some(1),
                sticky_counter_signal: None,
            },
        ];
        let artifact =
            PaintArtifact::build(nodes, &[(rect(0.0), 0), (rect(0.0), 1), (rect(20.0), 2)], 1);

        assert_eq!(artifact.sticky_owner, vec![None, Some(1), Some(1)]);
        assert_eq!(artifact.z_order, vec![i32::MIN, 3, 3]);
    }

    /// `paint_order_class` orders and ties exactly as `paint_order_key`.
    fn assert_paint_classes_follow_keys(artifact: &PaintArtifact) {
        let n = artifact.nodes.len() + 1;
        for left in 0..n {
            for right in 0..n {
                assert_eq!(
                    artifact
                        .paint_order_class(left)
                        .cmp(&artifact.paint_order_class(right)),
                    artifact
                        .paint_order_key(left)
                        .cmp(artifact.paint_order_key(right)),
                    "paint order class of {left} vs {right}"
                );
            }
        }
    }

    #[test]
    fn bidi_visual_fragments_paint_in_logical_order_without_changing_layout() {
        let mut nodes = vec![PaintNode {
            kind: ComponentKind::Row,
            style: Style::default().into(),
            parent: None,
            sticky_counter_signal: None,
        }];
        for rank in [2, 1, 0] {
            let mut style = Style::default();
            style.display = Display::Inline;
            style.custom_properties = Some(w3cos_std::style::CustomProperties::from_iter([(
                "--w3cos-internal-bidi-logical-order".to_string(),
                rank.to_string(),
            )]));
            nodes.push(PaintNode {
                kind: ComponentKind::Text {
                    content: rank.to_string(),
                },
                style: style.into(),
                parent: Some(0),
                sticky_counter_signal: None,
            });
        }
        let artifact = PaintArtifact::build(
            nodes,
            &[
                (rect(0.0), 0),
                (rect(0.0), 1),
                (rect(20.0), 2),
                (rect(40.0), 3),
            ],
            1,
        );
        assert!(artifact.paint_order_key(3) < artifact.paint_order_key(2));
        assert!(artifact.paint_order_key(2) < artifact.paint_order_key(1));
        assert_eq!(artifact.rect_by_index[1], Some(rect(0.0)));
        assert_eq!(artifact.nodes[1].style.z_index, 0);
        assert_paint_classes_follow_keys(&artifact);
    }

    #[test]
    fn auto_positioned_subtree_paints_after_later_normal_flow_content() {
        let root = PaintNode {
            kind: ComponentKind::Column,
            style: Style::default().into(),
            parent: None,
            sticky_counter_signal: None,
        };
        let mut absolute_style = Style::default();
        absolute_style.position = Position::Absolute;
        let absolute = PaintNode {
            kind: ComponentKind::Column,
            style: absolute_style.into(),
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let normal = PaintNode {
            kind: ComponentKind::Column,
            style: Style::default().into(),
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let artifact = PaintArtifact::build(
            [root, absolute, normal],
            &[(rect(0.0), 0), (rect(0.0), 1), (rect(0.0), 2)],
            1,
        );

        assert!(artifact.paint_order_key(0) < artifact.paint_order_key(2));
        assert!(
            artifact.paint_order_key(2) < artifact.paint_order_key(1),
            "positioned-auto subtree must paint after later normal-flow content"
        );
    }

    #[test]
    fn auto_positioned_container_paints_normal_blocks_before_positioned_children() {
        let nodes = [
            (None, Position::Static),
            (Some(0), Position::Relative),
            (Some(1), Position::Absolute),
            (Some(1), Position::Static),
        ]
        .map(|(parent, position)| PaintNode {
            kind: ComponentKind::Box,
            style: (Style {
                position,
                ..Style::default()
            })
            .into(),
            parent,
            sticky_counter_signal: None,
        });
        let artifact = PaintArtifact::build(
            nodes,
            &[
                (rect(0.0), 0),
                (rect(0.0), 1),
                (rect(0.0), 2),
                (rect(0.0), 3),
            ],
            1,
        );
        assert!(artifact.paint_order_key(1) < artifact.paint_order_key(3));
        assert!(artifact.paint_order_key(3) < artifact.paint_order_key(2));
    }

    #[test]
    fn nested_stacking_context_orders_background_auto_and_positive_children() {
        let root = PaintNode {
            kind: ComponentKind::Column,
            style: Style::default().into(),
            parent: None,
            sticky_counter_signal: None,
        };
        let wrapper = PaintNode {
            kind: ComponentKind::Column,
            style: (Style {
                position: Position::Relative,
                z_index: 1,
                ..Style::default()
            })
            .into(),
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let positive = PaintNode {
            kind: ComponentKind::Column,
            style: (Style {
                position: Position::Absolute,
                z_index: 1,
                ..Style::default()
            })
            .into(),
            parent: Some(1),
            sticky_counter_signal: None,
        };
        let auto = PaintNode {
            kind: ComponentKind::Column,
            style: (Style {
                position: Position::Absolute,
                ..Style::default()
            })
            .into(),
            parent: Some(1),
            sticky_counter_signal: None,
        };
        let artifact = PaintArtifact::build(
            [root, wrapper, positive, auto],
            &[
                (rect(0.0), 0),
                (rect(0.0), 1),
                (rect(0.0), 2),
                (rect(0.0), 3),
            ],
            1,
        );

        assert!(artifact.paint_order_key(1) < artifact.paint_order_key(3));
        assert!(artifact.paint_order_key(3) < artifact.paint_order_key(2));
        assert_paint_classes_follow_keys(&artifact);
    }

    #[test]
    fn split_positioned_inline_fragments_precede_independently_positioned_descendant() {
        let node = |style: Style, parent| PaintNode {
            kind: ComponentKind::Text {
                content: "B".into(),
            },
            style: style.into(),
            parent,
            sticky_counter_signal: None,
        };
        let mut fragment = Style {
            display: Display::Inline,
            position: Position::Relative,
            ..Style::default()
        };
        fragment
            .custom_properties
            .get_or_insert_with(Default::default)
            .insert(
                "--w3cos-internal-positioned-inline-owner".into(),
                "inline:17".into(),
            );
        let descendant = Style {
            position: Position::Relative,
            float: w3cos_std::style::Float::Right,
            ..Style::default()
        };
        let artifact = PaintArtifact::build(
            [
                node(Style::default(), None),
                node(fragment.clone(), Some(0)),
                node(descendant, Some(0)),
                node(fragment, Some(0)),
            ],
            &[
                (rect(0.0), 0),
                (rect(0.0), 1),
                (rect(0.0), 2),
                (rect(0.0), 3),
            ],
            1,
        );
        assert!(
            artifact.paint_order_key(3) < artifact.paint_order_key(2),
            "the trailing fragment belongs to its original positioned owner, not a later sibling context: fragment={:?}, descendant={:?}",
            artifact.paint_order_key(3),
            artifact.paint_order_key(2)
        );
    }

    #[test]
    fn transformed_static_float_paints_in_zero_level_stacking_phase() {
        let node = |kind, style: Style, parent| PaintNode {
            kind,
            style: style.into(),
            parent,
            sticky_counter_signal: None,
        };
        let mut floating = Style::default();
        floating.float = w3cos_std::style::Float::Left;
        floating.transform.translate_x = 40.0;
        // z-index does not apply to this static, non-flex/grid float.
        floating.z_index = -10;
        // `Style::default()` is `display: flex`, which would make the float a
        // flex item (where z-index does apply); the parent must be a block.
        let block_parent = Style {
            display: Display::Block,
            ..Style::default()
        };
        let artifact = PaintArtifact::build(
            [
                node(ComponentKind::Column, block_parent, None),
                node(ComponentKind::Row, floating, Some(0)),
                node(
                    ComponentKind::Text {
                        content: "A".into(),
                    },
                    Style {
                        display: Display::Inline,
                        ..Style::default()
                    },
                    Some(0),
                ),
            ],
            &[(rect(0.0), 0), (rect(0.0), 1), (rect(0.0), 2)],
            1,
        );
        assert!(
            artifact.paint_order_key(2) < artifact.paint_order_key(1),
            "transformed static float establishes a zero-level stacking context after normal inline ink: inline={:?}, float={:?}",
            artifact.paint_order_key(2),
            artifact.paint_order_key(1)
        );
    }

    #[test]
    fn static_flex_item_z_index_orders_like_a_positioned_box() {
        // z-index applies to flex items even when `position` is static, so a
        // negative-z item paints below its parent's normal-flow ink.
        let node = |kind, style: Style, parent| PaintNode {
            kind,
            style: style.into(),
            parent,
            sticky_counter_signal: None,
        };
        let build = |parent_display| {
            let item = Style {
                z_index: -10,
                ..Style::default()
            };
            let parent = Style {
                display: parent_display,
                ..Style::default()
            };
            let artifact = PaintArtifact::build(
                [
                    node(ComponentKind::Column, parent, None),
                    node(ComponentKind::Row, Style::default(), Some(0)),
                    node(ComponentKind::Row, item, Some(0)),
                ],
                &[(rect(0.0), 0), (rect(0.0), 1), (rect(0.0), 2)],
                1,
            );
            // The later sibling carries the negative z-index.
            artifact.paint_order_key(2) < artifact.paint_order_key(1)
        };
        assert!(build(Display::Flex), "flex item honours z-index");
        assert!(!build(Display::Block), "block child ignores z-index");
    }

    #[test]
    fn fixed_auto_stacking_context_contains_negative_descendants() {
        let root = PaintNode {
            kind: ComponentKind::Column,
            style: Style::default().into(),
            parent: None,
            sticky_counter_signal: None,
        };
        let mut relative_style = Style::default();
        relative_style.position = Position::Relative;
        let relative = PaintNode {
            kind: ComponentKind::Column,
            style: relative_style.into(),
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let mut fixed_style = Style::default();
        fixed_style.position = Position::Fixed;
        let fixed = PaintNode {
            kind: ComponentKind::Column,
            style: fixed_style.into(),
            parent: Some(1),
            sticky_counter_signal: None,
        };
        let mut negative_style = Style::default();
        negative_style.position = Position::Absolute;
        negative_style.z_index = -1;
        let negative = PaintNode {
            kind: ComponentKind::Column,
            style: negative_style.into(),
            parent: Some(2),
            sticky_counter_signal: None,
        };
        let artifact = PaintArtifact::build(
            [root, relative, fixed, negative],
            &[
                (rect(0.0), 0),
                (rect(0.0), 1),
                (rect(0.0), 2),
                (rect(0.0), 3),
            ],
            1,
        );

        assert_eq!(artifact.z_order, vec![i32::MIN, 1, 1, 2]);
        assert_eq!(artifact.display_items[2].client_index, 2);
        assert_eq!(artifact.display_items[3].client_index, 3);
    }

    #[test]
    fn css2_paint_phases_group_float_descendants_between_blocks_and_inlines() {
        let root = PaintNode {
            kind: ComponentKind::Column,
            style: Style::default().into(),
            parent: None,
            sticky_counter_signal: None,
        };
        let mut float_style = Style::default();
        float_style.float = w3cos_std::style::Float::Left;
        let floating = PaintNode {
            kind: ComponentKind::Column,
            style: float_style.into(),
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let float_child = PaintNode {
            kind: ComponentKind::Box,
            style: Style::default().into(),
            parent: Some(1),
            sticky_counter_signal: None,
        };
        let mut inline_style = Style::default();
        inline_style.display = Display::Inline;
        let inline = PaintNode {
            kind: ComponentKind::Text {
                content: "inline".to_string(),
            },
            style: inline_style.into(),
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let mut relative_style = Style::default();
        relative_style.position = Position::Relative;
        let relative = PaintNode {
            kind: ComponentKind::Column,
            style: relative_style.into(),
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let mut positioned_inline_style = Style::default();
        positioned_inline_style.display = Display::Inline;
        let positioned_inline = PaintNode {
            kind: ComponentKind::Text {
                content: "positioned inline".to_string(),
            },
            style: positioned_inline_style.into(),
            parent: Some(4),
            sticky_counter_signal: None,
        };
        let artifact = PaintArtifact::build(
            [
                root,
                floating,
                float_child,
                inline,
                relative,
                positioned_inline,
            ],
            &[
                (rect(0.0), 0),
                (rect(0.0), 1),
                (rect(0.0), 2),
                (rect(0.0), 3),
                (rect(0.0), 4),
                (rect(0.0), 5),
            ],
            1,
        );

        assert_eq!(artifact.css2_paint_phase(0), 0);
        assert_eq!(artifact.css2_paint_phase(1), 1);
        assert_eq!(artifact.css2_paint_phase(2), 1);
        assert_eq!(artifact.css2_paint_phase(3), 2);
        assert_eq!(artifact.css2_paint_phase(5), 0);
    }

    #[test]
    fn inline_block_descendants_remain_in_the_atomic_inline_paint_phase() {
        let root = PaintNode {
            kind: ComponentKind::Box,
            style: Style::default().into(),
            parent: None,
            sticky_counter_signal: None,
        };
        let inline_block = PaintNode {
            kind: ComponentKind::Box,
            style: (Style {
                display: Display::InlineBlock,
                ..Style::default()
            })
            .into(),
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let block_child = PaintNode {
            kind: ComponentKind::Box,
            style: Style::default().into(),
            parent: Some(1),
            sticky_counter_signal: None,
        };
        let artifact = PaintArtifact::build(
            [root, inline_block, block_child],
            &[(rect(0.0), 0), (rect(0.0), 1), (rect(0.0), 2)],
            1,
        );

        assert_eq!(artifact.css2_paint_phase(1), 2);
        assert_eq!(artifact.css2_paint_phase(2), 2);
        assert!(artifact.paint_order_key(1) < artifact.paint_order_key(2));
    }

    #[test]
    fn positioned_auto_descendants_paint_after_the_positioned_background() {
        let root = PaintNode {
            kind: ComponentKind::Box,
            style: Style::default().into(),
            parent: None,
            sticky_counter_signal: None,
        };
        let positioned = PaintNode {
            kind: ComponentKind::Box,
            style: (Style {
                position: Position::Relative,
                ..Style::default()
            })
            .into(),
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let image = PaintNode {
            kind: ComponentKind::Image {
                src: "green.png".into(),
            },
            style: Style::default().into(),
            parent: Some(1),
            sticky_counter_signal: None,
        };
        let artifact = PaintArtifact::build(
            [root, positioned, image],
            &[(rect(0.0), 0), (rect(0.0), 1), (rect(0.0), 2)],
            1,
        );

        assert!(artifact.paint_order_key(1) < artifact.paint_order_key(2));
    }

    #[test]
    fn float_descendants_do_not_escape_a_positioned_auto_subtree() {
        let root = PaintNode {
            kind: ComponentKind::Box,
            style: Style::default().into(),
            parent: None,
            sticky_counter_signal: None,
        };
        let control = PaintNode {
            kind: ComponentKind::Box,
            style: (Style {
                position: Position::Absolute,
                ..Style::default()
            })
            .into(),
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let container = PaintNode {
            kind: ComponentKind::Box,
            style: (Style {
                position: Position::Absolute,
                ..Style::default()
            })
            .into(),
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let floating = PaintNode {
            kind: ComponentKind::Box,
            style: (Style {
                float: w3cos_std::style::Float::Left,
                ..Style::default()
            })
            .into(),
            parent: Some(2),
            sticky_counter_signal: None,
        };
        let float_text = PaintNode {
            kind: ComponentKind::Text {
                content: "XXXX".to_string(),
            },
            style: (Style {
                display: Display::Inline,
                ..Style::default()
            })
            .into(),
            parent: Some(3),
            sticky_counter_signal: None,
        };
        let artifact = PaintArtifact::build(
            [root, control, container, floating, float_text],
            &[
                (rect(0.0), 0),
                (rect(0.0), 1),
                (rect(0.0), 2),
                (rect(0.0), 3),
                (rect(0.0), 4),
            ],
            1,
        );

        assert!(artifact.paint_order_key(1) < artifact.paint_order_key(2));
        assert!(artifact.paint_order_key(2) < artifact.paint_order_key(3));
        assert!(artifact.paint_order_key(3) < artifact.paint_order_key(4));
    }

    #[test]
    fn inline_border_paints_after_preceding_block_text_and_before_following_text() {
        for split in [false, true] {
            let mut border = Style::default();
            border.display = Display::Inline;
            border.line_height_is_normal = true;
            border.border_width = 3.0;
            if split {
                border.custom_properties = Some(w3cos_std::style::CustomProperties::from_iter([(
                    "--w3cos-internal-split-inline-edge".to_string(),
                    "1".to_string(),
                )]));
            }
            let mut text = Style::default();
            text.display = Display::Block;
            let artifact = PaintArtifact::build(
                [
                    PaintNode {
                        kind: ComponentKind::Column,
                        style: Style::default().into(),
                        parent: None,
                        sticky_counter_signal: None,
                    },
                    PaintNode {
                        kind: ComponentKind::Text {
                            content: "Eight".into(),
                        },
                        style: text.clone().into(),
                        parent: Some(0),
                        sticky_counter_signal: None,
                    },
                    PaintNode {
                        kind: ComponentKind::Row,
                        style: border.into(),
                        parent: Some(0),
                        sticky_counter_signal: None,
                    },
                    PaintNode {
                        kind: ComponentKind::Text {
                            content: "Nine".into(),
                        },
                        style: text.into(),
                        parent: Some(0),
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
            assert!(
                artifact.paint_order_key(1) < artifact.paint_order_key(2),
                "inline border must cover preceding block glyph overflow, split={split}"
            );
            assert!(
                artifact.paint_order_key(2) < artifact.paint_order_key(3),
                "inline border must remain behind following text, split={split}"
            );
        }
    }

    #[test]
    fn split_inline_edge_paints_behind_following_block_text() {
        let mut edge_style = Style::default();
        edge_style.display = Display::Inline;
        edge_style.custom_properties = Some(w3cos_std::style::CustomProperties::from_iter([(
            "--w3cos-internal-split-inline-edge".to_string(),
            "1".to_string(),
        )]));
        let mut text_style = Style::default();
        text_style.display = Display::Block;
        let artifact = PaintArtifact::build(
            [
                PaintNode {
                    kind: ComponentKind::Column,
                    style: Style::default().into(),
                    parent: None,
                    sticky_counter_signal: None,
                },
                PaintNode {
                    kind: ComponentKind::Row,
                    style: edge_style.into(),
                    parent: Some(0),
                    sticky_counter_signal: None,
                },
                PaintNode {
                    kind: ComponentKind::Text {
                        content: "Second line".into(),
                    },
                    style: text_style.into(),
                    parent: Some(0),
                    sticky_counter_signal: None,
                },
            ],
            &[(rect(0.0), 0), (rect(0.0), 1), (rect(0.0), 2)],
            1,
        );
        assert!(
            artifact.paint_order_key(1) < artifact.paint_order_key(2),
            "the first split inline edge must stay behind the following block's text"
        );
    }

    #[test]
    fn clean_subtree_does_not_clone_style() {
        let mut style = Style::default();
        style.filter = Some("blur(2px)".into());
        let kind = ComponentKind::Column;
        let incoming = [(&kind, &style, None, None)];
        let (first, clones) = reuse_or_clone_paint_nodes(Vec::new(), incoming);
        assert_eq!(clones, 1);
        let first_ptr = first[0].style.filter.as_ref().map(|s| s.as_ptr());
        let (second, clones) = reuse_or_clone_paint_nodes(first, incoming);
        assert_eq!(clones, 0);
        assert_eq!(
            second[0].style.filter.as_ref().map(|s| s.as_ptr()),
            first_ptr
        );
        let mut dirty = style.clone();
        dirty.opacity = 0.5;
        let incoming_dirty = [(&kind, &dirty, None, None)];
        let (_, dirty_clones) = reuse_or_clone_paint_nodes(second, incoming_dirty);
        assert_eq!(dirty_clones, 1);
    }
}

/// Blink's cull rect expansion (`CullRect::kPixelDistanceToExpand`).
pub(crate) const PAINT_CULL_EXPAND: f32 = 4000.0;
/// Blink keeps a cull rect until the new one moved by this much
/// (`kChangedEnoughMinimumDistance`).
pub(crate) const PAINT_CULL_CHANGED_ENOUGH: f32 = 512.0;

/// `W3COS_PAINT_CULL_RECT=1`: in headless frames, root content whose rect is
/// entirely above or below the viewport expanded by 4000px vertically emits
/// no display items (Blink paints only within the cull rect). `=verify`
/// also records the dropped boxes and reports any whose painted rect reaches
/// the viewport when the frame is painted. On by default (`=0` turns it
/// off).
pub(crate) fn paint_cull_rect_mode() -> u8 {
    #[cfg(test)]
    if let Some(mode) = PAINT_CULL_RECT_OVERRIDE.with(std::cell::Cell::get) {
        return mode;
    }
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(
        || match std::env::var("W3COS_PAINT_CULL_RECT").ok().as_deref() {
            Some("0" | "false") => 0,
            Some("verify") => 2,
            _ => 1,
        },
    )
}

/// `W3COS_PAINT_CULL_RUNS=1` (with `W3COS_PAINT_CULL_RECT`): append reuse
/// copies runs of boxes the cull band drops in both frames as slices, and
/// the paint diff skips boxes that paint nothing in either frame
/// (`=verify` also runs the diff without the skip and compares). On by
/// default (`=0` turns it off).
pub(crate) fn paint_cull_runs_mode() -> u8 {
    #[cfg(test)]
    if let Some(mode) = PAINT_CULL_RUNS_OVERRIDE.with(std::cell::Cell::get) {
        return mode;
    }
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(
        || match std::env::var("W3COS_PAINT_CULL_RUNS").ok().as_deref() {
            Some("0" | "false") => 0,
            Some("verify") => 2,
            _ => 1,
        },
    )
}

/// `W3COS_PAINT_CULL_SPAN_CURSOR=1|verify` (off by default): the reusing
/// build finds last frame's culled span at a box with cursors that follow
/// the build through the boxes (in order, stepping back a few boxes after a
/// failed run), instead of a binary search per box. `verify` also searches and compares.
pub(crate) fn paint_cull_span_cursor_mode() -> u8 {
    #[cfg(test)]
    if let Some(mode) = PAINT_CULL_SPAN_CURSOR_OVERRIDE.with(std::cell::Cell::get) {
        return mode;
    }
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| {
        match std::env::var("W3COS_PAINT_CULL_SPAN_CURSOR")
            .ok()
            .as_deref()
        {
            Some("1") => 1,
            Some("verify") => 2,
            _ => 0,
        }
    })
}

#[cfg(test)]
thread_local! {
    pub(crate) static PAINT_CULL_SPAN_CURSOR_OVERRIDE: std::cell::Cell<Option<u8>> =
        const { std::cell::Cell::new(None) };
}

thread_local! {
    /// `(span lookups by cursor, verify mismatches)`.
    pub(crate) static PAINT_CULL_SPAN_CURSOR_STATS: std::cell::Cell<(u64, u64)> =
        const { std::cell::Cell::new((0, 0)) };
}

/// Cursors into last frame's culled spans (sorted, disjoint) that follow
/// the box being built: `.0` the first span starting at or after it, `.1`
/// the first span ending after it.
#[derive(Clone, Copy, Default)]
struct CullSpanCursor(usize, usize);

impl CullSpanCursor {
    /// The first span starting at or after `at`. Boxes mostly move forward;
    /// a failed run steps back a few boxes.
    #[inline]
    fn starting(&mut self, spans: &[CullSpan], at: usize) -> usize {
        let mode = paint_cull_span_cursor_mode();
        self.0 = self.0.min(spans.len());
        while self.0 > 0 && (spans[self.0 - 1].start as usize) >= at {
            self.0 -= 1;
        }
        while self.0 < spans.len() && (spans[self.0].start as usize) < at {
            self.0 += 1;
        }
        if mode == 2 {
            let full = spans.partition_point(|span| (span.start as usize) < at);
            Self::count(full != self.0, "starting", at);
            return full;
        }
        Self::count(false, "", 0);
        self.0
    }

    /// The first span ending after `at` (see `starting`).
    #[inline]
    fn ending(&mut self, spans: &[CullSpan], at: usize) -> usize {
        let mode = paint_cull_span_cursor_mode();
        self.1 = self.1.min(spans.len());
        while self.1 > 0 && (spans[self.1 - 1].end as usize) > at {
            self.1 -= 1;
        }
        while self.1 < spans.len() && (spans[self.1].end as usize) <= at {
            self.1 += 1;
        }
        if mode == 2 {
            let full = spans.partition_point(|span| span.end as usize <= at);
            Self::count(full != self.1, "ending", at);
            return full;
        }
        Self::count(false, "", 0);
        self.1
    }

    #[inline]
    fn count(mismatch: bool, which: &str, at: usize) {
        if mismatch {
            eprintln!("[paint-cull-span-cursor-verify] mismatch: {which} span at box {at}");
        }
        PAINT_CULL_SPAN_CURSOR_STATS.with(|stats| {
            let (lookups, mismatches) = stats.get();
            stats.set((lookups + 1, mismatches + u64::from(mismatch)));
        });
    }
}

/// `W3COS_PAINT_CULL_RUN_GAPS` (on by default, `=0` turns it off): inside last frame's culled runs, and a
/// gap of the layout's changed boxes, `copy_culled_run` checks only what
/// may differ per box this frame (its parent, its geometry against the band)
/// instead of every input again: the rest held when the run was copied and
/// the box's inputs did not change since (Blink skips a culled subtree
/// without visiting it). `=verify` also runs the full check and compares.
pub(crate) fn paint_cull_run_gaps_mode() -> u8 {
    #[cfg(test)]
    if let Some(mode) = PAINT_CULL_RUN_GAPS_OVERRIDE.with(std::cell::Cell::get) {
        return mode;
    }
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(
        || match std::env::var("W3COS_PAINT_CULL_RUN_GAPS").ok().as_deref() {
            Some("0" | "false") => 0,
            Some("verify") => 2,
            _ => 1,
        },
    )
}

#[cfg(test)]
thread_local! {
    pub(crate) static PAINT_CULL_RUN_GAPS_OVERRIDE: std::cell::Cell<Option<u8>> =
        const { std::cell::Cell::new(None) };
}

thread_local! {
    /// `(boxes the fast check passed, verify mismatches)`.
    pub(crate) static PAINT_CULL_RUN_GAPS_STATS: std::cell::Cell<(u64, u64)> =
        const { std::cell::Cell::new((0, 0)) };
}

/// Whether a reader of the culled spans (`W3COS_PAINT_CULL_RUN_GAPS`,
/// `W3COS_PAINT_CULL_SUBTREES`, `W3COS_PAINT_DIFF_CULLED_SPANS`) is on, so
/// `copy_culled_run` records them.
fn cull_spans_wanted() -> bool {
    paint_cull_run_gaps_mode() != 0
        || paint_cull_subtrees_mode() != 0
        || crate::retained_layers::paint_diff_culled_spans_mode() != 0
}

/// `W3COS_PAINT_CULL_SUBTREES` (on by default, `=0` turns it off; with `W3COS_PAINT_CULL_RUNS` and the
/// layout's touched ranges): a culled span from last frame that lies in a
/// gap of the changed ranges only moved as a whole, so `copy_culled_run`
/// culls it with one test of its kept vertical extent against the band and
/// copies it without visiting its boxes (Blink culls a subtree by its visual
/// overflow rect). `=verify` also checks every box and counts disagreements.
pub(crate) fn paint_cull_subtrees_mode() -> u8 {
    #[cfg(test)]
    if let Some(mode) = PAINT_CULL_SUBTREES_OVERRIDE.with(std::cell::Cell::get) {
        return mode;
    }
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(
        || match std::env::var("W3COS_PAINT_CULL_SUBTREES").ok().as_deref() {
            Some("0" | "false") => 0,
            Some("verify") => 2,
            _ => 1,
        },
    )
}

#[cfg(test)]
thread_local! {
    pub(crate) static PAINT_CULL_SUBTREES_OVERRIDE: std::cell::Cell<Option<u8>> =
        const { std::cell::Cell::new(None) };
}

thread_local! {
    /// `(spans taken whole, boxes in them, verify mismatches)`.
    pub(crate) static PAINT_CULL_SUBTREE_STATS: std::cell::Cell<(u64, u64, u64)> =
        const { std::cell::Cell::new((0, 0, 0)) };
}

pub(crate) fn paint_cull_runs_enabled() -> bool {
    paint_cull_runs_mode() != 0
}

#[cfg(test)]
thread_local! {
    pub(crate) static PAINT_CULL_RUNS_OVERRIDE: std::cell::Cell<Option<u8>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
thread_local! {
    pub(crate) static PAINT_CULL_RECT_OVERRIDE: std::cell::Cell<Option<u8>> =
        const { std::cell::Cell::new(None) };
}

thread_local! {
    /// The band the next `build_reusing` culls with (set around the build).
    pub(crate) static PAINT_CULL_BAND: std::cell::Cell<Option<(f32, f32)>> =
        const { std::cell::Cell::new(None) };
    pub(crate) static PAINT_CULL_COUNT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// Boxes `copy_culled_run` copied.
    pub(crate) static PAINT_CULL_RUN_NODES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    pub(crate) static PAINT_CULL_DIFFS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Ink a box may paint outside its layout rect: shadows, outline, and a
/// line of text overflow.
fn paint_cull_ink_outset(style: &w3cos_std::component::ComponentStyle) -> f32 {
    let style = style.as_style();
    let shadow = style
        .box_shadows
        .iter()
        .chain(style.box_shadow.iter())
        .map(|shadow| {
            shadow.offset_y.abs() + shadow.blur_radius.abs() * 2.0 + shadow.spread_radius.abs()
        })
        .fold(0.0f32, f32::max);
    shadow + style.outline_width.abs() * 2.0 + style.font_size * 2.0 + 64.0
}
