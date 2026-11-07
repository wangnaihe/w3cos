//! Multicolumn layout (CSS Multicol / Blink NGColumnLayoutAlgorithm).
//!
//! Keep one logical flow laid out against the used column width (legacy
//! `layout.rs` + paint `column_fragments`). The principal box keeps the
//! full content width; paint projects fragments into columns. Balancing
//! for `height: auto` uses a simple equal split of the continuous flow
//! height across the column count. Inc 3 admits auto/percent/ch sizes;
//! Inc 4 admits floats and nested multicol in the continuous column
//! flow. Inc 5 admits abspos in the continuous-flow BFC. Inc 6 admits
//! auto-height text-align-last (NG text pieces skip paint-time re-align).
//! Inc 7 admits auto×auto under ordinary flex; grid/lanes/inline-grid parents stay legacy.

use crate::layout::FlatNodeInfo;
use w3cos_std::style::{ColumnFill, Dimension, Display, Style, WritingMode};

/// Block containers that establish a multicol formatting context.
pub(super) fn establishes(style: &Style) -> bool {
    matches!(
        style.display,
        Display::Block
            | Display::FlowRoot
            | Display::InlineBlock
            | Display::ListItem
            | Display::TableCell
            | Display::TableCaption
    ) && (style.column_count.is_some() || style.column_width != Dimension::Auto)
}

/// Inc 7: horizontal-tb block multicol may have auto/percent/ch width or
/// height, in-flow floats, nested multicol, abspos children,
/// auto-height text-align-last (layout-applied; paint pieces do not
/// re-align), and auto×auto under ordinary flex parents. Auto×auto under
/// a grid / grid-lanes / inline-grid parent stays legacy (gap-002: the
/// test's lanes item and the ref's inline-grid item must share an engine;
/// multicol-as-grid-item sizing is still wrong).
pub(super) fn eligible(flat: &[FlatNodeInfo], index: usize) -> bool {
    let style = flat[index].style;
    if !establishes(style) {
        return false;
    }
    if style.writing_mode != WritingMode::HorizontalTb {
        return false;
    }
    if matches!(style.width, Dimension::Auto) && matches!(style.height, Dimension::Auto) {
        if let Some(parent) = flat[index].parent {
            // Keep grid (incl. lanes / inline-grid) on legacy so gap-002's
            // test+ref stay same-engine. Ordinary flex parents are OK.
            if super::block::is_author_grid(flat[parent].style) {
                return false;
            }
        }
    }
    // Floats, nested multicol, abspos, and auto-height text-align-last use
    // the continuous-flow BFC (each nest re-checks eligible()). Paint must
    // not re-apply text-align-last on NG text pieces (block_text_paint).
    true
}

/// Used column content width (CSS Multicol §3.4).
pub(super) fn used_width(
    style: &Style,
    content_width: f32,
    viewport_w: f32,
    viewport_h: f32,
) -> Option<f32> {
    crate::layout::used_column_width(style, content_width, viewport_w, viewport_h)
}

/// Number of columns that fit the content width at `column_width` + gap.
pub(super) fn count(style: &Style, content_width: f32, column_width: f32) -> usize {
    let gap = style.column_gap.unwrap_or(style.font_size).max(0.0);
    let stride = column_width + gap;
    if stride <= 0.0 || !content_width.is_finite() {
        return 1;
    }
    let fitted = ((content_width + gap) / stride).round().max(1.0) as usize;
    style
        .column_count
        .map_or(fitted, |n| fitted.min(n.max(1) as usize))
        .max(1)
}

/// Content-box block size after laying out the continuous column flow of
/// height `flow_height`. Definite heights win; otherwise balance by
/// splitting the flow across the column count (Blink's equal-height
/// first approximation).
pub(super) fn content_block_size(
    style: &Style,
    specified_height: Option<f32>,
    flow_height: f32,
    column_count: usize,
) -> f32 {
    if let Some(height) = specified_height {
        return height.max(0.0);
    }
    let n = column_count.max(1) as f32;
    let balanced = (flow_height / n).ceil().max(0.0);
    match style.column_fill {
        ColumnFill::Auto => {
            // Auto with indefinite height fills one column (CSS Multicol
            // §7.1): the flow height is the used height.
            flow_height.max(0.0)
        }
        ColumnFill::Balance | ColumnFill::BalanceAll => balanced,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use w3cos_std::style::Style;

    #[test]
    fn balance_splits_flow_across_columns() {
        let style = Style {
            column_count: Some(3),
            column_fill: ColumnFill::Balance,
            column_gap: Some(0.0),
            ..Style::default()
        };
        assert_eq!(content_block_size(&style, None, 300.0, 3), 100.0);
        assert_eq!(content_block_size(&style, Some(80.0), 300.0, 3), 80.0);
        let auto = Style {
            column_fill: ColumnFill::Auto,
            ..style
        };
        assert_eq!(content_block_size(&auto, None, 300.0, 3), 300.0);
    }
}
