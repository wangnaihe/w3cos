//! Inline formatting context, stage S2a.
//!
//! Structure from Blink LayoutNG inline layout
//! (third_party/blink/renderer/core/layout/inline/, Copyright The Chromium
//! Authors, BSD-style license):
//!
//! * `NGInlineNode::CollectInlines`: the container's text leaves become a
//!   flat item list ([`InlineItem`]); shaping is the engine's existing
//!   `text_intrinsic_size` (Skia/fontdue, advances already snapped to
//!   LayoutUnit like `ShapeResult::SnappedWidth`).
//! * `NGLineBreaker`: items are placed while they fit; a trailing
//!   collapsible space hangs (CSS Text 3 4.1.3) and never causes a break;
//!   a break moves back to the last soft-wrap opportunity on the line.
//! * `NGInlineLayoutAlgorithm` / `NGLineHeightMetrics`: the line box is the
//!   union of the root inline box strut and every item's
//!   ascent/descent, each with its half-leading split as in
//!   `CalculateLeadingSpace` (upper half floored); line heights are
//!   LayoutUnits.
//! * `NGLineBoxFragmentBuilder` + text-align: the line is offset by the
//!   free space (hanging spaces excluded); an overflowing line is
//!   start-aligned.
//!
//! The paint contract is the engine's: a word-split item paints its text at
//! its rect (font content box), while a paragraph leaf (a block-level text
//! leaf, or the single `text-line-width` leaf of an IFC) is wrapped by paint
//! inside its rect, so its height comes from the same wrapper paint uses.

use super::BoxFragment;
use crate::layout::{
    FlatNodeInfo, inline_font_baseline_from_line_top, inline_font_content_ascent,
    inline_font_height, inline_style_line_height, inline_used_line_height, text_intrinsic_size,
    wrapped_text_height,
};
use w3cos_std::ComponentKind;
use w3cos_std::style::{
    Dimension, Display, Float, Position, Style, TextAlign, TextDirection, WhiteSpace,
};

const EPSILON: f32 = 1.0 / 128.0;

fn has_marker(style: &Style, key: &str) -> bool {
    style
        .custom_properties
        .as_ref()
        .is_some_and(|props| props.contains_key(key))
}

pub(super) const IFC_MARKER: &str = "--w3cos-internal-inline-formatting-context";
pub(super) const LINE_WIDTH_MARKER: &str = "--w3cos-internal-text-line-width";

fn zero_edges(style: &Style) -> bool {
    let padding = style.padding_lengths();
    let margin = style.margin_lengths();
    [padding.top, padding.right, padding.bottom, padding.left]
        .into_iter()
        .all(|v| v == 0.0)
        && [margin.left, margin.right].into_iter().all(|v| v == 0.0)
        && style.border_width == 0.0
        && [
            style.border_top_width,
            style.border_right_width,
            style.border_bottom_width,
            style.border_left_width,
        ]
        .into_iter()
        .flatten()
        .all(|v| v == 0.0)
}

fn is_soft_wrap_char(c: char) -> bool {
    matches!(c, ' ' | '-' | '\u{2010}' | '\u{00AD}' | '\u{200B}')
        || crate::text_layout::is_cjk_line_break_character(c)
}

/// Text style requirements shared by every S2a text leaf.
fn text_style_reason(style: &Style) -> Option<&'static str> {
    if style.position != Position::Static || style.float != Float::None {
        return Some("text:positioned");
    }
    if style.direction != TextDirection::Ltr {
        return Some("direction");
    }
    if !matches!(style.white_space, WhiteSpace::Normal | WhiteSpace::NoWrap) {
        return Some("text:white-space");
    }
    if style.text_indent != Dimension::Px(0.0) {
        return Some("text:indent");
    }
    if style.text_align == TextAlign::Justify {
        return Some("text:justify");
    }
    if crate::list_marker::inside_advance(style).is_some() {
        return Some("text:list-marker");
    }
    None
}

