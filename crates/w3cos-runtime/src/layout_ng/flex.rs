//! Flex formatting context, stage S5: one flex line, or several lines.
//!
//! Structure follows Blink's NGFlexLayoutAlgorithm
//! (third_party/blink/renderer/core/layout/flex/flex_layout_algorithm.cc,
//! Copyright The Chromium Authors, BSD-style license) and CSS Flexible Box
//! Layout Level 1 §4.5 (automatic minimum size) and §9 (main size, flexible
//! lengths, alignment, line breaking). An indefinite percentage basis
//! stays on the legacy engine.
//! A row aligns baseline items per line by their first baseline, or the
//! synthesized border-box under edge (FlexLayoutAlgorithm::BaselineAscent),
//! measured from the cross end under `wrap-reverse` (the minor group).
//! Column baseline groups and items whose subtree NG reports no baseline
//! for stay on legacy (`flex:baseline-column`, `flex:baseline-nested`).
//! `wrap-reverse` swaps the cross-start (Flexbox §5.2) and packs lines
//! from that edge. Column wrap breaks lines against a definite block size,
//! a `max-height`, or an `aspect-ratio` transferred from a definite width.
//! An automatic block size with none of those is one flex line (Flexbox
//! §9.2); `wrap-reverse` packs that line at the cross end. An automatic
//! item inline size is the fit-content size against the container's inner
//! width. A column wrap whose block size is still automatic and that has
//! a gap stays on legacy (`flex:wrap-column`): its reference uses logical
//! margins and a writing mode this engine does not store. `flex-wrap:
//! balance` breaks the greedy line count's lines evenly
//! (`balanced_line_ranges`). An item whose fit-content
//! size cannot be measured stays on legacy (`flex:wrap-column-auto-cross`).
//!
//! A text run is an anonymous flex item laid out as block text: its flex
//! base size is its max-content width in a row, its automatic minimum
//! the min-content width; in a column both are its height at the used
//! width. Its baseline is the first line's, so a wrapped baseline item
//! (several lines) bails `flex:baseline-unknown`.
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
    AlignContent, AlignItems, AlignSelf, BoxSizing, Dimension, Display, FlexDirection, FlexWrap,
    Float, JustifyContent, Spacing, Style, TextDirection,
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

/// Why this author flex container is outside S5. `None` when the line
/// (or the row wrap) can be laid out. The IFC marker is not a flex container.
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
    // A container sized by logical properties the cascade does not resolve
    // (`block-size`) whose items carry `calc()` widths: gap-003/008 and
    // their references only agree while both sides ignore both, so they
    // stay on legacy together.
    if style.custom_properties.as_ref().is_some_and(|props| {
        props
            .keys()
            .any(|key| key.starts_with("--w3cos-internal-logical-size-"))
    }) {
        let end = flat[index].subtree_end.max(index + 1);
        let mut child = index + 1;
        while child < end {
            let item = flat[child].style;
            if [item.width, item.min_width, item.max_width]
                .iter()
                .any(|dim| matches!(dim, Dimension::Calc { .. }))
            {
                return Some("flex:logical-calc");
            }
            child = flat[child].subtree_end.max(child + 1);
        }
    }
    // An automatic column block size is one flex line, except when a gap
    // is present. gap-003's reference spaces items with logical margins
    // under `writing-mode`, neither of which is stored; both sides of
    // that reftest have to stay on legacy. `max-height` and `aspect-ratio`
    // make the main size definite enough to wrap here.
    let column_wrap = style.flex_wrap != FlexWrap::NoWrap && !row_axis(style.flex_direction);
    if column_wrap
        && matches!(style.height, Dimension::Auto)
        && matches!(style.max_height, Dimension::Auto)
        && !has_marker(style, "--w3cos-internal-aspect-ratio")
        && (style.gap > 0.0
            || style.row_gap.unwrap_or(0.0) > 0.0
            || style.column_gap.unwrap_or(0.0) > 0.0)
    {
        return Some("flex:wrap-column");
    }
    // A nowrap row aligns first baselines below. Anything else with
    // `align-items: baseline` stays here, ahead of the later reasons.
    let baseline = baseline_reason(flat, index);
    if style.align_items == AlignItems::Baseline
        && let Some(reason) = baseline
    {
        return Some(reason);
    }
    if style.direction == TextDirection::Rtl {
        return Some("flex:rtl");
    }
    let row = row_axis(style.flex_direction);
    for child in rendered_children(flat, index) {
        let child_style = flat[child].style;
        if oof::is_out_of_flow(child_style) {
            return Some("flex:oof");
        }
        if child_style.float != Float::None {
            return Some("flex:float");
        }
        if child_style.align_self == AlignSelf::Baseline
            && let Some(reason) = baseline
        {
            return Some(reason);
        }
        // A text run is an anonymous flex item (CSS Flexbox §4), laid out
        // as block text (`block::node_box_reason` validates its content). A
        // run of only white space is not rendered; the DOM is expected to
        // drop it, so one that reaches layout stays on the legacy engine.
        if let w3cos_std::ComponentKind::Text { content } = flat[child].kind
            && content.chars().all(char::is_whitespace)
        {
            return Some("flex:text-space");
        }
        if subtree_has_float(flat, child) {
            return Some("flex:float-descendant");
        }
        // Percentage main sizes and percentage block descendants cycle
        // through the automatic minimum. Leave those columns on legacy.
        // A container with a definite (length) height is a basis for the
        // item's own percentages; only a percentage further down, which
        // resolves against the item's flexed size, still cycles.
        if !row && column_percent_size(flat, child) {
            let definite = matches!(style.height, Dimension::Px(_));
            let blocked = if definite {
                column_percent_descendant(flat, child)
            } else {
                // An indefinite container: the item's flexed size is not a
                // percentage basis (`fixed_block_indefinite`), so descendant
                // percentages are auto. The item's own percentages and its
                // automatic minimum stay on legacy.
                percent_block_size(child_style)
                    || child_style.min_height == Dimension::Auto
                    || super::block::is_author_flex(child_style)
                    || super::block::is_author_grid(child_style)
                    || matches!(
                        child_style.display,
                        w3cos_std::style::Display::Table | w3cos_std::style::Display::InlineTable
                    )
            };
            if blocked {
                return Some("flex:column-percent");
            }
        }
        let (cross_start, cross_end) = cross_margin_sides(
            child_style,
            style.flex_direction,
            style.flex_wrap == FlexWrap::WrapReverse,
        );
        if matches!(cross_start, Spacing::Auto) || matches!(cross_end, Spacing::Auto) {
            return Some("flex:auto-cross-margin");
        }
    }
    None
}

fn baseline_participant(container: AlignItems, item: AlignSelf) -> bool {
    match item {
        AlignSelf::Baseline => true,
        AlignSelf::Auto => container == AlignItems::Baseline,
        _ => false,
    }
}

/// A baseline participant's subtree whose baseline NG does not export:
/// a block-level grid container (only inline-grid reports one) or a
/// multi-line column flex container (`layout_wrapped` reports none).
fn unexported_baseline(flat: &[FlatNodeInfo], index: usize) -> bool {
    let end = flat[index].subtree_end.max(index + 1);
    let mut child = index;
    while child < end {
        let style = flat[child].style;
        if style.display == Display::None {
            child = flat[child].subtree_end.max(child + 1);
            continue;
        }
        if style.display == Display::Grid {
            return true;
        }
        if style.display == Display::Flex
            && style.flex_wrap != FlexWrap::NoWrap
            && !row_axis(style.flex_direction)
        {
            return true;
        }
        child += 1;
    }
    false
}

/// Why baseline alignment stays on legacy, if it does. A row aligns
/// first baselines per line (FlexLayoutAlgorithm::BaselineAscent). Column
/// baseline groups are not done here yet.
fn baseline_reason(flat: &[FlatNodeInfo], index: usize) -> Option<&'static str> {
    let style = flat[index].style;
    if !row_axis(style.flex_direction) {
        return Some("flex:baseline-column");
    }
    for child in rendered_children(flat, index) {
        if baseline_participant(style.align_items, flat[child].style.align_self)
            && unexported_baseline(flat, child)
        {
            return Some("flex:baseline-nested");
        }
    }
    None
}

