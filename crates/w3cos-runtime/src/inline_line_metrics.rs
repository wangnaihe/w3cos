//! Single-line metrics with authored inline struts and parent font context.
use std::collections::HashMap;
use w3cos_std::{Component, ComponentKind, Style};
use w3cos_std::style::{Dimension, Display, Float, Overflow, Position, TextDirection};
use crate::layout::{LayoutRect, count_nodes, inline_font_height,
    inline_font_content_ascent, inline_font_baseline_from_line_top,
    inline_leaf_alignment_keyword, inline_leaf_baseline_offset, inline_used_line_height};

struct Item {
    position: usize,
    subtree: Option<(usize, usize)>,
    height: f32,
    top: f32,
    offset: f32,
    font_height: Option<f32>,
    edge: u8,
}

struct Line {
    items: Vec<Item>,
    ascent: f32,
    descent: f32,
    top_height: f32,
    bottom_height: f32,
    width: f32,
    first_x: f32,
    last_x: f32,
    last_advance: f32,
}

fn place(item: &Item, x_shift: f32, y: f32, layouts: &mut [(LayoutRect, usize)],
    positions: &HashMap<usize, usize>) {
    let y_shift = y - layouts[item.position].0.y;
    if let Some((start, count)) = item.subtree {
        for index in start..start + count {
            if let Some(&position) = positions.get(&index) {
                layouts[position].0.x += x_shift;
                layouts[position].0.y += y_shift;
            }
        }
    } else {
        layouts[item.position].0.x += x_shift;
        layouts[item.position].0.y = y;
    }
    if let Some(height) = item.font_height { layouts[item.position].0.height = height; }
}

fn marker(component: &Component, name: &str) -> bool {
    component.style.custom_properties.as_ref().is_some_and(|p| p.contains_key(name))
}

impl Line {
    fn collect(&mut self, component: &Component, index: usize, parent: &Style,
        shift: f32, relative_y: f32, layouts: &[(LayoutRect, usize)], positions: &HashMap<usize, usize>) -> Option<()> {
        let style = &component.style;
        // Positioned descendants (including outside markers) are not line
        // participants. Their own positioning pass retains their geometry.
        if matches!(style.position, Position::Absolute | Position::Fixed) { return Some(()); }
        if !matches!(style.position, Position::Static | Position::Relative) || style.float != Float::None
            || style.direction != TextDirection::Ltr { return None; }
        // Relative positioning displaces paint, not the line's ascent or
        // descent. Carry the displacement separately from vertical-align.
        // Keep unresolved percentage/viewport insets and horizontal shifts
        // on the general layout path: its line-fitting coordinates differ.
        let relative_y = relative_y + if style.position == Position::Relative {
            if style.left != Dimension::Auto || style.right != Dimension::Auto { return None; }
            match (style.top, style.bottom) {
                (Dimension::Px(value), _) => value,
                (Dimension::Auto, Dimension::Px(value)) => -value,
                (Dimension::Auto, Dimension::Auto) => 0.0,
                _ => return None,
            }
        } else { 0.0 };
        let transparent = marker(component, "--w3cos-internal-unbroken-inline-word")
            || marker(component, "--w3cos-internal-inline-line-item");
        if transparent {
            let mut child_index = index + 1;
            for child in &component.children {
                self.collect(child, child_index, parent, shift, relative_y, layouts, positions)?;
                child_index += count_nodes(child);
            }
            return Some(());
        }
        let position = *positions.get(&index)?;
        let rect = layouts[position].0;
        let padding = style.padding_lengths();
        let margin = style.margin_lengths();
        // Collapsed source whitespace can survive lowering as an empty text
        // leaf at Taffy's former line edge. It creates neither an inline box
        // nor a horizontal cursor. In particular, it must not reject a real
        // replaced item restarting at the content edge after BR.
        if matches!(&component.kind, ComponentKind::Text { content } if content.is_empty())
            && component.children.is_empty() && style.display == Display::Inline
            && rect.width == 0.0 && padding.left == 0.0 && padding.right == 0.0
            && padding.top == 0.0 && padding.bottom == 0.0
            && margin.left == 0.0 && margin.right == 0.0
            && style.border_width == 0.0
            && [style.border_top_width, style.border_bottom_width,
                style.border_left_width, style.border_right_width].into_iter()
                .flatten().all(|width| width == 0.0)
        { return Some(()); }
        let keyword = inline_leaf_alignment_keyword(component);
        let text = matches!(component.kind, ComponentKind::Text { .. });
        let image = matches!(component.kind, ComponentKind::Image { .. });
        // An empty inline-block has no in-flow line baseline. Its margin
        // box participates just like a replaced atomic box, including when
        // top/bottom alignment leaves the surrounding text strut unchanged.
        let empty_atomic = component.children.iter().all(|child|
            child.style.display == Display::None
                || matches!(child.style.position, Position::Absolute | Position::Fixed))
            && matches!(component.kind, ComponentKind::Row | ComponentKind::Box)
            && style.display == Display::InlineBlock;
        let table = style.display == Display::InlineTable;
        let atomic_box = image || empty_atomic || table;
        // A lowered single-line inline-block text leaf exports its line's
        // font baseline, not the bottom of the glyph/line rectangle. Leave
        // explicit heights, scrolling and decorated atomic boxes to the
        // general atomic-inline path.
        let atomic_text = text && style.display == Display::InlineBlock
            && style.height == Dimension::Auto
            && style.overflow == Overflow::Visible
            && style.overflow_x.unwrap_or(style.overflow) == Overflow::Visible
            && style.overflow_y.unwrap_or(style.overflow) == Overflow::Visible
            && margin.top == 0.0 && margin.bottom == 0.0;
        let container = !component.children.is_empty()
            && matches!(component.kind, ComponentKind::Row | ComponentKind::Box)
            && style.display == Display::Inline;
        let decorated_text = text && style.display == Display::Inline;
        let border_top = style.border_top_width.unwrap_or(style.border_width);
        let border_bottom = style.border_bottom_width.unwrap_or(style.border_width);
        let vertical_edges = padding.top + padding.bottom + border_top + border_bottom;
        if !text && !atomic_box && !container { return None; }
        if !atomic_box && ((!atomic_text && style.display != Display::Inline)
            || (!container && !decorated_text && (padding.top != 0.0 || padding.bottom != 0.0))
            || (!text && !container && (padding.left != 0.0 || padding.right != 0.0))
            || !decorated_text && (style.border_width != 0.0
            || [style.border_top_width, style.border_bottom_width, style.border_left_width,
                style.border_right_width].into_iter().flatten().any(|v| v != 0.0))
            || !container && (margin.left != 0.0 || margin.right != 0.0)) { return None; }
        // Non-replaced inline vertical margins do not participate in line-box
        // metrics (including margins synthesized by DOM vertical-align lowering).
        if container && keyword != "baseline" { return None; }
        if atomic_box && !matches!(style.display, Display::InlineBlock | Display::InlineTable) { return None; }
        let font_height = inline_font_height(style);
        let mut height = if atomic_box { rect.height + margin.top + margin.bottom }
            else { inline_used_line_height(crate::layout::inline_style_line_height(&style)) };
        let mut text_baseline = inline_font_baseline_from_line_top(style, height);
        #[cfg(feature = "skia")]
        if style.line_height_is_normal && text && !atomic_box
            && let ComponentKind::Text { content } = &component.kind
            && let Some(metrics) = crate::render_skia::resolved_text_font_geometry(content, style)
        {
            // Concrete normal metrics already union each face's own leading.
            // Reapplying an estimated fallback ratio as primary-font leading
            // grows mixed-font lines and moves otherwise shared baselines.
            height = metrics.line_spacing();
            text_baseline = metrics.ascent;
        }
        if !height.is_finite() || height < 0.0 || !rect.width.is_finite() { return None; }
        if let ComponentKind::Text { content } = &component.kind {
            if content.contains(['\n', '\r', '\u{2028}']) || !component.children.is_empty()
                || rect.height > height.max(font_height) + vertical_edges + 0.01
                // A projected text rect may already have been reduced to its
                // em box. Recheck shaping at the used width before claiming a
                // single line; pre-wrap can retain soft breaks inside one leaf.
                // The candidate line height uses LayoutUnits. Compare the
                // wrapping measurement in those same units, not raw f32 px:
                // 19.2 versus 19.1875 otherwise rejects an actual single line.
                || inline_used_line_height(crate::layout::wrapped_text_height(content,
                    rect.width - style.border_left_width.unwrap_or(style.border_width)
                        - style.border_right_width.unwrap_or(style.border_width), style))
                    > height.max(font_height)
                        .max(crate::layout::inline_style_line_height(style)) + 0.01 { return None; }
        }
        let total_shift = shift + inline_leaf_baseline_offset(component);
        let atomic_baseline = if table {
            crate::inline_table_baseline::first_row(component, index, layouts, positions)? - rect.y + margin.top
        } else { height };
        let parent_ascent = inline_font_content_ascent(parent);
        let parent_descent = inline_font_height(parent) - parent_ascent;
        let estimated_x_height = parent.font_size * if parent.font_family.as_deref().is_some_and(|family|
            family.split(',').any(|name| name.trim().trim_matches(['\'', '"']).eq_ignore_ascii_case("ahem")))
            { 0.8 } else { 0.5 };
        #[cfg(feature = "skia")]
        let x_height = crate::render_skia::resolved_font_x_height(parent).unwrap_or(estimated_x_height);
        #[cfg(not(feature = "skia"))]
        let x_height = estimated_x_height;
        let (top, edge) = match keyword {
            "top" => (0.0, 1), "bottom" => (0.0, 2),
            "text-top" => (-parent_ascent - shift, 0),
            "text-bottom" => (parent_descent - height - shift, 0),
            "middle" => (-inline_used_line_height(x_height * 0.5) - height * 0.5 - shift, 0),
            "baseline" => (if atomic_box { -atomic_baseline - total_shift }
                else { -text_baseline - total_shift }, 0),
            _ => return None,
        };
        if edge == 1 { self.top_height = self.top_height.max(height); }
        else if edge == 2 { self.bottom_height = self.bottom_height.max(height); }
        else { self.ascent = self.ascent.max(-top); self.descent = self.descent.max(top + height); }
        self.items.push(Item { position, height, top, edge,
            subtree: table.then(|| (index, count_nodes(component))),
            // Non-replaced inline vertical padding decorates the font box;
            // it must not increase ascent/descent or move descendant text.
            // Text-only atomic inlines retain their principal line box;
            // their painter applies the internal half-leading itself.
            offset: relative_y + if atomic_box { margin.top }
                else if atomic_text { 0.0 }
                else { text_baseline - inline_font_content_ascent(style)
                    - if decorated_text { padding.top + border_top }
                        else if container { padding.top } else { 0.0 } },
            font_height: (!atomic_box && !atomic_text).then_some(font_height
                + if decorated_text { vertical_edges }
                    else if container { padding.top + padding.bottom } else { 0.0 }) });
        if container {
            // Horizontal decoration consumes line width, but does not create
            // a separate baseline for the non-replaced inline's descendants.
            self.width += padding.left + padding.right + margin.left + margin.right;
            let mut child_index = index + 1;
            for child in &component.children {
                self.collect(child, child_index, style, total_shift, relative_y, layouts, positions)?;
                child_index += count_nodes(child);
            }
        } else {
            let x = rect.x - margin.left;
            // A wrapped item can restart at exactly the previous item's
            // start. Later negative margins must not turn that established
            // break into a single line merely because the final sum fits.
            if x + 0.01 < self.last_x
                || ((x - self.last_x).abs() <= 0.01 && self.last_advance > 0.01)
            { return None; }
            self.last_x = x;
            if !self.first_x.is_finite() { self.first_x = x; }
            self.last_advance = rect.width + margin.left + margin.right;
            self.width += self.last_advance;
        }
        Some(())
    }

