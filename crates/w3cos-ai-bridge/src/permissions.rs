use serde::{Deserialize, Serialize};

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
        if self.blocked_selectors.iter().any(|s| selector.contains(s.as_str())) {
            return false;
        }
        if !self.allowed_selectors.is_empty() {
            return self.allowed_selectors.iter().any(|s| selector.contains(s.as_str()));
        }
        true
    }
}
