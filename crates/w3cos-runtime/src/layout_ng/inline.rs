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

use super::block::MinMax;
use super::{BoxFragment, ConstraintSpace, LastBaseline};
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
/// Anonymous block box wrapping an inline run between block siblings
/// (CSS 2.1 9.2.1.1, Blink LayoutBlockFlow::CreateAnonymousBlock): default
/// box style with inherited text properties, no percentage containing block.
pub(super) const ANONYMOUS_BLOCK_MARKER: &str = "--w3cos-internal-anonymous-block-line";
/// On the root component in quirks mode (w3cos_dom QUIRKS_MODE_MARKER).
const QUIRKS_MARKER: &str = "--w3cos-internal-quirks-mode";
/// A `<br>` the cascade keeps as its own leaf (Blink NGInlineItem kControl).
const FORCED_BREAK: &str = "\u{2028}";
pub(super) const LINE_WIDTH_MARKER: &str = "--w3cos-internal-text-line-width";
/// `vertical-align: top | text-top | bottom | text-bottom` (the cascade
/// maps them to align-self start/end and keeps the keyword here).
pub(super) const VALIGN_MARKER: &str = "--w3cos-internal-vertical-align-keyword";
/// `vertical-align: <length-percentage>`: "offset line-extension" in px
/// (the cascade resolves the length; it also turns the extension into
/// block-axis margins, which do not apply to inline non-replaced boxes).
pub(super) const VALIGN_LENGTH_MARKER: &str = "--w3cos-internal-vertical-align-length";

/// The resolved `vertical-align` length (positive raises).
fn valign_length(style: &Style) -> Option<f32> {
    style
        .custom_properties
        .as_ref()
        .and_then(|p| p.get(VALIGN_LENGTH_MARKER))
        .and_then(|v| v.split_whitespace().next())
        .and_then(|v| v.parse().ok())
}

/// `vertical-align: top` (Some(true)) or `bottom` (Some(false)): the box's
/// aligned subtree is placed against the line box edge (CSS 2.1 10.8.1).
fn line_relative_align(style: &Style) -> Option<bool> {
    match style
        .custom_properties
        .as_ref()
        .and_then(|p| p.get(VALIGN_MARKER))
        .map(String::as_str)
    {
        Some("top") => Some(true),
        Some("bottom") => Some(false),
        _ => None,
    }
}

/// Why the item's/box's vertical-align is outside S2: baseline, or top /
/// bottom of the line box.
fn valign_reason(style: &Style) -> Option<&'static str> {
    use w3cos_std::style::AlignSelf;
    let marker = style
        .custom_properties
        .as_ref()
        .and_then(|p| p.get(VALIGN_MARKER));
    match (marker.map(String::as_str), style.align_self) {
        // middle: a baseline shift (vertical_align_shift).
        (None, AlignSelf::Auto | AlignSelf::Baseline | AlignSelf::Center) => None,
        (Some("top" | "text-top"), AlignSelf::FlexStart)
        | (Some("bottom" | "text-bottom"), AlignSelf::FlexEnd) => None,
        (Some(_), _) => Some("vertical-align-keyword"),
        _ => Some("vertical-align"),
    }
}

/// The primary font's x-height (vertical-align: middle uses the parent's).
fn x_height(style: &Style) -> Option<f32> {
    #[cfg(feature = "skia")]
    {
        crate::render_skia::resolved_font_x_height(style)
    }
    // Without Skia only Ahem's metrics are known (x-height 0.8em).
    #[cfg(not(feature = "skia"))]
    {
        style
            .font_family
            .as_deref()
            .is_some_and(|family| {
                family.split(',').any(|name| {
                    name.trim()
                        .trim_matches(['\'', '"'])
                        .eq_ignore_ascii_case("ahem")
                })
            })
            .then_some(style.font_size * 0.8)
    }
}

/// Blink NGInlineBoxState::ComputeAlignmentBaselineShift for
/// `vertical-align: middle` (CSS 2.1 10.8.1): the box's vertical midpoint
/// sits half the parent's x-height above the parent's baseline, i.e. its
/// baseline moves down by (ascent - descent) / 2 - x-height / 2 (LayoutUnit
/// halving, rounded x-height).  `Some(0.0)` for other values; `None` when
/// the parent's x-height is unknown.  `text-top` / `text-bottom` (kTextTop,
/// kTextBottom) put the box's top / bottom at the parent's content area
/// edge (its primary font's ascent / descent).
fn vertical_align_shift(style: &Style, ascent: f32, descent: f32, parent: &Style) -> Option<f32> {
    // kLength: the baseline moves up by the length (LayoutUnit).
    if let Some(length) = valign_length(style) {
        return Some(-(length * 64.0).trunc() / 64.0);
    }
    match style
        .custom_properties
        .as_ref()
        .and_then(|p| p.get(VALIGN_MARKER))
        .map(String::as_str)
    {
        Some("text-top") => return Some(ascent - inline_font_content_ascent(parent)),
        Some("text-bottom") => {
            let parent_descent = inline_font_height(parent) - inline_font_content_ascent(parent);
            return Some(parent_descent - descent);
        }
        _ => {}
    }
    if style.align_self != w3cos_std::style::AlignSelf::Center {
        return Some(0.0);
    }
    let x_height = x_height(parent)?;
    let half = w3cos_std::style::used_box_spacing((ascent - descent) / 2.0);
    Some(half - (x_height / 2.0 * 64.0).round() / 64.0)
}

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
    // (direction and unicode-bidi are checked by the callers.)
    // Without a newline (the content gate): pre is one preserved unwrapped
    // line, pre-wrap wraps with hanging preserved spaces, pre-line equals
    // normal.  (break-spaces is not a WhiteSpace value here.)
    if !matches!(
        style.white_space,
        WhiteSpace::Normal
            | WhiteSpace::NoWrap
            | WhiteSpace::Pre
            | WhiteSpace::PreWrap
            | WhiteSpace::PreLine
    ) {
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

/// Text whose break opportunities S2 finds itself: Latin-1 (spaces).  Other
/// scripts are laid out only where no line break decision depends on them.
fn latin1(content: &str) -> bool {
    content.chars().all(|c| {
        // Control characters (form feed, C1) have their own rendering
        // and breaking rules; legacy handles them.
        let control = c < ' ' || ('\u{7f}'..='\u{9f}').contains(&c);
        // (Newlines are gated by the content checks.)
        ((c as u32) < 0x100 && !control) || matches!(c, '\n' | '\u{2028}' | '\u{2029}')
    })
}

/// An item's resolved bidi embedding level (NGInlineItem::BidiLevel).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct LeafLevel {
    level: u8,
    /// Its trailing spaces resolved to another level: only harmless where
    /// they are removed at the line end (UAX #9 L1 resets them anyway).
    tail: bool,
}

