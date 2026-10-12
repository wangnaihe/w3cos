use super::Document;
use w3cos_std::{Component, ComponentKind};

fn text(component: &Component) -> String {
    let mut result = match &component.kind {
        ComponentKind::Text { content } => content.clone(),
        _ => String::new(),
    };
    for child in &component.children { result.push_str(&text(child)); }
    result
}

#[test]
fn content_inherit_does_not_import_origin_or_ancestor_strings() {
    for inherited_child in [false, true] {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(".origin", &[("content", "'PARENT'")]);
        crate::stylesheet::register_rule(".target::after", &[("content", "inherit")]);
        if inherited_child {
            crate::stylesheet::register_rule(".target", &[("content", "inherit")]);
        }
        let mut document = Document::new();
        let origin = document.create_element("div");
        origin.set_attribute(&mut document, "class", if inherited_child { "origin" } else { "origin target" });
        let target = if inherited_child {
            let target = document.create_element("span");
            target.set_attribute(&mut document, "class", "target");
            origin.append_child(&mut document, target);
            target
        } else { origin };
        let visible = document.create_text_node("Actual");
        target.append_child(&mut document, visible);
        document.body().append_child(&mut document, origin);
        assert_eq!(text(&document.to_component_tree()), "Actual", "inherited child={inherited_child}");
    }
    crate::stylesheet::clear_rules();
}

#[test]
fn winning_content_inherit_suppresses_earlier_strings() {
    crate::stylesheet::clear_rules();
    crate::stylesheet::register_rule("div", &[("content", "'PARENT'")]);
    crate::stylesheet::register_rule("div::after", &[("content", "'OLD'"), ("content", "inherit")]);
    let mut document = Document::new();
    let element = document.create_element("div");
    let visible = document.create_text_node("Actual");
    element.append_child(&mut document, visible);
    document.body().append_child(&mut document, element);
    assert_eq!(text(&document.to_component_tree()), "Actual");
    crate::stylesheet::clear_rules();
}

#[test]
fn explicit_generated_string_is_not_suppressed_by_origin_content() {
    crate::stylesheet::clear_rules();
    crate::stylesheet::register_rule("div", &[("content", "'PARENT'")]);
    crate::stylesheet::register_rule("div::after", &[("content", "'OWN'")]);
    let mut document = Document::new();
    let element = document.create_element("div");
    let visible = document.create_text_node("Actual");
    element.append_child(&mut document, visible);
    document.body().append_child(&mut document, element);
    assert_eq!(text(&document.to_component_tree()), "ActualOWN");
    crate::stylesheet::clear_rules();
}

#[test]
fn generated_inline_table_keeps_anonymous_cell_and_adjacent_text() {
    use w3cos_std::style::Display;
    fn find(component: &Component, display: Display) -> Option<&Component> {
        if component.style.display == display { return Some(component); }
        component.children.iter().find_map(|child| find(child, display))
    }
    crate::stylesheet::clear_rules();
    crate::stylesheet::register_rule("div::before", &[("content", "'Filler text\\A0'"), ("display", "inline-table")]);
    let mut document = Document::new();
    let element = document.create_element("div");
    let visible = document.create_text_node("Filler text");
    element.append_child(&mut document, visible);
    document.body().append_child(&mut document, element);
    let tree = document.to_component_tree();
    let table = find(&tree, Display::InlineTable).expect("generated atomic table");
    let cell = find(table, Display::TableCell).expect("anonymous table cell");
    assert_eq!(text(cell), "Filler text\u{a0}");
    assert_eq!(text(table), "Filler text\u{a0}");
    assert_eq!(text(&tree), "Filler text\u{a0}Filler text");
    crate::stylesheet::clear_rules();
}
