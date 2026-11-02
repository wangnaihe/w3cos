//! Orthogonal flows: a `vertical-lr` / `vertical-rl` block inside a
//! horizontal block container.
//!
//! Modeled on Blink LayoutNG's orthogonal writing-mode handling
//! (third_party/blink/renderer/core/layout/, Copyright The Chromium
//! Authors, BSD-style license): an orthogonal root is an independent
//! formatting context whose *inline* size is the physical height. Its
//! available inline size is the containing block's definite block size, or
//! the initial containing block's height when that is indefinite
//! (NGConstraintSpace orthogonal fallback inline size), minus its margins,
//! and an `auto` inline size is fit-content (css-writing-modes-3 7.3). The
//! block size (the physical width) is the content's, never stretched.
//!
//! Here the root's subtree is laid out by the horizontal algorithms on a
//! copy of the tree with transposed styles (`transpose_style`: sizes swap
//! axes, margin/padding/border sides map to logical sides, writing-mode
//! becomes horizontal-tb), and the resulting fragments are mapped back to
//! physical space (`transpose_fragment`). Only shapes whose transposition is
//! exact are accepted (`root_flow`); every other vertical box keeps the
//! behaviour it had before.

use super::block;
use super::exclusion::ExclusionSpace;
use super::{BoxFragment, ConstraintSpace, LastBaseline, LayoutResult, MarginStrut, inline};
use crate::layout::{FlatNodeInfo, resolve_spacing_for_layout};
use w3cos_std::ComponentKind;
use w3cos_std::style::{
    Clear, Dimension, Display, Float, Position, Spacing, Style, TextAlign, TextDecoration,
    TextDirection, UnicodeBidi, WritingMode,
};

/// The block flow direction of an orthogonal root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Flow {
    /// `vertical-lr`: lines stack from left to right.
    Lr,
    /// `vertical-rl`: lines stack from right to left.
    Rl,
}

fn flow_of(style: &Style) -> Option<Flow> {
    match style.writing_mode {
        WritingMode::VerticalLr => Some(Flow::Lr),
        WritingMode::VerticalRl => Some(Flow::Rl),
        _ => None,
    }
}

/// Characters that stay in sideways (rotated) glyph runs under
/// `text-orientation: mixed`: Latin scripts and general punctuation. Upright
/// scripts, bidi controls and scripts with their own vertical rules are not
/// modeled.
fn sideways_char(c: char) -> bool {
    c <= '\u{24F}' || ('\u{2010}'..='\u{2027}').contains(&c)
}

/// Internal markers that only carry cascade values this module's styles are
/// unaffected by. Every other marker (aspect-ratio, size containment,
/// intrinsic keywords, flex/grid helpers, ...) changes sizing and is
/// not transposed, so a box carrying one stays on the existing path.
const INERT_MARKERS: &[&str] = &[
    "--w3cos-internal-font-size-origin",
    "--w3cos-internal-font-specified-size",
    "--w3cos-internal-html-standard-font",
    "--w3cos-internal-text-language",
    "--w3cos-internal-text-decoration-color",
    "--w3cos-internal-z-index-specified",
    "--w3cos-internal-background-inherit",
    "--w3cos-internal-quirks-mode",
];

fn has_internal_marker(style: &Style, allowed: &[&str]) -> bool {
    style.custom_properties.as_ref().is_some_and(|props| {
        props.keys().any(|key| {
            key.starts_with("--w3cos-internal")
                && !INERT_MARKERS.contains(&key.as_str())
                && !allowed.contains(&key.as_str())
        })
    })
}

fn plain_style(style: &Style, flow: Flow) -> bool {
    flow_of(style) == Some(flow)
        && style.direction == TextDirection::Ltr
        && style.float == Float::None
        && style.clear == Clear::None
        && style.position == Position::Static
        && style.unicode_bidi == UnicodeBidi::Normal
        && !block::non_visible_overflow(style)
        && matches!(style.text_align, TextAlign::Start)
        && style.text_decoration == TextDecoration::None
        && style.column_count.is_none()
        && style.column_width == Dimension::Auto
}

