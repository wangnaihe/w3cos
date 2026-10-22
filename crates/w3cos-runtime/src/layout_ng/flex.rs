//! Flex formatting context, stage S5: one nowrap line.
//!
//! Structure follows Blink's NGFlexLayoutAlgorithm
//! (third_party/blink/renderer/core/layout/flex/flex_layout_algorithm.cc,
//! Copyright The Chromium Authors, BSD-style license) and CSS Flexible Box
//! Layout Level 1 §4.5 (automatic minimum size) and §9 (main size, flexible
//! lengths, alignment). Wrapped lines, baseline alignment, inline-flex and
//! an indefinite percentage basis stay on the legacy engine.
//!
//! A flex row the DOM lowered from an inline formatting context carries the
//! IFC marker and is not a flex container; the block algorithm lays it out
//! as an IFC. Synthetic float groups are not flex containers either.

use super::Why;
use super::block::{self, BoxMetrics, MinMax};
use super::inline::rendered_children;
use super::oof;
use super::{BoxFragment, ConstraintSpace, LastBaseline, LayoutResult, MarginStrut};
use crate::layout::{FlatNodeInfo, resolve_spacing_for_layout};
use w3cos_std::style::used_box_spacing;
use w3cos_std::style::{
    AlignContent, AlignItems, AlignSelf, BoxSizing, Dimension, FlexDirection, FlexWrap, Float,
    JustifyContent, Spacing, Style, TextDirection,
};

const EPS: f32 = 1.0 / 128.0;

fn unit(v: f32) -> f32 {
    used_box_spacing(v)
}

fn has_marker(style: &Style, key: &str) -> bool {
    style
        .custom_properties
        .as_ref()
        .is_some_and(|props| props.contains_key(key))
}

fn row_axis(direction: FlexDirection) -> bool {
    matches!(direction, FlexDirection::Row | FlexDirection::RowReverse)
}

fn reverse_axis(direction: FlexDirection) -> bool {
    matches!(
        direction,
        FlexDirection::RowReverse | FlexDirection::ColumnReverse
    )
}

/// Why this author flex container is outside S5. `None` when the nowrap
/// line can be laid out. The IFC marker is not a flex container.
pub(super) fn container_reason(flat: &[FlatNodeInfo], index: usize) -> Option<&'static str> {
    let style = flat[index].style;
    // `display:flex` on the document element (the root flex item is the
    // body, with the UA margin). That pair stays on the legacy engine.
    if has_marker(style, "--w3cos-internal-document-element") {
        return Some("flex:root");
    }
    if has_marker(style, "--w3cos-internal-size-containment") {
        return Some("flex:contain");
    }
    if has_marker(style, "--w3cos-internal-width-keyword") {
        return Some("flex:width-keyword");
    }
    if style.flex_wrap != FlexWrap::NoWrap {
        return Some("flex:wrap");
    }
    if style.align_items == AlignItems::Baseline {
        return Some("flex:baseline");
    }
    if style.direction == TextDirection::Rtl {
        return Some("flex:rtl");
    }
    if !matches!(
        style.align_content,
        AlignContent::Stretch | AlignContent::FlexStart
    ) {
        return Some("flex:align-content");
    }
    let row = row_axis(style.flex_direction);
    // Column cross alignment other than stretch (fit-content width) is
    // still the legacy engine's. Stretch is the definite cross size.
    if !row && style.align_items != AlignItems::Stretch {
        return Some("flex:column-align");
    }
    for child in rendered_children(flat, index) {
        let child_style = flat[child].style;
        if oof::is_out_of_flow(child_style) {
            return Some("flex:oof");
        }
        if child_style.float != Float::None {
            return Some("flex:float");
        }
        if child_style.align_self == AlignSelf::Baseline {
            return Some("flex:baseline");
        }
        // A text run is an anonymous flex item. Its line breaking and
        // white-space handling stay on the legacy engine (a real flex
        // reference next to a fallen-back test must use the same engine).
        if matches!(flat[child].kind, w3cos_std::ComponentKind::Text { .. }) {
            return Some("flex:text-item");
        }
        if subtree_has_float(flat, child) {
            return Some("flex:float-descendant");
        }
        // Column cross alignment other than stretch, including align-self,
        // is still legacy (fit-content width and inline content offsets).
        if !row && used_align(style.align_items, child_style.align_self) != AlignItems::Stretch {
            return Some("flex:column-align-self");
        }
        // Percentage main sizes and percentage block descendants cycle
        // through the automatic minimum. Leave those columns on legacy.
        if !row && column_percent_size(flat, child) {
            return Some("flex:column-percent");
        }
        let (cross_start, cross_end) = cross_margin_sides(child_style, style.flex_direction);
        if matches!(cross_start, Spacing::Auto) || matches!(cross_end, Spacing::Auto) {
            return Some("flex:auto-cross-margin");
        }
    }
    None
}

fn subtree_has_float(flat: &[FlatNodeInfo], index: usize) -> bool {
    let end = flat[index].subtree_end;
    let mut child = index + 1;
    while child < end {
        if flat[child].style.display == w3cos_std::style::Display::None {
            child = flat[child].subtree_end.max(child + 1);
            continue;
        }
        if flat[child].style.float != Float::None {
            return true;
        }
        child += 1;
    }
    false
}

/// A percentage block size on this column item or a descendant. The
/// flex base, the used height, and min/max height all count.
fn column_percent_size(flat: &[FlatNodeInfo], index: usize) -> bool {
    let end = flat[index].subtree_end.max(index + 1);
    let mut child = index;
    while child < end {
        if flat[child].style.display == w3cos_std::style::Display::None {
            child = flat[child].subtree_end.max(child + 1);
            continue;
        }
        if percent_block_size(flat[child].style) {
            return true;
        }
        child += 1;
    }
    false
}

fn percent_block_size(style: &Style) -> bool {
    // `flex-basis: 0%` is a definite zero (`flex: 1`), not a cyclic
    // percentage. A non-zero percentage basis or a percentage block size
    // still cycles through the automatic minimum.
    let basis = match style.flex_basis {
        Dimension::Percent(value) => value.abs() > EPS,
        _ => false,
    };
    basis
        || matches!(style.height, Dimension::Percent(_))
        || matches!(style.min_height, Dimension::Percent(_))
        || matches!(style.max_height, Dimension::Percent(_))
}

struct Item {
    index: usize,
    order: i32,
    grow: f32,
    shrink: f32,
    /// Content-box flex base size, before min/max.
    base: f32,
    min: f32,
    max: f32,
    /// Content-box used main size.
    content: f32,
    main_edges: f32,
    cross_edges: f32,
    margin_main_start: f32,
    margin_main_end: f32,
    margin_cross_start: f32,
    margin_cross_end: f32,
    auto_main_start: bool,
    auto_main_end: bool,
    /// Content-box cross size when the property (or a later stretch) is definite.
    cross: Option<f32>,
    /// The cross size property was `auto` (stretch may assign it).
    cross_auto: bool,
    align: AlignItems,
    frozen: bool,
}

impl Item {
    fn outer_main(&self) -> f32 {
        outer_at(self, self.content)
    }

