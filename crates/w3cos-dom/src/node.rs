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
    CdataSection,
    ProcessingInstruction,
    Document,
    DocumentFragment,
    Comment,
    DocumentType,
}

impl NodeType {
    /// W3C `Node.nodeType` numeric constant.
    pub fn as_u16(self) -> u16 {
        match self {
            NodeType::Element => 1,
            NodeType::Text => 3,
            NodeType::CdataSection => 4,
            NodeType::ProcessingInstruction => 7,
            NodeType::Comment => 8,
            NodeType::Document => 9,
            NodeType::DocumentType => 10,
            NodeType::DocumentFragment => 11,
        }
    }
}

/// Namespace identity retained for an attribute whose qualified name alone is
/// insufficient for the DOM namespace APIs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributeNamespace {
    pub attribute_index: usize,
    pub qualified_name: Atom,
    pub namespace: Option<Atom>,
    pub prefix: Option<Atom>,
    pub local_name: Atom,
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
    pub attribute_namespaces: Vec<AttributeNamespace>,
    pub class_list: Vec<Atom>,
    /// Whether CSS selector name matching follows the HTML element rules.
    /// Foreign SVG/MathML and XML elements keep authored name casing.
    pub is_html_element: bool,
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
            attribute_namespaces: Vec::new(),
            class_list: Vec::new(),
            is_html_element: true,
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
            attribute_namespaces: Vec::new(),
            class_list: Vec::new(),
            is_html_element: false,
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
            attribute_namespaces: Vec::new(),
            class_list: Vec::new(),
            is_html_element: false,
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
            attribute_namespaces: Vec::new(),
            class_list: Vec::new(),
            is_html_element: false,
        }
    }

    pub fn new_cdata_section(id: NodeId, content: impl Into<String>) -> Self {
        Self {
            id,
            node_type: NodeType::CdataSection,
            tag: Atom::intern("#cdata-section"),
            text_content: Some(content.into()),
            parent: None,
            first_child: None,
            last_child: None,
            next_sibling: None,
            prev_sibling: None,
            attributes: Vec::new(),
            attribute_namespaces: Vec::new(),
            class_list: Vec::new(),
            is_html_element: false,
        }
    }

    pub fn new_processing_instruction(
        id: NodeId,
        target: impl AsRef<str>,
        data: impl Into<String>,
    ) -> Self {
        Self {
            id,
            node_type: NodeType::ProcessingInstruction,
            tag: Atom::intern(target.as_ref()),
            text_content: Some(data.into()),
            parent: None,
            first_child: None,
            last_child: None,
            next_sibling: None,
            prev_sibling: None,
            attributes: Vec::new(),
            attribute_namespaces: Vec::new(),
            class_list: Vec::new(),
            is_html_element: false,
        }
    }

    pub fn new_document_type(id: NodeId, name: impl AsRef<str>) -> Self {
        Self {
            id,
            node_type: NodeType::DocumentType,
            tag: Atom::intern(name.as_ref()),
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
            NodeType::CdataSection => "#cdata-section".to_string(),
            NodeType::ProcessingInstruction | NodeType::DocumentType => self.tag.as_str(),
            NodeType::Comment => "#comment".to_string(),
            NodeType::Document => "#document".to_string(),
            NodeType::DocumentFragment => "#document-fragment".to_string(),
        }
    }

    pub fn child_count_hint(&self) -> bool {
        self.first_child.is_some()
    }

    // ── contenteditable ───────────────────────────────────────────────────

    /// W3C `HTMLElement.contentEditable` — "true" | "false" | "inherit".
    pub fn content_editable(&self) -> &str {
        let ce = Atom::intern("contenteditable");
        for (k, v) in &self.attributes {
            if *k == ce {
                return v.as_str();
            }
        }
        "inherit"
    }

    /// Returns true if this element is editable (contenteditable="true" or "").
    pub fn is_content_editable(&self) -> bool {
        matches!(self.content_editable(), "true" | "")
    }

    /// Set `contenteditable` attribute.
    pub fn set_content_editable(&mut self, value: &str) {
        let ce = Atom::intern("contenteditable");
        if let Some(index) = self.attributes.iter().position(|(name, _)| *name == ce) {
            self.clear_attribute_namespace(index);
            self.attributes[index].1 = value.to_string();
            return;
        }
        self.attributes.push((ce, value.to_string()));
    }

    /// Get an attribute value by name.
    pub fn get_attribute(&self, name: &str) -> Option<&str> {
        let key = Atom::intern(name);
        self.attributes
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Set an attribute value by name.
    pub fn set_attribute(&mut self, name: &str, value: &str) {
        let key = Atom::intern(name);
        if let Some(index) = self.attributes.iter().position(|(name, _)| *name == key) {
            // DOM setAttribute updates the first attribute with the matching
            // qualified name without changing that Attr node's namespace.
            self.attributes[index].1 = value.to_string();
            return;
        }
        self.attributes.push((key, value.to_string()));
    }

    /// Set an attribute while retaining the namespace identity required by
    /// `getAttributeNS`, `Attr.namespaceURI`, and foreign-content parsing.
    pub fn set_attribute_ns(
        &mut self,
        namespace: Option<&str>,
        qualified_name: &str,
        prefix: Option<&str>,
        local_name: &str,
        value: &str,
    ) {
        let new_qualified_name = Atom::intern(qualified_name);
        let namespace_atom = namespace.map(Atom::intern);
        let local_name_atom = Atom::intern(local_name);
        let existing_index = self
            .attribute_namespaces
            .iter()
            .find(|attribute| {
                attribute.namespace == namespace_atom && attribute.local_name == local_name_atom
            })
            .map(|attribute| attribute.attribute_index)
            .or_else(|| {
                namespace
                    .is_none()
                    .then(|| {
                        self.attributes
                            .iter()
                            .enumerate()
                            .find(|(index, (name, _))| {
                                *name == new_qualified_name
                                    && self.attribute_namespace_at(*index).is_none()
                            })
                            .map(|(index, _)| index)
                    })
                    .flatten()
            });
        let attribute_index = if let Some(existing_index) = existing_index {
            // DOM's "set an attribute value" algorithm updates the existing
            // Attr's value when namespace/local-name identify it.  The Attr's
            // qualified name (and therefore its original prefix) is stable.
            self.attributes[existing_index].1 = value.to_string();
            return;
        } else {
            self.attributes
                .push((new_qualified_name, value.to_string()));
            self.attributes.len() - 1
        };
        self.set_attribute_namespace_metadata(
            attribute_index,
            new_qualified_name,
            namespace,
            prefix,
            local_name,
        );
    }

    fn set_attribute_namespace_metadata(
        &mut self,
        attribute_index: usize,
        qualified_name: Atom,
        namespace: Option<&str>,
        prefix: Option<&str>,
        local_name: &str,
    ) {
        self.clear_attribute_namespace(attribute_index);
        self.attribute_namespaces.push(AttributeNamespace {
            attribute_index,
            qualified_name,
            namespace: namespace.map(Atom::intern),
            prefix: prefix.map(Atom::intern),
            local_name: Atom::intern(local_name),
        });
    }

    /// Get an attribute by namespace URI and local name.
    pub fn get_attribute_ns(&self, namespace: Option<&str>, local_name: &str) -> Option<&str> {
        let namespace = namespace.map(Atom::intern);
        let local_name = Atom::intern(local_name);
        if let Some(metadata) = self.attribute_namespaces.iter().find(|attribute| {
            attribute.namespace == namespace && attribute.local_name == local_name
        }) {
            return self
                .attributes
                .get(metadata.attribute_index)
                .map(|(_, value)| value.as_str());
        }
        if namespace.is_none() {
            return self
                .attributes
                .iter()
                .enumerate()
                .find(|(index, (name, _))| {
                    *name == local_name && self.attribute_namespace_at(*index).is_none()
                })
                .map(|(_, (_, value))| value.as_str());
        }
        None
    }

    pub fn attribute_namespace_at(&self, attribute_index: usize) -> Option<&AttributeNamespace> {
        self.attribute_namespaces
            .iter()
            .find(|attribute| attribute.attribute_index == attribute_index)
    }

    /// Remove an attribute by name. Returns true if it existed.
    pub fn remove_attribute(&mut self, name: &str) -> bool {
        let key = Atom::intern(name);
        let Some(index) = self.attributes.iter().position(|(name, _)| *name == key) else {
            return false;
        };
        self.remove_attribute_at(index);
        true
    }

    /// Remove an attribute by namespace URI and local name.
    pub fn remove_attribute_ns(&mut self, namespace: Option<&str>, local_name: &str) -> bool {
        let namespace = namespace.map(Atom::intern);
        let local_name = Atom::intern(local_name);
        let attribute_index = self
            .attribute_namespaces
            .iter()
            .find(|attribute| {
                attribute.namespace == namespace && attribute.local_name == local_name
            })
            .map(|attribute| attribute.attribute_index)
            .or_else(|| {
                namespace
                    .is_none()
                    .then(|| {
                        self.attributes
                            .iter()
                            .enumerate()
                            .find(|(index, (name, _))| {
                                *name == local_name && self.attribute_namespace_at(*index).is_none()
                            })
                            .map(|(index, _)| index)
                    })
                    .flatten()
            });
        let Some(attribute_index) = attribute_index else {
            return false;
        };
        self.remove_attribute_at(attribute_index);
        true
    }

    pub(crate) fn clear_attribute_namespace(&mut self, attribute_index: usize) {
        self.attribute_namespaces
            .retain(|attribute| attribute.attribute_index != attribute_index);
    }

    pub(crate) fn remove_attribute_at(&mut self, attribute_index: usize) {
        self.attributes.remove(attribute_index);
        self.clear_attribute_namespace(attribute_index);
        for attribute in &mut self.attribute_namespaces {
            if attribute.attribute_index > attribute_index {
                attribute.attribute_index -= 1;
            }
        }
    }
}
