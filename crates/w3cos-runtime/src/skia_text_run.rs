//! Font positioning shared by Skia advance measurement, glyph paint and ink bounds.
//! Input is the visual-order font run supplied by the existing bidi stage.
use skia_safe::{Canvas, Font, GlyphId, Paint, Point, Rect, Typeface};
use std::{cell::RefCell, collections::HashMap, sync::Arc};
use w3cos_std::style::Style;

/// Use one raster policy for glyph painting and ink/advance measurement.
pub(crate) fn css_font(face: &Typeface, size: f32) -> Font {
    let font = Font::new(face, effective_font_size(size));
    // Match macOS Blink FontPlatformData::CreateSkFont: keep authored glyph
    // positions fractional rather than independently snapping every glyph.
    // Other platforms retain their existing host raster policy.
    #[cfg(target_os = "macos")]
    let font = {
        let mut font = font;
        font.set_subpixel(true)
            .set_linear_metrics(true)
            .set_embedded_bitmaps(false)
            .set_edging(skia_safe::font::Edging::SubpixelAntiAlias);
        font
    };
    font
}

/// Blink's FontDescription::EffectiveFontSize matches the platform font-cache
/// precision. This is a glyph/metrics boundary, not a computed CSS font-size
/// rewrite: line-height and inheritance retain the authored computed value.
pub(crate) fn effective_font_size(size: f32) -> f32 {
    if size.is_finite() && size >= 0.0 {
        (size * 100.0).floor() / 100.0
    } else {
        size
    }
}

/// Requested bold/italic styles can resolve to a normal registered face. Keep
/// synthetic ink in the font shared by paint and bounds, not in glyph advance.
pub(crate) fn css_font_for_style(face: &Typeface, size: f32, style: &Style) -> Font {
    let mut font = css_font(face, size);
    font.set_embolden(
        style.font_weight >= 600
            && face.font_style().weight() < skia_safe::font_style::Weight::SEMI_BOLD,
    );
    // Blink's synthetic italic uses a quarter-em shear. Do not synthesize
    // again when font matching already returned an italic/oblique face.
    font.set_skew_x(
        if style.font_style != w3cos_std::style::FontStyle::Normal
            && face.font_style().slant() == skia_safe::font_style::Slant::Upright
        {
            -0.25
        } else {
            0.0
        },
    );
    font
}

thread_local! {
    static FONT_TABLES: RefCell<HashMap<u32, Arc<Vec<u8>>>> = RefCell::new(HashMap::new());
    static OUTLINE_BOUNDS: RefCell<HashMap<(u32, u32, u32, u32, bool, GlyphId), Option<Rect>>> =
        RefCell::new(HashMap::new());
}

#[derive(Clone, PartialEq)]
pub(crate) struct GlyphRun {
    pub glyphs: Vec<GlyphId>,
    pub positions: Vec<Point>,
    pub advance: f32,
    /// UTF-8 cluster start and its contextual advance, including CSS spacing.
    pub clusters: Vec<(usize, f32)>,
    pub glyph_clusters: Vec<usize>,
    /// Empty for ordinary/native-feature runs; synthesis retains each glyph's
    /// font size independently from the inherited CSS size and line metrics.
    pub glyph_font_sizes: Vec<f32>,
}

impl GlyphRun {
    /// Vector ink before raster antialias expansion. Unknown bitmap outlines
    /// return None so callers retain conservative painting rather than cull.
    pub fn geometric_ink_bounds(&self, font: &Font) -> Option<Rect> {
        let mut ink = Rect::default();
        let mut known = true;
        self.visit_fonts(font, |font, range| {
            if let Some(bounds) =
                geometric_ink_bounds(font, &self.glyphs[range.clone()], &self.positions[range])
            {
                ink.join(bounds);
            } else {
                known = false;
            }
        });
        known.then_some(ink)
    }

    pub fn ink_bounds(&self, font: &Font) -> Option<Rect> {
        let mut ink = None::<Rect>;
        self.visit_fonts(font, |font, range| {
            let mut bounds = vec![Rect::default(); range.len()];
            font.get_bounds(&self.glyphs[range.clone()], &mut bounds, None);
            for (bounds, position) in bounds.into_iter().zip(&self.positions[range]) {
                if bounds.is_empty() {
                    continue;
                }
                let shifted = Rect::new(
                    bounds.left + position.x,
                    bounds.top + position.y,
                    bounds.right + position.x,
                    bounds.bottom + position.y,
                );
                if let Some(ink) = &mut ink {
                    ink.join(shifted);
                } else {
                    ink = Some(shifted);
                }
            }
        });
        ink
    }

    fn visit_fonts(&self, font: &Font, mut visit: impl FnMut(&Font, std::ops::Range<usize>)) {
        visit_glyph_fonts(font, &self.glyph_font_sizes, self.glyphs.len(), &mut visit);
    }

    pub(crate) fn draw(&self, canvas: &Canvas, origin: (f32, f32), font: &Font, paint: &Paint) {
        self.visit_fonts(font, |font, range| {
            canvas.draw_glyphs_at(
                &self.glyphs[range.clone()],
                &self.positions[range],
                origin,
                font,
                paint,
            )
        });
    }

    pub(crate) fn intercepts(&self, font: &Font, upper: f32, lower: f32) -> Vec<f32> {
        let mut result = Vec::new();
        self.visit_fonts(font, |font, range| {
            result.extend(font.get_intercepts(
                &self.glyphs[range.clone()],
                &self.positions[range],
                (upper, lower),
                None,
            ))
        });
        result
    }
}

pub(crate) fn visit_glyph_fonts(
    font: &Font,
    sizes: &[f32],
    count: usize,
    mut visit: impl FnMut(&Font, std::ops::Range<usize>),
) {
    if sizes.is_empty() {
        visit(font, 0..count);
        return;
    }
    debug_assert_eq!(sizes.len(), count);
    let mut start = 0;
    while start < count {
        let size = sizes[start];
        let end = sizes[start..]
            .iter()
            .position(|next| *next != size)
            .map_or(count, |offset| start + offset);
        let mut used = font.clone();
        used.set_size(effective_font_size(size));
        visit(&used, start..end);
        start = end;
    }
}

pub(crate) fn geometric_ink_bounds_with_sizes(
    font: &Font,
    glyphs: &[GlyphId],
    positions: &[Point],
    sizes: &[f32],
) -> Option<Rect> {
    let mut ink = Rect::default();
    let mut known = true;
    visit_glyph_fonts(font, sizes, glyphs.len(), |font, range| {
        if let Some(bounds) = geometric_ink_bounds(font, &glyphs[range.clone()], &positions[range])
        {
            ink.join(bounds);
        } else {
            known = false;
        }
    });
    known.then_some(ink)
}

/// Borrowed glyph slices share outline culling with shaped font runs.
pub(crate) fn geometric_ink_bounds(
    font: &Font,
    glyphs: &[GlyphId],
    positions: &[Point],
) -> Option<Rect> {
    let face = font.typeface().unique_id();
    let mut ink = Rect::default();
    for (&glyph, position) in glyphs.iter().zip(positions) {
        let key = (
            face,
            font.size().to_bits(),
            font.scale_x().to_bits(),
            font.skew_x().to_bits(),
            font.is_embolden(),
            glyph,
        );
        let bounds = OUTLINE_BOUNDS.with(|cache| {
            if let Some(bounds) = cache.borrow().get(&key) {
                return *bounds;
            }
            let bounds = font.get_path(glyph).map(|path| *path.bounds()).or_else(|| {
                let mut raster = [Rect::default()];
                font.get_bounds(&[glyph], &mut raster, None);
                raster[0].is_empty().then_some(Rect::default())
            });
            let mut cache = cache.borrow_mut();
            if cache.len() >= 2048 {
                cache.clear();
            }
            cache.insert(key, bounds);
            bounds
        })?;
        if !bounds.is_empty() {
            ink.join(Rect::new(
                bounds.left + position.x,
                bounds.top + position.y,
                bounds.right + position.x,
                bounds.bottom + position.y,
            ));
        }
    }
    Some(ink)
}

