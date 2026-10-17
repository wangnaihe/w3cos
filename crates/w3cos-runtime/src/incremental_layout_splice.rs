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
use w3cos_std::style::{Dimension, Display, Edges, Float, Overflow, Position, Spacing, Style};
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

/// No box anywhere in `root` can make a block subtree depend on more than
/// its containing block width.
pub(super) fn tree_is_spliceable(root: &Component) -> bool {
    let mut stack = vec![root];
    while let Some(component) = stack.pop() {
        let style = &component.style;
        if style.float != Float::None
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
        matches!(child.kind, ComponentKind::Text { .. })
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

/// Ancestors between `path`'s target and the first definite-height block
/// only move or grow. Returns the number of path levels (from the target
/// upwards) whose following siblings must shift, or None when an ancestor
/// could re-flow.
fn ancestor_levels(root: &Component, path: &[usize]) -> Option<usize> {
    let mut levels = 0;
    for depth in (0..path.len()).rev() {
        let parent = at_path(root, &path[..depth]);
        levels += 1;
        let style = &parent.style;
        if !is_block_flow(style.display) || !in_flow_static(style) || is_table_like(parent) {
            return None;
        }
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
    engine.set_skip_projections(skip_projections);
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
            let skip = can_skip_projections(&cache.component);
            let measure = needs_text_metric(&cache.component);
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
            let skip = can_skip_projections(&cache.component);
            let measure = needs_text_metric(&cache.component);
            let target = at_path_mut(&mut cache.component, &plan.path);
            let tail = target.children.split_off(keep);
            target.children.extend(removed);
            let target = at_path(&cache.component, &plan.path);
            let anchor = target.children[keep - 1].clone();
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
    crate::dom::with_document_mut(|document| {
        for (id, rect) in &rects {
            document.set_layout_rect(
                NodeId::from_u32(*id),
                w3cos_dom::DOMRect::new(rect.x + dx, rect.y + dy, rect.width, rect.height),
            );
        }
        shifted.sort_unstable();
        shifted.dedup();
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
    if std::env::var_os("W3COS_SPLICE_VERIFY").is_some() {
        verify(viewport_w, viewport_h, plan.node);
    }
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
}
