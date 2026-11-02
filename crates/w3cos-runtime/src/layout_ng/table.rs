//! Table formatting context.
//!
//! Structure follows Blink's NGTableLayoutAlgorithm
//! (third_party/blink/renderer/core/layout/table/ng_table_layout_algorithm.cc,
//! Copyright The Chromium Authors, BSD-style license) and CSS 2.1 §17 / CSS
//! Tables 3: column minima and maxima, fixed and automatic distribution,
//! rowspan and colspan, baseline and the other `vertical-align` values,
//! border-spacing, collapsed grid lines, and captions. The legacy formatter
//! is not consulted. `visibility: collapse` stays on that engine.
//!
//! A table establishes a block formatting context (CSS 2.1 9.4.1). An
//! `inline-table` is an atomic inline whose shrink-to-fit width is this
//! grid. A flex or grid item uses the size that formatting context assigned.

use super::Why;
use super::block::{self, MinMax};
use super::inline::{self, VALIGN_LENGTH_MARKER, VALIGN_MARKER};
use super::oof::OofCandidate;
use super::{BoxFragment, ConstraintSpace, LastBaseline, LayoutResult, MarginStrut};
use crate::layout::{FlatNodeInfo, resolve_spacing_for_layout};
use crate::table_grid::{self, GridCell};
use w3cos_std::ComponentKind;
use w3cos_std::style::used_box_spacing;
use w3cos_std::style::{
    AlignSelf, BorderLineStyle, BoxSizing, Dimension, Display, Float, Overflow, Position, Spacing,
    Style, TextDirection, Visibility,
};

const TABLE_MARKERS: &[&str] = &[
    "--w3cos-internal-html-table-element",
    "--w3cos-internal-border-spacing-source",
    "--w3cos-internal-table-cell-ua-padding",
    "--w3cos-internal-table-row-span",
    "--w3cos-internal-table-column-span",
    "--w3cos-internal-table-column-start",
    "--w3cos-internal-anonymous-table",
    VALIGN_MARKER,
    VALIGN_LENGTH_MARKER,
];

const UA_CELL_PADDING: f32 = 1.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Valign {
    Baseline,
    Top,
    Middle,
    Bottom,
}

struct Column {
    min: f32,
    max: f32,
    /// Cell and colspan content only. A specified width raises `min` but
    /// not this, so a flex-assigned table can compress that width.
    content_min: f32,
    /// 0 when the column has no percentage.
    percent: f32,
    /// A length from a column box or a cell. Percentages are not fixed.
    fixed: bool,
    /// Used size: cell border box (separated) or content+padding (collapsed).
    size: f32,
}

struct CellLayout {
    cell: GridCell,
    fragment: BoxFragment,
    /// Unstretched border-box height, including a specified height. This is
    /// what the row must be at least as tall as.
    height: f32,
    /// Content border box before a specified height stretches it. `top` /
    /// `middle` / `bottom` align this inside the final cell.
    content_height: f32,
    /// Border box that baseline alignment may grow. A specified cell height
    /// is a row minimum (`height`) and is not added again on top of the shift.
    baseline_box: f32,
    /// Baseline from the border top. `None` when a multi-line cell has no
    /// first-line baseline and shares the row with another baseline cell.
    baseline: Option<f32>,
    /// The baseline comes from a line box, not from an empty content edge.
    from_line: bool,
    valign: Valign,
    oof: Vec<OofCandidate>,
}

struct RowLayout {
    index: usize,
    section: Option<usize>,
    height: f32,
    /// Baseline from the row top, after single-row alignment.
    baseline: f32,
    /// A cell in this row contributed a line-box baseline.
    has_line: bool,
}

pub(super) fn is_table_display(display: Display) -> bool {
    matches!(
        display,
        Display::Table
            | Display::InlineTable
            | Display::TableRow
            | Display::TableRowGroup
            | Display::TableHeaderGroup
            | Display::TableFooterGroup
            | Display::TableCell
            | Display::TableCaption
            | Display::TableColumn
            | Display::TableColumnGroup
    )
}

fn is_table_box(display: Display) -> bool {
    matches!(display, Display::Table | Display::InlineTable)
}

fn is_section(display: Display) -> bool {
    matches!(
        display,
        Display::TableRowGroup | Display::TableHeaderGroup | Display::TableFooterGroup
    )
}

fn ancestor_table(flat: &[FlatNodeInfo], index: usize) -> Option<usize> {
    let mut current = Some(index);
    while let Some(at) = current {
        if is_table_box(flat[at].style.display) {
            return Some(at);
        }
        current = flat[at].parent;
    }
    None
}

fn marker<'a>(style: &'a Style, key: &str) -> Option<&'a str> {
    style
        .custom_properties
        .as_ref()
        .and_then(|props| props.get(key))
        .map(String::as_str)
}

fn scrollish(overflow: Overflow) -> bool {
    matches!(overflow, Overflow::Scroll | Overflow::Auto)
}

/// `None` when this table box can be laid out. Rows and cells follow it.
pub(super) fn reason(flat: &[FlatNodeInfo], index: usize) -> Option<&'static str> {
    let Some(table) = ancestor_table(flat, index) else {
        return Some("table");
    };
    if let Some(reason) = block::unsupported_marker(flat[index].style, TABLE_MARKERS) {
        return Some(reason);
    }
    let style = flat[table].style;
    if !matches!(style.position, Position::Static | Position::Relative) {
        return Some("table:position");
    }
    if style.column_count.is_some() || style.column_width != Dimension::Auto {
        return Some("table:columns");
    }
    let end = flat[table].subtree_end;
    for (offset, node) in flat[table..end].iter().enumerate() {
        if node.style.display == Display::None {
            continue;
        }
        // A float blockifies an internal table box into a block child of the
        // table. That block is not a grid slot.
        if node.style.float != Float::None && node.parent == Some(table) && offset != 0 {
            return Some("table:float");
        }
        // A floated internal box is blockified and is no longer a cell. It
        // is not a grid slot unless a real cell still contains it.
        if node.style.float != Float::None && offset != 0 {
            let mut parent = node.parent;
            let mut in_cell = false;
            while let Some(at) = parent {
                if at == table {
                    break;
                }
                if flat[at].style.display == Display::TableCell {
                    in_cell = true;
                    break;
                }
                parent = flat[at].parent;
            }
            if !in_cell {
                return Some("table:float");
            }
        }
        if node.style.position == Position::Sticky {
            return Some("table:sticky");
        }
        // The table width algorithms here know percentages and lengths,
        // not their `calc()` mix (css-tables calc-percent-plus-0px-*).
        if (offset == 0
            || is_table_display(node.style.display)
                && !matches!(
                    node.style.display,
                    Display::TableColumn | Display::TableColumnGroup
                ))
            && [
                node.style.width,
                node.style.min_width,
                node.style.max_width,
                node.style.height,
                node.style.min_height,
                node.style.max_height,
            ]
            .iter()
            .any(|dim| matches!(dim, Dimension::Calc { .. }))
        {
            return Some("table:calc");
        }
        // A relatively positioned row, row group or cell moves its fragment
        // by its insets and is the containing block of its absolute
        // descendants; the grid here places them unmoved. One with auto
        // insets and no absolute descendants is unaffected.
        if offset != 0
            && node.style.position == Position::Relative
            && (![
                node.style.top,
                node.style.right,
                node.style.bottom,
                node.style.left,
            ]
            .iter()
            .all(|inset| matches!(inset, Dimension::Auto))
                || flat[table + offset + 1..node.subtree_end.max(table + offset + 1)]
                    .iter()
                    .any(|inner| super::oof::is_out_of_flow(inner.style)))
            && matches!(
                node.style.display,
                Display::TableRow
                    | Display::TableRowGroup
                    | Display::TableHeaderGroup
                    | Display::TableFooterGroup
                    | Display::TableCell
            )
        {
            return Some("table:relative-part");
        }
        // A percentage block size on a cell's child resolves against the
        // cell's final height once the table, row or cell has a definite
        // one (Chrome lays the cell out again at its stretched height).
        // Cells are laid out once here, with an indefinite basis.
        if offset != 0 && percent_height(node.style) && cell_height_definite(flat, table, node) {
            return Some("table:cell-percent-height");
        }
        // A text leaf that is itself a row, section, or column (e.g. a
        // `display: table-row` span with only text) has no cell to hold its
        // line; legacy paints it. Text cells and captions are laid out.
        if matches!(node.kind, ComponentKind::Text { .. })
            && is_table_display(node.style.display)
            && !matches!(
                node.style.display,
                Display::TableCell | Display::TableCaption
            )
        {
            return Some("table:text-track");
        }
        if node.style.visibility == Visibility::Collapse
            && matches!(
                node.style.display,
                Display::TableRow
                    | Display::TableColumn
                    | Display::TableColumnGroup
                    | Display::TableRowGroup
                    | Display::TableHeaderGroup
                    | Display::TableFooterGroup
            )
        {
            return Some("table:collapse");
        }
        if scrollish(node.style.resolved_overflow_x())
            || scrollish(node.style.resolved_overflow_y())
        {
            return Some("table:scroll");
        }
    }
    None
}

fn percent_height(style: &Style) -> bool {
    [style.height, style.min_height, style.max_height]
        .iter()
        .any(|dim| matches!(dim, Dimension::Percent(_) | Dimension::Calc { .. }))
}

/// Whether `node` is the in-flow child of a (possibly anonymous) cell of
/// `table` whose cell, row, row group or table has a non-auto height.
fn cell_height_definite(flat: &[FlatNodeInfo], table: usize, node: &FlatNodeInfo) -> bool {
    if node.style.float != Float::None
        || !matches!(node.style.position, Position::Static | Position::Relative)
        || matches!(
            node.style.display,
            Display::TableCell
                | Display::TableRow
                | Display::TableRowGroup
                | Display::TableHeaderGroup
                | Display::TableFooterGroup
                | Display::TableColumn
                | Display::TableColumnGroup
                | Display::TableCaption
        )
    {
        return false;
    }
    let Some(parent) = node.parent else {
        return false;
    };
    if !matches!(
        flat[parent].style.display,
        Display::TableCell | Display::TableRow
    ) {
        return false;
    }
    let mut at = Some(parent);
    while let Some(index) = at {
        if flat[index].style.height != Dimension::Auto {
            return true;
        }
        if index == table {
            break;
        }
        at = flat[index].parent;
    }
    false
}

fn snap(value: f32) -> f32 {
    used_box_spacing(value)
}

fn border_widths(style: &Style) -> [f32; 4] {
    block::border_widths(style)
}

fn spacing_px(style: &Style, spacing: Spacing, cb: f32, space: &ConstraintSpace) -> f32 {
    match spacing {
        Spacing::Auto => 0.0,
        other => resolve_spacing_for_layout(other, cb, style, space.viewport_w, space.viewport_h),
    }
}

