use w3cos_std::color::Color;
use w3cos_std::style::{
    AlignItems, Dimension, Display, Edges, FlexDirection, FlexWrap, JustifyContent, Overflow,
    Position, Style,
};

/// CSSStyleDeclaration — the `element.style` property.
/// Mutable handle that writes directly to the node's Style.
#[derive(Debug, Clone)]
pub struct CSSStyleDeclaration {
    pub inner: Style,
}

impl CSSStyleDeclaration {
    pub fn new() -> Self {
        Self {
            inner: Style::default(),
        }
    }

    pub fn from_style(style: Style) -> Self {
        Self { inner: style }
    }

    pub fn set_property(&mut self, name: &str, value: &str) {
        match name {
            "display" => self.inner.display = parse_display(value),
            "position" => self.inner.position = parse_position(value),

            "flex-direction" | "flexDirection" => {
                self.inner.flex_direction = parse_flex_direction(value)
            }
            "justify-content" | "justifyContent" => {
                self.inner.justify_content = parse_justify_content(value)
            }
            "align-items" | "alignItems" => self.inner.align_items = parse_align_items(value),
            "flex-wrap" | "flexWrap" => self.inner.flex_wrap = parse_flex_wrap(value),
            "flex-grow" | "flexGrow" => {
                if let Ok(v) = value.parse() {
                    self.inner.flex_grow = v
                }
            }
            "flex-shrink" | "flexShrink" => {
                if let Ok(v) = value.parse() {
                    self.inner.flex_shrink = v
                }
            }

            "gap" => {
                if let Some(v) = parse_px(value) {
                    self.inner.gap = v
                }
            }
            "padding" => {
                if let Some(v) = parse_px(value) {
                    self.inner.padding = Edges::all(v)
                }
            }
            "padding-top" | "paddingTop" => {
                if let Some(v) = parse_px(value) {
                    self.inner.padding.top = v
                }
            }
            "padding-right" | "paddingRight" => {
                if let Some(v) = parse_px(value) {
                    self.inner.padding.right = v
                }
            }
            "padding-bottom" | "paddingBottom" => {
                if let Some(v) = parse_px(value) {
                    self.inner.padding.bottom = v
                }
            }
            "padding-left" | "paddingLeft" => {
                if let Some(v) = parse_px(value) {
                    self.inner.padding.left = v
                }
            }
            "margin" => {
                if let Some(v) = parse_px(value) {
                    self.inner.margin = Edges::all(v)
                }
            }

            "width" => self.inner.width = parse_dimension(value),
            "height" => self.inner.height = parse_dimension(value),
            "min-width" | "minWidth" => self.inner.min_width = parse_dimension(value),
            "min-height" | "minHeight" => self.inner.min_height = parse_dimension(value),
            "max-width" | "maxWidth" => self.inner.max_width = parse_dimension(value),
            "max-height" | "maxHeight" => self.inner.max_height = parse_dimension(value),

            "top" => self.inner.top = parse_dimension(value),
            "right" => self.inner.right = parse_dimension(value),
            "bottom" => self.inner.bottom = parse_dimension(value),
            "left" => self.inner.left = parse_dimension(value),
            "z-index" | "zIndex" => {
                if let Ok(v) = value.parse() {
                    self.inner.z_index = v
                }
            }

            "overflow" => self.inner.overflow = parse_overflow(value),

            "background" | "background-color" | "backgroundColor" => {
                self.inner.background = Color::from_hex(value)
            }
            "color" => self.inner.color = Color::from_hex(value),
            "font-size" | "fontSize" => {
                if let Some(v) = parse_px(value) {
                    self.inner.font_size = v
                }
            }
            "font-weight" | "fontWeight" => {
                if let Ok(v) = value.parse() {
                    self.inner.font_weight = v
                }
            }
            "border-radius" | "borderRadius" => {
                if let Some(v) = parse_px(value) {
                    self.inner.border_radius = v
                }
            }
            "border-width" | "borderWidth" => {
                if let Some(v) = parse_px(value) {
                    self.inner.border_width = v
                }
            }
            "border-color" | "borderColor" => self.inner.border_color = Color::from_hex(value),
            "opacity" => {
                if let Ok(v) = value.parse() {
                    self.inner.opacity = v
                }
            }

            // Box shadow: "offsetX offsetY blur spread color"
            "box-shadow" | "boxShadow" => {
                self.inner.box_shadow = parse_box_shadow(value);
            }

            // Transform
            "transform" => {
                self.inner.transform = parse_transform(value);
            }

            // Transition: "property duration easing"
            "transition" => {
                self.inner.transition = parse_transition(value);
            }

            // Text properties
            "text-align" | "textAlign" => self.inner.text_align = parse_text_align(value),
            "white-space" | "whiteSpace" => self.inner.white_space = parse_white_space(value),
            "line-height" | "lineHeight" => {
                if let Ok(v) = value.trim().trim_end_matches("px").parse() {
                    self.inner.line_height = v;
                }
            }
            "letter-spacing" | "letterSpacing" => {
                if let Some(v) = parse_px(value) {
                    self.inner.letter_spacing = v;
                }
            }
            "text-decoration" | "textDecoration" => self.inner.text_decoration = parse_text_decoration(value),
            "text-overflow" | "textOverflow" => self.inner.text_overflow = parse_text_overflow(value),
            "font-family" | "fontFamily" => {
                self.inner.font_family = Some(value.trim_matches('"').trim_matches('\'').to_string());
            }
            "font-style" | "fontStyle" => self.inner.font_style = parse_font_style(value),
            "word-break" | "wordBreak" => self.inner.word_break = parse_word_break(value),

            // Interaction
            "cursor" => self.inner.cursor = parse_cursor(value),
            "pointer-events" | "pointerEvents" => self.inner.pointer_events = parse_pointer_events(value),
            "user-select" | "userSelect" => self.inner.user_select = parse_user_select(value),

            // Visibility
            "visibility" => self.inner.visibility = parse_visibility(value),

            // Flex extras
            "flex-basis" | "flexBasis" => self.inner.flex_basis = parse_dimension(value),
            "order" => { if let Ok(v) = value.parse() { self.inner.order = v } }
            "align-self" | "alignSelf" => self.inner.align_self = parse_align_self(value),
            "align-content" | "alignContent" => self.inner.align_content = parse_align_content(value),

            // Outline
            "outline-width" | "outlineWidth" => {
                if let Some(v) = parse_px(value) { self.inner.outline_width = v }
            }
            "outline-color" | "outlineColor" => self.inner.outline_color = Color::from_hex(value),
            "outline-style" | "outlineStyle" => self.inner.outline_style = parse_outline_style(value),

            _ => {}
        }
    }

