use crate::style::Style;
use crate::{Color, SvgPathCommand};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// A component's computed style, shared until a caller applies a node-local
/// used-value adjustment. DerefMut uses safe copy-on-write; immutable style
/// readers retain the existing field-access API.
#[derive(Clone, Default)]
pub struct ComponentStyle(Arc<Style>);

impl ComponentStyle {
    pub fn shared(style: Arc<Style>) -> Self {
        Self(style)
    }

    pub fn as_style(&self) -> &Style {
        &self.0
    }

    pub fn make_mut(&mut self) -> &mut Style {
        Arc::make_mut(&mut self.0)
    }

    pub fn into_owned(self) -> Style {
        Arc::try_unwrap(self.0).unwrap_or_else(|style| (*style).clone())
    }
}

impl std::fmt::Debug for ComponentStyle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl From<Style> for ComponentStyle {
    fn from(style: Style) -> Self {
        Self(Arc::new(style))
    }
}

impl From<Arc<Style>> for ComponentStyle {
    fn from(style: Arc<Style>) -> Self {
        Self::shared(style)
    }
}

impl std::ops::Deref for ComponentStyle {
    type Target = Style;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for ComponentStyle {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.make_mut()
    }
}

impl PartialEq for ComponentStyle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || *self.0 == *other.0
    }
}

impl PartialEq<Style> for ComponentStyle {
    fn eq(&self, other: &Style) -> bool {
        **self == *other
    }
}

impl PartialEq<ComponentStyle> for Style {
    fn eq(&self, other: &ComponentStyle) -> bool {
        *self == **other
    }
}

impl Serialize for ComponentStyle {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ComponentStyle {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Style::deserialize(deserializer).map(Self::from)
    }
}

/// An action triggered by a UI event (click, input, etc.).
/// Actions modify signals in the reactive state store.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum EventAction {
    #[default]
    None,
    Increment(usize),
    Decrement(usize),
    Set(usize, i64),
    Toggle(usize),
    Notify(String, String),
    /// `history:push:route:2:/path` — pushState + set route signal
    HistoryPush {
        route_signal: usize,
        route_value: i64,
        path: String,
    },
    /// `history:back:route` — history.back() + restore route from state
    HistoryBack {
        route_signal: usize,
    },
    /// `fetch:GET:statusSig:bytesSig:https://...` — blocking HTTP GET, store status + body len
    FetchGet {
        url: String,
        status_signal: usize,
        bytes_signal: usize,
    },
    /// Start a Web Speech API-compatible recognition session.
    SpeechRecognitionStart {
        transcript_signal: usize,
        final_signal: usize,
        confidence_signal: usize,
        status_signal: usize,
        lang: String,
        process_locally: bool,
        continuous: bool,
        interim_results: bool,
    },
    /// Stop the active speech recognition session and finalize buffered audio.
    SpeechRecognitionStop {
        after_signal: Option<usize>,
        after_value: i64,
    },
    /// Event capabilities registered for one intrinsic host node.
    NativeHost {
        id: u64,
        click: bool,
        scroll: bool,
        input: bool,
        focus: bool,
        keyboard: bool,
        submit: bool,
        pointer: bool,
        wheel: bool,
    },
    /// Shell hook. The runtime does not interpret `code`. When
    /// `input_placeholder` is non-empty, the click path reads that text
    /// input and passes the text to the hook. Layout and key-repeat do not.
    Invoke {
        code: u8,
        input_placeholder: String,
    },
}

impl EventAction {
    pub fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }

    pub fn has_pointer_interaction(&self) -> bool {
        matches!(self, Self::NativeHost { click: true, .. })
            || !matches!(self, Self::NativeHost { .. } | Self::None)
    }
}

/// A UI component in the W3C OS component tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Component {
    pub kind: ComponentKind,
    pub style: ComponentStyle,
    pub children: Vec<Component>,
    #[serde(default)]
    pub on_click: EventAction,
    /// Signal whose value is increased when this node crosses its scrollport's
    /// sticky threshold. Used by declarative sticky-group summaries.
    #[serde(default)]
    pub sticky_counter_signal: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SvgEventTarget {
    pub svg_id: String,
    /// Paint-order ordinal used when the element has no author-provided ID.
    #[serde(default)]
    pub render_index: Option<u32>,
    #[serde(default = "default_svg_pointer_events")]
    pub pointer_events: String,
    /// DOM target followed by its SVG ancestors, including the `<svg>` root.
    pub host_chain: Vec<u64>,
}

