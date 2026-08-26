use std::collections::HashMap;
use std::rc::Rc;

use crate::atom::Atom;
use crate::css_style::CSSStyleDeclaration;
use crate::dom_rect::DOMRect;
use crate::element::Element;
use crate::events::EventRegistry;
use crate::node::{DomNode, NodeId, NodeType};
use crate::selection::{Range, Selection};
use crate::stylesheet;
use crate::user_agent;

#[derive(Clone, Default)]
struct CounterSnapshot {
    scopes: Vec<HashMap<String, i32>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum GeneratedContentItem {
    Text(String),
    Image(String),
}

fn push_generated_text(items: &mut Vec<GeneratedContentItem>, text: &str) {
    if let Some(GeneratedContentItem::Text(current)) = items.last_mut() {
        current.push_str(text);
    } else {
        items.push(GeneratedContentItem::Text(text.to_string()));
    }
}

impl CounterSnapshot {
    fn value(&self, name: &str) -> i32 {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
            .unwrap_or_default()
    }

    fn values(&self, name: &str) -> Vec<i32> {
        let values = self
            .scopes
            .iter()
            .filter_map(|scope| scope.get(name).copied())
            .collect::<Vec<_>>();
        if values.is_empty() { vec![0] } else { values }
    }
}

fn resolve_css_variables(value: &str, custom_properties: &HashMap<String, String>) -> String {
    let mut current = value.to_string();
    for _ in 0..10 {
        let Some(start) = current.find("var(") else {
            break;
        };
        let after = &current[start + 4..];
        let mut depth = 1i32;
        let mut end = None;
        for (index, character) in after.char_indices() {
            match character {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(index);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end else { break };
        let inner = after[..end].trim();
        let (name, fallback) = inner
            .split_once(',')
            .map_or((inner, None), |(name, fallback)| {
                (name.trim(), Some(fallback.trim()))
            });
        let Some(replacement) = custom_properties.get(name).map(String::as_str).or(fallback) else {
            break;
        };
        current = format!("{}{}{}", &current[..start], replacement, &after[end + 1..]);
    }
    current
}

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
    // The selected responsive-image source used by component lowering. This
    // is rendering state rather than a reflected HTML attribute: `img.src`
    // must continue to expose the author-provided fallback while `currentSrc`
    // reports the selected `srcset`/`picture` candidate.
    image_render_sources: HashMap<NodeId, String>,
    // Selection state
    selection: Selection,
    // HTML attribute selector matching has a few document-language-specific
    // case-folding rules which do not apply to XML/XHTML documents.
    html_document: bool,
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
            image_render_sources: HashMap::new(),
            selection: Selection::new(),
            html_document: true,
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
            attribute_namespaces: Vec::new(),
            class_list: Vec::new(),
            is_html_element: false,
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

    pub fn create_cdata_section(&mut self, content: &str) -> Element {
        let id = self.alloc_node(DomNode::new_cdata_section(NodeId(0), content));
        Element::new(id)
    }

    pub fn create_processing_instruction(&mut self, target: &str, data: &str) -> Element {
        let id = self.alloc_node(DomNode::new_processing_instruction(NodeId(0), target, data));
        Element::new(id)
    }

    pub fn create_document_type(&mut self, name: &str) -> Element {
        let id = self.alloc_node(DomNode::new_document_type(NodeId(0), name));
        Element::new(id)
    }

    pub fn body(&self) -> Element {
        Element::new(self.body_id)
    }

    /// Select the body subtree used by the component/layout bridge.
    ///
    /// HTML parsing reuses the bootstrap body, while XML/XHTML parsing creates
    /// its body from the response document and switches the render root here.
    pub fn set_render_body(&mut self, body: NodeId) {
        debug_assert_eq!(self.get_node(body).node_type, NodeType::Element);
        self.body_id = body;
    }

    pub fn set_html_document(&mut self, html_document: bool) {
        self.html_document = html_document;
    }

    pub fn is_html_document(&self) -> bool {
        self.html_document
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
        if self.insertion_would_create_cycle(parent, child) {
            return;
        }
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
                n.attribute_namespaces = node.attribute_namespaces.clone();
                n.class_list = node.class_list.clone();
                n.text_content = node.text_content.clone();
                n.is_html_element = node.is_html_element;
                n
            }
            NodeType::Text => {
                DomNode::new_text(NodeId(0), node.text_content.as_deref().unwrap_or(""))
            }
            NodeType::CdataSection => {
                DomNode::new_cdata_section(NodeId(0), node.text_content.as_deref().unwrap_or(""))
            }
            NodeType::ProcessingInstruction => DomNode::new_processing_instruction(
                NodeId(0),
                node.tag.as_str(),
                node.text_content.as_deref().unwrap_or(""),
            ),
            NodeType::Comment => {
                DomNode::new_comment(NodeId(0), node.text_content.as_deref().unwrap_or(""))
            }
            NodeType::DocumentType => DomNode::new_document_type(NodeId(0), node.tag.as_str()),
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
        if new_child == ref_child {
            return;
        }
        if self.insertion_would_create_cycle(parent, new_child) {
            return;
        }
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

    fn insertion_would_create_cycle(&self, parent: NodeId, child: NodeId) -> bool {
        let mut current = Some(parent);
        while let Some(node) = current {
            if node == child {
                return true;
            }
            current = self.get_node(node).parent;
        }
        false
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
        let initial_style =
            CSSStyleDeclaration::from_style(user_agent::html_default_style(&node.tag.as_str()));
        let id = if let Some(slot) = self.free_list.pop() {
            node.id = NodeId(slot);
            let idx = slot as usize;
            self.nodes[idx] = Some(node);
            self.styles[idx] = initial_style;
            self.layout_rects[idx] = DOMRect::zero();
            self.scroll_offsets[idx] = (0.0, 0.0);
            NodeId(slot)
        } else {
            let id = NodeId(self.nodes.len() as u32);
            node.id = id;
            let tag = node.tag;
            self.nodes.push(Some(node));
            self.styles.push(initial_style);
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
        self.image_render_sources.remove(&id);
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

    /// Remove a node and all descendants from the retained document.
    ///
    /// DOM wrappers normally become collectible after detachment. The native
    /// arena needs an explicit sweep so framework adapters can release host
    /// subtrees without leaking slots or selector indexes.
    pub fn remove_node(&mut self, id: NodeId) {
        let children = self.children_ids(id);
        for child in children {
            self.remove_node(child);
        }
        self.unlink_from_parent(id);
        self.events.remove_all(id);
        self.free_node(id);
    }

    // -----------------------------------------------------------------------
    // Node access
    // -----------------------------------------------------------------------

    pub fn get_node(&self, id: NodeId) -> &DomNode {
        self.nodes[id.0 as usize]
            .as_ref()
            .expect("accessing freed node")
    }

    /// Set the renderer-facing source selected for an `<img>` element without
    /// mutating its reflected `src` attribute.
    pub fn set_image_render_source(&mut self, id: NodeId, source: Option<&str>) {
        if let Some(source) = source {
            self.image_render_sources.insert(id, source.to_string());
        } else {
            self.image_render_sources.remove(&id);
        }
        self.mark_dirty(id);
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
        if let Some(document_element) = self.get_node(self.body_id).parent
            && self
                .get_node(document_element)
                .tag
                .as_str()
                .eq_ignore_ascii_case("html")
        {
            // Use one stable browser formatting root. Rooting ordinary pages
            // at body makes Taffy ignore the body's own margin, while pages
            // that happen to render html generated content keep that same
            // margin because body is nested. That structural optimization
            // changes pixels, so always retain the document element.
            return self.to_component_subtree(document_element);
        }
        // Keep the browser UA body margin on both the body fast path and the
        // promoted document-element path. Clearing it only for the fast path
        // makes equivalent pages shift when one happens to render an html
        // pseudo-element or an authored head child.
        self.to_component_subtree(self.body_id)
    }

    /// Lower one connected DOM subtree while preserving selector ancestry.
    pub fn to_component_subtree(&self, id: NodeId) -> w3cos_std::Component {
        let mut lineage = Vec::new();
        let mut current = self.get_node(id).parent;
        while let Some(parent) = current {
            if self.get_node(parent).node_type == NodeType::Element {
                lineage.push(parent);
            }
            current = self.get_node(parent).parent;
        }
        lineage.reverse();
        let mut ancestors = lineage
            .into_iter()
            .map(|ancestor| self.selector_context(ancestor))
            .collect();
        let inherited = self
            .get_node(id)
            .parent
            .map(|parent| self.computed_style_for(parent));
        self.node_to_component(id, &mut ancestors, inherited.as_ref())
    }

    fn attach_native_host(
        &self,
        id: NodeId,
        mut component: w3cos_std::Component,
    ) -> w3cos_std::Component {
        component.on_click = w3cos_std::EventAction::NativeHost {
            id: id.as_u32() as u64,
            click: false,
            scroll: false,
            input: false,
            focus: false,
            keyboard: false,
            submit: false,
            pointer: true,
            wheel: false,
        };
        component
    }

    pub fn descendant_text_content(&self, id: NodeId) -> String {
        let node = self.get_node(id);
        let mut text = node.text_content.clone().unwrap_or_default();
        for child in self.children_ids(id) {
            text.push_str(&self.descendant_text_content(child));
        }
        text
    }

    /// Match one element against an authored selector list using the same
    /// parser and tree context as stylesheet cascade matching.
    pub fn matches_selector(&self, id: NodeId, selector: &str) -> Result<bool, ()> {
        stylesheet::selector_matches_node(selector, self, id)
    }

    /// Match with browsing-context state that is intentionally not stored in
    /// the platform-neutral DOM tree.
    pub fn matches_selector_with_target(
        &self,
        id: NodeId,
        selector: &str,
        target_id: Option<&str>,
    ) -> Result<bool, ()> {
        stylesheet::selector_matches_node_with_target(selector, self, id, target_id)
    }

    pub fn matches_selector_relative_to_scope(
        &self,
        id: NodeId,
        selector: &str,
        scope: NodeId,
        target_id: Option<&str>,
    ) -> Result<bool, ()> {
        stylesheet::selector_matches_node_relative_to_scope(selector, self, id, scope, target_id)
    }

    /// Selector context for stylesheet matching: tag, id, classes, attributes.
    fn selector_context(&self, id: NodeId) -> stylesheet::SelectorContext {
        let mut context = self.selector_context_base(id);
        let Some(parent) = self.get_node(id).parent else {
            return context;
        };
        let mut previous_siblings = Vec::new();
        for sibling in self.children_ids(parent) {
            if sibling == id {
                break;
            }
            if self.get_node(sibling).node_type != NodeType::Element {
                continue;
            }
            let sibling_context = self
                .selector_context_base(sibling)
                .with_shared_previous_siblings(previous_siblings.clone());
            previous_siblings.push(Rc::new(sibling_context));
        }
        context.previous_siblings = previous_siblings;
        context
    }

    fn selector_context_base(&self, id: NodeId) -> stylesheet::SelectorContext {
        let node = self.get_node(id);
        let id_attr = node
            .attributes
            .iter()
            .find(|(k, _)| k.as_str() == "id")
            .map(|(_, v)| v.as_str());
        let classes: Vec<String> = node.class_list.iter().map(|c| c.as_str()).collect();
        let class_refs: Vec<&str> = classes.iter().map(String::as_str).collect();
        let attributes: Vec<(String, String)> = node
            .attributes
            .iter()
            .map(|(name, value)| (name.as_str().to_string(), value.as_str().to_string()))
            .collect();
        let attribute_refs: Vec<(&str, &str)> = attributes
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        let is_first_child = node.parent.is_some_and(|parent| {
            self.children_ids(parent)
                .into_iter()
                .find(|sibling| self.get_node(*sibling).node_type == NodeType::Element)
                == Some(id)
        });
        let is_root = node
            .parent
            .is_none_or(|parent| self.get_node(parent).node_type == NodeType::Document);
        stylesheet::SelectorContext::new(&node.tag.as_str(), id_attr, &class_refs)
            .with_attributes(&attribute_refs)
            .with_tree_state(is_first_child, is_root)
            .with_html_document(self.html_document)
            .with_html_element(self.html_document && node.is_html_element)
    }

    /// Computed style for a node: stylesheet-matched declarations first
    /// (ascending specificity, then registration order), inline style on top.
    /// Falls back to the raw inline style when no stylesheet rules apply.
    fn computed_style(
        &self,
        id: NodeId,
        _ancestors: &[stylesheet::SelectorContext],
        inherited: Option<&w3cos_std::style::Style>,
    ) -> w3cos_std::style::Style {
        let inline = &self.styles[id.0 as usize];
        let node = self.get_node(id);
        let matched = if stylesheet::has_rules() && node.node_type == NodeType::Element {
            stylesheet::matching_declarations_for_node(self, id)
        } else {
            Vec::new()
        };
        let mut merged =
            CSSStyleDeclaration::from_style(user_agent::html_default_style(&node.tag.as_str()));
        // The legacy HTML `text` presentational hint participates before
        // author declarations and supplies the body's inherited color. CSS2
        // generated-content tests still exercise this behavior for XHTML
        // serialization as well as text/html.
        let body_text_hint = if node.node_type == NodeType::Element
            && node.tag.as_str().eq_ignore_ascii_case("body")
        {
            node.attributes
                .iter()
                .find(|(name, _)| name.as_str().eq_ignore_ascii_case("text"))
                .map(|(_, value)| value.as_str())
        } else {
            None
        };
        if let Some(value) = body_text_hint {
            merged.set_property("color", value);
        }
        let body_background_hint = if node.node_type == NodeType::Element
            && node.tag.as_str().eq_ignore_ascii_case("body")
        {
            node.attributes
                .iter()
                .find(|(name, _)| name.as_str().eq_ignore_ascii_case("bgcolor"))
                .map(|(_, value)| value.as_str())
        } else {
            None
        };
        if let Some(value) = body_background_hint {
            merged.set_property("background-color", value);
        }
        let mut custom_properties = inherited
            .and_then(|style| style.custom_properties.clone())
            .unwrap_or_default();

        // Custom properties participate in the cascade independently of
        // declaration order. Collect them first, then resolve ordinary
        // declarations using the winning inherited/scoped/inline values.
        for (prop, value, _specificity) in &matched {
            if prop.starts_with("--") {
                custom_properties.insert(prop.clone(), value.clone());
            }
        }
        for (prop, value) in &inline.inline_declarations {
            if prop.starts_with("--") {
                custom_properties.insert(prop.clone(), value.clone());
            }
        }
        for (prop, value, _specificity) in &matched {
            if !prop.starts_with("--") {
                merged.set_property(prop, &resolve_css_variables(value, &custom_properties));
            }
        }
        // Inline wins: re-apply the node's own declarations on top.
        for (prop, value) in &inline.inline_declarations {
            if !prop.starts_with("--") {
                merged.set_property(prop, &resolve_css_variables(value, &custom_properties));
            }
        }
        merged.inner.custom_properties =
            (!custom_properties.is_empty()).then_some(custom_properties);
        let mut style = merged.to_style();

        if let Some(parent) = inherited {
            let declares = |property: &str| {
                (css_property_eq(property, "color") && body_text_hint.is_some())
                    || matched
                        .iter()
                        .any(|(name, _, _)| css_property_eq(name, property))
                    || inline
                        .inline_declarations
                        .iter()
                        .any(|(name, _)| css_property_eq(name, property))
            };
            inherit_text_style(&mut style, parent, &node.tag.as_str(), declares);
        }
        let declared_value = |properties: &[&str]| {
            matched
                .iter()
                .filter(|(name, _, _)| {
                    properties
                        .iter()
                        .any(|property| css_property_eq(name, property))
                })
                .map(|(_, value, _)| value.as_str())
                .chain(
                    inline
                        .inline_declarations
                        .iter()
                        .filter(|(name, _)| {
                            properties
                                .iter()
                                .any(|property| css_property_eq(name, property))
                        })
                        .map(|(_, value)| value.as_str()),
                )
                .last()
        };
        if let Some(value) = declared_value(&["float", "cssFloat"]) {
            match value.trim().to_ascii_lowercase().as_str() {
                "inherit" => {
                    style.float = inherited
                        .map(|parent| parent.float)
                        .unwrap_or(w3cos_std::style::Float::None);
                }
                "initial" | "unset" | "revert" | "revert-layer" => {
                    style.float = w3cos_std::style::Float::None;
                }
                _ => {}
            }
        }
        if matches!(
            style.position,
            w3cos_std::style::Position::Absolute | w3cos_std::style::Position::Fixed
        ) {
            // CSS2 blockifies the principal box for absolute positioning, but
            // the computed `float` value itself becomes `none`.
            style.float = w3cos_std::style::Float::None;
        }
        if declared_value(&["background", "background-color"])
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("currentcolor"))
        {
            style.background = style.color;
        }
        if declared_value(&["border-color"])
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("currentcolor"))
        {
            style.border_color = style.color;
        }
        let last_declared_border_color = matched
            .iter()
            .filter(|(name, _, _)| {
                matches!(name.as_str(), "border" | "border-color" | "borderColor")
            })
            .map(|(_, value, _)| value.as_str())
            .chain(
                inline
                    .inline_declarations
                    .iter()
                    .filter(|(name, _)| {
                        matches!(name.as_str(), "border" | "border-color" | "borderColor")
                    })
                    .map(|(_, value)| value.as_str()),
            )
            .last();
        let explicitly_transparent_border = last_declared_border_color.is_some_and(|value| {
            split_css_tokens(value)
                .iter()
                .any(|token| token.eq_ignore_ascii_case("transparent"))
        });
        let has_used_border_width = style.border_width > 0.0
            || [
                style.border_top_width,
                style.border_right_width,
                style.border_bottom_width,
                style.border_left_width,
            ]
            .into_iter()
            .flatten()
            .any(|width| width > 0.0);
        if has_used_border_width && style.border_color.a == 0 && !explicitly_transparent_border {
            // The initial border color is `currentcolor`, not transparent.
            // Resolve it only after text color inheritance has completed.
            style.border_color = style.color;
        }
        style
    }

    /// Resolve stylesheet rules, ancestor selectors, user-agent defaults, and
    /// inline declarations for a node.
    pub fn computed_style_for(&self, id: NodeId) -> w3cos_std::style::Style {
        let mut ancestor_ids = Vec::new();
        let mut current = self.get_node(id).parent;
        while let Some(parent) = current {
            ancestor_ids.push(parent);
            current = self.get_node(parent).parent;
        }
        ancestor_ids.reverse();
        let mut ancestors = Vec::new();
        let mut inherited = None;
        for ancestor in ancestor_ids {
            let style = self.computed_style(ancestor, &ancestors, inherited.as_ref());
            inherited = Some(style);
            if self.get_node(ancestor).node_type == NodeType::Element {
                ancestors.push(self.selector_context(ancestor));
            }
        }
        self.computed_style(id, &ancestors, inherited.as_ref())
    }

    /// Resolve the author cascade for a generated pseudo-element. Pseudo
    /// declarations are matched against the originating element but remain
    /// separate from its principal computed style.
    pub fn computed_pseudo_style_for(
        &self,
        id: NodeId,
        pseudo_element: &str,
    ) -> w3cos_std::style::Style {
        let mut merged = CSSStyleDeclaration::new();
        for (property, value, _) in
            stylesheet::matching_pseudo_declarations_for_node(self, id, pseudo_element)
        {
            merged.set_property(&property, &value);
        }
        merged.to_style()
    }

    fn generated_pseudo_component(
        &self,
        id: NodeId,
        pseudo_element: &str,
        origin_style: &w3cos_std::style::Style,
    ) -> Option<w3cos_std::Component> {
        let declarations =
            stylesheet::matching_pseudo_declarations_for_node(self, id, pseudo_element);
        let mut selected_content = None;
        for content_value in declarations
            .iter()
            .rev()
            .filter(|(property, _, _)| css_property_eq(property, "content"))
            .map(|(_, value, _)| value)
        {
            let keyword = content_value.trim();
            if keyword.eq_ignore_ascii_case("none") || keyword.eq_ignore_ascii_case("normal") {
                // These are valid computed values and suppress any earlier
                // generated value in the cascade.
                return None;
            }
            if keyword.eq_ignore_ascii_case("inherit")
                && self.inherited_element_content_value(id).is_none()
            {
                // Inheriting the initial `normal` value is also a valid,
                // non-generating result rather than invalid syntax eligible
                // for fallback.
                return None;
            }
            if let Some(content) =
                self.resolve_generated_content_items(id, pseudo_element, content_value)
            {
                selected_content = Some((content_value, content));
                break;
            }
        }
        {
            let (content_value, content) = selected_content.as_ref()?;
            let contains_string_token = content_value
                .chars()
                .any(|character| matches!(character, '\'' | '"'));
            let contains_image = content
                .iter()
                .any(|item| matches!(item, GeneratedContentItem::Image(_)));
            let contains_text = content
                .iter()
                .any(|item| matches!(item, GeneratedContentItem::Text(text) if !text.is_empty()));
            if !contains_image && !contains_text && !contains_string_token {
                // State-only quote operators (`no-open-quote`/`no-close-quote`)
                // and missing attr() values affect later generated content but do
                // not contribute glyphs or an anonymous line box. Keep their
                // cascade/counter traversal semantics and omit only the visual
                // component. An authored empty CSS string is different: it can
                // still be sized or positioned and therefore keeps a box below.
                return None;
            }
        }

        let mut merged = CSSStyleDeclaration::new();
        merged.set_property("display", "inline");
        for (property, value, _) in &declarations {
            if !css_property_eq(property, "content") {
                merged.set_property(property, value);
            }
        }
        let mut style = merged.to_style();
        if !generated_display_creates_box(style.display) {
            return None;
        }
        let declares = |property: &str| {
            declarations
                .iter()
                .any(|(name, _, _)| css_property_eq(name, property))
        };
        inherit_text_style(&mut style, origin_style, pseudo_element, declares);
        let explicitly_inherits = |property: &str| {
            declarations
                .iter()
                .rev()
                .find(|(name, _, _)| css_property_eq(name, property))
                .is_some_and(|(_, value, _)| value.trim().eq_ignore_ascii_case("inherit"))
        };
        if explicitly_inherits("border") {
            style.border_width = origin_style.border_width;
            style.border_color = origin_style.border_color;
            style.border_top_width = origin_style.border_top_width;
            style.border_right_width = origin_style.border_right_width;
            style.border_bottom_width = origin_style.border_bottom_width;
            style.border_left_width = origin_style.border_left_width;
            style.border_top_color = origin_style.border_top_color;
            style.border_right_color = origin_style.border_right_color;
            style.border_bottom_color = origin_style.border_bottom_color;
            style.border_left_color = origin_style.border_left_color;
        } else {
            if explicitly_inherits("border-width") {
                style.border_width = origin_style.border_width;
                style.border_top_width = origin_style.border_top_width;
                style.border_right_width = origin_style.border_right_width;
                style.border_bottom_width = origin_style.border_bottom_width;
                style.border_left_width = origin_style.border_left_width;
            }
            if explicitly_inherits("border-color") {
                style.border_color = origin_style.border_color;
                style.border_top_color = origin_style.border_top_color;
                style.border_right_color = origin_style.border_right_color;
                style.border_bottom_color = origin_style.border_bottom_color;
                style.border_left_color = origin_style.border_left_color;
            }
        }
        if selected_content.as_ref().is_some_and(|(_, content)| {
            !content
                .iter()
                .any(|item| matches!(item, GeneratedContentItem::Image(_)))
                && content
                    .iter()
                    .all(|item| matches!(item, GeneratedContentItem::Text(text) if text.is_empty()))
        }) && style.display == w3cos_std::style::Display::Inline
        {
            // Taffy has no native inline formatting context. A zero-length
            // generated inline box must shrink to zero width; leaving the
            // leaf as `Inline` makes it stretch across a block parent and can
            // paint an authored background over the full available width.
            style.display = w3cos_std::style::Display::InlineBlock;
        }
        normalize_css_table_internal_used_style(&mut style);
        let (_, content) = selected_content?;
        match content.as_slice() {
            [GeneratedContentItem::Text(text)] => {
                Some(w3cos_std::Component::text(text.clone(), style))
            }
            [GeneratedContentItem::Image(source)] => {
                Some(w3cos_std::Component::image(source.clone(), style))
            }
            _ => {
                // A pseudo-element is one principal CSS box whose `content`
                // items participate in an anonymous inline formatting
                // context. Preserve the authored display/position/box model
                // on the outer component and lower the ordered text/replaced
                // items into one transparent row inside it.
                let mut inherited = w3cos_std::style::Style::default();
                inherit_text_style(&mut inherited, &style, "", |_| false);
                inherited.text_decoration = style.text_decoration;
                inherited.visibility = style.visibility;

                let children = content
                    .into_iter()
                    .map(|item| match item {
                        GeneratedContentItem::Text(text) => {
                            let mut item_style = inherited.clone();
                            item_style.display = w3cos_std::style::Display::Inline;
                            w3cos_std::Component::text(text, item_style)
                        }
                        GeneratedContentItem::Image(source) => {
                            let mut item_style = inherited.clone();
                            item_style.display = w3cos_std::style::Display::InlineBlock;
                            w3cos_std::Component::image(source, item_style)
                        }
                    })
                    .collect();
                if matches!(
                    style.display,
                    w3cos_std::style::Display::Inline
                        | w3cos_std::style::Display::InlineBlock
                        | w3cos_std::style::Display::InlineFlex
                        | w3cos_std::style::Display::InlineTable
                        | w3cos_std::style::Display::TableRow
                ) {
                    style.flex_direction = w3cos_std::style::FlexDirection::Row;
                    style.align_items = w3cos_std::style::AlignItems::Baseline;
                    let children = fixup_css_table_children(&style, children);
                    Some(w3cos_std::Component::row(style, children))
                } else {
                    let mut line_style = inherited;
                    line_style.display = w3cos_std::style::Display::Flex;
                    line_style.flex_direction = w3cos_std::style::FlexDirection::Row;
                    line_style.align_items = w3cos_std::style::AlignItems::Baseline;
                    let children = fixup_css_table_children(
                        &style,
                        vec![w3cos_std::Component::row(line_style, children)],
                    );
                    Some(w3cos_std::Component::boxed(style, children))
                }
            }
        }
    }

    fn list_marker_component(
        &self,
        id: NodeId,
        origin_style: &w3cos_std::style::Style,
    ) -> Option<w3cos_std::Component> {
        let declarations = stylesheet::matching_declarations_for_node(self, id);
        let declared = |property: &str| {
            declarations
                .iter()
                .filter(|(name, _, _)| css_property_eq(name, property))
                .map(|(_, value, _)| value.trim())
                .last()
        };
        if !declared("list-style-position")
            .is_some_and(|value| value.eq_ignore_ascii_case("inside"))
        {
            return None;
        }
        let marker = match declared("list-style-type")?.to_ascii_lowercase().as_str() {
            "disc" => "•",
            "circle" => "◦",
            "square" => "▪",
            "none" => return None,
            _ => return None,
        };
        let mut style = CSSStyleDeclaration::new().to_style();
        style.display = w3cos_std::style::Display::Inline;
        inherit_text_style(&mut style, origin_style, "::marker", |_| false);
        Some(w3cos_std::Component::text(marker, style))
    }

    fn declared_counter_value(&self, id: NodeId, property: &str) -> Option<String> {
        stylesheet::matching_declarations_for_node(self, id)
            .into_iter()
            .filter(|(name, _, _)| css_property_eq(name, property))
            .map(|(_, value, _)| value)
            .chain(
                self.styles[id.0 as usize]
                    .inline_declarations
                    .iter()
                    .filter(|(name, _)| css_property_eq(name, property))
                    .map(|(_, value)| value.clone()),
            )
            .last()
    }

    fn pseudo_counter_value(
        &self,
        id: NodeId,
        pseudo_element: &str,
        property: &str,
    ) -> Option<String> {
        stylesheet::matching_pseudo_declarations_for_node(self, id, pseudo_element)
            .into_iter()
            .filter(|(name, _, _)| css_property_eq(name, property))
            .map(|(_, value, _)| value)
            .last()
    }

    fn apply_counter_declaration(
        scopes: &mut Vec<HashMap<String, i32>>,
        value: Option<String>,
        default_value: i32,
        operation: &str,
    ) {
        let Some(value) = value.filter(|value| !value.trim().eq_ignore_ascii_case("none")) else {
            return;
        };
        let tokens = value.split_ascii_whitespace().collect::<Vec<_>>();
        let mut index = 0;
        while index < tokens.len() {
            let name = tokens[index];
            index += 1;
            let amount = tokens
                .get(index)
                .and_then(|value| value.parse::<i32>().ok())
                .inspect(|_| index += 1)
                .unwrap_or(default_value);
            match operation {
                "reset" => {
                    scopes
                        .last_mut()
                        .expect("counter traversal scope")
                        .insert(name.to_string(), amount);
                }
                "set" => {
                    if let Some(scope) = scopes
                        .iter_mut()
                        .rev()
                        .find(|scope| scope.contains_key(name))
                    {
                        scope.insert(name.to_string(), amount);
                    } else {
                        scopes
                            .last_mut()
                            .expect("counter traversal scope")
                            .insert(name.to_string(), amount);
                    }
                }
                _ => {
                    if let Some(counter) = scopes
                        .iter_mut()
                        .rev()
                        .find_map(|scope| scope.get_mut(name))
                    {
                        *counter += amount;
                    } else {
                        scopes
                            .last_mut()
                            .expect("counter traversal scope")
                            .insert(name.to_string(), amount);
                    }
                }
            }
        }
    }

    fn apply_element_counters(&self, id: NodeId, scopes: &mut Vec<HashMap<String, i32>>) {
        Self::apply_counter_declaration(
            scopes,
            self.declared_counter_value(id, "counter-reset"),
            0,
            "reset",
        );
        Self::apply_counter_declaration(
            scopes,
            self.declared_counter_value(id, "counter-set"),
            0,
            "set",
        );
        Self::apply_counter_declaration(
            scopes,
            self.declared_counter_value(id, "counter-increment"),
            1,
            "increment",
        );
    }

    fn element_counter_reset_names(&self, id: NodeId) -> Vec<String> {
        let mut reset_scope = vec![HashMap::new()];
        Self::apply_counter_declaration(
            &mut reset_scope,
            self.declared_counter_value(id, "counter-reset"),
            0,
            "reset",
        );
        reset_scope
            .pop()
            .expect("counter reset probe scope")
            .into_keys()
            .collect()
    }

    fn authored_pseudo_content_value(&self, id: NodeId, pseudo_element: &str) -> Option<String> {
        stylesheet::matching_pseudo_declarations_for_node(self, id, pseudo_element)
            .into_iter()
            .filter(|(property, _, _)| css_property_eq(property, "content"))
            .map(|(_, value, _)| value)
            .last()
    }

    fn pseudo_generates_box(&self, id: NodeId, pseudo_element: &str) -> bool {
        let Some(content) = self.authored_pseudo_content_value(id, pseudo_element) else {
            return false;
        };
        if content.trim().eq_ignore_ascii_case("none")
            || content.trim().eq_ignore_ascii_case("normal")
        {
            return false;
        }
        generated_display_creates_box(self.computed_pseudo_style_for(id, pseudo_element).display)
    }

    fn quote_pairs_for(&self, id: NodeId) -> Vec<(String, String)> {
        let mut current = Some(id);
        while let Some(candidate) = current {
            let declared = stylesheet::matching_declarations_for_node(self, candidate)
                .into_iter()
                .filter(|(property, _, _)| css_property_eq(property, "quotes"))
                .map(|(_, value, _)| value)
                .chain(
                    self.styles[candidate.0 as usize]
                        .inline_declarations
                        .iter()
                        .filter(|(property, _)| css_property_eq(property, "quotes"))
                        .map(|(_, value)| value.clone()),
                )
                .last();
            if let Some(value) = declared {
                if value.trim().eq_ignore_ascii_case("inherit") {
                    current = self.get_node(candidate).parent;
                    continue;
                }
                if value.trim().eq_ignore_ascii_case("none") {
                    return Vec::new();
                }
                let strings = parse_css_string_list(&value);
                if strings.len() >= 2 && strings.len().is_multiple_of(2) {
                    return strings
                        .chunks_exact(2)
                        .map(|pair| (pair[0].clone(), pair[1].clone()))
                        .collect();
                }
            }
            current = self.get_node(candidate).parent;
        }
        vec![
            ("\"".to_string(), "\"".to_string()),
            ("'".to_string(), "'".to_string()),
        ]
    }

    fn visit_quote_tree(
        &self,
        id: NodeId,
        target: NodeId,
        target_pseudo: &str,
        depth: &mut usize,
    ) -> Option<usize> {
        let node = self.get_node(id);
        if node.node_type != NodeType::Element
            || self.computed_style_for(id).display == w3cos_std::style::Display::None
        {
            return None;
        }
        if self.pseudo_generates_box(id, "::before") {
            if id == target && target_pseudo == "::before" {
                return Some(*depth);
            }
            if let Some(content) = self.authored_pseudo_content_value(id, "::before") {
                adjust_quote_depth(&content, depth);
            }
        }
        for child in self.children_ids(id) {
            if let Some(depth) = self.visit_quote_tree(child, target, target_pseudo, depth) {
                return Some(depth);
            }
        }
        if self.pseudo_generates_box(id, "::after") {
            if id == target && target_pseudo == "::after" {
                return Some(*depth);
            }
            if let Some(content) = self.authored_pseudo_content_value(id, "::after") {
                adjust_quote_depth(&content, depth);
            }
        }
        None
    }

    fn quote_depth_at(&self, target: NodeId, target_pseudo: &str) -> usize {
        let mut depth = 0usize;
        for child in self.children_ids(NodeId(0)) {
            if let Some(depth) = self.visit_quote_tree(child, target, target_pseudo, &mut depth) {
                return depth;
            }
        }
        depth
    }

    fn visit_pseudo_counters(
        &self,
        id: NodeId,
        pseudo_element: &str,
        target: NodeId,
        target_pseudo: &str,
        scopes: &mut Vec<HashMap<String, i32>>,
        retain_scope_for_following_siblings: bool,
    ) -> Option<CounterSnapshot> {
        if !self.pseudo_generates_box(id, pseudo_element) {
            return None;
        }
        scopes.push(HashMap::new());
        for (property, default_value, operation) in [
            ("counter-reset", 0, "reset"),
            ("counter-set", 0, "set"),
            ("counter-increment", 1, "increment"),
        ] {
            Self::apply_counter_declaration(
                scopes,
                self.pseudo_counter_value(id, pseudo_element, property),
                default_value,
                operation,
            );
        }
        if id == target && pseudo_element == target_pseudo {
            return Some(CounterSnapshot {
                scopes: scopes.clone(),
            });
        }
        if !retain_scope_for_following_siblings {
            scopes.pop();
        }
        None
    }

    fn visit_counter_tree(
        &self,
        id: NodeId,
        target: NodeId,
        target_pseudo: &str,
        scopes: &mut Vec<HashMap<String, i32>>,
        retain_scope_for_following_siblings: bool,
    ) -> Option<CounterSnapshot> {
        let node = self.get_node(id);
        if node.node_type != NodeType::Element
            || self.computed_style_for(id).display == w3cos_std::style::Display::None
        {
            return None;
        }
        let incoming_scope_count = scopes.len();
        scopes.push(HashMap::new());
        self.apply_element_counters(id, scopes);
        let sibling_scope_base = scopes.len();
        if let Some(snapshot) =
            self.visit_pseudo_counters(id, "::before", target, target_pseudo, scopes, true)
        {
            return Some(snapshot);
        }
        let child_scope_base = scopes.len();
        for child in self.children_ids(id) {
            for name in self.element_counter_reset_names(child) {
                // A reset on a following sibling creates a new same-name
                // scope before any descendant content is evaluated. Mask a
                // retained scope from an earlier sibling (including
                // `::before`) immediately, rather than only after returning
                // from the child traversal.
                for prior_scope in &mut scopes[sibling_scope_base..] {
                    prior_scope.remove(&name);
                }
            }
            let scopes_before_child = scopes.len();
            if let Some(snapshot) =
                self.visit_counter_tree(child, target, target_pseudo, scopes, true)
            {
                return Some(snapshot);
            }
            if scopes.len() > scopes_before_child {
                let new_sibling_scope = scopes.pop().expect("retained sibling counter scope");
                for prior_scope in &mut scopes[sibling_scope_base..] {
                    for name in new_sibling_scope.keys() {
                        prior_scope.remove(name);
                    }
                }
                scopes.push(new_sibling_scope);
            }
        }
        scopes.truncate(child_scope_base);
        if let Some(snapshot) =
            self.visit_pseudo_counters(id, "::after", target, target_pseudo, scopes, false)
        {
            return Some(snapshot);
        }
        let element_scope = scopes[incoming_scope_count].clone();
        scopes.truncate(incoming_scope_count);
        if retain_scope_for_following_siblings && !element_scope.is_empty() {
            scopes.push(element_scope);
        }
        None
    }

    fn counter_snapshot_at(&self, target: NodeId, pseudo_element: &str) -> CounterSnapshot {
        let mut scopes = vec![HashMap::new()];
        for child in self.children_ids(NodeId(0)) {
            if let Some(snapshot) =
                self.visit_counter_tree(child, target, pseudo_element, &mut scopes, false)
            {
                return snapshot;
            }
        }
        CounterSnapshot { scopes }
    }

    fn resolve_generated_content_items(
        &self,
        id: NodeId,
        pseudo_element: &str,
        value: &str,
    ) -> Option<Vec<GeneratedContentItem>> {
        let value = value.trim();
        if value.eq_ignore_ascii_case("none") || value.eq_ignore_ascii_case("normal") {
            return None;
        }
        if value.eq_ignore_ascii_case("inherit") {
            let inherited = self.inherited_element_content_value(id)?;
            return self.resolve_generated_content_items(id, pseudo_element, &inherited);
        }

        let mut output = Vec::new();
        let mut remaining = value;
        let mut counters = None;
        let mut quote_depth = None;
        let quote_pairs = self.quote_pairs_for(id);
        while !remaining.trim_start().is_empty() {
            remaining = remaining.trim_start();
            let Some(first) = remaining.chars().next() else {
                break;
            };
            if matches!(first, '\'' | '"') {
                let mut escaped = false;
                let mut end = None;
                for (index, character) in remaining[first.len_utf8()..].char_indices() {
                    if escaped {
                        escaped = false;
                    } else if character == '\\' {
                        escaped = true;
                    } else if character == first {
                        end = Some(first.len_utf8() + index);
                        break;
                    }
                }
                let end = end?;
                push_generated_text(
                    &mut output,
                    &stylesheet::css_unescape(&remaining[first.len_utf8()..end])?,
                );
                remaining = &remaining[end + first.len_utf8()..];
                continue;
            }
            if remaining
                .get(..4)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("url("))
            {
                let (source, consumed) = generated_content_image_prefix(remaining)?;
                output.push(GeneratedContentItem::Image(source));
                remaining = &remaining[consumed..];
                continue;
            }
            if remaining
                .get(..5)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("attr("))
            {
                let end = remaining.find(')')?;
                let attribute = remaining[5..end].trim();
                push_generated_text(
                    &mut output,
                    self.get_node(id)
                        .attributes
                        .iter()
                        .find(|(name, _)| {
                            if self.html_document && self.get_node(id).is_html_element {
                                name.as_str().eq_ignore_ascii_case(attribute)
                            } else {
                                name.as_str() == attribute
                            }
                        })
                        .map(|(_, value)| value.as_str())
                        .unwrap_or_default(),
                );
                remaining = &remaining[end + 1..];
                continue;
            }
            let counter_function = if remaining
                .get(..9)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("counters("))
            {
                Some((true, 9))
            } else if remaining
                .get(..8)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("counter("))
            {
                Some((false, 8))
            } else {
                None
            };
            if let Some((multiple, prefix_len)) = counter_function {
                let end = remaining.find(')')?;
                let arguments = remaining[prefix_len..end]
                    .split(',')
                    .map(str::trim)
                    .collect::<Vec<_>>();
                if (multiple && !(2..=3).contains(&arguments.len()))
                    || (!multiple && !(1..=2).contains(&arguments.len()))
                {
                    return None;
                }
                let name = *arguments.first()?;
                let snapshot =
                    counters.get_or_insert_with(|| self.counter_snapshot_at(id, pseudo_element));
                if multiple {
                    let separator = arguments
                        .get(1)
                        .and_then(|separator| {
                            let quote = separator.chars().next()?;
                            (matches!(quote, '\'' | '"') && separator.ends_with(quote)).then(|| {
                                &separator[quote.len_utf8()..separator.len() - quote.len_utf8()]
                            })
                        })
                        .and_then(stylesheet::css_unescape)
                        .unwrap_or_default();
                    let style = arguments.get(2).copied().unwrap_or("decimal");
                    if !valid_counter_style(style) {
                        return None;
                    }
                    push_generated_text(
                        &mut output,
                        &snapshot
                            .values(name)
                            .into_iter()
                            .map(|value| format_counter_value(value, style))
                            .collect::<Vec<_>>()
                            .join(&separator),
                    );
                } else {
                    let style = arguments.get(1).copied().unwrap_or("decimal");
                    if !valid_counter_style(style) {
                        return None;
                    }
                    push_generated_text(
                        &mut output,
                        &format_counter_value(snapshot.value(name), style),
                    );
                }
                remaining = &remaining[end + 1..];
                continue;
            }
            let quote_operator = [
                "no-open-quote",
                "no-close-quote",
                "open-quote",
                "close-quote",
            ]
            .into_iter()
            .find(|operator| {
                remaining.len() >= operator.len()
                    && remaining[..operator.len()].eq_ignore_ascii_case(operator)
                    && remaining[operator.len()..]
                        .chars()
                        .next()
                        .is_none_or(|next| !next.is_ascii_alphanumeric() && next != '-')
            });
            if let Some(operator) = quote_operator {
                let depth =
                    quote_depth.get_or_insert_with(|| self.quote_depth_at(id, pseudo_element));
                match operator {
                    "open-quote" => {
                        if let Some((opening, _)) =
                            quote_pairs.get((*depth).min(quote_pairs.len().saturating_sub(1)))
                        {
                            push_generated_text(&mut output, opening);
                        }
                        *depth += 1;
                    }
                    "no-open-quote" => *depth += 1,
                    "close-quote" if *depth > 0 => {
                        *depth -= 1;
                        if let Some((_, closing)) =
                            quote_pairs.get((*depth).min(quote_pairs.len().saturating_sub(1)))
                        {
                            push_generated_text(&mut output, closing);
                        }
                    }
                    "no-close-quote" => *depth = depth.saturating_sub(1),
                    _ => {}
                }
                remaining = &remaining[operator.len()..];
                continue;
            }
            if remaining.trim_start().len() == remaining.len() {
                // Unsupported content items (images, counters and quote-depth
                // controls) stay inert without discarding adjacent strings or
                // attr() values that still have a visual representation.
                let end = remaining
                    .find(char::is_whitespace)
                    .unwrap_or(remaining.len());
                remaining = &remaining[end..];
            }
        }
        Some(output)
    }

    fn inherited_element_content_value(&self, id: NodeId) -> Option<String> {
        let mut current = Some(id);
        while let Some(node_id) = current {
            let declared = stylesheet::matching_declarations_for_node(self, node_id)
                .into_iter()
                .filter(|(property, _, _)| css_property_eq(property, "content"))
                .map(|(_, value, _)| value)
                .last();
            match declared.as_deref().map(str::trim) {
                Some(value) if value.eq_ignore_ascii_case("inherit") => {
                    current = self.get_node(node_id).parent;
                }
                Some(value)
                    if value.eq_ignore_ascii_case("normal")
                        || value.eq_ignore_ascii_case("none")
                        || value.eq_ignore_ascii_case("initial") =>
                {
                    return None;
                }
                Some(value) => return Some(value.to_string()),
                None => return None,
            }
        }
        None
    }

    fn render_child_ids(
        &self,
        parent_id: NodeId,
        child_ids: Vec<NodeId>,
        ancestors: &[stylesheet::SelectorContext],
        parent_style: &w3cos_std::style::Style,
    ) -> Vec<NodeId> {
        use w3cos_std::style::{Display, WhiteSpace};

        // Comments, doctypes and processing instructions participate in the
        // DOM tree but never generate CSS boxes. Keeping placeholder Columns
        // for them breaks anonymous inline formatting and can add measurable
        // height in comment-heavy XHTML reftests.
        let child_ids = child_ids
            .into_iter()
            .filter(|child_id| {
                !matches!(
                    self.get_node(*child_id).node_type,
                    NodeType::Comment | NodeType::DocumentType | NodeType::ProcessingInstruction
                )
            })
            .collect::<Vec<_>>();

        if !matches!(
            parent_style.white_space,
            WhiteSpace::Normal | WhiteSpace::NoWrap
        ) {
            return child_ids;
        }

        let is_collapsible_whitespace = |child_id: NodeId| {
            let child = self.get_node(child_id);
            child.node_type == NodeType::Text
                && child
                    .text_content
                    .as_deref()
                    .is_none_or(is_only_css_whitespace)
        };
        if !child_ids.iter().copied().any(is_collapsible_whitespace) {
            return child_ids;
        }

        if matches!(parent_style.display, Display::Flex | Display::Grid) {
            return child_ids
                .into_iter()
                .filter(|child_id| !is_collapsible_whitespace(*child_id))
                .collect();
        }
        let mut child_ancestors = ancestors.to_vec();
        child_ancestors.push(self.selector_context(parent_id));
        let participates_in_inline_flow = child_ids
            .iter()
            .map(|child_id| {
                let child = self.get_node(*child_id);
                match child.node_type {
                    NodeType::Text if is_collapsible_whitespace(*child_id) => None,
                    NodeType::Text => Some(true),
                    NodeType::Element => {
                        let child_style =
                            self.computed_style(*child_id, &child_ancestors, Some(parent_style));
                        let mut participates = matches!(
                            child_style.display,
                            Display::Inline
                                | Display::InlineBlock
                                | Display::InlineFlex
                                | Display::InlineTable
                        );
                        if child_style.display == Display::Inline {
                            let mut grandchild_ancestors = child_ancestors.clone();
                            grandchild_ancestors.push(self.selector_context(*child_id));
                            let contains_in_flow_block = self.children_ids(*child_id).iter().any(
                                |grandchild_id| {
                                    let grandchild = self.get_node(*grandchild_id);
                                    if grandchild.node_type != NodeType::Element {
                                        return false;
                                    }
                                    let style = self.computed_style(
                                        *grandchild_id,
                                        &grandchild_ancestors,
                                        Some(&child_style),
                                    );
                                    matches!(
                                        style.display,
                                        Display::Block | Display::Flex | Display::Grid
                                    ) && !matches!(
                                        style.position,
                                        w3cos_std::style::Position::Absolute
                                            | w3cos_std::style::Position::Fixed
                                    )
                                },
                            );
                            participates &= !contains_in_flow_block;
                        }
                        Some(participates)
                    }
                    _ => None,
                }
            })
            .collect::<Vec<_>>();

        child_ids
            .into_iter()
            .enumerate()
            .filter_map(|(index, child_id)| {
                if !is_collapsible_whitespace(child_id) {
                    return Some(child_id);
                }
                let inline_before = participates_in_inline_flow[..index]
                    .iter()
                    .rev()
                    .find_map(|participates| *participates)
                    .unwrap_or(false);
                let inline_after = participates_in_inline_flow[index + 1..]
                    .iter()
                    .find_map(|participates| *participates)
                    .unwrap_or(false);
                (inline_before && inline_after).then_some(child_id)
            })
            .collect()
    }

    fn rendered_text_content(
        &self,
        child_ids: &[NodeId],
        index: usize,
        ancestors: &[stylesheet::SelectorContext],
        parent_style: &w3cos_std::style::Style,
    ) -> String {
        use w3cos_std::style::{Display, WhiteSpace};

        let child = self.get_node(child_ids[index]);
        let raw = child.text_content.as_deref().unwrap_or_default();
        if !matches!(
            parent_style.white_space,
            WhiteSpace::Normal | WhiteSpace::NoWrap
        ) {
            return raw.to_string();
        }

        let sibling_inline_state = |sibling_id: NodeId| {
            let sibling = self.get_node(sibling_id);
            match sibling.node_type {
                NodeType::Text => Some(true),
                NodeType::Element => Some(matches!(
                    self.computed_style(sibling_id, ancestors, Some(parent_style))
                        .display,
                    Display::Inline
                        | Display::InlineBlock
                        | Display::InlineFlex
                        | Display::InlineTable
                )),
                _ => None,
            }
        };
        let inline_before = child_ids[..index]
            .iter()
            .rev()
            .find_map(|sibling| sibling_inline_state(*sibling))
            .unwrap_or(false);
        let inline_after = child_ids[index + 1..]
            .iter()
            .find_map(|sibling| sibling_inline_state(*sibling))
            .unwrap_or(false);
        collapse_css_whitespace(raw, inline_before, inline_after)
    }

    fn child_components(
        &self,
        child_ids: &[NodeId],
        text_context_ids: &[NodeId],
        ancestors: &mut Vec<stylesheet::SelectorContext>,
        parent_style: &w3cos_std::style::Style,
    ) -> Vec<w3cos_std::Component> {
        let mut components = Vec::new();
        for &child_id in child_ids {
            let child = self.get_node(child_id);
            if child.node_type == NodeType::Element {
                let child_style = self.computed_style(child_id, ancestors, Some(parent_style));
                if child_style.display == w3cos_std::style::Display::Contents {
                    ancestors.push(self.selector_context(child_id));
                    let nested_ids = self.render_child_ids(
                        child_id,
                        self.children_ids(child_id),
                        ancestors,
                        &child_style,
                    );
                    components.extend(self.child_components(
                        &nested_ids,
                        &nested_ids,
                        ancestors,
                        &child_style,
                    ));
                    ancestors.pop();
                    continue;
                }
            }

            let mut component = self.node_to_component(child_id, ancestors, Some(parent_style));
            if child.node_type == NodeType::Text
                && let w3cos_std::component::ComponentKind::Text { content } = &mut component.kind
            {
                let index = text_context_ids
                    .iter()
                    .position(|candidate| *candidate == child_id)
                    .unwrap_or_default();
                *content =
                    self.rendered_text_content(text_context_ids, index, ancestors, parent_style);
            }
            components.push(component);
        }
        components
    }

    fn coalesced_inline_text_run(
        &self,
        child_ids: &[NodeId],
        components: &[w3cos_std::Component],
        parent_style: &w3cos_std::style::Style,
        nowrap: bool,
    ) -> Option<w3cos_std::Component> {
        if child_ids.len() != components.len() {
            return None;
        }

        let same_text_style = |style: &w3cos_std::style::Style| {
            style.color == parent_style.color
                && style.font_size == parent_style.font_size
                && style.font_weight == parent_style.font_weight
                && style.font_family == parent_style.font_family
                && style.font_style == parent_style.font_style
                && style.line_height == parent_style.line_height
                && style.letter_spacing == parent_style.letter_spacing
                && style.text_decoration == parent_style.text_decoration
                && style.white_space == parent_style.white_space
        };
        fn append_text(
            component: &w3cos_std::Component,
            parent_style: &w3cos_std::style::Style,
            output: &mut String,
            first_style: &mut Option<w3cos_std::style::Style>,
        ) -> bool {
            let same_text_style = component.style.color == parent_style.color
                && component.style.font_size == parent_style.font_size
                && component.style.font_weight == parent_style.font_weight
                && component.style.font_family == parent_style.font_family
                && component.style.font_style == parent_style.font_style
                && component.style.line_height == parent_style.line_height
                && component.style.letter_spacing == parent_style.letter_spacing
                && component.style.text_decoration == parent_style.text_decoration
                && component.style.white_space == parent_style.white_space;
            if let w3cos_std::ComponentKind::Text { content } = &component.kind {
                if !same_text_style || !component.children.is_empty() {
                    return false;
                }
                first_style.get_or_insert_with(|| component.style.clone());
                output.push_str(content);
                return true;
            }
            if component.children.is_empty() {
                // State-only generated quote boxes can leave an empty,
                // transparent inline wrapper. It contributes no glyph or box
                // edge and must not split the surrounding inline text run.
                // Replaced leaves remain non-collapsible because their kind is
                // neither a Row nor a Box.
                return matches!(
                    component.kind,
                    w3cos_std::ComponentKind::Row | w3cos_std::ComponentKind::Box
                ) && matches!(
                    component.style.display,
                    w3cos_std::style::Display::Inline
                        | w3cos_std::style::Display::InlineFlex
                        | w3cos_std::style::Display::InlineBlock
                ) && principal_box_can_merge_generated_inline_text(&component.style);
            }
            component
                .children
                .iter()
                .all(|child| append_text(child, parent_style, output, first_style))
        }
        let mut content = String::new();
        let mut text_style = None;
        for (child_id, component) in child_ids.iter().zip(components) {
            let child = self.get_node(*child_id);
            match child.node_type {
                NodeType::Text => {
                    let w3cos_std::ComponentKind::Text { content: text } = &component.kind else {
                        return None;
                    };
                    if !same_text_style(&component.style) {
                        return None;
                    }
                    text_style.get_or_insert_with(|| component.style.clone());
                    content.push_str(text);
                }
                NodeType::Element => {
                    if child.tag.as_str().eq_ignore_ascii_case("br")
                        && !self.events.has_listeners(*child_id)
                        && component.children.is_empty()
                    {
                        content.push('\u{2028}');
                        continue;
                    }
                    if !self.passive_generated_inline_subtree(*child_id) {
                        return None;
                    }
                    if !append_text(component, parent_style, &mut content, &mut text_style) {
                        return None;
                    }
                }
                _ => return None,
            }
        }
        if child_ids.len() < 2 {
            return None;
        }
        let mut style = text_style?;
        style.display = if nowrap {
            w3cos_std::style::Display::InlineBlock
        } else {
            w3cos_std::style::Display::Inline
        };
        Some(w3cos_std::Component::text(content, style))
    }

    fn coalesced_generated_text_run(
        &self,
        components: &[w3cos_std::Component],
        parent_style: &w3cos_std::style::Style,
    ) -> Option<w3cos_std::Component> {
        if components.len() < 2 {
            return None;
        }
        let mut content = String::new();
        let mut text_style = None;
        for component in components {
            let w3cos_std::ComponentKind::Text {
                content: component_text,
            } = &component.kind
            else {
                return None;
            };
            if !component.children.is_empty()
                || component.style.position != w3cos_std::style::Position::Static
                || component.style.padding != w3cos_std::style::Edges::ZERO
                || component.style.margin != w3cos_std::style::Edges::ZERO
                || component.style.border_width != 0.0
                || component.style.background.a != 0
                || component.style.color != parent_style.color
                || component.style.font_size != parent_style.font_size
                || component.style.font_weight != parent_style.font_weight
                || component.style.font_family != parent_style.font_family
                || component.style.font_style != parent_style.font_style
                || component.style.line_height != parent_style.line_height
                || component.style.letter_spacing != parent_style.letter_spacing
                || component.style.text_decoration != parent_style.text_decoration
                || component.style.white_space != parent_style.white_space
            {
                return None;
            }
            text_style.get_or_insert_with(|| component.style.clone());
            content.push_str(component_text);
        }
        Some(w3cos_std::Component::text(content, text_style?))
    }

    fn passive_generated_inline_subtree(&self, id: NodeId) -> bool {
        let node = self.get_node(id);
        if node.node_type == NodeType::Text {
            return true;
        }
        if node.node_type != NodeType::Element
            || self.events.has_listeners(id)
            || !matches!(
                self.computed_style_for(id).display,
                w3cos_std::style::Display::Inline
                    | w3cos_std::style::Display::InlineBlock
                    | w3cos_std::style::Display::InlineFlex
            )
            || self.styles[id.0 as usize]
                .inline_declarations
                .iter()
                .any(|(property, value)| !passive_generated_inline_declaration(property, value))
            || stylesheet::matching_declarations_for_node(self, id)
                .iter()
                .any(|(property, value, _)| !passive_generated_inline_declaration(property, value))
        {
            return false;
        }
        self.children_ids(id)
            .into_iter()
            .filter(|child| {
                !matches!(
                    self.get_node(*child).node_type,
                    NodeType::Comment | NodeType::DocumentType | NodeType::ProcessingInstruction
                )
            })
            .all(|child| self.passive_generated_inline_subtree(child))
    }

    fn node_to_component(
        &self,
        id: NodeId,
        ancestors: &mut Vec<stylesheet::SelectorContext>,
        inherited: Option<&w3cos_std::style::Style>,
    ) -> w3cos_std::Component {
        let node = self.get_node(id);
        let mut style = self.computed_style(id, ancestors, inherited);
        let tag = node.tag.as_str();
        self.apply_svg_presentation_style(id, &tag, &mut style);

        match node.node_type {
            NodeType::Text | NodeType::CdataSection | NodeType::ProcessingInstruction => {
                let text = node.text_content.as_deref().unwrap_or("");
                // Text nodes do not generate principal CSS boxes of their
                // own. In the component IR, however, a nowrap text leaf must
                // carry an intrinsic inline width so anonymous line-box
                // whitespace advances exactly like browser text shaping.
                style.display = if style.white_space == w3cos_std::style::WhiteSpace::NoWrap {
                    w3cos_std::style::Display::InlineBlock
                } else {
                    w3cos_std::style::Display::Inline
                };
                w3cos_std::Component::text(text, style)
            }
            NodeType::Comment | NodeType::DocumentType => {
                return w3cos_std::Component::column(style, vec![]);
            }
            NodeType::Element | NodeType::Document | NodeType::DocumentFragment => {
                if tag.eq_ignore_ascii_case("br") {
                    // Preserve the forced-break semantics in portable IR. A
                    // zero-width marker paints nothing but still establishes
                    // the inherited line-height strut for following content.
                    style.display = w3cos_std::style::Display::Inline;
                    style.width = w3cos_std::style::Dimension::Px(0.0);
                    style.height = w3cos_std::style::Dimension::Px(
                        style.font_size * style.line_height,
                    );
                    return self.attach_native_host(
                        id,
                        w3cos_std::Component::text("\u{2028}", style),
                    );
                }
                if tag == "svg" {
                    let (attribute_width, attribute_height) = self.svg_root_size(id);
                    let width = match style.width {
                        w3cos_std::style::Dimension::Px(width) => width,
                        w3cos_std::style::Dimension::Em(width) => width * style.font_size,
                        w3cos_std::style::Dimension::Rem(width) => width * 16.0,
                        _ => attribute_width,
                    }
                    .max(1.0)
                    .ceil() as u32;
                    let height = match style.height {
                        w3cos_std::style::Dimension::Px(height) => height,
                        w3cos_std::style::Dimension::Em(height) => height * style.font_size,
                        w3cos_std::style::Dimension::Rem(height) => height * 16.0,
                        _ => attribute_height,
                    }
                    .max(1.0)
                    .ceil() as u32;
                    let (source, event_targets) = self.svg_markup(id);
                    let current_color = format!(
                        "rgba({}, {}, {}, {})",
                        style.color.r,
                        style.color.g,
                        style.color.b,
                        f32::from(style.color.a) / 255.0,
                    );
                    let source = source
                        .replace("currentColor", &current_color)
                        .replace("currentcolor", &current_color);
                    let component = w3cos_std::Component::svg_document_with_targets(
                        source,
                        width,
                        height,
                        event_targets,
                        style,
                    );
                    return self.attach_native_host(id, component);
                }

                // HTML parsers create real text-node children. Preserve the
                // inline element's own computed typography when lowering a
                // simple `<span>text</span>` instead of wrapping the text in
                // a zero-width container with an unrelated default style.
                let mut child_ids = self.children_ids(id);
                let mut before = self.generated_pseudo_component(id, "::before", &style);
                let mut after = self.generated_pseudo_component(id, "::after", &style);
                if tag == "details"
                    && !node
                        .attributes
                        .iter()
                        .any(|(name, _)| name.as_str().eq_ignore_ascii_case("open"))
                {
                    child_ids = child_ids
                        .into_iter()
                        .find(|child_id| {
                            let child = self.get_node(*child_id);
                            child.node_type == NodeType::Element
                                && child.tag.as_str().eq_ignore_ascii_case("summary")
                        })
                        .into_iter()
                        .collect();
                }
                child_ids = self.render_child_ids(id, child_ids, ancestors, &style);
                if child_ids.is_empty()
                    && !self.events.has_listeners(id)
                    && principal_box_can_collapse_generated_text(&style)
                {
                    let generated = [before.as_ref(), after.as_ref()]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>();
                    if !generated.is_empty()
                        && generated.iter().all(|component| {
                            matches!(component.kind, w3cos_std::ComponentKind::Text { .. })
                                && component.children.is_empty()
                                && component.style.position == w3cos_std::style::Position::Static
                                && component.style.padding == w3cos_std::style::Edges::ZERO
                                && component.style.margin == w3cos_std::style::Edges::ZERO
                                && component.style.border_width == 0.0
                                && component.style.background.a == 0
                        })
                        && generated[1..].iter().all(|component| {
                            component.style.color == generated[0].style.color
                                && component.style.font_size == generated[0].style.font_size
                                && component.style.font_weight == generated[0].style.font_weight
                                && component.style.font_family == generated[0].style.font_family
                                && component.style.font_style == generated[0].style.font_style
                                && component.style.line_height == generated[0].style.line_height
                                && component.style.letter_spacing
                                    == generated[0].style.letter_spacing
                                && component.style.text_decoration
                                    == generated[0].style.text_decoration
                        })
                    {
                        let content = generated
                            .iter()
                            .filter_map(|component| match &component.kind {
                                w3cos_std::ComponentKind::Text { content } => {
                                    Some(content.as_str())
                                }
                                _ => None,
                            })
                            .collect::<String>();
                        let generated_style = &generated[0].style;
                        style.color = generated_style.color;
                        style.font_size = generated_style.font_size;
                        style.font_weight = generated_style.font_weight;
                        style.font_family = generated_style.font_family.clone();
                        style.font_style = generated_style.font_style;
                        style.line_height = generated_style.line_height;
                        style.letter_spacing = generated_style.letter_spacing;
                        style.text_decoration = generated_style.text_decoration;
                        return self
                            .attach_native_host(id, w3cos_std::Component::text(content, style));
                    }
                }
                if matches!(
                    tag.as_str(),
                    "abbr"
                        | "b"
                        | "button"
                        | "a"
                        | "span"
                        | "label"
                        | "em"
                        | "i"
                        | "p"
                        | "strong"
                        | "code"
                        | "small"
                        | "h1"
                        | "h2"
                        | "h3"
                        | "h4"
                        | "h5"
                        | "h6"
                ) && child_ids.len() == 1
                    && before.is_none()
                    && after.is_none()
                {
                    let child = self.get_node(child_ids[0]);
                    if child.node_type == NodeType::Text {
                        let text = child.text_content.as_deref().unwrap_or("");
                        let component = match tag.as_str() {
                            "button" => w3cos_std::Component::button(text, style),
                            _ => w3cos_std::Component::text(text, style),
                        };
                        return self.attach_native_host(id, component);
                    }

                    // Browser inline formatting does not map directly onto
                    // Taffy's flex layout. In particular, Monaco emits each
                    // line as `span[absolute] > span.mtkN > #text`; keeping
                    // the outer span as an absolutely-positioned flex
                    // container gives it zero width and makes the otherwise
                    // valid Text component invisible. Collapse a transparent
                    // one-child inline wrapper and use the innermost
                    // element's computed typography.
                    let child_tag = child.tag.as_str();
                    if child.node_type == NodeType::Element
                        && matches!(
                            child_tag.as_str(),
                            "span" | "label" | "em" | "strong" | "code" | "small"
                        )
                    {
                        let grandchild_ids = self.children_ids(child_ids[0]);
                        if grandchild_ids.len() == 1 {
                            let grandchild = self.get_node(grandchild_ids[0]);
                            if grandchild.node_type == NodeType::Text {
                                ancestors.push(self.selector_context(id));
                                let child_style =
                                    self.computed_style(child_ids[0], ancestors, Some(&style));
                                ancestors.pop();
                                return self.attach_native_host(
                                    id,
                                    w3cos_std::Component::text(
                                        grandchild.text_content.as_deref().unwrap_or(""),
                                        child_style,
                                    ),
                                );
                            }
                        }
                    }
                }
                let pushed = if node.node_type == NodeType::Element {
                    ancestors.push(self.selector_context(id));
                    true
                } else {
                    false
                };
                let block_in_inline = style.display == w3cos_std::style::Display::Inline
                    && child_ids.iter().any(|child_id| {
                        let child = self.get_node(*child_id);
                        child.node_type == NodeType::Element
                            && matches!(
                                self.computed_style(*child_id, ancestors, Some(&style))
                                    .display,
                                w3cos_std::style::Display::Block
                                    | w3cos_std::style::Display::Flex
                                    | w3cos_std::style::Display::Grid
                            )
                    });
                if block_in_inline {
                    // CSS block-in-inline layout splits the inline around the
                    // in-flow block child and lays that child out against the
                    // surrounding containing block. The component IR has no
                    // fragmented inline boxes, so represent that formatting
                    // context as a block container instead of shrink-wrapping
                    // the child to the inline host (for example `a > div`).
                    style.display = w3cos_std::style::Display::Block;
                }
                let rendered_child_ids = child_ids
                    .iter()
                    .filter(|child_id| {
                        let child = self.get_node(**child_id);
                        !(block_in_inline
                            && child.node_type == NodeType::Text
                            && child
                                .text_content
                                .as_deref()
                                .is_none_or(is_only_css_whitespace))
                    })
                    .copied()
                    .collect::<Vec<_>>();
                let mut nowrap_inline_formatting_context = style.display
                    == w3cos_std::style::Display::Block
                    && style.white_space == w3cos_std::style::WhiteSpace::NoWrap
                    && rendered_child_ids.len() >= 2
                    && rendered_child_ids.iter().all(|child_id| {
                        let child = self.get_node(*child_id);
                        child.node_type != NodeType::Element
                            || {
                                let child_style =
                                    self.computed_style(*child_id, ancestors, Some(&style));
                                child_style.float == w3cos_std::style::Float::None
                                    && matches!(
                                        child_style.display,
                                        w3cos_std::style::Display::Inline
                                            | w3cos_std::style::Display::InlineBlock
                                            | w3cos_std::style::Display::InlineFlex
                                            | w3cos_std::style::Display::InlineTable
                                    )
                            }
                    });
                let mut children =
                    self.child_components(&rendered_child_ids, &child_ids, ancestors, &style);
                let mut anonymous_inline_formatting_context = false;
                if matches!(
                    style.white_space,
                    w3cos_std::style::WhiteSpace::Normal | w3cos_std::style::WhiteSpace::NoWrap
                ) {
                    let rendered_source_child = |from_start: bool| {
                        let children = self.children_ids(id);
                        let mut children = children.into_iter();
                        if from_start {
                            children.find(|child| {
                                !matches!(
                                    self.get_node(*child).node_type,
                                    NodeType::Comment
                                        | NodeType::DocumentType
                                        | NodeType::ProcessingInstruction
                                )
                            })
                        } else {
                            children.rev().find(|child| {
                                !matches!(
                                    self.get_node(*child).node_type,
                                    NodeType::Comment
                                        | NodeType::DocumentType
                                        | NodeType::ProcessingInstruction
                                )
                            })
                        }
                    };
                    if rendered_child_ids.is_empty()
                        && self.children_ids(id).iter().any(|child_id| {
                            let child = self.get_node(*child_id);
                            child.node_type == NodeType::Text
                                && child
                                    .text_content
                                    .as_deref()
                                    .is_some_and(|text| text.chars().any(is_css_whitespace))
                        })
                        && before.as_ref().is_some_and(|component| {
                            component.style.display == w3cos_std::style::Display::Inline
                        })
                        && after.as_ref().is_some_and(|component| {
                            component.style.display == w3cos_std::style::Display::Inline
                        })
                        && let Some(before_component) = before.as_mut()
                        && let w3cos_std::ComponentKind::Text { content } =
                            &mut before_component.kind
                        && !content.ends_with(' ')
                    {
                        // A whitespace-only source text node still separates
                        // two inline generated boxes. It collapses away at an
                        // outer edge, and before a block pseudo, but not in the
                        // inline `::before <space> ::after` boundary.
                        content.push(' ');
                    }
                    if !rendered_child_ids.is_empty()
                        && let Some(before_component) = before.as_mut()
                        && matches!(
                            before_component.style.display,
                            w3cos_std::style::Display::Inline
                                | w3cos_std::style::Display::InlineBlock
                                | w3cos_std::style::Display::InlineFlex
                                | w3cos_std::style::Display::InlineTable
                        )
                        && rendered_source_child(true).is_some_and(|first_id| {
                            let first = self.get_node(first_id);
                            first.node_type == NodeType::Text
                                && first.text_content.as_deref().is_some_and(|text| {
                                    text.chars().next().is_some_and(char::is_whitespace)
                                })
                        })
                    {
                        let preserved_on_source = rendered_source_child(true)
                            .and_then(|first_id| {
                                rendered_child_ids
                                    .iter()
                                    .position(|candidate| *candidate == first_id)
                            })
                            .and_then(|index| children.get_mut(index))
                            .and_then(|component| match &mut component.kind {
                                w3cos_std::ComponentKind::Text { content } => Some(content),
                                _ => None,
                            })
                            .is_some_and(|content| {
                                if !content.starts_with(' ') {
                                    content.insert(0, ' ');
                                }
                                true
                            });
                        if !preserved_on_source
                            && let w3cos_std::ComponentKind::Text { content } =
                                &mut before_component.kind
                            && !content.ends_with(' ')
                        {
                            content.push(' ');
                        }
                    }
                    if !rendered_child_ids.is_empty()
                        && let Some(after_component) = after.as_mut()
                        && matches!(
                            after_component.style.display,
                            w3cos_std::style::Display::Inline
                                | w3cos_std::style::Display::InlineBlock
                                | w3cos_std::style::Display::InlineFlex
                                | w3cos_std::style::Display::InlineTable
                        )
                        && rendered_source_child(false).is_some_and(|last_id| {
                            let last = self.get_node(last_id);
                            last.node_type == NodeType::Text
                                && last.text_content.as_deref().is_some_and(|text| {
                                    text.chars().next_back().is_some_and(char::is_whitespace)
                                })
                        })
                    {
                        let preserved_on_source = rendered_source_child(false)
                            .and_then(|last_id| {
                                rendered_child_ids
                                    .iter()
                                    .position(|candidate| *candidate == last_id)
                            })
                            .and_then(|index| children.get_mut(index))
                            .and_then(|component| match &mut component.kind {
                                w3cos_std::ComponentKind::Text { content } => Some(content),
                                _ => None,
                            })
                            .is_some_and(|content| {
                                if !content.ends_with(' ') {
                                    content.push(' ');
                                }
                                true
                            });
                        if !preserved_on_source
                            && let w3cos_std::ComponentKind::Text { content } =
                                &mut after_component.kind
                            && !content.starts_with(' ')
                        {
                            content.insert(0, ' ');
                        }
                    }
                }
                if style.display == w3cos_std::style::Display::Block {
                    let first_visible_is_block = children
                        .iter()
                        .find(|component| {
                            component.style.display != w3cos_std::style::Display::None
                        })
                        .is_some_and(|component| {
                            matches!(
                                component.style.display,
                                w3cos_std::style::Display::Block
                                    | w3cos_std::style::Display::Flex
                                    | w3cos_std::style::Display::Grid
                            )
                        });
                    let last_visible_is_block = children
                        .iter()
                        .rev()
                        .find(|component| {
                            component.style.display != w3cos_std::style::Display::None
                        })
                        .is_some_and(|component| {
                            matches!(
                                component.style.display,
                                w3cos_std::style::Display::Block
                                    | w3cos_std::style::Display::Flex
                                    | w3cos_std::style::Display::Grid
                            )
                        });
                    if first_visible_is_block
                        && before
                            .as_ref()
                            .is_some_and(collapsible_generated_whitespace)
                    {
                        before = None;
                    }
                    if last_visible_is_block
                        && after.as_ref().is_some_and(collapsible_generated_whitespace)
                    {
                        after = None;
                    }
                }
                let has_generated_content = before.is_some() || after.is_some();
                if let Some(before) = before {
                    children.insert(0, before);
                }
                if tag == "li"
                    && let Some(marker) = self.list_marker_component(id, &style)
                {
                    children.insert(0, marker);
                }
                if let Some(after) = after {
                    children.push(after);
                }
                if matches!(
                    style.display,
                    w3cos_std::style::Display::Block
                        | w3cos_std::style::Display::InlineBlock
                        | w3cos_std::style::Display::ListItem
                        | w3cos_std::style::Display::TableCell
                ) {
                    children = hoist_floats_into_block_formatting_context(&style, children);
                }
                if style.display == w3cos_std::style::Display::Table {
                    // CSS table fixup has a semantic group order independent
                    // of DOM/pseudo source order: header groups precede row
                    // groups and footer groups follow them. Keep ordering
                    // stable within each group class.
                    children.sort_by_key(|component| match component.style.display {
                        w3cos_std::style::Display::TableCaption => 0,
                        w3cos_std::style::Display::TableColumnGroup
                        | w3cos_std::style::Display::TableColumn => 1,
                        w3cos_std::style::Display::TableHeaderGroup => 2,
                        w3cos_std::style::Display::TableFooterGroup => 4,
                        _ => 3,
                    });
                }
                if rendered_child_ids.is_empty()
                    && let Some(text_run) = self.coalesced_generated_text_run(&children, &style)
                {
                    children = vec![text_run];
                }
                if children.len() >= 2
                    && rendered_child_ids.iter().all(|child_id| {
                        let child = self.get_node(*child_id);
                        child.node_type == NodeType::Text
                            || (child.node_type == NodeType::Element
                                && !self.events.has_listeners(*child_id)
                                && matches!(
                                    self.computed_style(*child_id, ancestors, Some(&style))
                                        .display,
                                    w3cos_std::style::Display::Inline
                                        | w3cos_std::style::Display::InlineBlock
                                        | w3cos_std::style::Display::InlineFlex
                                        | w3cos_std::style::Display::InlineTable
                                ))
                    })
                    && children.iter().all(|component| {
                        !matches!(
                            component.style.position,
                            w3cos_std::style::Position::Absolute
                                | w3cos_std::style::Position::Fixed
                        ) && matches!(
                            component.style.display,
                            w3cos_std::style::Display::Inline
                                | w3cos_std::style::Display::InlineBlock
                                | w3cos_std::style::Display::InlineFlex
                                | w3cos_std::style::Display::InlineTable
                        )
                    })
                {
                    let coalesced = if children.len() == rendered_child_ids.len() {
                        self.coalesced_inline_text_run(
                            &rendered_child_ids,
                            &children,
                            &style,
                            false,
                        )
                    } else {
                        self.coalesced_generated_text_run(&children, &style)
                    };
                    if let Some(text_run) = coalesced {
                        children = vec![text_run];
                    } else {
                        anonymous_inline_formatting_context = true;
                    }
                }
                if has_generated_content
                    && !self.events.has_listeners(id)
                    && principal_box_can_merge_generated_inline_text(&style)
                    && matches!(
                        tag.as_str(),
                        "abbr" | "a" | "span" | "label" | "em" | "i" | "strong" | "code" | "small"
                    )
                    && children.len() == 1
                    && children[0].children.is_empty()
                    && matches!(
                        children[0].style.display,
                        w3cos_std::style::Display::Inline
                            | w3cos_std::style::Display::InlineBlock
                            | w3cos_std::style::Display::InlineFlex
                            | w3cos_std::style::Display::InlineTable
                    )
                    && let w3cos_std::ComponentKind::Text { content } = &children[0].kind
                {
                    let mut text_style = children[0].style.clone();
                    text_style.position = style.position;
                    text_style.top = style.top;
                    text_style.right = style.right;
                    text_style.bottom = style.bottom;
                    text_style.left = style.left;
                    text_style.z_index = style.z_index;
                    text_style.order = style.order;
                    text_style.align_self = style.align_self;
                    text_style.flex_grow = style.flex_grow;
                    text_style.flex_shrink = style.flex_shrink;
                    text_style.flex_basis = style.flex_basis;
                    text_style.cursor = style.cursor;
                    text_style.pointer_events = style.pointer_events;
                    text_style.user_select = style.user_select;
                    return self.attach_native_host(
                        id,
                        w3cos_std::Component::text(content.clone(), text_style),
                    );
                }
                if nowrap_inline_formatting_context
                    && let Some(text_run) =
                        self.coalesced_inline_text_run(&rendered_child_ids, &children, &style, true)
                {
                    children = vec![text_run];
                    nowrap_inline_formatting_context = false;
                }
                if style.display == w3cos_std::style::Display::Block
                    && rendered_child_ids.len() >= 2
                    && rendered_child_ids.iter().all(|child_id| {
                        let child = self.get_node(*child_id);
                        child.node_type == NodeType::Text
                            || (matches!(
                                self.computed_style(*child_id, ancestors, Some(&style))
                                    .display,
                                w3cos_std::style::Display::Inline
                                    | w3cos_std::style::Display::InlineBlock
                                    | w3cos_std::style::Display::InlineFlex
                            ) && self
                                .computed_style(*child_id, ancestors, Some(&style))
                                .float
                                == w3cos_std::style::Float::None
                                && self.passive_generated_inline_subtree(*child_id))
                    })
                    && let Some(text_run) = self.coalesced_inline_text_run(
                        &rendered_child_ids,
                        &children,
                        &style,
                        false,
                    )
                {
                    children = vec![text_run];
                }
                if pushed {
                    ancestors.pop();
                }

                children = fixup_css_table_children(&style, children);

                if style.display == w3cos_std::style::Display::InlineBlock
                    && children.iter().any(|child| {
                        matches!(
                            child.style.display,
                            w3cos_std::style::Display::Block
                                | w3cos_std::style::Display::Flex
                                | w3cos_std::style::Display::Grid
                        )
                    })
                {
                    children.retain(|child| {
                        let has_active_host = !matches!(
                            child.on_click,
                            w3cos_std::EventAction::None
                                | w3cos_std::EventAction::NativeHost {
                                    click: false,
                                    scroll: false,
                                    input: false,
                                    focus: false,
                                    keyboard: false,
                                    submit: false,
                                    wheel: false,
                                    ..
                                }
                        );
                        !(child.children.is_empty()
                            && !has_active_host
                            && matches!(
                                child.kind,
                                w3cos_std::ComponentKind::Row | w3cos_std::ComponentKind::Box
                            )
                            && child.style.display == w3cos_std::style::Display::Inline
                            && principal_box_can_merge_generated_inline_text(&child.style))
                    });
                }

                if style.display == w3cos_std::style::Display::InlineBlock
                    && matches!(
                        style.align_self,
                        w3cos_std::style::AlignSelf::Auto | w3cos_std::style::AlignSelf::Baseline
                    )
                    && !children.is_empty()
                    && children.iter().all(|child| {
                        matches!(
                            child.style.display,
                            w3cos_std::style::Display::Block
                                | w3cos_std::style::Display::Flex
                                | w3cos_std::style::Display::Grid
                                | w3cos_std::style::Display::None
                        )
                    })
                    && children.iter().any(component_has_non_whitespace_text)
                {
                    // An inline-block with no in-flow line boxes uses its
                    // bottom margin edge as the baseline. Taffy otherwise
                    // leaks a text baseline from a block-level descendant.
                    style.align_self = w3cos_std::style::AlignSelf::FlexEnd;
                }

                if let Some(text) = &node.text_content {
                    if children.is_empty() {
                        let component = match tag.as_str() {
                            "button" => w3cos_std::Component::button(text, style),
                            _ => w3cos_std::Component::text(text, style),
                        };
                        return self.attach_native_host(id, component);
                    }
                }

                // Taffy intentionally has no browser inline-formatting
                // context. A nowrap block containing only inline-level boxes
                // is equivalent to one anonymous horizontal line box, so
                // lower that visual box as flex-row while retaining the DOM
                // children and their independently styled paint nodes.
                if nowrap_inline_formatting_context || anonymous_inline_formatting_context {
                    let has_negative_horizontal_margin = children.iter().any(|child| {
                        let is_negative = |spacing| {
                            matches!(
                                spacing,
                                w3cos_std::style::Spacing::Px(value)
                                    | w3cos_std::style::Spacing::Percent(value)
                                    | w3cos_std::style::Spacing::Rem(value)
                                    | w3cos_std::style::Spacing::Em(value)
                                    | w3cos_std::style::Spacing::Vw(value)
                                    | w3cos_std::style::Spacing::Vh(value)
                                    if value < 0.0
                            )
                        };
                        is_negative(child.style.margin.left)
                            || is_negative(child.style.margin.right)
                    });
                    let uses_inline_strut_wrappers = anonymous_inline_formatting_context
                        && has_negative_horizontal_margin
                        && children.iter().any(|child| {
                            !matches!(child.kind, w3cos_std::ComponentKind::Text { .. })
                        });
                    if uses_inline_strut_wrappers {
                        let line_height = (style.font_size * style.line_height).max(0.0);
                        children = children
                            .into_iter()
                            .map(|mut child| {
                                // Flex items shrink by default, while inline
                                // boxes retain their outer width and move to a
                                // new line when the remaining line is too
                                // narrow. Keep the authored box as the paint
                                // and event target inside a transparent line
                                // item so replaced elements also inherit the
                                // containing inline strut without changing
                                // their own painted height.
                                child.style.flex_shrink = 0.0;
                                let child_margin = child.style.margin;
                                let outer_width = match (
                                    child.style.width,
                                    child_margin.left,
                                    child_margin.right,
                                ) {
                                    (
                                        w3cos_std::style::Dimension::Px(width),
                                        w3cos_std::style::Spacing::Px(left),
                                        w3cos_std::style::Spacing::Px(right),
                                    ) => w3cos_std::style::Dimension::Px(width + left + right),
                                    (
                                        w3cos_std::style::Dimension::Em(width),
                                        w3cos_std::style::Spacing::Em(left),
                                        w3cos_std::style::Spacing::Em(right),
                                    ) => w3cos_std::style::Dimension::Em(width + left + right),
                                    (
                                        w3cos_std::style::Dimension::Em(width),
                                        w3cos_std::style::Spacing::Em(left),
                                        w3cos_std::style::Spacing::Px(right),
                                    ) if right == 0.0 => {
                                        w3cos_std::style::Dimension::Em(width + left)
                                    }
                                    (
                                        w3cos_std::style::Dimension::Em(width),
                                        w3cos_std::style::Spacing::Px(left),
                                        w3cos_std::style::Spacing::Em(right),
                                    ) if left == 0.0 => {
                                        w3cos_std::style::Dimension::Em(width + right)
                                    }
                                    (
                                        w3cos_std::style::Dimension::Rem(width),
                                        w3cos_std::style::Spacing::Rem(left),
                                        w3cos_std::style::Spacing::Rem(right),
                                    ) => w3cos_std::style::Dimension::Rem(width + left + right),
                                    _ => child.style.width,
                                };
                                let mut line_item_style =
                                    w3cos_std::style::Style::default();
                                line_item_style.display =
                                    w3cos_std::style::Display::InlineFlex;
                                line_item_style.flex_direction =
                                    w3cos_std::style::FlexDirection::Row;
                                line_item_style.align_items =
                                    w3cos_std::style::AlignItems::FlexEnd;
                                line_item_style.flex_shrink = 0.0;
                                line_item_style.font_size = child.style.font_size;
                                line_item_style.font_family = child.style.font_family.clone();
                                line_item_style.line_height = child.style.line_height;
                                line_item_style.width = outer_width;
                                if !matches!(outer_width, w3cos_std::style::Dimension::Auto) {
                                    line_item_style.min_width = outer_width;
                                    line_item_style.max_width = outer_width;
                                }
                                line_item_style.min_height =
                                    w3cos_std::style::Dimension::Px(line_height);
                                // The wrapper is the flex item, so use the
                                // authored outer width for line fitting while
                                // leaving the margin on the real paint box.
                                // This lets a negative margin pull its box into
                                // the preceding inline without making Taffy's
                                // greedy flex wrapper split an otherwise valid
                                // line first.
                                w3cos_std::Component::boxed(line_item_style, vec![child])
                            })
                            .collect();
                    }
                    if anonymous_inline_formatting_context
                        && matches!(
                            style.display,
                            w3cos_std::style::Display::Table
                                | w3cos_std::style::Display::TableRowGroup
                                | w3cos_std::style::Display::TableHeaderGroup
                                | w3cos_std::style::Display::TableFooterGroup
                                | w3cos_std::style::Display::TableCell
                                | w3cos_std::style::Display::TableCaption
                        )
                    {
                        // Table fixup keeps the authored table-part principal
                        // box and creates an anonymous inline row inside it.
                        // This is the same IR shape used for mixed generated
                        // content with the corresponding `display` value.
                        let mut line_style = w3cos_std::style::Style::default();
                        line_style.display = w3cos_std::style::Display::Flex;
                        line_style.flex_direction = w3cos_std::style::FlexDirection::Row;
                        line_style.align_items = if uses_inline_strut_wrappers {
                            w3cos_std::style::AlignItems::FlexStart
                        } else {
                            w3cos_std::style::AlignItems::Baseline
                        };
                        children = vec![w3cos_std::Component::row(line_style, children)];
                    } else if style.display == w3cos_std::style::Display::TableRow {
                        style.flex_direction = w3cos_std::style::FlexDirection::Row;
                        style.align_items = if uses_inline_strut_wrappers {
                            w3cos_std::style::AlignItems::FlexStart
                        } else {
                            w3cos_std::style::AlignItems::Baseline
                        };
                        if anonymous_inline_formatting_context
                            && style.white_space != w3cos_std::style::WhiteSpace::NoWrap
                        {
                            style.flex_wrap = w3cos_std::style::FlexWrap::Wrap;
                        }
                    } else {
                        style.display = if matches!(
                            style.display,
                            w3cos_std::style::Display::Inline
                                | w3cos_std::style::Display::InlineBlock
                                | w3cos_std::style::Display::InlineFlex
                                | w3cos_std::style::Display::InlineTable
                        ) {
                            w3cos_std::style::Display::InlineFlex
                        } else {
                            w3cos_std::style::Display::Flex
                        };
                        style.flex_direction = w3cos_std::style::FlexDirection::Row;
                        style.align_items = if uses_inline_strut_wrappers {
                            w3cos_std::style::AlignItems::FlexStart
                        } else {
                            w3cos_std::style::AlignItems::Baseline
                        };
                        if anonymous_inline_formatting_context
                            && style.white_space != w3cos_std::style::WhiteSpace::NoWrap
                        {
                            style.flex_wrap = w3cos_std::style::FlexWrap::Wrap;
                        }
                    }
                }

                normalize_css_table_internal_used_style(&mut style);

                let is_row = matches!(
                    style.flex_direction,
                    w3cos_std::style::FlexDirection::Row
                        | w3cos_std::style::FlexDirection::RowReverse
                );

                let mut component = match tag.as_str() {
                    "svg" | "g" | "defs" => w3cos_std::Component::boxed(style, children),
                    "rect" | "circle" | "ellipse" | "line" | "use" => {
                        w3cos_std::Component::boxed(style, children)
                    }
                    "polyline" | "polygon" | "path" => self
                        .svg_path_component(id, &tag, style.clone())
                        .unwrap_or_else(|| w3cos_std::Component::boxed(style, children)),
                    "text" => {
                        let text = self.descendant_text_content(id);
                        w3cos_std::Component::text(text, style)
                    }
                    "body" | "div" | "section" | "main" | "article" | "nav" | "header"
                    | "footer" | "aside" | "form" | "fieldset" | "ul" | "ol" | "dl" => {
                        if is_row {
                            w3cos_std::Component::row(style, children)
                        } else {
                            w3cos_std::Component::column(style, children)
                        }
                    }
                    "span" | "label" | "em" | "strong" | "code" | "small" | "li" | "dd" | "dt" => {
                        if let Some(text) = &node.text_content {
                            if children.is_empty() {
                                return self.attach_native_host(
                                    id,
                                    w3cos_std::Component::text(text, style),
                                );
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
                                return self.attach_native_host(
                                    id,
                                    w3cos_std::Component::text(text, style),
                                );
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
                        let label = self.descendant_text_content(id);
                        let has_visual_children = !children.is_empty();
                        let paint_label = if has_visual_children {
                            ""
                        } else if label.is_empty() {
                            "Button"
                        } else {
                            &label
                        };
                        // A non-leaf DOM button paints through its child nodes.
                        // Keeping the descendant text in Button::label would
                        // make every renderer paint it a second time.
                        let mut button = w3cos_std::Component::button(paint_label, style);
                        button.children = children;
                        button
                    }
                    "select" => {
                        // A collapsed HTML select paints only its current option.
                        // Lowering every `<option>` as a normal child makes the
                        // intrinsic width equal to the concatenated option list
                        // and pushes the surrounding flex row beyond the viewport.
                        let selected_value = node
                            .attributes
                            .iter()
                            .find(|(key, _)| key.as_str() == "value")
                            .map(|(_, value)| value.as_str());
                        let option_ids = self.children_ids(id);
                        let selected = option_ids
                            .iter()
                            .find(|&&option_id| {
                                let option = self.get_node(option_id);
                                let option_value = option
                                    .attributes
                                    .iter()
                                    .find(|(key, _)| key.as_str() == "value")
                                    .map(|(_, value)| value.as_str())
                                    .unwrap_or_default();
                                option
                                    .attributes
                                    .iter()
                                    .any(|(key, _)| key.as_str() == "selected")
                                    || selected_value.is_some_and(|value| value == option_value)
                            })
                            .copied()
                            .or_else(|| option_ids.first().copied());
                        let label = selected
                            .map(|option_id| self.descendant_text_content(option_id))
                            .unwrap_or_default();
                        w3cos_std::Component::button(&label, style)
                    }
                    "img" => {
                        let src = self
                            .image_render_sources
                            .get(&id)
                            .map(String::as_str)
                            .or_else(|| {
                                node.attributes
                                    .iter()
                                    .find(|(k, _)| k.as_str() == "src")
                                    .map(|(_, v)| v.as_str())
                            })
                            .unwrap_or("");
                        let mut image_style = style;
                        if matches!(image_style.width, w3cos_std::style::Dimension::Auto)
                            && let Some(width) = node
                                .attributes
                                .iter()
                                .find(|(key, _)| key.as_str() == "width")
                                .and_then(|(_, value)| {
                                    parse_html_dimension_attribute(value.as_str())
                                })
                        {
                            image_style.width = width;
                        }
                        if matches!(image_style.height, w3cos_std::style::Dimension::Auto)
                            && let Some(height) = node
                                .attributes
                                .iter()
                                .find(|(key, _)| key.as_str() == "height")
                                .and_then(|(_, value)| {
                                    parse_html_dimension_attribute(value.as_str())
                                })
                        {
                            image_style.height = height;
                        }
                        w3cos_std::Component::image(src, image_style)
                    }
                    "input" | "textarea" => {
                        let input_type = node
                            .attributes
                            .iter()
                            .find(|(key, _)| key.as_str() == "type")
                            .map(|(_, value)| value.as_str())
                            .unwrap_or("text");
                        if tag.as_str() == "input" && input_type.eq_ignore_ascii_case("file") {
                            let mut component = w3cos_std::Component::boxed(style, children);
                            component.on_click = w3cos_std::EventAction::NativeHost {
                                id: id.as_u32() as u64,
                                click: true,
                                scroll: false,
                                input: false,
                                focus: false,
                                keyboard: false,
                                submit: false,
                                pointer: true,
                                wheel: false,
                            };
                            return component;
                        }
                        let placeholder = node
                            .attributes
                            .iter()
                            .find(|(k, _)| k.as_str() == "placeholder")
                            .map(|(_, v)| v.as_str())
                            .unwrap_or("");
                        let value = node
                            .attributes
                            .iter()
                            .find(|(k, _)| k.as_str() == "value")
                            .map(|(_, v)| v.as_str())
                            .or(node.text_content.as_deref())
                            .unwrap_or("");
                        let secure =
                            tag.as_str() == "input" && input_type.eq_ignore_ascii_case("password");
                        let mut component = if secure {
                            w3cos_std::Component::secure_text_input(value, placeholder, style)
                        } else {
                            w3cos_std::Component::text_input(value, placeholder, style)
                        };
                        component.on_click = w3cos_std::EventAction::NativeHost {
                            id: id.as_u32() as u64,
                            click: true,
                            scroll: false,
                            input: true,
                            focus: true,
                            keyboard: true,
                            submit: false,
                            pointer: true,
                            wheel: false,
                        };
                        component
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
                };
                // Keep the originating DOM node on every rendered element,
                // not only form controls. Browser editors attach their mouse
                // handlers to container divs and focus a hidden textarea from
                // those handlers; without a native host id the runtime cannot
                // target or bubble pointer events through that DOM ancestry.
                if node.node_type == NodeType::Element
                    && !matches!(
                        component.on_click,
                        w3cos_std::EventAction::NativeHost { .. }
                    )
                {
                    component.on_click = w3cos_std::EventAction::NativeHost {
                        id: id.as_u32() as u64,
                        click: false,
                        scroll: false,
                        input: false,
                        focus: false,
                        keyboard: false,
                        submit: false,
                        pointer: true,
                        wheel: false,
                    };
                }
                component
            }
        }
    }

    fn svg_attribute(&self, id: NodeId, name: &str) -> Option<String> {
        let mut current = Some(id);
        while let Some(node_id) = current {
            let node = self.get_node(node_id);
            if let Some((_, value)) = node.attributes.iter().find(|(key, _)| key.as_str() == name) {
                return Some(value.clone());
            }
            current = node.parent;
        }
        None
    }

    fn svg_number(&self, id: NodeId, name: &str, default: f32) -> f32 {
        self.get_node(id)
            .attributes
            .iter()
            .find(|(key, _)| key.as_str() == name)
            .and_then(|(_, value)| value.trim().trim_end_matches("px").parse::<f32>().ok())
            .unwrap_or(default)
    }

    fn svg_transform_chain(&self, id: NodeId) -> Vec<String> {
        let mut values = Vec::new();
        let mut current = Some(id);
        while let Some(node_id) = current {
            let node = self.get_node(node_id);
            if let Some((_, value)) = node
                .attributes
                .iter()
                .find(|(key, _)| key.as_str() == "transform")
            {
                values.push(value.clone());
            }
            current = node.parent;
        }
        values.reverse();
        values
    }

    fn svg_root_size(&self, id: NodeId) -> (f32, f32) {
        let node = self.get_node(id);
        let view_box = node
            .attributes
            .iter()
            .find(|(key, _)| key.as_str().eq_ignore_ascii_case("viewbox"))
            .map(|(_, value)| {
                value
                    .split(|ch: char| ch.is_ascii_whitespace() || ch == ',')
                    .filter_map(|part| part.parse::<f32>().ok())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let fallback_width = view_box.get(2).copied().unwrap_or(300.0);
        let fallback_height = view_box.get(3).copied().unwrap_or(150.0);
        (
            self.svg_number(id, "width", fallback_width),
            self.svg_number(id, "height", fallback_height),
        )
    }

    fn svg_markup(&self, id: NodeId) -> (String, Vec<w3cos_std::SvgEventTarget>) {
        let mut source = String::new();
        let mut event_targets = Vec::new();
        let mut render_index = 0;
        self.serialize_svg_node(
            id,
            id,
            false,
            "auto",
            &mut source,
            &mut event_targets,
            &mut render_index,
        );
        let node = self.get_node(id);
        let has_namespace = node
            .attributes
            .iter()
            .any(|(name, _)| name.as_str() == "xmlns");
        if !has_namespace {
            source.insert_str("<svg".len(), " xmlns=\"http://www.w3.org/2000/svg\"");
        }
        (source, event_targets)
    }

    fn serialize_svg_node(
        &self,
        id: NodeId,
        svg_root: NodeId,
        in_defs: bool,
        inherited_pointer_events: &str,
        out: &mut String,
        event_targets: &mut Vec<w3cos_std::SvgEventTarget>,
        render_index: &mut u32,
    ) {
        let node = self.get_node(id);
        match node.node_type {
            NodeType::Text => {
                if let Some(text) = node.text_content.as_deref() {
                    push_xml_escaped(out, text, false);
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
                if let Some(text) = node.text_content.as_deref() {
                    out.push_str(text);
                }
                out.push_str("-->");
            }
            NodeType::Element => {
                let tag = node.tag.as_str();
                let in_defs = in_defs || tag == "defs";
                let author_id = node
                    .attributes
                    .iter()
                    .find(|(name, _)| name.as_str() == "id")
                    .map(|(_, value)| value.clone());
                let internal_use_id =
                    (!in_defs && tag == "use" && author_id.is_none()).then(|| {
                        let base = format!("__w3cos_internal_use_{}", id.as_u32());
                        let mut candidate = base.clone();
                        let mut suffix = 0_u32;
                        while self.get_element_by_id(&candidate).is_some() {
                            suffix = suffix.wrapping_add(1);
                            candidate = format!("{base}_{suffix}");
                        }
                        candidate
                    });
                let lookup_id = author_id.clone().or_else(|| internal_use_id.clone());
                let pointer_events = self
                    .get_style(id)
                    .inline_declarations
                    .iter()
                    .rev()
                    .find(|(name, _)| matches!(name.as_str(), "pointer-events" | "pointerEvents"))
                    .map(|(_, value)| value.clone())
                    .or_else(|| self.svg_attribute(id, "pointer-events"))
                    .unwrap_or_else(|| inherited_pointer_events.to_string());
                let node_render_index = (!in_defs
                    && matches!(
                        tag.as_str(),
                        "path"
                            | "rect"
                            | "circle"
                            | "ellipse"
                            | "line"
                            | "polyline"
                            | "polygon"
                            | "image"
                            | "text"
                    ))
                .then(|| {
                    let index = *render_index;
                    *render_index = (*render_index).wrapping_add(1);
                    index
                });
                if !in_defs && (lookup_id.is_some() || node_render_index.is_some()) {
                    let mut host_chain = Vec::new();
                    let mut current = Some(id);
                    while let Some(node_id) = current {
                        host_chain.push(node_id.as_u32() as u64);
                        if node_id == svg_root {
                            break;
                        }
                        current = self.get_node(node_id).parent;
                    }
                    event_targets.push(w3cos_std::SvgEventTarget {
                        svg_id: lookup_id.unwrap_or_default(),
                        render_index: node_render_index,
                        pointer_events: pointer_events.clone(),
                        host_chain,
                    });
                }

                out.push('<');
                out.push_str(&tag);
                for (name, value) in &node.attributes {
                    if name.as_str() == "style" {
                        continue;
                    }
                    out.push(' ');
                    out.push_str(&name.as_str());
                    out.push_str("=\"");
                    push_xml_escaped(out, value, true);
                    out.push('"');
                }
                if let Some(internal_use_id) = internal_use_id {
                    out.push_str(" id=\"");
                    push_xml_escaped(out, &internal_use_id, true);
                    out.push('"');
                }
                if !node.class_list.is_empty() {
                    out.push_str(" class=\"");
                    for (index, class) in node.class_list.iter().enumerate() {
                        if index > 0 {
                            out.push(' ');
                        }
                        push_xml_escaped(out, &class.as_str(), true);
                    }
                    out.push('"');
                }

                let attribute_style = node
                    .attributes
                    .iter()
                    .find(|(name, _)| name.as_str() == "style")
                    .map(|(_, value)| value.as_str())
                    .unwrap_or_default();
                let inline = &self.get_style(id).inline_declarations;
                if !attribute_style.is_empty() || !inline.is_empty() {
                    out.push_str(" style=\"");
                    push_xml_escaped(out, attribute_style, true);
                    if !attribute_style.is_empty() && !attribute_style.trim_end().ends_with(';') {
                        out.push(';');
                    }
                    for (name, value) in inline {
                        push_xml_escaped(out, name, true);
                        out.push(':');
                        push_xml_escaped(out, value, true);
                        out.push(';');
                    }
                    out.push('"');
                }

                out.push('>');
                if let Some(text) = node.text_content.as_deref() {
                    push_xml_escaped(out, text, false);
                }
                let mut child = node.first_child;
                while let Some(child_id) = child {
                    self.serialize_svg_node(
                        child_id,
                        svg_root,
                        in_defs,
                        &pointer_events,
                        out,
                        event_targets,
                        render_index,
                    );
                    child = self.get_node(child_id).next_sibling;
                }
                out.push_str("</");
                out.push_str(&tag);
                out.push('>');
            }
            NodeType::DocumentType => {
                out.push_str("<!DOCTYPE ");
                out.push_str(&node.tag.as_str());
                out.push('>');
            }
            NodeType::Document | NodeType::DocumentFragment => {
                let mut child = node.first_child;
                while let Some(child_id) = child {
                    self.serialize_svg_node(
                        child_id,
                        svg_root,
                        in_defs,
                        inherited_pointer_events,
                        out,
                        event_targets,
                        render_index,
                    );
                    child = self.get_node(child_id).next_sibling;
                }
            }
        }
    }

    fn svg_path_data(&self, id: NodeId, tag: &str) -> Result<w3cos_std::SvgPathData, String> {
        let node = self.get_node(id);
        let attribute = |name: &str| {
            node.attributes
                .iter()
                .find(|(key, _)| key.as_str() == name)
                .map(|(_, value)| value.as_str())
                .unwrap_or("")
        };
        match tag {
            "path" => w3cos_std::SvgPathData::parse(attribute("d")),
            "polyline" => w3cos_std::SvgPathData::from_points(attribute("points"), false),
            "polygon" => w3cos_std::SvgPathData::from_points(attribute("points"), true),
            _ => Err(format!("unsupported SVG path element `{tag}`")),
        }
    }

    fn svg_path_component(
        &self,
        id: NodeId,
        tag: &str,
        style: w3cos_std::style::Style,
    ) -> Option<w3cos_std::Component> {
        use w3cos_std::color::Color;

        let path = match self.svg_path_data(id, tag) {
            Ok(path) => path,
            Err(error) => {
                eprintln!("W3COS warning: <{tag}> geometry was ignored: {error}");
                return None;
            }
        };
        let fill = self
            .svg_attribute(id, "fill")
            .unwrap_or_else(|| "black".to_string());
        let fill = if fill == "none" {
            Color::TRANSPARENT
        } else if fill == "currentColor" {
            style.color
        } else {
            Color::from_css(&fill).unwrap_or(Color::BLACK)
        };
        let stroke = self
            .svg_attribute(id, "stroke")
            .filter(|stroke| stroke != "none")
            .and_then(|stroke| {
                if stroke == "currentColor" {
                    Some(style.color)
                } else {
                    Color::from_css(&stroke)
                }
            });
        Some(w3cos_std::Component::svg_path(
            path.commands,
            fill,
            stroke,
            self.svg_number(id, "stroke-width", 1.0).max(0.0),
            style,
        ))
    }

    fn apply_svg_presentation_style(
        &self,
        id: NodeId,
        tag: &str,
        style: &mut w3cos_std::style::Style,
    ) {
        use w3cos_std::color::Color;
        use w3cos_std::style::{Dimension, Position};

        if !matches!(
            tag,
            "svg"
                | "g"
                | "defs"
                | "rect"
                | "circle"
                | "ellipse"
                | "line"
                | "polyline"
                | "polygon"
                | "path"
                | "text"
                | "use"
        ) {
            return;
        }

        let fill = self
            .svg_attribute(id, "fill")
            .unwrap_or_else(|| "black".to_string());
        let fill = if fill == "none" {
            Color::TRANSPARENT
        } else if fill == "currentColor" {
            style.color
        } else {
            Color::from_css(&fill).unwrap_or(Color::BLACK)
        };
        let stroke = self
            .svg_attribute(id, "stroke")
            .filter(|stroke| stroke != "none")
            .and_then(|stroke| {
                if stroke == "currentColor" {
                    Some(style.color)
                } else {
                    Color::from_css(&stroke)
                }
            });
        let stroke_width = self.svg_number(id, "stroke-width", 1.0).max(0.0);
        let opacity = self.svg_number(id, "opacity", 1.0).clamp(0.0, 1.0);

        style.opacity *= opacity;
        style.flex_shrink = 0.0;
        match tag {
            "svg" => {
                let (width, height) = self.svg_root_size(id);
                style.position = Position::Relative;
                if matches!(style.width, Dimension::Auto) {
                    style.width = Dimension::Px(width);
                }
                if matches!(style.height, Dimension::Auto) {
                    style.height = Dimension::Px(height);
                }
            }
            "g" | "defs" => {
                style.position = Position::Absolute;
                style.left = Dimension::Px(0.0);
                style.top = Dimension::Px(0.0);
                style.width = Dimension::Percent(1.0);
                style.height = Dimension::Percent(1.0);
            }
            "rect" => {
                style.position = Position::Absolute;
                style.left = Dimension::Px(self.svg_number(id, "x", 0.0));
                style.top = Dimension::Px(self.svg_number(id, "y", 0.0));
                style.width = Dimension::Px(self.svg_number(id, "width", 0.0).max(0.0));
                style.height = Dimension::Px(self.svg_number(id, "height", 0.0).max(0.0));
                style.background = fill;
                style.border_radius = self
                    .svg_number(id, "rx", self.svg_number(id, "ry", 0.0))
                    .max(0.0);
            }
            "circle" => {
                let radius = self.svg_number(id, "r", 0.0).max(0.0);
                style.position = Position::Absolute;
                style.left = Dimension::Px(self.svg_number(id, "cx", 0.0) - radius);
                style.top = Dimension::Px(self.svg_number(id, "cy", 0.0) - radius);
                style.width = Dimension::Px(radius * 2.0);
                style.height = Dimension::Px(radius * 2.0);
                style.background = fill;
                style.border_radius = radius;
            }
            "ellipse" => {
                let rx = self.svg_number(id, "rx", 0.0).max(0.0);
                let ry = self.svg_number(id, "ry", 0.0).max(0.0);
                style.position = Position::Absolute;
                style.left = Dimension::Px(self.svg_number(id, "cx", 0.0) - rx);
                style.top = Dimension::Px(self.svg_number(id, "cy", 0.0) - ry);
                style.width = Dimension::Px(rx * 2.0);
                style.height = Dimension::Px(ry * 2.0);
                style.background = fill;
                style.border_radius = rx.min(ry);
            }
            "line" => {
                let x1 = self.svg_number(id, "x1", 0.0);
                let y1 = self.svg_number(id, "y1", 0.0);
                let x2 = self.svg_number(id, "x2", 0.0);
                let y2 = self.svg_number(id, "y2", 0.0);
                let length = (x2 - x1).hypot(y2 - y1);
                style.position = Position::Absolute;
                style.left = Dimension::Px(x1.min(x2));
                style.top = Dimension::Px(y1.min(y2));
                style.width = Dimension::Px(length);
                style.height = Dimension::Px(stroke_width.max(1.0));
                style.background = stroke.unwrap_or(fill);
                style.transform.rotate_deg = (y2 - y1).atan2(x2 - x1).to_degrees();
            }
            "text" => {
                style.position = Position::Absolute;
                style.left = Dimension::Px(self.svg_number(id, "x", 0.0));
                style.top =
                    Dimension::Px(self.svg_number(id, "y", style.font_size) - style.font_size);
                style.color = fill;
            }
            "polyline" | "polygon" | "path" => {
                style.position = Position::Absolute;
                match self.svg_path_data(id, tag) {
                    Ok(path) => {
                        style.left = Dimension::Px(path.bounds[0]);
                        style.top = Dimension::Px(path.bounds[1]);
                        style.width = Dimension::Px(path.bounds[2].max(stroke_width));
                        style.height = Dimension::Px(path.bounds[3].max(stroke_width));
                    }
                    Err(_) => {
                        style.left = Dimension::Px(0.0);
                        style.top = Dimension::Px(0.0);
                        style.width = Dimension::Px(0.0);
                        style.height = Dimension::Px(0.0);
                    }
                }
                style.background = Color::TRANSPARENT;
                style.border_width = 0.0;
                for transform in self.svg_transform_chain(id) {
                    apply_svg_transform(&transform, &mut style.transform);
                }
            }
            _ => {
                style.position = Position::Absolute;
                style.left = Dimension::Px(0.0);
                style.top = Dimension::Px(0.0);
                style.width = Dimension::Px(0.0);
                style.height = Dimension::Px(0.0);
            }
        }
        if let Some(stroke) = stroke
            && matches!(tag, "rect" | "circle" | "ellipse" | "line" | "text" | "use")
        {
            style.border_width = stroke_width;
            style.border_color = stroke;
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
            k if k.len() == 1 && !ctrl && !meta => (InputType::InsertText, Some(k.to_string())),
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
    pub fn handle_composition(&mut self, target: NodeId, phase: &str, data: &str) {
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
        comp_event.data = EventData::Composition {
            data: data.to_string(),
        };
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

fn css_property_eq(actual: &str, canonical: &str) -> bool {
    let normalize = |value: &str| {
        value
            .chars()
            .filter(|ch| *ch != '-')
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    normalize(actual) == normalize(canonical)
}

fn parse_css_string_list(value: &str) -> Vec<String> {
    let mut strings = Vec::new();
    let mut remaining = value;
    while !remaining.trim_start().is_empty() {
        remaining = remaining.trim_start();
        let Some(quote) = remaining.chars().next() else {
            break;
        };
        if !matches!(quote, '\'' | '"') {
            return Vec::new();
        }
        let mut escaped = false;
        let mut end = None;
        for (index, character) in remaining[quote.len_utf8()..].char_indices() {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == quote {
                end = Some(quote.len_utf8() + index);
                break;
            }
        }
        let Some(end) = end else {
            return Vec::new();
        };
        let Some(string) = stylesheet::css_unescape(&remaining[quote.len_utf8()..end]) else {
            return Vec::new();
        };
        strings.push(string);
        remaining = &remaining[end + quote.len_utf8()..];
    }
    strings
}

fn generated_content_image_prefix(value: &str) -> Option<(String, usize)> {
    let prefix = value.get(..4)?;
    if !prefix.eq_ignore_ascii_case("url(") {
        return None;
    }
    let mut quote = None;
    let mut escaped = false;
    let mut end = None;
    for (index, character) in value[4..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if let Some(active_quote) = quote {
            if character == active_quote {
                quote = None;
            }
            continue;
        }
        if matches!(character, '\'' | '"') {
            quote = Some(character);
        } else if character == ')' {
            end = Some(4 + index);
            break;
        }
    }
    let end = end?;
    let raw = value[4..end].trim();
    let unquoted = match raw.chars().next() {
        Some(quote @ ('\'' | '"')) if raw.ends_with(quote) && raw.len() >= 2 => {
            &raw[quote.len_utf8()..raw.len() - quote.len_utf8()]
        }
        Some('\'' | '"') => return None,
        _ => raw,
    };
    stylesheet::css_unescape(unquoted)
        .filter(|source| !source.is_empty())
        .map(|source| (source, end + 1))
}

fn quote_operations(value: &str) -> Vec<&str> {
    let mut operations = Vec::new();
    let mut index = 0usize;
    while index < value.len() {
        let Some(character) = value[index..].chars().next() else {
            break;
        };
        if matches!(character, '\'' | '"') {
            let quote = character;
            index += character.len_utf8();
            let mut escaped = false;
            while index < value.len() {
                let current = value[index..].chars().next().expect("content character");
                index += current.len_utf8();
                if escaped {
                    escaped = false;
                } else if current == '\\' {
                    escaped = true;
                } else if current == quote {
                    break;
                }
            }
            continue;
        }
        if character.is_ascii_alphabetic() || character == '-' {
            let start = index;
            index += character.len_utf8();
            while index < value.len() {
                let current = value[index..].chars().next().expect("content identifier");
                if current.is_ascii_alphanumeric() || current == '-' {
                    index += current.len_utf8();
                } else {
                    break;
                }
            }
            let identifier = &value[start..index];
            if matches!(
                identifier.to_ascii_lowercase().as_str(),
                "open-quote" | "close-quote" | "no-open-quote" | "no-close-quote"
            ) {
                operations.push(identifier);
            }
            continue;
        }
        index += character.len_utf8();
    }
    operations
}

fn adjust_quote_depth(value: &str, depth: &mut usize) {
    for operation in quote_operations(value) {
        match operation.to_ascii_lowercase().as_str() {
            "open-quote" | "no-open-quote" => *depth += 1,
            "close-quote" | "no-close-quote" => *depth = depth.saturating_sub(1),
            _ => {}
        }
    }
}

fn alphabetic_counter(mut value: i32, alphabet: &[char]) -> Option<String> {
    if value <= 0 || alphabet.is_empty() {
        return None;
    }
    let radix = alphabet.len() as i32;
    let mut output = Vec::new();
    while value > 0 {
        value -= 1;
        output.push(alphabet[(value % radix) as usize]);
        value /= radix;
    }
    output.reverse();
    Some(output.into_iter().collect())
}

fn roman_counter(mut value: i32, uppercase: bool) -> Option<String> {
    if !(1..=3999).contains(&value) {
        return None;
    }
    let mut output = String::new();
    for (number, digits) in [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ] {
        while value >= number {
            output.push_str(digits);
            value -= number;
        }
    }
    Some(if uppercase {
        output
    } else {
        output.to_ascii_lowercase()
    })
}

fn additive_counter(mut value: i32, symbols: &[(i32, char)], maximum: i32) -> Option<String> {
    if !(1..=maximum).contains(&value) {
        return None;
    }
    let mut output = String::new();
    for &(weight, symbol) in symbols {
        while value >= weight {
            output.push(symbol);
            value -= weight;
        }
    }
    Some(output)
}

fn valid_counter_style(style: &str) -> bool {
    matches!(
        style.trim().to_ascii_lowercase().as_str(),
        "none"
            | "decimal"
            | "decimal-leading-zero"
            | "disc"
            | "circle"
            | "square"
            | "lower-alpha"
            | "lower-latin"
            | "upper-alpha"
            | "upper-latin"
            | "lower-greek"
            | "lower-roman"
            | "upper-roman"
            | "georgian"
            | "armenian"
    )
}

fn format_counter_value(value: i32, style: &str) -> String {
    let style = style.trim().to_ascii_lowercase();
    match style.as_str() {
        "none" => String::new(),
        "disc" => "•".to_string(),
        "circle" => "◦".to_string(),
        "square" => "▪".to_string(),
        "decimal-leading-zero" if value < 0 && value > -10 => format!("-0{}", -value),
        "decimal-leading-zero" if value >= 0 && value < 10 => format!("0{value}"),
        "lower-alpha" | "lower-latin" => alphabetic_counter(
            value,
            &"abcdefghijklmnopqrstuvwxyz".chars().collect::<Vec<_>>(),
        )
        .unwrap_or_else(|| value.to_string()),
        "upper-alpha" | "upper-latin" => alphabetic_counter(
            value,
            &"ABCDEFGHIJKLMNOPQRSTUVWXYZ".chars().collect::<Vec<_>>(),
        )
        .unwrap_or_else(|| value.to_string()),
        "lower-greek" => alphabetic_counter(
            value,
            &"αβγδεζηθικλμνξοπρστυφχψω".chars().collect::<Vec<_>>(),
        )
        .unwrap_or_else(|| value.to_string()),
        "lower-roman" => roman_counter(value, false).unwrap_or_else(|| value.to_string()),
        "upper-roman" => roman_counter(value, true).unwrap_or_else(|| value.to_string()),
        "georgian" => additive_counter(
            value,
            &[
                (10_000, 'ჵ'),
                (9_000, 'ჰ'),
                (8_000, 'ჯ'),
                (7_000, 'ჴ'),
                (6_000, 'ხ'),
                (5_000, 'ჭ'),
                (4_000, 'წ'),
                (3_000, 'ძ'),
                (2_000, 'ც'),
                (1_000, 'ჩ'),
                (900, 'შ'),
                (800, 'ყ'),
                (700, 'ღ'),
                (600, 'ქ'),
                (500, 'ფ'),
                (400, 'ჳ'),
                (300, 'ტ'),
                (200, 'ს'),
                (100, 'რ'),
                (90, 'ჟ'),
                (80, 'პ'),
                (70, 'ო'),
                (60, 'ჲ'),
                (50, 'ნ'),
                (40, 'მ'),
                (30, 'ლ'),
                (20, 'კ'),
                (10, 'ი'),
                (9, 'თ'),
                (8, 'ჱ'),
                (7, 'ზ'),
                (6, 'ვ'),
                (5, 'ე'),
                (4, 'დ'),
                (3, 'გ'),
                (2, 'ბ'),
                (1, 'ა'),
            ],
            19_999,
        )
        .unwrap_or_else(|| value.to_string()),
        "armenian" => additive_counter(
            value,
            &[
                (9_000, 'Ք'),
                (8_000, 'Փ'),
                (7_000, 'Ւ'),
                (6_000, 'Ց'),
                (5_000, 'Ր'),
                (4_000, 'Տ'),
                (3_000, 'Վ'),
                (2_000, 'Ս'),
                (1_000, 'Ռ'),
                (900, 'Ջ'),
                (800, 'Պ'),
                (700, 'Չ'),
                (600, 'Ո'),
                (500, 'Շ'),
                (400, 'Ն'),
                (300, 'Յ'),
                (200, 'Մ'),
                (100, 'Ճ'),
                (90, 'Ղ'),
                (80, 'Ձ'),
                (70, 'Հ'),
                (60, 'Կ'),
                (50, 'Ծ'),
                (40, 'Խ'),
                (30, 'Լ'),
                (20, 'Ի'),
                (10, 'Ժ'),
                (9, 'Թ'),
                (8, 'Ը'),
                (7, 'Է'),
                (6, 'Զ'),
                (5, 'Ե'),
                (4, 'Դ'),
                (3, 'Գ'),
                (2, 'Բ'),
                (1, 'Ա'),
            ],
            9_999,
        )
        .unwrap_or_else(|| value.to_string()),
        _ => value.to_string(),
    }
}

fn inherit_text_style(
    style: &mut w3cos_std::style::Style,
    parent: &w3cos_std::style::Style,
    tag: &str,
    declares: impl Fn(&str) -> bool,
) {
    let form_control = matches!(tag, "button" | "input" | "select" | "textarea");
    let heading = matches!(tag, "h1" | "h2" | "h3" | "h4" | "h5" | "h6");

    if !declares("color") && !form_control {
        style.color = parent.color;
    }
    if !declares("font-size") && !declares("font") && !form_control && !heading {
        style.font_size = parent.font_size;
    }
    if !declares("font-weight")
        && !declares("font")
        && !heading
        && !matches!(tag, "b" | "strong")
    {
        style.font_weight = parent.font_weight;
    }
    if !declares("font-family") && !declares("font") {
        style.font_family = parent.font_family.clone();
    }
    if !declares("font-style") && !declares("font") && !matches!(tag, "em" | "i") {
        style.font_style = parent.font_style;
    }
    if !declares("line-height") && !declares("font") {
        style.line_height = parent.line_height;
    }
    if !declares("letter-spacing") {
        style.letter_spacing = parent.letter_spacing;
    }
    if !declares("text-align") {
        style.text_align = parent.text_align;
    }
    if !declares("white-space") {
        style.white_space = parent.white_space;
    }
    if !declares("word-break") && !declares("overflow-wrap") && !declares("word-wrap") {
        style.word_break = parent.word_break;
    }
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

fn apply_svg_transform(value: &str, transform: &mut w3cos_std::style::Transform2D) {
    let mut rest = value.trim();
    while let Some(open) = rest.find('(') {
        let name = rest[..open].trim();
        let Some(close_offset) = rest[open + 1..].find(')') else {
            eprintln!("W3COS warning: malformed SVG transform `{value}` was ignored");
            return;
        };
        let close = open + 1 + close_offset;
        let values = rest[open + 1..close]
            .split(|ch: char| ch.is_ascii_whitespace() || ch == ',')
            .filter(|part| !part.is_empty())
            .filter_map(|part| part.parse::<f32>().ok())
            .collect::<Vec<_>>();
        match (name, values.as_slice()) {
            ("translate", [x]) => transform.translate_x += x,
            ("translate", [x, y, ..]) => {
                transform.translate_x += x;
                transform.translate_y += y;
            }
            ("scale", [scale]) => {
                transform.scale_x *= scale;
                transform.scale_y *= scale;
            }
            ("scale", [x, y, ..]) => {
                transform.scale_x *= x;
                transform.scale_y *= y;
            }
            ("rotate", [degrees]) => transform.rotate_deg += degrees,
            ("rotate", [degrees, _, _, ..]) => {
                // The retained transform model rotates around the laid-out
                // shape center, which matches the common rotate(angle cx cy)
                // case after the path has been reduced to its bounds.
                transform.rotate_deg += degrees;
            }
            ("matrix", [a, b, c, d, e, f, ..]) if b.abs() < 0.0001 && c.abs() < 0.0001 => {
                transform.scale_x *= a;
                transform.scale_y *= d;
                transform.translate_x += e;
                transform.translate_y += f;
            }
            _ => eprintln!(
                "W3COS warning: SVG transform `{name}` uses unsupported skew/matrix semantics"
            ),
        }
        rest = rest[close + 1..].trim_start();
    }
}

fn push_xml_escaped(out: &mut String, value: &str, attribute: bool) {
    for character in value.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attribute => out.push_str("&quot;"),
            '\'' if attribute => out.push_str("&apos;"),
            _ => out.push(character),
        }
    }
}

fn passive_generated_inline_declaration(property: &str, value: &str) -> bool {
    let value = value.trim().to_ascii_lowercase();
    matches!(property, "display" | "quotes")
        || property.starts_with("counter-")
        || matches!(
            (property, value.as_str()),
            ("margin" | "padding", "0" | "0px")
                | ("width" | "height", "auto")
                | ("border", "none" | "0" | "0px")
                | ("color", "inherit")
                | ("background" | "background-color", "transparent")
        )
}

fn principal_box_can_collapse_generated_text(style: &w3cos_std::style::Style) -> bool {
    use w3cos_std::style::{Display, Position};

    style.display == Display::Inline
        && style.position == Position::Static
        && principal_box_can_merge_generated_inline_text(style)
}

fn principal_box_can_merge_generated_inline_text(style: &w3cos_std::style::Style) -> bool {
    use w3cos_std::style::{Dimension, Transform2D};

    style.width == Dimension::Auto
        && style.height == Dimension::Auto
        && style.min_width == Dimension::Auto
        && style.min_height == Dimension::Auto
        && style.max_width == Dimension::Auto
        && style.max_height == Dimension::Auto
        && style.padding == w3cos_std::style::Edges::ZERO
        && style.margin == w3cos_std::style::Edges::ZERO
        && style.border_width == 0.0
        && [
            style.border_top_width,
            style.border_right_width,
            style.border_bottom_width,
            style.border_left_width,
        ]
        .into_iter()
        .all(|width| width.unwrap_or(0.0) == 0.0)
        && style.background.a == 0
        && style.background_image.is_none()
        && style.box_shadow.is_none()
        && style.filter.is_none()
        && style.opacity == 1.0
        && style.transform == Transform2D::default()
}

fn generated_display_creates_box(display: w3cos_std::style::Display) -> bool {
    !matches!(
        display,
        w3cos_std::style::Display::None
            | w3cos_std::style::Display::TableColumn
            | w3cos_std::style::Display::TableColumnGroup
    )
}

fn split_css_tokens(value: &str) -> Vec<String> {
    value
        .split_ascii_whitespace()
        .map(|token| token.trim_matches(|character: char| matches!(character, ',' | '/')))
        .filter(|token| !token.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn normalize_css_table_internal_used_style(style: &mut w3cos_std::style::Style) {
    use w3cos_std::style::Display;

    let ignores_margin = matches!(
        style.display,
        Display::TableRowGroup
            | Display::TableHeaderGroup
            | Display::TableFooterGroup
            | Display::TableRow
            | Display::TableColumnGroup
            | Display::TableColumn
            | Display::TableCell
    );
    if ignores_margin {
        // CSS internal table boxes do not accept margins. Keep the authored
        // declarations in the DOM cascade, but remove them from the used
        // component style consumed by layout and paint.
        style.margin = w3cos_std::style::Edges::ZERO;
    }

    let ignores_padding_and_border = matches!(
        style.display,
        Display::TableRowGroup
            | Display::TableHeaderGroup
            | Display::TableFooterGroup
            | Display::TableRow
            | Display::TableColumnGroup
            | Display::TableColumn
    );
    if ignores_padding_and_border {
        // In the default separated-border model, row/row-group/column boxes
        // neither consume padding nor paint borders. Cells retain both.
        style.padding = w3cos_std::style::Edges::ZERO;
        style.border_width = 0.0;
        style.border_top_width = None;
        style.border_right_width = None;
        style.border_bottom_width = None;
        style.border_left_width = None;
    }
}

fn collapsible_generated_whitespace(component: &w3cos_std::Component) -> bool {
    matches!(
        &component.kind,
        w3cos_std::ComponentKind::Text { content }
            if is_only_css_whitespace(content)
    ) && component.children.is_empty()
        && component.style.display == w3cos_std::style::Display::Inline
        && component.style.position == w3cos_std::style::Position::Static
        && component.style.padding == w3cos_std::style::Edges::ZERO
        && component.style.margin == w3cos_std::style::Edges::ZERO
        && component.style.border_width == 0.0
        && component.style.background.a == 0
}

fn component_has_non_whitespace_text(component: &w3cos_std::Component) -> bool {
    matches!(
        &component.kind,
        w3cos_std::ComponentKind::Text { content } if !content.trim().is_empty()
    ) || component
        .children
        .iter()
        .any(component_has_non_whitespace_text)
}

fn anonymous_table_style(
    display: w3cos_std::style::Display,
    parent_style: &w3cos_std::style::Style,
) -> w3cos_std::style::Style {
    let mut style = w3cos_std::style::Style::default();
    inherit_text_style(&mut style, parent_style, "", |_| false);
    style.display = display;
    style.visibility = parent_style.visibility;
    style
}

fn specified_table_cell_height(style: &w3cos_std::style::Style) -> Option<f32> {
    match style.height {
        w3cos_std::style::Dimension::Px(height) => Some(height),
        w3cos_std::style::Dimension::Em(height) => Some(height * style.font_size),
        w3cos_std::style::Dimension::Rem(height) => Some(height * 16.0),
        _ => None,
    }
}

fn anonymous_table_row(
    parent_style: &w3cos_std::style::Style,
    mut cells: Vec<w3cos_std::Component>,
    containing_table_height: Option<f32>,
) -> w3cos_std::Component {
    let cell_height = cells
        .iter()
        .filter_map(|cell| specified_table_cell_height(&cell.style))
        .fold(0.0_f32, f32::max);
    let used_height = containing_table_height.unwrap_or(0.0).max(cell_height);
    if used_height > 0.0 {
        // A table-cell `height` contributes to the row's minimum height. The
        // cell box itself then spans the used row height; treating every
        // authored cell height as an independent block height incorrectly
        // stacks generated cells instead of aligning them in one row.
        for cell in &mut cells {
            cell.style.height = w3cos_std::style::Dimension::Auto;
        }
    }
    let mut row_style = anonymous_table_style(w3cos_std::style::Display::TableRow, parent_style);
    if used_height > 0.0 {
        row_style.height = w3cos_std::style::Dimension::Px(used_height);
    }
    row_style.flex_direction = w3cos_std::style::FlexDirection::Row;
    row_style.align_items = w3cos_std::style::AlignItems::Stretch;
    w3cos_std::Component::row(row_style, cells)
}

fn anonymous_table_cell(
    parent_style: &w3cos_std::style::Style,
    child: w3cos_std::Component,
) -> w3cos_std::Component {
    w3cos_std::Component::boxed(
        anonymous_table_style(w3cos_std::style::Display::TableCell, parent_style),
        vec![child],
    )
}

fn anonymous_table_cell_from_children(
    parent_style: &w3cos_std::style::Style,
    mut children: Vec<w3cos_std::Component>,
) -> w3cos_std::Component {
    if children.len() >= 2
        && children.iter().all(|child| {
            matches!(
                child.style.display,
                w3cos_std::style::Display::Inline
                    | w3cos_std::style::Display::InlineBlock
                    | w3cos_std::style::Display::InlineFlex
                    | w3cos_std::style::Display::InlineTable
            )
        })
    {
        // Anonymous table cells still establish an inline formatting context.
        // Lower a multi-item inline run to the same transparent flex row used
        // by an authored `td`, so its items share one baseline and intrinsic
        // width instead of stacking as independent block-axis children.
        let mut line_style = w3cos_std::style::Style::default();
        line_style.display = w3cos_std::style::Display::Flex;
        line_style.flex_direction = w3cos_std::style::FlexDirection::Row;
        line_style.align_items = w3cos_std::style::AlignItems::Baseline;
        children = vec![w3cos_std::Component::row(line_style, children)];
    }
    w3cos_std::Component::boxed(
        anonymous_table_style(w3cos_std::style::Display::TableCell, parent_style),
        children,
    )
}

fn table_row_from_misparented_children(
    parent_style: &w3cos_std::style::Style,
    children: Vec<w3cos_std::Component>,
    containing_table_height: Option<f32>,
) -> w3cos_std::Component {
    let cells = children
        .into_iter()
        .map(|child| {
            if child.style.display == w3cos_std::style::Display::TableCell {
                child
            } else {
                anonymous_table_cell(parent_style, child)
            }
        })
        .collect();
    anonymous_table_row(parent_style, cells, containing_table_height)
}

fn hoist_floats_into_block_formatting_context(
    formatting_context_style: &w3cos_std::style::Style,
    children: Vec<w3cos_std::Component>,
) -> Vec<w3cos_std::Component> {
    fn contributes_in_flow_content(component: &w3cos_std::Component) -> bool {
        match &component.kind {
            w3cos_std::ComponentKind::Text { content } => !content.trim().is_empty(),
            _ => true,
        }
    }

    fn merge_adjacent_text_runs(
        components: Vec<w3cos_std::Component>,
    ) -> Vec<w3cos_std::Component> {
        let mut merged: Vec<w3cos_std::Component> = Vec::new();
        for component in components {
            let can_merge = merged.last().is_some_and(|previous| {
                previous.children.is_empty()
                    && component.children.is_empty()
                    && previous.style == component.style
                    && matches!(previous.kind, w3cos_std::ComponentKind::Text { .. })
                    && matches!(component.kind, w3cos_std::ComponentKind::Text { .. })
            });
            if can_merge {
                let w3cos_std::ComponentKind::Text { content: next } = &component.kind else {
                    unreachable!("text merge predicate checked the component kind")
                };
                let w3cos_std::ComponentKind::Text { content } =
                    &mut merged.last_mut().expect("previous text run").kind
                else {
                    unreachable!("text merge predicate checked the previous kind")
                };
                if content.chars().next_back().is_some_and(is_css_whitespace)
                    && next.chars().next().is_some_and(is_css_whitespace)
                {
                    content.push_str(next.trim_start_matches(is_css_whitespace));
                } else {
                    content.push_str(next);
                }
            } else {
                merged.push(component);
            }
        }
        merged
    }

    fn collect(
        mut component: w3cos_std::Component,
        extract_self: bool,
        left: &mut Vec<w3cos_std::Component>,
        right: &mut Vec<w3cos_std::Component>,
    ) -> Option<w3cos_std::Component> {
        if component.style.display == w3cos_std::style::Display::None {
            // `display:none` suppresses the principal box; float must not
            // revive it through blockification.
            return Some(component);
        }
        if extract_self {
            match component.style.float {
                w3cos_std::style::Float::Left => {
                    component.style.display = w3cos_std::style::Display::Block;
                    left.push(component);
                    return None;
                }
                w3cos_std::style::Float::Right => {
                    // Preserve the static-position line strut contributed at
                    // the extraction boundary. Without it, a right float
                    // nested in an inline box starts one line above the same
                    // float authored directly in the containing block.
                    let mut strut_style = w3cos_std::style::Style::default();
                    strut_style.display = w3cos_std::style::Display::Inline;
                    strut_style.font_size = component.style.font_size;
                    strut_style.font_weight = component.style.font_weight;
                    strut_style.font_family = component.style.font_family.clone();
                    strut_style.font_style = component.style.font_style;
                    strut_style.line_height = component.style.line_height;
                    strut_style.letter_spacing = component.style.letter_spacing;
                    strut_style.white_space = component.style.white_space;
                    right.push(w3cos_std::Component::text(" ", strut_style));
                    component.style.display = w3cos_std::style::Display::Block;
                    right.push(component);
                    return None;
                }
                w3cos_std::style::Float::None => {}
            }
        } else if component.style.float == w3cos_std::style::Float::Right {
            // Right floats need a stable block-sized paint box before their
            // trailing placement is resolved by the outer scan. Direct left
            // floats retain their inline/replaced kind so following inline
            // content can share the same line in the portable layout model.
            component.style.display = w3cos_std::style::Display::Block;
        }

        if matches!(
            component.style.display,
            w3cos_std::style::Display::Inline
                | w3cos_std::style::Display::InlineBlock
                | w3cos_std::style::Display::InlineFlex
                | w3cos_std::style::Display::InlineTable
        ) {
            let mut has_prior_in_flow = false;
            let mut retained = Vec::new();
            for child in std::mem::take(&mut component.children) {
                let extract_child = child.style.float != w3cos_std::style::Float::Right
                    || has_prior_in_flow;
                if let Some(child) = collect(child, extract_child, left, right) {
                    has_prior_in_flow |= contributes_in_flow_content(&child);
                    retained.push(child);
                }
            }
            component.children = retained;
        }
        Some(component)
    }

    let mut left: Vec<w3cos_std::Component> = Vec::new();
    let mut in_flow: Vec<w3cos_std::Component> = Vec::new();
    let mut right: Vec<w3cos_std::Component> = Vec::new();
    let mut moved_direct_right = false;
    for child in children {
        let direct_float = child.style.float;
        // A direct child is already owned by this formatting context. Only
        // extract floats nested inside inline descendants here; direct float
        // ordering is handled after blockification.
        if let Some(mut child) = collect(child, false, &mut left, &mut right) {
            let has_prior_in_flow = in_flow.iter().any(contributes_in_flow_content);
            if direct_float == w3cos_std::style::Float::Left && has_prior_in_flow {
                let preceding_line_height = in_flow
                    .iter()
                    .rev()
                    .find(|component| {
                        !matches!(
                            component.style.position,
                            w3cos_std::style::Position::Absolute
                                | w3cos_std::style::Position::Fixed
                        ) && matches!(
                            component.style.display,
                            w3cos_std::style::Display::Inline
                                | w3cos_std::style::Display::InlineBlock
                                | w3cos_std::style::Display::InlineFlex
                                | w3cos_std::style::Display::InlineTable
                        )
                    })
                    .map_or(0.0, |component| {
                        component.style.font_size * component.style.line_height
                    })
                    .max(
                        formatting_context_style.font_size
                            * formatting_context_style.line_height,
                    );
                if let w3cos_std::style::Spacing::Px(margin_top) = child.style.margin.top {
                    child.style.margin.top =
                        w3cos_std::style::Spacing::Px(margin_top - preceding_line_height);
                }
                in_flow.push(child);
            } else if direct_float == w3cos_std::style::Float::Right && has_prior_in_flow {
                // A right float encountered after in-flow content cannot rise
                // above the earlier line box. Keep later text in flow and
                // place the float at this block's trailing float position.
                right.push(child);
                moved_direct_right = true;
            } else {
                in_flow.push(child);
            }
        }
    }
    if moved_direct_right {
        // Removing a float from the middle of an inline sequence joins the
        // text runs on both sides into the same anonymous line box.
        in_flow = merge_adjacent_text_runs(in_flow);
    }
    left.extend(in_flow);
    left.extend(right);
    left
}

fn anonymous_table_wrapper(
    parent_style: &w3cos_std::style::Style,
    children: Vec<w3cos_std::Component>,
) -> w3cos_std::Component {
    let table_height = children
        .iter()
        .filter_map(|child| specified_table_cell_height(&child.style))
        .fold(0.0_f32, f32::max);
    let row = table_row_from_misparented_children(
        parent_style,
        children,
        (table_height > 0.0).then_some(table_height),
    );
    let mut table_style = anonymous_table_style(w3cos_std::style::Display::Table, parent_style);
    if table_height > 0.0 {
        table_style.height = w3cos_std::style::Dimension::Px(table_height);
    }
    w3cos_std::Component::boxed(table_style, vec![row])
}

fn fixup_css_table_children(
    parent_style: &w3cos_std::style::Style,
    children: Vec<w3cos_std::Component>,
) -> Vec<w3cos_std::Component> {
    use w3cos_std::style::Display;

    match parent_style.display {
        Display::TableRow => {
            let mut cells = Vec::new();
            let mut anonymous_children = Vec::new();
            let mut anonymous_run_started = false;

            for child in children {
                if child.style.display == Display::TableCell {
                    if anonymous_run_started {
                        cells.push(anonymous_table_cell_from_children(
                            parent_style,
                            std::mem::take(&mut anonymous_children),
                        ));
                        anonymous_run_started = false;
                    }
                    cells.push(child);
                    continue;
                }

                // CSS table fixup wraps each consecutive run of improper row
                // children in one anonymous cell. Keeping text, replaced
                // content and text in separate cells changes both intrinsic
                // column sizing and inline baselines. Whitespace-only runs
                // still establish a cell, but collapse inside it.
                anonymous_run_started = true;
                if !collapsible_generated_whitespace(&child) {
                    anonymous_children.push(child);
                }
            }

            if anonymous_run_started {
                cells.push(anonymous_table_cell_from_children(
                    parent_style,
                    anonymous_children,
                ));
            }
            cells
        }
        Display::Table | Display::InlineTable
            if !children.is_empty()
                && children
                    .iter()
                    .all(|child| child.style.display == Display::TableCell) =>
        {
            let containing_height = specified_table_cell_height(parent_style);
            vec![anonymous_table_row(
                parent_style,
                children,
                containing_height,
            )]
        }
        _ => {
            // A run made entirely from misparented cells establishes one
            // anonymous table. Mixed inline/table-internal runs still need a
            // full whitespace-aware table fixup; leave those unchanged until
            // they can be grouped without perturbing their inline baselines.
            if !children.is_empty()
                && children
                    .iter()
                    .all(|child| child.style.display == Display::TableCell)
            {
                vec![anonymous_table_wrapper(parent_style, children)]
            } else {
                children
            }
        }
    }
}

fn collapse_css_whitespace(value: &str, keep_leading: bool, keep_trailing: bool) -> String {
    let starts_with_whitespace = value.chars().next().is_some_and(is_css_whitespace);
    let ends_with_whitespace = value.chars().next_back().is_some_and(is_css_whitespace);
    let mut output = String::with_capacity(value.len());
    let mut pending_space = false;
    for character in value.chars() {
        if is_css_whitespace(character) {
            pending_space = !output.is_empty();
            continue;
        }
        if pending_space {
            output.push(' ');
            pending_space = false;
        }
        output.push(character);
    }
    if output.is_empty() {
        if starts_with_whitespace && keep_leading && keep_trailing {
            output.push(' ');
        }
        return output;
    }
    if starts_with_whitespace && keep_leading {
        output.insert(0, ' ');
    }
    if ends_with_whitespace && keep_trailing {
        output.push(' ');
    }
    output
}

fn is_css_whitespace(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\n' | '\r' | '\u{000c}')
}

fn is_only_css_whitespace(value: &str) -> bool {
    value.chars().all(is_css_whitespace)
}

fn parse_html_dimension_attribute(value: &str) -> Option<w3cos_std::style::Dimension> {
    let value = value.trim();
    if let Some(percent) = value.strip_suffix('%') {
        return percent
            .trim()
            .parse::<f32>()
            .ok()
            .filter(|value| value.is_finite() && *value >= 0.0)
            .map(w3cos_std::style::Dimension::Percent);
    }
    value
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map(w3cos_std::style::Dimension::Px)
}

#[cfg(test)]
mod generated_counter_format_tests {
    use super::*;

    #[test]
    fn additive_georgian_and_armenian_counter_styles_match_css_symbols() {
        assert_eq!(format_counter_value(1, "georgian"), "ა");
        assert_eq!(format_counter_value(19_999, "georgian"), "ჵჰშჟთ");
        assert_eq!(format_counter_value(1, "armenian"), "Ա");
        assert_eq!(format_counter_value(9_999, "armenian"), "ՔՋՂԹ");
    }
}

#[cfg(test)]
mod image_component_tests {
    use super::*;
    use w3cos_std::component::ComponentKind;
    use w3cos_std::style::{AlignSelf, Dimension, Display, FlexWrap, Float, Position};

    #[test]
    fn image_width_and_height_attributes_become_layout_hints() {
        let mut document = Document::new();
        let image = document.create_element("img");
        image.set_attribute(&mut document, "src", "hero.png");
        image.set_attribute(&mut document, "width", "320");
        image.set_attribute(&mut document, "height", "180");
        document.body().append_child(&mut document, image);

        let tree = document.to_component_tree();
        let image = tree.children.first().expect("image component");
        assert!(matches!(
            image.kind,
            ComponentKind::Image { ref src } if src == "hero.png"
        ));
        assert_eq!(image.style.width, Dimension::Px(320.0));
        assert_eq!(image.style.height, Dimension::Px(180.0));
    }

    #[test]
    fn legacy_percentage_image_dimension_attribute_remains_responsive() {
        let mut document = Document::new();
        let image = document.create_element("img");
        image.set_attribute(&mut document, "src", "stripe.png");
        image.set_attribute(&mut document, "width", "100%");
        image.set_attribute(&mut document, "height", "50");
        document.body().append_child(&mut document, image);

        let tree = document.to_component_tree();
        let image = tree.children.first().expect("image component");
        assert_eq!(image.style.width, Dimension::Percent(100.0));
        assert_eq!(image.style.height, Dimension::Px(50.0));
    }

    #[test]
    fn responsive_image_render_source_does_not_mutate_the_src_attribute() {
        let mut document = Document::new();
        let image = document.create_element("img");
        image.set_attribute(&mut document, "src", "fallback.png");
        document.set_image_render_source(image.id, Some("hero-2x.png"));
        document.body().append_child(&mut document, image);

        let tree = document.to_component_tree();
        let component = tree.children.first().expect("image component");
        assert!(matches!(
            component.kind,
            ComponentKind::Image { ref src } if src == "hero-2x.png"
        ));
        assert_eq!(
            image.get_attribute(&document, "src"),
            Some("fallback.png"),
            "responsive selection must remain internal rendering state"
        );
    }

    #[test]
    fn negative_margin_mixed_inline_context_uses_stable_line_items() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("#mixed-inline", &[("width", "40px")]);
        crate::stylesheet::register_rule("#mixed-inline span", &[("margin-left", "-10px")]);

        let mut document = Document::new();
        let container = document.create_element("div");
        container.set_attribute(&mut document, "id", "mixed-inline");
        let image = document.create_element("img");
        image.set_attribute(&mut document, "width", "50");
        image.set_attribute(&mut document, "height", "6");
        let span = document.create_element("span");
        span.set_text_content(&mut document, "123");
        container.append_child(&mut document, image);
        container.append_child(&mut document, span);
        document.body().append_child(&mut document, container);

        let tree = document.to_component_tree();
        assert_eq!(tree.children[0].style.flex_wrap, FlexWrap::Wrap);
        assert_eq!(tree.children[0].children.len(), 2);
        assert!(tree.children[0]
            .children
            .iter()
            .all(|item| item.style.flex_shrink == 0.0
                && item.style.min_height == Dimension::Px(19.2)
                && item.children.len() == 1
                && item.children[0].style.flex_shrink == 0.0));
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn float_fixup_preserves_static_line_and_block_order() {
        let text = |content: &str| {
            let mut style = w3cos_std::style::Style::default();
            style.display = Display::Inline;
            w3cos_std::Component::text(content, style)
        };
        let floating_box = |side| {
            let mut style = w3cos_std::style::Style::default();
            style.display = Display::Inline;
            style.float = side;
            w3cos_std::Component::boxed(style, vec![])
        };

        let fixed = hoist_floats_into_block_formatting_context(
            &w3cos_std::style::Style::default(),
            vec![
                text("before "),
                floating_box(Float::Right),
                text(" after"),
            ],
        );
        assert_eq!(fixed.len(), 2);
        assert!(matches!(
            fixed[0].kind,
            ComponentKind::Text { ref content } if content == "before after"
        ));
        assert_eq!(fixed[1].style.float, Float::Right);
        assert_eq!(fixed[1].style.display, Display::Block);

        let paragraph = w3cos_std::Component::boxed(w3cos_std::style::Style::default(), vec![]);
        let fixed = hoist_floats_into_block_formatting_context(
            &w3cos_std::style::Style::default(),
            vec![paragraph, floating_box(Float::Left)],
        );
        assert_eq!(fixed[0].style.float, Float::None);
        assert_eq!(fixed[1].style.float, Float::Left);

        let mut inline_style = w3cos_std::style::Style::default();
        inline_style.display = Display::InlineFlex;
        let inline = w3cos_std::Component::boxed(
            inline_style,
            vec![text("nested"), floating_box(Float::Right)],
        );
        let fixed = hoist_floats_into_block_formatting_context(
            &w3cos_std::style::Style::default(),
            vec![inline],
        );
        assert_eq!(fixed.len(), 3);
        assert_eq!(fixed[0].style.float, Float::None);
        assert!(matches!(fixed[1].kind, ComponentKind::Text { ref content } if content == " "));
        assert_eq!(fixed[2].style.float, Float::Right);
        assert_eq!(fixed[2].style.display, Display::Block);

        let mut line_style = w3cos_std::style::Style::default();
        line_style.display = Display::Inline;
        line_style.line_height = 1.25;
        let line = w3cos_std::Component::text("\u{a0}", line_style);
        let mut absolute_style = w3cos_std::style::Style::default();
        absolute_style.position = w3cos_std::style::Position::Absolute;
        let absolute = w3cos_std::Component::boxed(absolute_style, vec![]);
        let mut float_style = w3cos_std::style::Style::default();
        float_style.float = Float::Left;
        float_style.margin.top = w3cos_std::style::Spacing::Px(20.0);
        let left_float = w3cos_std::Component::boxed(float_style, vec![]);
        let mut context_style = w3cos_std::style::Style::default();
        context_style.line_height = 1.25;
        let fixed = hoist_floats_into_block_formatting_context(
            &context_style,
            vec![line, absolute, left_float],
        );
        assert_eq!(fixed[2].style.margin.top, w3cos_std::style::Spacing::Px(0.0));
    }

    #[test]
    fn computed_float_resolves_global_keywords_and_positioned_boxes() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("#parent", &[("float", "left")]);
        crate::stylesheet::register_rule("#inherited", &[("float", "inherit")]);
        crate::stylesheet::register_rule("#initial", &[("float", "initial")]);
        crate::stylesheet::register_rule(
            "#positioned",
            &[("float", "right"), ("position", "absolute")],
        );

        let mut document = Document::new();
        let parent = document.create_element("div");
        parent.set_attribute(&mut document, "id", "parent");
        let inherited = document.create_element("div");
        inherited.set_attribute(&mut document, "id", "inherited");
        let initial = document.create_element("div");
        initial.set_attribute(&mut document, "id", "initial");
        let positioned = document.create_element("div");
        positioned.set_attribute(&mut document, "id", "positioned");
        parent.append_child(&mut document, inherited);
        parent.append_child(&mut document, initial);
        parent.append_child(&mut document, positioned);
        document.body().append_child(&mut document, parent);

        assert_eq!(document.computed_style_for(parent.id).float, Float::Left);
        assert_eq!(document.computed_style_for(inherited.id).float, Float::Left);
        assert_eq!(document.computed_style_for(initial.id).float, Float::None);
        let positioned = document.computed_style_for(positioned.id);
        assert_eq!(positioned.position, Position::Absolute);
        assert_eq!(positioned.float, Float::None);
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn inline_block_baseline_ignores_phantom_inline_before_a_block() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("#host", &[("display", "inline-block")]);
        crate::stylesheet::register_rule("#block", &[("display", "block")]);

        let mut document = Document::new();
        let host = document.create_element("div");
        host.set_attribute(&mut document, "id", "host");
        let phantom = document.create_element("span");
        let block = document.create_element("div");
        block.set_attribute(&mut document, "id", "block");
        host.append_child(&mut document, phantom);
        host.append_child(&mut document, block);
        document.body().append_child(&mut document, host);

        let tree = document.to_component_tree();
        let host = tree.children.first().expect("inline-block host");
        assert_eq!(host.children.len(), 1);
        assert_eq!(host.children[0].style.display, Display::Block);
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn block_only_inline_block_uses_its_bottom_edge_baseline() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("#host", &[("display", "inline-block")]);
        crate::stylesheet::register_rule("#block", &[("display", "block")]);

        let mut document = Document::new();
        let host = document.create_element("div");
        host.set_attribute(&mut document, "id", "host");
        let block = document.create_element("div");
        block.set_attribute(&mut document, "id", "block");
        block.set_text_content(&mut document, "baseline must stay inside the block");
        host.append_child(&mut document, block);
        document.body().append_child(&mut document, host);

        let tree = document.to_component_tree();
        let host = tree.children.first().expect("inline-block host");
        assert_eq!(host.style.align_self, AlignSelf::FlexEnd);
        crate::stylesheet::clear_rules();
    }
}

#[cfg(test)]
mod details_component_tests {
    use super::*;

    fn descendant_text(component: &w3cos_std::Component) -> String {
        let own = match &component.kind {
            w3cos_std::component::ComponentKind::Text { content } => content.as_str(),
            w3cos_std::component::ComponentKind::Button { label } => label.as_str(),
            _ => "",
        };
        component
            .children
            .iter()
            .fold(own.to_string(), |mut text, child| {
                text.push_str(&descendant_text(child));
                text
            })
    }

    fn details_document(open: bool) -> Document {
        let mut document = Document::new();
        let details = document.create_element("details");
        if open {
            details.set_attribute(&mut document, "open", "");
        }
        let summary = document.create_element("summary");
        summary.set_text_content(&mut document, "Completed actions");
        let content = document.create_element("div");
        content.set_text_content(&mut document, "Hidden history event");
        details.append_child(&mut document, summary);
        details.append_child(&mut document, content);
        document.body().append_child(&mut document, details);
        document
    }

    #[test]
    fn closed_details_only_lowers_its_summary() {
        let tree = details_document(false).to_component_tree();
        let text = descendant_text(&tree);
        assert!(text.contains("Completed actions"));
        assert!(!text.contains("Hidden history event"));
    }

    #[test]
    fn open_details_lowers_summary_and_content() {
        let tree = details_document(true).to_component_tree();
        let text = descendant_text(&tree);
        assert!(text.contains("Completed actions"));
        assert!(text.contains("Hidden history event"));
    }
}

#[cfg(test)]
mod tree_mutation_tests {
    use super::*;

    #[test]
    fn inserting_a_child_before_itself_is_a_noop() {
        let mut document = Document::new();
        let parent = document.create_element("div").id;
        let first = document.create_element("first").id;
        let second = document.create_element("second").id;
        document.append_child(parent, first);
        document.append_child(parent, second);

        document.insert_before(parent, first, first);

        assert_eq!(document.get_node(parent).first_child, Some(first));
        assert_eq!(document.get_node(parent).last_child, Some(second));
        assert_eq!(document.get_node(first).prev_sibling, None);
        assert_eq!(document.get_node(first).next_sibling, Some(second));
        assert_eq!(document.get_node(second).prev_sibling, Some(first));
    }

    #[test]
    fn appending_a_node_to_itself_or_its_descendant_is_a_noop() {
        let mut document = Document::new();
        let parent = document.create_element("parent").id;
        let child = document.create_element("child").id;
        document.append_child(parent, child);

        document.append_child(parent, parent);
        document.append_child(child, parent);

        assert_eq!(document.get_node(parent).parent, None);
        assert_eq!(document.get_node(parent).first_child, Some(child));
        assert_eq!(document.get_node(parent).last_child, Some(child));
        assert_eq!(document.get_node(child).parent, Some(parent));
        assert_eq!(document.get_node(child).next_sibling, None);
    }
}
