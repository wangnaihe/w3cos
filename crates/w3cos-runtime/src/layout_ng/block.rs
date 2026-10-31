//! Block formatting context: stage S1 block flow, S3a positioning, S3b
//! floats and clearance in BFC coordinates (NGBfcOffset).
//!
//! Ported in structure from Blink's NGBlockLayoutAlgorithm
//! (third_party/blink/renderer/core/layout/block_layout_algorithm.cc,
//! Copyright The Chromium Authors, BSD-style license): children are laid out
//! in order with a margin strut that is resolved when a non-adjoining edge
//! (border, padding, in-flow content, BFC root) is reached.  Width follows
//! CSS2 10.3.3, height 10.6.3, min/max 10.4/10.7, collapsing 8.3.1.

use super::Why;
use super::exclusion::ExclusionSpace;
use super::oof::{self, ContainingBlock, OofCandidate};
use super::{BoxFragment, ConstraintSpace, LastBaseline, LayoutResult, MarginStrut, inline};
use crate::layout::{FlatNodeInfo, resolve_spacing_for_layout};
use w3cos_std::ComponentKind;
use w3cos_std::style::{
    BoxSizing, Clear, Dimension, Display, Float, Overflow, Position, Spacing, Style, TextDirection,
    UnicodeBidi,
};

fn supported_dimension(_d: Dimension) -> bool {
    // Every Dimension unit resolves like legacy `to_taffy_dim`.
    true
}

fn supported_spacing(s: Spacing, allow_auto: bool) -> bool {
    match s {
        Spacing::Px(_)
        | Spacing::Percent(_)
        | Spacing::Rem(_)
        | Spacing::Em(_)
        | Spacing::Ch(_)
        | Spacing::Vw(_)
        | Spacing::Vh(_) => true,
        // A margin calc() may be negative; padding has its own variant.
        Spacing::Calc { .. } => allow_auto,
        Spacing::PaddingCalc { .. } => !allow_auto,
        Spacing::Auto => allow_auto,
        // env() insets depend on host state the legacy path owns.
        _ => false,
    }
}

/// Internal custom properties that only carry computed values for the
/// CSSOM, font selection or paint; they never mean a legacy layout
/// projection owns the box.
/// CSS Contain 2 size containment (the cascade keeps the `size` token;
/// `Contain` folds tokens together).
const SIZE_CONTAINMENT: &str = "--w3cos-internal-size-containment";
/// `contain-intrinsic-size` per axis: "<w> <h>", px or `none`.
const CONTAIN_INTRINSIC_SIZE: &str = "--w3cos-internal-contain-intrinsic-size";
/// `width: max-content | min-content | fit-content` (width is auto).
pub(super) const WIDTH_KEYWORD: &str = "--w3cos-internal-width-keyword";
/// `stretch` on a size property (see the DOM marker): the axes that carry it.
pub(super) const STRETCH: &str = "--w3cos-internal-stretch";
pub(super) const MIN_WIDTH_KEYWORD: &str = "--w3cos-internal-min-width-keyword";
/// `min-height` / `max-height` intrinsic keywords (the limit stays auto /
/// none in the style). Block flow resolves them to the content height;
/// other formatting contexts ignore them, as before the cascade kept them.
pub(super) const MIN_HEIGHT_KEYWORD: &str = "--w3cos-internal-min-height-keyword";
pub(super) const MAX_HEIGHT_KEYWORD: &str = "--w3cos-internal-max-height-keyword";
pub(super) const MAX_WIDTH_KEYWORD: &str = "--w3cos-internal-max-width-keyword";
const COMPUTED_HEIGHT_PERCENT: &str = "--w3cos-internal-computed-height-percent";

/// The `height` to size a box with. DOM lowering turns a percentage height
/// under an auto-height block parent into `auto` (an indefinite basis,
/// CSS 2.1 10.5) and keeps the percentage in a marker. When the parent's
/// block size is definite after all (stretched between its insets, a
/// stretched flex item, an aspect ratio), the percentage resolves against
/// it, as in Chrome (css-sizing intrinsic-percent-replaced-005).
pub(super) fn used_height(style: &Style, space: &ConstraintSpace) -> Dimension {
    if style.height == Dimension::Auto && space.percentage_block.is_some() {
        if let Some(value) = marker(style, COMPUTED_HEIGHT_PERCENT).and_then(|v| v.parse().ok()) {
            return Dimension::Percent(value);
        }
    }
    style.height
}

pub(super) fn marker<'a>(style: &'a Style, key: &str) -> Option<&'a str> {
    style
        .custom_properties
        .as_ref()
        .and_then(|p| p.get(key))
        .map(String::as_str)
}

/// An HTML `<button>` (the DOM marks it; a collapsed `<select>` lowers to
/// the same component kind but is a menu list, Blink LayoutMenuList).
pub(super) fn is_html_button(info: &FlatNodeInfo) -> bool {
    matches!(info.kind, ComponentKind::Button { .. })
        && marker(info.style, w3cos_dom::user_agent::BUTTON_PROPERTY).is_some()
}

/// The label a button leaf lays out: none for an empty button (its
/// placeholder label is only painted) or one with rendered children.
fn button_leaf_label<'a>(flat: &'a [FlatNodeInfo], index: usize) -> Option<&'a str> {
    let info = &flat[index];
    let ComponentKind::Button { label } = info.kind else {
        return None;
    };
    (inline::rendered_children(flat, index).next().is_none()
        && !label.is_empty()
        && marker(info.style, w3cos_dom::user_agent::BUTTON_PROPERTY) != Some("empty"))
    .then_some(label.as_str())
}

pub(super) fn size_contained(style: &Style) -> bool {
    marker(style, SIZE_CONTAINMENT).is_some()
}

/// The content-box size a size-contained box is laid out as if it had
/// (Blink ComputeMinMaxSizes / ComputeIntrinsicBlockSize with
/// ShouldApplySizeContainment: the contain-intrinsic size, else 0).
pub(super) fn contain_intrinsic_size(style: &Style) -> (f32, f32) {
    let mut axes = marker(style, CONTAIN_INTRINSIC_SIZE)
        .unwrap_or("")
        .split_ascii_whitespace()
        .map(|v| v.parse::<f32>().unwrap_or(0.0));
    (axes.next().unwrap_or(0.0), axes.next().unwrap_or(0.0))
}

fn is_value_carrier_marker(key: &str) -> bool {
    let name = key.trim_start_matches("--w3cos-internal-");
    matches!(
        name,
        "font-size-origin"
            | "font-specified-size"
            | "html-standard-font"
            | "list-style-image"
            | "text-language"
            | "text-decoration-color"
            | "z-index-specified"
            | "computed-height-percent"
            | "background-inherit"
            | "text-source-run"
            | "text-fragment-ends"
            | "quirks-mode"
            | "rendered-legend"
            | "document-element"
            | "size-containment"
            | "contain-intrinsic-size"
            // The computed `tab-size` (inherited). Text measure and paint
            // read it for preserved tabs; `text:tab` gates the leaves whose
            // stops need a line position NG does not pass yet.
            | "tab-size"
            // Grid reads these. The cascade leaves the legacy fields alone:
            // height stays the dimension it already stored, and justify-content
            // stays the enum flex already uses.
            | "height-keyword"
            | "justify-content"
            // Grid reads these: a replaced item stretches only for the
            // authored `stretch` keyword, not for `normal`.
            | "align-items-stretch"
            | "justify-items-stretch"
            // Flex reads this for `align-content: safe …` overflow.
            // The used align-content enum stays the legacy parse.
            | "safe-align-content"
            // `flex-wrap: balance` as the NG entry patched it: the flex
            // algorithm breaks the lines evenly.
            | "flex-balance"
            | "flex-line-count"
            // Flex reads this as a column wrap's definite block size.
            // Height stays auto, so legacy layout ignores the ratio.
            | "aspect-ratio"
            // `auto && <ratio>`: the ratio sizes a non-replaced content box.
            | "aspect-ratio-auto"
            // Grid-lanes stays `display: flex` for legacy. NG reads the
            // marker and lays the box out as a grid. Only a grid container
            // consults these; it resolves the `<percent> <px>` gap values.
            | "grid-lanes"
            | "percent-gap"
            | "row-gap-percent"
            | "column-gap-percent"
            // `word-break: auto-phrase`: inline text drops soft hyphens
            // as break opportunities.
            | "word-break-auto-phrase"
            // `-webkit-box` reads these (webkit_box_styles); on any other
            // box they have no effect.
            | "webkit-box-orient"
            | "webkit-line-clamp"
            | "clamp-lines"
            // Read with the run-in display marker (run_in_block_styles).
            | "run-in-block"
    ) || name.starts_with("decoration-owner")
        // Logical sizes the cascade does not resolve. Legacy layout ignores
        // them too, so both engines size the box the same way; falling back
        // only for these boxes moved tests and references apart.
        || name.starts_with("logical-size-")
}

/// Why a box is outside the current stage (`None`: supported).  The reason
/// is reported by `W3COS_LAYOUT_NG_TRACE` for coverage accounting.
fn unsupported_style(style: &Style, allowed: &[&str]) -> Option<&'static str> {
    unsupported_box_style(style, allowed, false)
}

/// `atomic`: an inline-block in an IFC (its display is the IFC's concern).
fn unsupported_box_style(style: &Style, allowed: &[&str], atomic: bool) -> Option<&'static str> {
    if let Some(reason) = unsupported_marker(style, allowed) {
        return Some(reason);
    }
    // A flex row the DOM lowered from an inline formatting context carries
    // the IFC marker and is laid out as an IFC, not by the flex algorithm.
    // An author `display:flex` (no marker) is a flex container (S5).
    let lowered_ifc = matches!(style.display, Display::Flex | Display::InlineFlex)
        && marker(style, inline::IFC_MARKER).is_some();
    let author_flex = style.display == Display::Flex && !lowered_ifc;
    // `display:grid` is a grid container when `grid::container_reason` accepts
    // the template. `inline-grid` keeps `display: flex` for legacy and is a
    // grid here (`is_author_grid`).
    let author_grid = style.display == Display::Grid || is_inline_grid(style);
    // CSS 2.1 §9.7 blockifies inline-block. A floated list-item keeps a
    // block principal box. The marker is not painted; a page that needs
    // it stays on a named fallback from the gate, not from this check.
    let blockified_float = style.float != Float::None
        && !oof::is_out_of_flow(style)
        && matches!(style.display, Display::InlineBlock | Display::ListItem);
    // So are absolutely positioned inline and inline-block boxes (the
    // replaced and text ones are rejected by position_reason).
    let blockified_oof = oof::is_out_of_flow(style)
        && matches!(style.display, Display::Inline | Display::InlineBlock);
    if !atomic
        && !blockified_float
        && !blockified_oof
        && !matches!(style.display, Display::Block | Display::FlowRoot)
        && !lowered_ifc
        && !author_flex
        && !author_grid
    {
        return Some(match style.display {
            Display::Inline => "display:inline",
            Display::InlineBlock => "display:inline-block",
            Display::Flex | Display::InlineFlex => "display:flex",
            _ => "display:other",
        });
    }
    // Positioned block-level boxes are checked by node_reason
    // (position_reason); relatively positioned atomics move with their
    // line (inline::relative_inline_offsets), other atomics stay on
    // legacy. A sticky block-level box is laid out as a static one (its
    // insets only move it at paint time); see position_reason.
    if atomic && !matches!(style.position, Position::Static | Position::Relative) {
        return Some("position");
    }
    // Floats and clearance of block-level boxes are checked by node_reason
    // (float_reason); atomics stay on legacy.
    if atomic && style.float != Float::None {
        return Some("float");
    }
    if atomic && style.clear != Clear::None {
        return Some("clear");
    }
    // CSS 2.1 9.4.1 / 11.1.1: a non-visible overflow on a block container
    // establishes a BFC. Paint already clips that padding box. Inline boxes
    // and non-cell internal table boxes do not clip; those stay on legacy.
    if non_visible_overflow(style) && !style.display.establishes_overflow_clip() {
        return Some("overflow:inline");
    }
    if style.column_count.is_some() || style.column_width != Dimension::Auto {
        return Some("columns");
    }
    // direction: rtl places block children from the right (resolve_inline).
    if !block_unicode_bidi_ok(style) {
        return Some("unicode-bidi");
    }
    if ![
        style.width,
        style.height,
        style.min_width,
        style.min_height,
        style.max_width,
        style.max_height,
    ]
    .into_iter()
    .all(supported_dimension)
    {
        return Some("size-unit");
    }
    if ![
        style.margin.top,
        style.margin.right,
        style.margin.bottom,
        style.margin.left,
    ]
    .into_iter()
    .all(|s| supported_spacing(s, true))
        || ![
            style.padding.top,
            style.padding.right,
            style.padding.bottom,
            style.padding.left,
        ]
        .into_iter()
        .all(|s| supported_spacing(s, false))
    {
        return Some("spacing-unit");
    }
    None
}

/// unicode-bidi on a block container (CSS Writing Modes 3 2.2): embed and
/// isolate do nothing beyond its content being a paragraph (the HTML UA
/// sheet gives every `[dir]` element `isolate`); bidi-override and
/// plaintext change the order or base direction of its inline content.
pub(super) fn block_unicode_bidi_ok(style: &Style) -> bool {
    matches!(
        style.unicode_bidi,
        UnicodeBidi::Normal | UnicodeBidi::Embed | UnicodeBidi::Isolate
    )
}

/// Internal markers mean a legacy projection owns this box, except value
/// carriers and the markers the current stage interprets (`allowed`).
/// The DOM's mark on a fieldset's first legend child (HTML §15.3.13).
fn is_rendered_legend(style: &Style) -> bool {
    style
        .custom_properties
        .as_ref()
        .is_some_and(|props| props.contains_key("--w3cos-internal-rendered-legend"))
}

pub(super) fn unsupported_marker(style: &Style, allowed: &[&str]) -> Option<&'static str> {
    style
        .custom_properties
        .as_ref()
        .is_some_and(|props| {
            props.keys().any(|key| {
                key.starts_with("--w3cos-internal")
                    && !is_value_carrier_marker(key)
                    && !allowed.contains(&key.as_str())
                    // Intrinsic width keywords: in-flow blocks resolve
                    // them (keyword_inline_size), flex containers through
                    // apply_width_keywords.
                    && key != WIDTH_KEYWORD
                    // `stretch`: block containers resolve it for their
                    // children (stretch_child_styles); flex, grid and
                    // table containers fall back when a child has it.
                    && key != STRETCH
                    // Blocks and grid items resolve min-width keywords and
                    // blocks max-width keywords; elsewhere the limit stays
                    // auto / none, as before the cascade kept them.
                    && key != MIN_WIDTH_KEYWORD
                    && key != MAX_WIDTH_KEYWORD
                    && key != MIN_HEIGHT_KEYWORD
                    && key != MAX_HEIGHT_KEYWORD
            })
        })
        .then_some("internal-marker")
}

/// `vertical-align` applies to inline-level boxes and table cells only
/// (CSS 2.1 10.8.1; NGBlockLayoutAlgorithm never reads it), so a
/// block-level box ignores the keyword marker (and the align-self the
/// cascade derives from it: block flow does not read align-self).  Length
/// values stay gated: the cascade also turns them into margins.
/// The DOM wraps runs of left floats in a synthetic flex row (consecutive
/// floats, or every float from a cleared one on).  NG reads the row as
/// transparent: its children are floats of the parent's BFC.
pub(super) const FLOAT_GROUP_MARKERS: [&str; 2] = [
    "--w3cos-internal-anonymous-float-group",
    "--w3cos-internal-cleared-float-wrapper",
];

pub(super) fn is_float_group(style: &Style) -> bool {
    style
        .custom_properties
        .as_ref()
        .is_some_and(|props| FLOAT_GROUP_MARKERS.iter().any(|m| props.contains_key(*m)))
}

fn float_group_reason(flat: &[FlatNodeInfo], index: usize) -> Option<&'static str> {
    let info = &flat[index];
    let style = info.style;
    if unsupported_marker(style, &FLOAT_GROUP_MARKERS).is_some() {
        return Some("float-group:marker");
    }
    let parent = info.parent.why("float-parent")?;
    if inline::is_inline_container(flat, parent)
        || matches!(flat[parent].kind, ComponentKind::Text { .. })
        || is_float_group(flat[parent].style)
        || !matches!(info.kind, ComponentKind::Row | ComponentKind::Box)
    {
        return Some("float-group:context");
    }
    let zero = |s: Spacing| matches!(s, Spacing::Px(v) if v == 0.0);
    if border(style) != [0.0; 4]
        || ![
            style.margin.top,
            style.margin.right,
            style.margin.bottom,
            style.margin.left,
            style.padding.top,
            style.padding.right,
            style.padding.bottom,
            style.padding.left,
        ]
        .into_iter()
        .all(zero)
        || style.position != Position::Static
    {
        return Some("float-group:box");
    }
    if inline::rendered_children(flat, index).any(|child| {
        let s = flat[child].style;
        s.float != Float::Left || oof::is_out_of_flow(s)
    }) {
        return Some("float-group:content");
    }
    None
}

fn node_reason(flat: &[FlatNodeInfo], index: usize) -> Option<&'static str> {
    if is_float_group(flat[index].style) {
        return float_group_reason(flat, index);
    }
    position_reason(flat, index)
        .or_else(|| float_reason(flat, index))
        .or_else(|| node_box_reason(flat, index))
}

/// CSS 2.1 §9.7: a non-none float blockifies `inline-block`. `list-item`
/// is already a block-level principal box, and the float lays that box
/// out as a block. Replaced boxes stay on their own reasons. Floated
/// flex and grid stay `float:display`.
pub(super) fn is_blockified_float(flat: &[FlatNodeInfo], index: usize) -> bool {
    let info = &flat[index];
    let style = info.style;
    style.float != Float::None
        && !oof::is_out_of_flow(style)
        && matches!(style.display, Display::InlineBlock | Display::ListItem)
        && !matches!(
            info.kind,
            ComponentKind::Image { .. }
                | ComponentKind::Canvas { .. }
                | ComponentKind::TextInput { .. }
                | ComponentKind::SvgDocument { .. }
        )
        && !matches!(info.kind, ComponentKind::SvgDocument { .. })
}

/// Floats of block-level boxes in a block container (S3b).  Floats among
/// inline content (NGInlineLayoutAlgorithm's float handling) and text
/// that would have to wrap around a float stay on legacy.
fn float_reason(flat: &[FlatNodeInfo], index: usize) -> Option<&'static str> {
    let info = &flat[index];
    let style = info.style;
    if style.float == Float::None || oof::is_out_of_flow(style) {
        return None;
    }
    // An inline-block or list-item float is a block box (§9.7). An
    // inline-block that is still inside an IFC hits `float:inline` below.
    let blockified = is_blockified_float(flat, index);
    if !blockified && (inline::is_atomic_inline(flat, index) || style.display == Display::Inline) {
        return Some("float:inline");
    }
    // A floated flex or grid container is a BFC root whose inner display
    // picks the algorithm (NGBlockNode::Layout); PositionFloat places its
    // margin box like any other float. A floated `inline-grid` stays here.
    let flex_or_grid = is_author_flex(style) || (is_author_grid(style) && !is_inline_grid(style));
    if !blockified
        && !flex_or_grid
        && !matches!(
            style.display,
            Display::Block | Display::FlowRoot | Display::Table
        )
        && !(is_author_flex(style) || style.display == Display::Grid)
    {
        return Some("float:display");
    }
    let Some(parent) = info.parent else {
        return Some("float:root");
    };
    // Legacy adds the float margins to the clearance of a cleared parent
    // (overflow-area-003-ref rows 112px apart, Chrome and NG 96px). Its
    // test still falls back for flex:column-align / flex:oof, so a flex or
    // grid float in a cleared box stays on legacy with it.
    if flex_or_grid && !blockified {
        let mut box_parent = parent;
        while is_float_group(flat[box_parent].style)
            && let Some(up) = flat[box_parent].parent
        {
            box_parent = up;
        }
        if flat[box_parent].style.clear != Clear::None {
            return Some("float:display");
        }
    }
    if inline::is_inline_container(flat, parent)
        || matches!(flat[parent].kind, ComponentKind::Text { .. })
    {
        return Some("float:inline");
    }
    None
}

/// CSS2 9.5.2: the BFC block offset a `clear` box's border box must not
/// be above (NGExclusionSpace::ClearanceOffset).
fn clearance_offset(style: &Style, exclusions: &ExclusionSpace) -> Option<f32> {
    match style.clear {
        Clear::None => None,
        Clear::Left => exclusions.clearance_offset(true, false),
        Clear::Right => exclusions.clearance_offset(false, true),
        Clear::Both => exclusions.clearance_offset(true, true),
    }
}

/// CSS2 10.3.5 / 10.6.7 / 9.5.1 (Blink PositionFloat): lay out a float
/// as a BFC root -- its specified or shrink-to-fit width, auto margins 0
/// -- and place its margin box in the exclusion space.  `start`/`width`:
/// the containing block's content box in BFC coordinates.  Returns the
/// float's border-box BFC offset.
fn layout_float(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    exclusions: &mut ExclusionSpace,
    start: f32,
    block_start: f32,
) -> Option<(f32, f32, LayoutResult)> {
    let style = flat[index].style;
    let side = |s: Spacing| match s {
        Spacing::Auto => 0.0,
        s => resolve_spacing_for_layout(
            s,
            if space.percent_unresolved {
                0.0
            } else {
                space.available_inline
            },
            style,
            space.viewport_w,
            space.viewport_h,
        ),
    };
    let b = border(style);
    let edges = b[1] + b[3] + side(style.padding.left) + side(style.padding.right);
    let (ml, mr) = (side(style.margin.left), side(style.margin.right));
    let (mt, mb) = (side(style.margin.top), side(style.margin.bottom));
    let width = match resolve_dim(style.width, Some(space.available_inline), style, space) {
        Some(w) => clamp_inline(
            style,
            space,
            edges,
            match style.box_sizing {
                BoxSizing::BorderBox => (w - edges).max(0.0),
                BoxSizing::ContentBox => w,
            },
        ),
        None => {
            let available = space.available_inline - ml - mr - edges;
            let sizes = intrinsic_sizes(flat, index, space).why("float-intrinsic")?;
            let w = match sizes.min {
                Some(min) => sizes.max.min(available.max(min)),
                None if sizes.max <= available + 1.0 / 128.0 => sizes.max,
                None => return super::bail("float-shrink-min"),
            };
            clamp_inline(style, space, edges, w.max(0.0))
        }
    };
    let result =
        layout_block_sized(flat, index, space, true, Some((width, None))).why("float-layout")?;
    let margin_w = ml + result.fragment.width + mr;
    let margin_h = mt + result.fragment.height + mb;
    let (x, y) = exclusions.place_float(
        margin_w,
        margin_h,
        style.float == Float::Left,
        start,
        start + space.available_inline,
        block_start,
    );
    Some((x + ml, y + mt, result))
}

/// Relative and out-of-flow positioning of block-level boxes (S3a).  An
/// out-of-flow box whose static position lies in an inline formatting
/// context (NGInlineLayoutAlgorithm's OOF handling) stays on legacy.
fn position_reason(flat: &[FlatNodeInfo], index: usize) -> Option<&'static str> {
    let info = &flat[index];
    let style = info.style;
    match style.position {
        Position::Static => None,
        Position::Sticky => {
            if inline::is_atomic_inline(flat, index) || style.display == Display::Inline {
                Some("position:inline")
            } else {
                None
            }
        }
        // An inline-level relatively positioned box in an IFC moves its
        // fragments after line layout (inline::relative_inline_offsets).
        Position::Relative
            if (inline::is_atomic_inline(flat, index) || style.display == Display::Inline)
                && !inline::in_inline_container(flat, index) =>
        {
            Some("position:inline")
        }
        Position::Relative => None,
        Position::Absolute | Position::Fixed if inline_level_oof(flat, index) => {
            // Blockified (CSS2 9.7) and laid out like a block box; its
            // static position is still inline-level (`inline_static_offset`).
            // In an IFC it would be an OOF inline item: legacy.
            if info.parent.is_none_or(|p| {
                inline::is_inline_container(flat, p) || inline::in_inline_container(flat, index)
            }) {
                return Some("position:inline");
            }
            if style.position == Position::Fixed {
                return oof::fixed_reason(flat, index);
            }
            None
        }
        _ if inline::is_atomic_inline(flat, index) || style.display == Display::Inline => {
            Some("position:inline")
        }
        Position::Absolute | Position::Fixed => {
            // An absolutely positioned box is blockified (CSS2 9.7):
            // `inline-flex` lays out as `flex`, and OOF layout sizes a flex
            // container like a block box (Blink
            // OutOfFlowLayoutPart::LayoutOOFNode runs its own algorithm).
            if !matches!(
                style.display,
                Display::Block | Display::FlowRoot | Display::Flex | Display::InlineFlex
            ) {
                return Some("position:display");
            }
            let Some(parent) = info.parent else {
                return Some("position:root");
            };
            if inline::is_inline_container(flat, parent)
                || matches!(flat[parent].kind, ComponentKind::Text { .. })
            {
                return Some("position:inline-static");
            }
            if style.position == Position::Fixed {
                return oof::fixed_reason(flat, index);
            }
            None
        }
    }
}