/// A parsed shaping face and the table bytes it borrows. `face` is declared
/// first so it drops before the bytes; the bytes sit in an `Arc` that is
/// never mutated, so the borrow stays valid while the entry lives.
struct ShapingFace {
    face: rustybuzz::Face<'static>,
    _bytes: Arc<Vec<u8>>,
}

thread_local! {
    static SHAPING_FACES: RefCell<w3cos_std::fxhash::FxHashMap<u32, std::rc::Rc<ShapingFace>>> =
        RefCell::new(Default::default());
}

fn shaping_face_cache_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("W3COS_SHAPING_FACE_CACHE").is_none_or(|v| v != "0"))
}

/// The rustybuzz face of a typeface, parsed once per face (Blink keeps one
/// HarfBuzzFace per FontPlatformData in its HarfBuzzFontCache rather than
/// rebuilding the face, and its lookup accelerators, for every run).
/// On by default; `W3COS_SHAPING_FACE_CACHE=0` parses the face per call.
fn with_shaping_face<R>(face: &Typeface, f: impl FnOnce(&rustybuzz::Face<'_>) -> R) -> Option<R> {
    if !shaping_face_cache_enabled() {
        let bytes = font_tables(face)?;
        let shaping_face = rustybuzz::Face::from_slice(&bytes, 0)?;
        return Some(f(&shaping_face));
    }
    let id = face.unique_id();
    let cached = SHAPING_FACES.with(|cache| cache.borrow().get(&id).cloned());
    let entry = match cached {
        Some(entry) => entry,
        None => {
            let bytes = font_tables(face)?;
            // SAFETY: the slice points into the Arc's heap buffer, which is
            // never mutated and is kept alive by `_bytes` for as long as the
            // face that borrows it (see ShapingFace).
            let slice: &'static [u8] =
                unsafe { std::slice::from_raw_parts(bytes.as_ptr(), bytes.len()) };
            let entry = std::rc::Rc::new(ShapingFace {
                face: rustybuzz::Face::from_slice(slice, 0)?,
                _bytes: bytes,
            });
            SHAPING_FACES.with(|cache| {
                let mut cache = cache.borrow_mut();
                // Bounded by concrete faces, like the table cache.
                if cache.len() >= 64 {
                    cache.clear();
                }
                cache.insert(id, entry.clone());
            });
            entry
        }
    };
    Some(f(&entry.face))
}

// Skia resolves the concrete system/registered face. Repackage its tables in
// memory so shaping cannot silently select a different font from painting.
fn font_tables(face: &Typeface) -> Option<Arc<Vec<u8>>> {
    FONT_TABLES.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(data) = cache.get(&face.unique_id()) {
            return Some(data.clone());
        }
        let mut tables: Vec<_> = face
            .read_table_tags()?
            .into_iter()
            .filter_map(|tag| face.copy_table_data(tag).map(|data| (tag, data)))
            .collect();
        tables.sort_by_key(|(tag, _)| *tag);
        let count = u16::try_from(tables.len()).ok()?;
        if count == 0 {
            return None;
        }
        let mut bytes = vec![0; 12 + tables.len() * 16];
        let cff = tables.iter().any(|(tag, _)| {
            *tag == u32::from_be_bytes(*b"CFF ") || *tag == u32::from_be_bytes(*b"CFF2")
        });
        bytes[..4].copy_from_slice(if cff { b"OTTO" } else { &[0, 1, 0, 0] });
        bytes[4..6].copy_from_slice(&count.to_be_bytes());
        let power = 1u16 << count.ilog2();
        bytes[6..8].copy_from_slice(&(power * 16).to_be_bytes());
        bytes[8..10].copy_from_slice(&(count.ilog2() as u16).to_be_bytes());
        bytes[10..12].copy_from_slice(&((count - power) * 16).to_be_bytes());
        for (i, (tag, data)) in tables.into_iter().enumerate() {
            let offset = u32::try_from(bytes.len()).ok()?;
            let length = u32::try_from(data.size()).ok()?;
            let entry = 12 + i * 16;
            bytes[entry..entry + 4].copy_from_slice(&tag.to_be_bytes());
            bytes[entry + 8..entry + 12].copy_from_slice(&offset.to_be_bytes());
            bytes[entry + 12..entry + 16].copy_from_slice(&length.to_be_bytes());
            bytes.extend_from_slice(data.as_bytes());
            while bytes.len() % 4 != 0 {
                bytes.push(0);
            }
        }
        // The cache is bounded by concrete faces, not by arbitrary text runs.
        if cache.len() >= 64 {
            cache.clear();
        }
        let data = Arc::new(bytes);
        cache.insert(face.unique_id(), data.clone());
        Some(data)
    })
}

pub(crate) fn shape_visual_run(text: &str, face: &Typeface, style: &Style) -> Option<GlyphRun> {
    shape_visual_run_impl(text, face, style, false, None)
}

pub(crate) fn shape_visual_run_with_clusters(
    text: &str,
    face: &Typeface,
    style: &Style,
) -> Option<GlyphRun> {
    shape_visual_run_impl(text, face, style, true, None)
}

pub(crate) fn shape_visual_run_with_expansion(
    text: &str,
    face: &Typeface,
    style: &Style,
    collect_clusters: bool,
    expansion: crate::text_layout::JustificationExpansion,
    byte_offset: usize,
) -> Option<GlyphRun> {
    shape_visual_run_impl(
        text,
        face,
        style,
        collect_clusters,
        Some((expansion, byte_offset)),
    )
}

// Blink's Apple path uses Skia nominal widths unless the font has a tracking
// table without bitmap strikes. Keep Rustybuzz's contextual adjustments, but
// replace its design-unit nominal input with the width of the painted face.
#[cfg(target_os = "macos")]
fn skia_nominal_advances(face: &Typeface, style: &Style, glyphs: &[GlyphId]) -> Option<Vec<f64>> {
    let tags = face.read_table_tags()?;
    if tags.contains(&u32::from_be_bytes(*b"trak")) && !tags.contains(&u32::from_be_bytes(*b"sbix"))
    {
        return None;
    }
    let mut widths = vec![0.0; glyphs.len()];
    css_font(face, style.font_size).get_widths(glyphs, &mut widths);
    Some(
        widths
            .into_iter()
            .map(|width| f64::from((width * 65536.0) as i32) / 65536.0)
            .collect(),
    )
}

/// Shaped runs kept by their text, face and the style inputs shaping reads
/// (the design of Blink's ShapeCache, platform/fonts/shaping/shape_cache.h:
/// a text run is shaped once per font and its ShapeResult reused, and the
/// cache is cleared when it reaches its bound). Layout measures a run for
/// its max-content size, again per wrapped line, and paint shapes it once
/// more; all of them read the same result. Justified and synthesized
/// small-caps runs read more of the style and are shaped each time.
/// On by default; `W3COS_SHAPE_CACHE=0` shapes every run.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct ShapeKey {
    text_hash: u64,
    face: u32,
    font_size: u32,
    letter_spacing: u32,
    word_spacing: u32,
    kern: bool,
    clusters: bool,
}

/// Blink's ShapeCache bound (kMaxSize).
const SHAPE_CACHE_MAX: usize = 10000;

#[derive(Default)]
struct FaceShapeCache {
    runs: w3cos_std::fxhash::FxHashMap<ShapeKey, Vec<(Box<str>, Option<GlyphRun>)>>,
    entries: usize,
}

/// One bounded cache per face, as Blink keeps one ShapeCache per font
/// (FontCache::GetShapeCache) rather than one shared by every font: a page
/// whose text spans several faces must not evict one face's words for
/// another's.
#[derive(Default)]
struct ShapeCache {
    faces: w3cos_std::fxhash::FxHashMap<u32, FaceShapeCache>,
}

