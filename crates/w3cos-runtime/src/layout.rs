use anyhow::Result;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::OnceLock;
use taffy::prelude::*;
use w3cos_std::component::EventAction;
use w3cos_std::style::{
    AlignContent as WAlignContent, AlignItems as WAlign, AlignSelf as WAlignSelf,
    BoxSizing as WBoxSizing, Clear as WClear, Dimension as WDim, Display as WDisplay, EdgeLengths,
    FlexDirection as WDir, FlexWrap as WWrap, Float as WFloat, JustifyContent as WJustify,
    Overflow as WOverflow, Position as WPos, Spacing as WSpacing, Visibility as WVisibility,
    WhiteSpace as WWhiteSpace, WordBreak as WWordBreak,
};
use w3cos_std::{Component, ComponentKind};

use crate::text_layout;

const ROOT_FONT_SIZE: f32 = 16.0;
/// Typical mobile content width for pre-wrap intrinsic sizing.
const DEFAULT_TEXT_WRAP_WIDTH: f32 = 360.0;

static LAYOUT_FONT: OnceLock<fontdue::Font> = OnceLock::new();

const TEXT_MEASURE_CACHE_CAPACITY: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TextMeasureKey {
    width: u32,
    font: u64,
    font_size: u32,
    line_height: u32,
    padding_top: u32,
    padding_right: u32,
    padding_bottom: u32,
    padding_left: u32,
    min_width: Option<u32>,
    white_space: u8,
    word_break: u8,
}

#[derive(Default)]
struct TextMeasureCache {
    intrinsic: HashMap<String, Vec<(TextMeasureKey, (f32, f32))>>,
    wrapped_height: HashMap<String, Vec<(TextMeasureKey, f32)>>,
    entries: usize,
}

impl TextMeasureCache {
    fn make_room(&mut self) {
        if self.entries >= TEXT_MEASURE_CACHE_CAPACITY {
            self.intrinsic.clear();
            self.wrapped_height.clear();
            self.entries = 0;
        }
    }
}

thread_local! {
    /// Blink keeps font metrics and shaped text runs across layout passes. This
    /// bounded per-UI-thread cache provides the same retained-measure behavior
    /// without coupling the layout engine to a particular application tree.
    static TEXT_MEASURE_CACHE: RefCell<TextMeasureCache> = RefCell::new(TextMeasureCache::default());
}

pub(crate) fn layout_font() -> &'static fontdue::Font {
    LAYOUT_FONT.get_or_init(|| crate::font_face::host_ui_font().font.clone())
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct ScrollExtent {
    pub max_x: f32,
    pub max_y: f32,
}

// ---------------------------------------------------------------------------
// FlatNodeInfo — O(1) indexed access to tree data (replaces O(n) recursive lookups)
// ---------------------------------------------------------------------------

pub struct FlatNodeInfo<'a> {
    /// Stable identity for this compiled tree slot across reactive rebuilds.
    /// The compiler keeps conditional branches mounted and only toggles
    /// display, so a structural path is stable even when visibility changes.
    pub stable_id: u64,
    pub kind: &'a ComponentKind,
    pub style: &'a w3cos_std::style::Style,
    pub on_click: &'a EventAction,
    pub sticky_counter_signal: Option<usize>,
    pub parent: Option<usize>,
}

pub fn pre_flatten(root: &Component) -> Vec<FlatNodeInfo<'_>> {
    let n = count_nodes(root);
    let mut out = Vec::with_capacity(n);
    pre_flatten_recursive(root, None, 0xcbf2_9ce4_8422_2325, &mut out);
    out
}

fn count_nodes(comp: &Component) -> usize {
    1 + comp.children.iter().map(count_nodes).sum::<usize>()
}

/// Leaf intrinsic size used by Taffy (must stay in sync with `build_taffy_tree`).
fn image_intrinsic_ratio(src: &str) -> Option<f32> {
    use crate::image_loader::SvgIntrinsicLength;

    if let Some(size) = crate::image_loader::svg_intrinsic_size(src) {
        return match (size.width, size.height) {
            (SvgIntrinsicLength::Px(width), SvgIntrinsicLength::Px(height)) if height > 0.0 => {
                Some(width / height)
            }
            _ => size.ratio,
        };
    }
    crate::image_loader::dimensions(src).and_then(|(width, height)| {
        (width > 0 && height > 0).then_some(width as f32 / height as f32)
    })
}

fn image_intrinsic_size(
    src: &str,
    style: &w3cos_std::style::Style,
    containing_width: Option<f32>,
) -> (f32, f32) {
    use crate::image_loader::SvgIntrinsicLength;

    let decoded = crate::image_loader::dimensions(src)
        .map(|(width, height)| (width as f32, height as f32))
        .unwrap_or_else(|| {
            if crate::image_loader::is_reserved_browser_source(src) {
                (0.0, 0.0)
            } else {
                (200.0, 200.0)
            }
        });
    let metadata = crate::image_loader::svg_intrinsic_size(src);
    let ratio = image_intrinsic_ratio(src);
    let (intrinsic_width, intrinsic_height) = if let Some(size) = metadata {
        let width = match size.width {
            SvgIntrinsicLength::Px(value) => Some(value),
            SvgIntrinsicLength::Auto | SvgIntrinsicLength::Percent(_) => None,
        };
        let height = match size.height {
            SvgIntrinsicLength::Px(value) => Some(value),
            SvgIntrinsicLength::Auto | SvgIntrinsicLength::Percent(_) => None,
        };
        match (width, height, ratio) {
            (Some(width), Some(height), _) => (width, height),
            (Some(width), None, Some(ratio)) if ratio > 0.0 => (width, width / ratio),
            (None, Some(height), Some(ratio)) if ratio > 0.0 => (height * ratio, height),
            (Some(width), None, _) => (width, 150.0),
            (None, Some(height), _) => (300.0, height),
            (None, None, Some(ratio)) if ratio > 0.0 => {
                let width = containing_width.unwrap_or(300.0).min(300.0).max(0.0);
                (width, width / ratio)
            }
            (None, None, _) => (
                containing_width.unwrap_or(300.0).clamp(0.0, 300.0),
                150.0,
            ),
        }
    } else {
        decoded
    };

    let width = dim_to_px(style.width);
    let height = dim_to_px(style.height);
    let (mut used_width, mut used_height) = match (width, height, ratio) {
        (Some(width), Some(height), _) => (width, height),
        (Some(width), None, Some(ratio)) if ratio > 0.0 => (width, width / ratio),
        (None, Some(height), Some(ratio)) if ratio > 0.0 => (height * ratio, height),
        (Some(width), None, _) => (width, intrinsic_height),
        (None, Some(height), _) => (intrinsic_width, height),
        (None, None, _) => (intrinsic_width, intrinsic_height),
    };

    let px = |dimension| match dimension {
        WDim::Px(value) => Some(value.max(0.0)),
        _ => None,
    };
    let min_width = px(style.min_width);
    let min_height = px(style.min_height);
    let max_width = px(style.max_width);
    let max_height = px(style.max_height);
    if width.is_none()
        && height.is_none()
        && let Some(ratio) = ratio
        && ratio > 0.0
        && used_width > 0.0
        && used_height > 0.0
    {
        let mut scale_min = 0.0_f32;
        let mut scale_max = f32::INFINITY;
        if let Some(value) = min_width {
            scale_min = scale_min.max(value / used_width);
        }
        if let Some(value) = min_height {
            scale_min = scale_min.max(value / used_height);
        }
        if let Some(value) = max_width {
            scale_max = scale_max.min(value / used_width);
        }
        if let Some(value) = max_height {
            scale_max = scale_max.min(value / used_height);
        }
        let scale = 1.0_f32.max(scale_min).min(scale_max);
        used_width *= scale;
        used_height *= scale;
    } else {
        if let Some(value) = min_width {
            used_width = used_width.max(value);
        }
        if let Some(value) = max_width {
            used_width = used_width.min(value);
        }
        if width.is_some()
            && height.is_none()
            && let Some(ratio) = ratio.filter(|ratio| *ratio > 0.0)
        {
            used_height = used_width / ratio;
        }
        if let Some(value) = min_height {
            used_height = used_height.max(value);
        }
        if let Some(value) = max_height {
            used_height = used_height.min(value);
        }
        if height.is_some()
            && width.is_none()
            && let Some(ratio) = ratio.filter(|ratio| *ratio > 0.0)
        {
            used_width = used_height * ratio;
        }
    }
    (used_width, used_height)
}

fn leaf_intrinsic_size_with_containing(
    kind: &ComponentKind,
    style: &w3cos_std::style::Style,
    containing_width: Option<f32>,
) -> (f32, f32) {
    match kind {
        ComponentKind::Text { content } => text_intrinsic_size(content, style),
        ComponentKind::Button { label } => button_intrinsic_size(label, style),
        ComponentKind::Image { src } => image_intrinsic_size(src, style, containing_width),
        ComponentKind::TextInput { .. } => {
            // Match the browser UA baseline for an `<input size="20">`
            // instead of imposing the former mobile-only 200×40 control.
            let w = dim_to_px(style.width).unwrap_or(169.0);
            let h = dim_to_px(style.height).unwrap_or(20.0);
            (w, h)
        }
        ComponentKind::Canvas { width, height } => (*width as f32, *height as f32),
        ComponentKind::SvgDocument { width, height, .. } => (*width as f32, *height as f32),
        _ => (0.0, 0.0),
    }
}

fn leaf_intrinsic_size(kind: &ComponentKind, style: &w3cos_std::style::Style) -> (f32, f32) {
    leaf_intrinsic_size_with_containing(kind, style, None)
}

fn component_max_content_width(component: &Component) -> f32 {
    let definite_content_height = match component.style.height {
        WDim::Px(height) => Some(height),
        WDim::Em(height) => Some(height * component.style.font_size),
        WDim::Rem(height) => Some(height * ROOT_FONT_SIZE),
        _ => None,
    }
    .map(|height| {
        if component.style.box_sizing == WBoxSizing::BorderBox {
            let padding = component.style.padding_lengths();
            (height
                - padding.top
                - padding.bottom
                - component
                    .style
                    .border_top_width
                    .unwrap_or(component.style.border_width)
                - component
                    .style
                    .border_bottom_width
                    .unwrap_or(component.style.border_width))
            .max(0.0)
        } else {
            height.max(0.0)
        }
    });
    let child_width = |child: &Component| {
        let intrinsic_width = component_max_content_width(child);
        let ratio = match &child.kind {
            ComponentKind::Image { src } => image_intrinsic_ratio(src),
            ComponentKind::Canvas { width, height } if *height > 0 => {
                Some(*width as f32 / *height as f32)
            }
            _ => None,
        };
        let percentage_replaced_width = match (
            child.style.width,
            child.style.height,
            child.style.min_width,
            child.style.max_width,
            definite_content_height,
            ratio,
        ) {
            (
                WDim::Auto,
                WDim::Percent(percent),
                WDim::Auto,
                WDim::Auto,
                Some(height),
                Some(ratio),
            ) => Some(height * percent / 100.0 * ratio),
            _ => None,
        };
        if let Some(width) = percentage_replaced_width {
            let intrinsic_content_width = leaf_intrinsic_size(&child.kind, &child.style).0;
            intrinsic_width + width - intrinsic_content_width
        } else {
            intrinsic_width
        }
    };
    let establishes_block_formatting_context = matches!(
        component.style.display,
        WDisplay::Block | WDisplay::ListItem | WDisplay::TableCell
    ) || (matches!(
        component.style.display,
        WDisplay::InlineBlock | WDisplay::InlineTable
    )
        && component.children.iter().any(|child| {
            matches!(
                child.style.display,
                WDisplay::Block
                    | WDisplay::Flex
                    | WDisplay::Grid
                    | WDisplay::Table
                    | WDisplay::ListItem
            )
        }));
    let intrinsic_width = if component.children.is_empty() {
        leaf_intrinsic_size(&component.kind, &component.style).0
    } else {
        match component.style.display {
            WDisplay::Table
            | WDisplay::TableRowGroup
            | WDisplay::TableHeaderGroup
            | WDisplay::TableFooterGroup => table_track_max_content_width(component),
            WDisplay::InlineTable
                if component.children.iter().any(|child| {
                    matches!(
                        child.style.display,
                        WDisplay::TableRow
                            | WDisplay::TableRowGroup
                            | WDisplay::TableHeaderGroup
                            | WDisplay::TableFooterGroup
                    )
                }) =>
            {
                component
                    .children
                    .iter()
                    .map(child_width)
                    .fold(0.0_f32, f32::max)
            }
            WDisplay::TableRow => {
                let children = component.children.iter().map(child_width).sum::<f32>();
                children + component.style.gap * component.children.len().saturating_sub(1) as f32
            }
            _ if establishes_block_formatting_context => {
                // A block formatting context contributes the widest generated
                // line/block row. Consecutive inline-level and floating boxes
                // can share a row; an in-flow block boundary flushes that row.
                // Summing every child made vertically stacked blocks twice as
                // wide and also joined anonymous text across block boundaries.
                let mut widest = 0.0_f32;
                let mut inline_row = 0.0_f32;
                for child in &component.children {
                    if matches!(child.style.position, WPos::Absolute | WPos::Fixed) {
                        continue;
                    }
                    let shares_inline_row = child.style.float != WFloat::None
                        || matches!(
                            child.style.display,
                            WDisplay::Inline
                                | WDisplay::InlineBlock
                                | WDisplay::InlineFlex
                                | WDisplay::InlineTable
                        );
                    if shares_inline_row {
                        inline_row += child_width(child);
                    } else {
                        widest = widest.max(inline_row).max(child_width(child));
                        inline_row = 0.0;
                    }
                }
                widest.max(inline_row)
            }
            _ => match component.style.flex_direction {
                WDir::Row | WDir::RowReverse => {
                    let children = component.children.iter().map(child_width).sum::<f32>();
                    children
                        + component.style.gap * component.children.len().saturating_sub(1) as f32
                }
                WDir::Column | WDir::ColumnReverse => component
                    .children
                    .iter()
                    .map(child_width)
                    .fold(0.0_f32, f32::max),
            },
        }
    };
    let specified_width = if component.style.display == WDisplay::Inline {
        None
    } else {
        match component.style.width {
            WDim::Px(width) => Some(width),
            WDim::Em(width) => Some(width * component.style.font_size),
            WDim::Rem(width) => Some(width * ROOT_FONT_SIZE),
            _ => None,
        }
    };
    let mut padding = component.style.padding_lengths();
    if component.style.border_collapse
        && matches!(
            component.style.display,
            WDisplay::Table | WDisplay::InlineTable
        )
    {
        padding.left = 0.0;
        padding.right = 0.0;
    }
    let border_width = if component.style.border_collapse
        && matches!(
            component.style.display,
            WDisplay::Table | WDisplay::InlineTable
        ) {
        0.0
    } else {
        component
            .style
            .border_left_width
            .unwrap_or(component.style.border_width)
            + component
                .style
                .border_right_width
                .unwrap_or(component.style.border_width)
    };
    let table_outer_spacing = if matches!(
        component.style.display,
        WDisplay::Table | WDisplay::InlineTable
    ) {
        effective_table_border_spacing(&component.style).0 * 2.0
    } else {
        0.0
    };
    let horizontal_inner_edges = padding.left + padding.right + border_width + table_outer_spacing;
    let mut border_box_width = match (specified_width, component.style.box_sizing) {
        (Some(width), WBoxSizing::BorderBox) => width,
        (Some(width), WBoxSizing::ContentBox) => width + horizontal_inner_edges,
        (None, _) => intrinsic_width + horizontal_inner_edges,
    };
    let intrinsic_constraint = |dimension| match dimension {
        WDim::Px(value) => Some(value),
        WDim::Em(value) => Some(value * component.style.font_size),
        WDim::Rem(value) => Some(value * ROOT_FONT_SIZE),
        WDim::Ch(value) => Some(
            value
                * layout_font()
                    .metrics('0', component.style.font_size)
                    .advance_width,
        ),
        WDim::Auto | WDim::Percent(_) | WDim::Vw(_) | WDim::Vh(_) => None,
    };
    let constraint_border_box = |dimension| {
        intrinsic_constraint(dimension).map(|width| {
            if component.style.box_sizing == WBoxSizing::ContentBox {
                width + horizontal_inner_edges
            } else {
                width
            }
        })
    };
    if component.style.display != WDisplay::Inline {
        if let Some(max_width) = constraint_border_box(component.style.max_width) {
            border_box_width = border_box_width.min(max_width);
        }
        if let Some(min_width) = constraint_border_box(component.style.min_width) {
            border_box_width = border_box_width.max(min_width);
        }
    }
    let margin = component.style.margin_lengths();
    let horizontal_margin = if component.style.display == WDisplay::TableCell {
        0.0
    } else {
        margin.left + margin.right
    };
    border_box_width + horizontal_margin
}

fn table_track_max_content_width(component: &Component) -> f32 {
    let caption_width = component
        .children
        .iter()
        .filter(|child| child.style.display == WDisplay::TableCaption)
        .map(component_max_content_width)
        .fold(0.0_f32, f32::max);
    if let Some(track) = collapsed_single_track_metrics(component) {
        return caption_width.max(track.outer_width());
    }
    let tracks = table_track_widths(component);
    if tracks.is_empty() {
        return caption_width.max(
            component
                .children
                .iter()
                .filter(|child| child.style.display != WDisplay::TableCaption)
                .map(component_max_content_width)
                .fold(0.0_f32, f32::max),
        );
    }
    let gap = effective_table_border_spacing(&component.style).0;
    let collapsed_outer = if component.style.border_collapse {
        collapsed_table_outer_inline_halves(component)
    } else {
        0.0
    };
    caption_width.max(
        tracks.iter().map(|width| width.max(0.0)).sum::<f32>()
            + gap * tracks.len().saturating_sub(1) as f32
            + collapsed_outer,
    )
}

#[derive(Clone, Copy, Debug)]
struct CollapsedSingleTrack {
    grid_width: f32,
    outer_left_half: f32,
    outer_right_half: f32,
}

impl CollapsedSingleTrack {
    fn outer_width(self) -> f32 {
        self.outer_left_half + self.grid_width + self.outer_right_half
    }

    fn cell_geometry(self, style: &w3cos_std::style::Style) -> (f32, f32) {
        let left = style.border_left_width.unwrap_or(style.border_width);
        let right = style.border_right_width.unwrap_or(style.border_width);
        let offset = self.outer_left_half - left / 2.0;
        let border_box_width = self.grid_width + left / 2.0 + right / 2.0;
        (offset, border_box_width)
    }
}

fn collapsed_single_track_metrics(component: &Component) -> Option<CollapsedSingleTrack> {
    if !component.style.border_collapse
        || !matches!(
            component.style.display,
            WDisplay::Table | WDisplay::InlineTable
        )
    {
        return None;
    }

    fn collect<'a>(component: &'a Component, cells: &mut Vec<&'a Component>) -> Option<()> {
        if component.style.display == WDisplay::TableRow {
            let mut row_cells = component
                .children
                .iter()
                .filter(|child| child.style.display == WDisplay::TableCell);
            let cell = row_cells.next()?;
            if row_cells.next().is_some() {
                return None;
            }
            cells.push(cell);
            return Some(());
        }
        for child in &component.children {
            if matches!(
                child.style.display,
                WDisplay::TableRow
                    | WDisplay::TableRowGroup
                    | WDisplay::TableHeaderGroup
                    | WDisplay::TableFooterGroup
            ) {
                collect(child, cells)?;
            }
        }
        Some(())
    }

    let mut cells = Vec::new();
    collect(component, &mut cells)?;
    if cells.is_empty() {
        return None;
    }

    let mut track = CollapsedSingleTrack {
        grid_width: 0.0,
        outer_left_half: 0.0,
        outer_right_half: 0.0,
    };
    for cell in cells {
        let left = cell
            .style
            .border_left_width
            .unwrap_or(cell.style.border_width);
        let right = cell
            .style
            .border_right_width
            .unwrap_or(cell.style.border_width);
        let border_box_width = component_max_content_width(cell);
        let content_and_padding = (border_box_width - left - right).max(0.0);
        track.grid_width = track
            .grid_width
            .max(content_and_padding + left / 2.0 + right / 2.0);
        track.outer_left_half = track.outer_left_half.max(left / 2.0);
        track.outer_right_half = track.outer_right_half.max(right / 2.0);
    }
    Some(track)
}

fn effective_table_border_spacing(style: &w3cos_std::style::Style) -> (f32, f32) {
    if style.border_collapse {
        // CSS 2.1 applies border-spacing only to the separated-border model.
        // Keep the computed value available to the DOM, but omit it from the
        // collapsed table grid's outer gutters and inter-row/column gaps.
        (0.0, 0.0)
    } else {
        (style.border_spacing_x, style.border_spacing_y)
    }
}

fn collapsed_layout_edge_width(style: &w3cos_std::style::Style, side: usize) -> f32 {
    match side {
        0 => style.border_top_width,
        1 => style.border_right_width,
        2 => style.border_bottom_width,
        _ => style.border_left_width,
    }
    .unwrap_or(style.border_width)
}

fn set_collapsed_layout_edge_width(style: &mut w3cos_std::style::Style, side: usize, width: f32) {
    match side {
        0 => style.border_top_width = Some(width),
        1 => style.border_right_width = Some(width),
        2 => style.border_bottom_width = Some(width),
        _ => style.border_left_width = Some(width),
    }
}

/// Resolve the widths of shared collapsed grid lines before handing the tree to
/// Taffy. The paint tree keeps the authored styles so conflict ownership and
/// colors are still resolved there; this layout-only clone gives both cells on
/// a boundary the winning width. Without it, a one-sided border is omitted
/// from one cell's border box and the negative overlap shortens every track.
fn resolve_collapsed_table_layout_borders(root: &mut Component) {
    fn collect_rows(component: &Component, rows: &mut Vec<Vec<[f32; 4]>>) {
        if component.style.display == WDisplay::TableRow {
            rows.push(
                component
                    .children
                    .iter()
                    .filter(|child| child.style.display == WDisplay::TableCell)
                    .map(|cell| {
                        [
                            collapsed_layout_edge_width(&cell.style, 0),
                            collapsed_layout_edge_width(&cell.style, 1),
                            collapsed_layout_edge_width(&cell.style, 2),
                            collapsed_layout_edge_width(&cell.style, 3),
                        ]
                    })
                    .collect(),
            );
            return;
        }
        for child in &component.children {
            if matches!(child.style.display, WDisplay::Table | WDisplay::InlineTable) {
                continue;
            }
            collect_rows(child, rows);
        }
    }

    fn apply_rows(component: &mut Component, rows: &mut impl Iterator<Item = Vec<[f32; 4]>>) {
        if component.style.display == WDisplay::TableRow {
            if let Some(edges) = rows.next() {
                for (cell, widths) in component
                    .children
                    .iter_mut()
                    .filter(|child| child.style.display == WDisplay::TableCell)
                    .zip(edges)
                {
                    for (side, width) in widths.into_iter().enumerate() {
                        set_collapsed_layout_edge_width(&mut cell.style, side, width);
                    }
                }
            }
            return;
        }
        for child in &mut component.children {
            if matches!(child.style.display, WDisplay::Table | WDisplay::InlineTable) {
                continue;
            }
            apply_rows(child, rows);
        }
    }

    if matches!(root.style.display, WDisplay::Table | WDisplay::InlineTable)
        && root.style.border_collapse
    {
        let mut rows = Vec::new();
        collect_rows(root, &mut rows);
        if !rows.is_empty() {
            for row in &mut rows {
                for column in 0..row.len().saturating_sub(1) {
                    let boundary = row[column][1].max(row[column + 1][3]);
                    row[column][1] = boundary;
                    row[column + 1][3] = boundary;
                }
            }
            for row_index in 0..rows.len().saturating_sub(1) {
                let columns = rows[row_index].len().min(rows[row_index + 1].len());
                for column in 0..columns {
                    let boundary = rows[row_index][column][2].max(rows[row_index + 1][column][0]);
                    rows[row_index][column][2] = boundary;
                    rows[row_index + 1][column][0] = boundary;
                }
            }

            let table_top = collapsed_layout_edge_width(&root.style, 0);
            let table_right = collapsed_layout_edge_width(&root.style, 1);
            let table_bottom = collapsed_layout_edge_width(&root.style, 2);
            let table_left = collapsed_layout_edge_width(&root.style, 3);
            let last_row = rows.len() - 1;
            for cell in &mut rows[0] {
                cell[0] = cell[0].max(table_top);
            }
            for cell in &mut rows[last_row] {
                cell[2] = cell[2].max(table_bottom);
            }
            for row in &mut rows {
                if let Some(cell) = row.first_mut() {
                    cell[3] = cell[3].max(table_left);
                }
                if let Some(cell) = row.last_mut() {
                    cell[1] = cell[1].max(table_right);
                }
            }

            apply_rows(root, &mut rows.into_iter());
        }
    }

    for child in &mut root.children {
        resolve_collapsed_table_layout_borders(child);
    }
}

fn table_track_widths(component: &Component) -> Vec<f32> {
    fn collect_columns(component: &Component, tracks: &mut Vec<f32>) {
        for child in &component.children {
            match child.style.display {
                WDisplay::TableColumn => tracks.push(
                    specified_border_box_width_with_basis(&child.style, None).unwrap_or(0.0),
                ),
                WDisplay::TableColumnGroup => {
                    if child
                        .children
                        .iter()
                        .any(|column| column.style.display == WDisplay::TableColumn)
                    {
                        collect_columns(child, tracks);
                    } else {
                        tracks.push(
                            specified_border_box_width_with_basis(&child.style, None)
                                .unwrap_or(0.0),
                        );
                    }
                }
                _ => {}
            }
        }
    }

    fn collect_rows(component: &Component, tracks: &mut Vec<f32>, collapsed: bool) {
        if component.style.display == WDisplay::TableRow {
            let mut column = 0;
            for cell in component
                .children
                .iter()
                .filter(|child| child.style.display == WDisplay::TableCell)
            {
                let span = table_cell_column_span(&cell.style);
                tracks.resize(tracks.len().max(column + span), 0.0);
                let mut width = component_max_content_width(cell);
                if collapsed {
                    width -=
                        (table_cell_edge_width(cell, 1) + table_cell_edge_width(cell, 3)) / 2.0;
                }
                let width_per_track = width.max(0.0) / span as f32;
                for track in &mut tracks[column..column + span] {
                    *track = track.max(width_per_track);
                }
                column += span;
            }
            return;
        }
        for child in &component.children {
            if matches!(
                child.style.display,
                WDisplay::TableRow
                    | WDisplay::TableRowGroup
                    | WDisplay::TableHeaderGroup
                    | WDisplay::TableFooterGroup
                    | WDisplay::TableColumnGroup
            ) {
                collect_rows(child, tracks, collapsed);
            }
        }
    }

    fn collect_collapsed_columns(component: &Component, columns: &mut Vec<bool>) {
        if component.style.display == WDisplay::TableColumn {
            columns.push(component.style.visibility == WVisibility::Collapse);
            return;
        }
        for child in &component.children {
            if child.style.display == WDisplay::TableColumnGroup {
                collect_collapsed_columns(child, columns);
            } else if child.style.display == WDisplay::TableColumn {
                columns.push(child.style.visibility == WVisibility::Collapse);
            }
        }
    }

    let mut tracks = Vec::new();
    collect_columns(component, &mut tracks);
    collect_rows(component, &mut tracks, component.style.border_collapse);
    let mut collapsed_columns = Vec::new();
    collect_collapsed_columns(component, &mut collapsed_columns);
    for (column, collapsed) in collapsed_columns.into_iter().enumerate() {
        if collapsed && let Some(track) = tracks.get_mut(column) {
            *track = -*track;
        }
    }
    tracks
}

fn table_cell_column_span(style: &w3cos_std::style::Style) -> usize {
    style
        .custom_properties
        .as_ref()
        .and_then(|properties| properties.get("--w3cos-internal-table-column-span"))
        .and_then(|span| span.parse::<usize>().ok())
        .filter(|span| *span > 0)
        .unwrap_or(1)
        .min(1000)
}

fn collapsed_table_outer_inline_halves(component: &Component) -> f32 {
    fn collect(component: &Component, left: &mut f32, right: &mut f32) {
        if component.style.display == WDisplay::TableRow {
            let cells = component
                .children
                .iter()
                .filter(|child| child.style.display == WDisplay::TableCell)
                .collect::<Vec<_>>();
            if let Some(first) = cells.first() {
                *left = left.max(table_cell_edge_width(first, 3) / 2.0);
            }
            if let Some(last) = cells.last() {
                *right = right.max(table_cell_edge_width(last, 1) / 2.0);
            }
            return;
        }
        for child in &component.children {
            if matches!(
                child.style.display,
                WDisplay::TableRow
                    | WDisplay::TableRowGroup
                    | WDisplay::TableHeaderGroup
                    | WDisplay::TableFooterGroup
                    | WDisplay::TableColumnGroup
            ) {
                collect(child, left, right);
            }
        }
    }
    let mut left = table_cell_edge_width(component, 3) / 2.0;
    let mut right = table_cell_edge_width(component, 1) / 2.0;
    collect(component, &mut left, &mut right);
    left + right
}

fn collapsed_table_specified_rows_min_height(component: &Component) -> Option<f32> {
    fn collect_rows<'a>(component: &'a Component, rows: &mut Vec<&'a Component>) {
        if component.style.display == WDisplay::TableRow {
            rows.push(component);
            return;
        }
        for child in &component.children {
            if matches!(
                child.style.display,
                WDisplay::TableRow
                    | WDisplay::TableRowGroup
                    | WDisplay::TableHeaderGroup
                    | WDisplay::TableFooterGroup
            ) {
                collect_rows(child, rows);
            }
        }
    }
    fn absolute_height(component: &Component) -> Option<f32> {
        match component.style.height {
            WDim::Px(height) => Some(height),
            WDim::Em(height) => Some(height * component.style.font_size),
            WDim::Rem(height) => Some(height * ROOT_FONT_SIZE),
            _ => None,
        }
    }

    let mut rows = Vec::new();
    collect_rows(component, &mut rows);
    let row_height = rows
        .iter()
        .try_fold(0.0, |height, row| Some(height + absolute_height(row)?))?;
    let first = rows.first()?;
    let last = rows.last()?;
    let top = first
        .children
        .iter()
        .filter(|cell| cell.style.display == WDisplay::TableCell)
        .map(|cell| table_cell_edge_width(cell, 0))
        .fold(table_cell_edge_width(component, 0), f32::max)
        / 2.0;
    let bottom = last
        .children
        .iter()
        .filter(|cell| cell.style.display == WDisplay::TableCell)
        .map(|cell| table_cell_edge_width(cell, 2))
        .fold(table_cell_edge_width(component, 2), f32::max)
        / 2.0;
    Some(row_height + top + bottom)
}

fn specified_border_box_width(style: &w3cos_std::style::Style) -> Option<f32> {
    specified_border_box_width_with_basis(style, None)
}

fn specified_border_box_width_with_basis(
    style: &w3cos_std::style::Style,
    percentage_basis: Option<f32>,
) -> Option<f32> {
    let width = constrained_specified_width_with_basis(style, percentage_basis).or_else(|| {
        resolve_width_dimension(style.min_width, style, percentage_basis)
    })?;
    if style.box_sizing == WBoxSizing::BorderBox {
        return Some(width.max(0.0));
    }
    let padding = style.padding_lengths();
    Some(
        (width
            + padding.left
            + padding.right
            + style.border_left_width.unwrap_or(style.border_width)
            + style.border_right_width.unwrap_or(style.border_width))
        .max(0.0),
    )
}

fn constrained_specified_width_with_basis(
    style: &w3cos_std::style::Style,
    percentage_basis: Option<f32>,
) -> Option<f32> {
    let mut width = resolve_width_dimension(style.width, style, percentage_basis)?.max(0.0);
    if let Some(max_width) =
        resolve_width_dimension(style.max_width, style, percentage_basis)
    {
        width = width.min(max_width);
    }
    if let Some(min_width) =
        resolve_width_dimension(style.min_width, style, percentage_basis)
    {
        width = width.max(min_width);
    }
    Some(width.max(0.0))
}

fn resolve_width_dimension(
    dimension: WDim,
    style: &w3cos_std::style::Style,
    percentage_basis: Option<f32>,
) -> Option<f32> {
    match dimension {
        WDim::Px(width) => Some(width),
        WDim::Em(width) => Some(width * style.font_size),
        WDim::Rem(width) => Some(width * ROOT_FONT_SIZE),
        WDim::Percent(width) => Some(percentage_basis? * width / 100.0),
        _ => None,
    }
}

fn fixed_table_track_widths(
    component: &Component,
    percentage_basis: Option<f32>,
) -> Option<Vec<f32>> {
    if !component.style.table_layout_fixed {
        return None;
    }
    // CSS table width participates in the table-width algorithm before track
    // distribution. Content-box borders stay outside that width; border-box
    // tables subtract them before deriving the inner grid.
    let table_width =
        constrained_specified_width_with_basis(&component.style, percentage_basis)?;

    fn collect_columns<'a>(
        component: &'a Component,
        columns: &mut Vec<Option<&'a w3cos_std::style::Style>>,
    ) {
        for child in &component.children {
            match child.style.display {
                WDisplay::TableColumn => {
                    columns.push(Some(&child.style));
                }
                WDisplay::TableColumnGroup => {
                    if child
                        .children
                        .iter()
                        .any(|column| column.style.display == WDisplay::TableColumn)
                    {
                        collect_columns(child, columns);
                    } else {
                        columns.push(Some(&child.style));
                    }
                }
                _ => {}
            }
        }
    }
    fn first_row(component: &Component) -> Option<&Component> {
        for child in &component.children {
            if child.style.display == WDisplay::TableRow {
                return Some(child);
            }
            if matches!(
                child.style.display,
                WDisplay::TableRowGroup | WDisplay::TableHeaderGroup | WDisplay::TableFooterGroup
            ) && let Some(row) = first_row(child)
            {
                return Some(row);
            }
        }
        None
    }

    let first_row = first_row(component);
    let row_cells = first_row
        .map(|row| {
            row.children
                .iter()
                .filter(|child| child.style.display == WDisplay::TableCell)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut column_styles = Vec::new();
    collect_columns(component, &mut column_styles);
    column_styles.resize(column_styles.len().max(row_cells.len()), None);
    if column_styles.is_empty() {
        return None;
    }

    let border_width = if component.style.box_sizing == WBoxSizing::BorderBox
        || is_html_table_element(&component.style)
    {
        component
            .style
            .border_left_width
            .unwrap_or(component.style.border_width)
            + component
                .style
                .border_right_width
                .unwrap_or(component.style.border_width)
    } else {
        0.0
    };
    let spacing = effective_table_border_spacing(&component.style).0;
    let grid_width =
        (table_width - border_width - spacing * (column_styles.len() + 1) as f32).max(0.0);
    let mut specified = column_styles
        .into_iter()
        .map(|style| {
            style.and_then(|style| specified_border_box_width_with_basis(style, Some(grid_width)))
        })
        .collect::<Vec<_>>();
    for (column, cell) in row_cells.into_iter().enumerate() {
        if specified[column].is_none() {
            specified[column] =
                specified_border_box_width_with_basis(&cell.style, Some(grid_width)).map(|width| {
                    if component.style.border_collapse
                        && cell.style.box_sizing == WBoxSizing::ContentBox
                    {
                        let left = cell
                            .style
                            .border_left_width
                            .unwrap_or(cell.style.border_width);
                        let right = cell
                            .style
                            .border_right_width
                            .unwrap_or(cell.style.border_width);
                        (width - (left + right) / 2.0).max(0.0)
                    } else {
                        width
                    }
                });
        }
    }
    let assigned = specified.iter().flatten().sum::<f32>();
    let automatic_count = specified.iter().filter(|width| width.is_none()).count();
    let automatic_width = if automatic_count == 0 {
        0.0
    } else {
        (grid_width - assigned).max(0.0) / automatic_count as f32
    };
    Some(
        specified
            .into_iter()
            .map(|width| width.unwrap_or(automatic_width))
            .collect(),
    )
}

fn auto_table_track_widths(component: &Component, percentage_basis: Option<f32>) -> Vec<f32> {
    let mut tracks = table_track_widths(component);
    let Some(table_width) =
        constrained_specified_width_with_basis(&component.style, percentage_basis).or_else(|| {
            resolve_width_dimension(
                component.style.min_width,
                &component.style,
                percentage_basis,
            )
        })
    else {
        return tracks;
    };
    let spacing = effective_table_border_spacing(&component.style).0;
    let grid_width = (table_width - spacing * (tracks.len() + 1) as f32).max(0.0);
    let intrinsic_width = tracks.iter().map(|width| width.max(0.0)).sum::<f32>();
    let visible_tracks = tracks
        .iter()
        .filter(|track| !track.is_sign_negative())
        .count();
    if grid_width > intrinsic_width && visible_tracks > 0 {
        let extra = (grid_width - intrinsic_width) / visible_tracks as f32;
        for track in &mut tracks {
            if !track.is_sign_negative() {
                *track += extra;
            }
        }
    }
    tracks
}

fn is_html_table_element(style: &w3cos_std::style::Style) -> bool {
    style
        .custom_properties
        .as_ref()
        .and_then(|properties| properties.get("--w3cos-internal-html-table-element"))
        .is_some_and(|value| value == "1")
}

fn table_caption_intrinsic_height(component: &Component) -> f32 {
    fn outer_height(component: &Component) -> f32 {
        let specified = match component.style.height {
            WDim::Px(height) => Some(height),
            WDim::Em(height) => Some(height * component.style.font_size),
            WDim::Rem(height) => Some(height * ROOT_FONT_SIZE),
            _ => None,
        };
        let content_height = specified.unwrap_or_else(|| {
            if component.children.is_empty() {
                return leaf_intrinsic_size(&component.kind, &component.style).1;
            }
            if matches!(component.style.flex_direction, WDir::Row | WDir::RowReverse) {
                component
                    .children
                    .iter()
                    .map(outer_height)
                    .fold(0.0_f32, f32::max)
            } else {
                component.children.iter().map(outer_height).sum()
            }
        });
        if specified.is_some() && component.style.box_sizing == WBoxSizing::BorderBox {
            return content_height.max(0.0);
        }
        let padding = component.style.padding_lengths();
        content_height
            + padding.top
            + padding.bottom
            + component
                .style
                .border_top_width
                .unwrap_or(component.style.border_width)
            + component
                .style
                .border_bottom_width
                .unwrap_or(component.style.border_width)
    }

    component
        .children
        .iter()
        .filter(|child| child.style.display == WDisplay::TableCaption)
        .map(outer_height)
        .sum()
}

fn table_cell_edge_width(component: &Component, edge: usize) -> f32 {
    match edge {
        0 => component
            .style
            .border_top_width
            .unwrap_or(component.style.border_width),
        1 => component
            .style
            .border_right_width
            .unwrap_or(component.style.border_width),
        2 => component
            .style
            .border_bottom_width
            .unwrap_or(component.style.border_width),
        _ => component
            .style
            .border_left_width
            .unwrap_or(component.style.border_width),
    }
}

fn collapsed_row_edge_width(row: &Component, edge: usize) -> f32 {
    row.children
        .iter()
        .filter(|child| child.style.display == WDisplay::TableCell)
        .map(|cell| table_cell_edge_width(cell, edge))
        .fold(table_cell_edge_width(row, edge), f32::max)
}

fn collapsed_table_part_block_edge_width(component: &Component, edge: usize) -> f32 {
    let own = table_cell_edge_width(component, edge);
    if component.style.display == WDisplay::TableRow {
        return own.max(collapsed_row_edge_width(component, edge));
    }
    let children = component.children.iter().filter(|child| {
        matches!(
            child.style.display,
            WDisplay::TableRow
                | WDisplay::TableRowGroup
                | WDisplay::TableHeaderGroup
                | WDisplay::TableFooterGroup
        )
    });
    let boundary = if edge == 0 {
        children.into_iter().next()
    } else {
        children.into_iter().last()
    };
    boundary
        .map(|child| own.max(collapsed_table_part_block_edge_width(child, edge)))
        .unwrap_or(own)
}

fn collapsed_empty_row_overlap(
    component: &Component,
    boundary_width: f32,
    viewport_w: f32,
    viewport_h: f32,
) -> Option<f32> {
    if component.style.display == WDisplay::TableRow
        && component.style.visibility == WVisibility::Collapse
    {
        return Some(0.0);
    }
    if component.style.display != WDisplay::TableRow
        || component
            .children
            .iter()
            .any(|child| child.style.display == WDisplay::TableCell)
    {
        return None;
    }
    let height = match component.style.height {
        WDim::Auto => 0.0,
        WDim::Px(value) => value,
        WDim::Percent(_) => return None,
        WDim::Rem(value) => value * ROOT_FONT_SIZE,
        WDim::Em(value) => value * component.style.font_size,
        WDim::Ch(value) => {
            value
                * layout_font()
                    .metrics('0', component.style.font_size)
                    .advance_width
        }
        WDim::Vw(value) => value * viewport_w / 100.0,
        WDim::Vh(value) => value * viewport_h / 100.0,
    };
    Some(height.max(0.0).min(boundary_width))
}

fn shrink_to_fit_used_width(component: &Component) -> f32 {
    let outer_width = component_max_content_width(component);
    if component.style.float != WFloat::None
        || matches!(
            component.style.display,
            WDisplay::Inline
                | WDisplay::InlineBlock
                | WDisplay::InlineFlex
                | WDisplay::InlineTable
                | WDisplay::Table
        )
    {
        // Taffy applies an inline-level box's margins separately. Its assigned
        // width is the border box, so do not make the painted background span
        // the margin. Table max-content aggregation still needs outer widths.
        let margin = component.style.margin_lengths();
        (outer_width - margin.left - margin.right).max(0.0)
    } else {
        outer_width
    }
}

fn component_min_content_width(component: &Component) -> f32 {
    if !matches!(component.style.width, WDim::Auto | WDim::Percent(_)) {
        return component_max_content_width(component);
    }
    let content_width = if component.children.is_empty() {
        match &component.kind {
            ComponentKind::Text { content }
                if matches!(
                    component.style.white_space,
                    WWhiteSpace::Normal | WWhiteSpace::PreLine
                ) =>
            {
                content
                    .split([' ', '\t', '\n', '\r'])
                    .filter(|word| !word.is_empty())
                    .map(|word| text_intrinsic_size(word, &component.style).0)
                    .fold(0.0_f32, f32::max)
            }
            _ => leaf_intrinsic_size(&component.kind, &component.style).0,
        }
    } else {
        component
            .children
            .iter()
            .filter(|child| !matches!(child.style.position, WPos::Absolute | WPos::Fixed))
            .map(component_min_content_width)
            .fold(0.0_f32, f32::max)
    };
    let padding = component.style.padding_lengths();
    let margin = component.style.margin_lengths();
    content_width
        + padding.left
        + padding.right
        + component
            .style
            .border_left_width
            .unwrap_or(component.style.border_width)
        + component
            .style
            .border_right_width
            .unwrap_or(component.style.border_width)
        + margin.left
        + margin.right
}

fn shrink_to_fit_used_width_with_available(component: &Component, available_width: f32) -> f32 {
    let preferred_width = shrink_to_fit_used_width(component);
    component_min_content_width(component)
        .max(available_width.max(0.0))
        .min(preferred_width)
}

fn dim_to_px(dim: WDim) -> Option<f32> {
    match dim {
        WDim::Px(v) => Some(v),
        WDim::Auto
        | WDim::Percent(_)
        | WDim::Rem(_)
        | WDim::Em(_)
        | WDim::Ch(_)
        | WDim::Vw(_)
        | WDim::Vh(_) => None,
    }
}

fn resolve_spacing_for_layout(
    spacing: WSpacing,
    percentage_basis: f32,
    font_size: f32,
    viewport_w: f32,
    viewport_h: f32,
) -> f32 {
    match spacing {
        WSpacing::Percent(value) => percentage_basis * value / 100.0,
        WSpacing::Rem(value) => value * ROOT_FONT_SIZE,
        WSpacing::Em(value) => value * font_size,
        WSpacing::Vw(value) => value * viewport_w / 100.0,
        WSpacing::Vh(value) => value * viewport_h / 100.0,
        WSpacing::Auto => 0.0,
        other => other.resolve(&w3cos_std::safe_area::current()),
    }
}

pub(crate) fn text_intrinsic_size(content: &str, style: &w3cos_std::style::Style) -> (f32, f32) {
    let registry = crate::font_face::FontRegistry::global();
    #[cfg(not(feature = "skia"))]
    let font_runs = registry.resolve_style_runs(style, content);
    #[cfg(not(feature = "skia"))]
    let has_registered_runs = font_runs.iter().any(|run| run.font.is_some());
    let key = text_measure_key(
        DEFAULT_TEXT_WRAP_WIDTH,
        style,
        registry.cascade_cache_key(style, content),
    );
    if let Some(measured) = TEXT_MEASURE_CACHE.with(|cache| {
        cache
            .borrow()
            .intrinsic
            .get(content)
            .and_then(|entries| entries.iter().find(|(cached, _)| *cached == key))
            .map(|(_, measured)| *measured)
    }) {
        return measured;
    }

    let measured = {
        #[cfg(feature = "skia")]
        {
            // Layout and paint must use the same shaping backend. Splitting
            // one browser inline run across several generated/span boxes
            // otherwise accumulates Fontdue-vs-Skia advance differences and
            // visibly removes whitespace by the end of the line.
            crate::render_skia::measure_skia_text_intrinsic_size(content, style)
        }
        #[cfg(not(feature = "skia"))]
        {
            if has_registered_runs {
                cascade_text_intrinsic_size(content, style, DEFAULT_TEXT_WRAP_WIDTH)
            } else {
                text_layout::text_intrinsic_size_font(
                    content,
                    style,
                    DEFAULT_TEXT_WRAP_WIDTH,
                    layout_font(),
                )
            }
        }
    };
    TEXT_MEASURE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.make_room();
        cache
            .intrinsic
            .entry(content.to_owned())
            .or_default()
            .push((key, measured));
        cache.entries += 1;
    });
    measured
}

fn text_intrinsic_size_for_taffy(content: &str, style: &w3cos_std::style::Style) -> (f32, f32) {
    let (width, height) = text_intrinsic_size(content, style);
    if style.box_sizing == WBoxSizing::BorderBox {
        return (width, height);
    }
    let padding = style.padding_lengths();
    (
        (width - padding.left - padding.right).max(0.0),
        (height - padding.top - padding.bottom).max(0.0),
    )
}

fn cascade_char_advance(character: char, style: &w3cos_std::style::Style) -> f32 {
    crate::font_face::FontRegistry::global()
        .resolve_style_for_character(style, character)
        .and_then(|font| font.parsed())
        .map_or_else(
            || text_layout::char_advance(character, style.font_size, layout_font()),
            |font| text_layout::char_advance(character, style.font_size, font.as_ref()),
        )
}

fn cascade_measure_width(text: &str, style: &w3cos_std::style::Style) -> f32 {
    text.chars()
        .map(|character| cascade_char_advance(character, style))
        .sum()
}

fn cascade_line_height(text: &str, style: &w3cos_std::style::Style) -> f32 {
    let registry = crate::font_face::FontRegistry::global();
    registry
        .resolve_style_runs(style, text)
        .into_iter()
        .map(|run| {
            let font = run
                .font
                .as_ref()
                .and_then(crate::font_face::LoadedFont::parsed);
            match font.as_deref() {
                Some(font) => text_layout::single_line_content_height(
                    &text[run.byte_range],
                    style.font_size,
                    style.line_height,
                    font,
                ),
                None => text_layout::single_line_content_height(
                    &text[run.byte_range],
                    style.font_size,
                    style.line_height,
                    layout_font(),
                ),
            }
        })
        .fold(style.font_size * style.line_height, f32::max)
}

fn cascade_text_intrinsic_size(
    content: &str,
    style: &w3cos_std::style::Style,
    wrap_width: f32,
) -> (f32, f32) {
    let padding = style.padding_lengths();
    if matches!(
        style.white_space,
        w3cos_std::style::WhiteSpace::NoWrap | w3cos_std::style::WhiteSpace::Pre
    ) {
        let mut width = cascade_measure_width(content, style) + padding.left + padding.right;
        if let w3cos_std::style::Dimension::Px(min_width) = style.min_width {
            width = width.max(min_width);
        }
        return (
            width,
            cascade_line_height(content, style) + padding.top + padding.bottom,
        );
    }
    let inner_width = (wrap_width - padding.left - padding.right).max(1.0);
    let lines = text_layout::wrap_text_with_char_width(
        content,
        inner_width,
        style.white_space,
        |character| cascade_char_advance(character, style),
    );
    let width = lines
        .iter()
        .map(|line| cascade_measure_width(line, style))
        .fold(0.0_f32, f32::max);
    let used_line_count = text_layout::used_text_line_count(content, style, &lines);
    let height = if used_line_count == 1 {
        cascade_line_height(&lines[0], style)
    } else {
        used_line_count as f32 * style.font_size * style.line_height
    };
    (
        width + padding.left + padding.right,
        height + padding.top + padding.bottom,
    )
}

fn text_intrinsic_size_in_parent(
    content: &str,
    style: &w3cos_std::style::Style,
    parent_display: Option<WDisplay>,
) -> (f32, f32) {
    let (width, mut height) = text_intrinsic_size(content, style);
    if let Some(browser_height) = browser_normal_cjk_height(content, style, parent_display) {
        height = height.max(browser_height);
    }
    (width, height)
}

fn text_intrinsic_size_in_parent_for_taffy(
    content: &str,
    style: &w3cos_std::style::Style,
    parent_display: Option<WDisplay>,
) -> (f32, f32) {
    let (width, height) = text_intrinsic_size_in_parent(content, style, parent_display);
    if style.box_sizing == WBoxSizing::BorderBox {
        return (width, height);
    }
    let padding = style.padding_lengths();
    (
        (width - padding.left - padding.right).max(0.0),
        (height - padding.top - padding.bottom).max(0.0),
    )
}

fn browser_normal_cjk_height(
    content: &str,
    style: &w3cos_std::style::Style,
    parent_display: Option<WDisplay>,
) -> Option<f32> {
    if (matches!(style.display, WDisplay::InlineBlock | WDisplay::InlineFlex)
        || matches!(
            parent_display,
            Some(WDisplay::InlineBlock | WDisplay::InlineFlex)
        ))
        && content.chars().any(is_cjk)
        && (style.line_height - w3cos_std::style::Style::default().line_height).abs() < f32::EPSILON
    {
        let padding = style.padding_lengths();
        let browser_normal_line_height = (style.font_size * 1.4 * 2.0).ceil() * 0.5;
        Some(browser_normal_line_height + padding.top + padding.bottom)
    } else {
        None
    }
}

fn is_cjk(ch: char) -> bool {
    matches!(
        ch,
        '\u{2E80}'..='\u{2FFF}'
            | '\u{3000}'..='\u{303F}'
            | '\u{31C0}'..='\u{31EF}'
            | '\u{3400}'..='\u{4DBF}'
            | '\u{4E00}'..='\u{9FFF}'
            | '\u{F900}'..='\u{FAFF}'
    )
}

fn wrapped_text_height(content: &str, width: f32, style: &w3cos_std::style::Style) -> f32 {
    let registry = crate::font_face::FontRegistry::global();
    #[cfg(not(feature = "skia"))]
    let font_runs = registry.resolve_style_runs(style, content);
    let key = text_measure_key(width, style, registry.cascade_cache_key(style, content));
    if let Some(measured) = TEXT_MEASURE_CACHE.with(|cache| {
        cache
            .borrow()
            .wrapped_height
            .get(content)
            .and_then(|entries| entries.iter().find(|(cached, _)| *cached == key))
            .map(|(_, measured)| *measured)
    }) {
        return measured;
    }

    let measured = {
        #[cfg(feature = "skia")]
        {
            crate::render_skia::measure_skia_wrapped_text_height(content, width, style)
        }
        #[cfg(not(feature = "skia"))]
        {
            if font_runs.iter().any(|run| run.font.is_some()) {
                cascade_text_intrinsic_size(content, style, width).1
            } else {
                text_layout::wrapped_block_height_font(content, width, style, layout_font())
            }
        }
    };
    TEXT_MEASURE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.make_room();
        cache
            .wrapped_height
            .entry(content.to_owned())
            .or_default()
            .push((key, measured));
        cache.entries += 1;
    });
    measured
}

fn text_measure_key(width: f32, style: &w3cos_std::style::Style, font: u64) -> TextMeasureKey {
    let padding = style.padding_lengths();
    TextMeasureKey {
        width: width.to_bits(),
        font,
        font_size: style.font_size.to_bits(),
        line_height: style.line_height.to_bits(),
        padding_top: padding.top.to_bits(),
        padding_right: padding.right.to_bits(),
        padding_bottom: padding.bottom.to_bits(),
        padding_left: padding.left.to_bits(),
        min_width: match style.min_width {
            WDim::Px(value) => Some(value.to_bits()),
            _ => None,
        },
        white_space: match style.white_space {
            WWhiteSpace::Normal => 0,
            WWhiteSpace::NoWrap => 1,
            WWhiteSpace::Pre => 2,
            WWhiteSpace::PreWrap => 3,
            WWhiteSpace::PreLine => 4,
        },
        word_break: match style.word_break {
            WWordBreak::Normal => 0,
            WWordBreak::BreakAll => 1,
            WWordBreak::BreakWord => 2,
            WWordBreak::KeepAll => 3,
        },
    }
}

fn button_intrinsic_size(label: &str, style: &w3cos_std::style::Style) -> (f32, f32) {
    let (mut w, mut h) = text_intrinsic_size(label, style);
    let pad = style.padding_lengths();
    let min_w = style.font_size * 2.0 + pad.left + pad.right;
    let min_h = style.font_size + pad.top + pad.bottom;
    w = w.max(min_w);
    h = h.max(min_h);
    (w, h)
}

/// Taffy leaf size: cross-axis `auto` so column `align-items: stretch` matches browser flex.
fn leaf_taffy_size(
    kind: &ComponentKind,
    style: &w3cos_std::style::Style,
    base: &taffy::Style,
    parent_display: Option<WDisplay>,
    containing_width: f32,
    viewport_w: f32,
    viewport_h: f32,
) -> taffy::Size<Dimension> {
    let replaced_containing_width = if matches!(kind, ComponentKind::Image { .. })
        && style.display == WDisplay::Block
        && matches!(style.width, WDim::Auto)
    {
        let padding = style.padding_lengths();
        let borders = style.border_left_width.unwrap_or(style.border_width)
            + style.border_right_width.unwrap_or(style.border_width);
        (containing_width - padding.left - padding.right - borders).max(0.0)
    } else {
        containing_width
    };
    // `display:inline` normally forces both axes to auto in `to_taffy_style`,
    // but replaced elements such as `<img>` still honor their CSS width and
    // height. Use the semantic dimensions here before applying leaf sizing.
    let replaced_width = (matches!(kind, ComponentKind::Image { .. })
        && !matches!(style.width, WDim::Auto))
    .then(|| to_taffy_dim(style.width, style.font_size, viewport_w, viewport_h));
    let replaced_height = (matches!(kind, ComponentKind::Image { .. })
        && !matches!(style.height, WDim::Auto))
    .then(|| to_taffy_dim(style.height, style.font_size, viewport_w, viewport_h));
    let width = if let Some(width) = replaced_width {
        width
    } else if matches!(style.width, WDim::Auto) {
        match kind {
            ComponentKind::TextInput { .. } => Dimension::length(
                leaf_intrinsic_size_with_containing(kind, style, Some(containing_width)).0,
            ),
            _ => Dimension::auto(),
        }
    } else {
        base.size.width
    };
    let height = if let Some(height) = replaced_height {
        height
    } else if matches!(style.height, WDim::Auto) {
        let h = match kind {
            ComponentKind::Text { content } => {
                text_intrinsic_size_in_parent_for_taffy(content, style, parent_display).1
            }
            ComponentKind::Button { label } => button_intrinsic_size(label, style).1,
            ComponentKind::Image { src }
                if !matches!(style.width, WDim::Auto) && image_intrinsic_ratio(src).is_some() =>
            {
                // Let Taffy resolve percentage/viewport-relative widths
                // before applying the replaced element's intrinsic ratio.
                return Size {
                    width,
                    height: Dimension::auto(),
                };
            }
            _ => {
                leaf_intrinsic_size_with_containing(kind, style, Some(replaced_containing_width)).1
            }
        };
        Dimension::length(h)
    } else {
        base.size.height
    };
    Size { width, height }
}

fn kinds_layout_eq(a: &ComponentKind, b: &ComponentKind) -> bool {
    match (a, b) {
        (ComponentKind::Root, ComponentKind::Root) => true,
        (ComponentKind::Column, ComponentKind::Column) => true,
        (ComponentKind::Row, ComponentKind::Row) => true,
        (ComponentKind::Box, ComponentKind::Box) => true,
        (ComponentKind::VirtualList { .. }, ComponentKind::VirtualList { .. }) => true,
        (ComponentKind::Text { .. }, ComponentKind::Text { .. }) => true,
        (ComponentKind::Button { label: la }, ComponentKind::Button { label: lb }) => la == lb,
        (ComponentKind::Image { src: sa }, ComponentKind::Image { src: sb }) => sa == sb,
        (
            ComponentKind::TextInput {
                value: va,
                placeholder: pa,
                secure: sa,
            },
            ComponentKind::TextInput {
                value: vb,
                placeholder: pb,
                secure: sb,
            },
        ) => va == vb && pa == pb && sa == sb,
        (
            ComponentKind::Canvas {
                width: wa,
                height: ha,
            },
            ComponentKind::Canvas {
                width: wb,
                height: hb,
            },
        ) => wa == wb && ha == hb,
        (
            ComponentKind::SvgDocument {
                width: wa,
                height: ha,
                ..
            },
            ComponentKind::SvgDocument {
                width: wb,
                height: hb,
                ..
            },
        ) => wa == wb && ha == hb,
        _ => false,
    }
}

/// Returns true when a reactive rebuild does not require reconstructing the Taffy tree.
pub fn layout_shape_unchanged(old: &[FlatNodeInfo<'_>], new: &[FlatNodeInfo<'_>]) -> bool {
    if old.len() != new.len() {
        return false;
    }
    for (o, n) in old.iter().zip(new.iter()) {
        if !kinds_layout_eq(o.kind, n.kind) {
            return false;
        }
        // Reactive Text size changes must not invalidate the Taffy tree (Blink-style stable slots).
        let compare_intrinsic = matches!(
            o.kind,
            ComponentKind::Button { .. } | ComponentKind::Image { .. }
        );
        if compare_intrinsic {
            let o_size = leaf_intrinsic_size(o.kind, o.style);
            let n_size = leaf_intrinsic_size(n.kind, n.style);
            if (o_size.0 - n_size.0).abs() > f32::EPSILON
                || (o_size.1 - n_size.1).abs() > f32::EPSILON
            {
                return false;
            }
        }
    }
    true
}

/// Returns true when reactive Show slots only toggled `display` (tree shape unchanged).
pub fn layout_display_unchanged(old: &[FlatNodeInfo<'_>], new: &[FlatNodeInfo<'_>]) -> bool {
    if old.len() != new.len() {
        return false;
    }
    old.iter()
        .zip(new.iter())
        .all(|(o, n)| o.style.display == n.style.display)
}

/// Returns true when styles are unchanged apart from `display`.
///
/// `display` has a dedicated incremental patch path. Other changes may affect
/// Taffy geometry (for example a virtualizer reusing a row slot with a new
/// absolute `top`) and therefore require rebuilding the persistent tree.
pub fn layout_styles_unchanged_except_display(
    old: &[FlatNodeInfo<'_>],
    new: &[FlatNodeInfo<'_>],
) -> bool {
    if old.len() != new.len() {
        return false;
    }
    old.iter()
        .zip(new.iter())
        .all(|(old, new)| old.style.eq_except_display(new.style))
}

/// Walk ancestors — false when any `display: none` (Show stable slots).
pub fn is_node_visible(flat: &[FlatNodeInfo<'_>], idx: usize) -> bool {
    let mut cur = Some(idx);
    while let Some(i) = cur {
        if i >= flat.len() {
            return false;
        }
        if matches!(flat[i].style.display, WDisplay::None) {
            return false;
        }
        cur = flat[i].parent;
    }
    true
}

fn pre_flatten_recursive<'a>(
    comp: &'a Component,
    parent: Option<usize>,
    stable_id: u64,
    out: &mut Vec<FlatNodeInfo<'a>>,
) {
    let my_idx = out.len();
    out.push(FlatNodeInfo {
        stable_id,
        kind: &comp.kind,
        style: &comp.style,
        on_click: &comp.on_click,
        sticky_counter_signal: comp.sticky_counter_signal,
        parent,
    });
    for (child_index, child) in comp.children.iter().enumerate() {
        // FNV-1a over the child ordinal gives each persistent tree slot an
        // identity independent from its current flattened array index.
        let mut child_id = stable_id;
        for byte in (child_index as u64).to_le_bytes() {
            child_id ^= byte as u64;
            child_id = child_id.wrapping_mul(0x0000_0100_0000_01b3);
        }
        pre_flatten_recursive(child, Some(my_idx), child_id, out);
    }
}

// ---------------------------------------------------------------------------
// LayoutEngine — persistent TaffyTree for incremental layout
// ---------------------------------------------------------------------------

pub struct LayoutEngine {
    tree: TaffyTree<usize>,
    root_node: Option<taffy::NodeId>,
    tree_valid: bool,
    viewport: Option<(f32, f32)>,
    /// Taffy `compute_layout` calls issued by the most recent `compute()`.
    /// 1 means text-leaf heights were already clean and the historic second
    /// full pass was skipped.
    pub last_compute_layout_passes: u8,
}

pub struct LayoutResults {
    pub layout_cache: Vec<(LayoutRect, usize)>,
    pub scrollable_nodes: Vec<(usize, LayoutRect, ScrollExtent)>,
    pub clip_only_nodes: Vec<(usize, LayoutRect)>,
    pub scroll_ancestor: Vec<Option<usize>>,
}

impl LayoutResults {
    pub fn empty() -> Self {
        Self {
            layout_cache: Vec::new(),
            scrollable_nodes: Vec::new(),
            clip_only_nodes: Vec::new(),
            scroll_ancestor: Vec::new(),
        }
    }
}

impl LayoutEngine {
    pub fn new() -> Self {
        let mut tree = TaffyTree::new();
        // Preserve CSS subpixel geometry. The renderer applies device-scale
        // rasterization later; rounding here would turn Chromium's 26.5px
        // inline box into 26px before a 3× mobile surface ever sees it.
        tree.disable_rounding();
        Self {
            tree,
            root_node: None,
            tree_valid: false,
            viewport: None,
            last_compute_layout_passes: 0,
        }
    }

    pub fn invalidate(&mut self) {
        self.tree_valid = false;
    }

    pub fn tree_valid(&self) -> bool {
        self.tree_valid
    }

    /// Patch `display` on existing Taffy nodes (Show route switch without tree rebuild).
    pub fn patch_display_styles(&mut self, flat: &[FlatNodeInfo<'_>]) -> Result<()> {
        let Some(root) = self.root_node else {
            return Ok(());
        };
        patch_taffy_display(&mut self.tree, root, flat)?;
        Ok(())
    }

    pub fn compute(
        &mut self,
        root: &Component,
        flat: &[FlatNodeInfo],
        viewport_w: f32,
        viewport_h: f32,
    ) -> Result<LayoutResults> {
        if self.viewport != Some((viewport_w, viewport_h)) {
            self.tree_valid = false;
            self.viewport = Some((viewport_w, viewport_h));
        }
        if !self.tree_valid {
            self.tree.clear();
            let mut idx = 0;
            let mut layout_root = root.clone();
            resolve_collapsed_table_layout_borders(&mut layout_root);
            self.root_node = Some(build_taffy_tree(
                &mut self.tree,
                &layout_root,
                &mut idx,
                None,
                None,
                None,
                None,
                viewport_w,
                viewport_h,
                viewport_w,
                Some(viewport_h),
                Some(viewport_h),
                None,
                false,
                None,
                None,
                false,
                None,
            )?);
            self.tree_valid = true;
        }

        let root_node = self.root_node.unwrap();
        let root_margins = root_used_margins(
            flat.first().map(|entry| entry.style),
            viewport_w,
            viewport_h,
        );
        let space = Size {
            width: AvailableSpace::Definite(viewport_w),
            height: AvailableSpace::Definite(viewport_h),
        };
        self.tree.compute_layout(root_node, space)?;
        self.last_compute_layout_passes = 1;
        if update_text_leaf_heights(&mut self.tree, root_node, flat, None)? {
            self.tree.compute_layout(root_node, space)?;
            self.last_compute_layout_passes = 2;
        }

        let mut results = Vec::new();
        let mut fixed_results = Vec::new();
        let mut scrollable = Vec::new();
        let mut clip_only = Vec::new();
        let mut scroll_ancestor = vec![None; flat.len()];
        let initial_containing_block = LayoutRect {
            x: 0.0,
            y: 0.0,
            width: viewport_w,
            height: viewport_h,
        };
        let root_x = root_auto_margin_offset(
            flat.first().map(|entry| entry.style),
            self.tree
                .layout(root_node)
                .map_or(viewport_w, |layout| layout.size.width),
            viewport_w,
            viewport_h,
        ) + root_margins.left;
        let (root_relative_x, root_relative_y) = root_relative_offset(
            flat.first().map(|entry| entry.style),
            viewport_w,
            viewport_h,
        );

        collect_layouts_fast(
            flat,
            &self.tree,
            root_node,
            root_x + root_relative_x,
            root_margins.top + root_relative_y,
            viewport_w,
            viewport_h,
            initial_containing_block,
            initial_containing_block,
            true,
            None,
            &mut results,
            &mut fixed_results,
            &mut scrollable,
            &mut clip_only,
            &mut scroll_ancestor,
        );

        project_rtl_fixed_block_alignment(&mut results, flat, viewport_w, viewport_h);
        project_empty_painted_inline_boxes(&mut results, flat);
        project_mixed_inline_block_definite_widths(&mut results, flat, viewport_w, viewport_h);
        project_fixed_table_cell_rects(&mut results, root, viewport_w, viewport_h);
        project_forced_break_lines(&mut results, root);
        align_inline_block_last_line_baselines(&mut results, root);
        project_collapsible_line_end_whitespace(&mut results, flat);
        project_leading_descendant_margin_groups(&mut results, root, viewport_w, viewport_h);
        project_inline_after_leading_empty_blocks(&mut results, root);
        project_constrained_height_trailing_margin_containment(
            &mut results,
            root,
            viewport_w,
            viewport_h,
        );
        project_simple_float_margin_boxes(&mut results, root, viewport_w, viewport_h);
        project_positioned_bfc_float_heights(&mut results, root, flat, viewport_w, viewport_h);
        project_table_column_background_rects(&mut results, flat);
        project_collapsed_table_row_rects(&mut results, flat);
        project_auto_table_child_heights(&mut results, flat);
        align_table_cell_baselines(&mut results, flat);
        project_table_cell_inline_vertical_padding(&mut results, flat);
        align_inline_table_first_row_baselines(&mut results, flat);
        align_empty_inline_table_baselines(&mut results, flat);

        extend_scroll_extents_from_descendants(&results, flat, &scroll_ancestor, &mut scrollable);

        results.extend(fixed_results);

        Ok(LayoutResults {
            layout_cache: results,
            scrollable_nodes: scrollable,
            clip_only_nodes: clip_only,
            scroll_ancestor,
        })
    }
}

// ---------------------------------------------------------------------------
// Public API (backward compatible — used by tests and simple callers)
// ---------------------------------------------------------------------------

pub fn compute(
    root: &Component,
    viewport_w: f32,
    viewport_h: f32,
) -> Result<Vec<(LayoutRect, usize)>> {
    let (results, _, _) = compute_with_scroll(root, viewport_w, viewport_h)?;
    Ok(results)
}

#[allow(clippy::type_complexity)]
pub fn compute_with_scroll(
    root: &Component,
    viewport_w: f32,
    viewport_h: f32,
) -> Result<(
    Vec<(LayoutRect, usize)>,
    Vec<(usize, LayoutRect, ScrollExtent)>,
    Vec<(usize, LayoutRect)>,
)> {
    let flat = pre_flatten(root);
    let mut layout_root = root.clone();
    resolve_collapsed_table_layout_borders(&mut layout_root);
    let mut tree: TaffyTree<usize> = TaffyTree::new();
    tree.disable_rounding();
    let mut node_index: usize = 0;

    let root_node = build_taffy_tree(
        &mut tree,
        &layout_root,
        &mut node_index,
        None,
        None,
        None,
        None,
        viewport_w,
        viewport_h,
        viewport_w,
        Some(viewport_h),
        Some(viewport_h),
        None,
        false,
        None,
        None,
        false,
        None,
    )?;
    let root_margins = root_used_margins(
        flat.first().map(|entry| entry.style),
        viewport_w,
        viewport_h,
    );
    let space = Size {
        width: AvailableSpace::Definite(viewport_w),
        height: AvailableSpace::Definite(viewport_h),
    };
    tree.compute_layout(root_node, space)?;
    if update_text_leaf_heights(&mut tree, root_node, &flat, None)? {
        tree.compute_layout(root_node, space)?;
    }

    let mut results = Vec::new();
    let mut fixed_results = Vec::new();
    let mut scrollable = Vec::new();
    let mut clip_only = Vec::new();
    let mut scroll_ancestor = vec![None; flat.len()];
    let initial_containing_block = LayoutRect {
        x: 0.0,
        y: 0.0,
        width: viewport_w,
        height: viewport_h,
    };
    let root_x = root_auto_margin_offset(
        flat.first().map(|entry| entry.style),
        tree.layout(root_node)
            .map_or(viewport_w, |layout| layout.size.width),
        viewport_w,
        viewport_h,
    ) + root_margins.left;
    let (root_relative_x, root_relative_y) = root_relative_offset(
        flat.first().map(|entry| entry.style),
        viewport_w,
        viewport_h,
    );

    collect_layouts_fast(
        &flat,
        &tree,
        root_node,
        root_x + root_relative_x,
        root_margins.top + root_relative_y,
        viewport_w,
        viewport_h,
        initial_containing_block,
        initial_containing_block,
        true,
        None,
        &mut results,
        &mut fixed_results,
        &mut scrollable,
        &mut clip_only,
        &mut scroll_ancestor,
    );

    project_rtl_fixed_block_alignment(&mut results, &flat, viewport_w, viewport_h);
    project_empty_painted_inline_boxes(&mut results, &flat);
    project_mixed_inline_block_definite_widths(&mut results, &flat, viewport_w, viewport_h);
    project_fixed_table_cell_rects(&mut results, &layout_root, viewport_w, viewport_h);
    project_forced_break_lines(&mut results, root);
    align_inline_block_last_line_baselines(&mut results, root);
    project_collapsible_line_end_whitespace(&mut results, &flat);
    project_leading_descendant_margin_groups(&mut results, root, viewport_w, viewport_h);
    project_inline_after_leading_empty_blocks(&mut results, root);
    project_constrained_height_trailing_margin_containment(
        &mut results,
        root,
        viewport_w,
        viewport_h,
    );
    project_simple_float_margin_boxes(&mut results, root, viewport_w, viewport_h);
    project_positioned_bfc_float_heights(&mut results, root, &flat, viewport_w, viewport_h);
    project_table_column_background_rects(&mut results, &flat);
    project_collapsed_table_row_rects(&mut results, &flat);
    project_auto_table_child_heights(&mut results, &flat);
    align_table_cell_baselines(&mut results, &flat);
    project_table_cell_inline_vertical_padding(&mut results, &flat);
    align_inline_table_first_row_baselines(&mut results, &flat);
    align_empty_inline_table_baselines(&mut results, &flat);

    extend_scroll_extents_from_descendants(&results, &flat, &scroll_ancestor, &mut scrollable);

    results.extend(fixed_results);
    Ok((results, scrollable, clip_only))
}

fn project_empty_painted_inline_boxes(
    layouts: &mut [(LayoutRect, usize)],
    flat: &[FlatNodeInfo<'_>],
) {
    let positions = layouts
        .iter()
        .enumerate()
        .map(|(position, (_, index))| (*index, position))
        .collect::<HashMap<_, _>>();
    for index in 0..flat.len() {
        let node = &flat[index];
        if node.style.display != WDisplay::Inline {
            continue;
        }
        let Some(position) = positions.get(&index).copied() else {
            continue;
        };
        if layouts[position].0.height.abs() > f32::EPSILON {
            continue;
        }
        let padding = node.style.padding_lengths();
        let paints_edge = node.style.border_width > 0.0
            || node
                .style
                .border_top_width
                .is_some_and(|width| width > 0.0)
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
                .is_some_and(|width| width > 0.0)
            || padding.top > 0.0
            || padding.right > 0.0
            || padding.bottom > 0.0
            || padding.left > 0.0;
        if !paints_edge {
            continue;
        }
        let Some(parent_position) = node.parent.and_then(|parent| positions.get(&parent).copied())
        else {
            continue;
        };
        let parent = layouts[parent_position].0;
        let painted_height = if parent.height > 0.0 {
            parent.height
        } else {
            node.style.font_size * node.style.line_height
        };
        if painted_height > 0.0 {
            layouts[position].0.y = parent.y;
            layouts[position].0.height = painted_height;
        }
    }
}

fn project_mixed_inline_block_definite_widths(
    layouts: &mut [(LayoutRect, usize)],
    flat: &[FlatNodeInfo<'_>],
    viewport_w: f32,
    viewport_h: f32,
) {
    // The flex fallback uses a full-width basis to force the block onto the
    // line after an anonymous inline run. Restore an authored block width for
    // painting after that line break has been resolved.
    let positions = layouts
        .iter()
        .enumerate()
        .map(|(position, (_, index))| (*index, position))
        .collect::<HashMap<_, _>>();
    for (index, node) in flat.iter().enumerate() {
        if matches!(node.style.position, WPos::Absolute | WPos::Fixed)
            || node.style.float != WFloat::None
            || !matches!(
                node.style.display,
                WDisplay::Block
                    | WDisplay::Flex
                    | WDisplay::Grid
                    | WDisplay::ListItem
                    | WDisplay::Table
            )
        {
            continue;
        }
        let Some(parent_index) = node.parent else {
            continue;
        };
        let parent = &flat[parent_index];
        if parent.style.display != WDisplay::Block
            || !matches!(parent.kind, ComponentKind::Row)
        {
            continue;
        }
        let Some(previous_index) = (0..index)
            .rev()
            .find(|candidate| flat[*candidate].parent == Some(parent_index))
        else {
            continue;
        };
        let previous = &flat[previous_index];
        let previous_has_children = flat
            .get(previous_index + 1)
            .is_some_and(|candidate| candidate.parent == Some(previous_index));
        let padding = previous.style.padding_lengths();
        let previous_paints_edge = previous.style.border_width > 0.0
            || previous
                .style
                .border_top_width
                .is_some_and(|width| width > 0.0)
            || previous
                .style
                .border_right_width
                .is_some_and(|width| width > 0.0)
            || previous
                .style
                .border_bottom_width
                .is_some_and(|width| width > 0.0)
            || previous
                .style
                .border_left_width
                .is_some_and(|width| width > 0.0)
            || padding.top > 0.0
            || padding.right > 0.0
            || padding.bottom > 0.0
            || padding.left > 0.0;
        if previous.style.display != WDisplay::Inline
            || previous_has_children
            || !matches!(previous.kind, ComponentKind::Row | ComponentKind::Box)
            || !previous_paints_edge
        {
            continue;
        }
        let (Some(position), Some(parent_position)) = (
            positions.get(&index).copied(),
            positions.get(&parent_index).copied(),
        ) else {
            continue;
        };
        let containing_width = component_content_width(
            parent.style,
            layouts[parent_position].0.width,
            viewport_w,
            viewport_h,
        );
        if let Some(width) =
            specified_border_box_width_with_basis(node.style, Some(containing_width))
        {
            layouts[position].0.width = width;
        }
    }
}

fn project_rtl_fixed_block_alignment(
    layouts: &mut [(LayoutRect, usize)],
    flat: &[FlatNodeInfo<'_>],
    viewport_w: f32,
    viewport_h: f32,
) {
    let positions = layouts
        .iter()
        .enumerate()
        .map(|(position, (_, index))| (*index, position))
        .collect::<HashMap<_, _>>();
    for (index, node) in flat.iter().enumerate() {
        if node.style.display != WDisplay::Block
            || (matches!(node.style.position, WPos::Absolute | WPos::Fixed)
                && (!matches!(node.style.left, WDim::Auto)
                    || !matches!(node.style.right, WDim::Auto)))
            || matches!(node.style.width, WDim::Auto)
            || matches!(node.style.margin.left, WSpacing::Auto)
            || matches!(node.style.margin.right, WSpacing::Auto)
        {
            continue;
        }
        let Some(parent_index) = node.parent else {
            continue;
        };
        let parent_node = &flat[parent_index];
        if parent_node.style.direction != w3cos_std::style::TextDirection::Rtl
            || parent_node.style.display != WDisplay::Block
        {
            continue;
        }
        let (Some(position), Some(parent_position)) = (
            positions.get(&index).copied(),
            positions.get(&parent_index).copied(),
        ) else {
            continue;
        };
        let parent = layouts[parent_position].0;
        let parent_padding = parent_node.style.padding_lengths();
        let parent_content_right = parent.x + parent.width
            - parent_padding.right
            - parent_node
                .style
                .border_right_width
                .unwrap_or(parent_node.style.border_width);
        let containing_width = component_content_width(
            parent_node.style,
            parent.width,
            viewport_w,
            viewport_h,
        );
        let margin_right = resolve_spacing_for_layout(
            node.style.margin.right,
            containing_width,
            node.style.font_size,
            viewport_w,
            viewport_h,
        );
        layouts[position].0.x = parent_content_right - margin_right - layouts[position].0.width;
    }
}

fn project_collapsible_line_end_whitespace(
    layouts: &mut [(LayoutRect, usize)],
    flat: &[FlatNodeInfo<'_>],
) {
    let positions = layouts
        .iter()
        .enumerate()
        .map(|(position, (_, index))| (*index, position))
        .collect::<HashMap<_, _>>();
    for (index, node) in flat.iter().enumerate() {
        let ComponentKind::Text { content } = node.kind else {
            continue;
        };
        if content.is_empty()
            || !content.chars().all(char::is_whitespace)
            || !matches!(node.style.white_space, WWhiteSpace::Normal | WWhiteSpace::PreLine)
        {
            continue;
        }
        let Some(parent_index) = node.parent else {
            continue;
        };
        let parent = &flat[parent_index];
        if parent.style.display != WDisplay::Flex || parent.style.flex_wrap == WWrap::NoWrap {
            continue;
        }
        let Some(position) = positions.get(&index).copied() else {
            continue;
        };
        let whitespace = layouts[position].0;
        let wraps_before_next = flat[index + 1..]
            .iter()
            .enumerate()
            .find(|(_, sibling)| sibling.parent == Some(parent_index))
            .and_then(|(offset, _)| positions.get(&(index + 1 + offset)).copied())
            .is_some_and(|next| layouts[next].0.y > whitespace.y + f32::EPSILON);
        if !wraps_before_next || whitespace.width <= f32::EPSILON {
            continue;
        }
        let shift = match parent.style.justify_content {
            WJustify::Center => whitespace.width / 2.0,
            WJustify::FlexEnd => whitespace.width,
            _ => 0.0,
        };
        layouts[position].0.width = 0.0;
        if shift <= f32::EPSILON {
            continue;
        }
        for sibling_index in 0..index {
            if flat[sibling_index].parent != Some(parent_index) {
                continue;
            }
            let Some(sibling_position) = positions.get(&sibling_index).copied() else {
                continue;
            };
            if (layouts[sibling_position].0.y - whitespace.y).abs() <= f32::EPSILON {
                layouts[sibling_position].0.x += shift;
            }
        }
    }
}

fn project_table_cell_inline_vertical_padding(
    layouts: &mut [(LayoutRect, usize)],
    flat: &[FlatNodeInfo<'_>],
) {
    fn descendant_of(flat: &[FlatNodeInfo<'_>], mut index: usize, ancestor: usize) -> bool {
        while let Some(parent) = flat[index].parent {
            if parent == ancestor {
                return true;
            }
            index = parent;
        }
        false
    }

    let positions = layouts
        .iter()
        .enumerate()
        .map(|(position, (_, index))| (*index, position))
        .collect::<HashMap<_, _>>();
    for (index, node) in flat.iter().enumerate() {
        let Some(parent_index) = node.parent else {
            continue;
        };
        let padding = node.style.padding_lengths();
        if node.style.display != WDisplay::Inline
            || flat[parent_index].style.display != WDisplay::TableCell
            || (padding.top.abs() <= f32::EPSILON && padding.bottom.abs() <= f32::EPSILON)
        {
            continue;
        }
        let (Some(position), Some(parent_position)) = (
            positions.get(&index).copied(),
            positions.get(&parent_index).copied(),
        ) else {
            continue;
        };
        let parent = layouts[parent_position].0;
        let parent_padding = flat[parent_index].style.padding_lengths();
        let target_content_top = parent.y
            + flat[parent_index]
                .style
                .border_top_width
                .unwrap_or(flat[parent_index].style.border_width)
            + parent_padding.top;
        let current_content_top = layouts[position].0.y
            + node
                .style
                .border_top_width
                .unwrap_or(node.style.border_width)
            + padding.top;
        let delta = target_content_top - current_content_top;
        if delta.abs() <= f32::EPSILON {
            continue;
        }
        for (rect, candidate) in layouts.iter_mut() {
            if *candidate == index || descendant_of(flat, *candidate, index) {
                rect.y += delta;
            }
        }
    }
}

fn align_table_cell_baselines(layouts: &mut [(LayoutRect, usize)], flat: &[FlatNodeInfo<'_>]) {
    fn descendant_of(flat: &[FlatNodeInfo<'_>], mut index: usize, ancestor: usize) -> bool {
        while let Some(parent) = flat[index].parent {
            if parent == ancestor {
                return true;
            }
            index = parent;
        }
        false
    }

    let positions = layouts
        .iter()
        .enumerate()
        .map(|(position, (_, index))| (*index, position))
        .collect::<HashMap<_, _>>();
    for (cell, node) in flat
        .iter()
        .enumerate()
        .filter(|(_, node)| node.style.display == WDisplay::TableCell)
    {
        let alignment = node.style.align_self;
        if !matches!(alignment, WAlignSelf::Center | WAlignSelf::FlexEnd) {
            continue;
        }
        if flat.iter().any(|child| {
            child.parent == Some(cell)
                && child.style.display != WDisplay::None
                && !matches!(child.style.position, WPos::Absolute | WPos::Fixed)
                && {
                    let margin = child.style.margin_lengths();
                    margin.top.abs() > f32::EPSILON || margin.bottom.abs() > f32::EPSILON
                }
        }) {
            continue;
        }
        let Some(cell_position) = positions.get(&cell).copied() else {
            continue;
        };
        let cell_rect = layouts[cell_position].0;
        let content_bounds = flat
            .iter()
            .enumerate()
            .filter(|(index, child)| {
                descendant_of(flat, *index, cell)
                    && !matches!(child.style.position, WPos::Fixed)
                    && child.style.display != WDisplay::None
            })
            .filter_map(|(index, _)| positions.get(&index).map(|position| layouts[*position].0))
            .fold(None::<(f32, f32)>, |bounds, rect| {
                Some(match bounds {
                    Some((top, bottom)) => (top.min(rect.y), bottom.max(rect.y + rect.height)),
                    None => (rect.y, rect.y + rect.height),
                })
            });
        let Some((content_top, content_bottom)) = content_bounds else {
            continue;
        };
        let padding = node.style.padding_lengths();
        let available_top = cell_rect.y
            + node
                .style
                .border_top_width
                .unwrap_or(node.style.border_width)
            + padding.top;
        let available_bottom = cell_rect.y + cell_rect.height
            - node
                .style
                .border_bottom_width
                .unwrap_or(node.style.border_width)
            - padding.bottom;
        let delta = match alignment {
            WAlignSelf::Center => {
                available_top + (available_bottom - available_top
                    - (content_bottom - content_top))
                    / 2.0
                    - content_top
            }
            WAlignSelf::FlexEnd => available_bottom - content_bottom,
            _ => 0.0,
        };
        if delta.abs() <= f32::EPSILON {
            continue;
        }
        for (rect, index) in layouts.iter_mut() {
            if descendant_of(flat, *index, cell)
                && !matches!(flat[*index].style.position, WPos::Absolute | WPos::Fixed)
            {
                rect.y += delta;
            }
        }
    }
    for (row, _) in flat
        .iter()
        .enumerate()
        .filter(|(_, node)| node.style.display == WDisplay::TableRow)
    {
        let cells = flat
            .iter()
            .enumerate()
            .filter(|(_, node)| {
                node.parent == Some(row)
                    && node.style.display == WDisplay::TableCell
                    && node.style.align_self == WAlignSelf::Baseline
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if cells.len() < 2 {
            continue;
        }
        let baselines = cells
            .iter()
            .filter_map(|cell| {
                let text = flat.iter().enumerate().find(|(index, node)| {
                    descendant_of(flat, *index, *cell)
                        && !matches!(node.style.position, WPos::Absolute | WPos::Fixed)
                        && matches!(node.kind, ComponentKind::Text { content } if content.chars().any(|character| !character.is_whitespace()))
                })?;
                let rect = layouts[*positions.get(&text.0)?].0;
                Some((*cell, rect.y))
            })
            .collect::<Vec<_>>();
        let Some(target) = baselines
            .iter()
            .map(|(_, baseline)| *baseline)
            .reduce(f32::max)
        else {
            continue;
        };
        for (cell, baseline) in baselines {
            let delta = target - baseline;
            if delta <= 0.0 {
                continue;
            }
            for (rect, index) in layouts.iter_mut() {
                if descendant_of(flat, *index, cell)
                    && !matches!(flat[*index].style.position, WPos::Absolute | WPos::Fixed)
                {
                    rect.y += delta;
                }
            }
        }
    }
}

fn align_empty_inline_table_baselines(
    layouts: &mut [(LayoutRect, usize)],
    flat: &[FlatNodeInfo<'_>],
) {
    fn descendant_of(flat: &[FlatNodeInfo<'_>], mut index: usize, ancestor: usize) -> bool {
        while let Some(parent) = flat[index].parent {
            if parent == ancestor {
                return true;
            }
            index = parent;
        }
        false
    }

    let positions = layouts
        .iter()
        .enumerate()
        .map(|(position, (_, index))| (*index, position))
        .collect::<HashMap<_, _>>();
    for table in 0..flat.len() {
        if flat[table].style.display != WDisplay::InlineTable
            || flat.iter().enumerate().any(|(index, node)| {
                descendant_of(flat, index, table)
                    && matches!(node.kind, ComponentKind::Text { content } if content.chars().any(|character| !character.is_whitespace()))
            })
        {
            continue;
        }
        let Some(parent) = flat[table].parent else {
            continue;
        };
        let Some(table_position) = positions.get(&table).copied() else {
            continue;
        };
        let table_rect = layouts[table_position].0;
        let target_baseline = (0..table)
            .filter(|index| flat[*index].parent == Some(parent))
            .filter(|index| {
                matches!(
                    flat[*index].style.display,
                    WDisplay::InlineBlock | WDisplay::InlineFlex | WDisplay::InlineTable
                )
            })
            .filter_map(|index| positions.get(&index).map(|position| layouts[*position].0))
            .map(|rect| rect.y + rect.height)
            .reduce(f32::max);
        let Some(target_baseline) = target_baseline else {
            continue;
        };
        let delta = target_baseline - (table_rect.y + table_rect.height);
        if delta == 0.0 {
            continue;
        }
        for (rect, index) in layouts.iter_mut() {
            if *index == table || descendant_of(flat, *index, table) {
                rect.y += delta;
            }
        }
        let child_bottom = layouts
            .iter()
            .filter(|(_, index)| flat[*index].parent == Some(parent))
            .map(|(rect, _)| rect.y + rect.height)
            .reduce(f32::max);
        if let (Some(parent_position), Some(child_bottom)) =
            (positions.get(&parent).copied(), child_bottom)
        {
            let parent_rect = &mut layouts[parent_position].0;
            parent_rect.height = (child_bottom - parent_rect.y).max(0.0);
        }
    }
}

fn align_inline_table_first_row_baselines(
    layouts: &mut [(LayoutRect, usize)],
    flat: &[FlatNodeInfo<'_>],
) {
    let positions = layouts
        .iter()
        .enumerate()
        .map(|(position, (_, index))| (*index, position))
        .collect::<HashMap<_, _>>();
    let mut line_top_by_parent = HashMap::<usize, f32>::new();
    for (table, node) in flat.iter().enumerate() {
        if node.style.display != WDisplay::InlineTable
            || !matches!(node.style.align_self, WAlignSelf::Auto | WAlignSelf::Baseline)
        {
            continue;
        }
        let Some(parent) = node.parent else {
            continue;
        };
        let descendant_of_table = |index: usize| {
            let mut ancestor = flat[index].parent;
            while let Some(candidate) = ancestor {
                if candidate == table {
                    return true;
                }
                ancestor = flat[candidate].parent;
            }
            false
        };
        let subtree_end = (table + 1..flat.len())
            .find(|index| !descendant_of_table(*index))
            .unwrap_or(flat.len());
        let has_vertical_table_geometry = flat[table..subtree_end].iter().any(|entry| {
            let padding = entry.style.padding_lengths();
            let margin = entry.style.margin_lengths();
            padding.top.abs() > f32::EPSILON
                || padding.bottom.abs() > f32::EPSILON
                || margin.top.abs() > f32::EPSILON
                || margin.bottom.abs() > f32::EPSILON
                || entry
                    .style
                    .border_top_width
                    .unwrap_or(entry.style.border_width)
                    .abs()
                    > f32::EPSILON
                || entry
                    .style
                    .border_bottom_width
                    .unwrap_or(entry.style.border_width)
                    .abs()
                    > f32::EPSILON
                || entry.style.border_spacing_y.abs() > f32::EPSILON
        });
        if !has_vertical_table_geometry {
            continue;
        }
        let first_visible_text = flat[table + 1..subtree_end]
            .iter()
            .enumerate()
            .find(|(_, descendant)| {
                descendant.style.visibility == WVisibility::Visible
                    && matches!(
                        descendant.kind,
                        ComponentKind::Text { content }
                            if content.chars().any(|character| !character.is_whitespace())
                    )
            })
            .map(|(offset, text)| (table + 1 + offset, text));
        let Some((text_index, text)) = first_visible_text else {
            continue;
        };
        let Some(text_position) = positions.get(&text_index).copied() else {
            continue;
        };
        let rect = layouts[text_position].0;
        let line_top = if text.style.display == WDisplay::Inline {
            rect.y
        } else {
            let padding = text.style.padding_lengths();
            let border_top = text
                .style
                .border_top_width
                .unwrap_or(text.style.border_width);
            let half_leading = ((text.style.font_size * text.style.line_height)
                - text.style.font_size)
                .max(0.0)
                * 0.5;
            rect.y + border_top + padding.top + half_leading
        };
        line_top_by_parent
            .entry(parent)
            .and_modify(|current| *current = current.max(line_top))
            .or_insert(line_top);
    }
    for (index, node) in flat.iter().enumerate() {
        if node.style.display != WDisplay::Inline
            || node.style.visibility != WVisibility::Visible
            || !matches!(node.kind, ComponentKind::Text { .. })
        {
            continue;
        }
        let Some(target) = node
            .parent
            .and_then(|parent| line_top_by_parent.get(&parent))
            .copied()
        else {
            continue;
        };
        if let Some(position) = positions.get(&index).copied() {
            layouts[position].0.y = target;
        }
    }
}

fn project_collapsed_table_row_rects(
    layouts: &mut [(LayoutRect, usize)],
    flat: &[FlatNodeInfo<'_>],
) {
    let rects = layouts
        .iter()
        .map(|(rect, index)| (*index, *rect))
        .collect::<HashMap<_, _>>();
    let is_descendant_of = |mut index: usize, ancestor: usize| {
        while let Some(parent) = flat.get(index).and_then(|node| node.parent) {
            if parent == ancestor {
                return true;
            }
            index = parent;
        }
        false
    };
    for container in 0..flat.len() {
        if !matches!(
            flat[container].style.display,
            WDisplay::Table
                | WDisplay::InlineTable
                | WDisplay::TableRowGroup
                | WDisplay::TableHeaderGroup
                | WDisplay::TableFooterGroup
        ) {
            continue;
        }
        let rows = flat
            .iter()
            .enumerate()
            .filter(|(index, node)| {
                node.style.display == WDisplay::TableRow && is_descendant_of(*index, container)
            })
            .collect::<Vec<_>>();
        if !rows
            .iter()
            .any(|(_, row)| row.style.visibility == WVisibility::Collapse)
        {
            continue;
        }
        let mut visible_bounds: Option<(f32, f32)> = None;
        for (index, row) in rows {
            if row.style.visibility == WVisibility::Collapse {
                continue;
            }
            let Some(rect) = rects.get(&index) else {
                continue;
            };
            visible_bounds = Some(match visible_bounds {
                Some((top, bottom)) => (top.min(rect.y), bottom.max(rect.y + rect.height)),
                None => (rect.y, rect.y + rect.height),
            });
        }
        let Some((top, bottom)) = visible_bounds else {
            continue;
        };
        if let Some((rect, _)) = layouts.iter_mut().find(|(_, index)| *index == container) {
            rect.y = top;
            rect.height = (bottom - top).max(0.0);
        }
    }
}

fn project_auto_table_child_heights(
    layouts: &mut [(LayoutRect, usize)],
    flat: &[FlatNodeInfo<'_>],
) {
    let positions = layouts
        .iter()
        .enumerate()
        .map(|(position, (_, index))| (*index, position))
        .collect::<HashMap<_, _>>();
    let mut child_bottoms = vec![None::<f32>; flat.len()];
    for (index, child) in flat.iter().enumerate() {
        let Some(parent) = child.parent else {
            continue;
        };
        if child.style.display == WDisplay::None
            || matches!(child.style.position, WPos::Absolute | WPos::Fixed)
        {
            continue;
        }
        let Some(position) = positions.get(&index).copied() else {
            continue;
        };
        let bottom = layouts[position].0.y + layouts[position].0.height;
        child_bottoms[parent] =
            Some(child_bottoms[parent].map_or(bottom, |value| value.max(bottom)));
    }
    for (table, node) in flat.iter().enumerate() {
        if !matches!(node.style.display, WDisplay::Table | WDisplay::InlineTable)
            || !matches!(node.style.height, WDim::Auto)
        {
            continue;
        }
        let Some(table_position) = positions.get(&table).copied() else {
            continue;
        };
        let table_y = layouts[table_position].0.y;
        let Some(child_bottom) = child_bottoms[table] else {
            continue;
        };
        let padding = node.style.padding_lengths();
        let bottom_edge = padding.bottom
            + node
                .style
                .border_bottom_width
                .unwrap_or(node.style.border_width);
        layouts[table_position].0.height = layouts[table_position]
            .0
            .height
            .max(child_bottom - table_y + bottom_edge);
    }
}

fn project_fixed_table_cell_rects(
    layouts: &mut [(LayoutRect, usize)],
    root: &Component,
    viewport_w: f32,
    viewport_h: f32,
) {
    let layout_position = layouts
        .iter()
        .enumerate()
        .map(|(position, (_, index))| (*index, position))
        .collect::<HashMap<_, _>>();

    fn shift_subtree_x(
        layouts: &mut [(LayoutRect, usize)],
        layout_position: &HashMap<usize, usize>,
        start: usize,
        count: usize,
        delta: f32,
    ) {
        for index in start..start + count {
            if let Some(position) = layout_position.get(&index) {
                layouts[*position].0.x += delta;
            }
        }
    }

    fn project_rows(
        component: &Component,
        component_index: usize,
        tracks: &[f32],
        gap: f32,
        collapsed: bool,
        rtl: bool,
        table_height: f32,
        row_growth: f32,
        viewport_w: f32,
        viewport_h: f32,
        layouts: &mut [(LayoutRect, usize)],
        layout_position: &HashMap<usize, usize>,
    ) {
        if component.style.display == WDisplay::TableRow {
            let Some(row_position) = layout_position.get(&component_index).copied() else {
                return;
            };
            let row_x = layouts[row_position].0.x;
            let specified_height = component.style.height.resolve(
                table_height,
                ROOT_FONT_SIZE,
                component.style.font_size,
                viewport_w,
                viewport_h,
            );
            let mut probe_index = component_index + 1;
            let cell_height = component
                .children
                .iter()
                .filter_map(|child| {
                    let index = probe_index;
                    probe_index += count_nodes(child);
                    (child.style.display == WDisplay::TableCell)
                        .then(|| {
                            layout_position
                                .get(&index)
                                .map(|position| layouts[*position].0.height)
                        })
                        .flatten()
                })
                .fold(0.0_f32, f32::max);
            let row_height = (layouts[row_position].0.height + row_growth)
                .max(cell_height)
                .max(specified_height.unwrap_or(0.0));
            layouts[row_position].0.height = row_height;
            let mut child_index = component_index + 1;
            let mut column = 0usize;
            let grid_width = tracks.iter().sum::<f32>()
                + gap * tracks.len().saturating_sub(1) as f32;
            let first_cell = component
                .children
                .iter()
                .find(|child| child.style.display == WDisplay::TableCell);
            let outer_half = if collapsed {
                first_cell.map_or(0.0, |cell| {
                    table_cell_edge_width(cell, if rtl { 1 } else { 3 }) / 2.0
                })
            } else {
                0.0
            };
            let mut target_x = if rtl {
                row_x + grid_width - outer_half
            } else {
                row_x + outer_half
            };
            for child in &component.children {
                let child_count = count_nodes(child);
                if child.style.display == WDisplay::TableCell {
                    if let Some(track) = tracks.get(column).copied()
                        && let Some(position) = layout_position.get(&child_index).copied()
                    {
                        if rtl {
                            target_x -= track;
                        }
                        let left_half = if collapsed {
                            table_cell_edge_width(child, 3) / 2.0
                        } else {
                            0.0
                        };
                        let right_half = if collapsed {
                            table_cell_edge_width(child, 1) / 2.0
                        } else {
                            0.0
                        };
                        let paint_x = target_x - left_half;
                        if collapsed {
                            // The border box extends by the shared half-border,
                            // while the content subtree moves with the grid
                            // track and stays after the full painted border.
                            let content_delta = target_x - layouts[position].0.x;
                            if child_count > 1 {
                                shift_subtree_x(
                                    layouts,
                                    layout_position,
                                    child_index + 1,
                                    child_count - 1,
                                    content_delta,
                                );
                            }
                            layouts[position].0.x = paint_x;
                        } else {
                            let delta = paint_x - layouts[position].0.x;
                            shift_subtree_x(
                                layouts,
                                layout_position,
                                child_index,
                                child_count,
                                delta,
                            );
                        }
                        layouts[position].0.width = track + left_half + right_half;
                        layouts[position].0.height = layouts[position].0.height.max(row_height);
                        if collapsed && !matches!(child.style.height, WDim::Auto) {
                            layouts[position].0.height += table_cell_edge_width(child, 0)
                                + table_cell_edge_width(child, 2);
                        }
                        if rtl {
                            target_x -= gap;
                        } else {
                            target_x += track + gap;
                        }
                    }
                    column += 1;
                }
                child_index += child_count;
            }
            layouts[row_position].0.width = grid_width;
            return;
        }

        let mut child_index = component_index + 1;
        for child in &component.children {
            if matches!(
                child.style.display,
                WDisplay::TableRow
                    | WDisplay::TableRowGroup
                    | WDisplay::TableHeaderGroup
                    | WDisplay::TableFooterGroup
            ) {
                project_rows(
                    child,
                    child_index,
                    tracks,
                    gap,
                    collapsed,
                    rtl,
                    table_height,
                    row_growth,
                    viewport_w,
                    viewport_h,
                    layouts,
                    layout_position,
                );
            }
            child_index += count_nodes(child);
        }
        if matches!(
            component.style.display,
            WDisplay::TableRowGroup | WDisplay::TableHeaderGroup | WDisplay::TableFooterGroup
        ) && let Some(position) = layout_position.get(&component_index).copied()
        {
            layouts[position].0.width =
                tracks.iter().sum::<f32>() + gap * tracks.len().saturating_sub(1) as f32;
        }
    }

    fn table_row_union(
        component: &Component,
        component_index: usize,
        layouts: &[(LayoutRect, usize)],
        layout_position: &HashMap<usize, usize>,
    ) -> Option<LayoutRect> {
        let mut result = if component.style.display == WDisplay::TableRow {
            layout_position
                .get(&component_index)
                .map(|position| layouts[*position].0)
        } else {
            None
        };
        let mut child_index = component_index + 1;
        for child in &component.children {
            if let Some(rect) = table_row_union(child, child_index, layouts, layout_position) {
                result = Some(result.map_or(rect, |current| {
                    let x = current.x.min(rect.x);
                    let y = current.y.min(rect.y);
                    let right = (current.x + current.width).max(rect.x + rect.width);
                    let bottom = (current.y + current.height).max(rect.y + rect.height);
                    LayoutRect {
                        x,
                        y,
                        width: right - x,
                        height: bottom - y,
                    }
                }));
            }
            child_index += count_nodes(child);
        }
        result
    }

    fn project_columns(
        component: &Component,
        component_index: usize,
        tracks: &[f32],
        gap: f32,
        grid_start_x: f32,
        grid_rect: Option<LayoutRect>,
        column: &mut usize,
        layouts: &mut [(LayoutRect, usize)],
        layout_position: &HashMap<usize, usize>,
    ) {
        let start_column = *column;
        let mut child_index = component_index + 1;
        for child in &component.children {
            match child.style.display {
                WDisplay::TableColumn => {
                    if let Some(track) = tracks.get(*column).copied()
                        && let Some(position) = layout_position.get(&child_index).copied()
                    {
                        layouts[position].0.x = grid_start_x
                            + tracks[..*column].iter().sum::<f32>()
                            + gap * *column as f32;
                        layouts[position].0.width = track;
                        if let Some(grid) = grid_rect {
                            layouts[position].0.y = grid.y;
                            layouts[position].0.height = grid.height;
                        }
                    }
                    *column += 1;
                }
                WDisplay::TableColumnGroup => project_columns(
                    child,
                    child_index,
                    tracks,
                    gap,
                    grid_start_x,
                    grid_rect,
                    column,
                    layouts,
                    layout_position,
                ),
                _ => {}
            }
            child_index += count_nodes(child);
        }
        if component.style.display == WDisplay::TableColumnGroup && *column == start_column {
            *column = (*column + table_cell_column_span(&component.style)).min(tracks.len());
        }
        if component.style.display == WDisplay::TableColumnGroup
            && *column > start_column
            && let Some(position) = layout_position.get(&component_index).copied()
        {
            layouts[position].0.x = grid_start_x
                + tracks[..start_column].iter().sum::<f32>()
                + gap * start_column as f32;
            layouts[position].0.width = tracks[start_column..*column].iter().sum::<f32>()
                + gap * (*column).saturating_sub(start_column + 1) as f32;
            if let Some(grid) = grid_rect {
                layouts[position].0.y = grid.y;
                layouts[position].0.height = grid.height;
            }
        }
    }

    fn visit(
        component: &Component,
        component_index: usize,
        containing_width: f32,
        viewport_w: f32,
        viewport_h: f32,
        layouts: &mut [(LayoutRect, usize)],
        layout_position: &HashMap<usize, usize>,
    ) {
        if matches!(
            component.style.display,
            WDisplay::Table | WDisplay::InlineTable
        ) && let Some(tracks) = fixed_table_track_widths(component, Some(containing_width))
            .or_else(|| {
                let has_declared_columns = component.children.iter().any(|child| {
                    matches!(
                        child.style.display,
                        WDisplay::TableColumn | WDisplay::TableColumnGroup
                    )
                });
                (component.style.table_layout_fixed || has_declared_columns)
                    .then(|| auto_table_track_widths(component, Some(containing_width)))
                    .filter(|tracks| !tracks.is_empty())
            })
        {
            let gap = effective_table_border_spacing(&component.style).0;
            let table_height = layout_position
                .get(&component_index)
                .map_or(0.0, |position| layouts[*position].0.height);
            let mut row_count = 0usize;
            let mut current_row_height = 0.0_f32;
            fn collect_row_heights(
                component: &Component,
                component_index: usize,
                layouts: &[(LayoutRect, usize)],
                layout_position: &HashMap<usize, usize>,
                row_count: &mut usize,
                current_row_height: &mut f32,
            ) {
                if component.style.display == WDisplay::TableRow {
                    if let Some(position) = layout_position.get(&component_index) {
                        *row_count += 1;
                        *current_row_height += layouts[*position].0.height;
                    }
                    return;
                }
                let mut child_index = component_index + 1;
                for child in &component.children {
                    collect_row_heights(
                        child,
                        child_index,
                        layouts,
                        layout_position,
                        row_count,
                        current_row_height,
                    );
                    child_index += count_nodes(child);
                }
            }
            collect_row_heights(
                component,
                component_index,
                layouts,
                layout_position,
                &mut row_count,
                &mut current_row_height,
            );
            let row_growth = if matches!(component.style.height, WDim::Auto)
                && !matches!(component.style.min_height, WDim::Auto)
                && row_count > 0
            {
                (table_height - current_row_height).max(0.0) / row_count as f32
            } else {
                0.0
            };
            project_rows(
                component,
                component_index,
                &tracks,
                gap,
                component.style.border_collapse,
                component.style.direction == w3cos_std::style::TextDirection::Rtl,
                table_height,
                row_growth,
                viewport_w,
                viewport_h,
                layouts,
                layout_position,
            );
            let grid_rect = table_row_union(component, component_index, layouts, layout_position);
            if let Some(position) = layout_position.get(&component_index).copied() {
                let border_left = component
                    .style
                    .border_left_width
                    .unwrap_or(component.style.border_width);
                let grid_start_x = layouts[position].0.x + border_left + gap;
                let mut column = 0;
                project_columns(
                    component,
                    component_index,
                    &tracks,
                    gap,
                    grid_start_x,
                    grid_rect,
                    &mut column,
                    layouts,
                    layout_position,
                );
            }
        }
        let child_containing_width = layout_position
            .get(&component_index)
            .copied()
            .map(|position| {
                let padding = component.style.padding_lengths();
                let horizontal_edges = padding.left
                    + padding.right
                    + component
                        .style
                        .border_left_width
                        .unwrap_or(component.style.border_width)
                    + component
                        .style
                        .border_right_width
                        .unwrap_or(component.style.border_width);
                (layouts[position].0.width - horizontal_edges).max(0.0)
            })
            .unwrap_or(containing_width);
        let mut child_index = component_index + 1;
        for child in &component.children {
            visit(
                child,
                child_index,
                child_containing_width,
                viewport_w,
                viewport_h,
                layouts,
                layout_position,
            );
            child_index += count_nodes(child);
        }
    }

    let root_width = layout_position
        .get(&0)
        .copied()
        .map_or(0.0, |position| layouts[position].0.width);
    visit(
        root,
        0,
        root_width,
        viewport_w,
        viewport_h,
        layouts,
        &layout_position,
    );
}

fn project_leading_descendant_margin_groups(
    layouts: &mut [(LayoutRect, usize)],
    root: &Component,
    viewport_w: f32,
    viewport_h: f32,
) {
    let layout_position = layouts
        .iter()
        .enumerate()
        .map(|(position, (_, index))| (*index, position))
        .collect::<HashMap<_, _>>();

    let collapse = |margins: &[f32]| {
        let positive = margins.iter().copied().fold(0.0_f32, f32::max);
        let negative = margins.iter().copied().fold(0.0_f32, f32::min);
        positive + negative
    };
    let resolve_margin = |spacing: WSpacing, style: &w3cos_std::style::Style, width: f32| {
        resolve_spacing_for_layout(spacing, width, style.font_size, viewport_w, viewport_h)
    };

    fn visit(
        component: &Component,
        component_index: usize,
        layouts: &mut [(LayoutRect, usize)],
        layout_position: &HashMap<usize, usize>,
        viewport_w: f32,
        viewport_h: f32,
        collapse: &impl Fn(&[f32]) -> f32,
        resolve_margin: &impl Fn(WSpacing, &w3cos_std::style::Style, f32) -> f32,
    ) {
        let Some(parent_position) = layout_position.get(&component_index).copied() else {
            return;
        };
        let containing_width = layouts[parent_position].0.width;
        let mut children = Vec::new();
        let mut child_index = component_index + 1;
        for child in &component.children {
            if !matches!(child.style.position, WPos::Absolute | WPos::Fixed)
                && child.style.display != WDisplay::None
                && child.style.float == WFloat::None
            {
                children.push((child_index, child));
            }
            child_index += count_nodes(child);
        }

        for pair in children.windows(2) {
            let (_, previous) = pair[0];
            let (current_index, current) = pair[1];
            if current.style.display != WDisplay::Block
                || current
                    .style
                    .border_top_width
                    .unwrap_or(current.style.border_width)
                    > 0.0
                || current.style.padding_lengths().top.abs() > f32::EPSILON
            {
                continue;
            }

            let Some(current_position) = layout_position.get(&current_index).copied() else {
                continue;
            };
            let current_width = layouts[current_position].0.width;
            let mut descendant_margins = Vec::new();
            let mut crossed_empty_block = false;
            let mut descendant_index = current_index + 1;
            for descendant in &current.children {
                let descendant_count = count_nodes(descendant);
                if matches!(descendant.style.position, WPos::Absolute | WPos::Fixed)
                    || descendant.style.display == WDisplay::None
                    || descendant.style.float != WFloat::None
                {
                    descendant_index += descendant_count;
                    continue;
                }
                descendant_margins.push(resolve_margin(
                    descendant.style.margin.top,
                    &descendant.style,
                    current_width,
                ));
                let empty_collapsible = layout_position
                    .get(&descendant_index)
                    .is_some_and(|position| layouts[*position].0.height.abs() <= f32::EPSILON)
                    && descendant.style.display == WDisplay::Block
                    && matches!(descendant.style.height, WDim::Auto | WDim::Px(0.0))
                    && descendant.style.padding_lengths().top.abs() <= f32::EPSILON
                    && descendant.style.padding_lengths().bottom.abs() <= f32::EPSILON
                    && descendant
                        .style
                        .border_top_width
                        .unwrap_or(descendant.style.border_width)
                        <= 0.0
                    && descendant
                        .style
                        .border_bottom_width
                        .unwrap_or(descendant.style.border_width)
                        <= 0.0;
                if empty_collapsible {
                    crossed_empty_block = true;
                    descendant_margins.push(resolve_margin(
                        descendant.style.margin.bottom,
                        &descendant.style,
                        current_width,
                    ));
                    descendant_index += descendant_count;
                    continue;
                }
                break;
            }
            if !crossed_empty_block || !descendant_margins.iter().any(|margin| *margin < 0.0) {
                continue;
            }

            let previous_bottom = resolve_margin(
                previous.style.margin.bottom,
                &previous.style,
                containing_width,
            );
            let current_top =
                resolve_margin(current.style.margin.top, &current.style, containing_width);
            let mut full_group = vec![previous_bottom, current_top];
            full_group.extend(descendant_margins);
            let delta = collapse(&full_group) - collapse(&[previous_bottom, current_top]);
            if delta >= -f32::EPSILON {
                continue;
            }
            let parent_end = component_index + count_nodes(component);
            for index in current_index..parent_end {
                if let Some(position) = layout_position.get(&index).copied() {
                    layouts[position].0.y += delta;
                }
            }
        }

        let mut child_index = component_index + 1;
        for child in &component.children {
            visit(
                child,
                child_index,
                layouts,
                layout_position,
                viewport_w,
                viewport_h,
                collapse,
                resolve_margin,
            );
            child_index += count_nodes(child);
        }
    }

    visit(
        root,
        0,
        layouts,
        &layout_position,
        viewport_w,
        viewport_h,
        &collapse,
        &resolve_margin,
    );
}

fn project_inline_after_leading_empty_blocks(
    layouts: &mut [(LayoutRect, usize)],
    root: &Component,
) {
    let layout_position = layouts
        .iter()
        .enumerate()
        .map(|(position, (_, index))| (*index, position))
        .collect::<HashMap<_, _>>();

    fn visit(
        component: &Component,
        component_index: usize,
        layouts: &mut [(LayoutRect, usize)],
        layout_position: &HashMap<usize, usize>,
    ) {
        let Some(parent_position) = layout_position.get(&component_index).copied() else {
            return;
        };
        let mut children = Vec::new();
        let mut child_index = component_index + 1;
        for child in &component.children {
            if !matches!(child.style.position, WPos::Absolute | WPos::Fixed)
                && child.style.display != WDisplay::None
                && child.style.float == WFloat::None
            {
                children.push((child_index, child));
            }
            child_index += count_nodes(child);
        }

        let leading_empty_count = children
            .iter()
            .take_while(|(index, child)| {
                layout_position
                    .get(index)
                    .is_some_and(|position| layouts[*position].0.height.abs() <= f32::EPSILON)
                    && child.style.display == WDisplay::Block
                    && matches!(child.style.height, WDim::Auto | WDim::Px(0.0))
                    && child.style.padding_lengths().top.abs() <= f32::EPSILON
                    && child.style.padding_lengths().bottom.abs() <= f32::EPSILON
                    && child
                        .style
                        .border_top_width
                        .unwrap_or(child.style.border_width)
                        <= 0.0
                    && child
                        .style
                        .border_bottom_width
                        .unwrap_or(child.style.border_width)
                        <= 0.0
            })
            .count();
        if leading_empty_count > 0
            && let Some((first_content_index, first_content)) = children.get(leading_empty_count)
            && matches!(
                first_content.style.display,
                WDisplay::Inline
                    | WDisplay::InlineBlock
                    | WDisplay::InlineFlex
                    | WDisplay::InlineTable
            )
            && component.style.display == WDisplay::Block
        {
            let border_top = component
                .style
                .border_top_width
                .unwrap_or(component.style.border_width);
            let padding_top = component.style.padding_lengths().top;
            if border_top <= 0.0
                && padding_top.abs() <= f32::EPSILON
                && let Some(content_position) = layout_position.get(first_content_index).copied()
            {
                let target_y = layouts[parent_position].0.y;
                let delta = target_y - layouts[content_position].0.y;
                if delta < -f32::EPSILON {
                    let component_end = component_index + count_nodes(component);
                    for index in *first_content_index..component_end {
                        if let Some(position) = layout_position.get(&index).copied() {
                            layouts[position].0.y += delta;
                        }
                    }
                }
            }
        }

        let mut child_index = component_index + 1;
        for child in &component.children {
            visit(child, child_index, layouts, layout_position);
            child_index += count_nodes(child);
        }
    }

    visit(root, 0, layouts, &layout_position);
}

fn project_constrained_height_trailing_margin_containment(
    layouts: &mut [(LayoutRect, usize)],
    root: &Component,
    viewport_w: f32,
    viewport_h: f32,
) {
    let layout_position = layouts
        .iter()
        .enumerate()
        .map(|(position, (_, index))| (*index, position))
        .collect::<HashMap<_, _>>();

    fn collapse(margins: impl IntoIterator<Item = f32>) -> f32 {
        let mut positive = 0.0_f32;
        let mut negative = 0.0_f32;
        for margin in margins {
            positive = positive.max(margin);
            negative = negative.min(margin);
        }
        positive + negative
    }

    fn shift_range(
        layouts: &mut [(LayoutRect, usize)],
        layout_position: &HashMap<usize, usize>,
        start: usize,
        end: usize,
        delta_y: f32,
    ) {
        for index in start..end {
            if let Some(position) = layout_position.get(&index).copied() {
                layouts[position].0.y += delta_y;
            }
        }
    }

    fn visit(
        component: &Component,
        component_index: usize,
        layouts: &mut [(LayoutRect, usize)],
        layout_position: &HashMap<usize, usize>,
        viewport_w: f32,
        viewport_h: f32,
    ) {
        let Some(component_position) = layout_position.get(&component_index).copied() else {
            return;
        };
        let containing_height = layouts[component_position].0.height;
        let component_end = component_index + count_nodes(component);
        let mut children = Vec::new();
        let mut child_index = component_index + 1;
        for child in &component.children {
            if !matches!(child.style.position, WPos::Absolute | WPos::Fixed)
                && child.style.display != WDisplay::None
                && child.style.float == WFloat::None
            {
                children.push((child_index, child));
            }
            child_index += count_nodes(child);
        }

        for pair in children.windows(2) {
            let (previous_index, previous) = pair[0];
            let (following_index, following) = pair[1];
            if previous.style.display != WDisplay::Block
                || !matches!(previous.style.height, WDim::Auto)
                || (matches!(previous.style.min_height, WDim::Auto)
                    && matches!(previous.style.max_height, WDim::Auto))
                || previous.style.padding_lengths().bottom.abs() > f32::EPSILON
                || previous
                    .style
                    .border_bottom_width
                    .unwrap_or(previous.style.border_width)
                    > 0.0
            {
                continue;
            }
            let previous_position = *layout_position
                .get(&previous_index)
                .expect("in-flow child layout");
            let previous_rect = layouts[previous_position].0;
            let previous_width = layouts[previous_position].0.width;
            let mut previous_children = Vec::new();
            let mut previous_child_index = previous_index + 1;
            for child in &previous.children {
                if !matches!(child.style.position, WPos::Absolute | WPos::Fixed)
                    && child.style.display != WDisplay::None
                    && child.style.float == WFloat::None
                {
                    previous_children.push((previous_child_index, child));
                }
                previous_child_index += count_nodes(child);
            }
            let content_overflows_bottom = previous_children.iter().any(|(child_index, _)| {
                layout_position.get(child_index).is_some_and(|position| {
                    let child = layouts[*position].0;
                    child.y + child.height > previous_rect.y + previous_rect.height + 0.01
                })
            });
            let mut trailing_margins = Vec::new();
            for &(child_index, child) in previous_children.iter().rev() {
                trailing_margins.push(resolve_spacing_for_layout(
                    child.style.margin.bottom,
                    previous_width,
                    child.style.font_size,
                    viewport_w,
                    viewport_h,
                ));
                let empty_collapsible = layout_position
                    .get(&child_index)
                    .is_some_and(|position| layouts[*position].0.height.abs() <= f32::EPSILON)
                    && child.style.display == WDisplay::Block
                    && matches!(child.style.height, WDim::Auto | WDim::Px(0.0))
                    && child.style.padding_lengths().top.abs() <= f32::EPSILON
                    && child.style.padding_lengths().bottom.abs() <= f32::EPSILON
                    && child
                        .style
                        .border_top_width
                        .unwrap_or(child.style.border_width)
                        <= 0.0
                    && child
                        .style
                        .border_bottom_width
                        .unwrap_or(child.style.border_width)
                        <= 0.0;
                if !empty_collapsible {
                    break;
                }
                trailing_margins.push(resolve_spacing_for_layout(
                    child.style.margin.top,
                    previous_width,
                    child.style.font_size,
                    viewport_w,
                    viewport_h,
                ));
            }
            let trailing_margin = collapse(trailing_margins);
            if trailing_margin <= 0.01 {
                continue;
            }
            let constrained_by_min = previous
                .style
                .min_height
                .resolve(
                    containing_height,
                    ROOT_FONT_SIZE,
                    previous.style.font_size,
                    viewport_w,
                    viewport_h,
                )
                .is_some_and(|min_height| previous_rect.height <= min_height + 0.01);
            let constrained_by_max = content_overflows_bottom
                && previous
                    .style
                    .max_height
                    .resolve(
                        containing_height,
                        ROOT_FONT_SIZE,
                        previous.style.font_size,
                        viewport_w,
                        viewport_h,
                    )
                    .is_some_and(|max_height| previous_rect.height <= max_height + 0.01);
            if !constrained_by_min && !constrained_by_max {
                continue;
            }
            let Some(following_position) = layout_position.get(&following_index).copied() else {
                continue;
            };
            let previous_margin = previous.style.margin_lengths();
            let following_margin = following.style.margin_lengths();
            let expected_y = previous_rect.y
                + previous_rect.height
                + collapse([previous_margin.bottom, following_margin.top]);
            let excess = layouts[following_position].0.y - expected_y;
            if excess > 0.01 {
                let correction = excess.min(trailing_margin);
                shift_range(
                    layouts,
                    layout_position,
                    following_index,
                    component_end,
                    -correction,
                );
                if matches!(component.style.height, WDim::Auto) {
                    layouts[component_position].0.height =
                        (layouts[component_position].0.height - correction).max(0.0);
                }
            }
        }

        let mut child_index = component_index + 1;
        for child in &component.children {
            visit(
                child,
                child_index,
                layouts,
                layout_position,
                viewport_w,
                viewport_h,
            );
            child_index += count_nodes(child);
        }
    }

    visit(root, 0, layouts, &layout_position, viewport_w, viewport_h);
}

fn project_simple_float_margin_boxes(
    layouts: &mut [(LayoutRect, usize)],
    root: &Component,
    viewport_w: f32,
    viewport_h: f32,
) {
    let layout_position = layouts
        .iter()
        .enumerate()
        .map(|(position, (_, index))| (*index, position))
        .collect::<HashMap<_, _>>();

    fn shift_subtree(
        layouts: &mut [(LayoutRect, usize)],
        layout_position: &HashMap<usize, usize>,
        start: usize,
        count: usize,
        delta_y: f32,
    ) {
        for index in start..start + count {
            if let Some(position) = layout_position.get(&index).copied() {
                layouts[position].0.y += delta_y;
            }
        }
    }

    fn shift_subtree_x(
        layouts: &mut [(LayoutRect, usize)],
        layout_position: &HashMap<usize, usize>,
        start: usize,
        count: usize,
        delta_x: f32,
    ) {
        for index in start..start + count {
            if let Some(position) = layout_position.get(&index).copied() {
                layouts[position].0.x += delta_x;
            }
        }
    }

    fn visit(
        component: &Component,
        component_index: usize,
        layouts: &mut [(LayoutRect, usize)],
        layout_position: &HashMap<usize, usize>,
        viewport_w: f32,
        viewport_h: f32,
    ) {
        let mut child_index = component_index + 1;
        for child in &component.children {
            visit(
                child,
                child_index,
                layouts,
                layout_position,
                viewport_w,
                viewport_h,
            );
            child_index += count_nodes(child);
        }

        if component.style.float != WFloat::None
            && matches!(component.style.height, WDim::Auto)
            && component.children.len() == 1
        {
            let child = &component.children[0];
            let child_index = component_index + 1;
            if child.style.display == WDisplay::Block
                && matches!(child.style.height, WDim::Auto | WDim::Px(0.0))
                && child.style.padding_lengths().top.abs() <= f32::EPSILON
                && child.style.padding_lengths().bottom.abs() <= f32::EPSILON
                && child
                    .style
                    .border_top_width
                    .unwrap_or(child.style.border_width)
                    <= 0.0
                && child
                    .style
                    .border_bottom_width
                    .unwrap_or(child.style.border_width)
                    <= 0.0
                && let (Some(parent_position), Some(child_position)) = (
                    layout_position.get(&component_index).copied(),
                    layout_position.get(&child_index).copied(),
                )
                && layouts[child_position].0.height.abs() <= f32::EPSILON
            {
                fn collect_collapsible_margins(
                    component: &Component,
                    component_index: usize,
                    containing_width: f32,
                    layouts: &[(LayoutRect, usize)],
                    layout_position: &HashMap<usize, usize>,
                    viewport_w: f32,
                    viewport_h: f32,
                    margins: &mut Vec<f32>,
                ) -> bool {
                    let Some(position) = layout_position.get(&component_index).copied() else {
                        return false;
                    };
                    if component.style.display != WDisplay::Block
                        || !matches!(component.style.height, WDim::Auto | WDim::Px(0.0))
                        || layouts[position].0.height.abs() > f32::EPSILON
                        || resolve_spacing_for_layout(
                            component.style.padding.top,
                            containing_width,
                            component.style.font_size,
                            viewport_w,
                            viewport_h,
                        )
                        .abs()
                            > f32::EPSILON
                        || resolve_spacing_for_layout(
                            component.style.padding.bottom,
                            containing_width,
                            component.style.font_size,
                            viewport_w,
                            viewport_h,
                        )
                        .abs()
                            > f32::EPSILON
                        || component
                            .style
                            .border_top_width
                            .unwrap_or(component.style.border_width)
                            > 0.0
                        || component
                            .style
                            .border_bottom_width
                            .unwrap_or(component.style.border_width)
                            > 0.0
                    {
                        return false;
                    }
                    let own = component.style.margin_lengths();
                    margins.extend([own.top, own.bottom]);
                    let child_containing_width = layouts[position].0.width;
                    let mut child_index = component_index + 1;
                    for child in &component.children {
                        let child_count = count_nodes(child);
                        if !matches!(child.style.position, WPos::Absolute | WPos::Fixed)
                            && child.style.display != WDisplay::None
                            && child.style.float == WFloat::None
                            && !collect_collapsible_margins(
                                child,
                                child_index,
                                child_containing_width,
                                layouts,
                                layout_position,
                                viewport_w,
                                viewport_h,
                                margins,
                            )
                        {
                            return false;
                        }
                        child_index += child_count;
                    }
                    true
                }

                let mut margins = Vec::new();
                if collect_collapsible_margins(
                    child,
                    child_index,
                    layouts[parent_position].0.width,
                    layouts,
                    layout_position,
                    viewport_w,
                    viewport_h,
                    &mut margins,
                ) {
                    let collapsed_margin = collapse(margins);
                    layouts[parent_position].0.height = collapsed_margin.max(0.0);
                }
            }
        }

        fn collapse(margins: impl IntoIterator<Item = f32>) -> f32 {
            let mut positive = 0.0_f32;
            let mut negative = 0.0_f32;
            for margin in margins {
                positive = positive.max(margin);
                negative = negative.min(margin);
            }
            positive + negative
        }

        fn leading_margin_group(component: &Component) -> f32 {
            let mut margins = vec![component.style.margin_lengths().top];
            let mut current = component;
            loop {
                if current.style.display != WDisplay::Block
                    || current.style.padding_lengths().top.abs() > f32::EPSILON
                    || current
                        .style
                        .border_top_width
                        .unwrap_or(current.style.border_width)
                        > 0.0
                {
                    break;
                }
                let Some(child) = current.children.iter().find(|child| {
                    !matches!(child.style.position, WPos::Absolute | WPos::Fixed)
                        && child.style.display != WDisplay::None
                        && child.style.float == WFloat::None
                }) else {
                    break;
                };
                if !matches!(
                    child.style.display,
                    WDisplay::Block | WDisplay::ListItem | WDisplay::Table
                ) {
                    break;
                }
                margins.push(child.style.margin_lengths().top);
                if child.style.display == WDisplay::Table {
                    break;
                }
                current = child;
            }
            collapse(margins)
        }

        let mut active_floats = Vec::<(WFloat, LayoutRect)>::new();
        let mut previous = None::<(usize, &Component)>;
        let mut previous_in_flow = None::<(usize, &Component)>;
        let mut float_since_in_flow = false;
        let mut normal_flow_correction = 0.0_f32;
        let mut child_index = component_index + 1;
        for child in &component.children {
            let child_count = count_nodes(child);
            if matches!(child.style.position, WPos::Absolute | WPos::Fixed)
                || child.style.display == WDisplay::None
            {
                child_index += child_count;
                continue;
            }
            if child.style.float == WFloat::None
                && child.style.clear == WClear::None
                && float_since_in_flow
                && let Some((previous_index, previous_child)) = previous_in_flow
                && let (Some(previous_position), Some(child_position)) = (
                    layout_position.get(&previous_index).copied(),
                    layout_position.get(&child_index).copied(),
                )
            {
                let previous_margin = previous_child.style.margin_lengths();
                let child_margin = child.style.margin_lengths();
                let target_y = layouts[previous_position].0.y
                    + layouts[previous_position].0.height
                    + collapse([previous_margin.bottom, child_margin.top]);
                let delta_y = target_y - layouts[child_position].0.y;
                if delta_y < -f32::EPSILON {
                    shift_subtree(layouts, layout_position, child_index, child_count, delta_y);
                    normal_flow_correction = normal_flow_correction.min(delta_y);
                }
                float_since_in_flow = false;
            }
            if child.style.float != WFloat::None
                && let Some((previous_index, previous_child)) = previous
                && previous_child.style.float == WFloat::None
                && let (Some(previous_position), Some(child_position)) = (
                    layout_position.get(&previous_index).copied(),
                    layout_position.get(&child_index).copied(),
                )
            {
                let previous_margin = previous_child.style.margin_lengths();
                let child_margin = child.style.margin_lengths();
                let target_y = layouts[previous_position].0.y
                    + layouts[previous_position].0.height
                    + collapse([previous_margin.bottom, child_margin.top]);
                let delta_y = target_y - layouts[child_position].0.y;
                if delta_y < -f32::EPSILON {
                    shift_subtree(layouts, layout_position, child_index, child_count, delta_y);
                }
            }
            if child.style.float == WFloat::None
                && child.style.clear == WClear::None
                && previous_in_flow.is_none()
                && !active_floats.is_empty()
                && let Some(position) = layout_position.get(&child_index).copied()
            {
                let current = layouts[position].0;
                let float_top = active_floats
                    .iter()
                    .map(|(_, rect)| rect.y)
                    .reduce(f32::min)
                    .unwrap_or(current.y);
                let float_bottom = active_floats
                    .iter()
                    .map(|(_, rect)| rect.y + rect.height)
                    .reduce(f32::max)
                    .unwrap_or(current.y);
                if current.y + f32::EPSILON >= float_bottom {
                    let delta_y = float_top - current.y;
                    shift_subtree(
                        layouts,
                        layout_position,
                        child_index,
                        child_count,
                        delta_y,
                    );
                    normal_flow_correction = normal_flow_correction.min(delta_y);
                    if child.style.resolved_overflow_x() != WOverflow::Visible
                        || child.style.resolved_overflow_y() != WOverflow::Visible
                    {
                        let left_float_edge = active_floats
                            .iter()
                            .filter(|(side, _)| *side == WFloat::Left)
                            .map(|(_, rect)| rect.x + rect.width)
                            .reduce(f32::max)
                            .unwrap_or(current.x);
                        if left_float_edge > current.x {
                            shift_subtree_x(
                                layouts,
                                layout_position,
                                child_index,
                                child_count,
                                left_float_edge - current.x,
                            );
                        }
                    }
                }
            }
            if child.style.float == WFloat::None
                && child.style.clear == WClear::None
                && matches!(
                    child.style.display,
                    WDisplay::Inline
                        | WDisplay::InlineBlock
                        | WDisplay::InlineFlex
                        | WDisplay::InlineTable
                )
                && !active_floats.is_empty()
                && let (Some(parent_position), Some(child_position)) = (
                    layout_position.get(&component_index).copied(),
                    layout_position.get(&child_index).copied(),
                )
            {
                let containing = layouts[parent_position].0;
                let mut current = layouts[child_position].0;
                loop {
                    let overlapping = active_floats
                        .iter()
                        .filter(|(_, float)| {
                            current.y < float.y + float.height - f32::EPSILON
                                && current.y + current.height > float.y + f32::EPSILON
                        })
                        .collect::<Vec<_>>();
                    if overlapping.is_empty() {
                        break;
                    }
                    let left_edge = overlapping
                        .iter()
                        .filter(|(side, _)| *side == WFloat::Left)
                        .map(|(_, float)| float.x + float.width)
                        .fold(containing.x, f32::max);
                    let right_edge = overlapping
                        .iter()
                        .filter(|(side, _)| *side == WFloat::Right)
                        .map(|(_, float)| float.x)
                        .fold(containing.x + containing.width, f32::min);
                    let margin = child.style.margin_lengths();
                    let outer_width = current.width + margin.left + margin.right;
                    if outer_width <= (right_edge - left_edge).max(0.0) + 0.01 {
                        break;
                    }
                    let next_y = overlapping
                        .iter()
                        .map(|(_, float)| float.y + float.height)
                        .fold(current.y, f32::max);
                    let delta_y = next_y - current.y;
                    if delta_y <= f32::EPSILON {
                        break;
                    }
                    shift_subtree(layouts, layout_position, child_index, child_count, delta_y);
                    current.y = next_y;
                }
            }
            if child.style.clear != WClear::None {
                let clearance_bottom = active_floats
                    .iter()
                    .filter(|(side, _)| {
                        matches!(
                            (child.style.clear, *side),
                            (WClear::Left, WFloat::Left)
                                | (WClear::Right, WFloat::Right)
                                | (WClear::Both, WFloat::Left | WFloat::Right)
                        )
                    })
                    .map(|(_, rect)| rect.y + rect.height)
                    .reduce(f32::max);
                if let (Some(clearance_bottom), Some(position)) =
                    (clearance_bottom, layout_position.get(&child_index).copied())
                {
                    let current_y = layouts[position].0.y;
                    let relative_offset = if child.style.position == WPos::Relative {
                        let resolve_inset = |dimension: WDim| {
                            (!matches!(dimension, WDim::Auto | WDim::Percent(_)))
                                .then(|| {
                                    dimension.resolve(
                                        viewport_h,
                                        ROOT_FONT_SIZE,
                                        child.style.font_size,
                                        viewport_w,
                                        viewport_h,
                                    )
                                })
                                .flatten()
                        };
                        resolve_inset(child.style.top)
                            .or_else(|| resolve_inset(child.style.bottom).map(|value| -value))
                            .unwrap_or(0.0)
                    } else {
                        0.0
                    };
                    let static_y = current_y - relative_offset;
                    let target_y = (static_y - leading_margin_group(child))
                        .max(clearance_bottom)
                        + relative_offset;
                    if (target_y - current_y).abs() > f32::EPSILON {
                        shift_subtree(
                            layouts,
                            layout_position,
                            child_index,
                            child_count,
                            target_y - current_y,
                        );
                    }
                }
            }
            if child.style.float != WFloat::None
                && let Some(position) = layout_position.get(&child_index).copied()
            {
                let rect = layouts[position].0;
                active_floats.push((
                    child.style.float,
                    LayoutRect {
                        height: rect.height + child.style.margin_lengths().bottom,
                        ..rect
                    },
                ));
                float_since_in_flow = true;
            } else {
                previous_in_flow = Some((child_index, child));
            }
            previous = Some((child_index, child));
            child_index += child_count;
        }
        if normal_flow_correction < -f32::EPSILON
            && matches!(component.style.height, WDim::Auto)
            && let Some(position) = layout_position.get(&component_index).copied()
        {
            layouts[position].0.height += normal_flow_correction;
        }
    }

    visit(
        root,
        0,
        layouts,
        &layout_position,
        viewport_w,
        viewport_h,
    );
}

fn project_positioned_bfc_float_heights(
    layouts: &mut [(LayoutRect, usize)],
    root: &Component,
    flat: &[FlatNodeInfo<'_>],
    viewport_w: f32,
    viewport_h: f32,
) {
    let positions = layouts
        .iter()
        .enumerate()
        .map(|(position, (_, index))| (*index, position))
        .collect::<HashMap<_, _>>();

    fn visit(
        component: &Component,
        index: usize,
        layouts: &mut [(LayoutRect, usize)],
        positions: &HashMap<usize, usize>,
        flat: &[FlatNodeInfo<'_>],
        viewport_w: f32,
        viewport_h: f32,
    ) {
        if matches!(component.style.position, WPos::Absolute | WPos::Fixed)
            && matches!(component.style.height, WDim::Auto)
            && let Some(position) = positions.get(&index).copied()
        {
            let container = layouts[position].0;
            let subtree_end = index + count_nodes(component);
            let float_bottom = (index + 1..subtree_end)
                .filter(|candidate| {
                    positions.contains_key(candidate)
                        && flat[*candidate].style.float != WFloat::None
                })
                .filter_map(|candidate| {
                    let float = &flat[candidate];
                    let rect = layouts[*positions.get(&candidate)?].0;
                    let margin_bottom = resolve_spacing_for_layout(
                        float.style.margin.bottom,
                        container.width,
                        float.style.font_size,
                        viewport_w,
                        viewport_h,
                    );
                    Some(rect.y + rect.height + margin_bottom)
                })
                .reduce(f32::max);
            if let Some(float_bottom) = float_bottom {
                let padding = component.style.padding_lengths();
                let bottom_edge = padding.bottom
                    + component
                        .style
                        .border_bottom_width
                        .unwrap_or(component.style.border_width);
                layouts[position].0.height = layouts[position]
                    .0
                    .height
                    .max(float_bottom + bottom_edge - container.y);
            }
        }

        let mut child_index = index + 1;
        for child in &component.children {
            visit(
                child,
                child_index,
                layouts,
                positions,
                flat,
                viewport_w,
                viewport_h,
            );
            child_index += count_nodes(child);
        }
    }

    visit(root, 0, layouts, &positions, flat, viewport_w, viewport_h);
}

fn project_forced_break_lines(layouts: &mut [(LayoutRect, usize)], root: &Component) {
    let layout_position = layouts
        .iter()
        .enumerate()
        .map(|(position, (_, index))| (*index, position))
        .collect::<HashMap<_, _>>();

    fn shift_subtree(
        layouts: &mut [(LayoutRect, usize)],
        layout_position: &HashMap<usize, usize>,
        start: usize,
        count: usize,
        delta_x: f32,
        delta_y: f32,
    ) {
        for index in start..start + count {
            if let Some(position) = layout_position.get(&index).copied() {
                layouts[position].0.x += delta_x;
                layouts[position].0.y += delta_y;
            }
        }
    }

    fn visit(
        component: &Component,
        component_index: usize,
        layouts: &mut [(LayoutRect, usize)],
        layout_position: &HashMap<usize, usize>,
    ) {
        let mut child_index = component_index + 1;
        for child in &component.children {
            visit(child, child_index, layouts, layout_position);
            child_index += count_nodes(child);
        }

        let Some(parent_position) = layout_position.get(&component_index).copied() else {
            return;
        };
        let parent_rect = layouts[parent_position].0;
        let padding = component.style.padding_lengths();
        let line_start = parent_rect.x
            + component
                .style
                .border_left_width
                .unwrap_or(component.style.border_width)
            + padding.left;
        let mut line_top = None::<f32>;
        let mut line_height = component.style.font_size * component.style.line_height;
        let mut child_index = component_index + 1;
        let children = component
            .children
            .iter()
            .map(|child| {
                let index = child_index;
                child_index += count_nodes(child);
                (child, index)
            })
            .collect::<Vec<_>>();
        if !children.iter().any(|(child, _)| {
            matches!(
                &child.kind,
                ComponentKind::Text { content } if content == "\u{2028}"
            )
        }) {
            return;
        }

        for (position, (child, index)) in children.iter().enumerate() {
            let is_break = matches!(
                &child.kind,
                ComponentKind::Text { content } if content == "\u{2028}"
            );
            if !is_break {
                if let Some(layout) = layout_position
                    .get(index)
                    .copied()
                    .map(|position| layouts[position].0)
                {
                    line_top.get_or_insert(layout.y);
                    line_height = line_height.max(layout.height);
                }
                continue;
            }
            if let Some(layout) = layout_position
                .get(index)
                .copied()
                .map(|position| layouts[position].0)
            {
                line_height = line_height.max(layout.height);
            }
            let Some((_, next_index)) = children.get(position + 1) else {
                continue;
            };
            if !layout_position.contains_key(next_index) {
                continue;
            }
            let target_y = line_top.unwrap_or(parent_rect.y) + line_height;
            let following_line = children[position + 1..]
                .iter()
                .take_while(|(following, _)| {
                    !matches!(
                        &following.kind,
                        ComponentKind::Text { content } if content == "\u{2028}"
                    )
                })
                .skip_while(|(following, _)| {
                    matches!(
                        &following.kind,
                        ComponentKind::Text { content }
                            if content.chars().all(char::is_whitespace)
                                && matches!(
                                    following.style.white_space,
                                    WWhiteSpace::Normal
                                        | WWhiteSpace::NoWrap
                                        | WWhiteSpace::PreLine
                                )
                    )
                })
                .collect::<Vec<_>>();
            let line_width = following_line
                .iter()
                .filter(|(following, _)| {
                    !matches!(following.style.position, WPos::Absolute | WPos::Fixed)
                })
                .filter_map(|(following, following_index)| {
                    let following_position = layout_position.get(following_index).copied()?;
                    let margin = following.style.margin_lengths();
                    let margin_left = match following.style.margin.left {
                        WSpacing::Percent(value) => parent_rect.width * value / 100.0,
                        _ => margin.left,
                    };
                    let margin_right = match following.style.margin.right {
                        WSpacing::Percent(value) => parent_rect.width * value / 100.0,
                        _ => margin.right,
                    };
                    Some(margin_left + layouts[following_position].0.width + margin_right)
                })
                .sum::<f32>();
            let content_width = (parent_rect.width
                - component
                    .style
                    .border_left_width
                    .unwrap_or(component.style.border_width)
                - component
                    .style
                    .border_right_width
                    .unwrap_or(component.style.border_width)
                - padding.left
                - padding.right)
                .max(0.0);
            let free_space = (content_width - line_width).max(0.0);
            let line_offset = match (component.style.text_align, component.style.direction) {
                (w3cos_std::style::TextAlign::Center, _) => free_space / 2.0,
                (w3cos_std::style::TextAlign::Right, _)
                | (
                    w3cos_std::style::TextAlign::Start,
                    w3cos_std::style::TextDirection::Rtl,
                )
                | (
                    w3cos_std::style::TextAlign::End,
                    w3cos_std::style::TextDirection::Ltr,
                ) => free_space,
                _ => 0.0,
            };
            let mut cursor_x = line_start + line_offset;
            for (following, following_index) in following_line {
                if matches!(following.style.position, WPos::Absolute | WPos::Fixed) {
                    // Out-of-flow descendants already receive their CSS static
                    // position from `inline_absolute_static_rect`. Moving them
                    // again with the projected in-flow line duplicates the
                    // inline-start padding after a forced break.
                    continue;
                }
                let Some(following_position) = layout_position.get(following_index).copied() else {
                    continue;
                };
                let margin = following.style.margin_lengths();
                let margin_left = match following.style.margin.left {
                    WSpacing::Percent(value) => parent_rect.width * value / 100.0,
                    _ => margin.left,
                };
                let margin_right = match following.style.margin.right {
                    WSpacing::Percent(value) => parent_rect.width * value / 100.0,
                    _ => margin.right,
                };
                let following_rect = layouts[following_position].0;
                let target_x = cursor_x + margin_left;
                shift_subtree(
                    layouts,
                    layout_position,
                    *following_index,
                    count_nodes(following),
                    target_x - following_rect.x,
                    target_y - following_rect.y,
                );
                cursor_x = target_x + following_rect.width + margin_right;
            }
            line_top = Some(target_y);
            line_height = component.style.font_size * component.style.line_height;
        }

        let descendant_bottom = children
            .iter()
            .filter_map(|(_, index)| {
                layout_position
                    .get(index)
                    .copied()
                    .map(|position| layouts[position].0)
            })
            .map(|rect| rect.y + rect.height)
            .fold(parent_rect.y, f32::max);
        let line_box_bottom = line_top.map_or(parent_rect.y, |top| top + line_height);
        if matches!(component.style.height, WDim::Auto) {
            layouts[parent_position].0.height =
                descendant_bottom.max(line_box_bottom) - parent_rect.y;
        }
    }

    visit(root, 0, layouts, &layout_position);
}

fn align_inline_block_last_line_baselines(
    layouts: &mut [(LayoutRect, usize)],
    root: &Component,
) {
    let positions = layouts
        .iter()
        .enumerate()
        .map(|(position, (_, index))| (*index, position))
        .collect::<HashMap<_, _>>();

    fn last_text_baseline(
        component: &Component,
        component_index: usize,
        layouts: &[(LayoutRect, usize)],
        positions: &HashMap<usize, usize>,
    ) -> Option<f32> {
        let own = (component.style.visibility == WVisibility::Visible
            && !matches!(component.style.position, WPos::Absolute | WPos::Fixed)
            && matches!(component.kind, ComponentKind::Text { .. }))
        .then(|| {
            positions
                .get(&component_index)
                .map(|position| layouts[*position].0.y + component.style.font_size * 0.8)
        })
        .flatten();
        let mut child_index = component_index + 1;
        component.children.iter().fold(own, |latest, child| {
            let child_baseline = last_text_baseline(child, child_index, layouts, positions);
            child_index += count_nodes(child);
            match (latest, child_baseline) {
                (Some(left), Some(right)) => Some(left.max(right)),
                (left, right) => left.or(right),
            }
        })
    }

    fn shift_subtree(
        layouts: &mut [(LayoutRect, usize)],
        positions: &HashMap<usize, usize>,
        component_index: usize,
        component: &Component,
        delta_y: f32,
    ) {
        for index in component_index..component_index + count_nodes(component) {
            if let Some(position) = positions.get(&index) {
                layouts[*position].0.y += delta_y;
            }
        }
    }

    fn visit(
        component: &Component,
        component_index: usize,
        layouts: &mut [(LayoutRect, usize)],
        positions: &HashMap<usize, usize>,
    ) {
        let mut child_index = component_index + 1;
        let children = component
            .children
            .iter()
            .map(|child| {
                let index = child_index;
                child_index += count_nodes(child);
                (child, index)
            })
            .collect::<Vec<_>>();
        for (child, index) in &children {
            visit(child, *index, layouts, positions);
        }

        let reference_baseline = children
            .iter()
            .filter(|(child, _)| {
                child.style.display == WDisplay::Inline
                    && child.style.visibility == WVisibility::Visible
                    && matches!(child.kind, ComponentKind::Text { .. })
            })
            .filter_map(|(child, index)| {
                let position = positions.get(index)?;
                let text_rect = layouts[*position].0;
                let shares_inline_line = children.iter().any(|(candidate, candidate_index)| {
                    if candidate.style.display != WDisplay::InlineBlock {
                        return false;
                    }
                    let Some(candidate_position) = positions.get(candidate_index) else {
                        return false;
                    };
                    let candidate_rect = layouts[*candidate_position].0;
                    text_rect.x + text_rect.width <= candidate_rect.x + f32::EPSILON
                        || text_rect.x >= candidate_rect.x + candidate_rect.width - f32::EPSILON
                });
                shares_inline_line
                    .then_some(text_rect.y + child.style.font_size * 0.8)
            })
            .reduce(f32::max);
        let Some(reference_baseline) = reference_baseline else {
            return;
        };
        let inline_blocks = children
            .iter()
            .filter(|(child, _)| {
                child.style.display == WDisplay::InlineBlock
                    && matches!(child.style.overflow, WOverflow::Visible)
            })
            .filter_map(|(child, index)| {
                last_text_baseline(child, *index, layouts, positions)
                    .map(|baseline| (*child, *index, baseline))
            })
            .collect::<Vec<_>>();
        let target_baseline = inline_blocks
            .iter()
            .filter(|(child, _, _)| {
                let margin = child.style.margin_lengths();
                let padding = child.style.padding_lengths();
                margin.top.abs() > f32::EPSILON
                    || margin.bottom.abs() > f32::EPSILON
                    || padding.top.abs() > f32::EPSILON
                    || padding.bottom.abs() > f32::EPSILON
                    || child
                        .style
                        .border_top_width
                        .unwrap_or(child.style.border_width)
                        .abs()
                        > f32::EPSILON
                    || child
                        .style
                        .border_bottom_width
                        .unwrap_or(child.style.border_width)
                        .abs()
                        > f32::EPSILON
            })
            .map(|(_, _, baseline)| *baseline)
            .fold(reference_baseline, f32::max);

        let sibling_delta = target_baseline - reference_baseline;
        if sibling_delta.abs() > f32::EPSILON {
            for (child, index) in &children {
                if child.style.display == WDisplay::Inline
                    && child.style.visibility == WVisibility::Visible
                    && matches!(child.kind, ComponentKind::Text { .. })
                {
                    shift_subtree(layouts, positions, *index, child, sibling_delta);
                }
            }
        }
        for (child, index, last_baseline) in inline_blocks {
            let delta_y = target_baseline - last_baseline;
            if delta_y.abs() > f32::EPSILON {
                shift_subtree(layouts, positions, index, child, delta_y);
            }
        }
    }

    visit(root, 0, layouts, &positions);
}

fn project_table_column_background_rects(
    layouts: &mut [(LayoutRect, usize)],
    flat: &[FlatNodeInfo<'_>],
) {
    let nearest_table = |index: usize| {
        let mut parent = flat.get(index).and_then(|entry| entry.parent);
        while let Some(index) = parent {
            let entry = flat.get(index)?;
            if matches!(entry.style.display, WDisplay::Table | WDisplay::InlineTable) {
                return Some(index);
            }
            parent = entry.parent;
        }
        None
    };
    let layout_rects = layouts
        .iter()
        .map(|(rect, index)| (*index, *rect))
        .collect::<HashMap<_, _>>();
    let union = |left: LayoutRect, right: LayoutRect| {
        let x = left.x.min(right.x);
        let y = left.y.min(right.y);
        let max_x = (left.x + left.width).max(right.x + right.width);
        let max_y = (left.y + left.height).max(right.y + right.height);
        LayoutRect {
            x,
            y,
            width: max_x - x,
            height: max_y - y,
        }
    };
    let mut table_columns = HashMap::<usize, Vec<LayoutRect>>::new();
    let mut projected_rows = HashMap::<usize, LayoutRect>::new();
    for (table, table_node) in flat
        .iter()
        .enumerate()
        .filter(|(_, node)| matches!(node.style.display, WDisplay::Table | WDisplay::InlineTable))
    {
        if !layout_rects.contains_key(&table) {
            continue;
        }
        let mut rows = flat
            .iter()
            .enumerate()
            .filter(|(index, node)| {
                node.style.display == WDisplay::TableRow
                    && nearest_table(*index) == Some(table)
                    && layout_rects.contains_key(index)
            })
            .map(|(index, _)| {
                let cells = flat
                    .iter()
                    .enumerate()
                    .filter(|(_, node)| {
                        node.parent == Some(index) && node.style.display == WDisplay::TableCell
                    })
                    .filter_map(|(cell, _)| {
                        layout_rects.get(&cell).copied().map(|rect| (cell, rect))
                    })
                    .collect::<Vec<_>>();
                (index, cells)
            })
            .collect::<Vec<_>>();
        rows.sort_by(|(left, _), (right, _)| {
            layout_rects[left]
                .y
                .total_cmp(&layout_rects[right].y)
                .then_with(|| left.cmp(right))
        });
        let column_count = rows.iter().map(|(_, cells)| cells.len()).max().unwrap_or(0);
        if column_count == 0 {
            continue;
        }
        let mut bounds = vec![None::<LayoutRect>; column_count];
        for (row, (row_index, cells)) in rows.iter().enumerate() {
            let mut row_bounds = None::<LayoutRect>;
            for (column, (_, cell)) in cells.iter().enumerate() {
                let left = if column == 0 {
                    collapsed_layout_edge_width(table_node.style, 3) / 2.0
                } else {
                    let previous = cells[column - 1].1;
                    (previous.x + previous.width - cell.x).max(0.0) / 2.0
                };
                let right = if column + 1 == cells.len() {
                    collapsed_layout_edge_width(table_node.style, 1) / 2.0
                } else {
                    (cell.x + cell.width - cells[column + 1].1.x).max(0.0) / 2.0
                };
                let top = if row == 0 {
                    collapsed_layout_edge_width(table_node.style, 0) / 2.0
                } else {
                    rows[row - 1].1.get(column).map_or(0.0, |(_, previous)| {
                        (previous.y + previous.height - cell.y).max(0.0) / 2.0
                    })
                };
                let bottom = if row + 1 == rows.len() {
                    collapsed_layout_edge_width(table_node.style, 2) / 2.0
                } else {
                    rows[row + 1].1.get(column).map_or(0.0, |(_, next)| {
                        (cell.y + cell.height - next.y).max(0.0) / 2.0
                    })
                };
                let projected = if table_node.style.border_collapse {
                    LayoutRect {
                        x: cell.x + left,
                        y: cell.y + top,
                        width: (cell.width - left - right).max(0.0),
                        height: (cell.height - top - bottom).max(0.0),
                    }
                } else {
                    *cell
                };
                row_bounds = Some(row_bounds.map_or(projected, |rect| union(rect, projected)));
                bounds[column] =
                    Some(bounds[column].map_or(projected, |rect| union(rect, projected)));
            }
            if let Some(mut rect) = row_bounds {
                if !table_node.style.border_collapse && table_node.style.table_layout_fixed {
                    let table_rect = layout_rects[&table];
                    let left = table_node
                        .style
                        .border_left_width
                        .unwrap_or(table_node.style.border_width);
                    let right = table_node
                        .style
                        .border_right_width
                        .unwrap_or(table_node.style.border_width);
                    // Fixed-layout row and row-group backgrounds span the
                    // table's definite inner inline area. The cell union can
                    // be narrower after track resolution; auto tables keep
                    // using their content-derived cell union.
                    rect.x = table_rect.x + left;
                    rect.width = (table_rect.width - left - right).max(0.0);
                }
                projected_rows.insert(*row_index, rect);
            }
        }
        table_columns.insert(table, bounds.into_iter().flatten().collect());
    }

    let mut projected = HashMap::<usize, LayoutRect>::new();
    let mut next_column = HashMap::<usize, usize>::new();
    for (index, node) in flat.iter().enumerate() {
        if node.style.display != WDisplay::TableColumn {
            continue;
        }
        let Some(table) = nearest_table(index) else {
            continue;
        };
        let column = next_column.entry(table).or_default();
        if let Some(rect) = table_columns
            .get(&table)
            .and_then(|columns| columns.get(*column))
        {
            projected.insert(index, *rect);
        }
        *column += 1;
    }
    for (index, node) in flat.iter().enumerate() {
        if node.style.display != WDisplay::TableColumnGroup {
            continue;
        }
        let rect = projected
            .iter()
            .filter(|(column, _)| {
                let mut parent = flat[**column].parent;
                while let Some(candidate) = parent {
                    if candidate == index {
                        return true;
                    }
                    parent = flat[candidate].parent;
                }
                false
            })
            .map(|(_, rect)| *rect)
            .reduce(union);
        if let Some(rect) = rect {
            projected.insert(index, rect);
        }
    }
    projected.extend(projected_rows.iter().map(|(index, rect)| (*index, *rect)));
    for (index, node) in flat.iter().enumerate() {
        if !matches!(
            node.style.display,
            WDisplay::TableRowGroup | WDisplay::TableHeaderGroup | WDisplay::TableFooterGroup
        ) {
            continue;
        }
        let rect = projected_rows
            .iter()
            .filter(|(row, _)| {
                let mut parent = flat[**row].parent;
                while let Some(candidate) = parent {
                    if candidate == index {
                        return true;
                    }
                    parent = flat[candidate].parent;
                }
                false
            })
            .map(|(_, rect)| *rect)
            .reduce(union);
        if let Some(rect) = rect {
            projected.insert(index, rect);
        }
    }
    for (rect, index) in layouts.iter_mut() {
        if let Some(projected) = projected.get(index) {
            let mut projected = *projected;
            let node = &flat[*index];
            let collapsed_table = nearest_table(*index)
                .and_then(|table| flat.get(table))
                .is_some_and(|table| table.style.border_collapse);
            if collapsed_table {
                let top = collapsed_layout_edge_width(node.style, 0);
                let right = collapsed_layout_edge_width(node.style, 1);
                let bottom = collapsed_layout_edge_width(node.style, 2);
                let left = collapsed_layout_edge_width(node.style, 3);
                match node.style.display {
                    WDisplay::TableRowGroup
                    | WDisplay::TableHeaderGroup
                    | WDisplay::TableFooterGroup => {
                        projected.x -= left;
                        projected.y -= top;
                        projected.width += left;
                        projected.height += top + bottom;
                    }
                    WDisplay::TableRow => {
                        projected.y -= top;
                        projected.width += right;
                        projected.height += top + bottom;
                    }
                    WDisplay::TableColumnGroup | WDisplay::TableColumn => {
                        projected.width += right;
                        projected.height += top + bottom;
                    }
                    _ => {}
                }
            }
            *rect = projected;
        }
    }
}

fn extend_scroll_extents_from_descendants(
    layouts: &[(LayoutRect, usize)],
    flat: &[FlatNodeInfo<'_>],
    scroll_ancestor: &[Option<usize>],
    scrollable: &mut [(usize, LayoutRect, ScrollExtent)],
) {
    let scrollport_positions = scrollable
        .iter()
        .enumerate()
        .map(|(position, (index, _, _))| (*index, position))
        .collect::<HashMap<_, _>>();
    for (child, child_index) in layouts {
        if matches!(
            flat.get(*child_index).map(|entry| entry.style.position),
            Some(WPos::Fixed)
        ) {
            continue;
        }
        let Some(scroll_index) = scroll_ancestor.get(*child_index).copied().flatten() else {
            continue;
        };
        let Some(position) = scrollport_positions.get(&scroll_index).copied() else {
            continue;
        };
        let (_, scrollport, extent) = &mut scrollable[position];
        extent.max_x = extent
            .max_x
            .max(child.x + child.width - (scrollport.x + scrollport.width));
        extent.max_y = extent
            .max_y
            .max(child.y + child.height - (scrollport.y + scrollport.height));
    }
}

fn root_auto_margin_offset(
    style: Option<&w3cos_std::style::Style>,
    root_width: f32,
    viewport_w: f32,
    viewport_h: f32,
) -> f32 {
    let Some(style) = style else {
        return 0.0;
    };
    let WSpacing::Auto = style.margin.left else {
        return 0.0;
    };
    let right_auto = matches!(style.margin.right, WSpacing::Auto);
    let right = if right_auto {
        0.0
    } else {
        match style.margin.right {
            WSpacing::Px(value) => value,
            WSpacing::Percent(value) => viewport_w * value / 100.0,
            WSpacing::Rem(value) => value * ROOT_FONT_SIZE,
            WSpacing::Em(value) => value * style.font_size,
            WSpacing::Vw(value) => value * viewport_w / 100.0,
            WSpacing::Vh(value) => value * viewport_h / 100.0,
            WSpacing::Auto => 0.0,
            other => other.resolve(&w3cos_std::safe_area::current()),
        }
    };
    let remaining = (viewport_w - root_width - right).max(0.0);
    if right_auto {
        remaining / 2.0
    } else {
        remaining
    }
}

fn root_used_margins(
    style: Option<&w3cos_std::style::Style>,
    viewport_w: f32,
    viewport_h: f32,
) -> EdgeLengths {
    let Some(style) = style else {
        return EdgeLengths {
            top: 0.0,
            right: 0.0,
            bottom: 0.0,
            left: 0.0,
        };
    };
    let resolve = |spacing: WSpacing| match spacing {
        WSpacing::Percent(value) => viewport_w * value / 100.0,
        WSpacing::Rem(value) => value * ROOT_FONT_SIZE,
        WSpacing::Em(value) => value * style.font_size,
        WSpacing::Vw(value) => value * viewport_w / 100.0,
        WSpacing::Vh(value) => value * viewport_h / 100.0,
        WSpacing::Auto => 0.0,
        other => other.resolve(&w3cos_std::safe_area::current()),
    };
    EdgeLengths {
        top: resolve(style.margin.top),
        right: resolve(style.margin.right),
        bottom: resolve(style.margin.bottom),
        left: resolve(style.margin.left),
    }
}

fn root_relative_offset(
    style: Option<&w3cos_std::style::Style>,
    viewport_w: f32,
    viewport_h: f32,
) -> (f32, f32) {
    let Some(style) = style.filter(|style| matches!(style.position, WPos::Relative)) else {
        return (0.0, 0.0);
    };
    let resolve_h = |dimension: WDim| {
        dimension.resolve(
            viewport_w,
            ROOT_FONT_SIZE,
            style.font_size,
            viewport_w,
            viewport_h,
        )
    };
    let resolve_v = |dimension: WDim| {
        dimension.resolve(
            viewport_h,
            ROOT_FONT_SIZE,
            style.font_size,
            viewport_w,
            viewport_h,
        )
    };
    let x = match (resolve_h(style.left), resolve_h(style.right)) {
        (Some(left), _) => left,
        (None, Some(right)) => -right,
        (None, None) => 0.0,
    };
    let y = match (resolve_v(style.top), resolve_v(style.bottom)) {
        (Some(top), _) => top,
        (None, Some(bottom)) => -bottom,
        (None, None) => 0.0,
    };
    (x, y)
}

// ---------------------------------------------------------------------------
// Internal: Taffy tree construction
// ---------------------------------------------------------------------------

fn component_content_width(
    style: &w3cos_std::style::Style,
    containing_width: f32,
    viewport_w: f32,
    viewport_h: f32,
) -> f32 {
    let resolve_edge = |spacing: WSpacing| match spacing {
        WSpacing::Percent(value) => containing_width * value / 100.0,
        WSpacing::Rem(value) => value * ROOT_FONT_SIZE,
        WSpacing::Em(value) => value * style.font_size,
        WSpacing::Vw(value) => value * viewport_w / 100.0,
        WSpacing::Vh(value) => value * viewport_h / 100.0,
        WSpacing::Auto => 0.0,
        other => other.resolve(&w3cos_std::safe_area::current()),
    };
    let horizontal_inner_edges = resolve_edge(style.padding.left)
        + resolve_edge(style.padding.right)
        + style.border_left_width.unwrap_or(style.border_width)
        + style.border_right_width.unwrap_or(style.border_width);
    let specified = match style.width {
        WDim::Ch(value) => Some(
            value
                * layout_font()
                    .metrics('0', style.font_size)
                    .advance_width,
        ),
        _ => style.width.resolve(
            containing_width,
            ROOT_FONT_SIZE,
            style.font_size,
            viewport_w,
            viewport_h,
        ),
    };
    let mut width = specified.unwrap_or_else(|| {
        containing_width
            - resolve_edge(style.margin.left)
            - resolve_edge(style.margin.right)
            - horizontal_inner_edges
    });
    if specified.is_some() && style.box_sizing == WBoxSizing::BorderBox {
        width -= horizontal_inner_edges;
    }
    width.max(0.0)
}

fn build_taffy_tree(
    tree: &mut TaffyTree<usize>,
    comp: &Component,
    idx: &mut usize,
    parent_direction: Option<WDir>,
    parent_display: Option<WDisplay>,
    parent_align_items: Option<WAlign>,
    _parent_font_size: Option<f32>,
    viewport_w: f32,
    viewport_h: f32,
    containing_width: f32,
    quirks_height_basis: Option<f32>,
    definite_height_basis: Option<f32>,
    inherited_table_tracks: Option<&[f32]>,
    inherited_fixed_table_layout: bool,
    inherited_collapsed_single_track: Option<CollapsedSingleTrack>,
    table_column: Option<usize>,
    parent_table_height_definite: bool,
    inherited_border_spacing: Option<(f32, f32)>,
) -> Result<NodeId, taffy::TaffyError> {
    let my_idx = *idx;
    *idx += 1;

    let mut style = to_taffy_style(&comp.style, viewport_w, viewport_h);
    let quirks_percentage_height = comp
        .style
        .custom_properties
        .as_ref()
        .is_some_and(|properties| {
            properties.contains_key("--w3cos-internal-quirks-percentage-height")
        });
    let own_quirks_height_basis = match comp.style.height {
        WDim::Px(value) => Some(value),
        WDim::Em(value) => Some(value * comp.style.font_size),
        WDim::Rem(value) => Some(value * ROOT_FONT_SIZE),
        WDim::Vh(value) => Some(value * viewport_h / 100.0),
        WDim::Percent(value) if quirks_percentage_height => {
            quirks_height_basis.map(|basis| basis * value / 100.0)
        }
        _ => None,
    };
    if quirks_percentage_height
        && let Some(height) = own_quirks_height_basis
    {
        style.size.height = Dimension::length(height);
    }
    let child_quirks_height_basis = own_quirks_height_basis.or(quirks_height_basis);
    let own_definite_height_basis = match comp.style.height {
        WDim::Px(value) => Some(value),
        WDim::Em(value) => Some(value * comp.style.font_size),
        WDim::Rem(value) => Some(value * ROOT_FONT_SIZE),
        WDim::Vh(value) => Some(value * viewport_h / 100.0),
        WDim::Percent(value) => definite_height_basis.map(|basis| basis * value / 100.0),
        WDim::Auto | WDim::Ch(_) | WDim::Vw(_) => None,
    };
    if let (WDim::Percent(value), Some(basis)) =
        (comp.style.min_height, definite_height_basis)
    {
        style.min_size.height = Dimension::length(basis * value / 100.0);
    }
    if let (WDim::Percent(value), Some(basis)) =
        (comp.style.max_height, definite_height_basis)
    {
        style.max_size.height = Dimension::length(basis * value / 100.0);
    }
    let marked_replaced_element = matches!(comp.kind, ComponentKind::SvgDocument { .. })
        || comp
            .style
            .custom_properties
            .as_ref()
            .is_some_and(|properties| {
                properties.contains_key("--w3cos-internal-replaced-element")
            });
    let passive_inline_edges = !marked_replaced_element
        && matches!(
        comp.kind,
        ComponentKind::Row
            | ComponentKind::Column
            | ComponentKind::Box
            | ComponentKind::Text { .. }
    )
        && comp.style.display == WDisplay::Inline
        && !matches!(comp.style.position, WPos::Absolute | WPos::Fixed);
    if passive_inline_edges {
        // Vertical padding and borders paint on a non-replaced inline box but
        // do not participate in line-box height. Taffy's flex item model
        // otherwise enlarges the line and moves every following line/float.
        // Preserve horizontal edges for line fitting; restore the painted
        // vertical border-box extent while collecting layout rectangles.
        style.padding.top = LengthPercentage::length(0.0);
        style.padding.bottom = LengthPercentage::length(0.0);
        style.border.top = LengthPercentage::length(0.0);
        style.border.bottom = LengthPercentage::length(0.0);
        style.min_size.width = Dimension::auto();
        style.max_size.width = Dimension::auto();
    }
    let owns_table_layout = matches!(comp.style.display, WDisplay::Table | WDisplay::InlineTable);
    if matches!(
        comp.style.display,
        WDisplay::TableColumnGroup | WDisplay::TableColumn
    ) {
        // Columns contribute track metadata and paint layers, not in-flow
        // block-axis boxes. Keeping them as ordinary Taffy children inserts
        // table `gap` slots before the first row in the separated model.
        style.position = taffy::Position::Absolute;
    }
    if comp.style.visibility == WVisibility::Collapse
        && matches!(
            comp.style.display,
            WDisplay::TableRow
                | WDisplay::TableCell
                | WDisplay::TableRowGroup
                | WDisplay::TableHeaderGroup
                | WDisplay::TableFooterGroup
        )
    {
        if matches!(
            comp.style.display,
            WDisplay::TableRow
                | WDisplay::TableRowGroup
                | WDisplay::TableHeaderGroup
                | WDisplay::TableFooterGroup
        ) {
            // Collapsed rows still participate in intrinsic table sizing and
            // border conflict collection, but rows and row groups generate no
            // used block-axis track.
            style.display = taffy::Display::None;
        }
        style.size.height = Dimension::length(0.0);
        style.min_size.height = Dimension::length(0.0);
        style.max_size.height = Dimension::length(0.0);
        style.padding.top = LengthPercentage::length(0.0);
        style.padding.bottom = LengthPercentage::length(0.0);
        style.border.top = LengthPercentage::length(0.0);
        style.border.bottom = LengthPercentage::length(0.0);
    }
    let active_fixed_table_layout = if owns_table_layout {
        comp.style.table_layout_fixed
    } else {
        inherited_fixed_table_layout
    };
    if owns_table_layout
        && !matches!(comp.style.width, WDim::Auto)
        && is_html_table_element(&comp.style)
    {
        // HTML table elements use the UA's border-box table sizing behavior.
        // An arbitrary element whose computed display is `table` keeps its
        // authored box-sizing, so a declared content width still expands by
        // its padding and borders.
        style.box_sizing = BoxSizing::BorderBox;
    }
    if owns_table_layout && comp.style.table_layout_fixed {
        // CSS table width is the used table border-box width in the fixed
        // algorithm. Taffy's generic content-box default would add the table
        // border and outer border-spacing a second time. Authored padding on
        // a separate-border table still expands that used width.
        style.box_sizing = BoxSizing::BorderBox;
        if !comp.style.border_collapse && !is_html_table_element(&comp.style) {
            let padding = comp.style.padding_lengths();
            let borders = comp
                .style
                .border_left_width
                .unwrap_or(comp.style.border_width)
                + comp
                    .style
                    .border_right_width
                    .unwrap_or(comp.style.border_width);
            if let Some(width) = comp.style.width.resolve(
                containing_width,
                ROOT_FONT_SIZE,
                comp.style.font_size,
                viewport_w,
                viewport_h,
            ) {
                style.size.width =
                    Dimension::length(width + padding.left + padding.right + borders);
            }
        }
    }
    if owns_table_layout && comp.style.border_collapse && matches!(comp.style.width, WDim::Auto) {
        // Taffy's flex intrinsic sizing counts every cell border in full even
        // after adjacent collapsed borders overlap through negative margins.
        // Resolve the table's auto border-box width from the collapsed tracks
        // so its principal box uses the same shared grid lines as its rows.
        style.size.width = Dimension::length(shrink_to_fit_used_width(comp));
        style.box_sizing = BoxSizing::BorderBox;
        style.max_size.width = Dimension::percent(1.0);
    }
    if owns_table_layout && comp.style.border_collapse {
        // Collapsed outer borders are centered on the table grid edge and
        // participate in the same conflict set as boundary cell borders. The
        // boundary cells carry the resolved paint; the table wrapper must not
        // inset the grid by a second full border width.
        style.border = Rect {
            top: LengthPercentage::length(0.0),
            right: LengthPercentage::length(0.0),
            bottom: LengthPercentage::length(0.0),
            left: LengthPercentage::length(0.0),
        };
        if matches!(comp.style.height, WDim::Auto)
            && let Some(min_height) = collapsed_table_specified_rows_min_height(comp)
        {
            style.min_size.height =
                Dimension::length(min_height + table_caption_intrinsic_height(comp));
        }
    }
    if owns_table_layout {
        let absolute_height = |dimension| match dimension {
            WDim::Px(height) => Some(height),
            WDim::Em(height) => Some(height * comp.style.font_size),
            WDim::Rem(height) => Some(height * ROOT_FONT_SIZE),
            _ => None,
        };
        let specified_height = absolute_height(comp.style.height);
        let mut used_height = specified_height
            .or_else(|| absolute_height(comp.style.min_height))
            .map(|height| height.max(0.0));
        if let (Some(height), Some(max_height)) =
            (used_height.as_mut(), absolute_height(comp.style.max_height))
        {
            *height = height.min(max_height);
        }
        if let (Some(height), Some(min_height)) =
            (used_height.as_mut(), absolute_height(comp.style.min_height))
        {
            *height = height.max(min_height);
        }
        if let Some(height) = used_height {
            let caption_height = table_caption_intrinsic_height(comp);
            // A table's constrained height applies to its grid; caption boxes
            // live outside that height but inside the anonymous table wrapper.
            // A minimum also participates in row-height distribution when the
            // authored height remains auto.
            style.size.height = Dimension::length(height + caption_height);
        }
    }
    if matches!(parent_display, Some(WDisplay::TableCell))
        && matches!(
            comp.style.display,
            WDisplay::Block | WDisplay::Flex | WDisplay::Grid | WDisplay::ListItem
        )
        && matches!(comp.style.width, WDim::Auto)
    {
        // A table cell establishes the containing block for normal block-level
        // children. Their auto inline size fills the available cell content
        // width even when the cell uses baseline alignment for inline content.
        style.align_self = Some(AlignSelf::Stretch);
    }
    let own_border_spacing = matches!(comp.style.display, WDisplay::Table | WDisplay::InlineTable)
        .then(|| effective_table_border_spacing(&comp.style));
    let active_border_spacing = own_border_spacing.or(inherited_border_spacing);
    if matches!(comp.style.display, WDisplay::Table | WDisplay::InlineTable) {
        let padding = comp.style.padding_lengths();
        let (spacing_x, spacing_y) = effective_table_border_spacing(&comp.style);
        let padding_top = if comp.style.border_collapse {
            0.0
        } else {
            padding.top
        };
        let padding_right = if comp.style.border_collapse {
            0.0
        } else {
            padding.right
        };
        let padding_bottom = if comp.style.border_collapse {
            0.0
        } else {
            padding.bottom
        };
        let padding_left = if comp.style.border_collapse {
            0.0
        } else {
            padding.left
        };
        style.padding = Rect {
            top: LengthPercentage::length(padding_top + spacing_y),
            right: LengthPercentage::length(padding_right + spacing_x),
            bottom: LengthPercentage::length(padding_bottom + spacing_y),
            left: LengthPercentage::length(padding_left + spacing_x),
        };
        style.gap.height = LengthPercentage::length(spacing_y);
    } else if let Some((spacing_x, spacing_y)) = active_border_spacing {
        if comp.style.display == WDisplay::TableRow {
            style.gap.width = LengthPercentage::length(spacing_x);
        } else if matches!(
            comp.style.display,
            WDisplay::TableRowGroup | WDisplay::TableHeaderGroup | WDisplay::TableFooterGroup
        ) {
            style.gap.height = LengthPercentage::length(spacing_y);
        }
    }
    let table_cell_padding = comp.style.padding_lengths();
    let uses_ua_table_cell_padding = comp.style.display == WDisplay::TableCell
        && comp
            .style
            .custom_properties
            .as_ref()
            .is_some_and(|properties| {
                properties.contains_key("--w3cos-internal-table-cell-ua-padding")
            });
    let absolute_dimension = |dimension: WDim| match dimension {
        WDim::Px(value) => Some(value),
        WDim::Rem(value) => Some(value * ROOT_FONT_SIZE),
        WDim::Em(value) => Some(value * comp.style.font_size),
        WDim::Ch(value) => Some(
            value
                * layout_font()
                    .metrics('0', comp.style.font_size)
                    .advance_width,
        ),
        WDim::Vw(value) => Some(value * viewport_w / 100.0),
        WDim::Vh(value) => Some(value * viewport_h / 100.0),
        WDim::Auto | WDim::Percent(_) => None,
    };
    let absorbs_ua_inline_padding =
        uses_ua_table_cell_padding && absolute_dimension(comp.style.width).is_some();
    let absorbs_ua_block_padding =
        uses_ua_table_cell_padding && absolute_dimension(comp.style.height).is_some();
    let table_cell_span = table_cell_column_span(&comp.style);
    let inherited_cell_tracks = if comp.style.display == WDisplay::TableCell {
        table_column.and_then(|column| {
            inherited_table_tracks?.get(column..column.saturating_add(table_cell_span))
        })
    } else {
        None
    };
    let partially_collapsed_span = inherited_cell_tracks.is_some_and(|tracks| {
        tracks.iter().any(|width| width.is_sign_negative())
            && tracks.iter().any(|width| !width.is_sign_negative())
    });
    let collapsed_span_leading_width = inherited_cell_tracks.map_or(0.0, |tracks| {
        tracks
            .iter()
            .take_while(|width| width.is_sign_negative())
            .map(|width| width.abs())
            .sum()
    });
    if partially_collapsed_span {
        // A spanning cell remains in the grid when only part of its covered
        // columns collapse. Clip the removed track's content rather than
        // hiding the entire cell.
        style.overflow.x = taffy::Overflow::Hidden;
    }
    if absorbs_ua_inline_padding || absorbs_ua_block_padding {
        // A table-cell's CSS width/height participates in table sizing as its
        // minimum box size. Normalize the UA's 1px padding into definite
        // dimensions so equivalent legacy cellpadding=0 references produce
        // the same anonymous content box without changing the outer cell.
        if let Some(width) = absolute_dimension(comp.style.width) {
            style.size.width =
                Dimension::length(width + table_cell_padding.left + table_cell_padding.right);
        }
        if let Some(height) = absolute_dimension(comp.style.height) {
            style.size.height =
                Dimension::length(height + table_cell_padding.top + table_cell_padding.bottom);
        }
        style.padding = Rect {
            top: LengthPercentage::length(if absorbs_ua_block_padding {
                0.0
            } else {
                table_cell_padding.top
            }),
            right: LengthPercentage::length(if absorbs_ua_inline_padding {
                0.0
            } else {
                table_cell_padding.right
            }),
            bottom: LengthPercentage::length(if absorbs_ua_block_padding {
                0.0
            } else {
                table_cell_padding.bottom
            }),
            left: LengthPercentage::length(if absorbs_ua_inline_padding {
                0.0
            } else {
                table_cell_padding.left
            }),
        };
    }
    if comp.style.display == WDisplay::TableCell
        && table_column == Some(0)
        && let Some(track) = inherited_collapsed_single_track
    {
        let (offset, border_box_width) = track.cell_geometry(&comp.style);
        let left = comp
            .style
            .border_left_width
            .unwrap_or(comp.style.border_width);
        let right = comp
            .style
            .border_right_width
            .unwrap_or(comp.style.border_width);
        let padding_width = if absorbs_ua_inline_padding {
            0.0
        } else {
            table_cell_padding.left + table_cell_padding.right
        };
        let content_width = (border_box_width - left - right - padding_width).max(0.0);
        style.size.width = Dimension::length(content_width);
        style.flex_basis = Dimension::length(content_width);
        style.flex_grow = 0.0;
        style.flex_shrink = 0.0;
        style.inset.left = LengthPercentageAuto::length(offset);
    } else if comp.style.display == WDisplay::TableCell
        && let Some(tracks) = inherited_cell_tracks
    {
        let collapsed_column = tracks.iter().all(|width| width.is_sign_negative());
        let width = tracks
            .iter()
            .filter(|width| !width.is_sign_negative())
            .sum::<f32>();
        let used_width = if collapsed_column {
            style.box_sizing = BoxSizing::BorderBox;
            style.min_size.width = Dimension::length(0.0);
            style.max_size.width = Dimension::length(0.0);
            style.padding.left = LengthPercentage::length(0.0);
            style.padding.right = LengthPercentage::length(0.0);
            style.border.left = LengthPercentage::length(0.0);
            style.border.right = LengthPercentage::length(0.0);
            0.0
        } else if active_fixed_table_layout {
            style.box_sizing = BoxSizing::BorderBox;
            if comp.style.border_collapse {
                // A collapsed cell owns only half of each shared grid-line
                // border for layout. Keep the authored widths on the
                // component for painting, but expose the shared halves to
                // Taffy so the specified content width is not squeezed by
                // both full borders.
                style.border.left = LengthPercentage::length(
                    comp.style
                        .border_left_width
                        .unwrap_or(comp.style.border_width)
                        / 2.0,
                );
                style.border.right = LengthPercentage::length(
                    comp.style
                        .border_right_width
                        .unwrap_or(comp.style.border_width)
                        / 2.0,
                );
            }
            width
        } else {
            let left_border = comp
                .style
                .border_left_width
                .unwrap_or(comp.style.border_width);
            let right_border = comp
                .style
                .border_right_width
                .unwrap_or(comp.style.border_width);
            let horizontal_padding = if absorbs_ua_inline_padding {
                0.0
            } else {
                table_cell_padding.left + table_cell_padding.right
            };
            let horizontal_inner_edges = horizontal_padding
                + if comp.style.border_collapse {
                    (left_border + right_border) / 2.0
                } else {
                    left_border + right_border
                };
            (width - horizontal_inner_edges).max(0.0)
        };
        style.size.width = Dimension::length(used_width);
        style.flex_basis = Dimension::length(used_width);
        style.flex_grow = 0.0;
        style.flex_shrink = 0.0;
    }
    let normal_flow_children = comp.children.iter().filter(|child| {
        !matches!(child.style.position, WPos::Absolute | WPos::Fixed)
            && child.style.display != WDisplay::None
    });
    let float_only_auto_block = comp.style.display == WDisplay::Block
        && matches!(comp.style.height, WDim::Auto)
        && comp
            .children
            .iter()
            .any(|child| child.style.float != WFloat::None)
        && comp.children.iter().all(|child| {
            child.style.display == WDisplay::None
                || matches!(child.style.position, WPos::Absolute | WPos::Fixed)
                || child.style.float != WFloat::None
        });
    let leading_float_margin_guard = if !float_only_auto_block
        && comp.style.display == WDisplay::Block
        && comp.style.padding.top == WSpacing::Px(0.0)
        && comp
            .style
            .border_top_width
            .unwrap_or(comp.style.border_width)
            == 0.0
    {
        comp.children
            .iter()
            .enumerate()
            .find(|(_, child)| {
                !matches!(child.style.position, WPos::Absolute | WPos::Fixed)
                    && child.style.display != WDisplay::None
            })
            .and_then(|(index, child)| {
                if child.style.float == WFloat::None {
                    return None;
                }
                let margin = resolve_spacing_for_layout(
                    child.style.margin.top,
                    containing_width,
                    child.style.font_size,
                    viewport_w,
                    viewport_h,
                )
                .max(0.0);
                (margin > 0.0).then_some((index, margin.min(1.0)))
            })
    } else {
        None
    };
    if let Some((_, guard)) = leading_float_margin_guard {
        // Taffy's block algorithm collapses the first child's margin through
        // its parent even when that child is a float. A compensated internal
        // padding edge establishes the required non-collapsing boundary while
        // preserving the authored used position exactly.
        style.padding.top = LengthPercentage::length(guard);
    }
    let empty_inline_establishes_visible_line = |child: &Component| {
        if child.style.display != WDisplay::Inline
            || !child.children.is_empty()
            || !matches!(&child.kind, ComponentKind::Row | ComponentKind::Box)
        {
            return false;
        }
        let padding = child.style.padding_lengths();
        child.style.border_width > 0.0
            || child
                .style
                .border_top_width
                .is_some_and(|width| width > 0.0)
            || child
                .style
                .border_right_width
                .is_some_and(|width| width > 0.0)
            || child
                .style
                .border_bottom_width
                .is_some_and(|width| width > 0.0)
            || child
                .style
                .border_left_width
                .is_some_and(|width| width > 0.0)
            || padding.top > 0.0
            || padding.right > 0.0
            || padding.bottom > 0.0
            || padding.left > 0.0
    };
    let establishes_inline_formatting_context = matches!(comp.kind, ComponentKind::Row)
        && comp.style.display == WDisplay::Block
        && normal_flow_children.clone().next().is_some()
        && normal_flow_children.clone().all(|child| {
            matches!(
                child.style.display,
                WDisplay::Inline
                    | WDisplay::InlineBlock
                    | WDisplay::InlineFlex
                    | WDisplay::InlineTable
            )
        });
    let mixed_inline_block_formatting_context = matches!(comp.kind, ComponentKind::Row)
        && comp.style.display == WDisplay::Block
        && normal_flow_children.clone().any(|child| {
            matches!(
                child.style.display,
                WDisplay::Inline
                    | WDisplay::InlineBlock
                    | WDisplay::InlineFlex
                    | WDisplay::InlineTable
            )
        })
        && normal_flow_children.clone().any(|child| {
            matches!(
                child.style.display,
                WDisplay::Block
                    | WDisplay::Flex
                    | WDisplay::Grid
                    | WDisplay::ListItem
                    | WDisplay::Table
            )
        });
    let mixed_inline_block_flex_fallback = mixed_inline_block_formatting_context
        && (normal_flow_children
            .clone()
            .any(|child| child.style.float != WFloat::None)
            || normal_flow_children
                .clone()
                .zip(normal_flow_children.clone().skip(1))
                .any(|(left, right)| {
                    let inline_level = |child: &Component| {
                        matches!(
                            child.style.display,
                            WDisplay::Inline
                                | WDisplay::InlineBlock
                                | WDisplay::InlineFlex
                                | WDisplay::InlineTable
                        )
                    };
                    inline_level(left) && inline_level(right)
                })
            || normal_flow_children
                .clone()
                .zip(normal_flow_children.clone().skip(1))
                .any(|(left, right)| {
                    empty_inline_establishes_visible_line(left)
                        && matches!(
                            right.style.display,
                            WDisplay::Block
                                | WDisplay::Flex
                                | WDisplay::Grid
                                | WDisplay::ListItem
                                | WDisplay::Table
                        )
                }));
    if establishes_inline_formatting_context {
        // A block whose normal-flow children are all inline-level establishes
        // line boxes. Keep the inherited line-height strut even when a shorter
        // replaced element is the only child, and let vertical-align map onto
        // the row cross axis.
        style.display = taffy::Display::Flex;
        style.flex_direction = FlexDirection::Row;
        style.flex_wrap = FlexWrap::Wrap;
        style.align_items = Some(AlignItems::FlexStart);
        style.align_content = Some(AlignContent::FlexStart);
        if matches!(comp.style.height, WDim::Auto)
            && matches!(comp.style.min_height, WDim::Auto)
            && matches!(comp.style.max_height, WDim::Auto)
        {
            let line_height = comp.style.font_size * comp.style.line_height;
            let baseline_replaced_height = comp
                .children
                .iter()
                .filter(|child| {
                    matches!(child.kind, ComponentKind::Image { .. })
                        && matches!(child.style.align_self, WAlignSelf::Auto | WAlignSelf::Baseline)
                })
                .map(|child| {
                    let height = leaf_intrinsic_size_with_containing(
                        &child.kind,
                        &child.style,
                        Some(containing_width),
                    )
                    .1;
                    height + line_height * 0.2
                })
                .fold(0.0_f32, f32::max);
            // A baseline-aligned replaced element owns the full area above
            // the baseline, while the line's font strut still contributes
            // its descent below it.
            style.min_size.height = Dimension::length(line_height.max(baseline_replaced_height));
        }
    }
    if mixed_inline_block_flex_fallback {
        // Inline runs on either side of an in-flow block generate anonymous
        // block boxes. Wrapped flex lines model those runs without inserting
        // a synthetic containing block that would capture percentages.
        style.display = taffy::Display::Flex;
        style.flex_direction = FlexDirection::Row;
        style.flex_wrap = FlexWrap::Wrap;
        style.align_items = Some(AlignItems::FlexStart);
        style.align_content = Some(AlignContent::FlexStart);
    }
    if comp.style.display == WDisplay::InlineBlock
        && comp.children.iter().any(|child| {
            matches!(
                child.style.display,
                WDisplay::Block
                    | WDisplay::Flex
                    | WDisplay::Grid
                    | WDisplay::Table
                    | WDisplay::ListItem
            )
        })
    {
        // An inline-block establishes an inline-level outer box but its
        // normal-flow block children still participate in a block formatting
        // context. DOM lowering may retain a Row component kind, so restore
        // the inner block axis here without changing its outer display.
        style.flex_direction = FlexDirection::Column;
    }
    if comp.style.display == WDisplay::Inline
        && comp.style.position == WPos::Relative
        && comp.children.iter().any(|child| {
            matches!(
                child.style.display,
                WDisplay::Block
                    | WDisplay::Flex
                    | WDisplay::Grid
                    | WDisplay::Table
                    | WDisplay::ListItem
            )
        })
    {
        // Positioned inline boxes are retained as semantic containing blocks
        // when in-flow block descendants split their inline formatting
        // context. The retained host still needs a block axis internally;
        // otherwise Taffy's inline flex fallback lays the split blocks out on
        // one row after nested floats have been hoisted.
        style.flex_direction = FlexDirection::Column;
    }
    if comp.style.display == WDisplay::InlineTable
        && comp.children.iter().any(|child| {
            matches!(
                child.style.display,
                WDisplay::Block
                    | WDisplay::Flex
                    | WDisplay::Grid
                    | WDisplay::ListItem
                    | WDisplay::Table
                    | WDisplay::TableRow
                    | WDisplay::TableRowGroup
                    | WDisplay::TableHeaderGroup
                    | WDisplay::TableFooterGroup
            )
        })
    {
        // Preserve the legacy row-like fallback only for direct non-table
        // inline content. Real table rows and row groups stack on the table's
        // block axis just like they do in a block-level table.
        style.flex_direction = FlexDirection::Column;
    }
    if comp.style.display == WDisplay::TableRow
        && parent_table_height_definite
        && matches!(
            parent_display,
            Some(
                WDisplay::Table
                    | WDisplay::InlineTable
                    | WDisplay::TableRowGroup
                    | WDisplay::TableHeaderGroup
                    | WDisplay::TableFooterGroup
            )
        )
    {
        // CSS tables distribute a definite table height through their rows.
        // Flex growth only consumes positive free space, so auto-height tables
        // retain their intrinsic row heights while definite tables stretch.
        style.flex_grow = 1.0;
    }
    if comp.style.display == WDisplay::TableCell
        && matches!(parent_display, Some(WDisplay::TableRow))
        && matches!(comp.style.width, WDim::Auto)
        && table_column
            .and_then(|column| inherited_table_tracks?.get(column))
            .is_none()
    {
        // The table fallback represents a row as a flex row. Auto-width cells
        // share the row's resolved inline size like table tracks; retaining
        // each text leaf's intrinsic flex basis would cluster all columns at
        // the row start instead.
        style.flex_grow = 1.0;
        style.flex_basis = Dimension::length(0.0);
        style.min_size.width = Dimension::length(0.0);
    }
    if matches!(
        &comp.kind,
        ComponentKind::Text { content } if content == "\u{2028}"
    ) {
        // A <br> marker has no inline advance but does establish the inherited
        // line-height strut. Generic inline sizing forces both axes to auto,
        // so restore this semantic marker's explicit zero width and strut height.
        style.size = Size {
            width: Dimension::length(0.0),
            height: to_taffy_dim(
                comp.style.height,
                comp.style.font_size,
                viewport_w,
                viewport_h,
            ),
        };
    }
    if comp.style.display == WDisplay::Inline
        && comp.children.is_empty()
        && matches!(comp.kind, ComponentKind::Row | ComponentKind::Box)
        && matches!(comp.style.width, WDim::Auto)
        && comp.style.padding_lengths().left == 0.0
        && comp.style.padding_lengths().right == 0.0
        && comp
            .style
            .border_left_width
            .unwrap_or(comp.style.border_width)
            == 0.0
        && comp
            .style
            .border_right_width
            .unwrap_or(comp.style.border_width)
            == 0.0
    {
        // An empty non-replaced inline with only block-axis decorations has
        // zero inline advance. Taffy's block fallback otherwise stretches its
        // auto width across the containing block and paints full-width top and
        // bottom borders.
        style.size.width = Dimension::length(0.0);
        style.min_size.width = Dimension::length(0.0);
        style.max_size.width = Dimension::length(0.0);
    }
    // CSS resolves every percentage padding side against the containing
    // block's width. Taffy leaves vertical percentages unresolved when that
    // block has an indefinite height, so provide their pixel basis here.
    if let WSpacing::Percent(value) = comp.style.padding.top {
        style.padding.top = LengthPercentage::length(containing_width * value / 100.0);
    }
    if let WSpacing::Percent(value) = comp.style.padding.bottom {
        style.padding.bottom = LengthPercentage::length(containing_width * value / 100.0);
    }
    if let WSpacing::Percent(value) = comp.style.padding.left {
        style.padding.left = LengthPercentage::length(containing_width * value / 100.0);
    }
    if let WSpacing::Percent(value) = comp.style.padding.right {
        style.padding.right = LengthPercentage::length(containing_width * value / 100.0);
    }
    // Percentage margins use the containing block's content width. Taffy's
    // block fallback otherwise resolves them against the parent's border box.
    if let WSpacing::Percent(value) = comp.style.margin.top {
        style.margin.top = LengthPercentageAuto::length(containing_width * value / 100.0);
    }
    if let WSpacing::Percent(value) = comp.style.margin.right {
        style.margin.right = LengthPercentageAuto::length(containing_width * value / 100.0);
    }
    if let WSpacing::Percent(value) = comp.style.margin.bottom {
        style.margin.bottom = LengthPercentageAuto::length(containing_width * value / 100.0);
    }
    if let WSpacing::Percent(value) = comp.style.margin.left {
        style.margin.left = LengthPercentageAuto::length(containing_width * value / 100.0);
    }
    let child_containing_width =
        component_content_width(&comp.style, containing_width, viewport_w, viewport_h);
    if comp.style.float != WFloat::None
        && !matches!(comp.style.width, WDim::Auto)
        && matches!(comp.style.max_width, WDim::Auto)
    {
        // A float with a definite width does not stretch to the available
        // block width. Taffy's block fallback otherwise applies block-axis
        // stretch to the blockified float despite its preferred width.
        style.max_size.width = style.size.width;
    }
    if comp.style.display == WDisplay::Inline
        && matches!(comp.style.position, WPos::Absolute | WPos::Fixed)
    {
        // Absolutely positioned inline boxes are blockified for their used
        // box. Preserve authored dimensions even though the semantic display
        // remains inline for hypothetical static-position calculations.
        style.size = Size {
            width: to_taffy_dim(
                comp.style.width,
                comp.style.font_size,
                viewport_w,
                viewport_h,
            ),
            height: to_taffy_dim(
                comp.style.height,
                comp.style.font_size,
                viewport_w,
                viewport_h,
            ),
        };
    }
    if marked_replaced_element && comp.style.display == WDisplay::Inline {
        style.size = Size {
            width: to_taffy_dim(
                comp.style.width,
                comp.style.font_size,
                viewport_w,
                viewport_h,
            ),
            height: to_taffy_dim(
                comp.style.height,
                comp.style.font_size,
                viewport_w,
                viewport_h,
            ),
        };
    }
    if comp.style.display == WDisplay::Inline
        && matches!(
            parent_display,
            Some(WDisplay::Flex | WDisplay::InlineFlex | WDisplay::Grid)
        )
    {
        // A flex/grid item is blockified. Its authored inline outer display
        // no longer suppresses width/height once it participates as an item.
        style.size = Size {
            width: to_taffy_dim(
                comp.style.width,
                comp.style.font_size,
                viewport_w,
                viewport_h,
            ),
            height: to_taffy_dim(
                comp.style.height,
                comp.style.font_size,
                viewport_w,
                viewport_h,
            ),
        };
    }

    if comp.children.is_empty() {
        if let ComponentKind::Canvas { width, height } = &comp.kind
            && *height > 0
        {
            style.aspect_ratio = Some(*width as f32 / *height as f32);
        }
        let constrained_replaced_size = if let ComponentKind::Image { src } = &comp.kind
            && let Some(ratio) = image_intrinsic_ratio(src)
        {
            // The intrinsic ratio derives an automatic axis; it must not
            // override two authored dimensions in Taffy's block algorithm.
            if matches!(comp.style.width, WDim::Auto)
                != matches!(comp.style.height, WDim::Auto)
            {
                style.aspect_ratio = Some(ratio);
            }
            if !matches!(comp.style.width, WDim::Auto | WDim::Percent(_))
                && matches!(comp.style.height, WDim::Auto)
                && let Some(mut used_width) = comp.style.width.resolve(
                    containing_width,
                    ROOT_FONT_SIZE,
                    comp.style.font_size,
                    viewport_w,
                    viewport_h,
                )
            {
                if let Some(min_width) = comp.style.min_width.resolve(
                    containing_width,
                    ROOT_FONT_SIZE,
                    comp.style.font_size,
                    viewport_w,
                    viewport_h,
                ) {
                    used_width = used_width.max(min_width);
                }
                if let Some(max_width) = comp.style.max_width.resolve(
                    containing_width,
                    ROOT_FONT_SIZE,
                    comp.style.font_size,
                    viewport_w,
                    viewport_h,
                ) {
                    used_width = used_width.min(max_width);
                }
                Some((used_width.max(0.0), (used_width / ratio).max(0.0)))
            } else {
                None
            }
        } else {
            None
        };
        let size = leaf_taffy_size(
            &comp.kind,
            &comp.style,
            &style,
            parent_display,
            containing_width,
            viewport_w,
            viewport_h,
        );
        let inline_control_in_block =
            matches!(
                comp.style.display,
                WDisplay::InlineBlock | WDisplay::InlineFlex
            ) && !matches!(parent_display, Some(WDisplay::Flex | WDisplay::Grid));
        let (min_w, size_w) = if passive_inline_edges
            && !matches!(comp.kind, ComponentKind::Text { .. })
        {
            // Width does not apply to a non-replaced inline box. An empty
            // inline leaf therefore has zero content advance; its authored
            // horizontal padding and borders remain on the Taffy style.
            (Dimension::length(0.0), Dimension::length(0.0))
        } else if matches!(comp.style.width, WDim::Auto) {
            match &comp.kind {
                ComponentKind::Text { content } => {
                    let nowrap = matches!(
                        comp.style.white_space,
                        WWhiteSpace::NoWrap | WWhiteSpace::Pre
                    );
                    // A lowered DOM text node commonly carries `display:inline`
                    // from its `<span>` host. Browser inline text still wraps to
                    // the containing block; treating it like inline-block locks
                    // the leaf to its intrinsic width and lets CJK text escape
                    // message bubbles. Only an actual inline-block shrink-fits.
                    //
                    // Likewise, `overflow:hidden` removes the automatic
                    // min-content size of a flex item in browsers, allowing a
                    // nowrap title to contract and be clipped by its own box.
                    let inline_text_in_block = comp.style.display == WDisplay::Inline
                        && matches!(parent_display, Some(WDisplay::Block | WDisplay::Grid));
                    let absolute_auto_shrink_to_fit = matches!(
                        comp.style.position,
                        WPos::Absolute | WPos::Fixed
                    ) && matches!(comp.style.left, WDim::Auto)
                        && matches!(comp.style.right, WDim::Auto);
                    let shrink_to_fit = absolute_auto_shrink_to_fit
                        || comp.style.float != WFloat::None
                        || inline_text_in_block
                        || matches!(
                            comp.style.display,
                            WDisplay::InlineBlock | WDisplay::InlineFlex
                        );
                    let clips_overflow = matches!(
                        comp.style.resolved_overflow_x(),
                        WOverflow::Hidden | WOverflow::Scroll | WOverflow::Auto
                    ) || matches!(
                        comp.style.resolved_overflow_y(),
                        WOverflow::Hidden | WOverflow::Scroll | WOverflow::Auto
                    );
                    if shrink_to_fit
                        || (nowrap
                            && !clips_overflow
                            && !matches!(
                                comp.style.display,
                                WDisplay::Block
                                    | WDisplay::Table
                                    | WDisplay::TableRowGroup
                                    | WDisplay::TableHeaderGroup
                                    | WDisplay::TableFooterGroup
                                    | WDisplay::TableCell
                                    | WDisplay::TableCaption
                                    | WDisplay::ListItem
                            ))
                    {
                        let mut w = text_intrinsic_size_for_taffy(content, &comp.style).0;
                        if comp.style.display != WDisplay::Inline {
                            if let Some(max_width) = comp.style.max_width.resolve(
                                containing_width,
                                ROOT_FONT_SIZE,
                                comp.style.font_size,
                                viewport_w,
                                viewport_h,
                            ) {
                                w = w.min(max_width);
                            }
                            if let Some(min_width) = comp.style.min_width.resolve(
                                containing_width,
                                ROOT_FONT_SIZE,
                                comp.style.font_size,
                                viewport_w,
                                viewport_h,
                            ) {
                                w = w.max(min_width);
                            }
                        }
                        let dim = Dimension::length(w);
                        if inline_text_in_block && !nowrap {
                            (Dimension::length(0.0), dim)
                        } else {
                            (dim, dim)
                        }
                    } else if text_uses_intrinsic_cross_size(
                        &comp.style,
                        parent_direction,
                        parent_display,
                        parent_align_items,
                    ) {
                        let mut w = text_intrinsic_size_for_taffy(content, &comp.style).0;
                        if let WDim::Px(mw) = comp.style.min_width {
                            w = w.max(mw);
                        }
                        let min_width = if matches!(
                            comp.style.word_break,
                            WWordBreak::BreakAll | WWordBreak::BreakWord
                        ) {
                            let padding = comp.style.padding_lengths();
                            comp.style.font_size
                                + if comp.style.box_sizing == WBoxSizing::BorderBox {
                                    padding.left + padding.right
                                } else {
                                    0.0
                                }
                        } else {
                            w
                        };
                        (Dimension::length(min_width), Dimension::length(w))
                    } else if matches!(parent_display, Some(WDisplay::Block | WDisplay::Grid))
                        || matches!(parent_direction, Some(WDir::Column | WDir::ColumnReverse))
                    {
                        let min_width = match comp.style.min_width {
                            WDim::Px(mw) => Dimension::length(mw),
                            _ => Dimension::length(0.0),
                        };
                        (min_width, Dimension::auto())
                    } else {
                        let mut w = text_intrinsic_size_for_taffy(content, &comp.style).0;
                        if let WDim::Px(mw) = comp.style.min_width {
                            w = w.max(mw);
                        }
                        (Dimension::length(w), Dimension::auto())
                    }
                }
                ComponentKind::Button { label } => {
                    let w = button_intrinsic_size(label, &comp.style).0;
                    let size = if inline_control_in_block {
                        Dimension::length(w)
                    } else {
                        Dimension::auto()
                    };
                    (Dimension::length(w), size)
                }
                ComponentKind::TextInput { .. } if inline_control_in_block => {
                    let w = leaf_intrinsic_size(&comp.kind, &comp.style).0;
                    (Dimension::length(w), Dimension::length(w))
                }
                ComponentKind::Image { src } => {
                    let intrinsic_containing_width = if comp.style.display == WDisplay::Block {
                        let padding = comp.style.padding_lengths();
                        let borders = comp
                            .style
                            .border_left_width
                            .unwrap_or(comp.style.border_width)
                            + comp
                                .style
                                .border_right_width
                                .unwrap_or(comp.style.border_width);
                        (containing_width - padding.left - padding.right - borders).max(0.0)
                    } else {
                        containing_width
                    };
                    let w = leaf_intrinsic_size_with_containing(
                        &comp.kind,
                        &comp.style,
                        Some(intrinsic_containing_width),
                    )
                    .0;
                    let size = if !matches!(comp.style.height, WDim::Auto)
                        && image_intrinsic_ratio(src).is_some()
                    {
                        // The specified cross size may be a percentage. Keep
                        // the auto axis unresolved until Taffy knows its used
                        // value, then apply the intrinsic aspect ratio.
                        Dimension::auto()
                    } else {
                        Dimension::length(w)
                    };
                    (Dimension::auto(), size)
                }
                _ => (Dimension::auto(), size.width),
            }
        } else {
            (Dimension::auto(), size.width)
        };
        let min_w = if comp.style.display == WDisplay::Inline
            || matches!(comp.style.min_width, WDim::Auto)
        {
            min_w
        } else {
            to_taffy_dim(
                comp.style.min_width,
                comp.style.font_size,
                viewport_w,
                viewport_h,
            )
        };
        let intrinsic_min_h = if matches!(comp.style.height, WDim::Auto)
            && matches!(comp.style.max_height, WDim::Auto)
        {
            match &comp.kind {
                ComponentKind::Text { content } => Dimension::length(
                    text_intrinsic_size_in_parent_for_taffy(content, &comp.style, parent_display).1,
                ),
                ComponentKind::Button { label } => {
                    Dimension::length(button_intrinsic_size(label, &comp.style).1)
                }
                // The intrinsic height is already the preferred leaf size.
                // Keeping it as an automatic minimum as well would override
                // an authored `max-height` on a replaced element.
                ComponentKind::Image { .. } => Dimension::auto(),
                _ => Dimension::auto(),
            }
        } else {
            Dimension::auto()
        };
        let min_h = match comp.style.min_height {
            WDim::Auto => intrinsic_min_h,
            _ => to_taffy_dim(
                comp.style.min_height,
                comp.style.font_size,
                viewport_w,
                viewport_h,
            ),
        };

        let mut leaf_style = Style {
            size: Size {
                width: size_w,
                height: size.height,
            },
            min_size: Size {
                width: min_w,
                height: min_h,
            },
            ..style
        };
        if let Some((width, height)) = constrained_replaced_size {
            leaf_style.size.width = Dimension::length(width);
            leaf_style.size.height = Dimension::length(height);
        }
        if matches!(comp.kind, ComponentKind::Canvas { .. })
            && matches!(parent_display, Some(WDisplay::InlineBlock))
            && !matches!(comp.style.height, WDim::Auto)
        {
            // The column flexbox is only an internal model for anonymous
            // block generation. A replaced element's percentage height is
            // not a shrinkable flex main size in the CSS block formatting
            // context it represents.
            leaf_style.flex_shrink = 0.0;
        }
        if matches!(comp.kind, ComponentKind::Text { .. })
            && matches!(
                comp.style.word_break,
                WWordBreak::BreakAll | WWordBreak::BreakWord
            )
            && matches!(comp.style.max_width, WDim::Auto)
        {
            // Breakable shrink-to-fit text may use its max-content width, but
            // the used width is still capped by the containing block.
            leaf_style.max_size.width = Dimension::percent(1.0);
            // This internal cap applies to the text leaf's margin box. Using
            // content-box here would add horizontal padding after the 100%
            // cap and overflow the grid track.
            leaf_style.box_sizing = BoxSizing::BorderBox;
        }
        tree.new_leaf_with_context(leaf_style, my_idx)
    } else {
        let fixed_table_tracks = fixed_table_track_widths(comp, Some(containing_width));
        if owns_table_layout
            && comp.style.table_layout_fixed
            && !comp.style.border_collapse
            && !is_html_table_element(&comp.style)
            && let Some(tracks) = fixed_table_tracks.as_deref()
        {
            let padding = comp.style.padding_lengths();
            let borders = comp
                .style
                .border_left_width
                .unwrap_or(comp.style.border_width)
                + comp
                    .style
                    .border_right_width
                    .unwrap_or(comp.style.border_width);
            let spacing = effective_table_border_spacing(&comp.style).0;
            let minimum_grid = tracks.iter().sum::<f32>() + spacing * (tracks.len() + 1) as f32;
            let minimum_outer = minimum_grid + padding.left + padding.right + borders;
            let declared_outer = constrained_specified_width_with_basis(
                &comp.style,
                Some(containing_width),
            )
                .unwrap_or(0.0)
                + padding.left
                + padding.right
                + borders;
            style.size.width = Dimension::length(minimum_outer.max(declared_outer));
        }
        let auto_table_tracks = (owns_table_layout && fixed_table_tracks.is_none())
            .then(|| auto_table_track_widths(comp, Some(containing_width)));
        let owned_table_tracks = fixed_table_tracks.or(auto_table_tracks);
        let owned_collapsed_single_track =
            (matches!(comp.style.display, WDisplay::Table | WDisplay::InlineTable)
                && matches!(comp.style.width, WDim::Auto))
            .then(|| collapsed_single_track_metrics(comp))
            .flatten();
        let active_collapsed_single_track =
            owned_collapsed_single_track.or(inherited_collapsed_single_track);
        let active_table_tracks = owned_table_tracks
            .as_deref()
            .filter(|tracks| !tracks.is_empty())
            .or(inherited_table_tracks);
        let mut next_table_column = 0;
        let child_table_columns = comp
            .children
            .iter()
            .map(|child| {
                if comp.style.display == WDisplay::TableRow
                    && child.style.display == WDisplay::TableCell
                {
                    let column = next_table_column;
                    next_table_column += table_cell_column_span(&child.style);
                    Some(column)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        let mut child_nodes: Vec<(i32, usize, NodeId)> =
            comp.children
                .iter()
                .enumerate()
                .map(|(source_index, c)| {
                    let node = build_taffy_tree(
                        tree,
                        c,
                        idx,
                        Some(comp.style.flex_direction),
                        Some(comp.style.display),
                        Some(comp.style.align_items),
                        Some(comp.style.font_size),
                        viewport_w,
                        viewport_h,
                        child_containing_width,
                        child_quirks_height_basis,
                        own_definite_height_basis,
                        active_table_tracks,
                        active_fixed_table_layout,
                        active_collapsed_single_track,
                        child_table_columns[source_index],
                        matches!(
                            comp.style.display,
                            WDisplay::Table
                                | WDisplay::InlineTable
                                | WDisplay::TableRowGroup
                                | WDisplay::TableHeaderGroup
                                | WDisplay::TableFooterGroup
                        ) && (!matches!(comp.style.height, WDim::Auto)
                            || !matches!(comp.style.min_height, WDim::Auto)),
                        active_border_spacing,
                    )?;
                    if float_only_auto_block && c.style.float != WFloat::None {
                        // Floats do not contribute to their ordinary block
                        // parent's auto height. Treat the Taffy fallback as
                        // out-of-flow here; the float projection retains its
                        // static position and margin box after layout.
                        let mut child_style = tree.style(node)?.clone();
                        child_style.position = Position::Absolute;
                        tree.set_style(node, child_style)?;
                    }
                    if c.style.display == WDisplay::TableCaption
                        && matches!(comp.style.display, WDisplay::Table | WDisplay::InlineTable)
                    {
                        let (spacing_x, spacing_y) = effective_table_border_spacing(&comp.style);
                        let mut child_style = tree.style(node)?.clone();
                        child_style.margin.left = LengthPercentageAuto::length(
                            resolve_spacing_for_layout(
                                c.style.margin.left,
                                containing_width,
                                c.style.font_size,
                                viewport_w,
                                viewport_h,
                            ) - spacing_x,
                        );
                        child_style.margin.top = LengthPercentageAuto::length(
                            resolve_spacing_for_layout(
                                c.style.margin.top,
                                containing_width,
                                c.style.font_size,
                                viewport_w,
                                viewport_h,
                            ) - spacing_y,
                        );
                        tree.set_style(node, child_style)?;
                    }
                    if let Some((float_index, guard)) = leading_float_margin_guard
                        && source_index == float_index
                    {
                        let mut child_style = tree.style(node)?.clone();
                        let margin = (resolve_spacing_for_layout(
                            c.style.margin.top,
                            containing_width,
                            c.style.font_size,
                            viewport_w,
                            viewport_h,
                        ) - guard)
                            .max(0.0);
                        child_style.margin.top = LengthPercentageAuto::length(margin);
                        tree.set_style(node, child_style)?;
                    }
                    if matches!(comp.style.display, WDisplay::Flex | WDisplay::InlineFlex)
                        && matches!(comp.style.flex_wrap, WWrap::Wrap | WWrap::WrapReverse)
                        && !comp.children.iter().any(|child| {
                            matches!(child.style.position, WPos::Absolute | WPos::Fixed)
                        })
                        && matches!(
                            &c.kind,
                            ComponentKind::Text { content } if content == "\u{2028}"
                        )
                    {
                        // A forced line break has no painted advance, but it
                        // terminates the current flex-backed inline line. A full
                        // flex basis with zero cross size creates that boundary
                        // without adding a phantom third line; the inherited
                        // line-height strut is projected after layout.
                        let mut child_style = tree.style(node)?.clone();
                        child_style.flex_basis = Dimension::percent(1.0);
                        child_style.size.width = Dimension::length(0.0);
                        child_style.size.height = Dimension::length(0.0);
                        child_style.min_size.height = Dimension::length(0.0);
                        tree.set_style(node, child_style)?;
                    }
                    if comp.style.display == WDisplay::InlineBlock
                        && matches!(comp.style.width, WDim::Auto)
                        && c.style.display == WDisplay::Inline
                        && matches!(c.style.width, WDim::Auto)
                        && matches!(c.kind, ComponentKind::Text { .. })
                    {
                        let mut child_style = tree.style(node)?.clone();
                        child_style.size.width = Dimension::percent(1.0);
                        child_style.min_size.width = Dimension::length(0.0);
                        child_style.flex_shrink = 1.0;
                        tree.set_style(node, child_style)?;
                    }
                    if mixed_inline_block_flex_fallback
                        && !matches!(c.style.position, WPos::Absolute | WPos::Fixed)
                        && empty_inline_establishes_visible_line(c)
                    {
                        let mut child_style = tree.style(node)?.clone();
                        child_style.min_size.height = Dimension::length(
                            c.style.font_size * c.style.line_height,
                        );
                        child_style.flex_shrink = 0.0;
                        tree.set_style(node, child_style)?;
                    }
                    if mixed_inline_block_flex_fallback
                        && !matches!(c.style.position, WPos::Absolute | WPos::Fixed)
                        && c.style.float == WFloat::None
                        && matches!(
                            c.style.display,
                            WDisplay::Block
                                | WDisplay::Flex
                                | WDisplay::Grid
                                | WDisplay::ListItem
                                | WDisplay::Table
                        )
                    {
                        let mut child_style = tree.style(node)?.clone();
                        child_style.size.width = Dimension::percent(1.0);
                        child_style.flex_basis = Dimension::percent(1.0);
                        child_style.flex_grow = 0.0;
                        child_style.flex_shrink = 0.0;
                        tree.set_style(node, child_style)?;
                    }
                    if comp.style.display == WDisplay::TableCell
                        && source_index == 0
                        && collapsed_span_leading_width > 0.0
                    {
                        let mut child_style = tree.style(node)?.clone();
                        child_style.margin.left =
                            LengthPercentageAuto::length(-collapsed_span_leading_width);
                        tree.set_style(node, child_style)?;
                    }
                    let mut collapsed_overlap = None;
                    if comp.style.border_collapse
                        && comp.style.display == WDisplay::TableRow
                        && c.style.display == WDisplay::TableCell
                        && let Some(next) = comp.children[source_index + 1..]
                            .iter()
                            .find(|next| next.style.display == WDisplay::TableCell)
                    {
                        let column = child_table_columns[source_index].unwrap_or(source_index);
                        let next_column = column + table_cell_column_span(&c.style);
                        let collapsed_column = active_table_tracks
                            .and_then(|tracks| tracks.get(next_column.saturating_sub(1)))
                            .is_some_and(|width| width.is_sign_negative())
                            || active_table_tracks
                                .and_then(|tracks| tracks.get(next_column))
                                .is_some_and(|width| width.is_sign_negative());
                        collapsed_overlap = Some((
                            true,
                            if collapsed_column {
                                0.0
                            } else {
                                table_cell_edge_width(c, 1).max(table_cell_edge_width(next, 3))
                            },
                        ));
                    } else if comp.style.border_collapse
                        && (matches!(
                            comp.style.display,
                            WDisplay::Table
                                | WDisplay::InlineTable
                                | WDisplay::TableRowGroup
                                | WDisplay::TableHeaderGroup
                                | WDisplay::TableFooterGroup
                        ) || (comp.style.display == WDisplay::Block
                            && comp.children.iter().any(|child| {
                                matches!(
                                    child.style.display,
                                    WDisplay::TableRowGroup
                                        | WDisplay::TableHeaderGroup
                                        | WDisplay::TableFooterGroup
                                )
                            })))
                        && matches!(
                            c.style.display,
                            WDisplay::TableRow
                                | WDisplay::TableRowGroup
                                | WDisplay::TableHeaderGroup
                                | WDisplay::TableFooterGroup
                        )
                        && let Some(next) = comp.children[source_index + 1..].iter().find(|next| {
                            matches!(
                                next.style.display,
                                WDisplay::TableRow
                                    | WDisplay::TableRowGroup
                                    | WDisplay::TableHeaderGroup
                                    | WDisplay::TableFooterGroup
                            )
                        })
                    {
                        let boundary_width = collapsed_table_part_block_edge_width(c, 2)
                            .max(collapsed_table_part_block_edge_width(next, 0));
                        collapsed_overlap = Some((
                            false,
                            collapsed_empty_row_overlap(c, boundary_width, viewport_w, viewport_h)
                                .unwrap_or(boundary_width),
                        ));
                    }
                    if let Some((inline, overlap)) = collapsed_overlap
                        && overlap > 0.0
                    {
                        let mut child_style = tree.style(node)?.clone();
                        if inline {
                            if comp.style.direction == w3cos_std::style::TextDirection::Rtl {
                                child_style.margin.left = LengthPercentageAuto::length(-overlap);
                            } else {
                                child_style.margin.right = LengthPercentageAuto::length(-overlap);
                            }
                        } else {
                            child_style.margin.bottom = LengthPercentageAuto::length(-overlap);
                        }
                        tree.set_style(node, child_style)?;
                    }
                    Ok((c.style.order, source_index, node))
                })
                .collect::<Result<_, _>>()?;
        child_nodes.sort_by_key(|(order, source_index, _)| (*order, *source_index));
        let rescue_overwide_first_pair =
            if comp.style.flex_wrap == WWrap::Wrap && child_nodes.len() >= 2 {
                let first = &comp.children[child_nodes[0].1];
                let second = &comp.children[child_nodes[1].1];
                let anonymous_line_item = |component: &Component| {
                    matches!(component.kind, ComponentKind::Box)
                        && component.children.len() == 1
                        && component.style.display == WDisplay::InlineFlex
                        && matches!(component.style.min_height, WDim::Px(_))
                };
                let first_width = component_max_content_width(first);
                let second_width = component_max_content_width(second);
                let second_strictly_reduces_line = second.children.first().is_some_and(|inner| {
                    if inner
                        .style
                        .font_family
                        .as_deref()
                        .is_some_and(|family| family.eq_ignore_ascii_case("ahem"))
                        && let ComponentKind::Text { content } = &inner.kind
                        && let WSpacing::Em(left) = inner.style.margin.left
                    {
                        // Ahem defines every character cell as exactly 1ch. Keep
                        // the zero-outer-width boundary distinct from a strictly
                        // negative following box; only the latter can rescue an
                        // overwide first item without a line break.
                        -left > content.chars().count() as f32
                    } else {
                        second_width < -0.01
                    }
                });
                let line_width = if matches!(comp.style.width, WDim::Auto) {
                    shrink_to_fit_used_width(comp)
                } else {
                    component_content_width(&comp.style, containing_width, viewport_w, viewport_h)
                };
                (anonymous_line_item(first)
                    && anonymous_line_item(second)
                    && second_strictly_reduces_line
                    && first_width > line_width + 0.01
                    && first_width + second_width <= line_width + 0.01)
                    .then_some((first_width + second_width).max(0.0))
            } else {
                None
            };
        let mut child_nodes: Vec<NodeId> =
            child_nodes.into_iter().map(|(_, _, node)| node).collect();
        if let Some(group_width) = rescue_overwide_first_pair {
            // Inline layout does not break before the first box on an empty
            // line. A following negative-margin box can therefore pull that
            // overwide first box back within the available line. Flexbox's
            // greedy wrapping closes the line too early, so keep precisely
            // that first pair together in an anonymous, context-free row.
            // Later overflow opportunities remain independent, matching CSS
            // inline line breaking (a third negative box cannot rescue an
            // overflow that already occurred between the first two boxes).
            let grouped = [child_nodes[0], child_nodes[1]];
            let group_style = Style {
                display: taffy::Display::Flex,
                flex_direction: FlexDirection::Row,
                align_items: Some(AlignItems::FlexStart),
                flex_shrink: 0.0,
                size: Size {
                    width: Dimension::length(group_width),
                    height: Dimension::auto(),
                },
                min_size: Size {
                    width: Dimension::length(group_width),
                    height: Dimension::auto(),
                },
                max_size: Size {
                    width: Dimension::length(group_width),
                    height: Dimension::auto(),
                },
                ..Style::default()
            };
            let group = tree.new_with_children(group_style, &grouped)?;
            child_nodes.splice(0..2, [group]);
        }
        let absolute_auto_shrink_to_fit = matches!(
            comp.style.position,
            WPos::Absolute | WPos::Fixed
        ) && matches!(comp.style.left, WDim::Auto)
            && matches!(comp.style.right, WDim::Auto);
        if matches!(comp.style.width, WDim::Auto)
            && (absolute_auto_shrink_to_fit
                || ((comp.style.float != WFloat::None
                    || matches!(
                        comp.style.display,
                        WDisplay::Inline
                            | WDisplay::InlineBlock
                            | WDisplay::InlineFlex
                            | WDisplay::InlineTable
                            | WDisplay::Table
                    ))
                    && (matches!(parent_display, Some(WDisplay::Block | WDisplay::Grid))
                        || (matches!(parent_display, Some(WDisplay::Flex))
                            && matches!(
                                comp.style.display,
                                WDisplay::InlineBlock
                                    | WDisplay::InlineFlex
                                    | WDisplay::InlineTable
                            )))))
        {
            let border_box_width = shrink_to_fit_used_width_with_available(
                comp,
                containing_width,
            );
            // Convert just the shrink-fit width to the existing box sizing.
            // Changing box-sizing here would also reinterpret authored heights.
            let inner_width = if style.box_sizing == BoxSizing::ContentBox {
                resolve_spacing_for_layout(comp.style.padding.left, containing_width,
                    comp.style.font_size, viewport_w, viewport_h)
                    + resolve_spacing_for_layout(comp.style.padding.right, containing_width,
                        comp.style.font_size, viewport_w, viewport_h)
                    + comp.style.border_left_width.unwrap_or(comp.style.border_width)
                    + comp.style.border_right_width.unwrap_or(comp.style.border_width)
            } else {
                0.0
            };
            style.size.width = Dimension::length((border_box_width - inner_width).max(0.0));
            if comp.style.display == WDisplay::Table {
                // CSS auto table layout shrink-wraps up to the available
                // containing-block width. An unconstrained max-content width
                // makes a wide table escape a viewport-sized block instead
                // of distributing its columns inside that block.
                style.max_size.width = Dimension::percent(1.0);
            }
        }
        let node = tree.new_with_children(style, &child_nodes)?;
        tree.set_node_context(node, Some(my_idx))?;
        Ok(node)
    }
}

fn text_uses_intrinsic_cross_size(
    style: &w3cos_std::style::Style,
    parent_direction: Option<WDir>,
    parent_display: Option<WDisplay>,
    parent_align_items: Option<WAlign>,
) -> bool {
    if !matches!(parent_direction, Some(WDir::Column | WDir::ColumnReverse))
        || !matches!(parent_display, Some(WDisplay::Flex))
    {
        return false;
    }

    match style.align_self {
        WAlignSelf::Stretch => false,
        WAlignSelf::Auto => !matches!(parent_align_items, Some(WAlign::Stretch)),
        WAlignSelf::FlexStart | WAlignSelf::FlexEnd | WAlignSelf::Center | WAlignSelf::Baseline => {
            true
        }
    }
}

fn patch_taffy_display(
    tree: &mut TaffyTree<usize>,
    node: NodeId,
    flat: &[FlatNodeInfo<'_>],
) -> Result<(), taffy::TaffyError> {
    if let Some(idx) = tree.get_node_context(node).copied() {
        if idx < flat.len() {
            let mut style = tree.style(node)?.clone();
            let new_display = to_taffy_display(flat[idx].style.display);
            if style.display != new_display {
                style.display = new_display;
                tree.set_style(node, style)?;
            }
        }
    }
    for child in tree.children(node)? {
        patch_taffy_display(tree, child, flat)?;
    }
    Ok(())
}

/// After first layout pass, set Text leaf heights from wrapped line count at assigned width.
/// Returns true when any leaf style changed and a second Taffy pass is required.
fn update_text_leaf_heights(
    tree: &mut TaffyTree<usize>,
    node: NodeId,
    flat: &[FlatNodeInfo<'_>],
    parent_display: Option<WDisplay>,
) -> Result<bool, taffy::TaffyError> {
    let layout = tree.layout(node)?;
    let node_width = layout.size.width;
    let mut dirty = false;

    if let Some(idx) = tree.get_node_context(node).copied() {
        if idx < flat.len() {
            if let ComponentKind::Text { content } = flat[idx].kind {
                let style = flat[idx].style;
                if matches!(style.height, WDim::Auto) {
                    let mut h = wrapped_text_height(content, node_width, style);
                    if let Some(browser_height) =
                        browser_normal_cjk_height(content, style, parent_display)
                    {
                        h = h.max(browser_height);
                    }
                    if style.box_sizing == WBoxSizing::ContentBox {
                        let padding = style.padding_lengths();
                        h = (h - padding.top - padding.bottom).max(0.0);
                    }
                    let mut taffy_style = tree.style(node)?.clone();
                    let measured_height = Dimension::length(h);
                    let measured_min_height = if matches!(style.min_height, WDim::Auto)
                        && matches!(style.max_height, WDim::Auto)
                    {
                        measured_height
                    } else {
                        taffy_style.min_size.height
                    };
                    if taffy_style.min_size.height != measured_min_height
                        || taffy_style.size.height != measured_height
                    {
                        taffy_style.min_size.height = measured_min_height;
                        taffy_style.size.height = measured_height;
                        tree.set_style(node, taffy_style)?;
                        dirty = true;
                    }
                }
            }
        }
    }

    let current_display = tree
        .get_node_context(node)
        .copied()
        .and_then(|idx| flat.get(idx))
        .map(|info| info.style.display);
    for child in tree.children(node)? {
        dirty |= update_text_leaf_heights(tree, child, flat, current_display)?;
    }
    Ok(dirty)
}

fn to_taffy_display(d: WDisplay) -> taffy::Display {
    match d {
        WDisplay::Flex
        | WDisplay::Inline
        | WDisplay::InlineBlock
        | WDisplay::InlineFlex
        | WDisplay::InlineTable
        | WDisplay::TableRow
        | WDisplay::Contents => taffy::Display::Flex,
        WDisplay::Grid => taffy::Display::Grid,
        WDisplay::Block
        | WDisplay::Table
        | WDisplay::TableRowGroup
        | WDisplay::TableHeaderGroup
        | WDisplay::TableFooterGroup
        | WDisplay::TableColumnGroup
        | WDisplay::TableColumn
        | WDisplay::TableCell
        | WDisplay::TableCaption
        | WDisplay::ListItem => taffy::Display::Block,
        WDisplay::None => taffy::Display::None,
    }
}

// ---------------------------------------------------------------------------
// Fast layout collection using pre-flattened array (O(1) lookups)
// Also propagates scroll container top-down (eliminates O(n*depth) parent walk)
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn collect_layouts_fast(
    flat: &[FlatNodeInfo],
    tree: &TaffyTree<usize>,
    node: NodeId,
    parent_x: f32,
    parent_y: f32,
    viewport_w: f32,
    viewport_h: f32,
    absolute_containing_block: LayoutRect,
    relative_containing_block: LayoutRect,
    relative_containing_block_height_definite: bool,
    current_scroll_container: Option<usize>,
    out: &mut Vec<(LayoutRect, usize)>,
    fixed_out: &mut Vec<(LayoutRect, usize)>,
    scrollable: &mut Vec<(usize, LayoutRect, ScrollExtent)>,
    clip_only: &mut Vec<(usize, LayoutRect)>,
    scroll_ancestor: &mut [Option<usize>],
) {
    let layout = tree.layout(node).unwrap();
    let x = parent_x + layout.location.x;
    let y = parent_y + layout.location.y;
    let mut rect = LayoutRect {
        x,
        y,
        width: layout.size.width,
        height: layout.size.height,
    };
    let mut passive_inline_top_edge = 0.0;
    let mut passive_inline_half_leading = 0.0;

    let mut new_scroll_container = current_scroll_container;
    let mut descendant_containing_block = absolute_containing_block;

    if let Some(&ctx) = tree.get_node_context(node) {
        if ctx < flat.len() {
            if !is_node_visible(flat, ctx) {
                return;
            }
            let info = &flat[ctx];

            let marked_replaced_element = info
                .style
                .custom_properties
                .as_ref()
                .is_some_and(|properties| {
                    properties.contains_key("--w3cos-internal-replaced-element")
                });
            if !marked_replaced_element
                && matches!(
                info.kind,
                ComponentKind::Row
                    | ComponentKind::Column
                    | ComponentKind::Box
                    | ComponentKind::Text { .. }
            )
                && info.style.display == WDisplay::Inline
                && !matches!(info.style.position, WPos::Absolute | WPos::Fixed)
            {
                let padding = info.style.padding_lengths();
                let line_height = info.style.font_size * info.style.line_height;
                let half_leading = (line_height - info.style.font_size) * 0.5;
                passive_inline_half_leading = half_leading;
                passive_inline_top_edge = padding.top
                    + info
                        .style
                        .border_top_width
                        .unwrap_or(info.style.border_width);
                rect.y += half_leading - passive_inline_top_edge;
                rect.height = info.style.font_size
                    + padding.top
                    + padding.bottom
                    + info
                        .style
                        .border_top_width
                        .unwrap_or(info.style.border_width)
                    + info
                        .style
                        .border_bottom_width
                        .unwrap_or(info.style.border_width);
            }

            let effective_scroll_container = if matches!(info.style.position, WPos::Absolute) {
                let mut ancestor = info.parent;
                let mut positioned_ancestor = None;
                while let Some(index) = ancestor {
                    if !matches!(flat[index].style.position, WPos::Static) {
                        positioned_ancestor = Some(index);
                        break;
                    }
                    ancestor = flat[index].parent;
                }
                current_scroll_container.and_then(|clip| {
                    let containing_block_is_inside_clip =
                        positioned_ancestor.is_some_and(|owner| {
                            let mut candidate = Some(owner);
                            while let Some(index) = candidate {
                                if index == clip {
                                    return true;
                                }
                                candidate = flat[index].parent;
                            }
                            false
                        });
                    if containing_block_is_inside_clip {
                        Some(clip)
                    } else {
                        scroll_ancestor[clip]
                    }
                })
            } else {
                current_scroll_container
            };
            scroll_ancestor[ctx] = effective_scroll_container;
            new_scroll_container = effective_scroll_container;

            if matches!(info.style.position, WPos::Fixed) {
                let replaced = matches!(
                    info.kind,
                    ComponentKind::Image { .. } | ComponentKind::Canvas { .. }
                        | ComponentKind::SvgDocument { .. }
                ) || marked_replaced_element;
                rect = compute_fixed_rect(info.style, viewport_w, viewport_h, rect, replaced);
                fixed_out.push((rect, ctx));
            } else {
                if matches!(info.style.position, WPos::Absolute) {
                    let mut ancestor = info.parent;
                    let mut containing_direction = info.style.direction;
                    while let Some(index) = ancestor {
                        containing_direction = flat[index].style.direction;
                        if flat[index].style.position != WPos::Static {
                            break;
                        }
                        ancestor = flat[index].parent;
                    }
                    let fallback = inline_absolute_static_rect(
                        flat,
                        tree,
                        node,
                        relative_containing_block,
                        rect,
                    )
                    .unwrap_or(rect);
                    rect = compute_absolute_rect(
                        info.style,
                        absolute_containing_block,
                        fallback,
                        viewport_w,
                        viewport_h,
                        containing_direction,
                        matches!(
                            info.kind,
                            ComponentKind::Image { .. } | ComponentKind::Canvas { .. }
                                | ComponentKind::SvgDocument { .. }
                        ) || marked_replaced_element,
                    );
                } else if matches!(info.style.position, WPos::Relative) {
                    rect = compute_relative_percentage_rect(
                        info.style,
                        relative_containing_block,
                        relative_containing_block_height_definite,
                        rect,
                    );
                }
                if info.style.float == WFloat::Left
                    && info
                        .style
                        .custom_properties
                        .as_ref()
                        .and_then(|properties| {
                            properties.get("--w3cos-internal-left-float-after-inline")
                        })
                        .is_some_and(|value| value == "1")
                {
                    rect.x = relative_containing_block.x;
                    if let Some(parent_style) = info.parent.and_then(|parent| flat.get(parent)) {
                        let line_height =
                            parent_style.style.font_size * parent_style.style.line_height;
                        let margin_top = resolve_spacing_for_layout(
                            info.style.margin.top,
                            relative_containing_block.width,
                            info.style.font_size,
                            viewport_w,
                            viewport_h,
                        );
                        rect.y = rect.y.max(
                            relative_containing_block.y + line_height.max(0.0) + margin_top,
                        );
                    }
                }
                out.push((rect, ctx));
            }

            if !matches!(info.style.position, WPos::Static) {
                descendant_containing_block =
                    positioned_descendant_containing_block(flat, tree, node, rect);
            }

            let overflow_x = info.style.resolved_overflow_x();
            let overflow_y = info.style.resolved_overflow_y();
            let scrolls_x = matches!(overflow_x, WOverflow::Scroll | WOverflow::Auto);
            let scrolls_y = matches!(overflow_y, WOverflow::Scroll | WOverflow::Auto);
            let clips_x = matches!(overflow_x, WOverflow::Hidden);
            let clips_y = matches!(overflow_y, WOverflow::Hidden);
            if scrolls_x || scrolls_y || clips_x || clips_y {
                let max_x = if scrolls_x || clips_x {
                    layout.scroll_width().max(0.0)
                } else {
                    0.0
                };
                let max_y = if scrolls_y || clips_y {
                    match info.kind {
                        ComponentKind::VirtualList { total_extent, .. } => {
                            (*total_extent - rect.height).max(0.0)
                        }
                        _ => layout.scroll_height().max(0.0),
                    }
                } else {
                    0.0
                };
                if max_x > 0.0 || max_y > 0.0 || clips_x || clips_y {
                    scrollable.push((ctx, rect, ScrollExtent { max_x, max_y }));
                } else {
                    clip_only.push((ctx, rect));
                }
                new_scroll_container = Some(ctx);
            }
        }
    }

    let child_relative_containing_block = LayoutRect {
        x: rect.x + layout.border.left + layout.padding.left,
        y: rect.y + passive_inline_top_edge + layout.border.top + layout.padding.top,
        width: layout.content_box_width(),
        height: layout.content_box_height(),
    };
    let child_relative_containing_block_height_definite = tree
        .get_node_context(node)
        .and_then(|index| flat.get(*index))
        .is_some_and(|info| match info.style.height {
            WDim::Auto => {
                matches!(info.style.position, WPos::Absolute | WPos::Fixed)
                    && !matches!(info.style.top, WDim::Auto)
                    && !matches!(info.style.bottom, WDim::Auto)
                    && (matches!(info.style.position, WPos::Fixed)
                        || relative_containing_block_height_definite)
            }
            WDim::Percent(_) => relative_containing_block_height_definite,
            WDim::Px(_)
            | WDim::Rem(_)
            | WDim::Em(_)
            | WDim::Ch(_)
            | WDim::Vw(_)
            | WDim::Vh(_) => true,
        });

    for &child in tree.children(node).unwrap().iter() {
        collect_layouts_fast(
            flat,
            tree,
            child,
            rect.x,
            rect.y + passive_inline_top_edge - passive_inline_half_leading,
            viewport_w,
            viewport_h,
            descendant_containing_block,
            child_relative_containing_block,
            child_relative_containing_block_height_definite,
            new_scroll_container,
            out,
            fixed_out,
            scrollable,
            clip_only,
            scroll_ancestor,
        );
    }
}

fn positioned_descendant_containing_block(
    flat: &[FlatNodeInfo<'_>],
    tree: &TaffyTree<usize>,
    node: NodeId,
    rect: LayoutRect,
) -> LayoutRect {
    let Some(index) = tree.get_node_context(node).copied() else {
        return rect;
    };
    let Some(info) = flat.get(index) else {
        return rect;
    };
    if !matches!(info.style.display, WDisplay::Inline) {
        let Ok(layout) = tree.layout(node) else {
            return rect;
        };
        return LayoutRect {
            x: rect.x + layout.border.left,
            y: rect.y + layout.border.top,
            width: (rect.width - layout.border.left - layout.border.right).max(0.0),
            height: (rect.height - layout.border.top - layout.border.bottom).max(0.0),
        };
    }

    let Ok(children) = tree.children(node) else {
        return rect;
    };
    let mut has_block_split = false;
    let mut current_fragment_width = 0.0_f32;
    let mut widest_fragment = 0.0_f32;
    for child in children {
        let Some(child_index) = tree.get_node_context(child).copied() else {
            continue;
        };
        let Some(child_info) = flat.get(child_index) else {
            continue;
        };
        if matches!(child_info.style.display, WDisplay::None)
            || matches!(child_info.style.position, WPos::Absolute | WPos::Fixed)
        {
            continue;
        }
        if matches!(
            child_info.style.display,
            WDisplay::Block | WDisplay::Flex | WDisplay::Grid | WDisplay::ListItem
        ) {
            has_block_split = true;
            widest_fragment = widest_fragment.max(current_fragment_width);
            current_fragment_width = 0.0;
            continue;
        }
        let Ok(layout) = tree.layout(child) else {
            continue;
        };
        current_fragment_width += layout.size.width;
    }
    widest_fragment = widest_fragment.max(current_fragment_width);

    if has_block_split && widest_fragment > 0.0 && widest_fragment < rect.width {
        LayoutRect {
            width: widest_fragment,
            ..rect
        }
    } else {
        rect
    }
}

fn inline_absolute_static_rect(
    flat: &[FlatNodeInfo<'_>],
    tree: &TaffyTree<usize>,
    node: NodeId,
    containing_block: LayoutRect,
    mut rect: LayoutRect,
) -> Option<LayoutRect> {
    let index = tree.get_node_context(node).copied()?;
    let style = flat.get(index)?.style;
    if !matches!(style.position, WPos::Absolute)
        || !matches!(style.left, WDim::Auto)
        || !matches!(style.right, WDim::Auto)
        || !matches!(style.top, WDim::Auto)
        || !matches!(style.bottom, WDim::Auto)
    {
        return None;
    }
    let parent = tree.parent(node)?;
    let siblings = tree.children(parent).ok()?;
    let parent_index = tree.get_node_context(parent).copied()?;
    let parent_style = flat.get(parent_index)?.style;
    let parent_layout = tree.layout(parent).ok()?;
    let parent_content_top = parent_layout.border.top + parent_layout.padding.top;
    let parent_margin = parent_style.margin_lengths();
    let parent_padding = parent_style.padding_lengths();
    let parent_border_left = parent_style
        .border_left_width
        .unwrap_or(parent_style.border_width);
    let inline_start_is_meaningful = matches!(parent_style.display, WDisplay::Inline)
        && [
            parent_margin.left,
            parent_padding.left,
            parent_border_left,
            parent_margin.top,
            parent_margin.bottom,
            parent_padding.top,
            parent_padding.bottom,
            parent_style
                .border_top_width
                .unwrap_or(parent_style.border_width),
            parent_style
                .border_bottom_width
                .unwrap_or(parent_style.border_width),
        ]
        .into_iter()
        .any(|value| value.abs() > f32::EPSILON);
    let mut cursor_x = if matches!(parent_style.display, WDisplay::Inline) {
        parent_margin.left + parent_padding.left + parent_border_left
    } else {
        0.0
    };
    let mut cursor_y = 0.0_f32;
    let mut line_height = if inline_start_is_meaningful {
        parent_style.font_size * parent_style.line_height
    } else {
        0.0
    };
    let mut has_meaningful_inline_predecessor = inline_start_is_meaningful;
    let mut has_in_flow_predecessor = false;
    let mut crossed_forced_line_break = false;
    let own_margin = style.margin_lengths();
    for sibling in siblings {
        if sibling == node {
            break;
        }
        let sibling_index = tree.get_node_context(sibling).copied()?;
        let sibling_info = flat.get(sibling_index)?;
        if matches!(sibling_info.style.display, WDisplay::None) {
            continue;
        }
        if matches!(sibling_info.style.position, WPos::Absolute | WPos::Fixed) {
            continue;
        }
        has_in_flow_predecessor = true;
        let layout = tree.layout(sibling).ok()?;
        if matches!(
            sibling_info.style.display,
            WDisplay::Block | WDisplay::Flex | WDisplay::Grid | WDisplay::ListItem
        ) {
            cursor_x = 0.0;
            cursor_y = layout.location.y + layout.size.height
                + sibling_info.style.margin_lengths().bottom
                - parent_content_top;
            line_height = 0.0;
            has_meaningful_inline_predecessor = false;
            crossed_forced_line_break = false;
            continue;
        }
        if !matches!(
            sibling_info.style.display,
            WDisplay::Inline | WDisplay::InlineBlock | WDisplay::InlineFlex | WDisplay::InlineTable
        ) {
            return None;
        }
        let (width, height, meaningful) = match sibling_info.kind {
            ComponentKind::Text { content } => {
                if content == "\u{2028}" {
                    cursor_x = 0.0;
                    cursor_y += line_height
                        .max(sibling_info.style.font_size * sibling_info.style.line_height);
                    line_height = 0.0;
                    crossed_forced_line_break = true;
                    continue;
                }
                if !crossed_forced_line_break
                    && content
                        .chars()
                        .all(|character| matches!(character, ' ' | '\t' | '\n' | '\r' | '\u{000c}'))
                {
                    // With the positioned sibling removed from normal flow,
                    // same-line whitespace immediately before it is trailing
                    // collapsible space and contributes no static advance.
                    // After a forced break it is instead the leading space of
                    // the hypothetical line and remains part of that position.
                    continue;
                }
                let (width, height) = text_intrinsic_size(content, sibling_info.style);
                (
                    width,
                    height,
                    content.chars().any(|character| !character.is_whitespace()),
                )
            }
            _ => (layout.size.width, layout.size.height, true),
        };
        if cursor_x > 0.0 && cursor_x + width > containing_block.width {
            cursor_x = 0.0;
            cursor_y += line_height;
            line_height = 0.0;
        }
        cursor_x += width;
        line_height = line_height.max(height);
        has_meaningful_inline_predecessor |= meaningful;
    }
    if !has_meaningful_inline_predecessor {
        if matches!(parent_style.display, WDisplay::Block) && !has_in_flow_predecessor {
            rect.x = match parent_style.direction {
                w3cos_std::style::TextDirection::Ltr => containing_block.x + own_margin.left,
                w3cos_std::style::TextDirection::Rtl => {
                    containing_block.x + containing_block.width - rect.width - own_margin.right
                }
            };
            rect.y = containing_block.y + own_margin.top;
            return Some(rect);
        }
        if !matches!(parent_style.display, WDisplay::Block) {
            return None;
        }
    }
    let fragmented_inline_start_padding = crossed_forced_line_break
        .then(|| {
            matches!(parent_style.display, WDisplay::Inline)
                .then_some(parent_style.padding_lengths().left)
        })
        .flatten()
        .unwrap_or(0.0);
    if matches!(
        style.display,
        WDisplay::Block | WDisplay::Flex | WDisplay::Grid | WDisplay::ListItem
    ) {
        // A block-level static-position placeholder splits its inline parent.
        // Its block starts after the preceding line box, at the containing
        // block's inline start rather than after the inline fragment itself.
        rect.x = match parent_style.direction {
            w3cos_std::style::TextDirection::Ltr => containing_block.x + own_margin.left,
            w3cos_std::style::TextDirection::Rtl => {
                containing_block.x + containing_block.width - rect.width - own_margin.right
            }
        };
        rect.y = containing_block.y + cursor_y + line_height + own_margin.top;
    } else {
        rect.x = containing_block.x - fragmented_inline_start_padding + cursor_x + own_margin.left;
        rect.y = containing_block.y + cursor_y + own_margin.top;
    }
    Some(rect)
}

fn compute_relative_percentage_rect(
    style: &w3cos_std::style::Style,
    containing_block: LayoutRect,
    containing_block_height_definite: bool,
    mut rect: LayoutRect,
) -> LayoutRect {
    match (style.left, style.right) {
        (WDim::Percent(value), _) => rect.x += containing_block.width * value / 100.0,
        (WDim::Auto, WDim::Percent(value)) => {
            rect.x -= containing_block.width * value / 100.0;
        }
        _ => {}
    }
    if containing_block_height_definite {
        match (style.top, style.bottom) {
            (WDim::Percent(value), _) => rect.y += containing_block.height * value / 100.0,
            (WDim::Auto, WDim::Percent(value)) => {
                rect.y -= containing_block.height * value / 100.0;
            }
            _ => {}
        }
    }
    rect
}

fn compute_absolute_rect(
    style: &w3cos_std::style::Style,
    containing_block: LayoutRect,
    fallback: LayoutRect,
    viewport_w: f32,
    viewport_h: f32,
    containing_direction: w3cos_std::style::TextDirection,
    replaced: bool,
) -> LayoutRect {
    let resolve_spacing = |spacing: WSpacing| match spacing {
        WSpacing::Percent(value) => containing_block.width * value / 100.0,
        WSpacing::Rem(value) => value * ROOT_FONT_SIZE,
        WSpacing::Em(value) => value * style.font_size,
        WSpacing::Vw(value) => value * viewport_w / 100.0,
        WSpacing::Vh(value) => value * viewport_h / 100.0,
        WSpacing::Auto => 0.0,
        other => other.resolve(&w3cos_std::safe_area::current()),
    };
    let resolve_h = |d: WDim| {
        d.resolve(
            containing_block.width,
            ROOT_FONT_SIZE,
            style.font_size,
            viewport_w,
            viewport_h,
        )
    };
    let resolve_v = |d: WDim| {
        d.resolve(
            containing_block.height,
            ROOT_FONT_SIZE,
            style.font_size,
            viewport_w,
            viewport_h,
        )
    };
    let (width, height) = positioned_percentage_border_box_size(
        style,
        containing_block.width,
        containing_block.height,
        viewport_w,
        viewport_h,
        fallback.width,
        fallback.height,
        replaced,
    );

    let x = match (resolve_h(style.left), resolve_h(style.right)) {
        (Some(left), Some(right)) => {
            let left_auto = matches!(style.margin.left, WSpacing::Auto);
            let right_auto = matches!(style.margin.right, WSpacing::Auto);
            let remaining = containing_block.width
                - left
                - right
                - width
                - resolve_spacing(style.margin.left)
                - resolve_spacing(style.margin.right);
            let margin_left = match (left_auto, right_auto) {
                (true, true) if remaining >= 0.0 => remaining / 2.0,
                (true, true) if containing_direction == w3cos_std::style::TextDirection::Ltr => 0.0,
                (true, _) => remaining,
                _ => resolve_spacing(style.margin.left),
            };
            if !left_auto
                && !right_auto
                && containing_direction == w3cos_std::style::TextDirection::Rtl
            {
                // An over-constrained RTL box ignores left, not right.
                containing_block.x + containing_block.width - right - width
                    - resolve_spacing(style.margin.right)
            } else {
                containing_block.x + left + margin_left
            }
        }
        (Some(left), _) => containing_block.x + left + resolve_spacing(style.margin.left),
        (None, Some(right)) => {
            containing_block.x + containing_block.width
                - right
                - width
                - resolve_spacing(style.margin.right)
        }
        (None, None) => fallback.x,
    };
    let y = match (resolve_v(style.top), resolve_v(style.bottom)) {
        (Some(top), Some(bottom))
            if replaced || !matches!(style.height, WDim::Auto)
                || (height
                    - (containing_block.height
                        - top
                        - bottom
                        - resolve_spacing(style.margin.top)
                        - resolve_spacing(style.margin.bottom))
                    .max(0.0))
                .abs() > f32::EPSILON =>
        {
            let top_auto = matches!(style.margin.top, WSpacing::Auto);
            let bottom_auto = matches!(style.margin.bottom, WSpacing::Auto);
            let remaining = containing_block.height
                - top
                - bottom
                - height
                - resolve_spacing(style.margin.top)
                - resolve_spacing(style.margin.bottom);
            let margin_top = if top_auto {
                // CSS2 absolute block-axis constraints divide even negative
                // remaining space equally when both margins are automatic.
                remaining / if bottom_auto { 2.0 } else { 1.0 }
            } else {
                resolve_spacing(style.margin.top)
            };
            containing_block.y + top + margin_top
        }
        (Some(top), _) => containing_block.y + top + resolve_spacing(style.margin.top),
        (None, Some(bottom)) => {
            containing_block.y + containing_block.height
                - bottom
                - height
                - resolve_spacing(style.margin.bottom)
        }
        (None, None) => fallback.y,
    };

    LayoutRect {
        x,
        y,
        width,
        height,
    }
}

fn compute_fixed_rect(
    style: &w3cos_std::style::Style,
    viewport_w: f32,
    viewport_h: f32,
    fallback: LayoutRect,
    replaced: bool,
) -> LayoutRect {
    let resolve_h = |d: WDim| {
        d.resolve(
            viewport_w,
            ROOT_FONT_SIZE,
            style.font_size,
            viewport_w,
            viewport_h,
        )
    };
    let resolve_v = |d: WDim| {
        d.resolve(
            viewport_h,
            ROOT_FONT_SIZE,
            style.font_size,
            viewport_w,
            viewport_h,
        )
    };

    let left = resolve_h(style.left);
    let right = resolve_h(style.right);
    let top = resolve_v(style.top);
    let bottom = resolve_v(style.bottom);
    let (width, height) = positioned_percentage_border_box_size(
        style,
        viewport_w,
        viewport_h,
        viewport_w,
        viewport_h,
        fallback.width,
        fallback.height,
        replaced,
    );

    let x = match (left, right) {
        (Some(l), _) => l,
        (None, Some(r)) => viewport_w - r - width,
        (None, None) => fallback.x,
    };
    let y = match (top, bottom) {
        (Some(t), _) => t,
        (None, Some(b)) => viewport_h - b - height,
        (None, None) => fallback.y,
    };

    LayoutRect {
        x,
        y,
        width,
        height,
    }
}

#[allow(clippy::too_many_arguments)]
fn positioned_percentage_border_box_size(
    style: &w3cos_std::style::Style,
    containing_width: f32,
    containing_height: f32,
    viewport_w: f32,
    viewport_h: f32,
    fallback_width: f32,
    fallback_height: f32,
    replaced: bool,
) -> (f32, f32) {
    let resolve_spacing = |spacing: WSpacing, percentage_basis: f32| match spacing {
        WSpacing::Percent(value) => percentage_basis * value / 100.0,
        WSpacing::Rem(value) => value * ROOT_FONT_SIZE,
        WSpacing::Em(value) => value * style.font_size,
        WSpacing::Vw(value) => value * viewport_w / 100.0,
        WSpacing::Vh(value) => value * viewport_h / 100.0,
        WSpacing::Auto => 0.0,
        other => other.resolve(&w3cos_std::safe_area::current()),
    };
    let content_width = style.width.resolve(
        containing_width,
        ROOT_FONT_SIZE,
        style.font_size,
        viewport_w,
        viewport_h,
    );
    let content_height = style.height.resolve(
        containing_height,
        ROOT_FONT_SIZE,
        style.font_size,
        viewport_w,
        viewport_h,
    );
    let width = if !replaced && matches!(style.width, WDim::Auto)
        && let (Some(left), Some(right)) = (
            style.left.resolve(
                containing_width,
                ROOT_FONT_SIZE,
                style.font_size,
                viewport_w,
                viewport_h,
            ),
            style.right.resolve(
                containing_width,
                ROOT_FONT_SIZE,
                style.font_size,
                viewport_w,
                viewport_h,
            ),
        ) {
        (containing_width
            - left
            - right
            - resolve_spacing(style.margin.left, containing_width)
            - resolve_spacing(style.margin.right, containing_width))
        .max(0.0)
    } else if matches!(style.width, WDim::Percent(_)) {
        let width = content_width.unwrap_or(fallback_width);
        if style.box_sizing == WBoxSizing::ContentBox {
            width
                + resolve_spacing(style.padding.left, containing_width)
                + resolve_spacing(style.padding.right, containing_width)
                + style.border_left_width.unwrap_or(style.border_width)
                + style.border_right_width.unwrap_or(style.border_width)
        } else {
            width
        }
    } else {
        fallback_width
    };
    let height = if !replaced && matches!(style.height, WDim::Auto)
        && let (Some(top), Some(bottom)) = (
            style.top.resolve(
                containing_height,
                ROOT_FONT_SIZE,
                style.font_size,
                viewport_w,
                viewport_h,
            ),
            style.bottom.resolve(
                containing_height,
                ROOT_FONT_SIZE,
                style.font_size,
                viewport_w,
                viewport_h,
            ),
        ) {
        (containing_height
            - top
            - bottom
            - resolve_spacing(style.margin.top, containing_width)
            - resolve_spacing(style.margin.bottom, containing_width))
        .max(0.0)
    } else if matches!(style.height, WDim::Percent(_)) {
        let height = content_height.unwrap_or(fallback_height);
        if style.box_sizing == WBoxSizing::ContentBox {
            height
                + resolve_spacing(style.padding.top, containing_width)
                + resolve_spacing(style.padding.bottom, containing_width)
                + style.border_top_width.unwrap_or(style.border_width)
                + style.border_bottom_width.unwrap_or(style.border_width)
        } else {
            height
        }
    } else {
        fallback_height
    };
    let horizontal_inner_edges = if style.box_sizing == WBoxSizing::ContentBox {
        resolve_spacing(style.padding.left, containing_width)
            + resolve_spacing(style.padding.right, containing_width)
            + style.border_left_width.unwrap_or(style.border_width)
            + style.border_right_width.unwrap_or(style.border_width)
    } else {
        0.0
    };
    let resolve_width_limit = |dimension: WDim| {
        dimension
            .resolve(
                containing_width,
                ROOT_FONT_SIZE,
                style.font_size,
                viewport_w,
                viewport_h,
            )
            .map(|value| value.max(0.0) + horizontal_inner_edges)
    };
    let width = width
        .min(resolve_width_limit(style.max_width).unwrap_or(f32::INFINITY))
        .max(resolve_width_limit(style.min_width).unwrap_or(0.0));
    let vertical_inner_edges = if style.box_sizing == WBoxSizing::ContentBox {
        resolve_spacing(style.padding.top, containing_width)
            + resolve_spacing(style.padding.bottom, containing_width)
            + style.border_top_width.unwrap_or(style.border_width)
            + style.border_bottom_width.unwrap_or(style.border_width)
    } else {
        0.0
    };
    let resolve_height_limit = |dimension: WDim| {
        dimension
            .resolve(
                containing_height,
                ROOT_FONT_SIZE,
                style.font_size,
                viewport_w,
                viewport_h,
            )
            .map(|value| value.max(0.0) + vertical_inner_edges)
    };
    // Insets may have resolved an auto height after Taffy applied min/max.
    // Reapply the constraints to this border-box result, with min winning.
    let height = height
        .min(resolve_height_limit(style.max_height).unwrap_or(f32::INFINITY))
        .max(resolve_height_limit(style.min_height).unwrap_or(0.0));
    (width, height)
}

// ---------------------------------------------------------------------------
// Style conversion helpers
// ---------------------------------------------------------------------------

fn to_taffy_style(s: &w3cos_std::style::Style, viewport_w: f32, viewport_h: f32) -> Style {
    let (display, flex_grow, flex_shrink, size) = match s.display {
        WDisplay::Flex => (
            taffy::Display::Flex,
            s.flex_grow,
            s.flex_shrink,
            Size {
                width: to_taffy_dim(s.width, s.font_size, viewport_w, viewport_h),
                height: to_taffy_dim(s.height, s.font_size, viewport_w, viewport_h),
            },
        ),
        WDisplay::Grid => (
            taffy::Display::Grid,
            s.flex_grow,
            s.flex_shrink,
            Size {
                width: to_taffy_dim(s.width, s.font_size, viewport_w, viewport_h),
                height: to_taffy_dim(s.height, s.font_size, viewport_w, viewport_h),
            },
        ),
        WDisplay::Block => (
            taffy::Display::Block,
            s.flex_grow,
            s.flex_shrink,
            Size {
                width: to_taffy_dim(s.width, s.font_size, viewport_w, viewport_h),
                height: to_taffy_dim(s.height, s.font_size, viewport_w, viewport_h),
            },
        ),
        WDisplay::Inline => (
            taffy::Display::Flex,
            s.flex_grow,
            s.flex_shrink,
            Size {
                width: Dimension::auto(),
                height: Dimension::auto(),
            },
        ),
        WDisplay::InlineBlock => (
            taffy::Display::Flex,
            s.flex_grow,
            s.flex_shrink,
            Size {
                width: to_taffy_dim(s.width, s.font_size, viewport_w, viewport_h),
                height: to_taffy_dim(s.height, s.font_size, viewport_w, viewport_h),
            },
        ),
        WDisplay::InlineFlex => (
            taffy::Display::Flex,
            s.flex_grow,
            s.flex_shrink,
            Size {
                width: to_taffy_dim(s.width, s.font_size, viewport_w, viewport_h),
                height: to_taffy_dim(s.height, s.font_size, viewport_w, viewport_h),
            },
        ),
        WDisplay::InlineTable
        | WDisplay::Table
        | WDisplay::TableRow
        | WDisplay::TableRowGroup
        | WDisplay::TableHeaderGroup
        | WDisplay::TableFooterGroup => (
            taffy::Display::Flex,
            s.flex_grow,
            s.flex_shrink,
            Size {
                width: to_taffy_dim(s.width, s.font_size, viewport_w, viewport_h),
                height: to_taffy_dim(s.height, s.font_size, viewport_w, viewport_h),
            },
        ),
        WDisplay::TableColumnGroup
        | WDisplay::TableColumn
        | WDisplay::TableCaption
        | WDisplay::ListItem
        | WDisplay::TableCell => (
            taffy::Display::Block,
            s.flex_grow,
            s.flex_shrink,
            Size {
                width: to_taffy_dim(s.width, s.font_size, viewport_w, viewport_h),
                height: to_taffy_dim(s.height, s.font_size, viewport_w, viewport_h),
            },
        ),
        // DOM lowering normally removes the generated box for `contents`.
        // Synthetic Component trees can still reach this conversion path.
        WDisplay::Contents => (
            taffy::Display::Flex,
            s.flex_grow,
            s.flex_shrink,
            Size {
                width: to_taffy_dim(s.width, s.font_size, viewport_w, viewport_h),
                height: to_taffy_dim(s.height, s.font_size, viewport_w, viewport_h),
            },
        ),
        WDisplay::None => (
            taffy::Display::None,
            s.flex_grow,
            s.flex_shrink,
            Size {
                width: to_taffy_dim(s.width, s.font_size, viewport_w, viewport_h),
                height: to_taffy_dim(s.height, s.font_size, viewport_w, viewport_h),
            },
        ),
    };
    let mut margin = if s.display == WDisplay::TableCell {
        // CSS table-cell boxes do not accept margins. Taffy's flex fallback
        // would otherwise add those margins to column and row sizing.
        Rect {
            top: LengthPercentageAuto::length(0.0),
            right: LengthPercentageAuto::length(0.0),
            bottom: LengthPercentageAuto::length(0.0),
            left: LengthPercentageAuto::length(0.0),
        }
    } else if s.display == WDisplay::Inline {
        // Vertical margins do not participate in the line box of a
        // non-replaced inline box. Keep the authored horizontal margins for
        // line fitting, but do not let Taffy's flex fallback move the inline
        // down or enlarge the line.
        Rect {
            top: LengthPercentageAuto::length(0.0),
            right: to_taffy_margin(s.margin.right, s.font_size, viewport_w, viewport_h),
            bottom: LengthPercentageAuto::length(0.0),
            left: to_taffy_margin(s.margin.left, s.font_size, viewport_w, viewport_h),
        }
    } else if matches!(
        s.display,
        WDisplay::InlineBlock | WDisplay::InlineFlex | WDisplay::InlineTable
    ) {
        // Auto margins on inline-level non-replaced boxes have a used value
        // of zero. Do not let the Flex fallback turn them into free-space
        // distribution and center a shrink-to-fit inline box.
        let inline_margin = |value| {
            if matches!(value, WSpacing::Auto) {
                LengthPercentageAuto::length(0.0)
            } else {
                to_taffy_margin(value, s.font_size, viewport_w, viewport_h)
            }
        };
        Rect {
            top: inline_margin(s.margin.top),
            right: inline_margin(s.margin.right),
            bottom: inline_margin(s.margin.bottom),
            left: inline_margin(s.margin.left),
        }
    } else {
        Rect {
            top: to_taffy_margin(s.margin.top, s.font_size, viewport_w, viewport_h),
            right: to_taffy_margin(s.margin.right, s.font_size, viewport_w, viewport_h),
            bottom: to_taffy_margin(s.margin.bottom, s.font_size, viewport_w, viewport_h),
            left: to_taffy_margin(s.margin.left, s.font_size, viewport_w, viewport_h),
        }
    };
    if s.float == WFloat::Right {
        // Taffy's block fallback has no float placement algorithm. An auto
        // start margin places the float's margin box against the inline end
        // while preserving its participation in the containing block.
        margin.left = LengthPercentageAuto::auto();
    }

    Style {
        display,
        box_sizing: match s.box_sizing {
            WBoxSizing::ContentBox => BoxSizing::ContentBox,
            WBoxSizing::BorderBox => BoxSizing::BorderBox,
        },
        position: match s.position {
            WPos::Static | WPos::Relative | WPos::Sticky => taffy::Position::Relative,
            WPos::Absolute | WPos::Fixed => taffy::Position::Absolute,
        },
        flex_direction: match (s.display, s.flex_direction) {
            (
                WDisplay::Table
                | WDisplay::TableRowGroup
                | WDisplay::TableHeaderGroup
                | WDisplay::TableFooterGroup
                | WDisplay::TableCell,
                _,
            ) => FlexDirection::Column,
            (WDisplay::TableRow, WDir::RowReverse) => FlexDirection::RowReverse,
            (WDisplay::TableRow, _) | (_, WDir::Row) => FlexDirection::Row,
            (_, WDir::Column) => FlexDirection::Column,
            (_, WDir::RowReverse) => FlexDirection::RowReverse,
            (_, WDir::ColumnReverse) => FlexDirection::ColumnReverse,
        },
        justify_content: Some(if s.display == WDisplay::TableCell {
            match s.align_self {
                WAlignSelf::FlexEnd => JustifyContent::FlexEnd,
                WAlignSelf::Center => JustifyContent::Center,
                _ => JustifyContent::FlexStart,
            }
        } else {
            match s.justify_content {
                WJustify::FlexStart => JustifyContent::FlexStart,
                WJustify::FlexEnd => JustifyContent::FlexEnd,
                WJustify::Center => JustifyContent::Center,
                WJustify::SpaceBetween => JustifyContent::SpaceBetween,
                WJustify::SpaceAround => JustifyContent::SpaceAround,
                WJustify::SpaceEvenly => JustifyContent::SpaceEvenly,
            }
        }),
        align_items: Some(match s.align_items {
            WAlign::FlexStart => AlignItems::FlexStart,
            WAlign::FlexEnd => AlignItems::FlexEnd,
            WAlign::Center => AlignItems::Center,
            WAlign::Stretch => AlignItems::Stretch,
            WAlign::Baseline => AlignItems::Baseline,
        }),
        align_content: Some(match s.align_content {
            WAlignContent::FlexStart => AlignContent::FlexStart,
            WAlignContent::FlexEnd => AlignContent::FlexEnd,
            WAlignContent::Center => AlignContent::Center,
            WAlignContent::SpaceBetween => AlignContent::SpaceBetween,
            WAlignContent::SpaceAround => AlignContent::SpaceAround,
            WAlignContent::SpaceEvenly => AlignContent::SpaceEvenly,
            WAlignContent::Stretch => AlignContent::Stretch,
        }),
        align_self: if s.display == WDisplay::TableCell {
            Some(AlignSelf::Stretch)
        } else {
            to_taffy_align_self(s.align_self)
        },
        justify_items: Some(match s.justify_items {
            WAlign::FlexStart => AlignItems::FlexStart,
            WAlign::FlexEnd => AlignItems::FlexEnd,
            WAlign::Center => AlignItems::Center,
            WAlign::Stretch => AlignItems::Stretch,
            WAlign::Baseline => AlignItems::Baseline,
        }),
        justify_self: to_taffy_align_self(s.justify_self),
        flex_wrap: match s.flex_wrap {
            WWrap::NoWrap => FlexWrap::NoWrap,
            WWrap::Wrap => FlexWrap::Wrap,
            WWrap::WrapReverse => FlexWrap::WrapReverse,
        },
        flex_grow,
        flex_shrink,
        flex_basis: to_taffy_dim(s.flex_basis, s.font_size, viewport_w, viewport_h),
        inset: Rect {
            top: to_taffy_inset(s.top, s.position, s.font_size, viewport_w, viewport_h),
            right: to_taffy_inset(s.right, s.position, s.font_size, viewport_w, viewport_h),
            bottom: to_taffy_inset(s.bottom, s.position, s.font_size, viewport_w, viewport_h),
            left: to_taffy_inset(s.left, s.position, s.font_size, viewport_w, viewport_h),
        },
        gap: Size {
            width: LengthPercentage::length(s.column_gap.unwrap_or(s.gap)),
            height: LengthPercentage::length(s.row_gap.unwrap_or(s.gap)),
        },
        padding: Rect {
            top: to_taffy_spacing(s.padding.top, s.font_size, viewport_w, viewport_h),
            right: to_taffy_spacing(s.padding.right, s.font_size, viewport_w, viewport_h),
            bottom: to_taffy_spacing(s.padding.bottom, s.font_size, viewport_w, viewport_h),
            left: to_taffy_spacing(s.padding.left, s.font_size, viewport_w, viewport_h),
        },
        border: Rect {
            top: LengthPercentage::length(s.border_top_width.unwrap_or(s.border_width)),
            right: LengthPercentage::length(s.border_right_width.unwrap_or(s.border_width)),
            bottom: LengthPercentage::length(s.border_bottom_width.unwrap_or(s.border_width)),
            left: LengthPercentage::length(s.border_left_width.unwrap_or(s.border_width)),
        },
        margin,
        overflow: taffy::Point {
            x: to_taffy_overflow(s.resolved_overflow_x()),
            y: to_taffy_overflow(s.resolved_overflow_y()),
        },
        size,
        min_size: Size {
            width: to_taffy_dim(s.min_width, s.font_size, viewport_w, viewport_h),
            height: to_taffy_dim(s.min_height, s.font_size, viewport_w, viewport_h),
        },
        max_size: Size {
            width: to_taffy_dim(s.max_width, s.font_size, viewport_w, viewport_h),
            height: to_taffy_dim(s.max_height, s.font_size, viewport_w, viewport_h),
        },
        grid_template_columns: s
            .grid_template_columns
            .as_deref()
            .map(parse_grid_template_columns)
            .unwrap_or_default(),
        // A one-column CSS grid with no explicit template stretches its
        // implicit column across the available inline size. Taffy's default
        // auto track remains max-content when our shared Style model maps the
        // default `justify-content` to FlexStart, which collapses nested form
        // rows to their smallest control. Model the browser's effective
        // single implicit track directly; explicit templates retain their own
        // track sizing below.
        grid_auto_columns: if matches!(s.display, WDisplay::Grid)
            && s.grid_template_columns.is_none()
        {
            vec![taffy::style_helpers::flex(1.0)]
        } else {
            Vec::new()
        },
        grid_column: s
            .grid_column
            .as_deref()
            .map(parse_grid_column)
            .unwrap_or_default(),
        ..Style::DEFAULT
    }
}

fn to_taffy_align_self(value: WAlignSelf) -> Option<AlignSelf> {
    match value {
        WAlignSelf::Auto => None,
        WAlignSelf::FlexStart => Some(AlignSelf::FlexStart),
        WAlignSelf::FlexEnd => Some(AlignSelf::FlexEnd),
        WAlignSelf::Center => Some(AlignSelf::Center),
        WAlignSelf::Baseline => Some(AlignSelf::Baseline),
        WAlignSelf::Stretch => Some(AlignSelf::Stretch),
    }
}

fn parse_grid_template_columns(value: &str) -> Vec<GridTemplateComponent<String>> {
    let mut tracks = Vec::new();
    for token in split_css_top_level_whitespace(value) {
        if let Some(inner) = token
            .strip_prefix("repeat(")
            .and_then(|value| value.strip_suffix(')'))
            && let Some((count, track)) = split_css_top_level_once(inner, ',')
        {
            let count = count
                .trim()
                .parse::<usize>()
                .ok()
                .or_else(|| {
                    count.rsplit_once(',').and_then(|(_, fallback)| {
                        fallback.trim_end_matches(')').trim().parse().ok()
                    })
                })
                .unwrap_or(1)
                .min(64);
            if let Some(track) = parse_grid_track(track.trim()) {
                tracks.extend(std::iter::repeat_n(track, count));
            }
        } else if let Some(track) = parse_grid_track(token.trim()) {
            tracks.push(track);
        }
    }
    tracks
}

fn parse_grid_track(value: &str) -> Option<GridTemplateComponent<String>> {
    let value = value.trim();
    if let Some(number) = value.strip_suffix("fr")
        && let Ok(number) = number.trim().parse::<f32>()
    {
        return Some(taffy::style_helpers::flex(number));
    }
    if let Some(inner) = value
        .strip_prefix("minmax(")
        .and_then(|value| value.strip_suffix(')'))
        && let Some((min, max)) = split_css_top_level_once(inner, ',')
    {
        let min = parse_grid_min_track(min.trim())?;
        let max = parse_grid_max_track(max.trim())?;
        return Some(taffy::style_helpers::minmax(min, max));
    }
    if value == "auto" {
        return Some(taffy::style_helpers::auto());
    }
    if let Some(number) = value.strip_suffix('%')
        && let Ok(number) = number.trim().parse::<f32>()
    {
        return Some(taffy::style_helpers::percent(number / 100.0));
    }
    parse_css_length_px(value).map(taffy::style_helpers::length)
}

fn parse_grid_min_track(value: &str) -> Option<MinTrackSizingFunction> {
    if value == "auto" {
        return Some(taffy::style_helpers::auto());
    }
    if value == "0" {
        return Some(taffy::style_helpers::zero());
    }
    if let Some(number) = value.strip_suffix('%')
        && let Ok(number) = number.trim().parse::<f32>()
    {
        return Some(taffy::style_helpers::percent(number / 100.0));
    }
    parse_css_length_px(value).map(taffy::style_helpers::length)
}

fn parse_grid_max_track(value: &str) -> Option<MaxTrackSizingFunction> {
    if let Some(number) = value.strip_suffix("fr")
        && let Ok(number) = number.trim().parse::<f32>()
    {
        return Some(taffy::style_helpers::fr(number));
    }
    if value == "auto" {
        return Some(taffy::style_helpers::auto());
    }
    if let Some(number) = value.strip_suffix('%')
        && let Ok(number) = number.trim().parse::<f32>()
    {
        return Some(taffy::style_helpers::percent(number / 100.0));
    }
    parse_css_length_px(value).map(taffy::style_helpers::length)
}

fn parse_css_length_px(value: &str) -> Option<f32> {
    value
        .trim()
        .strip_suffix("px")
        .unwrap_or(value.trim())
        .trim()
        .parse()
        .ok()
}

fn split_css_top_level_whitespace(value: &str) -> Vec<&str> {
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

fn split_css_top_level_once(value: &str, separator: char) -> Option<(&str, &str)> {
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

fn parse_grid_column(value: &str) -> Line<GridPlacement<String>> {
    let value = value.trim();
    if let Ok(start) = value.parse::<i16>() {
        return Line {
            start: taffy::style_helpers::line(start),
            end: GridPlacement::Auto,
        };
    }
    if let Some(span) = value.strip_prefix("span ")
        && let Ok(span) = span.trim().parse::<u16>()
    {
        return Line {
            start: taffy::style_helpers::span(span),
            end: GridPlacement::Auto,
        };
    }
    if let Some((start, end)) = value.split_once('/')
        && let (Ok(start), Ok(end)) = (start.trim().parse::<i16>(), end.trim().parse::<i16>())
    {
        return Line {
            start: taffy::style_helpers::line(start),
            end: taffy::style_helpers::line(end),
        };
    }
    Line::default()
}

fn to_taffy_spacing(
    spacing: WSpacing,
    local_font_size: f32,
    viewport_w: f32,
    viewport_h: f32,
) -> LengthPercentage {
    match spacing {
        WSpacing::Percent(v) => LengthPercentage::percent(v / 100.0),
        WSpacing::Rem(v) => LengthPercentage::length(v * ROOT_FONT_SIZE),
        WSpacing::Em(v) => LengthPercentage::length(v * local_font_size),
        WSpacing::Vw(v) => LengthPercentage::length(v * viewport_w / 100.0),
        WSpacing::Vh(v) => LengthPercentage::length(v * viewport_h / 100.0),
        WSpacing::Auto => LengthPercentage::length(0.0),
        other => LengthPercentage::length(other.resolve(&w3cos_std::safe_area::current())),
    }
}

fn to_taffy_margin(
    spacing: WSpacing,
    local_font_size: f32,
    viewport_w: f32,
    viewport_h: f32,
) -> LengthPercentageAuto {
    match spacing {
        WSpacing::Auto => LengthPercentageAuto::auto(),
        WSpacing::Percent(v) => LengthPercentageAuto::percent(v / 100.0),
        WSpacing::Rem(v) => LengthPercentageAuto::length(v * ROOT_FONT_SIZE),
        WSpacing::Em(v) => LengthPercentageAuto::length(v * local_font_size),
        WSpacing::Vw(v) => LengthPercentageAuto::length(v * viewport_w / 100.0),
        WSpacing::Vh(v) => LengthPercentageAuto::length(v * viewport_h / 100.0),
        other => LengthPercentageAuto::length(other.resolve(&w3cos_std::safe_area::current())),
    }
}

fn to_taffy_dim(d: WDim, local_font_size: f32, viewport_w: f32, viewport_h: f32) -> Dimension {
    match d {
        WDim::Auto => Dimension::auto(),
        WDim::Px(v) => Dimension::length(v),
        WDim::Percent(v) => Dimension::percent(v / 100.0),
        WDim::Rem(v) => Dimension::length(v * 16.0),
        WDim::Em(v) => Dimension::length(v * local_font_size),
        WDim::Ch(v) => Dimension::length(
            v * layout_font()
                .metrics('0', local_font_size)
                .advance_width,
        ),
        WDim::Vw(v) => Dimension::length(v * viewport_w / 100.0),
        WDim::Vh(v) => Dimension::length(v * viewport_h / 100.0),
    }
}

fn to_taffy_auto(
    d: WDim,
    local_font_size: f32,
    viewport_w: f32,
    viewport_h: f32,
) -> LengthPercentageAuto {
    match d {
        WDim::Auto => LengthPercentageAuto::auto(),
        WDim::Px(v) => LengthPercentageAuto::length(v),
        WDim::Percent(v) => LengthPercentageAuto::percent(v / 100.0),
        WDim::Rem(v) => LengthPercentageAuto::length(v * 16.0),
        WDim::Em(v) => LengthPercentageAuto::length(v * local_font_size),
        WDim::Ch(v) => LengthPercentageAuto::length(
            v * layout_font()
                .metrics('0', local_font_size)
                .advance_width,
        ),
        WDim::Vw(v) => LengthPercentageAuto::length(v * viewport_w / 100.0),
        WDim::Vh(v) => LengthPercentageAuto::length(v * viewport_h / 100.0),
    }
}

fn to_taffy_inset(
    dimension: WDim,
    position: WPos,
    local_font_size: f32,
    viewport_w: f32,
    viewport_h: f32,
) -> LengthPercentageAuto {
    if matches!(position, WPos::Static)
        || matches!(position, WPos::Relative) && matches!(dimension, WDim::Percent(_))
    {
        LengthPercentageAuto::auto()
    } else {
        to_taffy_auto(dimension, local_font_size, viewport_w, viewport_h)
    }
}

fn to_taffy_overflow(o: WOverflow) -> taffy::Overflow {
    match o {
        WOverflow::Visible => taffy::Overflow::Visible,
        WOverflow::Hidden => taffy::Overflow::Hidden,
        WOverflow::Scroll => taffy::Overflow::Scroll,
        WOverflow::Auto => taffy::Overflow::Scroll,
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use w3cos_std::color::Color;
    use w3cos_std::component::Component;
    use w3cos_std::style::{
        Dimension as WDim, Display as WDisp, FlexDirection as WDir, Position as WPos, Style,
    };

    fn s() -> Style {
        Style::default()
    }

    fn col() -> Style {
        Style {
            display: WDisp::Flex,
            flex_direction: WDir::Column,
            box_sizing: WBoxSizing::BorderBox,
            gap: 10.0,
            padding: w3cos_std::style::Edges::all(16.0),
            width: WDim::Px(400.0),
            height: WDim::Px(600.0),
            ..Style::default()
        }
    }

    fn row() -> Style {
        Style {
            display: WDisp::Flex,
            flex_direction: WDir::Row,
            gap: 8.0,
            width: WDim::Px(400.0),
            height: WDim::Px(100.0),
            ..Style::default()
        }
    }

    #[test]
    fn component_content_width_subtracts_auto_block_edges() {
        let auto = Style {
            margin: w3cos_std::style::Edges {
                left: WSpacing::Px(8.0),
                right: WSpacing::Px(8.0),
                ..w3cos_std::style::Edges::ZERO
            },
            padding: w3cos_std::style::Edges::all(5.0),
            border_width: 1.0,
            ..Style::default()
        };
        assert_eq!(component_content_width(&auto, 800.0, 800.0, 600.0), 772.0);

        let border_box = Style {
            width: WDim::Px(100.0),
            box_sizing: WBoxSizing::BorderBox,
            padding: w3cos_std::style::Edges::all(5.0),
            border_width: 1.0,
            ..Style::default()
        };
        assert_eq!(
            component_content_width(&border_box, 800.0, 800.0, 600.0),
            88.0
        );
    }

    #[test]
    fn fixed_table_tracks_use_first_row_cell_outer_width() {
        let column = || {
            Component::boxed(
                Style {
                    display: WDisp::TableColumn,
                    ..Style::default()
                },
                vec![],
            )
        };
        let cell = |width| {
            Component::boxed(
                Style {
                    display: WDisp::TableCell,
                    width,
                    padding: w3cos_std::style::Edges::xy(60.0, 0.0),
                    ..Style::default()
                },
                vec![],
            )
        };
        let table = Component::boxed(
            Style {
                display: WDisp::Table,
                table_layout_fixed: true,
                width: WDim::Px(400.0),
                ..Style::default()
            },
            vec![
                column(),
                column(),
                column(),
                Component::row(
                    Style {
                        display: WDisp::TableRow,
                        ..Style::default()
                    },
                    vec![cell(WDim::Auto), cell(WDim::Px(80.0)), cell(WDim::Auto)],
                ),
            ],
        );

        assert_eq!(
            fixed_table_track_widths(&table, None),
            Some(vec![100.0, 200.0, 100.0])
        );
        let layout = compute(&table, 800.0, 600.0).unwrap();
        let rect = |index| layout.iter().find(|(_, item)| *item == index).unwrap().0;
        assert_eq!((rect(5).x, rect(5).width), (0.0, 100.0));
        assert_eq!((rect(6).x, rect(6).width), (100.0, 200.0));
        assert_eq!((rect(7).x, rect(7).width), (300.0, 100.0));
    }

    #[test]
    fn fixed_table_row_height_stretches_its_cells() {
        let cell = Component::boxed(
            Style {
                display: WDisp::TableCell,
                ..Style::default()
            },
            vec![],
        );
        let row = Component::row(
            Style {
                display: WDisp::TableRow,
                height: WDim::Px(96.0),
                ..Style::default()
            },
            vec![cell],
        );
        let table = Component::boxed(
            Style {
                display: WDisp::Table,
                table_layout_fixed: true,
                width: WDim::Px(96.0),
                ..Style::default()
            },
            vec![row],
        );

        let layout = compute(&table, 800.0, 600.0).unwrap();
        let rect = |index| layout.iter().find(|(_, item)| *item == index).unwrap().0;

        assert_eq!(rect(0).height, 96.0);
        assert_eq!(rect(1).height, 96.0);
        assert_eq!(rect(2).height, 96.0);
    }

    #[test]
    fn fixed_table_min_height_stretches_its_rows_and_cells() {
        for display in [WDisp::Table, WDisp::InlineTable] {
            let table = Component::boxed(
                Style {
                    display,
                    table_layout_fixed: true,
                    min_height: WDim::Px(96.0),
                    ..Style::default()
                },
                vec![Component::row(
                    Style {
                        display: WDisp::TableRow,
                        ..Style::default()
                    },
                    vec![Component::boxed(
                        Style {
                            display: WDisp::TableCell,
                            width: WDim::Px(96.0),
                            ..Style::default()
                        },
                        Vec::new(),
                    )],
                )],
            );

            let layout = compute(&table, 800.0, 600.0).unwrap();
            let rect = |index| layout.iter().find(|(_, item)| *item == index).unwrap().0;
            assert_eq!(rect(0).height, 96.0);
            assert_eq!(rect(1).height, 96.0);
            assert_eq!(rect(2).height, 96.0);
        }
    }

    #[test]
    fn fixed_auto_table_min_width_stretches_its_track() {
        for display in [WDisp::Table, WDisp::InlineTable] {
            let table = Component::boxed(
                Style {
                    display,
                    table_layout_fixed: true,
                    min_width: WDim::Px(96.0),
                    ..Style::default()
                },
                vec![Component::row(
                    Style {
                        display: WDisp::TableRow,
                        ..Style::default()
                    },
                    vec![Component::boxed(
                        Style {
                            display: WDisp::TableCell,
                            height: WDim::Px(96.0),
                            ..Style::default()
                        },
                        Vec::new(),
                    )],
                )],
            );

            let layout = compute(&table, 800.0, 600.0).unwrap();
            let rect = |index| layout.iter().find(|(_, item)| *item == index).unwrap().0;
            assert_eq!(rect(0).width, 96.0);
            assert_eq!(rect(2).width, 96.0);
        }
    }

    #[test]
    fn fixed_table_max_width_caps_its_track() {
        let table = Component::boxed(
            Style {
                display: WDisp::Table,
                table_layout_fixed: true,
                width: WDim::Px(288.0),
                max_width: WDim::Px(96.0),
                ..Style::default()
            },
            vec![Component::row(
                Style {
                    display: WDisp::TableRow,
                    ..Style::default()
                },
                vec![Component::boxed(
                    Style {
                        display: WDisp::TableCell,
                        height: WDim::Px(96.0),
                        ..Style::default()
                    },
                    Vec::new(),
                )],
            )],
        );

        assert_eq!(fixed_table_track_widths(&table, None), Some(vec![96.0]));
        let layout = compute(&table, 800.0, 600.0).unwrap();
        let rect = |index| layout.iter().find(|(_, item)| *item == index).unwrap().0;
        assert_eq!(rect(0).width, 96.0);
        assert_eq!(rect(2).width, 96.0);
    }

    #[test]
    fn empty_column_group_max_width_supplies_an_intrinsic_track() {
        let table = Component::boxed(
            Style {
                display: WDisp::Table,
                table_layout_fixed: true,
                ..Style::default()
            },
            vec![
                Component::boxed(
                    Style {
                        display: WDisp::TableColumnGroup,
                        width: WDim::Px(288.0),
                        max_width: WDim::Px(96.0),
                        ..Style::default()
                    },
                    Vec::new(),
                ),
                Component::row(
                    Style {
                        display: WDisp::TableRow,
                        ..Style::default()
                    },
                    vec![Component::boxed(
                        Style {
                            display: WDisp::TableCell,
                            height: WDim::Px(96.0),
                            ..Style::default()
                        },
                        Vec::new(),
                    )],
                ),
            ],
        );

        assert_eq!(table_track_widths(&table), vec![96.0]);
        let layout = compute(&table, 800.0, 600.0).unwrap();
        let rect = |index| layout.iter().find(|(_, item)| *item == index).unwrap().0;
        assert_eq!(rect(0).width, 96.0);
        assert_eq!(rect(3).width, 96.0);
    }

    #[test]
    fn empty_column_group_min_width_supplies_an_intrinsic_track() {
        let table = Component::boxed(
            Style {
                display: WDisp::Table,
                table_layout_fixed: true,
                ..Style::default()
            },
            vec![
                Component::boxed(
                    Style {
                        display: WDisp::TableColumnGroup,
                        min_width: WDim::Px(96.0),
                        ..Style::default()
                    },
                    Vec::new(),
                ),
                Component::row(
                    Style {
                        display: WDisp::TableRow,
                        ..Style::default()
                    },
                    vec![Component::boxed(
                        Style {
                            display: WDisp::TableCell,
                            height: WDim::Px(96.0),
                            ..Style::default()
                        },
                        Vec::new(),
                    )],
                ),
            ],
        );

        assert_eq!(table_track_widths(&table), vec![96.0]);
        let layout = compute(&table, 800.0, 600.0).unwrap();
        let rect = |index| layout.iter().find(|(_, item)| *item == index).unwrap().0;
        assert_eq!(rect(0).width, 96.0);
        assert_eq!(rect(3).width, 96.0);
    }

    #[test]
    fn fixed_table_with_auto_width_uses_column_intrinsic_tracks() {
        let column = || {
            Component::boxed(
                Style {
                    display: WDisp::TableColumn,
                    width: WDim::Px(40.0),
                    ..Style::default()
                },
                Vec::new(),
            )
        };
        let cell = || {
            Component::boxed(
                Style {
                    display: WDisp::TableCell,
                    ..Style::default()
                },
                Vec::new(),
            )
        };
        let table = Component::boxed(
            Style {
                display: WDisp::Table,
                table_layout_fixed: true,
                ..Style::default()
            },
            vec![
                column(),
                column(),
                column(),
                Component::row(
                    Style {
                        display: WDisp::TableRow,
                        ..Style::default()
                    },
                    vec![cell(), cell(), cell()],
                ),
            ],
        );

        assert_eq!(fixed_table_track_widths(&table, None), None);
        assert_eq!(table_track_widths(&table), vec![40.0, 40.0, 40.0]);
        let layout = compute(&table, 800.0, 600.0).unwrap();
        let rect = |index| layout.iter().find(|(_, item)| *item == index).unwrap().0;
        assert_eq!(rect(0).width, 120.0);
        assert_eq!((rect(5).x, rect(5).width), (0.0, 40.0));
        assert_eq!((rect(6).x, rect(6).width), (40.0, 40.0));
        assert_eq!((rect(7).x, rect(7).width), (80.0, 40.0));
    }

    #[test]
    fn fixed_table_percentage_column_uses_inner_grid_width() {
        let column = |width| {
            Component::boxed(
                Style {
                    display: WDisp::TableColumn,
                    width,
                    ..Style::default()
                },
                vec![],
            )
        };
        let cell = || {
            Component::boxed(
                Style {
                    display: WDisp::TableCell,
                    ..Style::default()
                },
                vec![],
            )
        };
        let table = Component::boxed(
            Style {
                display: WDisp::Table,
                table_layout_fixed: true,
                width: WDim::Px(422.0),
                border_left_width: Some(6.0),
                border_right_width: Some(6.0),
                border_spacing_x: 2.0,
                ..Style::default()
            },
            vec![
                column(WDim::Auto),
                column(WDim::Auto),
                column(WDim::Percent(40.0)),
                column(WDim::Auto),
                Component::row(
                    Style {
                        display: WDisp::TableRow,
                        ..Style::default()
                    },
                    vec![cell(), cell(), cell(), cell()],
                ),
            ],
        );

        assert_eq!(
            fixed_table_track_widths(&table, None),
            Some(vec![80.0, 80.0, 160.0, 80.0])
        );
        let layout = compute(&table, 800.0, 600.0).unwrap();
        let rect = |index| layout.iter().find(|(_, item)| *item == index).unwrap().0;

        assert_eq!((rect(3).x, rect(3).width), (172.0, 160.0));
        assert_eq!((rect(8).x, rect(8).width), (172.0, 160.0));
    }

    #[test]
    fn fixed_html_table_tracks_exclude_the_outer_border() {
        let cell = Component::boxed(
            Style {
                display: WDisp::TableCell,
                ..Style::default()
            },
            Vec::new(),
        );
        let table = Component::boxed(
            Style {
                display: WDisp::Table,
                table_layout_fixed: true,
                width: WDim::Px(256.0),
                border_width: 3.0,
                custom_properties: Some(HashMap::from([(
                    "--w3cos-internal-html-table-element".to_string(),
                    "1".to_string(),
                )])),
                ..Style::default()
            },
            vec![Component::row(
                Style {
                    display: WDisp::TableRow,
                    ..Style::default()
                },
                vec![cell],
            )],
        );

        assert_eq!(fixed_table_track_widths(&table, None), Some(vec![250.0]));
    }

    #[test]
    fn fixed_percentage_table_resolves_against_containing_block() {
        let column = |width| {
            Component::boxed(
                Style {
                    display: WDisp::TableColumn,
                    width,
                    ..Style::default()
                },
                vec![],
            )
        };
        let table = Component::boxed(
            Style {
                display: WDisp::Table,
                table_layout_fixed: true,
                width: WDim::Percent(80.0),
                border_left_width: Some(11.0),
                border_right_width: Some(11.0),
                border_spacing_x: 18.0,
                ..Style::default()
            },
            vec![
                column(WDim::Percent(13.0)),
                column(WDim::Px(100.0)),
                column(WDim::Percent(31.0)),
                column(WDim::Auto),
            ],
        );

        assert_eq!(
            fixed_table_track_widths(&table, Some(640.0)),
            Some(vec![54.86, 100.0, 130.82, 136.32])
        );
    }

    #[test]
    fn fixed_separate_table_padding_expands_principal_box() {
        let table = Component::boxed(
            Style {
                display: WDisp::Table,
                table_layout_fixed: true,
                width: WDim::Px(50.0),
                padding: w3cos_std::style::Edges::all(25.0),
                ..Style::default()
            },
            vec![Component::row(
                Style {
                    display: WDisp::TableRow,
                    ..Style::default()
                },
                vec![Component::boxed(
                    Style {
                        display: WDisp::TableCell,
                        height: WDim::Px(50.0),
                        ..Style::default()
                    },
                    vec![],
                )],
            )],
        );

        let layout = compute(&table, 800.0, 600.0).unwrap();
        let table_rect = layout.iter().find(|(_, item)| *item == 0).unwrap().0;
        assert_eq!((table_rect.width, table_rect.height), (100.0, 100.0));
    }

    #[test]
    fn separate_table_spaces_direct_rows_on_the_block_axis() {
        let row = || {
            Component::row(
                Style {
                    display: WDisp::TableRow,
                    height: WDim::Px(40.0),
                    ..Style::default()
                },
                vec![],
            )
        };
        let table = Component::boxed(
            Style {
                display: WDisp::Table,
                table_layout_fixed: true,
                width: WDim::Px(100.0),
                padding: w3cos_std::style::Edges {
                    top: WSpacing::Px(5.0),
                    ..w3cos_std::style::Edges::ZERO
                },
                border_spacing_y: 5.0,
                ..Style::default()
            },
            vec![row(), row()],
        );

        let layout = compute(&table, 800.0, 600.0).unwrap();
        let table_rect = layout.iter().find(|(_, item)| *item == 0).unwrap().0;
        assert_eq!(table_rect.height, 100.0);
    }

    #[test]
    fn fixed_css_table_expands_to_column_and_spacing_minimum() {
        let cell = || {
            Component::boxed(
                Style {
                    display: WDisp::TableCell,
                    width: WDim::Px(20.0),
                    height: WDim::Px(20.0),
                    ..Style::default()
                },
                vec![],
            )
        };
        let table = Component::boxed(
            Style {
                display: WDisp::Table,
                table_layout_fixed: true,
                width: WDim::Px(70.0),
                border_spacing_x: 20.0,
                ..Style::default()
            },
            vec![Component::row(
                Style {
                    display: WDisp::TableRow,
                    ..Style::default()
                },
                vec![cell(), cell()],
            )],
        );

        let layout = compute(&table, 800.0, 600.0).unwrap();
        let table_rect = layout.iter().find(|(_, item)| *item == 0).unwrap().0;
        assert_eq!(table_rect.width, 100.0);
    }

    #[test]
    fn automatic_table_layout_distributes_explicit_grid_width_to_tracks() {
        let table = Component::boxed(
            Style {
                display: WDisp::Table,
                width: WDim::Px(200.0),
                border_left_width: Some(100.0),
                border_right_width: Some(100.0),
                padding: w3cos_std::style::Edges::xy(33.0, 0.0),
                border_spacing_x: 52.0,
                ..Style::default()
            },
            vec![Component::row(
                Style {
                    display: WDisp::TableRow,
                    ..Style::default()
                },
                vec![Component::boxed(
                    Style {
                        display: WDisp::TableCell,
                        height: WDim::Px(16.0),
                        ..Style::default()
                    },
                    vec![],
                )],
            )],
        );

        let layout = compute(&table, 800.0, 600.0).unwrap();
        let cell_rect = layout.iter().find(|(_, item)| *item == 2).unwrap().0;
        assert_eq!(cell_rect.width, 96.0);
    }

    #[test]
    fn collapsed_fixed_table_tracks_share_one_sided_cell_border() {
        let column = || {
            Component::boxed(
                Style {
                    display: WDisp::TableColumn,
                    ..Style::default()
                },
                vec![],
            )
        };
        let cell = |width, border_left_width| {
            Component::boxed(
                Style {
                    display: WDisp::TableCell,
                    border_collapse: true,
                    width,
                    padding: w3cos_std::style::Edges::xy(24.0, 0.0),
                    border_left_width: Some(border_left_width),
                    ..Style::default()
                },
                vec![],
            )
        };
        let table = Component::boxed(
            Style {
                display: WDisp::Table,
                border_collapse: true,
                table_layout_fixed: true,
                width: WDim::Px(400.0),
                ..Style::default()
            },
            vec![
                column(),
                column(),
                column(),
                Component::row(
                    Style {
                        display: WDisp::TableRow,
                        border_collapse: true,
                        ..Style::default()
                    },
                    vec![
                        cell(WDim::Auto, 0.0),
                        cell(WDim::Px(80.0), 72.0),
                        cell(WDim::Auto, 0.0),
                    ],
                ),
            ],
        );

        assert_eq!(
            fixed_table_track_widths(&table, None),
            Some(vec![118.0, 164.0, 118.0])
        );
        let layout = compute(&table, 800.0, 600.0).unwrap();
        let rect = |index| layout.iter().find(|(_, item)| *item == index).unwrap().0;

        assert_eq!((rect(6).x, rect(6).width), (82.0, 200.0));
    }

    #[test]
    fn collapsed_fixed_table_uses_shared_border_halves_for_cell_content() {
        let column = || {
            Component::boxed(
                Style {
                    display: WDisp::TableColumn,
                    ..Style::default()
                },
                vec![],
            )
        };
        let empty_cell = || {
            Component::boxed(
                Style {
                    display: WDisp::TableCell,
                    border_collapse: true,
                    ..Style::default()
                },
                vec![],
            )
        };
        let tested_cell = Component::boxed(
            Style {
                display: WDisp::TableCell,
                border_collapse: true,
                width: WDim::Px(80.0),
                border_left_width: Some(60.0),
                border_right_width: Some(60.0),
                font_size: 20.0,
                line_height: 1.2,
                ..Style::default()
            },
            vec![Component::text(
                "F01",
                Style {
                    font_size: 20.0,
                    line_height: 1.2,
                    ..Style::default()
                },
            )],
        );
        let table = Component::boxed(
            Style {
                display: WDisp::Table,
                border_collapse: true,
                table_layout_fixed: true,
                width: WDim::Px(400.0),
                ..Style::default()
            },
            vec![
                column(),
                column(),
                column(),
                Component::row(
                    Style {
                        display: WDisp::TableRow,
                        border_collapse: true,
                        ..Style::default()
                    },
                    vec![empty_cell(), tested_cell, empty_cell()],
                ),
            ],
        );

        assert_eq!(
            fixed_table_track_widths(&table, None),
            Some(vec![130.0, 140.0, 130.0])
        );
        let layout = compute(&table, 800.0, 600.0).unwrap();
        let rect = |index| layout.iter().find(|(_, item)| *item == index).unwrap().0;

        assert_eq!(
            (rect(6).x, rect(6).width, rect(6).height),
            (100.0, 200.0, 24.0)
        );
        assert_eq!((rect(7).x, rect(7).height), (160.0, 24.0));
    }

    #[test]
    fn collapsed_equal_cell_borders_share_grid_lines() {
        let cell = || {
            Component::boxed(
                Style {
                    display: WDisp::TableCell,
                    border_collapse: true,
                    border_width: 20.0,
                    padding: w3cos_std::style::Edges::ZERO,
                    ..Style::default()
                },
                vec![],
            )
        };
        let row = || {
            Component::row(
                Style {
                    display: WDisp::TableRow,
                    border_collapse: true,
                    ..Style::default()
                },
                vec![cell(), cell(), cell(), cell()],
            )
        };
        let table = Component::boxed(
            Style {
                display: WDisp::Table,
                border_collapse: true,
                ..Style::default()
            },
            vec![row(), row(), row(), row()],
        );

        let layout = compute(&table, 800.0, 600.0).unwrap();
        let table_rect = layout.iter().find(|(_, index)| *index == 0).unwrap().0;
        assert_eq!((table_rect.width, table_rect.height), (100.0, 100.0));
    }

    #[test]
    fn collapsed_tracks_keep_cell_content_between_shared_border_halves() {
        let cell = || {
            Component::boxed(
                Style {
                    display: WDisp::TableCell,
                    border_collapse: true,
                    border_width: 20.0,
                    padding: w3cos_std::style::Edges::ZERO,
                    ..Style::default()
                },
                vec![Component::boxed(
                    Style {
                        width: WDim::Px(20.0),
                        height: WDim::Px(20.0),
                        ..Style::default()
                    },
                    vec![],
                )],
            )
        };
        let row = Component::row(
            Style {
                display: WDisp::TableRow,
                border_collapse: true,
                ..Style::default()
            },
            vec![cell(), cell(), cell(), cell()],
        );
        let table = Component::boxed(
            Style {
                display: WDisp::Table,
                border_collapse: true,
                ..Style::default()
            },
            vec![row],
        );

        let layout = compute(&table, 800.0, 600.0).unwrap();
        let rect = |index| layout.iter().find(|(_, i)| *i == index).unwrap().0;
        assert_eq!(
            (rect(0).width, rect(2).width, rect(4).x),
            (180.0, 60.0, 40.0)
        );
    }

    #[test]
    fn collapsed_empty_row_only_absorbs_its_own_height() {
        let populated_row = || {
            Component::row(
                Style {
                    display: WDisp::TableRow,
                    border_collapse: true,
                    ..Style::default()
                },
                vec![Component::boxed(
                    Style {
                        display: WDisp::TableCell,
                        border_collapse: true,
                        border_width: 10.0,
                        padding: w3cos_std::style::Edges::ZERO,
                        ..Style::default()
                    },
                    vec![Component::boxed(
                        Style {
                            width: WDim::Px(10.0),
                            height: WDim::Px(10.0),
                            ..Style::default()
                        },
                        vec![],
                    )],
                )],
            )
        };
        let empty_row = Component::row(
            Style {
                display: WDisp::TableRow,
                border_collapse: true,
                height: WDim::Px(2.0),
                ..Style::default()
            },
            vec![],
        );
        let table = Component::boxed(
            Style {
                display: WDisp::Table,
                border_collapse: true,
                ..Style::default()
            },
            vec![Component::boxed(
                Style {
                    display: WDisp::TableRowGroup,
                    border_collapse: true,
                    ..Style::default()
                },
                vec![populated_row(), empty_row, populated_row()],
            )],
        );

        let layout = compute(&table, 800.0, 600.0).unwrap();
        let rect = |index| layout.iter().find(|(_, i)| *i == index).unwrap().0;
        assert_eq!((rect(2).y, rect(5).y, rect(6).y), (0.0, 20.0, 20.0));
        assert_eq!(rect(0).height, 50.0);
    }

    #[test]
    fn collapsed_row_group_consumes_no_table_height() {
        let group = |visibility| {
            Component::boxed(
                Style {
                    display: WDisp::TableRowGroup,
                    visibility,
                    ..Style::default()
                },
                vec![Component::row(
                    Style {
                        display: WDisp::TableRow,
                        visibility,
                        ..Style::default()
                    },
                    vec![Component::boxed(
                        Style {
                            display: WDisp::TableCell,
                            visibility,
                            width: WDim::Px(100.0),
                            height: WDim::Px(100.0),
                            ..Style::default()
                        },
                        vec![],
                    )],
                )],
            )
        };
        let table = Component::boxed(
            Style {
                display: WDisp::Table,
                ..Style::default()
            },
            vec![group(WVisibility::Collapse), group(WVisibility::Visible)],
        );

        let layout = compute(&table, 800.0, 600.0).unwrap();
        let rect = |index| layout.iter().find(|(_, item)| *item == index).unwrap().0;
        assert_eq!(
            (rect(0).height, rect(6).y, rect(6).height),
            (100.0, 0.0, 100.0)
        );
    }

    #[test]
    fn single_node_has_size() {
        let l = compute(&Component::text("Hi", s()), 800.0, 600.0).unwrap();
        assert_eq!(l.len(), 1);
        assert!(l[0].0.width > 0.0);
    }

    #[test]
    fn viewport_and_font_relative_units_use_web_reference_sizes() {
        let component = Component::boxed(
            Style {
                width: WDim::Vw(50.0),
                height: WDim::Vh(25.0),
                min_width: WDim::Em(10.0),
                font_size: 20.0,
                ..Style::default()
            },
            Vec::new(),
        );

        let layout = compute(&component, 800.0, 600.0).unwrap();

        assert_eq!(layout[0].0.width, 400.0);
        assert_eq!(layout[0].0.height, 150.0);
    }

    #[test]
    fn flex_basis_is_forwarded_to_taffy() {
        let style = to_taffy_style(
            &Style {
                flex_basis: WDim::Percent(25.0),
                ..Style::default()
            },
            400.0,
            800.0,
        );

        assert_eq!(style.flex_basis, Dimension::percent(0.25));
    }

    #[test]
    fn css_box_sizing_controls_declared_outer_size() {
        let make_box = |box_sizing| {
            Component::boxed(
                Style {
                    box_sizing,
                    width: WDim::Px(100.0),
                    height: WDim::Px(40.0),
                    padding: w3cos_std::style::Edges::all(10.0),
                    border_width: 2.0,
                    ..Style::default()
                },
                Vec::new(),
            )
        };

        let content_box = compute(
            &make_box(w3cos_std::style::BoxSizing::ContentBox),
            400.0,
            400.0,
        )
        .unwrap();
        let border_box = compute(
            &make_box(w3cos_std::style::BoxSizing::BorderBox),
            400.0,
            400.0,
        )
        .unwrap();

        assert_eq!(content_box[0].0.width, 124.0);
        assert_eq!(content_box[0].0.height, 64.0);
        assert_eq!(border_box[0].0.width, 100.0);
        assert_eq!(border_box[0].0.height, 40.0);

        let bottom_border_only = compute(
            &Component::boxed(
                Style {
                    width: WDim::Px(100.0),
                    height: WDim::Px(40.0),
                    border_bottom_width: Some(3.0),
                    ..Style::default()
                },
                Vec::new(),
            ),
            400.0,
            400.0,
        )
        .unwrap();
        assert_eq!(bottom_border_only[0].0.width, 100.0);
        assert_eq!(bottom_border_only[0].0.height, 43.0);
    }

    #[test]
    fn display_table_preserves_authored_content_box_sizing() {
        let table = Component::boxed(
            Style {
                display: WDisp::Table,
                width: WDim::Px(100.0),
                height: WDim::Px(100.0),
                border_width: 10.0,
                ..Style::default()
            },
            Vec::new(),
        );

        let layout = compute(&table, 800.0, 600.0).unwrap();

        assert_eq!(layout[0].0.width, 120.0);
        assert_eq!(layout[0].0.height, 120.0);
    }

    #[test]
    fn modern_grid_tracks_span_and_flex_order_match_css() {
        let spanning = Component::boxed(
            Style {
                grid_column: Some("1 / -1".to_string()),
                height: WDim::Px(20.0),
                ..Style::default()
            },
            Vec::new(),
        );
        let grid = Component::column(
            Style {
                display: WDisp::Grid,
                width: WDim::Px(210.0),
                grid_template_columns: Some("1fr 1fr".to_string()),
                column_gap: Some(10.0),
                ..Style::default()
            },
            vec![
                spanning,
                Component::boxed(
                    Style {
                        height: WDim::Px(20.0),
                        ..Style::default()
                    },
                    Vec::new(),
                ),
                Component::boxed(
                    Style {
                        height: WDim::Px(20.0),
                        ..Style::default()
                    },
                    Vec::new(),
                ),
            ],
        );
        let grid_layout = compute(&grid, 210.0, 200.0).unwrap();
        assert_eq!(grid_layout[1].0.width, 210.0);
        assert_eq!(grid_layout[2].0.width, 100.0);
        assert_eq!(grid_layout[3].0.x, 110.0);

        let ordered = Component::row(
            Style {
                width: WDim::Px(200.0),
                ..Style::default()
            },
            vec![
                Component::boxed(
                    Style {
                        order: 1,
                        width: WDim::Px(80.0),
                        ..Style::default()
                    },
                    Vec::new(),
                ),
                Component::boxed(
                    Style {
                        order: 0,
                        width: WDim::Px(80.0),
                        ..Style::default()
                    },
                    Vec::new(),
                ),
            ],
        );
        let ordered_layout = compute(&ordered, 200.0, 100.0).unwrap();
        let first_source_child = ordered_layout
            .iter()
            .find(|(_, index)| *index == 1)
            .unwrap()
            .0;
        let second_source_child = ordered_layout
            .iter()
            .find(|(_, index)| *index == 2)
            .unwrap()
            .0;
        assert_eq!(second_source_child.x, 0.0);
        assert_eq!(first_source_child.x, 80.0);
    }

    #[test]
    fn single_grid_line_places_item_in_requested_column() {
        let grid = Component::column(
            Style {
                display: WDisp::Grid,
                width: WDim::Px(210.0),
                grid_template_columns: Some("34px minmax(0, 1fr)".to_string()),
                column_gap: Some(10.0),
                ..Style::default()
            },
            vec![
                Component::boxed(
                    Style {
                        grid_column: Some("2".to_string()),
                        height: WDim::Px(20.0),
                        ..Style::default()
                    },
                    Vec::new(),
                ),
                Component::boxed(
                    Style {
                        grid_column: Some("1".to_string()),
                        height: WDim::Px(20.0),
                        ..Style::default()
                    },
                    Vec::new(),
                ),
            ],
        );
        let layout = compute(&grid, 210.0, 100.0).unwrap();
        assert_eq!(layout[1].0.x, 44.0);
        assert_eq!(layout[2].0.x, 0.0);
    }

    #[test]
    fn grid_repeat_uses_custom_property_fallback_count() {
        let tracks =
            parse_grid_template_columns("repeat(var(--schema-grid-columns, 12), minmax(0, 1fr))");
        assert_eq!(tracks.len(), 12);
    }

    #[test]
    fn persistent_layout_rebuilds_viewport_units_after_resize() {
        let component = Component::boxed(
            Style {
                width: WDim::Vw(50.0),
                height: WDim::Vh(50.0),
                ..Style::default()
            },
            Vec::new(),
        );
        let mut engine = LayoutEngine::new();
        let flat = pre_flatten(&component);

        let initial = engine.compute(&component, &flat, 800.0, 600.0).unwrap();
        let resized = engine.compute(&component, &flat, 400.0, 300.0).unwrap();

        assert_eq!(initial.layout_cache[0].0.width, 400.0);
        assert_eq!(initial.layout_cache[0].0.height, 300.0);
        assert_eq!(resized.layout_cache[0].0.width, 200.0);
        assert_eq!(resized.layout_cache[0].0.height, 150.0);
    }

    #[test]
    fn root_at_origin() {
        let l = compute(&Component::text("R", s()), 800.0, 600.0).unwrap();
        assert_eq!(l[0].0.x, 0.0);
        assert_eq!(l[0].0.y, 0.0);
    }

    #[test]
    fn root_margins_offset_and_reduce_the_initial_layout_space() {
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                margin: w3cos_std::style::Edges::all(10.0),
                ..Style::default()
            },
            Vec::new(),
        );
        let layout = compute(&root, 800.0, 600.0).unwrap();
        assert_eq!(layout[0].0.x, 10.0);
        assert_eq!(layout[0].0.y, 10.0);
        assert_eq!(layout[0].0.width, 780.0);
    }

    #[test]
    fn root_relative_insets_offset_the_root_and_its_descendants() {
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                position: WPos::Relative,
                left: WDim::Px(100.0),
                top: WDim::Px(100.0),
                ..Style::default()
            },
            vec![Component::text("child", s())],
        );
        let layout = compute(&root, 800.0, 600.0).unwrap();
        assert_eq!((layout[0].0.x, layout[0].0.y), (100.0, 100.0));
        assert_eq!((layout[1].0.x, layout[1].0.y), (100.0, 100.0));
    }

    #[test]
    fn static_insets_are_preserved_but_do_not_offset_layout() {
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                position: WPos::Static,
                left: WDim::Px(100.0),
                top: WDim::Px(100.0),
                ..Style::default()
            },
            vec![Component::text("child", s())],
        );
        let layout = compute(&root, 800.0, 600.0).unwrap();

        assert_eq!((layout[0].0.x, layout[0].0.y), (0.0, 0.0));
        assert_eq!((layout[1].0.x, layout[1].0.y), (0.0, 0.0));
    }

    #[test]
    fn root_left_auto_margin_uses_the_initial_containing_block() {
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                width: WDim::Px(100.0),
                margin: w3cos_std::style::Edges {
                    left: WSpacing::Auto,
                    ..w3cos_std::style::Edges::ZERO
                },
                ..Style::default()
            },
            vec![Component::text("root", s())],
        );
        let layout = compute(&root, 800.0, 600.0).unwrap();
        assert_eq!(layout[0].0.x, 700.0);
        assert_eq!(layout[1].0.x, 700.0);
    }

    #[test]
    fn positioned_percentage_sizes_use_their_css_containing_blocks() {
        let style = Style {
            position: WPos::Absolute,
            left: WDim::Px(0.0),
            top: WDim::Px(0.0),
            width: WDim::Percent(100.0),
            height: WDim::Percent(100.0),
            ..Style::default()
        };
        let containing_block = LayoutRect {
            x: 10.0,
            y: 20.0,
            width: 100.0,
            height: 80.0,
        };
        let absolute = compute_absolute_rect(
            &style,
            containing_block,
            LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
            },
            800.0,
            600.0,
            style.direction,
            false,
        );
        assert_eq!(absolute, containing_block);

        let fixed = compute_fixed_rect(
            &style,
            800.0,
            600.0,
            LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
            },
            false,
        );
        assert_eq!((fixed.width, fixed.height), (800.0, 600.0));

        let content_box_with_border = Style {
            width: WDim::Percent(50.0),
            height: WDim::Percent(50.0),
            border_width: 10.0,
            ..style
        };
        let fixed = compute_fixed_rect(
            &content_box_with_border,
            800.0,
            600.0,
            LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
            },
            false,
        );
        assert_eq!((fixed.width, fixed.height), (420.0, 320.0));

        let static_position = compute_fixed_rect(
            &Style {
                position: WPos::Fixed,
                width: WDim::Px(50.0),
                height: WDim::Px(50.0),
                ..Style::default()
            },
            800.0,
            600.0,
            LayoutRect {
                x: 58.0,
                y: 101.2,
                width: 50.0,
                height: 50.0,
            },
            false,
        );
        assert_eq!((static_position.x, static_position.y), (58.0, 101.2));
    }

    #[test]
    fn bordered_positioned_block_uses_its_padding_box_for_absolute_children() {
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                position: WPos::Relative,
                width: WDim::Px(150.0),
                height: WDim::Px(50.0),
                border_width: 1.0,
                ..Style::default()
            },
            vec![Component::boxed(
                Style {
                    display: WDisp::Block,
                    position: WPos::Absolute,
                    left: WDim::Px(-15.0),
                    top: WDim::Px(0.0),
                    width: WDim::Px(0.0),
                    height: WDim::Px(50.0),
                    border_left_width: Some(10.0),
                    ..Style::default()
                },
                Vec::new(),
            )],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let container = layout.iter().find(|(_, index)| *index == 0).unwrap().0;
        let absolute = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        assert_eq!(absolute.x, container.x + 1.0 - 15.0);
        assert_eq!(absolute.y, container.y + 1.0);
    }

    #[test]
    fn absolute_auto_size_stretches_between_opposing_insets() {
        let style = Style {
            position: WPos::Absolute,
            top: WDim::Px(10.0),
            right: WDim::Px(30.0),
            bottom: WDim::Px(10.0),
            left: WDim::Px(10.0),
            ..Style::default()
        };
        let containing_block = LayoutRect {
            x: 16.0,
            y: 51.0,
            width: 120.0,
            height: 120.0,
        };
        assert_eq!(
            compute_absolute_rect(
                &style,
                containing_block,
                LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: 0.0,
                    height: 0.0,
                },
                800.0,
                600.0,
                style.direction,
                false,
            ),
            LayoutRect {
                x: 26.0,
                y: 61.0,
                width: 80.0,
                height: 100.0,
            }
        );
    }

    #[test]
    fn absolute_insets_position_the_margin_box() {
        let containing_block = LayoutRect {
            x: 10.0,
            y: 20.0,
            width: 200.0,
            height: 160.0,
        };
        let fallback = LayoutRect {
            x: 0.0,
            y: 0.0,
            width: 40.0,
            height: 30.0,
        };
        let start = Style {
            position: WPos::Absolute,
            left: WDim::Px(5.0),
            top: WDim::Px(7.0),
            margin: w3cos_std::style::Edges {
                top: WSpacing::Px(11.0),
                left: WSpacing::Percent(10.0),
                ..w3cos_std::style::Edges::ZERO
            },
            ..Style::default()
        };
        let rect = compute_absolute_rect(
            &start, containing_block, fallback, 800.0, 600.0, start.direction, false,
        );
        assert_eq!((rect.x, rect.y), (35.0, 38.0));

        let end = Style {
            position: WPos::Absolute,
            right: WDim::Px(5.0),
            bottom: WDim::Px(7.0),
            margin: w3cos_std::style::Edges {
                right: WSpacing::Px(13.0),
                bottom: WSpacing::Percent(10.0),
                ..w3cos_std::style::Edges::ZERO
            },
            ..Style::default()
        };
        let rect = compute_absolute_rect(
            &end, containing_block, fallback, 800.0, 600.0, end.direction, false,
        );
        assert_eq!((rect.x, rect.y), (152.0, 123.0));
    }

    #[test]
    fn auto_inset_absolute_inline_uses_the_preceding_inline_static_position() {
        let text_style = Style {
            display: WDisp::Inline,
            font_size: 10.0,
            line_height: 10.0,
            ..Style::default()
        };
        let expected_x = text_intrinsic_size("12345", &text_style).0;
        let absolute = Component::text(
            "span",
            Style {
                display: WDisp::Inline,
                position: WPos::Absolute,
                font_size: 10.0,
                line_height: 10.0,
                ..Style::default()
            },
        );
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                position: WPos::Relative,
                width: WDim::Px(100.0),
                border_width: 1.0,
                ..Style::default()
            },
            vec![Component::text("12345", text_style), absolute],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        assert_eq!(layout[2].0.x - layout[0].0.x - 1.0, expected_x);
        assert_eq!(layout[2].0.y - layout[0].0.y - 1.0, 0.0);
    }

    #[test]
    fn auto_inset_absolute_block_keeps_its_own_top_margin() {
        let absolute = Component::boxed(
            Style {
                display: WDisp::Block,
                position: WPos::Absolute,
                width: WDim::Px(100.0),
                height: WDim::Px(40.0),
                margin: w3cos_std::style::Edges {
                    top: WSpacing::Px(40.0),
                    ..w3cos_std::style::Edges::ZERO
                },
                ..Style::default()
            },
            Vec::new(),
        );
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                position: WPos::Relative,
                width: WDim::Px(100.0),
                height: WDim::Px(80.0),
                ..Style::default()
            },
            vec![absolute],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        assert_eq!(layout[1].0.y, 40.0);
    }

    #[test]
    fn absolute_vertical_auto_margins_solve_the_remaining_space() {
        for (top_margin, bottom_margin, containing_height, expected_y) in [
            (WSpacing::Auto, WSpacing::Auto, 288.0, 96.0),
            (WSpacing::Auto, WSpacing::Px(48.0), 288.0, 96.0),
            (WSpacing::Px(48.0), WSpacing::Auto, 288.0, 96.0),
            (WSpacing::Auto, WSpacing::Auto, 144.0, 24.0),
        ] {
            let style = Style {
                position: WPos::Absolute,
                top: WDim::Px(48.0),
                bottom: WDim::Px(48.0),
                height: WDim::Px(96.0),
                margin: w3cos_std::style::Edges {
                    top: top_margin,
                    bottom: bottom_margin,
                    ..w3cos_std::style::Edges::ZERO
                },
                ..Style::default()
            };
            let rect = compute_absolute_rect(
                &style,
                LayoutRect {
                    x: 0.0,
                    y: 10.0,
                    width: 288.0,
                    height: containing_height,
                },
                LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: 288.0,
                    height: 96.0,
                },
                800.0,
                600.0,
                style.direction,
                false,
            );
            assert_eq!(rect.y, 10.0 + expected_y);
        }
    }

    #[test]
    fn absolute_auto_height_reenters_margin_equation_after_max_height() {
        let style = Style {
            position: WPos::Absolute,
            top: WDim::Px(96.0),
            bottom: WDim::Px(96.0),
            max_height: WDim::Px(48.0),
            margin: w3cos_std::style::Edges {
                top: WSpacing::Auto,
                bottom: WSpacing::Auto,
                ..w3cos_std::style::Edges::ZERO
            },
            ..Style::default()
        };
        let containing = LayoutRect {
            x: 0.0,
            y: 0.0,
            width: 288.0,
            height: 288.0,
        };
        let rect = compute_absolute_rect(
            &style, containing, containing, 800.0, 600.0, style.direction, false,
        );
        assert_eq!((rect.y, rect.height), (120.0, 48.0));
    }

    #[test]
    fn absolute_horizontal_auto_margins_follow_direction_for_negative_space() {
        use w3cos_std::style::TextDirection;
        for (direction, containing_width, right, expected_x) in [
            (TextDirection::Ltr, 400.0, -200.0, 300.0),
            (TextDirection::Ltr, 200.0, 100.0, 100.0),
            (TextDirection::Rtl, 200.0, 100.0, 0.0),
        ] {
            let style = Style {
                // The containing block, not the positioned child's own
                // overridden direction, governs this constraint equation.
                direction: match direction {
                    TextDirection::Ltr => TextDirection::Rtl,
                    TextDirection::Rtl => TextDirection::Ltr,
                },
                position: WPos::Absolute,
                left: WDim::Px(100.0),
                right: WDim::Px(right),
                width: WDim::Px(100.0),
                margin: w3cos_std::style::Edges {
                    left: WSpacing::Auto,
                    right: WSpacing::Auto,
                    ..w3cos_std::style::Edges::ZERO
                },
                ..Style::default()
            };
            let containing = LayoutRect {
                x: 10.0,
                y: 0.0,
                width: containing_width,
                height: 200.0,
            };
            let fallback = LayoutRect {
                width: 100.0,
                ..containing
            };
            let rect = compute_absolute_rect(&style, containing, fallback, 800.0, 600.0, direction, false);
            assert_eq!(rect.x, 10.0 + expected_x);
        }
    }

    #[test]
    fn absolute_auto_width_reenters_margin_equation_after_max_width() {
        for (right_margin, min_width, expected_x, expected_width) in [
            (WSpacing::Auto, WDim::Auto, 350.0, 100.0),
            (WSpacing::Px(0.0), WDim::Auto, 692.0, 100.0),
            (WSpacing::Auto, WDim::Px(120.0), 340.0, 120.0),
        ] {
            let style = Style {
                position: WPos::Absolute,
                left: WDim::Px(8.0),
                right: WDim::Px(8.0),
                max_width: WDim::Px(100.0),
                min_width,
                margin: w3cos_std::style::Edges {
                    left: WSpacing::Auto,
                    right: right_margin,
                    ..w3cos_std::style::Edges::ZERO
                },
                ..Style::default()
            };
            let containing = LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 800.0,
                height: 600.0,
            };
            let rect = compute_absolute_rect(
                &style, containing, containing, 800.0, 600.0, style.direction, false,
            );
            assert_eq!((rect.x, rect.width), (expected_x, expected_width));
        }
    }

    #[test]
    fn positioned_replaced_auto_axes_keep_their_intrinsic_size_between_insets() {
        let style = Style {
            position: WPos::Absolute,
            left: WDim::Px(100.0),
            right: WDim::Px(100.0),
            top: WDim::Px(96.0),
            bottom: WDim::Px(96.0),
            ..Style::default()
        };
        let size = positioned_percentage_border_box_size(
            &style, 300.0, 0.0, 800.0, 600.0, 15.0, 15.0, true,
        );
        assert_eq!(size, (15.0, 15.0));
    }

    #[test]
    fn rtl_absolute_auto_insets_use_the_right_static_edge() {
        let root = Component::row(
            Style {
                display: WDisp::Block,
                position: WPos::Relative,
                direction: w3cos_std::style::TextDirection::Rtl,
                width: WDim::Px(200.0),
                height: WDim::Px(200.0),
                border_width: 3.0,
                ..Style::default()
            },
            vec![
                Component::boxed(
                    Style {
                        display: WDisp::Block,
                        height: WDim::Px(40.0),
                        ..Style::default()
                    },
                    Vec::new(),
                ),
                Component::row(
                    Style {
                        display: WDisp::Flex,
                        position: WPos::Absolute,
                        direction: w3cos_std::style::TextDirection::Rtl,
                        ..Style::default()
                    },
                    vec![Component::boxed(
                        Style {
                            display: WDisp::InlineBlock,
                            width: WDim::Px(100.0),
                            height: WDim::Px(100.0),
                            ..Style::default()
                        },
                        Vec::new(),
                    )],
                ),
            ],
        );
        let layout = compute(&root, 800.0, 600.0).unwrap();
        assert_eq!((layout[2].0.x, layout[2].0.y), (103.0, 43.0));
    }

    #[test]
    fn floated_text_leaf_shrink_fits_before_its_internal_out_of_flow_layout() {
        let root = Component::row(
            Style {
                display: WDisp::Block,
                position: WPos::Absolute,
                ..Style::default()
            },
            vec![Component::text(
                "WWWWWWWWWW",
                Style {
                    display: WDisp::Block,
                    float: WFloat::Left,
                    font_size: 30.0,
                    max_width: WDim::Px(120.0),
                    ..Style::default()
                },
            )],
        );
        let layout = compute(&root, 800.0, 600.0).unwrap();
        assert_eq!(layout[1].0.width, 120.0);
    }

    #[test]
    fn auto_inset_absolute_block_applies_negative_margin_after_flow_content() {
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                position: WPos::Relative,
                width: WDim::Px(150.0),
                ..Style::default()
            },
            vec![
                Component::boxed(
                    Style {
                        display: WDisp::Block,
                        height: WDim::Px(20.0),
                        ..Style::default()
                    },
                    Vec::new(),
                ),
                Component::boxed(
                    Style {
                        display: WDisp::Block,
                        position: WPos::Absolute,
                        width: WDim::Px(150.0),
                        height: WDim::Px(150.0),
                        margin: w3cos_std::style::Edges {
                            top: WSpacing::Px(-2.0),
                            right: WSpacing::Px(-2.0),
                            bottom: WSpacing::Px(-2.0),
                            left: WSpacing::Px(-2.0),
                        },
                        ..Style::default()
                    },
                    Vec::new(),
                ),
            ],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let absolute = layout.iter().find(|(_, index)| *index == 2).unwrap().0;
        assert_eq!((absolute.x, absolute.y), (-2.0, 18.0));
    }

    #[test]
    fn non_breaking_space_contributes_to_absolute_static_line_height() {
        let line_style = Style {
            display: WDisp::Inline,
            font_size: 16.0,
            line_height: 1.25,
            ..Style::default()
        };
        let absolute = Component::boxed(
            Style {
                display: WDisp::Block,
                position: WPos::Absolute,
                width: WDim::Px(200.0),
                height: WDim::Px(200.0),
                ..Style::default()
            },
            Vec::new(),
        );
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                ..Style::default()
            },
            vec![Component::text("\u{00a0}", line_style), absolute],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let absolute = layout
            .iter()
            .find_map(|(rect, index)| (*index == 2).then_some(*rect))
            .expect("absolute layout");

        assert_eq!(absolute.y, 20.0);
    }

    #[test]
    fn auto_inset_absolute_inline_uses_the_line_after_a_forced_break() {
        let inline_style = Style {
            display: WDisp::Inline,
            font_size: 16.0,
            line_height: 1.2,
            ..Style::default()
        };
        let mut break_style = inline_style.clone();
        break_style.width = WDim::Px(0.0);
        break_style.height = WDim::Px(19.2);
        let absolute = Component::text(
            "Line 2",
            Style {
                display: WDisp::Inline,
                position: WPos::Absolute,
                padding: w3cos_std::style::Edges {
                    left: WSpacing::Px(100.0),
                    ..w3cos_std::style::Edges::ZERO
                },
                ..inline_style.clone()
            },
        );
        let outer = Component::boxed(
            Style {
                display: WDisp::Inline,
                padding: w3cos_std::style::Edges {
                    left: WSpacing::Px(100.0),
                    ..w3cos_std::style::Edges::ZERO
                },
                ..inline_style.clone()
            },
            vec![
                Component::text("Line 1", inline_style.clone()),
                Component::text("\u{2028}", break_style),
                Component::text(" ", inline_style.clone()),
                absolute,
            ],
        );
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                ..Style::default()
            },
            vec![outer],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let absolute = layout
            .iter()
            .find_map(|(rect, index)| (*index == 5).then_some(*rect))
            .expect("absolute inline layout");
        assert_eq!(absolute.x, text_intrinsic_size(" ", &inline_style).0);
        assert_eq!(absolute.y, 19.2);
    }

    #[test]
    fn standalone_forced_break_establishes_its_line_height_strut() {
        let forced_break = Component::text(
            "\u{2028}",
            Style {
                display: WDisp::Inline,
                width: WDim::Px(0.0),
                height: WDim::Px(200.0),
                font_size: 16.0,
                line_height: 12.5,
                ..Style::default()
            },
        );
        let following_block = Component::boxed(
            Style {
                display: WDisp::Block,
                width: WDim::Px(200.0),
                height: WDim::Px(200.0),
                ..Style::default()
            },
            vec![],
        );
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                ..Style::default()
            },
            vec![forced_break, following_block],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        assert_eq!(layout[1].0.width, 0.0);
        assert_eq!(layout[1].0.height, 200.0);
        assert_eq!(layout[2].0.y, 200.0);
    }

    #[test]
    fn block_inline_formatting_context_keeps_a_tall_line_height_strut() {
        let image = Component::image(
            "blue.png",
            Style {
                display: WDisp::InlineBlock,
                width: WDim::Px(15.0),
                height: WDim::Px(15.0),
                padding: w3cos_std::style::Edges {
                    left: WSpacing::Px(81.0),
                    ..w3cos_std::style::Edges::ZERO
                },
                ..Style::default()
            },
        );
        let root = Component::row(
            Style {
                display: WDisp::Block,
                width: WDim::Px(96.0),
                border_width: 3.0,
                font_size: 16.0,
                line_height: 6.0,
                ..Style::default()
            },
            vec![image],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let root = layout.iter().find(|(_, index)| *index == 0).unwrap().0;

        assert_eq!(root.height, 102.0);
    }

    #[test]
    fn definite_zero_height_block_does_not_expand_to_its_line_box() {
        let root = Component::row(
            Style {
                display: WDisp::Block,
                width: WDim::Px(100.0),
                height: WDim::Px(0.0),
                ..Style::default()
            },
            vec![Component::text(
                "overflowing text",
                Style {
                    display: WDisp::Inline,
                    width: WDim::Percent(100.0),
                    ..Style::default()
                },
            )],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let root = layout.iter().find(|(_, index)| *index == 0).unwrap().0;
        let text = layout.iter().find(|(_, index)| *index == 1).unwrap().0;

        assert_eq!(root.height, 0.0);
        assert!(text.height > root.height);
    }

    #[test]
    fn standalone_forced_break_moves_following_inline_box_to_next_line() {
        let inline_box = || {
            Component::boxed(
                Style {
                    display: WDisp::InlineBlock,
                    width: WDim::Px(52.0),
                    height: WDim::Px(22.0),
                    margin: w3cos_std::style::Edges {
                        left: WSpacing::Px(44.0),
                        ..w3cos_std::style::Edges::ZERO
                    },
                    ..Style::default()
                },
                vec![],
            )
        };
        let forced_break = Component::text(
            "\u{2028}",
            Style {
                display: WDisp::Inline,
                width: WDim::Px(0.0),
                height: WDim::Px(26.0),
                font_size: 22.0,
                line_height: 26.0 / 22.0,
                ..Style::default()
            },
        );
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                font_size: 22.0,
                line_height: 26.0 / 22.0,
                ..Style::default()
            },
            vec![inline_box(), forced_break, inline_box()],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let rect = |index| layout.iter().find(|(_, item)| *item == index).unwrap().0;

        assert_eq!((rect(1).x, rect(1).y), (44.0, 0.0));
        assert_eq!(rect(3).x, 44.0);
        assert!((rect(3).y - 26.0).abs() < 0.001);
        assert!(
            (rect(0).height - 52.0).abs() < 0.001,
            "projected parent height was {}",
            rect(0).height
        );
    }

    #[test]
    fn forced_break_centers_the_following_inline_line() {
        let inline_box = || {
            Component::boxed(
                Style {
                    display: WDisp::InlineBlock,
                    width: WDim::Px(100.0),
                    height: WDim::Px(20.0),
                    ..Style::default()
                },
                vec![],
            )
        };
        let forced_break = Component::text(
            "\u{2028}",
            Style {
                display: WDisp::Inline,
                width: WDim::Px(0.0),
                height: WDim::Px(20.0),
                ..Style::default()
            },
        );
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                width: WDim::Px(200.0),
                height: WDim::Px(200.0),
                text_align: w3cos_std::style::TextAlign::Center,
                justify_content: WJustify::Center,
                ..Style::default()
            },
            vec![inline_box(), forced_break, inline_box()],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        assert_eq!(layout[0].0.height, 200.0);
        let following = layout.iter().find(|(_, index)| *index == 3).unwrap().0;
        assert_eq!(following.x, 50.0);
        assert_eq!(following.y, 20.0);
    }

    #[test]
    fn forced_break_discards_collapsible_whitespace_at_the_next_line_start() {
        let forced_break = Component::text(
            "\u{2028}",
            Style {
                display: WDisp::Inline,
                width: WDim::Px(0.0),
                ..Style::default()
            },
        );
        let whitespace = Component::text(
            " ",
            Style {
                display: WDisp::Inline,
                ..Style::default()
            },
        );
        let inline_box = Component::boxed(
            Style {
                display: WDisp::InlineBlock,
                width: WDim::Px(15.0),
                height: WDim::Px(15.0),
                ..Style::default()
            },
            vec![],
        );
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                ..Style::default()
            },
            vec![forced_break, whitespace, inline_box],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let root_rect = layout.iter().find(|(_, index)| *index == 0).unwrap().0;
        let box_rect = layout.iter().find(|(_, index)| *index == 3).unwrap().0;
        assert_eq!(box_rect.x, root_rect.x);
    }

    #[test]
    fn block_static_position_follows_a_decorated_inline_fragment() {
        let absolute = Component::boxed(
            Style {
                display: WDisp::Block,
                position: WPos::Absolute,
                width: WDim::Px(100.0),
                height: WDim::Px(100.0),
                ..Style::default()
            },
            vec![],
        );
        let inline = Component::boxed(
            Style {
                display: WDisp::Inline,
                line_height: 6.25,
                margin: w3cos_std::style::Edges {
                    left: WSpacing::Px(-100.0),
                    ..w3cos_std::style::Edges::ZERO
                },
                border_left_width: Some(100.0),
                ..Style::default()
            },
            vec![absolute, Component::text("X", Style::default())],
        );
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                width: WDim::Px(100.0),
                height: WDim::Px(100.0),
                ..Style::default()
            },
            vec![inline],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        assert_eq!(layout[2].0.x, 0.0);
        assert_eq!(layout[2].0.y, 100.0);
    }

    #[test]
    fn auto_inset_absolute_inline_joins_the_anonymous_line_after_a_block() {
        let absolute = Component::boxed(
            Style {
                display: WDisp::Inline,
                position: WPos::Absolute,
                width: WDim::Px(100.0),
                height: WDim::Px(150.0),
                ..Style::default()
            },
            Vec::new(),
        );
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                width: WDim::Px(200.0),
                ..Style::default()
            },
            vec![
                Component::boxed(
                    Style {
                        display: WDisp::Block,
                        height: WDim::Px(50.0),
                        ..Style::default()
                    },
                    Vec::new(),
                ),
                Component::boxed(
                    Style {
                        display: WDisp::InlineBlock,
                        width: WDim::Px(100.0),
                        height: WDim::Px(150.0),
                        ..Style::default()
                    },
                    Vec::new(),
                ),
                Component::boxed(
                    Style {
                        display: WDisp::None,
                        height: WDim::Px(100.0),
                        ..Style::default()
                    },
                    Vec::new(),
                ),
                absolute,
            ],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let absolute = layout
            .iter()
            .find_map(|(rect, index)| (*index == 4).then_some(*rect))
            .expect("absolute layout");
        assert_eq!(
            absolute,
            LayoutRect {
                x: 100.0,
                y: 50.0,
                width: 100.0,
                height: 150.0,
            }
        );
    }

    #[test]
    fn vertical_percentage_padding_uses_the_containing_block_width() {
        let child = Component::boxed(
            Style {
                display: WDisp::Block,
                width: WDim::Px(100.0),
                height: WDim::Px(50.0),
                padding: w3cos_std::style::Edges {
                    top: WSpacing::Percent(10.0),
                    ..w3cos_std::style::Edges::ZERO
                },
                ..Style::default()
            },
            Vec::new(),
        );
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                width: WDim::Px(500.0),
                ..Style::default()
            },
            vec![child],
        );
        let layout = compute(&root, 800.0, 600.0).unwrap();
        assert_eq!((layout[1].0.width, layout[1].0.height), (100.0, 100.0));
    }

    #[test]
    fn horizontal_percentage_padding_uses_a_zero_width_containing_block() {
        let grandchild = Component::boxed(
            Style {
                display: WDisp::Block,
                width: WDim::Px(100.0),
                height: WDim::Px(20.0),
                ..Style::default()
            },
            Vec::new(),
        );
        let child = Component::boxed(
            Style {
                display: WDisp::Block,
                width: WDim::Px(100.0),
                padding: w3cos_std::style::Edges {
                    left: WSpacing::Percent(50.0),
                    right: WSpacing::Percent(50.0),
                    ..w3cos_std::style::Edges::ZERO
                },
                ..Style::default()
            },
            vec![grandchild],
        );
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                width: WDim::Px(0.0),
                ..Style::default()
            },
            vec![child],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        assert_eq!(layout[1].0.width, 100.0);
        assert_eq!(layout[2].0.x, 0.0);
    }

    #[test]
    fn relative_length_insets_are_applied_once_by_taffy() {
        let child = Component::boxed(
            Style {
                display: WDisp::Block,
                position: WPos::Relative,
                bottom: WDim::Px(20.0),
                width: WDim::Px(100.0),
                height: WDim::Px(40.0),
                ..Style::default()
            },
            Vec::new(),
        );
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                width: WDim::Px(100.0),
                ..Style::default()
            },
            vec![child],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        assert_eq!(layout[1].0.y, -20.0);
    }

    #[test]
    fn relative_positioned_descendant_contributes_to_scroll_extent() {
        let child = Component::boxed(
            Style {
                display: WDisp::Block,
                position: WPos::Relative,
                top: WDim::Percent(100.0),
                width: WDim::Px(100.0),
                height: WDim::Px(100.0),
                ..Style::default()
            },
            Vec::new(),
        );
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                overflow: WOverflow::Hidden,
                width: WDim::Px(200.0),
                height: WDim::Px(200.0),
                ..Style::default()
            },
            vec![child],
        );

        let (layouts, scrollable, _) = compute_with_scroll(&root, 800.0, 600.0).unwrap();
        assert_eq!(layouts[1].0.y, 200.0);
        assert_eq!(scrollable.len(), 1);
        assert_eq!(scrollable[0].2.max_y, 100.0);
    }

    #[test]
    fn relative_percentage_top_is_auto_for_an_auto_height_containing_block() {
        let child = Component::boxed(
            Style {
                display: WDisp::Block,
                position: WPos::Relative,
                top: WDim::Percent(50.0),
                width: WDim::Px(100.0),
                height: WDim::Px(100.0),
                ..Style::default()
            },
            Vec::new(),
        );
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                width: WDim::Px(100.0),
                ..Style::default()
            },
            vec![child],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        assert_eq!(layout[1].0.y, layout[0].0.y);
    }

    #[test]
    fn absolute_auto_height_with_opposing_insets_is_definite_for_relative_percentages() {
        let child = Component::boxed(
            Style {
                display: WDisp::Block,
                position: WPos::Relative,
                top: WDim::Percent(100.0),
                width: WDim::Px(100.0),
                height: WDim::Px(100.0),
                ..Style::default()
            },
            Vec::new(),
        );
        let scroller = Component::boxed(
            Style {
                display: WDisp::Block,
                position: WPos::Absolute,
                top: WDim::Px(0.0),
                right: WDim::Px(0.0),
                bottom: WDim::Px(0.0),
                left: WDim::Px(0.0),
                overflow: WOverflow::Hidden,
                ..Style::default()
            },
            vec![child],
        );
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                position: WPos::Relative,
                width: WDim::Px(200.0),
                height: WDim::Px(200.0),
                ..Style::default()
            },
            vec![scroller],
        );

        let (layouts, scrollable, _) = compute_with_scroll(&root, 800.0, 600.0).unwrap();
        assert_eq!(layouts[2].0.y, 200.0);
        assert_eq!(scrollable.len(), 1);
        assert_eq!(scrollable[0].2.max_y, 100.0);
    }

    #[test]
    fn absolute_percentage_height_uses_auto_containing_blocks_final_height() {
        use w3cos_dom::{Document, stylesheet};

        stylesheet::clear_rules();
        stylesheet::register_rule(
            "#containing-block",
            &[
                ("height", "auto"),
                ("position", "relative"),
                ("width", "192px"),
            ],
        );
        stylesheet::register_rule(
            "#percentage-child",
            &[
                ("height", "50%"),
                ("position", "absolute"),
                ("width", "96px"),
            ],
        );
        stylesheet::register_rule(
            "#in-flow-child",
            &[("height", "192px"), ("width", "192px")],
        );

        let mut document = Document::new();
        let containing_block = document.create_element("div");
        containing_block.set_attribute(&mut document, "id", "containing-block");
        let percentage_child = document.create_element("div");
        percentage_child.set_attribute(&mut document, "id", "percentage-child");
        let in_flow_child = document.create_element("div");
        in_flow_child.set_attribute(&mut document, "id", "in-flow-child");
        containing_block.append_child(&mut document, percentage_child);
        containing_block.append_child(&mut document, in_flow_child);
        document.body().append_child(&mut document, containing_block);

        let component = document.to_component_tree();
        assert_eq!(
            component.children[0].children[0].style.height,
            WDim::Percent(50.0)
        );
        let layout = compute(&component, 800.0, 600.0).unwrap();
        let rect = |index| layout.iter().find(|(_, item)| *item == index).unwrap().0;
        assert_eq!(rect(1).height, 192.0);
        assert_eq!(rect(2).height, 96.0, "layout={layout:#?}");
        stylesheet::clear_rules();
    }

    #[test]
    fn anonymous_inline_line_items_preserve_negative_margin_wrapping() {
        use w3cos_dom::{Document, stylesheet};

        fn image(
            document: &mut Document,
            width_class: &str,
            margin_class: Option<&str>,
        ) -> w3cos_dom::Element {
            let image = document.create_element("img");
            image.class_list_add(document, width_class);
            if let Some(margin_class) = margin_class {
                image.class_list_add(document, margin_class);
            }
            image
        }

        fn host_height(
            flat: &[FlatNodeInfo<'_>],
            layout: &[(LayoutRect, usize)],
            host_id: u64,
        ) -> f32 {
            let index = flat
                .iter()
                .position(|node| {
                    matches!(node.on_click, EventAction::NativeHost { id, .. } if *id == host_id)
                })
                .expect("native host component");
            layout
                .iter()
                .find_map(|(rect, candidate)| (*candidate == index).then_some(rect.height))
                .expect("native host layout")
        }

        stylesheet::clear_rules();
        stylesheet::register_rule(
            ".line",
            &[
                ("width", "40px"),
                ("font-size", "10px"),
                ("line-height", "1"),
            ],
        );
        stylesheet::register_rule("img", &[("height", "6px")]);
        stylesheet::register_rule(".w1", &[("width", "1ch")]);
        stylesheet::register_rule(".w2", &[("width", "2ch")]);
        stylesheet::register_rule(".w4", &[("width", "4ch")]);
        stylesheet::register_rule(".neg1", &[("margin-left", "-1ch")]);

        let mut document = Document::new();
        let one_line = document.create_element("div");
        one_line.class_list_add(&mut document, "line");
        let first = image(&mut document, "w4", None);
        let second = image(&mut document, "w1", Some("neg1"));
        one_line.append_child(&mut document, first);
        one_line.append_child(&mut document, second);

        let two_lines = document.create_element("div");
        two_lines.class_list_add(&mut document, "line");
        let first = image(&mut document, "w4", None);
        let second = image(&mut document, "w2", Some("neg1"));
        two_lines.append_child(&mut document, first);
        two_lines.append_child(&mut document, second);

        document.body().append_child(&mut document, one_line);
        document.body().append_child(&mut document, two_lines);

        let component = document.to_component_tree();
        let flat = pre_flatten(&component);
        let layout = compute(&component, 800.0, 600.0).unwrap();
        assert_eq!(
            host_height(&flat, &layout, one_line.id.as_u32() as u64),
            10.0,
            "40px + (10px - 10px) stays on one 10px line: {component:#?}"
        );
        assert_eq!(
            host_height(&flat, &layout, two_lines.id.as_u32() as u64),
            20.0,
            "40px + (20px - 10px) wraps to two 10px lines: {component:#?}"
        );
        stylesheet::clear_rules();
    }

    #[test]
    fn padded_column_stretches_card_inside_content_box() {
        let card = Component::column(
            Style {
                border_width: 1.0,
                ..Style::default()
            },
            vec![Component::text("card", s())],
        );
        let l = compute(&Component::column(col(), vec![card]), 400.0, 600.0).unwrap();
        let card_rect = l[1].0;
        assert_eq!(card_rect.x, 16.0);
        assert_eq!(card_rect.width, 368.0);
        assert!(card_rect.x + card_rect.width <= 400.0);
    }

    #[test]
    fn column_stacks_vertically() {
        let l = compute(
            &Component::column(
                col(),
                vec![Component::text("A", s()), Component::text("B", s())],
            ),
            800.0,
            600.0,
        )
        .unwrap();
        assert_eq!(l.len(), 3);
        assert!(l[2].0.y > l[1].0.y);
    }

    #[test]
    fn row_arranges_horizontally() {
        let l = compute(
            &Component::row(
                row(),
                vec![Component::text("A", s()), Component::text("B", s())],
            ),
            800.0,
            600.0,
        )
        .unwrap();
        assert_eq!(l.len(), 3);
        assert!(l[2].0.x > l[1].0.x);
    }

    #[test]
    fn padding_offsets_child() {
        let l = compute(
            &Component::column(
                Style {
                    display: WDisp::Flex,
                    padding: w3cos_std::style::Edges::all(40.0),
                    width: WDim::Px(400.0),
                    height: WDim::Px(300.0),
                    ..Style::default()
                },
                vec![Component::text("X", s())],
            ),
            800.0,
            600.0,
        )
        .unwrap();
        assert!(l[1].0.x >= 40.0);
        assert!(l[1].0.y >= 40.0);
    }

    #[test]
    fn empty_container_one_entry() {
        let l = compute(&Component::boxed(s(), vec![]), 800.0, 600.0).unwrap();
        assert_eq!(l.len(), 1);
    }

    #[test]
    fn deeply_nested_11_nodes() {
        let mut c = Component::text("D", s());
        for _ in 0..10 {
            c = Component::column(col(), vec![c]);
        }
        assert_eq!(compute(&c, 800.0, 600.0).unwrap().len(), 11);
    }

    #[test]
    fn button_has_minimum_size() {
        let l = compute(&Component::button("OK", s()), 800.0, 600.0).unwrap();
        assert!(l[0].0.width >= 32.0);
        assert!(l[0].0.height >= 16.0);
    }

    #[test]
    fn button_css_min_height_is_not_overwritten_by_intrinsic_height() {
        let layout = compute(
            &Component::button(
                "新对话",
                Style {
                    min_height: WDim::Px(44.0),
                    ..Style::default()
                },
            ),
            402.0,
            874.0,
        )
        .unwrap();
        assert_eq!(layout[0].0.height, 44.0);
    }

    #[test]
    fn three_row_children_ordered_ltr() {
        let l = compute(
            &Component::row(
                Style {
                    display: WDisp::Flex,
                    flex_direction: WDir::Row,
                    gap: 24.0,
                    width: WDim::Px(600.0),
                    height: WDim::Px(50.0),
                    ..Style::default()
                },
                vec![
                    Component::text("X", s()),
                    Component::text("Y", s()),
                    Component::text("Z", s()),
                ],
            ),
            800.0,
            600.0,
        )
        .unwrap();
        assert_eq!(l.len(), 4);
        assert!(l[1].0.x < l[2].0.x);
        assert!(l[2].0.x < l[3].0.x);
    }

    #[test]
    fn gap_vs_no_gap() {
        let ng = compute(
            &Component::column(
                Style {
                    display: WDisp::Flex,
                    flex_direction: WDir::Column,
                    width: WDim::Px(400.0),
                    height: WDim::Px(300.0),
                    ..Style::default()
                },
                vec![Component::text("A", s()), Component::text("B", s())],
            ),
            800.0,
            600.0,
        )
        .unwrap();
        let wg = compute(
            &Component::column(
                Style {
                    display: WDisp::Flex,
                    flex_direction: WDir::Column,
                    gap: 20.0,
                    width: WDim::Px(400.0),
                    height: WDim::Px(300.0),
                    ..Style::default()
                },
                vec![Component::text("A", s()), Component::text("B", s())],
            ),
            800.0,
            600.0,
        )
        .unwrap();
        let d0 = ng[2].0.y - (ng[1].0.y + ng[1].0.height);
        let d1 = wg[2].0.y - (wg[1].0.y + wg[1].0.height);
        assert!(d1 >= d0);
    }

    #[test]
    fn display_none_skips_gap() {
        let visible = compute(
            &Component::column(
                Style {
                    display: WDisp::Flex,
                    flex_direction: WDir::Column,
                    gap: 16.0,
                    width: WDim::Px(400.0),
                    height: WDim::Px(300.0),
                    ..Style::default()
                },
                vec![
                    Component::text("A", s()),
                    Component::column(
                        Style {
                            display: WDisp::None,
                            ..Style::default()
                        },
                        vec![],
                    ),
                    Component::text("B", s()),
                ],
            ),
            800.0,
            600.0,
        )
        .unwrap();
        let hidden = compute(
            &Component::column(
                Style {
                    display: WDisp::Flex,
                    flex_direction: WDir::Column,
                    gap: 16.0,
                    width: WDim::Px(400.0),
                    height: WDim::Px(300.0),
                    ..Style::default()
                },
                vec![
                    Component::text("A", s()),
                    Component::column(Style::default(), vec![]),
                    Component::text("B", s()),
                ],
            ),
            800.0,
            600.0,
        )
        .unwrap();
        // `display:none` nodes are omitted from the exported layout cache, so
        // B is entry 2 here and entry 3 when the middle node participates.
        let gap_visible = visible[2].0.y - (visible[1].0.y + visible[1].0.height);
        let gap_hidden = hidden[3].0.y - (hidden[1].0.y + hidden[1].0.height);
        assert!(
            gap_visible < gap_hidden,
            "display:none should not reserve flex gap (visible={gap_visible}, hidden={gap_hidden})"
        );
    }

    #[test]
    fn button_intrinsic_includes_padding() {
        let style = Style {
            font_size: 14.0,
            padding: w3cos_std::style::Edges::all(14.0),
            ..Style::default()
        };
        let (_, h) = leaf_intrinsic_size(
            &ComponentKind::Button {
                label: "GET".to_string(),
            },
            &style,
        );
        let expected = 14.0 * style.line_height + 28.0;
        assert!(
            (h - expected).abs() < 0.01,
            "button height {h} != expected {expected}"
        );
    }

    #[test]
    fn decoded_image_intrinsics_drive_auto_size_and_preserve_aspect_ratio() {
        let source = "browser-layout-intrinsic.png";
        let image = image::RgbaImage::from_pixel(4, 2, image::Rgba([1, 2, 3, 255]));
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        crate::image_loader::decode_and_install(source, &bytes.into_inner()).unwrap();

        let kind = ComponentKind::Image {
            src: source.to_string(),
        };
        assert_eq!(leaf_intrinsic_size(&kind, &Style::default()), (4.0, 2.0));
        assert_eq!(
            leaf_intrinsic_size(
                &kind,
                &Style {
                    width: WDim::Px(40.0),
                    ..Style::default()
                },
            ),
            (40.0, 20.0)
        );
        assert_eq!(
            leaf_intrinsic_size(
                &kind,
                &Style {
                    height: WDim::Px(10.0),
                    ..Style::default()
                },
            ),
            (20.0, 10.0)
        );
        assert_eq!(
            leaf_intrinsic_size(
                &kind,
                &Style {
                    width: WDim::Px(0.0),
                    min_width: WDim::Px(100.0),
                    ..Style::default()
                },
            ),
            (100.0, 50.0)
        );
        let layout = compute(
            &Component::boxed(
                Style {
                    display: WDisp::Block,
                    width: WDim::Px(800.0),
                    ..Style::default()
                },
                vec![Component::image(
                    source,
                    Style {
                        display: WDisp::Inline,
                        ..Style::default()
                    },
                )],
            ),
            800.0,
            600.0,
        )
        .unwrap();
        let image = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        assert_eq!((image.width, image.height), (4.0, 2.0));
        let constrained = compute(
            &Component::boxed(
                Style {
                    display: WDisp::Block,
                    width: WDim::Px(800.0),
                    ..Style::default()
                },
                vec![Component::image(
                    source,
                    Style {
                        display: WDisp::Inline,
                        width: WDim::Em(0.0),
                        min_width: WDim::Em(6.25),
                        ..Style::default()
                    },
                )],
            ),
            800.0,
            600.0,
        )
        .unwrap();
        let constrained = constrained
            .iter()
            .find(|(_, index)| *index == 1)
            .unwrap()
            .0;
        assert_eq!((constrained.width, constrained.height), (100.0, 50.0));
        crate::image_loader::invalidate(source);
    }

    #[test]
    fn nested_percentage_min_height_uses_the_viewport_basis() {
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                height: WDim::Percent(100.0),
                ..Style::default()
            },
            vec![Component::boxed(
                Style {
                    display: WDisp::Block,
                    height: WDim::Percent(100.0),
                    ..Style::default()
                },
                vec![Component::boxed(
                    Style {
                        display: WDisp::Flex,
                        min_height: WDim::Percent(100.0),
                        ..Style::default()
                    },
                    vec![Component::text(
                        "viewport",
                        Style {
                            display: WDisp::Inline,
                            ..Style::default()
                        },
                    )],
                )],
            )],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let rect = |index| layout.iter().find(|(_, item)| *item == index).unwrap().0;
        assert_eq!(rect(0).height, 600.0);
        assert_eq!(rect(1).height, 600.0);
        assert_eq!(rect(2).height, 600.0);
    }

    #[test]
    fn block_min_width_overrides_zero_width() {
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                width: WDim::Px(800.0),
                ..Style::default()
            },
            vec![Component::boxed(
                Style {
                    display: WDisp::Block,
                    width: WDim::Px(0.0),
                    min_width: WDim::Px(96.0),
                    height: WDim::Px(96.0),
                    ..Style::default()
                },
                Vec::new(),
            )],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let constrained = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        assert_eq!((constrained.width, constrained.height), (96.0, 96.0));
    }

    #[test]
    fn inline_text_ignores_min_width() {
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                width: WDim::Px(800.0),
                ..Style::default()
            },
            vec![Component::boxed(
                Style {
                    display: WDisp::Inline,
                    font_size: 16.0,
                    min_width: WDim::Px(200.0),
                    ..Style::default()
                },
                vec![Component::text(
                    "A",
                    Style {
                        display: WDisp::Inline,
                        font_size: 16.0,
                        ..Style::default()
                    },
                )],
            )],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let inline = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        assert!(inline.width < 200.0, "inline width was {}", inline.width);
    }

    #[test]
    fn empty_non_replaced_inline_ignores_width() {
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                width: WDim::Px(800.0),
                ..Style::default()
            },
            vec![Component::boxed(
                Style {
                    display: WDisp::Inline,
                    width: WDim::Px(96.0),
                    height: WDim::Px(96.0),
                    ..Style::default()
                },
                Vec::new(),
            )],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let inline = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        assert_eq!(inline.width, 0.0);
    }

    #[test]
    fn percentage_image_height_uses_definite_ancestor_and_intrinsic_ratio() {
        let source = "browser-layout-percent-height.png";
        let image = image::RgbaImage::from_pixel(1, 1, image::Rgba([1, 2, 3, 255]));
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        crate::image_loader::decode_and_install(source, &bytes.into_inner()).unwrap();

        let layout = compute(
            &Component::boxed(
                Style {
                    display: WDisp::Block,
                    width: WDim::Px(800.0),
                    height: WDim::Px(200.0),
                    ..Style::default()
                },
                vec![Component::image(
                    source,
                    Style {
                        display: WDisp::InlineBlock,
                        height: WDim::Percent(50.0),
                        ..Style::default()
                    },
                )],
            ),
            800.0,
            600.0,
        )
        .unwrap();
        let image = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        assert_eq!((image.width, image.height), (100.0, 100.0));
        crate::image_loader::invalidate(source);
    }

    #[test]
    fn percentage_canvas_height_uses_its_intrinsic_ratio_in_an_anonymous_block() {
        let layout = compute(
            &Component::boxed(
                Style {
                    display: WDisp::Block,
                    width: WDim::Px(784.0),
                    ..Style::default()
                },
                vec![Component::row(
                    Style {
                        display: WDisp::InlineBlock,
                        height: WDim::Px(100.0),
                        ..Style::default()
                    },
                    vec![
                        Component::canvas(
                            10,
                            10,
                            Style {
                                display: WDisp::InlineBlock,
                                height: WDim::Percent(100.0),
                                position: WPos::Relative,
                                z_index: -1,
                                ..Style::default()
                            },
                        ),
                        Component::boxed(
                            Style {
                                display: WDisp::Block,
                                margin: w3cos_std::style::Edges {
                                    top: WSpacing::Px(16.0),
                                    bottom: WSpacing::Px(16.0),
                                    ..w3cos_std::style::Edges::ZERO
                                },
                                ..Style::default()
                            },
                            Vec::new(),
                        ),
                    ],
                )],
            ),
            800.0,
            600.0,
        )
        .unwrap();
        let parent = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        let canvas = layout.iter().find(|(_, index)| *index == 2).unwrap().0;
        assert_eq!((parent.width, parent.height), (100.0, 100.0));
        assert_eq!((canvas.width, canvas.height), (100.0, 100.0));
    }

    #[test]
    fn zero_max_height_clamps_a_block_text_background_box() {
        let layout = compute(
            &Component::column(
                Style {
                    display: WDisp::Block,
                    width: WDim::Px(800.0),
                    ..Style::default()
                },
                vec![Component::row(
                    Style {
                        display: WDisp::Block,
                        max_height: WDim::Px(0.0),
                        ..Style::default()
                    },
                    vec![Component::text(
                        "Filler Text",
                        Style {
                            display: WDisp::Inline,
                            ..Style::default()
                        },
                    )],
                )],
            ),
            800.0,
            600.0,
        )
        .unwrap();

        let text = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        assert_eq!(text.height, 0.0);
    }

    #[test]
    fn absolute_shrink_fit_width_keeps_content_box_height() {
        let layout = compute(&Component::row(Style {
            display: WDisp::Block, width: WDim::Px(800.0),
            ..Style::default()
        }, vec![Component::row(Style {
            display: WDisp::Block, position: WPos::Absolute,
            height: WDim::Px(296.0), border_width: 3.0,
            ..Style::default()
        }, vec![Component::row(Style {
            display: WDisp::Block, width: WDim::Px(200.0), height: WDim::Px(50.0),
            margin: w3cos_std::style::Edges { left: WSpacing::Px(96.0),
                ..w3cos_std::style::Edges::ZERO },
            ..Style::default()
        }, vec![])])]), 800.0, 600.0).unwrap();
        let container = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        assert_eq!((container.width, container.height), (302.0, 302.0));
    }

    #[test]
    fn block_image_explicit_axes_override_intrinsic_ratio() {
        let src = "browser-layout-block-explicit-axes.svg";
        crate::image_loader::decode_and_install(src,
            br#"<svg xmlns="http://www.w3.org/2000/svg" width="15" height="15"/>"#).unwrap();
        let layout = compute(&Component::row(Style {
            display: WDisp::Block, width: WDim::Px(288.0), height: WDim::Px(288.0),
            ..Style::default()
        }, vec![Component::image(src, Style {
            display: WDisp::Block, width: WDim::Px(200.0), height: WDim::Px(50.0),
            ..Style::default()
        })]), 800.0, 600.0).unwrap();
        let image = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        assert_eq!((image.width, image.height), (200.0, 50.0));
    }

    #[test]
    fn svg_replaced_image_uses_css_intrinsic_metadata_instead_of_raster_fallback() {
        let ratio_only = "browser-layout-ratio-only.svg";
        crate::image_loader::decode_and_install(
            ratio_only,
            br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1000 500"/>"#,
        )
        .unwrap();
        let kind = ComponentKind::Image {
            src: ratio_only.to_string(),
        };
        assert_eq!(
            leaf_intrinsic_size_with_containing(&kind, &Style::default(), Some(200.0)),
            (200.0, 100.0)
        );
        assert_eq!(
            leaf_intrinsic_size_with_containing(
                &kind,
                &Style {
                    max_height: WDim::Px(20.0),
                    ..Style::default()
                },
                Some(200.0),
            ),
            (40.0, 20.0)
        );
        let constrained = leaf_intrinsic_size_with_containing(
            &kind,
            &Style {
                min_width: WDim::Px(240.0),
                ..Style::default()
            },
            Some(200.0),
        );
        assert!((constrained.0 - 240.0).abs() < 0.001);
        assert!((constrained.1 - 120.0).abs() < 0.001);

        let explicit_axes = "browser-layout-explicit-axes.svg";
        crate::image_loader::decode_and_install(
            explicit_axes,
            br#"<svg xmlns="http://www.w3.org/2000/svg" width="50" height="25" viewBox="0 0 1000 1000"/>"#,
        )
        .unwrap();
        let kind = ComponentKind::Image {
            src: explicit_axes.to_string(),
        };
        assert_eq!(
            leaf_intrinsic_size(
                &kind,
                &Style {
                    height: WDim::Px(20.0),
                    ..Style::default()
                }
            ),
            (40.0, 20.0)
        );

        let height_only = "browser-layout-height-only.svg";
        crate::image_loader::decode_and_install(
            height_only,
            br#"<svg xmlns="http://www.w3.org/2000/svg" height="25"/>"#,
        )
        .unwrap();
        let kind = ComponentKind::Image {
            src: height_only.to_string(),
        };
        assert_eq!(leaf_intrinsic_size(&kind, &Style::default()), (300.0, 25.0));

        let no_intrinsic_size = "browser-layout-no-intrinsic-size.svg";
        crate::image_loader::decode_and_install(
            no_intrinsic_size,
            br#"<svg xmlns="http://www.w3.org/2000/svg"/>"#,
        )
        .unwrap();
        let kind = ComponentKind::Image {
            src: no_intrinsic_size.to_string(),
        };
        assert_eq!(
            leaf_intrinsic_size_with_containing(&kind, &Style::default(), Some(150.0)),
            (150.0, 150.0)
        );

        crate::image_loader::invalidate(ratio_only);
        crate::image_loader::invalidate(explicit_axes);
        crate::image_loader::invalidate(height_only);
        crate::image_loader::invalidate(no_intrinsic_size);
    }

    #[test]
    fn block_replaced_auto_width_uses_parent_width_before_intrinsic_ratio() {
        let source = "browser-layout-block-auto-ratio.svg";
        crate::image_loader::decode_and_install(
            source,
            br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 2"/>"#,
        )
        .unwrap();
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                width: WDim::Px(200.0),
                ..Style::default()
            },
            vec![Component::image(
                source,
                Style {
                    display: WDisp::Block,
                    padding: w3cos_std::style::Edges {
                        right: WSpacing::Px(100.0),
                        ..w3cos_std::style::Edges::ZERO
                    },
                    ..Style::default()
                },
            )],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let image = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        assert_eq!((image.width, image.height), (200.0, 200.0));
        crate::image_loader::invalidate(source);
    }

    #[test]
    fn broken_browser_image_does_not_use_the_legacy_placeholder_size() {
        let source = "missing-generated-image.png";
        crate::image_loader::reserve_browser_source(source);
        let kind = ComponentKind::Image {
            src: source.to_string(),
        };

        assert_eq!(leaf_intrinsic_size(&kind, &Style::default()), (0.0, 0.0));
        crate::image_loader::invalidate(source);
    }

    #[test]
    fn image_with_fixed_width_and_auto_height_does_not_collapse_in_grid() {
        let image = Component::image(
            "blob:w3cos/pending-preview",
            Style {
                display: WDisp::Inline,
                width: WDim::Px(40.0),
                height: WDim::Auto,
                ..Style::default()
            },
        );
        let layout = compute(
            &Component::column(
                Style {
                    display: WDisp::Grid,
                    width: WDim::Px(260.0),
                    ..Style::default()
                },
                vec![image],
            ),
            402.0,
            874.0,
        )
        .unwrap();

        assert_eq!(layout[1].0.width, 40.0);
        assert!(layout[1].0.height > 0.0, "replaced image height collapsed");
    }

    #[test]
    fn inline_replaced_leaf_preserves_its_authored_width() {
        let style = Style {
            display: WDisp::Inline,
            width: WDim::Px(96.0),
            height: WDim::Auto,
            ..Style::default()
        };
        let base = to_taffy_style(&style, 800.0, 600.0);
        let size = leaf_taffy_size(
            &ComponentKind::Image {
                src: "missing-ratio-image.png".to_string(),
            },
            &style,
            &base,
            Some(WDisp::Block),
            800.0,
            800.0,
            600.0,
        );

        assert_eq!(size.width, Dimension::length(96.0));
    }

    #[test]
    fn percentage_sized_replaced_image_uses_containing_block_width() {
        let image = Component::image(
            "blob:w3cos/responsive-stripe",
            Style {
                display: WDisp::InlineBlock,
                width: WDim::Percent(100.0),
                height: WDim::Px(50.0),
                ..Style::default()
            },
        );
        let layout = compute(
            &Component::boxed(
                Style {
                    display: WDisp::Block,
                    width: WDim::Px(800.0),
                    ..Style::default()
                },
                vec![image],
            ),
            800.0,
            600.0,
        )
        .unwrap();

        assert_eq!((layout[1].0.width, layout[1].0.height), (800.0, 50.0));
    }

    #[test]
    fn wrapped_inline_lines_stay_at_block_start_in_a_fixed_height_container() {
        let image = || {
            Component::image(
                "blob:w3cos/line-item",
                Style {
                    display: WDisp::InlineBlock,
                    width: WDim::Px(80.0),
                    height: WDim::Px(20.0),
                    ..Style::default()
                },
            )
        };
        let layout = compute(
            &Component::boxed(
                Style {
                    display: WDisp::Block,
                    width: WDim::Px(100.0),
                    height: WDim::Px(200.0),
                    ..Style::default()
                },
                vec![image(), image()],
            ),
            800.0,
            600.0,
        )
        .unwrap();

        assert_eq!(layout[1].0.y, 0.0);
        assert_eq!(layout[2].0.y, 20.0);
    }

    #[test]
    fn column_stretch_fills_viewport_width() {
        let l = compute(
            &Component::column(
                Style {
                    display: WDisp::Flex,
                    flex_direction: WDir::Column,
                    box_sizing: WBoxSizing::BorderBox,
                    padding: w3cos_std::style::Edges::all(20.0),
                    width: WDim::Percent(100.0),
                    ..Style::default()
                },
                vec![Component::column(
                    Style {
                        display: WDisp::Flex,
                        flex_direction: WDir::Column,
                        padding: w3cos_std::style::Edges::all(12.0),
                        background: Color::from_hex("#1e1e28"),
                        ..Style::default()
                    },
                    vec![Component::button(
                        "GET httpbin.org/get",
                        Style {
                            padding: w3cos_std::style::Edges::all(14.0),
                            font_size: 14.0,
                            ..Style::default()
                        },
                    )],
                )],
            ),
            402.0,
            874.0,
        )
        .unwrap();
        let inner = l.iter().find(|(_, idx)| *idx == 1).map(|(r, _)| r);
        let btn = l.iter().find(|(_, idx)| *idx == 2).map(|(r, _)| r);
        let inner = inner.expect("inner column");
        let btn = btn.expect("button");
        assert!(
            (inner.width - 362.0).abs() < 2.0,
            "inner width {} expected ~362",
            inner.width
        );
        assert!(
            (btn.width - 338.0).abs() < 4.0,
            "button should stretch to inner column width, got {}",
            btn.width
        );
    }

    #[test]
    fn grid_text_wraps_to_the_available_track_width() {
        let layout = compute(
            &Component::column(
                Style {
                    display: WDisp::Grid,
                    width: WDim::Px(320.0),
                    ..Style::default()
                },
                vec![Component::text(
                    "使用手机号验证身份，继续处理你的物流协作任务",
                    Style::default(),
                )],
            ),
            390.0,
            844.0,
        )
        .unwrap();
        let text = layout
            .iter()
            .find(|(_, index)| *index == 1)
            .map(|(rect, _)| rect)
            .expect("text layout");
        assert!(
            text.width <= 320.0,
            "grid text escaped its track: {}",
            text.width
        );
        assert!(text.height > Style::default().font_size);
    }

    #[test]
    fn implicit_grid_column_stretches_nested_form_rows() {
        let layout = compute(
            &Component::column(
                Style {
                    display: WDisp::Grid,
                    width: WDim::Px(304.0),
                    ..Style::default()
                },
                vec![Component::column(
                    Style {
                        display: WDisp::Grid,
                        ..Style::default()
                    },
                    vec![Component::row(
                        Style {
                            display: WDisp::Flex,
                            gap: 8.0,
                            ..Style::default()
                        },
                        vec![
                            Component::button(
                                "+86",
                                Style {
                                    width: WDim::Px(92.0),
                                    flex_shrink: 0.0,
                                    ..Style::default()
                                },
                            ),
                            Component::text_input(
                                "",
                                "请输入手机号",
                                Style {
                                    width: WDim::Percent(100.0),
                                    min_width: WDim::Px(0.0),
                                    ..Style::default()
                                },
                            ),
                        ],
                    )],
                )],
            ),
            390.0,
            844.0,
        )
        .unwrap();
        let row = layout.iter().find(|(_, index)| *index == 2).unwrap().0;
        let input = layout.iter().find(|(_, index)| *index == 4).unwrap().0;
        assert!(
            (row.width - 304.0).abs() < 1.0,
            "implicit grid row width={}",
            row.width
        );
        assert!(
            input.width > 190.0 && input.x + input.width <= 304.0,
            "input={input:?}"
        );
    }

    #[test]
    fn inline_block_flex_item_honors_flex_grow() {
        let layout = compute(
            &Component::row(
                Style {
                    display: WDisp::Flex,
                    width: WDim::Px(375.0),
                    height: WDim::Px(64.0),
                    gap: 7.0,
                    padding: w3cos_std::style::Edges::all(8.0),
                    ..Style::default()
                },
                vec![
                    Component::button(
                        "图",
                        Style {
                            display: WDisp::InlineBlock,
                            width: WDim::Px(34.0),
                            height: WDim::Px(42.0),
                            flex_shrink: 0.0,
                            ..Style::default()
                        },
                    ),
                    Component::text_input(
                        "",
                        "问 通用对话，或继续补充上下文…",
                        Style {
                            display: WDisp::InlineBlock,
                            height: WDim::Px(42.0),
                            min_width: WDim::Px(0.0),
                            flex_grow: 1.0,
                            ..Style::default()
                        },
                    ),
                    Component::button(
                        "发",
                        Style {
                            display: WDisp::InlineBlock,
                            width: WDim::Px(42.0),
                            height: WDim::Px(42.0),
                            flex_shrink: 0.0,
                            ..Style::default()
                        },
                    ),
                ],
            ),
            375.0,
            812.0,
        )
        .unwrap();

        let input = layout
            .iter()
            .find(|(_, index)| *index == 2)
            .map(|(rect, _)| rect)
            .expect("input layout");
        assert!(
            input.width > 200.0,
            "flex-grow input should consume the remaining row width, got {}",
            input.width
        );
    }

    #[test]
    fn form_controls_keep_intrinsic_width_in_block_layout() {
        let layout = compute(
            &Component::boxed(
                Style {
                    display: WDisp::Block,
                    width: WDim::Px(375.0),
                    ..Style::default()
                },
                vec![
                    Component::text_input(
                        "shipper@demo",
                        "",
                        Style {
                            display: WDisp::InlineBlock,
                            ..Style::default()
                        },
                    ),
                    Component::button(
                        "登录",
                        Style {
                            display: WDisp::InlineBlock,
                            font_size: 13.333_333,
                            padding: w3cos_std::style::Edges::xy(6.0, 1.0),
                            border_width: 1.0,
                            ..Style::default()
                        },
                    ),
                ],
            ),
            375.0,
            812.0,
        )
        .unwrap();

        let input = layout
            .iter()
            .find(|(_, index)| *index == 1)
            .map(|(rect, _)| rect)
            .expect("input layout");
        let button = layout
            .iter()
            .find(|(_, index)| *index == 2)
            .map(|(rect, _)| rect)
            .expect("button layout");

        assert!(
            (input.width - 169.0).abs() < 2.0,
            "default input should stay near the browser's intrinsic width, got {}",
            input.width
        );
        assert!(
            button.width < 80.0,
            "default button should not stretch across a block container, got {}",
            button.width
        );
    }

    #[test]
    fn adjacent_block_margins_collapse_for_text_and_container_boxes() {
        let paragraph_style = Style {
            display: WDisp::Block,
            margin: w3cos_std::style::Edges {
                top: w3cos_std::style::Spacing::Px(16.0),
                right: w3cos_std::style::Spacing::Px(0.0),
                bottom: w3cos_std::style::Spacing::Px(16.0),
                left: w3cos_std::style::Spacing::Px(0.0),
            },
            color: w3cos_std::Color::BLACK,
            ..Style::default()
        };
        let block_style = Style {
            display: WDisp::Block,
            color: w3cos_std::Color::BLACK,
            ..Style::default()
        };
        let parent_style = Style {
            display: WDisp::Block,
            width: WDim::Px(800.0),
            ..Style::default()
        };
        let reference = compute(
            &Component::boxed(
                parent_style.clone(),
                vec![
                    Component::text("first", paragraph_style.clone()),
                    Component::text("second", paragraph_style),
                ],
            ),
            800.0,
            600.0,
        )
        .unwrap();
        let actual = compute(
            &Component::boxed(
                parent_style,
                vec![
                    Component::text(
                        "first",
                        Style {
                            display: WDisp::Block,
                            margin: w3cos_std::style::Edges {
                                top: w3cos_std::style::Spacing::Px(16.0),
                                right: w3cos_std::style::Spacing::Px(0.0),
                                bottom: w3cos_std::style::Spacing::Px(16.0),
                                left: w3cos_std::style::Spacing::Px(0.0),
                            },
                            color: w3cos_std::Color::BLACK,
                            ..Style::default()
                        },
                    ),
                    Component::boxed(
                        block_style.clone(),
                        vec![Component::text("second", block_style)],
                    ),
                ],
            ),
            800.0,
            600.0,
        )
        .unwrap();
        let reference_second = reference.iter().find(|(_, index)| *index == 2).unwrap().0;
        let actual_second = actual.iter().find(|(_, index)| *index == 3).unwrap().0;
        assert_eq!(actual_second.y, reference_second.y);
    }

    #[test]
    fn adjacent_block_margins_use_the_larger_collapsed_gap() {
        let component = Component::boxed(
            Style {
                display: WDisp::Block,
                width: WDim::Px(800.0),
                ..Style::default()
            },
            vec![
                Component::boxed(
                    Style {
                        display: WDisp::Flex,
                        height: WDim::Px(40.0),
                        margin: w3cos_std::style::Edges {
                            top: w3cos_std::style::Spacing::Px(16.0),
                            right: w3cos_std::style::Spacing::Px(0.0),
                            bottom: w3cos_std::style::Spacing::Px(16.0),
                            left: w3cos_std::style::Spacing::Px(0.0),
                        },
                        ..Style::default()
                    },
                    Vec::new(),
                ),
                Component::boxed(
                    Style {
                        display: WDisp::Block,
                        height: WDim::Px(100.0),
                        margin: w3cos_std::style::Edges {
                            top: w3cos_std::style::Spacing::Px(66.0),
                            right: w3cos_std::style::Spacing::Px(0.0),
                            bottom: w3cos_std::style::Spacing::Px(0.0),
                            left: w3cos_std::style::Spacing::Px(0.0),
                        },
                        ..Style::default()
                    },
                    Vec::new(),
                ),
            ],
        );

        let layout = compute(&component, 800.0, 600.0).unwrap();
        let first = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        let second = layout.iter().find(|(_, index)| *index == 2).unwrap().0;
        assert_eq!(first.y, 16.0);
        assert_eq!(second.y, 122.0);
    }

    #[test]
    fn percentage_margin_uses_the_containing_blocks_content_width() {
        let layout = compute(
            &Component::boxed(
                Style {
                    display: WDisp::Block,
                    width: WDim::Px(96.0),
                    border_left_width: Some(48.0),
                    ..Style::default()
                },
                vec![Component::boxed(
                    Style {
                        display: WDisp::Block,
                        height: WDim::Px(96.0),
                        margin: w3cos_std::style::Edges {
                            left: WSpacing::Percent(50.0),
                            ..w3cos_std::style::Edges::ZERO
                        },
                        ..Style::default()
                    },
                    Vec::new(),
                )],
            ),
            800.0,
            600.0,
        )
        .unwrap();

        let parent = layout.iter().find(|(_, index)| *index == 0).unwrap().0;
        let child = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        assert_eq!(parent.width, 144.0);
        assert_eq!(child.x, 96.0);
    }

    #[test]
    fn min_height_contains_the_last_childs_collapsed_bottom_margin() {
        let zero_edges = w3cos_std::style::Edges::ZERO;
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                border_top_width: Some(1.0),
                ..Style::default()
            },
            vec![
                Component::boxed(
                    Style {
                        display: WDisp::Block,
                        min_height: WDim::Px(50.0),
                        ..Style::default()
                    },
                    vec![Component::boxed(
                        Style {
                            display: WDisp::Block,
                            margin: w3cos_std::style::Edges {
                                bottom: WSpacing::Px(50.0),
                                ..zero_edges
                            },
                            ..Style::default()
                        },
                        Vec::new(),
                    )],
                ),
                Component::boxed(
                    Style {
                        display: WDisp::Block,
                        height: WDim::Px(50.0),
                        ..Style::default()
                    },
                    Vec::new(),
                ),
            ],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let parent = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        let following = layout.iter().find(|(_, index)| *index == 3).unwrap().0;
        assert_eq!(parent.height, 50.0);
        assert_eq!(following.y, parent.y + parent.height);
    }

    #[test]
    fn min_height_traps_a_collapsed_margin_through_an_empty_tail() {
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                border_top_width: Some(1.0),
                ..Style::default()
            },
            vec![
                Component::boxed(
                    Style {
                        display: WDisp::Block,
                        min_height: WDim::Px(200.0),
                        ..Style::default()
                    },
                    vec![
                        Component::boxed(
                            Style {
                                display: WDisp::Block,
                                height: WDim::Px(30.0),
                                margin: w3cos_std::style::Edges {
                                    bottom: WSpacing::Px(100.0),
                                    ..w3cos_std::style::Edges::ZERO
                                },
                                ..Style::default()
                            },
                            Vec::new(),
                        ),
                        Component::boxed(
                            Style {
                                display: WDisp::Block,
                                ..Style::default()
                            },
                            Vec::new(),
                        ),
                    ],
                ),
                Component::boxed(
                    Style {
                        display: WDisp::Block,
                        height: WDim::Px(50.0),
                        ..Style::default()
                    },
                    Vec::new(),
                ),
            ],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let root = layout.iter().find(|(_, index)| *index == 0).unwrap().0;
        let parent = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        let marker = layout.iter().find(|(_, index)| *index == 3).unwrap().0;
        let footer = layout.iter().find(|(_, index)| *index == 4).unwrap().0;
        assert_eq!(marker.y, parent.y + 130.0);
        assert_eq!(footer.y, parent.y + parent.height);
        assert_eq!(root.height, 251.0);
    }

    #[test]
    fn active_max_height_traps_an_overflowing_childs_bottom_margin() {
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                width: WDim::Px(100.0),
                ..Style::default()
            },
            vec![
                Component::boxed(
                    Style {
                        display: WDisp::Block,
                        max_height: WDim::Px(50.0),
                        ..Style::default()
                    },
                    vec![Component::boxed(
                        Style {
                            display: WDisp::Block,
                            height: WDim::Px(51.0),
                            margin: w3cos_std::style::Edges {
                                bottom: WSpacing::Px(10.0),
                                ..w3cos_std::style::Edges::ZERO
                            },
                            ..Style::default()
                        },
                        Vec::new(),
                    )],
                ),
                Component::boxed(
                    Style {
                        display: WDisp::Block,
                        height: WDim::Px(50.0),
                        ..Style::default()
                    },
                    Vec::new(),
                ),
            ],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let constrained = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        let following = layout.iter().find(|(_, index)| *index == 3).unwrap().0;
        assert_eq!(constrained.height, 50.0);
        assert_eq!(following.y, constrained.y + constrained.height);
    }

    #[test]
    fn empty_block_margins_form_a_floats_used_box_after_negative_flow_margin() {
        let empty = Component::boxed(
            Style {
                display: WDisp::Block,
                margin: w3cos_std::style::Edges {
                    top: WSpacing::Px(100.0),
                    right: WSpacing::Px(50.0),
                    bottom: WSpacing::Px(100.0),
                    left: WSpacing::Px(50.0),
                },
                ..Style::default()
            },
            Vec::new(),
        );
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                ..Style::default()
            },
            vec![
                Component::boxed(
                    Style {
                        display: WDisp::Block,
                        width: WDim::Px(100.0),
                        height: WDim::Px(100.0),
                        margin: w3cos_std::style::Edges {
                            bottom: WSpacing::Px(-100.0),
                            ..w3cos_std::style::Edges::ZERO
                        },
                        ..Style::default()
                    },
                    Vec::new(),
                ),
                Component::boxed(
                    Style {
                        display: WDisp::Block,
                        float: WFloat::Left,
                        ..Style::default()
                    },
                    vec![empty],
                ),
            ],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let control = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        let floating = layout.iter().find(|(_, index)| *index == 2).unwrap().0;
        assert_eq!(floating.y, control.y);
        assert_eq!((floating.width, floating.height), (100.0, 100.0));
    }

    #[test]
    fn clear_applies_after_collapsing_leading_descendant_margins() {
        let floating = || {
            Component::boxed(
                Style {
                    display: WDisp::Flex,
                    float: WFloat::Left,
                    width: WDim::Px(100.0),
                    height: WDim::Px(50.0),
                    ..Style::default()
                },
                Vec::new(),
            )
        };
        let cleared = |content| {
            Component::boxed(
                Style {
                    display: WDisp::Block,
                    clear: WClear::Left,
                    ..Style::default()
                },
                vec![content],
            )
        };
        let direct = Component::boxed(
            Style {
                display: WDisp::Table,
                width: WDim::Px(100.0),
                height: WDim::Px(50.0),
                margin: w3cos_std::style::Edges {
                    top: WSpacing::Px(50.0),
                    ..w3cos_std::style::Edges::ZERO
                },
                ..Style::default()
            },
            Vec::new(),
        );
        let nested = Component::boxed(
            Style {
                display: WDisp::Block,
                margin: w3cos_std::style::Edges {
                    top: WSpacing::Px(50.0),
                    ..w3cos_std::style::Edges::ZERO
                },
                ..Style::default()
            },
            vec![Component::boxed(
                Style {
                    display: WDisp::Block,
                    margin: w3cos_std::style::Edges {
                        top: WSpacing::Px(40.0),
                        ..w3cos_std::style::Edges::ZERO
                    },
                    ..Style::default()
                },
                vec![Component::boxed(
                    Style {
                        display: WDisp::Block,
                        width: WDim::Px(100.0),
                        height: WDim::Px(50.0),
                        margin: w3cos_std::style::Edges {
                            top: WSpacing::Px(30.0),
                            ..w3cos_std::style::Edges::ZERO
                        },
                        ..Style::default()
                    },
                    Vec::new(),
                )],
            )],
        );

        let direct_layout = compute(
            &Component::boxed(
                Style {
                    display: WDisp::Block,
                    ..Style::default()
                },
                vec![floating(), cleared(direct)],
            ),
            800.0,
            600.0,
        )
        .unwrap();
        let nested_layout = compute(
            &Component::boxed(
                Style {
                    display: WDisp::Block,
                    ..Style::default()
                },
                vec![floating(), cleared(nested)],
            ),
            800.0,
            600.0,
        )
        .unwrap();

        let direct_content = direct_layout
            .iter()
            .find(|(_, index)| *index == 3)
            .unwrap()
            .0;
        let nested_content = nested_layout
            .iter()
            .find(|(_, index)| *index == 5)
            .unwrap()
            .0;
        assert_eq!(direct_content.y, 50.0);
        assert_eq!(nested_content.y, 50.0);
    }

    #[test]
    fn clear_moves_below_float_after_empty_margin_group_cancels() {
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                ..Style::default()
            },
            vec![
                Component::boxed(
                    Style {
                        display: WDisp::Block,
                        height: WDim::Px(25.0),
                        margin: w3cos_std::style::Edges {
                            bottom: WSpacing::Px(25.0),
                            ..w3cos_std::style::Edges::ZERO
                        },
                        ..Style::default()
                    },
                    Vec::new(),
                ),
                Component::boxed(
                    Style {
                        display: WDisp::Flex,
                        float: WFloat::Left,
                        width: WDim::Px(25.0),
                        height: WDim::Px(25.0),
                        ..Style::default()
                    },
                    Vec::new(),
                ),
                Component::boxed(
                    Style {
                        display: WDisp::Block,
                        height: WDim::Px(0.0),
                        margin: w3cos_std::style::Edges {
                            bottom: WSpacing::Px(25.0),
                            ..w3cos_std::style::Edges::ZERO
                        },
                        ..Style::default()
                    },
                    vec![Component::boxed(
                        Style {
                            display: WDisp::Block,
                            margin: w3cos_std::style::Edges {
                                bottom: WSpacing::Px(-25.0),
                                ..w3cos_std::style::Edges::ZERO
                            },
                            ..Style::default()
                        },
                        Vec::new(),
                    )],
                ),
                Component::boxed(
                    Style {
                        display: WDisp::Block,
                        clear: WClear::Both,
                        height: WDim::Px(25.0),
                        ..Style::default()
                    },
                    Vec::new(),
                ),
            ],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let floating = layout.iter().find(|(_, index)| *index == 2).unwrap().0;
        let cleared = layout.iter().find(|(_, index)| *index == 5).unwrap().0;
        assert_eq!(cleared.y, floating.y + floating.height);
    }

    #[test]
    fn relative_offset_is_preserved_after_float_clearance() {
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                width: WDim::Px(200.0),
                ..Style::default()
            },
            vec![
                Component::boxed(
                    Style {
                        display: WDisp::Block,
                        float: WFloat::Left,
                        min_height: WDim::Px(200.0),
                        width: WDim::Px(200.0),
                        overflow: WOverflow::Auto,
                        ..Style::default()
                    },
                    Vec::new(),
                ),
                Component::boxed(
                    Style {
                        display: WDisp::Block,
                        position: WPos::Relative,
                        clear: WClear::Left,
                        top: WDim::Px(-200.0),
                        width: WDim::Px(200.0),
                        height: WDim::Px(200.0),
                        ..Style::default()
                    },
                    Vec::new(),
                ),
            ],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let floating = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        let cleared = layout.iter().find(|(_, index)| *index == 2).unwrap().0;
        assert_eq!(cleared.y, floating.y);
    }

    #[test]
    fn leading_block_overlaps_or_avoids_a_float_by_formatting_context() {
        let layout = |overflow| {
            compute(
                &Component::boxed(
                    Style {
                        display: WDisp::Block,
                        width: WDim::Px(200.0),
                        height: WDim::Px(200.0),
                        ..Style::default()
                    },
                    vec![
                        Component::boxed(
                            Style {
                                display: WDisp::Block,
                                float: WFloat::Left,
                                width: WDim::Px(50.0),
                                height: WDim::Px(50.0),
                                ..Style::default()
                            },
                            Vec::new(),
                        ),
                        Component::boxed(
                            Style {
                                display: WDisp::Block,
                                overflow,
                                width: WDim::Px(50.0),
                                height: WDim::Px(50.0),
                                ..Style::default()
                            },
                            Vec::new(),
                        ),
                    ],
                ),
                800.0,
                600.0,
            )
            .unwrap()
        };

        let visible = layout(WOverflow::Visible);
        let visible_float = visible.iter().find(|(_, index)| *index == 1).unwrap().0;
        let visible_block = visible.iter().find(|(_, index)| *index == 2).unwrap().0;
        assert_eq!(visible_block.y, visible_float.y);
        assert_eq!(visible_block.x, visible_float.x);

        let hidden = layout(WOverflow::Hidden);
        let hidden_float = hidden.iter().find(|(_, index)| *index == 1).unwrap().0;
        let hidden_block = hidden.iter().find(|(_, index)| *index == 2).unwrap().0;
        assert_eq!(hidden_block.y, hidden_float.y);
        assert_eq!(hidden_block.x, hidden_float.x + hidden_float.width);
    }

    #[test]
    fn float_only_auto_block_collapses_through_adjacent_margins() {
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                ..Style::default()
            },
            vec![
                Component::boxed(
                    Style {
                        display: WDisp::Block,
                        width: WDim::Px(50.0),
                        height: WDim::Px(50.0),
                        margin: w3cos_std::style::Edges {
                            bottom: WSpacing::Px(50.0),
                            ..w3cos_std::style::Edges::ZERO
                        },
                        ..Style::default()
                    },
                    Vec::new(),
                ),
                Component::boxed(
                    Style {
                        display: WDisp::Block,
                        position: WPos::Relative,
                        margin: w3cos_std::style::Edges {
                            top: WSpacing::Px(100.0),
                            ..w3cos_std::style::Edges::ZERO
                        },
                        ..Style::default()
                    },
                    vec![Component::boxed(
                        Style {
                            display: WDisp::Block,
                            float: WFloat::Left,
                            width: WDim::Px(50.0),
                            height: WDim::Px(50.0),
                            margin: w3cos_std::style::Edges {
                                top: WSpacing::Px(100.0),
                                ..w3cos_std::style::Edges::ZERO
                            },
                            ..Style::default()
                        },
                        Vec::new(),
                    )],
                ),
                Component::boxed(
                    Style {
                        display: WDisp::Block,
                        width: WDim::Px(50.0),
                        height: WDim::Px(50.0),
                        margin: w3cos_std::style::Edges {
                            top: WSpacing::Px(150.0),
                            ..w3cos_std::style::Edges::ZERO
                        },
                        ..Style::default()
                    },
                    Vec::new(),
                ),
            ],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let collapsed = layout.iter().find(|(_, index)| *index == 2).unwrap().0;
        let floating = layout.iter().find(|(_, index)| *index == 3).unwrap().0;
        let following = layout.iter().find(|(_, index)| *index == 4).unwrap().0;
        assert_eq!(collapsed.height, 0.0);
        assert_eq!(floating.y, collapsed.y + 100.0);
        assert_eq!(following.y, 200.0);
    }

    #[test]
    fn zero_resolved_percentage_padding_keeps_collapsed_margin_inside_a_float() {
        let empty_block = |margin_top, margin_bottom, padding| {
            Component::boxed(
                Style {
                    display: WDisp::Block,
                    margin: w3cos_std::style::Edges {
                        top: WSpacing::Px(margin_top),
                        bottom: WSpacing::Px(margin_bottom),
                        ..w3cos_std::style::Edges::ZERO
                    },
                    padding,
                    ..Style::default()
                },
                Vec::new(),
            )
        };
        let root = Component::column(
            Style {
                display: WDisp::Block,
                width: WDim::Px(800.0),
                ..Style::default()
            },
            vec![
                Component::boxed(
                    Style {
                        display: WDisp::Block,
                        height: WDim::Px(20.0),
                        margin: w3cos_std::style::Edges {
                            bottom: WSpacing::Px(16.0),
                            ..w3cos_std::style::Edges::ZERO
                        },
                        ..Style::default()
                    },
                    Vec::new(),
                ),
                Component::boxed(
                    Style {
                        display: WDisp::Block,
                        float: WFloat::Left,
                        width: WDim::Px(100.0),
                        ..Style::default()
                    },
                    vec![Component::boxed(
                        Style {
                            display: WDisp::Block,
                            width: WDim::Px(0.0),
                            ..Style::default()
                        },
                        vec![
                            empty_block(0.0, 100.0, w3cos_std::style::Edges::ZERO),
                            empty_block(
                                0.0,
                                0.0,
                                w3cos_std::style::Edges {
                                    top: WSpacing::Percent(100.0),
                                    bottom: WSpacing::Percent(100.0),
                                    ..w3cos_std::style::Edges::ZERO
                                },
                            ),
                            empty_block(100.0, 0.0, w3cos_std::style::Edges::ZERO),
                        ],
                    )],
                ),
            ],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let floating = layout.iter().find(|(_, index)| *index == 2).unwrap().0;
        assert_eq!(floating.y, 36.0);
        assert_eq!(floating.height, 100.0);
    }

    #[test]
    fn positioned_auto_height_bfc_contains_a_nested_float_margin_box() {
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                ..Style::default()
            },
            vec![Component::boxed(
                Style {
                    display: WDisp::Block,
                    position: WPos::Absolute,
                    width: WDim::Px(96.0),
                    ..Style::default()
                },
                vec![Component::boxed(
                    Style {
                        display: WDisp::Block,
                        ..Style::default()
                    },
                    vec![Component::boxed(
                        Style {
                            display: WDisp::Block,
                            float: WFloat::Left,
                            width: WDim::Percent(100.0),
                            height: WDim::Px(48.0),
                            margin: w3cos_std::style::Edges {
                                bottom: WSpacing::Px(48.0),
                                ..w3cos_std::style::Edges::ZERO
                            },
                            ..Style::default()
                        },
                        Vec::new(),
                    )],
                )],
            )],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let container = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        let wrapper = layout.iter().find(|(_, index)| *index == 2).unwrap().0;
        assert_eq!(container.height, 96.0);
        assert_eq!(wrapper.height, 0.0);
    }

    #[test]
    fn float_does_not_advance_following_normal_flow_block() {
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                border_width: 3.0,
                ..Style::default()
            },
            vec![
                Component::boxed(
                    Style {
                        display: WDisp::Block,
                        height: WDim::Px(50.0),
                        ..Style::default()
                    },
                    Vec::new(),
                ),
                Component::boxed(
                    Style {
                        display: WDisp::Flex,
                        float: WFloat::Left,
                        width: WDim::Px(20.0),
                        height: WDim::Px(20.0),
                        ..Style::default()
                    },
                    Vec::new(),
                ),
                Component::boxed(
                    Style {
                        display: WDisp::Block,
                        height: WDim::Px(50.0),
                        margin: w3cos_std::style::Edges {
                            top: WSpacing::Px(50.0),
                            ..w3cos_std::style::Edges::ZERO
                        },
                        ..Style::default()
                    },
                    Vec::new(),
                ),
            ],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let container = layout.iter().find(|(_, index)| *index == 0).unwrap().0;
        let first = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        let floating = layout.iter().find(|(_, index)| *index == 2).unwrap().0;
        let following = layout.iter().find(|(_, index)| *index == 3).unwrap().0;
        assert_eq!(floating.y, first.y + first.height);
        assert_eq!(following.y, first.y + first.height + 50.0);
        assert_eq!(container.height, 156.0);
    }

    #[test]
    fn leading_float_margin_does_not_collapse_with_its_containing_block() {
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                ..Style::default()
            },
            vec![Component::boxed(
                Style {
                    display: WDisp::Block,
                    margin: w3cos_std::style::Edges::all(8.0),
                    ..Style::default()
                },
                vec![
                    Component::boxed(
                        Style {
                            display: WDisp::Flex,
                            float: WFloat::Left,
                            height: WDim::Px(40.0),
                            margin: w3cos_std::style::Edges {
                                top: WSpacing::Px(16.0),
                                right: WSpacing::Px(0.0),
                                bottom: WSpacing::Px(16.0),
                                left: WSpacing::Px(0.0),
                            },
                            ..Style::default()
                        },
                        Vec::new(),
                    ),
                    Component::boxed(
                        Style {
                            display: WDisp::Block,
                            height: WDim::Px(96.0),
                            ..Style::default()
                        },
                        Vec::new(),
                    ),
                ],
            )],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let float = layout.iter().find(|(_, index)| *index == 2).unwrap().0;
        let following = layout.iter().find(|(_, index)| *index == 3).unwrap().0;
        assert_eq!(float.y, 24.0);
        assert_eq!(following.y, 80.0);
    }

    #[test]
    fn empty_leading_block_propagates_its_collapsed_margin_group_to_the_parent() {
        let zero_edges = w3cos_std::style::Edges::ZERO;
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                ..Style::default()
            },
            vec![
                Component::boxed(
                    Style {
                        display: WDisp::Flex,
                        height: WDim::Px(19.2),
                        margin: w3cos_std::style::Edges {
                            bottom: WSpacing::Px(16.0),
                            ..zero_edges
                        },
                        ..Style::default()
                    },
                    Vec::new(),
                ),
                Component::boxed(
                    Style {
                        display: WDisp::Block,
                        width: WDim::Px(192.0),
                        ..Style::default()
                    },
                    vec![
                        Component::boxed(
                            Style {
                                display: WDisp::Block,
                                position: WPos::Absolute,
                                height: WDim::Px(3.0),
                                ..Style::default()
                            },
                            Vec::new(),
                        ),
                        Component::boxed(
                            Style {
                                display: WDisp::Block,
                                margin: w3cos_std::style::Edges {
                                    bottom: WSpacing::Percent(50.0),
                                    ..zero_edges
                                },
                                ..Style::default()
                            },
                            Vec::new(),
                        ),
                        Component::boxed(
                            Style {
                                display: WDisp::Block,
                                height: WDim::Px(3.0),
                                margin: w3cos_std::style::Edges {
                                    top: WSpacing::Px(-96.0),
                                    ..zero_edges
                                },
                                ..Style::default()
                            },
                            Vec::new(),
                        ),
                    ],
                ),
            ],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let paragraph = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        let wrapper = layout.iter().find(|(_, index)| *index == 2).unwrap().0;
        assert_eq!(wrapper.y, paragraph.y + paragraph.height);
    }

    #[test]
    fn leading_empty_block_does_not_offset_following_inline_content() {
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                width: WDim::Px(100.0),
                ..Style::default()
            },
            vec![
                Component::boxed(
                    Style {
                        display: WDisp::Block,
                        margin: w3cos_std::style::Edges::all(50.0),
                        ..Style::default()
                    },
                    Vec::new(),
                ),
                Component::text(
                    "B",
                    Style {
                        display: WDisp::Inline,
                        font_size: 50.0,
                        line_height: 1.2,
                        ..Style::default()
                    },
                ),
            ],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let root = layout.iter().find(|(_, index)| *index == 0).unwrap().0;
        let text = layout.iter().find(|(_, index)| *index == 2).unwrap().0;
        assert_eq!(text.y, root.y);
    }

    #[test]
    fn nested_block_keeps_collapsed_sibling_gap_after_parent_margin() {
        let paragraph = Component::row(
            Style {
                display: WDisp::Flex,
                flex_wrap: WWrap::Wrap,
                font_size: 16.0,
                line_height: 1.25,
                margin: w3cos_std::style::Edges {
                    top: w3cos_std::style::Spacing::Em(1.0),
                    right: w3cos_std::style::Spacing::Px(0.0),
                    bottom: w3cos_std::style::Spacing::Em(1.0),
                    left: w3cos_std::style::Spacing::Px(0.0),
                },
                ..Style::default()
            },
            vec![
                Component::text(
                    "first line",
                    Style {
                        font_size: 16.0,
                        line_height: 1.25,
                        ..Style::default()
                    },
                ),
                Component::text(
                    "\u{2028}",
                    Style {
                        display: WDisp::Inline,
                        width: WDim::Px(0.0),
                        height: WDim::Px(20.0),
                        font_size: 16.0,
                        line_height: 1.25,
                        ..Style::default()
                    },
                ),
                Component::text(
                    "second line",
                    Style {
                        font_size: 16.0,
                        line_height: 1.25,
                        ..Style::default()
                    },
                ),
            ],
        );
        let following = Component::boxed(
            Style {
                display: WDisp::Block,
                height: WDim::Px(100.0),
                margin: w3cos_std::style::Edges {
                    top: w3cos_std::style::Spacing::Px(66.0),
                    right: w3cos_std::style::Spacing::Px(0.0),
                    bottom: w3cos_std::style::Spacing::Px(0.0),
                    left: w3cos_std::style::Spacing::Px(0.0),
                },
                ..Style::default()
            },
            Vec::new(),
        );
        let root = Component::row(
            Style {
                display: WDisp::Block,
                ..Style::default()
            },
            vec![Component::row(
                Style {
                    display: WDisp::Block,
                    margin: w3cos_std::style::Edges::all(8.0),
                    ..Style::default()
                },
                vec![paragraph, following],
            )],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let body = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        let first = layout.iter().find(|(_, index)| *index == 2).unwrap().0;
        let second = layout.iter().find(|(_, index)| *index == 6).unwrap().0;
        assert_eq!(body.y, 16.0);
        assert_eq!(first.y, 16.0);
        assert_eq!(second.y, 122.0);
    }

    #[test]
    fn inline_block_text_shrink_wraps_content_and_padding() {
        let style = Style {
            display: WDisp::InlineBlock,
            box_sizing: WBoxSizing::BorderBox,
            font_size: 13.0,
            line_height: 1.3,
            padding: w3cos_std::style::Edges::xy(10.0, 4.0),
            white_space: WWhiteSpace::NoWrap,
            ..Style::default()
        };
        let layout = compute(
            &Component::boxed(
                Style {
                    display: WDisp::Block,
                    width: WDim::Px(300.0),
                    ..Style::default()
                },
                vec![Component::text("首次入驻", style.clone())],
            ),
            300.0,
            200.0,
        )
        .unwrap();
        let badge = layout[1].0;
        let expected_width = text_intrinsic_size("首次入驻", &style).0;
        let pad_y = style.padding_lengths().top + style.padding_lengths().bottom;
        assert!(
            (badge.width - expected_width).abs() < 1.0,
            "inline-block width should equal content plus padding, got {} expected {expected_width}",
            badge.width
        );
        assert!(
            badge.height + 1.0 >= style.font_size + pad_y
                && badge.height < style.font_size * style.line_height * 2.0 + pad_y,
            "inline-block height should stay on one line box plus padding, got {}",
            badge.height
        );
        assert!(
            badge.width < 300.0,
            "inline-block must shrink-wrap instead of filling the containing block, got {}",
            badge.width
        );
    }

    #[test]
    fn inline_content_box_text_counts_padding_once() {
        let style = Style {
            display: WDisp::Inline,
            box_sizing: WBoxSizing::ContentBox,
            padding: w3cos_std::style::Edges::xy(16.0, 0.0),
            white_space: WWhiteSpace::NoWrap,
            ..Style::default()
        };
        let layout = compute(
            &Component::boxed(
                Style {
                    display: WDisp::Block,
                    width: WDim::Px(300.0),
                    ..Style::default()
                },
                vec![Component::text("This test has failed.", style.clone())],
            ),
            300.0,
            200.0,
        )
        .unwrap();
        let box_width = layout[1].0.width;
        let content_width = text_intrinsic_size(
            "This test has failed.",
            &Style {
                padding: w3cos_std::style::Edges::ZERO,
                ..style.clone()
            },
        )
        .0;
        let horizontal_padding = style.padding_lengths().left + style.padding_lengths().right;

        assert!(
            (box_width - content_width - horizontal_padding).abs() < 1.0,
            "content-box inline width should include its padding exactly once, got {box_width} for content {content_width}"
        );
    }

    #[test]
    fn normal_inline_content_box_text_counts_padding_once() {
        let base = Style {
            display: WDisp::Inline,
            box_sizing: WBoxSizing::ContentBox,
            white_space: WWhiteSpace::Normal,
            ..Style::default()
        };
        let padded = Style {
            padding: w3cos_std::style::Edges {
                top: w3cos_std::style::Spacing::Px(0.0),
                right: w3cos_std::style::Spacing::Px(16.0),
                bottom: w3cos_std::style::Spacing::Px(0.0),
                left: w3cos_std::style::Spacing::Px(0.0),
            },
            ..base.clone()
        };
        let width = |style: Style| {
            compute(
                &Component::boxed(
                    Style {
                        display: WDisp::Block,
                        width: WDim::Px(300.0),
                        ..Style::default()
                    },
                    vec![Component::text("This test has failed.", style)],
                ),
                300.0,
                200.0,
            )
            .unwrap()[1]
                .0
                .width
        };

        let content_width = width(base);
        let padded_width = width(padded);
        assert!(
            (padded_width - content_width - 16.0).abs() < 1.0,
            "normal inline padding should contribute once, got {content_width} -> {padded_width}"
        );
    }

    #[test]
    fn normal_inline_vertical_edges_wrap_the_content_area_not_line_height() {
        let inline = Component::text(
            "X",
            Style {
                display: WDisp::Inline,
                font_size: 100.0,
                line_height: 1.5,
                padding: w3cos_std::style::Edges::xy(0.0, 50.0),
                ..Style::default()
            },
        );
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                ..Style::default()
            },
            vec![inline],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let inline = layout.iter().find(|(_, index)| *index == 1).unwrap().0;

        assert_eq!(inline.height, 200.0);
    }

    #[test]
    fn block_text_content_box_counts_em_padding_once() {
        let text_style = Style {
            display: WDisp::Block,
            padding: w3cos_std::style::Edges {
                top: WSpacing::Em(2.0),
                right: WSpacing::Em(2.0),
                bottom: WSpacing::Em(2.0),
                left: WSpacing::Em(2.0),
            },
            ..Style::default()
        };
        let layout = compute(
            &Component::boxed(
                Style {
                    display: WDisp::Block,
                    width: WDim::Px(800.0),
                    height: WDim::Px(600.0),
                    ..Style::default()
                },
                vec![Component::text(
                    "There should be one line.",
                    text_style.clone(),
                )],
            ),
            800.0,
            600.0,
        )
        .unwrap();

        let text = layout[1].0;
        assert!(
            (text.height - 83.2).abs() < 0.1,
            "block text should have one 19.2px line plus 64px padding, got {}",
            text.height
        );
    }

    #[test]
    fn block_inline_image_uses_line_height_strut_and_vertical_align() {
        let image = Component::image(
            "line-box.png",
            Style {
                display: WDisp::InlineBlock,
                width: WDim::Px(96.0),
                height: WDim::Px(15.0),
                align_self: WAlignSelf::FlexEnd,
                ..Style::default()
            },
        );
        let layout = compute(
            &Component::row(
                Style {
                    display: WDisp::Block,
                    width: WDim::Px(96.0),
                    font_size: 16.0,
                    line_height: 6.0,
                    border_width: 3.0,
                    ..Style::default()
                },
                vec![image],
            ),
            800.0,
            600.0,
        )
        .unwrap();

        assert_eq!(layout[0].0.height, 102.0);
        assert_eq!(layout[1].0.y, 84.0);
    }

    #[test]
    fn empty_painted_inline_boxes_use_the_surrounding_line_height() {
        let inline = |border_left, margin_left, children| {
            Component::row(
                Style {
                    display: WDisp::Inline,
                    border_left_width: Some(border_left),
                    margin: w3cos_std::style::Edges {
                        left: WSpacing::Px(margin_left),
                        ..w3cos_std::style::Edges::ZERO
                    },
                    ..Style::default()
                },
                children,
            )
        };
        let layout = compute(
            &Component::row(
                Style {
                    display: WDisp::Block,
                    ..Style::default()
                },
                vec![
                    inline(5.0, 0.0, vec![inline(5.0, 50.0, Vec::new())]),
                    Component::text(
                        "Filler Text",
                        Style {
                            display: WDisp::Inline,
                            ..Style::default()
                        },
                    ),
                ],
            ),
            800.0,
            600.0,
        )
        .unwrap();

        let line = layout.iter().find(|(_, index)| *index == 0).unwrap().0;
        let outer = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        let inner = layout.iter().find(|(_, index)| *index == 2).unwrap().0;
        assert_eq!(outer.y, line.y);
        assert_eq!(inner.y, line.y);
        assert_eq!(outer.height, line.height);
        assert_eq!(inner.height, line.height);
    }

    #[test]
    fn empty_painted_inline_fragment_falls_back_to_its_computed_line_height() {
        let root = Component::row(
            Style {
                display: WDisp::Block,
                width: WDim::Px(200.0),
                ..Style::default()
            },
            vec![Component::row(
                Style {
                    display: WDisp::Inline,
                    border_right_width: Some(200.0),
                    margin: w3cos_std::style::Edges {
                        right: WSpacing::Px(-200.0),
                        ..w3cos_std::style::Edges::ZERO
                    },
                    font_size: 200.0,
                    line_height: 1.0,
                    ..Style::default()
                },
                Vec::new(),
            )],
        );
        let flat = pre_flatten(&root);
        let mut layout = vec![
            (
                LayoutRect {
                    x: 8.0,
                    y: 51.2,
                    width: 200.0,
                    height: 0.0,
                },
                0,
            ),
            (
                LayoutRect {
                    x: 8.0,
                    y: 51.2,
                    width: 200.0,
                    height: 0.0,
                },
                1,
            ),
        ];
        project_empty_painted_inline_boxes(&mut layout, &flat);

        let fragment = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        assert_eq!(fragment.height, 200.0);
    }

    #[test]
    fn inline_container_vertical_border_paints_outside_the_line_box() {
        let layout = compute(
            &Component::row(
                Style {
                    display: WDisp::Block,
                    width: WDim::Px(200.0),
                    border_top_width: Some(20.0),
                    font_size: 20.0,
                    line_height: 1.0,
                    ..Style::default()
                },
                vec![Component::row(
                    Style {
                        display: WDisp::Inline,
                        border_top_width: Some(20.0),
                        margin: w3cos_std::style::Edges {
                            top: WSpacing::Px(50.0),
                            ..w3cos_std::style::Edges::ZERO
                        },
                        font_size: 20.0,
                        line_height: 1.0,
                        ..Style::default()
                    },
                    vec![Component::text(
                        "XXXXXXXXXX",
                        Style {
                            display: WDisp::Inline,
                            font_size: 20.0,
                            line_height: 1.0,
                            ..Style::default()
                        },
                    )],
                )],
            ),
            800.0,
            600.0,
        )
        .unwrap();

        let parent = layout.iter().find(|(_, index)| *index == 0).unwrap().0;
        let inline = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        let text = layout.iter().find(|(_, index)| *index == 2).unwrap().0;
        assert_eq!(inline.y, parent.y);
        assert_eq!(text.y, inline.y + 20.0);
        assert_eq!(inline.height, 40.0);
    }

    #[test]
    fn nested_inline_content_applies_half_leading_once() {
        let layout = compute(
            &Component::row(
                Style {
                    display: WDisp::Block,
                    width: WDim::Px(200.0),
                    font_size: 20.0,
                    line_height: 1.2,
                    ..Style::default()
                },
                vec![Component::row(
                    Style {
                        display: WDisp::Inline,
                        font_size: 20.0,
                        line_height: 1.2,
                        ..Style::default()
                    },
                    vec![Component::text(
                        "text",
                        Style {
                            display: WDisp::Inline,
                            font_size: 20.0,
                            line_height: 1.2,
                            ..Style::default()
                        },
                    )],
                )],
            ),
            800.0,
            600.0,
        )
        .unwrap();

        let inline = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        let text = layout.iter().find(|(_, index)| *index == 2).unwrap().0;
        assert_eq!(text.y, inline.y);
    }

    #[test]
    fn table_cell_inline_padding_does_not_move_the_content_line() {
        let root = Component::row(
            Style {
                display: WDisp::TableCell,
                ..Style::default()
            },
            vec![Component::row(
                Style {
                    display: WDisp::Inline,
                    padding: w3cos_std::style::Edges {
                        top: WSpacing::Px(160.0),
                        ..w3cos_std::style::Edges::ZERO
                    },
                    ..Style::default()
                },
                vec![Component::text("control", Style::default())],
            )],
        );
        let flat = pre_flatten(&root);
        let mut layout = vec![
            (
                LayoutRect {
                    x: 8.0,
                    y: 192.0,
                    width: 200.0,
                    height: 16.0,
                },
                0,
            ),
            (
                LayoutRect {
                    x: 8.0,
                    y: 112.0,
                    width: 200.0,
                    height: 176.0,
                },
                1,
            ),
            (
                LayoutRect {
                    x: 8.0,
                    y: 272.0,
                    width: 200.0,
                    height: 16.0,
                },
                2,
            ),
        ];
        project_table_cell_inline_vertical_padding(&mut layout, &flat);

        let inline = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        let text = layout.iter().find(|(_, index)| *index == 2).unwrap().0;
        assert_eq!(inline.y, 32.0);
        assert_eq!(text.y, 192.0);
    }

    #[test]
    fn rtl_fixed_block_aligns_its_margin_box_to_the_content_right_edge() {
        let layout = compute(
            &Component::row(
                Style {
                    display: WDisp::Block,
                    width: WDim::Px(96.0),
                    border_right_width: Some(5.0),
                    direction: w3cos_std::style::TextDirection::Rtl,
                    ..Style::default()
                },
                vec![Component::row(
                    Style {
                        display: WDisp::Block,
                        width: WDim::Px(96.0),
                        border_right_width: Some(5.0),
                        ..Style::default()
                    },
                    Vec::new(),
                )],
            ),
            800.0,
            600.0,
        )
        .unwrap();

        let parent = layout.iter().find(|(_, index)| *index == 0).unwrap().0;
        let child = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        assert_eq!(parent.x + parent.width - 5.0, child.x + child.width);
    }

    #[test]
    fn collapsible_line_end_whitespace_is_removed_before_centering() {
        let inline_text = |content| {
            Component::text(
                content,
                Style {
                    display: WDisp::Inline,
                    ..Style::default()
                },
            )
        };
        let root = Component::row(
            Style {
                display: WDisp::Flex,
                flex_wrap: WWrap::Wrap,
                justify_content: WJustify::Center,
                ..Style::default()
            },
            vec![inline_text("O"), inline_text(" "), inline_text("B")],
        );
        let flat = pre_flatten(&root);
        let mut layouts = vec![
            (LayoutRect { x: 0.0, y: 0.0, width: 200.0, height: 200.0 }, 0),
            (LayoutRect { x: 0.0, y: 0.0, width: 100.0, height: 100.0 }, 1),
            (LayoutRect { x: 100.0, y: 0.0, width: 100.0, height: 100.0 }, 2),
            (LayoutRect { x: 50.0, y: 100.0, width: 100.0, height: 100.0 }, 3),
        ];

        project_collapsible_line_end_whitespace(&mut layouts, &flat);

        assert_eq!(layouts[1].0.x, 50.0);
        assert_eq!(layouts[2].0.width, 0.0);
        assert_eq!(layouts[3].0.x, 50.0);
    }

    #[test]
    fn baseline_aligned_tall_image_reserves_the_inline_strut_descent() {
        let layout = compute(
            &Component::row(
                Style {
                    display: WDisp::Block,
                    width: WDim::Px(100.0),
                    font_size: 40.0,
                    line_height: 1.2,
                    ..Style::default()
                },
                vec![Component::image(
                    "line-box.png",
                    Style {
                        display: WDisp::InlineBlock,
                        width: WDim::Px(100.0),
                        height: WDim::Px(100.0),
                        ..Style::default()
                    },
                )],
            ),
            800.0,
            600.0,
        )
        .unwrap();

        assert!(
            (layout[0].0.height - 109.6).abs() < 0.01,
            "baseline line height was {}",
            layout[0].0.height
        );
        assert_eq!(layout[1].0.height, 100.0);
    }

    #[test]
    fn inline_flex_badge_shrink_wraps_like_browser_css() {
        let style = Style {
            display: WDisp::InlineFlex,
            box_sizing: WBoxSizing::BorderBox,
            font_size: 12.0,
            line_height: 1.4,
            padding: w3cos_std::style::Edges::xy(8.0, 2.0),
            white_space: WWhiteSpace::NoWrap,
            ..Style::default()
        };
        let layout = compute(
            &Component::boxed(
                Style {
                    display: WDisp::Block,
                    width: WDim::Px(300.0),
                    ..Style::default()
                },
                vec![Component::text("首次入驻", style.clone())],
            ),
            300.0,
            200.0,
        )
        .unwrap();
        let badge = layout[1].0;
        let expected_width = text_intrinsic_size("首次入驻", &style).0;
        let pad_y = style.padding_lengths().top + style.padding_lengths().bottom;
        assert!(
            (badge.width - expected_width).abs() < 1.0,
            "inline-flex badge width should equal content plus padding, got {} expected {expected_width}",
            badge.width
        );
        assert!(
            badge.height + 1.0 >= style.font_size + pad_y
                && badge.height < style.font_size * style.line_height * 2.0 + pad_y,
            "inline-flex badge height should stay on one line box plus padding, got {}",
            badge.height
        );
        assert!(
            badge.width < 300.0,
            "inline-flex must shrink-wrap instead of filling the containing block, got {}",
            badge.width
        );
    }

    #[test]
    fn inline_block_container_uses_browser_cjk_normal_line_box() {
        let layout = compute(
            &Component::boxed(
                Style {
                    display: WDisp::Block,
                    width: WDim::Px(300.0),
                    ..Style::default()
                },
                vec![Component::boxed(
                    Style {
                        display: WDisp::InlineBlock,
                        padding: w3cos_std::style::Edges::xy(10.0, 4.0),
                        ..Style::default()
                    },
                    vec![Component::text(
                        "首次入驻",
                        Style {
                            font_size: 13.0,
                            ..Style::default()
                        },
                    )],
                )],
            ),
            300.0,
            200.0,
        )
        .unwrap();
        let badge = layout[1].0;
        assert!(
            (badge.height - 26.5).abs() < 0.01,
            "inline-block CJK normal line box should preserve Chromium subpixels, got {}",
            badge.height
        );
    }

    #[test]
    fn wrapping_text_shrinks_to_column_content_width() {
        let text = "SH12345 预计 15:42 到达，等待费申诉缺 1 项材料。";
        let l = compute(
            &Component::column(
                Style {
                    display: WDisp::Flex,
                    flex_direction: WDir::Column,
                    box_sizing: WBoxSizing::BorderBox,
                    padding: w3cos_std::style::Edges::all(16.0),
                    width: WDim::Px(370.0),
                    ..Style::default()
                },
                vec![Component::text(
                    text,
                    Style {
                        font_size: 15.0,
                        line_height: 1.4,
                        ..Style::default()
                    },
                )],
            ),
            402.0,
            874.0,
        )
        .unwrap();
        let text_rect = l.iter().find(|(_, idx)| *idx == 1).unwrap().0;
        assert!(
            (text_rect.width - 338.0).abs() < 2.0,
            "wrapping text width {} expected parent content width 338",
            text_rect.width
        );
        assert!(
            text_rect.height > 21.0,
            "text should wrap to multiple lines"
        );
    }

    #[test]
    fn lowered_inline_text_wraps_to_column_content_width() {
        let text = "AI 正在理解你的业务，识别角色、资料和流程，并进行结构校验。";
        let layout = compute(
            &Component::column(
                Style {
                    display: WDisp::Flex,
                    flex_direction: WDir::Column,
                    width: WDim::Px(300.0),
                    ..Style::default()
                },
                vec![Component::text(
                    text,
                    Style {
                        display: WDisp::Inline,
                        font_size: 14.0,
                        line_height: 1.55,
                        ..Style::default()
                    },
                )],
            ),
            402.0,
            874.0,
        )
        .unwrap();
        let text_rect = layout.iter().find(|(_, idx)| *idx == 1).unwrap().0;
        assert!(
            (text_rect.width - 300.0).abs() < 1.0,
            "inline text should use its containing block width, got {}",
            text_rect.width
        );
        assert!(
            text_rect.height > 22.0,
            "inline CJK text should wrap inside the containing block"
        );
    }

    #[test]
    fn preformatted_block_text_fills_its_containing_block_width() {
        let layout = compute(
            &Component::column(
                Style {
                    display: WDisp::Block,
                    width: WDim::Px(240.0),
                    ..Style::default()
                },
                vec![Component::text(
                    "Line 1\nLine 2",
                    Style {
                        display: WDisp::Block,
                        white_space: WWhiteSpace::Pre,
                        ..Style::default()
                    },
                )],
            ),
            800.0,
            600.0,
        )
        .unwrap();
        let text_rect = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        assert_eq!(text_rect.width, 240.0);
    }

    #[test]
    fn inline_flex_text_wrapper_shrink_fits_inside_a_block() {
        let layout = compute(
            &Component::column(
                Style {
                    display: WDisp::Block,
                    width: WDim::Px(300.0),
                    ..Style::default()
                },
                vec![Component::row(
                    Style {
                        display: WDisp::InlineFlex,
                        padding: w3cos_std::style::Edges::xy(16.0, 0.0),
                        ..Style::default()
                    },
                    vec![
                        Component::text("This test has failed.", Style::default()),
                        Component::text("\u{00a0}\u{00a0}", Style::default()),
                    ],
                )],
            ),
            800.0,
            600.0,
        )
        .unwrap();
        let wrapper = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        assert!(
            wrapper.width < 300.0,
            "inline flex wrapper must shrink-fit, got {wrapper:?}"
        );
        assert!(
            wrapper.width > 150.0,
            "wrapper lost its text width: {wrapper:?}"
        );
    }

    #[test]
    fn block_flow_places_a_generated_inline_line_directly_before_a_block_text_leaf() {
        let layout = compute(
            &Component::column(
                Style {
                    display: WDisplay::Block,
                    flex_direction: WDir::Row,
                    margin: w3cos_std::style::Edges::all(8.0),
                    ..Style::default()
                },
                vec![Component::row(
                    Style {
                        display: WDisplay::Block,
                        flex_direction: WDir::Row,
                        border_width: 2.0,
                        ..Style::default()
                    },
                    vec![
                        Component::text(
                            "0",
                            Style {
                                display: WDisplay::Inline,
                                flex_direction: WDir::Row,
                                ..Style::default()
                            },
                        ),
                        Component::text(
                            "0.0",
                            Style {
                                display: WDisplay::Block,
                                flex_direction: WDir::Row,
                                ..Style::default()
                            },
                        ),
                    ],
                )],
            ),
            800.0,
            600.0,
        )
        .unwrap();
        let first = layout.iter().find(|(_, index)| *index == 2).unwrap().0;
        let second = layout.iter().find(|(_, index)| *index == 3).unwrap().0;
        let expected_line_height = 16.0 * 1.2;
        assert!(
            (first.height - expected_line_height).abs() < 0.01
                && (second.height - expected_line_height).abs() < 0.01,
            "generated Latin line boxes should use CSS line-height {expected_line_height}: first={first:?}, second={second:?}"
        );
        assert!(
            (second.y - (first.y + first.height)).abs() < 0.01,
            "block child should follow the anonymous generated line without a gap: first={first:?}, second={second:?}"
        );
    }

    #[test]
    fn empty_decorated_inline_establishes_a_line_before_a_block() {
        let layout = compute(
            &Component::row(
                Style {
                    display: WDisplay::Block,
                    width: WDim::Px(800.0),
                    font_size: 20.0,
                    line_height: 1.0,
                    ..Style::default()
                },
                vec![
                    Component::row(
                        Style {
                            display: WDisplay::Inline,
                            border_left_width: Some(20.0),
                            font_size: 20.0,
                            line_height: 1.0,
                            ..Style::default()
                        },
                        Vec::new(),
                    ),
                    Component::text(
                        "FAIL",
                        Style {
                            display: WDisplay::Block,
                            width: WDim::Px(80.0),
                            font_size: 20.0,
                            line_height: 1.0,
                            ..Style::default()
                        },
                    ),
                    Component::text(
                        "PASS",
                        Style {
                            display: WDisplay::Block,
                            position: WPos::Absolute,
                            top: WDim::Px(20.0),
                            font_size: 20.0,
                            line_height: 1.0,
                            ..Style::default()
                        },
                    ),
                ],
            ),
            800.0,
            600.0,
        )
        .unwrap();

        let inline = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        let block = layout.iter().find(|(_, index)| *index == 2).unwrap().0;
        let absolute = layout.iter().find(|(_, index)| *index == 3).unwrap().0;
        assert_eq!(inline.height, 20.0, "inline={inline:?}, block={block:?}");
        assert_eq!(block.width, 80.0, "block={block:?}");
        assert_eq!(
            block.y,
            inline.y + inline.height,
            "inline={inline:?}, block={block:?}"
        );
        assert!(absolute.width > 0.0, "absolute={absolute:?}");
    }

    #[test]
    fn coalesced_generated_nowrap_line_contributes_to_following_block_flow() {
        use w3cos_dom::{Document, stylesheet};

        stylesheet::clear_rules();
        stylesheet::register_rule("body", &[("white-space", "nowrap")]);
        stylesheet::register_rule("#test", &[("counter-reset", "item")]);
        stylesheet::register_rule("#test span", &[("counter-increment", "item")]);
        stylesheet::register_rule("#test span::before", &[("content", "counter(item)")]);

        let mut document = Document::new();
        let generated_line = document.create_element("div");
        generated_line.set_attribute(&mut document, "id", "test");
        for _ in 0..3 {
            let span = document.create_element("span");
            generated_line.append_child(&mut document, span);
        }
        let reference_line = document.create_element("div");
        let reference_text = document.create_text_node("1 2 3");
        reference_line.append_child(&mut document, reference_text);
        document.body().append_child(&mut document, generated_line);
        document.body().append_child(&mut document, reference_line);

        let component = document.to_component_tree();
        let layout = compute(&component, 800.0, 600.0).unwrap();
        let first = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        let second = layout.iter().find(|(_, index)| *index == 3).unwrap().0;
        assert!(
            second.y >= first.y + first.height - 0.01,
            "generated line must advance the next block: first={first:?}, second={second:?}, component={component:#?}"
        );
        stylesheet::clear_rules();
    }

    #[test]
    fn anonymous_inline_text_fragments_keep_their_intrinsic_row_widths() {
        use w3cos_dom::{Document, stylesheet};

        stylesheet::clear_rules();
        stylesheet::register_rule("span", &[("color", "green")]);

        let mut document = Document::new();
        let table = document.create_element("table");
        let row = document.create_element("tr");
        let cell = document.create_element("td");
        let label = document.create_text_node("(Control: ");
        let span = document.create_element("span");
        let result = document.create_text_node("PASSED)");
        span.append_child(&mut document, result);
        cell.append_child(&mut document, label);
        cell.append_child(&mut document, span);
        row.append_child(&mut document, cell);
        table.append_child(&mut document, row);
        document.body().append_child(&mut document, table);

        let component = document.to_component_tree();
        let layout = compute(&component, 800.0, 600.0).unwrap();
        let flat = pre_flatten(&component);
        let text_index = |expected: &str| {
            flat.iter()
                .position(|node| {
                    matches!(node.kind, ComponentKind::Text { content } if content == expected)
                })
                .expect("text component")
        };
        let label_index = text_index("(Control: ");
        let result_index = text_index("PASSED)");
        let label = layout
            .iter()
            .find(|(_, index)| *index == label_index)
            .unwrap()
            .0;
        let result = layout
            .iter()
            .find(|(_, index)| *index == result_index)
            .unwrap()
            .0;
        let expected_label_width = text_intrinsic_size("(Control: ", &Style::default()).0;
        assert!(
            label.width >= expected_label_width - 0.01,
            "the first inline fragment must not be shrunk by its sibling: label={label:?}, result={result:?}, expected_label_width={expected_label_width}, component={component:#?}"
        );
        assert!(
            result.x >= label.x + label.width - 0.01,
            "inline fragments must remain adjacent without overlap: label={label:?}, result={result:?}"
        );
        stylesheet::clear_rules();
    }

    #[test]
    fn auto_width_css_table_shrink_wraps_its_rows() {
        let cell = |label: &str| {
            Component::text(
                label,
                Style {
                    display: WDisp::TableCell,
                    background: Color::WHITE,
                    border_width: 3.0,
                    border_color: Color::WHITE,
                    ..Style::default()
                },
            )
        };
        let table = Component::boxed(
            Style {
                display: WDisp::Table,
                background: Color::rgb(255, 0, 0),
                ..Style::default()
            },
            vec![
                Component::row(
                    Style {
                        display: WDisp::TableRow,
                        ..Style::default()
                    },
                    vec![cell("P"), cell("A"), cell("S"), cell("S")],
                ),
                Component::row(
                    Style {
                        display: WDisp::TableRow,
                        ..Style::default()
                    },
                    vec![cell("P"), cell("A"), cell("S"), cell("S")],
                ),
            ],
        );
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                width: WDim::Px(800.0),
                ..Style::default()
            },
            vec![table],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let table = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        let final_cell = layout.iter().find(|(_, index)| *index == 6).unwrap().0;
        assert!(
            table.width < 100.0,
            "auto table should shrink-wrap instead of stretching: {table:?}"
        );
        assert!(
            final_cell.x + final_cell.width <= table.x + table.width + 0.01,
            "the table max-content width must include cell borders: table={table:?}, final_cell={final_cell:?}"
        );
    }

    #[test]
    fn empty_table_grid_still_uses_its_caption_max_content_width() {
        let caption = Component::boxed(
            Style {
                display: WDisp::TableCaption,
                ..Style::default()
            },
            vec![Component::text("PASS PASS", Style::default())],
        );
        let empty_cell = Component::boxed(
            Style {
                display: WDisp::TableCell,
                ..Style::default()
            },
            vec![],
        );
        let row = Component::row(
            Style {
                display: WDisp::TableRow,
                ..Style::default()
            },
            vec![empty_cell],
        );
        let table = Component::boxed(
            Style {
                display: WDisp::Table,
                ..Style::default()
            },
            vec![caption.clone(), row],
        );

        assert_eq!(
            table_track_max_content_width(&table),
            component_max_content_width(&caption)
        );
    }

    #[test]
    fn table_caption_aligns_with_the_table_border_edge_not_cell_spacing() {
        let caption = Component::boxed(
            Style {
                display: WDisp::TableCaption,
                width: WDim::Px(50.0),
                height: WDim::Px(100.0),
                ..Style::default()
            },
            Vec::new(),
        );
        let table = Component::boxed(
            Style {
                display: WDisp::Table,
                border_spacing_x: 2.0,
                border_spacing_y: 2.0,
                ..Style::default()
            },
            vec![caption],
        );

        let layout = compute(&table, 800.0, 600.0).unwrap();
        let table = layout.iter().find(|(_, index)| *index == 0).unwrap().0;
        let caption = layout.iter().find(|(_, index)| *index == 1).unwrap().0;

        assert_eq!((caption.x, caption.y), (table.x, table.y));
    }

    #[test]
    fn empty_column_group_projects_over_its_implicit_track_and_rows() {
        let column_group = Component::boxed(
            Style {
                display: WDisp::TableColumnGroup,
                ..Style::default()
            },
            Vec::new(),
        );
        let row = Component::row(
            Style {
                display: WDisp::TableRow,
                ..Style::default()
            },
            vec![Component::boxed(
                Style {
                    display: WDisp::TableCell,
                    width: WDim::Px(96.0),
                    height: WDim::Px(192.0),
                    ..Style::default()
                },
                Vec::new(),
            )],
        );
        let table = Component::boxed(
            Style {
                display: WDisp::Table,
                table_layout_fixed: true,
                border_spacing_x: 0.0,
                border_spacing_y: 0.0,
                ..Style::default()
            },
            vec![column_group, row],
        );

        let layout = compute(&table, 800.0, 600.0).unwrap();
        let group = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        let row = layout.iter().find(|(_, index)| *index == 2).unwrap().0;

        assert_eq!(group, row);
    }

    #[test]
    fn separated_fixed_table_rows_include_the_outer_spacing_gutters() {
        let cell = || {
            Component::boxed(
                Style {
                    display: WDisp::TableCell,
                    height: WDim::Px(48.0),
                    ..Style::default()
                },
                Vec::new(),
            )
        };
        let row = || {
            Component::row(
                Style {
                    display: WDisp::TableRow,
                    ..Style::default()
                },
                vec![cell(), cell()],
            )
        };
        let table = Component::boxed(
            Style {
                display: WDisp::Table,
                table_layout_fixed: true,
                width: WDim::Px(96.0),
                border_spacing_x: 2.0,
                border_spacing_y: 2.0,
                ..Style::default()
            },
            vec![Component::row(
                Style {
                    display: WDisp::TableRowGroup,
                    ..Style::default()
                },
                vec![row(), row()],
            )],
        );

        assert_eq!(
            fixed_table_track_widths(&table, None),
            Some(vec![45.0, 45.0])
        );
        let layout = compute(&table, 800.0, 600.0).unwrap();
        let rect = |index| layout.iter().find(|(_, item)| *item == index).unwrap().0;
        assert_eq!(rect(1).width, 96.0);
        assert_eq!(rect(2).width, 96.0);
        assert_eq!(rect(3).x, rect(2).x + 2.0);
        assert_eq!(rect(4).x, rect(3).x + rect(3).width + 2.0);
    }

    #[test]
    fn block_table_wrapper_em_margins_use_the_parent_font() {
        let em_margin = |value| w3cos_std::style::Edges {
            top: WSpacing::Em(value),
            right: WSpacing::Em(value),
            bottom: WSpacing::Em(value),
            left: WSpacing::Em(value),
        };
        let float_margin = w3cos_std::style::Edges {
            top: WSpacing::Em(0.5),
            right: WSpacing::Em(1.0),
            bottom: WSpacing::Em(0.5),
            left: WSpacing::Em(1.0),
        };
        let child = |display, float, margin| {
            Component::boxed(
                Style {
                    display,
                    float,
                    font_size: 32.0,
                    margin,
                    ..Style::default()
                },
                vec![Component::text("same", Style::default())],
            )
        };
        let layout_for = |child| {
            let root = Component::boxed(
                Style {
                    display: WDisp::Block,
                    width: WDim::Px(800.0),
                    font_size: 16.0,
                    ..Style::default()
                },
                vec![child],
            );
            compute(&root, 800.0, 600.0)
                .unwrap()
                .into_iter()
                .find(|(_, index)| *index == 1)
                .unwrap()
                .0
        };

        let table = layout_for(child(WDisp::Table, WFloat::None, em_margin(1.0)));
        let floating = layout_for(child(WDisp::Block, WFloat::Left, float_margin));
        assert_eq!(table.y, 16.0);
        assert_eq!(table.y, floating.y);
        assert_eq!(table.x, 32.0);
        assert_eq!(table.x, floating.x);
    }

    #[test]
    fn definite_table_height_stretches_rows_and_bottom_aligns_cell_content() {
        let image = Component::image(
            "stripe.png",
            Style {
                display: WDisp::InlineBlock,
                width: WDim::Percent(100.0),
                height: WDim::Px(15.0),
                ..Style::default()
            },
        );
        let cell = Component::boxed(
            Style {
                display: WDisp::TableCell,
                width: WDim::Px(200.0),
                border_width: 3.0,
                align_self: WAlignSelf::FlexEnd,
                ..Style::default()
            },
            vec![image],
        );
        let table = Component::boxed(
            Style {
                display: WDisp::Table,
                height: WDim::Px(206.0),
                ..Style::default()
            },
            vec![Component::row(
                Style {
                    display: WDisp::TableRow,
                    ..Style::default()
                },
                vec![cell],
            )],
        );

        let layout = compute(&table, 800.0, 600.0).unwrap();
        let table_rect = layout.iter().find(|(_, index)| *index == 0).unwrap().0;
        let row_rect = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        let cell_rect = layout.iter().find(|(_, index)| *index == 2).unwrap().0;
        let image_rect = layout.iter().find(|(_, index)| *index == 3).unwrap().0;
        assert_eq!(table_rect.height, 206.0);
        assert_eq!(row_rect.height, 206.0);
        assert_eq!(cell_rect.height, 206.0);
        assert_eq!(
            image_rect.y + image_rect.height,
            cell_rect.y + cell_rect.height - 3.0
        );
    }

    #[test]
    fn table_row_group_stacks_rows_and_stretches_to_table_width() {
        let row = || {
            Component::row(
                Style {
                    display: WDisp::TableRow,
                    ..Style::default()
                },
                vec![Component::text(
                    "cell",
                    Style {
                        display: WDisp::TableCell,
                        height: WDim::Px(48.0),
                        ..Style::default()
                    },
                )],
            )
        };
        let group = Component::row(
            Style {
                display: WDisp::TableRowGroup,
                ..Style::default()
            },
            vec![row(), row()],
        );
        let table = Component::boxed(
            Style {
                display: WDisp::Table,
                width: WDim::Px(96.0),
                ..Style::default()
            },
            vec![group],
        );

        let layout = compute(&table, 800.0, 600.0).unwrap();
        let group = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        let first_row = layout.iter().find(|(_, index)| *index == 2).unwrap().0;
        let second_row = layout.iter().find(|(_, index)| *index == 4).unwrap().0;
        assert_eq!((group.width, group.height), (96.0, 96.0));
        assert_eq!(second_row.y, first_row.y + first_row.height);
    }

    #[test]
    fn collapsed_row_border_matches_equivalent_row_group_border_geometry() {
        let cell = || {
            Component::boxed(
                Style {
                    display: WDisp::TableCell,
                    border_collapse: true,
                    border_width: 10.0,
                    ..Style::default()
                },
                vec![Component::text("cell", Style::default())],
            )
        };
        let row = |border_width: f32| {
            Component::row(
                Style {
                    display: WDisp::TableRow,
                    border_collapse: true,
                    border_width,
                    ..Style::default()
                },
                vec![cell(), cell(), cell()],
            )
        };
        let group = |border_width: f32, rows: Vec<Component>| {
            Component::boxed(
                Style {
                    display: WDisp::TableRowGroup,
                    border_collapse: true,
                    border_width,
                    ..Style::default()
                },
                rows,
            )
        };
        let table = |groups: Vec<Component>| {
            Component::boxed(
                Style {
                    display: WDisp::Table,
                    border_collapse: true,
                    border_spacing_x: 2.0,
                    border_spacing_y: 2.0,
                    ..Style::default()
                },
                groups,
            )
        };

        let row_border = compute(
            &table(vec![group(0.0, vec![row(0.0), row(20.0), row(0.0)])]),
            800.0,
            600.0,
        )
        .unwrap();
        let group_border = compute(
            &table(vec![
                group(0.0, vec![row(0.0)]),
                group(20.0, vec![row(0.0)]),
                group(0.0, vec![row(0.0)]),
            ]),
            800.0,
            600.0,
        )
        .unwrap();

        let row_table = row_border.iter().find(|(_, index)| *index == 0).unwrap().0;
        let group_table = group_border
            .iter()
            .find(|(_, index)| *index == 0)
            .unwrap()
            .0;
        assert_eq!(row_table, group_table);
    }

    #[test]
    fn collapsed_table_ignores_authored_padding() {
        let cell = Component::boxed(
            Style {
                display: WDisp::TableCell,
                border_collapse: true,
                width: WDim::Px(100.0),
                padding: w3cos_std::style::Edges::all(10.0),
                border_width: 50.0,
                ..Style::default()
            },
            Vec::new(),
        );
        let row = Component::row(
            Style {
                display: WDisp::TableRow,
                border_collapse: true,
                ..Style::default()
            },
            vec![cell],
        );
        let table = Component::boxed(
            Style {
                display: WDisp::Table,
                border_collapse: true,
                padding: w3cos_std::style::Edges::all(10.0),
                ..Style::default()
            },
            vec![row],
        );

        let layout = compute(&table, 800.0, 600.0).unwrap();
        let table_rect = layout.iter().find(|(_, index)| *index == 0).unwrap().0;
        let row_rect = layout.iter().find(|(_, index)| *index == 1).unwrap().0;

        assert_eq!(table_rect.x, row_rect.x);
        assert_eq!(table_rect.width, row_rect.width);
    }

    #[test]
    fn collapsed_table_adds_outer_half_border_to_specified_row_height() {
        let cell = Component::boxed(
            Style {
                display: WDisp::TableCell,
                border_collapse: true,
                border_top_width: Some(96.0),
                ..Style::default()
            },
            Vec::new(),
        );
        let row = Component::row(
            Style {
                display: WDisp::TableRow,
                border_collapse: true,
                height: WDim::Px(96.0),
                ..Style::default()
            },
            vec![cell],
        );
        let table = Component::boxed(
            Style {
                display: WDisp::Table,
                border_collapse: true,
                ..Style::default()
            },
            vec![row],
        );

        let layout = compute(&table, 800.0, 600.0).unwrap();
        let table_rect = layout.iter().find(|(_, index)| *index == 0).unwrap().0;

        assert_eq!(table_rect.height, 144.0);
    }

    #[test]
    fn anonymous_table_wrapper_collapses_section_boundaries() {
        let group = || {
            Component::boxed(
                Style {
                    display: WDisp::TableRowGroup,
                    border_collapse: true,
                    ..Style::default()
                },
                vec![Component::row(
                    Style {
                        display: WDisp::TableRow,
                        border_collapse: true,
                        ..Style::default()
                    },
                    vec![Component::boxed(
                        Style {
                            display: WDisp::TableCell,
                            border_collapse: true,
                            height: WDim::Px(20.0),
                            border_width: 8.0,
                            ..Style::default()
                        },
                        Vec::new(),
                    )],
                )],
            )
        };
        let wrapper = Component::boxed(
            Style {
                display: WDisp::Block,
                border_collapse: true,
                ..Style::default()
            },
            vec![group(), group(), group()],
        );

        let layout = compute(&wrapper, 800.0, 600.0).unwrap();
        let wrapper_rect = layout.iter().find(|(_, index)| *index == 0).unwrap().0;
        let group_rects = [1, 4, 7].map(|index| {
            layout
                .iter()
                .find(|(_, candidate)| *candidate == index)
                .unwrap()
                .0
        });

        assert_eq!(
            wrapper_rect.height,
            group_rects.iter().map(|rect| rect.height).sum::<f32>() - 16.0
        );
    }

    #[test]
    fn collapsed_single_column_centers_unequal_borders_on_grid_lines() {
        let cell = |left: f32, right: f32, content_width: Option<f32>| {
            Component::boxed(
                Style {
                    display: WDisp::TableCell,
                    border_collapse: true,
                    flex_direction: WDir::Row,
                    align_items: WAlign::Baseline,
                    border_left_width: Some(left),
                    border_right_width: Some(right),
                    ..Style::default()
                },
                vec![Component::boxed(
                    Style {
                        display: WDisp::Block,
                        width: content_width.map_or(WDim::Auto, WDim::Px),
                        height: WDim::Px(25.0),
                        ..Style::default()
                    },
                    vec![],
                )],
            )
        };
        let row = |cell| {
            Component::row(
                Style {
                    display: WDisp::TableRow,
                    border_collapse: true,
                    ..Style::default()
                },
                vec![cell],
            )
        };
        let table = Component::boxed(
            Style {
                display: WDisp::Table,
                border_collapse: true,
                ..Style::default()
            },
            vec![Component::boxed(
                Style {
                    display: WDisp::TableRowGroup,
                    border_collapse: true,
                    ..Style::default()
                },
                vec![row(cell(150.0, 0.0, None)), row(cell(0.0, 100.0, None))],
            )],
        );

        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                width: WDim::Px(800.0),
                ..Style::default()
            },
            vec![table],
        );
        let layout = compute(&root, 800.0, 600.0).unwrap();
        let rect = |index| layout.iter().find(|(_, i)| *i == index).unwrap().0;
        assert_eq!(
            (rect(1).width, rect(4).x, rect(4).width),
            (200.0, 0.0, 150.0)
        );
        assert_eq!((rect(7).x, rect(7).width), (75.0, 125.0));
        assert_eq!((rect(8).x, rect(8).width), (75.0, 25.0));
    }

    #[test]
    fn auto_table_row_group_uses_all_cell_border_tracks() {
        let cell = |top: f32, right: f32, bottom: f32, left: f32| {
            Component::boxed(
                Style {
                    display: WDisp::TableCell,
                    border_top_width: Some(top),
                    border_right_width: Some(right),
                    border_bottom_width: Some(bottom),
                    border_left_width: Some(left),
                    ..Style::default()
                },
                vec![],
            )
        };
        let group = Component::boxed(
            Style {
                display: WDisp::TableRowGroup,
                ..Style::default()
            },
            vec![
                Component::row(
                    Style {
                        display: WDisp::TableRow,
                        ..Style::default()
                    },
                    vec![cell(60.0, 0.0, 0.0, 0.0), cell(0.0, 60.0, 0.0, 0.0)],
                ),
                Component::row(
                    Style {
                        display: WDisp::TableRow,
                        ..Style::default()
                    },
                    vec![cell(0.0, 0.0, 60.0, 60.0), cell(0.0, 0.0, 60.0, 0.0)],
                ),
            ],
        );
        let table = Component::boxed(
            Style {
                display: WDisp::Table,
                ..Style::default()
            },
            vec![group],
        );

        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                width: WDim::Px(800.0),
                ..Style::default()
            },
            vec![table],
        );
        let layout = compute(&root, 800.0, 600.0).unwrap();
        let table = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        let group = layout.iter().find(|(_, index)| *index == 2).unwrap().0;
        let bottom_right = layout.iter().find(|(_, index)| *index == 8).unwrap().0;

        assert_eq!((table.width, table.height), (120.0, 120.0));
        assert_eq!(group, table);
        assert_eq!((bottom_right.x, bottom_right.y), (60.0, 60.0));
    }

    #[test]
    fn table_track_width_does_not_change_cell_height_box_sizing() {
        let cell = Component::boxed(
            Style {
                display: WDisp::TableCell,
                width: WDim::Px(60.0),
                height: WDim::Px(60.0),
                border_bottom_width: Some(60.0),
                ..Style::default()
            },
            vec![],
        );
        let row = Component::row(
            Style {
                display: WDisp::TableRow,
                ..Style::default()
            },
            vec![cell],
        );
        let table = Component::boxed(
            Style {
                display: WDisp::Table,
                ..Style::default()
            },
            vec![row],
        );
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                width: WDim::Px(800.0),
                ..Style::default()
            },
            vec![table],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let cell = layout.iter().find(|(_, index)| *index == 3).unwrap().0;
        assert_eq!((cell.width, cell.height), (60.0, 120.0));
    }

    #[test]
    fn table_column_group_paints_over_rows_without_consuming_flow_height() {
        let column_group = Component::row(
            Style {
                display: WDisp::TableColumnGroup,
                ..Style::default()
            },
            vec![
                Component::boxed(
                    Style {
                        display: WDisp::TableColumn,
                        ..Style::default()
                    },
                    vec![],
                ),
                Component::boxed(
                    Style {
                        display: WDisp::TableColumn,
                        ..Style::default()
                    },
                    vec![],
                ),
            ],
        );
        let row = || {
            Component::row(
                Style {
                    display: WDisp::TableRow,
                    ..Style::default()
                },
                vec![Component::boxed(
                    Style {
                        display: WDisp::TableCell,
                        height: WDim::Px(48.0),
                        ..Style::default()
                    },
                    vec![],
                )],
            )
        };
        let table = Component::boxed(
            Style {
                display: WDisp::Table,
                width: WDim::Px(96.0),
                ..Style::default()
            },
            vec![column_group, row(), row()],
        );

        let layout = compute(&table, 800.0, 600.0).unwrap();
        let table = layout.iter().find(|(_, index)| *index == 0).unwrap().0;
        let column_group = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        let first_row = layout.iter().find(|(_, index)| *index == 4).unwrap().0;
        assert_eq!((table.width, table.height), (96.0, 96.0));
        assert_eq!(column_group, table);
        assert_eq!(first_row.y, table.y);
    }

    #[test]
    fn table_row_distributes_auto_cells_across_definite_width() {
        let cell = |label| {
            Component::text(
                label,
                Style {
                    display: WDisp::TableCell,
                    height: WDim::Px(48.0),
                    ..Style::default()
                },
            )
        };
        let table = Component::boxed(
            Style {
                display: WDisp::Table,
                width: WDim::Px(192.0),
                ..Style::default()
            },
            vec![Component::row(
                Style {
                    display: WDisp::TableRow,
                    ..Style::default()
                },
                vec![cell("a"), cell("b")],
            )],
        );

        let layout = compute(&table, 800.0, 600.0).unwrap();
        let first = layout.iter().find(|(_, index)| *index == 2).unwrap().0;
        let second = layout.iter().find(|(_, index)| *index == 3).unwrap().0;
        assert_eq!(first.width, 96.0);
        assert_eq!(second.width, 96.0);
        assert_eq!(second.x, first.x + first.width);
    }

    #[test]
    fn inline_block_stacks_direct_block_children() {
        let child = |label| {
            Component::text(
                label,
                Style {
                    display: WDisp::Block,
                    height: WDim::Px(48.0),
                    ..Style::default()
                },
            )
        };
        let inline_block = Component::row(
            Style {
                display: WDisp::InlineBlock,
                width: WDim::Px(96.0),
                ..Style::default()
            },
            vec![child("a"), child("b")],
        );

        let layout = compute(&inline_block, 800.0, 600.0).unwrap();
        let parent = layout.iter().find(|(_, index)| *index == 0).unwrap().0;
        let first = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        let second = layout.iter().find(|(_, index)| *index == 2).unwrap().0;
        assert_eq!((parent.width, parent.height), (96.0, 96.0));
        assert_eq!(second.y, first.y + first.height);
    }

    #[test]
    fn auto_width_inline_block_uses_the_widest_stacked_block_child() {
        let child = || {
            Component::boxed(
                Style {
                    display: WDisp::Block,
                    width: WDim::Px(96.0),
                    ..Style::default()
                },
                vec![],
            )
        };
        let inline_block = Component::row(
            Style {
                display: WDisp::InlineBlock,
                ..Style::default()
            },
            vec![child(), child()],
        );

        assert_eq!(shrink_to_fit_used_width(&inline_block), 96.0);
    }

    #[test]
    fn auto_width_inline_block_shrink_fits_to_available_width() {
        let inline_block = Component::row(
            Style {
                display: WDisp::InlineBlock,
                ..Style::default()
            },
            vec![Component::text(
                "several short words that exceed the available width",
                Style {
                    display: WDisp::Inline,
                    ..Style::default()
                },
            )],
        );

        assert_eq!(
            shrink_to_fit_used_width_with_available(&inline_block, 160.0),
            160.0
        );
    }

    #[test]
    fn auto_width_inline_block_preserves_an_overwide_min_content_child() {
        let inline_block = Component::row(
            Style {
                display: WDisp::InlineBlock,
                ..Style::default()
            },
            vec![Component::boxed(
                Style {
                    display: WDisp::Block,
                    width: WDim::Px(320.0),
                    ..Style::default()
                },
                vec![],
            )],
        );

        assert_eq!(
            shrink_to_fit_used_width_with_available(&inline_block, 160.0),
            320.0
        );
    }

    #[test]
    fn auto_width_inline_block_wraps_inline_text_to_its_used_width() {
        let inline_block = Component::row(
            Style {
                display: WDisp::InlineBlock,
                ..Style::default()
            },
            vec![Component::text(
                "several short words that exceed the available width",
                Style {
                    display: WDisp::Inline,
                    ..Style::default()
                },
            )],
        );
        let root = Component::row(
            Style {
                display: WDisp::Block,
                width: WDim::Px(160.0),
                ..Style::default()
            },
            vec![inline_block],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let inline_block = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        let text = layout.iter().find(|(_, index)| *index == 2).unwrap().0;
        assert_eq!(inline_block.width, 160.0);
        assert_eq!(text.width, inline_block.width);
        assert!(text.height > 19.2);
    }

    #[test]
    fn inline_block_auto_inline_margins_resolve_to_zero() {
        let child = Component::text(
            "X",
            Style {
                display: WDisp::InlineBlock,
                width: WDim::Px(100.0),
                margin: w3cos_std::style::Edges {
                    left: WSpacing::Auto,
                    right: WSpacing::Auto,
                    ..w3cos_std::style::Edges::ZERO
                },
                ..Style::default()
            },
        );
        let parent = Component::row(
            Style {
                display: WDisp::Block,
                width: WDim::Px(200.0),
                ..Style::default()
            },
            vec![child],
        );

        let layout = compute(&parent, 800.0, 600.0).unwrap();
        let parent_rect = layout.iter().find(|(_, index)| *index == 0).unwrap().0;
        let child_rect = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        assert_eq!(child_rect.x, parent_rect.x);
    }

    #[test]
    fn inline_block_max_width_constrains_parent_shrink_to_fit_width() {
        use w3cos_dom::{Document, stylesheet};

        stylesheet::clear_rules();
        stylesheet::register_rule(
            "div",
            &[
                ("display", "inline-block"),
                ("font", "30px/4 Ahem"),
                ("width", "auto"),
            ],
        );
        stylesheet::register_rule(
            "span",
            &[("display", "inline-block"), ("max-width", "4em")],
        );

        let mut document = Document::new();
        let parent = document.create_element("div");
        let constrained_child = document.create_element("span");
        let text = document.create_text_node("12345678");
        constrained_child.append_child(&mut document, text);
        parent.append_child(&mut document, constrained_child);
        document.body().append_child(&mut document, parent);

        let component = document.to_component_tree();
        let parent = &component.children[0];
        assert_eq!(parent.children[0].style.max_width, WDim::Em(4.0));
        assert_eq!(shrink_to_fit_used_width(parent), 120.0);
        let layout = compute(&component, 800.0, 600.0).unwrap();
        let rect = |index| layout.iter().find(|(_, item)| *item == index).unwrap().0;
        assert_eq!(rect(1).width, 120.0, "layout={layout:#?}");
        assert_eq!(rect(2).width, 120.0, "layout={layout:#?}");
        stylesheet::clear_rules();
    }

    #[test]
    fn inline_block_aligns_its_last_line_to_sibling_text() {
        let text = |content, display, visibility| {
            Component::text(
                content,
                Style {
                    display,
                    visibility,
                    ..Style::default()
                },
            )
        };
        let inline_block = Component::row(
            Style {
                display: WDisp::InlineBlock,
                ..Style::default()
            },
            vec![
                text("x", WDisp::Block, WVisibility::Hidden),
                text("bcd", WDisp::Inline, WVisibility::Visible),
            ],
        );
        let root = Component::row(
            Style {
                display: WDisp::Block,
                ..Style::default()
            },
            vec![
                text("a", WDisp::Inline, WVisibility::Visible),
                inline_block,
                text("e", WDisp::Inline, WVisibility::Visible),
            ],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let rect = |index| layout.iter().find(|(_, item)| *item == index).unwrap().0;
        assert_eq!(rect(4).y, rect(1).y);
        assert_eq!(rect(5).y, rect(1).y);
    }

    #[test]
    fn decorated_inline_block_moves_sibling_text_to_its_last_line() {
        let text = |content, display, visibility| {
            Component::text(
                content,
                Style {
                    display,
                    visibility,
                    ..Style::default()
                },
            )
        };
        let inline_block = Component::row(
            Style {
                display: WDisp::InlineBlock,
                margin: w3cos_std::style::Edges::xy(0.0, 3.0),
                padding: w3cos_std::style::Edges::xy(0.0, 9.0),
                border_top_width: Some(4.0),
                border_bottom_width: Some(4.0),
                ..Style::default()
            },
            vec![
                text("x", WDisp::Block, WVisibility::Hidden),
                text("bcd", WDisp::Inline, WVisibility::Visible),
            ],
        );
        let root = Component::row(
            Style {
                display: WDisp::Block,
                ..Style::default()
            },
            vec![
                text("a", WDisp::Inline, WVisibility::Visible),
                inline_block,
                text("e", WDisp::Inline, WVisibility::Visible),
            ],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let rect = |index| layout.iter().find(|(_, item)| *item == index).unwrap().0;
        assert_eq!(rect(1).y, rect(4).y);
        assert_eq!(rect(5).y, rect(4).y);
    }

    #[test]
    fn inline_table_stacks_table_rows() {
        let row = || {
            Component::row(
                Style {
                    display: WDisp::TableRow,
                    height: WDim::Px(48.0),
                    ..Style::default()
                },
                vec![],
            )
        };
        let inline_table = Component::row(
            Style {
                display: WDisp::InlineTable,
                width: WDim::Px(96.0),
                ..Style::default()
            },
            vec![row(), row()],
        );

        let layout = compute(&inline_table, 800.0, 600.0).unwrap();
        let table = layout.iter().find(|(_, index)| *index == 0).unwrap().0;
        let first = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        let second = layout.iter().find(|(_, index)| *index == 2).unwrap().0;
        assert_eq!((table.width, table.height), (96.0, 96.0));
        assert_eq!(second.y, first.y + first.height);
    }

    #[test]
    fn auto_table_wrapper_contains_its_anonymous_line_child() {
        let table = Component::row(
            Style {
                display: WDisp::Table,
                ..Style::default()
            },
            vec![Component::row(
                Style {
                    display: WDisp::Flex,
                    ..Style::default()
                },
                Vec::new(),
            )],
        );
        let flat = pre_flatten(&table);
        let mut layout = vec![
            (
                LayoutRect {
                    x: 8.0,
                    y: 8.0,
                    width: 160.0,
                    height: 19.2,
                },
                0,
            ),
            (
                LayoutRect {
                    x: 8.0,
                    y: 8.0,
                    width: 160.0,
                    height: 40.0,
                },
                1,
            ),
        ];

        project_auto_table_child_heights(&mut layout, &flat);

        assert_eq!(layout[0].0.height, 40.0);
    }

    #[test]
    fn inline_table_uses_its_first_cell_text_as_the_sibling_baseline() {
        let inline = |content| {
            Component::text(
                content,
                Style {
                    display: WDisp::Inline,
                    ..Style::default()
                },
            )
        };
        let table = Component::row(
            Style {
                display: WDisp::InlineTable,
                ..Style::default()
            },
            vec![Component::text(
                "bcd",
                Style {
                    display: WDisp::TableCell,
                    padding: w3cos_std::style::Edges::xy(0.0, 9.0),
                    border_top_width: Some(4.0),
                    ..Style::default()
                },
            )],
        );
        let root = Component::row(
            Style {
                display: WDisp::Block,
                ..Style::default()
            },
            vec![inline("a"), table, inline("e")],
        );
        let flat = pre_flatten(&root);
        let rect = |y, height| LayoutRect {
            x: 0.0,
            y,
            width: 0.0,
            height,
        };
        let mut layout = vec![
            (rect(0.0, 0.0), 0),
            (rect(28.6, 0.0), 1),
            (rect(30.0, 0.0), 2),
            (rect(48.0, 45.2), 3),
            (rect(28.6, 0.0), 4),
        ];

        align_inline_table_first_row_baselines(&mut layout, &flat);

        assert!((layout[1].0.y - 62.6).abs() < 0.01);
        assert!((layout[4].0.y - 62.6).abs() < 0.01);
    }

    #[test]
    fn inline_table_with_direct_inline_content_uses_the_full_row_width() {
        let inline_table = Component::row(
            Style {
                display: WDisp::InlineTable,
                ..Style::default()
            },
            vec![
                Component::text(
                    "1",
                    Style {
                        display: WDisp::Inline,
                        ..Style::default()
                    },
                ),
                Component::text(
                    "Before inline-table",
                    Style {
                        display: WDisp::Inline,
                        ..Style::default()
                    },
                ),
            ],
        );
        let expected = inline_table
            .children
            .iter()
            .map(component_max_content_width)
            .sum::<f32>();

        assert!(
            (shrink_to_fit_used_width(&inline_table) - expected).abs() < 0.01,
            "direct generated inline-table content must aggregate as one row"
        );
    }

    #[test]
    fn inline_table_block_child_starts_a_new_internal_row() {
        let inline_table = Component::row(
            Style {
                display: WDisp::InlineTable,
                ..Style::default()
            },
            vec![
                Component::text(
                    "bcd",
                    Style {
                        display: WDisp::Inline,
                        ..Style::default()
                    },
                ),
                Component::text(
                    "x",
                    Style {
                        display: WDisp::Block,
                        ..Style::default()
                    },
                ),
            ],
        );
        let expected = inline_table
            .children
            .iter()
            .map(component_max_content_width)
            .fold(0.0_f32, f32::max);

        assert!(
            (shrink_to_fit_used_width(&inline_table) - expected).abs() < 0.01,
            "block descendants must form separate inline-table rows"
        );

        let layout = compute(&inline_table, 800.0, 600.0).unwrap();
        let first = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        let second = layout.iter().find(|(_, index)| *index == 2).unwrap().0;
        assert!(
            second.y > first.y,
            "block descendants must stack below preceding inline content: first={first:?}, second={second:?}"
        );
    }

    #[test]
    fn auto_width_css_table_caps_max_content_at_its_containing_block() {
        let table = Component::boxed(
            Style {
                display: WDisp::Table,
                ..Style::default()
            },
            vec![Component::row(
                Style {
                    display: WDisp::TableRow,
                    ..Style::default()
                },
                vec![Component::text(
                    "A table cell whose max-content width is wider than its containing block",
                    Style {
                        display: WDisp::TableCell,
                        ..Style::default()
                    },
                )],
            )],
        );
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                width: WDim::Px(120.0),
                ..Style::default()
            },
            vec![table],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let table = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        assert!(
            table.width <= 120.01,
            "auto table must not exceed its containing block: {table:?}"
        );
    }

    #[test]
    fn table_cell_margins_do_not_participate_in_row_layout() {
        let plain_cell = Component::text(
            "left",
            Style {
                display: WDisp::TableCell,
                ..Style::default()
            },
        );
        let margined_cell = Component::text(
            "left",
            Style {
                display: WDisp::TableCell,
                margin: w3cos_std::style::Edges::all(5.0),
                ..Style::default()
            },
        );
        assert_eq!(
            component_max_content_width(&plain_cell),
            component_max_content_width(&margined_cell),
            "table-cell margins must not affect max-content column sizing"
        );

        let cell = |label: &str| {
            Component::text(
                label,
                Style {
                    display: WDisp::TableCell,
                    margin: w3cos_std::style::Edges::all(5.0),
                    ..Style::default()
                },
            )
        };
        let row = Component::row(
            Style {
                display: WDisp::TableRow,
                width: WDim::Px(200.0),
                ..Style::default()
            },
            vec![cell("left"), cell("right")],
        );

        let layout = compute(&row, 200.0, 100.0).unwrap();
        let first = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        let second = layout.iter().find(|(_, index)| *index == 2).unwrap().0;
        assert_eq!(first.x, 0.0);
        assert!(
            (second.x - first.x - first.width).abs() < 0.01,
            "table-cell margins must not create a gap: first={first:?}, second={second:?}"
        );
    }

    #[test]
    fn shrink_to_fit_width_excludes_inline_margins() {
        let plain = Component::boxed(
            Style {
                display: WDisp::Inline,
                ..Style::default()
            },
            vec![Component::text("inline", Style::default())],
        );
        let margined = Component::boxed(
            Style {
                display: WDisp::Inline,
                margin: w3cos_std::style::Edges::all(10.0),
                ..Style::default()
            },
            vec![Component::text("inline", Style::default())],
        );
        assert_eq!(
            shrink_to_fit_used_width(&plain),
            shrink_to_fit_used_width(&margined),
            "margins sit outside an auto shrink-to-fit box"
        );
    }

    #[test]
    fn block_max_content_uses_widest_stacked_block_child() {
        let block_child = |content| {
            Component::boxed(
                Style {
                    display: WDisp::Flex,
                    ..Style::default()
                },
                vec![Component::text(content, Style::default())],
            )
        };
        let first = block_child("first row");
        let second = block_child("a longer second row");
        let expected =
            component_max_content_width(&first).max(component_max_content_width(&second));
        let block = Component::boxed(
            Style {
                display: WDisp::Block,
                ..Style::default()
            },
            vec![first, second],
        );

        assert_eq!(component_max_content_width(&block), expected);

        let before = Component::text("short", Style::default());
        let boundary = block_child("the widest block row");
        let after = Component::text("tail", Style::default());
        let expected = component_max_content_width(&before)
            .max(component_max_content_width(&boundary))
            .max(component_max_content_width(&after));
        let mixed = Component::boxed(
            Style {
                display: WDisp::Block,
                ..Style::default()
            },
            vec![before, boundary, after],
        );
        assert_eq!(
            component_max_content_width(&mixed),
            expected,
            "a block boundary splits the anonymous inline rows on both sides"
        );
    }

    #[test]
    fn table_cell_max_content_uses_widest_stacked_block_child() {
        let block_child = |width| {
            Component::boxed(
                Style {
                    display: WDisp::Block,
                    width: WDim::Px(width),
                    ..Style::default()
                },
                Vec::new(),
            )
        };
        let cell = Component::boxed(
            Style {
                display: WDisp::TableCell,
                ..Style::default()
            },
            vec![block_child(50.0), block_child(100.0), block_child(75.0)],
        );

        assert_eq!(component_max_content_width(&cell), 100.0);
    }

    #[test]
    fn table_cell_collapses_adjacent_block_margins() {
        let child = || {
            Component::boxed(
                Style {
                    display: WDisp::Block,
                    width: WDim::Px(50.0),
                    height: WDim::Px(50.0),
                    margin: w3cos_std::style::Edges {
                        top: WSpacing::Px(50.0),
                        right: WSpacing::Px(0.0),
                        bottom: WSpacing::Px(50.0),
                        left: WSpacing::Px(0.0),
                    },
                    ..Style::default()
                },
                Vec::new(),
            )
        };
        let cell = Component::boxed(
            Style {
                display: WDisp::TableCell,
                ..Style::default()
            },
            vec![child(), child()],
        );

        let layout = compute(&cell, 800.0, 600.0).unwrap();
        let cell = layout.iter().find(|(_, index)| *index == 0).unwrap().0;
        let first = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        let second = layout.iter().find(|(_, index)| *index == 2).unwrap().0;
        assert_eq!((cell.height, first.y, second.y), (250.0, 50.0, 150.0));
    }

    #[test]
    fn block_boundary_splits_adjacent_anonymous_inline_lines() {
        let sized_box = |display, width, height| {
            Component::boxed(
                Style {
                    display,
                    width,
                    height: WDim::Px(height),
                    ..Style::default()
                },
                Vec::new(),
            )
        };
        let component = Component::row(
            Style {
                display: WDisp::Block,
                width: WDim::Px(200.0),
                ..Style::default()
            },
            vec![
                sized_box(WDisp::InlineBlock, WDim::Px(60.0), 50.0),
                sized_box(WDisp::Block, WDim::Auto, 100.0),
                sized_box(WDisp::InlineBlock, WDim::Px(60.0), 50.0),
            ],
        );

        let layout = compute(&component, 800.0, 600.0).unwrap();
        let first = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        let boundary = layout.iter().find(|(_, index)| *index == 2).unwrap().0;
        let last = layout.iter().find(|(_, index)| *index == 3).unwrap().0;
        assert_eq!(
            (first.x, first.y, first.width, first.height),
            (0.0, 0.0, 60.0, 50.0)
        );
        assert_eq!(
            (boundary.x, boundary.y, boundary.width, boundary.height),
            (0.0, 50.0, 200.0, 100.0)
        );
        assert_eq!(
            (last.x, last.y, last.width, last.height),
            (0.0, 150.0, 60.0, 50.0)
        );
    }

    #[test]
    fn table_row_preserves_rtl_column_axis_in_taffy() {
        let style = to_taffy_style(
            &Style {
                display: WDisp::TableRow,
                flex_direction: WDir::RowReverse,
                ..Style::default()
            },
            800.0,
            600.0,
        );

        assert_eq!(style.flex_direction, FlexDirection::RowReverse);
    }

    #[test]
    fn fixed_rtl_table_projects_first_cell_to_rightmost_track() {
        let cell = || {
            Component::boxed(
                Style {
                    display: WDisp::TableCell,
                    height: WDim::Px(50.0),
                    ..Style::default()
                },
                vec![],
            )
        };
        let table = Component::boxed(
            Style {
                display: WDisp::Table,
                direction: w3cos_std::style::TextDirection::Rtl,
                table_layout_fixed: true,
                width: WDim::Px(100.0),
                ..Style::default()
            },
            vec![Component::row(
                Style {
                    display: WDisp::TableRow,
                    flex_direction: WDir::RowReverse,
                    ..Style::default()
                },
                vec![cell(), cell()],
            )],
        );

        let layout = compute(&table, 800.0, 600.0).unwrap();
        let first = layout.iter().find(|(_, index)| *index == 2).unwrap().0;
        let second = layout.iter().find(|(_, index)| *index == 3).unwrap().0;
        assert_eq!((first.x, first.width), (50.0, 50.0));
        assert_eq!((second.x, second.width), (0.0, 50.0));
    }

    #[test]
    fn shrink_to_fit_width_excludes_table_and_float_margins() {
        let child = || Component::text("inline", Style::default());
        let shrink_width = |display, float| {
            shrink_to_fit_used_width(&Component::boxed(
                Style {
                    display,
                    float,
                    margin: w3cos_std::style::Edges::all(10.0),
                    ..Style::default()
                },
                vec![child()],
            ))
        };

        assert_eq!(
            shrink_width(WDisp::Table, WFloat::None),
            shrink_width(WDisp::Table, WFloat::Left),
            "table and float margins both stay outside the shrink-to-fit border box"
        );
        assert_eq!(
            shrink_width(WDisp::Block, WFloat::Left),
            shrink_width(WDisp::Inline, WFloat::None),
            "a blockified float uses the same intrinsic border-box width as inline content"
        );
    }

    #[test]
    fn auto_width_float_shrink_wraps_in_a_block_container() {
        let floating = Component::boxed(
            Style {
                display: WDisp::Block,
                float: WFloat::Left,
                margin: w3cos_std::style::Edges::all(10.0),
                padding: w3cos_std::style::Edges::all(12.0),
                border_width: 3.0,
                ..Style::default()
            },
            vec![Component::text("float", Style::default())],
        );
        let expected_width = shrink_to_fit_used_width(&floating);
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                width: WDim::Px(800.0),
                ..Style::default()
            },
            vec![floating],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let floating = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        assert!(
            (floating.width - expected_width).abs() < 0.01,
            "an auto-width float must use its intrinsic border box exactly: expected={expected_width}, actual={floating:?}"
        );
    }

    #[test]
    fn right_float_aligns_to_the_containing_block_end() {
        let floating = Component::boxed(
            Style {
                display: WDisp::Block,
                float: WFloat::Right,
                width: WDim::Px(50.0),
                height: WDim::Px(20.0),
                ..Style::default()
            },
            Vec::new(),
        );
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                width: WDim::Px(100.0),
                ..Style::default()
            },
            vec![floating],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let root_rect = layout.iter().find(|(_, index)| *index == 0).unwrap().0;
        let float_rect = layout.iter().find(|(_, index)| *index == 1).unwrap().0;

        assert_eq!(float_rect.width, 50.0);
        assert_eq!(float_rect.x, root_rect.x + 50.0);
    }

    #[test]
    fn definite_width_left_float_does_not_stretch() {
        let floating = Component::boxed(
            Style {
                display: WDisp::Block,
                float: WFloat::Left,
                width: WDim::Px(200.0),
                height: WDim::Px(200.0),
                ..Style::default()
            },
            Vec::new(),
        );
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                width: WDim::Px(784.0),
                ..Style::default()
            },
            vec![floating],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let float_rect = layout.iter().find(|(_, index)| *index == 1).unwrap().0;

        assert_eq!(float_rect.width, 200.0);
    }

    #[test]
    fn oversized_inline_replaced_box_wraps_below_a_left_float() {
        let floating = Component::image(
            "float.png",
            Style {
                display: WDisp::InlineBlock,
                float: WFloat::Left,
                width: WDim::Px(100.0),
                height: WDim::Px(100.0),
                ..Style::default()
            },
        );
        let whitespace = Component::text(
            " ",
            Style {
                display: WDisp::Inline,
                ..Style::default()
            },
        );
        let flow = Component::image(
            "flow.png",
            Style {
                display: WDisp::InlineBlock,
                width: WDim::Percent(100.0),
                height: WDim::Px(100.0),
                ..Style::default()
            },
        );
        let root = Component::row(
            Style {
                display: WDisp::Flex,
                width: WDim::Px(600.0),
                height: WDim::Px(100.0),
                flex_wrap: WWrap::Wrap,
                ..Style::default()
            },
            vec![floating, whitespace, flow],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let floating = layout.iter().find(|(_, index)| *index == 1).unwrap().0;
        let flow = layout.iter().find(|(_, index)| *index == 3).unwrap().0;

        assert!(
            flow.y >= floating.y + floating.height - 0.01,
            "an inline replaced box wider than the float-side band must wrap below the float: float={floating:?}, flow={flow:?}"
        );
    }

    #[test]
    fn left_float_after_inline_starts_below_the_established_line_box() {
        let image = Component::image(
            "one-pixel.png",
            Style {
                display: WDisp::InlineBlock,
                width: WDim::Px(1.0),
                height: WDim::Px(1.0),
                ..Style::default()
            },
        );
        let mut float_style = Style {
            display: WDisp::Block,
            float: WFloat::Left,
            height: WDim::Px(64.0),
            font_size: 64.0,
            line_height: 1.0,
            ..Style::default()
        };
        float_style
            .custom_properties
            .get_or_insert_with(Default::default)
            .insert(
                "--w3cos-internal-left-float-after-inline".to_string(),
                "1".to_string(),
            );
        let root = Component::row(
            Style {
                display: WDisp::Block,
                width: WDim::Px(0.0),
                font_size: 64.0,
                line_height: 1.0,
                ..Style::default()
            },
            vec![
                image,
                Component::row(
                    float_style,
                    vec![Component::text(
                        "XXXX",
                        Style {
                            display: WDisp::Inline,
                            font_size: 64.0,
                            line_height: 1.0,
                            ..Style::default()
                        },
                    )],
                ),
            ],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let root = layout.iter().find(|(_, index)| *index == 0).unwrap().0;
        let floating = layout.iter().find(|(_, index)| *index == 2).unwrap().0;
        assert!(floating.y >= root.y + 64.0 - 0.01);
        assert!(floating.width > 1.0);
    }

    #[test]
    fn non_replaced_inline_ignores_vertical_margins_in_line_layout() {
        let style = to_taffy_style(
            &Style {
                display: WDisp::Inline,
                margin: w3cos_std::style::Edges::all(16.0),
                ..Style::default()
            },
            800.0,
            600.0,
        );

        assert_eq!(style.margin.top, LengthPercentageAuto::length(0.0));
        assert_eq!(style.margin.bottom, LengthPercentageAuto::length(0.0));
        assert_eq!(style.margin.left, LengthPercentageAuto::length(16.0));
        assert_eq!(style.margin.right, LengthPercentageAuto::length(16.0));
    }

    #[test]
    fn non_replaced_inline_paints_vertical_edges_without_enlarging_the_line_box() {
        let inline = Component::text(
            "inline",
            Style {
                display: WDisp::Inline,
                font_size: 20.0,
                line_height: 1.0,
                padding: w3cos_std::style::Edges::xy(0.0, 5.0),
                border_width: 2.0,
                ..Style::default()
            },
        );
        let root = Component::boxed(
            Style {
                display: WDisp::Block,
                font_size: 20.0,
                line_height: 1.0,
                ..Style::default()
            },
            vec![inline],
        );

        let layout = compute(&root, 800.0, 600.0).unwrap();
        let root_rect = layout.iter().find(|(_, index)| *index == 0).unwrap().0;
        let inline_rect = layout.iter().find(|(_, index)| *index == 1).unwrap().0;

        assert_eq!(root_rect.height, 20.0);
        assert_eq!(inline_rect.height, 34.0);
    }

    #[test]
    fn generated_css_table_cells_cover_the_shrink_wrapped_table() {
        use w3cos_dom::{Document, stylesheet};

        stylesheet::clear_rules();
        stylesheet::register_rule(".table", &[("display", "table"), ("background", "red")]);
        stylesheet::register_rule(".row", &[("display", "table-row")]);
        stylesheet::register_rule(
            ".cell",
            &[
                ("display", "table-cell"),
                ("background", "white"),
                ("border", "solid white"),
            ],
        );
        stylesheet::register_rule(
            ".row.test::before",
            &[
                ("content", "'P'"),
                ("display", "table-cell"),
                ("background", "white"),
                ("border", "solid white"),
            ],
        );
        stylesheet::register_rule(
            ".row.test::after",
            &[
                ("content", "'S'"),
                ("display", "table-cell"),
                ("background", "white"),
                ("border", "solid white"),
            ],
        );

        let mut document = Document::new();
        let table = document.create_element("div");
        table.class_list_add(&mut document, "table");
        let first_row = document.create_element("div");
        first_row.class_list_add(&mut document, "row");
        for label in ["P", "A", "S", "S"] {
            let whitespace = document.create_text_node("\n  ");
            first_row.append_child(&mut document, whitespace);
            let cell = document.create_element("div");
            cell.class_list_add(&mut document, "cell");
            let text = document.create_text_node(label);
            cell.append_child(&mut document, text);
            first_row.append_child(&mut document, cell);
        }
        let whitespace = document.create_text_node("\n  ");
        first_row.append_child(&mut document, whitespace);
        let second_row = document.create_element("div");
        second_row.class_list_add(&mut document, "row");
        second_row.class_list_add(&mut document, "test");
        for label in ["A", "S"] {
            let whitespace = document.create_text_node("\n  ");
            second_row.append_child(&mut document, whitespace);
            let cell = document.create_element("div");
            cell.class_list_add(&mut document, "cell");
            let text = document.create_text_node(label);
            cell.append_child(&mut document, text);
            second_row.append_child(&mut document, cell);
        }
        let whitespace = document.create_text_node("\n  ");
        second_row.append_child(&mut document, whitespace);
        table.append_child(&mut document, first_row);
        table.append_child(&mut document, second_row);
        document.body().append_child(&mut document, table);

        let component = document.to_component_tree();
        let flat = pre_flatten(&component);
        let layout = compute(&component, 800.0, 600.0).unwrap();
        let table_index = flat
            .iter()
            .position(|node| node.style.background == Color::rgb(255, 0, 0))
            .expect("red table component");
        let table = layout
            .iter()
            .find_map(|(rect, index)| (*index == table_index).then_some(*rect))
            .expect("red table layout");
        let cell_right = layout
            .iter()
            .filter(|(_, index)| {
                flat[*index].style.display == WDisp::TableCell
                    && flat[*index].style.background == Color::WHITE
            })
            .map(|(rect, _)| rect.x + rect.width)
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(
            (table.x + table.width - cell_right).abs() < 0.01,
            "table background must end at the last cell edge: table={table:?}, cell_right={cell_right}, component={component:#?}"
        );
        stylesheet::clear_rules();
    }

    #[test]
    fn absolute_generated_children_skip_static_hosts_for_their_containing_block() {
        use w3cos_dom::{Document, stylesheet};

        fn document_body(document: &mut Document) -> w3cos_dom::Element {
            let html = document.create_element("html");
            let head = document.create_element("head");
            let body = document.create_element("body");
            html.append_child(document, head);
            html.append_child(document, body);
            document.body().append_child(document, html);
            document.set_render_body(body.id);
            body
        }

        fn absolute_bounds(component: &Component) -> LayoutRect {
            let absolute_indices = pre_flatten(component)
                .iter()
                .enumerate()
                .filter_map(|(index, node)| {
                    (node.style.position == WPos::Absolute).then_some(index)
                })
                .collect::<Vec<_>>();
            let layout = compute(component, 800.0, 600.0).unwrap();
            let rects = layout
                .iter()
                .filter_map(|(rect, index)| absolute_indices.contains(index).then_some(*rect))
                .collect::<Vec<_>>();
            LayoutRect {
                x: rects
                    .iter()
                    .map(|rect| rect.x)
                    .fold(f32::INFINITY, f32::min),
                y: rects
                    .iter()
                    .map(|rect| rect.y)
                    .fold(f32::INFINITY, f32::min),
                width: rects
                    .iter()
                    .map(|rect| rect.x + rect.width)
                    .fold(f32::NEG_INFINITY, f32::max)
                    - rects
                        .iter()
                        .map(|rect| rect.x)
                        .fold(f32::INFINITY, f32::min),
                height: rects
                    .iter()
                    .map(|rect| rect.y + rect.height)
                    .fold(f32::NEG_INFINITY, f32::max)
                    - rects
                        .iter()
                        .map(|rect| rect.y)
                        .fold(f32::INFINITY, f32::min),
            }
        }

        stylesheet::clear_rules();
        stylesheet::register_rule(
            "#test::before",
            &[
                ("content", "''"),
                ("position", "absolute"),
                ("right", "50px"),
                ("bottom", "0"),
                ("width", "50px"),
                ("height", "100px"),
                ("background", "blue"),
            ],
        );
        stylesheet::register_rule(
            "#test::after",
            &[
                ("content", "''"),
                ("position", "absolute"),
                ("right", "0"),
                ("bottom", "0"),
                ("width", "50px"),
                ("height", "100px"),
                ("background", "blue"),
            ],
        );
        let mut actual_document = Document::new();
        let actual_body = document_body(&mut actual_document);
        let paragraph = actual_document.create_element("p");
        let paragraph_text = actual_document.create_text_node("positioned");
        paragraph.append_child(&mut actual_document, paragraph_text);
        actual_body.append_child(&mut actual_document, paragraph);
        let host = actual_document.create_element("div");
        host.set_attribute(&mut actual_document, "id", "test");
        actual_body.append_child(&mut actual_document, host);
        let actual = actual_document.to_component_tree();

        stylesheet::clear_rules();
        stylesheet::register_rule(
            "#reference",
            &[
                ("position", "absolute"),
                ("right", "0"),
                ("bottom", "0"),
                ("width", "100px"),
                ("height", "100px"),
                ("background", "blue"),
            ],
        );
        let mut reference_document = Document::new();
        let reference_body = document_body(&mut reference_document);
        let paragraph = reference_document.create_element("p");
        let paragraph_text = reference_document.create_text_node("positioned");
        paragraph.append_child(&mut reference_document, paragraph_text);
        reference_body.append_child(&mut reference_document, paragraph);
        let reference_box = reference_document.create_element("div");
        reference_box.set_attribute(&mut reference_document, "id", "reference");
        reference_body.append_child(&mut reference_document, reference_box);
        let reference = reference_document.to_component_tree();

        let actual_bounds = absolute_bounds(&actual);
        let reference_bounds = absolute_bounds(&reference);
        assert_eq!(
            actual_bounds, reference_bounds,
            "static hosts must not become absolute containing blocks"
        );
        stylesheet::clear_rules();
    }

    #[test]
    fn clipped_nowrap_text_can_shrink_in_column() {
        let layout = compute(
            &Component::column(
                Style {
                    display: WDisp::Flex,
                    flex_direction: WDir::Column,
                    width: WDim::Px(96.0),
                    ..Style::default()
                },
                vec![Component::text(
                    "LogiDesk 对话标题",
                    Style {
                        overflow_x: Some(WOverflow::Hidden),
                        overflow_y: Some(WOverflow::Hidden),
                        white_space: WWhiteSpace::NoWrap,
                        ..Style::default()
                    },
                )],
            ),
            402.0,
            874.0,
        )
        .unwrap();
        let text_rect = layout.iter().find(|(_, idx)| *idx == 1).unwrap().0;
        assert!(
            (text_rect.width - 96.0).abs() < 1.0,
            "clipped nowrap text should shrink to its flex column, got {}",
            text_rect.width
        );
    }

    #[test]
    fn absolute_auto_height_row_contains_taller_card() {
        let host_tree_style = || Style {
            flex_shrink: 0.0,
            ..Style::default()
        };
        let text = |content: &str, font_size: f32| {
            Component::text(
                content,
                Style {
                    flex_shrink: 0.0,
                    font_size,
                    ..Style::default()
                },
            )
        };
        let header = Component::row(
            Style {
                flex_direction: WDir::Row,
                justify_content: WJustify::SpaceBetween,
                align_items: WAlign::Center,
                flex_shrink: 0.0,
                ..Style::default()
            },
            vec![
                text("待处理 · 会话 950", 11.0),
                text("每 25 条分布 1 项", 11.0),
            ],
        );
        let card = Component::column(
            Style {
                flex_direction: WDir::Column,
                flex_shrink: 0.0,
                min_height: WDim::Px(94.0),
                padding: w3cos_std::style::Edges::all(10.0),
                border_width: 1.0,
                gap: 6.0,
                ..Style::default()
            },
            vec![
                header,
                text("SH12345 上海 → 杭州 · 等待确认到达并补充 POD", 13.0),
                text("需上传签收凭证并确认异常责任方", 11.0),
            ],
        );
        let row = Component::boxed(
            Style {
                position: WPos::Absolute,
                top: WDim::Px(0.0),
                width: WDim::Percent(100.0),
                padding: w3cos_std::style::Edges::all(6.0),
                ..host_tree_style()
            },
            vec![card],
        );

        let layout = compute(&row, 393.0, 852.0).unwrap();
        let row_rect = layout.iter().find(|(_, idx)| *idx == 0).unwrap().0;
        let descendant_bottom = layout
            .iter()
            .filter(|(_, idx)| *idx != 0)
            .map(|(rect, _)| rect.y + rect.height)
            .fold(0.0f32, f32::max);

        assert!(
            row_rect.y + row_rect.height + 0.01 >= descendant_bottom + 6.0,
            "auto-height row {:?} does not contain descendants ending at {descendant_bottom}",
            row_rect
        );
    }

    #[test]
    fn explicit_text_height_is_preserved_after_wrap_pass() {
        let l = compute(
            &Component::text(
                "✦",
                Style {
                    width: WDim::Px(40.0),
                    height: WDim::Px(40.0),
                    ..Style::default()
                },
            ),
            402.0,
            874.0,
        )
        .unwrap();
        assert!((l[0].0.height - 40.0).abs() < 0.01);
    }

    #[test]
    fn breakable_text_does_not_force_a_message_grid_past_its_percent_max_width() {
        let avatar = Component::boxed(
            Style {
                width: WDim::Px(34.0),
                height: WDim::Px(34.0),
                ..Style::default()
            },
            vec![],
        );
        let message_text = Component::text(
            "app.error.individual_identity_verification_authority_invalid",
            Style {
                display: WDisplay::Block,
                padding: w3cos_std::style::Edges::xy(14.0, 11.0),
                word_break: w3cos_std::style::WordBreak::BreakWord,
                ..Style::default()
            },
        );
        let content = Component::column(
            Style {
                display: WDisplay::Flex,
                flex_direction: WDir::Column,
                align_items: WAlign::FlexStart,
                min_width: WDim::Px(0.0),
                gap: 4.0,
                ..Style::default()
            },
            vec![Component::text("LogiDesk", Style::default()), message_text],
        );
        let message = Component::boxed(
            Style {
                display: WDisplay::Grid,
                grid_template_columns: Some("34px minmax(0, 1fr)".to_string()),
                column_gap: Some(10.0),
                max_width: WDim::Percent(92.0),
                ..Style::default()
            },
            vec![avatar, content],
        );
        let feed = Component::column(
            Style {
                width: WDim::Px(370.0),
                flex_direction: WDir::Column,
                ..Style::default()
            },
            vec![message],
        );

        let layout = compute(&feed, 402.0, 874.0).unwrap();
        let message_rect = layout.iter().find(|(_, idx)| *idx == 1).unwrap().0;
        let text_rect = layout.iter().find(|(_, idx)| *idx == 5).unwrap().0;
        assert!(
            message_rect.width <= 340.5,
            "message width {} exceeded 92% of its 370px feed",
            message_rect.width
        );
        assert!(
            text_rect.x + text_rect.width <= message_rect.x + message_rect.width + 0.1,
            "text rect {text_rect:?} overflowed message rect {message_rect:?}"
        );
    }

    #[test]
    fn mixed_text_button_children() {
        let l = compute(
            &Component::column(
                col(),
                vec![Component::text("T", s()), Component::button("B", s())],
            ),
            800.0,
            600.0,
        )
        .unwrap();
        assert_eq!(l.len(), 3);
    }

    #[test]
    fn centered_column_text_children_keep_intrinsic_width() {
        let layout = compute(
            &Component::column(
                Style {
                    display: WDisp::Flex,
                    flex_direction: WDir::Column,
                    align_items: WAlign::Center,
                    width: WDim::Px(320.0),
                    ..Style::default()
                },
                vec![
                    Component::text("Product", s()),
                    Component::text("Welcome back", s()),
                    Component::text("Connect with a trusted identity", s()),
                ],
            ),
            390.0,
            844.0,
        )
        .unwrap();

        assert_eq!(layout.len(), 4);
        for (rect, _) in &layout[1..] {
            assert!(
                rect.width > 0.0,
                "centered text must remain paintable: {rect:?}"
            );
        }
    }

    #[test]
    fn column_vs_row_axes_differ() {
        let cl = compute(
            &Component::column(
                col(),
                vec![Component::text("A", s()), Component::text("B", s())],
            ),
            800.0,
            600.0,
        )
        .unwrap();
        let rl = compute(
            &Component::row(
                row(),
                vec![Component::text("A", s()), Component::text("B", s())],
            ),
            800.0,
            600.0,
        )
        .unwrap();
        assert!((cl[2].0.y - cl[1].0.y).abs() > (cl[2].0.x - cl[1].0.x).abs());
        assert!((rl[2].0.x - rl[1].0.x).abs() > (rl[2].0.y - rl[1].0.y).abs());
    }

    #[test]
    fn zero_viewport() {
        assert_eq!(
            compute(&Component::text("Z", s()), 0.0, 0.0).unwrap().len(),
            1
        );
    }

    #[test]
    fn narrow_viewport() {
        assert_eq!(
            compute(&Component::text("N", s()), 100.0, 100.0)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn rect_clone_debug() {
        let r = LayoutRect {
            x: 1.0,
            y: 2.0,
            width: 3.0,
            height: 4.0,
        };
        assert_eq!(r.x, r.clone().x);
        assert!(format!("{:?}", r).contains("LayoutRect"));
    }

    #[test]
    fn single_child_inside_parent() {
        let l = compute(
            &Component::column(col(), vec![Component::text("O", s())]),
            800.0,
            600.0,
        )
        .unwrap();
        assert!(l[1].0.x >= l[0].0.x);
        assert!(l[1].0.y >= l[0].0.y);
    }

    #[test]
    fn fixed_size_box_respected() {
        let l = compute(
            &Component::boxed(
                Style {
                    width: WDim::Px(200.0),
                    height: WDim::Px(100.0),
                    ..Style::default()
                },
                vec![],
            ),
            800.0,
            600.0,
        )
        .unwrap();
        assert!((l[0].0.width - 200.0).abs() < 2.0);
        assert!((l[0].0.height - 100.0).abs() < 2.0);
    }

    #[test]
    fn five_children_column() {
        let children: Vec<_> = (0..5)
            .map(|i| Component::text(&i.to_string(), s()))
            .collect();
        assert_eq!(
            compute(&Component::column(col(), children), 800.0, 600.0)
                .unwrap()
                .len(),
            6
        );
    }

    #[test]
    fn text_width_scales_with_length() {
        let short = compute(&Component::text("A", s()), 800.0, 600.0).unwrap();
        let long = compute(
            &Component::text("A very long text string", s()),
            800.0,
            600.0,
        )
        .unwrap();
        assert!(long[0].0.width > short[0].0.width);
    }

    #[test]
    fn large_viewport() {
        let l = compute(&Component::text("Big", s()), 4000.0, 3000.0).unwrap();
        assert!(l[0].0.width > 0.0);
    }

    #[test]
    fn pre_flatten_node_count() {
        let tree = Component::column(
            col(),
            vec![
                Component::text("A", s()),
                Component::row(row(), vec![Component::text("B", s())]),
            ],
        );
        let flat = pre_flatten(&tree);
        assert_eq!(flat.len(), 4);
        assert!(flat[0].parent.is_none());
        assert_eq!(flat[1].parent, Some(0));
        assert_eq!(flat[2].parent, Some(0));
        assert_eq!(flat[3].parent, Some(2));
        assert_ne!(flat[1].stable_id, flat[2].stable_id);

        let mut rebuilt = tree.clone();
        rebuilt.children[0].style.opacity = 0.5;
        let rebuilt_flat = pre_flatten(&rebuilt);
        assert_eq!(flat[1].stable_id, rebuilt_flat[1].stable_id);
        assert_eq!(flat[3].stable_id, rebuilt_flat[3].stable_id);
    }

    #[test]
    fn layout_engine_recompute_without_rebuild() {
        let tree = Component::column(col(), vec![Component::text("X", s())]);
        let flat = pre_flatten(&tree);
        let mut engine = LayoutEngine::new();
        let r1 = engine.compute(&tree, &flat, 800.0, 600.0).unwrap();
        assert_eq!(r1.layout_cache.len(), 2);

        let r2 = engine.compute(&tree, &flat, 1200.0, 800.0).unwrap();
        assert_eq!(r2.layout_cache.len(), 2);
    }

    #[test]
    fn static_overflow_ancestor_does_not_clip_viewport_positioned_absolute_descendant() {
        let absolute = Component::boxed(
            Style {
                display: WDisp::Block,
                position: WPos::Absolute,
                width: WDim::Px(100.0),
                height: WDim::Px(100.0),
                ..Style::default()
            },
            Vec::new(),
        );
        let tree = Component::boxed(
            Style {
                display: WDisp::Block,
                ..Style::default()
            },
            vec![Component::boxed(
                Style {
                    display: WDisp::Block,
                    overflow: WOverflow::Hidden,
                    width: WDim::Px(100.0),
                    height: WDim::Px(100.0),
                    ..Style::default()
                },
                vec![Component::boxed(
                    Style {
                        display: WDisp::Inline,
                        ..Style::default()
                    },
                    vec![absolute],
                )],
            )],
        );
        let flat = pre_flatten(&tree);
        let mut engine = LayoutEngine::new();

        let result = engine.compute(&tree, &flat, 800.0, 600.0).unwrap();

        assert_eq!(result.scroll_ancestor[3], None);
        assert!(
            result.clip_only_nodes.iter().any(|(index, _)| *index == 1)
                || result
                    .scrollable_nodes
                    .iter()
                    .any(|(index, _, _)| *index == 1)
        );
    }

    #[test]
    fn second_full_layout_is_skipped_when_text_heights_are_clean() {
        let tree = Component::column(
            Style {
                display: WDisp::Flex,
                flex_direction: WDir::Column,
                width: WDim::Px(80.0),
                ..Style::default()
            },
            vec![Component::text(
                "word word word word word word word word word",
                s(),
            )],
        );
        let flat = pre_flatten(&tree);
        let mut engine = LayoutEngine::new();
        let first = engine.compute(&tree, &flat, 80.0, 600.0).unwrap();
        assert_eq!(first.layout_cache.len(), 2);
        assert_eq!(
            engine.last_compute_layout_passes, 2,
            "first pass must still reflow wrapped Auto-height text"
        );
        let second = engine.compute(&tree, &flat, 80.0, 600.0).unwrap();
        assert_eq!(second.layout_cache, first.layout_cache);
        assert_eq!(
            engine.last_compute_layout_passes, 1,
            "clean text heights must skip the second full Taffy pass"
        );
    }

    #[test]
    fn layout_display_detects_show_toggle() {
        let hidden = Style {
            display: WDisp::None,
            ..Style::default()
        };
        let shown = Style {
            display: WDisp::Flex,
            flex_direction: WDir::Column,
            ..Style::default()
        };
        let a = Component::column(hidden.clone(), vec![Component::text("x", Style::default())]);
        let b = Component::column(shown.clone(), vec![Component::text("x", Style::default())]);
        assert!(!layout_display_unchanged(
            &pre_flatten(&a),
            &pre_flatten(&b)
        ));
        assert!(layout_shape_unchanged(&pre_flatten(&a), &pre_flatten(&b)));
        assert!(layout_styles_unchanged_except_display(
            &pre_flatten(&a),
            &pre_flatten(&b)
        ));
    }

    #[test]
    fn layout_style_detects_reused_absolute_slot_movement() {
        let first = Style {
            position: WPos::Absolute,
            top: WDim::Px(84.0),
            ..Style::default()
        };
        let moved = Style {
            top: WDim::Px(83_916.0),
            ..first.clone()
        };
        let a = Component::boxed(first, vec![Component::text("row", Style::default())]);
        let b = Component::boxed(moved, vec![Component::text("row", Style::default())]);

        assert!(layout_shape_unchanged(&pre_flatten(&a), &pre_flatten(&b)));
        assert!(!layout_styles_unchanged_except_display(
            &pre_flatten(&a),
            &pre_flatten(&b)
        ));
    }

    #[test]
    fn layout_shape_ignores_reactive_text_width() {
        let col = || Style {
            display: WDisp::Flex,
            flex_direction: WDir::Column,
            ..Style::default()
        };
        let s = || Style {
            font_size: 14.0,
            ..Style::default()
        };
        let a = Component::column(
            col(),
            vec![Component::text("9", s()), Component::button("Tap", s())],
        );
        let b = Component::column(
            col(),
            vec![Component::text("1000", s()), Component::button("Tap", s())],
        );
        let fa = pre_flatten(&a);
        let fb = pre_flatten(&b);
        assert!(layout_shape_unchanged(&fa, &fb));
    }

    #[test]
    fn is_node_visible_respects_display_none_wrapper() {
        let wrap = Style {
            display: WDisp::None,
            ..Style::default()
        };
        let tree = Component::column(wrap, vec![Component::text("hidden", Style::default())]);
        let flat = pre_flatten(&tree);
        assert!(!is_node_visible(&flat, 1));
        assert!(!is_node_visible(&flat, 0));
    }

    #[test]
    fn repeated_text_measurements_reuse_retained_metrics() {
        TEXT_MEASURE_CACHE.with(|cache| *cache.borrow_mut() = TextMeasureCache::default());
        let style = Style {
            font_size: 15.0,
            line_height: 1.4,
            ..Style::default()
        };
        for _ in 0..1_000 {
            let _ = text_intrinsic_size("上海 → 杭州运输节点已更新", &style);
            let _ = wrapped_text_height("上海 → 杭州运输节点已更新", 320.0, &style);
        }
        let entries = TEXT_MEASURE_CACHE.with(|cache| cache.borrow().entries);
        assert_eq!(
            entries, 2,
            "identical layout measurements should be retained"
        );

        let _ = wrapped_text_height("上海 → 杭州运输节点已更新", 280.0, &style);
        let entries = TEXT_MEASURE_CACHE.with(|cache| cache.borrow().entries);
        assert_eq!(entries, 3, "assigned width is part of the cache key");
    }

    #[cfg(feature = "skia")]
    #[test]
    fn shaped_intrinsic_width_does_not_create_a_second_paint_line() {
        let style = Style {
            font_size: 16.0,
            line_height: 1.2,
            ..Style::default()
        };
        let (width, height) = text_intrinsic_size("PASS", &style);
        assert_eq!(wrapped_text_height("PASS", width, &style), height);
    }

    #[test]
    fn registered_css_font_drives_layout_metrics_and_cache_identity() {
        const OWNER: u64 = 0x4c41_594f_5554;
        const FAMILY: &str = "W3COS Narrow Layout Test";
        fn table_offset(bytes: &[u8], tag: &[u8; 4]) -> Option<usize> {
            let table_count = u16::from_be_bytes(bytes.get(4..6)?.try_into().ok()?) as usize;
            (0..table_count).find_map(|index| {
                let entry = 12 + index * 16;
                (bytes.get(entry..entry + 4)? == tag).then(|| {
                    u32::from_be_bytes(bytes[entry + 8..entry + 12].try_into().unwrap()) as usize
                })
            })
        }

        let mut narrow_bytes = include_bytes!("../assets/Inter-Regular.ttf").to_vec();
        let hhea = table_offset(&narrow_bytes, b"hhea").expect("hhea table");
        let hmtx = table_offset(&narrow_bytes, b"hmtx").expect("hmtx table");
        let metrics = u16::from_be_bytes(
            narrow_bytes[hhea + 34..hhea + 36]
                .try_into()
                .expect("numberOfHMetrics"),
        ) as usize;
        for index in 0..metrics {
            let offset = hmtx + index * 4;
            let advance = u16::from_be_bytes(
                narrow_bytes[offset..offset + 2]
                    .try_into()
                    .expect("advance width"),
            );
            narrow_bytes[offset..offset + 2].copy_from_slice(&(advance / 2).max(1).to_be_bytes());
        }

        let style = Style {
            font_size: 20.0,
            white_space: WWhiteSpace::NoWrap,
            ..Style::default()
        };
        let fallback = text_intrinsic_size("WWWWWWWW", &style).0;
        crate::font_face::FontRegistry::global()
            .register_for_owner(
                OWNER,
                crate::font_face::FontFace {
                    family: FAMILY.to_string(),
                    src: crate::font_face::FontSource::Bytes(narrow_bytes),
                    unicode_range: Some("U+0057".to_string()),
                    ..crate::font_face::FontFace::default()
                },
            )
            .expect("register narrow test font");
        crate::font_face::FontRegistry::global()
            .register_for_owner(
                OWNER,
                crate::font_face::FontFace {
                    family: FAMILY.to_string(),
                    src: crate::font_face::FontSource::Bytes(
                        include_bytes!("../assets/Inter-Regular.ttf").to_vec(),
                    ),
                    unicode_range: Some("U+0030-0039".to_string()),
                    ..crate::font_face::FontFace::default()
                },
            )
            .expect("register digit subset test font");
        let custom_style = Style {
            font_family: Some(format!("Missing Font, \"{FAMILY}\"")),
            ..style.clone()
        };
        let custom = text_intrinsic_size("WWWWWWWW", &custom_style).0;
        assert!(
            custom < fallback * 0.75,
            "registered family must change measured width ({custom} vs {fallback})"
        );
        let narrow_w = text_intrinsic_size("W", &custom_style).0;
        let regular_digit = text_intrinsic_size("3", &custom_style).0;
        let mixed = text_intrinsic_size("W3W", &custom_style).0;
        assert!(
            (mixed - (narrow_w * 2.0 + regular_digit)).abs() < 0.01,
            "mixed subset runs must use each face's own metrics"
        );
        assert!(
            crate::font_face::FontRegistry::global()
                .resolve_style_for_character(&custom_style, 'A')
                .is_none(),
            "characters outside every unicode-range must continue through fallback"
        );

        crate::font_face::FontRegistry::global().clear_owner(OWNER);
        let restored = text_intrinsic_size("WWWWWWWW", &custom_style).0;
        assert!(
            (restored - fallback).abs() < 0.01,
            "font removal must not reuse stale custom metrics"
        );
    }

    #[test]
    fn persistent_layout_reflows_parent_when_show_branch_collapses() {
        let hidden = Style {
            display: WDisp::None,
            ..Style::default()
        };
        let visible = Style::default();
        let compact = Component::column(
            Style {
                height: WDim::Px(52.0),
                ..Style::default()
            },
            vec![],
        );
        let expanded = Component::column(
            Style {
                height: WDim::Px(520.0),
                ..Style::default()
            },
            vec![],
        );
        let make_tree = |compact_display: Style, expanded_display: Style| {
            Component::column(
                col(),
                vec![
                    Component::column(
                        Style {
                            position: WPos::Sticky,
                            ..Style::default()
                        },
                        vec![
                            Component::column(compact_display, vec![compact.clone()]),
                            Component::column(expanded_display, vec![expanded.clone()]),
                        ],
                    ),
                    Component::boxed(
                        Style {
                            height: WDim::Px(100.0),
                            ..Style::default()
                        },
                        vec![],
                    ),
                ],
            )
        };
        let old_tree = make_tree(hidden.clone(), visible.clone());
        let new_tree = make_tree(visible, hidden);
        let old_flat = pre_flatten(&old_tree);
        let new_flat = pre_flatten(&new_tree);
        assert!(layout_shape_unchanged(&old_flat, &new_flat));

        let mut engine = LayoutEngine::new();
        let old = engine.compute(&old_tree, &old_flat, 375.0, 700.0).unwrap();
        engine.patch_display_styles(&new_flat).unwrap();
        let new = engine.compute(&new_tree, &new_flat, 375.0, 700.0).unwrap();
        let rect = |results: &LayoutResults, idx| {
            results
                .layout_cache
                .iter()
                .find(|(_, node_idx)| *node_idx == idx)
                .map(|(rect, _)| *rect)
                .unwrap()
        };
        assert_eq!(rect(&old, 1).height, 520.0);
        assert_eq!(rect(&new, 1).height, 52.0);
        assert_eq!(rect(&new, 6).y, 78.0);
    }

    /// Host micro-bench for CI — logs 402×874 layout time budget.
    #[test]
    fn layout_microbench() {
        use std::time::Instant;
        let children: Vec<_> = (0..40)
            .map(|i| {
                Component::row(
                    row(),
                    vec![
                        Component::text(&format!("item-{i}"), s()),
                        Component::button("Tap", Style::default()),
                    ],
                )
            })
            .collect();
        let tree = Component::column(
            Style {
                display: WDisp::Flex,
                flex_direction: WDir::Column,
                gap: 8.0,
                padding: w3cos_std::style::Edges::all(20.0),
                width: WDim::Percent(100.0),
                height: WDim::Percent(100.0),
                overflow: WOverflow::Scroll,
                ..Style::default()
            },
            children,
        );
        let flat = pre_flatten(&tree);
        let mut engine = LayoutEngine::new();
        let t0 = Instant::now();
        for _ in 0..50 {
            let _ = engine.compute(&tree, &flat, 402.0, 874.0).unwrap();
        }
        let avg_us = t0.elapsed().as_micros() / 50;
        eprintln!("layout_microbench: 402×874 avg {avg_us}µs (50 iter)");
        assert!(avg_us < 8_000, "layout avg {avg_us}µs exceeds 8ms budget");
    }
}
