use super::{ComponentPatch, Document};
use crate::element::Element;
use w3cos_std::Component;

fn same(a: &Component, b: &Component, path: &str) {
    assert_eq!(a.kind, b.kind, "kind at {path}");
    assert!(a.style == b.style, "style differs at {path}");
    assert_eq!(
        format!("{:?}", a.on_click),
        format!("{:?}", b.on_click),
        "on_click at {path}"
    );
    assert_eq!(a.children.len(), b.children.len(), "child count at {path}");
    for (index, (x, y)) in a.children.iter().zip(&b.children).enumerate() {
        same(x, y, &format!("{path}/{index}"));
    }
}

struct Fixture {
    document: Document,
    list: Element,
    rows: Vec<Element>,
}

/// body > #shell > [header, .list > N rows, footer]. Each row is a block with
/// a label span and a text child, like an editor line or a palette entry.
fn fixture(rows: usize) -> Fixture {
    crate::stylesheet::clear_rules();
    crate::stylesheet::register_rule(".hot", &[("background-color", "#336699")]);
    crate::stylesheet::register_rule(".row", &[("display", "block"), ("height", "20px")]);
    let mut document = Document::new();
    let shell = document.create_element("div");
    shell.set_attribute(&mut document, "id", "shell");
    document.body().append_child(&mut document, shell);
    let header = document.create_element("div");
    shell.append_child(&mut document, header);
    let header_text = document.create_text_node("header");
    header.append_child(&mut document, header_text);
    let list = document.create_element("div");
    list.set_attribute(&mut document, "class", "list");
    shell.append_child(&mut document, list);
    let mut built = Vec::new();
    for index in 0..rows {
        let row = document.create_element("div");
        row.set_attribute(&mut document, "class", "row");
        let label = document.create_element("span");
        let label_text = document.create_text_node(&format!("row {index}"));
        label.append_child(&mut document, label_text);
        row.append_child(&mut document, label);
        list.append_child(&mut document, row);
        built.push(row);
    }
    let footer = document.create_element("div");
    shell.append_child(&mut document, footer);
    let footer_text = document.create_text_node("footer");
    footer.append_child(&mut document, footer_text);
    Fixture {
        document,
        list,
        rows: built,
    }
}

fn patch_matches_fresh(
    document: &Document,
    tree: &mut Component,
    base: &super::ComponentBaseline,
) -> ComponentPatch {
    let outcome = document.patch_component_tree(tree, base);
    if !matches!(outcome, ComponentPatch::NeedsRebuild) {
        same(tree, &document.to_component_tree(), "root");
    }
    outcome
}

#[test]
fn untouched_document_needs_no_patch() {
    let f = fixture(30);
    let base = f.document.component_baseline();
    let mut tree = f.document.to_component_tree();
    assert_eq!(
        f.document.patch_component_tree(&mut tree, &base),
        ComponentPatch::Unchanged
    );
}

#[test]
fn text_edit_relowers_only_the_row() {
    let mut f = fixture(60);
    let mut tree = f.document.to_component_tree();
    let base = f.document.component_baseline();
    let label = Element::new(f.document.get_node(f.rows[17].id).first_child.unwrap());
    label.set_text_content(&mut f.document, "edited");

    f.document.reset_computed_style_counters();
    let outcome = f.document.patch_component_tree(&mut tree, &base);
    let patched_work = f.document.computed_style_calls_for_test();
    assert!(
        matches!(outcome, ComponentPatch::Patched { frontiers: 1 }),
        "{outcome:?}"
    );

    f.document.reset_computed_style_counters();
    let fresh = f.document.to_component_tree();
    let full_work = f.document.computed_style_calls_for_test();
    same(&tree, &fresh, "root");
    assert!(
        patched_work * 5 < full_work,
        "patch looked up {patched_work} styles, full lowering {full_work}"
    );
}