thread_local! {
    static SHAPE_CACHE: RefCell<ShapeCache> = RefCell::new(ShapeCache::default());
}

fn shape_cache_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("W3COS_SHAPE_CACHE").is_none_or(|v| v != "0"))
}

fn shape_key(text: &str, face: &Typeface, style: &Style, collect_clusters: bool) -> ShapeKey {
    use std::hash::{Hash, Hasher};
    let mut hasher = w3cos_std::fxhash::FxHasher::default();
    text.hash(&mut hasher);
    ShapeKey {
        text_hash: hasher.finish(),
        face: face.unique_id(),
        font_size: style.font_size.to_bits(),
        letter_spacing: style.letter_spacing.to_bits(),
        word_spacing: style.word_spacing.to_bits(),
        kern: style.font_feature_kern.unwrap_or(style.font_kerning),
        clusters: collect_clusters,
    }
}

fn shape_visual_run_impl(
    text: &str,
    face: &Typeface,
    style: &Style,
    collect_clusters: bool,
    expansion: Option<(crate::text_layout::JustificationExpansion, usize)>,
) -> Option<GlyphRun> {
    if expansion.is_some()
        || style.font_variant == w3cos_std::style::FontVariant::SmallCaps
        || !shape_cache_enabled()
    {
        return shape_visual_run_uncached(text, face, style, collect_clusters, expansion);
    }
    let share = shape_cluster_share_mode();
    let lookup = |key: &ShapeKey| {
        SHAPE_CACHE.with(|cache| {
            cache
                .borrow()
                .faces
                .get(&key.face)?
                .runs
                .get(key)?
                .iter()
                .find(|(cached, _)| &**cached == text)
                .map(|(_, run)| run.clone())
        })
    };
    let key = shape_key(text, face, style, collect_clusters);
    if let Some(run) = lookup(&key) {
        #[cfg(debug_assertions)]
        debug_assert!(
            run == shape_visual_run_uncached(text, face, style, collect_clusters, None),
            "shape cache: stale run for {text:?}"
        );
        return run;
    }
    // `W3COS_SHAPE_CLUSTER_SHARE`: a run without clusters is the run with
    // clusters minus its cluster lists (shaping is the same), so both
    // lookups share one shaped entry (Blink keeps one ShapeResult per item
    // for measuring and line breaking).
    let cluster_key = (share != 0 && !collect_clusters).then(|| ShapeKey {
        clusters: true,
        ..key
    });
    let strip = |run: Option<GlyphRun>| {
        run.map(|mut run| {
            run.clusters = Vec::new();
            run.glyph_clusters = Vec::new();
            run
        })
    };
    if let Some(cluster_key) = cluster_key.as_ref()
        && let Some(run) = lookup(cluster_key)
    {
        let run = strip(run);
        if share == 2 {
            let direct = shape_visual_run_uncached(text, face, style, false, None);
            if direct != run {
                eprintln!("[shape-cluster-share-verify] mismatch for {text:?}");
                SHAPE_CLUSTER_SHARE_MISMATCHES.with(|count| count.set(count.get() + 1));
                return direct;
            }
        }
        return run;
    }
    let store_key = cluster_key.unwrap_or(key);
    let shaped = shape_visual_run_uncached(text, face, style, store_key.clusters, None);
    SHAPE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let cache = cache.faces.entry(store_key.face).or_default();
        if cache.entries >= SHAPE_CACHE_MAX {
            cache.runs.clear();
            cache.entries = 0;
        }
        cache
            .runs
            .entry(store_key)
            .or_default()
            .push((text.into(), shaped.clone()));
        cache.entries += 1;
    });
    if store_key.clusters == collect_clusters {
        return shaped;
    }
    let run = strip(shaped);
    if share == 2 {
        let direct = shape_visual_run_uncached(text, face, style, false, None);
        if direct != run {
            eprintln!("[shape-cluster-share-verify] mismatch for {text:?}");
            SHAPE_CLUSTER_SHARE_MISMATCHES.with(|count| count.set(count.get() + 1));
            return direct;
        }
    }
    run
}

/// `W3COS_SHAPE_CLUSTER_SHARE=1|verify` (off by default): see
/// `shape_visual_run_impl`; `verify` also shapes without clusters and
/// compares.
fn shape_cluster_share_mode() -> u8 {
    #[cfg(test)]
    if let Some(mode) = SHAPE_CLUSTER_SHARE_OVERRIDE.with(std::cell::Cell::get) {
        return mode;
    }
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(
        || match std::env::var("W3COS_SHAPE_CLUSTER_SHARE").as_deref() {
            Ok("1") => 1,
            Ok("verify") => 2,
            _ => 0,
        },
    )
}

thread_local! {
    #[cfg(test)]
    pub(crate) static SHAPE_CLUSTER_SHARE_OVERRIDE: std::cell::Cell<Option<u8>> =
        const { std::cell::Cell::new(None) };
    pub(crate) static SHAPE_CLUSTER_SHARE_MISMATCHES: std::cell::Cell<u64> =
        const { std::cell::Cell::new(0) };
}

fn shape_visual_run_uncached(
    text: &str,
    face: &Typeface,
    style: &Style,
    collect_clusters: bool,
    expansion: Option<(crate::text_layout::JustificationExpansion, usize)>,
) -> Option<GlyphRun> {
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.guess_segment_properties();
    let small_caps = style.font_variant == w3cos_std::style::FontVariant::SmallCaps;
    let native_caps = small_caps && supports_small_caps(face, buffer.script());
    if small_caps && !native_caps {
        return shape_synthetic_small_caps(text, face, style, collect_clusters, expansion);
    }
    shape_buffer(
        text,
        face,
        style,
        collect_clusters,
        expansion,
        buffer,
        native_caps,
        true,
    )
}

/// The shaper uses the default language system. A feature merely present for
/// another script must not suppress synthesis in this run.
fn supports_small_caps(face: &Typeface, script: rustybuzz::Script) -> bool {
    with_shaping_face(face, |face| face_supports_small_caps(face, script)).unwrap_or(false)
}

fn face_supports_small_caps(face: &rustybuzz::Face<'_>, script: rustybuzz::Script) -> bool {
    let Some(table) = face.tables().gsub else {
        return false;
    };
    let tag_bytes = script
        .tag()
        .0
        .to_be_bytes()
        .map(|byte| byte.to_ascii_lowercase());
    let tag = rustybuzz::ttf_parser::Tag::from_bytes(&tag_bytes);
    let script = table.scripts.find(tag).or_else(|| {
        table
            .scripts
            .find(rustybuzz::ttf_parser::Tag::from_bytes(b"DFLT"))
    });
    script
        .and_then(|script| script.default_language)
        .is_some_and(|language| {
            language
                .feature_indices
                .into_iter()
                .chain(language.required_feature)
                .filter_map(|index| table.features.get(index))
                .any(|feature| feature.tag == rustybuzz::ttf_parser::Tag::from_bytes(b"smcp"))
        })
}

pub(crate) fn synthesized_small_caps_size(size: f32) -> f32 {
    // Chromium141 SimpleFontData::CreateScaledFontData; this is a glyph font,
    // not an author style mutation (V2059 Ahem96 ->67px; Times96 ->67px).
    (size * 0.7).round()
}

