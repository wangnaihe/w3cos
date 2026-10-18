//! Block formatting context, stage S1 (no floats, no inline content).
//!
//! Ported in structure from Blink's NGBlockLayoutAlgorithm
//! (third_party/blink/renderer/core/layout/block_layout_algorithm.cc,
//! Copyright The Chromium Authors, BSD-style license): children are laid out
//! in order with a margin strut that is resolved when a non-adjoining edge
//! (border, padding, in-flow content, BFC root) is reached.  Width follows
//! CSS2 10.3.3, height 10.6.3, min/max 10.4/10.7, collapsing 8.3.1.

use super::{BoxFragment, ConstraintSpace, LayoutResult, MarginStrut};
use crate::layout::{FlatNodeInfo, resolve_spacing_for_layout};
use w3cos_std::ComponentKind;
use w3cos_std::style::{
    BoxSizing, Clear, Dimension, Display, Float, Overflow, Position, Spacing, Style, TextDirection,
};

fn supported_dimension(d: Dimension) -> bool {
    matches!(
        d,
        Dimension::Auto | Dimension::Px(_) | Dimension::Percent(_)
    )
}

fn supported_spacing(s: Spacing, allow_auto: bool) -> bool {
    match s {
        Spacing::Px(_) | Spacing::Percent(_) => true,
        Spacing::Auto => allow_auto,
        _ => false,
    }
}

fn supported_style(style: &Style) -> bool {
    matches!(style.display, Display::Block | Display::FlowRoot)
        && style.position == Position::Static
        && style.float == Float::None
        && style.clear == Clear::None
        && style.overflow == Overflow::Visible
        && style.overflow_x.is_none_or(|o| o == Overflow::Visible)
        && style.overflow_y.is_none_or(|o| o == Overflow::Visible)
        && style.column_count.is_none()
        && style.column_width == Dimension::Auto
        && style.direction == TextDirection::Ltr
        && [
            style.width,
            style.height,
            style.min_width,
            style.min_height,
            style.max_width,
            style.max_height,
        ]
        .into_iter()
        .all(supported_dimension)
        && [style.margin.top, style.margin.right, style.margin.bottom, style.margin.left]
            .into_iter()
            .all(|s| supported_spacing(s, true))
        && [style.padding.top, style.padding.right, style.padding.bottom, style.padding.left]
            .into_iter()
            .all(|s| supported_spacing(s, false))
        // Internal markers mean a legacy projection owns this box.
        && style.custom_properties.as_ref().is_none_or(|props| {
            !props.keys().any(|key| key.starts_with("--w3cos-internal"))
        })
}

/// True when every rendered node is a block box this stage lays out.
pub(super) fn supports_tree(flat: &[FlatNodeInfo]) -> bool {
    let mut index = 0;
    while index < flat.len() {
        let info = &flat[index];
        if info.style.display == Display::None {
            // Not rendered: the legacy engine emits nothing for the subtree.
            index = info.subtree_end.max(index + 1);
            continue;
        }
        if !matches!(
            info.kind,
            ComponentKind::Root | ComponentKind::Box | ComponentKind::Column | ComponentKind::Row
        ) || !supported_style(info.style)
        {
            return false;
        }
        index += 1;
    }
    true
}

fn border(style: &Style) -> [f32; 4] {
    [
        style.border_top_width.unwrap_or(style.border_width),
        style.border_right_width.unwrap_or(style.border_width),
        style.border_bottom_width.unwrap_or(style.border_width),
        style.border_left_width.unwrap_or(style.border_width),
    ]
}

/// Blink LayoutUnit: used lengths are stored in 1/64 px, truncating.
fn layout_unit(v: f32) -> f32 {
    w3cos_std::style::used_box_spacing(v)
}

fn resolve_dim(d: Dimension, basis: Option<f32>) -> Option<f32> {
    match d {
        Dimension::Px(v) => Some(v),
        Dimension::Percent(p) => basis.map(|b| layout_unit(b * p / 100.0)),
        _ => None,
    }
}

