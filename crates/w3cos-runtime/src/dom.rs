use std::cell::{Cell, RefCell};

use w3cos_dom::Document;
use w3cos_dom::node::NodeId;
use w3cos_std::EventAction;

thread_local! {
    static DOCUMENT: RefCell<Document> = RefCell::new(Document::new());
    static DOM_DIRTY: RefCell<bool> = RefCell::new(false);
    static DOM_MUTATION_GENERATION: Cell<u64> = const { Cell::new(0) };
    static SCROLL_REQUESTS: RefCell<Vec<(u32, Option<f32>, Option<f32>)>> = const {
        RefCell::new(Vec::new())
    };
}

pub fn with_document<R>(f: impl FnOnce(&Document) -> R) -> R {
    DOCUMENT.with(|d| f(&d.borrow()))
}

pub fn with_document_mut<R>(f: impl FnOnce(&mut Document) -> R) -> R {
    DOCUMENT.with(|d| f(&mut d.borrow_mut()))
}

pub(crate) fn mark_dom_dirty() {
    DOM_DIRTY.with(|d| *d.borrow_mut() = true);
    DOM_MUTATION_GENERATION.with(|generation| {
        generation.set(generation.get().wrapping_add(1));
    });
}

pub(crate) fn mutation_generation() -> u64 {
    DOM_MUTATION_GENERATION.with(Cell::get)
}

pub(crate) fn set_image_render_source(node: u32, source: Option<&str>) {
    with_document_mut(|document| {
        document.set_image_render_source(NodeId::from_u32(node), source);
    });
    mark_dom_dirty();
}

pub fn is_document_dirty() -> bool {
    DOM_DIRTY.with(|d| *d.borrow())
}

pub fn clear_document_dirty() {
    DOM_DIRTY.with(|d| *d.borrow_mut() = false);
}

pub fn reset_document() {
    DOCUMENT.with(|d| *d.borrow_mut() = Document::new());
    DOM_MUTATION_GENERATION.with(|generation| generation.set(0));
    SCROLL_REQUESTS.with(|requests| requests.borrow_mut().clear());
    clear_document_dirty();
}

// ---------------------------------------------------------------------------
// W3C-style DOM API wrappers (operate on thread-local Document)
// NodeId exposed as u32 to compiled code.
// ---------------------------------------------------------------------------

pub fn create_element(tag: &str) -> u32 {
    with_document_mut(|doc| {
        let el = doc.create_element(tag);
        el.id.as_u32()
    })
}

pub fn set_html_element(node: u32, is_html_element: bool) {
    with_document_mut(|doc| {
        doc.set_html_element(NodeId::from_u32(node), is_html_element);
    });
}

pub fn create_text_node(text: &str) -> u32 {
    with_document_mut(|doc| {
        let el = doc.create_text_node(text);
        el.id.as_u32()
    })
}

pub fn body_id() -> u32 {
    with_document(|doc| doc.body().id.as_u32())
}

pub(crate) fn set_render_body(node: u32) {
    with_document_mut(|document| document.set_render_body(NodeId::from_u32(node)));
    mark_dom_dirty();
}

pub(crate) fn set_html_document(html_document: bool) {
    with_document_mut(|document| document.set_html_document(html_document));
}

pub fn append_child(parent: u32, child: u32) {
    let old_parent = parent_node(child);
    let old_previous = previous_sibling(child);
    let old_next = next_sibling(child);
    with_document_mut(|doc| {
        doc.append_child(NodeId::from_u32(parent), NodeId::from_u32(child));
    });
    mark_dom_dirty();
    crate::jsdom::run_media_source_insertion_steps(parent, child);
    if let Some(old_parent) = old_parent {
        crate::observers_web::notify_child_list(old_parent, &[], &[child], old_previous, old_next);
    }
    crate::observers_web::notify_child_list(
        parent,
        &[child],
        &[],
        previous_sibling(child),
        next_sibling(child),
    );
    #[cfg(feature = "dynamic-js")]
    crate::dynamic_script::notify_node_inserted(child);
    #[cfg(feature = "dynamic-js")]
    crate::dynamic_script::notify_script_mutated(parent);
}

pub fn remove_child(parent: u32, child: u32) {
    let previous = previous_sibling(child);
    let next = next_sibling(child);
    let was_child = parent_node(child) == Some(parent);
    with_document_mut(|doc| {
        doc.remove_child(NodeId::from_u32(parent), NodeId::from_u32(child));
    });
    mark_dom_dirty();
    if was_child {
        crate::observers_web::notify_child_list(parent, &[], &[child], previous, next);
        #[cfg(feature = "dynamic-js")]
        crate::dynamic_script::notify_node_removed(child);
        #[cfg(feature = "dynamic-js")]
        crate::dynamic_script::notify_script_mutated(parent);
    }
}

