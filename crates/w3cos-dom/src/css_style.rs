use w3cos_std::color::Color;
use w3cos_std::safe_area::SafeAreaEdge;
use w3cos_std::style::{
    AlignItems, BoxSizing, Contain, Dimension, Display, Edges, FlexDirection, FlexWrap, Float,
    JustifyContent, Overflow, Position, Spacing, Style, WillChange,
};

/// CSSStyleDeclaration — the `element.style` property.
/// Mutable handle that writes directly to the node's Style.
#[derive(Debug, Clone)]
pub struct CSSStyleDeclaration {
    pub inner: Style,
    /// Raw `(property, value)` pairs applied through `set_property`, in order.
    /// `Document::to_component_tree` re-applies these above stylesheet-matched
    /// rules so that inline style wins the cascade.
    pub inline_declarations: Vec<(String, String)>,
}

impl CSSStyleDeclaration {
    pub fn new() -> Self {
        Self {
            inner: Style::default(),
            inline_declarations: Vec::new(),
        }
    }

    pub fn from_style(style: Style) -> Self {
        Self {
            inner: style,
            inline_declarations: Vec::new(),
        }
    }

    pub fn set_property(&mut self, name: &str, value: &str) {
        self.inline_declarations
            .push((name.to_string(), value.to_string()));
        if name.starts_with("--") {
            self.inner
                .custom_properties
                .get_or_insert_with(Default::default)
                .insert(name.to_string(), value.to_string());
            return;
        }
        match name {
            "display" => self.inner.display = parse_display(value),
            "position" => self.inner.position = parse_position(value),
            "float" | "cssFloat" => self.inner.float = parse_float(value),

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
            "flex" => apply_flex_shorthand(&mut self.inner, value),

            "gap" => {
                let parts = split_css_whitespace(value);
                if let Some(v) = parts.first().and_then(|part| parse_px(part)) {
                    self.inner.row_gap = Some(v);
                    self.inner.column_gap =
                        Some(parts.get(1).and_then(|part| parse_px(part)).unwrap_or(v));
                    self.inner.gap = v;
                }
            }
            "row-gap" | "rowGap" => self.inner.row_gap = parse_px(value),
            "column-gap" | "columnGap" => self.inner.column_gap = parse_px(value),
            "padding" => {
                if let Some(edges) = parse_padding_shorthand(value) {
                    self.inner.padding = edges
                }
            }
            "padding-top" | "paddingTop" => {
                if let Some(v) = parse_padding_spacing(value) {
                    self.inner.padding.top = v
                }
            }
            "padding-right" | "paddingRight" => {
                if let Some(v) = parse_padding_spacing(value) {
                    self.inner.padding.right = v
                }
            }
            "padding-bottom" | "paddingBottom" => {
                if let Some(v) = parse_padding_spacing(value) {
                    self.inner.padding.bottom = v
                }
            }
            "padding-left" | "paddingLeft" => {
                if let Some(v) = parse_padding_spacing(value) {
                    self.inner.padding.left = v
                }
            }
            "padding-inline" | "paddingInline" => {
                let values = split_css_whitespace(value);
                if let Some(start) = values.first().and_then(|value| parse_padding_spacing(value)) {
                    let end = values
                        .get(1)
                        .and_then(|value| parse_padding_spacing(value))
                        .unwrap_or(start);
                    self.inner.padding.left = start;
                    self.inner.padding.right = end;
                }
            }
            "padding-inline-start" | "paddingInlineStart" => {
                if let Some(v) = parse_padding_spacing(value) {
                    self.inner.padding.left = v
                }
            }
            "padding-inline-end" | "paddingInlineEnd" => {
                if let Some(v) = parse_padding_spacing(value) {
                    self.inner.padding.right = v
                }
            }
            "padding-block" | "paddingBlock" => {
                let values = split_css_whitespace(value);
                if let Some(start) = values.first().and_then(|value| parse_padding_spacing(value)) {
                    let end = values
                        .get(1)
                        .and_then(|value| parse_padding_spacing(value))
                        .unwrap_or(start);
                    self.inner.padding.top = start;
                    self.inner.padding.bottom = end;
                }
            }
            "margin" => {
                if let Some(edges) = parse_margin_shorthand(value) {
                    self.inner.margin = edges
                }
            }
            "margin-top" | "marginTop" => {
                if let Some(v) = parse_spacing(value) {
                    self.inner.margin.top = v
                }
            }
            "margin-right" | "marginRight" => {
                if let Some(v) = parse_spacing(value) {
                    self.inner.margin.right = v
                }
            }
            "margin-bottom" | "marginBottom" => {
                if let Some(v) = parse_spacing(value) {
                    self.inner.margin.bottom = v
                }
            }
            "margin-left" | "marginLeft" => {
                if let Some(v) = parse_spacing(value) {
                    self.inner.margin.left = v
                }
            }
            "margin-inline" | "marginInline" => {
                let values = split_css_whitespace(value);
                if let Some(start) = values.first().and_then(|value| parse_spacing(value)) {
                    let end = values
                        .get(1)
                        .and_then(|value| parse_spacing(value))
                        .unwrap_or(start);
                    self.inner.margin.left = start;
                    self.inner.margin.right = end;
                }
            }
            "margin-inline-start" | "marginInlineStart" => {
                if let Some(v) = parse_spacing(value) {
                    self.inner.margin.left = v
                }
            }
            "margin-inline-end" | "marginInlineEnd" => {
                if let Some(v) = parse_spacing(value) {
                    self.inner.margin.right = v
                }
            }
            "margin-block" | "marginBlock" => {
                let values = split_css_whitespace(value);
                if let Some(start) = values.first().and_then(|value| parse_spacing(value)) {
                    let end = values
                        .get(1)
                        .and_then(|value| parse_spacing(value))
                        .unwrap_or(start);
                    self.inner.margin.top = start;
                    self.inner.margin.bottom = end;
                }
            }

            "box-sizing" | "boxSizing" => {
                self.inner.box_sizing = if value.trim() == "border-box" {
                    BoxSizing::BorderBox
                } else {
                    BoxSizing::ContentBox
                }
            }
            "width" => {
                if let Some((width, max_width)) = parse_min_percent_and_fixed(value) {
                    self.inner.width = width;
                    self.inner.max_width = max_width;
                } else {
                    self.inner.width = parse_dimension(value);
                }
            }
            "height" => {
                if let Some((height, max_height)) = parse_min_percent_and_fixed(value) {
                    self.inner.height = height;
                    self.inner.max_height = max_height;
                } else {
                    self.inner.height = parse_dimension(value);
                }
            }
            "min-width" | "minWidth" => self.inner.min_width = parse_dimension(value),
            "min-height" | "minHeight" => self.inner.min_height = parse_dimension(value),
            "max-width" | "maxWidth" => self.inner.max_width = parse_dimension(value),
            "max-height" | "maxHeight" => self.inner.max_height = parse_dimension(value),

            "top" => self.inner.top = parse_dimension(value),
            "right" => self.inner.right = parse_dimension(value),
            "bottom" => self.inner.bottom = parse_dimension(value),
            "left" => self.inner.left = parse_dimension(value),
            "inset" => {
                let values: Vec<Dimension> =
                    value.split_whitespace().map(parse_dimension).collect();
                if let Some((top, right, bottom, left)) = match values.as_slice() {
                    [all] => Some((*all, *all, *all, *all)),
                    [vertical, horizontal] => {
                        Some((*vertical, *horizontal, *vertical, *horizontal))
                    }
                    [top, horizontal, bottom] => Some((*top, *horizontal, *bottom, *horizontal)),
                    [top, right, bottom, left] => Some((*top, *right, *bottom, *left)),
                    _ => None,
                } {
                    self.inner.top = top;
                    self.inner.right = right;
                    self.inner.bottom = bottom;
                    self.inner.left = left;
                }
            }
            "z-index" | "zIndex" => {
                if let Ok(v) = value.parse() {
                    self.inner.z_index = v
                }
            }

            "overflow" => self.inner.overflow = parse_overflow(value),
            "overflow-x" | "overflowX" => self.inner.overflow_x = Some(parse_overflow(value)),
            "overflow-y" | "overflowY" => self.inner.overflow_y = Some(parse_overflow(value)),
            "overflow-anchor" | "overflowAnchor" => {
                self.inner.overflow_anchor = !value.trim().eq_ignore_ascii_case("none")
            }

            "background-color" | "backgroundColor" => {
                if let Some(color) = Color::from_css(value) {
                    self.inner.background = color;
                }
            }
            "background-image" | "backgroundImage" => {
                if value.trim().eq_ignore_ascii_case("none") {
                    self.inner.background_image = None;
                } else if value.contains("gradient(") || value.contains("url(") {
                    self.inner.background_image = Some(value.trim().to_string());
                }
            }
            "background-size" | "backgroundSize" => {
                self.inner.background_size = css_background_value(value, "auto")
            }
            "background-position" | "backgroundPosition" => {
                self.inner.background_position = css_background_value(value, "0% 0%")
            }
            "background-repeat" | "backgroundRepeat" => {
                self.inner.background_repeat = css_background_value(value, "repeat")
            }
            "background-origin" | "backgroundOrigin" => {
                self.inner.background_origin = css_background_value(value, "padding-box")
            }
            "background-clip" | "backgroundClip" => {
                self.inner.background_clip = css_background_value(value, "border-box")
            }
            "background-attachment" | "backgroundAttachment" => {
                self.inner.background_attachment = css_background_value(value, "scroll")
            }
            "background-blend-mode" | "backgroundBlendMode" => {
                self.inner.background_blend_mode = css_background_value(value, "normal")
            }
            "background" => {
                apply_background_shorthand(&mut self.inner, value);
            }
            "color" => {
                if let Some(color) = Color::from_css(value) {
                    self.inner.color = color
                }
            }
            "font-size" | "fontSize" => {
                if let Some(v) = parse_px(value) {
                    self.inner.font_size = v
                }
            }
            "font" => apply_font_shorthand(&mut self.inner, value),
            "font-weight" | "fontWeight" => {
                if let Ok(v) = value.parse() {
                    self.inner.font_weight = v
                }
            }
            "border-radius" | "borderRadius" => {
                let values = split_css_whitespace(value)
                    .iter()
                    .filter_map(|part| parse_px(part))
                    .collect::<Vec<_>>();
                if let Some([top_left, top_right, bottom_right, bottom_left]) =
                    expand_border_radius(&values)
                {
                    self.inner.border_radius = top_left;
                    self.inner.border_top_left_radius = Some(top_left);
                    self.inner.border_top_right_radius = Some(top_right);
                    self.inner.border_bottom_right_radius = Some(bottom_right);
                    self.inner.border_bottom_left_radius = Some(bottom_left);
                }
            }
            "border-width" | "borderWidth" => {
                if let Some(v) = parse_px(value) {
                    self.inner.border_width = v
                }
            }
            "border-top-width" | "borderTopWidth" => self.inner.border_top_width = parse_px(value),
            "border-right-width" | "borderRightWidth" => {
                self.inner.border_right_width = parse_px(value)
            }
            "border-bottom-width" | "borderBottomWidth" => {
                self.inner.border_bottom_width = parse_px(value)
            }
            "border-left-width" | "borderLeftWidth" => {
                self.inner.border_left_width = parse_px(value)
            }
            "border-style" | "borderStyle" => {
                if let Some(visible) = parse_border_style_visibility(value) {
                    self.inner.border_width = if visible {
                        self.declared_uniform_border_width().unwrap_or(3.0)
                    } else {
                        0.0
                    };
                }
            }
            "border-inline-width" | "borderInlineWidth" => {
                let values = split_css_whitespace(value);
                if let Some(start) = values.first().and_then(|value| parse_px(value)) {
                    self.inner.border_left_width = Some(start);
                    self.inner.border_right_width = Some(
                        values
                            .get(1)
                            .and_then(|value| parse_px(value))
                            .unwrap_or(start),
                    );
                }
            }
            "border-color" | "borderColor" => {
                if let Some(color) = Color::from_css(value) {
                    self.inner.border_color = color
                }
            }
            "border-top-color" | "borderTopColor" => {
                self.inner.border_top_color = Color::from_css(value)
            }
            "border-right-color" | "borderRightColor" => {
                self.inner.border_right_color = Color::from_css(value)
            }
            "border-bottom-color" | "borderBottomColor" => {
                self.inner.border_bottom_color = Color::from_css(value)
            }
            "border-left-color" | "borderLeftColor" => {
                self.inner.border_left_color = Color::from_css(value)
            }
            "border" => apply_border_shorthand(&mut self.inner, value),
            "border-top" | "borderTop" => {
                apply_border_side_shorthand(&mut self.inner, value, BorderSide::Top)
            }
            "border-right" | "borderRight" => {
                apply_border_side_shorthand(&mut self.inner, value, BorderSide::Right)
            }
            "border-bottom" | "borderBottom" => {
                apply_border_side_shorthand(&mut self.inner, value, BorderSide::Bottom)
            }
            "border-left" | "borderLeft" => {
                apply_border_side_shorthand(&mut self.inner, value, BorderSide::Left)
            }
            "border-inline-start" | "borderInlineStart" => {
                apply_border_side_shorthand(&mut self.inner, value, BorderSide::Left)
            }
            "border-inline-end" | "borderInlineEnd" => {
                apply_border_side_shorthand(&mut self.inner, value, BorderSide::Right)
            }
            "border-block-start" | "borderBlockStart" => {
                apply_border_side_shorthand(&mut self.inner, value, BorderSide::Top)
            }
            "border-block-end" | "borderBlockEnd" => {
                apply_border_side_shorthand(&mut self.inner, value, BorderSide::Bottom)
            }
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

            // Compositor hints (standard CSS — UA picks GPU/CPU internally)
            "will-change" | "willChange" => {
                self.inner.will_change = WillChange::from_css(value);
            }
            "contain" => {
                self.inner.contain = Contain::from_css(value);
            }
            "filter" => {
                let v = value.trim();
                if v.is_empty() || v.eq_ignore_ascii_case("none") {
                    self.inner.filter = None;
                } else {
                    self.inner.filter = Some(v.to_string());
                }
            }

            // Transition: "property duration easing"
            "transition" => {
                self.inner.transition = parse_transition(value);
            }

            "transition-delay" | "transitionDelay" => {
                if let Some(transition) = self.inner.transition.as_mut()
                    && let Some(delay_ms) = parse_time_ms(value)
                {
                    transition.delay_ms = delay_ms;
                }
            }

            // Animation shorthand and the longhand used by browser pages.
            "animation" => self.inner.animation = parse_animation(value),
            "animation-delay" | "animationDelay" => {
                if let Some(animation) = self.inner.animation.as_mut()
                    && let Some(delay_ms) = parse_time_ms(value)
                {
                    animation.delay_ms = delay_ms;
                }
            }

            // Text properties
            "text-align" | "textAlign" => self.inner.text_align = parse_text_align(value),
            "white-space" | "whiteSpace" => self.inner.white_space = parse_white_space(value),
            "line-height" | "lineHeight" => {
                let value = value.trim();
                if let Some(px) = value.strip_suffix("px").and_then(|v| v.parse::<f32>().ok()) {
                    self.inner.line_height = (px / self.inner.font_size.max(1.0)).max(0.0);
                } else if let Ok(v) = value.parse::<f32>() {
                    self.inner.line_height = v.max(0.0);
                }
            }
            "letter-spacing" | "letterSpacing" => {
                if let Some(v) = parse_px(value) {
                    self.inner.letter_spacing = v;
                }
            }
            "text-decoration" | "textDecoration" => {
                self.inner.text_decoration = parse_text_decoration(value)
            }
            "text-overflow" | "textOverflow" => {
                self.inner.text_overflow = parse_text_overflow(value)
            }
            "font-family" | "fontFamily" => {
                self.inner.font_family =
                    Some(value.trim_matches('"').trim_matches('\'').to_string());
            }
            "font-style" | "fontStyle" => self.inner.font_style = parse_font_style(value),
            "word-break" | "wordBreak" => self.inner.word_break = parse_word_break(value),
            "overflow-wrap" | "overflowWrap" | "word-wrap" | "wordWrap" => {
                self.inner.word_break = parse_overflow_wrap(value)
            }

            // Interaction
            "cursor" => self.inner.cursor = parse_cursor(value),
            "pointer-events" | "pointerEvents" => {
                self.inner.pointer_events = parse_pointer_events(value)
            }
            "user-select" | "userSelect" => self.inner.user_select = parse_user_select(value),

            // Visibility
            "visibility" => self.inner.visibility = parse_visibility(value),

            // Flex extras
            "flex-basis" | "flexBasis" => self.inner.flex_basis = parse_dimension(value),
            "order" => {
                if let Ok(v) = value.parse() {
                    self.inner.order = v
                }
            }
            "align-self" | "alignSelf" => self.inner.align_self = parse_align_self(value),
            "vertical-align" | "verticalAlign" => {
                self.inner.align_self = match value.trim() {
                    "top" | "text-top" => w3cos_std::style::AlignSelf::FlexStart,
                    "bottom" | "text-bottom" => w3cos_std::style::AlignSelf::FlexEnd,
                    "middle" => w3cos_std::style::AlignSelf::Center,
                    _ => w3cos_std::style::AlignSelf::Baseline,
                }
            }
            "align-content" | "alignContent" => {
                self.inner.align_content = parse_align_content(value)
            }
            "justify-self" | "justifySelf" => self.inner.justify_self = parse_align_self(value),
            "justify-items" | "justifyItems" => self.inner.justify_items = parse_align_items(value),
            "place-items" | "placeItems" => {
                let mut values = value.split_whitespace();
                if let Some(align) = values.next() {
                    self.inner.align_items = parse_align_items(align);
                    self.inner.justify_items = parse_align_items(values.next().unwrap_or(align));
                }
            }
            "grid-template-columns" | "gridTemplateColumns" => {
                self.inner.grid_template_columns = Some(value.trim().to_string())
            }
            "grid-column" | "gridColumn" => self.inner.grid_column = Some(value.trim().to_string()),

            // Outline
            "outline-width" | "outlineWidth" => {
                if let Some(v) = parse_px(value) {
                    self.inner.outline_width = v
                }
            }
            "outline-color" | "outlineColor" => {
                if let Some(color) = Color::from_css(value) {
                    self.inner.outline_color = color
                }
            }
            "outline-style" | "outlineStyle" => {
                self.inner.outline_style = parse_outline_style(value)
            }

            _ => {}
        }
    }

