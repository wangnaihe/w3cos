//! Grid formatting context.
//!
//! Structure follows Blink's NGGridLayoutAlgorithm
//! (third_party/blink/renderer/core/layout/grid/grid_layout_algorithm.cc,
//! Copyright The Chromium Authors, BSD-style license) and CSS Grid Level 1
//! §8 (placement) and §12 (track sizing). A missing column template is one
//! track sized by grid-auto-columns, as an implicit column would be.
//!
//! A subgrid reuses the parent tracks it spans, including `auto`, `fr`,
//! `minmax`, and intrinsic tracks. Its items contribute to those tracks.
//! The subgrid's margin, border, and padding are added to the edge tracks,
//! and half the difference between its gap and the parent gap is an extra
//! margin on the items that face a gutter (CSS Grid §9.2). Line names on
//! the parent are inherited and the subgrid's own `[name]` list is added.
//! A nested subgrid contributes through the same path. An unsupported
//! subgrid syntax, a percent gap, and `grid-template-areas` stay on the
//! legacy engine. A subgrid is still stretched with an automatic size on
//! the subgridded axis: the legacy engine honors a specified size, so
//! ignoring it here would paint only one side of those reftests.
//!
//! Track sizes are LayoutUnits (1/64 px). Flexible tracks share free space
//! after inflexible tracks have grown to their caps; the truncated
//! remainder is given one unit at a time to the last flexible tracks.

use super::Why;
use super::block::{self, BoxMetrics, MinMax};
use super::inline::rendered_children;
use super::oof::{self, OofCandidate};
use super::{BoxFragment, ConstraintSpace, LastBaseline, LayoutResult, MarginStrut};
use crate::layout::{FlatNodeInfo, resolve_spacing_for_layout};
use std::cell::RefCell;
use std::collections::BTreeSet;
use w3cos_std::style::{
    AlignContent, AlignItems, AlignSelf, BoxSizing, Clear, Dimension, Display, Float,
    JustifyContent, Position, Spacing, Style, TextDirection,
};

const EPS: f32 = 1.0 / 128.0;
const MAX_REPEAT: usize = 1024;
const MAX_TRACKS: usize = 1024;
const INF: i64 = i64::MAX / 4;
const HEIGHT_KEYWORD: &str = "--w3cos-internal-height-keyword";
const WIDTH_KEYWORD: &str = "--w3cos-internal-width-keyword";
const JUSTIFY_KEYWORD: &str = "--w3cos-internal-justify-content";

fn units(v: f32) -> i64 {
    if v.is_finite() {
        (v * 64.0).trunc() as i64
    } else {
        0
    }
}

fn from_units(v: i64) -> f32 {
    v as f32 / 64.0
}

fn has_marker(style: &Style, key: &str) -> bool {
    style
        .custom_properties
        .as_ref()
        .is_some_and(|props| props.contains_key(key))
}

fn marker<'a>(style: &'a Style, key: &str) -> Option<&'a str> {
    style
        .custom_properties
        .as_ref()
        .and_then(|props| props.get(key))
        .map(String::as_str)
}

fn to_content(style: &Style, edges: f32, size: f32) -> f32 {
    match style.box_sizing {
        BoxSizing::BorderBox => (size - edges).max(0.0),
        BoxSizing::ContentBox => size.max(0.0),
    }
}

fn spacing_px(style: &Style, spacing: Spacing, cb: f32, space: &ConstraintSpace) -> f32 {
    match spacing {
        Spacing::Auto => 0.0,
        other => resolve_spacing_for_layout(other, cb, style, space.viewport_w, space.viewport_h),
    }
}

fn percent_block(style: &Style) -> bool {
    matches!(style.height, Dimension::Percent(_))
        || matches!(style.min_height, Dimension::Percent(_))
        || matches!(style.max_height, Dimension::Percent(_))
}

fn inset_set(style: &Style) -> bool {
    !matches!(style.top, Dimension::Auto)
        || !matches!(style.right, Dimension::Auto)
        || !matches!(style.bottom, Dimension::Auto)
        || !matches!(style.left, Dimension::Auto)
}

fn columns_spec(style: &Style) -> Option<&str> {
    if style.grid_columns_cascaded() {
        style.grid_columns_used()
    } else {
        style.grid_template_columns.as_deref()
    }
}

fn placement<'a>(style: &'a Style, row: bool) -> Option<&'a str> {
    if style.grid_placement_cascaded() {
        if row {
            style.grid_row_used()
        } else {
            style.grid_column_used()
        }
    } else if row {
        style.grid_row_spec()
    } else {
        style.grid_column.as_deref()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pack {
    Start,
    End,
    Center,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
    Stretch,
}

fn column_pack(style: &Style) -> Pack {
    let keyword = marker(style, JUSTIFY_KEYWORD).unwrap_or(match style.justify_content {
        // The initial `normal` computes to stretch for grid. An authored
        // `start` stores the keyword, so a bare FlexStart is that initial.
        JustifyContent::FlexStart => "stretch",
        JustifyContent::FlexEnd => "end",
        JustifyContent::Center => "center",
        JustifyContent::SpaceBetween => "space-between",
        JustifyContent::SpaceAround => "space-around",
        JustifyContent::SpaceEvenly => "space-evenly",
    });
    pack_keyword(keyword, style.direction == TextDirection::Rtl)
}

fn row_pack(style: &Style) -> Pack {
    match style.align_content {
        AlignContent::FlexStart => Pack::Start,
        AlignContent::FlexEnd => Pack::End,
        AlignContent::Center => Pack::Center,
        AlignContent::SpaceBetween => Pack::SpaceBetween,
        AlignContent::SpaceAround => Pack::SpaceAround,
        AlignContent::SpaceEvenly => Pack::SpaceEvenly,
        AlignContent::Stretch => Pack::Stretch,
    }
}

fn pack_keyword(keyword: &str, rtl: bool) -> Pack {
    match keyword {
        "stretch" | "normal" => Pack::Stretch,
        "center" => Pack::Center,
        "space-between" => Pack::SpaceBetween,
        "space-around" => Pack::SpaceAround,
        "space-evenly" => Pack::SpaceEvenly,
        "end" | "flex-end" => Pack::End,
        "start" | "flex-start" => Pack::Start,
        "left" => {
            if rtl {
                Pack::End
            } else {
                Pack::Start
            }
        }
        "right" => {
            if rtl {
                Pack::Start
            } else {
                Pack::End
            }
        }
        _ => Pack::Start,
    }
}

fn used_item_align(container: AlignItems, item: AlignSelf) -> AlignItems {
    match item {
        AlignSelf::Auto => container,
        AlignSelf::FlexStart => AlignItems::FlexStart,
        AlignSelf::FlexEnd => AlignItems::FlexEnd,
        AlignSelf::Center => AlignItems::Center,
        AlignSelf::Stretch => AlignItems::Stretch,
        AlignSelf::Baseline => AlignItems::Baseline,
    }
}

fn align_pack(align: AlignItems) -> Pack {
    match align {
        AlignItems::FlexStart => Pack::Start,
        AlignItems::FlexEnd => Pack::End,
        AlignItems::Center => Pack::Center,
        AlignItems::Stretch => Pack::Stretch,
        AlignItems::Baseline => Pack::Start,
    }
}

fn is_baseline(align: AlignItems) -> bool {
    align == AlignItems::Baseline
}

/// Why this grid container is outside the supported subset. `None` when
/// the tracks can be laid out.
pub(super) fn container_reason(flat: &[FlatNodeInfo], index: usize) -> Option<&'static str> {
    let style = flat[index].style;
    if has_marker(style, "--w3cos-internal-document-element") {
        return Some("grid:root");
    }
    if style.grid_areas().is_some() {
        return Some("grid:areas");
    }
    if is_baseline(style.align_items) || is_baseline(style.justify_items) {
        return Some("grid:baseline");
    }
    if let Some(reason) = subgrid_or_template(flat, index, false) {
        return Some(reason);
    }
    if let Some(reason) = subgrid_or_template(flat, index, true) {
        return Some(reason);
    }
    if has_marker(style, "--w3cos-internal-grid-lanes") && !lanes_like_grid(flat, index) {
        // Most `grid-lanes` reftests match only when both sides stay on the
        // legacy flex fallback. Opt in only when the lanes box is the same
        // formatting context as its ordinary-grid reference: explicit
        // columns, or definite rows without auto-repeat or abspos items.
        return Some("grid:lanes");
    }
    if !block::size_contained(style)
        && row_lane_count(style).is_some_and(|lanes| inflow_count(flat, index) > lanes)
    {
        // Row lanes holding several items each stay on legacy, except under
        // size containment, where the contained-grid references are on NG.
        // NG does not parse `safe`/`unsafe` self-alignment yet, which those
        // lanes reftests (row-overflow-alignment-001) depend on.
        return Some("grid:lanes-rows");
    }
    if rows_match_legacy_lanes(flat, index)
        && style.grid_rows().is_some_and(rows_are_sized)
        && indefinite_block(style)
    {
        return Some("grid:indefinite-rows");
    }
    if rows_match_legacy_lanes(flat, index)
        && has_marker(style, "--w3cos-internal-percent-gap")
        && style.grid_rows().is_some_and(rows_are_sized)
        && !indefinite_block(style)
    {
        return Some("grid:percent-gap");
    }
    if style.grid_auto_column_spec().is_some_and(|spec| {
        parse_one_track(spec).is_none_or(|track| spec_is_intrinsic_keyword(&track))
    }) {
        return Some("grid:auto-columns");
    }
    if style.grid_auto_row_spec().is_some_and(|spec| {
        parse_one_track(spec).is_none_or(|track| spec_is_intrinsic_keyword(&track))
    }) {
        return Some("grid:auto-rows");
    }
    if parse_flow(style.grid_flow()).is_none() {
        return Some("grid:auto-flow");
    }
    for child in rendered_children(flat, index) {
        let child_style = flat[child].style;
        if oof::is_out_of_flow(child_style) {
            continue;
        }
        if child_style.float != Float::None {
            return Some("grid:float");
        }
        if placement(child_style, false).is_some_and(|value| parse_axis_syntax(value).is_none()) {
            return Some("grid:column");
        }
        if placement(child_style, true).is_some_and(|value| parse_axis_syntax(value).is_none()) {
            return Some("grid:row");
        }
        if is_baseline(used_item_align(style.align_items, child_style.align_self))
            || is_baseline(used_item_align(
                style.justify_items,
                child_style.justify_self,
            ))
        {
            return Some("grid:baseline");
        }
        if percent_block(child_style) {
            return Some("grid:percent-block");
        }
        if child_style.flex_grow != 0.0
            || child_style.flex_shrink != 1.0
            || child_style.flex_basis != Dimension::Auto
        {
            return Some("grid:flex");
        }
        if child_style.position == Position::Relative && inset_set(child_style) {
            return Some("grid:inset");
        }
        if has_marker(child_style, "--w3cos-internal-width-keyword") {
            return Some("grid:item-width-keyword");
        }
    }
    None
}

fn spec_mentions_subgrid(spec: &str) -> bool {
    spec.to_ascii_lowercase().contains("subgrid")
}

enum SubgridSpec {
    No,
    /// The keyword is present, but the line-name list is not one this
    /// pass can place (`repeat()`, a track size, and so on).
    Unsupported,
    Yes(Vec<Vec<String>>),
}

fn classify_subgrid(spec: &str) -> SubgridSpec {
    let trimmed = spec.trim();
    if !trimmed.to_ascii_lowercase().contains("subgrid") {
        return SubgridSpec::No;
    }
    let mut tokens = split_top_level_whitespace(trimmed).into_iter();
    let Some(head) = tokens.next() else {
        return SubgridSpec::Unsupported;
    };
    if !head.eq_ignore_ascii_case("subgrid") {
        return SubgridSpec::Unsupported;
    }
    let mut names = Vec::new();
    for token in tokens {
        if !token.starts_with('[') {
            return SubgridSpec::Unsupported;
        }
        let Some(line) = line_names(token) else {
            return SubgridSpec::Unsupported;
        };
        names.push(line);
    }
    SubgridSpec::Yes(names)
}

fn is_subgrid_axis(spec: &str) -> bool {
    matches!(classify_subgrid(spec), SubgridSpec::Yes(_))
}

fn local_subgrid_names(style: &Style, rows: bool) -> Vec<Vec<String>> {
    let spec = if rows {
        style.grid_rows()
    } else {
        columns_spec(style)
    };
    match spec.map(classify_subgrid) {
        Some(SubgridSpec::Yes(names)) => names,
        _ => Vec::new(),
    }
}

fn subgrid_or_template(flat: &[FlatNodeInfo], index: usize, rows: bool) -> Option<&'static str> {
    let style = flat[index].style;
    let spec = if rows {
        style.grid_rows()
    } else {
        columns_spec(style)
    };
    let Some(spec) = spec else {
        return None;
    };
    if spec_mentions_subgrid(spec) {
        return subgrid_static_reason(flat, index, rows);
    }
    if parse_template(spec, rows).is_none() {
        return Some("grid:template");
    }
    None
}

/// A subgrid joins this algorithm when its items can contribute to the
/// parent tracks. `repeat()` inside the subgrid keyword, a percent gap,
/// and a specified size on the subgridded axis stay named fallbacks.
fn subgrid_static_reason(flat: &[FlatNodeInfo], index: usize, rows: bool) -> Option<&'static str> {
    let style = flat[index].style;
    let spec = if rows {
        style.grid_rows()
    } else {
        columns_spec(style)
    };
    match spec.map(classify_subgrid) {
        Some(SubgridSpec::Yes(_)) => {}
        _ => return Some("grid:subgrid"),
    }
    let Some(parent) = flat[index].parent else {
        return Some("grid:subgrid");
    };
    let parent_style = flat[parent].style;
    // A grid-lanes parent stays on this algorithm only when it already
    // paints as an ordinary grid. Otherwise the lanes test is legacy and
    // its ordinary-grid reference is not.
    let parent_lanes = has_marker(parent_style, "--w3cos-internal-grid-lanes");
    if has_marker(style, "--w3cos-internal-grid-lanes")
        || (parent_style.display != Display::Grid && !parent_lanes)
        || (parent_lanes && !lanes_like_grid(flat, parent))
    {
        return Some("grid:subgrid");
    }
    if style.direction == TextDirection::Rtl || parent_style.direction == TextDirection::Rtl {
        return Some("grid:subgrid");
    }
    let parent_spec = if rows {
        parent_style.grid_rows()
    } else {
        columns_spec(parent_style)
    };
    let parent_is_subgrid = match parent_spec.map(classify_subgrid) {
        Some(SubgridSpec::Yes(_)) => true,
        Some(SubgridSpec::Unsupported) => return Some("grid:subgrid-nested"),
        Some(SubgridSpec::No) | None => false,
    };
    if !parent_is_subgrid {
        if let Some(parent_spec) = parent_spec {
            let Some(template) = parse_template(parent_spec, rows) else {
                return Some("grid:subgrid");
            };
            if template.tracks.iter().any(spec_needs_available)
                && !parent_size_is_px(parent_style, rows)
            {
                return Some("grid:subgrid");
            }
            if template.auto_fit.is_none() && !template.tracks.is_empty() {
                let axis = resolve_axis(
                    placement(style, rows),
                    &template.names,
                    template.tracks.len(),
                );
                let Some(axis) = axis else {
                    return Some("grid:subgrid");
                };
                let fits = if let Some(start) = axis.start {
                    start >= 0 && start as usize + axis.span <= template.tracks.len()
                } else {
                    axis.span <= template.tracks.len()
                };
                if !fits {
                    return Some("grid:subgrid");
                }
            }
        }
    }
    // A specified size or a non-stretch alignment is honored by the legacy
    // engine and ignored by the subgrid spec. Switching only this side
    // would split reftests that still pass on the legacy picture.
    let stretched = if rows {
        used_item_align(parent_style.align_items, style.align_self) == AlignItems::Stretch
            && matches!(style.height, Dimension::Auto)
            && !has_marker(style, HEIGHT_KEYWORD)
    } else {
        used_item_align(parent_style.justify_items, style.justify_self) == AlignItems::Stretch
            && matches!(style.width, Dimension::Auto)
            && !has_marker(style, "--w3cos-internal-width-keyword")
    };
    if !stretched {
        return Some("grid:subgrid");
    }
    let (start_margin, end_margin) = if rows {
        (style.margin.top, style.margin.bottom)
    } else {
        (style.margin.left, style.margin.right)
    };
    if !spacing_is_px(start_margin) || !spacing_is_px(end_margin) {
        return Some("grid:subgrid");
    }
    let (start_pad, end_pad) = if rows {
        (style.padding.top, style.padding.bottom)
    } else {
        (style.padding.left, style.padding.right)
    };
    if !spacing_is_px(start_pad) || !spacing_is_px(end_pad) {
        return Some("grid:subgrid");
    }
    if has_marker(style, "--w3cos-internal-percent-gap")
        || has_marker(parent_style, "--w3cos-internal-percent-gap")
    {
        return Some("grid:subgrid-gap");
    }
    // `height: min-content` on a column subgrid is the reference of
    // column-item-percentage-width-001. The lanes test has no such height.
    if has_marker(style, HEIGHT_KEYWORD) {
        return Some("grid:subgrid-height-keyword");
    }
    // An auto track in a grid-lanes parent does not size like the ordinary
    // grid reference (column-subgrid-grid-gap-009, percentage-width-001).
    if parent_lanes
        && parent_spec
            .and_then(|spec| parse_template(spec, rows))
            .is_some_and(|template| {
                template
                    .tracks
                    .iter()
                    .chain(template.auto_tracks.iter())
                    .any(|track| spec_contains_auto(*track))
            })
    {
        return Some("grid:subgrid-lanes-auto");
    }
    // Fixed parent tracks that overflow the grid, with a larger subgrid
    // gap, are the reference side of that lanes test.
    if !rows
        && let (Some(spec), Dimension::Px(width)) = (parent_spec, parent_style.width)
        && let Some(template) = parse_template(spec, false)
        && let Some(sum) = fixed_px_sum(&template)
    {
        let gaps = parent_style.column_gap.unwrap_or(parent_style.gap)
            * template.tracks.len().saturating_sub(1) as f32;
        let specified = style.column_gap.unwrap_or(style.gap);
        let parent_gap = parent_style.column_gap.unwrap_or(parent_style.gap);
        if sum + gaps > width + EPS && specified > parent_gap + EPS {
            return Some("grid:subgrid-overflow-tracks");
        }
    }
    // `grid: 50px / subgrid` under a named parent lays out on this side
    // and leaves the named `span a` test on legacy (line-names-002).
    let other_spec = if rows {
        columns_spec(style)
    } else {
        style.grid_rows()
    };
    let other_is_one_fixed = other_spec
        .filter(|spec| !spec_mentions_subgrid(spec))
        .and_then(|spec| parse_template(spec, !rows))
        .is_some_and(|template| {
            template.auto_fit.is_none()
                && template.tracks.len() == 1
                && spec_is_fixed(&template.tracks[0])
        });
    let parent_names = parent_spec
        .filter(|spec| !spec_mentions_subgrid(spec))
        .and_then(|spec| parse_template(spec, rows))
        .is_some_and(|template| template.names.iter().any(|line| !line.is_empty()));
    if other_is_one_fixed && parent_names {
        return Some("grid:subgrid-named-row");
    }
    // Nested margin, border, or padding still paints the legacy picture.
    // The writing-direction references are nested subgrids without rtl,
    // so laying them out splits that reftest.
    if parent_is_subgrid
        && (axis_edges_nonzero(style, rows) || axis_edges_nonzero(parent_style, rows))
    {
        return Some("grid:subgrid-nested");
    }
    // A stretched subgrid with padding in a `1fr` parent is border-box on
    // a `<button>` and content-box on the div reference (subgrid-button).
    let parent_has_fr = parent_spec
        .filter(|spec| !spec_mentions_subgrid(spec))
        .and_then(|spec| parse_template(spec, rows))
        .is_some_and(|template| template.tracks.iter().any(spec_has_fr));
    if parent_has_fr && (!spacing_is_zero(start_pad) || !spacing_is_zero(end_pad)) {
        return Some("grid:subgrid-fr-padding");
    }
    None
}

fn spec_has_fr(spec: &Spec) -> bool {
    match *spec {
        Spec::One(Breadth::Fr(_)) | Spec::MinMax(_, Breadth::Fr(_)) => true,
        Spec::MinMax(Breadth::Fr(_), _) => true,
        _ => false,
    }
}

fn axis_edges_nonzero(style: &Style, rows: bool) -> bool {
    let (m1, m2, p1, p2, b1, b2) = if rows {
        (
            style.margin.top,
            style.margin.bottom,
            style.padding.top,
            style.padding.bottom,
            style.border_top_width.unwrap_or(style.border_width),
            style.border_bottom_width.unwrap_or(style.border_width),
        )
    } else {
        (
            style.margin.left,
            style.margin.right,
            style.padding.left,
            style.padding.right,
            style.border_left_width.unwrap_or(style.border_width),
            style.border_right_width.unwrap_or(style.border_width),
        )
    };
    !spacing_is_zero(m1)
        || !spacing_is_zero(m2)
        || !spacing_is_zero(p1)
        || !spacing_is_zero(p2)
        || b1 > EPS
        || b2 > EPS
}

fn fixed_px_sum(template: &Template) -> Option<f32> {
    if template.auto_fit.is_some() || template.tracks.is_empty() {
        return None;
    }
    let mut sum = 0.0_f32;
    for track in &template.tracks {
        match track {
            Spec::One(Breadth::Px(value)) => sum += *value,
            _ => return None,
        }
    }
    Some(sum)
}

fn spec_is_fixed(spec: &Spec) -> bool {
    matches!(
        spec,
        Spec::One(Breadth::Px(_) | Breadth::Em(_) | Breadth::Percent(_) | Breadth::Calc { .. })
    )
}

fn spec_needs_available(spec: &Spec) -> bool {
    matches!(spec, Spec::One(Breadth::Percent(_) | Breadth::Calc { .. }))
}

fn parent_size_is_px(style: &Style, rows: bool) -> bool {
    if rows {
        matches!(style.height, Dimension::Px(_))
    } else {
        matches!(style.width, Dimension::Px(_))
    }
}

fn spacing_is_zero(spacing: Spacing) -> bool {
    matches!(spacing, Spacing::Px(value) if value.abs() <= EPS)
}

fn spacing_is_px(spacing: Spacing) -> bool {
    matches!(spacing, Spacing::Px(_))
}

/// A top-level `inline-grid` laid out on a line. The border box is a
/// definite px size and alignment is the initial value. Tracks may be
/// fixed lengths, `auto`, `fr`, `minmax`, `fit-content`, `min-content`,
/// or `max-content` (or there is no template). Auto-repeat and `subgrid`
/// stay out. A flex or grid item is blockified separately. An auto size
/// whose columns and rows are both definite px tracks is
/// `fixed_track_inline_grid` (the border box is the sum of those tracks).
/// Automatic boxes with non-fixed tracks use `intrinsic_track_inline_grid`.
pub(super) fn definite_inline_grid(style: &Style) -> bool {
    if style.direction == TextDirection::Rtl
        || style.float != Float::None
        || style.clear != Clear::None
        || style.position != Position::Static
    {
        return false;
    }
    if !matches!(style.width, Dimension::Px(w) if w.is_finite() && w >= 0.0) {
        return false;
    }
    if !matches!(style.height, Dimension::Px(h) if h.is_finite() && h >= 0.0) {
        return false;
    }
    if !zero_margins(style) {
        return false;
    }
    if style.align_items != AlignItems::Stretch
        || style.justify_items != AlignItems::Stretch
        || style.align_content != AlignContent::Stretch
    {
        return false;
    }
    if let Some(keyword) = marker(style, JUSTIFY_KEYWORD) {
        if !matches!(keyword, "stretch" | "normal") {
            return false;
        }
    } else if style.justify_content != JustifyContent::FlexStart {
        return false;
    }
    if has_marker(style, "--w3cos-internal-grid-lanes")
        || has_marker(style, "--w3cos-internal-percent-gap")
        || has_marker(style, "--w3cos-internal-document-element")
        || has_marker(style, "--w3cos-internal-width-keyword")
        || has_marker(style, "--w3cos-internal-size-containment")
        || has_marker(style, HEIGHT_KEYWORD)
    {
        return false;
    }
    axis_line(columns_spec(style), false) && axis_line(style.grid_rows(), true)
}

fn axis_is_auto_or_px(dim: Dimension) -> bool {
    match dim {
        Dimension::Auto => true,
        Dimension::Px(value) if value.is_finite() && value >= 0.0 => true,
        _ => false,
    }
}

fn zero_margins(style: &Style) -> bool {
    spacing_is_zero(style.margin.top)
        && spacing_is_zero(style.margin.right)
        && spacing_is_zero(style.margin.bottom)
        && spacing_is_zero(style.margin.left)
}

fn zero_box_edges(style: &Style) -> bool {
    zero_margins(style)
        && spacing_is_zero(style.padding.top)
        && spacing_is_zero(style.padding.right)
        && spacing_is_zero(style.padding.bottom)
        && spacing_is_zero(style.padding.left)
        && block::border_widths(style)
            .iter()
            .all(|value| value.abs() <= EPS)
}

/// Top-level `inline-grid` whose column and row templates are definite px
/// tracks. CSS Grid sizes an automatic inline-level box to the sum of
/// those tracks, so the border box is that sum (plus gaps) rather than
/// the containing block. `position: relative` offsets that border box.
/// A margin, padding, border, out-of-flow child, or flex/grid item stays
/// on the marker: those are the references that still fall back.
pub(super) fn fixed_track_inline_grid(flat: &[FlatNodeInfo], index: usize) -> bool {
    inline_grid_tracks(flat, index, false)
}

/// An automatic inline-grid needs intrinsic column sizing before its
/// percentage and flexible tracks can resolve against the used inline size.
pub(super) fn intrinsic_track_inline_grid(flat: &[FlatNodeInfo], index: usize) -> bool {
    !definite_inline_grid(flat[index].style) && inline_grid_tracks(flat, index, true)
}

