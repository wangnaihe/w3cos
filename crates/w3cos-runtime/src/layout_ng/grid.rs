//! Grid formatting context, stage S5: one column, or px and fr tracks.
//!
//! Structure follows Blink's NGGridLayoutAlgorithm
//! (third_party/blink/renderer/core/layout/grid/grid_layout_algorithm.cc,
//! Copyright The Chromium Authors, BSD-style license) and CSS Grid Level 1
//! §8 (track sizing) and §9 (placement). Items are placed row-major into
//! explicit columns. A missing template is one column stretched to the
//! content box, which is what the legacy engine models with a single
//! `1fr` auto column. `auto`, `minmax`, percentages, `subgrid`, spans and
//! non-stretch alignment stay on the legacy engine.
//!
//! Track sizes are LayoutUnits (1/64 px). Flexible tracks share the free
//! space after fixed tracks and gaps; the truncated remainder is given one
//! unit at a time to the last flexible tracks.

use super::Why;
use super::block::{self, BoxMetrics};
use super::inline::rendered_children;
use super::oof;
use super::{BoxFragment, ConstraintSpace, LastBaseline, LayoutResult, MarginStrut};
use crate::layout::{FlatNodeInfo, resolve_spacing_for_layout};
use w3cos_std::ComponentKind;
use w3cos_std::style::{
    AlignContent, AlignItems, AlignSelf, BoxSizing, Dimension, Float, JustifyContent, Position,
    Spacing, Style, TextDirection,
};

const EPS: f32 = 1.0 / 128.0;
const MAX_REPEAT: usize = 64;

#[derive(Clone, Copy)]
enum Track {
    Px(f32),
    /// Parsed as `f64` so `1.01fr` stays on the integer LayoutUnit Chrome
    /// reports. An `f32` factor floors one unit early and the leftover
    /// lands on the last track.
    Fr(f64),
}

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

