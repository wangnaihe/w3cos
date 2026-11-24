//! Structural splice: lay out only the block subtree a DOM mutation touched.
//!
//! A text append or a tail `appendChild` rebuilt the whole component tree and
//! solved Taffy for every node, so each streamed markdown chunk cost the same
//! as the first full layout. In normal block flow without floats, out-of-flow
//! boxes, or multicol, a block box's used layout depends only on its
//! containing block width and its own subtree. When the mutated subtree sits
//! under a chain of auto-height blocks, the rest of the document only moves:
//! following boxes shift by the height delta and the auto-height ancestors
//! grow by it, up to the first box with a definite height.
//!
//! The splice lays out that subtree alone under a wrapper of the same width,
//! writes its rects at the old origin, applies the shift, and replaces the
//! subtree in the retained component tree. Anything outside the proven-safe
//! shape returns false and the caller rebuilds the whole tree.

use w3cos_dom::node::{NodeId, NodeType};
use w3cos_std::style::{
    AlignContent, AlignItems, AlignSelf, Dimension, Display, Edges, FlexDirection, FlexWrap, Float,
    JustifyContent, Overflow, Position, Spacing, Style,
};
use w3cos_std::{Component, ComponentKind};

use super::{Cache, can_skip_projections, flat_host_id, host_id, needs_text_metric};
use crate::layout::{LayoutEngine, pre_flatten};

thread_local! {
    /// (subtree splices, tail splices) since thread start.
    static SPLICES: std::cell::Cell<(u32, u32)> = const { std::cell::Cell::new((0, 0)) };
    /// Column append/remove splices that kept untouched items instead of
    /// laying them out again.
    static COLUMN_TAIL_REUSES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

thread_local! {
    /// Child-list splices that kept the untouched items' components.
    static CHILD_SPLICES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

pub(crate) fn child_splices() -> u32 {
    CHILD_SPLICES.with(std::cell::Cell::get)
}

pub(super) fn column_tail_reuses() -> u32 {
    COLUMN_TAIL_REUSES.with(|count| count.get())
}

/// `W3COS_SPLICE_DEBUG=1` prints why a flush fell back to a full rebuild.
pub(super) fn debug_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("W3COS_SPLICE_DEBUG").is_some())
}

fn bail(reason: &str) -> bool {
    if debug_enabled() {
        eprintln!("[splice] rebuild: {reason}");
    }
    false
}

/// A child-list edit in a column flex container keeps the lowered components
/// and boxes of the untouched items, lowers and lays out only the inserted
/// ones, and shifts the following items (Blink re-attaches only the inserted
/// children's layout objects and lays out only the dirty children of a flex
/// container). On by default; `W3COS_RETAIN_LOWERING=0` rebuilds instead.
pub(super) fn retain_lowering_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    #[cfg(test)]
    if tests::FORCE_RETAIN.with(std::cell::Cell::get) {
        return true;
    }
    *ON.get_or_init(|| {
        !matches!(
            std::env::var("W3COS_RETAIN_LOWERING").ok().as_deref(),
            Some("0") | Some("false")
        )
    })
}

/// `W3COS_RETAIN_LOWERING_VERIFY=1`: after each child-list splice, compare
/// the retained component tree and the boxes with a fresh full rebuild.
fn retain_lowering_verify() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("W3COS_RETAIN_LOWERING_VERIFY").is_some())
}

/// `W3COS_SUBTREE_RELAYOUT=0` keeps the previous whole-tree fallback so a
/// profile can compare the two on one binary.
pub(super) fn subtree_relayout_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        !matches!(
            std::env::var("W3COS_SUBTREE_RELAYOUT").ok().as_deref(),
            Some("0") | Some("false")
        )
    })
}

pub(crate) fn splice_counts() -> (u32, u32) {
    SPLICES.with(std::cell::Cell::get)
}

const LIST_MARKER: &str = "--w3cos-internal-outside-list-marker";

fn is_outside_list_marker(component: &Component) -> bool {
    component
        .style
        .custom_properties
        .as_ref()
        .is_some_and(|props| props.contains_key(LIST_MARKER))
}

fn is_split_inline_fragment(component: &Component) -> bool {
    component
        .style
        .custom_properties
        .as_ref()
        .is_some_and(|props| props.contains_key(w3cos_dom::document::SPLIT_INLINE_FRAGMENT_OWNER))
}

/// No box anywhere in `root` can make a block subtree depend on more than
/// its containing block width.
pub(super) fn tree_is_spliceable(root: &Component) -> bool {
    spliceable(root, false)
}

/// [`tree_is_spliceable`], except that split-inline fragments are allowed:
/// [`try_splice`] republishes the CSSOM fragment rects of the owners it
/// rebuilds and proves that no other box belongs to them.
pub(super) fn tree_is_spliceable_with_owners(root: &Component) -> bool {
    spliceable(root, true)
}

fn spliceable(root: &Component, owners_ok: bool) -> bool {
    let mut stack = vec![root];
    while let Some(component) = stack.pop() {
        let style = &component.style;
        // The sparse CSSOM fragment map is rebuilt from a complete flattened
        // tree. Only the structural splice republishes it per owner.
        if (!owners_ok && is_split_inline_fragment(component))
            || style.float != Float::None
            || matches!(style.position, Position::Fixed | Position::Sticky)
            || (style.position == Position::Absolute && !is_outside_list_marker(component))
            || style.column_count.is_some()
            || !matches!(style.column_width, Dimension::Auto)
        {
            return false;
        }
        stack.extend(component.children.iter());
    }
    true
}

fn is_block_flow(display: Display) -> bool {
    matches!(display, Display::Block | Display::FlowRoot)
}

fn in_flow_static(style: &Style) -> bool {
    matches!(style.position, Position::Static | Position::Relative) && style.float == Float::None
}

fn px_spacing(spacing: Spacing) -> Option<f32> {
    match spacing {
        Spacing::Px(px) if px.is_finite() => Some(px),
        _ => None,
    }
}

fn border_side(style: &Style, side: usize) -> f32 {
    style.computed_border_widths[side].unwrap_or(0.0)
}

/// Padding or border on this edge of `style` keeps child margins inside.
fn edge_has_padding_or_border(style: &Style, top: bool) -> bool {
    let (padding, border) = if top {
        (style.padding.top, border_side(style, 0))
    } else {
        (style.padding.bottom, border_side(style, 2))
    };
    px_spacing(padding).is_some_and(|px| px > 0.0) || border > 0.0
}

/// Children margins cannot collapse through this edge of `component`.
fn edge_blocks_margin_collapse(component: &Component, top: bool) -> bool {
    edge_has_padding_or_border(&component.style, top) || only_inline_children(component)
}

/// A restyle that adds or removes the padding/border on an edge child
/// margins could collapse through moves the box (and its followers) by
/// those margins (CSS 2 §8.3.1; Blink's NGBlockLayoutAlgorithm recomputes
/// the margin strut), which a subtree solve and a tail shift do not model.
pub(super) fn edge_collapse_changes(component: &Component, old: &Style) -> bool {
    // Margins only collapse through a block container in its parent's
    // block formatting context (not a flex, grid or flow-root box).
    let collapses_through = |style: &Style| {
        matches!(style.display, Display::Block | Display::ListItem) && in_flow_static(style)
    };
    if !collapses_through(old) && !collapses_through(&component.style) {
        return false;
    }
    [true, false].into_iter().any(|top| {
        edge_has_padding_or_border(old, top) != edge_has_padding_or_border(&component.style, top)
    }) && !only_inline_children(component)
}

/// Only inline content: there is no child margin to collapse.
fn only_inline_children(component: &Component) -> bool {
    component.children.iter().all(|child| {
        // A block element whose only content is text is lowered to a Text
        // component carrying the element's style; that is still a block.
        (matches!(child.kind, ComponentKind::Text { .. })
            && host_id(child).is_none_or(|id| {
                crate::dom::with_document(|document| {
                    document.get_node(NodeId::from_u32(id)).node_type == NodeType::Text
                })
            }))
            || matches!(
                child.style.display,
                Display::Inline
                    | Display::InlineBlock
                    | Display::InlineFlex
                    | Display::InlineTable
                    | Display::None
            )
            || matches!(child.style.position, Position::Absolute)
    })
}

/// Index path from `root` to the principal box of each element in `chain`
/// (chain[0] is `root`'s host). Anonymous boxes may sit between them.
fn component_path(root: &Component, chain: &[u32]) -> Option<Vec<usize>> {
    fn find(component: &Component, target: u32, path: &mut Vec<usize>) -> bool {
        for (index, child) in component.children.iter().enumerate() {
            match host_id(child) {
                Some(id) if id == target => {
                    path.push(index);
                    return true;
                }
                Some(_) => {}
                None => {
                    path.push(index);
                    if find(child, target, path) {
                        return true;
                    }
                    path.pop();
                }
            }
        }
        false
    }
    if host_id(root) != chain.first().copied() {
        return None;
    }
    let mut path = Vec::new();
    let mut current = root;
    for &target in &chain[1..] {
        let start = path.len();
        if !find(current, target, &mut path) {
            return None;
        }
        for &index in &path[start..] {
            current = &current.children[index];
        }
    }
    Some(path)
}

/// LayoutNG column flex on this path. A width edit changes whether the
/// item's sole text is a `text-line-width` paragraph, which only a fresh
/// lower records. The caller re-lowers that item instead of rebuilding
/// the document. `Mode::Off` never takes the branch.
fn path_has_column_flex(root: &Component, path: &[usize]) -> bool {
    if crate::layout_ng::mode() != crate::layout_ng::Mode::On {
        return false;
    }
    (0..=path.len()).any(|end| {
        let style = &at_path(root, &path[..end]).style;
        matches!(style.display, Display::Flex | Display::InlineFlex)
            && style.flex_direction == FlexDirection::Column
    })
}

fn at_path<'a>(root: &'a Component, path: &[usize]) -> &'a Component {
    path.iter().fold(root, |node, &index| &node.children[index])
}

fn at_path_mut<'a>(root: &'a mut Component, path: &[usize]) -> &'a mut Component {
    path.iter()
        .fold(root, |node, &index| &mut node.children[index])
}

/// Element ancestors from the layout root down to `id` (inclusive), or None
/// when `id` is not rendered under that root.
fn element_chain(
    document: &w3cos_dom::document::Document,
    id: NodeId,
    root: u32,
) -> Option<Vec<u32>> {
    let mut chain = Vec::new();
    let mut current = Some(id);
    while let Some(node) = current {
        let dom = document.get_node(node);
        if dom.node_type == NodeType::Element {
            chain.push(node.as_u32());
            if node.as_u32() == root {
                chain.reverse();
                return Some(chain);
            }
        }
        current = dom.parent;
    }
    None
}

fn common_prefix(a: &mut Vec<u32>, b: &[u32]) {
    let len = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    a.truncate(len);
}

/// `W3COS_TAIL_SHIFT_FAST=1|verify` (off by default): the boxes after a
/// splice that only move (Blink moves their fragments as a whole) are
/// deduplicated with a bitmap instead of a sort, and their DOM rects move in
/// one batched write. `verify` also sorts and compares the two sets.
fn tail_shift_fast_mode() -> u8 {
    #[cfg(test)]
    if let Some(mode) = TAIL_SHIFT_FAST_OVERRIDE.with(std::cell::Cell::get) {
        return mode;
    }
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| match std::env::var("W3COS_TAIL_SHIFT_FAST").as_deref() {
        Ok("1") => 1,
        Ok("verify") => 2,
        _ => 0,
    })
}

#[cfg(test)]
thread_local! {
    static TAIL_SHIFT_FAST_OVERRIDE: std::cell::Cell<Option<u8>> = const { std::cell::Cell::new(None) };
}

thread_local! {
    static TAIL_SHIFT_FAST_MISMATCHES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Remove repeated host ids (a host can own several boxes). Order is not
/// observable: every id gets the same edit.
fn dedup_hosts(ids: &mut Vec<u32>) {
    let mode = tail_shift_fast_mode();
    if mode == 0 {
        ids.sort_unstable();
        ids.dedup();
        return;
    }
    let reference = (mode == 2).then(|| {
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        sorted
    });
    let max = ids.iter().copied().max().unwrap_or(0) as usize;
    let mut seen = vec![0u64; max / 64 + 1];
    ids.retain(|&id| {
        let (word, bit) = (id as usize / 64, 1u64 << (id % 64));
        let fresh = seen[word] & bit == 0;
        seen[word] |= bit;
        fresh
    });
    if let Some(reference) = reference {
        let mut got = ids.clone();
        got.sort_unstable();
        if got != reference {
            TAIL_SHIFT_FAST_MISMATCHES.with(|n| n.set(n.get() + 1));
            eprintln!(
                "[tail-shift-fast-verify] mismatch: {} ids vs {} sorted",
                got.len(),
                reference.len()
            );
        }
    }
}

fn collect_hosts(component: &Component, out: &mut Vec<u32>) {
    let mut stack = vec![component];
    while let Some(node) = stack.pop() {
        if let Some(id) = host_id(node) {
            out.push(id);
        }
        stack.extend(node.children.iter());
    }
}

/// A column flex container stacks its items the same way a block stacks
/// its in-flow children: a used-height change translates the tail and grows
/// auto-height ancestors. Definite free space, wrapping, and gaps that the
/// shift does not model are rejected.
fn column_flex_stacks(component: &Component) -> bool {
    if !subtree_relayout_enabled() {
        return false;
    }
    let style = &component.style;
    if style.display != Display::Flex
        || style.flex_direction != FlexDirection::Column
        || style.flex_wrap != FlexWrap::NoWrap
        || style.justify_content != JustifyContent::FlexStart
        || !in_flow_static(style)
        || is_table_like(component)
        || style.gap != 0.0
        || style.row_gap.is_some()
        || style.column_gap.is_some()
    {
        return false;
    }
    let definite = matches!(style.height, Dimension::Px(_));
    component
        .children
        .iter()
        .all(|child| child.style.flex_grow == 0.0 && (!definite || child.style.flex_shrink == 0.0))
}

fn stacking_parent(component: &Component) -> bool {
    let style = &component.style;
    (is_block_flow(style.display) && in_flow_static(style) && !is_table_like(component))
        || column_flex_stacks(component)
}

/// Ancestors between `path`'s target and the first definite-height block
/// only move or grow. Returns the number of path levels (from the target
/// upwards) whose following siblings must shift, or None when an ancestor
/// could re-flow.
fn ancestor_levels(root: &Component, path: &[usize]) -> Option<usize> {
    let mut levels = 0;
    for depth in (0..path.len()).rev() {
        let parent = at_path(root, &path[..depth]);
        levels += 1;
        if !stacking_parent(parent) {
            return None;
        }
        let style = &parent.style;
        match style.height {
            Dimension::Px(_) => return Some(levels),
            Dimension::Auto => {}
            _ => return None,
        }
        if !matches!(style.min_height, Dimension::Auto)
            || !matches!(style.max_height, Dimension::Auto)
        {
            return None;
        }
    }
    Some(levels)
}

/// The vertical-stack parents a structural splice can grow through: those of
/// [`stacking_parent`], a column flexbox with gaps (a gap is a fixed offset
/// between items, so followers still only move), and a single-line row
/// flexbox whose other items have a definite height and start-aligned or
/// stretched cross axes. A row only grows by the item's height change when
/// that item is the tallest before and after; [`row_item_stays_tallest`]
/// checks that once the change is known.
fn splice_stacking_parent(parent: &Component, child: usize) -> bool {
    if stacking_parent(parent) {
        return true;
    }
    let style = &parent.style;
    if style.display != Display::Flex
        || style.flex_wrap != FlexWrap::NoWrap
        || !in_flow_static(style)
        || is_table_like(parent)
        || !subtree_relayout_enabled()
    {
        return false;
    }
    match style.flex_direction {
        FlexDirection::Column => {
            style.justify_content == JustifyContent::FlexStart
                && parent
                    .children
                    .iter()
                    .all(|item| item.style.flex_grow == 0.0)
                && (!matches!(style.height, Dimension::Px(_))
                    || parent
                        .children
                        .iter()
                        .all(|item| item.style.flex_shrink == 0.0))
        }
        FlexDirection::Row => {
            matches!(
                style.align_items,
                AlignItems::Stretch | AlignItems::FlexStart
            ) && parent.children.iter().enumerate().all(|(index, item)| {
                let item_style = &item.style;
                if matches!(item_style.position, Position::Absolute | Position::Fixed) {
                    return index != child;
                }
                matches!(
                    item_style.align_self,
                    AlignSelf::Auto | AlignSelf::Stretch | AlignSelf::FlexStart
                ) && in_flow_static(item_style)
                    && px_spacing(item_style.margin.top).is_some()
                    && px_spacing(item_style.margin.bottom).is_some()
                    && (index == child
                        || (matches!(item_style.height, Dimension::Px(_))
                            && host_id(item).is_some()))
            })
        }
        _ => false,
    }
}

fn is_row_flex(component: &Component) -> bool {
    component.style.display == Display::Flex && component.style.flex_direction == FlexDirection::Row
}

/// [`ancestor_levels`] over [`splice_stacking_parent`].
fn splice_ancestor_levels(root: &Component, path: &[usize]) -> Option<usize> {
    let mut levels = 0;
    for depth in (0..path.len()).rev() {
        let parent = at_path(root, &path[..depth]);
        levels += 1;
        if !splice_stacking_parent(parent, path[depth]) {
            return None;
        }
        let style = &parent.style;
        match style.height {
            Dimension::Px(_) => return Some(levels),
            Dimension::Auto => {}
            _ => return None,
        }
        if !matches!(style.min_height, Dimension::Auto)
            || !matches!(style.max_height, Dimension::Auto)
        {
            return None;
        }
    }
    Some(levels)
}

/// Every row-flex level a splice grows through keeps the changed item as its
/// tallest item, before and after the height change `dh`, so the row's
/// height changes by exactly `dh` and its other items keep their boxes.
fn row_item_stays_tallest(root: &Component, path: &[usize], levels: usize, dh: f32) -> bool {
    for level in 0..levels {
        let depth = path.len() - 1 - level;
        let parent = at_path(root, &path[..depth]);
        if !is_row_flex(parent) {
            continue;
        }
        let outer = |item: &Component| {
            let id = host_id(item)?;
            let rect = crate::dom::with_document(|document| {
                document.get_layout_rect(NodeId::from_u32(id))
            });
            Some(
                rect.height
                    + px_spacing(item.style.margin.top)?
                    + px_spacing(item.style.margin.bottom)?,
            )
        };
        let Some(item) = outer(&parent.children[path[depth]]) else {
            return false;
        };
        for (index, other) in parent.children.iter().enumerate() {
            if index == path[depth]
                || matches!(other.style.position, Position::Absolute | Position::Fixed)
            {
                continue;
            }
            let Some(other) = outer(other) else {
                return false;
            };
            if item + 0.01 < other || item + dh + 0.01 < other {
                return false;
            }
        }
    }
    true
}

fn carries_cssom_owner(component: &Component) -> bool {
    let mut counts = std::collections::HashMap::new();
    super::collect_cssom_owners(
        &Component {
            kind: component.kind.clone(),
            style: component.style.clone(),
            children: Vec::new(),
            on_click: w3cos_std::EventAction::None,
            sticky_counter_signal: None,
        },
        &mut counts,
    );
    !counts.is_empty()
}

fn is_table_like(component: &Component) -> bool {
    super::is_table(component.style.display)
}

enum PlanKind {
    /// Rebuild the whole subtree rooted at the plan node.
    Subtree(Component),
    /// Keep the first `keep` children of the plan node and rebuild the
    /// rest (a streamed tail block replaced or appended).
    Tail { keep: usize, tail: Vec<Component> },
    /// Replace the `removed` children after the first `keep` by `inserted`
    /// and keep every other child's component and boxes; the following
    /// children only move.
    Children {
        keep: usize,
        removed: usize,
        inserted: Vec<Component>,
    },
}

struct Plan {
    path: Vec<usize>,
    node: u32,
    kind: PlanKind,
}

fn plan_splice(cache: &Cache, dirty: &[NodeId]) -> Result<Option<Plan>, ()> {
    let root = host_id(&cache.component).ok_or(())?;
    // Child-list edits record the parent, whose subtree holds every sibling
    // a structural pseudo-class can observe. Only `+`/`~` let an attribute
    // edit on one element restyle its following siblings.
    let lift = w3cos_dom::stylesheet::has_sibling_selectors();
    crate::dom::with_document(|document| {
        let mut lca: Option<Vec<u32>> = None;
        let mut chains = Vec::new();
        for &id in dirty {
            if document.get_node(id).node_type == NodeType::Document {
                return Err(());
            }
            let mut start = id;
            if lift || document.get_node(id).node_type != NodeType::Element {
                match document.get_node(id).parent {
                    Some(parent) => start = parent,
                    None => continue,
                }
            }
            if lift && document.get_node(start).node_type != NodeType::Element {
                // A sibling-sensitive edit under a non-element parent.
                return Err(());
            }
            let Some(chain) = element_chain(document, start, root) else {
                // Detached: building a subtree off-document cannot move a box.
                continue;
            };
            match lca.as_mut() {
                None => lca = Some(chain.clone()),
                Some(prefix) => common_prefix(prefix, &chain),
            }
            chains.push(chain);
        }
        let Some(mut chain) = lca else {
            return Ok(None);
        };
        if debug_enabled() {
            let last = *chain.last().unwrap();
            eprintln!(
                "[splice] lca depth {} node {last} <{}> dirty {}",
                chain.len(),
                document.get_node(NodeId::from_u32(last)).tag.as_str(),
                dirty.len()
            );
        }
        // Walk up until the subtree root is a safe block box (the relayout
        // boundary of Blink's MarkContainerChainForLayout walk).
        // The root and body keep document-level margin rules; never splice them.
        let body = crate::dom::body_id();
        while chain.len() > 1 && *chain.last().unwrap() != body {
            let node = *chain.last().unwrap();
            if let Some(plan) = try_children(cache, document, &chain, &chains) {
                return Ok(Some(plan));
            }
            if let Some(plan) = try_tail(cache, document, &chain, &chains) {
                return Ok(Some(plan));
            }
            if let Some(plan) = try_candidate(cache, document, &chain, node) {
                return Ok(Some(plan));
            }
            if let Some(plan) = try_container(cache, document, &chain, node) {
                return Ok(Some(plan));
            }
            if let Some(plan) = try_container_children(cache, document, &chain, &chains) {
                return Ok(Some(plan));
            }
            // A column flex too large for `try_container` keeps its items and
            // lays out only the appended ones.
            if let Some(plan) = try_tail_in(cache, document, &chain, &chains, true) {
                return Ok(Some(plan));
            }
            chain.pop();
        }
        Err(())
    })
}

fn ordinary_block_child(component: &Component) -> bool {
    matches!(
        component.style.display,
        Display::Block | Display::FlowRoot | Display::ListItem
    ) && in_flow_static(&component.style)
}

/// A LayoutNG column flex container that stacks its items like a block
/// without margin collapsing: auto height, no wrap, flex-start, stretched
/// items, document order. Appending items leaves the kept ones in place, so
/// a flex wrapper holding the last kept item and the new tail reproduces
/// their positions (Blink's flex algorithm with an indefinite main size).
fn column_flex_tail_parent(component: &Component) -> bool {
    let style = &component.style;
    crate::layout_ng::mode() != crate::layout_ng::Mode::Off
        && subtree_relayout_enabled()
        && style.display == Display::Flex
        && style.flex_direction == FlexDirection::Column
        && style.flex_wrap == FlexWrap::NoWrap
        && style.justify_content == JustifyContent::FlexStart
        && style.align_items == AlignItems::Stretch
        && style.writing_mode == Default::default()
        && matches!(style.height, Dimension::Auto)
        && matches!(style.min_height, Dimension::Auto)
        && matches!(style.max_height, Dimension::Auto)
        && !is_table_like(component)
        && component.children.iter().all(column_flex_tail_item)
}

fn column_flex_tail_item(item: &Component) -> bool {
    let style = &item.style;
    style.flex_grow == 0.0
        && style.order == 0
        && in_flow_static(style)
        && matches!(style.align_self, AlignSelf::Auto | AlignSelf::Stretch)
        && px_spacing(style.margin.top).is_some()
        && px_spacing(style.margin.bottom).is_some()
}

/// Only the trailing children of a block changed. The kept prefix keeps its
/// cached results (LayoutNG reuses a clean child's layout result); the last
/// kept child is solved again only to seed sibling margin collapsing.
fn try_tail(
    cache: &Cache,
    document: &w3cos_dom::document::Document,
    chain: &[u32],
    chains: &[Vec<u32>],
) -> Option<Plan> {
    try_tail_in(cache, document, chain, chains, false)
}

fn try_tail_in(
    cache: &Cache,
    document: &w3cos_dom::document::Document,
    chain: &[u32],
    chains: &[Vec<u32>],
    flex_column: bool,
) -> Option<Plan> {
    if chain.len() <= 1 || chain.last() == Some(&crate::dom::body_id()) {
        return None;
    }
    let node = *chain.last()?;
    let path = component_path(&cache.component, chain)?;
    let old = at_path(&cache.component, &path);
    let style = &old.style;
    let bottom_closed =
        px_spacing(style.padding.bottom).is_some_and(|px| px > 0.0) || border_side(style, 2) > 0.0;
    // A definite height (a scroller such as a chat log) keeps its own box;
    // only its scrollable overflow changes.
    let definite = matches!(style.height, Dimension::Px(_));
    let column_flex = flex_column && column_flex_tail_parent(old);
    if flex_column != column_flex {
        return None;
    }
    if !(is_block_flow(style.display) || column_flex)
        || !in_flow_static(style)
        || !(definite
            || column_flex
            || (bottom_closed
                && matches!(style.height, Dimension::Auto)
                && matches!(style.min_height, Dimension::Auto)
                && matches!(style.max_height, Dimension::Auto)))
        || *style != document.computed_style_for(NodeId::from_u32(node))
    {
        return None;
    }
    splice_ancestor_levels(&cache.component, &path)?;
    let depth = chain.len();
    let touched: Vec<u32> = chains
        .iter()
        .filter(|other| other.len() > depth)
        .map(|other| other[depth])
        .collect();
    let mut dom_children = Vec::new();
    let mut child = document.get_node(NodeId::from_u32(node)).first_child;
    while let Some(id) = child {
        let dom = document.get_node(id);
        if dom.node_type != NodeType::Element {
            return None;
        }
        dom_children.push(id.as_u32());
        child = w3cos_dom::node::sib(dom.next_sibling);
    }
    let cached = old
        .children
        .iter()
        .map(host_id)
        .collect::<Option<Vec<_>>>()?;
    let keep = cached
        .iter()
        .zip(&dom_children)
        .take_while(|(cached, live)| cached == live && !touched.contains(cached))
        .count();
    if keep == 0 || (keep == cached.len() && keep == dom_children.len()) {
        return None;
    }
    let anchor = &old.children[keep - 1];
    let anchor_height = host_id(anchor)
        .map(|id| document.get_layout_rect(NodeId::from_u32(id)).height)
        .unwrap_or(0.0);
    if !(anchor_height > 0.0)
        || !ordinary_block_child(anchor)
        || !matches!(anchor.style.width, Dimension::Auto)
        || px_spacing(anchor.style.margin.left).is_none()
        || px_spacing(anchor.style.margin.right).is_none()
    {
        return None;
    }
    let mut tail = Vec::with_capacity(dom_children.len() - keep);
    for &id in &dom_children[keep..] {
        let component = document.to_component_subtree(NodeId::from_u32(id));
        if host_id(&component) != Some(id)
            || !ordinary_block_child(&component)
            || (column_flex && !column_flex_tail_item(&component))
            || !tree_is_spliceable_with_owners(&component)
        {
            return None;
        }
        tail.push(component);
    }
    if debug_enabled() {
        let mut hosts = Vec::new();
        for component in &tail {
            collect_hosts(component, &mut hosts);
        }
        eprintln!(
            "[splice] tail plan node {node}: keep {keep}, tail {} ({} boxes), anchor boxes {}",
            tail.len(),
            hosts.len(),
            {
                let mut anchor_hosts = Vec::new();
                collect_hosts(anchor, &mut anchor_hosts);
                anchor_hosts.len()
            }
        );
    }
    Some(Plan {
        path,
        node,
        kind: PlanKind::Tail { keep, tail },
    })
}

/// A child-list edit (insert, remove, or move of whole items) in a column
/// flex container: [`column_flex_tail_parent`] stacks items in order with
/// fixed gaps, so the items before the edit keep their boxes and the items
/// after it keep their size and move by one offset. Only the inserted
/// children are lowered and laid out (Blink keeps the layout objects of
/// untouched siblings and lays out only the children marked for layout).
fn try_children(
    cache: &Cache,
    document: &w3cos_dom::document::Document,
    chain: &[u32],
    chains: &[Vec<u32>],
) -> Option<Plan> {
    if !retain_lowering_enabled()
        || chain.len() <= 1
        || chain.last() == Some(&crate::dom::body_id())
        // Kept items were lowered against the old sibling order.
        || w3cos_dom::stylesheet::tree_order_affects_matching()
        || w3cos_dom::stylesheet::has_counter_rules()
    {
        return None;
    }
    let node = *chain.last()?;
    let path = component_path(&cache.component, chain)?;
    let old = at_path(&cache.component, &path);
    if !column_flex_tail_parent(old)
        || !in_flow_static(&old.style)
        || old.style != document.computed_style_for(NodeId::from_u32(node))
    {
        return None;
    }
    splice_ancestor_levels(&cache.component, &path)?;
    let depth = chain.len();
    let touched: Vec<u32> = chains
        .iter()
        .filter(|other| other.len() > depth)
        .map(|other| other[depth])
        .collect();
    let mut dom_children = Vec::new();
    let mut child = document.get_node(NodeId::from_u32(node)).first_child;
    while let Some(id) = child {
        let dom = document.get_node(id);
        if dom.node_type != NodeType::Element {
            return None;
        }
        dom_children.push(id.as_u32());
        child = w3cos_dom::node::sib(dom.next_sibling);
    }
    let cached = old
        .children
        .iter()
        .map(host_id)
        .collect::<Option<Vec<_>>>()?;
    let same = |cached: &u32, live: &u32| cached == live && !touched.contains(cached);
    let keep = cached
        .iter()
        .zip(&dom_children)
        .take_while(|(cached, live)| same(cached, live))
        .count();
    let room = cached.len().min(dom_children.len()) - keep;
    let suffix = cached
        .iter()
        .rev()
        .zip(dom_children.iter().rev())
        .take(room)
        .take_while(|(cached, live)| same(cached, live))
        .count();
    let removed = cached.len() - keep - suffix;
    let inserted_ids = &dom_children[keep..dom_children.len() - suffix];
    if removed == 0 && inserted_ids.is_empty() {
        return None;
    }
    if keep == 0 {
        // No kept item before the edit: the inserted items start at the
        // container's content box.
        let style = &old.style;
        if px_spacing(style.padding.top).is_none()
            || px_spacing(style.padding.left).is_none()
            || px_spacing(style.padding.right).is_none()
        {
            return None;
        }
    } else {
        let anchor = &old.children[keep - 1];
        if px_spacing(anchor.style.margin.left).is_none()
            || px_spacing(anchor.style.margin.right).is_none()
            || !(document
                .get_layout_rect(NodeId::from_u32(host_id(anchor)?))
                .height
                > 0.0)
        {
            return None;
        }
    }
    // Inserted and removed items must not move any counter or quote that a
    // kept item reads.
    let neutral = |id: u32| document.subtree_is_counter_neutral(NodeId::from_u32(id));
    // `W3COS_STYLE_LOCAL`: a restyle removes and re-inserts the same nodes;
    // the check reads the live DOM, so one pass answers both.
    let same_nodes =
        super::style_local_mode() != 0 && cached[keep..keep + removed] == *inserted_ids;
    let reused = reused_restyled_child(document, old, keep, removed, &cached, inserted_ids);
    if reused.is_none()
        && (!cached[keep..keep + removed].iter().all(|&id| neutral(id))
            || (!same_nodes && !inserted_ids.iter().all(|&id| neutral(id))))
    {
        return None;
    }
    let mut inserted = Vec::with_capacity(inserted_ids.len());
    let mut reused = reused;
    for &id in inserted_ids {
        let component = match reused.take() {
            Some(component) => component,
            None => document.to_component_subtree(NodeId::from_u32(id)),
        };
        if host_id(&component) != Some(id)
            || !column_flex_tail_item(&component)
            || !tree_is_spliceable_with_owners(&component)
        {
            return None;
        }
        inserted.push(component);
    }
    if debug_enabled() {
        eprintln!(
            "[splice] children plan node {node}: keep {keep}, removed {removed}, inserted {}, following {suffix}",
            inserted.len()
        );
    }
    Some(Plan {
        path,
        node,
        kind: PlanKind::Children {
            keep,
            removed,
            inserted,
        },
    })
}

thread_local! {
    /// The element a `W3COS_STYLE_LOCAL` restyle splice is for, set only
    /// when its lowering is stable under the edit (see
    /// [`set_style_reuse`]).
    static STYLE_REUSE: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
}

/// `W3COS_STYLE_LOCAL_REUSE` (on by default, `=0` turns it off; needs `W3COS_STYLE_LOCAL`):
/// a restyle whose lowering is stable (same display, fonts, text and
/// internal marks; the edited element is the only one restyled) splices the
/// retained subtree, whose host style the patch already replaced, instead of
/// lowering the element again from the DOM, like Blink keeps the layout
/// objects of a restyled element and only marks them for layout. The counter
/// check reads only the element's own inline declarations: the subtree's
/// nodes, styles and generated content are the ones already laid out.
/// `W3COS_STYLE_LOCAL=verify` compares every such splice with a full layout.
pub(super) fn style_reuse_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        !matches!(
            std::env::var("W3COS_STYLE_LOCAL_REUSE").as_deref(),
            Ok("0" | "false")
        )
    })
}

