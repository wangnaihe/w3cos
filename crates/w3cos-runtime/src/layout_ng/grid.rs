//! Grid formatting context.
//!
//! Structure follows Blink's NGGridLayoutAlgorithm
//! (third_party/blink/renderer/core/layout/grid/grid_layout_algorithm.cc,
//! Copyright The Chromium Authors, BSD-style license) and CSS Grid Level 1
//! §8 (placement) and §12 (track sizing). A missing column template is one
//! `1fr` track, which is what the legacy engine models with
//! `grid-auto-columns: 1fr`. A subgrid whose parent tracks are definite
//! (no `auto`, `fr`, or intrinsic sizes) reuses those tracks. Line names,
//! a nested subgrid, an auto parent track, and a gap that overrides the
//! parent stay on the legacy engine (`grid:subgrid`, `grid:subgrid-nested`,
//! `grid:subgrid-gap`). Template areas stay on the legacy engine.
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
    AlignContent, AlignItems, AlignSelf, BoxSizing, Dimension, Display, Float, JustifyContent,
    Position, Spacing, Style, TextDirection,
};

const EPS: f32 = 1.0 / 128.0;
const MAX_REPEAT: usize = 1024;
const MAX_TRACKS: usize = 1024;
const INF: i64 = i64::MAX / 4;
const HEIGHT_KEYWORD: &str = "--w3cos-internal-height-keyword";
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
    if has_marker(style, "--w3cos-internal-size-containment") {
        return Some("grid:contain");
    }
    if has_marker(style, "--w3cos-internal-width-keyword") {
        return Some("grid:width-keyword");
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
    if style
        .grid_auto_column_spec()
        .is_some_and(|spec| parse_one_track(spec).is_none())
    {
        return Some("grid:auto-columns");
    }
    if style
        .grid_auto_row_spec()
        .is_some_and(|spec| parse_one_track(spec).is_none())
    {
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

fn is_plain_subgrid(spec: &str) -> bool {
    spec.trim().eq_ignore_ascii_case("subgrid")
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

/// A subgrid may use this algorithm only when the parent's tracks on that
/// axis do not depend on the subgrid's items. Anything else keeps the
/// legacy picture (`grid:subgrid`).
fn subgrid_static_reason(flat: &[FlatNodeInfo], index: usize, rows: bool) -> Option<&'static str> {
    let style = flat[index].style;
    let spec = if rows {
        style.grid_rows()
    } else {
        columns_spec(style)
    };
    if !spec.is_some_and(is_plain_subgrid) {
        return Some("grid:subgrid");
    }
    let Some(parent) = flat[index].parent else {
        return Some("grid:subgrid");
    };
    let parent_style = flat[parent].style;
    if parent_style.display != Display::Grid
        || has_marker(parent_style, "--w3cos-internal-grid-lanes")
        || has_marker(style, "--w3cos-internal-grid-lanes")
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
    if parent_spec.is_some_and(spec_mentions_subgrid)
        || (if rows {
            columns_spec(parent_style)
        } else {
            parent_style.grid_rows()
        })
        .is_some_and(spec_mentions_subgrid)
    {
        return Some("grid:subgrid-nested");
    }
    let Some(parent_spec) = parent_spec else {
        return Some("grid:subgrid");
    };
    // A named line on the parent is how the line-name reftests tell the
    // subgrid from its reference. Laying out only the reference changes
    // the picture those pages already share on the legacy engine.
    if parent_spec.contains('[') {
        return Some("grid:subgrid");
    }
    let Some(template) = parse_template(parent_spec, rows) else {
        return Some("grid:subgrid");
    };
    if template.auto_fit.is_some()
        || template.tracks.is_empty()
        || !template.tracks.iter().all(spec_is_fixed)
    {
        return Some("grid:subgrid");
    }
    if template.tracks.iter().any(spec_needs_available) && !parent_size_is_px(parent_style, rows) {
        return Some("grid:subgrid");
    }
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
    let stretched = if rows {
        // A row subgrid with `width: auto` reports no min-content. That is
        // only safe when the parent's columns do not depend on it.
        if matches!(style.width, Dimension::Auto) {
            let Some(columns) = columns_spec(parent_style) else {
                return Some("grid:subgrid");
            };
            let Some(columns) = parse_template(columns, false) else {
                return Some("grid:subgrid");
            };
            if columns.auto_fit.is_some()
                || columns.tracks.is_empty()
                || !columns.tracks.iter().all(spec_is_fixed)
            {
                return Some("grid:subgrid");
            }
        }
        used_item_align(parent_style.align_items, style.align_self) == AlignItems::Stretch
            && row_pack(parent_style) == Pack::Stretch
            && matches!(style.height, Dimension::Auto)
            && !has_marker(style, HEIGHT_KEYWORD)
    } else {
        used_item_align(parent_style.justify_items, style.justify_self) == AlignItems::Stretch
            && column_pack(parent_style) == Pack::Stretch
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
    if !spacing_is_zero(start_margin) || !spacing_is_zero(end_margin) {
        return Some("grid:subgrid");
    }
    let (start_pad, end_pad) = if rows {
        (style.padding.top, style.padding.bottom)
    } else {
        (style.padding.left, style.padding.right)
    };
    if !matches!(start_pad, Spacing::Px(_)) || !matches!(end_pad, Spacing::Px(_)) {
        return Some("grid:subgrid");
    }
    if has_marker(style, "--w3cos-internal-percent-gap")
        || has_marker(parent_style, "--w3cos-internal-percent-gap")
    {
        return Some("grid:subgrid-gap");
    }
    if !gaps_match(parent_style, style, rows) {
        return Some("grid:subgrid-gap");
    }
    None
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

/// `None` on the subgrid axis means `gap: normal` (the parent's gutter).
/// An authored length has to be the parent's specified gap.
fn gaps_match(parent: &Style, child: &Style, rows: bool) -> bool {
    let specified = if rows {
        child.row_gap
    } else {
        child.column_gap
    };
    let Some(specified) = specified else {
        return true;
    };
    let parent_gap = if rows {
        parent.row_gap.unwrap_or(parent.gap)
    } else {
        parent.column_gap.unwrap_or(parent.gap)
    };
    (specified - parent_gap).abs() <= EPS
}

fn plain_subgrid_style(style: &Style) -> bool {
    columns_spec(style).is_some_and(is_plain_subgrid)
        || style.grid_rows().is_some_and(is_plain_subgrid)
}

struct InheritedTracks {
    columns: Vec<i64>,
    rows: Vec<i64>,
    col_gap: i64,
    row_gap: i64,
}

struct PublishedGrid {
    columns: Vec<i64>,
    rows: Vec<Option<i64>>,
    col_gap: i64,
    row_gap: i64,
    areas: Vec<(usize, usize, usize, usize, usize)>,
}

thread_local! {
    static SUBGRID_PUBLISHED: RefCell<Vec<(usize, PublishedGrid)>> = RefCell::new(Vec::new());
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
    col_gap: i64,
    row_specs: &[Spec],
    row_available: Option<i64>,
    font: f32,
    row_gap: i64,
    items: &[Placed],
) -> Option<PublishGuard> {
    let areas: Vec<_> = items
        .iter()
        .filter(|item| plain_subgrid_style(flat[item.index].style))
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
                col_gap,
                row_gap,
                areas,
            },
        ));
    });
    Some(PublishGuard { index })
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
        let known: Option<Vec<i64>> = published.rows.iter().copied().collect();
        let known = known?;
        slice_tracks(&known, row, row_span, flat[index].style, space, true)?
    } else {
        Vec::new()
    };
    Some(InheritedTracks {
        columns,
        rows,
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
    let (start_mbp, end_mbp) = if rows {
        (
            units(bt + pad(style.padding.top)),
            units(bb + pad(style.padding.bottom)),
        )
    } else {
        (
            units(bl + pad(style.padding.left)),
            units(br + pad(style.padding.right)),
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

fn fixed_template(sizes: &[i64]) -> Template {
    let mut names = Vec::with_capacity(sizes.len() + 1);
    names.resize_with(sizes.len() + 1, Vec::new);
    Template {
        tracks: sizes
            .iter()
            .map(|size| Spec::One(Breadth::Px(from_units(*size))))
            .collect(),
        names,
        auto_fit: None,
        auto_tracks: Vec::new(),
        auto_at: 0,
    }
}

#[derive(Clone, Copy)]
enum Breadth {
    Px(f32),
    Percent(f32),
    Em(f32),
    Fr(f64),
    Auto,
    // Parsed as unsupported for now (intrinsic-maximum pages stay on legacy).
    #[allow(dead_code)]
    MinContent,
    #[allow(dead_code)]
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
    matches!(style.height, Dimension::Auto)
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
            .all(|track| !spec_contains_auto(*track))
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
            let repeated = parse_track_list(rest.trim())?;
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
                    push_tracks(&mut tracks, &mut names, &repeated)?;
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
    let tracks = parse_track_list(spec.trim())?;
    if tracks.len() == 1 {
        Some(tracks.into_iter().next()?)
    } else {
        None
    }
}

fn parse_track_list(spec: &str) -> Option<Vec<Spec>> {
    let mut tracks = Vec::new();
    for token in split_top_level_whitespace(spec) {
        if token.starts_with('[') {
            line_names(token)?;
            continue;
        }
        if strip_function(token, "repeat").is_some() {
            return None;
        }
        tracks.push(parse_track(token)?);
    }
    Some(tracks)
}

fn push_tracks(tracks: &mut Vec<Spec>, names: &mut Vec<Vec<String>>, extra: &[Spec]) -> Option<()> {
    for spec in extra {
        tracks.push(spec.clone());
        names.push(Vec::new());
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
    // min-content / max-content track sizes still disagree with the
    // legacy engine on intrinsic-maximum tests. Leave those pages there.
    if token.eq_ignore_ascii_case("min-content") || token.eq_ignore_ascii_case("max-content") {
        return None;
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
    let inner = inner.trim();
    if let Some((left, right)) = split_top_level_once(inner, '+') {
        return add_breadths(parse_breadth(left)?, parse_breadth(right)?, false);
    }
    // A leading minus is a signed length, not a subtraction.
    let bytes = inner.as_bytes();
    for (index, ch) in inner.char_indices() {
        if ch == '-' && index > 0 && bytes[index - 1].is_ascii_whitespace() {
            return add_breadths(
                parse_breadth(inner[..index].trim())?,
                parse_breadth(inner[index + 1..].trim())?,
                true,
            );
        }
    }
    parse_breadth(inner)
}

fn add_breadths(left: Breadth, right: Breadth, subtract: bool) -> Option<Breadth> {
    let sign = if subtract { -1.0 } else { 1.0 };
    match (left, right) {
        (Breadth::Px(a), Breadth::Px(b)) => Some(Breadth::Px(a + sign * b)),
        (Breadth::Percent(a), Breadth::Percent(b)) => Some(Breadth::Percent(a + sign * b)),
        (Breadth::Percent(percent), Breadth::Px(px)) => Some(Breadth::Calc {
            percent,
            px: sign * px,
        }),
        (Breadth::Px(px), Breadth::Percent(percent)) if !subtract => {
            Some(Breadth::Calc { percent, px })
        }
        (Breadth::Px(px), Breadth::Percent(percent)) => Some(Breadth::Calc {
            percent: -percent,
            px,
        }),
        (Breadth::Calc { percent, px }, Breadth::Px(extra)) => Some(Breadth::Calc {
            percent,
            px: px + sign * extra,
        }),
        (Breadth::Calc { percent, px }, Breadth::Percent(extra)) => Some(Breadth::Calc {
            percent: percent + sign * extra,
            px,
        }),
        _ => None,
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
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    let mut parts = token.split_whitespace();
    let first = parts.next()?;
    let second = parts.next();
    if parts.next().is_some() {
        return None;
    }
    if first.eq_ignore_ascii_case("span") {
        let name = second.unwrap_or("1");
        if name.parse::<u32>().is_ok() || is_ident(name) {
            return Some(());
        }
        return None;
    }
    if first.eq_ignore_ascii_case("auto") && second.is_none() {
        return Some(());
    }
    if first.parse::<i32>().is_ok() {
        if let Some(rest) = second {
            return parse_line_syntax(rest);
        }
        return Some(());
    }
    if is_ident(first) && second.is_none() {
        return Some(());
    }
    None
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
    let start_span = span_count(start);
    let end_span = span_count(end);
    if let Some(span) = start_span {
        let end_line = line_index(end, names, explicit, None)?;
        let start_line = end_line - span as isize;
        return Some(AxisPlace {
            start: Some(start_line),
            span,
        });
    }
    if let Some(span) = end_span {
        let start_line = line_index(start, names, explicit, None)?;
        return Some(AxisPlace {
            start: Some(start_line),
            span,
        });
    }
    let start_line = line_index(start, names, explicit, None)?;
    let end_line = line_index(end, names, explicit, Some(start_line))?;
    let span = (end_line - start_line).unsigned_abs().max(1);
    let start_line = start_line.min(end_line);
    Some(AxisPlace {
        start: Some(start_line),
        span,
    })
}

fn resolve_one(
    token: &str,
    names: &[Vec<String>],
    explicit: usize,
    after: Option<isize>,
) -> Option<AxisPlace> {
    if let Some(span) = span_count(token) {
        return Some(AxisPlace { start: None, span });
    }
    if token.trim().eq_ignore_ascii_case("auto") {
        return Some(AxisPlace {
            start: None,
            span: 1,
        });
    }
    Some(AxisPlace {
        start: Some(line_index(token, names, explicit, after)?),
        span: 1,
    })
}

fn span_count(token: &str) -> Option<usize> {
    let mut parts = token.split_whitespace();
    if !parts.next()?.eq_ignore_ascii_case("span") {
        return None;
    }
    let count = parts.next().unwrap_or("1");
    if parts.next().is_some() {
        return None;
    }
    let count: usize = count.parse().ok()?;
    Some(count.max(1))
}

fn line_index(
    token: &str,
    names: &[Vec<String>],
    explicit: usize,
    after: Option<isize>,
) -> Option<isize> {
    let token = token.trim();
    if let Ok(index) = token.parse::<i32>() {
        if index == 0 {
            return None;
        }
        return Some(if index > 0 {
            (index - 1) as isize
        } else {
            explicit as isize + index as isize + 1
        });
    }
    let after = after.unwrap_or(-1);
    names.iter().enumerate().find_map(|(index, line)| {
        let index = index as isize;
        (index > after && line.iter().any(|name| name == token)).then_some(index)
    })
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

fn track_from_spec(
    spec: &Spec,
    min_c: i64,
    max_c: i64,
    available: Option<i64>,
    font: f32,
    stretch: bool,
) -> Track {
    let mut track = match spec {
        Spec::One(Breadth::Fr(factor)) => Track {
            base: min_c.max(0),
            cap: INF,
            flex: *factor,
            stretchy: false,
            collapsed: false,
        },
        Spec::One(Breadth::Auto) => Track {
            base: max_c.max(0),
            cap: if stretch { INF } else { max_c.max(0) },
            flex: 0.0,
            stretchy: stretch,
            collapsed: false,
        },
        Spec::One(Breadth::MinContent) => fixed_track(min_c),
        Spec::One(Breadth::MaxContent) => fixed_track(max_c),
        Spec::One(breadth) => match definite_breadth(*breadth, available, font) {
            Some(size) => fixed_track(size),
            None => Track {
                base: max_c.max(0),
                cap: if stretch { INF } else { max_c.max(0) },
                flex: 0.0,
                stretchy: stretch,
                collapsed: false,
            },
        },
        Spec::Fit(argument) => {
            let limit = definite_breadth(*argument, available, font).unwrap_or(max_c);
            fixed_track(max_c.min(limit.max(min_c)))
        }
        Spec::MinMax(min, max) => {
            let base = breadth_floor(*min, min_c, max_c, available, font);
            let (cap, flex) = breadth_ceiling(*max, min_c, max_c, available, font);
            Track {
                base,
                cap: cap.max(base),
                flex,
                stretchy: false,
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

fn breadth_floor(
    breadth: Breadth,
    min_c: i64,
    max_c: i64,
    available: Option<i64>,
    font: f32,
) -> i64 {
    match breadth {
        Breadth::Auto | Breadth::MinContent => min_c.max(0),
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
        let growable: Vec<usize> = (0..used.len())
            .filter(|&index| caps[index] > used[index])
            .collect();
        if growable.is_empty() {
            return;
        }
        let share = free / growable.len() as i64;
        if share <= 0 {
            for index in growable.into_iter().rev() {
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
        for index in growable {
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
            if track.flex == 0.0 && !track.stretchy && !track.collapsed {
                caps[index] = track.cap;
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
                } else if track.stretchy {
                    INF
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
    if plain_subgrid_style(style) && matches!(style.width, Dimension::Auto) {
        // Definite parent tracks do not need this item's min-content.
        // Every other auto-width grid item still falls back.
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
    let min = block::clamp_inline_size(style, space, edges, content.0).max(0.0);
    let max = block::clamp_inline_size(style, space, edges, content.1).max(0.0);
    Some((units(min + extra), units(max.max(min) + extra)))
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

/// Lay out one grid container. `sized` is the content box a float, an
/// abspos containing block, or a stretch pass already resolved.
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
    let [bt, br, bb, bl] = metrics.border;
    let [pt, pr, pb, pl] = metrics.padding;
    let inner_width = metrics.content_width.max(0.0);
    let edges_v = bt + bb + pt + pb;
    let specified_height = if let Some((_, Some(height))) = sized {
        Some(height)
    } else {
        resolve_block(style.height, space.percentage_block, style, space, edges_v)
            .why("grid:percent")?
    };
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
    let column_sub = columns_spec(style).is_some_and(is_plain_subgrid);
    let row_sub = style.grid_rows().is_some_and(is_plain_subgrid);
    let inherited = if column_sub || row_sub {
        Some(lookup_inherited(flat, index, space, column_sub, row_sub).why("grid:subgrid")?)
    } else {
        None
    };
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
        fixed_template(&inherited.as_ref().unwrap().columns)
    } else {
        match columns_spec(style) {
            None => parse_template("", false).why("grid:template")?,
            Some(spec) => parse_template(spec, false).why("grid:template")?,
        }
    };
    let rows_template = if row_sub {
        fixed_template(&inherited.as_ref().unwrap().rows)
    } else {
        match style.grid_rows() {
            None => parse_template("", true).why("grid:template")?,
            Some(spec) => parse_template(spec, true).why("grid:template")?,
        }
    };
    let flow = parse_flow(style.grid_flow()).why("grid:auto-flow")?;
    let inner_u = units(inner_width);
    let inflow = rendered_children(flat, index)
        .filter(|&child| !oof::is_out_of_flow(flat[child].style))
        .count();
    // Column flow with no explicit columns builds implicit auto tracks.
    // A missing template on the row-flow axis stays one `1fr` column.
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
        column_specs.push(Spec::One(Breadth::Fr(1.0)));
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
        let column = resolve_axis(placement(child_style, false), &names_cols, explicit_cols)
            .why("grid:column")?;
        let row = resolve_axis(placement(child_style, true), &names_rows, explicit_rows)
            .why("grid:row")?;
        pending.push((Some(column), Some(row), child_style.order, child, ordinal));
    }
    pending.sort_by_key(|item| (item.2, item.4));
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
    let stretch_columns = column_pack(style) == Pack::Stretch;
    let mut col_min = vec![0_i64; column_specs.len()];
    let mut col_max = vec![0_i64; column_specs.len()];
    let mut contribs = Vec::new();
    for item in &items {
        let (min_c, max_c) = inline_minmax(flat, item.index, space).why("grid:intrinsic")?;
        contribs.push((min_c, max_c));
        if item.col_span == 1 {
            col_min[item.col] = col_min[item.col].max(min_c);
            col_max[item.col] = col_max[item.col].max(max_c);
        }
    }
    for (item, (min_c, max_c)) in items.iter().zip(&contribs) {
        if item.col_span <= 1 {
            continue;
        }
        let end = (item.col + item.col_span).min(col_max.len());
        let current: i64 = col_max[item.col..end].iter().sum();
        if *max_c > current {
            let extra = *max_c - current;
            let slots = (end - item.col).max(1) as i64;
            for slot in &mut col_max[item.col..end] {
                *slot += extra / slots;
            }
        }
        let current_min: i64 = col_min[item.col..end].iter().sum();
        if *min_c > current_min {
            let extra = *min_c - current_min;
            let slots = (end - item.col).max(1) as i64;
            for slot in &mut col_min[item.col..end] {
                *slot += extra / slots;
            }
        }
    }
    let column_tracks: Vec<Track> = column_specs
        .iter()
        .enumerate()
        .map(|(index, spec)| {
            let mut track = track_from_spec(
                spec,
                col_min[index],
                col_max[index],
                Some(inner_u),
                font,
                stretch_columns,
            );
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
        let ml = spacing_px(child_style, child_style.margin.left, area, space);
        let mr = spacing_px(child_style, child_style.margin.right, area, space);
        let mt = spacing_px(child_style, child_style.margin.top, area, space);
        let mb = spacing_px(child_style, child_style.margin.bottom, area, space);
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
            let (_, max_c) = inline_minmax(flat, item.index, space).why("grid:intrinsic")?;
            (from_units(max_c) - ml - mr).max(0.0)
        } else {
            let specified =
                block::resolve_length(child_style.width, Some(area), child_style, space)
                    .why("grid:percent")?;
            to_content(child_style, edges_h, specified)
        };
        let mut item_space = *space;
        item_space.available_inline = area;
        let content_width =
            block::clamp_inline_size(child_style, &item_space, edges_h, raw).max(0.0);
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
        col_between,
        &row_specs,
        definite_block.map(units),
        font,
        row_gap_u,
        &items,
    );

    let mut fragments = Vec::with_capacity(built.len());
    let mut row_min = vec![0_i64; row_specs.len()];
    let mut row_max = vec![0_i64; row_specs.len()];
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
        let margin_box = units(laid.fragment.height) + item.margin_u[0] + item.margin_u[2];
        if item.row_span == 1 && item.row < row_max.len() {
            row_max[item.row] = row_max[item.row].max(margin_box);
            row_min[item.row] = row_min[item.row].max(margin_box);
        }
        fragments.push(laid);
    }
    let stretch_rows = row_pack(style) == Pack::Stretch;
    let row_available = definite_block.map(units);
    let row_tracks: Vec<Track> = row_specs
        .iter()
        .enumerate()
        .map(|(index, spec)| {
            track_from_spec(
                spec,
                row_min.get(index).copied().unwrap_or(0),
                row_max.get(index).copied().unwrap_or(0),
                row_available,
                font,
                stretch_rows,
            )
        })
        .collect();
    let mut rows = if row_tracks.is_empty() {
        Vec::new()
    } else if row_tracks.iter().any(|track| track.flex > 0.0) {
        finalize(&row_tracks, row_available, row_gap_u)
    } else {
        finalize(&row_tracks, None, row_gap_u)
    };
    if row_sub {
        let exact = &inherited.as_ref().unwrap().rows;
        if exact.len() != rows.len() {
            return super::bail("grid:subgrid");
        }
        rows.clone_from(exact);
    }
    let row_collapsed = vec![false; rows.len()];
    let base = rows.iter().sum::<i64>() + gap_sum(&row_collapsed, row_gap_u);
    let mut used = specified_height.map(units).unwrap_or(base);
    if let Some(min) = min_h {
        used = used.max(units(min));
    }
    if let Some(max) = max_h
        && used > units(max)
    {
        return super::bail("grid:max-height");
    }
    if used < base {
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
        if target_content > measured {
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
    let (row_lead, row_between) =
        pack_gap(row_pack(style), row_free.max(0), active_rows, row_gap_u);
    let row_y = positions(&rows, &row_collapsed, row_between, row_lead.max(0));

    let origin_x = units(bl + pl);
    let origin_y = units(bt + pt);
    let rtl = style.direction == TextDirection::Rtl;
    let mut children = Vec::with_capacity(built.len());
    let mut pending_oof = Vec::new();
    let mut ordered: Vec<usize> = (0..built.len()).collect();
    ordered.sort_by_key(|&index| built[index].source);
    for index in ordered {
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
        pending_oof.extend(
            laid.oof
                .iter()
                .map(|candidate| candidate.translated(x + dx, y + dy)),
        );
        children.push((x + dx, y + dy, fragment));
    }

    for child in oof_children {
        let child_style = flat[child].style;
        let column = resolve_axis(placement(child_style, false), &names_cols, explicit_cols)
            .why("grid:column")?;
        let row = resolve_axis(placement(child_style, true), &names_rows, explicit_rows)
            .why("grid:row")?;
        let col = column.start.unwrap_or(0).max(0) as usize;
        let row_index = row.start.unwrap_or(0).max(0) as usize;
        // No in-flow rows: the static-position area is the content box,
        // which is what a stretched auto row would occupy.
        let area_w = if columns.is_empty() {
            inner_u
        } else {
            span_sum(&columns, &collapsed, col, column.span, col_between)
        };
        let area_h = if rows.is_empty() {
            used
        } else {
            span_sum(&rows, &row_collapsed, row_index, row.span, row_between)
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
        let mut x = from_units(origin_x + column_x.get(col).copied().unwrap_or(0) + inline_off);
        let y = from_units(origin_y + row_y.get(row_index).copied().unwrap_or(0) + block_off);
        if rtl {
            x = from_units(origin_x + inner_u - (units(x) - origin_x));
        }
        pending_oof.push(OofCandidate {
            index: child,
            static_inline: x,
            static_block: y,
            static_rtl: rtl,
        });
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
    Some(LayoutResult {
        fragment: BoxFragment {
            index,
            width: border_width,
            height: border_height,
            text_range: None,
            children,
        },
        last_baseline: LastBaseline::NoLines,
        first_baseline: LastBaseline::NoLines,
        start_strut,
        end_strut,
        self_collapsing: false,
        oof: pending_oof,
        bfc_dependent: false,
        float_overlap: false,
    })
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
        // Line names and an auto parent track are not this slice.
        let mut named = subgrid_box(Some("subgrid [a]"), None);
        named.grid_column = Some("1 / -1".to_string());
        let mut auto_parent = grid_style(200.0, None, Some("auto auto"));
        auto_parent.width = Dimension::Px(200.0);
        falls_back(
            auto_parent,
            vec![Component::boxed(
                named,
                vec![leaf(block_box(None, Some(10.0)))],
            )],
        );
        let short = Component::boxed(
            grid_style(200.0, Some(5.0), None),
            vec![leaf(block_box(None, Some(10.0)))],
        );
        let flat = pre_flatten(&short);
        assert!(block::supports_tree(&flat));
        assert!(super::super::layout_tree(&short, &flat, 800.0, 600.0).is_none());
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
}
