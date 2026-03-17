use anyhow::Result;
use taffy::prelude::*;
use w3cos_std::style::{
    AlignItems as WAlign, Dimension as WDim, Display as WDisplay, FlexDirection as WDir,
    FlexWrap as WWrap, JustifyContent as WJustify, Overflow as WOverflow, Position as WPos,
};
use w3cos_std::{Component, ComponentKind};

#[derive(Debug, Clone, Copy)]
pub struct LayoutRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

pub fn compute(
    root: &Component,
    viewport_w: f32,
    viewport_h: f32,
) -> Result<Vec<(LayoutRect, usize)>> {
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
    collect_layouts(&tree, root_node, 0.0, 0.0, &mut results);
    Ok(results)
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

fn collect_layouts(
    tree: &TaffyTree<usize>,
    node: NodeId,
    parent_x: f32,
    parent_y: f32,
    out: &mut Vec<(LayoutRect, usize)>,
) {
    let layout = tree.layout(node).unwrap();
    let x = parent_x + layout.location.x;
    let y = parent_y + layout.location.y;
    let rect = LayoutRect {
        x,
        y,
        width: layout.size.width,
        height: layout.size.height,
    };

    if let Some(ctx) = tree.get_node_context(node) {
        out.push((rect, *ctx));
    }

    for &child in tree.children(node).unwrap().iter() {
        collect_layouts(tree, child, x, y, out);
    }
}

fn to_taffy_style(s: &w3cos_std::style::Style) -> Style {
    Style {
        display: match s.display {
            WDisplay::Flex => taffy::Display::Flex,
            WDisplay::Grid => taffy::Display::Grid,
            WDisplay::Block | WDisplay::Inline | WDisplay::InlineBlock => taffy::Display::Block,
            WDisplay::None => taffy::Display::None,
        },
        position: match s.position {
            WPos::Relative => taffy::Position::Relative,
            WPos::Absolute | WPos::Fixed | WPos::Sticky => taffy::Position::Absolute,
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
        flex_grow: s.flex_grow,
        flex_shrink: s.flex_shrink,
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
        size: Size {
            width: to_taffy_dim(s.width),
            height: to_taffy_dim(s.height),
        },
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
