pub mod dom;
#[cfg(feature = "devtools")]
pub mod devtools;
pub mod canvas2d;
pub mod clipboard;
pub mod font_face;
pub mod dialog;
pub mod eventsource;
pub mod fetch;
pub mod frame_cache;
pub mod fs;
pub mod fs_watch;
pub mod history;
pub mod image_loader;
pub mod indexed_db;
pub mod ipc;
pub mod layout;
pub mod manifest;
pub mod media;
pub mod menu;
pub mod multi_window;
pub mod notification;
pub mod observers;
pub mod process;
pub mod pwa;
#[cfg(unix)]
pub mod pty;
pub mod state;
pub mod storage;
pub mod streams;
pub mod text_encoding;
pub mod timers;
pub mod websocket;
pub mod worker;

// Native capability extensions
pub use w3cos_ffi as ffi;

#[cfg(feature = "gpu")]
#[path = "render_gpu.rs"]
pub mod render;

#[cfg(feature = "cpu-render")]
#[path = "render_cpu.rs"]
pub mod render;

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