struct BoxMetrics {
    margin: [f32; 4],
    border: [f32; 4],
    padding: [f32; 4],
    content_width: f32,
}

/// CSS2 10.3.3 (+10.4) used width and horizontal margins.
fn resolve_inline(style: &Style, space: &ConstraintSpace) -> BoxMetrics {
    let cb = space.available_inline;
    let spacing =
        |s: Spacing| resolve_spacing_for_layout(s, cb, style, space.viewport_w, space.viewport_h);
    let border = border(style);
    let padding = [
        spacing(style.padding.top),
        spacing(style.padding.right),
        spacing(style.padding.bottom),
        spacing(style.padding.left),
    ];
    let mut margin = [
        spacing(style.margin.top),
        spacing(style.margin.right),
        spacing(style.margin.bottom),
        spacing(style.margin.left),
    ];
    let edges = border[1] + border[3] + padding[1] + padding[3];
    let to_content = |w: f32| match style.box_sizing {
        BoxSizing::BorderBox => (w - edges).max(0.0),
        BoxSizing::ContentBox => w,
    };
    let specified = resolve_dim(style.width, Some(cb)).map(to_content);
    let mut content_width = specified
        .unwrap_or(cb - margin[1] - margin[3] - edges)
        .max(0.0);
    if let Some(max) = resolve_dim(style.max_width, Some(cb)).map(to_content) {
        content_width = content_width.min(max);
    }
    if let Some(min) = resolve_dim(style.min_width, Some(cb)).map(to_content) {
        content_width = content_width.max(min);
    }
    let auto_left = style.margin.left == Spacing::Auto;
    let auto_right = style.margin.right == Spacing::Auto;
    let width_was_auto =
        specified.is_none() && content_width == (cb - margin[1] - margin[3] - edges).max(0.0);
    if !width_was_auto {
        let remaining = cb - content_width - edges;
        match (auto_left, auto_right) {
            (true, true) => {
                let half = layout_unit((remaining / 2.0).max(0.0));
                margin[3] = half;
                margin[1] = half;
            }
            (true, false) => margin[3] = remaining - margin[1],
            (false, true) => margin[1] = remaining - margin[3],
            // Over-constrained, direction ltr: margin-right is ignored.
            (false, false) => margin[1] = remaining - margin[3],
        }
    }
    BoxMetrics {
        margin,
        border,
        padding,
        content_width,
    }
}

fn is_bfc_root(style: &Style) -> bool {
    style.display == Display::FlowRoot
}

/// Lay out the root box inside the initial containing block.  Root margins
/// never collapse (CSS2 8.3.1).  Returns the root's border-box offset.
pub(super) fn layout_root(
    flat: &[FlatNodeInfo],
    viewport_w: f32,
    viewport_h: f32,
) -> (f32, f32, BoxFragment) {
    let space = ConstraintSpace {
        available_inline: viewport_w,
        percentage_block: Some(viewport_h),
        viewport_w,
        viewport_h,
    };
    let result = layout_block(flat, 0, &space, true);
    let metrics = resolve_inline(flat[0].style, &space);
    (metrics.margin[3], metrics.margin[0], result.fragment)
}

