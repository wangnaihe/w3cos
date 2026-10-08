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
        font.set_subpixel(true).set_linear_metrics(true)
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
    font.set_embolden(style.font_weight >= 600
        && face.font_style().weight() < skia_safe::font_style::Weight::SEMI_BOLD);
    // Blink's synthetic italic uses a quarter-em shear. Do not synthesize
    // again when font matching already returned an italic/oblique face.
    font.set_skew_x(if style.font_style != w3cos_std::style::FontStyle::Normal
        && face.font_style().slant() == skia_safe::font_style::Slant::Upright
    { -0.25 } else { 0.0 });
    font
}

thread_local! {
    static FONT_TABLES: RefCell<HashMap<u32, Arc<Vec<u8>>>> = RefCell::new(HashMap::new());
    static OUTLINE_BOUNDS: RefCell<HashMap<(u32, u32, u32, u32, bool, GlyphId), Option<Rect>>> =
        RefCell::new(HashMap::new());
}

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
            if let Some(bounds) = geometric_ink_bounds(font, &self.glyphs[range.clone()], &self.positions[range]) {
                ink.join(bounds);
            } else { known = false; }
        });
        known.then_some(ink)
    }

    pub fn ink_bounds(&self, font: &Font) -> Option<Rect> {
        let mut ink = None::<Rect>;
        self.visit_fonts(font, |font, range| {
        let mut bounds = vec![Rect::default(); range.len()];
        font.get_bounds(&self.glyphs[range.clone()], &mut bounds, None);
        for (bounds, position) in bounds.into_iter().zip(&self.positions[range]) {
            if bounds.is_empty() { continue; }
            let shifted = Rect::new(bounds.left + position.x, bounds.top + position.y,
                bounds.right + position.x, bounds.bottom + position.y);
            if let Some(ink) = &mut ink { ink.join(shifted); } else { ink = Some(shifted); }
        }
        });
        ink
    }

    fn visit_fonts(&self, font: &Font, mut visit: impl FnMut(&Font, std::ops::Range<usize>)) {
        visit_glyph_fonts(font, &self.glyph_font_sizes, self.glyphs.len(), &mut visit);
    }

    pub(crate) fn draw(&self, canvas: &Canvas, origin: (f32, f32), font: &Font, paint: &Paint) {
        self.visit_fonts(font, |font, range|
            canvas.draw_glyphs_at(&self.glyphs[range.clone()], &self.positions[range], origin, font, paint));
    }

    pub(crate) fn intercepts(&self, font: &Font, upper: f32, lower: f32) -> Vec<f32> {
        let mut result = Vec::new();
        self.visit_fonts(font, |font, range| result.extend(font.get_intercepts(
            &self.glyphs[range.clone()], &self.positions[range], (upper, lower), None)));
        result
    }
}

pub(crate) fn visit_glyph_fonts(font: &Font, sizes: &[f32], count: usize,
    mut visit: impl FnMut(&Font, std::ops::Range<usize>)) {
    if sizes.is_empty() { visit(font, 0..count); return; }
    debug_assert_eq!(sizes.len(), count);
    let mut start = 0;
    while start < count {
        let size = sizes[start];
        let end = sizes[start..].iter().position(|next| *next != size)
            .map_or(count, |offset| start + offset);
        let mut used = font.clone();
        used.set_size(effective_font_size(size));
        visit(&used, start..end);
        start = end;
    }
}

pub(crate) fn geometric_ink_bounds_with_sizes(font: &Font, glyphs: &[GlyphId], positions: &[Point],
    sizes: &[f32]) -> Option<Rect> {
    let mut ink = Rect::default();
    let mut known = true;
    visit_glyph_fonts(font, sizes, glyphs.len(), |font, range| {
        if let Some(bounds) = geometric_ink_bounds(font, &glyphs[range.clone()], &positions[range]) {
            ink.join(bounds);
        } else { known = false; }
    });
    known.then_some(ink)
}