    fn metrics(&self) -> (f32, f32) {
        let core = self.ascent + self.descent;
        let height = core.max(self.top_height).max(self.bottom_height);
        let baseline = if self.bottom_height > core && self.top_height <= core {
            height - self.descent
        } else { self.ascent };
        (height, baseline)
    }
}

fn resolve(component: &Component, index: usize, layouts: &[(LayoutRect, usize)],
    positions: &HashMap<usize, usize>) -> Option<Line> {
    // An authored block containing only inline descendants establishes an IFC
    // even when DOM lowering did not need an anonymous flex-row marker. collect
    // rejects block/atomic descendants, floats and unsupported multi-line runs.
    // Top aligns the authored inline subtree against its containing line,
    // not each anonymous text child. Its baseline-only internal text line
    // still needs font ascent/descent resolution (Taffy exports leaf bottoms).
    let internal_top_line = component.style.display == Display::Inline
        && inline_leaf_alignment_keyword(component) == "top"
        && component.children.iter().all(|child|
            matches!(child.kind, ComponentKind::Text { .. })
                && child.children.is_empty()
                && inline_leaf_alignment_keyword(child) == "baseline");
    // Absolutely positioned inlines are blockified for their own formatting
    // context even when lowering retains the authored Inline display.
    let positioned_inline = component.style.display == Display::Inline
        && matches!(component.style.position, Position::Absolute | Position::Fixed);
    if !internal_top_line && !positioned_inline && !matches!(component.style.display, Display::Block | Display::InlineBlock | Display::TableCell)
        && !(component.style.display == Display::Flex
            && marker(component, "--w3cos-internal-inline-formatting-context")) { return None; }
    let height = inline_used_line_height(crate::layout::inline_style_line_height(&component.style));
    let ascent = inline_font_baseline_from_line_top(&component.style, height);
    let mut line = Line { items: Vec::new(), ascent, descent: height - ascent,
        top_height: 0.0, bottom_height: 0.0,
        width: 0.0, first_x: f32::INFINITY, last_x: f32::NEG_INFINITY, last_advance: 0.0 };
    let mut child_index = index + 1;
    for child in &component.children {
        line.collect(child, child_index, &component.style, 0.0, 0.0, layouts, positions)?;
        child_index += count_nodes(child);
    }
    let style = &component.style;
    let padding = style.padding_lengths();
    let available = layouts[*positions.get(&index)?].0.width - padding.left - padding.right
        - style.border_left_width.unwrap_or(style.border_width)
        - style.border_right_width.unwrap_or(style.border_width);
    (!line.items.is_empty() && line.width <= available + 0.01).then_some(line)
}

fn line_content_top(component: &Component, rect: LayoutRect) -> f32 {
    let style = &component.style;
    // Ordinary inline rectangles are already projected to their font boxes.
    // Recover the aligned line-box origin before projecting anonymous text;
    // otherwise an explicit line-height adds its half-leading a second time.
    let half_leading = if style.display == Display::Inline
        && !matches!(style.position, Position::Absolute | Position::Fixed) {
        ((inline_used_line_height(crate::layout::inline_style_line_height(&style))
            - inline_font_height(style)) * 0.5).floor()
    } else { 0.0 };
    rect.y - half_leading + style.padding_lengths().top
        + style.border_top_width.unwrap_or(style.border_width)
}

/// Preserve the line breaker's horizontal membership, but resolve each soft
/// line's own strut. A tall inline on an earlier line must not export its
/// baseline distance into a later line which contains only ordinary text.
fn soft_lines(component: &Component, index: usize, layouts: &[(LayoutRect, usize)],
    positions: &HashMap<usize, usize>) -> Option<Vec<Line>> {
    let style = &component.style;
    if style.direction != TextDirection::Ltr
        || !matches!(style.display, Display::Block | Display::InlineBlock | Display::TableCell)
            && !(style.display == Display::Flex
                && marker(component, "--w3cos-internal-inline-formatting-context"))
        || component.children.is_empty()
        || !component.children.iter().all(|child| child.children.is_empty()
            && child.style.display == Display::Inline && child.style.position == Position::Static
            && child.style.float == Float::None
            && matches!(&child.kind, ComponentKind::Text { content }
                if !content.contains(['\n', '\r', '\u{2028}'])))
    { return None; }
    let height = inline_used_line_height(crate::layout::inline_style_line_height(style));
    let ascent = inline_font_baseline_from_line_top(style, height);
    let fresh = || Line { items: Vec::new(), ascent, descent: height - ascent,
        top_height: 0.0, bottom_height: 0.0, width: 0.0,
        first_x: f32::INFINITY, last_x: f32::NEG_INFINITY, last_advance: 0.0 };
    let mut lines = vec![fresh()];
    let mut child_index = index + 1;
    for child in &component.children {
        let rect = layouts[*positions.get(&child_index)?].0;
        let previous = lines.last()?.items.last().map(|item| layouts[item.position].0);
        if let Some(previous) = previous
            && rect.x + 0.01 < previous.x && rect.y > previous.y + 0.01
        { lines.push(fresh()); }
        lines.last_mut()?.collect(child, child_index, style, 0.0, 0.0, layouts, positions)?;
        child_index += count_nodes(child);
    }
    let parent = layouts[*positions.get(&index)?].0;
    let padding = style.padding_lengths();
    let available = parent.width - padding.left - padding.right
        - style.border_left_width.unwrap_or(style.border_width)
        - style.border_right_width.unwrap_or(style.border_width);
    (lines.len() > 1 && lines.iter().all(|line| line.width <= available + 0.01)).then_some(lines)
}