    pub fn get_property(&self, name: &str) -> String {
        match name {
            "display" => match self.inner.display {
                Display::Block => "block".to_string(),
                Display::Flex => "flex".to_string(),
                Display::Grid => "grid".to_string(),
                Display::Inline => "inline".to_string(),
                Display::InlineBlock => "inline-block".to_string(),
                Display::InlineFlex => "inline-flex".to_string(),
                Display::Table => "table".to_string(),
                Display::InlineTable => "inline-table".to_string(),
                Display::TableRowGroup => "table-row-group".to_string(),
                Display::TableHeaderGroup => "table-header-group".to_string(),
                Display::TableFooterGroup => "table-footer-group".to_string(),
                Display::TableRow => "table-row".to_string(),
                Display::TableColumnGroup => "table-column-group".to_string(),
                Display::TableColumn => "table-column".to_string(),
                Display::TableCell => "table-cell".to_string(),
                Display::TableCaption => "table-caption".to_string(),
                Display::ListItem => "list-item".to_string(),
                Display::Contents => "contents".to_string(),
                Display::None => "none".to_string(),
            },
            "position" => format!("{:?}", self.inner.position).to_lowercase(),
            "float" | "cssFloat" => match self.inner.float {
                Float::None => "none".to_string(),
                Float::Left => "left".to_string(),
                Float::Right => "right".to_string(),
            },
            "vertical-align" | "verticalAlign" => match self.inner.align_self {
                w3cos_std::style::AlignSelf::FlexStart => "top".to_string(),
                w3cos_std::style::AlignSelf::FlexEnd => "bottom".to_string(),
                w3cos_std::style::AlignSelf::Center => "middle".to_string(),
                _ => "baseline".to_string(),
            },
            "font-size" | "fontSize" => format!("{}px", self.inner.font_size),
            "color" => format!(
                "#{:02x}{:02x}{:02x}",
                self.inner.color.r, self.inner.color.g, self.inner.color.b
            ),
            "background-color" | "backgroundColor" => format!(
                "#{:02x}{:02x}{:02x}",
                self.inner.background.r, self.inner.background.g, self.inner.background.b
            ),
            "background-image" | "backgroundImage" => self
                .inner
                .background_image
                .clone()
                .unwrap_or_else(|| "none".to_string()),
            "background-size" | "backgroundSize" => self
                .inner
                .background_size
                .clone()
                .unwrap_or_else(|| "auto".to_string()),
            "background-position" | "backgroundPosition" => self
                .inner
                .background_position
                .clone()
                .unwrap_or_else(|| "0% 0%".to_string()),
            "background-repeat" | "backgroundRepeat" => self
                .inner
                .background_repeat
                .clone()
                .unwrap_or_else(|| "repeat".to_string()),
            "background-origin" | "backgroundOrigin" => self
                .inner
                .background_origin
                .clone()
                .unwrap_or_else(|| "padding-box".to_string()),
            "background-clip" | "backgroundClip" => self
                .inner
                .background_clip
                .clone()
                .unwrap_or_else(|| "border-box".to_string()),
            "background-attachment" | "backgroundAttachment" => self
                .inner
                .background_attachment
                .clone()
                .unwrap_or_else(|| "scroll".to_string()),
            "background-blend-mode" | "backgroundBlendMode" => self
                .inner
                .background_blend_mode
                .clone()
                .unwrap_or_else(|| "normal".to_string()),
            "opacity" => format!("{}", self.inner.opacity),
            "top" => dimension_to_css(&self.inner.top),
            "right" => dimension_to_css(&self.inner.right),
            "bottom" => dimension_to_css(&self.inner.bottom),
            "left" => dimension_to_css(&self.inner.left),
            "transform" => transform_to_css(self.inner.transform),
            "transition" => self
                .inner
                .transition
                .as_ref()
                .map(transition_to_css)
                .unwrap_or_else(|| "none".to_string()),
            "animation" => self
                .inner
                .animation
                .as_ref()
                .map(animation_to_css)
                .unwrap_or_else(|| "none".to_string()),
            "width" => dimension_to_css(&self.inner.width),
            "height" => dimension_to_css(&self.inner.height),
            "min-width" | "minWidth" => dimension_to_css(&self.inner.min_width),
            "min-height" | "minHeight" => dimension_to_css(&self.inner.min_height),
            "max-width" | "maxWidth" => dimension_to_css(&self.inner.max_width),
            "max-height" | "maxHeight" => dimension_to_css(&self.inner.max_height),
            "flex-grow" | "flexGrow" => format!("{}", self.inner.flex_grow),
            "flex-shrink" | "flexShrink" => format!("{}", self.inner.flex_shrink),
            "flex-direction" | "flexDirection" => {
                format!("{:?}", self.inner.flex_direction).to_lowercase()
            }
            "justify-content" | "justifyContent" => {
                format!("{:?}", self.inner.justify_content).to_lowercase()
            }
            "align-items" | "alignItems" => format!("{:?}", self.inner.align_items).to_lowercase(),
            "overflow" => format!("{:?}", self.inner.overflow).to_lowercase(),
            "overflow-x" | "overflowX" => {
                format!("{:?}", self.inner.resolved_overflow_x()).to_lowercase()
            }
            "overflow-y" | "overflowY" => {
                format!("{:?}", self.inner.resolved_overflow_y()).to_lowercase()
            }
            "box-sizing" | "boxSizing" => match self.inner.box_sizing {
                BoxSizing::ContentBox => "content-box".to_string(),
                BoxSizing::BorderBox => "border-box".to_string(),
            },
            "will-change" | "willChange" => will_change_to_css(&self.inner.will_change),
            "contain" => contain_to_css(self.inner.contain),
            "filter" => self
                .inner
                .filter
                .clone()
                .unwrap_or_else(|| "none".to_string()),
            _ => String::new(),
        }
    }

