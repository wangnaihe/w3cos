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