fn length_px(
    dim: Dimension,
    style: &Style,
    basis: Option<f32>,
    space: &ConstraintSpace,
) -> Option<f32> {
    // A column's mixed `calc()` width is auto in an auto-layout table
    // (css-tables auto-layout-calc-width-001).
    if matches!(dim, Dimension::Calc { .. })
        && matches!(
            style.display,
            Display::TableColumn | Display::TableColumnGroup
        )
    {
        return None;
    }
    block::resolve_length(dim, basis, style, space)
}

fn ua_cell(style: &Style) -> bool {
    marker(style, "--w3cos-internal-table-cell-ua-padding").is_some()
}

/// Used cell padding. A definite width absorbs the UA's 1px so the outer
/// cell matches a `cellpadding=0` reference; an auto width keeps it.
fn cell_padding(style: &Style, space: &ConstraintSpace) -> [f32; 4] {
    let cb = space.available_inline;
    let mut pad = [
        spacing_px(style, style.padding.top, cb, space),
        spacing_px(style, style.padding.right, cb, space),
        spacing_px(style, style.padding.bottom, cb, space),
        spacing_px(style, style.padding.left, cb, space),
    ];
    if style.display == Display::TableCell && ua_cell(style) {
        let definite_width = length_px(style.width, style, Some(cb), space).is_some();
        if !definite_width {
            pad[1] += UA_CELL_PADDING;
            pad[3] += UA_CELL_PADDING;
        }
        // A definite height absorbs the UA block padding in both border
        // models; the collapsed model's used height then matches the
        // separated one (`height` + the authored 1px padding).
        let definite_height = length_px(style.height, style, None, space).is_some();
        if !definite_height {
            pad[0] += UA_CELL_PADDING;
            pad[2] += UA_CELL_PADDING;
        }
    }
    pad
}

fn valign_of(style: &Style) -> Valign {
    match style.align_self {
        AlignSelf::FlexStart => Valign::Top,
        AlignSelf::FlexEnd => Valign::Bottom,
        AlignSelf::Center => Valign::Middle,
        AlignSelf::Auto | AlignSelf::Baseline | AlignSelf::Stretch => Valign::Baseline,
    }
}

fn border_box_of(
    style: &Style,
    dim: Dimension,
    space: &ConstraintSpace,
    basis: Option<f32>,
) -> Option<f32> {
    let specified = length_px(dim, style, basis, space)?;
    let pad = cell_padding(style, space);
    let border = border_widths(style);
    let edges = pad[1] + pad[3] + border[1] + border[3];
    Some(match style.box_sizing {
        BoxSizing::BorderBox => specified.max(0.0),
        BoxSizing::ContentBox => (specified + edges).max(0.0),
    })
}

fn border_box_size(style: &Style, space: &ConstraintSpace, basis: Option<f32>) -> Option<f32> {
    border_box_of(style, style.width, space, basis)
}

fn border_box_block(style: &Style, dim: Dimension, space: &ConstraintSpace) -> Option<f32> {
    let specified = length_px(dim, style, space.percentage_block, space)?;
    let pad_t = spacing_px(style, style.padding.top, space.available_inline, space);
    let pad_b = spacing_px(style, style.padding.bottom, space.available_inline, space);
    let border = border_widths(style);
    let edges = pad_t + pad_b + border[0] + border[2];
    Some(match style.box_sizing {
        BoxSizing::BorderBox => specified.max(0.0),
        BoxSizing::ContentBox => (specified + edges).max(0.0),
    })
}

fn content_contribution(style: &Style, space: &ConstraintSpace, collapsed: bool) -> Option<f32> {
    let specified = length_px(style.width, style, Some(space.available_inline), space)?;
    let pad = cell_padding(style, space);
    let border = if collapsed {
        [0.0; 4]
    } else {
        border_widths(style)
    };
    let edges = pad[1] + pad[3] + border[1] + border[3];
    Some(
        match style.box_sizing {
            BoxSizing::BorderBox => {
                (specified - edges).max(0.0) + if collapsed { 0.0 } else { edges }
            }
            BoxSizing::ContentBox => specified + edges,
        }
        .max(0.0),
    )
}

fn max_contribution(style: &Style, space: &ConstraintSpace, collapsed: bool) -> Option<f32> {
    let specified = length_px(style.max_width, style, Some(space.available_inline), space)?;
    let pad = cell_padding(style, space);
    let border = if collapsed {
        [0.0; 4]
    } else {
        border_widths(style)
    };
    let edges = pad[1] + pad[3] + border[1] + border[3];
    Some(match style.box_sizing {
        BoxSizing::BorderBox => (specified - edges).max(0.0) + if collapsed { 0.0 } else { edges },
        BoxSizing::ContentBox => (specified + edges).max(0.0),
    })
}

fn min_contribution(style: &Style, space: &ConstraintSpace, collapsed: bool) -> Option<f32> {
    let specified = length_px(style.min_width, style, Some(space.available_inline), space)?;
    let pad = cell_padding(style, space);
    let border = if collapsed {
        [0.0; 4]
    } else {
        border_widths(style)
    };
    let edges = pad[1] + pad[3] + border[1] + border[3];
    Some(match style.box_sizing {
        BoxSizing::BorderBox => (specified - edges).max(0.0) + if collapsed { 0.0 } else { edges },
        BoxSizing::ContentBox => (specified + edges).max(0.0),
    })
}

fn percent_width(style: &Style) -> Option<f32> {
    match style.width {
        Dimension::Percent(value) if value > 0.0 => Some(value),
        _ => None,
    }
}

/// Content min/max of one cell, ignoring the cell's own width (that width
/// is a column constraint, not a block constraint).
fn cell_content_minmax(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
) -> Option<MinMax> {
    // `Element.textContent` with no child list is a text leaf that still
    // carries the cell's display. The glyphs are this node, not a child.
    // `text_intrinsic_size` folds the style's own padding into the advance;
    // the column pass adds that padding again as cell padding.
    if let ComponentKind::Text { content } = &flat[index].kind {
        let style = flat[index].style;
        let padding = style.padding_lengths();
        let edges = padding.left + padding.right;
        return Some(inline::text_min_max(content, style).map(|width| (width - edges).max(0.0)));
    }
    let mut sizes = MinMax::exact(0.0);
    let mut saw = false;
    for child in inline::rendered_children(flat, index) {
        let child_sizes = block::intrinsic_sizes(flat, child, space).why("table:intrinsic")?;
        let style = flat[child].style;
        let border = border_widths(style);
        let pad_l = spacing_px(style, style.padding.left, 0.0, space);
        let pad_r = spacing_px(style, style.padding.right, 0.0, space);
        let edges = border[1] + border[3] + pad_l + pad_r;
        let margin_l = spacing_px(style, style.margin.left, 0.0, space);
        let margin_r = spacing_px(style, style.margin.right, 0.0, space);
        let specified = length_px(style.width, style, None, space).map(|width| {
            let content = match style.box_sizing {
                BoxSizing::BorderBox => (width - edges).max(0.0),
                BoxSizing::ContentBox => width.max(0.0),
            };
            MinMax::exact(content)
        });
        let content = specified.unwrap_or(child_sizes);
        let box_size = content
            .map(|width| block::clamp_inline_size(style, space, edges, width).max(0.0) + edges);
        sizes.max = sizes.max.max(box_size.max + margin_l + margin_r);
        sizes.min = match (sizes.min, box_size.min) {
            (Some(a), Some(b)) => Some(a.max(b + margin_l + margin_r)),
            (None, other) if !saw => other.map(|b| b + margin_l + margin_r),
            (a, _) => a,
        };
        saw = true;
    }
    if !saw {
        return Some(MinMax::exact(0.0));
    }
    Some(sizes)
}

fn encompass_column(column: &mut Column, min: f32, max: f32) {
    let min = min.max(0.0);
    column.content_min = column.content_min.max(min);
    column.min = column.min.max(min);
    column.max = column.max.max(max.max(column.min));
}

fn apply_length(column: &mut Column, size: f32) {
    column.fixed = true;
    column.min = column.min.max(size);
    column.max = column.max.max(size);
}

fn apply_cap(column: &mut Column, min: Option<f32>, max: Option<f32>) {
    if let Some(max) = max {
        column.max = column.max.min(max.max(0.0));
        column.min = column.min.min(column.max);
    }
    if let Some(min) = min {
        column.min = column.min.max(min.max(0.0));
        column.max = column.max.max(column.min);
    }
}

fn distribute_span(columns: &mut [Column], start: usize, span: usize, need: f32, minimum: bool) {
    if span == 0 || start >= columns.len() {
        return;
    }
    let end = (start + span).min(columns.len());
    let slice = &mut columns[start..end];
    let current: f32 = slice
        .iter()
        .map(|column| if minimum { column.min } else { column.max })
        .sum();
    let extra = need - current;
    if extra <= 0.0 {
        return;
    }
    let weight: f32 = slice
        .iter()
        .map(|column| if minimum { column.min } else { column.max })
        .sum();
    let mut given = 0.0f32;
    let count = slice.len();
    let last = count - 1;
    for (index, column) in slice.iter_mut().enumerate() {
        let share = if index == last {
            extra - given
        } else if weight <= 0.0 {
            extra / count as f32
        } else {
            let base = if minimum { column.min } else { column.max };
            extra * base / weight
        };
        given += share;
        if minimum {
            column.min += share;
            column.content_min += share;
            column.max = column.max.max(column.min);
        } else {
            column.max += share;
            column.min = column.min.min(column.max);
        }
    }
}

fn grid_of(flat: &[FlatNodeInfo], table: usize) -> Option<crate::table_grid::TableGrid> {
    let end = flat[table].subtree_end;
    let local: Vec<(&Style, Option<usize>)> = (table..end)
        .map(|index| {
            let parent = flat[index]
                .parent
                .filter(|parent| *parent >= table && *parent < end)
                .map(|parent| parent - table);
            (flat[index].style, parent)
        })
        .collect();
    let mut grids = table_grid::calculate(&local);
    let grid = grids.iter().position(|grid| grid.table == 0)?;
    let mut grid = grids.swap_remove(grid);
    for row in &mut grid.rows {
        *row += table;
    }
    for cell in &mut grid.cells {
        cell.index += table;
    }
    grid.table = table;
    Some(grid)
}

fn section_of(flat: &[FlatNodeInfo], row: usize) -> Option<usize> {
    let parent = flat[row].parent?;
    is_section(flat[parent].style.display).then_some(parent)
}