    fn margin_box_cross(&self) -> f32 {
        self.margin_cross_start
            + self.margin_cross_end
            + self.cross_edges
            + self.cross.unwrap_or(0.0)
    }
}

fn outer_at(item: &Item, content: f32) -> f32 {
    item.margin_main_start + item.margin_main_end + item.main_edges + content
}

fn to_content(style: &Style, edges: f32, size: f32) -> f32 {
    match style.box_sizing {
        BoxSizing::BorderBox => (size - edges).max(0.0),
        BoxSizing::ContentBox => size.max(0.0),
    }
}

/// `Some(None)` is an automatic size. `None` is a percentage with no
/// definite basis (the caller falls back).
fn definite(
    dim: Dimension,
    basis: Option<f32>,
    style: &Style,
    space: &ConstraintSpace,
) -> Option<Option<f32>> {
    match dim {
        Dimension::Auto => Some(None),
        Dimension::Percent(_) if basis.is_none() => None,
        other => Some(block::resolve_length(other, basis, style, space)),
    }
}

fn spacing_px(style: &Style, spacing: Spacing, cb: f32, space: &ConstraintSpace) -> f32 {
    match spacing {
        Spacing::Auto => 0.0,
        other => resolve_spacing_for_layout(other, cb, style, space.viewport_w, space.viewport_h),
    }
}

fn main_margin_sides(style: &Style, direction: FlexDirection) -> (Spacing, Spacing) {
    match direction {
        FlexDirection::Row => (style.margin.left, style.margin.right),
        FlexDirection::RowReverse => (style.margin.right, style.margin.left),
        FlexDirection::Column => (style.margin.top, style.margin.bottom),
        FlexDirection::ColumnReverse => (style.margin.bottom, style.margin.top),
    }
}

fn cross_margin_sides(style: &Style, direction: FlexDirection) -> (Spacing, Spacing) {
    if row_axis(direction) {
        (style.margin.top, style.margin.bottom)
    } else {
        (style.margin.left, style.margin.right)
    }
}

fn used_align(container: AlignItems, item: AlignSelf) -> AlignItems {
    match item {
        AlignSelf::Auto => container,
        AlignSelf::FlexStart => AlignItems::FlexStart,
        AlignSelf::FlexEnd => AlignItems::FlexEnd,
        AlignSelf::Center => AlignItems::Center,
        AlignSelf::Stretch => AlignItems::Stretch,
        AlignSelf::Baseline => AlignItems::FlexStart,
    }
}

/// Content min/max of the item's contents, ignoring the item's own main
/// size (CSS Flexbox §4.5 content size suggestion). A child's definite
/// width is its min-content contribution; `intrinsic_sizes` only sees
/// descendants, so an empty sized box would otherwise contribute 0.
fn content_minmax(flat: &[FlatNodeInfo], index: usize, space: &ConstraintSpace) -> Option<MinMax> {
    let info = &flat[index];
    if block::size_contained(info.style) {
        return Some(MinMax::exact(block::contain_intrinsic_size(info.style).0));
    }
    if let w3cos_std::ComponentKind::Text { content } = info.kind {
        return Some(super::inline::text_min_max(content, info.style));
    }
    let mut sizes = MinMax::exact(0.0);
    let mut any = false;
    for child in rendered_children(flat, index) {
        if oof::is_out_of_flow(flat[child].style) || flat[child].style.float != Float::None {
            continue;
        }
        any = true;
        sizes.encompass(inline_contribution(flat, child, space).why("flex:intrinsic")?);
    }
    if !any {
        return Some(MinMax::exact(0.0));
    }
    Some(sizes)
}

/// Margin-box inline contribution of one in-flow child.
fn inline_contribution(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
) -> Option<MinMax> {
    let style = flat[index].style;
    let border = block::border_widths(style);
    let pad = |spacing| spacing_px(style, spacing, 0.0, space);
    let edges = border[1] + border[3] + pad(style.padding.left) + pad(style.padding.right);
    let margin = spacing_px(style, style.margin.left, 0.0, space)
        + spacing_px(style, style.margin.right, 0.0, space);
    let sizes = match definite(style.width, None, style, space).why("flex:percent")? {
        Some(size) => MinMax::exact(to_content(style, edges, size) + edges),
        None => {
            let inner = block::intrinsic_sizes(flat, index, space).why("flex:intrinsic")?;
            inner.map(|size| size + edges)
        }
    };
    Some(sizes.map(|size| size + margin))
}

/// Min-content block size of `index`'s content (column main axis).
/// Definite heights sum; auto heights recurse into their content.
fn block_content_min(flat: &[FlatNodeInfo], index: usize, space: &ConstraintSpace) -> Option<f32> {
    let info = &flat[index];
    if block::size_contained(info.style) {
        return Some(block::contain_intrinsic_size(info.style).1);
    }
    if let w3cos_std::ComponentKind::Text { .. } = info.kind {
        let line = info.style.font_size * info.style.line_height.max(1.0);
        return Some(line.max(0.0));
    }
    let mut sum = 0.0;
    for child in rendered_children(flat, index) {
        if oof::is_out_of_flow(flat[child].style) || flat[child].style.float != Float::None {
            continue;
        }
        sum += block_contribution(flat, child, space).why("flex:block-min")?;
    }
    Some(sum.max(0.0))
}

fn block_contribution(flat: &[FlatNodeInfo], index: usize, space: &ConstraintSpace) -> Option<f32> {
    let style = flat[index].style;
    let border = block::border_widths(style);
    let pad = |spacing| spacing_px(style, spacing, 0.0, space);
    let edges = border[0] + border[2] + pad(style.padding.top) + pad(style.padding.bottom);
    let margin = spacing_px(style, style.margin.top, 0.0, space)
        + spacing_px(style, style.margin.bottom, 0.0, space);
    let inner = match definite(style.height, None, style, space).why("flex:percent")? {
        Some(size) => to_content(style, edges, size),
        None => block_content_min(flat, index, space).why("flex:block-min")?,
    };
    Some((inner + edges + margin).max(0.0))
}

fn resolve_main_limits(
    style: &Style,
    row: bool,
    edges: f32,
    inner: Option<f32>,
    space: &ConstraintSpace,
    flat: &[FlatNodeInfo],
    index: usize,
) -> Option<(f32, f32)> {
    let (min_dim, max_dim, specified) = if row {
        (style.min_width, style.max_width, style.width)
    } else {
        (style.min_height, style.max_height, style.height)
    };
    let min = if matches!(min_dim, Dimension::Auto) {
        let content_min = if row {
            let content = content_minmax(flat, index, space).why("flex:intrinsic")?;
            content.min.unwrap_or(content.max)
        } else if block::size_contained(style) {
            block::contain_intrinsic_size(style).1
        } else {
            block_content_min(flat, index, space).why("flex:block-min")?
        }
        .max(0.0);
        match definite(specified, inner, style, space).why("flex:percent")? {
            Some(size) => content_min.min(to_content(style, edges, size)),
            None => content_min,
        }
    } else {
        match definite(min_dim, inner, style, space).why("flex:percent")? {
            Some(size) => to_content(style, edges, size),
            None => 0.0,
        }
    };
    let max = match definite(max_dim, inner, style, space).why("flex:percent")? {
        Some(size) => to_content(style, edges, size).max(min),
        None => f32::INFINITY,
    };
    Some((min.max(0.0), max))
}