pub(super) fn layout_block(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    force_bfc_root: bool,
) -> LayoutResult {
    let info = &flat[index];
    let style = info.style;
    let metrics = resolve_inline(style, space);
    let [bt, br, bb, bl] = metrics.border;
    let [pt, pr, pb, pl] = metrics.padding;
    let bfc_root = force_bfc_root || is_bfc_root(style);

    let to_content_h = |h: f32| match style.box_sizing {
        BoxSizing::BorderBox => (h - bt - bb - pt - pb).max(0.0),
        BoxSizing::ContentBox => h,
    };
    let specified_height = resolve_dim(style.height, space.percentage_block).map(to_content_h);
    let min_h = resolve_dim(style.min_height, space.percentage_block).map(to_content_h);
    let max_h = resolve_dim(style.max_height, space.percentage_block).map(to_content_h);
    let clamp_h = |mut h: f32| {
        if let Some(max) = max_h {
            h = h.min(max);
        }
        if let Some(min) = min_h {
            h = h.max(min);
        }
        h
    };
    // A definite height, after min/max, is the children's percentage basis.
    let child_space = ConstraintSpace {
        available_inline: metrics.content_width,
        percentage_block: specified_height.map(clamp_h),
        viewport_w: space.viewport_w,
        viewport_h: space.viewport_h,
    };

    let can_escape_top = !bfc_root && bt == 0.0 && pt == 0.0;
    let mut start_strut = MarginStrut::default();
    start_strut.append(metrics.margin[0]);
    let mut pending = MarginStrut::default();
    let mut cursor = 0.0f32;
    let mut at_top = true; // no in-flow content placed yet
    let mut children = Vec::new();

    let mut child = index + 1;
    while child < info.subtree_end {
        let child_info = &flat[child];
        let next = child_info.subtree_end.max(child + 1);
        if child_info.style.display == Display::None {
            child = next;
            continue;
        }
        let result = layout_block(flat, child, &child_space, false);
        let child_metrics = resolve_inline(child_info.style, &child_space);
        let x = bl + pl + child_metrics.margin[3];
        let y;
        if result.self_collapsing {
            let mut combined = pending;
            combined.merge(result.start_strut);
            if at_top && can_escape_top {
                y = 0.0;
                start_strut.merge(combined);
                start_strut.merge(result.end_strut);
            } else {
                y = cursor + combined.sum();
                pending.merge(result.start_strut);
                pending.merge(result.end_strut);
            }
        } else if at_top && can_escape_top {
            start_strut.merge(pending);
            start_strut.merge(result.start_strut);
            y = 0.0;
            cursor = result.fragment.height;
            pending = result.end_strut;
            at_top = false;
        } else {
            let mut combined = pending;
            combined.merge(result.start_strut);
            y = cursor + combined.sum();
            cursor = y + result.fragment.height;
            pending = result.end_strut;
            at_top = false;
        }
        children.push((x, bt + pt + y, result.fragment));
        child = next;
    }

    // CSS2.1 8.3.1: the last child's bottom margin adjoins ours when our
    // height is auto (min-height only matters for collapsing through).
    let can_escape_bottom = !bfc_root && bb == 0.0 && pb == 0.0 && specified_height.is_none();

    let mut end_strut = MarginStrut::default();
    let auto_content = if can_escape_bottom {
        cursor
    } else {
        // A negative end strut can pull the block end up (never below 0).
        (cursor + pending.sum()).max(0.0)
    };
    let content_height = clamp_h(specified_height.unwrap_or(auto_content));
    if can_escape_bottom && content_height == auto_content {
        end_strut = pending;
    }
    // Otherwise min/max moved the block end away from the last child's
    // margin edge, so its margins no longer adjoin ours (Blink FinishLayout
    // only propagates the end strut when the block size is the intrinsic one).
    // CSS2.1 8.3.1 collapse-through: no top/bottom border or padding, not a
    // BFC root, zero or auto height, zero min-height, no in-flow content.
    let self_collapsing = at_top
        && can_escape_top
        && !bfc_root
        && bb == 0.0
        && pb == 0.0
        && specified_height.is_none_or(|h| h == 0.0)
        && min_h.is_none_or(|m| m == 0.0)
        && content_height == 0.0;
    if self_collapsing {
        // CSS2 8.3.1: top and bottom margins adjoin; everything collapses
        // into one strut reported as the start strut.
        start_strut.merge(pending);
        end_strut = MarginStrut::default();
        end_strut.append(metrics.margin[2]);
    } else {
        end_strut.append(metrics.margin[2]);
    }
    if !can_escape_top {
        // Children's margins were resolved inside; only our own margin
        // participates outside.
        let mut own = MarginStrut::default();
        own.append(metrics.margin[0]);
        start_strut = own;
    }

    LayoutResult {
        fragment: BoxFragment {
            index,
            width: metrics.content_width + bl + br + pl + pr,
            height: content_height + bt + bb + pt + pb,
            children,
        },
        start_strut,
        end_strut,
        self_collapsing,
    }
}
