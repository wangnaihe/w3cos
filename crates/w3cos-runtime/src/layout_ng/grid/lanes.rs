//! `display: grid-lanes` (CSS Grid 3 masonry layout).
//!
//! Tracks run along the grid axis; items stack along the other axis, each
//! one placed in the lane (or lane span) whose running position is
//! smallest, within `flow-tolerance`. The placement rules follow the BSD
//! licensed Chromium implementation (GridLanesRunningPositions and
//! GridLanesLayoutAlgorithm). Variants this pass does not model stay on
//! the legacy engine through [`eligible`].

use super::{
    Breadth, Dimension, EPS, FlatNodeInfo, INF, Pack, Spec, Style, align_pack, axis_offset,
    column_pack, columns_spec, expand_auto, explicit_stretch, finalize, from_units, gap_sum, grow,
    has_marker, inline_minmax, is_baseline, is_subgrid_axis, layout_item, marker, pack_gap,
    parse_axis_syntax, parse_flow, parse_template, placement, positions, repeat_basis,
    resolve_axis, resolve_block, row_pack, spacing_px, span_sum, to_content, track_from_spec,
    units, used_item_align,
};
use crate::layout_ng::block::{self, BoxMetrics};
use crate::layout_ng::inline::rendered_children;
use crate::layout_ng::oof::{self, OofCandidate};
use crate::layout_ng::{
    BoxFragment, ConstraintSpace, LastBaseline, LayoutResult, MarginStrut, Why,
};
use w3cos_std::style::{AlignItems, Position, Spacing, TextDirection};

const LANES: &str = "--w3cos-internal-grid-lanes";
const TOLERANCE: &str = "--w3cos-internal-flow-tolerance";
const DIRECTION: &str = "--w3cos-internal-grid-lanes-direction";
const PACK: &str = "--w3cos-internal-grid-lanes-pack";

/// Direction of a lanes container (`grid-lanes-direction`).
#[derive(Clone, Copy)]
struct Direction {
    /// Lanes are columns.
    columns: bool,
    /// `fill-reverse`: items stack from the end of the stacking axis.
    fill_reverse: bool,
    /// `track-reverse`: auto-placed items fill the lanes last to first.
    track_reverse: bool,
}

/// The direction of a lanes container. Blink
/// `ComputedStyle::GridLanesTrackSizingDirection`: an explicit
/// `row`/`column` wins, else the specified template picks the axis,
/// columns first.
fn lane_direction(style: &Style) -> Option<Direction> {
    let direction = marker(style, DIRECTION).unwrap_or("normal");
    let mut axis = None;
    let (mut fill_reverse, mut track_reverse) = (false, false);
    for token in direction.split_whitespace() {
        match token {
            "row" => axis = Some(false),
            "column" => axis = Some(true),
            "normal" => {}
            "fill-reverse" => fill_reverse = true,
            "track-reverse" => track_reverse = true,
            _ => return None,
        }
    }
    let specified = |spec: Option<&str>| {
        spec.is_some_and(|spec| {
            let spec = spec.trim();
            !spec.is_empty() && !spec.eq_ignore_ascii_case("none")
        })
    };
    let columns = axis.unwrap_or_else(|| {
        if specified(columns_spec(style)) {
            true
        } else {
            !specified(style.grid_rows())
        }
    });
    Some(Direction {
        columns,
        fill_reverse,
        track_reverse,
    })
}

/// The track a lane span occupies: `track-reverse` counts lanes from the
/// last track.
fn track_of(reverse: bool, lanes: usize, start: usize, span: usize) -> usize {
    if reverse { lanes - start - span } else { start }
}

/// Whether the real lanes pass lays out this container. Everything else
/// stays on the previous handling (an ordinary grid or the legacy engine).
pub(super) fn eligible(flat: &[FlatNodeInfo], index: usize) -> bool {
    reason(flat, index).is_none()
}