fn column_count(flat: &[FlatNodeInfo], table: usize, grid: &crate::table_grid::TableGrid) -> usize {
    let mut count = grid
        .cells
        .iter()
        .map(|cell| cell.column + cell.column_span)
        .max()
        .unwrap_or(0);
    let end = flat[table].subtree_end;
    for node in &flat[table..end] {
        if node.style.display == Display::TableColumn {
            count = count.max(1);
        }
    }
    // Columns are counted in source order below; this is only a floor.
    let mut cols = 0usize;
    fn walk(flat: &[FlatNodeInfo], index: usize, end: usize, cols: &mut usize) {
        let mut child = index + 1;
        while child < end
            && flat
                .get(child)
                .is_some_and(|node| node.parent == Some(index))
        {
            let next = flat[child].subtree_end.max(child + 1);
            match flat[child].style.display {
                Display::TableColumn => *cols += 1,
                Display::TableColumnGroup => {
                    let before = *cols;
                    walk(flat, child, next, cols);
                    if *cols == before {
                        *cols += 1;
                    }
                }
                _ => {}
            }
            child = next;
        }
    }
    walk(flat, table, end, &mut cols);
    count.max(cols)
}

struct ColSpec {
    column: usize,
    span: usize,
    style_index: usize,
}

fn column_specs(flat: &[FlatNodeInfo], table: usize) -> (Vec<ColSpec>, Vec<ColSpec>) {
    let mut specs = Vec::new();
    let mut groups = Vec::new();
    let mut column = 0usize;
    fn walk(
        flat: &[FlatNodeInfo],
        index: usize,
        end: usize,
        column: &mut usize,
        specs: &mut Vec<ColSpec>,
        groups: &mut Vec<ColSpec>,
    ) {
        let mut child = index + 1;
        while child < end
            && flat
                .get(child)
                .is_some_and(|node| node.parent == Some(index))
        {
            let next = flat[child].subtree_end.max(child + 1);
            match flat[child].style.display {
                Display::TableColumn => {
                    let span = column_span(flat[child].style).max(1);
                    specs.push(ColSpec {
                        column: *column,
                        span,
                        style_index: child,
                    });
                    *column += span;
                }
                Display::TableColumnGroup => {
                    let before = specs.len();
                    let at = *column;
                    walk(flat, child, next, column, specs, groups);
                    if specs.len() == before {
                        specs.push(ColSpec {
                            column: at,
                            span: 1,
                            style_index: child,
                        });
                        *column += 1;
                    } else if *column > at {
                        // The group box covers its columns. `max-height` does
                        // not apply to it; the background still paints.
                        groups.push(ColSpec {
                            column: at,
                            span: *column - at,
                            style_index: child,
                        });
                    }
                }
                _ => {}
            }
            child = next;
        }
    }
    walk(
        flat,
        table,
        flat[table].subtree_end,
        &mut column,
        &mut specs,
        &mut groups,
    );
    (specs, groups)
}

fn build_columns(
    flat: &[FlatNodeInfo],
    table: usize,
    grid: &crate::table_grid::TableGrid,
    space: &ConstraintSpace,
    collapsed: bool,
    fixed_layout: bool,
) -> Option<Vec<Column>> {
    let count = column_count(flat, table, grid).max(
        grid.cells
            .iter()
            .map(|cell| cell.column + cell.column_span)
            .max()
            .unwrap_or(0),
    );
    if count == 0 {
        return Some(Vec::new());
    }
    let mut columns = Vec::with_capacity(count);
    for _ in 0..count {
        columns.push(Column {
            min: 0.0,
            max: 0.0,
            content_min: 0.0,
            percent: 0.0,
            fixed: false,
            size: 0.0,
        });
    }
    let (specs, _) = column_specs(flat, table);
    for spec in specs {
        let style = flat[spec.style_index].style;
        if let Some(percent) = percent_width(style) {
            for column in columns.iter_mut().skip(spec.column).take(spec.span) {
                column.percent = column.percent.max(percent);
            }
        } else if let Some(size) = content_contribution(style, space, collapsed) {
            // A spanning column box shares the length across its columns.
            let each = size / spec.span as f32;
            for column in columns.iter_mut().skip(spec.column).take(spec.span) {
                apply_length(column, each);
            }
        }
        let min = min_contribution(style, space, collapsed).map(|size| size / spec.span as f32);
        let max = max_contribution(style, space, collapsed).map(|size| size / spec.span as f32);
        for column in columns.iter_mut().skip(spec.column).take(spec.span) {
            apply_cap(column, min, max);
        }
    }
    for cell in &grid.cells {
        if cell.column_span != 1 || cell.column >= columns.len() {
            continue;
        }
        let style = flat[cell.index].style;
        // Fixed layout sizes columns from the first row's specified widths.
        // Cell content can overflow; it does not widen the column.
        if fixed_layout {
            {
                let column = &mut columns[cell.column];
                if let Some(percent) = percent_width(style) {
                    column.percent = column.percent.max(percent);
                } else if let Some(size) = content_contribution(style, space, collapsed) {
                    apply_length(column, size);
                }
            }
            // A specified width still honors min-width and max-width.
            apply_cap(
                &mut columns[cell.column],
                min_contribution(style, space, collapsed),
                max_contribution(style, space, collapsed),
            );
            continue;
        }
        let content = cell_content_minmax(flat, cell.index, space).why("table:cell")?;
        let pad = cell_padding(style, space);
        let border = if collapsed {
            [0.0; 4]
        } else {
            border_widths(style)
        };
        let edges = pad[1] + pad[3] + border[1] + border[3];
        let min = content.min.unwrap_or(content.max).max(0.0) + edges;
        let max = content.max.max(0.0) + edges;
        let column = &mut columns[cell.column];
        encompass_column(column, min, max);
        if let Some(percent) = percent_width(style) {
            column.percent = column.percent.max(percent);
        } else if let Some(size) = content_contribution(style, space, collapsed) {
            apply_length(column, size.max(min));
        }
        apply_cap(
            column,
            min_contribution(style, space, collapsed),
            max_contribution(style, space, collapsed),
        );
    }
    for cell in &grid.cells {
        if cell.column_span <= 1 {
            continue;
        }
        let style = flat[cell.index].style;
        if fixed_layout {
            if percent_width(style).is_none()
                && let Some(size) = content_contribution(style, space, collapsed)
            {
                let gaps = if collapsed {
                    0.0
                } else {
                    flat[table].style.border_spacing_x * cell.column_span.saturating_sub(1) as f32
                };
                distribute_span(
                    &mut columns,
                    cell.column,
                    cell.column_span,
                    (size - gaps).max(0.0),
                    true,
                );
                distribute_span(
                    &mut columns,
                    cell.column,
                    cell.column_span,
                    (size - gaps).max(0.0),
                    false,
                );
            }
            continue;
        }
        let content = cell_content_minmax(flat, cell.index, space).why("table:cell")?;
        let pad = cell_padding(style, space);
        let border = if collapsed {
            [0.0; 4]
        } else {
            border_widths(style)
        };
        let edges = pad[1] + pad[3] + border[1] + border[3];
        let gaps = if collapsed {
            0.0
        } else {
            flat[table].style.border_spacing_x * cell.column_span.saturating_sub(1) as f32
        };
        let min = (content.min.unwrap_or(content.max).max(0.0) + edges - gaps).max(0.0);
        let max = (content.max.max(0.0) + edges - gaps).max(0.0);
        distribute_span(&mut columns, cell.column, cell.column_span, max, false);
        distribute_span(&mut columns, cell.column, cell.column_span, min, true);
        if percent_width(style).is_none()
            && let Some(size) = content_contribution(style, space, collapsed)
        {
            distribute_span(
                &mut columns,
                cell.column,
                cell.column_span,
                (size - gaps).max(0.0),
                true,
            );
            distribute_span(
                &mut columns,
                cell.column,
                cell.column_span,
                (size - gaps).max(0.0),
                false,
            );
        }
    }
    // A `<col>` length is the preferred width (CSS 2.1 17.5.2.2). Cell
    // max-content must not widen it once min-content fits inside it.
    if !fixed_layout {
        let (specs, _) = column_specs(flat, table);
        for spec in specs {
            let spec_style = flat[spec.style_index].style;
            if percent_width(spec_style).is_some() {
                continue;
            }
            if let Some(size) = content_contribution(spec_style, space, collapsed) {
                let each = size / spec.span as f32;
                for column in columns.iter_mut().skip(spec.column).take(spec.span) {
                    if column.min <= each {
                        column.max = each.max(column.min);
                    }
                }
            }
        }
    }
    for column in &mut columns {
        column.min = snap(column.min.max(0.0));
        column.content_min = snap(column.content_min.max(0.0));
        column.max = snap(column.max.max(column.min));
    }
    Some(columns)
}

fn sum_sizes(columns: &[Column], field: impl Fn(&Column) -> f32) -> f32 {
    columns.iter().map(field).sum()
}

fn share(extra: f32, weights: &[f32], index: usize) -> f32 {
    let total: f32 = weights.iter().sum();
    if (index + 1) == weights.len() {
        return extra
            - weights
                .iter()
                .take(index)
                .map(|weight| {
                    if total <= 0.0 {
                        extra / weights.len() as f32
                    } else {
                        extra * weight / total
                    }
                })
                .sum::<f32>();
    }
    if total <= 0.0 {
        extra / weights.len() as f32
    } else {
        extra * weights[index] / total
    }
}

/// Pull preferred column sizes back down to `inner`, but not below the
/// content minimum. When the content itself does not fit, the sizes stay
/// and the table grows.
fn squeeze_to_inner(columns: &mut [Column], inner: f32) {
    let sum: f32 = columns.iter().map(|column| column.size).sum();
    if sum <= inner + 1.0 / 64.0 {
        return;
    }
    let content: f32 = columns.iter().map(|column| column.content_min).sum();
    if content >= inner - 1.0 / 64.0 {
        for column in columns.iter_mut() {
            column.size = column.size.max(column.content_min);
        }
        return;
    }
    let slack: f32 = columns
        .iter()
        .map(|column| (column.size - column.content_min).max(0.0))
        .sum();
    if slack <= 0.0 {
        return;
    }
    let cut = (sum - inner).min(slack);
    for column in columns.iter_mut() {
        let room = (column.size - column.content_min).max(0.0);
        column.size = snap((column.size - cut * room / slack).max(column.content_min));
    }
}

