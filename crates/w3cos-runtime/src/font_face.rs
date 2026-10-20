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

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

#[path = "system_fonts.rs"]
mod system_fonts;

/// Convert a browser font container into the sfnt bytes consumed by layout and
/// every renderer. Keeping this at the registry boundary prevents the browser
/// loader and native callers from growing separate font implementations.
pub(crate) fn normalize_font_bytes(bytes: &[u8]) -> Result<Vec<u8>, String> {
    normalize_font_bytes_with_limit(bytes, 64 * 1024 * 1024)
}

pub(crate) fn normalize_font_bytes_with_limit(
    bytes: &[u8],
    max_decoded_bytes: usize,
) -> Result<Vec<u8>, String> {
    let compressed = matches!(bytes.get(..4), Some(b"wOFF" | b"wOF2"));
    if compressed {
        let declared_size = bytes
            .get(16..20)
            .and_then(|raw| raw.try_into().ok())
            .map(u32::from_be_bytes)
            .ok_or_else(|| "truncated WOFF header".to_string())?
            as usize;
        if declared_size > max_decoded_bytes {
            return Err(format!(
                "decoded font exceeds source limit ({declared_size} > {max_decoded_bytes} bytes)"
            ));
        }
    }
    let decoded = match bytes.get(..4) {
        Some(b"wOFF") => {
            #[cfg(feature = "dynamic-js")]
            {
                wuff::decompress_woff1(bytes)
                    .map_err(|error| format!("WOFF decode failed: {error:?}"))
            }
            #[cfg(not(feature = "dynamic-js"))]
            {
                Err("WOFF decoding requires the dynamic-js browser feature".to_string())
            }
        }
        Some(b"wOF2") => {
            #[cfg(feature = "dynamic-js")]
            {
                wuff::decompress_woff2(bytes)
                    .map_err(|error| format!("WOFF2 decode failed: {error:?}"))
            }
            #[cfg(not(feature = "dynamic-js"))]
            {
                Err("WOFF2 decoding requires the dynamic-js browser feature".to_string())
            }
        }
        _ => Ok(bytes.to_vec()),
    }?;
    if decoded.len() > max_decoded_bytes {
        return Err(format!(
            "decoded font exceeds source limit ({} > {max_decoded_bytes} bytes)",
            decoded.len()
        ));
    }
    Ok(decoded)
}

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
    /// `unicode-range` — optional subset bounds enforced during glyph selection.
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

thread_local! {
    static FACE_HIT: RefCell<Option<(u64, LoadedFont)>> = const { RefCell::new(None) };
}

fn face_hit_key(family: &str, weight: FontWeight, style: FontFaceStyle) -> u64 {
    let mut hasher = w3cos_std::fxhash::FxHasher::default();
    family.hash(&mut hasher);
    weight.hash(&mut hasher);
    style.hash(&mut hasher);
    hasher.finish()
}

fn clear_face_hit() {
    FACE_HIT.with(|slot| *slot.borrow_mut() = None);
}

// ── Loaded font entry ──────────────────────────────────────────────────────

/// A successfully loaded font — holds the raw bytes ready for the renderer.
#[derive(Clone)]
pub struct LoadedFont {
    pub family: String,
    pub weight: FontWeight,
    pub style: FontFaceStyle,
    /// Canonical sfnt font bytes (TTF / OTF).
    pub data: Arc<Vec<u8>>,
    /// Whether this is a monospace font (detected from family name heuristic).
    pub is_monospace: bool,
    parsed: Option<Arc<fontdue::Font>>,
    unicode_ranges: Option<Arc<Vec<UnicodeRange>>>,
    cache_key: u64,
    collection_index: Option<u32>,
    #[cfg(feature = "skia")]
    skia_typeface: Arc<OnceLock<Option<skia_safe::Typeface>>>,
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

    /// Parsed font data shared by layout and every renderer.
    pub(crate) fn parsed(&self) -> Option<Arc<fontdue::Font>> {
        self.parsed.clone()
    }

    /// Stable identity for retained text and glyph caches.
    pub(crate) fn cache_key(&self) -> u64 {
        self.cache_key
    }

    fn covers_all_codepoints(&self) -> bool {
        self.unicode_ranges.is_none()
    }

    /// Whether this face is eligible for and contains a glyph for `character`.
    pub(crate) fn supports_character(&self, character: char) -> bool {
        self.permits_character(character)
            && self
                .parsed
                .as_ref()
                .is_some_and(|font| font.chars().contains_key(&character))
    }

    // Primary strut lookup checks CSS subset eligibility, not cmap coverage.
    fn permits_character(&self, character: char) -> bool {
        let codepoint = character as u32;
        !self
            .unicode_ranges
            .as_ref()
            .is_some_and(|ranges| !ranges.iter().any(|range| range.contains(codepoint)))
    }

