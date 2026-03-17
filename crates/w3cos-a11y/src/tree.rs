use serde::{Deserialize, Serialize};
use w3cos_dom::document::Document;
use w3cos_dom::node::{NodeId, NodeType};

use crate::role::AriaRole;

/// A single node in the accessibility tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A11yNode {
    /// Unique node identifier (matches DOM NodeId).
    pub id: u32,
    /// ARIA role.
    pub role: AriaRole,
    /// Human-readable name (from text content, aria-label, or tag).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Current value (for inputs, progressbars, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Heading level (1-6 for headings).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<u8>,
    /// Whether the element is interactive (clickable, editable).
    pub interactive: bool,
    /// Whether the element is currently focused.
    pub focused: bool,
    /// Whether the element is disabled.
    pub disabled: bool,
    /// Whether the element is visible.
    pub visible: bool,
    /// Bounding box in logical pixels: [x, y, width, height].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounds: Option<[f32; 4]>,
    /// Child nodes.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<A11yNode>,
}

/// Build an accessibility tree from a Document.
pub fn build_a11y_tree(doc: &Document) -> A11yNode {
    let body = doc.body();
    build_node(doc, body.id)
}

fn build_node(doc: &Document, id: NodeId) -> A11yNode {
    let dom_node = doc.get_node(id);

    // Determine role: explicit `role` attribute > inferred from tag
    let role = dom_node.attributes.iter()
        .find(|(k, _)| k == "role")
        .map(|(_, v)| AriaRole::from_attr(v))
        .unwrap_or_else(|| {
            if dom_node.node_type == NodeType::Text {
                AriaRole::Text
            } else {
                AriaRole::from_tag(&dom_node.tag)
            }
        });

    // Name: aria-label > aria-labelledby (skip for now) > text content > tag
    let name = dom_node.attributes.iter()
        .find(|(k, _)| k == "aria-label")
        .map(|(_, v): &(String, String)| v.clone())
        .or_else(|| dom_node.text_content.clone())
        .or_else(|| {
            dom_node.attributes.iter()
                .find(|(k, _)| k == "title" || k == "alt" || k == "placeholder")
                .map(|(_, v): &(String, String)| v.clone())
        });

    let value = dom_node.attributes.iter()
        .find(|(k, _)| k == "value")
        .map(|(_, v): &(String, String)| v.clone());

    // Heading level
    let level = match dom_node.tag.as_str() {
        "h1" => Some(1),
        "h2" => Some(2),
        "h3" => Some(3),
        "h4" => Some(4),
        "h5" => Some(5),
        "h6" => Some(6),
        _ => None,
    };

    let disabled = dom_node.attributes.iter().any(|(k, _)| k == "disabled");
    let style = doc.get_style(id);
    let visible = style.inner.opacity > 0.0
        && !matches!(style.inner.display, w3cos_std::style::Display::None);

    let children: Vec<A11yNode> = dom_node.children.iter()
        .map(|&child_id| build_node(doc, child_id))
        .filter(|node| node.visible && node.role != AriaRole::None)
        .collect();

    A11yNode {
        id: id.as_u32(),
        role: role.clone(),
        name,
        value,
        level,
        interactive: role.is_interactive(),
        focused: false,
        disabled,
        visible,
        bounds: None,
        children,
    }
}

/// Flatten the accessibility tree into a numbered list for AI consumption.
/// Returns lines like: "[1] button: Submit"  "[2] textbox: Email (value: user@example.com)"
pub fn flatten_for_ai(tree: &A11yNode) -> Vec<String> {
    let mut lines = Vec::new();
    let mut counter = 1u32;
    flatten_recursive(tree, &mut lines, &mut counter);
    lines
}

fn flatten_recursive(node: &A11yNode, lines: &mut Vec<String>, counter: &mut u32) {
    if node.visible && node.role != AriaRole::None {
        let idx = *counter;
        *counter += 1;

        let role_str = serde_json::to_string(&node.role)
            .unwrap_or_default()
            .trim_matches('"')
            .to_string();

        let mut desc = format!("[{idx}] {role_str}");

        if let Some(ref name) = node.name {
            let truncated = if name.len() > 80 {
                format!("{}...", &name[..77])
            } else {
                name.clone()
            };
            desc.push_str(&format!(": {truncated}"));
        }

        if let Some(ref value) = node.value {
            desc.push_str(&format!(" (value: {value})"));
        }

        if node.interactive {
            desc.push_str(" [interactive]");
        }
        if node.disabled {
            desc.push_str(" [disabled]");
        }

        lines.push(desc);
    }

    for child in &node.children {
        flatten_recursive(child, lines, counter);
    }
}