/// NGBidiParagraph::SetParagraph over the IFC's text content (atomic
/// inlines as U+FFFC, `<br>` as a paragraph separator, as in
/// NGInlineItemsBuilder), base level from the container's direction.  Each
/// item must resolve to one level (NGInlineItem splits at level changes;
/// S2 keeps one fragment per item).  Paint draws an item's glyphs in
/// logical order, so an odd-level (reversed) item must read the same
/// reversed and contain no mirrored characters (L4).  `Err` names why.
fn leaf_bidi_levels(
    flat: &[FlatNodeInfo],
    leaves: &[usize],
    container: &Style,
) -> Result<Vec<LeafLevel>, &'static str> {
    use unicode_bidi::{BidiInfo, Level};
    let mut text = String::new();
    let mut ranges = Vec::with_capacity(leaves.len());
    for &leaf in leaves {
        let start = text.len();
        match flat[leaf].kind {
            ComponentKind::Text { content } if !is_atomic_inline(flat, leaf) => {
                if content == FORCED_BREAK {
                    text.push('\n');
                } else {
                    text.push_str(content);
                }
            }
            _ => text.push('\u{FFFC}'),
        }
        ranges.push(start..text.len());
    }
    let base = if container.direction == TextDirection::Rtl {
        Level::rtl()
    } else {
        Level::ltr()
    };
    let info = BidiInfo::new(&text, Some(base));
    let mut out = Vec::with_capacity(leaves.len());
    for (&leaf, range) in leaves.iter().zip(ranges) {
        let content = &text[range.clone()];
        let levels = &info.levels[range.clone()];
        if content == "\n" {
            out.push(LeafLevel {
                level: base.number(),
                tail: false,
            });
            continue;
        }
        let core_len = match content.trim_end_matches(' ').len() {
            0 => content.len(),
            n => n,
        };
        let level = levels[0];
        if levels[..core_len].iter().any(|&l| l != level) {
            return Err("bidi:mixed-item");
        }
        let tail = levels[core_len..].iter().any(|&l| l != level);
        if level.is_rtl() && !is_atomic_inline(flat, leaf) {
            let drawn = if tail { &content[..core_len] } else { content };
            if drawn.chars().rev().ne(drawn.chars())
                || drawn
                    .chars()
                    .any(|c| unicode_bidi_mirroring::get_mirrored(c).is_some())
            {
                return Err("bidi:rtl-glyphs");
            }
        }
        out.push(LeafLevel {
            level: level.number(),
            tail,
        });
    }
    Ok(out)
}

/// UAX #9 L2 over one line's items (NGBidiParagraph::IndicesInVisualOrder):
/// from the highest level down to the lowest odd one, reverse every
/// maximal run at or above it.
fn visual_order(levels: &[u8]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..levels.len()).collect();
    let (Some(&max), Some(&min)) = (levels.iter().max(), levels.iter().min()) else {
        return order;
    };
    let lowest_odd = min | 1;
    let mut level = max;
    while level >= lowest_odd {
        let mut i = 0;
        while i < order.len() {
            if levels[order[i]] >= level {
                let mut j = i;
                while j < order.len() && levels[order[j]] >= level {
                    j += 1;
                }
                order[i..j].reverse();
                i = j;
            } else {
                i += 1;
            }
        }
        level -= 1;
    }
    order
}

/// Characters S2 lays out.  Shaping and font fallback are the engine's
/// (Skia measures the run with fallback fonts, as paint draws it), and a
/// paragraph leaf is wrapped by paint itself; but strong right-to-left
/// characters and bidi controls need bidi reordering (UAX #9), other space
/// separators have their own collapsing and hanging rules (CSS Text 3 4.1),
/// and control characters their own rendering: those stay on legacy.
fn supported_script(content: &str) -> bool {
    content.chars().all(|c| {
        let u = c as u32;
        if matches!(c, '\n' | '\u{2028}' | '\u{2029}') {
            return true;
        }
        if c < ' ' || ('\u{7f}'..='\u{9f}').contains(&c) {
            return false;
        }
        if u < 0x100 {
            return true;
        }
        let rtl = (0x0590..=0x08FF).contains(&u)
            || (0xFB1D..=0xFDFF).contains(&u)
            || (0xFE70..=0xFEFF).contains(&u)
            || (0x10800..=0x10FFF).contains(&u)
            || (0x1E800..=0x1EFFF).contains(&u);
        let bidi_control = matches!(u, 0x061C | 0x200E | 0x200F)
            || (0x202A..=0x202E).contains(&u)
            || (0x2066..=0x2069).contains(&u);
        let other_space = matches!(u, 0x1680 | 0x180E | 0x205F | 0x3000)
            || (0x2000..=0x200D).contains(&u)
            || u == 0x202F
            || (0x2060..=0x2064).contains(&u);
        let special = (0xD800..=0xDFFF).contains(&u) || (0xFFF0..=0xFFFF).contains(&u);
        !(rtl || bidi_control || other_space || special)
    })
}

/// `line-height: normal` text whose fallback faces grow the line box past
/// the primary font's: the engines resolve those used-font metrics on
/// different paths (paint wrapper, line metrics), so legacy keeps them.
fn fallback_grows_line(content: &str, style: &Style) -> bool {
    #[cfg(feature = "skia")]
    if style.line_height_is_normal && !latin1(content) {
        return crate::render_skia::text_fallback_grows_normal_line(content, style);
    }
    let _ = (content, style);
    false
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
    if !supported_script(content) {
        return Some("text:script");
    }
    if fallback_grows_line(content, style) {
        return Some("text:fallback-metrics");
    }
    // A block text leaf is its own paragraph.
    if !super::block::block_unicode_bidi_ok(style) {
        return Some("unicode-bidi");
    }
    text_style_reason(style)
}

/// Inline box edges the cascade put on a word item of a decorated inline
/// box (its first item carries the inline-start edges, its last the end
/// edges, every item the block-axis padding/border).  Absolute lengths
/// only; an edge next to a collapsible space would need the space handling
/// at inline box boundaries; forced breaks inside are left to legacy.
fn inline_edges_supported(content: &str, style: &Style) -> bool {
    use w3cos_std::style::Spacing;
    let absolute = |s: Spacing| matches!(s, Spacing::Px(_) | Spacing::Em(_) | Spacing::Rem(_));
    let m = style.margin_lengths();
    let edges = InlineEdges::of(style);
    [
        style.padding.top,
        style.padding.right,
        style.padding.bottom,
        style.padding.left,
        style.margin.left,
        style.margin.right,
    ]
    .into_iter()
    .all(absolute)
        && m.left >= 0.0
        && m.right >= 0.0
        && !content.contains(['\n', '\u{2028}', '\u{2029}'])
        && (edges.start == 0.0 || !content.starts_with(' '))
        && (edges.end == 0.0 || !content.ends_with(' '))
}

/// An item's inline box edges (NGInlineBoxState): inline-axis margin,
/// border and padding occupy advance; block-axis border and padding only
/// grow its painted box, not the line box (CSS 2.1 10.8.1).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct InlineEdges {
    start: f32,
    end: f32,
    margin_start: f32,
    margin_end: f32,
    top: f32,
    bottom: f32,
}

impl InlineEdges {
    fn of(style: &Style) -> Self {
        let p = style.padding_lengths();
        let m = style.margin_lengths();
        let border = |side: Option<f32>| side.unwrap_or(style.border_width);
        InlineEdges {
            start: m.left + border(style.border_left_width) + p.left,
            end: p.right + border(style.border_right_width) + m.right,
            margin_start: m.left,
            margin_end: m.right,
            top: border(style.border_top_width) + p.top,
            bottom: border(style.border_bottom_width) + p.bottom,
        }
    }
}

