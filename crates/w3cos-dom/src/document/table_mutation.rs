//! Anonymous table boxes have retained identity across DOM removals. A fresh
//! component lowering must not merge runs that belonged to distinct wrappers.
use super::{Document, NodeId, NodeType, is_only_css_whitespace};
use w3cos_std::{
    Component,
    style::{Display, Float, Position, Style},
};

const KEY: &str = "--w3cos-internal-retained-table-boundary";

#[derive(Clone, Copy)]
pub(super) struct Boundary {
    pub(super) parent: NodeId,
    kind: &'static str,
}

fn table_internal(display: Display) -> bool {
    matches!(
        display,
        Display::TableCell
            | Display::TableRow
            | Display::TableRowGroup
            | Display::TableHeaderGroup
            | Display::TableFooterGroup
            | Display::TableCaption
            | Display::TableColumn
            | Display::TableColumnGroup
    )
}

fn in_flow(style: &Style) -> bool {
    style.float == Float::None
        && !matches!(style.position, Position::Absolute | Position::Fixed)
        && style.display != Display::None
}

impl Document {
    /// A source space immediately after an improper inline-host cell is
    /// discarded before anonymous table coalescing. Authored inline tables
    /// remain atomic inline boxes and retain ordinary surrounding glue.
    pub(super) fn inline_space_follows_misparented_cell(&self, id: NodeId) -> bool {
        let mut previous = self.get_node(id).prev_sibling;
        while let Some(id) = previous {
            let node = self.get_node(id);
            previous = node.prev_sibling;
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

    /// A later preformatted text node can join the anonymous cell established
    /// by preceding raw text. An element box (even a plain inline) is a real
    /// boundary; hidden scripts and comments do not own an intervening box.
    pub(super) fn row_space_follows_raw_text(&self, id: NodeId) -> bool {
        let mut previous = self.get_node(id).prev_sibling;
        while let Some(id) = previous {
            let node = self.get_node(id);
            previous = node.prev_sibling;
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

    fn table_boundary_neighbor(&self, mut node: Option<NodeId>, forward: bool) -> Option<NodeId> {
        while let Some(id) = node {
            let current = self.get_node(id);
            let separator = current.node_type == NodeType::Comment
                || (current.node_type == NodeType::Text
                    && current
                        .text_content
                        .as_deref()
                        .is_none_or(is_only_css_whitespace));
            if !separator {
                return Some(id);
            }
            node = if forward {
                current.next_sibling
            } else {
                current.prev_sibling
            };
        }
        None
    }

    pub(super) fn preserve_anonymous_table_boundary(&mut self, parent: NodeId, child: NodeId) {
        if self.get_node(child).parent != Some(parent) {
            return;
        }
        let mut ancestor = Some(parent);
        while ancestor != Some(NodeId::ROOT) {
            let Some(id) = ancestor else {
                return;
            };
            ancestor = self.get_node(id).parent;
        }
        let Some(right) = self.table_boundary_neighbor(self.get_node(child).next_sibling, true)
        else {
            return;
        };
        // If the first cell of a retained run is itself removed, retain the
        // existing boundary on the next survivor, not the detached node slot.
        if let Some(boundary) = self.anonymous_table_boundaries.get(&child).copied() {
            self.anonymous_table_boundaries.insert(right, boundary);
            return;
        }
        let Some(left) = self.table_boundary_neighbor(self.get_node(child).prev_sibling, false)
        else {
            return;
        };
        let node = self.get_node(child);
        if node.node_type == NodeType::Comment
            || (node.node_type == NodeType::Text
                && node
                    .text_content
                    .as_deref()
                    .is_none_or(is_only_css_whitespace))
        {
            return;
        }
        let parent_style = self.computed_style_for(parent);
        let removed_style = self.computed_style_for(child);
        let left_style = self.computed_style_for(left);
        let right_style = self.computed_style_for(right);
        if !in_flow(&removed_style) || !in_flow(&left_style) || !in_flow(&right_style) {
            return;
        }
        let kind = match parent_style.display {
            Display::Table
            | Display::InlineTable
            | Display::TableRowGroup
            | Display::TableHeaderGroup
            | Display::TableFooterGroup
                if table_internal(removed_style.display)
                    && removed_style.display != Display::TableCell
                    && (matches!(parent_style.display, Display::Table | Display::InlineTable)
                        || removed_style.display == Display::TableRow)
                    && left_style.display == Display::TableCell
                    && right_style.display == Display::TableCell =>
            {
                "row"
            }
            display
                if !table_internal(display)
                    && !matches!(display, Display::Table | Display::InlineTable)
                    && !table_internal(removed_style.display)
                    && table_internal(left_style.display)
                    && table_internal(right_style.display) =>
            {
                "table"
            }
            _ => return,
        };
        self.anonymous_table_boundaries
            .insert(right, Boundary { parent, kind });
    }

    pub(crate) fn preserve_anonymous_table_text_change(&mut self, id: NodeId, text: &str) {
        let node = self.get_node(id);
        if matches!(node.node_type, NodeType::Text | NodeType::CdataSection)
            && is_only_css_whitespace(text)
            && node
                .text_content
                .as_deref()
                .is_some_and(|previous| !is_only_css_whitespace(previous))
            && let Some(parent) = node.parent
        {
            self.preserve_anonymous_table_boundary(parent, id);
        }
    }

    pub(super) fn apply_anonymous_table_boundary(&self, id: NodeId, component: &mut Component) {
        if let Some(boundary) = self.anonymous_table_boundaries.get(&id)
            && self.get_node(id).parent == Some(boundary.parent)
        {
            component
                .style
                .custom_properties
                .get_or_insert_with(Default::default)
                .insert(KEY.into(), boundary.kind.into());
        }
    }
}

pub(super) fn take_boundary(component: &mut Component, kind: &str) -> bool {
    component
        .style
        .custom_properties
        .as_mut()
        .and_then(|properties| properties.remove(KEY))
        .is_some_and(|value| value == kind)
}
