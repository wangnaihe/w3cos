use crate::atom::Atom;
use crate::css_style::CSSStyleDeclaration;
use crate::document::Document;
use crate::events::{Event, EventHandler, EventType};
use crate::node::NodeId;

/// W3C Element API — the primary interface for DOM manipulation.
///
/// Performance: all operations are O(1) through arena access + interned atoms.
pub struct Element {
    pub id: NodeId,
}

impl Element {
    pub fn new(id: NodeId) -> Self {
        Self { id }
    }

    pub fn tag_name(&self, doc: &Document) -> String {
        doc.get_node(self.id).tag.as_str()
    }

    pub fn text_content<'a>(&self, doc: &'a Document) -> Option<&'a str> {
        doc.get_node(self.id).text_content.as_deref()
    }

    pub fn set_text_content(&self, doc: &mut Document, text: &str) {
        doc.get_node_mut(self.id).text_content = Some(text.to_string());
        doc.mark_dirty(self.id);
    }

    pub fn append_child(&self, doc: &mut Document, child: Element) {
        doc.append_child(self.id, child.id);
    }

    pub fn remove_child(&self, doc: &mut Document, child: &Element) {
        doc.remove_child(self.id, child.id);
    }

    pub fn children(&self, doc: &Document) -> Vec<Element> {
        doc.children_ids(self.id)
            .iter()
            .map(|&id| Element::new(id))
            .collect()
    }

    pub fn parent_element(&self, doc: &Document) -> Option<Element> {
        doc.get_node(self.id).parent.map(Element::new)
    }

    pub fn set_attribute(&self, doc: &mut Document, name: &str, value: &str) {
        let atom_name = Atom::intern(name);
        let node = doc.get_node_mut(self.id);
        if let Some(attr) = node.attributes.iter_mut().find(|(k, _)| *k == atom_name) {
            if name == "id" {
                let old_val = attr.1.clone();
                attr.1 = value.to_string();
                doc.update_id_index(self.id, Some(&old_val), value);
            } else {
                attr.1 = value.to_string();
            }
        } else {
            let node = doc.get_node_mut(self.id);
            node.attributes.push((atom_name, value.to_string()));
            if name == "id" {
                doc.update_id_index(self.id, None, value);
            }
        }
        doc.mark_dirty(self.id);
    }

    pub fn get_attribute<'a>(&self, doc: &'a Document, name: &str) -> Option<&'a str> {
        let atom_name = Atom::intern(name);
        doc.get_node(self.id)
            .attributes
            .iter()
            .find(|(k, _)| *k == atom_name)
            .map(|(_, v)| v.as_str())
    }

    pub fn remove_attribute(&self, doc: &mut Document, name: &str) {
        let atom_name = Atom::intern(name);
        doc.get_node_mut(self.id)
            .attributes
            .retain(|(k, _)| *k != atom_name);
    }

    pub fn class_list_add(&self, doc: &mut Document, class: &str) {
        let atom = Atom::intern(class);
        let node = doc.get_node_mut(self.id);
        if !node.class_list.contains(&atom) {
            node.class_list.push(atom);
            doc.add_to_class_index(self.id, &atom);
            doc.mark_dirty(self.id);
        }
    }

    pub fn class_list_remove(&self, doc: &mut Document, class: &str) {
        let atom = Atom::intern(class);
        doc.get_node_mut(self.id).class_list.retain(|c| *c != atom);
        doc.remove_from_class_index(self.id, &atom);
        doc.mark_dirty(self.id);
    }

    pub fn class_list_toggle(&self, doc: &mut Document, class: &str) -> bool {
        let atom = Atom::intern(class);
        let contains = doc.get_node(self.id).class_list.contains(&atom);
        if contains {
            self.class_list_remove(doc, class);
            false
        } else {
            self.class_list_add(doc, class);
            true
        }
    }

    pub fn class_list_contains(&self, doc: &Document, class: &str) -> bool {
        let atom = Atom::intern(class);
        doc.get_node(self.id).class_list.contains(&atom)
    }

    pub fn style<'a>(&self, doc: &'a Document) -> &'a CSSStyleDeclaration {
        doc.get_style(self.id)
    }

    pub fn style_mut<'a>(&self, doc: &'a mut Document) -> &'a mut CSSStyleDeclaration {
        doc.mark_dirty(self.id);
        doc.get_style_mut(self.id)
    }

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
        *self
    }
}

impl Copy for Element {}