fn assign_automatic(columns: &mut [Column], inner: f32) {
    if columns.is_empty() {
        return;
    }
    let mut size = vec![0.0f32; columns.len()];
    let mut taken = 0.0f32;
    let mut auto = Vec::new();
    for (index, column) in columns.iter().enumerate() {
        if column.percent > 0.0 {
            let percent = (column.percent / 100.0 * inner).max(column.min);
            size[index] = percent;
            taken += percent;
        } else if column.fixed {
            let fixed = column.max.max(column.min);
            size[index] = fixed;
            taken += fixed;
        } else {
            auto.push(index);
        }
    }
    let remaining = inner - taken;
    if auto.is_empty() {
        let percent_sum: f32 = columns.iter().map(|column| column.percent).sum();
        if remaining < 0.0 && percent_sum <= 100.0 {
            // Content minima (a colspan, a nowrap cell) are wider than the
            // specified table. Keep each column and let the table grow.
            // Shrinking only the columns that still have slack makes a
            // 90%/auto row disagree with a 90%/10% row.
            for index in 0..columns.len() {
                size[index] = size[index].max(columns[index].min);
            }
        } else {
            // Wider than the columns, or percentages sum past 100%: share
            // the difference, but never go under a column's minimum.
            let budget = if remaining < 0.0 { inner } else { remaining };
            let weights: Vec<f32> = size.clone();
            if remaining < 0.0 {
                for index in 0..columns.len() {
                    size[index] = share(budget, &weights, index).max(columns[index].min);
                }
            } else {
                let mut assigned = vec![0.0f32; columns.len()];
                for index in 0..columns.len() {
                    assigned[index] = share(budget, &weights, index);
                }
                for index in 0..columns.len() {
                    size[index] = (size[index] + assigned[index]).max(columns[index].min);
                }
            }
        }
    } else {
        // `remaining` is the whole budget for the auto columns, not extra
        // on top of their max-content size.
        let max_sum: f32 = auto.iter().map(|&index| columns[index].max).sum();
        let min_sum: f32 = auto.iter().map(|&index| columns[index].min).sum();
        if remaining >= max_sum {
            let extra = remaining - max_sum;
            let weights: Vec<f32> = auto.iter().map(|&index| columns[index].max).collect();
            for (offset, &index) in auto.iter().enumerate() {
                size[index] = columns[index].max + share(extra, &weights, offset);
            }
        } else if remaining >= min_sum {
            let room = remaining - min_sum;
            let weights: Vec<f32> = auto
                .iter()
                .map(|&index| (columns[index].max - columns[index].min).max(0.0))
                .collect();
            for (offset, &index) in auto.iter().enumerate() {
                size[index] = columns[index].min + share(room, &weights, offset);
            }
        } else {
            for &index in &auto {
                size[index] = columns[index].min;
            }
        }
    }
    // The last column absorbs the rounding difference, then every column is
    // snapped. A shortfall stays on the last column so the sum matches.
    // Snap, and give the rounding remainder to the last column when the
    // assignment already filled `inner`. A sum past `inner` (minima) keeps
    // its sizes so the table can grow.
    let assigned: f32 = size.iter().sum();
    let fill = assigned <= inner + 1.0 / 64.0;
    let mut used = 0.0f32;
    for index in 0..columns.len() {
        size[index] = snap(size[index].max(columns[index].min).max(0.0));
        if index + 1 != columns.len() {
            used += size[index];
        }
    }
    if let Some(last) = size.last_mut()
        && fill
    {
        let floor = columns.last().map(|column| column.min).unwrap_or(0.0);
        *last = snap((inner - used).max(floor).max(0.0));
    }
    for (column, used) in columns.iter_mut().zip(size) {
        column.size = used;
    }
}

fn assign_fixed(columns: &mut [Column], inner: f32) {
    if columns.is_empty() {
        return;
    }
    let mut specified = vec![None; columns.len()];
    for (index, column) in columns.iter().enumerate() {
        if column.percent > 0.0 {
            specified[index] = Some(column.percent / 100.0 * inner);
        } else if column.fixed {
            specified[index] = Some(column.max.max(column.min));
        }
    }
    let known: Vec<usize> = specified
        .iter()
        .enumerate()
        .filter_map(|(i, v)| v.map(|_| i))
        .collect();
    let unknown: Vec<usize> = (0..columns.len())
        .filter(|index| specified[*index].is_none())
        .collect();
    let known_sum: f32 = known
        .iter()
        .map(|&index| specified[index].unwrap_or(0.0))
        .sum();
    let mut size = vec![0.0f32; columns.len()];
    if known_sum > inner && unknown.is_empty() {
        let weights: Vec<f32> = known
            .iter()
            .map(|&index| specified[index].unwrap_or(0.0))
            .collect();
        for (offset, &index) in known.iter().enumerate() {
            size[index] = share(inner, &weights, offset).max(0.0);
        }
    } else if known_sum > inner {
        // Specified columns keep their lengths. The table grows; auto
        // columns get nothing (Chrome, fixed layout with an overconstrained
        // first row).
        for &index in &known {
            size[index] = specified[index].unwrap_or(0.0);
        }
    } else if unknown.is_empty() {
        let extra = inner - known_sum;
        let weights: Vec<f32> = known
            .iter()
            .map(|&index| specified[index].unwrap_or(0.0))
            .collect();
        for (offset, &index) in known.iter().enumerate() {
            size[index] = specified[index].unwrap_or(0.0) + share(extra, &weights, offset);
        }
    } else {
        for &index in &known {
            size[index] = specified[index].unwrap_or(0.0);
        }
        let extra = inner - known_sum;
        let weights = vec![1.0f32; unknown.len()];
        for (offset, &index) in unknown.iter().enumerate() {
            size[index] = share(extra, &weights, offset).max(0.0);
        }
    }
    let last_index = size.len().saturating_sub(1);
    let last_absorbs = unknown.is_empty() || unknown.last().copied() == Some(last_index);
    let mut used = 0.0f32;
    for index in 0..size.len().saturating_sub(1) {
        size[index] = snap(size[index].max(0.0));
        used += size[index];
    }
    if let Some(last) = size.last_mut() {
        if known_sum <= inner && last_absorbs {
            *last = snap((inner - used).max(0.0));
        } else {
            *last = snap((*last).max(0.0));
        }
    }
    for (column, used) in columns.iter_mut().zip(size) {
        column.size = used.max(column.min).max(0.0);
    }
}

fn spacing_of(style: &Style) -> (f32, f32) {
    if style.border_collapse {
        (0.0, 0.0)
    } else {
        (
            style.border_spacing_x.max(0.0),
            style.border_spacing_y.max(0.0),
        )
    }
}

fn edge(style: &Style, side: usize) -> (f32, bool) {
    let hidden = style.border_styles[side] == Some(BorderLineStyle::Hidden);
    let width = border_widths(style)[side].max(0.0);
    if hidden {
        (0.0, true)
    } else if width <= 0.0 {
        (0.0, false)
    } else {
        (width, false)
    }
}

fn merge_line(line: &mut (f32, bool), style: &Style, side: usize) {
    let (width, hidden) = edge(style, side);
    if hidden || line.1 {
        *line = (0.0, true);
    } else {
        line.0 = line.0.max(width);
    }
}

fn collapsed_lines(
    flat: &[FlatNodeInfo],
    table: usize,
    grid: &crate::table_grid::TableGrid,
    columns: usize,
) -> (Vec<f32>, Vec<f32>) {
    let rows = grid.rows.len();
    let mut vertical = vec![(0.0f32, false); columns + 1];
    let mut horizontal = vec![(0.0f32, false); rows + 1];
    let table_style = flat[table].style;
    if columns > 0 {
        merge_line(&mut vertical[0], table_style, 3);
        merge_line(&mut vertical[columns], table_style, 1);
    }
    if rows > 0 {
        merge_line(&mut horizontal[0], table_style, 0);
        merge_line(&mut horizontal[rows], table_style, 2);
    }
    for cell in &grid.cells {
        let style = flat[cell.index].style;
        let left = cell.column.min(columns);
        let right = (cell.column + cell.column_span).min(columns);
        let top = cell.row.min(rows);
        let bottom = (cell.row + cell.row_span).min(rows);
        if left < vertical.len() {
            merge_line(&mut vertical[left], style, 3);
        }
        if right < vertical.len() {
            merge_line(&mut vertical[right], style, 1);
        }
        if top < horizontal.len() {
            merge_line(&mut horizontal[top], style, 0);
        }
        if bottom < horizontal.len() {
            merge_line(&mut horizontal[bottom], style, 2);
        }
        if let Some(row) = grid.rows.get(cell.row) {
            let row_style = flat[*row].style;
            if top < horizontal.len() {
                merge_line(&mut horizontal[top], row_style, 0);
            }
            if bottom < horizontal.len() {
                merge_line(&mut horizontal[bottom], row_style, 2);
            }
            if cell.column == 0 && left < vertical.len() {
                merge_line(&mut vertical[left], row_style, 3);
            }
            if right == columns && right < vertical.len() {
                merge_line(&mut vertical[right], row_style, 1);
            }
        }
    }
    // Column and column-group borders meet the vertical lines at the
    // group's edges (CSS 2.1 17.6.2).
    {
        let mut column = 0usize;
        fn walk(
            flat: &[FlatNodeInfo],
            index: usize,
            end: usize,
            column: &mut usize,
            vertical: &mut [(f32, bool)],
        ) {
            let mut child = index + 1;
            while child < end
                && flat
                    .get(child)
                    .is_some_and(|node| node.parent == Some(index))
            {
                let next = flat[child].subtree_end.max(child + 1);
                match flat[child].style.display {
                    Display::TableColumn => {
                        let span = column_span(flat[child].style).max(1);
                        let left = (*column).min(vertical.len().saturating_sub(1));
                        let right = (*column + span).min(vertical.len().saturating_sub(1));
                        if left < vertical.len() {
                            merge_line(&mut vertical[left], flat[child].style, 3);
                        }
                        if right < vertical.len() {
                            merge_line(&mut vertical[right], flat[child].style, 1);
                        }
                        *column += span;
                    }
                    Display::TableColumnGroup => {
                        let at = *column;
                        walk(flat, child, next, column, vertical);
                        if *column == at {
                            *column += 1;
                        }
                        let left = at.min(vertical.len().saturating_sub(1));
                        let right = (*column).min(vertical.len().saturating_sub(1));
                        if left < vertical.len() {
                            merge_line(&mut vertical[left], flat[child].style, 3);
                        }
                        if right < vertical.len() {
                            merge_line(&mut vertical[right], flat[child].style, 1);
                        }
                    }
                    _ => {}
                }
                child = next;
            }
        }
        walk(
            flat,
            table,
            flat[table].subtree_end,
            &mut column,
            &mut vertical,
        );
    }
    // Row-group borders meet the horizontal lines of the group's first and
    // last rows, and the table's outer vertical lines.
    let mut section_start = 0usize;
    while section_start < grid.rows.len() {
        let section = grid
            .rows
            .get(section_start)
            .and_then(|row| section_of(flat, *row));
        let mut section_end = section_start + 1;
        while section_end < grid.rows.len()
            && grid
                .rows
                .get(section_end)
                .and_then(|row| section_of(flat, *row))
                == section
        {
            section_end += 1;
        }
        if let Some(section) = section {
            let style = flat[section].style;
            if section_start < horizontal.len() {
                merge_line(&mut horizontal[section_start], style, 0);
            }
            if section_end < horizontal.len() {
                merge_line(&mut horizontal[section_end], style, 2);
            }
            if columns > 0 {
                merge_line(&mut vertical[0], style, 3);
                if columns < vertical.len() {
                    merge_line(&mut vertical[columns], style, 1);
                }
            }
        }
        section_start = section_end;
    }
    (
        vertical.into_iter().map(|(width, _)| width).collect(),
        horizontal.into_iter().map(|(width, _)| width).collect(),
    )
}