/// An absolutely positioned non-replaced box whose original display is
/// `inline` or `inline-block` (Blink IsOriginalDisplayInlineType).
pub(super) fn inline_level_oof(flat: &[FlatNodeInfo], index: usize) -> bool {
    let info = &flat[index];
    oof::is_out_of_flow(info.style)
        && matches!(info.style.display, Display::Inline | Display::InlineBlock)
        && matches!(
            info.kind,
            ComponentKind::Box
                | ComponentKind::Column
                | ComponentKind::Row
                | ComponentKind::Image { .. }
                | ComponentKind::Canvas { .. }
        )
}

/// CalculateOutOfFlowStaticInlineLevelOffset: the static inline offset of
/// an inline-level OOF box in a block-level context is where a zero-sized
/// line would start, i.e. inside the layout opportunity beside the floats
/// at that block offset, aligned by text-align and moved by text-indent.
/// `bfc` is the container content box's BFC offset; the result is the
/// logical offset from the content box's inline-start edge.
fn inline_static_offset(
    container: &Style,
    exclusions: &ExclusionSpace,
    bfc: (f32, f32),
    block: f32,
    available: f32,
    rtl: bool,
) -> Option<f32> {
    let (left, right) = exclusions.band(bfc.1 + block, 0.0);
    let start = left.map_or(0.0, |l| (l - bfc.0).max(0.0));
    let end = right.map_or(available, |r| (r - bfc.0).min(available));
    let size = (end - start).max(0.0);
    let line = inline::align_offset(container, size);
    let mut offset = if rtl {
        (available - end) + (size - line)
    } else {
        start + line
    };
    offset += match container.text_indent {
        Dimension::Px(v) => v,
        Dimension::Percent(p) => p / 100.0 * available,
        _ => return None,
    };
    Some(offset)
}

/// Inline text whose parent is a block container but not an inline
/// formatting context. CSS 2.1 9.2.1.1 wraps it in an anonymous block.
fn anonymous_block_text(flat: &[FlatNodeInfo], index: usize) -> bool {
    let Some(parent) = flat[index].parent else {
        return false;
    };
    if inline::is_inline_container(flat, parent) || inline::is_inline_box(flat, parent) {
        return false;
    }
    matches!(
        flat[parent].style.display,
        Display::Block
            | Display::FlowRoot
            | Display::InlineBlock
            | Display::ListItem
            | Display::TableCell
            | Display::TableCaption
    )
}

/// An `<img>` that is a flex or grid item. The item is blockified (Flexbox
/// 4, Grid 6.1), so its UA `inline`/`inline-block` display is not an atomic
/// inline of an IFC; the container sizes it from its replaced size.
pub(super) fn is_replaced_item(flat: &[FlatNodeInfo], index: usize) -> bool {
    matches!(flat[index].kind, ComponentKind::Image { .. })
        && flat[index].parent.is_some_and(|parent| {
            is_author_flex(flat[parent].style) || is_author_grid(flat[parent].style)
        })
}

fn node_box_reason(flat: &[FlatNodeInfo], index: usize) -> Option<&'static str> {
    let info = &flat[index];
    // Table, row, and cell boxes. The grid is laid out here; other
    // `display` values stay `display:other`.
    if super::table::is_table_display(info.style.display) {
        return super::table::reason(flat, index);
    }
    if let Some(reason) = overflow_reason(flat, index) {
        return Some(reason);
    }
    if matches!(info.kind, ComponentKind::Image { .. }) {
        return image_box_reason(flat, index);
    }
    if matches!(info.kind, ComponentKind::Canvas { .. }) {
        return canvas_box_reason(flat, index);
    }
    if matches!(info.kind, ComponentKind::TextInput { .. }) {
        return text_field_box_reason(flat, index);
    }
    if let ComponentKind::SvgDocument { source, .. } = info.kind {
        return svg_box_reason(flat, index, source);
    }
    if matches!(info.kind, ComponentKind::Button { .. }) {
        return button_box_reason(flat, index);
    }
    if inline::is_atomic_inline(flat, index) && !is_blockified_float(flat, index) {
        // A text run is an anonymous flex item whatever its siblings are:
        // an inline-block text under a flex container is block text even
        // when the container's first child is not inline-level.
        let flex_text_item = matches!(info.kind, ComponentKind::Text { .. })
            && info
                .parent
                .is_some_and(|parent| is_author_flex(flat[parent].style));
        if !flex_text_item && !inline::in_inline_container(flat, index) {
            return Some("display:inline-block");
        }
        // Its display and vertical-align are the IFC's concern.
        let own = match info.kind {
            ComponentKind::Text { content } => unsupported_box_style(
                info.style,
                &[inline::VALIGN_MARKER, inline::VALIGN_LENGTH_MARKER],
                true,
            )
            .or_else(|| inline::block_text_reason(content, info.style)),
            _ if inline::is_inline_container(flat, index) => unsupported_box_style(
                info.style,
                &[
                    inline::IFC_MARKER,
                    inline::ANONYMOUS_BLOCK_MARKER,
                    inline::VALIGN_MARKER,
                    inline::VALIGN_LENGTH_MARKER,
                ],
                true,
            )
            .or_else(|| inline::container_reason(flat, index)),
            _ => unsupported_box_style(
                info.style,
                &[inline::VALIGN_MARKER, inline::VALIGN_LENGTH_MARKER],
                true,
            ),
        };
        return own.or_else(|| inline::atomic_inline_reason(info.style));
    }
    match info.kind {
        ComponentKind::Root | ComponentKind::Box | ComponentKind::Column | ComponentKind::Row => {
            if inline::is_inline_box(flat, index) {
                // Validated with its IFC container (container_reason).
                (!inline::in_inline_container(flat, index)).then_some("display:inline")
            } else if is_author_flex(info.style) {
                // `run-in`, `-webkit-box`, and `-webkit-inline-box` compute
                // to `display: flex` for legacy. When this stage can lay
                // out that flex line, NG uses the same box. A rejection
                // (a text item, baseline, rtl, …) keeps the unrecognized
                // marker, so a line-clamp reference does not switch engines
                // alone. `-webkit-box-orient` is not stored, so a vertical
                // box stays the row the legacy engine already paints.
                let flex_reason = super::flex::container_reason(flat, index).or_else(|| {
                    // (A vertical box that run_in_block_styles left alone
                    // is no row.)
                    (marker(info.style, "--w3cos-internal-webkit-box-orient") == Some("vertical")
                        && is_legacy_webkit_box(info.style))
                    .then_some("webkit-box-vertical")
                });
                // A text item keeps the marker too: this stage's flex does
                // not apply `-webkit-line-clamp` to that text's lines.
                let text_item = inline::rendered_children(flat, index)
                    .any(|child| matches!(flat[child].kind, ComponentKind::Text { .. }));
                let allowed: &[&str] = if (is_run_in(info.style)
                    || is_legacy_webkit_box(info.style))
                    && flex_reason.is_none()
                    && !text_item
                {
                    &[
                        inline::VALIGN_MARKER,
                        "--w3cos-internal-unrecognized-display",
                    ]
                } else {
                    &[inline::VALIGN_MARKER]
                };
                unsupported_style(info.style, allowed).or_else(|| flex_reason)
            } else if is_author_grid(info.style) {
                // CSS Flexbox §4 / Grid §6.1 blockify `inline-grid` on a
                // flex or grid item. A top-level `inline-grid` joins the
                // grid for a definite px box, and for an auto box whose
                // columns and rows are definite px tracks
                // (`is_inline_grid_line`). The other boxes stay on the
                // marker so a reference that still falls back does not
                // switch engines alone.
                let blockified = is_blockified_inline_grid(flat, index);
                let allowed: &[&str] = if blockified || is_inline_grid_line(flat, index) {
                    &[
                        inline::VALIGN_MARKER,
                        "--w3cos-internal-unrecognized-display",
                    ]
                } else {
                    &[inline::VALIGN_MARKER]
                };
                unsupported_style(info.style, allowed)
                    .or_else(|| super::grid::container_reason(flat, index))
            } else if inline::is_inline_container(flat, index) {
                unsupported_style(
                    info.style,
                    &[
                        inline::IFC_MARKER,
                        inline::ANONYMOUS_BLOCK_MARKER,
                        inline::VALIGN_MARKER,
                    ],
                )
                .or_else(|| inline::container_reason(flat, index))
            } else {
                unsupported_style(info.style, &[inline::VALIGN_MARKER])
            }
        }
        ComponentKind::Text { content }
            if info.style.display != Display::Inline
                || info.parent.is_some_and(|parent| {
                    is_author_grid(flat[parent].style) || is_author_flex(flat[parent].style)
                }) =>
        {
            // A text grid or flex item is an anonymous block (CSS Grid §6,
            // CSS Flexbox §4: each contiguous text run is wrapped in an
            // anonymous block container). Its display stays inline in the
            // cascade; the grid or flex algorithm lays it out as block text.
            unsupported_box_style(
                info.style,
                &[
                    inline::VALIGN_MARKER,
                    inline::BIDI_FRAGMENT_MARKERS[0],
                    inline::BIDI_FRAGMENT_MARKERS[1],
                    inline::BIDI_FRAGMENT_MARKERS[2],
                ],
                info.style.display == Display::Inline,
            )
            .or_else(|| inline::block_text_reason(content, info.style))
        }
        ComponentKind::Text { content } => {
            // Validated with its container (container_reason). A block
            // container that also has a block-level sibling wraps this
            // inline run in an anonymous block (CSS 2.1 9.2.1.1). Laying
            // the text out as block text keeps the page on NG; bailing
            // here would send only this page back to the legacy engine.
            if !inline::in_inline_container(flat, index) {
                if anonymous_block_text(flat, index) {
                    inline::block_text_reason(content, info.style)
                } else {
                    Some("ifc:mixed")
                }
            } else {
                unsupported_marker(
                    info.style,
                    &[
                        inline::LINE_WIDTH_MARKER,
                        inline::VALIGN_MARKER,
                        inline::VALIGN_LENGTH_MARKER,
                        inline::BIDI_FRAGMENT_MARKERS[0],
                        inline::BIDI_FRAGMENT_MARKERS[1],
                        inline::BIDI_FRAGMENT_MARKERS[2],
                    ],
                )
            }
        }
        ComponentKind::Button { .. } => Some("kind:button"),
        ComponentKind::SvgPath { .. } | ComponentKind::SvgDocument { .. } => Some("kind:svg"),
        _ => Some("kind:other"),
    }
}

/// HTML §15.5.3 button layout (Blink LayoutButton: the content sits in an
/// anonymous inner block, centered in the block axis; the box is an
/// inline-block whatever its inline display). Its children are checked as
/// the children of a block container; a label leaf as block text.
fn button_box_reason(flat: &[FlatNodeInfo], index: usize) -> Option<&'static str> {
    let info = &flat[index];
    let style = info.style;
    if !is_html_button(info) {
        return Some("kind:select");
    }
    if !matches!(
        style.display,
        Display::Block | Display::FlowRoot | Display::Inline | Display::InlineBlock
    ) {
        return Some("button:display");
    }
    if style.float != Float::None || style.position != Position::Static {
        return Some("button:float-or-position");
    }
    let percent = |dim: Dimension| matches!(dim, Dimension::Percent(_) | Dimension::Calc { .. });
    if [
        style.width,
        style.height,
        style.min_width,
        style.max_width,
        style.min_height,
        style.max_height,
    ]
    .into_iter()
    .any(percent)
    {
        return Some("button:percent");
    }
    if size_contained(style) {
        return Some("button:contain");
    }
    if info
        .parent
        .is_some_and(|parent| is_author_grid(flat[parent].style))
    {
        return Some("button:grid-item");
    }
    let allowed = [
        inline::VALIGN_MARKER,
        // Paint only here: `appearance: none` keeps the button's layout
        // (the UA edges are ordinary declarations).
        w3cos_dom::user_agent::CONTROL_APPEARANCE_PROPERTY,
        w3cos_dom::user_agent::BUTTON_PROPERTY,
        inline::VALIGN_LENGTH_MARKER,
    ];
    let block_allowed = &allowed[..allowed.len() - 1];
    let content = || {
        if let Some(label) = button_leaf_label(flat, index) {
            inline::block_text_content_reason(label, style)
        } else if inline::is_inline_container(flat, index) {
            inline::container_reason(flat, index)
        } else {
            None
        }
    };
    if info
        .parent
        .is_some_and(|parent| is_author_flex(flat[parent].style))
    {
        // A flex item is blockified (CSS Display §2.7).
        return unsupported_box_style(style, block_allowed, true).or_else(content);
    }
    if matches!(style.display, Display::Block | Display::FlowRoot)
        && info.parent.is_some_and(|parent| {
            inline::rendered_children(flat, parent).any(|child| {
                child != index
                    && flat[child].style.float != Float::None
                    && !oof::is_out_of_flow(flat[child].style)
            })
        })
    {
        return Some("button:beside-float");
    }
    match style.display {
        Display::Block | Display::FlowRoot => {
            unsupported_box_style(style, block_allowed, false).or_else(content)
        }
        _ if inline::in_inline_container(flat, index) => {
            unsupported_box_style(style, &allowed, true)
                .or_else(|| inline::atomic_inline_reason(style))
                .or_else(content)
        }
        _ => Some("button:inline"),
    }
}

/// Replaced boxes this stage lays out (CSS 2.1 10.3.2 inline, 10.3.4 block).
/// Floats, abspos, border-box, and flex/grid items stay on legacy: their
/// used size is not the content-box intrinsic size this path resolves.
fn image_box_reason(flat: &[FlatNodeInfo], index: usize) -> Option<&'static str> {
    let info = &flat[index];
    let style = info.style;
    if is_replaced_item(flat, index) {
        // Floats do not apply to flex and grid items, and an abspos child
        // is not an item; both keep the legacy path.
        if style.float != Float::None
            || matches!(
                style.position,
                Position::Absolute | Position::Fixed | Position::Sticky
            )
        {
            return Some("image");
        }
        let parent = flat[index].parent.map(|parent| flat[parent].style);
        // A percentage size in an automatic grid track is cyclic (the track
        // sizes from the item's contribution, CSS Sizing 3 5.2.2). An
        // out-of-flow flex container's inset-derived height is transferred
        // through the item's ratio by `flex::layout`.
        if parent.is_some_and(|parent| {
            is_author_grid(parent)
                && [style.width, style.min_width, style.max_width]
                    .iter()
                    .any(|dim| matches!(dim, Dimension::Percent(_)))
        }) {
            return Some("image");
        }
        return unsupported_box_style(style, &[inline::VALIGN_MARKER], true);
    }
    // A border-box image is sized through its content box
    // (`replaced_content_size`). A CSS `aspect-ratio` relates the
    // border boxes then, which stays on legacy.
    if style.box_sizing == BoxSizing::BorderBox
        && marker(style, "--w3cos-internal-aspect-ratio").is_some()
    {
        return Some("image:border-box-ratio");
    }
    // min/max constraints: `constrained_replaced_size` (Blink
    // ComputeReplacedSizeInternal).
    if style.float != Float::None || style.position == Position::Sticky {
        return Some("image:float-or-oof");
    }
    // An out-of-flow replaced box is sized by `oof::layout_out_of_flow`
    // (CSS 2.1 10.3.8, 10.6.5); in a flex or grid container the container
    // places it, which stays on legacy.
    if oof::is_out_of_flow(style) {
        if info.parent.is_none_or(|parent| {
            is_author_flex(flat[parent].style) || is_author_grid(flat[parent].style)
        }) {
            return Some("image:float-or-oof");
        }
        return unsupported_box_style(
            style,
            &[inline::VALIGN_MARKER, inline::VALIGN_LENGTH_MARKER],
            false,
        );
    }
    // A flex item is sized by the flex algorithm (`layout_block_in` takes
    // its used content box). Grid items stay on legacy.
    if info
        .parent
        .is_some_and(|parent| is_author_grid(flat[parent].style))
    {
        return Some("image:flex-grid-item");
    }
    if info
        .parent
        .is_some_and(|parent| is_author_flex(flat[parent].style))
    {
        // Its inline-level display is blockified (CSS Display §2.7).
        return unsupported_box_style(style, &[inline::VALIGN_MARKER], true);
    }
    // A block replaced box must not overlap a float in the same BFC
    // (CSS 2.1 9.5). Placement beside a float is still the legacy path.
    if matches!(style.display, Display::Block | Display::FlowRoot)
        && info.parent.is_some_and(|parent| {
            inline::rendered_children(flat, parent).any(|child| {
                child != index
                    && flat[child].style.float != Float::None
                    && !oof::is_out_of_flow(flat[child].style)
            })
        })
    {
        return Some("image:beside-float");
    }
    match style.display {
        Display::Block | Display::FlowRoot => {
            unsupported_box_style(style, &[inline::VALIGN_MARKER], false)
        }
        // An inline replaced box in an inline formatting context is an
        // atomic inline (CSS 2.1 10.8.1). Outside one it stays on legacy.
        Display::Inline | Display::InlineBlock if inline::in_inline_container(flat, index) => {
            unsupported_box_style(
                style,
                &[inline::VALIGN_MARKER, inline::VALIGN_LENGTH_MARKER],
                true,
            )
            .or_else(|| inline::atomic_inline_reason(style))
        }
        Display::Inline | Display::InlineBlock => Some("image:inline"),
        _ => Some("image:display"),
    }
}

/// A definite `<canvas>` is a replaced box (CSS 2.1 10.3.2 / 10.4).
/// The width and height attributes are the intrinsic size. A specified
/// `aspect-ratio` replaces that ratio. SVG and form controls have
/// their own `kind:*` reasons.
fn canvas_box_reason(flat: &[FlatNodeInfo], index: usize) -> Option<&'static str> {
    let info = &flat[index];
    let style = info.style;
    if style.box_sizing == BoxSizing::BorderBox || size_contained(style) {
        return Some("canvas:border-box-or-contain");
    }
    if style.float != Float::None || style.position == Position::Sticky {
        return Some("canvas:float-or-oof");
    }
    // An out-of-flow replaced box is sized by `oof::layout_out_of_flow`
    // (CSS 2.1 10.3.8, 10.6.5); in a flex or grid container the container
    // places it, which stays on legacy.
    if oof::is_out_of_flow(style) {
        if info.parent.is_none_or(|parent| {
            is_author_flex(flat[parent].style) || is_author_grid(flat[parent].style)
        }) {
            return Some("canvas:float-or-oof");
        }
        return unsupported_box_style(
            style,
            &[inline::VALIGN_MARKER, inline::VALIGN_LENGTH_MARKER],
            false,
        );
    }
    // Like an image (Blink LayoutHTMLCanvas is a LayoutReplaced): a flex
    // item is sized by the flex algorithm and its inline-level display is
    // blockified (CSS Display §2.7). Grid items stay on legacy.
    if info
        .parent
        .is_some_and(|parent| is_author_grid(flat[parent].style))
    {
        return Some("canvas:flex-grid-item");
    }
    if info
        .parent
        .is_some_and(|parent| is_author_flex(flat[parent].style))
    {
        return unsupported_box_style(style, &[inline::VALIGN_MARKER], true);
    }
    if matches!(style.display, Display::Block | Display::FlowRoot)
        && info.parent.is_some_and(|parent| {
            inline::rendered_children(flat, parent).any(|child| {
                child != index
                    && flat[child].style.float != Float::None
                    && !oof::is_out_of_flow(flat[child].style)
            })
        })
    {
        return Some("canvas:beside-float");
    }
    match style.display {
        Display::Block | Display::FlowRoot => {
            unsupported_box_style(style, &[inline::VALIGN_MARKER], false)
        }
        // An inline canvas in an inline formatting context is an atomic
        // inline, like an inline image (CSS 2.1 10.8.1; Blink
        // LayoutHTMLCanvas is a LayoutReplaced). Outside one it stays on
        // legacy.
        Display::Inline | Display::InlineBlock if inline::in_inline_container(flat, index) => {
            unsupported_box_style(
                style,
                &[inline::VALIGN_MARKER, inline::VALIGN_LENGTH_MARKER],
                true,
            )
            .or_else(|| inline::atomic_inline_reason(style))
        }
        Display::Inline | Display::InlineBlock => Some("canvas:inline"),
        _ => Some("canvas:display"),
    }
}

/// The `<input>` type states laid out as a single-line text field (Blink
/// LayoutTextControlSingleLine without a decoration or picker: no search
/// cancel button, number spin button, or date / color picker).
const TEXT_FIELD_TYPES: [&str; 5] = ["text", "password", "email", "url", "tel"];
const INPUT_SIZE_MARKER: &str = "--w3cos-internal-input-size";

/// An `<input>` text field (Blink LayoutTextControlSingleLine). It is not
/// replaced, but its box is a leaf here: the DOM keeps the inner editor
/// out of the tree and the painter draws the value. Its content box has
/// the intrinsic size of `size` characters by one line (no aspect ratio),
/// and an auto width fits that size in any context, as Blink
/// LayoutBox::AutoWidthShouldFitContent does for form controls.
/// Textareas, other input types, percentage sizes and floated or
/// positioned fields stay on legacy.
fn text_field_box_reason(flat: &[FlatNodeInfo], index: usize) -> Option<&'static str> {
    let info = &flat[index];
    let style = info.style;
    let textarea = is_textarea(style);
    if textarea {
        if !cfg!(feature = "skia")
            || marker(style, w3cos_dom::user_agent::TEXTAREA_COLS_PROPERTY).is_none()
            || marker(style, w3cos_dom::user_agent::TEXTAREA_ROWS_PROPERTY).is_none()
        {
            return Some("text-input:textarea");
        }
    } else {
        if !marker(style, w3cos_dom::user_agent::INPUT_TYPE_PROPERTY)
            .is_some_and(|state| TEXT_FIELD_TYPES.contains(&state))
        {
            return Some("text-input:type");
        }
        if !cfg!(feature = "skia") || marker(style, INPUT_SIZE_MARKER).is_none() {
            return Some("text-input:size");
        }
        // A textarea's rows take any used line height; a text field's
        // inner editor centering is only checked for `normal`.
        if !style.line_height_is_normal {
            return Some("text-input:line-height");
        }
    }
    let percent = |dim: Dimension| matches!(dim, Dimension::Percent(_) | Dimension::Calc { .. });
    if [
        style.width,
        style.height,
        style.min_width,
        style.max_width,
        style.min_height,
        style.max_height,
    ]
    .into_iter()
    .any(percent)
    {
        return Some("text-input:percent");
    }
    if style.float != Float::None
        || matches!(
            style.position,
            Position::Absolute | Position::Fixed | Position::Sticky
        )
    {
        return Some("text-input:float-or-oof");
    }
    // A block-level (or blockified) field ignores the vertical-align
    // keyword marker only; lengths stay gated (see unsupported_box_style).
    let allowed = [
        inline::VALIGN_MARKER,
        INPUT_SIZE_MARKER,
        w3cos_dom::user_agent::INPUT_TYPE_PROPERTY,
        // Paint only: a text field's `appearance` does not change its box
        // (Blink LayoutTextControl sizes the same for `none` and `auto`).
        w3cos_dom::user_agent::CONTROL_APPEARANCE_PROPERTY,
        w3cos_dom::user_agent::TEXTAREA_PROPERTY,
        w3cos_dom::user_agent::TEXTAREA_COLS_PROPERTY,
        w3cos_dom::user_agent::TEXTAREA_ROWS_PROPERTY,
        // Paint / interaction only (CSS UI `resize` adds no box).
        w3cos_dom::css_style::RESIZE_PROPERTY,
        inline::VALIGN_LENGTH_MARKER,
    ];
    let block_allowed = &allowed[..allowed.len() - 1];
    if info
        .parent
        .is_some_and(|parent| is_author_grid(flat[parent].style))
    {
        return Some("text-input:grid-item");
    }
    if info
        .parent
        .is_some_and(|parent| is_author_flex(flat[parent].style))
    {
        // A flex item is blockified (CSS Display §2.7).
        return unsupported_box_style(style, block_allowed, true);
    }
    if matches!(style.display, Display::Block | Display::FlowRoot)
        && info.parent.is_some_and(|parent| {
            inline::rendered_children(flat, parent).any(|child| {
                child != index
                    && flat[child].style.float != Float::None
                    && !oof::is_out_of_flow(flat[child].style)
            })
        })
    {
        return Some("text-input:beside-float");
    }
    match style.display {
        Display::Block | Display::FlowRoot => unsupported_box_style(style, block_allowed, false),
        // An inline-level field is an atomic inline (CSS 2.1 10.8.1).
        Display::Inline | Display::InlineBlock if inline::in_inline_container(flat, index) => {
            unsupported_box_style(style, &allowed, true)
                .or_else(|| inline::atomic_inline_reason(style))
        }
        Display::Inline | Display::InlineBlock => Some("text-input:inline"),
        _ => Some("text-input:display"),
    }
}

