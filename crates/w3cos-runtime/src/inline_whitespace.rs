//! Hanging preserved-space tails in flex-backed inline formatting contexts.
use std::collections::HashMap;
use w3cos_std::{Component, ComponentKind, Style};
use w3cos_std::style::{AlignSelf, Dimension, Display, Edges, Float, Position, TextAlign, TextDirection, UnicodeBidi, WhiteSpace};
use crate::layout::{count_nodes, inline_used_line_height, LayoutRect};

fn passive(node: &Component, parent: &Style) -> bool {
    let s = &node.style;
    s.display == Display::Inline && s.position == Position::Static && s.float == Float::None
        && s.direction == TextDirection::Ltr && s.unicode_bidi == UnicodeBidi::Normal
        && matches!(s.align_self, AlignSelf::Auto | AlignSelf::Baseline)
        && !s.custom_properties.as_ref().is_some_and(|p| p.contains_key("--w3cos-internal-vertical-align-keyword")
            || p.contains_key("--w3cos-internal-vertical-align-length"))
        && s.font_size == parent.font_size && s.font_family == parent.font_family
        && s.font_weight == parent.font_weight && s.font_style == parent.font_style
        && s.line_height == parent.line_height && s.line_height_is_normal == parent.line_height_is_normal
        && s.padding == Edges::ZERO && s.margin == Edges::ZERO
        && s.border_width == 0.0 && [s.border_top_width, s.border_right_width,
            s.border_bottom_width, s.border_left_width].into_iter().all(|v| v.unwrap_or(0.0) == 0.0)
        && s.background.a == 0 && s.background_image.is_none()
        && s.opacity == 1.0 && s.transform.is_identity() && s.filter.is_none() && s.box_shadow.is_none()
        && matches!(node.kind, ComponentKind::Text { .. } | ComponentKind::Row | ComponentKind::Box)
        && node.children.iter().all(|child| passive(child, parent))
}

fn spaces(node: &Component) -> bool {
    match &node.kind {
        ComponentKind::Text { content } => content.chars().all(|ch| matches!(ch, ' ' | '\t' | '\u{200b}')),
        ComponentKind::Row | ComponentKind::Box => !node.children.is_empty() && node.children.iter().all(spaces),
        _ => false,
    }
}

fn preserved(node: &Component) -> bool {
    matches!(&node.kind, ComponentKind::Text { content }
        if node.style.white_space == WhiteSpace::Pre && !content.is_empty())
        || node.children.iter().any(preserved)
}

fn break_after(node: &Component) -> bool {
    match &node.kind {
        ComponentKind::Text { content } => matches!(node.style.white_space, WhiteSpace::Normal | WhiteSpace::PreLine)
            && content.ends_with([' ', '\t', '\u{200b}']),
        ComponentKind::Row | ComponentKind::Box => node.children.last().is_some_and(break_after),
        _ => false,
    }
}

fn shift(layouts: &mut [(LayoutRect, usize)], positions: &HashMap<usize, usize>,
    index: usize, count: usize, dx: f32, dy: f32) {
    for i in index..index + count {
        if let Some(&p) = positions.get(&i) { layouts[p].0.x += dx; layouts[p].0.y += dy; }
    }
}

fn children(node: &Component, index: usize) -> Vec<(&Component, usize)> {
    let mut next = index + 1;
    node.children.iter().map(|child| {
        let index = next; next += count_nodes(child); (child, index)
    }).collect()
}

fn visit(node: &Component, index: usize, layouts: &mut [(LayoutRect, usize)],
    positions: &HashMap<usize, usize>) {
    let children = children(node, index);
    for (child, index) in &children { visit(child, *index, layouts, positions); }
    if node.style.white_space != WhiteSpace::Pre || node.style.direction != TextDirection::Ltr
        || !matches!(node.style.text_align, TextAlign::Start | TextAlign::Left)
        || !node.style.custom_properties.as_ref().is_some_and(|p|
            p.contains_key("--w3cos-internal-inline-formatting-context"))
        || !children.iter().all(|(child, _)| passive(child, &node.style))
    { return; }
    let Some(&parent_position) = positions.get(&index) else { return; };
    let parent = layouts[parent_position].0;
    let left = parent.x + node.style.padding_lengths().left
        + node.style.border_left_width.unwrap_or(node.style.border_width);
    let line = inline_used_line_height(node.style.font_size * node.style.line_height);
    if line <= 0.0 { return; }
    let mut previous = None::<(LayoutRect, &Component)>;
    let mut has_preserved_tail = false;
    let mut moved_tail = false;
    for (source, (child, index)) in children.iter().enumerate() {
        let Some(&position) = positions.get(index) else { continue; };
        let mut rect = layouts[position].0;
        if spaces(child) {
            if let Some((last, _)) = previous
                && has_preserved_tail && rect.y > last.y + 0.01
            {
                // Preserved trailing spaces hang from the current line. A
                // later word's width must not create a separate blank line.
                shift(layouts, positions, *index, count_nodes(child), last.x + last.width - rect.x, last.y - rect.y);
                rect = layouts[position].0;
                moved_tail = true;
            }
            has_preserved_tail |= preserved(child);
        } else {
            if moved_tail && let Some((last, separator)) = previous
                && break_after(separator) && (rect.x - left).abs() < 0.01
                && rect.y > last.y + line + 0.01
            {
                let dy = last.y + line - rect.y;
                for (following, index) in children.iter().skip(source) {
                    shift(layouts, positions, *index, count_nodes(following), 0.0, dy);
                }
                if node.style.height == Dimension::Auto { layouts[parent_position].0.height += dy; }
                rect = layouts[position].0;
            }
            has_preserved_tail = false;
            moved_tail = false;
        }
        previous = Some((rect, child));
    }
}

fn propagate(node: &Component, index: usize, layouts: &mut [(LayoutRect, usize)],
    positions: &HashMap<usize, usize>, original: &HashMap<usize, LayoutRect>) {
    let children = children(node, index);
    for (child, index) in &children { propagate(child, *index, layouts, positions, original); }
    if !matches!(node.style.display, Display::Block | Display::FlowRoot | Display::InlineBlock | Display::ListItem) { return; }
    let mut delta = 0.0;
    let mut bottom = None::<f32>;
    for (child, index) in children {
        if child.style.display == Display::Inline || matches!(child.style.position, Position::Absolute | Position::Fixed)
            || child.style.float != Float::None { continue; }
        let (Some(before), Some(&position)) = (original.get(&index), positions.get(&index)) else { continue; };
        if bottom.is_some_and(|bottom| before.y >= bottom - 0.01) {
            shift(layouts, positions, index, count_nodes(child), 0.0, delta);
        }
        delta += layouts[position].0.height - before.height;
        bottom = Some(before.y + before.height);
    }
    if node.style.height == Dimension::Auto && let Some(&position) = positions.get(&index) {
        layouts[position].0.height += delta;
    }
}

pub(crate) fn project(layouts: &mut [(LayoutRect, usize)], root: &Component) {
    let positions = layouts.iter().enumerate().map(|(p, (_, i))| (*i, p)).collect();
    let original = layouts.iter().map(|(r, i)| (*i, *r)).collect();
    visit(root, 0, layouts, &positions);
    propagate(root, 0, layouts, &positions, &original);
}
