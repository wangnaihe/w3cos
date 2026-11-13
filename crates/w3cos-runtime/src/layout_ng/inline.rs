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

use super::Why;
use super::block::MinMax;
use super::{BoxFragment, ConstraintSpace, LastBaseline};
use crate::layout::{
    FlatNodeInfo, inline_font_baseline_from_line_top, inline_font_content_ascent,
    inline_font_height, inline_style_line_height, inline_used_line_height, text_intrinsic_size,
    wrapped_text_height,
};
use w3cos_std::ComponentKind;
use w3cos_std::style::{
    Dimension, Display, Edges, Float, LineBreak, Position, Style, TextAlign, TextDirection,
    WhiteSpace, WordBreak,
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
/// Line count of a `-webkit-line-clamp` box (`block::webkit_box_styles`
/// sets it on the block-flow `-webkit-box` it lays out).
pub(super) const CLAMP_MARKER: &str = "--w3cos-internal-clamp-lines";
/// CSS Overflow 4 `max-lines` from the `line-clamp` / `-webkit-line-clamp`
/// shorthands.
const LINE_CLAMP_MARKER: &str = "--w3cos-internal-line-clamp";
/// CSS Overflow 4 `continue`. `collapse` clamps any block container;
/// `webkit-legacy` clamps only through `CLAMP_MARKER` on a vertical
/// `-webkit-box`.
const CONTINUE_MARKER: &str = "--w3cos-internal-continue";

pub(super) fn clamp_lines(style: &Style) -> Option<usize> {
    // Standard `line-clamp: N` (`continue: collapse`) applies to any block
    // container. `-webkit-line-clamp` uses `continue: -webkit-legacy` and
    // only counts lines when webkit_box_styles copied `CLAMP_MARKER`.
    // `line-clamp: auto` keeps as many lines as fit in a definite
    // `max-height` / `height` (CSS Overflow 4).
    if marker_value(style, CONTINUE_MARKER) == Some("collapse") {
        return match marker_value(style, LINE_CLAMP_MARKER) {
            Some("auto") => auto_clamp_lines(style),
            Some(raw) => raw.parse().ok(),
            // `continue: collapse` without max-lines is the same as
            // `line-clamp: auto` (CSS Overflow 4; continue-001).
            None => auto_clamp_lines(style),
        };
    }
    marker_value(style, CLAMP_MARKER)?.parse().ok()
}

/// Whether this box's own clamp inserts a block-ellipsis (`line-clamp`
/// and `-webkit-line-clamp` set `block-ellipsis: auto`; `continue:
/// collapse` alone does not).
pub(super) fn clamp_has_ellipsis(style: &Style) -> bool {
    marker_value(style, LINE_CLAMP_MARKER).is_some() || marker_value(style, CLAMP_MARKER).is_some()
}

/// Line budget and ellipsis flag for this IFC: the box's own `line-clamp`
/// wins, otherwise a same-BFC ancestor budget on the constraint space.
/// An inherited height cap becomes `floor(remaining_block / used lh)`.
fn clamp_budget(style: &Style, space: &super::ConstraintSpace) -> Option<(usize, bool)> {
    if let Some(n) = clamp_lines(style) {
        return Some((n, clamp_has_ellipsis(style)));
    }
    let c = space.line_clamp?;
    let from_lines = c.remaining;
    let from_height = c.remaining_block.map(|cap| {
        let line = style.used_line_height_px();
        if line <= EPSILON {
            0
        } else {
            (cap / line + EPSILON).floor().max(0.0) as usize
        }
    });
    match (from_lines, from_height) {
        (Some(n), Some(h)) => Some((n.min(h), c.ellipsis)),
        (Some(n), None) => Some((n, c.ellipsis)),
        (None, Some(h)) => Some((h, c.ellipsis)),
        (None, None) => None,
    }
}

/// `line-clamp: auto` or `continue: collapse` without an integer
/// `max-lines`, when a definite height/`max-height` cap exists.
pub(super) fn clamp_is_auto(style: &Style) -> bool {
    marker_value(style, CONTINUE_MARKER) == Some("collapse")
        && matches!(marker_value(style, LINE_CLAMP_MARKER), Some("auto") | None)
        && auto_clamp_lines(style).is_some()
}

/// How many line boxes fit in the content-box block size cap used by
/// `line-clamp: auto`. `None` means the size is indefinite (no clamp).
fn auto_clamp_lines(style: &Style) -> Option<usize> {
    let cap = definite_clamp_block(style)?;
    let line = style.used_line_height_px();
    if line <= EPSILON {
        return Some(0);
    }
    Some((cap / line + EPSILON).floor().max(0.0) as usize)
}

/// Definite content-box block size from `height` and/or `max-height`.
/// `min-height` is not a clamp cap (line-clamp-auto-012).
pub(super) fn definite_clamp_block(style: &Style) -> Option<f32> {
    let resolve = |d: Dimension| -> Option<f32> {
        match d {
            Dimension::Px(v) => Some(v),
            Dimension::Em(v) => Some(v * style.font_size),
            Dimension::Rem(v) => Some(v * crate::layout::ROOT_FONT_SIZE),
            Dimension::Lh(v) => Some(v * style.used_line_height_px()),
            Dimension::Ch(v) => Some(v * style.ch_advance()),
            _ => None,
        }
    };
    let height = resolve(style.height);
    let max_height = resolve(style.max_height);
    let min_height = resolve(style.min_height);
    // `min-height` alone is not a clamp cap (auto-012). When both
    // min and max are definite, the used constraint is max(min, max)
    // (CSS 2.1 10.7; auto-014).
    let used = match (height, max_height, min_height) {
        (None, None, _) => return None,
        (Some(h), Some(m), Some(n)) => h.min(m).max(n),
        (Some(h), Some(m), None) => h.min(m),
        (Some(h), None, Some(n)) => h.max(n),
        (Some(h), None, None) => h,
        (None, Some(m), Some(n)) => m.max(n),
        (None, Some(m), None) => m,
        (None, None, _) => unreachable!(),
    };
    let padding = style.padding_lengths();
    let bt = style.border_top_width.unwrap_or(0.0);
    let bb = style.border_bottom_width.unwrap_or(0.0);
    let edges = padding.top + padding.bottom + bt + bb;
    let content = if style.box_sizing == w3cos_std::style::BoxSizing::BorderBox {
        (used - edges).max(0.0)
    } else {
        used
    };
    Some(content)
}

/// The substring of a text leaf this item paints (the item's range, or
/// the whole leaf when it has none).
fn clamp_item_text<'a>(item: &InlineItem, content: &'a str) -> &'a str {
    item.range
        .and_then(|(a, b)| content.get(a..b))
        .unwrap_or(content)
}

/// Whether a last-kept-line text item can carry a block-ellipsis: paint
/// draws the piece verbatim, with only single U+0020 spaces between words.
/// The check looks at the item's range, not the whole leaf, so a
/// `white-space: normal` paragraph whose source still has newlines can
/// clamp. Collapsed whitespace (`\n` / `\t` / `\r`) is treated as space.
fn clamp_ellipsis_piece_ok(item: &InlineItem, content: &str, style: &Style) -> bool {
    if item.atomic.is_some() || !zero_edges(style) {
        return false;
    }
    let slice = clamp_item_text(item, content);
    let preserve = matches!(style.white_space, WhiteSpace::Pre | WhiteSpace::PreWrap);
    if preserve {
        return !slice.contains(|c: char| c.is_whitespace() && c != ' ')
            && !slice.contains("  ")
            && !(item.range.is_none() && (item.starts_space || item.ends_space));
    }
    true
}

/// Longest prefix of `slice` ending at a soft wrap opportunity that is at
/// most `max_width` wide (CSS Overflow 4 block-ellipsis).
fn last_wrap_prefix_fit(slice: &str, style: &Style, max_width: f32) -> Option<(usize, f32)> {
    let full = text_intrinsic_size(slice, style).0;
    if full <= max_width + EPSILON {
        return Some((slice.len(), full));
    }
    let mut best = None;
    let mut i = 0;
    while i < slice.len() {
        let rest = &slice[i..];
        let Some(rel) = rest.find(|c: char| c.is_whitespace()) else {
            break;
        };
        let at = i + rel;
        let prefix = &slice[..at];
        let width = text_intrinsic_size(prefix, style).0;
        if width <= max_width + EPSILON {
            best = Some((at, width));
        }
        let ch = rest[rel..]
            .chars()
            .next()
            .map(|c| c.len_utf8())
            .unwrap_or(1);
        i = at + ch;
    }
    best
}

/// If the last kept line cannot fit its text plus the ellipsis, drop
/// trailing items and shrink the last remaining item back to the last
/// wrap opportunity that still leaves room (or keep only the ellipsis).
fn trim_line_for_block_ellipsis(
    items: &[InlineItem],
    flat: &[FlatNodeInfo],
    line: &mut Line,
    dropped: &mut Vec<usize>,
    ellipsis_width: f32,
) -> Option<(usize, Option<(usize, usize)>)> {
    let available = line.available;
    loop {
        let k = line
            .items
            .iter()
            .rev()
            .map(|&(k, ..)| k)
            .find(|&k| !items[k].forced)?;
        let item = &items[k];
        let ComponentKind::Text { content } = &flat[item.index].kind else {
            return None;
        };
        if !clamp_ellipsis_piece_ok(item, content, flat[item.index].style) {
            return None;
        }
        if line.width + ellipsis_width <= available + EPSILON {
            return Some((k, None));
        }
        let slice = clamp_item_text(item, content);
        let start = item.range.map(|(a, _)| a).unwrap_or(0);
        let (x, item_width) = line
            .items
            .iter()
            .rev()
            .find(|&&(ik, ..)| ik == k)
            .map(|&(_, x, w)| (x, w))
            .unwrap_or((0.0, 0.0));
        let text_w = text_intrinsic_size(slice, flat[item.index].style).0;
        let extras = (item_width - text_w).max(0.0);
        let max_text = available - x - extras - ellipsis_width;
        if let Some((kept_end, kept_w)) =
            last_wrap_prefix_fit(slice, flat[item.index].style, max_text)
        {
            let new_w = kept_w + extras;
            if let Some(slot) = line.items.iter_mut().rev().find(|slot| slot.0 == k) {
                slot.2 = new_w;
            }
            line.width = x + new_w;
            let range = (kept_end < slice.len()).then_some((start, start + kept_end));
            return Some((k, range));
        }
        let content_items = line
            .items
            .iter()
            .filter(|&&(ik, ..)| !items[ik].forced && !items[ik].whitespace_only)
            .count();
        if content_items <= 1 {
            if ellipsis_width > available + EPSILON {
                return None;
            }
            if let Some(slot) = line.items.iter_mut().rev().find(|slot| slot.0 == k) {
                slot.2 = extras;
            }
            line.width = x + extras;
            return Some((k, Some((start, start))));
        }
        while let Some(&(ik, ..)) = line.items.last() {
            dropped.push(ik);
            line.items.pop();
            if ik == k {
                break;
            }
        }
        line.width = line.items.last().map(|&(_, x, w)| x + w).unwrap_or(0.0);
    }
}

fn marker_value<'a>(style: &'a Style, key: &str) -> Option<&'a str> {
    style
        .custom_properties
        .as_ref()?
        .get(key)
        .map(String::as_str)
}
/// The cascade's UAX #9 pass (reorder_explicit_bidi_*) replaces a bidi
/// paragraph's text by visual-order fragments: ltr, already reversed and
/// mirrored, ordered for the lines it estimated.
pub(super) const BIDI_VISUAL_ORDER_MARKER: &str = "--w3cos-internal-bidi-visual-order";
pub(super) const BIDI_FRAGMENT_MARKERS: [&str; 3] = [
    BIDI_VISUAL_ORDER_MARKER,
    "--w3cos-internal-bidi-resolved-level",
    "--w3cos-internal-bidi-logical-order",
];

fn bidi_fragment(style: &Style) -> bool {
    has_marker(style, BIDI_VISUAL_ORDER_MARKER)
}
pub(super) const LINE_WIDTH_MARKER: &str = "--w3cos-internal-text-line-width";

/// A sole line-width leaf: paint wraps and draws its whole text itself.
fn paragraph_leaf(style: &Style, sole: bool, container: &Style) -> bool {
    sole && line_width_leaf(style, container)
}

/// The DOM marks a block's sole text run `text-line-width` (paint wraps it
/// inside the block's line width).  A run coalesced from dissolved inlines
/// (#325: it carries CSSOM owner ranges) is not marked, although it is the
/// same paragraph: infer the marker from the DOM's conditions so it takes
/// the same path (and line model) as an equivalent unsplit run.
/// (main 7f1c12d / shaping-000: ع<span>ع</span>ع folds to one Text with
/// cssom-owner-ranges inside a definite-width RTL block.)
fn line_width_leaf(style: &Style, container: &Style) -> bool {
    has_marker(style, LINE_WIDTH_MARKER)
        || (has_marker(style, w3cos_std::inline_text::CSSOM_OWNER_RANGES)
            && matches!(
                container.white_space,
                WhiteSpace::Normal | WhiteSpace::PreLine
            )
            && !matches!(style.position, Position::Absolute | Position::Fixed)
            && (!matches!(container.width, Dimension::Auto)
                || container.text_indent != Dimension::Px(0.0)
                || container.text_align != TextAlign::Start
                || has_marker(container, "--w3cos-internal-text-align-last")))
}

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
pub(super) fn line_relative_align(style: &Style) -> Option<bool> {
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
    let x_height = x_height(parent).why("x-height")?;
    let half = w3cos_std::style::used_box_spacing((ascent - descent) / 2.0);
    Some(half - (x_height / 2.0 * 64.0).round() / 64.0)
}

/// Whether an inline box draws nothing of its own (no edges, background,
/// shadow, outline, decoration or effects): only then may its per-line
/// fragments collapse into the one rect the engine keeps per node.
fn paints_nothing(style: &Style) -> bool {
    zero_edges(style)
        && style.background.a == 0
        && style.background_image.is_none()
        && style.box_shadow.is_none()
        && style.box_shadows.is_empty()
        && (style.outline_style == w3cos_std::style::OutlineStyle::None
            || style.outline_width == 0.0)
        && style.opacity == 1.0
        && style.filter.is_none()
        && style.transform == w3cos_std::style::Transform2D::default()
        && style.text_decoration == w3cos_std::style::TextDecoration::None
}