fn auto_placement(value: &str) -> bool {
    let parts: Vec<&str> = value.split('/').map(str::trim).collect();
    !parts.is_empty()
        && parts.len() <= 2
        && parts
            .iter()
            .all(|part| part.eq_ignore_ascii_case("auto") || part.eq_ignore_ascii_case("span 1"))
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

/// Why this grid container is outside the S5 subset. `None` when the
/// tracks can be laid out.
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
    if style.direction == TextDirection::Rtl {
        return Some("grid:rtl");
    }
    if style.align_items != AlignItems::Stretch {
        return Some("grid:align-items");
    }
    if style.justify_items != AlignItems::Stretch {
        return Some("grid:justify-items");
    }
    if !matches!(
        style.align_content,
        AlignContent::Stretch | AlignContent::FlexStart
    ) {
        return Some("grid:align-content");
    }
    if style.justify_content != JustifyContent::FlexStart {
        return Some("grid:justify-content");
    }
    if style
        .grid_template_columns
        .as_deref()
        .is_some_and(|spec| parse_columns(spec).is_none())
    {
        return Some("grid:template");
    }
    // A definite block size stretches auto-height items into the extra
    // space. `height: max-content` is not stored, so that item looks
    // automatic and would cover a reftest that must stay on legacy.
    // Grid-lanes references are ordinary definite grids beside an
    // unrecognized `display`, and they have to use the same engine.
    let definite_block =
        !matches!(style.height, Dimension::Auto) || !matches!(style.min_height, Dimension::Auto);
    let mut auto_item = false;
    for child in rendered_children(flat, index) {
        let child_style = flat[child].style;
        if oof::is_out_of_flow(child_style) {
            return Some("grid:oof");
        }
        if child_style.float != Float::None {
            return Some("grid:float");
        }
        if matches!(flat[child].kind, ComponentKind::Text { .. }) {
            return Some("grid:text-item");
        }
        if child_style
            .grid_column
            .as_deref()
            .is_some_and(|value| !auto_placement(value))
        {
            return Some("grid:column");
        }
        if child_style.order != 0 {
            return Some("grid:order");
        }
        if !matches!(child_style.align_self, AlignSelf::Auto | AlignSelf::Stretch) {
            return Some("grid:align-self");
        }
        if !matches!(
            child_style.justify_self,
            AlignSelf::Auto | AlignSelf::Stretch
        ) {
            return Some("grid:justify-self");
        }
        if percent_block(child_style) {
            return Some("grid:percent-block");
        }
        if matches!(child_style.margin.top, Spacing::Auto)
            || matches!(child_style.margin.right, Spacing::Auto)
            || matches!(child_style.margin.bottom, Spacing::Auto)
            || matches!(child_style.margin.left, Spacing::Auto)
        {
            return Some("grid:auto-margin");
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
        if matches!(child_style.height, Dimension::Auto) {
            auto_item = true;
        }
    }
    if definite_block && auto_item {
        return Some("grid:stretch-definite");
    }
    None
}

fn parse_columns(spec: &str) -> Option<Vec<Track>> {
    let spec = spec.trim();
    if spec.is_empty() || spec.eq_ignore_ascii_case("none") {
        return Some(vec![Track::Fr(1.0_f64)]);
    }
    let tracks = parse_track_list(spec, true)?;
    if tracks.is_empty() {
        None
    } else {
        Some(tracks)
    }
}

fn parse_track_list(spec: &str, allow_repeat: bool) -> Option<Vec<Track>> {
    let mut tracks = Vec::new();
    for token in split_top_level_whitespace(spec) {
        if let Some(inner) = strip_function(token, "repeat") {
            if !allow_repeat {
                return None;
            }
            let (count, rest) = split_top_level_once(inner, ',')?;
            let count: usize = count.trim().parse().ok()?;
            if !(1..=MAX_REPEAT).contains(&count) {
                return None;
            }
            let inner_tracks = parse_track_list(rest.trim(), false)?;
            if inner_tracks.is_empty() {
                return None;
            }
            for _ in 0..count {
                tracks.extend_from_slice(&inner_tracks);
            }
        } else {
            tracks.push(parse_track(token)?);
        }
    }
    Some(tracks)
}

fn parse_track(token: &str) -> Option<Track> {
    let token = token.trim();
    if token.is_empty() || token.contains(['[', ']', '(', ')', ',', '/', '%']) {
        return None;
    }
    if let Some(number) = strip_suffix_ci(token, "fr") {
        let value: f64 = number.trim().parse().ok()?;
        if value.is_finite() && value >= 0.0 {
            return Some(Track::Fr(value));
        }
        return None;
    }
    if let Some(number) = strip_suffix_ci(token, "px") {
        let value: f32 = number.trim().parse().ok()?;
        if value.is_finite() && value >= 0.0 {
            return Some(Track::Px(value));
        }
        return None;
    }
    if token == "0" {
        return Some(Track::Px(0.0));
    }
    None
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
    for (index, ch) in value.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ => {}
        }
        if ch.is_whitespace() && depth == 0 {
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

/// Fixed tracks keep their size. Flexible tracks share whatever free space
/// remains after gaps, and never go below zero. The leftover LayoutUnit
/// goes to the last flexible tracks.
fn column_sizes(tracks: &[Track], inner: f32, gap: f32) -> Vec<i64> {
    let count = tracks.len();
    let inner_u = units(inner.max(0.0));
    let gap_u = units(gap.max(0.0));
    let gaps = gap_u * count.saturating_sub(1) as i64;
    let mut fixed = 0_i64;
    let mut fr_sum = 0.0_f64;
    for track in tracks {
        match *track {
            Track::Px(value) => fixed += units(value),
            Track::Fr(value) => fr_sum += value,
        }
    }
    let free = (inner_u - fixed - gaps).max(0);
    let mut sizes = Vec::with_capacity(count);
    let mut flexible = Vec::new();
    let mut used = 0_i64;
    for (index, track) in tracks.iter().enumerate() {
        match *track {
            Track::Px(value) => sizes.push(units(value)),
            Track::Fr(value) => {
                let raw = if fr_sum <= 0.0 {
                    0
                } else {
                    ((free as f64) * value / fr_sum).floor() as i64
                };
                used += raw;
                if value > 0.0 {
                    flexible.push(index);
                }
                sizes.push(raw);
            }
        }
    }
    if fr_sum > 0.0 {
        let mut leftover = free - used;
        for index in flexible.into_iter().rev() {
            if leftover <= 0 {
                break;
            }
            sizes[index] += 1;
            leftover -= 1;
        }
    }
    sizes
}

fn distribute_extra(sizes: &mut [i64], extra: i64) {
    if sizes.is_empty() || extra <= 0 {
        return;
    }
    let count = sizes.len() as i64;
    let share = extra / count;
    let mut remainder = extra - share * count;
    for size in sizes.iter_mut() {
        *size += share;
    }
    for size in sizes.iter_mut().rev() {
        if remainder <= 0 {
            break;
        }
        *size += 1;
        remainder -= 1;
    }
}

struct Item {
    index: usize,
    column: usize,
    row: usize,
    content_width: f32,
    /// Top, right, bottom, left margins in LayoutUnits.
    margin_u: [i64; 4],
    edges_v: f32,
    auto_block: bool,
    specified_content_h: Option<f32>,
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
    // A grid item establishes an independent formatting context, so a
    // child's margin stays inside the item (CSS Grid §6.1, CSS2 8.3.1).
    block::layout_block_sized(
        flat,
        index,
        &nested,
        true,
        Some((content_width, content_height)),
    )
    .why("grid:item")
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
    let to_content_h = |size: f32| to_content(style, edges_v, size);
    let specified_height = if let Some((_, Some(height))) = sized {
        Some(height)
    } else {
        match resolve_block(style.height, space.percentage_block, style, space, edges_v)
            .why("grid:percent")?
        {
            Some(size) => Some(size),
            None => None,
        }
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
    let tracks = match style.grid_template_columns.as_deref() {
        None => vec![Track::Fr(1.0_f64)],
        Some(spec) => parse_columns(spec).why("grid:template")?,
    };
    let column_gap = style.column_gap.unwrap_or(style.gap).max(0.0);
    let row_gap = style.row_gap.unwrap_or(style.gap).max(0.0);
    let columns = column_sizes(&tracks, inner_width, column_gap);
    let column_count = columns.len().max(1);
    let mut column_x = Vec::with_capacity(columns.len());
    let mut cursor = 0_i64;
    let column_gap_u = units(column_gap);
    for (offset, size) in columns.iter().enumerate() {
        column_x.push(cursor);
        cursor += size;
        if offset + 1 != columns.len() {
            cursor += column_gap_u;
        }
    }

    let mut items = Vec::new();
    for (ordinal, child) in rendered_children(flat, index).enumerate() {
        let child_style = flat[child].style;
        if matches!(flat[child].kind, ComponentKind::Text { .. }) {
            return super::bail("grid:text-item");
        }
        let column = ordinal % column_count;
        let area = from_units(columns.get(column).copied().unwrap_or(0));
        let mut item_space = *space;
        item_space.available_inline = area;
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
        let raw = if matches!(child_style.width, Dimension::Auto) {
            (area - ml - mr - edges_h).max(0.0)
        } else {
            let specified =
                block::resolve_length(child_style.width, Some(area), child_style, space)
                    .why("grid:percent")?;
            to_content(child_style, edges_h, specified)
        };
        let content_width =
            block::clamp_inline_size(child_style, &item_space, edges_h, raw).max(0.0);
        let specified_content_h =
            resolve_block(child_style.height, None, child_style, space, edges_v)
                .why("grid:percent")?;
        items.push(Item {
            index: child,
            column,
            row: ordinal / column_count,
            content_width,
            margin_u: [units(mt), units(mr), units(mb), units(ml)],
            edges_v,
            auto_block: specified_content_h.is_none(),
            specified_content_h,
        });
    }

    let row_count = items.last().map(|item| item.row + 1).unwrap_or(0);
    let mut row_sizes = vec![0_i64; row_count];
    let mut fragments: Vec<LayoutResult> = Vec::with_capacity(items.len());
    for item in &items {
        let laid = layout_item(
            flat,
            item.index,
            space,
            from_units(columns.get(item.column).copied().unwrap_or(0)),
            item.content_width,
            item.specified_content_h,
        )?;
        let margin_box = units(laid.fragment.height) + item.margin_u[0] + item.margin_u[2];
        row_sizes[item.row] = row_sizes[item.row].max(margin_box);
        fragments.push(laid);
    }

    let row_gap_u = units(row_gap);
    let gaps = row_gap_u * row_count.saturating_sub(1) as i64;
    let base = row_sizes.iter().sum::<i64>() + gaps;
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
    let extra = used - base;
    if extra > 0 && style.align_content == AlignContent::Stretch {
        distribute_extra(&mut row_sizes, extra);
    }

    for (item, fragment) in items.iter().zip(fragments.iter_mut()) {
        if !item.auto_block || row_sizes.is_empty() {
            continue;
        }
        let target_border = (row_sizes[item.row] - item.margin_u[0] - item.margin_u[2]).max(0);
        let target_content = (target_border - units(item.edges_v)).max(0);
        let measured = units(fragment.fragment.height) - units(item.edges_v);
        if target_content > measured {
            *fragment = layout_item(
                flat,
                item.index,
                space,
                from_units(columns.get(item.column).copied().unwrap_or(0)),
                item.content_width,
                Some(from_units(target_content)),
            )?;
        }
    }

    let mut row_y = Vec::with_capacity(row_count);
    let mut block_cursor = 0_i64;
    for (offset, size) in row_sizes.iter().enumerate() {
        row_y.push(block_cursor);
        block_cursor += size;
        if offset + 1 != row_count {
            block_cursor += row_gap_u;
        }
    }

    let origin_x = units(bl + pl);
    let origin_y = units(bt + pt);
    let mut children = Vec::with_capacity(items.len());
    let mut pending_oof = Vec::new();
    for (item, laid) in items.iter().zip(fragments) {
        let x = from_units(origin_x + column_x[item.column] + item.margin_u[3]);
        let y = from_units(origin_y + row_y[item.row] + item.margin_u[0]);
        let (dx, dy) = oof::relative_offset(flat[item.index].style, space);
        pending_oof.extend(
            laid.oof
                .iter()
                .map(|candidate| candidate.translated(x + dx, y + dy)),
        );
        children.push((x + dx, y + dy, laid.fragment));
    }

    let content_height = from_units(used.max(0));
    let mut height = content_height;
    if let Some(max) = max_h {
        height = height.min(max);
    }
    if let Some(min) = min_h {
        height = height.max(min);
    }
    // The integer track sum is the used content height. A specified height
    // that only differed by a truncated LayoutUnit stays on that sum.
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
        start_strut,
        end_strut,
        self_collapsing: false,
        oof: pending_oof,
        bfc_dependent: false,
        float_overlap: false,
    })
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

    #[test]
    fn implicit_column_stacks_stretched_items() {
        // Chrome #a, body margin 0. No template is one stretched column.
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
        // Chrome #b and #i (`repeat(2, 100px)` is the same placement).
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
                (0, [0.0, 0.0, 200.0, 20.0]),
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
        // Chrome #c. The runtime-bench sections are this shape.
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
                (0, [0.0, 0.0, 200.0, 10.0]),
                (1, [0.0, 0.0, 50.0, 10.0]),
                (2, [50.0, 0.0, 50.0, 10.0]),
                (3, [100.0, 0.0, 50.0, 10.0]),
                (4, [150.0, 0.0, 50.0, 10.0]),
            ],
        );
    }

    #[test]
    fn fr_remainder_goes_to_the_last_flexible_tracks() {
        // Chrome #d. Free space 140px = 8960/64. 1fr floors to 46.65625
        // and 2fr takes the leftover unit (93.34375). The next row starts
        // after the 10px row and the 6px row gap.
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

        // Chrome #u. Three 1fr tracks, leftover two units: 46.65625,
        // 46.671875, 46.671875, with 20px tracks between them.
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

        // Chrome #r. Gaps consume free space before the fr split.
        let mut gapped = grid_style(200.0, None, Some("1fr 1fr 1fr"));
        gapped.column_gap = Some(10.0);
        expect(
            &Component::boxed(
                gapped,
                vec![
                    leaf(block_box(None, Some(10.0))),
                    leaf(block_box(None, Some(10.0))),
                    leaf(block_box(None, Some(10.0))),
                ],
            ),
            &[
                (1, [0.0, 0.0, 60.0, 10.0]),
                (2, [70.0, 0.0, 60.0, 10.0]),
                (3, [140.0, 0.0, 60.0, 10.0]),
            ],
        );
    }

    #[test]
    fn definite_height_stretches_rows_not_specified_items() {
        // Chrome #e. Two rows of specified 10px items in an 80px grid
        // become 40px rows. The items stay 10px at the row start.
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
        // A definite grid block size with an auto-height item stays on
        // legacy (`grid:stretch-definite`). Chrome does stretch those
        // rows; the bail keeps grid-lanes references on the same engine
        // as their unrecognized test.
        let mut item = block_box(None, None);
        item.min_height = Dimension::Px(10.0);
        falls_back(
            grid_style(200.0, Some(80.0), None),
            vec![leaf(item.clone()), leaf(item)],
        );

        // Chrome #j. The auto item stretches to its 30px sibling. The
        // second column is the free space after the 8px column gap.
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
        // Chrome #f. Margin sits outside the border box. A specified width
        // stays at the start of its column. Content-box padding adds to
        // the border box.
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
        // Chrome #g without the text run (the min-height is the content
        // size). Column 100, margin 2, padding 4: border box 96 by 26.
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
        // Chrome #m. Width 200 is the content box.
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
        // Chrome #p. 50% of the 100px column. Border-box width includes
        // the 4px padding, so the border box stays 50 by 10.
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
        // Chrome #q. 80px tracks in a 100px grid do not shrink.
        expect(
            &Component::boxed(
                grid_style(100.0, None, Some("80px 80px")),
                vec![
                    leaf(block_box(None, Some(10.0))),
                    leaf(block_box(None, Some(10.0))),
                ],
            ),
            &[
                (0, [0.0, 0.0, 100.0, 10.0]),
                (1, [0.0, 0.0, 80.0, 10.0]),
                (2, [80.0, 0.0, 80.0, 10.0]),
            ],
        );
    }

    #[test]
    fn empty_grid_uses_the_specified_height() {
        // Chrome #s and #t.
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
    fn definite_height_with_an_auto_item_stays_on_legacy() {
        let mut container = grid_style(200.0, Some(80.0), Some("100px 100px"));
        container.row_gap = Some(10.0);
        let mut auto = block_box(None, None);
        auto.min_height = Dimension::Px(10.0);
        falls_back(
            container,
            vec![
                leaf(block_box(None, Some(10.0))),
                leaf(block_box(None, Some(10.0))),
                leaf(auto),
                leaf(block_box(None, Some(20.0))),
            ],
        );
    }

    #[test]
    fn unsupported_templates_and_alignment_stay_on_legacy() {
        let item = vec![leaf(block_box(Some(10.0), Some(10.0)))];
        for columns in [
            "minmax(0, 1fr)",
            "auto auto",
            "40% 60%",
            "subgrid",
            "repeat(auto-fit, 100px)",
            "repeat(128, 1fr)",
        ] {
            falls_back(grid_style(200.0, None, Some(columns)), item.clone());
        }
        let mut center = grid_style(200.0, None, Some("80px 80px"));
        center.justify_content = JustifyContent::Center;
        falls_back(center, item.clone());
        let mut start = grid_style(200.0, None, None);
        start.align_items = AlignItems::FlexStart;
        falls_back(start, item.clone());
        let mut placed = block_box(None, Some(10.0));
        placed.grid_column = Some("2".to_string());
        falls_back(
            grid_style(200.0, None, Some("100px 100px")),
            vec![leaf(placed)],
        );
        let mut rtl = grid_style(200.0, None, None);
        rtl.direction = TextDirection::Rtl;
        falls_back(rtl, item);
        let short = Component::boxed(
            grid_style(200.0, Some(5.0), None),
            vec![leaf(block_box(None, Some(10.0)))],
        );
        let flat = pre_flatten(&short);
        assert!(
            block::supports_tree(&flat),
            "a short definite height is rejected while laying out"
        );
        assert!(super::super::layout_tree(&short, &flat, 800.0, 600.0).is_none());
    }
}