    pub fn to_style(&self) -> Style {
        self.inner.clone()
    }

    fn declared_uniform_border_width(&self) -> Option<f32> {
        self.inline_declarations
            .iter()
            .rev()
            .find_map(|(name, value)| {
                if matches!(name.as_str(), "border-width" | "borderWidth") {
                    split_css_whitespace(value)
                        .first()
                        .and_then(|part| parse_px(part))
                } else if name == "border" {
                    split_css_whitespace(value)
                        .iter()
                        .find_map(|part| parse_px(part))
                } else {
                    None
                }
            })
    }
}

fn css_background_value(value: &str, initial: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.eq_ignore_ascii_case(initial) {
        None
    } else {
        Some(value.to_string())
    }
}

fn apply_background_shorthand(style: &mut Style, value: &str) {
    let parsed = w3cos_std::background::parse_shorthand(value);
    style.background = parsed.color.unwrap_or(Color::TRANSPARENT);
    style.background_image = Some(parsed.images.join(", "));
    style.background_size = css_background_value(&parsed.sizes.join(", "), "auto");
    style.background_position = css_background_value(&parsed.positions.join(", "), "0% 0%");
    style.background_repeat = css_background_value(&parsed.repeats.join(", "), "repeat");
    style.background_origin = css_background_value(&parsed.origins.join(", "), "padding-box");
    style.background_clip = css_background_value(&parsed.clips.join(", "), "border-box");
    style.background_attachment = css_background_value(&parsed.attachments.join(", "), "scroll");
    // `background` resets every longhand, including the separately parsed blend mode.
    style.background_blend_mode = None;
}

