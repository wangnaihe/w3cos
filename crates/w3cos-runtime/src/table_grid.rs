//! Shared section-aware HTML/CSS table slot coordinates.
use w3cos_std::style::{Display, Style};
use std::collections::HashMap;

pub(crate) const ROW_SPAN: &str = "--w3cos-internal-table-row-span";
pub(crate) const COLUMN_SPAN: &str = "--w3cos-internal-table-column-span";
pub(crate) const COLUMN_START: &str = "--w3cos-internal-table-column-start";

fn property(style: &Style, name: &str) -> Option<usize> {
    style.custom_properties.as_ref()?.get(name)?.parse().ok()
}

pub(crate) fn row_span(style: &Style) -> usize { property(style, ROW_SPAN).unwrap_or(1).min(65534) }
pub(crate) fn column_start(style: &Style, fallback: usize) -> usize {
    property(style, COLUMN_START).unwrap_or(fallback)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GridCell {
    pub index: usize,
    pub row: usize,
    pub column: usize,
    pub column_span: usize,
    pub row_span: usize,
}

#[derive(Debug)]
pub(crate) struct TableGrid {
    pub table: usize,
    pub rows: Vec<usize>,
    pub cells: Vec<GridCell>,
}

pub(crate) fn calculate(nodes: &[(&Style, Option<usize>)]) -> Vec<TableGrid> {
    let mut owners = vec![None; nodes.len()];
    let mut sections = vec![None; nodes.len()];
    let mut children = vec![Vec::new(); nodes.len()];
    let mut rows = HashMap::<usize, Vec<usize>>::new();
    let mut tables = Vec::new();
    for (index, &(style, parent)) in nodes.iter().enumerate() {
        if let Some(parent) = parent {
            owners[index] = owners[parent];
            sections[index] = sections[parent];
            children[parent].push(index);
        }
        if matches!(style.display, Display::Table | Display::InlineTable) {
            owners[index] = Some(index);
            sections[index] = None;
            tables.push(index);
        } else if matches!(style.display, Display::TableRowGroup | Display::TableHeaderGroup | Display::TableFooterGroup) {
            sections[index] = Some(index);
        } else if style.display == Display::TableRow && let Some(table) = owners[index] {
            rows.entry(table).or_default().push(index);
        }
    }
    tables.into_iter().map(|table| {
        let rows = rows.remove(&table).unwrap_or_default();
        let mut cells = Vec::new();
        let mut section_start = 0;
        while section_start < rows.len() {
            let section = sections[rows[section_start]];
            let mut section_end = section_start + 1;
            while section_end < rows.len() && sections[rows[section_end]] == section { section_end += 1; }
            let mut occupied_until = Vec::<usize>::new();
            for row in section_start..section_end {
                let mut column = 0;
                for &index in &children[rows[row]] {
                    let style = nodes[index].0;
                    if style.display != Display::TableCell { continue; }
                    while occupied_until.get(column).is_some_and(|end| *end > row) { column += 1; }
                    let column_span = property(style, COLUMN_SPAN).unwrap_or(1).clamp(1, 1000);
                    let declared = row_span(style);
                    // Chromium clamps a cell to the remaining section rows;
                    // zero explicitly grows to the same section boundary.
                    let row_span = if declared == 0 { section_end - row }
                        else { declared.min(section_end - row) };
                    occupied_until.resize(occupied_until.len().max(column + column_span), 0);
                    for end in &mut occupied_until[column..column + column_span] { *end = (*end).max(row + row_span); }
                    cells.push(GridCell { index, row, column, column_span, row_span });
                    column += column_span;
                }
            }
            section_start = section_end;
        }
        TableGrid { table, rows, cells }
    }).collect()
}

pub(crate) fn annotate(root: &mut w3cos_std::Component) {
    let flat = crate::layout::pre_flatten(root);
    if !flat.iter().any(|node| row_span(node.style) != 1) { return; }
    let nodes = flat.iter().map(|node| (node.style, node.parent)).collect::<Vec<_>>();
    let placements = calculate(&nodes).into_iter()
        .filter(|grid| grid.cells.iter().any(|cell| cell.row_span > 1))
        .flat_map(|grid| grid.cells).map(|cell| (cell.index, cell.column)).collect::<HashMap<_, _>>();
    fn visit(component: &mut w3cos_std::Component, index: &mut usize, placements: &HashMap<usize, usize>) {
        if let Some(column) = placements.get(index) {
            component.style.custom_properties.get_or_insert_with(Default::default)
                .insert(COLUMN_START.into(), column.to_string());
        }
        *index += 1;
        for child in &mut component.children { visit(child, index, placements); }
    }
    visit(root, &mut 0, &placements);
}

/// Project a spanning cell onto its settled section rows before content
/// vertical alignment. Row sizing remains a separate upstream calculation.
pub(crate) fn project_rowspan_extents(
    layouts: &mut [(crate::layout::LayoutRect, usize)],
    flat: &[crate::layout::FlatNodeInfo<'_>],
) {
    if !flat.iter().any(|node| row_span(node.style) != 1) { return; }
    let nodes = flat.iter().map(|node| (node.style, node.parent)).collect::<Vec<_>>();
    let positions = layouts.iter().enumerate()
        .map(|(position, (_, index))| (*index, position)).collect::<HashMap<_, _>>();
    for grid in calculate(&nodes) {
        for cell in grid.cells.iter().filter(|cell| cell.row_span > 1) {
            let Some(&position) = positions.get(&cell.index) else { continue; };
            let Some(&first) = positions.get(&grid.rows[cell.row]) else { continue; };
            let Some(&last) = positions.get(&grid.rows[cell.row + cell.row_span - 1]) else { continue; };
            let top = layouts[first].0.y;
            let bottom = layouts[last].0.y + layouts[last].0.height;
            layouts[position].0.y = top;
            layouts[position].0.height = (bottom - top).max(0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(display: Display) -> Style { Style { display, ..Style::default() } }
    fn span(style: &mut Style, name: &str, value: usize) {
        style.custom_properties.get_or_insert_with(Default::default).insert(name.into(), value.to_string());
    }

    #[test]
    fn rowspan_occupancy_skips_middle_tracks_in_later_rows() {
        let mut styles = [Display::Table, Display::TableRowGroup, Display::TableRow,
            Display::TableCell, Display::TableCell, Display::TableCell, Display::TableRow,
            Display::TableCell, Display::TableCell, Display::TableRow,
            Display::TableCell, Display::TableCell].map(style);
        span(&mut styles[4], ROW_SPAN, 3);
        span(&mut styles[4], COLUMN_SPAN, 2);
        let parents = [None, Some(0), Some(1), Some(2), Some(2), Some(2), Some(1),
            Some(6), Some(6), Some(1), Some(9), Some(9)];
        let nodes = styles.iter().zip(parents).collect::<Vec<_>>();
        let grids = calculate(&nodes);
        assert_eq!(grids.len(), 1);
        assert_eq!(grids[0].rows, [2, 6, 9]);
        let cell = |index| *grids[0].cells.iter().find(|cell| cell.index == index).unwrap();
        assert_eq!((cell(4).column, cell(4).column_span, cell(4).row_span), (1, 2, 3));
        for index in [5, 8, 11] { assert_eq!(cell(index).column, 3); }
    }

    #[test]
    fn zero_rowspan_ends_at_its_section_and_empty_rows_count() {
        let mut styles = [Display::Table, Display::TableRowGroup, Display::TableRow,
            Display::TableCell, Display::TableRow, Display::TableRow, Display::TableCell,
            Display::TableRowGroup, Display::TableRow, Display::TableCell].map(style);
        span(&mut styles[3], ROW_SPAN, 0);
        let parents = [None, Some(0), Some(1), Some(2), Some(1), Some(1), Some(5),
            Some(0), Some(7), Some(8)];
        let nodes = styles.iter().zip(parents).collect::<Vec<_>>();
        let grids = calculate(&nodes);
        assert_eq!(grids.len(), 1);
        let cell = |index| *grids[0].cells.iter().find(|cell| cell.index == index).unwrap();
        assert_eq!(cell(3).row_span, 3);
        assert_eq!(cell(6).column, 1);
        assert_eq!(cell(9).column, 0);
    }

    #[test]
    fn nested_table_cells_have_independent_occupancy() {
        let mut styles = [Display::Table, Display::TableRow, Display::TableCell,
            Display::Table, Display::TableRow, Display::TableCell, Display::TableRow,
            Display::TableCell].map(style);
        span(&mut styles[2], ROW_SPAN, 2);
        let parents = [None, Some(0), Some(1), Some(2), Some(3), Some(4), Some(0), Some(6)];
        let nodes = styles.iter().zip(parents).collect::<Vec<_>>();
        let grids = calculate(&nodes);
        assert_eq!(grids.len(), 2);
        assert_eq!(grids[0].table, 0);
        assert_eq!(grids[0].cells.iter().find(|cell| cell.index == 7).unwrap().column, 1);
        assert_eq!(grids[1].table, 3);
        assert_eq!(grids[1].cells[0].column, 0);
    }
}