    #[cfg(feature = "skia")]
    pub(crate) fn skia_typeface(&self) -> Option<skia_safe::Typeface> {
        self.skia_typeface
            .get_or_init(|| {
                pinned_font_mgr().new_from_data(
                    self.data.as_slice(),
                    self.collection_index.map(|index| index as usize),
                )
            })
            .clone()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct UnicodeRange {
    start: u32,
    end: u32,
}

impl UnicodeRange {
    fn contains(self, codepoint: u32) -> bool {
        (self.start..=self.end).contains(&codepoint)
    }
}

fn parse_unicode_ranges(source: &str) -> Result<Vec<UnicodeRange>, String> {
    source
        .split(',')
        .map(|part| {
            let token = part.trim();
            let body = token
                .strip_prefix("U+")
                .or_else(|| token.strip_prefix("u+"))
                .ok_or_else(|| format!("invalid unicode-range token {token:?}"))?;
            if body.is_empty() {
                return Err(format!("invalid unicode-range token {token:?}"));
            }
            if body.contains('?') {
                if body.len() > 6
                    || !body
                        .chars()
                        .all(|character| character.is_ascii_hexdigit() || character == '?')
                    || body
                        .chars()
                        .skip_while(|character| character.is_ascii_hexdigit())
                        .any(|character| character != '?')
                {
                    return Err(format!("invalid unicode-range wildcard {token:?}"));
                }
                let start = u32::from_str_radix(&body.replace('?', "0"), 16)
                    .map_err(|_| format!("invalid unicode-range wildcard {token:?}"))?;
                let end = u32::from_str_radix(&body.replace('?', "F"), 16)
                    .map_err(|_| format!("invalid unicode-range wildcard {token:?}"))?;
                if end > 0x10_FFFF {
                    return Err(format!(
                        "unicode-range exceeds Unicode scalar values: {token:?}"
                    ));
                }
                return Ok(UnicodeRange { start, end });
            }
            let (start, end) = body.split_once('-').map_or((body, body), |parts| parts);
            if start.is_empty()
                || end.is_empty()
                || start.len() > 6
                || end.len() > 6
                || !start.chars().all(|character| character.is_ascii_hexdigit())
                || !end.chars().all(|character| character.is_ascii_hexdigit())
            {
                return Err(format!("invalid unicode-range token {token:?}"));
            }
            let start = u32::from_str_radix(start, 16)
                .map_err(|_| format!("invalid unicode-range token {token:?}"))?;
            let end = u32::from_str_radix(end, 16)
                .map_err(|_| format!("invalid unicode-range token {token:?}"))?;
            if start > end || end > 0x10_FFFF {
                return Err(format!("invalid unicode-range bounds {token:?}"));
            }
            Ok(UnicodeRange { start, end })
        })
        .collect()
}

pub(crate) fn unicode_range_matches_text(source: Option<&str>, text: &str) -> bool {
    if text.is_empty() {
        return true;
    }
    let Some(source) = source.map(str::trim).filter(|source| !source.is_empty()) else {
        return true;
    };
    let Ok(ranges) = parse_unicode_ranges(source) else {
        // Let registration report malformed descriptors instead of making the
        // demand loader silently skip the face forever.
        return true;
    };
    text.chars()
        .any(|character| ranges.iter().any(|range| range.contains(character as u32)))
}

#[derive(Clone)]
pub(crate) struct ResolvedFontRun {
    pub byte_range: Range<usize>,
    pub font: Option<LoadedFont>,
}

// ── FontRegistry ───────────────────────────────────────────────────────────

/// Global font registry — maps `(family, weight, style)` to loaded font data.
///
/// Access via `FontRegistry::global()`. Thread-safe.
pub struct FontRegistry {
    revision: AtomicU64,
    fonts: Mutex<HashMap<FontKey, Vec<(u64, LoadedFont)>>>,
    /// Ordered list of registered families (for CSS `font-family` stack resolution).
    families: Mutex<Vec<(u64, String)>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct FontKey {
    family: String,
    weight: FontWeight,
    style: FontFaceStyle,
}

static GLOBAL_REGISTRY: OnceLock<FontRegistry> = OnceLock::new();

pub(crate) struct HostUiFont {
    pub(crate) family: String,
    pub(crate) data: Arc<Vec<u8>>,
    pub(crate) index: u32,
    pub(crate) font: Arc<fontdue::Font>,
}

static HOST_UI_FONT: OnceLock<HostUiFont> = OnceLock::new();

/// Parsed face for a character the caller's font does not cover.
pub(crate) fn parsed_fallback(character: char) -> Option<Arc<fontdue::Font>> {
    let character = crate::text_layout::font_glyph_character(character);
    system_fonts::fallback_for_character(character)?.parsed()
}

/// Fontconfig-backed manager. Tests pin bundled faces before the first init.
///
/// `FontMgr::default()` builds a fresh platform manager on every call; on
/// macOS `SkFontMgr_Mac` copies the full family-name list over XPC in its
/// constructor (`CTFontManagerCopyAvailableFontFamilyNames`), which made
/// per-glyph fallback lookups dominate CJK pages (30-55 s). Skia expects
/// one long-lived manager (Blink keeps a single `FontCache`), so cache it
/// per thread.
#[cfg(feature = "skia")]
pub(crate) fn pinned_font_mgr() -> skia_safe::FontMgr {
    thread_local! {
        static MGR: skia_safe::FontMgr = {
            system_fonts::pin_test_fontconfig();
            skia_safe::FontMgr::default()
        };
    }
    MGR.with(|m| m.clone())
}

pub(crate) fn host_ui_font() -> &'static HostUiFont {
    HOST_UI_FONT.get_or_init(|| {
        // Tests pin fontconfig at a bundled set whose symlink faces and
        // fontdb's directory walk do not always select the same file.
        // Keep the database query there. Release and perf builds take the
        // single-file match so startup does not index every installed face.
        #[cfg(not(test))]
        if let Some(face) = system_fonts::preferred_host_face() {
            return finish_host_font(face.family, face.bytes, face.index);
        }
        let database = system_fonts::database();
        let id = database
            .query(&fontdb::Query {
                families: &system_fonts::host_ui_families(),
                ..fontdb::Query::default()
            })
            .or_else(|| database.faces().next().map(|face| face.id))
            .expect("host must provide at least one system font");
        let family = database
            .face(id)
            .and_then(|face| face.families.first())
            .map(|(family, _)| family.clone())
            .expect("selected system font must have a family");
        let (data, index) = database
            .with_face_data(id, |data, index| (Arc::new(data.to_vec()), index))
            .expect("selected system font must remain readable");
        finish_host_font(family, data, index)
    })
}

fn finish_host_font(family: String, data: Arc<Vec<u8>>, index: u32) -> HostUiFont {
    // Collection index 0 shares `cached_parsed_font` with `@font-face`,
    // so a page that names the host face does not parse those bytes twice.
    let font = if index == 0 {
        cached_parsed_font(data.as_slice()).expect("selected system font must be valid")
    } else {
        Arc::new(
            fontdue::Font::from_bytes(
                data.as_slice(),
                fontdue::FontSettings {
                    collection_index: index,
                    ..fontdue::FontSettings::default()
                },
            )
            .expect("selected system font must be valid"),
        )
    };
    HostUiFont {
        family,
        data,
        index,
        font,
    }
}

impl FontRegistry {
    pub(crate) fn new() -> Self {
        Self {
            revision: AtomicU64::new(0),
            fonts: Mutex::new(HashMap::new()),
            families: Mutex::new(Vec::new()),
        }
    }

    /// Access the global font registry (lazily initialized).
    pub fn global() -> &'static FontRegistry {
        GLOBAL_REGISTRY.get_or_init(FontRegistry::new)
    }

    /// Revision for invalidating styles resolved before font loading completed.
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    /// Resolve the font's own line metrics using the shared parsed face.
    pub fn normal_line_height(&self, style: &w3cos_std::style::Style) -> Option<f32> {
        use w3cos_std::style::FontStyle;
        let font_style = match style.font_style {
            FontStyle::Normal => FontFaceStyle::Normal,
            FontStyle::Italic => FontFaceStyle::Italic,
            FontStyle::Oblique => FontFaceStyle::Oblique,
        };
        let font = self.resolve_render_stack(
            style.font_family.as_deref()?,
            FontWeight(style.font_weight),
            font_style,
            None,
        )?;
        let metrics = font.parsed()?.horizontal_line_metrics(style.font_size)?;
        let ratio = metrics.new_line_size / style.font_size;
        (ratio.is_finite() && ratio > 0.0).then_some(ratio)
    }

    /// Used `normal` leading for a text leaf. A face named first in the CSS
    /// stack cannot supply its metrics for characters it does not cover.
    pub(crate) fn normal_line_height_for_text(
        &self,
        style: &w3cos_std::style::Style,
        text: &str,
    ) -> f32 {
        let fallback = w3cos_std::style::Style::default().line_height;
        self.resolve_style_runs(style, text)
            .into_iter()
            .filter(|run| {
                text[run.byte_range.clone()]
                    .chars()
                    .any(|ch| !ch.is_whitespace())
            })
            .map(|run| {
                run.font
                    .as_ref()
                    .and_then(|font| font.parsed())
                    .and_then(|font| font.horizontal_line_metrics(style.font_size))
                    .map(|metrics| metrics.new_line_size / style.font_size)
                    .filter(|ratio| ratio.is_finite() && *ratio > 0.0)
                    .unwrap_or(fallback)
            })
            .reduce(f32::max)
            .unwrap_or(fallback)
    }

    /// Register a `@font-face` rule. Loads font data immediately.
    pub fn register(&self, face: FontFace) -> Result<(), String> {
        self.register_for_owner(0, face)
    }

    /// Register a font for a lifecycle owner.
    pub fn register_for_owner(&self, owner: u64, face: FontFace) -> Result<(), String> {
        clear_face_hit();
        let data = match &face.src {
            FontSource::Bytes(b) => normalize_font_bytes(b).map(Arc::new)?,
            FontSource::Path(p) => {
                let bytes =
                    read_font_path(p).ok_or_else(|| format!("font load error {}", p.display()))?;
                normalize_font_bytes(&bytes).map(Arc::new)?
            }
            FontSource::Local(name) => {
                // Try to resolve from common system font paths
                let resolved = resolve_system_font(name);
                match resolved {
                    Some(bytes) => normalize_font_bytes(&bytes).map(Arc::new)?,
                    None => {
                        // Register as a placeholder — renderer will use fallback
                        Arc::new(Vec::new())
                    }
                }
            }
        };

        let is_monospace = LoadedFont::detect_monospace(&face.family);
        let unicode_ranges = face
            .unicode_range
            .as_deref()
            .map(parse_unicode_ranges)
            .transpose()?
            .map(Arc::new);
        let parsed = if data.is_empty() {
            None
        } else {
            cached_parsed_font(data.as_slice())
        };
        let mut cache_hasher = DefaultHasher::new();
        data.hash(&mut cache_hasher);
        face.family.hash(&mut cache_hasher);
        face.weight.hash(&mut cache_hasher);
        face.style.hash(&mut cache_hasher);
        unicode_ranges.hash(&mut cache_hasher);
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
            parsed,
            unicode_ranges: unicode_ranges.clone(),
            cache_key: cache_hasher.finish(),
            collection_index: None,
            #[cfg(feature = "skia")]
            skia_typeface: Arc::new(OnceLock::new()),
        };

        let mut fonts = self.fonts.lock().unwrap();
        let entries = fonts.entry(key).or_default();
        entries.retain(|(existing_owner, existing)| {
            *existing_owner != owner || existing.unicode_ranges != unicode_ranges
        });
        entries.push((owner, loaded));
        drop(fonts);

        let mut families = self.families.lock().unwrap();
        if !families
            .iter()
            .any(|(existing_owner, family)| *existing_owner == owner && family == &face.family)
        {
            families.push((owner, face.family));
        }
        self.revision.fetch_add(1, Ordering::Release);
        Ok(())
    }

