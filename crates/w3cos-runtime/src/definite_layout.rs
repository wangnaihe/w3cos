//! Border boxes for static, definite block / flex / grid trees.
//!
//! Author stylesheets, percentages, floats, abspos, overflow, and text-driven
//! auto heights stay on Taffy. Pages whose used sizes are already px (or a
//! stretch of the containing block) never build a Taffy tree.

use w3cos_dom::DOMRect;
use w3cos_dom::node::{NodeId, NodeType};
use w3cos_std::style::{
    AlignItems, AlignSelf, Dimension, Display, FlexDirection, FlexWrap, Float, JustifyContent,
    Overflow, Position, Spacing, Style, TextDirection,
};

enum Flow {
    Skip,
    Box { width: f32, height: f32 },
}

/// Write border boxes and return true when the whole tree was definite.
pub(crate) fn try_apply() -> bool {
    if w3cos_dom::stylesheet::has_rules() {
        return false;
    }
    let (viewport_w, _, _) = crate::jsdom::viewport();
    let viewport_w = viewport_w as f32;
    if !viewport_w.is_finite() || viewport_w <= 0.0 {
        return false;
    }
    crate::dom::with_document_mut(|document| {
        let root = layout_root(document);
        let mut rects = Vec::new();
        if layout_element(document, root, 0.0, 0.0, viewport_w, &mut rects).is_none() {
            return false;
        }
        for (id, rect) in rects {
            document.set_layout_rect(id, rect);
        }
        true
    })
}

fn layout_root(document: &w3cos_dom::Document) -> NodeId {
    let body = document.body().id;
    if let Some(parent) = document.get_node(body).parent {
        let is_html = document
            .get_node(parent)
            .tag
            .with_str(|name| name.eq_ignore_ascii_case("html"));
        if is_html {
            return parent;
        }
    }
    body
}

fn layout_element(
    document: &w3cos_dom::Document,
    id: NodeId,
    x: f32,
    y: f32,
    cb_width: f32,
    out: &mut Vec<(NodeId, DOMRect)>,
) -> Option<Flow> {
    let style = &document.get_style(id).inner;
    if style.display == Display::None {
        zero_subtree(document, id, out);
        return Some(Flow::Skip);
    }
    if !box_is_simple(style) {
        return None;
    }
    match style.display {
        Display::Block | Display::FlowRoot => layout_block(document, id, x, y, cb_width, out),
        Display::Flex => layout_flex(document, id, x, y, cb_width, out),
        Display::Grid => layout_grid(document, id, x, y, cb_width, out),
        _ => None,
    }
}

fn box_is_simple(style: &Style) -> bool {
    style.position == Position::Static
        && style.float == Float::None
        && style.direction == TextDirection::Ltr
        && style.column_count.is_none()
        && style.gap == 0.0
        && style.row_gap.unwrap_or(0.0) == 0.0
        && style.column_gap.unwrap_or(0.0) == 0.0
        && overflow_is_visible(style.overflow)
        && style.overflow_x.is_none_or(overflow_is_visible)
        && style.overflow_y.is_none_or(overflow_is_visible)
        && spacing_is_zero(style.margin.top)
        && spacing_is_zero(style.margin.right)
        && spacing_is_zero(style.margin.bottom)
        && spacing_is_zero(style.margin.left)
        && spacing_is_zero(style.padding.top)
        && spacing_is_zero(style.padding.right)
        && spacing_is_zero(style.padding.bottom)
        && spacing_is_zero(style.padding.left)
        && style.border_width == 0.0
        && style.border_top_width.unwrap_or(0.0) == 0.0
        && style.border_right_width.unwrap_or(0.0) == 0.0
        && style.border_bottom_width.unwrap_or(0.0) == 0.0
        && style.border_left_width.unwrap_or(0.0) == 0.0
        && matches!(style.min_width, Dimension::Auto)
        && matches!(style.min_height, Dimension::Auto)
        && matches!(style.max_width, Dimension::Auto)
        && matches!(style.max_height, Dimension::Auto)
}

fn overflow_is_visible(overflow: Overflow) -> bool {
    matches!(overflow, Overflow::Visible)
}

fn spacing_is_zero(spacing: Spacing) -> bool {
    matches!(spacing, Spacing::Px(value) if value == 0.0)
}

fn content_width(style: &Style, cb_width: f32) -> Option<f32> {
    match style.width {
        Dimension::Px(width) if width.is_finite() && width >= 0.0 => Some(width),
        Dimension::Auto if cb_width.is_finite() && cb_width >= 0.0 => Some(cb_width),
        _ => None,
    }
}