/// Intrinsic content size of a text field: Blink
/// LayoutTextControlSingleLine::PreferredContentLogicalWidth (`size`
/// average character widths plus the max-minus-average overhang) by the
/// inner editor's line height (`line-height: normal`). The same metrics
/// as the legacy engine (`render_skia::html_text_input_content_size`).
fn text_field_intrinsic(style: &Style) -> Option<(f32, f32)> {
    #[cfg(feature = "skia")]
    {
        if is_textarea(style) {
            return textarea_intrinsic(style);
        }
        let size = marker(style, INPUT_SIZE_MARKER)?.parse::<u32>().ok()?;
        Some(crate::render_skia::html_text_input_content_size(
            style, size,
        ))
    }
    #[cfg(not(feature = "skia"))]
    {
        let _ = style;
        None
    }
}

/// A `<textarea>` (Blink LayoutTextControlMultiLine's box): a leaf here
/// as well, a scroll container (`overflow: auto` from the UA sheet) whose
/// value the painter draws.
fn is_textarea(style: &Style) -> bool {
    marker(style, w3cos_dom::user_agent::TEXTAREA_PROPERTY).is_some()
}

/// The height of one textarea row: the used `line-height`, or the
/// primary font's line spacing for `normal`.
fn textarea_line_height(style: &Style) -> f32 {
    if style.line_height_is_normal {
        #[cfg(feature = "skia")]
        return crate::render_skia::html_text_input_content_size(style, 1).1;
        #[cfg(not(feature = "skia"))]
        return crate::layout::inline_style_line_height(style);
    } else {
        crate::layout::inline_style_line_height(style)
    }
}

/// Width of a classic scrollbar (Chrome on Linux: 15px). Scrollbars take
/// no layout space in this engine (`is_scroll_container`), but HTML
/// §15.5.15 counts one into a textarea's intrinsic size.
const TEXTAREA_SCROLLBAR: f32 = 15.0;

/// Intrinsic content size of a textarea, written from HTML §15.5.15: the
/// textarea effective width is `cols` average character widths plus a
/// scrollbar's width, the effective height `rows` lines plus a
/// scrollbar's height. As in Chrome, the vertical scrollbar counts unless
/// `overflow-y` is `hidden` (or `clip`), and the horizontal one only for
/// `overflow-x: scroll`; a line is the used `line-height` (`normal`: the
/// primary font's line spacing). `letter-spacing` does not enter.
#[cfg(feature = "skia")]
fn textarea_intrinsic(style: &Style) -> Option<(f32, f32)> {
    let count = |name: &str| marker(style, name)?.parse::<u32>().ok().filter(|n| *n > 0);
    let cols = count(w3cos_dom::user_agent::TEXTAREA_COLS_PROPERTY)?;
    let rows = count(w3cos_dom::user_agent::TEXTAREA_ROWS_PROPERTY)?;
    let line =
        (!style.line_height_is_normal).then(|| crate::layout::inline_style_line_height(style));
    let (width, height) = crate::render_skia::html_textarea_content_size(style, cols, rows, line);
    let vertical = !matches!(style.resolved_overflow_y(), Overflow::Hidden);
    let horizontal = matches!(style.resolved_overflow_x(), Overflow::Scroll);
    Some((
        width + if vertical { TEXTAREA_SCROLLBAR } else { 0.0 },
        height + if horizontal { TEXTAREA_SCROLLBAR } else { 0.0 },
    ))
}

/// Content-box size of a text field: a specified size (border-box sizing
/// removes the border and padding), else the intrinsic size, then min/max
/// with the min winning (CSS 2.1 10.4 / 10.7). The gate leaves no
/// percentage, so the containing block does not enter.
fn text_field_content_size(style: &Style, space: &ConstraintSpace) -> Option<(f32, f32)> {
    let (natural_w, natural_h) = text_field_intrinsic(style).why("text-input:size")?;
    let [bt, br, bb, bl] = border(style);
    let pad =
        |s: Spacing| resolve_spacing_for_layout(s, 0.0, style, space.viewport_w, space.viewport_h);
    let edges_w = bl + br + pad(style.padding.left) + pad(style.padding.right);
    let edges_h = bt + bb + pad(style.padding.top) + pad(style.padding.bottom);
    let axis = |dim: Dimension, edges: f32| {
        resolve_dim(dim, None, style, space).map(|v| match style.box_sizing {
            BoxSizing::BorderBox => (v - edges).max(0.0),
            BoxSizing::ContentBox => v,
        })
    };
    let used = |size: Dimension, min: Dimension, max: Dimension, natural: f32, edges: f32| {
        let mut v = axis(size, edges).unwrap_or(natural);
        if let Some(max) = axis(max, edges) {
            v = v.min(max);
        }
        if let Some(min) = axis(min, edges) {
            v = v.max(min);
        }
        v
    };
    Some((
        used(
            style.width,
            style.min_width,
            style.max_width,
            natural_w,
            edges_w,
        ),
        used(
            style.height,
            style.min_height,
            style.max_height,
            natural_h,
            edges_h,
        ),
    ))
}

/// Baseline of a text field from its content-box top: the inner editor's
/// line is centered in the content box (Blink centers a single-line text
/// field's inner editor block; checked in Chrome: a 50px-tall field moves
/// its baseline down by (50 - 15) / 2), then the font baseline in that
/// line.
fn text_field_baseline(style: &Style, content_height: f32) -> Option<f32> {
    let (_, line) = text_field_intrinsic(style)?;
    Some(
        layout_unit((content_height - line) / 2.0)
            + crate::layout::inline_font_baseline_from_line_top(style, line),
    )
}

/// Content-box size of a canvas. `None` when a percentage has no basis
/// or `box-sizing` is `border-box` (that used size stays on legacy).
fn canvas_content_size(
    attr_width: u32,
    attr_height: u32,
    style: &Style,
    space: &ConstraintSpace,
) -> Option<(f32, f32)> {
    replaced_attr_content_size(
        attr_width as f32,
        attr_height as f32,
        style,
        space,
        canvas_used_size,
    )
}

fn replaced_attr_content_size(
    attr_width: f32,
    attr_height: f32,
    style: &Style,
    space: &ConstraintSpace,
    used_size: fn(f32, f32, &Style) -> (f32, f32),
) -> Option<(f32, f32)> {
    if style.box_sizing == BoxSizing::BorderBox {
        return super::bail("canvas:size");
    }
    let unresolved = |dim: Dimension| matches!(dim, Dimension::Ch(_));
    if unresolved(style.width)
        || unresolved(style.height)
        || unresolved(style.min_width)
        || unresolved(style.max_width)
        || unresolved(style.min_height)
        || unresolved(style.max_height)
    {
        return super::bail("canvas:size");
    }
    let sub = |dim: Dimension, basis: Option<f32>| -> Option<Dimension> {
        match dim {
            Dimension::Percent(_)
            | Dimension::Calc { .. }
            | Dimension::Vw(_)
            | Dimension::Vh(_) => resolve_dim(dim, basis, style, space).map(Dimension::Px),
            other => Some(other),
        }
    };
    let mut definite = style.clone();
    definite.width = sub(style.width, Some(space.available_inline))?;
    definite.height = sub(used_height(style, space), space.percentage_block)?;
    definite.min_width = sub(style.min_width, Some(space.available_inline))?;
    definite.max_width = sub(style.max_width, Some(space.available_inline))?;
    definite.min_height = sub(style.min_height, space.percentage_block)?;
    definite.max_height = sub(style.max_height, space.percentage_block)?;
    Some(used_size(attr_width, attr_height, &definite))
}

fn canvas_axis_px(dim: Dimension, style: &Style) -> Option<f32> {
    match dim {
        Dimension::Px(value) => Some(value),
        Dimension::Em(value) => Some(value * style.font_size),
        Dimension::Rem(value) => Some(value * crate::layout::ROOT_FONT_SIZE),
        _ => None,
    }
}

/// CSS 2.1 10.3.2 / 10.4 / 10.6.2 for a canvas with a known intrinsic
/// size. A positive `--w3cos-internal-aspect-ratio` replaces the
/// attribute ratio (width / height).
fn canvas_used_size(attr_width: f32, attr_height: f32, style: &Style) -> (f32, f32) {
    let ratio = style
        .custom_properties
        .as_ref()
        .and_then(|props| props.get("--w3cos-internal-aspect-ratio"))
        .and_then(|raw| raw.parse::<f32>().ok())
        .filter(|ratio| ratio.is_finite() && *ratio > 0.0)
        .or_else(|| (attr_width > 0.0 && attr_height > 0.0).then_some(attr_width / attr_height));
    let width = canvas_axis_px(style.width, style);
    let height = canvas_axis_px(style.height, style);
    let (mut used_width, mut used_height) = match (width, height, ratio) {
        (Some(width), Some(height), _) => (width, height),
        (Some(width), None, Some(ratio)) if ratio > 0.0 => (width, width / ratio),
        (None, Some(height), Some(ratio)) if ratio > 0.0 => (height * ratio, height),
        (Some(width), None, _) => (width, attr_height),
        (None, Some(height), _) => (attr_width, height),
        (None, None, _) => (attr_width, attr_height),
    };
    let min_width = canvas_axis_px(style.min_width, style).map(|value| value.max(0.0));
    let min_height = canvas_axis_px(style.min_height, style).map(|value| value.max(0.0));
    let max_width = canvas_axis_px(style.max_width, style).map(|value| value.max(0.0));
    let max_height = canvas_axis_px(style.max_height, style).map(|value| value.max(0.0));
    if width.is_none()
        && height.is_none()
        && let Some(ratio) = ratio
        && ratio > 0.0
        && used_width > 0.0
        && used_height > 0.0
    {
        // f64: 100 * (60 / 100) in f32 is 60.000004, which LayoutUnit then
        // snaps up a 1/64 px step.
        let (w, h) = (f64::from(used_width), f64::from(used_height));
        let mut scale_min = 0.0_f64;
        let mut scale_max = f64::INFINITY;
        if let Some(value) = min_width {
            scale_min = scale_min.max(f64::from(value) / w);
        }
        if let Some(value) = min_height {
            scale_min = scale_min.max(f64::from(value) / h);
        }
        if let Some(value) = max_width {
            scale_max = scale_max.min(f64::from(value) / w);
        }
        if let Some(value) = max_height {
            scale_max = scale_max.min(f64::from(value) / h);
        }
        let scale = 1.0_f64.max(scale_min).min(scale_max);
        used_width = (w * scale) as f32;
        used_height = (h * scale) as f32;
    } else {
        if let Some(value) = min_width {
            used_width = used_width.max(value);
        }
        if let Some(value) = max_width {
            used_width = used_width.min(value);
        }
        if width.is_some()
            && height.is_none()
            && let Some(ratio) = ratio.filter(|ratio| *ratio > 0.0)
        {
            used_height = used_width / ratio;
        }
        if let Some(value) = min_height {
            used_height = used_height.max(value);
        }
        if let Some(value) = max_height {
            used_height = used_height.min(value);
        }
        if height.is_some()
            && width.is_none()
            && let Some(ratio) = ratio.filter(|ratio| *ratio > 0.0)
        {
            used_width = used_height * ratio;
        }
    }
    (used_width.max(0.0), used_height.max(0.0))
}

/// Either axis is `auto` or `scroll`: the box is a scroll container
/// (css-overflow-3 3.1). Scrollbars take no layout space in this engine
/// (no gutter, as with Blink overlay scrollbars), so until it scrolls its
/// layout is that of an `overflow: hidden` BFC root.
fn is_scroll_container(style: &Style) -> bool {
    let scrollish = |overflow: Overflow| matches!(overflow, Overflow::Scroll | Overflow::Auto);
    style.display.establishes_overflow_clip()
        && (scrollish(style.resolved_overflow_x()) || scrollish(style.resolved_overflow_y()))
}

/// Either axis computes to something other than visible (CSS2 11.1.1).
/// CSS 2.1 10.8.1: a box whose overflow clips (a scroll container) has
/// its bottom margin edge as baseline, instead of a line box's.
pub(super) fn baseline_is_margin_edge(style: &Style) -> bool {
    non_visible_overflow(style) && style.display.establishes_overflow_clip()
}

fn non_visible_overflow(style: &Style) -> bool {
    style.resolved_overflow_x() != Overflow::Visible
        || style.resolved_overflow_y() != Overflow::Visible
}

/// `hidden`, `scroll` or `auto` on a box that clips: a scroll container in
/// css-overflow-3 terms (ComputedStyle::IsScrollContainer). This engine has
/// no `overflow: clip`, so every non-visible value counts.
pub(super) fn is_overflow_scroll_container(style: &Style) -> bool {
    non_visible_overflow(style) && style.display.establishes_overflow_clip()
}

/// A scroll container whose overflow in one axis is itself scrollable
/// (`hidden`, `scroll` or `auto`). `overflow: clip` is stored as `visible`,
/// so a box that only clips that axis does not count (CSS Grid §6.6:
/// "a scrollable overflow value in that axis"). Horizontal writing mode.
pub(super) fn scrolls_in_axis(style: &Style, inline: bool) -> bool {
    let overflow = if inline {
        style.resolved_overflow_x()
    } else {
        style.resolved_overflow_y()
    };
    overflow != Overflow::Visible && style.display.establishes_overflow_clip()
}

/// Like `scrolls_in_axis`, but a `visible` axis beside a scrollable one
/// counts (it computes to `auto`, css-overflow-3 §3.1); flex items use it.
pub(super) fn computed_scrolls_in_axis(style: &Style, inline: bool) -> bool {
    let (this, other, clipped) = if inline {
        (
            style.resolved_overflow_x(),
            style.resolved_overflow_y(),
            style.overflow_clip_x,
        )
    } else {
        (
            style.resolved_overflow_y(),
            style.resolved_overflow_x(),
            style.overflow_clip_y,
        )
    };
    // A `visible` axis next to a scrollable one computes to `auto`
    // (css-overflow-3 §3.1), but an explicit `clip` stays `clip`.
    (this != Overflow::Visible || (other != Overflow::Visible && !clipped))
        && style.display.establishes_overflow_clip()
}

/// Overflow shapes that still disagree with the legacy engine: a flex or
/// grid item (automatic minimum), a negative margin, or a float/clear inside
/// the box. Scrollports (`hidden`, `scroll`, `auto`) are laid out here as BFC
/// roots without scrollbar space; the scroll containers themselves are
/// derived from the result by `layout::scroll_containers_from_layout`.
fn overflow_reason(flat: &[FlatNodeInfo], index: usize) -> Option<&'static str> {
    let style = flat[index].style;
    if !non_visible_overflow(style) || !style.display.establishes_overflow_clip() {
        return None;
    }
    // A flex or grid item that is a scroll container has no automatic
    // minimum: flex::resolve_main_limits and grid::inline_minimum.
    let negative = |spacing: Spacing| match spacing {
        Spacing::Px(v)
        | Spacing::Percent(v)
        | Spacing::Rem(v)
        | Spacing::Em(v)
        | Spacing::Ch(v)
        | Spacing::Vw(v)
        | Spacing::Vh(v) => v < 0.0,
        _ => false,
    };
    if negative(style.margin.top)
        || negative(style.margin.right)
        || negative(style.margin.bottom)
        || negative(style.margin.left)
    {
        return Some("overflow:negative-margin");
    }
    // A float alone inside the box matches legacy (the BFC contains it).
    // A float together with in-flow content, or a `clear`, still takes the
    // legacy adjoining-float path.
    let mut saw_float = false;
    let mut saw_inflow = false;
    let mut saw_clear = false;
    for node in &flat[index + 1..flat[index].subtree_end] {
        if node.style.display == Display::None || oof::is_out_of_flow(node.style) {
            continue;
        }
        // The DOM's synthetic float rows are transparent (their floats
        // belong to this BFC); they are not in-flow content themselves.
        if is_float_group(node.style) && node.style.clear == Clear::None {
            continue;
        }
        if node.style.clear != Clear::None {
            saw_clear = true;
        }
        if node.style.float != Float::None {
            saw_float = true;
        } else {
            saw_inflow = true;
        }
    }
    if saw_clear || (saw_float && saw_inflow) {
        return Some("overflow:float");
    }
    None
}

/// CSS 2.1 10.3.2 / 10.3.4 content-box size. Percent, vw, and vh are
/// resolved against this constraint space; em/rem/ch and the intrinsic
/// ratio stay in `image_intrinsic_size` so the two engines share one
/// replaced-size function. `None` when a percentage has no basis.
fn replaced_content_size(src: &str, style: &Style, space: &ConstraintSpace) -> Option<(f32, f32)> {
    if style.box_sizing == BoxSizing::BorderBox {
        // Border-box lengths are reduced by border and padding to content
        // lengths (never below zero); the size then resolves as for a
        // content box. A length with no basis stays unresolved and bails.
        if marker(style, "--w3cos-internal-aspect-ratio").is_some() {
            return super::bail("image");
        }
        let spacing = |s: Spacing| {
            resolve_spacing_for_layout(
                s,
                space.available_inline,
                style,
                space.viewport_w,
                space.viewport_h,
            )
        };
        let b = border(style);
        let inline_edges = b[1] + b[3] + spacing(style.padding.left) + spacing(style.padding.right);
        let block_edges = b[0] + b[2] + spacing(style.padding.top) + spacing(style.padding.bottom);
        let reduce = |dim: Dimension, basis: Option<f32>, edges: f32| -> Option<Dimension> {
            match dim {
                Dimension::Auto => Some(Dimension::Auto),
                other => resolve_dim(other, basis, style, space)
                    .map(|size| Dimension::Px((size - edges).max(0.0))),
            }
        };
        let mut content = style.clone();
        content.box_sizing = BoxSizing::ContentBox;
        content.width = reduce(style.width, Some(space.available_inline), inline_edges)?;
        content.height = reduce(
            used_height(style, space),
            space.percentage_block,
            block_edges,
        )?;
        content.min_width = reduce(style.min_width, Some(space.available_inline), inline_edges)?;
        content.max_width = reduce(style.max_width, Some(space.available_inline), inline_edges)?;
        content.min_height = reduce(style.min_height, space.percentage_block, block_edges)?;
        content.max_height = reduce(style.max_height, space.percentage_block, block_edges)?;
        return replaced_content_size(src, &content, space);
    }
    let mut definite = style.clone();
    let sub = |dim: Dimension, basis: Option<f32>| -> Option<Dimension> {
        match dim {
            Dimension::Percent(_)
            | Dimension::Calc { .. }
            | Dimension::Vw(_)
            | Dimension::Vh(_) => resolve_dim(dim, basis, style, space).map(Dimension::Px),
            other => Some(other),
        }
    };
    definite.width = sub(style.width, Some(space.available_inline))?;
    definite.height = sub(used_height(style, space), space.percentage_block)?;
    // Every min/max length (em, rem, ch, %, vw, vh) resolves here.
    let limit = |dim: Dimension, basis: Option<f32>| -> Option<Option<f32>> {
        match dim {
            Dimension::Auto => Some(None),
            other => resolve_dim(other, basis, style, space).map(Some),
        }
    };
    let limits = [
        limit(style.min_width, Some(space.available_inline))?,
        limit(style.max_width, Some(space.available_inline))?,
        limit(style.min_height, space.percentage_block)?,
        limit(style.max_height, space.percentage_block)?,
    ];
    // Legacy passes the content-box available width only for a block
    // replaced element with an auto width (the SVG ratio branch).
    let containing = if style.display == Display::Block && style.width == Dimension::Auto {
        let spacing = |s: Spacing| {
            resolve_spacing_for_layout(
                s,
                space.available_inline,
                style,
                space.viewport_w,
                space.viewport_h,
            )
        };
        let b = border(style);
        let edges = b[1] + b[3] + spacing(style.padding.left) + spacing(style.padding.right);
        Some((space.available_inline - edges).max(0.0))
    } else {
        Some(space.available_inline)
    };
    if limits.iter().all(Option::is_none) {
        return Some(crate::layout::image_intrinsic_size(
            src, &definite, containing,
        ));
    }
    Some(constrained_replaced_size(
        src, &definite, containing, limits,
    ))
}

/// Used content-box size of a replaced item (`None`: not a replaced item;
/// `Some(None)`: the size is not known here).
pub(super) fn replaced_item_size(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
) -> Option<Option<(f32, f32)>> {
    let ComponentKind::Image { src } = flat[index].kind else {
        return None;
    };
    Some(replaced_content_size(src, flat[index].style, space))
}

/// Natural content size of a replaced item with every authored size and
/// min/max dropped (its intrinsic size and ratio).
pub(super) fn replaced_intrinsic_size(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
) -> Option<(f32, f32)> {
    let ComponentKind::Image { src } = flat[index].kind else {
        return None;
    };
    let mut natural = flat[index].style.clone();
    natural.width = Dimension::Auto;
    natural.height = Dimension::Auto;
    natural.min_width = Dimension::Auto;
    natural.max_width = Dimension::Auto;
    natural.min_height = Dimension::Auto;
    natural.max_height = Dimension::Auto;
    replaced_content_size(src, &natural, space)
}

/// Inline size of a replaced box whose block size is `height` (a flex
/// item's definite stretched cross size): the height transfers through the
/// aspect ratio, then the inline min/max clamp it. `None` for a box that is
/// not replaced.
pub(super) fn replaced_inline_at_block(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    height: f32,
) -> Option<f32> {
    replaced_inline_at(flat, index, space, height, false)
}

/// [`replaced_inline_at_block`] with the box's own `width` read as auto:
/// its min-content inline size at that block size, which is a flex item's
/// content size suggestion (flex-minimum-width-flex-items-013).
pub(super) fn replaced_content_inline_at_block(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    height: f32,
) -> Option<f32> {
    replaced_inline_at(flat, index, space, height, true)
}

fn replaced_inline_at(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    height: f32,
    auto_width: bool,
) -> Option<f32> {
    if !is_replaced(flat[index].kind) {
        return None;
    }
    let mut definite = flat[index].style.clone();
    if auto_width {
        definite.width = Dimension::Auto;
    }
    definite.height = Dimension::Px(height);
    override_replaced_axis(&mut definite, false);
    definite.min_height = Dimension::Auto;
    definite.max_height = Dimension::Auto;
    replaced_size(flat[index].kind, &definite, space).map(|(width, _)| width)
}

/// An `<img>` or `<canvas>` (a Blink LayoutReplaced), or a text field:
/// a leaf whose content box has an intrinsic size (`replaced_size`). A
/// text field has no aspect ratio, so no size transfers through it.
pub(super) fn is_replaced(kind: &ComponentKind) -> bool {
    matches!(
        kind,
        ComponentKind::Image { .. }
            | ComponentKind::Canvas { .. }
            | ComponentKind::TextInput { .. }
            | ComponentKind::SvgDocument { .. }
    )
}

/// Content-box size of a replaced box under `style`; `None` (with the
/// kind's bail reason) when it has no NG size, or for a non-replaced box.
pub(super) fn replaced_size(
    kind: &ComponentKind,
    style: &Style,
    space: &ConstraintSpace,
) -> Option<(f32, f32)> {
    if size_contained(style)
        && matches!(
            kind,
            ComponentKind::Image { .. }
                | ComponentKind::Canvas { .. }
                | ComponentKind::SvgDocument { .. }
        )
    {
        return contained_replaced_size(style);
    }
    match kind {
        ComponentKind::Image { src } => replaced_content_size(src, style, space).why("image"),
        ComponentKind::Canvas { width, height } => {
            canvas_content_size(*width, *height, style, space).why("canvas:size")
        }
        ComponentKind::TextInput { .. } => text_field_content_size(style, space),
        ComponentKind::SvgDocument { source, .. } => {
            svg_content_size(source, style, space).why("svg:size")
        }
        _ => None,
    }
}

/// A size-contained replaced box has no natural size or aspect ratio: an
/// auto axis is its `contain-intrinsic-size` (zero when `none`), and a
/// length is used as given. Percentages, `calc()`, limits and keywords
/// stay with legacy (`None`).
fn contained_replaced_size(style: &Style) -> Option<(f32, f32)> {
    let (intrinsic_w, intrinsic_h) = contain_intrinsic_size(style);
    let axis = |dim: Dimension, intrinsic: f32| match dim {
        Dimension::Auto => Some(intrinsic),
        Dimension::Px(v) if style.box_sizing != BoxSizing::BorderBox => Some(v),
        _ => None,
    };
    let limited = [
        style.min_width,
        style.max_width,
        style.min_height,
        style.max_height,
    ]
    .iter()
    .any(|d| *d != Dimension::Auto);
    if limited
        || marker(style, WIDTH_KEYWORD).is_some()
        || marker(style, MIN_WIDTH_KEYWORD).is_some()
        || marker(style, MAX_WIDTH_KEYWORD).is_some()
    {
        return super::bail("replaced:contain-size-limits");
    }
    Some((
        axis(style.width, intrinsic_w).or_else(|| super::bail("replaced:contain-size-unit"))?,
        axis(style.height, intrinsic_h).or_else(|| super::bail("replaced:contain-size-unit"))?,
    ))
}

