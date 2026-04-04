use std::collections::HashMap;

use crate::atom::Atom;
use crate::css_style::CSSStyleDeclaration;
use crate::element::Element;
use crate::events::EventRegistry;
use crate::node::{DomNode, NodeId, NodeType};

/// W3C Document — the root of the DOM tree.
///
/// Performance characteristics (Chrome/Blink inspired):
/// - Arena-allocated nodes with O(1) access by NodeId
/// - LCRS tree: O(1) append_child, remove_child, insert_before
/// - Interned Atoms: O(1) tag/class comparison
/// - HashMap indexes: O(1) getElementById, querySelector by class/tag
/// - Node freelist: bounded memory with slot recycling
pub struct Document {
    nodes: Vec<Option<DomNode>>,
    styles: Vec<CSSStyleDeclaration>,
    free_list: Vec<u32>,
    dirty: Vec<NodeId>,
    pub(crate) events: EventRegistry,
    body_id: NodeId,
    // Fast lookup indexes
    id_index: HashMap<Atom, NodeId>,
    class_index: HashMap<Atom, Vec<NodeId>>,
    tag_index: HashMap<Atom, Vec<NodeId>>,
}

impl Document {
    pub fn new() -> Self {
        let mut doc = Self {
            nodes: Vec::new(),
            styles: Vec::new(),
            free_list: Vec::new(),
            dirty: Vec::new(),
            events: EventRegistry::new(),
            body_id: NodeId(0),
            id_index: HashMap::new(),
            class_index: HashMap::new(),
            tag_index: HashMap::new(),
        };

        let root_id = doc.alloc_node(DomNode {
            id: NodeId(0),
            node_type: NodeType::Document,
            tag: Atom::intern("#document"),
            text_content: None,
            parent: None,
            first_child: None,
            last_child: None,
            next_sibling: None,
            prev_sibling: None,
            attributes: Vec::new(),
            class_list: Vec::new(),
        });

        let body_id = doc.alloc_node(DomNode::new_element(NodeId(1), "body"));
        // Link body as child of root
        doc.link_child(root_id, body_id);
        doc.body_id = body_id;

        doc
    }

    // -----------------------------------------------------------------------
    // W3C Document API
    // -----------------------------------------------------------------------

    pub fn create_element(&mut self, tag: &str) -> Element {
        let id = self.alloc_node(DomNode::new_element(NodeId(0), tag));
        Element::new(id)
    }

    pub fn create_text_node(&mut self, content: &str) -> Element {
        let id = self.alloc_node(DomNode::new_text(NodeId(0), content));
        Element::new(id)
    }

    pub fn body(&self) -> Element {
        Element::new(self.body_id)
    }

    /// O(1) lookup via HashMap index.
    pub fn get_element_by_id(&self, id: &str) -> Option<Element> {
        let atom = Atom::intern(id);
        self.id_index.get(&atom).map(|&nid| Element::new(nid))
    }

    pub fn query_selector(&self, selector: &str) -> Option<Element> {
        if let Some(id) = selector.strip_prefix('#') {
            return self.get_element_by_id(id);
        }
        if let Some(class) = selector.strip_prefix('.') {
            let atom = Atom::intern(class);
            return self
                .class_index
                .get(&atom)
                .and_then(|ids| ids.first())
                .map(|&id| Element::new(id));
        }
        let atom = Atom::intern(selector);
        self.tag_index
            .get(&atom)
            .and_then(|ids| ids.first())
            .map(|&id| Element::new(id))
    }

    pub fn query_selector_all(&self, selector: &str) -> Vec<Element> {
        if let Some(id) = selector.strip_prefix('#') {
            return self.get_element_by_id(id).into_iter().collect();
        }
        if let Some(class) = selector.strip_prefix('.') {
            let atom = Atom::intern(class);
            return self
                .class_index
                .get(&atom)
                .map(|ids| ids.iter().map(|&id| Element::new(id)).collect())
                .unwrap_or_default();
        }
        let atom = Atom::intern(selector);
        self.tag_index
            .get(&atom)
            .map(|ids| ids.iter().map(|&id| Element::new(id)).collect())
            .unwrap_or_default()
    }

