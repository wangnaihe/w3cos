use serde::{Deserialize, Serialize};

use crate::dom_access::ActionType;

/// AI agent permission model.
/// Controls what an AI agent is allowed to do within the system.
/// Humans define these rules; AI agents must operate within them.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentPermissions {
    /// Can the agent read the DOM tree?
    pub can_read_dom: bool,
    /// Can the agent modify DOM elements (text, attributes, style)?
    pub can_write_dom: bool,
    /// Can the agent click buttons and trigger events?
    pub can_interact: bool,
    /// Can the agent create new elements?
    pub can_create_elements: bool,
    /// Can the agent remove elements?
    pub can_remove_elements: bool,
    /// Can the agent capture screenshots?
    pub can_screenshot: bool,
    /// Can the agent access the file system?
    pub can_access_files: bool,
    /// Can the agent make network requests?
    pub can_access_network: bool,
    /// Can the agent spawn new processes?
    pub can_spawn_processes: bool,
    /// Maximum number of actions per second (rate limiting).
    pub max_actions_per_second: u32,
    /// Selectors the agent is NOT allowed to touch.
    pub blocked_selectors: Vec<String>,
    /// Selectors the agent IS allowed to touch (if set, only these are allowed).
    pub allowed_selectors: Vec<String>,
}

impl Default for AgentPermissions {
    fn default() -> Self {
        Self {
            can_read_dom: true,
            can_write_dom: false,
            can_interact: true,
            can_create_elements: false,
            can_remove_elements: false,
            can_screenshot: true,
            can_access_files: false,
            can_access_network: false,
            can_spawn_processes: false,
            max_actions_per_second: 10,
            blocked_selectors: Vec::new(),
            allowed_selectors: Vec::new(),
        }
    }
}

impl AgentPermissions {
    /// Read-only observer: can see everything, can't change anything.
    pub fn observer() -> Self {
        Self {
            can_read_dom: true,
            can_write_dom: false,
            can_interact: false,
            can_create_elements: false,
            can_remove_elements: false,
            can_screenshot: true,
            can_access_files: false,
            can_access_network: false,
            can_spawn_processes: false,
            max_actions_per_second: 100,
            blocked_selectors: Vec::new(),
            allowed_selectors: Vec::new(),
        }
    }

    /// Standard interactive agent: can read, click, and type.
    pub fn interactive() -> Self {
        Self {
            can_read_dom: true,
            can_write_dom: true,
            can_interact: true,
            can_create_elements: false,
            can_remove_elements: false,
            can_screenshot: true,
            can_access_files: false,
            can_access_network: false,
            can_spawn_processes: false,
            max_actions_per_second: 10,
            blocked_selectors: Vec::new(),
            allowed_selectors: Vec::new(),
        }
    }

    /// Full system agent: can do everything. Use with caution.
    pub fn system() -> Self {
        Self {
            can_read_dom: true,
            can_write_dom: true,
            can_interact: true,
            can_create_elements: true,
            can_remove_elements: true,
            can_screenshot: true,
            can_access_files: true,
            can_access_network: true,
            can_spawn_processes: true,
            max_actions_per_second: 100,
            blocked_selectors: Vec::new(),
            allowed_selectors: Vec::new(),
        }
    }

    pub fn check_selector(&self, selector: &str) -> bool {
        if self
            .blocked_selectors
            .iter()
            .any(|s| selector.contains(s.as_str()))
        {
            return false;
        }
        if !self.allowed_selectors.is_empty() {
            return self
                .allowed_selectors
                .iter()
                .any(|s| selector.contains(s.as_str()));
        }
        true
    }
}

/// One bridge operation, shared by the HTTP server and the shell intent gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeOp {
    ReadDom,
    Screenshot,
    Query {
        selector: String,
    },
    Mutate {
        action: ActionType,
        selector: String,
    },
}

