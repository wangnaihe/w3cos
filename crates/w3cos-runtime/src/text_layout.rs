//! Text measurement and wrapping (layout estimates + font-accurate paint).

use w3cos_std::style::{Style, WhiteSpace};

/// Characters that should not begin a new line (CJK punctuation rules).
fn may_not_start_line(ch: char) -> bool {
    matches!(
        ch,
        '。' | '，'
            | '、'
            | '；'
            | '：'
            | '？'
            | '！'
            | '.'
            | ','
            | ';'
            | ':'
            | '?'
            | '!'
            | ')'
            | '）'
            | '」'
            | '』'
            | '》'
            | '】'
            | '％'
            | '%'
            | '…'
    )
}

/// Characters that should not end a line.
fn may_not_end_line(ch: char) -> bool {
    matches!(
        ch,
        '(' | '（' | '「' | '『' | '《' | '【' | '￥' | '$' | '£'
    )
}

fn is_orphan_punctuation_line(line: &str) -> bool {
    let t = line.trim();
    !t.is_empty() && t.chars().all(may_not_start_line)
}

fn merge_orphan_punctuation_lines(lines: &mut Vec<String>) {
    let mut i = 1;
    while i < lines.len() {
        if is_orphan_punctuation_line(&lines[i]) {
            let tail = lines[i].clone();
            lines[i - 1].push_str(&tail);
            lines.remove(i);
        } else {
            i += 1;
        }
    }
}

fn wrap_greedy<F>(
    text: &str,
    max_width: f32,
    mut char_width: F,
) -> Vec<String>
where
    F: FnMut(char) -> f32,
{
    if max_width <= 1.0 {
        return vec![text.to_string()];
    }

    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_w = 0.0f32;

    let mut flush = |lines: &mut Vec<String>, current: &mut String, current_w: &mut f32| {
        if !current.is_empty() {
            lines.push(std::mem::take(current));
            *current_w = 0.0;
        }
    };

    for ch in text.chars() {
        if ch == '\n' {
            flush(&mut lines, &mut current, &mut current_w);
            continue;
        }
        let cw = char_width(ch);
        if !current.is_empty() && current_w + cw > max_width {
            if may_not_start_line(ch) {
                current.push(ch);
                current_w += cw;
                continue;
            }
            if current
                .chars()
                .last()
                .is_some_and(may_not_end_line)
            {
                let last = current.pop().unwrap();
                flush(&mut lines, &mut current, &mut current_w);
                current.push(last);
                current_w = char_width(last);
            } else {
                flush(&mut lines, &mut current, &mut current_w);
            }
        }
        current.push(ch);
        current_w += cw;
    }
    flush(&mut lines, &mut current, &mut current_w);

    if lines.is_empty() {
        lines.push(String::new());
    }
    merge_orphan_punctuation_lines(&mut lines);
    lines
}

pub fn estimated_char_width(ch: char, font_size: f32) -> f32 {
    if ch == ' ' {
        font_size * 0.33
    } else if ch.is_ascii() {
        font_size * 0.55
    } else {
        font_size * 1.0
    }
}

pub fn char_advance(ch: char, font_size: f32, font: &fontdue::Font) -> f32 {
    let advance = font.rasterize(ch, font_size).0.advance_width;
    if advance > 0.0 {
        advance
    } else {
        estimated_char_width(ch, font_size)
    }
}

pub fn measure_text_width_estimate(text: &str, font_size: f32) -> f32 {
    text.chars()
        .map(|ch| estimated_char_width(ch, font_size))
        .sum()
}

/// Greedy wrap for layout (no font required).
pub fn wrap_text_estimate(
    text: &str,
    max_width: f32,
    font_size: f32,
    line_height: f32,
    white_space: WhiteSpace,
) -> (Vec<String>, f32) {
    let line_h = font_size * line_height;
    if matches!(white_space, WhiteSpace::NoWrap | WhiteSpace::Pre) {
        return (vec![text.to_string()], line_h);
    }
    if max_width <= 1.0 {
        return (vec![text.to_string()], line_h);
    }

    let lines = wrap_greedy(text, max_width, |ch| estimated_char_width(ch, font_size));
    let height = lines.len() as f32 * line_h;
    (lines, height)
}

pub fn wrapped_block_height_estimate(content: &str, width: f32, style: &Style) -> f32 {
    let inner_w = (width - style.padding_lengths().left - style.padding_lengths().right).max(1.0);
    let (_, h) = wrap_text_estimate(
        content,
        inner_w,
        style.font_size,
        style.line_height,
        style.white_space,
    );
    h + style.padding_lengths().top + style.padding_lengths().bottom
}

