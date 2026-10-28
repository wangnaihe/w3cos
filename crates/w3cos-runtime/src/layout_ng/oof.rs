//! Out-of-flow positioning, stage S3a.
//!
//! Ported in structure from Blink's NGOutOfFlowLayoutPart and
//! ComputeOutOfFlowInlineDimensions / ComputeOutOfFlowBlockDimensions
//! (third_party/blink/renderer/core/layout/out_of_flow_layout_part.cc,
//! absolute_utils.cc, Copyright The Chromium Authors, BSD-style license):
//! an absolutely positioned box leaves the flow at its static position (an
//! NGLogicalStaticPosition carried up as an OOF candidate, relative to each
//! ancestor's border box) and is laid out once its containing block's size
//! is known: the padding box of the nearest positioned ancestor, or the
//! initial containing block (CSS2 10.1).  Widths and horizontal insets
//! follow CSS2 10.3.7, heights and vertical insets 10.6.4.  Relative
//! offsets (CSS2 9.4.3, NGRelativeUtils::ComputeRelativeOffset) only move
//! the box's fragment.

use super::Why;
use super::block::{border_widths, clamp_inline_size, intrinsic_sizes_at, layout_block_sized};
use super::{BoxFragment, ConstraintSpace, MarginStrut};
use crate::layout::{FlatNodeInfo, resolve_spacing_for_layout};
use w3cos_std::style::{BoxSizing, Dimension, Position, Spacing, Style, TextDirection};

/// An out-of-flow box waiting for its containing block, with its static
/// position (NGLogicalStaticPosition: the margin-box inline-start edge
/// and block-start edge it would have in flow) relative to the border box
/// of the fragment currently carrying it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct OofCandidate {
    pub index: usize,
    /// Left edge (ltr parent) or right edge (rtl parent) of the static
    /// position.
    pub static_inline: f32,
    pub static_block: f32,
    pub static_rtl: bool,
}

impl OofCandidate {
    pub fn translated(mut self, x: f32, y: f32) -> Self {
        self.static_inline += x;
        self.static_block += y;
        self
    }
}

pub(super) fn is_out_of_flow(style: &Style) -> bool {
    matches!(style.position, Position::Absolute | Position::Fixed)
}

/// The containing block's padding box, in the border-box coordinates of
/// the fragment that resolves the candidates.
#[derive(Debug, Clone, Copy)]
pub(super) struct ContainingBlock {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub rtl: bool,
}

fn length(d: Dimension, basis: f32, style: &Style, space: &ConstraintSpace) -> Option<f32> {
    super::block::resolve_length(d, Some(basis), style, space)
}

/// CSS2 9.4.3 relative offset of a box whose containing block is `space`
/// (percentage top/bottom need a definite height, otherwise auto).
pub(super) fn relative_offset(style: &Style, space: &ConstraintSpace) -> (f32, f32) {
    if style.position != Position::Relative {
        return (0.0, 0.0);
    }
    let w = space.available_inline;
    let inset =
        |d: Dimension, basis: Option<f32>| super::block::resolve_length(d, basis, style, space);
    let left = inset(style.left, Some(w));
    let right = inset(style.right, Some(w));
    let dx = match (left, right) {
        (None, None) => 0.0,
        (Some(l), None) => l,
        (None, Some(r)) => -r,
        // Over-constrained: the containing block's end side is ignored.
        (Some(l), Some(r)) => {
            if space.rtl {
                -r
            } else {
                l
            }
        }
    };
    let top = inset(style.top, space.percentage_block);
    let bottom = inset(style.bottom, space.percentage_block);
    let dy = match (top, bottom) {
        (Some(t), _) => t,
        (None, Some(b)) => -b,
        (None, None) => 0.0,
    };
    (dx, dy)
}