fn forced_lines(component: &Component, index: usize, layouts: &[(LayoutRect, usize)],
    positions: &HashMap<usize, usize>) -> Option<Vec<Line>> {
    fn has_break(component: &Component) -> bool {
        matches!(&component.kind, ComponentKind::Text { content } if content == "\u{2028}")
            || component.style.display == Display::Inline
                && component.children.iter().any(has_break)
    }
    if !matches!(component.style.display, Display::Block | Display::InlineBlock)
        && !(component.style.display == Display::Flex
            && marker(component, "--w3cos-internal-inline-formatting-context")) { return None; }
    if !component.children.iter().any(has_break) { return None; }
    let style = &component.style;
    let height = inline_used_line_height(crate::layout::inline_style_line_height(&style));
    let ascent = inline_font_baseline_from_line_top(style, height);
    let fresh = || Line { items: Vec::new(), ascent, descent: height - ascent,
        top_height: 0.0, bottom_height: 0.0, width: 0.0,
        first_x: f32::INFINITY, last_x: f32::NEG_INFINITY, last_advance: 0.0 };
    let mut lines = vec![fresh()];
    fn collect(component: &Component, index: usize, parent: &Style,
        lines: &mut Vec<Line>, fresh: &impl Fn() -> Line,
        layouts: &[(LayoutRect, usize)], positions: &HashMap<usize, usize>) -> Option<()> {
        let style = &component.style;
        if matches!(&component.kind, ComponentKind::Text { content } if content == "\u{2028}") {
            if style.float != Float::None || style.position != Position::Static
                || style.clear != w3cos_std::style::Clear::None { return None; }
            let (advance, baseline) = baseline_text_metrics(parent, style);
            let line = lines.last_mut()?;
            line.ascent = line.ascent.max(baseline);
            line.descent = line.descent.max(advance - baseline);
            lines.push(fresh());
        } else if style.display == Display::Inline && component.children.iter().any(has_break) {
            // A passive inline is fragmented by its enclosing IFC's breaks;
            // its union rectangle is not an atomic line-fitting item. Keep
            // its strut on every traversed line, rather than measuring that
            // rectangle as one tall box and dropping the final descent.
            if style.position != Position::Static || style.float != Float::None
                || style.direction != TextDirection::Ltr
                || inline_leaf_alignment_keyword(component) != "baseline"
                || inline_leaf_baseline_offset(component) != 0.0
                || style.padding != w3cos_std::style::Edges::ZERO
                || style.margin_lengths().left != 0.0 || style.margin_lengths().right != 0.0
                || style.border_width != 0.0
                || [style.border_top_width, style.border_bottom_width, style.border_left_width,
                    style.border_right_width].into_iter().flatten().any(|v| v != 0.0)
            { return None; }
            let first_line = lines.len() - 1;
            let mut child_index = index + 1;
            for child in &component.children {
                collect(child, child_index, style, lines, fresh, layouts, positions)?;
                child_index += count_nodes(child);
            }
            let (height, baseline) = baseline_text_metrics(parent, style);
            for line in &mut lines[first_line..] {
                line.ascent = line.ascent.max(baseline);
                line.descent = line.descent.max(height - baseline);
            }
        } else {
            lines.last_mut()?.collect(component, index, parent, 0.0, 0.0, layouts, positions)?;
        }
        Some(())
    }
    let mut child_index = index + 1;
    for child in &component.children {
        collect(child, child_index, style, &mut lines, &fresh, layouts, positions)?;
        child_index += count_nodes(child);
    }
    // A terminal BR closes the current line without inventing another
    // paintable line. Consecutive breaks before it still retain empty struts.
    if lines.last()?.items.is_empty() { lines.pop(); }
    let padding = style.padding_lengths();
    let available = layouts[*positions.get(&index)?].0.width - padding.left - padding.right
        - style.border_left_width.unwrap_or(style.border_width)
        - style.border_right_width.unwrap_or(style.border_width);
    (!lines.is_empty() && lines.iter().all(|line| line.width <= available + 0.01)).then_some(lines)
}

/// The containing strut and a baseline-aligned text run both participate in
/// each line. Font boxes remain separate from this used line advance.
pub(crate) fn baseline_text_metrics(parent: &Style, text: &Style) -> (f32, f32) {
    let parent_height = inline_used_line_height(crate::layout::inline_style_line_height(&parent));
    let text_height = inline_used_line_height(crate::layout::inline_style_line_height(&text));
    let parent_ascent = inline_font_baseline_from_line_top(parent, parent_height);
    let text_ascent = inline_font_baseline_from_line_top(text, text_height);
    let ascent = parent_ascent.max(text_ascent);
    let descent = (parent_height - parent_ascent).max(text_height - text_ascent);
    (ascent + descent, ascent)
}

fn preserved_text(component: &Component, index: usize, layouts: &[(LayoutRect, usize)],
    positions: &HashMap<usize, usize>) -> Option<(usize, f32, f32, f32)> {
    if component.children.len() != 1
        || !matches!(component.style.display, Display::Block | Display::InlineBlock)
            && !(component.style.display == Display::Flex
                && marker(component, "--w3cos-internal-inline-formatting-context")) { return None; }
    let child = &component.children[0];
    let ComponentKind::Text { content } = &child.kind else { return None; };
    let style = &child.style;
    if style.display != Display::Inline || style.position != Position::Static
        || style.float != Float::None || inline_leaf_alignment_keyword(child) != "baseline"
        || inline_leaf_baseline_offset(child) != 0.0 || !child.children.is_empty()
        || style.padding != w3cos_std::style::Edges::ZERO || style.border_width != 0.0
        || [style.border_top_width, style.border_bottom_width, style.border_left_width,
            style.border_right_width].into_iter().flatten().any(|v| v != 0.0)
        || !(content.contains('\u{2028}') || content.contains(['\n', '\r'])
            && matches!(style.white_space, w3cos_std::style::WhiteSpace::Pre
                | w3cos_std::style::WhiteSpace::PreWrap | w3cos_std::style::WhiteSpace::PreLine))
    { return None; }
    let position = *positions.get(&(index + 1))?;
    let authored_height = crate::layout::inline_style_line_height(&style);
    if authored_height <= 0.0 { return None; }
    let measured_height = crate::layout::wrapped_text_height(content, layouts[position].0.width, style);
    let lines = (measured_height / authored_height).round();
    if lines < 2.0 { return None; }
    let (advance, mut baseline) = baseline_text_metrics(&component.style, style);
    #[cfg(feature = "skia")]
    if style.line_height_is_normal
        && let Some(metrics) = crate::render_skia::resolved_text_font_geometry(
            content.split(['\n', '\r', '\u{2028}']).next().unwrap_or(""), style)
    { baseline = baseline.max(metrics.ascent); }
    Some((position, measured_height.max(lines * advance), baseline - inline_font_content_ascent(style),
        inline_font_height(style)))
}

#[derive(Clone, Copy, Default)]
pub(crate) struct UsedHeightConstraints {
    pub minimum: Option<f32>,
    pub maximum: Option<f32>,
    pub fixed: Option<f32>,
}