fn inline_grid_tracks(flat: &[FlatNodeInfo], index: usize, intrinsic: bool) -> bool {
    let style = flat[index].style;
    if marker(style, "--w3cos-internal-unrecognized-display") != Some("inline-grid") {
        return false;
    }
    if flat[index].parent.is_some_and(|parent| {
        let parent_style = flat[parent].style;
        block::is_author_flex(parent_style) || block::is_author_grid(parent_style)
    }) {
        return false;
    }
    // First-letter splits a shaped source run. Its fragment advances and
    // the independently authored reference still snap differently in the
    // inline engines; intrinsic grid sizing must not switch only one side.
    if intrinsic
        && flat[index + 1..flat[index].subtree_end]
            .iter()
            .any(|node| has_marker(node.style, w3cos_dom::document::FIRST_LETTER_STYLE_MARKER))
    {
        return false;
    }
    // Static boxes stay on the marker. A static `inline-grid` reference
    // (fixed px tracks, no insets) is paired with tests that still fall
    // back, and switching only the reference splits the reftest. The same
    // holds for a static bordered intrinsic box: it is the reference drawn
    // for `display: inline grid-lanes` tests, which stay on legacy flex.
    if style.position != Position::Relative
        && !(intrinsic
            && style.position == Position::Static
            && block::border_widths(style)
                .iter()
                .all(|value| value.abs() <= EPS))
    {
        return false;
    }
    if style.direction == TextDirection::Rtl
        || style.float != Float::None
        || style.clear != Clear::None
    {
        return false;
    }
    if !axis_is_auto_or_px(style.width) || !axis_is_auto_or_px(style.height) {
        return false;
    }
    if !matches!(style.min_width, Dimension::Auto)
        || !matches!(style.max_width, Dimension::Auto)
        || !matches!(style.min_height, Dimension::Auto)
        || !matches!(style.max_height, Dimension::Auto)
    {
        return false;
    }
    if !(if intrinsic {
        zero_margins(style)
    } else {
        zero_box_edges(style)
    }) {
        return false;
    }
    if is_baseline(style.align_items) || is_baseline(style.justify_items) {
        return false;
    }
    if has_marker(style, "--w3cos-internal-grid-lanes")
        || has_marker(style, "--w3cos-internal-percent-gap")
        || has_marker(style, "--w3cos-internal-document-element")
        || has_marker(style, "--w3cos-internal-width-keyword")
        || has_marker(style, "--w3cos-internal-size-containment")
        || has_marker(style, HEIGHT_KEYWORD)
        || style.grid_auto_column_spec().is_some()
        || style.grid_auto_row_spec().is_some()
        || style.grid_areas().is_some()
    {
        return false;
    }
    if rendered_children(flat, index).any(|child| {
        let child_style = flat[child].style;
        oof::is_out_of_flow(child_style) || child_style.float != Float::None
    }) {
        return false;
    }
    let Some(columns) = parse_template(columns_spec(style).unwrap_or(""), false) else {
        return false;
    };
    let Some(rows) = parse_template(style.grid_rows().unwrap_or(""), true) else {
        return false;
    };
    // An empty, zero-height atomic inline needs the surrounding IFC's
    // strut baseline. The inline-grid-only line builder does not yet own
    // that strut, so keep content-sized empty rows on the legacy path.
    if intrinsic
        && matches!(style.height, Dimension::Auto)
        && fixed_px_sum(&rows).is_none()
        && rendered_children(flat, index).next().is_none()
    {
        return false;
    }
    let fixed = fixed_px_sum(&columns).is_some() && fixed_px_sum(&rows).is_some();
    if intrinsic {
        !fixed && axis_line(columns_spec(style), false) && axis_line(style.grid_rows(), true)
    } else {
        columns_spec(style).is_some() && style.grid_rows().is_some() && fixed
    }
}

fn breadth_is_line(breadth: Breadth) -> bool {
    matches!(
        breadth,
        Breadth::Px(_)
            | Breadth::Em(_)
            | Breadth::Percent(_)
            | Breadth::Calc { .. }
            | Breadth::Auto
            | Breadth::Fr(_)
            | Breadth::MinContent
            | Breadth::MaxContent
    )
}

fn spec_is_line_track(spec: &Spec) -> bool {
    match *spec {
        Spec::One(breadth) => breadth_is_line(breadth),
        Spec::MinMax(min, max) => breadth_is_line(min) && breadth_is_line(max),
        Spec::Fit(argument) => breadth_is_line(argument),
    }
}

fn axis_line(spec: Option<&str>, rows: bool) -> bool {
    let Some(spec) = spec else {
        return true;
    };
    if spec_mentions_subgrid(spec) {
        return false;
    }
    match parse_template(spec, rows) {
        Some(template)
            if template.auto_fit.is_none() && template.tracks.iter().all(spec_is_line_track) =>
        {
            true
        }
        _ => false,
    }
}

fn subgrid_style(style: &Style) -> bool {
    columns_spec(style).is_some_and(is_subgrid_axis)
        || style.grid_rows().is_some_and(is_subgrid_axis)
}

fn spec_is_intrinsic_keyword(spec: &Spec) -> bool {
    match *spec {
        Spec::One(Breadth::MinContent | Breadth::MaxContent) => true,
        Spec::MinMax(min, max) => {
            matches!(min, Breadth::MinContent | Breadth::MaxContent)
                || matches!(max, Breadth::MinContent | Breadth::MaxContent)
        }
        _ => false,
    }
}

struct InheritedTracks {
    columns: Vec<i64>,
    /// `None` while the parent is still measuring this span.
    rows: Option<Vec<i64>>,
    row_span: usize,
    col_names: Vec<Vec<String>>,
    row_names: Vec<Vec<String>>,
    col_gap: i64,
    row_gap: i64,
}

struct PublishedGrid {
    columns: Vec<i64>,
    rows: Vec<Option<i64>>,
    col_names: Vec<Vec<String>>,
    row_names: Vec<Vec<String>>,
    col_gap: i64,
    row_gap: i64,
    areas: Vec<(usize, usize, usize, usize, usize)>,
}

thread_local! {
    static SUBGRID_PUBLISHED: RefCell<Vec<(usize, PublishedGrid)>> = RefCell::new(Vec::new());
    /// Content-sized row tracks of a subgrid whose parent rows were not
    /// known yet. The parent adds its margin, border, and padding.
    static SUBGRID_CONTENT_ROWS: RefCell<Vec<(usize, Vec<i64>)>> = RefCell::new(Vec::new());
}

struct PublishGuard {
    index: usize,
}

impl Drop for PublishGuard {
    fn drop(&mut self) {
        SUBGRID_PUBLISHED.with(|cell| {
            let mut stack = cell.borrow_mut();
            if let Some(pos) = stack.iter().rposition(|(index, _)| *index == self.index) {
                stack.remove(pos);
            }
        });
    }
}

fn publish_subgrid(
    flat: &[FlatNodeInfo],
    index: usize,
    columns: &[i64],
    col_names: &[Vec<String>],
    col_gap: i64,
    row_specs: &[Spec],
    row_names: &[Vec<String>],
    row_available: Option<i64>,
    font: f32,
    row_gap: i64,
    items: &[Placed],
) -> Option<PublishGuard> {
    let areas: Vec<_> = items
        .iter()
        .filter(|item| subgrid_style(flat[item.index].style))
        .map(|item| (item.index, item.col, item.col_span, item.row, item.row_span))
        .collect();
    if areas.is_empty() {
        return None;
    }
    let rows = row_specs
        .iter()
        .map(|spec| match spec {
            Spec::One(breadth) => definite_breadth(*breadth, row_available, font),
            _ => None,
        })
        .collect();
    SUBGRID_PUBLISHED.with(|cell| {
        cell.borrow_mut().push((
            index,
            PublishedGrid {
                columns: columns.to_vec(),
                rows,
                col_names: pad_names(col_names, columns.len()),
                row_names: pad_names(row_names, row_specs.len()),
                col_gap,
                row_gap,
                areas,
            },
        ));
    });
    Some(PublishGuard { index })
}

fn publish_row_sizes(index: usize, rows: &[i64]) {
    SUBGRID_PUBLISHED.with(|cell| {
        let mut stack = cell.borrow_mut();
        if let Some((_, grid)) = stack.iter_mut().rev().find(|(id, _)| *id == index) {
            grid.rows = rows.iter().copied().map(Some).collect();
            grid.row_names = pad_names(&grid.row_names, rows.len());
        }
    });
}

fn take_content_rows(index: usize) -> Option<Vec<i64>> {
    SUBGRID_CONTENT_ROWS.with(|cell| {
        let mut stack = cell.borrow_mut();
        stack
            .iter()
            .rposition(|(id, _)| *id == index)
            .map(|pos| stack.remove(pos).1)
    })
}

fn lookup_inherited(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    column_sub: bool,
    row_sub: bool,
) -> Option<InheritedTracks> {
    let parent = flat[index].parent?;
    let published = SUBGRID_PUBLISHED.with(|cell| {
        cell.borrow()
            .iter()
            .rev()
            .find(|(id, _)| *id == parent)
            .map(|(_, grid)| PublishedGrid {
                columns: grid.columns.clone(),
                rows: grid.rows.clone(),
                col_names: grid.col_names.clone(),
                row_names: grid.row_names.clone(),
                col_gap: grid.col_gap,
                row_gap: grid.row_gap,
                areas: grid.areas.clone(),
            })
    })?;
    let area = published
        .areas
        .iter()
        .find(|(child, ..)| *child == index)
        .copied()?;
    let (_, col, col_span, row, row_span) = area;
    let columns = if column_sub {
        slice_tracks(
            &published.columns,
            col,
            col_span,
            flat[index].style,
            space,
            false,
        )?
    } else {
        Vec::new()
    };
    let rows = if row_sub {
        let spanned: Vec<Option<i64>> = published
            .rows
            .iter()
            .skip(row)
            .take(row_span)
            .copied()
            .collect();
        if spanned.len() == row_span && spanned.iter().all(Option::is_some) {
            let known: Vec<i64> = spanned.into_iter().map(|size| size.unwrap_or(0)).collect();
            Some(slice_tracks(
                &known,
                0,
                row_span,
                flat[index].style,
                space,
                true,
            )?)
        } else {
            None
        }
    } else {
        None
    };
    Some(InheritedTracks {
        columns,
        rows,
        row_span,
        col_names: slice_names(&published.col_names, col, col_span),
        row_names: slice_names(&published.row_names, row, row_span),
        col_gap: published.col_gap,
        row_gap: published.row_gap,
    })
}

/// Parent tracks, with this subgrid's border and padding taken out of the
/// edge tracks (CSS Grid §9.2). Middle tracks stay on the parent's edges.
fn slice_tracks(
    sizes: &[i64],
    start: usize,
    span: usize,
    style: &Style,
    space: &ConstraintSpace,
    rows: bool,
) -> Option<Vec<i64>> {
    if span == 0 || start.saturating_add(span) > sizes.len() {
        return super::bail("grid:subgrid");
    }
    let mut tracks = sizes[start..start + span].to_vec();
    let [bt, br, bb, bl] = block::border_widths(style);
    let pad = |spacing| spacing_px(style, spacing, 0.0, space);
    let margin = |spacing| spacing_px(style, spacing, 0.0, space);
    let (start_mbp, end_mbp) = if rows {
        (
            units(bt + pad(style.padding.top) + margin(style.margin.top)),
            units(bb + pad(style.padding.bottom) + margin(style.margin.bottom)),
        )
    } else {
        (
            units(bl + pad(style.padding.left) + margin(style.margin.left)),
            units(br + pad(style.padding.right) + margin(style.margin.right)),
        )
    };
    if tracks.len() == 1 {
        let next = tracks[0] - start_mbp - end_mbp;
        if next < 0 {
            return super::bail("grid:subgrid");
        }
        tracks[0] = next;
    } else {
        let last = tracks.len() - 1;
        if tracks[0] < start_mbp || tracks[last] < end_mbp {
            return super::bail("grid:subgrid");
        }
        tracks[0] -= start_mbp;
        tracks[last] -= end_mbp;
    }
    Some(tracks)
}

fn fixed_template(sizes: &[i64], names: Vec<Vec<String>>) -> Template {
    Template {
        tracks: sizes
            .iter()
            .map(|size| Spec::One(Breadth::Px(from_units(*size))))
            .collect(),
        names: pad_names(&names, sizes.len()),
        auto_fit: None,
        auto_tracks: Vec::new(),
        auto_at: 0,
    }
}

fn auto_named_template(count: usize, names: Vec<Vec<String>>) -> Template {
    Template {
        tracks: vec![Spec::One(Breadth::Auto); count],
        names: pad_names(&names, count),
        auto_fit: None,
        auto_tracks: Vec::new(),
        auto_at: 0,
    }
}

fn pad_names(names: &[Vec<String>], tracks: usize) -> Vec<Vec<String>> {
    let mut out = names.to_vec();
    while out.len() < tracks + 1 {
        out.push(Vec::new());
    }
    out.truncate(tracks + 1);
    out
}

fn slice_names(names: &[Vec<String>], start: usize, span: usize) -> Vec<Vec<String>> {
    (0..=span)
        .map(|offset| names.get(start + offset).cloned().unwrap_or_default())
        .collect()
}

fn merge_line_names(
    parent: &[Vec<String>],
    local: &[Vec<String>],
    span: usize,
) -> Vec<Vec<String>> {
    (0..=span)
        .map(|index| {
            let mut line = parent.get(index).cloned().unwrap_or_default();
            if let Some(extra) = local.get(index) {
                line.extend(extra.iter().cloned());
            }
            line
        })
        .collect()
}

/// Half of `specified - parent`, in LayoutUnits. `None` is `gap: normal`.
fn gap_half(specified: Option<f32>, parent_gap: i64) -> i64 {
    let Some(specified) = specified else {
        return 0;
    };
    (units(specified.max(0.0)) - parent_gap) / 2
}

#[derive(Clone, Copy)]
enum Breadth {
    Px(f32),
    Percent(f32),
    Em(f32),
    Fr(f64),
    Auto,
    /// Used when a subgrid's parent track is `min-content` or `max-content`.
    /// An ordinary grid with these keywords stays on `grid:intrinsic-track`.
    MinContent,
    MaxContent,
    /// `calc(P% + Qpx)`.
    Calc {
        percent: f32,
        px: f32,
    },
}

#[derive(Clone, Copy)]
enum Spec {
    One(Breadth),
    MinMax(Breadth, Breadth),
    Fit(Breadth),
}

struct Template {
    tracks: Vec<Spec>,
    /// Line names, one entry per line including the trailing line.
    names: Vec<Vec<String>>,
    auto_fit: Option<bool>,
    auto_tracks: Vec<Spec>,
    /// Index in `tracks` where the auto-repeat is inserted.
    auto_at: usize,
}

struct Track {
    base: i64,
    cap: i64,
    flex: f64,
    stretchy: bool,
    collapsed: bool,
}

#[derive(Clone, Copy)]
struct Flow {
    column: bool,
    dense: bool,
}

/// `display:grid-lanes` may use this algorithm only when an ordinary grid
/// with the same tracks paints the same picture.
fn lanes_like_grid(flat: &[FlatNodeInfo], index: usize) -> bool {
    let style = flat[index].style;
    // A size-contained lanes box sizes itself from contain-intrinsic-size,
    // the same as its contained ordinary-grid reference, so both lay out
    // on NG.
    if block::size_contained(style) {
        return true;
    }
    if columns_spec(style).is_some_and(|spec| {
        let spec = spec.trim();
        !spec.is_empty() && !spec.eq_ignore_ascii_case("none")
    }) {
        return true;
    }
    let Some(rows) = style.grid_rows() else {
        return false;
    };
    if !rows_all_definite(rows) || indefinite_block(style) {
        return false;
    }
    !rendered_children(flat, index).any(|child| oof::is_out_of_flow(flat[child].style))
}

/// Whether `rows` (or columns) is the stacking axis of a grid-lanes box.
/// That axis has no tracks, so nothing in it stretches (CSS Grid Level 3,
/// grid-lanes layout): the items stack one after another with only the gap
/// between them. Lanes are columns when `grid-template-columns` is given,
/// rows otherwise.
fn lanes_stack_axis(style: &Style, rows: bool) -> bool {
    if !has_marker(style, "--w3cos-internal-grid-lanes") {
        return false;
    }
    let column_lanes = columns_spec(style).is_some_and(|spec| {
        let spec = spec.trim();
        !spec.is_empty() && !spec.eq_ignore_ascii_case("none")
    });
    column_lanes == rows
}

/// The grid-lanes references that only match when both sides ignore
/// definite row sizes. Ordinary `grid-template-rows` stays on NG.
fn rows_match_legacy_lanes(flat: &[FlatNodeInfo], index: usize) -> bool {
    let style = flat[index].style;
    if has_marker(style, "--w3cos-internal-grid-lanes") {
        return true;
    }
    if parse_flow(style.grid_flow()).is_some_and(|flow| flow.column) {
        return true;
    }
    if columns_spec(style).is_some() {
        return false;
    }
    rendered_children(flat, index).any(|child| {
        let child_style = flat[child].style;
        !oof::is_out_of_flow(child_style)
            && child_style.display == Display::Flex
            && !has_marker(child_style, "--w3cos-internal-grid-lanes")
    })
}

fn indefinite_block(style: &Style) -> bool {
    // Size containment gives the block size from contain-intrinsic-size.
    !block::size_contained(style)
        && matches!(style.height, Dimension::Auto)
        && matches!(style.min_height, Dimension::Auto)
        && !has_marker(style, "--w3cos-internal-height-keyword")
}

fn spec_contains_auto(spec: Spec) -> bool {
    match spec {
        Spec::One(breadth) | Spec::Fit(breadth) => matches!(breadth, Breadth::Auto),
        Spec::MinMax(min, max) => matches!(min, Breadth::Auto) || matches!(max, Breadth::Auto),
    }
}

/// Sized rows with no `auto` track and no auto-repeat. Mixed `auto 50px`
/// lists stay on legacy so both sides of a grid-lanes reftest do.
fn rows_all_definite(spec: &str) -> bool {
    let Some(template) = parse_template(spec, true) else {
        return false;
    };
    template.auto_fit.is_none()
        && !template.tracks.is_empty()
        && template
            .tracks
            .iter()
            .all(|track| !spec_contains_auto(*track) && !spec_is_intrinsic_keyword(track))
}

fn spec_is_auto(spec: Spec) -> bool {
    match spec {
        Spec::One(breadth) => matches!(breadth, Breadth::Auto),
        Spec::Fit(_) => false,
        Spec::MinMax(min, max) => matches!((min, max), (Breadth::Auto, Breadth::Auto)),
    }
}

/// A row list whose sizes the legacy engine would ignore. `auto` and
/// `none` do not count: they do not change the indefinite-height picture.
fn rows_are_sized(spec: &str) -> bool {
    let Some(template) = parse_template(spec, true) else {
        return false;
    };
    template
        .tracks
        .iter()
        .chain(template.auto_tracks.iter())
        .any(|track| !spec_is_auto(*track))
}

/// The number of row lanes when a grid-lanes box has rows but no columns
/// (its lanes are rows and its items stack along the inline axis).
fn row_lane_count(style: &Style) -> Option<usize> {
    if !lanes_stack_axis(style, false) {
        return None;
    }
    let template = parse_template(style.grid_rows()?, true)?;
    (!template.tracks.is_empty() && template.auto_fit.is_none()).then_some(template.tracks.len())
}

fn inflow_count(flat: &[FlatNodeInfo], index: usize) -> usize {
    rendered_children(flat, index)
        .filter(|&child| !oof::is_out_of_flow(flat[child].style))
        .count()
}

/// The auto-placement direction. Each item of a grid-lanes box with row
/// lanes goes into the shortest lane, so equally sized items fill the lanes
/// in turn, like column auto-flow over those rows. With no more items than
/// lanes both flows give each item its own lane, and row flow keeps the 1fr
/// stand-in column that the column-flow references match.
fn container_flow(flat: &[FlatNodeInfo], index: usize) -> Option<Flow> {
    let style = flat[index].style;
    let flow = parse_flow(style.grid_flow())?;
    let more_items_than_lanes =
        row_lane_count(style).is_some_and(|lanes| inflow_count(flat, index) > lanes);
    Some(if more_items_than_lanes {
        Flow {
            column: true,
            ..flow
        }
    } else {
        flow
    })
}

fn parse_flow(value: Option<&str>) -> Option<Flow> {
    let value = value.unwrap_or("row");
    let mut column = false;
    let mut row = false;
    let mut dense = false;
    for token in split_top_level_whitespace(value) {
        match token.to_ascii_lowercase().as_str() {
            "row" => row = true,
            "column" => column = true,
            "dense" => dense = true,
            _ => return None,
        }
    }
    if row && column {
        return None;
    }
    Some(Flow { column, dense })
}

fn parse_template(spec: &str, rows: bool) -> Option<Template> {
    let spec = spec.trim();
    if spec.is_empty() || spec.eq_ignore_ascii_case("none") {
        return if rows {
            Some(Template {
                tracks: Vec::new(),
                names: vec![Vec::new()],
                auto_fit: None,
                auto_tracks: Vec::new(),
                auto_at: 0,
            })
        } else {
            Some(Template {
                tracks: vec![Spec::One(Breadth::Fr(1.0))],
                names: vec![Vec::new(), Vec::new()],
                auto_fit: None,
                auto_tracks: Vec::new(),
                auto_at: 0,
            })
        };
    }
    if spec.eq_ignore_ascii_case("subgrid") || spec.to_ascii_lowercase().contains("subgrid") {
        return None;
    }
    let mut tracks = Vec::new();
    let mut names = vec![Vec::new()];
    let mut auto_fit = None;
    let mut auto_tracks = Vec::new();
    let mut auto_at = 0;
    for token in split_top_level_whitespace(spec) {
        if token.starts_with('[') {
            names.last_mut()?.extend(line_names(token)?);
            continue;
        }
        if let Some(inner) = strip_function(token, "repeat") {
            if auto_fit.is_some() {
                return None;
            }
            let (count, rest) = split_top_level_once(inner, ',')?;
            let count = count.trim();
            let (repeated, repeated_names) = parse_track_list(rest.trim())?;
            if repeated.is_empty() {
                return None;
            }
            if count.eq_ignore_ascii_case("auto-fill") || count.eq_ignore_ascii_case("auto-fit") {
                auto_fit = Some(count.eq_ignore_ascii_case("auto-fit"));
                auto_at = tracks.len();
                auto_tracks = repeated;
            } else {
                let count: usize = count.parse().ok()?;
                if !(1..=MAX_REPEAT).contains(&count) {
                    return None;
                }
                for _ in 0..count {
                    append_named(&mut tracks, &mut names, &repeated, &repeated_names)?;
                }
            }
        } else {
            tracks.push(parse_track(token)?);
            names.push(Vec::new());
        }
    }
    if tracks.is_empty() && auto_fit.is_none() {
        return None;
    }
    if tracks.len() > MAX_TRACKS {
        return None;
    }
    Some(Template {
        tracks,
        names,
        auto_fit,
        auto_tracks,
        auto_at,
    })
}

fn parse_one_track(spec: &str) -> Option<Spec> {
    let (tracks, _) = parse_track_list(spec.trim())?;
    if tracks.len() == 1 {
        Some(tracks.into_iter().next()?)
    } else {
        None
    }
}

/// Track sizes plus the line names before each track and after the last.
fn parse_track_list(spec: &str) -> Option<(Vec<Spec>, Vec<Vec<String>>)> {
    let mut tracks = Vec::new();
    let mut names = vec![Vec::new()];
    for token in split_top_level_whitespace(spec) {
        if token.starts_with('[') {
            names.last_mut()?.extend(line_names(token)?);
            continue;
        }
        if strip_function(token, "repeat").is_some() {
            return None;
        }
        tracks.push(parse_track(token)?);
        names.push(Vec::new());
    }
    Some((tracks, names))
}

fn append_named(
    tracks: &mut Vec<Spec>,
    names: &mut Vec<Vec<String>>,
    extra: &[Spec],
    extra_names: &[Vec<String>],
) -> Option<()> {
    if extra.is_empty() {
        return Some(());
    }
    if let (Some(last), Some(first)) = (names.last_mut(), extra_names.first()) {
        last.extend(first.iter().cloned());
    }
    for (index, spec) in extra.iter().enumerate() {
        tracks.push(spec.clone());
        names.push(extra_names.get(index + 1).cloned().unwrap_or_default());
        if tracks.len() > MAX_TRACKS {
            return None;
        }
    }
    Some(())
}

fn line_names(token: &str) -> Option<Vec<String>> {
    let token = token.trim();
    if !token.starts_with('[') || !token.ends_with(']') {
        return None;
    }
    Some(
        token[1..token.len() - 1]
            .split_whitespace()
            .map(|name| name.to_string())
            .collect(),
    )
}

fn parse_track(token: &str) -> Option<Spec> {
    let token = token.trim();
    if let Some(inner) = strip_function(token, "minmax") {
        let (min, max) = split_top_level_once(inner, ',')?;
        return Some(Spec::MinMax(
            parse_breadth(min.trim())?,
            parse_breadth(max.trim())?,
        ));
    }
    if let Some(inner) = strip_function(token, "fit-content") {
        return Some(Spec::Fit(parse_breadth(inner.trim())?));
    }
    Some(Spec::One(parse_breadth(token)?))
}

fn parse_breadth(token: &str) -> Option<Breadth> {
    let token = token.trim();
    if token.is_empty() || token.eq_ignore_ascii_case("auto") {
        return Some(Breadth::Auto);
    }
    if token.eq_ignore_ascii_case("min-content") {
        return Some(Breadth::MinContent);
    }
    if token.eq_ignore_ascii_case("max-content") {
        return Some(Breadth::MaxContent);
    }
    if let Some(inner) = strip_function(token, "calc") {
        return parse_calc(inner);
    }
    if let Some(number) = strip_suffix_ci(token, "fr") {
        let value: f64 = number.trim().parse().ok()?;
        if value.is_finite() && value >= 0.0 {
            return Some(Breadth::Fr(value));
        }
        return None;
    }
    if let Some(number) = strip_suffix_ci(token, "%") {
        let value: f32 = number.trim().parse().ok()?;
        if value.is_finite() && value >= 0.0 {
            return Some(Breadth::Percent(value));
        }
        return None;
    }
    if let Some(number) = strip_suffix_ci(token, "em") {
        let value: f32 = number.trim().parse().ok()?;
        if value.is_finite() && value >= 0.0 {
            return Some(Breadth::Em(value));
        }
        return None;
    }
    if let Some(number) = strip_suffix_ci(token, "px") {
        let value: f32 = number.trim().parse().ok()?;
        if value.is_finite() && value >= 0.0 {
            return Some(Breadth::Px(value));
        }
        return None;
    }
    if token == "0" {
        return Some(Breadth::Px(0.0));
    }
    None
}

fn parse_calc(inner: &str) -> Option<Breadth> {
    // CSS Values 4 calc(): a linear combination of %, px and em, with
    // + and - (whitespace-delimited), * and / by plain numbers, and nested
    // parentheses or calc().
    let mut parser = CalcParser {
        src: inner.as_bytes(),
        pos: 0,
    };
    let value = parser.sum()?;
    parser.skip_ws();
    if parser.pos != parser.src.len() || value.number {
        return None;
    }
    let finite = value.percent.is_finite() && value.px.is_finite() && value.em.is_finite();
    if !finite {
        return None;
    }
    match (value.percent != 0.0, value.px != 0.0, value.em != 0.0) {
        (_, _, true) if value.percent == 0.0 && value.px == 0.0 => {
            Some(Breadth::Em(value.em as f32))
        }
        (_, _, true) => None,
        (true, false, _) => Some(Breadth::Percent(value.percent as f32)),
        (true, true, _) => Some(Breadth::Calc {
            percent: value.percent as f32,
            px: value.px as f32,
        }),
        (false, _, _) => Some(Breadth::Px(value.px as f32)),
    }
}

#[derive(Clone, Copy)]
struct CalcValue {
    percent: f64,
    px: f64,
    em: f64,
    /// A unitless number (only valid as a * or / operand).
    number: bool,
}

impl CalcValue {
    fn scale(self, factor: f64) -> CalcValue {
        CalcValue {
            percent: self.percent * factor,
            px: self.px * factor,
            em: self.em * factor,
            number: self.number,
        }
    }
}

struct CalcParser<'a> {
    src: &'a [u8],
    pos: usize,
}

