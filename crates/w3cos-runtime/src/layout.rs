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
#[cfg(test)]
mod tests {
    use super::*;
    use w3cos_std::component::Component;
    use w3cos_std::style::{Dimension as WDim, Display as WDisp, FlexDirection as WDir, Style};

    fn s() -> Style { Style::default() }

    fn col() -> Style {
        Style { display: WDisp::Flex, flex_direction: WDir::Column, gap: 10.0,
            padding: w3cos_std::style::Edges::all(16.0),
            width: WDim::Px(400.0), height: WDim::Px(600.0), ..Style::default() }
    }

    fn row() -> Style {
        Style { display: WDisp::Flex, flex_direction: WDir::Row, gap: 8.0,
            width: WDim::Px(400.0), height: WDim::Px(100.0), ..Style::default() }
    }

    #[test] fn single_node_has_size() {
        let l = compute(&Component::text("Hi", s()), 800.0, 600.0).unwrap();
        assert_eq!(l.len(), 1);
        assert!(l[0].0.width > 0.0);
    }

    #[test] fn root_at_origin() {
        let l = compute(&Component::text("R", s()), 800.0, 600.0).unwrap();
        assert_eq!(l[0].0.x, 0.0);
        assert_eq!(l[0].0.y, 0.0);
    }

    #[test] fn column_stacks_vertically() {
        let l = compute(&Component::column(col(), vec![
            Component::text("A", s()), Component::text("B", s()),
        ]), 800.0, 600.0).unwrap();
        assert_eq!(l.len(), 3);
        assert!(l[2].0.y > l[1].0.y);
    }

    #[test] fn row_arranges_horizontally() {
        let l = compute(&Component::row(row(), vec![
            Component::text("A", s()), Component::text("B", s()),
        ]), 800.0, 600.0).unwrap();
        assert_eq!(l.len(), 3);
        assert!(l[2].0.x > l[1].0.x);
    }

    #[test] fn padding_offsets_child() {
        let l = compute(&Component::column(Style {
            display: WDisp::Flex, padding: w3cos_std::style::Edges::all(40.0),
            width: WDim::Px(400.0), height: WDim::Px(300.0), ..Style::default()
        }, vec![Component::text("X", s())]), 800.0, 600.0).unwrap();
        assert!(l[1].0.x >= 40.0);
        assert!(l[1].0.y >= 40.0);
    }

    #[test] fn empty_container_one_entry() {
        let l = compute(&Component::boxed(s(), vec![]), 800.0, 600.0).unwrap();
        assert_eq!(l.len(), 1);
    }

    #[test] fn deeply_nested_11_nodes() {
        let mut c = Component::text("D", s());
        for _ in 0..10 { c = Component::column(col(), vec![c]); }
        assert_eq!(compute(&c, 800.0, 600.0).unwrap().len(), 11);
    }

    #[test] fn button_has_minimum_size() {
        let l = compute(&Component::button("OK", s()), 800.0, 600.0).unwrap();
        assert!(l[0].0.width > 32.0);
        assert!(l[0].0.height > 16.0);
    }

    #[test] fn three_row_children_ordered_ltr() {
        let l = compute(&Component::row(
            Style { display: WDisp::Flex, flex_direction: WDir::Row, gap: 24.0,
                width: WDim::Px(600.0), height: WDim::Px(50.0), ..Style::default() },
            vec![Component::text("X",s()), Component::text("Y",s()), Component::text("Z",s())],
        ), 800.0, 600.0).unwrap();
        assert_eq!(l.len(), 4);
        assert!(l[1].0.x < l[2].0.x);
        assert!(l[2].0.x < l[3].0.x);
    }

    #[test] fn gap_vs_no_gap() {
        let ng = compute(&Component::column(
            Style { display: WDisp::Flex, flex_direction: WDir::Column, width: WDim::Px(400.0), height: WDim::Px(300.0), ..Style::default() },
            vec![Component::text("A",s()), Component::text("B",s())],
        ), 800.0, 600.0).unwrap();
        let wg = compute(&Component::column(
            Style { display: WDisp::Flex, flex_direction: WDir::Column, gap: 20.0, width: WDim::Px(400.0), height: WDim::Px(300.0), ..Style::default() },
            vec![Component::text("A",s()), Component::text("B",s())],
        ), 800.0, 600.0).unwrap();
        let d0 = ng[2].0.y - (ng[1].0.y + ng[1].0.height);
        let d1 = wg[2].0.y - (wg[1].0.y + wg[1].0.height);
        assert!(d1 >= d0);
    }

    #[test] fn mixed_text_button_children() {
        let l = compute(&Component::column(col(), vec![
            Component::text("T", s()), Component::button("B", s()),
        ]), 800.0, 600.0).unwrap();
        assert_eq!(l.len(), 3);
    }

    #[test] fn column_vs_row_axes_differ() {
        let cl = compute(&Component::column(col(), vec![
            Component::text("A",s()), Component::text("B",s()),
        ]), 800.0, 600.0).unwrap();
        let rl = compute(&Component::row(row(), vec![
            Component::text("A",s()), Component::text("B",s()),
        ]), 800.0, 600.0).unwrap();
        assert!((cl[2].0.y - cl[1].0.y).abs() > (cl[2].0.x - cl[1].0.x).abs());
        assert!((rl[2].0.x - rl[1].0.x).abs() > (rl[2].0.y - rl[1].0.y).abs());
    }

    #[test] fn zero_viewport() {
        assert_eq!(compute(&Component::text("Z", s()), 0.0, 0.0).unwrap().len(), 1);
    }

    #[test] fn narrow_viewport() {
        assert_eq!(compute(&Component::text("N", s()), 100.0, 100.0).unwrap().len(), 1);
    }

    #[test] fn rect_clone_debug() {
        let r = LayoutRect { x: 1.0, y: 2.0, width: 3.0, height: 4.0 };
        assert_eq!(r.x, r.clone().x);
        assert!(format!("{:?}", r).contains("LayoutRect"));
    }

    #[test] fn single_child_inside_parent() {
        let l = compute(&Component::column(col(), vec![Component::text("O", s())]), 800.0, 600.0).unwrap();
        assert!(l[1].0.x >= l[0].0.x);
        assert!(l[1].0.y >= l[0].0.y);
    }

    #[test] fn fixed_size_box_respected() {
        let l = compute(&Component::boxed(
            Style { width: WDim::Px(200.0), height: WDim::Px(100.0), ..Style::default() }, vec![],
        ), 800.0, 600.0).unwrap();
        assert!((l[0].0.width - 200.0).abs() < 2.0);
        assert!((l[0].0.height - 100.0).abs() < 2.0);
    }

    #[test] fn five_children_column() {
        let children: Vec<_> = (0..5).map(|i| Component::text(&i.to_string(), s())).collect();
        assert_eq!(compute(&Component::column(col(), children), 800.0, 600.0).unwrap().len(), 6);
    }

    #[test] fn text_width_scales_with_length() {
        let short = compute(&Component::text("A", s()), 800.0, 600.0).unwrap();
        let long = compute(&Component::text("A very long text string", s()), 800.0, 600.0).unwrap();
        assert!(long[0].0.width > short[0].0.width);
    }

    #[test] fn large_viewport() {
        let l = compute(&Component::text("Big", s()), 4000.0, 3000.0).unwrap();
        assert!(l[0].0.width > 0.0);
    }
}
