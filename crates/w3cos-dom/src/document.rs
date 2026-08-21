use std::collections::HashMap;

use crate::atom::Atom;
use crate::css_style::CSSStyleDeclaration;
use crate::dom_rect::DOMRect;
use crate::element::Element;
use crate::events::EventRegistry;
use crate::node::{DomNode, NodeId, NodeType};
use crate::selection::{Range, Selection};
use crate::stylesheet;
use crate::user_agent;

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
                n.attribute_namespaces = node.attribute_namespaces.clone();
                n.class_list = node.class_list.clone();
                n.text_content = node.text_content.clone();
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
        let mut ancestors = Vec::new();
        self.node_to_component(self.body_id, &mut ancestors, None)
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

    /// Selector context for stylesheet matching: tag, id, classes, attributes.
    fn selector_context(&self, id: NodeId) -> stylesheet::SelectorContext {
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
        stylesheet::SelectorContext::new(&node.tag.as_str(), id_attr, &class_refs)
            .with_attributes(&attribute_refs)
    }

    /// Computed style for a node: stylesheet-matched declarations first
    /// (ascending specificity, then registration order), inline style on top.
    /// Falls back to the raw inline style when no stylesheet rules apply.
    fn computed_style(
        &self,
        id: NodeId,
        ancestors: &[stylesheet::SelectorContext],
        inherited: Option<&w3cos_std::style::Style>,
    ) -> w3cos_std::style::Style {
        let inline = &self.styles[id.0 as usize];
        let node = self.get_node(id);
        let matched = if stylesheet::has_rules() && node.node_type == NodeType::Element {
            let context = self.selector_context(id);
            stylesheet::matching_declarations_for_context(&context, ancestors)
        } else {
            Vec::new()
        };
        let mut merged =
            CSSStyleDeclaration::from_style(user_agent::html_default_style(&node.tag.as_str()));
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
                matched
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
                w3cos_std::Component::text(text, style)
            }
            NodeType::Comment | NodeType::DocumentType => {
                return w3cos_std::Component::column(style, vec![]);
            }
            NodeType::Element | NodeType::Document | NodeType::DocumentFragment => {
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
                {
                    let child = self.get_node(child_ids[0]);
                    if child.node_type == NodeType::Text {
                        let text = child.text_content.as_deref().unwrap_or("");
                        let component = match tag.as_str() {
                            "button" | "a" => w3cos_std::Component::button(text, style),
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
                let children: Vec<w3cos_std::Component> = child_ids
                    .iter()
                    .map(|&child_id| self.node_to_component(child_id, ancestors, Some(&style)))
                    .collect();
                if pushed {
                    ancestors.pop();
                }

                if let Some(text) = &node.text_content {
                    if children.is_empty() {
                        let component = match tag.as_str() {
                            "button" | "a" => w3cos_std::Component::button(text, style),
                            _ => w3cos_std::Component::text(text, style),
                        };
                        return self.attach_native_host(id, component);
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
                                .and_then(|(_, value)| value.parse::<f32>().ok())
                                .filter(|value| *value >= 0.0)
                        {
                            image_style.width = w3cos_std::style::Dimension::Px(width);
                        }
                        if matches!(image_style.height, w3cos_std::style::Dimension::Auto)
                            && let Some(height) = node
                                .attributes
                                .iter()
                                .find(|(key, _)| key.as_str() == "height")
                                .and_then(|(_, value)| value.parse::<f32>().ok())
                                .filter(|value| *value >= 0.0)
                        {
                            image_style.height = w3cos_std::style::Dimension::Px(height);
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
    if !declares("font-size") && !form_control && !heading {
        style.font_size = parent.font_size;
    }
    if !declares("font-weight") && !heading && !matches!(tag, "b" | "strong") {
        style.font_weight = parent.font_weight;
    }
    if !declares("font-family") {
        style.font_family = parent.font_family.clone();
    }
    if !declares("font-style") && !matches!(tag, "em" | "i") {
        style.font_style = parent.font_style;
    }
    if !declares("line-height") {
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

#[cfg(test)]
mod image_component_tests {
    use super::*;
    use w3cos_std::component::ComponentKind;
    use w3cos_std::style::Dimension;

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
