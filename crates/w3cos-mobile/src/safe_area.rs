//! Display cutout / home-indicator insets — re-exports + C FFI for native shells.

pub use w3cos_std::safe_area::{
    SafeAreaEdge, SafeAreaInsets, current, is_enabled, set_enabled, set_insets,
};

/// Called from iOS/Android shell before `w3cos_app_run` (logical px).
#[unsafe(no_mangle)]
pub extern "C" fn w3cos_set_safe_area_insets(top: f32, right: f32, bottom: f32, left: f32) {
    set_enabled(true);
    set_insets(SafeAreaInsets {
        top,
        right,
        bottom,
        left,
    });
}
