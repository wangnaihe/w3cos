pub mod css_style;
pub mod document;
pub mod element;
pub mod events;
pub mod node;
pub mod window;

pub use document::Document;
pub use element::Element;
pub use events::{Event, EventHandler, EventType};
pub use node::{NodeId, NodeType};
pub use window::Window;

#[cfg(test)]
mod tests {
    use crate::css_style::CSSStyleDeclaration;
    use crate::document::Document;
    use crate::events::{Event, EventType};
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
        assert_eq!(doc.get_node(text.id).tag, "#text");
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
    fn test_event_type_from_str_invalid() {
        assert_eq!(EventType::from_str("invalid"), None);
        assert_eq!(EventType::from_str(""), None);
        assert_eq!(EventType::from_str("CLICK"), None); // case sensitive
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
        assert_eq!(style.inner.padding.top, 16.0);
        assert_eq!(style.inner.padding.bottom, 16.0);
        style.set_property("margin", "8px");
        assert_eq!(style.inner.margin.top, 8.0);
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