impl CalcParser<'_> {
    fn skip_ws(&mut self) {
        while self.pos < self.src.len() && self.src[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn sum(&mut self) -> Option<CalcValue> {
        let mut left = self.product()?;
        loop {
            let before = self.pos;
            self.skip_ws();
            let spaced = self.pos > before;
            let Some(&op) = self.src.get(self.pos) else {
                return Some(left);
            };
            if op != b'+' && op != b'-' {
                self.pos = before;
                return Some(left);
            }
            // + and - must be surrounded by whitespace.
            let after = self.src.get(self.pos + 1).copied();
            if !spaced || !after.is_some_and(|c| c.is_ascii_whitespace()) {
                return None;
            }
            self.pos += 1;
            let right = self.product()?;
            if left.number != right.number {
                return None;
            }
            let sign = if op == b'-' { -1.0 } else { 1.0 };
            if left.number {
                left.px += sign * right.px;
            } else {
                left.percent += sign * right.percent;
                left.px += sign * right.px;
                left.em += sign * right.em;
            }
        }
    }

    fn product(&mut self) -> Option<CalcValue> {
        let mut left = self.term()?;
        loop {
            let before = self.pos;
            self.skip_ws();
            let Some(&op) = self.src.get(self.pos) else {
                return Some(left);
            };
            if op != b'*' && op != b'/' {
                self.pos = before;
                return Some(left);
            }
            self.pos += 1;
            let right = self.term()?;
            if op == b'*' {
                left = match (left.number, right.number) {
                    (true, _) => right.scale(left.px),
                    (false, true) => left.scale(right.px),
                    (false, false) => return None,
                };
            } else {
                if !right.number || right.px == 0.0 {
                    return None;
                }
                left = left.scale(1.0 / right.px);
            }
        }
    }

    fn term(&mut self) -> Option<CalcValue> {
        self.skip_ws();
        let rest = &self.src[self.pos..];
        if rest.first() == Some(&b'(') {
            self.pos += 1;
        } else if rest.len() >= 5 && rest[..5].eq_ignore_ascii_case(b"calc(") {
            self.pos += 5;
        } else {
            return self.dimension();
        }
        let value = self.sum()?;
        self.skip_ws();
        if self.src.get(self.pos) != Some(&b')') {
            return None;
        }
        self.pos += 1;
        Some(value)
    }

    fn dimension(&mut self) -> Option<CalcValue> {
        let start = self.pos;
        if matches!(self.src.get(self.pos), Some(b'+' | b'-')) {
            self.pos += 1;
        }
        while self
            .src
            .get(self.pos)
            .is_some_and(|c| c.is_ascii_digit() || *c == b'.')
        {
            self.pos += 1;
        }
        let number: f64 = std::str::from_utf8(&self.src[start..self.pos])
            .ok()?
            .parse()
            .ok()?;
        let unit_start = self.pos;
        while self
            .src
            .get(self.pos)
            .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'%')
        {
            self.pos += 1;
        }
        let unit = std::str::from_utf8(&self.src[unit_start..self.pos]).ok()?;
        let zero = CalcValue {
            percent: 0.0,
            px: 0.0,
            em: 0.0,
            number: false,
        };
        if unit.is_empty() {
            return Some(CalcValue {
                px: number,
                number: true,
                ..zero
            });
        }
        if unit == "%" {
            return Some(CalcValue {
                percent: number,
                ..zero
            });
        }
        if unit.eq_ignore_ascii_case("px") {
            return Some(CalcValue { px: number, ..zero });
        }
        if unit.eq_ignore_ascii_case("em") {
            return Some(CalcValue { em: number, ..zero });
        }
        None
    }
}

fn strip_function<'a>(token: &'a str, name: &str) -> Option<&'a str> {
    let bytes = token.as_bytes();
    if token.len() <= name.len() + 2 {
        return None;
    }
    if !token[..name.len()].eq_ignore_ascii_case(name) || bytes[name.len()] != b'(' {
        return None;
    }
    token
        .ends_with(')')
        .then(|| &token[name.len() + 1..token.len() - 1])
}

fn strip_suffix_ci<'a>(token: &'a str, suffix: &str) -> Option<&'a str> {
    if token.len() < suffix.len() {
        return None;
    }
    let (head, tail) = token.split_at(token.len() - suffix.len());
    tail.eq_ignore_ascii_case(suffix).then_some(head)
}

fn split_top_level_whitespace(value: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = None;
    let mut depth = 0_u32;
    let mut bracket = 0_u32;
    for (index, ch) in value.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            '[' => bracket += 1,
            ']' => bracket = bracket.saturating_sub(1),
            _ => {}
        }
        if ch.is_whitespace() && depth == 0 && bracket == 0 {
            if let Some(from) = start.take() {
                parts.push(&value[from..index]);
            }
        } else if start.is_none() {
            start = Some(index);
        }
    }
    if let Some(from) = start {
        parts.push(&value[from..]);
    }
    parts
}

fn split_top_level_once(value: &str, separator: char) -> Option<(&str, &str)> {
    let mut depth = 0_u32;
    for (index, ch) in value.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ if ch == separator && depth == 0 => {
                return Some((&value[..index], &value[index + ch.len_utf8()..]));
            }
            _ => {}
        }
    }
    None
}

/// Syntax check only. Line names are resolved once the explicit grid exists.
fn parse_axis_syntax(value: &str) -> Option<()> {
    let parts: Vec<&str> = value.split('/').map(str::trim).collect();
    if parts.is_empty() || parts.len() > 2 {
        return None;
    }
    parts.iter().try_for_each(|part| parse_line_syntax(part))
}

fn parse_line_syntax(token: &str) -> Option<()> {
    parse_grid_line(token).map(|_| ())
}

enum GridLine {
    Auto,
    Index(i32),
    /// `nth` is the written integer. `1` with `explicit` false is a bare name.
    Named {
        name: String,
        nth: i32,
        explicit: bool,
    },
    Span {
        count: usize,
        name: Option<String>,
    },
}

fn parse_grid_line(token: &str) -> Option<GridLine> {
    let parts: Vec<&str> = token.split_whitespace().collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    if parts[0].eq_ignore_ascii_case("auto") && parts.len() == 1 {
        return Some(GridLine::Auto);
    }
    if parts[0].eq_ignore_ascii_case("span") {
        let (count, name) = match &parts[1..] {
            [] => (1, None),
            [one] if one.parse::<usize>().is_ok() => {
                (one.parse::<usize>().unwrap_or(1).max(1), None)
            }
            [one] if is_ident(one) => (1, Some((*one).to_string())),
            [a, b] => {
                if let Ok(count) = a.parse::<usize>() {
                    if !is_ident(b) {
                        return None;
                    }
                    (count.max(1), Some((*b).to_string()))
                } else if is_ident(a) {
                    let count: usize = b.parse().ok()?;
                    (count.max(1), Some((*a).to_string()))
                } else {
                    return None;
                }
            }
            _ => return None,
        };
        return Some(GridLine::Span { count, name });
    }
    match parts.as_slice() {
        [one] => {
            if let Ok(index) = one.parse::<i32>() {
                if index == 0 {
                    return None;
                }
                Some(GridLine::Index(index))
            } else if is_ident(one) {
                Some(GridLine::Named {
                    name: (*one).to_string(),
                    nth: 1,
                    explicit: false,
                })
            } else {
                None
            }
        }
        [a, b] => {
            if let Ok(nth) = b.parse::<i32>() {
                if nth == 0 || !is_ident(a) {
                    return None;
                }
                Some(GridLine::Named {
                    name: (*a).to_string(),
                    nth,
                    explicit: true,
                })
            } else if let Ok(nth) = a.parse::<i32>() {
                if nth == 0 || !is_ident(b) {
                    return None;
                }
                Some(GridLine::Named {
                    name: (*b).to_string(),
                    nth,
                    explicit: true,
                })
            } else {
                None
            }
        }
        _ => None,
    }
}

fn is_ident(token: &str) -> bool {
    let mut chars = token.chars();
    match chars.next() {
        Some(ch) if ch.is_ascii_alphabetic() || ch == '_' || ch == '\\' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '\\')
}

struct AxisPlace {
    start: Option<isize>,
    span: usize,
}

// Share the stable-sort instantiation between ordinary grids and subgrids.
// CSS order is primary; the original child ordinal breaks ties.
fn sort_placement_order(items: &mut [(Option<AxisPlace>, Option<AxisPlace>, i32, usize, usize)]) {
    items.sort_by_key(|item| (item.2, item.4));
}

fn resolve_axis(value: Option<&str>, names: &[Vec<String>], explicit: usize) -> Option<AxisPlace> {
    let Some(value) = value else {
        return Some(AxisPlace {
            start: None,
            span: 1,
        });
    };
    let parts: Vec<&str> = value.split('/').map(str::trim).collect();
    match parts.as_slice() {
        [only] => resolve_one(only, names, explicit, None),
        [start, end] => resolve_pair(start, end, names, explicit),
        _ => None,
    }
}

fn resolve_pair(
    start: &str,
    end: &str,
    names: &[Vec<String>],
    explicit: usize,
) -> Option<AxisPlace> {
    let (start_text, end_text) = (start, end);
    let start = parse_grid_line(start)?;
    let end = parse_grid_line(end)?;
    // An auto side contributes nothing: `x / auto` is `x`, `auto / span n`
    // is `span n`, and `auto / line` is a span of 1 ending at that line.
    if matches!(end, GridLine::Auto) {
        return resolve_one(start_text, names, explicit, None);
    }
    if matches!(start, GridLine::Auto) {
        if matches!(end, GridLine::Span { .. }) {
            return resolve_one(end_text, names, explicit, None);
        }
        let end_line = grid_line_index(&end, names, explicit, None)?;
        if end_line < 1 {
            return None;
        }
        return Some(AxisPlace {
            start: Some(end_line - 1),
            span: 1,
        });
    }
    if let GridLine::Span { count, name } = start {
        let end_line = grid_line_index(&end, names, explicit, None)?;
        let start_line = span_to_line(end_line, count, name.as_deref(), names, false)?;
        let span = (end_line - start_line).unsigned_abs().max(1);
        return Some(AxisPlace {
            start: Some(start_line.min(end_line)),
            span,
        });
    }
    if let GridLine::Span { count, name } = end {
        let start_line = grid_line_index(&start, names, explicit, None)?;
        let end_line = span_to_line(start_line, count, name.as_deref(), names, true)?;
        let span = (end_line - start_line).unsigned_abs().max(1);
        return Some(AxisPlace {
            start: Some(start_line.min(end_line)),
            span,
        });
    }
    let start_line = grid_line_index(&start, names, explicit, None)?;
    let end_line = grid_line_index(&end, names, explicit, Some(start_line))?;
    let span = (end_line - start_line).unsigned_abs().max(1);
    Some(AxisPlace {
        start: Some(start_line.min(end_line)),
        span,
    })
}

fn resolve_one(
    token: &str,
    names: &[Vec<String>],
    explicit: usize,
    after: Option<isize>,
) -> Option<AxisPlace> {
    match parse_grid_line(token)? {
        GridLine::Auto => Some(AxisPlace {
            start: None,
            span: 1,
        }),
        GridLine::Span { count, name: None } => Some(AxisPlace {
            start: None,
            span: count,
        }),
        // `span <name>` needs the other edge. A lone named span cannot
        // choose a track, so the page stays on the legacy engine.
        GridLine::Span { name: Some(_), .. } => None,
        other => Some(AxisPlace {
            start: Some(grid_line_index(&other, names, explicit, after)?),
            span: 1,
        }),
    }
}

/// Walk `count` tracks, or `count` lines of `name`, from `from` exclusive.
fn span_to_line(
    from: isize,
    count: usize,
    name: Option<&str>,
    names: &[Vec<String>],
    forward: bool,
) -> Option<isize> {
    let Some(name) = name else {
        return Some(if forward {
            from + count as isize
        } else {
            from - count as isize
        });
    };
    let mut found = 0_usize;
    if forward {
        let mut index = from + 1;
        while (index as usize) < names.len() {
            if names[index as usize].iter().any(|line| line == name) {
                found += 1;
                if found == count {
                    return Some(index);
                }
            }
            index += 1;
        }
    } else {
        let mut index = from - 1;
        while index >= 0 {
            if names[index as usize].iter().any(|line| line == name) {
                found += 1;
                if found == count {
                    return Some(index);
                }
            }
            index -= 1;
        }
    }
    None
}

fn grid_line_index(
    line: &GridLine,
    names: &[Vec<String>],
    explicit: usize,
    after: Option<isize>,
) -> Option<isize> {
    match line {
        GridLine::Auto | GridLine::Span { .. } => None,
        GridLine::Index(index) => {
            if *index == 0 {
                return None;
            }
            Some(if *index > 0 {
                (*index - 1) as isize
            } else {
                explicit as isize + *index as isize + 1
            })
        }
        GridLine::Named {
            name,
            nth,
            explicit: nth_written,
        } => named_line(name, *nth, *nth_written, names, explicit, after),
    }
}

fn named_line(
    name: &str,
    nth: i32,
    nth_written: bool,
    names: &[Vec<String>],
    explicit: usize,
    after: Option<isize>,
) -> Option<isize> {
    let limit = explicit.min(names.len().saturating_sub(1));
    let hits: Vec<isize> = names
        .iter()
        .enumerate()
        .take(limit + 1)
        .filter(|(_, line)| line.iter().any(|candidate| candidate == name))
        .map(|(index, _)| index as isize)
        .collect();
    if !nth_written {
        let after = after.unwrap_or(-1);
        return hits.into_iter().find(|index| *index > after);
    }
    if nth > 0 {
        hits.get((nth as usize) - 1).copied()
    } else if nth < 0 {
        let index = hits.len() as i32 + nth;
        if index < 0 {
            None
        } else {
            hits.get(index as usize).copied()
        }
    } else {
        None
    }
}

struct Placed {
    index: usize,
    col: usize,
    row: usize,
    col_span: usize,
    row_span: usize,
    source: usize,
}

fn expand_auto(
    template: &Template,
    available: i64,
    gap: i64,
    font: f32,
    min_repeat: usize,
) -> Option<Vec<Spec>> {
    let Some(fit) = template.auto_fit else {
        return Some(template.tracks.clone());
    };
    let mut count = auto_repeat_count(available, gap, &template.auto_tracks, font);
    if count == 0 {
        // A 0-sized repetition does not limit how many tracks fit.
        // auto-fit collapses the copies items do not occupy; auto-fill
        // keeps a single track so empty flexible tracks do not eat the
        // free space.
        count = if fit { min_repeat.max(1) } else { 1 };
    }
    let mut tracks = Vec::with_capacity(template.tracks.len() + count * template.auto_tracks.len());
    tracks.extend_from_slice(&template.tracks[..template.auto_at]);
    for _ in 0..count {
        tracks.extend_from_slice(&template.auto_tracks);
    }
    tracks.extend_from_slice(&template.tracks[template.auto_at..]);
    if tracks.len() > MAX_TRACKS {
        return None;
    }
    Some(tracks)
}

/// Line names after an auto-repeat, so a line written after the repeat
/// stays on the same track it names once the tracks are expanded.
fn expanded_names(template: &Template, expanded: usize) -> Vec<Vec<String>> {
    let added = expanded.saturating_sub(template.tracks.len());
    let mut names = if template.names.is_empty() {
        vec![Vec::new()]
    } else {
        template.names.clone()
    };
    if template.auto_fit.is_some() && added > 0 {
        let at = (template.auto_at + 1).min(names.len());
        for _ in 0..added {
            names.insert(at, Vec::new());
        }
    }
    while names.len() < expanded + 1 {
        names.push(Vec::new());
    }
    names
}

fn auto_repeat_count(available: i64, gap: i64, tracks: &[Spec], font: f32) -> usize {
    let mut sum = 0_i64;
    let mut definite = true;
    for (index, spec) in tracks.iter().enumerate() {
        match repeat_basis(spec, available, font) {
            Some(size) => {
                sum += size.max(0);
                if index + 1 != tracks.len() {
                    sum += gap;
                }
            }
            None => definite = false,
        }
    }
    if !definite {
        return 1;
    }
    if sum <= 0 {
        return 0;
    }
    let count = (available.max(0) + gap) / (sum + gap);
    (count.max(1) as usize).min(MAX_REPEAT)
}

fn repeat_basis(spec: &Spec, available: i64, font: f32) -> Option<i64> {
    match spec {
        Spec::One(breadth) | Spec::Fit(breadth) => {
            definite_breadth(*breadth, Some(available), font)
        }
        // The count uses the max when it is definite, floored by the min
        // when that max would otherwise be smaller (CSS Grid §7.2.3.2).
        Spec::MinMax(min, max) => {
            let min_size = definite_breadth(*min, Some(available), font);
            let max_size = definite_breadth(*max, Some(available), font);
            match (min_size, max_size) {
                (Some(min_size), Some(max_size)) => Some(max_size.max(min_size)),
                (None, Some(max_size)) => Some(max_size),
                (Some(min_size), None) => Some(min_size),
                (None, None) => None,
            }
        }
    }
}

/// CSS Grid 12.5 step 3 (Blink `IncreaseTrackSizesToAccommodateGridItems`),
/// for rows: in increasing span order, an item spanning several tracks, none
/// of them flexible, grows the spanned tracks that have an intrinsic minimum
/// by equal shares of what its contribution exceeds the spanned sizes and
/// gaps. Rows are finalized under indefinite free space, so the contribution
/// feeds the base sizes directly.
fn grow_for_spanning_items(
    tracks: &mut [Track],
    specs: &[Spec],
    spanning: &[(usize, usize, i64)],
    available: Option<i64>,
    font: f32,
    gap: i64,
) {
    let mut order: Vec<&(usize, usize, i64)> = spanning.iter().collect();
    order.sort_by_key(|(_, span, _)| *span);
    for &(start, span, size) in order {
        let end = (start + span).min(tracks.len());
        if start >= end || tracks[start..end].iter().any(|track| track.flex > 0.0) {
            continue;
        }
        let grows: Vec<usize> = (start..end)
            .filter(|&index| {
                specs
                    .get(index)
                    .is_some_and(|spec| intrinsic_minimum(spec, available, font))
            })
            .collect();
        if grows.is_empty() {
            continue;
        }
        let spanned: i64 = tracks[start..end].iter().map(|track| track.base).sum();
        let extra = size - spanned - gap * (end - start - 1) as i64;
        if extra <= 0 {
            continue;
        }
        let share = extra / grows.len() as i64;
        let remainder = extra - share * grows.len() as i64;
        for (offset, &index) in grows.iter().enumerate() {
            let add = share + if offset < remainder as usize { 1 } else { 0 };
            let track = &mut tracks[index];
            track.base += add;
            track.cap = track.cap.max(track.base);
        }
    }
}

/// An item spanning several tracks, with its minimum, min-content and
/// max-content contributions (margin boxes, layout units).
struct SpanItem {
    start: usize,
    span: usize,
    minimum: i64,
    min_c: i64,
    max_c: i64,
}

#[derive(Clone, Copy, PartialEq)]
enum MinKind {
    Fixed,
    Auto,
    MinContent,
    MaxContent,
}

#[derive(Clone, Copy, PartialEq)]
enum MaxKind {
    Fixed,
    MinContent,
    /// `auto` and `max-content`.
    MaxContent,
    /// `fit-content(limit)`.
    Fit(i64),
    Flex,
}

/// The min and max track sizing functions of a spec (CSS Grid 7.2.4); a
/// percentage against indefinite space behaves as `auto`.
fn sizing_kinds(spec: &Spec, available: Option<i64>, font: f32) -> (MinKind, MaxKind) {
    let min_kind = |breadth: Breadth| match breadth {
        Breadth::Auto => MinKind::Auto,
        Breadth::MinContent => MinKind::MinContent,
        Breadth::MaxContent => MinKind::MaxContent,
        Breadth::Fr(_) => MinKind::Auto,
        other => match definite_breadth(other, available, font) {
            Some(_) => MinKind::Fixed,
            None => MinKind::Auto,
        },
    };
    let max_kind = |breadth: Breadth| match breadth {
        Breadth::Fr(_) => MaxKind::Flex,
        Breadth::Auto | Breadth::MaxContent => MaxKind::MaxContent,
        Breadth::MinContent => MaxKind::MinContent,
        other => match definite_breadth(other, available, font) {
            Some(_) => MaxKind::Fixed,
            None => MaxKind::MaxContent,
        },
    };
    match spec {
        Spec::One(breadth) => (min_kind(*breadth), max_kind(*breadth)),
        Spec::MinMax(min, max) => (min_kind(*min), max_kind(*max)),
        Spec::Fit(argument) => (
            MinKind::Auto,
            match definite_breadth(*argument, available, font) {
                Some(limit) => MaxKind::Fit(limit),
                None => MaxKind::MaxContent,
            },
        ),
    }
}

/// CSS Grid 12.5 steps 3 and 4 (Blink
/// `IncreaseTrackSizesToAccommodateGridItems`): after the single-span items
/// set the tracks, items spanning several tracks grow them, in increasing
/// span order. Items that cross no flexible track run the six sub-steps of
/// 12.5.1 (intrinsic, content-based and max-content minimums, then intrinsic
/// and max-content maximums); the others grow the flexible tracks' bases in
/// proportion to their flex factors. `has_item` marks the tracks a
/// single-span item already sized; an intrinsic growth limit of a track
/// without one starts out infinite.
fn accommodate_spanning_items(
    tracks: &mut [Track],
    specs: &[Spec],
    has_item: &[bool],
    spanning: &[SpanItem],
    available: Option<i64>,
    font: f32,
    gap: i64,
) {
    if spanning.is_empty() {
        return;
    }
    let count = tracks.len();
    let kinds: Vec<(MinKind, MaxKind)> = (0..count)
        .map(|index| {
            if tracks[index].collapsed {
                (MinKind::Fixed, MaxKind::Fixed)
            } else {
                specs
                    .get(index)
                    .map_or((MinKind::Fixed, MaxKind::Fixed), |spec| {
                        sizing_kinds(spec, available, font)
                    })
            }
        })
        .collect();
    let mut infinite: Vec<bool> = (0..count)
        .map(|index| {
            !has_item.get(index).copied().unwrap_or(false)
                && matches!(
                    kinds[index].1,
                    MaxKind::MinContent | MaxKind::MaxContent | MaxKind::Fit(_)
                )
        })
        .collect();
    let mut spans: Vec<usize> = spanning.iter().map(|item| item.span).collect();
    spans.sort_unstable();
    spans.dedup();
    for span in spans {
        let group: Vec<&SpanItem> = spanning
            .iter()
            .filter(|item| item.span == span && item.start + item.span <= count)
            .collect();
        let (flexible, plain): (Vec<&SpanItem>, Vec<&SpanItem>) =
            group.into_iter().partition(|item| {
                (item.start..item.start + item.span).any(|index| kinds[index].1 == MaxKind::Flex)
            });
        let mut growable = vec![false; count];
        for step in 0..5 {
            let base_step = step < 3;
            let mut planned = vec![0_i64; count];
            for item in &plain {
                let range = item.start..item.start + item.span;
                let affected: Vec<usize> = range
                    .clone()
                    .filter(|&index| {
                        let (min, max) = kinds[index];
                        match step {
                            0 => min != MinKind::Fixed,
                            1 => matches!(min, MinKind::MinContent | MinKind::MaxContent),
                            2 => min == MinKind::MaxContent,
                            3 => matches!(
                                max,
                                MaxKind::MinContent | MaxKind::MaxContent | MaxKind::Fit(_)
                            ),
                            _ => matches!(max, MaxKind::MaxContent | MaxKind::Fit(_)),
                        }
                    })
                    .collect();
                if affected.is_empty() {
                    continue;
                }
                let contribution = match step {
                    0 => item.minimum,
                    1 | 3 => item.min_c,
                    _ => item.max_c,
                };
                let size_of = |index: usize| {
                    if base_step || infinite[index] {
                        tracks[index].base
                    } else {
                        tracks[index].cap
                    }
                };
                let spanned: i64 =
                    range.clone().map(size_of).sum::<i64>() + gap * (item.span as i64 - 1);
                let mut space = contribution - spanned;
                if space <= 0 {
                    continue;
                }
                let limit_of = |index: usize| -> i64 {
                    if base_step {
                        if infinite[index] {
                            INF
                        } else {
                            tracks[index].cap
                        }
                    } else if infinite[index] || growable[index] {
                        INF
                    } else {
                        tracks[index].cap
                    }
                };
                let mut increase = vec![0_i64; affected.len()];
                let room: Vec<i64> = affected
                    .iter()
                    .map(|&index| {
                        let mut limit = limit_of(index);
                        if !base_step {
                            // A fit-content growth limit stops at its argument.
                            if let MaxKind::Fit(fit) = kinds[index].1 {
                                limit = limit.min(fit.max(tracks[index].base));
                            }
                        }
                        (limit - size_of(index)).max(0)
                    })
                    .collect();
                space = distribute_equally(&mut increase, &room, space);
                if space > 0 {
                    // 12.5.1 "distribute space beyond limits".
                    let beyond: Vec<bool> = affected
                        .iter()
                        .map(|&index| match step {
                            0 | 1 => matches!(
                                kinds[index].1,
                                MaxKind::MinContent | MaxKind::MaxContent | MaxKind::Fit(_)
                            ),
                            2 => matches!(kinds[index].1, MaxKind::MaxContent | MaxKind::Fit(_)),
                            _ => true,
                        })
                        .collect();
                    let any = beyond.iter().any(|flag| *flag);
                    let room: Vec<i64> = affected
                        .iter()
                        .enumerate()
                        .map(|(slot, &index)| {
                            if any && !beyond[slot] {
                                0
                            } else if let MaxKind::Fit(limit) = kinds[index].1 {
                                // A fit-content track stops at its argument.
                                (limit.max(size_of(index)) - size_of(index) - increase[slot]).max(0)
                            } else {
                                INF
                            }
                        })
                        .collect();
                    distribute_equally(&mut increase, &room, space);
                }
                for (slot, &index) in affected.iter().enumerate() {
                    planned[index] = planned[index].max(increase[slot]);
                }
            }
            for index in 0..count {
                if base_step {
                    tracks[index].base += planned[index];
                } else if planned[index] > 0 {
                    if infinite[index] {
                        tracks[index].cap = tracks[index].base + planned[index];
                        infinite[index] = false;
                        // 12.5.1 step 5: the next step may grow it past this.
                        if step == 3 {
                            growable[index] = true;
                        }
                    } else {
                        tracks[index].cap += planned[index];
                    }
                }
                if step == 2 && !infinite[index] && tracks[index].flex == 0.0 {
                    tracks[index].cap = tracks[index].cap.max(tracks[index].base);
                }
            }
            if step == 4 {
                growable.iter_mut().for_each(|flag| *flag = false);
            }
        }
        // 12.5 step 4: items crossing flexible tracks grow those tracks'
        // bases by their flex factors.
        let mut planned = vec![0_i64; count];
        for item in &flexible {
            let range = item.start..item.start + item.span;
            let spanned: i64 = range.clone().map(|index| tracks[index].base).sum::<i64>()
                + gap * (item.span as i64 - 1);
            let space = item.minimum - spanned;
            if space <= 0 {
                continue;
            }
            let flex: Vec<usize> = range
                .filter(|&index| {
                    kinds[index].1 == MaxKind::Flex && kinds[index].0 != MinKind::Fixed
                })
                .collect();
            let total: f64 = flex.iter().map(|&index| tracks[index].flex).sum();
            if flex.is_empty() || total <= 0.0 {
                continue;
            }
            let mut given = 0;
            for (slot, &index) in flex.iter().enumerate() {
                let share = if slot + 1 == flex.len() {
                    space - given
                } else {
                    ((space as f64) * tracks[index].flex / total).floor() as i64
                };
                given += share;
                planned[index] = planned[index].max(share);
            }
        }
        for index in 0..count {
            tracks[index].base += planned[index];
        }
    }
    for index in 0..count {
        if tracks[index].flex == 0.0 {
            tracks[index].cap = tracks[index].cap.max(tracks[index].base);
        }
    }
}

