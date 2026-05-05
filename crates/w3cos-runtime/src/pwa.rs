//! PWA Web App Manifest support.
//!
//! Parses the [W3C Web App Manifest] (commonly served as `manifest.webmanifest`
//! or `manifest.json`) and converts it into a W3C OS [`AppManifest`] so a
//! Progressive Web App can be installed as a native W3C OS application.
//!
//! ```ignore
//! use w3cos_runtime::pwa::PwaManifest;
//!
//! let json = r#"{
//!     "name": "Pixel Editor",
//!     "short_name": "Pixel",
//!     "start_url": "/",
//!     "display": "standalone",
//!     "theme_color": "#202020",
//!     "icons": [
//!         { "src": "/icon-192.png", "sizes": "192x192", "type": "image/png" }
//!     ]
//! }"#;
//!
//! let pwa = PwaManifest::from_json(json).unwrap();
//! let app = pwa.into_app_manifest("pixel-editor");
//! assert_eq!(app.name, "Pixel Editor");
//! ```
//!
//! [W3C Web App Manifest]: https://www.w3.org/TR/appmanifest/

use serde::{Deserialize, Serialize};

use crate::manifest::{AppManifest, WindowConfig};

/// W3C `display` enumeration: how the app is presented to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DisplayMode {
    /// Standalone window, no browser chrome (default for installed apps).
    #[default]
    Standalone,
    /// Full-screen — hides all OS chrome including the taskbar.
    Fullscreen,
    /// Browser-like window with minimal chrome (back/forward).
    MinimalUi,
    /// Open inside a browser tab.
    Browser,
    /// Newer `display_override` value: tabbed PWA window.
    Tabbed,
    /// Newer `display_override` value: window-controls-overlay layout.
    WindowControlsOverlay,
}

/// W3C `orientation` enumeration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Orientation {
    #[default]
    Any,
    Natural,
    Landscape,
    LandscapePrimary,
    LandscapeSecondary,
    Portrait,
    PortraitPrimary,
    PortraitSecondary,
}

/// One image entry from the `icons` array.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImageResource {
    pub src: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sizes: Option<String>,
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl ImageResource {
    /// Best-effort: parse the `sizes` field and return the largest square
    /// dimension. `"any"` maps to `u32::MAX`.
    pub fn largest_square(&self) -> Option<u32> {
        let raw = self.sizes.as_deref()?;
        let mut best = 0u32;
        for token in raw.split_whitespace() {
            if token.eq_ignore_ascii_case("any") {
                return Some(u32::MAX);
            }
            if let Some((w, h)) = token.split_once(['x', 'X']) {
                let w: u32 = w.parse().ok()?;
                let h: u32 = h.parse().ok()?;
                let s = w.min(h);
                if s > best {
                    best = s;
                }
            }
        }
        if best == 0 { None } else { Some(best) }
    }
}

/// W3C `shortcuts` member entry — surfaced as system jump-list items.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Shortcut {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub url: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub icons: Vec<ImageResource>,
}

/// Parsed W3C Web App Manifest.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PwaManifest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// W3C `id` member — a stable identity for the PWA. When absent we
    /// derive an id from the `start_url` per the spec.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default)]
    pub display: DisplayMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub display_override: Vec<DisplayMode>,
    #[serde(default)]
    pub orientation: Orientation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "dir")]
    pub direction: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub icons: Vec<ImageResource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub screenshots: Vec<ImageResource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shortcuts: Vec<Shortcut>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<String>,
    /// Installed PWAs may declare extra `permissions` (matches the proposed
    /// W3C `permissions` policy member, treated permissively here).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<String>,
}

/// Errors returned by [`PwaManifest::from_json`] / [`PwaManifest::from_file`].
#[derive(Debug)]
pub enum PwaError {
    Io(std::io::Error),
    Parse(serde_json::Error),
    /// Manifest has neither `name` nor `short_name` — required by the spec.
    MissingName,
}

impl std::fmt::Display for PwaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PwaError::Io(e) => write!(f, "io error: {e}"),
            PwaError::Parse(e) => write!(f, "json parse error: {e}"),
            PwaError::MissingName => write!(f, "manifest missing both `name` and `short_name`"),
        }
    }
}

impl std::error::Error for PwaError {}

impl From<std::io::Error> for PwaError {
    fn from(e: std::io::Error) -> Self {
        PwaError::Io(e)
    }
}