fn caption_indexes(flat: &[FlatNodeInfo], table: usize) -> Vec<usize> {
    inline::rendered_children(flat, table)
        .filter(|child| flat[*child].style.display == Display::TableCaption)
        .collect()
}

fn caption_block(
    flat: &[FlatNodeInfo],
    index: usize,
    width: f32,
    space: &ConstraintSpace,
) -> Option<(LayoutResult, [f32; 4])> {
    let style = flat[index].style;
    // CSS 2.1 17.4: the caption is a block box of the table wrapper. Its
    // own width, min/max, padding, and margins resolve against that width.
    let mut nested = *space;
    nested.available_inline = width.max(0.0);
    nested.rtl = style.direction == TextDirection::Rtl;
    nested.bfc_x = 0.0;
    nested.bfc_y = 0.0;
    nested.forced_bfc = false;
    nested.margin_strut = MarginStrut::default();
    let result =
        block::layout_block_sized(flat, index, &nested, true, None).why("table:caption")?;
    let metrics = block::resolve_inline(style, &nested);
    Some((result, metrics.margin))
}

fn layout_cell_fragment(
    flat: &[FlatNodeInfo],
    index: usize,
    content_width: f32,
    space: &ConstraintSpace,
) -> Option<LayoutResult> {
    // A cell is laid out once at its natural height and stretched to the
    // row afterwards, so a percentage block size below it stays auto. Where
    // the cell, its row or section, or the table has a specified height,
    // Chrome resolves it against the stretched cell (css-sizing/
    // table-child-percentage-height-with-border-box); that needs a second
    // pass, so such tables stay on the legacy engine.
    if flat[index + 1..flat[index].subtree_end]
        .iter()
        .any(|node| matches!(node.style.height, Dimension::Percent(_)))
    {
        let mut ancestor = Some(index);
        while let Some(node) = ancestor {
            if !matches!(flat[node].style.height, Dimension::Auto) {
                return super::bail("table-cell-percent-height");
            }
            if flat[node].style.display == Display::Table {
                break;
            }
            ancestor = flat[node].parent;
        }
    }
    let mut nested = *space;
    nested.available_inline = content_width.max(0.0);
    nested.percentage_block = None;
    nested.rtl = flat[index].style.direction == TextDirection::Rtl;
    nested.bfc_x = 0.0;
    nested.bfc_y = 0.0;
    nested.forced_bfc = true;
    nested.margin_strut = MarginStrut::default();
    nested.margin_base = 0.0;
    let mut result = block::layout_block_sized(
        flat,
        index,
        &nested,
        true,
        Some((content_width.max(0.0), None)),
    )
    .why("table:cell")?;
    if result.float_overlap {
        return super::bail("table:float");
    }
    // A text leaf is the cell. Block layout records its baseline but no
    // line box, so paint would center the glyphs in the whole cell. An
    // inline child of a real cell is a line box; match that.
    if let ComponentKind::Text { content } = &flat[index].kind {
        let _ = append_text_leaf_line(
            &mut result,
            flat[index].style,
            content,
            content_width,
            space,
        );
    }
    Some(result)
}

/// One font-content box for a single-line text-leaf cell, in the cell's
/// border-box coordinates. Wrapped or bidi leaves stay unfragmented.
pub(super) fn append_text_leaf_line(
    result: &mut LayoutResult,
    style: &Style,
    content: &str,
    content_width: f32,
    space: &ConstraintSpace,
) -> bool {
    if content.is_empty()
        || style
            .custom_properties
            .as_ref()
            .is_some_and(|props| props.contains_key("--w3cos-internal-bidi-logical-order"))
    {
        return false;
    }
    let (_, baseline) = inline::block_text_layout(content, content_width.max(0.0), style);
    let LastBaseline::At(ascent) = baseline else {
        return false;
    };
    let content_ascent = crate::layout::inline_font_content_ascent(style);
    let font_height = crate::layout::inline_font_height(style);
    let padding = style.padding_lengths();
    let advance =
        (crate::layout::text_intrinsic_size(content, style).0 - padding.left - padding.right)
            .max(0.0);
    let pad_t = spacing_px(style, style.padding.top, space.available_inline, space);
    let pad_l = spacing_px(style, style.padding.left, space.available_inline, space);
    let border = border_widths(style);
    let free = content_width - advance;
    result.fragment.children.push((
        border[3] + pad_l + inline::align_offset(style, free),
        border[0] + pad_t + ascent - content_ascent,
        BoxFragment {
            index: result.fragment.index,
            width: advance,
            height: font_height,
            text_range: Some((0, content.len())),
            ellipsis: false,
            vertical: false,
            children: Vec::new(),
        },
    ));
    true
}

fn content_extent(fragment: &BoxFragment, border_bottom: f32, pad_bottom: f32) -> f32 {
    let children = fragment
        .children
        .iter()
        .map(|(_, y, child)| y + child.height)
        .fold(0.0f32, f32::max);
    fragment.height.max(children + border_bottom + pad_bottom)
}

fn row_specified_height(style: &Style, space: &ConstraintSpace) -> f32 {
    length_px(style.height, style, None, space)
        .or_else(|| length_px(style.min_height, style, None, space))
        .unwrap_or(0.0)
        .max(0.0)
}

fn shift_children(fragment: &mut BoxFragment, dx: f32, dy: f32) {
    if dx.abs() <= f32::EPSILON && dy.abs() <= f32::EPSILON {
        return;
    }
    for child in &mut fragment.children {
        child.0 += dx;
        child.1 += dy;
    }
}

/// Min and max border-box inline sizes. Percent columns resolve against the
/// other columns (`max / (1 - percent)`), which is how an auto table sizes
/// a `width: 50%` cell next to a 40px cell.
pub(super) fn intrinsic_border_box(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
) -> Option<MinMax> {
    intrinsic_border_box_inner(flat, index, space, true, Some(space.available_inline))
}

/// The table's min/max-content contribution to its parent: like
/// `intrinsic_border_box`, but a percentage `width` is cyclic (the parent
/// is being sized) and counts as auto, CSS Sizing 3 5.2.1.
pub(super) fn contribution_border_box(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
) -> Option<MinMax> {
    intrinsic_border_box_inner(flat, index, space, true, None)
}

/// Column min/max without the table's own `width`. Flex uses this so a
/// specified width stays the flex base instead of a second minimum.
pub(super) fn content_intrinsic_border_box(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
) -> Option<MinMax> {
    intrinsic_border_box_inner(flat, index, space, false, Some(space.available_inline))
}

fn intrinsic_border_box_inner(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    include_specified: bool,
    specified_basis: Option<f32>,
) -> Option<MinMax> {
    let style = flat[index].style;
    let grid = grid_of(flat, index);
    let collapsed = style.border_collapse;
    let (spacing_x, _) = spacing_of(style);
    let grid = match grid {
        Some(grid) => grid,
        None => return Some(MinMax::exact(0.0)),
    };
    let columns = build_columns(
        flat,
        index,
        &grid,
        space,
        collapsed,
        style.table_layout_fixed,
    )?;
    let n = columns.len();
    let gaps = if collapsed {
        0.0
    } else {
        spacing_x * n.saturating_add(1) as f32
    };
    let lines = if collapsed {
        let (vertical, _) = collapsed_lines(flat, index, &grid, n);
        vertical.iter().sum::<f32>()
    } else {
        let border = border_widths(style);
        let pad = [
            spacing_px(style, style.padding.left, space.available_inline, space),
            spacing_px(style, style.padding.right, space.available_inline, space),
        ];
        border[1] + border[3] + pad[0] + pad[1]
    };
    let min_tracks = sum_sizes(&columns, |column| column.min);
    let max_tracks = sum_sizes(&columns, |column| column.max);
    let percent: f32 = columns
        .iter()
        .map(|column| column.percent)
        .sum::<f32>()
        .min(100.0);
    let non_percent_max: f32 = columns
        .iter()
        .filter(|column| column.percent <= 0.0)
        .map(|column| column.max)
        .sum();
    let mut max_tracks = max_tracks;
    if percent > 0.0 && percent < 100.0 {
        let implied = non_percent_max / (1.0 - percent / 100.0);
        max_tracks = max_tracks.max(implied);
    }
    let mut min = min_tracks + gaps + lines;
    let mut max = max_tracks + gaps + lines;
    if include_specified && let Some(specified) = border_box_size(style, space, specified_basis) {
        min = min.max(specified);
        max = max.max(specified);
    }
    for caption in caption_indexes(flat, index) {
        let caption_style = flat[caption].style;
        let border = border_widths(caption_style);
        let pad_l = spacing_px(
            caption_style,
            caption_style.padding.left,
            space.available_inline,
            space,
        );
        let pad_r = spacing_px(
            caption_style,
            caption_style.padding.right,
            space.available_inline,
            space,
        );
        let box_edges = border[1] + border[3] + pad_l + pad_r;
        // A caption's margin box widens the table wrapper (CSS 2.1 17.4).
        // `intrinsic_sizes` is the content box, so an auto width still has
        // to add the caption's own border and padding.
        let (mut cap_min, mut cap_max) = if let Some(specified) =
            border_box_of(caption_style, caption_style.width, space, None)
        {
            (specified, specified)
        } else {
            let sizes = block::intrinsic_sizes(flat, caption, space).why("table:caption")?;
            let min = sizes.min.unwrap_or(sizes.max) + box_edges;
            (min, sizes.max + box_edges)
        };
        if let Some(min) = border_box_of(caption_style, caption_style.min_width, space, None) {
            cap_min = cap_min.max(min);
            cap_max = cap_max.max(min);
        }
        if let Some(max) = border_box_of(caption_style, caption_style.max_width, space, None) {
            cap_max = cap_max.min(max).max(cap_min);
        }
        let margin_l = spacing_px(
            caption_style,
            caption_style.margin.left,
            space.available_inline,
            space,
        );
        let margin_r = spacing_px(
            caption_style,
            caption_style.margin.right,
            space.available_inline,
            space,
        );
        cap_min += margin_l + margin_r;
        cap_max += margin_l + margin_r;
        min = min.max(cap_min.max(0.0));
        max = max.max(cap_max.max(0.0));
    }
    Some(MinMax {
        min: Some(snap(min.max(0.0))),
        max: snap(max.max(min)),
    })
}

fn parent_assigns_used_size(flat: &[FlatNodeInfo], index: usize) -> bool {
    flat[index].parent.is_some_and(|parent| {
        block::is_author_flex(flat[parent].style) || block::is_author_grid(flat[parent].style)
    })
}

