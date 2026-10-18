//! Block formatting context, stage S1 (no floats, no inline content).
//!
//! Ported in structure from Blink's NGBlockLayoutAlgorithm
//! (third_party/blink/renderer/core/layout/block_layout_algorithm.cc,
//! Copyright The Chromium Authors, BSD-style license): children are laid out
//! in order with a margin strut that is resolved when a non-adjoining edge
//! (border, padding, in-flow content, BFC root) is reached.  Width follows
//! CSS2 10.3.3, height 10.6.3, min/max 10.4/10.7, collapsing 8.3.1.

use super::{BoxFragment, ConstraintSpace, LayoutResult, MarginStrut, inline};
use crate::layout::{FlatNodeInfo, resolve_spacing_for_layout};
use w3cos_std::ComponentKind;
use w3cos_std::style::{
    BoxSizing, Clear, Dimension, Display, Float, Overflow, Position, Spacing, Style, TextDirection,
};

fn supported_dimension(_d: Dimension) -> bool {
    // Every Dimension unit resolves like legacy `to_taffy_dim`.
    true
}

fn supported_spacing(s: Spacing, allow_auto: bool) -> bool {
    match s {
        Spacing::Px(_)
        | Spacing::Percent(_)
        | Spacing::Rem(_)
        | Spacing::Em(_)
        | Spacing::Ch(_)
        | Spacing::Vw(_)
        | Spacing::Vh(_) => true,
        Spacing::Auto => allow_auto,
        // env() insets depend on host state the legacy path owns.
        _ => false,
    }
}

/// Internal custom properties that only carry computed values for the
/// CSSOM, font selection or paint; they never mean a legacy layout
/// projection owns the box.
fn is_value_carrier_marker(key: &str) -> bool {
    let name = key.trim_start_matches("--w3cos-internal-");
    matches!(
        name,
        "font-size-origin"
            | "font-specified-size"
            | "html-standard-font"
            | "list-style-image"
            | "text-language"
            | "text-decoration-color"
            | "z-index-specified"
            | "computed-height-percent"
            | "background-inherit"
            | "text-source-run"
            | "text-fragment-ends"
            | "quirks-mode"
    ) || name.starts_with("decoration-owner")
}

/// Why a box is outside the current stage (`None`: supported).  The reason
/// is reported by `W3COS_LAYOUT_NG_TRACE` for coverage accounting.
fn unsupported_style(style: &Style, allowed: &[&str]) -> Option<&'static str> {
    if let Some(reason) = unsupported_marker(style, allowed) {
        return Some(reason);
    }
    if !matches!(style.display, Display::Block | Display::FlowRoot) {
        return Some(match style.display {
            Display::Inline => "display:inline",
            Display::InlineBlock => "display:inline-block",
            Display::Flex | Display::InlineFlex => "display:flex",
            Display::Grid => "display:grid",
            _ => "display:other",
        });
    }
    if style.position != Position::Static {
        return Some("position");
    }
    if style.float != Float::None {
        return Some("float");
    }
    if style.clear != Clear::None {
        return Some("clear");
    }
    if style.overflow != Overflow::Visible
        || style.overflow_x.is_some_and(|o| o != Overflow::Visible)
        || style.overflow_y.is_some_and(|o| o != Overflow::Visible)
    {
        return Some("overflow");
    }
    if style.column_count.is_some() || style.column_width != Dimension::Auto {
        return Some("columns");
    }
    if style.direction != TextDirection::Ltr {
        return Some("direction");
    }
    if ![
        style.width,
        style.height,
        style.min_width,
        style.min_height,
        style.max_width,
        style.max_height,
    ]
    .into_iter()
    .all(supported_dimension)
    {
        return Some("size-unit");
    }
    if ![
        style.margin.top,
        style.margin.right,
        style.margin.bottom,
        style.margin.left,
    ]
    .into_iter()
    .all(|s| supported_spacing(s, true))
        || ![
            style.padding.top,
            style.padding.right,
            style.padding.bottom,
            style.padding.left,
        ]
        .into_iter()
        .all(|s| supported_spacing(s, false))
    {
        return Some("spacing-unit");
    }
    None
}

/// Internal markers mean a legacy projection owns this box, except value
/// carriers and the markers the current stage interprets (`allowed`).
fn unsupported_marker(style: &Style, allowed: &[&str]) -> Option<&'static str> {
    style
        .custom_properties
        .as_ref()
        .is_some_and(|props| {
            props.keys().any(|key| {
                key.starts_with("--w3cos-internal")
                    && !is_value_carrier_marker(key)
                    && !allowed.contains(&key.as_str())
            })
        })
        .then_some("internal-marker")
}

