use crate::style::Style;
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
}

impl EventAction {
    pub fn is_none(&self) -> bool {
        matches!(self, Self::None)
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ComponentKind {
    Root,
    Column,
    Row,
    Text { content: String },
    Button { label: String },
    Box,
    Image { src: String },
    TextInput { value: String, placeholder: String },
    Canvas { width: u32, height: u32 },
}

impl Component {
    pub fn root(children: Vec<Component>) -> Self {
        Self {
            kind: ComponentKind::Root,
            style: Style::default(),
            children,
            on_click: EventAction::None,
        }
    }

    pub fn column(style: Style, children: Vec<Component>) -> Self {
        Self {
            kind: ComponentKind::Column,
            style,
            children,
            on_click: EventAction::None,
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
        }
    }

    pub fn boxed(style: Style, children: Vec<Component>) -> Self {
        Self {
            kind: ComponentKind::Box,
            style,
            children,
            on_click: EventAction::None,
        }
    }

    pub fn image(src: impl Into<String>, style: Style) -> Self {
        Self {
            kind: ComponentKind::Image { src: src.into() },
            style,
            children: vec![],
            on_click: EventAction::None,
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
            },
            style,
            children: vec![],
            on_click: EventAction::None,
        }
    }

    pub fn canvas(width: u32, height: u32, style: Style) -> Self {
        Self {
            kind: ComponentKind::Canvas { width, height },
            style,
            children: vec![],
            on_click: EventAction::None,
        }
    }
}