/// S2a measures with the primary font only; other scripts (font fallback,
/// bidi, CJK/Thai breaking) stay on the legacy path for now.
fn latin1(content: &str) -> bool {
    content
        .chars()
        .all(|c| (c as u32) < 0x100 || matches!(c, '\u{2028}' | '\u{2029}'))
}

/// A block-level text leaf (`<p>text</p>` lowered to one Text box).  Its
/// block-axis margins are handled by block layout.
pub(super) fn block_text_reason(content: &str, style: &Style) -> Option<&'static str> {
    let padding = style.padding_lengths();
    if [padding.top, padding.right, padding.bottom, padding.left]
        .into_iter()
        .any(|v| v != 0.0)
        || style.border_width != 0.0
        || [
            style.border_top_width,
            style.border_right_width,
            style.border_bottom_width,
            style.border_left_width,
        ]
        .into_iter()
        .flatten()
        .any(|v| v != 0.0)
    {
        return Some("text:edges");
    }
    if content.is_empty() || content.contains(['\n', '\r', '\t']) {
        return Some("text:content");
    }
    if !latin1(content) {
        return Some("text:script");
    }
    text_style_reason(style)
}

/// An inline text leaf inside an IFC container.
fn inline_text_reason(content: &str, style: &Style, sole: bool) -> Option<&'static str> {
    if style.display != Display::Inline {
        return Some("text:display");
    }
    if !zero_edges(style) {
        return Some("text:edges");
    }
    if content.is_empty() || content.contains(['\n', '\r', '\t']) {
        return Some("text:content");
    }
    if !latin1(content) {
        return Some("text:script");
    }
    if has_marker(style, LINE_WIDTH_MARKER) {
        if !sole {
            return Some("text:line-width-shared");
        }
    } else {
        // Word-split items must not need an internal break.
        let core = content.trim_matches(' ');
        if core
            .chars()
            .any(|c| is_soft_wrap_char(c) || matches!(c, '\u{2028}' | '\u{2029}'))
        {
            return Some("text:internal-break");
        }
    }
    text_style_reason(style)
}

fn rendered_children<'a>(
    flat: &'a [FlatNodeInfo<'a>],
    index: usize,
) -> impl Iterator<Item = usize> + 'a {
    let end = flat[index].subtree_end;
    let mut child = index + 1;
    std::iter::from_fn(move || {
        while child < end {
            let current = child;
            child = flat[current].subtree_end.max(current + 1);
            if flat[current].style.display != Display::None {
                return Some(current);
            }
        }
        None
    })
}

/// True when the block container's rendered children are inline text.
pub(super) fn is_inline_container(flat: &[FlatNodeInfo], index: usize) -> bool {
    rendered_children(flat, index).next().is_some_and(|child| {
        matches!(flat[child].kind, ComponentKind::Text { .. })
            && flat[child].style.display == Display::Inline
    })
}

/// Why this IFC container (and its items) is outside S2a.
pub(super) fn container_reason(flat: &[FlatNodeInfo], index: usize) -> Option<&'static str> {
    let style = flat[index].style;
    if style.text_align == TextAlign::Justify {
        return Some("text:justify");
    }
    if style.text_indent != Dimension::Px(0.0) {
        return Some("text:indent");
    }
    if !matches!(style.white_space, WhiteSpace::Normal | WhiteSpace::NoWrap) {
        return Some("text:white-space");
    }
    let children: Vec<usize> = rendered_children(flat, index).collect();
    let sole = children.len() == 1;
    for child in children {
        let info = &flat[child];
        let ComponentKind::Text { content } = info.kind else {
            return Some("ifc:atomic-or-span");
        };
        if info.subtree_end > child + 1 {
            return Some("ifc:atomic-or-span");
        }
        if info.style.white_space != style.white_space {
            return Some("text:white-space");
        }
        if let Some(reason) = inline_text_reason(content, info.style, sole) {
            return Some(reason);
        }
    }
    None
}

