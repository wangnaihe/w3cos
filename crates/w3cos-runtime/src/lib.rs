pub mod canvas2d;
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
pub mod clipboard;
pub mod compositor;
#[cfg(feature = "devtools")]
pub mod devtools;
pub mod dialog;
pub mod dom;
pub mod eventsource;
pub mod fetch;
pub mod filter;
mod fling;
pub mod font_face;
pub mod frame_cache;
pub mod fs;
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
pub mod fs_watch;
#[cfg(feature = "gpu")]
pub mod gpu_filter;
pub mod history;
pub mod image_loader;
pub mod indexed_db;
#[cfg(target_os = "ios")]
mod ios_input;
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
pub mod ipc;
pub mod jsdom;
pub mod layout;
pub mod manifest;
pub mod media;
pub mod menu;
pub mod multi_window;
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
pub mod notification;
pub mod observers;
mod overscroll;
pub mod paint_artifact;
pub mod perf;
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
pub mod process;
#[cfg(all(unix, any(target_os = "macos", target_os = "linux")))]
pub mod pty;
pub mod pwa;
pub mod speech;
pub mod state;
pub mod storage;
pub mod streams;
pub mod text_encoding;
pub mod text_layout;
pub mod tile_manager;
pub mod timers;
pub mod uitest;
pub mod virtual_list;
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
pub mod websocket;
pub mod worker;

// Native capability extensions
pub use w3cos_ffi as ffi;

// Runtime stylesheet registry (ESM CSS imports baked into the bundle).
pub use w3cos_dom::stylesheet;

#[cfg(feature = "gpu")]
#[path = "render_gpu.rs"]
pub mod render_gpu;

#[cfg(feature = "cpu-render")]
#[path = "render_cpu.rs"]
pub mod render_cpu;

#[cfg(feature = "skia")]
#[path = "render_skia.rs"]
pub mod render_skia;

#[cfg(all(feature = "skia", target_os = "android"))]
mod render_skia_vulkan;

#[cfg(all(feature = "gpu", not(feature = "cpu-render")))]
pub use render_gpu as render;

#[cfg(all(feature = "cpu-render", not(feature = "gpu")))]
pub use render_cpu as render;

pub mod window;

use anyhow::Result;
use w3cos_std::Component;

/// Enable the AI Bridge HTTP server by setting the W3COS_AI_PORT environment variable.
/// The server will start when the application window is created.
///
/// Example: `enable_ai_bridge(9222)` starts the server on `http://127.0.0.1:9222`
pub fn enable_ai_bridge(port: u16) {
    unsafe { std::env::set_var("W3COS_AI_PORT", port.to_string()) };
}

/// Run a W3C OS application with a reactive builder function.
/// The builder is re-called whenever signals change, producing a new component tree.
pub fn run_app(builder: fn() -> Component) -> Result<()> {
    window::run_reactive(builder)
}

/// Run a W3C OS application from a static component tree (non-reactive).
pub fn run_app_static(root: Component) -> Result<()> {
    window::run_static(root)
}

/// Run on Android with the activity-provided [`AndroidApp`] handle (NativeActivity entry).
#[cfg(target_os = "android")]
pub fn run_app_on_android(
    android_app: winit::platform::android::activity::AndroidApp,
    builder: fn() -> Component,
) -> Result<()> {
    window::run_reactive_android(android_app, builder)
}

/// Run a W3C OS application using the dynamic DOM model.
/// The setup function builds the initial DOM tree via `w3cos_runtime::dom::*` APIs.
/// DOM mutations and signal changes trigger automatic re-rendering.
pub fn run_app_dom(setup: fn()) -> Result<()> {
    window::run_dom(setup)
}

#[cfg(test)]
mod tests {
    #[test]
    fn stylesheet_registry_is_reexported_for_generated_bundles() {
        // Generated esm_bundle.rs calls w3cos_runtime::stylesheet::register_rule.
        crate::stylesheet::clear_rules();
        crate::stylesheet::register_rule(
            ".monaco-editor .find-widget",
            &[("position", "absolute")],
        );
        let ancestors = [crate::stylesheet::SelectorContext::new(
            "div",
            None,
            &["monaco-editor"],
        )];
        let matched =
            crate::stylesheet::matching_declarations("div", None, &["find-widget"], &ancestors);
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].0, "position");
        assert_eq!(matched[0].1, "absolute");
        crate::stylesheet::clear_rules();
    }
}
