use crate::document::Document;
use crate::node::NodeId;

/// W3C Range API — represents a fragment of the document tree.
///
/// A Range identifies a start and end point in the DOM tree.
/// Each point is a (node, offset) pair:
/// - For text nodes: offset is a character index
/// - For element nodes: offset is a child index
#[derive(Debug, Clone)]
pub struct Range {
    pub start_container: NodeId,
    pub start_offset: u32,
    pub end_container: NodeId,
    pub end_offset: u32,
}

impl Range {
    pub fn new() -> Self {
        Self {
            start_container: NodeId::ROOT,
            start_offset: 0,
            end_container: NodeId::ROOT,
            end_offset: 0,
        }
    }

    pub fn set_start(&mut self, node: NodeId, offset: u32) {
        self.start_container = node;
        self.start_offset = offset;
    }

    pub fn set_end(&mut self, node: NodeId, offset: u32) {
        self.end_container = node;
        self.end_offset = offset;
    }

    pub fn collapsed(&self) -> bool {
        self.start_container == self.end_container && self.start_offset == self.end_offset
    }

    /// Get the bounding rect of this range. Returns (x, y, width, height).
    /// Actual pixel coordinates require layout information from the runtime.
    /// Returns a placeholder; real implementation needs glyph-level hit testing.
    pub fn get_bounding_client_rect(&self) -> DOMRect {
        DOMRect {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        }
    }

    /// Extract the text content within this range.
    pub fn to_string(&self, doc: &Document) -> String {
        if self.start_container != self.end_container {
            let start_text = doc
                .get_node(self.start_container)
                .text_content
                .as_deref()
                .unwrap_or("");
            let end_text = doc
                .get_node(self.end_container)
                .text_content
                .as_deref()
                .unwrap_or("");
            let mut result = String::new();
            if !start_text.is_empty() {
                let chars: Vec<char> = start_text.chars().collect();
                let from = (self.start_offset as usize).min(chars.len());
                result.push_str(&chars[from..].iter().collect::<String>());
            }
            if !end_text.is_empty() {
                let chars: Vec<char> = end_text.chars().collect();
                let to = (self.end_offset as usize).min(chars.len());
                result.push_str(&chars[..to].iter().collect::<String>());
            }
            return result;
        }

        let text = doc
            .get_node(self.start_container)
            .text_content
            .as_deref()
            .unwrap_or("");
        let chars: Vec<char> = text.chars().collect();
        let from = (self.start_offset as usize).min(chars.len());
        let to = (self.end_offset as usize).min(chars.len());
        if from <= to {
            chars[from..to].iter().collect()
        } else {
            String::new()
        }
    }

    /// Clone the contents of this range as a string (simplified).
    pub fn clone_contents(&self, doc: &Document) -> String {
        self.to_string(doc)
    }

    /// Delete the contents of this range from the document.
    pub fn delete_contents(&self, doc: &mut Document) {
        if self.start_container == self.end_container {
            if let Some(text) = &doc.get_node(self.start_container).text_content.clone() {
                let chars: Vec<char> = text.chars().collect();
                let from = (self.start_offset as usize).min(chars.len());
                let to = (self.end_offset as usize).min(chars.len());
                if from <= to {
                    let mut new_text: Vec<char> = Vec::with_capacity(chars.len() - (to - from));
                    new_text.extend_from_slice(&chars[..from]);
                    new_text.extend_from_slice(&chars[to..]);
                    let result: String = new_text.into_iter().collect();
                    doc.get_node_mut(self.start_container).text_content = Some(result);
                    doc.mark_dirty(self.start_container);
                }
            }
        }
    }

    /// Extract contents: removes from DOM and returns the extracted text.
    pub fn extract_contents(&self, doc: &mut Document) -> String {
        let text = self.to_string(doc);
        self.delete_contents(doc);
        text
    }
}

impl Default for Range {
    fn default() -> Self {
        Self::new()
    }
}

/// W3C Selection API — represents the currently selected range of text.
///
/// Maps to `window.getSelection()` in the browser.
/// A selection has an anchor (where selection started) and a focus
/// (where selection ended, i.e., where the user dragged to).
#[derive(Debug, Clone)]
pub struct Selection {
    pub anchor_node: Option<NodeId>,
    pub anchor_offset: u32,
    pub focus_node: Option<NodeId>,
    pub focus_offset: u32,
    ranges: Vec<Range>,
}

impl Selection {
    pub fn new() -> Self {
        Self {
            anchor_node: None,
            anchor_offset: 0,
            focus_node: None,
            focus_offset: 0,
            ranges: Vec::new(),
        }
    }