/// A replaced box's content size with intrinsic `min-*` / `max-*`
/// keywords applied. In either axis min-content, max-content and
/// fit-content all mean the size it would have with that axis auto (its
/// natural size, or the other axis transferred through its ratio;
/// css-sizing-3 for replaced elements).
pub(super) fn replaced_size_with_keyword_limits(
    kind: &ComponentKind,
    style: &Style,
    space: &ConstraintSpace,
) -> Option<(f32, f32)> {
    let (mut width, height) = replaced_size(kind, style, space)?;
    let has = |key| marker(style, key).is_some();
    let (min_w, max_w) = (has(MIN_WIDTH_KEYWORD), has(MAX_WIDTH_KEYWORD));
    let (min_h, max_h) = (has(MIN_HEIGHT_KEYWORD), has(MAX_HEIGHT_KEYWORD));
    if !(min_w || max_w || min_h || max_h) {
        return Some((width, height));
    }
    if min_w || max_w {
        let mut auto = style.clone();
        auto.width = Dimension::Auto;
        override_replaced_axis(&mut auto, true);
        let content = replaced_size(kind, &auto, space)?.0;
        let limited = limit_by_content(width, content, min_w, max_w);
        if (limited - width).abs() > 1.0 / 128.0 && style.height == Dimension::Auto {
            // The height would follow the new width through the ratio.
            return super::bail("replaced:width-keyword");
        }
        width = limited;
    }
    if !(min_h || max_h) {
        return Some((width, height));
    }
    let mut auto = style.clone();
    auto.height = Dimension::Auto;
    override_replaced_axis(&mut auto, false);
    let content = replaced_size(kind, &auto, space)?.1;
    Some((width, limit_by_content(height, content, min_h, max_h)))
}

/// `size` limited by intrinsic min / max keywords that both resolve to
/// `content` (max first, then min).
fn limit_by_content(size: f32, content: f32, min: bool, max: bool) -> f32 {
    let size = if max { size.min(content) } else { size };
    if min { size.max(content) } else { size }
}

const SVG_WIDTH_MARKER: &str = "--w3cos-internal-svg-width";
const SVG_HEIGHT_MARKER: &str = "--w3cos-internal-svg-height";
/// The DOM's used width / height of an svg whose own attribute did not
/// give one: a fallback size, read as auto here.
const SVG_FALLBACK_WIDTH_MARKER: &str = "--w3cos-internal-svg-fallback-width";
const SVG_FALLBACK_HEIGHT_MARKER: &str = "--w3cos-internal-svg-fallback-height";
const SVG_MARKERS: [&str; 4] = [
    SVG_WIDTH_MARKER,
    SVG_HEIGHT_MARKER,
    SVG_FALLBACK_WIDTH_MARKER,
    SVG_FALLBACK_HEIGHT_MARKER,
];

/// `Some(Auto)` when the DOM put a fallback size on that axis of an svg
/// (its used width/height is then not a specified size).
fn svg_specified(style: &Style, width: bool) -> Option<Dimension> {
    let marker = if width {
        SVG_FALLBACK_WIDTH_MARKER
    } else {
        SVG_FALLBACK_HEIGHT_MARKER
    };
    style
        .custom_properties
        .as_ref()
        .is_some_and(|props| props.get(marker).is_some())
        .then_some(Dimension::Auto)
}

/// A layout override of an svg's width (or height) is a specified size:
/// the DOM fallback no longer applies on that axis.
fn override_replaced_axis(style: &mut Style, width: bool) {
    if let Some(props) = style.custom_properties.as_mut() {
        props.remove(if width {
            SVG_FALLBACK_WIDTH_MARKER
        } else {
            SVG_FALLBACK_HEIGHT_MARKER
        });
    }
}

/// Sizing attributes of an outer `<svg>` (Blink LayoutSVGRoot).
#[derive(Clone, Copy, Debug, PartialEq)]
struct SvgRootSizing {
    /// The width / height attributes as presentation sizes (Blink
    /// SVGSVGElement::CollectStyleForPresentationAttribute maps them on
    /// the outermost svg): `Auto` when absent.
    width: Dimension,
    height: Dimension,
    /// Natural sizes: the absolute (non-percentage) attributes.
    natural_width: Option<f32>,
    natural_height: Option<f32>,
    /// width:height when both natural sizes are non-empty, else the
    /// viewBox size.
    ratio: Option<f32>,
}

/// An svg length attribute as a CSS length. `Err` for a value whose
/// unit this stage does not resolve (ex, ch, viewport units, calc, a
/// negative or malformed length); an absent or empty one is auto.
fn svg_length_attribute(value: &str, style: &Style) -> Result<Dimension, ()> {
    let value = value.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("auto") {
        return Ok(Dimension::Auto);
    }
    let parsed = if let Some(percent) = value.strip_suffix('%') {
        percent.trim().parse::<f32>().ok().map(Dimension::Percent)
    } else if let Some(rem) = value.strip_suffix("rem") {
        rem.trim()
            .parse::<f32>()
            .ok()
            .map(|v| Dimension::Px(v * crate::layout::ROOT_FONT_SIZE))
    } else if let Some(em) = value.strip_suffix("em") {
        em.trim()
            .parse::<f32>()
            .ok()
            .map(|v| Dimension::Px(v * style.font_size))
    } else {
        w3cos_std::style::parse_absolute_length_px(value).map(Dimension::Px)
    };
    match parsed {
        Some(Dimension::Px(v)) | Some(Dimension::Percent(v)) if v.is_finite() && v >= 0.0 => {
            Ok(parsed.unwrap())
        }
        _ => Err(()),
    }
}

/// The outer svg element's sizing attributes from the serialized source.
/// `None` for a length this stage does not resolve.
fn svg_root_sizing(source: &str, style: &Style) -> Option<SvgRootSizing> {
    let mut reader = quick_xml::Reader::from_reader(source.as_bytes());
    let mut sizing = SvgRootSizing {
        width: Dimension::Auto,
        height: Dimension::Auto,
        natural_width: None,
        natural_height: None,
        ratio: None,
    };
    // The DOM keeps the written width / height as markers; the source has
    // a rasterization size in place of an absent one.
    let props = style.custom_properties.as_ref();
    for (property, slot) in [
        (SVG_WIDTH_MARKER, &mut sizing.width),
        (SVG_HEIGHT_MARKER, &mut sizing.height),
    ] {
        if let Some(value) = props.and_then(|props| props.get(property)) {
            *slot = svg_length_attribute(value, style).ok()?;
        }
    }
    let mut view_box = None;
    loop {
        match reader.read_event() {
            Ok(quick_xml::events::Event::Start(element))
            | Ok(quick_xml::events::Event::Empty(element))
                if element.local_name().as_ref() == b"svg" =>
            {
                for attribute in element.attributes().flatten() {
                    let Ok(name) = std::str::from_utf8(attribute.key.as_ref()) else {
                        continue;
                    };
                    let Ok(value) = attribute.decode_and_unescape_value(reader.decoder()) else {
                        continue;
                    };
                    if name.eq_ignore_ascii_case("viewbox") {
                        // An invalid viewBox is ignored (SVG 2 §8.2).
                        let values = value
                            .split(|c: char| c.is_ascii_whitespace() || c == ',')
                            .filter(|v| !v.is_empty())
                            .map(str::parse::<f32>)
                            .collect::<Result<Vec<_>, _>>()
                            .unwrap_or_default();
                        if values.len() == 4 && values[2] > 0.0 && values[3] > 0.0 {
                            view_box = Some(values[2] / values[3]);
                        }
                    }
                }
                break;
            }
            Ok(quick_xml::events::Event::Eof) | Err(_) => return None,
            _ => {}
        }
    }
    let natural = |dim: Dimension| match dim {
        Dimension::Px(v) => Some(v),
        _ => None,
    };
    sizing.natural_width = natural(sizing.width);
    sizing.natural_height = natural(sizing.height);
    sizing.ratio = match (sizing.natural_width, sizing.natural_height) {
        (Some(w), Some(h)) if w > 0.0 && h > 0.0 => Some(w / h),
        _ => view_box,
    };
    Some(sizing)
}

/// An outer `<svg>` (Blink LayoutSVGRoot, a LayoutReplaced). Like a
/// canvas, plus: an attribute size in a flex item stays on legacy (the
/// flex algorithm reads only the author width/height, so it would
/// stretch a presentation height), as does a CSS `aspect-ratio`.
fn svg_box_reason(flat: &[FlatNodeInfo], index: usize, source: &str) -> Option<&'static str> {
    let info = &flat[index];
    let style = info.style;
    if style.box_sizing == BoxSizing::BorderBox || size_contained(style) {
        return Some("svg:border-box-or-contain");
    }
    if style
        .custom_properties
        .as_ref()
        .is_some_and(|props| props.get("--w3cos-internal-aspect-ratio").is_some())
    {
        return Some("svg:aspect-ratio");
    }
    if svg_root_sizing(source, style).is_none() {
        return Some("svg:length");
    }
    if style.float != Float::None
        || matches!(
            style.position,
            Position::Absolute | Position::Fixed | Position::Sticky
        )
    {
        return Some("svg:float-or-oof");
    }
    if info
        .parent
        .is_some_and(|parent| is_author_grid(flat[parent].style))
    {
        return Some("svg:flex-grid-item");
    }
    if info
        .parent
        .is_some_and(|parent| is_author_flex(flat[parent].style))
    {
        if svg_specified(style, true).is_some() || svg_specified(style, false).is_some() {
            return Some("svg:flex-fallback-size");
        }
        return unsupported_box_style(
            style,
            &[
                inline::VALIGN_MARKER,
                SVG_MARKERS[0],
                SVG_MARKERS[1],
                SVG_MARKERS[2],
                SVG_MARKERS[3],
            ],
            true,
        );
    }
    if matches!(style.display, Display::Block | Display::FlowRoot)
        && info.parent.is_some_and(|parent| {
            inline::rendered_children(flat, parent).any(|child| {
                child != index
                    && flat[child].style.float != Float::None
                    && !oof::is_out_of_flow(flat[child].style)
            })
        })
    {
        return Some("svg:beside-float");
    }
    match style.display {
        Display::Block | Display::FlowRoot => unsupported_box_style(
            style,
            &[
                inline::VALIGN_MARKER,
                SVG_MARKERS[0],
                SVG_MARKERS[1],
                SVG_MARKERS[2],
                SVG_MARKERS[3],
            ],
            false,
        ),
        // An inline svg in an inline formatting context is an atomic
        // inline (Blink LayoutSVGRoot is a LayoutReplaced).
        Display::Inline | Display::InlineBlock if inline::in_inline_container(flat, index) => {
            unsupported_box_style(
                style,
                &[
                    SVG_MARKERS[0],
                    SVG_MARKERS[1],
                    SVG_MARKERS[2],
                    SVG_MARKERS[3],
                    inline::VALIGN_MARKER,
                    inline::VALIGN_LENGTH_MARKER,
                ],
                true,
            )
            .or_else(|| inline::atomic_inline_reason(style))
        }
        Display::Inline | Display::InlineBlock => Some("svg:inline"),
        _ => Some("svg:display"),
    }
}

/// Content-box size of an outer `<svg>`: the sizing behavior of Blink
/// `LayoutSVGRoot::UnscaledIntrinsicSizingInfo` (checked in Chrome),
/// written from SVG 2 §8.2 and CSS 2.1 10.3.2 / 10.6.2; no code is taken
/// from layout_svg_root.cc (LGPL). The min/max step is
/// `clamp_replaced_size` (Blink ComputeReplacedSizeInternal).
///
/// The width/height attributes are presentation sizes under the author
/// ones. Natural sizes are the absolute attributes; the ratio is theirs,
/// else the viewBox's. With only a ratio the inline size fills the
/// available inline size (less margins, border, padding); with nothing
/// the size is 300x150 (CSS 2.1 10.3.2). A percentage height without a
/// definite basis is auto (CSS 2.1 10.5).
fn svg_content_size(source: &str, style: &Style, space: &ConstraintSpace) -> Option<(f32, f32)> {
    if style.box_sizing == BoxSizing::BorderBox {
        return super::bail("svg:size");
    }
    let sizing = svg_root_sizing(source, style).why("svg:length")?;
    let width = resolve_dim(
        svg_specified(style, true).map_or(style.width, |_| sizing.width),
        Some(space.available_inline),
        style,
        space,
    );
    let height = resolve_dim(
        svg_specified(style, false).map_or(used_height(style, space), |_| sizing.height),
        space.percentage_block,
        style,
        space,
    );
    let limit = |dim: Dimension, basis: Option<f32>| -> Option<Option<f32>> {
        match dim {
            Dimension::Auto => Some(None),
            other => resolve_dim(other, basis, style, space).map(Some),
        }
    };
    let limits = [
        limit(style.min_width, Some(space.available_inline))?,
        limit(style.max_width, Some(space.available_inline))?,
        limit(style.min_height, space.percentage_block)?,
        limit(style.max_height, space.percentage_block)?,
    ];
    let ratio = sizing.ratio;
    let stretch = || {
        let spacing = |s: Spacing| {
            resolve_spacing_for_layout(
                s,
                space.available_inline,
                style,
                space.viewport_w,
                space.viewport_h,
            )
        };
        let b = border(style);
        let edges = b[1]
            + b[3]
            + spacing(style.padding.left)
            + spacing(style.padding.right)
            + spacing(style.margin.left)
            + spacing(style.margin.right);
        (space.available_inline - edges).max(0.0)
    };
    let (natural_w, natural_h) = (sizing.natural_width, sizing.natural_height);
    let unclamped = match (width, height) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (
            w,
            ratio.map_or(natural_h.unwrap_or(150.0), |r| layout_unit(w / r)),
        ),
        (None, Some(h)) => (
            ratio.map_or(natural_w.unwrap_or(300.0), |r| layout_unit(h * r)),
            h,
        ),
        (None, None) => match (natural_w, natural_h, ratio) {
            (Some(w), Some(h), _) => (w, h),
            (Some(w), None, Some(r)) => (w, layout_unit(w / r)),
            (None, Some(h), Some(r)) => (layout_unit(h * r), h),
            (Some(w), None, None) => (w, 150.0),
            (None, Some(h), None) => (300.0, h),
            (None, None, Some(r)) => {
                let w = layout_unit(stretch());
                (w, layout_unit(w / r))
            }
            (None, None, None) => (300.0, 150.0),
        },
    };
    Some(clamp_replaced_size(
        unclamped,
        ratio,
        width.is_some(),
        height.is_some(),
        limits,
    ))
}

/// Replaced size under min/max constraints.
///
/// Follows Blink `ComputeReplacedSizeInternal` (kNormal) in
/// third_party/blink/renderer/core/layout/length_utils.cc:
/// Copyright 2016 The Chromium Authors. Use of this source code is
/// governed by a BSD-style license (Chromium LICENSE file).
///
/// With an aspect ratio the auto axis is transferred from the clamped
/// axis, then clamped on its own axis. A min always wins over a max.
fn constrained_replaced_size(
    src: &str,
    definite: &Style,
    containing: Option<f32>,
    limits: [Option<f32>; 4],
) -> (f32, f32) {
    let mut auto = definite.clone();
    auto.min_width = Dimension::Auto;
    auto.max_width = Dimension::Auto;
    auto.min_height = Dimension::Auto;
    auto.max_height = Dimension::Auto;
    // Used size before min/max, with the specified width/height applied.
    let unclamped = crate::layout::image_intrinsic_size(src, &auto, containing);
    let ratio = crate::layout::image_intrinsic_ratio(src);
    clamp_replaced_size(
        unclamped,
        ratio,
        definite.width != Dimension::Auto,
        definite.height != Dimension::Auto,
        limits,
    )
}

/// The min/max step of `ComputeReplacedSizeInternal` on a used size
/// `(w0, h0)` computed without min/max.
fn clamp_replaced_size(
    (w0, h0): (f32, f32),
    ratio: Option<f32>,
    width_given: bool,
    height_given: bool,
    [min_w, max_w, min_h, max_h]: [Option<f32>; 4],
) -> (f32, f32) {
    let clamp = |v: f32, min: Option<f32>, max: Option<f32>| {
        let min = min.unwrap_or(0.0).max(0.0);
        let max = max.map_or(f32::INFINITY, |m| m.max(min));
        v.min(max).max(min)
    };
    let clamp_i = |v: f32| clamp(v, min_w, max_w);
    let clamp_b = |v: f32| clamp(v, min_h, max_h);
    match ratio.filter(|r| *r > 0.0) {
        Some(ratio) if !(width_given && height_given) => {
            if width_given {
                let w = clamp_i(w0);
                (w, clamp_b(layout_unit(w / ratio)))
            } else if height_given {
                let h = clamp_b(h0);
                (clamp_i(layout_unit(h * ratio)), h)
            } else {
                let h = clamp_b(layout_unit(clamp_i(w0) / ratio));
                (clamp_i(layout_unit(h * ratio)), h)
            }
        }
        _ => (clamp_i(w0), clamp_b(h0)),
    }
}

/// Visit rendered nodes (display:none subtrees emit nothing in either
/// engine) until `f` returns true.
fn walk_rendered(flat: &[FlatNodeInfo], mut f: impl FnMut(usize) -> bool) {
    let mut index = 0;
    while index < flat.len() {
        let info = &flat[index];
        if info.style.display == Display::None {
            index = info.subtree_end.max(index + 1);
            continue;
        }
        if f(index) {
            return;
        }
        index += 1;
    }
}

/// `None` when every rendered node is a box this stage lays out, otherwise
/// the first reason the legacy engine must run.
pub(super) fn unsupported_tree(flat: &[FlatNodeInfo]) -> Option<&'static str> {
    let mut reason = None;
    walk_rendered(flat, |index| {
        reason = node_reason(flat, index);
        reason.is_some()
    });
    reason
}

/// Trace token for one box whose fallback is the unrecognized-display
/// marker. Spaces in the authored value become `+` so the coverage line
/// stays one whitespace field. The context is the box's formatting
/// parent, not a second layout.
fn unrecognized_display_label(flat: &[FlatNodeInfo], index: usize) -> String {
    let value = unrecognized_display(flat[index].style).unwrap_or("");
    let mut token = String::from("unrecognized-display:");
    for ch in value.chars() {
        if ch.is_whitespace() {
            token.push('+');
        } else if ch == ',' || ch == '|' {
            token.push('_');
        } else {
            token.push(ch);
        }
    }
    token.push('|');
    token.push_str(unrecognized_display_context(flat, index));
    token
}

fn unrecognized_display_context(flat: &[FlatNodeInfo], index: usize) -> &'static str {
    let style = flat[index].style;
    if matches!(style.position, Position::Absolute | Position::Fixed) {
        return "out-of-flow";
    }
    if style.float != Float::None {
        return "float";
    }
    if let Some(parent) = flat[index].parent {
        let parent_style = flat[parent].style;
        if is_author_flex(parent_style) {
            return "flex-item";
        }
        if is_author_grid(parent_style) {
            return "grid-item";
        }
        if inline::in_inline_container(flat, index) {
            return "inline";
        }
    }
    "block"
}

/// Every distinct reason in the tree (trace mode only); internal markers
/// are reported by name. `unrecognized-display` includes the authored
/// value and formatting context (`unrecognized-display:<value>|<context>`).
pub(super) fn unsupported_reasons(flat: &[FlatNodeInfo]) -> Vec<String> {
    let mut reasons = Vec::new();
    walk_rendered(flat, |index| {
        let info = &flat[index];
        let Some(reason) = node_reason(flat, index) else {
            return false;
        };
        if reason == "internal-marker" {
            if let Some(properties) = &info.style.custom_properties {
                for key in properties.keys() {
                    if !key.starts_with("--w3cos-internal")
                        || is_value_carrier_marker(key)
                        || key == inline::IFC_MARKER
                        || key == inline::ANONYMOUS_BLOCK_MARKER
                        || key == inline::LINE_WIDTH_MARKER
                    {
                        continue;
                    }
                    let name = if key == "--w3cos-internal-unrecognized-display" {
                        unrecognized_display_label(flat, index)
                    } else {
                        key.trim_start_matches("--w3cos-internal-").to_string()
                    };
                    if !reasons.contains(&name) {
                        reasons.push(name);
                    }
                }
            }
        } else if !reasons.iter().any(|name| name == reason) {
            reasons.push(reason.to_string());
        }
        false
    });
    reasons.sort_unstable();
    reasons
}

#[cfg(test)]
pub(super) fn supports_tree(flat: &[FlatNodeInfo]) -> bool {
    unsupported_tree(flat).is_none()
}

fn border(style: &Style) -> [f32; 4] {
    [
        style.border_top_width.unwrap_or(style.border_width),
        style.border_right_width.unwrap_or(style.border_width),
        style.border_bottom_width.unwrap_or(style.border_width),
        style.border_left_width.unwrap_or(style.border_width),
    ]
}

/// Blink LayoutUnit: used lengths are stored in 1/64 px, truncating.
fn layout_unit(v: f32) -> f32 {
    w3cos_std::style::used_box_spacing(v)
}

pub(super) fn layout_unit_value(v: f32) -> f32 {
    layout_unit(v)
}

pub(super) fn border_widths(style: &Style) -> [f32; 4] {
    border(style)
}

pub(super) fn resolve_length(
    d: Dimension,
    basis: Option<f32>,
    style: &Style,
    space: &ConstraintSpace,
) -> Option<f32> {
    resolve_dim(d, basis, style, space)
}

pub(super) fn clamp_inline_size(
    style: &Style,
    space: &ConstraintSpace,
    edges: f32,
    width: f32,
) -> f32 {
    clamp_inline(style, space, edges, width)
}

/// Used length of a size property (legacy `to_taffy_dim` units).
fn resolve_dim(
    d: Dimension,
    basis: Option<f32>,
    style: &Style,
    space: &ConstraintSpace,
) -> Option<f32> {
    match d {
        Dimension::Auto => None,
        Dimension::Px(v) => Some(v),
        Dimension::Percent(p) => basis.map(|b| layout_unit(b * p / 100.0)),
        Dimension::Rem(v) => Some(v * 16.0),
        Dimension::Em(v) => Some(v * style.font_size),
        Dimension::Ch(v) => Some(v * style.ch_advance()),
        Dimension::Vw(v) => Some(v * space.viewport_w / 100.0),
        Dimension::Vh(v) => Some(v * space.viewport_h / 100.0),
        Dimension::Calc { percent, px } => {
            basis.map(|b| layout_unit(b * percent / 100.0 + px).max(0.0))
        }
    }
}

pub(super) struct BoxMetrics {
    pub margin: [f32; 4],
    pub border: [f32; 4],
    pub padding: [f32; 4],
    pub content_width: f32,
}

/// CSS2 10.3.3 (+10.4) used width and horizontal margins.
/// `aspect-ratio` of a non-replaced box (css-sizing-4; Blink
/// ComputeBlockSizeFromAspectRatio / ComputeInlineSizeFromAspectRatio).
/// The ratio relates the box-sizing boxes, except that `auto && <ratio>`
/// always relates the content boxes of a non-replaced box.
#[derive(Clone, Copy, Debug)]
pub(super) struct BoxAspectRatio {
    ratio: f32,
    content_box: bool,
    inline_edges: f32,
    block_edges: f32,
}

impl BoxAspectRatio {
    pub(super) fn of(style: &Style, metrics: &BoxMetrics) -> Option<Self> {
        let [bt, br, bb, bl] = metrics.border;
        let [pt, pr, pb, pl] = metrics.padding;
        Self::new(style, bl + br + pl + pr, bt + bb + pt + pb)
    }

    pub(super) fn new(style: &Style, inline_edges: f32, block_edges: f32) -> Option<Self> {
        let ratio = marker(style, "--w3cos-internal-aspect-ratio")?
            .parse::<f32>()
            .ok()
            .filter(|ratio| ratio.is_finite() && *ratio > 0.0)?;
        Some(Self {
            ratio,
            content_box: marker(style, "--w3cos-internal-aspect-ratio-auto").is_some()
                || style.box_sizing == BoxSizing::ContentBox,
            inline_edges,
            block_edges,
        })
    }

    /// Content block size for a content inline size.
    pub(super) fn block_from_inline(self, inline: f32) -> f32 {
        if self.content_box {
            inline / self.ratio
        } else {
            ((inline + self.inline_edges) / self.ratio - self.block_edges).max(0.0)
        }
    }

    /// Content inline size for a content block size.
    pub(super) fn inline_from_block(self, block: f32) -> f32 {
        if self.content_box {
            block * self.ratio
        } else {
            ((block + self.block_edges) * self.ratio - self.inline_edges).max(0.0)
        }
    }
}

