//! Retained paint output shared by every raster backend.
//!
//! This follows Blink's split between layout, PaintArtifact construction and
//! compositor consumption. The artifact owns immutable snapshots so scrolling
//! and raster scheduling never need to walk the application component tree.

use w3cos_std::color::Color;
use w3cos_std::component::ComponentKind;
use w3cos_std::style::{
    Display, Overflow, Position, Style, Transform2D, Visibility, WhiteSpace,
};

use crate::layout::LayoutRect;

pub type PropertyNodeId = usize;
pub type PaintChunkId = usize;
pub type PaintOrderLevel = (u8, i32, usize);

#[derive(Clone)]
pub struct PaintNode {
    pub kind: ComponentKind,
    pub style: Style,
    pub parent: Option<usize>,
    pub sticky_counter_signal: Option<usize>,
}

pub fn effective_z_order(style: &Style, inherited: i32) -> i32 {
    if style.z_index != 0 {
        style.z_index
    } else if matches!(
        style.position,
        Position::Relative | Position::Absolute | Position::Fixed | Position::Sticky
    ) {
        inherited.saturating_add(1)
    } else {
        inherited
    }
}

/// Rebuild paint nodes, cloning `Style` only for slots that actually changed.
///
/// Returns the node list and the number of Style clones performed. A clean
/// subtree (same length, same parent/kind/style) reuses the previous
/// allocation and reports 0 clones.
pub fn reuse_or_clone_paint_nodes<'a>(
    existing: Vec<PaintNode>,
    incoming: impl IntoIterator<Item = (&'a ComponentKind, &'a Style, Option<usize>, Option<usize>)>,
) -> (Vec<PaintNode>, usize) {
    let incoming: Vec<_> = incoming.into_iter().collect();
    if existing.len() != incoming.len() {
        let clones = incoming.len();
        let nodes = incoming
            .into_iter()
            .map(|(kind, style, parent, sticky)| PaintNode {
                kind: kind.clone(),
                style: style.clone(),
                parent,
                sticky_counter_signal: sticky,
            })
            .collect();
        return (nodes, clones);
    }
    let mut existing: Vec<Option<PaintNode>> = existing.into_iter().map(Some).collect();
    let mut out = Vec::with_capacity(incoming.len());
    let mut clones = 0;
    for (i, (kind, style, parent, sticky)) in incoming.into_iter().enumerate() {
        let reusable = existing[i].as_ref().is_some_and(|old| {
            old.parent == parent
                && old.sticky_counter_signal == sticky
                && old.kind == *kind
                && (std::ptr::eq(&old.style, style) || old.style == *style)
        });
        if reusable {
            out.push(existing[i].take().expect("paint node slot"));
        } else {
            clones += 1;
            out.push(PaintNode {
                kind: kind.clone(),
                style: style.clone(),
                parent,
                sticky_counter_signal: sticky,
            });
        }
    }
    (out, clones)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PaintProperties {
    pub transform: PropertyNodeId,
    pub clip: PropertyNodeId,
    pub effect: PropertyNodeId,
    pub scroll: PropertyNodeId,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransformNode {
    pub parent: PropertyNodeId,
    pub transform: Transform2D,
}

#[derive(Clone, Copy, Debug)]
pub struct ClipNode {
    pub parent: PropertyNodeId,
    pub rect: Option<LayoutRect>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EffectNode {
    pub parent: PropertyNodeId,
    pub opacity: f32,
    pub filter: Option<String>,
}

#[derive(Clone, Copy, Debug)]
pub struct ScrollNode {
    pub parent: PropertyNodeId,
    pub host_index: Option<usize>,
    pub scrollport: Option<LayoutRect>,
}

#[derive(Clone, Debug)]
pub struct PropertyTrees {
    pub transforms: Vec<TransformNode>,
    pub clips: Vec<ClipNode>,
    pub effects: Vec<EffectNode>,
    pub scrolls: Vec<ScrollNode>,
}

impl Default for PropertyTrees {
    fn default() -> Self {
        Self {
            transforms: vec![TransformNode {
                parent: 0,
                transform: Transform2D::IDENTITY,
            }],
            clips: vec![ClipNode {
                parent: 0,
                rect: None,
            }],
            effects: vec![EffectNode {
                parent: 0,
                opacity: 1.0,
                filter: None,
            }],
            scrolls: vec![ScrollNode {
                parent: 0,
                host_index: None,
                scrollport: None,
            }],
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DisplayItem {
    pub client_index: usize,
    pub visual_rect: LayoutRect,
    pub chunk_id: PaintChunkId,
}

#[derive(Clone, Copy, Debug)]
pub struct PaintChunk {
    pub begin: usize,
    pub end: usize,
    pub bounds: LayoutRect,
    pub properties: PaintProperties,
    pub z_order: i32,
}

#[derive(Clone)]
pub struct PaintArtifact {
    pub nodes: Vec<PaintNode>,
    pub canvas_background: Color,
    pub canvas_background_style: Option<Style>,
    pub canvas_background_source: Option<usize>,
    pub canvas_background_positioning_rect: Option<LayoutRect>,
    pub display_items: Vec<DisplayItem>,
    pub chunks: Vec<PaintChunk>,
    pub properties: PropertyTrees,
    pub node_properties: Vec<PaintProperties>,
    pub z_order: Vec<i32>,
    pub paint_order: Vec<Vec<PaintOrderLevel>>,
    pub sticky_owner: Vec<Option<usize>>,
    pub rect_by_index: Vec<Option<LayoutRect>>,
    pub generation: u64,
}

impl Default for PaintArtifact {
    fn default() -> Self {
        Self {
            nodes: Vec::new(),
            canvas_background: Color::WHITE,
            canvas_background_style: None,
            canvas_background_source: None,
            canvas_background_positioning_rect: None,
            display_items: Vec::new(),
            chunks: Vec::new(),
            properties: PropertyTrees::default(),
            node_properties: Vec::new(),
            z_order: Vec::new(),
            paint_order: Vec::new(),
            sticky_owner: Vec::new(),
            rect_by_index: Vec::new(),
            generation: 0,
        }
    }
}

fn background_position_uses_relative_basis(position: &str) -> bool {
    position.split_ascii_whitespace().any(|token| {
        token.contains('%')
            || matches!(
                token.to_ascii_lowercase().as_str(),
                "center" | "right" | "bottom"
            )
    })
}

fn inline_fragment_clip_rect(
    kind: &ComponentKind,
    style: &Style,
    rect: LayoutRect,
) -> Option<LayoutRect> {
    let internal_clip = style
        .custom_properties
        .as_ref()
        .and_then(|properties| properties.get("--w3cos-internal-inline-fragment-clip"))
        .and_then(|value| {
            let mut parts = value.split_ascii_whitespace();
            let alignment = parts.next()?;
            let height = parts.next()?.parse::<f32>().ok()?;
            Some((alignment, height))
        });
    let (alignment, height) = internal_clip.or_else(|| {
        if matches!(
            kind,
            ComponentKind::Image { .. }
                | ComponentKind::Canvas { .. }
                | ComponentKind::SvgDocument { .. }
        ) {
            return None;
        }
        if style.display != w3cos_std::style::Display::Inline {
            return None;
        }
        let alignment = match style.align_self {
            w3cos_std::style::AlignSelf::FlexStart => "top",
            w3cos_std::style::AlignSelf::FlexEnd => "bottom",
            _ => return None,
        };
        Some((alignment, style.font_size * style.line_height))
    })?;
    let height = height.clamp(0.0, rect.height);
    if height >= rect.height {
        return None;
    }
    Some(LayoutRect {
        x: rect.x,
        y: if alignment.eq_ignore_ascii_case("bottom") {
            rect.y + rect.height - height
        } else {
            rect.y
        },
        width: rect.width,
        height,
    })
}

fn css2_clip_rect(style: &Style, rect: LayoutRect) -> Option<LayoutRect> {
    if !matches!(style.position, Position::Absolute | Position::Fixed) {
        return None;
    }
    let clip = style.clip?;
    let resolve = |side: Option<w3cos_std::style::Dimension>, auto: f32| match side {
        Some(w3cos_std::style::Dimension::Px(value)) => value,
        Some(w3cos_std::style::Dimension::Rem(value)) => value * 16.0,
        Some(w3cos_std::style::Dimension::Em(value)) => value * style.font_size,
        Some(w3cos_std::style::Dimension::Ch(value)) => value * style.font_size * 0.5,
        Some(w3cos_std::style::Dimension::Vw(value)) => value * rect.width / 100.0,
        Some(w3cos_std::style::Dimension::Vh(value)) => value * rect.height / 100.0,
        Some(w3cos_std::style::Dimension::Percent(value)) => value * auto / 100.0,
        Some(w3cos_std::style::Dimension::Auto) | None => auto,
    };
    let top = resolve(clip.top, 0.0);
    let right = resolve(clip.right, rect.width);
    let bottom = resolve(clip.bottom, rect.height);
    let left = resolve(clip.left, 0.0);
    Some(LayoutRect {
        x: rect.x + left,
        y: rect.y + top,
        width: (right - left).max(0.0),
        height: (bottom - top).max(0.0),
    })
}

fn suppress_improper_nested_table_part_backgrounds(nodes: &mut [PaintNode]) {
    for index in 0..nodes.len() {
        if !matches!(
            nodes[index].style.display,
            Display::TableRowGroup
                | Display::TableHeaderGroup
                | Display::TableFooterGroup
                | Display::TableRow
                | Display::TableColumnGroup
                | Display::TableColumn
        ) {
            continue;
        }
        let mut parent = nodes[index].parent;
        let mut nested_in_cell = false;
        while let Some(parent_index) = parent {
            match nodes[parent_index].style.display {
                Display::TableCell => {
                    nested_in_cell = true;
                    break;
                }
                Display::Table | Display::InlineTable => break,
                _ => parent = nodes[parent_index].parent,
            }
        }
        if nested_in_cell {
            // CSS table fixup may place an improper internal table part inside
            // an anonymous cell. Row/row-group/column backgrounds are table
            // layer backgrounds, not independent box fills, so without cells
            // of their own they contribute no painted area.
            nodes[index].style.background = Color::TRANSPARENT;
            nodes[index].style.background_image = None;
        }
    }
}

fn suppress_hidden_empty_cell_paint(nodes: &mut [PaintNode]) {
    let has_child = nodes
        .iter()
        .filter_map(|node| node.parent)
        .collect::<std::collections::HashSet<_>>();
    for (index, node) in nodes.iter_mut().enumerate() {
        if node.style.display != Display::TableCell
            || node.style.border_collapse
            || !node.style.empty_cells_hide
            || has_child.contains(&index)
        {
            continue;
        }
        node.style.background = Color::TRANSPARENT;
        node.style.background_image = None;
        node.style.border_width = 0.0;
        node.style.border_top_width = None;
        node.style.border_right_width = None;
        node.style.border_bottom_width = None;
        node.style.border_left_width = None;
    }
}

fn trim_collapsible_inline_whitespace_at_line_start(
    nodes: &mut [PaintNode],
    rect_by_index: &[Option<LayoutRect>],
) {
    for index in 0..nodes.len() {
        let node = &nodes[index];
        if node.style.display != Display::Inline
            || matches!(node.style.white_space, WhiteSpace::Pre | WhiteSpace::PreWrap)
        {
            continue;
        }
        let (Some(parent), Some(rect)) = (
            node.parent,
            rect_by_index.get(index).copied().flatten(),
        ) else {
            continue;
        };
        let Some(parent_rect) = rect_by_index.get(parent).copied().flatten() else {
            continue;
        };
        let Some(parent_style) = nodes.get(parent).map(|parent| &parent.style) else {
            continue;
        };
        let line_start = parent_rect.x
            + parent_style
                .border_left_width
                .unwrap_or(parent_style.border_width)
            + parent_style.padding_lengths().left;
        if (rect.x - line_start).abs() > 0.01 {
            continue;
        }
        let ComponentKind::Text { content } = &mut nodes[index].kind else {
            continue;
        };
        let trimmed = content.trim_start_matches([' ', '\t', '\n', '\r', '\u{000c}']);
        if trimmed.len() != content.len() {
            *content = trimmed.to_string();
        }
    }
}

const TABLE_BACKGROUND_FRAGMENTS: &str = "--w3cos-internal-table-background-fragments";

fn annotate_separated_table_background_fragments(
    nodes: &mut [PaintNode],
    rect_by_index: &[Option<LayoutRect>],
) {
    fn nearest_table(nodes: &[PaintNode], index: usize) -> Option<usize> {
        let mut parent = nodes[index].parent;
        while let Some(parent_index) = parent {
            if matches!(
                nodes[parent_index].style.display,
                Display::Table | Display::InlineTable
            ) {
                return Some(parent_index);
            }
            parent = nodes[parent_index].parent;
        }
        None
    }
    fn column_span(style: &Style) -> usize {
        style
            .custom_properties
            .as_ref()
            .and_then(|properties| properties.get("--w3cos-internal-table-column-span"))
            .and_then(|span| span.parse::<usize>().ok())
            .filter(|span| *span > 0)
            .unwrap_or(1)
            .min(1000)
    }
    fn descendant_of(nodes: &[PaintNode], mut index: usize, ancestor: usize) -> bool {
        while let Some(parent) = nodes[index].parent {
            if parent == ancestor {
                return true;
            }
            index = parent;
        }
        false
    }

    let original_len = nodes.len();
    for source in 0..original_len {
        if !matches!(
            nodes[source].style.display,
            Display::TableColumnGroup | Display::TableColumn
        ) || (nodes[source].style.background.a == 0
            && nodes[source].style.background_image.is_none())
        {
            continue;
        }
        let Some(table) = nearest_table(nodes, source) else {
            continue;
        };
        let table_has_cells = (0..original_len).any(|index| {
            nodes[index].style.display == Display::TableCell
                && nearest_table(nodes, index) == Some(table)
        });
        if !table_has_cells {
            nodes[source].style.background = Color::TRANSPARENT;
            nodes[source].style.background_image = None;
            continue;
        }
        if nodes[table].style.border_collapse {
            continue;
        }
        let columns = (0..original_len)
            .filter(|index| {
                nodes[*index].style.display == Display::TableColumn
                    && nearest_table(nodes, *index) == Some(table)
            })
            .collect::<Vec<_>>();
        let covered = columns
            .iter()
            .enumerate()
            .filter(|(_, column)| {
                source == **column
                    || (nodes[source].style.display == Display::TableColumnGroup
                        && descendant_of(nodes, **column, source))
            })
            .map(|(column, _)| column)
            .collect::<std::collections::HashSet<_>>();
        if covered.is_empty() {
            continue;
        }
        let rows = (0..original_len)
            .filter(|index| {
                nodes[*index].style.display == Display::TableRow
                    && nearest_table(nodes, *index) == Some(table)
            })
            .collect::<Vec<_>>();
        let mut fragments = Vec::new();
        for row in rows {
            let cells = (0..original_len)
                .filter(|index| {
                    nodes[*index].parent == Some(row)
                        && nodes[*index].style.display == Display::TableCell
                })
                .collect::<Vec<_>>();
            let mut column = 0usize;
            for cell in cells {
                let span = column_span(&nodes[cell].style);
                if (column..column.saturating_add(span)).any(|index| covered.contains(&index))
                    && let Some(rect) = rect_by_index.get(cell).copied().flatten()
                {
                    fragments.push(format!(
                        "{} {} {} {}",
                        rect.x, rect.y, rect.width, rect.height
                    ));
                }
                column += span;
            }
        }
        if !fragments.is_empty() {
            nodes[source]
                .style
                .custom_properties
                .get_or_insert_with(Default::default)
                .insert(TABLE_BACKGROUND_FRAGMENTS.to_string(), fragments.join(";"));
        }
    }

    // Row and row-group backgrounds paint over their continuous row boxes,
    // including the separated-border spacing gutters. Only column layers
    // need per-cell fragments because columns have no principal CSS box.
}

fn project_collapsed_table_tracks_to_cells(nodes: &mut [PaintNode]) {
    fn column_span(style: &Style) -> usize {
        style
            .custom_properties
            .as_ref()
            .and_then(|properties| properties.get("--w3cos-internal-table-column-span"))
            .and_then(|span| span.parse::<usize>().ok())
            .filter(|span| *span > 0)
            .unwrap_or(1)
            .min(1000)
    }
    fn nearest_table(nodes: &[PaintNode], index: usize) -> Option<usize> {
        let mut parent = nodes[index].parent;
        while let Some(parent_index) = parent {
            if matches!(
                nodes[parent_index].style.display,
                Display::Table | Display::InlineTable
            ) {
                return Some(parent_index);
            }
            parent = nodes[parent_index].parent;
        }
        None
    }
    let tables = nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| matches!(node.style.display, Display::Table | Display::InlineTable))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    for table in tables {
        let collapsed_columns = nodes
            .iter()
            .enumerate()
            .filter(|(index, node)| {
                node.style.display == Display::TableColumn
                    && nearest_table(nodes, *index) == Some(table)
            })
            .map(|(_, node)| node.style.visibility == Visibility::Collapse)
            .collect::<Vec<_>>();
        if !collapsed_columns.iter().any(|collapsed| *collapsed) {
            continue;
        }
        let rows = nodes
            .iter()
            .enumerate()
            .filter(|(index, node)| {
                node.style.display == Display::TableRow
                    && nearest_table(nodes, *index) == Some(table)
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        for row in rows {
            let cells = nodes
                .iter()
                .enumerate()
                .filter(|(_, node)| {
                    node.parent == Some(row) && node.style.display == Display::TableCell
                })
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            let mut column: usize = 0;
            for cell in cells {
                let span = column_span(&nodes[cell].style);
                let covered_columns = collapsed_columns
                    .get(column..column.saturating_add(span))
                    .unwrap_or_default();
                let any_collapsed = covered_columns.iter().any(|collapsed| *collapsed);
                let all_collapsed = !covered_columns.is_empty()
                    && covered_columns.iter().all(|collapsed| *collapsed);
                if all_collapsed {
                    nodes[cell].style.visibility = Visibility::Collapse;
                    nodes[cell].style.overflow_x = Some(Overflow::Hidden);
                    nodes[cell].style.overflow_y = Some(Overflow::Hidden);
                } else if any_collapsed {
                    nodes[cell].style.overflow_x = Some(Overflow::Hidden);
                }
                column = column.saturating_add(span);
            }
        }
    }
    let collapsed_rows = nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| {
            node.style.display == Display::TableRow && node.style.visibility == Visibility::Collapse
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    for row in collapsed_rows {
        for node in nodes
            .iter_mut()
            .filter(|node| node.parent == Some(row) && node.style.display == Display::TableCell)
        {
            node.style.visibility = Visibility::Collapse;
        }
    }
}

fn resolve_collapsed_cell_border_conflicts(nodes: &mut [PaintNode]) {
    use w3cos_std::style::TextDirection;

    fn edge(style: &Style, side: usize) -> (f32, Color) {
        let width = match side {
            0 => style.border_top_width,
            1 => style.border_right_width,
            2 => style.border_bottom_width,
            _ => style.border_left_width,
        }
        .unwrap_or(style.border_width);
        let color = match side {
            0 => style.border_top_color,
            1 => style.border_right_color,
            2 => style.border_bottom_color,
            _ => style.border_left_color,
        }
        .unwrap_or(style.border_color);
        (width, color)
    }
    fn set_edge(style: &mut Style, side: usize, width: f32, color: Color) {
        match side {
            0 => {
                style.border_top_width = Some(width);
                style.border_top_color = Some(color);
            }
            1 => {
                style.border_right_width = Some(width);
                style.border_right_color = Some(color);
            }
            2 => {
                style.border_bottom_width = Some(width);
                style.border_bottom_color = Some(color);
            }
            _ => {
                style.border_left_width = Some(width);
                style.border_left_color = Some(color);
            }
        }
    }
    fn suppress_edge(style: &mut Style, side: usize, width: f32) {
        const NAMES: [&str; 4] = ["top", "right", "bottom", "left"];
        set_edge(style, side, width, Color::TRANSPARENT);
        style
            .custom_properties
            .get_or_insert_with(Default::default)
            .entry(COLLAPSED_BORDER_SUPPRESSED.to_string())
            .and_modify(|value| {
                value.push(' ');
                value.push_str(NAMES[side]);
            })
            .or_insert_with(|| NAMES[side].to_string());
    }

    let rows = nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| node.style.display == Display::TableRow && node.style.border_collapse)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    fn row_cells(nodes: &[PaintNode], row: usize) -> Vec<usize> {
        nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| {
                node.parent == Some(row) && node.style.display == Display::TableCell
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>()
    }

    for row in &rows {
        let cells = row_cells(nodes, *row);
        for pair in cells.windows(2) {
            let left = pair[0];
            let right = pair[1];
            let left_edge = edge(&nodes[left].style, 1);
            let right_edge = edge(&nodes[right].style, 3);
            let left_collapsed = nodes[left].style.visibility == Visibility::Collapse;
            let right_collapsed = nodes[right].style.visibility == Visibility::Collapse;
            let left_wins = if left_collapsed != right_collapsed {
                !left_collapsed
            } else if left_edge.0 > right_edge.0 {
                true
            } else if right_edge.0 > left_edge.0 {
                false
            } else {
                nodes[*row].style.direction != TextDirection::Rtl
            };
            if left_wins {
                set_edge(&mut nodes[left].style, 1, left_edge.0, left_edge.1);
                suppress_edge(&mut nodes[right].style, 3, left_edge.0);
            } else {
                suppress_edge(&mut nodes[left].style, 1, right_edge.0);
                set_edge(&mut nodes[right].style, 3, right_edge.0, right_edge.1);
            }
        }
    }

    fn nearest_table(nodes: &[PaintNode], index: usize) -> Option<usize> {
        let mut parent = nodes[index].parent;
        while let Some(parent_index) = parent {
            if matches!(
                nodes[parent_index].style.display,
                Display::Table | Display::InlineTable
            ) {
                return Some(parent_index);
            }
            parent = nodes[parent_index].parent;
        }
        None
    }
    let tables = nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| {
            matches!(node.style.display, Display::Table | Display::InlineTable)
                && node.style.border_collapse
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    for table in tables {
        let table_rows = rows
            .iter()
            .copied()
            .filter(|row| nearest_table(nodes, *row) == Some(table))
            .collect::<Vec<_>>();
        let visible_rows = table_rows
            .iter()
            .copied()
            .filter(|row| nodes[*row].style.visibility != Visibility::Collapse)
            .collect::<Vec<_>>();
        let boundary_rows = if visible_rows.is_empty() {
            &table_rows
        } else {
            &visible_rows
        };
        let Some(first_row) = boundary_rows.first().copied() else {
            continue;
        };
        let last_row = boundary_rows.last().copied().unwrap_or(first_row);
        let mut boundary_cells = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        boundary_cells[0] = row_cells(nodes, first_row)
            .into_iter()
            .filter(|cell| nodes[*cell].style.visibility != Visibility::Collapse)
            .collect();
        boundary_cells[2] = row_cells(nodes, last_row)
            .into_iter()
            .filter(|cell| nodes[*cell].style.visibility != Visibility::Collapse)
            .collect();
        for row in boundary_rows {
            let cells = row_cells(nodes, *row)
                .into_iter()
                .filter(|cell| nodes[*cell].style.visibility != Visibility::Collapse)
                .collect::<Vec<_>>();
            if let Some(first) = cells.first() {
                boundary_cells[3].push(*first);
            }
            if let Some(last) = cells.last() {
                boundary_cells[1].push(*last);
            }
        }
        for side in 0..4 {
            let table_edge = edge(&nodes[table].style, side);
            for cell in &boundary_cells[side] {
                let cell_edge = edge(&nodes[*cell].style, side);
                let winner = if table_edge.0 > cell_edge.0 {
                    table_edge
                } else {
                    cell_edge
                };
                set_edge(&mut nodes[*cell].style, side, winner.0, winner.1);
            }
            // Boundary cells paint the resolved collapsed edge on the grid.
            // Leaving the table wrapper border active would inset and paint a
            // second ring around that same edge.
            suppress_edge(&mut nodes[table].style, side, table_edge.0);
        }
    }
    for pair in rows.windows(2) {
        if nearest_table(nodes, pair[0]) != nearest_table(nodes, pair[1]) {
            continue;
        }
        let top_cells = row_cells(nodes, pair[0]);
        let bottom_cells = row_cells(nodes, pair[1]);
        for (top, bottom) in top_cells.into_iter().zip(bottom_cells) {
            let top_edge = edge(&nodes[top].style, 2);
            let bottom_edge = edge(&nodes[bottom].style, 0);
            let top_collapsed = nodes[top].style.visibility == Visibility::Collapse;
            let bottom_collapsed = nodes[bottom].style.visibility == Visibility::Collapse;
            if (top_collapsed && !bottom_collapsed) || bottom_edge.0 > top_edge.0 {
                suppress_edge(&mut nodes[top].style, 2, bottom_edge.0);
                set_edge(&mut nodes[bottom].style, 0, bottom_edge.0, bottom_edge.1);
            } else {
                set_edge(&mut nodes[top].style, 2, top_edge.0, top_edge.1);
                suppress_edge(&mut nodes[bottom].style, 0, top_edge.0);
            }
        }
    }
}

const COLLAPSED_BORDER_SUPPRESSED: &str = "--w3cos-internal-collapsed-border-suppressed";
const COLLAPSED_BORDER_BOTTOM_EXTENSION: &str =
    "--w3cos-internal-collapsed-border-bottom-extension";

fn extend_collapsed_borders_across_empty_rows(
    nodes: &mut [PaintNode],
    rect_by_index: &[Option<LayoutRect>],
) {
    fn row_cells(nodes: &[PaintNode], row: usize) -> Vec<usize> {
        nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| {
                node.parent == Some(row) && node.style.display == Display::TableCell
            })
            .map(|(index, _)| index)
            .collect()
    }
    let rows = nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| node.style.display == Display::TableRow && node.style.border_collapse)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    for rows in rows.windows(3) {
        let top_cells = row_cells(nodes, rows[0]);
        let empty_cells = row_cells(nodes, rows[1]);
        let bottom_cells = row_cells(nodes, rows[2]);
        if top_cells.is_empty() || !empty_cells.is_empty() || bottom_cells.is_empty() {
            continue;
        }
        let extension = rect_by_index
            .get(rows[1])
            .and_then(|rect| *rect)
            .map_or(0.0, |rect| rect.height.max(0.0));
        if extension == 0.0 {
            continue;
        }
        for cell in top_cells {
            let bottom_width = nodes[cell]
                .style
                .border_bottom_width
                .unwrap_or(nodes[cell].style.border_width);
            let extension = extension.min(bottom_width);
            if extension > 0.0 {
                nodes[cell]
                    .style
                    .custom_properties
                    .get_or_insert_with(Default::default)
                    .insert(
                        COLLAPSED_BORDER_BOTTOM_EXTENSION.to_string(),
                        extension.to_string(),
                    );
            }
        }
    }
}

pub(crate) fn border_edge_paint_rects(
    style: &Style,
    rect: LayoutRect,
    widths: [f32; 4],
) -> [LayoutRect; 4] {
    if style.border_collapse
        && matches!(style.display, Display::TableColumn | Display::TableColumnGroup)
    {
        // Column boxes describe grid tracks, not inset border boxes. A
        // collapsed border is centered on the corresponding grid line.
        return [
            LayoutRect {
                x: rect.x,
                y: rect.y - widths[0] / 2.0,
                width: rect.width,
                height: widths[0],
            },
            LayoutRect {
                x: rect.x + rect.width - widths[1] / 2.0,
                y: rect.y,
                width: widths[1],
                height: rect.height,
            },
            LayoutRect {
                x: rect.x,
                y: rect.y + rect.height - widths[2] / 2.0,
                width: rect.width,
                height: widths[2],
            },
            LayoutRect {
                x: rect.x - widths[3] / 2.0,
                y: rect.y,
                width: widths[3],
                height: rect.height,
            },
        ];
    }
    let suppressed = |name: &str| collapsed_border_suppressed(style, name);
    let top = if suppressed("top") { widths[0] } else { 0.0 };
    let right = if suppressed("right") { widths[1] } else { 0.0 };
    let bottom = if suppressed("bottom") { widths[2] } else { 0.0 };
    let left = if suppressed("left") { widths[3] } else { 0.0 };
    let bottom_extension = style
        .custom_properties
        .as_ref()
        .and_then(|properties| properties.get(COLLAPSED_BORDER_BOTTOM_EXTENSION))
        .and_then(|value| value.parse::<f32>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(0.0);
    [
        LayoutRect {
            x: rect.x + left,
            y: rect.y,
            width: (rect.width - left - right).max(0.0),
            height: widths[0],
        },
        LayoutRect {
            x: rect.x + rect.width - widths[1],
            y: rect.y + top,
            width: widths[1],
            height: (rect.height - top - bottom).max(0.0),
        },
        LayoutRect {
            x: rect.x + left,
            y: rect.y + rect.height - widths[2],
            width: (rect.width - left - right).max(0.0),
            height: widths[2] + bottom_extension,
        },
        LayoutRect {
            x: rect.x,
            y: rect.y + top,
            width: widths[3],
            height: (rect.height - top - bottom).max(0.0),
        },
    ]
}

fn collapsed_border_suppressed(style: &Style, name: &str) -> bool {
    style
        .custom_properties
        .as_ref()
        .and_then(|properties| properties.get(COLLAPSED_BORDER_SUPPRESSED))
        .is_some_and(|value| value.split_ascii_whitespace().any(|side| side == name))
}

pub(crate) fn box_background_paint_rect(style: &Style, rect: LayoutRect) -> LayoutRect {
    if style.border_collapse && style.display == Display::TableCell {
        let top = style.border_top_width.unwrap_or(style.border_width) / 2.0;
        let right = style.border_right_width.unwrap_or(style.border_width) / 2.0;
        let bottom = style.border_bottom_width.unwrap_or(style.border_width) / 2.0;
        let left = style.border_left_width.unwrap_or(style.border_width) / 2.0;
        return LayoutRect {
            x: rect.x + left,
            y: rect.y + top,
            width: (rect.width - left - right).max(0.0),
            height: (rect.height - top - bottom).max(0.0),
        };
    }
    if style.border_collapse && matches!(style.display, Display::Table | Display::InlineTable) {
        return rect;
    }
    let top = collapsed_border_suppressed(style, "top")
        .then(|| style.border_top_width.unwrap_or(style.border_width))
        .unwrap_or(0.0);
    let right = collapsed_border_suppressed(style, "right")
        .then(|| style.border_right_width.unwrap_or(style.border_width))
        .unwrap_or(0.0);
    let bottom = collapsed_border_suppressed(style, "bottom")
        .then(|| style.border_bottom_width.unwrap_or(style.border_width))
        .unwrap_or(0.0);
    let left = collapsed_border_suppressed(style, "left")
        .then(|| style.border_left_width.unwrap_or(style.border_width))
        .unwrap_or(0.0);
    LayoutRect {
        x: rect.x + left,
        y: rect.y + top,
        width: (rect.width - left - right).max(0.0),
        height: (rect.height - top - bottom).max(0.0),
    }
}

pub(crate) fn box_background_paint_rects(style: &Style, rect: LayoutRect) -> Vec<LayoutRect> {
    let Some(fragments) = style
        .custom_properties
        .as_ref()
        .and_then(|properties| properties.get(TABLE_BACKGROUND_FRAGMENTS))
    else {
        return vec![box_background_paint_rect(style, rect)];
    };
    let rects = fragments
        .split(';')
        .filter_map(|fragment| {
            let mut values = fragment
                .split_ascii_whitespace()
                .filter_map(|value| value.parse::<f32>().ok());
            Some(LayoutRect {
                x: values.next()?,
                y: values.next()?,
                width: values.next()?.max(0.0),
                height: values.next()?.max(0.0),
            })
        })
        .collect::<Vec<_>>();
    if rects.is_empty() {
        vec![box_background_paint_rect(style, rect)]
    } else {
        rects
    }
}

pub(crate) fn box_background_positioning_rect(
    style: &Style,
    rect: LayoutRect,
) -> Option<LayoutRect> {
    if !style.border_collapse || !matches!(style.display, Display::Table | Display::InlineTable) {
        return None;
    }
    let top = style.border_top_width.unwrap_or(style.border_width) / 2.0;
    let right = style.border_right_width.unwrap_or(style.border_width) / 2.0;
    let bottom = style.border_bottom_width.unwrap_or(style.border_width) / 2.0;
    let left = style.border_left_width.unwrap_or(style.border_width) / 2.0;
    Some(LayoutRect {
        x: rect.x + left,
        y: rect.y + top,
        width: (rect.width - left - right).max(0.0),
        height: (rect.height - top - bottom).max(0.0),
    })
}

const TABLE_CAPTION_INSETS: &str = "--w3cos-internal-table-caption-insets";

pub(crate) fn table_grid_paint_rect(style: &Style, mut rect: LayoutRect) -> LayoutRect {
    let Some(value) = style
        .custom_properties
        .as_ref()
        .and_then(|properties| properties.get(TABLE_CAPTION_INSETS))
    else {
        return rect;
    };
    let mut parts = value.split_ascii_whitespace();
    let Some(top) = parts.next().and_then(|value| value.parse::<f32>().ok()) else {
        return rect;
    };
    let Some(bottom) = parts.next().and_then(|value| value.parse::<f32>().ok()) else {
        return rect;
    };
    let scale_y = style.transform.scale_y;
    let top = top * scale_y;
    let bottom = bottom * scale_y;
    rect.y += top;
    rect.height = (rect.height - top - bottom).max(0.0);
    rect
}

fn annotate_table_caption_paint_insets(
    nodes: &mut [PaintNode],
    rect_by_index: &[Option<LayoutRect>],
) {
    let mut insets = vec![(0.0_f32, 0.0_f32); nodes.len()];
    for (index, node) in nodes.iter().enumerate() {
        if node.style.display != Display::TableCaption {
            continue;
        }
        let Some(parent) = node.parent else {
            continue;
        };
        if !matches!(
            nodes[parent].style.display,
            Display::Table | Display::InlineTable
        ) {
            continue;
        }
        let height = rect_by_index
            .get(index)
            .copied()
            .flatten()
            .map_or(0.0, |rect| rect.height);
        if node.style.caption_side_bottom {
            insets[parent].1 += height;
        } else {
            insets[parent].0 += height;
        }
    }
    for (index, (top, bottom)) in insets.into_iter().enumerate() {
        if top <= 0.0 && bottom <= 0.0 {
            continue;
        }
        nodes[index]
            .style
            .custom_properties
            .get_or_insert_with(Default::default)
            .insert(TABLE_CAPTION_INSETS.to_string(), format!("{top} {bottom}"));
    }
}

impl PaintArtifact {
    pub fn paint_order_key(&self, index: usize) -> &[PaintOrderLevel] {
        self.paint_order.get(index).map_or(&[], Vec::as_slice)
    }

    /// CSS2 paint phase within one effective stack level. In-flow block
    /// backgrounds paint before floats, and inline content paints after
    /// floats. A float's descendants stay in the float phase as one atomic
    /// paint group.
    pub fn css2_paint_phase(&self, index: usize) -> u8 {
        let mut cursor = Some(index);
        while let Some(current) = cursor {
            let Some(node) = self.nodes.get(current) else {
                break;
            };
            if matches!(
                node.style.position,
                Position::Relative | Position::Absolute | Position::Fixed | Position::Sticky
            ) {
                // Positioned subtrees are atomic at their effective stack
                // level. Do not lift inline descendants above a later
                // positioned sibling merely because they are inline content.
                return 0;
            }
            if node.style.float != w3cos_std::style::Float::None {
                return 1;
            }
            if current != index
                && matches!(
                    node.style.display,
                    Display::InlineBlock | Display::InlineFlex | Display::InlineTable
                )
            {
                // Atomic inline-level boxes paint their entire subtree in the
                // inline phase. A block child must not be globally sorted
                // ahead of its inline-block parent's background.
                return 2;
            }
            cursor = node.parent;
        }
        self.nodes.get(index).map_or(0, |node| {
            u8::from(matches!(
                node.style.display,
                Display::Inline | Display::InlineBlock | Display::InlineFlex | Display::InlineTable
            )) * 2
        })
    }

    pub fn build(
        nodes: impl IntoIterator<Item = PaintNode>,
        layout_cache: &[(LayoutRect, usize)],
        generation: u64,
    ) -> Self {
        Self::build_with_body_background(nodes, layout_cache, generation, None)
    }

    pub fn build_with_body_background(
        nodes: impl IntoIterator<Item = PaintNode>,
        layout_cache: &[(LayoutRect, usize)],
        generation: u64,
        body_index: Option<usize>,
    ) -> Self {
        let mut nodes: Vec<_> = nodes.into_iter().collect();
        let has_background_image = |style: &Style| {
            style.background_image.as_deref().is_some_and(|value| {
                value
                    .split(',')
                    .any(|layer| !layer.trim().eq_ignore_ascii_case("none"))
            })
        };
        let canvas_background_source = nodes
            .first()
            .filter(|node| node.style.background.a > 0 || has_background_image(&node.style))
            .map(|_| 0)
            .or_else(|| {
                body_index.filter(|index| {
                    *index != 0
                        && nodes.get(*index).is_some_and(|node| {
                            node.parent == Some(0)
                                && (node.style.background.a > 0
                                    || has_background_image(&node.style))
                        })
                })
            });
        let root_rect = layout_cache
            .iter()
            .find_map(|(rect, index)| (*index == 0).then_some(*rect));
        let root_style = &nodes[0].style;
        let root_borders = (
            root_style
                .border_left_width
                .unwrap_or(root_style.border_width),
            root_style
                .border_top_width
                .unwrap_or(root_style.border_width),
            root_style
                .border_right_width
                .unwrap_or(root_style.border_width),
            root_style
                .border_bottom_width
                .unwrap_or(root_style.border_width),
        );
        let root_positioning_rect = root_rect.map(|rect| LayoutRect {
            x: rect.x + root_borders.0,
            y: rect.y + root_borders.1,
            width: (rect.width - root_borders.0 - root_borders.2).max(0.0),
            height: (rect.height - root_borders.1 - root_borders.3).max(0.0),
        });
        let canvas_background_style = canvas_background_source.map(|index| {
            let mut style = nodes[index].style.clone();
            style.border_width = 0.0;
            style.border_top_width = None;
            style.border_right_width = None;
            style.border_bottom_width = None;
            style.border_left_width = None;
            if style.background_position.as_deref().is_none_or(|position| {
                matches!(
                    position.trim().to_ascii_lowercase().as_str(),
                    "" | "0% 0%" | "top left" | "left top"
                )
            }) {
                let (x, y) = if index == 0 {
                    // The positioning override already is the root padding box.
                    (0.0, 0.0)
                } else {
                    // A propagated body background uses the document element's
                    // padding edge as its canvas positioning origin.
                    (
                        root_positioning_rect.map_or(0.0, |rect| rect.x),
                        root_positioning_rect.map_or(0.0, |rect| rect.y),
                    )
                };
                style.background_position = Some(format!("{x}px {y}px"));
            }
            style
        });
        let canvas_background = canvas_background_source
            .map(|index| nodes[index].style.background)
            .filter(|color| color.a > 0)
            .unwrap_or(Color::WHITE);
        // CSS paints a propagated body background on the canvas as if it
        // were specified on the root element. Relative positions need the
        // root box as their percentage basis; absolute positions have already
        // been converted to canvas coordinates above.
        let canvas_background_positioning_rect = canvas_background_source
            .filter(|index| {
                *index == 0
                    || nodes[*index]
                        .style
                        .background_position
                        .as_deref()
                        .is_some_and(background_position_uses_relative_basis)
            })
            .and(root_positioning_rect);
        if let Some(index) = canvas_background_source {
            nodes[index].style.background = Color::TRANSPARENT;
            nodes[index].style.background_image = None;
        }
        let mut rect_by_index = vec![None; nodes.len()];
        for &(rect, index) in layout_cache {
            if let Some(slot) = rect_by_index.get_mut(index) {
                *slot = Some(rect);
            }
        }
        trim_collapsible_inline_whitespace_at_line_start(&mut nodes, &rect_by_index);
        suppress_improper_nested_table_part_backgrounds(&mut nodes);
        suppress_hidden_empty_cell_paint(&mut nodes);
        project_collapsed_table_tracks_to_cells(&mut nodes);
        resolve_collapsed_cell_border_conflicts(&mut nodes);
        extend_collapsed_borders_across_empty_rows(&mut nodes, &rect_by_index);
        annotate_separated_table_background_fragments(&mut nodes, &rect_by_index);
        let mut artifact = Self {
            rect_by_index,
            node_properties: vec![PaintProperties::default(); nodes.len()],
            z_order: vec![0; nodes.len()],
            paint_order: vec![Vec::new(); nodes.len()],
            sticky_owner: vec![None; nodes.len()],
            nodes,
            canvas_background,
            canvas_background_style,
            canvas_background_source,
            canvas_background_positioning_rect,
            generation,
            ..Self::default()
        };
        annotate_table_caption_paint_insets(&mut artifact.nodes, &artifact.rect_by_index);

        for index in 0..artifact.nodes.len() {
            artifact.append_node(index);
        }
        artifact
    }

    fn append_node(&mut self, index: usize) {
        let node = &self.nodes[index];
        let mut inherited = node
            .parent
            .and_then(|parent| self.node_properties.get(parent).copied())
            .unwrap_or_default();
        if matches!(node.style.position, Position::Absolute | Position::Fixed) {
            // Overflow clips between an out-of-flow box and its containing
            // block do not clip that box. Inherit the clip chain from the
            // nearest positioned containing-block ancestor, not blindly from
            // the DOM parent; fixed boxes use the viewport chain.
            let positioned_ancestor = if matches!(node.style.position, Position::Fixed) {
                None
            } else {
                let mut ancestor = node.parent;
                let mut owner = None;
                while let Some(index) = ancestor {
                    if !matches!(self.nodes[index].style.position, Position::Static) {
                        owner = Some(index);
                        break;
                    }
                    ancestor = self.nodes[index].parent;
                }
                owner
            };
            inherited.clip = positioned_ancestor
                .and_then(|owner| {
                    self.node_properties
                        .get(owner)
                        .map(|properties| properties.clip)
                })
                .unwrap_or_default();
        }
        let inherited_z = node
            .parent
            .and_then(|parent| {
                if self.nodes[parent].parent.is_none() {
                    Some(0)
                } else {
                    self.z_order.get(parent).copied()
                }
            })
            .unwrap_or_default();
        // The root box establishes the root stacking context. Its own
        // background and border paint below every descendant, including a
        // positioned descendant with a negative z-index.
        self.z_order[index] = if node.parent.is_none() {
            i32::MIN
        } else if matches!(node.style.position, Position::Fixed) && node.style.z_index == 0 {
            // A fixed auto box participates at stack level zero in the root
            // context. Positioned ancestors with z-index:auto do not create a
            // context, so their synthetic paint rank must not accumulate.
            1
        } else {
            effective_z_order(&node.style, inherited_z)
        };
        // A fixed-positioned box establishes a stacking context even when
        // z-index is auto. Negative descendants therefore paint above the
        // fixed box's own background instead of escaping behind it.
        let fixed_context_floor = node.parent.and_then(|mut ancestor| {
            loop {
                let ancestor_node = &self.nodes[ancestor];
                if matches!(ancestor_node.style.position, Position::Fixed)
                    && ancestor_node.style.z_index == 0
                {
                    break self
                        .z_order
                        .get(ancestor)
                        .copied()
                        .map(|z| z.saturating_add(1));
                }
                match ancestor_node.parent {
                    Some(parent) => ancestor = parent,
                    None => break None,
                }
            }
        });
        if let Some(floor) = fixed_context_floor {
            self.z_order[index] = self.z_order[index].max(floor);
        }
        self.paint_order[index] = self.hierarchical_paint_order(index);
        self.sticky_owner[index] = if matches!(node.style.position, Position::Sticky) {
            Some(index)
        } else {
            node.parent.and_then(|parent| self.sticky_owner[parent])
        };

        let mut properties = inherited;
        if !node.style.transform.is_identity() {
            properties.transform = self.properties.transforms.len();
            self.properties.transforms.push(TransformNode {
                parent: inherited.transform,
                transform: node.style.transform,
            });
        }
        let overflow_x = node.style.resolved_overflow_x();
        let overflow_y = node.style.resolved_overflow_y();
        if matches!(
            overflow_x,
            Overflow::Hidden | Overflow::Scroll | Overflow::Auto
        ) || matches!(
            overflow_y,
            Overflow::Hidden | Overflow::Scroll | Overflow::Auto
        ) {
            properties.clip = self.properties.clips.len();
            self.properties.clips.push(ClipNode {
                parent: inherited.clip,
                rect: self.rect_by_index[index],
            });
        }
        if let Some(rect) = self.rect_by_index[index]
            && let Some(css_clip) = css2_clip_rect(&node.style, rect)
        {
            let parent = properties.clip;
            properties.clip = self.properties.clips.len();
            self.properties.clips.push(ClipNode {
                parent,
                rect: Some(css_clip),
            });
        }
        if let Some(rect) = self.rect_by_index[index]
            && let Some(fragment_clip) = inline_fragment_clip_rect(&node.kind, &node.style, rect)
        {
            let parent = properties.clip;
            properties.clip = self.properties.clips.len();
            self.properties.clips.push(ClipNode {
                parent,
                rect: Some(fragment_clip),
            });
        }
        if node.style.opacity < 0.999 || node.style.filter.is_some() {
            properties.effect = self.properties.effects.len();
            self.properties.effects.push(EffectNode {
                parent: inherited.effect,
                opacity: node.style.opacity,
                filter: node.style.filter.clone(),
            });
        }
        if matches!(overflow_x, Overflow::Scroll | Overflow::Auto)
            || matches!(overflow_y, Overflow::Scroll | Overflow::Auto)
        {
            properties.scroll = self.properties.scrolls.len();
            self.properties.scrolls.push(ScrollNode {
                parent: inherited.scroll,
                host_index: Some(index),
                scrollport: self.rect_by_index[index],
            });
        }
        self.node_properties[index] = properties;

        if node.style.visibility != Visibility::Visible {
            return;
        }

        let Some(bounds) = self.rect_by_index[index] else {
            return;
        };
        let item_index = self.display_items.len();
        let chunk_id = self.chunks.len();
        self.display_items.push(DisplayItem {
            client_index: index,
            visual_rect: bounds,
            chunk_id,
        });
        self.chunks.push(PaintChunk {
            begin: item_index,
            end: item_index + 1,
            bounds,
            properties,
            z_order: self.z_order[index],
        });
    }

    fn hierarchical_paint_order(&self, index: usize) -> Vec<PaintOrderLevel> {
        let node = &self.nodes[index];
        if node.parent.is_none() {
            return vec![(0, i32::MIN, index)];
        }

        // Auto positioned boxes group their normal contents, but positioned
        // descendants participate in the nearest real stacking context.
        let mut parent = node.parent;
        if is_positioned(&node.style) {
            while let Some(current) = parent {
                if self.nodes[current].parent.is_none()
                    || establishes_stacking_context(&self.nodes[current])
                {
                    break;
                }
                parent = self.nodes[current].parent;
            }
        }
        let mut key = parent
            .and_then(|parent| self.paint_context_prefix(parent))
            .unwrap_or_default();
        key.push(self.local_paint_order_level(index));
        if establishes_stacking_context(node) || is_positioned(&node.style) {
            // A context's own background is the first item inside that
            // context. Descendant keys extend the prefix before adding their
            // local CSS2 paint phase.
            key.push((0, i32::MIN, index));
        }
        key
    }

    fn paint_context_prefix(&self, index: usize) -> Option<Vec<PaintOrderLevel>> {
        let node = self.nodes.get(index)?;
        if node.parent.is_none() || establishes_stacking_context(node) || is_positioned(&node.style) {
            let mut key = self.paint_order.get(index)?.clone();
            if establishes_stacking_context(node) || is_positioned(&node.style) {
                key.pop();
            }
            return Some(key);
        }
        node.parent
            .and_then(|parent| self.paint_context_prefix(parent))
    }

    fn local_paint_order_level(&self, index: usize) -> PaintOrderLevel {
        let node = &self.nodes[index];
        if is_positioned(&node.style) {
            return match node.style.z_index.cmp(&0) {
                std::cmp::Ordering::Less => (0, node.style.z_index, index),
                std::cmp::Ordering::Equal => (4, 0, index),
                std::cmp::Ordering::Greater => (5, node.style.z_index, index),
            };
        }

        let collapsed_table_part_border = node.style.border_collapse
            && node.style.background.a == 0
            && matches!(
                node.style.display,
                Display::TableRowGroup
                    | Display::TableHeaderGroup
                    | Display::TableFooterGroup
                    | Display::TableRow
                    | Display::TableColumnGroup
                    | Display::TableColumn
            )
            && (node.style.border_width > 0.0
                || node.style.border_top_width.is_some_and(|width| width > 0.0)
                || node.style.border_right_width.is_some_and(|width| width > 0.0)
                || node.style.border_bottom_width.is_some_and(|width| width > 0.0)
                || node.style.border_left_width.is_some_and(|width| width > 0.0));
        if collapsed_table_part_border {
            // Collapsed table borders paint over cell contents. Transparent
            // table-part backgrounds can therefore use a late display item
            // without disturbing the table background-layer ordering.
            return (4, 0, index);
        }

        let mut cursor = Some(index);
        while let Some(current) = cursor {
            let current_node = &self.nodes[current];
            if current != index
                && is_positioned(&current_node.style)
                && !establishes_stacking_context(current_node)
            {
                // The hierarchical prefix keeps this normal subtree in the
                // positioned participant; retain its local CSS paint phases.
                break;
            }
            if current_node.style.float != w3cos_std::style::Float::None {
                return (2, 0, index);
            }
            if current != index
                && matches!(
                    current_node.style.display,
                    Display::InlineBlock | Display::InlineFlex | Display::InlineTable
                )
            {
                return (3, 0, index);
            }
            if current != index && establishes_stacking_context(current_node) {
                break;
            }
            cursor = current_node.parent;
        }

        let phase = if matches!(
            node.style.display,
            Display::Inline | Display::InlineBlock | Display::InlineFlex | Display::InlineTable
        ) {
            3
        } else {
            1
        };
        (phase, 0, index)
    }
}

fn is_positioned(style: &Style) -> bool {
    matches!(
        style.position,
        Position::Relative | Position::Absolute | Position::Fixed | Position::Sticky
    )
}

fn establishes_stacking_context(node: &PaintNode) -> bool {
    let specifies_z_index = node.style.z_index != 0
        || node
            .style
            .custom_properties
            .as_ref()
            .is_some_and(|properties| {
                properties.contains_key("--w3cos-internal-z-index-specified")
            });
    node.parent.is_none()
        || (is_positioned(&node.style)
            && (specifies_z_index
                || matches!(node.style.position, Position::Fixed | Position::Sticky)))
        || node.style.opacity < 1.0
        || !node.style.transform.is_identity()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(y: f32) -> LayoutRect {
        LayoutRect {
            x: 0.0,
            y,
            width: 320.0,
            height: 80.0,
        }
    }

    #[test]
    fn collapsible_inline_whitespace_is_trimmed_at_a_soft_line_start() {
        let mut parent_style = Style::default();
        parent_style.padding.left = w3cos_std::style::Spacing::Px(10.0);
        parent_style.border_left_width = Some(2.0);
        let mut inline_style = Style::default();
        inline_style.display = Display::Inline;
        let mut nodes = vec![
            PaintNode {
                kind: ComponentKind::Row,
                style: parent_style,
                parent: None,
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Text {
                    content: "  next line".to_string(),
                },
                style: inline_style,
                parent: Some(0),
                sticky_counter_signal: None,
            },
        ];
        let rects = vec![
            Some(LayoutRect {
                x: 20.0,
                y: 0.0,
                width: 200.0,
                height: 40.0,
            }),
            Some(LayoutRect {
                x: 32.0,
                y: 20.0,
                width: 80.0,
                height: 20.0,
            }),
        ];

        trim_collapsible_inline_whitespace_at_line_start(&mut nodes, &rects);
        assert!(matches!(
            &nodes[1].kind,
            ComponentKind::Text { content } if content == "next line"
        ));

        nodes[1].style.white_space = WhiteSpace::Pre;
        nodes[1].kind = ComponentKind::Text {
            content: "  preserved".to_string(),
        };
        trim_collapsible_inline_whitespace_at_line_start(&mut nodes, &rects);
        assert!(matches!(
            &nodes[1].kind,
            ComponentKind::Text { content } if content == "  preserved"
        ));
    }

    #[test]
    fn inline_fragment_clip_keeps_layout_rect_and_clips_only_paint() {
        let mut style = Style::default();
        style.display = w3cos_std::style::Display::InlineFlex;
        style.font_size = 20.0;
        style.line_height = 1.0;
        style.align_self = w3cos_std::style::AlignSelf::FlexEnd;
        let layout = LayoutRect {
            x: 12.0,
            y: 40.0,
            width: 100.0,
            height: 80.0,
        };
        let text = ComponentKind::Text {
            content: "fragment".to_string(),
        };

        assert_eq!(
            inline_fragment_clip_rect(&text, &style, layout),
            Some(LayoutRect {
                x: 12.0,
                y: 100.0,
                width: 100.0,
                height: 20.0,
            })
        );
        assert_eq!(
            inline_fragment_clip_rect(
                &ComponentKind::Image {
                    src: "100x100.png".to_string(),
                },
                &style,
                layout,
            ),
            None,
            "vertical alignment must not crop a replaced element to the line-height strut"
        );
    }

    #[test]
    fn css2_clip_rect_uses_positioned_box_local_coordinates() {
        let style = Style {
            position: Position::Absolute,
            clip: Some(w3cos_std::style::CssClipRect {
                top: Some(w3cos_std::style::Dimension::Px(10.0)),
                right: Some(w3cos_std::style::Dimension::Px(70.0)),
                bottom: Some(w3cos_std::style::Dimension::Px(50.0)),
                left: Some(w3cos_std::style::Dimension::Px(20.0)),
            }),
            ..Style::default()
        };
        assert_eq!(
            css2_clip_rect(
                &style,
                LayoutRect {
                    x: 100.0,
                    y: 200.0,
                    width: 90.0,
                    height: 80.0,
                }
            ),
            Some(LayoutRect {
                x: 120.0,
                y: 210.0,
                width: 50.0,
                height: 40.0,
            })
        );
    }

    #[test]
    fn improper_table_part_nested_in_cell_has_no_independent_background() {
        let mut nodes = vec![
            PaintNode {
                kind: ComponentKind::Box,
                style: Style {
                    display: Display::Table,
                    ..Style::default()
                },
                parent: None,
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Box,
                style: Style {
                    display: Display::TableCell,
                    ..Style::default()
                },
                parent: Some(0),
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Box,
                style: Style {
                    display: Display::TableRowGroup,
                    background: Color::rgb(255, 0, 0),
                    ..Style::default()
                },
                parent: Some(1),
                sticky_counter_signal: None,
            },
        ];
        suppress_improper_nested_table_part_backgrounds(&mut nodes);
        assert_eq!(nodes[2].style.background, Color::TRANSPARENT);
    }

    #[test]
    fn empty_cells_hide_suppresses_separate_cell_background_and_border() {
        let mut nodes = vec![PaintNode {
            kind: ComponentKind::Box,
            style: Style {
                display: Display::TableCell,
                empty_cells_hide: true,
                background: Color::rgb(255, 0, 0),
                border_width: 5.0,
                ..Style::default()
            },
            parent: None,
            sticky_counter_signal: None,
        }];
        suppress_hidden_empty_cell_paint(&mut nodes);
        assert_eq!(nodes[0].style.background, Color::TRANSPARENT);
        assert_eq!(nodes[0].style.border_width, 0.0);
    }

    #[test]
    fn collapsed_equal_inline_borders_prefer_the_start_cell() {
        let start_color = Color::rgb(0, 128, 0);
        let end_color = Color::rgb(255, 0, 0);
        let mut nodes = vec![
            PaintNode {
                kind: ComponentKind::Box,
                style: Style {
                    display: Display::TableRow,
                    border_collapse: true,
                    ..Style::default()
                },
                parent: None,
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Box,
                style: Style {
                    display: Display::TableCell,
                    border_right_width: Some(20.0),
                    border_right_color: Some(start_color),
                    ..Style::default()
                },
                parent: Some(0),
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Box,
                style: Style {
                    display: Display::TableCell,
                    border_left_width: Some(20.0),
                    border_left_color: Some(end_color),
                    ..Style::default()
                },
                parent: Some(0),
                sticky_counter_signal: None,
            },
        ];

        resolve_collapsed_cell_border_conflicts(&mut nodes);

        assert_eq!(nodes[1].style.border_right_color, Some(start_color));
        assert_eq!(nodes[2].style.border_left_width, Some(20.0));
        assert_eq!(nodes[2].style.border_left_color, Some(Color::TRANSPARENT));
        assert_eq!(
            box_background_paint_rect(
                &nodes[2].style,
                LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: 100.0,
                    height: 40.0,
                },
            ),
            LayoutRect {
                x: 20.0,
                y: 0.0,
                width: 80.0,
                height: 40.0,
            }
        );
    }

    #[test]
    fn collapsed_explicit_transparent_border_remains_transparent() {
        let mut nodes = vec![
            PaintNode {
                kind: ComponentKind::Box,
                style: Style {
                    display: Display::TableRow,
                    border_collapse: true,
                    ..Style::default()
                },
                parent: None,
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Box,
                style: Style {
                    display: Display::TableCell,
                    color: Color::BLACK,
                    border_right_width: Some(2.0),
                    border_color: Color::TRANSPARENT,
                    ..Style::default()
                },
                parent: Some(0),
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Box,
                style: Style {
                    display: Display::TableCell,
                    color: Color::BLACK,
                    border_left_width: Some(2.0),
                    border_color: Color::TRANSPARENT,
                    ..Style::default()
                },
                parent: Some(0),
                sticky_counter_signal: None,
            },
        ];

        resolve_collapsed_cell_border_conflicts(&mut nodes);

        assert_eq!(nodes[1].style.border_right_color, Some(Color::TRANSPARENT));
        assert_eq!(nodes[2].style.border_left_color, Some(Color::TRANSPARENT));
    }

    #[test]
    fn collapsed_cell_background_uses_shared_border_halves() {
        let style = Style {
            display: Display::TableCell,
            border_collapse: true,
            border_top_width: Some(4.0),
            border_right_width: Some(2.0),
            border_bottom_width: Some(4.0),
            border_left_width: Some(2.0),
            ..Style::default()
        };

        assert_eq!(
            box_background_paint_rect(
                &style,
                LayoutRect {
                    x: 137.0,
                    y: 53.0,
                    width: 59.0,
                    height: 23.0,
                },
            ),
            LayoutRect {
                x: 138.0,
                y: 55.0,
                width: 57.0,
                height: 19.0,
            }
        );
    }

    #[test]
    fn collapsed_row_projects_visibility_to_its_cells() {
        let mut nodes = vec![
            PaintNode {
                kind: ComponentKind::Box,
                style: Style {
                    display: Display::TableRow,
                    visibility: Visibility::Collapse,
                    ..Style::default()
                },
                parent: None,
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Box,
                style: Style {
                    display: Display::TableCell,
                    visibility: Visibility::Visible,
                    ..Style::default()
                },
                parent: Some(0),
                sticky_counter_signal: None,
            },
        ];

        project_collapsed_table_tracks_to_cells(&mut nodes);

        assert_eq!(nodes[1].style.visibility, Visibility::Collapse);
    }

    #[test]
    fn collapsed_columns_clip_partial_spans_and_hide_full_cells() {
        let node = |display, visibility, parent, span: Option<&str>| PaintNode {
            kind: ComponentKind::Box,
            style: Style {
                display,
                visibility,
                custom_properties: span.map(|span| {
                    std::collections::HashMap::from([(
                        "--w3cos-internal-table-column-span".to_string(),
                        span.to_string(),
                    )])
                }),
                ..Style::default()
            },
            parent,
            sticky_counter_signal: None,
        };
        let mut nodes = vec![
            node(Display::Table, Visibility::Visible, None, None),
            node(Display::TableColumn, Visibility::Visible, Some(0), None),
            node(Display::TableColumn, Visibility::Collapse, Some(0), None),
            node(Display::TableColumn, Visibility::Visible, Some(0), None),
            node(Display::TableRow, Visibility::Visible, Some(0), None),
            node(Display::TableCell, Visibility::Visible, Some(4), Some("2")),
            node(Display::TableCell, Visibility::Visible, Some(4), None),
            node(Display::TableRow, Visibility::Visible, Some(0), None),
            node(Display::TableCell, Visibility::Visible, Some(7), None),
            node(Display::TableCell, Visibility::Visible, Some(7), None),
            node(Display::TableCell, Visibility::Visible, Some(7), None),
        ];

        project_collapsed_table_tracks_to_cells(&mut nodes);

        assert_eq!(nodes[5].style.visibility, Visibility::Visible);
        assert_eq!(nodes[5].style.overflow_x, Some(Overflow::Hidden));
        assert_eq!(nodes[9].style.visibility, Visibility::Collapse);
        assert_eq!(nodes[9].style.overflow_x, Some(Overflow::Hidden));
        assert_eq!(nodes[9].style.overflow_y, Some(Overflow::Hidden));
    }

    #[test]
    fn table_background_paint_rect_excludes_bottom_caption() {
        let nodes = vec![
            PaintNode {
                kind: ComponentKind::Box,
                style: Style {
                    display: Display::Table,
                    background: Color::rgb(0, 0, 255),
                    ..Style::default()
                },
                parent: None,
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Box,
                style: Style {
                    display: Display::TableCaption,
                    caption_side_bottom: true,
                    ..Style::default()
                },
                parent: Some(0),
                sticky_counter_signal: None,
            },
        ];
        let artifact = PaintArtifact::build(
            nodes,
            &[
                (
                    LayoutRect {
                        x: 0.0,
                        y: 0.0,
                        width: 192.0,
                        height: 115.0,
                    },
                    0,
                ),
                (
                    LayoutRect {
                        x: 0.0,
                        y: 96.0,
                        width: 192.0,
                        height: 19.0,
                    },
                    1,
                ),
            ],
            1,
        );
        assert_eq!(
            table_grid_paint_rect(&artifact.nodes[0].style, artifact.rect_by_index[0].unwrap()),
            LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 192.0,
                height: 96.0
            }
        );
    }

    #[test]
    fn separated_row_group_background_is_not_clipped_to_cell_fragments() {
        let mut nodes = vec![
            PaintNode {
                kind: ComponentKind::Box,
                style: Style {
                    display: Display::Table,
                    border_collapse: false,
                    ..Style::default()
                },
                parent: None,
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Box,
                style: Style {
                    display: Display::TableRowGroup,
                    background_image: Some("blue.png".into()),
                    ..Style::default()
                },
                parent: Some(0),
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Box,
                style: Style {
                    display: Display::TableRow,
                    ..Style::default()
                },
                parent: Some(1),
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Box,
                style: Style {
                    display: Display::TableCell,
                    ..Style::default()
                },
                parent: Some(2),
                sticky_counter_signal: None,
            },
        ];
        let row_group = LayoutRect {
            x: 3.0,
            y: 3.0,
            width: 96.0,
            height: 96.0,
        };
        let rects = vec![
            Some(LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 102.0,
                height: 102.0,
            }),
            Some(row_group),
            Some(row_group),
            Some(LayoutRect {
                x: 3.0,
                y: 3.0,
                width: 90.0,
                height: 96.0,
            }),
        ];

        annotate_separated_table_background_fragments(&mut nodes, &rects);

        assert!(
            nodes[1]
                .style
                .custom_properties
                .as_ref()
                .is_none_or(|properties| !properties.contains_key(TABLE_BACKGROUND_FRAGMENTS))
        );
        assert_eq!(
            box_background_paint_rects(&nodes[1].style, row_group),
            vec![row_group]
        );
    }

    #[test]
    fn first_line_internal_clip_uses_the_originating_line_height() {
        let mut style = Style::default();
        style.display = w3cos_std::style::Display::Inline;
        style.font_size = 100.0;
        style
            .custom_properties
            .get_or_insert_with(Default::default)
            .insert(
                "--w3cos-internal-inline-fragment-clip".to_string(),
                "top 60".to_string(),
            );
        let layout = LayoutRect {
            x: 0.0,
            y: 10.0,
            width: 80.0,
            height: 100.0,
        };
        let text = ComponentKind::Text {
            content: "fragment".to_string(),
        };

        assert_eq!(
            inline_fragment_clip_rect(&text, &style, layout),
            Some(LayoutRect {
                x: 0.0,
                y: 10.0,
                width: 80.0,
                height: 60.0,
            })
        );
    }

    #[test]
    fn atomic_inline_level_box_is_not_clipped_to_the_parent_line_height() {
        let style = Style {
            display: Display::InlineBlock,
            align_self: w3cos_std::style::AlignSelf::FlexStart,
            font_size: 16.0,
            line_height: 1.2,
            ..Style::default()
        };

        assert_eq!(
            inline_fragment_clip_rect(&ComponentKind::Box, &style, rect(0.0)),
            None
        );
    }

    #[test]
    fn propagates_root_background_to_the_canvas_without_repainting_the_root_box() {
        let mut style = Style::default();
        style.background = Color::rgb(255, 255, 0);
        style.background_image = Some("url(square-white.png)".into());
        style.border_width = 3.0;
        let artifact = PaintArtifact::build(
            [PaintNode {
                kind: ComponentKind::Column,
                style,
                parent: None,
                sticky_counter_signal: None,
            }],
            &[(rect(0.0), 0)],
            1,
        );

        assert_eq!(artifact.canvas_background, Color::rgb(255, 255, 0));
        assert_eq!(artifact.canvas_background_source, Some(0));
        assert_eq!(
            artifact.canvas_background_positioning_rect,
            Some(LayoutRect {
                x: 3.0,
                y: 3.0,
                width: 314.0,
                height: 74.0,
            })
        );
        let canvas_style = artifact.canvas_background_style.as_ref().unwrap();
        assert_eq!(canvas_style.border_width, 0.0);
        assert_eq!(canvas_style.background_position.as_deref(), Some("0px 0px"));
        assert_eq!(artifact.nodes[0].style.background, Color::TRANSPARENT);
        assert!(artifact.nodes[0].style.background_image.is_none());
    }

    #[test]
    fn root_sentinel_does_not_raise_negative_descendants_above_normal_flow() {
        let root = PaintNode {
            kind: ComponentKind::Column,
            style: Style::default(),
            parent: None,
            sticky_counter_signal: None,
        };
        let mut negative_style = Style::default();
        negative_style.position = Position::Absolute;
        negative_style.z_index = -1;
        let negative = PaintNode {
            kind: ComponentKind::Box,
            style: negative_style,
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let normal = PaintNode {
            kind: ComponentKind::Box,
            style: Style::default(),
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let artifact = PaintArtifact::build(
            [root, negative, normal],
            &[(rect(0.0), 0), (rect(0.0), 1), (rect(0.0), 2)],
            1,
        );
        assert_eq!(artifact.z_order, [i32::MIN, -1, 0]);
        assert!(
            artifact.paint_order_key(0) < artifact.paint_order_key(1),
            "the root background and border paint below negative descendants"
        );
        assert!(artifact.paint_order_key(1) < artifact.paint_order_key(2));
    }

    #[test]
    fn absolute_box_inherits_clip_chain_from_its_containing_block() {
        let nodes = vec![
            PaintNode {
                kind: ComponentKind::Box,
                style: Style::default(),
                parent: None,
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Box,
                style: Style {
                    overflow: Overflow::Hidden,
                    ..Style::default()
                },
                parent: Some(0),
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Box,
                style: Style {
                    position: Position::Absolute,
                    ..Style::default()
                },
                parent: Some(1),
                sticky_counter_signal: None,
            },
        ];
        let artifact =
            PaintArtifact::build(nodes, &[(rect(0.0), 0), (rect(0.0), 1), (rect(80.0), 2)], 1);

        assert_ne!(artifact.node_properties[1].clip, 0);
        assert_eq!(artifact.node_properties[2].clip, 0);
    }

    #[test]
    fn propagates_body_background_image_to_the_canvas() {
        let mut root_style = Style::default();
        root_style.background_image = Some("none".into());
        root_style.border_width = 3.0;
        let root = PaintNode {
            kind: ComponentKind::Column,
            style: root_style,
            parent: None,
            sticky_counter_signal: None,
        };
        let mut body_style = Style::default();
        body_style.background_image = Some("url(square-white.png)".into());
        body_style.background_position = Some("top left".into());
        let body = PaintNode {
            kind: ComponentKind::Column,
            style: body_style,
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let artifact = PaintArtifact::build_with_body_background(
            [root, body],
            &[
                (
                    LayoutRect {
                        x: 16.0,
                        ..rect(16.0)
                    },
                    0,
                ),
                (rect(0.0), 1),
            ],
            1,
            Some(1),
        );

        assert_eq!(artifact.canvas_background_source, Some(1));
        assert_eq!(artifact.canvas_background, Color::WHITE);
        assert_eq!(artifact.canvas_background_positioning_rect, None);
        let canvas_style = artifact.canvas_background_style.as_ref().unwrap();
        assert_eq!(canvas_style.border_width, 0.0);
        assert_eq!(
            canvas_style.background_position.as_deref(),
            Some("19px 19px")
        );
        assert!(artifact.nodes[1].style.background_image.is_none());
    }

    #[test]
    fn propagated_body_percentage_position_uses_the_root_box() {
        let root = PaintNode {
            kind: ComponentKind::Column,
            style: Style::default(),
            parent: None,
            sticky_counter_signal: None,
        };
        let mut body_style = Style::default();
        body_style.background_image = Some("url(square-purple.png)".into());
        body_style.background_position = Some("50% 50%".into());
        let body = PaintNode {
            kind: ComponentKind::Column,
            style: body_style,
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let root_rect = LayoutRect {
            x: 16.0,
            ..rect(16.0)
        };
        let artifact = PaintArtifact::build_with_body_background(
            [root, body],
            &[(root_rect, 0), (rect(0.0), 1)],
            1,
            Some(1),
        );

        assert_eq!(artifact.canvas_background_positioning_rect, Some(root_rect));
    }

    #[test]
    fn builds_independent_property_trees_and_display_chunks() {
        let root = PaintNode {
            kind: ComponentKind::Column,
            style: Style::default(),
            parent: None,
            sticky_counter_signal: None,
        };
        let mut scroll_style = Style::default();
        scroll_style.overflow = Overflow::Scroll;
        let scroll = PaintNode {
            kind: ComponentKind::Column,
            style: scroll_style,
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let mut child_style = Style::default();
        child_style.opacity = 0.5;
        child_style.transform.translate_y = 4.0;
        let child = PaintNode {
            kind: ComponentKind::Text {
                content: "row".into(),
            },
            style: child_style,
            parent: Some(1),
            sticky_counter_signal: None,
        };

        let artifact = PaintArtifact::build(
            [root, scroll, child],
            &[(rect(0.0), 0), (rect(0.0), 1), (rect(80.0), 2)],
            7,
        );

        assert_eq!(artifact.generation, 7);
        assert_eq!(artifact.display_items.len(), 3);
        assert_eq!(artifact.chunks.len(), 3);
        assert_eq!(artifact.properties.scrolls.len(), 2);
        assert_eq!(artifact.properties.clips.len(), 2);
        assert_eq!(artifact.properties.effects.len(), 2);
        assert_eq!(artifact.properties.transforms.len(), 2);
        assert_ne!(artifact.node_properties[2].scroll, 0);
        assert_ne!(artifact.node_properties[2].effect, 0);
        assert_ne!(artifact.node_properties[2].transform, 0);
    }

    #[test]
    fn sticky_owner_and_z_order_are_retained() {
        let mut sticky_style = Style::default();
        sticky_style.position = Position::Sticky;
        sticky_style.z_index = 3;
        let nodes = [
            PaintNode {
                kind: ComponentKind::Column,
                style: Style::default(),
                parent: None,
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Column,
                style: sticky_style,
                parent: Some(0),
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Text {
                    content: "inside".into(),
                },
                style: Style::default(),
                parent: Some(1),
                sticky_counter_signal: None,
            },
        ];
        let artifact =
            PaintArtifact::build(nodes, &[(rect(0.0), 0), (rect(0.0), 1), (rect(20.0), 2)], 1);

        assert_eq!(artifact.sticky_owner, vec![None, Some(1), Some(1)]);
        assert_eq!(artifact.z_order, vec![i32::MIN, 3, 3]);
    }

    #[test]
    fn auto_positioned_subtree_paints_after_later_normal_flow_content() {
        let root = PaintNode {
            kind: ComponentKind::Column,
            style: Style::default(),
            parent: None,
            sticky_counter_signal: None,
        };
        let mut absolute_style = Style::default();
        absolute_style.position = Position::Absolute;
        let absolute = PaintNode {
            kind: ComponentKind::Column,
            style: absolute_style,
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let normal = PaintNode {
            kind: ComponentKind::Column,
            style: Style::default(),
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let artifact = PaintArtifact::build(
            [root, absolute, normal],
            &[(rect(0.0), 0), (rect(0.0), 1), (rect(0.0), 2)],
            1,
        );

        assert_eq!(artifact.z_order, vec![i32::MIN, i32::MIN + 1, i32::MIN]);
    }

    #[test]
    fn auto_positioned_container_paints_normal_blocks_before_positioned_children() {
        let nodes = [
            (None, Position::Static),
            (Some(0), Position::Relative),
            (Some(1), Position::Absolute),
            (Some(1), Position::Static),
        ].map(|(parent, position)| PaintNode {
            kind: ComponentKind::Box,
            style: Style { position, ..Style::default() },
            parent,
            sticky_counter_signal: None,
        });
        let artifact = PaintArtifact::build(nodes,
            &[(rect(0.0), 0), (rect(0.0), 1), (rect(0.0), 2), (rect(0.0), 3)], 1);
        assert!(artifact.paint_order_key(1) < artifact.paint_order_key(3));
        assert!(artifact.paint_order_key(3) < artifact.paint_order_key(2));
    }

    #[test]
    fn nested_stacking_context_orders_background_auto_and_positive_children() {
        let root = PaintNode {
            kind: ComponentKind::Column,
            style: Style::default(),
            parent: None,
            sticky_counter_signal: None,
        };
        let wrapper = PaintNode {
            kind: ComponentKind::Column,
            style: Style {
                position: Position::Relative,
                z_index: 1,
                ..Style::default()
            },
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let positive = PaintNode {
            kind: ComponentKind::Column,
            style: Style {
                position: Position::Absolute,
                z_index: 1,
                ..Style::default()
            },
            parent: Some(1),
            sticky_counter_signal: None,
        };
        let auto = PaintNode {
            kind: ComponentKind::Column,
            style: Style {
                position: Position::Absolute,
                ..Style::default()
            },
            parent: Some(1),
            sticky_counter_signal: None,
        };
        let artifact = PaintArtifact::build(
            [root, wrapper, positive, auto],
            &[
                (rect(0.0), 0),
                (rect(0.0), 1),
                (rect(0.0), 2),
                (rect(0.0), 3),
            ],
            1,
        );

        assert!(artifact.paint_order_key(1) < artifact.paint_order_key(3));
        assert!(artifact.paint_order_key(3) < artifact.paint_order_key(2));
    }

    #[test]
    fn fixed_auto_stacking_context_contains_negative_descendants() {
        let root = PaintNode {
            kind: ComponentKind::Column,
            style: Style::default(),
            parent: None,
            sticky_counter_signal: None,
        };
        let mut relative_style = Style::default();
        relative_style.position = Position::Relative;
        let relative = PaintNode {
            kind: ComponentKind::Column,
            style: relative_style,
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let mut fixed_style = Style::default();
        fixed_style.position = Position::Fixed;
        let fixed = PaintNode {
            kind: ComponentKind::Column,
            style: fixed_style,
            parent: Some(1),
            sticky_counter_signal: None,
        };
        let mut negative_style = Style::default();
        negative_style.position = Position::Absolute;
        negative_style.z_index = -1;
        let negative = PaintNode {
            kind: ComponentKind::Column,
            style: negative_style,
            parent: Some(2),
            sticky_counter_signal: None,
        };
        let artifact = PaintArtifact::build(
            [root, relative, fixed, negative],
            &[
                (rect(0.0), 0),
                (rect(0.0), 1),
                (rect(0.0), 2),
                (rect(0.0), 3),
            ],
            1,
        );

        assert_eq!(artifact.z_order, vec![i32::MIN, 1, 1, 2]);
        assert_eq!(artifact.display_items[2].client_index, 2);
        assert_eq!(artifact.display_items[3].client_index, 3);
    }

    #[test]
    fn css2_paint_phases_group_float_descendants_between_blocks_and_inlines() {
        let root = PaintNode {
            kind: ComponentKind::Column,
            style: Style::default(),
            parent: None,
            sticky_counter_signal: None,
        };
        let mut float_style = Style::default();
        float_style.float = w3cos_std::style::Float::Left;
        let floating = PaintNode {
            kind: ComponentKind::Column,
            style: float_style,
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let float_child = PaintNode {
            kind: ComponentKind::Box,
            style: Style::default(),
            parent: Some(1),
            sticky_counter_signal: None,
        };
        let mut inline_style = Style::default();
        inline_style.display = Display::Inline;
        let inline = PaintNode {
            kind: ComponentKind::Text {
                content: "inline".to_string(),
            },
            style: inline_style,
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let mut relative_style = Style::default();
        relative_style.position = Position::Relative;
        let relative = PaintNode {
            kind: ComponentKind::Column,
            style: relative_style,
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let mut positioned_inline_style = Style::default();
        positioned_inline_style.display = Display::Inline;
        let positioned_inline = PaintNode {
            kind: ComponentKind::Text {
                content: "positioned inline".to_string(),
            },
            style: positioned_inline_style,
            parent: Some(4),
            sticky_counter_signal: None,
        };
        let artifact = PaintArtifact::build(
            [
                root,
                floating,
                float_child,
                inline,
                relative,
                positioned_inline,
            ],
            &[
                (rect(0.0), 0),
                (rect(0.0), 1),
                (rect(0.0), 2),
                (rect(0.0), 3),
                (rect(0.0), 4),
                (rect(0.0), 5),
            ],
            1,
        );

        assert_eq!(artifact.css2_paint_phase(0), 0);
        assert_eq!(artifact.css2_paint_phase(1), 1);
        assert_eq!(artifact.css2_paint_phase(2), 1);
        assert_eq!(artifact.css2_paint_phase(3), 2);
        assert_eq!(artifact.css2_paint_phase(5), 0);
    }

    #[test]
    fn inline_block_descendants_remain_in_the_atomic_inline_paint_phase() {
        let root = PaintNode {
            kind: ComponentKind::Box,
            style: Style::default(),
            parent: None,
            sticky_counter_signal: None,
        };
        let inline_block = PaintNode {
            kind: ComponentKind::Box,
            style: Style {
                display: Display::InlineBlock,
                ..Style::default()
            },
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let block_child = PaintNode {
            kind: ComponentKind::Box,
            style: Style::default(),
            parent: Some(1),
            sticky_counter_signal: None,
        };
        let artifact = PaintArtifact::build(
            [root, inline_block, block_child],
            &[(rect(0.0), 0), (rect(0.0), 1), (rect(0.0), 2)],
            1,
        );

        assert_eq!(artifact.css2_paint_phase(1), 2);
        assert_eq!(artifact.css2_paint_phase(2), 2);
        assert!(artifact.paint_order_key(1) < artifact.paint_order_key(2));
    }

    #[test]
    fn positioned_auto_descendants_paint_after_the_positioned_background() {
        let root = PaintNode {
            kind: ComponentKind::Box,
            style: Style::default(),
            parent: None,
            sticky_counter_signal: None,
        };
        let positioned = PaintNode {
            kind: ComponentKind::Box,
            style: Style {
                position: Position::Relative,
                ..Style::default()
            },
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let image = PaintNode {
            kind: ComponentKind::Image {
                src: "green.png".into(),
            },
            style: Style::default(),
            parent: Some(1),
            sticky_counter_signal: None,
        };
        let artifact = PaintArtifact::build(
            [root, positioned, image],
            &[(rect(0.0), 0), (rect(0.0), 1), (rect(0.0), 2)],
            1,
        );

        assert!(artifact.paint_order_key(1) < artifact.paint_order_key(2));
    }

    #[test]
    fn float_descendants_do_not_escape_a_positioned_auto_subtree() {
        let root = PaintNode {
            kind: ComponentKind::Box,
            style: Style::default(),
            parent: None,
            sticky_counter_signal: None,
        };
        let control = PaintNode {
            kind: ComponentKind::Box,
            style: Style {
                position: Position::Absolute,
                ..Style::default()
            },
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let container = PaintNode {
            kind: ComponentKind::Box,
            style: Style {
                position: Position::Absolute,
                ..Style::default()
            },
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let floating = PaintNode {
            kind: ComponentKind::Box,
            style: Style {
                float: w3cos_std::style::Float::Left,
                ..Style::default()
            },
            parent: Some(2),
            sticky_counter_signal: None,
        };
        let float_text = PaintNode {
            kind: ComponentKind::Text {
                content: "XXXX".to_string(),
            },
            style: Style {
                display: Display::Inline,
                ..Style::default()
            },
            parent: Some(3),
            sticky_counter_signal: None,
        };
        let artifact = PaintArtifact::build(
            [root, control, container, floating, float_text],
            &[
                (rect(0.0), 0),
                (rect(0.0), 1),
                (rect(0.0), 2),
                (rect(0.0), 3),
                (rect(0.0), 4),
            ],
            1,
        );

        assert!(artifact.paint_order_key(1) < artifact.paint_order_key(2));
        assert!(artifact.paint_order_key(2) < artifact.paint_order_key(3));
        assert!(artifact.paint_order_key(3) < artifact.paint_order_key(4));
    }

    #[test]
    fn clean_subtree_does_not_clone_style() {
        let mut style = Style::default();
        style.filter = Some("blur(2px)".into());
        let kind = ComponentKind::Column;
        let incoming = [(&kind, &style, None, None)];
        let (first, clones) = reuse_or_clone_paint_nodes(Vec::new(), incoming);
        assert_eq!(clones, 1);
        let first_ptr = first[0].style.filter.as_ref().map(|s| s.as_ptr());
        let (second, clones) = reuse_or_clone_paint_nodes(first, incoming);
        assert_eq!(clones, 0);
        assert_eq!(
            second[0].style.filter.as_ref().map(|s| s.as_ptr()),
            first_ptr
        );
        let mut dirty = style.clone();
        dirty.opacity = 0.5;
        let incoming_dirty = [(&kind, &dirty, None, None)];
        let (_, dirty_clones) = reuse_or_clone_paint_nodes(second, incoming_dirty);
        assert_eq!(dirty_clones, 1);
    }
}
