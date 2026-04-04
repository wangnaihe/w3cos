use std::cell::RefCell;

use w3cos_dom::node::NodeId;
use w3cos_dom::Document;
use w3cos_std::EventAction;

thread_local! {
    static DOCUMENT: RefCell<Document> = RefCell::new(Document::new());
    static DOM_DIRTY: RefCell<bool> = RefCell::new(false);
}

pub fn with_document<R>(f: impl FnOnce(&Document) -> R) -> R {
    DOCUMENT.with(|d| f(&d.borrow()))
}

pub fn with_document_mut<R>(f: impl FnOnce(&mut Document) -> R) -> R {
    DOCUMENT.with(|d| f(&mut d.borrow_mut()))
}

fn mark_dom_dirty() {
    DOM_DIRTY.with(|d| *d.borrow_mut() = true);
}

pub fn is_document_dirty() -> bool {
    DOM_DIRTY.with(|d| *d.borrow())
}

pub fn clear_document_dirty() {
    DOM_DIRTY.with(|d| *d.borrow_mut() = false);
}

pub fn reset_document() {
    DOCUMENT.with(|d| *d.borrow_mut() = Document::new());
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

pub fn create_text_node(text: &str) -> u32 {
    with_document_mut(|doc| {
        let el = doc.create_text_node(text);
        el.id.as_u32()
    })
}

pub fn body_id() -> u32 {
    with_document(|doc| doc.body().id.as_u32())
}

pub fn append_child(parent: u32, child: u32) {
    with_document_mut(|doc| {
        doc.append_child(NodeId::from_u32(parent), NodeId::from_u32(child));
    });
    mark_dom_dirty();
}

pub fn remove_child(parent: u32, child: u32) {
    with_document_mut(|doc| {
        doc.remove_child(NodeId::from_u32(parent), NodeId::from_u32(child));
    });
    mark_dom_dirty();
}

pub fn insert_before(parent: u32, new_child: u32, ref_child: u32) {
    with_document_mut(|doc| {
        doc.insert_before(
            NodeId::from_u32(parent),
            NodeId::from_u32(new_child),
            NodeId::from_u32(ref_child),
        );
    });
    mark_dom_dirty();
}

pub fn set_attribute(node: u32, name: &str, value: &str) {
    with_document_mut(|doc| {
        let el = w3cos_dom::Element::new(NodeId::from_u32(node));
        el.set_attribute(doc, name, value);
    });
    mark_dom_dirty();
}

pub fn get_attribute(node: u32, name: &str) -> Option<String> {
    with_document(|doc| {
        let el = w3cos_dom::Element::new(NodeId::from_u32(node));
        el.get_attribute(doc, name).map(|s| s.to_string())
    })
}

pub fn set_text_content(node: u32, text: &str) {
    with_document_mut(|doc| {
        let el = w3cos_dom::Element::new(NodeId::from_u32(node));
        el.set_text_content(doc, text);
    });
    mark_dom_dirty();
}

pub fn get_text_content(node: u32) -> Option<String> {
    with_document(|doc| {
        let el = w3cos_dom::Element::new(NodeId::from_u32(node));
        el.text_content(doc).map(|s| s.to_string())
    })
}

pub fn set_style_property(node: u32, prop: &str, value: &str) {
    with_document_mut(|doc| {
        let el = w3cos_dom::Element::new(NodeId::from_u32(node));
        el.style_mut(doc).set_property(prop, value);
    });
    mark_dom_dirty();
}

pub fn class_list_add(node: u32, class: &str) {
    with_document_mut(|doc| {
        let el = w3cos_dom::Element::new(NodeId::from_u32(node));
        el.class_list_add(doc, class);
    });
    mark_dom_dirty();
}

pub fn class_list_remove(node: u32, class: &str) {
    with_document_mut(|doc| {
        let el = w3cos_dom::Element::new(NodeId::from_u32(node));
        el.class_list_remove(doc, class);
    });
    mark_dom_dirty();
}

pub fn class_list_toggle(node: u32, class: &str) -> bool {
    let result = with_document_mut(|doc| {
        let el = w3cos_dom::Element::new(NodeId::from_u32(node));
        el.class_list_toggle(doc, class)
    });
    mark_dom_dirty();
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

/// Build Component tree from the current DOM state (for rendering).
pub fn to_component_tree() -> w3cos_std::Component {
    with_document(|doc| doc.to_component_tree())
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
}
