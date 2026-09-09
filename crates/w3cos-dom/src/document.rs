#[cfg(test)]
use std::cell::Cell;
use std::cell::RefCell;
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

struct CachedComputedStyle {
    node_revision: u64,
    stylesheet_generation: u64,
    inherited_style: Option<w3cos_std::style::Style>,
    style: w3cos_std::style::Style,
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
    style_revision_clock: u64,
    style_revisions: Vec<u64>,
    computed_style_cache: RefCell<HashMap<NodeId, CachedComputedStyle>>,
    #[cfg(test)]
    computed_style_cache_hits: Cell<usize>,
    #[cfg(test)]
    computed_style_cache_misses: Cell<usize>,
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
            style_revision_clock: 0,
            style_revisions: Vec::new(),
            computed_style_cache: RefCell::new(HashMap::new()),
            #[cfg(test)]
            computed_style_cache_hits: Cell::new(0),
            #[cfg(test)]
            computed_style_cache_misses: Cell::new(0),
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
        if self.html_document == html_document {
            return;
        }
        self.html_document = html_document;
        self.mark_dirty(NodeId::ROOT);
    }

    pub fn set_html_element(&mut self, id: NodeId, is_html_element: bool) {
        if self.get_node(id).is_html_element == is_html_element {
            return;
        }
        self.get_node_mut(id).is_html_element = is_html_element;
        self.mark_selector_dirty(id);
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
        let old_parent = self.get_node(child).parent;
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

        if let Some(old_parent) = old_parent
            && old_parent != parent
        {
            self.mark_dirty(old_parent);
        }
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
        let old_parent = self.get_node(new_child).parent;
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

        if let Some(old_parent) = old_parent
            && old_parent != parent
        {
            self.mark_dirty(old_parent);
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
        self.style_revision_clock = self.style_revision_clock.wrapping_add(1);
        let style_revision = self.style_revision_clock;
        let id = if let Some(slot) = self.free_list.pop() {
            node.id = NodeId(slot);
            let idx = slot as usize;
            self.nodes[idx] = Some(node);
            self.styles[idx] = initial_style;
            self.layout_rects[idx] = DOMRect::zero();
            self.scroll_offsets[idx] = (0.0, 0.0);
            self.style_revisions[idx] = style_revision;
            NodeId(slot)
        } else {
            let id = NodeId(self.nodes.len() as u32);
            node.id = id;
            let tag = node.tag;
            self.nodes.push(Some(node));
            self.styles.push(initial_style);
            self.layout_rects.push(DOMRect::zero());
            self.scroll_offsets.push((0.0, 0.0));
            self.style_revisions.push(style_revision);
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
        self.style_revision_clock = self.style_revision_clock.wrapping_add(1);
        self.style_revisions[id.0 as usize] = self.style_revision_clock;
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
        if let Some(parent) = self.get_node(id).parent {
            self.mark_dirty(parent);
        }
        self.remove_node_inner(id);
    }

    fn remove_node_inner(&mut self, id: NodeId) {
        let children = self.children_ids(id);
        for child in children {
            self.remove_node_inner(child);
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
        self.record_layout_dirty(id);
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
        let invalidation_root = if stylesheet::has_relational_dependencies() {
            NodeId::ROOT
        } else {
            id
        };
        self.invalidate_style_subtree(invalidation_root);
        self.record_layout_dirty(id);
    }

    /// Invalidate selectors whose subject, ancestor chain, or preceding
    /// siblings can observe a class/id/attribute mutation.
    pub(crate) fn mark_selector_dirty(&mut self, id: NodeId) {
        let invalidation_root = if stylesheet::has_relational_dependencies() {
            NodeId::ROOT
        } else if stylesheet::has_sibling_dependencies() {
            self.get_node(id).parent.unwrap_or(id)
        } else {
            id
        };
        self.invalidate_style_subtree(invalidation_root);
        self.record_layout_dirty(id);
    }

    /// Inline declarations cannot change selector matching, but inherited
    /// values and custom properties require the complete descendant subtree.
    pub(crate) fn mark_inline_style_dirty(&mut self, id: NodeId) {
        self.invalidate_style_subtree(id);
        self.record_layout_dirty(id);
    }

    /// Text mutations can change the parent's `:empty` state. Relational
    /// selectors such as `:has()` conservatively fall back to document scope.
    pub(crate) fn mark_text_dirty(&mut self, id: NodeId) {
        let invalidation_root = if stylesheet::has_relational_dependencies() {
            NodeId::ROOT
        } else {
            self.get_node(id).parent.unwrap_or(id)
        };
        self.invalidate_style_subtree(invalidation_root);
        self.record_layout_dirty(id);
    }

    fn record_layout_dirty(&mut self, id: NodeId) {
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

    fn invalidate_style_subtree(&mut self, root: NodeId) {
        self.style_revision_clock = self.style_revision_clock.wrapping_add(1);
        let revision = self.style_revision_clock;
        let mut pending = vec![root];
        while let Some(id) = pending.pop() {
            if self
                .nodes
                .get(id.0 as usize)
                .is_none_or(|node| node.is_none())
            {
                continue;
            }
            self.style_revisions[id.0 as usize] = revision;
            pending.extend(self.children_ids(id));
        }
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
        let mut component = self.node_to_component(id, &mut ancestors, inherited.as_ref());
        reorder_explicit_bidi_inline_rows(&mut component);
        component
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
        let node_revision = self.style_revisions[id.0 as usize];
        let stylesheet_generation = stylesheet::generation();
        if let Some(cached) = self.computed_style_cache.borrow().get(&id)
            && cached.node_revision == node_revision
            && cached.stylesheet_generation == stylesheet_generation
            && cached.inherited_style.as_ref() == inherited
        {
            #[cfg(test)]
            self.computed_style_cache_hits
                .set(self.computed_style_cache_hits.get() + 1);
            return cached.style.clone();
        }
        #[cfg(test)]
        self.computed_style_cache_misses
            .set(self.computed_style_cache_misses.get() + 1);

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
        let direction_hint = (node.node_type == NodeType::Element)
            .then(|| {
                node.attributes
                    .iter()
                    .find(|(name, _)| name.as_str().eq_ignore_ascii_case("dir"))
                    .map(|(_, value)| value.trim().to_ascii_lowercase())
            })
            .flatten()
            .filter(|value| matches!(value.as_str(), "ltr" | "rtl"));
        if let Some(value) = &direction_hint {
            merged.set_property("direction", value);
        }
        let mut custom_properties = inherited
            .and_then(|style| style.custom_properties.clone())
            .unwrap_or_default();
        custom_properties.retain(|property, _| !property.starts_with("--w3cos-internal-"));

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
        if let Some(internal) = &merged.inner.custom_properties {
            custom_properties.extend(
                internal
                    .iter()
                    .filter(|(property, _)| property.starts_with("--w3cos-internal-"))
                    .map(|(property, value)| (property.clone(), value.clone())),
            );
        }
        merged.inner.custom_properties =
            (!custom_properties.is_empty()).then_some(custom_properties);
        let mut style = merged.to_style();

        if let Some(parent) = inherited {
            let declares = |property: &str| {
                let winning_author_value = matched
                    .iter()
                    .filter(|(name, value, _)| {
                        css_property_eq(name, property)
                            && declaration_value_is_valid(property, value)
                    })
                    .map(|(_, value, _)| value.as_str())
                    .chain(
                        inline
                            .inline_declarations
                            .iter()
                            .filter(|(name, value)| {
                                css_property_eq(name, property)
                                    && declaration_value_is_valid(property, value)
                            })
                            .map(|(_, value)| value.as_str()),
                    )
                    .last();
                (css_property_eq(property, "color") && body_text_hint.is_some())
                    || (css_property_eq(property, "direction") && direction_hint.is_some())
                    || winning_author_value.is_some_and(|value| {
                        !matches!(
                            value.trim().to_ascii_lowercase().as_str(),
                            "inherit" | "unset"
                        )
                    })
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
        let declared_property_value = |properties: &[&str]| {
            matched
                .iter()
                .filter(|(name, _, _)| {
                    properties
                        .iter()
                        .any(|property| css_property_eq(name, property))
                })
                .map(|(name, value, _)| (name.as_str(), value.as_str()))
                .chain(
                    inline
                        .inline_declarations
                        .iter()
                        .filter(|(name, _)| {
                            properties
                                .iter()
                                .any(|property| css_property_eq(name, property))
                        })
                        .map(|(name, value)| (name.as_str(), value.as_str())),
                )
                .last()
        };
        if let Some((property, value)) = declared_property_value(&["font-size", "fontSize", "font"])
        {
            let parent = inherited.cloned().unwrap_or_default();
            let value = if css_property_eq(property, "font") {
                font_shorthand_size_token(value).unwrap_or(value)
            } else {
                value
            };
            let relative_size = relative_font_size_px(value, &parent);
            if let Some(relative_size) = relative_size {
                style.font_size = relative_size;
            }
        }
        if let Some((_, value)) = declared_property_value(&["vertical-align", "verticalAlign"])
            && let Some(offset) = vertical_align_length_px(value, &style)
        {
            let line_extension = (offset.abs() - style.font_size * 0.2).max(0.0);
            if offset >= 0.0 {
                style.margin.bottom = w3cos_std::style::Spacing::Px(line_extension);
            } else {
                style.margin.top = w3cos_std::style::Spacing::Px(line_extension);
            }
            style
                .custom_properties
                .get_or_insert_with(Default::default)
                .insert(
                    "--w3cos-internal-vertical-align-length".to_string(),
                    format!("{offset} {line_extension}"),
                );
        }
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
        if let Some(value) = declared_value(&["clear"]) {
            match value.trim().to_ascii_lowercase().as_str() {
                "inherit" => {
                    style.clear = inherited
                        .map(|parent| parent.clear)
                        .unwrap_or(w3cos_std::style::Clear::None);
                }
                "initial" | "unset" | "revert" | "revert-layer" => {
                    style.clear = w3cos_std::style::Clear::None;
                }
                _ => {}
            }
        }
        if let Some(value) = declared_value(&["display"]) {
            match value.trim().to_ascii_lowercase().as_str() {
                "inherit" => {
                    style.display = inherited
                        .map(|parent| parent.display)
                        .unwrap_or(w3cos_std::style::Display::Inline);
                }
                "initial" | "unset" | "revert" | "revert-layer" => {
                    style.display = w3cos_std::style::Display::Inline;
                }
                _ => {}
            }
        }
        if let Some(value) = declared_value(&["z-index", "zIndex"]) {
            match value.trim().to_ascii_lowercase().as_str() {
                "inherit" => {
                    style.z_index = inherited.map(|parent| parent.z_index).unwrap_or_default();
                    let parent_specifies_integer = inherited.is_some_and(|parent| {
                        parent.custom_properties.as_ref().is_some_and(|properties| {
                            properties.contains_key("--w3cos-internal-z-index-specified")
                        })
                    });
                    let properties = style.custom_properties.get_or_insert_with(Default::default);
                    if parent_specifies_integer {
                        properties.insert(
                            "--w3cos-internal-z-index-specified".to_string(),
                            "true".to_string(),
                        );
                    } else {
                        properties.remove("--w3cos-internal-z-index-specified");
                    }
                }
                "initial" | "unset" | "revert" | "revert-layer" | "auto" => {
                    style.z_index = 0;
                    if let Some(properties) = style.custom_properties.as_mut() {
                        properties.remove("--w3cos-internal-z-index-specified");
                    }
                }
                _ => {}
            }
        }
        let inherited_offset = |dimension, parent: &w3cos_std::style::Style| match dimension {
            w3cos_std::style::Dimension::Em(value) => {
                w3cos_std::style::Dimension::Px(value * parent.font_size)
            }
            w3cos_std::style::Dimension::Rem(value) => {
                w3cos_std::style::Dimension::Px(value * 16.0)
            }
            other => other,
        };
        for (property, target, parent_value) in [
            (
                "top",
                &mut style.top,
                inherited.map(|parent| inherited_offset(parent.top, parent)),
            ),
            (
                "right",
                &mut style.right,
                inherited.map(|parent| inherited_offset(parent.right, parent)),
            ),
            (
                "bottom",
                &mut style.bottom,
                inherited.map(|parent| inherited_offset(parent.bottom, parent)),
            ),
            (
                "left",
                &mut style.left,
                inherited.map(|parent| inherited_offset(parent.left, parent)),
            ),
        ] {
            if let Some(value) = declared_value(&[property]) {
                match value.trim().to_ascii_lowercase().as_str() {
                    "inherit" => {
                        *target = parent_value.unwrap_or(w3cos_std::style::Dimension::Auto)
                    }
                    "initial" | "unset" | "revert" | "revert-layer" => {
                        *target = w3cos_std::style::Dimension::Auto;
                    }
                    _ => {}
                }
            }
        }
        if let Some(value) = declared_value(&["clip"]) {
            match value.trim().to_ascii_lowercase().as_str() {
                "inherit" => {
                    style.clip = inherited.and_then(|parent| parent.clip);
                }
                "initial" | "unset" | "revert" | "revert-layer" => {
                    style.clip = None;
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
        if let Some((property, value)) =
            declared_property_value(&["background", "background-color", "backgroundColor"])
            && value.trim().eq_ignore_ascii_case("inherit")
            && let Some(parent) = inherited
        {
            style.background = parent.background;
            if css_property_eq(property, "background") {
                style.background_image = parent.background_image.clone();
                style.background_size = parent.background_size.clone();
                style.background_position = parent.background_position.clone();
                style.background_repeat = parent.background_repeat.clone();
                style.background_origin = parent.background_origin.clone();
                style.background_clip = parent.background_clip.clone();
                style.background_attachment = parent.background_attachment.clone();
                style.background_blend_mode = parent.background_blend_mode.clone();
            }
        }
        if let Some(parent) = inherited {
            if let Some((_, value)) = declared_property_value(&["text-indent", "textIndent"]) {
                match value.trim().to_ascii_lowercase().as_str() {
                    "inherit" | "unset" => style.text_indent = parent.text_indent,
                    "initial" | "revert" | "revert-layer" => {
                        style.text_indent = w3cos_std::style::Dimension::Px(0.0)
                    }
                    _ => {}
                }
            }
            if let Some((_, value)) = declared_property_value(&["text-transform", "textTransform"])
            {
                match value.trim().to_ascii_lowercase().as_str() {
                    "inherit" | "unset" => style.text_transform = parent.text_transform,
                    "initial" | "revert" | "revert-layer" => {
                        style.text_transform = w3cos_std::style::TextTransform::None
                    }
                    _ => {}
                }
            }
            if declared_property_value(&["direction"])
                .is_some_and(|(_, value)| value.trim().eq_ignore_ascii_case("inherit"))
            {
                style.direction = parent.direction;
            }
            if declared_property_value(&["unicode-bidi", "unicodeBidi"])
                .is_some_and(|(_, value)| value.trim().eq_ignore_ascii_case("inherit"))
            {
                style.unicode_bidi = parent.unicode_bidi;
            }
            let inherits_background_longhand = |property: &str| {
                declared_property_value(&["background", property]).is_some_and(
                    |(declared_property, value)| {
                        css_property_eq(declared_property, property)
                            && value.trim().eq_ignore_ascii_case("inherit")
                    },
                )
            };
            if inherits_background_longhand("background-image") {
                style.background_image = parent.background_image.clone();
            }
            if inherits_background_longhand("background-size") {
                style.background_size = parent.background_size.clone();
            }
            if inherits_background_longhand("background-position") {
                style.background_position = parent.background_position.clone();
            }
            if inherits_background_longhand("background-repeat") {
                style.background_repeat = parent.background_repeat.clone();
            }
            if inherits_background_longhand("background-origin") {
                style.background_origin = parent.background_origin.clone();
            }
            if inherits_background_longhand("background-clip") {
                style.background_clip = parent.background_clip.clone();
            }
            if inherits_background_longhand("background-attachment") {
                style.background_attachment = parent.background_attachment.clone();
            }
            if inherits_background_longhand("background-blend-mode") {
                style.background_blend_mode = parent.background_blend_mode.clone();
            }
            if declared_property_value(&["border"])
                .is_some_and(|(_, value)| value.trim().eq_ignore_ascii_case("inherit"))
            {
                style.border_width = parent.border_width;
                style.border_color = parent.border_color;
                style.border_top_width = parent.border_top_width;
                style.border_right_width = parent.border_right_width;
                style.border_bottom_width = parent.border_bottom_width;
                style.border_left_width = parent.border_left_width;
                style.border_top_color = parent.border_top_color;
                style.border_right_color = parent.border_right_color;
                style.border_bottom_color = parent.border_bottom_color;
                style.border_left_color = parent.border_left_color;
            }
        }
        let last_border_declaration = |properties: &[&str]| {
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
        if let Some(width) = last_border_declaration(&["border", "border-width"])
            .and_then(|value| relative_border_width_px(value, &style))
        {
            style.border_width = width;
        }
        let relative_side_width = |properties: &[&str]| {
            last_border_declaration(properties)
                .and_then(|value| relative_border_width_px(value, &style))
        };
        let top_width =
            relative_side_width(&["border", "border-width", "border-top", "border-top-width"]);
        let right_width = relative_side_width(&[
            "border",
            "border-width",
            "border-right",
            "border-right-width",
        ]);
        let bottom_width = relative_side_width(&[
            "border",
            "border-width",
            "border-bottom",
            "border-bottom-width",
        ]);
        let left_width =
            relative_side_width(&["border", "border-width", "border-left", "border-left-width"]);
        if let Some(width) = top_width {
            style.border_top_width = Some(width);
        }
        if let Some(width) = right_width {
            style.border_right_width = Some(width);
        }
        if let Some(width) = bottom_width {
            style.border_bottom_width = Some(width);
        }
        if let Some(width) = left_width {
            style.border_left_width = Some(width);
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
        self.computed_style_cache.borrow_mut().insert(
            id,
            CachedComputedStyle {
                node_revision,
                stylesheet_generation,
                inherited_style: inherited.cloned(),
                style: style.clone(),
            },
        );
        style
    }

    #[cfg(test)]
    fn computed_style_cache_stats(&self) -> (usize, usize) {
        (
            self.computed_style_cache_hits.get(),
            self.computed_style_cache_misses.get(),
        )
    }

    #[cfg(test)]
    fn computed_style_cache_is_current(&self, id: NodeId) -> bool {
        let node_revision = self.style_revisions[id.0 as usize];
        let stylesheet_generation = stylesheet::generation();
        self.computed_style_cache
            .borrow()
            .get(&id)
            .is_some_and(|entry| {
                entry.node_revision == node_revision
                    && entry.stylesheet_generation == stylesheet_generation
            })
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
        let mut declarations =
            stylesheet::matching_pseudo_declarations_for_node(self, id, pseudo_element);
        if !declarations
            .iter()
            .any(|(property, _, _)| css_property_eq(property, "content"))
            && let Some(content) = self.default_pseudo_content_value(id, pseudo_element)
        {
            declarations.push(("content".to_string(), content.to_string(), 0));
        }
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
            .or_else(|| {
                self.default_pseudo_content_value(id, pseudo_element)
                    .map(str::to_string)
            })
    }

    fn default_pseudo_content_value(
        &self,
        id: NodeId,
        pseudo_element: &str,
    ) -> Option<&'static str> {
        if !self.get_node(id).tag.as_str().eq_ignore_ascii_case("q") {
            return None;
        }
        match pseudo_element {
            "::before" => Some("open-quote"),
            "::after" => Some("close-quote"),
            _ => None,
        }
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
                        if child_style.display == Display::None {
                            return None;
                        }
                        let mut participates = matches!(
                            child_style.display,
                            Display::Inline
                                | Display::InlineBlock
                                | Display::InlineFlex
                                | Display::InlineTable
                        ) || (parent_style.display == Display::Inline
                            && matches!(
                                child_style.display,
                                Display::TableCell
                                    | Display::TableCaption
                                    | Display::TableRow
                                    | Display::TableRowGroup
                                    | Display::TableHeaderGroup
                                    | Display::TableFooterGroup
                                    | Display::TableColumn
                                    | Display::TableColumnGroup
                            ));
                        if child_style.display == Display::Inline {
                            let mut grandchild_ancestors = child_ancestors.clone();
                            grandchild_ancestors.push(self.selector_context(*child_id));
                            let contains_in_flow_block =
                                self.children_ids(*child_id).iter().any(|grandchild_id| {
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
                                });
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
                NodeType::Element => {
                    let display = self
                        .computed_style(sibling_id, ancestors, Some(parent_style))
                        .display;
                    if display == Display::None {
                        return None;
                    }
                    Some(
                        matches!(
                            display,
                            Display::Inline
                                | Display::InlineBlock
                                | Display::InlineFlex
                                | Display::InlineTable
                        ) || (parent_style.display == Display::Inline
                            && matches!(
                                display,
                                Display::TableCell
                                    | Display::TableCaption
                                    | Display::TableRow
                                    | Display::TableRowGroup
                                    | Display::TableHeaderGroup
                                    | Display::TableFooterGroup
                                    | Display::TableColumn
                                    | Display::TableColumnGroup
                            )),
                    )
                }
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
            if component.style.display == w3cos_std::style::Display::Contents {
                components.extend(std::mem::take(&mut component.children));
                continue;
            }
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
                && style.text_indent == parent_style.text_indent
                && style.letter_spacing == parent_style.letter_spacing
                && style.word_spacing == parent_style.word_spacing
                && style.text_decoration == parent_style.text_decoration
                && style.white_space == parent_style.white_space
        };
        fn append_rendered_text(
            output: &mut String,
            content: &str,
            style: &w3cos_std::style::Style,
        ) {
            let language = style.custom_properties.as_ref().and_then(|properties| {
                properties
                    .get("--w3cos-internal-text-language")
                    .map(String::as_str)
            });
            let continues_word = output
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric)
                || style.custom_properties.as_ref().is_some_and(|properties| {
                    properties
                        .get("--w3cos-internal-text-transform-continues-word")
                        .is_some_and(|value| value == "1")
                });
            output.push_str(&w3cos_std::style::transformed_text(
                content,
                style.text_transform,
                language,
                continues_word,
            ));
        }
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
                && component.style.text_indent == parent_style.text_indent
                && component.style.letter_spacing == parent_style.letter_spacing
                && component.style.word_spacing == parent_style.word_spacing
                && component.style.text_decoration == parent_style.text_decoration
                && component.style.white_space == parent_style.white_space;
            if let w3cos_std::ComponentKind::Text { content } = &component.kind {
                if !same_text_style || !component.children.is_empty() {
                    return false;
                }
                first_style.get_or_insert_with(|| component.style.clone());
                append_rendered_text(output, content, &component.style);
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
                    append_rendered_text(&mut content, text, &component.style);
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
        style.text_transform = w3cos_std::style::TextTransform::None;
        if let Some(properties) = style.custom_properties.as_mut() {
            properties.remove("--w3cos-internal-text-transform-continues-word");
        }
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
                || component.style.text_indent != parent_style.text_indent
                || component.style.text_transform != parent_style.text_transform
                || component.style.letter_spacing != parent_style.letter_spacing
                || component.style.word_spacing != parent_style.word_spacing
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
        if style.text_transform != w3cos_std::style::TextTransform::None {
            let mut language_node = Some(id);
            while let Some(language_id) = language_node {
                let candidate = self.get_node(language_id);
                if let Some(language) = candidate
                    .attributes
                    .iter()
                    .find(|(name, _)| {
                        name.as_str().eq_ignore_ascii_case("lang")
                            || name.as_str().eq_ignore_ascii_case("xml:lang")
                    })
                    .map(|(_, value)| value.trim())
                    .filter(|value| !value.is_empty())
                {
                    style
                        .custom_properties
                        .get_or_insert_with(Default::default)
                        .insert(
                            "--w3cos-internal-text-language".to_string(),
                            language.to_ascii_lowercase(),
                        );
                    break;
                }
                language_node = candidate.parent;
            }
            if style.text_transform == w3cos_std::style::TextTransform::Capitalize {
                let mut previous_sibling = node.prev_sibling;
                let previous_character = loop {
                    let Some(previous_id) = previous_sibling else {
                        break None;
                    };
                    let previous = self.get_node(previous_id);
                    match previous.node_type {
                        NodeType::Comment
                        | NodeType::DocumentType
                        | NodeType::ProcessingInstruction => {
                            previous_sibling = previous.prev_sibling;
                        }
                        NodeType::Text => {
                            break previous
                                .text_content
                                .as_deref()
                                .and_then(|text| text.chars().next_back());
                        }
                        NodeType::Element => {
                            break self
                                .descendant_text_content(previous_id)
                                .chars()
                                .next_back();
                        }
                        _ => break None,
                    }
                };
                if previous_character.is_some_and(char::is_alphanumeric) {
                    style
                        .custom_properties
                        .get_or_insert_with(Default::default)
                        .insert(
                            "--w3cos-internal-text-transform-continues-word".to_string(),
                            "1".to_string(),
                        );
                }
            }
        }
        self.apply_svg_presentation_style(id, &tag, &mut style);
        if let Some(source) = style
            .custom_properties
            .as_ref()
            .and_then(|properties| properties.get("--w3cos-internal-border-spacing-source"))
        {
            let resolve = |token: &str| {
                let token = token.trim().to_ascii_lowercase();
                let value = if let Some(value) = token.strip_suffix("px") {
                    value.parse::<f32>().ok()
                } else if let Some(value) = token.strip_suffix("rem") {
                    value.parse::<f32>().ok().map(|value| value * 16.0)
                } else if let Some(value) = token.strip_suffix("em") {
                    value
                        .parse::<f32>()
                        .ok()
                        .map(|value| value * style.font_size)
                } else if token == "0" {
                    Some(0.0)
                } else {
                    None
                };
                value.filter(|value| value.is_finite() && *value >= 0.0)
            };
            let values = source
                .split_ascii_whitespace()
                .filter_map(resolve)
                .collect::<Vec<_>>();
            if let Some(x) = values.first().copied() {
                style.border_spacing_x = x;
                style.border_spacing_y = values.get(1).copied().unwrap_or(x);
            }
        }
        if tag.as_str() == "table" {
            style
                .custom_properties
                .get_or_insert_with(Default::default)
                .insert(
                    "--w3cos-internal-html-table-element".to_string(),
                    "1".to_string(),
                );
            let author_declares_border_spacing = self.styles[id.0 as usize]
                .inline_declarations
                .iter()
                .any(|(property, _)| property == "border-spacing")
                || stylesheet::matching_declarations_for_node(self, id)
                    .iter()
                    .any(|(property, _, _)| property == "border-spacing");
            if !author_declares_border_spacing
                && let Some(cell_spacing) = node
                    .attributes
                    .iter()
                    .find(|(name, _)| name.as_str().eq_ignore_ascii_case("cellspacing"))
                    .and_then(|(_, value)| value.trim().parse::<f32>().ok())
                    .filter(|value| value.is_finite() && *value >= 0.0)
            {
                // The legacy HTML cellspacing attribute is a presentational
                // hint below author CSS, just like cellpadding.
                style.border_spacing_x = cell_spacing;
                style.border_spacing_y = cell_spacing;
            }
        }
        if matches!(tag.as_str(), "td" | "th") {
            if let Some(column_span) = node
                .attributes
                .iter()
                .find(|(name, _)| name.as_str().eq_ignore_ascii_case("colspan"))
                .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                .filter(|span| *span > 1)
            {
                style
                    .custom_properties
                    .get_or_insert_with(Default::default)
                    .insert(
                        "--w3cos-internal-table-column-span".to_string(),
                        column_span.min(1000).to_string(),
                    );
            }
            let author_declares_padding = self.styles[id.0 as usize]
                .inline_declarations
                .iter()
                .any(|(property, _)| property.starts_with("padding"))
                || stylesheet::matching_declarations_for_node(self, id)
                    .iter()
                    .any(|(property, _, _)| property.starts_with("padding"));
            if !author_declares_padding {
                let mut presentational_padding = false;
                let mut ancestor = node.parent;
                while let Some(ancestor_id) = ancestor {
                    let ancestor_node = self.get_node(ancestor_id);
                    if ancestor_node.tag.as_str() == "table" {
                        if let Some(cell_padding) = ancestor_node
                            .attributes
                            .iter()
                            .find(|(name, _)| name.as_str().eq_ignore_ascii_case("cellpadding"))
                            .and_then(|(_, value)| value.trim().parse::<f32>().ok())
                            .filter(|value| value.is_finite() && *value >= 0.0)
                        {
                            // The legacy HTML cellpadding attribute is a
                            // presentational hint below author CSS. Apply it
                            // only when the cell has no authored padding.
                            style.padding = w3cos_std::style::Edges::all(cell_padding);
                            presentational_padding = true;
                        }
                        break;
                    }
                    ancestor = ancestor_node.parent;
                }
                if !presentational_padding {
                    style
                        .custom_properties
                        .get_or_insert_with(Default::default)
                        .insert(
                            "--w3cos-internal-table-cell-ua-padding".to_string(),
                            "1".to_string(),
                        );
                }
            }
        }

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
                // Text nodes inherit computed table properties from their
                // element parent, but those properties do not apply to the
                // anonymous inline box used by the component IR. Normalize
                // after assigning the text node's used display so inherited
                // `caption-side`/`empty-cells` do not split a text run.
                normalize_css_table_internal_used_style(&mut style);
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
                    style.height =
                        w3cos_std::style::Dimension::Px(style.font_size * style.line_height);
                    return self
                        .attach_native_host(id, w3cos_std::Component::text("\u{2028}", style));
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
                let supports_text_pseudos = matches!(
                    style.display,
                    w3cos_std::style::Display::Block
                        | w3cos_std::style::Display::InlineBlock
                        | w3cos_std::style::Display::ListItem
                        | w3cos_std::style::Display::TableCell
                        | w3cos_std::style::Display::TableCaption
                );
                let first_line_declarations = supports_text_pseudos
                    .then(|| {
                        stylesheet::matching_pseudo_declarations_for_node(self, id, "::first-line")
                    })
                    .unwrap_or_default();
                let first_letter_declarations = supports_text_pseudos
                    .then(|| {
                        stylesheet::matching_pseudo_declarations_for_node(
                            self,
                            id,
                            "::first-letter",
                        )
                    })
                    .unwrap_or_default();
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
                                && component.style.text_indent == generated[0].style.text_indent
                                && component.style.text_transform
                                    == generated[0].style.text_transform
                                && component.style.letter_spacing
                                    == generated[0].style.letter_spacing
                                && component.style.word_spacing == generated[0].style.word_spacing
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
                        style.text_indent = generated_style.text_indent;
                        style.text_transform = generated_style.text_transform;
                        style.letter_spacing = generated_style.letter_spacing;
                        style.word_spacing = generated_style.word_spacing;
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
                    && first_line_declarations.is_empty()
                    && first_letter_declarations.is_empty()
                {
                    let child = self.get_node(child_ids[0]);
                    if child.node_type == NodeType::Text {
                        let text = self.rendered_text_content(&child_ids, 0, ancestors, &style);
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
                        && principal_box_can_merge_generated_inline_text(&style)
                        && style.custom_properties.as_ref().is_none_or(|properties| {
                            !properties.contains_key("--w3cos-internal-vertical-align-length")
                        })
                    {
                        let grandchild_ids = self.children_ids(child_ids[0]);
                        if grandchild_ids.len() == 1 {
                            let grandchild = self.get_node(grandchild_ids[0]);
                            if grandchild.node_type == NodeType::Text {
                                ancestors.push(self.selector_context(id));
                                let child_style =
                                    self.computed_style(child_ids[0], ancestors, Some(&style));
                                ancestors.pop();
                                if child_style.float == w3cos_std::style::Float::None
                                    && child_style.display == w3cos_std::style::Display::Inline
                                {
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
                }
                let pushed = if node.node_type == NodeType::Element {
                    ancestors.push(self.selector_context(id));
                    true
                } else {
                    false
                };
                let mut block_in_inline = style.display == w3cos_std::style::Display::Inline
                    && child_ids.iter().any(|child_id| {
                        let child = self.get_node(*child_id);
                        if child.node_type != NodeType::Element {
                            return false;
                        }
                        let child_style = self.computed_style(*child_id, ancestors, Some(&style));
                        matches!(
                            child_style.display,
                            w3cos_std::style::Display::Block
                                | w3cos_std::style::Display::Flex
                                | w3cos_std::style::Display::Grid
                        ) && !matches!(
                            child_style.position,
                            w3cos_std::style::Position::Absolute
                                | w3cos_std::style::Position::Fixed
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
                        child.node_type != NodeType::Element || {
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
                if !block_in_inline
                    && style.display == w3cos_std::style::Display::Inline
                    && children.iter().any(|child| {
                        matches!(
                            child.style.display,
                            w3cos_std::style::Display::Block
                                | w3cos_std::style::Display::Flex
                                | w3cos_std::style::Display::Grid
                        ) && !matches!(
                            child.style.position,
                            w3cos_std::style::Position::Absolute
                                | w3cos_std::style::Position::Fixed
                        )
                    })
                {
                    // A nested inline may flatten its own split fragments
                    // only after the outer element's source children were
                    // inspected. Re-run the block-in-inline decision on the
                    // resulting component children so the split propagates to
                    // every inline ancestor.
                    block_in_inline = true;
                    style.display = w3cos_std::style::Display::Block;
                    children.retain(|child| {
                        !matches!(
                            child.kind,
                            w3cos_std::ComponentKind::Text { ref content }
                                if is_only_css_whitespace(content)
                        )
                    });
                }
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
                if !first_line_declarations.is_empty() {
                    let available_width = match style.width {
                        w3cos_std::style::Dimension::Px(width) => Some(width),
                        w3cos_std::style::Dimension::Em(width) => Some(width * style.font_size),
                        w3cos_std::style::Dimension::Rem(width) => Some(width * 16.0),
                        _ => None,
                    };
                    let (fragmented, _) = apply_first_line_style(
                        &mut children,
                        &first_line_declarations,
                        style.font_size * style.line_height,
                        available_width,
                    );
                    anonymous_inline_formatting_context |= fragmented;
                }
                if !first_letter_declarations.is_empty()
                    && apply_first_letter_style(&mut children, &first_letter_declarations)
                {
                    if style.float != w3cos_std::style::Float::None {
                        let mut line_style = w3cos_std::style::Style::default();
                        line_style.display = w3cos_std::style::Display::Flex;
                        line_style.flex_direction = w3cos_std::style::FlexDirection::Row;
                        line_style.align_items = w3cos_std::style::AlignItems::Baseline;
                        line_style.font_size = style.font_size;
                        line_style.font_family = style.font_family.clone();
                        line_style.line_height = style.line_height;
                        children = vec![w3cos_std::Component::row(line_style, children)];
                    } else {
                        anonymous_inline_formatting_context |= children
                            .iter()
                            .find(|component| {
                                component.style.display != w3cos_std::style::Display::None
                            })
                            .is_some_and(|component| {
                                matches!(
                                    component.style.display,
                                    w3cos_std::style::Display::Inline
                                        | w3cos_std::style::Display::InlineBlock
                                        | w3cos_std::style::Display::InlineFlex
                                        | w3cos_std::style::Display::InlineTable
                                )
                            });
                    }
                }
                promote_passive_vertical_align_extension(&mut style, &children);
                if matches!(
                    style.display,
                    w3cos_std::style::Display::Block
                        | w3cos_std::style::Display::InlineBlock
                        | w3cos_std::style::Display::ListItem
                        | w3cos_std::style::Display::TableCell
                ) {
                    children = hoist_floats_into_block_formatting_context(&style, children);
                }
                if matches!(
                    style.display,
                    w3cos_std::style::Display::Table | w3cos_std::style::Display::InlineTable
                ) {
                    // CSS table fixup has a semantic group order independent
                    // of DOM/pseudo source order: header groups precede row
                    // groups and footer groups follow them. Keep ordering
                    // stable within each group class.
                    children.sort_by_key(|component| match component.style.display {
                        w3cos_std::style::Display::TableCaption
                            if !component.style.caption_side_bottom =>
                        {
                            0
                        }
                        w3cos_std::style::Display::TableColumnGroup
                        | w3cos_std::style::Display::TableColumn => 1,
                        w3cos_std::style::Display::TableHeaderGroup => 2,
                        w3cos_std::style::Display::TableFooterGroup => 4,
                        w3cos_std::style::Display::TableCaption => 5,
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
                        matches!(component.style.position, w3cos_std::style::Position::Static)
                            && matches!(
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
                if matches!(
                    style.display,
                    w3cos_std::style::Display::Block
                        | w3cos_std::style::Display::InlineBlock
                        | w3cos_std::style::Display::TableCell
                        | w3cos_std::style::Display::TableCaption
                ) && children.len() == 1
                    && matches!(
                        children[0].style.display,
                        w3cos_std::style::Display::Inline
                            | w3cos_std::style::Display::InlineBlock
                            | w3cos_std::style::Display::InlineFlex
                            | w3cos_std::style::Display::InlineTable
                    )
                    && (!matches!(style.width, w3cos_std::style::Dimension::Auto)
                        || style.text_indent != w3cos_std::style::Dimension::Px(0.0)
                        || style.text_align != w3cos_std::style::TextAlign::Start
                        || (style.direction == w3cos_std::style::TextDirection::Rtl
                            && matches!(children[0].kind, w3cos_std::ComponentKind::Text { .. }))
                        || matches!(
                            style.unicode_bidi,
                            w3cos_std::style::UnicodeBidi::BidiOverride
                                | w3cos_std::style::UnicodeBidi::IsolateOverride
                        ))
                {
                    // A block still establishes an anonymous line box when
                    // it contains one retained inline fragment. This occurs
                    // for bidi/decorated spans that cannot be folded into the
                    // principal text leaf; without the line box, logical
                    // `text-align:start` has no containing width to align in.
                    anonymous_inline_formatting_context = true;
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

                if block_in_inline && !matches!(style.position, w3cos_std::style::Position::Static)
                {
                    // A positioned inline remains the containing block for
                    // absolutely positioned descendants even when in-flow
                    // block children fragment its principal inline box. Do
                    // not flatten this semantic owner into display:contents:
                    // retaining the inline row lets layout derive the used
                    // containing width from its first and last fragments.
                    style.display = w3cos_std::style::Display::Inline;
                    return self.attach_native_host(id, w3cos_std::Component::row(style, children));
                }

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

                let inline_declares_text_indent = style.display
                    == w3cos_std::style::Display::Inline
                    && stylesheet::matching_declarations_for_node(self, id)
                        .iter()
                        .any(|(name, _, _)| css_property_eq(name, "text-indent"));
                if inline_declares_text_indent {
                    let containing_indent = inherited
                        .map_or(w3cos_std::style::Dimension::Px(0.0), |parent| {
                            parent.text_indent
                        });
                    style.text_indent = containing_indent;
                    for child in &mut children {
                        if child.style.display == w3cos_std::style::Display::Inline
                            && matches!(child.kind, w3cos_std::ComponentKind::Text { .. })
                        {
                            child.style.text_indent = containing_indent;
                        }
                    }
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
                    if anonymous_inline_formatting_context {
                        let indent_spacing = match style.text_indent {
                            w3cos_std::style::Dimension::Px(value) => {
                                w3cos_std::style::Spacing::Px(value)
                            }
                            w3cos_std::style::Dimension::Percent(value) => {
                                w3cos_std::style::Spacing::Percent(value)
                            }
                            w3cos_std::style::Dimension::Rem(value) => {
                                w3cos_std::style::Spacing::Rem(value)
                            }
                            w3cos_std::style::Dimension::Em(value) => {
                                w3cos_std::style::Spacing::Em(value)
                            }
                            w3cos_std::style::Dimension::Vw(value) => {
                                w3cos_std::style::Spacing::Vw(value)
                            }
                            w3cos_std::style::Dimension::Vh(value) => {
                                w3cos_std::style::Spacing::Vh(value)
                            }
                            w3cos_std::style::Dimension::Auto => w3cos_std::style::Spacing::Px(0.0),
                        };
                        if indent_spacing != w3cos_std::style::Spacing::Px(0.0) {
                            // `text-indent` moves the first in-flow inline box,
                            // including an atomic inline-level box. Keep the
                            // box's inherited value: its own first formatted
                            // line is indented again after entering its inner
                            // formatting context.
                            if let Some(first_atomic_inline) = children.iter_mut().find(|child| {
                                child.style.float == w3cos_std::style::Float::None
                                    && child.style.position == w3cos_std::style::Position::Static
                                    && !matches!(child.kind, w3cos_std::ComponentKind::Text { .. })
                            }) {
                                let margin = match style.direction {
                                    w3cos_std::style::TextDirection::Ltr => {
                                        &mut first_atomic_inline.style.margin.left
                                    }
                                    w3cos_std::style::TextDirection::Rtl => {
                                        &mut first_atomic_inline.style.margin.right
                                    }
                                };
                                if *margin == w3cos_std::style::Spacing::Px(0.0) {
                                    *margin = indent_spacing;
                                }
                            }
                        }
                    }
                    if (anonymous_inline_formatting_context
                        || matches!(style.text_indent, w3cos_std::style::Dimension::Percent(_)))
                        && children.len() == 1
                        && matches!(children[0].kind, w3cos_std::ComponentKind::Text { .. })
                        && matches!(
                            style.white_space,
                            w3cos_std::style::WhiteSpace::Normal
                                | w3cos_std::style::WhiteSpace::PreLine
                        )
                        && (!matches!(style.width, w3cos_std::style::Dimension::Auto)
                            || style.text_indent != w3cos_std::style::Dimension::Px(0.0)
                            || style.text_align != w3cos_std::style::TextAlign::Start)
                    {
                        // A text run in a fixed-width block is not an atomic
                        // flex item: its line box uses the block content width
                        // and wraps internally. Pass that width constraint to
                        // the portable text leaf instead of retaining its
                        // unconstrained max-content width.
                        children[0].style.width = w3cos_std::style::Dimension::Percent(100.0);
                        children[0].style.min_width = w3cos_std::style::Dimension::Px(0.0);
                        children[0].style.flex_shrink = 1.0;
                    }
                    if anonymous_inline_formatting_context {
                        for child in &mut children {
                            if matches!(
                                child.style.display,
                                w3cos_std::style::Display::InlineBlock
                                    | w3cos_std::style::Display::InlineFlex
                                    | w3cos_std::style::Display::InlineTable
                            ) || matches!(child.kind, w3cos_std::ComponentKind::Image { .. })
                            {
                                child.style.flex_shrink = 0.0;
                            }
                        }
                    }
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
                                let mut line_item_style = w3cos_std::style::Style::default();
                                line_item_style.display = w3cos_std::style::Display::InlineFlex;
                                line_item_style.flex_direction =
                                    w3cos_std::style::FlexDirection::Row;
                                line_item_style.align_items = w3cos_std::style::AlignItems::FlexEnd;
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
                        line_style.width = w3cos_std::style::Dimension::Percent(100.0);
                        line_style.direction = style.direction;
                        line_style.unicode_bidi = style.unicode_bidi;
                        line_style.text_align = style.text_align;
                        line_style.justify_content = match (style.text_align, style.direction) {
                            (w3cos_std::style::TextAlign::Right, _)
                            | (
                                w3cos_std::style::TextAlign::Start,
                                w3cos_std::style::TextDirection::Rtl,
                            )
                            | (
                                w3cos_std::style::TextAlign::End,
                                w3cos_std::style::TextDirection::Ltr,
                            ) => w3cos_std::style::JustifyContent::FlexEnd,
                            (w3cos_std::style::TextAlign::Center, _) => {
                                w3cos_std::style::JustifyContent::Center
                            }
                            _ => w3cos_std::style::JustifyContent::FlexStart,
                        };
                        line_style.align_items = if uses_inline_strut_wrappers {
                            w3cos_std::style::AlignItems::FlexStart
                        } else {
                            w3cos_std::style::AlignItems::Baseline
                        };
                        children = vec![w3cos_std::Component::row(line_style, children)];
                    } else if style.display == w3cos_std::style::Display::TableRow {
                        let table_direction =
                            inherited.map_or(style.direction, |style| style.direction);
                        style.flex_direction = match table_direction {
                            w3cos_std::style::TextDirection::Ltr => {
                                w3cos_std::style::FlexDirection::Row
                            }
                            w3cos_std::style::TextDirection::Rtl => {
                                w3cos_std::style::FlexDirection::RowReverse
                            }
                        };
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
                        let authored_block = style.display == w3cos_std::style::Display::Block;
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
                            match style.align_self {
                                w3cos_std::style::AlignSelf::FlexStart => {
                                    w3cos_std::style::AlignItems::FlexStart
                                }
                                w3cos_std::style::AlignSelf::FlexEnd => {
                                    w3cos_std::style::AlignItems::FlexEnd
                                }
                                _ => w3cos_std::style::AlignItems::Baseline,
                            }
                        };
                        if anonymous_inline_formatting_context
                            && style.white_space != w3cos_std::style::WhiteSpace::NoWrap
                        {
                            style.flex_wrap = w3cos_std::style::FlexWrap::Wrap;
                        }
                        if authored_block
                            && matches!(style.min_height, w3cos_std::style::Dimension::Auto)
                        {
                            // Lowering a block inline-formatting context to a
                            // flex row must retain the block's initial line
                            // box strut. A short replaced element otherwise
                            // collapses an authored tall `line-height`.
                            style.min_height = w3cos_std::style::Dimension::Px(
                                style.font_size * style.line_height,
                            );
                        }
                    }
                    style.justify_content = match (style.text_align, style.direction) {
                        (w3cos_std::style::TextAlign::Right, _)
                        | (
                            w3cos_std::style::TextAlign::Start,
                            w3cos_std::style::TextDirection::Rtl,
                        )
                        | (
                            w3cos_std::style::TextAlign::End,
                            w3cos_std::style::TextDirection::Ltr,
                        ) => w3cos_std::style::JustifyContent::FlexEnd,
                        (w3cos_std::style::TextAlign::Center, _) => {
                            w3cos_std::style::JustifyContent::Center
                        }
                        (w3cos_std::style::TextAlign::Left, _)
                        | (w3cos_std::style::TextAlign::Justify, _)
                        | (
                            w3cos_std::style::TextAlign::Start,
                            w3cos_std::style::TextDirection::Ltr,
                        )
                        | (
                            w3cos_std::style::TextAlign::End,
                            w3cos_std::style::TextDirection::Rtl,
                        ) => style.justify_content,
                    };
                }

                normalize_css_table_internal_used_style(&mut style);
                if style.display == w3cos_std::style::Display::TableRow {
                    // CSS `direction` on a table row does not itself reorder
                    // columns. Column order follows the containing table (or
                    // row-group) direction, which arrives as the inherited
                    // principal-box style here.
                    let table_direction =
                        inherited.map_or(style.direction, |style| style.direction);
                    style.flex_direction = match table_direction {
                        w3cos_std::style::TextDirection::Ltr => {
                            w3cos_std::style::FlexDirection::Row
                        }
                        w3cos_std::style::TextDirection::Rtl => {
                            w3cos_std::style::FlexDirection::RowReverse
                        }
                    };
                }

                if block_in_inline
                    && !self.events.has_listeners(id)
                    && matches!(style.position, w3cos_std::style::Position::Relative)
                    && children
                        .iter()
                        .any(|child| child.style.float != w3cos_std::style::Float::None)
                {
                    let mut passive_style = style.clone();
                    passive_style.position = w3cos_std::style::Position::Static;
                    passive_style.top = w3cos_std::style::Dimension::Auto;
                    passive_style.right = w3cos_std::style::Dimension::Auto;
                    passive_style.bottom = w3cos_std::style::Dimension::Auto;
                    passive_style.left = w3cos_std::style::Dimension::Auto;
                    if principal_box_can_merge_generated_inline_text(&passive_style) {
                        let mut floats = Vec::new();
                        let mut in_flow = Vec::new();
                        for mut child in children {
                            if child.style.float != w3cos_std::style::Float::None {
                                floats.push(child);
                                continue;
                            }
                            if matches!(child.style.position, w3cos_std::style::Position::Static) {
                                child.style.position = w3cos_std::style::Position::Relative;
                                child.style.top = style.top;
                                child.style.right = style.right;
                                child.style.bottom = style.bottom;
                                child.style.left = style.left;
                            }
                            in_flow.push(child);
                        }
                        floats.extend(in_flow);
                        let mut contents_style = w3cos_std::style::Style::default();
                        contents_style.display = w3cos_std::style::Display::Contents;
                        return w3cos_std::Component::boxed(contents_style, floats);
                    }
                }

                if block_in_inline
                    && !self.events.has_listeners(id)
                    && children.len() >= 2
                    && children.iter().all(|child| {
                        matches!(
                            child.style.display,
                            w3cos_std::style::Display::Block
                                | w3cos_std::style::Display::Flex
                                | w3cos_std::style::Display::Grid
                        )
                    })
                {
                    let fragment = |retain_left: bool, retain_right: bool| {
                        let mut fragment = style.clone();
                        fragment.display = w3cos_std::style::Display::Inline;
                        fragment.width = w3cos_std::style::Dimension::Auto;
                        fragment.height = w3cos_std::style::Dimension::Auto;
                        fragment.min_width = w3cos_std::style::Dimension::Auto;
                        fragment.min_height = w3cos_std::style::Dimension::Auto;
                        fragment.max_width = w3cos_std::style::Dimension::Auto;
                        fragment.max_height = w3cos_std::style::Dimension::Auto;
                        fragment.margin = w3cos_std::style::Edges::ZERO;
                        fragment.padding.left = if retain_left {
                            style.padding.left
                        } else {
                            w3cos_std::style::Spacing::Px(0.0)
                        };
                        fragment.padding.right = if retain_right {
                            style.padding.right
                        } else {
                            w3cos_std::style::Spacing::Px(0.0)
                        };
                        let top = style.border_top_width.unwrap_or(style.border_width);
                        let bottom = style.border_bottom_width.unwrap_or(style.border_width);
                        let left = style.border_left_width.unwrap_or(style.border_width);
                        let right = style.border_right_width.unwrap_or(style.border_width);
                        fragment.border_width = 0.0;
                        fragment.border_top_width = Some(top);
                        fragment.border_bottom_width = Some(bottom);
                        fragment.border_left_width = Some(if retain_left { left } else { 0.0 });
                        fragment.border_right_width = Some(if retain_right { right } else { 0.0 });
                        w3cos_std::Component::row(fragment, Vec::new())
                    };
                    let inline_start_is_left =
                        style.direction == w3cos_std::style::TextDirection::Ltr;
                    let mut fragments = vec![fragment(inline_start_is_left, !inline_start_is_left)];
                    let child_count = children.len();
                    for (index, child) in children.into_iter().enumerate() {
                        fragments.push(child);
                        if index + 1 < child_count {
                            fragments.push(fragment(false, false));
                        }
                    }
                    fragments.push(fragment(!inline_start_is_left, inline_start_is_left));
                    let mut contents_style = w3cos_std::style::Style::default();
                    contents_style.display = w3cos_std::style::Display::Contents;
                    return w3cos_std::Component::boxed(contents_style, fragments);
                }

                if block_in_inline
                    && !self.events.has_listeners(id)
                    && children.first().is_some_and(|child| {
                        matches!(
                            child.style.display,
                            w3cos_std::style::Display::Inline
                                | w3cos_std::style::Display::InlineBlock
                                | w3cos_std::style::Display::InlineFlex
                                | w3cos_std::style::Display::InlineTable
                        )
                    })
                    && children.iter().any(|child| {
                        matches!(
                            child.style.display,
                            w3cos_std::style::Display::Block
                                | w3cos_std::style::Display::Flex
                                | w3cos_std::style::Display::Grid
                        )
                    })
                    && children.iter().all(|child| {
                        matches!(
                            child.style.display,
                            w3cos_std::style::Display::Inline
                                | w3cos_std::style::Display::InlineBlock
                                | w3cos_std::style::Display::InlineFlex
                                | w3cos_std::style::Display::InlineTable
                                | w3cos_std::style::Display::Block
                                | w3cos_std::style::Display::Flex
                                | w3cos_std::style::Display::Grid
                                | w3cos_std::style::Display::TableCell
                                | w3cos_std::style::Display::Table
                        )
                    })
                {
                    let is_block = |child: &w3cos_std::Component| {
                        matches!(
                            child.style.display,
                            w3cos_std::style::Display::Block
                                | w3cos_std::style::Display::Flex
                                | w3cos_std::style::Display::Grid
                                | w3cos_std::style::Display::TableCell
                                | w3cos_std::style::Display::Table
                        )
                    };
                    let first_block = children.iter().position(is_block).expect("split block");
                    let last_block = children.iter().rposition(is_block).expect("split block");
                    let fragment_style = |retain_left: bool, retain_right: bool| {
                        let mut fragment = style.clone();
                        fragment.display = w3cos_std::style::Display::InlineFlex;
                        fragment.flex_direction = w3cos_std::style::FlexDirection::Row;
                        fragment.align_items = w3cos_std::style::AlignItems::Baseline;
                        fragment.width = w3cos_std::style::Dimension::Auto;
                        fragment.height = w3cos_std::style::Dimension::Auto;
                        fragment.min_width = w3cos_std::style::Dimension::Auto;
                        fragment.min_height = w3cos_std::style::Dimension::Auto;
                        fragment.max_width = w3cos_std::style::Dimension::Auto;
                        fragment.max_height = w3cos_std::style::Dimension::Auto;
                        fragment.margin = w3cos_std::style::Edges::ZERO;
                        fragment.padding.left = if retain_left {
                            style.padding.left
                        } else {
                            w3cos_std::style::Spacing::Px(0.0)
                        };
                        fragment.padding.right = if retain_right {
                            style.padding.right
                        } else {
                            w3cos_std::style::Spacing::Px(0.0)
                        };
                        let top = style.border_top_width.unwrap_or(style.border_width);
                        let bottom = style.border_bottom_width.unwrap_or(style.border_width);
                        let left = style.border_left_width.unwrap_or(style.border_width);
                        let right = style.border_right_width.unwrap_or(style.border_width);
                        fragment.border_width = 0.0;
                        fragment.border_top_width = Some(top);
                        fragment.border_bottom_width = Some(bottom);
                        fragment.border_left_width = Some(if retain_left { left } else { 0.0 });
                        fragment.border_right_width = Some(if retain_right { right } else { 0.0 });
                        fragment
                    };
                    let inline_start_is_left =
                        style.direction == w3cos_std::style::TextDirection::Ltr;
                    let mut remaining = children;
                    let leading = remaining.drain(..first_block).collect::<Vec<_>>();
                    let trailing = remaining.split_off(last_block - first_block + 1);
                    let mut fragments = vec![w3cos_std::Component::row(
                        fragment_style(inline_start_is_left, !inline_start_is_left),
                        leading,
                    )];
                    let mut middle_inline = Vec::new();
                    for mut child in remaining {
                        if is_block(&child) {
                            if !middle_inline.is_empty() {
                                fragments.push(w3cos_std::Component::row(
                                    fragment_style(false, false),
                                    std::mem::take(&mut middle_inline),
                                ));
                            }
                            if child.style.display == w3cos_std::style::Display::Table {
                                child
                                    .style
                                    .custom_properties
                                    .get_or_insert_with(Default::default)
                                    .insert(
                                        "--w3cos-internal-split-inline-block".to_string(),
                                        "1".to_string(),
                                    );
                            }
                            fragments.push(child);
                        } else {
                            middle_inline.push(child);
                        }
                    }
                    if !middle_inline.is_empty() {
                        fragments.push(w3cos_std::Component::row(
                            fragment_style(false, false),
                            middle_inline,
                        ));
                    }
                    fragments.push(w3cos_std::Component::row(
                        fragment_style(!inline_start_is_left, inline_start_is_left),
                        trailing,
                    ));
                    let mut contents_style = w3cos_std::style::Style::default();
                    contents_style.display = w3cos_std::style::Display::Contents;
                    return w3cos_std::Component::boxed(contents_style, fragments);
                }

                if block_in_inline
                    && !self.events.has_listeners(id)
                    && children.len() >= 2
                    && matches!(
                        children[0].style.display,
                        w3cos_std::style::Display::Block
                            | w3cos_std::style::Display::Flex
                            | w3cos_std::style::Display::Grid
                    )
                    && children[1..].iter().all(|child| {
                        matches!(
                            child.style.display,
                            w3cos_std::style::Display::Inline
                                | w3cos_std::style::Display::InlineBlock
                                | w3cos_std::style::Display::InlineFlex
                                | w3cos_std::style::Display::InlineTable
                        )
                    })
                {
                    let fragment_style = |inline_start: bool| {
                        let mut fragment = style.clone();
                        fragment.display = w3cos_std::style::Display::Inline;
                        fragment.width = w3cos_std::style::Dimension::Auto;
                        fragment.height = w3cos_std::style::Dimension::Auto;
                        fragment.min_width = w3cos_std::style::Dimension::Auto;
                        fragment.min_height = w3cos_std::style::Dimension::Auto;
                        fragment.max_width = w3cos_std::style::Dimension::Auto;
                        fragment.max_height = w3cos_std::style::Dimension::Auto;
                        fragment.margin = w3cos_std::style::Edges::ZERO;
                        fragment.padding.left = if inline_start {
                            style.padding.left
                        } else {
                            w3cos_std::style::Spacing::Px(0.0)
                        };
                        fragment.padding.right = if inline_start {
                            w3cos_std::style::Spacing::Px(0.0)
                        } else {
                            style.padding.right
                        };
                        let top = style.border_top_width.unwrap_or(style.border_width);
                        let bottom = style.border_bottom_width.unwrap_or(style.border_width);
                        let left = style.border_left_width.unwrap_or(style.border_width);
                        let right = style.border_right_width.unwrap_or(style.border_width);
                        fragment.border_width = 0.0;
                        fragment.border_top_width = Some(top);
                        fragment.border_bottom_width = Some(bottom);
                        fragment.border_left_width = Some(if inline_start { left } else { 0.0 });
                        fragment.border_right_width = Some(if inline_start { 0.0 } else { right });
                        fragment
                    };
                    let mut children = children.into_iter();
                    let block = children.next().expect("split inline block child");
                    let trailing = children.collect::<Vec<_>>();
                    let start = w3cos_std::Component::row(fragment_style(true), Vec::new());
                    let end = w3cos_std::Component::row(fragment_style(false), trailing);
                    let mut contents_style = w3cos_std::style::Style::default();
                    contents_style.display = w3cos_std::style::Display::Contents;
                    return w3cos_std::Component::boxed(contents_style, vec![start, block, end]);
                }

                if block_in_inline
                    && !self.events.has_listeners(id)
                    && children.len() == 1
                    && matches!(
                        children[0].style.display,
                        w3cos_std::style::Display::Block
                            | w3cos_std::style::Display::Flex
                            | w3cos_std::style::Display::Grid
                    )
                {
                    let side_fragment = |left: bool| {
                        let mut fragment = style.clone();
                        fragment.display = w3cos_std::style::Display::Inline;
                        fragment.width = w3cos_std::style::Dimension::Auto;
                        fragment.height = w3cos_std::style::Dimension::Auto;
                        fragment.min_width = w3cos_std::style::Dimension::Auto;
                        fragment.min_height = w3cos_std::style::Dimension::Auto;
                        fragment.max_width = w3cos_std::style::Dimension::Auto;
                        fragment.max_height = w3cos_std::style::Dimension::Auto;
                        fragment.margin = w3cos_std::style::Edges::ZERO;
                        let retained_padding = if left {
                            style.padding.left
                        } else {
                            style.padding.right
                        };
                        fragment.padding.left = if left {
                            retained_padding
                        } else {
                            w3cos_std::style::Spacing::Px(0.0)
                        };
                        fragment.padding.right = if left {
                            w3cos_std::style::Spacing::Px(0.0)
                        } else {
                            retained_padding
                        };
                        let top = style.border_top_width.unwrap_or(style.border_width);
                        let bottom = style.border_bottom_width.unwrap_or(style.border_width);
                        let side = if left {
                            style.border_left_width.unwrap_or(style.border_width)
                        } else {
                            style.border_right_width.unwrap_or(style.border_width)
                        };
                        fragment.border_width = 0.0;
                        fragment.border_top_width = Some(top);
                        fragment.border_bottom_width = Some(bottom);
                        fragment.border_left_width = Some(if left { side } else { 0.0 });
                        fragment.border_right_width = Some(if left { 0.0 } else { side });
                        w3cos_std::Component::row(fragment, Vec::new())
                    };
                    let has_left_fragment = style.padding.left
                        != w3cos_std::style::Spacing::Px(0.0)
                        || style.border_left_width.unwrap_or(style.border_width) > 0.0;
                    let has_right_fragment = style.padding.right
                        != w3cos_std::style::Spacing::Px(0.0)
                        || style.border_right_width.unwrap_or(style.border_width) > 0.0;
                    if !has_left_fragment
                        && !has_right_fragment
                        && principal_box_can_merge_generated_inline_text(&style)
                    {
                        // A passive split-inline wrapper has no principal box
                        // to lay out. Flatten it so the in-flow block keeps the
                        // surrounding block container as its containing block
                        // (notably for percentage heights).
                        return children.remove(0);
                    }
                    if has_left_fragment || has_right_fragment {
                        let block = children.remove(0);
                        let mut fragments = Vec::with_capacity(3);
                        let inline_start_is_left =
                            style.direction == w3cos_std::style::TextDirection::Ltr;
                        if (inline_start_is_left && has_left_fragment)
                            || (!inline_start_is_left && has_right_fragment)
                        {
                            fragments.push(side_fragment(inline_start_is_left));
                        }
                        fragments.push(block);
                        if (inline_start_is_left && has_right_fragment)
                            || (!inline_start_is_left && has_left_fragment)
                        {
                            fragments.push(side_fragment(!inline_start_is_left));
                        }
                        let mut contents_style = w3cos_std::style::Style::default();
                        contents_style.display = w3cos_std::style::Display::Contents;
                        return w3cos_std::Component::boxed(contents_style, fragments);
                    }
                }

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
                        if is_row {
                            w3cos_std::Component::row(style, children)
                        } else {
                            w3cos_std::Component::column(style, children)
                        }
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
                        if image_style.display == w3cos_std::style::Display::TableCell {
                            // CSS table-internal display values do not turn a
                            // replaced element into a real table cell. Its
                            // used value remains inline-level, and table fixup
                            // wraps it together with adjacent inline content
                            // in an anonymous cell.
                            image_style.display = w3cos_std::style::Display::InlineBlock;
                            image_style
                                .custom_properties
                                .get_or_insert_with(Default::default)
                                .insert(
                                    "--w3cos-internal-replaced-table-cell".to_string(),
                                    "1".to_string(),
                                );
                        }
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

        self.mark_text_dirty(target_id);

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
            self.mark_text_dirty(target_id);

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

fn declaration_value_is_valid(property: &str, value: &str) -> bool {
    if css_property_eq(property, "color") {
        let value = value.trim();
        return w3cos_std::Color::from_css(value).is_some()
            || matches!(
                value.to_ascii_lowercase().as_str(),
                "currentcolor" | "inherit" | "initial" | "revert" | "revert-layer" | "unset"
            );
    }
    true
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
    if !declares("font-weight") && !declares("font") && !heading && !matches!(tag, "b" | "strong") {
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
    if !declares("text-indent") {
        style.text_indent = parent.text_indent;
    }
    if !declares("text-transform") {
        style.text_transform = parent.text_transform;
    }
    if !declares("letter-spacing") {
        style.letter_spacing = parent.letter_spacing;
    }
    if !declares("word-spacing") {
        style.word_spacing = parent.word_spacing;
    }
    if !declares("border-collapse") {
        style.border_collapse = parent.border_collapse;
    }
    if !declares("empty-cells") {
        style.empty_cells_hide = parent.empty_cells_hide;
    }
    if !declares("caption-side") {
        style.caption_side_bottom = parent.caption_side_bottom;
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
    if !declares("direction") {
        style.direction = parent.direction;
    }
    if !declares("visibility") {
        style.visibility = parent.visibility;
    }
}

fn text_pseudo_style(
    base: &w3cos_std::style::Style,
    declarations: &[(String, String, u32)],
) -> w3cos_std::style::Style {
    let mut merged = CSSStyleDeclaration::from_style(base.clone());
    for (property, value, _) in declarations {
        if !css_property_eq(property, "content") {
            merged.set_property(property, value);
        }
    }
    let mut style = merged.to_style();
    if let Some((property, value)) = declarations
        .iter()
        .rev()
        .find(|(property, _, _)| {
            css_property_eq(property, "font-size") || css_property_eq(property, "font")
        })
        .map(|(property, value, _)| (property, value.as_str()))
    {
        let value = if css_property_eq(property, "font") {
            font_shorthand_size_token(value).unwrap_or(value)
        } else {
            value
        };
        let relative_size = relative_font_size_px(value, base);
        if let Some(relative_size) = relative_size {
            style.font_size = relative_size;
        }
    }
    style
}

fn font_shorthand_size_token(value: &str) -> Option<&str> {
    value
        .split_once('/')
        .map_or(value, |(before, _)| before)
        .split_ascii_whitespace()
        .rev()
        .find(|token| {
            token.ends_with("rem")
                || token.ends_with("em")
                || token.ends_with("ex")
                || token.ends_with('%')
                || token.ends_with("px")
                || token.parse::<f32>().is_ok()
        })
}

fn relative_font_size_px(value: &str, parent: &w3cos_std::style::Style) -> Option<f32> {
    let value = value.trim();
    value
        .strip_suffix("rem")
        .and_then(|number| number.trim().parse::<f32>().ok())
        .map(|number| number * 16.0)
        .or_else(|| {
            value
                .strip_suffix("em")
                .and_then(|number| number.trim().parse::<f32>().ok())
                .map(|number| number * parent.font_size)
        })
        .or_else(|| {
            value
                .strip_suffix("ex")
                .and_then(|number| number.trim().parse::<f32>().ok())
                .map(|number| number * css_ex_size(parent))
        })
        .or_else(|| {
            value
                .strip_suffix('%')
                .and_then(|number| number.trim().parse::<f32>().ok())
                .map(|number| number * parent.font_size / 100.0)
        })
}

fn vertical_align_length_px(value: &str, style: &w3cos_std::style::Style) -> Option<f32> {
    let value = value.trim();
    value
        .strip_suffix("rem")
        .and_then(|number| number.trim().parse::<f32>().ok())
        .map(|number| number * 16.0)
        .or_else(|| {
            value
                .strip_suffix("em")
                .and_then(|number| number.trim().parse::<f32>().ok())
                .map(|number| number * style.font_size)
        })
        .or_else(|| {
            value
                .strip_suffix("ex")
                .and_then(|number| number.trim().parse::<f32>().ok())
                .map(|number| number * css_ex_size(style))
        })
        .or_else(|| {
            value
                .strip_suffix('%')
                .and_then(|number| number.trim().parse::<f32>().ok())
                .map(|number| number * style.font_size * style.line_height / 100.0)
        })
        .or_else(|| w3cos_std::style::parse_absolute_length_px(value))
}

fn first_line_text_style(
    base: &w3cos_std::style::Style,
    declarations: &[(String, String, u32)],
    fragment_height: f32,
) -> w3cos_std::style::Style {
    let preserves_background = base.background.a != 0;
    let preserves_vertical_align = base.align_self != w3cos_std::style::AlignSelf::Auto;
    if !preserves_background && !preserves_vertical_align {
        let mut style = text_pseudo_style(base, declarations);
        attach_first_line_fragment_clip(&mut style, declarations, fragment_height);
        return style;
    }
    let inherited_inline_style = declarations
        .iter()
        .filter(|(property, _, _)| {
            !(preserves_background
                && (css_property_eq(property, "background")
                    || css_property_eq(property, "background-color")))
                && !(preserves_vertical_align && css_property_eq(property, "vertical-align"))
        })
        .cloned()
        .collect::<Vec<_>>();
    let mut style = text_pseudo_style(base, &inherited_inline_style);
    attach_first_line_fragment_clip(&mut style, declarations, fragment_height);
    promote_vertical_align_line_box_extension(&mut style);
    style
}

fn attach_first_line_fragment_clip(
    style: &mut w3cos_std::style::Style,
    declarations: &[(String, String, u32)],
    fragment_height: f32,
) {
    let alignment = declarations
        .iter()
        .rev()
        .find(|(property, _, _)| css_property_eq(property, "vertical-align"))
        .map(|(_, value, _)| value.trim().to_ascii_lowercase());
    if matches!(alignment.as_deref(), Some("top" | "bottom")) {
        style
            .custom_properties
            .get_or_insert_with(Default::default)
            .insert(
                "--w3cos-internal-inline-fragment-clip".to_string(),
                format!(
                    "{} {fragment_height}",
                    alignment.expect("alignment checked")
                ),
            );
    }
}

fn promote_passive_vertical_align_extension(
    style: &mut w3cos_std::style::Style,
    children: &[w3cos_std::Component],
) {
    if children.is_empty()
        || !matches!(
            style.display,
            w3cos_std::style::Display::Inline
                | w3cos_std::style::Display::InlineBlock
                | w3cos_std::style::Display::InlineFlex
        )
        || style.background.a != 0
        || style
            .background_image
            .as_deref()
            .is_some_and(|image| !image.eq_ignore_ascii_case("none"))
        || style.border_width != 0.0
    {
        return;
    }
    let has_length_alignment = style
        .custom_properties
        .as_ref()
        .and_then(|properties| properties.get("--w3cos-internal-vertical-align-length"))
        .is_some();
    if !has_length_alignment {
        return;
    }
    promote_vertical_align_line_box_extension(style);
}

fn promote_vertical_align_line_box_extension(style: &mut w3cos_std::style::Style) {
    let Some(offset) = style
        .custom_properties
        .as_ref()
        .and_then(|properties| properties.get("--w3cos-internal-vertical-align-length"))
        .and_then(|value| value.split_ascii_whitespace().next())
        .and_then(|value| value.parse::<f32>().ok())
    else {
        return;
    };
    let line_height = style.font_size * style.line_height;
    style.display = w3cos_std::style::Display::InlineFlex;
    style.height = w3cos_std::style::Dimension::Px(line_height + offset.abs());
    style.align_items = w3cos_std::style::AlignItems::FlexStart;
    style.margin.top = w3cos_std::style::Spacing::Px(0.0);
    style.margin.bottom = w3cos_std::style::Spacing::Px(0.0);
    style
        .custom_properties
        .get_or_insert_with(Default::default)
        .insert(
            "--w3cos-internal-inline-fragment-clip".to_string(),
            format!("top {line_height}"),
        );
}

fn cloned_text_fragment(
    source: &w3cos_std::Component,
    content: String,
    style: w3cos_std::style::Style,
) -> w3cos_std::Component {
    let mut fragment = source.clone();
    fragment.kind = w3cos_std::ComponentKind::Text { content };
    fragment.style = style;
    fragment.children.clear();
    fragment
}

fn first_letter_fragment_range(
    content: &str,
    letter_seen: &mut bool,
) -> (Option<std::ops::Range<usize>>, bool) {
    let mut start = None;
    let mut end = None;
    for (index, character) in content.char_indices() {
        if start.is_none() {
            if !*letter_seen && character.is_whitespace() {
                continue;
            }
            if *letter_seen && (character.is_whitespace() || character.is_alphanumeric()) {
                return (None, true);
            }
            start = Some(index);
        }
        if *letter_seen && (character.is_alphanumeric() || character.is_whitespace()) {
            return (
                Some(start.expect("first-letter fragment")..end.expect("fragment end")),
                true,
            );
        }
        end = Some(index + character.len_utf8());
        *letter_seen |= character.is_alphanumeric();
    }
    (start.zip(end).map(|(start, end)| start..end), false)
}

fn apply_first_letter_style(
    components: &mut Vec<w3cos_std::Component>,
    declarations: &[(String, String, u32)],
) -> bool {
    let mut letter_seen = false;
    apply_first_letter_style_inner(components, declarations, &mut letter_seen).0
}

fn apply_first_letter_style_inner(
    components: &mut Vec<w3cos_std::Component>,
    declarations: &[(String, String, u32)],
    letter_seen: &mut bool,
) -> (bool, bool) {
    let mut changed = false;
    let mut index = 0;
    while index < components.len() {
        if components[index].style.display == w3cos_std::style::Display::None
            || components[index].style.float != w3cos_std::style::Float::None
            || matches!(
                components[index].style.position,
                w3cos_std::style::Position::Absolute | w3cos_std::style::Position::Fixed
            )
        {
            index += 1;
            continue;
        }

        if let w3cos_std::ComponentKind::Text { content } = &components[index].kind {
            if content == "\u{2028}" {
                return (changed, true);
            }
            let letter_was_seen = *letter_seen;
            let (range, done) = first_letter_fragment_range(content, letter_seen);
            let Some(range) = range else {
                if done {
                    return (changed, true);
                }
                if !*letter_seen
                    && content.chars().all(char::is_whitespace)
                    && matches!(
                        components[index].style.white_space,
                        w3cos_std::style::WhiteSpace::Normal
                            | w3cos_std::style::WhiteSpace::NoWrap
                            | w3cos_std::style::WhiteSpace::PreLine
                    )
                {
                    components[index].kind = w3cos_std::ComponentKind::Text {
                        content: String::new(),
                    };
                    changed = true;
                }
                index += 1;
                continue;
            };
            let source = components[index].clone();
            let base_style = source.style.clone();
            let pseudo_style = text_pseudo_style(&base_style, declarations);
            let mut fragments = Vec::with_capacity(3);
            let collapses_leading_whitespace = !letter_was_seen
                && content[..range.start].chars().all(char::is_whitespace)
                && matches!(
                    base_style.white_space,
                    w3cos_std::style::WhiteSpace::Normal
                        | w3cos_std::style::WhiteSpace::NoWrap
                        | w3cos_std::style::WhiteSpace::PreLine
                );
            if range.start > 0 && !collapses_leading_whitespace {
                fragments.push(cloned_text_fragment(
                    &source,
                    content[..range.start].to_string(),
                    base_style.clone(),
                ));
            }
            fragments.push(cloned_text_fragment(
                &source,
                content[range.clone()].to_string(),
                pseudo_style,
            ));
            if range.end < content.len() {
                fragments.push(cloned_text_fragment(
                    &source,
                    content[range.end..].to_string(),
                    base_style,
                ));
            }
            components.splice(index..=index, fragments);
            changed = true;
            if done {
                return (changed, true);
            }
            index += 1;
            continue;
        }

        if !components[index].children.is_empty() {
            let (nested_changed, done) = apply_first_letter_style_inner(
                &mut components[index].children,
                declarations,
                letter_seen,
            );
            changed |= nested_changed;
            if done {
                return (changed, true);
            }
            index += 1;
            continue;
        }
        if !matches!(
            components[index].kind,
            w3cos_std::ComponentKind::Row | w3cos_std::ComponentKind::Box
        ) {
            return (changed, true);
        }
        index += 1;
    }
    (changed, false)
}

fn apply_first_line_style(
    components: &mut Vec<w3cos_std::Component>,
    declarations: &[(String, String, u32)],
    fragment_height: f32,
    available_width: Option<f32>,
) -> (bool, bool) {
    let mut used_width = 0.0;
    apply_first_line_style_inner(
        components,
        declarations,
        fragment_height,
        available_width,
        &mut used_width,
        false,
    )
}

fn apply_first_line_style_inner(
    components: &mut Vec<w3cos_std::Component>,
    declarations: &[(String, String, u32)],
    fragment_height: f32,
    available_width: Option<f32>,
    used_width: &mut f32,
    inside_first_line_inline: bool,
) -> (bool, bool) {
    let mut changed = false;
    let mut index = 0;
    while index < components.len() {
        if components[index].style.display == w3cos_std::style::Display::None
            || matches!(
                components[index].style.position,
                w3cos_std::style::Position::Absolute | w3cos_std::style::Position::Fixed
            )
        {
            index += 1;
            continue;
        }

        if components[index].style.float != w3cos_std::style::Float::None {
            if inside_first_line_inline {
                if let w3cos_std::ComponentKind::Text { content } = &components[index].kind {
                    if !content.is_empty() {
                        let advance = first_line_text_advance(content, &components[index].style);
                        components[index].style = first_line_text_style(
                            &components[index].style,
                            declarations,
                            fragment_height,
                        );
                        changed = true;
                        *used_width += advance;
                    }
                } else {
                    let (nested_changed, stopped) = apply_first_line_style_inner(
                        &mut components[index].children,
                        declarations,
                        fragment_height,
                        available_width,
                        used_width,
                        true,
                    );
                    changed |= nested_changed;
                    if stopped {
                        return (changed, true);
                    }
                }
            }
            index += 1;
            continue;
        }

        if let w3cos_std::ComponentKind::Text { content } = &components[index].kind {
            if let Some(break_at) = content.find('\u{2028}') {
                if break_at > 0 {
                    let source = components[index].clone();
                    let base_style = source.style.clone();
                    let pseudo_style =
                        first_line_text_style(&base_style, declarations, fragment_height);
                    let fragments = vec![
                        cloned_text_fragment(
                            &source,
                            content[..break_at].to_string(),
                            pseudo_style,
                        ),
                        cloned_text_fragment(&source, content[break_at..].to_string(), base_style),
                    ];
                    components.splice(index..=index, fragments);
                    changed = true;
                }
                return (changed, true);
            }
            if !content.is_empty() {
                let advance = first_line_text_advance(content, &components[index].style);
                components[index].style =
                    first_line_text_style(&components[index].style, declarations, fragment_height);
                changed = true;
                *used_width += advance;
                if available_width.is_some_and(|width| *used_width >= width) {
                    return (changed, true);
                }
            }
        } else if matches!(
            components[index].style.display,
            w3cos_std::style::Display::Inline
                | w3cos_std::style::Display::InlineBlock
                | w3cos_std::style::Display::InlineFlex
                | w3cos_std::style::Display::InlineTable
        ) {
            let (nested_changed, stopped) = apply_first_line_style_inner(
                &mut components[index].children,
                declarations,
                fragment_height,
                available_width,
                used_width,
                true,
            );
            changed |= nested_changed;
            if stopped {
                return (changed, true);
            }
        } else if !components[index].children.is_empty() {
            return (changed, true);
        }
        index += 1;
    }
    (changed, false)
}

fn first_line_text_advance(content: &str, style: &w3cos_std::style::Style) -> f32 {
    let uses_ahem = style.font_family.as_deref().is_some_and(|families| {
        families.split(',').any(|name| {
            name.trim()
                .trim_matches(['"', '\''])
                .eq_ignore_ascii_case("ahem")
        })
    });
    if !uses_ahem {
        return 0.0;
    }
    content
        .chars()
        .filter(|character| !character.is_whitespace())
        .count() as f32
        * style.font_size
}

fn css_ex_size(style: &w3cos_std::style::Style) -> f32 {
    let ratio = style
        .font_family
        .as_deref()
        .filter(|family| {
            family.split(',').any(|name| {
                name.trim()
                    .trim_matches(['"', '\''])
                    .eq_ignore_ascii_case("ahem")
            })
        })
        .map_or(0.5, |_| 0.8);
    style.font_size * ratio
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
    matches!(property, "display" | "quotes" | "text-transform")
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
        && style
            .background_image
            .as_deref()
            .is_none_or(|image| image.eq_ignore_ascii_case("none"))
        && style.box_shadow.is_none()
        && style.filter.is_none()
        && style.opacity == 1.0
        && style.transform == Transform2D::default()
}

fn painted_inline_text_box_can_merge(style: &w3cos_std::style::Style) -> bool {
    if style.background.a == 0
        || style
            .background_image
            .as_deref()
            .is_some_and(|image| !image.eq_ignore_ascii_case("none"))
    {
        return false;
    }
    let mut transparent = style.clone();
    transparent.background = w3cos_std::Color::TRANSPARENT;
    principal_box_can_merge_generated_inline_text(&transparent)
}

fn equivalent_text_style(left: &w3cos_std::style::Style, right: &w3cos_std::style::Style) -> bool {
    if left == right {
        return true;
    }
    let mut left = left.clone();
    let mut right = right.clone();
    for style in [&mut left, &mut right] {
        if style
            .background_image
            .as_deref()
            .is_some_and(|image| image.eq_ignore_ascii_case("none"))
        {
            style.background_image = None;
        }
    }
    left == right
}

fn equivalent_text_paint_style(
    left: &w3cos_std::style::Style,
    right: &w3cos_std::style::Style,
) -> bool {
    left.color == right.color
        && left.font_size == right.font_size
        && left.font_weight == right.font_weight
        && left.font_family == right.font_family
        && left.font_style == right.font_style
        && left.white_space == right.white_space
        && left.line_height == right.line_height
        && left.text_indent == right.text_indent
        && left.text_transform == right.text_transform
        && left.letter_spacing == right.letter_spacing
        && left.word_spacing == right.word_spacing
        && left.text_decoration == right.text_decoration
        && left.text_overflow == right.text_overflow
        && left.word_break == right.word_break
        && left.direction == right.direction
        && left.unicode_bidi == right.unicode_bidi
        && left.visibility == right.visibility
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

fn reorder_explicit_bidi_inline_rows(component: &mut w3cos_std::Component) {
    if let w3cos_std::ComponentKind::Text { content } = &mut component.kind
        && content.contains('\u{2028}')
        && component.style.unicode_bidi == w3cos_std::style::UnicodeBidi::Normal
        && (component.style.direction == w3cos_std::style::TextDirection::Rtl
            || content.chars().any(|character| {
                matches!(
                    unicode_bidi::bidi_class(character),
                    unicode_bidi::BidiClass::R
                        | unicode_bidi::BidiClass::AL
                        | unicode_bidi::BidiClass::AN
                )
            }))
    {
        use unicode_bidi::{BidiInfo, Level};

        let paragraph_level = match component.style.direction {
            w3cos_std::style::TextDirection::Ltr => Level::ltr(),
            w3cos_std::style::TextDirection::Rtl => Level::rtl(),
        };
        let bidi = BidiInfo::new(content, Some(paragraph_level));
        if let Some(paragraph) = bidi.paragraphs.first() {
            let mut visual_lines = Vec::new();
            let mut line_start = 0;
            let line_ranges = content
                .char_indices()
                .filter_map(|(offset, character)| (character == '\u{2028}').then_some(offset))
                .chain(std::iter::once(content.len()))
                .map(|line_end| {
                    let range = line_start..line_end;
                    line_start = line_end
                        + content[line_end..]
                            .chars()
                            .next()
                            .filter(|character| *character == '\u{2028}')
                            .map_or(0, char::len_utf8);
                    range
                });
            for range in line_ranges {
                let logical = content[range.clone()].chars().collect::<Vec<_>>();
                let char_start = content[..range.start].chars().count();
                let char_end = char_start + logical.len();
                let paragraph_levels = bidi.reordered_levels_per_char(paragraph, range);
                let levels = &paragraph_levels[char_start..char_end];
                let visual = BidiInfo::reorder_visual(levels)
                    .into_iter()
                    .map(|index| {
                        let character = logical[index];
                        if levels[index].is_rtl() {
                            unicode_bidi_mirroring::get_mirrored(character).unwrap_or(character)
                        } else {
                            character
                        }
                    })
                    .collect::<String>();
                visual_lines.push(visual);
            }
            *content = visual_lines.join("\n");
            component.style.direction = w3cos_std::style::TextDirection::Ltr;
            mark_bidi_visual_order(&mut component.style);
        }
    }
    if let w3cos_std::ComponentKind::Text { content } = &mut component.kind
        && component.style.direction == w3cos_std::style::TextDirection::Rtl
        && matches!(
            component.style.unicode_bidi,
            w3cos_std::style::UnicodeBidi::BidiOverride
                | w3cos_std::style::UnicodeBidi::IsolateOverride
        )
    {
        *content = content
            .chars()
            .rev()
            .map(|character| unicode_bidi_mirroring::get_mirrored(character).unwrap_or(character))
            .collect();
        component.style.text_align = match (component.style.text_align, component.style.direction) {
            (w3cos_std::style::TextAlign::Start, w3cos_std::style::TextDirection::Rtl)
            | (w3cos_std::style::TextAlign::End, w3cos_std::style::TextDirection::Ltr) => {
                w3cos_std::style::TextAlign::Right
            }
            (w3cos_std::style::TextAlign::Start, w3cos_std::style::TextDirection::Ltr)
            | (w3cos_std::style::TextAlign::End, w3cos_std::style::TextDirection::Rtl) => {
                w3cos_std::style::TextAlign::Left
            }
            (align, _) => align,
        };
        // The leaf now contains visual-order text. Do not feed the consumed
        // override into the font backend a second time; normalizing these two
        // fields also allows otherwise identical adjacent inline fragments to
        // shape as one browser text run.
        component.style.direction = w3cos_std::style::TextDirection::Ltr;
        component.style.unicode_bidi = w3cos_std::style::UnicodeBidi::Normal;
        mark_bidi_visual_order(&mut component.style);
        return;
    }
    let reordered = reorder_explicit_bidi_children(component);
    if !reordered {
        for child in &mut component.children {
            reorder_explicit_bidi_inline_rows(child);
        }
    }
    coalesce_passive_inline_text_children(component);
    if !reordered {
        reorder_explicit_bidi_children(component);
    }
}

fn mark_bidi_visual_order(style: &mut w3cos_std::style::Style) {
    style
        .custom_properties
        .get_or_insert_with(std::collections::HashMap::new)
        .insert(
            "--w3cos-internal-bidi-visual-order".to_string(),
            "1".to_string(),
        );
}

fn is_bidi_control(character: char) -> bool {
    matches!(
        character,
        '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'
            | '\u{202b}'
            | '\u{202c}'
            | '\u{202d}'
            | '\u{202e}'
            | '\u{2066}'
            | '\u{2067}'
            | '\u{2068}'
            | '\u{2069}'
    )
}

fn is_regional_indicator(character: char) -> bool {
    ('\u{1f1e6}'..='\u{1f1ff}').contains(&character)
}

fn empty_inline_box_has_no_area(component: &w3cos_std::Component) -> bool {
    use w3cos_std::component::ComponentKind;
    use w3cos_std::style::{Dimension, Display};

    fn subtree_is_empty(component: &w3cos_std::Component) -> bool {
        if component.style.position != w3cos_std::style::Position::Static {
            return false;
        }
        match &component.kind {
            ComponentKind::Text { content } => content.chars().all(is_css_whitespace),
            ComponentKind::Row | ComponentKind::Box => {
                component.children.iter().all(subtree_is_empty)
            }
            _ => false,
        }
    }

    matches!(component.kind, ComponentKind::Row | ComponentKind::Box)
        && subtree_is_empty(component)
        && matches!(
            component.style.display,
            Display::Inline | Display::InlineFlex
        )
        && component.style.width == Dimension::Auto
        && component.style.height == Dimension::Auto
        && component.style.padding == w3cos_std::style::Edges::ZERO
        && component.style.margin == w3cos_std::style::Edges::ZERO
        && component.style.border_width == 0.0
        && [
            component.style.border_top_width,
            component.style.border_right_width,
            component.style.border_bottom_width,
            component.style.border_left_width,
        ]
        .into_iter()
        .all(|width| width.unwrap_or(0.0) == 0.0)
}

fn reorder_explicit_bidi_children(component: &mut w3cos_std::Component) -> bool {
    use unicode_bidi::BidiInfo;
    use w3cos_std::component::ComponentKind;
    use w3cos_std::style::{
        BoxSizing, Dimension, Display, FlexWrap, Spacing, TextDirection, UnicodeBidi, WhiteSpace,
    };

    let bidi_control_for =
        |style: &w3cos_std::style::Style| match (style.direction, style.unicode_bidi) {
            (TextDirection::Rtl, UnicodeBidi::BidiOverride | UnicodeBidi::IsolateOverride) => {
                Some(('\u{202e}', '\u{202c}'))
            }
            (TextDirection::Ltr, UnicodeBidi::BidiOverride | UnicodeBidi::IsolateOverride) => {
                Some(('\u{202d}', '\u{202c}'))
            }
            (TextDirection::Rtl, UnicodeBidi::Embed) => Some(('\u{202b}', '\u{202c}')),
            (TextDirection::Ltr, UnicodeBidi::Embed) => Some(('\u{202a}', '\u{202c}')),
            _ => None,
        };
    let style_bidi_control = bidi_control_for(&component.style);
    component
        .children
        .retain(|child| !empty_inline_box_has_no_area(child));
    let trailing_isolated_indicators = component.children.last().and_then(|last| {
        if let ComponentKind::Text { content } = &last.kind {
            return (!content.is_empty() && content.chars().all(is_regional_indicator))
                .then(|| last.clone());
        }
        if !matches!(last.kind, ComponentKind::Row | ComponentKind::Box) || last.children.len() != 1
        {
            return None;
        }
        let text = &last.children[0];
        let ComponentKind::Text { content } = &text.kind else {
            return None;
        };
        (!content.is_empty() && content.chars().all(is_regional_indicator)).then(|| text.clone())
    });
    let leading_has_rtl = component.children[..component.children.len().saturating_sub(1)]
        .iter()
        .any(|child| match &child.kind {
            ComponentKind::Text { content } => content.chars().any(|character| {
                matches!(
                    unicode_bidi::bidi_class(character),
                    unicode_bidi::BidiClass::R | unicode_bidi::BidiClass::AL
                )
            }),
            _ => false,
        });
    if style_bidi_control.is_none()
        && leading_has_rtl
        && let Some(mut indicators) = trailing_isolated_indicators
    {
        indicators.style.unicode_bidi = UnicodeBidi::Isolate;
        if let Some(last) = component.children.last_mut() {
            *last = indicators;
        }
        component
            .style
            .custom_properties
            .get_or_insert_with(std::collections::HashMap::new)
            .insert(
                "--w3cos-internal-bidi-font-runs".to_string(),
                "1".to_string(),
            );
        return true;
    }
    if component
        .style
        .custom_properties
        .as_ref()
        .is_some_and(|properties| {
            properties
                .get("--w3cos-internal-bidi-font-runs")
                .is_some_and(|value| value == "1")
        })
    {
        return true;
    }
    if style_bidi_control.is_none()
        && let [child] = component.children.as_slice()
        && let ComponentKind::Text { content } = &child.kind
        && let Some(split) = content.find(is_regional_indicator)
        && content[..split].chars().any(|character| {
            matches!(
                unicode_bidi::bidi_class(character),
                unicode_bidi::BidiClass::R | unicode_bidi::BidiClass::AL
            )
        })
        && content[split..].chars().all(is_regional_indicator)
        && content[split..].chars().count() % 2 == 0
    {
        let mut leading = child.clone();
        leading.kind = ComponentKind::Text {
            content: content[..split].to_string(),
        };
        let mut indicator = child.clone();
        indicator.kind = ComponentKind::Text {
            content: content[split..].to_string(),
        };
        indicator.style.unicode_bidi = UnicodeBidi::Isolate;
        component.children = vec![leading, indicator];
        component
            .style
            .custom_properties
            .get_or_insert_with(std::collections::HashMap::new)
            .insert(
                "--w3cos-internal-bidi-font-runs".to_string(),
                "1".to_string(),
            );
        return true;
    }
    if style_bidi_control.is_none() {
        // Keep a coalescible plain-text run in logical order. The font backend
        // applies Unicode Bidi to the resulting single run; converting it to
        // visual order here would make that backend reorder it a second time.
        let mut logical_run = component.clone();
        coalesce_passive_inline_text_children(&mut logical_run);
        if matches!(logical_run.children.as_slice(), [child] if matches!(child.kind, ComponentKind::Text { .. }))
        {
            component.children = logical_run.children;
            return false;
        }
    }
    if component.children.len() < 2 && style_bidi_control.is_none() {
        return false;
    }

    struct InlineUnit {
        content: String,
        component: w3cos_std::Component,
        wrapper: Option<(w3cos_std::Component, w3cos_std::style::Style)>,
        leading_ahem_wrapper: bool,
    }

    let mut units = Vec::with_capacity(component.children.len());
    for child in &component.children {
        let (content, source, wrapper, leading_ahem_wrapper) = match &child.kind {
            ComponentKind::Text { content } if child.children.is_empty() => {
                (content.clone(), child.clone(), None, false)
            }
            ComponentKind::Row | ComponentKind::Box
                if matches!(child.style.display, Display::Inline | Display::InlineFlex)
                    && child.children.len() == 1 =>
            {
                let text = &child.children[0];
                let ComponentKind::Text { content } = &text.kind else {
                    return false;
                };
                if !text.children.is_empty() {
                    return false;
                }
                let leading_ahem_wrapper =
                    units.iter().all(|unit: &InlineUnit| {
                        unit.content.chars().all(|character| {
                            is_css_whitespace(character) || is_bidi_control(character)
                        })
                    }) && child.style.font_family.as_deref().is_some_and(|families| {
                        families.split(',').any(|family| {
                            family
                                .trim()
                                .trim_matches(['"', '\''])
                                .eq_ignore_ascii_case("ahem")
                        })
                    });
                let mut source = child.clone();
                source.kind = ComponentKind::Text {
                    content: String::new(),
                };
                source.children.clear();
                (
                    content.clone(),
                    source,
                    Some((child.clone(), text.style.clone())),
                    leading_ahem_wrapper,
                )
            }
            _ => return false,
        };
        units.push(InlineUnit {
            content,
            component: source,
            wrapper,
            leading_ahem_wrapper,
        });
    }

    let mut text = String::new();
    let mut logical = Vec::new();
    if let Some((control, _)) = style_bidi_control {
        text.push(control);
        logical.push((control, 0));
    }
    for (unit_index, unit) in units.iter().enumerate() {
        let unit_bidi_control = bidi_control_for(&unit.component.style);
        if let Some((control, _)) = unit_bidi_control {
            text.push(control);
            logical.push((control, unit_index));
        }
        text.push_str(&unit.content);
        logical.extend(
            unit.content
                .chars()
                .map(|character| (character, unit_index)),
        );
        if let Some((_, control)) = unit_bidi_control {
            text.push(control);
            logical.push((control, unit_index));
        }
    }
    if let Some((_, control)) = style_bidi_control {
        text.push(control);
        logical.push((control, units.len().saturating_sub(1)));
    }
    let paragraph_direction = if component.style.direction == TextDirection::Rtl
        || units
            .iter()
            .any(|unit| unit.component.style.direction == TextDirection::Rtl)
    {
        TextDirection::Rtl
    } else {
        TextDirection::Ltr
    };
    let has_bidi_content = paragraph_direction == TextDirection::Rtl
        || logical.iter().any(|(character, _)| {
            is_bidi_control(*character)
                || matches!(
                    unicode_bidi::bidi_class(*character),
                    unicode_bidi::BidiClass::R
                        | unicode_bidi::BidiClass::AL
                        | unicode_bidi::BidiClass::AN
                )
        });
    if !has_bidi_content {
        return false;
    }

    let paragraph_level = match paragraph_direction {
        TextDirection::Ltr => unicode_bidi::Level::ltr(),
        TextDirection::Rtl => unicode_bidi::Level::rtl(),
    };
    let bidi = BidiInfo::new(&text, Some(paragraph_level));
    let Some(paragraph) = bidi.paragraphs.first() else {
        return false;
    };
    let levels = bidi.reordered_levels_per_char(paragraph, paragraph.range.clone());
    if levels.len() != logical.len() {
        return false;
    }
    let is_control = is_bidi_control;

    let wrapping_width = if component.style.flex_wrap != FlexWrap::NoWrap
        && !matches!(
            component.style.white_space,
            WhiteSpace::NoWrap | WhiteSpace::Pre
        ) {
        let width = match component.style.width {
            Dimension::Px(width) => Some(width),
            Dimension::Em(width) => Some(width * component.style.font_size),
            Dimension::Rem(width) => Some(width * 16.0),
            _ => None,
        };
        width.map(|mut width| {
            if component.style.box_sizing == BoxSizing::BorderBox {
                let padding = component.style.padding_lengths();
                width -= padding.left
                    + padding.right
                    + component
                        .style
                        .border_left_width
                        .unwrap_or(component.style.border_width)
                    + component
                        .style
                        .border_right_width
                        .unwrap_or(component.style.border_width);
            }
            width.max(0.0)
        })
    } else {
        None
    };
    let estimated_line_ranges = |width: f32| {
        let mut advances = logical
            .iter()
            .map(|(character, unit_index)| {
                if is_control(*character) {
                    return 0.0;
                }
                let style = &units[*unit_index].component.style;
                let ahem = style.font_family.as_deref().is_some_and(|families| {
                    families.split(',').any(|family| {
                        family
                            .trim()
                            .trim_matches(['"', '\''])
                            .eq_ignore_ascii_case("ahem")
                    })
                });
                let ratio = if ahem {
                    1.0
                } else if character.is_ascii_whitespace() {
                    0.3335
                } else if character.is_ascii_uppercase() {
                    0.67
                } else if character.is_ascii_lowercase() || character.is_ascii_digit() {
                    0.56
                } else {
                    0.6
                };
                style.font_size * ratio
            })
            .collect::<Vec<_>>();
        for (unit_index, unit) in units.iter().enumerate() {
            let positions = logical
                .iter()
                .enumerate()
                .filter_map(|(index, (character, source))| {
                    (*source == unit_index && !is_control(*character)).then_some(index)
                })
                .collect::<Vec<_>>();
            let (Some(first), Some(last)) = (positions.first(), positions.last()) else {
                continue;
            };
            let padding = unit.component.style.padding_lengths();
            let margin = unit.component.style.margin_lengths();
            advances[*first] += padding.left
                + margin.left
                + unit
                    .component
                    .style
                    .border_left_width
                    .unwrap_or(unit.component.style.border_width);
            advances[*last] += padding.right
                + margin.right
                + unit
                    .component
                    .style
                    .border_right_width
                    .unwrap_or(unit.component.style.border_width);
        }

        let words = logical.iter().enumerate().fold(
            Vec::<std::ops::Range<usize>>::new(),
            |mut words, (index, (ch, _))| {
                if ch.is_whitespace() {
                    return words;
                }
                if let Some(last) = words.last_mut()
                    && last.end == index
                {
                    last.end += 1;
                } else {
                    words.push(index..index + 1);
                }
                words
            },
        );
        if words.is_empty() {
            return vec![0..logical.len()];
        }
        let mut ranges = Vec::new();
        let mut line_start = words[0].start;
        let mut line_end = words[0].end;
        let mut line_width = advances[words[0].clone()].iter().sum::<f32>();
        for word in words.into_iter().skip(1) {
            let candidate_width = advances[line_end..word.end].iter().sum::<f32>();
            if line_width + candidate_width > width {
                ranges.push(line_start..line_end);
                line_start = word.start;
                line_width = advances[word.clone()].iter().sum::<f32>();
            } else {
                line_width += candidate_width;
            }
            line_end = word.end;
        }
        ranges.push(line_start..line_end);
        ranges
    };
    let line_ranges = wrapping_width
        .filter(|width| *width > 0.0)
        .map(estimated_line_ranges)
        .filter(|ranges| ranges.len() > 1)
        .unwrap_or_else(|| vec![0..logical.len()]);

    let mut fragments: Vec<(usize, usize, String)> = Vec::new();
    for (line_index, range) in line_ranges.iter().enumerate() {
        let visual_order = BidiInfo::reorder_visual(&levels[range.clone()]);
        for relative_index in visual_order {
            let logical_index = range.start + relative_index;
            let (mut character, unit_index) = logical[logical_index];
            if is_control(character) {
                continue;
            }
            if levels[logical_index].is_rtl() {
                character = unicode_bidi_mirroring::get_mirrored(character).unwrap_or(character);
            }
            if let Some((last_line, last_unit, content)) = fragments.last_mut()
                && *last_line == line_index
                && *last_unit == unit_index
            {
                content.push(character);
            } else {
                fragments.push((line_index, unit_index, character.to_string()));
            }
        }
    }

    let mut fragment_positions = vec![Vec::new(); units.len()];
    for (position, (_, unit_index, _)) in fragments.iter().enumerate() {
        fragment_positions[*unit_index].push(position);
    }
    struct VisualFragment {
        line_index: usize,
        unit_index: usize,
        first: bool,
        last: bool,
        component: w3cos_std::Component,
    }

    let mut visual_fragments = fragments
        .into_iter()
        .enumerate()
        .map(|(position, (line_index, unit_index, content))| {
            let positions = &fragment_positions[unit_index];
            let first = positions.first() == Some(&position);
            let last = positions.last() == Some(&position);
            let mut fragment = units[unit_index].component.clone();
            fragment.kind = ComponentKind::Text { content };
            fragment.style.direction = TextDirection::Ltr;
            fragment.style.unicode_bidi = UnicodeBidi::Normal;
            mark_bidi_visual_order(&mut fragment.style);
            let has_left_edge = fragment
                .style
                .border_left_width
                .unwrap_or(fragment.style.border_width)
                > 0.0
                || fragment.style.padding.left != Spacing::Px(0.0)
                || fragment.style.margin.left != Spacing::Px(0.0);
            let has_right_edge = fragment
                .style
                .border_right_width
                .unwrap_or(fragment.style.border_width)
                > 0.0
                || fragment.style.padding.right != Spacing::Px(0.0)
                || fragment.style.margin.right != Spacing::Px(0.0);
            if !first && has_left_edge {
                fragment.style.border_left_width = Some(0.0);
                fragment.style.padding.left = Spacing::Px(0.0);
                fragment.style.margin.left = Spacing::Px(0.0);
            }
            if !last && has_right_edge {
                fragment.style.border_right_width = Some(0.0);
                fragment.style.padding.right = Spacing::Px(0.0);
                fragment.style.margin.right = Spacing::Px(0.0);
            }
            VisualFragment {
                line_index,
                unit_index,
                first,
                last,
                component: fragment,
            }
        })
        .collect::<Vec<_>>();

    // Inline backgrounds belong below the shaped text of the whole bidi run.
    // A later background fragment must not erase glyph overhang from its
    // preceding sibling.
    if visual_fragments
        .iter()
        .any(|fragment| fragment.component.style.background.a > 0)
    {
        for fragment in &mut visual_fragments {
            if fragment.component.style.background.a == 0 {
                fragment.component.style.z_index = fragment.component.style.z_index.max(1);
            }
        }
    }

    let anonymous_space = units
        .iter()
        .find(|unit| principal_box_can_merge_generated_inline_text(&unit.component.style))
        .map(|unit| {
            let mut space = unit.component.clone();
            space.kind = ComponentKind::Text {
                content: " ".to_string(),
            };
            space.children.clear();
            space.on_click = w3cos_std::EventAction::None;
            // Bidi controls have already been consumed into visual-order
            // fragments. Keep synthesized boundary whitespace in that same
            // normalized direction so otherwise identical text can be shaped
            // as one browser run instead of introducing artificial kerning
            // boundaries at each embedded inline edge.
            space.style.direction = TextDirection::Ltr;
            space.style.unicode_bidi = UnicodeBidi::Normal;
            mark_bidi_visual_order(&mut space.style);
            space
        });
    let freezes_estimated_line_breaks = units.iter().all(|unit| {
        unit.component
            .style
            .font_family
            .as_deref()
            .is_some_and(|families| {
                families.split(',').any(|family| {
                    family
                        .trim()
                        .trim_matches(['"', '\''])
                        .eq_ignore_ascii_case("ahem")
                })
            })
    });
    let mut edge_whitespace = Vec::with_capacity(visual_fragments.len());
    for fragment in &mut visual_fragments {
        let ComponentKind::Text { content } = &mut fragment.component.kind else {
            edge_whitespace.push((false, false));
            continue;
        };
        let leading = content.chars().next().is_some_and(is_css_whitespace);
        let trailing = content.chars().next_back().is_some_and(is_css_whitespace);
        if leading || trailing {
            *content = content.trim_matches(is_css_whitespace).to_string();
        }
        edge_whitespace.push((leading, trailing));
    }

    let mut normalized: Vec<w3cos_std::Component> = Vec::with_capacity(visual_fragments.len() * 2);
    let mut previous_line = None;
    for (index, mut fragment) in visual_fragments.into_iter().enumerate() {
        let starts_new_line = previous_line.is_some_and(|previous| previous != fragment.line_index);
        if starts_new_line && freezes_estimated_line_breaks {
            let mut break_style = anonymous_space
                .as_ref()
                .map(|space| space.style.clone())
                .unwrap_or_else(w3cos_std::style::Style::default);
            break_style.display = Display::Inline;
            break_style.width = Dimension::Px(0.0);
            break_style.height = Dimension::Px(break_style.font_size * break_style.line_height);
            break_style.min_width = Dimension::Auto;
            break_style.min_height = Dimension::Auto;
            break_style.max_width = Dimension::Auto;
            break_style.max_height = Dimension::Auto;
            break_style.margin = w3cos_std::style::Edges::ZERO;
            break_style.padding = w3cos_std::style::Edges::ZERO;
            break_style.border_width = 0.0;
            break_style.border_top_width = None;
            break_style.border_right_width = None;
            break_style.border_bottom_width = None;
            break_style.border_left_width = None;
            break_style.background = w3cos_std::Color::TRANSPARENT;
            break_style.background_image = None;
            normalized.push(w3cos_std::Component::text("\u{2028}", break_style));
        }
        if index > 0 && !starts_new_line {
            let boundary_has_space = edge_whitespace[index - 1].1 || edge_whitespace[index].0;
            let left_owns_space = normalized.last().is_some_and(|previous| {
                matches!(
                    &previous.kind,
                    ComponentKind::Text { content }
                        if content.chars().next_back().is_some_and(is_css_whitespace)
                )
            });
            let right_owns_space = matches!(
                &fragment.component.kind,
                ComponentKind::Text { content }
                    if content.chars().next().is_some_and(is_css_whitespace)
            );
            let continuation_wrapper_owns_space = boundary_has_space
                && !fragment.first
                && units[fragment.unit_index].leading_ahem_wrapper;
            if continuation_wrapper_owns_space {
                if let ComponentKind::Text { content } = &mut fragment.component.kind {
                    content.insert(0, '\u{00a0}');
                }
            } else if boundary_has_space
                && !left_owns_space
                && !right_owns_space
                && let Some(space) = &anonymous_space
            {
                normalized.push(space.clone());
            }
        }
        let preserve_wrapper = units[fragment.unit_index].wrapper.is_some()
            && (units[fragment.unit_index].leading_ahem_wrapper
                || (fragment.line_index > 0 && fragment.last));
        if preserve_wrapper
            && let Some((wrapper_source, inner_style)) = &units[fragment.unit_index].wrapper
            && let ComponentKind::Text { content } = &fragment.component.kind
        {
            let mut wrapper = wrapper_source.clone();
            wrapper.style = fragment.component.style.clone();
            wrapper.children = vec![w3cos_std::Component::text(
                content.clone(),
                inner_style.clone(),
            )];
            fragment.component = wrapper;
        }
        if !matches!(&fragment.component.kind, ComponentKind::Text { content } if content.is_empty())
        {
            normalized.push(fragment.component);
        }
        previous_line = Some(fragment.line_index);
    }
    component.children = normalized;
    component.style.justify_content = match (component.style.text_align, paragraph_direction) {
        (w3cos_std::style::TextAlign::Start, TextDirection::Rtl)
        | (w3cos_std::style::TextAlign::End, TextDirection::Ltr) => {
            w3cos_std::style::JustifyContent::FlexEnd
        }
        (w3cos_std::style::TextAlign::Center, _) => w3cos_std::style::JustifyContent::Center,
        _ => component.style.justify_content,
    };
    true
}

fn plain_anonymous_inline_table_text(
    component: &w3cos_std::Component,
) -> Option<w3cos_std::Component> {
    use w3cos_std::component::ComponentKind;
    use w3cos_std::style::{Dimension, Display, Edges, Position};

    if component
        .style
        .custom_properties
        .as_ref()
        .and_then(|properties| properties.get("--w3cos-internal-split-inline-block"))
        .is_some_and(|value| value == "1")
    {
        return None;
    }

    fn passive_host(action: &w3cos_std::EventAction) -> bool {
        matches!(
            action,
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
        )
    }
    fn collect(
        component: &w3cos_std::Component,
        content: &mut String,
        text_style: &mut Option<w3cos_std::style::Style>,
    ) -> bool {
        if component
            .style
            .custom_properties
            .as_ref()
            .and_then(|properties| properties.get("--w3cos-internal-split-inline-block"))
            .is_some_and(|value| value == "1")
        {
            return false;
        }
        if component.style.display == Display::None {
            return true;
        }
        if let ComponentKind::Text { content: text } = &component.kind {
            if !component.children.is_empty() {
                return false;
            }
            if text.is_empty() {
                return true;
            }
            if let Some(style) = text_style.as_ref()
                && !equivalent_text_paint_style(style, &component.style)
            {
                let mut normalized_style = style.clone();
                let mut normalized_component_style = component.style.clone();
                normalized_style.white_space = w3cos_std::style::WhiteSpace::Normal;
                normalized_component_style.white_space = w3cos_std::style::WhiteSpace::Normal;
                if !equivalent_text_paint_style(&normalized_style, &normalized_component_style) {
                    return false;
                }
            }
            text_style.get_or_insert_with(|| component.style.clone());
            content.push_str(text);
            return true;
        }
        if !matches!(component.kind, ComponentKind::Row | ComponentKind::Box)
            || !passive_host(&component.on_click)
            || component.style.position != Position::Static
            || component.style.background.a != 0
            || component.style.background_image.is_some()
            || component.style.border_width != 0.0
            || component
                .style
                .border_top_width
                .is_some_and(|width| width != 0.0)
            || component
                .style
                .border_right_width
                .is_some_and(|width| width != 0.0)
            || component
                .style
                .border_bottom_width
                .is_some_and(|width| width != 0.0)
            || component
                .style
                .border_left_width
                .is_some_and(|width| width != 0.0)
            || component.style.padding != Edges::ZERO
            || component.style.margin != Edges::ZERO
            || component.style.width != Dimension::Auto
            || component.style.height != Dimension::Auto
            || component.style.gap != 0.0
            || component.style.border_spacing_x != 0.0
            || component.style.border_spacing_y != 0.0
        {
            return false;
        }
        component
            .children
            .iter()
            .all(|child| collect(child, content, text_style))
    }

    if !matches!(
        component.style.display,
        Display::Table | Display::InlineTable
    ) || component
        .style
        .custom_properties
        .as_ref()
        .and_then(|properties| properties.get("--w3cos-internal-anonymous-table"))
        .is_none_or(|value| value != "1")
        || component.children.len() != 1
        || component.children[0].style.display != Display::TableRow
        || component.children[0].children.is_empty()
        || component.children[0]
            .children
            .iter()
            .any(|child| child.style.display != Display::TableCell)
    {
        return None;
    }
    let mut content = String::new();
    let mut style = None;
    if !collect(component, &mut content, &mut style) {
        return None;
    }
    let mut style = style?;
    if collapse_css_whitespace(&content, true, true) != content {
        return None;
    }
    style.display = Display::Inline;
    style.white_space = w3cos_std::style::WhiteSpace::Normal;
    Some(w3cos_std::Component::text(content, style))
}

fn coalesce_passive_inline_text_children(component: &mut w3cos_std::Component) {
    use w3cos_std::component::ComponentKind;

    let passive_host = |action: &w3cos_std::EventAction| {
        matches!(
            action,
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
        )
    };
    let children = std::mem::take(&mut component.children);
    let child_displays = children
        .iter()
        .map(|child| child.style.display)
        .collect::<Vec<_>>();
    let mut coalesced: Vec<w3cos_std::Component> = Vec::with_capacity(children.len());
    for (index, mut fragment) in children.into_iter().enumerate() {
        if fragment.style.display == w3cos_std::style::Display::None {
            continue;
        }
        if empty_inline_box_has_no_area(&fragment) {
            continue;
        }
        let anonymous_block_table = fragment.style.display == w3cos_std::style::Display::Table;
        if let Some(mut text) = plain_anonymous_inline_table_text(&fragment) {
            if anonymous_block_table {
                let has_previous_inline = coalesced.last().is_some_and(|previous| {
                    matches!(
                        previous.style.display,
                        w3cos_std::style::Display::Inline
                            | w3cos_std::style::Display::InlineBlock
                            | w3cos_std::style::Display::InlineFlex
                            | w3cos_std::style::Display::InlineTable
                    )
                });
                let has_following_inline = child_displays[index + 1..]
                    .iter()
                    .find(|display| **display != w3cos_std::style::Display::None)
                    .is_some_and(|display| {
                        matches!(
                            display,
                            w3cos_std::style::Display::Inline
                                | w3cos_std::style::Display::InlineBlock
                                | w3cos_std::style::Display::InlineFlex
                                | w3cos_std::style::Display::InlineTable
                        )
                    });
                if let ComponentKind::Text { content } = &mut text.kind {
                    if has_previous_inline {
                        content.insert_str(0, "\u{2028} ");
                    }
                    if has_following_inline {
                        content.push('\u{2028}');
                    }
                }
            }
            fragment = text;
        }
        loop {
            let painted_wrapper_matches_text = fragment.children.first().is_some_and(|child| {
                equivalent_text_style(&fragment.style, &child.style)
                    && painted_inline_text_box_can_merge(&fragment.style)
            });
            if !matches!(&fragment.kind, ComponentKind::Row | ComponentKind::Box)
                || !matches!(
                    fragment.style.display,
                    w3cos_std::style::Display::Inline | w3cos_std::style::Display::InlineFlex
                )
                || fragment.children.len() != 1
                || !passive_host(&fragment.on_click)
                || !(principal_box_can_merge_generated_inline_text(&fragment.style)
                    || painted_wrapper_matches_text)
                || !matches!(&fragment.children[0].kind, ComponentKind::Text { .. })
            {
                break;
            }
            fragment = fragment.children.remove(0);
        }
        let merged = coalesced.last_mut().is_some_and(|previous| {
            let is_inline_text = |display| {
                matches!(
                    display,
                    w3cos_std::style::Display::Inline
                        | w3cos_std::style::Display::InlineBlock
                        | w3cos_std::style::Display::InlineFlex
                        | w3cos_std::style::Display::InlineTable
                )
            };
            let transparent_text_styles_match =
                principal_box_can_merge_generated_inline_text(&previous.style)
                    && principal_box_can_merge_generated_inline_text(&fragment.style)
                    && previous.style.position == w3cos_std::style::Position::Static
                    && fragment.style.position == w3cos_std::style::Position::Static
                    && previous.style.float == w3cos_std::style::Float::None
                    && fragment.style.float == w3cos_std::style::Float::None
                    && equivalent_text_paint_style(&previous.style, &fragment.style);
            if !is_inline_text(previous.style.display)
                || !is_inline_text(fragment.style.display)
                || (!equivalent_text_style(&previous.style, &fragment.style)
                    && !transparent_text_styles_match)
                || !passive_host(&previous.on_click)
                || !passive_host(&fragment.on_click)
                || !(principal_box_can_merge_generated_inline_text(&previous.style)
                    || painted_inline_text_box_can_merge(&previous.style))
            {
                return false;
            }
            let ComponentKind::Text {
                content: previous_content,
            } = &mut previous.kind
            else {
                return false;
            };
            let ComponentKind::Text { content } = &mut fragment.kind else {
                return false;
            };
            previous_content.push_str(content);
            previous.on_click = w3cos_std::EventAction::None;
            true
        });
        if !merged {
            coalesced.push(fragment);
        }
    }
    component.children = coalesced;
}

fn relative_border_width_px(value: &str, style: &w3cos_std::style::Style) -> Option<f32> {
    split_css_tokens(value).into_iter().find_map(|token| {
        let token = token.to_ascii_lowercase();
        if let Some(number) = token.strip_suffix("rem") {
            return number.trim().parse::<f32>().ok().map(|value| value * 16.0);
        }
        if let Some(number) = token.strip_suffix("em") {
            return number
                .trim()
                .parse::<f32>()
                .ok()
                .map(|value| value * style.font_size);
        }
        if let Some(number) = token.strip_suffix("ex") {
            return number
                .trim()
                .parse::<f32>()
                .ok()
                .map(|value| value * css_ex_size(style));
        }
        None
    })
}

fn normalize_css_table_internal_used_style(style: &mut w3cos_std::style::Style) {
    use w3cos_std::style::Display;

    if !matches!(style.display, Display::Table | Display::InlineTable) {
        style.table_layout_fixed = false;
    }
    if style.display != Display::TableCell {
        // `empty-cells` is inherited as a computed value but affects only a
        // table-cell's used paint style. Keeping it on unrelated component
        // boxes would split otherwise identical inline runs.
        style.empty_cells_hide = false;
    }
    if style.display != Display::TableCaption {
        // Likewise, caption-side participates in inheritance through the DOM
        // computed style, while only caption boxes need it after lowering.
        style.caption_side_bottom = false;
    }

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
    if ignores_padding_and_border && !style.border_collapse {
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

#[cfg(test)]
mod collapsed_table_border_tests {
    use super::normalize_css_table_internal_used_style;
    use w3cos_std::style::{Display, Style};

    #[test]
    fn collapsed_table_row_group_retains_its_border() {
        let mut style = Style {
            display: Display::TableRowGroup,
            border_collapse: true,
            border_width: 4.0,
            ..Style::default()
        };
        normalize_css_table_internal_used_style(&mut style);
        assert_eq!(style.border_width, 4.0);
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
    row_style.flex_direction = match row_style.direction {
        w3cos_std::style::TextDirection::Ltr => w3cos_std::style::FlexDirection::Row,
        w3cos_std::style::TextDirection::Rtl => w3cos_std::style::FlexDirection::RowReverse,
    };
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

    fn group_consecutive_left_floats(
        components: Vec<w3cos_std::Component>,
        formatting_context_style: &w3cos_std::style::Style,
    ) -> Vec<w3cos_std::Component> {
        fn passive_host(action: &w3cos_std::EventAction) -> bool {
            matches!(
                action,
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
            )
        }
        fn collect_passive_text(
            component: &w3cos_std::Component,
            content: &mut String,
            text_style: &mut Option<w3cos_std::style::Style>,
        ) -> bool {
            if !passive_host(&component.on_click) {
                return false;
            }
            let mut box_style = component.style.clone();
            box_style.float = w3cos_std::style::Float::None;
            if !principal_box_can_merge_generated_inline_text(&box_style) {
                return false;
            }
            if let w3cos_std::ComponentKind::Text { content: text } = &component.kind {
                if component.children.is_empty()
                    && text_style
                        .as_ref()
                        .is_none_or(|style| equivalent_text_style(style, &component.style))
                {
                    text_style.get_or_insert_with(|| component.style.clone());
                    content.push_str(text);
                    return true;
                }
                return false;
            }
            matches!(
                component.kind,
                w3cos_std::ComponentKind::Row | w3cos_std::ComponentKind::Box
            ) && component
                .children
                .iter()
                .all(|child| collect_passive_text(child, content, text_style))
        }
        fn coalesced_float_text(
            left_floats: &[w3cos_std::Component],
        ) -> Option<w3cos_std::Component> {
            let mut content = String::new();
            let mut style = None;
            if !left_floats
                .iter()
                .all(|float| collect_passive_text(float, &mut content, &mut style))
            {
                return None;
            }
            let mut style = style?;
            style.display = w3cos_std::style::Display::Inline;
            style.float = w3cos_std::style::Float::None;
            style.flex_shrink = 0.0;
            Some(w3cos_std::Component::text(content, style))
        }
        let mut grouped = Vec::with_capacity(components.len());
        let mut left_floats = Vec::new();
        let flush = |left_floats: &mut Vec<w3cos_std::Component>,
                     grouped: &mut Vec<w3cos_std::Component>,
                     force_formatting_context: bool| {
            if left_floats.len() < 2 && !force_formatting_context {
                grouped.append(left_floats);
                return;
            }
            if let Some(text) = coalesced_float_text(left_floats) {
                left_floats.clear();
                grouped.push(text);
                return;
            }
            for float in left_floats.iter_mut() {
                float.style.flex_shrink = 0.0;
            }
            let mut row_style = w3cos_std::style::Style::default();
            row_style.display = w3cos_std::style::Display::Flex;
            row_style.flex_direction = w3cos_std::style::FlexDirection::Row;
            row_style.flex_wrap = w3cos_std::style::FlexWrap::Wrap;
            row_style.align_items = w3cos_std::style::AlignItems::Baseline;
            row_style.width = w3cos_std::style::Dimension::Percent(100.0);
            row_style.font_size = formatting_context_style.font_size;
            row_style.font_family = formatting_context_style.font_family.clone();
            row_style.line_height = formatting_context_style.line_height;
            grouped.push(w3cos_std::Component::row(
                row_style,
                std::mem::take(left_floats),
            ));
        };
        let mut force_formatting_context = false;
        for component in components {
            if component.style.float == w3cos_std::style::Float::Left {
                if matches!(
                    component.style.clear,
                    w3cos_std::style::Clear::Left | w3cos_std::style::Clear::Both
                ) {
                    flush(
                        &mut left_floats,
                        &mut grouped,
                        force_formatting_context,
                    );
                    force_formatting_context = true;
                }
                left_floats.push(component);
            } else {
                flush(
                    &mut left_floats,
                    &mut grouped,
                    force_formatting_context,
                );
                force_formatting_context = false;
                grouped.push(component);
            }
        }
        flush(
            &mut left_floats,
            &mut grouped,
            force_formatting_context,
        );
        grouped
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
                    strut_style.word_spacing = component.style.word_spacing;
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
                let extract_child =
                    child.style.float != w3cos_std::style::Float::Right || has_prior_in_flow;
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
            let has_prior_in_flow = in_flow.iter().any(|component| {
                component.style.float == w3cos_std::style::Float::None
                    && contributes_in_flow_content(component)
            });
            if direct_float == w3cos_std::style::Float::Left && has_prior_in_flow {
                // A later float is shifted to the inline-start edge of the
                // current line; earlier inline content flows beside it. Keep
                // its authored top margin so the float's margin edge, rather
                // than its border edge, is constrained by the prior line.
                child
                    .style
                    .custom_properties
                    .get_or_insert_with(Default::default)
                    .insert(
                        "--w3cos-internal-left-float-after-inline".to_string(),
                        "1".to_string(),
                    );
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
    group_consecutive_left_floats(left, formatting_context_style)
}

fn anonymous_table_wrapper(
    parent_style: &w3cos_std::style::Style,
    children: Vec<w3cos_std::Component>,
) -> w3cos_std::Component {
    let table_height = children
        .iter()
        .filter_map(|child| specified_table_cell_height(&child.style))
        .fold(0.0_f32, f32::max);
    let mut top_captions = Vec::new();
    let mut grid_children = Vec::new();
    let mut bottom_captions = Vec::new();
    for child in children {
        if child.style.display == w3cos_std::style::Display::TableCaption {
            if child.style.caption_side_bottom {
                bottom_captions.push(child);
            } else {
                top_captions.push(child);
            }
        } else {
            grid_children.push(child);
        }
    }
    grid_children.sort_by_key(|child| match child.style.display {
        w3cos_std::style::Display::TableColumnGroup | w3cos_std::style::Display::TableColumn => 0,
        w3cos_std::style::Display::TableHeaderGroup => 1,
        w3cos_std::style::Display::TableFooterGroup => 3,
        _ => 2,
    });
    let anonymous_display = if matches!(
        parent_style.display,
        w3cos_std::style::Display::Inline
            | w3cos_std::style::Display::InlineBlock
            | w3cos_std::style::Display::InlineFlex
    ) {
        w3cos_std::style::Display::InlineTable
    } else {
        w3cos_std::style::Display::Table
    };
    let mut table_style = anonymous_table_style(anonymous_display, parent_style);
    table_style
        .custom_properties
        .get_or_insert_with(Default::default)
        .insert(
            "--w3cos-internal-anonymous-table".to_string(),
            "1".to_string(),
        );
    if table_height > 0.0 {
        table_style.height = w3cos_std::style::Dimension::Px(table_height);
    }
    if !grid_children.is_empty() {
        if grid_children
            .iter()
            .all(|child| child.style.display == w3cos_std::style::Display::TableCell)
        {
            coalesce_plain_anonymous_table_cell_text(&mut grid_children);
            top_captions.push(table_row_from_misparented_children(
                parent_style,
                grid_children,
                (table_height > 0.0).then_some(table_height),
            ));
        } else {
            top_captions.extend(fixup_css_table_children(&table_style, grid_children));
        }
    }
    top_captions.extend(bottom_captions);
    w3cos_std::Component::boxed(table_style, top_captions)
}

fn coalesce_plain_anonymous_table_cell_text(cells: &mut [w3cos_std::Component]) {
    use w3cos_std::style::{Dimension, Display, Edges, WhiteSpace};

    let direct_text = |cell: &w3cos_std::Component| {
        cell.children.is_empty() && matches!(cell.kind, w3cos_std::ComponentKind::Text { .. })
    };
    let child_text = |cell: &w3cos_std::Component| {
        cell.children.len() == 1
            && cell.children[0].children.is_empty()
            && matches!(cell.children[0].kind, w3cos_std::ComponentKind::Text { .. })
    };
    if cells.len() < 2
        || cells.iter().any(|cell| {
            cell.style.display != Display::TableCell
                || cell.style.background.a != 0
                || cell.style.background_image.is_some()
                || cell.style.border_width != 0.0
                || cell
                    .style
                    .border_top_width
                    .is_some_and(|width| width != 0.0)
                || cell
                    .style
                    .border_right_width
                    .is_some_and(|width| width != 0.0)
                || cell
                    .style
                    .border_bottom_width
                    .is_some_and(|width| width != 0.0)
                || cell
                    .style
                    .border_left_width
                    .is_some_and(|width| width != 0.0)
                || cell.style.padding != Edges::ZERO
                || cell.style.margin != Edges::ZERO
                || cell.style.width != Dimension::Auto
                || cell.style.height != Dimension::Auto
                || (!direct_text(cell) && !child_text(cell))
        })
        || cells
            .iter()
            .skip(1)
            .any(|cell| direct_text(cell) != direct_text(&cells[0]))
    {
        return;
    }

    let normalized_text_style = |cell: &w3cos_std::Component| {
        let mut style = if direct_text(cell) {
            cell.style.clone()
        } else {
            cell.children[0].style.clone()
        };
        style.white_space = WhiteSpace::Normal;
        style
    };
    let first_style = normalized_text_style(&cells[0]);
    if cells
        .iter()
        .skip(1)
        .any(|cell| normalized_text_style(cell) != first_style)
    {
        return;
    }

    let content = cells
        .iter()
        .filter_map(|cell| {
            match if direct_text(cell) {
                &cell.kind
            } else {
                &cell.children[0].kind
            } {
                w3cos_std::ComponentKind::Text { content } => Some(content.as_str()),
                _ => None,
            }
        })
        .collect::<String>();
    if direct_text(&cells[0]) {
        cells[0].kind = w3cos_std::ComponentKind::Text { content };
        cells[0].style.white_space = WhiteSpace::Normal;
        for cell in &mut cells[1..] {
            cell.kind = w3cos_std::ComponentKind::Text {
                content: String::new(),
            };
        }
    } else {
        cells[0].children[0].kind = w3cos_std::ComponentKind::Text { content };
        cells[0].children[0].style.white_space = WhiteSpace::Normal;
        for cell in &mut cells[1..] {
            cell.children[0].kind = w3cos_std::ComponentKind::Text {
                content: String::new(),
            };
        }
    }
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
            let mut pending_whitespace = None;

            for child in children {
                let starts_replaced_cell_run = child
                    .style
                    .custom_properties
                    .as_ref()
                    .and_then(|properties| properties.get("--w3cos-internal-replaced-table-cell"))
                    .is_some_and(|value| value == "1")
                    && anonymous_run_started
                    && !anonymous_children.is_empty()
                    && anonymous_children
                        .iter()
                        .all(collapsible_generated_whitespace);
                if starts_replaced_cell_run {
                    cells.push(anonymous_table_cell_from_children(parent_style, Vec::new()));
                    pending_whitespace = None;
                }
                if child.style.display == Display::TableCell {
                    if anonymous_run_started {
                        let follows_replaced_cell_run =
                            anonymous_children.iter().any(|component| {
                                component
                                    .style
                                    .custom_properties
                                    .as_ref()
                                    .and_then(|properties| {
                                        properties.get("--w3cos-internal-replaced-table-cell")
                                    })
                                    .is_some_and(|value| value == "1")
                            });
                        cells.push(anonymous_table_cell_from_children(
                            parent_style,
                            std::mem::take(&mut anonymous_children),
                        ));
                        if follows_replaced_cell_run {
                            cells
                                .push(anonymous_table_cell_from_children(parent_style, Vec::new()));
                        }
                        anonymous_run_started = false;
                        pending_whitespace = None;
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
                if collapsible_generated_whitespace(&child) {
                    if matches!(
                        parent_style.white_space,
                        w3cos_std::style::WhiteSpace::Pre | w3cos_std::style::WhiteSpace::PreWrap
                    ) {
                        anonymous_children.push(child);
                        continue;
                    }
                    if !anonymous_children.is_empty() {
                        pending_whitespace = Some(child);
                    }
                    continue;
                }
                if let Some(mut whitespace) = pending_whitespace.take() {
                    whitespace.kind = w3cos_std::ComponentKind::Text {
                        content: " ".to_string(),
                    };
                    anonymous_children.push(whitespace);
                }
                anonymous_children.push(child);
            }

            if anonymous_run_started {
                cells.push(anonymous_table_cell_from_children(
                    parent_style,
                    anonymous_children,
                ));
            }
            cells
        }
        Display::Table | Display::InlineTable => {
            let containing_height = specified_table_cell_height(parent_style);
            let mut fixed = Vec::with_capacity(children.len());
            let mut cells = Vec::new();
            let flush_cells =
                |fixed: &mut Vec<w3cos_std::Component>, cells: &mut Vec<w3cos_std::Component>| {
                    if !cells.is_empty() {
                        fixed.push(anonymous_table_row(
                            parent_style,
                            std::mem::take(cells),
                            containing_height,
                        ));
                    }
                };
            for child in children {
                if child.style.display == Display::TableCell {
                    cells.push(child);
                } else {
                    flush_cells(&mut fixed, &mut cells);
                    fixed.push(child);
                }
            }
            flush_cells(&mut fixed, &mut cells);
            fixed
        }
        Display::TableRowGroup | Display::TableHeaderGroup | Display::TableFooterGroup => {
            let mut fixed = Vec::with_capacity(children.len());
            let mut improper = Vec::new();
            let flush_improper =
                |fixed: &mut Vec<w3cos_std::Component>,
                 improper: &mut Vec<w3cos_std::Component>| {
                    if !improper.is_empty() {
                        fixed.push(table_row_from_misparented_children(
                            parent_style,
                            std::mem::take(improper),
                            None,
                        ));
                    }
                };
            for child in children {
                if child.style.display == Display::TableRow {
                    flush_improper(&mut fixed, &mut improper);
                    fixed.push(child);
                } else {
                    improper.push(child);
                }
            }
            flush_improper(&mut fixed, &mut improper);
            fixed
        }
        Display::TableColumnGroup => children,
        _ => {
            let is_table_internal = |display| {
                matches!(
                    display,
                    Display::TableCell
                        | Display::TableCaption
                        | Display::TableRow
                        | Display::TableRowGroup
                        | Display::TableHeaderGroup
                        | Display::TableFooterGroup
                        | Display::TableColumn
                        | Display::TableColumnGroup
                )
            };
            let mut fixed = Vec::with_capacity(children.len());
            let mut table_run = Vec::new();
            let flush_table_run =
                |fixed: &mut Vec<w3cos_std::Component>,
                 table_run: &mut Vec<w3cos_std::Component>| {
                    if !table_run.is_empty() {
                        fixed.push(anonymous_table_wrapper(
                            parent_style,
                            std::mem::take(table_run),
                        ));
                    }
                };
            let table_internal = children
                .iter()
                .map(|child| is_table_internal(child.style.display))
                .collect::<Vec<_>>();
            for (index, child) in children.into_iter().enumerate() {
                let table_fixup_whitespace = matches!(
                    &child.kind,
                    w3cos_std::ComponentKind::Text { content }
                        if is_only_css_whitespace(content)
                ) && child.children.is_empty()
                    && child.style.display == Display::Inline;
                if table_fixup_whitespace
                    && index > 0
                    && index + 1 < table_internal.len()
                    && table_internal[index - 1]
                    && table_internal[index + 1]
                {
                    continue;
                }
                if is_table_internal(child.style.display) {
                    table_run.push(child);
                } else {
                    flush_table_run(&mut fixed, &mut table_run);
                    fixed.push(child);
                }
            }
            flush_table_run(&mut fixed, &mut table_run);
            fixed
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
        assert!(
            tree.children[0]
                .children
                .iter()
                .all(|item| item.style.flex_shrink == 0.0
                    && item.style.min_height == Dimension::Px(19.2)
                    && item.children.len() == 1
                    && item.children[0].style.flex_shrink == 0.0)
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn lowered_block_inline_context_retains_its_line_height_strut() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            "#line-box",
            &[
                ("display", "block"),
                ("line-height", "96px"),
                ("width", "96px"),
            ],
        );

        let mut document = Document::new();
        let container = document.create_element("div");
        container.set_attribute(&mut document, "id", "line-box");
        let image = document.create_element("img");
        image.set_attribute(&mut document, "width", "15");
        image.set_attribute(&mut document, "height", "15");
        container.append_child(&mut document, image);
        document.body().append_child(&mut document, container);

        let tree = document.to_component_tree();
        let line_box = tree.children.first().expect("line box");
        assert_eq!(line_box.style.display, Display::Flex);
        assert_eq!(line_box.style.line_height, 6.0);
        assert_eq!(line_box.style.min_height, Dimension::Px(96.0));
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
            vec![text("before "), floating_box(Float::Right), text(" after")],
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
        assert_eq!(
            fixed[1].style.margin.top,
            w3cos_std::style::Spacing::Px(0.0)
        );

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
        assert_eq!(
            fixed[2].style.margin.top,
            w3cos_std::style::Spacing::Px(20.0)
        );
        assert!(
            fixed[2]
                .style
                .custom_properties
                .as_ref()
                .and_then(|properties| {
                    properties.get("--w3cos-internal-left-float-after-inline")
                })
                .is_some_and(|value| value == "1")
        );

        let fixed = hoist_floats_into_block_formatting_context(
            &context_style,
            vec![floating_box(Float::Left), floating_box(Float::Left)],
        );
        assert!(fixed.iter().all(|component| {
            component
                .style
                .custom_properties
                .as_ref()
                .is_none_or(|properties| {
                    !properties.contains_key("--w3cos-internal-left-float-after-inline")
                })
        }));

        let mut first = floating_box(Float::Left);
        first.style.clear = w3cos_std::style::Clear::Left;
        let mut cleared = floating_box(Float::Left);
        cleared.style.clear = w3cos_std::style::Clear::Left;
        let fixed = hoist_floats_into_block_formatting_context(
            &context_style,
            vec![first, cleared],
        );
        assert_eq!(fixed.len(), 2);
        assert!(fixed.iter().all(|component| {
            component.style.display == Display::Flex
                && component.style.width == Dimension::Percent(100.0)
                && matches!(component.children.as_slice(), [child] if child.style.float == Float::Left)
        }));
    }

    #[test]
    fn consecutive_passive_text_floats_coalesce_before_layout_rounding() {
        let floating_text = |content: &str| {
            let mut style = w3cos_std::style::Style::default();
            style.display = Display::Block;
            style.float = Float::Left;
            w3cos_std::Component::text(content, style)
        };
        let fixed = hoist_floats_into_block_formatting_context(
            &w3cos_std::style::Style::default(),
            vec![floating_text("T"), floating_text("E"), floating_text("S")],
        );

        assert_eq!(fixed.len(), 1);
        assert!(matches!(
            fixed[0].kind,
            ComponentKind::Text { ref content } if content == "TES"
        ));
        assert_eq!(fixed[0].style.float, Float::None);
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
    fn positioned_offsets_can_inherit_from_a_static_parent() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            "#parent",
            &[("position", "static"), ("top", "3.125em"), ("left", "50%")],
        );
        crate::stylesheet::register_rule(
            "#child",
            &[
                ("position", "relative"),
                ("font-size", "6.25em"),
                ("top", "inherit"),
                ("left", "inherit"),
            ],
        );

        let mut document = Document::new();
        let parent = document.create_element("div");
        parent.set_attribute(&mut document, "id", "parent");
        let child = document.create_element("div");
        child.set_attribute(&mut document, "id", "child");
        parent.append_child(&mut document, child);
        document.body().append_child(&mut document, parent);

        let child_style = document.computed_style_for(child.id);
        assert_eq!(child_style.position, Position::Relative);
        assert_eq!(child_style.top, Dimension::Px(50.0));
        assert_eq!(child_style.left, Dimension::Percent(50.0));
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn z_index_inherit_uses_the_parent_computed_value() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("#parent", &[("position", "relative"), ("z-index", "1")]);
        crate::stylesheet::register_rule(
            "#child",
            &[
                ("position", "absolute"),
                ("z-index", "-1"),
                ("z-index", "inherit"),
            ],
        );

        let mut document = Document::new();
        let parent = document.create_element("div");
        parent.set_attribute(&mut document, "id", "parent");
        let child = document.create_element("div");
        child.set_attribute(&mut document, "id", "child");
        parent.append_child(&mut document, child);
        document.body().append_child(&mut document, parent);

        assert_eq!(document.computed_style_for(parent.id).z_index, 1);
        assert_eq!(document.computed_style_for(child.id).z_index, 1);
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

    #[test]
    fn single_text_child_fast_path_collapses_css_whitespace() {
        let mut document = Document::new();
        let paragraph = document.create_element("p");
        let text = document
            .create_text_node(" In the middle of the rectangle, there\nshould be one line.\n\n");
        paragraph.append_child(&mut document, text);
        document.body().append_child(&mut document, paragraph);

        let tree = document.to_component_tree();
        assert!(
            matches!(
                tree.children[0].kind,
                ComponentKind::Text { ref content }
                    if content == "In the middle of the rectangle, there should be one line."
            ),
            "unexpected component: {:?}",
            tree.children[0]
        );
    }

    fn descendant_text_runs(
        component: &w3cos_std::Component,
    ) -> Vec<(String, w3cos_std::style::Style)> {
        fn collect(
            component: &w3cos_std::Component,
            runs: &mut Vec<(String, w3cos_std::style::Style)>,
        ) {
            if let ComponentKind::Text { content } = &component.kind {
                runs.push((content.clone(), component.style.clone()));
            }
            for child in &component.children {
                collect(child, runs);
            }
        }

        let mut runs = Vec::new();
        collect(component, &mut runs);
        runs
    }

    #[test]
    fn first_letter_splits_only_the_initial_typographic_unit() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("p::first-letter", &[("color", "green")]);
        let mut document = Document::new();
        let paragraph = document.create_element("p");
        let text = document.create_text_node("This is text");
        paragraph.append_child(&mut document, text);
        document.body().append_child(&mut document, paragraph);

        let tree = document.to_component_tree();
        let runs = descendant_text_runs(&tree.children[0]);
        assert_eq!(
            runs.iter().map(|run| run.0.as_str()).collect::<Vec<_>>(),
            ["T", "his is text"]
        );
        assert_eq!(
            runs[0].1.color,
            w3cos_std::Color::from_named("green").unwrap()
        );
        assert_ne!(runs[1].1.color, runs[0].1.color);
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn first_letter_applies_to_generated_before_content() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("div::before", &[("content", "'Filler Text'")]);
        crate::stylesheet::register_rule("div::first-letter", &[("font-size", "98px")]);
        let mut document = Document::new();
        let block = document.create_element("div");
        document.body().append_child(&mut document, block);

        let tree = document.to_component_tree();
        let runs = descendant_text_runs(&tree.children[0]);
        assert_eq!(
            runs.iter().map(|run| run.0.as_str()).collect::<Vec<_>>(),
            ["F", "iller Text"]
        );
        assert_eq!(runs[0].1.font_size, 98.0);
        assert_ne!(runs[1].1.font_size, 98.0);
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn first_letter_skips_out_of_flow_content() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("div::first-letter", &[("color", "green")]);
        crate::stylesheet::register_rule("span", &[("position", "absolute")]);
        let mut document = Document::new();
        let block = document.create_element("div");
        let absolute = document.create_element("span");
        absolute.set_text_content(&mut document, "F");
        block.append_child(&mut document, absolute);
        let text = document.create_text_node("PASS");
        block.append_child(&mut document, text);
        document.body().append_child(&mut document, block);

        let tree = document.to_component_tree();
        let runs = descendant_text_runs(&tree.children[0]);
        let pass = runs.iter().position(|run| run.0 == "P").expect("split P");
        assert_eq!(
            runs[pass].1.color,
            w3cos_std::Color::from_named("green").unwrap()
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn first_line_stops_at_a_forced_break() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("p::first-line", &[("color", "fuchsia")]);
        let mut document = Document::new();
        let paragraph = document.create_element("p");
        let first = document.create_text_node("first line");
        paragraph.append_child(&mut document, first);
        let br = document.create_element("br");
        paragraph.append_child(&mut document, br);
        let second = document.create_text_node("second line");
        paragraph.append_child(&mut document, second);
        document.body().append_child(&mut document, paragraph);

        let tree = document.to_component_tree();
        let runs = descendant_text_runs(&tree.children[0]);
        let first = runs
            .iter()
            .find(|run| run.0 == "first line")
            .expect("first line");
        let second = runs
            .iter()
            .find(|run| run.0 == "second line")
            .expect("second line");
        assert_eq!(
            first.1.color,
            w3cos_std::Color::from_named("fuchsia").unwrap()
        );
        assert_ne!(second.1.color, first.1.color);
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn first_line_inherited_background_keeps_one_passive_text_run() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("div", &[("background", "green")]);
        crate::stylesheet::register_rule("div::first-line", &[("background-color", "red")]);
        crate::stylesheet::register_rule("span.one", &[("background", "inherit")]);
        crate::stylesheet::register_rule("span.two", &[("background-color", "inherit")]);
        let mut document = Document::new();
        let block = document.create_element("div");
        for (class_name, content) in [("one", "One"), ("two", "Two")] {
            let span = document.create_element("span");
            span.set_attribute(&mut document, "class", class_name);
            span.set_text_content(&mut document, content);
            block.append_child(&mut document, span);
        }
        document.body().append_child(&mut document, block);

        let tree = document.to_component_tree();
        let runs = descendant_text_runs(&tree.children[0]);
        assert_eq!(
            runs.iter().map(|run| run.0.as_str()).collect::<Vec<_>>(),
            ["OneTwo"],
            "unexpected tree: {:#?}",
            tree.children[0]
        );
        assert_eq!(
            runs[0].1.background,
            w3cos_std::Color::from_named("green").unwrap()
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn first_line_inherited_color_reaches_a_nested_float() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("div", &[("color", "red")]);
        crate::stylesheet::register_rule("div:first-line", &[("color", "green")]);
        let mut document = Document::new();
        let block = document.create_element("div");
        let inline = document.create_element("span");
        let floated = document.create_element("span");
        floated.set_attribute(&mut document, "style", "float: left");
        floated.set_text_content(&mut document, "This should be green");
        inline.append_child(&mut document, floated);
        block.append_child(&mut document, inline);
        document.body().append_child(&mut document, block);

        let tree = document.to_component_tree();
        let runs = descendant_text_runs(&tree.children[0]);
        assert_eq!(
            runs[0].1.color,
            w3cos_std::Color::from_named("green").unwrap(),
            "unexpected tree: {:#?}",
            tree.children[0]
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn first_line_length_stops_at_width_and_keeps_explicit_inline_wrapper() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            "p",
            &[
                ("width", "1em"),
                ("font", "50px/1 Ahem"),
                ("background", "yellow"),
            ],
        );
        crate::stylesheet::register_rule(".a", &[("background", "fuchsia")]);
        crate::stylesheet::register_rule(
            ".test:first-line, .control .first-line",
            &[("vertical-align", "0.8em")],
        );
        let mut document = Document::new();
        for (paragraph_class, wraps_first) in [("test", false), ("control", true)] {
            let paragraph = document.create_element("p");
            paragraph.set_attribute(&mut document, "class", paragraph_class);
            let first = document.create_element("span");
            first.set_attribute(
                &mut document,
                "class",
                if wraps_first { "first-line" } else { "a" },
            );
            if wraps_first {
                let a = document.create_element("span");
                a.set_attribute(&mut document, "class", "a");
                a.set_text_content(&mut document, "É");
                first.append_child(&mut document, a);
            } else {
                first.set_text_content(&mut document, "É");
            }
            let second = document.create_element("span");
            second.set_text_content(&mut document, "X");
            paragraph.append_child(&mut document, first);
            paragraph.append_child(&mut document, second);
            document.body().append_child(&mut document, paragraph);
        }
        let tree = document.to_component_tree();
        let pseudo_runs = descendant_text_runs(&tree.children[0]);
        assert_eq!(pseudo_runs[0].0, "É");
        assert_eq!(
            pseudo_runs[0].1.height,
            w3cos_std::style::Dimension::Px(90.0)
        );
        assert_eq!(pseudo_runs[1].0, "X");
        assert!(
            pseudo_runs[1]
                .1
                .custom_properties
                .as_ref()
                .is_none_or(|properties| {
                    !properties.contains_key("--w3cos-internal-vertical-align-length")
                })
        );

        let explicit_wrapper = &tree.children[1].children[0];
        assert_eq!(
            explicit_wrapper.style.display,
            w3cos_std::style::Display::InlineFlex
        );
        assert_eq!(
            explicit_wrapper.style.height,
            w3cos_std::style::Dimension::Px(90.0)
        );
        assert!(
            explicit_wrapper
                .style
                .custom_properties
                .as_ref()
                .is_some_and(
                    |properties| properties.contains_key("--w3cos-internal-vertical-align-length")
                )
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn q_uses_generated_quote_content_and_authored_quote_pairs() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("#custom", &[("quotes", "'<1>' '</1>'")]);
        let mut document = Document::new();
        let first = document.create_element("q");
        let first_text = document.create_text_node("Foo");
        first.append_child(&mut document, first_text);
        document.body().append_child(&mut document, first);
        let custom = document.create_element("q");
        custom.set_attribute(&mut document, "id", "custom");
        let custom_text = document.create_text_node("0");
        custom.append_child(&mut document, custom_text);
        document.body().append_child(&mut document, custom);

        let tree = document.to_component_tree();
        let rendered = descendant_text_runs(&tree)
            .into_iter()
            .map(|run| run.0)
            .collect::<String>();
        assert_eq!(rendered, "\"Foo\"<1>0</1>");
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn paragraph_with_mixed_inline_children_uses_the_computed_line_axis() {
        let mut document = Document::new();
        let paragraph = document.create_element("p");
        let before = document.create_text_node("before ");
        paragraph.append_child(&mut document, before);
        let strong = document.create_element("strong");
        strong.set_text_content(&mut document, "after");
        paragraph.append_child(&mut document, strong);
        document.body().append_child(&mut document, paragraph);

        let tree = document.to_component_tree();
        assert!(matches!(tree.children[0].kind, ComponentKind::Row));
        assert_eq!(
            tree.children[0].style.flex_direction,
            w3cos_std::style::FlexDirection::Row
        );
    }

    #[test]
    fn right_aligned_mixed_inline_content_aligns_the_anonymous_line() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("p", &[("text-align", "right")]);
        crate::stylesheet::register_rule("span", &[("margin-right", "2em")]);
        let mut document = Document::new();
        let paragraph = document.create_element("p");
        let before = document.create_text_node("before ");
        paragraph.append_child(&mut document, before);
        let span = document.create_element("span");
        span.set_text_content(&mut document, "after");
        paragraph.append_child(&mut document, span);
        document.body().append_child(&mut document, paragraph);

        let tree = document.to_component_tree();

        assert_eq!(
            tree.children[0].style.justify_content,
            w3cos_std::style::JustifyContent::FlexEnd
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn justified_single_text_run_uses_the_block_line_width() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("div", &[("text-align", "justify")]);
        let mut document = Document::new();
        let block = document.create_element("div");
        let text =
            document.create_text_node("A long text run must wrap to the anonymous line box width.");
        block.append_child(&mut document, text);
        document.body().append_child(&mut document, block);

        let tree = document.to_component_tree();
        let line = &tree.children[0];
        assert_eq!(line.style.display, Display::Flex);
        assert_eq!(line.children[0].style.width, Dimension::Percent(100.0));
        assert_eq!(line.children[0].style.min_width, Dimension::Px(0.0));
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn single_retained_inline_fragment_keeps_the_block_line_alignment_context() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            "div",
            &[("direction", "rtl"), ("unicode-bidi", "bidi-override")],
        );
        let mut document = Document::new();
        let block = document.create_element("div");
        let text = document.create_text_node(".d c b a");
        block.append_child(&mut document, text);
        document.body().append_child(&mut document, block);

        let tree = document.to_component_tree();
        let line = &tree.children[0];

        assert_eq!(line.style.display, Display::Flex);
        assert_eq!(
            line.style.justify_content,
            w3cos_std::style::JustifyContent::FlexEnd
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn fixed_width_block_constrains_its_single_wrapping_text_line() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("div", &[("width", "150px"), ("word-spacing", "75px")]);
        let mut document = Document::new();
        let block = document.create_element("div");
        let text = document.create_text_node("1 2 3 4");
        block.append_child(&mut document, text);
        document.body().append_child(&mut document, block);

        let tree = document.to_component_tree();
        let line = &tree.children[0];
        assert_eq!(line.style.display, Display::Flex);
        assert_eq!(
            line.children[0].style.width,
            w3cos_std::style::Dimension::Percent(100.0)
        );
        assert_eq!(
            line.children[0].style.min_width,
            w3cos_std::style::Dimension::Px(0.0)
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn rtl_inline_block_aligns_its_single_text_line_to_the_inline_end() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            "div",
            &[
                ("display", "inline-block"),
                ("direction", "rtl"),
                ("width", "100px"),
                ("background", "orange"),
            ],
        );
        let mut document = Document::new();
        let host = document.create_element("div");
        let text = document.create_text_node("X");
        host.append_child(&mut document, text);
        document.body().append_child(&mut document, host);

        let tree = document.to_component_tree();
        let host = &tree.children[0];
        assert_eq!(host.style.display, Display::InlineFlex);
        assert_eq!(
            host.style.justify_content,
            w3cos_std::style::JustifyContent::FlexEnd
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn rtl_anonymous_table_row_places_its_first_cell_on_the_right() {
        let mut table_style = w3cos_std::style::Style::default();
        table_style.direction = w3cos_std::style::TextDirection::Rtl;
        let row = anonymous_table_row(
            &table_style,
            vec![
                w3cos_std::Component::boxed(w3cos_std::style::Style::default(), vec![]),
                w3cos_std::Component::boxed(w3cos_std::style::Style::default(), vec![]),
            ],
            None,
        );

        assert_eq!(
            row.style.flex_direction,
            w3cos_std::style::FlexDirection::RowReverse
        );
    }

    #[test]
    fn table_cell_text_is_not_coalesced_across_column_boundaries() {
        let cell = |content| {
            w3cos_std::Component::text(
                content,
                w3cos_std::style::Style {
                    display: Display::TableCell,
                    ..w3cos_std::style::Style::default()
                },
            )
        };
        let mut row = w3cos_std::Component::row(
            w3cos_std::style::Style {
                display: Display::TableRow,
                ..w3cos_std::style::Style::default()
            },
            vec![cell("one"), cell("two"), cell("three")],
        );

        coalesce_passive_inline_text_children(&mut row);

        assert_eq!(row.children.len(), 3);
    }

    #[test]
    fn table_infers_a_row_for_leading_cells_before_a_row_group() {
        let cell = || {
            w3cos_std::Component::boxed(
                w3cos_std::style::Style {
                    display: Display::TableCell,
                    ..w3cos_std::style::Style::default()
                },
                vec![],
            )
        };
        let row_group = w3cos_std::Component::row(
            w3cos_std::style::Style {
                display: Display::TableRowGroup,
                ..w3cos_std::style::Style::default()
            },
            vec![],
        );
        let table_style = w3cos_std::style::Style {
            display: Display::Table,
            ..w3cos_std::style::Style::default()
        };

        let fixed = fixup_css_table_children(&table_style, vec![cell(), cell(), cell(), row_group]);

        assert_eq!(fixed.len(), 2);
        assert_eq!(fixed[0].style.display, Display::TableRow);
        assert_eq!(fixed[0].children.len(), 3);
        assert_eq!(fixed[1].style.display, Display::TableRowGroup);
    }

    #[test]
    fn anonymous_table_wraps_leading_cells_and_existing_row_group_together() {
        let cell = || {
            w3cos_std::Component::boxed(
                w3cos_std::style::Style {
                    display: Display::TableCell,
                    ..w3cos_std::style::Style::default()
                },
                vec![],
            )
        };
        let row_group = w3cos_std::Component::row(
            w3cos_std::style::Style {
                display: Display::TableRowGroup,
                ..w3cos_std::style::Style::default()
            },
            vec![],
        );

        let table = anonymous_table_wrapper(
            &w3cos_std::style::Style::default(),
            vec![cell(), cell(), cell(), row_group],
        );

        assert_eq!(table.style.display, Display::Table);
        assert_eq!(table.children.len(), 2);
        assert_eq!(table.children[0].style.display, Display::TableRow);
        assert_eq!(table.children[0].children.len(), 3);
        assert_eq!(table.children[1].style.display, Display::TableRowGroup);
    }

    #[test]
    fn table_row_group_keeps_authored_rows_without_a_nested_table() {
        let row = || {
            w3cos_std::Component::row(
                w3cos_std::style::Style {
                    display: Display::TableRow,
                    ..w3cos_std::style::Style::default()
                },
                vec![],
            )
        };
        let group_style = w3cos_std::style::Style {
            display: Display::TableRowGroup,
            ..w3cos_std::style::Style::default()
        };

        let fixed = fixup_css_table_children(&group_style, vec![row(), row()]);

        assert_eq!(fixed.len(), 2);
        assert!(
            fixed
                .iter()
                .all(|child| child.style.display == Display::TableRow)
        );
    }

    #[test]
    fn table_row_group_wraps_one_improper_child_run_in_one_row() {
        let component = |display| {
            w3cos_std::Component::boxed(
                w3cos_std::style::Style {
                    display,
                    ..w3cos_std::style::Style::default()
                },
                vec![],
            )
        };
        let group_style = w3cos_std::style::Style {
            display: Display::TableRowGroup,
            ..w3cos_std::style::Style::default()
        };

        let fixed = fixup_css_table_children(
            &group_style,
            vec![
                component(Display::TableCell),
                component(Display::Block),
                component(Display::TableCell),
            ],
        );

        assert_eq!(fixed.len(), 1);
        assert_eq!(fixed[0].style.display, Display::TableRow);
        assert_eq!(fixed[0].children.len(), 3);
        assert!(
            fixed[0]
                .children
                .iter()
                .all(|child| child.style.display == Display::TableCell)
        );
    }

    #[test]
    fn anonymous_cell_preserves_one_internal_collapsed_space_between_inline_boxes() {
        let text = |content: &str| {
            w3cos_std::Component::text(
                content,
                w3cos_std::style::Style {
                    display: Display::Inline,
                    ..w3cos_std::style::Style::default()
                },
            )
        };
        let row_style = w3cos_std::style::Style {
            display: Display::TableRow,
            ..w3cos_std::style::Style::default()
        };

        let fixed = fixup_css_table_children(
            &row_style,
            vec![text("Row 1,"), text("\n  "), text("Col 1")],
        );

        assert_eq!(fixed.len(), 1);
        let line = &fixed[0].children[0];
        assert!(matches!(
            &line.children[1].kind,
            ComponentKind::Text { content } if content == " "
        ));
    }

    #[test]
    fn anonymous_table_orders_header_body_and_footer_groups() {
        let group = |display| {
            w3cos_std::Component::row(
                w3cos_std::style::Style {
                    display,
                    ..w3cos_std::style::Style::default()
                },
                vec![],
            )
        };
        let table = anonymous_table_wrapper(
            &w3cos_std::style::Style::default(),
            vec![
                group(Display::TableRowGroup),
                group(Display::TableFooterGroup),
                group(Display::TableHeaderGroup),
            ],
        );

        assert_eq!(table.children[0].style.display, Display::TableHeaderGroup);
        assert_eq!(table.children[1].style.display, Display::TableRowGroup);
        assert_eq!(table.children[2].style.display, Display::TableFooterGroup);
    }

    #[test]
    fn block_child_splits_adjacent_anonymous_table_runs() {
        let component = |display| {
            w3cos_std::Component::boxed(
                w3cos_std::style::Style {
                    display,
                    ..w3cos_std::style::Style::default()
                },
                vec![],
            )
        };
        let fixed = fixup_css_table_children(
            &w3cos_std::style::Style {
                display: Display::Block,
                ..w3cos_std::style::Style::default()
            },
            vec![
                component(Display::TableRow),
                component(Display::Block),
                component(Display::TableRow),
            ],
        );

        assert_eq!(fixed.len(), 3);
        assert_eq!(fixed[0].style.display, Display::Table);
        assert_eq!(fixed[1].style.display, Display::Block);
        assert_eq!(fixed[2].style.display, Display::Table);
    }

    #[test]
    fn anonymous_table_wrapper_orders_captions_around_its_grid() {
        let parent_style = w3cos_std::style::Style::default();
        let bottom_style = w3cos_std::style::Style {
            display: Display::TableCaption,
            caption_side_bottom: true,
            ..w3cos_std::style::Style::default()
        };
        let top = w3cos_std::Component::boxed(
            w3cos_std::style::Style {
                display: Display::TableCaption,
                ..w3cos_std::style::Style::default()
            },
            vec![],
        );
        let cell = w3cos_std::Component::boxed(
            w3cos_std::style::Style {
                display: Display::TableCell,
                ..w3cos_std::style::Style::default()
            },
            vec![],
        );
        let bottom = w3cos_std::Component::boxed(bottom_style, vec![]);

        let table = anonymous_table_wrapper(&parent_style, vec![bottom, cell, top]);

        assert_eq!(table.style.display, Display::Table);
        assert_eq!(table.children[0].style.display, Display::TableCaption);
        assert_eq!(table.children[1].style.display, Display::TableRow);
        assert_eq!(table.children[2].style.display, Display::TableCaption);
        assert!(table.children[2].style.caption_side_bottom);
    }

    #[test]
    fn rtl_authored_table_row_reverses_cells_and_cell_line_fills_its_box() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("#table", &[("display", "table"), ("direction", "rtl")]);
        crate::stylesheet::register_rule("#row", &[("display", "table-row")]);
        crate::stylesheet::register_rule("#cell", &[("display", "table-cell")]);
        let mut document = Document::new();
        let table = document.create_element("div");
        table.set_attribute(&mut document, "id", "table");
        let row = document.create_element("div");
        row.set_attribute(&mut document, "id", "row");
        let cell = document.create_element("div");
        cell.set_attribute(&mut document, "id", "cell");
        let text = document.create_text_node("X");
        cell.append_child(&mut document, text);
        row.append_child(&mut document, cell);
        table.append_child(&mut document, row);
        document.body().append_child(&mut document, table);

        let tree = document.to_component_tree();
        let table = &tree.children[0];
        let row = &table.children[0];
        let cell = &row.children[0];
        let line = &cell.children[0];
        assert_eq!(
            row.style.flex_direction,
            w3cos_std::style::FlexDirection::RowReverse
        );
        assert_eq!(
            line.style.width,
            w3cos_std::style::Dimension::Percent(100.0)
        );
        assert_eq!(
            line.style.justify_content,
            w3cos_std::style::JustifyContent::FlexEnd
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn table_cellpadding_overrides_ua_padding_but_not_author_padding() {
        crate::stylesheet::clear_rules();
        let mut document = Document::new();
        let table = document.create_element("table");
        table.set_attribute(&mut document, "cellpadding", "0");
        let row = document.create_element("tr");
        let cell = document.create_element("td");
        row.append_child(&mut document, cell);
        table.append_child(&mut document, row);
        document.body().append_child(&mut document, table);

        let tree = document.to_component_tree();
        assert_eq!(
            tree.children[0].children[0].children[0].style.padding,
            w3cos_std::style::Edges::ZERO
        );

        crate::stylesheet::register_rule("td", &[("padding", "7px")]);
        let tree = document.to_component_tree();
        assert_eq!(
            tree.children[0].children[0].children[0].style.padding,
            w3cos_std::style::Edges::all(7.0)
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn table_cellspacing_overrides_ua_spacing_but_not_author_spacing() {
        crate::stylesheet::clear_rules();
        let mut document = Document::new();
        let table = document.create_element("table");
        table.set_attribute(&mut document, "cellspacing", "0");
        document.body().append_child(&mut document, table);

        let tree = document.to_component_tree();
        assert_eq!(tree.children[0].style.border_spacing_x, 0.0);
        assert_eq!(tree.children[0].style.border_spacing_y, 0.0);

        crate::stylesheet::register_rule("table", &[("border-spacing", "7px")]);
        let tree = document.to_component_tree();
        assert_eq!(tree.children[0].style.border_spacing_x, 7.0);
        assert_eq!(tree.children[0].style.border_spacing_y, 7.0);
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn border_spacing_em_resolves_against_final_computed_font_size() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            "#table",
            &[
                ("display", "table"),
                ("border-spacing", "1em"),
                ("font-size", "20px"),
            ],
        );
        let mut document = Document::new();
        let table = document.create_element("div");
        table.set_attribute(&mut document, "id", "table");
        document.body().append_child(&mut document, table);

        let tree = document.to_component_tree();
        assert_eq!(tree.children[0].style.border_spacing_x, 20.0);
        assert_eq!(tree.children[0].style.border_spacing_y, 20.0);
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn plain_anonymous_table_cells_share_one_text_shaping_run() {
        let cell = |content: &str, white_space| {
            let mut text_style = w3cos_std::style::Style::default();
            text_style.display = Display::Inline;
            text_style.white_space = white_space;
            w3cos_std::Component::boxed(
                w3cos_std::style::Style {
                    display: Display::TableCell,
                    ..w3cos_std::style::Style::default()
                },
                vec![w3cos_std::Component::text(content, text_style)],
            )
        };
        let mut cells = vec![
            cell("a", w3cos_std::style::WhiteSpace::Normal),
            cell(" ", w3cos_std::style::WhiteSpace::Pre),
            cell("bc", w3cos_std::style::WhiteSpace::Normal),
            cell(" ", w3cos_std::style::WhiteSpace::Pre),
            cell("d", w3cos_std::style::WhiteSpace::Normal),
        ];

        coalesce_plain_anonymous_table_cell_text(&mut cells);

        assert!(matches!(
            &cells[0].children[0].kind,
            ComponentKind::Text { content } if content == "a bc d"
        ));
        assert!(cells[1..].iter().all(|cell| matches!(
            &cell.children[0].kind,
            ComponentKind::Text { content } if content.is_empty()
        )));
    }

    #[test]
    fn optimized_text_table_cells_share_one_text_shaping_run() {
        let cell = |content: &str, white_space| {
            let mut style = w3cos_std::style::Style {
                display: Display::TableCell,
                ..w3cos_std::style::Style::default()
            };
            style.white_space = white_space;
            w3cos_std::Component::text(content, style)
        };
        let mut cells = vec![
            cell("a", w3cos_std::style::WhiteSpace::Normal),
            cell(" ", w3cos_std::style::WhiteSpace::Pre),
            cell("bc", w3cos_std::style::WhiteSpace::Normal),
            cell(" ", w3cos_std::style::WhiteSpace::Pre),
            cell("d", w3cos_std::style::WhiteSpace::Normal),
        ];

        coalesce_plain_anonymous_table_cell_text(&mut cells);

        assert!(matches!(
            &cells[0].kind,
            ComponentKind::Text { content } if content == "a bc d"
        ));
        assert!(cells[1..].iter().all(|cell| matches!(
            &cell.kind,
            ComponentKind::Text { content } if content.is_empty()
        )));
    }

    #[test]
    fn dynamically_inserted_anonymous_table_cell_joins_the_text_shaping_run() {
        let mut document = Document::new();
        let host = document.create_element("span");
        host.set_attribute(&mut document, "style", "display: block");
        let append_cell = |document: &mut Document,
                           host: Element,
                           content: &str,
                           preserve_space: bool|
         -> Element {
            let cell = document.create_element("span");
            cell.set_attribute(
                document,
                "style",
                if preserve_space {
                    "display: table-cell; white-space: pre"
                } else {
                    "display: table-cell"
                },
            );
            cell.set_text_content(document, content);
            host.append_child(document, cell);
            cell
        };
        append_cell(&mut document, host, "a", false);
        append_cell(&mut document, host, " ", true);
        let insertion_point = append_cell(&mut document, host, " ", true);
        append_cell(&mut document, host, "d", false);
        let inserted = document.create_element("span");
        inserted.set_attribute(&mut document, "style", "display: table-cell");
        inserted.set_text_content(&mut document, "bc");
        host.insert_before(&mut document, inserted, insertion_point);
        document.body().append_child(&mut document, host);

        fn collect_text(component: &w3cos_std::Component, output: &mut Vec<String>) {
            if let ComponentKind::Text { content } = &component.kind
                && !content.is_empty()
            {
                output.push(content.clone());
            }
            for child in &component.children {
                collect_text(child, output);
            }
        }
        let tree = document.to_component_tree();
        let mut text_runs = Vec::new();
        collect_text(&tree, &mut text_runs);
        assert_eq!(text_runs, ["a bc d"]);
    }

    #[test]
    fn inline_anonymous_table_run_preserves_collapsed_spaces_at_its_edges() {
        let mut document = Document::new();
        let host = document.create_element("span");
        let append_span =
            |document: &mut Document, host: Element, content: &str, display: Option<&str>| {
                let child = document.create_element("span");
                if let Some(display) = display {
                    child.style_mut(document).set_property("display", display);
                }
                child.set_text_content(document, content);
                host.append_child(document, child);
            };
        append_span(&mut document, host, "a", None);
        let space = document.create_text_node("\n ");
        host.append_child(&mut document, space);
        append_span(&mut document, host, "b", Some("table-cell"));
        let space = document.create_text_node("\n ");
        host.append_child(&mut document, space);
        append_span(&mut document, host, "c", Some("table-cell"));
        let space = document.create_text_node("\n ");
        host.append_child(&mut document, space);
        append_span(&mut document, host, "d", None);
        document.body().append_child(&mut document, host);

        fn inspect(component: &w3cos_std::Component, text: &mut String, inline_table: &mut bool) {
            *inline_table |= component.style.display == Display::InlineTable;
            if let ComponentKind::Text { content } = &component.kind {
                text.push_str(content);
            }
            for child in &component.children {
                inspect(child, text, inline_table);
            }
        }
        let tree = document.to_component_tree();
        let mut text = String::new();
        let mut inline_table = false;
        inspect(&tree, &mut text, &mut inline_table);
        assert_eq!(text, "a bc d");
        assert!(!inline_table);
    }

    #[test]
    fn block_anonymous_table_row_shares_one_text_shaping_run() {
        let mut document = Document::new();
        let host = document.create_element("div");
        let row = document.create_element("span");
        row.set_attribute(
            &mut document,
            "style",
            "display: table-row; white-space: pre",
        );
        let leading = document.create_element("span");
        leading.set_attribute(&mut document, "style", "display: table-cell");
        leading.set_text_content(&mut document, "a");
        row.append_child(&mut document, leading);
        let middle = document.create_text_node(" bc ");
        row.append_child(&mut document, middle);
        let trailing = document.create_element("span");
        trailing.set_attribute(&mut document, "style", "display: table-cell");
        trailing.set_text_content(&mut document, "d");
        row.append_child(&mut document, trailing);
        host.append_child(&mut document, row);
        document.body().append_child(&mut document, host);

        fn collect(component: &w3cos_std::Component, runs: &mut Vec<String>) {
            if let ComponentKind::Text { content } = &component.kind
                && !content.is_empty()
            {
                runs.push(content.clone());
            }
            for child in &component.children {
                collect(child, runs);
            }
        }
        let tree = document.to_component_tree();
        let mut runs = Vec::new();
        collect(&tree, &mut runs);
        assert_eq!(runs, ["a bc d"]);
    }

    #[test]
    fn preformatted_anonymous_cell_preserves_spaces_around_inline_content() {
        let mut document = Document::new();
        let host = document.create_element("div");
        let row = document.create_element("span");
        row.set_attribute(
            &mut document,
            "style",
            "display: table-row; white-space: pre",
        );
        let append =
            |document: &mut Document, row: Element, content: &str, display: Option<&str>| {
                let child = document.create_element("span");
                if let Some(display) = display {
                    child.style_mut(document).set_property("display", display);
                }
                child.set_text_content(document, content);
                row.append_child(document, child);
            };
        append(&mut document, row, "a", Some("table-cell"));
        let leading_space = document.create_text_node(" ");
        row.append_child(&mut document, leading_space);
        append(&mut document, row, "bc", None);
        let trailing_space = document.create_text_node(" ");
        row.append_child(&mut document, trailing_space);
        append(&mut document, row, "d", Some("table-cell"));
        host.append_child(&mut document, row);
        document.body().append_child(&mut document, host);

        fn collect(component: &w3cos_std::Component, text: &mut String) {
            if let ComponentKind::Text { content } = &component.kind {
                text.push_str(content);
            }
            for child in &component.children {
                collect(child, text);
            }
        }
        let tree = document.to_component_tree();
        let mut text = String::new();
        collect(&tree, &mut text);
        assert_eq!(text, "a bc d");
    }

    #[test]
    fn hidden_script_text_is_not_revived_by_anonymous_table_flattening() {
        let mut document = Document::new();
        let host = document.create_element("div");
        let row = document.create_element("span");
        row.set_attribute(
            &mut document,
            "style",
            "display: table-row; white-space: pre",
        );
        let append_cell = |document: &mut Document, row: Element, content: &str| {
            let cell = document.create_element("span");
            cell.style_mut(document)
                .set_property("display", "table-cell");
            cell.set_text_content(document, content);
            row.append_child(document, cell);
        };
        append_cell(&mut document, row, "a");
        let leading_space = document.create_text_node(" ");
        row.append_child(&mut document, leading_space);
        let script = document.create_element("script");
        script.set_text_content(&mut document, "document.body.offsetWidth");
        row.append_child(&mut document, script);
        let middle = document.create_text_node("bc");
        row.append_child(&mut document, middle);
        let script = document.create_element("script");
        script.set_text_content(&mut document, "document.body.offsetWidth");
        row.append_child(&mut document, script);
        let trailing_space = document.create_text_node(" ");
        row.append_child(&mut document, trailing_space);
        append_cell(&mut document, row, "d");
        host.append_child(&mut document, row);
        document.body().append_child(&mut document, host);

        fn collect(component: &w3cos_std::Component, text: &mut String) {
            if component.style.display == Display::None {
                return;
            }
            if let ComponentKind::Text { content } = &component.kind {
                text.push_str(content);
            }
            for child in &component.children {
                collect(child, text);
            }
        }
        let tree = document.to_component_tree();
        let mut text = String::new();
        collect(&tree, &mut text);
        assert_eq!(text, "abcd");
    }

    #[test]
    fn hidden_script_does_not_split_adjacent_visible_text_runs() {
        let mut document = Document::new();
        let host = document.create_element("span");
        let leading = document.create_text_node("a");
        host.append_child(&mut document, leading);
        let script = document.create_element("script");
        script.set_text_content(&mut document, "document.body.offsetWidth");
        host.append_child(&mut document, script);
        let trailing = document.create_text_node(" b");
        host.append_child(&mut document, trailing);
        document.body().append_child(&mut document, host);

        fn collect(component: &w3cos_std::Component, runs: &mut Vec<String>) {
            if let ComponentKind::Text { content } = &component.kind
                && !content.is_empty()
            {
                runs.push(content.clone());
            }
            for child in &component.children {
                collect(child, runs);
            }
        }
        let tree = document.to_component_tree();
        let mut runs = Vec::new();
        collect(&tree, &mut runs);
        assert_eq!(runs, ["a b"]);
    }

    #[test]
    fn replaced_image_cannot_establish_a_table_cell_box() {
        let mut document = Document::new();
        let table = document.create_element("div");
        table
            .style_mut(&mut document)
            .set_property("display", "table");
        let row = document.create_element("div");
        row.style_mut(&mut document)
            .set_property("display", "table-row");
        let image = document.create_element("img");
        image
            .style_mut(&mut document)
            .set_property("display", "table-cell");
        row.append_child(&mut document, image);
        table.append_child(&mut document, row);
        document.body().append_child(&mut document, table);

        fn image_display(component: &w3cos_std::Component) -> Option<Display> {
            if matches!(component.kind, ComponentKind::Image { .. }) {
                return Some(component.style.display);
            }
            component.children.iter().find_map(image_display)
        }
        assert_eq!(
            image_display(&document.to_component_tree()),
            Some(Display::InlineBlock)
        );
    }

    #[test]
    fn block_anonymous_table_preserves_a_line_break_from_inline_siblings() {
        let mut document = Document::new();
        let above = document.create_text_node("above");
        document.body().append_child(&mut document, above);
        let cell = document.create_element("span");
        cell.style_mut(&mut document)
            .set_property("display", "table-cell");
        cell.set_text_content(&mut document, "below");
        document.body().append_child(&mut document, cell);

        fn collect(component: &w3cos_std::Component, text: &mut String) {
            if let ComponentKind::Text { content } = &component.kind {
                text.push_str(content);
            }
            for child in &component.children {
                collect(child, text);
            }
        }
        let mut text = String::new();
        collect(&document.to_component_tree(), &mut text);
        assert_eq!(text, "above\u{2028} below");
    }

    #[test]
    fn generated_after_text_joins_a_transparent_anonymous_inline_table_run() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("#generated::after", &[("content", "' d'")]);
        let mut document = Document::new();
        let host = document.create_element("span");
        host.set_attribute(&mut document, "id", "generated");
        let prefix = document.create_text_node("a ");
        host.append_child(&mut document, prefix);
        for content in ["b", "c"] {
            let cell = document.create_element("span");
            cell.style_mut(&mut document)
                .set_property("display", "table-cell");
            cell.set_text_content(&mut document, content);
            host.append_child(&mut document, cell);
        }
        document.body().append_child(&mut document, host);

        fn collect(component: &w3cos_std::Component, runs: &mut Vec<String>) {
            if let ComponentKind::Text { content } = &component.kind
                && !content.is_empty()
            {
                runs.push(content.clone());
            }
            for child in &component.children {
                collect(child, runs);
            }
        }
        let tree = document.to_component_tree();
        let mut runs = Vec::new();
        collect(&tree, &mut runs);
        assert_eq!(runs, ["a bc d"]);
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn direction_declared_on_table_row_does_not_reorder_columns() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("#table", &[("display", "table")]);
        crate::stylesheet::register_rule("#row", &[("display", "table-row"), ("direction", "rtl")]);
        crate::stylesheet::register_rule(".cell", &[("display", "table-cell")]);
        let mut document = Document::new();
        let table = document.create_element("div");
        table.set_attribute(&mut document, "id", "table");
        let row = document.create_element("div");
        row.set_attribute(&mut document, "id", "row");
        for _ in 0..2 {
            let cell = document.create_element("div");
            cell.set_attribute(&mut document, "class", "cell");
            row.append_child(&mut document, cell);
        }
        table.append_child(&mut document, row);
        document.body().append_child(&mut document, table);

        let tree = document.to_component_tree();
        assert_eq!(
            tree.children[0].children[0].style.flex_direction,
            w3cos_std::style::FlexDirection::Row
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn explicit_bidi_overrides_reorder_across_inline_box_boundaries() {
        let text = |content: &str| {
            let mut style = w3cos_std::style::Style::default();
            style.display = Display::Inline;
            w3cos_std::Component::text(content, style)
        };
        let boxed = |content: &str| {
            let mut style = w3cos_std::style::Style::default();
            style.display = Display::Inline;
            style.border_width = 3.0;
            style.border_left_width = Some(3.0);
            style.border_right_width = Some(3.0);
            w3cos_std::Component::row(style, vec![text(content)])
        };
        let mut line = w3cos_std::Component::row(
            w3cos_std::style::Style::default(),
            vec![
                text("a\u{202e}l\u{202d}"),
                boxed("c\u{202e}j\u{202d}e\u{202e}"),
                text("h\u{202d}g\u{202c}f"),
                boxed("\u{202c}i\u{202c}d\u{202c}k\u{202c}b"),
                text("\u{202c}m"),
            ],
        );

        reorder_explicit_bidi_inline_rows(&mut line);
        let visual = line
            .children
            .iter()
            .filter_map(|child| match &child.kind {
                ComponentKind::Text { content } => Some(content.as_str()),
                _ => None,
            })
            .collect::<String>();
        assert_eq!(visual, "abcdefghijklm");
        assert!(line.children.iter().any(|child| {
            matches!(child.kind, ComponentKind::Text { ref content } if content == "fgh")
        }));
        let aqua_fragments = line
            .children
            .iter()
            .filter(|child| child.style.border_width == 3.0)
            .collect::<Vec<_>>();
        assert_eq!(aqua_fragments.len(), 7);
        assert_eq!(aqua_fragments[0].style.border_left_width, Some(3.0));
        assert_eq!(aqua_fragments[1].style.border_left_width, Some(3.0));
        assert_eq!(aqua_fragments[2].style.border_left_width, Some(0.0));
    }

    #[test]
    fn explicit_bidi_moves_collapsed_spaces_outside_decorated_fragments() {
        let plain = |content: &str| {
            let mut style = w3cos_std::style::Style::default();
            style.display = Display::InlineBlock;
            w3cos_std::Component::text(content, style)
        };
        let decorated = |content: &str| {
            let mut style = w3cos_std::style::Style::default();
            style.display = Display::Inline;
            style.border_width = 3.0;
            w3cos_std::Component::row(style, vec![plain(content)])
        };
        let mut line = w3cos_std::Component::row(
            w3cos_std::style::Style::default(),
            vec![
                decorated(" aaa bbb ccc \u{202e} lll kkk jjj "),
                plain(" iii hhh ggg "),
                decorated(" fff eee ddd \u{202c} mmm nnn ooo "),
            ],
        );

        reorder_explicit_bidi_inline_rows(&mut line);

        assert!(
            line.children
                .iter()
                .filter(|child| child.style.border_width > 0.0)
                .all(|child| matches!(
                    &child.kind,
                    ComponentKind::Text { content }
                        if content == content.trim_matches(is_css_whitespace)
                ))
        );
        assert!(line.children.iter().any(|child| matches!(
            &child.kind,
            ComponentKind::Text { content } if content == " " && child.style.border_width == 0.0
        )));
    }

    #[test]
    fn explicit_bidi_reorders_each_estimated_wrapped_line_independently() {
        let plain = |content: &str| {
            let mut style = w3cos_std::style::Style::default();
            style.display = Display::Inline;
            w3cos_std::Component::text(content, style)
        };
        let decorated = |content: &str| {
            let mut style = w3cos_std::style::Style::default();
            style.display = Display::Inline;
            style.padding.left = w3cos_std::style::Spacing::Px(16.0);
            style.padding.right = w3cos_std::style::Spacing::Px(16.0);
            style.border_width = 2.0;
            w3cos_std::Component::row(style, vec![plain(content)])
        };
        let mut line_style = w3cos_std::style::Style::default();
        line_style.width = w3cos_std::style::Dimension::Em(17.0);
        line_style.flex_wrap = w3cos_std::style::FlexWrap::Wrap;
        let mut line = w3cos_std::Component::row(
            line_style,
            vec![
                decorated("AAABBBCCC\u{202e}IIIHHHGGG"),
                plain("FFFEEEDDD LLLKKKJJJ\u{202c}MMMNNNOOO"),
            ],
        );

        reorder_explicit_bidi_inline_rows(&mut line);
        let visual = line
            .children
            .iter()
            .fold(String::new(), |mut visual, child| {
                if let ComponentKind::Text { content } = &child.kind {
                    visual.push_str(content);
                }
                visual
            });
        assert_eq!(visual, "AAABBBCCCDDDEEEFFFGGGHHHIIIJJJKKKLLLMMMNNNOOO");
    }

    #[test]
    fn ordinary_bidi_ignores_an_empty_painted_inline_box() {
        let mut inline_style = w3cos_std::style::Style::default();
        inline_style.display = Display::Inline;
        let mut empty_style = inline_style.clone();
        empty_style.background = w3cos_std::Color::WHITE;
        let mut line = w3cos_std::Component::row(
            w3cos_std::style::Style::default(),
            vec![
                w3cos_std::Component::text("א", inline_style.clone()),
                w3cos_std::Component::row(empty_style, vec![]),
                w3cos_std::Component::text("בג", inline_style),
            ],
        );

        reorder_explicit_bidi_inline_rows(&mut line);

        let logical = line
            .children
            .iter()
            .fold(String::new(), |mut logical, child| {
                if let ComponentKind::Text { content } = &child.kind {
                    logical.push_str(content);
                }
                logical
            });
        assert_eq!(logical, "אבג");
        assert_eq!(line.children.len(), 1);
        assert!(line.children.iter().all(|child| {
            !matches!(child.kind, ComponentKind::Row | ComponentKind::Box)
                || !child.children.is_empty()
        }));
    }

    #[test]
    fn rtl_text_keeps_a_trailing_flag_in_its_own_font_run() {
        let mut inline_style = w3cos_std::style::Style::default();
        inline_style.display = Display::Inline;
        let mut line = w3cos_std::Component::row(
            w3cos_std::style::Style::default(),
            vec![w3cos_std::Component::text("לום🇱🇮", inline_style)],
        );
        reorder_explicit_bidi_inline_rows(&mut line);
        assert_eq!(line.children.len(), 2);
        assert!(
            matches!(&line.children[0].kind, ComponentKind::Text { content } if content == "לום")
        );
        assert!(
            matches!(&line.children[1].kind, ComponentKind::Text { content } if content == "🇱🇮")
        );
    }

    #[test]
    fn css_bidi_override_reorders_block_inline_content() {
        let mut style = w3cos_std::style::Style::default();
        style.flex_direction = w3cos_std::style::FlexDirection::Row;
        style.direction = w3cos_std::style::TextDirection::Rtl;
        style.unicode_bidi = w3cos_std::style::UnicodeBidi::BidiOverride;
        let mut inline_style = w3cos_std::style::Style::default();
        inline_style.display = Display::Inline;
        let mut line = w3cos_std::Component::row(
            style,
            vec![
                w3cos_std::Component::text("dnoceS", inline_style.clone()),
                w3cos_std::Component::text(" tsriF", inline_style),
            ],
        );

        reorder_explicit_bidi_inline_rows(&mut line);

        let visual = if let ComponentKind::Text { content } = &line.kind {
            content.clone()
        } else {
            line.children
                .iter()
                .fold(String::new(), |mut visual, child| {
                    if let ComponentKind::Text { content } = &child.kind {
                        visual.push_str(content);
                    }
                    visual
                })
        };
        assert_eq!(visual, "First Second");
    }

    #[test]
    fn css_bidi_override_reorders_a_single_inline_text_child() {
        let mut style = w3cos_std::style::Style::default();
        style.display = Display::Inline;
        style.direction = w3cos_std::style::TextDirection::Rtl;
        style.unicode_bidi = w3cos_std::style::UnicodeBidi::BidiOverride;
        let mut inline = w3cos_std::Component::text("dnoceS", style);

        reorder_explicit_bidi_inline_rows(&mut inline);

        assert!(matches!(
            &inline.kind,
            ComponentKind::Text { content } if content == "Second"
        ));
        assert_eq!(inline.style.direction, w3cos_std::style::TextDirection::Ltr);
        assert_eq!(
            inline.style.unicode_bidi,
            w3cos_std::style::UnicodeBidi::Normal
        );
        assert_eq!(
            inline.style.text_align,
            w3cos_std::style::TextAlign::Right,
            "logical start must remain the RTL inline end after consuming the override"
        );
    }

    #[test]
    fn unicode_line_separator_keeps_one_bidi_paragraph_across_visual_lines() {
        let mut line = w3cos_std::Component::text(
            "א + - × ÷ \u{a0}\u{2028}\u{a0} + - × ÷ ת",
            w3cos_std::style::Style::default(),
        );

        reorder_explicit_bidi_inline_rows(&mut line);

        assert!(matches!(
            &line.kind,
            w3cos_std::ComponentKind::Text { content }
                if content == "\u{a0} ÷ × - + א\nת ÷ × - + \u{a0}"
        ));
        assert_eq!(line.style.direction, w3cos_std::style::TextDirection::Ltr);
    }

    #[test]
    fn nested_bidi_overrides_shape_as_one_passive_inline_run() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            ".rtol",
            &[("direction", "rtl"), ("unicode-bidi", "bidi-override")],
        );
        crate::stylesheet::register_rule(
            ".ltor",
            &[("direction", "ltr"), ("unicode-bidi", "bidi-override")],
        );
        let mut document = Document::new();
        let paragraph = document.create_element("p");
        let outer = document.create_element("span");
        outer.set_attribute(&mut document, "class", "rtol");
        let outer_before = document.create_text_node("ba");
        outer.append_child(&mut document, outer_before);
        let inner = document.create_element("span");
        inner.set_attribute(&mut document, "class", "ltor");
        let inner_text = document.create_text_node("ad");
        inner.append_child(&mut document, inner_text);
        outer.append_child(&mut document, inner);
        let outer_after = document.create_text_node("eR");
        outer.append_child(&mut document, outer_after);
        paragraph.append_child(&mut document, outer);
        let paragraph_after = document.create_text_node("le");
        paragraph.append_child(&mut document, paragraph_after);
        document.body().append_child(&mut document, paragraph);

        fn text_runs(component: &w3cos_std::Component, runs: &mut Vec<String>) {
            if let ComponentKind::Text { content } = &component.kind {
                runs.push(content.clone());
            }
            for child in &component.children {
                text_runs(child, runs);
            }
        }
        let tree = document.to_component_tree();
        let mut runs = Vec::new();
        text_runs(&tree, &mut runs);

        assert_eq!(runs, vec!["Readable"]);
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn rtl_embed_reorders_surrounding_ltr_runs_as_one_paragraph() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("div", &[("direction", "rtl")]);
        crate::stylesheet::register_rule("span", &[("unicode-bidi", "embed")]);
        let mut document = Document::new();
        let block = document.create_element("div");
        let before = document.create_text_node("IJ K ");
        block.append_child(&mut document, before);
        let embedded = document.create_element("span");
        let embedded_text = document.create_text_node("DEF GH");
        embedded.append_child(&mut document, embedded_text);
        block.append_child(&mut document, embedded);
        let after = document.create_text_node(" AB C");
        block.append_child(&mut document, after);
        document.body().append_child(&mut document, block);

        fn collect_text(component: &w3cos_std::Component, output: &mut String) {
            if let ComponentKind::Text { content } = &component.kind {
                output.push_str(content);
            }
            for child in &component.children {
                collect_text(child, output);
            }
        }
        let tree = document.to_component_tree();
        let mut visual = String::new();
        collect_text(&tree.children[0], &mut visual);
        assert_eq!(visual, "AB C DEF GH IJ K");
        assert_eq!(
            tree.children[0].children.len(),
            1,
            "the visually reordered paragraph must shape as one text run: {:#?}",
            tree.children[0].children
        );
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
mod computed_style_cache_tests {
    use super::*;

    #[test]
    fn cache_reuses_styles_and_observes_dom_and_stylesheet_version_fences() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(".active", &[("color", "red")]);

        let mut document = Document::new();
        let target = document.create_element("div");
        target.set_attribute(&mut document, "id", "target");
        target.class_list_add(&mut document, "active");
        document.body().append_child(&mut document, target);

        assert_eq!(
            document.computed_style_for(target.id).color,
            w3cos_std::Color::rgb(255, 0, 0)
        );
        let after_first = document.computed_style_cache_stats();
        assert_eq!(
            document.computed_style_for(target.id).color,
            w3cos_std::Color::rgb(255, 0, 0)
        );
        let after_second = document.computed_style_cache_stats();
        assert_eq!(after_second.1, after_first.1);
        assert!(after_second.0 > after_first.0);

        target.class_list_remove(&mut document, "active");
        assert_ne!(
            document.computed_style_for(target.id).color,
            w3cos_std::Color::rgb(255, 0, 0)
        );
        let after_dom_mutation = document.computed_style_cache_stats();
        assert!(after_dom_mutation.1 > after_second.1);

        crate::stylesheet::register_rule("#target", &[("font-size", "31px")]);
        assert_eq!(document.computed_style_for(target.id).font_size, 31.0);
        let after_stylesheet_mutation = document.computed_style_cache_stats();
        assert!(after_stylesheet_mutation.1 > after_dom_mutation.1);

        crate::stylesheet::clear_rules();
    }

    #[test]
    fn font_size_ex_resolves_against_the_inherited_font_metrics() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            "#parent",
            &[("font-size", "20px"), ("font-family", "Ahem")],
        );
        crate::stylesheet::register_rule("#child", &[("font-size", "2.5ex")]);

        let mut document = Document::new();
        let parent = document.create_element("div");
        parent.set_attribute(&mut document, "id", "parent");
        let child = document.create_element("div");
        child.set_attribute(&mut document, "id", "child");
        parent.append_child(&mut document, child);
        document.body().append_child(&mut document, parent);

        assert_eq!(document.computed_style_for(child.id).font_size, 40.0);
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn relative_font_size_is_preserved_on_the_principal_box() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            "#target",
            &[("font-size", "2.5em"), ("width", "17em"), ("height", "1em")],
        );
        let mut document = Document::new();
        let target = document.create_element("div");
        target.set_attribute(&mut document, "id", "target");
        document.body().append_child(&mut document, target);

        let style = document.computed_style_for(target.id);
        assert_eq!(style.font_size, 40.0);
        assert_eq!(style.width, w3cos_std::style::Dimension::Em(17.0));
        assert_eq!(style.height, w3cos_std::style::Dimension::Em(1.0));

        let tree = document.to_component_tree();
        let principal = &tree.children[0];
        assert_eq!(principal.style.font_size, 40.0);
        assert_eq!(principal.style.width, w3cos_std::style::Dimension::Em(17.0));
        assert_eq!(principal.style.height, w3cos_std::style::Dimension::Em(1.0));
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn unicode_bidi_inherit_uses_the_parent_computed_value() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("#parent", &[("unicode-bidi", "bidi-override")]);
        crate::stylesheet::register_rule("#child", &[("unicode-bidi", "inherit")]);
        let mut document = Document::new();
        let parent = document.create_element("div");
        parent.set_attribute(&mut document, "id", "parent");
        let child = document.create_element("div");
        child.set_attribute(&mut document, "id", "child");
        parent.append_child(&mut document, child);
        document.body().append_child(&mut document, parent);

        assert_eq!(
            document.computed_style_for(child.id).unicode_bidi,
            w3cos_std::style::UnicodeBidi::BidiOverride
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn visibility_inherits_but_an_explicit_visible_descendant_overrides_it() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule("#parent", &[("visibility", "hidden")]);
        crate::stylesheet::register_rule("#visible", &[("visibility", "visible")]);
        let mut document = Document::new();
        let parent = document.create_element("div");
        parent.set_attribute(&mut document, "id", "parent");
        let inherited = document.create_element("span");
        let visible = document.create_element("span");
        visible.set_attribute(&mut document, "id", "visible");
        parent.append_child(&mut document, inherited);
        parent.append_child(&mut document, visible);
        document.body().append_child(&mut document, parent);

        assert_eq!(
            document.computed_style_for(inherited.id).visibility,
            w3cos_std::style::Visibility::Hidden
        );
        assert_eq!(
            document.computed_style_for(visible.id).visibility,
            w3cos_std::style::Visibility::Visible
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn html_dir_hint_sets_direction_but_author_css_can_override_it() {
        crate::stylesheet::clear_rules();
        let mut document = Document::new();
        let target = document.create_element("div");
        target.set_attribute(&mut document, "id", "target");
        target.set_attribute(&mut document, "dir", "rtl");
        document.body().append_child(&mut document, target);

        assert_eq!(
            document.computed_style_for(target.id).direction,
            w3cos_std::style::TextDirection::Rtl
        );
        crate::stylesheet::register_rule("#target", &[("direction", "ltr")]);
        assert_eq!(
            document.computed_style_for(target.id).direction,
            w3cos_std::style::TextDirection::Ltr
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn relative_border_width_resolves_against_the_computed_font_size() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            "#target",
            &[("font-size", "20px"), ("border", "1em solid blue")],
        );

        let mut document = Document::new();
        let target = document.create_element("div");
        target.set_attribute(&mut document, "id", "target");
        document.body().append_child(&mut document, target);

        let style = document.computed_style_for(target.id);
        assert_eq!(style.border_width, 20.0);
        assert_eq!(style.border_top_width, Some(20.0));
        assert_eq!(style.border_right_width, Some(20.0));
        assert_eq!(style.border_bottom_width, Some(20.0));
        assert_eq!(style.border_left_width, Some(20.0));
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn later_same_specificity_rule_and_declaration_win_after_invalid_background() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            "p",
            &[
                ("color", "red"),
                ("border", "solid red"),
                ("background", "red url( { test )"),
                ("border", "solid green"),
            ],
        );
        crate::stylesheet::register_rule("p", &[("color", "green")]);

        let mut document = Document::new();
        let target = document.create_element("p");
        document.body().append_child(&mut document, target);

        let style = document.computed_style_for(target.id);
        let green = w3cos_std::Color::from_css("green").unwrap();
        assert_eq!(style.color, green);
        assert_eq!(style.border_color, green);
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn border_shorthand_inherit_copies_the_parent_used_border() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            "#parent",
            &[("border", "1em solid lime"), ("font-size", "20px")],
        );
        crate::stylesheet::register_rule("#child", &[("border", "inherit")]);

        let mut document = Document::new();
        let parent = document.create_element("div");
        parent.set_attribute(&mut document, "id", "parent");
        let child = document.create_element("div");
        child.set_attribute(&mut document, "id", "child");
        parent.append_child(&mut document, child);
        document.body().append_child(&mut document, parent);

        let parent_style = document.computed_style_for(parent.id);
        let child_style = document.computed_style_for(child.id);
        assert_eq!(child_style.border_width, parent_style.border_width);
        assert_eq!(child_style.border_top_width, parent_style.border_top_width);
        assert_eq!(child_style.border_color, parent_style.border_color);
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn unrelated_branches_keep_their_cached_styles_on_local_selector_mutation() {
        crate::stylesheet::clear_rules();
        let mut document = Document::new();
        let left = document.create_element("section");
        let leaf = document.create_element("span");
        left.append_child(&mut document, leaf);
        let right = document.create_element("aside");
        document.body().append_child(&mut document, left);
        document.body().append_child(&mut document, right);

        document.computed_style_for(right.id);
        assert!(document.computed_style_cache_is_current(right.id));
        leaf.class_list_add(&mut document, "changed");

        assert!(document.computed_style_cache_is_current(right.id));
    }

    #[test]
    fn inherited_style_mutation_invalidates_the_descendant_subtree() {
        crate::stylesheet::clear_rules();
        let mut document = Document::new();
        let parent = document.create_element("div");
        let child = document.create_element("span");
        parent.append_child(&mut document, child);
        document.body().append_child(&mut document, parent);

        document.computed_style_for(child.id);
        assert!(document.computed_style_cache_is_current(child.id));
        parent.style_mut(&mut document).set_property("color", "red");

        assert!(!document.computed_style_cache_is_current(child.id));
        assert_eq!(
            document.computed_style_for(child.id).color,
            w3cos_std::Color::rgb(255, 0, 0)
        );
    }

    #[test]
    fn sibling_selector_dependency_invalidates_the_parent_subtree() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(".trigger + .peer", &[("color", "red")]);
        let mut document = Document::new();
        let parent = document.create_element("div");
        let trigger = document.create_element("span");
        let peer = document.create_element("span");
        peer.class_list_add(&mut document, "peer");
        parent.append_child(&mut document, trigger);
        parent.append_child(&mut document, peer);
        document.body().append_child(&mut document, parent);

        document.computed_style_for(peer.id);
        assert!(document.computed_style_cache_is_current(peer.id));
        trigger.class_list_add(&mut document, "trigger");

        assert!(!document.computed_style_cache_is_current(peer.id));
        assert_eq!(
            document.computed_style_for(peer.id).color,
            w3cos_std::Color::rgb(255, 0, 0)
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn has_dependency_falls_back_to_document_scope_without_stale_ancestors() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(".host:has(.flag)", &[("color", "red")]);
        let mut document = Document::new();
        let host = document.create_element("div");
        host.class_list_add(&mut document, "host");
        let child = document.create_element("span");
        host.append_child(&mut document, child);
        document.body().append_child(&mut document, host);

        document.computed_style_for(host.id);
        assert!(document.computed_style_cache_is_current(host.id));
        child.class_list_add(&mut document, "flag");

        assert!(!document.computed_style_cache_is_current(host.id));
        assert_eq!(
            document.computed_style_for(host.id).color,
            w3cos_std::Color::rgb(255, 0, 0)
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn recycled_node_ids_never_reuse_the_previous_nodes_cached_style() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(".old", &[("color", "red")]);
        let mut document = Document::new();
        let old = document.create_element("div");
        old.class_list_add(&mut document, "old");
        document.body().append_child(&mut document, old);
        assert_eq!(
            document.computed_style_for(old.id).color,
            w3cos_std::Color::rgb(255, 0, 0)
        );

        document.remove_node(old.id);
        let replacement = document.create_element("div");
        assert_eq!(replacement.id, old.id);
        assert!(!document.computed_style_cache_is_current(replacement.id));
        assert_ne!(
            document.computed_style_for(replacement.id).color,
            w3cos_std::Color::rgb(255, 0, 0)
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn moving_a_node_invalidates_structural_styles_in_its_old_parent() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(".item:last-child", &[("color", "red")]);
        let mut document = Document::new();
        let old_parent = document.create_element("div");
        let new_parent = document.create_element("div");
        let first = document.create_element("span");
        first.class_list_add(&mut document, "item");
        let second = document.create_element("span");
        second.class_list_add(&mut document, "item");
        old_parent.append_child(&mut document, first);
        old_parent.append_child(&mut document, second);
        document.body().append_child(&mut document, old_parent);
        document.body().append_child(&mut document, new_parent);

        assert_ne!(
            document.computed_style_for(first.id).color,
            w3cos_std::Color::rgb(255, 0, 0)
        );
        new_parent.append_child(&mut document, second);

        assert!(!document.computed_style_cache_is_current(first.id));
        assert_eq!(
            document.computed_style_for(first.id).color,
            w3cos_std::Color::rgb(255, 0, 0)
        );
        crate::stylesheet::clear_rules();
    }

    #[test]
    fn text_mutation_invalidates_the_parents_empty_pseudo_class() {
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(".box:empty", &[("color", "red")]);
        let mut document = Document::new();
        let parent = document.create_element("div");
        parent.class_list_add(&mut document, "box");
        let text = document.create_text_node("content");
        parent.append_child(&mut document, text);
        document.body().append_child(&mut document, parent);

        assert_ne!(
            document.computed_style_for(parent.id).color,
            w3cos_std::Color::rgb(255, 0, 0)
        );
        text.set_text_content(&mut document, "");

        assert!(!document.computed_style_cache_is_current(parent.id));
        assert_eq!(
            document.computed_style_for(parent.id).color,
            w3cos_std::Color::rgb(255, 0, 0)
        );
        crate::stylesheet::clear_rules();
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
