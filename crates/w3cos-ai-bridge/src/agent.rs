use anyhow::Result;
use w3cos_dom::document::Document;

use crate::a11y_api;
use crate::dom_access::{self, DomAction, DomResult};
use crate::permissions::{self, AgentPermissions, BridgeOp};

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
        self.require(BridgeOp::ReadDom)?;
        Ok(a11y_api::get_ui_summary(doc))
    }

    /// Get the full accessibility tree as JSON.
    pub fn observe_full(&self, doc: &Document) -> Result<String> {
        self.require(BridgeOp::ReadDom)?;
        Ok(a11y_api::get_tree_json(doc))
    }

    /// Query a specific element by CSS selector.
    pub fn query(&self, doc: &Document, selector: &str) -> Result<DomResult> {
        self.require(BridgeOp::Query {
            selector: selector.to_string(),
        })?;
        Ok(dom_access::query(doc, selector))
    }

    /// Execute a DOM action (click, set text, modify style, etc.).
    pub fn act(&mut self, doc: &mut Document, action: &DomAction) -> Result<DomResult> {
        self.require(BridgeOp::Mutate {
            action: action.action,
            selector: action.selector.clone(),
        })?;

        self.action_count += 1;
        eprintln!(
            "[AI:{}] {} → {}",
            self.name,
            serde_json::to_string(&action.action)
                .unwrap_or_default()
                .trim_matches('"'),
            action.selector
        );

        Ok(dom_access::execute(doc, action))
    }

    /// Get the number of actions this agent has performed.
    pub fn action_count(&self) -> u32 {
        self.action_count
    }

    fn require(&self, op: BridgeOp) -> Result<()> {
        permissions::authorize(&self.permissions, &op).map_err(|err| {
            let detail = match (&err, &op) {
                (permissions::AuthError::Selector, BridgeOp::Mutate { selector, .. })
                | (permissions::AuthError::Selector, BridgeOp::Query { selector }) => {
                    format!(
                        "Agent '{}' is not allowed to access '{selector}'",
                        self.name
                    )
                }
                _ => err.message(&self.name),
            };
            anyhow::anyhow!(detail)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom_access::{ActionType, DomAction};
    use std::cell::Cell;
    use std::rc::Rc;
    use w3cos_dom::Document;

    fn button() -> (Document, Rc<Cell<u32>>) {
        let mut doc = Document::new();
        let button = doc.create_element("button");
        button.set_attribute(&mut doc, "id", "go");
        let hits = Rc::new(Cell::new(0u32));
        let hits_for_handler = hits.clone();
        button.add_event_listener(
            &mut doc,
            "click",
            Box::new(move |_| {
                hits_for_handler.set(hits_for_handler.get() + 1);
            }),
        );
        doc.body().append_child(&mut doc, button);
        (doc, hits)
    }

    fn click(selector: &str) -> DomAction {
        DomAction {
            action: ActionType::Click,
            selector: selector.to_string(),
            value: None,
        }
    }

    #[test]
    fn observer_cannot_click_and_does_not_mutate() {
        let (mut doc, hits) = button();
        let mut agent = AiAgent::new("obs", AgentPermissions::observer());
        let err = agent.act(&mut doc, &click("#go")).unwrap_err();
        assert!(err.to_string().contains("interaction"), "{err}");
        assert_eq!(hits.get(), 0);
        assert_eq!(agent.action_count(), 0);
    }

    #[test]
    fn focus_requires_interaction_permission() {
        let (mut doc, hits) = button();
        let mut agent = AiAgent::new("obs", AgentPermissions::observer());
        let err = agent
            .act(
                &mut doc,
                &DomAction {
                    action: ActionType::Focus,
                    selector: "#go".into(),
                    value: None,
                },
            )
            .unwrap_err();
        assert!(err.to_string().contains("interaction"), "{err}");
        assert_eq!(hits.get(), 0);
    }

    #[test]
    fn blocked_selector_is_rejected_without_a_click() {
        let (mut doc, hits) = button();
        let mut perms = AgentPermissions::interactive();
        perms.blocked_selectors.push("#go".into());
        let mut agent = AiAgent::new("bot", perms);
        let err = agent.act(&mut doc, &click("#go")).unwrap_err();
        assert!(err.to_string().contains("#go"), "{err}");
        assert_eq!(hits.get(), 0);
    }

    #[test]
    fn interactive_click_fires_once() {
        let (mut doc, hits) = button();
        let mut agent = AiAgent::new("bot", AgentPermissions::interactive());
        assert!(agent.act(&mut doc, &click("#go")).unwrap().success);
        assert_eq!(hits.get(), 1);
        assert_eq!(agent.action_count(), 1);
    }
}