/// An inline text leaf inside an IFC container.
fn inline_text_reason(
    content: &str,
    style: &Style,
    sole: bool,
    container: &Style,
) -> Option<&'static str> {
    if style.display != Display::Inline {
        return Some("text:display");
    }
    // The paragraph and preserved-newline paths place the leaf without a
    // baseline shift.
    if valign_length(style).is_some()
        && (has_marker(style, LINE_WIDTH_MARKER) || content.contains('\n'))
    {
        return Some("vertical-align-length");
    }
    // An embedding or isolate inside the paragraph needs bidi resolution
    // (around a sole leaf, the whole paragraph, it changes nothing).
    if style.direction != container.direction
        || (style.unicode_bidi != w3cos_std::style::UnicodeBidi::Normal
            && !(sole && super::block::block_unicode_bidi_ok(style)))
    {
        return Some("direction");
    }
    // An rtl paragraph (UAX #9 paragraph level 1): paint orders a paragraph
    // leaf itself; word items get their levels from leaf_bidi_levels and
    // are reordered per line, with line-end spaces removed (collapsible
    // white space only).
    let paragraph = sole && has_marker(style, LINE_WIDTH_MARKER);
    if container.direction == TextDirection::Rtl
        && !paragraph
        && !matches!(style.white_space, WhiteSpace::Normal | WhiteSpace::NoWrap)
    {
        return Some("bidi:white-space");
    }
    if !zero_edges(style)
        && (has_marker(style, LINE_WIDTH_MARKER) || !inline_edges_supported(content, style))
    {
        return Some("text:edges");
    }
    // Preserved newlines (CSS Text 3 4.1.1) in a sole leaf: paint breaks
    // the lines; a trailing one only through the paragraph wrapper.
    let newline_ok = sole
        && match style.white_space {
            WhiteSpace::PreLine => has_marker(style, LINE_WIDTH_MARKER),
            // (line-height: normal mixes the strut's computed line height
            // with the font's line spacing differently in legacy.)
            WhiteSpace::Pre | WhiteSpace::PreWrap => {
                has_marker(style, LINE_WIDTH_MARKER)
                    || (!content.ends_with('\n') && !style.line_height_is_normal)
            }
            _ => false,
        };
    if content.is_empty()
        || content.contains(['\r', '\t'])
        || (content.contains('\n') && !newline_ok)
    {
        return Some("text:content");
    }
    if !supported_script(content) {
        return Some("text:script");
    }
    if fallback_grows_line(content, style) {
        return Some("text:fallback-metrics");
    }
    if has_marker(style, LINE_WIDTH_MARKER) {
        if !sole {
            return Some("text:line-width-shared");
        }
    } else {
        // vertical-align (middle maps to align-self; the other keywords and
        // lengths carry markers the marker gate rejects).
        if let Some(reason) = valign_reason(style) {
            return Some(reason);
        }
        if sole && line_relative_align(style).is_some() {
            return Some("vertical-align-keyword");
        }
        // break-all / overflow-wrap add emergency breaks inside an item.
        if !matches!(
            style.word_break,
            w3cos_std::style::WordBreak::Normal | w3cos_std::style::WordBreak::KeepAll
        ) {
            return Some("text:word-break");
        }
        // Items may hold internal soft-wrap opportunities (the breaker falls
        // back when it would need one), but no forced break.
        if content.contains(['\u{2028}', '\u{2029}']) && content != FORCED_BREAK {
            return Some("text:internal-break");
        }
    }
    text_style_reason(style)
}

pub(super) fn rendered_children<'a>(
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

/// An inline box the cascade keeps as a box (a `<span>` around content
/// with its own font or edges); its text leaves are items of the enclosing
/// IFC.
pub(super) fn is_inline_box(flat: &[FlatNodeInfo], index: usize) -> bool {
    matches!(
        flat[index].kind,
        ComponentKind::Box | ComponentKind::Column | ComponentKind::Row
    ) && flat[index].style.display == Display::Inline
}

/// An inline-block box: an atomic inline (NGInlineItem kAtomicInline),
/// laid out as its own block formatting context.
/// A text leaf the cascade gives `display: inline-block` (an inline-block
/// holding only text) is one too: a block text leaf as its own BFC.
pub(super) fn is_atomic_inline(flat: &[FlatNodeInfo], index: usize) -> bool {
    matches!(
        flat[index].kind,
        ComponentKind::Box
            | ComponentKind::Column
            | ComponentKind::Row
            | ComponentKind::Text { .. }
    ) && flat[index].style.display == Display::InlineBlock
}

/// True when the block container's rendered children are inline content.
pub(super) fn is_inline_container(flat: &[FlatNodeInfo], index: usize) -> bool {
    !is_inline_box(flat, index)
        && rendered_children(flat, index).next().is_some_and(|child| {
            (matches!(flat[child].kind, ComponentKind::Text { .. })
                && flat[child].style.display == Display::Inline)
                || is_inline_box(flat, child)
                || is_atomic_inline(flat, child)
        })
}

/// Why an inline-block (its box and content checked by the block gate)
/// is outside S2: auto inline margins are 0 for inline blocks;
/// vertical-align other than baseline, top or bottom.  (A shrink-to-fit width whose
/// min-content size is unknown falls back at layout time.)
pub(super) fn atomic_inline_reason(style: &Style) -> Option<&'static str> {
    use w3cos_std::style::Spacing;
    if matches!(style.margin.left, Spacing::Auto) || matches!(style.margin.right, Spacing::Auto) {
        return Some("inline-block:auto-margin");
    }
    // top / bottom: its margin box is the aligned subtree (CSS 2.1 10.8.1).
    valign_reason(style)
}

/// True when `index` sits (through inline boxes) in an IFC container.
pub(super) fn in_inline_container(flat: &[FlatNodeInfo], index: usize) -> bool {
    let mut parent = flat[index].parent;
    while let Some(p) = parent {
        if !is_inline_box(flat, p) {
            return is_inline_container(flat, p);
        }
        parent = flat[p].parent;
    }
    false
}

/// NGInlineNode::CollectInlines: the IFC's leaves in order with the open
/// and close tags of the inline boxes around them (kOpenTag/kCloseTag
/// items).  Leaves of other kinds are rejected by the gate.
#[derive(Clone, Copy, Debug, PartialEq)]
enum InlineEntry {
    Open(usize),
    Leaf(usize),
    Close(usize),
}

fn collect_inlines(flat: &[FlatNodeInfo], index: usize) -> Vec<InlineEntry> {
    fn walk(flat: &[FlatNodeInfo], index: usize, out: &mut Vec<InlineEntry>) {
        for child in rendered_children(flat, index) {
            if is_inline_box(flat, child) {
                out.push(InlineEntry::Open(child));
                walk(flat, child, out);
                out.push(InlineEntry::Close(child));
            } else {
                out.push(InlineEntry::Leaf(child));
            }
        }
    }
    let mut out = Vec::new();
    walk(flat, index, &mut out);
    out
}

