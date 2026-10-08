use super::Document;
use crate::events::EventType;
use w3cos_std::{ComponentKind, style::Display};

#[test]
fn html_control_auto_appearance_yields_to_author_decoration() {
    for (tag, part) in [("input", "textfield"), ("button", "button")] {
        for (property, value, expected) in [
            ("color", "blue", part),
            ("font-size", "20px", part),
            ("appearance", "none", "none"),
            ("-webkit-appearance", "none", "none"),
            ("border", "2px solid red", "none"),
            ("background-color", "pink", "none"),
        ] {
            let mut document = Document::new();
            let control = document.create_element(tag);
            document.body().append_child(&mut document, control);
            control.style_mut(&mut document).set_property(property, value);
            let style = document.computed_style_for(control.id);
            assert_eq!(style.custom_properties.as_ref()
                .and_then(|p| p.get("--w3cos-internal-html-control-appearance"))
                .map(String::as_str), Some(expected), "{tag}: {property}: {value}");
        }
    }
}

#[test]
fn ua_field_border_color_is_not_replaced_by_initial_currentcolor() {
    for tag in ["input", "textarea", "select", "button"] {
        let mut document = Document::new();
        let control = document.create_element(tag);
        control.style_mut(&mut document).set_property("color", "blue");
        document.body().append_child(&mut document, control);
        let style = document.computed_style_for(control.id);
        assert_eq!(style.border_color, w3cos_std::Color::rgb(118, 118, 118), "{tag}");
        assert_eq!(style.border_current_color, Some([false; 4]), "{tag}");
        control.style_mut(&mut document).set_property("border-color", "currentcolor");
        let style = document.computed_style_for(control.id);
        assert_eq!(style.border_color, style.color, "{tag}");
        assert_eq!(style.border_current_color, Some([true; 4]), "{tag}");
    }
}

#[test]
fn controls_keep_ua_font_unless_author_explicitly_inherits() {
    for tag in ["input", "button", "select", "textarea"] {
        let mut document = Document::new();
        let parent = document.create_element("div");
        parent.style_mut(&mut document).set_property("font", "italic bold 20px/3 Ahem");
        let control = document.create_element(tag);
        parent.append_child(&mut document, control);
        document.body().append_child(&mut document, parent);
        let style = document.computed_style_for(control.id);
        assert_eq!(style.font_family.as_deref(),
            Some(if tag == "textarea" { "monospace" } else { "Arial" }), "{tag}");
        assert_eq!(style.font_weight, 400, "{tag}");
        assert_eq!(style.font_style, w3cos_std::style::FontStyle::Normal, "{tag}");
        assert!(style.line_height_is_normal, "{tag}");
        assert!(style.font_size < 14.0, "{tag}");
        control.style_mut(&mut document).set_property("font", "inherit");
        let inherited = document.computed_style_for(parent.id);
        let style = document.computed_style_for(control.id);
        assert_eq!(style.font_family, inherited.font_family, "{tag}");
        assert_eq!(style.font_size, inherited.font_size, "{tag}");
        assert_eq!(style.font_weight, inherited.font_weight, "{tag}");
        assert_eq!(style.font_style, inherited.font_style, "{tag}");
        assert_eq!(style.line_height, inherited.line_height, "{tag}");
    }
}

#[test]
fn interactive_atomic_control_keeps_shared_inline_word_breaks() {
    for interactive in [false, true] {
        let mut document = Document::new();
        let paragraph = document.create_element("p");
        let text = document.create_text_node("alpha beta gamma delta ");
        paragraph.append_child(&mut document, text);
        let input = document.create_element("input");
        paragraph.append_child(&mut document, input);
        let button = document.create_element("button");
        button.set_text_content(&mut document, "Apply");
        if interactive {
            document.events.add(button.id, EventType::Click, Box::new(|_| {}));
        }
        paragraph.append_child(&mut document, button);
        document.body().append_child(&mut document, paragraph);
        let component = document.to_component_subtree(paragraph.id);
        assert!(component.style.custom_properties.as_ref().is_some_and(|properties|
            properties.contains_key("--w3cos-internal-inline-formatting-context")),
            "listener must not disable the line context: interactive={interactive}");
        assert!(component.children.iter().filter(|child|
            matches!(child.kind, ComponentKind::Text { .. })).count() > 1,
            "text before an atomic control must expose word boundaries");
        assert!(component.children.iter().any(|child|
            matches!(child.kind, ComponentKind::Button { .. }) && child.style.display == Display::InlineBlock));
    }
}