#[test]
fn structural_and_style_edits_match_a_fresh_lowering() {
    let mut f = fixture(40);
    let mut tree = f.document.to_component_tree();

    // class change
    let mut base = f.document.component_baseline();
    f.rows[5].class_list_add(&mut f.document, "hot");
    assert_eq!(
        patch_matches_fresh(&f.document, &mut tree, &base),
        ComponentPatch::Patched { frontiers: 1 },
        "a class change re-lowers only that row"
    );

    // inline style
    base = f.document.component_baseline();
    f.rows[9].set_attribute(&mut f.document, "style", "color: red; height: 33px");
    assert!(matches!(
        patch_matches_fresh(&f.document, &mut tree, &base),
        ComponentPatch::Patched { .. }
    ));

    // append a row
    base = f.document.component_baseline();
    let row = f.document.create_element("div");
    row.set_attribute(&mut f.document, "class", "row");
    let text = f.document.create_text_node("appended");
    row.append_child(&mut f.document, text);
    f.list.append_child(&mut f.document, row);
    assert!(matches!(
        patch_matches_fresh(&f.document, &mut tree, &base),
        ComponentPatch::Patched { .. }
    ));

    // remove a row
    base = f.document.component_baseline();
    f.list.remove_child(&mut f.document, &f.rows[3]);
    assert!(matches!(
        patch_matches_fresh(&f.document, &mut tree, &base),
        ComponentPatch::Patched { .. }
    ));

    // hide and show a row
    base = f.document.component_baseline();
    f.rows[11].set_attribute(&mut f.document, "style", "display: none");
    assert!(matches!(
        patch_matches_fresh(&f.document, &mut tree, &base),
        ComponentPatch::Patched { .. }
    ));
    base = f.document.component_baseline();
    f.rows[11].remove_attribute(&mut f.document, "style");
    assert!(matches!(
        patch_matches_fresh(&f.document, &mut tree, &base),
        ComponentPatch::Patched { .. }
    ));

    // a listener changes how a node is lowered
    base = f.document.component_baseline();
    f.rows[20].add_event_listener(&mut f.document, "click", Box::new(|_| {}));
    assert!(matches!(
        patch_matches_fresh(&f.document, &mut tree, &base),
        ComponentPatch::Patched { .. }
    ));
}

#[test]
fn document_wide_inputs_force_a_rebuild() {
    let mut f = fixture(10);
    let mut tree = f.document.to_component_tree();
    let base = f.document.component_baseline();
    crate::stylesheet::register_rule(".row", &[("color", "blue")]);
    assert_eq!(
        f.document.patch_component_tree(&mut tree, &base),
        ComponentPatch::NeedsRebuild
    );

    let base = f.document.component_baseline();
    f.document.set_font_size_minimums(11.0, 0.0);
    assert_eq!(
        f.document.patch_component_tree(&mut tree, &base),
        ComponentPatch::NeedsRebuild
    );

    // Detached edits never reach the lowered tree.
    let base = f.document.component_baseline();
    let detached = f.document.create_element("div");
    let text = f.document.create_text_node("x");
    detached.append_child(&mut f.document, text);
    text.set_text_content(&mut f.document, "y");
    assert_eq!(
        f.document.patch_component_tree(&mut tree, &base),
        ComponentPatch::Unchanged
    );
}

#[test]
fn mixed_inline_content_is_not_patched_unsafely() {
    let mut f = fixture(6);
    // A block with an inline run between two block children needs anonymous
    // block wrapping; editing next to it must not leave a stale wrapper.
    let mixed = f.document.create_element("div");
    f.list.append_child(&mut f.document, mixed);
    let before = f.document.create_element("div");
    let run = f.document.create_text_node("run");
    let after = f.document.create_element("div");
    mixed.append_child(&mut f.document, before);
    mixed.append_child(&mut f.document, run);
    mixed.append_child(&mut f.document, after);
    let mut tree = f.document.to_component_tree();
    let base = f.document.component_baseline();
    before.set_attribute(&mut f.document, "style", "height: 9px");
    let _ = patch_matches_fresh(&f.document, &mut tree, &base);
    let base = f.document.component_baseline();
    run.set_text_content(&mut f.document, "other run");
    let _ = patch_matches_fresh(&f.document, &mut tree, &base);
}

