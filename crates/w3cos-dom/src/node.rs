use crate::atom::Atom;

/// Unique identifier for a DOM node within a Document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub(crate) u32);

impl NodeId {
    pub const ROOT: Self = Self(0);

    pub fn as_u32(self) -> u32 {
        self.0
    }

    pub fn from_u32(v: u32) -> Self {
        Self(v)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeType {
    Element,
    Text,
    Document,
    DocumentFragment,
    Comment,
}

impl NodeType {
    /// W3C `Node.nodeType` numeric constant.
    pub fn as_u16(self) -> u16 {
        match self {
            NodeType::Element => 1,
            NodeType::Text => 3,
            NodeType::Comment => 8,
            NodeType::Document => 9,
            NodeType::DocumentFragment => 11,
        }
    }
}

/// Internal node storage for the DOM tree.
///
/// Uses Left-Child Right-Sibling (LCRS) tree structure (like Chrome/Blink):
/// - O(1) append_child, remove_child, insert_before
/// - Iterate children: first_child -> next_sibling chain
///
/// Uses interned Atoms for tag/attribute/class names:
/// - O(1) string comparison (integer equality)
/// - No heap allocation for common strings
#[derive(Debug, Clone)]
pub struct DomNode {
    pub id: NodeId,
    pub node_type: NodeType,
    pub tag: Atom,
    pub text_content: Option<String>,
    // LCRS tree pointers (replaces children: Vec<NodeId>)
    pub parent: Option<NodeId>,
    pub first_child: Option<NodeId>,
    pub last_child: Option<NodeId>,
    pub next_sibling: Option<NodeId>,
    pub prev_sibling: Option<NodeId>,
    // Attributes: key is Atom (interned), value is String
    pub attributes: Vec<(Atom, String)>,
    pub class_list: Vec<Atom>,
}

impl DomNode {
    pub fn new_element(id: NodeId, tag: impl AsRef<str>) -> Self {
        Self {
            id,
            node_type: NodeType::Element,
            tag: Atom::intern(tag.as_ref()),
            text_content: None,
            parent: None,
            first_child: None,
            last_child: None,
            next_sibling: None,
            prev_sibling: None,
            attributes: Vec::new(),
            class_list: Vec::new(),
        }
    }

    pub fn new_text(id: NodeId, content: impl Into<String>) -> Self {
        Self {
            id,
            node_type: NodeType::Text,
            tag: Atom::intern("#text"),
            text_content: Some(content.into()),
            parent: None,
            first_child: None,
            last_child: None,
            next_sibling: None,
            prev_sibling: None,
            attributes: Vec::new(),
            class_list: Vec::new(),
        }
    }

    pub fn new_document_fragment(id: NodeId) -> Self {
        Self {
            id,
            node_type: NodeType::DocumentFragment,
            tag: Atom::intern("#document-fragment"),
            text_content: None,
            parent: None,
            first_child: None,
            last_child: None,
            next_sibling: None,
            prev_sibling: None,
            attributes: Vec::new(),
            class_list: Vec::new(),
        }
    }

    pub fn new_comment(id: NodeId, content: impl Into<String>) -> Self {
        Self {
            id,
            node_type: NodeType::Comment,
            tag: Atom::intern("#comment"),
            text_content: Some(content.into()),
            parent: None,
            first_child: None,
            last_child: None,
            next_sibling: None,
            prev_sibling: None,
            attributes: Vec::new(),
            class_list: Vec::new(),
        }
    }

    pub fn tag_str(&self) -> String {
        self.tag.as_str()
    }

    /// W3C `Node.nodeName`.
    pub fn node_name(&self) -> String {
        match self.node_type {
            NodeType::Element => self.tag.as_str().to_ascii_uppercase(),
            NodeType::Text => "#text".to_string(),
            NodeType::Comment => "#comment".to_string(),
            NodeType::Document => "#document".to_string(),
            NodeType::DocumentFragment => "#document-fragment".to_string(),
        }
    }

    pub fn child_count_hint(&self) -> bool {
        self.first_child.is_some()
    }
}
