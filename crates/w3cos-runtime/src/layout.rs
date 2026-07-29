use anyhow::Result;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::OnceLock;
use taffy::prelude::*;
use w3cos_std::component::EventAction;
use w3cos_std::style::{
    AlignItems as WAlign, AlignSelf as WAlignSelf, BoxSizing as WBoxSizing, Dimension as WDim,
    Display as WDisplay, FlexDirection as WDir, FlexWrap as WWrap, JustifyContent as WJustify,
    Overflow as WOverflow, Position as WPos, Spacing as WSpacing, WhiteSpace as WWhiteSpace,
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

#[derive(Debug, Clone, Copy)]
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
                .unwrap_or((200.0, 200.0));
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

fn dim_to_px(dim: WDim) -> Option<f32> {
    match dim {
        WDim::Px(v) => Some(v),
        WDim::Percent(p) => Some(p),
        WDim::Auto | WDim::Rem(_) | WDim::Em(_) | WDim::Vw(_) | WDim::Vh(_) => None,
    }
}

pub(crate) fn text_intrinsic_size(content: &str, style: &w3cos_std::style::Style) -> (f32, f32) {
    let registry = crate::font_face::FontRegistry::global();
    let font_runs = registry.resolve_style_runs(style, content);
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

    let measured = if has_registered_runs {
        cascade_text_intrinsic_size(content, style, DEFAULT_TEXT_WRAP_WIDTH)
    } else if matches!(style.display, WDisplay::InlineBlock) {
        #[cfg(feature = "skia")]
        {
            crate::render_skia::measure_skia_text_intrinsic_size(content, style)
        }
        #[cfg(not(feature = "skia"))]
        {
            text_layout::text_intrinsic_size_font(
                content,
                style,
                DEFAULT_TEXT_WRAP_WIDTH,
                layout_font(),
            )
        }
    } else {
        text_layout::text_intrinsic_size_font(
            content,
            style,
            DEFAULT_TEXT_WRAP_WIDTH,
            layout_font(),
        )
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
    let height = if lines.len() == 1 {
        cascade_line_height(&lines[0], style)
    } else {
        lines.len() as f32 * style.font_size * style.line_height
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

fn browser_normal_cjk_height(
    content: &str,
    style: &w3cos_std::style::Style,
    parent_display: Option<WDisplay>,
) -> Option<f32> {
    if (matches!(style.display, WDisplay::InlineBlock)
        || matches!(parent_display, Some(WDisplay::InlineBlock)))
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

    let measured = if font_runs.iter().any(|run| run.font.is_some()) {
        cascade_text_intrinsic_size(content, style, width).1
    } else {
        text_layout::wrapped_block_height_font(content, width, style, layout_font())
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
    let width = if matches!(style.width, WDim::Auto) {
        match kind {
            ComponentKind::TextInput { .. } => {
                Dimension::length(leaf_intrinsic_size(kind, style).0)
            }
            _ => Dimension::auto(),
        }
    } else {
        base.size.width
    };
    let height = if matches!(style.height, WDim::Auto) {
        let h = match kind {
            ComponentKind::Text { content } => {
                text_intrinsic_size_in_parent(content, style, parent_display).1
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
    old.iter().zip(new.iter()).all(|(old, new)| {
        let mut old_style = old.style.clone();
        let mut new_style = new.style.clone();
        old_style.display = WDisplay::Flex;
        new_style.display = WDisplay::Flex;
        old_style == new_style
    })
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
            )?);
            self.tree_valid = true;
        }

        let root_node = self.root_node.unwrap();
        let space = Size {
            width: AvailableSpace::Definite(viewport_w),
            height: AvailableSpace::Definite(viewport_h),
        };
        self.tree.compute_layout(root_node, space)?;
        update_text_leaf_heights(&mut self.tree, root_node, flat, None)?;
        self.tree.compute_layout(root_node, space)?;

        let mut results = Vec::new();
        let mut fixed_results = Vec::new();
        let mut scrollable = Vec::new();
        let mut clip_only = Vec::new();
        let mut scroll_ancestor = vec![None; flat.len()];

        collect_layouts_fast(
            flat,
            &self.tree,
            root_node,
            0.0,
            0.0,
            viewport_w,
            viewport_h,
            None,
            &mut results,
            &mut fixed_results,
            &mut scrollable,
            &mut clip_only,
            &mut scroll_ancestor,
        );

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
    )?;
    tree.compute_layout(
        root_node,
        Size {
            width: AvailableSpace::Definite(viewport_w),
            height: AvailableSpace::Definite(viewport_h),
        },
    )?;
    update_text_leaf_heights(&mut tree, root_node, &flat, None)?;
    tree.compute_layout(
        root_node,
        Size {
            width: AvailableSpace::Definite(viewport_w),
            height: AvailableSpace::Definite(viewport_h),
        },
    )?;

    let mut results = Vec::new();
    let mut fixed_results = Vec::new();
    let mut scrollable = Vec::new();
    let mut clip_only = Vec::new();
    let mut scroll_ancestor = vec![None; flat.len()];

    collect_layouts_fast(
        &flat,
        &tree,
        root_node,
        0.0,
        0.0,
        viewport_w,
        viewport_h,
        None,
        &mut results,
        &mut fixed_results,
        &mut scrollable,
        &mut clip_only,
        &mut scroll_ancestor,
    );

    results.extend(fixed_results);
    Ok((results, scrollable, clip_only))
}

// ---------------------------------------------------------------------------
// Internal: Taffy tree construction
// ---------------------------------------------------------------------------

fn build_taffy_tree(
    tree: &mut TaffyTree<usize>,
    comp: &Component,
    idx: &mut usize,
    parent_direction: Option<WDir>,
    parent_display: Option<WDisplay>,
    parent_align_items: Option<WAlign>,
    viewport_w: f32,
    viewport_h: f32,
) -> Result<NodeId, taffy::TaffyError> {
    let my_idx = *idx;
    *idx += 1;

    let style = to_taffy_style(&comp.style, viewport_w, viewport_h);

    if comp.children.is_empty() {
        let size = leaf_taffy_size(&comp.kind, &comp.style, &style, parent_display);
        let inline_control_in_block = matches!(comp.style.display, WDisplay::InlineBlock)
            && !matches!(parent_display, Some(WDisplay::Flex | WDisplay::Grid));
        let (min_w, size_w) = if matches!(comp.style.width, WDim::Auto) {
            match &comp.kind {
                ComponentKind::Text { content } => {
                    let nowrap = matches!(
                        comp.style.white_space,
                        WWhiteSpace::NoWrap | WWhiteSpace::Pre
                    );
                    let shrink_to_fit =
                        matches!(comp.style.display, WDisplay::Inline | WDisplay::InlineBlock);
                    if nowrap || shrink_to_fit {
                        let mut w = text_intrinsic_size(content, &comp.style).0;
                        if let WDim::Px(mw) = comp.style.min_width {
                            w = w.max(mw);
                        }
                        let dim = Dimension::length(w);
                        (dim, dim)
                    } else if text_uses_intrinsic_cross_size(
                        &comp.style,
                        parent_direction,
                        parent_display,
                        parent_align_items,
                    ) {
                        let mut w = text_intrinsic_size(content, &comp.style).0;
                        if let WDim::Px(mw) = comp.style.min_width {
                            w = w.max(mw);
                        }
                        (Dimension::length(w), Dimension::auto())
                    } else if matches!(parent_direction, Some(WDir::Column | WDir::ColumnReverse)) {
                        let min_width = match comp.style.min_width {
                            WDim::Px(mw) => Dimension::length(mw),
                            _ => Dimension::length(0.0),
                        };
                        (min_width, Dimension::auto())
                    } else {
                        let mut w = text_intrinsic_size(content, &comp.style).0;
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
                _ => (Dimension::auto(), size.width),
            }
        } else {
            (Dimension::auto(), size.width)
        };
        let min_h = if matches!(comp.style.height, WDim::Auto) {
            match &comp.kind {
                ComponentKind::Text { content } => Dimension::length(
                    text_intrinsic_size_in_parent(content, &comp.style, parent_display).1,
                ),
                ComponentKind::Button { label } => {
                    Dimension::length(button_intrinsic_size(label, &comp.style).1)
                }
                _ => Dimension::auto(),
            }
        } else {
            Dimension::auto()
        };

        let leaf_style = Style {
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
        tree.new_leaf_with_context(leaf_style, my_idx)
    } else {
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
                )?;
                Ok((c.style.order, source_index, node))
            })
            .collect::<Result<_, _>>()?;
        child_nodes.sort_by_key(|(order, source_index, _)| (*order, *source_index));
        let child_nodes: Vec<NodeId> = child_nodes.into_iter().map(|(_, _, node)| node).collect();
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
fn update_text_leaf_heights(
    tree: &mut TaffyTree<usize>,
    node: NodeId,
    flat: &[FlatNodeInfo<'_>],
    parent_display: Option<WDisplay>,
) -> Result<(), taffy::TaffyError> {
    let layout = tree.layout(node)?;
    let node_width = layout.size.width;

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
                    let mut taffy_style = tree.style(node)?.clone();
                    let measured_height = Dimension::length(h);
                    if taffy_style.min_size.height != measured_height
                        || taffy_style.size.height != measured_height
                    {
                        taffy_style.min_size.height = measured_height;
                        taffy_style.size.height = measured_height;
                        tree.set_style(node, taffy_style)?;
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
        update_text_leaf_heights(tree, child, flat, current_display)?;
    }
    Ok(())
}

fn to_taffy_display(d: WDisplay) -> taffy::Display {
    match d {
        WDisplay::Flex | WDisplay::Inline | WDisplay::InlineBlock => taffy::Display::Flex,
        WDisplay::Grid => taffy::Display::Grid,
        WDisplay::Block => taffy::Display::Block,
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

    if let Some(&ctx) = tree.get_node_context(node) {
        if ctx < flat.len() {
            if !is_node_visible(flat, ctx) {
                return;
            }
            let info = &flat[ctx];

            scroll_ancestor[ctx] = current_scroll_container;

            if matches!(info.style.position, WPos::Fixed) {
                rect =
                    compute_fixed_rect(info.style, viewport_w, viewport_h, rect.width, rect.height);
                fixed_out.push((rect, ctx));
            } else {
                out.push((rect, ctx));
            }

            let overflow_x = info.style.resolved_overflow_x();
            let overflow_y = info.style.resolved_overflow_y();
            let scrolls_x = matches!(overflow_x, WOverflow::Scroll | WOverflow::Auto);
            let scrolls_y = matches!(overflow_y, WOverflow::Scroll | WOverflow::Auto);
            if scrolls_x || scrolls_y {
                let max_x = if scrolls_x {
                    layout.scroll_width().max(0.0)
                } else {
                    0.0
                };
                let max_y = if scrolls_y {
                    match info.kind {
                        ComponentKind::VirtualList { total_extent, .. } => {
                            (*total_extent - rect.height).max(0.0)
                        }
                        _ => layout.scroll_height().max(0.0),
                    }
                } else {
                    0.0
                };
                if max_x > 0.0 || max_y > 0.0 {
                    scrollable.push((ctx, rect, ScrollExtent { max_x, max_y }));
                } else {
                    clip_only.push((ctx, rect));
                }
                new_scroll_container = Some(ctx);
            } else if matches!(overflow_x, WOverflow::Hidden)
                || matches!(overflow_y, WOverflow::Hidden)
            {
                clip_only.push((ctx, rect));
                new_scroll_container = Some(ctx);
            }
        }
    }

    for &child in tree.children(node).unwrap().iter() {
        collect_layouts_fast(
            flat,
            tree,
            child,
            x,
            y,
            viewport_w,
            viewport_h,
            new_scroll_container,
            out,
            fixed_out,
            scrollable,
            clip_only,
            scroll_ancestor,
        );
    }
}

fn compute_fixed_rect(
    style: &w3cos_std::style::Style,
    viewport_w: f32,
    viewport_h: f32,
    width: f32,
    height: f32,
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

    let x = match (left, right) {
        (Some(l), _) => l,
        (None, Some(r)) => viewport_w - r - width,
        (None, None) => 0.0,
    };
    let y = match (top, bottom) {
        (Some(t), _) => t,
        (None, Some(b)) => viewport_h - b - height,
        (None, None) => 0.0,
    };

    LayoutRect {
        x,
        y,
        width,
        height,
    }
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
        flex_direction: match s.flex_direction {
            WDir::Row => FlexDirection::Row,
            WDir::Column => FlexDirection::Column,
            WDir::RowReverse => FlexDirection::RowReverse,
            WDir::ColumnReverse => FlexDirection::ColumnReverse,
        },
        justify_content: Some(match s.justify_content {
            WJustify::FlexStart => JustifyContent::FlexStart,
            WJustify::FlexEnd => JustifyContent::FlexEnd,
            WJustify::Center => JustifyContent::Center,
            WJustify::SpaceBetween => JustifyContent::SpaceBetween,
            WJustify::SpaceAround => JustifyContent::SpaceAround,
            WJustify::SpaceEvenly => JustifyContent::SpaceEvenly,
        }),
        align_items: Some(match s.align_items {
            WAlign::FlexStart => AlignItems::FlexStart,
            WAlign::FlexEnd => AlignItems::FlexEnd,
            WAlign::Center => AlignItems::Center,
            WAlign::Stretch => AlignItems::Stretch,
            WAlign::Baseline => AlignItems::Baseline,
        }),
        align_self: to_taffy_align_self(s.align_self),
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
            top: to_taffy_auto(s.top, s.font_size, viewport_w, viewport_h),
            right: to_taffy_auto(s.right, s.font_size, viewport_w, viewport_h),
            bottom: to_taffy_auto(s.bottom, s.font_size, viewport_w, viewport_h),
            left: to_taffy_auto(s.left, s.font_size, viewport_w, viewport_h),
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
        margin: Rect {
            top: to_taffy_margin(s.margin.top, s.font_size, viewport_w, viewport_h),
            right: to_taffy_margin(s.margin.right, s.font_size, viewport_w, viewport_h),
            bottom: to_taffy_margin(s.margin.bottom, s.font_size, viewport_w, viewport_h),
            left: to_taffy_margin(s.margin.left, s.font_size, viewport_w, viewport_h),
        },
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
        crate::image_loader::invalidate(source);
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
    fn inline_block_text_shrink_wraps_content_and_padding() {
        let layout = compute(
            &Component::boxed(
                Style {
                    display: WDisp::Block,
                    width: WDim::Px(300.0),
                    ..Style::default()
                },
                vec![Component::text(
                    "首次入驻",
                    Style {
                        display: WDisp::InlineBlock,
                        box_sizing: WBoxSizing::BorderBox,
                        font_size: 13.0,
                        line_height: 1.3,
                        padding: w3cos_std::style::Edges::xy(10.0, 4.0),
                        ..Style::default()
                    },
                )],
            ),
            300.0,
            200.0,
        )
        .unwrap();
        let badge = layout[1].0;
        assert!(
            (badge.width - 72.0).abs() < 1.0,
            "inline-block width should equal four CJK ems plus padding, got {}",
            badge.width
        );
        assert!(
            (badge.height - 24.9).abs() < 1.0,
            "inline-block height should equal line box plus padding, got {}",
            badge.height
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
