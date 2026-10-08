//! Character boundaries for whitespace collapse across non-atomic inline boxes.

use super::{Document, NodeId, NodeType, is_only_css_whitespace};
use w3cos_std::style::{Display, Float, Position, WhiteSpace};

impl Document {
    /// None means an empty/collapsible inline boundary to look through; false
    /// is an in-flow block or hard break, and true is character/atomic content.
    pub(super) fn inline_character_boundary(&self, id: NodeId, from_end: bool) -> Option<bool> {
        let node = self.get_node(id);
        if node.node_type == NodeType::Text {
            let raw = node.text_content.as_deref().unwrap_or_default();
            if raw.is_empty() {
                return None;
            }
            let style = self.computed_style_for(id);
            return (!is_only_css_whitespace(raw)
                || !matches!(
                    style.white_space,
                    WhiteSpace::Normal | WhiteSpace::NoWrap | WhiteSpace::PreLine
                ))
            .then_some(true);
        }
        if node.node_type != NodeType::Element {
            return None;
        }
        let style = self.computed_style_for(id);
        if style.display == Display::None
            || style.float != Float::None
            || matches!(style.position, Position::Absolute | Position::Fixed)
        {
            return None;
        }
        if node.tag.as_str().eq_ignore_ascii_case("br") {
            return Some(false);
        }
        if matches!(
            style.display,
            Display::InlineBlock | Display::InlineFlex | Display::InlineTable
        ) {
            return Some(true);
        }
        // Misparented table roles inside an inline are lowered together into
        // an anonymous atomic table, not a block-in-inline split boundary.
        if matches!(
            style.display,
            Display::TableCell
                | Display::TableCaption
                | Display::TableRow
                | Display::TableRowGroup
                | Display::TableHeaderGroup
                | Display::TableFooterGroup
                | Display::TableColumn
                | Display::TableColumnGroup
        ) && node
            .parent
            .is_some_and(|parent| self.computed_style_for(parent).display == Display::Inline)
        {
            return Some(true);
        }
        if !matches!(style.display, Display::Inline | Display::Contents) {
            return Some(false);
        }
        if matches!(
            node.tag.as_str().as_str(),
            "img"
                | "object"
                | "embed"
                | "iframe"
                | "canvas"
                | "svg"
                | "video"
                | "audio"
                | "input"
                | "textarea"
                | "select"
        ) {
            return Some(true);
        }
        let pseudo = if from_end { "::after" } else { "::before" };
        if self.pseudo_generates_box(id, pseudo) {
            return Some(true);
        }
        // This DOM also stores set_text_content's direct element text in the
        // element slot; it has the same boundary as an ordinary text child.
        if node.text_content.as_deref().is_some_and(|raw| {
            !raw.is_empty()
                && (!is_only_css_whitespace(raw)
                    || !matches!(
                        style.white_space,
                        WhiteSpace::Normal | WhiteSpace::NoWrap | WhiteSpace::PreLine
                    ))
        }) {
            return Some(true);
        }
        let mut child = if from_end {
            node.last_child
        } else {
            node.first_child
        };
        while let Some(child_id) = child {
            let child_node = self.get_node(child_id);
            child = if from_end {
                child_node.prev_sibling
            } else {
                child_node.next_sibling
            };
            if let Some(boundary) = self.inline_character_boundary(child_id, from_end) {
                return Some(boundary);
            }
        }
        self.pseudo_generates_box(id, if from_end { "::before" } else { "::after" })
            .then_some(true)
    }
}

#[cfg(test)]
mod tests {
    use super::super::Document;
    use w3cos_std::{Component, ComponentKind};

    fn text(component: &Component) -> String {
        let mut result = match &component.kind {
            ComponentKind::Text { content } => content.clone(),
            _ => String::new(),
        };
        for child in &component.children {
            result.push_str(&text(child));
        }
        result
    }

    #[test]
    fn empty_decorated_inline_does_not_preserve_line_edge_spaces() {
        crate::stylesheet::clear_rules();
        let mut document = Document::new();
        for leading in [false, true] {
            let block = document.create_element("div");
            let empty = document.create_element("span");
            empty
                .style_mut(&mut document)
                .set_property("border", "5px solid blue");
            let run = document.create_text_node(if leading {
                " Third line, yes"
            } else {
                "First line "
            });
            if leading {
                block.append_child(&mut document, empty);
                block.append_child(&mut document, run);
            } else {
                block.append_child(&mut document, run);
                block.append_child(&mut document, empty);
            }
            document.body().append_child(&mut document, block);
        }
        assert_eq!(
            text(&document.to_component_tree()),
            "First lineThird line, yes"
        );
    }

    #[test]
    fn split_inline_block_does_not_preserve_neighboring_line_edge_spaces() {
        crate::stylesheet::clear_rules();
        let mut document = Document::new();
        let outer = document.create_element("span");
        let before = document.create_text_node("First line ");
        outer.append_child(&mut document, before);
        let decorated = document.create_element("span");
        decorated
            .style_mut(&mut document)
            .set_property("border", "5px solid blue");
        let block = document.create_element("span");
        block
            .style_mut(&mut document)
            .set_property("display", "block");
        let middle = document.create_text_node(" Second line ");
        block.append_child(&mut document, middle);
        decorated.append_child(&mut document, block);
        outer.append_child(&mut document, decorated);
        let after = document.create_text_node(" Third line, yes");
        outer.append_child(&mut document, after);
        document.body().append_child(&mut document, outer);
        assert_eq!(
            text(&document.to_component_tree()),
            "First lineSecond lineThird line, yes"
        );
    }

    #[test]
    fn empty_inline_preserves_interior_separator_and_atomic_boundaries() {
        crate::stylesheet::clear_rules();
        let mut document = Document::new();
        for atomic in [false, true] {
            let block = document.create_element("div");
            let before = document.create_text_node("A ");
            block.append_child(&mut document, before);
            let empty = document.create_element("span");
            empty
                .style_mut(&mut document)
                .set_property("border", "5px solid blue");
            if atomic {
                empty
                    .style_mut(&mut document)
                    .set_property("display", "inline-block");
            }
            block.append_child(&mut document, empty);
            let after = document.create_text_node(" B");
            block.append_child(&mut document, after);
            document.body().append_child(&mut document, block);
        }
        assert_eq!(text(&document.to_component_tree()), "A BA  B");
    }
}