fn flex_base(
    style: &Style,
    row: bool,
    edges: f32,
    inner: Option<f32>,
    space: &ConstraintSpace,
    flat: &[FlatNodeInfo],
    index: usize,
    measured: Option<f32>,
) -> Option<f32> {
    let property = if row { style.width } else { style.height };
    let basis = match style.flex_basis {
        Dimension::Auto => property,
        other => other,
    };
    match definite(basis, inner, style, space).why("flex:percent")? {
        Some(size) => Some(to_content(style, edges, size)),
        None => {
            if row {
                content_minmax(flat, index, space).map(|sizes| sizes.max.max(0.0))
            } else {
                measured
            }
        }
    }
}

fn scaled_factor(growing: bool, item: &Item) -> f32 {
    if growing {
        item.grow
    } else {
        // CSS Flexbox §9.7: flex shrink factor × flex base size.
        item.shrink * item.base.max(0.0)
    }
}

/// CSS Flexbox §9.7.4. `content` starts as the hypothetical main size
/// (the base clamped by min/max). Targets are resolved from the flex base
/// size, and the free-space sign is locked to the initial one.
fn distribute(items: &mut [Item], inner: f32, gap_sum: f32) {
    if items.is_empty() {
        return;
    }
    let hypothetical_sum: f32 = items.iter().map(|item| item.outer_main()).sum();
    let initial_free = inner - hypothetical_sum - gap_sum;
    if !initial_free.is_finite() || initial_free.abs() <= EPS {
        return;
    }
    let growing = initial_free > 0.0;
    for item in items.iter_mut() {
        let factor = if growing { item.grow } else { item.shrink };
        // Max-clamped items cannot grow; min-clamped items cannot shrink.
        let clamped_off_base = if growing {
            item.base > item.content + EPS
        } else {
            item.base + EPS < item.content
        };
        if factor <= 0.0 || clamped_off_base {
            item.frozen = true;
        }
    }
    for _ in 0..items.len() + 1 {
        let mut frozen_sum = 0.0;
        let mut base_sum = 0.0;
        let mut flexible = Vec::new();
        for (i, item) in items.iter().enumerate() {
            if item.frozen {
                frozen_sum += item.outer_main();
            } else {
                base_sum += outer_at(item, item.base.max(0.0));
                flexible.push(i);
            }
        }
        if flexible.is_empty() {
            return;
        }
        let mut free = inner - frozen_sum - base_sum - gap_sum;
        // CSS Flexbox §9.7.4: when the sum of unfrozen flex factors is less
        // than one, only that fraction of the initial free space is
        // distributed (flex-grow/shrink 0.1 must not absorb the whole gap).
        let factor_sum: f32 = flexible
            .iter()
            .map(|&i| {
                if growing {
                    items[i].grow
                } else {
                    items[i].shrink
                }
            })
            .sum();
        if factor_sum < 1.0 {
            let scaled_initial = initial_free * factor_sum;
            if scaled_initial.abs() < free.abs() {
                free = scaled_initial;
            }
        }
        if growing {
            free = free.max(0.0);
        } else {
            free = free.min(0.0);
        }
        let total: f32 = flexible
            .iter()
            .map(|&i| scaled_factor(growing, &items[i]))
            .sum();
        if total <= EPS || free.abs() <= EPS {
            for &i in &flexible {
                items[i].content = items[i].base.max(0.0).clamp(items[i].min, items[i].max);
                items[i].frozen = true;
            }
            return;
        }
        let mut violation = 0.0;
        let mut planned = Vec::with_capacity(flexible.len());
        for &i in &flexible {
            let share = scaled_factor(growing, &items[i]);
            let raw = items[i].base.max(0.0) + free * share / total;
            let clamped = raw.clamp(items[i].min, items[i].max);
            violation += clamped - raw;
            planned.push((i, raw, clamped));
        }
        if violation.abs() <= EPS {
            for (i, _, clamped) in planned {
                items[i].content = clamped;
                items[i].frozen = true;
            }
            return;
        }
        let mut froze_one = false;
        for (i, raw, clamped) in planned {
            if (clamped - raw).abs() > EPS {
                items[i].content = clamped;
                items[i].frozen = true;
                froze_one = true;
            }
        }
        if !froze_one {
            return;
        }
    }
}

fn snap_to_inner(items: &mut [Item], inner: f32, gap_sum: f32) {
    if items.is_empty() {
        return;
    }
    let free = inner - items.iter().map(|item| item.outer_main()).sum::<f32>() - gap_sum;
    if free.abs() > EPS {
        for item in items.iter_mut() {
            item.content = unit(item.content);
        }
        return;
    }
    let last = items.len() - 1;
    let mut used = gap_sum;
    for item in items.iter_mut().take(last) {
        item.content = unit(item.content);
        used += item.outer_main();
    }
    let edges =
        items[last].margin_main_start + items[last].margin_main_end + items[last].main_edges;
    items[last].content = unit((inner - used - edges).max(items[last].min));
}

fn justify(kind: JustifyContent, free: f32, count: usize) -> (f32, f32) {
    if count == 0 {
        return (0.0, 0.0);
    }
    // Center and flex-end keep their alignment when the line overflows
    // (unsafe overflow: the lead may be negative). Distribution values
    // pack to the start, which is what the legacy engine paints and what
    // flexbox-justify-content-horiz-004's reference expects.
    if free <= EPS {
        return match kind {
            JustifyContent::FlexEnd => (free, 0.0),
            JustifyContent::Center => (free / 2.0, 0.0),
            _ => (0.0, 0.0),
        };
    }
    let n = count as f32;
    match kind {
        JustifyContent::FlexStart => (0.0, 0.0),
        JustifyContent::FlexEnd => (free, 0.0),
        JustifyContent::Center => (free / 2.0, 0.0),
        JustifyContent::SpaceBetween => {
            if count == 1 {
                (0.0, 0.0)
            } else {
                (0.0, free / (n - 1.0))
            }
        }
        JustifyContent::SpaceAround => (free / (2.0 * n), free / n),
        JustifyContent::SpaceEvenly => {
            let gap = free / (n + 1.0);
            (gap, gap)
        }
    }
}

fn layout_item(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    container_inner_width: f32,
    container_inner_height: Option<f32>,
    row: bool,
    main: Option<f32>,
    cross: Option<f32>,
) -> Option<LayoutResult> {
    let mut nested = *space;
    nested.available_inline = container_inner_width;
    nested.percentage_block = container_inner_height;
    nested.rtl = false;
    nested.bfc_x = 0.0;
    nested.bfc_y = 0.0;
    nested.forced_bfc = false;
    let (width, height) = if row {
        (main.why("flex:main")?, cross)
    } else {
        (cross.why("flex:cross")?, main)
    };
    block::layout_block_sized(flat, index, &nested, false, Some((width, height))).why("flex:item")
}

