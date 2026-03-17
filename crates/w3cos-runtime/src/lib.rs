pub mod layout;
pub mod render;
pub mod window;

use anyhow::Result;
use w3cos_std::Component;

/// Run a W3C OS application from a root component tree.
pub fn run_app(root: Component) -> Result<()> {
    window::run(root)
}
