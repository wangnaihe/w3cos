use crate::atom::Atom;
use crate::css_style::CSSStyleDeclaration;
use crate::document::Document;
use crate::dom_rect::DOMRect;
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
        if name.eq_ignore_ascii_case("class") {
            self.set_class_name(doc, value);
            return;
        }
        let atom_name = Atom::intern(name);
        let existing_index = doc
            .get_node(self.id)
            .attributes
            .iter()
            .position(|(key, _)| *key == atom_name);
        if let Some(existing_index) = existing_index {
            let old_value = doc.get_node(self.id).attributes[existing_index].1.clone();
            let node = doc.get_node_mut(self.id);
            if name == "id" {
                node.attributes[existing_index].1 = value.to_string();
                doc.update_id_index(self.id, Some(&old_value), value);
            } else {
                node.attributes[existing_index].1 = value.to_string();
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

    pub fn set_attribute_ns(
        &self,
        doc: &mut Document,
        namespace: Option<&str>,
        qualified_name: &str,
        prefix: Option<&str>,
        local_name: &str,
        value: &str,
    ) {
        let old_id = (namespace.is_none() && local_name == "id")
            .then(|| self.get_attribute_ns(doc, None, "id").map(str::to_string))
            .flatten();
        doc.get_node_mut(self.id).set_attribute_ns(
            namespace,
            qualified_name,
            prefix,
            local_name,
            value,
        );
        if namespace.is_none() && local_name == "id" {
            doc.update_id_index(self.id, old_id.as_deref(), value);
        }
        if namespace.is_none() && local_name.eq_ignore_ascii_case("class") {
            let old_classes = std::mem::take(&mut doc.get_node_mut(self.id).class_list);
            for class in old_classes {
                doc.remove_from_class_index(self.id, &class);
            }
            for class in value
                .split([' ', '\t', '\n', '\r', '\x0c'])
                .filter(|class| !class.is_empty())
            {
                self.class_list_add(doc, class);
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

    pub fn get_attribute_ns<'a>(
        &self,
        doc: &'a Document,
        namespace: Option<&str>,
        local_name: &str,
    ) -> Option<&'a str> {
        doc.get_node(self.id)
            .get_attribute_ns(namespace, local_name)
    }

    pub fn remove_attribute(&self, doc: &mut Document, name: &str) {
        if name.eq_ignore_ascii_case("class") {
            let old_classes: Vec<Atom> = std::mem::take(&mut doc.get_node_mut(self.id).class_list);
            for class in old_classes {
                doc.remove_from_class_index(self.id, &class);
            }
        }
        let node = doc.get_node_mut(self.id);
        node.remove_attribute(name);
        doc.mark_dirty(self.id);
    }

    pub fn remove_attribute_ns(
        &self,
        doc: &mut Document,
        namespace: Option<&str>,
        local_name: &str,
    ) -> bool {
        let removed = doc
            .get_node_mut(self.id)
            .remove_attribute_ns(namespace, local_name);
        if removed {
            doc.mark_dirty(self.id);
        }
        removed
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

    /// Like [`Element::add_event_listener`], but takes an already-resolved
    /// [`EventType`] and returns the listener id.
    ///
    /// Needed by the jsdom bridge: `EventType::from_str` mints a *fresh*
    /// `Custom(id)` on every call for unknown event names, so the bridge
    /// memoizes name → `EventType` itself and registers by the stable value.
    pub fn add_event_listener_typed(
        &self,
        doc: &mut Document,
        event_type: EventType,
        handler: EventHandler,
    ) -> u32 {
        doc.events.add(self.id, event_type, handler)
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
        let class_attr = Atom::intern("class");
        if let Some(index) = node
            .attributes
            .iter()
            .position(|(name, _)| *name == class_attr)
        {
            node.clear_attribute_namespace(index);
        }
        let old_classes: Vec<Atom> = std::mem::take(&mut node.class_list);
        for class in old_classes {
            doc.remove_from_class_index(self.id, &class);
        }
        for class in name
            .split([' ', '\t', '\n', '\r', '\x0c'])
            .filter(|class| !class.is_empty())
        {
            self.class_list_add(doc, class);
        }
        let node = doc.get_node_mut(self.id);
        if let Some((_, value)) = node
            .attributes
            .iter_mut()
            .find(|(key, _)| *key == class_attr)
        {
            *value = name.to_string();
        } else {
            node.attributes.push((class_attr, name.to_string()));
        }
        doc.mark_dirty(self.id);
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

    // ── W3C Layout API ─────────────────────────────────────────────────

    /// W3C `Element.getBoundingClientRect()` — returns the element's layout rect.
    ///
    /// In w3cos the layout engine stores computed rects in `Document::layout_rects`.
    /// If no layout has been computed yet (rect is zero), returns a zeroed DOMRect.
    /// The runtime must call `Document::set_layout_rect` after each layout pass.
    pub fn get_bounding_client_rect(&self, doc: &Document) -> DOMRect {
        doc.get_layout_rect(self.id)
    }

    /// W3C `Element.getClientRects()` — returns a list containing the bounding rect.
    /// For block-level elements this is always a single-element list.
    pub fn get_client_rects(&self, doc: &Document) -> Vec<DOMRect> {
        vec![self.get_bounding_client_rect(doc)]
    }

    /// W3C `window.getComputedStyle(element)` — returns the computed CSS style.
    ///
    /// In w3cos, computed style is the same as the inline style stored in the
    /// document's style arena (CSS cascade is not yet fully implemented).
    /// Returns a clone so callers can read properties without borrowing the doc.
    pub fn get_computed_style(&self, doc: &Document) -> crate::css_style::CSSStyleDeclaration {
        crate::css_style::CSSStyleDeclaration::from_style(doc.computed_style_for(self.id))
    }

    /// W3C `Element.scrollTop` / `scrollLeft` — scroll position.
    pub fn scroll_top(&self, doc: &Document) -> f32 {
        doc.get_scroll(self.id).1
    }

    pub fn scroll_left(&self, doc: &Document) -> f32 {
        doc.get_scroll(self.id).0
    }

    pub fn set_scroll_top(&self, doc: &mut Document, value: f32) {
        doc.set_scroll(self.id, None, Some(value));
    }

    pub fn set_scroll_left(&self, doc: &mut Document, value: f32) {
        doc.set_scroll(self.id, Some(value), None);
    }

    /// W3C `Element.scrollWidth` / `scrollHeight` — full scrollable size.
    pub fn scroll_width(&self, doc: &Document) -> f32 {
        self.scroll_extent(doc).0
    }

    pub fn scroll_height(&self, doc: &Document) -> f32 {
        self.scroll_extent(doc).1
    }

    fn scroll_extent(&self, doc: &Document) -> (f32, f32) {
        let root = doc.get_layout_rect(self.id);
        let mut width = root.width;
        let mut height = root.height;
        let mut pending = doc.children_ids(self.id);
        while let Some(id) = pending.pop() {
            let rect = doc.get_layout_rect(id);
            width = width.max(rect.x + rect.width - root.x);
            height = height.max(rect.y + rect.height - root.y);
            pending.extend(doc.children_ids(id));
        }
        (width.max(0.0), height.max(0.0))
    }

    /// W3C `Element.clientWidth` / `clientHeight` — visible content size.
    pub fn client_width(&self, doc: &Document) -> f32 {
        doc.get_layout_rect(self.id).width
    }

    pub fn client_height(&self, doc: &Document) -> f32 {
        doc.get_layout_rect(self.id).height
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
            NodeType::CdataSection => {
                out.push_str("<![CDATA[");
                out.push_str(node.text_content.as_deref().unwrap_or(""));
                out.push_str("]]>");
            }
            NodeType::ProcessingInstruction => {
                out.push_str("<?");
                out.push_str(&node.tag.as_str());
                if let Some(text) = node.text_content.as_deref().filter(|text| !text.is_empty()) {
                    out.push(' ');
                    out.push_str(text);
                }
                out.push_str("?>");
            }
            NodeType::Comment => {
                out.push_str("<!--");
                if let Some(ref t) = node.text_content {
                    out.push_str(t);
                }
                out.push_str("-->");
            }
            NodeType::DocumentType => {
                out.push_str("<!DOCTYPE ");
                out.push_str(&node.tag.as_str());
                out.push('>');
            }
            NodeType::Element => {
                let tag = node.tag.as_str();
                out.push('<');
                out.push_str(&tag);
                for (k, v) in &node.attributes {
                    out.push(' ');
                    out.push_str(&k.as_str());
                    out.push_str("=\"");
                    push_html_attribute_escaped(out, v);
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

fn push_html_attribute_escaped(out: &mut String, value: &str) {
    for character in value.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            character => out.push(character),
        }
    }
}

impl Clone for Element {
    fn clone(&self) -> Self {
        *self
    }
}

impl Copy for Element {}
