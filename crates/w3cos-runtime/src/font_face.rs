//! CSS @font-face — font registration and loading
//!
//! Mirrors the CSS Fonts Level 4 @font-face rule:
//! https://www.w3.org/TR/css-fonts-4/#font-face-rule
//!
//! Provides a global `FontRegistry` that maps font-family names to loaded
//! font data. The renderer queries this registry when drawing text, falling
//! back to a built-in system font if the requested family is not found.
//!
//! # Example
//! ```ignore
//! // Register from file path (e.g. a bundled monospace font)
//! FontRegistry::global().register(FontFace {
//!     family: "JetBrains Mono".into(),
//!     src: FontSource::Path("/usr/share/fonts/JetBrainsMono-Regular.ttf".into()),
//!     weight: FontWeight::Normal,
//!     style: FontFaceStyle::Normal,
//!     ..Default::default()
//! }).unwrap();
//!
//! // Register from embedded bytes (zero-copy)
//! FontRegistry::global().register(FontFace {
//!     family: "JetBrains Mono".into(),
//!     src: FontSource::Bytes(include_bytes!("../fonts/JetBrainsMono-Regular.ttf").to_vec()),
//!     ..Default::default()
//! }).unwrap();
//!
//! // Query
//! let data = FontRegistry::global().resolve("JetBrains Mono", FontWeight::Normal, FontFaceStyle::Normal);
//! ```

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

// ── FontWeight ─────────────────────────────────────────────────────────────

/// CSS `font-weight` — numeric value (100–900) or keyword.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FontWeight(pub u16);

impl FontWeight {
    pub const THIN: Self = Self(100);
    pub const EXTRA_LIGHT: Self = Self(200);
    pub const LIGHT: Self = Self(300);
    pub const NORMAL: Self = Self(400);
    pub const MEDIUM: Self = Self(500);
    pub const SEMI_BOLD: Self = Self(600);
    pub const BOLD: Self = Self(700);
    pub const EXTRA_BOLD: Self = Self(800);
    pub const BLACK: Self = Self(900);

    pub fn from_str(s: &str) -> Self {
        match s.trim() {
            "thin" => Self::THIN,
            "extra-light" | "ultralight" => Self::EXTRA_LIGHT,
            "light" => Self::LIGHT,
            "normal" | "regular" => Self::NORMAL,
            "medium" => Self::MEDIUM,
            "semi-bold" | "semibold" | "demi-bold" => Self::SEMI_BOLD,
            "bold" => Self::BOLD,
            "extra-bold" | "extrabold" | "ultra-bold" => Self::EXTRA_BOLD,
            "black" | "heavy" => Self::BLACK,
            n => n.parse::<u16>().map(Self).unwrap_or(Self::NORMAL),
        }
    }
}

impl Default for FontWeight {
    fn default() -> Self {
        Self::NORMAL
    }
}

// ── FontFaceStyle ──────────────────────────────────────────────────────────

/// CSS `font-style` in a @font-face rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FontFaceStyle {
    #[default]
    Normal,
    Italic,
    Oblique,
}

impl FontFaceStyle {
    pub fn from_str(s: &str) -> Self {
        match s.trim() {
            "italic" => Self::Italic,
            "oblique" => Self::Oblique,
            _ => Self::Normal,
        }
    }
}

// ── FontDisplay ────────────────────────────────────────────────────────────

/// CSS `font-display` — controls how a font is displayed while loading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FontDisplay {
    #[default]
    Auto,
    Block,
    Swap,
    Fallback,
    Optional,
}

// ── FontSource ─────────────────────────────────────────────────────────────

/// The source of font data — file path, embedded bytes, or system font name.
#[derive(Debug, Clone)]
pub enum FontSource {
    /// Load from a file path at registration time.
    Path(PathBuf),
    /// Embedded font bytes (e.g. `include_bytes!(...)`).
    Bytes(Vec<u8>),
    /// System font name — resolved by the OS font stack.
    /// e.g. `local("Arial")`, `local("Helvetica Neue")`
    Local(String),
}