impl From<serde_json::Error> for PwaError {
    fn from(e: serde_json::Error) -> Self {
        PwaError::Parse(e)
    }
}

impl PwaManifest {
    /// Parse a JSON string. Returns [`PwaError::MissingName`] when neither
    /// `name` nor `short_name` is present (the spec requires at least one).
    pub fn from_json(json: &str) -> Result<Self, PwaError> {
        let manifest: PwaManifest = serde_json::from_str(json)?;
        if manifest.name.is_none() && manifest.short_name.is_none() {
            return Err(PwaError::MissingName);
        }
        Ok(manifest)
    }

    /// Read & parse a `manifest.json` / `manifest.webmanifest` file.
    pub fn from_file(path: impl AsRef<std::path::Path>) -> Result<Self, PwaError> {
        let bytes = std::fs::read_to_string(path)?;
        Self::from_json(&bytes)
    }

    /// Display name — `name` if available, else `short_name`, else fallback.
    pub fn display_name(&self) -> &str {
        self.name
            .as_deref()
            .or(self.short_name.as_deref())
            .unwrap_or("Web App")
    }

    /// Pick the icon best matching the desired square size in CSS pixels.
    /// Prefers `purpose: any` icons; falls back to any icon.
    pub fn pick_icon(&self, target_px: u32) -> Option<&ImageResource> {
        let preferred = |icon: &ImageResource| {
            icon.purpose
                .as_deref()
                .map(|p| p.split_whitespace().any(|t| t.eq_ignore_ascii_case("any")))
                .unwrap_or(true)
        };

        let mut best: Option<&ImageResource> = None;
        let mut best_distance = u64::MAX;
        for icon in &self.icons {
            if !preferred(icon) {
                continue;
            }
            let size = icon.largest_square().unwrap_or(0);
            let dist = (size as i64 - target_px as i64).unsigned_abs();
            if dist < best_distance {
                best = Some(icon);
                best_distance = dist;
            }
        }
        best.or_else(|| self.icons.first())
    }

    /// Convert into a W3C OS [`AppManifest`]. The supplied `app_id` is used
    /// when the manifest does not declare one explicitly.
    pub fn into_app_manifest(self, fallback_app_id: impl Into<String>) -> AppManifest {
        let id = self
            .id
            .clone()
            .or_else(|| derive_id_from_start_url(self.start_url.as_deref()))
            .unwrap_or_else(|| fallback_app_id.into());

        let icon_192 = self
            .pick_icon(192)
            .map(|icon| icon.src.clone())
            .or_else(|| self.icons.first().map(|i| i.src.clone()));

        let entry = self
            .start_url
            .clone()
            .unwrap_or_else(|| "/".to_string());

        let frame = !matches!(self.display, DisplayMode::Fullscreen);

        AppManifest {
            id,
            name: self.display_name().to_string(),
            version: "0.1.0".to_string(),
            entry,
            icon: icon_192,
            permissions: self.permissions.clone(),
            window: WindowConfig {
                title: self.name.clone().or_else(|| self.short_name.clone()),
                frame,
                ..WindowConfig::default()
            },
        }
    }

    /// Compute the "effective display mode" honouring `display_override`
    /// per [the spec](https://www.w3.org/TR/manifest-app-info/).
    pub fn effective_display(&self) -> DisplayMode {
        // Standalone is the lowest fallback for known modes.
        let known = |m: &DisplayMode| {
            matches!(
                m,
                DisplayMode::Fullscreen
                    | DisplayMode::Standalone
                    | DisplayMode::MinimalUi
                    | DisplayMode::Browser
                    | DisplayMode::Tabbed
                    | DisplayMode::WindowControlsOverlay
            )
        };
        for mode in &self.display_override {
            if known(mode) {
                return *mode;
            }
        }
        self.display
    }
}