fn target_border_width(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    sized: Option<f32>,
    content_floor: f32,
    content_min_floor: f32,
    max_border: f32,
    edges: f32,
) -> f32 {
    let style = flat[index].style;
    let margin_l = spacing_px(style, style.margin.left, space.available_inline, space);
    let margin_r = spacing_px(style, style.margin.right, space.available_inline, space);
    let cb = (space.available_inline - margin_l - margin_r).max(0.0);
    // `sized` from a float opportunity is the available width. From a flex
    // or grid item it is the used content width, and it wins over `width`.
    let limited = sized.map(|content| content + edges);
    if parent_assigns_used_size(flat, index) {
        let border = limited.unwrap_or(cb);
        if style.table_layout_fixed {
            return snap(border.max(0.0));
        }
        // Real column content can grow the item (a 100px child under
        // width:10px). A specified cell width cannot: the used flex size
        // wins and those columns compress.
        return snap(border.max(content_min_floor));
    }
    if let Some(specified) = border_box_size(style, space, Some(space.available_inline)) {
        // Fixed layout uses the specified width even when content is wider
        // (CSS 2.1 17.5.2). An overconstrained column group grows the table
        // later. Automatic layout's width is a minimum of the min-content.
        let floor = if style.table_layout_fixed {
            0.0
        } else {
            content_floor
        };
        return snap(specified.max(floor));
    }
    // `width: auto` shrinks to fit for both algorithms (CSS 2.1 17.5.2:
    // a fixed table with width auto may use the automatic width). Specified
    // columns have equal min and max, so the used width stays their sum
    // instead of the containing block. A float opportunity's `sized` width
    // is the available width beside the float.
    let available = limited.unwrap_or(cb);
    let width = max_border.min(available.max(content_floor));
    snap(width.max(content_floor))
}

pub(super) fn layout(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    sized: Option<(f32, Option<f32>)>,
) -> Option<LayoutResult> {
    layout_with_block_size(flat, index, space, sized, true)
}

