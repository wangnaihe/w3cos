//! Virtual keyboard inset — aligns with [Visual Viewport] / `interactive-widget=resizes-content`.
//!
//! Runtime value for `env(keyboard-inset-height)` and Visual Viewport geometry.
//!
//! [Visual Viewport]: https://www.w3.org/TR/visual-viewport/

use std::sync::atomic::{AtomicU32, Ordering};

static BOTTOM_BITS: AtomicU32 = AtomicU32::new(0);

/// Bottom keyboard inset in logical (CSS) pixels.
pub fn bottom() -> f32 {
    f32::from_bits(BOTTOM_BITS.load(Ordering::Relaxed))
}

pub fn set_bottom(value: f32) {
    BOTTOM_BITS.store(value.max(0.0).to_bits(), Ordering::Relaxed);
}
