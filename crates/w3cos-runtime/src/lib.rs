pub mod fetch;
pub mod history;
pub mod layout;
pub mod notification;
pub mod state;

#[cfg(feature = "gpu")]
#[path = "render_gpu.rs"]
pub mod render;

#[cfg(feature = "cpu-render")]
#[path = "render_cpu.rs"]
pub mod render;

pub mod window;

use anyhow::Result;
use w3cos_std::Component;

/// Run a W3C OS application with a reactive builder function.
/// The builder is re-called whenever signals change, producing a new component tree.
pub fn run_app(builder: fn() -> Component) -> Result<()> {
    window::run_reactive(builder)
}

/// Run a W3C OS application from a static component tree (non-reactive).
pub fn run_app_static(root: Component) -> Result<()> {
    window::run_static(root)
}