/// A spacing LayoutNG's inline items can read without a percentage basis.
fn basis_free_spacing(s: w3cos_std::style::Spacing) -> bool {
    use w3cos_std::style::Spacing;
    match s {
        Spacing::Percent(p) => p == 0.0,
        Spacing::Calc { percent, .. } | Spacing::PaddingCalc { percent, .. } => percent == 0.0,
        _ => true,
    }
}

fn zero_edges(style: &Style) -> bool {
    // A percentage edge needs the containing block's width, which inline
    // items do not carry; it is not zero, and inline_edges_supported
    // sends it to legacy.
    if ![
        style.padding.top,
        style.padding.right,
        style.padding.bottom,
        style.padding.left,
        style.margin.left,
        style.margin.right,
    ]
    .into_iter()
    .all(basis_free_spacing)
    {
        return false;
    }
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

/// CSS Text 3 4.1.2: in `normal` / `nowrap`, a newline, tab, or form feed
/// is a collapsible space (same byte length as U+0020, so ranges stay valid
/// when the character is rewritten as a space for line breaking).
fn is_collapsible_space(c: char) -> bool {
    matches!(c, ' ' | '\n' | '\t' | '\u{0c}')
}

fn collapses_whitespace(style: &Style) -> bool {
    matches!(style.white_space, WhiteSpace::Normal | WhiteSpace::NoWrap)
}

/// Line-breaking text of an item: collapsed whitespace modes rewrite
/// source newlines / tabs / form feeds as U+0020 so UAX #14 does not
/// treat them as mandatory breaks (CSS Text 3 4.1.2).
fn as_collapsed_spaces<'a>(text: &'a str, style: &Style) -> std::borrow::Cow<'a, str> {
    if collapses_whitespace(style) && text.bytes().any(|b| matches!(b, b'\n' | b'\t' | 0x0c)) {
        std::borrow::Cow::Owned(text.replace(['\n', '\t', '\u{0c}'], " "))
    } else {
        std::borrow::Cow::Borrowed(text)
    }
}

fn ifc_break_opportunities(text: &str, style: &Style) -> Vec<usize> {
    let text = as_collapsed_spaces(text, style);
    crate::line_break::break_opportunities(&text, break_mode(style))
}

/// `word-break: auto-phrase` suppresses hyphenation, so a soft hyphen is
/// not a break opportunity (CSS Text 4 word-break).
fn suppresses_hyphenation(style: &Style) -> bool {
    has_marker(style, "--w3cos-internal-word-break-auto-phrase")
}

/// The run's crate::line_break mode. `word-break: auto-phrase` in Japanese
/// is Blink's LineBreakType::kPhrase: opportunities only at phrase
/// boundaries, none inside a phrase (keep-all). Phrase segmentation is not
/// modeled yet, so such text breaks only where keep-all would; other
/// languages fall back to `normal` (CSS Text 4).
fn break_mode(style: &Style) -> crate::line_break::BreakMode {
    let mut mode = crate::line_break::BreakMode::of(style);
    if mode.ja
        && suppresses_hyphenation(style)
        && mode.kind == crate::line_break::LineBreakType::Normal
    {
        mode.kind = crate::line_break::LineBreakType::KeepAll;
    }
    mode
}

/// Text style requirements shared by every S2a text leaf.
fn text_style_reason(content: &str, style: &Style) -> Option<&'static str> {
    // An inline-level relatively positioned leaf only moves its fragments
    // (`relative_inline_offsets`); a block-level one stays on legacy.
    let inline_relative = style.position == Position::Relative && style.display == Display::Inline;
    if (style.position != Position::Static && !inline_relative) || style.float != Float::None {
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
    // hanging-punctuation: last hangs the paragraph's final closing
    // bracket or quote (`mark_hanging_last`; a paragraph leaf through
    // crate::text_layout's BreakMode::hang_last_punctuation).
    // line-break: loose / strict / anywhere all reach the breaker through
    // crate::line_break::BreakMode (Blink LazyLineBreakIterator strictness /
    // BreakCharacter for anywhere). Branch already lays loose/strict on NG;
    // allowing anywhere matches main #404 without forcing loose/strict onto
    // legacy (paired losses on line-break-loose/strict-011..018).
    // break-spaces: the sole-leaf wrap runs through crate::line_break
    // (BreakSpace::AfterEverySpace); multi-item lines stay on legacy.
    // break-all / keep-all only change the opportunities
    // (crate::line_break::BreakMode); break-word adds emergency breaks.
    // break-word (overflow-wrap: break-word / anywhere, word-break:
    // break-word) only adds emergency breaks inside a word that overflows its
    // line; line building bails when one is needed ("break-word-overflow").
    if !matches!(
        style.word_break,
        WordBreak::Normal | WordBreak::BreakAll | WordBreak::KeepAll
    ) && !break_word_supported(style)
    {
        return Some("text:word-break");
    }
    if style.text_align == TextAlign::Justify {
        return Some("text:justify");
    }
    if !text_align_last_supported(style) {
        return Some("text:align-last");
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
///
/// `reordered`: a visual-order fragment, whose right-to-left characters
/// the cascade already reversed (paint draws and measures them as given).
fn supported_script_in(content: &str, reordered: bool) -> bool {
    content.chars().all(|c| {
        let u = c as u32;
        // (Preserved tabs are gated by `text_content_reason`.)
        if matches!(c, '\n' | '\t' | '\u{2028}' | '\u{2029}') {
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
        let bidi_control =
            matches!(u, 0x061C) || (0x202A..=0x202E).contains(&u) || (0x2066..=0x2069).contains(&u);
        // Break opportunities come from crate::line_break (UAX #14 via
        // ICU, as Blink's LazyLineBreakIterator): the ideographic space,
        // the zero width space and the other space separators of class BA
        // (U+1680, U+2000..U+2006, U+2008..U+200A, U+205F) allow a break
        // after them; the GL ones (U+2007, U+202F, U+180E, U+2060) never
        // do. None of them is a collapsible or hanging space in
        // NGLineBreaker (only U+0020 / tab trail; break-spaces aside), so
        // they keep their advance at the line end and in min-/max-content,
        // as Chrome measures. The joiners (U+200C, U+200D) and the
        // invisible operators (U+2061..U+2064) still stay on legacy.
        let other_space = matches!(u, 0x200C | 0x200D) || (0x2061..=0x2064).contains(&u);
        let special = (0xD800..=0xDFFF).contains(&u) || (0xFFF0..=0xFFFF).contains(&u);
        !((rtl && !reordered) || bidi_control || other_space || special)
    })
}

/// `line-height: normal` text whose fallback faces grow the line box past
/// the primary font's. Such a line box takes each used face's rounded
/// ascent / descent (Blink FontHeight::Unite over the item's fonts), and
/// its text fragment is that line box so paint sits on its baseline.
fn fallback_grows_line(content: &str, style: &Style) -> bool {
    #[cfg(feature = "skia")]
    if style.line_height_is_normal {
        // Tabs are never shaped (NGInlineItem kControl): they cannot pull
        // in a fallback face.
        let shaped: std::borrow::Cow<str> = if content.contains('\t') {
            content.replace('\t', "").into()
        } else {
            content.into()
        };
        if !latin1(&shaped) {
            return crate::render_skia::text_fallback_grows_normal_line(&shaped, style);
        }
    }
    let _ = (content, style);
    false
}

/// The content a text leaf cannot carry yet: an empty leaf, a carriage
/// return, a preserved tab (unless `tabs_ok`), or a preserved newline
/// (unless `newline_ok`).
fn text_content_reason(content: &str, newline_ok: bool, tabs_ok: bool) -> Option<&'static str> {
    if content.is_empty() {
        Some("text:empty")
    } else if content.contains('\r') {
        Some("text:cr")
    } else if content.contains('\t') && !tabs_ok {
        Some("text:tab")
    } else if content.contains('\n') && !newline_ok {
        Some("text:newline")
    } else {
        None
    }
}

/// Preserved tabs in a leaf that is its line's only content from the line
/// start (Blink NGInlineItemsBuilder keeps a preserved tab as its own
/// item; its ShapeResult advances to the next stop of the block's
/// `tab-size` interval from the line start, stops computed before
/// text-align).  The leaf's measured advance and paint then both resolve
/// the stops from offset 0: one `pre` LTR line, and the block's stops
/// equal the leaf's own (`block` is the leaf's own style for a block text
/// leaf).  Items past the first line (after a forced break) need their own
/// line position: legacy.
fn preserved_tabs_ok(content: &str, style: &Style, block: &Style) -> bool {
    // (pre-wrap: paint resolves block tab stops from the line box only for
    // `pre` text, InlineLineContext.)
    style.white_space == WhiteSpace::Pre
        && style.direction == TextDirection::Ltr
        && block.direction == TextDirection::Ltr
        && !content.contains(['\n', '\r', '\u{2028}', '\u{2029}'])
        && style.text_indent == Dimension::Px(0.0)
        && block.text_indent == Dimension::Px(0.0)
        && matches!(block.text_align, TextAlign::Start | TextAlign::Left)
        && style.font_family == block.font_family
        && style.font_size == block.font_size
        && style.font_weight == block.font_weight
        && style.font_style == block.font_style
        && style.font_variant == block.font_variant
        && style.letter_spacing == block.letter_spacing
        && style.word_spacing == block.word_spacing
}

/// A block-level text leaf (`<p>text</p>` lowered to one Text box).  Its
/// block-axis margins are handled by block layout.
pub(super) fn block_text_reason(content: &str, style: &Style) -> Option<&'static str> {
    if text_leaf_has_edges(style) {
        return Some("text:edges");
    }
    block_text_leaf_reason(content, style)
}

/// Padding or border on a text leaf (an element lowered to one Text box).
fn text_leaf_has_edges(style: &Style) -> bool {
    let padding = style.padding_lengths();
    [padding.top, padding.right, padding.bottom, padding.left]
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
}

/// [`block_text_reason`] for the text leaf `index`: an in-flow horizontal
/// leaf in a horizontal parent may carry padding and border, which block
/// layout puts around the line boxes like any other box edge (the text
/// paints at the content edge).  Orthogonal, vertical, out-of-flow and
/// percentage-edged leaves keep the bail.
pub(super) fn block_text_reason_at(
    flat: &[FlatNodeInfo],
    index: usize,
    content: &str,
) -> Option<&'static str> {
    let style = flat[index].style;
    if text_leaf_has_edges(style) {
        let horizontal = |s: &Style| s.writing_mode == w3cos_std::style::WritingMode::HorizontalTb;
        let parent_horizontal = flat[index]
            .parent
            .is_some_and(|parent| horizontal(flat[parent].style));
        if !(horizontal(style)
            && parent_horizontal
            && !super::oof::is_out_of_flow(style)
            // Grid items stay legacy (animated tracks reach only legacy), and
            // so do intrinsic width keywords: NG and legacy min-content
            // differ for text NG cannot shape (joiners), which a reftest's
            // NG-rendered reference would expose.
            && !flat[index]
                .parent
                .is_some_and(|parent| super::block::is_author_grid(flat[parent].style))
            && [
                super::block::WIDTH_KEYWORD,
                super::block::MIN_WIDTH_KEYWORD,
                super::block::MAX_WIDTH_KEYWORD,
            ]
            .into_iter()
            .all(|key| super::block::marker(style, key).is_none())
            && [
                style.padding.top,
                style.padding.right,
                style.padding.bottom,
                style.padding.left,
            ]
            .into_iter()
            .all(basis_free_spacing))
        {
            return Some("text:edges");
        }
    }
    block_text_leaf_reason(content, style)
}

fn block_text_leaf_reason(content: &str, style: &Style) -> Option<&'static str> {
    if super::oof::is_out_of_flow(style) {
        // An absolutely positioned text leaf (generated `::before` with
        // `position: absolute`) is blockified (CSS2 9.7) and is the block
        // text of its own OOF box; its position is the OOF layout's.
        let mut in_box = style.clone();
        in_box.position = Position::Static;
        return block_text_content_reason(content, &in_box);
    }
    block_text_content_reason(content, style)
}

/// [`block_text_reason`] without the box edges: the text of a block text
/// leaf, or the label of a button leaf whose edges are its own box's.
pub(super) fn block_text_content_reason(content: &str, style: &Style) -> Option<&'static str> {
    if let Some(reason) =
        text_content_reason(content, false, preserved_tabs_ok(content, style, style))
    {
        return Some(reason);
    }
    // (A visual-order fragment from the cascade's bidi pass: paint draws
    // the paragraph leaf as given.)
    if !supported_script_in(content, bidi_fragment(style)) {
        return Some("text:script");
    }
    // A block text leaf is its own paragraph.
    if !super::block::block_unicode_bidi_ok(style) {
        return Some("unicode-bidi");
    }
    text_style_reason(content, style)
}

