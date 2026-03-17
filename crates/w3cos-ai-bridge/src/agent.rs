use anyhow::{bail, Result};
use w3cos_dom::document::Document;

use crate::a11y_api;
use crate::dom_access::{self, DomAction, DomResult};
use crate::permissions::AgentPermissions;

/// High-level AI Agent interface.
/// Wraps DOM access + a11y + permissions into a single, safe API.
///
/// This is what external AI agents (Claude, GPT, local LLMs) interact with.
/// All actions are permission-checked before execution.
pub struct AiAgent {
    pub name: String,
    pub permissions: AgentPermissions,
    action_count: u32,
}

impl AiAgent {
    pub fn new(name: impl Into<String>, permissions: AgentPermissions) -> Self {
        Self {
            name: name.into(),
            permissions,
            action_count: 0,
        }
    }

    /// Get the accessibility tree as a compact text summary.
    /// This is the recommended way for AI to understand the current UI.
    pub fn observe(&self, doc: &Document) -> Result<String> {
        if !self.permissions.can_read_dom {
            bail!("Agent '{}' does not have DOM read permission", self.name);
        }
        Ok(a11y_api::get_ui_summary(doc))
    }

    /// Get the full accessibility tree as JSON.
    pub fn observe_full(&self, doc: &Document) -> Result<String> {
        if !self.permissions.can_read_dom {
            bail!("Agent '{}' does not have DOM read permission", self.name);
        }
        Ok(a11y_api::get_tree_json(doc))
    }

    /// Query a specific element by CSS selector.
    pub fn query(&self, doc: &Document, selector: &str) -> Result<DomResult> {
        if !self.permissions.can_read_dom {
            bail!("Agent '{}' does not have DOM read permission", self.name);
        }
        if !self.permissions.check_selector(selector) {
            bail!("Agent '{}' is not allowed to access '{}'", self.name, selector);
        }
        Ok(dom_access::query(doc, selector))
    }

    /// Execute a DOM action (click, set text, modify style, etc.).
    pub fn act(&mut self, doc: &mut Document, action: &DomAction) -> Result<DomResult> {
        // Permission checks
        if !self.permissions.check_selector(&action.selector) {
            bail!("Agent '{}' is not allowed to access '{}'", self.name, action.selector);
        }

        match action.action {
            dom_access::ActionType::Click => {
                if !self.permissions.can_interact {
                    bail!("Agent '{}' does not have interaction permission", self.name);
                }
            }
            dom_access::ActionType::SetText |
            dom_access::ActionType::SetAttribute |
            dom_access::ActionType::RemoveAttribute |
            dom_access::ActionType::AddClass |
            dom_access::ActionType::RemoveClass |
            dom_access::ActionType::SetStyle => {
                if !self.permissions.can_write_dom {
                    bail!("Agent '{}' does not have DOM write permission", self.name);
                }
            }
            _ => {}
        }

        self.action_count += 1;
        eprintln!("[AI:{}] {} → {}", self.name, 
            serde_json::to_string(&action.action).unwrap_or_default().trim_matches('"'),
            action.selector
        );

        Ok(dom_access::execute(doc, action))
    }

    /// Get the number of actions this agent has performed.
    pub fn action_count(&self) -> u32 {
        self.action_count
    }
}