fn node_reason(flat: &[FlatNodeInfo], index: usize) -> Option<&'static str> {
    let info = &flat[index];
    match info.kind {
        ComponentKind::Root | ComponentKind::Box | ComponentKind::Column | ComponentKind::Row => {
            if inline::is_inline_container(flat, index) {
                unsupported_style(
                    info.style,
                    &[inline::IFC_MARKER, inline::ANONYMOUS_BLOCK_MARKER],
                )
                .or_else(|| inline::container_reason(flat, index))
            } else {
                unsupported_style(info.style, &[])
            }
        }
        ComponentKind::Text { content } if info.style.display != Display::Inline => {
            unsupported_style(info.style, &[])
                .or_else(|| inline::block_text_reason(content, info.style))
        }
        ComponentKind::Text { .. } => {
            // Validated with its container (container_reason).
            let in_ifc = info
                .parent
                .is_some_and(|p| inline::is_inline_container(flat, p));
            if !in_ifc {
                Some("ifc:mixed")
            } else {
                unsupported_marker(info.style, &[inline::LINE_WIDTH_MARKER])
            }
        }
        ComponentKind::Image { .. } => Some("image"),
        _ => Some("kind:other"),
    }
}

/// Visit rendered nodes (display:none subtrees emit nothing in either
/// engine) until `f` returns true.
fn walk_rendered(flat: &[FlatNodeInfo], mut f: impl FnMut(usize) -> bool) {
    let mut index = 0;
    while index < flat.len() {
        let info = &flat[index];
        if info.style.display == Display::None {
            index = info.subtree_end.max(index + 1);
            continue;
        }
        if f(index) {
            return;
        }
        index += 1;
    }
}

/// `None` when every rendered node is a box this stage lays out, otherwise
/// the first reason the legacy engine must run.
pub(super) fn unsupported_tree(flat: &[FlatNodeInfo]) -> Option<&'static str> {
    let mut reason = None;
    walk_rendered(flat, |index| {
        reason = node_reason(flat, index);
        reason.is_some()
    });
    reason
}

/// Every distinct reason in the tree (trace mode only); internal markers
/// are reported by name.
pub(super) fn unsupported_reasons(flat: &[FlatNodeInfo]) -> Vec<String> {
    let mut reasons = Vec::new();
    walk_rendered(flat, |index| {
        let info = &flat[index];
        let Some(reason) = node_reason(flat, index) else {
            return false;
        };
        let names: Vec<String> = if reason == "internal-marker" {
            info.style
                .custom_properties
                .iter()
                .flat_map(|props| props.keys())
                .filter(|key| {
                    key.starts_with("--w3cos-internal")
                        && !is_value_carrier_marker(key)
                        && key.as_str() != inline::IFC_MARKER
                        && key.as_str() != inline::ANONYMOUS_BLOCK_MARKER
                        && key.as_str() != inline::LINE_WIDTH_MARKER
                })
                .map(|key| key.trim_start_matches("--w3cos-internal-").to_string())
                .collect()
        } else {
            vec![reason.to_string()]
        };
        for name in names {
            if !reasons.contains(&name) {
                reasons.push(name);
            }
        }
        false
    });
    reasons.sort_unstable();
    reasons
}

