pub mod layout;
pub mod render;
pub mod state;
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