pub(super) fn resolve_inline(style: &Style, space: &ConstraintSpace) -> BoxMetrics {
    let cb = space.available_inline;
    // Cyclic inline percentages (an intrinsic probe): zero margins and
    // padding, auto width, no min/max-width.
    let pct_cb = (!space.percent_unresolved).then_some(cb);
    let spacing = |s: Spacing| {
        resolve_spacing_for_layout(
            s,
            pct_cb.unwrap_or(0.0),
            style,
            space.viewport_w,
            space.viewport_h,
        )
    };
    let border = border(style);
    let padding = [
        spacing(style.padding.top),
        spacing(style.padding.right),
        spacing(style.padding.bottom),
        spacing(style.padding.left),
    ];
    let mut margin = [
        spacing(style.margin.top),
        spacing(style.margin.right),
        spacing(style.margin.bottom),
        spacing(style.margin.left),
    ];
    let edges = border[1] + border[3] + padding[1] + padding[3];
    let to_content = |w: f32| match style.box_sizing {
        BoxSizing::BorderBox => (w - edges).max(0.0),
        BoxSizing::ContentBox => w,
    };
    // An intrinsic keyword (only accepted under size containment) is the
    // contain-intrinsic inline size: min-, max- and fit-content coincide.
    let specified = if marker(style, WIDTH_KEYWORD).is_some() && size_contained(style) {
        Some(contain_intrinsic_size(style).0)
    } else {
        resolve_dim(style.width, pct_cb, style, space).map(to_content)
    };
    let mut content_width = specified
        .unwrap_or(cb - margin[1] - margin[3] - edges)
        .max(0.0);
    if let Some(max) = resolve_dim(style.max_width, pct_cb, style, space).map(to_content) {
        content_width = content_width.min(max);
    }
    if let Some(min) = resolve_dim(style.min_width, pct_cb, style, space).map(to_content) {
        content_width = content_width.max(min);
    }
    // Inline-start / inline-end sides in the containing block's direction.
    let (start, end) = if space.rtl { (1, 3) } else { (3, 1) };
    let auto_start = [
        style.margin.top,
        style.margin.right,
        style.margin.bottom,
        style.margin.left,
    ][start]
        == Spacing::Auto;
    let auto_end = [
        style.margin.top,
        style.margin.right,
        style.margin.bottom,
        style.margin.left,
    ][end]
        == Spacing::Auto;
    let width_was_auto =
        specified.is_none() && content_width == (cb - margin[1] - margin[3] - edges).max(0.0);
    let remaining = cb - content_width - edges;
    if !width_was_auto {
        match (auto_start, auto_end) {
            (true, true) => {
                let half = layout_unit((remaining / 2.0).max(0.0));
                margin[3] = half;
                margin[1] = half;
            }
            // Blink ResolveInlineAutoMargins: an auto start margin never
            // goes negative (CSS2 margin-auto-on-block-box, as in Chrome).
            (true, false) => margin[start] = (remaining - margin[end]).max(0.0),
            // Over-constrained: the end margin is ignored (CSS2 10.3.3).
            (false, _) => margin[end] = remaining - margin[start],
        }
    }
    if space.rtl {
        // The box is placed from the right edge; the left margin is the
        // remainder (also when an auto width was floored at 0).
        margin[3] = remaining - margin[1];
    }
    BoxMetrics {
        margin,
        border,
        padding,
        content_width,
    }
}

/// Min-content and max-content inline sizes of a box's content box (Blink
/// NGBlockNode::ComputeMinMaxSizes, CSS Sizing 3 5.1).  `min` is `None`
/// where this stage cannot find the min-content size (break opportunities
/// it does not model); shrink-to-fit then needs the max-content size to fit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct MinMax {
    pub min: Option<f32>,
    pub max: f32,
}

impl MinMax {
    pub fn exact(size: f32) -> Self {
        MinMax {
            min: Some(size),
            max: size,
        }
    }

    /// MinMaxSizes::Encompass.
    pub(super) fn encompass(&mut self, other: MinMax) {
        self.min = self.min.zip(other.min).map(|(a, b)| a.max(b));
        self.max = self.max.max(other.max);
    }

    pub(super) fn map(self, f: impl Fn(f32) -> f32) -> Self {
        MinMax {
            min: self.min.map(&f),
            max: f(self.max),
        }
    }
}

/// Available inline size of a max-content layout (LayoutUnit::Max in
/// Blink's line breaker).
pub(super) const INDEFINITE_INLINE: f32 = 33_554_431.0;

/// Percentages in the inline axis resolve against the containing block,
/// which intrinsic sizing has not sized yet (CSS Sizing 3 5.2.1 cyclic
/// percentages): such subtrees stay on legacy.
fn percent_inline(style: &Style) -> bool {
    let dim = |d: Dimension| matches!(d, Dimension::Percent(_) | Dimension::Calc { .. });
    let spacing = |s: Spacing| {
        matches!(
            s,
            Spacing::Percent(_) | Spacing::Calc { .. } | Spacing::PaddingCalc { .. }
        )
    };
    dim(style.width)
        || dim(style.min_width)
        || dim(style.max_width)
        || spacing(style.margin.left)
        || spacing(style.margin.right)
        || spacing(style.padding.left)
        || spacing(style.padding.right)
}

/// The content-box min/max-content contribution of a block-level replaced
/// child (image, canvas or SVG root) whose only inline percentages are its
/// `width` and/or `max-width`: a compressible replaced element
/// (css-sizing-3) resolves them against zero for its min-content
/// contribution, and treats them as `auto` / `none` for its max-content
/// contribution, which is then its natural size. `Some(None)` when the rule
/// does not apply; `None` is a bail.
fn compressible_replaced_content(
    flat: &[FlatNodeInfo],
    child: usize,
    space: &ConstraintSpace,
) -> Option<Option<MinMax>> {
    let style = flat[child].style;
    let pct = |d: Dimension| matches!(d, Dimension::Percent(_) | Dimension::Calc { .. });
    let spacing = |s: Spacing| {
        matches!(
            s,
            Spacing::Percent(_) | Spacing::Calc { .. } | Spacing::PaddingCalc { .. }
        )
    };
    let applies = is_replaced(flat[child].kind)
        && (pct(style.width) || pct(style.max_width))
        && !pct(style.min_width)
        && !spacing(style.margin.left)
        && !spacing(style.margin.right)
        && !spacing(style.padding.left)
        && !spacing(style.padding.right)
        && marker(style, WIDTH_KEYWORD).is_none()
        && marker(style, MIN_WIDTH_KEYWORD).is_none()
        && marker(style, MAX_WIDTH_KEYWORD).is_none();
    if !applies {
        return Some(None);
    }
    let mut auto = style.clone();
    if pct(auto.width) {
        auto.width = Dimension::Auto;
    }
    if pct(auto.max_width) {
        auto.max_width = Dimension::Auto;
    }
    let (natural, _) = match flat[child].kind {
        ComponentKind::Image { src } => replaced_content_size(src, &auto, space),
        ComponentKind::Canvas { width, height } => {
            canvas_content_size(*width, *height, &auto, space)
        }
        ComponentKind::SvgDocument { source, .. } => svg_content_size(source, &auto, space),
        _ => return Some(None),
    }
    .why("compressible-replaced")?;
    Some(Some(MinMax {
        min: Some(0.0),
        max: natural,
    }))
}

/// [`percent_inline`] for the box `node` generates. Text has no box of its
/// own (its style carries the runtime's leaf width, which never applies),
/// and width, min-width and max-width do not apply to a non-replaced
/// inline box (CSS 2.1 10.3.1), so only its margins and padding count.
fn box_percent_inline(flat: &[FlatNodeInfo], node: usize) -> bool {
    let style = flat[node].style;
    if matches!(flat[node].kind, ComponentKind::Text { .. }) {
        return false;
    }
    if style.display == Display::Inline && !inline::is_atomic_inline(flat, node) {
        let spacing = |s: Spacing| {
            matches!(
                s,
                Spacing::Percent(_) | Spacing::Calc { .. } | Spacing::PaddingCalc { .. }
            )
        };
        return spacing(style.margin.left)
            || spacing(style.margin.right)
            || spacing(style.padding.left)
            || spacing(style.padding.right);
    }
    percent_inline(style)
}

/// Whether a descendant that feeds `index`'s min/max-content sizes has a
/// percentage inline size, margin or padding. Out-of-flow and
/// `display: none` subtrees contribute nothing to intrinsic sizes (CSS
/// Sizing 3), so their percentages are skipped with them.
fn contributing_percent_inline(flat: &[FlatNodeInfo], index: usize) -> bool {
    let end = flat[index].subtree_end.max(index + 1);
    let mut node = index + 1;
    while node < end {
        let info = &flat[node];
        if info.style.display == Display::None || oof::is_out_of_flow(info.style) {
            node = info.subtree_end.max(node + 1);
            continue;
        }
        if box_percent_inline(flat, node) {
            return true;
        }
        node += 1;
    }
    false
}

/// [`contributing_percent_inline`] for a flex container. Its items'
/// percentage margins resolve against zero in the min/max-content
/// contributions (as in Blink's flex ComputeMinMaxSizes) and against the
/// container's inner width in layout, which `min_max_contribution` and
/// `flex::layout` already do, so only those margins are allowed
/// (calc-margins-percent-flex).
fn flex_percent_below(flat: &[FlatNodeInfo], index: usize) -> bool {
    inline::rendered_children(flat, index).any(|child| {
        let style = flat[child].style;
        if oof::is_out_of_flow(style) || matches!(flat[child].kind, ComponentKind::Text { .. }) {
            return false;
        }
        let dim = |d: Dimension| matches!(d, Dimension::Percent(_) | Dimension::Calc { .. });
        let spacing = |s: Spacing| {
            matches!(
                s,
                Spacing::Percent(_) | Spacing::Calc { .. } | Spacing::PaddingCalc { .. }
            )
        };
        dim(style.width)
            || dim(style.min_width)
            || dim(style.max_width)
            || spacing(style.padding.left)
            || spacing(style.padding.right)
            || contributing_percent_inline(flat, child)
    })
}

/// [`contributing_percent_inline`] for an inline formatting context. An
/// atomic inline is laid out at its own shrink-to-fit width, which sizes
/// its contents from their own intrinsic sizes (this function's block,
/// table and inline paths), so percentages inside it resolve against that
/// width as in layout. Only a percentage on an inline-level box itself
/// (the atomic inline, or an inline box's margin and padding) would
/// resolve against the probe widths of [`inline::inline_min_max`].
fn inline_percent_inline(flat: &[FlatNodeInfo], index: usize) -> bool {
    let end = flat[index].subtree_end.max(index + 1);
    let mut node = index + 1;
    while node < end {
        let info = &flat[node];
        let next = info.subtree_end.max(node + 1);
        if info.style.display == Display::None || oof::is_out_of_flow(info.style) {
            node = next;
            continue;
        }
        if box_percent_inline(flat, node) {
            return true;
        }
        node = if inline::is_atomic_inline(flat, node) {
            next
        } else {
            node + 1
        };
    }
    false
}

/// Clamp a content-box inline size by min-width / max-width (CSS2 10.4).
fn clamp_inline(style: &Style, space: &ConstraintSpace, edges: f32, width: f32) -> f32 {
    clamp_inline_in(style, space, edges, width, Some(space.available_inline))
}

/// `clamp_inline` against an explicit containing-block inline size. A
/// `None` basis (an intrinsic-size computation: the containing block is
/// not sized yet) leaves percentage min-width / max-width unresolved, so
/// they clamp nothing (CSS Sizing 3 5.2.1, Blink's indefinite-size
/// ResolveMinInlineLength / ResolveMaxInlineLength).
fn clamp_inline_in(
    style: &Style,
    space: &ConstraintSpace,
    edges: f32,
    mut width: f32,
    cb: Option<f32>,
) -> f32 {
    let to_content = |w: f32| match style.box_sizing {
        BoxSizing::BorderBox => (w - edges).max(0.0),
        BoxSizing::ContentBox => w,
    };
    if let Some(max) = resolve_dim(style.max_width, cb, style, space).map(to_content) {
        width = width.min(max);
    }
    if let Some(min) = resolve_dim(style.min_width, cb, style, space).map(to_content) {
        width = width.max(min);
    }
    width
}

/// NGBlockNode::ComputeMinMaxSizes of `index`'s content box.  Block
/// children contribute their margin box (ComputeMinAndMaxContentContribution:
/// a definite width, else their own sizes, clamped by min/max-width);
/// inline content is measured by the line breaker (NGInlineNode::
/// ComputeMinMaxSizes).
pub(super) fn intrinsic_sizes(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
) -> Option<MinMax> {
    intrinsic_sizes_at(flat, index, space, None)
}

/// [`intrinsic_sizes`] of a box whose content block size is already known
/// (`block`), which a flex row's stretched images transfer through their
/// aspect ratio (CSS Flexbox §9.8).
pub(super) fn intrinsic_sizes_at(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    block: Option<f32>,
) -> Option<MinMax> {
    let info = &flat[index];
    let subtree = &flat[index + 1..info.subtree_end.max(index + 1)];
    let rendered = |node: &&FlatNodeInfo| node.style.display != Display::None;
    // A table resolves percentage columns itself. The caller's border and
    // padding are added back onto this content box.
    if matches!(info.style.display, Display::Table | Display::InlineTable) {
        let border_box =
            super::table::contribution_border_box(flat, index, space).why("table:intrinsic")?;
        let style = info.style;
        let side = |s: Spacing| match s {
            Spacing::Auto => 0.0,
            s => resolve_spacing_for_layout(s, 0.0, style, space.viewport_w, space.viewport_h),
        };
        let edges = if style.border_collapse {
            0.0
        } else {
            let b = border(style);
            b[1] + b[3] + side(style.padding.left) + side(style.padding.right)
        };
        return Some(border_box.map(|width| (width - edges).max(0.0)));
    }
    // Flex, grid and inline content size their items' percentages their own
    // way, so a percentage anywhere below them stays on legacy. The block
    // children below handle theirs (BlockNode::ComputeMinMaxSizes).
    let percent_below = || {
        if is_author_flex(info.style) {
            flex_percent_below(flat, index)
        } else {
            contributing_percent_inline(flat, index)
        }
    };
    if (is_author_flex(info.style) || is_author_grid(info.style)) && percent_below() {
        return super::bail("intrinsic-percent:flex-grid");
    }
    if is_author_flex(info.style) {
        return super::flex::container_min_max_at(flat, index, space, block);
    }
    if is_author_grid(info.style) {
        // A grid's min/max-content size is the sum of its tracks, not the
        // block-flow sum below (Blink GridLayoutAlgorithm::ComputeMinMaxSizes).
        return super::grid::min_max(flat, index, space);
    }
    if let ComponentKind::Text { content } = info.kind {
        return Some(inline::text_min_max(content, info.style));
    }
    if let ComponentKind::Image { src } = info.kind {
        let (width, _) = replaced_content_size(src, info.style, space).why("image-intrinsic")?;
        return Some(MinMax::exact(width));
    }
    if let ComponentKind::Canvas { width, height } = info.kind {
        let (width, _) =
            canvas_content_size(*width, *height, info.style, space).why("canvas:size")?;
        return Some(MinMax::exact(width));
    }
    if let ComponentKind::TextInput { .. } = info.kind {
        let (width, _) = text_field_content_size(info.style, space)?;
        return Some(MinMax::exact(width));
    }
    if let ComponentKind::SvgDocument { source, .. } = info.kind {
        let (width, _) = svg_content_size(source, info.style, space).why("svg:size")?;
        return Some(MinMax::exact(width));
    }
    if matches!(info.kind, ComponentKind::Button { .. })
        && inline::rendered_children(flat, index).next().is_none()
    {
        return Some(match button_leaf_label(flat, index) {
            Some(label) => inline::text_min_max(label, info.style),
            None => MinMax::exact(0.0),
        });
    }
    if size_contained(info.style) {
        // Size containment: intrinsic sizes ignore the contents.
        return Some(MinMax::exact(contain_intrinsic_size(info.style).0));
    }
    // BlockNode::ComputeMinMaxSizes passes its children the percentage
    // resolution block size of this box (Blink
    // CalculateChildPercentageBlockSizeForMinMax): its definite content
    // height, or none. Anonymous blocks are skipped (CSS 2.1 10.5). A
    // block size the caller already knows (an out-of-flow box's specified
    // or inset-stretched height) is that definite content height.
    let child_space = ConstraintSpace {
        percentage_block: block.or_else(|| child_percentage_block(info.style, space)),
        ..*space
    };
    let space = &child_space;
    if inline::is_inline_container(flat, index) {
        // Floats among inline content would take the float_left /
        // float_right inline sizes into InlineNode::ComputeMinMaxSizes.
        if subtree
            .iter()
            .filter(rendered)
            .any(|node| node.style.float != Float::None && !oof::is_out_of_flow(node.style))
        {
            return super::bail("intrinsic-inline-float");
        }
        return inline::inline_min_max(flat, index, space);
    }
    // BlockLayoutAlgorithm::ComputeMinMaxSizes: floats and new formatting
    // contexts sit on one conceptual "line" (the float inline sizes add up
    // until clearance or an in-flow block starts a new one); the min-content
    // contribution assumes every float on its own line.
    let mut sizes = MinMax::exact(0.0);
    let (mut float_left, mut float_right) = (0.0f32, 0.0f32);
    for child in inline::rendered_children(flat, index) {
        let style = flat[child].style;
        // Out-of-flow children contribute nothing (CSS Sizing 3 5.1).
        if oof::is_out_of_flow(style) {
            continue;
        }
        let floating = style.float != Float::None;
        let new_fc = !floating && is_bfc_root(style);
        if floating || new_fc {
            if style.clear != Clear::None {
                sizes.max = sizes.max.max(float_left + float_right);
            }
            if matches!(style.clear, Clear::Both | Clear::Left) {
                float_left = 0.0;
            }
            if matches!(style.clear, Clear::Both | Clear::Right) {
                float_right = 0.0;
            }
        }
        // A replaced child sizes a percentage against its own containing
        // block, and its min-content contribution would drop to zero
        // (compressible replaced elements); not modelled yet. A table's
        // percentage is resolved afterwards (percent_unresolved).
        let plain = matches!(
            flat[child].kind,
            ComponentKind::Box | ComponentKind::Column | ComponentKind::Row
        );
        let compressible = compressible_replaced_content(flat, child, space)?;
        if !plain && compressible.is_none() && box_percent_inline(flat, child) {
            return super::bail("intrinsic-percent:replaced");
        }
        // Percentages have nothing to resolve against here (Blink resolves
        // percentage margins and padding against zero for min/max sizes):
        // a percentage width acts as auto, percentage min/max-width as
        // none, and percentage margins and padding as zero.
        let side = |s: Spacing| match s {
            Spacing::Auto => 0.0,
            s => resolve_spacing_for_layout(s, 0.0, style, space.viewport_w, space.viewport_h),
        };
        let b = border(style);
        let edges = b[1] + b[3] + side(style.padding.left) + side(style.padding.right);
        let (margin_left, margin_right) = (side(style.margin.left), side(style.margin.right));
        let content = match compressible {
            Some(content) => content,
            None => match resolve_dim(style.width, None, style, space) {
                Some(w) => MinMax::exact(match style.box_sizing {
                    BoxSizing::BorderBox => (w - edges).max(0.0),
                    BoxSizing::ContentBox => w,
                }),
                None => {
                    let sizes = intrinsic_sizes(flat, child, space).why("intrinsic")?;
                    // An aspect-ratio box with a definite height contributes
                    // its transferred width (aspect-ratio/intrinsic-size-001).
                    let block_edges =
                        b[0] + b[2] + side(style.padding.top) + side(style.padding.bottom);
                    match ratio_inline_contribution(flat, child, space, edges, block_edges, sizes)?
                    {
                        Some(inline) => MinMax::exact(inline),
                        None => keyword_width_contribution(flat, child, space, edges, sizes)?,
                    }
                }
            },
        };
        let limits = keyword_contribution_limits(flat, child, space, edges)?;
        let border_box = apply_contribution_limits(content, limits, |w| {
            clamp_inline_in(style, space, edges, w, None)
        })
        .map(|w| w + edges);
        let max_contribution = if floating {
            // A float outside the content area (negative margins) adds
            // nothing.
            let float_size = border_box.max + margin_left + margin_right;
            if float_size > 0.0 {
                if style.float == Float::Right {
                    float_right += float_size;
                } else {
                    float_left += float_size;
                }
            }
            float_left + float_right
        } else if new_fc {
            let inset = |floats: f32, margin: f32| {
                if margin > 0.0 {
                    floats.max(margin)
                } else {
                    floats + margin
                }
            };
            border_box.max + inset(float_left, margin_left) + inset(float_right, margin_right)
        } else {
            border_box.max + margin_left + margin_right
        };
        sizes.max = sizes.max.max(max_contribution);
        sizes.min = sizes
            .min
            .zip(border_box.min)
            .map(|(a, b)| a.max(b + margin_left + margin_right));
        if !floating {
            float_left = 0.0;
            float_right = 0.0;
        }
    }
    Some(sizes)
}

/// Percentage basis that a box gives its in-flow children: its definite
/// content height after min/max (CSS 2.1 10.5), or `None`.
fn child_percentage_block(style: &Style, space: &ConstraintSpace) -> Option<f32> {
    let anonymous_block = style
        .custom_properties
        .as_ref()
        .is_some_and(|props| props.contains_key(inline::ANONYMOUS_BLOCK_MARKER));
    if anonymous_block {
        return space.percentage_block;
    }
    let edges = match style.box_sizing {
        BoxSizing::BorderBox => {
            let b = border(style);
            let side = |s: Spacing| match s {
                Spacing::Auto => 0.0,
                s => resolve_spacing_for_layout(
                    s,
                    space.available_inline,
                    style,
                    space.viewport_w,
                    space.viewport_h,
                ),
            };
            b[0] + b[2] + side(style.padding.top) + side(style.padding.bottom)
        }
        BoxSizing::ContentBox => 0.0,
    };
    let content = |dim: Dimension| {
        resolve_dim(dim, space.percentage_block, style, space).map(|h| (h - edges).max(0.0))
    };
    let mut height = content(used_height(style, space))?;
    if let Some(max) = content(style.max_height) {
        height = height.min(max);
    }
    if let Some(min) = content(style.min_height) {
        height = height.max(min);
    }
    Some(height)
}

/// An auto inline size transferred from a definite block size through the
/// aspect ratio of a non-replaced box without a width keyword, floored by
/// the automatic minimum (min-content) of a non-scroll container.
/// `Some(None)` when there is no such ratio or block size.
fn ratio_inline_contribution(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    edges: f32,
    block_edges: f32,
    sizes: MinMax,
) -> Option<Option<f32>> {
    if marker(flat[index].style, WIDTH_KEYWORD).is_some() {
        return Some(None);
    }
    ratio_inline_size(flat, index, space, edges, block_edges, sizes)
}

/// [`ratio_inline_contribution`] whatever the `width`: Blink's
/// ComputeMinMaxSizes of a box with an aspect ratio and a definite block
/// size is the transferred inline size, which intrinsic `width` keywords
/// then resolve against (aspect-ratio/intrinsic-size-004, -014).
fn ratio_inline_size(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    edges: f32,
    block_edges: f32,
    sizes: MinMax,
) -> Option<Option<f32>> {
    let style = flat[index].style;
    if is_replaced(flat[index].kind) || size_contained(style) {
        return Some(None);
    }
    let Some(ratio) = BoxAspectRatio::new(style, edges, block_edges) else {
        return Some(None);
    };
    let Some(height) = child_percentage_block(style, space) else {
        return Some(None);
    };
    let floor = if style.min_width == Dimension::Auto && !non_visible_overflow(style) {
        sizes.min.or_else(|| super::bail("aspect-ratio-min"))?
    } else {
        0.0
    };
    Some(Some(ratio.inline_from_block(height).max(floor)))
}

/// ComputeMinAndMaxContentContribution of a child: its margin box, from a
/// definite width or its own intrinsic sizes, clamped by min/max-width.
pub(super) fn min_max_contribution(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
) -> Option<MinMax> {
    let style = flat[index].style;
    let side = |s: Spacing| match s {
        Spacing::Auto => 0.0,
        s => resolve_spacing_for_layout(s, 0.0, style, space.viewport_w, space.viewport_h),
    };
    let b = border(style);
    let edges = b[1] + b[3] + side(style.padding.left) + side(style.padding.right);
    let margins = side(style.margin.left) + side(style.margin.right);
    let content = match resolve_dim(style.width, None, style, space) {
        Some(w) => MinMax::exact(match style.box_sizing {
            BoxSizing::BorderBox => (w - edges).max(0.0),
            BoxSizing::ContentBox => w,
        }),
        None if marker(style, WIDTH_KEYWORD).is_some()
            && !size_contained(style)
            && !is_replaced(flat[index].kind) =>
        {
            let sizes = intrinsic_sizes(flat, index, space).why("intrinsic")?;
            keyword_width_contribution(flat, index, space, edges, sizes)?
        }
        None => {
            let sizes = intrinsic_sizes(flat, index, space).why("intrinsic")?;
            let block_edges = b[0] + b[2] + side(style.padding.top) + side(style.padding.bottom);
            match ratio_inline_contribution(flat, index, space, edges, block_edges, sizes)? {
                Some(inline) => MinMax::exact(inline),
                None => sizes,
            }
        }
    };
    let limits = keyword_contribution_limits(flat, index, space, edges)?;
    Some(
        apply_contribution_limits(content, limits, |w| clamp_inline(style, space, edges, w))
            .map(|w| w + edges + margins),
    )
}

