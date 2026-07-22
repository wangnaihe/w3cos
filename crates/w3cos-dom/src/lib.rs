pub mod atom;
pub mod canvas;
pub mod css_style;
pub mod document;
pub mod dom_rect;
pub mod element;
pub mod events;
pub mod history;
pub mod host_runtime;
pub mod location;
pub mod node;
pub mod selection;
pub mod stylesheet;
pub mod user_agent;
pub mod window;

pub use document::Document;
pub use dom_rect::DOMRect;
pub use element::Element;
pub use events::{
    Event, EventData, EventHandler, EventPhase, EventType, KeyboardEventData, ListenerOptions,
    MouseEventData, PointerEventData, WheelEventData,
};
pub use history::History;
pub use location::Location;
pub use node::{NodeId, NodeType};
pub use window::Window;

#[cfg(test)]
mod tests {
    use crate::atom::Atom;
    use crate::css_style::CSSStyleDeclaration;
    use crate::document::Document;
    use crate::events::{Event, EventType};
    use crate::stylesheet;
    use w3cos_std::style::Dimension;

    // --- Document tests ---

    #[test]
    fn test_document_create_element() {
        let mut doc = Document::new();
        let div = doc.create_element("div");
        assert_eq!(div.tag_name(&doc), "div");
        assert_eq!(doc.node_count(), 3); // root + body + div
    }

    #[test]
    fn test_document_create_text_node() {
        let mut doc = Document::new();
        let text = doc.create_text_node("Hello World");
        assert_eq!(text.text_content(&doc), Some("Hello World"));
        assert_eq!(doc.get_node(text.id).tag_str(), "#text");
    }

    #[test]
    fn test_document_body() {
        let doc = Document::new();
        let body = doc.body();
        assert_eq!(body.tag_name(&doc), "body");
    }

