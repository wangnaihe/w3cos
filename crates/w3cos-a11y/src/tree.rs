use serde::{Deserialize, Serialize};
use w3cos_dom::atom::Atom;
use w3cos_dom::document::Document;
use w3cos_dom::node::{NodeId, NodeType};

use crate::role::AriaRole;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A11yNode {
    pub id: u32,
    pub role: AriaRole,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<u8>,
    pub interactive: bool,
    pub focused: bool,
    pub disabled: bool,
    pub visible: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounds: Option<[f32; 4]>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<A11yNode>,
}

pub fn build_a11y_tree(doc: &Document) -> A11yNode {
    let body = doc.body();
    build_node(doc, body.id)
}

fn build_node(doc: &Document, id: NodeId) -> A11yNode {
    let dom_node = doc.get_node(id);

    let role_atom = Atom::intern("role");
    let role = dom_node
        .attributes
        .iter()
        .find(|(k, _)| *k == role_atom)
        .map(|(_, v)| AriaRole::from_attr(v))
        .unwrap_or_else(|| {
            if dom_node.node_type == NodeType::Text {
                AriaRole::Text
            } else {
                AriaRole::from_tag(&dom_node.tag.as_str())
            }
        });

    let aria_label_atom = Atom::intern("aria-label");
    let title_atom = Atom::intern("title");
    let alt_atom = Atom::intern("alt");
    let placeholder_atom = Atom::intern("placeholder");
    let value_atom = Atom::intern("value");
    let disabled_atom = Atom::intern("disabled");

    let name = dom_node
        .attributes
        .iter()
        .find(|(k, _)| *k == aria_label_atom)
        .map(|(_, v)| v.clone())
        .or_else(|| dom_node.text_content.clone())
        .or_else(|| {
            dom_node
                .attributes
                .iter()
                .find(|(k, _)| *k == title_atom || *k == alt_atom || *k == placeholder_atom)
                .map(|(_, v)| v.clone())
        });

    let value = dom_node
        .attributes
        .iter()
        .find(|(k, _)| *k == value_atom)
        .map(|(_, v)| v.clone());

    let tag_str = dom_node.tag.as_str();
    let level = match tag_str.as_str() {
        "h1" => Some(1),
        "h2" => Some(2),
        "h3" => Some(3),
        "h4" => Some(4),
        "h5" => Some(5),
        "h6" => Some(6),
        _ => None,
    };

    let disabled = dom_node
        .attributes
        .iter()
        .any(|(k, _)| *k == disabled_atom);
    let style = doc.get_style(id);
    let visible = style.inner.opacity > 0.0
        && !matches!(style.inner.display, w3cos_std::style::Display::None);

    let children: Vec<A11yNode> = doc
        .children_ids(id)
        .iter()
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