/// Content-box `min-width` / `max-width` keyword limits of an intrinsic
/// contribution, as (min-content side, max-content side) pairs. Under a
/// min-content constraint `fit-content` is min-content and under a
/// max-content constraint it is max-content (css-sizing-3;
/// fit-content-min-inline-size). Other keywords limit both sides alike.
type ContributionLimits = (Option<(f32, f32)>, Option<(f32, f32)>);

fn keyword_contribution_limits(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    edges: f32,
) -> Option<ContributionLimits> {
    let style = flat[index].style;
    let (min_kw, max_kw) = (
        marker(style, MIN_WIDTH_KEYWORD),
        marker(style, MAX_WIDTH_KEYWORD),
    );
    let (min_limit, max_limit) = keyword_inline_limits(flat, index, space, edges, None, None)?;
    let fit = |keyword: Option<&str>| matches!(keyword, Some("fit-content"));
    let sides = if fit(min_kw) || fit(max_kw) {
        let sizes = own_keyword_sizes(flat, index, space, edges)?;
        let min = sizes.min.or_else(|| super::bail("width-keyword-min"))?;
        Some((min, sizes.max.max(min)))
    } else {
        None
    };
    let pair = |keyword: Option<&str>, limit: Option<f32>| match (sides, fit(keyword)) {
        (Some(sides), true) => Some(sides),
        _ => limit.map(|l| (l, l)),
    };
    Some((pair(min_kw, min_limit), pair(max_kw, max_limit)))
}

/// Clamp a content-box contribution with `clamp`, then cap each side by
/// its max keyword limit and floor it by its min keyword limit.
fn apply_contribution_limits(
    content: MinMax,
    (min_kw, max_kw): ContributionLimits,
    clamp: impl Fn(f32) -> f32,
) -> MinMax {
    let side = |w: f32, pick: fn((f32, f32)) -> f32| {
        let mut w = clamp(w);
        if let Some(max) = max_kw {
            w = w.min(pick(max));
        }
        if let Some(min) = min_kw {
            w = w.max(pick(min));
        }
        w
    };
    MinMax {
        min: content.min.map(|w| side(w, |p| p.0)),
        max: side(content.max, |p| p.1),
    }
}

/// The content-box contribution of a box with an intrinsic `width`
/// keyword, from its own `sizes`: min-content and max-content contribute
/// that size to both intrinsic sizes; fit-content contributes like auto,
/// and so does fit-content(<percentage>), whose percentage is cyclic here
/// (css-sizing-3: it behaves as the initial value). Without a keyword (or
/// for a replaced or size-contained box) the sizes are returned as is.
pub(super) fn keyword_width_contribution(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    edges: f32,
    sizes: MinMax,
) -> Option<MinMax> {
    let style = flat[index].style;
    let keyword = marker(style, WIDTH_KEYWORD).unwrap_or_default();
    if !(matches!(keyword, "min-content" | "max-content") || keyword.starts_with("fit-content("))
        || size_contained(style)
        || is_replaced(flat[index].kind)
    {
        return Some(sizes);
    }
    let side = |s: Spacing| match s {
        Spacing::Auto => 0.0,
        s => resolve_spacing_for_layout(s, 0.0, style, space.viewport_w, space.viewport_h),
    };
    let b = border(style);
    let block_edges = b[0] + b[2] + side(style.padding.top) + side(style.padding.bottom);
    let sizes = match ratio_inline_size(flat, index, space, edges, block_edges, sizes)? {
        Some(inline) => MinMax::exact(inline),
        None => sizes,
    };
    let min = sizes.min.or_else(|| super::bail("width-keyword-min"))?;
    Some(
        match keyword_content_size(keyword, min, sizes.max, None, None, style, space, edges) {
            Some(width) => MinMax::exact(width),
            None => sizes,
        },
    )
}

/// The `<length-percentage>` of a `fit-content(...)` marker value, in the
/// canonical units the cascade writes.
fn fit_content_argument(keyword: &str) -> Option<Dimension> {
    let inner = keyword
        .strip_prefix("fit-content(")?
        .strip_suffix(')')?
        .trim();
    let number = |suffix: &str| inner.strip_suffix(suffix)?.trim().parse::<f32>().ok();
    if let Some(n) = number("rem") {
        return Some(Dimension::Rem(n));
    }
    [
        ("px", Dimension::Px as fn(f32) -> Dimension),
        ("%", Dimension::Percent),
        ("em", Dimension::Em),
        ("vw", Dimension::Vw),
        ("vh", Dimension::Vh),
    ]
    .into_iter()
    .find_map(|(suffix, make)| number(suffix).map(make))
}

/// The content-box size of an intrinsic sizing keyword on an inline size
/// property, given the box's min/max-content sizes: `min-content`,
/// `max-content`, `fit-content` (the available content size clamped
/// between them) and `fit-content(L)` = max(min-content, min(max-content,
/// L)), with L a border-box size under `box-sizing: border-box`. `None`
/// when the keyword needs a size this pass does not have: an indefinite
/// available size, or a percentage against an indefinite containing block.
#[allow(clippy::too_many_arguments)]
pub(super) fn keyword_content_size(
    keyword: &str,
    min: f32,
    max: f32,
    available: Option<f32>,
    cb: Option<f32>,
    style: &Style,
    space: &ConstraintSpace,
    edges: f32,
) -> Option<f32> {
    let max = max.max(min);
    match keyword {
        "min-content" => Some(min),
        "max-content" => Some(max),
        "fit-content" => available.map(|a| max.min(a.max(min))),
        _ => {
            let length = resolve_dim(fit_content_argument(keyword)?, cb, style, space)?;
            let length = match style.box_sizing {
                BoxSizing::BorderBox => (length - edges).max(0.0),
                BoxSizing::ContentBox => length,
            };
            Some(max.min(length.max(min)))
        }
    }
}

/// The content-box min/max-content sizes intrinsic `min-width` /
/// `max-width` keywords resolve against: a box with an aspect ratio and a
/// definite height has its transferred inline size
/// (border-box-and-max-content-002).
fn own_keyword_sizes(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    edges: f32,
) -> Option<MinMax> {
    let style = flat[index].style;
    let sizes = intrinsic_sizes(flat, index, space).why("width-keyword")?;
    let side = |s: Spacing| match s {
        Spacing::Auto => 0.0,
        s => resolve_spacing_for_layout(s, 0.0, style, space.viewport_w, space.viewport_h),
    };
    let b = border(style);
    let block_edges = b[0] + b[2] + side(style.padding.top) + side(style.padding.bottom);
    Some(
        match ratio_inline_size(flat, index, space, edges, block_edges, sizes)? {
            Some(inline) => MinMax::exact(inline),
            None => sizes,
        },
    )
}

/// Content-box `min-width` / `max-width` from intrinsic keywords (Blink
/// ResolveMinInlineLength / ResolveMaxInlineLength). With no available
/// size (an intrinsic contribution) a keyword that needs one behaves as
/// the initial value would after the min/max-content clamp: `min-width`
/// as min-content, `max-width` as max-content.
fn keyword_inline_limits(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    edges: f32,
    available: Option<f32>,
    cb: Option<f32>,
) -> Option<(Option<f32>, Option<f32>)> {
    let style = flat[index].style;
    let min_kw = marker(style, MIN_WIDTH_KEYWORD);
    let max_kw = marker(style, MAX_WIDTH_KEYWORD);
    if (min_kw.is_none() && max_kw.is_none())
        || size_contained(style)
        || is_replaced(flat[index].kind)
    {
        return Some((None, None));
    }
    let sizes = own_keyword_sizes(flat, index, space, edges)?;
    let min = sizes.min.or_else(|| super::bail("width-keyword-min"))?;
    let max = sizes.max.max(min);
    let resolve = |keyword: &str, fallback: f32| {
        keyword_content_size(keyword, min, max, available, cb, style, space, edges)
            .unwrap_or(fallback)
    };
    Some((
        min_kw.map(|k| resolve(k, min)),
        max_kw.map(|k| resolve(k, max)),
    ))
}

/// An out-of-flow box's content-box width limited by intrinsic
/// `min-width` / `max-width` keywords (`available` is the inline space
/// between its insets, `cb` the containing block width).
pub(super) fn keyword_limited_inline(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    edges: f32,
    width: f32,
    available: f32,
    cb: f32,
) -> Option<f32> {
    let (min_kw, max_kw) =
        keyword_inline_limits(flat, index, space, edges, Some(available), Some(cb))?;
    let mut width = width;
    if let Some(max) = max_kw {
        width = width.min(max);
    }
    if let Some(min) = min_kw {
        width = width.max(min);
    }
    Some(width)
}

/// Apply intrinsic min-width / max-width keywords to a laid-out box's
/// inline size: re-solve the box as for that definite width so auto
/// margins follow (CSS2 10.4).
fn apply_keyword_inline_limits(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    style: &Style,
    metrics: BoxMetrics,
) -> Option<BoxMetrics> {
    let edges = metrics.border[1] + metrics.border[3] + metrics.padding[1] + metrics.padding[3];
    // The available size subtracts the computed margins (auto as zero),
    // not the used ones: an over-constrained box's used end margin takes
    // up the rest of the line (CSS2 10.3.3) and would leave only its own
    // width (flex-minimum-width-flex-items-014).
    let margin = |spacing| match spacing {
        Spacing::Auto => 0.0,
        other => resolve_spacing_for_layout(
            other,
            space.available_inline,
            style,
            space.viewport_w,
            space.viewport_h,
        ),
    };
    let available =
        space.available_inline - margin(style.margin.left) - margin(style.margin.right) - edges;
    let (min_kw, max_kw) = keyword_inline_limits(
        flat,
        index,
        space,
        edges,
        Some(available),
        Some(space.available_inline),
    )?;
    if min_kw.is_none() && max_kw.is_none() {
        return Some(metrics);
    }
    let mut width = metrics.content_width;
    if let Some(max) = max_kw {
        width = width.min(max);
    }
    if let Some(min) = min_kw {
        width = width.max(min);
    }
    if width == metrics.content_width {
        return Some(metrics);
    }
    let mut definite = style.clone();
    definite.width = Dimension::Px(match style.box_sizing {
        BoxSizing::BorderBox => width + edges,
        BoxSizing::ContentBox => width,
    });
    definite.min_width = Dimension::Auto;
    definite.max_width = Dimension::Auto;
    Some(resolve_inline(&definite, space))
}

/// A non-replaced box's inline metrics after its intrinsic `width`
/// keyword and its `min-width` / `max-width` keywords: the keyword size
/// becomes a definite width, then the limits clamp it. Flex containers
/// laid out without a parent-imposed width use this.
pub(super) fn apply_width_keywords(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    mut metrics: BoxMetrics,
) -> Option<BoxMetrics> {
    let style = flat[index].style;
    if size_contained(style) || is_replaced(flat[index].kind) {
        return Some(metrics);
    }
    if let Some(keyword) = marker(style, WIDTH_KEYWORD) {
        let content = keyword_inline_size(flat, index, space, &metrics, keyword)?;
        let edges = metrics.border[1] + metrics.border[3] + metrics.padding[1] + metrics.padding[3];
        let mut definite = style.clone();
        definite.width = Dimension::Px(match style.box_sizing {
            BoxSizing::BorderBox => content + edges,
            BoxSizing::ContentBox => content,
        });
        metrics = resolve_inline(&definite, space);
    }
    apply_keyword_inline_limits(flat, index, space, style, metrics)
}

/// The content-box inline size of an intrinsic `width` keyword
/// (css-sizing-3 min-content, max-content and fit-content).
fn keyword_inline_size(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    metrics: &BoxMetrics,
    keyword: &str,
) -> Option<f32> {
    let sizes = intrinsic_sizes(flat, index, space).why("width-keyword")?;
    let edges = metrics.border[1] + metrics.border[3] + metrics.padding[1] + metrics.padding[3];
    let block_edges =
        metrics.border[0] + metrics.border[2] + metrics.padding[0] + metrics.padding[2];
    let sizes = match ratio_inline_size(flat, index, space, edges, block_edges, sizes)? {
        Some(inline) => MinMax::exact(inline),
        None => sizes,
    };
    let min = sizes.min.or_else(|| super::bail("width-keyword-min"))?;
    let available = space.available_inline - metrics.margin[1] - metrics.margin[3] - edges;
    keyword_content_size(
        keyword,
        min,
        sizes.max,
        Some(available),
        Some(space.available_inline),
        flat[index].style,
        space,
        edges,
    )
    .or_else(|| super::bail("width-keyword"))
}

/// CSS2 10.3.9 shrink-to-fit width of an inline-block's content box:
/// min(max(min-content, available), max-content), then min/max-width.
fn shrink_to_fit(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    metrics: &BoxMetrics,
) -> Option<f32> {
    let style = flat[index].style;
    let side = |s: Spacing| match s {
        Spacing::Auto => 0.0,
        s => resolve_spacing_for_layout(
            s,
            space.available_inline,
            style,
            space.viewport_w,
            space.viewport_h,
        ),
    };
    let edges = metrics.border[1] + metrics.border[3] + metrics.padding[1] + metrics.padding[3];
    let available =
        space.available_inline - side(style.margin.left) - side(style.margin.right) - edges;
    let sizes = intrinsic_sizes(flat, index, space).why("shrink-intrinsic")?;
    let width = match sizes.min {
        Some(min) => sizes.max.min(available.max(min)),
        None if sizes.max <= available + 1.0 / 128.0 => sizes.max,
        None => return super::bail("shrink-min"),
    };
    let cb = (!space.percent_unresolved).then_some(space.available_inline);
    Some(clamp_inline_in(style, space, edges, width.max(0.0), cb))
}

/// Author `display:flex`. Synthetic boxes stay on their own algorithms:
/// an IFC-marker row is an inline formatting context, and a float-group
/// row is transparent in its parent's BFC.
fn is_grid_lanes(style: &Style) -> bool {
    marker(style, "--w3cos-internal-grid-lanes").is_some()
}

fn unrecognized_display(style: &Style) -> Option<&str> {
    marker(style, "--w3cos-internal-unrecognized-display")
}

fn is_inline_grid(style: &Style) -> bool {
    unrecognized_display(style).is_some_and(|value| value == "inline-grid")
}

fn is_blockified_inline_grid(flat: &[FlatNodeInfo], index: usize) -> bool {
    is_inline_grid(flat[index].style)
        && flat[index].parent.is_some_and(|parent| {
            is_author_flex(flat[parent].style) || is_author_grid(flat[parent].style)
        })
}

/// Top-level `inline-grid` that shares a line with its siblings. A flex or
/// grid item is blockified and is not one of these.
fn is_inline_grid_line(flat: &[FlatNodeInfo], index: usize) -> bool {
    is_inline_grid(flat[index].style)
        && !is_blockified_inline_grid(flat, index)
        && (super::grid::definite_inline_grid(flat[index].style)
            || super::grid::fixed_track_inline_grid(flat, index)
            || super::grid::intrinsic_track_inline_grid(flat, index))
}

/// Ascent of an inline-grid on a line. `NoLines` is the bottom border
/// edge: an empty grid synthesizes its baseline there.
fn inline_grid_line_ascent(result: &LayoutResult) -> f32 {
    match result.first_baseline {
        LastBaseline::At(baseline) => baseline,
        LastBaseline::NoLines | LastBaseline::Unknown => result.fragment.height,
    }
}

fn is_run_in(style: &Style) -> bool {
    unrecognized_display(style).is_some_and(|value| value == "run-in")
}

/// Chrome rejects `display: run-in` as invalid, so the earlier cascaded
/// value applies. The DOM marks a run-in over `block`; such a box is laid
/// out as that block (in place of the legacy flex lowering) when its
/// contents need no anonymous boxes: nothing, one inline text, or only
/// in-flow block-level boxes. Returns the replacement styles by index.
pub(super) fn run_in_block_styles(flat: &[FlatNodeInfo]) -> Vec<(usize, Style)> {
    let mut styles = Vec::new();
    for (index, info) in flat.iter().enumerate() {
        let run_in = is_run_in(info.style) && marker(info.style, RUN_IN_BLOCK).is_some();
        if matches!(info.kind, ComponentKind::Text { .. })
            || (!run_in && !is_vertical_webkit_box(info.style))
        {
            continue;
        }
        let children: Vec<&FlatNodeInfo> = flat[index + 1..info.subtree_end]
            .iter()
            .filter(|child| child.parent == Some(index))
            .collect();
        let contents = match children.as_slice() {
            [] => true,
            [text] if matches!(text.kind, ComponentKind::Text { .. }) => {
                text.style.display == Display::Inline
            }
            _ => children.iter().all(|child| {
                matches!(
                    child.style.display,
                    Display::Block | Display::FlowRoot | Display::ListItem | Display::Table
                ) && child.style.float == Float::None
                    && matches!(child.style.position, Position::Static | Position::Relative)
                    && unrecognized_display(child.style).is_none()
            }),
        };
        if contents {
            let mut style = info.style.clone();
            style.display = if run_in {
                Display::Block
            } else {
                Display::FlowRoot
            };
            if let Some(properties) = style.custom_properties.as_mut() {
                properties.remove("--w3cos-internal-unrecognized-display");
                properties.remove(RUN_IN_BLOCK);
                properties.remove("--w3cos-internal-webkit-box-orient");
                // `-webkit-line-clamp` counts the box's lines (inline.rs).
                if let Some(lines) = properties.remove("--w3cos-internal-webkit-line-clamp") {
                    properties.insert(inline::CLAMP_MARKER.into(), lines);
                }
                if properties.is_empty() {
                    style.custom_properties = None;
                }
            }
            styles.push((index, style));
        }
    }
    styles
}

/// Legacy `display: -webkit-box` and `-webkit-inline-box`. Both parse as
/// `display: flex` plus the unrecognized marker, so NG off is unchanged.
/// `-webkit-inline-box` is not an inline-flex: the used value is the
/// block flex box the legacy engine already paints.
const RUN_IN_BLOCK: &str = "--w3cos-internal-run-in-block";

/// A `-webkit-box` with `-webkit-box-orient: vertical` stacks its in-flow
/// children as a block flow root; the legacy engine lays out the flex row.
fn is_vertical_webkit_box(style: &Style) -> bool {
    unrecognized_display(style).is_some_and(|value| value == "-webkit-box")
        && marker(style, "--w3cos-internal-webkit-box-orient") == Some("vertical")
        && style.direction == TextDirection::Ltr
}

fn is_legacy_webkit_box(style: &Style) -> bool {
    unrecognized_display(style)
        .is_some_and(|value| matches!(value, "-webkit-box" | "-webkit-inline-box"))
}

pub(super) fn is_author_flex(style: &Style) -> bool {
    style.display == Display::Flex
        && marker(style, inline::IFC_MARKER).is_none()
        && !is_float_group(style)
        && !is_grid_lanes(style)
        && !is_inline_grid(style)
}

pub(super) fn is_author_grid(style: &Style) -> bool {
    style.display == Display::Grid || is_grid_lanes(style) || is_inline_grid(style)
}

fn is_bfc_root(style: &Style) -> bool {
    // A flex or grid container establishes an independent formatting
    // context (CSS Flexbox §3, CSS Grid §1). Non-visible overflow does
    // too, on the boxes CSS 2.1 11.1.1 clips (CSS2 9.4.1).
    style.display == Display::FlowRoot
        || style.display == Display::Table
        || is_author_flex(style)
        || is_author_grid(style)
        || (non_visible_overflow(style) && style.display.establishes_overflow_clip())
}

/// Lay out the root box inside the initial containing block.  Root margins
/// never collapse (CSS2 8.3.1).  Returns the root's border-box offset.
pub(super) fn layout_root(
    flat: &[FlatNodeInfo],
    viewport_w: f32,
    viewport_h: f32,
) -> Option<(f32, f32, BoxFragment)> {
    let space = ConstraintSpace {
        available_inline: viewport_w,
        percentage_block: Some(viewport_h),
        viewport_w,
        viewport_h,
        // The initial containing block takes the root element's direction.
        rtl: flat[0].style.direction == TextDirection::Rtl,
        bfc_x: 0.0,
        bfc_y: 0.0,
        margin_base: 0.0,
        margin_strut: MarginStrut::default(),
        forced_bfc: false,
        adjoining_from: 0,
        fixed_block_indefinite: false,
        percent_unresolved: false,
    };
    let result = layout_block(flat, 0, &space, true).why("root-layout")?;
    if result.float_overlap {
        return super::bail("float-overlap");
    }
    let metrics = resolve_inline(flat[0].style, &space);
    let style = flat[0].style;
    let (mut x, y) = (metrics.margin[3], metrics.margin[0]);
    if matches!(style.display, Display::Table | Display::InlineTable)
        && (style.margin.left == Spacing::Auto || style.margin.right == Spacing::Auto)
    {
        let auto_left = style.margin.left == Spacing::Auto;
        let auto_right = style.margin.right == Spacing::Auto;
        let specified_left = if auto_left { 0.0 } else { metrics.margin[3] };
        let specified_right = if auto_right { 0.0 } else { metrics.margin[1] };
        let extra = viewport_w - result.fragment.width - specified_left - specified_right;
        x = if space.rtl {
            match (auto_right, auto_left) {
                (true, true) => layout_unit((extra / 2.0).max(0.0)),
                (true, false) => specified_left,
                (false, true) => (specified_left + extra).max(0.0),
                (false, false) => metrics.margin[3],
            }
        } else {
            match (auto_left, auto_right) {
                (true, true) => layout_unit((extra / 2.0).max(0.0)),
                (true, false) => extra.max(0.0),
                (false, true) => specified_left,
                (false, false) => metrics.margin[3],
            }
        };
    }
    let mut fragment = result.fragment;
    // The initial containing block (CSS2 10.1): the viewport-sized
    // rectangle at the canvas origin; it also contains fixed boxes.
    let icb = ContainingBlock {
        x: -x,
        y: -y,
        width: viewport_w,
        height: viewport_h,
        rtl: space.rtl,
    };
    let (placed, rest) = oof::resolve(flat, result.oof, &icb, true, viewport_w, viewport_h)?;
    debug_assert!(rest.is_empty());
    fragment.children.extend(placed);
    Some((x, y, fragment))
}

pub(super) fn layout_block(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    force_bfc_root: bool,
) -> Option<LayoutResult> {
    layout_block_sized(flat, index, space, force_bfc_root, None)
}

/// `sized`: the content-box width and (optionally) height an out-of-flow
/// box's containing block or a float's shrink-to-fit resolved (CSS2
/// 10.3.7 / 10.6.4 / 10.3.5).  Lays out a BFC root: fresh exclusions.
pub(super) fn layout_block_sized(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    force_bfc_root: bool,
    sized: Option<(f32, Option<f32>)>,
) -> Option<LayoutResult> {
    let space = ConstraintSpace {
        bfc_x: 0.0,
        bfc_y: 0.0,
        ..*space
    };
    layout_block_in(
        flat,
        index,
        &space,
        force_bfc_root,
        sized,
        &mut ExclusionSpace::default(),
    )
}

/// Visits the boxes whose containing block is `index`'s content box: its
/// rendered children, looking through synthetic float rows.
fn for_each_cb_child(flat: &[FlatNodeInfo], index: usize, f: &mut dyn FnMut(usize)) {
    let end = flat[index].subtree_end;
    let mut child = index + 1;
    while child < end {
        let next = flat[child].subtree_end.max(child + 1);
        if flat[child].style.display != Display::None {
            if is_float_group(flat[child].style) {
                for_each_cb_child(flat, child, f);
            } else {
                f(child);
            }
        }
        child = next;
    }
}

