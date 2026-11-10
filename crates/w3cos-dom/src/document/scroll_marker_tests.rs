//! CSS Overflow 5 `::scroll-marker-group` / `::column::scroll-marker`.

use super::*;
use w3cos_std::component::ComponentKind;
use w3cos_std::style::{Contain, Display, FlexDirection, ScrollMarkerGroup, TextDecoration};

fn find_marker_group(component: &w3cos_std::Component) -> Option<&w3cos_std::Component> {
    if component
        .style
        .custom_properties
        .as_ref()
        .is_some_and(|properties| properties.contains_key("--w3cos-internal-scroll-marker-group"))
    {
        return Some(component);
    }
    component.children.iter().find_map(find_marker_group)
}

fn descendant_text(component: &w3cos_std::Component) -> String {
    let mut output = String::new();
    fn walk(component: &w3cos_std::Component, output: &mut String) {
        if let ComponentKind::Text { content } = &component.kind {
            output.push_str(content);
        }
        for child in &component.children {
            walk(child, output);
        }
    }
    walk(component, &mut output);
    output
}

#[test]
fn scroll_marker_group_is_a_preceding_sibling_with_column_markers() {
    crate::stylesheet::clear_rules();
    crate::stylesheet::register_rule(
        "div",
        &[("scroll-marker-group", "before"), ("overflow", "hidden")],
    );
    crate::stylesheet::register_rule(
        "div::scroll-marker-group",
        &[
            ("display", "flex"),
            ("height", "50px"),
            ("background", "cyan"),
        ],
    );
    crate::stylesheet::register_rule(
        "div::column::scroll-marker",
        &[
            ("display", "flex"),
            ("width", "50px"),
            ("height", "50px"),
            ("background", "hotpink"),
            ("content", "'*'"),
        ],
    );
    crate::stylesheet::register_rule(
        "div::column::scroll-marker:target-current",
        &[("background", "green")],
    );

    let mut document = Document::new();
    let container = document.create_element("div");
    document.body().append_child(&mut document, container);
    assert_eq!(
        document
            .computed_style_for(container.id)
            .scroll_marker_group,
        ScrollMarkerGroup::Before
    );

    // First build has no layout-time column count yet.
    let tree = document.to_component_tree();
    let group = find_marker_group(&tree).expect("group on first build");
    assert_eq!(group.style.display, Display::Flex);
    assert_eq!(group.style.flex_direction, FlexDirection::Row);
    assert!(group.children.is_empty());

    assert!(set_column_pseudo_count(container.id, 5));
    let tree = document.to_component_tree();
    let group = find_marker_group(&tree).expect("group after count");
    assert_eq!(group.children.len(), 5);
    assert_eq!(descendant_text(&group.children[0]), "*");
    assert_eq!(group.children[0].style.background.g, 128); // green
    assert_eq!(group.children[1].style.background.g, 105); // hotpink
    assert_eq!(
        group.children[1].style.color,
        w3cos_std::Color::rgb(0, 0, 238)
    );
    assert_eq!(
        group.children[1].style.text_decoration,
        TextDecoration::Underline
    );
    assert!(!set_column_pseudo_count(container.id, 5));

    crate::stylesheet::clear_rules();
    clear_column_pseudo_counts();
}

#[test]
fn child_scroll_markers_fill_the_group_without_a_column_count() {
    crate::stylesheet::clear_rules();
    crate::stylesheet::register_rule(
        "section",
        &[("scroll-marker-group", "after"), ("overflow", "hidden")],
    );
    crate::stylesheet::register_rule(
        "section::scroll-marker-group",
        &[("display", "flex"), ("height", "20px")],
    );
    crate::stylesheet::register_rule(
        "section>div::scroll-marker",
        &[
            ("content", "''"),
            ("width", "20px"),
            ("background", "green"),
        ],
    );

    let mut document = Document::new();
    let scroller = document.create_element("section");
    let first = document.create_element("div");
    let second = document.create_element("div");
    scroller.append_child(&mut document, first);
    scroller.append_child(&mut document, second);
    document.body().append_child(&mut document, scroller);

    let tree = document.to_component_tree();
    let group = find_marker_group(&tree).expect("group for child markers");
    assert_eq!(group.children.len(), 2);
    assert_eq!(
        document.computed_style_for(scroller.id).scroll_marker_group,
        ScrollMarkerGroup::After
    );

    crate::stylesheet::clear_rules();
    clear_column_pseudo_counts();
}