// ── FontFace ───────────────────────────────────────────────────────────────

/// CSS `@font-face` rule — registers a font family with its source and metadata.
#[derive(Debug, Clone)]
pub struct FontFace {
    /// `font-family` — the name used in CSS `font-family` properties.
    pub family: String,
    /// `src` — where to load the font data from.
    pub src: FontSource,
    /// `font-weight` — defaults to 400 (normal).
    pub weight: FontWeight,
    /// `font-style` — defaults to normal.
    pub style: FontFaceStyle,
    /// `font-display` — defaults to auto.
    pub display: FontDisplay,
    /// `unicode-range` — optional subset hint (informational, not enforced).
    pub unicode_range: Option<String>,
}

impl Default for FontFace {
    fn default() -> Self {
        Self {
            family: String::new(),
            src: FontSource::Local("sans-serif".into()),
            weight: FontWeight::NORMAL,
            style: FontFaceStyle::Normal,
            display: FontDisplay::Auto,
            unicode_range: None,
        }
    }
}

// ── Loaded font entry ──────────────────────────────────────────────────────

/// A successfully loaded font — holds the raw bytes ready for the renderer.
#[derive(Clone)]
pub struct LoadedFont {
    pub family: String,
    pub weight: FontWeight,
    pub style: FontFaceStyle,
    /// Raw font bytes (TTF / OTF / WOFF2).
    pub data: Arc<Vec<u8>>,
    /// Whether this is a monospace font (detected from family name heuristic).
    pub is_monospace: bool,
}

impl LoadedFont {
    fn detect_monospace(family: &str) -> bool {
        let lower = family.to_lowercase();
        lower.contains("mono")
            || lower.contains("courier")
            || lower.contains("consolas")
            || lower.contains("menlo")
            || lower.contains("inconsolata")
            || lower.contains("fira code")
            || lower.contains("source code")
            || lower.contains("jetbrains")
            || lower.contains("hack")
            || lower.contains("cascadia")
    }
}

// ── FontRegistry ───────────────────────────────────────────────────────────

/// Global font registry — maps `(family, weight, style)` to loaded font data.
///
/// Access via `FontRegistry::global()`. Thread-safe.
pub struct FontRegistry {
    fonts: Mutex<HashMap<FontKey, LoadedFont>>,
    /// Ordered list of registered families (for CSS `font-family` stack resolution).
    families: Mutex<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct FontKey {
    family: String,
    weight: FontWeight,
    style: FontFaceStyle,
}

static GLOBAL_REGISTRY: OnceLock<FontRegistry> = OnceLock::new();

impl FontRegistry {
    fn new() -> Self {
        Self {
            fonts: Mutex::new(HashMap::new()),
            families: Mutex::new(Vec::new()),
        }
    }

    /// Access the global font registry (lazily initialized).
    pub fn global() -> &'static FontRegistry {
        GLOBAL_REGISTRY.get_or_init(FontRegistry::new)
    }

    /// Register a `@font-face` rule. Loads font data immediately.
    pub fn register(&self, face: FontFace) -> Result<(), String> {
        let data = match &face.src {
            FontSource::Bytes(b) => Arc::new(b.clone()),
            FontSource::Path(p) => {
                let bytes = std::fs::read(p)
                    .map_err(|e| format!("font load error {}: {e}", p.display()))?;
                Arc::new(bytes)
            }
            FontSource::Local(name) => {
                // Try to resolve from common system font paths
                let resolved = resolve_system_font(name);
                match resolved {
                    Some(bytes) => Arc::new(bytes),
                    None => {
                        // Register as a placeholder — renderer will use fallback
                        Arc::new(Vec::new())
                    }
                }
            }
        };

        let is_monospace = LoadedFont::detect_monospace(&face.family);
        let key = FontKey {
            family: face.family.clone(),
            weight: face.weight,
            style: face.style,
        };

        let loaded = LoadedFont {
            family: face.family.clone(),
            weight: face.weight,
            style: face.style,
            data,
            is_monospace,
        };

        self.fonts.lock().unwrap().insert(key, loaded);

        let mut families = self.families.lock().unwrap();
        if !families.contains(&face.family) {
            families.push(face.family);
        }

        Ok(())
    }

