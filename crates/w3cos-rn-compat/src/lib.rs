#![allow(non_snake_case)]
//! React Native compatibility layer for W3C OS.
//!
//! Maps common React Native components and APIs to their W3C OS equivalents,
//! enabling easy migration of RN apps to native W3C OS binaries.
//!
//! | React Native       | W3C OS               |
//! |---------------------|----------------------|
//! | `View`              | `Column` / `Row`     |
//! | `Text`              | `Text`               |
//! | `TouchableOpacity`  | `Button`             |
//! | `ScrollView`        | `Column` (overflow)  |
//! | `Image`             | `Image`              |
//! | `TextInput`         | `TextInput`          |
//! | `StyleSheet.create` | Inline style objects |
//! | `useState`          | `signal()`           |
//! | `onPress`           | `onClick`            |

use w3cos_std::color::Color;
use w3cos_std::style::*;
use w3cos_std::{Component, EventAction, Style};

pub fn View(style: Style, children: Vec<Component>) -> Component {
    let is_row = matches!(
        style.flex_direction,
        FlexDirection::Row | FlexDirection::RowReverse
    );
    if is_row {
        Component::row(style, children)
    } else {
        Component::column(style, children)
    }
}

pub fn Text(content: impl Into<String>, style: Style) -> Component {
    Component::text(content, style)
}

pub fn TouchableOpacity(
    label: impl Into<String>,
    style: Style,
    on_press: EventAction,
) -> Component {
    Component::button_with_click(label, style, on_press)
}

pub fn Pressable(
    label: impl Into<String>,
    style: Style,
    on_press: EventAction,
) -> Component {
    Component::button_with_click(label, style, on_press)
}

pub fn ScrollView(style: Style, children: Vec<Component>) -> Component {
    let scroll_style = Style {
        overflow: Overflow::Scroll,
        ..style
    };
    Component::column(scroll_style, children)
}

pub fn Image(src: impl Into<String>, style: Style) -> Component {
    Component::image(src, style)
}

pub fn TextInput(
    value: impl Into<String>,
    placeholder: impl Into<String>,
    style: Style,
) -> Component {
    Component::text_input(value, placeholder, style)
}

pub fn SafeAreaView(style: Style, children: Vec<Component>) -> Component {
    Component::column(style, children)
}

pub fn FlatList<T, F>(data: &[T], render_item: F, style: Style) -> Component
where
    F: Fn(&T, usize) -> Component,
{
    let children: Vec<Component> = data.iter().enumerate().map(|(i, item)| render_item(item, i)).collect();
    ScrollView(style, children)
}

/// React Native StyleSheet.create equivalent.
/// In W3C OS, styles are plain `Style` structs — this is a pass-through
/// that mirrors the RN API pattern.
pub mod StyleSheet {
    use std::collections::HashMap;
    use w3cos_std::Style;

    pub fn create(styles: HashMap<&str, Style>) -> HashMap<String, Style> {
        styles
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect()
    }
}

/// Maps to `w3cos_runtime::state::create_signal` / `get_signal`.
pub fn use_state(initial: i64) -> (usize, i64) {
    let id = w3cos_runtime::state::create_signal(initial);
    let value = w3cos_runtime::state::get_signal(id);
    (id, value)
}

/// Convenience for creating a View with Row flex direction.
pub fn HorizontalView(style: Style, children: Vec<Component>) -> Component {
    let row_style = Style {
        flex_direction: FlexDirection::Row,
        ..style
    };
    Component::row(row_style, children)
}

/// StatusBar placeholder — no-op in W3C OS native environment.
pub fn StatusBar(_style: Style) -> Component {
    Component::column(Style::default(), vec![])
}

/// ActivityIndicator — renders as a simple text placeholder.
pub fn ActivityIndicator(style: Style) -> Component {
    Component::text("Loading...", style)
}

/// Button — a simple labeled button, matching RN's Button API.
pub fn Button(title: impl Into<String>, style: Style, on_press: EventAction) -> Component {
    Component::button_with_click(title, style, on_press)
}

/// Switch — rendered as a toggle button with [ON]/[OFF] state.
pub fn Switch(is_on: bool, style: Style, on_toggle: EventAction) -> Component {
    let label = if is_on { "[ON]" } else { "[OFF]" };
    let bg = if is_on {
        Color::from_hex("#4cd964")
    } else {
        Color::from_hex("#808080")
    };
    let switch_style = Style {
        background: bg,
        border_radius: 16.0,
        ..style
    };
    Component::button_with_click(label, switch_style, on_toggle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_creates_column_by_default() {
        let view = View(Style::default(), vec![]);
        assert!(matches!(
            view.kind,
            w3cos_std::ComponentKind::Column
        ));
    }

    #[test]
    fn view_creates_row_when_row_direction() {
        let style = Style {
            flex_direction: FlexDirection::Row,
            ..Style::default()
        };
        let view = View(style, vec![]);
        assert!(matches!(
            view.kind,
            w3cos_std::ComponentKind::Row
        ));
    }

    #[test]
    fn text_creates_text_component() {
        let text = Text("Hello", Style::default());
        assert!(matches!(
            text.kind,
            w3cos_std::ComponentKind::Text { .. }
        ));
    }

    #[test]
    fn scroll_view_has_overflow_scroll() {
        let sv = ScrollView(Style::default(), vec![]);
        assert!(matches!(sv.style.overflow, Overflow::Scroll));
    }

    #[test]
    fn image_creates_image_component() {
        let img = Image("test.png", Style::default());
        assert!(matches!(
            img.kind,
            w3cos_std::ComponentKind::Image { .. }
        ));
    }

    #[test]
    fn text_input_creates_text_input_component() {
        let ti = TextInput("", "Enter text...", Style::default());
        assert!(matches!(
            ti.kind,
            w3cos_std::ComponentKind::TextInput { .. }
        ));
    }

    #[test]
    fn flat_list_renders_items() {
        let data = vec!["Item 1", "Item 2", "Item 3"];
        let list = FlatList(&data, |item, _| Text(*item, Style::default()), Style::default());
        assert_eq!(list.children.len(), 3);
    }

    #[test]
    fn switch_renders_on_state() {
        let sw = Switch(true, Style::default(), EventAction::None);
        if let w3cos_std::ComponentKind::Button { label } = &sw.kind {
            assert_eq!(label, "[ON]");
        } else {
            panic!("Expected Button");
        }
    }
}