#[test]
fn column_scroll_marker_defaults_to_a_link_sized_box() {
    crate::stylesheet::clear_rules();
    crate::stylesheet::register_rule(
        "div",
        &[("scroll-marker-group", "before"), ("overflow", "hidden")],
    );
    crate::stylesheet::register_rule("div::scroll-marker-group", &[("display", "flex")]);
    crate::stylesheet::register_rule(
        "div::column::scroll-marker",
        &[
            ("content", "counter(item)"),
            ("width", "100px"),
            ("height", "100px"),
            ("background", "green"),
        ],
    );

    let mut document = Document::new();
    let container = document.create_element("div");
    document.body().append_child(&mut document, container);
    assert!(set_column_pseudo_count(container.id, 1));
    let tree = document.to_component_tree();
    let group = find_marker_group(&tree).expect("group");
    assert_eq!(group.children.len(), 1);
    let marker = &group.children[0];
    assert_eq!(marker.style.display, Display::Inline);
    assert_eq!(marker.style.width, w3cos_std::style::Dimension::Px(100.0));
    assert_eq!(marker.style.height, w3cos_std::style::Dimension::Px(100.0));
    assert_eq!(marker.style.color, w3cos_std::Color::rgb(0, 0, 238));
    assert_eq!(marker.style.text_decoration, TextDecoration::Underline);
    assert!(
        !matches!(marker.kind, ComponentKind::Text { .. }),
        "a sized generated marker keeps a principal box, got {:?}",
        marker.kind
    );
    assert!(
        !descendant_text(marker).is_empty(),
        "the principal box wraps the generated text run, got {:?}",
        marker.kind
    );
    // CSS text-decoration is not inherited; the decorating principal
    // box paints the UA underline. Copying it onto an inner run would
    // paint the same stripe twice. Walk through an inner anonymous IFC.
    fn assert_text_runs_have_no_copied_decoration(component: &w3cos_std::Component) {
        if matches!(component.kind, ComponentKind::Text { .. }) {
            assert_eq!(
                component.style.text_decoration,
                TextDecoration::None,
                "inner generated text must not copy the owner's decoration"
            );
        }
        for child in &component.children {
            assert_text_runs_have_no_copied_decoration(child);
        }
    }
    assert_text_runs_have_no_copied_decoration(marker);

    crate::stylesheet::clear_rules();
    clear_column_pseudo_counts();
}

#[test]
fn active_column_marker_follows_the_overflow5_scroll_algorithm() {
    // 001/006/007: 5 columns of 140 in a 420px port, range 280.
    let port_stride = (420.0, 140.0);
    assert_eq!(
        Document::select_active_column_scroll_marker(0.0, port_stride, 5),
        0
    );
    assert_eq!(
        Document::select_active_column_scroll_marker(280.0, port_stride, 5),
        4
    );
    assert_eq!(
        Document::select_active_column_scroll_marker(400.0, port_stride, 5),
        4
    );
    // 014: 3 columns of 100 in a 100px port, range 200, scrollTo(100, 0).
    let port_stride = (100.0, 100.0);
    assert_eq!(
        Document::select_active_column_scroll_marker(0.0, port_stride, 3),
        0
    );
    assert_eq!(
        Document::select_active_column_scroll_marker(100.0, port_stride, 3),
        1
    );
    assert_eq!(
        Document::select_active_column_scroll_marker(200.0, port_stride, 3),
        2
    );
}

#[test]
fn column_scroll_marker_counters_increment_in_column_order() {
    crate::stylesheet::clear_rules();
    clear_column_pseudo_counts();
    crate::stylesheet::register_rule(
        "#scroller",
        &[
            ("overflow", "hidden"),
            ("scroll-marker-group", "after"),
            ("counter-reset", "item"),
            ("columns", "1"),
            ("height", "100px"),
            ("width", "100px"),
        ],
    );
    crate::stylesheet::register_rule(
        "#scroller::scroll-marker-group",
        &[("display", "flex"), ("height", "100px"), ("width", "300px")],
    );
    crate::stylesheet::register_rule(
        "#scroller::column::scroll-marker",
        &[
            ("content", "counter(item)"),
            ("counter-increment", "item"),
            ("width", "100px"),
            ("height", "100px"),
        ],
    );

    let mut document = Document::new();
    let scroller = document.create_element("div");
    scroller.set_attribute(&mut document, "id", "scroller");
    for label in ["1", "2", "3"] {
        let item = document.create_element("div");
        item.set_attribute(&mut document, "class", "item");
        let text = document.create_text_node(label);
        item.append_child(&mut document, text);
        scroller.append_child(&mut document, item);
    }
    document.body().append_child(&mut document, scroller);
    assert!(set_column_pseudo_count(scroller.id, 3));

    let tree = document.to_component_tree();
    let group = find_marker_group(&tree).expect("group");
    let labels: Vec<String> = group.children.iter().map(descendant_text).collect();
    assert_eq!(
        labels,
        vec!["1".to_string(), "2".to_string(), "3".to_string()],
        "each ::column::scroll-marker should increment then use counter(item)"
    );

    crate::stylesheet::clear_rules();
    clear_column_pseudo_counts();
}

