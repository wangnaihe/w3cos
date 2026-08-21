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
    fn svg_subtree_lowers_to_one_retained_document() {
        let mut doc = Document::new();
        let svg = doc.create_element("svg");
        svg.set_attribute(&mut doc, "width", "100");
        svg.set_attribute(&mut doc, "height", "80");

        let path = doc.create_element("path");
        path.set_attribute(&mut doc, "id", "route");
        path.set_attribute(&mut doc, "d", "M10 20h20v20z");
        path.set_attribute(&mut doc, "fill", "#ff0000");
        path.set_attribute(&mut doc, "stroke", "#0000ff");
        path.set_attribute(&mut doc, "stroke-width", "2");
        path.set_attribute(&mut doc, "transform", "translate(3 4) scale(2)");
        svg.append_child(&mut doc, path);

        let polygon = doc.create_element("polygon");
        polygon.set_attribute(&mut doc, "points", "0,0 10,0 5,10");
        polygon
            .style_mut(&mut doc)
            .set_property("fill", "rgb(0, 255, 0)");
        polygon
            .style_mut(&mut doc)
            .set_property("pointer-events", "none");
        svg.append_child(&mut doc, polygon);
        let label = doc.create_element("text");
        let label_text = doc.create_text_node("A&B");
        label.append_child(&mut doc, label_text);
        svg.append_child(&mut doc, label);
        doc.body().append_child(&mut doc, svg);

        let tree = doc.to_component_tree();
        let svg = &tree.children[0];
        let w3cos_std::ComponentKind::SvgDocument {
            source,
            width,
            height,
            event_targets,
        } = &svg.kind
        else {
            panic!("SVG root should lower to SvgDocument");
        };
        assert_eq!((*width, *height), (100, 80));
        assert!(
            event_targets
                .iter()
                .any(|target| target.host_chain.len() > 1)
        );
        assert!(event_targets.iter().any(|target| {
            target.svg_id.is_empty()
                && target.render_index.is_some()
                && target.pointer_events == "none"
        }));
        assert!(
            source.contains("xmlns=\"http://www.w3.org/2000/svg\"")
                && source.contains("d=\"M10 20h20v20z\"")
                && source.contains("<polygon points=\"0,0 10,0 5,10\"")
                && source.contains("style=\"fill:rgb(0, 255, 0);pointer-events:none;\"")
                && source.contains(">A&amp;B</text>"),
            "{source}"
        );
        assert!(!source.contains("__w3cos_dom_node_"));
        assert!(svg.children.is_empty());
    }

    #[test]
    fn svg_current_color_uses_the_host_computed_color() {
        let mut doc = Document::new();
        let host = doc.create_element("span");
        host.style_mut(&mut doc).set_property("color", "#f8fafc");
        let svg = doc.create_element("svg");
        svg.set_attribute(&mut doc, "width", "24");
        svg.set_attribute(&mut doc, "height", "24");
        let path = doc.create_element("path");
        path.set_attribute(&mut doc, "d", "M0 0h24v24z");
        path.set_attribute(&mut doc, "fill", "currentColor");
        path.set_attribute(&mut doc, "stroke", "currentColor");
        svg.append_child(&mut doc, path);
        host.append_child(&mut doc, svg);
        doc.body().append_child(&mut doc, host);

        let tree = doc.to_component_tree();
        let svg = tree.children[0]
            .children
            .first()
            .expect("inline host SVG child");
        let w3cos_std::ComponentKind::SvgDocument { source, .. } = &svg.kind else {
            panic!("inline host child should lower to an SVG document");
        };
        assert!(
            source.contains("fill=\"rgba(248, 250, 252, 1)\""),
            "{source}"
        );
        assert!(
            source.contains("stroke=\"rgba(248, 250, 252, 1)\""),
            "{source}"
        );
        assert!(!source.contains("currentColor"), "{source}");
    }

    #[test]
    fn stroke_only_svg_inherits_color_and_explicit_size_through_button_host() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            ".tool",
            &[("color", "#295da7"), ("width", "46px"), ("height", "46px")],
        );
        crate::stylesheet::register_rule(".tool .semantic-icon", &[("font-size", "21px")]);
        crate::stylesheet::register_rule(
            ".semantic-icon > svg",
            &[("width", "1em"), ("height", "1em")],
        );

        let mut doc = Document::new();
        let button = doc.create_element("button");
        button.set_class_name(&mut doc, "tool");
        let host = doc.create_element("span");
        host.set_class_name(&mut doc, "semantic-icon");
        let svg = doc.create_element("svg");
        svg.set_attribute(&mut doc, "viewBox", "0 0 24 24");
        svg.set_attribute(&mut doc, "fill", "none");
        svg.set_attribute(&mut doc, "stroke", "currentColor");
        svg.set_attribute(&mut doc, "stroke-width", "1.8");
        let path = doc.create_element("path");
        path.set_attribute(&mut doc, "d", "M5 12h14");
        svg.append_child(&mut doc, path);
        host.append_child(&mut doc, svg);
        button.append_child(&mut doc, host);
        doc.body().append_child(&mut doc, button);

        let tree = doc.to_component_tree();
        let button = &tree.children[0];
        let host = &button.children[0];
        let svg = &host.children[0];
        let w3cos_std::ComponentKind::SvgDocument {
            source,
            width,
            height,
            ..
        } = &svg.kind
        else {
            panic!("button icon should lower to an SVG document");
        };
        assert_eq!((*width, *height), (21, 21));
        assert_eq!(host.style.font_size, 21.0);
        assert_eq!(svg.style.border_width, 0.0);
        assert!(
            source.contains("stroke=\"rgba(41, 93, 167, 1)\""),
            "{source}"
        );
    }

    #[test]
    fn button_preserves_image_children_for_attachment_previews() {
        let mut doc = Document::new();
        let button = doc.create_element("button");
        let image = doc.create_element("img");
        image.set_attribute(&mut doc, "src", "blob:w3cos/attachment-preview");
        button.append_child(&mut doc, image);
        doc.body().append_child(&mut doc, button);

        let tree = doc.to_component_tree();
        let button = &tree.children[0];
        assert_eq!(button.children.len(), 1);
        assert!(matches!(
            &button.children[0].kind,
            w3cos_std::ComponentKind::Image { src } if src == "blob:w3cos/attachment-preview"
        ));
    }

    #[test]
    fn svg_defs_do_not_shift_anonymous_paint_ordinals() {
        let mut doc = Document::new();
        let svg = doc.create_element("svg");
        svg.set_attribute(&mut doc, "width", "100");
        svg.set_attribute(&mut doc, "height", "50");
        let defs = doc.create_element("defs");
        let template = doc.create_element("circle");
        template.set_attribute(&mut doc, "id", "template");
        template.set_attribute(&mut doc, "r", "5");
        defs.append_child(&mut doc, template);
        svg.append_child(&mut doc, defs);
        let painted = doc.create_element("rect");
        painted.set_attribute(&mut doc, "width", "20");
        painted.set_attribute(&mut doc, "height", "10");
        svg.append_child(&mut doc, painted);
        let instance = doc.create_element("use");
        instance.set_attribute(&mut doc, "href", "#template");
        svg.append_child(&mut doc, instance);
        doc.body().append_child(&mut doc, svg);

        let tree = doc.to_component_tree();
        let w3cos_std::ComponentKind::SvgDocument {
            source,
            event_targets,
            ..
        } = &tree.children[0].kind
        else {
            panic!("SVG root should lower to SvgDocument");
        };
        assert!(
            !event_targets
                .iter()
                .any(|target| target.svg_id == "template")
        );
        assert!(
            event_targets
                .iter()
                .any(|target| target.svg_id.is_empty() && target.render_index == Some(0))
        );
        assert!(
            event_targets
                .iter()
                .any(|target| target.svg_id.starts_with("__w3cos_internal_use_")
                    && target.render_index.is_none())
        );
        assert!(source.contains("<use href=\"#template\" id=\"__w3cos_internal_use_"));
        assert!(instance.get_attribute(&doc, "id").is_none());
    }

    #[test]
    fn svg_event_targets_inherit_pointer_events() {
        let mut doc = Document::new();
        let svg = doc.create_element("svg");
        let group = doc.create_element("g");
        group.set_attribute(&mut doc, "pointer-events", "fill");
        let circle = doc.create_element("circle");
        circle.set_attribute(&mut doc, "fill", "none");
        circle.set_attribute(&mut doc, "stroke", "none");
        group.append_child(&mut doc, circle);
        svg.append_child(&mut doc, group);
        doc.body().append_child(&mut doc, svg);

        let tree = doc.to_component_tree();
        let w3cos_std::ComponentKind::SvgDocument { event_targets, .. } = &tree.children[0].kind
        else {
            panic!("SVG root should lower to SvgDocument");
        };
        let circle_target = event_targets
            .iter()
            .find(|target| target.render_index == Some(0))
            .expect("anonymous circle should have an event target");
        assert_eq!(circle_target.pointer_events, "fill");
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
    fn test_element_namespaced_attributes_retain_identity_and_clone() {
        let mut doc = Document::new();
        let el = doc.create_element("use");
        el.set_attribute_ns(
            &mut doc,
            Some("http://www.w3.org/1999/xlink"),
            "xlink:href",
            Some("xlink"),
            "href",
            "#shape",
        );
        assert_eq!(el.get_attribute(&doc, "xlink:href"), Some("#shape"));
        assert_eq!(
            el.get_attribute_ns(&doc, Some("http://www.w3.org/1999/xlink"), "href"),
            Some("#shape")
        );
        assert_eq!(el.get_attribute_ns(&doc, None, "href"), None);
        el.set_attribute(&mut doc, "xlink:href", "#ordinary");
        assert_eq!(
            el.get_attribute_ns(&doc, Some("http://www.w3.org/1999/xlink"), "href"),
            None
        );
        el.set_attribute_ns(
            &mut doc,
            Some("http://www.w3.org/1999/xlink"),
            "xlink:href",
            Some("xlink"),
            "href",
            "#shape",
        );
        el.set_attribute_ns(
            &mut doc,
            Some("http://www.w3.org/1999/xlink"),
            "legacy:href",
            Some("legacy"),
            "href",
            "#renamed",
        );
        assert_eq!(el.get_attribute(&doc, "xlink:href"), None);
        assert_eq!(el.get_attribute(&doc, "legacy:href"), Some("#renamed"));
        assert_eq!(
            el.get_attribute_ns(&doc, Some("http://www.w3.org/1999/xlink"), "href"),
            Some("#renamed")
        );

        let cloned = doc.clone_node(el.id, false);
        let cloned = crate::Element::new(cloned);
        assert_eq!(
            cloned.get_attribute_ns(&doc, Some("http://www.w3.org/1999/xlink"), "href"),
            Some("#renamed")
        );
        assert!(cloned.remove_attribute_ns(&mut doc, Some("http://www.w3.org/1999/xlink"), "href"));
        assert_eq!(
            cloned.get_attribute_ns(&doc, Some("http://www.w3.org/1999/xlink"), "href"),
            None
        );
    }

    #[test]
    fn test_class_attribute_updates_selector_state() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(".title", &[("font-size", "24px")]);

        let mut doc = Document::new();
        let el = doc.create_element("div");
        el.set_attribute(&mut doc, "class", "title featured");
        assert_eq!(el.get_attribute(&doc, "class"), Some("title featured"));
        assert!(el.class_list_contains(&doc, "title"));
        assert!(el.class_list_contains(&doc, "featured"));

        el.set_text_content(&mut doc, "hello");
        doc.body().append_child(&mut doc, el);
        assert_eq!(doc.to_component_tree().children[0].style.font_size, 24.0);

        el.set_attribute(&mut doc, "class", "replacement");
        assert!(!el.class_list_contains(&doc, "title"));
        assert!(el.class_list_contains(&doc, "replacement"));
        el.remove_attribute(&mut doc, "class");
        assert_eq!(el.get_attribute(&doc, "class"), None);
        assert!(!el.class_list_contains(&doc, "replacement"));
        crate::stylesheet::clear_rules();
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
    fn test_stylesheet_attribute_rule_applies_in_component_tree() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            ".message[data-role='user']",
            &[("color", "#ffffff"), ("background", "#176bd1")],
        );

        let mut doc = Document::new();
        let el = doc.create_element("article");
        el.class_list_add(&mut doc, "message");
        el.set_attribute(&mut doc, "data-role", "user");
        doc.body().append_child(&mut doc, el);

        let tree = doc.to_component_tree();
        let style = &tree.children[0].style;
        assert_eq!(style.color.r, 255);
        assert_eq!(style.background.r, 23);
        assert_eq!(style.background.g, 107);
        assert_eq!(style.background.b, 209);
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
    fn test_password_input_becomes_secure_text_input() {
        let mut doc = Document::new();
        let input = doc.create_element("input");
        input.set_attribute(&mut doc, "type", "password");
        input.set_attribute(&mut doc, "value", "demo");
        doc.body().append_child(&mut doc, input);

        let tree = doc.to_component_tree();
        assert!(matches!(
            &tree.children[0].kind,
            w3cos_std::ComponentKind::TextInput {
                value,
                secure: true,
                ..
            } if value == "demo"
        ));
    }

    #[test]
    fn test_file_input_is_clickable_without_text_keyboard_semantics() {
        let mut doc = Document::new();
        let input = doc.create_element("input");
        input.set_attribute(&mut doc, "type", "file");
        doc.body().append_child(&mut doc, input);

        let tree = doc.to_component_tree();
        assert!(!matches!(
            tree.children[0].kind,
            w3cos_std::ComponentKind::TextInput { .. }
        ));
        assert!(matches!(
            tree.children[0].on_click,
            w3cos_std::EventAction::NativeHost {
                id,
                click: true,
                input: false,
                focus: false,
                keyboard: false,
                ..
            } if id == input.id.as_u32() as u64
        ));
    }

    #[test]
    fn test_select_renders_only_its_current_option() {
        let mut doc = Document::new();
        let select = doc.create_element("select");
        select.set_attribute(&mut doc, "value", "+86");
        for value in ["+86", "+852", "+853"] {
            let option = doc.create_element("option");
            option.set_attribute(&mut doc, "value", value);
            let label = doc.create_text_node(value);
            option.append_child(&mut doc, label);
            select.append_child(&mut doc, option);
        }
        doc.body().append_child(&mut doc, select);

        let tree = doc.to_component_tree();
        assert!(matches!(
            &tree.children[0].kind,
            w3cos_std::ComponentKind::Button { label } if label == "+86"
        ));
        assert!(tree.children[0].children.is_empty());
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
    fn non_leaf_button_paints_its_text_child_once() {
        let mut doc = Document::new();
        let button = doc.create_element("button");
        let label = doc.create_element("span");
        let label_text = doc.create_text_node("Enter workspace");
        label.append_child(&mut doc, label_text);
        let icon = doc.create_element("span");
        button.append_child(&mut doc, label);
        button.append_child(&mut doc, icon);
        doc.body().append_child(&mut doc, button);

        let component = doc.to_component_subtree(button.id);
        assert!(matches!(
            component.kind,
            w3cos_std::ComponentKind::Button { ref label } if label.is_empty()
        ));
        assert_eq!(component.children.len(), 2);
        assert!(matches!(
            component.children[0].kind,
            w3cos_std::ComponentKind::Text { ref content } if content == "Enter workspace"
        ));
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
    fn test_computed_text_style_inherits_through_nested_elements() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            ".list",
            &[
                ("font-size", "13px"),
                ("line-height", "1.4"),
                ("color", "#123456"),
            ],
        );
        crate::stylesheet::register_rule(".explicit", &[("font-size", "11px")]);

        let mut doc = Document::new();
        let list = doc.create_element("ul");
        list.set_class_name(&mut doc, "list");
        let item = doc.create_element("li");
        let inherited = doc.create_element("span");
        let explicit = doc.create_element("span");
        explicit.set_class_name(&mut doc, "explicit");
        item.append_child(&mut doc, inherited);
        item.append_child(&mut doc, explicit);
        list.append_child(&mut doc, item);
        doc.body().append_child(&mut doc, list);

        let inherited_style = doc.computed_style_for(inherited.id);
        assert_eq!(inherited_style.font_size, 13.0);
        assert_eq!(inherited_style.line_height, 1.4);
        assert_eq!(inherited_style.color, w3cos_std::Color::from_hex("#123456"));
        assert_eq!(doc.computed_style_for(explicit.id).font_size, 11.0);
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn test_scoped_inline_custom_properties_resolve_in_descendant_rules() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            ".card",
            &[
                ("padding", "var(--card-padding)"),
                (
                    "border",
                    "var(--card-border-width) solid var(--card-border-color)",
                ),
                ("background", "var(--card-background)"),
            ],
        );

        let mut doc = Document::new();
        let surface = doc.create_element("section");
        surface
            .style_mut(&mut doc)
            .set_property("--card-padding", "16px");
        surface
            .style_mut(&mut doc)
            .set_property("--card-border-width", "1px");
        surface
            .style_mut(&mut doc)
            .set_property("--card-border-color", "#d7e0ee");
        surface
            .style_mut(&mut doc)
            .set_property("--card-background", "#ffffff");
        let card = doc.create_element("div");
        card.set_class_name(&mut doc, "card");
        surface.append_child(&mut doc, card);
        doc.body().append_child(&mut doc, surface);

        let style = doc.computed_style_for(card.id);
        assert_eq!(style.padding.left, w3cos_std::style::Spacing::Px(16.0));
        assert_eq!(style.border_width, 1.0);
        assert_eq!(style.border_color, w3cos_std::Color::from_hex("#d7e0ee"));
        assert_eq!(style.background, w3cos_std::Color::from_hex("#ffffff"));
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn test_current_color_background_uses_inherited_text_color() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(".status", &[("color", "#1677ff")]);
        crate::stylesheet::register_rule(
            ".dot",
            &[
                ("width", "8px"),
                ("height", "8px"),
                ("background", "currentColor"),
            ],
        );

        let mut doc = Document::new();
        let status = doc.create_element("div");
        status.set_class_name(&mut doc, "status");
        let dot = doc.create_element("span");
        dot.set_class_name(&mut doc, "dot");
        status.append_child(&mut doc, dot);
        doc.body().append_child(&mut doc, status);

        let style = doc.computed_style_for(dot.id);
        assert_eq!(style.color, w3cos_std::Color::from_hex("#1677ff"));
        assert_eq!(style.background, style.color);
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
        assert!(matches!(style.inner.resolved_overflow_y(), Overflow::Auto));
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
