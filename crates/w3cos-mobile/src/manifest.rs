//! `w3cos.app.json` — mobile app manifest (extends PWA manifest fields).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MobileShellConfig {
    #[serde(default = "default_status_bar")]
    pub status_bar_style: String,
    #[serde(default = "default_content_slot")]
    pub content_slot: String,
}

fn default_status_bar() -> String {
    "light".into()
}

fn default_content_slot() -> String {
    "root".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MobileAppManifest {
    pub name: String,
    pub bundle_id: String,
    pub entry: String,
    #[serde(default = "default_orientation")]
    pub orientation: String,
    #[serde(default)]
    pub safe_area: bool,
    #[serde(default)]
    pub ai_bridge_port: Option<u16>,
    #[serde(default)]
    pub shell: MobileShellConfig,
}

fn default_orientation() -> String {
    "portrait".into()
}

impl MobileAppManifest {
    pub fn from_file(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read manifest {}", path.display()))?;
        serde_json::from_str(&text).context("parse w3cos.app.json")
    }
}
