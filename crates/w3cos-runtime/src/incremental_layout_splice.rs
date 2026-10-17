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

struct Plan {
    path: Vec<usize>,
    node: u32,
    fresh: Component,
}

fn plan_splice(cache: &Cache, dirty: &[NodeId]) -> Result<Option<Plan>, ()> {
    let root = host_id(&cache.component).ok_or(())?;
    let lift = w3cos_dom::stylesheet::tree_order_affects_matching();
    crate::dom::with_document(|document| {
        let mut lca: Option<Vec<u32>> = None;
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
                None => lca = Some(chain),
                Some(prefix) => common_prefix(prefix, &chain),
            }
        }
        let Some(mut chain) = lca else {
            return Ok(None);
        };
        // Walk up until the subtree root is a safe block box.
        while chain.len() > 2 {
            let node = *chain.last().unwrap();
            if let Some(plan) = try_candidate(cache, document, &chain, node) {
                return Ok(Some(plan));
            }
            chain.pop();
        }
        Err(())
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
    Some(Plan { path, node, fresh })
}

fn wrapper_for(parent: &Component, width: f32, child: Component) -> Component {
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
        children: vec![child],
        on_click: w3cos_std::EventAction::None,
        sticky_counter_signal: None,
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
        return false;
    }
    let plan = match plan_splice(cache, dirty) {
        Ok(Some(plan)) => plan,
        // Every mutation was off-document: the used boxes are unchanged.
        Ok(None) => return true,
        Err(()) => return false,
    };
    let Some(levels) = ancestor_levels(&cache.component, &plan.path) else {
        return false;
    };
    let node = NodeId::from_u32(plan.node);
    let old_rect = crate::dom::with_document(|document| document.get_layout_rect(node));
    let old = at_path(&cache.component, &plan.path);
    let margins = old.style.margin_lengths();
    let width = old_rect.width + margins.left + margins.right;
    if !(old_rect.height > 0.0) || !width.is_finite() || width <= 0.0 {
        return false;
    }
    // Engine flags are whole-tree properties: a full rebuild would derive
    // them from the spliced tree, so the subtree solve uses exactly those.
    let old_subtree = std::mem::replace(at_path_mut(&mut cache.component, &plan.path), plan.fresh);
    let skip_projections = can_skip_projections(&cache.component);
    let measure_text = needs_text_metric(&cache.component);
    let fresh = std::mem::replace(at_path_mut(&mut cache.component, &plan.path), old_subtree);
    let parent = at_path(&cache.component, &plan.path[..plan.path.len() - 1]);
    let wrapper = wrapper_for(parent, width, fresh);
    let flat = pre_flatten(&wrapper);
    let mut engine = LayoutEngine::new();
    engine.set_skip_projections(skip_projections);
    engine.set_measure_text(measure_text);
    let Ok(results) = engine.compute(&wrapper, &flat, viewport_w, viewport_h) else {
        return false;
    };
    let Some(new_rect) = results.layout_cache.iter().find_map(|(rect, index)| {
        (flat.get(*index).map(flat_host_id) == Some(plan.node)).then_some(*rect)
    }) else {
        return false;
    };
    if !(new_rect.height > 0.0) || (new_rect.width - old_rect.width).abs() > 0.01 {
        return false;
    }
    let dx = old_rect.x - new_rect.x;
    let dy = old_rect.y - new_rect.y;
    let dh = new_rect.height - old_rect.height;

    // Boxes after the subtree move down by `dh`; auto-height ancestors grow.
    let mut shifted = Vec::new();
    let mut grown = Vec::new();
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
    }
    crate::dom::with_document_mut(|document| {
        for (rect, index) in &results.layout_cache {
            let Some(info) = flat.get(*index) else {
                continue;
            };
            let id = flat_host_id(info);
            if id == u32::MAX {
                continue;
            }
            document.set_layout_rect(
                NodeId::from_u32(id),
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
    drop(flat);
    let mut wrapper = wrapper;
    let fresh = wrapper.children.pop().expect("wrapper child");
    cache.skip_projections = skip_projections;
    cache.measure_text = measure_text;
    *at_path_mut(&mut cache.component, &plan.path) = fresh;
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
        for step in 0..40 {
            crate::dom::set_text_content(text, &"streamed words ".repeat(step + 1));
            assert!(crate::incremental_layout::flush());
            assert_matches_full(&format!("step {step}"));
        }
    }
}