/// Lay out one nowrap flex container. `sized` is the content box a float,
/// an abspos containing block, or a stretch pass already resolved.
pub(super) fn layout(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    sized: Option<(f32, Option<f32>)>,
) -> Option<LayoutResult> {
    let style = flat[index].style;
    let mut metrics: BoxMetrics = block::resolve_inline(style, space);
    if let Some((width, _)) = sized {
        metrics.content_width = width;
    }
    let direction = style.flex_direction;
    let row = row_axis(direction);
    let reverse = reverse_axis(direction);
    let [bt, br, bb, bl] = metrics.border;
    let [pt, pr, pb, pl] = metrics.padding;
    let inner_width = metrics.content_width.max(0.0);
    let to_content_h = |size: f32| to_content(style, bt + bb + pt + pb, size);
    let specified_height = if let Some((_, Some(height))) = sized {
        Some(height)
    } else {
        match definite(style.height, space.percentage_block, style, space).why("flex:percent")? {
            Some(size) => Some(to_content_h(size)),
            None => None,
        }
    };
    let inner_main = if row {
        Some(inner_width)
    } else {
        specified_height
    };
    let gap = if row {
        style.column_gap.unwrap_or(style.gap)
    } else {
        style.row_gap.unwrap_or(style.gap)
    }
    .max(0.0);

    let mut items = Vec::new();
    for child in rendered_children(flat, index) {
        let child_style = flat[child].style;
        if oof::is_out_of_flow(child_style) {
            return super::bail("flex:oof");
        }
        if child_style.float != Float::None {
            return super::bail("flex:float");
        }
        let main_edges = item_edges(child_style, space, inner_width, row);
        let cross_edges = item_edges(child_style, space, inner_width, !row);
        let (min, max) =
            resolve_main_limits(child_style, row, main_edges, inner_main, space, flat, child)
                .why("flex:min")?;
        let (main_start, main_end) = main_margin_sides(child_style, direction);
        let (cross_start, cross_end) = cross_margin_sides(child_style, direction);
        let (auto_main_start, auto_main_end) = (
            matches!(main_start, Spacing::Auto),
            matches!(main_end, Spacing::Auto),
        );
        if (!row && (auto_main_start || auto_main_end)) && specified_height.is_none() {
            return super::bail("flex:auto-margin");
        }
        let margin_main_start = spacing_px(child_style, main_start, inner_width, space);
        let margin_main_end = spacing_px(child_style, main_end, inner_width, space);
        let margin_cross_start = spacing_px(child_style, cross_start, inner_width, space);
        let margin_cross_end = spacing_px(child_style, cross_end, inner_width, space);
        let cross_dim = if row {
            child_style.height
        } else {
            child_style.width
        };
        let cross_basis = if row {
            specified_height
        } else {
            Some(inner_width)
        };
        let cross_auto = matches!(cross_dim, Dimension::Auto);
        let mut cross =
            match definite(cross_dim, cross_basis, child_style, space).why("flex:percent")? {
                Some(size) => Some(to_content(child_style, cross_edges, size)),
                None => None,
            };
        // A column item's cross size is its width, which is definite before
        // an auto height is measured.
        if !row && cross.is_none() {
            let align = used_align(style.align_items, child_style.align_self);
            let raw = if align == AlignItems::Stretch {
                (inner_width - margin_cross_start - margin_cross_end - cross_edges).max(0.0)
            } else {
                content_minmax(flat, child, space)
                    .why("flex:intrinsic")?
                    .max
                    .max(0.0)
            };
            cross = Some(
                clamp_cross(
                    child_style,
                    false,
                    cross_edges,
                    raw,
                    Some(inner_width),
                    space,
                )
                .why("flex:cross")?,
            );
        }
        let basis_auto = matches!(child_style.flex_basis, Dimension::Auto)
            && matches!(child_style.height, Dimension::Auto);
        let measured = if !row && basis_auto {
            let laid = layout_item(
                flat,
                child,
                space,
                inner_width,
                specified_height,
                false,
                None,
                cross,
            )?;
            Some((laid.fragment.height - main_edges).max(0.0))
        } else {
            None
        };
        let base = flex_base(
            child_style,
            row,
            main_edges,
            inner_main,
            space,
            flat,
            child,
            measured,
        )
        .why("flex:basis")?;
        let base = base.max(0.0);
        items.push(Item {
            index: child,
            order: child_style.order,
            grow: child_style.flex_grow.max(0.0),
            shrink: child_style.flex_shrink.max(0.0),
            base,
            min,
            max,
            content: base.clamp(min, max),
            main_edges,
            cross_edges,
            margin_main_start,
            margin_main_end,
            margin_cross_start,
            margin_cross_end,
            auto_main_start,
            auto_main_end,
            cross,
            cross_auto,
            align: used_align(style.align_items, child_style.align_self),
            frozen: false,
        });
    }
    items.sort_by(|a, b| a.order.cmp(&b.order).then(a.index.cmp(&b.index)));

    let gap_sum = gap * items.len().saturating_sub(1) as f32;
    let main_inner = if let Some(inner) = inner_main {
        if !items.is_empty() {
            distribute(&mut items, inner, gap_sum);
            snap_to_inner(&mut items, inner, gap_sum);
        }
        inner
    } else {
        for item in &mut items {
            item.content = unit(item.content);
        }
        items.iter().map(|item| item.outer_main()).sum::<f32>() + gap_sum
    };

    if row {
        for item in &mut items {
            if item.cross.is_some() {
                continue;
            }
            let laid = layout_item(
                flat,
                item.index,
                space,
                inner_width,
                specified_height,
                true,
                Some(item.content),
                None,
            )?;
            item.cross = Some((laid.fragment.height - item.cross_edges).max(0.0));
        }
    }

    let content_cross_definite = if row {
        specified_height
    } else {
        Some(inner_width)
    };
    let max_margin_box = items
        .iter()
        .map(|item| item.margin_box_cross())
        .fold(0.0_f32, f32::max);
    let line_inner = if style.align_content == AlignContent::Stretch {
        content_cross_definite.unwrap_or(max_margin_box)
    } else {
        max_margin_box
    };

    for item in &mut items {
        if item.align == AlignItems::Stretch && item.cross_auto {
            let raw =
                (line_inner - item.margin_cross_start - item.margin_cross_end - item.cross_edges)
                    .max(0.0);
            let basis = if row {
                specified_height
            } else {
                Some(inner_width)
            };
            item.cross = Some(
                clamp_cross(
                    flat[item.index].style,
                    row,
                    item.cross_edges,
                    raw,
                    basis,
                    space,
                )
                .why("flex:cross")?,
            );
        }
    }

    let mut fragments = Vec::with_capacity(items.len());
    for item in &items {
        fragments.push(layout_item(
            flat,
            item.index,
            space,
            inner_width,
            specified_height,
            row,
            Some(item.content),
            item.cross,
        )?);
    }

    // Positive free space left after flexible lengths goes to auto main
    // margins, then to justify-content. Center and flex-end also apply
    // when free space is negative.
    let used_outer = items.iter().map(|item| item.outer_main()).sum::<f32>() + gap_sum;
    let mut free = main_inner - used_outer;
    let auto_margins = items
        .iter()
        .map(|item| usize::from(item.auto_main_start) + usize::from(item.auto_main_end))
        .sum::<usize>();
    if free > EPS && auto_margins > 0 {
        let share = free / auto_margins as f32;
        for item in &mut items {
            if item.auto_main_start {
                item.margin_main_start += share;
            }
            if item.auto_main_end {
                item.margin_main_end += share;
            }
        }
        free = 0.0;
    }
    let (mut lead, between) = justify(style.justify_content, free, items.len());
    if reverse {
        lead = main_inner - lead;
    }
    let mut cursor = lead;
    let mut children = Vec::with_capacity(items.len());
    let mut oof = Vec::new();
    for (item, laid) in items.iter().zip(fragments) {
        let border_main = item.content + item.main_edges;
        let main = if reverse {
            cursor - item.margin_main_start - border_main
        } else {
            cursor + item.margin_main_start
        };
        let main = unit(main);
        let margin_box = item.margin_box_cross();
        let extra = (line_inner - margin_box).max(0.0);
        // A definite cross size does not stretch; it aligns as flex-start.
        let cross_offset = match item.align {
            AlignItems::FlexEnd => extra,
            AlignItems::Center => extra / 2.0,
            AlignItems::Stretch | AlignItems::FlexStart | AlignItems::Baseline => 0.0,
        };
        let cross = unit(item.margin_cross_start + cross_offset);
        let (x, y) = if row {
            (bl + pl + main, bt + pt + cross)
        } else {
            (bl + pl + cross, bt + pt + main)
        };
        let (dx, dy) = oof::relative_offset(flat[item.index].style, space);
        oof.extend(
            laid.oof
                .iter()
                .map(|candidate| candidate.translated(x + dx, y + dy)),
        );
        children.push((x + dx, y + dy, laid.fragment));
        let step = item.outer_main() + gap + between;
        if reverse {
            cursor -= step;
        } else {
            cursor += step;
        }
    }

    let content_main = if row {
        inner_width
    } else {
        main_inner.max(0.0)
    };
    let content_cross = if row {
        specified_height.unwrap_or(line_inner)
    } else {
        inner_width
    };
    let (width, mut height) = if row {
        (content_main, content_cross)
    } else {
        (content_cross, content_main)
    };
    if let Some(max) = block::resolve_length(style.max_height, space.percentage_block, style, space)
    {
        let max = to_content_h(max);
        if height > max + EPS {
            return super::bail("flex:clamp");
        }
        height = height.min(max);
    }
    if let Some(min) = block::resolve_length(style.min_height, space.percentage_block, style, space)
    {
        if !matches!(style.min_height, Dimension::Auto) {
            let min = to_content_h(min);
            if height + EPS < min {
                return super::bail("flex:clamp");
            }
            height = height.max(min);
        }
    }
    let border_width = width + bl + br + pl + pr;
    let border_height = height + bt + bb + pt + pb;
    if style.position != w3cos_std::style::Position::Static && !oof.is_empty() {
        let cb = oof::ContainingBlock {
            x: bl,
            y: bt,
            width: border_width - bl - br,
            height: border_height - bt - bb,
            rtl: false,
        };
        let (placed, rest) = oof::resolve(
            flat,
            std::mem::take(&mut oof),
            &cb,
            false,
            space.viewport_w,
            space.viewport_h,
        )
        .why("flex:oof-resolve")?;
        children.extend(placed);
        oof = rest;
    }
    let mut start_strut = MarginStrut::default();
    start_strut.append(metrics.margin[0]);
    let mut end_strut = MarginStrut::default();
    end_strut.append(metrics.margin[2]);
    Some(LayoutResult {
        fragment: BoxFragment {
            index,
            width: border_width,
            height: border_height,
            text_range: None,
            children,
        },
        last_baseline: LastBaseline::NoLines,
        start_strut,
        end_strut,
        self_collapsing: false,
        oof,
        bfc_dependent: false,
        float_overlap: false,
    })
}