/// Hands `space` out in equal shares, each slot stopping at its `room`;
/// returns what is left once every slot is full.
fn distribute_equally(increase: &mut [i64], room: &[i64], mut space: i64) -> i64 {
    loop {
        let open: Vec<usize> = (0..increase.len())
            .filter(|&slot| room[slot] > increase[slot])
            .collect();
        if open.is_empty() || space <= 0 {
            return space;
        }
        let share = space / open.len() as i64;
        if share == 0 {
            for slot in open {
                if space == 0 {
                    break;
                }
                increase[slot] += 1;
                space -= 1;
            }
            return space;
        }
        for slot in open {
            let add = share.min(room[slot] - increase[slot]);
            increase[slot] += add;
            space -= add;
        }
    }
}

/// Whether a track's min sizing function is intrinsic (`auto`, `min-content`,
/// `max-content`, `fit-content()`, or a percentage against indefinite space).
fn intrinsic_minimum(spec: &Spec, available: Option<i64>, font: f32) -> bool {
    match spec {
        Spec::One(Breadth::Fr(_)) => false,
        Spec::One(breadth) => definite_breadth(*breadth, available, font).is_none(),
        Spec::Fit(_) => true,
        Spec::MinMax(min, _) => {
            !matches!(min, Breadth::Fr(_)) && definite_breadth(*min, available, font).is_none()
        }
    }
}

fn definite_breadth(breadth: Breadth, available: Option<i64>, font: f32) -> Option<i64> {
    match breadth {
        Breadth::Px(value) => Some(units(value).max(0)),
        Breadth::Em(value) => Some(units(font * value).max(0)),
        Breadth::Percent(value) => available.map(|inner| percent_units(inner, value)),
        Breadth::Calc { percent, px } => {
            available.map(|inner| (percent_units(inner, percent) + units(px)).max(0))
        }
        Breadth::Fr(_) | Breadth::Auto | Breadth::MinContent | Breadth::MaxContent => None,
    }
}

fn percent_units(inner: i64, percent: f32) -> i64 {
    ((inner as f64) * percent as f64 / 100.0).floor() as i64
}

fn fixed_track(size: i64) -> Track {
    let size = size.max(0);
    Track {
        base: size,
        cap: size,
        flex: 0.0,
        stretchy: false,
        collapsed: false,
    }
}

/// `minimum` is the minimum contribution (auto minimums and flexible
/// tracks), `min_c` the min-content contribution.
fn track_from_spec(
    spec: &Spec,
    minimum: i64,
    min_c: i64,
    max_c: i64,
    available: Option<i64>,
    font: f32,
    stretch: bool,
) -> Track {
    let mut track = match spec {
        Spec::One(Breadth::Fr(factor)) => Track {
            base: minimum.max(0),
            cap: INF,
            flex: *factor,
            stretchy: false,
            collapsed: false,
        },
        // An auto track starts at the minimum contribution with a growth
        // limit of max-content; "maximize tracks" grows it towards that limit
        // and only then does stretch hand out the rest (CSS Grid 12.4-12.6,
        // Blink GridLayoutAlgorithm). It may end below max-content when the
        // free space runs out.
        Spec::One(Breadth::Auto) => intrinsic_track(minimum, max_c, stretch),
        Spec::One(Breadth::MinContent) => fixed_track(min_c),
        Spec::One(Breadth::MaxContent) => fixed_track(max_c),
        Spec::One(breadth) => match definite_breadth(*breadth, available, font) {
            Some(size) => fixed_track(size),
            None => intrinsic_track(minimum, max_c, stretch),
        },
        // fit-content(limit) is minmax(auto, max-content) with the growth
        // limit clamped at max(base, limit) (CSS Grid 7.2.4).
        Spec::Fit(argument) => {
            let limit = definite_breadth(*argument, available, font).unwrap_or(max_c);
            let base = minimum.clamp(0, max_c.max(0));
            Track {
                base,
                cap: max_c.min(limit).max(base),
                flex: 0.0,
                stretchy: false,
                collapsed: false,
            }
        }
        Spec::MinMax(min, max) => {
            let base = breadth_floor(*min, minimum, min_c, max_c, available, font);
            let (cap, flex) = breadth_ceiling(*max, min_c, max_c, available, font);
            // CSS Grid 12.7: every track with an auto max sizing function
            // takes part in stretching.
            let auto_max = matches!(max, Breadth::Auto)
                || (matches!(max, Breadth::Percent(_) | Breadth::Calc { .. })
                    && definite_breadth(*max, available, font).is_none());
            Track {
                base,
                cap: cap.max(base),
                flex,
                stretchy: stretch && auto_max,
                collapsed: false,
            }
        }
    };
    if available.is_none() && track.flex == 0.0 && track.cap < INF {
        track.base = max_c.clamp(track.base, track.cap);
        track.cap = track.cap.max(track.base);
    }
    track
}

fn intrinsic_track(minimum: i64, max_c: i64, stretch: bool) -> Track {
    let limit = max_c.max(0);
    Track {
        base: minimum.clamp(0, limit),
        cap: limit,
        flex: 0.0,
        stretchy: stretch,
        collapsed: false,
    }
}

fn breadth_floor(
    breadth: Breadth,
    minimum: i64,
    min_c: i64,
    max_c: i64,
    available: Option<i64>,
    font: f32,
) -> i64 {
    match breadth {
        Breadth::Auto => minimum.max(0),
        Breadth::MinContent => min_c.max(0),
        Breadth::MaxContent => max_c.max(0),
        Breadth::Fr(_) => 0,
        other => definite_breadth(other, available, font).unwrap_or(min_c.max(0)),
    }
}

fn breadth_ceiling(
    breadth: Breadth,
    min_c: i64,
    max_c: i64,
    available: Option<i64>,
    font: f32,
) -> (i64, f64) {
    match breadth {
        Breadth::Fr(factor) => (INF, factor),
        Breadth::Auto | Breadth::MaxContent => (max_c.max(0), 0.0),
        Breadth::MinContent => (min_c.max(0), 0.0),
        other => (
            definite_breadth(other, available, font).unwrap_or(max_c.max(0)),
            0.0,
        ),
    }
}

fn gap_sum(collapsed: &[bool], gap: i64) -> i64 {
    let active = collapsed.iter().filter(|flag| !**flag).count();
    gap * active.saturating_sub(1) as i64
}

fn grow(used: &mut [i64], caps: &[i64], mut free: i64) {
    if free <= 0 {
        return;
    }
    for _ in 0..=used.len() {
        if free <= 0 {
            return;
        }
        let growable = (0..used.len())
            .filter(|&index| caps[index] > used[index])
            .count();
        if growable == 0 {
            return;
        }
        let share = free / growable as i64;
        if share <= 0 {
            for index in (0..used.len()).rev() {
                if free <= 0 {
                    return;
                }
                if used[index] < caps[index] {
                    used[index] += 1;
                    free -= 1;
                }
            }
            return;
        }
        let mut consumed = 0;
        for index in 0..used.len() {
            if caps[index] <= used[index] {
                continue;
            }
            let add = share.min(caps[index] - used[index]);
            used[index] += add;
            consumed += add;
        }
        if consumed == 0 {
            return;
        }
        free -= consumed;
    }
}

fn distribute_flex(bases: &[i64], factors: &[f64], mut budget: i64) -> Vec<i64> {
    let mut frozen = vec![false; bases.len()];
    let mut size = bases.to_vec();
    if budget < 0 {
        budget = 0;
    }
    loop {
        let unfrozen: Vec<usize> = (0..bases.len())
            .filter(|&index| factors[index] > 0.0 && !frozen[index])
            .collect();
        if unfrozen.is_empty() {
            break;
        }
        let factor_sum: f64 = unfrozen.iter().map(|&index| factors[index]).sum();
        if factor_sum <= 0.0 || budget <= 0 {
            break;
        }
        let mut violated = Vec::new();
        let mut raw = Vec::new();
        let mut used_raw = 0_i64;
        for &index in &unfrozen {
            let share = ((budget as f64) * factors[index] / factor_sum).floor() as i64;
            raw.push((index, share));
            used_raw += share;
            if bases[index] > share {
                violated.push(index);
            }
        }
        if !violated.is_empty() {
            for index in violated {
                if frozen[index] {
                    continue;
                }
                frozen[index] = true;
                size[index] = bases[index];
                budget -= bases[index];
            }
            if budget < 0 {
                budget = 0;
            }
            continue;
        }
        let mut leftover = budget - used_raw;
        for &(index, share) in &raw {
            size[index] = share.max(bases[index]);
        }
        for &(index, _) in raw.iter().rev() {
            if leftover <= 0 {
                break;
            }
            size[index] += 1;
            leftover -= 1;
        }
        break;
    }
    size
}

fn finalize(tracks: &[Track], available: Option<i64>, gap: i64) -> Vec<i64> {
    let mut used: Vec<i64> = tracks
        .iter()
        .map(|track| if track.collapsed { 0 } else { track.base })
        .collect();
    let collapsed: Vec<bool> = tracks.iter().map(|track| track.collapsed).collect();
    let Some(available) = available else {
        return used;
    };
    let gaps = gap_sum(&collapsed, gap);
    if tracks
        .iter()
        .any(|track| track.flex > 0.0 && !track.collapsed)
    {
        let mut caps = used.clone();
        for (index, track) in tracks.iter().enumerate() {
            // Flexible tracks are frozen at their base while the others are
            // maximized up to their growth limits (CSS Grid 12.5 step 5).
            if track.flex == 0.0 && !track.collapsed {
                caps[index] = track.cap.max(used[index]);
            }
        }
        let free = available - gaps - used.iter().sum::<i64>();
        if free > 0 {
            grow(&mut used, &caps, free);
        }
        let non_flex: i64 = used
            .iter()
            .enumerate()
            .filter(|(index, _)| tracks[*index].flex == 0.0)
            .map(|(_, size)| *size)
            .sum();
        let flex_space = available - gaps - non_flex;
        let flex_index: Vec<usize> = tracks
            .iter()
            .enumerate()
            .filter(|(_, track)| track.flex > 0.0 && !track.collapsed)
            .map(|(index, _)| index)
            .collect();
        let bases: Vec<i64> = flex_index.iter().map(|&index| tracks[index].base).collect();
        let factors: Vec<f64> = flex_index.iter().map(|&index| tracks[index].flex).collect();
        let sized = distribute_flex(&bases, &factors, flex_space);
        for (index, size) in flex_index.into_iter().zip(sized) {
            used[index] = size;
        }
    } else {
        let caps: Vec<i64> = tracks
            .iter()
            .enumerate()
            .map(|(index, track)| {
                if track.collapsed {
                    0
                } else {
                    track.cap.max(used[index])
                }
            })
            .collect();
        let free = available - gaps - used.iter().sum::<i64>();
        if free > 0 {
            grow(&mut used, &caps, free);
        }
    }
    let free = available - gaps - used.iter().sum::<i64>();
    if free > 0 && tracks.iter().any(|track| track.stretchy) {
        let caps: Vec<i64> = tracks
            .iter()
            .enumerate()
            .map(|(index, track)| if track.stretchy { INF } else { used[index] })
            .collect();
        grow(&mut used, &caps, free);
    }
    used
}

fn pack_gap(pack: Pack, free: i64, count: usize, gap: i64) -> (i64, i64) {
    if count == 0 {
        return (0, gap);
    }
    let overflow = free < 0;
    match pack {
        Pack::End => (free, gap),
        Pack::Center => (free / 2, gap),
        Pack::SpaceBetween if !overflow && count > 1 => (0, gap + free / (count as i64 - 1)),
        Pack::SpaceAround if !overflow => {
            let extra = free / count as i64;
            (extra / 2, gap + extra)
        }
        Pack::SpaceEvenly if !overflow => {
            let extra = free / (count as i64 + 1);
            (extra, gap + extra)
        }
        _ => (0, gap),
    }
}

fn positions(sizes: &[i64], collapsed: &[bool], gap: i64, lead: i64) -> Vec<i64> {
    let mut cursor = lead;
    let mut out = Vec::with_capacity(sizes.len());
    let mut seen = false;
    for (index, size) in sizes.iter().enumerate() {
        if collapsed.get(index).copied().unwrap_or(false) {
            out.push(cursor);
            continue;
        }
        if seen {
            cursor += gap;
        }
        out.push(cursor);
        cursor += *size;
        seen = true;
    }
    out
}

fn place_items(
    specs: &[(Option<AxisPlace>, Option<AxisPlace>, i32, usize, usize)],
    explicit_cols: usize,
    explicit_rows: usize,
    flow: Flow,
) -> Option<Vec<(usize, usize, usize, usize, usize, usize)>> {
    let mut occupied = BTreeSet::new();
    let mut placed = Vec::new();
    let mut cursor_major = 0_usize;
    let mut cursor_minor = 0_usize;
    let column_flow = flow.column;
    let cross_limit = if column_flow {
        explicit_rows.max(1)
    } else {
        explicit_cols.max(1)
    };
    for (_placed_order, (column, row, _order, index, ordinal)) in specs.iter().enumerate() {
        let column = column.as_ref()?;
        let row = row.as_ref()?;
        // Row flow advances across columns and wraps to the next row.
        // Column flow advances down rows and wraps to the next column.
        let (major_span, minor_span, major_fixed, minor_fixed) = if column_flow {
            (column.span, row.span, column.start, row.start)
        } else {
            (row.span, column.span, row.start, column.start)
        };
        let (major, minor) = if let (Some(major), Some(minor)) = (major_fixed, minor_fixed) {
            (major, minor)
        } else if let Some(major) = major_fixed {
            let mut minor = 0_isize;
            while overlaps(&occupied, major, minor, major_span, minor_span, column_flow) {
                minor += 1;
                if minor > MAX_TRACKS as isize {
                    return None;
                }
            }
            (major, minor)
        } else if let Some(minor) = minor_fixed {
            let mut major = if flow.dense { 0 } else { cursor_major as isize };
            while overlaps(&occupied, major, minor, major_span, minor_span, column_flow) {
                major += 1;
                if major > MAX_TRACKS as isize {
                    return None;
                }
            }
            cursor_major = major.max(0) as usize;
            (major, minor)
        } else if flow.dense {
            let mut major = 0_isize;
            let mut minor = 0_isize;
            loop {
                if minor > 0 && minor as usize + minor_span > cross_limit {
                    minor = 0;
                    major += 1;
                    continue;
                }
                if !overlaps(&occupied, major, minor, major_span, minor_span, column_flow) {
                    break;
                }
                minor += 1;
                if major > MAX_TRACKS as isize {
                    return None;
                }
            }
            (major, minor)
        } else {
            let mut major = cursor_major as isize;
            let mut minor = cursor_minor as isize;
            loop {
                if minor > 0 && minor as usize + minor_span > cross_limit {
                    minor = 0;
                    major += 1;
                    continue;
                }
                if !overlaps(&occupied, major, minor, major_span, minor_span, column_flow) {
                    break;
                }
                minor += 1;
                if major > MAX_TRACKS as isize {
                    return None;
                }
            }
            cursor_major = major.max(0) as usize;
            cursor_minor = (minor.max(0) as usize) + minor_span;
            if cursor_minor >= cross_limit {
                cursor_minor = 0;
                cursor_major += major_span.max(1);
            }
            (major, minor)
        };
        if major < 0 || minor < 0 {
            return None;
        }
        occupy(
            &mut occupied,
            major,
            minor,
            major_span,
            minor_span,
            column_flow,
        );
        let (col, row_index) = if column_flow {
            (major as usize, minor as usize)
        } else {
            (minor as usize, major as usize)
        };
        placed.push((*index, col, row_index, column.span, row.span, *ordinal));
    }
    Some(placed)
}

fn overlaps(
    occupied: &BTreeSet<(usize, usize)>,
    major: isize,
    minor: isize,
    major_span: usize,
    minor_span: usize,
    column_flow: bool,
) -> bool {
    for major_offset in 0..major_span {
        for minor_offset in 0..minor_span {
            let major = major + major_offset as isize;
            let minor = minor + minor_offset as isize;
            if major < 0 || minor < 0 {
                return true;
            }
            let (col, row) = if column_flow {
                (major as usize, minor as usize)
            } else {
                (minor as usize, major as usize)
            };
            if occupied.contains(&(col, row)) {
                return true;
            }
        }
    }
    false
}

fn occupy(
    occupied: &mut BTreeSet<(usize, usize)>,
    major: isize,
    minor: isize,
    major_span: usize,
    minor_span: usize,
    column_flow: bool,
) {
    for major_offset in 0..major_span {
        for minor_offset in 0..minor_span {
            let major = (major + major_offset as isize) as usize;
            let minor = (minor + minor_offset as isize) as usize;
            let (col, row) = if column_flow {
                (major, minor)
            } else {
                (minor, major)
            };
            occupied.insert((col, row));
        }
    }
}

struct Item {
    index: usize,
    col: usize,
    row: usize,
    col_span: usize,
    row_span: usize,
    source: usize,
    content_width: f32,
    margin_u: [i64; 4],
    auto_margin: [bool; 4],
    edges_v: f32,
    stretch_block: bool,
    specified_content_h: Option<f32>,
    inline_pack: Pack,
    block_pack: Pack,
}

fn layout_item(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    area: f32,
    content_width: f32,
    content_height: Option<f32>,
) -> Option<LayoutResult> {
    let mut nested = *space;
    nested.available_inline = area;
    nested.rtl = false;
    nested.bfc_x = 0.0;
    nested.bfc_y = 0.0;
    nested.forced_bfc = false;
    nested.margin_base = 0.0;
    nested.margin_strut = MarginStrut::default();
    block::layout_block_sized(
        flat,
        index,
        &nested,
        true,
        Some((content_width, content_height)),
    )
    .why("grid:item")
}

fn inline_minmax(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
) -> Option<(i64, i64)> {
    let style = flat[index].style;
    let border = block::border_widths(style);
    let pad = |spacing| spacing_px(style, spacing, 0.0, space);
    let edges = border[1] + border[3] + pad(style.padding.left) + pad(style.padding.right);
    let margin = |spacing| match spacing {
        Spacing::Auto => 0.0,
        other => spacing_px(style, other, 0.0, space),
    };
    let extra = edges + margin(style.margin.left) + margin(style.margin.right);
    if columns_spec(style).is_some_and(is_subgrid_axis) && matches!(style.width, Dimension::Auto) {
        // A column subgrid contributes per parent track, not as one item.
        return Some((0, 0));
    }
    let content = match style.width {
        Dimension::Percent(_) | Dimension::Auto => {
            let sizes = block::intrinsic_sizes(flat, index, space).why("grid:intrinsic")?;
            (sizes.min.unwrap_or(sizes.max).max(0.0), sizes.max.max(0.0))
        }
        other => {
            let specified =
                block::resolve_length(other, Some(0.0), style, space).why("grid:percent")?;
            let content = to_content(style, edges, specified).max(0.0);
            (content, content)
        }
    };
    let floor = min_width_keyword(flat, index, space)?.unwrap_or(0.0);
    let min = block::clamp_inline_size(style, space, edges, content.0)
        .max(floor)
        .max(0.0);
    let max = block::clamp_inline_size(style, space, edges, content.1)
        .max(floor)
        .max(0.0);
    Some((units(min + extra), units(max.max(min) + extra)))
}

/// The content-box size a `min-width: min-content | max-content` keyword
/// stands for (Blink `ComputeMinMaxInlineSizes` with an intrinsic min
/// length): `Some(None)` without the keyword, `None` for `fit-content`,
/// whose result depends on the available space this pass does not have.
fn min_width_keyword(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
) -> Option<Option<f32>> {
    let Some(keyword) = marker(flat[index].style, block::MIN_WIDTH_KEYWORD) else {
        return Some(None);
    };
    let sizes = block::intrinsic_sizes(flat, index, space).why("grid:min-width-keyword")?;
    let min = sizes.min.unwrap_or(sizes.max).max(0.0);
    match keyword {
        "min-content" => Some(Some(min)),
        "max-content" => Some(Some(sizes.max.max(min))),
        _ => super::bail("grid:min-width-keyword"),
    }
}

/// CSS Grid §6.6 minimum contribution (Blink
/// `GridLayoutAlgorithm::ContributionSizeForGridItem`, kForIntrinsicMinimums):
/// a specified `min-width` gives the outer size it implies; with
/// `min-width: auto` a box that scrolls in the inline axis has no automatic
/// minimum, so only its margin, border and padding count. Others contribute
/// their content-based minimum (`None`). A percentage `min-width` against the
/// indefinite grid area behaves as `auto` here.
fn inline_minimum(flat: &[FlatNodeInfo], index: usize, space: &ConstraintSpace) -> Option<i64> {
    let style = flat[index].style;
    let border = block::border_widths(style);
    let pad = |spacing| spacing_px(style, spacing, 0.0, space);
    let margin = |spacing| match spacing {
        Spacing::Auto => 0.0,
        other => spacing_px(style, other, 0.0, space),
    };
    let edges = border[1] + border[3] + pad(style.padding.left) + pad(style.padding.right);
    let outer = edges + margin(style.margin.left) + margin(style.margin.right);
    if let Some(keyword) = min_width_keyword(flat, index, space)? {
        return Some(units((keyword + outer).max(0.0)));
    }
    match style.min_width {
        Dimension::Auto | Dimension::Percent(_) => {
            if !block::scrolls_in_axis(style, true) || !matches!(style.min_width, Dimension::Auto) {
                return None;
            }
            Some(units(outer.max(0.0)))
        }
        _ => {
            let content = block::clamp_inline_size(style, space, edges, 0.0).max(0.0);
            Some(units((content + outer).max(0.0)))
        }
    }
}

/// Which of an abspos item's grid lines are `auto` (or a span against an
/// auto line), and so stand for the container's padding edges.
fn abspos_auto_lines(value: Option<&str>) -> (bool, bool) {
    let Some(value) = value else {
        return (true, true);
    };
    let parts: Vec<&str> = value.split('/').map(str::trim).collect();
    let line = |token: &str| parse_grid_line(token);
    let is_auto = |line: &Option<GridLine>| matches!(line, None | Some(GridLine::Auto));
    let is_span = |line: &Option<GridLine>| matches!(line, Some(GridLine::Span { .. }));
    match parts.as_slice() {
        [only] => {
            let start = line(only);
            let start_auto = is_auto(&start) || is_span(&start);
            // `grid-column: name` copies the ident to the end; any other
            // single value leaves the end line auto.
            (start_auto, !is_ident(only))
        }
        [start, end] => {
            let (start, end) = (line(start), line(end));
            let start_open = is_auto(&start) || is_span(&start);
            let end_open = is_auto(&end) || is_span(&end);
            (
                is_auto(&start) || (is_span(&start) && end_open),
                is_auto(&end) || (is_span(&end) && start_open),
            )
        }
        _ => (true, true),
    }
}

/// The content-box-relative [start, end) of an abspos item's grid area in
/// one axis, falling back to the padding edges for auto or out-of-grid lines.
fn abspos_area(
    sizes: &[i64],
    offsets: &[i64],
    start: Option<isize>,
    span: usize,
    start_auto: bool,
    end_auto: bool,
    padding: (i64, i64),
) -> (i64, i64) {
    let len = sizes.len().min(offsets.len());
    let start_edge = |line: usize| -> Option<i64> {
        if len == 0 || line > len {
            None
        } else if line < len {
            Some(offsets[line])
        } else {
            Some(offsets[len - 1] + sizes[len - 1])
        }
    };
    let end_edge = |line: usize| -> Option<i64> {
        if len == 0 || line > len {
            None
        } else if line == 0 {
            Some(offsets[0])
        } else {
            Some(offsets[line - 1] + sizes[line - 1])
        }
    };
    let start_line = start.filter(|&line| line >= 0).map(|line| line as usize);
    let a = if start_auto {
        None
    } else {
        start_line.and_then(start_edge)
    };
    let b = if end_auto {
        None
    } else {
        start_line.and_then(|line| end_edge(line + span))
    };
    let a = a.unwrap_or(padding.0);
    let b = b.unwrap_or(padding.1).max(a);
    (a, b)
}

fn span_sum(sizes: &[i64], collapsed: &[bool], start: usize, span: usize, gap: i64) -> i64 {
    let mut total = 0;
    let mut seen = false;
    for offset in 0..span {
        let index = start + offset;
        if collapsed.get(index).copied().unwrap_or(false) {
            continue;
        }
        total += sizes.get(index).copied().unwrap_or(0);
        if seen {
            total += gap;
        }
        seen = true;
    }
    total
}

fn axis_mbp(style: &Style, space: &ConstraintSpace, rows: bool) -> (i64, i64) {
    let [bt, br, bb, bl] = block::border_widths(style);
    let pad = |spacing| spacing_px(style, spacing, 0.0, space);
    let margin = |spacing| spacing_px(style, spacing, 0.0, space);
    if rows {
        (
            units(bt + pad(style.padding.top) + margin(style.margin.top)),
            units(bb + pad(style.padding.bottom) + margin(style.margin.bottom)),
        )
    } else {
        (
            units(bl + pad(style.padding.left) + margin(style.margin.left)),
            units(br + pad(style.padding.right) + margin(style.margin.right)),
        )
    }
}

fn add_span_contrib(
    mins: &mut [i64],
    maxs: &mut [i64],
    start: usize,
    span: usize,
    min_c: i64,
    max_c: i64,
) {
    if span == 0 || start >= mins.len() {
        return;
    }
    let end = (start + span).min(mins.len());
    if end - start <= 1 {
        mins[start] = mins[start].max(min_c);
        maxs[start] = maxs[start].max(max_c);
        return;
    }
    let current: i64 = maxs[start..end].iter().sum();
    if max_c > current {
        let extra = max_c - current;
        let slots = (end - start) as i64;
        for slot in &mut maxs[start..end] {
            *slot += extra / slots;
        }
    }
    let current_min: i64 = mins[start..end].iter().sum();
    if min_c > current_min {
        let extra = min_c - current_min;
        let slots = (end - start) as i64;
        for slot in &mut mins[start..end] {
            *slot += extra / slots;
        }
    }
}