#[cfg(test)]
pub(super) fn supports_tree(flat: &[FlatNodeInfo]) -> bool {
    unsupported_tree(flat).is_none()
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

/// Used length of a size property (legacy `to_taffy_dim` units).
fn resolve_dim(
    d: Dimension,
    basis: Option<f32>,
    style: &Style,
    space: &ConstraintSpace,
) -> Option<f32> {
    match d {
        Dimension::Auto => None,
        Dimension::Px(v) => Some(v),
        Dimension::Percent(p) => basis.map(|b| layout_unit(b * p / 100.0)),
        Dimension::Rem(v) => Some(v * 16.0),
        Dimension::Em(v) => Some(v * style.font_size),
        Dimension::Ch(v) => Some(v * style.ch_advance()),
        Dimension::Vw(v) => Some(v * space.viewport_w / 100.0),
        Dimension::Vh(v) => Some(v * space.viewport_h / 100.0),
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
    let specified = resolve_dim(style.width, Some(cb), style, space).map(to_content);
    let mut content_width = specified
        .unwrap_or(cb - margin[1] - margin[3] - edges)
        .max(0.0);
    if let Some(max) = resolve_dim(style.max_width, Some(cb), style, space).map(to_content) {
        content_width = content_width.min(max);
    }
    if let Some(min) = resolve_dim(style.min_width, Some(cb), style, space).map(to_content) {
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
            // Blink ResolveInlineAutoMargins: an auto start margin never
            // goes negative (CSS2 margin-auto-on-block-box, as in Chrome).
            (true, false) => margin[3] = (remaining - margin[1]).max(0.0),
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
) -> Option<(f32, f32, BoxFragment)> {
    let space = ConstraintSpace {
        available_inline: viewport_w,
        percentage_block: Some(viewport_h),
        viewport_w,
        viewport_h,
    };
    let result = layout_block(flat, 0, &space, true)?;
    let metrics = resolve_inline(flat[0].style, &space);
    Some((metrics.margin[3], metrics.margin[0], result.fragment))
}

pub(super) fn layout_block(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    force_bfc_root: bool,
) -> Option<LayoutResult> {
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
    let specified_height =
        resolve_dim(style.height, space.percentage_block, style, space).map(to_content_h);
    // DOM lowering gives an authored IFC block `min-height: font-size *
    // line-height` so its flex-row form keeps the initial strut.  The NG
    // line box already contains that strut (in LayoutUnits), so the
    // synthetic floor is not a real constraint here.
    let synthetic_strut_floor = style
        .custom_properties
        .as_ref()
        .is_some_and(|props| props.contains_key(inline::IFC_MARKER))
        && style.min_height == Dimension::Px(style.font_size * style.line_height)
        && style.height == Dimension::Auto
        && style.max_height == Dimension::Auto;
    let min_h = if synthetic_strut_floor {
        None
    } else {
        resolve_dim(style.min_height, space.percentage_block, style, space).map(to_content_h)
    };
    let max_h =
        resolve_dim(style.max_height, space.percentage_block, style, space).map(to_content_h);
    let clamp_h = |mut h: f32| {
        if let Some(max) = max_h {
            h = h.min(max);
        }
        if let Some(min) = min_h {
            h = h.max(min);
        }
        h
    };
    let anonymous_block = style
        .custom_properties
        .as_ref()
        .is_some_and(|props| props.contains_key(inline::ANONYMOUS_BLOCK_MARKER));
    // A definite height, after min/max, is the children's percentage basis.
    let child_space = ConstraintSpace {
        available_inline: metrics.content_width,
        // Anonymous blocks are skipped when finding the percentage
        // containing block (CSS 2.1 10.5).
        percentage_block: if anonymous_block {
            space.percentage_block
        } else {
            specified_height.map(clamp_h)
        },
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

    if let ComponentKind::Text { content } = info.kind {
        // Block-level text leaf: in-flow content of its own.
        cursor = inline::block_text_height(content, metrics.content_width, style);
        at_top = false;
    } else if inline::is_inline_container(flat, index) {
        let result = inline::layout_inline(flat, index, metrics.content_width)?;
        cursor = result.height;
        at_top = result.children.is_empty();
        children.extend(
            result
                .children
                .into_iter()
                .map(|(x, y, fragment)| (bl + pl + x, bt + pt + y, fragment)),
        );
    }
    let mut child = if inline::is_inline_container(flat, index) {
        info.subtree_end
    } else {
        index + 1
    };
    while child < info.subtree_end {
        let child_info = &flat[child];
        let next = child_info.subtree_end.max(child + 1);
        if child_info.style.display == Display::None {
            child = next;
            continue;
        }
        let result = layout_block(flat, child, &child_space, false)?;
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
    // BFC root, no in-flow content, and a zero *used* block size.  Blink
    // (NGBlockLayoutAlgorithm::FinishLayout) leaves the BFC block offset
    // unresolved exactly when the final block size is zero, so
    // `height: 100px; max-height: 0` collapses through like `height: 0`
    // (CSS2 max-height-100), while min-height > 0 keeps it from doing so.
    let self_collapsing =
        at_top && can_escape_top && !bfc_root && bb == 0.0 && pb == 0.0 && content_height == 0.0;
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

    Some(LayoutResult {
        fragment: BoxFragment {
            index,
            width: metrics.content_width + bl + br + pl + pr,
            height: content_height + bt + bb + pt + pb,
            children,
        },
        start_strut,
        end_strut,
        self_collapsing,
    })
}