fn px_size(dimension: Dimension) -> Option<f32> {
    match dimension {
        Dimension::Px(value) if value.is_finite() && value >= 0.0 => Some(value),
        _ => None,
    }
}

fn layout_block(
    document: &w3cos_dom::Document,
    id: NodeId,
    x: f32,
    y: f32,
    cb_width: f32,
    out: &mut Vec<(NodeId, DOMRect)>,
) -> Option<Flow> {
    let style = &document.get_style(id).inner;
    let width = content_width(style, cb_width)?;
    let specified_height = match style.height {
        Dimension::Auto => None,
        other => Some(px_size(other)?),
    };
    let mut cursor = y;
    let mut saw_text = false;
    let mut child = document.get_node(id).first_child;
    while let Some(cid) = child {
        let node = document.get_node(cid);
        match node.node_type {
            NodeType::Element => match layout_element(document, cid, x, cursor, width, out)? {
                Flow::Skip => {}
                Flow::Box { height, .. } => cursor += height,
            },
            NodeType::Text => {
                if node
                    .text_content
                    .as_ref()
                    .is_some_and(|text| !text.is_empty())
                {
                    saw_text = true;
                }
                out.push((cid, DOMRect::zero()));
            }
            _ => out.push((cid, DOMRect::zero())),
        }
        child = w3cos_dom::node::sib(document.get_node(cid).next_sibling);
    }
    if saw_text && specified_height.is_none() {
        return None;
    }
    let height = specified_height.unwrap_or(cursor - y);
    out.push((id, DOMRect::new(x, y, width, height)));
    Some(Flow::Box { width, height })
}

fn layout_flex(
    document: &w3cos_dom::Document,
    id: NodeId,
    x: f32,
    y: f32,
    cb_width: f32,
    out: &mut Vec<(NodeId, DOMRect)>,
) -> Option<Flow> {
    let style = &document.get_style(id).inner;
    if style.flex_direction != FlexDirection::Row
        || !matches!(style.flex_wrap, FlexWrap::NoWrap | FlexWrap::Wrap)
        || style.justify_content != JustifyContent::FlexStart
        || !matches!(
            style.align_items,
            AlignItems::Stretch | AlignItems::FlexStart
        )
    {
        return None;
    }
    let width = content_width(style, cb_width)?;
    let specified_height = match style.height {
        Dimension::Auto => None,
        other => Some(px_size(other)?),
    };
    if specified_height.is_some()
        && style.align_content != w3cos_std::style::AlignContent::FlexStart
    {
        return None;
    }
    let wrap = style.flex_wrap == FlexWrap::Wrap;
    let mut lines: Vec<Vec<(NodeId, f32, f32)>> = vec![Vec::new()];
    let mut line_width = 0.0;
    let mut child = document.get_node(id).first_child;
    while let Some(cid) = child {
        let node = document.get_node(cid);
        match node.node_type {
            NodeType::Element => {
                let child_style = &document.get_style(cid).inner;
                if child_style.display == Display::None {
                    zero_subtree(document, cid, out);
                } else {
                    if child_style.flex_grow != 0.0
                        || child_style.order != 0
                        || child_style.align_self != AlignSelf::Auto
                        || !matches!(child_style.flex_basis, Dimension::Auto)
                    {
                        return None;
                    }
                    let item_width = px_size(child_style.width)?;
                    let item_height = px_size(child_style.height)?;
                    if item_width > width + 0.05 {
                        return None;
                    }
                    if wrap && line_width > 0.0 && line_width + item_width > width + 0.05 {
                        lines.push(Vec::new());
                        line_width = 0.0;
                    } else if !wrap && line_width + item_width > width + 0.05 {
                        return None;
                    }
                    lines
                        .last_mut()
                        .unwrap()
                        .push((cid, item_width, item_height));
                    line_width += item_width;
                }
            }
            NodeType::Text => {
                if node
                    .text_content
                    .as_ref()
                    .is_some_and(|text| !text.is_empty())
                {
                    return None;
                }
                out.push((cid, DOMRect::zero()));
            }
            _ => out.push((cid, DOMRect::zero())),
        }
        child = w3cos_dom::node::sib(document.get_node(cid).next_sibling);
    }
    let mut cursor = y;
    for line in lines {
        let line_height = line.iter().map(|item| item.2).fold(0.0, f32::max);
        let mut item_x = x;
        for (item, item_width, _) in line {
            layout_element(document, item, item_x, cursor, item_width, out)?;
            item_x += item_width;
        }
        cursor += line_height;
    }
    let height = specified_height.unwrap_or(cursor - y);
    out.push((id, DOMRect::new(x, y, width, height)));
    Some(Flow::Box { width, height })
}