/// Lay out one out-of-flow box in its containing block.  Returns its
/// border-box offset in the resolving fragment's coordinates.
pub(super) fn layout_out_of_flow(
    flat: &[FlatNodeInfo],
    candidate: &OofCandidate,
    cb: &ContainingBlock,
    viewport_w: f32,
    viewport_h: f32,
) -> Option<(f32, f32, BoxFragment)> {
    let index = candidate.index;
    let style = flat[index].style;
    let space = ConstraintSpace {
        available_inline: cb.width,
        percentage_block: Some(cb.height),
        viewport_w,
        viewport_h,
        rtl: cb.rtl,
        bfc_x: 0.0,
        bfc_y: 0.0,
        margin_base: 0.0,
        margin_strut: MarginStrut::default(),
        forced_bfc: false,
        adjoining_from: 0,
        fixed_block_indefinite: false,
    };
    let (w, h) = (cb.width, cb.height);
    let spacing = |s: Spacing| match s {
        Spacing::Auto => None,
        // Percentage margins and padding refer to the containing block's
        // width (CSS2 8.3, 8.4).
        s => Some(resolve_spacing_for_layout(
            s, w, style, viewport_w, viewport_h,
        )),
    };
    let b = border_widths(style);
    let p = [
        spacing(style.padding.top).unwrap_or(0.0),
        spacing(style.padding.right).unwrap_or(0.0),
        spacing(style.padding.bottom).unwrap_or(0.0),
        spacing(style.padding.left).unwrap_or(0.0),
    ];
    let edges_h = b[1] + b[3] + p[1] + p[3];
    let edges_v = b[0] + b[2] + p[0] + p[2];
    let (mt, mr, mb, ml) = (
        spacing(style.margin.top),
        spacing(style.margin.right),
        spacing(style.margin.bottom),
        spacing(style.margin.left),
    );

    // `stretch` fills the containing block's padding box between the
    // insets (an auto inset is zero unless both are auto, which places the
    // box at its static position). Its min/max forms stay on legacy.
    let stretch = super::block::marker(style, super::block::STRETCH).unwrap_or("");
    if stretch
        .split(',')
        .any(|t| matches!(t, "minw" | "maxw" | "minh" | "maxh"))
    {
        return super::bail("oof-stretch-limit");
    }
    let stretch_w = stretch.split(',').any(|t| t == "w");
    let stretch_h = stretch.split(',').any(|t| t == "h");
    // CSS2 10.3.7.
    let mut left = length(style.left, w, style, &space);
    let mut right = length(style.right, w, style, &space);
    let static_inline = candidate.static_inline - cb.x;
    if left.is_none() && right.is_none() {
        if candidate.static_rtl {
            right = Some(w - static_inline);
        } else {
            left = Some(static_inline);
        }
    }
    let content_w = |v: f32| match style.box_sizing {
        BoxSizing::BorderBox => (v - edges_h).max(0.0),
        BoxSizing::ContentBox => v,
    };
    // A block size that does not depend on the inline size
    // (specified, or stretched between both insets, CSS2
    // 10.6.4) is definite for the content (CSS Flexbox §9.8).
    let content_h = |v: f32| match style.box_sizing {
        BoxSizing::BorderBox => (v - edges_v).max(0.0),
        BoxSizing::ContentBox => v,
    };
    let block = length(style.height, h, style, &space)
        .map(content_h)
        .or_else(|| {
            if stretch_h {
                // Both insets resolve below; here only whether the block
                // size is definite for the content.
                let top = length(style.top, h, style, &space).unwrap_or(0.0);
                let bottom = length(style.bottom, h, style, &space).unwrap_or(0.0);
                return Some(
                    content_h(h - top - bottom - mt.unwrap_or(0.0) - mb.unwrap_or(0.0)).max(0.0),
                );
            }
            if content_height_keyword(style) {
                return None;
            }
            let top = length(style.top, h, style, &space)?;
            let bottom = length(style.bottom, h, style, &space)?;
            Some((h - top - bottom - mt.unwrap_or(0.0) - mb.unwrap_or(0.0) - edges_v).max(0.0))
        });
    // A replaced box has no shrink-to-fit width and is not stretched between
    // its insets: auto sizes are its natural size (or the other axis through
    // its ratio), CSS 2.1 10.3.8 and 10.6.5 (Blink ComputeReplacedSize).
    // `stretch` fills the space between the insets and margins.
    let replaced_dims = if super::block::is_replaced(flat[index].kind) {
        let top = length(style.top, h, style, &space);
        let bottom = length(style.bottom, h, style, &space);
        let top = if top.is_none() && bottom.is_none() {
            Some(candidate.static_block - cb.y)
        } else {
            top
        };
        let mut sized = style.clone();
        let border_box = |avail: f32, edges: f32| match style.box_sizing {
            BoxSizing::BorderBox => avail.max(0.0),
            BoxSizing::ContentBox => (avail - edges).max(0.0),
        };
        if stretch_w {
            let avail = w
                - left.unwrap_or(0.0)
                - right.unwrap_or(0.0)
                - ml.unwrap_or(0.0)
                - mr.unwrap_or(0.0);
            sized.width = Dimension::Px(border_box(avail, edges_h));
        }
        if stretch_h {
            let avail = h
                - top.unwrap_or(0.0)
                - bottom.unwrap_or(0.0)
                - mt.unwrap_or(0.0)
                - mb.unwrap_or(0.0);
            sized.height = Dimension::Px(border_box(avail, edges_v));
        }
        Some(super::block::replaced_size_with_keyword_limits(
            &flat[index].kind,
            &sized,
            &space,
        )?)
    } else {
        None
    };
    // An intrinsic `width` keyword (css-sizing-3) is the box's own
    // min/max-content size, or for fit-content the available size clamped
    // between them, whatever its insets.
    let width_keyword = super::block::marker(style, super::block::WIDTH_KEYWORD).filter(|_| {
        style.width == Dimension::Auto
            && !super::block::is_replaced(flat[index].kind)
            && !super::block::size_contained(style)
    });
    let width = if let Some((replaced_w, _)) = replaced_dims {
        replaced_w
    } else if stretch_w {
        clamp_inline_size(
            style,
            &space,
            edges_h,
            content_w(
                w - left.unwrap_or(0.0)
                    - right.unwrap_or(0.0)
                    - ml.unwrap_or(0.0)
                    - mr.unwrap_or(0.0),
            )
            .max(0.0),
        )
    } else if let Some(keyword) = width_keyword {
        let available = w
            - left.unwrap_or(0.0)
            - right.unwrap_or(0.0)
            - ml.unwrap_or(0.0)
            - mr.unwrap_or(0.0)
            - edges_h;
        let sizes = intrinsic_sizes_at(flat, index, &space, block).why("oof-intrinsic")?;
        let min = sizes.min.or_else(|| super::bail("oof-width-keyword-min"))?;
        let content = super::block::keyword_content_size(
            keyword,
            min,
            sizes.max,
            Some(available.max(0.0)),
            Some(w),
            style,
            &space,
            edges_h,
        )
        .why("oof-width-keyword")?;
        clamp_inline_size(style, &space, edges_h, content.max(0.0))
    } else {
        match length(style.width, w, style, &space).map(content_w) {
            Some(v) => clamp_inline_size(style, &space, edges_h, v),
            None => match (left, right) {
                (Some(l), Some(r)) => clamp_inline_size(
                    style,
                    &space,
                    edges_h,
                    (w - l - r - ml.unwrap_or(0.0) - mr.unwrap_or(0.0) - edges_h).max(0.0),
                ),
                _ => {
                    // Shrink-to-fit (CSS2 10.3.7 cases 1, 3).
                    let available = w
                        - left.unwrap_or(0.0)
                        - right.unwrap_or(0.0)
                        - ml.unwrap_or(0.0)
                        - mr.unwrap_or(0.0)
                        - edges_h;
                    let sizes =
                        intrinsic_sizes_at(flat, index, &space, block).why("oof-intrinsic")?;
                    let fit = match sizes.min {
                        Some(min) => sizes.max.min(available.max(min)),
                        None if sizes.max <= available + 1.0 / 128.0 => sizes.max,
                        None => return super::bail("oof-shrink-min"),
                    };
                    clamp_inline_size(style, &space, edges_h, fit.max(0.0))
                }
            },
        }
    };
    // Intrinsic min-width / max-width keywords (css-sizing-3).
    let available = w
        - left.unwrap_or(0.0)
        - right.unwrap_or(0.0)
        - ml.unwrap_or(0.0)
        - mr.unwrap_or(0.0)
        - edges_h;
    let width = if replaced_dims.is_some() {
        width
    } else {
        super::block::keyword_limited_inline(
            flat,
            index,
            &space,
            edges_h,
            width,
            available.max(0.0),
            w,
        )
        .why("oof-width-keyword")?
    };
    let border_w = width + edges_h;
    let (margin_left, x) = match (left, right) {
        (Some(l), Some(r)) => {
            let remaining = w - l - r - border_w;
            let (ml_used, mr_used) = match (ml, mr) {
                (None, None) => {
                    let half = super::block::layout_unit_value(remaining / 2.0);
                    if remaining < 0.0 {
                        // Negative free space goes to the end side.
                        if cb.rtl {
                            (remaining, 0.0)
                        } else {
                            (0.0, remaining)
                        }
                    } else {
                        (half, remaining - half)
                    }
                }
                (None, Some(m)) => (remaining - m, m),
                (Some(m), None) => (m, remaining - m),
                (Some(a), Some(c)) => (a, c),
            };
            if ml.is_some() && mr.is_some() && cb.rtl {
                // Over-constrained: left is ignored in an rtl containing block.
                (ml_used, w - r - mr_used - border_w - ml_used)
            } else {
                (ml_used, l)
            }
        }
        (Some(l), None) => (ml.unwrap_or(0.0), l),
        (None, Some(r)) => {
            let m = ml.unwrap_or(0.0);
            (m, w - r - mr.unwrap_or(0.0) - border_w - m)
        }
        (None, None) => unreachable!(),
    };

    // CSS2 10.6.4.
    let mut top = length(style.top, h, style, &space);
    let bottom = length(style.bottom, h, style, &space);
    if top.is_none() && bottom.is_none() {
        top = Some(candidate.static_block - cb.y);
    }
    let content_h = |v: f32| match style.box_sizing {
        BoxSizing::BorderBox => (v - edges_v).max(0.0),
        BoxSizing::ContentBox => v,
    };
    let specified_h = length(style.height, h, style, &space)
        .map(content_h)
        .or_else(|| {
            stretch_h.then(|| {
                content_h(
                    h - top.unwrap_or(0.0)
                        - bottom.unwrap_or(0.0)
                        - mt.unwrap_or(0.0)
                        - mb.unwrap_or(0.0),
                )
                .max(0.0)
            })
        });
    let stretched = match (specified_h, top, bottom) {
        // `height: min-content | max-content | fit-content` is the content
        // height in the block axis (css-sizing-3), not stretched between
        // the insets (fit-content-block-size-abspos).
        (None, Some(_), Some(_)) if content_height_keyword(style) => None,
        (None, Some(t), Some(bo)) => {
            Some((h - t - bo - mt.unwrap_or(0.0) - mb.unwrap_or(0.0) - edges_v).max(0.0))
        }
        _ => None,
    };
    let mut fixed_h = match replaced_dims {
        Some((_, replaced_h)) => Some(replaced_h),
        None => specified_h.or(stretched),
    };
    // An intrinsic min-height / max-height keyword is the content height
    // (css-sizing-3); a specified or stretched height is limited by it.
    let min_keyword = replaced_dims.is_none()
        && super::block::marker(style, super::block::MIN_HEIGHT_KEYWORD).is_some();
    let max_keyword = replaced_dims.is_none()
        && super::block::marker(style, super::block::MAX_HEIGHT_KEYWORD).is_some();
    if let Some(fixed) = fixed_h.filter(|_| min_keyword || max_keyword) {
        let auto =
            layout_block_sized(flat, index, &space, true, Some((width, None))).why("oof-layout")?;
        let content = (auto.fragment.height - edges_v).max(0.0);
        let mut limited = fixed;
        if max_keyword {
            limited = limited.min(content);
        }
        if min_keyword {
            limited = limited.max(content);
        }
        fixed_h = Some(limited);
    }
    let result =
        layout_block_sized(flat, index, &space, true, Some((width, fixed_h))).why("oof-layout")?;
    let border_h = result.fragment.height;
    let y = match (top, bottom) {
        (Some(t), Some(bo)) => {
            let remaining = h - t - bo - border_h;
            let mt_used = match (mt, mb) {
                (None, None) => super::block::layout_unit_value(remaining / 2.0),
                (None, Some(m)) => remaining - m,
                (Some(m), _) => m,
            };
            t + mt_used
        }
        (Some(t), None) => t + mt.unwrap_or(0.0),
        (None, Some(bo)) => h - bo - mb.unwrap_or(0.0) - border_h,
        (None, None) => unreachable!(),
    };
    if !result.oof.is_empty() {
        // (Its own absolute descendants were resolved inside: it is
        // positioned.  Fixed descendants would need carrying further.)
        return super::bail("oof-nested");
    }
    Some((cb.x + x + margin_left, cb.y + y, result.fragment))
}