/// Why an inline box (its open tag at `entries[0]`) is outside S2.
fn inline_box_reason(
    flat: &[FlatNodeInfo],
    entries: &[InlineEntry],
    container: &Style,
) -> Option<&'static str> {
    use w3cos_std::style::Spacing;
    let InlineEntry::Open(index) = entries[0] else {
        return None;
    };
    let style = flat[index].style;
    if super::block::unsupported_marker(style, &[VALIGN_MARKER]).is_some() {
        return Some("internal-marker");
    }
    if style.position != Position::Static || style.float != Float::None {
        return Some("text:positioned");
    }
    if style.direction != container.direction
        || style.unicode_bidi != w3cos_std::style::UnicodeBidi::Normal
    {
        return Some("direction");
    }
    if style.white_space != container.white_space {
        return Some("text:white-space");
    }
    if let Some(reason) = valign_reason(style) {
        return Some(reason);
    }
    // (A middle / text-top / text-bottom box would shift its whole subtree.)
    if style.align_self == w3cos_std::style::AlignSelf::Center {
        return Some("vertical-align-middle-box");
    }
    if matches!(
        style
            .custom_properties
            .as_ref()
            .and_then(|p| p.get(VALIGN_MARKER))
            .map(String::as_str),
        Some("text-top" | "text-bottom")
    ) {
        return Some("vertical-align-keyword");
    }
    let absolute = |s: Spacing| matches!(s, Spacing::Px(_) | Spacing::Em(_) | Spacing::Rem(_));
    let m = style.margin_lengths();
    if ![
        style.padding.top,
        style.padding.right,
        style.padding.bottom,
        style.padding.left,
        style.margin.left,
        style.margin.right,
    ]
    .into_iter()
    .all(absolute)
        || m.left < 0.0
        || m.right < 0.0
    {
        return Some("inline-box:edges");
    }
    // The box's strut (NGInlineBoxState text metrics) is taken from text it
    // holds directly in its own font, which always gets a fragment.
    let strut_text = rendered_children(flat, index).any(|child| {
        matches!(flat[child].kind, ComponentKind::Text { content }
            if !content.trim_matches(' ').is_empty() && content != FORCED_BREAK)
            && same_font(flat[child].style, style)
    });
    if !strut_text {
        return Some("inline-box:strut");
    }
    // An edge next to a collapsible space needs the space handling at
    // inline box boundaries.
    let close = entries
        .iter()
        .position(|&e| e == InlineEntry::Close(index))
        .unwrap_or(entries.len());
    let leaf_text = |e: &InlineEntry| match *e {
        InlineEntry::Leaf(c) => match flat[c].kind {
            ComponentKind::Text { content } => Some(content.as_str()),
            _ => Some(""),
        },
        _ => None,
    };
    let edges = InlineEdges::of(style);
    let first = entries[..close].iter().find_map(leaf_text).unwrap_or("");
    let last = entries[..close]
        .iter()
        .rev()
        .find_map(leaf_text)
        .unwrap_or("");
    if (edges.start != 0.0 && first.starts_with(' ')) || (edges.end != 0.0 && last.ends_with(' ')) {
        return Some("inline-box:edge-space");
    }
    None
}

/// Why this IFC container (and its items) is outside S2a.
pub(super) fn container_reason(flat: &[FlatNodeInfo], index: usize) -> Option<&'static str> {
    let style = flat[index].style;
    // The cascade marks multi-item IFC roots.  An unmarked box may still be
    // the parent of plain text (`<div>text</div>`), whose style then is the
    // box's own font; a box whose text does not inherit its font (a shadow
    // <slot>, display:contents in Chrome, carries a default style) does not
    // own the strut, so its style must not drive line metrics.
    if !has_marker(style, IFC_MARKER)
        && rendered_children(flat, index).any(|child| !same_font(flat[child].style, style))
    {
        return Some("ifc:unmarked");
    }
    if style.text_align == TextAlign::Justify {
        return Some("text:justify");
    }
    if style.text_indent != Dimension::Px(0.0) {
        return Some("text:indent");
    }
    // Without a newline (the content gate): pre is one preserved unwrapped
    // line, pre-wrap wraps with hanging preserved spaces, pre-line equals
    // normal.  (break-spaces is not a WhiteSpace value here.)
    if !matches!(
        style.white_space,
        WhiteSpace::Normal
            | WhiteSpace::NoWrap
            | WhiteSpace::Pre
            | WhiteSpace::PreWrap
            | WhiteSpace::PreLine
    ) {
        return Some("text:white-space");
    }
    let entries = collect_inlines(flat, index);
    let sole = matches!(entries[..], [InlineEntry::Leaf(_)]);
    // Open inline boxes; a top/bottom-aligned subtree must not contain
    // another one (one level of line-relative alignment).
    let mut open: Vec<usize> = Vec::new();
    for (position, &entry) in entries.iter().enumerate() {
        let child = match entry {
            InlineEntry::Open(b) => {
                if let Some(reason) = inline_box_reason(flat, &entries[position..], style) {
                    return Some(reason);
                }
                if line_relative_align(flat[b].style).is_some()
                    && open
                        .iter()
                        .any(|&o| line_relative_align(flat[o].style).is_some())
                {
                    return Some("vertical-align-nested");
                }
                open.push(b);
                continue;
            }
            InlineEntry::Close(_) => {
                open.pop();
                continue;
            }
            InlineEntry::Leaf(child) => child,
        };
        let info = &flat[child];
        if line_relative_align(info.style).is_some() && !open.is_empty() {
            return Some("vertical-align-nested");
        }
        let parent = open.last().map_or(style, |&b| flat[b].style);
        if info.style.align_self == w3cos_std::style::AlignSelf::Center
            && x_height(parent).is_none()
        {
            return Some("vertical-align-middle-font");
        }
        if is_atomic_inline(flat, child) {
            // Validated by the block gate (node_reason); its bidi level (a
            // U+FFFC neutral) comes from leaf_bidi_levels.
            continue;
        }
        let ComponentKind::Text { content } = info.kind else {
            return Some("ifc:atomic-or-span");
        };
        if info.subtree_end > child + 1 {
            return Some("ifc:atomic-or-span");
        }
        if info.style.white_space != style.white_space {
            return Some("text:white-space");
        }
        if let Some(reason) = inline_text_reason(content, info.style, sole, style) {
            return Some(reason);
        }
        if content == FORCED_BREAK {
            // NGLineBreaker forced break: the item adds no metrics, so its
            // font must be the strut's (a <br> in a differently sized inline
            // box would need that box's strut, NGInlineBoxState).  Quirks
            // empty-line rules and preserved spaces before it: legacy.
            if !same_font(info.style, style)
                || !matches!(style.white_space, WhiteSpace::Normal | WhiteSpace::NoWrap)
                || has_marker(flat[0].style, QUIRKS_MARKER)
            {
                return Some("text:forced-break");
            }
        }
    }
    if let Some(reason) = rtl_paragraph_reason(flat, &entries, style) {
        return Some(reason);
    }
    None
}

/// The bidi part of the container gate: an rtl paragraph laid out by items
/// (a sole line-width leaf is ordered by paint).
fn rtl_paragraph_reason(
    flat: &[FlatNodeInfo],
    entries: &[InlineEntry],
    container: &Style,
) -> Option<&'static str> {
    if container.direction != TextDirection::Rtl {
        return None;
    }
    if let [InlineEntry::Leaf(c)] = entries[..]
        && !is_atomic_inline(flat, c)
        && has_marker(flat[c].style, LINE_WIDTH_MARKER)
    {
        return None;
    }
    let leaves: Vec<usize> = entries
        .iter()
        .filter_map(|e| match *e {
            InlineEntry::Leaf(c) => Some(c),
            _ => None,
        })
        .collect();
    let levels = match leaf_bidi_levels(flat, &leaves, container) {
        Ok(levels) => levels,
        Err(reason) => return Some(reason),
    };
    if leaves.len() != entries.len() && needs_reorder(&levels) {
        // Inline box fragments around reordered items (NGInlineBoxState
        // with bidi-split boxes) are not modelled.
        return Some("bidi:inline-box");
    }
    None
}