fn host_of(component: &Component) -> Option<u32> {
    match component.on_click {
        w3cos_std::EventAction::NativeHost { id, .. } => u32::try_from(id).ok(),
        _ => None,
    }
}

fn find_host<'a>(component: &'a mut Component, host: u32) -> Option<&'a mut Component> {
    if host_of(component) == Some(host) {
        return Some(component);
    }
    component
        .children
        .iter_mut()
        .find_map(|child| find_host(child, host))
}

#[test]
fn a_parent_changed_after_lowering_is_not_patched_around() {
    let mut f = fixture(20);
    let mut tree = f.document.to_component_tree();
    // A host pass (grafts, leading, margin collapsing) left the list component
    // different from what its style computes to. Patching a row inside it
    // would keep that state while a fresh lowering would not.
    let list = find_host(&mut tree, f.list.id.0).expect("list lowered");
    list.style.opacity = 0.5;
    let base = f.document.component_baseline();
    f.rows[4].class_list_add(&mut f.document, "hot");
    let outcome = patch_matches_fresh(&f.document, &mut tree, &base);
    // The row cannot be re-lowered under that parent, so the parent itself is
    // re-lowered (healing it); the result still equals a fresh lowering.
    assert_eq!(outcome, ComponentPatch::Patched { frontiers: 1 });
}

/// Random edits against a mixed tree. Whenever the patch claims success the
/// result must equal a fresh lowering; otherwise the caller rebuilds.
#[test]
fn random_edits_never_diverge_from_a_fresh_lowering() {
    let mut f = fixture(24);
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut tree = f.document.to_component_tree();
    let mut patched = 0;
    let mut rebuilt = 0;
    for step in 0..120 {
        let base = f.document.component_baseline();
        let row = f.rows[(next() % f.rows.len() as u64) as usize];
        match next() % 6 {
            0 => {
                let label = Element::new(f.document.get_node(row.id).first_child.unwrap());
                label.set_text_content(&mut f.document, &format!("t{step}"));
            }
            1 => row.set_attribute(
                &mut f.document,
                "style",
                &format!("height: {}px", 10 + step % 30),
            ),
            2 => {
                row.class_list_toggle(&mut f.document, "hot");
            }
            3 => {
                let extra = f.document.create_element("span");
                let text = f.document.create_text_node(&format!("x{step}"));
                extra.append_child(&mut f.document, text);
                row.append_child(&mut f.document, extra);
            }
            4 => {
                let style = if step % 2 == 0 {
                    "display: none"
                } else {
                    "display: block"
                };
                row.set_attribute(&mut f.document, "style", style);
            }
            _ => {
                let bare = f.document.create_text_node("bare text");
                row.append_child(&mut f.document, bare);
            }
        }
        match patch_matches_fresh(&f.document, &mut tree, &base) {
            ComponentPatch::NeedsRebuild => {
                rebuilt += 1;
                tree = f.document.to_component_tree();
            }
            ComponentPatch::Patched { .. } => patched += 1,
            ComponentPatch::Unchanged => {}
        }
    }
    assert!(patched > 60, "patched {patched}, rebuilt {rebuilt}");
}

#[test]
fn flex_item_under_mixed_flex_parent_is_patched() {
    // A flex container with a text node between two items is still safe to
    // patch: flex items do not share anonymous block wrappers.
    crate::stylesheet::clear_rules();
    crate::stylesheet::register_rule(
        ".shell",
        &[("display", "flex"), ("flex-direction", "column")],
    );
    crate::stylesheet::register_rule(".row", &[("display", "block"), ("height", "20px")]);
    let mut document = Document::new();
    let shell = document.create_element("div");
    shell.set_attribute(&mut document, "class", "shell");
    document.body().append_child(&mut document, shell);
    let first = document.create_element("div");
    first.set_attribute(&mut document, "class", "row");
    let spacer = document.create_text_node(" ");
    let second = document.create_element("div");
    second.set_attribute(&mut document, "class", "row");
    let label = document.create_element("span");
    let label_text = document.create_text_node("item");
    label.append_child(&mut document, label_text);
    second.append_child(&mut document, label);
    shell.append_child(&mut document, first);
    shell.append_child(&mut document, spacer);
    shell.append_child(&mut document, second);

    let mut tree = document.to_component_tree();
    let base = document.component_baseline();
    second.set_attribute(&mut document, "style", "height: 28px");
    let outcome = patch_matches_fresh(&document, &mut tree, &base);
    assert!(
        matches!(outcome, ComponentPatch::Patched { frontiers: 1 }),
        "expected a one-frontier flex patch, got {outcome:?}"
    );
}