    /// Resolve one CSS `local()` source and register its bytes under the
    /// authored family. Missing local fonts return an error so callers may try
    /// the next `src` candidate.
    pub fn register_local_for_owner(
        &self,
        owner: u64,
        mut face: FontFace,
        local_name: &str,
    ) -> Result<(), String> {
        let bytes = resolve_system_font(local_name)
            .ok_or_else(|| format!("local font {local_name:?} is unavailable"))?;
        let bytes = normalize_font_bytes(&bytes)?;
        if cached_parsed_font(bytes.as_slice()).is_none() {
            return Err(format!("local font {local_name:?} cannot be decoded"));
        }
        face.src = FontSource::Bytes(bytes);
        self.register_for_owner(owner, face)
    }

    /// Remove every font registered by one page/stylesheet lifecycle owner.
    pub fn clear_owner(&self, owner: u64) {
        clear_face_hit();
        let mut fonts = self.fonts.lock().unwrap();
        fonts.retain(|_, entries| {
            entries.retain(|(existing_owner, _)| *existing_owner != owner);
            !entries.is_empty()
        });
        let mut families = self.families.lock().unwrap();
        families.retain(|(existing_owner, _)| *existing_owner != owner);
        self.revision.fetch_add(1, Ordering::Release);
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
        resolve_family(&fonts, family, weight, style, None)
    }