#[test]
fn scroll_marker_group_is_not_created_without_a_scroll_container() {
    crate::stylesheet::clear_rules();
    crate::stylesheet::register_rule("div", &[("scroll-marker-group", "after")]);
    crate::stylesheet::register_rule(
        "div::scroll-marker-group",
        &[
            ("background-color", "red"),
            ("display", "flex"),
            ("height", "100px"),
            ("width", "100px"),
        ],
    );
    let mut document = Document::new();
    let box_elem = document.create_element("div");
    document.body().append_child(&mut document, box_elem);
    let tree = document.to_component_tree();
    assert!(
        find_marker_group(&tree).is_none(),
        "non-scroller must not generate ::scroll-marker-group"
    );
    crate::stylesheet::clear_rules();
}

#[test]
fn unstyled_scroll_marker_group_is_size_contained() {
    crate::stylesheet::clear_rules();
    crate::stylesheet::register_rule(
        "div",
        &[("scroll-marker-group", "after"), ("overflow", "hidden")],
    );
    // No author display/size on the group: UA display is block and
    // Overflow 5 applies `contain: size !important`.
    crate::stylesheet::register_rule(
        "div::column::scroll-marker",
        &[
            ("content", "' '"),
            ("width", "50px"),
            ("height", "50px"),
            ("background", "blue"),
        ],
    );
    let mut document = Document::new();
    let scroller = document.create_element("div");
    document.body().append_child(&mut document, scroller);
    assert!(set_column_pseudo_count(scroller.id, 2));
    let tree = document.to_component_tree();
    let group = find_marker_group(&tree).expect("unstyled group is still generated");
    assert_eq!(group.style.display, Display::Block);
    assert_eq!(group.style.contain, Contain::Size);
    assert_eq!(group.children.len(), 2);
    crate::stylesheet::clear_rules();
    clear_column_pseudo_counts();
}

#[test]
fn author_display_none_suppresses_the_scroll_marker_group() {
    crate::stylesheet::clear_rules();
    crate::stylesheet::register_rule(
        "div",
        &[("scroll-marker-group", "after"), ("overflow", "hidden")],
    );
    crate::stylesheet::register_rule("div::scroll-marker-group", &[("display", "none")]);
    crate::stylesheet::register_rule(
        "div::column::scroll-marker",
        &[("content", "' '"), ("width", "50px"), ("height", "50px")],
    );
    let mut document = Document::new();
    let scroller = document.create_element("div");
    document.body().append_child(&mut document, scroller);
    assert!(set_column_pseudo_count(scroller.id, 2));
    let tree = document.to_component_tree();
    assert!(
        find_marker_group(&tree).is_none(),
        "author display:none must suppress the group and its markers"
    );
    crate::stylesheet::clear_rules();
    clear_column_pseudo_counts();
}

#[test]
fn child_scroll_marker_host_is_the_originating_element() {
    crate::stylesheet::clear_rules();
    crate::stylesheet::register_rule(
        "section",
        &[("scroll-marker-group", "before"), ("overflow", "auto")],
    );
    crate::stylesheet::register_rule(
        "section::scroll-marker-group",
        &[("display", "flex"), ("height", "20px"), ("width", "40px")],
    );
    crate::stylesheet::register_rule(
        "section>div::scroll-marker",
        &[
            ("content", "''"),
            ("width", "100px"),
            ("height", "20px"),
            ("display", "inline-block"),
        ],
    );
    let mut document = Document::new();
    let scroller = document.create_element("section");
    let target = document.create_element("div");
    scroller.append_child(&mut document, target);
    document.body().append_child(&mut document, scroller);
    let tree = document.to_component_tree();
    let group = find_marker_group(&tree).expect("group");
    assert_eq!(group.children.len(), 1);
    let origin = group.children[0]
        .style
        .custom_properties
        .as_ref()
        .and_then(|properties| properties.get(super::SCROLL_MARKER_ORIGIN))
        .cloned();
    assert_eq!(origin, Some(target.id.as_u32().to_string()));
    assert!(
        !matches!(
            group.children[0].on_click,
            w3cos_std::EventAction::NativeHost { .. }
        ),
        "marker must not alias the originating element's layout rect"
    );
    crate::stylesheet::clear_rules();
}