    // -----------------------------------------------------------------------
    // LCRS Tree Operations — all O(1)
    // -----------------------------------------------------------------------

    pub fn append_child(&mut self, parent: NodeId, child: NodeId) {
        self.unlink_from_parent(child);

        let parent_last = self.get_node(parent).last_child;

        if let Some(last) = parent_last {
            self.get_node_mut(last).next_sibling = Some(child);
            self.get_node_mut(child).prev_sibling = Some(last);
        } else {
            self.get_node_mut(parent).first_child = Some(child);
            self.get_node_mut(child).prev_sibling = None;
        }

        self.get_node_mut(child).next_sibling = None;
        self.get_node_mut(child).parent = Some(parent);
        self.get_node_mut(parent).last_child = Some(child);

        self.mark_dirty(parent);
    }

    pub fn remove_child(&mut self, parent: NodeId, child: NodeId) {
        self.unlink_from_parent(child);
        self.get_node_mut(child).parent = None;
        self.mark_dirty(parent);
    }

    pub fn insert_before(&mut self, parent: NodeId, new_child: NodeId, ref_child: NodeId) {
        self.unlink_from_parent(new_child);

        let ref_prev = self.get_node(ref_child).prev_sibling;

        self.get_node_mut(new_child).next_sibling = Some(ref_child);
        self.get_node_mut(new_child).prev_sibling = ref_prev;
        self.get_node_mut(new_child).parent = Some(parent);
        self.get_node_mut(ref_child).prev_sibling = Some(new_child);

        if let Some(prev) = ref_prev {
            self.get_node_mut(prev).next_sibling = Some(new_child);
        } else {
            self.get_node_mut(parent).first_child = Some(new_child);
        }

        self.mark_dirty(parent);
    }

    fn unlink_from_parent(&mut self, child: NodeId) {
        let node = self.get_node(child);
        let parent = node.parent;
        let prev = node.prev_sibling;
        let next = node.next_sibling;

        if let Some(prev_id) = prev {
            self.get_node_mut(prev_id).next_sibling = next;
        } else if let Some(parent_id) = parent {
            self.get_node_mut(parent_id).first_child = next;
        }

        if let Some(next_id) = next {
            self.get_node_mut(next_id).prev_sibling = prev;
        } else if let Some(parent_id) = parent {
            self.get_node_mut(parent_id).last_child = prev;
        }

        self.get_node_mut(child).prev_sibling = None;
        self.get_node_mut(child).next_sibling = None;
    }

    // -----------------------------------------------------------------------
    // Node allocation + freelist
    // -----------------------------------------------------------------------

    fn alloc_node(&mut self, mut node: DomNode) -> NodeId {
        let id = if let Some(slot) = self.free_list.pop() {
            node.id = NodeId(slot);
            let idx = slot as usize;
            self.nodes[idx] = Some(node);
            self.styles[idx] = CSSStyleDeclaration::new();
            NodeId(slot)
        } else {
            let id = NodeId(self.nodes.len() as u32);
            node.id = id;
            let tag = node.tag;
            self.nodes.push(Some(node));
            self.styles.push(CSSStyleDeclaration::new());
            // Update tag index
            self.tag_index.entry(tag).or_default().push(id);
            id
        };
        id
    }

    /// Free a node slot for reuse. Does NOT unlink from tree — call remove_child first.
    pub fn free_node(&mut self, id: NodeId) {
        if let Some(node) = &self.nodes[id.0 as usize] {
            let tag = node.tag;
            // Remove from tag index
            if let Some(ids) = self.tag_index.get_mut(&tag) {
                ids.retain(|&nid| nid != id);
            }
            // Remove from id index
            for (_, attr_val) in &node.attributes {
                // handled on removal
                let _ = attr_val;
            }
            let id_atom_key = node
                .attributes
                .iter()
                .find(|(k, _)| k.as_str() == "id")
                .map(|(_, v)| Atom::intern(v));
            if let Some(id_atom) = id_atom_key {
                self.id_index.remove(&id_atom);
            }
            // Remove from class index
            for class in &node.class_list {
                if let Some(ids) = self.class_index.get_mut(class) {
                    ids.retain(|&nid| nid != id);
                }
            }
        }
        self.nodes[id.0 as usize] = None;
        self.free_list.push(id.0);
    }