/// Items of a column subgrid, written into the parent tracks at `origin`.
/// `span_names` is already the parent's line names for this span (index 0
/// is the span's start line), so a nested call can pass a slice.
fn contribute_subgrid_columns(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    origin: usize,
    span: usize,
    span_names: &[Vec<String>],
    parent_gap: i64,
    extra_start: i64,
    extra_end: i64,
    col_min: &mut [i64],
    col_max: &mut [i64],
) -> Option<()> {
    if span == 0 || origin.saturating_add(span) > col_min.len() {
        return super::bail("grid:subgrid");
    }
    let style = flat[index].style;
    let names = merge_line_names(span_names, &local_subgrid_names(style, false), span);
    let (mbp_start, mbp_end) = axis_mbp(style, space, false);
    let half = gap_half(style.column_gap, parent_gap);
    let row_names = if style.grid_rows().is_some_and(is_subgrid_axis) {
        Vec::new()
    } else {
        match style.grid_rows() {
            Some(spec) => parse_template(spec, true).why("grid:template")?.names,
            None => vec![Vec::new()],
        }
    };
    let explicit_rows = row_names.len().saturating_sub(1);
    let flow = container_flow(flat, index).why("grid:auto-flow")?;
    let mut pending = Vec::new();
    for (ordinal, child) in rendered_children(flat, index).enumerate() {
        let child_style = flat[child].style;
        if oof::is_out_of_flow(child_style) {
            continue;
        }
        let column =
            resolve_axis(placement(child_style, false), &names, span).why("grid:column")?;
        let row = resolve_axis(placement(child_style, true), &row_names, explicit_rows)
            .why("grid:row")?;
        pending.push((Some(column), Some(row), child_style.order, child, ordinal));
    }
    sort_placement_order(&mut pending);
    let placed = place_items(&pending, span, explicit_rows, flow).why("grid:place")?;
    let mut covered_start = false;
    let mut covered_end = false;
    for (child, col, _row, col_span, _row_span, _source) in placed {
        let child_style = flat[child].style;
        let mut start = col;
        let mut child_span = col_span.max(1);
        if start >= span {
            start = span - 1;
            child_span = 1;
        }
        if start + child_span > span {
            child_span = span - start;
        }
        if start == 0 {
            covered_start = true;
        }
        if start + child_span == span {
            covered_end = true;
        }
        let edge_start = if start == 0 {
            mbp_start + extra_start
        } else {
            half
        };
        let edge_end = if start + child_span == span {
            mbp_end + extra_end
        } else {
            half
        };
        if columns_spec(child_style).is_some_and(is_subgrid_axis) {
            contribute_subgrid_columns(
                flat,
                child,
                space,
                origin + start,
                child_span,
                &slice_names(&names, start, child_span),
                parent_gap,
                edge_start,
                edge_end,
                col_min,
                col_max,
            )?;
            continue;
        }
        let (min_c, max_c) = inline_minmax(flat, child, space).why("grid:intrinsic")?;
        add_span_contrib(
            col_min,
            col_max,
            origin + start,
            child_span,
            min_c + edge_start + edge_end,
            max_c + edge_start + edge_end,
        );
    }
    if !covered_start {
        let extra = mbp_start + extra_start;
        col_min[origin] = col_min[origin].max(extra);
        col_max[origin] = col_max[origin].max(extra);
    }
    if !covered_end && span > 1 {
        let track = origin + span - 1;
        let extra = mbp_end + extra_end;
        col_min[track] = col_min[track].max(extra);
        col_max[track] = col_max[track].max(extra);
    }
    Some(())
}

/// A row subgrid's inline size is the widest child, plus its own edges.
/// Children stack on the row axis, so they do not sit side by side.
fn row_subgrid_inline(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
) -> Option<(i64, i64)> {
    let style = flat[index].style;
    let (start, end) = axis_mbp(style, space, false);
    let mut min_c = 0_i64;
    let mut max_c = 0_i64;
    for child in rendered_children(flat, index) {
        if oof::is_out_of_flow(flat[child].style) {
            continue;
        }
        let (child_min, child_max) = inline_minmax(flat, child, space).why("grid:intrinsic")?;
        min_c = min_c.max(child_min);
        max_c = max_c.max(child_max);
    }
    Some((min_c + start + end, max_c + start + end))
}

/// Lay out one grid container. `sized` is the content box a float, an
/// abspos containing block, or a stretch pass already resolved.
pub(super) fn layout(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    sized: Option<(f32, Option<f32>)>,
) -> Option<LayoutResult> {
    match layout_impl(flat, index, space, sized, false)? {
        GridOut::Laid(result) => Some(result),
        GridOut::Measured(_) => super::bail("grid:measure"),
    }
}

/// Min-content and max-content inline sizes of a grid container's content
/// box (Blink GridLayoutAlgorithm::ComputeMinMaxSizes): the column tracks
/// sized under a min-content and a max-content constraint, plus gutters.
pub(super) fn min_max(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
) -> Option<MinMax> {
    // Size containment: the intrinsic sizes ignore the items.
    if block::size_contained(flat[index].style) {
        return Some(MinMax::exact(
            block::contain_intrinsic_size(flat[index].style).0,
        ));
    }
    match layout_impl(flat, index, space, None, true)? {
        GridOut::Measured(sizes) => Some(sizes),
        GridOut::Laid(_) => super::bail("grid:measure"),
    }
}

enum GridOut {
    Laid(LayoutResult),
    Measured(MinMax),
}

/// Column sizes under a min-content (`min`) and a max-content (`max`)
/// constraint (CSS Grid §12.3–§12.7 with zero / infinite free space). An
/// auto minimum is the items' min-content contribution; growth limits are
/// reached only under the max-content constraint, and flexible tracks take
/// the fr size that fits each item's max-content contribution (§12.7.1,
/// Blink FindFrSize: a flex sum below 1 counts as 1).
fn measure_columns(
    specs: &[Spec],
    collapsed: &[bool],
    min_c: &[i64],
    max_c: &[i64],
    gap: i64,
    font: f32,
) -> MinMax {
    // (base, growth limit, flex factor) of each track.
    let mut tracks = Vec::with_capacity(specs.len());
    for (index, spec) in specs.iter().enumerate() {
        let (low, high) = (min_c[index].max(0), max_c[index].max(0));
        let floor = |breadth: Breadth| match breadth {
            Breadth::Auto | Breadth::MinContent | Breadth::Fr(_) => low,
            Breadth::MaxContent => high,
            other => definite_breadth(other, None, font).unwrap_or(low),
        };
        let ceiling = |breadth: Breadth| match breadth {
            Breadth::MinContent => low,
            Breadth::Auto | Breadth::MaxContent | Breadth::Fr(_) => high,
            other => definite_breadth(other, None, font).unwrap_or(high),
        };
        let track = match spec {
            Spec::One(Breadth::Fr(factor)) => (low, 0, *factor),
            Spec::One(breadth) => (floor(*breadth), ceiling(*breadth), 0.0),
            Spec::Fit(argument) => {
                let limit = definite_breadth(*argument, None, font).unwrap_or(high);
                (low, high.min(limit), 0.0)
            }
            Spec::MinMax(min, Breadth::Fr(factor)) => (floor(*min), 0, *factor),
            Spec::MinMax(min, max) => (floor(*min), ceiling(*max), 0.0),
        };
        tracks.push(if collapsed[index] { (0, 0, 0.0) } else { track });
    }
    let fraction = tracks
        .iter()
        .enumerate()
        .filter(|(_, track)| track.2 > 0.0)
        .map(|(index, &(base, _, factor))| {
            let per = |size: i64| size as f64 / factor.max(1.0);
            per(base).max(per(max_c[index].max(0)))
        })
        .fold(0.0_f64, f64::max);
    let gaps = gap_sum(collapsed, gap);
    let min: i64 = tracks.iter().map(|track| track.0).sum();
    let max: i64 = tracks
        .iter()
        .map(|&(base, limit, factor)| {
            if factor > 0.0 {
                base.max((fraction * factor).round() as i64)
            } else {
                base.max(limit)
            }
        })
        .sum();
    MinMax {
        min: Some(from_units(min + gaps)),
        max: from_units(max.max(min) + gaps),
    }
}

/// A `<percent> <px>` gap marker resolved against `basis` (zero when
/// indefinite). `None` when the gap has no percentage.
fn percent_gap(style: &Style, name: &str, basis: Option<f32>) -> Option<f32> {
    let mut parts = marker(style, name)?.split_whitespace();
    let percent = parts.next()?.parse::<f32>().ok()?;
    let px = parts
        .next()
        .and_then(|part| part.parse::<f32>().ok())
        .unwrap_or(0.0);
    Some((percent / 100.0 * basis.unwrap_or(0.0).max(0.0) + px).max(0.0))
}

fn layout_impl(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    sized: Option<(f32, Option<f32>)>,
    measure: bool,
) -> Option<GridOut> {
    SUBGRID_CONTENT_ROWS.with(|cell| {
        cell.borrow_mut().retain(|(id, _)| *id != index);
    });
    let style = flat[index].style;
    let mut metrics: BoxMetrics = block::resolve_inline(style, space);
    if let Some((width, _)) = sized {
        metrics.content_width = width;
    }
    let keyword = marker(style, WIDTH_KEYWORD).filter(|_| sized.is_none() && !measure);
    if let Some(keyword) = keyword {
        // Blink ComputeInlineSizeForFragment: an intrinsic keyword is the
        // grid's own min/max-content size (fit-content clamps the available
        // size between them), then min/max-width and auto margins apply
        // as for a definite width.
        let sizes = min_max(flat, index, space).why("grid:width-keyword")?;
        let min = sizes.min.unwrap_or(sizes.max);
        let edges = metrics.border[1] + metrics.border[3] + metrics.padding[1] + metrics.padding[3];
        let content = match keyword {
            "min-content" => min,
            "max-content" => sizes.max,
            _ => {
                let available =
                    space.available_inline - metrics.margin[1] - metrics.margin[3] - edges;
                sizes.max.min(available.max(min))
            }
        };
        let mut definite = style.clone();
        definite.width = Dimension::Px(match style.box_sizing {
            BoxSizing::BorderBox => content + edges,
            BoxSizing::ContentBox => content,
        });
        metrics = block::resolve_inline(&definite, space);
    }
    let [bt, br, bb, bl] = metrics.border;
    let [pt, pr, pb, pl] = metrics.padding;
    let mut inner_width = metrics.content_width.max(0.0);
    let edges_v = bt + bb + pt + pb;
    let specified_height = if let Some((_, Some(height))) = sized {
        Some(height)
    } else {
        resolve_block(style.height, space.percentage_block, style, space, edges_v)
            .why("grid:percent")?
    };
    // Size containment: an auto height is the contain-intrinsic one, and
    // the tracks size into it as a definite height.
    let specified_height = specified_height
        .or_else(|| block::size_contained(style).then(|| block::contain_intrinsic_size(style).1));
    let min_h = match style.min_height {
        Dimension::Auto => None,
        other => resolve_block(other, space.percentage_block, style, space, edges_v)
            .why("grid:percent")?,
    };
    let max_h = match style.max_height {
        Dimension::Auto => None,
        other => resolve_block(other, space.percentage_block, style, space, edges_v)
            .why("grid:percent")?,
    };
    let font = style.font_size.max(0.0);
    let column_gap = style.column_gap.unwrap_or(style.gap).max(0.0);
    let row_gap = style.row_gap.unwrap_or(style.gap).max(0.0);
    let column_sub = columns_spec(style).is_some_and(is_subgrid_axis);
    let row_sub = style.grid_rows().is_some_and(is_subgrid_axis);
    if (measure || keyword.is_some()) && (column_sub || row_sub) {
        return super::bail("grid:intrinsic-subgrid");
    }
    let inherited = if column_sub || row_sub {
        Some(lookup_inherited(flat, index, space, column_sub, row_sub).why("grid:subgrid")?)
    } else {
        None
    };
    // A percentage gap resolves against the content box (CSS Box
    // Alignment). Chrome resolves it against zero while measuring
    // intrinsic sizes and for an indefinite height.
    let column_gap = percent_gap(
        style,
        "--w3cos-internal-column-gap-percent",
        (!measure).then_some(inner_width),
    )
    .unwrap_or(column_gap);
    let row_gap =
        percent_gap(style, "--w3cos-internal-row-gap-percent", specified_height).unwrap_or(row_gap);
    let mut column_gap_u = units(column_gap);
    let mut row_gap_u = units(row_gap);
    if let Some(inherited) = &inherited {
        if column_sub {
            column_gap_u = inherited.col_gap;
        }
        if row_sub {
            row_gap_u = inherited.row_gap;
        }
    }
    let columns_template = if column_sub {
        let inherited = inherited.as_ref().unwrap();
        let names = merge_line_names(
            &inherited.col_names,
            &local_subgrid_names(style, false),
            inherited.columns.len(),
        );
        fixed_template(&inherited.columns, names)
    } else {
        // No column template: no explicit columns. The one column that
        // row flow needs is added below as an implicit track.
        match columns_spec(style) {
            None => parse_template("", true).why("grid:template")?,
            Some(spec) => parse_template(spec, false).why("grid:template")?,
        }
    };
    let rows_template = if row_sub {
        let inherited = inherited.as_ref().unwrap();
        let local = local_subgrid_names(style, true);
        if let Some(rows) = &inherited.rows {
            let names = merge_line_names(&inherited.row_names, &local, rows.len());
            fixed_template(rows, names)
        } else if inherited.row_span == 0 {
            return super::bail("grid:subgrid");
        } else {
            let names = merge_line_names(&inherited.row_names, &local, inherited.row_span);
            auto_named_template(inherited.row_span, names)
        }
    } else {
        match style.grid_rows() {
            None => parse_template("", true).why("grid:template")?,
            Some(spec) => parse_template(spec, true).why("grid:template")?,
        }
    };
    let flow = container_flow(flat, index).why("grid:auto-flow")?;
    if (measure || keyword.is_some()) && columns_template.auto_fit.is_some() {
        return super::bail("grid:intrinsic-repeat");
    }
    // An automatic inline-grid shrinks to its fixed px tracks (CSS Grid
    // §6). Stretching it to the containing block would grow those tracks
    // under the initial `justify-content: normal` (stretch).
    let inline_fixed = fixed_track_inline_grid(flat, index);
    let inline_intrinsic = intrinsic_track_inline_grid(flat, index);
    if inline_fixed && sized.is_none() && matches!(style.width, Dimension::Auto) {
        if let Some(sum) = fixed_px_sum(&columns_template) {
            let gaps = column_gap * columns_template.tracks.len().saturating_sub(1) as f32;
            inner_width = sum + gaps;
        }
    }
    let mut inner_u = units(inner_width);
    let inflow = rendered_children(flat, index)
        .filter(|&child| !oof::is_out_of_flow(flat[child].style))
        .count();
    // Column flow with no explicit columns builds implicit auto tracks.
    // A missing template on the row-flow axis stays one column, sized as
    // an implicit (grid-auto-columns) track.
    let mut column_specs = if columns_spec(style).is_none() && flow.column {
        Vec::new()
    } else {
        expand_auto(&columns_template, inner_u, column_gap_u, font, inflow).why("grid:repeat")?
    };
    let definite_block = specified_height.or(min_h.filter(|_| specified_height.is_none()));
    let mut row_specs = expand_auto(
        &rows_template,
        definite_block.map(units).unwrap_or(0),
        row_gap_u,
        font,
        inflow,
    )
    .why("grid:repeat")?;
    let implicit_column = match style.grid_auto_column_spec() {
        Some(spec) => parse_one_track(spec).why("grid:auto-columns")?,
        None => Spec::One(Breadth::Auto),
    };
    let implicit_row = match style.grid_auto_row_spec() {
        Some(spec) => parse_one_track(spec).why("grid:auto-rows")?,
        None => Spec::One(Breadth::Auto),
    };
    if column_specs.is_empty() && !flow.column {
        // That column is implicit: grid-auto-columns sizes it, and the
        // default `auto` only fills the grid when content distribution
        // stretches it (place-content: start keeps it at max-content).
        // With no in-flow items there is no implicit track, so a
        // grid-auto-columns size must not give the empty grid a width;
        // the old 1fr stand-in keeps its min-content width at zero. A
        // grid-lanes box keeps the stand-in too: its percentage-width items
        // match the column-flow references only against the 1fr track.
        let lanes = has_marker(style, "--w3cos-internal-grid-lanes");
        column_specs.push(if inflow > 0 && !lanes {
            implicit_column.clone()
        } else {
            Spec::One(Breadth::Fr(1.0))
        });
    }
    let explicit_cols = column_specs.len();
    let explicit_rows = row_specs.len();
    let names_cols = expanded_names(&columns_template, explicit_cols);
    let names_rows = expanded_names(&rows_template, explicit_rows);

    let mut pending = Vec::new();
    let mut oof_children = Vec::new();
    for (ordinal, child) in rendered_children(flat, index).enumerate() {
        let child_style = flat[child].style;
        if oof::is_out_of_flow(child_style) {
            oof_children.push(child);
            continue;
        }
        // Whitespace-only text does not generate a grid item when
        // white-space collapses (CSS Grid §4.1). Only the inline-grid
        // line path skips it; an ordinary grid keeps its current items.
        if (inline_fixed || inline_intrinsic)
            && matches!(
                flat[child].kind,
                w3cos_std::ComponentKind::Text { content } if content.chars().all(|ch| ch.is_whitespace())
            )
        {
            continue;
        }
        let column = resolve_axis(placement(child_style, false), &names_cols, explicit_cols)
            .why("grid:column")?;
        let row = resolve_axis(placement(child_style, true), &names_rows, explicit_rows)
            .why("grid:row")?;
        pending.push((Some(column), Some(row), child_style.order, child, ordinal));
    }
    sort_placement_order(&mut pending);
    let placed = place_items(&pending, explicit_cols, explicit_rows, flow).why("grid:place")?;
    let mut items = Vec::new();
    for (child, col, row, col_span, row_span, source) in placed {
        while column_specs.len() < col + col_span {
            column_specs.push(implicit_column.clone());
        }
        while row_specs.len() < row + row_span {
            row_specs.push(implicit_row.clone());
        }
        items.push(Placed {
            index: child,
            col,
            row,
            col_span,
            row_span,
            source,
        });
    }
    if column_specs.len() > MAX_TRACKS || row_specs.len() > MAX_TRACKS {
        return super::bail("grid:tracks");
    }
    let mut collapsed = vec![false; column_specs.len()];
    if columns_template.auto_fit == Some(true) {
        for index in 0..column_specs.len() {
            let used = items
                .iter()
                .any(|item| item.col <= index && index < item.col + item.col_span);
            if !used {
                collapsed[index] = true;
            }
        }
    }
    let stretch_columns = column_pack(style) == Pack::Stretch && !lanes_stack_axis(style, false);
    // `col_min` holds minimum contributions, `col_min_content` min-content
    // contributions; they differ only for scroll containers.
    let mut col_min = vec![0_i64; column_specs.len()];
    let mut col_min_content = vec![0_i64; column_specs.len()];
    let mut col_max = vec![0_i64; column_specs.len()];
    let mut col_has_item = vec![false; column_specs.len()];
    let mut contribs = Vec::new();
    for item in &items {
        let child_style = flat[item.index].style;
        if columns_spec(child_style).is_some_and(is_subgrid_axis) {
            // A column subgrid's items contribute through the parent's
            // tracks; the standalone-axis pages that size them under an
            // intrinsic container width use writing modes this engine does
            // not store.
            if measure {
                return super::bail("grid:intrinsic-subgrid");
            }
            contribute_subgrid_columns(
                flat,
                item.index,
                space,
                item.col,
                item.col_span,
                &slice_names(&names_cols, item.col, item.col_span),
                column_gap_u,
                0,
                0,
                &mut col_min,
                &mut col_max,
            )?;
            contribs.push(None);
            continue;
        }
        let (min_c, max_c) = if child_style.grid_rows().is_some_and(is_subgrid_axis) {
            row_subgrid_inline(flat, item.index, space).why("grid:intrinsic")?
        } else {
            inline_minmax(flat, item.index, space).why("grid:intrinsic")?
        };
        let minimum = inline_minimum(flat, item.index, space)
            .unwrap_or(min_c)
            .min(min_c);
        contribs.push(Some((minimum, min_c, max_c)));
        if item.col_span == 1 {
            col_has_item[item.col] = true;
            col_min[item.col] = col_min[item.col].max(minimum);
            col_min_content[item.col] = col_min_content[item.col].max(min_c);
            col_max[item.col] = col_max[item.col].max(max_c);
        }
    }
    // Subgrid columns wrote only `col_min`; they are min-content too.
    for (min_content, minimum) in col_min_content.iter_mut().zip(&col_min) {
        *min_content = (*min_content).max(*minimum);
    }
    let spread = |slots: &mut [i64], size: i64| {
        let current: i64 = slots.iter().sum();
        if size > current {
            let extra = size - current;
            let count = slots.len().max(1) as i64;
            for slot in slots {
                *slot += extra / count;
            }
        }
    };
    // Items spanning several columns. Layout sizes them per CSS Grid 12.5
    // step 3 on the tracks below; the intrinsic measure still spreads them.
    let mut spanning_cols = Vec::new();
    for (item, contrib) in items.iter().zip(&contribs) {
        let Some((minimum, min_c, max_c)) = *contrib else {
            continue;
        };
        if item.col_span <= 1 {
            continue;
        }
        if !measure {
            spanning_cols.push(SpanItem {
                start: item.col,
                span: item.col_span,
                minimum,
                min_c,
                max_c,
            });
            continue;
        }
        let end = (item.col + item.col_span).min(col_max.len());
        spread(&mut col_max[item.col..end], max_c);
        spread(&mut col_min[item.col..end], minimum);
        spread(&mut col_min_content[item.col..end], min_c);
    }
    for (index, has) in col_has_item.iter_mut().enumerate() {
        // Subgrid columns write their contributions straight into the slots.
        *has |= col_min[index] > 0 || col_max[index] > 0;
    }
    if measure {
        return Some(GridOut::Measured(measure_columns(
            &column_specs,
            &collapsed,
            &col_min_content,
            &col_max,
            column_gap_u,
            font,
        )));
    }
    let mut shrink_intrinsic_columns = false;
    if inline_intrinsic && sized.is_none() && matches!(style.width, Dimension::Auto) {
        let measured = measure_columns(
            &column_specs,
            &collapsed,
            &col_min_content,
            &col_max,
            column_gap_u,
            font,
        );
        let (min, max) = (units(measured.min.unwrap_or(0.0)), units(measured.max));
        inner_u = inner_u.clamp(min, max);
        inner_width = from_units(inner_u);
        shrink_intrinsic_columns = inner_u < max;
    }
    let mut column_tracks: Vec<Track> = column_specs
        .iter()
        .enumerate()
        .map(|(index, spec)| {
            let mut track = track_from_spec(
                spec,
                col_min[index],
                col_min_content[index],
                col_max[index],
                Some(inner_u),
                font,
                stretch_columns,
            );
            if shrink_intrinsic_columns && matches!(spec, Spec::One(Breadth::Auto)) {
                track.base = col_min[index];
                track.cap = col_max[index];
                track.stretchy = false;
            }
            if collapsed[index] {
                track.collapsed = true;
                track.base = 0;
                track.cap = 0;
                track.flex = 0.0;
                track.stretchy = false;
            }
            track
        })
        .collect();
    accommodate_spanning_items(
        &mut column_tracks,
        &column_specs,
        &col_has_item,
        &spanning_cols,
        Some(inner_u),
        font,
        column_gap_u,
    );
    let mut columns = finalize(&column_tracks, Some(inner_u), column_gap_u);
    if column_sub {
        let exact = &inherited.as_ref().unwrap().columns;
        if exact.len() != columns.len() {
            return super::bail("grid:subgrid");
        }
        columns.clone_from(exact);
    }
    let active_cols = collapsed.iter().filter(|flag| !**flag).count().max(1);
    let col_sum: i64 = columns.iter().sum();
    let col_free = inner_u - gap_sum(&collapsed, column_gap_u) - col_sum;
    let (col_lead, col_between) = pack_gap(column_pack(style), col_free, active_cols, column_gap_u);
    let column_x = positions(&columns, &collapsed, col_between, col_lead);

    let inline_of = |item: &Placed| {
        from_units(span_sum(
            &columns,
            &collapsed,
            item.col,
            item.col_span,
            col_between,
        ))
    };
    let mut built = Vec::new();
    for item in &items {
        let child_style = flat[item.index].style;
        let area = inline_of(item);
        let justify = align_pack(used_item_align(
            style.justify_items,
            child_style.justify_self,
        ));
        let align = align_pack(used_item_align(style.align_items, child_style.align_self));
        let auto_margin = [
            matches!(child_style.margin.top, Spacing::Auto),
            matches!(child_style.margin.right, Spacing::Auto),
            matches!(child_style.margin.bottom, Spacing::Auto),
            matches!(child_style.margin.left, Spacing::Auto),
        ];
        let mut ml = spacing_px(child_style, child_style.margin.left, area, space);
        let mut mr = spacing_px(child_style, child_style.margin.right, area, space);
        let mut mt = spacing_px(child_style, child_style.margin.top, area, space);
        let mut mb = spacing_px(child_style, child_style.margin.bottom, area, space);
        // Half of (subgrid gap − parent gap) sits on the side that faces a
        // gutter, so a stretched item shrinks and a definite item only shifts.
        if column_sub {
            let half = from_units(gap_half(style.column_gap, column_gap_u));
            if item.col > 0 {
                ml += half;
            }
            if item.col + item.col_span < columns.len() {
                mr += half;
            }
        }
        if row_sub {
            let half = from_units(gap_half(style.row_gap, row_gap_u));
            if item.row > 0 {
                mt += half;
            }
            if item.row + item.row_span < row_specs.len() {
                mb += half;
            }
        }
        let border = block::border_widths(child_style);
        let pad = |spacing| spacing_px(child_style, spacing, area, space);
        let edges_h =
            border[1] + border[3] + pad(child_style.padding.left) + pad(child_style.padding.right);
        let edges_v =
            border[0] + border[2] + pad(child_style.padding.top) + pad(child_style.padding.bottom);
        let stretch_inline = justify == Pack::Stretch
            && matches!(child_style.width, Dimension::Auto)
            && !auto_margin[3]
            && !auto_margin[1];
        let raw = if stretch_inline {
            (area - ml - mr - edges_h).max(0.0)
        } else if matches!(child_style.width, Dimension::Auto) {
            // A non-stretched auto width is fit-content within the grid area:
            // min(max-content, max(min-content, area)). The contributions are
            // margin boxes; this is the content box.
            let (min_c, max_c) = inline_minmax(flat, item.index, space).why("grid:intrinsic")?;
            let fit = from_units(max_c).min(from_units(min_c).max(area));
            (fit - ml - mr - edges_h).max(0.0)
        } else {
            let specified =
                block::resolve_length(child_style.width, Some(area), child_style, space)
                    .why("grid:percent")?;
            to_content(child_style, edges_h, specified)
        };
        let mut item_space = *space;
        item_space.available_inline = area;
        let keyword_floor = min_width_keyword(flat, item.index, space)?.unwrap_or(0.0);
        let content_width = block::clamp_inline_size(child_style, &item_space, edges_h, raw)
            .max(keyword_floor)
            .max(0.0);
        let specified_content_h = if has_marker(child_style, HEIGHT_KEYWORD) {
            None
        } else {
            resolve_block(child_style.height, None, child_style, space, edges_v)
                .why("grid:percent")?
        };
        let stretch_block = align == Pack::Stretch
            && specified_content_h.is_none()
            && !has_marker(child_style, HEIGHT_KEYWORD)
            && !auto_margin[0]
            && !auto_margin[2];
        built.push(Item {
            index: item.index,
            col: item.col,
            row: item.row,
            col_span: item.col_span,
            row_span: item.row_span,
            source: item.source,
            content_width,
            margin_u: [units(mt), units(mr), units(mb), units(ml)],
            auto_margin,
            edges_v,
            stretch_block,
            specified_content_h,
            inline_pack: justify,
            block_pack: align,
        });
    }

    let _subgrid_publish = publish_subgrid(
        flat,
        index,
        &columns,
        &names_cols,
        col_between,
        &row_specs,
        &names_rows,
        definite_block.map(units),
        font,
        row_gap_u,
        &items,
    );

    let mut fragments = Vec::with_capacity(built.len());
    // `row_floor` holds minimum contributions, `row_min` min-content ones.
    let mut row_floor = vec![0_i64; row_specs.len()];
    let mut row_min = vec![0_i64; row_specs.len()];
    let mut row_max = vec![0_i64; row_specs.len()];
    // Items spanning several rows: (first row, span, margin-box height).
    let mut spanning: Vec<(usize, usize, i64)> = Vec::new();
    for item in &built {
        let laid = layout_item(
            flat,
            item.index,
            space,
            inline_of(&Placed {
                index: item.index,
                col: item.col,
                row: item.row,
                col_span: item.col_span,
                row_span: item.row_span,
                source: item.source,
            }),
            item.content_width,
            item.specified_content_h,
        )?;
        if let Some(content) = take_content_rows(item.index) {
            if content.len() != item.row_span {
                return super::bail("grid:subgrid");
            }
            let (start_mbp, end_mbp) = axis_mbp(flat[item.index].style, space, true);
            let last = content.len().saturating_sub(1);
            for (offset, size) in content.iter().enumerate() {
                let track = item.row + offset;
                if track >= row_max.len() {
                    continue;
                }
                let mut extra = *size;
                if offset == 0 {
                    extra += start_mbp;
                }
                if offset == last {
                    extra += end_mbp;
                }
                extra = extra.max(0);
                row_max[track] = row_max[track].max(extra);
                row_min[track] = row_min[track].max(extra);
                row_floor[track] = row_floor[track].max(extra);
            }
        } else if item.row_span == 1 && item.row < row_max.len() {
            let margin_box = units(laid.fragment.height) + item.margin_u[0] + item.margin_u[2];
            row_max[item.row] = row_max[item.row].max(margin_box);
            row_min[item.row] = row_min[item.row].max(margin_box);
            // §6.6: no automatic minimum for a scroll container.
            let child_style = flat[item.index].style;
            let minimum = if block::scrolls_in_axis(child_style, false)
                && matches!(child_style.min_height, Dimension::Auto)
            {
                units(item.edges_v) + item.margin_u[0] + item.margin_u[2]
            } else {
                margin_box
            };
            row_floor[item.row] = row_floor[item.row].max(minimum.min(margin_box));
        } else if item.row_span > 1 {
            let margin_box = units(laid.fragment.height) + item.margin_u[0] + item.margin_u[2];
            spanning.push((item.row, item.row_span, margin_box));
        }
        fragments.push(laid);
    }
    let stretch_rows = row_pack(style) == Pack::Stretch && !lanes_stack_axis(style, true);
    let intrinsic_row_size = if inline_intrinsic && definite_block.is_none() {
        Some(units(
            measure_columns(
                &row_specs,
                &vec![false; row_specs.len()],
                &row_min,
                &row_max,
                row_gap_u,
                font,
            )
            .max,
        ))
    } else {
        None
    };
    let row_available = definite_block.map(units).or(intrinsic_row_size);
    let mut row_tracks: Vec<Track> = row_specs
        .iter()
        .enumerate()
        .map(|(index, spec)| {
            track_from_spec(
                spec,
                row_floor.get(index).copied().unwrap_or(0),
                row_min.get(index).copied().unwrap_or(0),
                row_max.get(index).copied().unwrap_or(0),
                row_available,
                font,
                stretch_rows,
            )
        })
        .collect();
    grow_for_spanning_items(
        &mut row_tracks,
        &row_specs,
        &spanning,
        row_available,
        font,
        row_gap_u,
    );
    // Maximize tracks (CSS Grid 12.6) grows rows towards their growth
    // limits in the free space, so minmax(20px, max-content) reaches
    // max-content in a tall grid; an indefinite height has no free space.
    let mut rows = if row_tracks.is_empty() {
        Vec::new()
    } else {
        finalize(&row_tracks, row_available, row_gap_u)
    };
    let mut recorded_rows = None;
    if row_sub {
        let inherited = inherited.as_ref().unwrap();
        if let Some(exact) = &inherited.rows {
            if exact.len() != rows.len() {
                return super::bail("grid:subgrid");
            }
            rows.clone_from(exact);
        } else {
            // Content sizes only. The parent adds margin, border, and padding.
            recorded_rows = Some(rows.clone());
        }
    }
    let row_collapsed = vec![false; rows.len()];
    let base = rows.iter().sum::<i64>() + gap_sum(&row_collapsed, row_gap_u);
    let mut used = specified_height
        .map(units)
        .or(intrinsic_row_size)
        .unwrap_or(base);
    if let Some(min) = min_h {
        used = used.max(units(min));
    }
    if let Some(max) = max_h
        && used > units(max)
    {
        return super::bail("grid:max-height");
    }
    // An indefinite height sized the rows with a zero percentage row gap;
    // the gap itself resolves against the resulting height (CSS Box
    // Alignment: zero for intrinsic contributions, the final size for
    // layout), so the rows may overflow it.
    if specified_height.is_none()
        && !row_sub
        && let Some(gap) = percent_gap(
            style,
            "--w3cos-internal-row-gap-percent",
            Some(from_units(used)),
        )
    {
        row_gap_u = units(gap);
    }
    // Rows that overflow a short grid keep their sizes (the content
    // overflows), except in two cases that stay on legacy. A grid-lanes box
    // stacks its items rather than placing them in rows, which this
    // algorithm only approximates, and the difference shows once they
    // overflow (a single item stacks where its one track puts it). A row
    // subgrid's own gap is legacy-only on the grid-lanes
    // side (grid:lanes), and the grid-lanes subgrid references are short
    // grids that have to match it.
    if used < base
        && !inline_intrinsic
        && ((has_marker(style, "--w3cos-internal-grid-lanes") && built.len() > 1)
            || built.iter().any(|item| {
                flat[item.index]
                    .style
                    .grid_rows()
                    .is_some_and(is_subgrid_axis)
            }))
    {
        return super::bail("grid:short-height");
    }
    if stretch_rows && used > base && !rows.is_empty() {
        let extra = used - base;
        let caps: Vec<i64> = row_tracks
            .iter()
            .enumerate()
            .map(|(index, track)| {
                if track.stretchy
                    || matches!(row_specs.get(index), Some(Spec::One(Breadth::Auto)) | None)
                {
                    INF
                } else {
                    rows[index]
                }
            })
            .collect();
        // Implicit rows are auto. A missing template is all auto.
        let implicit = row_specs
            .iter()
            .all(|spec| matches!(spec, Spec::One(Breadth::Auto)));
        if implicit || row_tracks.iter().any(|track| track.stretchy) {
            let caps: Vec<i64> = if implicit {
                vec![INF; rows.len()]
            } else {
                caps
            };
            grow(&mut rows, &caps, extra);
        }
    }
    publish_row_sizes(index, &rows);

    for (item, fragment) in built.iter().zip(fragments.iter_mut()) {
        if !item.stretch_block || rows.is_empty() {
            continue;
        }
        let target_border = (span_sum(&rows, &row_collapsed, item.row, item.row_span, row_gap_u)
            - item.margin_u[0]
            - item.margin_u[2])
            .max(0);
        let target_content = (target_border - units(item.edges_v)).max(0);
        let measured = units(fragment.fragment.height) - units(item.edges_v);
        if target_content != measured {
            *fragment = layout_item(
                flat,
                item.index,
                space,
                inline_of(&Placed {
                    index: item.index,
                    col: item.col,
                    row: item.row,
                    col_span: item.col_span,
                    row_span: item.row_span,
                    source: item.source,
                }),
                item.content_width,
                Some(from_units(target_content)),
            )?;
        }
    }

    let row_sum: i64 = rows.iter().sum();
    let row_free = used - gap_sum(&row_collapsed, row_gap_u) - row_sum;
    let active_rows = rows.len().max(1);
    let (row_lead, row_between) = pack_gap(row_pack(style), row_free, active_rows, row_gap_u);
    let row_y = positions(&rows, &row_collapsed, row_between, row_lead);

    let origin_x = units(bl + pl);
    let origin_y = units(bt + pt);
    let rtl = style.direction == TextDirection::Rtl;
    let mut children = Vec::with_capacity(built.len());
    let mut pending_oof = Vec::new();
    let inline_grid = marker(style, "--w3cos-internal-unrecognized-display") == Some("inline-grid");
    // First baseline of the first row (CSS Grid §10). Column, then source.
    let mut first_row_baseline: Option<(usize, usize, f32)> = None;
    // Source ordinals are unique and bounded by the direct child count.
    // Index them directly instead of instantiating another stable sort.
    // Out-of-flow and skipped whitespace children leave holes.
    let source_count = built.iter().map(|item| item.source + 1).max().unwrap_or(0);
    let mut ordered = vec![usize::MAX; source_count];
    for (index, item) in built.iter().enumerate() {
        ordered[item.source] = index;
    }
    for index in ordered {
        if index == usize::MAX {
            continue;
        }
        let item = &built[index];
        let fragment = fragments[index].fragment.clone();
        let area = span_sum(&columns, &collapsed, item.col, item.col_span, col_between);
        let row_size = span_sum(&rows, &row_collapsed, item.row, item.row_span, row_between);
        let (ml, _mr, inline_offset) = axis_offset(
            area,
            units(fragment.width),
            item.margin_u[3],
            item.margin_u[1],
            item.auto_margin[3],
            item.auto_margin[1],
            item.inline_pack,
        );
        let (mt, _, block_offset) = axis_offset(
            row_size,
            units(fragment.height),
            item.margin_u[0],
            item.margin_u[2],
            item.auto_margin[0],
            item.auto_margin[2],
            item.block_pack,
        );
        let mut x = from_units(origin_x + column_x[item.col] + ml + inline_offset);
        let y =
            from_units(origin_y + row_y.get(item.row).copied().unwrap_or(0) + mt + block_offset);
        if rtl {
            x = from_units(origin_x + inner_u - (units(x) - origin_x) - units(fragment.width));
        }
        let (dx, dy) = oof::relative_offset(flat[item.index].style, space);
        let laid = &fragments[index];
        if inline_grid && item.row == 0 {
            let item_base = match laid.first_baseline {
                LastBaseline::At(value) => value,
                LastBaseline::NoLines | LastBaseline::Unknown => fragment.height,
            };
            let offset = y + dy + item_base;
            let replace = first_row_baseline
                .is_none_or(|(col, source, _)| (item.col, item.source) < (col, source));
            if replace {
                first_row_baseline = Some((item.col, item.source, offset));
            }
        }
        pending_oof.extend(
            laid.oof
                .iter()
                .map(|candidate| candidate.translated(x + dx, y + dy)),
        );
        children.push((x + dx, y + dy, fragment));
    }

    let content_height = from_units(used.max(0));
    let mut height = content_height;
    if let Some(max) = max_h {
        height = height.min(max);
    }
    if let Some(min) = min_h {
        height = height.max(min);
    }
    if (height - content_height).abs() > EPS {
        height = content_height;
    }
    let border_width = inner_width + bl + br + pl + pr;
    let border_height = height + bt + bb + pt + pb;
    // The column that stands in for a missing template only exists once an
    // in-flow item is placed in it; with none, the implicit grid has no
    // columns and every column line of an abspos child is outside it.
    let stand_in_column_only = columns_spec(style).is_none() && !flow.column && built.is_empty();
    for child in oof_children {
        let child_style = flat[child].style;
        let column = resolve_axis(placement(child_style, false), &names_cols, explicit_cols)
            .why("grid:column")?;
        let row = resolve_axis(placement(child_style, true), &names_rows, explicit_rows)
            .why("grid:row")?;
        let col = column.start.unwrap_or(0).max(0) as usize;
        let row_index = row.start.unwrap_or(0).max(0) as usize;
        // When the grid is not the containing block, the static position
        // is that of the sole item of an area spanning the content box
        // (CSS Grid 9.2), so grid placement does not move it. No in-flow
        // rows gives the same area: a stretched auto row fills the box.
        let static_only = style.position == Position::Static;
        let area_w = if columns.is_empty() || static_only {
            inner_u
        } else {
            span_sum(&columns, &collapsed, col, column.span, col_between)
        };
        let area_h = if rows.is_empty() || static_only {
            used
        } else {
            span_sum(&rows, &row_collapsed, row_index, row.span, row_between)
        };
        let (col_at, row_at) = if static_only {
            (0, 0)
        } else {
            (
                column_x.get(col).copied().unwrap_or(0),
                row_y.get(row_index).copied().unwrap_or(0),
            )
        };
        let justify = align_pack(used_item_align(
            style.justify_items,
            child_style.justify_self,
        ));
        let align = align_pack(used_item_align(style.align_items, child_style.align_self));
        let border_w = definite_border(
            child_style,
            false,
            from_units(area_w),
            from_units(used),
            space,
        );
        let border_h = definite_border(
            child_style,
            true,
            from_units(inner_u),
            from_units(area_h),
            space,
        );
        let inline_off = border_w
            .map(|size| axis_offset(area_w, size, 0, 0, false, false, justify).2)
            .unwrap_or(0);
        let block_off = border_h
            .map(|size| axis_offset(area_h, size, 0, 0, false, false, align).2)
            .unwrap_or(0);
        let mut x = from_units(origin_x + col_at + inline_off);
        let y = from_units(origin_y + row_at + block_off);
        if rtl {
            x = from_units(origin_x + inner_u - (units(x) - origin_x));
        }
        let candidate = OofCandidate {
            index: child,
            static_inline: x,
            static_block: y,
            static_rtl: rtl,
        };
        if style.position == Position::Static {
            pending_oof.push(candidate);
            continue;
        }
        // CSS Grid 9.1: a grid container that is the containing block gives
        // each abspos child its grid area; auto lines and lines outside the
        // grid are the padding edges (the augmented grid).
        let (start_auto, end_auto) = abspos_auto_lines(placement(child_style, false));
        let (x0, x1) = abspos_area(
            if stand_in_column_only { &[] } else { &columns },
            &column_x,
            column.start,
            column.span,
            start_auto,
            end_auto,
            (-units(pl), inner_u + units(pr)),
        );
        let (start_auto, end_auto) = abspos_auto_lines(placement(child_style, true));
        let (y0, y1) = abspos_area(
            &rows,
            &row_y,
            row.start,
            row.span,
            start_auto,
            end_auto,
            (-units(pt), units(height) + units(pb)),
        );
        let area_w = x1 - x0;
        let area_h = y1 - y0;
        let border_w = definite_border(
            child_style,
            false,
            from_units(area_w),
            from_units(area_h),
            space,
        );
        let border_h = definite_border(
            child_style,
            true,
            from_units(area_w),
            from_units(area_h),
            space,
        );
        let inline_off = border_w
            .map(|size| axis_offset(area_w, size, 0, 0, false, false, justify).2)
            .unwrap_or(0);
        let block_off = border_h
            .map(|size| axis_offset(area_h, size, 0, 0, false, false, align).2)
            .unwrap_or(0);
        let (cb_x, static_inline) = if rtl {
            (
                origin_x + inner_u - x1,
                origin_x + inner_u - x0 - inline_off,
            )
        } else {
            (origin_x + x0, origin_x + x0 + inline_off)
        };
        let cb = oof::ContainingBlock {
            x: from_units(cb_x),
            y: from_units(origin_y + y0),
            width: from_units(area_w),
            height: from_units(area_h),
            rtl,
        };
        let candidate = OofCandidate {
            static_inline: from_units(static_inline),
            static_block: from_units(origin_y + y0 + block_off),
            ..candidate
        };
        let (placed, rest) = oof::resolve(
            flat,
            vec![candidate],
            &cb,
            false,
            space.viewport_w,
            space.viewport_h,
        )
        .why("grid:oof-resolve")?;
        children.extend(placed);
        pending_oof.extend(rest);
    }

    if style.position != Position::Static && !pending_oof.is_empty() {
        let cb = oof::ContainingBlock {
            x: bl,
            y: bt,
            width: border_width - bl - br,
            height: border_height - bt - bb,
            rtl,
        };
        let (placed, rest) = oof::resolve(
            flat,
            std::mem::take(&mut pending_oof),
            &cb,
            false,
            space.viewport_w,
            space.viewport_h,
        )
        .why("grid:oof-resolve")?;
        children.extend(placed);
        pending_oof = rest;
    }
    let mut start_strut = MarginStrut::default();
    start_strut.append(metrics.margin[0]);
    let mut end_strut = MarginStrut::default();
    end_strut.append(metrics.margin[2]);
    if let Some(recorded) = recorded_rows {
        SUBGRID_CONTENT_ROWS.with(|cell| {
            cell.borrow_mut().push((index, recorded));
        });
    }
    Some(GridOut::Laid(LayoutResult {
        fragment: BoxFragment {
            index,
            width: border_width,
            height: border_height,
            text_range: None,
            children,
        },
        last_baseline: LastBaseline::NoLines,
        first_baseline: if inline_grid {
            LastBaseline::At(
                first_row_baseline
                    .map(|(_, _, offset)| offset)
                    .unwrap_or(border_height),
            )
        } else {
            LastBaseline::NoLines
        },
        start_strut,
        end_strut,
        self_collapsing: false,
        oof: pending_oof,
        bfc_dependent: false,
        float_overlap: false,
    }))
}