fn shape_synthetic_small_caps(
    text: &str,
    face: &Typeface,
    style: &Style,
    collect_clusters: bool,
    expansion: Option<(crate::text_layout::JustificationExpansion, usize)>,
) -> Option<GlyphRun> {
    let language = style
        .custom_properties
        .as_ref()
        .and_then(|properties| properties.get("--w3cos-internal-text-language"))
        .map(String::as_str);
    let mut segments = Vec::<(usize, bool)>::new();
    let mut behavior = false;
    for (index, character) in text.char_indices() {
        let source = &text[index..index + character.len_utf8()];
        let uppercase = w3cos_std::style::transformed_text(
            source,
            w3cos_std::style::TextTransform::Uppercase,
            language,
            false,
        );
        let combining = unicode_ccc::get_canonical_combining_class(character)
            != unicode_ccc::CanonicalCombiningClass::NotReordered;
        let next = if combining {
            behavior
        } else {
            uppercase.as_ref() != source
        };
        if segments.is_empty() || behavior != next {
            segments.push((index, next));
        }
        behavior = next;
    }
    let mut output = GlyphRun {
        glyphs: Vec::new(),
        positions: Vec::new(),
        advance: 0.0,
        clusters: Vec::new(),
        glyph_clusters: Vec::new(),
        glyph_font_sizes: Vec::new(),
    };
    let mut cursor = 0.0_f64;
    for (segment, &(start, uppercase)) in segments.iter().enumerate() {
        let end = segments
            .get(segment + 1)
            .map_or(text.len(), |segment| segment.0);
        let source = &text[start..end];
        let mut buffer = rustybuzz::UnicodeBuffer::new();
        for (index, character) in source.char_indices() {
            let value = &source[index..index + character.len_utf8()];
            let value = if uppercase {
                w3cos_std::style::transformed_text(
                    value,
                    w3cos_std::style::TextTransform::Uppercase,
                    language,
                    false,
                )
            } else {
                std::borrow::Cow::Borrowed(value)
            };
            // Uppercase expansions share the original UTF-8 source cluster;
            // CSS spacing and justification apply once, not per expansion.
            for character in value.chars() {
                buffer.add(character, u32::try_from(index).ok()?);
            }
        }
        buffer.guess_segment_properties();
        let mut used = style.clone();
        used.font_variant = w3cos_std::style::FontVariant::Normal;
        if uppercase {
            used.font_size = synthesized_small_caps_size(style.font_size);
        }
        let run = shape_buffer(
            source,
            face,
            &used,
            collect_clusters,
            expansion.map(|(expansion, offset)| (expansion, offset + start)),
            buffer,
            false,
            false,
        )?;
        output.positions.extend(
            run.positions
                .iter()
                .map(|position| Point::new((cursor + f64::from(position.x)) as f32, position.y)),
        );
        output
            .glyph_font_sizes
            .extend(std::iter::repeat_n(used.font_size, run.glyphs.len()));
        output.glyphs.extend(run.glyphs);
        output.glyph_clusters.extend(
            run.glyph_clusters
                .into_iter()
                .map(|cluster| start + cluster),
        );
        output.clusters.extend(
            run.clusters
                .into_iter()
                .map(|(cluster, advance)| (start + cluster, advance)),
        );
        cursor += f64::from(run.advance);
    }
    output.advance = cursor as f32;
    Some(output)
}

fn shape_buffer(
    text: &str,
    face: &Typeface,
    style: &Style,
    collect_clusters: bool,
    expansion: Option<(crate::text_layout::JustificationExpansion, usize)>,
    buffer: rustybuzz::UnicodeBuffer,
    native_caps: bool,
    reusable: bool,
) -> Option<GlyphRun> {
    with_shaping_face(face, |shaping_face| {
        shape_buffer_with_face(
            text,
            face,
            shaping_face,
            style,
            collect_clusters,
            expansion,
            buffer,
            native_caps,
            reusable,
        )
    })?
}

#[allow(clippy::too_many_arguments)]
fn shape_buffer_with_face(
    text: &str,
    face: &Typeface,
    shaping_face: &rustybuzz::Face<'_>,
    style: &Style,
    collect_clusters: bool,
    expansion: Option<(crate::text_layout::JustificationExpansion, usize)>,
    mut buffer: rustybuzz::UnicodeBuffer,
    native_caps: bool,
    // `buffer` holds exactly `text` (not a synthesized small-caps segment).
    reusable: bool,
) -> Option<GlyphRun> {
    // The typeface only supplies macOS nominal advances.
    #[cfg(not(target_os = "macos"))]
    let _ = face;
    // Bidi ordering and mirrored characters have already been resolved by
    // font_render_text_for_style. Do not reorder this visual run a second time.
    buffer.set_direction(rustybuzz::Direction::LeftToRight);
    let mut features: Vec<rustybuzz::Feature> = Vec::new();
    if native_caps {
        features.push("smcp=1".parse().ok()?);
    }
    if style.letter_spacing != 0.0 {
        features.extend(
            ["liga=0", "clig=0"]
                .into_iter()
                .filter_map(|feature| feature.parse::<rustybuzz::Feature>().ok()),
        );
    }
    if !style.font_feature_kern.unwrap_or(style.font_kerning) {
        features.extend(
            ["kern=0"]
                .into_iter()
                .filter_map(|feature| feature.parse::<rustybuzz::Feature>().ok()),
        );
    }
    let features_key = (
        native_caps,
        style.letter_spacing != 0.0,
        !style.font_feature_kern.unwrap_or(style.font_kerning),
    );
    let verify_text = (shape_plan_cache_verify()
        && style.font_variant != w3cos_std::style::FontVariant::SmallCaps)
        .then_some(text);
    let shaped = shape_raw_reusing(
        text,
        face.unique_id(),
        shaping_face,
        &features,
        features_key,
        buffer,
        verify_text,
        reusable && expansion.is_none() && !native_caps,
    );
    // Do not derive a cluster's width by subtracting accumulated f32 cursors:
    // its precision would depend on the unrelated text before the cluster.
    // Keep accumulation precise; Skia's point/advance boundary remains f32.
    let scale =
        f64::from(effective_font_size(style.font_size)) / f64::from(shaping_face.units_per_em());
    let glyphs = shaped
        .iter()
        .map(|glyph| u16::try_from(glyph.id).ok())
        .collect::<Option<Vec<_>>>()?;
    #[cfg(target_os = "macos")]
    let nominal_advances = skia_nominal_advances(face, style, &glyphs);
    let mut positions = Vec::with_capacity(shaped.len());
    let mut cursor = 0.0;
    let mut clusters = Vec::new();
    let mut glyph_clusters = Vec::new();
    let mut cluster_start_advance = 0.0;
    for (i, info) in shaped.iter().enumerate() {
        let position = info;
        if collect_clusters {
            glyph_clusters.push(info.cluster as usize);
        }
        positions.push(Point::new(
            (cursor + f64::from(position.x_offset) * scale) as f32,
            (-f64::from(position.y_offset) * scale) as f32,
        ));
        let advance = f64::from(position.x_advance) * scale;
        #[cfg(target_os = "macos")]
        let advance = if position.x_advance != 0 {
            nominal_advances
                .as_ref()
                .and_then(|advances| {
                    let nominal = shaping_face
                        .glyph_hor_advance(rustybuzz::ttf_parser::GlyphId(glyphs[i]))?;
                    Some(advances[i] + f64::from(position.x_advance - i32::from(nominal)) * scale)
                })
                .unwrap_or(advance)
        } else {
            advance
        };
        // Apple inline shaping consumes 16.16 advances, including contextual
        // adjustments. Retaining design-unit fractions until the completed
        // run is snapped can move a font boundary by a whole CSS LayoutUnit.
        // Justification uses the same precision on the other backends too.
        let advance = if cfg!(target_os = "macos") || expansion.is_some() {
            (advance * 65536.0).trunc() / 65536.0
        } else {
            advance
        };
        cursor += advance;
        let cluster_ends = shaped
            .get(i + 1)
            .is_none_or(|next| next.cluster != info.cluster);
        if cluster_ends {
            // CSS inline advances retain spacing after the final typographic
            // character, including when the next character is in another run.
            cursor += f64::from(style.letter_spacing);
            if text
                .get(info.cluster as usize..)
                .and_then(|text| text.chars().next())
                .is_some_and(|character| matches!(character, ' ' | '\u{00a0}'))
            {
                cursor += f64::from(style.word_spacing);
            }
            if let Some((expansion, byte_offset)) = expansion
                && text
                    .get(info.cluster as usize..)
                    .is_some_and(|text| text.starts_with(' '))
            {
                cursor += expansion.at_space(byte_offset + info.cluster as usize);
            }
            if collect_clusters {
                clusters.push((
                    info.cluster as usize,
                    (cursor - cluster_start_advance) as f32,
                ));
                cluster_start_advance = cursor;
            }
        }
    }
    Some(GlyphRun {
        glyphs,
        positions,
        advance: cursor as f32,
        clusters,
        glyph_clusters,
        glyph_font_sizes: Vec::new(),
    })
}