pub(super) fn set_style_reuse(id: Option<u32>) {
    STYLE_REUSE.with(|cell| cell.set(id));
}

fn reused_restyled_child(
    document: &w3cos_dom::document::Document,
    old: &Component,
    keep: usize,
    removed: usize,
    cached: &[u32],
    inserted_ids: &[u32],
) -> Option<Component> {
    let id = STYLE_REUSE.with(|cell| cell.get())?;
    // The splice root is the restyled element or the item of the edited
    // container that holds it; either way the retained item already carries
    // the patched style.
    let [item] = inserted_ids else {
        return None;
    };
    if removed != 1
        || cached.get(keep) != Some(item)
        || document.inline_declares_counters(NodeId::from_u32(id))
    {
        return None;
    }
    let child = old.children.get(keep)?;
    fn holds(component: &Component, id: u32) -> bool {
        host_id(component) == Some(id) || component.children.iter().any(|child| holds(child, id))
    }
    (host_id(child) == Some(*item) && holds(child, id)).then(|| child.clone())
}

fn try_candidate(
    cache: &Cache,
    document: &w3cos_dom::document::Document,
    chain: &[u32],
    node: u32,
) -> Option<Plan> {
    let path = component_path(&cache.component, chain)?;
    let old = at_path(&cache.component, &path);
    let style = &old.style;
    if !matches!(
        style.display,
        Display::Block | Display::FlowRoot | Display::ListItem
    ) || !in_flow_static(style)
        || !matches!(style.width, Dimension::Auto)
        || px_spacing(style.margin.left).is_none()
        || px_spacing(style.margin.right).is_none()
        || matches!(style.height, Dimension::Percent(_))
        // A zero-height block may be self-collapsing: its margins collapse
        // through it, so its position depends on its content. Go up.
        || !(document.get_layout_rect(NodeId::from_u32(node)).height > 0.0)
    {
        return None;
    }
    splice_ancestor_levels(&cache.component, &path)?;
    let fresh = document.to_component_subtree(NodeId::from_u32(node));
    if fresh.style != old.style
        || host_id(&fresh) != Some(node)
        || !tree_is_spliceable_with_owners(&fresh)
        || !edge_blocks_margin_collapse(&fresh, true)
        || !edge_blocks_margin_collapse(&fresh, false)
        || !edge_blocks_margin_collapse(old, true)
        || !edge_blocks_margin_collapse(old, false)
    {
        return None;
    }
    Some(Plan {
        path,
        node,
        kind: PlanKind::Subtree(fresh),
    })
}

const MAX_SOLVE_NODES: usize = 512;

fn width_is_definite(style: &Style) -> bool {
    match style.width {
        Dimension::Px(px) => px.is_finite() && px >= 0.0,
        Dimension::Percent(percent) => percent.is_finite() && percent >= 0.0,
        _ => false,
    }
}

fn is_flex_or_grid(style: &Style) -> bool {
    matches!(
        style.display,
        Display::Flex | Display::InlineFlex | Display::Grid
    )
}

/// Child-list edits under a flex or grid container re-solve that container
/// when its used width is definite and the container is smaller than the
/// document. The existing block tail plan does not apply: flex items do not
/// collapse margins, and grid items are not a vertical stack.
fn try_container(
    cache: &Cache,
    document: &w3cos_dom::document::Document,
    chain: &[u32],
    node: u32,
) -> Option<Plan> {
    if !subtree_relayout_enabled()
        || chain.len() <= 1
        || chain.last() == Some(&crate::dom::body_id())
    {
        return None;
    }
    let path = component_path(&cache.component, chain)?;
    let old = at_path(&cache.component, &path);
    if !is_flex_or_grid(&old.style)
        || !in_flow_static(&old.style)
        || !width_is_definite(&old.style)
        || crate::layout::count_nodes(old) > MAX_SOLVE_NODES
    {
        return None;
    }
    splice_ancestor_levels(&cache.component, &path)?;
    let fresh = document.to_component_subtree(NodeId::from_u32(node));
    if host_id(&fresh) != Some(node)
        || fresh.style != old.style
        || !tree_is_spliceable_with_owners(&fresh)
        || crate::layout::count_nodes(&fresh) > MAX_SOLVE_NODES
    {
        return None;
    }
    Some(Plan {
        path,
        node,
        kind: PlanKind::Subtree(fresh),
    })
}

/// A child-list edit under a flex or grid container that [`try_children`]
/// cannot shift (growing items, wrapping, alignment): keep the lowered
/// components of the untouched items, lower only the inserted ones, and lay
/// out every item again in a wrapper at the container's content box. Blink re-attaches
/// only the inserted children and relayouts up to the container, which is a
/// relayout boundary when its width does not depend on its content.
fn try_container_children(
    cache: &Cache,
    document: &w3cos_dom::document::Document,
    chain: &[u32],
    chains: &[Vec<u32>],
) -> Option<Plan> {
    if !retain_lowering_enabled()
        || !subtree_relayout_enabled()
        || crate::layout_ng::mode() == crate::layout_ng::Mode::Off
        || chain.len() <= 1
        || chain.last() == Some(&crate::dom::body_id())
        || w3cos_dom::stylesheet::tree_order_affects_matching()
        || w3cos_dom::stylesheet::has_counter_rules()
    {
        return None;
    }
    let node = *chain.last()?;
    let path = component_path(&cache.component, chain)?;
    let old = at_path(&cache.component, &path);
    let parent = at_path(&cache.component, &path[..path.len() - 1]);
    let style = &old.style;
    // The used width must come from the containing block: a definite width,
    // or a block-level box filling its block parent (checked below).
    let width_from_parent = width_is_definite(style)
        || (matches!(style.width, Dimension::Auto)
            && px_spacing(style.margin.left).is_some()
            && px_spacing(style.margin.right).is_some());
    if !matches!(style.display, Display::Flex | Display::Grid)
        || !in_flow_static(style)
        || !width_from_parent
        || style.writing_mode != Default::default()
        // The wrapper has an auto height: so must the container, and a block
        // parent keeps it content-sized (a flex or grid parent may stretch it).
        || !matches!(style.height, Dimension::Auto)
        || !matches!(style.min_height, Dimension::Auto)
        || !matches!(style.max_height, Dimension::Auto)
        || !matches!(
            parent.style.display,
            Display::Block | Display::FlowRoot | Display::ListItem
        )
        // Items are laid out from the content box.
        || px_spacing(style.padding.top).is_none()
        || px_spacing(style.padding.left).is_none()
        || px_spacing(style.padding.right).is_none()
        || is_table_like(old)
        || *style != document.computed_style_for(NodeId::from_u32(node))
        || !(document.get_layout_rect(NodeId::from_u32(node)).height > 0.0)
    {
        return None;
    }
    splice_ancestor_levels(&cache.component, &path)?;
    let depth = chain.len();
    let touched: Vec<u32> = chains
        .iter()
        .filter(|other| other.len() > depth)
        .map(|other| other[depth])
        .collect();
    let mut dom_children = Vec::new();
    let mut child = document.get_node(NodeId::from_u32(node)).first_child;
    while let Some(id) = child {
        let dom = document.get_node(id);
        if dom.node_type != NodeType::Element {
            return None;
        }
        dom_children.push(id.as_u32());
        child = w3cos_dom::node::sib(dom.next_sibling);
    }
    let cached = old
        .children
        .iter()
        .map(host_id)
        .collect::<Option<Vec<_>>>()?;
    let same = |cached: &u32, live: &u32| cached == live && !touched.contains(cached);
    let keep = cached
        .iter()
        .zip(&dom_children)
        .take_while(|(cached, live)| same(cached, live))
        .count();
    let room = cached.len().min(dom_children.len()) - keep;
    let suffix = cached
        .iter()
        .rev()
        .zip(dom_children.iter().rev())
        .take(room)
        .take_while(|(cached, live)| same(cached, live))
        .count();
    let removed = cached.len() - keep - suffix;
    let inserted_ids = &dom_children[keep..dom_children.len() - suffix];
    if removed == 0 && inserted_ids.is_empty() {
        return None;
    }
    let neutral = |id: u32| document.subtree_is_counter_neutral(NodeId::from_u32(id));
    // `W3COS_STYLE_LOCAL`: a restyle removes and re-inserts the same nodes;
    // the check reads the live DOM, so one pass answers both.
    let same_nodes =
        super::style_local_mode() != 0 && cached[keep..keep + removed] == *inserted_ids;
    let reused = reused_restyled_child(document, old, keep, removed, &cached, inserted_ids);
    if reused.is_none()
        && (!cached[keep..keep + removed].iter().all(|&id| neutral(id))
            || (!same_nodes && !inserted_ids.iter().all(|&id| neutral(id))))
    {
        return None;
    }
    let mut inserted = Vec::with_capacity(inserted_ids.len());
    let mut reused = reused;
    for &id in inserted_ids {
        let component = match reused.take() {
            Some(component) => component,
            None => document.to_component_subtree(NodeId::from_u32(id)),
        };
        if host_id(&component) != Some(id) || !tree_is_spliceable_with_owners(&component) {
            return None;
        }
        inserted.push(component);
    }
    // Every item is solved again in a wrapper at the content box; only the
    // untouched items' lowered components are reused.
    let mut items = Vec::with_capacity(keep + inserted.len() + suffix);
    items.extend(old.children[..keep].iter().cloned());
    items.extend(inserted);
    items.extend(old.children[keep + removed..].iter().cloned());
    if debug_enabled() {
        eprintln!(
            "[splice] container children plan node {node}: keep {keep}, removed {removed}, inserted {}, following {suffix}",
            inserted_ids.len()
        );
    }
    Some(Plan {
        path,
        node,
        kind: PlanKind::Children {
            keep: 0,
            removed: cached.len(),
            inserted: items,
        },
    })
}

fn wrapper_for(parent: &Component, width: f32, children: Vec<Component>) -> Component {
    let mut style = parent.style.clone();
    // Flex and grid items do not share a block's used width or margin
    // collapse. LayoutNG's line breaks follow that used width, so a block
    // wrapper can come back shorter than the same item in the real column
    // and the followers keep the old y. The legacy engine's block and flex
    // heights agree on these splices, and that path stays a block wrapper.
    if crate::layout_ng::mode() == crate::layout_ng::Mode::Off || !is_flex_or_grid(&parent.style) {
        style.display = Display::Block;
    }
    style.position = Position::Static;
    style.float = Float::None;
    style.width = Dimension::Px(width);
    style.height = Dimension::Auto;
    style.min_width = Dimension::Auto;
    style.min_height = Dimension::Auto;
    style.max_width = Dimension::Auto;
    style.max_height = Dimension::Auto;
    style.margin = Edges::ZERO;
    style.padding = Edges::ZERO;
    style.border_width = 0.0;
    style.border_top_width = None;
    style.border_right_width = None;
    style.border_bottom_width = None;
    style.border_left_width = None;
    style.computed_border_widths = [Some(0.0); 4];
    style.overflow = Overflow::Visible;
    style.overflow_x = None;
    style.overflow_y = None;
    Component {
        kind: parent.kind.clone(),
        style,
        children,
        on_click: w3cos_std::EventAction::None,
        sticky_counter_signal: None,
    }
}

/// Lay out `wrapper` alone. Returns host rects in flat order and the
/// wrapper's own rect.
fn solve(
    wrapper: &Component,
    skip_projections: bool,
    measure_text: bool,
    viewport_w: f32,
    viewport_h: f32,
) -> Option<(
    Vec<(u32, crate::layout::LayoutRect)>,
    crate::layout::LayoutRect,
)> {
    solve_with_client_rects(
        wrapper,
        skip_projections,
        measure_text,
        viewport_w,
        viewport_h,
        false,
    )
    .map(|(rects, own, _)| (rects, own))
}

use super::ClientRectMap;

/// [`solve`], also returning the CSSOM fragment rects the wrapper's boxes
/// publish (wrapper coordinates) when `client_rects` is set.
fn solve_with_client_rects(
    wrapper: &Component,
    skip_projections: bool,
    measure_text: bool,
    viewport_w: f32,
    viewport_h: f32,
    client_rects: bool,
) -> Option<(
    Vec<(u32, crate::layout::LayoutRect)>,
    crate::layout::LayoutRect,
    ClientRectMap,
)> {
    solve_raw(
        wrapper,
        skip_projections,
        measure_text,
        viewport_w,
        viewport_h,
        client_rects,
    )
    .map(|(rects, own, published, _)| (rects, own, published))
}

/// The wrapper's own layout results: per flat index rects and text line
/// fragments, in wrapper coordinates.
#[derive(Clone)]
struct RawSolve {
    layouts: Vec<(crate::layout::LayoutRect, usize)>,
    texts: Vec<crate::layout_ng::TextFragment>,
}

/// [`solve_with_client_rects`], also returning the raw results.
fn solve_raw(
    wrapper: &Component,
    skip_projections: bool,
    measure_text: bool,
    viewport_w: f32,
    viewport_h: f32,
    client_rects: bool,
) -> Option<(
    Vec<(u32, crate::layout::LayoutRect)>,
    crate::layout::LayoutRect,
    ClientRectMap,
    RawSolve,
)> {
    let flat = pre_flatten(wrapper);
    // `LayoutEngine::compute` publishes legacy rects when LayoutNG bails.
    // A subtree that bails would then store a legacy line count beside a
    // full NG layout, and followers keep the short y. Under Mode::On, refuse
    // that result so the flush rebuilds with the same engine as `full_rects`.
    if crate::layout_ng::mode() == crate::layout_ng::Mode::On {
        let (layout_cache, texts) =
            crate::layout_ng::layout_tree_with_text(wrapper, &flat, viewport_w, viewport_h)?;
        let published = if client_rects {
            super::client_rects_for(&flat, &layout_cache, &texts)
        } else {
            ClientRectMap::default()
        };
        let (rects, own) = host_rects(&flat, &layout_cache)?;
        let raw = RawSolve {
            layouts: layout_cache,
            texts,
        };
        return Some((rects, own, published, raw));
    }
    let mut engine = LayoutEngine::new();
    // Projections repair floats, tables, inline-block and RTL boxes. A
    // subtree with none of them gets nothing from them even when the rest
    // of the document needs them (checked by W3COS_SPLICE_VERIFY).
    engine.set_skip_projections(skip_projections || can_skip_projections(wrapper));
    engine.set_measure_text(measure_text);
    let results = engine
        .compute(wrapper, &flat, viewport_w, viewport_h)
        .ok()?;
    let published = if client_rects {
        super::client_rects_for(&flat, &results.layout_cache, &results.text_fragments)
    } else {
        ClientRectMap::default()
    };
    let (rects, own) = host_rects(&flat, &results.layout_cache)?;
    let raw = RawSolve {
        layouts: results.layout_cache,
        texts: results.text_fragments,
    };
    Some((rects, own, published, raw))
}

fn host_rects(
    flat: &[crate::layout::FlatNodeInfo],
    layout_cache: &[(crate::layout::LayoutRect, usize)],
) -> Option<(
    Vec<(u32, crate::layout::LayoutRect)>,
    crate::layout::LayoutRect,
)> {
    let mut rects = Vec::with_capacity(layout_cache.len());
    let mut own = None;
    for (rect, index) in layout_cache {
        if *index == 0 {
            own = Some(*rect);
        }
        let id = flat.get(*index).map(flat_host_id).unwrap_or(u32::MAX);
        if id != u32::MAX {
            rects.push((id, *rect));
        }
    }
    Some((rects, own?))
}

fn rect_of(
    rects: &[(u32, crate::layout::LayoutRect)],
    id: u32,
) -> Option<crate::layout::LayoutRect> {
    rects
        .iter()
        .find(|(host, _)| *host == id)
        .map(|(_, rect)| *rect)
}

/// The margin a block's bottom edge contributes to the following sibling's
/// margin strut (CSS 2.1 §8.3.1; LayoutNG NGMarginStrut). `None` when this
/// cannot be read off the retained tree cheaply and exactly: negative margins
/// (the strut keeps positive and negative parts apart), self-collapsing or
/// non-pixel boxes, floats and other formatting-context subtleties.
fn end_margin_strut(component: &Component) -> Option<f32> {
    let style = &component.style;
    let own = px_spacing(style.margin.bottom)?;
    if own < 0.0 {
        return None;
    }
    if matches!(style.display, Display::FlowRoot) || edge_blocks_margin_collapse(component, false) {
        return Some(own);
    }
    let overflow_visible = |overflow: Option<Overflow>| {
        overflow.is_none_or(|overflow| matches!(overflow, Overflow::Visible))
    };
    if !matches!(style.display, Display::Block | Display::ListItem)
        || !matches!(style.overflow, Overflow::Visible)
        || !overflow_visible(style.overflow_x)
        || !overflow_visible(style.overflow_y)
        || !matches!(style.height, Dimension::Auto)
        || !matches!(style.min_height, Dimension::Auto | Dimension::Px(0.0))
        || component
            .children
            .iter()
            .any(|child| child.style.float != Float::None)
    {
        return None;
    }
    let last = component.children.iter().rev().find(|child| {
        !matches!(child.style.display, Display::None)
            && !matches!(child.style.position, Position::Absolute | Position::Fixed)
    })?;
    let inline = matches!(
        last.style.display,
        Display::Inline | Display::InlineBlock | Display::InlineFlex | Display::InlineTable
    ) || (matches!(last.kind, ComponentKind::Text { .. })
        && host_id(last).is_none_or(|id| {
            crate::dom::with_document(|document| {
                document.get_node(NodeId::from_u32(id)).node_type == NodeType::Text
            })
        }));
    if inline {
        // The last line box separates the child margins from this edge.
        return Some(own);
    }
    if !ordinary_block_child(last) {
        return None;
    }
    // A self-collapsing last child lets earlier margins through. Bail.
    let last_id = host_id(last)?;
    let height = crate::dom::with_document(|document| {
        document.get_layout_rect(NodeId::from_u32(last_id)).height
    });
    if !(height > 0.0) {
        return None;
    }
    Some(own.max(end_margin_strut(last)?))
}

/// The kept child before a tail only seeds sibling margin collapsing. A
/// childless block of the same used height whose bottom margin is the
/// anchor's end margin strut is equivalent, and costs one box instead of
/// re-solving its whole subtree (LayoutNG reuses the clean child's layout
/// result; only its NGMarginStrut matters to what follows).
fn anchor_stand_in(source: &Component, flex_item: bool) -> Component {
    let height = host_id(source).map(|id| {
        crate::dom::with_document(|document| document.get_layout_rect(NodeId::from_u32(id)).height)
    });
    let strut = || {
        // A flex item is a formatting context root: no child margin
        // escapes it, and its own margins never collapse.
        if flex_item || edge_blocks_margin_collapse(source, false) {
            Some(source.style.margin.bottom)
        } else {
            end_margin_strut(source).map(Spacing::Px)
        }
    };
    match (height, strut()) {
        (Some(height), Some(bottom)) if height > 0.0 => {
            let mut style = Style {
                display: Display::Block,
                ..Style::default()
            };
            style.height = Dimension::Px(height);
            style.margin = source.style.margin;
            style.margin.bottom = bottom;
            style.font_size = source.style.font_size;
            Component {
                kind: ComponentKind::Box,
                style: style.into(),
                children: Vec::new(),
                on_click: source.on_click.clone(),
                sticky_counter_signal: None,
            }
        }
        _ => source.clone(),
    }
}

/// `can_skip_projections` of the spliced tree without walking all of it.
/// The predicate is "no box anywhere needs a projection", so only the
/// replaced and inserted subtrees can change it (the cache already holds
/// the answer for the rest).
fn spliced_skip_projections(cache: &Cache, removed: &[Component], inserted: &[Component]) -> bool {
    let clean = |list: &[Component]| list.iter().all(can_skip_projections);
    if cache.skip_projections {
        clean(inserted)
    } else if clean(removed) {
        // Whatever needed projections is outside the replaced subtree.
        false
    } else {
        can_skip_projections(&cache.component)
    }
}

fn component_same(left: &Component, right: &Component) -> bool {
    host_id(left) == host_id(right)
        && left.style == right.style
        && left.kind == right.kind
        && left.children.len() == right.children.len()
        && left
            .children
            .iter()
            .zip(&right.children)
            .all(|(left, right)| component_same(left, right))
}

/// Append or remove of the last column-flex item. Earlier items are identical,
/// so their used boxes stay. Anything else, including a middle insert or a
/// text edit, returns `None` and the caller lays out the whole column.
fn column_tail_edit(old: &Component, fresh: &Component) -> Option<bool> {
    if crate::layout_ng::mode() != crate::layout_ng::Mode::On || !column_flex_stacks(old) {
        return None;
    }
    let align = old.style.align_items;
    let content = old.style.align_content;
    if !matches!(align, AlignItems::Stretch | AlignItems::FlexStart)
        || !matches!(content, AlignContent::Stretch | AlignContent::FlexStart)
        || !matches!(old.style.height, Dimension::Auto)
        || !matches!(old.style.min_height, Dimension::Auto)
        || !matches!(old.style.max_height, Dimension::Auto)
    {
        return None;
    }
    let old_len = old.children.len();
    let new_len = fresh.children.len();
    let (prefix, appended) = if new_len == old_len + 1 {
        (old_len, true)
    } else if old_len == new_len + 1 && new_len > 0 {
        (new_len, false)
    } else {
        return None;
    };
    if !old.children[..prefix]
        .iter()
        .zip(&fresh.children[..prefix])
        .all(|(left, right)| component_same(left, right))
    {
        return None;
    }
    if fresh
        .children
        .iter()
        .any(|child| child.style.flex_grow != 0.0)
    {
        return None;
    }
    if appended {
        let child = &fresh.children[prefix];
        if !matches!(
            child.style.align_self,
            AlignSelf::Auto | AlignSelf::Stretch | AlignSelf::FlexStart
        ) {
            return None;
        }
    }
    Some(appended)
}