/// Replacement styles for the in-flow and floated children of block
/// container `index` that carry `stretch`; `cb` is its content box.
/// `ignore_top` / `ignore_bottom`: the container has no top (bottom)
/// border or padding and is not a BFC root, so the child's block margin
/// on that side does not take part in the block size.
pub(super) fn stretch_child_styles(
    flat: &[FlatNodeInfo],
    index: usize,
    cb: &ConstraintSpace,
    ignore_top: bool,
    ignore_bottom: bool,
) -> Vec<(usize, Style)> {
    let mut out = Vec::new();
    // A box that establishes a formatting context beside a float is sized
    // against the space the float leaves; that avoidance is not modelled,
    // so its inline `stretch` stays auto (as for flex, grid and table
    // items, which do not resolve the keyword either).
    let mut has_float = false;
    for_each_cb_child(flat, index, &mut |child| {
        has_float |= flat[child].style.float != Float::None;
    });
    for_each_cb_child(flat, index, &mut |child| {
        let style = flat[child].style;
        let Some(tokens) = marker(style, STRETCH) else {
            return;
        };
        if oof::is_out_of_flow(style) {
            return;
        }
        let side = |s: Spacing| match s {
            Spacing::Auto => 0.0,
            s => resolve_spacing_for_layout(
                s,
                cb.available_inline,
                style,
                cb.viewport_w,
                cb.viewport_h,
            ),
        };
        let border = border(style);
        let edges_h = border[1] + border[3] + side(style.padding.left) + side(style.padding.right);
        let edges_v = border[0] + border[2] + side(style.padding.top) + side(style.padding.bottom);
        let in_flow_block = style.float == Float::None && !inline::is_atomic_inline(flat, child);
        let to_content = |size: f32, edges: f32| match style.box_sizing {
            BoxSizing::BorderBox => size.max(0.0),
            BoxSizing::ContentBox => (size - edges).max(0.0),
        };
        let inline_size = to_content(
            cb.available_inline - side(style.margin.left) - side(style.margin.right),
            edges_h,
        );
        let block_size = cb.percentage_block.map(|available| {
            let top = if in_flow_block && ignore_top {
                0.0
            } else {
                side(style.margin.top)
            };
            let bottom = if in_flow_block && ignore_bottom {
                0.0
            } else {
                side(style.margin.bottom)
            };
            to_content(available - top - bottom, edges_v)
        });
        let mut patched = style.clone();
        for token in tokens.split(',') {
            match token {
                "w" | "minw" | "maxw"
                    if has_float && style.float == Float::None && is_bfc_root(style) => {}
                "w" => patched.width = Dimension::Px(inline_size),
                "minw" => patched.min_width = Dimension::Px(inline_size),
                "maxw" => patched.max_width = Dimension::Px(inline_size),
                "h" => {
                    if let Some(size) = block_size {
                        patched.height = Dimension::Px(size);
                    }
                }
                "minh" => {
                    if let Some(size) = block_size {
                        patched.min_height = Dimension::Px(size);
                    }
                }
                "maxh" => {
                    if let Some(size) = block_size {
                        patched.max_height = Dimension::Px(size);
                    }
                }
                _ => {}
            }
        }
        out.push((child, patched));
    });
    out
}

/// `flat` with the styles of `patched` boxes replaced.
pub(super) fn flat_with_styles<'a>(
    flat: &'a [FlatNodeInfo<'a>],
    patched: &'a [(usize, Style)],
) -> Vec<FlatNodeInfo<'a>> {
    flat.iter()
        .enumerate()
        .map(|(i, node)| {
            let mut node = node.clone();
            if let Some((_, style)) = patched.iter().find(|(j, _)| *j == i) {
                node.style = style;
            }
            node
        })
        .collect()
}