fn layout_grid(
    document: &w3cos_dom::Document,
    id: NodeId,
    x: f32,
    y: f32,
    cb_width: f32,
    out: &mut Vec<(NodeId, DOMRect)>,
) -> Option<Flow> {
    let style = &document.get_style(id).inner;
    if !matches!(
        style.align_items,
        AlignItems::Stretch | AlignItems::FlexStart
    ) || !matches!(
        style.justify_items,
        AlignItems::Stretch | AlignItems::FlexStart
    ) || style.justify_content != JustifyContent::FlexStart
    {
        return None;
    }
    let columns = style
        .grid_template_columns
        .as_deref()
        .and_then(parse_equal_fr_columns)?;
    let width = content_width(style, cb_width)?;
    let specified_height = match style.height {
        Dimension::Auto => None,
        other => Some(px_size(other)?),
    };
    if specified_height.is_some()
        && style.align_content != w3cos_std::style::AlignContent::FlexStart
    {
        return None;
    }
    let column_width = width / columns as f32;
    if !column_width.is_finite() {
        return None;
    }
    let mut items = Vec::new();
    let mut child = document.get_node(id).first_child;
    while let Some(cid) = child {
        let node = document.get_node(cid);
        match node.node_type {
            NodeType::Element => {
                let child_style = &document.get_style(cid).inner;
                if child_style.display == Display::None {
                    zero_subtree(document, cid, out);
                } else {
                    if child_style.grid_column.is_some()
                        || child_style.flex_grow != 0.0
                        || child_style.order != 0
                        || child_style.align_self != AlignSelf::Auto
                    {
                        return None;
                    }
                    let item_height = px_size(child_style.height)?;
                    if !matches!(child_style.width, Dimension::Auto | Dimension::Px(_)) {
                        return None;
                    }
                    if matches!(child_style.width, Dimension::Auto)
                        && style.justify_items != AlignItems::Stretch
                    {
                        return None;
                    }
                    items.push((cid, item_height));
                }
            }
            NodeType::Text => {
                if node
                    .text_content
                    .as_ref()
                    .is_some_and(|text| !text.is_empty())
                {
                    return None;
                }
                out.push((cid, DOMRect::zero()));
            }
            _ => out.push((cid, DOMRect::zero())),
        }
        child = w3cos_dom::node::sib(document.get_node(cid).next_sibling);
    }
    let rows = items.len().div_ceil(columns);
    let mut row_heights: Vec<f32> = vec![0.0; rows];
    for (index, (_, height)) in items.iter().enumerate() {
        let row = index / columns;
        row_heights[row] = row_heights[row].max(*height);
    }
    let mut row_y = Vec::with_capacity(rows);
    let mut cursor = y;
    for height in &row_heights {
        row_y.push(cursor);
        cursor += *height;
    }
    for (index, (item, _)) in items.iter().enumerate() {
        let column = index % columns;
        let row = index / columns;
        let item_x = x + column as f32 * column_width;
        layout_element(document, *item, item_x, row_y[row], column_width, out)?;
    }
    let height = specified_height.unwrap_or(cursor - y);
    out.push((id, DOMRect::new(x, y, width, height)));
    Some(Flow::Box { width, height })
}

fn parse_equal_fr_columns(spec: &str) -> Option<usize> {
    let spec = spec.trim();
    let rest = spec.strip_prefix("repeat(")?.strip_suffix(')')?;
    let (count, track) = rest.split_once(',')?;
    let count: usize = count.trim().parse().ok()?;
    if count == 0 || count > 4096 {
        return None;
    }
    track.trim().eq_ignore_ascii_case("1fr").then_some(count)
}

fn zero_subtree(document: &w3cos_dom::Document, id: NodeId, out: &mut Vec<(NodeId, DOMRect)>) {
    out.push((id, DOMRect::zero()));
    let mut child = document.get_node(id).first_child;
    while let Some(cid) = child {
        zero_subtree(document, cid, out);
        child = w3cos_dom::node::sib(document.get_node(cid).next_sibling);
    }
}
