use crate::css_style::CSSStyleDeclaration;
use crate::element::Element;
use crate::events::EventRegistry;
use crate::node::{DomNode, NodeId, NodeType};

/// W3C Document — the root of the DOM tree.
///
/// Arena-allocated: all nodes live in a flat Vec, referenced by NodeId.
/// No Rc, no RefCell, no GC. O(1) node access.
pub struct Document {
    nodes: Vec<DomNode>,
    styles: Vec<CSSStyleDeclaration>,
    dirty: Vec<NodeId>,
    pub(crate) events: EventRegistry,
    body_id: NodeId,
}

impl Document {
    pub fn new() -> Self {
        let mut doc = Self {
            nodes: Vec::new(),
            styles: Vec::new(),
            dirty: Vec::new(),
            events: EventRegistry::new(),
            body_id: NodeId(0),
        };

        // Node 0: the document root
        let root_id = doc.alloc_node(DomNode {
            id: NodeId(0),
            node_type: NodeType::Document,
            tag: "#document".to_string(),
            text_content: None,
            parent: None,
            children: Vec::new(),
            attributes: Vec::new(),
            class_list: Vec::new(),
        });

        // Node 1: <body>
        let body_id = doc.alloc_node(DomNode::new_element(NodeId(1), "body"));
        doc.nodes[root_id.0 as usize].children.push(body_id);
        doc.nodes[body_id.0 as usize].parent = Some(root_id);
        doc.body_id = body_id;

        doc
    }

    // --- W3C Document API ---

    pub fn create_element(&mut self, tag: &str) -> Element {
        let id = NodeId(self.nodes.len() as u32);
        self.alloc_node(DomNode::new_element(id, tag));
        Element::new(id)
    }

    pub fn create_text_node(&mut self, content: &str) -> Element {
        let id = NodeId(self.nodes.len() as u32);
        self.alloc_node(DomNode::new_text(id, content));
        Element::new(id)
    }

    pub fn body(&self) -> Element {
        Element::new(self.body_id)
    }

    pub fn get_element_by_id(&self, id: &str) -> Option<Element> {
        self.nodes.iter().find(|n| {
            n.attributes.iter().any(|(k, v)| k == "id" && v == id)
        }).map(|n| Element::new(n.id))
    }

    pub fn query_selector(&self, selector: &str) -> Option<Element> {
        // Simplified: supports #id, .class, tag selectors
        if let Some(id) = selector.strip_prefix('#') {
            return self.get_element_by_id(id);
        }
        if let Some(class) = selector.strip_prefix('.') {
            return self.nodes.iter().find(|n| {
                n.class_list.contains(&class.to_string())
            }).map(|n| Element::new(n.id));
        }
        // Tag selector
        self.nodes.iter().find(|n| {
            n.tag == selector && n.node_type == NodeType::Element
        }).map(|n| Element::new(n.id))
    }

    pub fn query_selector_all(&self, selector: &str) -> Vec<Element> {
        if let Some(class) = selector.strip_prefix('.') {
            return self.nodes.iter()
                .filter(|n| n.class_list.contains(&class.to_string()))
                .map(|n| Element::new(n.id))
                .collect();
        }
        if let Some(id) = selector.strip_prefix('#') {
            return self.get_element_by_id(id).into_iter().collect();
        }
        self.nodes.iter()
            .filter(|n| n.tag == selector && n.node_type == NodeType::Element)
            .map(|n| Element::new(n.id))
            .collect()
    }

    // --- Tree Operations ---

    pub fn append_child(&mut self, parent: NodeId, child: NodeId) {
        // Remove from old parent if any
        if let Some(old_parent) = self.nodes[child.0 as usize].parent {
            self.nodes[old_parent.0 as usize].children.retain(|&id| id != child);
        }
        self.nodes[parent.0 as usize].children.push(child);
        self.nodes[child.0 as usize].parent = Some(parent);
        self.mark_dirty(parent);
    }

    pub fn remove_child(&mut self, parent: NodeId, child: NodeId) {
        self.nodes[parent.0 as usize].children.retain(|&id| id != child);
        self.nodes[child.0 as usize].parent = None;
        self.mark_dirty(parent);
    }

    pub fn insert_before(&mut self, parent: NodeId, new_child: NodeId, ref_child: NodeId) {
        if let Some(old_parent) = self.nodes[new_child.0 as usize].parent {
            self.nodes[old_parent.0 as usize].children.retain(|&id| id != new_child);
        }
        let children = &mut self.nodes[parent.0 as usize].children;
        if let Some(pos) = children.iter().position(|&id| id == ref_child) {
            children.insert(pos, new_child);
        } else {
            children.push(new_child);
        }
        self.nodes[new_child.0 as usize].parent = Some(parent);
        self.mark_dirty(parent);
    }

    // --- Internal ---

    fn alloc_node(&mut self, node: DomNode) -> NodeId {
        let id = node.id;
        self.nodes.push(node);
        self.styles.push(CSSStyleDeclaration::new());
        id
    }

    pub fn get_node(&self, id: NodeId) -> &DomNode {
        &self.nodes[id.0 as usize]
    }

    pub fn get_node_mut(&mut self, id: NodeId) -> &mut DomNode {
        &mut self.nodes[id.0 as usize]
    }

    pub fn get_style(&self, id: NodeId) -> &CSSStyleDeclaration {
        &self.styles[id.0 as usize]
    }

    pub fn get_style_mut(&mut self, id: NodeId) -> &mut CSSStyleDeclaration {
        &mut self.styles[id.0 as usize]
    }

    pub fn mark_dirty(&mut self, id: NodeId) {
        if !self.dirty.contains(&id) {
            self.dirty.push(id);
        }
    }

    pub fn take_dirty(&mut self) -> Vec<NodeId> {
        std::mem::take(&mut self.dirty)
    }

    pub fn is_dirty(&self) -> bool {
        !self.dirty.is_empty()
    }

    /// Convert the DOM tree to a w3cos-std Component tree for rendering.
    pub fn to_component_tree(&self) -> w3cos_std::Component {
        self.node_to_component(self.body_id)
    }

    fn node_to_component(&self, id: NodeId) -> w3cos_std::Component {
        let node = &self.nodes[id.0 as usize];
        let style = self.styles[id.0 as usize].to_style();

        match node.node_type {
            NodeType::Text => {
                let text = node.text_content.as_deref().unwrap_or("");
                w3cos_std::Component::text(text, style)
            }
            NodeType::Element | NodeType::Document => {
                let children: Vec<w3cos_std::Component> = node.children.iter()
                    .map(|&child_id| self.node_to_component(child_id))
                    .collect();

                if let Some(text) = &node.text_content {
                    if children.is_empty() {
                        return match node.tag.as_str() {
                            "button" | "a" => w3cos_std::Component::button(text, style),
                            _ => w3cos_std::Component::text(text, style),
                        };
                    }
                }

                match node.tag.as_str() {
                    "body" | "div" | "section" | "main" | "article" | "nav" | "header" | "footer" =>
                        w3cos_std::Component::column(style, children),
                    "span" | "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                        if let Some(text) = &node.text_content {
                            w3cos_std::Component::text(text, style)
                        } else {
                            w3cos_std::Component::column(style, children)
                        }
                    }
                    "button" => {
                        let label = node.text_content.as_deref().unwrap_or("Button");
                        w3cos_std::Component::button(label, style)
                    }
                    _ => w3cos_std::Component::column(style, children),
                }
            }
        }
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}
