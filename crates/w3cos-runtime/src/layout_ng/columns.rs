//! Multicolumn layout (CSS Multicol / Blink NGColumnLayoutAlgorithm).
//!
//! First slice: keep one logical flow laid out against the used column
//! width (legacy `layout.rs` + paint `column_fragments`). The principal
//! box keeps the full content width; paint projects fragments into
//! columns. Balancing for `height: auto` uses a simple equal split of the
//! continuous flow height across the column count.

use crate::layout::FlatNodeInfo;
use w3cos_std::style::{ColumnFill, Dimension, Display, Float, Position, Style, WritingMode};

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

/// Narrow first slice: horizontal-tb, no floats / abspos in the subtree,
/// and either a definite height (column-fill auto or balance) or
/// balance/balance-all with auto height.
pub(super) fn eligible(flat: &[FlatNodeInfo], index: usize) -> bool {
    let style = flat[index].style;
    if !establishes(style) {
        return false;
    }
    if style.writing_mode != WritingMode::HorizontalTb {
        return false;
    }
    // First slice needs a definite border box so paint's fragmentainer
    // height matches the principal box (aspect-ratio-052+, sticky
    // multicol). Indefinite multicol inside grid-lanes (gap-002) stays
    // legacy to preserve paired passes.
    if !matches!(style.width, Dimension::Px(_)) || !matches!(style.height, Dimension::Px(_)) {
        return false;
    }
    // Nested multicol stays on legacy for this slice.
    let end = flat[index].subtree_end;
    for node in &flat[index + 1..end] {
        if node.style.display == Display::None {
            continue;
        }
        if node.style.float != Float::None {
            return false;
        }
        if matches!(node.style.position, Position::Absolute | Position::Fixed) {
            return false;
        }
        if establishes(node.style) {
            return false;
        }
    }
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
