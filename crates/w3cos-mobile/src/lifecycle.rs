//! App foreground / background (maps to W3C Page Visibility + RN AppState).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AppLifecycleState {
    Active,
    Background,
    Inactive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifecycleEvent {
    pub state: AppLifecycleState,
}

impl LifecycleEvent {
    pub fn is_active(&self) -> bool {
        matches!(self.state, AppLifecycleState::Active)
    }
}
