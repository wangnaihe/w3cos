//! Retained paint output shared by every raster backend.
//!
//! This follows Blink's split between layout, PaintArtifact construction and
//! compositor consumption. The artifact owns immutable snapshots so scrolling
//! and raster scheduling never need to walk the application component tree.

use w3cos_std::color::Color;
use w3cos_std::component::ComponentKind;
use w3cos_std::style::{
    Display, Overflow, Position, Spacing, Style, Transform2D, Visibility, WhiteSpace,
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

pub(crate) struct AppliedTextDecoration<'a> {
    pub style: std::borrow::Cow<'a, Style>,
    pub baseline_shift: f32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct InlineLineContext {
    pub line_box: LayoutRect,
    pub first_line_box: LayoutRect,
    pub direction: w3cos_std::style::TextDirection,
    pub text_align: w3cos_std::style::TextAlign,
    pub line_advance: Option<f32>,
    #[cfg(feature = "skia")]
    pub tab_stops: crate::render_skia::TabStops,
}

impl InlineLineContext {
    pub fn fragment_box(&self, style: &Style, first: bool, last: bool) -> LayoutRect {
        let line = if first {
            self.first_line_box
        } else {
            self.line_box
        };
        let padding = style.padding_lengths();
        let margin = style.margin_lengths();
        let widths = crate::text_layout::inline_fragment_border_widths(style, first, last);
        let rtl = style.direction == w3cos_std::style::TextDirection::Rtl;
        let left = if (first && !rtl) || (last && rtl) {
            margin.left + padding.left + widths[3]
        } else {
            0.0
        };
        let right = if (last && !rtl) || (first && rtl) {
            margin.right + padding.right + widths[1]
        } else {
            0.0
        };
        LayoutRect {
            x: line.x + left,
            width: (line.width - left - right).max(1.0),
            ..line
        }
    }

    pub fn alignment(&self) -> w3cos_std::style::TextAlign {
        use w3cos_std::style::{TextAlign, TextDirection};
        match (self.text_align, self.direction) {
            (TextAlign::Start, TextDirection::Rtl) | (TextAlign::End, TextDirection::Ltr) => {
                TextAlign::Right
            }
            (TextAlign::Start, TextDirection::Ltr) | (TextAlign::End, TextDirection::Rtl) => {
                TextAlign::Left
            }
            (align, _) => align,
        }
    }
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
    /// A compositor hint isolates this subtree's raster surface even when
    /// its opacity is one and its transform is currently the identity.
    pub isolates_surface: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct ScrollNode {
    pub parent: PropertyNodeId,
    pub host_index: Option<usize>,
    pub scrollport: Option<LayoutRect>,
    pub clip: Option<PropertyNodeId>,
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
                isolates_surface: false,
            }],
            scrolls: vec![ScrollNode {
                parent: 0,
                host_index: None,
                scrollport: None,
                clip: None,
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

/// A physical slice of one logical source box. Vertical clipping slices the
/// box decoration rather than repeating top/bottom borders at column breaks.
/// Inline-axis overflow is not clipped to the nominal column width.
#[derive(Clone, Copy, Debug)]
pub struct ColumnFragment {
    pub visual_rect: LayoutRect,
    pub translate_x: f32,
    pub translate_y: f32,
    pub clip_top: f32,
    pub clip_bottom: f32,
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
    /// Root viewport scroll in CSS pixels; layout and recordings stay in
    /// document coordinates so scrolling can replay retained pictures.
    pub viewport_scroll: (f32, f32),
    pub body_index: Option<usize>,
    pub canvas_background: Color,
    pub canvas_background_style: Option<Style>,
    pub canvas_background_source: Option<usize>,
    pub canvas_background_positioning_rect: Option<LayoutRect>,
    pub display_items: Vec<DisplayItem>,
    pub chunks: Vec<PaintChunk>,
    pub properties: PropertyTrees,
    pub node_properties: Vec<PaintProperties>,
    /// The clip chain tip the node's own background and border paint under.
    ///
    /// This is `node_properties[index].clip` except for a box that clips its
    /// own overflow: CSS 2.1 11.1.1 scopes an `overflow` clip to "the contents
    /// of an element", so the box's own border and background stay outside its
    /// clip. The `clip` property and inline fragment clips do apply to the box
    /// itself, so they appear here as well. Kept beside `node_properties`
    /// rather than inside `PaintProperties` because the latter is the
    /// compositor's layer identity: the overflow box's own paint remains
    /// stationary while its contents can scroll in a separate layer.
    pub self_clip: Vec<PropertyNodeId>,
    pub z_order: Vec<i32>,
    pub paint_order: Vec<Vec<PaintOrderLevel>>,
    logical_paint_ordinals: Vec<usize>,
    pub sticky_owner: Vec<Option<usize>>,
    pub rect_by_index: Vec<Option<LayoutRect>>,
    pub column_fragments: Vec<Vec<ColumnFragment>>,
    pub generation: u64,
}

impl Default for PaintArtifact {
    fn default() -> Self {
        Self {
            nodes: Vec::new(),
            viewport_scroll: (0.0, 0.0),
            body_index: None,
            canvas_background: Color::WHITE,
            canvas_background_style: None,
            canvas_background_source: None,
            canvas_background_positioning_rect: None,
            display_items: Vec::new(),
            chunks: Vec::new(),
            properties: PropertyTrees::default(),
            node_properties: Vec::new(),
            self_clip: Vec::new(),
            z_order: Vec::new(),
            paint_order: Vec::new(),
            logical_paint_ordinals: Vec::new(),
            sticky_owner: Vec::new(),
            rect_by_index: Vec::new(),
            column_fragments: Vec::new(),
            generation: 0,
        }
    }
}

fn logical_paint_ordinals(nodes: &[PaintNode]) -> Vec<usize> {
    let rank = |index: usize| {
        nodes[index]
            .style
            .custom_properties
            .as_ref()
            .and_then(|properties| properties.get("--w3cos-internal-bidi-logical-order"))
            .and_then(|value| value.parse::<usize>().ok())
    };
    let mut ordinals: Vec<_> = (0..nodes.len()).collect();
    if !(0..nodes.len()).any(|index| rank(index).is_some()) {
        return ordinals;
    }
    // Recover logical tree traversal only for normalized bidi sibling runs.
    // Subtrees remain contiguous; indices and layout stay in visual order.
    let mut children = vec![Vec::new(); nodes.len() + 1];
    for (index, node) in nodes.iter().enumerate() {
        children[node
            .parent
            .filter(|parent| *parent < nodes.len())
            .unwrap_or(nodes.len())]
        .push(index);
    }
    for siblings in &mut children {
        if siblings.iter().all(|index| rank(*index).is_some()) {
            siblings.sort_by_key(|index| rank(*index));
        }
    }
    let mut stack: Vec<_> = children[nodes.len()].iter().rev().copied().collect();
    let mut ordinal = 0;
    while let Some(index) = stack.pop() {
        ordinals[index] = ordinal;
        ordinal += 1;
        stack.extend(children[index].iter().rev().copied());
    }
    ordinals
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
    if matches!(
        kind,
        ComponentKind::Image { .. }
            | ComponentKind::Canvas { .. }
            | ComponentKind::SvgDocument { .. }
    ) {
        return None;
    }
    // Vertical alignment positions an inline box; it does not clip its
    // background, borders, or overflowing descendants. Only an explicit
    // fragment boundary from inline splitting creates a paint clip.
    let (alignment, height) = internal_clip?;
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
            || matches!(
                node.style.white_space,
                WhiteSpace::Pre | WhiteSpace::PreWrap
            )
        {
            continue;
        }
        let (Some(mut parent), Some(rect)) =
            (node.parent, rect_by_index.get(index).copied().flatten())
        else {
            continue;
        };
        while nodes[parent].style.display == Display::Inline {
            let Some(ancestor) = nodes[parent].parent else {
                break;
            };
            parent = ancestor;
        }
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
        let node = &mut nodes[index];
        let ComponentKind::Text { content } = &mut node.kind else {
            continue;
        };
        let trimmed = content.trim_start_matches([' ', '\t', '\n', '\r', '\u{000c}']);
        if trimmed.len() != content.len() {
            let removed = content.len() - trimmed.len();
            let ends = w3cos_std::inline_text::fragment_ends(content, &node.style);
            *content = trimmed.to_string();
            if let Some(ends) = ends {
                // Anonymous text may contain independently authored DOM or
                // pseudo-element runs. Rebase their byte boundaries and text
                // fingerprint when discarding a line-leading separator.
                let ends = ends.into_iter().filter(|end| *end > removed)
                    .map(|end| end - removed).collect::<Vec<_>>();
                w3cos_std::inline_text::set_fragment_ends(&mut node.style, content, &ends);
            }
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
            Display::TableColumnGroup
                | Display::TableColumn
                | Display::TableRow
                | Display::TableRowGroup
                | Display::TableHeaderGroup
                | Display::TableFooterGroup
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
        let row_source = matches!(
            nodes[source].style.display,
            Display::TableRow
                | Display::TableRowGroup
                | Display::TableHeaderGroup
                | Display::TableFooterGroup
        );
        if covered.is_empty() && !row_source {
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
            if row_source && row != source && !descendant_of(nodes, row, source) {
                continue;
            }
            let cells = (0..original_len)
                .filter(|index| {
                    nodes[*index].parent == Some(row)
                        && nodes[*index].style.display == Display::TableCell
                })
                .collect::<Vec<_>>();
            let mut column = 0usize;
            for cell in cells {
                let span = column_span(&nodes[cell].style);
                if (row_source
                    || (column..column.saturating_add(span)).any(|index| covered.contains(&index)))
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
                let all_collapsed = !covered_columns.is_empty()
                    && covered_columns.iter().all(|collapsed| *collapsed);
                if all_collapsed {
                    nodes[cell].style.visibility = Visibility::Collapse;
                    nodes[cell].style.overflow_x = Some(Overflow::Hidden);
                    nodes[cell].style.overflow_y = Some(Overflow::Hidden);
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
    fn hidden(style: &Style, side: usize) -> bool {
        style.border_styles[side] == Some(w3cos_std::style::BorderLineStyle::Hidden)
    }
    fn adopt_edge(nodes: &mut [PaintNode], owner: usize, cell: usize, side: usize) {
        let (width, color) = edge(&nodes[owner].style, side);
        let line_style = nodes[owner].style.border_styles[side];
        let current_color = nodes[owner].style.border_current_color
            .is_some_and(|mask| mask[side]);
        set_edge(&mut nodes[cell].style, side, width, color);
        nodes[cell].style.border_styles[side] = line_style;
        if current_color || nodes[cell].style.border_current_color.is_some() {
            nodes[cell].style.border_current_color.get_or_insert([false; 4])[side] = current_color;
        }
    }
    fn line_priority(style: &Style, side: usize) -> u8 {
        use w3cos_std::style::BorderLineStyle;
        // Compare authored styles before color/owner precedence. Outset's
        // groove-like painting does not change its conflict priority.
        match style.border_styles[side].unwrap_or(BorderLineStyle::Solid) {
            BorderLineStyle::Hidden => 10,
            BorderLineStyle::Double => 9,
            BorderLineStyle::Solid => 8,
            BorderLineStyle::Dashed => 7,
            BorderLineStyle::Dotted => 6,
            BorderLineStyle::Ridge => 5,
            BorderLineStyle::Outset => 4,
            BorderLineStyle::Groove => 3,
            BorderLineStyle::Inset => 2,
            BorderLineStyle::None => 0,
        }
    }
    fn hide_edge(style: &mut Style, side: usize) {
        style.border_styles[side] = Some(w3cos_std::style::BorderLineStyle::Hidden);
        set_edge(style, side, 0.0, Color::TRANSPARENT);
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
    let mut parts = nodes
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, node)| {
            node.style.border_collapse
                && matches!(
                    node.style.display,
                    Display::TableRow
                        | Display::TableRowGroup
                        | Display::TableHeaderGroup
                        | Display::TableFooterGroup
                        | Display::TableColumn
                        | Display::TableColumnGroup
                )
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    // Equal-width/color conflicts retain the higher-priority owner. Cells
    // already own their authored edges; resolve parts in CSS precedence
    // order, independently of their order in the flattened paint tree.
    parts.sort_by_key(|index| match nodes[*index].style.display {
        Display::TableRow => 0,
        Display::TableRowGroup | Display::TableHeaderGroup | Display::TableFooterGroup => 1,
        Display::TableColumn => 2,
        _ => 3,
    });
    for part in parts {
        let column_part = matches!(
            nodes[part].style.display,
            Display::TableColumn | Display::TableColumnGroup
        );
        let part_rows = if nodes[part].style.display == Display::TableRow {
            vec![part]
        } else {
            rows.iter()
                .copied()
                .filter(|row| {
                    if nearest_table(nodes, *row) != nearest_table(nodes, part) {
                        return false;
                    }
                    if column_part {
                        return true;
                    }
                    let mut parent = nodes[*row].parent;
                    while let Some(index) = parent {
                        if index == part {
                            return true;
                        }
                        parent = nodes[index].parent;
                    }
                    false
                })
                .collect()
        };
        let populated_rows = part_rows
            .iter()
            .copied()
            .filter(|row| !row_cells(nodes, *row).is_empty())
            .collect::<Vec<_>>();
        let Some(first) = populated_rows.first().copied() else {
            continue;
        };
        let last = populated_rows.last().copied().unwrap_or(first);
        let mut boundary_cells = [
            row_cells(nodes, first),
            Vec::new(),
            row_cells(nodes, last),
            Vec::new(),
        ];
        for row in populated_rows {
            let cells = row_cells(nodes, row);
            if let Some(first) = cells.first() {
                boundary_cells[3].push(*first);
            }
            if let Some(last) = cells.last() {
                boundary_cells[1].push(*last);
            }
        }
        if column_part {
            fn descendant(nodes: &[PaintNode], mut index: usize, ancestor: usize) -> bool {
                while let Some(parent) = nodes[index].parent {
                    if parent == ancestor {
                        return true;
                    }
                    index = parent;
                }
                false
            }
            fn span(style: &Style) -> usize {
                style
                    .custom_properties
                    .as_ref()
                    .and_then(|properties| properties.get("--w3cos-internal-table-column-span"))
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(1)
                    .clamp(1, 1000)
            }
            let mut columns = Vec::new();
            for (index, node) in nodes.iter().enumerate() {
                if nearest_table(nodes, index) != nearest_table(nodes, part) {
                    continue;
                }
                let implicit_group = node.style.display == Display::TableColumnGroup
                    && !nodes.iter().enumerate().any(|(child, node)| {
                        node.style.display == Display::TableColumn
                            && descendant(nodes, child, index)
                    });
                if node.style.display == Display::TableColumn || implicit_group {
                    columns.extend(std::iter::repeat_n(index, span(&node.style)));
                }
            }
            let covered = columns
                .iter()
                .enumerate()
                .filter(|(_, column)| **column == part || descendant(nodes, **column, part))
                .map(|(column, _)| column)
                .collect::<Vec<_>>();
            let Some(start) = covered.first().copied() else {
                continue;
            };
            let end = covered.last().copied().unwrap_or(start) + 1;
            boundary_cells = std::array::from_fn(|_| Vec::new());
            for row in &part_rows {
                let mut column = 0usize;
                for cell in row_cells(nodes, *row) {
                    let next = column + span(&nodes[cell].style);
                    if column < end && next > start {
                        if *row == first {
                            boundary_cells[0].push(cell);
                        }
                        if *row == last {
                            boundary_cells[2].push(cell);
                        }
                    }
                    if column == start {
                        boundary_cells[3].push(cell);
                    }
                    if next == end {
                        boundary_cells[1].push(cell);
                    }
                    column = next;
                }
            }
        }
        for side in 0..4 {
            let part_edge = edge(&nodes[part].style, side);
            for cell in &boundary_cells[side] {
                if hidden(&nodes[part].style, side) || hidden(&nodes[*cell].style, side) {
                    hide_edge(&mut nodes[*cell].style, side);
                } else if part_edge.0 > edge(&nodes[*cell].style, side).0
                    || (part_edge.0 == edge(&nodes[*cell].style, side).0
                        && line_priority(&nodes[part].style, side) > line_priority(&nodes[*cell].style, side))
                {
                    adopt_edge(nodes, part, *cell, side);
                }
            }
            if !boundary_cells[side].is_empty() {
                // Cells now own the winning edge. Keep the part's background
                // on its grid box without painting a second border ring.
                set_edge(&mut nodes[part].style, side, 0.0, Color::TRANSPARENT);
            }
        }
    }

    let grid_nodes = nodes.iter().map(|node| (&node.style, node.parent)).collect::<Vec<_>>();
    let spanning_grids = crate::table_grid::calculate(&grid_nodes).into_iter()
        .filter(|grid| nodes[grid.table].style.border_collapse
            && grid.cells.iter().any(|cell| cell.row_span > 1)).collect::<Vec<_>>();
    let mut occupied_rows = std::collections::HashMap::new();
    for grid in &spanning_grids {
        for (row_number, row) in grid.rows.iter().enumerate() {
            let mut cells = grid.cells.iter().filter(|cell| cell.row <= row_number
                && row_number < cell.row + cell.row_span).copied().collect::<Vec<_>>();
            cells.sort_by_key(|cell| cell.column);
            occupied_rows.insert(*row, cells);
        }
    }
    #[derive(Default)]
    struct HorizontalVotes { covered: usize, span: usize, won: usize, hidden: usize, width: f32 }
    let mut horizontal_votes = std::collections::HashMap::<(usize, usize), HorizontalVotes>::new();
    for row in &rows {
        let pairs = if let Some(cells) = occupied_rows.get(row) {
            cells.windows(2).filter(|pair| pair[0].column + pair[0].column_span == pair[1].column)
                .map(|pair| (pair[0].index, pair[1].index, pair[0].row_span, pair[1].row_span))
                .collect::<Vec<_>>()
        } else {
            row_cells(nodes, *row).windows(2).map(|pair| (pair[0], pair[1], 1, 1)).collect()
        };
        for (left, right, left_span, right_span) in pairs {
            let left_edge = edge(&nodes[left].style, 1);
            let right_edge = edge(&nodes[right].style, 3);
            let hidden_conflict = hidden(&nodes[left].style, 1) || hidden(&nodes[right].style, 3);
            let left_collapsed = nodes[left].style.visibility == Visibility::Collapse;
            let right_collapsed = nodes[right].style.visibility == Visibility::Collapse;
            let left_wins = if left_collapsed != right_collapsed {
                !left_collapsed
            } else if left_edge.0 > right_edge.0 {
                true
            } else if right_edge.0 > left_edge.0 {
                false
            } else if line_priority(&nodes[left].style, 1) != line_priority(&nodes[right].style, 3) {
                line_priority(&nodes[left].style, 1) > line_priority(&nodes[right].style, 3)
            } else {
                nodes[*row].style.direction != TextDirection::Rtl
            };
            for (cell, side, span, won, other_width) in [
                (left, 1, left_span, left_wins, right_edge.0),
                (right, 3, right_span, !left_wins, left_edge.0),
            ] {
                let vote = horizontal_votes.entry((cell, side)).or_default();
                vote.span = span;
                vote.covered += 1;
                vote.won += usize::from(won);
                vote.hidden += usize::from(hidden_conflict);
                vote.width = vote.width.max(other_width);
            }
        }
    }
    for ((cell, side), vote) in horizontal_votes {
        // A neighbor on one row cannot suppress an entire rowspan edge.
        // Mixed ownership needs per-segment painting, just as for colspan.
        if vote.covered != vote.span { continue; }
        if vote.hidden == vote.covered {
            hide_edge(&mut nodes[cell].style, side);
        } else if vote.hidden == 0 && vote.won == 0 {
            suppress_edge(&mut nodes[cell].style, side, vote.width);
        } else if vote.hidden == 0 && vote.won == vote.covered {
            let winner = edge(&nodes[cell].style, side);
            set_edge(&mut nodes[cell].style, side, winner.0, winner.1);
        }
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
                if hidden(&nodes[table].style, side) || hidden(&nodes[*cell].style, side) {
                    hide_edge(&mut nodes[*cell].style, side);
                    continue;
                }
                let cell_edge = edge(&nodes[*cell].style, side);
                if table_edge.0 > cell_edge.0
                    || (table_edge.0 == cell_edge.0
                        && line_priority(&nodes[table].style, side) > line_priority(&nodes[*cell].style, side))
                {
                    adopt_edge(nodes, table, *cell, side);
                } else {
                    set_edge(&mut nodes[*cell].style, side, cell_edge.0, cell_edge.1);
                }
            }
            // Boundary cells paint the resolved collapsed edge on the grid.
            // Leaving the table wrapper border active would inset and paint a
            // second ring around that same edge.
            // Keep the resolved table padding-box geometry while cells own
            // its ink. CSS2 takes inline outer edges from the first row;
            // block outer edges use the maximum across the boundary row.
            let used_width = if matches!(side, 1 | 3) {
                boundary_cells[side]
                    .first()
                    .map(|cell| edge(&nodes[*cell].style, side).0)
            } else {
                boundary_cells[side]
                    .iter()
                    .map(|cell| edge(&nodes[*cell].style, side).0)
                    .reduce(f32::max)
            }
            .unwrap_or(table_edge.0);
            suppress_edge(&mut nodes[table].style, side, used_width);
        }
    }
    for pair in rows.windows(2) {
        if nearest_table(nodes, pair[0]) != nearest_table(nodes, pair[1]) {
            continue;
        }
        let ranges = |row| {
            let mut column = 0usize;
            row_cells(nodes, row).into_iter().map(|cell| {
                let span = nodes[cell].style.custom_properties.as_ref()
                    .and_then(|properties| properties.get("--w3cos-internal-table-column-span"))
                    .and_then(|span| span.parse::<usize>().ok()).unwrap_or(1).clamp(1, 1000);
                let start = column;
                column += span;
                (cell, start, column)
            }).collect::<Vec<_>>()
        };
        let top_cells = ranges(pair[0]);
        let bottom_cells = ranges(pair[1]);
        #[derive(Default)]
        struct Votes { covered: usize, span: usize, won: usize, hidden: usize, width: f32 }
        let mut votes = std::collections::HashMap::<(usize, usize), Votes>::new();
        let (mut top_index, mut bottom_index) = (0, 0);
        while top_index < top_cells.len() && bottom_index < bottom_cells.len() {
            let (top, top_start, top_end) = top_cells[top_index];
            let (bottom, bottom_start, bottom_end) = bottom_cells[bottom_index];
            let overlap = top_end.min(bottom_end).saturating_sub(top_start.max(bottom_start));
            let top_edge = edge(&nodes[top].style, 2);
            let bottom_edge = edge(&nodes[bottom].style, 0);
            let hidden_conflict = hidden(&nodes[top].style, 2) || hidden(&nodes[bottom].style, 0);
            let top_collapsed = nodes[top].style.visibility == Visibility::Collapse;
            let bottom_collapsed = nodes[bottom].style.visibility == Visibility::Collapse;
            let bottom_wins = (top_collapsed && !bottom_collapsed) || bottom_edge.0 > top_edge.0
                || (bottom_edge.0 == top_edge.0
                    && line_priority(&nodes[bottom].style, 0) > line_priority(&nodes[top].style, 2));
            for (cell, side, span, won, other_width) in [
                (top, 2, top_end - top_start, !bottom_wins, bottom_edge.0),
                (bottom, 0, bottom_end - bottom_start, bottom_wins, top_edge.0),
            ] {
                let vote = votes.entry((cell, side)).or_default();
                vote.span = span;
                vote.covered += overlap;
                vote.won += if won { overlap } else { 0 };
                vote.hidden += if hidden_conflict { overlap } else { 0 };
                vote.width = vote.width.max(other_width);
            }
            if top_end <= bottom_end { top_index += 1; }
            if bottom_end <= top_end { bottom_index += 1; }
        }
        for ((cell, side), vote) in votes {
            // Never suppress an entire spanning edge on the strength of a
            // single track. Mixed winners require separate ink segments.
            if vote.covered != vote.span { continue; }
            if vote.hidden == vote.covered {
                hide_edge(&mut nodes[cell].style, side);
            } else if vote.hidden == 0 && vote.won == 0 {
                suppress_edge(&mut nodes[cell].style, side, vote.width);
            }
        }
    }
}

const COLLAPSED_BORDER_SUPPRESSED: &str = "--w3cos-internal-collapsed-border-suppressed";
const COLLAPSED_BORDER_BOTTOM_EXTENSION: &str =
    "--w3cos-internal-collapsed-border-bottom-extension";

const COLLAPSED_BORDER_JOINTS: &str = "--w3cos-internal-collapsed-border-joints";

fn annotate_collapsed_border_joints(nodes: &mut [PaintNode], rects: &[Option<LayoutRect>]) {
    use std::collections::HashMap;
    use w3cos_std::style::BorderLineStyle;
    struct Edge {
        cell: usize,
        side: usize,
        width: f32,
        priority: u8,
    }
    let mut edges = Vec::<Edge>::new();
    let mut joints = HashMap::<(usize, i64, i64), Vec<(usize, usize)>>::new();
    let mut lines = HashMap::<(usize, usize, i64), Vec<(i64, i64, usize)>>::new();
    let mut offsets = HashMap::<usize, [[f32; 2]; 4]>::new();
    for (cell, node) in nodes.iter().enumerate() {
        if node.style.display != Display::TableCell || !node.style.border_collapse
            || node.style.visibility == Visibility::Collapse {
            continue;
        }
        let Some(rect) = rects.get(cell).and_then(|rect| *rect) else { continue };
        let mut parent = node.parent;
        let mut table = None;
        while let Some(index) = parent {
            if matches!(nodes[index].style.display, Display::Table | Display::InlineTable) {
                table = Some(index);
                break;
            }
            parent = nodes[index].parent;
        }
        let Some(table) = table else { continue };
        let widths = [node.style.border_top_width, node.style.border_right_width,
            node.style.border_bottom_width, node.style.border_left_width]
            .map(|width| width.unwrap_or(node.style.border_width));
        let endpoints = [
            [(rect.x, rect.y), (rect.x + rect.width, rect.y)],
            [(rect.x + rect.width, rect.y), (rect.x + rect.width, rect.y + rect.height)],
            [(rect.x, rect.y + rect.height), (rect.x + rect.width, rect.y + rect.height)],
            [(rect.x, rect.y), (rect.x, rect.y + rect.height)],
        ];
        for side in 0..4 {
            if widths[side] <= 0.0 || collapsed_border_suppressed(&node.style, ["top", "right", "bottom", "left"][side]) {
                continue;
            }
            let priority = match node.style.border_styles[side].unwrap_or(BorderLineStyle::Solid) {
                BorderLineStyle::None | BorderLineStyle::Hidden => continue,
                BorderLineStyle::Double => 9, BorderLineStyle::Solid => 8,
                BorderLineStyle::Dashed => 7, BorderLineStyle::Dotted => 6,
                BorderLineStyle::Ridge => 5, BorderLineStyle::Outset => 4,
                BorderLineStyle::Groove => 3, BorderLineStyle::Inset => 2,
            };
            let edge_index = edges.len();
            edges.push(Edge { cell, side, width: widths[side], priority });
            offsets.entry(cell).or_insert([[0.0; 2]; 4]);
            let points = endpoints[side].map(|(x, y)| ((x * 64.0).round() as i64, (y * 64.0).round() as i64));
            let (line, start, end) = if side % 2 == 0 {
                (points[0].1, points[0].0, points[1].0)
            } else { (points[0].0, points[0].1, points[1].1) };
            lines.entry((table, side % 2, line)).or_default().push((start, end, edge_index));
            for (endpoint, (x, y)) in endpoints[side].into_iter().enumerate() {
                // Layout coordinates are CSS layout units, not snapped paint
                // pixels. Keep nearby but distinct joints separate.
                joints.entry((table, (x * 64.0).round() as i64, (y * 64.0).round() as i64))
                    .or_default().push((edge_index, endpoint));
            }
        }
    }
    // Include edges passing through T-joints, not just edges ending there.
    // Row/column indexes and prefix maximum endpoints keep ordinary joints
    // local rather than scanning every edge in the table.
    let indexed_lines: HashMap<_, _> = lines.into_iter().map(|(key, mut segments)| {
        segments.sort_by_key(|segment| segment.0);
        let mut maximum = i64::MIN;
        let maxima = segments.iter().map(|segment| {
            maximum = maximum.max(segment.1);
            maximum
        }).collect::<Vec<_>>();
        (key, (segments, maxima))
    }).collect();
    for (&(table, x, y), incident) in &mut joints {
        for (axis, line, coordinate) in [(0, y, x), (1, x, y)] {
            let Some((segments, maxima)) = indexed_lines.get(&(table, axis, line)) else { continue };
            let mut index = segments.partition_point(|segment| segment.0 < coordinate);
            while index > 0 && maxima[index - 1] > coordinate {
                index -= 1;
                let (_, end, edge) = segments[index];
                if end > coordinate { incident.push((edge, usize::MAX)); }
            }
        }
    }
    let compare = |a: &Edge, b: &Edge| a.width.total_cmp(&b.width)
        .then(a.priority.cmp(&b.priority)).then(b.cell.cmp(&a.cell));
    for incident in joints.values() {
        let Some((winner, _)) = incident.iter().max_by(|(a, _), (b, _)| compare(&edges[*a], &edges[*b])) else { continue };
        for &(edge_index, endpoint) in incident {
            if endpoint == usize::MAX { continue; }
            let edge = &edges[edge_index];
            let perpendicular_width = incident.iter()
                .filter(|(other, _)| edges[*other].side % 2 != edge.side % 2)
                .map(|(other, _)| edges[*other].width).fold(0.0, f32::max);
            let wins = !compare(edge, &edges[*winner]).is_lt();
            let signed_half = if wins { perpendicular_width / 2.0 } else { -perpendicular_width / 2.0 };
            offsets.get_mut(&edge.cell).unwrap()[edge.side][endpoint] =
                if endpoint == 0 { -signed_half } else { signed_half };
        }
    }
    for (cell, offsets) in offsets {
        nodes[cell].style.custom_properties.get_or_insert_with(Default::default)
            .insert(COLLAPSED_BORDER_JOINTS.into(), offsets.into_iter().flatten()
                .map(|value| value.to_string()).collect::<Vec<_>>().join(" "));
    }
}

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
    let grid = rect;
    let rect = if style.border_collapse && style.display == Display::TableCell {
        LayoutRect {
            y: rect.y - widths[0] / 2.0,
            height: rect.height + widths[0] / 2.0 + widths[2] / 2.0,
            ..rect
        }
    } else {
        rect
    };
    if style.border_collapse
        && matches!(
            style.display,
            Display::TableColumn | Display::TableColumnGroup
        )
    {
        // Column boxes describe grid tracks, not inset border boxes. A
        // collapsed border is centered on the corresponding grid line.
        // Block edges include the adjacent inline half-borders so the
        // outer corner quadrants are covered, not four disconnected strips.
        return [
            LayoutRect {
                x: rect.x - widths[3] / 2.0,
                y: rect.y - widths[0] / 2.0,
                width: rect.width + (widths[3] + widths[1]) / 2.0,
                height: widths[0],
            },
            LayoutRect {
                x: rect.x + rect.width - widths[1] / 2.0,
                y: rect.y,
                width: widths[1],
                height: rect.height,
            },
            LayoutRect {
                x: rect.x - widths[3] / 2.0,
                y: rect.y + rect.height - widths[2] / 2.0,
                width: rect.width + (widths[3] + widths[1]) / 2.0,
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
    let cell_inline_half = if style.border_collapse && style.display == Display::TableCell {
        0.5
    } else {
        0.0
    };
    let suppressed = |name: &str| collapsed_border_suppressed(style, name);
    let top = if suppressed("top") { widths[0] } else { 0.0 };
    let right = if suppressed("right") {
        widths[1] * (1.0 - cell_inline_half)
    } else {
        0.0
    };
    let bottom = if suppressed("bottom") { widths[2] } else { 0.0 };
    let left = if suppressed("left") {
        widths[3] * (1.0 - cell_inline_half)
    } else {
        0.0
    };
    let bottom_extension = style
        .custom_properties
        .as_ref()
        .and_then(|properties| properties.get(COLLAPSED_BORDER_BOTTOM_EXTENSION))
        .and_then(|value| value.parse::<f32>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(0.0);
    if style.border_collapse && style.display == Display::TableCell {
        let joints = style.custom_properties.as_ref()
            .and_then(|properties| properties.get(COLLAPSED_BORDER_JOINTS))
            .and_then(|value| value.split_ascii_whitespace().map(str::parse::<f32>)
                .collect::<Result<Vec<_>, _>>().ok())
            .filter(|values| values.len() == 8 && values.iter().all(|value| value.is_finite()));
        if let Some(joints) = joints {
            return [
                LayoutRect { x: grid.x + joints[0], y: grid.y - widths[0] / 2.0,
                    width: (grid.width + joints[1] - joints[0]).max(0.0), height: widths[0] },
                LayoutRect { x: grid.x + grid.width - widths[1] / 2.0, y: grid.y + joints[2],
                    width: widths[1], height: (grid.height + joints[3] - joints[2]).max(0.0) },
                LayoutRect { x: grid.x + joints[4], y: grid.y + grid.height - widths[2] / 2.0,
                    width: (grid.width + joints[5] - joints[4]).max(0.0), height: widths[2] + bottom_extension },
                LayoutRect { x: grid.x - widths[3] / 2.0, y: grid.y + joints[6],
                    width: widths[3], height: (grid.height + joints[7] - joints[6]).max(0.0) },
            ];
        }
    }
    let horizontal_insets = |width: f32| {
        // A wider collapsed inline edge owns the junction, including its
        // inner half. Horizontal ink must not paint over that winning edge.
        let wider_left = if cell_inline_half > 0.0 && widths[3] > width {
            widths[3] * cell_inline_half
        } else { 0.0 };
        let wider_right = if cell_inline_half > 0.0 && widths[1] > width {
            widths[1] * cell_inline_half
        } else { 0.0 };
        (left.max(wider_left), right.max(wider_right))
    };
    let (top_left, top_right) = horizontal_insets(widths[0]);
    let (bottom_left, bottom_right) = horizontal_insets(widths[2]);
    [
        LayoutRect {
            x: rect.x + top_left,
            y: rect.y,
            width: (rect.width - top_left - top_right).max(0.0),
            height: widths[0],
        },
        LayoutRect {
            x: rect.x + rect.width - widths[1] * (1.0 - cell_inline_half),
            y: rect.y + top,
            width: widths[1],
            height: (rect.height - top - bottom).max(0.0),
        },
        LayoutRect {
            x: rect.x + bottom_left,
            y: rect.y + rect.height - widths[2],
            width: (rect.width - bottom_left - bottom_right).max(0.0),
            height: widths[2] + bottom_extension,
        },
        LayoutRect {
            x: rect.x - widths[3] * cell_inline_half,
            y: rect.y + top,
            width: widths[3],
            height: (rect.height - top - bottom).max(0.0),
        },
    ]
}

pub(crate) fn paint_inline_border_widths(style: &Style) -> (f32, f32) {
    let scale = if style.border_collapse && style.display == Display::TableCell {
        0.5
    } else {
        1.0
    };
    (
        style.border_left_width.unwrap_or(style.border_width) * scale,
        style.border_right_width.unwrap_or(style.border_width) * scale,
    )
}

fn collapsed_border_suppressed(style: &Style, name: &str) -> bool {
    style
        .custom_properties
        .as_ref()
        .and_then(|properties| properties.get(COLLAPSED_BORDER_SUPPRESSED))
        .is_some_and(|value| value.split_ascii_whitespace().any(|side| side == name))
}

/// `overflow` applies to block containers (CSS 2.1 11.1.1). An inline box and
/// the internal table boxes that are not block containers therefore never clip
/// their overflowing content, however `overflow: hidden` is authored.
fn establishes_overflow_clip(display: Display) -> bool {
    display.establishes_overflow_clip()
}

/// The overflow clipping region is the element's padding box (CSS 2.1 11.1.1),
/// so the border widths come off the border box. A collapsed-border box owns
/// only half of each shared edge, the same convention as
/// `paint_inline_border_widths` and `box_background_positioning_rect`.
pub(crate) fn overflow_clip_rect(style: &Style, rect: LayoutRect) -> LayoutRect {
    let scale = if style.border_collapse
        && matches!(
            style.display,
            Display::TableCell | Display::Table | Display::InlineTable
        ) {
        0.5
    } else {
        1.0
    };
    let top = style.border_top_width.unwrap_or(style.border_width) * scale;
    let right = style.border_right_width.unwrap_or(style.border_width) * scale;
    let bottom = style.border_bottom_width.unwrap_or(style.border_width) * scale;
    let left = style.border_left_width.unwrap_or(style.border_width) * scale;
    LayoutRect {
        x: rect.x + left,
        y: rect.y + top,
        width: (rect.width - left - right).max(0.0),
        height: (rect.height - top - bottom).max(0.0),
    }
}

pub(crate) fn box_background_paint_rect(style: &Style, rect: LayoutRect) -> LayoutRect {
    if style.border_collapse && style.display == Display::TableCell {
        // Cell rects end on shared grid-line centers on both axes.
        return rect;
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
    if let Some(right) = parts.next().and_then(|value| value.parse::<f32>().ok()) {
        rect.width = (rect.width - right * style.transform.scale_x).max(0.0);
    }
    rect
}

fn annotate_table_caption_paint_insets(
    nodes: &mut [PaintNode],
    rect_by_index: &[Option<LayoutRect>],
) {
    let mut insets = vec![(0.0_f32, 0.0_f32); nodes.len()];
    let mut has_top_caption = vec![false; nodes.len()];
    let mut grid_widths = vec![None::<f32>; nodes.len()];
    let mut grid_tops = vec![None::<f32>; nodes.len()];
    for (index, node) in nodes.iter().enumerate() {
        if matches!(
            node.style.display,
            Display::TableRow
                | Display::TableRowGroup
                | Display::TableHeaderGroup
                | Display::TableFooterGroup
        ) && !matches!(node.style.position, Position::Absolute | Position::Fixed)
            && let Some(parent) = node.parent
            && matches!(
                nodes[parent].style.display,
                Display::Table | Display::InlineTable
            )
            && let Some(rect) = rect_by_index.get(index).copied().flatten()
        {
            grid_widths[parent] = Some(grid_widths[parent].unwrap_or(0.0).max(rect.width));
            grid_tops[parent] = Some(grid_tops[parent].unwrap_or(rect.y).min(rect.y));
        }
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
            has_top_caption[parent] = true;
            insets[parent].0 += height;
        }
    }
    for (index, (caption_top, bottom)) in insets.into_iter().enumerate() {
        let style = &nodes[index].style;
        let top = if has_top_caption[index] {
            grid_tops[index]
                .zip(rect_by_index.get(index).copied().flatten())
                .map(|(row_top, table_rect)| {
                    let border_top = style.border_top_width.unwrap_or(style.border_width);
                    let before_row = if style.border_collapse {
                        border_top * 0.5
                    } else {
                        border_top + style.padding_lengths().top + style.border_spacing_y
                    };
                    (row_top - table_rect.y - before_row).max(0.0)
                })
                .unwrap_or(caption_top)
        } else {
            caption_top
        };
        if top <= 0.0 && bottom <= 0.0 {
            continue;
        }
        let right = if !style.border_collapse {
            grid_widths[index]
                .zip(rect_by_index.get(index).copied().flatten())
                .map_or(0.0, |(grid_width, rect)| {
                    let padding = style.padding_lengths();
                    let edges = padding.left
                        + padding.right
                        + style.border_left_width.unwrap_or(style.border_width)
                        + style.border_right_width.unwrap_or(style.border_width)
                        + 2.0 * style.border_spacing_x;
                    // The wrapper includes the caption, but table background
                    // paints only the grid and its own edges.
                    (rect.width - grid_width - edges).max(0.0)
                })
        } else {
            0.0
        };
        nodes[index]
            .style
            .custom_properties
            .get_or_insert_with(Default::default)
            .insert(
                TABLE_CAPTION_INSETS.to_string(),
                format!("{top} {bottom} {right}"),
            );
    }
}

impl PaintArtifact {
    /// Applied decoration belongs to the originating box, not the child's
    /// computed property. Atomic inline and out-of-flow boxes isolate their
    /// contents, but an isolating box's own decoration still reaches them.
    pub(crate) fn ancestor_text_decorations(&self, index: usize) -> Vec<AppliedTextDecoration<'_>> {
        use w3cos_std::style::{Float, TextDecoration};
        let isolates = |style: &Style| {
            matches!(style.display, Display::InlineBlock | Display::InlineFlex | Display::InlineTable)
                || style.float != Float::None
                || matches!(style.position, Position::Absolute | Position::Fixed)
        };
        let Some(source) = self.nodes.get(index) else { return Vec::new(); };
        if !matches!(source.kind, ComponentKind::Text { .. }) || isolates(&source.style) {
            return Vec::new();
        }
        let anonymous = source.style.custom_properties.as_ref().is_some_and(|properties| {
            properties.get(w3cos_std::inline_text::SOURCE_RUN).is_some_and(|source| source.starts_with("text:") || source.starts_with("pseudo:"))
        });
        let baseline_offset = |style: &Style| style.custom_properties.as_ref()
            .and_then(|properties| properties.get("--w3cos-internal-vertical-align-length"))
            .and_then(|value| value.split_ascii_whitespace().next())
            .and_then(|value| value.parse::<f32>().ok()).unwrap_or(0.0);
        let mut baseline_shift = baseline_offset(&source.style);
        let mut parent = source.parent;
        let mut owners = w3cos_std::inline_text::decoration_owners(&source.style)
            .into_iter().rev().map(|style| AppliedTextDecoration {
                style: std::borrow::Cow::Owned(style), baseline_shift: 0.0,
            }).collect::<Vec<_>>();
        while let Some(owner) = parent {
            let node = &self.nodes[owner];
            // The immediate anonymous text already paints its parent's used
            // decoration. Do not paint the same line twice at fractional edges.
            let represented_locally = anonymous && source.parent == Some(owner)
                && source.style.text_decoration == node.style.text_decoration
                && source.style.color == node.style.color
                && source.style.font_size == node.style.font_size
                && source.style.font_family == node.style.font_family
                && source.style.font_weight == node.style.font_weight
                && source.style.font_style == node.style.font_style
                && source.style.font_variant == node.style.font_variant;
            if node.style.text_decoration != TextDecoration::None
                && node.style.display != Display::Contents && !represented_locally
            {
                owners.push(AppliedTextDecoration {
                    style: std::borrow::Cow::Borrowed(&node.style), baseline_shift });
            }
            if isolates(&node.style) { break; }
            owners.extend(w3cos_std::inline_text::decoration_owners(&node.style)
                .into_iter().rev().map(|style| AppliedTextDecoration {
                    style: std::borrow::Cow::Owned(style), baseline_shift,
                }));
            baseline_shift += baseline_offset(&node.style);
            parent = node.parent;
        }
        owners.reverse();
        owners
    }

    /// The paragraph's available line geometry is distinct from the inline
    /// owner's intrinsic decoration box and its own bidi direction.
    pub(crate) fn inline_line_context(&self, index: usize) -> Option<InlineLineContext> {
        let node = self.nodes.get(index)?;
        let ComponentKind::Text { content } = &node.kind else {
            return None;
        };
        let has_preserved_break = content.contains('\u{2028}')
            || (matches!(
                node.style.white_space,
                WhiteSpace::Pre | WhiteSpace::PreWrap | WhiteSpace::PreLine
            ) && content.contains(['\n', '\r']));
        let has_preserved_tab = node.style.white_space == WhiteSpace::Pre && content.contains('\t');
        if node.style.display != Display::Inline || !(has_preserved_break || has_preserved_tab) {
            return None;
        }
        let mut owner = index;
        let mut parent = node.parent?;
        loop {
            let ancestor = self.nodes.get(parent)?;
            if matches!(ancestor.style.display, Display::Inline | Display::Contents) {
                owner = parent;
                parent = ancestor.parent?;
                continue;
            }
            let anonymous = ancestor
                .style
                .custom_properties
                .as_ref()
                .is_some_and(|properties| {
                    properties.contains_key("--w3cos-internal-inline-formatting-context")
                });
            if !anonymous
                && !matches!(
                    ancestor.style.display,
                    Display::Block
                        | Display::FlowRoot
                        | Display::ListItem
                        | Display::TableCell
                        | Display::InlineBlock
                )
            {
                return None;
            }
            let rect = self.rect_by_index.get(parent).copied().flatten()?;
            let padding = ancestor.style.padding_lengths();
            let left = ancestor
                .style
                .border_left_width
                .unwrap_or(ancestor.style.border_width)
                + padding.left;
            let right = ancestor
                .style
                .border_right_width
                .unwrap_or(ancestor.style.border_width)
                + padding.right;
            let line_box = LayoutRect {
                x: rect.x + left,
                width: (rect.width - left - right).max(1.0),
                ..rect
            };
            let mut first_line_box = line_box;
            if let Some((previous_index, previous)) = self.nodes[..owner]
                .iter()
                .enumerate()
                .rev()
                .find(|(_, previous)| {
                    previous.parent == Some(parent)
                        && previous.style.display != Display::None
                        && previous.style.float == w3cos_std::style::Float::None
                        && !matches!(
                            previous.style.position,
                            Position::Absolute | Position::Fixed
                        )
                })
                && matches!(
                    previous.style.display,
                    Display::Inline
                        | Display::InlineBlock
                        | Display::InlineFlex
                        | Display::InlineTable
                )
                && !matches!(&previous.kind, ComponentKind::Text { content } if content == "\u{2028}")
                && let Some(previous_rect) =
                    self.rect_by_index.get(previous_index).copied().flatten()
                && !self.rect_by_index.get(index).copied().flatten().is_some_and(|current| {
                    // A multiline fragment can have a union x at the line
                    // start even when its first line continues a sibling.
                    // Reset only when both axes prove a later, rewound line.
                    ancestor.style.direction == w3cos_std::style::TextDirection::Ltr
                        && current.x + 0.01 < previous_rect.x + previous_rect.width
                        && current.y >= previous_rect.y + previous_rect.height - 0.01
                })
            {
                let margin = previous.style.margin_lengths();
                match ancestor.style.direction {
                    w3cos_std::style::TextDirection::Ltr => {
                        first_line_box.x =
                            (previous_rect.x + previous_rect.width + margin.right).max(line_box.x);
                        first_line_box.width =
                            (line_box.x + line_box.width - first_line_box.x).max(1.0);
                    }
                    w3cos_std::style::TextDirection::Rtl => {
                        first_line_box.width =
                            (previous_rect.x - margin.left - line_box.x).max(1.0);
                    }
                }
            }
            return Some(InlineLineContext {
                line_box,
                first_line_box,
                direction: ancestor.style.direction,
                text_align: ancestor.style.text_align,
                line_advance: Some(crate::inline_line_metrics::baseline_text_metrics(
                    &ancestor.style, &node.style,
                ).0),
                #[cfg(feature = "skia")]
                tab_stops: crate::render_skia::tab_stops_for_style(&ancestor.style),
            });
        }
    }

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
            u8::from(
                unadorned_block_text(node)
                    || matches!(
                        node.style.display,
                        Display::Inline
                            | Display::InlineBlock
                            | Display::InlineFlex
                            | Display::InlineTable
                    ),
            ) * 2
        })
    }

    pub fn build(
        nodes: impl IntoIterator<Item = PaintNode>,
        layout_cache: &[(LayoutRect, usize)],
        generation: u64,
    ) -> Self {
        Self::build_with_body_background(nodes, layout_cache, generation, None)
    }

    /// Fixed boxes and their descendants are attached to the viewport rather
    /// than the scrolling document. This mirrors layout's fixed-box owner.
    pub(crate) fn viewport_attached(&self, index: usize) -> bool {
        let mut current = Some(index);
        while let Some(index) = current {
            let Some(node) = self.nodes.get(index) else {
                break;
            };
            if node.style.position == Position::Fixed {
                return true;
            }
            current = node.parent;
        }
        false
    }

    pub(crate) fn viewport_scroll_for(&self, index: usize) -> (f32, f32) {
        if self.viewport_attached(index) {
            (0.0, 0.0)
        } else {
            self.viewport_scroll
        }
    }

    pub fn build_with_body_background(
        nodes: impl IntoIterator<Item = PaintNode>,
        layout_cache: &[(LayoutRect, usize)],
        generation: u64,
        body_index: Option<usize>,
    ) -> Self {
        Self::build_with_body_background_and_viewport(
            nodes,
            layout_cache,
            generation,
            body_index,
            None,
        )
    }

    pub(crate) fn build_with_body_background_and_viewport(
        nodes: impl IntoIterator<Item = PaintNode>,
        layout_cache: &[(LayoutRect, usize)],
        generation: u64,
        body_index: Option<usize>,
        viewport: Option<(f32, f32)>,
    ) -> Self {
        let mut nodes: Vec<_> = nodes.into_iter().collect();
        let mut has_percentage_padding = false;
        for node in &mut nodes {
            crate::background_image::annotate_fixed_background_viewport(&mut node.style, viewport);
            node.style.resolve_used_border_widths();
            has_percentage_padding |= [
                node.style.padding.top,
                node.style.padding.right,
                node.style.padding.bottom,
                node.style.padding.left,
            ]
            .into_iter()
            .any(|edge| matches!(edge, Spacing::Percent(_)));
        }
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
                            node.style.display != Display::None
                                && (node.style.background.a > 0 || has_background_image(&node.style))
                        })
                })
            })
            // A display:none document element has no box to paint on the
            // canvas. Its suppressed body cannot supply a fallback either.
            .filter(|_| nodes[0].style.display != Display::None);
        let root_rect = layout_cache
            .iter()
            .find_map(|(rect, index)| (*index == 0).then_some(*rect));
        if has_percentage_padding {
            let mut rects = vec![None; nodes.len()];
            for (rect, index) in layout_cache {
                if let Some(slot) = rects.get_mut(*index) {
                    *slot = Some(*rect);
                }
            }
            let root_basis =
                viewport.map_or_else(|| root_rect.map_or(0.0, |rect| rect.width), |size| size.0);
            let mut child_bases = vec![root_basis; nodes.len()];
            for index in 0..nodes.len() {
                let basis = nodes[index]
                    .parent
                    .map_or(root_basis, |parent| child_bases[parent]);
                let style = &mut nodes[index].style;
                for edge in [
                    &mut style.padding.top,
                    &mut style.padding.right,
                    &mut style.padding.bottom,
                    &mut style.padding.left,
                ] {
                    if let Spacing::Percent(percent) = *edge {
                        *edge = Spacing::Px(basis * percent / 100.0);
                    }
                }
                child_bases[index] = if matches!(style.display, Display::Inline | Display::Contents)
                {
                    basis
                } else {
                    let padding = style.padding_lengths();
                    let borders = style.border_left_width.unwrap_or(style.border_width)
                        + style.border_right_width.unwrap_or(style.border_width);
                    (rects[index].map_or(basis, |rect| rect.width)
                        - padding.left
                        - padding.right
                        - borders)
                        .max(0.0)
                };
            }
        }
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
        for node in &mut nodes {
            if let Some(properties) = node.style.custom_properties.as_mut() {
                properties.remove("--w3cos-internal-float-line-bands");
            }
        }
        if let Some((width, height)) = viewport {
            let refs: Vec<_> = nodes
                .iter()
                .map(|node| (&node.kind, &node.style, node.parent))
                .collect();
            let flows =
                crate::layout::resolve_float_text_layouts(&refs, &rect_by_index, width, height);
            for flow in flows {
                let encoded = flow
                    .bands
                    .iter()
                    .map(|band| {
                        format!(
                            "{} {} {}",
                            band.x - flow.content.x,
                            band.y - flow.content.y,
                            band.width
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(";");
                nodes[flow.text_index]
                    .style
                    .custom_properties
                    .get_or_insert_with(Default::default)
                    .insert("--w3cos-internal-float-line-bands".into(), encoded);
            }
        }
        trim_collapsible_inline_whitespace_at_line_start(&mut nodes, &rect_by_index);
        suppress_improper_nested_table_part_backgrounds(&mut nodes);
        suppress_hidden_empty_cell_paint(&mut nodes);
        project_collapsed_table_tracks_to_cells(&mut nodes);
        resolve_collapsed_cell_border_conflicts(&mut nodes);
        annotate_collapsed_border_joints(&mut nodes, &rect_by_index);
        extend_collapsed_borders_across_empty_rows(&mut nodes, &rect_by_index);
        annotate_separated_table_background_fragments(&mut nodes, &rect_by_index);
        let mut artifact = Self {
            logical_paint_ordinals: logical_paint_ordinals(&nodes),
            body_index,
            rect_by_index,
            node_properties: vec![PaintProperties::default(); nodes.len()],
            self_clip: vec![0; nodes.len()],
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

        if artifact.nodes.iter().any(|node| {
            node.style.column_count.is_some()
                || !matches!(node.style.column_width, w3cos_std::style::Dimension::Auto)
        }) {
            let column_nodes: Vec<_> = artifact.nodes.iter()
                .map(|node| (&node.style, node.parent)).collect();
            let fragment_heights = crate::layout::resolve_column_fragment_heights(
                &column_nodes, &artifact.rect_by_index, viewport);
            artifact.column_fragments = (0..artifact.nodes.len())
                .map(|index| artifact.build_column_fragments(index, viewport, &fragment_heights))
                .collect();
        }

        for index in 0..artifact.nodes.len() {
            artifact.append_node(index);
        }
        artifact
    }

    fn build_column_fragments(&self, index: usize, viewport: Option<(f32, f32)>, fragment_heights: &[Option<f32>]) -> Vec<ColumnFragment> {
        use w3cos_std::style::TextDirection;
        let Some(rect) = self.rect_by_index[index] else { return Vec::new(); };
        if rect.height <= 0.0 || !rect.height.is_finite() { return Vec::new(); }
        let mut ancestor = self.nodes[index].parent;
        if matches!(self.nodes[index].style.position, Position::Absolute | Position::Fixed) {
            return Vec::new();
        }
        while let Some(owner) = ancestor {
            let style = &self.nodes[owner].style;
            let Some(principal) = self.rect_by_index[owner] else { return Vec::new(); };
            let padding = style.padding_lengths();
            let left = style.border_left_width.unwrap_or(style.border_width) + padding.left;
            let right = style.border_right_width.unwrap_or(style.border_width) + padding.right;
            let top = style.border_top_width.unwrap_or(style.border_width) + padding.top;
            let content_width = (principal.width - left - right).max(0.0);
            let (vw, vh) = viewport.unwrap_or((principal.width, principal.height));
            if let Some(column_width) = crate::layout::used_column_width(style, content_width, vw, vh) {
                let Some(height) = fragment_heights[owner] else { return Vec::new(); };
                if !height.is_finite() || height <= 0.0 { return Vec::new(); }
                let gap = style.column_gap.unwrap_or(style.font_size).max(0.0);
                let stride = column_width + gap;
                let origin_y = principal.y + top;
                let first = ((rect.y - origin_y) / height).floor().max(0.0) as usize;
                let last = ((rect.y + rect.height - origin_y) / height).ceil().max(1.0) as usize;
                let rtl = style.direction == TextDirection::Rtl;
                let first_offset = if rtl { content_width - column_width } else { 0.0 };
                return (first..last).filter_map(|column| {
                    let band_top = origin_y + column as f32 * height;
                    let start = rect.y.max(band_top);
                    let end = (rect.y + rect.height).min(band_top + height);
                    if end <= start { return None; }
                    let translate_x = first_offset + column as f32 * stride * if rtl { -1.0 } else { 1.0 };
                    let translate_y = -(column as f32 * height);
                    Some(ColumnFragment {
                        visual_rect: LayoutRect { x: rect.x + translate_x, y: start + translate_y,
                            width: rect.width, height: end - start },
                        translate_x, translate_y, clip_top: origin_y, clip_bottom: origin_y + height,
                    })
                }).collect();
            }
            if matches!(style.position, Position::Absolute | Position::Fixed) { break; }
            ancestor = self.nodes[owner].parent;
        }
        Vec::new()
    }

    fn append_node(&mut self, index: usize) {
        let node = &self.nodes[index];
        let mut inherited = node
            .parent
            .and_then(|parent| self.node_properties.get(parent).copied())
            .unwrap_or_default();
        if node.style.display == Display::TableCaption
            && let Some(parent) = node.parent
            && matches!(
                self.nodes[parent].style.display,
                Display::Table | Display::InlineTable
            )
            && matches!(
                self.nodes[parent].style.resolved_overflow_x(),
                Overflow::Hidden
            )
        {
            inherited.clip = self.self_clip[parent];
        }
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
            let containing_properties = positioned_ancestor
                .and_then(|owner| self.node_properties.get(owner))
                .copied()
                .unwrap_or_default();
            inherited.clip = containing_properties.clip;
            // An out-of-flow box escapes intermediate overflow scrollports
            // together with their clips. Keep both property chains rooted at
            // its actual positioned containing block.
            inherited.scroll = containing_properties.scroll;
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
        // Snapshot the chain the box's own background and border paint under.
        // `overflow` clips the box's contents, not the box: a
        // `border: 10px solid black; overflow: auto` box keeps all ten pixels
        // of its border, and a negative-margin child must not erase the
        // parent's border either. The `clip` property and the inline fragment
        // clip below do apply to the box itself, so each refreshes this value.
        let mut self_clip = properties.clip;
        let mut overflow_clip = None;
        if establishes_overflow_clip(node.style.display)
            && (matches!(
                overflow_x,
                Overflow::Hidden | Overflow::Scroll | Overflow::Auto
            ) || matches!(
                overflow_y,
                Overflow::Hidden | Overflow::Scroll | Overflow::Auto
            ))
        {
            properties.clip = self.properties.clips.len();
            overflow_clip = Some(properties.clip);
            self.properties.clips.push(ClipNode {
                parent: inherited.clip,
                // The clipping region is the padding box, not the border box
                // (CSS 2.1 11.1.1). Without the border inset a border lets
                // exactly its own width of overflowing content show through:
                // `css/CSS2/ui/overflow-applies-to-009.xht` leaks the 5 px of
                // `border: 5px solid transparent`, and a 20 px border leaks 20.
                rect: self.rect_by_index[index].map(|rect| {
                    overflow_clip_rect(&node.style, table_grid_paint_rect(&node.style, rect))
                }),
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
            self_clip = properties.clip;
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
            self_clip = properties.clip;
        }
        if node.style.opacity < 0.999 || node.style.filter.is_some()
            || node.style.will_change.promotes_layer() {
            properties.effect = self.properties.effects.len();
            self.properties.effects.push(EffectNode {
                parent: inherited.effect,
                opacity: node.style.opacity,
                filter: node.style.filter.clone(),
                isolates_surface: node.style.will_change.promotes_layer(),
            });
        }
        let self_scroll = properties.scroll;
        // `overflow: hidden` is also a scroll container: it has no user
        // scrollbar, but CSSOM may move its scroll offset programmatically.
        if establishes_overflow_clip(node.style.display)
            && (matches!(
                overflow_x,
                Overflow::Hidden | Overflow::Scroll | Overflow::Auto
            ) || matches!(
                overflow_y,
                Overflow::Hidden | Overflow::Scroll | Overflow::Auto
            ))
        {
            properties.scroll = self.properties.scrolls.len();
            self.properties.scrolls.push(ScrollNode {
                parent: inherited.scroll,
                host_index: Some(index),
                scrollport: self.rect_by_index[index],
                clip: overflow_clip,
            });
        }
        self.node_properties[index] = properties;
        self.self_clip[index] = self_clip;

        if node.style.visibility != Visibility::Visible {
            return;
        }

        let Some(bounds) = self.rect_by_index[index] else {
            return;
        };
        let bounds = if node.style.border_collapse && node.style.display == Display::TableCell {
            let (left, right) = paint_inline_border_widths(&node.style);
            LayoutRect {
                x: bounds.x - left,
                width: bounds.width + left + right,
                ..bounds
            }
        } else {
            bounds
        };
        let fragments = self.column_fragments.get(index).map(Vec::as_slice).unwrap_or(&[]);
        let physical_bounds = fragments.iter()
            .map(|fragment| fragment.visual_rect)
            .chain(fragments.is_empty().then_some(bounds));
        for bounds in physical_bounds {
            let item_index = self.display_items.len();
            let chunk_id = self.chunks.len();
            self.display_items.push(DisplayItem { client_index: index, visual_rect: bounds, chunk_id });
            self.chunks.push(PaintChunk { begin: item_index, end: item_index + 1, bounds,
                properties: PaintProperties { scroll: self_scroll, ..properties },
                z_order: self.z_order[index],
            });
        }
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
        if node.parent.is_none() || establishes_stacking_context(node) || is_positioned(&node.style)
        {
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
        let ordinal = self
            .logical_paint_ordinals
            .get(index)
            .copied()
            .unwrap_or(index);
        if is_positioned(&node.style) {
            return match node.style.z_index.cmp(&0) {
                std::cmp::Ordering::Less => (0, node.style.z_index, ordinal),
                std::cmp::Ordering::Equal => (4, 0, ordinal),
                std::cmp::Ordering::Greater => (5, node.style.z_index, ordinal),
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
                || node
                    .style
                    .border_right_width
                    .is_some_and(|width| width > 0.0)
                || node
                    .style
                    .border_bottom_width
                    .is_some_and(|width| width > 0.0)
                || node
                    .style
                    .border_left_width
                    .is_some_and(|width| width > 0.0));
        if collapsed_table_part_border {
            // Collapsed table borders paint over cell contents. Transparent
            // table-part backgrounds can therefore use a late display item
            // without disturbing the table background-layer ordering.
            return (4, 0, ordinal);
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
                return (2, 0, ordinal);
            }
            if current != index
                && matches!(
                    current_node.style.display,
                    Display::InlineBlock | Display::InlineFlex | Display::InlineTable
                )
            {
                return (3, 0, ordinal);
            }
            if current != index && establishes_stacking_context(current_node) {
                break;
            }
            cursor = current_node.parent;
        }

        let split_inline_edge = node
            .style
            .custom_properties
            .as_ref()
            .is_some_and(|properties| {
                properties.contains_key("--w3cos-internal-split-inline-edge")
            });
        let normal_inline_border = node.style.display == Display::Inline
            && node.style.line_height_is_normal
            && matches!(node.kind, ComponentKind::Row | ComponentKind::Box)
            && (node.style.border_width > 0.0
                || node.style.border_top_width.is_some_and(|width| width > 0.0)
                || node.style.border_right_width.is_some_and(|width| width > 0.0)
                || node.style.border_bottom_width.is_some_and(|width| width > 0.0)
                || node.style.border_left_width.is_some_and(|width| width > 0.0));
        let phase = if split_inline_edge || normal_inline_border {
            // Inline decoration and in-flow foreground share tree order:
            // a later fragment covers preceding block glyph overflow, while
            // remaining behind its own/following text. Moving every border
            // before all foreground loses that ordering across split lines.
            3
        } else if unadorned_block_text(node)
            || matches!(
                node.style.display,
                Display::Inline | Display::InlineBlock | Display::InlineFlex | Display::InlineTable
            )
        {
            3
        } else {
            1
        };
        (phase, 0, ordinal)
    }
}

fn unadorned_block_text(node: &PaintNode) -> bool {
    matches!(node.kind, ComponentKind::Text { .. })
        && node.style.display == Display::Block
        && node.style.background.a == 0
        && node
            .style
            .background_image
            .as_deref()
            .is_none_or(|image| image == "none")
        && node.style.border_width == 0.0
        && [
            node.style.border_top_width,
            node.style.border_right_width,
            node.style.border_bottom_width,
            node.style.border_left_width,
        ]
        .into_iter()
        .all(|width| width.unwrap_or(0.0) == 0.0)
        && node.style.box_shadow.is_none()
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

    #[test]
    fn inline_line_context_resets_only_after_a_rewound_later_line() {
        for (x, y, expected_x) in [(8.0, 28.0, 8.0), (88.0, 8.0, 88.0), (8.0, 8.0, 88.0)] {
            let mut parent = Style::default();
            parent.display = Display::Block;
            parent.font_size = 20.0;
            parent.line_height = 1.0;
            let mut text = parent.clone();
            text.display = Display::Inline;
            text.white_space = WhiteSpace::Pre;
            let nodes = [
                PaintNode { kind: ComponentKind::Row, style: parent, parent: None, sticky_counter_signal: None },
                PaintNode { kind: ComponentKind::Text { content: "\u{200b}".into() },
                    style: text.clone(), parent: Some(0), sticky_counter_signal: None },
                PaintNode { kind: ComponentKind::Text { content: "XX\nXX".into() },
                    style: text, parent: Some(0), sticky_counter_signal: None },
            ];
            let artifact = PaintArtifact::build(nodes, &[
                (LayoutRect { x: 8.0, y: 8.0, width: 100.0, height: 60.0 }, 0),
                (LayoutRect { x: 88.0, y: 8.0, width: 0.0, height: 20.0 }, 1),
                (LayoutRect { x, y, width: 100.0, height: 20.0 }, 2),
            ], 1);
            let context = artifact.inline_line_context(2).unwrap();
            assert_eq!(context.first_line_box.x, expected_x, "x={x}, y={y}");
            assert_eq!(context.first_line_box.width, 108.0 - expected_x, "x={x}, y={y}");
        }
    }

    #[test]
    fn paint_snapshot_resolves_percentage_padding_from_containing_block() {
        let mut text_style = Style::default();
        text_style.display = Display::Block;
        text_style.padding.right = w3cos_std::style::Spacing::Percent(45.3);
        let nodes = vec![
            PaintNode {
                kind: ComponentKind::Row,
                style: Style::default(),
                parent: None,
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Row,
                style: Style {
                    display: Display::Block,
                    ..Style::default()
                },
                parent: Some(0),
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Text {
                    content: "x".into(),
                },
                style: text_style,
                parent: Some(1),
                sticky_counter_signal: None,
            },
        ];
        let layouts = [
            (
                LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: 800.0,
                    height: 600.0,
                },
                0,
            ),
            (
                LayoutRect {
                    x: 8.0,
                    y: 0.0,
                    width: 106.0,
                    height: 100.0,
                },
                1,
            ),
            (
                LayoutRect {
                    x: 8.0,
                    y: 0.0,
                    width: 106.0,
                    height: 10.0,
                },
                2,
            ),
        ];
        let artifact = PaintArtifact::build(nodes, &layouts, 1);
        assert!((artifact.nodes[2].style.padding_lengths().right - 48.018).abs() < 0.01);
    }

    #[test]
    fn inline_ancestor_does_not_replace_percentage_padding_basis() {
        let mut parent_style = Style::default();
        parent_style.display = Display::Block;
        parent_style.padding.left = Spacing::Px(10.0);
        let mut inline_style = Style::default();
        inline_style.display = Display::Inline;
        let mut text_style = Style::default();
        text_style.display = Display::Inline;
        text_style.padding.right = Spacing::Percent(50.0);
        let nodes = vec![
            PaintNode {
                kind: ComponentKind::Row,
                style: Style::default(),
                parent: None,
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Row,
                style: parent_style,
                parent: Some(0),
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Row,
                style: inline_style,
                parent: Some(1),
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Text {
                    content: "x".into(),
                },
                style: text_style,
                parent: Some(2),
                sticky_counter_signal: None,
            },
        ];
        let layouts = [
            (
                LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: 800.0,
                    height: 600.0,
                },
                0,
            ),
            (
                LayoutRect {
                    x: 8.0,
                    y: 0.0,
                    width: 106.0,
                    height: 100.0,
                },
                1,
            ),
            (
                LayoutRect {
                    x: 18.0,
                    y: 0.0,
                    width: 20.0,
                    height: 10.0,
                },
                2,
            ),
            (
                LayoutRect {
                    x: 18.0,
                    y: 0.0,
                    width: 20.0,
                    height: 10.0,
                },
                3,
            ),
        ];
        let artifact = PaintArtifact::build(nodes, &layouts, 1);
        assert_eq!(artifact.nodes[3].style.padding_lengths().right, 48.0);
    }

    #[test]
    fn collapsed_column_edges_cover_outer_corner_quadrants() {
        for display in [Display::TableColumn, Display::TableColumnGroup] {
            let style = Style {
                display,
                border_collapse: true,
                ..Style::default()
            };
            let edges = border_edge_paint_rects(
                &style,
                LayoutRect {
                    x: 10.0,
                    y: 53.0,
                    width: 100.0,
                    height: 100.0,
                },
                [4.0; 4],
            );
            for (x, y) in [(8.5, 51.5), (111.5, 51.5), (8.5, 154.5), (111.5, 154.5)] {
                assert!(
                    edges.iter().any(|rect| x >= rect.x
                        && x < rect.x + rect.width
                        && y >= rect.y
                        && y < rect.y + rect.height),
                    "{display:?} leaves the outer corner ({x}, {y}) uncovered"
                );
            }
        }
    }

    fn rect(y: f32) -> LayoutRect {
        LayoutRect {
            x: 0.0,
            y,
            width: 320.0,
            height: 80.0,
        }
    }

    #[test]
    fn overflow_clips_only_block_containers() {
        // `overflow` applies to block containers (CSS 2.1 11.1.1), so an inline
        // box and the internal table boxes must leave their overflowing content
        // visible however the author writes `overflow: hidden`
        // (css/CSS2/ui/overflow-applies-to-008.xht and -001..-004).
        for display in [
            Display::Inline,
            Display::TableRow,
            Display::TableRowGroup,
            Display::TableHeaderGroup,
            Display::TableFooterGroup,
            Display::TableColumn,
            Display::TableColumnGroup,
        ] {
            assert!(
                !establishes_overflow_clip(display),
                "{display:?} is not a block container and must not clip"
            );
        }
        for display in [
            Display::Block,
            Display::FlowRoot,
            Display::Flex,
            Display::Grid,
            Display::InlineBlock,
            Display::InlineFlex,
            Display::ListItem,
            Display::Table,
            Display::InlineTable,
            Display::TableCell,
            Display::TableCaption,
        ] {
            assert!(
                establishes_overflow_clip(display),
                "{display:?} is a block container and must clip"
            );
        }
    }

    #[test]
    fn overflow_clip_region_is_the_padding_box() {
        // The clipping region is the padding box, not the border box, so a
        // border must not let its own width of overflowing content show
        // through: css/CSS2/ui/overflow-applies-to-009.xht leaks exactly the
        // 5 px of `border: 5px solid transparent` without this inset.
        let style = Style {
            border_width: 5.0,
            ..Style::default()
        };
        let border_box = LayoutRect {
            x: 8.0,
            y: 51.2,
            width: 110.0,
            height: 30.0,
        };
        assert_eq!(
            overflow_clip_rect(&style, border_box),
            LayoutRect {
                x: 13.0,
                y: 56.2,
                width: 100.0,
                height: 20.0
            }
        );
    }

    #[test]
    fn overflow_clip_applies_to_the_contents_and_not_to_the_box() {
        // CSS 2.1 11.1.1 scopes an `overflow` clip to "the contents of an
        // element", so a box keeps painting its own border and background while
        // its descendants are clipped. `css/CSS2/normal-flow/negative-margin-001.html`
        // (a `border: 10px solid orange` BFC whose negative margins push it past
        // its parent) and `css/CSS2/positioning/absolute-non-replaced-height-006.xht`
        // (`border: 10px solid black; overflow: auto`) both lost exactly the
        // area of their own border when the box was clipped with its contents.
        let outer = PaintNode {
            kind: ComponentKind::Box,
            style: Style::default(),
            parent: None,
            sticky_counter_signal: None,
        };
        let clipping = PaintNode {
            kind: ComponentKind::Box,
            style: Style {
                overflow: Overflow::Hidden,
                border_width: 10.0,
                ..Style::default()
            },
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let child = PaintNode {
            kind: ComponentKind::Box,
            style: Style::default(),
            parent: Some(1),
            sticky_counter_signal: None,
        };
        let artifact = PaintArtifact::build(
            vec![outer, clipping, child],
            &[(rect(0.0), 0), (rect(0.0), 1), (rect(20.0), 2)],
            1,
        );

        let contents = artifact.node_properties[1].clip;
        assert_ne!(contents, 0, "the clipping box hands a clip to its contents");
        assert_eq!(
            artifact.node_properties[2].clip, contents,
            "the contents inherit the overflow clip"
        );
        assert_eq!(
            artifact.self_clip[1], 0,
            "the box paints its own border outside its overflow clip"
        );
        assert_eq!(
            artifact.self_clip[2], contents,
            "a descendant still paints under its parent's overflow clip"
        );
    }

    #[test]
    fn collapsible_inline_whitespace_is_trimmed_at_a_soft_line_start() {
        let mut parent_style = Style::default();
        parent_style.padding.left = w3cos_std::style::Spacing::Px(10.0);
        parent_style.border_left_width = Some(2.0);
        let mut inline_style = Style::default();
        inline_style.display = Display::Inline;
        w3cos_std::inline_text::set_fragment_ends(
            &mut inline_style, "  next line", &[7, 11],
        );
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
        assert_eq!(w3cos_std::inline_text::fragment_ends("next line", &nodes[1].style),
            Some(vec![5, 9]), "line-start trimming must rebase authored shaping boundaries");

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
    fn nested_inline_leading_space_is_not_trimmed_mid_line() {
        let mut inline_style = Style::default();
        inline_style.display = Display::Inline;
        let mut nodes = vec![
            PaintNode {
                kind: ComponentKind::Row,
                style: Style::default(),
                parent: None,
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Row,
                style: inline_style.clone(),
                parent: Some(0),
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Text {
                    content: " X".into(),
                },
                style: inline_style,
                parent: Some(1),
                sticky_counter_signal: None,
            },
        ];
        let mut rects = vec![
            Some(LayoutRect {
                x: 28.0,
                y: 0.0,
                width: 744.0,
                height: 20.0,
            }),
            Some(LayoutRect {
                x: 208.0,
                y: 0.0,
                width: 40.0,
                height: 20.0,
            }),
            Some(LayoutRect {
                x: 208.0,
                y: 0.0,
                width: 40.0,
                height: 20.0,
            }),
        ];

        trim_collapsible_inline_whitespace_at_line_start(&mut nodes, &rects);
        assert!(matches!(&nodes[2].kind, ComponentKind::Text { content } if content == " X"));

        for rect in &mut rects[1..] {
            rect.as_mut().unwrap().x = 28.0;
        }
        trim_collapsible_inline_whitespace_at_line_start(&mut nodes, &rects);
        assert!(matches!(&nodes[2].kind, ComponentKind::Text { content } if content == "X"));
    }

    #[test]
    fn explicit_inline_fragment_clip_keeps_layout_rect_and_clips_only_paint() {
        let mut style = Style::default();
        style.display = w3cos_std::style::Display::Inline;
        style.font_size = 20.0;
        style.line_height = 1.0;
        style
            .custom_properties
            .get_or_insert_with(Default::default)
            .insert(
                "--w3cos-internal-inline-fragment-clip".into(),
                "bottom 20".into(),
            );
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
    fn top_aligned_decorated_inline_keeps_its_vertical_border_paint() {
        let style = Style {
            display: Display::Inline,
            align_self: w3cos_std::style::AlignSelf::FlexStart,
            border_top_width: Some(4.0),
            border_bottom_width: Some(4.0),
            font_size: 16.0,
            line_height: 1.2,
            ..Style::default()
        };
        let rect = LayoutRect {
            x: 8.0,
            y: 5.6,
            width: 16.0,
            height: 24.0,
        };
        assert_eq!(
            inline_fragment_clip_rect(&ComponentKind::Row, &style, rect),
            None
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
    fn collapsed_column_color_defers_to_cell_and_row_owners() {
        let green = Color::rgb(0, 128, 0);
        let red = Color::rgb(255, 0, 0);
        let node = |display, width, color, parent| PaintNode {
            kind: ComponentKind::Box,
            style: Style {
                display,
                border_collapse: true,
                border_width: width,
                border_color: color,
                ..Style::default()
            },
            parent,
            sticky_counter_signal: None,
        };
        for (cell_owner, display) in [
            (false, Display::TableColumn),
            (true, Display::TableColumn),
            (false, Display::TableColumnGroup),
            (true, Display::TableColumnGroup),
        ] {
            let mut nodes = vec![
                node(Display::Table, 0.0, red, None),
                node(display, 25.0, red, Some(0)),
                node(
                    Display::TableRow,
                    if cell_owner { 0.0 } else { 25.0 },
                    green,
                    Some(0),
                ),
                node(
                    Display::TableCell,
                    if cell_owner { 25.0 } else { 0.0 },
                    green,
                    Some(2),
                ),
            ];
            resolve_collapsed_cell_border_conflicts(&mut nodes);
            assert_eq!(
                [
                    nodes[1].style.border_top_width,
                    nodes[1].style.border_right_width,
                    nodes[1].style.border_bottom_width,
                    nodes[1].style.border_left_width
                ],
                [Some(0.0); 4],
                "cell owner: {cell_owner}"
            );
            assert_eq!(
                [
                    nodes[3].style.border_top_color,
                    nodes[3].style.border_right_color,
                    nodes[3].style.border_bottom_color,
                    nodes[3].style.border_left_color
                ],
                [Some(green); 4],
                "cell owner: {cell_owner}"
            );
        }
    }

    #[test]
    fn hidden_table_parts_suppress_solid_cell_paint_edges() {
        use w3cos_std::style::BorderLineStyle;
        let node = |display, parent| PaintNode {
            kind: ComponentKind::Box,
            style: Style {
                display,
                border_collapse: true,
                ..Style::default()
            },
            parent,
            sticky_counter_signal: None,
        };
        for display in [
            Display::TableRow,
            Display::TableRowGroup,
            Display::TableColumn,
            Display::TableColumnGroup,
        ] {
            let mut nodes = vec![node(Display::Table, None)];
            let cell;
            if display == Display::TableRow {
                nodes.push(node(display, Some(0)));
                cell = 2;
                nodes.push(node(Display::TableCell, Some(1)));
            } else if display == Display::TableRowGroup {
                nodes.push(node(display, Some(0)));
                nodes.push(node(Display::TableRow, Some(1)));
                cell = 3;
                nodes.push(node(Display::TableCell, Some(2)));
            } else {
                nodes.push(node(display, Some(0)));
                nodes.push(node(Display::TableRow, Some(0)));
                cell = 3;
                nodes.push(node(Display::TableCell, Some(2)));
            }
            nodes[1].style.border_styles = [Some(BorderLineStyle::Hidden); 4];
            nodes[cell].style.border_styles = [Some(BorderLineStyle::Solid); 4];
            nodes[cell].style.border_width = 3.0;
            nodes[cell].style.border_color = Color::rgb(255, 0, 0);
            resolve_collapsed_cell_border_conflicts(&mut nodes);
            let style = &nodes[cell].style;
            assert_eq!(
                [
                    style.border_top_width,
                    style.border_right_width,
                    style.border_bottom_width,
                    style.border_left_width
                ],
                [Some(0.0); 4],
                "{display:?}"
            );
        }
    }

    #[test]
    fn collapsed_part_edges_preserve_owner_priority_and_nested_table_boundary() {
        let green = Color::rgb(0, 128, 0);
        let red = Color::rgb(255, 0, 0);
        let blue = Color::rgb(0, 0, 255);
        let node = |display, border_width, border_color, parent| PaintNode {
            kind: ComponentKind::Box,
            style: Style {
                display,
                border_collapse: true,
                border_width,
                border_color,
                ..Style::default()
            },
            parent,
            sticky_counter_signal: None,
        };
        let mut nodes = vec![
            node(Display::Table, 0.0, green, None),
            node(Display::TableRowGroup, 20.0, green, Some(0)),
            node(Display::TableRow, 20.0, red, Some(1)),
            node(Display::TableCell, 10.0, blue, Some(2)),
            node(Display::Table, 0.0, green, Some(1)),
            node(Display::TableRow, 0.0, green, Some(4)),
            node(Display::TableCell, 10.0, blue, Some(5)),
        ];
        nodes[3].style.border_top_width = Some(20.0);
        nodes[3].style.border_top_color = Some(blue);
        resolve_collapsed_cell_border_conflicts(&mut nodes);
        assert_eq!(nodes[3].style.border_top_color, Some(blue));
        assert_eq!(nodes[3].style.border_bottom_color, Some(red));
        assert_eq!(nodes[3].style.border_left_color, Some(red));
        assert_eq!(nodes[3].style.border_left_width, Some(20.0));
        assert_eq!(nodes[1].style.border_left_width, Some(0.0));
        assert_eq!(nodes[2].style.border_bottom_width, Some(0.0));
        assert_eq!(nodes[6].style.border_left_width, Some(10.0));
        assert_eq!(nodes[6].style.border_left_color, Some(blue));
    }

    #[test]
    fn collapsed_colspan_border_resolves_every_overlapping_track() {
        use w3cos_std::style::BorderLineStyle;
        let node = |display, parent, line_style| PaintNode {
            kind: ComponentKind::Box,
            style: Style { display, border_collapse: true,
                border_width: if display == Display::TableCell { 10.0 } else { 0.0 },
                border_color: Color::rgb(0, 128, 0),
                border_styles: [Some(line_style); 4], ..Style::default() },
            parent, sticky_counter_signal: None,
        };
        let mut nodes = vec![node(Display::Table, None, BorderLineStyle::Solid),
            node(Display::TableRow, Some(0), BorderLineStyle::Solid),
            node(Display::TableCell, Some(1), BorderLineStyle::Outset),
            node(Display::TableCell, Some(1), BorderLineStyle::Outset),
            node(Display::TableCell, Some(1), BorderLineStyle::Outset),
            node(Display::TableRow, Some(0), BorderLineStyle::Solid),
            node(Display::TableCell, Some(5), BorderLineStyle::Solid),
            node(Display::TableRow, Some(0), BorderLineStyle::Solid),
            node(Display::TableCell, Some(7), BorderLineStyle::Outset),
            node(Display::TableCell, Some(7), BorderLineStyle::Outset),
            node(Display::TableCell, Some(7), BorderLineStyle::Outset)];
        nodes[6].style.custom_properties.get_or_insert_with(Default::default)
            .insert("--w3cos-internal-table-column-span".into(), "3".into());
        resolve_collapsed_cell_border_conflicts(&mut nodes);
        for cell in [2, 3, 4] {
            assert_eq!(nodes[cell].style.border_bottom_color, Some(Color::TRANSPARENT),
                "upper track cell {cell} yields to the solid spanning cell");
        }
        for cell in [8, 9, 10] {
            assert_eq!(nodes[cell].style.border_top_color, Some(Color::TRANSPARENT),
                "lower track cell {cell} yields to the solid spanning cell");
        }
        assert_ne!(nodes[6].style.border_bottom_color, Some(Color::TRANSPARENT));
    }

    #[test]
    fn collapsed_rowspan_border_owns_neighbors_in_every_occupied_row() {
        use w3cos_std::style::BorderLineStyle;
        let node = |display, parent, line_style| PaintNode {
            kind: ComponentKind::Box,
            style: Style { display, border_collapse: true,
                border_width: if display == Display::TableCell { 10.0 } else { 0.0 },
                border_color: Color::rgb(0, 128, 0),
                border_styles: [Some(line_style); 4], ..Style::default() },
            parent, sticky_counter_signal: None,
        };
        let mut nodes = vec![node(Display::Table, None, BorderLineStyle::Solid),
            node(Display::TableRow, Some(0), BorderLineStyle::Solid),
            node(Display::TableCell, Some(1), BorderLineStyle::Outset),
            node(Display::TableCell, Some(1), BorderLineStyle::Solid),
            node(Display::TableCell, Some(1), BorderLineStyle::Outset),
            node(Display::TableRow, Some(0), BorderLineStyle::Solid),
            node(Display::TableCell, Some(5), BorderLineStyle::Outset),
            node(Display::TableCell, Some(5), BorderLineStyle::Outset),
            node(Display::TableRow, Some(0), BorderLineStyle::Solid),
            node(Display::TableCell, Some(8), BorderLineStyle::Outset),
            node(Display::TableCell, Some(8), BorderLineStyle::Outset)];
        nodes[3].style.custom_properties.get_or_insert_with(Default::default)
            .insert(crate::table_grid::ROW_SPAN.into(), "3".into());
        resolve_collapsed_cell_border_conflicts(&mut nodes);
        for cell in [2, 6, 9] {
            assert_eq!(nodes[cell].style.border_right_color, Some(Color::TRANSPARENT),
                "left neighbor {cell} yields to the solid rowspan edge");
        }
        for cell in [4, 7, 10] {
            assert_eq!(nodes[cell].style.border_left_color, Some(Color::TRANSPARENT));
        }
        assert_ne!(nodes[3].style.border_left_color, Some(Color::TRANSPARENT));
        assert_ne!(nodes[3].style.border_right_color, Some(Color::TRANSPARENT));
    }

    #[test]
    fn collapsed_part_style_priority_transfers_the_winning_line_style() {
        use w3cos_std::style::BorderLineStyle;
        let node = |display, parent, width, line_style| PaintNode {
            kind: ComponentKind::Box,
            style: Style { display, border_collapse: true, border_width: width,
                border_color: Color::rgb(0, 128, 0),
                border_styles: [Some(line_style); 4], ..Style::default() },
            parent, sticky_counter_signal: None,
        };
        for part_display in [Display::Table, Display::TableRow, Display::TableRowGroup,
            Display::TableColumn, Display::TableColumnGroup] {
            let mut nodes = vec![node(Display::Table, None, 0.0, BorderLineStyle::Solid)];
            let cell;
            if part_display == Display::Table {
                nodes[0].style.border_width = 10.0;
                nodes.push(node(Display::TableRow, Some(0), 0.0, BorderLineStyle::Solid));
                cell = 2;
                nodes.push(node(Display::TableCell, Some(1), 10.0, BorderLineStyle::Outset));
            } else if part_display == Display::TableRow {
                nodes.push(node(part_display, Some(0), 10.0, BorderLineStyle::Solid));
                cell = 2;
                nodes.push(node(Display::TableCell, Some(1), 10.0, BorderLineStyle::Outset));
            } else {
                nodes.push(node(part_display, Some(0), 10.0, BorderLineStyle::Solid));
                let parent = if part_display == Display::TableRowGroup { 1 } else { 0 };
                nodes.push(node(Display::TableRow, Some(parent), 0.0, BorderLineStyle::Solid));
                cell = 3;
                nodes.push(node(Display::TableCell, Some(2), 10.0, BorderLineStyle::Outset));
            }
            resolve_collapsed_cell_border_conflicts(&mut nodes);
            assert_eq!(nodes[cell].style.border_styles, [Some(BorderLineStyle::Solid); 4],
                "same-width solid {part_display:?} wins over cell outset and supplies the ink style");
        }
    }

    #[test]
    fn collapsed_grid_joints_use_neighbor_owner_and_extend_winning_edges() {
        use w3cos_std::style::BorderLineStyle;
        let node = |display, parent| PaintNode {
            kind: ComponentKind::Box,
            style: Style { display, border_collapse: true,
                border_width: if display == Display::TableCell { 10.0 } else { 0.0 },
                border_color: Color::rgb(0, 128, 0),
                border_styles: [Some(BorderLineStyle::Groove); 4], ..Style::default() },
            parent, sticky_counter_signal: None,
        };
        let mut nodes = vec![node(Display::Table, None), node(Display::TableRow, Some(0)),
            node(Display::TableCell, Some(1)), node(Display::TableCell, Some(1))];
        let rects = vec![None, None,
            Some(LayoutRect { x: 20.0, y: 30.0, width: 40.0, height: 30.0 }),
            Some(LayoutRect { x: 60.0, y: 30.0, width: 40.0, height: 30.0 })];
        resolve_collapsed_cell_border_conflicts(&mut nodes);
        annotate_collapsed_border_joints(&mut nodes, &rects);
        let first = border_edge_paint_rects(&nodes[2].style, rects[2].unwrap(), [10.0; 4]);
        let second = border_edge_paint_rects(&nodes[3].style, rects[3].unwrap(), [10.0; 4]);
        assert_eq!((first[0].x, first[0].width), (15.0, 50.0),
            "winning horizontal edge fills both joint quadrants");
        assert_eq!((first[1].y, first[1].height), (25.0, 40.0));
        assert_eq!((second[0].x, second[0].width), (65.0, 40.0),
            "later cell's horizontal edge yields its start joint to the earlier owner");
    }

    #[test]
    fn collapsed_wider_inline_edge_owns_the_block_edge_junction() {
        let style = Style {
            display: Display::TableCell,
            border_collapse: true,
            ..Style::default()
        };
        let rect = LayoutRect { x: 20.0, y: 30.0, width: 40.0, height: 30.0 };
        let edges = border_edge_paint_rects(&style, rect, [10.0, 11.0, 10.0, 11.0]);
        // The wider inline borders win at both horizontal junctions, rather
        // than letting the later-painted bottom edge cover their inner halves.
        for horizontal in [edges[0], edges[2]] {
            assert_eq!(horizontal.x, 25.5);
            assert_eq!(horizontal.width, 29.0);
        }
    }

    #[test]
    fn collapsed_equal_width_border_prefers_solid_over_outset() {
        use w3cos_std::style::BorderLineStyle;
        let node = |display, parent, line_style| PaintNode {
            kind: ComponentKind::Box,
            style: Style { display, border_collapse: true, border_width: 10.0,
                border_color: Color::rgb(0, 128, 0),
                border_styles: [Some(line_style); 4], ..Style::default() },
            parent, sticky_counter_signal: None,
        };
        let mut nodes = vec![
            node(Display::TableRow, None, BorderLineStyle::None),
            node(Display::TableCell, Some(0), BorderLineStyle::Outset),
            node(Display::TableCell, Some(0), BorderLineStyle::Solid),
        ];
        nodes[0].style.border_width = 0.0;
        resolve_collapsed_cell_border_conflicts(&mut nodes);
        assert_eq!(nodes[1].style.border_right_color, Some(Color::TRANSPARENT),
            "equal-width solid must win before directional cell-owner precedence");
        assert_eq!(nodes[2].style.border_left_color, Some(Color::rgb(0, 128, 0)));
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
    fn collapsed_cell_visual_bounds_include_centered_inline_borders() {
        let node = PaintNode {
            kind: ComponentKind::Box,
            style: Style {
                display: Display::TableCell,
                border_collapse: true,
                border_left_width: Some(25.0),
                border_right_width: Some(25.0),
                ..Style::default()
            },
            parent: None,
            sticky_counter_signal: None,
        };
        let rect = LayoutRect {
            x: 12.5,
            y: 0.0,
            width: 75.0,
            height: 100.0,
        };
        let artifact = PaintArtifact::build(vec![node], &[(rect, 0)], 1);
        assert_eq!(
            artifact.display_items[0].visual_rect,
            LayoutRect {
                x: 0.0,
                width: 100.0,
                ..rect
            }
        );
        assert_eq!(artifact.rect_by_index[0], Some(rect));
    }

    #[test]
    fn collapsed_table_background_origin_uses_resolved_outer_edges() {
        let node = |display, border_width, parent| PaintNode {
            kind: ComponentKind::Box,
            style: Style {
                display,
                border_collapse: true,
                border_width,
                border_color: Color::rgb(0, 128, 0),
                ..Style::default()
            },
            parent,
            sticky_counter_signal: None,
        };
        let mut nodes = vec![
            node(Display::Table, 2.0, None),
            node(Display::TableRow, 0.0, Some(0)),
            node(Display::TableCell, 6.0, Some(1)),
        ];
        resolve_collapsed_cell_border_conflicts(&mut nodes);
        let rect = LayoutRect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        };
        assert_eq!(
            box_background_positioning_rect(&nodes[0].style, rect),
            Some(LayoutRect {
                x: 3.0,
                y: 3.0,
                width: 94.0,
                height: 94.0
            })
        );
        assert_eq!(nodes[0].style.border_left_color, Some(Color::TRANSPARENT));
        assert_eq!(
            nodes[2].style.border_left_color,
            Some(Color::rgb(0, 128, 0))
        );
    }

    #[test]
    fn collapsed_cell_background_retains_centered_inline_grid_bounds() {
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
                    // Both axes now use shared grid-line centers.
                    x: 138.0,
                    y: 55.0,
                    width: 57.0,
                    height: 19.0,
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
    fn collapsed_columns_preserve_partial_span_overflow_and_hide_full_cells() {
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
        assert_eq!(nodes[5].style.overflow_x, None,
            "Chromium preserves authored overflow for partially collapsed spans");
        assert_eq!(nodes[9].style.visibility, Visibility::Collapse);
        assert_eq!(nodes[9].style.overflow_x, Some(Overflow::Hidden));
        assert_eq!(nodes[9].style.overflow_y, Some(Overflow::Hidden));
        nodes[5].style.overflow_x = Some(Overflow::Hidden);
        project_collapsed_table_tracks_to_cells(&mut nodes);
        assert_eq!(nodes[5].style.overflow_x, Some(Overflow::Hidden),
            "explicit author clipping is not removed");
    }

    #[test]
    fn table_background_paint_rect_uses_grid_width_not_wider_caption() {
        for bottom in [false, true] {
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
                        display: Display::TableCaption,
                        caption_side_bottom: bottom,
                        ..Style::default()
                    },
                    parent: Some(0),
                    sticky_counter_signal: None,
                },
                PaintNode {
                    kind: ComponentKind::Box,
                    style: Style {
                        display: Display::TableRowGroup,
                        ..Style::default()
                    },
                    parent: Some(0),
                    sticky_counter_signal: None,
                },
            ];
            let grid_y = if bottom { 0.0 } else { 30.0 };
            let rects = vec![
                Some(LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: 192.0,
                    height: 60.0,
                }),
                Some(LayoutRect {
                    x: 0.0,
                    y: if bottom { 30.0 } else { 0.0 },
                    width: 192.0,
                    height: 30.0,
                }),
                Some(LayoutRect {
                    x: 0.0,
                    y: grid_y,
                    width: 100.0,
                    height: 30.0,
                }),
            ];
            annotate_table_caption_paint_insets(&mut nodes, &rects);
            assert_eq!(
                table_grid_paint_rect(&nodes[0].style, rects[0].unwrap()),
                LayoutRect {
                    x: 0.0,
                    y: grid_y,
                    width: 100.0,
                    height: 30.0
                }
            );
        }
    }

    #[test]
    fn empty_top_caption_does_not_expand_table_overflow_clip() {
        let node = |display, parent: Option<usize>| PaintNode {
            kind: ComponentKind::Box,
            style: Style {
                display,
                overflow: if parent.is_none() {
                    Overflow::Hidden
                } else {
                    Overflow::Visible
                },
                ..Style::default()
            },
            parent,
            sticky_counter_signal: None,
        };
        let artifact = PaintArtifact::build(
            vec![
                node(Display::Table, None),
                node(Display::TableCaption, Some(0)),
                node(Display::TableRow, Some(0)),
                node(Display::TableCell, Some(2)),
                node(Display::Block, Some(3)),
            ],
            &[
                (
                    LayoutRect {
                        x: 8.0,
                        y: 51.0,
                        width: 20.0,
                        height: 30.0,
                    },
                    0,
                ),
                (
                    LayoutRect {
                        x: 8.0,
                        y: 51.0,
                        width: 20.0,
                        height: 0.0,
                    },
                    1,
                ),
                (
                    LayoutRect {
                        x: 8.0,
                        y: 61.0,
                        width: 20.0,
                        height: 20.0,
                    },
                    2,
                ),
                (
                    LayoutRect {
                        x: 8.0,
                        y: 61.0,
                        width: 20.0,
                        height: 20.0,
                    },
                    3,
                ),
                (
                    LayoutRect {
                        x: 8.0,
                        y: 46.0,
                        width: 20.0,
                        height: 35.0,
                    },
                    4,
                ),
            ],
            1,
        );
        let clip = artifact.node_properties[0].clip;
        assert_eq!(artifact.properties.clips[clip].rect.unwrap().y, 61.0);
        assert_eq!(
            artifact.node_properties[1].clip, 0,
            "caption lives outside table grid"
        );
        assert_eq!(artifact.node_properties[4].clip, clip);
    }

    #[test]
    fn table_grid_border_tracks_a_row_overlapped_by_the_top_caption() {
        let mut nodes = vec![
            PaintNode {
                kind: ComponentKind::Box,
                style: Style {
                    display: Display::Table,
                    border_top_width: Some(16.0),
                    ..Style::default()
                },
                parent: None,
                sticky_counter_signal: None,
            },
            PaintNode {
                kind: ComponentKind::Box,
                style: Style {
                    display: Display::TableCaption,
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
                parent: Some(0),
                sticky_counter_signal: None,
            },
        ];
        let rects = vec![
            Some(LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 200.0,
                height: 119.2,
            }),
            Some(LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 200.0,
                height: 22.4,
            }),
            Some(LayoutRect {
                x: 0.0,
                y: 19.2,
                width: 200.0,
                height: 100.0,
            }),
        ];
        annotate_table_caption_paint_insets(&mut nodes, &rects);
        let grid = table_grid_paint_rect(&nodes[0].style, rects[0].unwrap());
        assert!(
            (grid.y - 3.2).abs() < 0.01,
            "grid border precedes the first row: {grid:?}"
        );
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
    fn separated_row_group_background_excludes_cell_spacing() {
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
                .is_some_and(|properties| properties.contains_key(TABLE_BACKGROUND_FRAGMENTS))
        );
        assert_eq!(
            box_background_paint_rects(&nodes[1].style, row_group),
            vec![LayoutRect {
                width: 90.0,
                ..row_group
            }]
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
    fn display_none_root_suppresses_root_and_body_canvas_backgrounds() {
        for background in [Color::rgb(0, 128, 0), Color::TRANSPARENT] {
            let root = PaintNode {
                kind: ComponentKind::Column,
                style: Style { display: Display::None, background, ..Style::default() },
                parent: None,
                sticky_counter_signal: None,
            };
            let body = PaintNode {
                kind: ComponentKind::Column,
                style: Style {
                    background: Color::rgb(255, 0, 0),
                    background_image: Some("url(square-white.png)".into()),
                    ..Style::default()
                },
                parent: Some(0),
                sticky_counter_signal: None,
            };
            let artifact = PaintArtifact::build_with_body_background(
                [root, body], &[], 1, Some(1),
            );
            assert_eq!(artifact.canvas_background, Color::WHITE);
            assert_eq!(artifact.canvas_background_source, None);
            assert!(artifact.canvas_background_style.is_none());
        }
    }

    #[test]
    fn display_none_body_does_not_propagate_its_canvas_background() {
        let node = |parent, style| PaintNode {
            kind: ComponentKind::Column, style, parent, sticky_counter_signal: None,
        };
        let artifact = PaintArtifact::build_with_body_background(
            [node(None, Style::default()), node(Some(0), Style {
                display: Display::None,
                background: Color::rgb(0, 128, 0),
                background_image: Some("url(square-white.png)".into()),
                ..Style::default()
            })], &[(rect(0.0), 0)], 1, Some(1),
        );
        assert_eq!(artifact.canvas_background, Color::WHITE);
        assert_eq!(artifact.canvas_background_source, None);
        assert!(artifact.canvas_background_style.is_none());
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
    fn transparent_block_text_paints_after_later_in_flow_block_background() {
        let root = PaintNode {
            kind: ComponentKind::Column,
            style: Style::default(),
            parent: None,
            sticky_counter_signal: None,
        };
        let mut text_style = Style::default();
        text_style.display = Display::Block;
        let text = PaintNode {
            kind: ComponentKind::Text {
                content: "instruction".into(),
            },
            style: text_style,
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let mut bar_style = Style::default();
        bar_style.background = Color::rgb(255, 165, 0);
        let bar = PaintNode {
            kind: ComponentKind::Box,
            style: bar_style,
            parent: Some(0),
            sticky_counter_signal: None,
        };
        let artifact = PaintArtifact::build(
            [root, text, bar],
            &[(rect(0.0), 0), (rect(0.0), 1), (rect(0.0), 2)],
            1,
        );

        assert!(artifact.paint_order_key(2) < artifact.paint_order_key(1));
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
        assert_ne!(artifact.node_properties[1].scroll, 0);
        assert_eq!(artifact.node_properties[2].scroll, 0);
    }

    #[test]
    fn body_background_propagates_through_anonymous_table_wrappers() {
        let node = |parent, display, background| PaintNode {
            kind: ComponentKind::Box,
            style: Style {
                display,
                background,
                ..Style::default()
            },
            parent,
            sticky_counter_signal: None,
        };
        let nodes = [
            node(None, Display::Table, Color::TRANSPARENT),
            node(Some(0), Display::TableRow, Color::TRANSPARENT),
            node(Some(1), Display::TableCell, Color::TRANSPARENT),
            node(Some(2), Display::Block, Color::rgb(255, 255, 0)),
        ];
        let artifact = PaintArtifact::build_with_body_background(
            nodes,
            &[
                (rect(0.0), 0),
                (rect(0.0), 1),
                (rect(0.0), 2),
                (rect(0.0), 3),
            ],
            1,
            Some(3),
        );
        assert_eq!(artifact.canvas_background_source, Some(3));
        assert_eq!(artifact.canvas_background, Color::rgb(255, 255, 0));
        assert_eq!(artifact.nodes[3].style.background, Color::TRANSPARENT);
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
    fn nonvisible_css_borders_use_zero_width_paint_snapshots() {
        use w3cos_std::style::BorderLineStyle;
        for line_style in [
            Some(BorderLineStyle::None),
            Some(BorderLineStyle::Hidden),
            Some(BorderLineStyle::Solid),
            None,
        ] {
            let source = Style {
                border_width: 32.0,
                border_styles: [line_style; 4],
                ..Style::default()
            };
            let artifact = PaintArtifact::build(
                [PaintNode {
                    kind: ComponentKind::Column,
                    style: source.clone(),
                    parent: None,
                    sticky_counter_signal: None,
                }],
                &[(rect(0.0), 0)],
                1,
            );
            let used = &artifact.nodes[0].style;
            let expected = if line_style.is_some_and(|style| !style.is_visible()) {
                0.0
            } else {
                32.0
            };
            for width in [
                used.border_top_width,
                used.border_right_width,
                used.border_bottom_width,
                used.border_left_width,
            ] {
                assert_eq!(
                    width.unwrap_or(used.border_width),
                    expected,
                    "{line_style:?}"
                );
            }
            assert_eq!(
                used.border_styles, source.border_styles,
                "hidden identity retained"
            );
            assert_eq!(source.border_width, 32.0, "computed width retained");
        }
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
        assert!(artifact.column_fragments.is_empty(), "ordinary pages do not allocate per-node column slices");
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
    fn fixed_auto_multicol_artifact_retains_physical_fragments_and_source_identity() {
        use w3cos_std::style::{ColumnFill, Dimension};
        let node = |style, parent| PaintNode {
            kind: ComponentKind::Column, style, parent, sticky_counter_signal: None,
        };
        let artifact = PaintArtifact::build([
            node(Style { display: Display::Block, width: Dimension::Px(300.0),
                height: Dimension::Px(100.0), column_width: Dimension::Px(100.0),
                column_gap: Some(0.0), column_fill: ColumnFill::Auto,
                ..Style::default() }, None),
            node(Style { display: Display::Block, ..Style::default() }, Some(0)),
            node(Style { display: Display::Block, position: Position::Absolute,
                ..Style::default() }, Some(0)),
        ], &[
            (LayoutRect { x: 20.0, y: 30.0, width: 300.0, height: 100.0 }, 0),
            (LayoutRect { x: 20.0, y: 30.0, width: 100.0, height: 250.0 }, 1),
            (LayoutRect { x: 20.0, y: 30.0, width: 20.0, height: 250.0 }, 2),
        ], 1);
        assert_eq!(artifact.nodes.len(), 3, "fragments do not duplicate source nodes");
        let fragments: Vec<_> = artifact.display_items.iter()
            .filter(|item| item.client_index == 1).map(|item| item.visual_rect).collect();
        assert_eq!(fragments, vec![
            LayoutRect { x: 20.0, y: 30.0, width: 100.0, height: 100.0 },
            LayoutRect { x: 120.0, y: 30.0, width: 100.0, height: 100.0 },
            LayoutRect { x: 220.0, y: 30.0, width: 100.0, height: 50.0 },
        ]);
        assert_eq!(artifact.display_items.iter().filter(|item| item.client_index == 2).count(), 1,
            "absolute children do not flow through columns");
        assert_eq!(artifact.rect_by_index[1].unwrap().height, 250.0,
            "logical source geometry remains available");
    }

    #[test]
    fn balanced_multicol_artifact_uses_shorter_fragmentainers_with_fixed_principal_height() {
        use w3cos_std::style::{ColumnFill, Dimension};
        let node = |style, parent| PaintNode {
            kind: ComponentKind::Column, style, parent, sticky_counter_signal: None,
        };
        let artifact = PaintArtifact::build([
            node(Style { display: Display::Block, width: Dimension::Px(300.0),
                height: Dimension::Px(100.0), column_width: Dimension::Px(100.0),
                column_gap: Some(0.0), column_fill: ColumnFill::Balance,
                ..Style::default() }, None),
            node(Style { display: Display::Block, ..Style::default() }, Some(0)),
        ], &[
            (LayoutRect { x: 20.0, y: 30.0, width: 300.0, height: 100.0 }, 0),
            (LayoutRect { x: 20.0, y: 30.0, width: 100.0, height: 255.0 }, 1),
        ], 1);
        let fragments: Vec<_> = artifact.display_items.iter()
            .filter(|item| item.client_index == 1).map(|item| item.visual_rect).collect();
        assert_eq!(fragments, vec![
            LayoutRect { x: 20.0, y: 30.0, width: 100.0, height: 85.0 },
            LayoutRect { x: 120.0, y: 30.0, width: 100.0, height: 85.0 },
            LayoutRect { x: 220.0, y: 30.0, width: 100.0, height: 85.0 },
        ]);
        assert_eq!(artifact.rect_by_index[0].unwrap().height, 100.0,
            "balancing shortens fragmentainers, not an explicit principal height");
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
    fn bidi_visual_fragments_paint_in_logical_order_without_changing_layout() {
        let mut nodes = vec![PaintNode {
            kind: ComponentKind::Row,
            style: Style::default(),
            parent: None,
            sticky_counter_signal: None,
        }];
        for rank in [2, 1, 0] {
            let mut style = Style::default();
            style.display = Display::Inline;
            style.custom_properties = Some(std::collections::HashMap::from([(
                "--w3cos-internal-bidi-logical-order".to_string(),
                rank.to_string(),
            )]));
            nodes.push(PaintNode {
                kind: ComponentKind::Text {
                    content: rank.to_string(),
                },
                style,
                parent: Some(0),
                sticky_counter_signal: None,
            });
        }
        let artifact = PaintArtifact::build(
            nodes,
            &[
                (rect(0.0), 0),
                (rect(0.0), 1),
                (rect(20.0), 2),
                (rect(40.0), 3),
            ],
            1,
        );
        assert!(artifact.paint_order_key(3) < artifact.paint_order_key(2));
        assert!(artifact.paint_order_key(2) < artifact.paint_order_key(1));
        assert_eq!(artifact.rect_by_index[1], Some(rect(0.0)));
        assert_eq!(artifact.nodes[1].style.z_index, 0);
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

        assert!(artifact.paint_order_key(0) < artifact.paint_order_key(2));
        assert!(artifact.paint_order_key(2) < artifact.paint_order_key(1),
            "positioned-auto subtree must paint after later normal-flow content");
    }

    #[test]
    fn auto_positioned_container_paints_normal_blocks_before_positioned_children() {
        let nodes = [
            (None, Position::Static),
            (Some(0), Position::Relative),
            (Some(1), Position::Absolute),
            (Some(1), Position::Static),
        ]
        .map(|(parent, position)| PaintNode {
            kind: ComponentKind::Box,
            style: Style {
                position,
                ..Style::default()
            },
            parent,
            sticky_counter_signal: None,
        });
        let artifact = PaintArtifact::build(
            nodes,
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
    fn inline_border_paints_after_preceding_block_text_and_before_following_text() {
        for split in [false, true] {
            let mut border = Style::default();
            border.display = Display::Inline;
            border.line_height_is_normal = true;
            border.border_width = 3.0;
            if split {
                border.custom_properties = Some(std::collections::HashMap::from([(
                    "--w3cos-internal-split-inline-edge".to_string(), "1".to_string(),
                )]));
            }
            let mut text = Style::default();
            text.display = Display::Block;
            let artifact = PaintArtifact::build(
                [
                    PaintNode { kind: ComponentKind::Column, style: Style::default(), parent: None, sticky_counter_signal: None },
                    PaintNode { kind: ComponentKind::Text { content: "Eight".into() }, style: text.clone(), parent: Some(0), sticky_counter_signal: None },
                    PaintNode { kind: ComponentKind::Row, style: border, parent: Some(0), sticky_counter_signal: None },
                    PaintNode { kind: ComponentKind::Text { content: "Nine".into() }, style: text, parent: Some(0), sticky_counter_signal: None },
                ],
                &[(rect(0.0), 0), (rect(0.0), 1), (rect(0.0), 2), (rect(0.0), 3)],
                1,
            );
            assert!(artifact.paint_order_key(1) < artifact.paint_order_key(2), "inline border must cover preceding block glyph overflow, split={split}");
            assert!(artifact.paint_order_key(2) < artifact.paint_order_key(3), "inline border must remain behind following text, split={split}");
        }
    }

    #[test]
    fn split_inline_edge_paints_behind_following_block_text() {
        let mut edge_style = Style::default();
        edge_style.display = Display::Inline;
        edge_style.custom_properties = Some(std::collections::HashMap::from([(
            "--w3cos-internal-split-inline-edge".to_string(),
            "1".to_string(),
        )]));
        let mut text_style = Style::default();
        text_style.display = Display::Block;
        let artifact = PaintArtifact::build(
            [
                PaintNode {
                    kind: ComponentKind::Column,
                    style: Style::default(),
                    parent: None,
                    sticky_counter_signal: None,
                },
                PaintNode {
                    kind: ComponentKind::Row,
                    style: edge_style,
                    parent: Some(0),
                    sticky_counter_signal: None,
                },
                PaintNode {
                    kind: ComponentKind::Text {
                        content: "Second line".into(),
                    },
                    style: text_style,
                    parent: Some(0),
                    sticky_counter_signal: None,
                },
            ],
            &[(rect(0.0), 0), (rect(0.0), 1), (rect(0.0), 2)],
            1,
        );
        assert!(
            artifact.paint_order_key(1) < artifact.paint_order_key(2),
            "the first split inline edge must stay behind the following block's text"
        );
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