pub fn insert_before(parent: u32, new_child: u32, ref_child: u32) {
    let old_parent = parent_node(new_child);
    let old_previous = previous_sibling(new_child);
    let old_next = next_sibling(new_child);
    with_document_mut(|doc| {
        doc.insert_before(
            NodeId::from_u32(parent),
            NodeId::from_u32(new_child),
            NodeId::from_u32(ref_child),
        );
    });
    mark_dom_dirty();
    crate::jsdom::run_media_source_insertion_steps(parent, new_child);
    if let Some(old_parent) = old_parent {
        crate::observers_web::notify_child_list(
            old_parent,
            &[],
            &[new_child],
            old_previous,
            old_next,
        );
    }
    crate::observers_web::notify_child_list(
        parent,
        &[new_child],
        &[],
        previous_sibling(new_child),
        next_sibling(new_child),
    );
    #[cfg(feature = "dynamic-js")]
    crate::dynamic_script::notify_node_inserted(new_child);
    #[cfg(feature = "dynamic-js")]
    crate::dynamic_script::notify_script_mutated(parent);
}

pub fn set_attribute(node: u32, name: &str, value: &str) {
    let old_value = get_attribute(node, name);
    with_document_mut(|doc| {
        let el = w3cos_dom::Element::new(NodeId::from_u32(node));
        el.set_attribute(doc, name, value);
    });
    mark_dom_dirty();
    crate::observers_web::notify_attribute(node, name, old_value.as_deref());
    if old_value.as_deref() != Some(value) {
        #[cfg(feature = "dynamic-js")]
        if matches!(
            name.to_ascii_lowercase().as_str(),
            "src"
                | "href"
                | "rel"
                | "type"
                | "media"
                | "disabled"
                | "integrity"
                | "referrerpolicy"
                | "crossorigin"
                | "srcset"
                | "sizes"
        ) {
            crate::dynamic_script::notify_script_mutated(node);
        }
    }
}

pub fn set_attribute_ns(node: u32, namespace: Option<&str>, qualified_name: &str, value: &str) {
    let namespace = namespace.filter(|namespace| !namespace.is_empty());
    let (prefix, local_name) = qualified_name
        .split_once(':')
        .map_or((None, qualified_name), |(prefix, local_name)| {
            (Some(prefix), local_name)
        });
    set_attribute_ns_parts(node, namespace, qualified_name, prefix, local_name, value);
}

pub(crate) fn set_attribute_ns_parts(
    node: u32,
    namespace: Option<&str>,
    qualified_name: &str,
    prefix: Option<&str>,
    local_name: &str,
    value: &str,
) {
    let old_value = get_attribute_ns(node, namespace, local_name);
    with_document_mut(|doc| {
        let el = w3cos_dom::Element::new(NodeId::from_u32(node));
        el.set_attribute_ns(doc, namespace, qualified_name, prefix, local_name, value);
    });
    mark_dom_dirty();
    crate::observers_web::notify_attribute_ns(node, local_name, namespace, old_value.as_deref());
    if old_value.as_deref() != Some(value) {
        #[cfg(feature = "dynamic-js")]
        if namespace.is_none() && matches!(local_name.to_ascii_lowercase().as_str(), "src" | "type")
        {
            crate::dynamic_script::notify_script_mutated(node);
        }
    }
}

pub fn get_attribute(node: u32, name: &str) -> Option<String> {
    with_document(|doc| {
        let el = w3cos_dom::Element::new(NodeId::from_u32(node));
        el.get_attribute(doc, name).map(|s| s.to_string())
    })
}

pub fn get_attribute_ns(node: u32, namespace: Option<&str>, local_name: &str) -> Option<String> {
    let namespace = namespace.filter(|namespace| !namespace.is_empty());
    with_document(|doc| {
        let el = w3cos_dom::Element::new(NodeId::from_u32(node));
        el.get_attribute_ns(doc, namespace, local_name)
            .map(str::to_string)
    })
}