/// Resolve the candidates this fragment is the containing block for
/// (`all`: the initial containing block takes fixed boxes too).  Returns
/// the placed fragments and the candidates still travelling up.
pub(super) fn resolve(
    flat: &[FlatNodeInfo],
    candidates: Vec<OofCandidate>,
    cb: &ContainingBlock,
    all: bool,
    viewport_w: f32,
    viewport_h: f32,
) -> Option<(Vec<(f32, f32, BoxFragment)>, Vec<OofCandidate>)> {
    let mut placed = Vec::new();
    let mut rest = Vec::new();
    for candidate in candidates {
        if all || flat[candidate.index].style.position == Position::Absolute {
            placed.push(
                layout_out_of_flow(flat, &candidate, cb, viewport_w, viewport_h)
                    .why("oof-layout")?,
            );
        } else {
            rest.push(candidate);
        }
    }
    Some((placed, rest))
}

/// A fixed box's containing block is the viewport unless an ancestor
/// establishes one for it (transform, filter, will-change: transform):
/// those stay on legacy.
pub(super) fn fixed_reason(flat: &[FlatNodeInfo], index: usize) -> Option<&'static str> {
    let mut ancestor = flat[index].parent;
    while let Some(a) = ancestor {
        let s = flat[a].style;
        if s.transform != w3cos_std::style::Transform2D::IDENTITY
            || s.will_change.transform
            || s.filter.is_some()
        {
            return Some("position:fixed-containing-block");
        }
        ancestor = flat[a].parent;
    }
    None
}

/// Whether an auto `height` carries an intrinsic keyword, which in the
/// block axis means the content height.
fn content_height_keyword(style: &Style) -> bool {
    matches!(style.height, w3cos_std::style::Dimension::Auto)
        && matches!(
            super::block::marker(style, "--w3cos-internal-height-keyword"),
            Some("min-content" | "max-content" | "fit-content")
        )
}

pub(super) fn direction_rtl(style: &Style) -> bool {
    style.direction == TextDirection::Rtl
}