fn default_svg_pointer_events() -> String {
    "auto".to_string()
}

/// Preorder index of the first text input with `placeholder`, plus its
/// declared value. The index matches `layout::pre_flatten`.
pub fn find_text_input<'a>(root: &'a Component, placeholder: &str) -> Option<(usize, &'a str)> {
    fn walk<'a>(
        node: &'a Component,
        placeholder: &str,
        index: &mut usize,
    ) -> Option<(usize, &'a str)> {
        let current = *index;
        *index += 1;
        if let ComponentKind::TextInput {
            value,
            placeholder: field,
            ..
        } = &node.kind
            && field == placeholder
        {
            return Some((current, value.as_str()));
        }
        for child in &node.children {
            if let Some(found) = walk(child, placeholder, index) {
                return Some(found);
            }
        }
        None
    }
    let mut index = 0;
    walk(root, placeholder, &mut index)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ComponentKind {
    Root,
    Column,
    Row,
    Text {
        content: String,
    },
    Button {
        label: String,
    },
    Box,
    Image {
        src: String,
    },
    TextInput {
        value: String,
        placeholder: String,
        #[serde(default)]
        secure: bool,
    },
    Canvas {
        width: u32,
        height: u32,
    },
    SvgPath {
        commands: Vec<SvgPathCommand>,
        fill: Color,
        stroke: Option<Color>,
        stroke_width: f32,
    },
    /// Retained SVG subtree. The runtime parses `source` into a normalized
    /// usvg tree and caches the raster independently from compositor-only
    /// transform/opacity updates.
    SvgDocument {
        source: String,
        width: u32,
        height: u32,
        #[serde(default)]
        event_targets: Vec<SvgEventTarget>,
    },
    /// Runtime-windowed list. `children` contains the item template before the
    /// runtime materializes the current keyed window.
    VirtualList {
        item_count: usize,
        estimated_item_height: f32,
        overscan: f32,
        total_extent: f32,
    },
}

impl Component {
    pub fn root(children: Vec<Component>) -> Self {
        Self {
            kind: ComponentKind::Root,
            style: Style::default().into(),
            children,
            on_click: EventAction::None,
            sticky_counter_signal: None,
        }
    }

    pub fn column(style: Style, children: Vec<Component>) -> Self {
        Self::column_shared(style.into(), children)
    }

    #[doc(hidden)]
    pub fn column_shared(style: ComponentStyle, children: Vec<Component>) -> Self {
        Self {
            kind: ComponentKind::Column,
            style,
            children,
            on_click: EventAction::None,
            sticky_counter_signal: None,
        }
    }

    pub fn row(style: Style, children: Vec<Component>) -> Self {
        Self::row_shared(style.into(), children)
    }

    #[doc(hidden)]
    pub fn row_shared(mut style: ComponentStyle, children: Vec<Component>) -> Self {
        let flex_direction = match style.flex_direction {
            crate::style::FlexDirection::RowReverse => crate::style::FlexDirection::RowReverse,
            _ => crate::style::FlexDirection::Row,
        };
        if style.flex_direction != flex_direction {
            style.flex_direction = flex_direction;
        }
        Self {
            kind: ComponentKind::Row,
            style,
            children,
            on_click: EventAction::None,
            sticky_counter_signal: None,
        }
    }

    pub fn text(content: impl Into<String>, style: Style) -> Self {
        Self::text_shared(content, style.into())
    }

    #[doc(hidden)]
    pub fn text_shared(content: impl Into<String>, style: ComponentStyle) -> Self {
        Self {
            kind: ComponentKind::Text {
                content: content.into(),
            },
            style,
            children: vec![],
            on_click: EventAction::None,
            sticky_counter_signal: None,
        }
    }

    pub fn button(label: impl Into<String>, style: Style) -> Self {
        Self::button_shared(label, style.into())
    }

    #[doc(hidden)]
    pub fn button_shared(label: impl Into<String>, style: ComponentStyle) -> Self {
        Self {
            kind: ComponentKind::Button {
                label: label.into(),
            },
            style,
            children: vec![],
            on_click: EventAction::None,
            sticky_counter_signal: None,
        }
    }