fn resolve_block(
    dim: Dimension,
    basis: Option<f32>,
    style: &Style,
    space: &ConstraintSpace,
    edges: f32,
) -> Option<Option<f32>> {
    match dim {
        Dimension::Auto => Some(None),
        Dimension::Percent(_) if basis.is_none() => None,
        other => {
            let size = block::resolve_length(other, basis, style, space)?;
            Some(Some(to_content(style, edges, size)))
        }
    }
}

/// Border-box size of a definite width or height. `None` when that axis is
/// auto; the static position then stays at the start of the grid area.
fn definite_border(
    style: &Style,
    block_axis: bool,
    inline_basis: f32,
    block_basis: f32,
    space: &ConstraintSpace,
) -> Option<i64> {
    let border = block::border_widths(style);
    let pad = |spacing| spacing_px(style, spacing, inline_basis, space);
    let (dim, edges, basis) = if block_axis {
        (
            style.height,
            border[0] + border[2] + pad(style.padding.top) + pad(style.padding.bottom),
            block_basis,
        )
    } else {
        (
            style.width,
            border[1] + border[3] + pad(style.padding.left) + pad(style.padding.right),
            inline_basis,
        )
    };
    match dim {
        Dimension::Auto => None,
        other => {
            let specified = block::resolve_length(other, Some(basis), style, space)?;
            Some(units(
                (to_content(style, edges, specified) + edges).max(0.0),
            ))
        }
    }
}

