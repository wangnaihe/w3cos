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
const WIDTH_KEYWORD: &str = "--w3cos-internal-width-keyword";

fn marker<'a>(style: &'a Style, key: &str) -> Option<&'a str> {
    style
        .custom_properties
        .as_ref()
        .and_then(|p| p.get(key))
        .map(String::as_str)
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
            | "document-element"
            | "size-containment"
            | "contain-intrinsic-size"
            // Grid reads these. The cascade leaves the legacy fields alone:
            // height stays the dimension it already stored, and justify-content
            // stays the enum flex already uses.
            | "height-keyword"
            | "justify-content"
            // Grid-lanes stays `display: flex` for legacy. NG reads the
            // marker and lays the box out as a grid. Percent gaps are not
            // stored; only a grid container consults the marker.
            | "grid-lanes"
            | "percent-gap"
    ) || name.starts_with("decoration-owner")
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
    if !atomic
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
    // (position_reason); atomics and sticky stay on legacy.
    if style.position != Position::Static && (atomic || style.position == Position::Sticky) {
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
        return Some("overflow");
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
pub(super) fn unsupported_marker(style: &Style, allowed: &[&str]) -> Option<&'static str> {
    style
        .custom_properties
        .as_ref()
        .is_some_and(|props| {
            props.keys().any(|key| {
                key.starts_with("--w3cos-internal")
                    && !is_value_carrier_marker(key)
                    && !allowed.contains(&key.as_str())
                    // Intrinsic width keywords are resolved under size
                    // containment; elsewhere width stays auto, as before
                    // the cascade kept the keyword (legacy does the same).
                    // TODO: Blink ComputeInlineSizeForFragment keywords.
                    && key != WIDTH_KEYWORD
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

/// Floats of block-level boxes in a block container (S3b).  Floats among
/// inline content (NGInlineLayoutAlgorithm's float handling) and text
/// that would have to wrap around a float stay on legacy.
fn float_reason(flat: &[FlatNodeInfo], index: usize) -> Option<&'static str> {
    let info = &flat[index];
    let style = info.style;
    if style.float == Float::None || oof::is_out_of_flow(style) {
        return None;
    }
    if inline::is_atomic_inline(flat, index) || style.display == Display::Inline {
        return Some("float:inline");
    }
    if !matches!(
        style.display,
        Display::Block | Display::FlowRoot | Display::Table
    ) {
        return Some("float:display");
    }
    let Some(parent) = info.parent else {
        return Some("float:root");
    };
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
            space.available_inline,
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
        Position::Static | Position::Sticky => None,
        _ if inline::is_atomic_inline(flat, index) || style.display == Display::Inline => {
            Some("position:inline")
        }
        Position::Relative => None,
        Position::Absolute | Position::Fixed => {
            if !matches!(style.display, Display::Block | Display::FlowRoot) {
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
    if inline::is_atomic_inline(flat, index) {
        if !inline::in_inline_container(flat, index) {
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
                // `run-in` and `-webkit-box` compute to `display: flex` for
                // legacy. A `run-in` box whose flex line this stage can lay
                // out uses that flex box (the applies-to squares). A run-in
                // the flex algorithm rejects keeps the unrecognized marker,
                // so the page stays on legacy instead of changing only the
                // trace reason. `-webkit-box` stays on the marker: line-clamp
                // references still match only on legacy.
                let flex_reason = super::flex::container_reason(flat, index);
                let allowed: &[&str] = if is_run_in(info.style) && flex_reason.is_none() {
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
                // flex or grid item. A top-level `inline-grid` stays on
                // legacy: those boxes are the references of tests that
                // still fall back, and switching only the reference
                // splits the reftest.
                let blockified = is_inline_grid(info.style)
                    && info.parent.is_some_and(|parent| {
                        is_author_flex(flat[parent].style) || is_author_grid(flat[parent].style)
                    });
                let allowed: &[&str] = if blockified {
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
                || info
                    .parent
                    .is_some_and(|parent| is_author_grid(flat[parent].style)) =>
        {
            // A text grid item is an anonymous block. Its display stays
            // inline in the cascade; the grid lays it out as block text.
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
        _ => Some("kind:other"),
    }
}

/// Replaced boxes this stage lays out (CSS 2.1 10.3.2 inline, 10.3.4 block).
/// Floats, abspos, border-box, and flex/grid items stay on legacy: their
/// used size is not the content-box intrinsic size this path resolves.
fn image_box_reason(flat: &[FlatNodeInfo], index: usize) -> Option<&'static str> {
    let info = &flat[index];
    let style = info.style;
    if style.box_sizing == BoxSizing::BorderBox {
        return Some("image");
    }
    // min/max recompute the intrinsic ratio (CSS 2.1 10.4). That path does
    // not match the legacy used size yet.
    if !matches!(style.min_width, Dimension::Auto)
        || !matches!(style.max_width, Dimension::Auto)
        || !matches!(style.min_height, Dimension::Auto)
        || !matches!(style.max_height, Dimension::Auto)
    {
        return Some("image");
    }
    if style.float != Float::None
        || matches!(
            style.position,
            Position::Absolute | Position::Fixed | Position::Sticky
        )
    {
        return Some("image");
    }
    if info.parent.is_some_and(|parent| {
        is_author_flex(flat[parent].style) || is_author_grid(flat[parent].style)
    }) {
        return Some("image");
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
        return Some("image");
    }
    match style.display {
        Display::Block | Display::FlowRoot => {
            unsupported_box_style(style, &[inline::VALIGN_MARKER], false)
        }
        // An inline replaced box sits on the font strut. Legacy then runs
        // `project_single_baseline_replaced_image` and
        // `align_inline_block_last_line_baselines`. That line box is still
        // taller than the NG line, so inline images stay on legacy.
        Display::Inline | Display::InlineBlock => Some("image"),
        _ => Some("image"),
    }
}

/// Either axis computes to something other than visible (CSS2 11.1.1).
fn non_visible_overflow(style: &Style) -> bool {
    style.resolved_overflow_x() != Overflow::Visible
        || style.resolved_overflow_y() != Overflow::Visible
}

/// Overflow shapes that still disagree with the legacy engine: scrollports
/// (`scroll` / `auto`), a flex or grid item (automatic minimum), a negative
/// margin, or a float/clear inside the box. `hidden` on a block that only
/// establishes a BFC stays here.
fn overflow_reason(flat: &[FlatNodeInfo], index: usize) -> Option<&'static str> {
    let style = flat[index].style;
    if !non_visible_overflow(style) || !style.display.establishes_overflow_clip() {
        return None;
    }
    if flat[index].parent.is_some_and(|parent| {
        is_author_flex(flat[parent].style) || is_author_grid(flat[parent].style)
    }) {
        return Some("overflow");
    }
    let scrollish = |overflow: Overflow| matches!(overflow, Overflow::Scroll | Overflow::Auto);
    if scrollish(style.resolved_overflow_x()) || scrollish(style.resolved_overflow_y()) {
        return Some("overflow");
    }
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
        return Some("overflow");
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
        return Some("overflow");
    }
    None
}

/// CSS 2.1 10.3.2 / 10.3.4 content-box size. Percent, vw, and vh are
/// resolved against this constraint space; em/rem/ch and the intrinsic
/// ratio stay in `image_intrinsic_size` so the two engines share one
/// replaced-size function. `None` when a percentage has no basis.
fn replaced_content_size(src: &str, style: &Style, space: &ConstraintSpace) -> Option<(f32, f32)> {
    if style.box_sizing == BoxSizing::BorderBox {
        return super::bail("image");
    }
    let mut definite = style.clone();
    let sub = |dim: Dimension, basis: Option<f32>| -> Option<Dimension> {
        match dim {
            Dimension::Percent(_) | Dimension::Vw(_) | Dimension::Vh(_) => {
                resolve_dim(dim, basis, style, space).map(Dimension::Px)
            }
            other => Some(other),
        }
    };
    definite.width = sub(style.width, Some(space.available_inline))?;
    definite.height = sub(style.height, space.percentage_block)?;
    definite.min_width = sub(style.min_width, Some(space.available_inline))?;
    definite.max_width = sub(style.max_width, Some(space.available_inline))?;
    definite.min_height = sub(style.min_height, space.percentage_block)?;
    definite.max_height = sub(style.max_height, space.percentage_block)?;
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
    Some(crate::layout::image_intrinsic_size(
        src, &definite, containing,
    ))
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

/// Every distinct reason in the tree (trace mode only); internal markers
/// are reported by name.
pub(super) fn unsupported_reasons(flat: &[FlatNodeInfo]) -> Vec<String> {
    let mut reasons = Vec::new();
    walk_rendered(flat, |index| {
        let info = &flat[index];
        let Some(reason) = node_reason(flat, index) else {
            return false;
        };
        let names: Vec<String> = if reason == "internal-marker" {
            info.style
                .custom_properties
                .iter()
                .flat_map(|props| props.keys())
                .filter(|key| {
                    key.starts_with("--w3cos-internal")
                        && !is_value_carrier_marker(key)
                        && key.as_str() != inline::IFC_MARKER
                        && key.as_str() != inline::ANONYMOUS_BLOCK_MARKER
                        && key.as_str() != inline::LINE_WIDTH_MARKER
                })
                .map(|key| key.trim_start_matches("--w3cos-internal-").to_string())
                .collect()
        } else {
            vec![reason.to_string()]
        };
        for name in names {
            if !reasons.contains(&name) {
                reasons.push(name);
            }
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
    }
}

pub(super) struct BoxMetrics {
    pub margin: [f32; 4],
    pub border: [f32; 4],
    pub padding: [f32; 4],
    pub content_width: f32,
}

/// CSS2 10.3.3 (+10.4) used width and horizontal margins.
pub(super) fn resolve_inline(style: &Style, space: &ConstraintSpace) -> BoxMetrics {
    let cb = space.available_inline;
    let spacing =
        |s: Spacing| resolve_spacing_for_layout(s, cb, style, space.viewport_w, space.viewport_h);
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
        resolve_dim(style.width, Some(cb), style, space).map(to_content)
    };
    let mut content_width = specified
        .unwrap_or(cb - margin[1] - margin[3] - edges)
        .max(0.0);
    if let Some(max) = resolve_dim(style.max_width, Some(cb), style, space).map(to_content) {
        content_width = content_width.min(max);
    }
    if let Some(min) = resolve_dim(style.min_width, Some(cb), style, space).map(to_content) {
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
    let dim = |d: Dimension| matches!(d, Dimension::Percent(_));
    let spacing = |s: Spacing| matches!(s, Spacing::Percent(_));
    dim(style.width)
        || dim(style.min_width)
        || dim(style.max_width)
        || spacing(style.margin.left)
        || spacing(style.margin.right)
        || spacing(style.padding.left)
        || spacing(style.padding.right)
}

/// Clamp a content-box inline size by min-width / max-width (CSS2 10.4).
fn clamp_inline(style: &Style, space: &ConstraintSpace, edges: f32, mut width: f32) -> f32 {
    let cb = Some(space.available_inline);
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
    let info = &flat[index];
    let subtree = &flat[index + 1..info.subtree_end.max(index + 1)];
    let rendered = |node: &&FlatNodeInfo| node.style.display != Display::None;
    // A table resolves percentage columns itself. The caller's border and
    // padding are added back onto this content box.
    if matches!(info.style.display, Display::Table | Display::InlineTable) {
        let border_box =
            super::table::intrinsic_border_box(flat, index, space).why("table:intrinsic")?;
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
    if subtree
        .iter()
        .filter(rendered)
        .any(|node| percent_inline(node.style))
    {
        return super::bail("intrinsic-percent");
    }
    if is_author_grid(info.style) {
        // A grid's min/max-content size is the sum of its tracks, not the
        // block-flow sum below. Shrink-to-fit of an auto-width grid stays
        // on the legacy engine.
        return super::bail("grid:intrinsic");
    }
    if let ComponentKind::Text { content } = info.kind {
        return Some(inline::text_min_max(content, info.style));
    }
    if let ComponentKind::Image { src } = info.kind {
        let (width, _) = replaced_content_size(src, info.style, space).why("image-intrinsic")?;
        return Some(MinMax::exact(width));
    }
    if size_contained(info.style) {
        // Size containment: intrinsic sizes ignore the contents.
        return Some(MinMax::exact(contain_intrinsic_size(info.style).0));
    }
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
        let side = |s: Spacing| match s {
            Spacing::Auto => 0.0,
            s => resolve_spacing_for_layout(s, 0.0, style, space.viewport_w, space.viewport_h),
        };
        let b = border(style);
        let edges = b[1] + b[3] + side(style.padding.left) + side(style.padding.right);
        let (margin_left, margin_right) = (side(style.margin.left), side(style.margin.right));
        let content = match resolve_dim(style.width, None, style, space) {
            Some(w) => MinMax::exact(match style.box_sizing {
                BoxSizing::BorderBox => (w - edges).max(0.0),
                BoxSizing::ContentBox => w,
            }),
            None => intrinsic_sizes(flat, child, space).why("intrinsic")?,
        };
        let border_box = content.map(|w| clamp_inline(style, space, edges, w) + edges);
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
    Some(clamp_inline(style, space, edges, width.max(0.0)))
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

fn is_run_in(style: &Style) -> bool {
    unrecognized_display(style).is_some_and(|value| value == "run-in")
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
    let image_size = if let ComponentKind::Image { src } = info.kind {
        Some(replaced_content_size(src, style, space).why("image")?)
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
    if image_size.is_none() {
        if let Some((width, _)) = sized {
            metrics.content_width = width;
        } else if style.width == Dimension::Auto && inline::is_atomic_inline(flat, index) {
            metrics.content_width =
                shrink_to_fit(flat, index, space, &metrics).why("shrink-to-fit")?;
        }
    }
    let [bt, br, bb, bl] = metrics.border;
    let [pt, pr, pb, pl] = metrics.padding;
    let bfc_root = force_bfc_root || is_bfc_root(style);
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
    let specified_height = match sized {
        Some((_, Some(height))) => Some(height),
        _ => resolve_dim(style.height, space.percentage_block, style, space).map(to_content_h),
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
        } else {
            specified_height.map(clamp_h)
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
        // the baseline stays the bottom margin edge (CSS 2.1 10.8.1).
        cursor = height;
        at_top = false;
    } else if let ComponentKind::Text { content } = info.kind {
        // Block-level text leaf: in-flow content of its own.
        let (height, baseline) = inline::block_text_layout(content, metrics.content_width, style);
        cursor = height;
        last_baseline = baseline.offset(content_top);
        first_baseline = last_baseline;
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
            oof_candidates.push(OofCandidate {
                index: child,
                static_inline: if child_space.rtl {
                    bl + pl + content_width
                } else {
                    bl + pl
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
        if is_bfc_root(child_info.style) && !exclusions.is_empty() {
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
                    let content = if style.width == Dimension::Auto {
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
            && (matches!(child_info.kind, ComponentKind::Text { .. }) || is_bfc_root(child_style))
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
        if result.last_baseline != LastBaseline::NoLines {
            last_baseline = result.last_baseline.offset(bt + pt + y);
        }
        if !matches!(first_baseline, LastBaseline::At(_)) {
            let candidate = match result.first_baseline {
                LastBaseline::At(_) => result.first_baseline,
                _ => result.last_baseline,
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
    let content_height = clamp_h(specified_height.unwrap_or(auto_content));
    // A definite block size that clips overflowing content is a scrollport.
    // Scroll offsets stay on the legacy engine. An auto-height BFC does not.
    if non_visible_overflow(style)
        && !is_author_flex(style)
        && !is_author_grid(style)
        && style.display != Display::FlowRoot
        && specified_height.is_some()
        && auto_content > content_height + 1.0 / 128.0
    {
        return super::bail("overflow-scroll");
    }
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

    let width = metrics.content_width + bl + br + pl + pr;
    let height = content_height + bt + bb + pt + pb;
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
    Some(LayoutResult {
        last_baseline,
        first_baseline,
        fragment: BoxFragment {
            index,
            text_range: None,
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