    pub fn get_property(&self, name: &str) -> String {
        match name {
            "display" => format!("{:?}", self.inner.display).to_lowercase(),
            "position" => format!("{:?}", self.inner.position).to_lowercase(),
            "font-size" | "fontSize" => format!("{}px", self.inner.font_size),
            "color" => format!(
                "#{:02x}{:02x}{:02x}",
                self.inner.color.r, self.inner.color.g, self.inner.color.b
            ),
            "opacity" => format!("{}", self.inner.opacity),
            _ => String::new(),
        }
    }

    pub fn to_style(&self) -> Style {
        self.inner.clone()
    }
}

impl Default for CSSStyleDeclaration {
    fn default() -> Self {
        Self::new()
    }
}

fn parse_px(value: &str) -> Option<f32> {
    let v = value.trim().trim_end_matches("px");
    v.parse().ok()
}

fn parse_display(value: &str) -> Display {
    match value.trim() {
        "flex" => Display::Flex,
        "grid" => Display::Grid,
        "block" => Display::Block,
        "inline" => Display::Inline,
        "inline-block" => Display::InlineBlock,
        "none" => Display::None,
        _ => Display::Flex,
    }
}

fn parse_position(value: &str) -> Position {
    match value.trim() {
        "relative" => Position::Relative,
        "absolute" => Position::Absolute,
        "fixed" => Position::Fixed,
        "sticky" => Position::Sticky,
        _ => Position::Relative,
    }
}

fn parse_flex_direction(value: &str) -> FlexDirection {
    match value.trim() {
        "row" => FlexDirection::Row,
        "column" => FlexDirection::Column,
        "row-reverse" => FlexDirection::RowReverse,
        "column-reverse" => FlexDirection::ColumnReverse,
        _ => FlexDirection::Column,
    }
}