/// Item metrics as the legacy line-metrics pass reads them, so both engines
/// paint glyphs on the same baseline: (line-height box height, baseline
/// offset from its top).
fn text_line_metrics(content: &str, style: &Style) -> (f32, f32) {
    let height = inline_used_line_height(inline_style_line_height(style));
    let baseline = inline_font_baseline_from_line_top(style, height);
    #[cfg(feature = "skia")]
    if style.line_height_is_normal
        && let Some(metrics) = crate::render_skia::resolved_text_font_geometry(content, style)
    {
        return (metrics.line_spacing(), metrics.ascent);
    }
    let _ = content;
    (height, baseline)
}

/// NGLineHeightMetrics of the root inline box (the block's strut).
fn strut(style: &Style) -> (f32, f32) {
    let height = inline_used_line_height(inline_style_line_height(style));
    let ascent = inline_font_baseline_from_line_top(style, height);
    (ascent, height - ascent)
}

#[derive(Debug)]
struct InlineItem {
    index: usize,
    /// Raw advance in its run's shaping context (unsnapped).
    raw: f32,
    /// Raw advance without trailing collapsible spaces (they hang).
    raw_hang: f32,
    /// Items of one authored text run (NGInlineItem) share an id: a line
    /// slice of a run occupies the LayoutUnit ceiling of its raw advance.
    run: usize,
    starts_space: bool,
    ends_space: bool,
    whitespace_only: bool,
    ascent: f32,
    descent: f32,
    content_ascent: f32,
    font_height: f32,
}

/// ShapeResult::SnappedWidth.
fn snap(advance: f32) -> f32 {
    crate::text_layout::inline_layout_advance(advance)
}

/// Shaping identity of a word piece: pieces of one authored text node with
/// the same font are shaped as one run.
fn same_run(a: &Style, b: &Style) -> bool {
    let source = |s: &Style| {
        s.custom_properties
            .as_ref()
            .and_then(|p| p.get(w3cos_std::inline_text::SOURCE_RUN))
            .cloned()
    };
    source(a).is_some()
        && source(a) == source(b)
        && a.font_family == b.font_family
        && a.font_size == b.font_size
        && a.font_weight == b.font_weight
        && a.font_style == b.font_style
        && a.font_variant == b.font_variant
        && a.letter_spacing == b.letter_spacing
        && a.word_spacing == b.word_spacing
}

/// Raw in-context advances of one run's pieces (Skia shapes the run once);
/// the non-Skia backend measures each piece.
fn run_advances(texts: &[&str], style: &Style) -> Vec<f32> {
    #[cfg(feature = "skia")]
    if texts.len() > 1
        && let Some(advances) =
            crate::render_skia::measure_skia_inline_fragment_advances(texts, style)
        && advances.len() == texts.len()
    {
        return advances;
    }
    texts
        .iter()
        .map(|t| text_intrinsic_size(t, style).0)
        .collect()
}

/// Raw advance of `content` without its trailing spaces.
fn hang_advance(content: &str, raw: f32, style: &Style) -> f32 {
    let trimmed = content.trim_end_matches(' ');
    if trimmed.is_empty() {
        return 0.0;
    }
    if trimmed.len() == content.len() {
        return raw;
    }
    let spaces = &content[trimmed.len()..];
    let parts = run_advances(&[trimmed, spaces], style);
    (raw - parts[1]).max(0.0)
}

/// LayoutUnit width of a line holding `line` (item indices in order); the
/// last item's trailing spaces hang.
fn line_width(items: &[InlineItem], line: &[usize]) -> f32 {
    let mut width = 0.0f32;
    let mut slice = 0.0f32;
    let mut run = usize::MAX;
    for (position, &k) in line.iter().enumerate() {
        let item = &items[k];
        if item.run != run {
            width += snap(slice);
            slice = 0.0;
            run = item.run;
        }
        slice += if position + 1 == line.len() {
            item.raw_hang
        } else {
            item.raw
        };
    }
    width + snap(slice)
}

/// One laid-out line: items with their inline offsets and widths.
struct Line {
    items: Vec<(usize, f32, f32)>,
    width: f32,
    ascent: f32,
    descent: f32,
}

