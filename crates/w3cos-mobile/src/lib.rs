//! Mobile platform layer for W3C OS — RN-like shell host integration.
//!
//! - **Desktop dev:** `run_mobile_app` delegates to `w3cos_runtime::run_app` (same as `w3cos build`).
//! - **Android (M1+):** NDK surface + touch via [`android`] module and `templates/android/`.
//!
//! Generic only — no product-specific apps in this crate.

pub mod lifecycle;
pub mod manifest;
pub mod safe_area;
pub mod touch;

#[cfg(target_os = "android")]
pub mod android;

use anyhow::Result;
use w3cos_std::Component;

/// Run a mobile application. Uses the reactive component builder (same as desktop).
///
/// On desktop targets this is a dev convenience until the Android/iOS backend is linked.
pub fn run_mobile_app(builder: fn() -> Component) -> Result<()> {
    #[cfg(target_os = "android")]
    {
        return android::run(builder);
    }

    #[cfg(not(target_os = "android"))]
    {
        #[cfg(target_os = "ios")]
        w3cos_std::safe_area::set_enabled(true);
        w3cos_runtime::run_app(builder)
    }
}

/// C ABI entry for Android shell (`templates/android` loads `libw3cos_mobile.so`).
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub extern "C" fn w3cos_mobile_run() -> i32 {
    match android::run_from_shell() {
        Ok(()) => 0,
        Err(e) => {
            log::error!("w3cos_mobile_run failed: {e:#}");
            1
        }
    }
}
