use anyhow::Result;
use taffy::prelude::*;
use w3cos_std::component::EventAction;
use w3cos_std::style::{
    AlignItems as WAlign, Dimension as WDim, Display as WDisplay, FlexDirection as WDir,
    FlexWrap as WWrap, JustifyContent as WJustify, Overflow as WOverflow, Position as WPos,
};
use w3cos_std::{Component, ComponentKind};

const ROOT_FONT_SIZE: f32 = 16.0;

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
        self.tree.compute_layout(
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

#[cfg(test)]
mod tests {
    use super::*;
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
        assert!(l[0].0.width > 32.0);
        assert!(l[0].0.height > 16.0);
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
}