/// The flow of `index` when it is an orthogonal root this module lays out
/// exactly; `None` leaves the box on the pre-existing path.
pub(super) fn root_flow(flat: &[FlatNodeInfo], index: usize) -> Option<Flow> {
    let info = &flat[index];
    let style = info.style;
    let flow = flow_of(style)?;
    let parent = info.parent?;
    let parent_style = flat[parent].style;
    if parent_style.writing_mode != WritingMode::HorizontalTb
        || parent_style.direction != TextDirection::Ltr
        || !matches!(parent_style.display, Display::Block | Display::FlowRoot)
        || block::is_author_flex(parent_style)
        || block::is_author_grid(parent_style)
        || block::is_float_group(parent_style)
        || inline::is_inline_container(flat, parent)
        || !matches!(
            flat[parent].kind,
            ComponentKind::Box | ComponentKind::Column | ComponentKind::Row | ComponentKind::Root
        )
    {
        return None;
    }
    // A parent whose block size comes from a ratio or containment gives its
    // children percentages the transposed layout cannot see.
    if has_internal_marker(parent_style, &[]) {
        return None;
    }
    if !matches!(style.display, Display::Block | Display::FlowRoot)
        || !matches!(
            info.kind,
            ComponentKind::Box | ComponentKind::Column | ComponentKind::Row
        )
        || !plain_style(style, flow)
        || block::is_author_flex(style)
        || block::is_author_grid(style)
        || has_internal_marker(style, &[])
        || [
            style.margin.top,
            style.margin.right,
            style.margin.bottom,
            style.margin.left,
        ]
        .contains(&Spacing::Auto)
    {
        return None;
    }
    for node in index + 1..info.subtree_end {
        let node_info = &flat[node];
        let s = node_info.style;
        if s.display == Display::None {
            continue;
        }
        let kind_ok = match node_info.kind {
            ComponentKind::Text { content } => content.chars().all(sideways_char),
            ComponentKind::Box | ComponentKind::Column | ComponentKind::Row => true,
            _ => false,
        };
        let display_ok = match s.display {
            Display::Block | Display::FlowRoot | Display::Inline => true,
            Display::Flex => s
                .custom_properties
                .as_ref()
                .is_some_and(|p| p.contains_key(inline::IFC_MARKER)),
            _ => false,
        };
        if !kind_ok
            || !display_ok
            || !plain_style(s, flow)
            || block::is_author_flex(s)
            || block::is_author_grid(s)
            || has_internal_marker(s, &[inline::IFC_MARKER])
        {
            return None;
        }
    }
    Some(flow)
}

/// The logical (horizontal-tb) sides of a physical top/right/bottom/left
/// quadruple: index of the physical side each of top', right', bottom',
/// left' reads.
fn side_map(flow: Flow) -> [usize; 4] {
    match flow {
        // block-start = left, inline-start = top.
        Flow::Lr => [3, 2, 1, 0],
        // block-start = right.
        Flow::Rl => [1, 2, 3, 0],
    }
}

fn map_sides<T: Copy>(physical: [T; 4], flow: Flow) -> [T; 4] {
    let map = side_map(flow);
    [
        physical[map[0]],
        physical[map[1]],
        physical[map[2]],
        physical[map[3]],
    ]
}

/// The style of a subtree box in transposed (horizontal-tb) space.
pub(super) fn transpose_style(style: &Style, flow: Flow) -> Style {
    let mut out = style.clone();
    out.writing_mode = WritingMode::HorizontalTb;
    out.width = style.height;
    out.height = style.width;
    out.min_width = style.min_height;
    out.min_height = style.min_width;
    out.max_width = style.max_height;
    out.max_height = style.max_width;
    let margin = map_sides(
        [
            style.margin.top,
            style.margin.right,
            style.margin.bottom,
            style.margin.left,
        ],
        flow,
    );
    out.margin.top = margin[0];
    out.margin.right = margin[1];
    out.margin.bottom = margin[2];
    out.margin.left = margin[3];
    let padding = map_sides(
        [
            style.padding.top,
            style.padding.right,
            style.padding.bottom,
            style.padding.left,
        ],
        flow,
    );
    out.padding.top = padding[0];
    out.padding.right = padding[1];
    out.padding.bottom = padding[2];
    out.padding.left = padding[3];
    let border = map_sides(block::border_widths(style), flow);
    out.border_top_width = Some(border[0]);
    out.border_right_width = Some(border[1]);
    out.border_bottom_width = Some(border[2]);
    out.border_left_width = Some(border[3]);
    out.border_styles = map_sides(style.border_styles, flow);
    out.computed_border_widths = map_sides(style.computed_border_widths, flow);
    out
}