    /// Resolve a font by family name, weight, and style.
    /// Falls back to closest weight match within the same family.
    pub fn resolve(
        &self,
        family: &str,
        weight: FontWeight,
        style: FontFaceStyle,
    ) -> Option<LoadedFont> {
        let fonts = self.fonts.lock().unwrap();

        // Exact match
        let key = FontKey {
            family: family.to_string(),
            weight,
            style,
        };
        if let Some(f) = fonts.get(&key) {
            return Some(f.clone());
        }

        // Closest weight match (CSS font matching algorithm step 3c)
        let candidates: Vec<&LoadedFont> = fonts
            .values()
            .filter(|f| f.family == family && f.style == style)
            .collect();

        if candidates.is_empty() {
            // Try ignoring style
            let any_style: Vec<&LoadedFont> =
                fonts.values().filter(|f| f.family == family).collect();
            if any_style.is_empty() {
                return None;
            }
            return Some(closest_weight(any_style, weight).clone());
        }

        Some(closest_weight(candidates, weight).clone())
    }

    /// Resolve a CSS `font-family` stack (comma-separated families).
    /// Returns the first family that has a registered font.
    pub fn resolve_stack(
        &self,
        stack: &str,
        weight: FontWeight,
        style: FontFaceStyle,
    ) -> Option<LoadedFont> {
        for family in stack.split(',') {
            let family = family.trim().trim_matches('"').trim_matches('\'');
            if let Some(f) = self.resolve(family, weight, style) {
                return Some(f);
            }
        }
        None
    }

    /// List all registered family names.
    pub fn families(&self) -> Vec<String> {
        self.families.lock().unwrap().clone()
    }

    /// Returns true if any monospace font is registered.
    pub fn has_monospace(&self) -> bool {
        self.fonts.lock().unwrap().values().any(|f| f.is_monospace)
    }

    /// Get the first registered monospace font (for code editors).
    pub fn default_monospace(&self) -> Option<LoadedFont> {
        self.fonts
            .lock()
            .unwrap()
            .values()
            .find(|f| f.is_monospace)
            .cloned()
    }
}

fn closest_weight<'a>(candidates: Vec<&'a LoadedFont>, target: FontWeight) -> &'a LoadedFont {
    candidates
        .into_iter()
        .min_by_key(|f| (f.weight.0 as i32 - target.0 as i32).unsigned_abs())
        .unwrap()
}

// ── System font resolution ─────────────────────────────────────────────────

/// Try to load a system font by name from common OS font directories.
fn resolve_system_font(name: &str) -> Option<Vec<u8>> {
    let search_dirs: &[&str] = &[
        // macOS
        "/System/Library/Fonts",
        "/Library/Fonts",
        "~/Library/Fonts",
        // Linux
        "/usr/share/fonts",
        "/usr/local/share/fonts",
        "~/.fonts",
        "~/.local/share/fonts",
        // Windows
        "C:\\Windows\\Fonts",
    ];

    let name_lower = name.to_lowercase().replace(' ', "");
    let extensions = ["ttf", "otf", "ttc", "woff2", "woff"];

    for dir in search_dirs {
        let dir = dir.replace('~', &std::env::var("HOME").unwrap_or_default());
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let fname = entry.file_name().to_string_lossy().to_lowercase();
                let stem = fname
                    .rsplit_once('.')
                    .map(|(s, _)| s)
                    .unwrap_or(&fname)
                    .replace(['-', '_', ' '], "");
                let ext = fname.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
                if extensions.contains(&ext) && stem.contains(&name_lower) {
                    if let Ok(bytes) = std::fs::read(entry.path()) {
                        return Some(bytes);
                    }
                }
            }
        }
    }
    None
}

// ── CSS @font-face parser ──────────────────────────────────────────────────