#[cfg(test)]
mod shaping_face_tests {
    use super::*;

    #[test]
    fn cluster_shared_runs_match_direct_shaping() {
        let Some(face) =
            skia_safe::FontMgr::new().legacy_make_typeface(None, skia_safe::FontStyle::normal())
        else {
            return;
        };
        struct Off;
        impl Drop for Off {
            fn drop(&mut self) {
                SHAPE_CLUSTER_SHARE_OVERRIDE.with(|mode| mode.set(None));
            }
        }
        let _off = Off;
        let style = Style {
            font_size: 15.0,
            letter_spacing: 0.5,
            ..Style::default()
        };
        let before = SHAPE_CLUSTER_SHARE_MISMATCHES.with(std::cell::Cell::get);
        for mode in [2, 1] {
            SHAPE_CLUSTER_SHARE_OVERRIDE.with(|value| value.set(Some(mode)));
            for (at, text) in [
                "Waffle office AVA To",
                "fi fl ffi, kerning: AV Ta",
                "a b  c",
            ]
            .into_iter()
            .enumerate()
            {
                let text = format!("{text} {mode}");
                // Clusters first, then without; and the other order.
                let order = if at % 2 == 0 {
                    [true, false]
                } else {
                    [false, true]
                };
                for clusters in order.into_iter().chain(order) {
                    let run = shape_visual_run_impl(&text, &face, &style, clusters, None);
                    let direct = shape_visual_run_uncached(&text, &face, &style, clusters, None);
                    assert!(run == direct, "{text:?} clusters {clusters}");
                }
            }
        }
        assert_eq!(
            SHAPE_CLUSTER_SHARE_MISMATCHES.with(std::cell::Cell::get),
            before
        );
    }

    /// `W3COS_SHAPE_REUSE`: a growing paragraph keeps the glyphs before
    /// its last safe cut and matches shaping the whole text.
    #[test]
    fn shape_reuse_matches_whole_shaping() {
        let Some(face) =
            skia_safe::FontMgr::new().legacy_make_typeface(None, skia_safe::FontStyle::normal())
        else {
            return;
        };
        struct Off;
        impl Drop for Off {
            fn drop(&mut self) {
                SHAPE_REUSE_OVERRIDE.with(|mode| mode.set(None));
            }
        }
        let _off = Off;
        for (mode, spacing) in [(2u8, 0.0f32), (1, 0.0), (2, 0.5), (1, 0.5)] {
            SHAPE_REUSE_OVERRIDE.with(|value| value.set(Some(mode)));
            SHAPE_REUSE.with(|kept| kept.borrow_mut().clear());
            let hits = SHAPE_REUSE_HITS.with(std::cell::Cell::get);
            let style = Style {
                font_size: 15.0,
                letter_spacing: spacing,
                ..Style::default()
            };
            let mut text = String::from("Waffle office AVA To fi fl ffi, kerning: AV Ta. ");
            for step in 0..12 {
                text.push_str(["Wave ", "office ", "AVAT ", "fluffy, ", "To. "][step % 5]);
                for clusters in [true, false] {
                    let reused = shape_visual_run_uncached(&text, &face, &style, clusters, None);
                    SHAPE_REUSE_OVERRIDE.with(|value| value.set(Some(0)));
                    let direct = shape_visual_run_uncached(&text, &face, &style, clusters, None);
                    SHAPE_REUSE_OVERRIDE.with(|value| value.set(Some(mode)));
                    assert!(
                        reused == direct,
                        "mode {mode} spacing {spacing} step {step} clusters {clusters}"
                    );
                }
            }
            assert!(
                SHAPE_REUSE_HITS.with(std::cell::Cell::get) > hits,
                "mode {mode}: no reuse"
            );
        }
        assert_eq!(SHAPE_REUSE_MISMATCHES.with(std::cell::Cell::get), 0);
    }

    #[test]
    fn cached_shaping_face_shapes_like_a_freshly_parsed_face() {
        // Any concrete system face; a host without fonts has nothing to shape.
        let Some(face) =
            skia_safe::FontMgr::new().legacy_make_typeface(None, skia_safe::FontStyle::normal())
        else {
            return;
        };
        let style = Style {
            font_size: 16.0,
            ..Style::default()
        };
        let text = "Waffle office AVA To";
        let buffer = || {
            let mut buffer = rustybuzz::UnicodeBuffer::new();
            buffer.push_str(text);
            buffer.guess_segment_properties();
            buffer
        };
        let bytes = font_tables(&face).unwrap();
        let fresh = rustybuzz::Face::from_slice(&bytes, 0).unwrap();
        let expected = shape_buffer_with_face(
            text,
            &face,
            &fresh,
            &style,
            true,
            None,
            buffer(),
            false,
            false,
        )
        .unwrap();
        for _ in 0..2 {
            let cached =
                shape_buffer(text, &face, &style, true, None, buffer(), false, false).unwrap();
            assert_eq!(cached.glyphs, expected.glyphs);
            assert_eq!(cached.positions, expected.positions);
            assert_eq!(cached.advance.to_bits(), expected.advance.to_bits());
            assert_eq!(cached.clusters, expected.clusters);
        }
        assert!(
            !shaping_face_cache_enabled()
                || SHAPING_FACES.with(|cache| cache.borrow().contains_key(&face.unique_id()))
        );
    }

    #[test]
    fn cached_shape_plans_shape_like_fresh_plans() {
        let Some(face) =
            skia_safe::FontMgr::new().legacy_make_typeface(None, skia_safe::FontStyle::normal())
        else {
            return;
        };
        let bytes = font_tables(&face).unwrap();
        let shaping_face = rustybuzz::Face::from_slice(&bytes, 0).unwrap();
        let buffer = |text: &str| {
            let mut buffer = rustybuzz::UnicodeBuffer::new();
            buffer.push_str(text);
            buffer.guess_segment_properties();
            buffer.set_direction(rustybuzz::Direction::LeftToRight);
            buffer
        };
        let feature_sets: [(&[&str], (bool, bool, bool)); 3] = [
            (&[], (false, false, false)),
            (&["liga=0", "clig=0"], (false, true, false)),
            (&["kern=0"], (false, false, true)),
        ];
        SHAPE_PLAN_OVERRIDE.with(|cell| cell.set(Some(true)));
        for text in ["Waffle office AVA To", "fi fl ffi", "Привет мир", "x"] {
            for (names, key) in feature_sets {
                let features: Vec<rustybuzz::Feature> =
                    names.iter().filter_map(|name| name.parse().ok()).collect();
                let expected = rustybuzz::shape(&shaping_face, &features, buffer(text));
                // The second round shapes with the plan the first cached.
                for _ in 0..2 {
                    let cached = shape_with_cached_plan(
                        face.unique_id(),
                        &shaping_face,
                        &features,
                        key,
                        buffer(text),
                        None,
                    );
                    let ids = |run: &rustybuzz::GlyphBuffer| {
                        run.glyph_infos()
                            .iter()
                            .map(|info| (info.glyph_id, info.cluster))
                            .collect::<Vec<_>>()
                    };
                    let positions = |run: &rustybuzz::GlyphBuffer| {
                        run.glyph_positions()
                            .iter()
                            .map(|p| (p.x_advance, p.y_advance, p.x_offset, p.y_offset))
                            .collect::<Vec<_>>()
                    };
                    assert_eq!(ids(&cached), ids(&expected), "{text} {names:?}");
                    assert_eq!(positions(&cached), positions(&expected), "{text} {names:?}");
                }
            }
        }
        assert!(SHAPE_PLANS.with(|plans| !plans.borrow().is_empty()));
        SHAPE_PLAN_OVERRIDE.with(|cell| cell.set(None));
    }
}

