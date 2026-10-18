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

use super::block::{border_widths, clamp_inline_size, intrinsic_sizes, layout_block_sized};
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
    let width = match length(style.width, w, style, &space).map(content_w) {
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
                let sizes = intrinsic_sizes(flat, index, &space)?;
                let fit = match sizes.min {
                    Some(min) => sizes.max.min(available.max(min)),
                    None if sizes.max <= available + 1.0 / 128.0 => sizes.max,
                    None => return None,
                };
                clamp_inline_size(style, &space, edges_h, fit.max(0.0))
            }
        },
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
    let specified_h = length(style.height, h, style, &space).map(content_h);
    let stretched = match (specified_h, top, bottom) {
        (None, Some(t), Some(bo)) => {
            Some((h - t - bo - mt.unwrap_or(0.0) - mb.unwrap_or(0.0) - edges_v).max(0.0))
        }
        _ => None,
    };
    let result = layout_block_sized(
        flat,
        index,
        &space,
        true,
        Some((width, specified_h.or(stretched))),
    )?;
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
        return None;
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
            placed.push(layout_out_of_flow(
                flat, &candidate, cb, viewport_w, viewport_h,
            )?);
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

pub(super) fn direction_rtl(style: &Style) -> bool {
    style.direction == TextDirection::Rtl
}
