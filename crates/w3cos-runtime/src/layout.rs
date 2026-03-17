use anyhow::Result;
use taffy::prelude::*;
use w3cos_std::style::{
    AlignItems as WAlign, Dimension as WDim, Display as WDisplay, FlexDirection as WDir,
    FlexWrap as WWrap, JustifyContent as WJustify, Overflow as WOverflow, Position as WPos,
};
use w3cos_std::{Component, ComponentKind};

/// Default root font size for resolving dimension values.
const ROOT_FONT_SIZE: f32 = 16.0;

#[derive(Debug, Clone, Copy)]
pub struct LayoutRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Scroll extent for a scrollable node (max scroll offset in x and y).
#[derive(Debug, Clone, Copy)]
pub struct ScrollExtent {
    pub max_x: f32,
    pub max_y: f32,
}

pub fn compute(
    root: &Component,
    viewport_w: f32,
    viewport_h: f32,
) -> Result<Vec<(LayoutRect, usize)>> {
    let (results, _, _) = compute_with_scroll(root, viewport_w, viewport_h)?;
    Ok(results)
}

/// Compute layout and return both layout results and scrollable node info.
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
    collect_layouts(
        root,
        &tree,
        root_node,
        0.0,
        0.0,
        viewport_w,
        viewport_h,
        &mut results,
        &mut fixed_results,
        &mut scrollable,
        &mut clip_only,
    );
    // Fixed nodes render last (on top of everything else)
    results.extend(fixed_results);
    Ok((results, scrollable, clip_only))
}