impl BridgeOp {
    pub fn is_mutating(&self) -> bool {
        matches!(self, Self::Mutate { .. })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthError {
    ReadDom,
    Screenshot,
    Interact,
    WriteDom,
    Selector,
}

impl AuthError {
    pub fn message(self, agent: &str) -> String {
        match self {
            Self::ReadDom => format!("Agent '{agent}' does not have DOM read permission"),
            Self::Screenshot => format!("Agent '{agent}' does not have screenshot permission"),
            Self::Interact => format!("Agent '{agent}' does not have interaction permission"),
            Self::WriteDom => format!("Agent '{agent}' does not have DOM write permission"),
            Self::Selector => format!("Agent '{agent}' is not allowed to access this selector"),
        }
    }
}

/// Resolve `W3COS_AI_PERMISSIONS`.
///
/// Unset and empty values are `observer`. `interactive` and `system` are
/// explicit overrides. Any other spelling fails closed to `observer`.
///
/// Issue #48 originally asked for a default of `interactive()`. That default
/// is superseded: a listener with no explicit grant must not click or type.
pub fn permissions_from_label(label: &str) -> AgentPermissions {
    match label.trim().to_ascii_lowercase().as_str() {
        "interactive" => AgentPermissions::interactive(),
        "system" => AgentPermissions::system(),
        "" | "observer" => AgentPermissions::observer(),
        _ => AgentPermissions::observer(),
    }
}

pub fn permissions_label_is_known(label: &str) -> bool {
    matches!(
        label.trim().to_ascii_lowercase().as_str(),
        "" | "interactive" | "observer" | "system"
    )
}

/// Permission check used by both `AiAgent` and the HTTP server.
///
/// Selector matching is still the existing substring check. It does not prove
/// that a node belongs to a particular app or window.
pub fn authorize(perms: &AgentPermissions, op: &BridgeOp) -> Result<(), AuthError> {
    match op {
        BridgeOp::ReadDom => {
            if !perms.can_read_dom {
                return Err(AuthError::ReadDom);
            }
        }
        BridgeOp::Screenshot => {
            if !perms.can_screenshot {
                return Err(AuthError::Screenshot);
            }
        }
        BridgeOp::Query { selector } => {
            if !perms.can_read_dom {
                return Err(AuthError::ReadDom);
            }
            if !perms.check_selector(selector) {
                return Err(AuthError::Selector);
            }
        }
        BridgeOp::Mutate { action, selector } => {
            if !perms.check_selector(selector) {
                return Err(AuthError::Selector);
            }
            let allowed = match action {
                ActionType::Click | ActionType::Focus | ActionType::ScrollIntoView => {
                    perms.can_interact
                }
                ActionType::SetText
                | ActionType::SetAttribute
                | ActionType::RemoveAttribute
                | ActionType::AddClass
                | ActionType::RemoveClass
                | ActionType::SetStyle => perms.can_write_dom,
            };
            if !allowed {
                return Err(match action {
                    ActionType::Click | ActionType::Focus | ActionType::ScrollIntoView => {
                        AuthError::Interact
                    }
                    _ => AuthError::WriteDom,
                });
            }
        }
    }
    Ok(())
}

/// Fixed one-second window. Callers pass the clock so tests stay deterministic.
#[derive(Debug, Default)]
pub struct ActionRateLimiter {
    window_started_ms: u64,
    count: u32,
}

impl ActionRateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Consumes one slot when `now_ms` is inside the current second and the
    /// count is still below `max_per_second`. A max of zero denies every call.
    pub fn try_acquire(&mut self, now_ms: u64, max_per_second: u32) -> bool {
        if max_per_second == 0 {
            return false;
        }
        if self.count == 0 || now_ms.saturating_sub(self.window_started_ms) >= 1000 {
            self.window_started_ms = now_ms;
            self.count = 0;
        }
        if self.count >= max_per_second {
            return false;
        }
        self.count += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_permission_label_fails_closed_to_observer() {
        assert!(permissions_label_is_known(""));
        assert!(permissions_label_is_known("Interactive"));
        assert!(!permissions_label_is_known("observr"));
        assert!(!permissions_from_label("").can_interact);
        assert!(!permissions_from_label("").can_write_dom);
        assert!(permissions_from_label("interactive").can_interact);
        assert!(!permissions_from_label("interactive").can_spawn_processes);
        assert!(!permissions_from_label("observer").can_interact);
        assert!(!permissions_from_label("observr").can_interact);
        assert!(permissions_from_label("system").can_spawn_processes);
    }

    #[test]
    fn observer_cannot_authorize_click_or_typing() {
        let perms = AgentPermissions::observer();
        assert_eq!(
            authorize(
                &perms,
                &BridgeOp::Mutate {
                    action: ActionType::Click,
                    selector: "button".into(),
                },
            ),
            Err(AuthError::Interact)
        );
        assert_eq!(
            authorize(
                &perms,
                &BridgeOp::Mutate {
                    action: ActionType::SetText,
                    selector: "input".into(),
                },
            ),
            Err(AuthError::WriteDom)
        );
        assert_eq!(
            authorize(
                &perms,
                &BridgeOp::Mutate {
                    action: ActionType::Focus,
                    selector: "button".into(),
                },
            ),
            Err(AuthError::Interact)
        );
    }

    #[test]
    fn blocked_selector_is_rejected_before_the_action_bit() {
        let mut perms = AgentPermissions::interactive();
        perms.blocked_selectors.push("secret".into());
        assert_eq!(
            authorize(
                &perms,
                &BridgeOp::Mutate {
                    action: ActionType::Click,
                    selector: "#secret".into(),
                },
            ),
            Err(AuthError::Selector)
        );
    }

    #[test]
    fn interactive_click_is_allowed_and_default_cannot_write() {
        assert!(
            authorize(
                &AgentPermissions::interactive(),
                &BridgeOp::Mutate {
                    action: ActionType::Click,
                    selector: "#go".into(),
                },
            )
            .is_ok()
        );
        assert_eq!(
            authorize(
                &AgentPermissions::default(),
                &BridgeOp::Mutate {
                    action: ActionType::SetText,
                    selector: "input".into(),
                },
            ),
            Err(AuthError::WriteDom)
        );
    }

    #[test]
    fn rate_limiter_blocks_inside_the_window_and_resets_after() {
        let mut limiter = ActionRateLimiter::new();
        assert!(limiter.try_acquire(1_000, 1));
        assert!(!limiter.try_acquire(1_100, 1));
        assert!(limiter.try_acquire(2_000, 1));
        assert!(!limiter.try_acquire(2_500, 0));
    }
}
