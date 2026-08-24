pub mod animations_web;
pub mod audio_web;
mod background_image;
pub mod badging_web;
pub mod barcode_detection_web;
pub mod battery_web;
pub mod bluetooth_web;
#[cfg(feature = "dynamic-js")]
pub mod browser_controller;
pub(crate) mod browser_http_cache;
#[cfg(feature = "dynamic-js")]
pub mod browser_page_domain;
pub mod cache_web;
pub mod canvas2d;
pub mod canvas_web;
#[cfg(all(
    any(target_os = "macos", target_os = "linux", target_os = "windows"),
    not(target_env = "ohos")
))]
pub mod clipboard;
pub mod clipboard_web;
pub mod close_watcher_web;
pub mod compat_web;
pub mod compositor;
pub mod cookie_store_web;
pub mod credentials_web;
pub mod css_rules_web;
pub mod css_typed_om_web;
pub mod custom_elements_web;
pub mod device_access_web;
#[cfg(feature = "devtools")]
pub mod devtools;
pub mod dialog;
pub mod document_context;
pub mod dom;
pub mod dom_constructors;
#[cfg(feature = "dynamic-js")]
pub mod dynamic_script;
pub mod edit_context_web;
pub mod encrypted_media_web;
pub mod eventsource;
pub mod experimental_web;
pub mod fetch;
pub mod file_system_web;
pub mod files;
pub mod filter;
mod fling;
pub mod font_face;
pub mod font_loading_web;
pub mod form_data;
pub mod fragment_directive_web;
pub mod frame_cache;
pub mod fs;
#[cfg(all(
    any(target_os = "macos", target_os = "linux", target_os = "windows"),
    not(target_env = "ohos")
))]
pub mod fs_watch;
pub mod gamepad_web;
pub mod geolocation_web;
pub mod geometry_web;
#[cfg(feature = "gpu")]
pub mod gpu_filter;
#[cfg(any(target_env = "ohos", feature = "ohos-check"))]
pub mod harmony;
#[cfg(feature = "skia")]
pub mod headless;
pub mod highlight_web;
pub mod history;
mod html_compat;
mod html_fragment_policy;
mod html_parser_host;
mod html_parser_state;
mod html_tree_builder;
mod xml_tree_builder;
pub mod image_decoder_web;
pub mod image_loader;
pub mod indexed_db;
mod indexed_db_sqlite;
pub mod indexed_db_web;
#[cfg(target_os = "ios")]
mod ios_input;
#[cfg(all(
    any(target_os = "macos", target_os = "linux", target_os = "windows"),
    not(target_env = "ohos")
))]
pub mod ipc;
pub mod jsdom;
pub mod launch_handler_web;
pub mod layout;
pub mod locks_web;
pub mod manifest;
pub mod media;
pub mod media_capabilities_web;
pub mod media_devices_web;
pub mod media_recording_web;
pub mod media_session_web;
pub mod media_source_web;
pub mod menu;
pub mod midi_web;
pub mod multi_window;
pub mod navigation_web;
pub mod navigator_web;
pub mod network_information_web;
#[cfg(all(
    any(target_os = "macos", target_os = "linux", target_os = "windows"),
    not(target_env = "ohos")
))]
pub mod notification;
pub mod notification_web;
pub mod observable_web;
pub mod observers;
pub mod observers_web;
pub mod orientation_web;
mod overscroll;
pub mod paint_artifact;
pub mod retained_layers;
pub mod payment_web;
pub mod perf;
pub mod permissions_web;
pub mod presentation_web;
pub mod pressure_web;
#[cfg(all(
    any(target_os = "macos", target_os = "linux", target_os = "windows"),
    not(target_env = "ohos")
))]
pub mod process;
#[cfg(all(
    unix,
    any(target_os = "macos", target_os = "linux"),
    not(target_env = "ohos")
))]
pub mod pty;
pub mod push_web;
pub mod pwa;
pub mod reporting_web;
pub mod sanitizer_web;
pub mod scheduler_web;
pub mod screen_details_web;
pub mod sensors_web;
pub mod service_worker_web;
pub mod speech;
pub mod speech_synthesis_web;
pub mod speech_web;
pub mod state;
pub mod storage;
pub mod storage_buckets_web;
pub mod storage_manager_web;
pub mod streams;
pub mod streams_web;
pub mod svg_renderer;
pub mod svg_values_web;
pub mod text_encoding;
pub mod text_layout;
pub mod text_tracks_web;
pub mod tile_manager;
pub mod timers;
pub mod trusted_types_web;
pub mod uitest;
pub mod unsupported;
pub mod uri_codec;
pub mod user_activation_web;
pub mod user_mediated_web;
pub mod view_transition_web;
pub mod virtual_list;
pub mod wake_lock_web;
pub mod web_events;
pub mod web_nfc;
pub mod web_share;
pub mod web_transport_web;
pub mod webcodecs_web;
mod webgl_constants_generated;
pub mod webgl_web;
pub mod webgpu_web;
pub mod webrtc_web;
pub mod websocket;
pub mod webxr_web;
pub mod worker;
#[cfg(feature = "dynamic-js")]
pub(crate) mod worker_realm;
pub mod worker_web;
pub mod xhr;
pub mod xpath_web;
pub mod xslt_web;

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

#[cfg(not(target_env = "ohos"))]
pub mod window;
pub mod window_environment_web;

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
    #[cfg(target_env = "ohos")]
    {
        let _ = builder;
        anyhow::bail!("OHOS applications are driven by ArkUI XComponent surface callbacks");
    }
    #[cfg(not(target_env = "ohos"))]
    window::run_reactive(builder)
}

/// Run a W3C OS application from a static component tree (non-reactive).
pub fn run_app_static(root: Component) -> Result<()> {
    #[cfg(target_env = "ohos")]
    {
        let _ = root;
        anyhow::bail!("OHOS applications are driven by ArkUI XComponent surface callbacks");
    }
    #[cfg(not(target_env = "ohos"))]
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

/// Run a dynamic-DOM application on Android's NativeActivity event loop.
#[cfg(target_os = "android")]
pub fn run_app_on_android_dom(
    android_app: winit::platform::android::activity::AndroidApp,
    setup: fn(),
) -> Result<()> {
    if let Some(data_dir) = android_app.internal_data_path() {
        storage::set_base_dir(data_dir.join("w3cos").join("storage"));
        indexed_db::set_base_dir(data_dir.join("w3cos").join("indexeddb"));
        if let Err(error) =
            cookie_store_web::set_persistence_dir(data_dir.join("w3cos").join("cookies"))
        {
            eprintln!("[w3cos] warning: failed to load persistent cookies: {error}");
        }
    }
    window::run_dom_android(android_app, setup)
}

/// Run a W3C OS application using the dynamic DOM model.
/// The setup function builds the initial DOM tree via `w3cos_runtime::dom::*` APIs.
/// DOM mutations and signal changes trigger automatic re-rendering.
pub fn run_app_dom(setup: fn()) -> Result<()> {
    #[cfg(target_env = "ohos")]
    {
        let _ = setup;
        anyhow::bail!("OHOS applications are driven by ArkUI XComponent surface callbacks");
    }
    #[cfg(not(target_env = "ohos"))]
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
