use super::Document;
use crate::Element;
use w3cos_std::style::Display;

fn element(document: &mut Document, display: &str, text: &str) -> Element {
    let node = document.create_element("span");
    node.style_mut(document).set_property("display", display);
    if !text.is_empty() {
        let content = document.create_text_node(text);
        node.append_child(document, content);
    }
    node
}

fn count_display(component: &w3cos_std::Component, display: Display) -> usize {
    usize::from(component.style.display == display)
        + component
            .children
            .iter()
            .map(|child| count_display(child, display))
            .sum::<usize>()
}

fn text_content(component: &w3cos_std::Component) -> String {
    let mut text = match &component.kind {
        w3cos_std::ComponentKind::Text { content } => content.clone(),
        _ => String::new(),
    };
    for child in &component.children {
        text.push_str(&text_content(child));
    }
    text
}

#[test]
fn text_only_list_item_keeps_its_generated_marker() {
    for tag in ["span", "p", "div"] {
        let mut document = Document::new();
        let item = document.create_element(tag);
        item.style_mut(&mut document)
            .set_property("display", "list-item");
        let text = document.create_text_node("Filler Text");
        item.append_child(&mut document, text);
        document.body().append_child(&mut document, item);
        let component = document.to_component_subtree(item.id);
        assert_eq!(component.style.display, Display::Block, "{tag}");
        assert!(
            component.children.iter().any(|child| child
                .style
                .custom_properties
                .as_ref()
                .is_some_and(
                    |properties| properties.contains_key("--w3cos-internal-outside-list-marker")
                )),
            "{tag}"
        );
    }
}

#[test]
fn row_source_whitespace_does_not_enter_anonymous_cell_text() {
    let mut document = Document::new();
    let row = element(&mut document, "table-row", "");
    row.style_mut(&mut document)
        .set_property("white-space", "nowrap");
    document.body().append_child(&mut document, row);
    for text in ["A", "B"] {
        let separator = document.create_text_node("\n  ");
        row.append_child(&mut document, separator);
        let span = element(&mut document, "inline", text);
        row.append_child(&mut document, span);
    }
    assert_eq!(text_content(&document.to_component_subtree(row.id)), "AB");
}

#[test]
fn table_cell_source_whitespace_still_separates_inline_text() {
    let mut document = Document::new();
    let cell = element(&mut document, "table-cell", "");
    document.body().append_child(&mut document, cell);
    let first = element(&mut document, "inline", "A");
    let space = document.create_text_node(" ");
    let last = element(&mut document, "inline", "B");
    for child in [first, space, last] {
        cell.append_child(&mut document, child);
    }
    assert_eq!(text_content(&document.to_component_subtree(cell.id)), "A B");
}

#[test]
fn anonymous_plain_cells_keep_shaping_boundaries() {
    use w3cos_std::{
        Component, ComponentKind,
        style::{Style, WhiteSpace},
    };
    for nested in [false, true] {
        let parent = Style {
            display: Display::Block,
            white_space: WhiteSpace::NoWrap,
            ..Style::default()
        };
        let cells = ["Row 333, Col 1", "Row 333, Col 2", "Row 333, Col 3"]
            .into_iter()
            .map(|text| {
                let cell = Style {
                    display: Display::TableCell,
                    ..parent.clone()
                };
                if nested {
                    Component::boxed(
                        cell,
                        vec![Component::text(
                            text,
                            Style {
                                display: Display::Inline,
                                ..parent.clone()
                            },
                        )],
                    )
                } else {
                    Component::text(text, cell)
                }
            })
            .collect();
        let table = super::anonymous_table_wrapper(&parent, cells);
        let text = super::plain_anonymous_inline_table_text(&table).unwrap();
        let ComponentKind::Text { content } = &text.kind else {
            panic!("text expected")
        };
        assert_eq!(
            w3cos_std::inline_text::fragment_ends(content, &text.style),
            Some(vec![14, 28, 42]),
            "nested={nested}"
        );
    }
}

#[test]
fn pre_row_raw_text_keeps_following_space_not_leading_space() {
    let mut document = Document::new();
    let row = element(&mut document, "table-row", "");
    row.style_mut(&mut document)
        .set_property("white-space", "pre");
    document.body().append_child(&mut document, row);
    let first = element(&mut document, "table-cell", "a");
    row.append_child(&mut document, first);
    for content in [" ", "bc", " "] {
        let text = document.create_text_node(content);
        row.append_child(&mut document, text);
    }
    let last = element(&mut document, "table-cell", "d");
    row.append_child(&mut document, last);
    assert_eq!(
        text_content(&document.to_component_subtree(row.id)),
        "abc d"
    );
}

#[test]
fn misparented_cell_trailing_glue_differs_from_authored_inline_table() {
    for white_space in ["normal", "nowrap", "pre"] {
        for display in ["table-cell", "inline-table"] {
            let mut document = Document::new();
            let host = element(&mut document, "inline", "");
            host.style_mut(&mut document)
                .set_property("white-space", white_space);
            document.body().append_child(&mut document, host);
            let first = element(&mut document, "inline", "a");
            let leading = document.create_text_node(" ");
            let cell = element(&mut document, display, "bc");
            let trailing = document.create_text_node(" ");
            let last = element(&mut document, "inline", "d");
            for child in [first, leading, cell, trailing, last] {
                host.append_child(&mut document, child);
            }
            let expected = if display == "table-cell" && white_space != "pre" {
                "a bcd"
            } else {
                "a bc d"
            };
            assert_eq!(
                text_content(&document.to_component_subtree(host.id)),
                expected,
                "display={display} white-space={white_space}"
            );
        }
    }
}