/// A dimension of the physical axis whose percentages refer to `basis`
/// (the containing block's physical size), as an absolute length where that
/// basis is definite and `auto` / `none` where it is not.
fn resolve_percent(d: Dimension, basis: Option<f32>) -> Dimension {
    match d {
        Dimension::Percent(p) => basis.map_or(Dimension::Auto, |b| Dimension::Px(b * p / 100.0)),
        Dimension::Calc { percent, px } => {
            basis.map_or(Dimension::Auto, |b| Dimension::Px(b * percent / 100.0 + px))
        }
        other => other,
    }
}

/// Lay out the orthogonal root `index` (see `root_flow`). `None` is a bail.
pub(super) fn layout(
    flat: &[FlatNodeInfo],
    index: usize,
    flow: Flow,
    space: &ConstraintSpace,
    sized: Option<(f32, Option<f32>)>,
    exclusions: &ExclusionSpace,
) -> Option<LayoutResult> {
    // Contexts this slice does not model: a measured size (flex/grid/float
    // items), a probe, a float the root would have to avoid, a right-to-left
    // containing block.
    if sized.is_some() {
        return super::bail("orthogonal:sized");
    }
    if space.percent_unresolved || space.rtl {
        return super::bail("orthogonal:probe");
    }
    if !exclusions.is_empty() {
        return super::bail("orthogonal:float");
    }
    let style = flat[index].style;
    let cb_width = space.available_inline;
    let side = |s: Spacing| {
        resolve_spacing_for_layout(s, cb_width, style, space.viewport_w, space.viewport_h)
    };
    // Physical margins: the root's inline margins (top/bottom) and block
    // margins (left/right) are the parent's to place, through the style.
    let margin_top = side(style.margin.top);
    let margin_bottom = side(style.margin.bottom);

    // The root's own transposed style: margins stay with the parent;
    // percentages of both axes resolve against the containing block here
    // (padding and margin against its width, the physical height against
    // its definite block size, the physical width against its width).
    let mut inner = transpose_style(style, flow);
    inner.margin.top = Spacing::Px(0.0);
    inner.margin.right = Spacing::Px(0.0);
    inner.margin.bottom = Spacing::Px(0.0);
    inner.margin.left = Spacing::Px(0.0);
    let padding = map_sides(
        [
            side(style.padding.top),
            side(style.padding.right),
            side(style.padding.bottom),
            side(style.padding.left),
        ],
        flow,
    );
    inner.padding.top = Spacing::Px(padding[0]);
    inner.padding.right = Spacing::Px(padding[1]);
    inner.padding.bottom = Spacing::Px(padding[2]);
    inner.padding.left = Spacing::Px(padding[3]);
    // The physical height is this root's inline size: a percentage of an
    // indefinite containing block resolves against the initial containing
    // block's height (the orthogonal fallback size).
    let has_percent_height = [style.height, style.min_height, style.max_height]
        .iter()
        .any(|d| matches!(d, Dimension::Percent(_) | Dimension::Calc { .. }));
    if has_percent_height && space.percentage_block.is_none() {
        // Whether the containing block's height is really indefinite is
        // not known here (ratio-sized, stretched or flexed boxes).
        return super::bail("orthogonal:percent-height");
    }
    let height_basis = space.percentage_block;
    inner.width = resolve_percent(style.height, height_basis);
    inner.min_width = resolve_percent(style.min_height, height_basis);
    inner.max_width = resolve_percent(style.max_height, height_basis);
    inner.height = resolve_percent(style.width, Some(cb_width));
    inner.min_height = resolve_percent(style.min_width, Some(cb_width));
    inner.max_height = resolve_percent(style.max_width, Some(cb_width));

    // Transposed copy of the tree: the root's subtree in horizontal space.
    let mut styles: Vec<Style> = Vec::new();
    styles.push(inner);
    for node in index + 1..flat[index].subtree_end {
        styles.push(transpose_style(flat[node].style, flow));
    }
    let transposed: Vec<FlatNodeInfo> = flat
        .iter()
        .enumerate()
        .map(|(i, node)| {
            let mut node = node.clone();
            if (index..flat[index].subtree_end).contains(&i) {
                node.style = &styles[i - index];
            }
            node
        })
        .collect();

    // Available inline size: the definite block size of the containing
    // block, else the initial containing block's height.
    let available = space.percentage_block.unwrap_or(space.viewport_h) - margin_top - margin_bottom;
    let inner_space = ConstraintSpace {
        available_inline: available.max(0.0),
        percentage_block: None,
        viewport_w: space.viewport_w,
        viewport_h: space.viewport_h,
        rtl: false,
        bfc_x: 0.0,
        bfc_y: 0.0,
        margin_base: 0.0,
        margin_strut: MarginStrut::default(),
        forced_bfc: false,
        adjoining_from: 0,
        fixed_block_indefinite: false,
        percent_unresolved: false,
    };
    let inner_style = &styles[0];
    let inline_sized = if inner_style.width == Dimension::Auto {
        let metrics = block::resolve_inline(inner_style, &inner_space);
        let width = block::shrink_to_fit(&transposed, index, &inner_space, &metrics)?;
        Some((width, None))
    } else {
        None
    };
    let mut local = ExclusionSpace::default();
    let result = block::layout_block_in(
        &transposed,
        index,
        &inner_space,
        true,
        inline_sized,
        &mut local,
    )?;
    if !result.oof.is_empty() {
        return super::bail("orthogonal:oof");
    }
    let fragment = transpose_fragment(&result.fragment, flow, flat);
    let mut start_strut = MarginStrut::default();
    start_strut.append(margin_top);
    let mut end_strut = MarginStrut::default();
    end_strut.append(margin_bottom);
    Some(LayoutResult {
        fragment,
        last_baseline: LastBaseline::NoLines,
        first_baseline: LastBaseline::NoLines,
        start_strut,
        end_strut,
        self_collapsing: false,
        oof: Vec::new(),
        bfc_dependent: false,
        float_overlap: false,
    })
}