/// Borrowed glyph slices share outline culling with shaped font runs.
pub(crate) fn geometric_ink_bounds(font: &Font, glyphs: &[GlyphId], positions: &[Point]) -> Option<Rect> {
        let face = font.typeface().unique_id();
        let mut ink = Rect::default();
        for (&glyph, position) in glyphs.iter().zip(positions) {
            let key = (face, font.size().to_bits(), font.scale_x().to_bits(),
                font.skew_x().to_bits(), font.is_embolden(), glyph);
            let bounds = OUTLINE_BOUNDS.with(|cache| {
                if let Some(bounds) = cache.borrow().get(&key) { return *bounds; }
                let bounds = font.get_path(glyph).map(|path| *path.bounds()).or_else(|| {
                    let mut raster = [Rect::default()];
                    font.get_bounds(&[glyph], &mut raster, None);
                    raster[0].is_empty().then_some(Rect::default())
                });
                let mut cache = cache.borrow_mut();
                if cache.len() >= 2048 { cache.clear(); }
                cache.insert(key, bounds);
                bounds
            })?;
            if !bounds.is_empty() {
                ink.join(Rect::new(bounds.left + position.x, bounds.top + position.y,
                    bounds.right + position.x, bounds.bottom + position.y));
            }
        }
        Some(ink)
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

pub(crate) fn shape_visual_run_with_clusters(text: &str, face: &Typeface, style: &Style) -> Option<GlyphRun> {
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
    shape_visual_run_impl(text, face, style, collect_clusters, Some((expansion, byte_offset)))
}

// Blink's Apple path uses Skia nominal widths unless the font has a tracking
// table without bitmap strikes. Keep Rustybuzz's contextual adjustments, but
// replace its design-unit nominal input with the width of the painted face.
#[cfg(target_os = "macos")]
fn skia_nominal_advances(face: &Typeface, style: &Style, glyphs: &[GlyphId]) -> Option<Vec<f64>> {
    let tags = face.read_table_tags()?;
    if tags.contains(&u32::from_be_bytes(*b"trak"))
        && !tags.contains(&u32::from_be_bytes(*b"sbix")) {
        return None;
    }
    let mut widths = vec![0.0; glyphs.len()];
    css_font(face, style.font_size).get_widths(glyphs, &mut widths);
    Some(widths.into_iter().map(|width|
        f64::from((width * 65536.0) as i32) / 65536.0).collect())
}

fn shape_visual_run_impl(
    text: &str, face: &Typeface, style: &Style, collect_clusters: bool,
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
    shape_buffer(text, face, style, collect_clusters, expansion, buffer, native_caps)
}

/// The shaper uses the default language system. A feature merely present for
/// another script must not suppress synthesis in this run.
fn supports_small_caps(face: &Typeface, script: rustybuzz::Script) -> bool {
    let Some(bytes) = font_tables(face) else { return false; };
    let Some(face) = rustybuzz::Face::from_slice(&bytes, 0) else { return false; };
    let Some(table) = face.tables().gsub else { return false; };
    let tag_bytes = script.tag().0.to_be_bytes().map(|byte| byte.to_ascii_lowercase());
    let tag = rustybuzz::ttf_parser::Tag::from_bytes(&tag_bytes);
    let script = table.scripts.find(tag)
        .or_else(|| table.scripts.find(rustybuzz::ttf_parser::Tag::from_bytes(b"DFLT")));
    script.and_then(|script| script.default_language).is_some_and(|language|
        language.feature_indices.into_iter().chain(language.required_feature)
            .filter_map(|index| table.features.get(index))
            .any(|feature| feature.tag == rustybuzz::ttf_parser::Tag::from_bytes(b"smcp")))
}

pub(crate) fn synthesized_small_caps_size(size: f32) -> f32 {
    // Chromium141 SimpleFontData::CreateScaledFontData; this is a glyph font,
    // not an author style mutation (V2059 Ahem96 ->67px; Times96 ->67px).
    (size * 0.7).round()
}

fn shape_synthetic_small_caps(
    text: &str, face: &Typeface, style: &Style, collect_clusters: bool,
    expansion: Option<(crate::text_layout::JustificationExpansion, usize)>,
) -> Option<GlyphRun> {
    let language = style.custom_properties.as_ref()
        .and_then(|properties| properties.get("--w3cos-internal-text-language"))
        .map(String::as_str);
    let mut segments = Vec::<(usize, bool)>::new();
    let mut behavior = false;
    for (index, character) in text.char_indices() {
        let source = &text[index..index + character.len_utf8()];
        let uppercase = w3cos_std::style::transformed_text(source,
            w3cos_std::style::TextTransform::Uppercase, language, false);
        let combining = unicode_ccc::get_canonical_combining_class(character)
            != unicode_ccc::CanonicalCombiningClass::NotReordered;
        let next = if combining { behavior } else { uppercase.as_ref() != source };
        if segments.is_empty() || behavior != next { segments.push((index, next)); }
        behavior = next;
    }
    let mut output = GlyphRun { glyphs: Vec::new(), positions: Vec::new(), advance: 0.0,
        clusters: Vec::new(), glyph_clusters: Vec::new(), glyph_font_sizes: Vec::new() };
    let mut cursor = 0.0_f64;
    for (segment, &(start, uppercase)) in segments.iter().enumerate() {
        let end = segments.get(segment + 1).map_or(text.len(), |segment| segment.0);
        let source = &text[start..end];
        let mut buffer = rustybuzz::UnicodeBuffer::new();
        for (index, character) in source.char_indices() {
            let value = &source[index..index + character.len_utf8()];
            let value = if uppercase {
                w3cos_std::style::transformed_text(value, w3cos_std::style::TextTransform::Uppercase, language, false)
            } else { std::borrow::Cow::Borrowed(value) };
            // Uppercase expansions share the original UTF-8 source cluster;
            // CSS spacing and justification apply once, not per expansion.
            for character in value.chars() { buffer.add(character, u32::try_from(index).ok()?); }
        }
        buffer.guess_segment_properties();
        let mut used = style.clone();
        used.font_variant = w3cos_std::style::FontVariant::Normal;
        if uppercase { used.font_size = synthesized_small_caps_size(style.font_size); }
        let run = shape_buffer(source, face, &used, collect_clusters,
            expansion.map(|(expansion, offset)| (expansion, offset + start)), buffer, false)?;
        output.positions.extend(run.positions.iter().map(|position|
            Point::new((cursor + f64::from(position.x)) as f32, position.y)));
        output.glyph_font_sizes.extend(std::iter::repeat_n(used.font_size, run.glyphs.len()));
        output.glyphs.extend(run.glyphs);
        output.glyph_clusters.extend(run.glyph_clusters.into_iter().map(|cluster| start + cluster));
        output.clusters.extend(run.clusters.into_iter().map(|(cluster, advance)| (start + cluster, advance)));
        cursor += f64::from(run.advance);
    }
    output.advance = cursor as f32;
    Some(output)
}

fn shape_buffer(
    text: &str, face: &Typeface, style: &Style, collect_clusters: bool,
    expansion: Option<(crate::text_layout::JustificationExpansion, usize)>,
    mut buffer: rustybuzz::UnicodeBuffer, native_caps: bool,
) -> Option<GlyphRun> {
    let bytes = font_tables(face)?;
    let shaping_face = rustybuzz::Face::from_slice(&bytes, 0)?;
    // Bidi ordering and mirrored characters have already been resolved by
    // font_render_text_for_style. Do not reorder this visual run a second time.
    buffer.set_direction(rustybuzz::Direction::LeftToRight);
    let mut features: Vec<rustybuzz::Feature> = Vec::new();
    if native_caps { features.push("smcp=1".parse().ok()?); }
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
    let shaped = rustybuzz::shape(&shaping_face, &features, buffer);
    // Do not derive a cluster's width by subtracting accumulated f32 cursors:
    // its precision would depend on the unrelated text before the cluster.
    // Keep accumulation precise; Skia's point/advance boundary remains f32.
    let scale = f64::from(effective_font_size(style.font_size)) / f64::from(shaping_face.units_per_em());
    let glyphs = shaped.glyph_infos().iter()
        .map(|info| u16::try_from(info.glyph_id).ok()).collect::<Option<Vec<_>>>()?;
    #[cfg(target_os = "macos")]
    let nominal_advances = skia_nominal_advances(face, style, &glyphs);
    let mut positions = Vec::with_capacity(shaped.len());
    let mut cursor = 0.0;
    let mut clusters = Vec::new();
    let mut glyph_clusters = Vec::new();
    let mut cluster_start_advance = 0.0;
    for (i, (info, position)) in shaped
        .glyph_infos()
        .iter()
        .zip(shaped.glyph_positions())
        .enumerate()
    {
        if collect_clusters { glyph_clusters.push(info.cluster as usize); }
        positions.push(Point::new(
            (cursor + f64::from(position.x_offset) * scale) as f32,
            (-f64::from(position.y_offset) * scale) as f32,
        ));
        let advance = f64::from(position.x_advance) * scale;
        #[cfg(target_os = "macos")]
        let advance = if position.x_advance != 0 {
            nominal_advances.as_ref().and_then(|advances| {
                let nominal = shaping_face.glyph_hor_advance(rustybuzz::ttf_parser::GlyphId(glyphs[i]))?;
                Some(advances[i] + f64::from(position.x_advance - i32::from(nominal)) * scale)
            }).unwrap_or(advance)
        } else { advance };
        // Apple inline shaping consumes 16.16 advances, including contextual
        // adjustments. Retaining design-unit fractions until the completed
        // run is snapped can move a font boundary by a whole CSS LayoutUnit.
        // Justification uses the same precision on the other backends too.
        let advance = if cfg!(target_os = "macos") || expansion.is_some() {
            (advance * 65536.0).trunc() / 65536.0
        } else { advance };
        cursor += advance;
        let cluster_ends = shaped
            .glyph_infos()
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
                && text.get(info.cluster as usize..).is_some_and(|text| text.starts_with(' '))
            {
                cursor += expansion.at_space(byte_offset + info.cluster as usize);
            }
            if collect_clusters {
                clusters.push((info.cluster as usize, (cursor - cluster_start_advance) as f32));
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

#[cfg(all(test, target_os = "macos"))]
mod precision_tests {
    use super::*;

    #[test]
    fn synthetic_small_caps_keep_css_size_and_original_source_clusters() {
        use w3cos_std::style::FontVariant;
        let face = skia_safe::FontMgr::default()
            .match_family_style("Times", skia_safe::FontStyle::normal()).unwrap();
        let style = Style { font_size: 96.0, font_variant: FontVariant::SmallCaps, ..Style::default() };
        let run = shape_visual_run_with_clusters("eE", &face, &style).unwrap();
        assert_eq!(style.font_size, 96.0, "glyph synthesis must not rewrite CSS size");
        assert_eq!(run.glyph_font_sizes, vec![67.0, 96.0]);
        assert_eq!(run.glyphs[0], run.glyphs[1], "lowercase e synthesizes uppercase E");
        assert_eq!(run.glyph_clusters, vec![0, 1]);
        // DEFAULT Chromium141 original font-003 span Range, V2059.
        assert_eq!(crate::text_layout::inline_layout_advance(run.clusters[0].1), 40.9375);
        let font = css_font_for_style(&face, style.font_size, &style);
        let first = geometric_ink_bounds_with_sizes(&font, &run.glyphs[..1], &run.positions[..1],
            &run.glyph_font_sizes[..1]).unwrap();
        let small = css_font_for_style(&face, 67.0, &style);
        assert_eq!(first, geometric_ink_bounds(&small, &run.glyphs[..1], &run.positions[..1]).unwrap());
    }

    #[test]
    fn synthetic_small_caps_expansions_and_combining_marks_keep_source_spacing() {
        use w3cos_std::style::FontVariant;
        let face = skia_safe::FontMgr::default()
            .match_family_style("Times", skia_safe::FontStyle::normal()).unwrap();
        let style = Style { font_size: 16.0, font_variant: FontVariant::SmallCaps, ..Style::default() };
        let plain = shape_visual_run_with_clusters("ßA", &face, &style).unwrap();
        let spaced = shape_visual_run_with_clusters("ßA", &face,
            &Style { letter_spacing: 2.0, ..style.clone() }).unwrap();
        assert_eq!(spaced.glyph_clusters, vec![0, 0, 2]);
        assert_eq!(spaced.advance - plain.advance, 4.0,
            "one spacing per original source character, not per uppercase expansion");
        let marked = shape_visual_run_with_clusters("e\u{0301}E", &face, &style).unwrap();
        assert_eq!(*marked.glyph_clusters.last().unwrap(), 3);
        assert_eq!(*marked.glyph_font_sizes.last().unwrap(), 16.0);
        assert!(marked.glyph_font_sizes[..marked.glyph_font_sizes.len()-1].iter().all(|size| *size == 11.0));
    }


    #[test]
    fn browser_justified_pingfang_character_origin_uses_layout_range() {
        let face = skia_safe::FontMgr::default()
            .match_family_style("PingFang SC", skia_safe::FontStyle::normal()).unwrap();
        let text = "decreased): its sides should be approximately equidistant from the";
        let style = Style { font_size: 16.0, ..Style::default() };
        let raw = shape_visual_run(text, &face, &style).unwrap();
        let expansion = crate::text_layout::justification_expansion(text, 544.0, raw.advance).unwrap();
        let run = shape_visual_run_with_expansion(text, &face, &style, true, expansion, 0).unwrap();
        let cluster = text.find("equidistant").unwrap() + 8;
        let index = run.glyph_clusters.iter().position(|value| *value == cluster).unwrap();
        // DEFAULT Chromium 141 DOM Range start for this glyph, V711.
        // The range coordinate floors the precise origin to 1/64 CSS px.
        let origin = 128.0 + run.positions[index].x;
        assert_eq!((origin * 64.0).floor() / 64.0, 567.609375,
            "original WPT line origin before rasterization");
    }

    #[test]
    fn justified_full_line_preserves_contextual_glyphs_and_space_advances() {
        let face = skia_safe::FontMgr::default()
            .match_family_style("Times", skia_safe::FontStyle::normal()).unwrap();
        let text = "Some words justified";
        let style = Style { font_size: 20.0, ..Style::default() };
        let raw = shape_visual_run(text, &face, &style).unwrap();
        let available = crate::text_layout::inline_layout_advance(raw.advance) + 20.0;
        let expansion = crate::text_layout::justification_expansion(text, available, raw.advance).unwrap();
        let justified = shape_visual_run_with_expansion(text, &face, &style, true, expansion, 0).unwrap();
        let reference = shape_visual_run_with_clusters(text, &face,
            &Style { word_spacing: 10.0, ..style }).unwrap();
        assert_eq!(justified.glyphs, raw.glyphs);
        assert_eq!(justified.glyph_clusters, reference.glyph_clusters);
        assert_eq!(justified.positions, reference.positions);
        assert_eq!(justified.advance, reference.advance);
    }

    #[test]
    fn effective_platform_font_size_preserves_computed_css_size() {
        let face = skia_safe::FontMgr::default()
            .match_family_style("Courier", skia_safe::FontStyle::normal()).unwrap();
        for (computed, effective) in [(13.333333_f32, 13.33_f32), (19.219, 19.21),
            (16.0, 16.0), (0.009, 0.0), (0.0, 0.0)] {
            let style = Style { font_size: computed, ..Style::default() };
            assert_eq!(css_font_for_style(&face, computed, &style).size(), effective);
            assert_eq!(style.font_size, computed, "computed/inherited size is not quantized");
        }
        let style = Style { font_family: Some("monospace".into()),
            font_size: (10.0_f64 * 96.0 / 72.0) as f32, ..Style::default() };
        let text = " The   spacing  on      these         two  sentences need to be the    same!  ";
        let run = shape_visual_run(text, &face, &style).unwrap();
        // Chromium141 Canvas measurement for the original WPT sentence.
        assert_eq!(run.advance, 623.945068359375_f32);
    }

    #[test]
    fn synthetic_italic_only_shears_upright_faces_and_preserves_advance() {
        let manager = skia_safe::FontMgr::default();
        for face_style in [skia_safe::FontStyle::normal(), skia_safe::FontStyle::italic()] {
            let face = manager.match_family_style("Times", face_style).unwrap();
            assert_eq!(face.font_style().slant(), face_style.slant());
            for requested in [w3cos_std::style::FontStyle::Normal,
                w3cos_std::style::FontStyle::Italic, w3cos_std::style::FontStyle::Oblique] {
                let style = Style { font_style: requested, font_size: 16.0, ..Style::default() };
                let font = css_font_for_style(&face, 16.0, &style);
                let synthetic = requested != w3cos_std::style::FontStyle::Normal
                    && face_style.slant() == skia_safe::font_style::Slant::Upright;
                assert_eq!(font.skew_x(), if synthetic { -0.25 } else { 0.0 });
                let glyphs = font.str_to_glyphs_vec("including");
                let mut actual = vec![0.0; glyphs.len()];
                let mut regular = actual.clone();
                font.get_widths(&glyphs, &mut actual);
                css_font(&face, 16.0).get_widths(&glyphs, &mut regular);
                assert_eq!(actual, regular, "synthetic ink must not change text advance");
                if synthetic {
                    let positions = vec![Point::new(0.0, 0.0); glyphs.len()];
                    assert_ne!(geometric_ink_bounds(&font, &glyphs, &positions),
                        geometric_ink_bounds(&css_font(&face, 16.0), &glyphs, &positions),
                        "culling bounds must use the same synthetic ink as painting");
                }
            }
        }
    }

    #[test]
    fn macos_untracked_font_advance_matches_skia_fixed_point_callback() {
        let face = skia_safe::FontMgr::default()
            .match_family_style("Times", skia_safe::FontStyle::normal()).unwrap();
        let tags = face.read_table_tags().unwrap();
        assert!(!tags.contains(&u32::from_be_bytes(*b"trak")));
        let style = Style { font_family: Some("serif".into()), font_size: 19.2,
            font_kerning: false, ..Style::default() };
        let run = shape_visual_run_with_clusters("n", &face, &style).unwrap();
        let mut widths = vec![0.0; run.glyphs.len()];
        css_font_for_style(&face, style.font_size, &style).get_widths(&run.glyphs, &mut widths);
        // Chromium 141's SkiaScalarToHarfBuzzPosition uses a truncating
        // ClampTo<int> conversion to 16.16 after SkFont glyph measurement.
        let expected = (widths[0] * 65536.0) as i32 as f32 / 65536.0;
        eprintln!("Times n: skia={} callback={} native={}", widths[0], expected, run.advance);
        assert_eq!(run.advance, expected,
            "a font without trak must use the same nominal advance source as Skia paint");
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
        eprintln!("AUTHORED_GLYPH_DIAGNOSTIC {}", serde_json::to_string(&diagnostic).unwrap());
    }

    #[test]
    fn fractional_cluster_advance_does_not_depend_on_prefix_length() {
        let face = skia_safe::FontMgr::default()
            .match_family_style("Times", skia_safe::FontStyle::normal()).unwrap();
        let style = Style { font_family: Some("serif".into()), font_size: 19.2,
            ..Style::default() };
        let prefix = "This line is all ";
        let short = shape_visual_run_with_clusters("font", &face, &style).unwrap();
        let long = shape_visual_run_with_clusters(&format!("{prefix}font"), &face, &style).unwrap();
        let advance = |run: &GlyphRun, byte| run.clusters.iter()
            .find(|(start, _)| *start == byte).unwrap().1;
        assert_eq!(advance(&long, prefix.len() + 2), advance(&short, 2),
            "the same n/t shaping context must not inherit accumulated cursor rounding");
    }
}