fn parse_justify_content(value: &str) -> JustifyContent {
    match value.trim() {
        "flex-start" | "start" => JustifyContent::FlexStart,
        "flex-end" | "end" => JustifyContent::FlexEnd,
        "center" => JustifyContent::Center,
        "space-between" => JustifyContent::SpaceBetween,
        "space-around" => JustifyContent::SpaceAround,
        "space-evenly" => JustifyContent::SpaceEvenly,
        _ => JustifyContent::FlexStart,
    }
}

fn parse_align_items(value: &str) -> AlignItems {
    match value.trim() {
        "flex-start" | "start" => AlignItems::FlexStart,
        "flex-end" | "end" => AlignItems::FlexEnd,
        "center" => AlignItems::Center,
        "stretch" => AlignItems::Stretch,
        "baseline" => AlignItems::Baseline,
        _ => AlignItems::Stretch,
    }
}

fn parse_flex_wrap(value: &str) -> FlexWrap {
    match value.trim() {
        "nowrap" => FlexWrap::NoWrap,
        "wrap" => FlexWrap::Wrap,
        "wrap-reverse" => FlexWrap::WrapReverse,
        _ => FlexWrap::NoWrap,
    }
}

fn parse_overflow(value: &str) -> Overflow {
    match value.trim() {
        "visible" => Overflow::Visible,
        "hidden" => Overflow::Hidden,
        "scroll" => Overflow::Scroll,
        "auto" => Overflow::Auto,
        _ => Overflow::Visible,
    }
}

fn parse_dimension(value: &str) -> Dimension {
    let v = value.trim();
    if v == "auto" {
        return Dimension::Auto;
    }
    if let Some(n) = v.strip_suffix("rem")
        && let Ok(n) = n.trim().parse()
    {
        return Dimension::Rem(n);
    }
    if let Some(n) = v.strip_suffix("em")
        && let Ok(n) = n.trim().parse()
    {
        return Dimension::Em(n);
    }
    if let Some(n) = v.strip_suffix("vw")
        && let Ok(n) = n.trim().parse()
    {
        return Dimension::Vw(n);
    }
    if let Some(n) = v.strip_suffix("vh")
        && let Ok(n) = n.trim().parse()
    {
        return Dimension::Vh(n);
    }
    if let Some(n) = v.strip_suffix('%')
        && let Ok(n) = n.trim().parse()
    {
        return Dimension::Percent(n);
    }
    if let Some(px) = parse_px(v) {
        return Dimension::Px(px);
    }
    Dimension::Auto
}

fn parse_box_shadow(value: &str) -> Option<w3cos_std::style::BoxShadow> {
    // Format: "4px 4px 10px 0px rgba(0,0,0,0.5)" or "4 4 10 0 #000000"
    let parts: Vec<&str> = value.trim().splitn(5, ' ').collect();
    if parts.len() < 4 {
        return None;
    }
    let ox = parse_px(parts[0])?;
    let oy = parse_px(parts[1])?;
    let blur = parse_px(parts[2])?;
    let spread = parse_px(parts.get(3).unwrap_or(&"0"));
    let color = if let Some(c) = parts.get(4) {
        Color::from_hex(c)
    } else {
        Color::rgba(0, 0, 0, 80)
    };
    Some(w3cos_std::style::BoxShadow::new(
        ox,
        oy,
        blur,
        spread.unwrap_or(0.0),
        color,
    ))
}

fn parse_transform(value: &str) -> w3cos_std::style::Transform2D {
    let mut t = w3cos_std::style::Transform2D::IDENTITY;
    let v = value.trim();

    // translateX(10px)
    if let Some(inner) = extract_fn(v, "translateX") {
        t.translate_x = parse_px(inner).unwrap_or(0.0);
    }
    if let Some(inner) = extract_fn(v, "translateY") {
        t.translate_y = parse_px(inner).unwrap_or(0.0);
    }
    // translate(10px, 20px)
    if let Some(inner) = extract_fn(v, "translate") {
        let parts: Vec<&str> = inner.split(',').collect();
        if let Some(x) = parts.first().and_then(|s| parse_px(s.trim())) {
            t.translate_x = x;
        }
        if let Some(y) = parts.get(1).and_then(|s| parse_px(s.trim())) {
            t.translate_y = y;
        }
    }
    // scale(1.5) or scale(1.5, 2.0)
    if let Some(inner) = extract_fn(v, "scale") {
        let parts: Vec<&str> = inner.split(',').collect();
        if let Ok(sx) = parts[0].trim().parse::<f32>() {
            t.scale_x = sx;
            t.scale_y = parts
                .get(1)
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(sx);
        }
    }
    if let Some(inner) = extract_fn(v, "scaleX")
        && let Ok(s) = inner.trim().parse::<f32>()
    {
        t.scale_x = s;
    }
    if let Some(inner) = extract_fn(v, "scaleY")
        && let Ok(s) = inner.trim().parse::<f32>()
    {
        t.scale_y = s;
    }
    // rotate(45deg)
    if let Some(inner) = extract_fn(v, "rotate") {
        let deg_str = inner.trim().trim_end_matches("deg").trim_end_matches("rad");
        if let Ok(d) = deg_str.parse::<f32>() {
            t.rotate_deg = if inner.contains("rad") {
                d.to_degrees()
            } else {
                d
            };
        }
    }

    t
}

