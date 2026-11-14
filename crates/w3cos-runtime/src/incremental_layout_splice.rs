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

/// Children margins cannot collapse through this edge of `component`.
fn edge_blocks_margin_collapse(component: &Component, top: bool) -> bool {
    let style = &component.style;
    let (padding, border) = if top {
        (style.padding.top, border_side(style, 0))
    } else {
        (style.padding.bottom, border_side(style, 2))
    };
    if px_spacing(padding).is_some_and(|px| px > 0.0) || border > 0.0 {
        return true;
    }
    // Only inline content: there is no child margin to collapse.
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
                    || parent.children.iter().all(|item| item.style.flex_shrink == 0.0))
        }
        FlexDirection::Row => {
            matches!(style.align_items, AlignItems::Stretch | AlignItems::FlexStart)
                && parent.children.iter().enumerate().all(|(index, item)| {
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
    component.style.display == Display::Flex
        && component.style.flex_direction == FlexDirection::Row
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
            if let Some(plan) = try_tail(cache, document, &chain, &chains) {
                return Ok(Some(plan));
            }
            if let Some(plan) = try_candidate(cache, document, &chain, node) {
                return Ok(Some(plan));
            }
            if let Some(plan) = try_container(cache, document, &chain, node) {
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

/// Only the trailing children of a block changed. The kept prefix keeps its
/// cached results (LayoutNG reuses a clean child's layout result); the last
/// kept child is solved again only to seed sibling margin collapsing.
fn try_tail(
    cache: &Cache,
    document: &w3cos_dom::document::Document,
    chain: &[u32],
    chains: &[Vec<u32>],
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
    if !is_block_flow(style.display)
        || !in_flow_static(style)
        || !(definite
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
    solve_with_client_rects(wrapper, skip_projections, measure_text, viewport_w, viewport_h, false)
        .map(|(rects, own, _)| (rects, own))
}

type ClientRectMap = std::collections::HashMap<u32, Vec<w3cos_dom::DOMRect>>;

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
            ClientRectMap::new()
        };
        return host_rects(&flat, &layout_cache).map(|(rects, own)| (rects, own, published));
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
        ClientRectMap::new()
    };
    host_rects(&flat, &results.layout_cache).map(|(rects, own)| (rects, own, published))
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
fn anchor_stand_in(source: &Component) -> Component {
    let height = host_id(source).map(|id| {
        crate::dom::with_document(|document| document.get_layout_rect(NodeId::from_u32(id)).height)
    });
    let strut = || {
        if edge_blocks_margin_collapse(source, false) {
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
                (rects, dx, dy, dh, skip, measure, PlanKind::Subtree(fresh), None)
            } else {
                let old = at_path(&cache.component, &plan.path);
                let margins = old.style.margin_lengths();
                let width = node_rect.width + margins.left + margins.right;
                if !(node_rect.height > 0.0) || !width.is_finite() || width <= 0.0 {
                    return bail("solve 555");
                }
                // Engine flags are whole-tree properties: a full rebuild would
                // derive them from the spliced tree, so the subtree uses those.
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
                let mut wrapper = wrapper_for(parent, width, vec![fresh]);
                let Some((rects, _, client)) = solve_with_client_rects(
                    &wrapper, skip, measure, viewport_w, viewport_h, true,
                ) else {
                    return bail("solve 568");
                };
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
            let target = at_path_mut(&mut cache.component, &plan.path);
            let removed = target.children.split_off(keep);
            target.children.extend(tail);
            let skip = spliced_skip_projections(
                cache,
                &removed,
                &at_path(&cache.component, &plan.path).children[keep..],
            );
            let measure = text_metric_after_edit(cache, &plan.path);
            let target = at_path_mut(&mut cache.component, &plan.path);
            let tail = target.children.split_off(keep);
            target.children.extend(removed);
            let target = at_path(&cache.component, &plan.path);
            let anchor = anchor_stand_in(&target.children[keep - 1]);
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
            let mut children = Vec::with_capacity(tail.len() + 1);
            children.push(anchor);
            children.extend(tail);
            let mut wrapper = wrapper_for(target, width, children);
            let Some((rects, own, client)) =
                solve_with_client_rects(&wrapper, skip, measure, viewport_w, viewport_h, true)
            else {
                return bail("solve 616");
            };
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
    };

    // Boxes after the subtree move down by `dh`; auto-height ancestors grow.
    let mut shifted = Vec::new();
    let mut grown = Vec::new();
    if matches!(commit, PlanKind::Tail { .. }) {
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
    let mut shifted_owners = std::collections::HashMap::new();
    if dh != 0.0 {
        if !row_item_stays_tallest(&cache.component, &plan.path, levels, dh) {
            return bail("row item height");
        }
        for level in 0..levels {
            let depth = plan.path.len() - 1 - level;
            let parent = at_path(&cache.component, &plan.path[..depth]);
            // Items beside the changed one in a row keep their boxes.
            if !is_row_flex(parent) {
                for sibling in &parent.children[plan.path[depth] + 1..] {
                    collect_hosts(sibling, &mut shifted);
                    super::collect_cssom_owners(sibling, &mut shifted_owners);
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
    shifted.sort_unstable();
    shifted.dedup();
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
    };
    let mut old_owners = std::collections::HashMap::new();
    let mut new_owners = std::collections::HashMap::new();
    for component in &old_region {
        super::collect_cssom_owners(component, &mut old_owners);
    }
    for component in &new_region {
        super::collect_cssom_owners(component, &mut new_owners);
    }
    let touches_owners =
        !old_owners.is_empty() || !new_owners.is_empty() || !shifted_owners.is_empty();
    if touches_owners {
        // The roots of the region and every ancestor (for a tail plan, the
        // target block too) must not be fragments of an owner themselves.
        let roots_own = old_region
            .iter()
            .chain(new_region.iter())
            .any(|component| carries_cssom_owner(component));
        let ancestor_depths = match &commit {
            PlanKind::Subtree(_) => plan.path.len(),
            PlanKind::Tail { .. } => plan.path.len() + 1,
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
        if old_owners.iter().any(|(owner, count)| total(owner) != *count)
            || new_owners
                .keys()
                .any(|owner| total(owner) != old_owners.get(owner).copied().unwrap_or(0))
            || shifted_owners.iter().any(|(owner, count)| total(owner) != *count)
        {
            return bail("cssom owner outside splice");
        }
    }
    crate::dom::with_document_mut(|document| {
        for (id, rect) in &rects {
            document.set_layout_rect(
                NodeId::from_u32(*id),
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
    let target = at_path_mut(&mut cache.component, &plan.path);
    SPLICES.with(|count| {
        let (subtree, tail) = count.get();
        count.set(match commit {
            PlanKind::Subtree(_) => (subtree + 1, tail),
            PlanKind::Tail { .. } => (subtree, tail + 1),
        });
    });
    match commit {
        PlanKind::Subtree(fresh) => *target = fresh,
        PlanKind::Tail { keep, tail } => {
            target.children.truncate(keep);
            target.children.extend(tail);
        }
    }
    cache.node_ids_stale = true;
    cache.width_memos.clear();
    let solved = crate::layout::count_nodes(at_path(&cache.component, &plan.path)) as u32;
    super::note_flush("splice", solved, shifted_nodes);
    if std::env::var_os("W3COS_SPLICE_VERIFY").is_some() {
        verify(viewport_w, viewport_h, plan.node);
    }
    true
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
    if !subtree_relayout_enabled()
        || !cache.splice_ok
        || !cache.skip_projections
        || cache.viewport_w_bits != viewport_w.to_bits()
        || cache.viewport_h_bits != viewport_h.to_bits()
        || !origin_stable(old, new)
    {
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
        at_path_mut(&mut cache.component, &solve_path).children = relowered.children;
    }
    cache.engine.invalidate();
    cache.width_memos.clear();
    super::note_flush("style-subtree", solved_nodes, shifted_nodes);
    true
}

/// Debug aid: compare every spliced rect with a from-scratch layout.
fn verify(viewport_w: f32, viewport_h: f32, node: u32) {
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
    let extra = got.keys().filter(|owner| !expected.contains_key(owner)).count();
    eprintln!(
        "[splice-verify] root {node}: {} owners, {bad_owners} mismatched, {extra} extra",
        expected.len()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(!expected.is_empty(), "{label}: page publishes no fragment owners");
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
            style(msg, &[("display", "flex"), ("flex-direction", "row"), ("gap", "8px")]);
            let avatar = crate::dom::create_element("div");
            let avatar_height = format!("{avatar_px}px");
            style(
                avatar,
                &[("width", "32px"), ("height", avatar_height.as_str()), ("flex", "none")],
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
            style(code, &[("font-family", "monospace"), ("background", "#f0f0f0")]);
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
        assert_eq!(splice_counts().0 - before.0, 12, "text edits under row messages must splice");
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
        let extra = crate::dom::create_element("div");
        style(extra, &[("margin", "4px"), ("padding", "4px")]);
        crate::dom::append_child(extra, crate::dom::create_text_node("tail item"));
        crate::dom::append_child(column, extra);
        assert!(crate::incremental_layout::flush());
        assert_matches_full("append after long text");
        if crate::layout_ng::mode() == crate::layout_ng::Mode::On {
            assert!(
                column_tail_reuses() > before,
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
        crate::dom::remove_child(column, extra);
        assert!(crate::incremental_layout::flush());
        assert_matches_full("remove tail");
        if crate::layout_ng::mode() == crate::layout_ng::Mode::On {
            assert!(column_tail_reuses() > before_remove);
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
}