/// Lay out only the appended item, or drop the removed item's margin box.
/// `None` means the caller must solve the column.
fn reuse_column_tail(
    cache: &Cache,
    path: &[usize],
    fresh: &Component,
    viewport_w: f32,
    viewport_h: f32,
) -> Option<(Vec<(u32, crate::layout::LayoutRect)>, f32, f32, f32)> {
    let old = at_path(&cache.component, path);
    let appended = column_tail_edit(old, fresh)?;
    let Some(column_id) = host_id(old) else {
        return None;
    };
    let column_rect =
        crate::dom::with_document(|document| document.get_layout_rect(NodeId::from_u32(column_id)));
    if appended {
        let child = fresh.children.last()?.clone();
        let last = old.children.last()?;
        let last_id = host_id(last)?;
        let last_bottom = px_spacing(last.style.margin.bottom)?;
        let content_width = content_box_width(&old.style, column_rect.width);
        if !content_width.is_finite() || content_width <= 0.0 {
            return None;
        }
        let skip_child = can_skip_projections(&child);
        let wrapper = wrapper_for(old, content_width, vec![child]);
        let (rects, own) = solve(&wrapper, skip_child, true, viewport_w, viewport_h)?;
        let child_id = host_id(wrapper.children.first()?)?;
        let child_rect = rect_of(&rects, child_id)?;
        let rects: Vec<_> = rects
            .into_iter()
            .filter(|(id, _)| *id != column_id)
            .collect();
        let last_rect = crate::dom::with_document(|document| {
            document.get_layout_rect(NodeId::from_u32(last_id))
        });
        let pad = old.style.padding_lengths();
        let origin_x = column_rect.x + border_side(&old.style, 3) + pad.left;
        let local_x = child_rect.x - own.x;
        let local_y = child_rect.y - own.y;
        let target_x = origin_x + local_x;
        let target_y = last_rect.y + last_rect.height + last_bottom + local_y;
        let dh = own.height;
        if !dh.is_finite() || dh <= 0.0 || !(child_rect.height > 0.0) {
            return None;
        }
        COLUMN_TAIL_REUSES.with(|count| count.set(count.get() + 1));
        Some((rects, target_x - child_rect.x, target_y - child_rect.y, dh))
    } else {
        let removed = old.children.last()?;
        let removed_id = host_id(removed)?;
        let removed_rect = crate::dom::with_document(|document| {
            document.get_layout_rect(NodeId::from_u32(removed_id))
        });
        let top = px_spacing(removed.style.margin.top)?;
        let bottom = px_spacing(removed.style.margin.bottom)?;
        let dh = -(removed_rect.height + top + bottom);
        if !dh.is_finite() || column_rect.height + dh <= 0.0 {
            return None;
        }
        COLUMN_TAIL_REUSES.with(|count| count.set(count.get() + 1));
        Some((Vec::new(), 0.0, 0.0, dh))
    }
}

pub(super) fn try_splice(
    cache: &mut Cache,
    dirty: &[NodeId],
    viewport_w: f32,
    viewport_h: f32,
) -> bool {
    // The retained tree was laid out by the legacy engine (LayoutNG
    // refused it as a whole); a subtree solve would run LayoutNG on the
    // part it accepts and publish different geometry than a full layout.
    if cache.engine.ng_fell_back() {
        if debug_enabled() {
            eprintln!("[splice] refused: full tree is on the legacy engine");
        }
        return false;
    }
    if super::preorder_sizes_mode() == 0 {
        return try_splice_with(cache, dirty, viewport_w, viewport_h, &mut None, &mut None);
    }
    // `W3COS_PREORDER_SIZES`: hold the retained tree's preorder sizes across
    // the splice (its solve swaps subtrees in and out, which would drop
    // them). A refused splice leaves the tree as it was; a child-list
    // commit updates them; any other commit drops them.
    let mut preorder = Some(cache.component.take_preorder());
    let mut hosts = (super::preorder_hosts_mode() != 0).then(|| cache.component.take_hosts());
    let spliced = try_splice_with(
        cache,
        dirty,
        viewport_w,
        viewport_h,
        &mut preorder,
        &mut hosts,
    );
    if let Some(sizes) = preorder {
        cache.component.put_preorder(sizes);
        if let Some(hosts) = hosts {
            cache.component.put_hosts(hosts);
        }
    }
    spliced
}

fn try_splice_with(
    cache: &mut Cache,
    dirty: &[NodeId],
    viewport_w: f32,
    viewport_h: f32,
    preorder: &mut Option<Vec<u32>>,
    hosts: &mut Option<Vec<u32>>,
) -> bool {
    if !cache.splice_owners_ok
        || cache.viewport_w_bits != viewport_w.to_bits()
        || cache.viewport_h_bits != viewport_h.to_bits()
        || cache.sheet_generation != w3cos_dom::stylesheet::generation()
        || dirty.is_empty()
        || w3cos_dom::stylesheet::has_relational_selectors()
    {
        return bail("precondition");
    }
    let plan = match plan_splice(cache, dirty) {
        Ok(Some(plan)) => plan,
        // Every mutation was off-document: the used boxes are unchanged.
        Ok(None) => return true,
        Err(()) => return bail("no safe subtree root"),
    };
    let Some(levels) = splice_ancestor_levels(&cache.component, &plan.path) else {
        return bail("ancestors");
    };
    let node = NodeId::from_u32(plan.node);
    let node_rect = crate::dom::with_document(|document| document.get_layout_rect(node));
    // Solved rects to store, offset to apply to them, height change of the
    // plan node's border box, and the first path level whose following
    // siblings shift (0: the plan node's siblings; the Tail plan grows the
    // plan node itself and starts there too).
    // A child-list splice's raw wrapper results, the inserted items' first
    // wrapper flat index, and their node count.
    let mut children_raw: Option<(RawSolve, usize, usize)> = None;
    // The inserted boxes' raw fragments for the paint journal: (results,
    // first wrapper flat index, node count).
    let mut paint_raw: Option<(RawSolve, usize, usize)> = None;
    let (rects, dx, dy, dh, skip_projections, measure_text, commit, client): (
        Vec<(u32, crate::layout::LayoutRect)>,
        f32,
        f32,
        f32,
        bool,
        bool,
        PlanKind,
        Option<ClientRectMap>,
    ) = match plan.kind {
        PlanKind::Subtree(fresh) => {
            if let Some((rects, dx, dy, dh)) =
                reuse_column_tail(cache, &plan.path, &fresh, viewport_w, viewport_h)
            {
                let skip = if cache.skip_projections {
                    can_skip_projections(&fresh)
                } else {
                    false
                };
                let measure = text_metric_for_fresh(cache, &plan.path, &fresh);
                (
                    rects,
                    dx,
                    dy,
                    dh,
                    skip,
                    measure,
                    PlanKind::Subtree(fresh),
                    None,
                )
            } else {
                let old = at_path(&cache.component, &plan.path);
                let margins = old.style.margin_lengths();
                let width = node_rect.width + margins.left + margins.right;
                if !(node_rect.height > 0.0) || !width.is_finite() || width <= 0.0 {
                    return bail("solve 555");
                }
                // Engine flags are whole-tree properties: a full rebuild would
                // derive them from the spliced tree, so the subtree uses those.
                crate::layout_ng::forget_patch_memo();
                let old_subtree =
                    std::mem::replace(at_path_mut(&mut cache.component, &plan.path), fresh);
                let skip = spliced_skip_projections(
                    cache,
                    std::slice::from_ref(&old_subtree),
                    std::slice::from_ref(at_path(&cache.component, &plan.path)),
                );
                let measure = text_metric_after_edit(cache, &plan.path);
                let fresh =
                    std::mem::replace(at_path_mut(&mut cache.component, &plan.path), old_subtree);
                let parent = at_path(&cache.component, &plan.path[..plan.path.len() - 1]);
                let fresh_len = crate::layout::count_nodes(&fresh);
                let mut wrapper = wrapper_for(parent, width, vec![fresh]);
                let Some((rects, _, client, raw)) =
                    solve_raw(&wrapper, skip, measure, viewport_w, viewport_h, true)
                else {
                    return bail("solve 568");
                };
                if paint_splice_enabled() {
                    paint_raw = Some((raw, 1, fresh_len));
                }
                let Some(new_rect) = rect_of(&rects, plan.node) else {
                    return bail("solve 571");
                };
                if !(new_rect.height > 0.0) || (new_rect.width - node_rect.width).abs() > 0.01 {
                    return bail("solve 574");
                }
                let fresh = wrapper.children.pop().expect("wrapper child");
                (
                    rects,
                    node_rect.x - new_rect.x,
                    node_rect.y - new_rect.y,
                    new_rect.height - node_rect.height,
                    skip,
                    measure,
                    PlanKind::Subtree(fresh),
                    Some(client),
                )
            }
        }
        PlanKind::Tail { keep, tail } => {
            crate::layout_ng::forget_patch_memo();
            let target = at_path_mut(&mut cache.component, &plan.path);
            let removed = target.children.split_off(keep);
            target.children.extend(tail);
            let skip = spliced_skip_projections(
                cache,
                &removed,
                &at_path(&cache.component, &plan.path).children[keep..],
            );
            let measure = text_metric_after_edit(cache, &plan.path);
            crate::layout_ng::forget_patch_memo();
            let target = at_path_mut(&mut cache.component, &plan.path);
            let tail = target.children.split_off(keep);
            target.children.extend(removed);
            let target = at_path(&cache.component, &plan.path);
            let flex_item = is_flex_or_grid(&target.style);
            let anchor = anchor_stand_in(&target.children[keep - 1], flex_item);
            let Some(anchor_id) = host_id(&anchor) else {
                return bail("solve 599");
            };
            let anchor_old = crate::dom::with_document(|document| {
                document.get_layout_rect(NodeId::from_u32(anchor_id))
            });
            let margins = anchor.style.margin_lengths();
            let width = anchor_old.width + margins.left + margins.right;
            if !(anchor_old.height > 0.0) || !width.is_finite() || width <= 0.0 {
                return bail("solve 607");
            }
            let padding_bottom = target.style.padding_lengths().bottom;
            let border_bottom = border_side(&target.style, 2);
            let tail_first = 1 + crate::layout::count_nodes(&anchor);
            let tail_len = tail.iter().map(crate::layout::count_nodes).sum::<usize>();
            let mut children = Vec::with_capacity(tail.len() + 1);
            children.push(anchor);
            children.extend(tail);
            let mut wrapper = wrapper_for(target, width, children);
            let Some((rects, own, client, raw)) =
                solve_raw(&wrapper, skip, measure, viewport_w, viewport_h, true)
            else {
                return bail("solve 616");
            };
            if paint_splice_enabled() {
                paint_raw = Some((raw, tail_first, tail_len));
            }
            let Some(anchor_new) = rect_of(&rects, anchor_id) else {
                return bail("solve 619");
            };
            if (anchor_new.height - anchor_old.height).abs() > 0.01
                || (anchor_new.width - anchor_old.width).abs() > 0.01
            {
                return bail("solve 624");
            }
            let dx = anchor_old.x - anchor_new.x;
            let dy = anchor_old.y - anchor_new.y;
            // The wrapper is a BFC root, so its auto height ends at the last
            // child's bottom margin edge, as the padded parent's content does.
            let content_end = own.y + own.height + dy;
            let new_height = if matches!(target.style.height, Dimension::Px(_)) {
                node_rect.height
            } else {
                content_end + padding_bottom + border_bottom - node_rect.y
            };
            if !new_height.is_finite() || new_height <= 0.0 {
                return bail("solve 633");
            }
            let mut tail = std::mem::take(&mut wrapper.children);
            tail.remove(0);
            let rects = rects
                .into_iter()
                .filter(|(id, _)| *id != anchor_id)
                .collect();
            (
                rects,
                dx,
                dy,
                new_height - node_rect.height,
                skip,
                measure,
                PlanKind::Tail { keep, tail },
                Some(client),
            )
        }
        PlanKind::Children {
            keep,
            removed,
            inserted,
        } => {
            // Engine flags are whole-tree properties: derive them from the
            // spliced child list, then put the old one back until commit.
            crate::layout_ng::forget_patch_memo();
            let count = inserted.len();
            let target = at_path_mut(&mut cache.component, &plan.path);
            let following = target.children.split_off(keep + removed);
            let old_items = target.children.split_off(keep);
            target.children.extend(inserted);
            target.children.extend(following);
            let skip = spliced_skip_projections(
                cache,
                &old_items,
                &at_path(&cache.component, &plan.path).children[keep..keep + count],
            );
            let measure = text_metric_after_edit(cache, &plan.path);
            crate::layout_ng::forget_patch_memo();
            let target = at_path_mut(&mut cache.component, &plan.path);
            let mut rest = target.children.split_off(keep);
            let inserted: Vec<Component> = rest.drain(..count).collect();
            target.children.extend(old_items);
            target.children.extend(rest);
            let target = at_path(&cache.component, &plan.path);
            let anchor = (keep > 0).then(|| anchor_stand_in(&target.children[keep - 1], true));
            let anchor_id = match &anchor {
                Some(anchor) => match host_id(anchor) {
                    Some(id) => Some(id),
                    None => return bail("children anchor"),
                },
                None => None,
            };
            let next = target
                .children
                .get(keep + removed)
                .map(|item| anchor_stand_in(item, true));
            let next_id = next.as_ref().and_then(host_id);
            if next.is_some() && next_id.is_none() {
                return bail("children next");
            }
            let (anchor_old, next_old) = crate::dom::with_document(|document| {
                (
                    anchor_id.map(|id| document.get_layout_rect(NodeId::from_u32(id))),
                    next_id.map(|id| document.get_layout_rect(NodeId::from_u32(id))),
                )
            });
            let padding = target.style.padding_lengths();
            let border_left = border_side(&target.style, 3);
            let border_top = border_side(&target.style, 0);
            let width = match (&anchor, anchor_old) {
                (Some(anchor), Some(anchor_old)) => {
                    if !(anchor_old.height > 0.0) {
                        return bail("children anchor height");
                    }
                    let margins = anchor.style.margin_lengths();
                    anchor_old.width + margins.left + margins.right
                }
                _ => {
                    node_rect.width
                        - padding.left
                        - padding.right
                        - border_left
                        - border_side(&target.style, 1)
                }
            };
            if !width.is_finite() || width <= 0.0 {
                return bail("children width");
            }
            // The anchor seeds the position of the first inserted item (or
            // the content box does); a stand-in for the first following item
            // measures their offset.
            let inserted_first = 1 + anchor.as_ref().map_or(0, crate::layout::count_nodes);
            let inserted_len = inserted
                .iter()
                .map(crate::layout::count_nodes)
                .sum::<usize>();
            let mut children = Vec::with_capacity(count + 2);
            children.extend(anchor);
            children.extend(inserted);
            children.extend(next);
            let mut wrapper = wrapper_for(target, width, children);
            // Pending rects are rebased from the raw results below; only a
            // published map needs the inserted items' rects now.
            let publish = !(retain_lowering_enabled() && super::client_rects_pending());
            let Some((rects, own, client, raw)) =
                solve_raw(&wrapper, skip, measure, viewport_w, viewport_h, publish)
            else {
                return bail("children solve");
            };
            let (dx, dy) = match (anchor_id, anchor_old) {
                (Some(id), Some(anchor_old)) => {
                    let Some(anchor_new) = rect_of(&rects, id) else {
                        return bail("children anchor rect");
                    };
                    if (anchor_new.height - anchor_old.height).abs() > 0.01
                        || (anchor_new.width - anchor_old.width).abs() > 0.01
                    {
                        return bail("children anchor size");
                    }
                    (anchor_old.x - anchor_new.x, anchor_old.y - anchor_new.y)
                }
                _ => (
                    node_rect.x + border_left + padding.left - own.x,
                    node_rect.y + border_top + padding.top - own.y,
                ),
            };
            let dh = match (next_id, next_old) {
                (Some(id), Some(next_old)) => {
                    let Some(next_new) = rect_of(&rects, id) else {
                        return bail("children next rect");
                    };
                    if (next_new.height - next_old.height).abs() > 0.01 {
                        return bail("children next size");
                    }
                    next_new.y + dy - next_old.y
                }
                _ => {
                    let padding_bottom = target.style.padding_lengths().bottom;
                    let border_bottom = border_side(&target.style, 2);
                    own.y + own.height + dy + padding_bottom + border_bottom
                        - node_rect.y
                        - node_rect.height
                }
            };
            if !dh.is_finite() || node_rect.height + dh <= 0.0 {
                return bail("children height");
            }
            if paint_splice_enabled() {
                paint_raw = Some((raw.clone(), inserted_first, inserted_len));
            }
            children_raw = Some((raw, inserted_first, inserted_len));
            if next_id.is_some() {
                wrapper.children.pop();
            }
            let mut inserted = std::mem::take(&mut wrapper.children);
            if anchor_id.is_some() {
                inserted.remove(0);
            }
            let rects = rects
                .into_iter()
                .filter(|(id, _)| Some(*id) != anchor_id && Some(*id) != next_id)
                .collect();
            (
                rects,
                dx,
                dy,
                dh,
                skip,
                measure,
                PlanKind::Children {
                    keep,
                    removed,
                    inserted,
                },
                Some(client),
            )
        }
    };

    // Boxes after the subtree move down by `dh`; auto-height ancestors grow.
    let mut shifted = Vec::new();
    let mut grown = Vec::new();
    if matches!(commit, PlanKind::Tail { .. } | PlanKind::Children { .. }) {
        grown.push(plan.node);
    }
    if matches!(commit, PlanKind::Subtree(_))
        && dh != 0.0
        && !rects.iter().any(|(id, _)| *id == plan.node)
    {
        // A reused column tail does not re-solve the column box. Its
        // auto height still grows or shrinks by the item's margin box.
        // Tail plans already record `plan.node` above; adding it again
        // would apply `dh` twice.
        let column = at_path(&cache.component, &plan.path);
        if !matches!(column.style.height, Dimension::Px(_)) {
            grown.push(plan.node);
        }
    }
    // Deferred CSSOM rects of a child-list splice stay deferred: the pending
    // fragments move with the boxes (Blink keeps fragment geometry on the
    // fragments and answers getClientRects() from them on demand), so the
    // published map needs no per-owner edit.
    let rebase = retain_lowering_enabled()
        && matches!(commit, PlanKind::Children { .. })
        && children_raw.is_some()
        && super::client_rects_pending();
    let mut shifted_owners = std::collections::HashMap::new();
    // `W3COS_PREORDER_HOSTS`: the moved boxes are preorder ranges of the
    // kept host ids (the tree here is still the one they describe).
    let by_range = match (preorder.as_deref(), hosts.as_deref()) {
        (Some(sizes), Some(ids)) if dh != 0.0 && ids.len() == sizes.len() => {
            let (keep, removed) = match &commit {
                PlanKind::Children { keep, removed, .. } => (*keep, *removed),
                _ => (0, 0),
            };
            let (starts, _, removed_end) = preorder_span(sizes, &plan.path, keep, removed);
            let end = |start: usize| start + sizes[start] as usize;
            let mut ranges = Vec::new();
            if matches!(commit, PlanKind::Children { .. }) {
                ranges.push((removed_end, end(starts[plan.path.len()])));
            }
            for level in 0..levels {
                let depth = plan.path.len() - 1 - level;
                let parent = at_path(&cache.component, &plan.path[..depth]);
                if !is_row_flex(parent) {
                    ranges.push((end(starts[depth + 1]), end(starts[depth])));
                }
            }
            let mut out = Vec::new();
            for (a, b) in merged_ranges(&ranges) {
                out.extend(ids[a..b].iter().copied().filter(|&id| id != u32::MAX));
            }
            super::PREORDER_HOSTS_RANGED.with(|n| n.set(n.get() + 1));
            Some(out)
        }
        _ => None,
    };
    let walk_hosts = by_range.is_none() || super::preorder_hosts_mode() == 2;
    if dh != 0.0 {
        if let PlanKind::Children { keep, removed, .. } = &commit {
            // The kept items after the edit move with the following boxes.
            let target = at_path(&cache.component, &plan.path);
            for item in &target.children[keep + removed..] {
                if walk_hosts {
                    collect_hosts(item, &mut shifted);
                }
                if !rebase {
                    super::collect_cssom_owners(item, &mut shifted_owners);
                }
            }
        }
        if !row_item_stays_tallest(&cache.component, &plan.path, levels, dh) {
            return bail("row item height");
        }
        for level in 0..levels {
            let depth = plan.path.len() - 1 - level;
            let parent = at_path(&cache.component, &plan.path[..depth]);
            // Items beside the changed one in a row keep their boxes.
            if !is_row_flex(parent) {
                for sibling in &parent.children[plan.path[depth] + 1..] {
                    if walk_hosts {
                        collect_hosts(sibling, &mut shifted);
                    }
                    if !rebase {
                        super::collect_cssom_owners(sibling, &mut shifted_owners);
                    }
                }
            }
            if let Some(id) = host_id(parent)
                && !matches!(parent.style.height, Dimension::Px(_))
            {
                grown.push(id);
            }
        }
    } else {
        grown.clear();
    }
    if let Some(ranged) = by_range {
        if super::preorder_hosts_mode() == 2 {
            let mut walked = shifted.clone();
            walked.sort_unstable();
            walked.dedup();
            let mut got = ranged.clone();
            got.sort_unstable();
            got.dedup();
            if got != walked {
                super::PREORDER_HOSTS_MISMATCHES.with(|n| n.set(n.get() + 1));
                eprintln!(
                    "[preorder-hosts-verify] mismatch: {} ranged vs {} walked",
                    got.len(),
                    walked.len()
                );
            }
        }
        shifted = ranged;
    }
    dedup_hosts(&mut shifted);
    let shifted_nodes = shifted.len() as u32;

    // CSSOM fragment rects (Blink keeps them on the fragments; here one map
    // per owner). Republish the owners of the rebuilt boxes, and move the
    // owners of shifted boxes, when every box of each such owner is inside
    // that region. An owner on the path itself would change shape.
    let (old_region, new_region): (Vec<&Component>, Vec<&Component>) = match &commit {
        PlanKind::Subtree(fresh) => (vec![at_path(&cache.component, &plan.path)], vec![fresh]),
        PlanKind::Tail { keep, tail } => (
            at_path(&cache.component, &plan.path).children[*keep..]
                .iter()
                .collect(),
            tail.iter().collect(),
        ),
        PlanKind::Children {
            keep,
            removed,
            inserted,
        } => (
            at_path(&cache.component, &plan.path).children[*keep..*keep + *removed]
                .iter()
                .collect(),
            inserted.iter().collect(),
        ),
    };
    let mut old_owners = std::collections::HashMap::new();
    let mut new_owners = std::collections::HashMap::new();
    if !rebase {
        for component in &old_region {
            super::collect_cssom_owners(component, &mut old_owners);
        }
    }
    for component in &new_region {
        super::collect_cssom_owners(component, &mut new_owners);
    }
    let touches_owners =
        !rebase && (!old_owners.is_empty() || !new_owners.is_empty() || !shifted_owners.is_empty());
    if touches_owners {
        // The roots of the region and every ancestor (for a tail plan, the
        // target block too) must not be fragments of an owner themselves.
        let roots_own = old_region
            .iter()
            .chain(new_region.iter())
            .any(|component| carries_cssom_owner(component));
        let ancestor_depths = match &commit {
            PlanKind::Subtree(_) => plan.path.len(),
            PlanKind::Tail { .. } | PlanKind::Children { .. } => plan.path.len() + 1,
        };
        let ancestors_own = (0..ancestor_depths)
            .any(|depth| carries_cssom_owner(at_path(&cache.component, &plan.path[..depth])));
        if client.is_none() || roots_own || ancestors_own {
            return bail("cssom owner shape");
        }
        let totals = cache.owner_counts.get_or_insert_with(|| {
            let mut counts = std::collections::HashMap::new();
            super::collect_cssom_owners(&cache.component, &mut counts);
            counts
        });
        let total = |owner: &u32| totals.get(owner).copied().unwrap_or(0);
        if old_owners
            .iter()
            .any(|(owner, count)| total(owner) != *count)
            || new_owners
                .keys()
                .any(|owner| total(owner) != old_owners.get(owner).copied().unwrap_or(0))
            || shifted_owners
                .iter()
                .any(|(owner, count)| total(owner) != *count)
        {
            return bail("cssom owner outside splice");
        }
    }
    if retain_lowering_enabled() {
        if rebase
            && let (PlanKind::Children { keep, removed, .. }, Some((raw, first, len))) =
                (&commit, children_raw.take())
        {
            let shift = RebaseShift { dx, dy, dh, levels };
            rebase_pending_client_rects(
                cache,
                &plan.path,
                *keep,
                *removed,
                raw,
                first,
                len,
                &shift,
                preorder.as_deref(),
            );
            // Owner totals are recounted when a later splice edits the map.
            cache.owner_counts = None;
        } else {
            let start = debug_enabled().then(std::time::Instant::now);
            super::settle_client_rects(&cache.component);
            if let Some(start) = start {
                eprintln!(
                    "[splice] settled client rects in {}us",
                    start.elapsed().as_micros()
                );
            }
        }
    }
    crate::dom::with_document_mut(|document| {
        for (id, rect) in &rects {
            document.set_layout_rect(
                NodeId::from_u32(*id),
                w3cos_dom::DOMRect::new(rect.x + dx, rect.y + dy, rect.width, rect.height),
            );
        }
        if tail_shift_fast_mode() != 0 {
            document.shift_layout_rects_y(&shifted, dh);
        } else {
            for id in shifted {
                let id = NodeId::from_u32(id);
                let mut rect = document.get_layout_rect(id);
                rect.y += dh;
                document.set_layout_rect(id, rect);
            }
        }
        for id in grown {
            let id = NodeId::from_u32(id);
            let mut rect = document.get_layout_rect(id);
            rect.height += dh;
            document.set_layout_rect(id, rect);
        }
    });
    if touches_owners {
        let mut client = client.unwrap_or_default();
        client.retain(|owner, _| new_owners.contains_key(owner));
        super::splice_client_rects(
            old_owners.keys().copied(),
            client,
            dx,
            dy,
            shifted_owners.keys().copied(),
            dh,
        );
        if let Some(totals) = cache.owner_counts.as_mut() {
            for (owner, count) in &old_owners {
                if let Some(total) = totals.get_mut(owner) {
                    *total -= count;
                    if *total == 0 {
                        totals.remove(owner);
                    }
                }
            }
            for (owner, count) in &new_owners {
                *totals.entry(*owner).or_default() += count;
            }
        }
    }
    if !new_owners.is_empty() {
        // The style-subtree path does not republish CSSOM fragment rects.
        cache.splice_ok = false;
    }
    cache.skip_projections = skip_projections;
    cache.measure_text = measure_text;
    // `W3COS_STYLE_LOCAL`: keep the known style paths (under their old key)
    // so the style route can carry them to the new structure key.
    let carried = (super::style_local_mode() != 0).then(super::take_style_paths);
    crate::layout_ng::forget_patch_memo();
    if let Some(paths) = carried {
        let key = paths.0;
        super::restore_style_paths(paths, key);
    }
    let child_solved = match &commit {
        PlanKind::Children { inserted, .. } => Some(
            inserted
                .iter()
                .map(crate::layout::count_nodes)
                .sum::<usize>() as u32,
        ),
        _ => None,
    };
    if paint_splice_enabled() {
        match paint_raw.take() {
            Some((raw, first, len)) => {
                let target = at_path(&cache.component, &plan.path);
                let (children, inserted) = match &commit {
                    PlanKind::Subtree(fresh) => (None, vec![fresh.clone()]),
                    PlanKind::Tail { keep, tail } => (
                        Some((*keep, target.children.len().saturating_sub(*keep))),
                        tail.clone(),
                    ),
                    PlanKind::Children {
                        keep,
                        removed,
                        inserted,
                    } => (Some((*keep, *removed)), inserted.clone()),
                };
                record_paint_splice(PaintSplice {
                    path: plan.path.clone(),
                    node: plan.node,
                    children,
                    inserted,
                    raw,
                    first,
                    len,
                    dx,
                    dy,
                    dh,
                    levels,
                });
            }
            None => clear_paint_journal(),
        }
    }
    let target = at_path_mut(&mut cache.component, &plan.path);
    SPLICES.with(|count| {
        let (subtree, tail) = count.get();
        count.set(match commit {
            PlanKind::Subtree(_) => (subtree + 1, tail),
            PlanKind::Tail { .. } | PlanKind::Children { .. } => (subtree, tail + 1),
        });
    });
    match commit {
        PlanKind::Subtree(fresh) => {
            // A subtree swap is a one-child splice in the parent's list (the
            // root has none: count the fresh tree).
            match plan.path.split_last() {
                Some((&at, parent)) if preorder_tail_keep() => {
                    if let Some(sizes) = preorder.as_mut() {
                        splice_hosts(hosts, sizes, parent, at, 1, std::slice::from_ref(&fresh));
                        splice_preorder(sizes, parent, at, 1, std::slice::from_ref(&fresh));
                    }
                }
                _ => {
                    *preorder = None;
                    *hosts = None;
                }
            }
            *target = fresh;
        }
        PlanKind::Tail { keep, tail } => {
            if preorder_tail_keep() {
                if let Some(sizes) = preorder.as_mut() {
                    let removed = target.children.len() - keep;
                    splice_hosts(hosts, sizes, &plan.path, keep, removed, &tail);
                    splice_preorder(sizes, &plan.path, keep, removed, &tail);
                }
            } else {
                *preorder = None;
                *hosts = None;
            }
            target.children.truncate(keep);
            target.children.extend(tail);
        }
        PlanKind::Children {
            keep,
            removed,
            inserted,
        } => {
            CHILD_SPLICES.with(|count| count.set(count.get() + 1));
            if let Some(sizes) = preorder.as_mut() {
                splice_hosts(hosts, sizes, &plan.path, keep, removed, &inserted);
                splice_preorder(sizes, &plan.path, keep, removed, &inserted);
            }
            target.children.splice(keep..keep + removed, inserted);
        }
    }
    if preorder.is_none() {
        *hosts = None;
    }
    if super::preorder_hosts_mode() == 2
        && let Some(kept) = hosts.as_ref()
        && *kept != super::preorder_hosts_of(&cache.component)
    {
        super::PREORDER_HOSTS_MISMATCHES.with(|n| n.set(n.get() + 1));
        eprintln!("[preorder-hosts-verify] mismatch after commit");
    }
    if super::preorder_sizes_mode() == 2
        && preorder_tail_keep()
        && let Some(sizes) = preorder.as_ref()
    {
        let counted = super::preorder_sizes_of(&cache.component);
        if counted != *sizes {
            eprintln!(
                "[preorder-sizes-verify] mismatch after commit: kept {} counted {}",
                sizes.len(),
                counted.len()
            );
        }
    }

    cache.node_ids_stale = true;
    cache.width_memos.clear();
    let solved = child_solved.unwrap_or_else(|| {
        crate::layout::count_nodes(at_path(&cache.component, &plan.path)) as u32
    });
    super::note_flush("splice", solved, shifted_nodes);
    if std::env::var_os("W3COS_SPLICE_VERIFY").is_some() || retain_lowering_verify() {
        // The flush holds the cache: settle against the spliced tree here.
        super::settle_client_rects(&cache.component);
        verify(viewport_w, viewport_h, plan.node);
    }
    if retain_lowering_verify() {
        verify_lowering(cache, plan.node);
    }
    true
}

