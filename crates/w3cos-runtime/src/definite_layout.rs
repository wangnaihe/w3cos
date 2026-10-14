//! Border boxes for static, definite block / flex / grid trees.
//!
//! Px and stretch trees with no author rules never build a Taffy tree.
//! Simple author blocks (percent widths, one text run per box) are solved
//! the same way and remembered by inline style. Column flex of definite
//! rows, definite overflow boxes, and absolute children of a relative
//! parent are stacked here. Other flex, grid, float, and indefinite
//! overflow trees stay on Taffy.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use w3cos_dom::DOMRect;
use w3cos_dom::node::{NodeId, NodeType};
use w3cos_std::style::{
    AlignItems, AlignSelf, Dimension, Display, FlexDirection, FlexWrap, Float, JustifyContent,
    Overflow, Position, Spacing, Style, TextDirection, WhiteSpace,
};

enum Flow {
    Skip,
    Box { width: f32, height: f32 },
}

/// Write border boxes and return true when the whole tree was definite.
pub(crate) fn try_apply() -> bool {
    let (viewport_w, _, _) = crate::jsdom::viewport();
    let viewport_w = viewport_w as f32;
    if !viewport_w.is_finite() || viewport_w <= 0.0 {
        return false;
    }
    if w3cos_dom::stylesheet::has_rules() {
        return try_apply_authored(viewport_w);
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
    position_is_in_flow(style)
        && style.float == Float::None
        && style.direction == TextDirection::Ltr
        && style.column_count.is_none()
        && style.gap == 0.0
        && style.row_gap.unwrap_or(0.0) == 0.0
        && style.column_gap.unwrap_or(0.0) == 0.0
        && overflow_is_definite(style)
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

/// Static boxes, and relative boxes that are not offset, keep their in-flow
/// border box. Relative is the containing block for definite absolute children.
fn position_is_in_flow(style: &Style) -> bool {
    match style.position {
        Position::Static => true,
        Position::Relative => {
            matches!(style.top, Dimension::Auto)
                && matches!(style.right, Dimension::Auto)
                && matches!(style.bottom, Dimension::Auto)
                && matches!(style.left, Dimension::Auto)
        }
        _ => false,
    }
}

/// Overflow does not change a definite px border box. Auto height still
/// depends on the visible overflow model, so that stays on Taffy.
fn overflow_is_definite(style: &Style) -> bool {
    let visible = overflow_is_visible(style.overflow)
        && style.overflow_x.is_none_or(overflow_is_visible)
        && style.overflow_y.is_none_or(overflow_is_visible);
    visible || (px_size(style.width).is_some() && px_size(style.height).is_some())
}

enum AbsoluteBox {
    Flow,
    Definite {
        left: f32,
        top: f32,
        width: f32,
        height: f32,
    },
    Unsupported,
}

fn classify_absolute(style: &Style) -> AbsoluteBox {
    if !matches!(style.position, Position::Absolute | Position::Fixed) {
        return AbsoluteBox::Flow;
    }
    if style.position == Position::Fixed || !box_is_simple_ignoring_position(style) {
        return AbsoluteBox::Unsupported;
    }
    match (
        px_size(style.left),
        px_size(style.top),
        px_size(style.width),
        px_size(style.height),
    ) {
        (Some(left), Some(top), Some(width), Some(height)) => AbsoluteBox::Definite {
            left,
            top,
            width,
            height,
        },
        _ => AbsoluteBox::Unsupported,
    }
}

fn box_is_simple_ignoring_position(style: &Style) -> bool {
    style.float == Float::None
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
    let positioned = style.position == Position::Relative;
    if let Some(flow) = layout_repeated_text_rows(document, id, x, y, width, specified_height, out)
    {
        return Some(flow);
    }
    let mut cursor = y;
    let mut saw_text = false;
    let mut child = document.get_node(id).first_child;
    while let Some(cid) = child {
        let node = document.get_node(cid);
        match node.node_type {
            NodeType::Element => {
                match classify_absolute(&document.get_style(cid).inner) {
                    AbsoluteBox::Flow => {}
                    AbsoluteBox::Unsupported => return None,
                    AbsoluteBox::Definite {
                        left,
                        top,
                        width: box_width,
                        height: box_height,
                    } => {
                        // Only a relative parent is the containing block here.
                        // A static overflow box keeps absolute descendants on Taffy.
                        if !positioned {
                            return None;
                        }
                        out.push((cid, DOMRect::new(x + left, y + top, box_width, box_height)));
                        let mut nested = document.get_node(cid).first_child;
                        while let Some(nested_id) = nested {
                            zero_subtree(document, nested_id, out);
                            nested =
                                w3cos_dom::node::sib(document.get_node(nested_id).next_sibling);
                        }
                        child = w3cos_dom::node::sib(document.get_node(cid).next_sibling);
                        continue;
                    }
                }
                match layout_element(document, cid, x, cursor, width, out)? {
                    Flow::Skip => {}
                    Flow::Box { height, .. } => cursor += height,
                }
            }
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

/// Identical simple blocks that each carry one text run and a definite px
/// height. The border box is the specified height, so glyph measurement and
/// a per-node Taffy box are unnecessary.
fn layout_repeated_text_rows(
    document: &w3cos_dom::Document,
    id: NodeId,
    x: f32,
    y: f32,
    width: f32,
    specified_height: Option<f32>,
    out: &mut Vec<(NodeId, DOMRect)>,
) -> Option<Flow> {
    let first = document.get_node(id).first_child?;
    let first_node = document.get_node(first);
    if first_node.node_type != NodeType::Element {
        return None;
    }
    let row_style = document.get_style(first);
    let row_ptr = row_style as *const _;
    let row_height = px_size(row_style.inner.height)?;
    let row_width = content_width(&row_style.inner, width)?;
    if !box_is_simple(&row_style.inner)
        || !matches!(row_style.inner.display, Display::Block | Display::FlowRoot)
    {
        return None;
    }
    // Validate every sibling before writing. A mismatch must leave `out`
    // untouched so the ordinary block walk can still place the tree.
    let mut count = 0u32;
    let mut child = Some(first);
    while let Some(cid) = child {
        let node = document.get_node(cid);
        if node.node_type != NodeType::Element {
            return None;
        }
        if (document.get_style(cid) as *const _) != row_ptr {
            return None;
        }
        let Some(text) = node.first_child else {
            return None;
        };
        let text_node = document.get_node(text);
        if text_node.node_type != NodeType::Text || text_node.next_sibling != 0 {
            return None;
        }
        child = w3cos_dom::node::sib(node.next_sibling);
        count += 1;
    }
    if count == 0 {
        return None;
    }
    let mut cursor = y;
    child = Some(first);
    while let Some(cid) = child {
        let node = document.get_node(cid);
        let text = node.first_child.expect("validated text child");
        out.push((cid, DOMRect::new(x, cursor, row_width, row_height)));
        out.push((text, DOMRect::zero()));
        cursor += row_height;
        child = w3cos_dom::node::sib(node.next_sibling);
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
    if style.flex_direction == FlexDirection::Column {
        return layout_flex_column(document, id, x, y, cb_width, out);
    }
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

/// Column flex of definite-height rows. Items with `flex-grow`/`flex-shrink`
/// keep their base size when the container height is auto, because there is
/// no free space to distribute. A definite container that would grow or
/// shrink items stays on Taffy.
fn layout_flex_column(
    document: &w3cos_dom::Document,
    id: NodeId,
    x: f32,
    y: f32,
    cb_width: f32,
    out: &mut Vec<(NodeId, DOMRect)>,
) -> Option<Flow> {
    let (width, specified_height, stretch) = {
        let style = &document.get_style(id).inner;
        if style.flex_wrap != FlexWrap::NoWrap
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
        (
            width,
            specified_height,
            style.align_items == AlignItems::Stretch,
        )
    };
    struct Row {
        id: NodeId,
        height: f32,
        width: f32,
        text: Option<NodeId>,
    }
    let mut rows = Vec::new();
    let mut child = document.get_node(id).first_child;
    while let Some(cid) = child {
        let node = document.get_node(cid);
        match node.node_type {
            NodeType::Element => {
                let child_style = &document.get_style(cid).inner;
                if child_style.display == Display::None {
                    rows.push(None);
                } else {
                    if child_style.order != 0
                        || child_style.align_self != AlignSelf::Auto
                        || !matches!(child_style.flex_basis, Dimension::Auto | Dimension::Px(_))
                    {
                        return None;
                    }
                    if specified_height.is_some()
                        && (child_style.flex_grow != 0.0 || child_style.flex_shrink != 0.0)
                    {
                        return None;
                    }
                    if !box_is_simple(child_style)
                        || !matches!(child_style.display, Display::Block | Display::FlowRoot)
                    {
                        return None;
                    }
                    let item_height = px_size(child_style.height)?;
                    if let Dimension::Px(basis) = child_style.flex_basis {
                        if (basis - item_height).abs() > 0.05 {
                            return None;
                        }
                    }
                    let item_width = match child_style.width {
                        Dimension::Px(item_width)
                            if item_width.is_finite() && item_width >= 0.0 =>
                        {
                            item_width
                        }
                        Dimension::Auto if stretch => width,
                        _ => return None,
                    };
                    let text = match node.first_child {
                        None => None,
                        Some(text) => {
                            let text_node = document.get_node(text);
                            if text_node.node_type != NodeType::Text || text_node.next_sibling != 0
                            {
                                return None;
                            }
                            Some(text)
                        }
                    };
                    rows.push(Some(Row {
                        id: cid,
                        height: item_height,
                        width: item_width,
                        text,
                    }));
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
                rows.push(None);
            }
            _ => rows.push(None),
        }
        child = w3cos_dom::node::sib(document.get_node(cid).next_sibling);
    }
    let mut cursor = y;
    child = document.get_node(id).first_child;
    let mut index = 0;
    while let Some(cid) = child {
        match rows.get(index) {
            Some(None) => {
                if document.get_node(cid).node_type == NodeType::Element {
                    zero_subtree(document, cid, out);
                } else {
                    out.push((cid, DOMRect::zero()));
                }
            }
            Some(Some(row)) => {
                out.push((row.id, DOMRect::new(x, cursor, row.width, row.height)));
                if let Some(text) = row.text {
                    out.push((text, DOMRect::zero()));
                }
                cursor += row.height;
            }
            None => return None,
        }
        index += 1;
        child = w3cos_dom::node::sib(document.get_node(cid).next_sibling);
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

struct AuthoredMemo {
    key: u64,
    rects: Vec<(NodeId, DOMRect)>,
}

struct Placed {
    border_height: f32,
    margin_top: f32,
    margin_bottom: f32,
}

thread_local! {
    static AUTHORED: RefCell<Vec<AuthoredMemo>> = const { RefCell::new(Vec::new()) };
}

pub(crate) fn invalidate() {
    AUTHORED.with(|slot| slot.borrow_mut().clear());
}

/// Author blocks whose used widths are px, percent, or stretch, and whose
/// auto heights are child sums or one measured text run. Anything else
/// returns false so Taffy still owns flex, grid, floats, and inline layout.
fn try_apply_authored(viewport_w: f32) -> bool {
    crate::dom::with_document_mut(|document| {
        let key = authored_fingerprint(document);
        let remembered = AUTHORED.with(|slot| {
            slot.borrow()
                .iter()
                .find(|memo| memo.key == key)
                .map(|memo| memo.rects.clone())
        });
        if let Some(rects) = remembered {
            for (id, rect) in rects {
                document.set_layout_rect(id, rect);
            }
            return true;
        }
        let root = layout_root(document);
        let mut rects = Vec::new();
        let mut walk = AuthorWalk {
            reuse: !w3cos_dom::stylesheet::has_position_dependent_rules(),
            styles: HashMap::new(),
        };
        if layout_authored(
            document, root, 0.0, 0.0, viewport_w, None, 0, &mut walk, &mut rects,
        )
        .is_none()
        {
            return false;
        }
        for (id, rect) in &rects {
            document.set_layout_rect(*id, *rect);
        }
        AUTHORED.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.len() >= 4 {
                slot.remove(0);
            }
            slot.push(AuthoredMemo { key, rects });
        });
        true
    })
}

fn authored_fingerprint(document: &w3cos_dom::Document) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    document.structure_generation().hash(&mut hasher);
    document.cascade_generation().hash(&mut hasher);
    w3cos_dom::stylesheet::generation().hash(&mut hasher);
    hash_authored_node(document, layout_root(document), &mut hasher);
    hasher.finish()
}

fn hash_authored_node(document: &w3cos_dom::Document, id: NodeId, hasher: &mut impl Hasher) {
    let (node_type, first) = {
        let node = document.get_node(id);
        (node.node_type, node.first_child)
    };
    node_type.as_u16().hash(hasher);
    id.as_u32().hash(hasher);
    if node_type == NodeType::Element {
        document.get_node(id).tag.with_str(|tag| tag.hash(hasher));
        for (name, value) in &document.get_style(id).inline_declarations {
            name.hash(hasher);
            value.hash(hasher);
        }
    } else if let Some(text) = document.get_node(id).text_content.as_deref() {
        text.hash(hasher);
    }
    let mut child = first;
    while let Some(cid) = child {
        hash_authored_node(document, cid, hasher);
        child = w3cos_dom::node::sib(document.get_node(cid).next_sibling);
    }
}

struct AuthorWalk {
    /// Safe when no rule distinguishes elements by sibling index or descendants.
    reuse: bool,
    styles: HashMap<u64, Style>,
}

fn cascade_signature(document: &w3cos_dom::Document, id: NodeId) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let (node_type, html, tag_id, classes, namespaced) = {
        let node = document.get_node(id);
        let mut class_ids = Vec::new();
        for class in node.class_list.iter() {
            class_ids.push(class.as_u32());
        }
        (
            node.node_type,
            node.is_html_element,
            node.tag.as_u32(),
            class_ids,
            !node.attribute_namespaces.is_empty(),
        )
    };
    node_type.as_u16().hash(&mut hasher);
    html.hash(&mut hasher);
    tag_id.hash(&mut hasher);
    classes.hash(&mut hasher);
    if namespaced {
        // Namespaced attributes are rare and not part of the shared signature.
        id.as_u32().hash(&mut hasher);
    }
    for &(name, value) in document.get_node(id).attributes.iter() {
        name.as_u32().hash(&mut hasher);
        value.as_str().hash(&mut hasher);
    }
    if node_type == NodeType::Element {
        for (name, value) in &document.get_style(id).inline_declarations {
            name.hash(&mut hasher);
            value.hash(&mut hasher);
        }
    }
    hasher.finish()
}

fn layout_authored(
    document: &w3cos_dom::Document,
    id: NodeId,
    x: f32,
    y: f32,
    cb_width: f32,
    inherited: Option<&Style>,
    chain: u64,
    walk: &mut AuthorWalk,
    out: &mut Vec<(NodeId, DOMRect)>,
) -> Option<Placed> {
    let child_chain = if walk.reuse {
        chain
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(cascade_signature(document, id))
    } else {
        0
    };
    let style = if walk.reuse {
        if let Some(style) = walk.styles.get(&child_chain) {
            style.clone()
        } else {
            let style = document.computed_style_inherited(id, inherited);
            walk.styles.insert(child_chain, style.clone());
            style
        }
    } else {
        document.computed_style_inherited(id, inherited)
    };
    if style.display == Display::None {
        zero_subtree(document, id, out);
        return Some(Placed {
            border_height: 0.0,
            margin_top: 0.0,
            margin_bottom: 0.0,
        });
    }
    if !matches!(style.display, Display::Block | Display::FlowRoot) || !authored_box_ok(&style) {
        return None;
    }
    let margin_top = spacing_px(style.margin.top)?;
    let margin_right = spacing_px(style.margin.right)?;
    let margin_bottom = spacing_px(style.margin.bottom)?;
    let margin_left = spacing_px(style.margin.left)?;
    let root_margin = document
        .get_node(id)
        .tag
        .with_str(|name| name.eq_ignore_ascii_case("html") || name.eq_ignore_ascii_case("body"));
    if (margin_top != 0.0 || margin_right != 0.0 || margin_bottom != 0.0 || margin_left != 0.0)
        && !root_margin
    {
        return None;
    }
    let width = match style.width {
        Dimension::Px(width) if width.is_finite() && width >= 0.0 => width,
        Dimension::Percent(percent) if percent.is_finite() && percent >= 0.0 => {
            cb_width * percent / 100.0
        }
        Dimension::Auto if cb_width.is_finite() && cb_width >= margin_left + margin_right => {
            (cb_width - margin_left - margin_right).max(0.0)
        }
        _ => return None,
    };
    if !width.is_finite() {
        return None;
    }
    let specified_height = match style.height {
        Dimension::Auto => None,
        other => Some(px_size(other)?),
    };
    let border_x = x + margin_left;
    let border_y = y + margin_top;
    let mut cursor = border_y;
    let mut child = document.get_node(id).first_child;
    while let Some(cid) = child {
        let (node_type, next) = {
            let node = document.get_node(cid);
            (node.node_type, w3cos_dom::node::sib(node.next_sibling))
        };
        match node_type {
            NodeType::Element => {
                let placed = layout_authored(
                    document,
                    cid,
                    border_x,
                    cursor,
                    width,
                    Some(&style),
                    child_chain,
                    walk,
                    out,
                )?;
                cursor += placed.margin_top + placed.border_height + placed.margin_bottom;
            }
            NodeType::Text => {
                let text = document
                    .get_node(cid)
                    .text_content
                    .as_deref()
                    .unwrap_or("")
                    .to_string();
                if text.is_empty() || text.chars().all(char::is_whitespace) {
                    out.push((cid, DOMRect::zero()));
                } else if specified_height.is_some() {
                    out.push((cid, DOMRect::zero()));
                } else if style.white_space != WhiteSpace::Normal {
                    return None;
                } else {
                    let height = crate::layout::block_text_border_height(&text, width, &style);
                    if !height.is_finite() || height < 0.0 {
                        return None;
                    }
                    out.push((cid, DOMRect::new(border_x, cursor, width, height)));
                    cursor += height;
                }
            }
            _ => out.push((cid, DOMRect::zero())),
        }
        child = next;
    }
    let border_height = specified_height.unwrap_or(cursor - border_y);
    out.push((id, DOMRect::new(border_x, border_y, width, border_height)));
    Some(Placed {
        border_height,
        margin_top,
        margin_bottom,
    })
}

fn authored_box_ok(style: &Style) -> bool {
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

fn spacing_px(spacing: Spacing) -> Option<f32> {
    match spacing {
        Spacing::Px(value) if value.is_finite() && value >= 0.0 => Some(value),
        _ => None,
    }
}

fn zero_subtree(document: &w3cos_dom::Document, id: NodeId, out: &mut Vec<(NodeId, DOMRect)>) {
    out.push((id, DOMRect::zero()));
    let mut child = document.get_node(id).first_child;
    while let Some(cid) = child {
        zero_subtree(document, cid, out);
        child = w3cos_dom::node::sib(document.get_node(cid).next_sibling);
    }
}
