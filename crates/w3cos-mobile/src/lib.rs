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
#[cfg(target_os = "ios")]
use std::path::PathBuf;
use w3cos_std::Component;

#[cfg(target_os = "ios")]
fn configure_ios_data_directory() {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return;
    };
    let data_dir = home
        .join("Library")
        .join("Application Support")
        .join("w3cos");
    w3cos_runtime::storage::set_base_dir(data_dir.join("storage"));
    w3cos_runtime::indexed_db::set_base_dir(data_dir.join("indexeddb"));
}

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
        {
            configure_ios_data_directory();
            w3cos_std::safe_area::set_enabled(true);
        }
        w3cos_runtime::run_app(builder)
    }
}

/// Run a mobile application backed by the dynamic W3C DOM.
pub fn run_mobile_app_dom(setup: fn()) -> Result<()> {
    #[cfg(target_os = "android")]
    {
        // NativeActivity supplies its AndroidApp through `android_main`; this
        // function is used by iOS and desktop entry points.
        return w3cos_runtime::run_app_dom(setup);
    }

    #[cfg(not(target_os = "android"))]
    {
        #[cfg(target_os = "ios")]
        {
            configure_ios_data_directory();
            w3cos_std::safe_area::set_enabled(true);
        }
        w3cos_runtime::run_app_dom(setup)
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