/// `W3COS_REBASE_RANGES` (on by default, `=0` turns it off; with `W3COS_STYLE_LOCAL`): the
/// in-place rebase of the deferred CSSOM rects shifts only the entries of
/// each shifted range (found by binary search in the sorted tail) and
/// renumbers only when the splice changed the box count, instead of testing
/// every following entry. `=verify` also runs the per-entry loop and
/// reports a difference.
fn rebase_ranges_mode() -> bool {
    rebase_ranges_raw() != 0
}

fn rebase_ranges_mode_verify() -> bool {
    rebase_ranges_raw() == 2
}

fn rebase_ranges_raw() -> u8 {
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| match std::env::var("W3COS_REBASE_RANGES").as_deref() {
        Ok("0" | "false") => 0,
        Ok("verify") => 2,
        _ => 1,
    })
}

/// Sorted, disjoint union of half-open index ranges.
fn merged_ranges(ranges: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let mut sorted: Vec<_> = ranges.iter().copied().filter(|(a, b)| a < b).collect();
    sorted.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(sorted.len());
    for (a, b) in sorted {
        match merged.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => merged.push((a, b)),
        }
    }
    merged
}

/// From preorder sizes: the preorder start of each box on `path` (the root
/// first, the target last) and the preorder range of the target's children
/// `keep..keep + removed`.
/// `W3COS_PREORDER_TAIL_KEEP` (on by default, `=0` turns it off; only read under
/// `W3COS_PREORDER_SIZES`): subtree and tail commits (a text edit's
/// paragraph, a stream step) update the kept preorder sizes like a
/// child-list commit does, instead of dropping them for the next splice to
/// recount the whole tree.
fn preorder_tail_keep() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        !matches!(
            std::env::var("W3COS_PREORDER_TAIL_KEEP").as_deref(),
            Ok("0" | "false")
        )
    })
}

/// Update preorder `sizes` (still describing the old tree) for replacing
/// children `keep..keep + removed` of the box at `path` with `inserted`.
fn splice_preorder(
    sizes: &mut Vec<u32>,
    path: &[usize],
    keep: usize,
    removed: usize,
    inserted: &[Component],
) {
    let (starts, removed_start, removed_end) = preorder_span(sizes, path, keep, removed);
    let mut fresh = Vec::new();
    for item in inserted {
        fresh.extend(super::preorder_sizes_of(item));
    }
    let delta = fresh.len() as i64 - (removed_end - removed_start) as i64;
    sizes.splice(removed_start..removed_end, fresh);
    for start in starts {
        sizes[start] = (sizes[start] as i64 + delta) as u32;
    }
}

/// [`splice_preorder`] for the kept host ids; `sizes` still describe the
/// old tree (call it first).
fn splice_hosts(
    hosts: &mut Option<Vec<u32>>,
    sizes: &[u32],
    path: &[usize],
    keep: usize,
    removed: usize,
    inserted: &[Component],
) {
    let Some(ids) = hosts.as_mut() else {
        return;
    };
    if ids.len() != sizes.len() {
        *hosts = None;
        return;
    }
    let (_, removed_start, removed_end) = preorder_span(sizes, path, keep, removed);
    let mut fresh = Vec::new();
    for item in inserted {
        fresh.extend(super::preorder_hosts_of(item));
    }
    ids.splice(removed_start..removed_end, fresh);
}

fn preorder_span(
    sizes: &[u32],
    path: &[usize],
    keep: usize,
    removed: usize,
) -> (Vec<usize>, usize, usize) {
    let mut starts = Vec::with_capacity(path.len() + 1);
    let mut index = 0;
    starts.push(0);
    for &child in path {
        let mut at = index + 1;
        for _ in 0..child {
            at += sizes[at] as usize;
        }
        index = at;
        starts.push(index);
    }
    let mut at = index + 1;
    for _ in 0..keep {
        at += sizes[at] as usize;
    }
    let removed_start = at;
    for _ in 0..removed {
        at += sizes[at] as usize;
    }
    (starts, removed_start, at)
}

struct RebaseShift {
    dx: f32,
    dy: f32,
    dh: f32,
    levels: usize,
}

/// Move the deferred CSSOM layout of the tree a child-list splice is about
/// to commit (`cache.component` is still the old tree) onto the spliced
/// tree: drop the removed items' fragments, add the inserted items' (from
/// the wrapper solve, at `first..first + len`), renumber the flat indices
/// after them, and apply the same shift and growth the splice writes to the
/// DOM rects.
#[allow(clippy::too_many_arguments)]
fn rebase_pending_client_rects(
    cache: &Cache,
    path: &[usize],
    keep: usize,
    removed: usize,
    raw: RawSolve,
    first: usize,
    len: usize,
    shift: &RebaseShift,
    preorder: Option<&[u32]>,
) {
    let Some(pending) = super::take_pending_client_rects() else {
        return;
    };
    let root = &*cache.component;
    if let Some(sizes) = preorder {
        if super::preorder_sizes_mode() == 2 {
            let counted = super::preorder_sizes_of(root);
            if counted.as_slice() != sizes {
                eprintln!(
                    "[preorder-sizes-verify] mismatch: kept {} counted {}",
                    sizes.len(),
                    counted.len()
                );
            }
        }
        let (starts, removed_start, removed_end) = preorder_span(sizes, path, keep, removed);
        let size_at = |start: usize| sizes[start] as usize;
        let sizes: Vec<usize> = starts.iter().map(|&start| size_at(start)).collect();
        let target_index = starts[path.len()];
        rebase_with_spans(
            root,
            pending,
            path,
            raw,
            first,
            len,
            shift,
            &starts,
            &sizes,
            target_index,
            removed_start,
            removed_end,
        );
        return;
    }
    let count = crate::layout::count_nodes;
    // Preorder start of each node on the path, and each one's node count,
    // counted once bottom-up so the large subtree on the path is walked once.
    let mut starts = Vec::with_capacity(path.len() + 1);
    let mut node = root;
    let mut index = 0;
    starts.push(0);
    for &child in path {
        index += 1 + node.children[..child].iter().map(count).sum::<usize>();
        starts.push(index);
        node = &node.children[child];
    }
    let target = node;
    let target_index = index;
    let child_counts: Vec<usize> = target.children.iter().map(|child| count(child)).collect();
    let removed_start = target_index + 1 + child_counts[..keep].iter().sum::<usize>();
    let removed_end = removed_start + child_counts[keep..keep + removed].iter().sum::<usize>();
    let mut sizes = vec![0; path.len() + 1];
    sizes[path.len()] = 1 + child_counts.iter().sum::<usize>();
    for depth in (0..path.len()).rev() {
        let parent = at_path(root, &path[..depth]);
        sizes[depth] = 1
            + sizes[depth + 1]
            + parent
                .children
                .iter()
                .enumerate()
                .filter(|(position, _)| *position != path[depth])
                .map(|(_, child)| count(child))
                .sum::<usize>();
    }
    rebase_with_spans(
        root,
        pending,
        path,
        raw,
        first,
        len,
        shift,
        &starts,
        &sizes,
        target_index,
        removed_start,
        removed_end,
    );
}

/// [`rebase_pending_client_rects`] once the preorder spans are known:
/// `starts`/`sizes` are the preorder start and node count of each box on
/// `path` (root first).
#[allow(clippy::too_many_arguments)]
fn rebase_with_spans(
    root: &Component,
    pending: super::PendingClientRects,
    path: &[usize],
    raw: RawSolve,
    first: usize,
    len: usize,
    shift: &RebaseShift,
    starts: &[usize],
    sizes: &[usize],
    target_index: usize,
    removed_start: usize,
    removed_end: usize,
) {
    let target_end = target_index + sizes[path.len()];
    let mut shifted: Vec<(usize, usize)> = Vec::new();
    let mut grown: Vec<usize> = Vec::new();
    if shift.dh != 0.0 {
        shifted.push((removed_end, target_end));
        grown.push(target_index);
        for level in 0..shift.levels {
            let depth = path.len() - 1 - level;
            let parent = at_path(root, &path[..depth]);
            let child_end = starts[depth + 1] + sizes[depth + 1];
            if !is_row_flex(parent) {
                shifted.push((child_end, starts[depth] + sizes[depth]));
            }
            if host_id(parent).is_some() && !matches!(parent.style.height, Dimension::Px(_)) {
                grown.push(starts[depth]);
            }
        }
    }
    let delta = len as isize - (removed_end - removed_start) as isize;
    let renumber = |index: usize| {
        if index >= removed_end {
            (index as isize + delta) as usize
        } else {
            index
        }
    };
    let in_shift = |index: usize| shifted.iter().any(|&(a, b)| index >= a && index < b);
    if super::style_local_mode() != 0 {
        // `W3COS_STYLE_LOCAL`: edit the index-sorted vectors in place. The
        // removed range becomes the inserted items; only the entries after
        // it are renumbered and shifted (every shifted range starts at or
        // after `removed_end`, every grown box starts before
        // `removed_start`).
        let super::PendingClientRects {
            layouts: mut old_layouts,
            texts: mut old_texts,
            ..
        } = pending;
        if !old_layouts.is_sorted_by_key(|entry| entry.1) {
            old_layouts.sort_by_key(|entry| entry.1);
        }
        if !old_texts.is_sorted_by_key(|fragment| fragment.index) {
            old_texts.sort_by_key(|fragment| fragment.index);
        }
        let inserted = |index: usize| index >= first && index < first + len;
        let mut fresh: Vec<_> = raw
            .layouts
            .into_iter()
            .filter(|entry| inserted(entry.1))
            .map(|(mut rect, index)| {
                rect.x += shift.dx;
                rect.y += shift.dy;
                (rect, removed_start + index - first)
            })
            .collect();
        if !fresh.is_sorted_by_key(|entry| entry.1) {
            fresh.sort_by_key(|entry| entry.1);
        }
        let lo = old_layouts.partition_point(|entry| entry.1 < removed_start);
        let hi = old_layouts.partition_point(|entry| entry.1 < removed_end);
        let fresh_len = fresh.len();
        old_layouts.splice(lo..hi, fresh);
        for &index in &grown {
            let from = old_layouts[..lo].partition_point(|entry| entry.1 < index);
            for entry in old_layouts[from..lo]
                .iter_mut()
                .take_while(|entry| entry.1 == index)
            {
                entry.0.height += shift.dh;
            }
        }
        let ranges = rebase_ranges_mode().then(|| merged_ranges(&shifted));
        let check = (rebase_ranges_mode_verify()).then(|| {
            let mut copy = old_layouts[lo + fresh_len..].to_vec();
            for entry in &mut copy {
                if in_shift(entry.1) {
                    entry.0.y += shift.dh;
                }
                entry.1 = (entry.1 as isize + delta) as usize;
            }
            copy
        });
        if let Some(ranges) = &ranges {
            // `W3COS_REBASE_RANGES`: the tail is sorted by index; shift only
            // the entries inside each (merged) shifted range, and renumber
            // only when the box count changed.
            let tail = &mut old_layouts[lo + fresh_len..];
            if shift.dh != 0.0 {
                for &(a, b) in ranges {
                    let from = tail.partition_point(|entry| entry.1 < a);
                    let to = tail.partition_point(|entry| entry.1 < b);
                    for entry in &mut tail[from..to] {
                        entry.0.y += shift.dh;
                    }
                }
            }
            if delta != 0 {
                for entry in tail.iter_mut() {
                    entry.1 = (entry.1 as isize + delta) as usize;
                }
            }
        } else {
            for entry in &mut old_layouts[lo + fresh_len..] {
                if in_shift(entry.1) {
                    entry.0.y += shift.dh;
                }
                entry.1 = (entry.1 as isize + delta) as usize;
            }
        }
        if let Some(copy) = check
            && copy
                .iter()
                .zip(&old_layouts[lo + fresh_len..])
                .any(|(a, b)| a.1 != b.1 || a.0.y.to_bits() != b.0.y.to_bits())
        {
            eprintln!("[rebase-ranges-verify] layout mismatch");
        }
        let mut fresh: Vec<_> = raw
            .texts
            .into_iter()
            .filter(|fragment| inserted(fragment.index))
            .map(|mut fragment| {
                fragment.rect.x += shift.dx;
                fragment.rect.y += shift.dy;
                fragment.index = removed_start + fragment.index - first;
                fragment
            })
            .collect();
        if !fresh.is_sorted_by_key(|fragment| fragment.index) {
            fresh.sort_by_key(|fragment| fragment.index);
        }
        let lo = old_texts.partition_point(|fragment| fragment.index < removed_start);
        let hi = old_texts.partition_point(|fragment| fragment.index < removed_end);
        let fresh_len = fresh.len();
        old_texts.splice(lo..hi, fresh);
        let check = (rebase_ranges_mode_verify()).then(|| {
            old_texts[lo + fresh_len..]
                .iter()
                .map(|fragment| {
                    let y = if in_shift(fragment.index) {
                        fragment.rect.y + shift.dh
                    } else {
                        fragment.rect.y
                    };
                    ((fragment.index as isize + delta) as usize, y)
                })
                .collect::<Vec<_>>()
        });
        if let Some(ranges) = &ranges {
            let tail = &mut old_texts[lo + fresh_len..];
            if shift.dh != 0.0 {
                for &(a, b) in ranges {
                    let from = tail.partition_point(|fragment| fragment.index < a);
                    let to = tail.partition_point(|fragment| fragment.index < b);
                    for fragment in &mut tail[from..to] {
                        fragment.rect.y += shift.dh;
                    }
                }
            }
            if delta != 0 {
                for fragment in tail.iter_mut() {
                    fragment.index = (fragment.index as isize + delta) as usize;
                }
            }
        } else {
            for fragment in &mut old_texts[lo + fresh_len..] {
                if in_shift(fragment.index) {
                    fragment.rect.y += shift.dh;
                }
                fragment.index = (fragment.index as isize + delta) as usize;
            }
        }
        if let Some(copy) = check
            && copy
                .iter()
                .zip(&old_texts[lo + fresh_len..])
                .any(|(a, b)| a.0 != b.index || a.1.to_bits() != b.rect.y.to_bits())
        {
            eprintln!("[rebase-ranges-verify] text mismatch");
        }
        super::forget_client_plan();
        super::defer_client_rects(None, old_layouts, old_texts);
        return;
    }
    let removed_range = |index: usize| index >= removed_start && index < removed_end;
    let mut layouts = Vec::with_capacity(pending.layouts.len() + len);
    let mut texts = Vec::with_capacity(pending.texts.len());
    let super::PendingClientRects {
        layouts: old_layouts,
        texts: old_texts,
        ..
    } = pending;
    for (mut rect, index) in old_layouts {
        if removed_range(index) {
            continue;
        }
        if in_shift(index) {
            rect.y += shift.dh;
        }
        if grown.contains(&index) {
            rect.height += shift.dh;
        }
        layouts.push((rect, renumber(index)));
    }
    let inserted = |index: usize| index >= first && index < first + len;
    for (mut rect, index) in raw.layouts {
        if inserted(index) {
            rect.x += shift.dx;
            rect.y += shift.dy;
            layouts.push((rect, removed_start + index - first));
        }
    }
    for mut fragment in old_texts {
        if removed_range(fragment.index) {
            continue;
        }
        if in_shift(fragment.index) {
            fragment.rect.y += shift.dh;
        }
        fragment.index = renumber(fragment.index);
        texts.push(fragment);
    }
    for mut fragment in raw.texts {
        if inserted(fragment.index) {
            fragment.rect.x += shift.dx;
            fragment.rect.y += shift.dy;
            fragment.index = removed_start + fragment.index - first;
            texts.push(fragment);
        }
    }
    // The box-tree plan is keyed by the old tree: publish without it.
    super::forget_client_plan();
    super::defer_client_rects(None, layouts, texts);
}

#[cfg(test)]
thread_local! {
    /// Test override of [`paint_splice_enabled`] (also turns verify on).
    pub(super) static FORCE_PAINT_SPLICE: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

/// A headless paint after a structural splice applies the splice to its
/// own tree and layout instead of lowering and laying out the whole
/// document again (Blink paints from the layout tree the lifecycle just
/// updated). LayoutNG only; on by default, `W3COS_PAINT_SPLICE=0` rebuilds.
pub(super) fn paint_splice_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    #[cfg(test)]
    if let Some(forced) = FORCE_PAINT_SPLICE.with(std::cell::Cell::get) {
        return forced && crate::layout_ng::mode() == crate::layout_ng::Mode::On;
    }
    *ON.get_or_init(|| {
        !matches!(
            std::env::var("W3COS_PAINT_SPLICE").ok().as_deref(),
            Some("0") | Some("false")
        )
    }) && crate::layout_ng::mode() == crate::layout_ng::Mode::On
}

/// `W3COS_PAINT_SPLICE_VERIFY=1`: compare each spliced paint layout with a
/// full lowering and layout.
pub(super) fn paint_splice_verify() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    #[cfg(test)]
    if FORCE_PAINT_SPLICE.with(std::cell::Cell::get).is_some() {
        return true;
    }
    *ON.get_or_init(|| std::env::var("W3COS_PAINT_SPLICE_VERIFY").is_ok_and(|value| value == "1"))
}

/// One committed splice, as the paint cache replays it: the same subtree
/// or child-list replacement, the wrapper's raw fragments for the inserted
/// boxes, and the shift and growth the splice wrote to the DOM rects.
pub(super) struct PaintSplice {
    /// Subtree: the replaced box. Child list: the box whose children change.
    path: Vec<usize>,
    node: u32,
    /// `Some((keep, removed))`: children `keep..keep + removed` are replaced.
    children: Option<(usize, usize)>,
    inserted: Vec<Component>,
    raw: RawSolve,
    first: usize,
    len: usize,
    dx: f32,
    dy: f32,
    dh: f32,
    levels: usize,
}

/// Splices committed to the flush cache since `epoch` began. The paint
/// cache remembers the (epoch, count) it matches; any other flush cache
/// change starts a new epoch.
struct PaintJournal {
    epoch: u64,
    entries: Vec<PaintSplice>,
    nodes: usize,
}

/// Past this many replayed boxes a full paint layout is as cheap.
const PAINT_JOURNAL_MAX_NODES: usize = 32_768;
const PAINT_JOURNAL_MAX_ENTRIES: usize = 256;

thread_local! {
    static PAINT_JOURNAL: std::cell::RefCell<PaintJournal> = const {
        std::cell::RefCell::new(PaintJournal {
            epoch: 0,
            entries: Vec::new(),
            nodes: 0,
        })
    };
}

/// The flush cache changed other than by a recorded splice.
pub(super) fn clear_paint_journal() {
    PAINT_JOURNAL.with(|journal| {
        let mut journal = journal.borrow_mut();
        journal.epoch += 1;
        journal.entries.clear();
        journal.nodes = 0;
    });
}

/// `W3COS_PAINT_JOURNAL_TRIM=1`: after the paint tree replayed every
/// journaled splice, the journal drops them (a new epoch, so no older
/// position stays valid). Without it the entries pile up until a size cap
/// clears the journal and the next paint rebuilds the whole tree (one frame
/// in ~51 of a text stream). On by default (`=0` turns it off).
pub(super) fn paint_journal_trim_enabled() -> bool {
    #[cfg(test)]
    if let Some(enabled) = PAINT_JOURNAL_TRIM_OVERRIDE.with(std::cell::Cell::get) {
        return enabled;
    }
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        !std::env::var("W3COS_PAINT_JOURNAL_TRIM").is_ok_and(|v| v == "0" || v == "false")
    })
}

#[cfg(test)]
thread_local! {
    pub(super) static PAINT_JOURNAL_TRIM_OVERRIDE: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

/// Drop every journaled splice and start a new epoch; returns the position
/// matching the flush cache now.
pub(super) fn trim_paint_journal() -> (u64, usize) {
    PAINT_JOURNAL.with(|journal| {
        let mut journal = journal.borrow_mut();
        journal.epoch += 1;
        journal.entries.clear();
        journal.nodes = 0;
        (journal.epoch, 0)
    })
}

/// The journal position matching the flush cache now.
pub(super) fn paint_journal_point() -> (u64, usize) {
    PAINT_JOURNAL.with(|journal| {
        let journal = journal.borrow();
        (journal.epoch, journal.entries.len())
    })
}

fn record_paint_splice(entry: PaintSplice) {
    PAINT_JOURNAL.with(|journal| {
        let mut journal = journal.borrow_mut();
        journal.nodes += entry.len.max(1);
        journal.entries.push(entry);
        if journal.nodes > PAINT_JOURNAL_MAX_NODES
            || journal.entries.len() > PAINT_JOURNAL_MAX_ENTRIES
        {
            journal.epoch += 1;
            journal.entries.clear();
            journal.nodes = 0;
        }
    });
}

/// Replay the splices after `point` onto a paint tree and layout that
/// match the flush cache at `point`. `None`: the journal no longer reaches
/// back to `point`. `Some(false)`: a splice did not fit and the inputs may
/// be partly edited.
pub(super) fn replay_paint_splices(
    point: (u64, usize),
    root: &mut Component,
    layouts: &mut Vec<(crate::layout::LayoutRect, usize)>,
    texts: &mut Vec<crate::layout_ng::TextFragment>,
    sizes: &mut Option<Vec<u32>>,
) -> Option<(bool, usize)> {
    PAINT_JOURNAL.with(|journal| {
        let journal = journal.borrow();
        if journal.epoch != point.0 || journal.entries.len() < point.1 {
            return None;
        }
        let entries = &journal.entries[point.1..];
        let incremental = paint_splice_incremental();
        if !incremental {
            *sizes = None;
        } else if sizes.as_ref().is_some_and(|known| {
            known.first().map(|&root_size| root_size as usize) != Some(known.len())
        }) {
            *sizes = None;
        }
        LAST_PAINT_CHANGES.with(|slot| slot.borrow_mut().take());
        // Only a consumer pays for the per-box set.
        let mut changes = paint_changes_wanted().then(PaintChanges::default);
        let verify = incremental && paint_splice_incremental_verify();
        let mut shadow = verify.then(|| layouts.clone());
        let mut lazy = LazyLayoutShifts::default();
        for entry in entries {
            let Some(geometry) = paint_splice_geometry(root, entry, sizes.as_deref(), verify)
            else {
                return Some((false, 0));
            };
            changes = changes.and_then(|mut changes| {
                let inserted = entry
                    .inserted
                    .iter()
                    .map(crate::layout::count_nodes)
                    .sum::<usize>();
                changes.add(&geometry, inserted)?;
                Some(changes)
            });
            if !(incremental && lazy.add(layouts, entry, &geometry)) {
                lazy.flush(layouts);
                splice_layouts_eagerly(layouts, entry, &geometry);
            }
            if let Some(shadow) = shadow.as_mut() {
                splice_layouts_eagerly(shadow, entry, &geometry);
            }
            splice_texts(texts, entry, &geometry);
            splice_tree(root, entry, &geometry, sizes);
        }
        lazy.flush(layouts);
        let changes = changes.map(|mut changes| {
            changes.generation = crate::incremental_layout::paint_layout_generation();
            changes
        });
        LAST_PAINT_CHANGES.with(|slot| *slot.borrow_mut() = changes);
        if let Some(shadow) = shadow {
            let same = shadow.len() == layouts.len()
                && shadow.iter().zip(layouts.iter()).all(|((a, i), (b, j))| {
                    i == j
                        && a.x.to_bits() == b.x.to_bits()
                        && a.y.to_bits() == b.y.to_bits()
                        && a.width.to_bits() == b.width.to_bits()
                        && a.height.to_bits() == b.height.to_bits()
                });
            if !same {
                eprintln!(
                    "[paint-splice-incremental-verify] layout mismatch after {} entries",
                    entries.len()
                );
                #[cfg(test)]
                panic!("incremental paint splice layouts differ from the eager replay");
                #[allow(unreachable_code)]
                {
                    *layouts = shadow;
                }
            }
        }
        Some((true, entries.len()))
    })
}

/// Replay paint splices with kept preorder subtree sizes (instead of
/// recounting the siblings of every box on the path) and with the vertical
/// shifts of same-size leaf splices applied to the layout list once per
/// replay instead of once per splice. On by default;
/// `W3COS_PAINT_SPLICE_INCREMENTAL=0` restores the eager replay.
pub(super) fn paint_splice_incremental() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    #[cfg(test)]
    if FORCE_PAINT_SPLICE.with(std::cell::Cell::get).is_some() {
        return true;
    }
    *ON.get_or_init(|| {
        std::env::var("W3COS_PAINT_SPLICE_INCREMENTAL").map_or(true, |value| value != "0")
    })
}

