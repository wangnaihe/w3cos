use super::Document;
use w3cos_std::style::{AlignItems, Display};

/// A flex container's inline-block children are blockified flex items, not
/// an inline formatting context (CSS Display §2.7), so the container keeps
/// its authored `align-items` and the text-only items still stretch
/// (flexbox_flex-* WPT).
#[test]
fn flex_container_with_inline_block_text_items_keeps_its_alignment() {
    crate::stylesheet::clear_rules();
    crate::stylesheet::register_rule(
        "#flex",
        &[("display", "flex"), ("width", "300px"), ("height", "100px")],
    );
    crate::stylesheet::register_rule(".item", &[("display", "inline-block"), ("width", "50px")]);
    let mut document = Document::new();
    let flex = document.create_element("div");
    flex.set_attribute(&mut document, "id", "flex");
    for word in ["one", "two"] {
        let item = document.create_element("span");
        item.set_attribute(&mut document, "class", "item");
        let text = document.create_text_node(word);
        item.append_child(&mut document, text);
        flex.append_child(&mut document, item);
    }
    document.body().append_child(&mut document, flex);
    let tree = document.to_component_tree();
    fn find(component: &w3cos_std::Component) -> Option<&w3cos_std::Component> {
        if component.style.display == Display::Flex {
            return Some(component);
        }
        component.children.iter().find_map(find)
    }
    let container = find(&tree).expect("flex container");
    assert_eq!(container.style.align_items, AlignItems::Stretch);
    assert_eq!(container.children.len(), 2);
    crate::stylesheet::clear_rules();
}

#[test]
fn break_spaces_auto_width_block_does_not_pin_line_width_in_the_component_tree() {
    crate::stylesheet::clear_rules();
    crate::stylesheet::register_rule(".container", &[("width", "100px")]);
    crate::stylesheet::register_rule(
        ".test",
        &[
            ("white-space", "break-spaces"),
            ("overflow-wrap", "break-word"),
        ],
    );
    let mut document = Document::new();
    let container = document.create_element("div");
    container.set_attribute(&mut document, "class", "container");
    let test = document.create_element("div");
    test.set_attribute(&mut document, "class", "test");
    let text = document.create_text_node(" XXXXX ");
    test.append_child(&mut document, text);
    container.append_child(&mut document, test);
    document.body().append_child(&mut document, container);
    let tree = document.to_component_tree();
    fn find_test<'a>(component: &'a w3cos_std::Component) -> Option<&'a w3cos_std::Component> {
        if component
            .style
            .white_space
            == w3cos_std::style::WhiteSpace::PreWrap
            && matches!(component.kind, w3cos_std::ComponentKind::Box)
            || component.children.iter().any(|child| {
                matches!(&child.kind, w3cos_std::ComponentKind::Text { content } if content.contains("XXXXX"))
            })
        {
            if component.children.iter().any(|child| {
                matches!(&child.kind, w3cos_std::ComponentKind::Text { content } if content.contains("XXXXX"))
            }) {
                return Some(component);
            }
        }
        component.children.iter().find_map(find_test)
    }
    let test_box = find_test(&tree).expect("test block with XXXXX");
    let summary: Vec<String> = test_box
        .children
        .iter()
        .map(|child| {
            let kind = match &child.kind {
                w3cos_std::ComponentKind::Text { content } => format!("text:{content:?}"),
                other => format!("{other:?}"),
            };
            let marker = child
                .style
                .custom_properties
                .as_ref()
                .is_some_and(|properties| {
                    properties.contains_key("--w3cos-internal-text-line-width")
                });
            format!(
                "{kind} ws={:?} width={:?} marker={marker}",
                child.style.white_space, child.style.width
            )
        })
        .collect();
    assert!(
        test_box.children.iter().all(|child| {
            child
                .style
                .custom_properties
                .as_ref()
                .is_none_or(|properties| {
                    !properties.contains_key("--w3cos-internal-text-line-width")
                })
        }),
        "line-width wrapping is a legacy Taffy concern; the shared tree must stay unmarked so NG-on is unchanged, children={summary:?} parent_display={:?} parent_width={:?} parent_ws={:?} parent_marker={:?}",
        test_box.style.display,
        test_box.style.width,
        test_box.style.white_space,
        test_box.style.custom_properties,
    );
    assert!(
        test_box
            .children
            .iter()
            .all(|child| { matches!(child.style.width, w3cos_std::style::Dimension::Auto) }),
        "sole break-spaces text must keep auto width in the shared tree, children={summary:?}",
    );
    crate::stylesheet::clear_rules();
}

#[test]
fn grid_lanes_keeps_initial_align_items_in_the_component_tree() {
    crate::stylesheet::clear_rules();
    crate::stylesheet::register_rule(
        ".lanes",
        &[
            ("display", "grid-lanes"),
            ("grid-template-columns", "100px 100px"),
            ("width", "200px"),
        ],
    );
    let mut document = Document::new();
    let lanes = document.create_element("div");
    lanes.set_attribute(&mut document, "class", "lanes");
    for _ in 0..2 {
        let item = document.create_element("div");
        lanes.append_child(&mut document, item);
    }
    document.body().append_child(&mut document, lanes);
    let tree = document.to_component_tree();
    fn find(component: &w3cos_std::Component) -> Option<&w3cos_std::Component> {
        if component
            .style
            .custom_properties
            .as_ref()
            .is_some_and(|properties| {
                properties.contains_key(crate::css_style::GRID_LANES_PROPERTY)
            })
        {
            return Some(component);
        }
        component.children.iter().find_map(find)
    }
    let container = find(&tree).expect("grid-lanes container");
    assert_eq!(container.style.align_items, AlignItems::Stretch);
    crate::stylesheet::clear_rules();
}
