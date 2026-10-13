use std::ops::Deref;

use crate::atom::Atom;

/// Short DOM strings live in the node. Longer ones spill to the heap.
#[derive(Clone, Debug)]
pub struct CompactStr {
    inline: [u8; 22],
    /// `255` means `heap` owns the bytes.
    len: u8,
    heap: String,
}

impl CompactStr {
    pub fn new() -> Self {
        Self {
            inline: [0; 22],
            len: 0,
            heap: String::new(),
        }
    }

    pub fn set(&mut self, text: &str) {
        if text.len() <= 22 {
            self.heap = String::new();
            self.inline[..text.len()].copy_from_slice(text.as_bytes());
            self.len = text.len() as u8;
        } else {
            self.len = 255;
            self.heap = text.to_string();
        }
    }

    pub fn push_str(&mut self, extra: &str) {
        if self.len != 255 {
            let current = self.len as usize;
            if current + extra.len() <= 22 {
                self.inline[current..current + extra.len()].copy_from_slice(extra.as_bytes());
                self.len = (current + extra.len()) as u8;
                return;
            }
            let mut owned = self.as_str().to_string();
            owned.push_str(extra);
            self.len = 255;
            self.heap = owned;
            return;
        }
        self.heap.push_str(extra);
    }

    pub fn as_str(&self) -> &str {
        if self.len == 255 {
            self.heap.as_str()
        } else {
            std::str::from_utf8(&self.inline[..self.len as usize]).unwrap_or("")
        }
    }
}

impl Default for CompactStr {
    fn default() -> Self {
        Self::new()
    }
}