pub fn set_text_content(node: u32, text: &str) {
    let old_text = get_text_content(node);
    let old_children = children(node);
    let character_data = matches!(node_type(node), 3 | 4 | 7 | 8);
    with_document_mut(|doc| {
        let el = w3cos_dom::Element::new(NodeId::from_u32(node));
        el.set_text_content(doc, text);
    });
    mark_dom_dirty();
    #[cfg(feature = "dynamic-js")]
    for child in &old_children {
        crate::dynamic_script::notify_node_removed(*child);
    }
    if character_data {
        crate::observers_web::notify_character_data(node, old_text.as_deref());
        if old_text.as_deref() != Some(text) {
            #[cfg(feature = "dynamic-js")]
            crate::dynamic_script::notify_script_mutated(node);
        }
        return;
    }
    if old_text.as_deref() == Some(text) {
        return;
    }
    let new_children = children(node);
    if old_children.is_empty() && new_children.is_empty() {
        return;
    }
    crate::observers_web::notify_child_list(node, &new_children, &old_children, None, None);
    #[cfg(feature = "dynamic-js")]
    crate::dynamic_script::notify_script_mutated(node);
}

pub fn get_text_content(node: u32) -> Option<String> {
    with_document(|doc| {
        let el = w3cos_dom::Element::new(NodeId::from_u32(node));
        el.text_content(doc).map(|s| s.to_string())
    })
}

pub fn get_descendant_text_content(node: u32) -> String {
    with_document(|doc| doc.descendant_text_content(NodeId::from_u32(node)))
}

pub fn set_style_property(node: u32, prop: &str, value: &str) {
    let old_value = get_attribute(node, "style");
    with_document_mut(|doc| {
        let el = w3cos_dom::Element::new(NodeId::from_u32(node));
        el.style_mut(doc).set_property(prop, value);
    });
    mark_dom_dirty();
    let _ = (prop, value);
    crate::observers_web::notify_attribute(node, "style", old_value.as_deref());
}

pub fn class_list_add(node: u32, class: &str) {
    let old_value = get_attribute(node, "class");
    with_document_mut(|doc| {
        let el = w3cos_dom::Element::new(NodeId::from_u32(node));
        el.class_list_add(doc, class);
    });
    mark_dom_dirty();
    if get_attribute(node, "class") != old_value {
        crate::observers_web::notify_attribute(node, "class", old_value.as_deref());
    }
}

pub fn class_list_remove(node: u32, class: &str) {
    let old_value = get_attribute(node, "class");
    with_document_mut(|doc| {
        let el = w3cos_dom::Element::new(NodeId::from_u32(node));
        el.class_list_remove(doc, class);
    });
    mark_dom_dirty();
    if get_attribute(node, "class") != old_value {
        crate::observers_web::notify_attribute(node, "class", old_value.as_deref());
    }
}

pub fn class_list_toggle(node: u32, class: &str) -> bool {
    let old_value = get_attribute(node, "class");
    let result = with_document_mut(|doc| {
        let el = w3cos_dom::Element::new(NodeId::from_u32(node));
        el.class_list_toggle(doc, class)
    });
    mark_dom_dirty();
    if get_attribute(node, "class") != old_value {
        crate::observers_web::notify_attribute(node, "class", old_value.as_deref());
    }
    result
}

pub fn add_event_listener(node: u32, event: &str, action: EventAction) {
    with_document_mut(|doc| {
        let el = w3cos_dom::Element::new(NodeId::from_u32(node));
        el.add_event_listener(
            doc,
            event,
            Box::new(move |_ev| {
                crate::state::execute_action(&action);
            }),
        );
    });
}

pub fn query_selector(selector: &str) -> Option<u32> {
    with_document(|doc| doc.query_selector(selector).map(|el| el.id.as_u32()))
}

pub fn query_selector_all(selector: &str) -> Vec<u32> {
    with_document(|doc| {
        doc.query_selector_all(selector)
            .iter()
            .map(|el| el.id.as_u32())
            .collect()
    })
}

pub fn get_element_by_id(id: &str) -> Option<u32> {
    with_document(|doc| doc.get_element_by_id(id).map(|el| el.id.as_u32()))
}

pub fn computed_style_property(node: u32, property: &str) -> String {
    with_document(|doc| {
        let element = w3cos_dom::Element::new(NodeId::from_u32(node));
        element.get_computed_style(doc).get_property(property)
    })
}

pub fn computed_pseudo_style_property(node: u32, pseudo: &str, property: &str) -> String {
    with_document(|doc| {
        let style = doc.computed_pseudo_style_for(NodeId::from_u32(node), pseudo);
        w3cos_dom::css_style::CSSStyleDeclaration::from_style(style).get_property(property)
    })
}