/// `W3COS_PAINT_SPLICE_INCREMENTAL_VERIFY=1`: also count sizes and replay
/// the layout list the eager way, compare, and keep the eager result.
fn paint_splice_incremental_verify() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    #[cfg(test)]
    if FORCE_PAINT_SPLICE.with(std::cell::Cell::get).is_some() {
        return true;
    }
    *ON.get_or_init(|| {
        std::env::var("W3COS_PAINT_SPLICE_INCREMENTAL_VERIFY").is_ok_and(|value| value == "1")
    })
}

/// Preorder subtree sizes of `component`, appended to `out`.
pub(super) fn push_subtree_sizes(component: &Component, out: &mut Vec<u32>) -> usize {
    let at = out.len();
    out.push(0);
    let mut size = 1;
    for child in &component.children {
        size += push_subtree_sizes(child, out);
    }
    out[at] = size as u32;
    size
}

/// Where one paint splice lands in the preorder of the tree it is applied
/// to, and which boxes it moves and grows.
/// Per box of the paint tree after a replay: whether a replayed splice
/// replaced, moved or grew it (Blink's layout tells paint which objects need
/// a paint invalidation instead of paint diffing the whole tree).
#[derive(Clone, Debug, Default)]
pub(crate) struct PaintChanges {
    /// Set by the first splice.
    pub(crate) started: bool,
    /// Boxes in the tree after the last splice folded in.
    pub(crate) len: usize,
    /// Sorted, merged, disjoint `[start, end)` index ranges of boxes that
    /// were inserted or are on a splice's path or grew: their inputs or size
    /// may have changed.
    pub(crate) changed: Vec<(u32, u32)>,
    /// Same form: boxes the layout only moved (same inputs, new position).
    pub(crate) moved: Vec<(u32, u32)>,
    /// The paint layout that replayed these splices.
    pub(crate) generation: u64,
    /// Boxes in the tree before the first splice.
    pub(crate) base_len: usize,
    /// Where a box outside `changed` was before the first splice: sorted
    /// `(first index now, offset)` pieces, the box at `i` was at
    /// `i + offset` of the piece holding `i` (empty: no splice yet).
    pub(crate) segments: Vec<(u32, i64)>,
}

/// Sort and merge `[start, end)` ranges in place (adjacent ones join).
pub(crate) fn normalize_ranges(ranges: &mut Vec<(u32, u32)>) {
    ranges.retain(|&(a, b)| a < b);
    ranges.sort_unstable();
    let mut out = 0usize;
    for i in 0..ranges.len() {
        let (a, b) = ranges[i];
        if out > 0 && a <= ranges[out - 1].1 {
            ranges[out - 1].1 = ranges[out - 1].1.max(b);
        } else {
            ranges[out] = (a, b);
            out += 1;
        }
    }
    ranges.truncate(out);
}

/// Whether `index` lies in one of the sorted disjoint `ranges`.
pub(crate) fn ranges_contain(ranges: &[(u32, u32)], index: usize) -> bool {
    let index = index as u32;
    let at = ranges.partition_point(|&(_, end)| end <= index);
    ranges.get(at).is_some_and(|&(start, _)| start <= index)
}

/// The end of the gap starting at `index` (no range covers
/// `index..returned`); `index` itself when a range covers it.
pub(crate) fn ranges_gap_end(ranges: &[(u32, u32)], index: usize, len: usize) -> usize {
    let at = ranges.partition_point(|&(_, end)| end as usize <= index);
    match ranges.get(at) {
        Some(&(start, _)) => (start as usize).max(index).min(len),
        None => len,
    }
}

impl PaintChanges {
    /// Fold one splice in; its geometry is in the indices of the tree it
    /// edits (after the splices before it). `None`: sizes do not add up.
    /// O(ranges) per splice: earlier ranges are remapped, not boxes.
    fn add(&mut self, geometry: &SpliceGeometry, inserted: usize) -> Option<()> {
        if !self.started {
            self.started = true;
            self.len = geometry.total;
            self.base_len = geometry.total;
            self.segments = vec![(0, 0)];
        }
        let (start, end) = (geometry.region_start, geometry.region_end);
        if self.len != geometry.total || end > self.len || start > end {
            return None;
        }
        let delta = inserted as i64 - (end - start) as i64;
        let new_len = (self.len as i64 + delta) as usize;
        let map = |old: usize| -> usize {
            if old >= end {
                (old as i64 + delta) as usize
            } else {
                old
            }
        };
        // A range's part before the region stays, its part after shifts;
        // its part inside the region is replaced by the inserted boxes.
        let remap = |ranges: &mut Vec<(u32, u32)>| {
            let mut next = Vec::with_capacity(ranges.len() + 2);
            for &(a, b) in ranges.iter() {
                let (a, b) = (a as usize, b as usize);
                if a < start {
                    next.push((a as u32, b.min(start) as u32));
                }
                if b > end {
                    next.push((map(a.max(end)) as u32, map(b) as u32));
                }
            }
            *ranges = next;
        };
        remap(&mut self.changed);
        remap(&mut self.moved);
        // The pieces before the region stay; the ones after it shift by
        // `delta` and their offset to the first tree shrinks by `delta`.
        // (Boxes inside the region are changed: any offset will do.)
        if delta != 0 {
            let old_len = self.len;
            let pieces = std::mem::take(&mut self.segments);
            let mut next = Vec::with_capacity(pieces.len() + 1);
            for &(from, offset) in &pieces {
                if (from as usize) < start {
                    next.push((from, offset));
                }
            }
            for (k, &(from, offset)) in pieces.iter().enumerate() {
                let to = pieces
                    .get(k + 1)
                    .map_or(old_len, |&(next, _)| next as usize);
                let a = (from as usize).max(end);
                if a < to {
                    next.push(((a as i64 + delta) as u32, offset - delta));
                }
            }
            next.dedup_by(|b, a| a.1 == b.1);
            if next.first().is_none_or(|&(from, _)| from != 0) {
                next.insert(0, (0, 0));
            }
            self.segments = next;
        }
        self.changed.push((start as u32, (start + inserted) as u32));
        for &index in geometry.starts.iter().chain(&geometry.grown) {
            let at = map(index);
            if at >= new_len {
                return None;
            }
            self.changed.push((at as u32, at as u32 + 1));
        }
        for &(from, to) in &geometry.shifted {
            // Parts before and after the replaced region (none inside it).
            for (a, b) in [(from, to.min(start)), (from.max(end), to)] {
                if a >= b {
                    continue;
                }
                let (a, b) = (map(a), map(b - 1) + 1);
                if b > new_len {
                    return None;
                }
                self.moved.push((a as u32, b as u32));
            }
        }
        normalize_ranges(&mut self.changed);
        normalize_ranges(&mut self.moved);
        self.len = new_len;
        Some(())
    }

    /// Per box: changed or moved (for verify and dumps only).
    pub(crate) fn touched_bitmap(&self) -> Vec<bool> {
        let mut touched = vec![false; self.len];
        for &(a, b) in self.changed.iter().chain(&self.moved) {
            touched[a as usize..(b as usize).min(self.len)].fill(true);
        }
        touched
    }
}

thread_local! {
    /// Set by a paint splice replay, taken by the paint build of the same
    /// frame.
    static LAST_PAINT_CHANGES: std::cell::RefCell<Option<PaintChanges>> =
        const { std::cell::RefCell::new(None) };
}

/// `W3COS_PAINT_CHANGES_VERIFY=1`: record [`PaintChanges`] and have the
/// headless paint check them against its full diff. Off by default (no
/// consumer uses the set yet).
pub(crate) fn paint_changes_wanted() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("W3COS_PAINT_CHANGES_VERIFY").is_ok_and(|v| v == "1"))
        || crate::paint_artifact::paint_touched_mode() != 0
}

/// The changes of the last paint replay, without taking them.
pub(crate) fn with_paint_changes<R>(read: impl FnOnce(Option<&PaintChanges>) -> R) -> R {
    LAST_PAINT_CHANGES.with(|slot| read(slot.borrow().as_ref()))
}

/// The changes of the last paint replay, once.
pub(crate) fn take_paint_changes() -> Option<PaintChanges> {
    LAST_PAINT_CHANGES.with(|slot| slot.borrow_mut().take())
}

struct SpliceGeometry {
    /// Boxes in the tree before the splice.
    total: usize,
    region_start: usize,
    region_end: usize,
    /// Preorder start of each box on the path, root first, target last.
    starts: Vec<usize>,
    shifted: Vec<(usize, usize)>,
    grown: Vec<usize>,
}

fn paint_splice_geometry(
    root: &Component,
    entry: &PaintSplice,
    sizes: Option<&[u32]>,
    verify: bool,
) -> Option<SpliceGeometry> {
    let count = crate::layout::count_nodes;
    let path = &entry.path;
    if entry.levels > path.len() {
        return None;
    }
    // `sizes[i]`: node count of the subtree starting at preorder `i`. A
    // child's start is its parent's plus one plus its preceding siblings'
    // sizes, found by hopping over them instead of walking them.
    let hop = |sizes: &[u32], parent_start: usize, children: usize| -> Option<usize> {
        let mut at = parent_start + 1;
        for _ in 0..children {
            at += *sizes.get(at)? as usize;
        }
        Some(at)
    };
    let mut starts = Vec::with_capacity(path.len() + 1);
    let mut node: &Component = root;
    let mut index = 0;
    starts.push(0);
    for &child in path {
        if child >= node.children.len() {
            return None;
        }
        index = match sizes {
            Some(sizes) => hop(sizes, index, child)?,
            None => index + 1 + node.children[..child].iter().map(count).sum::<usize>(),
        };
        starts.push(index);
        node = &node.children[child];
    }
    if host_id(node) != Some(entry.node) {
        return None;
    }
    let target = node;
    let target_index = index;
    let target_size = match sizes {
        Some(sizes) => *sizes.get(target_index)? as usize,
        None => count(target),
    };
    let (region_start, region_end) = match entry.children {
        None => (target_index, target_index + target_size),
        Some((keep, removed)) => {
            if keep + removed > target.children.len() {
                return None;
            }
            match sizes {
                Some(sizes) => {
                    let start = hop(sizes, target_index, keep)?;
                    let end = hop(sizes, start - 1, removed)?;
                    (start, end)
                }
                None => {
                    let start =
                        target_index + 1 + target.children[..keep].iter().map(count).sum::<usize>();
                    (
                        start,
                        start
                            + target.children[keep..keep + removed]
                                .iter()
                                .map(count)
                                .sum::<usize>(),
                    )
                }
            }
        }
    };
    // An ancestor's size: itself and the preceding siblings on the path
    // (already summed into `starts`), the path child, and the following
    // siblings; only the latter are counted here.
    let mut ancestor_sizes = vec![0; path.len() + 1];
    ancestor_sizes[path.len()] = target_size;
    for depth in (0..path.len()).rev() {
        ancestor_sizes[depth] = match sizes {
            Some(sizes) => *sizes.get(starts[depth])? as usize,
            None => {
                let parent = at_path(root, &path[..depth]);
                (starts[depth + 1] - starts[depth])
                    + ancestor_sizes[depth + 1]
                    + parent.children[path[depth] + 1..]
                        .iter()
                        .map(count)
                        .sum::<usize>()
            }
        };
    }
    if verify && sizes.is_some() {
        let counted = paint_splice_geometry(root, entry, None, false)?;
        if counted.region_start != region_start
            || counted.region_end != region_end
            || counted.starts != starts
        {
            eprintln!(
                "[paint-splice-incremental-verify] sizes mismatch: kept ({region_start}, {region_end}) counted ({}, {})",
                counted.region_start, counted.region_end
            );
            #[cfg(test)]
            panic!("kept subtree sizes differ from counted ones");
            #[allow(unreachable_code)]
            return Some(counted);
        }
    }
    // The same boxes `try_splice` moves and grows in the DOM, as preorder
    // ranges (every box of a shifted subtree moves, not only its hosts).
    let mut shifted: Vec<(usize, usize)> = Vec::new();
    let mut grown: Vec<usize> = Vec::new();
    if entry.dh != 0.0 {
        if entry.children.is_some() {
            shifted.push((region_end, target_index + target_size));
            grown.push(target_index);
        }
        for level in 0..entry.levels {
            let depth = path.len() - 1 - level;
            let parent = at_path(root, &path[..depth]);
            let child_end = starts[depth + 1] + ancestor_sizes[depth + 1];
            if !is_row_flex(parent) {
                shifted.push((child_end, starts[depth] + ancestor_sizes[depth]));
            }
            if host_id(parent).is_some() && !matches!(parent.style.height, Dimension::Px(_)) {
                grown.push(starts[depth]);
            }
        }
    }
    Some(SpliceGeometry {
        total: ancestor_sizes[0],
        region_start,
        region_end,
        starts,
        shifted,
        grown,
    })
}

/// Vertical shifts and growth of same-size leaf splices, applied to the
/// layout list in one pass when the replay ends (or before a splice that
/// renumbers). The result is the eager replay's: the replaced box goes
/// where the eager list edit puts it, and every box gets the same f32
/// additions in the same order (a replaced box only those of later
/// splices).
#[derive(Default)]
struct LazyLayoutShifts {
    /// Preorder index -> position in the layout list (`u32::MAX`: none).
    position_of: Vec<u32>,
    mapped: bool,
    /// Per deferred splice: its shifted ranges and dh.
    splices: Vec<(Vec<(usize, usize)>, f32)>,
    /// Deferred growth: (layout position, splice order, dh).
    growth: Vec<(usize, usize, f32)>,
    /// Layout positions replaced by a deferred splice, with the number of
    /// deferred splices before it (which no longer apply to that box).
    since: Vec<(usize, usize)>,
}

impl LazyLayoutShifts {
    fn map(&mut self, layouts: &[(crate::layout::LayoutRect, usize)]) {
        self.mapped = true;
        let len = layouts
            .iter()
            .map(|(_, index)| index + 1)
            .max()
            .unwrap_or(0);
        self.position_of = vec![u32::MAX; len];
        for (position, (_, index)) in layouts.iter().enumerate() {
            let slot = &mut self.position_of[*index];
            if *slot != u32::MAX {
                // A repeated index: positions are not one per box.
                self.position_of.clear();
                return;
            }
            *slot = position as u32;
        }
    }

    fn position(&self, index: usize) -> u32 {
        self.position_of.get(index).copied().unwrap_or(u32::MAX)
    }

    /// Defers `entry` when it replaces one leaf box by one box at the same
    /// preorder index; false (touching nothing) when it must be applied
    /// eagerly.
    fn add(
        &mut self,
        layouts: &mut [(crate::layout::LayoutRect, usize)],
        entry: &PaintSplice,
        geometry: &SpliceGeometry,
    ) -> bool {
        if entry.children.is_some()
            || entry.len != 1
            || geometry.region_end != geometry.region_start + 1
        {
            return false;
        }
        if !self.mapped {
            self.map(layouts);
        }
        if self.position_of.is_empty() && !layouts.is_empty() {
            return false;
        }
        let mut inserted = entry
            .raw
            .layouts
            .iter()
            .filter(|(_, index)| *index == entry.first);
        let replacement = inserted.next();
        if inserted.next().is_some() {
            return false;
        }
        let target = geometry.region_start;
        let old = self.position(target);
        match (replacement, old) {
            (None, u32::MAX) => {}
            (Some(&(mut rect, _)), old) if old != u32::MAX => {
                let old = old as usize;
                rect.x += entry.dx;
                rect.y += entry.dy;
                // The eager edit puts the new box before the first box at
                // or after the region in list order.
                let at = layouts
                    .iter()
                    .position(|(_, index)| *index >= target)
                    .unwrap_or(old);
                // The replaced box's deferred state goes with it.
                self.since.retain(|&(position, _)| position != old);
                self.growth.retain(|&(position, _, _)| position != old);
                if at < old {
                    layouts[at..=old].rotate_right(1);
                    for (_, index) in &layouts[at + 1..=old] {
                        self.position_of[*index] += 1;
                    }
                    let moved = |position: &mut usize| {
                        if *position >= at && *position < old {
                            *position += 1;
                        }
                    };
                    self.since
                        .iter_mut()
                        .for_each(|(position, _)| moved(position));
                    self.growth
                        .iter_mut()
                        .for_each(|(position, _, _)| moved(position));
                }
                layouts[at] = (rect, target);
                self.position_of[target] = at as u32;
                self.since.push((at, self.splices.len()));
            }
            _ => return false,
        }
        if entry.dh != 0.0 {
            let order = self.splices.len();
            for &grown in &geometry.grown {
                let position = self.position(grown);
                if position != u32::MAX {
                    self.growth.push((position as usize, order, entry.dh));
                }
            }
            self.splices.push((geometry.shifted.clone(), entry.dh));
        }
        true
    }

    fn flush(&mut self, layouts: &mut [(crate::layout::LayoutRect, usize)]) {
        let splices = std::mem::take(&mut self.splices);
        let since = std::mem::take(&mut self.since);
        let growth = std::mem::take(&mut self.growth);
        self.mapped = false;
        self.position_of = Vec::new();
        if splices.is_empty() {
            return;
        }
        let after = |position: usize| {
            since
                .iter()
                .find(|&&(at, _)| at == position)
                .map_or(0, |&(_, after)| after)
        };
        // Cut the preorder at every range end; each segment between cuts is
        // shifted by the same splices.
        let mut cuts: Vec<usize> = splices
            .iter()
            .flat_map(|(ranges, _)| ranges.iter().flat_map(|&(a, b)| [a, b]))
            .collect();
        cuts.sort_unstable();
        cuts.dedup();
        let segment_splices: Vec<Vec<usize>> = (0..cuts.len().saturating_sub(1))
            .map(|segment| {
                let at = cuts[segment];
                splices
                    .iter()
                    .enumerate()
                    .filter(|(_, (ranges, _))| ranges.iter().any(|&(a, b)| at >= a && at < b))
                    .map(|(order, _)| order)
                    .collect()
            })
            .collect();
        if let (Some(&first_cut), Some(&last_cut)) = (cuts.first(), cuts.last()) {
            for (position, (rect, index)) in layouts.iter_mut().enumerate() {
                if *index < first_cut || *index >= last_cut {
                    continue;
                }
                let segment = cuts.partition_point(|&cut| cut <= *index) - 1;
                let orders = &segment_splices[segment];
                if orders.is_empty() {
                    continue;
                }
                let after = if since.is_empty() { 0 } else { after(position) };
                for &order in orders {
                    if order >= after {
                        rect.y += splices[order].1;
                    }
                }
            }
        }
        // Growth in splice order (`growth` was pushed in that order).
        for (position, order, dh) in growth {
            if order >= after(position) {
                layouts[position].0.height += dh;
            }
        }
    }
}

fn splice_layouts_eagerly(
    layouts: &mut Vec<(crate::layout::LayoutRect, usize)>,
    entry: &PaintSplice,
    geometry: &SpliceGeometry,
) {
    let (region_start, region_end) = (geometry.region_start, geometry.region_end);
    let (shifted, grown) = (&geometry.shifted, &geometry.grown);
    let removed_len = region_end - region_start;
    let delta = entry.len as isize - removed_len as isize;
    let renumber = |index: usize| {
        if index >= region_end {
            (index as isize + delta) as usize
        } else {
            index
        }
    };
    let in_shift = |index: usize| shifted.iter().any(|&(a, b)| index >= a && index < b);
    let in_region = |index: usize| index >= region_start && index < region_end;
    let inserted = |index: usize| index >= entry.first && index < entry.first + entry.len;
    // The inserted fragments take the removed ones' place in the output
    // order (before the first fragment after the region when none was
    // removed), as a full layout would emit them. The other fragments are
    // updated in place; only a region scattered through the list (not one
    // run) goes through a rebuilt list.
    let layout_inserted = || {
        entry
            .raw
            .layouts
            .iter()
            .filter(|(_, index)| inserted(*index))
            .map(|&(mut rect, index)| {
                rect.x += entry.dx;
                rect.y += entry.dy;
                (rect, region_start + index - entry.first)
            })
    };
    let update_layout = |(rect, index): &mut (crate::layout::LayoutRect, usize)| {
        if in_shift(*index) {
            rect.y += entry.dh;
        }
        if grown.contains(index) {
            rect.height += entry.dh;
        }
        *index = renumber(*index);
    };
    if !splice_region_in_place(
        layouts,
        (region_start, region_end),
        |(_, index)| *index,
        update_layout,
        layout_inserted(),
    ) {
        let old_layouts = std::mem::take(layouts);
        let mut out = Vec::with_capacity(old_layouts.len() + entry.len);
        let mut placed = false;
        for mut item in old_layouts {
            if in_region(item.1) {
                if !placed {
                    out.extend(layout_inserted());
                    placed = true;
                }
                continue;
            }
            if !placed && item.1 >= region_end {
                out.extend(layout_inserted());
                placed = true;
            }
            update_layout(&mut item);
            out.push(item);
        }
        if !placed {
            out.extend(layout_inserted());
        }
        *layouts = out;
    }
}

fn splice_texts(
    texts: &mut Vec<crate::layout_ng::TextFragment>,
    entry: &PaintSplice,
    geometry: &SpliceGeometry,
) {
    let (region_start, region_end) = (geometry.region_start, geometry.region_end);
    let shifted = &geometry.shifted;
    let removed_len = region_end - region_start;
    let delta = entry.len as isize - removed_len as isize;
    let renumber = |index: usize| {
        if index >= region_end {
            (index as isize + delta) as usize
        } else {
            index
        }
    };
    let in_shift = |index: usize| shifted.iter().any(|&(a, b)| index >= a && index < b);
    let in_region = |index: usize| index >= region_start && index < region_end;
    let inserted = |index: usize| index >= entry.first && index < entry.first + entry.len;
    let text_inserted = || {
        entry
            .raw
            .texts
            .iter()
            .filter(|fragment| inserted(fragment.index))
            .map(|fragment| {
                let mut fragment = fragment.clone();
                fragment.rect.x += entry.dx;
                fragment.rect.y += entry.dy;
                fragment.index = region_start + fragment.index - entry.first;
                fragment
            })
    };
    let update_text = |fragment: &mut crate::layout_ng::TextFragment| {
        if in_shift(fragment.index) {
            fragment.rect.y += entry.dh;
        }
        fragment.index = renumber(fragment.index);
    };
    if !splice_region_in_place(
        texts,
        (region_start, region_end),
        |fragment| fragment.index,
        update_text,
        text_inserted(),
    ) {
        let old_texts = std::mem::take(texts);
        let mut out = Vec::with_capacity(old_texts.len() + entry.raw.texts.len());
        let mut placed = false;
        for mut fragment in old_texts {
            if in_region(fragment.index) {
                if !placed {
                    out.extend(text_inserted());
                    placed = true;
                }
                continue;
            }
            if !placed && fragment.index >= region_end {
                out.extend(text_inserted());
                placed = true;
            }
            update_text(&mut fragment);
            out.push(fragment);
        }
        if !placed {
            out.extend(text_inserted());
        }
        *texts = out;
    }
}

fn splice_tree(
    root: &mut Component,
    entry: &PaintSplice,
    geometry: &SpliceGeometry,
    sizes: &mut Option<Vec<u32>>,
) {
    let path = &entry.path;
    let target = at_path_mut(root, path);
    match entry.children {
        None => *target = entry.inserted[0].clone(),
        Some((keep, removed)) => {
            target
                .children
                .splice(keep..keep + removed, entry.inserted.iter().cloned());
        }
    }
    let Some(known) = sizes.as_mut() else {
        return;
    };
    let mut fresh = Vec::new();
    for component in &entry.inserted {
        push_subtree_sizes(component, &mut fresh);
    }
    let delta = fresh.len() as isize - (geometry.region_end - geometry.region_start) as isize;
    if geometry.region_end > known.len() {
        *sizes = None;
        return;
    }
    known.splice(geometry.region_start..geometry.region_end, fresh);
    // Every box on the path holds the region, except a replaced target.
    let holders = match entry.children {
        None => &geometry.starts[..path.len()],
        Some(_) => &geometry.starts[..],
    };
    for &start in holders {
        known[start] = (known[start] as isize + delta) as u32;
    }
}

/// Replace the items whose index is in `region` with `inserted` and run
/// `update` on every other item, in place. The inserted items go where the
/// region's first item was, or before the first item past the region when
/// the region has none, which is where a rebuilt list would put them.
/// Returns false, touching nothing, when the region's items are not one run
/// at that place; the caller then rebuilds the list.
fn splice_region_in_place<T>(
    items: &mut Vec<T>,
    (start, end): (usize, usize),
    index_of: impl Fn(&T) -> usize,
    mut update: impl FnMut(&mut T),
    inserted: impl Iterator<Item = T>,
) -> bool {
    let mut at = None;
    let mut first_region = None;
    let mut last_region = 0;
    let mut region_items = 0;
    for (position, item) in items.iter().enumerate() {
        let index = index_of(item);
        let in_region = index >= start && index < end;
        if at.is_none() && (in_region || index >= end) {
            at = Some(position);
        }
        if in_region {
            first_region.get_or_insert(position);
            last_region = position;
            region_items += 1;
        }
    }
    let at = at.unwrap_or(items.len());
    if region_items > 0 && (first_region != Some(at) || last_region + 1 - at != region_items) {
        return false;
    }
    for (position, item) in items.iter_mut().enumerate() {
        if position < at || position >= at + region_items {
            update(item);
        }
    }
    items.splice(at..at + region_items, inserted);
    true
}

/// [`retain_lowering_verify`]: the retained component tree must equal the
/// one a full rebuild lowers.
fn verify_lowering(cache: &Cache, node: u32) {
    let fresh = format!("{:?}", crate::dom::to_component_tree());
    let kept = format!("{:?}", &*cache.component);
    if fresh == kept {
        eprintln!("[retain-verify] root {node}: lowering matches");
        return;
    }
    let at = fresh
        .bytes()
        .zip(kept.bytes())
        .position(|(a, b)| a != b)
        .unwrap_or(fresh.len().min(kept.len()));
    let start = at.saturating_sub(120);
    let excerpt = |text: &str| {
        text.get(start..(at + 120).min(text.len()))
            .unwrap_or("")
            .to_string()
    };
    eprintln!(
        "[retain-verify] root {node}: lowering MISMATCH at byte {at}\n  kept  {}\n  fresh {}",
        excerpt(&kept),
        excerpt(&fresh)
    );
}

/// Whole-tree `needs_text_metric`, without walking nodes the cache already
/// classified. A definite-height or out-of-flow ancestor makes that predicate
/// ignore the subtree, so a tail inside a scroller must not turn measurement
/// on by itself.
fn text_metric_for_fresh(cache: &Cache, path: &[usize], fresh: &Component) -> bool {
    if cache.measure_text {
        return true;
    }
    for depth in 0..path.len() {
        let style = &at_path(&cache.component, &path[..depth]).style;
        if matches!(style.position, Position::Absolute | Position::Fixed)
            || matches!(style.height, Dimension::Px(_))
        {
            return false;
        }
    }
    needs_text_metric(fresh)
}

fn text_metric_after_edit(cache: &Cache, path: &[usize]) -> bool {
    if cache.measure_text {
        return true;
    }
    for depth in 0..path.len() {
        let style = &at_path(&cache.component, &path[..depth]).style;
        if matches!(style.position, Position::Absolute | Position::Fixed)
            || matches!(style.height, Dimension::Px(_))
        {
            return false;
        }
    }
    needs_text_metric(at_path(&cache.component, path))
}

fn content_box_width(style: &Style, border_box: f32) -> f32 {
    let padding = style.padding_lengths();
    let border = border_side(style, 1) + border_side(style, 3);
    (border_box - padding.left - padding.right - border).max(0.0)
}