/// Some item is not at the even level shared by all (L2 changes order).
fn needs_reorder(levels: &[LeafLevel]) -> bool {
    levels
        .first()
        .is_some_and(|first| first.level % 2 == 1 || levels.iter().any(|l| l.level != first.level))
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
    /// A forced line break (`<br>`): ends the line, zero advance.
    forced: bool,
    /// Edges of the inline box this item starts/ends/sits in.
    edges: InlineEdges,
    /// Inline-start / inline-end edges of the enclosing inline boxes
    /// opened right before / closed right after this item (NGInlineBoxState
    /// kOpenTag/kCloseTag advances).
    box_start: f32,
    box_end: f32,
    /// Root of the top/bottom-aligned subtree holding this item (flat index
    /// of the aligned box or item, and whether it is `top`).
    aligned: Option<(usize, bool)>,
    /// A soft-wrap opportunity inside the item's non-space core: NG keeps
    /// one fragment per item, so a line that needs it falls back.
    internal_break: bool,
    /// An atomic inline: its laid-out fragment and inline-start / block-
    /// start margins (the item's advance and metrics are its margin box).
    atomic: Option<(BoxFragment, f32, f32)>,
    /// Outside Latin-1: opportunities between such items (ideographs,
    /// dictionary breaks) are not modelled, so a line needing a break
    /// next to one falls back.
    non_latin: bool,
    /// Resolved bidi level and trailing-space level change (LeafLevel).
    bidi: LeafLevel,
    /// Baseline shift (down) from vertical-align: middle.
    shift: f32,
    ascent: f32,
    descent: f32,
    content_ascent: f32,
    font_height: f32,
}

/// ShapeResult::SnappedWidth.
fn snap(advance: f32) -> f32 {
    crate::text_layout::inline_layout_advance(advance)
}