fn reason(flat: &[FlatNodeInfo], index: usize) -> Option<&'static str> {
    let style = flat[index].style;
    if !has_marker(style, LANES) {
        return Some("lanes:none");
    }
    let Some(Direction { columns, .. }) = lane_direction(style) else {
        return Some("lanes:direction");
    };
    if has_marker(style, block::WIDTH_KEYWORD) || has_marker(style, super::HEIGHT_KEYWORD) {
        return Some("lanes:size-keyword");
    }
    if marker(style, PACK).is_some_and(|pack| pack != "normal" && pack != "dense") {
        return Some("lanes:pack");
    }
    if parse_flow(style.grid_flow()).is_none_or(|flow| flow.column) {
        return Some("lanes:flow");
    }
    if style.direction == TextDirection::Rtl || has_marker(style, "--w3cos-internal-percent-gap") {
        return Some("lanes:direction");
    }
    let (grid_spec, other_spec) = if columns {
        (columns_spec(style), style.grid_rows())
    } else {
        (style.grid_rows(), columns_spec(style))
    };
    if other_spec.is_some_and(|spec| {
        let spec = spec.trim();
        !spec.is_empty() && !spec.eq_ignore_ascii_case("none")
    }) {
        // Templates on the stacking axis are ignored by lanes, but keep
        // the probing on the safe side.
        return Some("lanes:stacking-template");
    }
    let Some(spec) = grid_spec else {
        return Some("lanes:no-template");
    };
    if is_subgrid_axis(spec) {
        return Some("lanes:subgrid");
    }
    let Some(template) = parse_template(spec, !columns) else {
        return Some("lanes:template");
    };
    if template.tracks.is_empty() && template.auto_fit.is_none() {
        return Some("lanes:no-template");
    }
    if template.auto_fit == Some(true) {
        return Some("lanes:auto-fit");
    }
    if template.auto_fit.is_some()
        && template
            .auto_tracks
            .iter()
            .any(|track| repeat_basis(track, INF / 2, style.font_size).is_none())
    {
        return Some("lanes:intrinsic-repeat");
    }
    if style.grid_auto_row_spec().is_some() || style.grid_auto_column_spec().is_some() {
        return Some("lanes:auto-tracks");
    }
    // Content alignment on the stacking axis is not modelled.
    let stacking_aligned = if columns {
        row_pack(style) != Pack::Stretch
    } else {
        !matches!(column_pack(style), Pack::Stretch | Pack::Start)
    };
    if stacking_aligned {
        return Some("lanes:stacking-alignment");
    }
    if tolerance_spec(style).is_none() {
        return Some("lanes:tolerance");
    }
    for child in rendered_children(flat, index) {
        let child_style = flat[child].style;
        if oof::is_out_of_flow(child_style) {
            return Some("lanes:oof");
        }
        if block::is_replaced_item(flat, child)
            || columns_spec(child_style).is_some_and(is_subgrid_axis)
            || child_style.grid_rows().is_some_and(is_subgrid_axis)
        {
            return Some("lanes:item");
        }
        if is_baseline(used_item_align(style.align_items, child_style.align_self))
            || is_baseline(used_item_align(
                style.justify_items,
                child_style.justify_self,
            ))
        {
            return Some("lanes:baseline");
        }
        let margin = child_style.margin;
        if [margin.top, margin.right, margin.bottom, margin.left]
            .iter()
            .any(|side| matches!(side, Spacing::Auto | Spacing::Percent(_)))
        {
            return Some("lanes:margin");
        }
        if is_dense(style)
            && placement(child_style, !columns)
                .is_some_and(|value| value.contains("span") || value.contains('/'))
        {
            return Some("lanes:dense-span");
        }
        for row in [false, true] {
            if placement(child_style, row).is_some_and(|value| parse_axis_syntax(value).is_none()) {
                return Some("lanes:placement");
            }
        }
    }
    None
}

/// `flow-tolerance`, resolved later against the grid-axis size.
#[derive(Clone, Copy)]
enum Tolerance {
    Normal,
    Infinite,
    Px(f32),
    Em(f32),
    Percent(f32),
}

fn tolerance_spec(style: &Style) -> Option<Tolerance> {
    let Some(value) = marker(style, TOLERANCE) else {
        return Some(Tolerance::Normal);
    };
    let value = value.trim();
    match value {
        "normal" => return Some(Tolerance::Normal),
        "infinite" => return Some(Tolerance::Infinite),
        "0" => return Some(Tolerance::Px(0.0)),
        _ => {}
    }
    let number = |suffix: &str| {
        value
            .strip_suffix(suffix)
            .and_then(|digits| digits.trim().parse::<f32>().ok())
            .filter(|number| number.is_finite())
    };
    if let Some(px) = number("px") {
        return Some(Tolerance::Px(px));
    }
    if let Some(em) = number("em").filter(|_| !value.ends_with("rem")) {
        return Some(Tolerance::Em(em));
    }
    number("%").map(Tolerance::Percent)
}

/// Blink `ResolveFitToleranceForGridLanes`: `normal` is the computed
/// font size in whole pixels.
fn tolerance_units(style: &Style, basis: i64) -> i64 {
    match tolerance_spec(style).unwrap_or(Tolerance::Normal) {
        Tolerance::Normal => units((style.font_size.max(0.0) + 0.5).floor()),
        Tolerance::Infinite => INF,
        Tolerance::Px(px) => units(px),
        Tolerance::Em(em) => units(style.font_size * em),
        Tolerance::Percent(percent) => ((basis.max(0) as f64) * percent as f64 / 100.0) as i64,
    }
}