/// Inline box edges the cascade put on a word item of a decorated inline
/// box (its first item carries the inline-start edges, its last the end
/// edges, every item the block-axis padding/border).  Absolute lengths
/// only; an edge next to a collapsible space would need the space handling
/// at inline box boundaries; forced breaks inside are left to legacy.
fn inline_edges_supported(content: &str, style: &Style) -> bool {
    use w3cos_std::style::Spacing;
    let absolute = |s: Spacing| {
        matches!(s, Spacing::Px(_) | Spacing::Em(_) | Spacing::Rem(_))
            || matches!(s, Spacing::Calc { percent, .. } | Spacing::PaddingCalc { percent, .. } if percent == 0.0)
    };
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
    first_line: bool,
    container: &Style,
) -> Option<&'static str> {
    if style.display != Display::Inline {
        return Some("text:display");
    }
    // A span's block-axis padding and a used line-height several times
    // the IFC's (split-inline-borders: padding 10px and line-height 200px
    // copied onto each word, normal line-height on the anonymous block).
    // The fragment side of that reftest carries split-inline-edge and is
    // already legacy; the authored reference must stay on legacy too, or
    // run-snapped advances move glyphs onto different device pixels.
    // A tall line-height with no padding (vertical-align fixtures) stays.
    let padding = style.padding_lengths();
    if (padding.top > 0.0 || padding.bottom > 0.0)
        && container.line_height > 0.0
        && style.line_height > container.line_height * 4.0
    {
        return Some("text:line-height");
    }
    // The paragraph and preserved-newline paths place the leaf without a
    // baseline shift.
    let line_width =
        has_marker(style, LINE_WIDTH_MARKER) || (sole && line_width_leaf(style, container));
    if valign_length(style).is_some() && (line_width || content.contains('\n')) {
        return Some("vertical-align-length");
    }
    // An embedding or isolate inside the paragraph needs bidi resolution
    // (around a sole leaf, the whole paragraph, it changes nothing).
    // (A visual-order fragment is laid out ltr whatever the paragraph's
    // direction.)
    let fragment = bidi_fragment(style);
    if (style.direction != container.direction && !fragment)
        || (style.unicode_bidi != w3cos_std::style::UnicodeBidi::Normal
            && !(sole && super::block::block_unicode_bidi_ok(style)))
    {
        return Some("direction");
    }
    // An rtl paragraph (UAX #9 paragraph level 1): paint orders a paragraph
    // leaf itself; word items get their levels from leaf_bidi_levels and
    // are reordered per line.  Preserved spaces that hang (pre-wrap) would
    // hang at the line's left end (L1): legacy.
    let paragraph = paragraph_leaf(style, sole, container);
    if container.direction == TextDirection::Rtl
        && !paragraph
        && style.white_space == WhiteSpace::PreWrap
    {
        return Some("bidi:white-space");
    }
    if !zero_edges(style) && (line_width || !inline_edges_supported(content, style)) {
        return Some("text:edges");
    }
    // Preserved newlines (CSS Text 3 4.1.1) in a sole leaf: paint breaks
    // the lines; a trailing one only through the paragraph wrapper.
    let newline_ok = sole
        && match style.white_space {
            WhiteSpace::PreLine => line_width,
            // (line-height: normal mixes the strut's computed line height
            // with the font's line spacing differently in legacy.)
            // Trailing newlines still stay on legacy (the paragraph
            // wrapper / letter-spacing-end-of-line path). `line-height:
            // normal` is allowed: the sole-leaf path uses the same strut
            // + face metrics as a specified unit line-height of 1em on
            // Ahem, which is Blink's used line box for these pre leaves.
            WhiteSpace::Pre | WhiteSpace::PreWrap => line_width || !content.ends_with('\n'),
            // Collapsed (not preserved) newlines are wrap spaces, CSS Text 3
            // 4.1.2. Only take this on a line-clamp container: a general
            // paragraph with source newlines still uses the legacy path
            // until IFC collapse is proven gate-safe.
            WhiteSpace::Normal | WhiteSpace::NoWrap => clamp_lines(container).is_some(),
        };
    // (`first_line`: the item's line offset is known when items are built;
    // see `tab_item_advance`.)
    let tabs_ok = (sole || first_line) && preserved_tabs_ok(content, style, container);
    if let Some(reason) = text_content_reason(content, newline_ok, tabs_ok) {
        return Some(reason);
    }
    // A fragment with a line break the cascade froze inside it (one
    // item per fragment here).
    if fragment && content.contains('\u{2028}') && content != FORCED_BREAK && !paragraph {
        return Some("bidi:frozen-break");
    }
    if !supported_script_in(content, fragment) {
        return Some("text:script");
    }
    if line_width {
        if !sole {
            return Some("text:line-width-shared");
        }
    } else {
        // vertical-align (middle maps to align-self; the other keywords and
        // lengths carry markers the marker gate rejects).
        if let Some(reason) = valign_reason(style) {
            return Some(reason);
        }
        // A top/bottom-aligned sole leaf is an ordinary aligned item
        // (`aligned` covers the leaf itself) unless it takes the
        // preserved-newline path, which places it without alignment.
        if sole && line_relative_align(style).is_some() && content.contains('\n') {
            return Some("vertical-align-keyword-newline");
        }
        // break-all / overflow-wrap add emergency breaks inside an item.
        if !matches!(
            style.word_break,
            w3cos_std::style::WordBreak::Normal
                | w3cos_std::style::WordBreak::KeepAll
                | w3cos_std::style::WordBreak::BreakAll
        ) && !break_word_supported(style)
        {
            return Some("text:word-break");
        }
        // BreakSpaceType::kAfterEverySpace between items is not modelled
        // (a paragraph leaf is wrapped by paint with crate::line_break).
        if style.break_spaces {
            return Some("text:break-spaces");
        }
        // Items may hold internal soft-wrap opportunities (the breaker falls
        // back when it would need one).  A <br> folded into a text leaf
        // (U+2028) becomes its own forced-break item (NGInlineItemsBuilder
        // kControl); a paragraph separator stays on legacy.
        if content.contains('\u{2029}') {
            return Some("text:internal-break");
        }
    }
    text_style_reason(content, style)
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
        // (An absolutely positioned one is blockified, CSS2 9.7.)
        && !super::oof::is_out_of_flow(flat[index].style)
}

/// An inline-block box: an atomic inline (NGInlineItem kAtomicInline),
/// laid out as its own block formatting context.
/// A text leaf the cascade gives `display: inline-block` (an inline-block
/// holding only text) is one too: a block text leaf as its own BFC.
pub(super) fn is_atomic_inline(flat: &[FlatNodeInfo], index: usize) -> bool {
    let inline_block = matches!(
        flat[index].style.display,
        Display::InlineBlock | Display::InlineTable
    );
    if matches!(
        flat[index].kind,
        ComponentKind::Image { .. }
            | ComponentKind::Canvas { .. }
            | ComponentKind::TextInput { .. }
            | ComponentKind::SvgDocument { .. }
            | ComponentKind::Button { .. }
    ) {
        // Replaced elements are atomic inlines (CSS 2.1 10.3.2). The UA
        // sheet gives img and canvas `inline-block`; an author `inline` is
        // the same kind of item (Blink LayoutReplaced is atomic either way).
        // So is a text field (Blink LayoutTextControl is an inline-block
        // whatever its inline display). A button is an inline-block
        // too (HTML §15.5.3: `inline` behaves as `inline-block`).
        return inline_block || flat[index].style.display == Display::Inline;
    }
    matches!(
        flat[index].kind,
        ComponentKind::Box
            | ComponentKind::Column
            | ComponentKind::Row
            | ComponentKind::Text { .. }
    ) && (inline_block || super::block::is_author_inline_flex(flat[index].style))
        // An absolutely positioned inline-block is blockified (CSS2 9.7).
        && !super::block::inline_level_oof(flat, index)
}

fn inline_level_child(flat: &[FlatNodeInfo], child: usize) -> bool {
    // A floated inline-block is a block box (CSS 2.1 §9.7). Counting it
    // as an atomic inline would make its block parent an IFC and send
    // the float back to `float:inline`. Replaced floats stay atomic so
    // their reason does not move from `float:inline` to `image`.
    if super::block::is_blockified_float(flat, child) {
        return false;
    }
    (matches!(flat[child].kind, ComponentKind::Text { .. })
        && flat[child].style.display == Display::Inline)
        || is_inline_box(flat, child)
        || is_atomic_inline(flat, child)
        // An OOF box with an inline original display is still an inline
        // item of its parent (NGInlineItem kOutOfFlowPositioned).
        || super::block::inline_level_oof(flat, child)
}