#[test]
fn grid_item_under_mixed_grid_parent_is_patched() {
    crate::stylesheet::clear_rules();
    crate::stylesheet::register_rule(".shell", &[("display", "grid")]);
    crate::stylesheet::register_rule(".row", &[("display", "block"), ("height", "20px")]);
    let mut document = Document::new();
    let shell = document.create_element("div");
    shell.set_attribute(&mut document, "class", "shell");
    document.body().append_child(&mut document, shell);
    let first = document.create_element("div");
    first.set_attribute(&mut document, "class", "row");
    let spacer = document.create_text_node("\n");
    let second = document.create_element("div");
    second.set_attribute(&mut document, "class", "row");
    shell.append_child(&mut document, first);
    shell.append_child(&mut document, spacer);
    shell.append_child(&mut document, second);

    let mut tree = document.to_component_tree();
    let base = document.component_baseline();
    first.set_attribute(&mut document, "style", "height: 40px");
    let outcome = patch_matches_fresh(&document, &mut tree, &base);
    assert!(
        matches!(outcome, ComponentPatch::Patched { .. }),
        "expected a grid patch, got {outcome:?}"
    );
}

#[test]
fn flex_patch_keeps_item_order_and_geometry_tokens() {
    // Semantic guard: after patching one flex item, the lowered tree must keep
    // the same host order as a fresh lowering (text spacer stays between items;
    // edited item keeps its place). Geometry equality is covered by `same`.
    crate::stylesheet::clear_rules();
    crate::stylesheet::register_rule(".shell", &[("display", "flex"), ("flex-direction", "row")]);
    crate::stylesheet::register_rule(
        ".row",
        &[("display", "block"), ("width", "40px"), ("height", "20px")],
    );
    let mut document = Document::new();
    let shell = document.create_element("div");
    shell.set_attribute(&mut document, "class", "shell");
    document.body().append_child(&mut document, shell);
    let left = document.create_element("div");
    left.set_attribute(&mut document, "class", "row");
    left.set_attribute(&mut document, "id", "left");
    let spacer = document.create_text_node(" between ");
    let right = document.create_element("div");
    right.set_attribute(&mut document, "class", "row");
    right.set_attribute(&mut document, "id", "right");
    shell.append_child(&mut document, left);
    shell.append_child(&mut document, spacer);
    shell.append_child(&mut document, right);

    let mut tree = document.to_component_tree();
    let base = document.component_baseline();
    right.set_attribute(&mut document, "style", "width: 64px; height: 20px");
    assert!(matches!(
        patch_matches_fresh(&document, &mut tree, &base),
        ComponentPatch::Patched { .. }
    ));

    fn host_ids(component: &Component, out: &mut Vec<Option<u32>>) {
        out.push(match component.on_click {
            w3cos_std::EventAction::NativeHost { id, .. } => u32::try_from(id).ok(),
            _ => None,
        });
        for child in &component.children {
            host_ids(child, out);
        }
    }
    let mut patched_ids = Vec::new();
    host_ids(&tree, &mut patched_ids);
    let mut fresh_ids = Vec::new();
    host_ids(&document.to_component_tree(), &mut fresh_ids);
    assert_eq!(
        patched_ids, fresh_ids,
        "flex patch must preserve host order"
    );
    let left_pos = patched_ids.iter().position(|h| *h == Some(left.id.0));
    let right_pos = patched_ids.iter().position(|h| *h == Some(right.id.0));
    assert!(
        left_pos.is_some() && right_pos.is_some() && left_pos < right_pos,
        "left item must still precede right after patch: {patched_ids:?}"
    );
}
