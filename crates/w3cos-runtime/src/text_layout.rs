//! Text measurement and wrapping (layout estimates + font-accurate paint).

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;

use w3cos_std::style::{Display, Style, WhiteSpace};

const TEXT_PAINT_CACHE_CAPACITY: usize = 4096;
const FORCED_LINE_BREAK: char = '\u{2028}';

#[derive(Clone, PartialEq, Eq, Hash)]
struct TextPaintKey {
    text: String,
    max_width: u32,
    font: u64,
    font_size: u32,
    white_space: u8,
}

/// Retained shaped text data shared by CPU and GPU painters. Blink keeps the
/// equivalent data in shaped text fragments/display items so scrolling a
/// previously prepared interest rect does not repeat line breaking and ink
/// measurement on the presentation frame.
pub struct TextPaintLayout {
    pub lines: Vec<String>,
    pub ink_bounds: Vec<InkBounds>,
}

pub struct TextPrepaintRequest {
    pub text: String,
    pub width: f32,
    pub font_size: f32,
    pub white_space: WhiteSpace,
}

#[derive(Default)]
struct TextPaintCache {
    entries: HashMap<TextPaintKey, Rc<TextPaintLayout>>,
}

thread_local! {
    static TEXT_PAINT_CACHE: RefCell<TextPaintCache> = RefCell::new(TextPaintCache::default());
}

fn white_space_key(value: WhiteSpace) -> u8 {
    match value {
        WhiteSpace::Normal => 0,
        WhiteSpace::NoWrap => 1,
        WhiteSpace::Pre => 2,
        WhiteSpace::PreWrap => 3,
        WhiteSpace::PreLine => 4,
    }
}

fn normalized_segment_breaks(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\r' => {
                if characters.peek() == Some(&'\n') {
                    characters.next();
                }
                output.push('\n');
            }
            '\u{000c}' => output.push('\n'),
            _ => output.push(character),
        }
    }
    output
}

fn collapse_css_whitespace_sequences(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut pending_space = false;
    for character in text.chars() {
        if matches!(character, ' ' | '\t' | '\n' | '\u{000c}') {
            pending_space = true;
        } else {
            if pending_space {
                output.push(' ');
                pending_space = false;
            }
            output.push(character);
        }
    }
    if pending_space {
        output.push(' ');
    }
    output
}

