//! W3C CSS Environment Variables — `safe-area-inset-*` runtime values.
//!
//! See [CSS Environment Variables Module Level 1](https://www.w3.org/TR/css-env-1/).

use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// Which `env(safe-area-inset-*)` edge is referenced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SafeAreaEdge {
    Top,
    Right,
    Bottom,
    Left,
}

/// Device safe area in logical pixels (CSS px).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct SafeAreaInsets {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl SafeAreaInsets {
    pub const ZERO: Self = Self {
        top: 0.0,
        right: 0.0,
        bottom: 0.0,
        left: 0.0,
    };

    pub fn value(&self, edge: SafeAreaEdge) -> f32 {
        match edge {
            SafeAreaEdge::Top => self.top,
            SafeAreaEdge::Right => self.right,
            SafeAreaEdge::Bottom => self.bottom,
            SafeAreaEdge::Left => self.left,
        }
    }
}

static ENABLED: AtomicBool = AtomicBool::new(false);
static INSETS: Mutex<SafeAreaInsets> = Mutex::new(SafeAreaInsets::ZERO);

pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

pub fn set_insets(insets: SafeAreaInsets) {
    if let Ok(mut guard) = INSETS.lock() {
        *guard = insets;
    }
}

/// Current insets for `env(safe-area-inset-*)` resolution. Zero when disabled.
pub fn current() -> SafeAreaInsets {
    if !is_enabled() {
        return SafeAreaInsets::ZERO;
    }
    INSETS.lock().map(|g| *g).unwrap_or(SafeAreaInsets::ZERO)
}
