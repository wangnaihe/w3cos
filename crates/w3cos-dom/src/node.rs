/// Unique identifier for a DOM node within a Document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub(crate) u32);

impl NodeId {
    pub const ROOT: Self = Self(0);

    pub fn as_u32(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeType {
    Element,
    Text,
    Document,
}

/// Internal node storage for the DOM tree.
/// The tree is arena-allocated: all nodes live in a Vec<DomNode> and reference
/// each other by NodeId. No Rc/RefCell, no GC, no runtime overhead.
#[derive(Debug, Clone)]
pub struct DomNode {
    pub id: NodeId,
    pub node_type: NodeType,
    pub tag: String,
    pub text_content: Option<String>,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub attributes: Vec<(String, String)>,
    pub class_list: Vec<String>,
}

impl DomNode {
    pub fn new_element(id: NodeId, tag: impl Into<String>) -> Self {
        Self {
            id,
            node_type: NodeType::Element,
            tag: tag.into(),
            text_content: None,
            parent: None,
            children: Vec::new(),
            attributes: Vec::new(),
            class_list: Vec::new(),
        }
    }

    pub fn new_text(id: NodeId, content: impl Into<String>) -> Self {
        Self {
            id,
            node_type: NodeType::Text,
            tag: "#text".to_string(),
            text_content: Some(content.into()),
            parent: None,
            children: Vec::new(),
            attributes: Vec::new(),
            class_list: Vec::new(),
        }
    }
}
