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

/// The sum of fixed (`px` / `em`) tracks plus gaps: the intrinsic grid-axis
/// size. `None` when any track is not fixed.
fn fixed_tracks_size(template: &super::Template, gap: f32, font: f32) -> Option<f32> {
    let mut total = 0.0;
    for track in &template.tracks {
        total += match track {
            Spec::One(Breadth::Px(value)) => *value,
            Spec::One(Breadth::Em(value)) => value * font,
            _ => return None,
        };
    }
    let count = template.tracks.len();
    Some(total + gap * count.saturating_sub(1) as f32)
}

/// Whether the real lanes pass lays out this container. Everything else
/// stays on the previous handling (an ordinary grid or the legacy engine).
pub(super) fn eligible(flat: &[FlatNodeInfo], index: usize) -> bool {
    let why = reason(flat, index);
    #[cfg(any(test, feature = "layout-ng-trace"))]
    if let Some(why) = why
        && why != "lanes:none"
        && super::super::trace()
    {
        eprintln!("[layout-ng] lanes-ineligible {why}");
    }
    why.is_none()
}

fn reason(flat: &[FlatNodeInfo], index: usize) -> Option<&'static str> {
    let style = flat[index].style;
    if !has_marker(style, LANES) {
        return Some("lanes:none");
    }
    let Some(Direction { columns, .. }) = lane_direction(style) else {
        return Some("lanes:direction");
    };
    if has_marker(style, super::HEIGHT_KEYWORD) {
        return Some("lanes:size-keyword");
    }
    if has_marker(style, "--w3cos-internal-overflow-alignment") {
        return Some("lanes:overflow-alignment");
    }
    if marker(style, PACK).is_some_and(|pack| pack != "normal" && pack != "dense") {
        return Some("lanes:pack");
    }
    if parse_flow(style.grid_flow()).is_none_or(|flow| flow.column) {
        return Some("lanes:flow");
    }
    if has_marker(style, "--w3cos-internal-percent-gap") {
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
    if template.auto_fit.is_some()
        && !columns
        && !matches!(style.height, Dimension::Px(_))
        && !matches!(style.min_height, Dimension::Px(_))
    {
        // The repetition count needs a definite grid-axis size.
        return Some("lanes:repeat-indefinite");
    }
    if has_marker(style, block::WIDTH_KEYWORD)
        && (!columns || block::size_contained(style) || template.auto_fit.is_some())
    {
        // The intrinsic width of column lanes follows the track sizing under
        // the min/max-content constraints; row lanes and auto-repeat stay off.
        return Some("lanes:size-keyword");
    }
    if style.grid_auto_row_spec().is_some() || style.grid_auto_column_spec().is_some() {
        return Some("lanes:auto-tracks");
    }
    // Content alignment on the stacking axis is not modelled.
    // Column lanes model it as one group offset (`stack_lead`); row lanes
    // keep `start` only.
    let stacking_aligned = if columns {
        false
    } else {
        !matches!(column_pack(style), Pack::Stretch | Pack::Start)
    };
    if stacking_aligned {
        return Some("lanes:stacking-alignment");
    }
    let content_shifted = columns && row_pack(style) != Pack::Stretch;
    match tolerance_spec(style) {
        None => return Some("lanes:tolerance"),
        // A percentage resolves against the intrinsic grid-axis size,
        // which the ordinary-grid reference does not model.
        // Row lanes take the intrinsic height from the tracks themselves
        // (`layout_rows` resolves it against the track sum), so only the
        // column axis stays off.
        Some(Tolerance::Percent(_)) if columns && matches!(style.width, Dimension::Auto) => {
            return Some("lanes:percent-tolerance");
        }
        Some(_) => {}
    }
    // Items with an explicit line plus a span (`4 / span 2`; a lone `1 / auto` is fine) over intrinsic
    // column tracks feed the track sizing in ways the ordinary-grid
    // reference does not match. Span-only placements follow main's
    // opening-based placement, as do row lanes (Chrome goldens,
    // column-explicit-placement-003, column-auto-placement-001).
    let intrinsic_axis = columns
        && template
            .tracks
            .iter()
            .chain(&template.auto_tracks)
            .any(spec_is_intrinsic);
    for child in rendered_children(flat, index) {
        let child_style = flat[child].style;
        if oof::is_out_of_flow(child_style) {
            let Direction { track_reverse, .. } = lane_direction(style)?;
            // Track reversal and rtl mirror the grid-axis lines; auto-fit
            // collapse renumbers them.
            if track_reverse
                || style.direction == TextDirection::Rtl
                || template.auto_fit == Some(true)
                || template
                    .tracks
                    .iter()
                    .chain(&template.auto_tracks)
                    .any(|spec| matches!(spec, Spec::Fit(_)))
            {
                // fit-content tracks next to auto tracks distribute spanning
                // items differently from Blink; the track edges only show
                // through out-of-flow areas.
                return Some("lanes:oof");
            }
            continue;
        }
        if template.auto_fit.is_some()
            && placement(child_style, !columns).is_some_and(|value| {
                let value = value.trim();
                !value.starts_with("span") && !value.eq_ignore_ascii_case("auto")
            })
        {
            // An explicit line may fall past the resolved repetition,
            // which needs implicit tracks this pass does not build.
            return Some("lanes:repeat-explicit");
        }
        if has_marker(child_style, "--w3cos-internal-overflow-alignment") {
            return Some("lanes:overflow-alignment");
        }
        if content_shifted && stack_align(style, child_style, columns) != StackAlign::Normal {
            // Item alignment inside the openings of a shifted group.
            return Some("lanes:stacking-alignment");
        }
        if intrinsic_axis
            && placement(child_style, !columns)
                .is_some_and(|value| value.contains('/') && value.contains("span"))
        {
            return Some("lanes:intrinsic-span");
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
    /// Dense packing: the free `[start, end)` stretches of every lane in
    /// stacking order. The last one is open ended and starts at the lane's
    /// running position (Blink `TrackOpening`).
    openings: Option<Vec<Vec<(i64, i64)>>>,
}

/// An opening path chosen by dense packing.
struct DenseSlot {
    start: usize,
    at: i64,
    path: Vec<usize>,
}

impl Running {
    fn new(lanes: usize, tolerance: i64, dense: bool) -> Self {
        Self {
            positions: vec![0; lanes],
            cursor: 0,
            tolerance,
            openings: dense.then(|| vec![vec![(0, i64::MAX)]; lanes]),
        }
    }

    /// Blink `AccumulateTrackOpeningsToAccommodateItem`: the first opening
    /// path over consecutive lanes whose overlap holds `contribution`.
    /// Returns the overlap and the opening index in each lane.
    fn opening_path(
        &self,
        contribution: i64,
        previous: (i64, i64),
        remaining: usize,
        lane: usize,
    ) -> Option<(i64, i64, Vec<usize>)> {
        let openings = self.openings.as_ref()?;
        for (i, &(start, end)) in openings[lane].iter().enumerate() {
            let overlap_start = previous.0.max(start);
            let overlap_end = previous.1.min(end);
            // Chrome 156 refills only the closed openings between items; the
            // open end of a lane is where normal placement already goes.
            if end == i64::MAX
                || overlap_start > overlap_end
                || overlap_end - overlap_start < contribution
            {
                continue;
            }
            if remaining == 0 {
                return Some((overlap_start, overlap_end, vec![i]));
            }
            if let Some((found_start, found_end, mut path)) = self.opening_path(
                contribution,
                (overlap_start, overlap_end),
                remaining - 1,
                lane + 1,
            ) {
                path.insert(0, i);
                return Some((found_start, found_end, path));
            }
        }
        None
    }

    /// Blink `GetEligibleTrackOpeningAndUpdateGridLanesItemSpan`: an
    /// earlier opening (higher up, or at the same position in an earlier
    /// lane) that fits the item, among spans of the same used size. The
    /// scan runs in flow order from the first lane.
    fn dense_find(
        &self,
        span: usize,
        contribution: i64,
        explicit: bool,
        initial: usize,
        auto_at: i64,
        size_of: &dyn Fn(usize) -> i64,
    ) -> Option<DenseSlot> {
        self.openings.as_ref()?;
        let lanes = self.positions.len();
        let used = size_of(initial);
        let mut found: Vec<(usize, i64, Vec<usize>)> = Vec::new();
        let mut lowest = i64::MAX;
        let range = if explicit {
            initial..=initial
        } else {
            0..=lanes - span
        };
        for start in range {
            if size_of(start) != used {
                continue;
            }
            let Some((at, _, path)) =
                self.opening_path(contribution, (0, i64::MAX), span - 1, start)
            else {
                continue;
            };
            lowest = lowest.min(at);
            found.push((start, at, path));
        }
        let allowed = lowest.saturating_add(self.tolerance);
        found
            .into_iter()
            .find(|(start, at, _)| {
                *at <= allowed && (*at < auto_at || (*at == auto_at && *start < initial))
            })
            .map(|(start, at, path)| DenseSlot { start, at, path })
    }

    /// Place an item in the chosen openings (Blink splits each opening
    /// into the remainders above and below the item).
    fn dense_apply(&mut self, slot: &DenseSlot, contribution: i64) {
        let Some(openings) = self.openings.as_mut() else {
            return;
        };
        let end = slot.at + contribution;
        for (offset, &index) in slot.path.iter().enumerate() {
            let lane = slot.start + offset;
            let (start, stop) = openings[lane][index];
            let above = start < slot.at;
            let below = end < stop;
            match (above, below) {
                (true, true) => {
                    openings[lane][index] = (start, slot.at);
                    openings[lane].insert(index + 1, (end, stop));
                }
                (true, false) => openings[lane][index] = (start, slot.at),
                (false, true) => openings[lane][index] = (end, stop),
                (false, false) => {
                    openings[lane].remove(index);
                }
            }
            if let Some(last) = openings[lane].last() {
                self.positions[lane] = last.0;
            }
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

    fn place(&mut self, start: usize, span: usize, at: i64, end: i64) {
        for lane in start..start + span {
            if let Some(openings) = self.openings.as_mut() {
                let last = openings[lane].last_mut().expect("open ended opening");
                if self.positions[lane] < at {
                    last.1 = at;
                    openings[lane].push((end, i64::MAX));
                } else {
                    last.0 = end;
                }
            }
            self.positions[lane] = end;
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
thread_local! {
    static AS_GRID: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether the last `layout` bailed only because the box should lay out as
/// an ordinary grid. Reading clears it.
pub(super) fn take_as_grid() -> bool {
    AS_GRID.with(|flag| flag.replace(false))
}

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
    let mut content_width = sized.map_or(metrics.content_width, |(width, _)| width);
    if let Some(keyword) = marker(style, block::WIDTH_KEYWORD).filter(|_| sized.is_none()) {
        // `min-content` / `max-content` / `fit-content` of column lanes is
        // the track sum under the matching constraint.
        let sizes = intrinsic_columns(flat, index, space)?;
        let (min, max) = (sizes.min.unwrap_or(sizes.max), sizes.max);
        let edges = metrics.border[1] + metrics.border[3] + metrics.padding[1] + metrics.padding[3];
        let available = space.available_inline - metrics.margin[1] - metrics.margin[3] - edges;
        let content = block::keyword_content_size(
            keyword,
            min,
            max,
            Some(available),
            Some(space.available_inline),
            style,
            space,
            edges,
        )
        .why("lanes:size-keyword")?;
        content_width = block::clamp_inline_size(style, space, edges, content).max(0.0);
    }
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
    let inflow = rendered_children(flat, index)
        .filter(|&child| !oof::is_out_of_flow(flat[child].style))
        .count();
    // An intrinsic repetition (and `auto-fit`) resolves its count from a
    // hypothetical track sizing (CSS Grid 3 3.1.1).
    let intrinsic_repeat = template
        .auto_tracks
        .iter()
        .any(|track| repeat_basis(track, INF / 2, font).is_none());
    let forced = if template.auto_fit == Some(true) || intrinsic_repeat {
        Some(repeat_count(
            flat,
            index,
            style,
            space,
            &template,
            columns,
            grid_available.unwrap_or(0),
            grid_gap_u,
            font,
            inflow,
            inner_width,
        )?)
    } else {
        None
    };
    let mut specs = expand_auto(
        &template,
        grid_available.unwrap_or(0),
        grid_gap_u,
        font,
        inflow,
        forced,
    )
    .why("lanes:repeat")?;
    let lanes = specs.len();
    if lanes == 0 {
        return crate::layout_ng::bail("lanes:no-template");
    }

    let mut entries = Vec::new();
    let names = super::expanded_names(&template, lanes);
    let mut oof_children = Vec::new();
    let in_flow = rendered_children(flat, index).filter(|&child| {
        let out = oof::is_out_of_flow(flat[child].style);
        if out {
            oof_children.push(child);
        }
        !out
    });
    for (ordinal, child) in in_flow.enumerate() {
        let child_style = flat[child].style;
        let place =
            resolve_axis(placement(child_style, !columns), &names, lanes).why("lanes:placement")?;
        let start = match place.start {
            Some(start) if start < 0 => return crate::layout_ng::bail("lanes:negative-line"),
            Some(start) => Some(start as usize),
            None => None,
        };
        // A repeat can resolve to fewer tracks than an auto-placed item
        // spans; the box then lays out as an ordinary grid, which adds
        // implicit tracks for the span.
        if start.is_none() && template.auto_fit.is_some() && place.span > lanes {
            AS_GRID.with(|flag| flag.set(true));
            return crate::layout_ng::bail("lanes:span-implicit");
        }
        let span = place.span;
        if span > lanes || start.is_some_and(|start| start + span > lanes) {
            return crate::layout_ng::bail("lanes:span");
        }
        entries.push(Entry {
            index: child,
            source: ordinal,
            order: child_style.order,
            start,
            span,
        });
    }
    entries.sort_by_key(|entry| (entry.order, entry.source));
    if template.auto_fit == Some(true) {
        collapse_empty_repeats(&template, &mut specs, &mut entries)?;
    }
    let lanes = specs.len();
    for entry in entries.iter_mut().filter(|entry| entry.start.is_none()) {
        entry.span = entry.span.min(lanes);
    }

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
    let (children, mut pending_oof, stacking_extent, geometry) = if columns {
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
            true,
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
            true,
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
    let rtl = style.direction == TextDirection::Rtl;
    if rtl && !pending_oof.is_empty() {
        return crate::layout_ng::bail("lanes:rtl-oof");
    }
    // The pass lays out in logical (ltr) coordinates; `direction: rtl`
    // mirrors every border box inside the content box (Blink converts
    // logical to physical at the end).
    let mut children: Vec<(f32, f32, BoxFragment)> = children
        .into_iter()
        .map(|(x, y, fragment)| {
            let x = if rtl {
                inner_width - x - fragment.width
            } else {
                x
            };
            (x + ox, y + oy, fragment)
        })
        .collect();
    for candidate in &mut pending_oof {
        *candidate = candidate.translated(ox, oy);
    }
    if !oof_children.is_empty() {
        let frame = OofFrame {
            columns,
            fill_reverse,
            origin_x,
            origin_y,
            inner_u,
            used,
            height,
            lanes,
            padding: [pt, pr, pb, pl],
            geometry: &geometry,
            names: &super::expanded_names(&template, lanes),
        };
        place_oof_children(
            flat,
            style,
            space,
            &oof_children,
            &frame,
            &mut children,
            &mut pending_oof,
        )?;
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
            vertical: false,
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
/// Placement of the lane axis (sizes and offsets per track).
#[derive(Default, Clone)]
struct Geometry {
    sizes: Vec<i64>,
    pos: Vec<i64>,
    collapsed: Vec<bool>,
    between: i64,
}

type Stacked = (Placed, Vec<OofCandidate>, i64, Geometry);

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

/// Free stacking-axis space after each item (Blink track openings): the
/// gap to the next item in the lane, or to the container end, whichever
/// lane of the span is tighter. Computed from the final positions, so it
/// covers densely packed items too.
struct Openings {
    lanes: usize,
    /// Start lane, span, margin-box start and end (gap included).
    items: Vec<(usize, usize, i64, i64)>,
}

impl Openings {
    fn new(lanes: usize) -> Self {
        Self {
            lanes,
            items: Vec::new(),
        }
    }

    fn place(&mut self, start: usize, span: usize, at: i64, end: i64) {
        self.items.push((start, span, at, end));
    }

    fn finish(&self, gap: i64, container_end: i64) -> Vec<i64> {
        let mut space = vec![i64::MAX; self.items.len()];
        for lane in 0..self.lanes {
            let mut order: Vec<usize> = (0..self.items.len())
                .filter(|&item| {
                    let (start, span, _, _) = self.items[item];
                    (start..start + span).contains(&lane)
                })
                .collect();
            order.sort_by_key(|&item| (self.items[item].2, item));
            for pair in order.windows(2) {
                let opening = (self.items[pair[1]].2 - self.items[pair[0]].3).max(0);
                space[pair[0]] = space[pair[0]].min(opening);
            }
            if let Some(&last) = order.last() {
                let opening = container_end - (self.items[last].3 - gap);
                space[last] = space[last].min(opening);
            }
        }
        space
    }
}

/// Offset of the whole stack along the stacking axis (Chrome-verified:
/// `space-between` acts as `start`, `space-around` and `space-evenly` as
/// `center`; a plain `normal` stacks from the end under `fill-reverse`).
fn stack_lead(pack: Pack, fill_reverse: bool, free: i64) -> i64 {
    if pack == Pack::Stretch && fill_reverse {
        return free;
    }
    let free = free.max(0);
    match pack {
        Pack::End => free,
        Pack::Center | Pack::SpaceAround | Pack::SpaceEvenly => free / 2,
        _ => 0,
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
    exact: bool,
) -> Option<Stacked> {
    let rtl = style.direction == TextDirection::Rtl;
    let lanes = specs.len();
    let mut contribs = Contribs::new(lanes, exact);
    // Virtual items: an auto-placed item contributes at every start line.
    for entry in entries {
        let (min_c, max_c) = inline_minmax(flat, entry.index, space).why("lanes:intrinsic")?;
        let starts = match entry.start {
            Some(start) => start..=start,
            None => 0..=lanes - entry.span,
        };
        for start in starts {
            contribs.add(start, entry.span, min_c, max_c);
        }
    }
    let stretch = column_pack(style) == Pack::Stretch;
    let collapsed = vec![false; lanes];
    let tracks = contribs.tracks(specs, Some(inner_u), font, column_gap_u, stretch);
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
    let mut running = Running::new(lanes, tolerance, is_dense(style));
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
            space.percentage_block,
        )?;
        let fragment = laid.fragment;
        let item_oof = laid.oof;
        let [mt, mr, mb, ml] = prepared.margin.map(units);
        // Under rtl the layout is logical (mirrored at the end), so the
        // inline-start margin is the physical right one.
        let (start_margin, end_margin) = if rtl { (mr, ml) } else { (ml, mr) };
        let (ml, _mr, inline_offset) = axis_offset(
            area,
            units(fragment.width),
            start_margin,
            end_margin,
            false,
            false,
            prepared.justify,
        );
        let contribution = (units(fragment.height) + mt + mb + row_gap_u).max(0);
        let (dx, dy) = oof::relative_offset(flat[entry.index].style, space);
        if rtl && dx != 0.0 {
            return crate::layout_ng::bail("lanes:rtl-relative");
        }
        let size_of = |lane: usize| {
            let track = track_of(reverse.track, lanes, lane, entry.span);
            span_sum(&sizes, &collapsed, track, entry.span, between)
        };
        let slot = running.dense_find(
            entry.span,
            contribution,
            entry.start.is_some(),
            start,
            at,
            &size_of,
        );
        let (start, at) = match slot {
            Some(slot) => {
                running.dense_apply(&slot, contribution);
                (slot.start, slot.at)
            }
            None => {
                running.place(start, entry.span, at, at + contribution);
                if entry.start.is_none() {
                    running.cursor = start + entry.span;
                }
                (start, at)
            }
        };
        let track = track_of(reverse.track, lanes, start, entry.span);
        let x = from_units(column_x[track] + ml + inline_offset);
        openings.place(start, entry.span, at, at + contribution);
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
    if aligned && (used < extent || clamped) {
        return crate::layout_ng::bail("lanes:stacking-overflow");
    }
    let spaces = openings.finish(row_gap_u, used);
    let lead = stack_lead(row_pack(style), reverse.fill, used - extent);
    for (record, room) in records.into_iter().zip(spaces) {
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
                    space.percentage_block,
                )?;
                fragment = laid.fragment;
                oof = laid.oof;
            }
        }
        let y = if reverse.fill {
            // Items stack from the end of the group: the margin box
            // mirrors; margins keep their physical sides.
            lead + extent - at - units(fragment.height) - after
        } else {
            lead + at + before
        };
        placed.push((source, cross + dx, from_units(y) + dy, fragment, oof));
    }
    placed.sort_by_key(|item| item.0);
    let geometry = Geometry {
        sizes: sizes.clone(),
        pos: column_x.clone(),
        collapsed: collapsed.clone(),
        between,
    };
    Some(unzip_placed(placed, extent, geometry))
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
    exact: bool,
) -> Option<Stacked> {
    let rtl = style.direction == TextDirection::Rtl;
    let lanes = specs.len();
    let inner = from_units(inner_u);
    // Items do not depend on the lane in the inline axis, so each one is
    // measured once and contributes at every candidate start line.
    let mut measured = Vec::with_capacity(entries.len());
    let mut contribs = Contribs::new(lanes, exact);
    for entry in entries {
        let prepared = prepare(flat, style, space, entry.index, inner, false, None)?;
        let laid = layout_item(
            flat,
            entry.index,
            space,
            inner,
            prepared.content_width,
            prepared.specified_content_h,
            space.percentage_block,
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
                    space.percentage_block,
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
            contribs.add(start, entry.span, min_block, max_block);
        }
        measured.push((prepared, laid.fragment, laid.oof));
    }
    let stretch_rows = row_pack(style) == Pack::Stretch;
    let collapsed = vec![false; lanes];
    let tracks = contribs.tracks(specs, definite_block, font, row_gap_u, stretch_rows);
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
    let mut running = Running::new(lanes, tolerance, is_dense(style));
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
            if target_content != measured_content {
                let relaid = layout_item(
                    flat,
                    entry.index,
                    space,
                    inner,
                    prepared.content_width,
                    Some(from_units(target_content)),
                    space.percentage_block,
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
        let (dx, dy) = oof::relative_offset(flat[entry.index].style, space);
        if rtl && dx != 0.0 {
            return crate::layout_ng::bail("lanes:rtl-relative");
        }
        let size_of = |lane: usize| {
            let track = track_of(reverse.track, lanes, lane, entry.span);
            span_sum(&sizes, &collapsed, track, entry.span, between)
        };
        let slot = running.dense_find(
            entry.span,
            contribution,
            entry.start.is_some(),
            start,
            at,
            &size_of,
        );
        let (start, at) = match slot {
            Some(slot) => {
                running.dense_apply(&slot, contribution);
                (slot.start, slot.at)
            }
            None => {
                running.place(start, entry.span, at, at + contribution);
                if entry.start.is_none() {
                    running.cursor = start + entry.span;
                }
                (start, at)
            }
        };
        let track = track_of(reverse.track, lanes, start, entry.span);
        let y = from_units(row_y[track] + mt + block_offset);
        openings.place(start, entry.span, at, at + contribution);
        records.push(Record {
            source: entry.source,
            index: entry.index,
            cross: y,
            at,
            before: if rtl { mr } else { ml },
            after: if rtl { ml } else { mr },
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
    if rtl && aligned {
        // Stack alignment keywords are not mirrored yet.
        return crate::layout_ng::bail("lanes:rtl-stack-align");
    }
    if aligned && !records.is_empty() && extent > inner_u {
        return crate::layout_ng::bail("lanes:stacking-overflow");
    }
    let spaces = openings.finish(column_gap_u, inner_u);
    for (record, room) in records.into_iter().zip(spaces) {
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
                    space.percentage_block,
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
    let geometry = Geometry {
        sizes: sizes.clone(),
        pos: row_y.clone(),
        collapsed: collapsed.clone(),
        between,
    };
    Some(unzip_placed(placed, used, geometry))
}

fn unzip_placed(
    placed: Vec<(usize, f32, f32, BoxFragment, Vec<OofCandidate>)>,
    extent: i64,
    geometry: Geometry,
) -> Stacked {
    let mut children = Vec::with_capacity(placed.len());
    let mut oof = Vec::new();
    for (_, x, y, fragment, item_oof) in placed {
        oof.extend(item_oof.iter().map(|candidate| candidate.translated(x, y)));
        children.push((x, y, fragment));
    }
    (children, oof, extent, geometry)
}

fn is_dense(style: &Style) -> bool {
    marker(style, PACK) == Some("dense")
        || parse_flow(style.grid_flow()).is_some_and(|flow| flow.dense)
}

fn spec_is_intrinsic(spec: &Spec) -> bool {
    let intrinsic = |breadth: &Breadth| {
        matches!(
            breadth,
            Breadth::Auto | Breadth::MinContent | Breadth::MaxContent
        )
    };
    match spec {
        Spec::One(breadth) | Spec::Fit(breadth) => intrinsic(breadth),
        Spec::MinMax(min, max) => intrinsic(min) || intrinsic(max),
    }
}

/// Block-axis (row lanes) or inline-axis (column lanes) min-content and
/// max-content contributions of an item to the grid axis.
fn grid_axis_contribution(
    flat: &[FlatNodeInfo],
    style: &Style,
    space: &ConstraintSpace,
    child: usize,
    columns: bool,
    inner: f32,
) -> Option<(i64, i64)> {
    if columns {
        return inline_minmax(flat, child, space).why("lanes:intrinsic");
    }
    let block_of = |width: Option<i64>| -> Option<i64> {
        let prepared = prepare(flat, style, space, child, inner, false, width)?;
        let laid = layout_item(
            flat,
            child,
            space,
            inner,
            prepared.content_width,
            prepared.specified_content_h,
            space.percentage_block,
        )?;
        let [mt, _, mb, _] = prepared.margin.map(units);
        Some(units(laid.fragment.height) + mt + mb)
    };
    let natural = block_of(None)?;
    if !matches!(flat[child].style.width, Dimension::Auto) {
        return Some((natural, natural));
    }
    let (min_c, max_c) = inline_minmax(flat, child, space).why("lanes:intrinsic")?;
    let min_block = block_of(Some(min_c))?;
    let max_block = block_of(Some(max_c))?.max(min_block);
    Some((min_block, max_block))
}

/// Number of repetitions of an `auto-fill` / `auto-fit` repeat. Every
/// track (including those outside the repeat) takes the size of a
/// hypothetical sizing where the repeat is expanded once, item placement is
/// ignored, and a spanning item contributes its size less the spanned gaps
/// divided by its span to every track (Chromium
/// `GetIntrinsicRepeaterTrackSizes`, which uses the growth limit).
#[allow(clippy::too_many_arguments)]
fn repeat_count(
    flat: &[FlatNodeInfo],
    index: usize,
    style: &Style,
    space: &ConstraintSpace,
    template: &super::Template,
    columns: bool,
    available: i64,
    gap: i64,
    font: f32,
    inflow: usize,
    inner: f32,
) -> Option<usize> {
    let fit = template.auto_fit?;
    let repeat_len = template.auto_tracks.len();
    // One repetition first, only to resolve the largest item span.
    let mut one: Vec<Spec> = Vec::new();
    one.extend_from_slice(&template.tracks[..template.auto_at]);
    one.extend_from_slice(&template.auto_tracks);
    one.extend_from_slice(&template.tracks[template.auto_at..]);
    let names = super::expanded_names(template, one.len());
    let mut items: Vec<(usize, i64, i64)> = Vec::new();
    let needs_items = one.iter().any(spec_is_intrinsic);
    let mut largest = 1;
    if needs_items {
        for child in rendered_children(flat, index) {
            let child_style = flat[child].style;
            if oof::is_out_of_flow(child_style) {
                continue;
            }
            let span = resolve_axis(placement(child_style, !columns), &names, one.len())
                .map_or(1, |place| place.span)
                .max(1);
            largest = largest.max(span);
            let (min_c, max_c) = grid_axis_contribution(flat, style, space, child, columns, inner)?;
            items.push((span, min_c, max_c));
        }
    }
    // Expand the repeat to cover every automatic placement of each item:
    // 2 + (largest span - 2) / tracks in the repeat, rounded down.
    let reps = if largest < 2 || repeat_len == 0 {
        1
    } else {
        (2 + (largest - 2) / repeat_len).min(super::MAX_REPEAT)
    };
    let mut specs: Vec<Spec> = Vec::new();
    specs.extend_from_slice(&template.tracks[..template.auto_at]);
    let from = specs.len();
    for _ in 0..reps {
        specs.extend_from_slice(&template.auto_tracks);
    }
    let to = specs.len();
    specs.extend_from_slice(&template.tracks[template.auto_at..]);
    let total = specs.len();
    let mut contribs = Contribs::new(total, true);
    for &(span, min_c, max_c) in &items {
        let span = span.min(total.max(1));
        for start in 0..=(total - span) {
            contribs.add(start, span, min_c, max_c);
        }
    }
    let sizes: Vec<i64> = contribs
        .tracks(&specs, Some(available), font, gap, false)
        .iter()
        .map(|track| {
            if track.cap < INF {
                track.cap
            } else {
                track.base
            }
        })
        .collect();
    // The hypothetical size of a repeat entry is the largest track with
    // that index.
    let entry_sizes: Vec<i64> = (0..repeat_len)
        .map(|entry| {
            (0..reps)
                .map(|rep| sizes[from + rep * repeat_len + entry])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let repeat_sum: i64 = entry_sizes.iter().sum();
    let outside_sum: i64 = sizes[..from].iter().sum::<i64>() + sizes[to..].iter().sum::<i64>();
    let outside_len = total - (to - from);
    let denominator = repeat_sum + repeat_len as i64 * gap;
    if repeat_len == 0 || denominator <= 0 {
        return Some(if fit { inflow.max(1) } else { 1 });
    }
    let room = available + gap - outside_sum - outside_len as i64 * gap;
    Some(((room / denominator).max(1) as usize).min(super::MAX_REPEAT))
}

/// CSS Grid 3 3.3.1: `auto-fit` collapses the repeated tracks no item is
/// expected to occupy. Tracks holding an explicitly placed item are
/// occupied, and so are the first N unoccupied tracks, N being the sum of
/// the auto-placed items' spans. A collapsed track takes no placement; it
/// is dropped (its gap goes with it).
fn collapse_empty_repeats(
    template: &super::Template,
    specs: &mut Vec<Spec>,
    entries: &mut [Entry],
) -> Option<()> {
    let lanes = specs.len();
    let added = lanes.saturating_sub(template.tracks.len());
    let (from, to) = (template.auto_at, template.auto_at + added);
    let mut occupied = vec![false; lanes];
    let mut auto_spans = 0;
    for entry in entries.iter() {
        match entry.start {
            Some(start) => {
                for slot in occupied.iter_mut().skip(start).take(entry.span) {
                    *slot = true;
                }
            }
            None => auto_spans += entry.span,
        }
    }
    for slot in occupied.iter_mut() {
        if auto_spans == 0 {
            break;
        }
        if !*slot {
            *slot = true;
            auto_spans -= 1;
        }
    }
    let kept: Vec<bool> = (0..lanes)
        .map(|lane| occupied[lane] || lane < from || lane >= to)
        .collect();
    if kept.iter().all(|keep| *keep) {
        return Some(());
    }
    let mut new_index = vec![0_usize; lanes + 1];
    for lane in 0..lanes {
        new_index[lane + 1] = new_index[lane] + usize::from(kept[lane]);
    }
    if new_index[lanes] == 0 {
        return crate::layout_ng::bail("lanes:auto-fit-empty");
    }
    for entry in entries.iter_mut() {
        if let Some(start) = entry.start {
            entry.span = new_index[(start + entry.span).min(lanes)] - new_index[start];
            entry.start = Some(new_index[start]);
        }
    }
    let mut lane = 0;
    specs.retain(|_| {
        let keep = kept[lane];
        lane += 1;
        keep
    });
    Some(())
}

/// Track-sizing contributions of the (virtual) items of a lanes axis.
/// With `exact`, spanning items size the tracks by the CSS Grid 12.5
/// spanning-item steps; otherwise they spread evenly.
struct Contribs {
    min: Vec<i64>,
    max: Vec<i64>,
    has_item: Vec<bool>,
    spanning: Vec<super::SpanItem>,
    exact: bool,
}

impl Contribs {
    fn new(lanes: usize, exact: bool) -> Self {
        Self {
            min: vec![0; lanes],
            max: vec![0; lanes],
            has_item: vec![false; lanes],
            spanning: Vec::new(),
            exact,
        }
    }

    fn add(&mut self, start: usize, span: usize, min_c: i64, max_c: i64) {
        if self.exact && span > 1 {
            if start + span <= self.min.len() {
                self.spanning.push(super::SpanItem {
                    start,
                    span,
                    minimum: min_c,
                    min_c,
                    max_c,
                });
            }
            return;
        }
        if span == 1 && start < self.has_item.len() {
            self.has_item[start] = true;
        }
        super::add_span_contrib(&mut self.min, &mut self.max, start, span, min_c, max_c);
    }

    fn tracks(
        &self,
        specs: &[Spec],
        available: Option<i64>,
        font: f32,
        gap: i64,
        stretch: bool,
    ) -> Vec<super::Track> {
        let mut tracks: Vec<super::Track> = specs
            .iter()
            .enumerate()
            .map(|(lane, spec)| {
                track_from_spec(
                    spec,
                    self.min[lane],
                    self.min[lane],
                    self.max[lane],
                    available,
                    font,
                    stretch,
                )
            })
            .collect();
        if self.exact {
            super::accommodate_spanning_items(
                &mut tracks,
                specs,
                &self.has_item,
                &self.spanning,
                available,
                font,
                gap,
            );
        }
        tracks
    }
}

/// The content-box min/max-content widths of a lanes container measured as
/// an item of another formatting context. Only column lanes are measured.
pub(super) fn measured(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
) -> Option<super::MinMax> {
    match lane_direction(flat[index].style) {
        Some(Direction { columns: true, .. }) => intrinsic_columns(flat, index, space),
        _ => crate::layout_ng::bail("grid:lanes-measure"),
    }
}

/// Min-content and max-content sizes of column lanes: the tracks sized by
/// the items' contributions (an auto-placed item contributes at every start
/// line) under a zero and an unbounded constraint.
fn intrinsic_columns(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
) -> Option<super::MinMax> {
    let style = flat[index].style;
    let template = parse_template(columns_spec(style).why("lanes:no-template")?, false)
        .why("lanes:template")?;
    if template.auto_fit.is_some() || !template.auto_tracks.is_empty() {
        return crate::layout_ng::bail("lanes:intrinsic-repeat");
    }
    let specs = template.tracks.clone();
    let lanes = specs.len();
    if lanes == 0 {
        return crate::layout_ng::bail("lanes:no-template");
    }
    let names = super::expanded_names(&template, lanes);
    let mut contribs = Contribs::new(lanes, false);
    for child in rendered_children(flat, index) {
        let child_style = flat[child].style;
        if oof::is_out_of_flow(child_style) {
            continue;
        }
        let place =
            resolve_axis(placement(child_style, false), &names, lanes).why("lanes:placement")?;
        let span = place.span;
        let starts = match place.start {
            Some(start) if start < 0 => return crate::layout_ng::bail("lanes:negative-line"),
            Some(start) => start as usize..=start as usize,
            None if span <= lanes => 0..=lanes - span,
            None => return crate::layout_ng::bail("lanes:span"),
        };
        if span > lanes || *starts.end() + span > lanes {
            return crate::layout_ng::bail("lanes:span");
        }
        let (min_c, max_c) = inline_minmax(flat, child, space).why("lanes:intrinsic")?;
        for start in starts {
            contribs.add(start, span, min_c, max_c);
        }
    }
    let gap = units(style.column_gap.unwrap_or(style.gap).max(0.0));
    Some(super::measure_columns(
        &specs,
        &vec![false; lanes],
        &contribs.min,
        &contribs.max,
        gap,
        style.font_size.max(0.0),
    ))
}

/// Layout state an out-of-flow child of a lanes container needs.
struct OofFrame<'a> {
    columns: bool,
    fill_reverse: bool,
    origin_x: i64,
    origin_y: i64,
    inner_u: i64,
    used: i64,
    height: f32,
    lanes: usize,
    /// Top, right, bottom and left padding.
    padding: [f32; 4],
    geometry: &'a Geometry,
    names: &'a [Vec<String>],
}

/// Out-of-flow children of a lanes container behave as in a grid whose
/// grid-axis tracks are the lanes and whose other axis is one area spanning
/// the content box (CSS Grid 3 abspos; Blink GridLanesLayoutAlgorithm).
/// Under `fill-reverse` the static position sits at the end of the
/// stacking axis, as the content does.
fn place_oof_children(
    flat: &[FlatNodeInfo],
    style: &Style,
    space: &ConstraintSpace,
    oof_children: &[usize],
    frame: &OofFrame,
    children: &mut Vec<(f32, f32, BoxFragment)>,
    pending_oof: &mut Vec<OofCandidate>,
) -> Option<()> {
    let OofFrame {
        columns: lane_columns,
        fill_reverse,
        origin_x,
        origin_y,
        inner_u,
        used,
        height,
        lanes,
        padding: [pt, pr, pb, pl],
        geometry,
        ..
    } = *frame;
    let none = Geometry::default();
    let (col_geom, row_geom) = if lane_columns {
        (geometry, &none)
    } else {
        (&none, geometry)
    };
    let names = frame.names;
    let empty_names: &[Vec<String>] = &[];
    let (names_cols, names_rows, explicit_cols, explicit_rows) = if lane_columns {
        (names, empty_names, lanes, 0)
    } else {
        (empty_names, names, 0, lanes)
    };
    for &child in oof_children {
        let child_style = flat[child].style;
        let padded = |a: Spacing, b: Spacing| {
            [a, b]
                .iter()
                .any(|side| matches!(side, Spacing::Px(px) if *px > 0.0))
        };
        let column = super::resolve_abspos_axis(
            placement(child_style, false),
            names_cols,
            explicit_cols,
            padded(style.padding.left, style.padding.right),
        )
        .why("lanes:oof-column")?;
        let row = super::resolve_abspos_axis(
            placement(child_style, true),
            names_rows,
            explicit_rows,
            padded(style.padding.top, style.padding.bottom),
        )
        .why("lanes:oof-row")?;
        let col = column.start.unwrap_or(0).max(0) as usize;
        let row_index = row.start.unwrap_or(0).max(0) as usize;
        let static_only = style.position == Position::Static;
        let area_w = if col_geom.sizes.is_empty() || static_only {
            inner_u
        } else {
            span_sum(
                &col_geom.sizes,
                &col_geom.collapsed,
                col,
                column.span,
                col_geom.between,
            )
        };
        let area_h = if row_geom.sizes.is_empty() || static_only {
            used
        } else {
            span_sum(
                &row_geom.sizes,
                &row_geom.collapsed,
                row_index,
                row.span,
                row_geom.between,
            )
        };
        let (col_at, row_at) = if static_only {
            (0, 0)
        } else {
            (
                col_geom.pos.get(col).copied().unwrap_or(0),
                row_geom.pos.get(row_index).copied().unwrap_or(0),
            )
        };
        let mut justify = align_pack(used_item_align(
            style.justify_items,
            child_style.justify_self,
        ));
        let mut align = align_pack(used_item_align(style.align_items, child_style.align_self));
        if fill_reverse {
            if lane_columns {
                align = Pack::End;
            } else {
                justify = Pack::End;
            }
        }
        let border_w = super::definite_border(
            child_style,
            false,
            from_units(area_w),
            from_units(used),
            space,
        );
        let border_h = super::definite_border(
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
        let candidate = OofCandidate {
            index: child,
            static_inline: from_units(origin_x + col_at + inline_off),
            static_block: from_units(origin_y + row_at + block_off),
            static_rtl: false,
        };
        if static_only {
            pending_oof.push(candidate);
            continue;
        }
        let (start_auto, end_auto) = super::abspos_auto_lines(placement(child_style, false));
        let (x0, x1) = super::abspos_area(
            &col_geom.sizes,
            &col_geom.pos,
            column.start,
            column.span,
            start_auto,
            end_auto,
            (-units(pl), inner_u + units(pr)),
        );
        let (start_auto, end_auto) = super::abspos_auto_lines(placement(child_style, true));
        let (y0, y1) = super::abspos_area(
            &row_geom.sizes,
            &row_geom.pos,
            row.start,
            row.span,
            start_auto,
            end_auto,
            (-units(pt), units(height) + units(pb)),
        );
        let (area_w, area_h) = (x1 - x0, y1 - y0);
        let border_w = super::definite_border(
            child_style,
            false,
            from_units(area_w),
            from_units(area_h),
            space,
        );
        let border_h = super::definite_border(
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
        let cb = oof::ContainingBlock {
            x: from_units(origin_x + x0),
            y: from_units(origin_y + y0),
            width: from_units(area_w),
            height: from_units(area_h),
            rtl: false,
        };
        let candidate = OofCandidate {
            static_inline: from_units(origin_x + x0 + inline_off),
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
        .why("lanes:oof-resolve")?;
        children.extend(placed);
        pending_oof.extend(rest);
    }
    Some(())
}