/// Keep the sizing solver's resolved constraints separate from natural line
/// metrics: a minimum is a floor, not a fixed size for a multi-line IFC.
pub(crate) fn projected_height(style: &Style, previous: f32, natural: f32,
    used: Option<UsedHeightConstraints>) -> f32 {
    if style.height != Dimension::Auto { return previous; }
    if style.display == Display::Inline { return natural; }
    let constraints = used.unwrap_or_else(|| {
        let absolute = |dimension| match dimension {
            Dimension::Px(value) => Some(value),
            Dimension::Em(value) => Some(value * style.font_size),
            Dimension::Rem(value) => Some(value * crate::layout::ROOT_FONT_SIZE),
            _ => None,
        };
        let padding = style.padding_lengths();
        let edges = if style.box_sizing == w3cos_std::style::BoxSizing::ContentBox {
            padding.top + padding.bottom + style.border_top_width.unwrap_or(style.border_width)
                + style.border_bottom_width.unwrap_or(style.border_width)
        } else { 0.0 };
        UsedHeightConstraints {
            minimum: absolute(style.min_height).map(|h| h + edges),
            maximum: absolute(style.max_height).map(|h| h + edges),
            fixed: (matches!(style.position, Position::Absolute | Position::Fixed)
                && style.top != Dimension::Auto && style.bottom != Dimension::Auto).then_some(previous),
        }
    });
    if let Some(fixed) = constraints.fixed { return fixed; }
    let height = constraints.maximum.map_or(natural, |maximum| natural.min(maximum));
    constraints.minimum.map_or(height, |minimum| height.max(minimum))
}

pub(crate) fn project(component: &Component, index: usize, layouts: &mut [(LayoutRect, usize)],
    positions: &HashMap<usize, usize>) -> bool {
    project_with_constraints(component, index, layouts, positions, None)
}

pub(crate) fn project_with_constraints(component: &Component, index: usize,
    layouts: &mut [(LayoutRect, usize)], positions: &HashMap<usize, usize>,
    used: Option<UsedHeightConstraints>) -> bool {
    if let Some(lines) = forced_lines(component, index, layouts, positions)
        .or_else(|| soft_lines(component, index, layouts, positions)) {
        let position = positions[&index];
        let mut top = line_content_top(component, layouts[position].0);
        let initial_top = top;
        for line in lines {
            let (height, baseline) = line.metrics();
            // Each forced line owns its horizontal alignment independently;
            // Taffy's initial row position may still belong to the prior line.
            let style = &component.style;
            let rect = layouts[position].0;
            let padding = style.padding_lengths();
            let left = rect.x + padding.left + style.border_left_width.unwrap_or(style.border_width);
            let available = rect.width - padding.left - padding.right
                - style.border_left_width.unwrap_or(style.border_width)
                - style.border_right_width.unwrap_or(style.border_width);
            let free = (available - line.width).max(0.0);
            let offset = match style.text_align {
                w3cos_std::style::TextAlign::Center => free * 0.5,
                w3cos_std::style::TextAlign::Right | w3cos_std::style::TextAlign::End => free,
                _ => 0.0,
            };
            let shift_x = if line.first_x.is_finite() { left + offset - line.first_x } else { 0.0 };
            for item in line.items {
                let y = top + match item.edge {
                    1 => 0.0, 2 => height - item.height, _ => baseline + item.top,
                } + item.offset;
                place(&item, shift_x, y, layouts, positions);
            }
            top += height;
        }
        if component.style.height == Dimension::Auto {
            let style = &component.style;
            let natural = style.padding_lengths().top
                + style.border_top_width.unwrap_or(style.border_width) + top - initial_top
                + style.padding_lengths().bottom + style.border_bottom_width.unwrap_or(style.border_width);
            layouts[position].0.height = projected_height(style, layouts[position].0.height, natural, used);
        }
        return true;
    }
    if let Some((child, height, offset, font_height)) = preserved_text(component, index, layouts, positions) {
        let position = positions[&index];
        let top = line_content_top(component, layouts[position].0);
        layouts[child].0.y = top + offset;
        layouts[child].0.height = font_height;
        if component.style.height == Dimension::Auto {
            let style = &component.style;
            let natural = style.padding_lengths().top
                + style.border_top_width.unwrap_or(style.border_width) + height
                + style.padding_lengths().bottom + style.border_bottom_width.unwrap_or(style.border_width);
            layouts[position].0.height = projected_height(style, layouts[position].0.height, natural, used);
        }
        return true;
    }
    let Some(line) = resolve(component, index, layouts, positions) else { return false; };
    let position = positions[&index];
    let style = &component.style;
    let padding = style.padding_lengths();
    let top = line_content_top(component, layouts[position].0);
    let (height, baseline) = line.metrics();
    // Taffy's floating-point center can leave half a CSS LayoutUnit.
    // Preserve its line/float placement, but consume the alignment offset
    // at the inline layout boundary; do not quantize individual glyphs.
    let shift_x = if style.text_align == w3cos_std::style::TextAlign::Center {
        let left = layouts[position].0.x + padding.left
            + style.border_left_width.unwrap_or(style.border_width);
        let offset = line.first_x - left;
        if offset.is_finite() { (offset * 64.0).trunc() / 64.0 - offset } else { 0.0 }
    } else { 0.0 };
    for item in line.items {
        let y = top + match item.edge {
            1 => 0.0, 2 => height - item.height, _ => baseline + item.top,
        } + item.offset;
        place(&item, shift_x, y, layouts, positions);
    }
    if style.height == Dimension::Auto && (style.display != Display::Inline
        || matches!(style.position, Position::Absolute | Position::Fixed)) {
        let natural = padding.top + style.border_top_width.unwrap_or(style.border_width)
            + height + padding.bottom + style.border_bottom_width.unwrap_or(style.border_width);
        layouts[position].0.height = projected_height(style, layouts[position].0.height, natural, used);
    }
    true
}