struct Running {
    positions: Vec<i64>,
    cursor: usize,
    tolerance: i64,
}

impl Running {
    fn new(lanes: usize, tolerance: i64) -> Self {
        Self {
            positions: vec![0; lanes],
            cursor: 0,
            tolerance,
        }
    }

    fn span_max(&self, start: usize, span: usize) -> i64 {
        self.positions[start..start + span]
            .iter()
            .copied()
            .max()
            .unwrap_or(0)
    }

    /// Blink `GetFirstEligibleLine`: the first start line at or after the
    /// cursor (wrapping) whose span max is within the tolerance of the
    /// smallest span max.
    fn first_eligible(&self, span: usize) -> (usize, i64) {
        let last_start = self.positions.len() - span;
        let maxima: Vec<i64> = (0..=last_start)
            .map(|start| self.span_max(start, span))
            .collect();
        let smallest = maxima.iter().copied().min().unwrap_or(0);
        let allowed = smallest.saturating_add(self.tolerance);
        let first = if self.cursor > last_start {
            0
        } else {
            self.cursor
        };
        for step in 0..=last_start {
            let start = (first + step) % (last_start + 1);
            if maxima[start] <= allowed {
                return (start, maxima[start]);
            }
        }
        (0, maxima[0])
    }

    fn place(&mut self, start: usize, span: usize, end: i64) {
        for slot in &mut self.positions[start..start + span] {
            *slot = end;
        }
    }
}

struct Entry {
    index: usize,
    source: usize,
    order: i32,
    /// Grid-axis start when the item has a definite position.
    start: Option<usize>,
    span: usize,
}