/// `honor_specified_block` is false when a flex item's content-based
/// minimum is being measured. The table's own `height` is a preferred
/// size there, not part of the min-content block size.
pub(super) fn layout_with_block_size(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    sized: Option<(f32, Option<f32>)>,
    honor_specified_block: bool,
) -> Option<LayoutResult> {
    let style = flat[index].style;
    let collapsed = style.border_collapse;
    let grid = grid_of(flat, index).why("table:grid")?;
    // No cells: border-spacing would open a gap between captions (HTML's
    // 2px spacing on each side of an empty anonymous row).
    let (spacing_x, spacing_y) = if grid.cells.is_empty() {
        (0.0, 0.0)
    } else {
        spacing_of(style)
    };
    let mut columns = build_columns(
        flat,
        index,
        &grid,
        space,
        collapsed,
        style.table_layout_fixed,
    )
    .why("table:columns")?;
    let n = columns.len();
    let (vertical_lines, horizontal_lines) = if collapsed {
        collapsed_lines(flat, index, &grid, n)
    } else {
        (Vec::new(), Vec::new())
    };
    let line_sum: f32 = vertical_lines.iter().copied().sum();
    let border = border_widths(style);
    let pad = [
        if collapsed {
            0.0
        } else {
            spacing_px(style, style.padding.top, space.available_inline, space)
        },
        if collapsed {
            0.0
        } else {
            spacing_px(style, style.padding.right, space.available_inline, space)
        },
        if collapsed {
            0.0
        } else {
            spacing_px(style, style.padding.bottom, space.available_inline, space)
        },
        if collapsed {
            0.0
        } else {
            spacing_px(style, style.padding.left, space.available_inline, space)
        },
    ];
    let edges = if collapsed {
        0.0
    } else {
        border[1] + border[3] + pad[1] + pad[3]
    };
    let gaps_x = if collapsed {
        0.0
    } else {
        spacing_x * n.saturating_add(1) as f32
    };
    let intrinsic = intrinsic_border_box(flat, index, space).why("table:intrinsic")?;
    let max_border = intrinsic.max.max(intrinsic.min.unwrap_or(intrinsic.max));
    // Column minima only. The table's own `width` is applied in
    // `target_border_width`, so a flex item can be narrower than that width.
    let content_floor = snap(
        sum_sizes(&columns, |column| column.min)
            + gaps_x
            + if collapsed { line_sum } else { edges },
    );
    let content_min_floor = snap(
        sum_sizes(&columns, |column| column.content_min)
            + gaps_x
            + if collapsed { line_sum } else { edges },
    );
    let mut border_width = target_border_width(
        flat,
        index,
        space,
        sized.map(|sized| sized.0),
        content_floor,
        content_min_floor,
        max_border,
        edges,
    );
    // Fixed layout with an overconstrained first row grows the table.
    if style.table_layout_fixed {
        let specified_sum: f32 = columns
            .iter()
            .filter(|column| column.fixed || column.percent > 0.0)
            .map(|column| column.max.max(column.min))
            .sum();
        let fixed_min = specified_sum + gaps_x + if collapsed { line_sum } else { edges };
        border_width = border_width.max(snap(fixed_min));
    }
    // Automatic layout treats the used width as a minimum of the
    // min-content size (CSS 2.1 17.5.2). Fixed layout keeps the specified
    // or containing-block width; only an overconstrained first row grows it.
    if !style.table_layout_fixed && !parent_assigns_used_size(flat, index) {
        border_width = border_width.max(content_floor);
    }
    // Flex and grid already resolved min/max against the item's base.
    // An automatic table's min-content still wins over `max-width` there
    // (the item fragment grows and paints over a smaller flex area).
    if !parent_assigns_used_size(flat, index) {
        if let Some(max_width) =
            border_box_of(style, style.max_width, space, Some(space.available_inline))
        {
            let floor = if style.table_layout_fixed {
                0.0
            } else {
                content_floor
            };
            border_width = border_width.min(max_width).max(floor).max(0.0);
        }
        if let Some(min_width) =
            border_box_of(style, style.min_width, space, Some(space.available_inline))
        {
            border_width = border_width.max(min_width);
        }
    }
    let inner = if collapsed {
        (border_width - line_sum).max(0.0)
    } else {
        (border_width - edges - gaps_x).max(0.0)
    };
    if style.table_layout_fixed {
        // Percent columns in fixed layout resolve against the final border box.
        for column in &mut columns {
            if column.percent > 0.0 {
                column.fixed = false;
            }
        }
        assign_fixed(&mut columns, inner);
    } else {
        assign_automatic(&mut columns, inner);
        // The flex/grid used size is final. Specified cell widths shrink
        // down to real content so a 100px cell beside "FAIL" stays inside
        // the 100px item instead of growing the table.
        if parent_assigns_used_size(flat, index) {
            squeeze_to_inner(&mut columns, inner);
        }
    }
    // A fixed table that grew past the guess (auto columns at 0, specified
    // columns over the assigned width) keeps the sum of the used columns.
    let used_tracks: f32 = columns.iter().map(|column| column.size).sum();
    let grid_width = if collapsed {
        used_tracks + line_sum
    } else {
        used_tracks + gaps_x + edges
    };
    border_width = border_width.max(snap(grid_width));

    let mut rows: Vec<RowLayout> = grid
        .rows
        .iter()
        .map(|&row| RowLayout {
            index: row,
            section: section_of(flat, row),
            height: row_specified_height(flat[row].style, space),
            baseline: 0.0,
            has_line: false,
        })
        .collect();

    let mut cells: Vec<CellLayout> = Vec::with_capacity(grid.cells.len());
    for cell in &grid.cells {
        let style = flat[cell.index].style;
        let pad = cell_padding(style, space);
        let border = border_widths(style);
        let span = cell.column_span.max(1);
        let track: f32 = columns
            .iter()
            .skip(cell.column)
            .take(span)
            .map(|column| column.size)
            .sum();
        let internal = if collapsed {
            vertical_lines
                .iter()
                .skip(cell.column + 1)
                .take(span.saturating_sub(1))
                .sum::<f32>()
        } else {
            spacing_x * span.saturating_sub(1) as f32
        };
        // Collapsed column sizes are already content+padding. Separated
        // sizes are border boxes, and a colspan box includes the spacing
        // between its columns.
        let content_width = if collapsed {
            (track - pad[1] - pad[3]).max(0.0)
        } else {
            (track + internal - border[1] - border[3] - pad[1] - pad[3]).max(0.0)
        };
        let mut result = layout_cell_fragment(flat, cell.index, content_width, space)?;
        let extent = content_extent(&result.fragment, border[2], pad[2]);
        let specified = length_px(style.height, style, None, space)
            .map(|height| match style.box_sizing {
                BoxSizing::BorderBox => height.max(0.0),
                BoxSizing::ContentBox => height.max(0.0) + border[0] + border[2] + pad[0] + pad[2],
            })
            .unwrap_or(0.0);
        let border_box = extent.max(specified);
        // Collapsed grid lines own the borders. The row's inner size is
        // the cell's content and padding. A specified `height` stretches
        // the border box in block layout and leaves the content at the
        // top; `top` / `middle` / `bottom` then move that content. A
        // content-sized cell is already the size vertical-align centers,
        // so its children stay where block layout put them.
        let content_bottom = result
            .fragment
            .children
            .iter()
            .map(|(_, y, child)| y + child.height)
            .fold(0.0f32, f32::max);
        // The cell is stretched only when its specified height is what
        // sizes the box. A height smaller than the content-sized box is just
        // the row minimum: the box is sized by its content and borders, and
        // the children stay where block layout put them.
        let stretched = specified > 0.0
            && specified + 0.5 >= extent
            && content_bottom > 0.0
            && content_bottom + 0.5 < extent;
        let content_height = if collapsed {
            (extent - border[0] - border[2]).max(0.0)
        } else if stretched {
            content_bottom
        } else {
            extent
        };
        let height = if collapsed {
            (border_box - border[0] - border[2]).max(0.0)
        } else {
            border_box
        };
        // A specified height stretches the cell before baseline alignment.
        // The row minimum is that height; the baseline box is the content,
        // so an empty 0.5in cell does not pull the row baseline to its bottom.
        let intrinsic_box = if collapsed {
            content_bottom.max(0.0)
        } else if content_bottom > 0.0 {
            content_bottom.max(border[0] + pad[0]) + border[2] + pad[2]
        } else {
            border[0] + border[2] + pad[0] + pad[2]
        };
        let baseline_box = if specified > 0.0 && intrinsic_box + 0.5 < height {
            intrinsic_box
        } else {
            height
        };
        if collapsed {
            let left = vertical_lines.get(cell.column).copied().unwrap_or(0.0) / 2.0;
            let top = horizontal_lines.get(cell.row).copied().unwrap_or(0.0) / 2.0;
            shift_children(&mut result.fragment, left - border[3], top - border[0]);
        }
        let source = match result.first_baseline {
            LastBaseline::At(_) => result.first_baseline,
            _ => result.last_baseline,
        };
        let noline = if content_bottom > 0.0 {
            content_bottom
        } else if collapsed {
            0.0
        } else {
            border[0] + pad[0]
        };
        let (baseline, from_line) = match source {
            LastBaseline::At(at) => {
                let at = if collapsed {
                    (at - border[0]).max(0.0)
                } else {
                    at
                };
                (Some(at), true)
            }
            LastBaseline::NoLines => (Some(noline), false),
            LastBaseline::Unknown => (None, false),
        };
        cells.push(CellLayout {
            cell: *cell,
            fragment: result.fragment,
            height,
            content_height,
            baseline_box,
            baseline,
            from_line,
            valign: valign_of(style),
            oof: result.oof,
        });
    }

    // Single-row baseline alignment grows the row so the baselines meet.
    // The shift itself is applied once the final row heights are known.
    for row_index in 0..rows.len() {
        let mut row_baseline = 0.0f32;
        let mut baseline_cells = 0usize;
        let mut unknown = false;
        for cell in cells
            .iter()
            .filter(|cell| cell.cell.row == row_index && cell.cell.row_span == 1)
        {
            if cell.valign != Valign::Baseline {
                rows[row_index].height = rows[row_index].height.max(cell.height);
                continue;
            }
            baseline_cells += 1;
            if cell.from_line {
                rows[row_index].has_line = true;
            }
            match cell.baseline {
                Some(baseline) => row_baseline = row_baseline.max(baseline),
                None => unknown = true,
            }
            rows[row_index].height = rows[row_index].height.max(cell.height);
        }
        if unknown && baseline_cells > 1 {
            return super::bail("table:baseline");
        }
        rows[row_index].baseline = row_baseline;
        for cell in cells.iter().filter(|cell| {
            cell.cell.row == row_index && cell.cell.row_span == 1 && cell.valign == Valign::Baseline
        }) {
            if let Some(baseline) = cell.baseline {
                let shift = (row_baseline - baseline).max(0.0);
                rows[row_index].height = rows[row_index].height.max(cell.baseline_box + shift);
            }
        }
    }
    // Rowspan: a cell taller than the rows it covers shares the extra.
    for cell in &cells {
        if cell.cell.row_span <= 1 {
            continue;
        }
        let start = cell.cell.row;
        let end = (start + cell.cell.row_span).min(rows.len());
        if end <= start {
            continue;
        }
        let gaps = if collapsed {
            horizontal_lines
                .iter()
                .skip(start + 1)
                .take(end - start - 1)
                .copied()
                .sum()
        } else {
            spacing_y * (end - start - 1) as f32
        };
        let current: f32 = rows[start..end].iter().map(|row| row.height).sum::<f32>() + gaps;
        let extra = cell.height - current;
        if extra <= 0.0 {
            continue;
        }
        let each = extra / (end - start) as f32;
        for row in &mut rows[start..end] {
            row.height += each;
        }
    }
    for row in &mut rows {
        row.height = snap(row.height.max(0.0));
    }

    // A definite table height grows the rows (the specified height is a
    // minimum). Captions are outside that grid.
    let row_gaps = if rows.is_empty() {
        0.0
    } else {
        spacing_y * rows.len().saturating_add(1) as f32
    };
    let horizontal_sum: f32 = if collapsed {
        horizontal_lines.iter().copied().sum()
    } else {
        0.0
    };
    let grid_height: f32 = rows.iter().map(|row| row.height).sum::<f32>()
        + if collapsed { horizontal_sum } else { row_gaps };
    let captions = caption_indexes(flat, index);
    let mut caption_results: Vec<(usize, LayoutResult, [f32; 4])> = Vec::new();
    let mut caption_height = 0.0f32;
    for caption in &captions {
        let (result, margin) = caption_block(flat, *caption, border_width, space)?;
        caption_height += margin[0] + result.fragment.height + margin[2];
        let outer = margin[3] + result.fragment.width + margin[1];
        if outer > border_width {
            border_width = snap(outer);
        }
        caption_results.push((*caption, result, margin));
    }
    let mut specified_border_height = sized
        .and_then(|(_, height)| height)
        .or_else(|| {
            honor_specified_block
                .then(|| length_px(style.height, style, space.percentage_block, space))
                .flatten()
        })
        .map(|height| match style.box_sizing {
            BoxSizing::BorderBox => height.max(0.0),
            BoxSizing::ContentBox => (height + border[0] + border[2] + pad[0] + pad[2]).max(0.0),
        });
    if honor_specified_block {
        if let Some(min_height) = border_box_block(style, style.min_height, space) {
            specified_border_height = Some(
                specified_border_height
                    .map(|height| height.max(min_height))
                    .unwrap_or(min_height),
            );
        }
        if let Some(max_height) = border_box_block(style, style.max_height, space) {
            let min_height = border_box_block(style, style.min_height, space).unwrap_or(0.0);
            // A min-height above max-height wins (CSS 2.1 10.7).
            if min_height <= max_height {
                if let Some(height) = specified_border_height.as_mut() {
                    *height = (*height).min(max_height);
                }
            }
        }
    }
    let specified_height = specified_border_height
        .map(|height| (height - border[0] - border[2] - pad[0] - pad[2]).max(0.0));
    if let Some(target) = specified_height {
        let grid_target = (target - caption_height).max(0.0);
        if grid_target > grid_height && !rows.is_empty() {
            let extra = grid_target - grid_height;
            let each = extra / rows.len() as f32;
            for row in &mut rows {
                row.height = snap(row.height + each);
            }
        }
    }
    // `max-height` caps the extra row growth above, and does not shrink a
    // table under its content size (css-tables max-height-table).

    // Place rows in header, body, footer order (CSS 2.1 17.4).
    let mut order: Vec<usize> = Vec::with_capacity(rows.len());
    for (index_row, row) in rows.iter().enumerate() {
        if row
            .section
            .is_some_and(|section| flat[section].style.display == Display::TableHeaderGroup)
        {
            order.push(index_row);
        }
    }
    for (index_row, row) in rows.iter().enumerate() {
        let footer = row
            .section
            .is_some_and(|section| flat[section].style.display == Display::TableFooterGroup);
        let header = row
            .section
            .is_some_and(|section| flat[section].style.display == Display::TableHeaderGroup);
        if !header && !footer {
            order.push(index_row);
        }
    }
    for (index_row, row) in rows.iter().enumerate() {
        if row
            .section
            .is_some_and(|section| flat[section].style.display == Display::TableFooterGroup)
        {
            order.push(index_row);
        }
    }
    let top_captions: f32 = caption_results
        .iter()
        .filter(|(caption, _, _)| !flat[*caption].style.caption_side_bottom)
        .map(|(_, result, margin)| margin[0] + result.fragment.height + margin[2])
        .sum();
    let origin_y = if collapsed {
        horizontal_lines.first().copied().unwrap_or(0.0) / 2.0
    } else {
        border[0] + pad[0] + spacing_y
    } + top_captions;
    let mut cursor = origin_y;
    let mut row_y = vec![0.0f32; rows.len()];
    for (placed, &row_index) in order.iter().enumerate() {
        row_y[row_index] = cursor;
        if collapsed {
            // Next row starts at the next grid-line center.
            let top = horizontal_lines.get(row_index).copied().unwrap_or(0.0);
            let bottom = horizontal_lines.get(row_index + 1).copied().unwrap_or(0.0);
            cursor += top / 2.0 + rows[row_index].height + bottom / 2.0;
        } else {
            cursor += rows[row_index].height;
            if placed + 1 != order.len() {
                cursor += spacing_y;
            }
        }
    }
    let grid_bottom = if rows.is_empty() {
        origin_y
    } else if collapsed {
        let last = order.last().copied().unwrap_or(0);
        let top = horizontal_lines.get(last).copied().unwrap_or(0.0);
        let bottom = horizontal_lines.get(last + 1).copied().unwrap_or(0.0);
        // Past the last center by the outer half of the bottom line.
        row_y[last] + top / 2.0 + rows[last].height + bottom
    } else {
        cursor + spacing_y
    };

    let origin_x = if collapsed {
        vertical_lines.first().copied().unwrap_or(0.0) / 2.0
    } else {
        border[3] + pad[3] + spacing_x
    };
    let mut col_x = vec![0.0f32; n];
    let mut cursor_x = origin_x;
    for (index_col, column) in columns.iter().enumerate() {
        col_x[index_col] = cursor_x;
        if collapsed {
            let left = vertical_lines.get(index_col).copied().unwrap_or(0.0);
            let right = vertical_lines.get(index_col + 1).copied().unwrap_or(0.0);
            cursor_x += left / 2.0 + column.size + right / 2.0;
        } else {
            cursor_x += column.size;
            if index_col + 1 != n {
                cursor_x += spacing_x;
            }
        }
    }

    let mut cell_origin = vec![(0.0f32, 0.0f32, 0.0f32, 0.0f32); cells.len()];
    for (index_cell, cell) in cells.iter().enumerate() {
        let span = cell.cell.column_span.max(1);
        let x = col_x.get(cell.cell.column).copied().unwrap_or(origin_x);
        let mut width: f32 = columns
            .iter()
            .skip(cell.cell.column)
            .take(span)
            .map(|column| column.size)
            .sum();
        if !collapsed {
            width += spacing_x * span.saturating_sub(1) as f32;
        } else {
            // `column.size` is content+padding. The border box runs from
            // grid-line center to grid-line center.
            let left = vertical_lines.get(cell.cell.column).copied().unwrap_or(0.0) / 2.0;
            let right = vertical_lines
                .get(cell.cell.column + span)
                .copied()
                .unwrap_or(0.0)
                / 2.0;
            let internal: f32 = vertical_lines
                .iter()
                .skip(cell.cell.column + 1)
                .take(span.saturating_sub(1))
                .copied()
                .sum();
            width += left + right + internal;
        }
        let y = row_y.get(cell.cell.row).copied().unwrap_or(origin_y);
        let end_row = (cell.cell.row + cell.cell.row_span).min(rows.len());
        let height = if end_row == 0 {
            cell.height
        } else if collapsed {
            let top = horizontal_lines.get(cell.cell.row).copied().unwrap_or(0.0) / 2.0;
            let bottom = horizontal_lines.get(end_row).copied().unwrap_or(0.0) / 2.0;
            let inner: f32 = rows[cell.cell.row..end_row]
                .iter()
                .map(|row| row.height)
                .sum();
            let internal: f32 = horizontal_lines
                .iter()
                .skip(cell.cell.row + 1)
                .take(end_row - cell.cell.row - 1)
                .copied()
                .sum();
            top + inner + internal + bottom
        } else {
            // `row_y` already includes the spacing between spanned rows.
            let last = end_row - 1;
            (row_y[last] + rows[last].height - y).max(cell.height)
        };
        cell_origin[index_cell] = (x, y, snap(width), snap(height));
    }

    // Vertical-align inside the final border box.
    for (index_cell, cell) in cells.iter_mut().enumerate() {
        let (_, _, _, slot_h) = cell_origin[index_cell];
        let row = cell.cell.row;
        let end_row = (row + cell.cell.row_span).min(rows.len());
        // Collapsed alignment is inside the content area. The grid-line
        // inset was applied when the cell was laid out.
        let align_slot = if collapsed && end_row > row {
            let inner: f32 = rows[row..end_row].iter().map(|row| row.height).sum();
            let internal: f32 = horizontal_lines
                .iter()
                .skip(row + 1)
                .take(end_row - row - 1)
                .copied()
                .sum();
            inner + internal
        } else {
            slot_h
        };
        let shift = match cell.valign {
            Valign::Top => 0.0,
            Valign::Bottom => (align_slot - cell.content_height).max(0.0),
            Valign::Middle => (align_slot - cell.content_height).max(0.0) / 2.0,
            Valign::Baseline => {
                if cell.cell.row_span > 1 {
                    0.0
                } else {
                    cell.baseline
                        .map(|baseline| {
                            (rows.get(row).map(|row| row.baseline).unwrap_or(0.0) - baseline)
                                .max(0.0)
                        })
                        .unwrap_or(0.0)
                }
            }
        };
        shift_children(&mut cell.fragment, 0.0, shift);
        cell.fragment.width = cell_origin[index_cell].2;
        cell.fragment.height = slot_h;
    }

    if style.direction == TextDirection::Rtl && border_width > 0.0 {
        for origin in &mut cell_origin {
            origin.0 = snap(border_width - origin.0 - origin.2);
        }
    }

    let bottom_captions: f32 = caption_results
        .iter()
        .filter(|(caption, _, _)| flat[*caption].style.caption_side_bottom)
        .map(|(_, result, margin)| margin[0] + result.fragment.height + margin[2])
        .sum();
    let mut table_height =
        snap(grid_bottom + bottom_captions + if collapsed { 0.0 } else { border[2] + pad[2] });
    // CSS 2.1 17.5.3: a specified table height is a minimum border-box
    // size. An empty grid has no row to absorb it, so the box itself grows.
    if let Some(specified) = specified_border_height {
        table_height = table_height.max(snap(specified));
    }
    border_width = snap(border_width.max(grid_width));

    // First row's baseline, for an inline-table (CSS 2.1 10.8.1).
    let first_visual = order.first().copied();
    let last_baseline = first_visual
        .map(|row| {
            let inset = if collapsed {
                horizontal_lines.get(row).copied().unwrap_or(0.0) / 2.0
            } else {
                0.0
            };
            let aligned = if rows[row].has_line {
                rows[row].baseline
            } else {
                // No line box: the baseline is the bottom of the row's
                // content edge, including a table min-height that stretched
                // the row after the cells were measured.
                rows[row].height
            };
            LastBaseline::At(row_y[row] + inset + aligned)
        })
        .unwrap_or(LastBaseline::NoLines);

    let mut oof = Vec::new();
    // Captions, columns, sections, rows, and cells in source order under
    // their DOM parent. Positions are table-relative; `emit` subtracts the
    // parent.
    let mut placed: Vec<Option<(f32, f32, f32, f32)>> = vec![None; flat[index].subtree_end - index];
    let local = |node: usize| node - index;
    placed[0] = Some((0.0, 0.0, border_width, table_height));
    let mut cap_top = 0.0f32;
    let mut cap_bottom = grid_bottom;
    let mut caption_space = *space;
    caption_space.available_inline = border_width;
    for (caption, result, margin) in &caption_results {
        let (dx, dy) = super::oof::relative_offset(flat[*caption].style, &caption_space);
        let outer = margin[0] + result.fragment.height + margin[2];
        let y = if flat[*caption].style.caption_side_bottom {
            let y = cap_bottom + margin[0];
            cap_bottom += outer;
            y
        } else {
            let y = cap_top + margin[0];
            cap_top += outer;
            y
        };
        placed[local(*caption)] = Some((
            snap(margin[3] + dx),
            snap(y + dy),
            result.fragment.width,
            result.fragment.height,
        ));
        for candidate in &result.oof {
            oof.push(candidate.translated(margin[3] + dx, y + dy));
        }
    }
    for (index_cell, cell) in cells.iter().enumerate() {
        placed[local(cell.cell.index)] = Some(cell_origin[index_cell]);
        for candidate in &cell.oof {
            oof.push(candidate.translated(cell_origin[index_cell].0, cell_origin[index_cell].1));
        }
    }
    for (row_index, row) in rows.iter().enumerate() {
        let y = row_y[row_index];
        let x = cells
            .iter()
            .enumerate()
            .filter(|(_, cell)| cell.cell.row == row_index)
            .map(|(index_cell, _)| cell_origin[index_cell].0)
            .fold(f32::MAX, f32::min);
        let right = cells
            .iter()
            .enumerate()
            .filter(|(_, cell)| cell.cell.row == row_index)
            .map(|(index_cell, _)| cell_origin[index_cell].0 + cell_origin[index_cell].2)
            .fold(0.0f32, f32::max);
        let x = if x.is_finite() { x } else { origin_x };
        let width = (right - x).max(0.0);
        let height = if collapsed {
            let top = horizontal_lines.get(row_index).copied().unwrap_or(0.0) / 2.0;
            let bottom = horizontal_lines.get(row_index + 1).copied().unwrap_or(0.0) / 2.0;
            top + row.height + bottom
        } else {
            row.height
        };
        placed[local(row.index)] = Some((x, y, snap(width), snap(height)));
    }
    // Sections wrap their rows.
    let mut section_ids: Vec<usize> = rows.iter().filter_map(|row| row.section).collect();
    section_ids.sort_unstable();
    section_ids.dedup();
    for section in section_ids {
        let members: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.section == Some(section))
            .map(|(index_row, _)| index_row)
            .collect();
        if members.is_empty() {
            continue;
        }
        let top = members
            .iter()
            .filter_map(|&row| placed[local(rows[row].index)].map(|rect| rect.1))
            .fold(f32::MAX, f32::min);
        let bottom = members
            .iter()
            .filter_map(|&row| placed[local(rows[row].index)].map(|rect| rect.1 + rect.3))
            .fold(0.0f32, f32::max);
        let left = members
            .iter()
            .filter_map(|&row| placed[local(rows[row].index)].map(|rect| rect.0))
            .fold(f32::MAX, f32::min);
        let right = members
            .iter()
            .filter_map(|&row| placed[local(rows[row].index)].map(|rect| rect.0 + rect.2))
            .fold(0.0f32, f32::max);
        let left = if left.is_finite() { left } else { 0.0 };
        placed[local(section)] =
            Some((left, top, (right - left).max(0.0), (bottom - top).max(0.0)));
    }
    let col_top = order.first().map(|&row| row_y[row]).unwrap_or(origin_y);
    let col_bottom = if let Some(&last) = order.last() {
        if collapsed {
            let top = horizontal_lines.get(last).copied().unwrap_or(0.0) / 2.0;
            let bottom = horizontal_lines.get(last + 1).copied().unwrap_or(0.0) / 2.0;
            row_y[last] + top + rows[last].height + bottom
        } else {
            row_y[last] + rows[last].height
        }
    } else {
        col_top
    };
    let (column_boxes, group_boxes) = column_specs(flat, index);
    for spec in column_boxes.iter().chain(&group_boxes) {
        if spec.column >= col_x.len() {
            continue;
        }
        let mut width: f32 = columns
            .iter()
            .skip(spec.column)
            .take(spec.span)
            .map(|column| column.size)
            .sum();
        if collapsed {
            let left = vertical_lines.get(spec.column).copied().unwrap_or(0.0) / 2.0;
            let right = vertical_lines
                .get(spec.column + spec.span)
                .copied()
                .unwrap_or(0.0)
                / 2.0;
            let internal: f32 = vertical_lines
                .iter()
                .skip(spec.column + 1)
                .take(spec.span.saturating_sub(1))
                .copied()
                .sum();
            width += left + right + internal;
        } else if spec.span > 1 {
            width += spacing_x * spec.span.saturating_sub(1) as f32;
        }
        width = snap(width);
        let x = if style.direction == TextDirection::Rtl {
            snap(border_width - col_x[spec.column] - width)
        } else {
            col_x[spec.column]
        };
        placed[local(spec.style_index)] =
            Some((x, col_top, width, snap((col_bottom - col_top).max(0.0))));
    }
    fn emit(
        flat: &[FlatNodeInfo],
        index: usize,
        table: usize,
        placed: &[Option<(f32, f32, f32, f32)>],
        cells: &[CellLayout],
        captions: &[(usize, LayoutResult, [f32; 4])],
    ) -> BoxFragment {
        let local = index - table;
        let rect = placed[local].unwrap_or((0.0, 0.0, 0.0, 0.0));
        if let Some(cell) = cells.iter().find(|cell| cell.cell.index == index) {
            return cell.fragment.clone();
        }
        if let Some((_, result, _)) = captions.iter().find(|(caption, _, _)| *caption == index) {
            return result.fragment.clone();
        }
        let mut children = Vec::new();
        for child in inline::rendered_children(flat, index) {
            let child_local = child - table;
            if child_local >= placed.len() || placed[child_local].is_none() {
                continue;
            }
            let child_rect = placed[child_local].unwrap();
            let fragment = emit(flat, child, table, placed, cells, captions);
            children.push((child_rect.0 - rect.0, child_rect.1 - rect.1, fragment));
        }
        BoxFragment {
            index,
            text_range: None,
            ellipsis: false,
            vertical: false,
            width: rect.2,
            height: rect.3,
            children,
        }
    }

    let mut fragment = emit(flat, index, index, &placed, &cells, &caption_results);
    // A relative table is the containing block of its absolute descendants.
    // Leaving them unresolved paints `inset: 0` against the viewport.
    if style.position != Position::Static && !oof.is_empty() {
        let cb = super::oof::ContainingBlock {
            x: border[3] + pad[3],
            y: border[0] + pad[0],
            width: (border_width - border[1] - border[3] - pad[1] - pad[3]).max(0.0),
            height: (table_height - border[0] - border[2] - pad[0] - pad[2]).max(0.0),
            rtl: style.direction == TextDirection::Rtl,
        };
        let (placed_oof, rest) = super::oof::resolve(
            flat,
            std::mem::take(&mut oof),
            &cb,
            false,
            space.viewport_w,
            space.viewport_h,
        )
        .why("table:oof")?;
        for (x, y, child) in placed_oof {
            fragment.children.push((cb.x + x, cb.y + y, child));
        }
        oof = rest;
    }
    let metrics = block::resolve_inline(style, space);
    let mut start_strut = MarginStrut::default();
    start_strut.append(metrics.margin[0]);
    let mut end_strut = MarginStrut::default();
    end_strut.append(metrics.margin[2]);
    Some(LayoutResult {
        fragment,
        last_baseline,
        first_baseline: last_baseline,
        start_strut,
        end_strut,
        self_collapsing: false,
        oof,
        bfc_dependent: false,
        float_overlap: false,
    })
}

/// A column box uses the same custom property as a cell when the DOM recorded
/// a span; otherwise it covers one column.
fn column_span(style: &Style) -> usize {
    style
        .custom_properties
        .as_ref()
        .and_then(|props| props.get("--w3cos-internal-table-column-span"))
        .and_then(|span| span.parse::<usize>().ok())
        .filter(|span| *span > 0)
        .unwrap_or(1)
        .min(1000)
}
