use serde::{Deserialize, Serialize};
use w3cos_dom::document::Document;
use w3cos_dom::events::Event;

/// Layer 1: Direct DOM access for AI agents.
/// The most powerful interface — AI reads and writes the DOM directly.
/// This is equivalent to running JavaScript in a browser's DevTools console.

#[derive(Debug, Serialize, Deserialize)]
pub struct DomQuery {
    pub selector: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DomAction {
    pub action: ActionType,
    pub selector: String,
    #[serde(default)]
    pub value: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionType {
    Click,
    SetText,
    SetAttribute,
    RemoveAttribute,
    AddClass,
    RemoveClass,
    SetStyle,
    Focus,
    ScrollIntoView,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DomResult {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl DomResult {
    pub fn ok(data: serde_json::Value) -> Self {
        Self {
            success: true,
            error: None,
            data: Some(data),
        }
    }
    pub fn ok_empty() -> Self {
        Self {
            success: true,
            error: None,
            data: None,
        }
    }
    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            success: false,
            error: Some(msg.into()),
            data: None,
        }
    }
}

/// Query the DOM and return element info as JSON.
pub fn query(doc: &Document, selector: &str) -> DomResult {
    match doc.query_selector(selector) {
        Some(el) => {
            let node = doc.get_node(el.id);
            let info = serde_json::json!({
                "id": el.id.as_u32(),
                "tag": node.tag,
                "text": node.text_content,
                "classes": node.class_list,
                "attributes": node.attributes.iter()
                    .map(|(k, v)| serde_json::json!({ "name": k, "value": v }))
                    .collect::<Vec<_>>(),
                "childCount": doc.children_ids(el.id).len(),
            });
            DomResult::ok(info)
        }
        None => DomResult::err(format!("No element matching '{selector}'")),
    }
}

/// Query all matching elements.
pub fn query_all(doc: &Document, selector: &str) -> DomResult {
    let elements = doc.query_selector_all(selector);
    let results: Vec<serde_json::Value> = elements
        .iter()
        .map(|el| {
            let node = doc.get_node(el.id);
            serde_json::json!({
                "id": el.id.as_u32(),
                "tag": node.tag,
                "text": node.text_content,
                "classes": node.class_list,
            })
        })
        .collect();
    DomResult::ok(serde_json::json!(results))
}

/// Execute a DOM action.
pub fn execute(doc: &mut Document, action: &DomAction) -> DomResult {
    let el = match doc.query_selector(&action.selector) {
        Some(e) => e,
        None => return DomResult::err(format!("No element matching '{}'", action.selector)),
    };

    match action.action {
        ActionType::Click => {
            let mut event = Event::click(el.id, 0.0, 0.0);
            el.dispatch_event(doc, &mut event);
            DomResult::ok_empty()
        }
        ActionType::SetText => {
            let text = action.value.as_deref().unwrap_or("");
            el.set_text_content(doc, text);
            DomResult::ok_empty()
        }
        ActionType::SetAttribute => {
            if let Some(ref val) = action.value
                && let Some((name, value)) = val.split_once('=')
            {
                el.set_attribute(doc, name.trim(), value.trim());
                DomResult::ok_empty()
            } else {
                DomResult::err("SetAttribute requires value in format 'name=value'")
            }
        }
        ActionType::RemoveAttribute => {
            if let Some(ref name) = action.value {
                el.remove_attribute(doc, name);
                DomResult::ok_empty()
            } else {
                DomResult::err("RemoveAttribute requires attribute name in value")
            }
        }
        ActionType::AddClass => {
            if let Some(ref class) = action.value {
                el.class_list_add(doc, class);
                DomResult::ok_empty()
            } else {
                DomResult::err("AddClass requires class name in value")
            }
        }
        ActionType::RemoveClass => {
            if let Some(ref class) = action.value {
                el.class_list_remove(doc, class);
                DomResult::ok_empty()
            } else {
                DomResult::err("RemoveClass requires class name in value")
            }
        }
        ActionType::SetStyle => {
            if let Some(ref val) = action.value
                && let Some((prop, value)) = val.split_once(':')
            {
                el.style_mut(doc).set_property(prop.trim(), value.trim());
                DomResult::ok_empty()
            } else {
                DomResult::err("SetStyle requires value in format 'property: value'")
            }
        }
        ActionType::Focus | ActionType::ScrollIntoView => DomResult::ok_empty(),
    }
}