fn parse_padding_spacing(value: &str) -> Option<Spacing> {
    let value = value.trim();
    let environments = [
        ("env(safe-area-inset-top)", SafeAreaEdge::Top),
        ("env(safe-area-inset-right)", SafeAreaEdge::Right),
        ("env(safe-area-inset-bottom)", SafeAreaEdge::Bottom),
        ("env(safe-area-inset-left)", SafeAreaEdge::Left),
    ];
    if let Some((_, edge)) = environments
        .iter()
        .find(|(environment, _)| value == *environment)
    {
        return Some(Spacing::SafeAreaInset(*edge));
    }
    if let Some(inner) = value
        .strip_prefix("max(")
        .and_then(|value| value.strip_suffix(')'))
        && let Some((first, second)) = split_top_level_once(inner, ',')
    {
        for (length, environment) in [(first.trim(), second.trim()), (second.trim(), first.trim())]
        {
            if let Some(px) = parse_px(length)
                && let Some((_, edge)) = environments
                    .iter()
                    .find(|(candidate, _)| environment == *candidate)
            {
                return Some(Spacing::Maximum {
                    px,
                    safe_area: *edge,
                });
            }
        }
    }
    if let Some(inner) = value
        .strip_prefix("calc(")
        .and_then(|value| value.strip_suffix(')'))
    {
        for (environment, edge) in environments {
            if inner.contains(environment)
                && let Some(px) = inner
                    .replace(environment, "")
                    .replace('+', "")
                    .trim()
                    .strip_suffix("px")
                    .and_then(|value| value.trim().parse().ok())
            {
                return Some(Spacing::Composite {
                    px,
                    safe_area: Some(edge),
                    keyboard_inset: false,
                });
            }
        }
    }
    let spacing = parse_spacing(value)?;
    match spacing {
        Spacing::Px(value)
        | Spacing::Percent(value)
        | Spacing::Rem(value)
        | Spacing::Em(value)
        | Spacing::Vw(value)
        | Spacing::Vh(value)
            if value < 0.0 => None,
        Spacing::Auto => None,
        _ => Some(spacing),
    }
}

fn split_top_level_once(value: &str, separator: char) -> Option<(&str, &str)> {
    let mut depth = 0_u32;
    for (index, character) in value.char_indices() {
        match character {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ if character == separator && depth == 0 => {
                return Some((&value[..index], &value[index + character.len_utf8()..]));
            }
            _ => {}
        }
    }
    None
}

fn parse_padding_shorthand(value: &str) -> Option<Edges> {
    let parts: Vec<Spacing> = split_css_whitespace(value)
        .iter()
        .map(|part| parse_padding_spacing(part))
        .collect::<Option<_>>()?;
    expand_edge_spacing(&parts)
}

fn parse_margin_shorthand(value: &str) -> Option<Edges> {
    let parts: Vec<Spacing> = split_css_whitespace(value)
        .iter()
        .map(|part| parse_spacing(part))
        .collect::<Option<_>>()?;
    expand_edge_spacing(&parts)
}

fn expand_edge_spacing(parts: &[Spacing]) -> Option<Edges> {
    let (top, right, bottom, left) = match parts {
        [all] => (*all, *all, *all, *all),
        [vertical, horizontal] => (*vertical, *horizontal, *vertical, *horizontal),
        [top, horizontal, bottom] => (*top, *horizontal, *bottom, *horizontal),
        [top, right, bottom, left] => (*top, *right, *bottom, *left),
        _ => return None,
    };
    Some(Edges {
        top,
        right,
        bottom,
        left,
    })
}

fn parse_spacing(value: &str) -> Option<Spacing> {
    let value = value.trim();
    if value == "auto" {
        return Some(Spacing::Auto);
    }
    for (suffix, constructor) in [
        ("rem", Spacing::Rem as fn(f32) -> Spacing),
        // `ch` is the advance of the zero glyph. The portable style IR does
        // not yet carry per-font character-relative units, so retain it as
        // the existing local-font-relative dimension. This is exact for
        // monospace/Ahem and preserves responsive scaling on every target.
        ("ch", Spacing::Em),
        ("em", Spacing::Em),
        ("vw", Spacing::Vw),
        ("dvh", Spacing::Vh),
        ("svh", Spacing::Vh),
        ("lvh", Spacing::Vh),
        ("vh", Spacing::Vh),
        ("%", Spacing::Percent),
    ] {
        if let Some(number) = value.strip_suffix(suffix)
            && let Ok(number) = number.trim().parse()
        {
            return Some(constructor(number));
        }
    }
    parse_px(value).map(Spacing::Px)
}

fn apply_flex_shorthand(style: &mut Style, value: &str) {
    let value = value.trim();
    match value {
        "none" => {
            style.flex_grow = 0.0;
            style.flex_shrink = 0.0;
            style.flex_basis = Dimension::Auto;
            return;
        }
        "auto" => {
            style.flex_grow = 1.0;
            style.flex_shrink = 1.0;
            style.flex_basis = Dimension::Auto;
            return;
        }
        "initial" => {
            style.flex_grow = 0.0;
            style.flex_shrink = 1.0;
            style.flex_basis = Dimension::Auto;
            return;
        }
        _ => {}
    }

    let parts: Vec<&str> = value.split_whitespace().collect();
    if let [grow] = parts.as_slice()
        && let Ok(grow) = grow.parse::<f32>()
    {
        style.flex_grow = grow;
        style.flex_shrink = 1.0;
        style.flex_basis = Dimension::Percent(0.0);
        return;
    }
    if let [grow, second] = parts.as_slice()
        && let Ok(grow) = grow.parse::<f32>()
    {
        style.flex_grow = grow;
        if let Ok(shrink) = second.parse::<f32>() {
            style.flex_shrink = shrink;
            style.flex_basis = Dimension::Percent(0.0);
        } else {
            style.flex_shrink = 1.0;
            style.flex_basis = parse_dimension(second);
        }
        return;
    }
    if let [grow, shrink, basis] = parts.as_slice()
        && let (Ok(grow), Ok(shrink)) = (grow.parse::<f32>(), shrink.parse::<f32>())
    {
        style.flex_grow = grow;
        style.flex_shrink = shrink;
        style.flex_basis = parse_dimension(basis);
    }
}

impl Default for CSSStyleDeclaration {
    fn default() -> Self {
        Self::new()
    }
}

fn dimension_to_css(dim: &w3cos_std::style::Dimension) -> String {
    match dim {
        w3cos_std::style::Dimension::Px(v) => format!("{v}px"),
        w3cos_std::style::Dimension::Percent(v) => format!("{v}%"),
        w3cos_std::style::Dimension::Rem(v) => format!("{v}rem"),
        w3cos_std::style::Dimension::Em(v) => format!("{v}em"),
        w3cos_std::style::Dimension::Vw(v) => format!("{v}vw"),
        w3cos_std::style::Dimension::Vh(v) => format!("{v}vh"),
        w3cos_std::style::Dimension::Auto => "auto".to_string(),
    }
}

fn parse_px(value: &str) -> Option<f32> {
    w3cos_std::style::parse_absolute_length_px(value)
}

fn will_change_to_css(wc: &WillChange) -> String {
    let mut parts = Vec::new();
    if wc.transform {
        parts.push("transform");
    }
    if wc.opacity {
        parts.push("opacity");
    }
    if wc.filter {
        parts.push("filter");
    }
    if wc.scroll_position {
        parts.push("scroll-position");
    }
    if parts.is_empty() {
        "auto".to_string()
    } else {
        parts.join(", ")
    }
}

fn contain_to_css(c: Contain) -> String {
    match c {
        Contain::None => "none".to_string(),
        Contain::Layout => "layout".to_string(),
        Contain::Size => "size".to_string(),
        Contain::Content => "content".to_string(),
        Contain::Strict => "strict".to_string(),
    }
}