fn build_taffy_tree(
    tree: &mut TaffyTree<usize>,
    comp: &Component,
    idx: &mut usize,
) -> Result<NodeId, taffy::TaffyError> {
    let my_idx = *idx;
    *idx += 1;

    let style = to_taffy_style(&comp.style);

    if comp.children.is_empty() {
        let size = match &comp.kind {
            ComponentKind::Text { content } => {
                let char_w = comp.style.font_size * 0.6;
                let w = content.len() as f32 * char_w;
                let h = comp.style.font_size * 1.4;
                Size {
                    width: Dimension::length(w),
                    height: Dimension::length(h),
                }
            }
            ComponentKind::Button { label } => {
                let char_w = comp.style.font_size * 0.6;
                let w = (label.len() as f32 * char_w) + 32.0;
                let h = comp.style.font_size * 1.4 + 16.0;
                Size {
                    width: Dimension::length(w),
                    height: Dimension::length(h),
                }
            }
            ComponentKind::Image { .. } => {
                // Default 200x200 if width/height are Auto; otherwise use style dimensions
                let w = if matches!(comp.style.width, WDim::Auto) {
                    Dimension::length(200.0)
                } else {
                    style.size.width
                };
                let h = if matches!(comp.style.height, WDim::Auto) {
                    Dimension::length(200.0)
                } else {
                    style.size.height
                };
                Size {
                    width: w,
                    height: h,
                }
            }
            ComponentKind::TextInput { .. } => {
                let w = if matches!(comp.style.width, WDim::Auto) {
                    Dimension::length(200.0)
                } else {
                    style.size.width
                };
                let h = if matches!(comp.style.height, WDim::Auto) {
                    Dimension::length(40.0)
                } else {
                    style.size.height
                };
                Size {
                    width: w,
                    height: h,
                }
            }
            _ => Size {
                width: style.size.width,
                height: style.size.height,
            },
        };

        let leaf_style = Style { size, ..style };
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

#[allow(clippy::too_many_arguments)]
fn collect_layouts(
    root: &Component,
    tree: &TaffyTree<usize>,
    node: NodeId,
    parent_x: f32,
    parent_y: f32,
    viewport_w: f32,
    viewport_h: f32,
    out: &mut Vec<(LayoutRect, usize)>,
    fixed_out: &mut Vec<(LayoutRect, usize)>,
    scrollable: &mut Vec<(usize, LayoutRect, ScrollExtent)>,
    clip_only: &mut Vec<(usize, LayoutRect)>,
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

    if let Some(ctx) = tree.get_node_context(node) {
        let position = get_position_at_index(root, *ctx);
        if matches!(position, WPos::Fixed) {
            rect = compute_fixed_rect(
                get_style_at_index(root, *ctx),
                viewport_w,
                viewport_h,
                rect.width,
                rect.height,
            );
            fixed_out.push((rect, *ctx));
        } else {
            out.push((rect, *ctx));
        }
        let overflow = get_overflow_at_index(root, *ctx);
        match overflow {
            WOverflow::Scroll | WOverflow::Auto => {
                let max_x = layout.scroll_width().max(0.0);
                let max_y = layout.scroll_height().max(0.0);
                if max_x > 0.0 || max_y > 0.0 {
                    scrollable.push((*ctx, rect, ScrollExtent { max_x, max_y }));
                } else {
                    clip_only.push((*ctx, rect));
                }
            }
            WOverflow::Hidden => clip_only.push((*ctx, rect)),
            WOverflow::Visible => {}
        }
    }

    for &child in tree.children(node).unwrap().iter() {
        collect_layouts(
            root, tree, child, x, y, viewport_w, viewport_h, out, fixed_out, scrollable, clip_only,
        );
    }
}

fn get_overflow_at_index(root: &Component, index: usize) -> WOverflow {
    get_overflow_recursive(root, index, &mut 0).unwrap_or(WOverflow::Visible)
}

fn get_position_at_index(root: &Component, index: usize) -> WPos {
    get_position_recursive(root, index, &mut 0).unwrap_or(WPos::Relative)
}

fn get_position_recursive(comp: &Component, target: usize, counter: &mut usize) -> Option<WPos> {
    let my_idx = *counter;
    *counter += 1;
    if my_idx == target {
        return Some(comp.style.position);
    }
    for child in &comp.children {
        if let Some(pos) = get_position_recursive(child, target, counter) {
            return Some(pos);
        }
    }
    None
}

fn get_style_at_index(root: &Component, index: usize) -> &w3cos_std::style::Style {
    get_style_recursive(root, index, &mut 0).unwrap_or(&root.style)
}

fn get_style_recursive<'a>(
    comp: &'a Component,
    target: usize,
    counter: &mut usize,
) -> Option<&'a w3cos_std::style::Style> {
    let my_idx = *counter;
    *counter += 1;
    if my_idx == target {
        return Some(&comp.style);
    }
    for child in &comp.children {
        if let Some(s) = get_style_recursive(child, target, counter) {
            return Some(s);
        }
    }
    None
}

/// Compute viewport-relative rect for position: fixed using top/right/bottom/left.
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

fn get_overflow_recursive(
    comp: &Component,
    target: usize,
    counter: &mut usize,
) -> Option<WOverflow> {
    let my_idx = *counter;
    *counter += 1;
    if my_idx == target {
        return Some(comp.style.overflow);
    }
    for child in &comp.children {
        if let Some(ov) = get_overflow_recursive(child, target, counter) {
            return Some(ov);
        }
    }
    None
}

fn to_taffy_style(s: &w3cos_std::style::Style) -> Style {
    // Inline/InlineBlock: Taffy 0.9 has no native inline support; approximate via Flex.
    // Inline: fit content (auto size), flex_grow=0, flex_shrink=1.
    // InlineBlock: respect explicit width/height, flex_grow=0, flex_shrink=0.
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
            top: LengthPercentage::length(s.padding.top),
            right: LengthPercentage::length(s.padding.right),
            bottom: LengthPercentage::length(s.padding.bottom),
            left: LengthPercentage::length(s.padding.left),
        },
        margin: Rect {
            top: LengthPercentageAuto::length(s.margin.top),
            right: LengthPercentageAuto::length(s.margin.right),
            bottom: LengthPercentageAuto::length(s.margin.bottom),
            left: LengthPercentageAuto::length(s.margin.left),
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