fn margins_are_lengths(style: &Style) -> bool {
    px_spacing(style.margin.left).is_some()
        && px_spacing(style.margin.right).is_some()
        && px_spacing(style.margin.top).is_some()
        && px_spacing(style.margin.bottom).is_some()
}

fn origin_stable(old: &Style, new: &Style) -> bool {
    old.margin == new.margin
        && old.position == new.position
        && old.top == new.top
        && old.right == new.right
        && old.bottom == new.bottom
        && old.left == new.left
        && old.display == new.display
        && old.float == new.float
        && old.flex_direction == new.flex_direction
        && old.flex_wrap == new.flex_wrap
        && old.flex_grow == new.flex_grow
        && old.flex_shrink == new.flex_shrink
        && old.align_self == new.align_self
        && old.justify_content == new.justify_content
        && old.align_items == new.align_items
        && margins_are_lengths(new)
}

/// The border-box origin stays put when the cross size changes. Center,
/// flex-end, and baseline alignment move it.
fn cross_axis_start_stable(parent: &Style, child: &Style) -> bool {
    if is_block_flow(parent.display) {
        return in_flow_static(child);
    }
    if parent.display != Display::Flex || parent.flex_direction != FlexDirection::Column {
        return false;
    }
    let align = match child.align_self {
        AlignSelf::Auto => parent.align_items,
        AlignSelf::FlexStart => AlignItems::FlexStart,
        AlignSelf::FlexEnd => AlignItems::FlexEnd,
        AlignSelf::Center => AlignItems::Center,
        AlignSelf::Baseline => AlignItems::Baseline,
        AlignSelf::Stretch => AlignItems::Stretch,
    };
    matches!(align, AlignItems::Stretch | AlignItems::FlexStart)
}

fn follower_shift(component: &Component, path: &[usize], dh: f32) -> Option<(Vec<u32>, Vec<u32>)> {
    let levels = ancestor_levels(component, path)?;
    let mut shifted = Vec::new();
    let mut grown = Vec::new();
    if dh != 0.0 {
        for level in 0..levels {
            let depth = path.len() - 1 - level;
            let parent = at_path(component, &path[..depth]);
            for sibling in &parent.children[path[depth] + 1..] {
                collect_hosts(sibling, &mut shifted);
            }
            if let Some(id) = host_id(parent)
                && !matches!(parent.style.height, Dimension::Px(_))
            {
                grown.push(id);
            }
        }
    }
    shifted.sort_unstable();
    shifted.dedup();
    Some((shifted, grown))
}

/// Whether `try_style_subtree` gets past its first precondition.
pub(super) fn style_subtree_may_apply(
    cache: &Cache,
    old: &Style,
    new: &Style,
    viewport_w: f32,
    viewport_h: f32,
) -> bool {
    subtree_relayout_enabled()
        && cache.splice_ok
        // A subtree solve on a tree the legacy engine laid out as a whole
        // would mix engines (see `try_splice`).
        && !cache.engine.ng_fell_back()
        && cache.skip_projections
        && cache.viewport_w_bits == viewport_w.to_bits()
        && cache.viewport_h_bits == viewport_h.to_bits()
        && origin_stable(old, new)
}

/// A geometry edit on one element. Lay out that element, or its flex/grid
/// container when siblings share the formatting context, and translate the
/// tail. Returns false when the result would not match a full solve.
pub(super) fn try_style_subtree(
    cache: &mut Cache,
    id: u32,
    old: &Style,
    new: &Style,
    viewport_w: f32,
    viewport_h: f32,
) -> bool {
    if !style_subtree_may_apply(cache, old, new, viewport_w, viewport_h) {
        if debug_enabled() {
            eprintln!(
                "[splice] style precondition: enabled {} splice_ok {} skip_projections {} viewport {} origin_stable {} margins_are_lengths {}",
                subtree_relayout_enabled(),
                cache.splice_ok,
                cache.skip_projections,
                cache.viewport_w_bits == viewport_w.to_bits()
                    && cache.viewport_h_bits == viewport_h.to_bits(),
                origin_stable(old, new),
                margins_are_lengths(new)
            );
        }
        return bail("style precondition");
    }
    let Some(root_id) = host_id(&cache.component) else {
        return bail("style root");
    };
    let Some(chain) = crate::dom::with_document(|document| {
        element_chain(document, NodeId::from_u32(id), root_id)
    }) else {
        return bail("style chain");
    };
    let Some(path) = component_path(&cache.component, &chain) else {
        return bail("style path");
    };
    if path.is_empty() || host_id(at_path(&cache.component, &path)) != Some(id) {
        return bail("style host");
    }
    if edge_collapse_changes(at_path(&cache.component, &path), old) {
        return bail("style margin collapse edge");
    }
    // A column-flex item whose width just became definite (or auto again)
    // is re-lowered below. The cached text still has the previous
    // `text-line-width` mark, and the paragraph line count would not match
    // a full lower of the same border box.
    let column_flex = path_has_column_flex(&cache.component, &path);
    let parent = at_path(&cache.component, &path[..path.len() - 1]);
    let node_alone = stacking_parent(parent)
        && cross_axis_start_stable(&parent.style, new)
        && crate::layout::count_nodes(at_path(&cache.component, &path)) <= MAX_SOLVE_NODES
        && ancestor_levels(&cache.component, &path).is_some();
    let solve_path: Vec<usize> = if node_alone {
        path.clone()
    } else if is_flex_or_grid(&parent.style)
        && width_is_definite(&parent.style)
        && crate::layout::count_nodes(parent) <= MAX_SOLVE_NODES
        && ancestor_levels(&cache.component, &path[..path.len() - 1]).is_some()
    {
        path[..path.len() - 1].to_vec()
    } else {
        return bail("style scope");
    };
    if solve_path.is_empty() {
        return bail("style scope root");
    }
    let target = at_path(&cache.component, &solve_path);
    let Some(host) = host_id(target) else {
        return bail("style scope host");
    };
    let old_rect =
        crate::dom::with_document(|document| document.get_layout_rect(NodeId::from_u32(host)));
    if !(old_rect.height > 0.0) || !(old_rect.width > 0.0) {
        return bail("style old box");
    }
    let parent_of_target = at_path(&cache.component, &solve_path[..solve_path.len() - 1]);
    let wrapper_width = if node_alone && solve_path == path {
        let Some(parent_id) = host_id(parent_of_target) else {
            return bail("style parent host");
        };
        let parent_rect = crate::dom::with_document(|document| {
            document.get_layout_rect(NodeId::from_u32(parent_id))
        });
        content_box_width(&parent_of_target.style, parent_rect.width)
    } else {
        let margins = target.style.margin_lengths();
        old_rect.width + margins.left + margins.right
    };
    if !wrapper_width.is_finite() || wrapper_width <= 0.0 {
        return bail("style width");
    }
    let fresh = if column_flex {
        let lowered = crate::dom::with_document(|document| {
            document.to_component_subtree(NodeId::from_u32(host))
        });
        if host_id(&lowered) != Some(host) {
            return bail("style relower");
        }
        lowered
    } else {
        target.clone()
    };
    let relowered = column_flex.then(|| fresh.clone());
    let solved_nodes = crate::layout::count_nodes(&fresh) as u32;
    let wrapper = wrapper_for(parent_of_target, wrapper_width, vec![fresh]);
    let Some((rects, _)) = solve(
        &wrapper,
        true,
        text_metric_after_edit(cache, &solve_path),
        viewport_w,
        viewport_h,
    ) else {
        return bail("style solve");
    };
    let Some(new_rect) = rect_of(&rects, host) else {
        return bail("style solved host");
    };
    if !(new_rect.height > 0.0) || !new_rect.width.is_finite() {
        return bail("style solved box");
    }
    let container_solve = solve_path != path;
    if container_solve && (new_rect.width - old_rect.width).abs() > 0.05 {
        return bail("style container width");
    }
    let dx = old_rect.x - new_rect.x;
    let dy = old_rect.y - new_rect.y;
    let dh = new_rect.height - old_rect.height;
    let Some((shifted, grown)) = follower_shift(&cache.component, &solve_path, dh) else {
        return bail("style ancestors");
    };
    let shifted_nodes = shifted.len() as u32;
    crate::dom::with_document_mut(|document| {
        for (node_id, rect) in &rects {
            document.set_layout_rect(
                NodeId::from_u32(*node_id),
                w3cos_dom::DOMRect::new(rect.x + dx, rect.y + dy, rect.width, rect.height),
            );
        }
        for id in shifted {
            let id = NodeId::from_u32(id);
            let mut rect = document.get_layout_rect(id);
            rect.y += dh;
            document.set_layout_rect(id, rect);
        }
        for id in grown {
            let id = NodeId::from_u32(id);
            let mut rect = document.get_layout_rect(id);
            rect.height += dh;
            document.set_layout_rect(id, rect);
        }
    });
    if let Some(relowered) = relowered {
        // The text run's line-width mark lives on the children. The element's
        // own style stays the computed style already assigned, so a later
        // background edit is still paint-only.
        crate::layout_ng::forget_patch_memo();
        at_path_mut(&mut cache.component, &solve_path).children = relowered.children;
    }
    cache.engine.invalidate();
    cache.engine_pending.clear();
    cache.width_memos.clear();
    super::note_flush("style-subtree", solved_nodes, shifted_nodes);
    true
}