fn collapse_pre_line_whitespace(text: &str) -> String {
    text.split('\n')
        .map(|line| {
            collapse_css_whitespace_sequences(line)
                .trim_matches(' ')
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn expand_tabs(text: &str) -> String {
    const TAB_SIZE: usize = 8;

    let mut output = String::with_capacity(text.len());
    let mut column = 0usize;
    for character in text.chars() {
        match character {
            '\n' | FORCED_LINE_BREAK => {
                output.push('\n');
                column = 0;
            }
            '\t' => {
                let spaces = TAB_SIZE - column % TAB_SIZE;
                output.extend(std::iter::repeat_n(' ', spaces));
                column += spaces;
            }
            _ => {
                output.push(character);
                column += 1;
            }
        }
    }
    output
}

pub fn prepare_text_for_white_space(text: &str, white_space: WhiteSpace) -> String {
    let text = normalized_segment_breaks(text);
    match white_space {
        WhiteSpace::Normal | WhiteSpace::NoWrap => collapse_css_whitespace_sequences(&text),
        WhiteSpace::PreLine => collapse_pre_line_whitespace(&text),
        WhiteSpace::Pre | WhiteSpace::PreWrap => expand_tabs(&text),
    }
}

/// The break at the end of an inline text fragment terminates its current
/// line, but there is no following fragment with paintable area. Keep the
/// break in shaping/flow while excluding that synthetic empty fragment from
/// the inline box's used block size. Block/pre boxes still retain their final
/// empty line.
pub fn used_text_line_count(text: &str, style: &Style, lines: &[String]) -> usize {
    let terminal_preserved_break = text
        .chars()
        .next_back()
        .is_some_and(|character| matches!(character, '\n' | FORCED_LINE_BREAK));
    if style.display == Display::Inline
        && terminal_preserved_break
        && lines.last().is_some_and(String::is_empty)
    {
        lines.len().saturating_sub(1).max(1)
    } else {
        lines.len().max(1)
    }
}

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

fn wrap_greedy<F>(text: &str, max_width: f32, mut char_width: F) -> Vec<String>
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
        if matches!(ch, '\n' | FORCED_LINE_BREAK) {
            lines.push(std::mem::take(&mut current));
            current_w = 0.0;
            continue;
        }
        let cw = char_width(ch);
        if !current.is_empty() && current_w + cw > max_width {
            if may_not_start_line(ch) {
                // Keep closing punctuation with the preceding character
                // without letting the completed line exceed its paint box.
                // Moving one character is the usual CJK kinsoku fallback.
                if let Some(last) = current.pop() {
                    let last_w = char_width(last);
                    current_w = (current_w - last_w).max(0.0);
                    flush(&mut lines, &mut current, &mut current_w);
                    current.push(last);
                    current_w = last_w;
                }
            } else if current.chars().last().is_some_and(may_not_end_line) {
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
    if !current.is_empty() || text.ends_with('\n') {
        lines.push(current);
    }

    if lines.is_empty() {
        lines.push(String::new());
    }
    merge_orphan_punctuation_lines(&mut lines);
    lines
}

/// Greedy wrapping backed by shaped-run measurement.
///
/// Summing isolated glyph advances loses kerning and shaping adjustments. That
/// is observable when a shrink-to-fit inline box is exactly its max-content
/// width: layout measures the shaped word, while paint used to wrap its final
/// glyph because the isolated advances were wider. Measure each candidate run
/// with the paint backend so line breaking and intrinsic sizing share one
/// metric space.
fn wrap_greedy_with_run_width<F>(text: &str, max_width: f32, mut run_width: F) -> Vec<String>
where
    F: FnMut(&str) -> f32,
{
    if max_width <= 1.0 {
        return vec![text.to_string()];
    }

    let mut lines = Vec::new();
    let mut current = String::new();
    let flush = |lines: &mut Vec<String>, current: &mut String| {
        if !current.is_empty() {
            lines.push(std::mem::take(current));
        }
    };

    for ch in text.chars() {
        if matches!(ch, '\n' | FORCED_LINE_BREAK) {
            lines.push(std::mem::take(&mut current));
            continue;
        }

        let had_content = !current.is_empty();
        current.push(ch);
        if had_content && run_width(&current) > max_width {
            current.pop();
            if may_not_start_line(ch) {
                if let Some(last) = current.pop() {
                    flush(&mut lines, &mut current);
                    current.push(last);
                }
            } else if current.chars().last().is_some_and(may_not_end_line) {
                let last = current.pop().unwrap();
                flush(&mut lines, &mut current);
                current.push(last);
            } else {
                flush(&mut lines, &mut current);
            }
            current.push(ch);
        }
    }
    if !current.is_empty() || text.ends_with('\n') {
        lines.push(current);
    }

    if lines.is_empty() {
        lines.push(String::new());
    }
    merge_orphan_punctuation_lines(&mut lines);
    lines
}

pub fn estimated_char_width(ch: char, font_size: f32) -> f32 {
    if is_bidi_format_control(ch) {
        return 0.0;
    }
    let ch = font_glyph_character(ch);
    if ch == ' ' {
        font_size * 0.33
    } else if ch.is_ascii() {
        font_size * 0.55
    } else {
        font_size * 1.0
    }
}

pub fn char_advance(ch: char, font_size: f32, font: &fontdue::Font) -> f32 {
    if is_bidi_format_control(ch) {
        return 0.0;
    }
    let ch = font_glyph_character(ch);
    if !font.chars().contains_key(&ch) {
        return estimated_char_width(ch, font_size);
    }
    let advance = font.metrics(ch, font_size).advance_width;
    if advance > 0.0 {
        advance
    } else {
        estimated_char_width(ch, font_size)
    }
}

pub(crate) fn font_glyph_character(character: char) -> char {
    if character == '\u{00a0}' {
        ' '
    } else {
        character
    }
}

pub(crate) fn font_render_text(text: &str) -> Cow<'_, str> {
    if text.is_ascii() {
        return Cow::Borrowed(text);
    }

    let bidi = unicode_bidi::BidiInfo::new(text, None);
    let visual = bidi.paragraphs.first().map_or_else(
        || Cow::Borrowed(text),
        |paragraph| bidi.reorder_line(paragraph, paragraph.range.clone()),
    );
    let rendered = visual
        .chars()
        .filter(|character| !is_bidi_format_control(*character))
        .map(font_glyph_character)
        .collect::<String>();
    if rendered == text {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(rendered)
    }
}

pub(crate) fn is_bidi_format_control(character: char) -> bool {
    matches!(
        character,
        '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'
            | '\u{202b}'
            | '\u{202c}'
            | '\u{202d}'
            | '\u{202e}'
            | '\u{2066}'
            | '\u{2067}'
            | '\u{2068}'
            | '\u{2069}'
    )
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
    let lines = wrap_text_with_char_width(text, max_width, white_space, |ch| {
        estimated_char_width(ch, font_size)
    });
    let height = lines.len() as f32 * line_h;
    (lines, height)
}

pub fn wrapped_block_height_estimate(content: &str, width: f32, style: &Style) -> f32 {
    let inner_w = (width - style.padding_lengths().left - style.padding_lengths().right).max(1.0);
    let (lines, _) = wrap_text_estimate(
        content,
        inner_w,
        style.font_size,
        style.line_height,
        style.white_space,
    );
    let h =
        used_text_line_count(content, style, &lines) as f32 * style.font_size * style.line_height;
    h + style.padding_lengths().top + style.padding_lengths().bottom
}

pub fn text_intrinsic_size_estimate(content: &str, style: &Style, wrap_width: f32) -> (f32, f32) {
    let inner_w =
        (wrap_width - style.padding_lengths().left - style.padding_lengths().right).max(1.0);
    let (lines, _) = wrap_text_estimate(
        content,
        inner_w,
        style.font_size,
        style.line_height,
        style.white_space,
    );
    let h =
        used_text_line_count(content, style, &lines) as f32 * style.font_size * style.line_height;
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
    let inner_w =
        (wrap_width - style.padding_lengths().left - style.padding_lengths().right).max(1.0);
    let lines = wrap_text_font(content, inner_w, style.font_size, font, style.white_space);
    let line_h = style.font_size * style.line_height;
    let used_line_count = used_text_line_count(content, style, &lines);
    let h = if used_line_count == 1 {
        single_line_content_height(&lines[0], style.font_size, style.line_height, font)
    } else {
        used_line_count as f32 * line_h
    };
    let max_line_w = lines
        .iter()
        .map(|line| measure_text_width_font(line, style.font_size, font))
        .fold(0.0f32, f32::max);
    let mut width = max_line_w + style.padding_lengths().left + style.padding_lengths().right;
    if let w3cos_std::style::Dimension::Px(min_width) = style.min_width {
        width = width.max(min_width);
    }
    (
        width,
        h + style.padding_lengths().top + style.padding_lengths().bottom,
    )
}

pub fn wrapped_block_height_font(
    content: &str,
    width: f32,
    style: &Style,
    font: &fontdue::Font,
) -> f32 {
    let inner_w = (width - style.padding_lengths().left - style.padding_lengths().right).max(1.0);
    let lines = wrap_text_font(content, inner_w, style.font_size, font, style.white_space);
    let line_h = style.font_size * style.line_height;
    let used_line_count = used_text_line_count(content, style, &lines);
    let block_h = if used_line_count == 1 {
        single_line_content_height(&lines[0], style.font_size, style.line_height, font)
    } else {
        used_line_count as f32 * line_h
    };
    block_h + style.padding_lengths().top + style.padding_lengths().bottom
}

/// Top/bottom extents relative to baseline at y = 0 (same coords as [`draw_text_line`]).
pub fn single_line_vertical_metrics(
    text: &str,
    font_size: f32,
    font: &fontdue::Font,
) -> (f32, f32) {
    let mut top = f32::MAX;
    let mut bottom = f32::MIN;
    for character in text.chars() {
        if is_bidi_format_control(character) {
            continue;
        }
        let ch = font_glyph_character(character);
        if !font.chars().contains_key(&ch) {
            top = top.min(-font_size * 0.88);
            bottom = bottom.max(font_size * 0.12);
            continue;
        }
        let m = font.metrics(ch, font_size);
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

pub fn single_line_content_height(
    text: &str,
    font_size: f32,
    line_height: f32,
    font: &fontdue::Font,
) -> f32 {
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

    for character in text.chars() {
        if is_bidi_format_control(character) {
            continue;
        }
        let ch = font_glyph_character(character);
        if !font.chars().contains_key(&ch) {
            let advance = estimated_char_width(ch, font_size);
            saw_ink = true;
            left = left.min(cursor_x);
            top = top.min(origin_y);
            right = right.max(cursor_x + advance);
            bottom = bottom.max(origin_y + font_size);
            cursor_x += advance;
            continue;
        }
        let metrics = font.metrics(ch, font_size);
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
    wrap_text_with_char_width(text, max_width, white_space, |ch| {
        char_advance(ch, font_size, font)
    })
}

pub fn wrap_text_with_char_width(
    text: &str,
    max_width: f32,
    white_space: WhiteSpace,
    char_width: impl FnMut(char) -> f32,
) -> Vec<String> {
    let text = prepare_text_for_white_space(text, white_space);
    if white_space == WhiteSpace::NoWrap {
        return text.split(FORCED_LINE_BREAK).map(str::to_string).collect();
    }
    if white_space == WhiteSpace::Pre {
        return text
            .split(['\n', FORCED_LINE_BREAK])
            .map(str::to_string)
            .collect();
    }
    if max_width <= 1.0 {
        return text
            .split(['\n', FORCED_LINE_BREAK])
            .map(str::to_string)
            .collect();
    }
    wrap_greedy(&text, max_width, char_width)
}

pub fn wrap_text_with_run_width(
    text: &str,
    max_width: f32,
    white_space: WhiteSpace,
    run_width: impl FnMut(&str) -> f32,
) -> Vec<String> {
    let text = prepare_text_for_white_space(text, white_space);
    if white_space == WhiteSpace::NoWrap {
        return text.split(FORCED_LINE_BREAK).map(str::to_string).collect();
    }
    if white_space == WhiteSpace::Pre {
        return text
            .split(['\n', FORCED_LINE_BREAK])
            .map(str::to_string)
            .collect();
    }
    if max_width <= 1.0 {
        return text
            .split(['\n', FORCED_LINE_BREAK])
            .map(str::to_string)
            .collect();
    }
    wrap_greedy_with_run_width(&text, max_width, run_width)
}

pub fn retained_text_paint_layout(
    text: &str,
    max_width: f32,
    font_size: f32,
    font: &fontdue::Font,
    white_space: WhiteSpace,
) -> Rc<TextPaintLayout> {
    let mut font_hasher = DefaultHasher::new();
    font.hash(&mut font_hasher);
    retained_text_paint_layout_with(
        text,
        max_width,
        font_size,
        white_space,
        font_hasher.finish(),
        |character| char_advance(character, font_size, font),
        |line| measure_text_ink_bounds(line, font_size, font, 0.0, 0.0),
    )
}

pub fn retained_text_paint_layout_with(
    text: &str,
    max_width: f32,
    font_size: f32,
    white_space: WhiteSpace,
    font_identity: u64,
    char_width: impl FnMut(char) -> f32,
    mut measure_ink: impl FnMut(&str) -> InkBounds,
) -> Rc<TextPaintLayout> {
    let key = TextPaintKey {
        text: text.to_owned(),
        max_width: max_width.to_bits(),
        font: font_identity,
        font_size: font_size.to_bits(),
        white_space: white_space_key(white_space),
    };
    if let Some(cached) = TEXT_PAINT_CACHE.with(|cache| cache.borrow().entries.get(&key).cloned()) {
        return cached;
    }

    let lines = wrap_text_with_char_width(text, max_width, white_space, char_width);
    let ink_bounds = lines.iter().map(|line| measure_ink(line)).collect();
    let layout = Rc::new(TextPaintLayout { lines, ink_bounds });
    TEXT_PAINT_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.entries.len() >= TEXT_PAINT_CACHE_CAPACITY {
            cache.entries.clear();
        }
        cache.entries.insert(key, layout.clone());
    });
    layout
}

pub fn retained_text_paint_layout_with_run_width(
    text: &str,
    max_width: f32,
    font_size: f32,
    white_space: WhiteSpace,
    font_identity: u64,
    run_width: impl FnMut(&str) -> f32,
    mut measure_ink: impl FnMut(&str) -> InkBounds,
) -> Rc<TextPaintLayout> {
    let key = TextPaintKey {
        text: text.to_owned(),
        max_width: max_width.to_bits(),
        font: font_identity,
        font_size: font_size.to_bits(),
        white_space: white_space_key(white_space),
    };
    if let Some(cached) = TEXT_PAINT_CACHE.with(|cache| cache.borrow().entries.get(&key).cloned()) {
        return cached;
    }

    let lines = wrap_text_with_run_width(text, max_width, white_space, run_width);
    let ink_bounds = lines.iter().map(|line| measure_ink(line)).collect();
    let layout = Rc::new(TextPaintLayout { lines, ink_bounds });
    TEXT_PAINT_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.entries.len() >= TEXT_PAINT_CACHE_CAPACITY {
            cache.entries.clear();
        }
        cache.entries.insert(key, layout.clone());
    });
    layout
}

pub fn prepaint_text_interest_rect(
    requests: &[TextPrepaintRequest],
    font: &fontdue::Font,
    budget: std::time::Duration,
) -> usize {
    let started = std::time::Instant::now();
    let mut prepared = 0;
    for request in requests {
        if prepared > 0 && started.elapsed() >= budget {
            break;
        }
        retained_text_paint_layout(
            &request.text,
            request.width.max(1.0),
            request.font_size,
            font,
            request.white_space,
        );
        prepared += 1;
    }
    prepared
}

pub fn clear_paint_cache() {
    TEXT_PAINT_CACHE.with(|cache| cache.borrow_mut().entries.clear());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_paint_layout_reuses_shaping_for_same_interest_item() {
        TEXT_PAINT_CACHE.with(|cache| cache.borrow_mut().entries.clear());
        let font = fontdue::Font::from_bytes(
            include_bytes!("../assets/Inter-Regular.ttf") as &[u8],
            fontdue::FontSettings::default(),
        )
        .unwrap();
        let first =
            retained_text_paint_layout("prepared text", 180.0, 16.0, &font, WhiteSpace::Normal);
        let second =
            retained_text_paint_layout("prepared text", 180.0, 16.0, &font, WhiteSpace::Normal);
        assert!(Rc::ptr_eq(&first, &second));
    }

    #[test]
    fn cjk_estimate_uses_chars_not_bytes() {
        let w_byte_guess = "中文".len() as f32 * 12.0;
        let w = measure_text_width_estimate("中文", 12.0);
        assert!(w < w_byte_guess);
        assert!((w - 24.0).abs() < 0.1);
    }

    #[test]
    fn compact_metrics_face_uses_one_em_for_missing_cjk() {
        let font = fontdue::Font::from_bytes(
            include_bytes!("../assets/Inter-Regular.ttf") as &[u8],
            fontdue::FontSettings::default(),
        )
        .unwrap();
        assert!(!font.chars().contains_key(&'中'));
        assert!((char_advance('中', 16.0, &font) - 16.0).abs() < 0.1);
        let ink = measure_text_ink_bounds("中", 16.0, &font, 0.0, 0.0);
        assert!((ink.width - 16.0).abs() < 0.1);
        assert!((ink.height - 16.0).abs() < 0.1);
    }

    #[test]
    fn cjk_closing_punctuation_stays_inside_wrap_width() {
        let max_width = 4.0;
        let lines = wrap_greedy("甲乙丙丁。戊", max_width, |_| 1.0);
        assert_eq!(lines, vec!["甲乙丙", "丁。戊"]);
        assert!(
            lines
                .iter()
                .all(|line| line.chars().count() as f32 <= max_width)
        );
    }

    #[test]
    fn shaped_run_width_keeps_kerning_pair_inside_its_intrinsic_box() {
        let isolated = wrap_text_with_char_width("PASS", 3.5, WhiteSpace::Normal, |_| 1.0);
        assert_eq!(isolated, vec!["PAS", "S"]);

        let shaped = wrap_text_with_run_width("PASS", 3.5, WhiteSpace::Normal, |text| {
            text.chars().count() as f32 - text.chars().count().saturating_sub(1) as f32 * 0.2
        });
        assert_eq!(shaped, vec!["PASS"]);
    }

    #[test]
    fn css_white_space_modes_prepare_generated_text_before_shaping() {
        let source = "This text\n\tshould   be";
        assert_eq!(
            prepare_text_for_white_space(source, WhiteSpace::Normal),
            "This text should be"
        );
        assert_eq!(
            prepare_text_for_white_space(source, WhiteSpace::NoWrap),
            "This text should be"
        );
        assert_eq!(
            prepare_text_for_white_space(source, WhiteSpace::PreLine),
            "This text\nshould be"
        );
        assert_eq!(
            prepare_text_for_white_space(source, WhiteSpace::Pre),
            "This text\n        should   be"
        );
    }

    #[test]
    fn non_breaking_space_uses_the_regular_space_glyph_advance() {
        let font = fontdue::Font::from_bytes(
            include_bytes!("../assets/Inter-Regular.ttf") as &[u8],
            fontdue::FontSettings::default(),
        )
        .unwrap();

        assert_eq!(
            prepare_text_for_white_space("left\u{00a0}right", WhiteSpace::Normal),
            "left\u{00a0}right",
            "NBSP must retain its no-break identity during line breaking"
        );
        assert!(
            (char_advance('\u{00a0}', 16.0, &font) - char_advance(' ', 16.0, &font)).abs() < 0.01,
            "NBSP must paint with the regular space glyph advance"
        );
    }

    #[test]
    fn explicit_bidi_overrides_are_reordered_before_font_rendering() {
        assert_eq!(font_render_text("\u{202e}elbadaer"), "readable");
        assert_eq!(font_render_text("\u{202e}d c \u{202d}ab\u{202c}"), "ab c d");
    }

    #[test]
    fn pre_preserves_explicit_and_empty_lines_without_soft_wrapping() {
        let lines = wrap_text_with_run_width("first\n\nsecond", 1.0, WhiteSpace::Pre, |_| 100.0);
        assert_eq!(lines, vec!["first", "", "second"]);
    }

    #[test]
    fn terminal_break_does_not_create_a_painted_empty_inline_fragment() {
        let content = "  \n";
        let lines = wrap_text_with_run_width(content, 100.0, WhiteSpace::Pre, |_| 8.0);
        assert_eq!(lines, vec!["  ", ""]);
        assert_eq!(
            used_text_line_count(
                content,
                &Style {
                    display: Display::Inline,
                    white_space: WhiteSpace::Pre,
                    ..Style::default()
                },
                &lines,
            ),
            1
        );
        assert_eq!(
            used_text_line_count(
                content,
                &Style {
                    display: Display::Block,
                    white_space: WhiteSpace::Pre,
                    ..Style::default()
                },
                &lines,
            ),
            2,
            "block preformatted content retains its terminal empty line"
        );
    }

    #[test]
    fn forced_break_survives_normal_and_nowrap_whitespace_modes() {
        for white_space in [WhiteSpace::Normal, WhiteSpace::NoWrap] {
            let lines =
                wrap_text_with_run_width("first\u{2028}second", 1000.0, white_space, |text| {
                    text.len() as f32
                });
            assert_eq!(lines, vec!["first", "second"]);
        }
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

    #[test]
    fn embedded_font_covers_common_simplified_chinese_input() {
        let data = include_bytes!("../assets/CJK-Subset.ttf");
        let font =
            fontdue::Font::from_bytes(data as &[u8], fontdue::FontSettings::default()).unwrap();

        for ch in "我说的是啥的都是和聊天候选输入法上海→杭州★▼×".chars()
        {
            assert_ne!(font.lookup_glyph_index(ch), 0, "missing glyph for {ch}");
        }
    }
}