#[test]
fn after_pseudo_does_not_revive_discarded_table_cell_source_glue() {
    for white_space in ["normal", "nowrap", "pre"] {
        for display in ["table-cell", "inline-table"] {
            crate::stylesheet::clear_rules();
            crate::stylesheet::register_rule("#source-pseudo::after", &[("content", "'d'")]);
            let mut document = Document::new();
            let host = element(&mut document, "inline", "");
            host.set_attribute(&mut document, "id", "source-pseudo");
            host.style_mut(&mut document)
                .set_property("white-space", white_space);
            document.body().append_child(&mut document, host);
            let first = document.create_text_node("a ");
            let cell = element(&mut document, display, "bc");
            let trailing = document.create_text_node(" ");
            for child in [first, cell, trailing] {
                host.append_child(&mut document, child);
            }
            let text = text_content(&document.to_component_subtree(host.id));
            crate::stylesheet::clear_rules();
            let expected = if display == "table-cell" && white_space != "pre" {
                "a bcd"
            } else {
                "a bc d"
            };
            assert_eq!(
                text, expected,
                "display={display} white-space={white_space}"
            );
        }
    }
}

#[test]
fn removal_preserves_anonymous_table_wrapper_boundary() {
    let mut document = Document::new();
    let parent = document.create_element("div");
    parent
        .style_mut(&mut document)
        .set_property("display", "block");
    document.body().append_child(&mut document, parent);
    let first = element(&mut document, "table-cell", "First cell");
    let separator = element(&mut document, "inline", "Separator");
    let last = element(&mut document, "table-cell", "Last cell");
    for child in [first, separator, last] {
        parent.append_child(&mut document, child);
    }
    assert_eq!(
        count_display(&document.to_component_subtree(parent.id), Display::Table),
        2
    );
    parent.remove_child(&mut document, &separator);
    assert_eq!(
        count_display(&document.to_component_subtree(parent.id), Display::Table),
        2,
        "removal must preserve the two existing anonymous table wrappers"
    );
}

#[test]
fn removal_preserves_anonymous_row_boundary_within_row_group() {
    let mut document = Document::new();
    let parent = element(&mut document, "table-row-group", "");
    document.body().append_child(&mut document, parent);
    let first = element(&mut document, "table-cell", "First cell");
    let separator = element(&mut document, "table-row", "Separator");
    let last = element(&mut document, "table-cell", "Last cell");
    for child in [first, separator, last] {
        parent.append_child(&mut document, child);
    }
    assert_eq!(
        count_display(&document.to_component_subtree(parent.id), Display::TableRow),
        3
    );
    parent.remove_child(&mut document, &separator);
    assert_eq!(
        count_display(&document.to_component_subtree(parent.id), Display::TableRow),
        2,
        "removing a row must not merge its neighboring anonymous rows"
    );
}

#[test]
fn static_adjacent_cells_share_one_anonymous_table_and_row() {
    let mut document = Document::new();
    let parent = document.create_element("div");
    parent
        .style_mut(&mut document)
        .set_property("display", "block");
    document.body().append_child(&mut document, parent);
    for text in ["First cell", "Last cell"] {
        let child = element(&mut document, "table-cell", text);
        parent.append_child(&mut document, child);
    }
    let tree = document.to_component_subtree(parent.id);
    assert_eq!(count_display(&tree, Display::Table), 1);
    assert_eq!(count_display(&tree, Display::TableRow), 1);
}

#[test]
fn removing_improper_group_child_does_not_split_its_anonymous_row() {
    for display in [
        "table-row-group",
        "table-column",
        "table-column-group",
        "table-caption",
    ] {
        let mut document = Document::new();
        let parent = element(&mut document, "table-row-group", "");
        document.body().append_child(&mut document, parent);
        let first = element(&mut document, "table-cell", "First cell");
        let separator = element(&mut document, display, "Separator");
        let last = element(&mut document, "table-cell", "Last cell");
        for child in [first, separator, last] {
            parent.append_child(&mut document, child);
        }
        parent.remove_child(&mut document, &separator);
        assert_eq!(
            count_display(&document.to_component_subtree(parent.id), Display::TableRow),
            1,
            "{display} is an improper group child inside the same anonymous row"
        );
    }
}

#[test]
fn replacing_separator_text_with_whitespace_retains_table_boundary() {
    let mut document = Document::new();
    let parent = document.create_element("div");
    parent
        .style_mut(&mut document)
        .set_property("display", "block");
    document.body().append_child(&mut document, parent);
    let first = element(&mut document, "table-cell", "First cell");
    let separator = document.create_text_node("Separator");
    let last = element(&mut document, "table-cell", "Last cell");
    for child in [first, separator, last] {
        parent.append_child(&mut document, child);
    }
    separator.set_text_content(&mut document, " ");
    assert_eq!(
        count_display(&document.to_component_subtree(parent.id), Display::Table),
        2
    );
}

#[test]
fn disconnected_removal_does_not_create_retained_layout_boundaries() {
    let mut document = Document::new();
    let parent = document.create_element("div");
    parent
        .style_mut(&mut document)
        .set_property("display", "block");
    let first = element(&mut document, "table-cell", "First cell");
    let separator = element(&mut document, "inline", "Separator");
    let last = element(&mut document, "table-cell", "Last cell");
    for child in [first, separator, last] {
        parent.append_child(&mut document, child);
    }
    parent.remove_child(&mut document, &separator);
    document.body().append_child(&mut document, parent);
    assert_eq!(
        count_display(&document.to_component_subtree(parent.id), Display::Table),
        1
    );
}