    // -----------------------------------------------------------------------
    // Node access
    // -----------------------------------------------------------------------

    pub fn get_node(&self, id: NodeId) -> &DomNode {
        self.nodes[id.0 as usize]
            .as_ref()
            .expect("accessing freed node")
    }

    pub fn get_node_mut(&mut self, id: NodeId) -> &mut DomNode {
        self.nodes[id.0 as usize]
            .as_mut()
            .expect("accessing freed node")
    }

    pub fn get_style(&self, id: NodeId) -> &CSSStyleDeclaration {
        &self.styles[id.0 as usize]
    }

    pub fn get_style_mut(&mut self, id: NodeId) -> &mut CSSStyleDeclaration {
        &mut self.styles[id.0 as usize]
    }

    // -----------------------------------------------------------------------
    // Index maintenance (called by Element methods)
    // -----------------------------------------------------------------------

    pub(crate) fn update_id_index(&mut self, node_id: NodeId, old_id: Option<&str>, new_id: &str) {
        if let Some(old) = old_id {
            self.id_index.remove(&Atom::intern(old));
        }
        self.id_index.insert(Atom::intern(new_id), node_id);
    }

    pub(crate) fn add_to_class_index(&mut self, node_id: NodeId, class: &Atom) {
        self.class_index.entry(*class).or_default().push(node_id);
    }

    pub(crate) fn remove_from_class_index(&mut self, node_id: NodeId, class: &Atom) {
        if let Some(ids) = self.class_index.get_mut(class) {
            ids.retain(|&id| id != node_id);
        }
    }

    // -----------------------------------------------------------------------
    // Dirty tracking
    // -----------------------------------------------------------------------

    /// Mark a node as dirty. Walks up to find the nearest `contain` boundary
    /// (or document root) and marks that scope dirty — not the whole tree.
    /// This enables incremental re-layout of only affected subtrees.
    pub fn mark_dirty(&mut self, id: NodeId) {
        let scope = self.find_layout_scope(id);
        if !self.dirty.contains(&scope) {
            self.dirty.push(scope);
        }
    }

    /// Walk up from `id` to find the nearest ancestor with CSS `contain` set,
    /// or the body node if none found. This is the scope that needs re-layout.
    fn find_layout_scope(&self, id: NodeId) -> NodeId {
        let mut current = id;
        loop {
            let style = &self.styles[current.0 as usize];
            if !matches!(style.inner.contain, w3cos_std::style::Contain::None) {
                return current;
            }
            match self.get_node(current).parent {
                Some(parent_id) if parent_id != NodeId::ROOT => {
                    current = parent_id;
                }
                _ => return current,
            }
        }
    }

    pub fn take_dirty(&mut self) -> Vec<NodeId> {
        std::mem::take(&mut self.dirty)
    }

    pub fn is_dirty(&self) -> bool {
        !self.dirty.is_empty()
    }

    // -----------------------------------------------------------------------
    // Child iteration helper
    // -----------------------------------------------------------------------

    pub fn children_ids(&self, parent: NodeId) -> Vec<NodeId> {
        let mut result = Vec::new();
        let mut current = self.get_node(parent).first_child;
        while let Some(id) = current {
            result.push(id);
            current = self.get_node(id).next_sibling;
        }
        result
    }

    // -----------------------------------------------------------------------
    // Component tree bridge
    // -----------------------------------------------------------------------

    pub fn to_component_tree(&self) -> w3cos_std::Component {
        self.node_to_component(self.body_id)
    }

