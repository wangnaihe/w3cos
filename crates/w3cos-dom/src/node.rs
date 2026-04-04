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

    /// Convenience: get the tag as a &str (via atom lookup).
    pub fn tag_str(&self) -> String {
        self.tag.as_str()
    }

    /// Iterate over child NodeIds (follows first_child -> next_sibling chain).
    /// Requires access to the node arena to follow pointers.
    pub fn child_count_hint(&self) -> bool {
        self.first_child.is_some()
    }
}