fn parse_display(value: &str) -> Display {
    match value.trim() {
        "flex" => Display::Flex,
        "grid" => Display::Grid,
        "block" => Display::Block,
        "inline" => Display::Inline,
        "inline-block" => Display::InlineBlock,
        "inline-flex" => Display::InlineFlex,
        // `flow-root` creates a block formatting context. The internal
        // display model does not need a second block layout algorithm; map it
        // to Block rather than falling through to the legacy Flex default.
        "flow-root" => Display::Block,
        "table" => Display::Table,
        "inline-table" => Display::InlineTable,
        "table-row-group" => Display::TableRowGroup,
        "table-header-group" => Display::TableHeaderGroup,
        "table-footer-group" => Display::TableFooterGroup,
        "table-row" => Display::TableRow,
        "table-column-group" => Display::TableColumnGroup,
        "table-column" => Display::TableColumn,
        "table-cell" => Display::TableCell,
        "table-caption" => Display::TableCaption,
        "list-item" => Display::ListItem,
        "contents" => Display::Contents,
        "none" => Display::None,
        _ => Display::Flex,
    }
}

fn parse_position(value: &str) -> Position {
    match value.trim() {
        "static" => Position::Static,
        "relative" => Position::Relative,
        "absolute" => Position::Absolute,
        "fixed" => Position::Fixed,
        "sticky" => Position::Sticky,
        _ => Position::Static,
    }
}