/// True when the block container's rendered children are inline content.
pub(super) fn is_inline_container(flat: &[FlatNodeInfo], index: usize) -> bool {
    if is_inline_box(flat, index) {
        return false;
    }
    let mut children = rendered_children(flat, index);
    let Some(first) = children.next() else {
        return false;
    };
    if !inline_level_child(flat, first) {
        return false;
    }
    // Only out-of-flow boxes: no line box is made (an empty line is not
    // laid out), so the box is a block container whose OOF children have
    // the static position of a zero-sized line.
    if rendered_children(flat, index).all(|child| super::oof::is_out_of_flow(flat[child].style)) {
        return false;
    }
    // A table cell whose later child is a block (or a nested table) is a
    // block container. The first-child check would call it an IFC, and the
    // line breaker then bails `ifc:atomic-or-span`, sending the whole page
    // back to the legacy engine. A reference without that cell stays on NG.
    if flat[index].style.display == Display::TableCell {
        return children.all(|child| inline_level_child(flat, child));
    }
    true
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
    if !matches!(style.position, Position::Static | Position::Relative)
        || style.float != Float::None
    {
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
        return Some("vertical-align-text-edge-box");
    }
    let absolute = |s: Spacing| {
        matches!(s, Spacing::Px(_) | Spacing::Em(_) | Spacing::Rem(_))
            || matches!(s, Spacing::Calc { percent, .. } | Spacing::PaddingCalc { percent, .. } if percent == 0.0)
    };
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
    // The box's strut (NGInlineBoxState text metrics, CSS 2.1 10.8.1) is
    // its own font. Direct non-space text in that font always gets a
    // fragment and is the usual source. An empty or space-only box still
    // has a strut from its font; those spaces may collapse (CSS 2.1 16.6.1)
    // and then paint nothing. A direct text child in a different font is
    // still outside this stage.
    let strut_text = rendered_children(flat, index).any(|child| {
        matches!(flat[child].kind, ComponentKind::Text { content }
            if !content.trim_matches(' ').is_empty() && content != FORCED_BREAK)
            && same_font(flat[child].style, style)
    });
    if !strut_text {
        let other_font_text = rendered_children(flat, index).any(|child| {
            matches!(flat[child].kind, ComponentKind::Text { .. })
                && !same_font(flat[child].style, style)
        });
        if other_font_text {
            return Some("inline-box:strut");
        }
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
    // Only a direct text child can carry a font the box does not own. An
    // element child with its own font (an inline box, or a block lowered to
    // inline) lays out through its own strut.
    // (main 7f1c12d / flexbox-baseline-multi-item-horiz-001 ref: unmarked
    // inline-block with a bigFont inline child must stay on NG.)
    if !has_marker(style, IFC_MARKER)
        && rendered_children(flat, index).any(|child| {
            matches!(flat[child].kind, ComponentKind::Text { .. })
                && !same_text_font(flat[child].style, style)
        })
    {
        return Some("ifc:unmarked");
    }
    if style.text_align == TextAlign::Justify {
        return Some("text:justify");
    }
    if !text_align_last_supported(style) {
        return Some("text:align-last");
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
    // A sole line-width leaf in a much larger font (block-non-replaced-
    // height-005: `font: 100px/1 Ahem` in a 16px serif IFC). Its reference
    // is an image, so that document is already legacy. LayoutUnit snaps on
    // the same page's word runs move a trailing glyph across a device
    // pixel; keep this document on legacy too.
    if let [InlineEntry::Leaf(c)] = entries[..]
        && !is_atomic_inline(flat, c)
        && has_marker(flat[c].style, LINE_WIDTH_MARKER)
        && flat[c].style.font_family != style.font_family
        && style.font_size > 0.0
        && flat[c].style.font_size >= style.font_size * 4.0
    {
        return Some("text:line-width-font");
    }
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
        // On the first line (no forced break or preserved newline in an
        // earlier leaf), a `pre` line never soft-wraps, so the item's line
        // offset is the advance of everything before it.
        let first_line = !entries[..position].iter().any(|entry| {
            matches!(entry, InlineEntry::Leaf(earlier)
                if matches!(&flat[*earlier].kind, ComponentKind::Text { content }
                    if content.contains(['\n', '\r', '\u{2028}', '\u{2029}'])))
        });
        if let Some(reason) = inline_text_reason(content, info.style, sole, first_line, style) {
            return Some(reason);
        }
        if content != FORCED_BREAK && content.contains('\u{2028}') {
            // Forced breaks folded into the leaf (split into kControl items
            // at item building): the forced-break rules below, with the
            // leaf's own font; visual-order and rtl paragraphs stay on legacy.
            if !matches!(style.white_space, WhiteSpace::Normal | WhiteSpace::NoWrap)
                || has_marker(flat[0].style, QUIRKS_MARKER)
                || !same_font(info.style, style)
                || style.direction == TextDirection::Rtl
                || bidi_fragment(info.style)
                || !zero_edges(info.style)
            {
                return Some("text:internal-break");
            }
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
    // Visual-order fragments: the paragraph is already reordered, and all
    // of its text must be (the cascade's order covers the whole paragraph).
    let texts = || {
        entries.iter().filter_map(|e| match *e {
            InlineEntry::Leaf(c)
                if !is_atomic_inline(flat, c)
                    && !matches!(flat[c].kind, ComponentKind::Text { content } if content == FORCED_BREAK) =>
            {
                Some(c)
            }
            _ => None,
        })
    };
    if texts().any(|c| bidi_fragment(flat[c].style)) {
        return (!texts().all(|c| bidi_fragment(flat[c].style))).then_some("bidi:fragment-mixed");
    }
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
    // Tabs are control items (never shaped), so they take the primary
    // font's metrics, not a fallback face's.
    #[cfg(feature = "skia")]
    if style.line_height_is_normal {
        let shaped = content.replace('\t', "");
        let text = if content.contains('\t') {
            shaped.as_str()
        } else {
            content
        };
        if !text.is_empty()
            && let Some(metrics) = crate::render_skia::resolved_text_font_geometry(text, style)
        {
            return (metrics.line_spacing(), metrics.ascent);
        }
    }
    let _ = content;
    (height, baseline)
}

/// Whether a text leaf painted alone in its rect (no inline line context)
/// puts its glyphs on the baseline `text_line_metrics` gives an IFC: the
/// line box is the font box, so paint's centring has nothing to offset.
pub(super) fn leaf_paints_like_ifc(content: &str, style: &Style) -> bool {
    #[cfg(feature = "skia")]
    {
        let Some(font) = crate::render_skia::resolved_font_geometry(style) else {
            return false;
        };
        let (line, _) = text_line_metrics(content, style);
        font.line_gap.abs() < 0.01 && (line - font.height()).abs() < 0.01
    }
    #[cfg(not(feature = "skia"))]
    {
        let _ = (content, style);
        true
    }
}

/// NGLineHeightMetrics of the root inline box (the block's strut).
pub(super) fn strut(style: &Style) -> (f32, f32) {
    let height = inline_used_line_height(inline_style_line_height(style));
    let ascent = inline_font_baseline_from_line_top(style, height);
    (ascent, height - ascent)
}

/// Typographic first-baseline metrics of the root inline box in a
/// transposed vertical IFC (`sideways`: under faces block-start).
fn sideways_strut(style: &Style) -> (f32, f32) {
    let (ascent, descent) = strut(style);
    (descent, ascent)
}

/// `text-box-trim` on this IFC container: (trim-start, trim-end).
/// Blink: node start/end trim (not fragmentainer); `text-box-edge: text`
/// removes half-leading outside the em box of the first/last line.
fn text_box_trim_ends(style: &Style) -> (bool, bool) {
    let trim = style
        .custom_properties
        .as_ref()
        .and_then(|properties| properties.get("--w3cos-internal-text-box-trim"))
        .map(String::as_str);
    let edge = style
        .custom_properties
        .as_ref()
        .and_then(|properties| properties.get("--w3cos-internal-text-box-edge"))
        .map(String::as_str);
    // Only an authored `text-box-edge: text` is implemented so far.
    // Initial/`auto` is left alone: unclassed start-001 refs assume no
    // trim until a variant sets the edge class.
    if edge != Some("text") {
        return (false, false);
    }
    match trim {
        Some("trim-start") => (true, false),
        Some("trim-end") => (false, true),
        Some("trim-both") => (true, true),
        _ => (false, false),
    }
}

#[derive(Debug, Clone)]
struct InlineItem {
    index: usize,
    /// Position of the item's leaf among the IFC children (box edges,
    /// bidi levels); split pieces of one leaf share it.
    pos: usize,
    /// Byte range of the leaf's text this item covers (NGInlineItem
    /// start/end offsets); `None` is the whole text.
    range: Option<(usize, usize)>,
    /// Raw advance in its run's shaping context (unsnapped).
    raw: f32,
    /// Raw advance without trailing collapsible spaces (they hang).
    raw_hang: f32,
    /// `hanging-punctuation: last`: advance of the closing bracket or quote
    /// ending the paragraph's last item (CSS Text 3 8.2), which hangs and so
    /// is not measured for fit or alignment. Zero elsewhere.
    hang_last: f32,
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
    /// Resolved bidi level and trailing-space level change (LeafLevel).
    bidi: LeafLevel,
    /// Baseline shift (down) from vertical-align: middle.
    shift: f32,
    ascent: f32,
    descent: f32,
    content_ascent: f32,
    font_height: f32,
}

/// A soft-wrap opportunity inside the item's non-space core: the engine's
/// legacy set (spaces, hyphens, CJK) or any UAX #14 opportunity of
/// crate::line_break.
pub(super) fn internal_opportunity(content: &str, style: &Style) -> bool {
    let core = content.trim_matches(' ');
    let unhyphenated;
    let core = if suppresses_hyphenation(style) && core.contains('\u{ad}') {
        unhyphenated = core.replace('\u{ad}', "");
        unhyphenated.as_str()
    } else {
        core
    };
    if style.word_break != WordBreak::BreakAll
        && style.line_break != LineBreak::Anywhere
        && core.bytes().all(|b| b.is_ascii_alphanumeric())
    {
        // FillAscii: no opportunity between ASCII letters and digits
        // (break-all adds one between any two; line-break: anywhere is
        // BreakCharacter, every grapheme).
        return false;
    }
    // keep-all / auto-phrase remove the CJK opportunities of the legacy
    // set: only crate::line_break's own count there.
    let keep_all = style.word_break == WordBreak::KeepAll || suppresses_hyphenation(style);
    (!keep_all && core.chars().any(is_soft_wrap_char))
        || ifc_break_opportunities(core, style)
            .iter()
            .any(|&o| o < core.len())
}

/// Whether `split_text_item` has an opportunity to try inside `item`: a
/// crate::line_break opportunity past its leading non-space text.
fn has_split_opportunity(flat: &[FlatNodeInfo], item: &InlineItem) -> bool {
    let text = item_text(flat, item);
    ifc_break_opportunities(text, flat[item.index].style)
        .into_iter()
        .any(|o| o > 0 && o < text.len() && !text[..o].trim_end_matches(' ').is_empty())
}

/// A UAX #14 break opportunity between two adjacent text items
/// (crate::line_break over their concatenated text, with the first item's
/// line-break / word-break).
fn text_boundary_opportunity(flat: &[FlatNodeInfo], prev: &InlineItem, next: &InlineItem) -> bool {
    if prev.atomic.is_some() || next.atomic.is_some() || prev.forced || next.forced {
        return false;
    }
    let (a, b) = (item_text(flat, prev), item_text(flat, next));
    if a.is_empty() || b.is_empty() || a == FORCED_BREAK || b == FORCED_BREAK {
        return false;
    }
    let joined = format!("{a}{b}");
    ifc_break_opportunities(&joined, flat[prev.index].style)
        .binary_search(&a.len())
        .is_ok()
}

/// ShapeResult::SnappedWidth.
fn snap(advance: f32) -> f32 {
    crate::text_layout::inline_layout_advance(advance)
}

fn same_text_font(a: &Style, b: &Style) -> bool {
    same_font(a, b)
        || (a.line_height_is_normal
            && b.line_height_is_normal
            && (a.line_height - b.line_height).abs() < 0.01
            && a.font_family == b.font_family
            && a.font_size == b.font_size
            && a.font_weight == b.font_weight
            && a.font_style == b.font_style)
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

/// A text item holding preserved tabs (the gate keeps it on the first line
/// of a `pre` LTR IFC): Blink shapes the tab from the item's line position
/// (HarfBuzzShaper tab stops from the text run's x position), so its
/// advance is measured from the end of the earlier items plus the item's
/// own start edges.  Paint resolves the same stops from the run's x in the
/// line box.
fn tab_item_advance(
    content: &str,
    style: &Style,
    container: &Style,
    earlier: &[InlineItem],
    start_edges: f32,
) -> Option<f32> {
    if !content.contains('\t') || earlier.iter().any(|item| item.forced) {
        return None;
    }
    #[cfg(feature = "skia")]
    {
        let offset = earlier.iter().map(|item| item.raw).sum::<f32>() + start_edges;
        crate::render_skia::preserved_tab_advance_in_block(content, style, container, offset)
    }
    // Without Skia tabs are measured from the line start (no stops).
    #[cfg(not(feature = "skia"))]
    {
        let _ = (style, container, start_edges);
        None
    }
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
    // One piece, or Skia shaping declined: the unwrapped advance. Without
    // Skia, `text_intrinsic_size` wraps at 360px, so a long item would look
    // like it fits a wider line (the runtime-bench text edit).
    texts.iter().map(|t| max_content_width(t, style)).collect()
}

/// Raw advance of `content` without its trailing spaces.
fn hang_advance(content: &str, raw: f32, style: &Style) -> f32 {
    let trimmed = if collapses_whitespace(style) {
        content.trim_end_matches(is_collapsible_space)
    } else {
        content.trim_end_matches(' ')
    };
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

/// The text an item covers (its leaf's text, or a split piece of it).
fn item_text<'a>(flat: &'a [FlatNodeInfo], item: &InlineItem) -> &'a str {
    let full = match &flat[item.index].kind {
        ComponentKind::Text { content } => content.as_str(),
        _ => "",
    };
    match item.range {
        Some((start, end)) => &full[start..end],
        None => full,
    }
}

/// Cumulative character advances of `text`, plus the horizontal padding
/// `max_content_width` adds. `None` when that width is not the snapped sum
/// of per-character advances (a tab, Skia shaping, or a named family).
fn additive_char_prefix(text: &str, style: &Style) -> Option<(Vec<f32>, f32)> {
    // This branch measures preserved tabs from line position
    // (`tab_item_advance`); their width is not a sum of char advances.
    if text.contains('\t') {
        return None;
    }
    #[cfg(feature = "skia")]
    {
        return None;
    }
    #[cfg(not(feature = "skia"))]
    {
        if style.font_family.is_some() {
            return None;
        }
        let padding = style.padding_lengths();
        let pad = padding.left + padding.right;
        let font = crate::layout::layout_font();
        let mut prefix = Vec::with_capacity(text.len() + 1);
        prefix.push(0.0);
        let mut sum = 0.0f32;
        for ch in text.chars() {
            sum += crate::text_layout::char_advance(ch, style.font_size, font);
            for _ in 0..ch.len_utf8() {
                prefix.push(sum);
            }
        }
        Some((prefix, pad))
    }
}

/// `hang_advance` from a prefix built by [`additive_char_prefix`].
fn hang_from_prefix(piece: &str, raw: f32, prefix: &[f32], start: usize, pad: f32) -> f32 {
    let trimmed = piece.trim_end_matches(' ').len();
    if trimmed == 0 {
        return 0.0;
    }
    if trimmed == piece.len() {
        return raw;
    }
    let spaces = snap(prefix[start + piece.len()] - prefix[start + trimmed]) + pad;
    (raw - spaces).max(0.0)
}

/// NGLineBreaker breaking inside a text item: split `item` at the latest
/// UAX #14 opportunity (crate::line_break over the item's text) for which
/// `fits(prefix)` holds, or, with `overflow`, at its first opportunity.
/// The prefix keeps the opening box edges, the rest the closing ones; both
/// are measured as one shaping run (NGShapeResult::SubRange).
fn split_text_item(
    flat: &[FlatNodeInfo],
    item: &InlineItem,
    overflow: bool,
    mut fits: impl FnMut(&InlineItem) -> bool,
) -> Option<(InlineItem, InlineItem)> {
    let style = flat[item.index].style;
    if item.atomic.is_some() || item.forced || !zero_edges(style) {
        return None;
    }
    let text = item_text(flat, item);
    let base = item.range.map_or(0, |r| r.0);
    let candidates: Vec<usize> = ifc_break_opportunities(text, style)
        .into_iter()
        .filter(|&o| o > 0 && o < text.len() && !text[..o].trim_end_matches(' ').is_empty())
        .collect();
    if candidates.is_empty() {
        return None;
    }
    // Char advances add. A binary search that remeasures both sides of every
    // probe repeats that sum for the whole remaining paragraph on each line.
    // The prefix is the same sum `max_content_width` uses, so the snapped
    // slice widths stay identical. Shaped faces (Skia, a named family) do
    // not take this path.
    let additive = additive_char_prefix(text, style);
    let piece = |o: usize| {
        let (head, tail) = text.split_at(o);
        let (head_width, tail_width) = if let Some((prefix, pad)) = additive.as_ref() {
            (
                snap(prefix[o] - prefix[0]) + pad,
                snap(prefix[text.len()] - prefix[o]) + pad,
            )
        } else {
            let adv = run_advances(&[head, tail], style);
            (adv[0], adv[1])
        };
        let hang = |piece_text: &str, raw: f32, start: usize| {
            if let Some((prefix, pad)) = additive.as_ref() {
                hang_from_prefix(piece_text, raw, prefix, start, *pad)
            } else {
                hang_advance(piece_text, raw, style)
            }
        };
        let mut a = item.clone();
        a.range = Some((base, base + o));
        a.raw = head_width + item.box_start;
        a.raw_hang = hang(head, head_width, 0) + item.box_start;
        a.hang_last = 0.0;
        a.ends_space = if collapses_whitespace(style) {
            head.ends_with(is_collapsible_space)
        } else {
            head.ends_with(' ')
        };
        a.whitespace_only = if collapses_whitespace(style) {
            head.trim_end_matches(is_collapsible_space).is_empty()
        } else {
            head.trim_end_matches(' ').is_empty()
        };
        a.internal_break = internal_opportunity(head, style);
        a.box_end = 0.0;
        let mut b = item.clone();
        b.range = Some((base + o, base + text.len()));
        b.raw = tail_width + item.box_end;
        b.raw_hang = hang(tail, tail_width, o) + item.box_end;
        b.starts_space = if collapses_whitespace(style) {
            tail.starts_with(is_collapsible_space)
        } else {
            tail.starts_with(' ')
        };
        b.whitespace_only = if collapses_whitespace(style) {
            tail.trim_end_matches(is_collapsible_space).is_empty()
        } else {
            tail.trim_end_matches(' ').is_empty()
        };
        b.internal_break = internal_opportunity(tail, style);
        b.box_start = 0.0;
        (a, b)
    };
    // Prefix advances grow with the offset: the latest fitting one by
    // binary search.
    let (mut lo, mut hi) = (0usize, candidates.len());
    let mut best = None;
    while lo < hi {
        let mid = (lo + hi) / 2;
        let pieces = piece(candidates[mid]);
        if fits(&pieces.0) {
            best = Some(pieces);
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    best.or_else(|| overflow.then(|| piece(candidates[0])))
}

/// NGInlineItemsBuilder: a text leaf holding folded `<br>`s (U+2028) is
/// emitted as text items around kControl forced-break items, all sharing the
/// leaf (`range` selects each piece).  Leading collapsible spaces after a
/// break get their own item so the breaker drops them at the line start;
/// the enclosing boxes' edges stay on the first and last piece.
fn split_forced_breaks(
    content: &str,
    whole: InlineItem,
    style: &Style,
    hang_preserved: bool,
    items: &mut Vec<InlineItem>,
) -> Option<()> {
    let mut pieces: Vec<(usize, usize, bool)> = Vec::new();
    let mut at = 0;
    for (offset, _) in content.match_indices('\u{2028}') {
        if offset > at {
            pieces.push((at, offset, false));
        }
        let end = offset + '\u{2028}'.len_utf8();
        pieces.push((offset, end, true));
        at = end;
    }
    if at < content.len() {
        pieces.push((at, content.len(), false));
    }
    // Split off leading spaces of text that follows a break.
    let mut split: Vec<(usize, usize, bool)> = Vec::with_capacity(pieces.len());
    for (n, &(a, b, forced)) in pieces.iter().enumerate() {
        let lead = content[a..b].len() - content[a..b].trim_start_matches(' ').len();
        if !forced && n > 0 && lead > 0 && lead < b - a {
            split.push((a, a + lead, false));
            split.push((a + lead, b, false));
        } else {
            split.push((a, b, forced));
        }
    }
    let first_forced = split.first().is_some_and(|p| p.2);
    let last_forced = split.last().is_some_and(|p| p.2);
    if (first_forced && whole.box_start > 0.0) || (last_forced && whole.box_end > 0.0) {
        // A break at a box edge: the <br> would carry the box's edge.
        return super::bail("internal-break-edges");
    }
    let texts: Vec<&str> = split
        .iter()
        .filter(|p| !p.2)
        .map(|&(a, b, _)| &content[a..b])
        .collect();
    let advances = run_advances(&texts, style);
    let mut next = advances.into_iter();
    let count = split.len();
    // Metrics come from the pieces' own text (NGInlineItem shape results):
    // the U+2028 itself is a control item, never shaped, so it must not
    // pull in a fallback font.  A break takes the leaf's text metrics.
    let unbroken = content.replace('\u{2028}', "");
    let metrics = |text: &str| -> (f32, f32) {
        let source = if text.is_empty() {
            unbroken.as_str()
        } else {
            text
        };
        let (height, baseline) = text_line_metrics(source, style);
        (baseline, height - baseline)
    };
    for (n, &(a, b, forced)) in split.iter().enumerate() {
        let mut piece = whole.clone();
        (piece.ascent, piece.descent) = metrics(if forced { "" } else { &content[a..b] });
        piece.range = Some((a, b));
        piece.box_start = if n == 0 { whole.box_start } else { 0.0 };
        piece.box_end = if n + 1 == count { whole.box_end } else { 0.0 };
        piece.forced = forced;
        if forced {
            piece.raw = 0.0;
            piece.raw_hang = 0.0;
            piece.starts_space = false;
            piece.ends_space = false;
            piece.whitespace_only = false;
            piece.internal_break = false;
        } else {
            let text = &content[a..b];
            let adv = next.next()?;
            let edges = piece.box_start + piece.box_end;
            piece.raw = adv + edges;
            piece.raw_hang = hang_advance(text, adv, style) + edges;
            piece.starts_space = text.starts_with(' ');
            piece.ends_space = text.ends_with(' ');
            piece.whitespace_only = text.trim_end_matches(' ').is_empty();
            piece.internal_break = internal_opportunity(text, style)
                || (hang_preserved
                    && text.starts_with(' ')
                    && !text.trim_start_matches(' ').is_empty());
        }
        items.push(piece);
    }
    Some(())
}

/// `hanging-punctuation: last` (CSS Text 3 8.2): a closing bracket or
/// quote at the very end of the paragraph hangs past the end edge, so the
/// paragraph's last item records its advance for `line_width_of` to skip.
/// Only an item ending without trailing spaces or a closing box edge.
fn mark_hanging_last(flat: &[FlatNodeInfo], items: &mut [InlineItem]) {
    let Some(item) = items
        .iter_mut()
        .rev()
        .find(|i| !i.whitespace_only && !i.forced)
    else {
        return;
    };
    let style = flat[item.index].style;
    if !style.hanging_punctuation_last || item.atomic.is_some() || item.box_end != 0.0 {
        return;
    }
    let text = item_text(flat, item);
    let Some(last) = text.chars().next_back() else {
        return;
    };
    if !crate::line_break::is_hangable_last(last) {
        return;
    }
    let (head, tail) = text.split_at(text.len() - last.len_utf8());
    // In LayoutUnits, as the advances it is taken from.
    item.hang_last = snap(if head.is_empty() {
        max_content_width(tail, style)
    } else {
        run_advances(&[head, tail], style)[1]
    });
}

/// LayoutUnit width of a line holding `line` (item indices in order) for
/// fitting and alignment: trailing spaces hang (removed when collapsible,
/// CSS Text 3 4.1.2; hanging when preserved by pre-wrap, 4.1.3), so the
/// trailing whitespace-only items and the last other item's trailing spaces
/// do not count.
fn line_width(items: &[InlineItem], line: &[usize]) -> f32 {
    line_width_of(&line.iter().map(|&k| &items[k]).collect::<Vec<_>>())
}

fn line_width_of(line: &[&InlineItem]) -> f32 {
    let end = line
        .iter()
        .rposition(|item| !item.whitespace_only && !item.forced)
        .map_or(0, |p| p + 1);
    let mut width = 0.0f32;
    let mut slice = 0.0f32;
    let mut run = usize::MAX;
    for (position, &item) in line[..end].iter().enumerate() {
        if item.run != run {
            width += snap(slice);
            slice = 0.0;
            run = item.run;
        }
        slice += if position + 1 == end {
            item.raw_hang - item.hang_last
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
    /// Line layout opportunity: block offset (beside floats only; else
    /// lines stack), inline offset and size.
    top: f32,
    offset: f32,
    available: f32,
    /// The paragraph's last line or one ending in a forced break.
    last: bool,
}

/// NGLineBoxFragmentBuilder alignment offset; `start` / `end` follow the
/// container's direction, and an overflowing line is start-aligned (CSS
/// Text 3 6.1), which in rtl puts it left of the content box.
pub(super) fn align_offset(style: &Style, free: f32) -> f32 {
    align_offset_by(style, style.text_align, free)
}

const TEXT_ALIGN_LAST: &str = "--w3cos-internal-text-align-last";
const TEXT_JUSTIFY: &str = "--w3cos-internal-text-justify";

/// The used `text-align-last` (CSS Text 3 6.3) of the last line and of
/// lines ending in a forced break, or None for `auto` (the line follows
/// `text-align`, which never justifies here) and for `justify` with
/// justification opportunities, which NG does not distribute yet.
/// `text-justify: none` leaves no opportunities (7.4), so a justified
/// line is start-aligned as in Chrome (Blink NGInlineLayoutAlgorithm
/// ApplyJustify falls back to the start offset when nothing expands).
fn text_align_last(style: &Style) -> Option<TextAlign> {
    match marker_value(style, TEXT_ALIGN_LAST)? {
        "left" => Some(TextAlign::Left),
        "right" => Some(TextAlign::Right),
        "center" => Some(TextAlign::Center),
        "start" => Some(TextAlign::Start),
        "end" => Some(TextAlign::End),
        "justify" if marker_value(style, TEXT_JUSTIFY) == Some("none") => Some(TextAlign::Start),
        _ => None,
    }
}

/// `text-align-last` values NG lays out (see `text_align_last`).
fn text_align_last_supported(style: &Style) -> bool {
    match marker_value(style, TEXT_ALIGN_LAST) {
        None | Some("auto") => true,
        Some(_) => text_align_last(style).is_some(),
    }
}

/// `align_offset` for a line box: the last line and lines ending in a
/// forced break use `text-align-last` (CSS Text 3 6.3).
pub(super) fn line_align_offset(style: &Style, free: f32, last: bool) -> f32 {
    let align = last
        .then(|| text_align_last(style))
        .flatten()
        .unwrap_or(style.text_align);
    align_offset_by(style, align, free)
}

fn align_offset_by(style: &Style, align: TextAlign, free: f32) -> f32 {
    let rtl = style.direction == TextDirection::Rtl;
    if free < 0.0 {
        return if rtl { free } else { 0.0 };
    }
    match align {
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
    /// The first line box's baseline from the content-box top.
    pub first_baseline: LastBaseline,
    /// The widest line's content width (hanging spaces excluded): the
    /// min-content / max-content size at available 0 / indefinite.
    pub max_line_width: f32,
    /// Line boxes produced after clamping; an ancestor line-clamp budget
    /// subtracts this from its remaining count.
    pub lines_used: usize,
}

/// Height of a paragraph leaf's text at `width` and whether it is a single
/// line.  The line count is the paint wrapper's (it wraps the leaf), via its
/// height; a single line then uses the line-box model (LayoutUnit line
/// height, strut), several lines keep the wrapper's height.
/// Quirks mode (the DOM marks the root).
pub(super) fn quirks_mode(flat: &[FlatNodeInfo]) -> bool {
    has_marker(flat[0].style, QUIRKS_MARKER)
}

/// A paragraph leaf's line-box height, whether it is one line, that line's
/// baseline, and the rect paint reads (y offset and height): the font
/// content box of a single line, else the lines from the upper leading.
pub(super) fn paragraph_box(
    content: &str,
    available: f32,
    style: &Style,
    container: &Style,
    quirks: bool,
) -> (f32, bool, f32, f32, f32) {
    let (height, single, ascent) = paragraph_height(content, available, style, container, quirks);
    let font_height = inline_font_height(style);
    let (y, rect_height) = if single {
        // As for line items: a fallback-grown line-height: normal line
        // paints from its own line box (see `fragments`).
        if fallback_grows_line(content, style) {
            let (line, baseline) = text_line_metrics(content, style);
            (ascent - baseline, line)
        } else {
            (ascent - inline_font_content_ascent(style), font_height)
        }
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
    (height, single, ascent, y, rect_height)
}

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
        || max_content_width(content, style) <= width + EPSILON;
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

/// Unwrapped max-content inline size. With Skia, `text_intrinsic_size` is
/// that size. Without Skia it wraps at 360px and returns the longest line,
/// so a paragraph wider than 360px but narrower than the available width
/// (the runtime-bench text edit) was treated as one line.
fn max_content_width(content: &str, style: &Style) -> f32 {
    #[cfg(feature = "skia")]
    {
        text_intrinsic_size(content, style).0
    }
    #[cfg(not(feature = "skia"))]
    {
        let padding = style.padding_lengths();
        // A named family, including a test's registered Ahem, is measured
        // with that face. The host face here is often the embedded CJK
        // subset (`Noto Sans SC`, 573/1000 em), which is not Ahem's em cell.
        let registered = crate::layout::registered_style_font(style);
        let host;
        let font = if let Some(face) = registered.as_ref() {
            face.as_ref()
        } else {
            host = crate::layout::layout_font();
            host
        };
        let inner = if style.font_family.is_none() {
            crate::text_layout::measure_text_width_font(content, style.font_size, font)
        } else {
            let (width, _) =
                crate::text_layout::text_intrinsic_size_font(content, style, f32::MAX / 4.0, font);
            width - padding.left - padding.right
        };
        crate::text_layout::inline_layout_advance(inner.max(0.0)) + padding.left + padding.right
    }
}

/// `break-word` is modelled only where its extra emergency breaks are never
/// needed (line building bails otherwise).
fn break_word_supported(style: &Style) -> bool {
    style.word_break == WordBreak::BreakWord
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

/// The style a block text leaf's lines are measured with. The legacy text
/// measurers inset a text component by its own padding; on NG the leaf's
/// padding is a block box edge that block layout already applied (main #399).
pub(super) fn block_text_measure_style(style: &Style) -> std::borrow::Cow<'_, Style> {
    let padding = style.padding_lengths();
    if [padding.top, padding.right, padding.bottom, padding.left]
        .into_iter()
        .all(|v| v == 0.0)
    {
        return std::borrow::Cow::Borrowed(style);
    }
    std::borrow::Cow::Owned(Style {
        padding: Edges::ZERO,
        ..style.clone()
    })
}

/// Content height of a block-level text leaf of content width `width`.
/// Content height and last baseline of a block-level text leaf.
pub(super) fn block_text_layout(content: &str, width: f32, style: &Style) -> (f32, LastBaseline) {
    let style = &*block_text_measure_style(style);
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
    layout_inline_beside(flat, index, space, None)
}

/// The floats of the enclosing BFC an IFC's line boxes must avoid
/// (NGLineLayoutOpportunity): the exclusion space and the BFC offset of
/// the container's content box.
#[derive(Clone, Copy)]
pub(super) struct LineFloats<'a> {
    pub exclusions: &'a super::exclusion::ExclusionSpace,
    pub x: f32,
    pub y: f32,
}

impl LineFloats<'_> {
    /// The line opportunity for a line box at `top` of block size
    /// `height` (content-box coordinates): (inline offset, inline size).
    fn opportunity(&self, top: f32, height: f32, available: f32) -> (f32, f32) {
        let (left, right) = self.exclusions.band(self.y + top, height);
        let start = left.map_or(0.0, |l| (l - self.x).max(0.0));
        let end = right.map_or(available, |r| (r - self.x).min(available));
        (start, (end - start).max(0.0))
    }

    /// The next block offset where the band may widen (a float bottom).
    fn next_offset(&self, top: f32, height: f32) -> Option<f32> {
        self.exclusions
            .next_bottom(self.y + top, height)
            .map(|b| b - self.y)
    }
}

/// The IFC shapes whose lines paint re-wraps around floats from the final
/// rects (resolve_float_text_layouts / forced_break_float_text_layouts):
/// a sole text leaf, or text leaves alternating with forced breaks.
fn paint_rewraps_beside_floats(flat: &[FlatNodeInfo], children: &[usize]) -> bool {
    let text = |k: usize| match flat[k].kind {
        ComponentKind::Text { content } => Some(content.as_str()),
        _ => None,
    };
    match children {
        [only] => text(*only).is_some(),
        _ if children.len() >= 3 && children.len() % 2 == 1 => children
            .iter()
            .enumerate()
            .all(|(n, &k)| text(k).is_some_and(|c| (n % 2 == 1) == (c == "\u{2028}"))),
        _ => false,
    }
}

/// NGInlineLayoutAlgorithm beside floats: each line is broken in the line
/// layout opportunity at its block offset, and moves down past floats when
/// not even its first piece fits.  Line tops assume the container's strut
/// height (checked once the lines are built).
pub(super) fn layout_inline_beside(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    floats: Option<LineFloats>,
) -> Option<InlineResult> {
    let mut result = layout_lines(flat, index, space, floats)?;
    relative_inline_offsets(flat, index, space, &mut result.children);
    Some(result)
}

/// NGInlineLayoutAlgorithm / NGInlineBoxState: a relatively positioned
/// inline box, atomic inline, or text leaf keeps its place in the line and
/// only its fragments (and those of everything inside it) move by its
/// relative offset (CSS2 9.4.3, NGRelativeUtils::ComputeRelativeOffset
/// against the IFC container's content box).  The fragments here are flat,
/// so each one moves by the sum of the offsets of its relatively
/// positioned inline ancestors below the container.
fn relative_inline_offsets(
    flat: &[FlatNodeInfo],
    container: usize,
    space: &ConstraintSpace,
    children: &mut [(f32, f32, BoxFragment)],
) {
    let offset_of = |mut node: usize| {
        let (mut dx, mut dy) = (0.0f32, 0.0f32);
        while node != container {
            let style = flat[node].style;
            if style.position == Position::Relative {
                let (x, y) = super::oof::relative_offset(style, space);
                dx += x;
                dy += y;
            }
            match flat[node].parent {
                Some(p) => node = p,
                None => break,
            }
        }
        (dx, dy)
    };
    for (x, y, fragment) in children.iter_mut() {
        let (dx, dy) = offset_of(fragment.index);
        *x += dx;
        *y += dy;
    }
}

fn layout_lines(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    floats: Option<LineFloats>,
) -> Option<InlineResult> {
    let available = space.available_inline;
    let container = flat[index].style;
    let quirks = quirks_mode(flat);
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
                        return super::bail("inline-empty-box");
                    }
                    closes[leaf - 1].push(b);
                    stack.pop();
                }
            }
        }
        if !pending.is_empty() {
            return super::bail("inline-unclosed");
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
        && line_width_leaf(flat[child].style, container)
        // A clamped paragraph cannot use the paint-wraps-itself path:
        // CSS Overflow 4 block-ellipsis has to be placed on the last
        // kept IFC line (block-ellipsis-001).
        && clamp_lines(container).is_none()
    {
        let style = flat[child].style;
        let ComponentKind::Text { content } = flat[child].kind else {
            return super::bail("line-width-text");
        };
        let (height, single, ascent, y, rect_height) =
            paragraph_box(content, available, style, container, quirks);
        if floats.is_some_and(|f| {
            f.exclusions
                .overlaps(f.x, f.y, f.x + available, f.y + height)
        }) {
            // (Paint wraps the paragraph in one rect.)
            return super::bail("float-paint-paragraph");
        }
        let baseline = if single {
            LastBaseline::At(ascent)
        } else {
            LastBaseline::Unknown
        };
        return Some(InlineResult {
            lines_used: 0,
            height,
            max_line_width: available,
            last_baseline: baseline,
            first_baseline: baseline,
            children: vec![(
                0.0,
                y,
                BoxFragment {
                    index: child,
                    text_range: None,
                    ellipsis: false,
                    vertical: false,
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
        // Collapsed newlines in `normal` / `nowrap` are wrap spaces; they
        // take the IFC wrap path below, not the preserved-newline split.
        && !collapses_whitespace(flat[child].style)
    {
        let style = flat[child].style;
        let width = text_intrinsic_size(content, style).0;
        if style.white_space == WhiteSpace::PreWrap
            && content
                .split('\n')
                .any(|line| text_intrinsic_size(line, style).0 > available + EPSILON)
        {
            return super::bail("line-width-overflow");
        }
        let (strut_ascent, strut_descent) = if quirks {
            (f32::NEG_INFINITY, f32::NEG_INFINITY)
        } else {
            strut(container)
        };
        let (line, baseline) = text_line_metrics(content, style);
        let ascent = strut_ascent.max(baseline);
        let single = ascent + strut_descent.max(line - baseline);
        let line_count = content.split('\n').count();
        let (clamp_n, use_ellipsis) = match clamp_budget(container, space) {
            Some((n, e)) => (Some(n), e),
            None => (None, false),
        };
        // Inherited height-based auto clamp emits per-line pieces even
        // when every line fits, so a later sibling that does not fit can
        // still attach the block-ellipsis to the last kept line (auto-033).
        let force_lines = space
            .line_clamp
            .is_some_and(|c| c.remaining_block.is_some());
        if let Some(n) = clamp_n.filter(|&n| n < line_count || force_lines) {
            let n = n.min(line_count);
            let use_ellipsis = use_ellipsis && n < line_count;
            // `-webkit-line-clamp`: one piece per kept line, the last one
            // followed by the block ellipsis. `line-clamp: auto` with a
            // zero content-box cap keeps no lines. `continue: collapse`
            // without block-ellipsis keeps the line and hides the rest.
            if floats.is_some() || style.white_space != WhiteSpace::Pre {
                return super::bail("clamp-text");
            }
            if n == 0 {
                return Some(InlineResult {
                    lines_used: 0,
                    height: 0.0,
                    max_line_width: 0.0,
                    last_baseline: LastBaseline::NoLines,
                    first_baseline: LastBaseline::NoLines,
                    children: Vec::new(),
                });
            }
            let ellipsis = text_intrinsic_size("\u{2026}", style).0;
            let top = ascent - inline_font_content_ascent(style);
            let mut pieces = Vec::with_capacity(n);
            let mut widest = 0.0f32;
            let mut start = 0;
            for (k, text) in content.split('\n').take(n).enumerate() {
                let last = k + 1 == n;
                let mut width = text_intrinsic_size(text, style).0;
                // CSS Overflow 4 block-ellipsis: place U+2026 after the last
                // soft wrap opportunity on the last kept line that still
                // fits. `white-space: pre` has no wrap opportunities, so a
                // line that cannot fit its text plus the ellipsis keeps
                // only the ellipsis (block-ellipsis-013).
                let mut range_end = start + text.len();
                if last && use_ellipsis {
                    if width + ellipsis > available + EPSILON {
                        if ellipsis > available + EPSILON {
                            return super::bail("clamp-ellipsis");
                        }
                        width = ellipsis;
                        range_end = start;
                    } else {
                        width += ellipsis;
                    }
                }
                widest = widest.max(width);
                pieces.push((
                    line_align_offset(container, available - width, true),
                    top + k as f32 * single,
                    BoxFragment {
                        index: child,
                        text_range: Some((start, range_end)),
                        ellipsis: last && use_ellipsis,
                        vertical: false,
                        width,
                        height: inline_font_height(style),
                        children: Vec::new(),
                    },
                ));
                start += text.len() + 1;
            }
            return Some(InlineResult {
                lines_used: n,
                height: single * n as f32,
                max_line_width: widest,
                last_baseline: LastBaseline::At(ascent + single * (n - 1) as f32),
                first_baseline: LastBaseline::At(ascent),
                children: pieces,
            });
        }
        let lines = line_count as f32;
        if floats.is_some_and(|f| {
            f.exclusions
                .overlaps(f.x, f.y, f.x + available, f.y + single * lines)
        }) {
            return super::bail("float-paint-paragraph");
        }
        return Some(InlineResult {
            lines_used: line_count,
            height: single * lines,
            max_line_width: width,
            last_baseline: LastBaseline::Unknown,
            first_baseline: LastBaseline::At(ascent),
            children: vec![(
                line_align_offset(container, available - width, true),
                ascent - inline_font_content_ascent(style),
                BoxFragment {
                    index: child,
                    text_range: None,
                    ellipsis: false,
                    vertical: false,
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
    // Visual-order fragments are placed in the given order (one line per
    // estimated line: a soft wrap here falls back below).
    let fragments = children.iter().any(|&c| bidi_fragment(flat[c].style));
    let levels = if container.direction == TextDirection::Rtl && !fragments {
        leaf_bidi_levels(flat, &children, container).ok()?
    } else {
        vec![LeafLevel::default(); children.len()]
    };
    let reorder = needs_reorder(&levels);
    if reorder && has_boxes {
        return super::bail("reorder-boxes");
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
            let result =
                super::block::layout_block(flat, child, space, true).why("atomic-layout")?;
            if !result.oof.is_empty() {
                // (Out-of-flow descendants carried through a line box.)
                return super::bail("inline-oof");
            }
            // Authored margins (no CSS2 10.3.3 over-constraint rule; auto is
            // gated).
            let side = |m| {
                crate::layout::resolve_spacing_for_layout(
                    m,
                    if space.percent_unresolved {
                        0.0
                    } else {
                        available
                    },
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
            // CSS Align 3 §9.1 / CSS Inline 3 / csswg #4675 (Blink
            // LayoutNG atomic inlines, BSD-style Chromium license): a
            // box whose writing mode is orthogonal to the IFC has no
            // typographic baseline in the line's block axis. Inline-level
            // boxes synthesize the alphabetic baseline from the under
            // margin edge; in a horizontal-tb line that is the physical
            // bottom of the margin box.
            let orthogonal = container.writing_mode == w3cos_std::style::WritingMode::HorizontalTb
                && style.writing_mode != w3cos_std::style::WritingMode::HorizontalTb;
            let ascent = match result.last_baseline {
                // (A button keeps its content baseline, as in block.rs.)
                _ if super::block::baseline_is_margin_edge(style)
                    && !matches!(flat[child].kind, ComponentKind::Button { .. }) =>
                {
                    height
                }
                _ if orthogonal => height,
                LastBaseline::At(b) => {
                    // A transposed vertical IFC maps block-start to the
                    // typographic under (`sideways`). Use padding-top +
                    // descent, not the HTB alphabetic-from-over.
                    let b = if super::orthogonal::is_transposed(style) {
                        super::orthogonal::sideways_typographic_ascent(
                            style,
                            result.fragment.height,
                            b,
                        )
                    } else {
                        b
                    };
                    margin[0] + b
                }
                LastBaseline::NoLines => height,
                LastBaseline::Unknown => return super::bail("atomic-baseline"),
            };
            let box_start = start_edges(&opens[k]);
            let box_end = end_edges(&closes[k]);
            let raw = margin[3] + result.fragment.width + margin[1] + box_start + box_end;
            let parent = parents[k].map_or(container, |b| flat[b].style);
            let shift = vertical_align_shift(style, ascent, height - ascent, parent)
                .why("vertical-align")?;
            items.push(InlineItem {
                index: child,
                pos: k,
                range: None,
                raw,
                raw_hang: raw,
                hang_last: 0.0,
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
            && !fragments
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
            let k = start + offset;
            let box_start = start_edges(&opens[k]);
            let box_end = end_edges(&closes[k]);
            let text_raw = if content == FORCED_BREAK {
                0.0
            } else if let Some(advance) =
                tab_item_advance(content, measure, container, &items, box_start + edges.start)
            {
                advance
            } else if bare.is_some() {
                text_intrinsic_size(content, measure).0
            } else {
                advances[offset]
            };
            let raw = text_raw + inline_edges + box_start + box_end;
            let (height, baseline) = text_line_metrics(content, style);
            let parent = parents[k].map_or(container, |b| flat[b].style);
            let shift = vertical_align_shift(style, baseline, height - baseline, parent)
                .why("vertical-align")?;
            items.push(InlineItem {
                index: child,
                pos: k,
                range: None,
                raw,
                // Preserved spaces (pre) do not hang (CSS Text 3 4.1.3).
                raw_hang: if container.white_space == WhiteSpace::Pre {
                    raw
                } else {
                    hang_advance(content, text_raw, measure) + inline_edges + box_start + box_end
                },
                hang_last: 0.0,
                run,
                starts_space: if collapses_whitespace(style) {
                    content.starts_with(is_collapsible_space)
                } else {
                    content.starts_with(' ')
                },
                ends_space: if collapses_whitespace(style) {
                    content.ends_with(is_collapsible_space)
                } else {
                    content.ends_with(' ')
                },
                whitespace_only: if collapses_whitespace(style) {
                    content.trim_end_matches(is_collapsible_space).is_empty()
                } else {
                    content.trim_end_matches(' ').is_empty()
                },
                forced: content == FORCED_BREAK,
                edges,
                box_start,
                box_end,
                aligned: aligned[k],
                // Preserved leading spaces (pre-wrap) end in an opportunity
                // inside the item too; collapsible ones vanish at a line start.
                internal_break: internal_opportunity(content, style)
                    || (hang_preserved
                        && content.starts_with(' ')
                        && !content.trim_start_matches(' ').is_empty()),
                bidi: levels[k],
                shift,
                atomic: None,
                ascent: baseline,
                descent: height - baseline,
                content_ascent: inline_font_content_ascent(style),
                font_height: inline_font_height(style),
            });
            if content != FORCED_BREAK && content.contains('\u{2028}') {
                let whole = items.pop().expect("just pushed");
                split_forced_breaks(content, whole, style, hang_preserved, &mut items)?;
            }
        }
        run += 1;
        start = end;
    }

    mark_hanging_last(flat, &mut items);

    // NGLineBreaker.
    let can_wrap = matches!(
        container.white_space,
        WhiteSpace::Normal | WhiteSpace::PreWrap | WhiteSpace::PreLine
    );
    let mut lines: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    // NGLineLayoutOpportunity per line: (top, inline offset, inline size).
    let assumed_line = if floats.is_some() {
        if quirks {
            return super::bail("float-quirks");
        }
        let (a, d) = strut(container);
        a + d
    } else {
        0.0
    };
    let mut opportunities: Vec<(f32, f32, f32)> = Vec::new();
    let mut line_top = 0.0f32;
    let mut opportunity = match floats {
        Some(f) => f.opportunity(line_top, assumed_line, available),
        None => (0.0, available),
    };
    let mut i = 0;
    while i < items.len() {
        let item = &items[i];
        if !preserve && current.is_empty() && item.whitespace_only {
            // A collapsible space at the start of a line is removed.
            i += 1;
            continue;
        }
        if lines.len() > opportunities.len() {
            // A line was finished: the next one starts below it.
            opportunities.push((line_top, opportunity.0, opportunity.1));
            line_top += assumed_line;
            if let Some(f) = floats {
                opportunity = f.opportunity(line_top, assumed_line, available);
            }
        }
        if item.forced {
            // NGLineBreaker::HandleForcedLineBreak.
            current.push(i);
            lines.push(std::mem::take(&mut current));
            i += 1;
            continue;
        }
        let available = opportunity.1;
        current.push(i);
        let fits = line_width(&items, &current) <= available + EPSILON;
        current.pop();
        if !fits && can_wrap && item.internal_break {
            // NGLineBreaker breaks inside the item at its latest opportunity
            // that fits; on an otherwise empty line, at its first one.
            // (Preserved spaces stay on legacy.)
            if preserve {
                return super::bail("internal-break-preserve");
            }
            if item.atomic.is_some() || !zero_edges(flat[item.index].style) {
                return super::bail("internal-break-edges");
            }
            let split = split_text_item(flat, item, current.is_empty(), |a| {
                let mut probe: Vec<&InlineItem> = current.iter().map(|&k| &items[k]).collect();
                probe.push(a);
                line_width_of(&probe) <= available + EPSILON
            });
            if let Some((a, b)) = split {
                items[i] = a;
                items.insert(i + 1, b);
                current.push(i);
                lines.push(std::mem::take(&mut current));
                i += 1;
                continue;
            }
            if current.is_empty() && has_split_opportunity(flat, item) {
                return super::bail("internal-break");
            }
            // (No UAX #14 opportunity inside the item for its BreakMode,
            // e.g. CJK under keep-all: it is unbreakable and overflows.)
            // No prefix fits after the line's content: NGLineBreaker
            // rewinds to the last opportunity before this item (a boundary
            // or one inside an earlier item) and retries it on the next line
            // (the soft-wrap path below). A line that overflows with an
            // internal opportunity still falls back after line building
            // ("internal-break-earlier").
        }
        if !fits && current.is_empty() && opportunity.1 < space.available_inline - EPSILON {
            // Not even the first piece fits beside the floats: the line
            // moves down to where the band widens.
            let f = floats.why("line-opportunity")?;
            line_top = f
                .next_offset(line_top, assumed_line)
                .why("line-opportunity")?;
            opportunity = f.opportunity(line_top, assumed_line, space.available_inline);
            continue;
        }
        if !fits
            && current.is_empty()
            && can_wrap
            && flat[item.index].style.word_break == WordBreak::BreakWord
            && !item.whitespace_only
            // pre / pre-wrap never take overflow-wrap emergency breaks
            // (main #413; white-space-intrinsic-size-003 / 016).
            && !matches!(
                flat[item.index].style.white_space,
                WhiteSpace::Pre | WhiteSpace::PreWrap
            )
        {
            // An unbreakable word wider than the line takes an emergency
            // break (overflow-wrap: break-word), which S2 does not model.
            return super::bail("break-word-overflow");
        }
        if fits || current.is_empty() || !can_wrap {
            current.push(i);
            i += 1;
            continue;
        }
        if item.starts_space && !item.whitespace_only {
            // The opportunity lies after this item's leading space.
            return super::bail("leading-space");
        }
        // NGLineBreaker::HandleOverflow walks the line's items backwards and
        // rewinds to the latest opportunity: the boundary before an item
        // (after an item ending in a space, at the end of a space run;
        // preserved spaces in pre-wrap do not break between each other), or
        // one inside the item before that boundary (BreakText with the width
        // left to it).
        let boundary = |j: usize| {
            let next = current.get(j).copied().unwrap_or(i);
            let prev = current[j - 1];
            // Atomic inlines break before and after (UAX #14 CB, LB20).
            ((items[prev].ends_space || items[prev].atomic.is_some() || items[next].atomic.is_some())
                && !items[next].starts_space)
                // Between two text items: the UAX #14 opportunity of the
                // IFC's text content at their boundary (NGLineBreaker reads
                // the break iterator over the whole paragraph text).
                || (!items[prev].ends_space
                    && !items[next].starts_space
                    && text_boundary_opportunity(flat, &items[prev], &items[next]))
        };
        let mut break_at = None;
        let mut inside = None;
        for j in (1..=current.len()).rev() {
            if boundary(j) {
                break_at = Some(j);
                break;
            }
            let k = current[j - 1];
            if !items[k].internal_break {
                continue;
            }
            if preserve {
                return super::bail("internal-break-preserve");
            }
            if items[k].atomic.is_some() || !zero_edges(flat[items[k].index].style) {
                return super::bail("internal-break-edges");
            }
            let head = &current[..j - 1];
            let split = split_text_item(flat, &items[k], false, |a| {
                let mut probe: Vec<&InlineItem> = head.iter().map(|&h| &items[h]).collect();
                probe.push(a);
                line_width_of(&probe) <= available + EPSILON
            });
            match split {
                Some(pieces) => {
                    inside = Some((j - 1, pieces));
                    break;
                }
                None => return super::bail("internal-break-earlier"),
            }
        }
        if let Some((p, (a, b))) = inside {
            // The line ends inside current[p]; its rest and the items after
            // it start the next line, which retries the overflowing item.
            let k = current[p];
            items[k] = a;
            items.insert(k + 1, b);
            let rest: Vec<usize> = std::iter::once(k + 1)
                .chain(current[p + 1..].iter().map(|&r| r + 1))
                .collect();
            current.truncate(p + 1);
            lines.push(std::mem::take(&mut current));
            if rest
                .iter()
                .any(|&r| items[r].starts_space && !items[r].whitespace_only)
            {
                return super::bail("leading-space");
            }
            current = rest;
            i += 1;
            continue;
        }
        match break_at {
            Some(j) if j == current.len() => lines.push(std::mem::take(&mut current)),
            Some(j) => {
                let rest = current.split_off(j);
                lines.push(std::mem::take(&mut current));
                if rest
                    .iter()
                    .any(|&k| items[k].starts_space && !items[k].whitespace_only)
                {
                    return super::bail("leading-space");
                }
                current = rest;
            }
            None => {
                // No opportunity on this line: the item overflows.
                if flat[item.index].style.word_break == WordBreak::BreakWord
                    && !item.whitespace_only
                {
                    return super::bail("break-word-overflow");
                }
                current.push(i);
                i += 1;
            }
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.len() > opportunities.len() {
        opportunities.push((line_top, opportunity.0, opportunity.1));
    }
    debug_assert_eq!(lines.len(), opportunities.len());
    // Tab advances were measured from the first line's start (see
    // `tab_item_advance`): a wrapped (pre-wrap) or float-shifted tab item
    // needs its own line position.
    let has_tab = |i: &usize| {
        matches!(&flat[items[*i].index].kind, ComponentKind::Text { content }
            if content.contains('\t'))
    };
    if lines.iter().skip(1).flatten().any(has_tab)
        || (floats.is_some() && lines.iter().flatten().any(has_tab))
    {
        return super::bail("text:tab-wrapped");
    }
    // Paint re-wraps a sole leaf beside floats from its rect, without the
    // line-relative alignment.
    if floats.is_some() && children.len() == 1 && aligned.iter().any(Option::is_some) {
        return super::bail("vertical-align-keyword-float");
    }
    if floats.is_some()
        && paint_rewraps_beside_floats(flat, &children)
        && opportunities
            .iter()
            .enumerate()
            .any(|(n, &(top, offset, size))| {
                (top - n as f32 * assumed_line).abs() > EPSILON
                    || offset > EPSILON
                    || size < available - EPSILON
            })
    {
        // Paint re-wraps these shapes around floats itself
        // (layout::resolve_float_text_layouts): their lines must stay on
        // the legacy geometry.
        return super::bail("float-paint-rewrap");
    }
    if fragments
        && lines[..lines.len().saturating_sub(1)]
            .iter()
            .any(|line| !line.last().is_some_and(|&k| items[k].forced))
    {
        // A soft wrap the cascade's line estimate may not have made.
        return super::bail("soft-wrap-estimate");
    }

    // Line boxes (NGLineBoxFragmentBuilder): run slices start on LayoutUnit
    // offsets; pieces inside a slice keep their raw shaping offsets and the
    // slice's last piece absorbs the snap remainder.
    // A transposed typographic IFC of only inline-blocks uses the sideways
    // strut (under faces block-start). Text-only orthogonal roots keep the
    // HTB strut so their typographic text metrics stay consistent.
    let atomic_only = items
        .iter()
        .all(|item| item.atomic.is_some() || item.whitespace_only || item.forced);
    let (strut_ascent, strut_descent) = if quirks {
        (f32::NEG_INFINITY, f32::NEG_INFINITY)
    } else if super::orthogonal::is_transposed(container) && atomic_only {
        sideways_strut(container)
    } else {
        strut(container)
    };
    let line_count = lines.len();
    let mut max_line_width = 0.0f32;
    let mut built = Vec::with_capacity(lines.len());
    for (line_index, mut line) in lines.into_iter().enumerate() {
        let (line_top, line_offset, available) = opportunities[line_index];
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
            if floats.is_some() {
                // (The assumed line tops counted it.)
                return super::bail("float-empty-line");
            }
            continue;
        }
        // The item whose trailing spaces hang (the logical line end).
        let hang_item = content_len.checked_sub(1).map(|p| line[p]);
        if line[..content_len]
            .iter()
            .any(|&k| items[k].bidi.tail && (Some(k) != hang_item || preserve))
        {
            // Spaces at another level inside the line would sit apart from
            // their item after reordering.
            return super::bail("bidi-space");
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
        // hanging-punctuation: last (`mark_hanging_last`) hangs past the end
        // edge: not measured for fit or alignment (CSS Text 3 8.2).
        let hung = hang_item.map_or(0.0, |k| items[k].hang_last);
        if can_wrap
            && content_width - hung > available + EPSILON
            && line.iter().any(|&k| items[k].internal_break)
        {
            return super::bail("internal-break-earlier");
        }
        // Max-content (an indefinite available size): preserved spaces
        // before a forced break or at the end of the block hang only
        // conditionally, which does not keep them out of the intrinsic
        // size (CSS Text 3 4.1.3; white-space-intrinsic-size-004).
        let counts_hanging = hang_preserved
            && available >= super::block::INDEFINITE_INLINE * 0.5
            && (line_index + 1 == line_count || forced);
        max_line_width = max_line_width.max(if counts_hanging {
            content_width.max(slice_start)
        } else {
            content_width
        });
        // CSS Text 3 4.1.3: before a forced break or the end of the block,
        // preserved spaces hang only conditionally: the part that fits
        // still counts for alignment.
        let align_width = if hang_preserved && line_index + 1 == line_count {
            content_width.max(slice_start.min(available))
        } else {
            content_width - hung
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
        if floats.is_some() && (ascent + descent - assumed_line).abs() > EPSILON {
            // A line taller or shorter than the strut: the opportunities
            // were computed for the wrong block offsets.
            return super::bail("float-line-height");
        }
        built.push(Line {
            items: placed,
            width: align_width,
            ascent,
            descent,
            subtrees,
            top: line_top,
            offset: line_offset,
            available,
            last: forced || line_index + 1 == line_count,
        });
    }

    // text-box-trim (CSS Inline 3 / Blink node start/end): pull the first
    // line's block-start to the text-over edge and the last line's
    // block-end to the text-under edge. Per-column fragmentainer trim is
    // a later slice (multicol-001).
    let (trim_start, trim_end) = text_box_trim_ends(container);
    // Float opportunities were computed for untrimmed line heights.
    if (trim_start || trim_end) && !built.is_empty() && floats.is_none() {
        let content_edges = |line: &Line| {
            let mut content_ascent = 0.0f32;
            let mut content_descent = 0.0f32;
            let mut saw = false;
            for &(k, ..) in &line.items {
                let item = &items[k];
                if item.forced || item.aligned.is_some() {
                    continue;
                }
                saw = true;
                content_ascent = content_ascent.max((item.content_ascent - item.shift).max(0.0));
                let font_descent = (item.font_height - item.content_ascent).max(0.0);
                content_descent = content_descent.max((font_descent + item.shift).max(0.0));
            }
            saw.then_some((content_ascent, content_descent))
        };
        if trim_start {
            if let Some((ca, _)) = content_edges(&built[0]) {
                built[0].ascent = ca.max(0.0).min(built[0].ascent);
            }
        }
        if trim_end {
            let last = built.len() - 1;
            if let Some((_, cd)) = content_edges(&built[last]) {
                built[last].descent = cd.max(0.0).min(built[last].descent);
            }
        }
    }

    // `-webkit-line-clamp`: keep the first N lines; the last kept line's final
    // text item takes the ellipsis.  Items on dropped lines keep empty
    // placeholder rects (the engine lays out every node).
    let mut dropped: Vec<usize> = Vec::new();
    let mut ellipsis_item = None;
    let mut ellipsis_width = 0.0f32;
    let mut ellipsis_range = None;
    let (clamp_n, use_ellipsis) = match clamp_budget(container, space) {
        Some((n, e)) => (Some(n), e),
        None => (None, false),
    };
    if let Some(n) = clamp_n.filter(|&n| n < built.len()) {
        if floats.is_some() || has_boxes || reorder || preserve || !can_wrap {
            return super::bail("clamp-ifc");
        }
        if n == 0 {
            dropped = built
                .iter()
                .flat_map(|l| l.items.iter().map(|&(k, ..)| k))
                .collect();
            built.clear();
        } else if !use_ellipsis {
            dropped = built[n..]
                .iter()
                .flat_map(|l| l.items.iter().map(|&(k, ..)| k))
                .collect();
            built.truncate(n);
        } else {
            dropped = built[n..]
                .iter()
                .flat_map(|l| l.items.iter().map(|&(k, ..)| k))
                .collect();
            built.truncate(n);
            let last = &mut built[n - 1];
            let k = last
                .items
                .iter()
                .rev()
                .map(|&(k, ..)| k)
                .find(|&k| !items[k].forced)
                .why("clamp-ifc")?;
            let item = &items[k];
            let ComponentKind::Text { content } = &flat[item.index].kind else {
                return super::bail("clamp-ifc");
            };
            if !clamp_ellipsis_piece_ok(item, content, flat[item.index].style) {
                return super::bail("clamp-ifc");
            }
            ellipsis_width = text_intrinsic_size("…", flat[item.index].style).0;
            match trim_line_for_block_ellipsis(&items, flat, last, &mut dropped, ellipsis_width) {
                Some((k, range)) => {
                    ellipsis_item = Some(k);
                    ellipsis_range = range;
                }
                None => return super::bail("clamp-ellipsis"),
            }
        }
    }
    let mut top = 0.0f32;
    let mut first_line_baseline = None;
    let mut last_line_baseline = None;
    let mut out = Vec::with_capacity(items.len() + entries.len() - children.len());
    // (line, inline offset, width, baseline) of each placed item.
    // Keyed by leaf position: the first piece of a split leaf opens its
    // boxes, the last one closes them.
    let mut placed: Vec<Option<(usize, f32, f32, f32)>> = vec![None; opens.len()];
    let mut placed_last: Vec<Option<(usize, f32, f32, f32)>> = vec![None; opens.len()];
    // Every placed piece as (leaf position, line, offset, width, baseline):
    // a box split across lines spans the pieces of its leaves per line.
    let mut placed_all: Vec<(usize, usize, f32, f32, f32)> = Vec::new();
    let lines_used = built.len();
    for (line_index, line) in built.into_iter().enumerate() {
        if floats.is_some() {
            top = line.top;
        }
        let available = line.available;
        let offset = line.offset + line_align_offset(container, available - line.width, line.last);
        let baseline = top + line.ascent;
        first_line_baseline.get_or_insert(baseline);
        last_line_baseline = Some(baseline);
        for (k, x, width) in line.items {
            let item = &items[k];
            let baseline = match item.aligned {
                None => baseline,
                Some((root, _)) => {
                    let &(_, top_aligned, a, d) = line
                        .subtrees
                        .iter()
                        .find(|s| s.0 == root)
                        .why("line-subtree")?;
                    if top_aligned {
                        top + a
                    } else {
                        top + line.ascent + line.descent - d
                    }
                }
            };
            if item.forced && item.range.is_some() {
                // A break folded into a text leaf: the leaf's rect is the
                // union of its text pieces (no <br> node of its own).
                continue;
            }
            if item.forced {
                // The engine's <br> marker: a zero-height full-width rect at
                // the line end, painting nothing.
                out.push((
                    line.offset,
                    top + line.ascent + line.descent,
                    BoxFragment {
                        index: item.index,
                        text_range: None,
                        ellipsis: false,
                        vertical: false,
                        width: available,
                        height: 0.0,
                        children: Vec::new(),
                    },
                ));
                continue;
            }
            let at = Some((line_index, offset + x, width, baseline));
            placed[item.pos] = placed[item.pos].or(at);
            placed_last[item.pos] = at;
            placed_all.push((item.pos, line_index, offset + x, width, baseline));
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
            let clamped = ellipsis_item == Some(k);
            // Paint lays a text fragment out as a line box of its used
            // (fallback-grown) metrics from the rect top: when a fallback
            // face grows a line-height: normal line, the rect is that line
            // box (NGLineHeightMetrics of the item) rather than the primary
            // font box, so the glyphs sit on the line's baseline.
            let (content_ascent, font_height) = match flat[item.index].kind {
                ComponentKind::Text { content }
                    if item.ascent > item.content_ascent + EPSILON
                        && fallback_grows_line(
                            item.range.map_or(content, |(a, b)| &content[a..b]),
                            flat[item.index].style,
                        ) =>
                {
                    (item.ascent, item.ascent + item.descent)
                }
                _ => (item.content_ascent, item.font_height),
            };
            out.push((
                offset + x + item.box_start + e.margin_start,
                baseline - content_ascent - e.top,
                BoxFragment {
                    index: item.index,
                    text_range: match (ellipsis_range, item.range, clamped, &flat[item.index].kind)
                    {
                        (Some(range), _, true, _) => Some(range),
                        (None, None, true, ComponentKind::Text { content }) => {
                            Some((0, content.len()))
                        }
                        (_, range, ..) => range,
                    },
                    ellipsis: clamped,
                    vertical: false,
                    width: if clamped { ellipsis_width } else { 0.0 } + width
                        - item.box_start
                        - item.box_end
                        - e.margin_start
                        - e.margin_end,
                    height: font_height + e.top + e.bottom,
                    children: Vec::new(),
                },
            ));
        }
        top += line.ascent + line.descent;
    }
    for k in dropped {
        let index = items[k].index;
        if out.iter().all(|(_, _, f)| f.index != index) {
            out.push((
                0.0,
                top,
                BoxFragment {
                    index,
                    text_range: (!items[k].forced).then_some((0, 0)),
                    ellipsis: false,
                    vertical: false,
                    width: 0.0,
                    height: 0.0,
                    children: Vec::new(),
                },
            ));
        }
    }
    // Inline box fragments (NGInlineBoxState::CreateBoxFragment): the border
    // box from the first to the last item's edges, block-axis padding and
    // border around the box's font content area.  A box split across lines
    // has several fragments (one per line); the engine keeps one rect per
    // node, so such a box gets their union (LayoutInline's bounding box)
    // when it paints nothing of its own; otherwise legacy lays it out.
    if has_boxes {
        let mut starts: std::collections::HashMap<usize, (usize, usize, f32, f32)> =
            std::collections::HashMap::new();
        for (k, boxes) in opens.iter().enumerate() {
            // A leaf that never contributed a piece (a collapsed space,
            // CSS 2.1 16.6.1) does not place the inline boxes that would
            // open on it; those boxes have no fragment.
            if placed[k].is_none() {
                continue;
            }
            for (pos, &b) in boxes.iter().enumerate() {
                let (line, x, _, baseline) = placed[k].why("inline-box-placement")?;
                let start =
                    x + start_edges(&boxes[..pos]) + InlineEdges::of(flat[b].style).margin_start;
                starts.insert(b, (k, line, start, baseline));
            }
        }
        for (k, boxes) in closes.iter().enumerate() {
            if placed_last[k].is_none() {
                continue;
            }
            for (pos, &b) in boxes.iter().enumerate() {
                let (line, x, width, _) = placed_last[k].why("inline-box-placement")?;
                let (first, start_line, start, baseline) =
                    starts.get(&b).copied().why("inline-box-placement")?;
                let style = flat[b].style;
                if start_line != line {
                    if !paints_nothing(style) {
                        return super::bail("split-box-line");
                    }
                    let (mut x0, mut x1) = (f32::INFINITY, f32::NEG_INFINITY);
                    let (mut top_baseline, mut bottom_baseline) = (baseline, baseline);
                    for &(pos, _, px, pw, pb) in &placed_all {
                        if (first..=k).contains(&pos) {
                            x0 = x0.min(px);
                            x1 = x1.max(px + pw);
                            top_baseline = top_baseline.min(pb);
                            bottom_baseline = bottom_baseline.max(pb);
                        }
                    }
                    if start > x0 + EPSILON {
                        // offsetLeft is the first fragment's start, not the
                        // union's; one rect cannot carry both.
                        return super::bail("split-box-line");
                    }
                    let ascent = inline_font_content_ascent(style);
                    let y0 = top_baseline - ascent;
                    out.push((
                        x0,
                        y0,
                        BoxFragment {
                            index: b,
                            text_range: None,
                            ellipsis: false,
                            vertical: false,
                            width: x1 - x0,
                            height: bottom_baseline - ascent + inline_font_height(style) - y0,
                            children: Vec::new(),
                        },
                    ));
                    continue;
                }
                let e = InlineEdges::of(style);
                let end = x + width - end_edges(&boxes[pos + 1..]) - e.margin_end;
                out.push((
                    start,
                    baseline - inline_font_content_ascent(style) - e.top,
                    BoxFragment {
                        index: b,
                        text_range: None,
                        ellipsis: false,
                        vertical: false,
                        width: end - start,
                        height: inline_font_height(style) + e.top + e.bottom,
                        children: Vec::new(),
                    },
                ));
            }
        }
    }
    Some(InlineResult {
        lines_used,
        height: top,
        children: out,
        last_baseline: last_line_baseline.map_or(LastBaseline::NoLines, LastBaseline::At),
        first_baseline: first_line_baseline.map_or(LastBaseline::NoLines, LastBaseline::At),
        max_line_width,
    })
}

/// Min/max-content sizes of a paragraph (block-level or sole line-width)
/// text leaf, which paint wraps itself: max-content is the unwrapped
/// advance (widest forced-break segment); min-content the widest word where
/// spaces are the only opportunities (Latin-1 without other soft-wrap
/// characters), the widest segment between crate::line_break opportunities
/// otherwise, the whole text for nowrap / pre.
pub(super) fn text_min_max(content: &str, style: &Style) -> MinMax {
    let width = |t: &str| snap(text_intrinsic_size(t, style).0);
    let collapsed = as_collapsed_spaces(content, style);
    let measure = collapsed.as_ref();
    let max = if collapses_whitespace(style) {
        width(measure)
    } else {
        content
            .split(['\n', '\u{2028}', '\u{2029}'])
            .map(width)
            .fold(0.0f32, f32::max)
    };
    // Preserved newlines force a max-content break; collapsed ones do not.
    let forced = has_forced_break(content, style)
        || (content.contains('\n')
            && !collapses_whitespace(style)
            && !matches!(style.white_space, WhiteSpace::Pre));
    let min = if matches!(style.white_space, WhiteSpace::NoWrap | WhiteSpace::Pre) {
        Some(max)
    } else if !forced
        && latin1(measure)
        && !measure.chars().any(|c| {
            c != ' ' && is_soft_wrap_char(c) && !(c == '\u{ad}' && suppresses_hyphenation(style))
        })
    {
        // Pre-wrap: trailing preserved spaces hang and do not affect
        // min-content (CSS Text 3; white-space-intrinsic-size-003).
        let body = if style.white_space == WhiteSpace::PreWrap {
            measure.trim_end_matches(' ')
        } else {
            measure
        };
        Some(body.split(' ').map(width).fold(0.0f32, f32::max))
    } else if !forced {
        // NGLineBreaker at available 0: every UAX #14 opportunity of the
        // run's BreakMode is taken; trailing hanging spaces (pre-wrap)
        // do not contribute to min-content.
        let body = if style.white_space == WhiteSpace::PreWrap {
            measure.trim_end_matches(' ')
        } else {
            measure
        };
        let mut cuts = ifc_break_opportunities(body, style);
        cuts.retain(|&o| o > 0 && o < body.len());
        cuts.push(body.len());
        let mut start = 0;
        let mut widest = 0.0f32;
        for cut in cuts {
            let segment = body[start..cut].trim_end_matches(' ');
            if !segment.is_empty() {
                widest = widest.max(width(segment));
            }
            start = cut;
        }
        Some(widest)
    } else {
        None
    };
    MinMax { min, max }
}

/// NGInlineNode::ComputeMinMaxSizes: the line breaker's widest line at
/// available 0 (every opportunity taken) and at an indefinite size (only
/// forced breaks).  A paragraph leaf is measured as text. `space` carries
/// the container's child percentage basis (block::intrinsic_sizes_at), so
/// an atomic inline's percentage height resolves as in layout.
pub(super) fn inline_min_max(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
) -> Option<MinMax> {
    let entries = collect_inlines(flat, index);
    if let [InlineEntry::Leaf(child)] = entries[..]
        && !is_atomic_inline(flat, child)
        && let ComponentKind::Text { content } = flat[child].kind
        && (has_marker(flat[child].style, LINE_WIDTH_MARKER)
            || content.contains('\n')
            || flat[child].style.white_space == WhiteSpace::PreWrap)
    {
        return Some(text_min_max(content, flat[child].style));
    }
    let at = |available: f32| {
        layout_inline(
            flat,
            index,
            &ConstraintSpace {
                available_inline: available,
                percent_unresolved: true,
                cyclic_percent_auto: false,
                line_clamp: None,
                ..*space
            },
        )
        .map(|result| result.max_line_width)
    };
    let max = at(super::block::INDEFINITE_INLINE).why("inline-intrinsic")?;
    Some(MinMax {
        min: at(0.0).map(|min| min.min(max)),
        max,
    })
}

#[cfg(test)]
mod tab_tests {
    use super::*;

    #[test]
    fn sole_break_spaces_leaf_and_break_word_stay_on_ng() {
        let break_spaces = Style {
            white_space: WhiteSpace::PreWrap,
            break_spaces: true,
            ..Style::default()
        };
        assert_eq!(text_style_reason("a b", &break_spaces), None);
        let break_word = Style {
            word_break: WordBreak::BreakWord,
            ..Style::default()
        };
        assert_eq!(text_style_reason("abc", &break_word), None);
        let pre_wrap_break_word = Style {
            white_space: WhiteSpace::PreWrap,
            word_break: WordBreak::BreakWord,
            ..Style::default()
        };
        assert_eq!(text_style_reason("abc", &pre_wrap_break_word), None);
        let break_spaces_break_word = Style {
            break_spaces: true,
            ..pre_wrap_break_word
        };
        assert_eq!(text_style_reason("abc", &break_spaces_break_word), None);
    }

    #[test]
    fn preserved_tabs_stay_on_ng_only_on_the_first_line() {
        let pre = Style {
            white_space: WhiteSpace::Pre,
            ..Style::default()
        };
        assert!(preserved_tabs_ok("a\tb", &pre, &pre));
        assert_eq!(text_content_reason("a\tb", false, true), None);
        assert_eq!(text_content_reason("a\tb", false, false), Some("text:tab"));
        assert!(supported_script_in("\tP", false));
        // pre-wrap tabs: paint resolves line-box stops only for `pre`; a
        // preserved newline starts a line whose stops NG does not resolve.
        let pre_wrap = Style {
            white_space: WhiteSpace::PreWrap,
            ..Style::default()
        };
        assert!(!preserved_tabs_ok("a\tb", &pre_wrap, &pre_wrap));
        assert_eq!(
            block_text_content_reason("a\tb", &pre_wrap),
            Some("text:tab")
        );
        assert_eq!(block_text_content_reason("a\tb", &pre), None);
        assert!(!preserved_tabs_ok("a\n\tb", &pre, &pre));
        // Paint offsets the stops by the line's alignment shift.
        let centered = Style {
            text_align: TextAlign::Center,
            ..pre.clone()
        };
        assert!(!preserved_tabs_ok("\t", &pre, &centered));
        // The block's space advance sets the interval.
        let spaced = Style {
            letter_spacing: 2.0,
            ..pre.clone()
        };
        assert!(!preserved_tabs_ok("\t", &pre, &spaced));
        let indented = Style {
            text_indent: Dimension::Px(4.0),
            ..pre.clone()
        };
        assert!(!preserved_tabs_ok("\t", &pre, &indented));
    }
}

/// [`text_min_max`] of a text leaf's content box: the legacy measure behind
/// it includes the leaf's own horizontal padding, which the caller adds back.
pub(super) fn text_content_min_max(content: &str, style: &Style) -> MinMax {
    let padding = style.padding_lengths();
    let edges = padding.left + padding.right;
    text_min_max(content, style).map(|width| (width - edges).max(0.0))
}