pub fn text_intrinsic_size_estimate(content: &str, style: &Style, wrap_width: f32) -> (f32, f32) {
    if matches!(style.white_space, WhiteSpace::NoWrap | WhiteSpace::Pre) {
        let w = measure_text_width_estimate(content, style.font_size)
            + style.padding_lengths().left
            + style.padding_lengths().right;
        let h = style.font_size * style.line_height + style.padding_lengths().top + style.padding_lengths().bottom;
        return (w, h);
    }
    let inner_w = (wrap_width - style.padding_lengths().left - style.padding_lengths().right).max(1.0);
    let (lines, h) = wrap_text_estimate(
        content,
        inner_w,
        style.font_size,
        style.line_height,
        style.white_space,
    );
    let max_line_w = lines
        .iter()
        .map(|line| measure_text_width_estimate(line, style.font_size))
        .fold(0.0f32, f32::max);
    (
        max_line_w + style.padding_lengths().left + style.padding_lengths().right,
        h + style.padding_lengths().top + style.padding_lengths().bottom,
    )
}

/// Font-accurate intrinsic size — must match paint-time metrics for layout/paint parity.
pub fn text_intrinsic_size_font(
    content: &str,
    style: &Style,
    wrap_width: f32,
    font: &fontdue::Font,
) -> (f32, f32) {
    if matches!(style.white_space, WhiteSpace::NoWrap | WhiteSpace::Pre) {
        let mut w = measure_text_width_font(content, style.font_size, font)
            + style.padding_lengths().left
            + style.padding_lengths().right;
        if let w3cos_std::style::Dimension::Px(mw) = style.min_width {
            w = w.max(mw);
        }
        let h = single_line_content_height(content, style.font_size, style.line_height, font)
            + style.padding_lengths().top
            + style.padding_lengths().bottom;
        return (w, h);
    }
    let inner_w = (wrap_width - style.padding_lengths().left - style.padding_lengths().right).max(1.0);
    let lines = wrap_text_font(
        content,
        inner_w,
        style.font_size,
        font,
        style.white_space,
    );
    let line_h = style.font_size * style.line_height;
    let h = if lines.len() == 1 {
        single_line_content_height(&lines[0], style.font_size, style.line_height, font)
    } else {
        lines.len() as f32 * line_h
    };
    let max_line_w = lines
        .iter()
        .map(|line| measure_text_width_font(line, style.font_size, font))
        .fold(0.0f32, f32::max);
    (
        max_line_w + style.padding_lengths().left + style.padding_lengths().right,
        h + style.padding_lengths().top + style.padding_lengths().bottom,
    )
}

pub fn wrapped_block_height_font(content: &str, width: f32, style: &Style, font: &fontdue::Font) -> f32 {
    let inner_w = (width - style.padding_lengths().left - style.padding_lengths().right).max(1.0);
    let lines = wrap_text_font(
        content,
        inner_w,
        style.font_size,
        font,
        style.white_space,
    );
    let line_h = style.font_size * style.line_height;
    let block_h = if lines.len() == 1 {
        single_line_content_height(&lines[0], style.font_size, style.line_height, font)
    } else {
        lines.len() as f32 * line_h
    };
    block_h + style.padding_lengths().top + style.padding_lengths().bottom
}

/// Top/bottom extents relative to baseline at y = 0 (same coords as [`draw_text_line`]).
pub fn single_line_vertical_metrics(text: &str, font_size: f32, font: &fontdue::Font) -> (f32, f32) {
    let mut top = f32::MAX;
    let mut bottom = f32::MIN;
    for ch in text.chars() {
        let (m, _) = font.rasterize(ch, font_size);
        if m.width == 0 && m.height == 0 {
            continue;
        }
        let char_top = -(m.height as f32) - m.ymin as f32;
        let char_bottom = -m.ymin as f32;
        top = top.min(char_top);
        bottom = bottom.max(char_bottom);
    }
    if top == f32::MAX {
        (-font_size, font_size * 0.2)
    } else {
        (top, bottom)
    }
}

/// `y` argument for [`draw_text_line`] so glyphs are vertically centered in `box_height`.
pub fn y_for_draw_text_line_centered(
    text: &str,
    font_size: f32,
    font: &fontdue::Font,
    box_top: f32,
    box_height: f32,
) -> f32 {
    let (top, bottom) = single_line_vertical_metrics(text, font_size, font);
    let text_h = (bottom - top).max(1.0);
    let baseline = box_top + (box_height - text_h) * 0.5 - top;
    baseline - font_size
}

pub fn single_line_content_height(text: &str, font_size: f32, line_height: f32, font: &fontdue::Font) -> f32 {
    let (top, bottom) = single_line_vertical_metrics(text, font_size, font);
    let visual = bottom - top;
    visual.max(font_size * line_height)
}

