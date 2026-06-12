use std::collections::HashMap;

use crate::atom::Atom;
use crate::css_style::CSSStyleDeclaration;
use crate::dom_rect::DOMRect;
use crate::element::Element;
use crate::events::EventRegistry;
use crate::node::{DomNode, NodeId, NodeType};
use crate::selection::{Range, Selection};

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
    /// Layout rects computed by the layout engine after each pass.
    /// Indexed by NodeId — same arena as nodes/styles.
    layout_rects: Vec<DOMRect>,
    /// Scroll offsets (scroll_left, scroll_top) per node.
    scroll_offsets: Vec<(f32, f32)>,
    free_list: Vec<u32>,
    dirty: Vec<NodeId>,
    pub(crate) events: EventRegistry,
    body_id: NodeId,
    // Fast lookup indexes
    id_index: HashMap<Atom, NodeId>,
    class_index: HashMap<Atom, Vec<NodeId>>,
    tag_index: HashMap<Atom, Vec<NodeId>>,
    // Selection state
    selection: Selection,
}

impl Document {
    pub fn new() -> Self {
        let mut doc = Self {
            nodes: Vec::new(),
            styles: Vec::new(),
            layout_rects: Vec::new(),
            scroll_offsets: Vec::new(),
            free_list: Vec::new(),
            dirty: Vec::new(),
            events: EventRegistry::new(),
            body_id: NodeId(0),
            id_index: HashMap::new(),
            class_index: HashMap::new(),
            tag_index: HashMap::new(),
            selection: Selection::new(),
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

    pub fn create_document_fragment(&mut self) -> Element {
        let id = self.alloc_node(DomNode::new_document_fragment(NodeId(0)));
        Element::new(id)
    }

    pub fn create_comment(&mut self, content: &str) -> Element {
        let id = self.alloc_node(DomNode::new_comment(NodeId(0), content));
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

    /// W3C `document.createRange()` — creates a new Range object.
    pub fn create_range(&self) -> Range {
        Range::new()
    }

    /// W3C `window.getSelection()` — returns the current selection.
    pub fn get_selection(&self) -> &Selection {
        &self.selection
    }

    /// W3C `window.getSelection()` — returns the current selection (mutable).
    pub fn get_selection_mut(&mut self) -> &mut Selection {
        &mut self.selection
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

    pub fn replace_child(&mut self, parent: NodeId, new_child: NodeId, old_child: NodeId) {
        self.insert_before(parent, new_child, old_child);
        self.remove_child(parent, old_child);
    }

    /// Deep-clone a node and its subtree. Returns the new root NodeId.
    pub fn clone_node(&mut self, source: NodeId, deep: bool) -> NodeId {
        let node = self.get_node(source);
        let mut new_node = match node.node_type {
            NodeType::Element => {
                let mut n = DomNode::new_element(NodeId(0), &node.tag.as_str());
                n.attributes = node.attributes.clone();
                n.class_list = node.class_list.clone();
                n.text_content = node.text_content.clone();
                n
            }
            NodeType::Text => DomNode::new_text(NodeId(0), node.text_content.as_deref().unwrap_or("")),
            NodeType::Comment => DomNode::new_comment(NodeId(0), node.text_content.as_deref().unwrap_or("")),
            NodeType::DocumentFragment => DomNode::new_document_fragment(NodeId(0)),
            NodeType::Document => DomNode::new_element(NodeId(0), "div"),
        };
        new_node.parent = None;
        new_node.first_child = None;
        new_node.last_child = None;
        new_node.next_sibling = None;
        new_node.prev_sibling = None;

        let source_style = self.get_style(source).clone();
        let new_id = self.alloc_node(new_node);
        self.styles[new_id.0 as usize] = source_style;

        if deep {
            let child_ids = self.children_ids(source);
            for child_id in child_ids {
                let cloned_child = self.clone_node(child_id, true);
                self.append_child(new_id, cloned_child);
            }
        }

        new_id
    }

    // ── Query helpers ──

    pub fn get_elements_by_tag_name(&self, tag: &str) -> Vec<Element> {
        let atom = Atom::intern(tag);
        self.tag_index
            .get(&atom)
            .map(|ids| ids.iter().map(|&id| Element::new(id)).collect())
            .unwrap_or_default()
    }

    pub fn get_elements_by_class_name(&self, class: &str) -> Vec<Element> {
        let atom = Atom::intern(class);
        self.class_index
            .get(&atom)
            .map(|ids| ids.iter().map(|&id| Element::new(id)).collect())
            .unwrap_or_default()
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
            self.layout_rects[idx] = DOMRect::zero();
            self.scroll_offsets[idx] = (0.0, 0.0);
            NodeId(slot)
        } else {
            let id = NodeId(self.nodes.len() as u32);
            node.id = id;
            let tag = node.tag;
            self.nodes.push(Some(node));
            self.styles.push(CSSStyleDeclaration::new());
            self.layout_rects.push(DOMRect::zero());
            self.scroll_offsets.push((0.0, 0.0));
            // Update tag index
            self.tag_index.entry(tag).or_default().push(id);
            id
        };
        id
    }

    // -----------------------------------------------------------------------
    // Layout rect API — called by the layout engine after each pass
    // -----------------------------------------------------------------------

    /// Get the last computed bounding rect for a node.
    /// Returns `DOMRect::zero()` if no layout has been run yet.
    pub fn get_layout_rect(&self, id: NodeId) -> DOMRect {
        self.layout_rects
            .get(id.0 as usize)
            .copied()
            .unwrap_or_default()
    }

    /// Store the computed bounding rect for a node.
    /// Called by the layout engine after each layout pass.
    pub fn set_layout_rect(&mut self, id: NodeId, rect: DOMRect) {
        let idx = id.0 as usize;
        if idx < self.layout_rects.len() {
            self.layout_rects[idx] = rect;
        }
    }

    /// Bulk-update layout rects from a slice of (NodeId, DOMRect) pairs.
    /// More efficient than calling `set_layout_rect` in a loop.
    pub fn apply_layout_rects(&mut self, rects: &[(NodeId, DOMRect)]) {
        for &(id, rect) in rects {
            self.set_layout_rect(id, rect);
        }
    }

    // -----------------------------------------------------------------------
    // Scroll offset API
    // -----------------------------------------------------------------------

    /// Get the scroll offset (scroll_left, scroll_top) for a node.
    pub fn get_scroll(&self, id: NodeId) -> (f32, f32) {
        self.scroll_offsets
            .get(id.0 as usize)
            .copied()
            .unwrap_or((0.0, 0.0))
    }

    /// Set scroll offset. Pass `None` to leave an axis unchanged.
    pub fn set_scroll(&mut self, id: NodeId, left: Option<f32>, top: Option<f32>) {
        let idx = id.0 as usize;
        if idx < self.scroll_offsets.len() {
            if let Some(l) = left {
                self.scroll_offsets[idx].0 = l;
            }
            if let Some(t) = top {
                self.scroll_offsets[idx].1 = t;
            }
        }
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
            NodeType::Comment => {
                return w3cos_std::Component::column(style, vec![]);
            }
            NodeType::Element | NodeType::Document | NodeType::DocumentFragment => {
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
                    "canvas" => {
                        let width = node
                            .attributes
                            .iter()
                            .find(|(k, _)| k.as_str() == "width")
                            .and_then(|(_, v)| v.parse::<u32>().ok())
                            .unwrap_or(300);
                        let height = node
                            .attributes
                            .iter()
                            .find(|(k, _)| k.as_str() == "height")
                            .and_then(|(_, v)| v.parse::<u32>().ok())
                            .unwrap_or(150);
                        w3cos_std::Component::canvas(width, height, style)
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

    /// Full W3C event dispatch with capturing and bubbling phases.
    pub fn dispatch_event_bubbling(&mut self, event: &mut crate::events::Event) {
        // Build ancestor chain: [target, parent, ..., root]
        let mut chain = Vec::new();
        let mut current = Some(event.target);
        while let Some(id) = current {
            chain.push(id);
            current = self.get_node(id).parent;
        }

        // Phase 1: Capturing — root to target (exclusive)
        event.event_phase = crate::events::EventPhase::Capturing;
        for &node_id in chain.iter().rev().skip(0) {
            if node_id == event.target {
                break;
            }
            self.events.dispatch_at_node(node_id, event);
            if event.stop_propagation {
                return;
            }
        }

        // Phase 2: At target
        event.event_phase = crate::events::EventPhase::AtTarget;
        self.events.dispatch_at_node(event.target, event);
        if event.stop_propagation {
            return;
        }

        // Phase 3: Bubbling — target parent to root
        if event.bubbles {
            event.event_phase = crate::events::EventPhase::Bubbling;
            for &node_id in chain.iter().skip(1) {
                self.events.dispatch_at_node(node_id, event);
                if event.stop_propagation {
                    return;
                }
            }
        }

        event.event_phase = crate::events::EventPhase::None;
    }

    fn link_child(&mut self, parent: NodeId, child: NodeId) {
        self.append_child(parent, child);
    }

    // ── selectionchange ───────────────────────────────────────────────────

    /// Fire a `selectionchange` event on the document root.
    /// CodeMirror's DOMObserver listens to this to track cursor/selection changes.
    /// Call this whenever `Selection` state is updated by the runtime.
    pub fn dispatch_selection_change(&mut self) {
        use crate::events::{Event, EventType};
        let root = NodeId::ROOT;
        let mut ev = Event::new(EventType::SelectionChange, root);
        ev.bubbles = false;
        self.events.dispatch_at_node(root, &mut ev);
    }

    /// Add an event listener on the document root (for document-level events
    /// like `selectionchange`). Returns the listener id for later removal.
    pub fn add_document_event_listener(
        &mut self,
        event: &str,
        handler: crate::events::EventHandler,
    ) -> u32 {
        if let Some(event_type) = crate::events::EventType::from_str(event) {
            self.events.add(NodeId::ROOT, event_type, handler)
        } else {
            0
        }
    }

    /// Fire a `beforeinput` event on the given target element.
    /// Returns `true` if `preventDefault()` was called (caller should suppress the input).
    pub fn dispatch_before_input(
        &mut self,
        target: NodeId,
        data: Option<String>,
        input_type: Option<crate::events::InputType>,
        target_ranges: Vec<(NodeId, usize, NodeId, usize)>,
    ) -> bool {
        use crate::events::{Event, EventData, EventType};
        let mut ev = Event::new(EventType::BeforeInput, target);
        ev.bubbles = true;
        ev.cancelable = true;
        ev.data = EventData::BeforeInput {
            data,
            input_type,
            is_composing: false,
            target_ranges,
        };
        self.dispatch_event_bubbling(&mut ev);
        ev.prevent_default
    }

    // ── contenteditable ───────────────────────────────────────────────────

    /// Returns true if the given node has `contenteditable="true"` or `""`.
    pub fn is_content_editable(&self, id: NodeId) -> bool {
        self.get_node(id).is_content_editable()
    }

    /// Walk up the ancestor chain to find the nearest contenteditable root.
    pub fn editable_root(&self, id: NodeId) -> Option<NodeId> {
        let mut current = Some(id);
        while let Some(node_id) = current {
            let node = self.get_node(node_id);
            if node.is_content_editable() {
                return Some(node_id);
            }
            current = node.parent;
        }
        None
    }

    /// Handle a keyboard event on a `contenteditable` element.
    /// Mutates the text content of the focused node and fires a W3C `InputEvent`.
    /// Returns true if the event was handled (caller should call `preventDefault`).
    pub fn handle_contenteditable_key(
        &mut self,
        target: NodeId,
        key: &str,
        ctrl: bool,
        meta: bool,
    ) -> bool {
        use crate::events::{Event, EventData, EventType, InputType};

        let editable_id = match self.editable_root(target) {
            Some(id) => id,
            None => return false,
        };

        // Find the text node child to mutate, or use the element's text_content
        let text_node_id = {
            let node = self.get_node(editable_id);
            node.first_child
        };

        let (input_type, inserted_text) = match key {
            // Printable character — insert
            k if k.len() == 1 && !ctrl && !meta => {
                (InputType::InsertText, Some(k.to_string()))
            }
            "Enter" => (InputType::InsertParagraph, Some("\n".to_string())),
            "Backspace" => (InputType::DeleteContentBackward, None),
            "Delete" => (InputType::DeleteContentForward, None),
            // Ctrl/Cmd+Z — undo
            "z" | "Z" if ctrl || meta => (InputType::HistoryUndo, None),
            // Ctrl/Cmd+Y or Ctrl/Cmd+Shift+Z — redo
            "y" | "Y" if ctrl || meta => (InputType::HistoryRedo, None),
            // Ctrl/Cmd+X — cut
            "x" | "X" if ctrl || meta => (InputType::DeleteByCut, None),
            // Ctrl/Cmd+V — paste (caller handles actual clipboard read)
            "v" | "V" if ctrl || meta => (InputType::InsertFromPaste, None),
            _ => return false,
        };

        // Mutate text content
        let target_id = text_node_id.unwrap_or(editable_id);
        {
            let node = self.get_node_mut(target_id);
            let text = node.text_content.get_or_insert_with(String::new);
            match &input_type {
                InputType::InsertText | InputType::InsertParagraph => {
                    if let Some(ref s) = inserted_text {
                        text.push_str(s);
                    }
                }
                InputType::DeleteContentBackward => {
                    // Remove last char (respects multi-byte UTF-8)
                    let mut chars = text.chars();
                    chars.next_back();
                    *text = chars.as_str().to_string();
                }
                InputType::DeleteContentForward => {
                    if !text.is_empty() {
                        let mut chars = text.chars();
                        chars.next();
                        *text = chars.as_str().to_string();
                    }
                }
                _ => {}
            }
        }

        self.mark_dirty(target_id);

        // Fire W3C InputEvent (bubbles, not cancelable per spec)
        let mut input_event = Event::new(EventType::Input, editable_id);
        input_event.bubbles = true;
        input_event.cancelable = false;
        input_event.data = EventData::Input {
            data: inserted_text,
            input_type: Some(input_type),
            is_composing: false,
        };
        self.dispatch_event_bubbling(&mut input_event);

        true
    }

    /// Handle IME composition events on a `contenteditable` element.
    /// `phase`: "start" | "update" | "end"
    pub fn handle_composition(
        &mut self,
        target: NodeId,
        phase: &str,
        data: &str,
    ) {
        use crate::events::{Event, EventData, EventType, InputType};

        let editable_id = match self.editable_root(target) {
            Some(id) => id,
            None => return,
        };

        let event_type = match phase {
            "start" => EventType::CompositionStart,
            "update" => EventType::CompositionUpdate,
            _ => EventType::CompositionEnd,
        };

        let mut comp_event = Event::new(event_type, editable_id);
        comp_event.bubbles = true;
        comp_event.data = EventData::Composition { data: data.to_string() };
        self.dispatch_event_bubbling(&mut comp_event);

        // On compositionend, fire an InputEvent with insertCompositionText
        if phase == "end" && !data.is_empty() {
            let text_node_id = self.get_node(editable_id).first_child;
            let target_id = text_node_id.unwrap_or(editable_id);
            {
                let node = self.get_node_mut(target_id);
                let text = node.text_content.get_or_insert_with(String::new);
                text.push_str(data);
            }
            self.mark_dirty(target_id);

            let mut input_event = Event::new(EventType::Input, editable_id);
            input_event.bubbles = true;
            input_event.cancelable = false;
            input_event.data = EventData::Input {
                data: Some(data.to_string()),
                input_type: Some(InputType::InsertCompositionText),
                is_composing: false,
            };
            self.dispatch_event_bubbling(&mut input_event);
        }
    }
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}