    /// Returns true if no text is selected (cursor only).
    pub fn is_collapsed(&self) -> bool {
        self.anchor_node == self.focus_node && self.anchor_offset == self.focus_offset
    }

    /// Get the selected text as a string.
    pub fn to_string(&self, doc: &Document) -> String {
        self.ranges
            .first()
            .map(|r| r.to_string(doc))
            .unwrap_or_default()
    }

    /// Collapse the selection to a single cursor position.
    pub fn collapse(&mut self, node: NodeId, offset: u32) {
        self.anchor_node = Some(node);
        self.anchor_offset = offset;
        self.focus_node = Some(node);
        self.focus_offset = offset;
        self.ranges.clear();
        let mut range = Range::new();
        range.set_start(node, offset);
        range.set_end(node, offset);
        self.ranges.push(range);
    }

    /// Select all children of a given node.
    pub fn select_all_children(&mut self, doc: &Document, node: NodeId) {
        let children = doc.children_ids(node);
        if children.is_empty() {
            let text_len = doc
                .get_node(node)
                .text_content
                .as_ref()
                .map(|t| t.chars().count() as u32)
                .unwrap_or(0);
            self.anchor_node = Some(node);
            self.anchor_offset = 0;
            self.focus_node = Some(node);
            self.focus_offset = text_len;

            self.ranges.clear();
            let mut range = Range::new();
            range.set_start(node, 0);
            range.set_end(node, text_len);
            self.ranges.push(range);
        } else {
            let first = children[0];
            let last = *children.last().unwrap();
            self.anchor_node = Some(first);
            self.anchor_offset = 0;
            self.focus_node = Some(last);
            self.focus_offset = doc.children_ids(last).len() as u32;

            self.ranges.clear();
            let mut range = Range::new();
            range.set_start(first, 0);
            range.set_end(last, doc.children_ids(last).len() as u32);
            self.ranges.push(range);
        }
    }

    /// Remove all ranges from the selection.
    pub fn remove_all_ranges(&mut self) {
        self.ranges.clear();
        self.anchor_node = None;
        self.anchor_offset = 0;
        self.focus_node = None;
        self.focus_offset = 0;
    }

    /// Add a range to the selection.
    pub fn add_range(&mut self, range: Range) {
        if self.ranges.is_empty() {
            self.anchor_node = Some(range.start_container);
            self.anchor_offset = range.start_offset;
            self.focus_node = Some(range.end_container);
            self.focus_offset = range.end_offset;
        }
        self.ranges.push(range);
    }

    /// Get the number of ranges in the selection.
    pub fn range_count(&self) -> usize {
        self.ranges.len()
    }

    /// Get a range by index.
    pub fn get_range_at(&self, index: usize) -> Option<&Range> {
        self.ranges.get(index)
    }

    /// Extend the selection's focus to a new position.
    pub fn extend(&mut self, node: NodeId, offset: u32) {
        self.focus_node = Some(node);
        self.focus_offset = offset;

        if let Some(anchor) = self.anchor_node {
            self.ranges.clear();
            let mut range = Range::new();
            range.set_start(anchor, self.anchor_offset);
            range.set_end(node, offset);
            self.ranges.push(range);
        }
    }
}

impl Default for Selection {
    fn default() -> Self {
        Self::new()
    }
}