#[cfg(all(test, target_os = "macos"))]
mod precision_tests {
    use super::*;

    #[test]
    fn synthetic_small_caps_keep_css_size_and_original_source_clusters() {
        use w3cos_std::style::FontVariant;
        let face = crate::font_face::pinned_font_mgr()
            .match_family_style("Times", skia_safe::FontStyle::normal())
            .unwrap();
        let style = Style {
            font_size: 96.0,
            font_variant: FontVariant::SmallCaps,
            ..Style::default()
        };
        let run = shape_visual_run_with_clusters("eE", &face, &style).unwrap();
        assert_eq!(
            style.font_size, 96.0,
            "glyph synthesis must not rewrite CSS size"
        );
        assert_eq!(run.glyph_font_sizes, vec![67.0, 96.0]);
        assert_eq!(
            run.glyphs[0], run.glyphs[1],
            "lowercase e synthesizes uppercase E"
        );
        assert_eq!(run.glyph_clusters, vec![0, 1]);
        // DEFAULT Chromium141 original font-003 span Range, V2059.
        assert_eq!(
            crate::text_layout::inline_layout_advance(run.clusters[0].1),
            40.9375
        );
        let font = css_font_for_style(&face, style.font_size, &style);
        let first = geometric_ink_bounds_with_sizes(
            &font,
            &run.glyphs[..1],
            &run.positions[..1],
            &run.glyph_font_sizes[..1],
        )
        .unwrap();
        let small = css_font_for_style(&face, 67.0, &style);
        assert_eq!(
            first,
            geometric_ink_bounds(&small, &run.glyphs[..1], &run.positions[..1]).unwrap()
        );
    }

    #[test]
    fn synthetic_small_caps_expansions_and_combining_marks_keep_source_spacing() {
        use w3cos_std::style::FontVariant;
        let face = crate::font_face::pinned_font_mgr()
            .match_family_style("Times", skia_safe::FontStyle::normal())
            .unwrap();
        let style = Style {
            font_size: 16.0,
            font_variant: FontVariant::SmallCaps,
            ..Style::default()
        };
        let plain = shape_visual_run_with_clusters("ßA", &face, &style).unwrap();
        let spaced = shape_visual_run_with_clusters(
            "ßA",
            &face,
            &Style {
                letter_spacing: 2.0,
                ..style.clone()
            },
        )
        .unwrap();
        assert_eq!(spaced.glyph_clusters, vec![0, 0, 2]);
        assert_eq!(
            spaced.advance - plain.advance,
            4.0,
            "one spacing per original source character, not per uppercase expansion"
        );
        let marked = shape_visual_run_with_clusters("e\u{0301}E", &face, &style).unwrap();
        assert_eq!(*marked.glyph_clusters.last().unwrap(), 3);
        assert_eq!(*marked.glyph_font_sizes.last().unwrap(), 16.0);
        assert!(
            marked.glyph_font_sizes[..marked.glyph_font_sizes.len() - 1]
                .iter()
                .all(|size| *size == 11.0)
        );
    }

    #[test]
    fn browser_justified_pingfang_character_origin_uses_layout_range() {
        let face = crate::font_face::pinned_font_mgr()
            .match_family_style("PingFang SC", skia_safe::FontStyle::normal())
            .unwrap();
        let text = "decreased): its sides should be approximately equidistant from the";
        let style = Style {
            font_size: 16.0,
            ..Style::default()
        };
        let raw = shape_visual_run(text, &face, &style).unwrap();
        let expansion =
            crate::text_layout::justification_expansion(text, 544.0, raw.advance).unwrap();
        let run = shape_visual_run_with_expansion(text, &face, &style, true, expansion, 0).unwrap();
        let cluster = text.find("equidistant").unwrap() + 8;
        let index = run
            .glyph_clusters
            .iter()
            .position(|value| *value == cluster)
            .unwrap();
        // DEFAULT Chromium 141 DOM Range start for this glyph, V711.
        // The range coordinate floors the precise origin to 1/64 CSS px.
        let origin = 128.0 + run.positions[index].x;
        assert_eq!(
            (origin * 64.0).floor() / 64.0,
            567.609375,
            "original WPT line origin before rasterization"
        );
    }

    #[test]
    fn justified_full_line_preserves_contextual_glyphs_and_space_advances() {
        let face = crate::font_face::pinned_font_mgr()
            .match_family_style("Times", skia_safe::FontStyle::normal())
            .unwrap();
        let text = "Some words justified";
        let style = Style {
            font_size: 20.0,
            ..Style::default()
        };
        let raw = shape_visual_run(text, &face, &style).unwrap();
        let available = crate::text_layout::inline_layout_advance(raw.advance) + 20.0;
        let expansion =
            crate::text_layout::justification_expansion(text, available, raw.advance).unwrap();
        let justified =
            shape_visual_run_with_expansion(text, &face, &style, true, expansion, 0).unwrap();
        let reference = shape_visual_run_with_clusters(
            text,
            &face,
            &Style {
                word_spacing: 10.0,
                ..style
            },
        )
        .unwrap();
        assert_eq!(justified.glyphs, raw.glyphs);
        assert_eq!(justified.glyph_clusters, reference.glyph_clusters);
        assert_eq!(justified.positions, reference.positions);
        assert_eq!(justified.advance, reference.advance);
    }

    #[test]
    fn effective_platform_font_size_preserves_computed_css_size() {
        let face = crate::font_face::pinned_font_mgr()
            .match_family_style("Courier", skia_safe::FontStyle::normal())
            .unwrap();
        for (computed, effective) in [
            (13.333333_f32, 13.33_f32),
            (19.219, 19.21),
            (16.0, 16.0),
            (0.009, 0.0),
            (0.0, 0.0),
        ] {
            let style = Style {
                font_size: computed,
                ..Style::default()
            };
            assert_eq!(
                css_font_for_style(&face, computed, &style).size(),
                effective
            );
            assert_eq!(
                style.font_size, computed,
                "computed/inherited size is not quantized"
            );
        }
        let style = Style {
            font_family: Some("monospace".into()),
            font_size: (10.0_f64 * 96.0 / 72.0) as f32,
            ..Style::default()
        };
        let text = " The   spacing  on      these         two  sentences need to be the    same!  ";
        let run = shape_visual_run_with_clusters(text, &face, &style).unwrap();
        // Chrome155: all 78 Courier glyphs measure 7.999298095703125px.
        // Their precise inline sum is not Canvas's separately accumulated
        // 623.945068359375px total. Keep the glyph and DOM Range boundaries
        // exact; do not substitute a Canvas measurement for this inline run.
        assert_eq!(run.clusters.len(), 78);
        for (_, advance) in &run.clusters {
            assert_eq!(*advance, 7.999298095703125_f32);
        }
        assert_eq!(run.advance, 623.94525146484375_f32);
        assert_eq!(
            crate::text_layout::inline_layout_advance(run.advance),
            623.953125
        );
    }