/// Clamp a cross size by the item's specified min/max on that axis.
/// `min-width`/`min-height: auto` is not a floor here (the main-axis
/// automatic minimum is resolved separately).
fn clamp_cross(
    style: &Style,
    row: bool,
    edges: f32,
    mut size: f32,
    basis: Option<f32>,
    space: &ConstraintSpace,
) -> Option<f32> {
    let (min_dim, max_dim) = if row {
        (style.min_height, style.max_height)
    } else {
        (style.min_width, style.max_width)
    };
    if let Some(max) = definite(max_dim, basis, style, space).why("flex:percent")? {
        size = size.min(to_content(style, edges, max));
    }
    if !matches!(min_dim, Dimension::Auto) {
        if let Some(min) = definite(min_dim, basis, style, space).why("flex:percent")? {
            size = size.max(to_content(style, edges, min));
        }
    }
    Some(size.max(0.0))
}

/// Border plus padding on one axis. Percentage padding resolves against
/// the flex container's content width (CSS 2.1 8.4).
fn item_edges(style: &Style, space: &ConstraintSpace, cb: f32, horizontal: bool) -> f32 {
    let border = block::border_widths(style);
    let pad = |spacing| spacing_px(style, spacing, cb, space);
    if horizontal {
        border[1] + border[3] + pad(style.padding.left) + pad(style.padding.right)
    } else {
        border[0] + border[2] + pad(style.padding.top) + pad(style.padding.bottom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::pre_flatten;
    use w3cos_std::Component;
    use w3cos_std::style::{Display, Edges};

    fn px(v: f32) -> Spacing {
        Spacing::Px(v)
    }

    fn edges(t: f32, r: f32, b: f32, l: f32) -> Edges {
        Edges {
            top: px(t),
            right: px(r),
            bottom: px(b),
            left: px(l),
        }
    }

    fn block_box(width: Option<f32>, height: Option<f32>) -> Style {
        Style {
            display: Display::Block,
            width: width.map(Dimension::Px).unwrap_or(Dimension::Auto),
            height: height.map(Dimension::Px).unwrap_or(Dimension::Auto),
            ..Style::default()
        }
    }

    fn flex_style(direction: FlexDirection, width: f32, height: Option<f32>) -> Style {
        Style {
            display: Display::Flex,
            flex_direction: direction,
            width: Dimension::Px(width),
            height: height.map(Dimension::Px).unwrap_or(Dimension::Auto),
            ..Style::default()
        }
    }

    fn lay(root: &Component) -> Vec<(crate::layout::LayoutRect, usize)> {
        let flat = pre_flatten(root);
        assert!(
            block::supports_tree(&flat),
            "unsupported: {:?}",
            block::unsupported_reasons(&flat)
        );
        super::super::layout_tree(root, &flat, 800.0, 600.0).expect("ng flex")
    }

    fn rect(out: &[(crate::layout::LayoutRect, usize)], index: usize) -> [f32; 4] {
        let r = out.iter().find(|(_, i)| *i == index).expect("rect").0;
        [r.x, r.y, r.width, r.height]
    }

    fn expect(root: &Component, expected: &[(usize, [f32; 4])]) {
        let out = lay(root);
        for (index, want) in expected {
            let got = rect(&out, *index);
            assert_eq!(got, *want, "index {index}");
        }
    }

    fn row_of(width: f32, height: Option<f32>, children: Vec<Component>) -> Component {
        Component::boxed(flex_style(FlexDirection::Row, width, height), children)
    }

    fn leaf(style: Style) -> Component {
        Component::boxed(style, vec![])
    }

    #[test]
    fn chrome_nowrap_main_and_cross() {
        // Headless Chrome, body margin 0, viewport 800x600, content-box.
        // Rects are border boxes. `flex:1` is grow 1, shrink 1, basis 0%.
        let fixed = row_of(
            300.0,
            Some(80.0),
            vec![
                leaf(block_box(Some(50.0), Some(40.0))),
                leaf(block_box(Some(70.0), Some(30.0))),
            ],
        );
        expect(
            &fixed,
            &[
                (0, [0.0, 0.0, 300.0, 80.0]),
                (1, [0.0, 0.0, 50.0, 40.0]),
                (2, [50.0, 0.0, 70.0, 30.0]),
            ],
        );

        let mut grow = block_box(None, Some(40.0));
        grow.flex_grow = 1.0;
        grow.flex_basis = Dimension::Percent(0.0);
        let grown = row_of(
            300.0,
            None,
            vec![leaf(block_box(Some(50.0), Some(40.0))), leaf(grow)],
        );
        expect(
            &grown,
            &[
                (0, [0.0, 0.0, 300.0, 40.0]),
                (1, [0.0, 0.0, 50.0, 40.0]),
                (2, [50.0, 0.0, 250.0, 40.0]),
            ],
        );

        let mut centered = flex_style(FlexDirection::Row, 300.0, None);
        centered.justify_content = JustifyContent::Center;
        expect(
            &Component::boxed(
                centered,
                vec![
                    leaf(block_box(Some(40.0), Some(20.0))),
                    leaf(block_box(Some(60.0), Some(20.0))),
                ],
            ),
            &[(1, [100.0, 0.0, 40.0, 20.0]), (2, [140.0, 0.0, 60.0, 20.0])],
        );

        let mut between = flex_style(FlexDirection::Row, 300.0, None);
        between.justify_content = JustifyContent::SpaceBetween;
        expect(
            &Component::boxed(
                between,
                vec![
                    leaf(block_box(Some(40.0), Some(20.0))),
                    leaf(block_box(Some(60.0), Some(20.0))),
                    leaf(block_box(Some(20.0), Some(20.0))),
                ],
            ),
            &[
                (1, [0.0, 0.0, 40.0, 20.0]),
                (2, [130.0, 0.0, 60.0, 20.0]),
                (3, [280.0, 0.0, 20.0, 20.0]),
            ],
        );

        let mut gapped = flex_style(FlexDirection::Row, 300.0, None);
        gapped.gap = 10.0;
        gapped.justify_content = JustifyContent::SpaceBetween;
        expect(
            &Component::boxed(
                gapped,
                vec![
                    leaf(block_box(Some(40.0), Some(20.0))),
                    leaf(block_box(Some(60.0), Some(20.0))),
                    leaf(block_box(Some(20.0), Some(20.0))),
                ],
            ),
            &[
                (1, [0.0, 0.0, 40.0, 20.0]),
                (2, [130.0, 0.0, 60.0, 20.0]),
                (3, [280.0, 0.0, 20.0, 20.0]),
            ],
        );

        let mut around = flex_style(FlexDirection::Row, 300.0, None);
        around.justify_content = JustifyContent::SpaceAround;
        expect(
            &Component::boxed(
                around,
                vec![
                    leaf(block_box(Some(40.0), Some(20.0))),
                    leaf(block_box(Some(60.0), Some(20.0))),
                ],
            ),
            &[(1, [50.0, 0.0, 40.0, 20.0]), (2, [190.0, 0.0, 60.0, 20.0])],
        );

        let mut evenly = flex_style(FlexDirection::Row, 300.0, None);
        evenly.justify_content = JustifyContent::SpaceEvenly;
        expect(
            &Component::boxed(
                evenly,
                vec![
                    leaf(block_box(Some(40.0), Some(20.0))),
                    leaf(block_box(Some(60.0), Some(20.0))),
                ],
            ),
            &[
                (1, [66.65625, 0.0, 40.0, 20.0]),
                (2, [173.328125, 0.0, 60.0, 20.0]),
            ],
        );

        let mut end = flex_style(FlexDirection::Row, 300.0, None);
        end.justify_content = JustifyContent::FlexEnd;
        expect(
            &Component::boxed(
                end,
                vec![
                    leaf(block_box(Some(40.0), Some(20.0))),
                    leaf(block_box(Some(60.0), Some(20.0))),
                ],
            ),
            &[(1, [200.0, 0.0, 40.0, 20.0]), (2, [240.0, 0.0, 60.0, 20.0])],
        );
    }

    #[test]
    fn chrome_shrink_grow_min_and_order() {
        let mut basis = block_box(None, Some(20.0));
        basis.flex_grow = 0.0;
        basis.flex_shrink = 1.0;
        basis.flex_basis = Dimension::Px(150.0);
        expect(
            &row_of(200.0, None, vec![leaf(basis.clone()), leaf(basis)]),
            &[(1, [0.0, 0.0, 100.0, 20.0]), (2, [100.0, 0.0, 100.0, 20.0])],
        );

        let mut flexible = block_box(None, Some(20.0));
        flexible.flex_grow = 1.0;
        flexible.flex_shrink = 1.0;
        flexible.flex_basis = Dimension::Px(80.0);
        flexible.min_width = Dimension::Px(60.0);
        expect(
            &row_of(
                100.0,
                None,
                vec![
                    leaf(flexible.clone()),
                    leaf(block_box(Some(50.0), Some(20.0))),
                ],
            ),
            &[
                (1, [0.0, 0.0, 61.53125, 20.0]),
                (2, [61.53125, 0.0, 38.46875, 20.0]),
            ],
        );

        flexible.min_width = Dimension::Px(70.0);
        expect(
            &row_of(
                100.0,
                None,
                vec![leaf(flexible), leaf(block_box(Some(50.0), Some(20.0)))],
            ),
            &[(1, [0.0, 0.0, 70.0, 20.0]), (2, [70.0, 0.0, 30.0, 20.0])],
        );

        // min-width above the flex base freezes that item; the other grows
        // from its own base (80 and 20, not 85 and 15).
        let mut above = block_box(None, Some(20.0));
        above.flex_grow = 1.0;
        above.flex_shrink = 1.0;
        above.flex_basis = Dimension::Px(10.0);
        let mut floored = above.clone();
        floored.min_width = Dimension::Px(80.0);
        expect(
            &row_of(100.0, None, vec![leaf(floored), leaf(above)]),
            &[(1, [0.0, 0.0, 80.0, 20.0]), (2, [80.0, 0.0, 20.0, 20.0])],
        );

        let mut one = block_box(None, Some(20.0));
        one.flex_grow = 1.0;
        one.flex_basis = Dimension::Percent(0.0);
        let mut two = one.clone();
        two.flex_grow = 2.0;
        expect(
            &row_of(300.0, None, vec![leaf(one), leaf(two)]),
            &[(1, [0.0, 0.0, 100.0, 20.0]), (2, [100.0, 0.0, 200.0, 20.0])],
        );

        let mut later = block_box(Some(40.0), Some(20.0));
        later.order = 1;
        expect(
            &row_of(
                300.0,
                None,
                vec![leaf(later), leaf(block_box(Some(60.0), Some(20.0)))],
            ),
            &[(1, [60.0, 0.0, 40.0, 20.0]), (2, [0.0, 0.0, 60.0, 20.0])],
        );
    }

    #[test]
    fn chrome_alignment_reverse_margins_and_column() {
        let mut center = flex_style(FlexDirection::Row, 200.0, Some(100.0));
        center.align_items = AlignItems::Center;
        expect(
            &Component::boxed(
                center,
                vec![
                    leaf(block_box(Some(40.0), Some(20.0))),
                    leaf(block_box(Some(30.0), Some(50.0))),
                ],
            ),
            &[(1, [0.0, 40.0, 40.0, 20.0]), (2, [40.0, 25.0, 30.0, 50.0])],
        );

        let mut end = flex_style(FlexDirection::Row, 200.0, Some(100.0));
        end.align_items = AlignItems::FlexEnd;
        expect(
            &Component::boxed(
                end,
                vec![
                    leaf(block_box(Some(40.0), Some(20.0))),
                    leaf(block_box(Some(30.0), Some(50.0))),
                ],
            ),
            &[(1, [0.0, 80.0, 40.0, 20.0]), (2, [40.0, 50.0, 30.0, 50.0])],
        );

        expect(
            &row_of(
                200.0,
                Some(100.0),
                vec![
                    leaf(block_box(Some(40.0), None)),
                    leaf(block_box(Some(30.0), Some(50.0))),
                ],
            ),
            &[(1, [0.0, 0.0, 40.0, 100.0]), (2, [40.0, 0.0, 30.0, 50.0])],
        );

        let mut reversed = flex_style(FlexDirection::RowReverse, 300.0, None);
        expect(
            &Component::boxed(
                reversed.clone(),
                vec![
                    leaf(block_box(Some(40.0), Some(20.0))),
                    leaf(block_box(Some(60.0), Some(20.0))),
                ],
            ),
            &[(1, [260.0, 0.0, 40.0, 20.0]), (2, [200.0, 0.0, 60.0, 20.0])],
        );
        let mut auto_start = block_box(Some(40.0), Some(20.0));
        auto_start.margin.right = Spacing::Auto;
        expect(
            &Component::boxed(
                reversed,
                vec![leaf(auto_start), leaf(block_box(Some(60.0), Some(20.0)))],
            ),
            &[(1, [60.0, 0.0, 40.0, 20.0]), (2, [0.0, 0.0, 60.0, 20.0])],
        );

        let mut auto_left = block_box(Some(40.0), Some(20.0));
        auto_left.margin.left = Spacing::Auto;
        expect(
            &row_of(
                300.0,
                None,
                vec![leaf(auto_left), leaf(block_box(Some(50.0), Some(20.0)))],
            ),
            &[(1, [210.0, 0.0, 40.0, 20.0]), (2, [250.0, 0.0, 50.0, 20.0])],
        );

        let mut padded = flex_style(FlexDirection::Row, 300.0, Some(80.0));
        padded.box_sizing = BoxSizing::BorderBox;
        padded.padding = edges(10.0, 10.0, 10.0, 10.0);
        padded.border_width = 5.0;
        expect(
            &Component::boxed(padded, vec![leaf(block_box(Some(40.0), Some(20.0)))]),
            &[(0, [0.0, 0.0, 300.0, 80.0]), (1, [15.0, 15.0, 40.0, 20.0])],
        );

        let mut border_box = block_box(Some(100.0), Some(40.0));
        border_box.box_sizing = BoxSizing::BorderBox;
        border_box.padding = edges(10.0, 10.0, 10.0, 10.0);
        border_box.border_width = 5.0;
        expect(
            &row_of(
                300.0,
                None,
                vec![leaf(border_box), leaf(block_box(Some(50.0), Some(20.0)))],
            ),
            &[
                (0, [0.0, 0.0, 300.0, 40.0]),
                (1, [0.0, 0.0, 100.0, 40.0]),
                (2, [100.0, 0.0, 50.0, 20.0]),
            ],
        );

        expect(
            &Component::boxed(
                flex_style(FlexDirection::Column, 100.0, None),
                vec![
                    leaf(block_box(None, Some(30.0))),
                    leaf(block_box(None, Some(50.0))),
                ],
            ),
            &[
                (0, [0.0, 0.0, 100.0, 80.0]),
                (1, [0.0, 0.0, 100.0, 30.0]),
                (2, [0.0, 30.0, 100.0, 50.0]),
            ],
        );

        let mut col_grow = block_box(None, None);
        col_grow.flex_grow = 1.0;
        col_grow.flex_basis = Dimension::Percent(0.0);
        expect(
            &Component::boxed(
                flex_style(FlexDirection::Column, 100.0, Some(200.0)),
                vec![leaf(block_box(None, Some(40.0))), leaf(col_grow)],
            ),
            &[(1, [0.0, 0.0, 100.0, 40.0]), (2, [0.0, 40.0, 100.0, 160.0])],
        );

        expect(
            &Component::boxed(
                flex_style(FlexDirection::ColumnReverse, 80.0, Some(200.0)),
                vec![
                    leaf(block_box(None, Some(30.0))),
                    leaf(block_box(None, Some(50.0))),
                ],
            ),
            &[(1, [0.0, 170.0, 80.0, 30.0]), (2, [0.0, 120.0, 80.0, 50.0])],
        );

        // Auto main size of a column item is the height of its contents.
        expect(
            &Component::boxed(
                flex_style(FlexDirection::Column, 100.0, None),
                vec![
                    Component::boxed(
                        block_box(None, None),
                        vec![leaf(block_box(None, Some(30.0)))],
                    ),
                    leaf(block_box(None, Some(20.0))),
                ],
            ),
            &[
                (0, [0.0, 0.0, 100.0, 50.0]),
                (1, [0.0, 0.0, 100.0, 30.0]),
                (2, [0.0, 0.0, 100.0, 30.0]),
                (3, [0.0, 30.0, 100.0, 20.0]),
            ],
        );
    }

    #[test]
    fn definite_child_sets_the_automatic_minimum() {
        // Item width 100, child width 200: min(content, specified) is 100,
        // so a 10px row cannot shrink the item. Chrome, body margin 0.
        let item = Component::boxed(
            block_box(Some(100.0), None),
            vec![leaf(block_box(Some(200.0), Some(100.0)))],
        );
        expect(
            &row_of(10.0, None, vec![item]),
            &[(1, [0.0, 0.0, 100.0, 100.0]), (2, [0.0, 0.0, 200.0, 100.0])],
        );

        // Column container height 10. The child's definite height is the
        // item's content min-height, so the item stays 100.
        let item = Component::boxed(
            block_box(None, None),
            vec![leaf(block_box(Some(100.0), Some(100.0)))],
        );
        expect(
            &Component::boxed(
                flex_style(FlexDirection::Column, 100.0, Some(10.0)),
                vec![item],
            ),
            &[
                (0, [0.0, 0.0, 100.0, 10.0]),
                (1, [0.0, 0.0, 100.0, 100.0]),
                (2, [0.0, 0.0, 100.0, 100.0]),
            ],
        );
    }

    #[test]
    fn flex_factor_below_one_scales_free_space() {
        // CSS Flexbox §9.7.4. shrink 0.9 of (50 - 550) absorbs 450, not 500.
        let mut shrink = block_box(Some(550.0), Some(50.0));
        shrink.flex_shrink = 0.9;
        expect(
            &row_of(50.0, None, vec![leaf(shrink)]),
            &[(1, [0.0, 0.0, 100.0, 50.0])],
        );

        // grow 0.1 of (190 - 90) adds 10, not the whole 100.
        let mut grow = block_box(Some(90.0), Some(50.0));
        grow.flex_grow = 0.1;
        expect(
            &row_of(190.0, None, vec![leaf(grow)]),
            &[(1, [0.0, 0.0, 100.0, 50.0])],
        );

        // Two items, shrink 0.25 each, base 75, container 50.
        // Sum of factors 0.5 scales the -100 free space to -50.
        let mut half = block_box(Some(75.0), Some(50.0));
        half.flex_shrink = 0.25;
        expect(
            &row_of(50.0, None, vec![leaf(half.clone()), leaf(half)]),
            &[(1, [0.0, 0.0, 50.0, 50.0]), (2, [50.0, 0.0, 50.0, 50.0])],
        );
    }

    #[test]
    fn column_stretch_obeys_max_width() {
        let mut item = block_box(None, Some(40.0));
        item.max_width = Dimension::Px(50.0);
        expect(
            &Component::boxed(
                flex_style(FlexDirection::Column, 100.0, None),
                vec![leaf(item)],
            ),
            &[(0, [0.0, 0.0, 100.0, 40.0]), (1, [0.0, 0.0, 50.0, 40.0])],
        );
    }

    #[test]
    fn centered_overflow_uses_a_negative_lead() {
        // flexbox_justifycontent-center-overflow.html: three margin boxes
        // of 60 in a 50px line. free = -130, center lead = -65, so the
        // border boxes sit at -55, 5 and 65.
        let mut container = flex_style(FlexDirection::Row, 50.0, Some(150.0));
        container.justify_content = JustifyContent::Center;
        let mut item = block_box(None, Some(20.0));
        item.flex_grow = 1.0;
        item.flex_shrink = 0.0;
        item.flex_basis = Dimension::Px(40.0);
        item.margin = edges(10.0, 10.0, 10.0, 10.0);
        expect(
            &Component::boxed(
                container,
                vec![leaf(item.clone()), leaf(item.clone()), leaf(item)],
            ),
            &[
                (1, [-55.0, 10.0, 40.0, 20.0]),
                (2, [5.0, 10.0, 40.0, 20.0]),
                (3, [65.0, 10.0, 40.0, 20.0]),
            ],
        );

        // flex-end overflow shifts the whole line to the start. A 45px
        // margin box in a 30px line starts at -15 (horiz-004, one item).
        let mut end = flex_style(FlexDirection::Row, 30.0, None);
        end.justify_content = JustifyContent::FlexEnd;
        let mut wide = block_box(None, Some(10.0));
        wide.flex_grow = 0.0;
        wide.flex_shrink = 0.0;
        wide.flex_basis = Dimension::Px(35.0);
        wide.padding = edges(2.0, 2.0, 2.0, 2.0);
        wide.border_left_width = Some(4.0);
        wide.border_right_width = Some(2.0);
        wide.border_top_width = Some(1.0);
        wide.border_bottom_width = Some(3.0);
        expect(
            &Component::boxed(end, vec![leaf(wide)]),
            &[(1, [-15.0, 0.0, 45.0, 18.0])],
        );

        // Distribution values with negative free space stay packed at the
        // start (horiz-004 reference), unlike center.
        let mut around = flex_style(FlexDirection::Row, 30.0, None);
        around.justify_content = JustifyContent::SpaceAround;
        let mut piece = block_box(Some(40.0), Some(10.0));
        piece.flex_grow = 0.0;
        piece.flex_shrink = 0.0;
        piece.flex_basis = Dimension::Px(40.0);
        expect(
            &Component::boxed(around, vec![leaf(piece.clone()), leaf(piece)]),
            &[(1, [0.0, 0.0, 40.0, 10.0]), (2, [40.0, 0.0, 40.0, 10.0])],
        );
    }

    #[test]
    fn document_element_flex_stays_on_legacy() {
        let mut root = flex_style(FlexDirection::Row, 800.0, Some(600.0));
        root.justify_content = JustifyContent::Center;
        root.custom_properties = Some(w3cos_std::style::CustomProperties::from_iter([(
            "--w3cos-internal-document-element".to_string(),
            "1".to_string(),
        )]));
        let tree = Component::boxed(root, vec![leaf(block_box(Some(40.0), Some(20.0)))]);
        let flat = pre_flatten(&tree);
        assert!(super::super::layout_tree(&tree, &flat, 800.0, 600.0).is_none());
    }

    #[test]
    fn column_cross_alignment_stays_on_legacy() {
        let mut column = flex_style(FlexDirection::Column, 100.0, Some(80.0));
        column.align_items = AlignItems::FlexStart;
        let root = Component::boxed(column, vec![leaf(block_box(None, Some(20.0)))]);
        let flat = pre_flatten(&root);
        assert!(super::super::layout_tree(&root, &flat, 800.0, 600.0).is_none());
    }

    #[test]
    fn wrap_baseline_and_rtl_stay_on_legacy() {
        for style in [
            Style {
                flex_wrap: FlexWrap::Wrap,
                ..flex_style(FlexDirection::Row, 100.0, None)
            },
            Style {
                align_items: AlignItems::Baseline,
                ..flex_style(FlexDirection::Row, 100.0, None)
            },
            Style {
                direction: TextDirection::Rtl,
                ..flex_style(FlexDirection::Row, 100.0, None)
            },
        ] {
            let root = Component::boxed(style, vec![leaf(block_box(Some(10.0), Some(10.0)))]);
            let flat = pre_flatten(&root);
            assert!(super::super::layout_tree(&root, &flat, 800.0, 600.0).is_none());
        }
    }

    #[test]
    fn ifc_marker_is_not_a_flex_container() {
        let children = vec![
            leaf(block_box(Some(20.0), Some(20.0))),
            leaf(block_box(Some(30.0), Some(30.0))),
        ];
        let row = row_of(100.0, None, children.clone());
        // The same boxes in a row sit side by side.
        expect(
            &row,
            &[(1, [0.0, 0.0, 20.0, 20.0]), (2, [20.0, 0.0, 30.0, 30.0])],
        );
        // The IFC marker keeps the DOM's lowered flex row off the flex
        // algorithm. Block children make it not an IFC either, so NG
        // falls back instead of placing them in a row.
        let mut marked = flex_style(FlexDirection::Row, 100.0, None);
        marked
            .custom_properties
            .get_or_insert_with(Default::default)
            .insert(super::super::inline::IFC_MARKER.into(), "1".into());
        let root = Component::boxed(marked, children);
        let flat = pre_flatten(&root);
        assert!(super::super::layout_tree(&root, &flat, 800.0, 600.0).is_none());
    }
}