fn align_offset(style: &Style, free: f32) -> f32 {
    let free = free.max(0.0);
    match style.text_align {
        TextAlign::Center => w3cos_std::style::used_box_spacing(free / 2.0),
        TextAlign::Right | TextAlign::End => free,
        _ => 0.0,
    }
}

/// Result of laying out an inline formatting context: content-box height
/// and the text fragments with offsets from the content-box origin.
pub(super) struct InlineResult {
    pub height: f32,
    pub children: Vec<(f32, f32, BoxFragment)>,
}

/// Height of a paragraph leaf's text at `width` and whether it is a single
/// line.  The line count is the paint wrapper's (it wraps the leaf), via its
/// height; a single line then uses the line-box model (LayoutUnit line
/// height, strut), several lines keep the wrapper's height.
fn paragraph_height(
    content: &str,
    width: f32,
    style: &Style,
    container: &Style,
) -> (f32, bool, f32) {
    let (strut_ascent, strut_descent) = strut(container);
    let (line, baseline) = text_line_metrics(content, style);
    let ascent = strut_ascent.max(baseline);
    let single_height = ascent + strut_descent.max(line - baseline);
    let forced = content.contains(['\u{2028}', '\u{2029}']);
    let fits = style.white_space == WhiteSpace::NoWrap
        || text_intrinsic_size(content, style).0 <= width + EPSILON;
    if !forced && fits {
        return (single_height, true, ascent);
    }
    let wrapped = wrapped_text_height(content, width, style);
    if !forced && wrapped < 1.5 * line.max(1.0) {
        // An unbreakable overflowing line.
        (single_height, true, ascent)
    } else {
        (wrapped, false, 0.0)
    }
}

/// Content height of a block-level text leaf of content width `width`.
pub(super) fn block_text_height(content: &str, width: f32, style: &Style) -> f32 {
    paragraph_height(content, width, style, style).0
}