/// DOMRect placeholder for bounding rectangle.
#[derive(Debug, Clone, Copy, Default)]
pub struct DOMRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl DOMRect {
    pub fn top(&self) -> f32 {
        self.y
    }
    pub fn right(&self) -> f32 {
        self.x + self.width
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.height
    }
    pub fn left(&self) -> f32 {
        self.x
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    #[test]
    fn range_new_is_collapsed() {
        let r = Range::new();
        assert!(r.collapsed());
    }

    #[test]
    fn range_set_start_end() {
        let mut r = Range::new();
        let node = NodeId::from_u32(5);
        r.set_start(node, 2);
        r.set_end(node, 8);
        assert_eq!(r.start_container, node);
        assert_eq!(r.start_offset, 2);
        assert_eq!(r.end_container, node);
        assert_eq!(r.end_offset, 8);
        assert!(!r.collapsed());
    }

    #[test]
    fn range_to_string_same_node() {
        let mut doc = Document::new();
        let text = doc.create_text_node("Hello World");
        doc.body().append_child(&mut doc, text);

        let mut r = Range::new();
        r.set_start(text.id, 0);
        r.set_end(text.id, 5);
        assert_eq!(r.to_string(&doc), "Hello");
    }

    #[test]
    fn range_to_string_substring() {
        let mut doc = Document::new();
        let text = doc.create_text_node("Hello World");
        doc.body().append_child(&mut doc, text);

        let mut r = Range::new();
        r.set_start(text.id, 6);
        r.set_end(text.id, 11);
        assert_eq!(r.to_string(&doc), "World");
    }

    #[test]
    fn range_delete_contents() {
        let mut doc = Document::new();
        let text = doc.create_text_node("Hello World");
        doc.body().append_child(&mut doc, text);

        let r = Range {
            start_container: text.id,
            start_offset: 5,
            end_container: text.id,
            end_offset: 11,
        };
        r.delete_contents(&mut doc);
        let remaining = doc.get_node(text.id).text_content.as_deref().unwrap();
        assert_eq!(remaining, "Hello");
    }

    #[test]
    fn range_extract_contents() {
        let mut doc = Document::new();
        let text = doc.create_text_node("Hello World");
        doc.body().append_child(&mut doc, text);

        let r = Range {
            start_container: text.id,
            start_offset: 0,
            end_container: text.id,
            end_offset: 5,
        };
        let extracted = r.extract_contents(&mut doc);
        assert_eq!(extracted, "Hello");
        let remaining = doc.get_node(text.id).text_content.as_deref().unwrap();
        assert_eq!(remaining, " World");
    }

    #[test]
    fn selection_new_is_collapsed() {
        let sel = Selection::new();
        assert!(sel.is_collapsed());
    }

    #[test]
    fn selection_collapse() {
        let mut sel = Selection::new();
        let node = NodeId::from_u32(3);
        sel.collapse(node, 5);
        assert!(sel.is_collapsed());
        assert_eq!(sel.anchor_node, Some(node));
        assert_eq!(sel.anchor_offset, 5);
        assert_eq!(sel.focus_node, Some(node));
        assert_eq!(sel.focus_offset, 5);
        assert_eq!(sel.range_count(), 1);
    }

    #[test]
    fn selection_add_range() {
        let mut sel = Selection::new();
        let node = NodeId::from_u32(2);
        let mut range = Range::new();
        range.set_start(node, 0);
        range.set_end(node, 10);
        sel.add_range(range);
        assert_eq!(sel.range_count(), 1);
        assert_eq!(sel.anchor_node, Some(node));
        assert_eq!(sel.focus_node, Some(node));
    }

    #[test]
    fn selection_remove_all_ranges() {
        let mut sel = Selection::new();
        let node = NodeId::from_u32(1);
        sel.collapse(node, 0);
        assert_eq!(sel.range_count(), 1);
        sel.remove_all_ranges();
        assert_eq!(sel.range_count(), 0);
        assert!(sel.anchor_node.is_none());
    }

    #[test]
    fn selection_to_string() {
        let mut doc = Document::new();
        let text = doc.create_text_node("Hello Selection API");
        doc.body().append_child(&mut doc, text);

        let mut sel = Selection::new();
        let mut range = Range::new();
        range.set_start(text.id, 6);
        range.set_end(text.id, 15);
        sel.add_range(range);
        assert_eq!(sel.to_string(&doc), "Selection");
    }

    #[test]
    fn selection_extend() {
        let mut sel = Selection::new();
        let node = NodeId::from_u32(5);
        sel.collapse(node, 0);
        sel.extend(node, 10);
        assert!(!sel.is_collapsed());
        assert_eq!(sel.focus_offset, 10);
        assert_eq!(sel.range_count(), 1);
    }

    #[test]
    fn selection_select_all_children_text_node() {
        let mut doc = Document::new();
        let text = doc.create_text_node("Select Me");
        doc.body().append_child(&mut doc, text);

        let mut sel = Selection::new();
        sel.select_all_children(&doc, text.id);
        assert_eq!(sel.anchor_offset, 0);
        assert_eq!(sel.focus_offset, 9); // "Select Me" = 9 chars
        assert_eq!(sel.to_string(&doc), "Select Me");
    }

    #[test]
    fn range_clone_contents() {
        let mut doc = Document::new();
        let text = doc.create_text_node("Cloneable");
        doc.body().append_child(&mut doc, text);

        let mut r = Range::new();
        r.set_start(text.id, 0);
        r.set_end(text.id, 5);
        assert_eq!(r.clone_contents(&doc), "Clone");
    }

    #[test]
    fn dom_rect_accessors() {
        let rect = DOMRect {
            x: 10.0,
            y: 20.0,
            width: 100.0,
            height: 50.0,
        };
        assert_eq!(rect.top(), 20.0);
        assert_eq!(rect.right(), 110.0);
        assert_eq!(rect.bottom(), 70.0);
        assert_eq!(rect.left(), 10.0);
    }
}