    #[test]
    fn synthetic_italic_only_shears_upright_faces_and_preserves_advance() {
        let manager = crate::font_face::pinned_font_mgr();
        for face_style in [
            skia_safe::FontStyle::normal(),
            skia_safe::FontStyle::italic(),
        ] {
            let face = manager.match_family_style("Times", face_style).unwrap();
            assert_eq!(face.font_style().slant(), face_style.slant());
            for requested in [
                w3cos_std::style::FontStyle::Normal,
                w3cos_std::style::FontStyle::Italic,
                w3cos_std::style::FontStyle::Oblique,
            ] {
                let style = Style {
                    font_style: requested,
                    font_size: 16.0,
                    ..Style::default()
                };
                let font = css_font_for_style(&face, 16.0, &style);
                let synthetic = requested != w3cos_std::style::FontStyle::Normal
                    && face_style.slant() == skia_safe::font_style::Slant::Upright;
                assert_eq!(font.skew_x(), if synthetic { -0.25 } else { 0.0 });
                let glyphs = font.str_to_glyphs_vec("including");
                let mut actual = vec![0.0; glyphs.len()];
                let mut regular = actual.clone();
                font.get_widths(&glyphs, &mut actual);
                css_font(&face, 16.0).get_widths(&glyphs, &mut regular);
                assert_eq!(
                    actual, regular,
                    "synthetic ink must not change text advance"
                );
                if synthetic {
                    let positions = vec![Point::new(0.0, 0.0); glyphs.len()];
                    assert_ne!(
                        geometric_ink_bounds(&font, &glyphs, &positions),
                        geometric_ink_bounds(&css_font(&face, 16.0), &glyphs, &positions),
                        "culling bounds must use the same synthetic ink as painting"
                    );
                }
            }
        }
    }

    #[test]
    fn macos_untracked_font_advance_matches_skia_fixed_point_callback() {
        let face = crate::font_face::pinned_font_mgr()
            .match_family_style("Times", skia_safe::FontStyle::normal())
            .unwrap();
        let tags = face.read_table_tags().unwrap();
        assert!(!tags.contains(&u32::from_be_bytes(*b"trak")));
        let style = Style {
            font_family: Some("serif".into()),
            font_size: 19.2,
            font_kerning: false,
            ..Style::default()
        };
        let run = shape_visual_run_with_clusters("n", &face, &style).unwrap();
        let mut widths = vec![0.0; run.glyphs.len()];
        css_font_for_style(&face, style.font_size, &style).get_widths(&run.glyphs, &mut widths);
        // Chromium 141's SkiaScalarToHarfBuzzPosition uses a truncating
        // ClampTo<int> conversion to 16.16 after SkFont glyph measurement.
        let expected = (widths[0] * 65536.0) as i32 as f32 / 65536.0;
        eprintln!(
            "Times n: skia={} callback={} native={}",
            widths[0], expected, run.advance
        );
        assert_eq!(
            run.advance, expected,
            "a font without trak must use the same nominal advance source as Skia paint"
        );
        // Diagnostic data only: Canvas measurements and text ranges are not
        // interchangeable with glyph positions or original WPT acceptance.
        let mut diagnostic_style = style.clone();
        diagnostic_style.font_kerning = true;
        let diagnostic = ["This line is all in one font size ", "This line is all ", "in one font", " size "]
            .into_iter().map(|text| {
                let run = shape_visual_run_with_clusters(text, &face, &diagnostic_style).unwrap();
                let glyphs = run.positions.iter().zip(&run.glyph_clusters)
                    .map(|(point, cluster)| serde_json::json!({"cluster":cluster,"x":point.x}))
                    .collect::<Vec<_>>();
                let prefixes = (0..=text.len()).map(|end| {
                    let prefix = shape_visual_run(&text[..end], &face, &diagnostic_style).unwrap();
                    serde_json::json!({"end":end,"width":prefix.advance})
                }).collect::<Vec<_>>();
                serde_json::json!({"text":text,"advance":run.advance,"glyphs":glyphs,"prefixes":prefixes})
            }).collect::<Vec<_>>();
        eprintln!(
            "AUTHORED_GLYPH_DIAGNOSTIC {}",
            serde_json::to_string(&diagnostic).unwrap()
        );
    }

    #[test]
    fn fractional_cluster_advance_does_not_depend_on_prefix_length() {
        let face = crate::font_face::pinned_font_mgr()
            .match_family_style("Times", skia_safe::FontStyle::normal())
            .unwrap();
        let style = Style {
            font_family: Some("serif".into()),
            font_size: 19.2,
            ..Style::default()
        };
        let prefix = "This line is all ";
        let short = shape_visual_run_with_clusters("font", &face, &style).unwrap();
        let long = shape_visual_run_with_clusters(&format!("{prefix}font"), &face, &style).unwrap();
        let advance = |run: &GlyphRun, byte| {
            run.clusters
                .iter()
                .find(|(start, _)| *start == byte)
                .unwrap()
                .1
        };
        assert_eq!(
            advance(&long, prefix.len() + 2),
            advance(&short, 2),
            "the same n/t shaping context must not inherit accumulated cursor rounding"
        );
    }
}

#[cfg(test)]
thread_local! {
    static SHAPE_PLAN_OVERRIDE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

thread_local! {
    /// HarfBuzz's `hb_shape_plan_create_cached`: one plan per (face,
    /// direction, script, language, user features), as Blink's HarfBuzz
    /// shaper reuses. Faces are keyed by Skia's typeface id, which is never
    /// reused, so a plan never outlives its face's tables.
    static SHAPE_PLANS: RefCell<
        HashMap<(u32, u32, Option<String>, (bool, bool, bool)), std::rc::Rc<rustybuzz::ShapePlan>>,
    > = RefCell::new(HashMap::new());
}

/// Reuse shape plans across runs. On by default; `W3COS_SHAPE_PLAN_CACHE=0`
/// compiles a plan per run, `=verify` also shapes each plain run with a
/// fresh plan and compares glyphs, clusters and positions.
fn shape_plan_cache_enabled() -> bool {
    #[cfg(test)]
    if let Some(enabled) = SHAPE_PLAN_OVERRIDE.with(std::cell::Cell::get) {
        return enabled;
    }
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED
        .get_or_init(|| std::env::var("W3COS_SHAPE_PLAN_CACHE").map_or(true, |value| value != "0"))
}

fn shape_plan_cache_verify() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("W3COS_SHAPE_PLAN_CACHE").is_ok_and(|value| value == "verify")
    })
}

/// One shaped glyph in font units, as the shaper returned it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct RawGlyph {
    id: u32,
    cluster: u32,
    x_advance: i32,
    x_offset: i32,
    y_offset: i32,
    unsafe_to_break: bool,
}

fn raw_glyphs(shaped: &rustybuzz::GlyphBuffer) -> Vec<RawGlyph> {
    shaped
        .glyph_infos()
        .iter()
        .zip(shaped.glyph_positions())
        .map(|(info, position)| RawGlyph {
            id: info.glyph_id,
            cluster: info.cluster,
            x_advance: position.x_advance,
            x_offset: position.x_offset,
            y_offset: position.y_offset,
            unsafe_to_break: info.unsafe_to_break(),
        })
        .collect()
}

/// `W3COS_SHAPE_REUSE=1|verify` (default off): Blink's ReusingTextShaper.
/// When a paragraph's text changes after an unchanged prefix (a streamed
/// message growing), the glyphs of the previous shaping up to a
/// safe-to-break cluster after a space, well inside the unchanged prefix,
/// are kept and only the rest is shaped, with the kept text as pre-context.
/// ASCII text only (no joining scripts or reordering). `verify` also shapes
/// the whole text and compares the glyphs.
fn shape_reuse_mode() -> u8 {
    #[cfg(test)]
    if let Some(mode) = SHAPE_REUSE_OVERRIDE.with(std::cell::Cell::get) {
        return mode;
    }
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| match std::env::var("W3COS_SHAPE_REUSE").as_deref() {
        Ok("1") => 1,
        Ok("verify") => 2,
        _ => 0,
    })
}

type RawShapeKey = (u32, u32, (bool, bool, bool));

/// Recent paragraph shapings (Blink keeps the previous ShapeResults on the
/// inline items; here the last few long runs).
const SHAPE_REUSE_KEEP: usize = 16;
/// Shorter runs are words and labels: reshaping them is cheap.
const SHAPE_REUSE_MIN_BYTES: usize = 64;
/// Unchanged text kept after the cut, so the shaper's context at the cut
/// is the same as in the previous shaping.
const SHAPE_REUSE_MARGIN: usize = 16;

