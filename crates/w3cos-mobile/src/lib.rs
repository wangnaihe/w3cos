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
#[cfg(any(target_env = "ohos", feature = "ohos-check"))]
pub mod harmony;

use anyhow::Result;
#[cfg(target_os = "ios")]
use std::path::PathBuf;
use w3cos_std::Component;

pub(crate) fn configure_mobile_web_capabilities() {
    #[cfg(any(target_os = "android", target_os = "ios", target_env = "ohos"))]
    {
        static CONFIGURE: std::sync::Once = std::sync::Once::new();
        CONFIGURE.call_once(|| {
            const MOBILE_TOUCH_FALLBACK: u32 = 5;
            eprintln!(
                "W3COS warning: navigator.maxTouchPoints uses mobile fallback {MOBILE_TOUCH_FALLBACK}; exact hardware reporting is pending"
            );
            w3cos_runtime::jsdom::set_max_touch_points(MOBILE_TOUCH_FALLBACK);
        });
    }
}

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
    if std::env::var("W3COS_UITEST").ok().as_deref() == Some("1")
        && std::env::var("W3COS_CLEAR_STORAGE").ok().as_deref() == Some("1")
    {
        w3cos_runtime::storage::clear();
    }
}

/// Run a mobile application. Uses the reactive component builder (same as desktop).
///
/// On desktop targets this is a dev convenience until the Android/iOS backend is linked.
pub fn run_mobile_app(builder: fn() -> Component) -> Result<()> {
    configure_mobile_web_capabilities();
    #[cfg(target_os = "android")]
    {
        return android::run(builder);
    }

    #[cfg(all(not(target_os = "android"), not(target_env = "ohos")))]
    {
        #[cfg(target_os = "ios")]
        {
            configure_ios_data_directory();
            w3cos_std::safe_area::set_enabled(true);
        }
        w3cos_runtime::run_app(builder)
    }
    #[cfg(target_env = "ohos")]
    {
        let _ = builder;
        anyhow::bail!("HarmonyOS apps start from an ArkUI XComponent surface callback")
    }
}

/// Run a mobile application backed by the dynamic W3C DOM.
pub fn run_mobile_app_dom(setup: fn()) -> Result<()> {
    configure_mobile_web_capabilities();
    #[cfg(target_os = "android")]
    {
        // NativeActivity supplies its AndroidApp through `android_main`; this
        // function is used by iOS and desktop entry points.
        return w3cos_runtime::run_app_dom(setup);
    }

    #[cfg(all(not(target_os = "android"), not(target_env = "ohos")))]
    {
        #[cfg(target_os = "ios")]
        {
            configure_ios_data_directory();
            w3cos_std::safe_area::set_enabled(true);
        }
        w3cos_runtime::run_app_dom(setup)
    }
    #[cfg(target_env = "ohos")]
    {
        let _ = setup;
        anyhow::bail!("HarmonyOS apps start from an ArkUI XComponent surface callback")
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