    #[test]
    fn test_document_append_child() {
        let mut doc = Document::new();
        let div = doc.create_element("div");
        let body = doc.body();
        body.append_child(&mut doc, div);
        let children = body.children(&doc);
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].tag_name(&doc), "div");
    }

    #[test]
    fn test_document_query_selector_id() {
        let mut doc = Document::new();
        let div = doc.create_element("div");
        div.set_attribute(&mut doc, "id", "main");
        doc.body().append_child(&mut doc, div);
        let found = doc.query_selector("#main");
        assert!(found.is_some());
        assert_eq!(found.unwrap().get_attribute(&doc, "id"), Some("main"));
    }

    #[test]
    fn test_document_query_selector_class() {
        let mut doc = Document::new();
        let div = doc.create_element("div");
        div.class_list_add(&mut doc, "container");
        doc.body().append_child(&mut doc, div);
        let found = doc.query_selector(".container");
        assert!(found.is_some());
        assert!(found.unwrap().class_list_contains(&doc, "container"));
    }

    #[test]
    fn test_document_query_selector_tag() {
        let mut doc = Document::new();
        let div = doc.create_element("div");
        doc.body().append_child(&mut doc, div);
        let found = doc.query_selector("div");
        assert!(found.is_some());
        assert_eq!(found.unwrap().tag_name(&doc), "div");
    }

    #[test]
    fn test_document_query_selector_all() {
        let mut doc = Document::new();
        let div1 = doc.create_element("div");
        div1.class_list_add(&mut doc, "item");
        let div2 = doc.create_element("span");
        div2.class_list_add(&mut doc, "item");
        doc.body().append_child(&mut doc, div1);
        doc.body().append_child(&mut doc, div2);
        let found = doc.query_selector_all(".item");
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn test_document_query_selector_all_tag() {
        let mut doc = Document::new();
        let div1 = doc.create_element("span");
        let div2 = doc.create_element("span");
        doc.body().append_child(&mut doc, div1);
        doc.body().append_child(&mut doc, div2);
        let found = doc.query_selector_all("span");
        assert_eq!(found.len(), 2);
    }

    // --- Element tests ---

    #[test]
    fn test_element_tag_name() {
        let mut doc = Document::new();
        let el = doc.create_element("section");
        assert_eq!(el.tag_name(&doc), "section");
    }

    #[test]
    fn test_element_text_content() {
        let mut doc = Document::new();
        let el = doc.create_element("p");
        assert_eq!(el.text_content(&doc), None);
        el.set_text_content(&mut doc, "Hello");
        assert_eq!(el.text_content(&doc), Some("Hello"));
    }

    #[test]
    fn test_element_set_text_content() {
        let mut doc = Document::new();
        let el = doc.create_element("p");
        el.set_text_content(&mut doc, "Initial");
        assert_eq!(el.text_content(&doc), Some("Initial"));
        el.set_text_content(&mut doc, "Updated");
        assert_eq!(el.text_content(&doc), Some("Updated"));
    }

    #[test]
    fn inline_element_with_text_node_lowers_to_text_component() {
        use w3cos_std::ComponentKind;

        let mut doc = Document::new();
        let span = doc.create_element("span");
        doc.get_node_mut(span.id)
            .class_list
            .push(Atom::intern("token"));
        let text = doc.create_text_node("hello");
        doc.append_child(span.id, text.id);
        doc.append_child(doc.body().id, span.id);

        stylesheet::clear_rules();
        stylesheet::register_rule(".token", &[("color", "#d4d4d4")]);
        let tree = doc.to_component_tree();
        assert!(matches!(
            &tree.children[0].kind,
            ComponentKind::Text { content } if content == "hello"
        ));
        assert_eq!(
            tree.children[0].style.color,
            w3cos_std::Color::rgb(212, 212, 212)
        );
        stylesheet::clear_rules();
    }

    #[test]
    fn test_element_set_attribute() {
        let mut doc = Document::new();
        let el = doc.create_element("div");
        el.set_attribute(&mut doc, "id", "test-id");
        el.set_attribute(&mut doc, "data-foo", "bar");
        assert_eq!(el.get_attribute(&doc, "id"), Some("test-id"));
        assert_eq!(el.get_attribute(&doc, "data-foo"), Some("bar"));
    }

    #[test]
    fn test_element_get_attribute() {
        let mut doc = Document::new();
        let el = doc.create_element("a");
        el.set_attribute(&mut doc, "href", "https://example.com");
        assert_eq!(el.get_attribute(&doc, "href"), Some("https://example.com"));
        assert_eq!(el.get_attribute(&doc, "nonexistent"), None);
    }

    #[test]
    fn test_element_set_attribute_overwrite() {
        let mut doc = Document::new();
        let el = doc.create_element("div");
        el.set_attribute(&mut doc, "id", "old");
        el.set_attribute(&mut doc, "id", "new");
        assert_eq!(el.get_attribute(&doc, "id"), Some("new"));
    }

    #[test]
    fn test_element_class_list_add() {
        let mut doc = Document::new();
        let el = doc.create_element("div");
        assert!(!el.class_list_contains(&doc, "active"));
        el.class_list_add(&mut doc, "active");
        assert!(el.class_list_contains(&doc, "active"));
        el.class_list_add(&mut doc, "active"); // idempotent
        assert!(el.class_list_contains(&doc, "active"));
    }

    #[test]
    fn test_element_class_list_remove() {
        let mut doc = Document::new();
        let el = doc.create_element("div");
        el.class_list_add(&mut doc, "foo");
        el.class_list_remove(&mut doc, "foo");
        assert!(!el.class_list_contains(&doc, "foo"));
    }

    #[test]
    fn test_element_class_list_toggle() {
        let mut doc = Document::new();
        let el = doc.create_element("div");
        let added = el.class_list_toggle(&mut doc, "highlight");
        assert!(added);
        assert!(el.class_list_contains(&doc, "highlight"));
        let removed = el.class_list_toggle(&mut doc, "highlight");
        assert!(!removed);
        assert!(!el.class_list_contains(&doc, "highlight"));
    }

    #[test]
    fn test_element_class_list_contains() {
        let mut doc = Document::new();
        let el = doc.create_element("div");
        el.class_list_add(&mut doc, "visible");
        assert!(el.class_list_contains(&doc, "visible"));
        assert!(!el.class_list_contains(&doc, "hidden"));
    }

    // --- Events tests ---

    #[test]
    fn test_event_type_from_str_click() {
        assert_eq!(EventType::from_str("click"), Some(EventType::Click));
    }

    #[test]
    fn test_event_type_from_str_all_variants() {
        let cases = [
            ("click", EventType::Click),
            ("mousedown", EventType::MouseDown),
            ("mouseup", EventType::MouseUp),
            ("mouseenter", EventType::MouseEnter),
            ("mouseleave", EventType::MouseLeave),
            ("keydown", EventType::KeyDown),
            ("keyup", EventType::KeyUp),
            ("focus", EventType::Focus),
            ("blur", EventType::Blur),
            ("input", EventType::Input),
            ("change", EventType::Change),
            ("scroll", EventType::Scroll),
            ("resize", EventType::Resize),
        ];
        for (s, expected) in cases {
            assert_eq!(EventType::from_str(s), Some(expected), "failed for {}", s);
        }
    }

    #[test]
    fn test_event_type_from_str_custom() {
        // Unknown event names now produce Custom variants
        let ev = EventType::from_str("myCustomEvent");
        assert!(matches!(ev, Some(EventType::Custom(_))));
        // Known names remain correct
        assert_eq!(EventType::from_str("click"), Some(EventType::Click));
    }

    #[test]
    fn test_add_event_listener() {
        let mut doc = Document::new();
        let btn = doc.create_element("button");
        doc.body().append_child(&mut doc, btn);
        btn.add_event_listener(
            &mut doc,
            "click",
            Box::new(|e: &mut Event| {
                e.prevent_default();
            }),
        );
        let mut ev = Event::click(btn.id, 10.0, 20.0);
        btn.dispatch_event(&mut doc, &mut ev);
        assert!(ev.prevent_default);
    }

    #[test]
    fn test_add_event_listener_invalid_event_ignored() {
        let mut doc = Document::new();
        let el = doc.create_element("div");
        doc.body().append_child(&mut doc, el);
        el.add_event_listener(&mut doc, "nonexistent", Box::new(|_| {}));
        // Should not panic; invalid events are silently ignored
    }

    // --- Stylesheet registry integration (to_component_tree) ---

    #[test]
    fn test_stylesheet_class_rule_applies_in_component_tree() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(".title", &[("font-size", "24px"), ("color", "#ff0000")]);

        let mut doc = Document::new();
        let el = doc.create_element("div");
        el.class_list_add(&mut doc, "title");
        el.set_text_content(&mut doc, "hello");
        doc.body().append_child(&mut doc, el);

        let tree = doc.to_component_tree();
        let child = &tree.children[0];
        assert_eq!(child.style.font_size, 24.0);
        assert_eq!(child.style.color.r, 255);
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn test_stylesheet_inline_style_wins() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(".title", &[("color", "#ff0000"), ("width", "42px")]);

        let mut doc = Document::new();
        let el = doc.create_element("div");
        el.class_list_add(&mut doc, "title");
        el.style_mut(&mut doc).set_property("color", "#0000ff");
        doc.body().append_child(&mut doc, el);

        let tree = doc.to_component_tree();
        let style = &tree.children[0].style;
        // Inline color overrides the matched rule...
        assert_eq!(style.color.b, 255);
        assert_eq!(style.color.r, 0);
        // ...while the untouched width still comes from the stylesheet.
        assert!(matches!(style.width, Dimension::Px(42.0)));
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn html_form_controls_use_ua_defaults_below_author_styles() {
        crate::stylesheet::clear_rules();
        let mut doc = Document::new();
        let input = doc.create_element("input");
        input.set_attribute(&mut doc, "value", "demo");
        doc.body().append_child(&mut doc, input);

        let tree = doc.to_component_tree();
        assert_eq!(tree.children[0].style.background, w3cos_std::Color::WHITE);
        assert_eq!(tree.children[0].style.border_width, 1.0);

        crate::stylesheet::register_rule(
            "input",
            &[("background-color", "#123456"), ("border-radius", "8px")],
        );
        let tree = doc.to_component_tree();
        assert_eq!(
            tree.children[0].style.background,
            w3cos_std::Color::rgb(18, 52, 86)
        );
        assert_eq!(tree.children[0].style.border_radius, 8.0);
        assert_eq!(tree.children[0].style.border_width, 1.0);
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn test_stylesheet_descendant_selector_uses_dom_ancestors() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            ".monaco-editor .find-widget",
            &[("position", "absolute")],
        );

        let mut doc = Document::new();
        let outer = doc.create_element("div");
        outer.class_list_add(&mut doc, "monaco-editor");
        let inner = doc.create_element("div");
        inner.class_list_add(&mut doc, "find-widget");
        doc.body().append_child(&mut doc, outer);
        outer.append_child(&mut doc, inner);

        let tree = doc.to_component_tree();
        let inner_component = &tree.children[0].children[0];
        assert!(matches!(
            inner_component.style.position,
            w3cos_std::style::Position::Absolute
        ));
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn test_monaco_nested_inline_span_collapses_to_styled_text() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(".monaco-editor.vs-dark .mtk1", &[("color", "#d4d4d4")]);
        crate::stylesheet::register_rule(
            ".monaco-editor .view-line > span",
            &[("position", "absolute")],
        );

        let mut doc = Document::new();
        let editor = doc.create_element("div");
        editor.class_list_add(&mut doc, "monaco-editor");
        editor.class_list_add(&mut doc, "vs-dark");
        let line = doc.create_element("div");
        line.class_list_add(&mut doc, "view-line");
        let outer = doc.create_element("span");
        let token = doc.create_element("span");
        token.class_list_add(&mut doc, "mtk1");
        let text = doc.create_text_node("function hello() {");

        doc.body().append_child(&mut doc, editor);
        editor.append_child(&mut doc, line);
        line.append_child(&mut doc, outer);
        outer.append_child(&mut doc, token);
        doc.append_child(token.id, text.id);

        let tree = doc.to_component_tree();
        let rendered_line = &tree.children[0].children[0];
        assert_eq!(rendered_line.children.len(), 1);
        let rendered_text = &rendered_line.children[0];
        assert!(matches!(
            &rendered_text.kind,
            w3cos_std::ComponentKind::Text { content } if content == "function hello() {"
        ));
        assert_eq!(rendered_text.style.color.r, 0xd4);
        assert_eq!(rendered_text.style.color.g, 0xd4);
        assert_eq!(rendered_text.style.color.b, 0xd4);
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn test_textarea_becomes_focusable_native_text_input() {
        let mut doc = Document::new();
        let textarea = doc.create_element("textarea");
        textarea.set_attribute(&mut doc, "value", "hello");
        doc.body().append_child(&mut doc, textarea);

        let tree = doc.to_component_tree();
        let component = &tree.children[0];
        assert!(matches!(
            &component.kind,
            w3cos_std::ComponentKind::TextInput { value, .. } if value == "hello"
        ));
        assert!(matches!(
            component.on_click,
            w3cos_std::EventAction::NativeHost {
                id,
                input: true,
                focus: true,
                keyboard: true,
                ..
            } if id == textarea.id.as_u32() as u64
        ));
    }

    #[test]
    fn test_dom_container_keeps_native_host_for_pointer_dispatch() {
        let mut doc = Document::new();
        let editor = doc.create_element("div");
        let line = doc.create_element("div");
        editor.append_child(&mut doc, line);
        doc.body().append_child(&mut doc, editor);

        let tree = doc.to_component_tree();
        let component = &tree.children[0];
        assert!(matches!(
            component.on_click,
            w3cos_std::EventAction::NativeHost {
                id,
                pointer: true,
                ..
            } if id == editor.id.as_u32() as u64
        ));
    }

    #[test]
    fn test_component_subtree_preserves_dom_ancestry_and_host_identity() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(".panel .action", &[("color", "#123456")]);
        let mut doc = Document::new();
        let panel = doc.create_element("section");
        panel.set_class_name(&mut doc, "panel");
        let button = doc.create_element("button");
        button.set_class_name(&mut doc, "action");
        let label = doc.create_text_node("Dispatch");
        button.append_child(&mut doc, label);
        panel.append_child(&mut doc, button);
        doc.body().append_child(&mut doc, panel);

        let component = doc.to_component_subtree(button.id);

        assert!(matches!(
            component.kind,
            w3cos_std::ComponentKind::Button { ref label } if label == "Dispatch"
        ));
        assert_eq!(component.style.color, w3cos_std::Color::from_hex("#123456"));
        assert!(matches!(
            component.on_click,
            w3cos_std::EventAction::NativeHost { id, .. }
                if id == button.id.as_u32() as u64
        ));
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn test_stylesheet_specificity_id_beats_class() {
        crate::stylesheet::clear_rules();
        // Class registered after id on purpose — specificity must win over order.
        crate::stylesheet::register_rule("#main", &[("color", "#ff0000")]);
        crate::stylesheet::register_rule(".box", &[("color", "#0000ff")]);

        let mut doc = Document::new();
        let el = doc.create_element("div");
        el.set_attribute(&mut doc, "id", "main");
        el.class_list_add(&mut doc, "box");
        doc.body().append_child(&mut doc, el);

        let tree = doc.to_component_tree();
        let style = &tree.children[0].style;
        assert_eq!(style.color.r, 255);
        assert_eq!(style.color.b, 0);
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn test_no_rules_registered_preserves_inline_only() {
        crate::stylesheet::clear_rules();
        let mut doc = Document::new();
        let el = doc.create_element("div");
        el.style_mut(&mut doc).set_property("width", "33px");
        doc.body().append_child(&mut doc, el);

        let tree = doc.to_component_tree();
        assert!(matches!(tree.children[0].style.width, Dimension::Px(33.0)));
    }

    // --- CSSStyleDeclaration tests ---

    #[test]
    fn test_css_set_get_display() {
        let mut style = CSSStyleDeclaration::new();
        style.set_property("display", "flex");
        assert_eq!(style.get_property("display"), "flex");
        style.set_property("display", "none");
        assert_eq!(style.get_property("display"), "none");
    }

    #[test]
    fn test_css_set_get_position() {
        let mut style = CSSStyleDeclaration::new();
        style.set_property("position", "absolute");
        assert_eq!(style.get_property("position"), "absolute");
    }

    #[test]
    fn test_css_set_property_width_height() {
        let mut style = CSSStyleDeclaration::new();
        style.set_property("width", "100px");
        assert!(matches!(style.inner.width, Dimension::Px(100.0)));
        style.set_property("height", "50%");
        assert!(matches!(style.inner.height, Dimension::Percent(50.0)));
    }

    #[test]
    fn test_css_parse_dimension_px() {
        let mut style = CSSStyleDeclaration::new();
        style.set_property("width", "42px");
        assert!(matches!(style.inner.width, Dimension::Px(42.0)));
    }

    #[test]
    fn test_css_parse_dimension_rem_em_vw_vh() {
        let mut style = CSSStyleDeclaration::new();
        style.set_property("width", "2rem");
        assert!(matches!(style.inner.width, Dimension::Rem(2.0)));
        style.set_property("width", "1.5em");
        assert!(matches!(style.inner.width, Dimension::Em(1.5)));
        style.set_property("width", "50vw");
        assert!(matches!(style.inner.width, Dimension::Vw(50.0)));
        style.set_property("width", "25vh");
        assert!(matches!(style.inner.width, Dimension::Vh(25.0)));
    }

    #[test]
    fn test_css_parse_dimension_percent_auto() {
        let mut style = CSSStyleDeclaration::new();
        style.set_property("width", "100%");
        assert!(matches!(style.inner.width, Dimension::Percent(100.0)));
        style.set_property("width", "auto");
        assert!(matches!(style.inner.width, Dimension::Auto));
    }

    #[test]
    fn test_css_set_property_padding_margin() {
        let mut style = CSSStyleDeclaration::new();
        style.set_property("padding", "16px");
        assert_eq!(style.inner.padding.top, w3cos_std::style::Spacing::Px(16.0));
        assert_eq!(
            style.inner.padding.bottom,
            w3cos_std::style::Spacing::Px(16.0)
        );
        style.set_property("margin", "8px");
        assert_eq!(style.inner.margin.top, w3cos_std::style::Spacing::Px(8.0));
    }

    #[test]
    fn test_css_set_property_font_size_color() {
        let mut style = CSSStyleDeclaration::new();
        style.set_property("font-size", "14px");
        assert_eq!(style.get_property("font-size"), "14px");
        style.set_property("color", "#ff0000");
        assert!(style.get_property("color").contains("ff"));
        assert!(style.get_property("color").contains("00"));
    }

    #[test]
    fn test_css_set_property_flex_direction() {
        let mut style = CSSStyleDeclaration::new();
        style.set_property("flex-direction", "row");
        assert_eq!(
            format!("{:?}", style.inner.flex_direction).to_lowercase(),
            "row"
        );
    }

    #[test]
    fn test_css_set_property_background() {
        let mut style = CSSStyleDeclaration::new();
        style.set_property("background-color", "#00ff00");
        assert_eq!(style.inner.background.g, 255);
    }

    #[test]
    fn test_cssom_supports_web_host_style_mutations() {
        use w3cos_std::Color;
        use w3cos_std::safe_area::SafeAreaEdge;
        use w3cos_std::style::{Overflow, Spacing};

        let mut style = CSSStyleDeclaration::new();
        style.set_property("fontSize", "16");
        style.set_property("lineHeight", "24px");
        style.set_property("marginTop", "12px");
        style.set_property("paddingTop", "calc(18px + env(safe-area-inset-top))");
        style.set_property("overflowY", "auto");
        style.set_property("backgroundColor", "rgba(10, 20, 30, 0.5)");

        assert_eq!(style.inner.line_height, 1.5);
        assert_eq!(style.inner.margin.top, Spacing::Px(12.0));
        assert!(matches!(
            style.inner.padding.top,
            Spacing::Composite {
                px: 18.0,
                safe_area: Some(SafeAreaEdge::Top),
                keyboard_inset: false,
            }
        ));
        assert!(matches!(style.inner.overflow, Overflow::Auto));
        assert_eq!(style.inner.background, Color::rgba(10, 20, 30, 128));
    }

    #[test]
    fn test_css_compositor_properties() {
        let mut style = CSSStyleDeclaration::new();
        style.set_property("will-change", "transform, opacity");
        assert!(style.inner.will_change.transform);
        assert!(style.inner.will_change.opacity);
        assert_eq!(style.get_property("will-change"), "transform, opacity");

        style.set_property("contain", "layout");
        assert!(matches!(
            style.inner.contain,
            w3cos_std::style::Contain::Layout
        ));

        style.set_property("filter", "blur(4px)");
        assert_eq!(style.inner.filter.as_deref(), Some("blur(4px)"));
        style.set_property("filter", "none");
        assert!(style.inner.filter.is_none());
        assert_eq!(style.get_property("filter"), "none");
    }

    // --- Node tree tests ---

    #[test]
    fn test_node_tree_append_child_parent_relationship() {
        let mut doc = Document::new();
        let parent = doc.create_element("div");
        let child = doc.create_element("span");
        doc.body().append_child(&mut doc, parent);
        parent.append_child(&mut doc, child);
        assert_eq!(child.parent_element(&doc).map(|e| e.id), Some(parent.id));
        assert_eq!(parent.children(&doc).len(), 1);
        assert_eq!(parent.children(&doc)[0].id, child.id);
    }

    #[test]
    fn test_node_tree_remove_child() {
        let mut doc = Document::new();
        let parent = doc.create_element("div");
        let child = doc.create_element("span");
        doc.body().append_child(&mut doc, parent);
        parent.append_child(&mut doc, child);
        assert_eq!(parent.children(&doc).len(), 1);
        parent.remove_child(&mut doc, &child);
        assert_eq!(parent.children(&doc).len(), 0);
        assert!(child.parent_element(&doc).is_none());
    }

    #[test]
    fn test_remove_node_reclaims_a_retained_subtree() {
        let mut doc = Document::new();
        let parent = doc.create_element("div");
        let child = doc.create_element("span");
        doc.body().append_child(&mut doc, parent);
        parent.append_child(&mut doc, child);
        assert_eq!(doc.node_count(), 4);

        doc.remove_node(parent.id);

        assert_eq!(doc.node_count(), 2);
        assert!(doc.body().children(&doc).is_empty());
    }

    #[test]
    fn test_node_tree_multiple_children() {
        let mut doc = Document::new();
        let parent = doc.create_element("div");
        let c1 = doc.create_element("span");
        let c2 = doc.create_element("span");
        doc.body().append_child(&mut doc, parent);
        parent.append_child(&mut doc, c1);
        parent.append_child(&mut doc, c2);
        let children = parent.children(&doc);
        assert_eq!(children.len(), 2);
        assert_eq!(c1.parent_element(&doc).map(|e| e.id), Some(parent.id));
        assert_eq!(c2.parent_element(&doc).map(|e| e.id), Some(parent.id));
    }

    #[test]
    fn test_node_tree_move_child_to_new_parent() {
        let mut doc = Document::new();
        let p1 = doc.create_element("div");
        let p2 = doc.create_element("div");
        let child = doc.create_element("span");
        doc.body().append_child(&mut doc, p1);
        doc.body().append_child(&mut doc, p2);
        p1.append_child(&mut doc, child);
        assert_eq!(child.parent_element(&doc).map(|e| e.id), Some(p1.id));
        p2.append_child(&mut doc, child);
        assert_eq!(child.parent_element(&doc).map(|e| e.id), Some(p2.id));
        assert_eq!(p1.children(&doc).len(), 0);
        assert_eq!(p2.children(&doc).len(), 1);
    }
}
