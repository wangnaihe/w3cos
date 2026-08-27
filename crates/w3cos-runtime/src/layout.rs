use anyhow::Result;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::OnceLock;
use taffy::prelude::*;
use w3cos_std::component::EventAction;
use w3cos_std::style::{
    AlignItems as WAlign, AlignSelf as WAlignSelf, BoxSizing as WBoxSizing, Dimension as WDim,
    Display as WDisplay, EdgeLengths, FlexDirection as WDir, FlexWrap as WWrap,
    JustifyContent as WJustify, Overflow as WOverflow, Position as WPos, Spacing as WSpacing,
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
fn leaf_intrinsic_size(kind: &ComponentKind, style: &w3cos_std::style::Style) -> (f32, f32) {
    match kind {
        ComponentKind::Text { content } => text_intrinsic_size(content, style),
        ComponentKind::Button { label } => button_intrinsic_size(label, style),
        ComponentKind::Image { src } => {
            let (natural_width, natural_height) = crate::image_loader::dimensions(src)
                .map(|(width, height)| (width as f32, height as f32))
                .unwrap_or_else(|| {
                    if crate::image_loader::is_reserved_browser_source(src) {
                        (0.0, 0.0)
                    } else {
                        (200.0, 200.0)
                    }
                });
            let width = dim_to_px(style.width);
            let height = dim_to_px(style.height);
            match (width, height) {
                (Some(width), Some(height)) => (width, height),
                (Some(width), None) if natural_width > 0.0 => {
                    (width, natural_height * width / natural_width)
                }
                (None, Some(height)) if natural_height > 0.0 => {
                    (natural_width * height / natural_height, height)
                }
                (Some(width), None) => (width, natural_height),
                (None, Some(height)) => (natural_width, height),
                (None, None) => (natural_width, natural_height),
            }
        }
        ComponentKind::TextInput { .. } => {
            // Match the browser UA baseline for an `<input size="20">`
            // instead of imposing the former mobile-only 200×40 control.
            let w = dim_to_px(style.width).unwrap_or(169.0);
            let h = dim_to_px(style.height).unwrap_or(20.0);
            (w, h)
        }
        ComponentKind::SvgDocument { width, height, .. } => (*width as f32, *height as f32),
        _ => (0.0, 0.0),
    }
}

fn component_max_content_width(component: &Component) -> f32 {
    let child_width = |child: &Component| component_max_content_width(child);
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
    let specified_width = match component.style.width {
        WDim::Px(width) => Some(width),
        WDim::Em(width) => Some(width * component.style.font_size),
        WDim::Rem(width) => Some(width * ROOT_FONT_SIZE),
        _ => None,
    };
    let padding = component.style.padding_lengths();
    let border_width = component
        .style
        .border_left_width
        .unwrap_or(component.style.border_width)
        + component
            .style
            .border_right_width
            .unwrap_or(component.style.border_width);
    let table_outer_spacing = if matches!(
        component.style.display,
        WDisplay::Table | WDisplay::InlineTable
    ) {
        component.style.border_spacing_x * 2.0
    } else {
        0.0
    };
    let horizontal_inner_edges =
        padding.left + padding.right + border_width + table_outer_spacing;
    let border_box_width = match (specified_width, component.style.box_sizing) {
        (Some(width), WBoxSizing::BorderBox) => width,
        (Some(width), WBoxSizing::ContentBox) => width + horizontal_inner_edges,
        (None, _) => intrinsic_width + horizontal_inner_edges,
    };
    let margin = component.style.margin_lengths();
    let horizontal_margin = if component.style.display == WDisplay::TableCell {
        0.0
    } else {
        margin.left + margin.right
    };
    border_box_width + horizontal_margin
}

fn table_track_max_content_width(component: &Component) -> f32 {
    let tracks = table_track_widths(component);
    if tracks.is_empty() {
        return component
            .children
            .iter()
            .map(component_max_content_width)
            .fold(0.0_f32, f32::max);
    }
    let gap = component.style.border_spacing_x;
    tracks.iter().sum::<f32>() + gap * tracks.len().saturating_sub(1) as f32
}

fn table_track_widths(component: &Component) -> Vec<f32> {
    fn collect_rows(component: &Component, tracks: &mut Vec<f32>) {
        if component.style.display == WDisplay::TableRow {
            for (column, cell) in component.children.iter().enumerate() {
                if column >= tracks.len() {
                    tracks.push(0.0);
                }
                tracks[column] = tracks[column].max(component_max_content_width(cell));
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
            ) {
                collect_rows(child, tracks);
            }
        }
    }

    let mut tracks = Vec::new();
    collect_rows(component, &mut tracks);
    tracks
}

fn shrink_to_fit_used_width(component: &Component) -> f32 {
    let outer_width = component_max_content_width(component);
    if matches!(
        component.style.display,
        WDisplay::Inline | WDisplay::InlineBlock | WDisplay::InlineFlex | WDisplay::InlineTable
    ) {
        // Taffy applies an inline-level box's margins separately. Its assigned
        // width is the border box, so do not make the painted background span
        // the margin. Table max-content aggregation still needs outer widths.
        let margin = component.style.margin_lengths();
        (outer_width - margin.left - margin.right).max(0.0)
    } else {
        outer_width
    }
}

fn dim_to_px(dim: WDim) -> Option<f32> {
    match dim {
        WDim::Px(v) => Some(v),
        WDim::Percent(p) => Some(p),
        WDim::Auto | WDim::Rem(_) | WDim::Em(_) | WDim::Vw(_) | WDim::Vh(_) => None,
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
) -> taffy::Size<Dimension> {
    // `display:inline` normally forces both axes to auto in `to_taffy_style`,
    // but replaced elements such as `<img>` still honor their CSS width and
    // height. Use the semantic dimensions here before applying leaf sizing.
    let replaced_width = (matches!(kind, ComponentKind::Image { .. })
        && !matches!(style.width, WDim::Auto))
    .then_some(base.size.width);
    let replaced_height = (matches!(kind, ComponentKind::Image { .. })
        && !matches!(style.height, WDim::Auto))
    .then_some(base.size.height);
    let width = if let Some(width) = replaced_width {
        width
    } else if matches!(style.width, WDim::Auto) {
        match kind {
            ComponentKind::TextInput { .. } => {
                Dimension::length(leaf_intrinsic_size(kind, style).0)
            }
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
            _ => leaf_intrinsic_size(kind, style).1,
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
            self.root_node = Some(build_taffy_tree(
                &mut self.tree,
                root,
                &mut idx,
                None,
                None,
                None,
                viewport_w,
                viewport_h,
                viewport_w,
                None,
                None,
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

        project_table_column_background_rects(&mut results, flat);

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
    let mut tree: TaffyTree<usize> = TaffyTree::new();
    tree.disable_rounding();
    let mut node_index: usize = 0;

    let root_node = build_taffy_tree(
        &mut tree,
        root,
        &mut node_index,
        None,
        None,
        None,
        viewport_w,
        viewport_h,
        viewport_w,
        None,
        None,
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

    project_table_column_background_rects(&mut results, &flat);

    extend_scroll_extents_from_descendants(&results, &flat, &scroll_ancestor, &mut scrollable);

    results.extend(fixed_results);
    Ok((results, scrollable, clip_only))
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
    let mut row_bounds = HashMap::<usize, (f32, f32)>::new();
    for (rect, index) in layouts.iter() {
        if !matches!(
            flat.get(*index).map(|entry| entry.style.display),
            Some(WDisplay::TableRow)
        ) {
            continue;
        }
        let Some(table) = nearest_table(*index) else {
            continue;
        };
        row_bounds
            .entry(table)
            .and_modify(|(top, bottom)| {
                *top = top.min(rect.y);
                *bottom = bottom.max(rect.y + rect.height);
            })
            .or_insert((rect.y, rect.y + rect.height));
    }
    for (rect, index) in layouts.iter_mut() {
        if !matches!(
            flat.get(*index).map(|entry| entry.style.display),
            Some(WDisplay::TableColumnGroup | WDisplay::TableColumn)
        ) {
            continue;
        }
        let Some((top, bottom)) = nearest_table(*index).and_then(|table| row_bounds.get(&table))
        else {
            continue;
        };
        rect.y = *top;
        rect.height = bottom - top;
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
    let specified = style.width.resolve(
        containing_width,
        ROOT_FONT_SIZE,
        style.font_size,
        viewport_w,
        viewport_h,
    );
    let mut width = specified.unwrap_or(containing_width);
    if style.box_sizing == WBoxSizing::BorderBox {
        let resolve_edge = |spacing: WSpacing| match spacing {
            WSpacing::Percent(value) => containing_width * value / 100.0,
            WSpacing::Rem(value) => value * ROOT_FONT_SIZE,
            WSpacing::Em(value) => value * style.font_size,
            WSpacing::Vw(value) => value * viewport_w / 100.0,
            WSpacing::Vh(value) => value * viewport_h / 100.0,
            WSpacing::Auto => 0.0,
            other => other.resolve(&w3cos_std::safe_area::current()),
        };
        width -= resolve_edge(style.padding.left)
            + resolve_edge(style.padding.right)
            + style.border_left_width.unwrap_or(style.border_width)
            + style.border_right_width.unwrap_or(style.border_width);
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
    viewport_w: f32,
    viewport_h: f32,
    containing_width: f32,
    inherited_table_tracks: Option<&[f32]>,
    table_column: Option<usize>,
    inherited_border_spacing: Option<(f32, f32)>,
) -> Result<NodeId, taffy::TaffyError> {
    let my_idx = *idx;
    *idx += 1;

    let mut style = to_taffy_style(&comp.style, viewport_w, viewport_h);
    let own_border_spacing = matches!(comp.style.display, WDisplay::Table | WDisplay::InlineTable)
        .then_some((comp.style.border_spacing_x, comp.style.border_spacing_y));
    let active_border_spacing = own_border_spacing.or(inherited_border_spacing);
    if matches!(comp.style.display, WDisplay::Table | WDisplay::InlineTable) {
        let padding = comp.style.padding_lengths();
        style.padding = Rect {
            top: LengthPercentage::length(padding.top + comp.style.border_spacing_y),
            right: LengthPercentage::length(padding.right + comp.style.border_spacing_x),
            bottom: LengthPercentage::length(padding.bottom + comp.style.border_spacing_y),
            left: LengthPercentage::length(padding.left + comp.style.border_spacing_x),
        };
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
    if comp.style.display == WDisplay::TableCell
        && let Some(width) = table_column
            .and_then(|column| inherited_table_tracks?.get(column))
            .copied()
    {
        let padding = comp.style.padding_lengths();
        let horizontal_inner_edges = padding.left
            + padding.right
            + comp
                .style
                .border_left_width
                .unwrap_or(comp.style.border_width)
            + comp
                .style
                .border_right_width
                .unwrap_or(comp.style.border_width);
        let content_width = (width - horizontal_inner_edges).max(0.0);
        style.size.width = Dimension::length(content_width);
        style.flex_basis = Dimension::length(content_width);
        style.flex_grow = 0.0;
        style.flex_shrink = 0.0;
    }
    let normal_flow_children = comp.children.iter().filter(|child| {
        !matches!(child.style.position, WPos::Absolute | WPos::Fixed)
            && child.style.display != WDisplay::None
    });
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
    if establishes_inline_formatting_context {
        // A block whose normal-flow children are all inline-level establishes
        // line boxes. Keep the inherited line-height strut even when a shorter
        // replaced element is the only child, and let vertical-align map onto
        // the row cross axis.
        style.display = taffy::Display::Flex;
        style.flex_direction = FlexDirection::Row;
        style.flex_wrap = FlexWrap::Wrap;
        style.align_items = Some(AlignItems::FlexStart);
        if matches!(comp.style.min_height, WDim::Auto) {
            style.min_size.height = Dimension::length(comp.style.font_size * comp.style.line_height);
        }
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
    if comp.style.display == WDisplay::InlineTable
        && comp.children.iter().any(|child| {
            matches!(
                child.style.display,
                WDisplay::TableRow
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
    // CSS resolves every percentage padding side against the containing
    // block's width. Taffy leaves vertical percentages unresolved when that
    // block has an indefinite height, so provide their pixel basis here.
    if let WSpacing::Percent(value) = comp.style.padding.top {
        style.padding.top = LengthPercentage::length(containing_width * value / 100.0);
    }
    if let WSpacing::Percent(value) = comp.style.padding.bottom {
        style.padding.bottom = LengthPercentage::length(containing_width * value / 100.0);
    }
    let child_containing_width =
        component_content_width(&comp.style, containing_width, viewport_w, viewport_h);
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
        let size = leaf_taffy_size(&comp.kind, &comp.style, &style, parent_display);
        let inline_control_in_block =
            matches!(
                comp.style.display,
                WDisplay::InlineBlock | WDisplay::InlineFlex
            ) && !matches!(parent_display, Some(WDisplay::Flex | WDisplay::Grid));
        let (min_w, size_w) = if matches!(comp.style.width, WDim::Auto) {
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
                    let shrink_to_fit = inline_text_in_block
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
                        if let WDim::Px(mw) = comp.style.min_width {
                            w = w.max(mw);
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
                ComponentKind::Image { .. } => {
                    let w = leaf_intrinsic_size(&comp.kind, &comp.style).0;
                    (Dimension::length(w), Dimension::length(w))
                }
                _ => (Dimension::auto(), size.width),
            }
        } else {
            (Dimension::auto(), size.width)
        };
        let intrinsic_min_h = if matches!(comp.style.height, WDim::Auto) {
            match &comp.kind {
                ComponentKind::Text { content } => Dimension::length(
                    text_intrinsic_size_in_parent_for_taffy(content, &comp.style, parent_display).1,
                ),
                ComponentKind::Button { label } => {
                    Dimension::length(button_intrinsic_size(label, &comp.style).1)
                }
                // Replaced elements keep their intrinsic cross-axis size when
                // their CSS height is `auto`. Without this automatic minimum,
                // a grid/flex parent can collapse an otherwise valid image to
                // 0px before the paint pass ever gets a chance to decode it.
                ComponentKind::Image { .. } => {
                    Dimension::length(leaf_intrinsic_size(&comp.kind, &comp.style).1)
                }
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
        let owned_table_tracks = (matches!(
            comp.style.display,
            WDisplay::Table | WDisplay::InlineTable
        ) && matches!(comp.style.width, WDim::Auto))
        .then(|| table_track_widths(comp));
        let active_table_tracks = owned_table_tracks
            .as_deref()
            .filter(|tracks| !tracks.is_empty())
            .or(inherited_table_tracks);
        let mut child_nodes: Vec<(i32, usize, NodeId)> = comp
            .children
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
                    viewport_w,
                    viewport_h,
                    child_containing_width,
                    active_table_tracks,
                    (comp.style.display == WDisplay::TableRow).then_some(source_index),
                    active_border_spacing,
                )?;
                Ok((c.style.order, source_index, node))
            })
            .collect::<Result<_, _>>()?;
        child_nodes.sort_by_key(|(order, source_index, _)| (*order, *source_index));
        let rescue_overwide_first_pair = if comp.style.flex_wrap == WWrap::Wrap
            && child_nodes.len() >= 2
        {
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
                component_content_width(
                    &comp.style,
                    containing_width,
                    viewport_w,
                    viewport_h,
                )
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
        if matches!(comp.style.width, WDim::Auto)
            && matches!(
                comp.style.display,
                WDisplay::Inline
                    | WDisplay::InlineBlock
                    | WDisplay::InlineFlex
                    | WDisplay::InlineTable
                    | WDisplay::Table
            )
            && (matches!(parent_display, Some(WDisplay::Block | WDisplay::Grid))
                || (matches!(parent_display, Some(WDisplay::Flex))
                    && matches!(
                        comp.style.display,
                        WDisplay::InlineBlock | WDisplay::InlineFlex | WDisplay::InlineTable
                    )))
        {
            style.size.width = Dimension::length(shrink_to_fit_used_width(comp));
            if matches!(comp.style.display, WDisplay::Table | WDisplay::InlineTable) {
                // `shrink_to_fit_used_width` returns the table border box.
                // Keep table padding, borders, and outer border spacing inside
                // that resolved width instead of adding them a second time.
                style.box_sizing = BoxSizing::BorderBox;
            }
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
                    if taffy_style.min_size.height != measured_height
                        || taffy_style.size.height != measured_height
                    {
                        taffy_style.min_size.height = measured_height;
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

    let mut new_scroll_container = current_scroll_container;
    let mut descendant_containing_block = absolute_containing_block;

    if let Some(&ctx) = tree.get_node_context(node) {
        if ctx < flat.len() {
            if !is_node_visible(flat, ctx) {
                return;
            }
            let info = &flat[ctx];

            scroll_ancestor[ctx] = current_scroll_container;

            if matches!(info.style.position, WPos::Fixed) {
                rect = compute_fixed_rect(info.style, viewport_w, viewport_h, rect);
                fixed_out.push((rect, ctx));
            } else {
                if matches!(info.style.position, WPos::Absolute) {
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
                    );
                } else if matches!(info.style.position, WPos::Relative) {
                    rect = compute_relative_percentage_rect(
                        info.style,
                        relative_containing_block,
                        relative_containing_block_height_definite,
                        rect,
                    );
                }
                out.push((rect, ctx));
            }

            if !matches!(info.style.position, WPos::Static) {
                descendant_containing_block = positioned_descendant_containing_block(
                    flat, tree, node, rect,
                );
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
        y: rect.y + layout.border.top + layout.padding.top,
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
            WDim::Px(_) | WDim::Rem(_) | WDim::Em(_) | WDim::Vw(_) | WDim::Vh(_) => true,
        });

    for &child in tree.children(node).unwrap().iter() {
        collect_layouts_fast(
            flat,
            tree,
            child,
            rect.x,
            rect.y,
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
        return rect;
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
            cursor_y = layout.location.y
                + layout.size.height
                + sibling_info.style.margin_lengths().bottom;
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
                    cursor_y += line_height.max(
                        sibling_info.style.font_size * sibling_info.style.line_height,
                    );
                    line_height = 0.0;
                    crossed_forced_line_break = true;
                    continue;
                }
                if crossed_forced_line_break
                    && cursor_x == 0.0
                    && content.chars().all(char::is_whitespace)
                {
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
            rect.x = containing_block.x;
            rect.y = containing_block.y;
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
        rect.x = containing_block.x;
        rect.y = containing_block.y + cursor_y + line_height;
    } else {
        rect.x = containing_block.x - fragmented_inline_start_padding + cursor_x;
        rect.y = containing_block.y + cursor_y;
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
) -> LayoutRect {
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
    );

    let x = match (resolve_h(style.left), resolve_h(style.right)) {
        (Some(left), _) => containing_block.x + left,
        (None, Some(right)) => containing_block.x + containing_block.width - right - width,
        (None, None) => fallback.x,
    };
    let y = match (resolve_v(style.top), resolve_v(style.bottom)) {
        (Some(top), _) => containing_block.y + top,
        (None, Some(bottom)) => containing_block.y + containing_block.height - bottom - height,
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
    let width = if matches!(style.width, WDim::Auto)
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
        )
    {
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
    let height = if matches!(style.height, WDim::Auto)
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
        )
    {
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
        | WDisplay::TableFooterGroup
        | WDisplay::TableCell => (
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
        | WDisplay::ListItem => (
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
    let margin = if s.display == WDisplay::TableCell {
        // CSS table-cell boxes do not accept margins. Taffy's flex fallback
        // would otherwise add those margins to column and row sizing.
        Rect {
            top: LengthPercentageAuto::length(0.0),
            right: LengthPercentageAuto::length(0.0),
            bottom: LengthPercentageAuto::length(0.0),
            left: LengthPercentageAuto::length(0.0),
        }
    } else {
        Rect {
            top: to_taffy_margin(s.margin.top, s.font_size, viewport_w, viewport_h),
            right: to_taffy_margin(s.margin.right, s.font_size, viewport_w, viewport_h),
            bottom: to_taffy_margin(s.margin.bottom, s.font_size, viewport_w, viewport_h),
            left: to_taffy_margin(s.margin.left, s.font_size, viewport_w, viewport_h),
        }
    };

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
                | WDisplay::TableFooterGroup,
                _,
            ) => FlexDirection::Column,
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
    if matches!(position, WPos::Relative) && matches!(dimension, WDim::Percent(_)) {
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
        );
        assert_eq!((static_position.x, static_position.y), (58.0, 101.2));
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
                Component::text(" ", inline_style),
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
        assert_eq!(layout[5].0.x, 0.0);
        assert_eq!(layout[5].0.y, 19.2);
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
    fn anonymous_inline_line_items_preserve_negative_margin_wrapping() {
        use w3cos_dom::{Document, stylesheet};

        fn image(document: &mut Document, width_class: &str, margin_class: Option<&str>) -> w3cos_dom::Element {
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
                vec![Component::text("There should be one line.", text_style.clone())],
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
    fn inline_table_with_direct_inline_content_uses_the_full_row_width() {
        let inline_table = Component::row(
            Style {
                display: WDisp::InlineTable,
                ..Style::default()
            },
            vec![
                Component::text("1", Style::default()),
                Component::text("Before inline-table", Style::default()),
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