/// NGInlineLayoutAlgorithm for an S2a container of content width
/// `available`.  `None` means a case this stage cannot express (an item
/// needing an internal break); the caller falls back to the legacy engine.
pub(super) fn layout_inline(
    flat: &[FlatNodeInfo],
    index: usize,
    available: f32,
) -> Option<InlineResult> {
    let container = flat[index].style;
    let children: Vec<usize> = rendered_children(flat, index).collect();

    // Paragraph leaf: paint wraps the text inside the line-width rect.
    if let [child] = children[..]
        && has_marker(flat[child].style, LINE_WIDTH_MARKER)
    {
        let style = flat[child].style;
        let ComponentKind::Text { content } = flat[child].kind else {
            return None;
        };
        let (height, single, ascent) = paragraph_height(content, available, style, container);
        let font_height = inline_font_height(style);
        let (y, rect_height) = if single {
            (ascent - inline_font_content_ascent(style), font_height)
        } else {
            let line = inline_used_line_height(inline_style_line_height(style));
            (
                crate::layout::inline_upper_leading(line, font_height),
                height,
            )
        };
        return Some(InlineResult {
            height,
            children: vec![(
                0.0,
                y,
                BoxFragment {
                    index: child,
                    width: available,
                    height: rect_height,
                    children: Vec::new(),
                },
            )],
        });
    }

    // CollectInlines: group word pieces into authored runs and shape each
    // run once.
    let mut items: Vec<InlineItem> = Vec::with_capacity(children.len());
    let mut start = 0;
    let mut run = 0;
    while start < children.len() {
        let first = flat[children[start]].style;
        let mut end = start + 1;
        while end < children.len() && same_run(first, flat[children[end]].style) {
            end += 1;
        }
        let texts: Vec<&str> = children[start..end]
            .iter()
            .map(|&c| match flat[c].kind {
                ComponentKind::Text { content } => content.as_str(),
                _ => "",
            })
            .collect();
        let advances = run_advances(&texts, first);
        for (offset, &child) in children[start..end].iter().enumerate() {
            let style = flat[child].style;
            let content = texts[offset];
            let raw = advances[offset];
            let (height, baseline) = text_line_metrics(content, style);
            items.push(InlineItem {
                index: child,
                raw,
                raw_hang: hang_advance(content, raw, style),
                run,
                starts_space: content.starts_with(' '),
                ends_space: content.ends_with(' '),
                whitespace_only: content.trim_end_matches(' ').is_empty(),
                ascent: baseline,
                descent: height - baseline,
                content_ascent: inline_font_content_ascent(style),
                font_height: inline_font_height(style),
            });
        }
        run += 1;
        start = end;
    }

    // NGLineBreaker.
    let can_wrap = container.white_space == WhiteSpace::Normal;
    let mut lines: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < items.len() {
        let item = &items[i];
        if current.is_empty() && item.whitespace_only {
            // A collapsible space at the start of a line is removed.
            i += 1;
            continue;
        }
        current.push(i);
        let fits = line_width(&items, &current) <= available + EPSILON;
        current.pop();
        if fits || current.is_empty() || !can_wrap {
            current.push(i);
            i += 1;
            continue;
        }
        if item.starts_space && !item.whitespace_only {
            // The opportunity lies after this item's leading space.
            return None;
        }
        // Last soft-wrap opportunity: after an item ending in a space.
        let break_at = (1..=current.len())
            .rev()
            .find(|&j| items[current[j - 1]].ends_space);
        match break_at {
            Some(j) if j == current.len() => lines.push(std::mem::take(&mut current)),
            Some(j) => {
                let rest = current.split_off(j);
                lines.push(std::mem::take(&mut current));
                if rest
                    .iter()
                    .any(|&k| items[k].starts_space && !items[k].whitespace_only)
                {
                    return None;
                }
                current = rest;
            }
            None => {
                // No opportunity on this line: the item overflows.
                current.push(i);
                i += 1;
            }
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }

    // Line boxes (NGLineBoxFragmentBuilder): run slices start on LayoutUnit
    // offsets; pieces inside a slice keep their raw shaping offsets and the
    // slice's last piece absorbs the snap remainder.
    let (strut_ascent, strut_descent) = strut(container);
    let mut built = Vec::with_capacity(lines.len());
    for mut line in lines {
        // Trailing collapsible spaces are removed at the line end.
        while line.last().is_some_and(|&k| items[k].whitespace_only) {
            line.pop();
        }
        if line.is_empty() {
            continue;
        }
        let mut ascent = strut_ascent;
        let mut descent = strut_descent;
        let mut placed = Vec::with_capacity(line.len());
        let mut slice_start = 0.0f32;
        let mut slice = 0.0f32;
        for (position, &k) in line.iter().enumerate() {
            let item = &items[k];
            let last_on_line = position + 1 == line.len();
            let advance = if last_on_line {
                item.raw_hang
            } else {
                item.raw
            };
            let x = slice_start + slice;
            slice += advance;
            let slice_ends = last_on_line || items[line[position + 1]].run != item.run;
            let width = if slice_ends {
                let end = slice_start + snap(slice);
                slice_start = end;
                slice = 0.0;
                end - x
            } else {
                advance
            };
            placed.push((k, x, width));
            ascent = ascent.max(item.ascent);
            descent = descent.max(item.descent);
        }
        built.push(Line {
            items: placed,
            width: slice_start,
            ascent,
            descent,
        });
    }

    let mut top = 0.0f32;
    let mut out = Vec::with_capacity(items.len());
    for line in built {
        let offset = align_offset(container, available - line.width);
        let baseline = top + line.ascent;
        for (k, x, width) in line.items {
            let item = &items[k];
            out.push((
                offset + x,
                baseline - item.content_ascent,
                BoxFragment {
                    index: item.index,
                    width,
                    height: item.font_height,
                    children: Vec::new(),
                },
            ));
        }
        top += line.ascent + line.descent;
    }
    Some(InlineResult {
        height: top,
        children: out,
    })
}
