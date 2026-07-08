use anyhow::Result;
use std::sync::OnceLock;
use taffy::prelude::*;
use w3cos_std::component::EventAction;
use w3cos_std::style::{
    AlignItems as WAlign, Dimension as WDim, Display as WDisplay, FlexDirection as WDir,
    FlexWrap as WWrap, JustifyContent as WJustify, Overflow as WOverflow, Position as WPos,
    WhiteSpace as WWhiteSpace,
};
use w3cos_std::{Component, ComponentKind};

use crate::text_layout;

const ROOT_FONT_SIZE: f32 = 16.0;
/// Typical mobile content width for pre-wrap intrinsic sizing.
const DEFAULT_TEXT_WRAP_WIDTH: f32 = 360.0;

static LAYOUT_FONT: OnceLock<fontdue::Font> = OnceLock::new();

fn layout_font() -> &'static fontdue::Font {
    LAYOUT_FONT.get_or_init(|| {
        let data = include_bytes!("../assets/CJK-Subset.ttf");
        fontdue::Font::from_bytes(data as &[u8], fontdue::FontSettings::default())
            .expect("embedded layout font")
    })
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
    pub kind: &'a ComponentKind,
    pub style: &'a w3cos_std::style::Style,
    pub on_click: &'a EventAction,
    pub parent: Option<usize>,
}

pub fn pre_flatten(root: &Component) -> Vec<FlatNodeInfo<'_>> {
    let n = count_nodes(root);
    let mut out = Vec::with_capacity(n);
    pre_flatten_recursive(root, None, &mut out);
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
        ComponentKind::Image { .. } => {
            let w = if matches!(style.width, WDim::Auto) {
                200.0
            } else {
                dim_to_px(style.width).unwrap_or(200.0)
            };
            let h = if matches!(style.height, WDim::Auto) {
                200.0
            } else {
                dim_to_px(style.height).unwrap_or(200.0)
            };
            (w, h)
        }
        ComponentKind::TextInput { .. } => {
            let w = dim_to_px(style.width).unwrap_or(200.0);
            let h = dim_to_px(style.height).unwrap_or(40.0);
            (w, h)
        }
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

