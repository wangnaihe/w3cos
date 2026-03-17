use crate::style::Style;
use serde::{Deserialize, Serialize};

/// A UI component in the W3C OS component tree.
/// All UI is built from this unified node type — no legacy DOM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Component {
    pub kind: ComponentKind,
    pub style: Style,
    pub children: Vec<Component>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ComponentKind {
    /// Root container — the top-level application frame.
    Root,
    /// Vertical stack (flex-direction: column).
    Column,
    /// Horizontal stack (flex-direction: row).
    Row,
    /// Text content.
    Text { content: String },
    /// Clickable button.
    Button { label: String },
    /// Generic container with custom styling.
    Box,
    /// Image (future).
    Image { src: String },
}

impl Component {
    pub fn root(children: Vec<Component>) -> Self {
        Self {
            kind: ComponentKind::Root,
            style: Style::default(),
            children,
        }
    }

    pub fn column(style: Style, children: Vec<Component>) -> Self {
        Self {
            kind: ComponentKind::Column,
            style,
            children,
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
        }
    }

    pub fn text(content: impl Into<String>, style: Style) -> Self {
        Self {
            kind: ComponentKind::Text {
                content: content.into(),
            },
            style,
            children: vec![],
        }
    }

    pub fn button(label: impl Into<String>, style: Style) -> Self {
        Self {
            kind: ComponentKind::Button {
                label: label.into(),
            },
            style,
            children: vec![],
        }
    }

    pub fn boxed(style: Style, children: Vec<Component>) -> Self {
        Self {
            kind: ComponentKind::Box,
            style,
            children,
        }
    }
}