/// Same used font and line-height (text inheriting directly from a box).
fn same_font(a: &Style, b: &Style) -> bool {
    a.font_family == b.font_family
        && a.font_size == b.font_size
        && a.font_weight == b.font_weight
        && a.font_style == b.font_style
        && a.line_height == b.line_height
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
    // An inline box edge ends the shaping run (its item starts or ends a
    // box fragment).
    source(a).is_some()
        && source(a) == source(b)
        && zero_edges(a)
        && zero_edges(b)
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

/// LayoutUnit width of a line holding `line` (item indices in order) for
/// fitting and alignment: trailing spaces hang (removed when collapsible,
/// CSS Text 3 4.1.2; hanging when preserved by pre-wrap, 4.1.3), so the
/// trailing whitespace-only items and the last other item's trailing spaces
/// do not count.
fn line_width(items: &[InlineItem], line: &[usize]) -> f32 {
    let end = line
        .iter()
        .rposition(|&k| !items[k].whitespace_only && !items[k].forced)
        .map_or(0, |p| p + 1);
    let mut width = 0.0f32;
    let mut slice = 0.0f32;
    let mut run = usize::MAX;
    for (position, &k) in line[..end].iter().enumerate() {
        let item = &items[k];
        if item.run != run {
            width += snap(slice);
            slice = 0.0;
            run = item.run;
        }
        slice += if position + 1 == end {
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
    /// Top/bottom-aligned subtrees on the line: (root, top, ascent,
    /// descent) relative to the subtree's own baseline.
    subtrees: Vec<(usize, bool, f32, f32)>,
}

/// NGLineBoxFragmentBuilder alignment offset; `start` / `end` follow the
/// container's direction, and an overflowing line is start-aligned (CSS
/// Text 3 6.1), which in rtl puts it left of the content box.
fn align_offset(style: &Style, free: f32) -> f32 {
    let rtl = style.direction == TextDirection::Rtl;
    if free < 0.0 {
        return if rtl { free } else { 0.0 };
    }
    match style.text_align {
        TextAlign::Center => w3cos_std::style::used_box_spacing(free / 2.0),
        TextAlign::Right => free,
        TextAlign::Left => 0.0,
        TextAlign::Start | TextAlign::Justify => {
            if rtl {
                free
            } else {
                0.0
            }
        }
        TextAlign::End => {
            if rtl {
                0.0
            } else {
                free
            }
        }
    }
}

/// Result of laying out an inline formatting context: content-box height
/// and the text fragments with offsets from the content-box origin.
pub(super) struct InlineResult {
    pub height: f32,
    pub children: Vec<(f32, f32, BoxFragment)>,
    /// The last line box's baseline from the content-box top.
    pub last_baseline: LastBaseline,
    /// The widest line's content width (hanging spaces excluded): the
    /// min-content / max-content size at available 0 / indefinite.
    pub max_line_width: f32,
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
    quirks: bool,
) -> (f32, bool, f32) {
    // Quirks line height calculation quirk: the root inline box (strut)
    // contributes only through text it holds directly, whose metrics are the
    // strut's own when the fonts match — so dropping it is exact either way.
    let (strut_ascent, strut_descent) = if quirks {
        (f32::NEG_INFINITY, f32::NEG_INFINITY)
    } else {
        strut(container)
    };
    let (line, baseline) = text_line_metrics(content, style);
    let ascent = strut_ascent.max(baseline);
    let single_height = ascent + strut_descent.max(line - baseline);
    let forced = has_forced_break(content, style);
    let fits = matches!(style.white_space, WhiteSpace::NoWrap | WhiteSpace::Pre)
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

/// A forced line break inside a leaf: a coalesced `<br>` or a preserved
/// newline.
fn has_forced_break(content: &str, style: &Style) -> bool {
    content.contains(['\u{2028}', '\u{2029}'])
        || (content.contains('\n')
            && matches!(
                style.white_space,
                WhiteSpace::Pre | WhiteSpace::PreWrap | WhiteSpace::PreLine
            ))
}

/// Content height of a block-level text leaf of content width `width`.
/// Content height and last baseline of a block-level text leaf.
pub(super) fn block_text_layout(content: &str, width: f32, style: &Style) -> (f32, LastBaseline) {
    let (height, single, ascent) = paragraph_height(content, width, style, style, false);
    (
        height,
        if single {
            LastBaseline::At(ascent)
        } else {
            LastBaseline::Unknown
        },
    )
}

/// NGInlineLayoutAlgorithm for an S2a container of content width
/// `available`.  `None` means a case this stage cannot express (an item
/// needing an internal break); the caller falls back to the legacy engine.
pub(super) fn layout_inline(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
) -> Option<InlineResult> {
    let available = space.available_inline;
    let container = flat[index].style;
    let quirks = has_marker(flat[0].style, QUIRKS_MARKER);
    let entries = collect_inlines(flat, index);
    let children: Vec<usize> = entries
        .iter()
        .filter_map(|e| match *e {
            InlineEntry::Leaf(c) => Some(c),
            _ => None,
        })
        .collect();
    let has_boxes = children.len() != entries.len();
    // Each leaf's open tags before it (outer first) and close tags after it
    // (inner first).
    let mut opens: Vec<Vec<usize>> = vec![Vec::new(); children.len()];
    let mut closes: Vec<Vec<usize>> = vec![Vec::new(); children.len()];
    let mut aligned: Vec<Option<(usize, bool)>> = vec![None; children.len()];
    // Each leaf's innermost inline box (its vertical-align parent).
    let mut parents: Vec<Option<usize>> = vec![None; children.len()];
    {
        let mut leaf = 0;
        let mut pending = Vec::new();
        let mut stack: Vec<usize> = Vec::new();
        for entry in &entries {
            match *entry {
                InlineEntry::Open(b) => {
                    pending.push(b);
                    stack.push(b);
                }
                InlineEntry::Leaf(c) => {
                    opens[leaf] = std::mem::take(&mut pending);
                    parents[leaf] = stack.last().copied();
                    aligned[leaf] = stack
                        .iter()
                        .chain(std::iter::once(&c))
                        .find_map(|&n| line_relative_align(flat[n].style).map(|top| (n, top)));
                    leaf += 1;
                }
                InlineEntry::Close(b) => {
                    if leaf == 0 {
                        return None;
                    }
                    closes[leaf - 1].push(b);
                    stack.pop();
                }
            }
        }
        if !pending.is_empty() {
            return None;
        }
    }
    let start_edges = |boxes: &[usize]| -> f32 {
        boxes
            .iter()
            .map(|&b| InlineEdges::of(flat[b].style).start)
            .sum()
    };
    let end_edges = |boxes: &[usize]| -> f32 {
        boxes
            .iter()
            .map(|&b| InlineEdges::of(flat[b].style).end)
            .sum()
    };

    // Paragraph leaf: paint wraps the text inside the line-width rect.
    if !has_boxes
        && let [child] = children[..]
        && !is_atomic_inline(flat, child)
        && has_marker(flat[child].style, LINE_WIDTH_MARKER)
    {
        let style = flat[child].style;
        let ComponentKind::Text { content } = flat[child].kind else {
            return None;
        };
        let (height, single, ascent) =
            paragraph_height(content, available, style, container, quirks);
        let font_height = inline_font_height(style);
        let (y, rect_height) = if single {
            (ascent - inline_font_content_ascent(style), font_height)
        } else {
            let line = inline_used_line_height(inline_style_line_height(style));
            // Paint steps forced (U+2028, a coalesced <br>) lines itself
            // from the first line's content box; soft-wrapped paragraphs
            // cover all their lines.
            let soft_wrapped = || {
                content
                    .split(['\u{2028}', '\u{2029}', '\n'])
                    .any(|line| text_intrinsic_size(line, style).0 > available + EPSILON)
            };
            // (Legacy covers all lines when the content box overflows the
            // line box, or when line-height: normal lines also soft-wrap.)
            let forced = has_forced_break(content, style)
                && font_height <= line + EPSILON
                && !(style.line_height_is_normal && soft_wrapped());
            (
                crate::layout::inline_upper_leading(line, font_height),
                if forced { font_height } else { height },
            )
        };
        return Some(InlineResult {
            height,
            max_line_width: available,
            last_baseline: if single {
                LastBaseline::At(ascent)
            } else {
                LastBaseline::Unknown
            },
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

    // Preserved newlines in a sole pre / pre-wrap leaf: one line per segment
    // (pre-wrap only when none wraps).  The engine's rect is the first
    // line's content box, as wide as the widest line and aligned by it.
    if !has_boxes
        && let [child] = children[..]
        && !is_atomic_inline(flat, child)
        && let ComponentKind::Text { content } = flat[child].kind
        && content.contains('\n')
    {
        let style = flat[child].style;
        let width = text_intrinsic_size(content, style).0;
        if style.white_space == WhiteSpace::PreWrap
            && content
                .split('\n')
                .any(|line| text_intrinsic_size(line, style).0 > available + EPSILON)
        {
            return None;
        }
        let (strut_ascent, strut_descent) = if quirks {
            (f32::NEG_INFINITY, f32::NEG_INFINITY)
        } else {
            strut(container)
        };
        let (line, baseline) = text_line_metrics(content, style);
        let ascent = strut_ascent.max(baseline);
        let single = ascent + strut_descent.max(line - baseline);
        let lines = content.split('\n').count() as f32;
        return Some(InlineResult {
            height: single * lines,
            max_line_width: width,
            last_baseline: LastBaseline::Unknown,
            children: vec![(
                align_offset(container, available - width),
                ascent - inline_font_content_ascent(style),
                BoxFragment {
                    index: child,
                    width,
                    height: inline_font_height(style),
                    children: Vec::new(),
                },
            )],
        });
    }

    // Preserved spaces: pre (neither collapse nor hang) and pre-wrap (hang at
    // the line end, CSS Text 3 4.1.3).
    let preserve = matches!(container.white_space, WhiteSpace::Pre | WhiteSpace::PreWrap);
    let hang_preserved = container.white_space == WhiteSpace::PreWrap;
    // NGBidiParagraph levels (an ltr paragraph without strong R, which the
    // script gate rejects, resolves to level 0 throughout).
    let levels = if container.direction == TextDirection::Rtl {
        leaf_bidi_levels(flat, &children, container).ok()?
    } else {
        vec![LeafLevel::default(); children.len()]
    };
    let reorder = needs_reorder(&levels);
    if reorder && has_boxes {
        return None;
    }
    // CollectInlines: group word pieces into authored runs and shape each
    // run once.
    let mut items: Vec<InlineItem> = Vec::with_capacity(children.len());
    let mut start = 0;
    let mut run = 0;
    while start < children.len() {
        if is_atomic_inline(flat, children[start]) {
            // NGInlineItem kAtomicInline: a block formatting context root
            // whose margin box is the item.  Its baseline is its last line
            // box's, or the bottom margin edge without one (CSS 2.1 10.8.1).
            let k = start;
            let child = children[k];
            let style = flat[child].style;
            let result = super::block::layout_block(flat, child, space, true)?;
            // Authored margins (no CSS2 10.3.3 over-constraint rule; auto is
            // gated).
            let side = |m| {
                crate::layout::resolve_spacing_for_layout(
                    m,
                    available,
                    style,
                    space.viewport_w,
                    space.viewport_h,
                )
            };
            let margin = [
                side(style.margin.top),
                side(style.margin.right),
                side(style.margin.bottom),
                side(style.margin.left),
            ];
            let height = margin[0] + result.fragment.height + margin[2];
            let ascent = match result.last_baseline {
                LastBaseline::At(b) => margin[0] + b,
                LastBaseline::NoLines => height,
                LastBaseline::Unknown => return None,
            };
            let box_start = start_edges(&opens[k]);
            let box_end = end_edges(&closes[k]);
            let raw = margin[3] + result.fragment.width + margin[1] + box_start + box_end;
            let parent = parents[k].map_or(container, |b| flat[b].style);
            let shift = vertical_align_shift(style, ascent, height - ascent, parent)?;
            items.push(InlineItem {
                index: child,
                raw,
                raw_hang: raw,
                run,
                starts_space: false,
                ends_space: false,
                whitespace_only: false,
                forced: false,
                edges: InlineEdges::default(),
                box_start,
                box_end,
                aligned: aligned[k],
                internal_break: false,
                atomic: Some((result.fragment, margin[3], margin[0])),
                non_latin: false,
                bidi: levels[k],
                shift,
                ascent,
                descent: height - ascent,
                content_ascent: 0.0,
                font_height: 0.0,
            });
            run += 1;
            start += 1;
            continue;
        }
        let first = flat[children[start]].style;
        let mut end = start + 1;
        // A box boundary ends the shaping run; so does a bidi level change
        // (NGInlineItem segments), and reversed items are shaped alone.
        while end < children.len()
            && opens[end].is_empty()
            && closes[end - 1].is_empty()
            && same_run(first, flat[children[end]].style)
            && levels[end].level == levels[start].level
            && levels[start].level % 2 == 0
        {
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
            // A forced break has no advance (kControl item).
            let edges = InlineEdges::of(style);
            let inline_edges = edges.start + edges.end;
            // The text measurement adds a leaf's own padding; an edge item
            // (its own run) is shaped without its box edges.
            let bare = (!zero_edges(style)).then(|| {
                let defaults = Style::default();
                Style {
                    padding: defaults.padding,
                    margin: defaults.margin,
                    border_width: 0.0,
                    border_top_width: None,
                    border_right_width: None,
                    border_bottom_width: None,
                    border_left_width: None,
                    ..style.clone()
                }
            });
            let measure = bare.as_ref().unwrap_or(style);
            let text_raw = if content == FORCED_BREAK {
                0.0
            } else if bare.is_some() {
                text_intrinsic_size(content, measure).0
            } else {
                advances[offset]
            };
            let k = start + offset;
            let box_start = start_edges(&opens[k]);
            let box_end = end_edges(&closes[k]);
            let raw = text_raw + inline_edges + box_start + box_end;
            let (height, baseline) = text_line_metrics(content, style);
            let parent = parents[k].map_or(container, |b| flat[b].style);
            let shift = vertical_align_shift(style, baseline, height - baseline, parent)?;
            items.push(InlineItem {
                index: child,
                raw,
                // Preserved spaces (pre) do not hang (CSS Text 3 4.1.3).
                raw_hang: if container.white_space == WhiteSpace::Pre {
                    raw
                } else {
                    hang_advance(content, text_raw, measure) + inline_edges + box_start + box_end
                },
                run,
                starts_space: content.starts_with(' '),
                ends_space: content.ends_with(' '),
                whitespace_only: content.trim_end_matches(' ').is_empty(),
                forced: content == FORCED_BREAK,
                edges,
                box_start,
                box_end,
                aligned: aligned[k],
                // Preserved leading spaces (pre-wrap) end in an opportunity
                // inside the item too; collapsible ones vanish at a line start.
                internal_break: content.trim_matches(' ').chars().any(is_soft_wrap_char)
                    || (hang_preserved
                        && content.starts_with(' ')
                        && !content.trim_start_matches(' ').is_empty()),
                non_latin: !latin1(content),
                bidi: levels[k],
                shift,
                atomic: None,
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
    let can_wrap = matches!(
        container.white_space,
        WhiteSpace::Normal | WhiteSpace::PreWrap | WhiteSpace::PreLine
    );
    let mut lines: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < items.len() {
        let item = &items[i];
        if !preserve && current.is_empty() && item.whitespace_only {
            // A collapsible space at the start of a line is removed.
            i += 1;
            continue;
        }
        if item.forced {
            // NGLineBreaker::HandleForcedLineBreak.
            current.push(i);
            lines.push(std::mem::take(&mut current));
            i += 1;
            continue;
        }
        current.push(i);
        let fits = line_width(&items, &current) <= available + EPSILON;
        current.pop();
        if !fits && can_wrap && item.internal_break {
            // NGLineBreaker would break inside this item.
            return None;
        }
        if !fits && can_wrap && (item.non_latin || current.iter().any(|&k| items[k].non_latin)) {
            // The break position may depend on UAX #14 classes S2 does not
            // model (ideographic or dictionary opportunities).
            return None;
        }
        if fits || current.is_empty() || !can_wrap {
            current.push(i);
            i += 1;
            continue;
        }
        if item.starts_space && !item.whitespace_only {
            // The opportunity lies after this item's leading space.
            return None;
        }
        // Last soft-wrap opportunity: after an item ending in a space, at the
        // end of a space run (preserved spaces in pre-wrap do not break
        // between each other).
        let break_at = (1..=current.len()).rev().find(|&j| {
            let next = current.get(j).copied().unwrap_or(i);
            let prev = current[j - 1];
            // Atomic inlines break before and after (UAX #14 CB, LB20).
            (items[prev].ends_space || items[prev].atomic.is_some() || items[next].atomic.is_some())
                && !items[next].starts_space
        });
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
    let (strut_ascent, strut_descent) = if quirks {
        (f32::NEG_INFINITY, f32::NEG_INFINITY)
    } else {
        strut(container)
    };
    let line_count = lines.len();
    let mut max_line_width = 0.0f32;
    let mut built = Vec::with_capacity(lines.len());
    for (line_index, mut line) in lines.into_iter().enumerate() {
        // Trailing collapsible spaces are removed at the line end, also
        // before a forced break.
        let forced = line.last().is_some_and(|&k| items[k].forced);
        let brk = if forced { line.pop() } else { None };
        while !preserve && line.last().is_some_and(|&k| items[k].whitespace_only) {
            line.pop();
        }
        line.extend(brk);
        let content_len = line.len() - usize::from(forced);
        if line.is_empty() {
            continue;
        }
        // The item whose trailing spaces hang (the logical line end).
        let hang_item = content_len.checked_sub(1).map(|p| line[p]);
        if line[..content_len]
            .iter()
            .any(|&k| items[k].bidi.tail && Some(k) != hang_item)
        {
            // Spaces at another level inside the line would sit apart from
            // their item after reordering.
            return None;
        }
        // UAX #9 L1/L2: the line's items in visual order (the <br> stays
        // at the end; it paints nothing).
        let logical = line;
        let line: Vec<usize> = if reorder {
            let item_levels: Vec<u8> = logical[..content_len]
                .iter()
                .map(|&k| items[k].bidi.level)
                .collect();
            visual_order(&item_levels)
                .into_iter()
                .map(|p| logical[p])
                .chain(logical[content_len..].iter().copied())
                .collect()
        } else {
            logical
        };
        let mut ascent = strut_ascent;
        let mut descent = strut_descent;
        let mut subtrees: Vec<(usize, bool, f32, f32)> = Vec::new();
        let mut placed = Vec::with_capacity(line.len());
        let mut slice_start = 0.0f32;
        let mut slice = 0.0f32;
        for (position, &k) in line.iter().enumerate() {
            let item = &items[k];
            let last_on_line = Some(k) == hang_item || item.forced;
            // Hanging preserved spaces keep their advance in the fragment.
            let advance = if last_on_line && !hang_preserved {
                item.raw_hang
            } else {
                item.raw
            };
            let x = slice_start + slice;
            slice += advance;
            let slice_ends = last_on_line
                || position + 1 >= content_len
                || items[line[position + 1]].run != item.run;
            let width = if slice_ends {
                let end = slice_start + snap(slice);
                slice_start = end;
                slice = 0.0;
                end - x
            } else {
                advance
            };
            placed.push((k, x, width));
            if let Some((root, top)) = item.aligned {
                match subtrees
                    .iter_mut()
                    .find(|s: &&mut (usize, bool, f32, f32)| s.0 == root)
                {
                    Some(s) => {
                        s.2 = s.2.max(item.ascent - item.shift);
                        s.3 = s.3.max(item.descent + item.shift);
                    }
                    None => subtrees.push((
                        root,
                        top,
                        item.ascent - item.shift,
                        item.descent + item.shift,
                    )),
                }
            } else {
                ascent = ascent.max(item.ascent - item.shift);
                descent = descent.max(item.descent + item.shift);
            }
        }
        let content_width = if hang_preserved {
            line_width(&items, &line)
        } else {
            slice_start
        };
        if can_wrap
            && content_width > available + EPSILON
            && line.iter().any(|&k| items[k].internal_break)
        {
            return None;
        }
        max_line_width = max_line_width.max(content_width);
        // CSS Text 3 4.1.3: before a forced break or the end of the block,
        // preserved spaces hang only conditionally: the part that fits
        // still counts for alignment.
        let align_width = if hang_preserved && line_index + 1 == line_count {
            content_width.max(slice_start.min(available))
        } else {
            content_width
        };
        // NGInlineLayoutStateStack: top/bottom subtrees are aligned after
        // the baseline-aligned part; a taller one grows the line box away
        // from the edge it is aligned to.
        if ascent == f32::NEG_INFINITY {
            // (Quirks, nothing baseline-aligned.)
            ascent = 0.0;
            descent = 0.0;
        }
        for &(_, top, a, d) in &subtrees {
            let extra = (a + d) - (ascent + descent);
            if extra > 0.0 {
                if top {
                    descent += extra;
                } else {
                    ascent += extra;
                }
            }
        }
        built.push(Line {
            items: placed,
            width: align_width,
            ascent,
            descent,
            subtrees,
        });
    }

    let mut top = 0.0f32;
    let mut last_line_baseline = None;
    let mut out = Vec::with_capacity(items.len() + entries.len() - children.len());
    // (line, inline offset, width, baseline) of each placed item.
    let mut placed: Vec<Option<(usize, f32, f32, f32)>> = vec![None; items.len()];
    for (line_index, line) in built.into_iter().enumerate() {
        let offset = align_offset(container, available - line.width);
        let baseline = top + line.ascent;
        last_line_baseline = Some(baseline);
        for (k, x, width) in line.items {
            let item = &items[k];
            let baseline = match item.aligned {
                None => baseline,
                Some((root, _)) => {
                    let &(_, top_aligned, a, d) = line.subtrees.iter().find(|s| s.0 == root)?;
                    if top_aligned {
                        top + a
                    } else {
                        top + line.ascent + line.descent - d
                    }
                }
            };
            if item.forced {
                // The engine's <br> marker: a zero-height full-width rect at
                // the line end, painting nothing.
                out.push((
                    0.0,
                    top + line.ascent + line.descent,
                    BoxFragment {
                        index: item.index,
                        width: available,
                        height: 0.0,
                        children: Vec::new(),
                    },
                ));
                continue;
            }
            placed[k] = Some((line_index, offset + x, width, baseline));
            // The item's own baseline after its vertical-align shift (the
            // enclosing boxes stay on the unshifted one).
            let baseline = baseline + item.shift;
            if let Some((fragment, margin_start, margin_top)) = &item.atomic {
                out.push((
                    offset + x + item.box_start + margin_start,
                    baseline - item.ascent + margin_top,
                    fragment.clone(),
                ));
                continue;
            }
            // The engine's rect is the item's border box: margins excluded,
            // block-axis padding/border around the font content box.
            let e = item.edges;
            out.push((
                offset + x + item.box_start + e.margin_start,
                baseline - item.content_ascent - e.top,
                BoxFragment {
                    index: item.index,
                    width: width - item.box_start - item.box_end - e.margin_start - e.margin_end,
                    height: item.font_height + e.top + e.bottom,
                    children: Vec::new(),
                },
            ));
        }
        top += line.ascent + line.descent;
    }
    // Inline box fragments (NGInlineBoxState::CreateBoxFragment): the border
    // box from the first to the last item's edges, block-axis padding and
    // border around the box's font content area.  A box split across lines
    // has several fragments; the engine keeps one rect, so legacy lays it out.
    if has_boxes {
        let mut starts: std::collections::HashMap<usize, (usize, f32, f32)> =
            std::collections::HashMap::new();
        for (k, boxes) in opens.iter().enumerate() {
            for (pos, &b) in boxes.iter().enumerate() {
                let (line, x, _, baseline) = placed[k]?;
                let start =
                    x + start_edges(&boxes[..pos]) + InlineEdges::of(flat[b].style).margin_start;
                starts.insert(b, (line, start, baseline));
            }
        }
        for (k, boxes) in closes.iter().enumerate() {
            for (pos, &b) in boxes.iter().enumerate() {
                let (line, x, width, _) = placed[k]?;
                let (start_line, start, baseline) = starts.get(&b).copied()?;
                if start_line != line {
                    return None;
                }
                let style = flat[b].style;
                let e = InlineEdges::of(style);
                let end = x + width - end_edges(&boxes[pos + 1..]) - e.margin_end;
                out.push((
                    start,
                    baseline - inline_font_content_ascent(style) - e.top,
                    BoxFragment {
                        index: b,
                        width: end - start,
                        height: inline_font_height(style) + e.top + e.bottom,
                        children: Vec::new(),
                    },
                ));
            }
        }
    }
    Some(InlineResult {
        height: top,
        children: out,
        last_baseline: last_line_baseline.map_or(LastBaseline::NoLines, LastBaseline::At),
        max_line_width,
    })
}

/// Min/max-content sizes of a paragraph (block-level or sole line-width)
/// text leaf, which paint wraps itself: max-content is the unwrapped
/// advance (widest forced-break segment); min-content the widest word where
/// spaces are the only opportunities (Latin-1 without other soft-wrap
/// characters), the whole text for nowrap / pre.
pub(super) fn text_min_max(content: &str, style: &Style) -> MinMax {
    let width = |t: &str| snap(text_intrinsic_size(t, style).0);
    let max = content
        .split(['\n', '\u{2028}', '\u{2029}'])
        .map(width)
        .fold(0.0f32, f32::max);
    let forced = has_forced_break(content, style)
        || (content.contains('\n') && !matches!(style.white_space, WhiteSpace::Pre));
    let min = if matches!(style.white_space, WhiteSpace::NoWrap | WhiteSpace::Pre) {
        Some(max)
    } else if !forced
        && latin1(content)
        && !content.chars().any(|c| c != ' ' && is_soft_wrap_char(c))
        && style.white_space != WhiteSpace::PreWrap
    {
        Some(content.split(' ').map(width).fold(0.0f32, f32::max))
    } else {
        None
    };
    MinMax { min, max }
}

/// NGInlineNode::ComputeMinMaxSizes: the line breaker's widest line at
/// available 0 (every opportunity taken) and at an indefinite size (only
/// forced breaks).  A paragraph leaf is measured as text.
pub(super) fn inline_min_max(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
) -> Option<MinMax> {
    let entries = collect_inlines(flat, index);
    if let [InlineEntry::Leaf(child)] = entries[..]
        && !is_atomic_inline(flat, child)
        && let ComponentKind::Text { content } = flat[child].kind
        && (has_marker(flat[child].style, LINE_WIDTH_MARKER) || content.contains('\n'))
    {
        return Some(text_min_max(content, flat[child].style));
    }
    let at = |available: f32| {
        layout_inline(
            flat,
            index,
            &ConstraintSpace {
                available_inline: available,
                percentage_block: None,
                ..*space
            },
        )
        .map(|result| result.max_line_width)
    };
    let max = at(super::block::INDEFINITE_INLINE)?;
    Some(MinMax {
        min: at(0.0).map(|min| min.min(max)),
        max,
    })
}
