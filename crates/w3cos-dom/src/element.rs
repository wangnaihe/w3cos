use crate::css_style::CSSStyleDeclaration;
use crate::document::Document;
use crate::events::{Event, EventHandler, EventType};
use crate::node::NodeId;

/// W3C Element API — the primary interface for DOM manipulation.
///
/// This is NOT a browser DOM. All operations compile to direct struct mutations
/// and signal updates. No GC, no reference counting, no runtime overhead.
///
/// Usage mirrors the W3C spec:
///   let el = document.create_element("div");
///   el.set_attribute(&mut doc, "id", "main");
///   el.style(&mut doc).set_property("display", "flex");
///   el.set_text_content(&mut doc, "Hello");
///   document.body().append_child(&mut doc, el);
pub struct Element {
    pub id: NodeId,
}

impl Element {
    pub fn new(id: NodeId) -> Self {
        Self { id }
    }

    // --- W3C Properties ---

    pub fn tag_name<'a>(&self, doc: &'a Document) -> &'a str {
        &doc.get_node(self.id).tag
    }

    pub fn text_content<'a>(&self, doc: &'a Document) -> Option<&'a str> {
        doc.get_node(self.id).text_content.as_deref()
    }

    pub fn set_text_content(&self, doc: &mut Document, text: &str) {
        doc.get_node_mut(self.id).text_content = Some(text.to_string());
        doc.mark_dirty(self.id);
    }

    // --- Children ---

    pub fn append_child(&self, doc: &mut Document, child: Element) {
        doc.append_child(self.id, child.id);
    }

    pub fn remove_child(&self, doc: &mut Document, child: &Element) {
        doc.remove_child(self.id, child.id);
    }

    pub fn children(&self, doc: &Document) -> Vec<Element> {
        doc.get_node(self.id).children.iter()
            .map(|&id| Element::new(id))
            .collect()
    }

    pub fn parent_element(&self, doc: &Document) -> Option<Element> {
        doc.get_node(self.id).parent.map(Element::new)
    }

    // --- Attributes ---

    pub fn set_attribute(&self, doc: &mut Document, name: &str, value: &str) {
        let node = doc.get_node_mut(self.id);
        if let Some(attr) = node.attributes.iter_mut().find(|(k, _)| k == name) {
            attr.1 = value.to_string();
        } else {
            node.attributes.push((name.to_string(), value.to_string()));
        }
        if name == "id" || name == "class" {
            doc.mark_dirty(self.id);
        }
    }

    pub fn get_attribute<'a>(&self, doc: &'a Document, name: &str) -> Option<&'a str> {
        doc.get_node(self.id).attributes.iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn remove_attribute(&self, doc: &mut Document, name: &str) {
        doc.get_node_mut(self.id).attributes.retain(|(k, _)| k != name);
    }

    // --- classList ---

    pub fn class_list_add(&self, doc: &mut Document, class: &str) {
        let node = doc.get_node_mut(self.id);
        if !node.class_list.contains(&class.to_string()) {
            node.class_list.push(class.to_string());
            doc.mark_dirty(self.id);
        }
    }

    pub fn class_list_remove(&self, doc: &mut Document, class: &str) {
        let node = doc.get_node_mut(self.id);
        node.class_list.retain(|c| c != class);
        doc.mark_dirty(self.id);
    }

    pub fn class_list_toggle(&self, doc: &mut Document, class: &str) -> bool {
        let node = doc.get_node(self.id);
        if node.class_list.contains(&class.to_string()) {
            self.class_list_remove(doc, class);
            false
        } else {
            self.class_list_add(doc, class);
            true
        }
    }

    pub fn class_list_contains(&self, doc: &Document, class: &str) -> bool {
        doc.get_node(self.id).class_list.contains(&class.to_string())
    }

    // --- Style ---

    pub fn style<'a>(&self, doc: &'a Document) -> &'a CSSStyleDeclaration {
        doc.get_style(self.id)
    }

    pub fn style_mut<'a>(&self, doc: &'a mut Document) -> &'a mut CSSStyleDeclaration {
        doc.mark_dirty(self.id);
        doc.get_style_mut(self.id)
    }

    // --- Events ---

    pub fn add_event_listener(&self, doc: &mut Document, event: &str, handler: EventHandler) {
        if let Some(event_type) = EventType::from_str(event) {
            doc.events.add(self.id, event_type, handler);
        }
    }

    pub fn remove_event_listeners(&self, doc: &mut Document) {
        doc.events.remove_all(self.id);
    }

    pub fn dispatch_event(&self, doc: &mut Document, event: &mut Event) {
        doc.events.dispatch(event);
    }
}

impl Clone for Element {
    fn clone(&self) -> Self {
        Self { id: self.id }
    }
}

impl Copy for Element {}