/// Item ascent for baseline alignment (Blink BaselineAscent): the cross
/// start margin plus the first baseline, or the synthesized border-box
/// under edge when the item has no line. `None` for an unknown baseline.
fn baseline_ascent(item: &Item, laid: &LayoutResult) -> Option<f32> {
    let baseline = match laid.first_baseline {
        LastBaseline::At(baseline) => baseline,
        LastBaseline::NoLines => laid.fragment.height,
        LastBaseline::Unknown => return None,
    };
    Some(item.margin_cross_start + baseline)
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

/// A percentage block size on a descendant of this column item (not the
/// item itself).
fn column_percent_descendant(flat: &[FlatNodeInfo], index: usize) -> bool {
    let end = flat[index].subtree_end.max(index + 1);
    let mut child = index + 1;
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
        || matches!(style.height, Dimension::Percent(_) | Dimension::Calc { .. })
        || matches!(
            style.min_height,
            Dimension::Percent(_) | Dimension::Calc { .. }
        )
        || matches!(
            style.max_height,
            Dimension::Percent(_) | Dimension::Calc { .. }
        )
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
        Dimension::Percent(_) | Dimension::Calc { .. } if basis.is_none() => None,
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

fn cross_margin_sides(
    style: &Style,
    direction: FlexDirection,
    wrap_reverse: bool,
) -> (Spacing, Spacing) {
    // `wrap-reverse` swaps cross-start and cross-end (Flexbox §5.2).
    let (start, end) = if row_axis(direction) {
        (style.margin.top, style.margin.bottom)
    } else {
        (style.margin.left, style.margin.right)
    };
    if wrap_reverse {
        (end, start)
    } else {
        (start, end)
    }
}

fn used_align(container: AlignItems, item: AlignSelf) -> AlignItems {
    match item {
        AlignSelf::Auto => container,
        AlignSelf::FlexStart => AlignItems::FlexStart,
        AlignSelf::FlexEnd => AlignItems::FlexEnd,
        AlignSelf::Center => AlignItems::Center,
        AlignSelf::Stretch => AlignItems::Stretch,
        AlignSelf::Baseline => AlignItems::Baseline,
    }
}

/// Content min/max of the item's contents, ignoring the item's own main
/// size (CSS Flexbox §4.5 content size suggestion). A child's definite
/// width is its min-content contribution; `intrinsic_sizes` only sees
/// descendants, so an empty sized box would otherwise contribute 0.
fn content_minmax(flat: &[FlatNodeInfo], index: usize, space: &ConstraintSpace) -> Option<MinMax> {
    let info = &flat[index];
    if matches!(info.style.display, Display::Table | Display::InlineTable) {
        // The specified width is the flex base, not the content minimum.
        let border =
            super::table::content_intrinsic_border_box(flat, index, space).why("flex:intrinsic")?;
        let style = info.style;
        let side = |spacing| spacing_px(style, spacing, 0.0, space);
        let edges = if style.border_collapse {
            0.0
        } else {
            let border = block::border_widths(style);
            border[1] + border[3] + side(style.padding.left) + side(style.padding.right)
        };
        return Some(border.map(|width| (width - edges).max(0.0)));
    }
    if block::size_contained(info.style) {
        return Some(MinMax::exact(block::contain_intrinsic_size(info.style).0));
    }
    if let w3cos_std::ComponentKind::Text { content } = info.kind {
        return Some(super::inline::text_min_max(content, info.style));
    }
    // A replaced item's min- and max-content sizes are its natural size
    // under its constraints (CSS Sizing 3 §5.1).
    if block::is_replaced(info.kind) {
        return block::intrinsic_sizes(flat, index, space);
    }
    // So are a button leaf's: its label (or nothing when empty).
    if matches!(info.kind, w3cos_std::ComponentKind::Button { .. })
        && rendered_children(flat, index).next().is_none()
    {
        return block::intrinsic_sizes(flat, index, space);
    }
    // A nested flex container's min/max-content sizes are its own flex
    // algorithm's (FlexLayoutAlgorithm::ComputeMinMaxSizes): a row sums
    // its items, it does not take the widest one. `block::intrinsic_sizes`
    // routes the same way.
    if block::is_author_flex(info.style) {
        return container_min_max(flat, index, space);
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

/// NGFlexLayoutAlgorithm::ComputeMinMaxSizes of the content box. A column
/// encompasses its items' contributions. A row adds up their max-content
/// contributions and the column gaps; its min-content size is the sum
/// too on one line, the largest item when it wraps.
pub(super) fn container_min_max(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
) -> Option<MinMax> {
    container_min_max_at(flat, index, space, None)
}

/// [`container_min_max`] for a container whose content block size is
/// already known (`block`: an absolutely positioned box stretched between
/// its insets, CSS2 10.6.4), as `height` gives it otherwise.
pub(super) fn container_min_max_at(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    block: Option<f32>,
) -> Option<MinMax> {
    let style = flat[index].style;
    let row = row_axis(style.flex_direction);
    let line = match block {
        Some(block) => Some(block),
        None => match definite(style.height, space.percentage_block, style, space)
            .why("flex:percent")?
        {
            Some(size) => {
                if [style.padding.top, style.padding.bottom]
                    .iter()
                    .any(|side| matches!(side, Spacing::Percent(_) | Spacing::PaddingCalc { .. }))
                {
                    return super::bail("flex:intrinsic-percent-padding");
                }
                Some(to_content(
                    style,
                    item_edges(style, space, 0.0, false),
                    size,
                ))
            }
            None => None,
        },
    };
    let multiline = style.flex_wrap != FlexWrap::NoWrap;
    let column_line = if !row && multiline {
        line.or(block::resolve_length(style.max_height, None, style, space)
            .map(|size| to_content(style, item_edges(style, space, 0.0, false), size)))
    } else {
        None
    };
    let mut sizes = MinMax::exact(0.0);
    let mut count = 0usize;
    // CSS Flexbox §9.9.1: a row whose items carry a definite `flex-basis`
    // sizes the container from the items' flex base sizes and factors.
    let mut spec_items: Vec<SpecItem> = Vec::new();
    let use_spec = row
        && rendered_children(flat, index).any(|child| {
            !oof::is_out_of_flow(flat[child].style)
                && matches!(flat[child].style.flex_basis, Dimension::Px(_))
        });
    for child in rendered_children(flat, index) {
        if oof::is_out_of_flow(flat[child].style) {
            continue;
        }
        let stretched =
            match stretched_image_main(flat, child, style, space, 0.0, line).why("flex:image")? {
                Some(main) => Some(main),
                None => stretched_ratio_main(flat, child, style, space, 0.0, line)
                    .why("flex:aspect-ratio")?,
            };
        let item = match stretched {
            Some(main) => {
                let child_style = flat[child].style;
                let margin = spacing_px(child_style, child_style.margin.left, 0.0, space)
                    + spacing_px(child_style, child_style.margin.right, 0.0, space);
                MinMax::exact(main + item_edges(child_style, space, 0.0, true) + margin)
            }
            None => block::min_max_contribution(flat, child, space).why("flex:intrinsic")?,
        };
        count += 1;
        if !row {
            sizes.encompass(item);
            continue;
        }
        if use_spec {
            spec_items.push(spec_item(flat, child, space, item, stretched.is_some())?);
        }
        sizes.max += item.max;
        sizes.min = sizes
            .min
            .zip(item.min)
            .map(|(a, b)| if multiline { a.max(b) } else { a + b });
    }
    if use_spec {
        let max_sum = spec_line_sum(&spec_items, false);
        sizes.max = max_sum;
        if !multiline {
            sizes.min = Some(spec_line_sum(&spec_items, true));
        }
    }
    if row && count > 1 {
        let gaps = style.column_gap.unwrap_or(style.gap).max(0.0) * (count - 1) as f32;
        sizes.max += gaps;
        if !multiline {
            sizes.min = sizes.min.map(|min| min + gaps);
        }
    }
    if let Some(main) = column_line
        && count > 0
    {
        // A column-wrap container's max-content size is the sum of its
        // flex lines' cross sizes, with the lines broken at its main size
        // (or max main size); its min-content size stays the largest
        // item contribution.
        sizes.max = column_wrap_max_content(flat, index, space, main)?.max(sizes.max);
    }
    Some(sizes)
}

/// One flex item as the intrinsic main size algorithm reads it (CSS
/// Flexbox §9.9.1). Sizes are outer (margin box) except `base`, `min`,
/// and `max`, which are content-box and share `outer_extra` with them.
struct SpecItem {
    min_contribution: f32,
    max_contribution: f32,
    base: f32,
    min: f32,
    max: f32,
    outer_extra: f32,
    grow: f32,
    shrink: f32,
}

fn spec_item(
    flat: &[FlatNodeInfo],
    child: usize,
    space: &ConstraintSpace,
    contribution: MinMax,
    fixed: bool,
) -> Option<SpecItem> {
    let style = flat[child].style;
    let edges = item_edges(style, space, 0.0, true);
    let margin = spacing_px(style, style.margin.left, 0.0, space)
        + spacing_px(style, style.margin.right, 0.0, space);
    let outer_extra = edges + margin;
    let max_contribution = contribution.max;
    let min_contribution = contribution.min.unwrap_or(max_contribution);
    if fixed {
        // A stretched replaced item's size is already its final one.
        let content = (max_contribution - outer_extra).max(0.0);
        return Some(SpecItem {
            min_contribution,
            max_contribution,
            base: content,
            min: content,
            max: content,
            outer_extra,
            grow: 0.0,
            shrink: 0.0,
        });
    }
    let base = flex_base(style, true, edges, None, space, flat, child, None)?;
    let (min, max) = resolve_main_limits(style, true, edges, None, space, flat, child)?;
    Some(SpecItem {
        min_contribution,
        max_contribution,
        base: base.max(0.0),
        min,
        max,
        outer_extra,
        grow: style.flex_grow.max(0.0),
        shrink: style.flex_shrink.max(0.0),
    })
}

/// The line's content-based size: each item takes its flex base size plus
/// its factor times the line's chosen flex fraction, clamped by its min and
/// max main sizes (Flexbox §9.9.1 steps 1-5). `min_content` reads the
/// min-content contributions instead of the max-content ones.
fn spec_line_sum(items: &[SpecItem], min_content: bool) -> f32 {
    let contribution = |item: &SpecItem| {
        if min_content {
            item.min_contribution
        } else {
            item.max_contribution
        }
    };
    let fraction = |item: &SpecItem| {
        let diff = contribution(item) - (item.base + item.outer_extra);
        if diff > 0.0 {
            if item.grow >= 1.0 {
                diff / item.grow
            } else {
                diff * item.grow
            }
        } else if diff < 0.0 {
            let scaled = item.shrink * item.base;
            if scaled == 0.0 {
                f32::NEG_INFINITY
            } else {
                diff / scaled
            }
        } else {
            0.0
        }
    };
    let mut chosen = items.iter().map(fraction).fold(f32::NEG_INFINITY, f32::max);
    let grow_sum: f32 = items.iter().map(|item| item.grow).sum();
    let shrink_sum: f32 = items.iter().map(|item| item.shrink).sum();
    if chosen > 0.0 && grow_sum < 1.0 && grow_sum > 0.0 {
        chosen /= grow_sum;
    } else if chosen < 0.0 && chosen.is_finite() && shrink_sum < 1.0 {
        chosen *= shrink_sum;
    }
    items
        .iter()
        .map(|item| {
            let factor = if chosen > 0.0 {
                item.grow
            } else {
                item.shrink * item.base
            };
            let delta = if factor == 0.0 { 0.0 } else { factor * chosen };
            let size = (item.base + delta).min(item.max).max(item.min);
            size + item.outer_extra
        })
        .sum()
}

/// The max-content inline size of a column-wrap flex container whose
/// content block size (or max block size) is `main`: items in `order`
/// are collected into flex lines by their hypothetical outer main sizes,
/// each line is as wide as its widest max-content contribution, and the
/// lines are separated by `column-gap` (CSS Flexbox §9.3 and §9.9.1).
/// Each item's content-based flex base size is measured at its
/// max-content contribution.
fn column_wrap_max_content(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    main: f32,
) -> Option<f32> {
    let style = flat[index].style;
    let row_gap = style.row_gap.unwrap_or(style.gap).max(0.0);
    let column_gap = style.column_gap.unwrap_or(style.gap).max(0.0);
    let mut children: Vec<usize> = rendered_children(flat, index)
        .filter(|&child| !oof::is_out_of_flow(flat[child].style))
        .collect();
    children.sort_by_key(|&child| flat[child].style.order);
    let mut total = 0.0_f32;
    let mut lines = 0usize;
    let (mut line_main, mut line_cross) = (0.0_f32, 0.0_f32);
    for child in children {
        let child_style = flat[child].style;
        let cross = block::min_max_contribution(flat, child, space)
            .why("flex:intrinsic")?
            .max
            .max(0.0);
        let main_edges = item_edges(child_style, space, 0.0, false);
        let cross_edges = item_edges(child_style, space, 0.0, true);
        let (start, end) = main_margin_sides(child_style, style.flex_direction);
        let main_margins =
            spacing_px(child_style, start, 0.0, space) + spacing_px(child_style, end, 0.0, space);
        let (cross_start, cross_end) = cross_margin_sides(child_style, style.flex_direction, false);
        let cross_margins = spacing_px(child_style, cross_start, 0.0, space)
            + spacing_px(child_style, cross_end, 0.0, space);
        let basis_auto = matches!(child_style.flex_basis, Dimension::Auto)
            && matches!(child_style.height, Dimension::Auto);
        let measured = if basis_auto {
            let width = (cross - cross_margins - cross_edges).max(0.0);
            let laid = layout_item(flat, child, space, width, None, false, None, Some(width))?;
            Some((laid.fragment.height - main_edges).max(0.0))
        } else {
            None
        };
        let base = flex_base(
            child_style,
            false,
            main_edges,
            Some(main),
            space,
            flat,
            child,
            measured,
        )
        .why("flex:basis")?
        .max(0.0);
        let (min, max) = resolve_main_limits(
            child_style,
            false,
            main_edges,
            Some(main),
            space,
            flat,
            child,
        )
        .why("flex:min")?;
        let outer = base.clamp(min, max.max(min)) + main_edges + main_margins;
        if lines > 0 && line_main + row_gap + outer > main + EPS {
            total += line_cross;
            (line_main, line_cross) = (outer, cross);
            lines += 1;
        } else if lines == 0 {
            (line_main, line_cross) = (outer, cross);
            lines = 1;
        } else {
            line_main += row_gap + outer;
            line_cross = line_cross.max(cross);
        }
    }
    total += line_cross;
    Some(total + column_gap * lines.saturating_sub(1) as f32)
}

/// The content-box inline size of an image row item in a single-line
/// container whose content block size `line` is definite, when the item
/// is stretched: its definite cross size is the line's (CSS Flexbox §9.8
/// item 1) and transfers through the aspect ratio. `Some(None)` when the
/// item is not such an image. Blink builds the intrinsic space with
/// `kStretchExplicit` for the same items (BuildSpaceForIntrinsicInlineSize).
fn stretched_image_main(
    flat: &[FlatNodeInfo],
    child: usize,
    container: &Style,
    space: &ConstraintSpace,
    inner_width: f32,
    line: Option<f32>,
) -> Option<Option<f32>> {
    if !matches!(flat[child].style.width, Dimension::Auto) {
        return Some(None);
    }
    match stretched_image_cross(flat, child, container, space, inner_width, line)? {
        Some(stretched) => block::replaced_inline_at_block(flat, child, space, stretched).map(Some),
        None => Some(None),
    }
}

/// The definite stretched content block size of an image row item in a
/// single-line container whose content block size `line` is definite
/// (CSS Flexbox §9.8 item 1), whatever its own `width`. `Some(None)` when
/// the item is not such an image.
fn stretched_image_cross(
    flat: &[FlatNodeInfo],
    child: usize,
    container: &Style,
    space: &ConstraintSpace,
    inner_width: f32,
    line: Option<f32>,
) -> Option<Option<f32>> {
    if !block::is_replaced(flat[child].kind) {
        return Some(None);
    }
    stretched_row_cross(flat, child, container, space, inner_width, line)
}

/// The content inline size of a non-replaced row item with a preferred
/// aspect ratio and auto width and height, stretched in a single line
/// whose content block size `line` is definite: the stretched block size
/// transfers through the ratio (CSS Flexbox §9.8 item 1; Blink
/// ComputeMinMaxInlineSizesFromAspectRatio), floored by its min-content
/// width when its automatic minimum applies. `Some(None)` when the item
/// is not such a box (aspect-ratio-intrinsic-size-005).
fn stretched_ratio_main(
    flat: &[FlatNodeInfo],
    child: usize,
    container: &Style,
    space: &ConstraintSpace,
    inner_width: f32,
    line: Option<f32>,
) -> Option<Option<f32>> {
    let style = flat[child].style;
    if block::is_replaced(flat[child].kind)
        || !matches!(style.width, Dimension::Auto)
        || block::marker(style, block::WIDTH_KEYWORD).is_some()
    {
        return Some(None);
    }
    let Some(ratio) = block::BoxAspectRatio::new(
        style,
        item_edges(style, space, inner_width, true),
        item_edges(style, space, inner_width, false),
    ) else {
        return Some(None);
    };
    let Some(stretched) = stretched_row_cross(flat, child, container, space, inner_width, line)?
    else {
        return Some(None);
    };
    let mut inline = ratio.inline_from_block(stretched);
    if matches!(style.min_width, Dimension::Auto) && !block::is_overflow_scroll_container(style) {
        let content = content_minmax(flat, child, space).why("flex:intrinsic")?;
        inline = inline.max(content.min.unwrap_or(content.max));
    }
    Some(Some(inline))
}

/// The definite stretched content block size of a row item in a
/// single-line container whose content block size `line` is definite
/// (CSS Flexbox §9.8 item 1). `Some(None)` when it is not stretched.
fn stretched_row_cross(
    flat: &[FlatNodeInfo],
    child: usize,
    container: &Style,
    space: &ConstraintSpace,
    inner_width: f32,
    line: Option<f32>,
) -> Option<Option<f32>> {
    let style = flat[child].style;
    let (start, end) = cross_margin_sides(style, container.flex_direction, false);
    let Some(line) = line else {
        return Some(None);
    };
    if !row_axis(container.flex_direction)
        || container.flex_wrap != FlexWrap::NoWrap
        || !matches!(style.height, Dimension::Auto)
        || used_align(container.align_items, style.align_self) != AlignItems::Stretch
        || matches!(start, Spacing::Auto)
        || matches!(end, Spacing::Auto)
    {
        return Some(None);
    }
    let edges = item_edges(style, space, inner_width, false);
    let margins =
        spacing_px(style, start, inner_width, space) + spacing_px(style, end, inner_width, space);
    let stretched = clamp_cross(
        style,
        true,
        edges,
        line - margins - edges,
        Some(line),
        space,
    )
    .why("flex:cross")?;
    Some(Some(stretched))
}

/// The max-content inline size of a column-wrap flex item stretched in a
/// single-line row container with a definite cross size: its flex lines
/// break at that stretched block size (CSS Flexbox §9.8 treats it as
/// definite), as in Chrome (col-wrap-009).
fn stretched_column_wrap_max(
    flat: &[FlatNodeInfo],
    child: usize,
    container: &Style,
    space: &ConstraintSpace,
    inner_width: f32,
    line: Option<f32>,
) -> Option<Option<f32>> {
    let style = flat[child].style;
    let (start, end) = cross_margin_sides(style, container.flex_direction, false);
    let Some(line) = line else {
        return Some(None);
    };
    if !row_axis(container.flex_direction)
        || container.flex_wrap != FlexWrap::NoWrap
        || !block::is_author_flex(style)
        || row_axis(style.flex_direction)
        || style.flex_wrap == FlexWrap::NoWrap
        || !matches!(style.flex_basis, Dimension::Auto)
        || !matches!(style.width, Dimension::Auto)
        || !matches!(style.height, Dimension::Auto)
        || used_align(container.align_items, style.align_self) != AlignItems::Stretch
        || matches!(start, Spacing::Auto)
        || matches!(end, Spacing::Auto)
    {
        return Some(None);
    }
    let edges = item_edges(style, space, inner_width, false);
    let margins =
        spacing_px(style, start, inner_width, space) + spacing_px(style, end, inner_width, space);
    let stretched = clamp_cross(
        style,
        true,
        edges,
        line - margins - edges,
        Some(line),
        space,
    )
    .why("flex:cross")?;
    container_min_max_at(flat, child, space, Some(stretched)).map(|sizes| Some(sizes.max))
}

/// Fit-content inline size of a column item (CSS Sizing 3 §5.2.1):
/// `clamp(min-content, available, max-content)`. `available` is the
/// container's inner width minus this item's cross margins and edges.
fn fit_content_size(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    available: f32,
) -> Option<f32> {
    let sizes = block::intrinsic_sizes(flat, index, space).why("flex:wrap-column-auto-cross")?;
    let min = sizes.min.unwrap_or(sizes.max).max(0.0);
    let max = sizes.max.max(min);
    Some(max.min(min.max(available.max(0.0))))
}

/// `aspect-ratio` transferred through a definite width, as a content-box
/// block size. The marker is a width/height ratio. Legacy layout ignores it.
fn column_aspect_main(style: &Style, content_width: f32) -> Option<f32> {
    if !matches!(style.height, Dimension::Auto) || matches!(style.width, Dimension::Auto) {
        return None;
    }
    let ratio = style
        .custom_properties
        .as_ref()?
        .get("--w3cos-internal-aspect-ratio")?
        .parse::<f32>()
        .ok()?;
    if !ratio.is_finite() || ratio <= 0.0 {
        return None;
    }
    Some((content_width / ratio).max(0.0))
}

/// Main-size limits of a column item after its intrinsic `min-height` /
/// `max-height` keywords, which resolve to its auto-height content block
/// size (all three keywords mean that in the block axis). The max is
/// applied first and the min wins. When the item has a non-auto height
/// (its content size would need a separate layout), the limits are kept.
#[allow(clippy::too_many_arguments)]
fn column_keyword_limits(
    flat: &[FlatNodeInfo],
    child: usize,
    row: bool,
    space: &ConstraintSpace,
    inner_width: f32,
    specified_height: Option<f32>,
    cross: Option<f32>,
    measured: Option<f32>,
    (min, max): (f32, f32),
) -> Option<(f32, f32)> {
    let style = flat[child].style;
    let min_kw = block::marker(style, block::MIN_HEIGHT_KEYWORD).is_some();
    let max_kw = block::marker(style, block::MAX_HEIGHT_KEYWORD).is_some();
    if row || block::is_replaced(flat[child].kind) || !(min_kw || max_kw) {
        return Some((min, max));
    }
    if !matches!(style.height, Dimension::Auto) {
        return Some((min, max));
    }
    let content = match measured {
        Some(height) => height,
        None => {
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
            let edges = item_edges(style, space, inner_width, false);
            (laid.fragment.height - edges).max(0.0)
        }
    };
    let mut limits = (min, max);
    if max_kw {
        limits.1 = limits.1.min(content);
    }
    if min_kw {
        limits.0 = limits.0.max(content);
    }
    limits.1 = limits.1.max(limits.0);
    Some(limits)
}

/// Margin-box inline contribution of one in-flow child.
fn inline_contribution(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
) -> Option<MinMax> {
    let style = flat[index].style;
    // Intrinsic width keywords (and min-/max-width keywords) clamp the
    // contribution; the block contribution resolves them
    // (flex-item-content-is-min-width-max-content).
    if [
        block::WIDTH_KEYWORD,
        block::MIN_WIDTH_KEYWORD,
        block::MAX_WIDTH_KEYWORD,
    ]
    .iter()
    .any(|name| block::marker(style, name).is_some())
    {
        return block::min_max_contribution(flat, index, space);
    }
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
    if matches!(info.style.display, Display::Table | Display::InlineTable) {
        // Row heights, not the sum of the cells in one row.
        let sizes = block::intrinsic_sizes(flat, index, space).why("flex:intrinsic")?;
        let laid = super::table::layout_with_block_size(
            flat,
            index,
            space,
            Some((sizes.max.max(0.0), None)),
            false,
        )
        .why("flex:table")?;
        let border = block::border_widths(info.style);
        let pad = |spacing| spacing_px(info.style, spacing, 0.0, space);
        let edges =
            border[0] + border[2] + pad(info.style.padding.top) + pad(info.style.padding.bottom);
        return Some((laid.fragment.height - edges).max(0.0));
    }
    if block::size_contained(info.style) {
        return Some(block::contain_intrinsic_size(info.style).1);
    }
    if let w3cos_std::ComponentKind::Text { .. } = info.kind {
        let line = info.style.font_size * info.style.line_height.max(1.0);
        return Some(line.max(0.0));
    }
    if block::is_replaced(info.kind) {
        // Natural block size; a column item's is transferred from its used
        // width in `layout`.
        let laid = block::layout_block_sized(flat, index, space, false, None).why("flex:image")?;
        let border = block::border_widths(info.style);
        let pad = |spacing| spacing_px(info.style, spacing, 0.0, space);
        let edges =
            border[0] + border[2] + pad(info.style.padding.top) + pad(info.style.padding.bottom);
        return Some((laid.fragment.height - edges).max(0.0));
    }
    let mut sum = 0.0;
    // Adjacent inline text nodes share a line (`XXX` followed by a
    // whitespace node is one line, not two).
    let mut after_inline_text = false;
    for child in rendered_children(flat, index) {
        if oof::is_out_of_flow(flat[child].style) || flat[child].style.float != Float::None {
            continue;
        }
        let inline_text = matches!(flat[child].kind, w3cos_std::ComponentKind::Text { .. })
            && flat[child].style.display == Display::Inline;
        if !(inline_text && after_inline_text) {
            sum += block_contribution(flat, child, space).why("flex:block-min")?;
        }
        after_inline_text = inline_text;
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
    // A length `min-height` / `max-height` bounds the box's block size
    // (flex-minimum-height-flex-items-019).
    let inner = if let Dimension::Px(min) = style.min_height {
        inner.max(to_content(style, edges, min))
    } else {
        inner
    };
    let inner = if let Dimension::Px(max) = style.max_height {
        inner.min(to_content(style, edges, max))
    } else {
        inner
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
    let min = if matches!(min_dim, Dimension::Auto) && block::is_overflow_scroll_container(style) {
        // CSS Flexbox §4.5: the automatic minimum size applies only to items
        // that are not scroll containers; for those `min-*: auto` is 0
        // (FlexLayoutAlgorithm::ShouldApplyMinSizeAutoForChild).
        0.0
    } else if matches!(min_dim, Dimension::Auto) {
        let (main_dim, cross_dim) = if row {
            (style.width, style.height)
        } else {
            (style.height, style.width)
        };
        let natural_replaced = matches!(
            flat[index].kind,
            w3cos_std::ComponentKind::Image { .. }
                | w3cos_std::ComponentKind::Canvas { .. }
                | w3cos_std::ComponentKind::SvgDocument { .. }
        ) && !matches!(main_dim, Dimension::Auto)
            && !matches!(cross_dim, Dimension::Percent(_) | Dimension::Calc { .. });
        let content_min = if natural_replaced {
            // A replaced item's min-content size is its natural size (its
            // own main size read as auto), with a definite cross size
            // transferred through its aspect ratio; the specified size
            // suggestion caps it below (flexbox-min-width-auto-002a,
            // flexbox-min-height-auto-002a).
            let mut natural = style.clone();
            if row {
                natural.width = Dimension::Auto;
            } else {
                natural.height = Dimension::Auto;
            }
            let size = block::replaced_size(flat[index].kind, &natural, space)?;
            if row { size.0 } else { size.1 }
        } else if row {
            let content = content_minmax(flat, index, space).why("flex:intrinsic")?;
            content.min.unwrap_or(content.max)
        } else if block::size_contained(style) {
            block::contain_intrinsic_size(style).1
        } else {
            block_content_min(flat, index, space).why("flex:block-min")?
        }
        .max(0.0);
        let suggestion = match definite(specified, inner, style, space).why("flex:percent")? {
            Some(size) => content_min.min(to_content(style, edges, size)),
            None => content_min,
        };
        // The content-based minimum size is further clamped by a definite
        // max main size (CSS Flexbox §4.5; flexbox-min-width-auto-001).
        match definite(max_dim, inner, style, space).why("flex:percent")? {
            Some(size) => suggestion.min(to_content(style, edges, size)),
            None => suggestion,
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
    // A percentage flex-basis against an indefinite container main size
    // behaves as `content` (css-flexbox §7.2.1; percentage-heights-017).
    let resolved = if !row && inner.is_none() && matches!(basis, Dimension::Percent(_)) {
        Some(None)
    } else {
        definite(basis, inner, style, space)
    };
    match resolved.why("flex:percent")? {
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

/// Leading offset and extra space between lines for `align-content`.
/// Stretch has already grown the lines; negative free space makes the
/// distributed values fall back to flex-start (space-between) or center.
fn align_lines(kind: AlignContent, free: f32, count: usize) -> (f32, f32) {
    let n = count.max(1) as f32;
    match kind {
        AlignContent::FlexStart | AlignContent::Stretch => (0.0, 0.0),
        AlignContent::FlexEnd => (free, 0.0),
        AlignContent::Center => (free / 2.0, 0.0),
        AlignContent::SpaceBetween if free > 0.0 && count > 1 => (0.0, free / (n - 1.0)),
        AlignContent::SpaceBetween => (0.0, 0.0),
        AlignContent::SpaceAround if free > 0.0 => (free / n / 2.0, free / n),
        AlignContent::SpaceEvenly if free > 0.0 => (free / (n + 1.0), free / (n + 1.0)),
        AlignContent::SpaceAround | AlignContent::SpaceEvenly => (free / 2.0, 0.0),
    }
}

/// `justify-content: safe <position>`.
fn safe_justify(style: &Style) -> bool {
    style
        .custom_properties
        .as_ref()
        .and_then(|props| props.get("--w3cos-internal-justify-content"))
        .is_some_and(|value| value.split_ascii_whitespace().any(|token| token == "safe"))
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

/// A text run is an anonymous block flex item (CSS Flexbox §4) whose line
/// box sits at its top, however far the item stretches or flexes. The run
/// has no box of its own, so the node takes the rect paint reads for a
/// paragraph leaf (`inline::paragraph_box`): the font content box of a
/// single line, else its lines from the upper leading.
fn text_item_paint_box(
    flat: &[FlatNodeInfo],
    item: usize,
    container: usize,
    fragment: BoxFragment,
) -> (f32, BoxFragment) {
    let style = flat[item].style;
    let w3cos_std::ComponentKind::Text { content } = flat[item].kind else {
        return (0.0, fragment);
    };
    if style.display != Display::Inline {
        // An element lowered to a text leaf keeps its own box.
        return (0.0, fragment);
    }
    let (_, _, _, y, height) = super::inline::paragraph_box(
        content,
        fragment.width,
        style,
        flat[container].style,
        super::inline::quirks_mode(flat),
    );
    (y, BoxFragment { height, ..fragment })
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
    // Blink `IsFixedBlockSizeIndefinite`: a column item's flexed size is not
    // a percentage basis when the container height is indefinite, unless the
    // size is definite without layout: a fully inflexible item with a length
    // basis (percentage-heights-007), or one whose height transfers from an
    // aspect ratio (flex-aspect-ratio-032).
    let style = flat[index].style;
    let inflexible_length = style.flex_grow <= 0.0
        && style.flex_shrink <= 0.0
        && matches!(style.flex_basis, Dimension::Px(_));
    nested.fixed_block_indefinite = !row
        && container_inner_height.is_none()
        && !inflexible_length
        && !has_marker(style, "--w3cos-internal-aspect-ratio");
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
    } else {
        // Intrinsic `width` / `min-width` / `max-width` keywords resolve
        // against the container's own min/max-content sizes
        // (container_min_max_at), as for a block.
        metrics = block::apply_width_keywords(flat, index, space, metrics)?;
    }
    let direction = style.flex_direction;
    let row = row_axis(direction);
    let reverse = reverse_axis(direction);
    let [bt, br, bb, bl] = metrics.border;
    let [pt, pr, pb, pl] = metrics.padding;
    let inner_width = metrics.content_width.max(0.0);
    let to_content_h = |size: f32| to_content(style, bt + bb + pt + pb, size);
    let mut specified_height = if let Some((_, Some(height))) = sized {
        Some(height)
    } else {
        match definite(style.height, space.percentage_block, style, space).why("flex:percent")? {
            Some(size) => Some(to_content_h(size)),
            None => None,
        }
    };
    // `max-height` is the line-breaking limit when the block size is auto,
    // but the used block size is the flexed line (Chrome). `aspect-ratio`
    // transfers a definite width into a definite block size.
    let column_wrap = !row && style.flex_wrap != FlexWrap::NoWrap;
    let max_main = if column_wrap {
        block::resolve_length(style.max_height, space.percentage_block, style, space)
            .map(|size| to_content_h(size))
    } else {
        None
    };
    if specified_height.is_none()
        && column_wrap
        && let Some(aspect) = column_aspect_main(style, inner_width)
    {
        specified_height = Some(max_main.map(|max| aspect.min(max)).unwrap_or(aspect));
    }
    let (inner_main, main_is_max) = if row {
        (Some(inner_width), false)
    } else if let Some(height) = specified_height {
        (Some(height), false)
    } else if let Some(max) = max_main {
        (Some(max), true)
    } else {
        (None, false)
    };
    let gap = if row {
        style.column_gap.unwrap_or(style.gap)
    } else {
        style.row_gap.unwrap_or(style.gap)
    }
    .max(0.0);

    // CSS Sizing 4 stretch-fit sizing: an item's `stretch` sizes resolve
    // against this container's content box (as for block children).
    let item_space = ConstraintSpace {
        available_inline: inner_width,
        percentage_block: specified_height,
        ..*space
    };
    let stretch_styles = block::stretch_child_styles(flat, index, &item_space, false, false);
    let stretched_flat;
    let flat = if stretch_styles.is_empty() {
        flat
    } else {
        stretched_flat = block::flat_with_styles(flat, &stretch_styles);
        &stretched_flat[..]
    };

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
        let (cross_start, cross_end) = cross_margin_sides(
            child_style,
            direction,
            style.flex_wrap == FlexWrap::WrapReverse,
        );
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
                // The used cross size honors the cross min/max sizes
                // (Blink ComputeInlineSizeForFragment / ComputeBlockSize
                // clamp a fixed size too).
                Some(size) => Some(
                    clamp_cross(
                        child_style,
                        row,
                        cross_edges,
                        to_content(child_style, cross_edges, size),
                        cross_basis,
                        space,
                    )
                    .why("flex:cross")?,
                ),
                None => None,
            };
        // A column item's cross size is its width. Nowrap stretch uses the
        // container's inner width. A wrap line's hypothetical cross size is
        // fit-content; `cross_auto` stays set so the line can still stretch
        // the item after align-content grows the line.
        if !row && cross.is_none() {
            let align = used_align(style.align_items, child_style.align_self);
            let available =
                (inner_width - margin_cross_start - margin_cross_end - cross_edges).max(0.0);
            // A single-line item that does not stretch is fit-content too
            // (CSS Flexbox §9.4 hypothetical cross size).
            // A replaced item sizes as a replaced box in the line's inline
            // size, so a ratio-only image fills it (CSS 2.1 §10.3.2).
            let raw = if !column_wrap
                && align != AlignItems::Stretch
                && block::is_replaced(flat[child].kind)
            {
                let line = ConstraintSpace {
                    available_inline: available,
                    ..*space
                };
                block::replaced_size(flat[child].kind, child_style, &line)?.0
            } else if column_wrap || align != AlignItems::Stretch {
                fit_content_size(flat, child, space, available)
                    .why("flex:wrap-column-auto-cross")?
            } else {
                available
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
        let basis_auto = (matches!(child_style.flex_basis, Dimension::Auto)
            && matches!(child_style.height, Dimension::Auto))
            // A percentage basis with an indefinite main size is `content`.
            || (inner_main.is_none()
                && matches!(child_style.flex_basis, Dimension::Percent(_)));
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
        // A column text item's min-content block size is its laid-out
        // height at its used width, not one line (the anonymous block's
        // content size, FlexLayoutAlgorithm::ComputeMinMainSize).
        // A replaced column item's content size suggestion is its block
        // size transferred from its used width through the aspect ratio,
        // capped by its max main size (CSS Flexbox §4.5).
        let min = match (flat[child].kind, measured) {
            (w3cos_std::ComponentKind::Text { .. }, Some(height))
                if !row && matches!(child_style.min_height, Dimension::Auto) =>
            {
                min.max(height)
            }
            (kind, Some(height))
                if block::is_replaced(kind)
                    && !row
                    && matches!(child_style.min_height, Dimension::Auto)
                    && !block::is_overflow_scroll_container(child_style) =>
            {
                height.min(max)
            }
            _ => min,
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
        let mut base = base.max(0.0);
        // A replaced row item stretched in a single line with a definite
        // cross size has that definite cross size (CSS Flexbox §9.8), so its
        // content-based flex base size and its content size suggestion are
        // transferred from it through the aspect ratio (§9.2.3 B, §4.5).
        let mut min = min;
        if let Some(transferred) =
            stretched_ratio_main(flat, child, style, space, inner_width, specified_height)
                .why("flex:aspect-ratio")?
        {
            if matches!(child_style.flex_basis, Dimension::Auto) {
                base = transferred;
            }
            if matches!(child_style.min_width, Dimension::Auto)
                && !block::is_overflow_scroll_container(child_style)
            {
                min = transferred.min(max);
            }
        } else if let Some(transferred) =
            stretched_image_main(flat, child, style, space, inner_width, specified_height)
                .why("flex:image")?
        {
            if matches!(child_style.flex_basis, Dimension::Auto) {
                base = transferred;
            }
            if matches!(child_style.min_width, Dimension::Auto)
                && !block::is_overflow_scroll_container(child_style)
            {
                min = transferred.min(max);
            }
        } else if matches!(child_style.min_width, Dimension::Auto)
            && !block::is_overflow_scroll_container(child_style)
        {
            // With a specified width the content size suggestion is still
            // the image's min-content width at the stretched height, and
            // the specified size suggestion caps it (§4.5).
            if let Some(stretched) =
                stretched_image_cross(flat, child, style, space, inner_width, specified_height)
                    .why("flex:image")?
            {
                let content =
                    block::replaced_content_inline_at_block(flat, child, space, stretched)
                        .why("flex:image")?;
                let specified = match definite(child_style.width, inner_main, child_style, space)
                    .why("flex:percent")?
                {
                    Some(size) => content.min(to_content(child_style, main_edges, size)),
                    None => content,
                };
                min = specified.min(max);
            }
        }
        if let Some(stretched) =
            stretched_column_wrap_max(flat, child, style, space, inner_width, specified_height)
                .why("flex:column-wrap-item")?
        {
            base = stretched;
        }
        // Intrinsic min/max-height keywords on a column item are its
        // content block size at its used width (css-sizing-3): the max
        // caps, the min floors its main size
        // (flex-item-max-height-min-content).
        let (min, max) = column_keyword_limits(
            flat,
            child,
            row,
            space,
            inner_width,
            specified_height,
            cross,
            measured,
            (min, max),
        )
        .why("flex:keyword-limit")?;
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

    // A definite main size, or a max-height used as that limit, is what
    // makes a column wrap (Flexbox §9.2). An automatic block size with
    // neither is one line and stays on the single-line path below.
    if style.flex_wrap != FlexWrap::NoWrap
        && let Some(inner) = inner_main
    {
        return layout_wrapped(
            flat,
            index,
            space,
            style,
            &metrics,
            &mut items,
            inner_width,
            specified_height,
            inner,
            main_is_max,
            gap,
        );
    }

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
    // A nowrap container's single line fills a definite cross size
    // whatever align-content says (CSS Flexbox §9.4 step 15). An automatic
    // wrap column takes this path too; there only stretch and flex-start
    // are handled.
    let nowrap = style.flex_wrap == FlexWrap::NoWrap;
    if !nowrap
        && !matches!(
            style.align_content,
            AlignContent::Stretch | AlignContent::FlexStart
        )
    {
        return super::bail("flex:align-content");
    }
    let definite_line = if nowrap || style.align_content == AlignContent::Stretch {
        content_cross_definite
    } else {
        None
    };
    // Baseline participants (FlexLayoutAlgorithm::BuildFlexLines): the
    // line's baseline size is the largest ascent plus the largest descent,
    // unless the single line has a definite cross size.
    let mut ascents = vec![None; items.len()];
    let mut max_ascent = None::<f32>;
    let mut max_descent = 0.0_f32;
    for (slot, item) in ascents.iter_mut().zip(&items) {
        if item.align != AlignItems::Baseline {
            continue;
        }
        if !row {
            return super::bail("flex:baseline-column");
        }
        let laid = layout_item(
            flat,
            item.index,
            space,
            inner_width,
            specified_height,
            true,
            Some(item.content),
            item.cross,
        )?;
        let ascent = baseline_ascent(item, &laid).why("flex:baseline-unknown")?;
        let margin_box = laid.fragment.height + item.margin_cross_start + item.margin_cross_end;
        *slot = Some(ascent);
        max_ascent = Some(max_ascent.map_or(ascent, |max| max.max(ascent)));
        max_descent = max_descent.max(margin_box - ascent);
    }
    let baseline_extent = max_ascent.map_or(0.0, |ascent| ascent + max_descent);
    let line_inner = definite_line.unwrap_or(max_margin_box.max(baseline_extent));

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

    // `wrap-reverse` on an automatic column packs the single line at the
    // cross end (the right). A safe overflow stays at the cross start.
    let column_wrap_reverse = !row && style.flex_wrap == FlexWrap::WrapReverse;
    let safe_cross = has_marker(style, "--w3cos-internal-safe-align-content");
    let reverse_cross = column_wrap_reverse && !(safe_cross && max_margin_box > inner_width + EPS);
    let line_origin = if reverse_cross {
        (inner_width - line_inner).max(0.0)
    } else {
        0.0
    };
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
    if free < -EPS && safe_justify(style) {
        // `safe` overflow aligns as `start` (CSS Align §4.4): the physical
        // start edge, the flex-end side under a reversed main axis.
        lead = if reverse { free } else { 0.0 };
    }
    if reverse {
        lead = main_inner - lead;
    }
    let mut cursor = lead;
    let mut children = Vec::with_capacity(items.len());
    let mut oof = Vec::new();
    let mut fallback_baseline = None;
    for ((item, laid), ascent) in items.iter().zip(fragments).zip(&ascents) {
        let border_main = item.content + item.main_edges;
        let main = if reverse {
            cursor - item.margin_main_start - border_main
        } else {
            cursor + item.margin_main_start
        };
        let main = unit(main);
        let margin_box = item.margin_box_cross();
        // Unsafe alignment (FlexItem::AlignmentOffset): an item larger than
        // its line overflows both sides when centered, the start when
        // flex-end. A definite cross size does not stretch; it aligns as
        // flex-start.
        let extra = line_inner - margin_box;
        let from_start = match item.align {
            AlignItems::FlexEnd => extra,
            AlignItems::Center => extra / 2.0,
            AlignItems::Baseline => {
                max_ascent.unwrap_or(0.0) - ascent.unwrap_or(item.margin_cross_start)
            }
            AlignItems::Stretch | AlignItems::FlexStart => 0.0,
        };
        let cross = if reverse_cross {
            let border_cross = item.cross.unwrap_or(0.0) + item.cross_edges;
            line_origin + line_inner - from_start - item.margin_cross_start - border_cross
        } else {
            item.margin_cross_start + from_start
        };
        let cross = unit(cross);
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
        // BaselineAccumulator: the fallback is the first item of the line
        // after ApplyReversals (the last in order for a reversed row),
        // offset by its position without the relative offset.
        let item_baseline = match laid.first_baseline {
            LastBaseline::NoLines => LastBaseline::At(laid.fragment.height),
            other => other,
        }
        .offset(y);
        if reverse {
            fallback_baseline = Some(item_baseline);
        } else {
            fallback_baseline.get_or_insert(item_baseline);
        }
        let (paint_dy, fragment) = text_item_paint_box(flat, item.index, index, laid.fragment);
        children.push((x + dx, y + dy + paint_dy, fragment));
        let step = item.outer_main() + gap + between;
        if reverse {
            cursor -= step;
        } else {
            cursor += step;
        }
    }

    // A row's major baseline is the line's largest ascent; otherwise the
    // fallback item's baseline (FlexLayoutAlgorithm FirstBaseline). A
    // column keeps reporting none, as before.
    let container_baseline = match (row, max_ascent) {
        (true, Some(ascent)) => LastBaseline::At(bt + pt + ascent),
        (true, None) => fallback_baseline.unwrap_or(LastBaseline::NoLines),
        (false, _) => LastBaseline::NoLines,
    };
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
            ellipsis: false,
            children,
        },
        last_baseline: container_baseline,
        first_baseline: container_baseline,
        start_strut,
        end_strut,
        self_collapsing: false,
        oof,
        bfc_dependent: false,
        float_overlap: false,
    })
}

/// Flex lines from hypothetical outer main sizes (Flexbox §9.3). A line
/// keeps at least one item, even when that item is wider than the container.
/// Marker the NG entry sets on a flex container whose `flex-wrap` is
/// `balance` (with its wrap direction already stored in `flex_wrap`).
pub(super) const BALANCE_MARKER: &str = "--w3cos-internal-flex-balance";

/// CSS Flexbox 2 §9.7 balanced lines. The line count is the greedy break's.
/// The items split into that many contiguous sequences, each at most the
/// inner main size unless it is a single item, minimizing the sum of the
/// squared differences between a line's size (items and gaps) and the inner
/// main size. Ties prefer more items on the first line, then the second, ...
/// Each line still flexes against the real inner size afterwards.
fn balanced_line_ranges(items: &[Item], inner: f32, gap: f32) -> Vec<(usize, usize)> {
    let greedy = flex_line_ranges(items, inner, gap);
    let lines = greedy.len();
    let count = items.len();
    if lines <= 1 {
        return greedy;
    }
    let size = |start: usize, end: usize| -> f64 {
        let sum: f64 = items[start..end]
            .iter()
            .map(|i| f64::from(i.outer_main().max(0.0)))
            .sum();
        sum + f64::from(gap) * (end - start - 1) as f64
    };
    let fits =
        |start: usize, end: usize| end - start == 1 || size(start, end) <= f64::from(inner) + 1e-4;
    // best[j][i]: least error placing items i.. on j lines (INFINITY: none).
    let mut best = vec![vec![f64::INFINITY; count + 1]; lines + 1];
    best[0][count] = 0.0;
    for j in 1..=lines {
        for start in 0..count {
            for end in start + 1..=count {
                if !fits(start, end) || !best[j - 1][end].is_finite() {
                    continue;
                }
                let error = (f64::from(inner) - size(start, end)).powi(2) + best[j - 1][end];
                if error < best[j][start] {
                    best[j][start] = error;
                }
            }
        }
    }
    if !best[lines][0].is_finite() {
        return greedy;
    }
    let mut ranges = Vec::with_capacity(lines);
    let mut start = 0;
    for j in (1..=lines).rev() {
        // The largest end that still reaches the minimum (start bias).
        let target = best[j][start];
        let mut chosen = None;
        for end in (start + 1..=count).rev() {
            if fits(start, end)
                && best[j - 1][end].is_finite()
                && ((f64::from(inner) - size(start, end)).powi(2) + best[j - 1][end] - target).abs()
                    <= 1e-6 * target.max(1.0)
            {
                chosen = Some(end);
                break;
            }
        }
        let Some(end) = chosen else {
            return greedy;
        };
        ranges.push((start, end));
        start = end;
    }
    ranges
}

fn flex_line_ranges(items: &[Item], inner: f32, gap: f32) -> Vec<(usize, usize)> {
    if items.is_empty() {
        return vec![(0, 0)];
    }
    let mut lines = Vec::new();
    let mut start = 0;
    let mut used = 0.0;
    for (i, item) in items.iter().enumerate() {
        let outer = item.outer_main();
        if i > start && used + gap + outer > inner + EPS {
            lines.push((start, i));
            start = i;
            used = outer;
        } else {
            if i > start {
                used += gap;
            }
            used += outer;
        }
    }
    lines.push((start, items.len()));
    lines
}

/// `flex-wrap: wrap` and `wrap-reverse` with a definite main size.
/// Each line flexes against the container's inner main size. `align-content:
/// stretch` gives leftover cross space to the lines; `flex-start` packs them
/// at the cross start. `wrap-reverse` has already swapped cross-start to the
/// far cross edge, so the first line and `flex-start` items sit there.
/// A column's cross axis is horizontal: lines are columns.
fn layout_wrapped(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    style: &Style,
    metrics: &BoxMetrics,
    items: &mut [Item],
    inner_width: f32,
    specified_height: Option<f32>,
    inner_main: f32,
    main_is_max: bool,
    main_gap: f32,
) -> Option<LayoutResult> {
    let direction = style.flex_direction;
    let row = row_axis(direction);
    // Column baseline groups stay on legacy. `container_reason` already
    // rejects them; this keeps a later caller from painting those items
    // at the cross start.
    if !row && items.iter().any(|item| item.align == AlignItems::Baseline) {
        return super::bail("flex:baseline-column");
    }
    let reverse = reverse_axis(direction);
    let wrap_reverse = style.flex_wrap == FlexWrap::WrapReverse;
    let [bt, br, bb, bl] = metrics.border;
    let [pt, pr, pb, pl] = metrics.padding;
    let cross_gap = (if row {
        style.row_gap.unwrap_or(style.gap)
    } else {
        style.column_gap.unwrap_or(style.gap)
    })
    .max(0.0);
    let lines = if has_marker(style, BALANCE_MARKER) {
        balanced_line_ranges(items, inner_main, main_gap)
    } else {
        flex_line_ranges(items, inner_main, main_gap)
    };
    for &(start, end) in &lines {
        let line = &mut items[start..end];
        let gap_sum = main_gap * line.len().saturating_sub(1) as f32;
        if !line.is_empty() {
            distribute(line, inner_main, gap_sum);
            snap_to_inner(line, inner_main, gap_sum);
        }
    }
    if row {
        for item in items.iter_mut() {
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
    // Per-line baseline groups (FlexLayoutAlgorithm::BuildFlexLines). Each
    // line's baseline size is its largest ascent plus its largest descent.
    // Under `wrap-reverse` the group is the minor one (DetermineBaselineGroup
    // with is_flipped): BaselineAscent measures from the cross end, which is
    // where `margin_cross_start` already sits here.
    let mut ascents = vec![None::<f32>; items.len()];
    let mut line_ascent = vec![None::<f32>; lines.len()];
    let mut line_cross: Vec<f32> = Vec::with_capacity(lines.len());
    for (line_index, &(start, end)) in lines.iter().enumerate() {
        let mut size = 0.0_f32;
        let mut max_ascent = None::<f32>;
        let mut max_descent = 0.0_f32;
        for (offset, item) in items[start..end].iter().enumerate() {
            size = size.max(item.margin_box_cross());
            if item.align != AlignItems::Baseline {
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
                item.cross,
            )?;
            let baseline = match laid.first_baseline {
                LastBaseline::At(baseline) => baseline,
                LastBaseline::NoLines => laid.fragment.height,
                LastBaseline::Unknown => return super::bail("flex:baseline-unknown"),
            };
            let from_cross_start = if wrap_reverse {
                laid.fragment.height - baseline
            } else {
                baseline
            };
            let ascent = item.margin_cross_start + from_cross_start;
            let margin_box = laid.fragment.height + item.margin_cross_start + item.margin_cross_end;
            ascents[start + offset] = Some(ascent);
            max_ascent = Some(max_ascent.map_or(ascent, |max| max.max(ascent)));
            max_descent = max_descent.max(margin_box - ascent);
        }
        if let Some(ascent) = max_ascent {
            size = size.max(ascent + max_descent);
        }
        line_ascent[line_index] = max_ascent;
        line_cross.push(size);
    }
    if line_cross.is_empty() {
        line_cross.push(0.0);
    }
    let cross_gap_sum = cross_gap * line_cross.len().saturating_sub(1) as f32;
    let packed = line_cross.iter().copied().sum::<f32>() + cross_gap_sum;
    let definite_cross = if row {
        specified_height
    } else {
        Some(inner_width)
    };
    if style.align_content == AlignContent::Stretch
        && let Some(definite) = definite_cross
    {
        let extra = definite - packed;
        if extra > EPS && !line_cross.is_empty() {
            let share = extra / line_cross.len() as f32;
            for size in &mut line_cross {
                *size += share;
            }
        }
    }
    for (line_index, &(start, end)) in lines.iter().enumerate() {
        let line_inner = line_cross[line_index];
        for item in &mut items[start..end] {
            if item.align == AlignItems::Stretch && item.cross_auto {
                let raw = (line_inner
                    - item.margin_cross_start
                    - item.margin_cross_end
                    - item.cross_edges)
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
    }
    let mut fragments = Vec::with_capacity(items.len());
    for item in items.iter() {
        fragments.push(Some(layout_item(
            flat,
            item.index,
            space,
            inner_width,
            specified_height,
            row,
            Some(item.content),
            item.cross,
        )?));
    }
    let used_cross = if row {
        specified_height.unwrap_or(packed)
    } else {
        inner_width
    };
    // `safe` overflow alignment uses `start` instead of the flex cross-start
    // (CSS Align §4.4). For a row that is the physical top, so an overflowing
    // wrap-reverse line stays at y=0 and overflows downward.
    let safe_cross = has_marker(style, "--w3cos-internal-safe-align-content");
    let cross_overflows = definite_cross.is_some_and(|size| packed > size + EPS);
    let reverse_cross = wrap_reverse && !(safe_cross && cross_overflows);
    // A safe overflow keeps `wrap-reverse` lines on the physical start
    // while the minor baseline group still measures from the cross end.
    if wrap_reverse && !reverse_cross && ascents.iter().any(Option::is_some) {
        return super::bail("flex:baseline-wrap-safe");
    }
    // BaselineAccumulator over the first line after ApplyReversals, which
    // is the physically top line: with `wrap-reverse` that is the last
    // line here. Its major (or, flipped, minor) baseline, else its first
    // item after reversals (the last in order for a reversed row).
    let mut container_baseline = None::<LastBaseline>;
    let mut children = Vec::with_capacity(items.len());
    let mut oof_out = Vec::new();
    let mut frag_at = 0;
    // align-content packs the lines in the free cross space (CSS Align
    // §5.3), with the distributed values falling back when it is negative.
    let cross_free = definite_cross.map_or(0.0, |size| size - packed);
    let (cross_lead, cross_between) = if safe_cross && cross_free < -EPS {
        (0.0, 0.0)
    } else {
        align_lines(style.align_content, cross_free, line_cross.len())
    };
    let mut cross_cursor = if reverse_cross {
        used_cross - cross_lead
    } else {
        cross_lead
    };
    for (line_index, &(start, end)) in lines.iter().enumerate() {
        let line_size = line_cross[line_index];
        if reverse_cross {
            cross_cursor -= line_size;
        }
        let top_line = if reverse_cross { true } else { line_index == 0 };
        let line_top = bt + pt + cross_cursor;
        let mut fallback_baseline = None::<LastBaseline>;
        let line = &mut items[start..end];
        let gap_sum = main_gap * line.len().saturating_sub(1) as f32;
        let used_outer = line.iter().map(|item| item.outer_main()).sum::<f32>() + gap_sum;
        let mut free = inner_main - used_outer;
        let auto_margins = line
            .iter()
            .map(|item| usize::from(item.auto_main_start) + usize::from(item.auto_main_end))
            .sum::<usize>();
        if free > EPS && auto_margins > 0 {
            let share = free / auto_margins as f32;
            for item in line.iter_mut() {
                if item.auto_main_start {
                    item.margin_main_start += share;
                }
                if item.auto_main_end {
                    item.margin_main_end += share;
                }
            }
            free = 0.0;
        }
        let (mut lead, between) = justify(style.justify_content, free, line.len());
        if free < -EPS && safe_justify(style) {
            // `safe` overflow aligns as `start` (CSS Align §4.4): the physical
            // start edge, the flex-end side under a reversed main axis.
            lead = if reverse { free } else { 0.0 };
        }
        if reverse {
            lead = inner_main - lead;
        }
        let mut cursor = lead;
        for (offset, item) in line.iter().enumerate() {
            let ascent = ascents[start + offset];
            let laid = fragments[frag_at].take();
            frag_at += 1;
            let Some(laid) = laid else {
                return super::bail("flex:line");
            };
            let border_main = item.content + item.main_edges;
            let main = if reverse {
                cursor - item.margin_main_start - border_main
            } else {
                cursor + item.margin_main_start
            };
            let main = unit(main);
            let margin_box = item.margin_box_cross();
            // Unsafe alignment, as in the single-line path.
            let extra = line_size - margin_box;
            let from_cross_start = match item.align {
                AlignItems::FlexEnd => extra,
                AlignItems::Center => extra / 2.0,
                AlignItems::Baseline => match (line_ascent[line_index], ascent) {
                    (Some(max), Some(ascent)) => max - ascent,
                    _ => 0.0,
                },
                AlignItems::Stretch | AlignItems::FlexStart => 0.0,
            };
            let border_cross = item.cross_edges + item.cross.unwrap_or(0.0);
            // Cross-start is the top of the line, unless `wrap-reverse` put
            // it at the bottom. The offset is the border box from the line's
            // physical top.
            let cross = if reverse_cross {
                line_size - from_cross_start - item.margin_cross_start - border_cross
            } else {
                from_cross_start + item.margin_cross_start
            };
            let cross = unit(cross);
            let (x, y) = if row {
                (bl + pl + main, bt + pt + cross_cursor + cross)
            } else {
                (bl + pl + cross_cursor + cross, bt + pt + main)
            };
            if row && top_line {
                let item_baseline = match laid.first_baseline {
                    LastBaseline::NoLines => LastBaseline::At(laid.fragment.height),
                    other => other,
                }
                .offset(y);
                if reverse {
                    fallback_baseline = Some(item_baseline);
                } else {
                    fallback_baseline.get_or_insert(item_baseline);
                }
            }
            let (dx, dy) = oof::relative_offset(flat[item.index].style, space);
            oof_out.extend(
                laid.oof
                    .iter()
                    .map(|candidate| candidate.translated(x + dx, y + dy)),
            );
            let (paint_dy, fragment) = text_item_paint_box(flat, item.index, index, laid.fragment);
            children.push((x + dx, y + dy + paint_dy, fragment));
            let step = item.outer_main() + main_gap + between;
            if reverse {
                cursor -= step;
            } else {
                cursor += step;
            }
        }
        if row && top_line {
            container_baseline = match line_ascent.get(line_index).copied().flatten() {
                Some(max) if wrap_reverse => Some(LastBaseline::At(line_top + line_size - max)),
                Some(max) => Some(LastBaseline::At(line_top + max)),
                None => fallback_baseline,
            };
        }
        if reverse_cross {
            cross_cursor -= cross_gap + cross_between;
        } else {
            cross_cursor += line_size + cross_gap + cross_between;
        }
    }
    // A max-height limit flexes each line but does not force the used
    // block size: the container is as tall as its tallest flexed line.
    let content_main = if row {
        inner_width
    } else if main_is_max {
        lines
            .iter()
            .map(|&(start, end)| {
                let line = &items[start..end];
                let gaps = main_gap * line.len().saturating_sub(1) as f32;
                line.iter().map(|item| item.outer_main()).sum::<f32>() + gaps
            })
            .fold(0.0_f32, f32::max)
    } else {
        inner_main.max(0.0)
    };
    let content_cross = if row {
        specified_height.unwrap_or(packed)
    } else {
        inner_width
    };
    let (width, mut height) = if row {
        (content_main, content_cross)
    } else {
        (content_cross, content_main)
    };
    let to_content_h = |size: f32| to_content(style, bt + bb + pt + pb, size);
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
    if style.position != w3cos_std::style::Position::Static && !oof_out.is_empty() {
        let cb = oof::ContainingBlock {
            x: bl,
            y: bt,
            width: border_width - bl - br,
            height: border_height - bt - bb,
            rtl: false,
        };
        let (placed, rest) = oof::resolve(
            flat,
            std::mem::take(&mut oof_out),
            &cb,
            false,
            space.viewport_w,
            space.viewport_h,
        )
        .why("flex:oof-resolve")?;
        children.extend(placed);
        oof_out = rest;
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
            ellipsis: false,
            children,
        },
        // As in the single-line path, the first baseline is reported for
        // both: an inline-flex uses its first baseline.
        last_baseline: container_baseline.unwrap_or(LastBaseline::NoLines),
        first_baseline: container_baseline.unwrap_or(LastBaseline::NoLines),
        start_strut,
        end_strut,
        self_collapsing: false,
        oof: oof_out,
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
    fn column_cross_alignment_uses_fit_content_width() {
        // A nowrap column item that does not stretch is fit-content wide and
        // aligned in the container's inner width.
        let mut column = flex_style(FlexDirection::Column, 100.0, Some(80.0));
        column.align_items = AlignItems::Center;
        let mut end = block_box(Some(40.0), Some(20.0));
        end.align_self = AlignSelf::FlexEnd;
        let root = Component::boxed(
            column,
            vec![
                leaf(block_box(Some(40.0), Some(20.0))),
                leaf(end),
                leaf(block_box(None, Some(10.0))),
            ],
        );
        expect(
            &root,
            &[
                (1, [30.0, 0.0, 40.0, 20.0]),
                (2, [60.0, 20.0, 40.0, 20.0]),
                (3, [50.0, 40.0, 0.0, 10.0]),
            ],
        );
    }

    #[test]
    fn indefinite_column_wrap_with_a_gap_stays_on_legacy() {
        // gap-003's reference uses logical margins and writing-mode, which
        // are not stored. An automatic column with a gap stays on legacy
        // so both sides of that reftest use the same engine.
        let mut gapped = flex_style(FlexDirection::Column, 100.0, None);
        gapped.flex_wrap = FlexWrap::Wrap;
        gapped.gap = 20.0;
        gapped.row_gap = Some(20.0);
        let root = Component::boxed(gapped, vec![leaf(block_box(Some(40.0), Some(40.0)))]);
        let flat = pre_flatten(&root);
        assert!(super::super::layout_tree(&root, &flat, 800.0, 600.0).is_none());
    }

    fn stays_legacy(root: &Component) {
        let flat = pre_flatten(root);
        assert!(super::super::layout_tree(root, &flat, 800.0, 600.0).is_none());
    }

    fn baseline_row(width: f32, height: Option<f32>, children: Vec<Component>) -> Component {
        let mut container = flex_style(FlexDirection::Row, width, height);
        container.align_items = AlignItems::Baseline;
        Component::boxed(container, children)
    }

    #[test]
    fn synthesized_baseline_aligns_definite_border_boxes() {
        // Headless Chrome, body margin 0, content-box. The shared edge is
        // the border-box under edge, not the margin edge.
        expect(
            &baseline_row(
                200.0,
                None,
                vec![
                    leaf(block_box(Some(40.0), Some(10.0))),
                    leaf(block_box(Some(40.0), Some(30.0))),
                ],
            ),
            &[
                (0, [0.0, 0.0, 200.0, 30.0]),
                (1, [0.0, 20.0, 40.0, 10.0]),
                (2, [40.0, 0.0, 40.0, 30.0]),
            ],
        );

        // margin 4px 0 6px 0 on the short item. Border bottoms meet at 30;
        // the line is 36 because the 6px margin hangs below that edge.
        let mut short = block_box(Some(40.0), Some(10.0));
        short.margin = edges(4.0, 0.0, 6.0, 0.0);
        expect(
            &baseline_row(
                200.0,
                None,
                vec![leaf(short), leaf(block_box(Some(40.0), Some(30.0)))],
            ),
            &[
                (0, [0.0, 0.0, 200.0, 36.0]),
                (1, [0.0, 20.0, 40.0, 10.0]),
                (2, [40.0, 0.0, 40.0, 30.0]),
            ],
        );

        // padding 3, border 2, content height 10 → border box 50×20,
        // shifted so its under edge meets the 50px sibling.
        let mut padded = block_box(Some(40.0), Some(10.0));
        padded.padding = edges(3.0, 3.0, 3.0, 3.0);
        padded.border_top_width = Some(2.0);
        padded.border_right_width = Some(2.0);
        padded.border_bottom_width = Some(2.0);
        padded.border_left_width = Some(2.0);
        expect(
            &baseline_row(
                200.0,
                None,
                vec![leaf(padded), leaf(block_box(Some(40.0), Some(50.0)))],
            ),
            &[
                (0, [0.0, 0.0, 200.0, 50.0]),
                (1, [0.0, 30.0, 50.0, 20.0]),
                (2, [50.0, 0.0, 40.0, 50.0]),
            ],
        );

        // A definite container height keeps the same offsets; leftover
        // cross space stays after the group.
        expect(
            &baseline_row(
                200.0,
                Some(80.0),
                vec![
                    leaf(block_box(Some(40.0), Some(10.0))),
                    leaf(block_box(Some(40.0), Some(30.0))),
                ],
            ),
            &[
                (0, [0.0, 0.0, 200.0, 80.0]),
                (1, [0.0, 20.0, 40.0, 10.0]),
                (2, [40.0, 0.0, 40.0, 30.0]),
            ],
        );

        // margin-bottom 20 on a 10px item against a 25px item. The line
        // is max ascent 25 plus that hanging margin.
        let mut hanging = block_box(Some(40.0), Some(10.0));
        hanging.margin = edges(0.0, 0.0, 20.0, 0.0);
        expect(
            &baseline_row(
                200.0,
                None,
                vec![leaf(hanging), leaf(block_box(Some(40.0), Some(25.0)))],
            ),
            &[
                (0, [0.0, 0.0, 200.0, 45.0]),
                (1, [0.0, 15.0, 40.0, 10.0]),
                (2, [40.0, 0.0, 40.0, 25.0]),
            ],
        );
    }

    #[test]
    fn baseline_self_on_one_item_leaves_the_other_at_flex_start() {
        // align-items: flex-start, align-self: baseline only on the 30px
        // item. One participant, so both border boxes stay at the line start.
        let mut container = flex_style(FlexDirection::Row, 200.0, None);
        container.align_items = AlignItems::FlexStart;
        let mut tall = block_box(Some(40.0), Some(30.0));
        tall.align_self = AlignSelf::Baseline;
        expect(
            &Component::boxed(
                container,
                vec![leaf(block_box(Some(40.0), Some(10.0))), leaf(tall)],
            ),
            &[
                (0, [0.0, 0.0, 200.0, 30.0]),
                (1, [0.0, 0.0, 40.0, 10.0]),
                (2, [40.0, 0.0, 40.0, 30.0]),
            ],
        );
    }

    #[test]
    fn empty_block_descendant_uses_the_item_border_edge() {
        // A 10px empty block inside a 50px item does not move the item.
        // The item's synthesized baseline is its own border under edge.
        let inner = leaf(block_box(Some(40.0), Some(10.0)));
        expect(
            &baseline_row(
                200.0,
                None,
                vec![
                    Component::boxed(block_box(Some(40.0), Some(50.0)), vec![inner]),
                    leaf(block_box(Some(40.0), Some(30.0))),
                ],
            ),
            &[
                (0, [0.0, 0.0, 200.0, 50.0]),
                (1, [0.0, 0.0, 40.0, 50.0]),
                (2, [0.0, 0.0, 40.0, 10.0]),
                (3, [40.0, 20.0, 40.0, 30.0]),
            ],
        );
    }

    #[test]
    fn column_and_nested_grid_or_column_wrap_baseline_stay_on_legacy() {
        let mut column = flex_style(FlexDirection::Column, 100.0, Some(80.0));
        column.align_items = AlignItems::Baseline;
        stays_legacy(&Component::boxed(
            column,
            vec![leaf(block_box(Some(40.0), Some(10.0)))],
        ));

        // A multi-line column flex reports no baseline on NG yet.
        let mut column_wrap = flex_style(FlexDirection::Column, 40.0, Some(50.0));
        column_wrap.flex_wrap = FlexWrap::Wrap;
        stays_legacy(&baseline_row(
            200.0,
            None,
            vec![Component::boxed(
                column_wrap,
                vec![leaf(block_box(Some(40.0), Some(10.0)))],
            )],
        ));

        // A block-level grid reports no baseline on NG yet.
        let mut grid = block_box(Some(40.0), Some(50.0));
        grid.display = Display::Grid;
        stays_legacy(&baseline_row(
            200.0,
            None,
            vec![Component::boxed(
                grid,
                vec![leaf(block_box(None, Some(10.0)))],
            )],
        ));

        // An automatic cross size, a text line, and a nested nowrap flex
        // all have a first baseline (or synthesize one) and stay on NG.
        let mut text = block_box(Some(40.0), Some(30.0));
        text.display = Display::Block;
        let mut nested = block_box(Some(40.0), Some(50.0));
        nested.display = Display::Flex;
        nested.flex_direction = FlexDirection::Row;
        // A multi-line row aligns per line and exports its first baseline.
        let mut row_wrap = flex_style(FlexDirection::Row, 100.0, None);
        row_wrap.align_items = AlignItems::Baseline;
        row_wrap.flex_wrap = FlexWrap::Wrap;
        for root in [
            baseline_row(200.0, None, vec![leaf(block_box(Some(40.0), None))]),
            baseline_row(
                200.0,
                None,
                vec![Component::boxed(
                    text,
                    vec![Component::text("a", Style::default())],
                )],
            ),
            baseline_row(
                200.0,
                None,
                vec![
                    Component::boxed(nested, vec![leaf(block_box(Some(40.0), Some(10.0)))]),
                    leaf(block_box(Some(40.0), Some(30.0))),
                ],
            ),
            Component::boxed(
                row_wrap.clone(),
                vec![
                    leaf(block_box(Some(60.0), Some(10.0))),
                    leaf(block_box(Some(60.0), Some(30.0))),
                ],
            ),
            baseline_row(
                200.0,
                None,
                vec![Component::boxed(
                    row_wrap,
                    vec![leaf(block_box(Some(40.0), Some(10.0)))],
                )],
            ),
        ] {
            let flat = pre_flatten(&root);
            assert!(super::super::layout_tree(&root, &flat, 800.0, 600.0).is_some());
        }

        let mut rtl = flex_style(FlexDirection::Row, 100.0, None);
        rtl.direction = TextDirection::Rtl;
        stays_legacy(&Component::boxed(
            rtl,
            vec![leaf(block_box(Some(10.0), Some(10.0)))],
        ));
    }

    fn fixed_item(width: f32, height: f32) -> Style {
        let mut style = block_box(Some(width), Some(height));
        style.flex_grow = 0.0;
        style.flex_shrink = 0.0;
        style.flex_basis = Dimension::Px(width);
        style
    }

    #[test]
    fn definite_height_wrap_with_gap_stretches_auto_height_lines() {
        // gap-008-ltr shape: height 100, `gap: 40px 20px`, two 40px items
        // per line. The two empty lines share the free 60px and the
        // auto-height items stretch to 30px (align-content: stretch,
        // align-items: stretch).
        let mut container = flex_style(FlexDirection::Row, 100.0, Some(100.0));
        container.flex_wrap = FlexWrap::Wrap;
        container.row_gap = Some(40.0);
        container.column_gap = Some(20.0);
        let item = || {
            let mut style = block_box(Some(40.0), None);
            style.flex_grow = 0.0;
            style.flex_shrink = 0.0;
            style.flex_basis = Dimension::Px(40.0);
            leaf(style)
        };
        expect(
            &Component::boxed(container, vec![item(), item(), item(), item()]),
            &[
                (1, [0.0, 0.0, 40.0, 30.0]),
                (2, [60.0, 0.0, 40.0, 30.0]),
                (3, [0.0, 70.0, 40.0, 30.0]),
                (4, [60.0, 70.0, 40.0, 30.0]),
            ],
        );
    }

    #[test]
    fn balanced_wrap_minimizes_squared_line_error_with_greedy_line_count() {
        // Items 30/20/20/30, width 100, gap 10. Greedy: [30,20,20] [30]
        // (two lines). The even split is [30,20] [20,30] (sizes 60/60);
        // each line flexes (grow 1) against the real 100px: 50/40, 40/50.
        let mut container = flex_style(FlexDirection::Row, 100.0, Some(30.0));
        container.flex_wrap = FlexWrap::Wrap;
        container.gap = 10.0;
        container.row_gap = Some(10.0);
        container.column_gap = Some(10.0);
        let mut props = w3cos_std::style::CustomProperties::default();
        props.insert(BALANCE_MARKER.to_string(), "1".to_string());
        container.custom_properties = Some(props);
        let item = |width: f32| {
            let mut style = block_box(Some(width), Some(10.0));
            style.flex_grow = 1.0;
            style.flex_shrink = 0.0;
            style.flex_basis = Dimension::Px(width);
            leaf(style)
        };
        expect(
            &Component::boxed(
                container,
                vec![item(30.0), item(20.0), item(20.0), item(30.0)],
            ),
            &[
                (1, [0.0, 0.0, 50.0, 10.0]),
                (2, [60.0, 0.0, 40.0, 10.0]),
                (3, [0.0, 20.0, 40.0, 10.0]),
                (4, [50.0, 20.0, 50.0, 10.0]),
            ],
        );
    }

    #[test]
    fn row_wrap_reverse_places_the_first_line_on_the_cross_start() {
        // Headless Chrome, body margin 0. `flex: 0 0 40px`, height 10.
        // Auto height: container 100×20, first line at the bottom
        // (0,10), (40,10), second line (0,0).
        let mut container = flex_style(FlexDirection::Row, 100.0, None);
        container.flex_wrap = FlexWrap::WrapReverse;
        expect(
            &Component::boxed(
                container,
                vec![
                    leaf(fixed_item(40.0, 10.0)),
                    leaf(fixed_item(40.0, 10.0)),
                    leaf(fixed_item(40.0, 10.0)),
                ],
            ),
            &[
                (0, [0.0, 0.0, 100.0, 20.0]),
                (1, [0.0, 10.0, 40.0, 10.0]),
                (2, [40.0, 10.0, 40.0, 10.0]),
                (3, [0.0, 0.0, 40.0, 10.0]),
            ],
        );
    }

    #[test]
    fn row_wrap_reverse_flex_start_packs_lines_at_the_end() {
        // align-content: flex-start, height 50. Lines stay 10px and sit
        // at the bottom: (0,40), (40,40), (0,30).
        let mut container = flex_style(FlexDirection::Row, 100.0, Some(50.0));
        container.flex_wrap = FlexWrap::WrapReverse;
        container.align_content = AlignContent::FlexStart;
        expect(
            &Component::boxed(
                container,
                vec![
                    leaf(fixed_item(40.0, 10.0)),
                    leaf(fixed_item(40.0, 10.0)),
                    leaf(fixed_item(40.0, 10.0)),
                ],
            ),
            &[
                (0, [0.0, 0.0, 100.0, 50.0]),
                (1, [0.0, 40.0, 40.0, 10.0]),
                (2, [40.0, 40.0, 40.0, 10.0]),
                (3, [0.0, 30.0, 40.0, 10.0]),
            ],
        );
    }

    #[test]
    fn row_wrap_reverse_stretch_keeps_definite_items_on_the_cross_start() {
        // Default align-content: stretch grows each line by 15px in a
        // 50px height. Definite 10px items stay on the cross-start
        // (the bottom of each line): (0,40), (40,40), (0,15).
        let mut container = flex_style(FlexDirection::Row, 100.0, Some(50.0));
        container.flex_wrap = FlexWrap::WrapReverse;
        expect(
            &Component::boxed(
                container,
                vec![
                    leaf(fixed_item(40.0, 10.0)),
                    leaf(fixed_item(40.0, 10.0)),
                    leaf(fixed_item(40.0, 10.0)),
                ],
            ),
            &[
                (0, [0.0, 0.0, 100.0, 50.0]),
                (1, [0.0, 40.0, 40.0, 10.0]),
                (2, [40.0, 40.0, 40.0, 10.0]),
                (3, [0.0, 15.0, 40.0, 10.0]),
            ],
        );
    }

    #[test]
    fn row_wrap_reverse_flex_end_uses_the_cross_end() {
        // Height 40, two lines (60px items). Stretch adds 3px to each
        // line (13 and 27). align-items: flex-end is the cross-end, the
        // top of the line: (0,27,60,10) and (0,0,60,24).
        let mut container = flex_style(FlexDirection::Row, 100.0, Some(40.0));
        container.flex_wrap = FlexWrap::WrapReverse;
        container.align_items = AlignItems::FlexEnd;
        expect(
            &Component::boxed(
                container,
                vec![leaf(fixed_item(60.0, 10.0)), leaf(fixed_item(60.0, 24.0))],
            ),
            &[
                (0, [0.0, 0.0, 100.0, 40.0]),
                (1, [0.0, 27.0, 60.0, 10.0]),
                (2, [0.0, 0.0, 60.0, 24.0]),
            ],
        );
    }

    #[test]
    fn row_wrap_reverse_keeps_asymmetric_cross_margins_on_the_start_edge() {
        // One item, margin-top 2, margin-bottom 8, height 10, container
        // height 40, align-content stretch. The margin box is 20px and
        // the line grows to 40. Cross-start is the bottom, so the 8px
        // margin sits there and the border box is at y=22.
        let mut container = flex_style(FlexDirection::Row, 100.0, Some(40.0));
        container.flex_wrap = FlexWrap::WrapReverse;
        let mut item = fixed_item(40.0, 10.0);
        item.margin = edges(2.0, 0.0, 8.0, 0.0);
        expect(
            &Component::boxed(container, vec![leaf(item)]),
            &[(0, [0.0, 0.0, 100.0, 40.0]), (1, [0.0, 22.0, 40.0, 10.0])],
        );
    }

    #[test]
    fn row_wrap_reverse_safe_overflow_stays_at_the_start_edge() {
        // `align-content: safe flex-start`. The 100px item overflows a 90px
        // wrap-reverse container, so CSS Align §4.4 aligns it as `start`:
        // border box (0,0,100,100), overflowing the end edges. Headless
        // Chrome matches that for flexbox-safe-overflow-position-002.
        let mut container = flex_style(FlexDirection::Row, 90.0, Some(90.0));
        container.flex_wrap = FlexWrap::WrapReverse;
        container.custom_properties = Some(w3cos_std::style::CustomProperties::from_iter([(
            "--w3cos-internal-safe-align-content".to_string(),
            "1".to_string(),
        )]));
        expect(
            &Component::boxed(container, vec![leaf(fixed_item(100.0, 100.0))]),
            &[(0, [0.0, 0.0, 90.0, 90.0]), (1, [0.0, 0.0, 100.0, 100.0])],
        );
    }

    #[test]
    fn align_content_distributes_wrapped_lines() {
        // Two 40px-wide items wrap into two 20px lines in a 100px-tall row.
        // Free cross space is 60px.
        let lines = |kind: AlignContent, wrap: FlexWrap| {
            let mut container = flex_style(FlexDirection::Row, 60.0, Some(100.0));
            container.flex_wrap = wrap;
            container.align_content = kind;
            let root = Component::boxed(
                container,
                vec![leaf(fixed_item(40.0, 20.0)), leaf(fixed_item(40.0, 20.0))],
            );
            let out = lay(&root);
            (rect(&out, 1)[1], rect(&out, 2)[1])
        };
        assert_eq!(lines(AlignContent::Center, FlexWrap::Wrap), (30.0, 50.0));
        assert_eq!(lines(AlignContent::FlexEnd, FlexWrap::Wrap), (60.0, 80.0));
        assert_eq!(
            lines(AlignContent::SpaceBetween, FlexWrap::Wrap),
            (0.0, 80.0)
        );
        assert_eq!(
            lines(AlignContent::SpaceAround, FlexWrap::Wrap),
            (15.0, 65.0)
        );
        assert_eq!(
            lines(AlignContent::SpaceEvenly, FlexWrap::Wrap),
            (20.0, 60.0)
        );
        // wrap-reverse: flex-end packs at the top, first line below.
        assert_eq!(
            lines(AlignContent::FlexEnd, FlexWrap::WrapReverse),
            (20.0, 0.0)
        );
    }

    #[test]
    fn safe_justify_overflow_under_row_reverse_aligns_at_start() {
        // `flex-flow: row-reverse; justify-content: safe flex-start`. The
        // 100px item overflows a 90px line, so it aligns as `start`: the
        // left edge, overflowing on the right.
        let mut container = flex_style(FlexDirection::RowReverse, 90.0, Some(90.0));
        container.custom_properties = Some(w3cos_std::style::CustomProperties::from_iter([(
            "--w3cos-internal-justify-content".to_string(),
            "safe flex-start".to_string(),
        )]));
        expect(
            &Component::boxed(container, vec![leaf(fixed_item(100.0, 100.0))]),
            &[(1, [0.0, 0.0, 100.0, 100.0])],
        );
    }

    #[test]
    fn row_reverse_wrap_reverse_packs_main_and_cross_starts() {
        // row-reverse wrap-reverse. First line of two 40px items sits at
        // the bottom, packed to the right: (60,10), (20,10). The single
        // item on the next line is (60,0).
        let mut container = flex_style(FlexDirection::RowReverse, 100.0, None);
        container.flex_wrap = FlexWrap::WrapReverse;
        expect(
            &Component::boxed(
                container,
                vec![
                    leaf(fixed_item(40.0, 10.0)),
                    leaf(fixed_item(40.0, 10.0)),
                    leaf(fixed_item(40.0, 10.0)),
                ],
            ),
            &[
                (0, [0.0, 0.0, 100.0, 20.0]),
                (1, [60.0, 10.0, 40.0, 10.0]),
                (2, [20.0, 10.0, 40.0, 10.0]),
                (3, [60.0, 0.0, 40.0, 10.0]),
            ],
        );
    }

    #[test]
    fn row_wrap_breaks_on_the_hypothetical_main_size() {
        // 40 + 40 fits in 100; the third 40 starts a line. Shrink is 0, so
        // leftover main space stays empty (flex-start). Headless Chrome
        // border boxes, body margin 0: container 100×20, items at
        // (0,0,40,10), (40,0,40,10), (0,10,40,10).
        let mut container = flex_style(FlexDirection::Row, 100.0, None);
        container.flex_wrap = FlexWrap::Wrap;
        let root = Component::boxed(
            container,
            vec![
                leaf(fixed_item(40.0, 10.0)),
                leaf(fixed_item(40.0, 10.0)),
                leaf(fixed_item(40.0, 10.0)),
            ],
        );
        expect(
            &root,
            &[
                (0, [0.0, 0.0, 100.0, 20.0]),
                (1, [0.0, 0.0, 40.0, 10.0]),
                (2, [40.0, 0.0, 40.0, 10.0]),
                (3, [0.0, 10.0, 40.0, 10.0]),
            ],
        );
    }

    #[test]
    fn row_wrap_grows_each_line_and_honors_gaps() {
        // Line 1 holds two bases of 40 in a 100px row (free 20, grow 1 → 50).
        // Line 2's single item grows to 100. column-gap 0, row-gap 6.
        // Headless Chrome: container 100×26, items (0,0,50,10),
        // (50,0,50,10), (0,16,100,10).
        let mut item = fixed_item(40.0, 10.0);
        item.flex_grow = 1.0;
        let mut container = flex_style(FlexDirection::Row, 100.0, None);
        container.flex_wrap = FlexWrap::Wrap;
        container.row_gap = Some(6.0);
        expect(
            &Component::boxed(
                container,
                vec![leaf(item.clone()), leaf(item.clone()), leaf(item)],
            ),
            &[
                (0, [0.0, 0.0, 100.0, 26.0]),
                (1, [0.0, 0.0, 50.0, 10.0]),
                (2, [50.0, 0.0, 50.0, 10.0]),
                (3, [0.0, 16.0, 100.0, 10.0]),
            ],
        );
    }

    #[test]
    fn row_wrap_that_fits_matches_a_single_line() {
        let mut wrapped = flex_style(FlexDirection::Row, 100.0, None);
        wrapped.flex_wrap = FlexWrap::Wrap;
        let children = vec![leaf(fixed_item(30.0, 12.0)), leaf(fixed_item(30.0, 12.0))];
        let expected = [(1, [0.0, 0.0, 30.0, 12.0]), (2, [30.0, 0.0, 30.0, 12.0])];
        expect(&Component::boxed(wrapped, children.clone()), &expected);
        expect(&row_of(100.0, None, children), &expected);
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

    fn column_box(width: f32, height: f32) -> Style {
        let mut style = block_box(Some(width), Some(height));
        style.flex_grow = 0.0;
        style.flex_shrink = 0.0;
        style.flex_basis = Dimension::Px(height);
        style
    }

    fn column_flow(direction: FlexDirection, wrap: FlexWrap, width: f32, height: f32) -> Style {
        let mut style = flex_style(direction, width, Some(height));
        style.flex_wrap = wrap;
        style
    }

    #[test]
    fn column_wrap_fills_columns_from_the_cross_start() {
        // Headless Chrome, body margin 0. Nine 40px border boxes in a
        // 120px column wrap: three columns, source order down each column.
        let items = vec![leaf(column_box(40.0, 40.0)); 9];
        expect(
            &Component::boxed(
                column_flow(FlexDirection::Column, FlexWrap::Wrap, 120.0, 120.0),
                items,
            ),
            &[
                (0, [0.0, 0.0, 120.0, 120.0]),
                (1, [0.0, 0.0, 40.0, 40.0]),
                (2, [0.0, 40.0, 40.0, 40.0]),
                (3, [0.0, 80.0, 40.0, 40.0]),
                (4, [40.0, 0.0, 40.0, 40.0]),
                (5, [40.0, 40.0, 40.0, 40.0]),
                (6, [40.0, 80.0, 40.0, 40.0]),
                (7, [80.0, 0.0, 40.0, 40.0]),
                (8, [80.0, 40.0, 40.0, 40.0]),
                (9, [80.0, 80.0, 40.0, 40.0]),
            ],
        );
    }

    #[test]
    fn column_wrap_reverse_packs_the_first_column_at_the_end() {
        let items = vec![leaf(column_box(40.0, 40.0)); 9];
        expect(
            &Component::boxed(
                column_flow(FlexDirection::Column, FlexWrap::WrapReverse, 120.0, 120.0),
                items,
            ),
            &[
                (1, [80.0, 0.0, 40.0, 40.0]),
                (2, [80.0, 40.0, 40.0, 40.0]),
                (3, [80.0, 80.0, 40.0, 40.0]),
                (4, [40.0, 0.0, 40.0, 40.0]),
                (7, [0.0, 0.0, 40.0, 40.0]),
            ],
        );
    }

    #[test]
    fn column_reverse_wrap_packs_from_the_main_start() {
        // column-reverse: the first item in each column sits on the bottom.
        // wrap-reverse also moves the first column to the right.
        let down = vec![leaf(column_box(40.0, 40.0)); 9];
        expect(
            &Component::boxed(
                column_flow(FlexDirection::ColumnReverse, FlexWrap::Wrap, 120.0, 120.0),
                down.clone(),
            ),
            &[
                (1, [0.0, 80.0, 40.0, 40.0]),
                (2, [0.0, 40.0, 40.0, 40.0]),
                (3, [0.0, 0.0, 40.0, 40.0]),
                (4, [40.0, 80.0, 40.0, 40.0]),
            ],
        );
        expect(
            &Component::boxed(
                column_flow(
                    FlexDirection::ColumnReverse,
                    FlexWrap::WrapReverse,
                    120.0,
                    120.0,
                ),
                down,
            ),
            &[
                (1, [80.0, 80.0, 40.0, 40.0]),
                (2, [80.0, 40.0, 40.0, 40.0]),
                (3, [80.0, 0.0, 40.0, 40.0]),
                (4, [40.0, 80.0, 40.0, 40.0]),
            ],
        );
    }

    #[test]
    fn column_wrap_stretch_and_flex_start_and_center() {
        // Four 40px items in a 200×120 column. flex-start keeps 40px
        // columns. stretch grows each column to 100 and leaves the
        // definite items on the cross-start. center uses the midpoint.
        let items = || vec![leaf(column_box(40.0, 40.0)); 4];
        let mut start = column_flow(FlexDirection::Column, FlexWrap::Wrap, 200.0, 120.0);
        start.align_content = AlignContent::FlexStart;
        expect(
            &Component::boxed(start, items()),
            &[
                (1, [0.0, 0.0, 40.0, 40.0]),
                (2, [0.0, 40.0, 40.0, 40.0]),
                (3, [0.0, 80.0, 40.0, 40.0]),
                (4, [40.0, 0.0, 40.0, 40.0]),
            ],
        );
        expect(
            &Component::boxed(
                column_flow(FlexDirection::Column, FlexWrap::Wrap, 200.0, 120.0),
                items(),
            ),
            &[(1, [0.0, 0.0, 40.0, 40.0]), (4, [100.0, 0.0, 40.0, 40.0])],
        );
        let mut center = column_flow(FlexDirection::Column, FlexWrap::Wrap, 200.0, 120.0);
        center.align_items = AlignItems::Center;
        expect(
            &Component::boxed(center, items()),
            &[(1, [30.0, 0.0, 40.0, 40.0]), (4, [130.0, 0.0, 40.0, 40.0])],
        );
        let mut reverse = column_flow(FlexDirection::Column, FlexWrap::WrapReverse, 200.0, 120.0);
        reverse.align_content = AlignContent::FlexStart;
        expect(
            &Component::boxed(reverse, items()),
            &[(1, [160.0, 0.0, 40.0, 40.0]), (4, [120.0, 0.0, 40.0, 40.0])],
        );
        expect(
            &Component::boxed(
                column_flow(FlexDirection::Column, FlexWrap::WrapReverse, 200.0, 120.0),
                items(),
            ),
            &[(1, [160.0, 0.0, 40.0, 40.0]), (4, [60.0, 0.0, 40.0, 40.0])],
        );
    }

    #[test]
    fn column_wrap_gap_safe_overflow_margins_and_borders() {
        // row-gap is the main gap, column-gap the cross gap. Stretch adds
        // 5px to each 40px column; the second column starts after the gap
        // at x=55. Headless Chrome.
        let mut gapped = column_flow(FlexDirection::Column, FlexWrap::Wrap, 100.0, 100.0);
        gapped.row_gap = Some(10.0);
        gapped.column_gap = Some(10.0);
        expect(
            &Component::boxed(
                gapped,
                vec![
                    leaf(column_box(40.0, 40.0)),
                    leaf(column_box(40.0, 40.0)),
                    leaf(column_box(40.0, 40.0)),
                ],
            ),
            &[
                (1, [0.0, 0.0, 40.0, 40.0]),
                (2, [0.0, 50.0, 40.0, 40.0]),
                (3, [55.0, 0.0, 40.0, 40.0]),
            ],
        );

        let mut safe = column_flow(FlexDirection::Column, FlexWrap::WrapReverse, 90.0, 90.0);
        safe.custom_properties = Some(w3cos_std::style::CustomProperties::from_iter([(
            "--w3cos-internal-safe-align-content".to_string(),
            "1".to_string(),
        )]));
        expect(
            &Component::boxed(safe, vec![leaf(column_box(100.0, 100.0))]),
            &[(0, [0.0, 0.0, 90.0, 90.0]), (1, [0.0, 0.0, 100.0, 100.0])],
        );

        // wrap-reverse swaps cross margins, so the 8px end margin sits on
        // the right (the cross-start). The border box is at x=52.
        let mut margined = column_box(40.0, 40.0);
        margined.margin = edges(0.0, 8.0, 0.0, 2.0);
        expect(
            &Component::boxed(
                column_flow(FlexDirection::Column, FlexWrap::WrapReverse, 100.0, 40.0),
                vec![leaf(margined.clone())],
            ),
            &[(1, [52.0, 0.0, 40.0, 40.0])],
        );
        expect(
            &Component::boxed(
                column_flow(FlexDirection::Column, FlexWrap::Wrap, 100.0, 40.0),
                vec![leaf(margined)],
            ),
            &[(1, [2.0, 0.0, 40.0, 40.0])],
        );

        // 38px content + 1px border is a 40px border box. Same grid as
        // the content-box 40px items (flexbox-flex-direction-column).
        let mut bordered = column_box(38.0, 38.0);
        bordered.border_top_width = Some(1.0);
        bordered.border_right_width = Some(1.0);
        bordered.border_bottom_width = Some(1.0);
        bordered.border_left_width = Some(1.0);
        expect(
            &Component::boxed(
                column_flow(FlexDirection::Column, FlexWrap::Wrap, 120.0, 120.0),
                vec![leaf(bordered); 9],
            ),
            &[
                (1, [0.0, 0.0, 40.0, 40.0]),
                (2, [0.0, 40.0, 40.0, 40.0]),
                (4, [40.0, 0.0, 40.0, 40.0]),
                (7, [80.0, 0.0, 40.0, 40.0]),
            ],
        );
    }

    #[test]
    fn column_wrap_grows_items_on_each_line() {
        // Two flex:1 1 30px items fit on one column and grow to 50.
        let mut item = column_box(40.0, 30.0);
        item.flex_grow = 1.0;
        item.flex_shrink = 1.0;
        expect(
            &Component::boxed(
                column_flow(FlexDirection::Column, FlexWrap::Wrap, 80.0, 100.0),
                vec![leaf(item.clone()), leaf(item)],
            ),
            &[
                (0, [0.0, 0.0, 80.0, 100.0]),
                (1, [0.0, 0.0, 40.0, 50.0]),
                (2, [0.0, 50.0, 40.0, 50.0]),
            ],
        );
    }

    fn auto_column(width: f32) -> Style {
        let mut style = flex_style(FlexDirection::Column, width, None);
        style.flex_wrap = FlexWrap::Wrap;
        style
    }

    #[test]
    fn automatic_column_wrap_is_one_line_and_wrap_reverse_packs_at_the_end() {
        // Headless Chrome, body margin 0. Height auto collects every item
        // into one column. wrap-reverse puts that column at x=80 in a
        // 120px container. Items are 40×30.
        let items = || {
            vec![
                leaf(column_box(40.0, 30.0)),
                leaf(column_box(40.0, 30.0)),
                leaf(column_box(40.0, 30.0)),
            ]
        };
        expect(
            &Component::boxed(auto_column(120.0), items()),
            &[
                (0, [0.0, 0.0, 120.0, 90.0]),
                (1, [0.0, 0.0, 40.0, 30.0]),
                (2, [0.0, 30.0, 40.0, 30.0]),
                (3, [0.0, 60.0, 40.0, 30.0]),
            ],
        );
        let mut reverse = auto_column(120.0);
        reverse.flex_wrap = FlexWrap::WrapReverse;
        expect(
            &Component::boxed(reverse, items()),
            &[
                (0, [0.0, 0.0, 120.0, 90.0]),
                (1, [80.0, 0.0, 40.0, 30.0]),
                (2, [80.0, 30.0, 40.0, 30.0]),
                (3, [80.0, 60.0, 40.0, 30.0]),
            ],
        );
    }

    #[test]
    fn column_wrap_max_height_uses_the_flexed_line_not_the_limit() {
        // Headless Chrome. max-height 50 wraps 40px items into their own
        // columns, and the used height is 40, not 50. wrap-reverse packs
        // the first column at the right. flex-grow fills the limit, so
        // that container is 100×100.
        let mut limited = auto_column(120.0);
        limited.max_height = Dimension::Px(50.0);
        let items = vec![
            leaf(column_box(40.0, 40.0)),
            leaf(column_box(40.0, 40.0)),
            leaf(column_box(40.0, 40.0)),
        ];
        expect(
            &Component::boxed(limited, items.clone()),
            &[
                (0, [0.0, 0.0, 120.0, 40.0]),
                (1, [0.0, 0.0, 40.0, 40.0]),
                (2, [40.0, 0.0, 40.0, 40.0]),
                (3, [80.0, 0.0, 40.0, 40.0]),
            ],
        );
        let mut reverse = auto_column(120.0);
        reverse.flex_wrap = FlexWrap::WrapReverse;
        reverse.max_height = Dimension::Px(50.0);
        expect(
            &Component::boxed(reverse, items),
            &[
                (0, [0.0, 0.0, 120.0, 40.0]),
                (1, [80.0, 0.0, 40.0, 40.0]),
                (2, [40.0, 0.0, 40.0, 40.0]),
                (3, [0.0, 0.0, 40.0, 40.0]),
            ],
        );

        let mut grow = column_box(50.0, 50.0);
        grow.flex_grow = 1.0;
        let mut filled = auto_column(100.0);
        filled.max_height = Dimension::Px(100.0);
        expect(
            &Component::boxed(filled, vec![leaf(grow), leaf(column_box(50.0, 100.0))]),
            &[
                (0, [0.0, 0.0, 100.0, 100.0]),
                (1, [0.0, 0.0, 50.0, 100.0]),
                (2, [50.0, 0.0, 50.0, 100.0]),
            ],
        );
    }

    #[test]
    fn column_wrap_aspect_ratio_is_a_definite_block_size() {
        // Headless Chrome. width 100 and aspect-ratio 1/1 wrap two 50×100
        // items into a 100×100 square.
        let mut container = auto_column(100.0);
        container.min_height = Dimension::Px(0.0);
        container.custom_properties = Some(w3cos_std::style::CustomProperties::from_iter([(
            "--w3cos-internal-aspect-ratio".to_string(),
            "1".to_string(),
        )]));
        expect(
            &Component::boxed(
                container,
                vec![leaf(column_box(50.0, 100.0)), leaf(column_box(50.0, 100.0))],
            ),
            &[
                (0, [0.0, 0.0, 100.0, 100.0]),
                (1, [0.0, 0.0, 50.0, 100.0]),
                (2, [50.0, 0.0, 50.0, 100.0]),
            ],
        );
    }

    fn width_content(width: f32, height: f32) -> Component {
        let mut item = block_box(None, Some(height));
        item.flex_grow = 0.0;
        item.flex_shrink = 0.0;
        item.flex_basis = Dimension::Px(height);
        Component::boxed(item, vec![leaf(block_box(Some(width), Some(10.0)))])
    }

    #[test]
    fn column_wrap_auto_cross_is_fit_content_against_the_inner_width() {
        // Headless Chrome. Two items, content widths 80 and 30, in a
        // 100×100 column: each is its own line at that content width.
        // Five 25px inline-blocks in a 50px-wide container clamp to 50.
        // margin-right 25 in a 125px container leaves a 100px item.
        expect(
            &Component::boxed(
                column_flow(FlexDirection::Column, FlexWrap::Wrap, 100.0, 100.0),
                vec![width_content(80.0, 60.0), width_content(30.0, 60.0)],
            ),
            &[
                (0, [0.0, 0.0, 100.0, 100.0]),
                (1, [0.0, 0.0, 80.0, 60.0]),
                (3, [80.0, 0.0, 30.0, 60.0]),
            ],
        );

        let inline_blocks = |item_height: f32, piece: f32| {
            let mut item = block_box(None, Some(item_height));
            item.flex_grow = 0.0;
            item.flex_shrink = 0.0;
            item.flex_basis = Dimension::Px(item_height);
            let piece = Style {
                display: Display::InlineBlock,
                width: Dimension::Px(piece),
                height: Dimension::Px(0.0),
                ..Style::default()
            };
            Component::boxed(item, vec![leaf(piece); 5])
        };
        expect(
            &Component::boxed(
                column_flow(FlexDirection::Column, FlexWrap::Wrap, 50.0, 100.0),
                vec![inline_blocks(100.0, 25.0), inline_blocks(100.0, 25.0)],
            ),
            &[
                (0, [0.0, 0.0, 50.0, 100.0]),
                (1, [0.0, 0.0, 50.0, 100.0]),
                (7, [50.0, 0.0, 50.0, 100.0]),
            ],
        );
        let mut margined = block_box(None, Some(100.0));
        margined.flex_grow = 0.0;
        margined.flex_shrink = 0.0;
        margined.flex_basis = Dimension::Px(100.0);
        margined.margin.right = px(25.0);
        let piece = Style {
            display: Display::InlineBlock,
            width: Dimension::Px(50.0),
            height: Dimension::Px(0.0),
            ..Style::default()
        };
        expect(
            &Component::boxed(
                column_flow(FlexDirection::Column, FlexWrap::Wrap, 125.0, 100.0),
                vec![Component::boxed(margined, vec![leaf(piece); 5])],
            ),
            &[(0, [0.0, 0.0, 125.0, 100.0]), (1, [0.0, 0.0, 100.0, 100.0])],
        );
    }
}
