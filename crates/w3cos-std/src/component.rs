use crate::style::Style;
use crate::{Color, SvgPathCommand};
use serde::{Deserialize, Serialize};

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
    /// Event capabilities registered for one React AOT intrinsic host.
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
    pub style: Style,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
            style: Style::default(),
            children,
            on_click: EventAction::None,
            sticky_counter_signal: None,
        }
    }

    pub fn column(style: Style, children: Vec<Component>) -> Self {
        Self {
            kind: ComponentKind::Column,
            style,
            children,
            on_click: EventAction::None,
            sticky_counter_signal: None,
        }
    }

    pub fn row(style: Style, children: Vec<Component>) -> Self {
        Self {
            kind: ComponentKind::Row,
            style: Style {
                flex_direction: crate::style::FlexDirection::Row,
                ..style
            },
            children,
            on_click: EventAction::None,
            sticky_counter_signal: None,
        }
    }

    pub fn text(content: impl Into<String>, style: Style) -> Self {
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
        Self {
            kind: ComponentKind::Box,
            style,
            children,
            on_click: EventAction::None,
            sticky_counter_signal: None,
        }
    }

    pub fn image(src: impl Into<String>, style: Style) -> Self {
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