    /// Resolve one character through CSS family, style, weight, unicode-range
    /// and actual cmap coverage.
    pub(crate) fn resolve_for_character(
        &self,
        family: &str,
        weight: FontWeight,
        style: FontFaceStyle,
        character: char,
    ) -> Option<LoadedFont> {
        let fonts = self.fonts.lock().unwrap();
        resolve_family(&fonts, family, weight, style, Some(character))
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

    pub(crate) fn resolve_stack_for_character(
        &self,
        stack: &str,
        weight: FontWeight,
        style: FontFaceStyle,
        character: char,
    ) -> Option<LoadedFont> {
        for family in stack.split(',') {
            let family = family.trim().trim_matches('"').trim_matches('\'');
            if let Some(font) = self.resolve_for_character(family, weight, style, character) {
                return Some(font);
            }
        }
        None
    }

    /// Resolve the CSS font properties carried by the shared style object.
    pub(crate) fn resolve_style(&self, style: &w3cos_std::style::Style) -> Option<LoadedFont> {
        let family = style.font_family.as_deref()?;
        let face_style = match style.font_style {
            w3cos_std::style::FontStyle::Normal => FontFaceStyle::Normal,
            w3cos_std::style::FontStyle::Italic => FontFaceStyle::Italic,
            w3cos_std::style::FontStyle::Oblique => FontFaceStyle::Oblique,
        };
        self.resolve_render_stack(family, FontWeight(style.font_weight), face_style, None)
    }

    pub(crate) fn resolve_style_for_character(
        &self,
        style: &w3cos_std::style::Style,
        character: char,
    ) -> Option<LoadedFont> {
        let character = crate::text_layout::font_glyph_character(character);
        let family = style.font_family.as_deref()?;
        let face_style = match style.font_style {
            w3cos_std::style::FontStyle::Normal => FontFaceStyle::Normal,
            w3cos_std::style::FontStyle::Italic => FontFaceStyle::Italic,
            w3cos_std::style::FontStyle::Oblique => FontFaceStyle::Oblique,
        };
        let weight = FontWeight(style.font_weight);
        let key = face_hit_key(family, weight, face_style);
        let cached = FACE_HIT.with(|slot| {
            slot.borrow().as_ref().and_then(|(cached_key, font)| {
                (*cached_key == key && font.supports_character(character)).then(|| font.clone())
            })
        });
        if cached.is_some() {
            return cached;
        }
        let resolved = self.resolve_render_stack(family, weight, face_style, Some(character))?;
        if resolved.covers_all_codepoints() {
            FACE_HIT.with(|slot| *slot.borrow_mut() = Some((key, resolved.clone())));
        }
        Some(resolved)
    }

    /// One unrestricted face that has a glyph for every character. Unicode-range
    /// siblings still go through the per-character cascade.
    fn single_covering_face(
        &self,
        stack: &str,
        weight: FontWeight,
        style: FontFaceStyle,
        text: &str,
    ) -> Option<LoadedFont> {
        let key = face_hit_key(stack, weight, style);
        if let Some(cached) = FACE_HIT.with(|slot| {
            slot.borrow()
                .as_ref()
                .and_then(|(cached_key, font)| (*cached_key == key).then(|| font.clone()))
        }) {
            if cached.covers_all_codepoints()
                && text.chars().all(|character| {
                    cached.supports_character(crate::text_layout::font_glyph_character(character))
                })
            {
                return Some(cached);
            }
        }
        let first = text.chars().next()?;
        let glyph = crate::text_layout::font_glyph_character(first);
        let font = self.resolve_render_stack(stack, weight, style, Some(glyph))?;
        if !font.covers_all_codepoints() {
            return None;
        }
        if text.chars().any(|character| {
            !font.supports_character(crate::text_layout::font_glyph_character(character))
        }) {
            return None;
        }
        FACE_HIT.with(|slot| *slot.borrow_mut() = Some((key, font.clone())));
        Some(font)
    }

    /// Blink's primary line strut queries U+0020 without requiring its glyph.
    pub(crate) fn resolve_style_for_strut(
        &self,
        style: &w3cos_std::style::Style,
    ) -> Option<LoadedFont> {
        let family = style.font_family.as_deref()?;
        let face_style = match style.font_style {
            w3cos_std::style::FontStyle::Normal => FontFaceStyle::Normal,
            w3cos_std::style::FontStyle::Italic => FontFaceStyle::Italic,
            w3cos_std::style::FontStyle::Oblique => FontFaceStyle::Oblique,
        };
        self.resolve_render_stack_with_eligibility(
            family,
            FontWeight(style.font_weight),
            face_style,
            Some(' '),
            false,
        )
    }

    /// Resolve render faces without promoting installed fonts into document
    /// registrations. A generic family is a cascade boundary handled by the
    /// renderer's platform defaults, not an alias for a later named family.
    fn resolve_render_stack(
        &self,
        stack: &str,
        weight: FontWeight,
        style: FontFaceStyle,
        character: Option<char>,
    ) -> Option<LoadedFont> {
        self.resolve_render_stack_with_eligibility(stack, weight, style, character, true)
    }

    /// Per-character fallback memo, like Blink's FontFallbackList caching the
    /// font chosen for a character: shaping a streamed paragraph resolved
    /// the whole family stack (mutex, lowercase, system lookup) once per
    /// character on every measure. Keyed by registry revision so a newly
    /// loaded @font-face is seen.
    fn resolve_render_stack_with_eligibility(
        &self,
        stack: &str,
        weight: FontWeight,
        style: FontFaceStyle,
        character: Option<char>,
        require_glyph: bool,
    ) -> Option<LoadedFont> {
        type Key = (usize, u64, FontWeight, FontFaceStyle, Option<char>, bool);
        thread_local! {
            static MEMO: RefCell<(u64, w3cos_std::fxhash::FxHashMap<Key, Option<LoadedFont>>)> =
                RefCell::new((u64::MAX, Default::default()));
        }
        let revision = self.revision();
        let mut hasher = w3cos_std::fxhash::FxHasher::default();
        stack.hash(&mut hasher);
        let key: Key = (
            self as *const Self as usize,
            hasher.finish(),
            weight,
            style,
            character,
            require_glyph,
        );
        if let Some(hit) = MEMO.with(|memo| {
            let memo = memo.borrow();
            (memo.0 == revision)
                .then(|| memo.1.get(&key).cloned())
                .flatten()
        }) {
            return hit;
        }
        let resolved =
            self.resolve_render_stack_uncached(stack, weight, style, character, require_glyph);
        MEMO.with(|memo| {
            let mut memo = memo.borrow_mut();
            if memo.0 != revision || memo.1.len() > 65_536 {
                memo.0 = revision;
                memo.1.clear();
            }
            memo.1.insert(key, resolved.clone());
        });
        resolved
    }

    fn resolve_render_stack_uncached(
        &self,
        stack: &str,
        weight: FontWeight,
        style: FontFaceStyle,
        character: Option<char>,
        require_glyph: bool,
    ) -> Option<LoadedFont> {
        for raw in stack.split(',') {
            let raw = raw.trim();
            if matches!(
                raw.to_ascii_lowercase().as_str(),
                "serif"
                    | "sans-serif"
                    | "monospace"
                    | "cursive"
                    | "fantasy"
                    | "system-ui"
                    | "ui-serif"
                    | "ui-sans-serif"
                    | "ui-monospace"
                    | "ui-rounded"
                    | "-apple-system"
                    | "blinkmacsystemfont"
            ) {
                return None;
            }
            let family = raw.trim_matches(['"', '\'']);
            let (registered, shadows_system) = {
                let fonts = self.fonts.lock().unwrap();
                (
                    resolve_family_with_eligibility(
                        &fonts,
                        family,
                        weight,
                        style,
                        character,
                        require_glyph,
                    ),
                    fonts
                        .keys()
                        .any(|key| key.family.eq_ignore_ascii_case(family)),
                )
            };
            if let Some(font) = registered {
                return Some(font);
            }
            // @font-face defines this family even outside its unicode-range;
            // missing coverage falls through to the next CSS family, not a
            // system face with the same name.
            if !shadows_system
                && let Some(font) = system_fonts::resolve(family, weight, style)
                && character.is_none_or(|character| {
                    if require_glyph {
                        font.supports_character(character)
                    } else {
                        font.permits_character(character)
                    }
                })
            {
                return Some(font);
            }
        }
        None
    }

    pub(crate) fn resolve_style_runs(
        &self,
        style: &w3cos_std::style::Style,
        text: &str,
    ) -> Vec<ResolvedFontRun> {
        #[cfg(feature = "dynamic-js")]
        if std::ptr::eq(self, Self::global()) {
            crate::dynamic_script::request_stylesheet_fonts_for_text(style, text);
        }
        let Some(stack) = style.font_family.as_deref() else {
            return vec![ResolvedFontRun {
                byte_range: 0..text.len(),
                font: None,
            }];
        };
        let face_style = match style.font_style {
            w3cos_std::style::FontStyle::Normal => FontFaceStyle::Normal,
            w3cos_std::style::FontStyle::Italic => FontFaceStyle::Italic,
            w3cos_std::style::FontStyle::Oblique => FontFaceStyle::Oblique,
        };
        let weight = FontWeight(style.font_weight);
        if let Some(font) = self.single_covering_face(stack, weight, face_style, text) {
            return vec![ResolvedFontRun {
                byte_range: 0..text.len(),
                font: Some(font),
            }];
        }
        let mut runs: Vec<ResolvedFontRun> = Vec::new();
        for (offset, character) in text.char_indices() {
            let glyph_character = crate::text_layout::font_glyph_character(character);
            let font = self.resolve_render_stack(stack, weight, face_style, Some(glyph_character));
            let same_font = runs.last().is_some_and(|run| {
                run.font.as_ref().map(LoadedFont::cache_key)
                    == font.as_ref().map(LoadedFont::cache_key)
            });
            if same_font {
                if let Some(run) = runs.last_mut() {
                    run.byte_range.end = offset + character.len_utf8();
                }
            } else {
                runs.push(ResolvedFontRun {
                    byte_range: offset..offset + character.len_utf8(),
                    font,
                });
            }
        }
        if runs.is_empty() {
            runs.push(ResolvedFontRun {
                byte_range: 0..0,
                font: self.resolve_style(style),
            });
        }
        runs
    }

    /// Memoized per (font stack, weight, style, kerning, text), like Blink's
    /// ShapeCache keyed by FontDescription + text: re-measuring an unchanged
    /// text node walked the fallback list once per character.
    pub(crate) fn cascade_cache_key(&self, style: &w3cos_std::style::Style, text: &str) -> u64 {
        thread_local! {
            static MEMO: RefCell<(
                u64,
                usize,
                w3cos_std::fxhash::FxHashMap<u64, w3cos_std::fxhash::FxHashMap<String, u64>>,
            )> = RefCell::new((u64::MAX, 0, Default::default()));
        }
        #[cfg(feature = "dynamic-js")]
        if std::ptr::eq(self, Self::global()) {
            crate::dynamic_script::request_stylesheet_fonts_for_text(style, text);
        }
        let revision = self.revision();
        let mut meta = w3cos_std::fxhash::FxHasher::default();
        (self as *const Self as usize).hash(&mut meta);
        style.font_family.hash(&mut meta);
        style.font_weight.hash(&mut meta);
        (style.font_style as u8).hash(&mut meta);
        style.font_kerning.hash(&mut meta);
        style.font_feature_kern.hash(&mut meta);
        let meta = meta.finish();
        if let Some(hit) = MEMO.with(|memo| {
            let memo = memo.borrow();
            (memo.0 == revision)
                .then(|| memo.2.get(&meta).and_then(|texts| texts.get(text)).copied())
                .flatten()
        }) {
            return hit;
        }
        let key = self.cascade_cache_key_uncached(style, text);
        // A font load during resolution bumps the revision; storing under the
        // old one makes the next lookup recompute.
        MEMO.with(|memo| {
            let mut memo = memo.borrow_mut();
            if memo.0 != revision || memo.1 > 65_536 {
                memo.0 = revision;
                memo.1 = 0;
                memo.2.clear();
            }
            memo.1 += 1;
            memo.2.entry(meta).or_default().insert(text.to_owned(), key);
        });
        key
    }

    fn cascade_cache_key_uncached(&self, style: &w3cos_std::style::Style, text: &str) -> u64 {
        let mut hasher = w3cos_std::fxhash::FxHasher::default();
        style.font_kerning.hash(&mut hasher);
        style.font_feature_kern.hash(&mut hasher);
        for run in self.resolve_style_runs(style, text) {
            run.byte_range.hash(&mut hasher);
            run.font
                .as_ref()
                .map(LoadedFont::cache_key)
                .unwrap_or_default()
                .hash(&mut hasher);
        }
        hasher.finish()
    }

    pub(crate) fn style_char_advance(
        &self,
        style: &w3cos_std::style::Style,
        character: char,
        font_size: f32,
        fallback: &fontdue::Font,
    ) -> f32 {
        let font = self
            .resolve_style_for_character(style, character)
            .and_then(|font| font.parsed());
        crate::text_layout::char_advance(character, font_size, font.as_deref().unwrap_or(fallback))
    }

    pub(crate) fn measure_style_ink_bounds(
        &self,
        style: &w3cos_std::style::Style,
        text: &str,
        font_size: f32,
        fallback: &fontdue::Font,
    ) -> crate::text_layout::InkBounds {
        let mut cursor_x = 0.0_f32;
        let cursor_y = font_size;
        let mut left = f32::MAX;
        let mut top = f32::MAX;
        let mut right = f32::MIN;
        let mut bottom = f32::MIN;
        let mut saw_ink = false;

        let render_text = crate::text_layout::font_render_text_for_style(text, style);
        for character in render_text.chars() {
            let character = crate::text_layout::font_glyph_character(character);
            let selected = self
                .resolve_style_for_character(style, character)
                .and_then(|font| font.parsed());
            let system = if selected
                .as_ref()
                .is_some_and(|font| font.chars().contains_key(&character))
                || fallback.chars().contains_key(&character)
            {
                None
            } else {
                parsed_fallback(character)
            };
            let font = selected
                .as_deref()
                .or(system.as_deref())
                .unwrap_or(fallback);
            if !font.chars().contains_key(&character) {
                let advance = crate::text_layout::estimated_char_width(character, font_size);
                saw_ink = true;
                left = left.min(cursor_x);
                top = top.min(0.0);
                right = right.max(cursor_x + advance);
                bottom = bottom.max(font_size);
                cursor_x += advance;
                continue;
            }
            let metrics = font.metrics(character, font_size);
            let advance = if metrics.advance_width > 0.0 {
                metrics.advance_width
            } else {
                crate::text_layout::estimated_char_width(character, font_size)
            };
            if metrics.width > 0 && metrics.height > 0 {
                saw_ink = true;
                let (x, y) = crate::text_layout::glyph_pixel_origin(cursor_x, cursor_y, &metrics);
                let x = x as f32;
                let y = y as f32;
                left = left.min(x);
                top = top.min(y);
                right = right.max(x + metrics.width as f32);
                bottom = bottom.max(y + metrics.height as f32);
            }
            cursor_x += advance;
        }
        if !saw_ink {
            return crate::text_layout::InkBounds::empty();
        }
        crate::text_layout::InkBounds {
            left,
            top,
            width: (right - left).max(0.0),
            height: (bottom - top).max(0.0),
        }
    }

    pub(crate) fn style_single_line_content_height(
        &self,
        style: &w3cos_std::style::Style,
        text: &str,
        fallback: &fontdue::Font,
    ) -> f32 {
        self.resolve_style_runs(style, text)
            .into_iter()
            .map(|run| {
                let font = run.font.as_ref().and_then(LoadedFont::parsed);
                crate::text_layout::single_line_content_height(
                    &text[run.byte_range],
                    style.font_size,
                    style.line_height,
                    font.as_deref().unwrap_or(fallback),
                )
            })
            .fold(0.0_f32, f32::max)
    }

    /// List all registered family names.
    pub fn families(&self) -> Vec<String> {
        let mut unique = Vec::new();
        for (_, family) in self.families.lock().unwrap().iter() {
            if !unique.contains(family) {
                unique.push(family.clone());
            }
        }
        unique
    }

    /// Returns true if any monospace font is registered.
    pub fn has_monospace(&self) -> bool {
        self.fonts
            .lock()
            .unwrap()
            .values()
            .filter_map(|entries| entries.last().map(|(_, font)| font))
            .any(|font| font.is_monospace)
    }

    /// Get the first registered monospace font (for code editors).
    pub fn default_monospace(&self) -> Option<LoadedFont> {
        self.fonts
            .lock()
            .unwrap()
            .values()
            .filter_map(|entries| entries.last().map(|(_, font)| font))
            .find(|font| font.is_monospace)
            .cloned()
    }
}

fn resolve_family(
    fonts: &HashMap<FontKey, Vec<(u64, LoadedFont)>>,
    family: &str,
    weight: FontWeight,
    style: FontFaceStyle,
    character: Option<char>,
) -> Option<LoadedFont> {
    resolve_family_with_eligibility(fonts, family, weight, style, character, true)
}

fn resolve_family_with_eligibility(
    fonts: &HashMap<FontKey, Vec<(u64, LoadedFont)>>,
    family: &str,
    weight: FontWeight,
    style: FontFaceStyle,
    character: Option<char>,
    require_glyph: bool,
) -> Option<LoadedFont> {
    let eligible = |font: &LoadedFont| {
        character.is_none_or(|character| {
            if require_glyph {
                font.supports_character(character)
            } else {
                font.permits_character(character)
            }
        })
    };
    let exact = FontKey {
        family: family.to_string(),
        weight,
        style,
    };
    if let Some(font) = fonts
        .get(&exact)
        .and_then(|entries| entries.iter().rev().find(|(_, font)| eligible(font)))
        .map(|(_, font)| font.clone())
    {
        return Some(font);
    }

    let closest = |match_style: bool| {
        fonts
            .iter()
            .filter(|(key, _)| {
                key.family.eq_ignore_ascii_case(family) && (!match_style || key.style == style)
            })
            .filter_map(|(key, entries)| {
                entries
                    .iter()
                    .rev()
                    .find(|(_, font)| eligible(font))
                    .map(|(_, font)| {
                        (
                            (key.weight.0 as i32 - weight.0 as i32).unsigned_abs(),
                            key.weight.0,
                            font,
                        )
                    })
            })
            .min_by_key(|(distance, candidate_weight, _)| (*distance, *candidate_weight))
            .map(|(_, _, font)| font.clone())
    };
    closest(true).or_else(|| closest(false))
}

// ── System font resolution ─────────────────────────────────────────────────

fn cached_parsed_font(data: &[u8]) -> Option<Arc<fontdue::Font>> {
    use std::collections::HashMap;
    use std::hash::{Hash, Hasher};
    use std::sync::Mutex;
    static CACHE: Mutex<Option<HashMap<u64, Arc<fontdue::Font>>>> = Mutex::new(None);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    data.len().hash(&mut hasher);
    data.hash(&mut hasher);
    let key = hasher.finish();
    let mut guard = CACHE.lock().unwrap_or_else(|poison| poison.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    if let Some(font) = map.get(&key) {
        return Some(Arc::clone(font));
    }
    let font = Arc::new(fontdue::Font::from_bytes(data, fontdue::FontSettings::default()).ok()?);
    map.insert(key, Arc::clone(&font));
    Some(font)
}

fn read_font_path(path: &std::path::Path) -> Option<Vec<u8>> {
    if let Ok(bytes) = std::fs::read(path)
        && !bytes.is_empty()
    {
        return Some(bytes);
    }
    let name = path.file_name()?.to_str()?;
    if name.eq_ignore_ascii_case("NotoSans-Regular.ttf") {
        return std::fs::read("/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf").ok();
    }
    None
}

/// Try to load a system font by name from the installed font database, then
/// common OS font directories.
fn resolve_system_font(name: &str) -> Option<Vec<u8>> {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static CACHE: Mutex<Option<HashMap<String, Arc<Vec<u8>>>>> = Mutex::new(None);
    let key = name.to_ascii_lowercase();
    {
        let guard = CACHE.lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some(hit) = guard.as_ref().and_then(|map| map.get(&key)) {
            return Some(hit.as_ref().clone());
        }
    }
    let bytes = resolve_system_font_uncached(name)?;
    let mut guard = CACHE.lock().unwrap_or_else(|poison| poison.into_inner());
    guard
        .get_or_insert_with(HashMap::new)
        .insert(key, Arc::new(bytes.clone()));
    Some(bytes)
}

fn resolve_system_font_uncached(name: &str) -> Option<Vec<u8>> {
    {
        let database = system_fonts::database();
        if let Some(id) = database.query(&fontdb::Query {
            families: &[fontdb::Family::Name(name)],
            ..fontdb::Query::default()
        }) && let Some(bytes) = database.with_face_data(id, |data, _index| data.to_vec())
            && !bytes.is_empty()
        {
            return Some(bytes);
        }
    }
    if (name.eq_ignore_ascii_case("Noto Sans") || name.eq_ignore_ascii_case("Noto Sans Regular"))
        && let Some(bytes) = read_font_path(std::path::Path::new(
            "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
        ))
    {
        return Some(bytes);
    }
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
fn parse_one_src(val: &str) -> Option<FontSource> {
    let val = val.trim().trim_end_matches(';');
    if let Some(inner) = val
        .strip_prefix("url(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let url = inner.trim().trim_matches('"').trim_matches('\'');
        return Some(FontSource::Path(url.into()));
    }
    if let Some(inner) = val
        .strip_prefix("local(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let name = inner.trim().trim_matches('"').trim_matches('\'');
        return Some(FontSource::Local(name.to_string()));
    }
    None
}

fn parse_src_list(val: &str) -> Vec<FontSource> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (index, ch) in val.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                if let Some(src) = parse_one_src(&val[start..index]) {
                    out.push(src);
                }
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }
    if let Some(src) = parse_one_src(&val[start..]) {
        out.push(src);
    }
    out
}