    pub fn button_with_click(
        label: impl Into<String>,
        style: Style,
        on_click: EventAction,
    ) -> Self {
        Self::button_with_click_shared(label, style.into(), on_click)
    }

    #[doc(hidden)]
    pub fn button_with_click_shared(
        label: impl Into<String>,
        style: ComponentStyle,
        on_click: EventAction,
    ) -> Self {
        Self {
            kind: ComponentKind::Button {
                label: label.into(),
            },
            style,
            children: vec![],
            on_click,
            sticky_counter_signal: None,
        }
    }

    pub fn boxed(style: Style, children: Vec<Component>) -> Self {
        Self::boxed_shared(style.into(), children)
    }

    #[doc(hidden)]
    pub fn boxed_shared(style: ComponentStyle, children: Vec<Component>) -> Self {
        Self {
            kind: ComponentKind::Box,
            style,
            children,
            on_click: EventAction::None,
            sticky_counter_signal: None,
        }
    }

    pub fn image(src: impl Into<String>, style: Style) -> Self {
        Self::image_shared(src, style.into())
    }

    #[doc(hidden)]
    pub fn image_shared(src: impl Into<String>, style: ComponentStyle) -> Self {
        Self {
            kind: ComponentKind::Image { src: src.into() },
            style,
            children: vec![],
            on_click: EventAction::None,
            sticky_counter_signal: None,
        }
    }

    pub fn text_input(
        value: impl Into<String>,
        placeholder: impl Into<String>,
        style: Style,
    ) -> Self {
        Self::text_input_shared(value, placeholder, style.into())
    }

    #[doc(hidden)]
    pub fn text_input_shared(
        value: impl Into<String>,
        placeholder: impl Into<String>,
        style: ComponentStyle,
    ) -> Self {
        Self {
            kind: ComponentKind::TextInput {
                value: value.into(),
                placeholder: placeholder.into(),
                secure: false,
            },
            style,
            children: vec![],
            on_click: EventAction::None,
            sticky_counter_signal: None,
        }
    }

    pub fn secure_text_input(
        value: impl Into<String>,
        placeholder: impl Into<String>,
        style: Style,
    ) -> Self {
        Self::secure_text_input_shared(value, placeholder, style.into())
    }

    #[doc(hidden)]
    pub fn secure_text_input_shared(
        value: impl Into<String>,
        placeholder: impl Into<String>,
        style: ComponentStyle,
    ) -> Self {
        Self {
            kind: ComponentKind::TextInput {
                value: value.into(),
                placeholder: placeholder.into(),
                secure: true,
            },
            style,
            children: vec![],
            on_click: EventAction::None,
            sticky_counter_signal: None,
        }
    }

    pub fn canvas(width: u32, height: u32, style: Style) -> Self {
        Self::canvas_shared(width, height, style.into())
    }

    #[doc(hidden)]
    pub fn canvas_shared(width: u32, height: u32, style: ComponentStyle) -> Self {
        Self {
            kind: ComponentKind::Canvas { width, height },
            style,
            children: vec![],
            on_click: EventAction::None,
            sticky_counter_signal: None,
        }
    }

    pub fn svg_path(
        commands: Vec<SvgPathCommand>,
        fill: Color,
        stroke: Option<Color>,
        stroke_width: f32,
        style: Style,
    ) -> Self {
        Self::svg_path_shared(commands, fill, stroke, stroke_width, style.into())
    }

    #[doc(hidden)]
    pub fn svg_path_shared(
        commands: Vec<SvgPathCommand>,
        fill: Color,
        stroke: Option<Color>,
        stroke_width: f32,
        style: ComponentStyle,
    ) -> Self {
        Self {
            kind: ComponentKind::SvgPath {
                commands,
                fill,
                stroke,
                stroke_width,
            },
            style,
            children: vec![],
            on_click: EventAction::None,
            sticky_counter_signal: None,
        }
    }

    pub fn svg_document(source: impl Into<String>, width: u32, height: u32, style: Style) -> Self {
        Self::svg_document_with_targets(source, width, height, Vec::new(), style)
    }

    pub fn svg_document_with_targets(
        source: impl Into<String>,
        width: u32,
        height: u32,
        event_targets: Vec<SvgEventTarget>,
        style: Style,
    ) -> Self {
        Self::svg_document_with_targets_shared(source, width, height, event_targets, style.into())
    }