pub(crate) fn is_projected(component: &Component, index: usize, layouts: &[(LayoutRect, usize)],
    positions: &HashMap<usize, usize>) -> bool {
    if let Some(lines) = forced_lines(component, index, layouts, positions)
        .or_else(|| soft_lines(component, index, layouts, positions)) {
        let mut top = line_content_top(component, layouts[positions[&index]].0);
        for line in lines {
            let (height, baseline) = line.metrics();
            if !line.items.iter().all(|item| {
                let expected = top + match item.edge {
                    1 => 0.0, 2 => height - item.height, _ => baseline + item.top,
                } + item.offset;
                (layouts[item.position].0.y - expected).abs() <= 0.01
            }) { return false; }
            top += height;
        }
        return true;
    }
    if let Some((child, _, offset, _)) = preserved_text(component, index, layouts, positions) {
        let top = line_content_top(component, layouts[positions[&index]].0);
        return (layouts[child].0.y - top - offset).abs() <= 0.01;
    }
    let Some(line) = resolve(component, index, layouts, positions) else { return false; };
    let top = line_content_top(component, layouts[positions[&index]].0);
    let (height, baseline) = line.metrics();
    line.items.iter().all(|item| {
        let expected = top + match item.edge {
            1 => 0.0, 2 => height - item.height, _ => baseline + item.top,
        } + item.offset;
        (layouts[item.position].0.y - expected).abs() <= 0.01
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_of_flow_children_keep_empty_atomic_bottom_baseline() {
        let parent = Style { display: Display::Block, font_family: Some("Ahem".into()),
            font_size: 20.0, line_height: 1.0, line_height_is_normal: false,
            ..Style::default() };
        let tree = Component::row(parent.clone(), vec![
            Component::boxed(Style { display: Display::InlineBlock,
                width: Dimension::Px(0.0), height: Dimension::Px(100.0), ..parent.clone() },
                vec![Component::image("marker.png", Style { position: Position::Absolute,
                    width: Dimension::Px(100.0), height: Dimension::Px(100.0), ..parent.clone() })]),
            Component::text("X", Style { display: Display::Inline, ..parent }),
        ]);
        let mut layouts = vec![
            (LayoutRect { x: 0.0, y: 0.0, width: 120.0, height: 104.0 }, 0),
            (LayoutRect { x: 0.0, y: 0.0, width: 0.0, height: 100.0 }, 1),
            (LayoutRect { x: -107.0, y: 0.0, width: 100.0, height: 100.0 }, 2),
            (LayoutRect { x: 0.0, y: 84.0, width: 20.0, height: 20.0 }, 3),
        ];
        assert!(project(&tree, 0, &mut layouts, &HashMap::from([(0,0),(1,1),(2,2),(3,3)])));
        assert_eq!(layouts[0].0.height, 104.0);
        assert_eq!(layouts[3].0.y, 84.0);
        assert_eq!(layouts[2].0.x, -107.0);
    }

    #[test]
    fn horizontal_inline_padding_keeps_middle_on_the_parent_strut() {
        let parent = Style { display: Display::Block,
            font_size: 24.0, line_height: 1.375, line_height_is_normal: true,
            custom_properties: Some(HashMap::from([
                (w3cos_dom::user_agent::HTML_STANDARD_FONT_PROPERTY.into(), "1".into()),
            ])),
            ..Style::default() };
        let child = Style { display: Display::Inline, font_size: 16.08,
            line_height: 22.0 / 16.08, align_self: w3cos_std::style::AlignSelf::Center,
            padding: w3cos_std::style::Edges { left: w3cos_std::style::Spacing::Px(3.216),
                ..w3cos_std::style::Edges::ZERO }, ..parent.clone() };
        let root = Component::row(parent, vec![Component::text("(nothing)", child)]);
        let mut layouts = vec![(LayoutRect { x: 104.0, y: 54.0, width: 688.0, height: 33.0 }, 0),
            (LayoutRect { x: 105.0, y: 54.0, width: 69.59375, height: 22.0 }, 1)];
        assert!(project(&root, 0, &mut layouts, &HashMap::from([(0, 0), (1, 1)])),
            "horizontal decoration must not disable the containing line's vertical strut: font={}, used={}, wrap={}",
            inline_font_height(&root.children[0].style),
            crate::layout::inline_style_line_height(&root.children[0].style),
            crate::layout::wrapped_text_height("(nothing)", 69.59375, &root.children[0].style));
        #[cfg(all(feature = "skia", target_os = "macos"))]
        assert_eq!(layouts[1].0.y, 61.796875,
            "V2882 Chromium141 font box: parent_height={} parent_ascent={} child_height={} child_ascent={} x_height={:?}",
            inline_font_height(&root.style), inline_font_content_ascent(&root.style),
            inline_font_height(&root.children[0].style), inline_font_content_ascent(&root.children[0].style),
            crate::render_skia::resolved_font_x_height(&root.style));
        assert_eq!(layouts[1].0.x, 105.0);
        assert_eq!(layouts[0].0.height, 33.0);
    }

    #[test]
    fn out_of_flow_marker_does_not_disable_inline_strut_projection() {
        let parent = Style { display: Display::Block, font_family: Some("Ahem".into()),
            font_size: 24.0, line_height: 1.0, line_height_is_normal: false,
            ..Style::default() };
        let child = Style { display: Display::Inline, font_size: 16.0,
            align_self: w3cos_std::style::AlignSelf::Center, ..parent.clone() };
        let root = Component::row(parent.clone(), vec![
            Component::row(Style { position: Position::Absolute, ..parent }, vec![]),
            Component::text("X", child),
        ]);
        let mut layouts = vec![(LayoutRect { x: 0.0, y: 0.0, width: 100.0, height: 24.0 }, 0),
            (LayoutRect { x: -15.0, y: 0.0, width: 0.0, height: 0.0 }, 1),
            (LayoutRect { x: 0.0, y: 0.0, width: 16.0, height: 16.0 }, 2)];
        let marker = layouts[1].0;
        assert!(project(&root, 0, &mut layouts, &HashMap::from([(0, 0), (1, 1), (2, 2)])));
        assert_eq!(layouts[1].0, marker, "out-of-flow geometry is owned by its positioning pass");
        assert!((layouts[2].0.y - 1.6).abs() < 0.01);
    }

    #[test]
    fn collapsed_empty_text_after_break_does_not_reject_replaced_line_metrics() {
        let parent = Style { display: Display::Block, font_size: 16.0,
            line_height: 1.375, line_height_is_normal: true,
            width: Dimension::Px(288.0), height: Dimension::Px(288.0), border_width: 3.0,
            custom_properties: Some(HashMap::from([
                (w3cos_dom::user_agent::HTML_STANDARD_FONT_PROPERTY.into(), "1".into())])),
            ..Style::default() };
        for image_height in [50.0, 100.0] {
            let text = Style { display: Display::Inline, border_width: 0.0,
                width: Dimension::Auto, height: Dimension::Auto, ..parent.clone() };
            let image = Style { display: Display::InlineBlock,
                width: Dimension::Px(200.0), height: Dimension::Px(image_height),
                ..text.clone() };
            let root = Component::boxed(parent.clone(), vec![
                Component::image("first.png", image.clone()),
                Component::text("\u{2028}", text.clone()),
                Component::text("", text),
                Component::image("second.png", image),
            ]);
            let rect = |x,y,width,height| LayoutRect { x,y,width,height };
            let mut layouts = vec![(rect(8.0,76.0,294.0,294.0),0),
                (rect(11.0,79.0,200.0,image_height),1),
                (rect(11.0,79.0+image_height+22.0,288.0,0.0),2),
                (rect(299.0,79.0+image_height,0.0,22.0),3),
                (rect(11.0,96.0,200.0,image_height),4)];
            let positions = (0..5).map(|i|(i,i)).collect();
            assert!(project(&root,0,&mut layouts,&positions),
                "collapsed empty text must not establish a phantom right-edge cursor");
            assert_eq!(layouts[1].0.y,79.0);
            assert_eq!(layouts[4].0.y,79.0+image_height+5.0,
                "next image follows the replaced height plus Chromium parent descent");
        }
    }

    #[test]
    #[cfg(all(feature = "skia", target_os = "macos"))]
    fn normal_fallback_line_does_not_reapply_primary_leading() {
        use crate::font_face::{FontFace, FontRegistry, FontSource};
        const OWNER: u64 = 202610080132;
        let bytes = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"),
            "/../../../wpt/fonts/Ahem.ttf")).expect("pinned Ahem font");
        let registry = FontRegistry::global();
        registry.register_for_owner(OWNER, FontFace { family: "Ahem".into(),
            src: FontSource::Bytes(bytes), ..Default::default() }).unwrap();
        let parent = Style { display: Display::Block, font_family: Some("serif".into()),
            font_size: 16.0, line_height: 1.125, line_height_is_normal: true,
            ..Style::default() };
        let ahem = Style { display: Display::Inline,
            font_family: Some("Ahem, \"Courier New\"".into()), font_size: 64.0,
            line_height: 1.1328125, ..parent.clone() };
        let courier = Style { font_family: Some("Courier New".into()),
            line_height: 1.125, ..ahem.clone() };
        let root = Component::row(parent.clone(), vec![
            Component::text("Ţęşţ", ahem),
            Component::text(" — ", Style { display: Display::Inline, ..parent }),
            Component::text("Ţęşţ", courier),
        ]);
        let rect = |x,y,width,height| LayoutRect { x,y,width,height };
        let mut layouts = vec![(rect(8.0,222.0,784.0,74.0),0),
            (rect(8.0,226.0,153.625,64.0),1),
            (rect(161.625,263.0,24.0,18.0),2),
            (rect(185.625,224.0,153.625,72.0),3)];
        let positions = HashMap::from([(0,0),(1,1),(2,2),(3,3)]);
        let projected = project(&root,0,&mut layouts,&positions);
        registry.clear_owner(OWNER);
        assert!(projected);
        assert_eq!(layouts[0].0.height,72.0,"Chromium normal mixed-font line advance");
        assert_eq!(layouts[1].0.y,224.0,"primary Ahem font box");
        assert_eq!(layouts[3].0.y,222.0,"Courier fallback font box");
    }

    #[test]
    fn normal_line_height_keeps_the_embedding_font_estimate() {
        for (size, ratio) in [(16.0, 1.2), (13.333333, 1.125)] {
            let style = Style { font_size: size, line_height: ratio,
                line_height_is_normal: true, ..Style::default() };
            assert_eq!(crate::layout::inline_style_line_height(&style), size * ratio,
                "normal is font spacing, not an authored unitless ratio");
        }
    }

    #[test]
    fn line_height_preserves_browser_unitless_and_length_boundaries() {
        // Chromium141 DOM geometry, line-height-oracle-v1922. Identical
        // computed CSS serialization does not imply identical used height.
        for (size, ratio, length, expected) in [
            (20.8, 2.5, None, 51.984375), (20.8, 2.5, Some(52.0), 52.0),
            (14.2, 1.5, None, 21.296875), (14.2, 1.5, Some(21.3), 21.296875),
            (13.333333, 1.2, None, 15.984375), (16.0, 1.2, None, 19.1875),
            (22.0, 26.0/22.0, Some(26.0), 26.0), (22.0, 26.0/22.0, None, 26.0),
        ] {
            let style = Style { font_size: size, line_height: ratio,
                line_height_is_normal: false, line_height_computed_px: length,
                ..Style::default() };
            assert_eq!(crate::layout::inline_style_line_height(&style), expected,
                "font={size}, ratio={ratio}, length={length:?}");
            assert_eq!(style.font_size, size, "used metrics do not rewrite computed font-size");
        }
    }

    #[test]
    fn centered_inline_line_truncates_half_layout_unit_in_alignment_offset() {
        for parent_x in [0.0, 381.296875] {
            let root = Component::boxed(Style { display: Display::Block,
                line_height: 2.0, text_align: w3cos_std::style::TextAlign::Center, ..Style::default() },
                vec![Component::text("text", Style { display: Display::Inline,
                    line_height: 2.0, ..Style::default() })]);
            let width = 23.984375;
            let mut layouts = vec![(LayoutRect { x: parent_x, y: 0.0,
                width: 48.0, height: 32.0 }, 0), (LayoutRect {
                x: parent_x + (48.0 - width) / 2.0, y: 0.0,
                width, height: 22.0 }, 1)];
            assert!(project(&root, 0, &mut layouts, &HashMap::from([(0,0),(1,1)])));
            assert_eq!(layouts[1].0.x, parent_x + 12.0,
                "center alignment uses integer LayoutUnit division, not a half unit");
            assert_eq!(layouts[1].0.width, width, "alignment does not change the shaped advance");
        }
    }

    #[test]
    fn inline_line_projection_preserves_used_constrained_height() {
        for (constraint, used_height) in [
            (Style { min_height: Dimension::Px(600.0), ..Style::default() }, 600.0),
            (Style { min_height: Dimension::Percent(50.0), ..Style::default() }, 300.0),
            (Style { max_height: Dimension::Px(10.0), ..Style::default() }, 10.0),
            (Style { position: Position::Absolute, top: Dimension::Px(0.0),
                bottom: Dimension::Px(0.0), ..Style::default() }, 600.0),
        ] {
            let style = Style { display: Display::Block, font_size: 16.0,
                line_height: 1.375, line_height_is_normal: false, ..constraint };
            let root = Component::row(style.clone(), vec![Component::text("text", Style {
                display: Display::Inline, position: Position::Static,
                min_height: Dimension::Auto, max_height: Dimension::Auto,
                ..style
            })]);
            let mut layouts = vec![(LayoutRect { x:0.0,y:0.0,width:800.0,height:used_height },0),
                (LayoutRect { x:0.0,y:0.0,width:40.0,height:22.0 },1)];
            let positions = HashMap::from([(0,0),(1,1)]);
            let used = UsedHeightConstraints {
                minimum: (root.style.min_height != Dimension::Auto).then_some(used_height),
                maximum: (root.style.max_height != Dimension::Auto).then_some(used_height),
                fixed: (root.style.position == Position::Absolute).then_some(used_height),
            };
            assert!(project_with_constraints(&root,0,&mut layouts,&positions,Some(used)));
            assert_eq!(layouts[0].0.height, used_height,
                "line projection must not replace the constraint solver's used height");
        }
    }

    #[test]
    fn anonymous_ifc_keeps_percentage_minimum_and_absolute_stretch() {
        for absolute in [false, true] {
            let mut style = Style { display: Display::Flex,
                custom_properties: Some(HashMap::from([(
                    "--w3cos-internal-inline-formatting-context".into(), "1".into())])),
                ..Style::default() };
            if absolute {
                style.position = Position::Absolute;
                style.top = Dimension::Px(0.0); style.bottom = Dimension::Px(0.0);
                style.left = Dimension::Px(0.0); style.right = Dimension::Px(0.0);
            } else { style.min_height = Dimension::Percent(100.0); }
            let root = Component::row(Style { display: Display::Block,
                height: Dimension::Percent(100.0), ..Style::default() }, vec![
                Component::row(style, vec![Component::text("viewport", Style {
                    display: Display::Inline, ..Style::default()
                })]),
            ]);
            let layouts = crate::layout::compute(&root,800.0,600.0).unwrap();
            assert_eq!(layouts.iter().find(|(_, index)| *index==1).unwrap().0.height,600.0,
                "absolute={absolute}, layouts={layouts:?}");
        }
    }

    #[test]
    #[cfg(all(feature = "skia", target_os = "macos"))]
    fn dom_empty_atomic_top_bottom_text_uses_shared_line_metrics() {
        use crate::html_parser_host::InertParserScriptHost;
        use crate::html_parser_state::StreamingDocumentParser;
        use std::rc::Rc;
        for keyword in ["top", "bottom"] {
            for padding in [0, 20] {
                crate::dom::reset_document();
                crate::jsdom::reset_bridge();
                let mut parser = StreamingDocumentParser::new_with_script_host(
                    Rc::new(InertParserScriptHost), "https://example.test/atomic-edge.html").unwrap();
                parser.write(&format!("<!doctype html>
                    <div style='margin-top:50px;font-size:10px;line-height:1'>
                    <span style='padding-{keyword}:{padding}px'>\n Next\n
                    <inline-block style='display:inline-block;height:30px;width:30px;background:blue;
                    vertical-align:{keyword}'></inline-block>\n </span></div>")).unwrap();
                parser.finish().unwrap();
                let root = crate::dom::to_component_tree();
                let flat = crate::layout::pre_flatten(&root);
                let layouts = crate::layout::compute(&root, 800.0, 600.0).unwrap();
                let text_index = flat.iter().position(|node|
                    matches!(&node.kind, ComponentKind::Text { content } if content.contains("Next"))).unwrap();
                let rect = layouts.iter().find(|(_, index)| *index == text_index).unwrap().0;
                let mut line_index = flat[text_index].parent.unwrap();
                while matches!(flat[line_index].style.display, Display::Inline | Display::Contents) {
                    line_index = flat[line_index].parent.unwrap();
                }
                let line = layouts.iter().find(|(_, index)| *index == line_index).unwrap().0;
                assert_eq!(rect.y - line.y, if keyword == "top" { -2.0 } else { 18.0 },
                    "original Chromium text font origin, edge={keyword}, padding={padding}, layouts={layouts:?}");
            }
        }
    }

    #[test]
    #[cfg(all(feature = "skia", target_os = "macos"))]
    fn empty_atomic_top_bottom_boxes_preserve_text_strut_with_inline_padding() {
        for keyword in ["top", "bottom"] {
            for decorated in [false, true] {
                let parent = Style { display: Display::Block, font_family: Some("serif".into()),
                    font_size: 10.0, line_height: 1.0, line_height_is_normal: false,
                    ..Style::default() };
                let text = Component::text("Next ", Style { display: Display::Inline, ..parent.clone() });
                let atomic = Component::row(Style { display: Display::InlineBlock,
                    width: Dimension::Px(30.0), height: Dimension::Px(30.0),
                    custom_properties: Some(HashMap::from([(
                        "--w3cos-internal-vertical-align-keyword".into(), keyword.into())])),
                    ..parent.clone() }, vec![]);
                let mut inline = Style { display: Display::Inline, ..parent.clone() };
                if decorated {
                    if keyword == "top" { inline.padding.top = w3cos_std::style::Spacing::Px(20.0); }
                    else { inline.padding.bottom = w3cos_std::style::Spacing::Px(20.0); }
                }
                let root = Component::row(parent, vec![Component::row(inline, vec![text, atomic])]);
                let line_y = if keyword == "top" { 50.0 } else { 130.0 };
                let mut layouts = vec![
                    (LayoutRect { x: 8.0, y: line_y, width: 784.0, height: 30.0 }, 0),
                    (LayoutRect { x: 8.0, y: line_y - 2.0 - if decorated && keyword == "top" { 20.0 } else { 0.0 },
                        width: 51.953125, height: if decorated { 33.0 } else { 13.0 } }, 1),
                    (LayoutRect { x: 8.0, y: line_y + 20.0, width: 21.953125, height: 13.0 }, 2),
                    (LayoutRect { x: 29.953125, y: line_y, width: 30.0, height: 30.0 }, 3),
                ];
                let positions = HashMap::from([(0, 0), (1, 1), (2, 2), (3, 3)]);
                assert!(project(&root, 0, &mut layouts, &positions),
                    "one line with empty atomic {keyword} box, inline padding={decorated}");
                assert_eq!(layouts[2].0.y, if keyword == "top" { 48.0 } else { 148.0 },
                    "Chromium141 original WPT text Range origin");
                assert_eq!(layouts[3].0.y, line_y, "atomic box remains at the line edge");
                assert_eq!(layouts[0].0.height, 30.0, "inline padding does not expand line height");
                assert!(is_projected(&root, 0, &layouts, &positions));
            }
        }
    }

    #[test]
    #[cfg(all(feature = "skia", target_os = "macos"))]
    fn fractional_line_height_uses_same_layout_units_for_wrap_detection() {
        for display in [Display::Block, Display::Flex] {
            let style = Style { display, font_family:Some("serif".into()),
                font_size:16.0,line_height:1.2,line_height_is_normal:false,
                custom_properties:Some(HashMap::from([(
                    "--w3cos-internal-inline-formatting-context".into(),"1".into())])),
                ..Style::default() };
            let child = Style { display:Display::Inline, custom_properties:None,..style.clone() };
            let root = Component::row(style.clone(),vec![Component::text("Block 1",child.clone())]);
            let mut layouts = vec![(LayoutRect{x:8.0,y:50.0,width:320.0,height:19.2},0),
                (LayoutRect{x:8.0,y:50.6,width:320.0,height:18.0},1)];
            let positions = HashMap::from([(0,0),(1,1)]);
            assert!(project(&root,0,&mut layouts,&positions),
                "fractional single line must not be rejected as wrapped");
            assert_eq!(layouts[1].0.y,50.0,"Chromium Times16 explicit19.2px font top");
            assert_eq!(layouts[0].0.height,19.1875);
            assert!(is_projected(&root,0,&layouts,&positions));
            let wrapped = Component::row(style,vec![Component::text("Block 1 Block 1",child)]);
            layouts[1].0.width=8.0;
            assert!(!project(&wrapped,0,&mut layouts,&positions),
                "actual wrapped text must still use the multiline path");
        }
    }

    #[test]
    #[cfg(all(feature = "skia", target_os = "macos"))]
    fn top_inline_font_box_does_not_apply_explicit_half_leading_twice() {
        let style=Style { display:Display::Inline,font_family:Some("serif".into()),
            font_size:12.0,line_height:20.0/12.0,line_height_is_normal:false,
            align_self:w3cos_std::style::AlignSelf::FlexStart,
            custom_properties:Some(HashMap::from([(
                "--w3cos-internal-vertical-align-keyword".into(),"top".into())])),..Style::default() };
        let child=Style { align_self:w3cos_std::style::AlignSelf::Auto,custom_properties:None,..style.clone() };
        let root=Component::row(style,vec![Component::text("XX",child)]);
        let rect=LayoutRect { x:8.0,y:11.0,width:17.34375,height:14.0 };
        let mut layouts=vec![(rect,0),(rect,1)];
        let positions=HashMap::from([(0,0),(1,1)]);
        assert!(project(&root,0,&mut layouts,&positions));
        assert_eq!(layouts[1].0.y,11.0,"Chromium141 top-inline explicit leading applied once");
        assert_eq!(layouts[0].0,rect,"principal remains a font box");
        assert!(is_projected(&root,0,&layouts,&positions));
    }

    #[test]
    #[cfg(all(feature = "skia", target_os = "macos"))]
    fn inline_block_single_line_exports_font_baseline_not_leaf_bottom() {
        for atomic_leaf in [false, true] {
            let small = Style { display: Display::Inline, font_family: Some("serif".into()),
                font_size:16.0, line_height:1.125, line_height_is_normal:true, ..Style::default() };
            let large = Style { display:if atomic_leaf { Display::InlineBlock } else { Display::Inline },
                font_size:32.0, line_height:1.15625, ..small.clone() };
            let children=vec![Component::text("XXXXX",small.clone()),Component::text("XXXXX",large)];
            let root=if atomic_leaf {
                Component::row(Style { display:Display::Block,..small },children)
            } else {
                Component::row(Style { display:Display::InlineBlock,..small.clone() },vec![
                    Component::row(Style { display:Display::InlineFlex,
                        custom_properties:Some(HashMap::from([(
                            "--w3cos-internal-unbroken-inline-word".into(),"1".into())])),..small },children)])
            };
            let rect=|x,y,width,height| LayoutRect { x,y,width,height };
            let mut layouts=vec![(rect(8.0,50.0,173.328125,41.0),0)];
            if !atomic_leaf { layouts.push((rect(8.0,50.0,173.328125,37.0),1)); }
            let first=layouts.len();
            layouts.push((rect(8.0,69.0,57.78125,18.0),first));
            layouts.push((rect(65.78125,50.0,115.546875,37.0),first+1));
            let positions=layouts.iter().enumerate().map(|(p,(_,i))|(*i,p)).collect();
            assert!(project(&root,0,&mut layouts,&positions),"single-line inline-block baseline, atomic_leaf={atomic_leaf}");
            assert_eq!(layouts[first].0.y,65.0,"Chromium141 small font top");
            assert_eq!(layouts[first+1].0.y,50.0);
            assert_eq!(layouts[0].0.height,37.0);
            assert!(is_projected(&root,0,&layouts,&positions));
        }
    }

    #[test]
    #[cfg(all(feature = "skia", target_os = "macos"))]
    fn top_aligned_inline_keeps_internal_mixed_font_baseline() {
        let style = Style { display: Display::Inline,
            align_self: w3cos_std::style::AlignSelf::FlexStart,
            font_family: Some("serif".into()), font_size:16.0,
            line_height:1.125, line_height_is_normal:true,
            custom_properties:Some(HashMap::from([(
                "--w3cos-internal-vertical-align-keyword".into(),"top".into())])),
            ..Style::default() };
        let small = Style { align_self:w3cos_std::style::AlignSelf::Auto,
            custom_properties:None, ..style.clone() };
        let large = Style { font_size:80.0, line_height:1.15, ..small.clone() };
        let root = Component::row(style, vec![Component::text("A",large),Component::text(" x",small)]);
        let rect = |x,y,width,height| LayoutRect { x,y,width,height };
        let mut layouts = vec![(rect(8.0,8.0,69.78125,18.0),0),
            (rect(8.0,8.0,57.78125,92.0),1),(rect(65.78125,82.0,12.0,18.0),2)];
        let positions=HashMap::from([(0,0),(1,1),(2,2)]);
        assert!(project(&root,0,&mut layouts,&positions),"outer top alignment does not disable its internal baseline");
        assert_eq!(layouts[1].0.y,8.0,"large font anchor remains unchanged");
        assert_eq!(layouts[2].0.y,66.0,"Chromium141 anonymous text font box");
        assert_eq!(layouts[0].0.height,18.0,"inline principal is not a block line box");
        assert!(is_projected(&root,0,&layouts,&positions));
    }

    #[test]
    fn soft_wrapped_text_cannot_be_projected_as_a_single_strut() {
        let parent = Style { display: Display::Block, font_family: Some("Ahem".into()),
            font_size: 20.0, line_height: 1.0, line_height_is_normal: false,
            white_space: w3cos_std::style::WhiteSpace::PreWrap, ..Style::default() };
        let child = Style { display: Display::Inline, ..parent.clone() };
        let root = Component::row(parent, vec![Component::text("XX  XX", child)]);
        let mut layouts = vec![(LayoutRect { x: 0.0, y: 0.0, width: 100.0, height: 40.0 }, 0),
            (LayoutRect { x: 0.0, y: 0.0, width: 100.0, height: 20.0 }, 1)];
        let before = layouts.clone();
        assert!(!project(&root, 0, &mut layouts, &HashMap::from([(0, 0), (1, 1)])),
            "a font-sized projected rect does not prove the text fits on one line");
        assert_eq!(layouts, before);
    }

    #[test]
    fn nested_text_lengths_use_outer_strut_and_individual_baseline_offsets() {
        let parent = Style { display: Display::Block, font_family: Some("serif".into()),
            font_size: 60.0, line_height: 0.4, line_height_is_normal: false,
            custom_properties: Some(HashMap::from([
                ("--w3cos-internal-inline-formatting-context".into(), "1".into()),
            ])), ..Style::default() };
        let small = Style { display: Display::Inline, font_size: 24.0,
            custom_properties: None, ..parent.clone() };
        let leaves = [24.0_f32, 12.0, 0.0].into_iter().map(|offset| {
            let mut style = small.clone();
            style.custom_properties = Some(HashMap::from([
                ("--w3cos-internal-vertical-align-length".into(), format!("{offset} 0")),
            ]));
            Component::text("x", style)
        }).collect();
        let root = Component::row(parent.clone(), vec![Component::row(small.clone(), leaves)]);
        let rect = |x, y, width, height| LayoutRect { x, y, width, height };
        let mut layouts = vec![(rect(0.0, 50.0, 300.0, 24.0), 0),
            (rect(0.0, 40.8, 72.0, 28.0), 1),
            (rect(0.0, 14.4, 24.0, 28.0), 2),
            (rect(24.0, 26.4, 24.0, 28.0), 3),
            (rect(48.0, 40.8, 24.0, 28.0), 4)];
        let positions = HashMap::from([(0,0), (1,1), (2,2), (3,3), (4,4)]);
        assert!(project(&root, 0, &mut layouts, &positions),
            "nested text-only lines need the same shared strut solver as nested image/text lines");
        #[cfg(all(feature = "skia", target_os = "macos"))]
        {
            assert_eq!(layouts[1].0.y, 64.0, "Chromium141 wrapper font box");
            assert_eq!(layouts[2].0.y, 40.0, "Chromium141 raised font box");
            assert_eq!(layouts[3].0.y, 52.0, "Chromium141 middle font box");
            assert_eq!(layouts[4].0.y, 64.0, "Chromium141 baseline font box");
            assert_eq!(layouts[0].0.height, 33.59375, "used line-height retains LayoutUnit precision");
        }
        let small_ascent = inline_font_content_ascent(&small);
        let baseline = layouts[4].0.y + small_ascent;
        assert!((layouts[2].0.y + small_ascent + 24.0 - baseline).abs() < 0.01);
        assert!((layouts[3].0.y + small_ascent + 12.0 - baseline).abs() < 0.01);
        assert!((layouts[1].0.y + small_ascent - baseline).abs() < 0.01);
        assert!(layouts[2].0.y >= 39.0, "negative child leading must not erase the preceding paragraph");
        assert!(layouts[0].0.height > 24.0, "nested descents can extend the containing line box");
        assert!(is_projected(&root, 0, &layouts, &positions));
    }

    #[test]
    fn flat_text_length_uses_outer_strut_without_vertical_margin_inflation() {
        let parent = Style { display: Display::Block, font_family: Some("serif".into()),
            font_size: 60.0, line_height: 0.4, line_height_is_normal: false,
            custom_properties: Some(HashMap::from([
                ("--w3cos-internal-inline-formatting-context".into(), "1".into()),
            ])), ..Style::default() };
        let child = Style { display: Display::Inline, font_size: 24.0,
            custom_properties: Some(HashMap::from([
                ("--w3cos-internal-vertical-align-length".into(), "24 0".into()),
            ])), ..parent.clone() };
        let rect = |y, height| LayoutRect { x: 0.0, y, width: 100.0, height };
        let positions = HashMap::from([(0, 0), (1, 1)]);
        let mut plain = vec![(rect(50.0, 24.0), 0), (rect(14.4, 28.0), 1)];
        assert!(project(&Component::row(parent.clone(), vec![Component::text("abc", child.clone())]),
            0, &mut plain, &positions));
        let mut margined_child = child;
        margined_child.margin.top = w3cos_std::style::Spacing::Px(30.0);
        margined_child.margin.bottom = w3cos_std::style::Spacing::Px(19.2);
        let mut margined = vec![(rect(50.0, 24.0), 0), (rect(14.4, 28.0), 1)];
        assert!(project(&Component::row(parent, vec![Component::text("abc", margined_child)]),
            0, &mut margined, &positions));
        assert_eq!(plain, margined);
        assert!(plain[1].0.y >= 39.0);
    }

    #[test]
    fn nested_inline_alignment_uses_parent_font_strut() {
        let small = Style { display: Display::Inline, font_family: Some("serif".into()),
            font_size: 16.0, line_height: 1.125, line_height_is_normal: false,
            ..Style::default() };
        let large = Style { font_size: 32.0, ..small.clone() };
        for keyword in ["baseline", "text-top", "text-bottom", "top", "bottom", "middle"] {
            for offset in [-20.0_f32, 0.0, 20.0] {
                let leaf = Style { custom_properties: Some(HashMap::from([
                    ("--w3cos-internal-vertical-align-keyword".into(), keyword.into()),
                ])), ..small.clone() };
                let nested = Style { custom_properties: Some(HashMap::from([
                    ("--w3cos-internal-vertical-align-length".into(), format!("{offset} 0")),
                ])), ..large.clone() };
                let root = Component::row(Style { display: Display::Flex,
                    custom_properties: Some(HashMap::from([
                        ("--w3cos-internal-inline-formatting-context".into(), "1".into()),
                    ])), ..small.clone() }, vec![
                    Component::image("strut.png", Style { display: Display::InlineBlock,
                        ..small.clone() }),
                    Component::row(nested, vec![Component::text("x", leaf),
                        Component::text("X", large.clone())]),
                ]);
                let rect = |x, width, height| LayoutRect { x, y: 10.0, width, height };
                let mut layouts = vec![(rect(0.0, 100.0, 30.0), 0),
                    (rect(0.0, 30.0, 30.0), 1), (rect(30.0, 48.0, 36.0), 2),
                    (rect(30.0, 16.0, 18.0), 3), (rect(46.0, 32.0, 36.0), 4)];
                let positions = HashMap::from([(0,0), (1,1), (2,2), (3,3), (4,4)]);
                assert!(project(&root, 0, &mut layouts, &positions));
                let baseline = layouts[1].0.y + 30.0;
                let large_ascent = inline_font_content_ascent(&large);
                let small_ascent = inline_font_content_ascent(&small);
                let large_descent = inline_font_height(&large) - large_ascent;
                let small_leading = (18.0 - inline_font_height(&small)) * 0.5;
                let expected = match keyword {
                    "text-top" => baseline - offset - large_ascent + small_leading,
                    "text-bottom" => baseline - offset + large_descent - 18.0 + small_leading,
                    "middle" => {
                        let x_height = 32.0 * 0.5;
                        #[cfg(feature = "skia")]
                        let x_height = crate::render_skia::resolved_font_x_height(&large).unwrap_or(x_height);
                        baseline - offset - inline_used_line_height(x_height * 0.5) - 9.0 + small_leading
                    },
                    "top" => 10.0 + small_leading,
                    "bottom" => 10.0 + layouts[0].0.height - 18.0 + small_leading,
                    _ => baseline - offset - small_ascent,
                };
                assert!((layouts[3].0.y - expected).abs() < 0.01, "{keyword}, {offset}");
                assert!((layouts[4].0.y + large_ascent + offset - baseline).abs() < 0.01);
                assert_eq!(layouts[2].0.y, layouts[4].0.y);
                assert!(is_projected(&root, 0, &layouts, &positions));
            }
        }
    }
}