fn source_bytes(src: &FontSource) -> Option<Vec<u8>> {
    match src {
        FontSource::Bytes(bytes) => Some(bytes.clone()),
        FontSource::Path(path) => read_font_path(path),
        FontSource::Local(name) => resolve_system_font(name),
    }
}

pub fn parse_and_register(css_block: &str) -> Result<(), String> {
    let mut family = String::new();
    let mut sources: Vec<FontSource> = Vec::new();
    let mut weight = FontWeight::NORMAL;
    let mut style = FontFaceStyle::Normal;
    let mut display = FontDisplay::Auto;
    let mut unicode_range: Option<String> = None;

    let normalized = css_block.replace(';', "\n");
    for line in normalized.lines() {
        let line = line.trim().trim_end_matches(';');
        if let Some(val) = strip_property(line, "font-family") {
            family = val.trim_matches('"').trim_matches('\'').to_string();
        } else if let Some(val) = strip_property(line, "src") {
            sources.extend(parse_src_list(val));
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

    let loaded = sources
        .iter()
        .find_map(|src| source_bytes(src).map(FontSource::Bytes));
    let src = loaded
        .or_else(|| sources.into_iter().next())
        .unwrap_or(FontSource::Local("sans-serif".into()));
    let face = FontFace {
        family,
        src,
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

    #[cfg(target_os = "macos")]
    #[test]
    fn render_runs_use_the_same_installed_face_as_metrics() {
        let registry = FontRegistry::new();
        let style = w3cos_std::Style {
            font_family: Some("Verdana, sans-serif".into()),
            font_weight: 900,
            font_size: 28.0,
            ..Default::default()
        };
        let measured = registry.resolve_style(&style).unwrap();
        let runs = registry
            .resolve_style_runs(&style, "This box, which should have a bright green border.");
        assert_eq!(runs.len(), 1);
        assert_eq!(
            runs[0].font.as_ref().map(LoadedFont::cache_key),
            Some(measured.cache_key()),
            "paint runs must not use the registered-only cascade while metrics use installed fonts"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn render_stack_resolves_named_system_fonts_without_registering_them() {
        let registry = FontRegistry::new();
        let mut style = w3cos_std::Style {
            font_family: Some("Verdana, sans-serif".into()),
            font_weight: 900,
            font_size: 28.0,
            ..Default::default()
        };
        let before = registry.revision();
        let installed = registry.resolve_style(&style).expect("installed Verdana");
        assert_eq!(installed.family, "Verdana");
        assert!(
            installed.weight.0 >= 600,
            "resolve actual bold face, not normal bytes labelled900"
        );
        assert!(installed.supports_character('W'));
        assert!(installed.parsed().is_some());
        assert!(
            registry.families().is_empty(),
            "installed lookup is not registration"
        );
        assert_eq!(registry.revision(), before);
        style.font_weight = 700;
        let bold = registry.resolve_style(&style).unwrap();
        assert!(
            Arc::ptr_eq(&installed.data, &bold.data),
            "weights selecting one face share font data"
        );
        #[cfg(feature = "skia")]
        {
            let face = installed.skia_typeface().unwrap();
            assert_eq!(face.family_name(), "Verdana");
            assert!(face.font_style().weight() >= skia_safe::font_style::Weight::SEMI_BOLD);
        }
        for stack in ["Arial, Verdana", "'Arial', 'Verdana'"] {
            style.font_family = Some(stack.into());
            assert_eq!(
                registry.resolve_style(&style).unwrap().family,
                "Arial",
                "CSS order: {stack}"
            );
        }
        for stack in ["serif, Verdana", "monospace, Verdana"] {
            style.font_family = Some(stack.into());
            assert!(
                registry.resolve_style(&style).is_none(),
                "generic boundary: {stack}"
            );
        }
        registry
            .register_for_owner(
                247,
                FontFace {
                    family: "Verdana".into(),
                    src: FontSource::Bytes(include_bytes!("../assets/Inter-Regular.ttf").to_vec()),
                    unicode_range: Some("U+0057".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        style.font_family = Some("verdana, Arial".into());
        let owned = registry.resolve_style_for_character(&style, 'W').unwrap();
        assert!(
            !Arc::ptr_eq(&owned.data, &installed.data),
            "registered face overrides installed face case-insensitively"
        );
        assert_eq!(
            registry
                .resolve_style_for_character(&style, 'X')
                .unwrap()
                .family,
            "Arial",
            "out-of-range glyph advances to the next family, not shadowed system Verdana"
        );
        registry.clear_owner(247);
        assert!(Arc::ptr_eq(
            &registry.resolve_style(&style).unwrap().data,
            &installed.data
        ));
        assert!(registry.families().is_empty());
    }

    fn scaled_inter_font(divisor: u16) -> Vec<u8> {
        fn table_offset(bytes: &[u8], tag: &[u8; 4]) -> Option<usize> {
            let table_count = u16::from_be_bytes(bytes.get(4..6)?.try_into().ok()?) as usize;
            (0..table_count).find_map(|index| {
                let entry = 12 + index * 16;
                (bytes.get(entry..entry + 4)? == tag).then(|| {
                    u32::from_be_bytes(bytes[entry + 8..entry + 12].try_into().unwrap()) as usize
                })
            })
        }

        let mut bytes = include_bytes!("../assets/Inter-Regular.ttf").to_vec();
        let hhea = table_offset(&bytes, b"hhea").expect("hhea table");
        let hmtx = table_offset(&bytes, b"hmtx").expect("hmtx table");
        let metrics = u16::from_be_bytes(bytes[hhea + 34..hhea + 36].try_into().unwrap()) as usize;
        for index in 0..metrics {
            let offset = hmtx + index * 4;
            let advance = u16::from_be_bytes(bytes[offset..offset + 2].try_into().unwrap());
            bytes[offset..offset + 2].copy_from_slice(&(advance / divisor).max(1).to_be_bytes());
        }
        bytes
    }

    #[test]
    fn font_weight_from_str() {
        assert_eq!(FontWeight::from_str("bold"), FontWeight::BOLD);
        assert_eq!(FontWeight::from_str("700"), FontWeight::BOLD);
        assert_eq!(FontWeight::from_str("normal"), FontWeight::NORMAL);
        assert_eq!(FontWeight::from_str("400"), FontWeight::NORMAL);
    }

    #[test]
    fn parsed_font_cache_reuses_one_face_for_the_same_bytes() {
        let host = host_ui_font();
        let first = cached_parsed_font(host.data.as_slice()).expect("host font parses");
        let second = cached_parsed_font(host.data.as_slice()).expect("host font parses again");
        assert!(std::sync::Arc::ptr_eq(&first, &second));
        if host.index == 0 {
            assert!(std::sync::Arc::ptr_eq(&host.font, &first));
        }
    }

    #[test]
    fn host_ui_font_is_available_without_packaged_font_bytes() {
        let host = host_ui_font();
        assert_ne!(host.font.lookup_glyph_index('A'), 0);
        #[cfg(target_os = "macos")]
        assert_ne!(host.font.lookup_glyph_index('输'), 0);
    }

    #[test]
    fn normal_line_height_uses_loaded_metrics_and_tracks_font_revision() {
        let registry = FontRegistry::new();
        let style = w3cos_std::style::Style {
            font_family: Some("MetricFixture".into()),
            font_size: 20.0,
            ..Default::default()
        };
        assert!(registry.normal_line_height(&style).is_none());
        let before = registry.revision();
        registry
            .register_for_owner(
                42,
                FontFace {
                    family: "MetricFixture".into(),
                    src: FontSource::Bytes(include_bytes!("../assets/Inter-Regular.ttf").to_vec()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(registry.revision() > before);
        let font = registry
            .resolve("MetricFixture", FontWeight::NORMAL, FontFaceStyle::Normal)
            .unwrap();
        let expected = font
            .parsed()
            .unwrap()
            .horizontal_line_metrics(20.0)
            .unwrap()
            .new_line_size
            / 20.0;
        assert_eq!(registry.normal_line_height(&style), Some(expected));
        let loaded = registry.revision();
        registry.clear_owner(42);
        assert!(registry.revision() > loaded);
        assert!(registry.normal_line_height(&style).is_none());
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

    #[cfg(feature = "dynamic-js")]
    #[test]
    fn normalizes_woff_and_woff2_to_shared_sfnt_bytes() {
        use base64::Engine as _;

        let woff = base64::engine::general_purpose::STANDARD
            .decode(concat!(
                "d09GRgABAAAAAAMIAAoAAAAAAuwAAgAAAAAAAAAAAAAAAAAAAAAAAAAAAABPUy8y",
                "AAABbAAAAFQAAABgd8ttZWNtYXAAAAHMAAAAIwAAADQADAFRZ2x5ZgAAAfgAAADc",
                "AAAA3AP/KkZoZWFkAAAA9AAAADYAAAA28KFcQmhoZWEAAAEsAAAAIAAAACQPswhJ",
                "aG10eAAAAcAAAAAMAAAADBDoAYxsb2NhAAAB8AAAAAgAAAAIAEQAbm1heHAAAAFM",
                "AAAAHQAAACAAMQGSbmFtZQAAAtQAAAAgAAAAIAFQBbJwb3N0AAAC9AAAABMAAAAg",
                "/64AmwABAAAAAgAAiIyMQV8PPPUBIQgAAAAAAKLYK+AAAAAA5o3lVPmJ+uUNfAsr",
                "AAAACQACAAAAAAAAeJxjYGRg4GD4x8CwmnfXz84/3bw1DEARFMAMAJOSBhB4nGNg",
                "ZGBgYGYUY1BiqGJgZwDxEICFgREAEIMAxQAAAHicY2BmecU4gYGVgYHlIstZBgbG",
                "WRCaqYVhNuNDDjYmblZmZhYWFiYWBqAkAxJwDAjwYXBgMGYIZ/P758ewmr2F8SFM",
                "DUssmwCjGIMCAwMA+rkOCQQAAJMFVQCrB5MATnicY2BgYGRgYGAGYh4gBAENBghg",
                "AmJjKAapCYdiJgAWaQFeAAAAAAAARABuAAEAq//RBHwGPgAwAAABNTMyPgE1NCYj",
                "Ig4BHQEjNTQ2MzIWFRQOAQceARUUACEiJj0BMxUUFjMyPgE1NCYjAYx0iJpalHpG",
                "ekGyzu/j5EKBeb28/vn+9enWup17YJJM5ewDCJtWgVBldCRWLwoKesrCmVSRciM2",
                "1Z+7/s3HfQoKVFRHoF6RxQABAE4AAAdGBhAAFQAAISMBMxMXMzcBMwEXMzcTMwEj",
                "AScjBwJ7zf6gwfciCSIBHMgBFSAJIf+x/pfN/uElCiMGEPu7u7sERfu4uLgESPnw",
                "BFm+vgAAAAEAEgADAAEECQACAA4AAABSAGUAZwB1AGwAYQByeJxjYGYAg/+rGWYz",
                "YAEANHkCSQA="
            ))
            .expect("embedded WOFF fixture");
        let woff2 = base64::engine::general_purpose::STANDARD
            .decode(concat!(
                "d09GMgABAAAAAAFwAAoAAAAAAlQAAAEoAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                "BmAAKApUbgE2AiQDCAsGAAQgBSAHIBu3AWAuCmNj5eRPhLMIVYH5vdURznTLEXy/",
                "3+ue+24QFaD7wiYpO0JUndiya1ztV0VywK51BDq1qQecZTDyNIAlcfNE0ufvxMXy",
                "fLABBxooDsArb7D6ADx4yJ4eTSNQ5L9xQQX5Q35DXMnBNGY8vlRXprqKosg5p4wA",
                "gGnz58821TLLKudOzlWuGosvCMhrKptj+kCgQL160AUILMMyBEggI9CJEiADorNJJ",
                "+tOLKlbdkodHyqX1i4+9WaVeebDs/rvs/9+9+ddrPcO++54wrXO8k6sGHqkgUcAg",
                "cStTweerPPwv5r+asDVj6XPwM+znZlqOF1XQo0EgmqTlBvu36wROloKLsd3cwVSd",
                "BpwUpUAAFkgtCpErpE0YqEttjlitw0OKoD/ZeMAAAA="
            ))
            .expect("embedded WOFF2 fixture");

        for compressed in [&woff, &woff2] {
            let sfnt = normalize_font_bytes(compressed).expect("decode browser font");
            assert_ne!(&sfnt[..4], b"wOFF");
            assert_ne!(&sfnt[..4], b"wOF2");
            fontdue::Font::from_bytes(sfnt, fontdue::FontSettings::default())
                .expect("decoded font is consumable by the shared renderer parser");
        }

        let mut oversized = woff2.clone();
        oversized[16..20].copy_from_slice(&(4097_u32).to_be_bytes());
        assert!(
            normalize_font_bytes_with_limit(&oversized, 4096)
                .unwrap_err()
                .contains("exceeds source limit")
        );
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

    /// The corpus's per-character fallback cases are all built on Ahem, which
    /// covers 278 codepoints and none of Latin Extended-A. `font-family-013`
    /// asserts that `"Ahem", "Times New Roman"` renders `U+0162` exactly like
    /// `"Times New Roman"` alone, which only holds if cmap coverage - not the
    /// family order - decides which face paints each character.
    #[test]
    fn ahem_does_not_claim_latin_extended_a() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../wpt/fonts/Ahem.ttf");
        let Ok(bytes) = std::fs::read(path) else {
            eprintln!("skipped: no pinned WPT checkout at {path}");
            return;
        };
        let registry = FontRegistry::new();
        registry
            .register(FontFace {
                family: "Ahem".into(),
                src: FontSource::Bytes(bytes),
                ..Default::default()
            })
            .unwrap();

        let ahem = registry
            .resolve("Ahem", FontWeight::NORMAL, FontFaceStyle::Normal)
            .expect("Ahem resolves once registered");
        assert!(ahem.supports_character('T'), "Ahem covers U+0054");
        for character in ['\u{0162}', '\u{0119}', '\u{015f}', '\u{0163}'] {
            assert!(
                !ahem.supports_character(character),
                "Ahem must not claim U+{:04X}",
                character as u32
            );
        }
        let style = w3cos_std::style::Style {
            font_family: Some("\"Ahem\", \"Times New Roman\"".into()),
            font_size: 64.0,
            ..Default::default()
        };
        assert_eq!(registry.normal_line_height_for_text(&style, "T"), 1.0);
        let fallback_style = w3cos_std::style::Style {
            font_family: Some("\"Times New Roman\"".into()),
            ..style.clone()
        };
        assert_eq!(
            registry.normal_line_height_for_text(&style, "\u{0162}\u{0119}\u{015f}\u{0163}"),
            registry
                .normal_line_height_for_text(&fallback_style, "\u{0162}\u{0119}\u{015f}\u{0163}"),
            "uncovered Ahem characters use the next family's metrics, not a hard-coded fallback",
        );
    }

    /// The same rule through the public entry point: an uncovered character
    /// must move to the next family rather than being painted by the first.
    #[test]
    fn an_uncovered_character_falls_through_to_the_next_family() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../wpt/fonts/Ahem.ttf");
        let Ok(ahem_bytes) = std::fs::read(path) else {
            eprintln!("skipped: no pinned WPT checkout at {path}");
            return;
        };
        let registry = FontRegistry::new();
        registry
            .register(FontFace {
                family: "Ahem".into(),
                src: FontSource::Bytes(ahem_bytes),
                ..Default::default()
            })
            .unwrap();
        registry
            .register(FontFace {
                family: "Second".into(),
                src: FontSource::Bytes(scaled_inter_font(1)),
                ..Default::default()
            })
            .unwrap();

        let style = w3cos_std::style::Style {
            font_family: Some("\"Ahem\", Second".into()),
            ..Default::default()
        };
        let runs = registry.resolve_style_runs(&style, "\u{0162}T");
        let families = runs
            .iter()
            .map(|run| {
                run.font
                    .as_ref()
                    .map_or("<none>".to_string(), |font| font.family.clone())
            })
            .collect::<Vec<_>>();
        assert_eq!(
            families.first().map(String::as_str),
            Some("Second"),
            "U+0162 is not in Ahem, so the second family must paint it: {families:?}"
        );
        assert_eq!(
            families.last().map(String::as_str),
            Some("Ahem"),
            "U+0054 is in Ahem, so the first family keeps it: {families:?}"
        );
    }

    #[test]
    fn unicode_ranges_keep_sibling_faces_and_select_per_character() {
        let registry = FontRegistry::new();
        registry
            .register_for_owner(
                7,
                FontFace {
                    family: "Subset Family".into(),
                    src: FontSource::Bytes(scaled_inter_font(4)),
                    unicode_range: Some("U+0057".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        registry
            .register_for_owner(
                7,
                FontFace {
                    family: "Subset Family".into(),
                    src: FontSource::Bytes(scaled_inter_font(1)),
                    unicode_range: Some("U+0030-0039".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let style = w3cos_std::style::Style {
            font_family: Some("Subset Family".into()),
            ..Default::default()
        };
        let runs = registry.resolve_style_runs(&style, "W3W");
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0].byte_range, 0..1);
        assert_eq!(runs[1].byte_range, 1..2);
        assert_eq!(runs[2].byte_range, 2..3);
        assert_eq!(
            runs[0].font.as_ref().unwrap().cache_key(),
            runs[2].font.as_ref().unwrap().cache_key()
        );
        assert_ne!(
            runs[0].font.as_ref().unwrap().cache_key(),
            runs[1].font.as_ref().unwrap().cache_key()
        );
        assert!(
            registry
                .resolve_style_runs(&style, "A")
                .first()
                .unwrap()
                .font
                .is_none()
        );
    }

    #[test]
    fn parses_unicode_range_wildcards_and_rejects_invalid_bounds() {
        let ranges = parse_unicode_ranges("U+4??, U+1F600-1F64F").unwrap();
        assert!(ranges.iter().any(|range| range.contains(0x4ab)));
        assert!(ranges.iter().any(|range| range.contains(0x1f602)));
        assert!(parse_unicode_ranges("U+110000").is_err());
        assert!(parse_unicode_ranges("U+00?F").is_err());
    }

    #[test]
    fn unicode_range_text_matching_selects_only_required_subsets() {
        assert!(unicode_range_matches_text(Some("U+0030-0039"), "W3W"));
        assert!(!unicode_range_matches_text(Some("U+1F600-1F64F"), "W3W"));
        assert!(unicode_range_matches_text(Some("U+1F600-1F64F"), "😀"));
        assert!(unicode_range_matches_text(None, "W3W"));
        assert!(
            unicode_range_matches_text(Some("invalid"), "W3W"),
            "invalid descriptors must reach registration for diagnostics"
        );
    }

    #[test]
    fn owner_scoped_registration_restores_previous_face_on_clear() {
        let registry = FontRegistry::new();
        registry
            .register_for_owner(
                1,
                FontFace {
                    family: "OwnerScoped".into(),
                    src: FontSource::Bytes(vec![1]),
                    ..Default::default()
                },
            )
            .unwrap();
        registry
            .register_for_owner(
                2,
                FontFace {
                    family: "OwnerScoped".into(),
                    src: FontSource::Bytes(vec![2]),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            registry
                .resolve("OwnerScoped", FontWeight::NORMAL, FontFaceStyle::Normal)
                .unwrap()
                .data
                .as_slice(),
            &[2]
        );
        registry.clear_owner(2);
        assert_eq!(
            registry
                .resolve("OwnerScoped", FontWeight::NORMAL, FontFaceStyle::Normal)
                .unwrap()
                .data
                .as_slice(),
            &[1]
        );
    }

    #[test]
    fn font_face_set_waits_for_all_concurrent_loading_cycles() {
        let set = FontFaceSet::new();
        set.mark_loading();
        set.mark_loading();
        assert!(!set.is_ready());
        set.mark_ready();
        assert!(
            !set.is_ready(),
            "one completed request must not finish a concurrent loading cycle"
        );
        set.mark_ready();
        assert!(set.is_ready());
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
/// Native, programmatic, and Browser stylesheet loads share this readiness
/// state. Concurrent cycles keep it loading until the final cycle settles.
pub struct FontFaceSet {
    /// Callbacks registered via `ready.then(cb)`.
    ready_callbacks: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
    /// Whether every active loading cycle has settled.
    is_ready: std::sync::atomic::AtomicBool,
    /// Number of in-flight loading cycles sharing this registry.
    active_loads: std::sync::atomic::AtomicUsize,
}

impl FontFaceSet {
    pub fn new() -> Self {
        Self {
            ready_callbacks: Mutex::new(Vec::new()),
            is_ready: std::sync::atomic::AtomicBool::new(false),
            active_loads: std::sync::atomic::AtomicUsize::new(0),
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

    pub fn mark_loading(&self) {
        self.active_loads
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        self.is_ready
            .store(false, std::sync::atomic::Ordering::Release);
    }

    /// Register a callback to be called when fonts are ready.
    /// If already ready, the callback is queued for the next `flush()`.
    pub fn ready_then(&self, cb: impl FnOnce() + Send + 'static) {
        self.ready_callbacks.lock().unwrap().push(Box::new(cb));
        // If already ready, flush immediately on next poll.
    }

    /// Complete one loading cycle and flush callbacks after the final cycle.
    pub fn mark_ready(&self) {
        let mut active = self.active_loads.load(std::sync::atomic::Ordering::Acquire);
        loop {
            if active == 0 {
                self.mark_ready_if_idle();
                return;
            }
            match self.active_loads.compare_exchange_weak(
                active,
                active - 1,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            ) {
                Ok(_) if active == 1 => {
                    self.mark_ready_if_idle();
                    return;
                }
                Ok(_) => return,
                Err(current) => active = current,
            }
        }
    }

    pub(crate) fn mark_ready_if_idle(&self) {
        if self.active_loads.load(std::sync::atomic::Ordering::Acquire) == 0 {
            self.is_ready
                .store(true, std::sync::atomic::Ordering::Release);
            self.flush_ready_callbacks();
        }
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