    fn node_to_component(&self, id: NodeId) -> w3cos_std::Component {
        let node = self.get_node(id);
        let style = self.styles[id.0 as usize].to_style();
        let tag = node.tag.as_str();

        match node.node_type {
            NodeType::Text => {
                let text = node.text_content.as_deref().unwrap_or("");
                w3cos_std::Component::text(text, style)
            }
            NodeType::Element | NodeType::Document => {
                let children: Vec<w3cos_std::Component> = self
                    .children_ids(id)
                    .iter()
                    .map(|&child_id| self.node_to_component(child_id))
                    .collect();

                if let Some(text) = &node.text_content {
                    if children.is_empty() {
                        return match tag.as_str() {
                            "button" | "a" => w3cos_std::Component::button(text, style),
                            _ => w3cos_std::Component::text(text, style),
                        };
                    }
                }

                let is_row = matches!(
                    style.flex_direction,
                    w3cos_std::style::FlexDirection::Row
                        | w3cos_std::style::FlexDirection::RowReverse
                );

                match tag.as_str() {
                    "body" | "div" | "section" | "main" | "article" | "nav" | "header"
                    | "footer" | "aside" | "form" | "fieldset" | "ul" | "ol" | "dl" => {
                        if is_row {
                            w3cos_std::Component::row(style, children)
                        } else {
                            w3cos_std::Component::column(style, children)
                        }
                    }
                    "span" | "label" | "em" | "strong" | "code" | "small" | "li" | "dd"
                    | "dt" => {
                        if let Some(text) = &node.text_content {
                            if children.is_empty() {
                                return w3cos_std::Component::text(text, style);
                            }
                        }
                        if is_row {
                            w3cos_std::Component::row(style, children)
                        } else {
                            w3cos_std::Component::column(style, children)
                        }
                    }
                    "p" => {
                        if let Some(text) = &node.text_content {
                            if children.is_empty() {
                                return w3cos_std::Component::text(text, style);
                            }
                        }
                        w3cos_std::Component::column(style, children)
                    }
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                        if let Some(text) = &node.text_content {
                            let mut heading_style = style;
                            let default_size = match tag.as_str() {
                                "h1" => 32.0,
                                "h2" => 24.0,
                                "h3" => 20.0,
                                "h4" => 18.0,
                                "h5" => 16.0,
                                _ => 14.0,
                            };
                            if heading_style.font_size == 16.0 {
                                heading_style.font_size = default_size;
                            }
                            if heading_style.font_weight == 400 {
                                heading_style.font_weight = 700;
                            }
                            w3cos_std::Component::text(text, heading_style)
                        } else {
                            w3cos_std::Component::column(style, children)
                        }
                    }
                    "button" => {
                        let label = node.text_content.as_deref().unwrap_or("Button");
                        w3cos_std::Component::button(label, style)
                    }
                    "img" => {
                        let src = node
                            .attributes
                            .iter()
                            .find(|(k, _)| k.as_str() == "src")
                            .map(|(_, v)| v.as_str())
                            .unwrap_or("");
                        w3cos_std::Component::image(src, style)
                    }
                    "input" => {
                        let placeholder = node
                            .attributes
                            .iter()
                            .find(|(k, _)| k.as_str() == "placeholder")
                            .map(|(_, v)| v.as_str())
                            .unwrap_or("");
                        let value = node.text_content.as_deref().unwrap_or("");
                        w3cos_std::Component::text_input(value, placeholder, style)
                    }
                    _ => {
                        if is_row {
                            w3cos_std::Component::row(style, children)
                        } else {
                            w3cos_std::Component::column(style, children)
                        }
                    }
                }
            }
        }
    }

    pub fn node_count(&self) -> usize {
        self.nodes.iter().filter(|n| n.is_some()).count()
    }

    /// Event dispatch with bubbling — walks parent pointers directly (O(depth), no HashMap).
    pub fn dispatch_event_bubbling(&mut self, event: &mut crate::events::Event) {
        let mut chain = Vec::new();
        let mut current = Some(event.target);
        while let Some(id) = current {
            chain.push(id);
            current = self.get_node(id).parent;
        }
        for node_id in chain {
            self.events.dispatch_at_node(node_id, event);
            if event.stop_propagation {
                return;
            }
        }
    }

    fn link_child(&mut self, parent: NodeId, child: NodeId) {
        self.append_child(parent, child);
    }
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}