/// Debug aid: compare every spliced rect with a from-scratch layout.
pub(super) fn verify(viewport_w: f32, viewport_h: f32, node: u32) {
    let component = crate::dom::to_component_tree();
    let flat = pre_flatten(&component);
    let mut engine = LayoutEngine::new();
    super::configure_engine(&mut engine, &component);
    let Ok(results) = engine.compute(&component, &flat, viewport_w, viewport_h) else {
        eprintln!("[splice-verify] full layout failed");
        return;
    };
    let mut expected = std::collections::HashMap::new();
    for (rect, index) in &results.layout_cache {
        if let Some(info) = flat.get(*index) {
            let id = flat_host_id(info);
            if id != u32::MAX {
                expected.insert(id, *rect);
            }
        }
    }
    let mut bad = 0;
    crate::dom::with_document(|document| {
        for (&id, rect) in &expected {
            let got = document.get_layout_rect(NodeId::from_u32(id));
            if (got.x - rect.x).abs() > 0.05
                || (got.y - rect.y).abs() > 0.05
                || (got.width - rect.width).abs() > 0.05
                || (got.height - rect.height).abs() > 0.05
            {
                if bad < 5 {
                    eprintln!(
                        "[splice-verify] root {node}: node {id} got ({:.2},{:.2},{:.2},{:.2}) want ({:.2},{:.2},{:.2},{:.2})",
                        got.x,
                        got.y,
                        got.width,
                        got.height,
                        rect.x,
                        rect.y,
                        rect.width,
                        rect.height
                    );
                }
                bad += 1;
            }
        }
    });
    eprintln!(
        "[splice-verify] root {node}: {} rects, {bad} mismatched",
        expected.len()
    );
    let expected = super::client_rects_for(&flat, &results.layout_cache, &results.text_fragments);
    let got = super::client_rect_snapshot();
    let mut bad_owners = 0;
    let close = |a: &[w3cos_dom::DOMRect], b: &[w3cos_dom::DOMRect]| {
        a.len() == b.len()
            && a.iter().zip(b).all(|(a, b)| {
                (a.x - b.x).abs() <= 0.05
                    && (a.y - b.y).abs() <= 0.05
                    && (a.width - b.width).abs() <= 0.05
                    && (a.height - b.height).abs() <= 0.05
            })
    };
    for (owner, rects) in &expected {
        if !got.get(owner).is_some_and(|list| close(list, rects)) {
            if bad_owners < 5 {
                eprintln!(
                    "[splice-verify] owner {owner}: got {:?} want {rects:?}",
                    got.get(owner)
                );
            }
            bad_owners += 1;
        }
    }
    let extra = got
        .keys()
        .filter(|owner| !expected.contains_key(owner))
        .count();
    eprintln!(
        "[splice-verify] root {node}: {} owners, {bad_owners} mismatched, {extra} extra",
        expected.len()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two splices folded as ranges mark the same boxes as the per-box
    /// remap they replace: the first's ranges move with the second's region.
    #[test]
    fn paint_changes_ranges_follow_later_splices() {
        let geometry =
            |total, region_start, region_end, starts: Vec<usize>, shifted, grown| SpliceGeometry {
                total,
                region_start,
                region_end,
                starts,
                shifted,
                grown,
            };
        let mut changes = PaintChanges::default();
        // 20 boxes: box 5's subtree 5..8 becomes 5 boxes, 8..20 move.
        changes
            .add(
                &geometry(20, 5, 8, vec![0, 4], vec![(8, 20)], vec![0, 4]),
                5,
            )
            .unwrap();
        assert_eq!(changes.len, 22);
        assert_eq!(changes.changed, vec![(0, 1), (4, 10)]);
        assert_eq!(changes.moved, vec![(10, 22)]);
        // Then 15..17 (moved boxes) is removed: later moved boxes shift up.
        changes
            .add(
                &geometry(22, 15, 17, vec![0, 14], vec![(17, 22)], vec![]),
                0,
            )
            .unwrap();
        assert_eq!(changes.len, 20);
        assert_eq!(changes.changed, vec![(0, 1), (4, 10), (14, 15)]);
        assert_eq!(changes.moved, vec![(10, 20)]);
        let touched = changes.touched_bitmap();
        assert_eq!(touched.iter().filter(|t| **t).count(), 1 + 6 + 10);
        assert!(!touched[1] && !touched[3] && touched[19]);
        assert_eq!(ranges_gap_end(&changes.changed, 1, 20), 4);
        assert_eq!(ranges_gap_end(&changes.changed, 4, 20), 4);
        assert_eq!(ranges_gap_end(&changes.changed, 15, 20), 20);
        assert!(ranges_contain(&changes.changed, 9) && !ranges_contain(&changes.changed, 10));
        // A size that does not add up drops the set.
        assert!(
            changes
                .add(&geometry(21, 0, 1, vec![], vec![], vec![]), 1)
                .is_none()
        );
    }

    #[test]
    fn region_splice_in_place_matches_a_rebuilt_list() {
        // (index) items; region 3..5 becomes two items 3, 4, 5; the rest
        // shift by +1.
        let update = |item: &mut usize| {
            if *item >= 5 {
                *item += 1;
            }
        };
        let mut items = vec![0, 1, 2, 3, 4, 5, 6];
        assert!(splice_region_in_place(
            &mut items,
            (3, 5),
            |i| *i,
            update,
            [3, 4, 5].into_iter()
        ));
        assert_eq!(items, [0, 1, 2, 3, 4, 5, 6, 7]);
        // No item in the region: insert before the first item past it.
        let mut items = vec![0, 1, 6, 2, 7];
        assert!(splice_region_in_place(
            &mut items,
            (3, 5),
            |i| *i,
            update,
            [3].into_iter()
        ));
        assert_eq!(items, [0, 1, 3, 7, 2, 8]);
        // A region split by another item is left for the caller.
        let mut items = vec![0, 3, 1, 4, 5];
        assert!(!splice_region_in_place(
            &mut items,
            (3, 5),
            |i| *i,
            update,
            [3].into_iter()
        ));
        assert_eq!(items, [0, 3, 1, 4, 5]);
        // An item past the region before it: also left for the caller.
        let mut items = vec![0, 6, 3, 4];
        assert!(!splice_region_in_place(
            &mut items,
            (3, 5),
            |i| *i,
            update,
            [3].into_iter()
        ));
    }

    /// Column flex uses the same subtree kind in every mode. Geometry stays
    /// exact; a full-document rebuild is not the supported path.
    fn expect_column_flex_edit(label: &str, fast_kind: &str, fast_node_limit: Option<u32>) {
        let stats = crate::incremental_layout::last_flush_stats();
        assert_eq!(stats.kind, fast_kind, "{label} {}", stats.kind);
        if let Some(limit) = fast_node_limit {
            assert!(
                stats.solved_nodes < limit,
                "{label} solved {} nodes",
                stats.solved_nodes
            );
        }
    }

    fn style(node: u32, decls: &[(&str, &str)]) {
        for (name, value) in decls {
            crate::dom::set_style_property(node, name, value);
        }
    }

    fn full_rects() -> std::collections::HashMap<u32, crate::layout::LayoutRect> {
        let component = crate::dom::to_component_tree();
        let flat = pre_flatten(&component);
        let mut engine = LayoutEngine::new();
        super::super::configure_engine(&mut engine, &component);
        let results = engine
            .compute(&component, &flat, 1024.0, 768.0)
            .expect("layout");
        let mut out = std::collections::HashMap::new();
        for (rect, index) in &results.layout_cache {
            let id = flat_host_id(&flat[*index]);
            if id != u32::MAX {
                out.insert(id, *rect);
            }
        }
        out
    }

    fn assert_matches_full(label: &str) {
        let expected = full_rects();
        crate::dom::with_document(|document| {
            for (&id, rect) in &expected {
                let got = document.get_layout_rect(NodeId::from_u32(id));
                assert!(
                    (got.x - rect.x).abs() < 0.05
                        && (got.y - rect.y).abs() < 0.05
                        && (got.width - rect.width).abs() < 0.05
                        && (got.height - rect.height).abs() < 0.05,
                    "{label}: node {id} spliced {got:?} vs full {rect:?}"
                );
            }
        });
    }

    fn chat_page() -> (u32, u32) {
        crate::dom::reset_document();
        crate::incremental_layout::invalidate();
        let chat = crate::dom::create_element("div");
        style(
            chat,
            &[
                ("width", "760px"),
                ("height", "700px"),
                ("overflow", "auto"),
                ("font-size", "14px"),
                ("line-height", "20px"),
            ],
        );
        crate::dom::append_child(crate::dom::body_id(), chat);
        for index in 0..3 {
            let msg = crate::dom::create_element("div");
            style(msg, &[("padding", "8px 12px"), ("margin", "0 0 8px 0")]);
            let p = crate::dom::create_element("p");
            style(p, &[("margin", "4px 0")]);
            let text =
                crate::dom::create_text_node(&format!("history message {index} ").repeat(12));
            crate::dom::append_child(p, text);
            crate::dom::append_child(msg, p);
            crate::dom::append_child(chat, msg);
        }
        let reply = crate::dom::create_element("div");
        style(reply, &[("padding", "8px 12px"), ("margin", "0 0 8px 0")]);
        crate::dom::append_child(chat, reply);
        assert!(crate::incremental_layout::flush());
        (chat, reply)
    }

    fn assert_client_rects_match_full(label: &str) {
        let component = crate::dom::to_component_tree();
        let flat = pre_flatten(&component);
        let mut engine = LayoutEngine::new();
        super::super::configure_engine(&mut engine, &component);
        let results = engine
            .compute(&component, &flat, 1024.0, 768.0)
            .expect("layout");
        let expected =
            super::super::client_rects_for(&flat, &results.layout_cache, &results.text_fragments);
        let got = super::super::client_rect_snapshot();
        assert!(
            !expected.is_empty(),
            "{label}: page publishes no fragment owners"
        );
        assert_eq!(
            got.keys().collect::<std::collections::BTreeSet<_>>(),
            expected.keys().collect::<std::collections::BTreeSet<_>>(),
            "{label}: owner set"
        );
        for (owner, rects) in &expected {
            let list = &got[owner];
            assert_eq!(list.len(), rects.len(), "{label}: owner {owner} rect count");
            for (a, b) in list.iter().zip(rects) {
                assert!(
                    (a.x - b.x).abs() < 0.05
                        && (a.y - b.y).abs() < 0.05
                        && (a.width - b.width).abs() < 0.05
                        && (a.height - b.height).abs() < 0.05,
                    "{label}: owner {owner} spliced {a:?} vs full {b:?}"
                );
            }
        }
    }

    /// Markdown chat shape: a column flexbox with gaps of row messages
    /// (fixed avatar + column bubble) whose paragraphs hold strong, code and
    /// link runs, which publish CSSOM fragment rects per owner.
    fn markdown_chat_page(avatar_px: u32) -> Vec<(u32, u32)> {
        crate::dom::reset_document();
        crate::incremental_layout::invalidate();
        let chat = crate::dom::create_element("div");
        style(
            chat,
            &[
                ("display", "flex"),
                ("flex-direction", "column"),
                ("gap", "12px"),
                ("padding", "16px"),
                ("font-size", "14px"),
                ("line-height", "20px"),
            ],
        );
        crate::dom::append_child(crate::dom::body_id(), chat);
        let mut streams = Vec::new();
        for index in 0..3 {
            let msg = crate::dom::create_element("div");
            style(
                msg,
                &[
                    ("display", "flex"),
                    ("flex-direction", "row"),
                    ("gap", "8px"),
                ],
            );
            let avatar = crate::dom::create_element("div");
            let avatar_height = format!("{avatar_px}px");
            style(
                avatar,
                &[
                    ("width", "32px"),
                    ("height", avatar_height.as_str()),
                    ("flex", "none"),
                ],
            );
            let bubble = crate::dom::create_element("div");
            style(
                bubble,
                &[
                    ("flex", "1 1 0"),
                    ("min-width", "0"),
                    ("display", "flex"),
                    ("flex-direction", "column"),
                    ("padding", "12px"),
                ],
            );
            let p = crate::dom::create_element("p");
            style(p, &[("margin", "0 0 8px 0")]);
            let lead = crate::dom::create_text_node(&format!("message {index} lead words "));
            crate::dom::append_child(p, lead);
            let strong = crate::dom::create_element("strong");
            style(strong, &[("font-weight", "700")]);
            crate::dom::append_child(strong, crate::dom::create_text_node("bold run of words"));
            crate::dom::append_child(p, strong);
            crate::dom::append_child(p, crate::dom::create_text_node(" then "));
            let code = crate::dom::create_element("code");
            style(
                code,
                &[("font-family", "monospace"), ("background", "#f0f0f0")],
            );
            crate::dom::append_child(code, crate::dom::create_text_node("fn_call()"));
            crate::dom::append_child(p, code);
            crate::dom::append_child(p, crate::dom::create_text_node(" and "));
            // A passive colored inline coalesces into the paragraph's text
            // run and keeps CSSOM owner ranges for its bytes.
            let link = crate::dom::create_element("a");
            style(link, &[("color", "#0645ad")]);
            crate::dom::append_child(link, crate::dom::create_text_node("linked words here"));
            crate::dom::append_child(p, link);
            let tail = crate::dom::create_text_node(" tail");
            crate::dom::append_child(p, tail);
            crate::dom::append_child(bubble, p);
            let list = crate::dom::create_element("ul");
            style(list, &[("margin", "0 0 8px 0"), ("padding-left", "24px")]);
            for item in 0..2 {
                let li = crate::dom::create_element("li");
                style(li, &[("display", "list-item")]);
                crate::dom::append_child(
                    li,
                    crate::dom::create_text_node(&format!("list item {item} words")),
                );
                crate::dom::append_child(list, li);
            }
            crate::dom::append_child(bubble, list);
            crate::dom::append_child(msg, avatar);
            crate::dom::append_child(msg, bubble);
            crate::dom::append_child(chat, msg);
            streams.push((p, tail));
        }
        assert!(crate::incremental_layout::flush());
        streams
    }

    #[test]
    fn split_inline_owner_rects_follow_streamed_splice() {
        let streams = markdown_chat_page(32);
        assert_matches_full("initial");
        assert_client_rects_match_full("initial");
        // Stream into the first message: the later messages and their
        // fragment owners shift by the paragraph's height change.
        let (_p, tail) = streams[0];
        let before = splice_counts();
        for step in 0..12 {
            crate::dom::set_text_content(tail, &" streamed tokens".repeat(step * 3 + 1));
            assert!(crate::incremental_layout::flush());
            assert_matches_full(&format!("step {step}"));
            assert_client_rects_match_full(&format!("step {step}"));
        }
        assert_eq!(
            splice_counts().0 - before.0,
            12,
            "text edits under row messages must splice"
        );
    }

    #[test]
    fn new_paragraph_in_column_flex_bubble_splices_tail() {
        let streams = markdown_chat_page(32);
        let bubbles: Vec<u32> = streams
            .iter()
            .map(|&(p, _)| {
                crate::dom::with_document(|document| {
                    document
                        .get_node(NodeId::from_u32(p))
                        .parent
                        .unwrap()
                        .as_u32()
                })
            })
            .collect();
        let before = splice_counts();
        // Append into the middle message (followers shift) and the last one.
        for (step, &bubble) in [bubbles[1], bubbles[2], bubbles[1]].iter().enumerate() {
            let p = crate::dom::create_element("p");
            style(p, &[("margin", "6px 0 10px 0")]);
            crate::dom::append_child(p, crate::dom::create_text_node("new paragraph words "));
            let code = crate::dom::create_element("code");
            style(
                code,
                &[("font-family", "monospace"), ("background", "#f0f0f0")],
            );
            crate::dom::append_child(code, crate::dom::create_text_node("tail_call()"));
            crate::dom::append_child(p, code);
            crate::dom::append_child(bubble, p);
            assert!(crate::incremental_layout::flush());
            assert_matches_full(&format!("append {step}"));
            assert_client_rects_match_full(&format!("append {step}"));
            let text = crate::dom::create_text_node(&" streamed".repeat(step * 4 + 3));
            crate::dom::append_child(p, text);
            assert!(crate::incremental_layout::flush());
            assert_matches_full(&format!("stream {step}"));
            assert_client_rects_match_full(&format!("stream {step}"));
        }
        if crate::layout_ng::mode() != crate::layout_ng::Mode::Off {
            let after = splice_counts();
            assert_eq!(
                after.1 - before.1,
                3,
                "appends to a column flex bubble splice the tail"
            );
            assert_eq!(
                after.0 - before.0,
                3,
                "text edits in the new paragraphs splice"
            );
        }
    }

    #[test]
    fn taller_row_sibling_falls_back_to_rebuild() {
        // The avatar is taller than the bubble, so the row's height does not
        // follow the paragraph; the splice must refuse and the result match.
        let streams = markdown_chat_page(400);
        let (_p, tail) = streams[1];
        for step in 0..4 {
            crate::dom::set_text_content(tail, &" more".repeat(step + 1));
            assert!(crate::incremental_layout::flush());
            assert_matches_full(&format!("step {step}"));
            assert_client_rects_match_full(&format!("step {step}"));
        }
    }

    #[test]
    fn streamed_paragraph_text_matches_full_layout() {
        let (_chat, reply) = chat_page();
        let p = crate::dom::create_element("p");
        style(p, &[("margin", "4px 0")]);
        let text = crate::dom::create_text_node("start");
        crate::dom::append_child(p, text);
        crate::dom::append_child(reply, p);
        assert!(crate::incremental_layout::flush());
        assert_matches_full("tail block");
        let before = splice_counts();
        for step in 0..40 {
            crate::dom::set_text_content(text, &"streamed words ".repeat(step + 1));
            assert!(crate::incremental_layout::flush());
            assert_matches_full(&format!("step {step}"));
        }
        assert_eq!(splice_counts().0 - before.0, 40, "text edits must splice");
    }

    #[test]
    fn replaced_tail_blocks_match_full_layout() {
        let (chat, reply) = chat_page();
        let mut tail: Option<u32> = None;
        let before = splice_counts();
        for step in 0..30 {
            if let Some(old) = tail.take() {
                crate::dom::remove_child(reply, old);
            }
            // A finished line stays; the open block is replaced each chunk.
            if step % 4 == 0 {
                let done = crate::dom::create_element("div");
                let p = crate::dom::create_element("p");
                style(p, &[("margin", "6px 0")]);
                crate::dom::append_child(
                    p,
                    crate::dom::create_text_node(&"done line ".repeat(step + 1)),
                );
                crate::dom::append_child(done, p);
                crate::dom::append_child(reply, done);
            }
            let open = crate::dom::create_element("div");
            let p = crate::dom::create_element("p");
            style(p, &[("margin", "4px 0")]);
            crate::dom::append_child(
                p,
                crate::dom::create_text_node(&"open ".repeat(step * 3 + 1)),
            );
            crate::dom::append_child(open, p);
            crate::dom::append_child(reply, open);
            tail = Some(open);
            assert!(crate::incremental_layout::flush());
            assert_matches_full(&format!("tail step {step}"));
        }
        assert!(
            splice_counts().1 - before.1 >= 28,
            "tail edits must splice: {:?}",
            splice_counts()
        );
        // A sibling after the reply moves with it.
        let after = crate::dom::create_element("div");
        style(after, &[("height", "10px")]);
        crate::dom::append_child(chat, after);
        assert!(crate::incremental_layout::flush());
        let p = crate::dom::create_element("p");
        crate::dom::append_child(p, crate::dom::create_text_node("late"));
        crate::dom::append_child(reply, p);
        assert!(crate::incremental_layout::flush());
        assert_matches_full("sibling after reply");
    }

    #[test]
    fn nested_anchor_margin_strut_matches_full_layout() {
        let (_chat, reply) = chat_page();
        // anchor: div(mb 3) > div(mb 2) > p(mb 10): end strut is 10px.
        let anchor = crate::dom::create_element("div");
        style(anchor, &[("margin", "0 0 3px 0")]);
        let inner = crate::dom::create_element("div");
        style(inner, &[("margin", "0 0 2px 0")]);
        let p = crate::dom::create_element("p");
        style(p, &[("margin", "1px 0 10px 0")]);
        crate::dom::append_child(p, crate::dom::create_text_node("anchor text"));
        crate::dom::append_child(inner, p);
        crate::dom::append_child(anchor, inner);
        crate::dom::append_child(reply, anchor);
        assert!(crate::incremental_layout::flush());
        let component = crate::dom::with_document(|document| {
            document.to_component_subtree(NodeId::from_u32(anchor))
        });
        assert_eq!(end_margin_strut(&component), Some(10.0));
        let mut tail: Option<u32> = None;
        for step in 0..6 {
            if let Some(old) = tail.take() {
                crate::dom::remove_child(reply, old);
            }
            let open = crate::dom::create_element("div");
            style(
                open,
                &[("margin", format!("{}px 0 0 0", step * 3).as_str())],
            );
            crate::dom::append_child(
                open,
                crate::dom::create_text_node(&"open ".repeat(step + 1)),
            );
            crate::dom::append_child(reply, open);
            tail = Some(open);
            assert!(crate::incremental_layout::flush());
            assert_matches_full(&format!("strut step {step}"));
        }
        // A negative margin anywhere in the strut keeps the full subtree.
        style(p, &[("margin", "1px 0 -4px 0")]);
        assert!(crate::incremental_layout::flush());
        let component = crate::dom::with_document(|document| {
            document.to_component_subtree(NodeId::from_u32(anchor))
        });
        assert_eq!(end_margin_strut(&component), None);
    }

    #[test]
    fn text_only_block_child_still_collapses_through_the_parent_edge() {
        let (_chat, reply) = chat_page();
        let holder = crate::dom::create_element("div");
        let p = crate::dom::create_element("p");
        style(p, &[("margin", "6px 0")]);
        crate::dom::append_child(p, crate::dom::create_text_node("only text"));
        crate::dom::append_child(holder, p);
        crate::dom::append_child(reply, holder);
        let inline = crate::dom::create_element("div");
        crate::dom::append_child(inline, crate::dom::create_text_node("inline text"));
        crate::dom::append_child(reply, inline);
        let (holder_c, inline_c) = crate::dom::with_document(|document| {
            (
                document.to_component_subtree(NodeId::from_u32(holder)),
                document.to_component_subtree(NodeId::from_u32(inline)),
            )
        });
        // <p> with only text is lowered to a Text component carrying the
        // block style; its margin still collapses through the holder.
        assert!(!edge_blocks_margin_collapse(&holder_c, false));
        assert!(!edge_blocks_margin_collapse(&holder_c, true));
        // Anonymous inline text has no margin to collapse.
        assert!(edge_blocks_margin_collapse(&inline_c, false));
    }

    #[test]
    fn message_appended_to_definite_scroller_splices() {
        let (chat, _reply) = chat_page();
        let before = splice_counts();
        for index in 0..5 {
            let msg = crate::dom::create_element("div");
            style(msg, &[("padding", "8px 12px"), ("margin", "0 0 8px 0")]);
            let p = crate::dom::create_element("p");
            style(p, &[("margin", "4px 0")]);
            crate::dom::append_child(
                p,
                crate::dom::create_text_node(&"new message ".repeat(index + 1)),
            );
            crate::dom::append_child(msg, p);
            crate::dom::append_child(chat, msg);
            assert!(crate::incremental_layout::flush());
            assert_matches_full(&format!("message {index}"));
        }
        assert_eq!(
            splice_counts().1 - before.1,
            5,
            "appends under a px-height scroller splice"
        );
    }

    fn flex_column() -> (u32, Vec<u32>) {
        crate::dom::reset_document();
        crate::incremental_layout::invalidate();
        crate::jsdom::set_viewport(1024.0, 768.0);
        let column = crate::dom::create_element("div");
        style(
            column,
            &[
                ("display", "flex"),
                ("flex-direction", "column"),
                ("width", "420px"),
            ],
        );
        crate::dom::append_child(crate::dom::body_id(), column);
        let mut items = Vec::new();
        for index in 0..6 {
            let item = crate::dom::create_element("div");
            style(item, &[("margin", "4px"), ("padding", "4px")]);
            crate::dom::append_child(
                item,
                crate::dom::create_text_node(&format!(
                    "item {index} alpha beta gamma delta epsilon zeta"
                )),
            );
            crate::dom::append_child(column, item);
            items.push(item);
        }
        assert!(crate::incremental_layout::flush());
        assert_matches_full("flex initial");
        (column, items)
    }

    #[test]
    fn flex_column_mutations_match_full_layout() {
        let (column, items) = flex_column();
        crate::dom::set_style_property(items[1], "width", "36px");
        assert!(crate::incremental_layout::flush());
        expect_column_flex_edit("width edit", "style-subtree", Some(30));
        assert_matches_full("flex width");

        let text = crate::dom::with_document(|document| {
            document
                .get_node(w3cos_dom::node::NodeId::from_u32(items[2]))
                .first_child
                .expect("text")
                .as_u32()
        });
        crate::dom::set_text_content(
            text,
            "one two three four five six seven eight nine ten eleven twelve thirteen fourteen",
        );
        assert!(crate::incremental_layout::flush());
        expect_column_flex_edit("text edit", "splice", None);
        assert_matches_full("flex text");

        let extra = crate::dom::create_element("div");
        style(extra, &[("margin", "4px"), ("padding", "4px")]);
        crate::dom::append_child(extra, crate::dom::create_text_node("appended row"));
        crate::dom::append_child(column, extra);
        assert!(crate::incremental_layout::flush());
        expect_column_flex_edit("append", "splice", None);
        assert_matches_full("flex append");

        crate::dom::remove_child(column, extra);
        assert!(crate::incremental_layout::flush());
        expect_column_flex_edit("remove", "splice", None);
        assert_matches_full("flex remove");
    }

    /// Appending or removing the last column item must not re-solve an
    /// untouched text cell. A text edit and a middle insert still match a
    /// full layout, and they do not take that reuse.
    #[test]
    fn column_tail_reuses_an_untouched_text_cell() {
        let (column, items) = flex_column();
        let text = crate::dom::with_document(|document| {
            document
                .get_node(w3cos_dom::node::NodeId::from_u32(items[0]))
                .first_child
                .expect("text")
                .as_u32()
        });
        crate::dom::set_text_content(text, &"alpha beta gamma delta epsilon ".repeat(40));
        assert!(crate::incremental_layout::flush());
        assert_matches_full("long text");
        let before = column_tail_reuses();
        // Retained lowering keeps the untouched items as a child-list splice.
        let kept = || retain_lowering_enabled().then(child_splices);
        let kept_before = kept();
        let extra = crate::dom::create_element("div");
        style(extra, &[("margin", "4px"), ("padding", "4px")]);
        crate::dom::append_child(extra, crate::dom::create_text_node("tail item"));
        crate::dom::append_child(column, extra);
        assert!(crate::incremental_layout::flush());
        assert_matches_full("append after long text");
        if crate::layout_ng::mode() == crate::layout_ng::Mode::On {
            assert!(
                column_tail_reuses() > before || kept() > kept_before,
                "append should reuse untouched column items"
            );
        } else {
            assert_eq!(column_tail_reuses(), before);
        }
        let middle = crate::dom::create_element("div");
        style(middle, &[("margin", "4px"), ("padding", "4px")]);
        crate::dom::append_child(middle, crate::dom::create_text_node("middle"));
        let reused = column_tail_reuses();
        crate::dom::insert_before(column, middle, items[2]);
        assert!(crate::incremental_layout::flush());
        assert_matches_full("middle insert");
        assert_eq!(
            column_tail_reuses(),
            reused,
            "a middle insert lays the column out again"
        );
        crate::dom::set_text_content(text, "short again");
        assert!(crate::incremental_layout::flush());
        assert_matches_full("text edit invalidates the cell");
        assert_eq!(column_tail_reuses(), reused);
        let before_remove = column_tail_reuses();
        let kept_before = kept();
        crate::dom::remove_child(column, extra);
        assert!(crate::incremental_layout::flush());
        assert_matches_full("remove tail");
        if crate::layout_ng::mode() == crate::layout_ng::Mode::On {
            assert!(column_tail_reuses() > before_remove || kept() > kept_before);
        } else {
            assert_eq!(column_tail_reuses(), before_remove);
        }
        let grown = crate::dom::create_element("div");
        style(grown, &[("flex-grow", "1"), ("margin", "4px")]);
        crate::dom::append_child(grown, crate::dom::create_text_node("grows"));
        let before_grow = column_tail_reuses();
        crate::dom::append_child(column, grown);
        assert!(crate::incremental_layout::flush());
        assert_matches_full("flex-grow append");
        assert_eq!(
            column_tail_reuses(),
            before_grow,
            "a flex-grow item lays the column out again"
        );
    }

    /// A long flex-item paragraph is broken inside one text item. A later
    /// width or font edit, and a shorter replacement, still match a full
    /// layout: the break widths are not reused from the previous string.
    #[test]
    fn long_paragraph_edits_match_full_layout() {
        let (column, items) = flex_column();
        let text = crate::dom::with_document(|document| {
            document
                .get_node(w3cos_dom::node::NodeId::from_u32(items[0]))
                .first_child
                .expect("text")
                .as_u32()
        });
        crate::dom::set_text_content(
            text,
            &"alpha beta gamma delta epsilon zeta eta theta ".repeat(36),
        );
        assert!(crate::incremental_layout::flush());
        expect_column_flex_edit("long paragraph", "splice", Some(30));
        assert_matches_full("long paragraph");
        let wide = crate::dom::with_document(|document| {
            document
                .get_layout_rect(w3cos_dom::node::NodeId::from_u32(items[0]))
                .height
        });
        crate::dom::set_style_property(column, "width", "180px");
        assert!(crate::incremental_layout::flush());
        assert_matches_full("narrow column");
        let narrow = crate::dom::with_document(|document| {
            document
                .get_layout_rect(w3cos_dom::node::NodeId::from_u32(items[0]))
                .height
        });
        assert!(
            narrow > wide + 1.0,
            "narrowing the column did not rewrap: wide {wide} narrow {narrow}"
        );
        crate::dom::set_style_property(items[0], "font-size", "22px");
        assert!(crate::incremental_layout::flush());
        assert_matches_full("font size");
        crate::dom::set_text_content(text, "short again");
        assert!(crate::incremental_layout::flush());
        assert_matches_full("text replaced");
    }

    /// The runtime bench's sections are `width:100%` columns, not a fixed
    /// 420px. The same edits have to match a full LayoutNG pass there too,
    /// including a background change that must not move the next item.
    #[test]
    fn percent_column_edits_match_full_layout() {
        crate::dom::reset_document();
        crate::incremental_layout::invalidate();
        crate::jsdom::set_viewport(1024.0, 768.0);
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("div", &[("font-size", "14px")]);
        let column = crate::dom::create_element("div");
        style(
            column,
            &[
                ("display", "flex"),
                ("flex-direction", "column"),
                ("width", "100%"),
            ],
        );
        crate::dom::append_child(crate::dom::body_id(), column);
        let mut items = Vec::new();
        for index in 0..4 {
            let item = crate::dom::create_element("div");
            style(item, &[("margin", "2px"), ("padding", "4px")]);
            crate::dom::append_child(
                item,
                crate::dom::create_text_node(&format!(
                    "Item {index} alpha beta gamma delta epsilon zeta"
                )),
            );
            crate::dom::append_child(column, item);
            items.push(item);
        }
        assert!(crate::incremental_layout::flush());
        assert_matches_full("percent initial");

        crate::dom::set_style_property(items[0], "width", "160px");
        assert!(crate::incremental_layout::flush());
        expect_column_flex_edit("percent width", "style-subtree", Some(30));
        assert_matches_full("percent width");

        let text = crate::dom::with_document(|document| {
            document
                .get_node(w3cos_dom::node::NodeId::from_u32(items[1]))
                .first_child
                .expect("text")
                .as_u32()
        });
        crate::dom::set_text_content(
            text,
            "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda",
        );
        assert!(crate::incremental_layout::flush());
        expect_column_flex_edit("percent text", "splice", None);
        assert_matches_full("percent text");

        let sibling_before = crate::dom::with_document(|document| {
            document
                .get_layout_rect(w3cos_dom::node::NodeId::from_u32(items[1]))
                .y
        });
        crate::dom::set_style_property(items[0], "background", "#204060");
        assert!(crate::incremental_layout::flush());
        let stats = crate::incremental_layout::last_flush_stats();
        assert_eq!(stats.kind, "paint", "percent background {}", stats.kind);
        let sibling_after = crate::dom::with_document(|document| {
            document
                .get_layout_rect(w3cos_dom::node::NodeId::from_u32(items[1]))
                .y
        });
        assert_eq!(
            sibling_before, sibling_after,
            "background moved the next item"
        );
        assert_matches_full("percent background");

        let extra = crate::dom::create_element("div");
        style(extra, &[("margin", "2px"), ("padding", "4px")]);
        crate::dom::append_child(extra, crate::dom::create_text_node("Inserted row"));
        crate::dom::append_child(column, extra);
        assert!(crate::incremental_layout::flush());
        expect_column_flex_edit("percent append", "splice", None);
        assert_matches_full("percent append");
        crate::dom::remove_child(column, extra);
        assert!(crate::incremental_layout::flush());
        expect_column_flex_edit("percent remove", "splice", None);
        assert_matches_full("percent remove");
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn grid_text_and_append_match_full_layout() {
        crate::dom::reset_document();
        crate::incremental_layout::invalidate();
        crate::jsdom::set_viewport(1024.0, 768.0);
        let grid = crate::dom::create_element("div");
        style(
            grid,
            &[
                ("display", "grid"),
                ("width", "480px"),
                ("grid-template-columns", "1fr 1fr 1fr 1fr"),
            ],
        );
        crate::dom::append_child(crate::dom::body_id(), grid);
        let mut cells = Vec::new();
        for index in 0..8 {
            let cell = crate::dom::create_element("div");
            style(cell, &[("margin", "2px"), ("padding", "2px")]);
            crate::dom::append_child(cell, crate::dom::create_text_node(&format!("cell {index}")));
            crate::dom::append_child(grid, cell);
            cells.push(cell);
        }
        assert!(crate::incremental_layout::flush());
        assert_matches_full("grid initial");

        let text = crate::dom::with_document(|document| {
            document
                .get_node(w3cos_dom::node::NodeId::from_u32(cells[1]))
                .first_child
                .expect("text")
                .as_u32()
        });
        crate::dom::set_text_content(
            text,
            "a much longer grid cell that wraps onto another row of the track",
        );
        assert!(crate::incremental_layout::flush());
        assert_matches_full("grid text");

        let extra = crate::dom::create_element("div");
        style(extra, &[("margin", "2px"), ("padding", "2px")]);
        crate::dom::append_child(extra, crate::dom::create_text_node("extra cell"));
        crate::dom::append_child(grid, extra);
        assert!(crate::incremental_layout::flush());
        assert_matches_full("grid append");
        crate::dom::remove_child(grid, extra);
        assert!(crate::incremental_layout::flush());
        assert_matches_full("grid remove");
    }

    #[test]
    fn class_width_toggle_matches_full_layout() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(".wide", &[("width", "48px")]);
        let (_column, items) = flex_column();
        crate::dom::class_list_add(items[0], "wide");
        assert!(crate::incremental_layout::flush());
        expect_column_flex_edit("class toggle", "style-subtree", None);
        assert_matches_full("class on");
        crate::dom::class_list_remove(items[0], "wide");
        assert!(crate::incremental_layout::flush());
        assert_matches_full("class off");
        crate::stylesheet::clear_rules();
    }

    /// A padding class toggle under LayoutNG patches the retained component
    /// (no Taffy tree to patch) instead of re-lowering the document.
    #[test]
    fn class_padding_toggle_keeps_lowered_tree() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(".pad", &[("padding", "12px")]);
        crate::dom::reset_document();
        crate::incremental_layout::invalidate();
        crate::jsdom::set_viewport(1024.0, 768.0);
        // Row flex messages with auto width (a chat log): the bubble is not
        // the subtree path's scope, so this exercises the retained compute.
        let mut items = Vec::new();
        for index in 0..4 {
            let row = crate::dom::create_element("div");
            style(row, &[("display", "flex"), ("gap", "6px")]);
            let avatar = crate::dom::create_element("div");
            style(avatar, &[("width", "20px"), ("height", "20px")]);
            let bubble = crate::dom::create_element("div");
            style(bubble, &[("min-width", "0px")]);
            crate::dom::append_child(
                bubble,
                crate::dom::create_text_node(&format!(
                    "message {index} alpha beta gamma delta epsilon zeta eta theta"
                )),
            );
            crate::dom::append_child(row, avatar);
            crate::dom::append_child(row, bubble);
            crate::dom::append_child(crate::dom::body_id(), row);
            items.push(bubble);
        }
        assert!(crate::incremental_layout::flush());
        assert_matches_full("rows initial");
        // A flex item sizes from its content in the row: a STYLE_LOCAL
        // restyle relays out the row (Blink: not a relayout boundary).
        let row = crate::dom::parent_node(items[1]).expect("bubble has a row");
        let (root, style_local) =
            crate::incremental_layout::style_local_root_of(items[1]).expect("retained tree");
        assert_eq!(root, row, "flex item relayout root");
        crate::dom::class_list_add(items[1], "pad");
        assert!(crate::incremental_layout::flush());
        if crate::layout_ng::mode() == crate::layout_ng::Mode::On {
            let stats = crate::incremental_layout::last_flush_stats();
            // (Under STYLE_LOCAL the row is not a safe splice root either,
            // so this stays on the retained compute.)
            let _ = style_local;
            assert_eq!(stats.kind, "retained", "padding on");
        }
        assert_matches_full("padding on");
        crate::dom::class_list_remove(items[1], "pad");
        assert!(crate::incremental_layout::flush());
        assert_matches_full("padding off");
        crate::stylesheet::clear_rules();
    }

    /// `W3COS_STYLE_ROOT_PATH=verify`: the relayout root climbed from the
    /// known style path is the one the tree search finds, over the class
    /// toggles of the neighbouring tests.
    #[test]
    fn style_root_path_matches_the_tree_search() {
        struct Off;
        impl Drop for Off {
            fn drop(&mut self) {
                crate::incremental_layout::STYLE_ROOT_PATH_OVERRIDE.with(|mode| mode.set(None));
            }
        }
        let _off = Off;
        crate::incremental_layout::STYLE_ROOT_PATH_OVERRIDE.with(|mode| mode.set(Some(2)));
        let before =
            crate::incremental_layout::STYLE_ROOT_PATH_MISMATCHES.with(std::cell::Cell::get);
        class_width_toggle_matches_full_layout();
        class_padding_toggle_keeps_lowered_tree();
        style_local_own_width_edit_matches_full();
        let after =
            crate::incremental_layout::STYLE_ROOT_PATH_MISMATCHES.with(std::cell::Cell::get);
        assert_eq!(
            after, before,
            "kept-path relayout root differs from the search"
        );
        crate::incremental_layout::STYLE_ROOT_PATH_OVERRIDE.with(|mode| mode.set(Some(1)));
        class_width_toggle_matches_full_layout();
        class_padding_toggle_keeps_lowered_tree();
        style_local_own_width_edit_matches_full();
    }

    /// `W3COS_TAIL_SHIFT_FAST`: the bitmap dedup keeps exactly the sorted
    /// set, and a class toggle that moves the following messages still
    /// matches a full layout.
    #[test]
    fn tail_shift_fast_matches_the_sorted_shift() {
        struct Off;
        impl Drop for Off {
            fn drop(&mut self) {
                TAIL_SHIFT_FAST_OVERRIDE.with(|m| m.set(None));
            }
        }
        let _off = Off;
        TAIL_SHIFT_FAST_OVERRIDE.with(|m| m.set(Some(2)));
        let mut ids = vec![9, 3, 70, 3, 0, 70, 129, 9];
        dedup_hosts(&mut ids);
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, vec![0, 3, 9, 70, 129]);
        assert_eq!(ids.len(), 5);
        assert_eq!(TAIL_SHIFT_FAST_MISMATCHES.with(std::cell::Cell::get), 0);
        for mode in [2u8, 1] {
            TAIL_SHIFT_FAST_OVERRIDE.with(|m| m.set(Some(mode)));
            crate::stylesheet::clear_rules();
            crate::stylesheet::register_rule(".pad", &[("padding", "14px")]);
            crate::dom::reset_document();
            crate::incremental_layout::invalidate();
            crate::jsdom::set_viewport(1024.0, 768.0);
            let mut bubbles = Vec::new();
            for index in 0..5 {
                let bubble = crate::dom::create_element("div");
                let p = crate::dom::create_element("p");
                crate::dom::append_child(
                    p,
                    crate::dom::create_text_node(&format!("message {index} alpha beta")),
                );
                crate::dom::append_child(bubble, p);
                crate::dom::append_child(crate::dom::body_id(), bubble);
                bubbles.push(bubble);
            }
            assert!(crate::incremental_layout::flush());
            for step in 0..4 {
                if step % 2 == 0 {
                    crate::dom::class_list_add(bubbles[1], "pad");
                } else {
                    crate::dom::class_list_remove(bubbles[1], "pad");
                }
                assert!(crate::incremental_layout::flush());
                assert_matches_full(&format!("tail shift mode {mode} step {step}"));
            }
        }
        assert_eq!(TAIL_SHIFT_FAST_MISMATCHES.with(std::cell::Cell::get), 0);
        crate::stylesheet::clear_rules();
    }

    /// A class that adds padding to a box whose first child's top margin
    /// used to collapse through it moves the box by that margin (CSS 2
    /// §8.3.1): the edit relays out from the parent, as Blink recomputes the
    /// margin strut there, instead of a subtree solve plus a tail shift.
    #[test]
    fn padding_toggle_through_collapsed_margin_matches_full() {
        for (nest, span, pad) in [
            (false, false, "14px"),
            (true, false, "14px"),
            (false, true, "14px"),
            (true, true, "14px"),
            (true, true, "0px 14px"),
            (true, false, "14px 0px"),
        ] {
            crate::stylesheet::clear_rules();
            crate::stylesheet::register_rule(".pad", &[("padding", pad)]);
            crate::dom::reset_document();
            crate::incremental_layout::invalidate();
            crate::jsdom::set_viewport(1024.0, 768.0);
            let list = if nest {
                let list = crate::dom::create_element("div");
                crate::dom::append_child(crate::dom::body_id(), list);
                list
            } else {
                crate::dom::body_id()
            };
            let mut bubbles = Vec::new();
            for index in 0..4 {
                let bubble = crate::dom::create_element("div");
                let p = crate::dom::create_element("p");
                crate::dom::append_child(
                    p,
                    crate::dom::create_text_node(&format!("message {index}")),
                );
                crate::dom::append_child(bubble, p);
                if span {
                    let meta = crate::dom::create_element("span");
                    crate::dom::append_child(meta, crate::dom::create_text_node("meta"));
                    crate::dom::append_child(bubble, meta);
                }
                crate::dom::append_child(list, bubble);
                bubbles.push(bubble);
            }
            assert!(crate::incremental_layout::flush());
            for step in 0..2 {
                if step == 0 {
                    crate::dom::class_list_add(bubbles[1], "pad");
                } else {
                    crate::dom::class_list_remove(bubbles[1], "pad");
                }
                assert!(crate::incremental_layout::flush());
                assert_matches_full(&format!("nest {nest} span {span} pad {pad} step {step}"));
            }
        }
        crate::stylesheet::clear_rules();
    }

    /// `W3COS_PREORDER_HOSTS`: the moved boxes read as preorder ranges of
    /// the kept host ids are the ones the subtree walk finds, through class
    /// toggles, appended messages, removals and text edits.
    #[test]
    fn preorder_hosts_match_the_walk() {
        use crate::incremental_layout::{
            PREORDER_HOSTS_MISMATCHES, PREORDER_HOSTS_OVERRIDE, PREORDER_HOSTS_RANGED,
        };
        struct Off;
        impl Drop for Off {
            fn drop(&mut self) {
                PREORDER_HOSTS_OVERRIDE.with(|m| m.set(None));
            }
        }
        let _off = Off;
        for mode in [0u8, 2, 1] {
            PREORDER_HOSTS_OVERRIDE.with(|m| m.set(Some(mode)));
            PREORDER_HOSTS_RANGED.with(|n| n.set(0));
            crate::stylesheet::clear_rules();
            crate::stylesheet::register_rule(".bub", &[("padding", "4px")]);
            crate::stylesheet::register_rule(".bub.pad", &[("padding", "14px")]);
            crate::dom::reset_document();
            crate::incremental_layout::invalidate();
            crate::jsdom::set_viewport(1024.0, 768.0);
            let list = crate::dom::create_element("div");
            crate::dom::append_child(crate::dom::body_id(), list);
            let mut bubbles = Vec::new();
            let mut texts = Vec::new();
            let add = |index: usize, bubbles: &mut Vec<u32>, texts: &mut Vec<_>| {
                let bubble = crate::dom::create_element("div");
                crate::dom::class_list_add(bubble, "bub");
                let p = crate::dom::create_element("p");
                let text = crate::dom::create_text_node(&format!("message {index} alpha beta"));
                crate::dom::append_child(p, text);
                let span = crate::dom::create_element("span");
                crate::dom::append_child(span, crate::dom::create_text_node("meta"));
                crate::dom::append_child(bubble, p);
                crate::dom::append_child(bubble, span);
                if index >= 10 && bubbles.len() > 2 {
                    // In the middle: the later messages are kept and move.
                    crate::dom::insert_before(list, bubble, bubbles[2]);
                    bubbles.insert(2, bubble);
                    texts.insert(2, text);
                } else {
                    crate::dom::append_child(list, bubble);
                    bubbles.push(bubble);
                    texts.push(text);
                }
            };
            for index in 0..6 {
                add(index, &mut bubbles, &mut texts);
            }
            assert!(crate::incremental_layout::flush());
            for step in 0..8 {
                match step % 4 {
                    0 => crate::dom::class_list_add(bubbles[1], "pad"),
                    1 => add(10 + step, &mut bubbles, &mut texts),
                    2 => crate::dom::set_text_content(
                        texts[2],
                        &format!("message two grows {step} {}", "with many more words to wrap ".repeat(8 * step)),
                    ),
                    _ => {
                        crate::dom::class_list_remove(bubbles[1], "pad");
                        let gone = bubbles.remove(3);
                        texts.remove(3);
                        crate::dom::remove_child(list, gone);
                    }
                }
                assert!(crate::incremental_layout::flush());
                assert_matches_full(&format!("preorder hosts mode {mode} step {step}"));
            }
            assert!(
                mode == 0 || PREORDER_HOSTS_RANGED.with(std::cell::Cell::get) > 0,
                "mode {mode}: no splice read the ranges"
            );
        }
        assert_eq!(PREORDER_HOSTS_MISMATCHES.with(std::cell::Cell::get), 0);
        crate::stylesheet::clear_rules();
    }

    /// `W3COS_ELIG_IFC_MEMO`: a padding toggle on a bubble keeps its
    /// paragraphs' IFC verdicts; an inherited `text-align: justify` (the
    /// paragraphs' own revisions change) and a text edit recompute them.
    #[test]
    fn elig_ifc_memo_matches_the_gate() {
        struct Off;
        impl Drop for Off {
            fn drop(&mut self) {
                crate::layout_ng::set_elig_ifc_memo_override(None);
            }
        }
        let _off = Off;
        let run = |check: bool| {
            crate::stylesheet::clear_rules();
            crate::stylesheet::register_rule(".pad", &[("padding", "14px")]);
            crate::stylesheet::register_rule(".just", &[("text-align", "justify")]);
            crate::dom::reset_document();
            crate::incremental_layout::invalidate();
            crate::jsdom::set_viewport(1024.0, 768.0);
            let mut bubbles = Vec::new();
            let mut texts = Vec::new();
            for index in 0..4 {
                let row = crate::dom::create_element("div");
                style(row, &[("display", "flex"), ("gap", "6px")]);
                let avatar = crate::dom::create_element("div");
                style(avatar, &[("width", "20px"), ("height", "20px")]);
                let bubble = crate::dom::create_element("div");
                style(bubble, &[("min-width", "0px")]);
                for para in 0..3 {
                    let p = crate::dom::create_element("p");
                    let text = crate::dom::create_text_node(&format!(
                        "message {index} paragraph {para} alpha beta gamma "
                    ));
                    crate::dom::append_child(p, text);
                    texts.push(text);
                    let b = crate::dom::create_element("b");
                    crate::dom::append_child(b, crate::dom::create_text_node("bold"));
                    crate::dom::append_child(p, b);
                    crate::dom::append_child(p, crate::dom::create_text_node(" tail words"));
                    crate::dom::append_child(bubble, p);
                }
                crate::dom::append_child(row, avatar);
                crate::dom::append_child(row, bubble);
                crate::dom::append_child(crate::dom::body_id(), row);
                bubbles.push(bubble);
            }
            assert!(crate::incremental_layout::flush());
            if check {
                assert_matches_full("ifc memo initial");
            }
            let mid = bubbles[2];
            for step in 0..6 {
                match step {
                    0 | 2 => crate::dom::class_list_add(mid, "pad"),
                    1 | 3 => crate::dom::class_list_remove(mid, "pad"),
                    4 => crate::dom::class_list_add(mid, "just"),
                    _ => crate::dom::class_list_remove(mid, "just"),
                }
                assert!(crate::incremental_layout::flush());
                if check {
                    assert_matches_full(&format!("ifc memo step {step}"));
                }
            }
            crate::dom::set_text_content(texts[7], "edited paragraph text ");
            assert!(crate::incremental_layout::flush());
            crate::dom::class_list_add(mid, "pad");
            assert!(crate::incremental_layout::flush());
            if check {
                assert_matches_full("ifc memo after text edit");
            }
        };
        if std::env::var_os("W3COS_IFC_MEMO_TEST_OFF").is_some() {
            crate::layout_ng::set_elig_ifc_memo_override(Some(0));
            run(true);
            return;
        }
        crate::layout_ng::set_elig_ifc_memo_override(Some(2));
        let (hits, mismatches) = crate::layout_ng::elig_ifc_memo_stats();
        run(false);
        let (hits_after, mismatches_after) = crate::layout_ng::elig_ifc_memo_stats();
        assert_eq!(
            mismatches_after, mismatches,
            "kept IFC verdict differs from the gate"
        );
        if crate::layout_ng::mode() == crate::layout_ng::Mode::On {
            assert!(hits_after > hits, "no kept IFC verdict was used");
        }
        crate::layout_ng::set_elig_ifc_memo_override(Some(1));
        run(true);
    }

    /// An inherited `text-align: justify` sends the page to the legacy
    /// fallback (`text:justify`); removing it must bring the paragraphs'
    /// lowered styles back, so the page returns to LayoutNG and matches a
    /// full layout (the retained patch kept their stale justify).
    #[test]
    fn inherited_justify_toggle_leaves_the_fallback() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(".pad", &[("padding", "14px")]);
        crate::stylesheet::register_rule(".just", &[("text-align", "justify")]);
        crate::dom::reset_document();
        crate::incremental_layout::invalidate();
        crate::jsdom::set_viewport(1024.0, 768.0);
        let mut bubbles = Vec::new();
        for index in 0..4 {
            let row = crate::dom::create_element("div");
            style(row, &[("display", "flex"), ("gap", "6px")]);
            let avatar = crate::dom::create_element("div");
            style(avatar, &[("width", "20px"), ("height", "20px")]);
            let bubble = crate::dom::create_element("div");
            style(bubble, &[("min-width", "0px")]);
            for para in 0..3 {
                let p = crate::dom::create_element("p");
                crate::dom::append_child(
                    p,
                    crate::dom::create_text_node(&format!(
                        "message {index} paragraph {para} alpha beta gamma "
                    )),
                );
                let b = crate::dom::create_element("b");
                crate::dom::append_child(b, crate::dom::create_text_node("bold"));
                crate::dom::append_child(p, b);
                crate::dom::append_child(p, crate::dom::create_text_node(" tail words"));
                crate::dom::append_child(bubble, p);
            }
            crate::dom::append_child(row, avatar);
            crate::dom::append_child(row, bubble);
            crate::dom::append_child(crate::dom::body_id(), row);
            bubbles.push(bubble);
        }
        assert!(crate::incremental_layout::flush());
        assert_matches_full("justify initial");
        let mid = bubbles[2];
        for (step, (class, add)) in [
            ("pad", true),
            ("pad", false),
            ("just", true),
            ("just", false),
            ("pad", true),
            ("just", true),
            ("pad", false),
            ("just", false),
        ]
        .into_iter()
        .enumerate()
        {
            if add {
                crate::dom::class_list_add(mid, class);
            } else {
                crate::dom::class_list_remove(mid, class);
            }
            assert!(crate::incremental_layout::flush());
            assert_matches_full(&format!("justify step {step} ({class} {add})"));
        }
    }

    /// css/css-text/letter-spacing/letter-spacing-trim-start-002.html:
    /// `width: 15ch` -> `width: max-content` on a block (legacy layout reads
    /// the keyword as auto) must lay it out at its containing block's width,
    /// not inside a wrapper as wide as its old 15ch border box.
    #[test]
    fn style_local_own_width_edit_matches_full() {
        crate::stylesheet::clear_rules();
        crate::dom::reset_document();
        crate::incremental_layout::invalidate();
        crate::jsdom::set_viewport(1024.0, 768.0);
        let body = crate::dom::body_id();
        style(body, &[("font", "20px/1 monospace")]);
        let mut divs = Vec::new();
        for width in ["max-content", "15ch"] {
            let div = crate::dom::create_element("div");
            style(
                div,
                &[
                    ("width", width),
                    ("margin", "10px"),
                    ("letter-spacing", "2ch"),
                ],
            );
            crate::dom::append_child(div, crate::dom::create_text_node("aaaaa bbb"));
            crate::dom::append_child(body, div);
            divs.push(div);
        }
        assert!(crate::incremental_layout::flush());
        assert_matches_full("initial");
        // The edited element's own width changed: relay out from its parent.
        let (root, _) =
            crate::incremental_layout::style_local_root_of(divs[1]).expect("retained tree");
        assert_eq!(root, divs[1], "unchanged-width relayout root");
        style(divs[1], &[("width", "max-content")]);
        assert!(crate::incremental_layout::flush());
        assert_matches_full("width max-content");
        let _ = crate::dom::with_document(|document| {
            document.get_layout_rect(NodeId::from_u32(divs[1])).width
        });
        style(divs[1], &[("width", "15ch")]);
        assert!(crate::incremental_layout::flush());
        assert_matches_full("width 15ch");
        crate::stylesheet::clear_rules();
    }

    /// css/CSS2/linebox/iframe-in-block-in-inline-ref.html: a restyle of an
    /// iframe host must keep its replaced box (inline-block, 300x150
    /// fallback), not patch in its computed inline/auto style.
    #[test]
    fn iframe_restyle_keeps_replaced_box() {
        crate::stylesheet::clear_rules();
        crate::dom::reset_document();
        crate::incremental_layout::invalidate();
        crate::jsdom::set_viewport(1024.0, 768.0);
        let body = crate::dom::body_id();
        let div = crate::dom::create_element("div");
        let frame = crate::dom::create_element("iframe");
        crate::dom::append_child(div, frame);
        crate::dom::append_child(body, div);
        let after = crate::dom::create_element("div");
        crate::dom::append_child(after, crate::dom::create_text_node("after"));
        crate::dom::append_child(body, after);
        assert!(crate::incremental_layout::flush());
        assert_matches_full("initial");
        style(frame, &[("border-width", "3px")]);
        assert!(crate::incremental_layout::flush());
        assert_matches_full("iframe border");
        style(frame, &[("color", "red")]);
        assert!(crate::incremental_layout::flush());
        assert_matches_full("iframe color");
        crate::stylesheet::clear_rules();
    }

    /// css/css-tables/min-max-size-table-content-box-ref.html: removing and
    /// re-inserting the only child of a max-width grid item (an
    /// inline-block, blockified) with a layout flush in between must lay
    /// it out as a full layout does.
    #[test]
    fn grid_item_child_remove_reinsert_matches_full() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            ".table",
            &[
                ("box-sizing", "content-box"),
                ("border", "3px solid black"),
                ("padding", "5px"),
                ("display", "inline-block"),
            ],
        );
        crate::stylesheet::register_rule(".td", &[("padding", "1px")]);
        crate::stylesheet::register_rule(".big", &[("width", "75px"), ("height", "75px")]);
        crate::stylesheet::register_rule(
            ".grid",
            &[("display", "grid"), ("width", "75px"), ("height", "75px")],
        );
        crate::dom::reset_document();
        crate::incremental_layout::invalidate();
        crate::jsdom::set_viewport(1024.0, 768.0);
        let body = crate::dom::body_id();
        style(body, &[("margin", "0px"), ("font", "16px/0 monospace")]);
        let class = |tag: &str, name: &str| {
            let node = crate::dom::create_element(tag);
            crate::dom::class_list_add(node, name);
            node
        };
        for _ in 0..2 {
            let table = class("div", "table");
            let td = class("div", "td");
            crate::dom::append_child(td, class("div", "big"));
            crate::dom::append_child(table, td);
            crate::dom::append_child(body, table);
            crate::dom::append_child(body, crate::dom::create_element("br"));
            crate::dom::append_child(body, crate::dom::create_text_node("\n"));
        }
        let grid = class("div", "grid");
        let table = class("div", "table");
        style(table, &[("max-width", "50px")]);
        let td = class("div", "td");
        crate::dom::append_child(table, td);
        crate::dom::append_child(grid, table);
        crate::dom::append_child(body, grid);
        assert!(crate::incremental_layout::flush());
        assert_matches_full("grid item initial");
        crate::dom::remove_child(table, td);
        assert!(crate::incremental_layout::flush());
        assert_matches_full("grid item child removed");
        crate::dom::append_child(table, td);
        assert!(crate::incremental_layout::flush());
        assert_matches_full("grid item child re-inserted");
        crate::stylesheet::clear_rules();
    }

    /// A row flex item that fills the row (the only growing item, definite
    /// basis, `min-width: 0`, rigid fixed-width siblings) keeps its width
    /// when its padding changes, so a STYLE_LOCAL restyle relays out only
    /// that item; the result matches a full layout either way.
    #[test]
    fn fill_flex_item_padding_toggle_is_its_own_relayout_root() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(".pad", &[("padding", "12px")]);
        crate::dom::reset_document();
        crate::incremental_layout::invalidate();
        crate::jsdom::set_viewport(1024.0, 768.0);
        let mut items = Vec::new();
        for index in 0..4 {
            let row = crate::dom::create_element("div");
            style(row, &[("display", "flex"), ("gap", "6px")]);
            let avatar = crate::dom::create_element("div");
            style(
                avatar,
                &[("width", "20px"), ("height", "20px"), ("flex", "none")],
            );
            let bubble = crate::dom::create_element("div");
            style(bubble, &[("flex", "1 1 0px"), ("min-width", "0px")]);
            crate::dom::append_child(
                bubble,
                crate::dom::create_text_node(&format!(
                    "message {index} alpha beta gamma delta epsilon zeta eta theta"
                )),
            );
            crate::dom::append_child(row, avatar);
            crate::dom::append_child(row, bubble);
            crate::dom::append_child(crate::dom::body_id(), row);
            items.push(bubble);
        }
        assert!(crate::incremental_layout::flush());
        assert_matches_full("fill rows initial");
        let (root, _) =
            crate::incremental_layout::style_local_root_of(items[1]).expect("retained tree");
        assert_eq!(root, items[1], "fill item relayout root");
        crate::dom::class_list_add(items[1], "pad");
        assert!(crate::incremental_layout::flush());
        assert_matches_full("fill padding on");
        crate::dom::class_list_remove(items[1], "pad");
        assert!(crate::incremental_layout::flush());
        assert_matches_full("fill padding off");
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn restyles_between_retained_passes_relayout_their_boxes() {
        // (The legacy path still restores width memos across other edits.)
        if crate::layout_ng::mode() != crate::layout_ng::Mode::On {
            return;
        }
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(".pad", &[("padding", "12px")]);
        crate::dom::reset_document();
        crate::incremental_layout::invalidate();
        crate::jsdom::set_viewport(1024.0, 768.0);
        // Per-box layout inputs carry over between retained passes; a box
        // restyled by a flush that laid nothing out (a width memo hit, a
        // paint-only edit) must still lay out again in the next one.
        let mut items = Vec::new();
        for index in 0..4 {
            let row = crate::dom::create_element("div");
            style(row, &[("display", "flex"), ("gap", "6px")]);
            let avatar = crate::dom::create_element("div");
            style(avatar, &[("width", "20px"), ("height", "20px")]);
            let bubble = crate::dom::create_element("div");
            style(bubble, &[("min-width", "0px")]);
            crate::dom::append_child(
                bubble,
                crate::dom::create_text_node(&format!(
                    "message {index} alpha beta gamma delta epsilon zeta eta theta"
                )),
            );
            crate::dom::append_child(row, avatar);
            crate::dom::append_child(row, bubble);
            crate::dom::append_child(crate::dom::body_id(), row);
            items.push(bubble);
        }
        assert!(crate::incremental_layout::flush());
        assert_matches_full("rows initial");
        // (Checking against a full layout resets what carries over, so
        // each run of edits is checked once at its end.)
        let runs: [&[(usize, &str, &str)]; 3] = [
            &[
                (1, "width", "180px"),
                (1, "width", "90px"),
                (1, "width", "180px"),
                (3, "width", "120px"),
            ],
            &[
                (3, "width", "60px"),
                (2, "color", "red"),
                (2, "width", "100px"),
                (0, "width", "70px"),
            ],
            &[
                (1, "width", "90px"),
                (1, "width", "180px"),
                (1, "width", "90px"),
                (0, "padding", "9px"),
            ],
        ];
        for (run, edits) in runs.into_iter().enumerate() {
            for (item, property, value) in edits {
                style(items[*item], &[(property, value)]);
                assert!(crate::incremental_layout::flush());
            }
            assert_matches_full(&format!("run {run}"));
        }
        crate::dom::class_list_add(items[2], "pad");
        assert!(crate::incremental_layout::flush());
        assert_matches_full("pad on 2");
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn stretched_flex_lines_end_at_the_container_end() {
        // (LayoutNG: the legacy path restores its width memo here.)
        if crate::layout_ng::mode() != crate::layout_ng::Mode::On {
            return;
        }
        // align-content-stretch-rounding: the last line ends exactly where
        // the container's content box does, at every 0.1px height.
        crate::stylesheet::clear_rules();
        crate::dom::reset_document();
        crate::incremental_layout::invalidate();
        crate::jsdom::set_viewport(1024.0, 768.0);
        let flex = crate::dom::create_element("div");
        style(
            flex,
            &[
                ("display", "flex"),
                ("flex-wrap", "wrap"),
                ("width", "10px"),
                ("height", "40px"),
                ("align-content", "stretch"),
            ],
        );
        let mut last = 0;
        for _ in 0..8 {
            last = crate::dom::create_element("div");
            style(last, &[("width", "10px"), ("min-height", "5px")]);
            crate::dom::append_child(flex, last);
        }
        crate::dom::append_child(crate::dom::body_id(), flex);
        assert!(crate::incremental_layout::flush());
        for step in 0..10 {
            style(
                flex,
                &[("height", &format!("{}px", 40.0 + step as f32 / 10.0))],
            );
            assert!(crate::incremental_layout::flush());
            assert_matches_full(&format!("step {step}"));
            let (container, item) = crate::dom::with_document(|document| {
                (
                    document.get_layout_rect(NodeId::from_u32(flex)),
                    document.get_layout_rect(NodeId::from_u32(last)),
                )
            });
            assert_eq!(
                item.y as f64 + item.height as f64,
                container.y as f64 + container.height as f64,
                "step {step}"
            );
        }
    }

    #[test]
    fn flex_row_width_matches_full_layout() {
        crate::dom::reset_document();
        crate::incremental_layout::invalidate();
        crate::jsdom::set_viewport(1024.0, 768.0);
        let row = crate::dom::create_element("div");
        style(
            row,
            &[
                ("display", "flex"),
                ("flex-direction", "row"),
                ("width", "420px"),
            ],
        );
        crate::dom::append_child(crate::dom::body_id(), row);
        let mut items = Vec::new();
        for index in 0..4 {
            let item = crate::dom::create_element("div");
            style(item, &[("margin", "4px"), ("padding", "4px")]);
            crate::dom::append_child(item, crate::dom::create_text_node(&format!("row {index}")));
            crate::dom::append_child(row, item);
            items.push(item);
        }
        assert!(crate::incremental_layout::flush());
        crate::dom::set_style_property(items[0], "width", "80px");
        assert!(crate::incremental_layout::flush());
        assert_matches_full("flex row width");
    }

    thread_local! {
        pub(super) static FORCE_RETAIN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    struct RetainGuard;

    impl RetainGuard {
        fn on() -> Self {
            FORCE_RETAIN.with(|force| force.set(true));
            RetainGuard
        }
    }

    impl Drop for RetainGuard {
        fn drop(&mut self) {
            FORCE_RETAIN.with(|force| force.set(false));
        }
    }

    fn assert_lowering_matches_full(label: &str) {
        let fresh = format!("{:?}", crate::dom::to_component_tree());
        let kept = super::super::CACHE
            .with(|slot| format!("{:?}", &*slot.borrow().as_ref().expect("cache").component));
        assert!(
            fresh == kept,
            "{label}: retained lowering differs from a full rebuild"
        );
    }

    fn chat_items() -> (u32, Vec<u32>) {
        let streams = markdown_chat_page(32);
        let msg = |p: u32| {
            crate::dom::with_document(|document| {
                let bubble = document.get_node(NodeId::from_u32(p)).parent.unwrap();
                document.get_node(bubble).parent.unwrap().as_u32()
            })
        };
        let msgs: Vec<u32> = streams.iter().map(|&(p, _)| msg(p)).collect();
        let chat = crate::dom::with_document(|document| {
            document
                .get_node(NodeId::from_u32(msgs[0]))
                .parent
                .unwrap()
                .as_u32()
        });
        (chat, msgs)
    }

    fn check(label: &str) {
        assert_matches_full(label);
        assert_client_rects_match_full(label);
        assert_lowering_matches_full(label);
    }

    #[test]
    fn column_flex_child_list_edits_keep_untouched_items() {
        let _retain = RetainGuard::on();
        let (chat, msgs) = chat_items();
        check("initial");
        let before = child_splices();
        // Remove the middle message, put it back, move the last one up,
        // and insert a new message between kept ones.
        crate::dom::remove_child(chat, msgs[1]);
        assert!(crate::incremental_layout::flush());
        check("remove middle");
        crate::dom::insert_before(chat, msgs[1], msgs[2]);
        assert!(crate::incremental_layout::flush());
        check("reinsert middle");
        crate::dom::insert_before(chat, msgs[2], msgs[1]);
        assert!(crate::incremental_layout::flush());
        check("move last up");
        let fresh = crate::dom::create_element("div");
        style(fresh, &[("padding", "6px"), ("margin", "2px 0 4px 0")]);
        let p = crate::dom::create_element("p");
        style(p, &[("margin", "0")]);
        crate::dom::append_child(
            p,
            crate::dom::create_text_node("inserted words ".repeat(9).as_str()),
        );
        let strong = crate::dom::create_element("strong");
        crate::dom::append_child(strong, crate::dom::create_text_node("bold"));
        crate::dom::append_child(p, strong);
        crate::dom::append_child(fresh, p);
        crate::dom::insert_before(chat, fresh, msgs[1]);
        assert!(crate::incremental_layout::flush());
        check("insert new");
        crate::dom::remove_child(chat, fresh);
        crate::dom::remove_child(chat, msgs[1]);
        assert!(crate::incremental_layout::flush());
        check("remove tail two");
        if crate::layout_ng::mode() != crate::layout_ng::Mode::Off {
            assert_eq!(
                child_splices() - before,
                5,
                "child-list edits keep the items"
            );
        }
    }

    #[test]
    fn container_child_list_edits_relayout_only_the_container() {
        let _retain = RetainGuard::on();
        let (chat, msgs) = chat_items();
        check("initial");
        let before = child_splices();
        // A growing item: the column cannot shift its followers by one
        // offset, so the container is laid out again with its kept items.
        let grow = crate::dom::create_element("div");
        style(grow, &[("flex-grow", "1"), ("padding", "3px")]);
        crate::dom::append_child(grow, crate::dom::create_text_node("grows"));
        crate::dom::insert_before(chat, grow, msgs[1]);
        assert!(crate::incremental_layout::flush());
        check("insert growing item");
        crate::dom::remove_child(chat, grow);
        assert!(crate::incremental_layout::flush());
        check("remove growing item");
        // A wrapping row and a grid after the chat.
        let row = crate::dom::create_element("div");
        style(
            row,
            &[
                ("display", "flex"),
                ("flex-wrap", "wrap"),
                ("gap", "6px"),
                ("padding", "4px"),
            ],
        );
        let grid = crate::dom::create_element("div");
        style(
            grid,
            &[
                ("display", "grid"),
                ("grid-template-columns", "repeat(3, 1fr)"),
                ("gap", "5px"),
                ("padding", "2px"),
            ],
        );
        let mut cells = Vec::new();
        for index in 0..7 {
            for parent in [row, grid] {
                let cell = crate::dom::create_element("div");
                style(cell, &[("padding", "3px"), ("width", "180px")]);
                crate::dom::append_child(
                    cell,
                    crate::dom::create_text_node(&"cell words ".repeat(index + 1)),
                );
                crate::dom::append_child(parent, cell);
                cells.push(cell);
            }
        }
        let body = crate::dom::body_id();
        crate::dom::append_child(body, row);
        crate::dom::append_child(body, grid);
        assert!(crate::incremental_layout::flush());
        check("row and grid");
        let splices = child_splices();
        crate::dom::remove_child(row, cells[2]);
        assert!(crate::incremental_layout::flush());
        check("remove from wrapping row");
        crate::dom::insert_before(row, cells[2], cells[0]);
        assert!(crate::incremental_layout::flush());
        check("insert first in wrapping row");
        crate::dom::remove_child(grid, cells[5]);
        assert!(crate::incremental_layout::flush());
        check("remove from grid");
        crate::dom::append_child(grid, cells[5]);
        assert!(crate::incremental_layout::flush());
        check("append to grid");
        if crate::layout_ng::mode() != crate::layout_ng::Mode::Off {
            assert_eq!(splices - before, 2, "growing item edits keep the items");
            assert_eq!(
                child_splices() - splices,
                4,
                "row and grid edits keep the items"
            );
        }
    }

    #[test]
    fn column_flex_prepend_keeps_untouched_items() {
        let _retain = RetainGuard::on();
        let (chat, msgs) = chat_items();
        let before = child_splices();
        // Load older history above the first message, then drop it, then
        // move the first message to the end.
        let older = crate::dom::create_element("div");
        style(
            older,
            &[("padding", "5px 7px"), ("margin", "3px 1px 6px 2px")],
        );
        crate::dom::append_child(
            older,
            crate::dom::create_text_node(&"older words ".repeat(14)),
        );
        crate::dom::insert_before(chat, older, msgs[0]);
        assert!(crate::incremental_layout::flush());
        check("prepend");
        crate::dom::remove_child(chat, older);
        assert!(crate::incremental_layout::flush());
        check("remove first");
        crate::dom::append_child(chat, msgs[0]);
        assert!(crate::incremental_layout::flush());
        check("move first to end");
        crate::dom::remove_child(chat, msgs[1]);
        crate::dom::remove_child(chat, msgs[2]);
        crate::dom::remove_child(chat, msgs[0]);
        assert!(crate::incremental_layout::flush());
        // No fragment owner is left to compare.
        assert_matches_full("remove all");
        assert_lowering_matches_full("remove all");
        crate::dom::append_child(chat, msgs[1]);
        assert!(crate::incremental_layout::flush());
        check("append into empty");
        if crate::layout_ng::mode() != crate::layout_ng::Mode::Off {
            assert_eq!(child_splices() - before, 5, "prepends keep the items");
        }
    }

    #[test]
    fn column_flex_child_list_edits_move_deferred_client_rects() {
        let _retain = RetainGuard::on();
        // No read between the layout and the edit: the CSSOM rects are still
        // deferred when each splice lands, so they move with the boxes.
        let (chat, msgs) = chat_items();
        let before = child_splices();
        crate::dom::remove_child(chat, msgs[1]);
        assert!(crate::incremental_layout::flush());
        crate::dom::insert_before(chat, msgs[1], msgs[0]);
        assert!(crate::incremental_layout::flush());
        check("remove then prepend, deferred");
        crate::incremental_layout::invalidate();
        assert!(crate::incremental_layout::flush());
        crate::dom::insert_before(chat, msgs[1], msgs[2]);
        assert!(crate::incremental_layout::flush());
        check("move after rebuild, deferred");
        if crate::layout_ng::mode() != crate::layout_ng::Mode::Off {
            assert_eq!(
                child_splices() - before,
                3,
                "deferred rects keep the splice"
            );
        }
    }

    #[test]
    fn column_flex_child_list_edit_with_counters_rebuilds() {
        let _retain = RetainGuard::on();
        let (chat, msgs) = chat_items();
        // A list item directly in the column reads a `list-item` count the
        // removed one changes.
        let li = crate::dom::create_element("div");
        style(li, &[("display", "list-item"), ("margin-left", "20px")]);
        crate::dom::append_child(li, crate::dom::create_text_node("numbered"));
        crate::dom::insert_before(chat, li, msgs[1]);
        assert!(crate::incremental_layout::flush());
        check("insert list item");
        let before = child_splices();
        crate::dom::remove_child(chat, li);
        assert!(crate::incremental_layout::flush());
        check("remove list item");
        assert_eq!(child_splices(), before, "a list item edit must rebuild");
        w3cos_dom::stylesheet::register_rule("div", &[("counter-increment", "msg")]);
        crate::incremental_layout::invalidate();
        assert!(crate::incremental_layout::flush());
        crate::dom::remove_child(chat, msgs[1]);
        assert!(crate::incremental_layout::flush());
        check("counter rule");
        assert_eq!(child_splices(), before, "counter rules must rebuild");
        w3cos_dom::stylesheet::clear_rules();
    }
}