pub fn children(node: u32) -> Vec<u32> {
    with_document(|doc| {
        doc.children_ids(NodeId::from_u32(node))
            .iter()
            .map(|id| id.as_u32())
            .collect()
    })
}

pub fn parent_node(node: u32) -> Option<u32> {
    with_document(|doc| {
        doc.get_node(NodeId::from_u32(node))
            .parent
            .map(|id| id.as_u32())
    })
}

pub fn tag_name(node: u32) -> String {
    with_document(|doc| doc.get_node(NodeId::from_u32(node)).tag.as_str())
}

pub fn node_count() -> usize {
    with_document(|doc| doc.node_count())
}

// ── Phase 1 additions ──

pub fn replace_child(parent: u32, new_child: u32, old_child: u32) {
    if parent_node(old_child) != Some(parent) {
        return;
    }
    if new_child == old_child {
        let next = next_sibling(old_child);
        remove_child(parent, old_child);
        match next {
            Some(next) => insert_before(parent, new_child, next),
            None => append_child(parent, new_child),
        }
        return;
    }
    if let Some(old_parent) = parent_node(new_child) {
        remove_child(old_parent, new_child);
    }
    let previous = previous_sibling(old_child);
    let next = next_sibling(old_child);
    with_document_mut(|doc| {
        doc.replace_child(
            NodeId::from_u32(parent),
            NodeId::from_u32(new_child),
            NodeId::from_u32(old_child),
        );
    });
    mark_dom_dirty();
    crate::jsdom::run_media_source_insertion_steps(parent, new_child);
    crate::observers_web::notify_child_list(parent, &[new_child], &[old_child], previous, next);
    #[cfg(feature = "dynamic-js")]
    {
        crate::dynamic_script::notify_node_removed(old_child);
        crate::dynamic_script::notify_node_inserted(new_child);
    }
}

pub fn clone_node(node: u32, deep: bool) -> u32 {
    with_document_mut(|doc| doc.clone_node(NodeId::from_u32(node), deep).as_u32())
}

pub fn matches_selector(node: u32, selector: &str) -> Result<bool, ()> {
    with_document(|doc| doc.matches_selector(NodeId::from_u32(node), selector))
}

pub fn matches_selector_with_target(
    node: u32,
    selector: &str,
    target_id: Option<&str>,
) -> Result<bool, ()> {
    with_document(|doc| {
        doc.matches_selector_with_target(NodeId::from_u32(node), selector, target_id)
    })
}

pub fn matches_selector_relative_to_scope(
    node: u32,
    selector: &str,
    scope: u32,
    target_id: Option<&str>,
) -> Result<bool, ()> {
    with_document(|doc| {
        doc.matches_selector_relative_to_scope(
            NodeId::from_u32(node),
            selector,
            NodeId::from_u32(scope),
            target_id,
        )
    })
}

pub fn create_document_fragment() -> u32 {
    with_document_mut(|doc| doc.create_document_fragment().id.as_u32())
}

pub fn create_comment(text: &str) -> u32 {
    with_document_mut(|doc| doc.create_comment(text).id.as_u32())
}

pub fn create_cdata_section(text: &str) -> u32 {
    with_document_mut(|doc| doc.create_cdata_section(text).id.as_u32())
}

pub fn create_processing_instruction(target: &str, data: &str) -> u32 {
    with_document_mut(|doc| doc.create_processing_instruction(target, data).id.as_u32())
}

pub fn create_document_type(name: &str) -> u32 {
    with_document_mut(|doc| doc.create_document_type(name).id.as_u32())
}

pub fn get_elements_by_tag_name(tag: &str) -> Vec<u32> {
    with_document(|doc| {
        doc.get_elements_by_tag_name(tag)
            .iter()
            .map(|el| el.id.as_u32())
            .collect()
    })
}

pub fn get_elements_by_class_name(class: &str) -> Vec<u32> {
    with_document(|doc| {
        doc.get_elements_by_class_name(class)
            .iter()
            .map(|el| el.id.as_u32())
            .collect()
    })
}

pub fn next_sibling(node: u32) -> Option<u32> {
    with_document(|doc| {
        doc.get_node(NodeId::from_u32(node))
            .next_sibling
            .map(|id| id.as_u32())
    })
}

pub fn previous_sibling(node: u32) -> Option<u32> {
    with_document(|doc| {
        doc.get_node(NodeId::from_u32(node))
            .prev_sibling
            .map(|id| id.as_u32())
    })
}