/// The min/max-content contribution of an orthogonal root to its (horizontal)
/// parent: its block size, which does not depend on the parent's available
/// space (the inline size comes from the containing block's height), as a
/// content-box width like every `intrinsic_sizes_at` result.
pub(super) fn intrinsic(
    flat: &[FlatNodeInfo],
    index: usize,
    flow: Flow,
    space: &ConstraintSpace,
) -> Option<block::MinMax> {
    let result = layout(flat, index, flow, space, None, &ExclusionSpace::default())?;
    let style = flat[index].style;
    let side = |s: Spacing| {
        resolve_spacing_for_layout(
            s,
            space.available_inline,
            style,
            space.viewport_w,
            space.viewport_h,
        )
    };
    let border = block::border_widths(style);
    let edges = border[1] + border[3] + side(style.padding.left) + side(style.padding.right);
    Some(block::MinMax::exact(
        (result.fragment.width - edges).max(0.0),
    ))
}

/// A fragment of transposed space mapped back to physical space.
pub(super) fn transpose_fragment(
    fragment: &BoxFragment,
    flow: Flow,
    flat: &[FlatNodeInfo],
) -> BoxFragment {
    let width = fragment.height;
    let height = fragment.width;
    let children = fragment
        .children
        .iter()
        .map(|(x, y, child)| {
            let child = transpose_fragment(child, flow, flat);
            let px = match flow {
                Flow::Lr => *y,
                Flow::Rl => width - *y - child.width,
            };
            (px, *x, child)
        })
        .collect();
    BoxFragment {
        index: fragment.index,
        width,
        height,
        text_range: fragment.text_range,
        ellipsis: fragment.ellipsis,
        vertical: matches!(flat[fragment.index].kind, ComponentKind::Text { .. }),
        children,
    }
}