    #[doc(hidden)]
    pub fn svg_document_with_targets_shared(
        source: impl Into<String>,
        width: u32,
        height: u32,
        event_targets: Vec<SvgEventTarget>,
        style: ComponentStyle,
    ) -> Self {
        Self {
            kind: ComponentKind::SvgDocument {
                source: source.into(),
                width: width.max(1),
                height: height.max(1),
                event_targets,
            },
            style,
            children: vec![],
            on_click: EventAction::None,
            sticky_counter_signal: None,
        }
    }

    pub fn virtual_list(
        item_count: usize,
        estimated_item_height: f32,
        overscan: f32,
        style: Style,
        item_template: Component,
    ) -> Self {
        Self::virtual_list_shared(
            item_count,
            estimated_item_height,
            overscan,
            style.into(),
            item_template,
        )
    }

    #[doc(hidden)]
    pub fn virtual_list_shared(
        item_count: usize,
        estimated_item_height: f32,
        overscan: f32,
        style: ComponentStyle,
        item_template: Component,
    ) -> Self {
        Self {
            kind: ComponentKind::VirtualList {
                item_count,
                estimated_item_height: estimated_item_height.max(1.0),
                overscan: overscan.max(0.0),
                total_extent: item_count as f32 * estimated_item_height.max(1.0),
            },
            style,
            children: vec![item_template],
            on_click: EventAction::None,
            sticky_counter_signal: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Component, ComponentStyle};
    use crate::style::{FlexDirection, Style};

    #[test]
    fn row_component_preserves_an_explicit_reverse_axis() {
        let row = Component::row(
            Style {
                flex_direction: FlexDirection::RowReverse,
                ..Style::default()
            },
            vec![],
        );

        assert_eq!(row.style.flex_direction, FlexDirection::RowReverse);
    }

    #[test]
    fn component_style_detaches_before_mutating_a_shared_style() {
        let shared = ComponentStyle::from(Style::default());
        let original_direction = shared.flex_direction;
        let mut local = shared.clone();
        local.flex_direction = FlexDirection::RowReverse;

        assert_eq!(shared.flex_direction, original_direction);
        assert_eq!(local.flex_direction, FlexDirection::RowReverse);
    }

    #[test]
    fn public_constructor_keeps_concrete_style_inference() {
        let column = Component::column(Default::default(), vec![]);
        assert_eq!(column.style, Style::default());
    }

    #[test]
    fn shared_row_constructor_accepts_component_style_without_eager_conversion() {
        let shared = ComponentStyle::from(Style {
            flex_direction: FlexDirection::Row,
            ..Style::default()
        });
        let peer = shared.clone();
        let row = Component::row_shared(shared, vec![]);

        assert_eq!(row.style.flex_direction, FlexDirection::Row);
        assert_eq!(peer.flex_direction, FlexDirection::Row);
    }

    #[test]
    fn component_style_explicit_access_and_owned_conversion_preserve_values() {
        let mut shared = ComponentStyle::from(Style::default());
        shared.make_mut().flex_direction = FlexDirection::RowReverse;
        assert_eq!(shared.as_style().flex_direction, FlexDirection::RowReverse);

        let owned = shared.into_owned();
        assert_eq!(owned.flex_direction, FlexDirection::RowReverse);
    }

    #[test]
    fn component_style_debug_matches_the_inner_style() {
        let style = Style {
            flex_direction: FlexDirection::RowReverse,
            ..Style::default()
        };
        assert_eq!(
            format!("{style:?}"),
            format!("{:?}", ComponentStyle::from(style))
        );
    }

    #[test]
    fn component_style_serde_keeps_the_style_wire_shape() {
        let style = Style {
            flex_direction: FlexDirection::RowReverse,
            ..Style::default()
        };
        let old_style_json = serde_json::to_value(&style).unwrap();
        let component = Component::column(style.clone(), vec![]);
        let component_json = serde_json::to_value(&component).unwrap();

        assert_eq!(component_json["style"], old_style_json);
        assert!(component_json["style"].get("0").is_none());

        let decoded: Component = serde_json::from_value(component_json).unwrap();
        assert_eq!(decoded.style, style);
    }
}