/// Parse a CSS `@font-face { ... }` block and register it.
///
/// Supports: `font-family`, `src: url(...)`, `src: local(...)`,
/// `font-weight`, `font-style`, `font-display`, `unicode-range`.
pub fn parse_and_register(css_block: &str) -> Result<(), String> {
    let mut family = String::new();
    let mut src: Option<FontSource> = None;
    let mut weight = FontWeight::NORMAL;
    let mut style = FontFaceStyle::Normal;
    let mut display = FontDisplay::Auto;
    let mut unicode_range: Option<String> = None;

    for line in css_block.lines() {
        let line = line.trim().trim_end_matches(';');
        if let Some(val) = strip_property(line, "font-family") {
            family = val.trim_matches('"').trim_matches('\'').to_string();
        } else if let Some(val) = strip_property(line, "src") {
            if val.starts_with("url(") {
                let url = val
                    .trim_start_matches("url(")
                    .trim_end_matches(')')
                    .trim_matches('"')
                    .trim_matches('\'');
                src = Some(FontSource::Path(url.into()));
            } else if val.starts_with("local(") {
                let name = val
                    .trim_start_matches("local(")
                    .trim_end_matches(')')
                    .trim_matches('"')
                    .trim_matches('\'');
                src = Some(FontSource::Local(name.to_string()));
            }
        } else if let Some(val) = strip_property(line, "font-weight") {
            weight = FontWeight::from_str(val);
        } else if let Some(val) = strip_property(line, "font-style") {
            style = FontFaceStyle::from_str(val);
        } else if let Some(val) = strip_property(line, "font-display") {
            display = match val {
                "block" => FontDisplay::Block,
                "swap" => FontDisplay::Swap,
                "fallback" => FontDisplay::Fallback,
                "optional" => FontDisplay::Optional,
                _ => FontDisplay::Auto,
            };
        } else if let Some(val) = strip_property(line, "unicode-range") {
            unicode_range = Some(val.to_string());
        }
    }

    if family.is_empty() {
        return Err("@font-face missing font-family".into());
    }

    let face = FontFace {
        family,
        src: src.unwrap_or(FontSource::Local("sans-serif".into())),
        weight,
        style,
        display,
        unicode_range,
    };

    FontRegistry::global().register(face)
}

fn strip_property<'a>(line: &'a str, prop: &str) -> Option<&'a str> {
    let prefix = format!("{}:", prop);
    line.strip_prefix(&prefix).map(|v| v.trim())
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_weight_from_str() {
        assert_eq!(FontWeight::from_str("bold"), FontWeight::BOLD);
        assert_eq!(FontWeight::from_str("700"), FontWeight::BOLD);
        assert_eq!(FontWeight::from_str("normal"), FontWeight::NORMAL);
        assert_eq!(FontWeight::from_str("400"), FontWeight::NORMAL);
    }

    #[test]
    fn register_bytes_font() {
        // Minimal valid TTF header (just enough to not crash)
        let fake_ttf = vec![0u8; 12];
        let registry = FontRegistry::new();
        registry
            .register(FontFace {
                family: "TestMono".into(),
                src: FontSource::Bytes(fake_ttf),
                weight: FontWeight::NORMAL,
                style: FontFaceStyle::Normal,
                ..Default::default()
            })
            .unwrap();

        let resolved = registry.resolve("TestMono", FontWeight::NORMAL, FontFaceStyle::Normal);
        assert!(resolved.is_some());
        assert!(resolved.unwrap().is_monospace);
    }

    #[test]
    fn closest_weight_fallback() {
        let registry = FontRegistry::new();
        // Register only bold
        registry
            .register(FontFace {
                family: "TestFont".into(),
                src: FontSource::Bytes(vec![]),
                weight: FontWeight::BOLD,
                style: FontFaceStyle::Normal,
                ..Default::default()
            })
            .unwrap();

        // Request normal — should get bold as closest
        let resolved = registry.resolve("TestFont", FontWeight::NORMAL, FontFaceStyle::Normal);
        assert!(resolved.is_some());
        assert_eq!(resolved.unwrap().weight, FontWeight::BOLD);
    }

    #[test]
    fn parse_font_face_css() {
        let css = r#"
            font-family: "MyFont";
            src: local(Arial);
            font-weight: bold;
            font-style: italic;
        "#;
        // Should not error
        let result = parse_and_register(css);
        assert!(result.is_ok(), "{:?}", result);
    }

    #[test]
    fn resolve_stack() {
        let registry = FontRegistry::new();
        registry
            .register(FontFace {
                family: "Fallback".into(),
                src: FontSource::Bytes(vec![]),
                weight: FontWeight::NORMAL,
                style: FontFaceStyle::Normal,
                ..Default::default()
            })
            .unwrap();

        let resolved = registry.resolve_stack(
            "\"Missing Font\", Fallback, sans-serif",
            FontWeight::NORMAL,
            FontFaceStyle::Normal,
        );
        assert!(resolved.is_some());
        assert_eq!(resolved.unwrap().family, "Fallback");
    }
}

