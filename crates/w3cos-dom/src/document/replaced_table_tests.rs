use super::Document;
use w3cos_std::{Component, ComponentKind, style::Display};

fn collect<'a>(root: &'a Component, nodes: &mut Vec<&'a Component>) {
    nodes.push(root);
    for child in &root.children {
        collect(child, nodes);
    }
}

#[test]
fn replaced_table_cell_images_stack_in_one_anonymous_cell() {
    let mut document = Document::new();
    let table = document.create_element("div");
    table
        .style_mut(&mut document)
        .set_property("display", "table");
    table
        .style_mut(&mut document)
        .set_property("white-space", "pre");
    let row = document.create_element("div");
    row.style_mut(&mut document)
        .set_property("display", "table-row");
    let mut image_ids = vec![];
    for separator in [" ", "\t "] {
        let text = document.create_text_node(separator);
        row.append_child(&mut document, text);
        let image = document.create_element("img");
        image
            .style_mut(&mut document)
            .set_property("display", "table-cell");
        image_ids.push(image.id);
        row.append_child(&mut document, image);
    }
    let tail = document.create_text_node("   ");
    row.append_child(&mut document, tail);
    table.append_child(&mut document, row);
    document.body().append_child(&mut document, table);
    let root = document.to_component_tree();
    for image in image_ids {
        assert_eq!(
            document.computed_style(image, &[], None).display,
            Display::TableCell,
            "used lowering must not rewrite computed CSSOM display"
        );
    }
    let mut nodes = vec![];
    collect(&root, &mut nodes);
    let images = nodes
        .iter()
        .filter(|node| matches!(node.kind, ComponentKind::Image { .. }))
        .collect::<Vec<_>>();
    assert_eq!(images.len(), 2);
    assert!(
        images
            .iter()
            .all(|node| node.style.display == Display::Block),
        "replaced table-cell display must not become an atomic inline"
    );
    assert_eq!(
        nodes
            .iter()
            .filter(|node| node.style.display == Display::TableCell)
            .count(),
        1,
        "replaced internals share an anonymous cell, not separate table tracks"
    );
    assert!(
        !nodes.iter().any(
            |node| matches!(&node.kind, ComponentKind::Text { content } if !content.is_empty())
        ),
        "table-role separators must not become preformatted inline lines"
    );
}

#[test]
fn replaced_column_images_have_block_used_display_without_changing_cssom() {
    for (css,display) in [("table-column",Display::TableColumn),("table-column-group",Display::TableColumnGroup)] {
        let mut document=Document::new();
        let image=document.create_element("img");
        image.style_mut(&mut document).set_property("display",css);
        document.body().append_child(&mut document,image);
        let root=document.to_component_tree();
        let mut nodes=vec![];
        collect(&root,&mut nodes);
        let used=nodes.iter().find(|node| matches!(node.kind,ComponentKind::Image {..})).unwrap();
        assert_eq!(document.computed_style(image.id,&[],None).display,display);
        assert_eq!(used.style.display,Display::Block,"replaced column role must not suppress its image outline");
    }
}

#[test]
fn ordinary_inline_image_is_not_blockified() {
    let mut document = Document::new();
    let image = document.create_element("img");
    document.body().append_child(&mut document, image);
    let root = document.to_component_tree();
    let mut nodes = vec![];
    collect(&root, &mut nodes);
    let image = nodes
        .iter()
        .find(|node| matches!(node.kind, ComponentKind::Image { .. }))
        .unwrap();
    assert_eq!(image.style.display, Display::InlineBlock);
}