impl Deref for CompactStr {
    type Target = str;

    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl std::fmt::Display for CompactStr {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl PartialEq<str> for CompactStr {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for CompactStr {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl From<&str> for CompactStr {
    fn from(text: &str) -> Self {
        let mut value = Self::new();
        value.set(text);
        value
    }
}

impl From<String> for CompactStr {
    fn from(text: String) -> Self {
        if text.len() <= 22 {
            Self::from(text.as_str())
        } else {
            Self {
                inline: [0; 22],
                len: 255,
                heap: text,
            }
        }
    }
}

impl From<&CompactStr> for String {
    fn from(text: &CompactStr) -> Self {
        text.as_str().to_string()
    }
}

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

/// Sibling link. `0` is none — the document root is never a sibling, so the
/// link is a raw `u32` instead of `Option<NodeId>`.
#[inline]
pub fn sib(link: u32) -> Option<NodeId> {
    if link == 0 {
        None
    } else {
        Some(NodeId(link))
    }
}

#[inline]
pub fn sib_of(id: Option<NodeId>) -> u32 {
    id.map(|id| id.0).unwrap_or(0)
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

/// Attribute value owned by the atom arena. Copying the value does not allocate.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct AttrValue(Atom);

impl AttrValue {
    pub const EMPTY: Self = Self(Atom::EMPTY);

    pub fn new(value: &str) -> Self {
        Self(Atom::intern(value))
    }

    pub(crate) fn from_atom(atom: Atom) -> Self {
        Self(atom)
    }

    pub fn atom(self) -> Atom {
        self.0
    }

    pub fn as_str(&self) -> &'static str {
        self.0.as_static()
    }
}

impl Default for AttrValue {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl std::ops::Deref for AttrValue {
    type Target = str;

    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl PartialEq<str> for AttrValue {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for AttrValue {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl std::fmt::Debug for AttrValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl serde::Serialize for AttrValue {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

const INLINE_ATTRS: usize = 2;
const INLINE_CLASSES: usize = 2;

/// At most two attributes live in the node. A third spills to the heap.
#[derive(Clone, Debug)]
pub struct AttrList {
    inline: [(Atom, AttrValue); INLINE_ATTRS],
    len: u8,
    spill: Vec<(Atom, AttrValue)>,
}

impl Default for AttrList {
    fn default() -> Self {
        Self::new()
    }
}

impl AttrList {
    pub fn new() -> Self {
        Self {
            inline: [(Atom::EMPTY, AttrValue::EMPTY); INLINE_ATTRS],
            len: 0,
            spill: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.len as usize + self.spill.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn push(&mut self, item: (Atom, AttrValue)) {
        if self.spill.is_empty() && (self.len as usize) < INLINE_ATTRS {
            self.inline[self.len as usize] = item;
            self.len += 1;
            return;
        }
        self.spill.push(item);
    }

    pub fn get(&self, index: usize) -> Option<&(Atom, AttrValue)> {
        if index < self.len() {
            Some(&self[index])
        } else {
            None
        }
    }

    pub fn iter(&self) -> AttrIter<'_> {
        AttrIter {
            list: self,
            index: 0,
        }
    }

    pub fn remove(&mut self, index: usize) -> (Atom, AttrValue) {
        assert!(index < self.len(), "attribute index");
        if self.spill.is_empty() {
            let removed = self.inline[index];
            let last = self.len as usize;
            for slot in index + 1..last {
                self.inline[slot - 1] = self.inline[slot];
            }
            self.len -= 1;
            return removed;
        }
        if index < self.len as usize {
            let removed = self.inline[index];
            let last = self.len as usize - 1;
            for slot in index..last {
                self.inline[slot] = self.inline[slot + 1];
            }
            self.inline[last] = self.spill.remove(0);
            return removed;
        }
        self.spill.remove(index - self.len as usize)
    }
}

pub struct AttrIter<'a> {
    list: &'a AttrList,
    index: usize,
}

impl<'a> Iterator for AttrIter<'a> {
    type Item = &'a (Atom, AttrValue);

    fn next(&mut self) -> Option<Self::Item> {
        if self.index < self.list.len as usize {
            let item = &self.list.inline[self.index];
            self.index += 1;
            return Some(item);
        }
        let spill = self.index - self.list.len as usize;
        if spill < self.list.spill.len() {
            self.index += 1;
            return Some(&self.list.spill[spill]);
        }
        None
    }
}

impl<'a> IntoIterator for &'a AttrList {
    type Item = &'a (Atom, AttrValue);
    type IntoIter = AttrIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl std::ops::Index<usize> for AttrList {
    type Output = (Atom, AttrValue);

    fn index(&self, index: usize) -> &Self::Output {
        if index < self.len as usize {
            &self.inline[index]
        } else {
            &self.spill[index - self.len as usize]
        }
    }
}

impl std::ops::IndexMut<usize> for AttrList {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        if index < self.len as usize {
            &mut self.inline[index]
        } else {
            &mut self.spill[index - self.len as usize]
        }
    }
}

/// One or two classes live in the node. More spill to the heap.
#[derive(Clone, Debug)]
pub struct ClassList {
    inline: [Atom; INLINE_CLASSES],
    len: u8,
    spill: Vec<Atom>,
}

impl Default for ClassList {
    fn default() -> Self {
        Self::new()
    }
}

impl ClassList {
    pub fn new() -> Self {
        Self {
            inline: [Atom::EMPTY; INLINE_CLASSES],
            len: 0,
            spill: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.len as usize + self.spill.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, index: usize) -> Option<&Atom> {
        if index < self.len as usize {
            Some(&self.inline[index])
        } else {
            self.spill.get(index - self.len as usize)
        }
    }

    pub fn contains(&self, atom: &Atom) -> bool {
        self.iter().any(|class| class == atom)
    }

    pub fn push(&mut self, atom: Atom) {
        if self.spill.is_empty() && (self.len as usize) < INLINE_CLASSES {
            self.inline[self.len as usize] = atom;
            self.len += 1;
            return;
        }
        self.spill.push(atom);
    }

    pub fn retain(&mut self, mut keep: impl FnMut(&Atom) -> bool) {
        if self.spill.is_empty() {
            let mut write = 0usize;
            for read in 0..self.len as usize {
                if keep(&self.inline[read]) {
                    self.inline[write] = self.inline[read];
                    write += 1;
                }
            }
            self.len = write as u8;
            return;
        }
        let mut kept = Vec::new();
        for class in self.iter() {
            if keep(class) {
                kept.push(*class);
            }
        }
        *self = Self::new();
        for class in kept {
            self.push(class);
        }
    }

    pub fn iter(&self) -> ClassIter<'_> {
        ClassIter {
            list: self,
            index: 0,
        }
    }
}

pub struct ClassIter<'a> {
    list: &'a ClassList,
    index: usize,
}

impl<'a> Iterator for ClassIter<'a> {
    type Item = &'a Atom;

    fn next(&mut self) -> Option<Self::Item> {
        let item = self.list.get(self.index)?;
        self.index += 1;
        Some(item)
    }
}

impl<'a> IntoIterator for &'a ClassList {
    type Item = &'a Atom;
    type IntoIter = ClassIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl IntoIterator for ClassList {
    type Item = Atom;
    type IntoIter = ClassIntoIter;

    fn into_iter(self) -> Self::IntoIter {
        ClassIntoIter {
            inline: self.inline,
            len: self.len,
            index: 0,
            spill: self.spill.into_iter(),
        }
    }
}

pub struct ClassIntoIter {
    inline: [Atom; INLINE_CLASSES],
    len: u8,
    index: u8,
    spill: std::vec::IntoIter<Atom>,
}

impl Iterator for ClassIntoIter {
    type Item = Atom;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index < self.len {
            let atom = self.inline[self.index as usize];
            self.index += 1;
            return Some(atom);
        }
        self.spill.next()
    }
}

impl serde::Serialize for ClassList {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(self.len()))?;
        for atom in self.iter() {
            seq.serialize_element(atom)?;
        }
        seq.end()
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
    pub text_content: Option<CompactStr>,
    // LCRS tree pointers (replaces children: Vec<NodeId>)
    pub parent: Option<NodeId>,
    pub first_child: Option<NodeId>,
    pub last_child: Option<NodeId>,
    /// `0` means no sibling. See [`sib`].
    pub next_sibling: u32,
    /// `0` means no sibling. See [`sib`].
    pub prev_sibling: u32,
    /// Up to two attributes are inline. Values are atom ids, not separate Strings.
    pub attributes: AttrList,
    pub attribute_namespaces: Vec<AttributeNamespace>,
    /// Up to two classes are inline.
    pub class_list: ClassList,
    /// Whether CSS selector name matching follows the HTML element rules.
    /// Foreign SVG/MathML and XML elements keep authored name casing.
    pub is_html_element: bool,
}

fn cached_tag(tag: &str) -> Atom {
    thread_local! {
        static DIV: std::cell::Cell<u32> = const { std::cell::Cell::new(u32::MAX) };
        static TEXT: std::cell::Cell<u32> = const { std::cell::Cell::new(u32::MAX) };
    }
    fn load(slot: &std::cell::Cell<u32>, tag: &str) -> Atom {
        let id = slot.get();
        if id != u32::MAX {
            return Atom::from_u32(id);
        }
        let atom = Atom::intern(tag);
        slot.set(atom.as_u32());
        atom
    }
    match tag {
        "div" => DIV.with(|slot| load(slot, tag)),
        "#text" => TEXT.with(|slot| load(slot, tag)),
        _ => Atom::intern(tag),
    }
}

impl DomNode {
    pub fn new_element(id: NodeId, tag: impl AsRef<str>) -> Self {
        Self {
            id,
            node_type: NodeType::Element,
            tag: cached_tag(tag.as_ref()),
            text_content: None,
            parent: None,
            first_child: None,
            last_child: None,
            next_sibling: 0,
            prev_sibling: 0,
            attributes: AttrList::new(),
            attribute_namespaces: Vec::new(),
            class_list: ClassList::new(),
            is_html_element: true,
        }
    }

    pub fn new_text(id: NodeId, content: impl Into<CompactStr>) -> Self {
        Self {
            id,
            node_type: NodeType::Text,
            tag: cached_tag("#text"),
            text_content: Some(content.into()),
            parent: None,
            first_child: None,
            last_child: None,
            next_sibling: 0,
            prev_sibling: 0,
            attributes: AttrList::new(),
            attribute_namespaces: Vec::new(),
            class_list: ClassList::new(),
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
            next_sibling: 0,
            prev_sibling: 0,
            attributes: AttrList::new(),
            attribute_namespaces: Vec::new(),
            class_list: ClassList::new(),
            is_html_element: false,
        }
    }

    pub fn new_comment(id: NodeId, content: impl Into<CompactStr>) -> Self {
        Self {
            id,
            node_type: NodeType::Comment,
            tag: Atom::intern("#comment"),
            text_content: Some(content.into()),
            parent: None,
            first_child: None,
            last_child: None,
            next_sibling: 0,
            prev_sibling: 0,
            attributes: AttrList::new(),
            attribute_namespaces: Vec::new(),
            class_list: ClassList::new(),
            is_html_element: false,
        }
    }

    pub fn new_cdata_section(id: NodeId, content: impl Into<CompactStr>) -> Self {
        Self {
            id,
            node_type: NodeType::CdataSection,
            tag: Atom::intern("#cdata-section"),
            text_content: Some(content.into()),
            parent: None,
            first_child: None,
            last_child: None,
            next_sibling: 0,
            prev_sibling: 0,
            attributes: AttrList::new(),
            attribute_namespaces: Vec::new(),
            class_list: ClassList::new(),
            is_html_element: false,
        }
    }

    pub fn new_processing_instruction(
        id: NodeId,
        target: impl AsRef<str>,
        data: impl Into<CompactStr>,
    ) -> Self {
        Self {
            id,
            node_type: NodeType::ProcessingInstruction,
            tag: Atom::intern(target.as_ref()),
            text_content: Some(data.into()),
            parent: None,
            first_child: None,
            last_child: None,
            next_sibling: 0,
            prev_sibling: 0,
            attributes: AttrList::new(),
            attribute_namespaces: Vec::new(),
            class_list: ClassList::new(),
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
            next_sibling: 0,
            prev_sibling: 0,
            attributes: AttrList::new(),
            attribute_namespaces: Vec::new(),
            class_list: ClassList::new(),
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
            self.attributes[index].1 = AttrValue::new(value);
            return;
        }
        self.attributes.push((ce, AttrValue::new(value)));
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
            self.attributes[index].1 = AttrValue::new(value);
            return;
        }
        self.attributes.push((key, AttrValue::new(value)));
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
            self.attributes[existing_index].1 = AttrValue::new(value);
            return;
        } else {
            self.attributes
                .push((new_qualified_name, AttrValue::new(value)));
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