fn parse_float(value: &str) -> Float {
    match value.trim() {
        "left" => Float::Left,
        "right" => Float::Right,
        _ => Float::None,
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
    if let Some(n) = v.strip_suffix("ch")
        && let Ok(n) = n.trim().parse()
    {
        return Dimension::Em(n);
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
    if let Some(n) = v
        .strip_suffix("dvh")
        .or_else(|| v.strip_suffix("svh"))
        .or_else(|| v.strip_suffix("lvh"))
        .or_else(|| v.strip_suffix("vh"))
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

/// Resolve the common responsive `min(100%, <fixed-length>)` shape into the
/// equivalent layout constraints understood by Taffy: `size: 100%` plus a
/// fixed maximum. CSS permits either argument order.
fn parse_min_percent_and_fixed(value: &str) -> Option<(Dimension, Dimension)> {
    let inner = value.trim().strip_prefix("min(")?.strip_suffix(')')?;
    let mut parts = inner.split(',').map(str::trim);
    let first = parse_dimension(parts.next()?);
    let second = parse_dimension(parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    match (first, second) {
        (Dimension::Percent(percent), fixed) | (fixed, Dimension::Percent(percent))
            if !matches!(fixed, Dimension::Auto | Dimension::Percent(_)) =>
        {
            Some((Dimension::Percent(percent), fixed))
        }
        _ => None,
    }
}

fn parse_box_shadow(value: &str) -> Option<w3cos_std::style::BoxShadow> {
    // CSS allows the spread radius to be omitted: `x y blur color`.
    let parts = split_css_whitespace(value);
    if parts.len() < 3 {
        return None;
    }
    let ox = parse_px(&parts[0])?;
    let oy = parse_px(&parts[1])?;
    let blur = parse_px(&parts[2])?;
    let (spread, color_index) = parts
        .get(3)
        .and_then(|part| parse_px(part))
        .map_or((0.0, 3), |spread| (spread, 4));
    let color = parts
        .get(color_index)
        .and_then(|color| Color::from_css(color))
        .unwrap_or(Color::rgba(0, 0, 0, 80));
    Some(w3cos_std::style::BoxShadow::new(
        ox, oy, blur, spread, color,
    ))
}

fn split_css_whitespace(value: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut start = None;
    let mut depth = 0_u32;
    for (index, ch) in value.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ => {}
        }
        if ch.is_whitespace() && depth == 0 {
            if let Some(from) = start.take() {
                parts.push(value[from..index].to_string());
            }
        } else if start.is_none() {
            start = Some(index);
        }
    }
    if let Some(from) = start {
        parts.push(value[from..].to_string());
    }
    parts
}

fn parse_border_style_visibility(value: &str) -> Option<bool> {
    let styles = split_css_whitespace(value);
    if styles.is_empty() || styles.len() > 4 {
        return None;
    }
    let mut visible = false;
    for style in styles {
        match style.to_ascii_lowercase().as_str() {
            "none" | "hidden" => {}
            "dotted" | "dashed" | "solid" | "double" | "groove" | "ridge" | "inset" | "outset" => {
                visible = true
            }
            _ => return None,
        }
    }
    Some(visible)
}

fn expand_border_radius(values: &[f32]) -> Option<[f32; 4]> {
    match values {
        [all] => Some([*all; 4]),
        [vertical, horizontal] => Some([*vertical, *horizontal, *vertical, *horizontal]),
        [top_left, opposite, bottom_right] => {
            Some([*top_left, *opposite, *bottom_right, *opposite])
        }
        [top_left, top_right, bottom_right, bottom_left] => {
            Some([*top_left, *top_right, *bottom_right, *bottom_left])
        }
        _ => None,
    }
}

fn apply_border_shorthand(style: &mut Style, value: &str) {
    let mut width = None;
    let mut visible = None;
    for part in split_css_whitespace(value) {
        if let Some(parsed) = parse_px(&part) {
            width = Some(parsed);
        } else if let Some(color) = Color::from_css(&part) {
            style.border_color = color;
        } else if let Some(parsed) = parse_border_style_visibility(&part) {
            visible = Some(parsed);
        }
    }
    if let Some(width) = match visible {
        Some(false) => Some(0.0),
        Some(true) => Some(width.unwrap_or(3.0)),
        None => width,
    } {
        style.border_width = width;
        style.border_top_width = Some(width);
        style.border_right_width = Some(width);
        style.border_bottom_width = Some(width);
        style.border_left_width = Some(width);
    }
}

#[derive(Clone, Copy)]
enum BorderSide {
    Top,
    Right,
    Bottom,
    Left,
}

fn apply_border_side_shorthand(style: &mut Style, value: &str, side: BorderSide) {
    let mut width = None;
    let mut color = None;
    let mut visible = None;
    for part in split_css_whitespace(value) {
        if let Some(parsed) = parse_px(&part) {
            width = Some(parsed);
        } else if let Some(parsed) = Color::from_css(&part) {
            color = Some(parsed);
        } else if let Some(parsed) = parse_border_style_visibility(&part) {
            visible = Some(parsed);
        }
    }
    let width = match visible {
        Some(false) => Some(0.0),
        Some(true) => Some(width.unwrap_or(3.0)),
        None => width,
    };
    match side {
        BorderSide::Top => {
            style.border_top_width = width;
            style.border_top_color = color;
        }
        BorderSide::Right => {
            style.border_right_width = width;
            style.border_right_color = color;
        }
        BorderSide::Bottom => {
            style.border_bottom_width = width;
            style.border_bottom_color = color;
        }
        BorderSide::Left => {
            style.border_left_width = width;
            style.border_left_color = color;
        }
    }
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

fn apply_font_shorthand(style: &mut Style, value: &str) {
    let (before_line_height, after_slash) = value
        .split_once('/')
        .map_or((value, None), |(before, after)| (before, Some(after)));
    let before_parts = split_css_whitespace(before_line_height);
    let Some((size_index, size)) = before_parts
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, value)| parse_font_size(value, style.font_size).map(|size| (index, size)))
    else {
        return;
    };

    style.font_size = size;
    for part in &before_parts[..size_index] {
        match part.as_str() {
            "italic" | "oblique" | "normal" => style.font_style = parse_font_style(part),
            "bold" => style.font_weight = 700,
            _ => {
                if let Ok(weight) = part.parse::<u16>() {
                    style.font_weight = weight;
                }
            }
        }
    }

    let family = if let Some(after_slash) = after_slash {
        let after_slash = after_slash.trim_start();
        let line_height_end = after_slash
            .find(char::is_whitespace)
            .unwrap_or(after_slash.len());
        let line_height = &after_slash[..line_height_end];
        if let Some(line_height) = parse_font_line_height(line_height, style.font_size) {
            style.line_height = line_height;
        }
        after_slash[line_height_end..].trim()
    } else {
        value_after_nth_whitespace_token(value, size_index + 1)
    };

    if !family.is_empty() {
        style.font_family = Some(family.trim_matches('"').trim_matches('\'').to_string());
    }
}

fn parse_font_size(value: &str, inherited_size: f32) -> Option<f32> {
    let value = value.trim();
    if let Some(number) = value.strip_suffix("rem") {
        return number.trim().parse::<f32>().ok().map(|number| number * 16.0);
    }
    if let Some(number) = value.strip_suffix("em") {
        return number
            .trim()
            .parse::<f32>()
            .ok()
            .map(|number| number * inherited_size);
    }
    if let Some(number) = value.strip_suffix('%') {
        return number
            .trim()
            .parse::<f32>()
            .ok()
            .map(|number| number * inherited_size / 100.0);
    }
    parse_px(value)
}

fn parse_font_line_height(value: &str, font_size: f32) -> Option<f32> {
    let value = value.trim();
    if value == "normal" {
        return Some(1.2);
    }
    if let Some(number) = value.strip_suffix('%') {
        return number.trim().parse::<f32>().ok().map(|number| number / 100.0);
    }
    if let Ok(number) = value.parse::<f32>() {
        return Some(number.max(0.0));
    }
    if let Some(px) = parse_px(value) {
        return Some((px / font_size.max(1.0)).max(0.0));
    }
    None
}

fn value_after_nth_whitespace_token(value: &str, token_count: usize) -> &str {
    let mut seen = 0;
    let mut in_token = false;
    for (index, ch) in value.char_indices() {
        if ch.is_whitespace() {
            if in_token {
                seen += 1;
                in_token = false;
            }
        } else if !in_token {
            if seen == token_count {
                return &value[index..];
            }
            in_token = true;
        }
    }
    ""
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

fn parse_overflow_wrap(value: &str) -> w3cos_std::style::WordBreak {
    use w3cos_std::style::WordBreak;
    match value.trim() {
        "anywhere" | "break-word" => WordBreak::BreakWord,
        _ => WordBreak::Normal,
    }
}

#[cfg(test)]
mod overflow_wrap_tests {
    use super::*;

    #[test]
    fn overflow_wrap_anywhere_updates_line_breaking_style() {
        let mut declaration = CSSStyleDeclaration::new();
        declaration.set_property("overflow-wrap", "anywhere");
        assert_eq!(
            declaration.to_style().word_break,
            w3cos_std::style::WordBreak::BreakWord
        );
    }
}

#[cfg(test)]
mod font_shorthand_tests {
    use super::*;

    #[test]
    fn font_shorthand_preserves_unitless_line_height() {
        let mut declaration = CSSStyleDeclaration::new();
        declaration.set_property("font", "1em/1.25 serif");

        let style = declaration.to_style();
        assert_eq!(style.line_height, 1.25);
        assert_eq!(style.font_family.as_deref(), Some("serif"));
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
    let parts = split_css_whitespace(value);
    if parts.is_empty() || value.trim().eq_ignore_ascii_case("none") {
        return None;
    }

    let property = match parts[0].as_str() {
        "all" => TransitionProperty::All,
        "opacity" => TransitionProperty::Opacity,
        "transform" => TransitionProperty::Transform,
        "background" | "background-color" => TransitionProperty::Background,
        "color" => TransitionProperty::Color,
        p => TransitionProperty::Custom(p.to_string()),
    };
    let mut times = parts.iter().skip(1).filter_map(|part| parse_time_ms(part));
    let duration_ms = times.next().unwrap_or(0);
    let delay_ms = times.next().unwrap_or(0);
    let easing = parts
        .iter()
        .skip(1)
        .find_map(|part| parse_easing(part))
        .unwrap_or(Easing::Ease);

    Some(Transition {
        property,
        duration_ms,
        easing,
        delay_ms,
    })
}

fn parse_animation(value: &str) -> Option<w3cos_std::style::Animation> {
    use w3cos_std::style::{
        Animation, AnimationDirection, AnimationFillMode, AnimationIterationCount, Easing,
    };
    let parts = split_css_whitespace(value);
    if parts.is_empty() || value.trim().eq_ignore_ascii_case("none") {
        return None;
    }
    let mut times = parts.iter().filter_map(|part| parse_time_ms(part));
    let duration_ms = times.next().unwrap_or(0);
    let delay_ms = times.next().unwrap_or(0);
    let easing = parts
        .iter()
        .find_map(|part| parse_easing(part))
        .unwrap_or(Easing::Ease);
    let iteration_count = if parts.iter().any(|part| part == "infinite") {
        AnimationIterationCount::Infinite
    } else {
        parts
            .iter()
            .find_map(|part| part.parse::<u32>().ok())
            .map(AnimationIterationCount::Count)
            .unwrap_or(AnimationIterationCount::Once)
    };
    let direction = if parts.iter().any(|part| part == "alternate-reverse") {
        AnimationDirection::AlternateReverse
    } else if parts.iter().any(|part| part == "alternate") {
        AnimationDirection::Alternate
    } else if parts.iter().any(|part| part == "reverse") {
        AnimationDirection::Reverse
    } else {
        AnimationDirection::Normal
    };
    let fill_mode = if parts.iter().any(|part| part == "forwards") {
        AnimationFillMode::Forwards
    } else if parts.iter().any(|part| part == "backwards") {
        AnimationFillMode::Backwards
    } else if parts.iter().any(|part| part == "both") {
        AnimationFillMode::Both
    } else {
        AnimationFillMode::None
    };
    let name = parts
        .iter()
        .rev()
        .find(|part| {
            parse_time_ms(part).is_none()
                && parse_easing(part).is_none()
                && !matches!(
                    part.as_str(),
                    "infinite"
                        | "normal"
                        | "reverse"
                        | "alternate"
                        | "alternate-reverse"
                        | "none"
                        | "forwards"
                        | "backwards"
                        | "both"
                        | "running"
                        | "paused"
                )
                && part.parse::<f32>().is_err()
        })?
        .to_string();
    Some(Animation {
        name,
        duration_ms,
        easing,
        delay_ms,
        iteration_count,
        direction,
        fill_mode,
    })
}

fn parse_time_ms(value: &str) -> Option<u32> {
    let value = value.trim();
    if let Some(ms) = value.strip_suffix("ms") {
        ms.parse::<f32>().ok().map(|value| value.max(0.0) as u32)
    } else if let Some(seconds) = value.strip_suffix('s') {
        seconds
            .parse::<f32>()
            .ok()
            .map(|value| (value.max(0.0) * 1000.0) as u32)
    } else {
        None
    }
}

fn parse_easing(value: &str) -> Option<w3cos_std::style::Easing> {
    use w3cos_std::style::{Easing, StepPosition};
    match value.trim() {
        "linear" => Some(Easing::Linear),
        "ease" => Some(Easing::Ease),
        "ease-in" => Some(Easing::EaseIn),
        "ease-out" => Some(Easing::EaseOut),
        "ease-in-out" => Some(Easing::EaseInOut),
        value if value.starts_with("cubic-bezier(") && value.ends_with(')') => {
            let values = value[13..value.len() - 1]
                .split(',')
                .map(str::trim)
                .map(str::parse::<f32>)
                .collect::<Result<Vec<_>, _>>()
                .ok()?;
            let [x1, y1, x2, y2] = values.as_slice() else {
                return None;
            };
            // The x control points define time and must remain in [0, 1].
            // CSS permits y values outside that interval for overshoot curves.
            if !(0.0..=1.0).contains(x1) || !(0.0..=1.0).contains(x2) {
                return None;
            }
            Some(Easing::CubicBezier(*x1, *y1, *x2, *y2))
        }
        value if value.starts_with("steps(") && value.ends_with(')') => {
            let inner = &value[6..value.len() - 1];
            let mut parts = inner.split(',').map(str::trim);
            let count = parts.next()?.parse::<u32>().ok()?.max(1);
            let position = match parts.next().unwrap_or("jump-end") {
                "start" | "jump-start" => StepPosition::JumpStart,
                "jump-none" => StepPosition::JumpNone,
                "jump-both" => StepPosition::JumpBoth,
                _ => StepPosition::JumpEnd,
            };
            Some(Easing::Steps(count, position))
        }
        _ => None,
    }
}

fn easing_to_css(easing: w3cos_std::style::Easing) -> String {
    use w3cos_std::style::{Easing, StepPosition};
    match easing {
        Easing::Ease => "ease".to_string(),
        Easing::Linear => "linear".to_string(),
        Easing::EaseIn => "ease-in".to_string(),
        Easing::EaseOut => "ease-out".to_string(),
        Easing::EaseInOut => "ease-in-out".to_string(),
        Easing::CubicBezier(x1, y1, x2, y2) => {
            format!("cubic-bezier({x1}, {y1}, {x2}, {y2})")
        }
        Easing::Steps(count, position) => {
            let position = match position {
                StepPosition::JumpStart => "jump-start",
                StepPosition::JumpEnd => "jump-end",
                StepPosition::JumpNone => "jump-none",
                StepPosition::JumpBoth => "jump-both",
            };
            format!("steps({count}, {position})")
        }
    }
}

fn transition_to_css(transition: &w3cos_std::style::Transition) -> String {
    use w3cos_std::style::TransitionProperty;
    let property = match &transition.property {
        TransitionProperty::All => "all",
        TransitionProperty::Opacity => "opacity",
        TransitionProperty::Transform => "transform",
        TransitionProperty::Background => "background",
        TransitionProperty::Color => "color",
        TransitionProperty::Custom(property) => property,
    };
    format!(
        "{property} {}ms {} {}ms",
        transition.duration_ms,
        easing_to_css(transition.easing),
        transition.delay_ms
    )
}

fn animation_to_css(animation: &w3cos_std::style::Animation) -> String {
    format!(
        "{} {}ms {} {}ms",
        animation.name,
        animation.duration_ms,
        easing_to_css(animation.easing),
        animation.delay_ms
    )
}

fn transform_to_css(transform: w3cos_std::style::Transform2D) -> String {
    if transform.is_identity() {
        "none".to_string()
    } else {
        format!(
            "matrix({}, {}, {}, {}, {}, {})",
            transform.scale_x,
            0.0,
            0.0,
            transform.scale_y,
            transform.translate_x,
            transform.translate_y
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_and_css_float_share_the_typed_property() {
        let mut declaration = CSSStyleDeclaration::new();
        declaration.set_property("float", "right");
        assert_eq!(declaration.inner.float, Float::Right);
        assert_eq!(declaration.get_property("cssFloat"), "right");

        declaration.set_property("cssFloat", "left");
        assert_eq!(declaration.inner.float, Float::Left);
        assert_eq!(declaration.get_property("float"), "left");

        declaration.set_property("float", "none");
        assert_eq!(declaration.inner.float, Float::None);
    }

    #[test]
    fn vertical_align_maps_to_portable_cross_axis_alignment() {
        let mut declaration = CSSStyleDeclaration::new();

        for (value, expected) in [
            ("top", w3cos_std::style::AlignSelf::FlexStart),
            ("middle", w3cos_std::style::AlignSelf::Center),
            ("bottom", w3cos_std::style::AlignSelf::FlexEnd),
            ("baseline", w3cos_std::style::AlignSelf::Baseline),
        ] {
            declaration.set_property("vertical-align", value);
            assert_eq!(declaration.inner.align_self, expected);
            assert_eq!(declaration.get_property("vertical-align"), value);
        }
    }

    #[test]
    fn css_motion_shorthands_preserve_timing_and_longhand_delay() {
        let mut declaration = CSSStyleDeclaration::new();
        declaration.set_property("transition", "left 60s steps(1, jump-both)");
        let transition = declaration.inner.transition.as_ref().expect("transition");
        assert_eq!(transition.duration_ms, 60_000);
        assert_eq!(
            transition.easing,
            w3cos_std::style::Easing::Steps(1, w3cos_std::style::StepPosition::JumpBoth)
        );

        declaration.set_property("animation", "1s linear infinite alternate slide");
        declaration.set_property("animation-delay", "100ms");
        let animation = declaration.inner.animation.as_ref().expect("animation");
        assert_eq!(animation.name, "slide");
        assert_eq!(animation.duration_ms, 1_000);
        assert_eq!(animation.delay_ms, 100);
        assert_eq!(
            animation.iteration_count,
            w3cos_std::style::AnimationIterationCount::Infinite
        );
        assert_eq!(
            animation.direction,
            w3cos_std::style::AnimationDirection::Alternate
        );

        declaration.set_property("transition", "all 1s cubic-bezier(0, -0.5, 1, -0.5)");
        assert_eq!(
            declaration
                .inner
                .transition
                .expect("bezier transition")
                .easing,
            w3cos_std::style::Easing::CubicBezier(0.0, -0.5, 1.0, -0.5)
        );
    }

    #[test]
    fn responsive_min_width_becomes_percent_size_with_fixed_cap() {
        for value in ["min(420px, 100%)", "min(100%, 420px)"] {
            let mut declaration = CSSStyleDeclaration::new();
            declaration.set_property("width", value);
            assert_eq!(declaration.inner.width, Dimension::Percent(100.0));
            assert_eq!(declaration.inner.max_width, Dimension::Px(420.0));
        }
    }

    #[test]
    fn border_shorthand_accepts_rgba_color() {
        let mut declaration = CSSStyleDeclaration::new();
        declaration.set_property("border", "1px solid rgba(215, 224, 238, 0.92)");
        assert_eq!(declaration.inner.border_width, 1.0);
        assert_eq!(
            declaration.inner.border_color,
            Color::rgba(215, 224, 238, 235)
        );
    }

    #[test]
    fn border_none_resets_an_earlier_shorthand_width_on_every_side() {
        let mut declaration = CSSStyleDeclaration::new();
        declaration.set_property("border", "1px solid cyan");
        declaration.set_property("border", "none");

        assert_eq!(declaration.inner.border_width, 0.0);
        assert_eq!(declaration.inner.border_top_width, Some(0.0));
        assert_eq!(declaration.inner.border_right_width, Some(0.0));
        assert_eq!(declaration.inner.border_bottom_width, Some(0.0));
        assert_eq!(declaration.inner.border_left_width, Some(0.0));
    }

    #[test]
    fn border_side_none_resets_only_that_side_width() {
        let mut declaration = CSSStyleDeclaration::new();
        declaration.set_property("border", "2px solid cyan");
        declaration.set_property("border-left", "none");

        assert_eq!(declaration.inner.border_top_width, Some(2.0));
        assert_eq!(declaration.inner.border_right_width, Some(2.0));
        assert_eq!(declaration.inner.border_bottom_width, Some(2.0));
        assert_eq!(declaration.inner.border_left_width, Some(0.0));
    }

    #[test]
    fn visible_border_style_uses_initial_medium_width() {
        let mut declaration = CSSStyleDeclaration::new();
        declaration.set_property("border-color", "orange");
        declaration.set_property("border-style", "solid");
        assert_eq!(declaration.inner.border_width, 3.0);
        assert_eq!(declaration.inner.border_color, Color::rgb(255, 165, 0));

        declaration.set_property("border-style", "none");
        assert_eq!(declaration.inner.border_width, 0.0);
    }

    #[test]
    fn multi_value_border_radius_preserves_css_corner_order() {
        let mut declaration = CSSStyleDeclaration::new();
        declaration.set_property("border-radius", "4px 16px 16px");
        assert_eq!(
            declaration.inner.border_corner_radii(),
            [4.0, 16.0, 16.0, 16.0]
        );
    }

    #[test]
    fn box_shadow_accepts_omitted_spread_and_rgba_color() {
        let mut declaration = CSSStyleDeclaration::new();
        declaration.set_property("box-shadow", "0 18px 56px rgba(28, 55, 90, 0.12)");
        let shadow = declaration.inner.box_shadow.expect("parsed shadow");
        assert_eq!(shadow.spread_radius, 0.0);
        assert_eq!(shadow.color, Color::rgba(28, 55, 90, 31));
    }

    #[test]
    fn background_image_preserves_url_layers_and_none_clears_them() {
        let mut declaration = CSSStyleDeclaration::new();
        declaration.set_property(
            "background-image",
            "linear-gradient(red, blue), url('images/card.png')",
        );
        assert_eq!(
            declaration.get_property("background-image"),
            "linear-gradient(red, blue), url('images/card.png')"
        );
        declaration.set_property("backgroundImage", "none");
        assert_eq!(declaration.get_property("background-image"), "none");
    }

    #[test]
    fn background_shorthand_expands_raster_placement_fields() {
        let mut declaration = CSSStyleDeclaration::new();
        declaration.set_property(
            "background",
            "#123456 url('map/tile.png') center / cover no-repeat content-box padding-box",
        );
        assert_eq!(
            declaration.inner.background_image.as_deref(),
            Some("url('map/tile.png')")
        );
        assert_eq!(declaration.inner.background_size.as_deref(), Some("cover"));
        assert_eq!(
            declaration.inner.background_position.as_deref(),
            Some("center")
        );
        assert_eq!(
            declaration.inner.background_repeat.as_deref(),
            Some("no-repeat")
        );
        assert_eq!(
            declaration.inner.background_origin.as_deref(),
            Some("content-box")
        );
        assert_eq!(
            declaration.inner.background_clip.as_deref(),
            Some("padding-box")
        );
        assert_eq!(declaration.inner.background, Color::from_hex("#123456"));
    }

    #[test]
    fn background_retains_gradient_layers() {
        let value = "radial-gradient(circle at 85% 8%, rgba(22, 119, 255, 0.18), transparent 34%), linear-gradient(160deg, #f7faff 0%, #eef3fb 100%)";
        let mut declaration = CSSStyleDeclaration::new();
        declaration.set_property("background", value);
        assert_eq!(declaration.inner.background_image.as_deref(), Some(value));
    }

    #[test]
    fn background_attachment_and_blend_longhands_round_trip() {
        let mut declaration = CSSStyleDeclaration::new();
        declaration.set_property("background", "url(a.png) fixed, url(b.png) local");
        declaration.set_property("backgroundBlendMode", "multiply, screen");
        assert_eq!(
            declaration.get_property("background-attachment"),
            "fixed, local"
        );
        assert_eq!(
            declaration.get_property("background-blend-mode"),
            "multiply, screen"
        );
    }

    #[test]
    fn box_edge_shorthands_expand_like_css() {
        let mut declaration = CSSStyleDeclaration::new();
        declaration.set_property("padding", "12px 14px");
        declaration.set_property("margin", "1px 2px 3px 4px");
        assert_eq!(
            declaration.inner.padding,
            Edges {
                top: Spacing::Px(12.0),
                right: Spacing::Px(14.0),
                bottom: Spacing::Px(12.0),
                left: Spacing::Px(14.0),
            }
        );
        assert_eq!(
            declaration.inner.margin,
            Edges {
                top: Spacing::Px(1.0),
                right: Spacing::Px(2.0),
                bottom: Spacing::Px(3.0),
                left: Spacing::Px(4.0),
            }
        );
    }

    #[test]
    fn negative_padding_declarations_are_ignored() {
        let mut declaration = CSSStyleDeclaration::new();
        declaration.set_property("padding", "4px");
        declaration.set_property("padding-top", "-1%");
        declaration.set_property("padding-bottom", "-2px");
        declaration.set_property("padding", "1px -3px");
        assert_eq!(declaration.inner.padding, Edges::all(4.0));
    }

    #[test]
    fn negative_margin_and_character_relative_lengths_remain_valid() {
        let mut declaration = CSSStyleDeclaration::new();
        declaration.set_property("font-size", "10px");
        declaration.set_property("width", "4ch");
        declaration.set_property("margin", "0 -1ch");
        assert_eq!(declaration.inner.width, Dimension::Em(4.0));
        assert_eq!(
            declaration.inner.margin,
            Edges {
                top: Spacing::Px(0.0),
                right: Spacing::Em(-1.0),
                bottom: Spacing::Px(0.0),
                left: Spacing::Em(-1.0),
            }
        );
    }

    #[test]
    fn box_edge_shorthand_preserves_safe_area_environment_values() {
        let mut declaration = CSSStyleDeclaration::new();
        declaration.set_property(
            "padding",
            "calc(36px + env(safe-area-inset-top)) 22px calc(28px + env(safe-area-inset-bottom))",
        );
        assert_eq!(
            declaration.inner.padding,
            Edges {
                top: Spacing::Composite {
                    px: 36.0,
                    safe_area: Some(SafeAreaEdge::Top),
                    keyboard_inset: false,
                },
                right: Spacing::Px(22.0),
                bottom: Spacing::Composite {
                    px: 28.0,
                    safe_area: Some(SafeAreaEdge::Bottom),
                    keyboard_inset: false,
                },
                left: Spacing::Px(22.0),
            }
        );
    }

    #[test]
    fn box_edge_shorthand_preserves_safe_area_maximum_values() {
        let mut declaration = CSSStyleDeclaration::new();
        declaration.set_property(
            "padding",
            "max(8px, env(safe-area-inset-top)) max(16px, env(safe-area-inset-right)) 8px max(16px, env(safe-area-inset-left))",
        );
        assert_eq!(
            declaration.inner.padding,
            Edges {
                top: Spacing::Maximum {
                    px: 8.0,
                    safe_area: SafeAreaEdge::Top,
                },
                right: Spacing::Maximum {
                    px: 16.0,
                    safe_area: SafeAreaEdge::Right,
                },
                bottom: Spacing::Px(8.0),
                left: Spacing::Maximum {
                    px: 16.0,
                    safe_area: SafeAreaEdge::Left,
                },
            }
        );
    }

    #[test]
    fn modern_box_model_values_match_css_semantics() {
        let mut declaration = CSSStyleDeclaration::new();
        declaration.set_property("box-sizing", "border-box");
        declaration.set_property("margin", "1rem auto 12px 5%");
        declaration.set_property("gap", "8px 16px");
        declaration.set_property("flex", "1 0%");
        declaration.set_property("overflow-x", "hidden");
        declaration.set_property("overflow-y", "auto");
        declaration.set_property("border-left", "4px solid #e74c3c");

        assert_eq!(declaration.inner.box_sizing, BoxSizing::BorderBox);
        assert_eq!(declaration.inner.margin.top, Spacing::Rem(1.0));
        assert_eq!(declaration.inner.margin.right, Spacing::Auto);
        assert_eq!(declaration.inner.margin.bottom, Spacing::Px(12.0));
        assert_eq!(declaration.inner.margin.left, Spacing::Percent(5.0));
        assert_eq!(declaration.inner.row_gap, Some(8.0));
        assert_eq!(declaration.inner.column_gap, Some(16.0));
        assert_eq!(declaration.inner.flex_grow, 1.0);
        assert_eq!(declaration.inner.flex_shrink, 1.0);
        assert_eq!(declaration.inner.flex_basis, Dimension::Percent(0.0));
        assert_eq!(declaration.inner.resolved_overflow_x(), Overflow::Hidden);
        assert_eq!(declaration.inner.resolved_overflow_y(), Overflow::Auto);
        assert_eq!(declaration.inner.border_left_width, Some(4.0));
        assert_eq!(
            declaration.inner.border_left_color,
            Some(Color::rgb(231, 76, 60))
        );
    }

    #[test]
    fn flex_shorthand_sets_grow_shrink_and_basis() {
        let mut declaration = CSSStyleDeclaration::new();
        declaration.set_property("flex", "1");
        assert_eq!(declaration.inner.flex_grow, 1.0);
        assert_eq!(declaration.inner.flex_shrink, 1.0);
        assert_eq!(declaration.inner.flex_basis, Dimension::Percent(0.0));

        declaration.set_property("flex", "0 0 auto");
        assert_eq!(declaration.inner.flex_grow, 0.0);
        assert_eq!(declaration.inner.flex_shrink, 0.0);
        assert_eq!(declaration.inner.flex_basis, Dimension::Auto);
    }

    #[test]
    fn logical_box_properties_map_to_ltr_physical_edges() {
        let mut declaration = CSSStyleDeclaration::new();
        declaration.set_property("padding-inline", "10px 12px");
        declaration.set_property("margin-block", "4px 6px");
        declaration.set_property("border-inline-width", "0");
        declaration.set_property("border-block-start", "1px solid #dbe4ef");

        assert_eq!(declaration.inner.padding.left, Spacing::Px(10.0));
        assert_eq!(declaration.inner.padding.right, Spacing::Px(12.0));
        assert_eq!(declaration.inner.margin.top, Spacing::Px(4.0));
        assert_eq!(declaration.inner.margin.bottom, Spacing::Px(6.0));
        assert_eq!(declaration.inner.border_left_width, Some(0.0));
        assert_eq!(declaration.inner.border_right_width, Some(0.0));
        assert_eq!(declaration.inner.border_top_width, Some(1.0));
    }

    #[test]
    fn dynamic_viewport_height_uses_viewport_units() {
        let mut declaration = CSSStyleDeclaration::new();
        declaration.set_property("height", "100dvh");
        assert_eq!(declaration.inner.height, Dimension::Vh(100.0));
    }
}
#[test]
fn inline_flex_round_trips_without_falling_back_to_block_flex() {
    let mut declaration = CSSStyleDeclaration::new();
    declaration.set_property("display", "inline-flex");
    assert_eq!(declaration.inner.display, Display::InlineFlex);
    assert_eq!(declaration.get_property("display"), "inline-flex");
}

#[test]
fn flow_root_uses_block_layout_instead_of_flex_fallback() {
    let mut declaration = CSSStyleDeclaration::new();
    declaration.set_property("display", "flow-root");
    assert_eq!(declaration.inner.display, Display::Block);
}

#[test]
fn table_display_values_round_trip_without_falling_back_to_flex() {
    for (value, expected) in [
        ("table", Display::Table),
        ("inline-table", Display::InlineTable),
        ("table-row-group", Display::TableRowGroup),
        ("table-row", Display::TableRow),
        ("table-cell", Display::TableCell),
        ("table-caption", Display::TableCaption),
    ] {
        let mut declaration = CSSStyleDeclaration::new();
        declaration.set_property("display", value);
        assert_eq!(declaration.inner.display, expected);
        assert_eq!(declaration.get_property("display"), value);
    }
}
