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
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

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

    /// Whether this face is eligible for and contains a glyph for `character`.
    pub(crate) fn supports_character(&self, character: char) -> bool {
        let codepoint = character as u32;
        if self
            .unicode_ranges
            .as_ref()
            .is_some_and(|ranges| !ranges.iter().any(|range| range.contains(codepoint)))
        {
            return false;
        }
        self.parsed
            .as_ref()
            .is_some_and(|font| font.chars().contains_key(&character))
    }

    #[cfg(feature = "skia")]
    pub(crate) fn skia_typeface(&self) -> Option<skia_safe::Typeface> {
        self.skia_typeface
            .get_or_init(|| skia_safe::FontMgr::default().new_from_data(self.data.as_slice(), None))
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
    pub(crate) data: Arc<Vec<u8>>,
    pub(crate) index: u32,
    pub(crate) font: fontdue::Font,
}

static HOST_UI_FONT: OnceLock<HostUiFont> = OnceLock::new();

pub(crate) fn host_ui_font() -> &'static HostUiFont {
    HOST_UI_FONT.get_or_init(|| {
        let mut database = fontdb::Database::new();
        database.load_system_fonts();
        #[cfg(any(target_os = "android", target_env = "ohos"))]
        database.load_fonts_dir("/system/fonts");
        #[cfg(target_os = "ios")]
        {
            database.load_fonts_dir("/System/Library/Fonts");
            database.load_fonts_dir("/System/Library/Fonts/Core");
            database.load_fonts_dir("/System/Library/Fonts/Cache");
        }
        let id = database
            .query(&fontdb::Query {
                families: &[
                    fontdb::Family::Name("PingFang SC"),
                    fontdb::Family::Name("Microsoft YaHei"),
                    fontdb::Family::Name("Noto Sans CJK SC"),
                    fontdb::Family::Name("Noto Sans SC"),
                    fontdb::Family::Name("Noto Sans"),
                    fontdb::Family::Name("Roboto"),
                    fontdb::Family::SansSerif,
                ],
                ..fontdb::Query::default()
            })
            .or_else(|| database.faces().next().map(|face| face.id))
            .expect("host must provide at least one system font");
        let (data, index) = database
            .with_face_data(id, |data, index| (Arc::new(data.to_vec()), index))
            .expect("selected system font must remain readable");
        let font = fontdue::Font::from_bytes(
            data.as_slice(),
            fontdue::FontSettings {
                collection_index: index,
                ..fontdue::FontSettings::default()
            },
        )
        .expect("selected system font must be valid");
        HostUiFont { data, index, font }
    })
}

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
        self.register_for_owner(0, face)
    }

    /// Register a font for a lifecycle owner.
    pub fn register_for_owner(&self, owner: u64, face: FontFace) -> Result<(), String> {
        let data = match &face.src {
            FontSource::Bytes(b) => normalize_font_bytes(b).map(Arc::new)?,
            FontSource::Path(p) => {
                let bytes = std::fs::read(p)
                    .map_err(|e| format!("font load error {}: {e}", p.display()))?;
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
            fontdue::Font::from_bytes(data.as_slice(), fontdue::FontSettings::default())
                .ok()
                .map(Arc::new)
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
        fontdue::Font::from_bytes(bytes.as_slice(), fontdue::FontSettings::default())
            .map_err(|error| format!("local font {local_name:?} cannot be decoded: {error}"))?;
        face.src = FontSource::Bytes(bytes);
        self.register_for_owner(owner, face)
    }

    /// Remove every font registered by one page/stylesheet lifecycle owner.
    pub fn clear_owner(&self, owner: u64) {
        let mut fonts = self.fonts.lock().unwrap();
        fonts.retain(|_, entries| {
            entries.retain(|(existing_owner, _)| *existing_owner != owner);
            !entries.is_empty()
        });
        let mut families = self.families.lock().unwrap();
        families.retain(|(existing_owner, _)| *existing_owner != owner);
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
        self.resolve_stack(family, FontWeight(style.font_weight), face_style)
    }

    pub(crate) fn resolve_style_for_character(
        &self,
        style: &w3cos_std::style::Style,
        character: char,
    ) -> Option<LoadedFont> {
        let family = style.font_family.as_deref()?;
        let face_style = match style.font_style {
            w3cos_std::style::FontStyle::Normal => FontFaceStyle::Normal,
            w3cos_std::style::FontStyle::Italic => FontFaceStyle::Italic,
            w3cos_std::style::FontStyle::Oblique => FontFaceStyle::Oblique,
        };
        self.resolve_stack_for_character(
            family,
            FontWeight(style.font_weight),
            face_style,
            character,
        )
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
        let mut runs: Vec<ResolvedFontRun> = Vec::new();
        for (offset, character) in text.char_indices() {
            let font = self.resolve_stack_for_character(stack, weight, face_style, character);
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

    pub(crate) fn cascade_cache_key(&self, style: &w3cos_std::style::Style, text: &str) -> u64 {
        let mut hasher = DefaultHasher::new();
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

        for character in text.chars() {
            let selected = self
                .resolve_style_for_character(style, character)
                .and_then(|font| font.parsed());
            let font = selected.as_deref().unwrap_or(fallback);
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
    let eligible =
        |font: &LoadedFont| character.is_none_or(|character| font.supports_character(character));
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
            .filter(|(key, _)| key.family == family && (!match_style || key.style == style))
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
    fn host_ui_font_is_available_without_packaged_font_bytes() {
        let host = host_ui_font();
        assert_ne!(host.font.lookup_glyph_index('A'), 0);
        #[cfg(target_os = "macos")]
        assert_ne!(host.font.lookup_glyph_index('输'), 0);
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