pub fn first_child(node: u32) -> Option<u32> {
    with_document(|doc| {
        doc.get_node(NodeId::from_u32(node))
            .first_child
            .map(|id| id.as_u32())
    })
}

pub fn last_child(node: u32) -> Option<u32> {
    with_document(|doc| {
        doc.get_node(NodeId::from_u32(node))
            .last_child
            .map(|id| id.as_u32())
    })
}

pub fn node_type(node: u32) -> u16 {
    with_document(|doc| doc.get_node(NodeId::from_u32(node)).node_type.as_u16())
}

pub fn inner_text(node: u32) -> String {
    with_document(|doc| {
        let el = w3cos_dom::Element::new(NodeId::from_u32(node));
        el.inner_text(doc)
    })
}

pub fn outer_html(node: u32) -> String {
    with_document(|doc| {
        let el = w3cos_dom::Element::new(NodeId::from_u32(node));
        el.outer_html(doc)
    })
}

// ── jsdom bridge helpers ──
// Small additive wrappers used by `crate::jsdom` (Value-level DOM bridge).
// They follow the same conventions as the wrappers above: u32 node ids and
// DOM_DIRTY marking for mutations.

/// Force the DOM dirty flag on. Used by the jsdom bridge when it mutates the
/// document through `with_document_mut` on paths that have no wrapper here.
pub fn touch_document() {
    mark_dom_dirty();
}

pub fn remove_attribute(node: u32, name: &str) {
    let old_value = get_attribute(node, name);
    with_document_mut(|doc| {
        let el = w3cos_dom::Element::new(NodeId::from_u32(node));
        el.remove_attribute(doc, name);
    });
    mark_dom_dirty();
    if old_value.is_some() {
        crate::observers_web::notify_attribute(node, name, old_value.as_deref());
        #[cfg(feature = "dynamic-js")]
        if matches!(
            name.to_ascii_lowercase().as_str(),
            "src"
                | "href"
                | "rel"
                | "type"
                | "media"
                | "disabled"
                | "integrity"
                | "referrerpolicy"
                | "crossorigin"
                | "srcset"
                | "sizes"
        ) {
            crate::dynamic_script::notify_script_mutated(node);
        }
    }
}

pub fn remove_attribute_ns(node: u32, namespace: Option<&str>, local_name: &str) {
    let namespace = namespace.filter(|namespace| !namespace.is_empty());
    let previous = get_attribute_ns(node, namespace, local_name);
    let removed = with_document_mut(|doc| {
        let el = w3cos_dom::Element::new(NodeId::from_u32(node));
        el.remove_attribute_ns(doc, namespace, local_name)
    });
    if removed {
        mark_dom_dirty();
        crate::observers_web::notify_attribute_ns(node, local_name, namespace, previous.as_deref());
    }
}

pub fn has_attribute(node: u32, name: &str) -> bool {
    with_document(|doc| {
        let el = w3cos_dom::Element::new(NodeId::from_u32(node));
        el.get_attribute(doc, name).is_some()
    })
}

pub fn has_attribute_ns(node: u32, namespace: Option<&str>, local_name: &str) -> bool {
    get_attribute_ns(node, namespace, local_name).is_some()
}

pub fn class_name(node: u32) -> String {
    with_document(|doc| {
        let el = w3cos_dom::Element::new(NodeId::from_u32(node));
        el.class_name(doc)
    })
}

pub fn set_class_name(node: u32, name: &str) {
    with_document_mut(|doc| {
        let el = w3cos_dom::Element::new(NodeId::from_u32(node));
        el.set_class_name(doc, name);
    });
    mark_dom_dirty();
}

pub fn class_list_contains(node: u32, class: &str) -> bool {
    with_document(|doc| {
        let el = w3cos_dom::Element::new(NodeId::from_u32(node));
        el.class_list_contains(doc, class)
    })
}

/// W3C `Node.nodeName` — uppercase tag for elements, `#text`/`#comment`/...
pub fn node_name(node: u32) -> String {
    with_document(|doc| doc.get_node(NodeId::from_u32(node)).node_name())
}

/// W3C `Node.isConnected` — true when the node is attached to the document tree.
pub fn is_connected(node: u32) -> bool {
    with_document(|doc| {
        let el = w3cos_dom::Element::new(NodeId::from_u32(node));
        el.is_connected(doc)
    })
}