fn extract_fn<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    let start = s.find(name)?;
    let rest = &s[start + name.len()..];
    let open = rest.find('(')?;
    let close = rest.find(')')?;
    Some(&rest[open + 1..close])
}

fn parse_text_align(value: &str) -> w3cos_std::style::TextAlign {
    use w3cos_std::style::TextAlign;
    match value.trim() {
        "left" => TextAlign::Left,
        "right" => TextAlign::Right,
        "center" => TextAlign::Center,
        "justify" => TextAlign::Justify,
        _ => TextAlign::Left,
    }
}

fn parse_white_space(value: &str) -> w3cos_std::style::WhiteSpace {
    use w3cos_std::style::WhiteSpace;
    match value.trim() {
        "normal" => WhiteSpace::Normal,
        "nowrap" => WhiteSpace::NoWrap,
        "pre" => WhiteSpace::Pre,
        "pre-wrap" => WhiteSpace::PreWrap,
        "pre-line" => WhiteSpace::PreLine,
        _ => WhiteSpace::Normal,
    }
}

fn parse_text_decoration(value: &str) -> w3cos_std::style::TextDecoration {
    use w3cos_std::style::TextDecoration;
    match value.trim() {
        "none" => TextDecoration::None,
        "underline" => TextDecoration::Underline,
        "overline" => TextDecoration::Overline,
        "line-through" => TextDecoration::LineThrough,
        _ => TextDecoration::None,
    }
}

fn parse_text_overflow(value: &str) -> w3cos_std::style::TextOverflow {
    use w3cos_std::style::TextOverflow;
    match value.trim() {
        "clip" => TextOverflow::Clip,
        "ellipsis" => TextOverflow::Ellipsis,
        _ => TextOverflow::Clip,
    }
}

fn parse_font_style(value: &str) -> w3cos_std::style::FontStyle {
    use w3cos_std::style::FontStyle;
    match value.trim() {
        "normal" => FontStyle::Normal,
        "italic" => FontStyle::Italic,
        "oblique" => FontStyle::Oblique,
        _ => FontStyle::Normal,
    }
}

fn parse_word_break(value: &str) -> w3cos_std::style::WordBreak {
    use w3cos_std::style::WordBreak;
    match value.trim() {
        "normal" => WordBreak::Normal,
        "break-all" => WordBreak::BreakAll,
        "keep-all" => WordBreak::KeepAll,
        "break-word" => WordBreak::BreakWord,
        _ => WordBreak::Normal,
    }
}

fn parse_cursor(value: &str) -> w3cos_std::style::Cursor {
    use w3cos_std::style::Cursor;
    match value.trim() {
        "default" => Cursor::Default,
        "pointer" => Cursor::Pointer,
        "text" => Cursor::Text,
        "move" => Cursor::Move,
        "grab" => Cursor::Grab,
        "grabbing" => Cursor::Grabbing,
        "not-allowed" => Cursor::NotAllowed,
        "crosshair" => Cursor::Crosshair,
        "help" => Cursor::Help,
        "wait" => Cursor::Wait,
        "progress" => Cursor::Progress,
        "col-resize" => Cursor::ColResize,
        "row-resize" => Cursor::RowResize,
        "none" => Cursor::None,
        _ => Cursor::Default,
    }
}

