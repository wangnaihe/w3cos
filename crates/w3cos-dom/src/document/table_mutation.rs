//! Current-DOM whitespace rules for anonymous table reconstruction.
//! Mutation history must not split newly adjacent table-internal runs.
use super::{Document, NodeId, NodeType, is_only_css_whitespace};
use w3cos_std::style::Display;

impl Document {
    /// A source space immediately after an improper inline-host cell is
    /// discarded before anonymous table coalescing. Authored inline tables
    /// remain atomic inline boxes and retain ordinary surrounding glue.
    pub(super) fn inline_space_follows_misparented_cell(&self, id: NodeId) -> bool {
        let mut previous = crate::node::sib(self.get_node(id).prev_sibling);
        while let Some(id) = previous {
            let node = self.get_node(id);
            previous = crate::node::sib(node.prev_sibling);
            match node.node_type {
                NodeType::Text
                    if node
                        .text_content
                        .as_deref()
                        .is_none_or(is_only_css_whitespace) => {}
                NodeType::Comment => {}
                NodeType::Element => {
                    let display = self.computed_style_for(id).display;
                    if display != Display::None {
                        return display == Display::TableCell;
                    }
                }
                _ => return false,
            }
        }
        false
    }

    /// A preformatted row space next to an element that specifies `display:inline`
    /// belongs to that improper inline's anonymous cell. A plain row child with
    /// no display declaration does not give the space a box.
    pub(super) fn pre_row_space_adjoins_specified_inline(&self, id: NodeId) -> bool {
        let specifies_inline = |candidate: NodeId| {
            let node = self.get_node(candidate);
            if node.node_type != NodeType::Element {
                return false;
            }
            self.styles
                .get(candidate.0 as usize)
                .into_iter()
                .flat_map(|style| style.inline_declarations.iter())
                .any(|(name, value)| name == "display" && value.trim().eq_ignore_ascii_case("inline"))
        };
        let node = self.get_node(id);
        crate::node::sib(node.prev_sibling).is_some_and(specifies_inline)
            || crate::node::sib(node.next_sibling).is_some_and(specifies_inline)
    }

    /// A later preformatted text node can join the anonymous cell established
    /// by preceding raw text. An element box (even a plain inline) is a real
    /// boundary; hidden scripts and comments do not own an intervening box.
    pub(super) fn row_space_follows_raw_text(&self, id: NodeId) -> bool {
        let mut previous = crate::node::sib(self.get_node(id).prev_sibling);
        while let Some(id) = previous {
            let node = self.get_node(id);
            previous = crate::node::sib(node.prev_sibling);
            match node.node_type {
                NodeType::Text => {
                    if node
                        .text_content
                        .as_deref()
                        .is_some_and(|text| !is_only_css_whitespace(text))
                    {
                        return true;
                    }
                }
                NodeType::Comment => {}
                NodeType::Element if self.computed_style_for(id).display == Display::None => {}
                _ => return false,
            }
        }
        false
    }

}