/// Scroll offset `(scroll_left, scroll_top)` for a node.
pub fn get_scroll_offset(node: u32) -> (f32, f32) {
    with_document(|doc| doc.get_scroll(NodeId::from_u32(node)))
}

/// Set scroll offsets; pass `None` to leave an axis unchanged.
pub fn set_scroll_offset(node: u32, left: Option<f32>, top: Option<f32>) {
    with_document_mut(|doc| doc.set_scroll(NodeId::from_u32(node), left, top));
    SCROLL_REQUESTS.with(|requests| requests.borrow_mut().push((node, left, top)));
}

/// Synchronize a clamped native offset into CSSOM without queuing another request.
pub fn sync_scroll_offset(node: u32, left: Option<f32>, top: Option<f32>) {
    with_document_mut(|doc| doc.set_scroll(NodeId::from_u32(node), left, top));
}

pub fn take_scroll_requests() -> Vec<(u32, Option<f32>, Option<f32>)> {
    SCROLL_REQUESTS.with(|requests| std::mem::take(&mut *requests.borrow_mut()))
}

/// W3C `Element.getBoundingClientRect` — zeros until the layout engine runs.
pub fn bounding_rect(node: u32) -> w3cos_dom::DOMRect {
    with_document(|doc| {
        let el = w3cos_dom::Element::new(NodeId::from_u32(node));
        el.get_bounding_client_rect(doc)
    })
}

/// Build Component tree from the current DOM state (for rendering).
pub fn to_component_tree() -> w3cos_std::Component {
    let mut tree = with_document(|doc| doc.to_component_tree());
    #[cfg(feature = "dynamic-js")]
    crate::jsdom::graft_shadow_component_subtrees(&mut tree);
    crate::jsdom::graft_frame_component_subtrees(&mut tree);
    tree
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_append() {
        reset_document();
        let div = create_element("div");
        let body = body_id();
        append_child(body, div);
        assert_eq!(children(body).len(), 1);
        assert_eq!(tag_name(div), "div");
    }

    #[test]
    fn set_and_get_text() {
        reset_document();
        let p = create_element("p");
        set_text_content(p, "Hello W3C OS");
        assert_eq!(get_text_content(p), Some("Hello W3C OS".to_string()));
    }

    #[test]
    fn style_property() {
        reset_document();
        let div = create_element("div");
        set_style_property(div, "display", "flex");
        set_style_property(div, "gap", "10px");
        let body = body_id();
        append_child(body, div);
        assert!(is_document_dirty());
    }

    #[test]
    fn query_selectors() {
        reset_document();
        let div = create_element("div");
        set_attribute(div, "id", "main");
        class_list_add(div, "container");
        append_child(body_id(), div);

        assert_eq!(get_element_by_id("main"), Some(div));
        assert_eq!(query_selector("#main"), Some(div));
        assert_eq!(query_selector(".container"), Some(div));
        assert_eq!(query_selector("div"), Some(div));
    }

    #[test]
    fn remove_child_works() {
        reset_document();
        let div = create_element("div");
        let body = body_id();
        append_child(body, div);
        assert_eq!(children(body).len(), 1);
        remove_child(body, div);
        assert_eq!(children(body).len(), 0);
    }

    #[test]
    fn to_component_tree_works() {
        reset_document();
        let div = create_element("div");
        set_style_property(div, "gap", "20px");
        let text = create_text_node("Hello");
        append_child(div, text);
        append_child(body_id(), div);

        let tree = to_component_tree();
        assert!(!tree.children.is_empty());
    }

    #[test]
    fn event_listener_with_action() {
        reset_document();
        let btn = create_element("button");
        append_child(body_id(), btn);
        add_event_listener(btn, "click", EventAction::Increment(0));
    }

    #[test]
    fn dirty_tracking() {
        reset_document();
        clear_document_dirty();
        assert!(!is_document_dirty());
        let div = create_element("div");
        append_child(body_id(), div);
        assert!(is_document_dirty());
        clear_document_dirty();
        assert!(!is_document_dirty());
    }

    #[test]
    fn cssom_scroll_write_queues_a_native_request() {
        reset_document();
        let div = create_element("div");
        set_scroll_offset(div, None, Some(120.0));
        assert_eq!(get_scroll_offset(div), (0.0, 120.0));
        assert_eq!(take_scroll_requests(), vec![(div, None, Some(120.0))]);
        sync_scroll_offset(div, None, Some(80.0));
        assert_eq!(get_scroll_offset(div), (0.0, 80.0));
        assert!(take_scroll_requests().is_empty());
    }
}