/// Lay out one lanes container. `None` falls back to the legacy engine.
pub(super) fn layout(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    sized: Option<(f32, Option<f32>)>,
) -> Option<LayoutResult> {
    let style = flat[index].style;
    let Direction {
        columns,
        fill_reverse,
        track_reverse,
    } = lane_direction(style).why("lanes:direction")?;
    let metrics: BoxMetrics = block::resolve_inline(style, space);
    let content_width = sized.map_or(metrics.content_width, |(width, _)| width);
    let [bt, br, bb, bl] = metrics.border;
    let [pt, pr, pb, pl] = metrics.padding;
    let inner_width = content_width.max(0.0);
    let inner_u = units(inner_width);
    let edges_v = bt + bb + pt + pb;
    let specified_height = if let Some((_, Some(height))) = sized {
        Some(height)
    } else {
        resolve_block(style.height, space.percentage_block, style, space, edges_v)
            .why("lanes:percent")?
    };
    let min_h = match style.min_height {
        Dimension::Auto => None,
        other => resolve_block(other, space.percentage_block, style, space, edges_v)
            .why("lanes:percent")?,
    };
    let max_h = match style.max_height {
        Dimension::Auto => None,
        other => resolve_block(other, space.percentage_block, style, space, edges_v)
            .why("lanes:percent")?,
    };
    let font = style.font_size.max(0.0);
    let column_gap_u = units(style.column_gap.unwrap_or(style.gap).max(0.0));
    let row_gap_u = units(style.row_gap.unwrap_or(style.gap).max(0.0));
    let definite_block = specified_height.or(min_h);
    let (grid_gap_u, stacking_gap_u) = if columns {
        (column_gap_u, row_gap_u)
    } else {
        (row_gap_u, column_gap_u)
    };
    let spec = if columns {
        columns_spec(style)
    } else {
        style.grid_rows()
    }
    .why("lanes:no-template")?;
    let template = parse_template(spec, !columns).why("lanes:template")?;
    let grid_available = if columns {
        Some(inner_u)
    } else {
        definite_block.map(units)
    };
    if template.auto_fit.is_some() && grid_available.is_none() {
        return crate::layout_ng::bail("lanes:repeat-indefinite");
    }
    let inflow = rendered_children(flat, index).count();
    let specs = expand_auto(
        &template,
        grid_available.unwrap_or(0),
        grid_gap_u,
        font,
        inflow,
    )
    .why("lanes:repeat")?;
    let lanes = specs.len();
    if lanes == 0 {
        return crate::layout_ng::bail("lanes:no-template");
    }

    let mut entries = Vec::new();
    let names = super::expanded_names(&template, lanes);
    for (ordinal, child) in rendered_children(flat, index).enumerate() {
        let child_style = flat[child].style;
        let place =
            resolve_axis(placement(child_style, !columns), &names, lanes).why("lanes:placement")?;
        let start = match place.start {
            Some(start) if start < 0 => return crate::layout_ng::bail("lanes:negative-line"),
            Some(start) => Some(start as usize),
            None => None,
        };
        if place.span > lanes || start.is_some_and(|start| start + place.span > lanes) {
            return crate::layout_ng::bail("lanes:span");
        }
        entries.push(Entry {
            index: child,
            source: ordinal,
            order: child_style.order,
            start,
            span: place.span,
        });
    }
    // Dense packing only refills openings left by spanning items, so with
    // single-lane items it places exactly like the default pack.
    if is_dense(style) && entries.iter().any(|entry| entry.span > 1) {
        return crate::layout_ng::bail("lanes:dense-span");
    }
    entries.sort_by_key(|entry| (entry.order, entry.source));

    let used_of = |extent: i64| {
        let mut used = specified_height.map(units).unwrap_or(extent.max(0));
        if let Some(min) = min_h {
            used = used.max(units(min));
        }
        if let Some(max) = max_h {
            used = used.min(units(max));
        }
        used
    };
    let reverse = Reverse {
        fill: fill_reverse,
        track: track_reverse,
    };
    let (children, mut pending_oof, stacking_extent) = if columns {
        stack_columns(
            flat,
            style,
            space,
            &specs,
            &entries,
            inner_u,
            grid_gap_u,
            stacking_gap_u,
            font,
            reverse,
            &used_of,
            min_h.is_some() || max_h.is_some(),
        )?
    } else {
        stack_rows(
            flat,
            style,
            space,
            &specs,
            &entries,
            inner_u,
            grid_gap_u,
            stacking_gap_u,
            definite_block.map(units),
            specified_height.map(units),
            font,
            reverse,
        )?
    };

    let origin_x = units(bl + pl);
    let origin_y = units(bt + pt);
    let used = used_of(stacking_extent);
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
    let (ox, oy) = (from_units(origin_x), from_units(origin_y));
    let mut children: Vec<(f32, f32, BoxFragment)> = children
        .into_iter()
        .map(|(x, y, fragment)| (x + ox, y + oy, fragment))
        .collect();
    for candidate in &mut pending_oof {
        *candidate = candidate.translated(ox, oy);
    }
    if style.position != Position::Static && !pending_oof.is_empty() {
        let cb = oof::ContainingBlock {
            x: bl,
            y: bt,
            width: border_width - bl - br,
            height: border_height - bt - bb,
            rtl: false,
        };
        let (placed, rest) = oof::resolve(
            flat,
            std::mem::take(&mut pending_oof),
            &cb,
            false,
            space.viewport_w,
            space.viewport_h,
        )
        .why("lanes:oof-resolve")?;
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
            ellipsis: false,
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

#[derive(Clone, Copy)]
struct Reverse {
    fill: bool,
    track: bool,
}

type Placed = Vec<(f32, f32, BoxFragment)>;
type Stacked = (Placed, Vec<OofCandidate>, i64);

struct Prepared {
    content_width: f32,
    margin: [f32; 4],
    edges_v: f32,
    specified_content_h: Option<f32>,
    justify: Pack,
    align: Pack,
    /// Alignment on the stacking axis (`align-items` for column lanes).
    stack: StackAlign,
}

/// Item alignment on the stacking axis. `Normal` is the unset value,
/// which differs from `Start` once another item sets an alignment.
#[derive(Clone, Copy, PartialEq)]
enum StackAlign {
    Normal,
    Start,
    Center,
    End,
    Stretch,
}

fn stack_align(style: &Style, child: &Style, columns: bool) -> StackAlign {
    let (items, own) = if columns {
        (style.align_items, child.align_self)
    } else {
        (style.justify_items, child.justify_self)
    };
    match used_item_align(items, own) {
        AlignItems::Stretch if explicit_stretch(style, own, columns) => StackAlign::Stretch,
        AlignItems::Stretch => StackAlign::Normal,
        AlignItems::FlexEnd => StackAlign::End,
        AlignItems::Center => StackAlign::Center,
        AlignItems::FlexStart | AlignItems::Baseline => StackAlign::Start,
    }
}

/// A placed item before the stacking-axis alignment pass.
struct Record {
    source: usize,
    index: usize,
    /// Grid-axis offset (columns: x; rows: y), already aligned.
    cross: f32,
    /// Stacking-axis position of the margin box start.
    at: i64,
    before: i64,
    after: i64,
    dx: f32,
    dy: f32,
    fragment: BoxFragment,
    oof: Vec<OofCandidate>,
    prepared: Prepared,
    area: i64,
}

/// Tracks the free stacking-axis space after each item (Blink track
/// openings): the gap to the next item in the lane, or to the container
/// end, whichever lane of the span is tighter.
struct Openings {
    space: Vec<i64>,
    last: Vec<Option<usize>>,
}

impl Openings {
    fn new(lanes: usize) -> Self {
        Self {
            space: Vec::new(),
            last: vec![None; lanes],
        }
    }

    fn place(&mut self, running: &Running, start: usize, span: usize, at: i64) {
        let item = self.space.len();
        self.space.push(i64::MAX);
        for lane in start..start + span {
            if let Some(previous) = self.last[lane] {
                let opening = (at - running.positions[lane]).max(0);
                self.space[previous] = self.space[previous].min(opening);
            }
            self.last[lane] = Some(item);
        }
    }

    fn finish(&mut self, running: &Running, gap: i64, end: i64) {
        for (lane, last) in self.last.iter().enumerate() {
            if let Some(item) = *last {
                let opening = end - (running.positions[lane] - gap);
                self.space[item] = self.space[item].min(opening);
            }
        }
    }
}

/// Offset along the stacking axis and whether the item stretches. In
/// `fill-reverse` the layout is mirrored afterwards, so the alignment
/// swaps `start` and `end` (Blink ApplyStackingAxisAlignment).
fn stack_shift(align: StackAlign, fill_reverse: bool, space: i64) -> (i64, bool) {
    let align = match (align, fill_reverse) {
        (StackAlign::Stretch, _) => return (0, true),
        (StackAlign::Normal | StackAlign::Start, false) => StackAlign::Start,
        (StackAlign::Normal | StackAlign::Start, true) => StackAlign::End,
        (StackAlign::End, true) => StackAlign::Start,
        (other, _) => other,
    };
    match align {
        StackAlign::End => (space, false),
        StackAlign::Center => (space / 2, false),
        _ => (0, false),
    }
}

/// Item inline and block metrics inside `area` (a track span for column
/// lanes, the container width for row lanes).
fn prepare(
    flat: &[FlatNodeInfo],
    style: &Style,
    space: &ConstraintSpace,
    child: usize,
    area: f32,
    columns: bool,
    margin_box: Option<i64>,
) -> Option<Prepared> {
    let child_style = flat[child].style;
    let justify = if columns {
        align_pack(used_item_align(
            style.justify_items,
            child_style.justify_self,
        ))
    } else {
        // Row lanes stack on the inline axis, which behaves as `start`.
        Pack::Start
    };
    let align = if columns {
        Pack::Start
    } else {
        align_pack(used_item_align(style.align_items, child_style.align_self))
    };
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
    let width_auto = matches!(child_style.width, Dimension::Auto);
    let stretch_inline = justify == Pack::Stretch && width_auto;
    let raw = if stretch_inline {
        (area - ml - mr - edges_h).max(0.0)
    } else if width_auto {
        let (min_c, max_c) = inline_minmax(flat, child, space).why("lanes:intrinsic")?;
        let available = units(area);
        let margin_box = margin_box.unwrap_or_else(|| max_c.min(available.max(min_c)));
        (from_units(margin_box) - ml - mr - edges_h).max(0.0)
    } else {
        let specified = block::resolve_length(child_style.width, Some(area), child_style, space)
            .why("lanes:percent")?;
        to_content(child_style, edges_h, specified)
    };
    let mut item_space = *space;
    item_space.available_inline = area;
    let content_width = block::clamp_inline_size(child_style, &item_space, edges_h, raw).max(0.0);
    let specified_content_h = if has_marker(child_style, super::HEIGHT_KEYWORD) {
        None
    } else {
        resolve_block(child_style.height, None, child_style, space, edges_v).why("lanes:percent")?
    };
    Some(Prepared {
        content_width,
        margin: [mt, mr, mb, ml],
        edges_v,
        specified_content_h,
        justify,
        align,
        stack: stack_align(style, child_style, columns),
    })
}

#[allow(clippy::too_many_arguments)]
fn stack_columns(
    flat: &[FlatNodeInfo],
    style: &Style,
    space: &ConstraintSpace,
    specs: &[Spec],
    entries: &[Entry],
    inner_u: i64,
    column_gap_u: i64,
    row_gap_u: i64,
    font: f32,
    reverse: Reverse,
    used_of: &dyn Fn(i64) -> i64,
    clamped: bool,
) -> Option<Stacked> {
    let lanes = specs.len();
    let mut col_min = vec![0_i64; lanes];
    let mut col_max = vec![0_i64; lanes];
    // Virtual items: an auto-placed item contributes at every start line.
    for entry in entries {
        let (min_c, max_c) = inline_minmax(flat, entry.index, space).why("lanes:intrinsic")?;
        let starts = match entry.start {
            Some(start) => start..=start,
            None => 0..=lanes - entry.span,
        };
        for start in starts {
            super::add_span_contrib(&mut col_min, &mut col_max, start, entry.span, min_c, max_c);
        }
    }
    let stretch = column_pack(style) == Pack::Stretch;
    let collapsed = vec![false; lanes];
    let tracks: Vec<_> = specs
        .iter()
        .enumerate()
        .map(|(lane, spec)| {
            track_from_spec(
                spec,
                col_min[lane],
                col_max[lane],
                Some(inner_u),
                font,
                stretch,
            )
        })
        .collect();
    let sizes = finalize(&tracks, Some(inner_u), column_gap_u);
    let free = inner_u - gap_sum(&collapsed, column_gap_u) - sizes.iter().sum::<i64>();
    let (lead, between) = pack_gap(column_pack(style), free, lanes, column_gap_u);
    let column_x = positions(&sizes, &collapsed, between, lead);
    if reverse.track
        && (lead != 0
            || between != column_gap_u
            || sizes.iter().sum::<i64>() + gap_sum(&collapsed, column_gap_u) > inner_u)
    {
        return crate::layout_ng::bail("lanes:track-reverse-overflow");
    }
    let tolerance = tolerance_units(style, inner_u);
    let mut running = Running::new(lanes, tolerance);
    let mut placed = Vec::with_capacity(entries.len());
    let mut extent = 0_i64;
    let mut records = Vec::with_capacity(entries.len());
    let mut openings = Openings::new(lanes);
    for entry in entries {
        let explicit = entry
            .start
            .map(|start| track_of(reverse.track, lanes, start, entry.span));
        let (start, at) = match explicit {
            Some(start) => (start, running.span_max(start, entry.span)),
            None => running.first_eligible(entry.span),
        };
        let track = track_of(reverse.track, lanes, start, entry.span);
        let area = span_sum(&sizes, &collapsed, track, entry.span, between);
        let prepared = prepare(
            flat,
            style,
            space,
            entry.index,
            from_units(area),
            true,
            None,
        )?;
        let laid = layout_item(
            flat,
            entry.index,
            space,
            from_units(area),
            prepared.content_width,
            prepared.specified_content_h,
        )?;
        let fragment = laid.fragment;
        let item_oof = laid.oof;
        let [mt, mr, mb, ml] = prepared.margin.map(units);
        let (ml, _mr, inline_offset) = axis_offset(
            area,
            units(fragment.width),
            ml,
            mr,
            false,
            false,
            prepared.justify,
        );
        let contribution = (units(fragment.height) + mt + mb + row_gap_u).max(0);
        let x = from_units(column_x[track] + ml + inline_offset);
        let (dx, dy) = oof::relative_offset(flat[entry.index].style, space);
        openings.place(&running, start, entry.span, at);
        running.place(start, entry.span, at + contribution);
        if entry.start.is_none() {
            running.cursor = start + entry.span;
        }
        records.push(Record {
            source: entry.source,
            index: entry.index,
            cross: x,
            at,
            before: mt,
            after: mb,
            dx,
            dy,
            fragment,
            oof: item_oof,
            prepared,
            area,
        });
    }
    for position in &running.positions {
        extent = extent.max(*position);
    }
    let extent = if entries.is_empty() {
        0
    } else {
        (extent - row_gap_u).max(0)
    };
    let used = used_of(extent);
    let aligned = records
        .iter()
        .any(|record| record.prepared.stack != StackAlign::Normal);
    if (reverse.fill && used < extent) || (aligned && (used < extent || clamped)) {
        return crate::layout_ng::bail("lanes:stacking-overflow");
    }
    openings.finish(&running, row_gap_u, used);
    for (record, room) in records.into_iter().zip(openings.space) {
        let Record {
            source,
            index,
            cross,
            at,
            before,
            after,
            dx,
            dy,
            mut fragment,
            mut oof,
            prepared,
            area,
        } = record;
        let mut at = at;
        if aligned {
            let (shift, stretch) = stack_shift(prepared.stack, reverse.fill, room);
            at += shift;
            let child = flat[index].style;
            let explicit = !matches!(child.height, Dimension::Auto)
                || has_marker(child, super::HEIGHT_KEYWORD);
            if stretch && !explicit && room > 0 {
                let target = units(fragment.height) + room - units(prepared.edges_v);
                let laid = layout_item(
                    flat,
                    index,
                    space,
                    from_units(area),
                    prepared.content_width,
                    Some(from_units(target.max(0))),
                )?;
                fragment = laid.fragment;
                oof = laid.oof;
            }
        }
        let y = if reverse.fill {
            // Items stack from the end of the container: the margin box
            // mirrors; margins keep their physical sides.
            used - at - units(fragment.height) - after
        } else {
            at + before
        };
        placed.push((source, cross + dx, from_units(y) + dy, fragment, oof));
    }
    placed.sort_by_key(|item| item.0);
    Some(unzip_placed(placed, extent))
}

#[allow(clippy::too_many_arguments)]
fn stack_rows(
    flat: &[FlatNodeInfo],
    style: &Style,
    space: &ConstraintSpace,
    specs: &[Spec],
    entries: &[Entry],
    inner_u: i64,
    row_gap_u: i64,
    column_gap_u: i64,
    definite_block: Option<i64>,
    specified_block: Option<i64>,
    font: f32,
    reverse: Reverse,
) -> Option<Stacked> {
    let lanes = specs.len();
    let inner = from_units(inner_u);
    // Items do not depend on the lane in the inline axis, so each one is
    // measured once and contributes at every candidate start line.
    let mut measured = Vec::with_capacity(entries.len());
    let mut row_min = vec![0_i64; lanes];
    let mut row_max = vec![0_i64; lanes];
    for entry in entries {
        let prepared = prepare(flat, style, space, entry.index, inner, false, None)?;
        let laid = layout_item(
            flat,
            entry.index,
            space,
            inner,
            prepared.content_width,
            prepared.specified_content_h,
        )?;
        let [mt, _, mb, _] = prepared.margin.map(units);
        let block_of = |fragment: &BoxFragment| units(fragment.height) + mt + mb;
        // The track's minimum contribution is the block size at the
        // item's min-content width, its maximum at the max-content width
        // (the inline size is indefinite while the lanes are sized).
        let (mut min_block, mut max_block) = (block_of(&laid.fragment), block_of(&laid.fragment));
        if matches!(flat[entry.index].style.width, Dimension::Auto) {
            let (min_c, max_c) = inline_minmax(flat, entry.index, space).why("lanes:intrinsic")?;
            let at = |width: i64| -> Option<i64> {
                let sized = prepare(flat, style, space, entry.index, inner, false, Some(width))?;
                let laid = layout_item(
                    flat,
                    entry.index,
                    space,
                    inner,
                    sized.content_width,
                    sized.specified_content_h,
                )?;
                Some(block_of(&laid.fragment))
            };
            min_block = at(min_c)?;
            max_block = at(max_c)?;
        }
        let (min_block, max_block) = (min_block, max_block.max(min_block));
        let starts = match entry.start {
            Some(start) => start..=start,
            None => 0..=lanes - entry.span,
        };
        for start in starts {
            super::add_span_contrib(
                &mut row_min,
                &mut row_max,
                start,
                entry.span,
                min_block,
                max_block,
            );
        }
        measured.push((prepared, laid.fragment, laid.oof));
    }
    let stretch_rows = row_pack(style) == Pack::Stretch;
    let collapsed = vec![false; lanes];
    let tracks: Vec<_> = specs
        .iter()
        .enumerate()
        .map(|(lane, spec)| {
            track_from_spec(
                spec,
                row_min[lane],
                row_max[lane],
                definite_block,
                font,
                stretch_rows,
            )
        })
        .collect();
    let mut sizes = if tracks.iter().any(|track| track.flex > 0.0) {
        finalize(&tracks, definite_block, row_gap_u)
    } else {
        finalize(&tracks, None, row_gap_u)
    };
    let base = sizes.iter().sum::<i64>() + gap_sum(&collapsed, row_gap_u);
    let used = specified_block.or(definite_block).unwrap_or(base);
    if used < base {
        return crate::layout_ng::bail("lanes:short-height");
    }
    if stretch_rows && used > base {
        let implicit = specs
            .iter()
            .all(|spec| matches!(spec, Spec::One(Breadth::Auto)));
        if implicit || tracks.iter().any(|track| track.stretchy) {
            let caps: Vec<i64> = tracks
                .iter()
                .enumerate()
                .map(|(lane, track)| {
                    if implicit || track.stretchy {
                        INF
                    } else {
                        sizes[lane]
                    }
                })
                .collect();
            grow(&mut sizes, &caps, used - base);
        }
    }
    let free = used - gap_sum(&collapsed, row_gap_u) - sizes.iter().sum::<i64>();
    let (lead, between) = pack_gap(row_pack(style), free.max(0), lanes, row_gap_u);
    let row_y = positions(&sizes, &collapsed, between, lead.max(0));
    if reverse.track && (lead != 0 || between != row_gap_u) {
        return crate::layout_ng::bail("lanes:track-reverse-overflow");
    }
    let tolerance = tolerance_units(style, used);
    let mut running = Running::new(lanes, tolerance);
    let mut placed = Vec::with_capacity(entries.len());
    let mut records = Vec::with_capacity(entries.len());
    let mut openings = Openings::new(lanes);
    for (entry, (prepared, mut fragment, mut item_oof)) in entries.iter().zip(measured) {
        let explicit = entry
            .start
            .map(|start| track_of(reverse.track, lanes, start, entry.span));
        let (start, at) = match explicit {
            Some(start) => (start, running.span_max(start, entry.span)),
            None => running.first_eligible(entry.span),
        };
        let track = track_of(reverse.track, lanes, start, entry.span);
        let [mt, mr, mb, ml] = prepared.margin.map(units);
        let lane_size = span_sum(&sizes, &collapsed, track, entry.span, between);
        let stretch_block = prepared.align == Pack::Stretch
            && prepared.specified_content_h.is_none()
            && !has_marker(flat[entry.index].style, super::HEIGHT_KEYWORD);
        if stretch_block {
            let target_border = (lane_size - mt - mb).max(0);
            let target_content = (target_border - units(prepared.edges_v)).max(0);
            let measured_content = units(fragment.height) - units(prepared.edges_v);
            if target_content > measured_content {
                let relaid = layout_item(
                    flat,
                    entry.index,
                    space,
                    inner,
                    prepared.content_width,
                    Some(from_units(target_content)),
                )?;
                fragment = relaid.fragment;
                item_oof = relaid.oof;
            }
        }
        let (mt, _, block_offset) = axis_offset(
            lane_size,
            units(fragment.height),
            mt,
            mb,
            false,
            false,
            prepared.align,
        );
        let contribution = (units(fragment.width) + ml + mr + column_gap_u).max(0);
        let y = from_units(row_y[track] + mt + block_offset);
        let (dx, dy) = oof::relative_offset(flat[entry.index].style, space);
        openings.place(&running, start, entry.span, at);
        running.place(start, entry.span, at + contribution);
        if entry.start.is_none() {
            running.cursor = start + entry.span;
        }
        records.push(Record {
            source: entry.source,
            index: entry.index,
            cross: y,
            at,
            before: ml,
            after: mr,
            dx,
            dy,
            fragment,
            oof: item_oof,
            prepared,
            area: 0,
        });
    }
    let extent = running
        .positions
        .iter()
        .map(|position| position - column_gap_u)
        .max()
        .unwrap_or(0);
    let aligned = records
        .iter()
        .any(|record| record.prepared.stack != StackAlign::Normal);
    if (reverse.fill || aligned) && !records.is_empty() && extent > inner_u {
        return crate::layout_ng::bail("lanes:stacking-overflow");
    }
    openings.finish(&running, column_gap_u, inner_u);
    for (record, room) in records.into_iter().zip(openings.space) {
        let Record {
            source,
            index,
            cross,
            at,
            before,
            after,
            dx,
            dy,
            mut fragment,
            mut oof,
            prepared,
            ..
        } = record;
        let mut at = at;
        if aligned {
            let (shift, stretch) = stack_shift(prepared.stack, reverse.fill, room);
            at += shift;
            let child = flat[index].style;
            let explicit =
                !matches!(child.width, Dimension::Auto) || has_marker(child, block::WIDTH_KEYWORD);
            if stretch && !explicit && room > 0 {
                let edges_h = units(fragment.width) - units(prepared.content_width);
                let width = units(fragment.width) + room - edges_h;
                let content_h = units(fragment.height) - units(prepared.edges_v);
                let laid = layout_item(
                    flat,
                    index,
                    space,
                    inner,
                    from_units(width.max(0)),
                    Some(from_units(content_h.max(0))),
                )?;
                fragment = laid.fragment;
                oof = laid.oof;
            }
        }
        let x = if reverse.fill {
            // Items stack from the inline end: the margin box mirrors.
            inner_u - at - units(fragment.width) - after
        } else {
            at + before
        };
        placed.push((source, from_units(x) + dx, cross + dy, fragment, oof));
    }
    placed.sort_by_key(|item| item.0);
    Some(unzip_placed(placed, used))
}

fn unzip_placed(
    placed: Vec<(usize, f32, f32, BoxFragment, Vec<OofCandidate>)>,
    extent: i64,
) -> Stacked {
    let mut children = Vec::with_capacity(placed.len());
    let mut oof = Vec::new();
    for (_, x, y, fragment, item_oof) in placed {
        oof.extend(item_oof.iter().map(|candidate| candidate.translated(x, y)));
        children.push((x, y, fragment));
    }
    (children, oof, extent)
}

fn is_dense(style: &Style) -> bool {
    marker(style, PACK) == Some("dense")
        || parse_flow(style.grid_flow()).is_some_and(|flow| flow.dense)
}