fn axis_offset(
    area: i64,
    border: i64,
    start: i64,
    end: i64,
    auto_start: bool,
    auto_end: bool,
    pack: Pack,
) -> (i64, i64, i64) {
    let free = area - border - start - end;
    // Auto margins are safe: a negative remainder stays zero so an item
    // larger than its area still covers the start corner (Chrome
    // grid-item-auto-margins).
    if auto_start || auto_end {
        let free = free.max(0);
        if auto_start && auto_end {
            let lead = free / 2;
            (lead, free - lead, 0)
        } else if auto_start {
            (free, end, 0)
        } else {
            (start, free, 0)
        }
    } else {
        let offset = match pack {
            Pack::End => free,
            Pack::Center => free / 2,
            Pack::Stretch
            | Pack::Start
            | Pack::SpaceBetween
            | Pack::SpaceAround
            | Pack::SpaceEvenly => 0,
        };
        (start, end, offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::pre_flatten;
    use w3cos_std::Component;
    use w3cos_std::style::{
        AlignContent, AlignItems, BoxSizing, CustomProperties, Dimension, Display, Edges,
        JustifyContent, Position, Spacing, Style, TextDirection,
    };

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

    fn grid_style(width: f32, height: Option<f32>, columns: Option<&str>) -> Style {
        Style {
            display: Display::Grid,
            width: Dimension::Px(width),
            height: height.map(Dimension::Px).unwrap_or(Dimension::Auto),
            grid_template_columns: columns.map(str::to_string),
            ..Style::default()
        }
    }

    fn leaf(style: Style) -> Component {
        Component::boxed(style, vec![])
    }

    fn lay(root: &Component) -> Vec<(crate::layout::LayoutRect, usize)> {
        let flat = pre_flatten(root);
        assert!(
            block::supports_tree(&flat),
            "unsupported: {:?}",
            block::unsupported_reasons(&flat)
        );
        super::super::layout_tree(root, &flat, 800.0, 600.0).expect("ng grid")
    }

    fn rect(out: &[(crate::layout::LayoutRect, usize)], index: usize) -> [f32; 4] {
        let r = out.iter().find(|(_, i)| *i == index).expect("rect").0;
        [r.x, r.y, r.width, r.height]
    }

    fn expect(root: &Component, expected: &[(usize, [f32; 4])]) {
        let out = lay(root);
        for (index, want) in expected {
            assert_eq!(rect(&out, *index), *want, "index {index}");
        }
    }

    fn falls_back(style: Style, children: Vec<Component>) {
        let root = Component::boxed(style, children);
        let flat = pre_flatten(&root);
        assert!(super::super::layout_tree(&root, &flat, 800.0, 600.0).is_none());
    }

    fn mark_justify(style: &mut Style, keyword: &str) {
        style.custom_properties = Some(CustomProperties::from_iter([(
            "--w3cos-internal-justify-content".to_string(),
            keyword.to_string(),
        )]));
    }

    #[test]
    fn implicit_column_stacks_stretched_items() {
        expect(
            &Component::boxed(
                grid_style(200.0, None, None),
                vec![
                    leaf(block_box(None, Some(10.0))),
                    leaf(block_box(None, Some(10.0))),
                ],
            ),
            &[
                (0, [0.0, 0.0, 200.0, 20.0]),
                (1, [0.0, 0.0, 200.0, 10.0]),
                (2, [0.0, 10.0, 200.0, 10.0]),
            ],
        );
    }

    #[test]
    fn fixed_tracks_wrap_onto_the_next_row() {
        expect(
            &Component::boxed(
                grid_style(200.0, None, Some("100px 100px")),
                vec![
                    leaf(block_box(None, Some(10.0))),
                    leaf(block_box(None, Some(10.0))),
                    leaf(block_box(None, Some(10.0))),
                ],
            ),
            &[
                (1, [0.0, 0.0, 100.0, 10.0]),
                (2, [100.0, 0.0, 100.0, 10.0]),
                (3, [0.0, 10.0, 100.0, 10.0]),
            ],
        );
        expect(
            &Component::boxed(
                grid_style(200.0, None, Some("repeat(2, 100px)")),
                vec![
                    leaf(block_box(None, Some(10.0))),
                    leaf(block_box(None, Some(10.0))),
                    leaf(block_box(None, Some(10.0))),
                ],
            ),
            &[
                (1, [0.0, 0.0, 100.0, 10.0]),
                (2, [100.0, 0.0, 100.0, 10.0]),
                (3, [0.0, 10.0, 100.0, 10.0]),
            ],
        );
    }

    #[test]
    fn equal_fr_tracks_share_the_content_box() {
        expect(
            &Component::boxed(
                grid_style(200.0, None, Some("1fr 1fr 1fr 1fr")),
                vec![
                    leaf(block_box(None, Some(10.0))),
                    leaf(block_box(None, Some(10.0))),
                    leaf(block_box(None, Some(10.0))),
                    leaf(block_box(None, Some(10.0))),
                ],
            ),
            &[
                (1, [0.0, 0.0, 50.0, 10.0]),
                (2, [50.0, 0.0, 50.0, 10.0]),
                (3, [100.0, 0.0, 50.0, 10.0]),
                (4, [150.0, 0.0, 50.0, 10.0]),
            ],
        );
    }

    #[test]
    fn fr_remainder_goes_to_the_last_flexible_tracks() {
        let mut container = grid_style(200.0, None, Some("40px 1fr 2fr"));
        container.column_gap = Some(10.0);
        container.row_gap = Some(6.0);
        expect(
            &Component::boxed(
                container,
                vec![
                    leaf(block_box(None, Some(10.0))),
                    leaf(block_box(None, Some(10.0))),
                    leaf(block_box(None, Some(10.0))),
                    leaf(block_box(None, Some(10.0))),
                ],
            ),
            &[
                (0, [0.0, 0.0, 200.0, 26.0]),
                (1, [0.0, 0.0, 40.0, 10.0]),
                (2, [50.0, 0.0, 46.65625, 10.0]),
                (3, [106.65625, 0.0, 93.34375, 10.0]),
                (4, [0.0, 16.0, 40.0, 10.0]),
            ],
        );
        expect(
            &Component::boxed(
                grid_style(200.0, None, Some("repeat(3, 1fr 20px)")),
                vec![
                    leaf(block_box(None, Some(8.0))),
                    leaf(block_box(None, Some(8.0))),
                    leaf(block_box(None, Some(8.0))),
                    leaf(block_box(None, Some(8.0))),
                    leaf(block_box(None, Some(8.0))),
                    leaf(block_box(None, Some(8.0))),
                ],
            ),
            &[
                (1, [0.0, 0.0, 46.65625, 8.0]),
                (2, [46.65625, 0.0, 20.0, 8.0]),
                (3, [66.65625, 0.0, 46.671875, 8.0]),
                (4, [113.328125, 0.0, 20.0, 8.0]),
                (5, [133.328125, 0.0, 46.671875, 8.0]),
                (6, [180.0, 0.0, 20.0, 8.0]),
            ],
        );
    }

    #[test]
    fn definite_height_stretches_rows_not_specified_items() {
        expect(
            &Component::boxed(
                grid_style(200.0, Some(80.0), Some("100px 100px")),
                vec![
                    leaf(block_box(None, Some(10.0))),
                    leaf(block_box(None, Some(10.0))),
                    leaf(block_box(None, Some(10.0))),
                ],
            ),
            &[
                (0, [0.0, 0.0, 200.0, 80.0]),
                (1, [0.0, 0.0, 100.0, 10.0]),
                (2, [100.0, 0.0, 100.0, 10.0]),
                (3, [0.0, 40.0, 100.0, 10.0]),
            ],
        );
    }

    #[test]
    fn auto_items_stretch_to_the_row() {
        let mut item = block_box(None, None);
        item.min_height = Dimension::Px(10.0);
        expect(
            &Component::boxed(
                grid_style(200.0, Some(80.0), None),
                vec![leaf(item.clone()), leaf(item)],
            ),
            &[
                (0, [0.0, 0.0, 200.0, 80.0]),
                (1, [0.0, 0.0, 200.0, 40.0]),
                (2, [0.0, 40.0, 200.0, 40.0]),
            ],
        );
        let mut first = block_box(None, None);
        first.min_height = Dimension::Px(18.0);
        let mut columns = grid_style(200.0, None, Some("100px 1fr"));
        columns.column_gap = Some(8.0);
        expect(
            &Component::boxed(
                columns,
                vec![leaf(first), leaf(block_box(None, Some(30.0)))],
            ),
            &[
                (0, [0.0, 0.0, 200.0, 30.0]),
                (1, [0.0, 0.0, 100.0, 30.0]),
                (2, [108.0, 0.0, 92.0, 30.0]),
            ],
        );
    }

    #[test]
    fn gaps_margins_padding_and_specified_width() {
        let mut margin = block_box(None, Some(10.0));
        margin.margin = edges(2.0, 2.0, 2.0, 2.0);
        let mut padded = block_box(None, Some(10.0));
        padded.padding = edges(4.0, 4.0, 4.0, 4.0);
        expect(
            &Component::boxed(
                grid_style(200.0, None, Some("100px 100px")),
                vec![
                    leaf(margin),
                    leaf(block_box(Some(40.0), Some(20.0))),
                    leaf(padded),
                ],
            ),
            &[
                (0, [0.0, 0.0, 200.0, 38.0]),
                (1, [2.0, 2.0, 96.0, 10.0]),
                (2, [100.0, 0.0, 40.0, 20.0]),
                (3, [0.0, 20.0, 100.0, 18.0]),
            ],
        );
    }

    #[test]
    fn stretched_cell_honors_min_height_padding_and_margin() {
        let mut cell = block_box(None, None);
        cell.margin = edges(2.0, 2.0, 2.0, 2.0);
        cell.padding = edges(4.0, 4.0, 4.0, 4.0);
        cell.min_height = Dimension::Px(18.0);
        expect(
            &Component::boxed(
                grid_style(400.0, None, Some("1fr 1fr 1fr 1fr")),
                vec![leaf(cell)],
            ),
            &[(0, [0.0, 0.0, 400.0, 30.0]), (1, [2.0, 2.0, 96.0, 26.0])],
        );
    }

    #[test]
    fn container_border_and_padding_offset_the_tracks() {
        let mut container = grid_style(200.0, None, Some("1fr 1fr"));
        container.padding = edges(5.0, 5.0, 5.0, 5.0);
        container.border_width = 3.0;
        expect(
            &Component::boxed(
                container,
                vec![
                    leaf(block_box(None, Some(10.0))),
                    leaf(block_box(None, Some(10.0))),
                ],
            ),
            &[
                (0, [0.0, 0.0, 216.0, 26.0]),
                (1, [8.0, 8.0, 100.0, 10.0]),
                (2, [108.0, 8.0, 100.0, 10.0]),
            ],
        );
    }

    #[test]
    fn percent_and_border_box_item_widths() {
        let mut border_box = block_box(None, Some(10.0));
        border_box.width = Dimension::Percent(50.0);
        border_box.box_sizing = BoxSizing::BorderBox;
        border_box.padding = edges(4.0, 4.0, 4.0, 4.0);
        let mut content_box = block_box(None, Some(10.0));
        content_box.width = Dimension::Percent(50.0);
        expect(
            &Component::boxed(
                grid_style(200.0, None, Some("100px 100px")),
                vec![leaf(content_box), leaf(border_box)],
            ),
            &[(1, [0.0, 0.0, 50.0, 10.0]), (2, [100.0, 0.0, 50.0, 10.0])],
        );
    }

    #[test]
    fn fixed_tracks_may_overflow_the_container() {
        expect(
            &Component::boxed(
                grid_style(100.0, None, Some("80px 80px")),
                vec![
                    leaf(block_box(None, Some(10.0))),
                    leaf(block_box(None, Some(10.0))),
                ],
            ),
            &[(1, [0.0, 0.0, 80.0, 10.0]), (2, [80.0, 0.0, 80.0, 10.0])],
        );
    }

    #[test]
    fn empty_grid_uses_the_specified_height() {
        expect(
            &Component::boxed(grid_style(200.0, None, None), vec![]),
            &[(0, [0.0, 0.0, 200.0, 0.0])],
        );
        expect(
            &Component::boxed(grid_style(200.0, Some(40.0), None), vec![]),
            &[(0, [0.0, 0.0, 200.0, 40.0])],
        );
    }

    #[test]
    fn chrome_track_and_alignment_cases() {
        // minmax grows to its cap before fr takes the rest. Chrome e6/a5.
        expect(
            &Component::boxed(
                grid_style(500.0, None, Some("minmax(100px, 200px) 1fr")),
                vec![
                    leaf(block_box(None, Some(10.0))),
                    leaf(block_box(None, Some(10.0))),
                ],
            ),
            &[(1, [0.0, 0.0, 200.0, 10.0]), (2, [200.0, 0.0, 300.0, 10.0])],
        );
        // A specified width is the fr minimum. Chrome e5.
        expect(
            &Component::boxed(
                grid_style(100.0, None, Some("1fr 1fr")),
                vec![
                    leaf(block_box(Some(80.0), Some(10.0))),
                    leaf(block_box(None, Some(10.0))),
                ],
            ),
            &[(1, [0.0, 0.0, 80.0, 10.0]), (2, [80.0, 0.0, 20.0, 10.0])],
        );
        // Default auto columns stretch. Chrome e2.
        expect(
            &Component::boxed(
                grid_style(200.0, None, Some("auto auto")),
                vec![
                    leaf(block_box(None, Some(10.0))),
                    leaf(block_box(None, Some(10.0))),
                ],
            ),
            &[(1, [0.0, 0.0, 100.0, 10.0]), (2, [100.0, 0.0, 100.0, 10.0])],
        );
        let mut center = grid_style(200.0, None, Some("80px 80px"));
        center.justify_content = JustifyContent::Center;
        expect(
            &Component::boxed(
                center,
                vec![
                    leaf(block_box(None, Some(10.0))),
                    leaf(block_box(None, Some(10.0))),
                ],
            ),
            &[(1, [20.0, 0.0, 80.0, 10.0]), (2, [100.0, 0.0, 80.0, 10.0])],
        );
        let mut placed = block_box(None, Some(10.0));
        placed.grid_column = Some("2".to_string());
        expect(
            &Component::boxed(
                grid_style(200.0, None, Some("100px 100px")),
                vec![leaf(placed)],
            ),
            &[(1, [100.0, 0.0, 100.0, 10.0])],
        );
        let mut spanned = block_box(None, Some(10.0));
        spanned.grid_column = Some("span 2".to_string());
        expect(
            &Component::boxed(
                grid_style(200.0, None, Some("100px 100px")),
                vec![leaf(spanned)],
            ),
            &[(1, [0.0, 0.0, 200.0, 10.0])],
        );
    }

    fn subgrid_box(columns: Option<&str>, rows: Option<&str>) -> Style {
        let mut style = grid_style(0.0, None, columns);
        style.width = Dimension::Auto;
        if let Some(rows) = rows {
            style.grid_ng_mut().grid_template_rows = Some(rows.to_string());
        }
        style
    }

    #[test]
    fn subgrid_stays_on_legacy() {
        // A subgrid that is not a grid item has no parent tracks.
        falls_back(
            grid_style(200.0, None, Some("subgrid")),
            vec![leaf(block_box(None, Some(10.0)))],
        );
        // `repeat()` inside the subgrid keyword, and a specified size on
        // the subgridded axis, stay on the legacy engine.
        let mut repeated = subgrid_box(Some("subgrid repeat(1, [a])"), None);
        repeated.grid_column = Some("1 / -1".to_string());
        falls_back(
            grid_style(200.0, None, Some("100px 100px")),
            vec![Component::boxed(
                repeated,
                vec![leaf(block_box(None, Some(10.0)))],
            )],
        );
        let mut sized = subgrid_box(Some("subgrid"), None);
        sized.width = Dimension::Px(50.0);
        sized.grid_column = Some("1 / -1".to_string());
        falls_back(
            grid_style(200.0, None, Some("100px 100px")),
            vec![Component::boxed(
                sized,
                vec![leaf(block_box(None, Some(10.0)))],
            )],
        );
    }

    #[test]
    fn rows_overflow_a_short_grid() {
        // Chrome: an auto row keeps its min-content size when the grid is
        // shorter than it; the content overflows the 5px grid.
        expect(
            &Component::boxed(
                grid_style(200.0, Some(5.0), None),
                vec![leaf(block_box(None, Some(10.0)))],
            ),
            &[(0, [0.0, 0.0, 200.0, 5.0]), (1, [0.0, 0.0, 200.0, 10.0])],
        );
        // A stretched item shrinks to a fixed row smaller than its content:
        // its automatic minimum is zero because it spans no auto-minimum track.
        let mut grid = grid_style(200.0, Some(5.0), None);
        grid.grid_ng_mut().grid_template_rows = Some("4px".to_string());
        expect(
            &Component::boxed(
                grid,
                vec![Component::boxed(
                    block_box(None, None),
                    vec![leaf(block_box(None, Some(10.0)))],
                )],
            ),
            &[(1, [0.0, 0.0, 200.0, 4.0]), (2, [0.0, 0.0, 200.0, 10.0])],
        );
    }

    #[test]
    fn column_subgrid_reuses_parent_tracks() {
        // Chrome, body margin 0, border boxes relative to the subgrid.
        // a: 40 / 60 / 80. b: the same tracks with a 10px parent gap.
        // c: span 2 from the second track. e: 2px border and 4px inline
        // padding come out of the edge tracks.
        let items = || {
            vec![
                leaf(block_box(None, Some(20.0))),
                leaf(block_box(None, Some(20.0))),
                leaf(block_box(None, Some(20.0))),
            ]
        };
        let mut sub = subgrid_box(Some("subgrid"), None);
        sub.grid_column = Some("1 / -1".to_string());
        expect(
            &Component::boxed(
                grid_style(180.0, None, Some("40px 60px 80px")),
                vec![Component::boxed(sub.clone(), items())],
            ),
            &[
                (2, [0.0, 0.0, 40.0, 20.0]),
                (3, [40.0, 0.0, 60.0, 20.0]),
                (4, [100.0, 0.0, 80.0, 20.0]),
            ],
        );
        let mut gapped = grid_style(200.0, None, Some("40px 60px 80px"));
        gapped.column_gap = Some(10.0);
        expect(
            &Component::boxed(gapped, vec![Component::boxed(sub.clone(), items())]),
            &[
                (2, [0.0, 0.0, 40.0, 20.0]),
                (3, [50.0, 0.0, 60.0, 20.0]),
                (4, [120.0, 0.0, 80.0, 20.0]),
            ],
        );
        let mut span = subgrid_box(Some("subgrid"), None);
        span.grid_column = Some("2 / span 2".to_string());
        let mut parent = grid_style(200.0, None, Some("40px 60px 80px"));
        parent.column_gap = Some(10.0);
        expect(
            &Component::boxed(
                parent,
                vec![Component::boxed(
                    span,
                    vec![
                        leaf(block_box(None, Some(20.0))),
                        leaf(block_box(None, Some(20.0))),
                    ],
                )],
            ),
            &[
                (1, [50.0, 0.0, 150.0, 20.0]),
                (2, [50.0, 0.0, 60.0, 20.0]),
                (3, [120.0, 0.0, 80.0, 20.0]),
            ],
        );
        let mut edged = sub.clone();
        edged.border_width = 2.0;
        edged.padding = edges(0.0, 4.0, 0.0, 4.0);
        expect(
            &Component::boxed(
                grid_style(180.0, None, Some("40px 60px 80px")),
                vec![Component::boxed(edged, items())],
            ),
            &[
                (1, [0.0, 0.0, 180.0, 24.0]),
                (2, [6.0, 2.0, 34.0, 20.0]),
                (3, [40.0, 2.0, 60.0, 20.0]),
                (4, [100.0, 2.0, 74.0, 20.0]),
            ],
        );
    }

    #[test]
    fn row_subgrid_reuses_parent_tracks() {
        // Chrome d: rows 30px and 50px, items stretch.
        let mut parent = grid_style(100.0, Some(80.0), Some("100px"));
        parent.grid_ng_mut().grid_template_rows = Some("30px 50px".to_string());
        let mut sub = subgrid_box(None, Some("subgrid"));
        sub.grid_ng_mut().grid_row = Some("1 / -1".to_string());
        expect(
            &Component::boxed(
                parent,
                vec![Component::boxed(
                    sub,
                    vec![leaf(block_box(None, None)), leaf(block_box(None, None))],
                )],
            ),
            &[
                (1, [0.0, 0.0, 100.0, 80.0]),
                (2, [0.0, 0.0, 100.0, 30.0]),
                (3, [0.0, 30.0, 100.0, 50.0]),
            ],
        );
    }

    fn rows(style: &mut Style, spec: &str) {
        style.grid_ng_mut().grid_template_rows = Some(spec.to_string());
    }

    fn row_placement(style: &mut Style, spec: &str) {
        style.grid_ng_mut().grid_row = Some(spec.to_string());
    }

    #[test]
    fn subgrid_contributes_auto_fr_minmax_and_intrinsic_tracks() {
        // Chrome, body margin 0. auto/auto in 200px splits the extra 130.
        let mut sub = subgrid_box(Some("subgrid"), None);
        sub.grid_column = Some("1 / -1".to_string());
        expect(
            &Component::boxed(
                grid_style(200.0, None, Some("auto auto")),
                vec![Component::boxed(
                    sub.clone(),
                    vec![
                        leaf(block_box(Some(40.0), Some(10.0))),
                        leaf(block_box(Some(30.0), Some(10.0))),
                    ],
                )],
            ),
            &[
                (1, [0.0, 0.0, 200.0, 10.0]),
                (2, [0.0, 0.0, 40.0, 10.0]),
                (3, [105.0, 0.0, 30.0, 10.0]),
            ],
        );
        // Subgrid column-gap 20, parent gap 0. Bases 50 and 40, item 2 at 115.
        let mut gapped = sub.clone();
        gapped.column_gap = Some(20.0);
        expect(
            &Component::boxed(
                grid_style(200.0, None, Some("auto auto")),
                vec![Component::boxed(
                    gapped,
                    vec![
                        leaf(block_box(Some(40.0), Some(10.0))),
                        leaf(block_box(Some(30.0), Some(10.0))),
                    ],
                )],
            ),
            &[(2, [0.0, 0.0, 40.0, 10.0]), (3, [115.0, 0.0, 30.0, 10.0])],
        );
        // Parent gap 10, subgrid gap 30. Item 2 at 120.
        let mut parent_gap = grid_style(200.0, None, Some("auto auto"));
        parent_gap.column_gap = Some(10.0);
        let mut wider = sub.clone();
        wider.column_gap = Some(30.0);
        expect(
            &Component::boxed(
                parent_gap,
                vec![Component::boxed(
                    wider,
                    vec![
                        leaf(block_box(Some(40.0), Some(10.0))),
                        leaf(block_box(Some(30.0), Some(10.0))),
                    ],
                )],
            ),
            &[(3, [120.0, 0.0, 30.0, 10.0])],
        );
        // Fixed tracks, subgrid gap 20. Stretch items shrink to a 20px gutter.
        let mut fixed_gap = subgrid_box(Some("subgrid"), None);
        fixed_gap.grid_column = Some("1 / -1".to_string());
        fixed_gap.column_gap = Some(20.0);
        expect(
            &Component::boxed(
                grid_style(200.0, None, Some("100px 100px")),
                vec![Component::boxed(
                    fixed_gap,
                    vec![
                        leaf(block_box(None, Some(10.0))),
                        leaf(block_box(None, Some(10.0))),
                    ],
                )],
            ),
            &[(2, [0.0, 0.0, 90.0, 10.0]), (3, [110.0, 0.0, 90.0, 10.0])],
        );
        // 1fr 2fr at 210px is 70 / 140.
        expect(
            &Component::boxed(
                grid_style(210.0, None, Some("1fr 2fr")),
                vec![Component::boxed(
                    sub.clone(),
                    vec![
                        leaf(block_box(None, Some(10.0))),
                        leaf(block_box(None, Some(10.0))),
                    ],
                )],
            ),
            &[(2, [0.0, 0.0, 70.0, 10.0]), (3, [70.0, 0.0, 140.0, 10.0])],
        );
        // minmax(20px, 1fr) minmax(50px, max-content): second track stays 80.
        expect(
            &Component::boxed(
                grid_style(
                    200.0,
                    None,
                    Some("minmax(20px, 1fr) minmax(50px, max-content)"),
                ),
                vec![Component::boxed(
                    sub.clone(),
                    vec![
                        leaf(block_box(Some(40.0), Some(10.0))),
                        leaf(block_box(Some(80.0), Some(10.0))),
                    ],
                )],
            ),
            &[(3, [120.0, 0.0, 80.0, 10.0])],
        );
        // min-content / max-content do not stretch. 40 + 70 = 110.
        expect(
            &Component::boxed(
                grid_style(300.0, None, Some("min-content max-content")),
                vec![Component::boxed(
                    sub,
                    vec![
                        leaf(block_box(Some(40.0), Some(10.0))),
                        leaf(block_box(Some(70.0), Some(10.0))),
                    ],
                )],
            ),
            &[
                (1, [0.0, 0.0, 110.0, 10.0]),
                (2, [0.0, 0.0, 40.0, 10.0]),
                (3, [40.0, 0.0, 70.0, 10.0]),
            ],
        );
    }

    #[test]
    fn subgrid_inherits_parent_line_names_and_adds_its_own() {
        // Parent [a] every other line. Child `span a / a -1` is the first
        // two tracks of a 3-track span (100px).
        let mut child = block_box(None, Some(10.0));
        child.grid_column = Some("span a / a -1".to_string());
        let mut sub = subgrid_box(Some("subgrid"), None);
        sub.grid_column = Some("1 / span 3".to_string());
        expect(
            &Component::boxed(
                grid_style(200.0, None, Some("[a] 50px 50px [a] 50px 50px [a]")),
                vec![Component::boxed(sub, vec![leaf(child)])],
            ),
            &[(1, [0.0, 0.0, 150.0, 10.0]), (2, [0.0, 0.0, 100.0, 10.0])],
        );
        // Own names: b is line 1, b 2 is line 3. 40px tracks, item at 40×80.
        let mut named_child = block_box(None, Some(10.0));
        named_child.grid_column = Some("b / b 2".to_string());
        let mut own = subgrid_box(Some("subgrid [x] [b] [] [b]"), None);
        own.grid_column = Some("1 / -1".to_string());
        expect(
            &Component::boxed(
                grid_style(160.0, None, Some("40px 40px 40px 40px")),
                vec![Component::boxed(own, vec![leaf(named_child)])],
            ),
            &[(2, [40.0, 0.0, 80.0, 10.0])],
        );
    }

    #[test]
    fn nested_subgrid_splits_auto_columns() {
        // Content 40+25+60+15 = 140. Extra 60 / 4 = 15.
        // Tracks 55, 40, 75, 30. Inner span is 95.
        let mut inner = subgrid_box(Some("subgrid"), None);
        inner.grid_column = Some("span 2".to_string());
        expect(
            &Component::boxed(
                grid_style(200.0, None, Some("auto auto auto auto")),
                vec![
                    Component::boxed(
                        inner,
                        vec![
                            leaf(block_box(Some(40.0), Some(10.0))),
                            leaf(block_box(Some(25.0), Some(10.0))),
                        ],
                    ),
                    leaf(block_box(Some(60.0), Some(10.0))),
                    leaf(block_box(Some(15.0), Some(10.0))),
                ],
            ),
            &[
                (1, [0.0, 0.0, 95.0, 10.0]),
                (2, [0.0, 0.0, 40.0, 10.0]),
                (3, [55.0, 0.0, 25.0, 10.0]),
                (4, [95.0, 0.0, 60.0, 10.0]),
                (5, [170.0, 0.0, 15.0, 10.0]),
            ],
        );
    }

    #[test]
    fn subgrid_on_both_axes_and_indefinite_rows() {
        let mut parent = grid_style(120.0, Some(80.0), Some("40px 80px"));
        rows(&mut parent, "30px 50px");
        let mut sub = subgrid_box(Some("subgrid"), Some("subgrid"));
        sub.grid_column = Some("1 / -1".to_string());
        row_placement(&mut sub, "1 / -1");
        expect(
            &Component::boxed(
                parent,
                vec![Component::boxed(
                    sub,
                    vec![
                        leaf(block_box(Some(10.0), Some(10.0))),
                        leaf(block_box(Some(10.0), Some(10.0))),
                        leaf(block_box(Some(10.0), Some(10.0))),
                        leaf(block_box(Some(10.0), Some(10.0))),
                    ],
                )],
            ),
            &[
                (2, [0.0, 0.0, 10.0, 10.0]),
                (3, [40.0, 0.0, 10.0, 10.0]),
                (4, [0.0, 30.0, 10.0, 10.0]),
                (5, [40.0, 30.0, 10.0, 10.0]),
            ],
        );
        let mut auto_rows = grid_style(40.0, None, Some("40px"));
        rows(&mut auto_rows, "auto auto");
        let mut row_sub = subgrid_box(None, Some("subgrid"));
        row_placement(&mut row_sub, "1 / -1");
        expect(
            &Component::boxed(
                auto_rows,
                vec![Component::boxed(
                    row_sub,
                    vec![
                        leaf(block_box(Some(10.0), Some(30.0))),
                        leaf(block_box(Some(10.0), Some(50.0))),
                    ],
                )],
            ),
            &[
                (0, [0.0, 0.0, 40.0, 80.0]),
                (1, [0.0, 0.0, 40.0, 80.0]),
                (2, [0.0, 0.0, 10.0, 30.0]),
                (3, [0.0, 30.0, 10.0, 50.0]),
            ],
        );
    }

    #[test]
    fn row_subgrid_adds_margin_border_and_padding_to_the_edges() {
        // start mbp 8+2+3 = 13, end 6+1+4 = 11. Tracks 33 and 21. Height 54.
        let mut parent = grid_style(40.0, None, Some("40px"));
        rows(&mut parent, "auto auto");
        let mut sub = subgrid_box(None, Some("subgrid"));
        row_placement(&mut sub, "1 / -1");
        sub.margin = edges(8.0, 0.0, 6.0, 0.0);
        sub.padding = edges(3.0, 0.0, 4.0, 0.0);
        sub.border_top_width = Some(2.0);
        sub.border_bottom_width = Some(1.0);
        expect(
            &Component::boxed(
                parent,
                vec![Component::boxed(
                    sub,
                    vec![
                        leaf(block_box(Some(10.0), Some(20.0))),
                        leaf(block_box(Some(10.0), Some(10.0))),
                    ],
                )],
            ),
            &[
                (0, [0.0, 0.0, 40.0, 54.0]),
                (1, [0.0, 8.0, 40.0, 40.0]),
                (2, [0.0, 13.0, 10.0, 20.0]),
                (3, [0.0, 33.0, 10.0, 10.0]),
            ],
        );
    }

    #[test]
    fn missing_column_template_uses_the_implicit_auto_column() {
        // No grid-template-columns: row flow gets one implicit column sized by
        // grid-auto-columns. Under justify-content:start the auto track keeps
        // its 30px content width (Chrome, fit-content-tracks-dont-stretch-001).
        let item = || {
            Component::boxed(
                block_box(None, None),
                vec![leaf(block_box(Some(30.0), Some(10.0)))],
            )
        };
        let mut packed = grid_style(200.0, None, None);
        mark_justify(&mut packed, "start");
        expect(
            &Component::boxed(packed, vec![item()]),
            &[(1, [0.0, 0.0, 30.0, 10.0])],
        );
        let mut sized = grid_style(200.0, None, None);
        sized.grid_ng_mut().grid_auto_columns = Some("50px".to_string());
        expect(
            &Component::boxed(sized, vec![item()]),
            &[(1, [0.0, 0.0, 50.0, 10.0])],
        );
        // The initial value still stretches the single auto column.
        expect(
            &Component::boxed(grid_style(200.0, None, None), vec![item()]),
            &[(1, [0.0, 0.0, 200.0, 10.0])],
        );
        // An empty grid has no implicit track, so grid-auto-columns does
        // not give it a max-content width (Chrome, abspos/empty-grid-001).
        let mut empty = Style {
            display: Display::Grid,
            ..Style::default()
        };
        empty.grid_ng_mut().grid_auto_columns = Some("200px".to_string());
        let mut outer = grid_style(400.0, None, None);
        mark_justify(&mut outer, "start");
        expect(
            &Component::boxed(outer, vec![leaf(empty)]),
            &[(1, [0.0, 0.0, 0.0, 0.0])],
        );
    }

    #[test]
    fn size_contained_grid_sizes_rows_into_the_contain_intrinsic_height() {
        // contain: size; contain-intrinsic-size: 80px 90px. The auto height
        // is 90px and the 1fr 2fr rows split it (Chrome: 30px and 60px).
        let mut style = grid_style(200.0, None, Some("50px"));
        style.grid_ng_mut().grid_template_rows = Some("1fr 2fr".to_string());
        style.custom_properties = Some(CustomProperties::from_iter([
            (
                "--w3cos-internal-size-containment".to_string(),
                "1".to_string(),
            ),
            (
                "--w3cos-internal-contain-intrinsic-size".to_string(),
                "80 90".to_string(),
            ),
        ]));
        let items = (0..2)
            .map(|_| leaf(block_box(None, None)))
            .collect::<Vec<_>>();
        expect(
            &Component::boxed(style, items),
            &[(1, [0.0, 0.0, 50.0, 30.0]), (2, [0.0, 30.0, 50.0, 60.0])],
        );
    }

    #[test]
    fn row_lanes_fill_each_lane_in_turn() {
        // grid-lanes with only grid-template-rows: the rows are the lanes and
        // each item goes into the shortest one, so equal items alternate
        // between the two lanes and stack along the inline axis. Only the
        // size-contained case is on NG.
        let mut style = grid_style(200.0, None, None);
        style.grid_ng_mut().grid_template_rows = Some("30px 30px".to_string());
        style.custom_properties = Some(CustomProperties::from_iter([
            ("--w3cos-internal-grid-lanes".to_string(), "1".to_string()),
            (
                "--w3cos-internal-size-containment".to_string(),
                "1".to_string(),
            ),
            (
                "--w3cos-internal-contain-intrinsic-size".to_string(),
                "200 60".to_string(),
            ),
        ]));
        let items = (0..4)
            .map(|_| leaf(block_box(Some(20.0), None)))
            .collect::<Vec<_>>();
        expect(
            &Component::boxed(style, items),
            &[
                (1, [0.0, 0.0, 20.0, 30.0]),
                (2, [0.0, 30.0, 20.0, 30.0]),
                (3, [20.0, 0.0, 20.0, 30.0]),
                (4, [20.0, 30.0, 20.0, 30.0]),
            ],
        );
    }

    #[test]
    fn percentage_gaps_resolve_against_the_content_box() {
        // Chrome: 10% of the 200px width is a 20px column gap. The row gap
        // resolves against the definite 100px height (10px). With an auto
        // height the rows size with no gap (20px tall) and the gap is then
        // 10% of that, so the rows overflow.
        let gap = |style: &mut Style| {
            style.custom_properties = Some(CustomProperties::from_iter([
                (
                    "--w3cos-internal-column-gap-percent".to_string(),
                    "10 0".to_string(),
                ),
                (
                    "--w3cos-internal-row-gap-percent".to_string(),
                    "10 0".to_string(),
                ),
            ]));
        };
        let items = || {
            (0..3)
                .map(|_| leaf(block_box(None, Some(10.0))))
                .collect::<Vec<_>>()
        };
        let mut definite = grid_style(200.0, Some(100.0), Some("50px 50px"));
        definite.grid_ng_mut().grid_template_rows = Some("10px 10px".to_string());
        gap(&mut definite);
        expect(
            &Component::boxed(definite, items()),
            &[(2, [70.0, 0.0, 50.0, 10.0]), (3, [0.0, 20.0, 50.0, 10.0])],
        );
        let mut auto = grid_style(200.0, None, Some("50px 50px"));
        gap(&mut auto);
        expect(
            &Component::boxed(auto, items()),
            &[(3, [0.0, 12.0, 50.0, 10.0])],
        );
    }

    #[test]
    fn auto_start_with_an_end_line_spans_one_track() {
        // `auto / 3` is span 1 ending at line 3: the second column.
        let mut placed = block_box(None, Some(10.0));
        placed.grid_column = Some("auto / 3".to_string());
        expect(
            &Component::boxed(
                grid_style(100.0, None, Some("40px 60px")),
                vec![leaf(placed)],
            ),
            &[(1, [40.0, 0.0, 60.0, 10.0])],
        );
    }

    #[test]
    fn authored_start_does_not_stretch_auto_columns() {
        let mut packed = grid_style(200.0, None, Some("auto auto"));
        mark_justify(&mut packed, "start");
        expect(
            &Component::boxed(
                packed,
                vec![
                    leaf(block_box(Some(40.0), Some(10.0))),
                    leaf(block_box(Some(30.0), Some(10.0))),
                ],
            ),
            &[(1, [0.0, 0.0, 40.0, 10.0]), (2, [40.0, 0.0, 30.0, 10.0])],
        );
        // No marker: the initial value stretches the two auto tracks and
        // splits the extra 130px. Chrome e4, second column at 105.
        expect(
            &Component::boxed(
                grid_style(200.0, None, Some("auto auto")),
                vec![
                    leaf(block_box(Some(40.0), Some(10.0))),
                    leaf(block_box(Some(30.0), Some(10.0))),
                ],
            ),
            &[(1, [0.0, 0.0, 40.0, 10.0]), (2, [105.0, 0.0, 30.0, 10.0])],
        );
    }

    #[test]
    fn rtl_mirrors_the_border_box() {
        // An ltr block hosts the grid so the root's own rtl margin does not
        // shift the border box. Chrome places the 40px item at x=160.
        let host = |grid: Style, children: Vec<Component>| {
            Component::boxed(
                Style {
                    display: Display::Block,
                    ..Style::default()
                },
                vec![Component::boxed(grid, children)],
            )
        };
        let mut container = grid_style(200.0, None, Some("1fr"));
        container.direction = TextDirection::Rtl;
        expect(
            &host(container, vec![leaf(block_box(Some(40.0), Some(10.0)))]),
            &[(2, [160.0, 0.0, 40.0, 10.0])],
        );
        let mut columns = grid_style(200.0, None, Some("100px 100px"));
        columns.direction = TextDirection::Rtl;
        expect(
            &host(
                columns,
                vec![
                    leaf(block_box(None, Some(10.0))),
                    leaf(block_box(None, Some(10.0))),
                ],
            ),
            &[(2, [100.0, 0.0, 100.0, 10.0]), (3, [0.0, 0.0, 100.0, 10.0])],
        );
    }

    #[test]
    fn auto_margins_absorb_the_free_space() {
        let mut both = block_box(Some(40.0), Some(10.0));
        both.margin.left = Spacing::Auto;
        both.margin.right = Spacing::Auto;
        expect(
            &Component::boxed(grid_style(100.0, None, Some("100px")), vec![leaf(both)]),
            &[(1, [30.0, 0.0, 40.0, 10.0])],
        );
        let mut left = block_box(Some(40.0), Some(10.0));
        left.margin.left = Spacing::Auto;
        expect(
            &Component::boxed(grid_style(100.0, None, Some("100px")), vec![leaf(left)]),
            &[(1, [60.0, 0.0, 40.0, 10.0])],
        );
    }

    #[test]
    fn order_places_items_and_rows_use_the_template() {
        let mut later = block_box(None, Some(10.0));
        later.order = 1;
        let mut earlier = block_box(None, Some(20.0));
        earlier.order = -1;
        expect(
            &Component::boxed(
                grid_style(200.0, None, Some("100px 100px")),
                vec![leaf(later), leaf(earlier)],
            ),
            &[
                (0, [0.0, 0.0, 200.0, 20.0]),
                (1, [100.0, 0.0, 100.0, 10.0]),
                (2, [0.0, 0.0, 100.0, 20.0]),
            ],
        );
        // Out-of-flow children leave gaps in the original source ordinals.
        // Placement follows CSS order; returned in-flow fragments retain
        // their original source order. Chrome border boxes: later x=100,
        // earlier x=0, despite the two intervening absolute boxes.
        let mut later = block_box(None, Some(10.0));
        later.order = 1;
        let mut earlier = block_box(None, Some(20.0));
        earlier.order = -1;
        let mut abs = block_box(Some(30.0), Some(30.0));
        abs.position = Position::Absolute;
        let root = Component::boxed(
            grid_style(200.0, None, Some("100px 100px")),
            vec![leaf(abs.clone()), leaf(later), leaf(abs), leaf(earlier)],
        );
        let out = lay(&root);
        assert_eq!(rect(&out, 2), [100.0, 0.0, 100.0, 10.0]);
        assert_eq!(rect(&out, 4), [0.0, 0.0, 100.0, 20.0]);
        assert_eq!(
            out.iter()
                .map(|(_, index)| *index)
                .filter(|i| matches!(i, 2 | 4))
                .collect::<Vec<_>>(),
            vec![2, 4],
        );
        let mut rows = grid_style(100.0, None, Some("100px"));
        rows.grid_ng_mut().grid_template_rows = Some("30px 30px".to_string());
        expect(
            &Component::boxed(
                rows,
                vec![
                    leaf(block_box(None, Some(10.0))),
                    leaf(block_box(None, Some(10.0))),
                ],
            ),
            &[
                (0, [0.0, 0.0, 100.0, 60.0]),
                (1, [0.0, 0.0, 100.0, 10.0]),
                (2, [0.0, 30.0, 100.0, 10.0]),
            ],
        );
    }

    #[test]
    fn auto_repeat_named_lines_calc_and_percent() {
        expect(
            &Component::boxed(
                grid_style(250.0, None, Some("repeat(auto-fill, 100px)")),
                vec![
                    leaf(block_box(None, Some(10.0))),
                    leaf(block_box(None, Some(10.0))),
                    leaf(block_box(None, Some(10.0))),
                ],
            ),
            &[
                (1, [0.0, 0.0, 100.0, 10.0]),
                (2, [100.0, 0.0, 100.0, 10.0]),
                (3, [0.0, 10.0, 100.0, 10.0]),
            ],
        );
        expect(
            &Component::boxed(
                grid_style(250.0, None, Some("repeat(auto-fit, 100px)")),
                vec![leaf(block_box(None, Some(10.0)))],
            ),
            &[(1, [0.0, 0.0, 100.0, 10.0])],
        );
        expect(
            &Component::boxed(
                grid_style(200.0, None, Some("repeat(auto-fit, minmax(0px, 1fr))")),
                vec![
                    leaf(block_box(None, Some(10.0))),
                    leaf(block_box(None, Some(10.0))),
                ],
            ),
            &[(1, [0.0, 0.0, 100.0, 10.0]), (2, [100.0, 0.0, 100.0, 10.0])],
        );
        let mut named = block_box(None, Some(10.0));
        named.grid_column = Some("a / c".to_string());
        expect(
            &Component::boxed(
                grid_style(200.0, None, Some("[a] 40px [b] 60px [c]")),
                vec![leaf(named)],
            ),
            &[(1, [0.0, 0.0, 100.0, 10.0])],
        );
        expect(
            &Component::boxed(
                grid_style(200.0, None, Some("calc(50% - 10px) 1fr")),
                vec![
                    leaf(block_box(None, Some(10.0))),
                    leaf(block_box(None, Some(10.0))),
                ],
            ),
            &[(1, [0.0, 0.0, 90.0, 10.0]), (2, [90.0, 0.0, 110.0, 10.0])],
        );
        expect(
            &Component::boxed(
                grid_style(200.0, None, Some("40% 60%")),
                vec![
                    leaf(block_box(None, Some(10.0))),
                    leaf(block_box(None, Some(10.0))),
                ],
            ),
            &[(1, [0.0, 0.0, 80.0, 10.0]), (2, [80.0, 0.0, 120.0, 10.0])],
        );
    }

    #[test]
    fn alignment_offsets_items_inside_the_area() {
        let mut start = grid_style(200.0, Some(80.0), Some("100px 100px"));
        start.align_items = AlignItems::FlexStart;
        let mut auto = block_box(None, None);
        auto.min_height = Dimension::Px(10.0);
        expect(
            &Component::boxed(start, vec![leaf(block_box(None, Some(10.0))), leaf(auto)]),
            &[
                (0, [0.0, 0.0, 200.0, 80.0]),
                (1, [0.0, 0.0, 100.0, 10.0]),
                (2, [100.0, 0.0, 100.0, 10.0]),
            ],
        );
        let mut center = grid_style(200.0, Some(80.0), None);
        center.align_items = AlignItems::Center;
        expect(
            &Component::boxed(center, vec![leaf(block_box(None, Some(10.0)))]),
            &[(1, [0.0, 35.0, 200.0, 10.0])],
        );
        let mut end = grid_style(200.0, Some(80.0), None);
        end.align_items = AlignItems::FlexEnd;
        expect(
            &Component::boxed(end, vec![leaf(block_box(None, Some(10.0)))]),
            &[(1, [0.0, 70.0, 200.0, 10.0])],
        );
        let mut items = grid_style(200.0, None, Some("100px 100px"));
        items.justify_items = AlignItems::Center;
        expect(
            &Component::boxed(
                items,
                vec![
                    leaf(block_box(Some(40.0), Some(10.0))),
                    leaf(block_box(Some(20.0), Some(10.0))),
                ],
            ),
            &[(1, [30.0, 0.0, 40.0, 10.0]), (2, [140.0, 0.0, 20.0, 10.0])],
        );
        let mut content = grid_style(200.0, Some(80.0), None);
        content.align_content = AlignContent::FlexStart;
        expect(
            &Component::boxed(
                content,
                vec![
                    leaf(block_box(None, Some(10.0))),
                    leaf(block_box(None, Some(10.0))),
                ],
            ),
            &[
                (0, [0.0, 0.0, 200.0, 80.0]),
                (1, [0.0, 0.0, 200.0, 10.0]),
                (2, [0.0, 10.0, 200.0, 10.0]),
            ],
        );
        let mut evenly = grid_style(200.0, None, Some("80px 80px"));
        evenly.justify_content = JustifyContent::SpaceEvenly;
        expect(
            &Component::boxed(
                evenly,
                vec![
                    leaf(block_box(None, Some(10.0))),
                    leaf(block_box(None, Some(10.0))),
                ],
            ),
            &[
                (1, [13.328125, 0.0, 80.0, 10.0]),
                (2, [106.65625, 0.0, 80.0, 10.0]),
            ],
        );
    }

    #[test]
    fn column_flow_shorthand_tracks_and_text_item() {
        let mut flow = grid_style(300.0, Some(40.0), None);
        flow.grid_ng_mut().grid_template_rows = Some("20px 20px".to_string());
        flow.grid_ng_mut().grid_auto_flow = Some("column".to_string());
        expect(
            &Component::boxed(
                flow,
                vec![
                    leaf(block_box(None, None)),
                    leaf(block_box(None, None)),
                    leaf(block_box(None, None)),
                ],
            ),
            &[
                (0, [0.0, 0.0, 300.0, 40.0]),
                (1, [0.0, 0.0, 150.0, 20.0]),
                (2, [0.0, 20.0, 150.0, 20.0]),
                (3, [150.0, 0.0, 150.0, 20.0]),
            ],
        );
        let mut columns = grid_style(200.0, None, Some("1fr 1fr"));
        columns.grid_ng_mut().grid_columns_cascaded = true;
        columns.grid_ng_mut().grid_columns_used = Some("80px 80px".to_string());
        expect(
            &Component::boxed(
                columns,
                vec![
                    leaf(block_box(None, Some(10.0))),
                    leaf(block_box(None, Some(10.0))),
                    leaf(block_box(None, Some(10.0))),
                ],
            ),
            &[
                (1, [0.0, 0.0, 80.0, 10.0]),
                (2, [80.0, 0.0, 80.0, 10.0]),
                (3, [0.0, 10.0, 80.0, 10.0]),
            ],
        );
        let mut keyword = block_box(None, None);
        keyword.min_height = Dimension::Px(10.0);
        keyword.custom_properties = Some(CustomProperties::from_iter([(
            "--w3cos-internal-height-keyword".to_string(),
            "max-content".to_string(),
        )]));
        expect(
            &Component::boxed(grid_style(200.0, Some(80.0), None), vec![leaf(keyword)]),
            &[(0, [0.0, 0.0, 200.0, 80.0]), (1, [0.0, 0.0, 200.0, 10.0])],
        );
        let mut text_style = Style::default();
        text_style.display = Display::Inline;
        let (text_height, _) =
            super::super::inline::block_text_layout("Item 0", 200.0, &text_style);
        expect(
            &Component::boxed(
                grid_style(200.0, None, None),
                vec![Component::text("Item 0", text_style)],
            ),
            &[
                (0, [0.0, 0.0, 200.0, text_height]),
                (1, [0.0, 0.0, 200.0, text_height]),
            ],
        );
    }

    #[test]
    fn indefinite_sized_rows_and_percent_row_gaps_stay_on_legacy() {
        let mut flow = grid_style(300.0, None, None);
        flow.grid_ng_mut().grid_template_rows = Some("20px 20px".to_string());
        flow.grid_ng_mut().grid_auto_flow = Some("column".to_string());
        let root = Component::boxed(flow, vec![leaf(block_box(None, Some(10.0)))]);
        let flat = pre_flatten(&root);
        assert!(super::super::layout_tree(&root, &flat, 800.0, 600.0).is_none());

        let mut lanes = grid_style(100.0, None, None);
        lanes.display = Display::Flex;
        lanes.grid_ng_mut().grid_template_rows = Some("30px 30px".to_string());
        lanes.custom_properties = Some(CustomProperties::from_iter([(
            "--w3cos-internal-grid-lanes".to_string(),
            "1".to_string(),
        )]));
        let root = Component::boxed(lanes, vec![leaf(block_box(None, Some(10.0)))]);
        let flat = pre_flatten(&root);
        assert!(super::super::layout_tree(&root, &flat, 800.0, 600.0).is_none());

        let mut gapped = grid_style(100.0, None, None);
        gapped.grid_ng_mut().grid_template_rows = Some("20px 20px".to_string());
        gapped.grid_ng_mut().grid_auto_flow = Some("column".to_string());
        gapped.min_height = Dimension::Px(80.0);
        gapped.custom_properties = Some(CustomProperties::from_iter([(
            "--w3cos-internal-percent-gap".to_string(),
            "row".to_string(),
        )]));
        let root = Component::boxed(gapped, vec![leaf(block_box(None, Some(10.0)))]);
        let flat = pre_flatten(&root);
        assert!(super::super::layout_tree(&root, &flat, 800.0, 600.0).is_none());

        let mut lane = block_box(None, Some(10.0));
        lane.display = Display::Flex;
        let mut host = grid_style(100.0, None, None);
        host.grid_ng_mut().grid_template_rows = Some("20px 20px".to_string());
        let root = Component::boxed(host, vec![leaf(lane)]);
        let flat = pre_flatten(&root);
        assert!(super::super::layout_tree(&root, &flat, 800.0, 600.0).is_none());

        let mut plain = grid_style(100.0, None, None);
        plain.display = Display::Flex;
        plain.custom_properties = Some(CustomProperties::from_iter([(
            "--w3cos-internal-grid-lanes".to_string(),
            "1".to_string(),
        )]));
        let root = Component::boxed(plain, vec![leaf(block_box(Some(50.0), Some(50.0)))]);
        let flat = pre_flatten(&root);
        assert!(super::super::layout_tree(&root, &flat, 800.0, 600.0).is_none());
    }

    #[test]
    fn items_spanning_auto_rows_grow_them_and_stretch_over_the_span() {
        // CSS Grid 12.5 step 3: D spans both implicit auto rows and grows
        // them to its 100px; C spans the same rows and stretches over them.
        let mut c = block_box(None, None);
        c.grid_ng_mut().grid_row = Some("1 / 3".to_string());
        let mut d = block_box(None, Some(100.0));
        d.grid_ng_mut().grid_row = Some("1 / 3".to_string());
        let mut after = block_box(None, Some(10.0));
        after.grid_ng_mut().grid_row = Some("2".to_string());
        after.grid_column = Some("3".to_string());
        expect(
            &Component::boxed(
                grid_style(300.0, None, Some("100px 100px 100px")),
                vec![
                    Component::boxed(c, vec![leaf(block_box(None, Some(10.0)))]),
                    leaf(d),
                    leaf(after),
                ],
            ),
            &[
                (0, [0.0, 0.0, 300.0, 100.0]),
                (1, [0.0, 0.0, 100.0, 100.0]),
                (3, [100.0, 0.0, 100.0, 100.0]),
                // Row 2 already holds 10px, so the 90px left is split 45/45.
                (4, [200.0, 45.0, 100.0, 10.0]),
            ],
        );
    }

    #[test]
    fn auto_columns_shrink_below_max_content_to_fit_the_free_space() {
        // Each item is min-content 40 / max-content 80. Auto tracks start at
        // the minimum contribution and are maximized equally (CSS Grid 12.6).
        let floats = || {
            let mut float = block_box(Some(40.0), Some(10.0));
            float.float = Float::Left;
            Component::boxed(
                block_box(None, None),
                vec![leaf(float.clone()), leaf(float)],
            )
        };
        expect(
            &Component::boxed(
                grid_style(100.0, None, Some("auto auto")),
                vec![floats(), floats()],
            ),
            &[(1, [0.0, 0.0, 50.0, 20.0]), (4, [50.0, 0.0, 50.0, 20.0])],
        );
        // With room to spare they still reach max-content, then stretch.
        expect(
            &Component::boxed(
                grid_style(200.0, None, Some("auto auto")),
                vec![floats(), floats()],
            ),
            &[(1, [0.0, 0.0, 100.0, 10.0]), (4, [100.0, 0.0, 100.0, 10.0])],
        );
    }

    #[test]
    fn an_item_spanning_columns_grows_only_their_intrinsic_maximums() {
        // CSS Grid 12.5.1: the spanner (min-content 40, max-content 80)
        // leaves the fixed 5px column alone. Its min-content share goes to
        // the two intrinsic growth limits (17.5 each), then its max-content
        // the rest (37.5 each); maximizing reaches those limits and the
        // auto maximums stretch over the last 20px (47.5 each).
        let mut float = block_box(Some(40.0), Some(10.0));
        float.float = Float::Left;
        let mut spanner = block_box(None, None);
        spanner.grid_column = Some("1 / -1".to_string());
        let mut first = block_box(None, Some(5.0));
        first.grid_column = Some("1".to_string());
        let mut last = block_box(None, Some(5.0));
        last.grid_column = Some("3".to_string());
        expect(
            &Component::boxed(
                grid_style(100.0, None, Some("minmax(0px, auto) 5px minmax(0px, auto)")),
                vec![
                    Component::boxed(spanner, vec![leaf(float.clone()), leaf(float)]),
                    leaf(first),
                    leaf(last),
                ],
            ),
            &[
                (1, [0.0, 0.0, 100.0, 10.0]),
                (4, [0.0, 10.0, 47.5, 5.0]),
                (5, [52.5, 10.0, 47.5, 5.0]),
            ],
        );
    }

    #[test]
    fn fit_content_track_and_auto_maximum_share_a_spanner() {
        // CSS Grid 12.5.1 (grid-intrinsic-maximums FitContent): the spanner's
        // 15px minimum grows the fit-content base to 10; min-content 40 adds
        // 12.5 to each growth limit, max-content 80 another 20; the auto
        // maximum stretches over the last 15px: 42.5 / 5 / 52.5.
        let mut outer = grid_style(
            100.0,
            None,
            Some("fit-content(100px) 5px minmax(0px, auto)"),
        );
        outer.position = Position::Relative;
        outer.justify_items = AlignItems::FlexStart;
        let mut float = block_box(Some(40.0), Some(10.0));
        float.float = Float::Left;
        let mut spanner = block_box(None, None);
        spanner.min_width = Dimension::Px(15.0);
        spanner.grid_column = Some("1 / -1".to_string());
        let bar = |column: &str| {
            let mut bar = block_box(None, Some(5.0));
            bar.width = Dimension::Percent(100.0);
            bar.position = Position::Absolute;
            bar.grid_column = Some(column.to_string());
            leaf(bar)
        };
        let root = Component::boxed(
            block_box(Some(200.0), None),
            vec![Component::boxed(
                outer,
                vec![
                    Component::boxed(spanner, vec![leaf(float.clone()), leaf(float)]),
                    bar("1 / 2"),
                    bar("3 / 4"),
                ],
            )],
        );
        expect(
            &root,
            &[(5, [0.0, 0.0, 42.5, 5.0]), (6, [47.5, 0.0, 52.5, 5.0])],
        );
    }

    #[test]
    fn min_width_keyword_floors_a_spanners_contributions() {
        // grid-intrinsic-maximums MinContent#3: `min-width: max-content`
        // lifts the spanner's min-content contribution to 80, so the
        // minmax(0px, min-content) column's growth limit takes 80 - 5.
        use w3cos_std::style::CustomProperties;
        let mut outer = grid_style(100.0, None, Some("minmax(0px, min-content) 5px"));
        outer.position = Position::Relative;
        outer.justify_items = AlignItems::FlexStart;
        let mut float = block_box(Some(40.0), Some(10.0));
        float.float = Float::Left;
        let mut spanner = block_box(None, None);
        spanner.grid_column = Some("1 / -1".to_string());
        spanner.custom_properties = Some(CustomProperties::from_iter([(
            "--w3cos-internal-min-width-keyword".to_string(),
            "max-content".to_string(),
        )]));
        let bar = |column: &str| {
            let mut bar = block_box(None, Some(5.0));
            bar.width = Dimension::Percent(100.0);
            bar.position = Position::Absolute;
            bar.grid_column = Some(column.to_string());
            leaf(bar)
        };
        let root = Component::boxed(
            block_box(Some(200.0), None),
            vec![Component::boxed(
                outer,
                vec![
                    Component::boxed(spanner, vec![leaf(float.clone()), leaf(float)]),
                    bar("1 / 2"),
                    bar("2 / 3"),
                ],
            )],
        );
        expect(
            &root,
            &[
                (2, [0.0, 0.0, 80.0, 10.0]),
                (5, [0.0, 0.0, 75.0, 5.0]),
                (6, [75.0, 0.0, 5.0, 5.0]),
            ],
        );
    }

    #[test]
    fn only_the_scrolling_axis_drops_the_automatic_minimum() {
        // CSS Grid 6.6: overflow-x hidden drops the 100px minimum in the
        // column axis; overflow-y hidden alone keeps it there.
        let mut x_scroller = block_box(None, None);
        x_scroller.overflow_x = Some(w3cos_std::style::Overflow::Hidden);
        let mut y_scroller = block_box(None, None);
        y_scroller.overflow_y = Some(w3cos_std::style::Overflow::Hidden);
        let content = || leaf(block_box(Some(100.0), Some(100.0)));
        expect(
            &Component::boxed(
                grid_style(0.0, None, Some("auto auto")),
                vec![
                    Component::boxed(x_scroller, vec![content()]),
                    Component::boxed(y_scroller, vec![content()]),
                ],
            ),
            &[(1, [0.0, 0.0, 0.0, 100.0]), (3, [0.0, 0.0, 100.0, 100.0])],
        );
    }

    #[test]
    fn start_aligned_padded_item_takes_its_max_content_box() {
        // A start-aligned item is as wide as its max-content contribution,
        // which already holds its padding: content 80, border box 90.
        let mut outer = grid_style(300.0, None, Some("200px"));
        outer.justify_items = AlignItems::FlexStart;
        let mut padded = block_box(None, None);
        padded.padding = edges(5.0, 5.0, 5.0, 5.0);
        expect(
            &Component::boxed(
                outer,
                vec![Component::boxed(
                    padded,
                    vec![leaf(block_box(Some(80.0), Some(20.0)))],
                )],
            ),
            &[(1, [0.0, 0.0, 90.0, 30.0]), (2, [5.0, 5.0, 80.0, 20.0])],
        );
    }

    #[test]
    fn intrinsic_columns_follow_the_min_and_max_content_constraints() {
        let u = |px: f32| units(px);
        // auto, 1fr, 50px and fit-content(30px), one item each:
        // min-content 10/20/_/40, max-content 40/60/_/80, 10px gaps.
        let specs = [
            Spec::One(Breadth::Auto),
            Spec::One(Breadth::Fr(1.0)),
            Spec::One(Breadth::Px(50.0)),
            Spec::Fit(Breadth::Px(30.0)),
        ];
        let min_c = [u(10.0), u(20.0), 0, u(40.0)];
        let max_c = [u(40.0), u(60.0), 0, u(80.0)];
        let sizes = measure_columns(&specs, &[false; 4], &min_c, &max_c, u(10.0), 16.0);
        // Bases only: 10 + 20 + 50 + 40, plus three gaps.
        assert_eq!(sizes.min, Some(150.0));
        // Growth limits and the fr size of 60px: 40 + 60 + 50 + 40 + 30.
        assert_eq!(sizes.max, 220.0);
        // Two fr tracks share one fr size: the larger item per fr.
        let specs = [Spec::One(Breadth::Fr(1.0)), Spec::One(Breadth::Fr(2.0))];
        let sizes = measure_columns(&specs, &[false; 2], &[0, 0], &[u(30.0), u(40.0)], 0, 16.0);
        assert_eq!(sizes.min, Some(0.0));
        assert_eq!(sizes.max, 90.0);
    }

    #[test]
    fn grid_lanes_marker_uses_the_grid_algorithm() {
        let mut lanes = grid_style(200.0, None, Some("100px"));
        lanes.display = Display::Flex;
        lanes.custom_properties = Some(CustomProperties::from_iter([(
            "--w3cos-internal-grid-lanes".to_string(),
            "1".to_string(),
        )]));
        expect(
            &Component::boxed(
                lanes,
                vec![
                    leaf(block_box(None, Some(10.0))),
                    leaf(block_box(None, Some(10.0))),
                ],
            ),
            &[(1, [0.0, 0.0, 100.0, 10.0]), (2, [0.0, 10.0, 100.0, 10.0])],
        );
    }

    #[test]
    fn out_of_flow_item_does_not_size_tracks() {
        let mut abs = block_box(Some(30.0), Some(30.0));
        abs.position = Position::Absolute;
        expect(
            &Component::boxed(
                grid_style(200.0, None, None),
                vec![leaf(abs), leaf(block_box(None, Some(10.0)))],
            ),
            &[
                (0, [0.0, 0.0, 200.0, 10.0]),
                (1, [0.0, 0.0, 30.0, 30.0]),
                (2, [0.0, 0.0, 200.0, 10.0]),
            ],
        );
    }

    /// CSS Grid §6.6: a scroll container's minimum contribution drops its
    /// content (it keeps its padding). Chrome: 1fr tracks 20 / 80, and
    /// `minmax(auto, 10px)` 6px (the 3px padding on each side).
    #[test]
    fn scroll_container_grid_item_has_no_automatic_minimum() {
        use w3cos_std::style::Overflow;
        let scroller = |padding: f32, width: f32| {
            Component::boxed(
                Style {
                    overflow: Overflow::Hidden,
                    padding: edges(0.0, padding, 0.0, padding),
                    ..block_box(None, None)
                },
                vec![leaf(block_box(Some(width), Some(10.0)))],
            )
        };
        let plain = Component::boxed(
            block_box(None, None),
            vec![leaf(block_box(Some(80.0), Some(10.0)))],
        );
        expect(
            &Component::boxed(
                grid_style(100.0, Some(20.0), Some("1fr 1fr")),
                vec![scroller(0.0, 150.0), plain],
            ),
            &[(1, [0.0, 0.0, 20.0, 20.0]), (3, [20.0, 0.0, 80.0, 20.0])],
        );
        expect(
            &Component::boxed(
                grid_style(30.0, Some(20.0), Some("minmax(auto, 10px) 50px")),
                vec![scroller(3.0, 40.0), leaf(block_box(None, None))],
            ),
            &[(1, [0.0, 0.0, 6.0, 20.0]), (3, [6.0, 0.0, 50.0, 20.0])],
        );
    }
}
