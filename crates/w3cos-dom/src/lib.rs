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
    fn display_contents_promotes_children_into_the_parent_component() {
        let mut doc = Document::new();
        let parent = doc.create_element("div");
        parent.style_mut(&mut doc).set_property("display", "flex");
        parent.style_mut(&mut doc).set_property("width", "100px");
        let contents = doc.create_element("div");
        contents
            .style_mut(&mut doc)
            .set_property("display", "contents");
        let child = doc.create_element("div");
        child.style_mut(&mut doc).set_property("display", "inline");
        child.style_mut(&mut doc).set_property("height", "100px");
        child.style_mut(&mut doc).set_property("flex-grow", "1");
        contents.append_child(&mut doc, child);
        parent.append_child(&mut doc, contents);
        doc.body().append_child(&mut doc, parent);

        let tree = doc.to_component_tree();
        let parent = &tree.children[0];
        assert_eq!(parent.children.len(), 1);
        assert_eq!(parent.children[0].style.flex_grow, 1.0);
        assert_eq!(parent.children[0].style.height, Dimension::Px(100.0));
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
    fn block_flow_drops_inter_element_whitespace_but_keeps_inline_spacing() {
        use w3cos_std::ComponentKind;

        let mut doc = Document::new();
        let paragraph = doc.create_element("p");
        let paragraph_text = doc.create_text_node("first");
        doc.append_child(paragraph.id, paragraph_text.id);
        doc.append_child(doc.body().id, paragraph.id);
        let block_space = doc.create_text_node("\n    ");
        doc.append_child(doc.body().id, block_space.id);
        let div = doc.create_element("div");
        let div_text = doc.create_text_node("second");
        doc.append_child(div.id, div_text.id);
        doc.append_child(doc.body().id, div.id);

        let tree = doc.to_component_tree();
        assert_eq!(tree.children.len(), 2);

        let mut inline_doc = Document::new();
        for (index, text) in ["left", "right"].into_iter().enumerate() {
            if index > 0 {
                let spacing = inline_doc.create_text_node(" ");
                inline_doc.append_child(inline_doc.body().id, spacing.id);
            }
            let span = inline_doc.create_element("span");
            let text = inline_doc.create_text_node(text);
            inline_doc.append_child(span.id, text.id);
            inline_doc.append_child(inline_doc.body().id, span.id);
        }
        let inline_tree = inline_doc.to_component_tree();
        assert_eq!(inline_tree.children.len(), 1);
        assert!(matches!(
            &inline_tree.children[0].kind,
            ComponentKind::Text { content } if content == "left right"
        ));

        let mut nested_doc = Document::new();
        let span = nested_doc.create_element("span");
        for content in ["\n  ", "content", "  \n"] {
            let text = nested_doc.create_text_node(content);
            nested_doc.append_child(span.id, text.id);
        }
        nested_doc.append_child(nested_doc.body().id, span.id);
        let nested_tree = nested_doc.to_component_tree();
        assert!(matches!(
            &nested_tree.children[0].kind,
            ComponentKind::Text { content } if content == "content"
        ));

        let mut collapsed_doc = Document::new();
        let indented = collapsed_doc.create_text_node("\n    Filler   Text\n  ");
        collapsed_doc.append_child(collapsed_doc.body().id, indented.id);
        let collapsed_tree = collapsed_doc.to_component_tree();
        assert!(matches!(
            &collapsed_tree.children[0].kind,
            ComponentKind::Text { content } if content == "Filler Text"
        ));
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
            Some("#ordinary")
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
        assert_eq!(el.get_attribute(&doc, "xlink:href"), Some("#renamed"));
        assert_eq!(el.get_attribute(&doc, "legacy:href"), None);
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
        assert_eq!(cloned.get_attribute(&doc, "xlink:href"), None);
    }

    #[test]
    fn test_remove_attribute_only_removes_first_matching_qualified_name() {
        let mut doc = Document::new();
        let el = doc.create_element("p");
        el.set_attribute(&mut doc, "x", "first");
        el.set_attribute_ns(&mut doc, Some("foo"), "x", None, "x", "second");

        assert_eq!(doc.get_node(el.id).attributes.len(), 2);
        assert_eq!(el.get_attribute(&doc, "x"), Some("first"));
        assert_eq!(el.get_attribute_ns(&doc, None, "x"), Some("first"));
        assert_eq!(el.get_attribute_ns(&doc, Some("foo"), "x"), Some("second"));

        el.remove_attribute(&mut doc, "x");

        assert_eq!(doc.get_node(el.id).attributes.len(), 1);
        assert_eq!(el.get_attribute(&doc, "x"), Some("second"));
        assert_eq!(el.get_attribute_ns(&doc, None, "x"), None);
        assert_eq!(el.get_attribute_ns(&doc, Some("foo"), "x"), Some("second"));

        let namespaced = doc.create_element("p");
        namespaced.set_attribute_ns(&mut doc, Some("foo"), "x", None, "x", "first");
        namespaced.set_attribute_ns(&mut doc, Some("foo2"), "x", None, "x", "second");
        namespaced.remove_attribute(&mut doc, "x");

        assert_eq!(namespaced.get_attribute(&doc, "x"), Some("second"));
        assert_eq!(namespaced.get_attribute_ns(&doc, Some("foo"), "x"), None);
        assert_eq!(
            namespaced.get_attribute_ns(&doc, Some("foo2"), "x"),
            Some("second")
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
    fn generated_pseudo_content_lowers_strings_and_attributes_in_tree_order() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            "#item:before",
            &[
                ("content", "'before ' attr(data-label)"),
                ("color", "green"),
            ],
        );
        crate::stylesheet::register_rule("#item::after", &[("content", "' after'")]);

        let mut doc = Document::new();
        let item = doc.create_element("div");
        item.set_attribute(&mut doc, "id", "item");
        item.set_attribute(&mut doc, "data-label", "value");
        let body_text = doc.create_text_node("body");
        item.append_child(&mut doc, body_text);
        doc.body().append_child(&mut doc, item);

        let tree = doc.to_component_tree();
        let children = &tree.children[0].children;
        assert!(matches!(
            &children[0].kind,
            w3cos_std::ComponentKind::Text { content } if content == "before value"
        ));
        assert_eq!(children[0].style.color, w3cos_std::Color::rgb(0, 128, 0));
        assert!(matches!(
            &children[1].kind,
            w3cos_std::ComponentKind::Text { content } if content == "body"
        ));
        assert!(matches!(
            &children[2].kind,
            w3cos_std::ComponentKind::Text { content } if content == " after"
        ));
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn visible_head_content_promotes_the_render_root_to_the_document_element() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("head", &[("display", "block")]);
        crate::stylesheet::register_rule("head::before", &[("content", "'HEAD'")]);

        let mut doc = Document::new();
        let html = doc.create_element("html");
        let head = doc.create_element("head");
        let body = doc.create_element("body");
        html.append_child(&mut doc, head);
        html.append_child(&mut doc, body);
        doc.body().append_child(&mut doc, html);
        doc.set_render_body(body.id);

        let tree = doc.to_component_tree();
        fn contains_head(component: &w3cos_std::Component) -> bool {
            matches!(
                &component.kind,
                w3cos_std::ComponentKind::Text { content } if content == "HEAD"
            ) || component.children.iter().any(contains_head)
        }
        assert!(contains_head(&tree));
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn body_fast_path_retains_the_browser_default_margin() {
        let doc = Document::new();
        assert_eq!(
            doc.to_component_tree().style.margin,
            w3cos_std::style::Edges::all(8.0)
        );
    }

    #[test]
    fn document_element_is_the_stable_root_and_edge_whitespace_does_not_add_a_line() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("html::before", &[("content", "' '")]);

        let mut doc = Document::new();
        let html = doc.create_element("html");
        let head = doc.create_element("head");
        let body = doc.create_element("body");
        let text = doc.create_text_node("body");
        body.append_child(&mut doc, text);
        html.append_child(&mut doc, head);
        html.append_child(&mut doc, body);
        doc.body().append_child(&mut doc, html);
        doc.set_render_body(body.id);

        let tree = doc.to_component_tree();
        let visible_children = tree
            .children
            .iter()
            .filter(|child| child.style.display != w3cos_std::style::Display::None)
            .collect::<Vec<_>>();
        assert_eq!(visible_children.len(), 1);
        assert_eq!(
            visible_children[0].style.margin,
            w3cos_std::style::Edges::all(8.0)
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn final_none_content_declaration_suppresses_an_earlier_generated_value() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            "#item::before",
            &[("content", "'FAIL'"), ("content", "none")],
        );

        let mut doc = Document::new();
        let item = doc.create_element("div");
        item.set_attribute(&mut doc, "id", "item");
        doc.body().append_child(&mut doc, item);

        let tree = doc.to_component_tree();
        assert!(tree.children[0].children.is_empty());
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn generated_attr_name_is_ascii_case_insensitive_for_html_elements() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("#item::before", &[("content", "attr(Title)")]);

        let mut doc = Document::new();
        let item = doc.create_element("div");
        item.set_attribute(&mut doc, "id", "item");
        item.set_attribute(&mut doc, "title", "PASS");
        doc.body().append_child(&mut doc, item);

        let tree = doc.to_component_tree();
        fn contains_pass(component: &w3cos_std::Component) -> bool {
            matches!(
                &component.kind,
                w3cos_std::ComponentKind::Text { content } if content == "PASS"
            ) || component.children.iter().any(contains_pass)
        }
        assert!(contains_pass(&tree.children[0]), "{tree:#?}");
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn block_child_of_inline_host_uses_the_surrounding_block_width() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("div", &[("border", "2px solid black")]);

        let mut doc = Document::new();
        let link = doc.create_element("a");
        let block = doc.create_element("div");
        let text = doc.create_text_node("#");
        block.append_child(&mut doc, text);
        link.append_child(&mut doc, block);
        doc.body().append_child(&mut doc, link);

        let tree = doc.to_component_tree();
        let link = &tree.children[0];
        assert_eq!(link.style.display, w3cos_std::style::Display::Block);
        assert_eq!(link.children.len(), 1);
        assert_eq!(
            link.children[0].style.display,
            w3cos_std::style::Display::Block
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn plain_anchor_text_does_not_use_button_centering() {
        crate::stylesheet::clear_rules();
        let mut doc = Document::new();
        let link = doc.create_element("a");
        let text = doc.create_text_node("PASS PASS");
        link.append_child(&mut doc, text);
        doc.body().append_child(&mut doc, link);

        let tree = doc.to_component_tree();
        assert!(matches!(
            &tree.children[0].kind,
            w3cos_std::ComponentKind::Text { content } if content == "PASS PASS"
        ));
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn legacy_body_text_attribute_supplies_the_inherited_text_color() {
        crate::stylesheet::clear_rules();
        let mut doc = Document::new();
        let body = doc.create_element("body");
        body.set_attribute(&mut doc, "text", "green");
        doc.body().append_child(&mut doc, body);

        assert_eq!(
            doc.computed_style_for(body.id).color,
            w3cos_std::Color::rgb(0, 128, 0)
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn legacy_body_bgcolor_attribute_supplies_the_background() {
        crate::stylesheet::clear_rules();
        let mut doc = Document::new();
        let body = doc.create_element("body");
        body.set_attribute(&mut doc, "bgcolor", "#ffff00");
        doc.body().append_child(&mut doc, body);

        assert_eq!(
            doc.computed_style_for(body.id).background,
            w3cos_std::Color::rgb(255, 255, 0)
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn missing_generated_attr_does_not_create_an_anonymous_line_box() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            "#item::before",
            &[("content", "attr(missing)"), ("background", "red")],
        );

        let mut doc = Document::new();
        let item = doc.create_element("div");
        item.set_attribute(&mut doc, "id", "item");
        doc.body().append_child(&mut doc, item);

        let tree = doc.to_component_tree();
        assert!(tree.children[0].children.is_empty());
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn explicit_empty_generated_string_keeps_a_shrink_to_fit_leaf() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            "#item::before",
            &[("content", "''"), ("background", "red")],
        );

        let mut doc = Document::new();
        let item = doc.create_element("div");
        item.set_attribute(&mut doc, "id", "item");
        doc.body().append_child(&mut doc, item);

        let tree = doc.to_component_tree();
        let generated = &tree.children[0].children[0];
        assert!(matches!(
            &generated.kind,
            w3cos_std::ComponentKind::Text { content } if content.is_empty()
        ));
        assert_eq!(
            generated.style.display,
            w3cos_std::style::Display::InlineBlock
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn generated_pseudo_content_tracks_scoped_sibling_counters() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("#list", &[("counter-reset", "item")]);
        crate::stylesheet::register_rule("#list span", &[("counter-increment", "item")]);
        crate::stylesheet::register_rule("#list span:before", &[("content", "counter(item) '.'")]);

        let mut doc = Document::new();
        let list = doc.create_element("div");
        list.set_attribute(&mut doc, "id", "list");
        let first = doc.create_element("span");
        let second = doc.create_element("span");
        list.append_child(&mut doc, first);
        list.append_child(&mut doc, second);
        doc.body().append_child(&mut doc, list);

        fn collect_text(component: &w3cos_std::Component, output: &mut String) {
            if let w3cos_std::ComponentKind::Text { content } = &component.kind {
                output.push_str(content);
            }
            for child in &component.children {
                collect_text(child, output);
            }
        }
        let mut text = String::new();
        collect_text(&doc.to_component_tree(), &mut text);
        assert_eq!(text, "1.2.");
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn generated_counter_accepts_a_multibyte_identifier() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("#item", &[("counter-reset", "计数 7")]);
        crate::stylesheet::register_rule("#item::before", &[("content", "counter(计数)")]);

        let mut doc = Document::new();
        let item = doc.create_element("span");
        item.set_attribute(&mut doc, "id", "item");
        doc.body().append_child(&mut doc, item);

        let tree = doc.to_component_tree();
        assert!(matches!(
            &tree.children[0].kind,
            w3cos_std::ComponentKind::Text { content } if content == "7"
        ));
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn before_counter_scope_is_visible_to_generated_content_in_descendants() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            "div::before",
            &[
                ("content", "counters(item, '.')"),
                ("counter-reset", "item"),
            ],
        );

        let mut doc = Document::new();
        let outer = doc.create_element("div");
        let inner = doc.create_element("div");
        outer.append_child(&mut doc, inner);
        doc.body().append_child(&mut doc, outer);

        fn collect_text(component: &w3cos_std::Component, output: &mut Vec<String>) {
            if let w3cos_std::ComponentKind::Text { content } = &component.kind
                && !content.trim().is_empty()
            {
                output.push(content.clone());
            }
            for child in &component.children {
                collect_text(child, output);
            }
        }

        let tree = doc.to_component_tree();
        let mut text = Vec::new();
        collect_text(&tree, &mut text);
        assert_eq!(text, ["0", "0.0"]);
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn child_reset_masks_a_retained_before_counter_scope_before_descendants() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("#root", &[("counter-reset", "item 4")]);
        crate::stylesheet::register_rule(
            "#root::before",
            &[
                ("content", "' '"),
                ("counter-reset", "item 9999"),
                ("counter-increment", "item 9999"),
            ],
        );
        crate::stylesheet::register_rule("#child", &[("counter-reset", "item 8")]);
        crate::stylesheet::register_rule("#leaf::before", &[("content", "counters(item, '.')")]);

        let mut doc = Document::new();
        let root = doc.create_element("div");
        root.set_attribute(&mut doc, "id", "root");
        let child = doc.create_element("div");
        child.set_attribute(&mut doc, "id", "child");
        let leaf = doc.create_element("span");
        leaf.set_attribute(&mut doc, "id", "leaf");
        child.append_child(&mut doc, leaf);
        root.append_child(&mut doc, child);
        doc.body().append_child(&mut doc, root);

        fn collect_text(component: &w3cos_std::Component, output: &mut String) {
            if let w3cos_std::ComponentKind::Text { content } = &component.kind {
                output.push_str(content);
            }
            for child in &component.children {
                collect_text(child, output);
            }
        }

        let mut text = String::new();
        collect_text(&doc.to_component_tree(), &mut text);
        assert_eq!(text.trim(), "4.8");
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn sibling_counter_reset_remains_in_scope_for_following_siblings() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("#list", &[("counter-reset", "item")]);
        crate::stylesheet::register_rule("#list span", &[("counter-increment", "item")]);
        crate::stylesheet::register_rule("#list span::before", &[("content", "counter(item)")]);

        let mut doc = Document::new();
        let list = doc.create_element("div");
        list.set_attribute(&mut doc, "id", "list");
        for reset in [None, Some("item 48"), None] {
            let span = doc.create_element("span");
            if let Some(reset) = reset {
                span.style_mut(&mut doc)
                    .set_property("counter-reset", reset);
            }
            list.append_child(&mut doc, span);
        }
        doc.body().append_child(&mut doc, list);

        fn collect_text(component: &w3cos_std::Component, output: &mut String) {
            if let w3cos_std::ComponentKind::Text { content } = &component.kind {
                output.push_str(content);
            }
            for child in &component.children {
                collect_text(child, output);
            }
        }
        let mut text = String::new();
        collect_text(&doc.to_component_tree(), &mut text);
        assert_eq!(text, "14950");
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn hidden_elements_and_ungenerated_pseudos_do_not_modify_counters() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("#list", &[("counter-reset", "item")]);
        crate::stylesheet::register_rule(
            "#hidden",
            &[("display", "none"), ("counter-increment", "item")],
        );
        crate::stylesheet::register_rule("#inert::before", &[("counter-increment", "item")]);
        crate::stylesheet::register_rule("#result::before", &[("content", "counter(item)")]);

        let mut doc = Document::new();
        let list = doc.create_element("div");
        list.set_attribute(&mut doc, "id", "list");
        for id in ["hidden", "inert", "result"] {
            let span = doc.create_element("span");
            span.set_attribute(&mut doc, "id", id);
            list.append_child(&mut doc, span);
        }
        doc.body().append_child(&mut doc, list);

        let tree = doc.to_component_tree();
        assert!(matches!(
            &tree.children[0].children[2].kind,
            w3cos_std::ComponentKind::Text { content } if content == "0"
        ));
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn generated_text_preserves_a_sized_and_bordered_principal_box() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            "#item",
            &[("height", "30px"), ("border", "2px solid black")],
        );
        crate::stylesheet::register_rule("#item::before", &[("content", "'0'")]);

        let mut doc = Document::new();
        let item = doc.create_element("div");
        item.set_attribute(&mut doc, "id", "item");
        doc.body().append_child(&mut doc, item);

        let tree = doc.to_component_tree();
        let principal = &tree.children[0];
        assert!(
            !matches!(principal.kind, w3cos_std::ComponentKind::Text { .. }),
            "the principal box must not collapse into generated text: {:?}",
            principal.kind
        );
        assert_eq!(principal.style.height, Dimension::Px(30.0));
        assert_eq!(principal.style.border_width, 2.0);
        assert!(matches!(
            &principal.children[0].kind,
            w3cos_std::ComponentKind::Text { content } if content == "0"
        ));
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn generated_text_preserves_an_unsized_block_principal_box() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("#item", &[("white-space", "pre")]);
        crate::stylesheet::register_rule("#item::before", &[("content", "'first\\A'")]);
        crate::stylesheet::register_rule("#item::after", &[("content", "'second'")]);

        let mut doc = Document::new();
        let item = doc.create_element("div");
        item.set_attribute(&mut doc, "id", "item");
        doc.body().append_child(&mut doc, item);

        let tree = doc.to_component_tree();
        let principal = &tree.children[0];
        assert_eq!(principal.style.display, w3cos_std::style::Display::Block);
        assert!(
            !matches!(principal.kind, w3cos_std::ComponentKind::Text { .. }),
            "a block formatting-context principal must survive generated text lowering"
        );
        assert!(
            principal.children.iter().any(|child| {
                matches!(
                    &child.kind,
                    w3cos_std::ComponentKind::Text { content }
                        if content.contains("first") && content.contains("second")
                )
            }),
            "generated inline runs should remain inside the block principal: {principal:#?}"
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn discarded_whitespace_does_not_expand_a_bordered_before_pseudo() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            "body::before",
            &[("content", "'Before'"), ("border", "inherit")],
        );

        let mut doc = Document::new();
        let body = doc.body();
        body.set_attribute(&mut doc, "style", "border: 2px solid green");
        let whitespace = doc.create_text_node("\n\n");
        body.append_child(&mut doc, whitespace);

        let tree = doc.to_component_tree();
        let before = tree.children.first().expect("generated before component");
        assert!(matches!(
            &before.kind,
            w3cos_std::ComponentKind::Text { content } if content == "Before"
        ));
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn whitespace_only_source_separates_inline_generated_pseudos() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("body::before", &[("content", "'before'")]);
        crate::stylesheet::register_rule("body::after", &[("content", "'after!'")]);

        let mut doc = Document::new();
        let body = doc.body();
        let whitespace = doc.create_text_node("\n\n");
        body.append_child(&mut doc, whitespace);

        let tree = doc.to_component_tree();
        assert!(
            matches!(
                &tree.children[0].kind,
                w3cos_std::ComponentKind::Text { content } if content == "before after!"
            ),
            "{tree:#?}"
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn generated_content_url_lowers_to_an_image_component() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            "#item::before",
            &[("content", "url('../support/green_box.png')")],
        );

        let mut doc = Document::new();
        let item = doc.create_element("div");
        item.set_attribute(&mut doc, "id", "item");
        doc.body().append_child(&mut doc, item);

        let tree = doc.to_component_tree();
        let principal = &tree.children[0];
        assert!(principal.children.iter().any(|child| {
            matches!(
                &child.kind,
                w3cos_std::ComponentKind::Image { src }
                    if src == "../support/green_box.png"
            )
        }));
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn mixed_generated_content_preserves_text_image_text_order() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            "#item::before",
            &[(
                "content",
                "'A' url('../support/green_box.png') 'B' attr(data-label)",
            )],
        );
        crate::stylesheet::register_rule(
            "#item::after",
            &[("content", "'D' url('../support/green_box.png') 'E'")],
        );

        let mut doc = Document::new();
        let item = doc.create_element("div");
        item.set_attribute(&mut doc, "id", "item");
        item.set_attribute(&mut doc, "data-label", "C");
        let authored = doc.create_text_node("Inner");
        item.append_child(&mut doc, authored);
        doc.body().append_child(&mut doc, item);

        let tree = doc.to_component_tree();
        let principal = &tree.children[0];
        assert_eq!(
            principal.style.display,
            w3cos_std::style::Display::Flex,
            "a block containing inline mixed content needs one anonymous line row"
        );
        let generated = &principal.children[0];
        assert_eq!(
            generated.style.display,
            w3cos_std::style::Display::Inline,
            "the authored pseudo display belongs to its principal box"
        );
        let items = &generated.children;
        assert!(matches!(
            &items[0].kind,
            w3cos_std::ComponentKind::Text { content } if content == "A"
        ));
        assert!(matches!(
            &items[1].kind,
            w3cos_std::ComponentKind::Image { src }
                if src == "../support/green_box.png"
        ));
        assert!(matches!(
            &items[2].kind,
            w3cos_std::ComponentKind::Text { content } if content == "BC"
        ));
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn authored_inline_block_with_an_image_stays_inline_level() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("#part", &[("display", "inline-block")]);

        let mut doc = Document::new();
        let host = doc.create_element("div");
        let part = doc.create_element("span");
        part.set_attribute(&mut doc, "id", "part");
        let before = doc.create_text_node("Before");
        let image = doc.create_element("img");
        image.set_attribute(&mut doc, "src", "square.png");
        let after = doc.create_text_node("After");
        part.append_child(&mut doc, before);
        part.append_child(&mut doc, image);
        part.append_child(&mut doc, after);
        host.append_child(&mut doc, part);
        doc.body().append_child(&mut doc, host);

        let tree = doc.to_component_tree();
        let host = &tree.children[0];
        assert_eq!(
            host.children[0].style.display,
            w3cos_std::style::Display::InlineFlex
        );
        assert!(host.children[0].children.iter().any(|component| {
            matches!(
                &component.kind,
                w3cos_std::ComponentKind::Image { src } if src == "square.png"
            )
        }));
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn authored_inline_table_stays_in_the_parent_inline_context() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("#part", &[("display", "inline-table")]);

        let mut doc = Document::new();
        let host = doc.create_element("div");
        let before = doc.create_text_node("Before");
        let part = doc.create_element("span");
        part.set_attribute(&mut doc, "id", "part");
        let part_before = doc.create_text_node("Part");
        let image = doc.create_element("img");
        image.set_attribute(&mut doc, "src", "square.png");
        let part_after = doc.create_text_node("End");
        part.append_child(&mut doc, part_before);
        part.append_child(&mut doc, image);
        part.append_child(&mut doc, part_after);
        let after = doc.create_text_node("After");
        host.append_child(&mut doc, before);
        host.append_child(&mut doc, part);
        host.append_child(&mut doc, after);
        doc.body().append_child(&mut doc, host);

        let tree = doc.to_component_tree();
        let host = &tree.children[0];
        assert_eq!(host.style.display, w3cos_std::style::Display::Flex);
        assert_eq!(
            host.children[1].style.display,
            w3cos_std::style::Display::InlineFlex
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn positioned_inline_replaced_content_stays_in_the_parent_line() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("#part", &[("position", "relative"), ("top", "-10px")]);

        let mut doc = Document::new();
        let host = doc.create_element("div");
        let before = doc.create_text_node("Begin ");
        let part = doc.create_element("span");
        part.set_attribute(&mut doc, "id", "part");
        let part_before = doc.create_text_node("Before");
        let image = doc.create_element("img");
        image.set_attribute(&mut doc, "src", "square.png");
        let part_after = doc.create_text_node("After");
        part.append_child(&mut doc, part_before);
        part.append_child(&mut doc, image);
        part.append_child(&mut doc, part_after);
        let after = doc.create_text_node(" End");
        host.append_child(&mut doc, before);
        host.append_child(&mut doc, part);
        host.append_child(&mut doc, after);
        doc.body().append_child(&mut doc, host);

        let tree = doc.to_component_tree();
        let host = &tree.children[0];
        assert_eq!(host.style.display, w3cos_std::style::Display::Flex);
        assert_eq!(
            host.children[1].style.position,
            w3cos_std::style::Position::Relative
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn authored_table_cell_with_mixed_content_keeps_its_table_principal() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("#cell", &[("display", "table-cell")]);

        let mut doc = Document::new();
        let cell = doc.create_element("span");
        cell.set_attribute(&mut doc, "id", "cell");
        let before = doc.create_text_node("Before");
        let image = doc.create_element("img");
        image.set_attribute(&mut doc, "src", "square.png");
        let after = doc.create_text_node("After");
        cell.append_child(&mut doc, before);
        cell.append_child(&mut doc, image);
        cell.append_child(&mut doc, after);
        doc.body().append_child(&mut doc, cell);

        let tree = doc.to_component_tree();
        let table = &tree.children[0];
        assert_eq!(table.style.display, w3cos_std::style::Display::Table);
        let row = &table.children[0];
        assert_eq!(row.style.display, w3cos_std::style::Display::TableRow);
        let cell = &row.children[0];
        assert_eq!(cell.style.display, w3cos_std::style::Display::TableCell);
        assert_eq!(cell.children.len(), 1);
        assert!(matches!(
            cell.children[0].kind,
            w3cos_std::ComponentKind::Row
        ));
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn misparented_table_cells_form_one_anonymous_row_at_the_tallest_cell_height() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(".cell", &[("display", "table-cell")]);
        crate::stylesheet::register_rule("#first", &[("height", "100px")]);
        crate::stylesheet::register_rule("#second", &[("height", "80px")]);
        crate::stylesheet::register_rule("#third", &[("height", "60px")]);

        let mut doc = Document::new();
        let host = doc.create_element("div");
        for (id, label) in [("first", "Before"), ("second", "Inner"), ("third", "After")] {
            let cell = doc.create_element("div");
            cell.class_list_add(&mut doc, "cell");
            cell.set_attribute(&mut doc, "id", id);
            let text = doc.create_text_node(label);
            cell.append_child(&mut doc, text);
            host.append_child(&mut doc, cell);
        }
        doc.body().append_child(&mut doc, host);

        let tree = doc.to_component_tree();
        let table = &tree.children[0].children[0];
        assert_eq!(table.style.display, w3cos_std::style::Display::Table);
        assert_eq!(table.style.height, w3cos_std::style::Dimension::Px(100.0));
        let row = &table.children[0];
        assert_eq!(row.style.display, w3cos_std::style::Display::TableRow);
        assert_eq!(row.style.height, w3cos_std::style::Dimension::Px(100.0));
        assert_eq!(row.children.len(), 3);
        assert!(row.children.iter().all(|cell| {
            cell.style.display == w3cos_std::style::Display::TableCell
                && cell.style.height == w3cos_std::style::Dimension::Auto
        }));
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn table_row_keeps_direct_table_cells_without_an_extra_table_wrapper() {
        crate::stylesheet::clear_rules();

        let mut doc = Document::new();
        let table = doc.create_element("table");
        let body = doc.create_element("tbody");
        let row = doc.create_element("tr");
        for label in ["left", "right"] {
            let cell = doc.create_element("td");
            let text = doc.create_text_node(label);
            cell.append_child(&mut doc, text);
            row.append_child(&mut doc, cell);
        }
        body.append_child(&mut doc, row);
        table.append_child(&mut doc, body);
        doc.body().append_child(&mut doc, table);

        let tree = doc.to_component_tree();
        let row = &tree.children[0].children[0].children[0];
        assert_eq!(row.style.display, w3cos_std::style::Display::TableRow);
        assert_eq!(row.children.len(), 2);
        assert!(
            row.children
                .iter()
                .all(|cell| cell.style.display == w3cos_std::style::Display::TableCell)
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn internal_table_boxes_use_css_table_edge_rules() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            "div",
            &[
                ("margin", "5px"),
                ("padding", "7px"),
                ("border", "2px solid green"),
            ],
        );
        crate::stylesheet::register_rule("#group", &[("display", "table-row-group")]);
        crate::stylesheet::register_rule("#row", &[("display", "table-row")]);
        crate::stylesheet::register_rule("#cell", &[("display", "table-cell")]);

        let mut doc = Document::new();
        let table = doc.create_element("table");
        let group = doc.create_element("div");
        group.set_attribute(&mut doc, "id", "group");
        let row = doc.create_element("div");
        row.set_attribute(&mut doc, "id", "row");
        let cell = doc.create_element("div");
        cell.set_attribute(&mut doc, "id", "cell");
        let text = doc.create_text_node("cell");
        cell.append_child(&mut doc, text);
        row.append_child(&mut doc, cell);
        group.append_child(&mut doc, row);
        table.append_child(&mut doc, group);
        doc.body().append_child(&mut doc, table);

        let tree = doc.to_component_tree();
        let group = &tree.children[0].children[0];
        let row = &group.children[0];
        let cell = &row.children[0];
        for internal in [group, row] {
            assert_eq!(internal.style.margin, w3cos_std::style::Edges::ZERO);
            assert_eq!(internal.style.padding, w3cos_std::style::Edges::ZERO);
            assert_eq!(internal.style.border_width, 0.0);
        }
        assert_eq!(cell.style.margin, w3cos_std::style::Edges::ZERO);
        assert_eq!(cell.style.padding, w3cos_std::style::Edges::all(7.0));
        assert_eq!(cell.style.border_width, 2.0);
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn border_solid_uses_current_color_after_inheritance() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            "#cell",
            &[
                ("display", "table-cell"),
                ("color", "green"),
                ("border", "solid"),
            ],
        );

        let mut doc = Document::new();
        let cell = doc.create_element("div");
        cell.set_attribute(&mut doc, "id", "cell");
        doc.body().append_child(&mut doc, cell);

        let tree = doc.to_component_tree();
        let cell = &tree.children[0].children[0].children[0];
        assert_eq!(cell.style.border_width, 3.0);
        assert_eq!(
            cell.style.border_color,
            w3cos_std::color::Color::rgb(0, 128, 0)
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn generated_row_whitespace_establishes_an_empty_anonymous_cell() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("#row::before", &[("content", "' '")]);

        let mut doc = Document::new();
        let table = doc.create_element("table");
        let body = doc.create_element("tbody");
        let row = doc.create_element("tr");
        row.set_attribute(&mut doc, "id", "row");
        let cell = doc.create_element("td");
        row.append_child(&mut doc, cell);
        body.append_child(&mut doc, row);
        table.append_child(&mut doc, body);
        doc.body().append_child(&mut doc, table);

        let tree = doc.to_component_tree();
        let row = &tree.children[0].children[0].children[0];
        assert_eq!(row.children.len(), 2);
        assert_eq!(
            row.children[0].style.display,
            w3cos_std::style::Display::TableCell
        );
        assert!(row.children[0].children.is_empty());
        assert_eq!(
            row.children[1].style.display,
            w3cos_std::style::Display::TableCell
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn generated_row_inline_run_uses_one_anonymous_cell() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            "#body::before",
            &[
                (
                    "content",
                    "'one' url(support/square-outline-32x32.png) 'two'",
                ),
                ("display", "table-row"),
            ],
        );

        let mut doc = Document::new();
        let table = doc.create_element("table");
        let body = doc.create_element("tbody");
        body.set_attribute(&mut doc, "id", "body");
        let row = doc.create_element("tr");
        let cell = doc.create_element("td");
        row.append_child(&mut doc, cell);
        body.append_child(&mut doc, row);
        table.append_child(&mut doc, body);
        doc.body().append_child(&mut doc, table);

        let tree = doc.to_component_tree();
        let generated_row = &tree.children[0].children[0].children[0];
        assert_eq!(
            generated_row.style.display,
            w3cos_std::style::Display::TableRow
        );
        assert_eq!(generated_row.children.len(), 1);
        assert_eq!(
            generated_row.children[0].style.display,
            w3cos_std::style::Display::TableCell
        );
        assert_eq!(generated_row.children[0].children.len(), 1);
        let inline_run = &generated_row.children[0].children[0];
        assert_eq!(inline_run.style.display, w3cos_std::style::Display::Flex);
        assert_eq!(inline_run.children.len(), 3);
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn css_table_groups_use_header_body_footer_order() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            "#table::before",
            &[("content", "'footer'"), ("display", "table-footer-group")],
        );
        crate::stylesheet::register_rule(
            "#table::after",
            &[("content", "'header'"), ("display", "table-header-group")],
        );

        let mut doc = Document::new();
        let table = doc.create_element("table");
        table.set_attribute(&mut doc, "id", "table");
        let body = doc.create_element("tbody");
        table.append_child(&mut doc, body);
        doc.body().append_child(&mut doc, table);

        let tree = doc.to_component_tree();
        let table = &tree.children[0];
        assert_eq!(table.style.display, w3cos_std::style::Display::Table);
        assert_eq!(
            table
                .children
                .iter()
                .map(|component| component.style.display)
                .collect::<Vec<_>>(),
            [
                w3cos_std::style::Display::TableHeaderGroup,
                w3cos_std::style::Display::TableRowGroup,
                w3cos_std::style::Display::TableFooterGroup,
            ]
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn generated_before_and_after_share_one_line_inside_a_bordered_principal_box() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("#item", &[("border", "2px solid black")]);
        crate::stylesheet::register_rule("#item::before", &[("content", "'PASS '")]);
        crate::stylesheet::register_rule("#item::after", &[("content", "'PASS'")]);

        let mut doc = Document::new();
        let item = doc.create_element("div");
        item.set_attribute(&mut doc, "id", "item");
        doc.body().append_child(&mut doc, item);

        let tree = doc.to_component_tree();
        let principal = &tree.children[0];
        assert_eq!(principal.style.border_width, 2.0);
        assert_eq!(principal.children.len(), 1);
        assert!(matches!(
            &principal.children[0].kind,
            w3cos_std::ComponentKind::Text { content } if content == "PASS PASS"
        ));
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn positioned_inline_generated_and_authored_text_share_the_principal_text_box() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            "#item",
            &[("position", "absolute"), ("top", "0"), ("left", "0")],
        );
        crate::stylesheet::register_rule("#item::before", &[("content", "'TEST '")]);

        let mut doc = Document::new();
        let item = doc.create_element("span");
        item.set_attribute(&mut doc, "id", "item");
        let authored = doc.create_text_node("PASS");
        item.append_child(&mut doc, authored);
        doc.body().append_child(&mut doc, item);

        let tree = doc.to_component_tree();
        let principal = &tree.children[0];
        assert!(matches!(
            &principal.kind,
            w3cos_std::ComponentKind::Text { content } if content == "TEST PASS"
        ));
        assert!(principal.children.is_empty());
        assert_eq!(
            principal.style.position,
            w3cos_std::style::Position::Absolute
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn inherited_positioned_generated_text_stays_in_one_shaped_run() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            "div",
            &[
                ("position", "relative"),
                ("font", "3em sans-serif"),
                ("color", "red"),
            ],
        );
        crate::stylesheet::register_rule(
            "span",
            &[("position", "absolute"), ("top", "0"), ("left", "0")],
        );
        crate::stylesheet::register_rule(
            ".test::before",
            &[("content", "'TEST &#x46;&#x41;&#x49;&#x4c;'")],
        );

        let mut doc = Document::new();
        doc.set_html_document(false);
        let host = doc.create_element("div");
        let item = doc.create_element("span");
        item.set_class_name(&mut doc, "test");
        let authored = doc.create_text_node("ED");
        item.append_child(&mut doc, authored);
        host.append_child(&mut doc, item);
        doc.body().append_child(&mut doc, host);

        let tree = doc.to_component_tree();
        let principal = &tree.children[0].children[0];
        assert!(matches!(
            &principal.kind,
            w3cos_std::ComponentKind::Text { content }
                if content == "TEST &#x46;&#x41;&#x49;&#x4c;ED"
        ));
        assert!(principal.children.is_empty());
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn collapsed_generated_inline_text_keeps_its_pseudo_paint_and_whitespace_style() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("span", &[("color", "red")]);
        crate::stylesheet::register_rule(
            "span::before",
            &[
                ("content", "'Test\\Apasses'"),
                ("white-space", "nowrap"),
                ("background", "green"),
                ("color", "white"),
            ],
        );

        let mut doc = Document::new();
        let item = doc.create_element("span");
        doc.body().append_child(&mut doc, item);

        let tree = doc.to_component_tree();
        let principal = &tree.children[0];
        assert!(matches!(
            &principal.kind,
            w3cos_std::ComponentKind::Text { .. }
        ));
        assert_eq!(
            principal.style.white_space,
            w3cos_std::style::WhiteSpace::NoWrap
        );
        assert_eq!(principal.style.background, w3cos_std::Color::rgb(0, 128, 0));
        assert_eq!(principal.style.color, w3cos_std::Color::WHITE);
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn explicit_block_generated_content_keeps_its_independent_line_box() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            "#item::before",
            &[("content", "'Filler text'"), ("display", "block")],
        );

        let mut doc = Document::new();
        let item = doc.create_element("div");
        item.set_attribute(&mut doc, "id", "item");
        let text = doc.create_text_node("Filler text");
        item.append_child(&mut doc, text);
        doc.body().append_child(&mut doc, item);

        let tree = doc.to_component_tree();
        let principal = &tree.children[0];
        assert_eq!(principal.style.display, w3cos_std::style::Display::Block);
        assert_eq!(principal.children.len(), 2);
        assert_eq!(
            principal.children[0].style.display,
            w3cos_std::style::Display::Block
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn generated_table_columns_do_not_create_rendered_boxes() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            "#item::before",
            &[
                ("content", "'BEFORE FAIL'"),
                ("display", "table-column-group"),
            ],
        );
        crate::stylesheet::register_rule(
            "#item::after",
            &[("content", "'AFTER FAIL'"), ("display", "table-column")],
        );

        let mut doc = Document::new();
        let item = doc.create_element("div");
        item.set_attribute(&mut doc, "id", "item");
        let text = doc.create_text_node("Filler text");
        item.append_child(&mut doc, text);
        doc.body().append_child(&mut doc, item);

        fn collect_text(component: &w3cos_std::Component, output: &mut String) {
            if let w3cos_std::ComponentKind::Text { content } = &component.kind {
                output.push_str(content);
            }
            for child in &component.children {
                collect_text(child, output);
            }
        }

        let tree = doc.to_component_tree();
        let mut rendered_text = String::new();
        collect_text(&tree, &mut rendered_text);
        assert_eq!(rendered_text, "Filler text");
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn generated_border_style_uses_its_initial_width_not_the_origin_width() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            "#item",
            &[("border", "15px solid blue"), ("color", "green")],
        );
        crate::stylesheet::register_rule(
            "#item::after",
            &[
                ("content", "'PASS PASS'"),
                ("border-color", "orange"),
                ("border-style", "solid"),
            ],
        );

        let mut doc = Document::new();
        let item = doc.create_element("div");
        item.set_attribute(&mut doc, "id", "item");
        doc.body().append_child(&mut doc, item);

        let tree = doc.to_component_tree();
        let principal = &tree.children[0];
        let generated = &principal.children[0];
        assert_eq!(principal.style.border_width, 15.0);
        assert_eq!(generated.style.border_width, 3.0);
        assert_eq!(
            generated.style.border_color,
            w3cos_std::Color::rgb(255, 165, 0)
        );
        assert_eq!(generated.style.color, w3cos_std::Color::rgb(0, 128, 0));
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn generated_border_shorthand_can_explicitly_inherit_from_the_origin() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("#item", &[("border", "2px solid green")]);
        crate::stylesheet::register_rule(
            "#item::before",
            &[("content", "'Before'"), ("border", "inherit")],
        );

        let mut doc = Document::new();
        let item = doc.create_element("div");
        item.set_attribute(&mut doc, "id", "item");
        doc.body().append_child(&mut doc, item);

        let tree = doc.to_component_tree();
        let principal = &tree.children[0];
        let generated = &principal.children[0];
        assert_eq!(generated.style.border_width, principal.style.border_width);
        assert_eq!(generated.style.border_color, principal.style.border_color);
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn br_lowers_as_a_forced_break_inside_one_anonymous_text_run() {
        crate::stylesheet::clear_rules();
        let mut doc = Document::new();
        let line = doc.create_element("div");
        let first = doc.create_text_node("first");
        let break_element = doc.create_element("br");
        let second = doc.create_text_node("second");
        line.append_child(&mut doc, first);
        line.append_child(&mut doc, break_element);
        line.append_child(&mut doc, second);
        doc.body().append_child(&mut doc, line);

        let tree = doc.to_component_tree();
        assert_eq!(tree.children[0].children.len(), 1);
        assert!(matches!(
            &tree.children[0].children[0].kind,
            w3cos_std::ComponentKind::Text { content } if content == "first\u{2028}second"
        ));
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn nowrap_block_with_inline_siblings_lowers_as_one_line_box() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("#line", &[("white-space", "nowrap")]);

        let mut doc = Document::new();
        let line = doc.create_element("div");
        line.set_attribute(&mut doc, "id", "line");
        for content in ["first", "second"] {
            if !doc.children_ids(line.id).is_empty() {
                let spacing = doc.create_text_node("\n  ");
                line.append_child(&mut doc, spacing);
            }
            let span = doc.create_element("span");
            let text = doc.create_text_node(content);
            span.append_child(&mut doc, text);
            line.append_child(&mut doc, span);
        }
        doc.body().append_child(&mut doc, line);

        let tree = doc.to_component_tree();
        assert_eq!(
            tree.children[0].style.display,
            w3cos_std::style::Display::Flex
        );
        assert!(matches!(
            tree.children[0].kind,
            w3cos_std::ComponentKind::Row
        ));
        assert_eq!(tree.children[0].children.len(), 1);
        assert!(matches!(
            &tree.children[0].children[0].kind,
            w3cos_std::ComponentKind::Text { content } if content == "first second"
        ));
        assert_eq!(
            tree.children[0].children[0].style.display,
            w3cos_std::style::Display::Inline
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn passive_generated_nowrap_siblings_share_one_shaped_text_run() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            "#line",
            &[("white-space", "nowrap"), ("counter-reset", "item")],
        );
        crate::stylesheet::register_rule("#line span", &[("counter-increment", "item")]);
        crate::stylesheet::register_rule("#line span::before", &[("content", "counter(item)")]);

        let mut doc = Document::new();
        let line = doc.create_element("div");
        line.set_attribute(&mut doc, "id", "line");
        for index in 0..3 {
            if index > 0 {
                let spacing = doc.create_text_node("\n  ");
                line.append_child(&mut doc, spacing);
            }
            let span = doc.create_element("span");
            line.append_child(&mut doc, span);
        }
        doc.body().append_child(&mut doc, line);

        let tree = doc.to_component_tree();
        assert_eq!(tree.children[0].children.len(), 1);
        assert!(matches!(
            &tree.children[0].children[0].kind,
            w3cos_std::ComponentKind::Text { content } if content == "1 2 3"
        ));
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn nested_passive_counter_fragments_share_one_shaped_text_run() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("#line", &[("white-space", "nowrap")]);
        crate::stylesheet::register_rule("#line > span", &[("counter-reset", "item")]);
        crate::stylesheet::register_rule(
            "#line span span::before",
            &[("content", "counter(item)")],
        );

        let mut doc = Document::new();
        let line = doc.create_element("div");
        line.set_attribute(&mut doc, "id", "line");
        for index in 0..2 {
            if index > 0 {
                let spacing = doc.create_text_node(" ");
                line.append_child(&mut doc, spacing);
            }
            let outer = doc.create_element("span");
            let inner = doc.create_element("span");
            outer.append_child(&mut doc, inner);
            line.append_child(&mut doc, outer);
        }
        doc.body().append_child(&mut doc, line);

        let tree = doc.to_component_tree();
        assert_eq!(tree.children[0].children.len(), 1);
        assert!(matches!(
            &tree.children[0].children[0].kind,
            w3cos_std::ComponentKind::Text { content } if content == "0 0"
        ));
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn invalid_late_counter_content_declaration_falls_back_in_the_cascade() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("#item", &[("counter-reset", "item 1")]);
        crate::stylesheet::register_rule(
            "#item::before",
            &[
                ("content", "counter(item)"),
                ("content", "counter(item, decimal, decimal)"),
            ],
        );

        let mut doc = Document::new();
        let item = doc.create_element("span");
        item.set_attribute(&mut doc, "id", "item");
        doc.body().append_child(&mut doc, item);

        let tree = doc.to_component_tree();
        assert!(
            matches!(
                &tree.children[0].kind,
                w3cos_std::ComponentKind::Text { content } if content == "1"
            ),
            "invalid later content declaration should leave the earlier value active: {tree:#?}"
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn pseudo_content_inherit_reads_the_originating_elements_computed_content() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("#row", &[("content", "'PASSED'")]);
        crate::stylesheet::register_rule("#cell", &[("content", "inherit")]);
        crate::stylesheet::register_rule(
            "#cell::after",
            &[("content", "inherit"), ("color", "green")],
        );

        let mut doc = Document::new();
        let table = doc.create_element("table");
        let row = doc.create_element("tr");
        row.set_attribute(&mut doc, "id", "row");
        let cell = doc.create_element("td");
        cell.set_attribute(&mut doc, "id", "cell");
        let label = doc.create_text_node("Test has: ");
        cell.append_child(&mut doc, label);
        row.append_child(&mut doc, cell);
        table.append_child(&mut doc, row);
        doc.body().append_child(&mut doc, table);

        fn generated_text(component: &w3cos_std::Component) -> Option<&w3cos_std::Component> {
            if matches!(
                &component.kind,
                w3cos_std::ComponentKind::Text { content } if content.trim() == "PASSED"
            ) {
                return Some(component);
            }
            component.children.iter().find_map(generated_text)
        }

        fn has_inline_pair(component: &w3cos_std::Component) -> bool {
            let has_label = component.children.iter().any(|child| {
                matches!(
                    &child.kind,
                    w3cos_std::ComponentKind::Text { content } if content.trim_end() == "Test has:"
                )
            });
            let has_generated = component.children.iter().any(|child| {
                matches!(
                    &child.kind,
                    w3cos_std::ComponentKind::Text { content } if content.trim() == "PASSED"
                )
            });
            (component.style.display == w3cos_std::style::Display::Flex
                && has_label
                && has_generated)
                || component.children.iter().any(has_inline_pair)
        }

        let tree = doc.to_component_tree();
        let generated =
            generated_text(&tree).unwrap_or_else(|| panic!("inherited generated text: {tree:#?}"));
        assert!(matches!(
            &generated.kind,
            w3cos_std::ComponentKind::Text { content } if content == "PASSED"
        ));
        assert_eq!(generated.style.color, w3cos_std::Color::rgb(0, 128, 0));
        assert!(has_inline_pair(&tree));
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn generated_quotes_inherit_pairs_and_track_document_depth() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("#line", &[("quotes", "'P' 'S' 'A' 'S'")]);
        crate::stylesheet::register_rule(
            "#line span",
            &[
                ("display", "inline"),
                ("margin", "0"),
                ("padding", "0"),
                ("width", "auto"),
                ("height", "auto"),
                ("border", "none"),
                ("color", "inherit"),
                ("background", "transparent"),
            ],
        );
        crate::stylesheet::register_rule(
            "#pass::before",
            &[("content", "open-quote open-quote close-quote close-quote")],
        );
        crate::stylesheet::register_rule("#advance::before", &[("content", "no-open-quote")]);
        crate::stylesheet::register_rule("#nested::before", &[("content", "open-quote")]);

        let mut doc = Document::new();
        let line = doc.create_element("div");
        line.set_attribute(&mut doc, "id", "line");
        for id in ["pass", "advance", "nested"] {
            let comment = doc.create_comment("does not generate a CSS box");
            line.append_child(&mut doc, comment);
            let span = doc.create_element("span");
            span.set_attribute(&mut doc, "id", id);
            let nested_comment = doc.create_comment("also inert inside an inline subtree");
            span.append_child(&mut doc, nested_comment);
            line.append_child(&mut doc, span);
        }
        doc.body().append_child(&mut doc, line);

        let tree = doc.to_component_tree();
        fn rendered_text(component: &w3cos_std::Component) -> String {
            let own = match &component.kind {
                w3cos_std::ComponentKind::Text { content } => content.clone(),
                _ => String::new(),
            };
            component.children.iter().fold(own, |mut text, child| {
                text.push_str(&rendered_text(child));
                text
            })
        }
        assert_eq!(rendered_text(&tree.children[0]), "PASSA");
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn generated_quotes_preserve_collapsed_spaces_at_nested_inline_boundaries() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("#wrapper", &[("quotes", "'\"' '\"' \"'\" \"'\"")]);
        crate::stylesheet::register_rule(".spans::before", &[("content", "open-quote")]);
        crate::stylesheet::register_rule(".spans::after", &[("content", "close-quote")]);
        crate::stylesheet::register_rule("#inner::after", &[("content", "no-close-quote")]);

        let mut doc = Document::new();
        let wrapper = doc.create_element("div");
        wrapper.set_attribute(&mut doc, "id", "wrapper");
        let outer = doc.create_element("span");
        outer.class_list_add(&mut doc, "spans");
        let leading = doc.create_text_node("\n  ");
        let inner = doc.create_element("span");
        inner.set_attribute(&mut doc, "id", "inner");
        inner.class_list_add(&mut doc, "spans");
        let trailing = doc.create_text_node("\n");
        outer.append_child(&mut doc, leading);
        outer.append_child(&mut doc, inner);
        outer.append_child(&mut doc, trailing);
        wrapper.append_child(&mut doc, outer);
        doc.body().append_child(&mut doc, wrapper);

        fn contains_expected(component: &w3cos_std::Component) -> bool {
            matches!(
                &component.kind,
                w3cos_std::ComponentKind::Text { content } if content == "\" ' \""
            ) || component.children.iter().any(contains_expected)
        }
        assert!(contains_expected(&doc.to_component_tree()));
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn state_only_quote_wrappers_do_not_split_a_passive_inline_run() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(".party *", &[("display", "inline")]);
        crate::stylesheet::register_rule("#first::before", &[("content", "'A'")]);
        crate::stylesheet::register_rule("#state::before", &[("content", "no-open-quote")]);
        crate::stylesheet::register_rule("#last::before", &[("content", "'B'")]);

        let mut doc = Document::new();
        let line = doc.create_element("div");
        line.class_list_add(&mut doc, "party");
        for id in ["first", "state", "last"] {
            let child = doc.create_element("div");
            child.set_attribute(&mut doc, "id", id);
            line.append_child(&mut doc, child);
        }
        doc.body().append_child(&mut doc, line);

        let tree = doc.to_component_tree();
        assert_eq!(tree.children[0].children.len(), 1);
        assert!(matches!(
            &tree.children[0].children[0].kind,
            w3cos_std::ComponentKind::Text { content } if content == "AB"
        ));
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn inside_list_marker_lowers_to_a_text_component() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            "li",
            &[
                ("list-style-type", "square"),
                ("list-style-position", "inside"),
                ("color", "blue"),
            ],
        );

        let mut doc = Document::new();
        let item = doc.create_element("li");
        doc.body().append_child(&mut doc, item);

        let tree = doc.to_component_tree();
        assert!(matches!(
            &tree.children[0].children[0].kind,
            w3cos_std::ComponentKind::Text { content } if content == "▪"
        ));
        assert_eq!(
            tree.children[0].children[0].style.color,
            w3cos_std::Color::rgb(0, 0, 255)
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn component_tree_inherits_styles_from_the_html_root() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("html", &[("color", "green"), ("font-size", "20px")]);

        let mut doc = Document::new();
        let body = doc.body();
        let document = doc.get_node(body.id).parent.expect("document root");
        let html = doc.create_element("html");
        doc.remove_child(document, body.id);
        doc.append_child(document, html.id);
        doc.append_child(html.id, body.id);

        let tree = doc.to_component_tree();
        assert_eq!(tree.style.color, w3cos_std::Color::rgb(0, 128, 0));
        assert_eq!(tree.style.font_size, 20.0);
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
    fn test_stylesheet_sibling_selectors_use_element_siblings_only() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(".first + .second", &[("color", "red")]);
        crate::stylesheet::register_rule(".first ~ .third", &[("color", "blue")]);

        let mut doc = Document::new();
        for class_name in ["first", "second", "third"] {
            let element = doc.create_element("div");
            element.class_list_add(&mut doc, class_name);
            doc.append_child(doc.body().id, element.id);
            let whitespace = doc.create_text_node("\n  ");
            doc.append_child(doc.body().id, whitespace.id);
        }
        let tree = doc.to_component_tree();
        assert_eq!(
            tree.children[1].style.color,
            w3cos_std::Color::rgb(255, 0, 0)
        );
        assert_eq!(
            tree.children[2].style.color,
            w3cos_std::Color::rgb(0, 0, 255)
        );
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
        assert_eq!(component.children.len(), 1);
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
    fn test_background_color_inherit_uses_parent_computed_color() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("#wrapper", &[("background-color", "green")]);
        crate::stylesheet::register_rule(
            "#test",
            &[("background-color", "red"), ("background-color", "inherit")],
        );

        let mut doc = Document::new();
        let wrapper = doc.create_element("div");
        wrapper.set_attribute(&mut doc, "id", "wrapper");
        let test = doc.create_element("div");
        test.set_attribute(&mut doc, "id", "test");
        wrapper.append_child(&mut doc, test);
        doc.body().append_child(&mut doc, wrapper);

        assert_eq!(
            doc.computed_style_for(test.id).background,
            w3cos_std::Color::from_css("green").unwrap()
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn test_background_position_inherit_uses_parent_computed_position() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("#parent", &[("background-position", "192px")]);
        crate::stylesheet::register_rule("#child", &[("background-position", "inherit")]);

        let mut doc = Document::new();
        let parent = doc.create_element("div");
        parent.set_attribute(&mut doc, "id", "parent");
        let child = doc.create_element("div");
        child.set_attribute(&mut doc, "id", "child");
        parent.append_child(&mut doc, child);
        doc.body().append_child(&mut doc, parent);

        assert_eq!(
            doc.computed_style_for(child.id)
                .background_position
                .as_deref(),
            Some("192px")
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn test_font_shorthand_survives_computed_style_inheritance() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("p", &[("font", "1em/1.25 serif")]);

        let mut doc = Document::new();
        let paragraph = doc.create_element("p");
        let text = doc.create_element("span");
        paragraph.append_child(&mut doc, text);
        doc.body().append_child(&mut doc, paragraph);

        let paragraph_style = doc.computed_style_for(paragraph.id);
        assert_eq!(paragraph_style.font_size, 16.0);
        assert_eq!(paragraph_style.line_height, 1.25);
        assert_eq!(paragraph_style.font_family.as_deref(), Some("serif"));
        assert_eq!(doc.computed_style_for(text.id).line_height, 1.25);
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
    fn invalid_white_space_declaration_does_not_override_valid_value() {
        let mut style = CSSStyleDeclaration::new();
        style.set_property("white-space", "pre");
        style.set_property("white-space", "pre-lines");

        assert_eq!(
            style.inner.white_space,
            w3cos_std::style::WhiteSpace::Pre
        );
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