fn derive_id_from_start_url(start_url: Option<&str>) -> Option<String> {
    let raw = start_url?;
    let trimmed = raw.trim_start_matches('/').trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    let slug: String = trimmed
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() { None } else { Some(slug) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PIXEL_EDITOR: &str = r##"{
        "name": "Pixel Editor",
        "short_name": "Pixel",
        "start_url": "/editor",
        "display": "standalone",
        "theme_color": "#202020",
        "background_color": "#ffffff",
        "icons": [
            {"src": "/icon-48.png",  "sizes": "48x48",   "type": "image/png"},
            {"src": "/icon-192.png", "sizes": "192x192", "type": "image/png"},
            {"src": "/icon-512.png", "sizes": "512x512", "type": "image/png", "purpose": "any maskable"}
        ],
        "shortcuts": [
            {"name": "New Drawing", "url": "/editor?new=true"}
        ]
    }"##;

    #[test]
    fn parses_pixel_editor() {
        let m = PwaManifest::from_json(PIXEL_EDITOR).unwrap();
        assert_eq!(m.display_name(), "Pixel Editor");
        assert_eq!(m.short_name.as_deref(), Some("Pixel"));
        assert_eq!(m.start_url.as_deref(), Some("/editor"));
        assert_eq!(m.display, DisplayMode::Standalone);
        assert_eq!(m.icons.len(), 3);
        assert_eq!(m.shortcuts.len(), 1);
    }

    #[test]
    fn missing_name_is_rejected() {
        let bad = r#"{"start_url": "/"}"#;
        match PwaManifest::from_json(bad) {
            Err(PwaError::MissingName) => {}
            other => panic!("expected MissingName, got {other:?}"),
        }
    }

    #[test]
    fn picks_closest_icon_size() {
        let m = PwaManifest::from_json(PIXEL_EDITOR).unwrap();
        let icon = m.pick_icon(192).expect("icon at 192");
        assert_eq!(icon.src, "/icon-192.png");
        let icon = m.pick_icon(64).expect("icon at 64 -> 48");
        assert_eq!(icon.src, "/icon-48.png");
        let icon = m.pick_icon(700).expect("icon at 700 -> 512");
        assert_eq!(icon.src, "/icon-512.png");
    }

    #[test]
    fn into_app_manifest_uses_start_url_for_id() {
        let m = PwaManifest::from_json(PIXEL_EDITOR).unwrap();
        let app = m.into_app_manifest("fallback");
        assert_eq!(app.id, "editor");
        assert_eq!(app.name, "Pixel Editor");
        assert_eq!(app.entry, "/editor");
        assert_eq!(app.icon.as_deref(), Some("/icon-192.png"));
        assert!(app.window.frame);
    }

    #[test]
    fn fullscreen_disables_window_frame() {
        let json = r#"{"name": "Game", "display": "fullscreen", "start_url": "/"}"#;
        let m = PwaManifest::from_json(json).unwrap();
        let app = m.into_app_manifest("game");
        assert!(!app.window.frame);
    }

    #[test]
    fn explicit_id_overrides_start_url() {
        let json = r#"{"name": "App", "id": "my-pwa", "start_url": "/foo"}"#;
        let m = PwaManifest::from_json(json).unwrap();
        let app = m.into_app_manifest("fallback");
        assert_eq!(app.id, "my-pwa");
    }

    #[test]
    fn fallback_id_when_no_start_url() {
        let json = r#"{"name": "Solo"}"#;
        let m = PwaManifest::from_json(json).unwrap();
        let app = m.into_app_manifest("fallback-id");
        assert_eq!(app.id, "fallback-id");
    }

    #[test]
    fn display_override_picks_first_known() {
        let json = r#"{
            "name": "App",
            "display": "browser",
            "display_override": ["window-controls-overlay", "minimal-ui"]
        }"#;
        let m = PwaManifest::from_json(json).unwrap();
        assert_eq!(m.effective_display(), DisplayMode::WindowControlsOverlay);
    }

    #[test]
    fn largest_square_handles_any() {
        let icon = ImageResource {
            src: "x".into(),
            sizes: Some("any".into()),
            ..Default::default()
        };
        assert_eq!(icon.largest_square(), Some(u32::MAX));

        let icon = ImageResource {
            src: "x".into(),
            sizes: Some("48x48 96x96".into()),
            ..Default::default()
        };
        assert_eq!(icon.largest_square(), Some(96));
    }

    #[test]
    fn from_file_round_trip() {
        let path = std::env::temp_dir().join("w3cos-pwa-test.json");
        std::fs::write(&path, PIXEL_EDITOR).unwrap();
        let m = PwaManifest::from_file(&path).unwrap();
        assert_eq!(m.display_name(), "Pixel Editor");
        std::fs::remove_file(path).ok();
    }
}