fn parse_pointer_events(value: &str) -> w3cos_std::style::PointerEvents {
    use w3cos_std::style::PointerEvents;
    match value.trim() {
        "auto" => PointerEvents::Auto,
        "none" => PointerEvents::None,
        _ => PointerEvents::Auto,
    }
}

fn parse_user_select(value: &str) -> w3cos_std::style::UserSelect {
    use w3cos_std::style::UserSelect;
    match value.trim() {
        "auto" => UserSelect::Auto,
        "none" => UserSelect::None,
        "text" => UserSelect::Text,
        "all" => UserSelect::All,
        _ => UserSelect::Auto,
    }
}

fn parse_visibility(value: &str) -> w3cos_std::style::Visibility {
    use w3cos_std::style::Visibility;
    match value.trim() {
        "visible" => Visibility::Visible,
        "hidden" => Visibility::Hidden,
        "collapse" => Visibility::Collapse,
        _ => Visibility::Visible,
    }
}

fn parse_align_self(value: &str) -> w3cos_std::style::AlignSelf {
    use w3cos_std::style::AlignSelf;
    match value.trim() {
        "auto" => AlignSelf::Auto,
        "flex-start" | "start" => AlignSelf::FlexStart,
        "flex-end" | "end" => AlignSelf::FlexEnd,
        "center" => AlignSelf::Center,
        "baseline" => AlignSelf::Baseline,
        "stretch" => AlignSelf::Stretch,
        _ => AlignSelf::Auto,
    }
}

fn parse_align_content(value: &str) -> w3cos_std::style::AlignContent {
    use w3cos_std::style::AlignContent;
    match value.trim() {
        "flex-start" | "start" => AlignContent::FlexStart,
        "flex-end" | "end" => AlignContent::FlexEnd,
        "center" => AlignContent::Center,
        "space-between" => AlignContent::SpaceBetween,
        "space-around" => AlignContent::SpaceAround,
        "space-evenly" => AlignContent::SpaceEvenly,
        "stretch" => AlignContent::Stretch,
        _ => AlignContent::Stretch,
    }
}

fn parse_outline_style(value: &str) -> w3cos_std::style::OutlineStyle {
    use w3cos_std::style::OutlineStyle;
    match value.trim() {
        "none" => OutlineStyle::None,
        "solid" => OutlineStyle::Solid,
        "dashed" => OutlineStyle::Dashed,
        "dotted" => OutlineStyle::Dotted,
        "double" => OutlineStyle::Double,
        _ => OutlineStyle::None,
    }
}

fn parse_transition(value: &str) -> Option<w3cos_std::style::Transition> {
    use w3cos_std::style::{Easing, Transition, TransitionProperty};
    let parts: Vec<&str> = value.split_whitespace().collect();
    if parts.is_empty() {
        return None;
    }

    let property = match parts[0] {
        "all" => TransitionProperty::All,
        "opacity" => TransitionProperty::Opacity,
        "transform" => TransitionProperty::Transform,
        "background" | "background-color" => TransitionProperty::Background,
        "color" => TransitionProperty::Color,
        p => TransitionProperty::Custom(p.to_string()),
    };

    let duration_ms = parts
        .get(1)
        .and_then(|s| {
            if let Some(ms) = s.strip_suffix("ms") {
                ms.parse().ok()
            } else if let Some(sec) = s.strip_suffix('s') {
                sec.parse::<f32>().ok().map(|v| (v * 1000.0) as u32)
            } else {
                s.parse().ok()
            }
        })
        .unwrap_or(300);

    let easing = parts
        .get(2)
        .map(|s| match *s {
            "linear" => Easing::Linear,
            "ease" => Easing::Ease,
            "ease-in" => Easing::EaseIn,
            "ease-out" => Easing::EaseOut,
            "ease-in-out" => Easing::EaseInOut,
            _ => Easing::Ease,
        })
        .unwrap_or(Easing::Ease);

    let delay_ms = parts
        .get(3)
        .and_then(|s| {
            if let Some(ms) = s.strip_suffix("ms") {
                ms.parse().ok()
            } else if let Some(sec) = s.strip_suffix('s') {
                sec.parse::<f32>().ok().map(|v| (v * 1000.0) as u32)
            } else {
                None
            }
        })
        .unwrap_or(0);

    Some(Transition {
        property,
        duration_ms,
        easing,
        delay_ms,
    })
}