// ── FontFaceSet (document.fonts) ───────────────────────────────────────────

/// W3C `FontFaceSet` — the `document.fonts` interface.
/// https://www.w3.org/TR/css-font-loading/#fontfaceset
///
/// Provides the `ready` promise semantics that CodeMirror uses:
/// ```typescript
/// if (document.fonts?.ready) document.fonts.ready.then(() => { ... })
/// ```
///
/// In w3cos, font loading is synchronous (fonts are registered before the
/// document is rendered), so `ready` resolves immediately. Callbacks
/// registered via `then()` are called synchronously on the next `flush()`.
pub struct FontFaceSet {
    /// Callbacks registered via `ready.then(cb)`.
    ready_callbacks: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
    /// Whether all fonts have been loaded (always true after `mark_ready()`).
    is_ready: std::sync::atomic::AtomicBool,
}

impl FontFaceSet {
    pub fn new() -> Self {
        Self {
            ready_callbacks: Mutex::new(Vec::new()),
            is_ready: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Global singleton — `document.fonts`.
    pub fn global() -> &'static FontFaceSet {
        static INSTANCE: OnceLock<FontFaceSet> = OnceLock::new();
        INSTANCE.get_or_init(FontFaceSet::new)
    }

    /// Returns true if all fonts are loaded (mirrors `FontFaceSet.status == "loaded"`).
    pub fn is_ready(&self) -> bool {
        self.is_ready.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Register a callback to be called when fonts are ready.
    /// If already ready, the callback is queued for the next `flush()`.
    pub fn ready_then(&self, cb: impl FnOnce() + Send + 'static) {
        self.ready_callbacks.lock().unwrap().push(Box::new(cb));
        // If already ready, flush immediately on next poll.
    }

    /// Mark all fonts as loaded and flush pending ready callbacks.
    /// The runtime should call this after the initial font registration pass.
    pub fn mark_ready(&self) {
        self.is_ready
            .store(true, std::sync::atomic::Ordering::Release);
        self.flush_ready_callbacks();
    }

    /// Drain and invoke all pending ready callbacks.
    /// Call this from the main event loop after font loading completes.
    pub fn flush_ready_callbacks(&self) {
        let callbacks: Vec<_> = {
            let mut guard = self.ready_callbacks.lock().unwrap();
            std::mem::take(&mut *guard)
        };
        for cb in callbacks {
            cb();
        }
    }

    /// Add a `FontFace` to the set and register it in the global `FontRegistry`.
    pub fn add(&self, face: FontFace) -> Result<(), String> {
        FontRegistry::global().register(face)
    }

    /// Check if a font matching the given family/weight/style is available.
    pub fn check(&self, family: &str, weight: FontWeight, style: FontFaceStyle) -> bool {
        FontRegistry::global()
            .resolve(family, weight, style)
            .is_some()
    }
}

impl Default for FontFaceSet {
    fn default() -> Self {
        Self::new()
    }
}
