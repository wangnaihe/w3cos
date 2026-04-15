use crate::atom::Atom;
use crate::css_style::CSSStyleDeclaration;
use crate::document::Document;
use crate::events::{Event, EventHandler, EventType};
use crate::node::{NodeId, NodeType};

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

    // ── W3C Node tree traversal ────────────────────────────────────────

    pub fn next_sibling(&self, doc: &Document) -> Option<Element> {
        doc.get_node(self.id).next_sibling.map(Element::new)
    }

    pub fn previous_sibling(&self, doc: &Document) -> Option<Element> {
        doc.get_node(self.id).prev_sibling.map(Element::new)
    }

    pub fn first_child(&self, doc: &Document) -> Option<Element> {
        doc.get_node(self.id).first_child.map(Element::new)
    }

    pub fn last_child(&self, doc: &Document) -> Option<Element> {
        doc.get_node(self.id).last_child.map(Element::new)
    }

    pub fn child_element_count(&self, doc: &Document) -> usize {
        doc.children_ids(self.id)
            .iter()
            .filter(|&&id| doc.get_node(id).node_type == NodeType::Element)
            .count()
    }

    /// W3C `Node.nodeType` — returns numeric constant.
    pub fn node_type(&self, doc: &Document) -> u16 {
        doc.get_node(self.id).node_type.as_u16()
    }

    /// W3C `Node.nodeName` — tag name (uppercase for elements).
    pub fn node_name(&self, doc: &Document) -> String {
        doc.get_node(self.id).node_name()
    }

    /// Check if this element is connected to the document tree.
    pub fn is_connected(&self, doc: &Document) -> bool {
        let mut current = self.id;
        loop {
            let node = doc.get_node(current);
            if node.node_type == NodeType::Document {
                return true;
            }
            match node.parent {
                Some(parent_id) => current = parent_id,
                None => return false,
            }
        }
    }

    // ── W3C Node tree mutation ─────────────────────────────────────────

    pub fn replace_child(&self, doc: &mut Document, new_child: Element, old_child: Element) {
        doc.replace_child(self.id, new_child.id, old_child.id);
    }

    pub fn insert_before(&self, doc: &mut Document, new_child: Element, ref_child: Element) {
        doc.insert_before(self.id, new_child.id, ref_child.id);
    }

    // ── Attribute convenience ──────────────────────────────────────────

    pub fn id(&self, doc: &Document) -> Option<String> {
        self.get_attribute(doc, "id").map(|s| s.to_string())
    }

    pub fn set_id(&self, doc: &mut Document, id: &str) {
        self.set_attribute(doc, "id", id);
    }

    pub fn class_name(&self, doc: &Document) -> String {
        let node = doc.get_node(self.id);
        node.class_list
            .iter()
            .map(|a| a.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn set_class_name(&self, doc: &mut Document, name: &str) {
        let node = doc.get_node_mut(self.id);
        let old_classes: Vec<Atom> = std::mem::take(&mut node.class_list);
        for class in old_classes {
            doc.remove_from_class_index(self.id, &class);
        }
        for class in name.split_whitespace() {
            self.class_list_add(doc, class);
        }
    }

    /// `element.dataset` — returns all `data-*` attributes as key/value pairs.
    pub fn dataset(&self, doc: &Document) -> std::collections::HashMap<String, String> {
        doc.get_node(self.id)
            .attributes
            .iter()
            .filter_map(|(k, v)| {
                let name = k.as_str();
                name.strip_prefix("data-").map(|key| {
                    let camel = key
                        .split('-')
                        .enumerate()
                        .map(|(i, part)| {
                            if i == 0 {
                                part.to_string()
                            } else {
                                let mut c = part.chars();
                                match c.next() {
                                    None => String::new(),
                                    Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                                }
                            }
                        })
                        .collect::<String>();
                    (camel, v.clone())
                })
            })
            .collect()
    }

    /// Recursively collect text content from all descendant text nodes.
    pub fn inner_text(&self, doc: &Document) -> String {
        let mut result = String::new();
        Self::collect_text(doc, self.id, &mut result);
        result
    }

    fn collect_text(doc: &Document, id: NodeId, out: &mut String) {
        let node = doc.get_node(id);
        if node.node_type == NodeType::Text {
            if let Some(ref text) = node.text_content {
                out.push_str(text);
            }
            return;
        }
        if let Some(ref text) = node.text_content {
            out.push_str(text);
        }
        let mut child = node.first_child;
        while let Some(child_id) = child {
            Self::collect_text(doc, child_id, out);
            child = doc.get_node(child_id).next_sibling;
        }
    }

    /// Serialize this element as an HTML string (read-only).
    pub fn outer_html(&self, doc: &Document) -> String {
        let mut result = String::new();
        Self::serialize_node(doc, self.id, &mut result);
        result
    }

    fn serialize_node(doc: &Document, id: NodeId, out: &mut String) {
        let node = doc.get_node(id);
        match node.node_type {
            NodeType::Text => {
                if let Some(ref t) = node.text_content {
                    out.push_str(t);
                }
            }
            NodeType::Comment => {
                out.push_str("<!--");
                if let Some(ref t) = node.text_content {
                    out.push_str(t);
                }
                out.push_str("-->");
            }
            NodeType::Element => {
                let tag = node.tag.as_str();
                out.push('<');
                out.push_str(&tag);
                for (k, v) in &node.attributes {
                    out.push(' ');
                    out.push_str(&k.as_str());
                    out.push_str("=\"");
                    out.push_str(v);
                    out.push('"');
                }
                if !node.class_list.is_empty() {
                    out.push_str(" class=\"");
                    for (i, c) in node.class_list.iter().enumerate() {
                        if i > 0 {
                            out.push(' ');
                        }
                        out.push_str(&c.as_str());
                    }
                    out.push('"');
                }
                out.push('>');
                if let Some(ref t) = node.text_content {
                    out.push_str(t);
                }
                let mut child = node.first_child;
                while let Some(child_id) = child {
                    Self::serialize_node(doc, child_id, out);
                    child = doc.get_node(child_id).next_sibling;
                }
                out.push_str("</");
                out.push_str(&tag);
                out.push('>');
            }
            _ => {
                let mut child = node.first_child;
                while let Some(child_id) = child {
                    Self::serialize_node(doc, child_id, out);
                    child = doc.get_node(child_id).next_sibling;
                }
            }
        }
    }
}

impl Clone for Element {
    fn clone(&self) -> Self {
        *self
    }
}

impl Copy for Element {}