thread_local! {
    static SHAPE_REUSE: RefCell<std::collections::VecDeque<(RawShapeKey, Box<str>, std::rc::Rc<[RawGlyph]>)>> =
        RefCell::new(std::collections::VecDeque::new());
    #[cfg(test)]
    pub(crate) static SHAPE_REUSE_OVERRIDE: std::cell::Cell<Option<u8>> = const { std::cell::Cell::new(None) };
    pub(crate) static SHAPE_REUSE_HITS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    pub(crate) static SHAPE_REUSE_MISMATCHES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The kept glyph count and byte cut for reshaping `text` from `old`.
fn shape_reuse_cut(old_text: &str, old: &[RawGlyph], text: &str) -> Option<(usize, usize)> {
    let common = old_text
        .bytes()
        .zip(text.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    if common == text.len() && common == old_text.len() {
        return None;
    }
    let limit = common.checked_sub(SHAPE_REUSE_MARGIN)?;
    let bytes = text.as_bytes();
    // Last glyph that starts a safe-to-break cluster right after a space.
    let glyph = (1..old.len()).rev().find(|&index| {
        let glyph = &old[index];
        let at = glyph.cluster as usize;
        at > 0
            && at <= limit
            && !glyph.unsafe_to_break
            && old[index - 1].cluster != glyph.cluster
            && bytes[at - 1] == b' '
    })?;
    let cut = old[glyph].cluster as usize;
    // LTR ASCII: every kept glyph's cluster is before the cut.
    old[..glyph]
        .iter()
        .all(|kept| (kept.cluster as usize) < cut)
        .then_some((glyph, cut))
}

#[allow(clippy::too_many_arguments)]
fn shape_raw_reusing(
    text: &str,
    face_id: u32,
    shaping_face: &rustybuzz::Face<'_>,
    features: &[rustybuzz::Feature],
    features_key: (bool, bool, bool),
    mut buffer: rustybuzz::UnicodeBuffer,
    verify_text: Option<&str>,
    plain: bool,
) -> Vec<RawGlyph> {
    let mode = shape_reuse_mode();
    if mode == 0 || !plain || text.len() < SHAPE_REUSE_MIN_BYTES || !text.is_ascii() {
        return raw_glyphs(&shape_with_cached_plan(
            face_id,
            shaping_face,
            features,
            features_key,
            buffer,
            verify_text,
        ));
    }
    buffer.guess_segment_properties();
    let script = buffer.script();
    let language = buffer.language();
    let key: RawShapeKey = (
        face_id,
        u32::from_be_bytes(script.tag().to_bytes()),
        features_key,
    );
    let previous = SHAPE_REUSE.with(|kept| {
        kept.borrow().iter().find_map(|(other, old_text, old)| {
            (*other == key)
                .then(|| shape_reuse_cut(old_text, old, text).map(|cut| (old.clone(), cut)))
                .flatten()
        })
    });
    let reused = previous.map(|(old, (glyph, cut))| {
        let mut rest = rustybuzz::UnicodeBuffer::new();
        rest.push_str(&text[cut..]);
        rest.set_direction(rustybuzz::Direction::LeftToRight);
        rest.set_script(script);
        if let Some(language) = language.clone() {
            rest.set_language(language);
        }
        rest.set_pre_context(&text[..cut]);
        let tail =
            shape_with_cached_plan(face_id, shaping_face, features, features_key, rest, None);
        let mut glyphs = old[..glyph].to_vec();
        glyphs.extend(raw_glyphs(&tail).into_iter().map(|mut glyph| {
            glyph.cluster += cut as u32;
            glyph
        }));
        glyphs
    });
    let glyphs = match reused {
        Some(glyphs) => {
            SHAPE_REUSE_HITS.with(|n| n.set(n.get() + 1));
            if mode == 2 {
                let whole = raw_glyphs(&shape_with_cached_plan(
                    face_id,
                    shaping_face,
                    features,
                    features_key,
                    buffer,
                    verify_text,
                ));
                let same = whole.len() == glyphs.len()
                    && whole.iter().zip(&glyphs).all(|(a, b)| {
                        (a.id, a.cluster, a.x_advance, a.x_offset, a.y_offset)
                            == (b.id, b.cluster, b.x_advance, b.x_offset, b.y_offset)
                    });
                if !same {
                    SHAPE_REUSE_MISMATCHES.with(|n| n.set(n.get() + 1));
                    eprintln!(
                        "[shape-reuse-verify] mismatch: {} reused vs {} shaped glyphs for {} bytes",
                        glyphs.len(),
                        whole.len(),
                        text.len()
                    );
                }
                whole
            } else {
                glyphs
            }
        }
        None => raw_glyphs(&shape_with_cached_plan(
            face_id,
            shaping_face,
            features,
            features_key,
            buffer,
            verify_text,
        )),
    };
    SHAPE_REUSE.with(|kept| {
        let mut kept = kept.borrow_mut();
        kept.push_front((key, text.into(), glyphs.as_slice().into()));
        kept.truncate(SHAPE_REUSE_KEEP);
    });
    glyphs
}

fn shape_with_cached_plan(
    face_id: u32,
    shaping_face: &rustybuzz::Face<'_>,
    features: &[rustybuzz::Feature],
    features_key: (bool, bool, bool),
    mut buffer: rustybuzz::UnicodeBuffer,
    verify_text: Option<&str>,
) -> rustybuzz::GlyphBuffer {
    if !shape_plan_cache_enabled() {
        return rustybuzz::shape(shaping_face, features, buffer);
    }
    buffer.guess_segment_properties();
    let script = buffer.script();
    // `shape` plans with the buffer's optional script; an unknown one
    // keeps the uncached path so the plan sees exactly the same input.
    if script == rustybuzz::script::UNKNOWN
        || buffer.direction() != rustybuzz::Direction::LeftToRight
    {
        return rustybuzz::shape(shaping_face, features, buffer);
    }
    let language = buffer.language();
    let key = (
        face_id,
        u32::from_be_bytes(script.tag().to_bytes()),
        language
            .as_ref()
            .map(|language| language.as_str().to_string()),
        features_key,
    );
    let plan = SHAPE_PLANS.with(|plans| {
        let mut plans = plans.borrow_mut();
        if let Some(plan) = plans.get(&key) {
            return plan.clone();
        }
        if plans.len() >= 256 {
            plans.clear();
        }
        let plan = std::rc::Rc::new(rustybuzz::ShapePlan::new(
            shaping_face,
            rustybuzz::Direction::LeftToRight,
            Some(script),
            language.as_ref(),
            features,
        ));
        plans.insert(key, plan.clone());
        plan
    });
    let length = buffer.len();
    let shaped = rustybuzz::shape_with_plan(shaping_face, &plan, buffer);
    if let Some(text) = verify_text {
        let mut fresh = rustybuzz::UnicodeBuffer::new();
        fresh.push_str(text);
        fresh.guess_segment_properties();
        fresh.set_direction(rustybuzz::Direction::LeftToRight);
        if fresh.len() == length && fresh.script() == script && fresh.language() == language {
            let expected = rustybuzz::shape(shaping_face, features, fresh);
            let same = expected.len() == shaped.len()
                && expected
                    .glyph_infos()
                    .iter()
                    .zip(shaped.glyph_infos())
                    .all(|(a, b)| a.glyph_id == b.glyph_id && a.cluster == b.cluster)
                && expected
                    .glyph_positions()
                    .iter()
                    .zip(shaped.glyph_positions())
                    .all(|(a, b)| {
                        (a.x_advance, a.y_advance, a.x_offset, a.y_offset)
                            == (b.x_advance, b.y_advance, b.x_offset, b.y_offset)
                    });
            if !same {
                eprintln!("[shape-plan-verify] mismatch for {text:?}");
            }
            thread_local! {
                static CHECKED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
            }
            let checked = CHECKED.with(|checked| {
                checked.set(checked.get() + 1);
                checked.get()
            });
            if checked.is_power_of_two() {
                eprintln!("[shape-plan-verify] checked {checked} runs");
            }
        }
    }
    shaped
}