fn text_intrinsic_size(content: &str, style: &w3cos_std::style::Style) -> (f32, f32) {
    text_layout::text_intrinsic_size_font(
        content,
        style,
        DEFAULT_TEXT_WRAP_WIDTH,
        layout_font(),
    )
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
) -> taffy::Size<Dimension> {
    let width = if matches!(style.width, WDim::Auto) {
        Dimension::auto()
    } else {
        base.size.width
    };
    let height = if matches!(style.height, WDim::Auto) {
        let h = match kind {
            ComponentKind::Text { content } => text_intrinsic_size(content, style).1,
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
        (ComponentKind::Text { .. }, ComponentKind::Text { .. }) => true,
        (ComponentKind::Button { label: la }, ComponentKind::Button { label: lb }) => la == lb,
        (ComponentKind::Image { src: sa }, ComponentKind::Image { src: sb }) => sa == sb,
        (
            ComponentKind::TextInput {
                value: va,
                placeholder: pa,
            },
            ComponentKind::TextInput {
                value: vb,
                placeholder: pb,
            },
        ) => va == vb && pa == pb,
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
    out: &mut Vec<FlatNodeInfo<'a>>,
) {
    let my_idx = out.len();
    out.push(FlatNodeInfo {
        kind: &comp.kind,
        style: &comp.style,
        on_click: &comp.on_click,
        parent,
    });
    for child in &comp.children {
        pre_flatten_recursive(child, Some(my_idx), out);
    }
}

// ---------------------------------------------------------------------------
// LayoutEngine — persistent TaffyTree for incremental layout
// ---------------------------------------------------------------------------

pub struct LayoutEngine {
    tree: TaffyTree<usize>,
    root_node: Option<taffy::NodeId>,
    tree_valid: bool,
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
        Self {
            tree: TaffyTree::new(),
            root_node: None,
            tree_valid: false,
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
        if !self.tree_valid {
            self.tree.clear();
            let mut idx = 0;
            self.root_node = Some(build_taffy_tree(&mut self.tree, root, &mut idx)?);
            self.tree_valid = true;
        }

        let root_node = self.root_node.unwrap();
        let space = Size {
            width: AvailableSpace::Definite(viewport_w),
            height: AvailableSpace::Definite(viewport_h),
        };
        self.tree.compute_layout(root_node, space)?;
        update_text_leaf_heights(&mut self.tree, root_node, flat)?;
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
    let mut node_index: usize = 0;

    let root_node = build_taffy_tree(&mut tree, root, &mut node_index)?;
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
) -> Result<NodeId, taffy::TaffyError> {
    let my_idx = *idx;
    *idx += 1;

    let style = to_taffy_style(&comp.style);

    if comp.children.is_empty() {
        let size = leaf_taffy_size(&comp.kind, &comp.style, &style);
        let (min_w, size_w) = if matches!(comp.style.width, WDim::Auto) {
            match &comp.kind {
                ComponentKind::Text { content } => {
                    let mut w = text_intrinsic_size(content, &comp.style).0;
                    if let WDim::Px(mw) = comp.style.min_width {
                        w = w.max(mw);
                    }
                    let dim = Dimension::length(w);
                    let nowrap = matches!(
                        comp.style.white_space,
                        WWhiteSpace::NoWrap | WWhiteSpace::Pre
                    );
                    if nowrap {
                        (dim, dim)
                    } else {
                        (dim, Dimension::auto())
                    }
                }
                ComponentKind::Button { label } => {
                    let w = button_intrinsic_size(label, &comp.style).0;
                    (Dimension::length(w), Dimension::auto())
                }
                _ => (Dimension::auto(), size.width),
            }
        } else {
            (Dimension::auto(), size.width)
        };
        let min_h = if matches!(comp.style.height, WDim::Auto) {
            match &comp.kind {
                ComponentKind::Text { content } => {
                    Dimension::length(text_intrinsic_size(content, &comp.style).1)
                }
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
        let child_nodes: Vec<NodeId> = comp
            .children
            .iter()
            .map(|c| build_taffy_tree(tree, c, idx))
            .collect::<Result<_, _>>()?;
        let node = tree.new_with_children(style, &child_nodes)?;
        tree.set_node_context(node, Some(my_idx))?;
        Ok(node)
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
) -> Result<(), taffy::TaffyError> {
    let layout = tree.layout(node)?;
    let node_width = layout.size.width;

    if let Some(idx) = tree.get_node_context(node).copied() {
        if idx < flat.len() {
            if let ComponentKind::Text { content } = flat[idx].kind {
                let style = flat[idx].style;
                let h = text_layout::wrapped_block_height_font(
                    content,
                    node_width,
                    style,
                    layout_font(),
                );
                let mut taffy_style = tree.style(node)?.clone();
                taffy_style.min_size.height = Dimension::length(h);
                taffy_style.size.height = Dimension::length(h);
                tree.set_style(node, taffy_style)?;
            }
        }
    }

    for child in tree.children(node)? {
        update_text_leaf_heights(tree, child, flat)?;
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
                rect = compute_fixed_rect(
                    info.style,
                    viewport_w,
                    viewport_h,
                    rect.width,
                    rect.height,
                );
                fixed_out.push((rect, ctx));
            } else {
                out.push((rect, ctx));
            }

            match info.style.overflow {
                WOverflow::Scroll | WOverflow::Auto => {
                    let max_x = layout.scroll_width().max(0.0);
                    let max_y = layout.scroll_height().max(0.0);
                    if max_x > 0.0 || max_y > 0.0 {
                        scrollable.push((ctx, rect, ScrollExtent { max_x, max_y }));
                    } else {
                        clip_only.push((ctx, rect));
                    }
                    new_scroll_container = Some(ctx);
                }
                WOverflow::Hidden => {
                    clip_only.push((ctx, rect));
                    new_scroll_container = Some(ctx);
                }
                WOverflow::Visible => {}
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

fn to_taffy_style(s: &w3cos_std::style::Style) -> Style {
    let pad = s.padding_lengths();
    let mar = s.margin_lengths();
    let (display, flex_grow, flex_shrink, size) = match s.display {
        WDisplay::Flex => (
            taffy::Display::Flex,
            s.flex_grow,
            s.flex_shrink,
            Size {
                width: to_taffy_dim(s.width),
                height: to_taffy_dim(s.height),
            },
        ),
        WDisplay::Grid => (
            taffy::Display::Grid,
            s.flex_grow,
            s.flex_shrink,
            Size {
                width: to_taffy_dim(s.width),
                height: to_taffy_dim(s.height),
            },
        ),
        WDisplay::Block => (
            taffy::Display::Block,
            s.flex_grow,
            s.flex_shrink,
            Size {
                width: to_taffy_dim(s.width),
                height: to_taffy_dim(s.height),
            },
        ),
        WDisplay::Inline => (
            taffy::Display::Flex,
            0.0,
            1.0,
            Size {
                width: Dimension::auto(),
                height: Dimension::auto(),
            },
        ),
        WDisplay::InlineBlock => (
            taffy::Display::Flex,
            0.0,
            0.0,
            Size {
                width: to_taffy_dim(s.width),
                height: to_taffy_dim(s.height),
            },
        ),
        WDisplay::None => (
            taffy::Display::None,
            s.flex_grow,
            s.flex_shrink,
            Size {
                width: to_taffy_dim(s.width),
                height: to_taffy_dim(s.height),
            },
        ),
    };

    Style {
        display,
        position: match s.position {
            WPos::Relative | WPos::Sticky => taffy::Position::Relative,
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
        flex_wrap: match s.flex_wrap {
            WWrap::NoWrap => FlexWrap::NoWrap,
            WWrap::Wrap => FlexWrap::Wrap,
            WWrap::WrapReverse => FlexWrap::WrapReverse,
        },
        flex_grow,
        flex_shrink,
        inset: Rect {
            top: to_taffy_auto(s.top),
            right: to_taffy_auto(s.right),
            bottom: to_taffy_auto(s.bottom),
            left: to_taffy_auto(s.left),
        },
        gap: Size {
            width: LengthPercentage::length(s.gap),
            height: LengthPercentage::length(s.gap),
        },
        padding: Rect {
            top: LengthPercentage::length(pad.top + s.border_width),
            right: LengthPercentage::length(pad.right + s.border_width),
            bottom: LengthPercentage::length(pad.bottom + s.border_width),
            left: LengthPercentage::length(pad.left + s.border_width),
        },
        margin: Rect {
            top: LengthPercentageAuto::length(mar.top),
            right: LengthPercentageAuto::length(mar.right),
            bottom: LengthPercentageAuto::length(mar.bottom),
            left: LengthPercentageAuto::length(mar.left),
        },
        overflow: taffy::Point {
            x: to_taffy_overflow(s.overflow),
            y: to_taffy_overflow(s.overflow),
        },
        size,
        min_size: Size {
            width: to_taffy_dim(s.min_width),
            height: to_taffy_dim(s.min_height),
        },
        max_size: Size {
            width: to_taffy_dim(s.max_width),
            height: to_taffy_dim(s.max_height),
        },
        ..Style::DEFAULT
    }
}

fn to_taffy_dim(d: WDim) -> Dimension {
    match d {
        WDim::Auto => Dimension::auto(),
        WDim::Px(v) => Dimension::length(v),
        WDim::Percent(v) => Dimension::percent(v / 100.0),
        WDim::Rem(v) => Dimension::length(v * 16.0),
        WDim::Em(v) => Dimension::length(v * 16.0),
        WDim::Vw(v) => Dimension::percent(v / 100.0),
        WDim::Vh(v) => Dimension::percent(v / 100.0),
    }
}

fn to_taffy_auto(d: WDim) -> LengthPercentageAuto {
    match d {
        WDim::Auto => LengthPercentageAuto::auto(),
        WDim::Px(v) => LengthPercentageAuto::length(v),
        WDim::Percent(v) => LengthPercentageAuto::percent(v / 100.0),
        WDim::Rem(v) => LengthPercentageAuto::length(v * 16.0),
        WDim::Em(v) => LengthPercentageAuto::length(v * 16.0),
        WDim::Vw(v) => LengthPercentageAuto::percent(v / 100.0),
        WDim::Vh(v) => LengthPercentageAuto::percent(v / 100.0),
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
    use super::*;
    use w3cos_std::color::Color;
    use w3cos_std::component::Component;
    use w3cos_std::style::{Dimension as WDim, Display as WDisp, FlexDirection as WDir, Style};

    fn s() -> Style {
        Style::default()
    }

    fn col() -> Style {
        Style {
            display: WDisp::Flex,
            flex_direction: WDir::Column,
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
    fn root_at_origin() {
        let l = compute(&Component::text("R", s()), 800.0, 600.0).unwrap();
        assert_eq!(l[0].0.x, 0.0);
        assert_eq!(l[0].0.y, 0.0);
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
        let gap_visible = visible[3].0.y - (visible[1].0.y + visible[1].0.height);
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
    fn column_stretch_fills_viewport_width() {
        let l = compute(
            &Component::column(
                Style {
                    display: WDisp::Flex,
                    flex_direction: WDir::Column,
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
                    vec![Component::button("GET httpbin.org/get", Style {
                        padding: w3cos_std::style::Edges::all(14.0),
                        font_size: 14.0,
                        ..Style::default()
                    })],
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
        assert!(layout_shape_unchanged(
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
            vec![
                Component::text("9", s()),
                Component::button("Tap", s()),
            ],
        );
        let b = Component::column(
            col(),
            vec![
                Component::text("1000", s()),
                Component::button("Tap", s()),
            ],
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
        let tree = Component::column(
            wrap,
            vec![Component::text("hidden", Style::default())],
        );
        let flat = pre_flatten(&tree);
        assert!(!is_node_visible(&flat, 1));
        assert!(!is_node_visible(&flat, 0));
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
