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
    AlignItems, AlignSelf, Dimension, Display, Edges, FlexDirection, FlexWrap, Float,
    JustifyContent, Overflow, Position, Spacing, Style,
};
use w3cos_std::{Component, ComponentKind};

use super::{Cache, can_skip_projections, flat_host_id, host_id, needs_text_metric};
use crate::layout::{LayoutEngine, pre_flatten};

thread_local! {
    /// (subtree splices, tail splices) since thread start.
    static SPLICES: std::cell::Cell<(u32, u32)> = const { std::cell::Cell::new((0, 0)) };
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
    let mut stack = vec![root];
    while let Some(component) = stack.pop() {
        let style = &component.style;
        // The sparse CSSOM fragment map is rebuilt from a complete flattened
        // tree. Keep split-inline trees on that path until splice publishes
        // equivalent ordered fragment ownership itself.
        if is_split_inline_fragment(component)
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
    ancestor_levels(&cache.component, &path)?;
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
            || !tree_is_spliceable(&component)
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
    ancestor_levels(&cache.component, &path)?;
    let fresh = document.to_component_subtree(NodeId::from_u32(node));
    if fresh.style != old.style
        || host_id(&fresh) != Some(node)
        || !tree_is_spliceable(&fresh)
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
    ancestor_levels(&cache.component, &path)?;
    let fresh = document.to_component_subtree(NodeId::from_u32(node));
    if host_id(&fresh) != Some(node)
        || fresh.style != old.style
        || !tree_is_spliceable(&fresh)
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
    style.display = Display::Block;
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
    let flat = pre_flatten(wrapper);
    let mut engine = LayoutEngine::new();
    // Projections repair floats, tables, inline-block and RTL boxes. A
    // subtree with none of them gets nothing from them even when the rest
    // of the document needs them (checked by W3COS_SPLICE_VERIFY).
    engine.set_skip_projections(skip_projections || can_skip_projections(wrapper));
    engine.set_measure_text(measure_text);
    let results = engine
        .compute(wrapper, &flat, viewport_w, viewport_h)
        .ok()?;
    let mut rects = Vec::with_capacity(results.layout_cache.len());
    let mut own = None;
    for (rect, index) in &results.layout_cache {
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

pub(super) fn try_splice(
    cache: &mut Cache,
    dirty: &[NodeId],
    viewport_w: f32,
    viewport_h: f32,
) -> bool {
    if !cache.splice_ok
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
    let Some(levels) = ancestor_levels(&cache.component, &plan.path) else {
        return bail("ancestors");
    };
    let node = NodeId::from_u32(plan.node);
    let node_rect = crate::dom::with_document(|document| document.get_layout_rect(node));
    // Solved rects to store, offset to apply to them, height change of the
    // plan node's border box, and the first path level whose following
    // siblings shift (0: the plan node's siblings; the Tail plan grows the
    // plan node itself and starts there too).
    let (rects, dx, dy, dh, skip_projections, measure_text, commit): (
        Vec<(u32, crate::layout::LayoutRect)>,
        f32,
        f32,
        f32,
        bool,
        bool,
        PlanKind,
    ) = match plan.kind {
        PlanKind::Subtree(fresh) => {
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
            let Some((rects, _)) = solve(&wrapper, skip, measure, viewport_w, viewport_h) else {
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
            )
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
            let Some((rects, own)) = solve(&wrapper, skip, measure, viewport_w, viewport_h) else {
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
            )
        }
    };

    // Boxes after the subtree move down by `dh`; auto-height ancestors grow.
    let mut shifted = Vec::new();
    let mut grown = Vec::new();
    if matches!(commit, PlanKind::Tail { .. }) {
        grown.push(plan.node);
    }
    if dh != 0.0 {
        for level in 0..levels {
            let depth = plan.path.len() - 1 - level;
            let parent = at_path(&cache.component, &plan.path[..depth]);
            for sibling in &parent.children[plan.path[depth] + 1..] {
                collect_hosts(sibling, &mut shifted);
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
    let fresh = target.clone();
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let stats = crate::incremental_layout::last_flush_stats();
        assert_eq!(
            stats.kind, "style-subtree",
            "width edit solved {}",
            stats.kind
        );
        assert!(
            stats.solved_nodes < 30,
            "width edit solved {} nodes",
            stats.solved_nodes
        );
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
        assert_eq!(
            crate::incremental_layout::last_flush_stats().kind,
            "splice",
            "text edit"
        );
        assert_matches_full("flex text");

        let extra = crate::dom::create_element("div");
        style(extra, &[("margin", "4px"), ("padding", "4px")]);
        crate::dom::append_child(extra, crate::dom::create_text_node("appended row"));
        crate::dom::append_child(column, extra);
        assert!(crate::incremental_layout::flush());
        assert_eq!(
            crate::incremental_layout::last_flush_stats().kind,
            "splice",
            "append"
        );
        assert_matches_full("flex append");

        crate::dom::remove_child(column, extra);
        assert!(crate::incremental_layout::flush());
        assert_eq!(
            crate::incremental_layout::last_flush_stats().kind,
            "splice",
            "remove"
        );
        assert_matches_full("flex remove");
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
        let stats = crate::incremental_layout::last_flush_stats();
        assert_eq!(stats.kind, "style-subtree", "class toggle {}", stats.kind);
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