/// NGBlockLayoutAlgorithm in a block formatting context whose exclusion
/// space is `exclusions` (a BFC root starts its own).
fn layout_block_in(
    flat: &[FlatNodeInfo],
    index: usize,
    space: &ConstraintSpace,
    force_bfc_root: bool,
    sized: Option<(f32, Option<f32>)>,
    exclusions: &mut ExclusionSpace,
) -> Option<LayoutResult> {
    let info = &flat[index];
    let style = info.style;
    // NGFlexLayoutAlgorithm. Lowered IFC rows and synthetic float groups
    // stay on the paths below.
    if is_author_flex(style) {
        return super::flex::layout(flat, index, space, sized);
    }
    if is_author_grid(style) {
        return super::grid::layout(flat, index, space, sized);
    }
    if matches!(style.display, Display::Table | Display::InlineTable) {
        return super::table::layout(flat, index, space, sized);
    }
    // CSS 2.1 10.3.2 / 10.3.4: a replaced width is the intrinsic (or
    // specified) content size, then auto margins are solved as for a
    // definite width. `sized` is a shrink-to-fit override for floats and
    // new formatting contexts; a replaced box keeps its own width and the
    // overlap check sends a float collision back to legacy.
    let image_size = if is_replaced(info.kind) {
        Some(match sized {
            // A flex item's used content box (FlexLayoutAlgorithm gives the
            // replaced box its flexed and stretched sizes). An automatic
            // block size is transferred from that inline size through the
            // aspect ratio, then clamped (Blink ComputeReplacedSize with an
            // inline-size override).
            Some((width, Some(height))) => (width, height),
            Some((width, None)) => {
                let mut definite = style.clone();
                definite.width = Dimension::Px(width);
                override_replaced_axis(&mut definite, true);
                definite.min_width = Dimension::Auto;
                definite.max_width = Dimension::Auto;
                let mut height = replaced_size(info.kind, &definite, space)?.1;
                // An image with no known ratio (not decoded: the 200x200
                // placeholder) that a flex item's max-width or shrinking
                // made narrower than its natural width scales both axes,
                // as the min/max constraint table does (CSS 2.1 10.4),
                // instead of keeping the natural height
                // (flex-item auto-min-size-001).
                if matches!(style.height, Dimension::Auto)
                    && matches!(style.width, Dimension::Auto)
                    && matches!(info.kind, ComponentKind::Image { src } if crate::layout::image_intrinsic_ratio(src).is_none())
                    && let Some((natural_width, natural_height)) =
                        replaced_intrinsic_size(flat, index, space)
                    && natural_width > width
                    && width >= 0.0
                {
                    height = natural_height * width / natural_width;
                }
                (width, height)
            }
            None => replaced_size_with_keyword_limits(info.kind, style, space)?,
        })
    } else {
        None
    };
    let mut metrics = if let Some((width, _)) = image_size {
        let mut definite = style.clone();
        definite.width = Dimension::Px(width);
        definite.min_width = Dimension::Auto;
        definite.max_width = Dimension::Auto;
        definite.box_sizing = BoxSizing::ContentBox;
        resolve_inline(&definite, space)
    } else {
        resolve_inline(style, space)
    };
    let width_keyword = marker(style, WIDTH_KEYWORD)
        .filter(|_| image_size.is_none() && sized.is_none() && !size_contained(style));
    if let Some(keyword) = width_keyword {
        // Blink ComputeInlineSizeForFragment: an intrinsic keyword is the
        // box's own min/max-content size (fit-content clamps the available
        // size between them), then min/max-width and auto margins apply as
        // for a definite width.
        let content = keyword_inline_size(flat, index, space, &metrics, keyword)?;
        let edges = metrics.border[1] + metrics.border[3] + metrics.padding[1] + metrics.padding[3];
        let mut definite = style.clone();
        definite.width = Dimension::Px(match style.box_sizing {
            BoxSizing::BorderBox => content + edges,
            BoxSizing::ContentBox => content,
        });
        metrics = resolve_inline(&definite, space);
    } else if image_size.is_none() {
        if let Some((width, _)) = sized {
            metrics.content_width = width;
        } else if style.width == Dimension::Auto
            && (inline::is_atomic_inline(flat, index) || is_html_button(info))
        {
            // A button's auto width is fit-content even when it is
            // block-level (HTML §15.5.3, Blink LayoutButton).
            metrics.content_width =
                shrink_to_fit(flat, index, space, &metrics).why("shrink-to-fit")?;
        }
    }
    if image_size.is_none() && sized.is_none() {
        metrics = apply_keyword_inline_limits(flat, index, space, style, metrics)?;
    }
    let [bt, br, bb, bl] = metrics.border;
    let [pt, pr, pb, pl] = metrics.padding;
    // A button establishes a formatting context for its content.
    let button = is_html_button(info);
    let bfc_root = force_bfc_root || is_bfc_root(style) || button;
    // NGBfcOffset of our border box; a BFC root starts new coordinates and
    // a new exclusion space (NGConstraintSpace::IsNewFormattingContext).
    let mut own_exclusions = ExclusionSpace::default();
    let (exclusions, bfc_x, bfc_y) = if bfc_root {
        (&mut own_exclusions, 0.0, 0.0)
    } else {
        (exclusions, space.bfc_x, space.bfc_y)
    };
    let (margin_base, margin_strut) = (space.margin_base, space.margin_strut);
    let entry_exclusions = !exclusions.is_empty();
    let mut float_overlap = false;

    let to_content_h = |h: f32| match style.box_sizing {
        BoxSizing::BorderBox => (h - bt - bb - pt - pb).max(0.0),
        BoxSizing::ContentBox => h,
    };
    // A replaced box's used height is its content height (an svg's used
    // `height` may be the DOM's fallback size, not a specified one).
    let specified_height = match (sized, image_size) {
        (Some((_, Some(height))), _) => Some(height),
        (_, Some((_, height))) => Some(height),
        _ => resolve_dim(
            used_height(style, space),
            space.percentage_block,
            style,
            space,
        )
        .map(to_content_h),
    };
    // DOM lowering gives an authored IFC block `min-height: font-size *
    // line-height` so its flex-row form keeps the initial strut.  The NG
    // line box already contains that strut (in LayoutUnits), so the
    // synthetic floor is not a real constraint here.
    let synthetic_strut_floor = style
        .custom_properties
        .as_ref()
        .is_some_and(|props| props.contains_key(inline::IFC_MARKER))
        && style.min_height == Dimension::Px(style.font_size * style.line_height)
        && style.height == Dimension::Auto
        && style.max_height == Dimension::Auto;
    let min_h = if synthetic_strut_floor {
        None
    } else {
        resolve_dim(style.min_height, space.percentage_block, style, space).map(to_content_h)
    };
    let max_h =
        resolve_dim(style.max_height, space.percentage_block, style, space).map(to_content_h);
    let clamp_h = |mut h: f32| {
        if let Some(max) = max_h {
            h = h.min(max);
        }
        if let Some(min) = min_h {
            h = h.max(min);
        }
        h
    };
    // css-sizing-4 aspect-ratio on a non-replaced block container. With a
    // definite block size an auto inline size comes from the ratio (its
    // automatic minimum is the min-content size); otherwise the block size
    // comes from the inline size, whose range takes the min/max block
    // sizes through the ratio without overriding min/max-width (Blink
    // ComputeTransferredMinMaxInlineSizes).
    let aspect_ratio = image_size
        .is_none()
        .then(|| BoxAspectRatio::of(style, &metrics))
        .flatten();
    let mut ratio_height = None;
    if let Some(ratio) = aspect_ratio {
        let cb = space.available_inline;
        let edges = ratio.inline_edges;
        let to_content_w = |w: f32| match style.box_sizing {
            BoxSizing::BorderBox => (w - edges).max(0.0),
            BoxSizing::ContentBox => w,
        };
        let min_w = resolve_dim(style.min_width, Some(cb), style, space).map(to_content_w);
        let max_w = resolve_dim(style.max_width, Some(cb), style, space).map(to_content_w);
        let own_width = sized.is_none()
            && style.width == Dimension::Auto
            && marker(style, WIDTH_KEYWORD).is_none();
        let mut width = None;
        match specified_height {
            Some(height) if own_width => {
                let mut w = ratio.inline_from_block(clamp_h(height));
                if style.min_width == Dimension::Auto && !non_visible_overflow(style) {
                    let min_content = intrinsic_sizes(flat, index, space)
                        .why("aspect-ratio-intrinsic")?
                        .min
                        .or_else(|| super::bail("aspect-ratio-min"))?;
                    w = w.max(max_w.map_or(min_content, |max| min_content.min(max)));
                }
                width = Some(w);
            }
            Some(_) => {}
            None => {
                if own_width {
                    let mut w = metrics.content_width;
                    if let Some(max) = max_h.map(|h| ratio.inline_from_block(h)) {
                        w = w.min(max.max(min_w.unwrap_or(0.0)));
                    }
                    if let Some(min) = min_h.map(|h| ratio.inline_from_block(h)) {
                        w = w.max(max_w.map_or(min, |max| min.min(max)));
                    }
                    if (w - metrics.content_width).abs() > 1.0 / 128.0 {
                        width = Some(w);
                    }
                }
                let w = width.unwrap_or(metrics.content_width);
                ratio_height = Some(ratio.block_from_inline(w));
            }
        }
        if let Some(mut w) = width {
            if let Some(max) = max_w {
                w = w.min(max);
            }
            if let Some(min) = min_w {
                w = w.max(min);
            }
            let mut definite = style.clone();
            definite.width = Dimension::Px(w);
            definite.min_width = Dimension::Auto;
            definite.max_width = Dimension::Auto;
            definite.box_sizing = BoxSizing::ContentBox;
            metrics = resolve_inline(&definite, space);
            if ratio_height.is_some() {
                ratio_height = Some(ratio.block_from_inline(w));
            }
        }
    }
    let anonymous_block = style
        .custom_properties
        .as_ref()
        .is_some_and(|props| props.contains_key(inline::ANONYMOUS_BLOCK_MARKER));
    // A definite height, after min/max, is the children's percentage basis.
    let child_space = ConstraintSpace {
        available_inline: metrics.content_width,
        // Anonymous blocks are skipped when finding the percentage
        // containing block (CSS 2.1 10.5).
        percentage_block: if anonymous_block {
            space.percentage_block
        } else if space.fixed_block_indefinite && matches!(sized, Some((_, Some(_)))) {
            None
        } else {
            specified_height.or(ratio_height).map(clamp_h)
        },
        viewport_w: space.viewport_w,
        viewport_h: space.viewport_h,
        rtl: style.direction == TextDirection::Rtl,
        bfc_x: 0.0,
        bfc_y: 0.0,
        margin_base: 0.0,
        margin_strut: MarginStrut::default(),
        forced_bfc: false,
        adjoining_from: 0,
        fixed_block_indefinite: false,
        percent_unresolved: false,
    };

    // CSS Sizing 4 stretch-fit sizing: a child's `stretch` sizes resolve
    // against this box's content box. Block margins that would collapse
    // with ours are ignored for the block size.
    let stretch_styles = stretch_child_styles(
        flat,
        index,
        &child_space,
        !bfc_root && bt == 0.0 && pt == 0.0,
        !bfc_root && bb == 0.0 && pb == 0.0,
    );
    let orig_flat = flat;
    let mut container_has_float = false;
    for_each_cb_child(flat, index, &mut |child| {
        container_has_float |= flat[child].style.float != Float::None;
    });
    let stretched_flat;
    let flat = if stretch_styles.is_empty() {
        flat
    } else {
        stretched_flat = flat_with_styles(flat, &stretch_styles);
        &stretched_flat[..]
    };
    let can_escape_top = !bfc_root && bt == 0.0 && pt == 0.0;
    // Past clearance our top is resolved: margins that would collapse
    // through it resolve at our border box instead.
    let forced = space.forced_bfc && !bfc_root;
    // Floats placed from here on while our top is unresolved adjoin it;
    // a resolved top (border, padding, BFC root, clearance) starts anew.
    let adjoining_from = if can_escape_top && !forced {
        space.adjoining_from
    } else {
        exclusions.len()
    };
    let mut start_strut = MarginStrut::default();
    start_strut.append(metrics.margin[0]);
    let mut pending = MarginStrut::default();
    let mut cursor = 0.0f32;
    let mut at_top = true; // no in-flow content placed yet
    // PreviousInflowPosition::self_collapsing_child_had_clearance.
    let mut last_cleared_self_collapsing = false;
    let mut children = Vec::new();
    // Offset of the content box from the border box.
    let content_top = bt + pt;
    let mut last_baseline = LastBaseline::NoLines;
    let mut first_baseline = LastBaseline::NoLines;
    let mut oof_candidates: Vec<OofCandidate> = Vec::new();

    if let Some((_, height)) = image_size {
        // The replaced content is the in-flow block size. No line box, so
        // the baseline stays the bottom margin edge (CSS 2.1 10.8.1). A
        // text field's baseline is its inner editor's line.
        cursor = height;
        at_top = false;
        if let ComponentKind::TextInput { .. } = info.kind
            && is_textarea(style)
        {
            // A textarea's first baseline is its inner editor's first
            // line, at the content-box top (Chrome: a flex item at
            // border + padding + ascent, also when empty). As a scroll
            // container its last baseline is synthesized from the margin
            // box end (set below), so an inline textarea sits on its
            // bottom margin edge, as do inline-blocks that end with one.
            let line = textarea_line_height(style);
            first_baseline = LastBaseline::At(
                content_top + crate::layout::inline_font_baseline_from_line_top(style, line),
            );
        } else if let ComponentKind::TextInput { .. } = info.kind {
            let baseline = text_field_baseline(style, height).why("text-input:size")?;
            last_baseline = LastBaseline::At(content_top + baseline);
            first_baseline = last_baseline;
        }
    } else if let ComponentKind::Text { content } = info.kind {
        // Block-level text leaf: in-flow content of its own.
        let (height, baseline) = inline::block_text_layout(content, metrics.content_width, style);
        cursor = height;
        last_baseline = baseline.offset(content_top);
        first_baseline = last_baseline;
        at_top = false;
    } else if button && inline::rendered_children(flat, index).next().is_none() {
        // A button leaf: its label is one line of block text (paint
        // draws it on one line), or nothing for an empty button.
        if let Some(label) = button_leaf_label(flat, index) {
            let (height, baseline) = inline::block_text_layout(label, metrics.content_width, style);
            if !matches!(baseline, LastBaseline::At(_)) {
                return super::bail("button:wrap");
            }
            cursor = height;
            last_baseline = baseline.offset(content_top);
            first_baseline = last_baseline;
        }
        at_top = false;
    } else if inline::is_inline_container(flat, index) {
        // NGLineLayoutOpportunity: line boxes avoid the BFC's floats.
        let floats = (!exclusions.is_empty()).then_some(inline::LineFloats {
            exclusions: &*exclusions,
            x: bfc_x + bl + pl,
            y: bfc_y + bt + pt,
        });
        let result = inline::layout_inline_beside(flat, index, &child_space, floats).why("ifc")?;
        cursor = result.height;
        last_baseline = result.last_baseline.offset(content_top);
        first_baseline = result.first_baseline.offset(content_top);
        at_top = result.children.is_empty();
        children.extend(
            result
                .children
                .into_iter()
                .map(|(x, y, fragment)| (bl + pl + x, bt + pt + y, fragment)),
        );
    }
    let mut child = if image_size.is_some() || inline::is_inline_container(flat, index) {
        info.subtree_end
    } else {
        index + 1
    };
    while child < info.subtree_end {
        let child_info = &flat[child];
        let next = child_info.subtree_end.max(child + 1);
        if child_info.style.display == Display::None {
            child = next;
            continue;
        }
        if oof::is_out_of_flow(child_info.style) {
            // NGBlockLayoutAlgorithm::HandleOutOfFlowPositioned: the static
            // position is the content edge at the current block offset,
            // past the pending margins unless they may still collapse
            // through our top (then it is our top edge).
            let block = if at_top && can_escape_top {
                0.0
            } else {
                cursor + pending.sum()
            };
            let content_width = metrics.content_width;
            let inline_offset = if inline_level_oof(flat, child) {
                inline_static_offset(
                    info.style,
                    exclusions,
                    (bfc_x + bl + pl, bfc_y + bt + pt),
                    block,
                    content_width,
                    child_space.rtl,
                )
                .why("oof:inline-static-offset")?
            } else {
                0.0
            };
            oof_candidates.push(OofCandidate {
                index: child,
                static_inline: if child_space.rtl {
                    bl + pl + content_width - inline_offset
                } else {
                    bl + pl + inline_offset
                },
                static_block: bt + pt + block,
                static_rtl: child_space.rtl,
            });
            child = next;
            continue;
        }
        let content_bfc_y = bfc_y + bt + pt;
        let group = is_float_group(child_info.style);
        if group || child_info.style.float != Float::None {
            // PositionFloat: at the current block offset past the pending
            // margins (which do not collapse with the float's), unless
            // they may still collapse through our top.
            let block = if at_top && can_escape_top {
                0.0
            } else {
                cursor + pending.sum()
            };
            let floats: Vec<usize> = if group {
                inline::rendered_children(flat, child).collect()
            } else {
                vec![child]
            };
            let mut placed = Vec::with_capacity(floats.len());
            for float in floats {
                let float_style = flat[float].style;
                let mut block_start = content_bfc_y + block;
                if let Some(clear) = clearance_offset(float_style, exclusions) {
                    block_start = block_start.max(clear);
                }
                let (fx, fy, result) = layout_float(
                    flat,
                    float,
                    &child_space,
                    exclusions,
                    bfc_x + bl + pl,
                    block_start,
                )
                .why("float-layout")?;
                let (dx, dy) = oof::relative_offset(float_style, &child_space);
                let (fx, fy) = (fx - bfc_x + dx, fy - bfc_y + dy);
                oof_candidates.extend(result.oof.iter().map(|c| c.translated(fx, fy)));
                placed.push((fx, fy, result.fragment));
            }
            if group {
                // The synthetic row's own box: the anonymous group spans
                // the content box (width: 100%), a cleared wrapper
                // shrink-wraps its floats; both start at the first float.
                let x0 = bl + pl;
                let y0 = placed
                    .iter()
                    .map(|p| p.1)
                    .reduce(f32::min)
                    .unwrap_or(bt + pt + block);
                let right = placed
                    .iter()
                    .map(|p| p.0 + p.2.width)
                    .reduce(f32::max)
                    .unwrap_or(x0);
                let bottom = placed
                    .iter()
                    .map(|p| p.1 + p.2.height)
                    .reduce(f32::max)
                    .unwrap_or(y0);
                let width = if child_info.style.float == Float::None {
                    metrics.content_width
                } else {
                    (right - x0).max(0.0)
                };
                children.push((
                    x0,
                    y0,
                    BoxFragment {
                        index: child,
                        text_range: None,
                        ellipsis: false,
                        width,
                        height: (bottom - y0).max(0.0),
                        children: placed
                            .into_iter()
                            .map(|(x, y, f)| (x - x0, y - y0, f))
                            .collect(),
                    },
                ));
            } else {
                children.extend(placed);
            }
            child = next;
            continue;
        }
        // A definite top-level inline-grid is an atomic inline (CSS Grid
        // §3). Consecutive ones share a line; a flex or grid item is
        // blockified and takes the grid path above instead.
        if is_inline_grid_line(flat, child) {
            if !exclusions.is_empty() || child_space.rtl {
                return super::bail("grid:inline");
            }
            let mut run = vec![child];
            let mut scan = next;
            while scan < info.subtree_end {
                let scan_info = &flat[scan];
                let scan_next = scan_info.subtree_end.max(scan + 1);
                if scan_info.style.display == Display::None {
                    scan = scan_next;
                    continue;
                }
                if !is_inline_grid_line(flat, scan) {
                    break;
                }
                run.push(scan);
                scan = scan_next;
            }
            let y = if at_top && can_escape_top {
                start_strut.merge(pending);
                pending = MarginStrut::default();
                0.0
            } else {
                let y = cursor + pending.sum();
                pending = MarginStrut::default();
                y
            };
            let available = metrics.content_width;
            let mut line_y = y;
            let mut index = 0;
            while index < run.len() {
                let mut line = Vec::new();
                let mut line_x = 0.0f32;
                while index < run.len() {
                    let item = run[index];
                    let result = layout_block_in(flat, item, &child_space, false, None, exclusions)
                        .why("inline-grid")?;
                    let width = result.fragment.width;
                    if line_x > 0.0 && line_x + width > available + 1.0 / 128.0 {
                        break;
                    }
                    float_overlap |= result.float_overlap;
                    line_x += width;
                    line.push(result);
                    index += 1;
                }
                // `vertical-align: baseline` (the initial value). The
                // ascent is the grid's first baseline, or the bottom
                // margin edge when the grid has no items (CSS Grid §10,
                // CSS 2.1 10.8.1).
                let ascent = line.iter().map(inline_grid_line_ascent).fold(0.0, f32::max);
                let mut placed_x = 0.0f32;
                let mut line_h = 0.0f32;
                for result in line {
                    let shift = (ascent - inline_grid_line_ascent(&result)).max(0.0);
                    let width = result.fragment.width;
                    let height = result.fragment.height;
                    let (dx, dy) =
                        oof::relative_offset(flat[result.fragment.index].style, &child_space);
                    let (fx, fy) = (bl + pl + placed_x + dx, bt + pt + line_y + shift + dy);
                    oof_candidates.extend(result.oof.iter().map(|c| c.translated(fx, fy)));
                    if result.last_baseline != LastBaseline::NoLines {
                        last_baseline = result.last_baseline.offset(bt + pt + line_y + shift);
                    }
                    children.push((fx, fy, result.fragment));
                    placed_x += width;
                    line_h = line_h.max(shift + height);
                }
                line_y += line_h;
            }
            cursor = line_y;
            at_top = false;
            child = scan;
            continue;
        }
        // A replaced child has a definite content width (10.3.4) even when
        // its CSS width is auto, so auto margins center against that width.
        let child_metrics = if let ComponentKind::Image { src } = child_info.kind {
            let (width, _) =
                replaced_content_size(src, child_info.style, &child_space).why("image")?;
            let mut definite = child_info.style.clone();
            definite.width = Dimension::Px(width);
            definite.min_width = Dimension::Auto;
            definite.max_width = Dimension::Auto;
            definite.box_sizing = BoxSizing::ContentBox;
            resolve_inline(&definite, &child_space)
        } else if matches!(
            child_info.kind,
            ComponentKind::Canvas { .. }
                | ComponentKind::TextInput { .. }
                | ComponentKind::SvgDocument { .. }
        ) {
            let (width, _) = replaced_size(child_info.kind, child_info.style, &child_space)?;
            let mut definite = child_info.style.clone();
            definite.width = Dimension::Px(width);
            definite.min_width = Dimension::Auto;
            definite.max_width = Dimension::Auto;
            definite.box_sizing = BoxSizing::ContentBox;
            resolve_inline(&definite, &child_space)
        } else if is_html_button(child_info) && child_info.style.width == Dimension::Auto {
            // A block-level button is fit-content wide; auto margins are
            // then solved against that width (as for a replaced box).
            let auto = resolve_inline(child_info.style, &child_space);
            let width =
                shrink_to_fit(flat, child, &child_space, &auto).why("button:shrink-to-fit")?;
            let mut definite = child_info.style.clone();
            definite.width = Dimension::Px(width);
            definite.min_width = Dimension::Auto;
            definite.max_width = Dimension::Auto;
            definite.box_sizing = BoxSizing::ContentBox;
            resolve_inline(&definite, &child_space)
        } else if let ComponentKind::SvgDocument { source, .. } = child_info.kind {
            let (width, _) =
                svg_content_size(source, child_info.style, &child_space).why("kind:other")?;
            let mut definite = child_info.style.clone();
            definite.width = Dimension::Px(width);
            definite.min_width = Dimension::Auto;
            definite.max_width = Dimension::Auto;
            definite.box_sizing = BoxSizing::ContentBox;
            resolve_inline(&definite, &child_space)
        } else {
            resolve_inline(child_info.style, &child_space)
        };
        let x = bl + pl + child_metrics.margin[3];
        // CSS2 9.5.2: clearance places the border box at the clear offset
        // when the hypothetical position is above it; the cleared box's
        // top margin then no longer collapses with what precedes it.
        let clear = clearance_offset(child_info.style, exclusions);
        // Blink NGBlockLayoutAlgorithm::HandleInflow: with clearance past
        // adjoining floats (HasClearancePastAdjoiningFloats) our BFC block
        // offset resolves first, at the margin strut so far and without
        // the child's margin (ResolveBfcBlockOffset(previous_inflow_
        // position)), which is where those floats already sit; the child
        // then goes exactly to the clearance offset (forced BFC block
        // offset, is_pushed_by_floats): its margins have no effect.
        let mut forced_clearance = None;
        if let Some(c) = clear.filter(|_| at_top && can_escape_top && !forced) {
            let (left, right) = match child_info.style.clear {
                Clear::Left => (true, false),
                Clear::Right => (false, true),
                _ => (true, true),
            };
            if exclusions.has_float_since(adjoining_from, left, right) {
                start_strut.merge(pending);
                pending = MarginStrut::default();
                cursor = 0.0;
                at_top = false;
                forced_clearance = Some(c);
            }
        }
        let place = |start: MarginStrut| {
            if let Some(c) = forced_clearance {
                return Some((c - content_bfc_y, true));
            }
            if at_top && can_escape_top {
                // The margins still collapse through us: the hypothetical
                // position is past all of them.
                let mut combined = margin_strut;
                combined.merge(start_strut);
                combined.merge(pending);
                combined.merge(start);
                let top = if forced {
                    content_bfc_y
                } else {
                    margin_base + combined.sum()
                };
                return match clear {
                    Some(c) if top < c - 1.0 / 128.0 => None,
                    _ => Some((0.0, false)),
                };
            }
            let mut combined = pending;
            combined.merge(start);
            let hypothetical = cursor + combined.sum();
            Some(match clear {
                Some(c) if content_bfc_y + hypothetical < c - 1.0 / 128.0 => {
                    (c - content_bfc_y, true)
                }
                _ => (hypothetical, false),
            })
        };
        // NGBlockLayoutAlgorithm estimates the child's BFC block offset
        // from its own margin-top, lays it out, and relayouts it when the
        // resolved offset differs and the layout depended on it.
        let mut own_top = MarginStrut::default();
        own_top.append(child_metrics.margin[0]);
        // (Clearance separating a first child from margins that would
        // collapse through us is not implemented.)
        let (estimate, estimate_cleared) = place(own_top).why("place-estimate")?;
        let snapshot = exclusions.snapshot();
        let mut nested = child_space;
        nested.bfc_x = bfc_x + x;
        nested.bfc_y = content_bfc_y + estimate;
        // Margins collapsing through us: the child (and we) end up past
        // all of them, wherever our own estimate put us.
        let collapsed_top = |start: MarginStrut| {
            if forced {
                return content_bfc_y;
            }
            let mut combined = margin_strut;
            combined.merge(start_strut);
            combined.merge(pending);
            combined.merge(start);
            margin_base + combined.sum()
        };
        if at_top && can_escape_top {
            nested.bfc_y = collapsed_top(own_top);
            nested.margin_base = margin_base;
            nested.margin_strut = margin_strut;
            nested.margin_strut.merge(start_strut);
            nested.margin_strut.merge(pending);
            nested.forced_bfc = forced;
            nested.adjoining_from = adjoining_from;
        } else {
            nested.margin_base = content_bfc_y + cursor;
            nested.margin_strut = pending;
            nested.forced_bfc = estimate_cleared;
            nested.adjoining_from = exclusions.len();
        }
        let mut result =
            layout_block_in(flat, child, &nested, false, None, exclusions).why("child-layout")?;
        let (mut resolved, mut cleared) = place(result.start_strut).why("place")?;
        let resolved_bfc = |resolved: f32, start: MarginStrut| {
            if at_top && can_escape_top {
                collapsed_top(start)
            } else {
                content_bfc_y + resolved
            }
        };
        let resolved_forced = if at_top && can_escape_top {
            forced
        } else {
            cleared
        };
        if result.bfc_dependent
            && ((resolved_bfc(resolved, result.start_strut) - nested.bfc_y).abs() > 1.0 / 128.0
                || resolved_forced != nested.forced_bfc)
        {
            exclusions.restore(snapshot);
            nested.bfc_y = resolved_bfc(resolved, result.start_strut);
            nested.forced_bfc = resolved_forced;
            result = layout_block_in(flat, child, &nested, false, None, exclusions)
                .why("child-relayout")?;
            let (again, c) = place(result.start_strut).why("place-relayout")?;
            if (again - resolved).abs() > 1.0 / 128.0 {
                return super::bail("relayout-mismatch");
            }
            (resolved, cleared) = (again, c);
        }
        let mut x = x;
        // A BFC root whose inline `stretch` was left unresolved because a
        // float in this container may take part of the line (see
        // stretch_child_styles): its width comes from the opportunity.
        let stretch_tokens = marker(child_info.style, STRETCH).filter(|tokens| {
            container_has_float
                && child_info.style.float == Float::None
                && is_bfc_root(child_info.style)
                && tokens
                    .split(',')
                    .any(|t| matches!(t, "w" | "minw" | "maxw"))
        });
        if (is_bfc_root(child_info.style) && !exclusions.is_empty()) || stretch_tokens.is_some() {
            // NGBlockLayoutAlgorithm::HandleNewFormattingContext: the BFC
            // root's border box goes into the first layout opportunity at
            // or below its position where it fits for its whole height.
            let style = child_info.style;
            // A non-zero margin on an overflow BFC beside a float still
            // disagrees with legacy (the margin must not clear the float,
            // and an adjoining margin must separate like clearance).
            let zero_margin =
                |spacing: Spacing| matches!(spacing, Spacing::Px(0.0) | Spacing::Auto);
            if non_visible_overflow(style)
                && style.display != Display::FlowRoot
                && !is_author_flex(style)
                && !is_author_grid(style)
                && !(zero_margin(style.margin.top)
                    && zero_margin(style.margin.right)
                    && zero_margin(style.margin.bottom)
                    && zero_margin(style.margin.left))
            {
                return super::bail("overflow-bfc-margin");
            }
            if child_space.rtl
                || matches!(style.margin.left, Spacing::Auto)
                || matches!(style.margin.right, Spacing::Auto)
            {
                return super::bail("bfc-root-float-margins");
            }
            let m = &child_metrics;
            let edges = m.border[1] + m.border[3] + m.padding[1] + m.padding[3];
            // Authored margins: the used ones absorb the over-constraint
            // of CSS2 10.3.3, which the opportunity replaces.
            let authored = |s: Spacing| {
                resolve_spacing_for_layout(
                    s,
                    child_space.available_inline,
                    style,
                    child_space.viewport_w,
                    child_space.viewport_h,
                )
            };
            let (ml, mr) = (authored(style.margin.left), authored(style.margin.right));
            let cb_x = bfc_x + bl + pl;
            // Margins may overlap floats; the border box may not.
            let line_start = cb_x + ml;
            let line_end = cb_x + metrics.content_width - mr;
            let hypothetical = resolved_bfc(resolved, result.start_strut);
            let mut top = hypothetical;
            let mut height = result.fragment.height;
            let mut found = None;
            'opportunities: for _ in 0..64 {
                for _ in 0..4 {
                    let (l, r) = exclusions.band(top, height);
                    let narrowed = l.is_some() || r.is_some();
                    let start = l.map_or(line_start, |l| l.max(line_start));
                    let end = r.map_or(line_end, |r| r.min(line_end));
                    let opportunity = (end - start - edges).max(0.0);
                    let content = if let Some(tokens) = stretch_tokens {
                        let orig = orig_flat[child].style;
                        let has = |t: &str| tokens.split(',').any(|x| x == t);
                        let mut c = if has("w") {
                            clamp_inline(orig, &child_space, edges, opportunity)
                        } else {
                            match orig.width {
                                Dimension::Auto => {
                                    clamp_inline(orig, &child_space, edges, opportunity)
                                }
                                Dimension::Px(w) => match orig.box_sizing {
                                    BoxSizing::BorderBox => (w - edges).max(0.0),
                                    BoxSizing::ContentBox => w,
                                },
                                _ => return super::bail("stretch-bfc-width"),
                            }
                        };
                        if has("minw") {
                            c = c.max(opportunity);
                        }
                        if has("maxw") {
                            c = c.min(opportunity);
                        }
                        c
                    } else if style.width == Dimension::Auto {
                        clamp_inline(style, &child_space, edges, (end - start - edges).max(0.0))
                    } else {
                        m.content_width
                    };
                    if narrowed && start + content + edges > end + 1.0 / 128.0 {
                        break;
                    }
                    let fitted = layout_block_in(
                        flat,
                        child,
                        &nested,
                        false,
                        Some((content, None)),
                        exclusions,
                    )
                    .why("bfc-root-fit")?;
                    // A table can refuse the opportunity width when its
                    // min-content is wider. The border box must not overlap
                    // the float, so try the next band.
                    if narrowed && start + fitted.fragment.width > end + 1.0 / 128.0 {
                        break;
                    }
                    if (fitted.fragment.height - height).abs() <= 1.0 / 128.0 {
                        found = Some((start, fitted));
                        break 'opportunities;
                    }
                    // A different height covers different floats: retry.
                    height = fitted.fragment.height;
                }
                top = exclusions
                    .next_bottom(top, height)
                    .why("bfc-root-no-opportunity")?;
            }
            let (start, fitted) = found.why("bfc-root-no-opportunity")?;
            if top > hypothetical + 1.0 / 128.0 {
                if at_top && can_escape_top {
                    // The margins still collapse through us at the
                    // hypothetical position (our top); the box sits below.
                    start_strut.merge(pending);
                    start_strut.merge(fitted.start_strut);
                    pending = MarginStrut::default();
                    resolved = top - hypothetical;
                } else {
                    resolved = top - content_bfc_y;
                }
                // Moved past floats: like clearance, the margin above no
                // longer collapses with the box.
                cleared = true;
            }
            x = start - bfc_x;
            result = fitted;
        }
        float_overlap |= result.float_overlap;
        // Paint-wrapped block text and BFC roots must avoid floats too
        // (IFC line boxes use their line opportunities); fall back.
        let child_style = child_info.style;
        if !exclusions.is_empty()
            && result.fragment.height > 0.0
            && (matches!(child_info.kind, ComponentKind::Text { .. })
                || is_bfc_root(child_style)
                || is_html_button(child_info))
            && exclusions.overlaps(
                bfc_x + x,
                content_bfc_y + resolved,
                bfc_x + x + result.fragment.width,
                content_bfc_y + resolved + result.fragment.height,
            )
        {
            float_overlap = true;
        }
        let y;
        // Blink self_or_sibling_self_collapsing_child_had_clearance: the flag
        // survives later self-collapsing siblings.
        let sibling_had_clearance = last_cleared_self_collapsing;
        last_cleared_self_collapsing = false;
        if cleared && result.self_collapsing {
            // Blink FinishInflow/ComputeInflowPosition: a self-collapsing
            // child pushed down by clearance sits at the clearance offset;
            // its margins are separated from the preceding strut (relayout
            // with only its own margin) and the inflow position becomes
            // that offset minus its end margin strut, so the margins after
            // it collapse with its own (CSS2 9.5.2 negative clearance).
            if at_top && can_escape_top {
                start_strut.merge(pending);
                at_top = false;
            }
            // Clearance uses the child's end margin strut before its own
            // block-end margin is appended (that is our start strut).
            y = resolved;
            cursor = y - result.start_strut.sum();
            pending = result.start_strut;
            pending.merge(result.end_strut);
            last_cleared_self_collapsing = true;
        } else if cleared {
            if at_top && can_escape_top {
                start_strut.merge(pending);
            }
            y = resolved;
            cursor = y + result.fragment.height;
            pending = result.end_strut;
            at_top = false;
        } else if result.self_collapsing {
            last_cleared_self_collapsing = sibling_had_clearance;
            let mut combined = pending;
            combined.merge(result.start_strut);
            if at_top && can_escape_top {
                y = 0.0;
                start_strut.merge(combined);
                start_strut.merge(result.end_strut);
            } else {
                y = cursor + combined.sum();
                pending.merge(result.start_strut);
                pending.merge(result.end_strut);
            }
        } else if at_top && can_escape_top {
            start_strut.merge(pending);
            start_strut.merge(result.start_strut);
            y = 0.0;
            cursor = result.fragment.height;
            pending = result.end_strut;
            at_top = false;
        } else {
            let mut combined = pending;
            combined.merge(result.start_strut);
            y = cursor + combined.sum();
            cursor = y + result.fragment.height;
            pending = result.end_strut;
            at_top = false;
        }
        // A scroll container block child exposes its bottom margin edge as
        // its baseline, whatever lines it holds (css/CSS2/linebox/
        // baseline-block-with-overflow-001, measured in Chrome).
        let (child_last, child_first) = if baseline_is_margin_edge(child_info.style)
            && !matches!(child_info.kind, ComponentKind::Button { .. })
        {
            let edge = LastBaseline::At(result.fragment.height + child_metrics.margin[2]);
            // The first baseline stays the content's (a flex item's
            // baseline, align-items-baseline-overflow-non-visible).
            (edge, result.first_baseline)
        } else {
            (result.last_baseline, result.first_baseline)
        };
        if child_last != LastBaseline::NoLines {
            last_baseline = child_last.offset(bt + pt + y);
        }
        // A fieldset's rendered legend sits outside its anonymous content
        // box, whose baseline is the fieldset's (Blink
        // FieldsetLayoutAlgorithm).
        if !matches!(first_baseline, LastBaseline::At(_)) && !is_rendered_legend(child_info.style) {
            let candidate = match child_first {
                LastBaseline::At(_) => child_first,
                _ => child_last,
            };
            if matches!(candidate, LastBaseline::At(_)) {
                first_baseline = candidate.offset(bt + pt + y);
            }
        }
        // A shrink-to-fit table solves auto horizontal margins after its
        // used border box is known (CSS 2.1 10.3.3). `resolve_inline`
        // treats width:auto as filling the containing block.
        if matches!(
            child_info.style.display,
            Display::Table | Display::InlineTable
        ) && (child_info.style.margin.left == Spacing::Auto
            || child_info.style.margin.right == Spacing::Auto)
        {
            let auto_left = child_info.style.margin.left == Spacing::Auto;
            let auto_right = child_info.style.margin.right == Spacing::Auto;
            let used = result.fragment.width;
            let specified_left = if auto_left {
                0.0
            } else {
                child_metrics.margin[3]
            };
            let specified_right = if auto_right {
                0.0
            } else {
                child_metrics.margin[1]
            };
            let extra = child_space.available_inline - used - specified_left - specified_right;
            let left = if child_space.rtl {
                match (auto_right, auto_left) {
                    (true, true) => layout_unit((extra / 2.0).max(0.0)),
                    (true, false) => specified_left,
                    (false, true) => (specified_left + extra).max(0.0),
                    (false, false) => child_metrics.margin[3],
                }
            } else {
                match (auto_left, auto_right) {
                    (true, true) => layout_unit((extra / 2.0).max(0.0)),
                    (true, false) => extra.max(0.0),
                    (false, true) => specified_left,
                    (false, false) => child_metrics.margin[3],
                }
            };
            x = bl + pl + left;
        }
        // CSS2 9.4.3: a relative offset moves the fragment (and what it
        // carries), not the flow.
        let (dx, dy) = oof::relative_offset(child_info.style, &child_space);
        let (fx, fy) = (x + dx, bt + pt + y + dy);
        oof_candidates.extend(result.oof.iter().map(|c| c.translated(fx, fy)));
        children.push((fx, fy, result.fragment));
        child = next;
    }

    // CSS2.1 8.3.1: the last child's bottom margin adjoins ours when our
    // height is auto (min-height only matters for collapsing through).
    // Blink FinishLayout: after a self-collapsing child affected by
    // clearance the end strut is part of our size and does not escape.
    let can_escape_bottom = !bfc_root
        && bb == 0.0
        && pb == 0.0
        && specified_height.is_none()
        && !last_cleared_self_collapsing
        && !size_contained(style);

    let mut end_strut = MarginStrut::default();
    let auto_content = if can_escape_bottom {
        cursor
    } else {
        // A negative end strut can pull the block end up (never below 0).
        (cursor + pending.sum()).max(0.0)
    };
    if bfc_root && float_overlap {
        // (Line boxes or a BFC root beside a float.)
        return super::bail("float-overlap");
    }
    // CSS2 10.6.7: a BFC root's auto height includes its floats.
    let auto_content = match exclusions.max_bottom() {
        Some(bottom) if bfc_root => auto_content.max(bottom - bt - pt),
        _ => auto_content,
    };
    // Size containment: the auto block size is the contain-intrinsic one.
    let auto_content = if size_contained(style) {
        contain_intrinsic_size(style).1
    } else {
        auto_content
    };
    let content_height = match (specified_height, ratio_height) {
        (Some(height), _) => clamp_h(height),
        // css-sizing-4: the automatic minimum block size of a box with a
        // preferred aspect ratio is its content size (capped by max-height)
        // unless it is a scroll container or min-height is not auto.
        (None, Some(height)) => {
            let floor = if min_h.is_none() && !non_visible_overflow(style) {
                max_h.map_or(auto_content, |max| auto_content.min(max))
            } else {
                0.0
            };
            clamp_h(height.max(floor))
        }
        (None, None) => clamp_h(auto_content),
    };
    // css-sizing-3: in a block container's block axis min-content,
    // max-content and fit-content all mean its content (auto) height, so
    // a keyword max-height caps the height there and a keyword min-height
    // floors it (min wins over max). A replaced box and a block size the
    // parent's algorithm already fixed (flex / grid items) keep theirs.
    let keyword_limits = image_size.is_none() && !matches!(sized, Some((_, Some(_))));
    let content_height = if keyword_limits && marker(style, MAX_HEIGHT_KEYWORD).is_some() {
        content_height.min(auto_content)
    } else {
        content_height
    };
    let content_height = if keyword_limits && marker(style, MIN_HEIGHT_KEYWORD).is_some() {
        content_height.max(auto_content)
    } else {
        content_height
    };
    // CSS2.1 8.3.1: the last child's bottom margin adjoins ours when our
    // computed height is auto, whatever max-height does to the used height
    // (margin-collapse-038; Blink's FinishLayout drops the strut whenever
    // the used size differs from the intrinsic one).  A min-height that
    // grows the box does move its end away from the child's margin edge
    // (CSS2 min-height-separates-margin).
    if can_escape_bottom && content_height <= auto_content {
        end_strut = pending;
    }
    // CSS2.1 8.3.1 collapse-through: no top/bottom border or padding, not a
    // BFC root, no in-flow content, and a zero *used* block size.  Blink
    // (NGBlockLayoutAlgorithm::FinishLayout) leaves the BFC block offset
    // unresolved exactly when the final block size is zero, so
    // `height: 100px; max-height: 0` collapses through like `height: 0`
    // (CSS2 max-height-100), while min-height > 0 keeps it from doing so.
    let self_collapsing =
        at_top && can_escape_top && !bfc_root && bb == 0.0 && pb == 0.0 && content_height == 0.0;
    if self_collapsing {
        // CSS2 8.3.1: top and bottom margins adjoin; everything collapses
        // into one strut reported as the start strut.
        start_strut.merge(pending);
        end_strut = MarginStrut::default();
        end_strut.append(metrics.margin[2]);
    } else {
        end_strut.append(metrics.margin[2]);
    }
    if !can_escape_top {
        // Children's margins were resolved inside; only our own margin
        // participates outside.
        let mut own = MarginStrut::default();
        own.append(metrics.margin[0]);
        start_strut = own;
    }

    if button {
        // HTML §15.5.3 / Blink LayoutButton: the anonymous content box is
        // centered in the block axis when it fits (safe alignment, like
        // its auto margins); an overflowing one starts at the top. The
        // baseline is the content's last line, else the content-box
        // bottom, whatever the overflow.
        let shift = ((content_height - auto_content) / 2.0).max(0.0);
        for placed in &mut children {
            placed.1 += shift;
        }
        for candidate in &mut oof_candidates {
            *candidate = candidate.translated(0.0, shift);
        }
        let bottom = LastBaseline::At(content_top + content_height);
        let centered = |baseline: LastBaseline| match baseline {
            LastBaseline::NoLines => bottom,
            other => other.offset(shift),
        };
        last_baseline = centered(last_baseline);
        first_baseline = centered(first_baseline);
    }
    let width = metrics.content_width + bl + br + pl + pr;
    let height = content_height + bt + bb + pt + pb;
    if matches!(info.kind, ComponentKind::TextInput { .. }) && is_textarea(style) {
        last_baseline = LastBaseline::At(height + metrics.margin[2]);
    }
    if style.position != Position::Static && !oof_candidates.is_empty() {
        // A positioned box is the containing block of its absolute
        // descendants: its padding box (CSS2 10.1 item 4).
        let cb = ContainingBlock {
            x: bl,
            y: bt,
            width: width - bl - br,
            height: height - bt - bb,
            rtl: oof::direction_rtl(style),
        };
        let (placed, rest) = oof::resolve(
            flat,
            std::mem::take(&mut oof_candidates),
            &cb,
            false,
            space.viewport_w,
            space.viewport_h,
        )
        .why("oof-resolve")?;
        children.extend(placed);
        oof_candidates = rest;
    }
    if !matches!(first_baseline, LastBaseline::At(_))
        && matches!(last_baseline, LastBaseline::At(_))
    {
        first_baseline = last_baseline;
    }
    // A box that clips its overflow synthesizes its last baseline from its
    // block-end margin edge (CSS 2.1 10.8.1 for an inline-block; Chrome
    // does the same for such a block inside one, so an inline-block ending
    // with it sits on that block's bottom margin edge). The first baseline
    // stays the content's. Form controls such as buttons keep their
    // content baseline.
    if non_visible_overflow(style)
        && style.display.establishes_overflow_clip()
        && matches!(
            info.kind,
            ComponentKind::Box | ComponentKind::Column | ComponentKind::Row
        )
    {
        last_baseline = LastBaseline::At(height + metrics.margin[2]);
    }
    Some(LayoutResult {
        last_baseline,
        first_baseline,
        fragment: BoxFragment {
            index,
            text_range: None,
            ellipsis: false,
            width,
            height,
            children,
        },
        start_strut,
        end_strut,
        self_collapsing,
        oof: oof_candidates,
        bfc_dependent: !bfc_root && (entry_exclusions || !exclusions.is_empty()),
        float_overlap,
    })
}