pub fn measure_text_width_font(text: &str, font_size: f32, font: &fontdue::Font) -> f32 {
    text.chars()
        .map(|ch| char_advance(ch, font_size, font))
        .sum()
}

/// Pixel origin for a glyph — shared by ink measurement and CPU paint.
pub fn glyph_pixel_origin(cursor_x: f32, cursor_y: f32, metrics: &fontdue::Metrics) -> (i32, i32) {
    let gx = cursor_x.round() as i32;
    let gy = (cursor_y - metrics.height as f32 - metrics.ymin as f32).round() as i32;
    (gx, gy)
}

/// Visual ink bounds when drawn with [`draw_text_line`] at `(origin_x, origin_y)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InkBounds {
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
}

impl InkBounds {
    pub fn empty() -> Self {
        Self {
            left: 0.0,
            top: 0.0,
            width: 0.0,
            height: 0.0,
        }
    }
}

/// Same placement rules as [`crate::render_cpu::draw_text_line`].
pub fn measure_text_ink_bounds(
    text: &str,
    font_size: f32,
    font: &fontdue::Font,
    origin_x: f32,
    origin_y: f32,
) -> InkBounds {
    let mut cursor_x = origin_x;
    let cursor_y = origin_y + font_size;
    let mut left = f32::MAX;
    let mut top = f32::MAX;
    let mut right = f32::MIN;
    let mut bottom = f32::MIN;
    let mut saw_ink = false;

    for ch in text.chars() {
        let (metrics, _) = font.rasterize(ch, font_size);
        let advance = if metrics.advance_width > 0.0 {
            metrics.advance_width
        } else {
            estimated_char_width(ch, font_size)
        };
        if metrics.width == 0 || metrics.height == 0 {
            cursor_x += advance;
            continue;
        }

        saw_ink = true;
        let (gx, gy) = glyph_pixel_origin(cursor_x, cursor_y, &metrics);
        let gx = gx as f32;
        let gy = gy as f32;
        left = left.min(gx);
        top = top.min(gy);
        right = right.max(gx + metrics.width as f32);
        bottom = bottom.max(gy + metrics.height as f32);
        cursor_x += advance;
    }

    if !saw_ink {
        return InkBounds::empty();
    }

    InkBounds {
        left,
        top,
        width: (right - left).max(0.0),
        height: (bottom - top).max(0.0),
    }
}

pub fn wrap_text_font(
    text: &str,
    max_width: f32,
    font_size: f32,
    font: &fontdue::Font,
    white_space: WhiteSpace,
) -> Vec<String> {
    if matches!(white_space, WhiteSpace::NoWrap | WhiteSpace::Pre) {
        return vec![text.to_string()];
    }
    if max_width <= 1.0 {
        return vec![text.to_string()];
    }

    let lines = wrap_greedy(text, max_width, |ch| char_advance(ch, font_size, font));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cjk_estimate_uses_chars_not_bytes() {
        let w_byte_guess = "中文".len() as f32 * 12.0;
        let w = measure_text_width_estimate("中文", 12.0);
        assert!(w < w_byte_guess);
        assert!((w - 24.0).abs() < 0.1);
    }

    #[test]
    fn vertical_metrics_orders_top_bottom() {
        let data = include_bytes!("../assets/CJK-Subset.ttf");
        let font =
            fontdue::Font::from_bytes(data as &[u8], fontdue::FontSettings::default()).unwrap();
        let (top, bottom) = single_line_vertical_metrics("AI", 12.0, &font);
        assert!(bottom > top);
        let y = y_for_draw_text_line_centered("AI", 12.0, &font, 0.0, 18.0);
        assert!(y.is_finite());
    }

    #[test]
    fn ink_bounds_centered_in_box() {
        let data = include_bytes!("../assets/CJK-Subset.ttf");
        let font =
            fontdue::Font::from_bytes(data as &[u8], fontdue::FontSettings::default()).unwrap();
        let ink = measure_text_ink_bounds("发", 14.0, &font, 0.0, 0.0);
        assert!(ink.width > 0.0);
        assert!(ink.height > 0.0);
        let box_top = 10.0;
        let box_h = 40.0;
        let y = box_top + (box_h - ink.height) * 0.5 - ink.top;
        let ink_after = measure_text_ink_bounds("发", 14.0, &font, -ink.left, y);
        let center_y = box_top + box_h * 0.5;
        let ink_center_y = ink_after.top + ink_after.height * 0.5;
        assert!((ink_center_y - center_y).abs() < 0.6);
    }
}
