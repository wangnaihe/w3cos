//! Skia replay backend for the retained W3COS paint artifact.
//!
//! This module intentionally consumes the same pre-painted node stream as the
//! Vello and tiny-skia backends. It does not perform layout or invent native
//! widget defaults: CSS-derived geometry and style remain the source of truth.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use skia_safe::canvas::SaveLayerRec;
use skia_safe::{
    AlphaType, BlurStyle, Canvas, Color, Color4f, ColorType, Data, FontMgr, FontStyle, Image,
    ImageFilter, ImageInfo, MaskFilter, Matrix, Paint, PathBuilder, Picture, PictureRecorder,
    RRect, Rect, Surface, TileMode, Typeface, Vector, color_filters, gradient_shader,
    image_filters, images, paint,
};
use w3cos_std::SvgPathCommand;
use w3cos_std::component::ComponentKind;
use w3cos_std::style::{Display, JustifyContent, Style, TextAlign, Transform2D};

use crate::filter::{FilterChain, FilterOp, parse_css_filter};
use crate::layout::LayoutRect;
use crate::paint_artifact::PaintArtifact;
use crate::retained_layers::{
    CompositorOverrides, LayerPaintAction, RetainedLayerTree, layer_css_transform,
    layer_opacity as compositor_layer_opacity, layer_scroll_translation,
};
use crate::text_layout;
#[path = "text_decoration.rs"]
mod text_decoration;
#[path = "analytic_ellipse.rs"]
mod analytic_ellipse;
#[path = "analytic_round_rect.rs"]
mod analytic_round_rect;
#[path = "browser_blur.rs"]
mod browser_blur;
#[path = "browser_dither.rs"]
mod browser_dither;
#[path = "media_controls_paint.rs"]
mod media_controls_paint;
#[cfg(target_os = "macos")]
#[path = "browser_font_defaults.rs"]
mod browser_font_defaults;
#[cfg(all(test, target_os = "macos"))]
#[path = "metal_blur_tests.rs"]
mod metal_blur_tests;
#[cfg(test)]
use skia_safe::Font;

const FONT_FALLBACK_CACHE_CAPACITY: usize = 2048;

const IMAGE_TEXTURE_CACHE_LIMIT: usize = 256;

thread_local! {
    static SKIA_IMAGES: RefCell<HashMap<usize, Image>> = RefCell::new(HashMap::new());
    /// System font matching is comparatively expensive on Apple platforms.
    /// Cache Skia typeface references for characters missing from the primary
    /// face; this does not copy the underlying system font into application memory.
    static FONT_FALLBACK_CACHE: RefCell<HashMap<(u32, char, u16), Option<Typeface>>> =
        RefCell::new(HashMap::new());
    static GENERIC_SLANTED_TYPEFACES: RefCell<HashMap<(u32, u8), Option<Typeface>>> =
        RefCell::new(HashMap::new());
    static INTRINSIC_PRIMARY_TYPEFACE: Typeface = {
        #[cfg(test)]
        {
            primary_typeface(include_bytes!("../assets/Inter-Regular.ttf"))
                .expect("Skia test font")
        }
        #[cfg(not(test))]
        {
            host_typeface()
                .or_else(|| primary_typeface(include_bytes!("../assets/Inter-Regular.ttf")))
                .expect("embedded Skia fallback font")
        }
    };
    static GENERIC_SERIF_TYPEFACE: Option<Typeface> = {
        let manager = FontMgr::default();
        // CoreText's generic alias can resolve to Times New Roman, whereas
        // the macOS browser default serif face is the system Times family.
        #[cfg(target_os = "macos")]
        let platform_default = manager.match_family_style("Times", FontStyle::normal());
        #[cfg(not(target_os = "macos"))]
        let platform_default = None;
        platform_default
            .or_else(|| manager.match_family_style("serif", FontStyle::normal()))
            .or_else(|| manager.match_family_style("Times New Roman", FontStyle::normal()))
    };
    static HTML_STANDARD_TYPEFACE: Option<Typeface> = host_typeface();
    #[cfg(target_os = "macos")]
    static LANGUAGE_GENERIC_TYPEFACES: RefCell<HashMap<&'static str, Option<Typeface>>> =
        RefCell::new(HashMap::new());
    static GENERIC_MONOSPACE_TYPEFACE: Option<Typeface> = {
        let manager = FontMgr::default();
        #[cfg(target_os = "macos")]
        let platform_default = manager.match_family_style("Courier", FontStyle::normal());
        #[cfg(not(target_os = "macos"))]
        let platform_default = None;
        platform_default.or_else(|| manager.match_family_style("monospace", FontStyle::normal()))
    };
    static SKIA_IMAGE_UPLOADS: Cell<u64> = const { Cell::new(0) };
    static SKIA_IMAGE_REUSES: Cell<u64> = const { Cell::new(0) };
}

pub(crate) fn skia_image_upload_count() -> u64 {
    SKIA_IMAGE_UPLOADS.with(Cell::get)
}

pub(crate) fn skia_image_reuse_count() -> u64 {
    SKIA_IMAGE_REUSES.with(Cell::get)
}

pub(crate) fn reset_image_texture_stats() {
    SKIA_IMAGE_UPLOADS.with(|count| count.set(0));
    SKIA_IMAGE_REUSES.with(|count| count.set(0));
}

pub(crate) fn clear_image_texture_cache() {
    SKIA_IMAGES.with(|cache| cache.borrow_mut().clear());
}

pub(crate) fn invalidate_image_texture(pixels_id: usize) {
    SKIA_IMAGES.with(|cache| {
        cache.borrow_mut().remove(&pixels_id);
    });
}

fn cached_skia_image(decoded: &crate::image_loader::DecodedImage) -> Option<Image> {
    let pixels_id = decoded.pixels_id();
    SKIA_IMAGES.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(image) = cache.get(&pixels_id) {
            SKIA_IMAGE_REUSES.with(|count| count.set(count.get().saturating_add(1)));
            return Some(image.clone());
        }
        if cache.len() >= IMAGE_TEXTURE_CACHE_LIMIT {
            cache.clear();
        }
        let pixels = decoded.data.as_slice();
        let width = decoded.width;
        let height = decoded.height;
        if width == 0 || height == 0 || pixels.len() != width as usize * height as usize * 4 {
            return None;
        }
        let info = ImageInfo::new(
            (width as i32, height as i32),
            ColorType::RGBA8888,
            AlphaType::Premul,
            None,
        );
        // Browser image textures interpolate premultiplied channels. Keeping
        // straight alpha here selects Skia's float conversion path even for
        // opaque pixels, changing fractional bilinear rounding. Do not mutate
        // the decoded Arc shared with DOM/canvas and other paint backends.
        let data = if pixels.chunks_exact(4).all(|pixel| pixel[3] == 255) {
            Data::new_copy(pixels)
        } else {
            let mut premultiplied = pixels.to_vec();
            for pixel in premultiplied.chunks_exact_mut(4) {
                let alpha = u16::from(pixel[3]);
                for channel in &mut pixel[..3] {
                    *channel = ((u16::from(*channel) * alpha + 127) / 255) as u8;
                }
            }
            Data::new_copy(&premultiplied)
        };
        let image = images::raster_from_data(&info, data, width as usize * 4)?;
        SKIA_IMAGE_UPLOADS.with(|count| count.set(count.get().saturating_add(1)));
        cache.insert(pixels_id, image.clone());
        Some(image)
    })
}

fn primary_typeface(font_bytes: &[u8]) -> Option<Typeface> {
    #[cfg(target_os = "ios")]
    {
        let font_manager = FontMgr::default();
        // Use the concrete Latin face behind CSS `-apple-system`. The generic
        // Apple cascade face reports CJK glyphs while retaining its shorter
        // Latin line metrics; Blink instead resolves those glyphs to PingFang
        // and uses that fallback face's full line box.
        if let Some(system) = font_manager.match_family_style("SF Pro Text", FontStyle::normal()) {
            return Some(system);
        }
    }
    FontMgr::default().new_from_data(font_bytes, None)
}

fn host_typeface() -> Option<Typeface> {
    let host = crate::font_face::host_ui_font();
    let manager = FontMgr::default();
    manager.new_from_data(host.data.as_slice(), Some(host.index as usize))
        // Some platform font collections cannot be instantiated from copied
        // bytes by the OS-backed manager. Resolve the already-selected family,
        // never substitute an unrelated bundled/default font for its metrics.
        .or_else(|| manager.match_family_style(&host.family, FontStyle::normal()))
}

fn registered_typeface(style: &Style) -> Option<(crate::font_face::LoadedFont, Typeface)> {
    let loaded = crate::font_face::FontRegistry::global().resolve_style(style)?;
    loaded.parsed()?;
    let typeface = loaded.skia_typeface()?;
    Some((loaded, typeface))
}

/// The registered face for `style`, but only when it covers every character of
/// `text`.
///
/// `css_font_runs` already prefers a registered family per character, so the
/// registered face may only act as the fallback base for text it can paint
/// itself. A stack such as `"Ahem", "Times New Roman"` must not measure Times
/// characters with Ahem's metrics.
fn registered_typeface_covering(
    style: &Style,
    text: &str,
) -> Option<(crate::font_face::LoadedFont, Typeface)> {
    let (font, typeface) = registered_typeface(style)?;
    let render_text = text_layout::font_render_text_for_style(text, style);
    font_covers_text(&font, render_text.as_ref()).then_some((font, typeface))
}

fn generic_serif_typeface(style: &Style) -> Option<Typeface> {
    let base = generic_serif_base_typeface(style)?;
    let slant = match style.font_style {
        w3cos_std::style::FontStyle::Normal => return Some(base),
        w3cos_std::style::FontStyle::Italic => skia_safe::font_style::Slant::Italic,
        w3cos_std::style::FontStyle::Oblique => skia_safe::font_style::Slant::Oblique,
    };
    // An installed italic has different outlines and advances from a sheared
    // upright face. Share this selection across shaping, painting and metrics;
    // css_font_for_style synthesizes only if the family has no slanted face.
    let key = (base.unique_id(), slant as u8);
    GENERIC_SLANTED_TYPEFACES.with(|cache| {
        if let Some(face) = cache.borrow().get(&key) { return face.clone().or(Some(base)); }
        let face = FontMgr::default().match_family_style(&base.family_name(),
            FontStyle::new(base.font_style().weight(), skia_safe::font_style::Width::NORMAL, slant));
        let mut cache = cache.borrow_mut();
        if cache.len() >= 256 { cache.clear(); }
        cache.insert(key, face.clone());
        face.or(Some(base))
    })
}

fn generic_serif_base_typeface(style: &Style) -> Option<Typeface> {
    let Some(families) = style.font_family.as_deref() else {
        return style.custom_properties.as_ref().is_some_and(|properties| {
            properties.get(w3cos_dom::user_agent::HTML_STANDARD_FONT_PROPERTY)
                .is_some_and(|value| value == "1")
        }).then(|| language_generic_typeface(style, "standard")
            .or_else(|| HTML_STANDARD_TYPEFACE.with(Clone::clone))).flatten();
    };
    // A bare generic fixed family resolves to the platform's browser face,
    // not the embedding's primary UI font. All geometry and glyph callers
    // consume this same resolution. Named/registered stacks retain their
    // existing per-character cascade rather than being globally overridden.
    if families.trim().eq_ignore_ascii_case("monospace") {
        return language_generic_typeface(style, "monospace")
            .or_else(|| GENERIC_MONOSPACE_TYPEFACE.with(Clone::clone));
    }
    if families.trim().eq_ignore_ascii_case("sans-serif") {
        // A bare browser sans family uses the host's locale-aware default,
        // already used by the glyph fallback. Resolve it here as well so
        // font struts, negative leading and paint baselines share its metrics.
        return language_generic_typeface(style, "sans-serif")
            .or_else(|| HTML_STANDARD_TYPEFACE.with(Clone::clone));
    }
    let mut names = families
        .split(',')
        .map(|family| family.trim().trim_matches(['"', '\'']));
    let uses_serif = names
        .clone()
        .any(|family| family.eq_ignore_ascii_case("serif"));
    let explicitly_non_serif = names.clone().any(|family| {
        [
            "sans-serif",
            "monospace",
            "system-ui",
            "cursive",
            "fantasy",
            "ui-sans-serif",
            "ui-monospace",
        ]
        .iter()
        .any(|generic| family.eq_ignore_ascii_case(generic))
    });
    // Resolve the whole authored stack before choosing a default. An explicit
    // generic is distinct from an unresolved stack's HTML standard context.
    let unresolved = !uses_serif
        && !explicitly_non_serif
        && crate::font_face::FontRegistry::global()
            .resolve_style(style)
            .is_none()
        && {
            let manager = FontMgr::default();
            names.all(|family| {
                manager
                    .match_family_style(family, FontStyle::normal())
                    .is_none()
            })
        };
    if unresolved && style.custom_properties.as_ref().is_some_and(|properties|
        properties.get(w3cos_dom::user_agent::HTML_STANDARD_FONT_PROPERTY)
            .is_some_and(|value| value == "1"))
    {
        return language_generic_typeface(style, "standard")
            .or_else(|| HTML_STANDARD_TYPEFACE.with(Clone::clone));
    }
    // Native embeddings without an HTML standard context retain their
    // existing default. Never replace an explicit serif or installed family.
    (uses_serif || unresolved)
        .then(|| language_generic_typeface(style, "serif")
            .or_else(|| GENERIC_SERIF_TYPEFACE.with(Clone::clone)))
        .flatten()
}

fn language_generic_typeface(style: &Style, generic: &str) -> Option<Typeface> {
    #[cfg(target_os = "macos")]
    {
        use browser_font_defaults::GenericFamily;
        let generic = match generic {
            "standard" => GenericFamily::Standard,
            "serif" => GenericFamily::Serif,
            "sans-serif" => GenericFamily::Sans,
            "monospace" => GenericFamily::Monospace,
            _ => return None,
        };
        let language = style.custom_properties.as_ref()?
            .get(w3cos_dom::user_agent::TEXT_LANGUAGE_PROPERTY)?;
        let family = browser_font_defaults::family(language, generic)?;
        // The finite platform-default family set bounds this cache. Geometry
        // and glyph callers reuse the same face; no per-character OS lookup.
        LANGUAGE_GENERIC_TYPEFACES.with(|cache| cache.borrow_mut().entry(family)
            .or_insert_with(|| FontMgr::default().match_family_style(family, FontStyle::normal()))
            .clone())
    }
    #[cfg(not(target_os = "macos"))]
    { let _ = (style, generic); None }
}

#[derive(Clone, Copy)]
pub(crate) struct ResolvedFontGeometry {
    pub ascent: f32,
    pub descent: f32,
    pub line_gap: f32,
}

impl ResolvedFontGeometry {
    pub fn height(self) -> f32 { self.ascent + self.descent }
    pub fn line_spacing(self) -> f32 { self.height() + self.line_gap }

    fn normal_line_box(self) -> Self {
        let upper = crate::layout::inline_upper_leading(self.line_spacing(), self.height());
        Self { ascent: self.ascent + upper,
            descent: self.descent + self.line_gap - upper, line_gap: 0.0 }
    }
}

thread_local! {
    static FONT_GEOMETRY_CACHE: RefCell<HashMap<(u32, u32), ResolvedFontGeometry>> =
        RefCell::new(HashMap::new());
}

fn typeface_font_geometry(face: &Typeface, size: f32) -> ResolvedFontGeometry {
    let key = (face.unique_id(), size.to_bits());
    FONT_GEOMETRY_CACHE.with(|cache| {
        if let Some(metrics) = cache.borrow().get(&key).copied() { return metrics; }
        let (_, raw) = crate::skia_text_run::css_font(face, size).metrics();
        let tiny = -raw.ascent < 3.0 || raw.descent - raw.ascent < 2.0;
        let mut ascent = if tiny { -raw.ascent } else { (-raw.ascent).round() };
        let descent = if tiny { raw.descent } else { raw.descent.round() };
        // Match the browser's macOS web-font compatibility metrics, not
        // AppKit's extra line spacing. The adjustment belongs to ascent and
        // must be shared by normal struts, inline font boxes and baselines.
        #[cfg(target_os = "macos")]
        if matches!(face.family_name().as_str(), "Times" | "Helvetica" | "Courier") {
            ascent += ((ascent + descent) * 0.15 + 0.5).floor();
        }
        let metrics = ResolvedFontGeometry { ascent, descent, line_gap: raw.leading.round() };
        let mut cache = cache.borrow_mut();
        if cache.len() >= 256 { cache.clear(); }
        cache.insert(key, metrics);
        metrics
    })
}

/// Resolve only a face shared with the existing CSS glyph painter. An
/// embedding's unspecified primary font cannot be guessed from CSS alone.
pub(crate) fn resolved_font_geometry(style: &Style) -> Option<ResolvedFontGeometry> {
    if !style.font_size.is_finite() || style.font_size <= 0.0 { return None; }
    let registered = registered_typeface(style);
    if registered.is_none() && style_uses_ahem(style) {
        // The unregistered Ahem painter uses deterministic em cells rather
        // than the unresolved font-stack fallback. Decoration and line
        // metrics must describe those same cells, not a Times fallback.
        return Some(ResolvedFontGeometry {
            ascent: style.font_size * 0.8,
            descent: style.font_size * 0.2,
            line_gap: 0.0,
        });
    }
    let generic = generic_serif_typeface(style);
    let face = registered.as_ref().map(|(_, face)| face).or(generic.as_ref())?;
    let face = if registered.is_none() {
        typeface_for_character(face, 'x', style.font_weight)
    } else { face.clone() };
    let metrics = typeface_font_geometry(&face, style.font_size);
    (metrics.height().is_finite() && metrics.height() > 0.0).then_some(metrics)
}

/// Normal lines include the concrete fallback faces used by glyph shaping,
/// not just the CSS stack's primary strut. Keep the primary inline font box
/// unchanged: this geometry expands line ascent/descent, not decoration.
pub(crate) fn resolved_text_font_geometry(text: &str, style: &Style) -> Option<ResolvedFontGeometry> {
    let primary = resolved_font_geometry(style)?;
    if !style.line_height_is_normal || text.is_empty() {
        return Some(primary);
    }
    Some(with_text_typeface(text, style, |face| {
        // Leading belongs to each face's own line box. Combining maximum
        // ink ascent/descent with another face's leading invents extra space
        // (Times + its Hebrew fallback would incorrectly grow 18px to 19px).
        css_font_runs(text, face, style).into_iter().fold(primary.normal_line_box(), |metrics, run| {
            let fallback = typeface_font_geometry(&run.typeface, style.font_size).normal_line_box();
            ResolvedFontGeometry { ascent: metrics.ascent.max(fallback.ascent),
                descent: metrics.descent.max(fallback.descent),
                line_gap: 0.0 }
        })
    }))
}

pub(crate) struct ReplayFrame<'a> {
    pub nodes: &'a [(usize, LayoutRect, &'a ComponentKind, &'a Style)],
    pub metrics_font: &'a fontdue::Font,
    pub scroll_info: &'a [Option<(f32, f32, LayoutRect)>],
    pub text_input_values: &'a HashMap<usize, String>,
    pub focused_index: Option<usize>,
    pub background: w3cos_std::color::Color,
    pub artifact: Option<&'a PaintArtifact>,
    pub retained: Option<&'a mut RetainedSkiaCache>,
    pub compositor_overrides: Option<&'a CompositorOverrides>,
    pub scale_factor: f32,
}

#[derive(Default)]
pub struct RetainedSkiaCache {
    tree: RetainedLayerTree,
    pictures: Vec<Picture>,
    glyph_clip_decisions: Vec<Vec<GlyphClipDecision>>,
}

#[derive(Clone)]
struct GlyphClipDecision {
    ink: Rect,
    visible: bool,
}

struct GlyphClipRecording {
    clip: Option<Rect>,
    decisions: Vec<GlyphClipDecision>,
}

thread_local! {
    static GLYPH_CLIP_RECORDING: RefCell<Option<GlyphClipRecording>> = const { RefCell::new(None) };
}

struct GlyphClipRecordingScope {
    previous: Option<GlyphClipRecording>,
    active: bool,
}

impl GlyphClipRecordingScope {
    fn new(clip: Option<Rect>) -> Self {
        let previous = GLYPH_CLIP_RECORDING.with(|recording|
            recording.replace(Some(GlyphClipRecording { clip, decisions: Vec::new() })));
        Self { previous, active: true }
    }

    fn finish(mut self) -> Vec<GlyphClipDecision> {
        let recording = GLYPH_CLIP_RECORDING.with(|recording| recording.replace(self.previous.take()));
        self.active = false;
        recording.map_or_else(Vec::new, |recording| recording.decisions)
    }
}

impl Drop for GlyphClipRecordingScope {
    fn drop(&mut self) {
        if self.active {
            GLYPH_CLIP_RECORDING.with(|recording| recording.replace(self.previous.take()));
        }
    }
}

fn glyph_ink_visible(ink: Rect, clip: Option<Rect>) -> bool {
    clip.is_none_or(|clip| !clip.is_empty() && ink.left < clip.right && ink.right > clip.left
        && ink.top < clip.bottom && ink.bottom > clip.top)
}

fn glyph_clip_decisions_match(decisions: &[GlyphClipDecision], clip: Option<Rect>) -> bool {
    decisions.iter().all(|decision| glyph_ink_visible(decision.ink, clip) == decision.visible)
}

impl RetainedSkiaCache {
    pub fn invalidate_recordings(&mut self) {
        self.tree.invalidate_recordings();
    }

    #[cfg(test)]
    pub(crate) fn full_scene_rebuilds(&self) -> u64 {
        self.tree.full_scene_rebuilds
    }

    #[cfg(test)]
    pub(crate) fn compositor_replays(&self) -> u64 {
        self.tree.compositor_replays
    }
}

pub struct SkiaRasterizer {
    surface: Option<Surface>,
    size: (u32, u32),
    rgba: Vec<u8>,
    typeface: Typeface,
    retained: RetainedSkiaCache,
}

impl SkiaRasterizer {
    pub fn new(font_bytes: &[u8]) -> Option<Self> {
        let typeface = primary_typeface(font_bytes)?;
        Some(Self {
            surface: None,
            size: (0, 0),
            rgba: Vec::new(),
            typeface,
            retained: RetainedSkiaCache::default(),
        })
    }

    pub fn new_host() -> Option<Self> {
        let typeface = host_typeface()?;
        Some(Self {
            surface: None,
            size: (0, 0),
            rgba: Vec::new(),
            typeface,
            retained: RetainedSkiaCache::default(),
        })
    }

    pub fn invalidate_recordings(&mut self) {
        self.retained.invalidate_recordings();
    }

    #[cfg(test)]
    pub(crate) fn retained_rebuilds(&self) -> u64 {
        self.retained.full_scene_rebuilds()
    }

    #[cfg(test)]
    pub(crate) fn retained_replays(&self) -> u64 {
        self.retained.compositor_replays()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn render_frame(
        &mut self,
        width: u32,
        height: u32,
        nodes: &[(usize, LayoutRect, &ComponentKind, &Style)],
        metrics_font: &fontdue::Font,
        scroll_info: &[Option<(f32, f32, LayoutRect)>],
        text_input_values: &HashMap<usize, String>,
        focused_index: Option<usize>,
        background: w3cos_std::color::Color,
        artifact: Option<&PaintArtifact>,
        compositor_overrides: Option<&CompositorOverrides>,
        scale_factor: f32,
    ) -> Option<&[u8]> {
        self.ensure_surface(width, height)?;
        let surface = self.surface.as_mut()?;
        replay_frame(
            surface.canvas(),
            &self.typeface,
            ReplayFrame {
                nodes,
                metrics_font,
                scroll_info,
                text_input_values,
                focused_index,
                background,
                artifact,
                retained: Some(&mut self.retained),
                compositor_overrides,
                scale_factor,
            },
        );
        let expected = width as usize * height as usize * 4;
        self.rgba.resize(expected, 0);
        let info = ImageInfo::new(
            (width as i32, height as i32),
            ColorType::RGBA8888,
            AlphaType::Premul,
            None,
        );
        surface
            .read_pixels(&info, &mut self.rgba, width as usize * 4, (0, 0))
            .then_some(self.rgba.as_slice())
    }

    fn ensure_surface(&mut self, width: u32, height: u32) -> Option<()> {
        if width == 0 || height == 0 {
            return None;
        }
        if self.size != (width, height) {
            self.surface = Surface::new_raster_n32_premul((width as i32, height as i32));
            self.size = (width, height);
        }
        self.surface.as_ref().map(|_| ())
    }
}

fn paint_display_list(
    canvas: &Canvas,
    typeface: &Typeface,
    frame: ReplayFrame<'_>,
    bake_compositor_props: bool,
) {
    paint_display_list_with_suppressed_effects(canvas, typeface, frame, bake_compositor_props, &[], true);
}

fn paint_display_list_with_suppressed_effects(
    canvas: &Canvas, typeface: &Typeface, frame: ReplayFrame<'_>,
    bake_compositor_props: bool, suppressed: &[usize], paint_background: bool,
) {
    let shaped_fragments = frame.artifact.map(|artifact| {
        crate::inline_shaping::build(artifact, frame.nodes.iter().map(|node| node.0), typeface)
    }).unwrap_or_default();
    let background_groups = frame.artifact.map(inline_background_groups).unwrap_or_default();
    let grouped_backgrounds: std::collections::HashSet<usize> = background_groups.values()
        .flatten().copied().collect();
    let blur_bounds = small_blur_input_bounds(&frame, bake_compositor_props);
    // Layer recordings must not bake a background clear; composite applies
    // the canvas background, then opacity / transform / scroll.
    if bake_compositor_props && paint_background {
        canvas.clear(to_skia_color(frame.background, 1.0));
        paint_canvas_background_image(canvas, frame.artifact, frame.scale_factor);
    }
    let mut active_filters = Vec::new();
    let mut suppressed_subtree = None;
    for &(idx, rect, kind, style) in frame.nodes {
        let mut filter_path: Vec<_> = effect_path(frame.artifact, idx, bake_compositor_props)
            .into_iter().filter(|id| !suppressed.contains(id)).collect();
        if suppressed_subtree.is_some_and(|id| filter_path.contains(&id)) { continue; }
        suppressed_subtree = None;
        let prepared = filter_path.iter().position(|id| blur_bounds.contains_key(id)).and_then(|position| {
            let id = filter_path[position];
            let artifact = frame.artifact?;
            let effect = artifact.properties.effects.get(id)?;
            let chain = effect.filter.as_deref().and_then(parse_css_filter)?;
            let [FilterOp::Blur(sigma)] = chain.ops.as_slice() else { return None; };
            let nodes: Vec<_> = frame.nodes.iter().copied().filter(|node|
                effect_path(frame.artifact, node.0, bake_compositor_props).contains(&id)).collect();
            let mut excluded = suppressed.to_vec();
            excluded.extend_from_slice(&filter_path[..=position]);
            let (image, origin) = browser_blur::rasterize_small_blur(*sigma, blur_bounds[&id], |input| {
                paint_display_list_with_suppressed_effects(input, typeface, ReplayFrame {
                    nodes: &nodes, metrics_font: frame.metrics_font, scroll_info: frame.scroll_info,
                    text_input_values: frame.text_input_values, focused_index: frame.focused_index,
                    background: frame.background, artifact: frame.artifact, retained: None,
                    compositor_overrides: frame.compositor_overrides, scale_factor: frame.scale_factor,
                }, bake_compositor_props, &excluded, false);
            })?;
            Some((position, id, image, origin, effect.opacity))
        });
        if let Some((position, ..)) = prepared.as_ref() { filter_path.truncate(*position); }
        let common = active_filters
            .iter()
            .zip(&filter_path)
            .take_while(|(left, right)| left == right)
            .count();
        for _ in common..active_filters.len() {
            canvas.restore();
        }
        active_filters.truncate(common);
        for &effect_id in &filter_path[common..] {
            let Some(effect) = frame
                .artifact
                .and_then(|artifact| artifact.properties.effects.get(effect_id))
            else {
                continue;
            };
            let mut paint = opacity_layer_paint(
                if bake_compositor_props { effect.opacity } else { 1.0 });
            if let Some(filter) = effect
                .filter
                .as_deref()
                .and_then(parse_css_filter)
                .and_then(|chain| skia_filter_chain(&chain))
            {
                paint.set_image_filter(filter);
            }
            canvas.save_layer(&SaveLayerRec::default().paint(&paint));
            active_filters.push(effect_id);
        }
        if let Some((_, id, image, origin, opacity)) = prepared {
            let paint = color_paint(w3cos_std::Color::WHITE,
                if bake_compositor_props { opacity } else { 1.0 });
            canvas.draw_image(&image, origin, Some(&paint));
            suppressed_subtree = Some(id);
            continue;
        }
        if style.opacity <= 0.0 {
            continue;
        }
        let (rect, clip) = match frame.scroll_info.get(idx).copied().flatten() {
            Some((sx, sy, clip)) => (
                LayoutRect {
                    x: rect.x - sx,
                    y: rect.y - sy,
                    ..rect
                },
                Some(clip),
            ),
            None => (rect, None),
        };

        let fragments = frame.artifact.and_then(|artifact| artifact.column_fragments.get(idx))
            .filter(|fragments| !fragments.is_empty());
        for fragment_index in 0..fragments.map_or(1, Vec::len) {
        let save = canvas.save();
        if bake_compositor_props && let Some(artifact) = frame.artifact {
            let (sx, sy) = artifact.viewport_scroll_for(idx);
            canvas.translate((-sx * frame.scale_factor, -sy * frame.scale_factor));
        }
        if let Some(fragment) = fragments.and_then(|fragments| fragments.get(fragment_index)) {
            let scale = frame.scale_factor;
            if let Some(bounds) = canvas.local_clip_bounds() {
                canvas.clip_rect(Rect::new(bounds.left(), fragment.clip_top * scale,
                    bounds.right(), fragment.clip_bottom * scale), None, Some(false));
            }
            canvas.translate((fragment.translate_x * scale, fragment.translate_y * scale));
        }
        for clip in clip_path(frame.artifact, idx, !bake_compositor_props) {
            canvas.clip_rect(to_rect(clip), None, Some(false));
        }
        if let Some(clip) = clip {
            canvas.clip_rect(to_rect(clip), None, Some(false));
        }
        let local_filter = frame.artifact.is_none().then(|| {
            style
                .filter
                .as_deref()
                .and_then(parse_css_filter)
                .and_then(|chain| skia_filter_chain(&chain))
                .map(|filter| {
                    let mut paint = Paint::default();
                    paint.set_image_filter(filter);
                    paint
                })
        });
        if let Some(Some(paint)) = local_filter.as_ref() {
            canvas.save_layer(&SaveLayerRec::default().paint(paint));
        }
        // With a PaintArtifact, opacity belongs to the Effect tree and must be
        // applied once to the whole subtree. Avoid multiplying it into this
        // display item a second time.
        if let Some(group) = background_groups.get(&idx) {
            let artifact = frame.artifact.expect("groups require retained nodes");
            let origin = artifact.rect_by_index[idx].expect("group has layout");
            let first_line_font_box = style.custom_properties.as_ref()
                .and_then(|p| p.get("--w3cos-internal-inline-background-group"))
                .filter(|source| source.as_str() == "first-line")
                .and_then(|_| artifact.nodes[idx].parent)
                .and_then(|parent| group.iter().find(|&&member| {
                    let font = &artifact.nodes[member].style;
                    let owner = &artifact.nodes[parent].style;
                    font.font_size == owner.font_size && font.font_family == owner.font_family
                        && font.font_weight == owner.font_weight && font.font_style == owner.font_style
                }))
                .and_then(|&member| artifact.rect_by_index[member].map(|font_box| {
                    let font = &artifact.nodes[member].style;
                    (font_box.y + rect.y - origin.y, resolved_font_geometry(font)
                        .map_or(font.font_size, ResolvedFontGeometry::height))
                }));
            for &member in group {
                let node = &artifact.nodes[member];
                let ComponentKind::Text { content } = &node.kind else { continue; };
                let Some(mut member_rect) = artifact.rect_by_index[member] else { continue; };
                member_rect.x += rect.x - origin.x;
                member_rect.y += rect.y - origin.y;
                let mut background_style = node.style.clone();
                background_style.opacity = 1.0;
                if plain_inline_background(&background_style)
                    && background_style.background_image.as_deref().is_none_or(|image|
                        image.trim().eq_ignore_ascii_case("none"))
                    && let Some((y, height)) = first_line_font_box
                {
                    // A first-line pseudo owns one principal font box. A
                    // larger descendant's ink does not enlarge that source.
                    background_style.custom_properties.get_or_insert_with(Default::default)
                        .insert("--w3cos-internal-inline-background-font-box".into(), format!("{y} {height}"));
                }
                let context = artifact.inline_line_context(member).map(|mut context| {
                    context.line_box.x += rect.x - origin.x;
                    context.line_box.y += rect.y - origin.y;
                    context
                });
                draw_text_in_rect_with_line_painter(canvas, member_rect, content,
                    &background_style, typeface, frame.metrics_font, context, true,
                    &mut |_, _, _, _, advance, _, _| advance);
            }
        }
        let normalized_style = (frame.artifact.is_some()
            && (style.opacity < 0.999 || grouped_backgrounds.contains(&idx))).then(|| {
            let mut normalized = style.clone();
            normalized.opacity = 1.0;
            if grouped_backgrounds.contains(&idx) {
                normalized.background = w3cos_std::Color::TRANSPARENT;
            }
            normalized
        });
        let render_rect = inline_background_union_rect(
            frame.artifact,
            idx,
            rect,
            kind,
            normalized_style.as_ref().unwrap_or(style),
        );
        let normal_plain_white_background = style.line_height_is_normal
            && style.background.r == 255
            && style.background.g == 255
            && style.background.b == 255
            && style.padding_lengths().top == 0.0
            && style.padding_lengths().right == 0.0
            && style.padding_lengths().bottom == 0.0
            && style.padding_lengths().left == 0.0
            && style.border_width == 0.0
            && style.border_top_width.unwrap_or(0.0) == 0.0
            && style.border_right_width.unwrap_or(0.0) == 0.0
            && style.border_bottom_width.unwrap_or(0.0) == 0.0
            && style.border_left_width.unwrap_or(0.0) == 0.0;
        let extra_inline_background_rects = if style.display == Display::Inline
            && style.background.a > 0
            && matches!(kind, ComponentKind::Row | ComponentKind::Box)
        {
            frame
                .artifact
                .map(|artifact| {
                    artifact
                        .nodes
                        .iter()
                        .enumerate()
                        .filter_map(|(descendant, node)| {
                            let mut parent = node.parent;
                            let mut is_descendant = false;
                            let mut under_decorated = false;
                            while let Some(candidate) = parent {
                                if candidate == idx {
                                    is_descendant = true;
                                    break;
                                }
                                let ancestor = artifact.nodes.get(candidate)?;
                                if ancestor.style.background.a > 0
                                    || ancestor.style.background_image.is_some()
                                    || ancestor.style.padding_lengths().top > 0.0
                                    || ancestor.style.padding_lengths().right > 0.0
                                    || ancestor.style.padding_lengths().bottom > 0.0
                                    || ancestor.style.padding_lengths().left > 0.0
                                    || ancestor.style.border_width > 0.0
                                    || ancestor
                                        .style
                                        .border_top_width
                                        .is_some_and(|width| width > 0.0)
                                    || ancestor
                                        .style
                                        .border_right_width
                                        .is_some_and(|width| width > 0.0)
                                    || ancestor
                                        .style
                                        .border_bottom_width
                                        .is_some_and(|width| width > 0.0)
                                    || ancestor
                                        .style
                                        .border_left_width
                                        .is_some_and(|width| width > 0.0)
                                {
                                    under_decorated = true;
                                }
                                parent = ancestor.parent;
                            }
                            if !is_descendant
                                || under_decorated
                                // A descendant's different em box is not a
                                // continuation of this inline's decoration.
                                // Its text remains independently paintable.
                                || (node.style.font_size - style.font_size).abs() > 0.01
                                || !matches!(node.kind, ComponentKind::Text { .. })
                            {
                                return None;
                            }
                            let rect = artifact.rect_by_index.get(descendant).copied().flatten()?;
                            let ComponentKind::Text { content } = &node.kind else {
                                return None;
                            };
                            // Coincident geometry does not remove a text
                            // fragment. In a block-split inline the first
                            // text rect can equal the principal rect, while
                            // its painted advance is shorter than that rect.
                            Some((rect, content.clone()))
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let line_context = frame
            .artifact
            .and_then(|artifact| artifact.inline_line_context(idx))
            .map(|mut context| {
                if let Some((sx, sy, _)) = frame.scroll_info.get(idx).copied().flatten() {
                    context.line_box.x -= sx;
                    context.line_box.y -= sy;
                    context.first_line_box.x -= sx;
                    context.first_line_box.y -= sy;
                }
                context
            });
        // A decorated inline nested inside another inline shares the
        // parent's anonymous line box, but its own background/border should
        // remain centered in that line box. Keep this context for decoration
        // paint only; text still uses the original layout rect.
        let inline_decoration_line_box = frame.artifact.and_then(|artifact| {
            let parent = artifact.nodes.get(idx)?.parent?;
            if !matches!(
                artifact.nodes.get(parent)?.style.display,
                Display::Inline | Display::Contents
            ) {
                return None;
            }
            let mut ancestor = Some(parent);
            while let Some(index) = ancestor {
                let node = artifact.nodes.get(index)?;
                let is_line = node
                    .style
                    .custom_properties
                    .as_ref()
                    .is_some_and(|properties| {
                        properties.contains_key("--w3cos-internal-anonymous-block-line")
                    });
                if is_line {
                    return artifact.rect_by_index.get(index).copied().flatten();
                }
                ancestor = node.parent;
            }
            None
        });
        let inline_text_leading = frame.artifact.and_then(|artifact| {
            if !matches!(kind, ComponentKind::Text { .. }) {
                return None;
            }
            let mut ancestor = artifact.nodes.get(idx)?.parent;
            let mut saw_inline = false;
            while let Some(index) = ancestor {
                let node = artifact.nodes.get(index)?;
                if matches!(node.style.display, Display::Inline | Display::Contents) {
                    let padding = node.style.padding_lengths();
                    let has_edge = node.style.border_width > 0.0
                        || node.style.border_top_width.is_some_and(|width| width > 0.0)
                        || node.style.border_right_width.is_some_and(|width| width > 0.0)
                        || node.style.border_bottom_width.is_some_and(|width| width > 0.0)
                        || node.style.border_left_width.is_some_and(|width| width > 0.0)
                        || [padding.top, padding.right, padding.bottom, padding.left]
                            .into_iter()
                            .any(|edge| edge > 0.0);
                    if has_edge || top_aligned_inline(&node.style) {
                        return Some(false);
                    }
                    saw_inline = true;
                }
                if node
                    .style
                    .custom_properties
                    .as_ref()
                    .is_some_and(|properties| {
                        properties.contains_key("--w3cos-internal-anonymous-block-line")
                    })
                {
                    let text_rect = artifact.rect_by_index.get(idx).copied().flatten()?;
                    let line_rect = artifact.rect_by_index.get(index).copied().flatten()?;
                    let strut_height = crate::layout::inline_style_line_height(&node.style);
                    return Some(saw_inline && inline_text_is_in_first_strut(
                        text_rect, line_rect, strut_height,
                    ));
                }
                ancestor = node.parent;
            }
            Some(false)
        });
        let applied_decorations = frame.artifact
            .map_or_else(Vec::new, |artifact| artifact.ancestor_text_decorations(idx));
        render_node_with_applied_decorations(
            canvas,
            idx,
            render_rect,
            kind,
            normalized_style.as_ref().unwrap_or(style),
            typeface,
            frame.metrics_font,
            frame.text_input_values.get(&idx).map(String::as_str),
            frame.focused_index == Some(idx),
            bake_compositor_props,
            line_context,
            inline_decoration_line_box,
            inline_text_leading.unwrap_or(false),
            frame.artifact.is_some_and(|artifact| {
                style.display == Display::Inline
                    && matches!(kind, ComponentKind::Row | ComponentKind::Box)
                    && inline_has_content(artifact, idx)
            }),
            &extra_inline_background_rects,
            // Replay can combine several source words under the first client
            // ID. That ID's original glyph slice no longer describes the new
            // Text; reshape it instead of silently painting only the first word.
            shaped_fragments.get(&idx).filter(|_| frame.artifact.is_some_and(|artifact|
                artifact.nodes.get(idx).is_some_and(|source| match (kind, &source.kind) {
                    (ComponentKind::Text { content }, ComponentKind::Text { content: original }) => content == original,
                    _ => false,
                }))),
            &applied_decorations,
            frame.artifact.and_then(|artifact| terminal_inline_padding_fragment(artifact, idx, render_rect)),
        );
        if matches!(local_filter, Some(Some(_))) {
            canvas.restore();
        }
        canvas.restore_to_count(save);
        }
    }
    for _ in 0..active_filters.len() {
        canvas.restore();
    }
}

pub(crate) fn replay_frame(canvas: &Canvas, typeface: &Typeface, mut frame: ReplayFrame<'_>) {
    let retained = frame.retained.take();
    let overrides = frame.compositor_overrides;
    let scale_factor = if frame.scale_factor > 0.0 {
        frame.scale_factor
    } else {
        1.0
    };
    if let (Some(artifact), Some(cache)) = (frame.artifact, retained) {
        let mut scrolls = HashMap::new();
        for (idx, info) in frame.scroll_info.iter().enumerate() {
            if let Some((sx, sy, _)) = info {
                scrolls.insert(idx, (*sx, *sy));
            }
        }
        let default_overrides = CompositorOverrides::default();
        let overrides = overrides.unwrap_or(&default_overrides);
        let action = cache.tree.sync(artifact, &scrolls, overrides);
        let glyph_clips = cache.tree.layers.iter().map(|layer|
            layer_glyph_clip(layer, artifact, frame.scroll_info, overrides, scale_factor,
                canvas.local_clip_bounds().unwrap_or_default()))
            .collect::<Vec<_>>();
        let reusable = matches!(action, LayerPaintAction::Replay)
            && cache.tree.recordings_valid()
            && cache.pictures.len() == cache.tree.layers.len()
            && cache.glyph_clip_decisions.len() == cache.tree.layers.len();
        let can_replay = reusable && cache.glyph_clip_decisions.iter().zip(&glyph_clips)
            .all(|(decisions, clip)| glyph_clip_decisions_match(decisions, *clip));
        if can_replay {
            cache.tree.note_replay();
            crate::perf::record_paint_path("retained-layer-replay");
            composite_skia_layers(
                canvas,
                &cache.pictures,
                &cache.tree.layers,
                artifact,
                frame.scroll_info,
                overrides,
                frame.background,
                scale_factor,
            );
            return;
        }
        let mut pictures = Vec::with_capacity(cache.tree.layers.len());
        let mut glyph_clip_decisions = Vec::with_capacity(cache.tree.layers.len());
        for (layer_index, layer) in cache.tree.layers.iter().enumerate() {
            if reusable && glyph_clip_decisions_match(
                &cache.glyph_clip_decisions[layer_index], glyph_clips[layer_index],
            ) {
                pictures.push(cache.pictures[layer_index].clone());
                glyph_clip_decisions.push(cache.glyph_clip_decisions[layer_index].clone());
                continue;
            }
            let layer_nodes: Vec<_> = frame
                .nodes
                .iter()
                .copied()
                .filter(|(idx, _, _, _)| layer.client_indices.contains(idx))
                .collect();
            let mut recorder = PictureRecorder::new();
            let bounds = Rect::new(
                layer.bounds.x * scale_factor - 64.0,
                layer.bounds.y * scale_factor - 64.0,
                (layer.bounds.x + layer.bounds.width) * scale_factor + 64.0,
                (layer.bounds.y + layer.bounds.height) * scale_factor + 64.0,
            );
            let recording = recorder.begin_recording(bounds, false);
            let glyph_clip = GlyphClipRecordingScope::new(glyph_clips[layer_index]);
            paint_display_list(
                recording,
                typeface,
                ReplayFrame {
                    nodes: &layer_nodes,
                    metrics_font: frame.metrics_font,
                    scroll_info: &[],
                    text_input_values: frame.text_input_values,
                    focused_index: frame.focused_index,
                    background: w3cos_std::color::Color::TRANSPARENT,
                    artifact: Some(artifact),
                    retained: None,
                    compositor_overrides: None,
                    scale_factor,
                },
                false,
            );
            let decisions = glyph_clip.finish();
            if let Some(picture) = recorder.finish_recording_as_picture(None) {
                pictures.push(picture);
                glyph_clip_decisions.push(decisions);
            }
        }
        cache.pictures = pictures;
        cache.glyph_clip_decisions = glyph_clip_decisions;
        cache.tree.note_rebuild();
        crate::perf::record_paint_path("full-scene-rebuild");
        composite_skia_layers(
            canvas,
            &cache.pictures,
            &cache.tree.layers,
            artifact,
            frame.scroll_info,
            overrides,
            frame.background,
            scale_factor,
        );
        return;
    }
    paint_display_list(canvas, typeface, frame, true);
}

fn layer_scrollport_clips(
    layer: &crate::retained_layers::CompositorLayer, artifact: &PaintArtifact,
    scroll_info: &[Option<(f32, f32, LayoutRect)>],
) -> Vec<LayoutRect> {
    let mut clips = Vec::new();
    if let Some(clip) = layer_scroll_translation(layer, scroll_info).2 { clips.push(clip); }
    let root_overflow_visible = artifact.nodes.first().is_some_and(|root|
        root.style.resolved_overflow_x() == w3cos_std::style::Overflow::Visible
            && root.style.resolved_overflow_y() == w3cos_std::style::Overflow::Visible);
    let mut scroll = layer.properties.scroll;
    while scroll != 0 {
        let Some(node) = artifact.properties.scrolls.get(scroll) else { break; };
        let viewport_host = artifact.body_index.is_some() && (node.host_index == Some(0)
            || (root_overflow_visible && node.host_index == artifact.body_index));
        if !viewport_host && let Some(rect) = node.clip
            .and_then(|clip| artifact.properties.clips.get(clip)).and_then(|clip| clip.rect)
        { clips.push(rect); }
        if node.parent == scroll { break; }
        scroll = node.parent;
    }
    clips
}

fn layer_glyph_clip(
    layer: &crate::retained_layers::CompositorLayer, artifact: &PaintArtifact,
    scroll_info: &[Option<(f32, f32, LayoutRect)>], overrides: &CompositorOverrides, scale: f32,
    viewport: Rect,
) -> Option<Rect> {
    // Filters can spread ink across a clip even when the unfiltered outline
    // lies wholly outside it. Keep the recording conservative until the
    // filter's visual outsets are represented in the glyph bounds.
    for &index in &layer.client_indices {
        let mut current = artifact.node_properties.get(index)?.effect;
        while current != 0 {
            let effect = artifact.properties.effects.get(current)?;
            if effect.filter.is_some() { return None; }
            if effect.parent == current { break; }
            current = effect.parent;
        }
    }
    let (sx, sy) = layer.client_indices.first().map_or((0.0, 0.0), |index|
        artifact.viewport_scroll_for(*index));
    let viewport = Rect::new(viewport.left + sx * scale, viewport.top + sy * scale,
        viewport.right + sx * scale, viewport.bottom + sy * scale);
    let clip = layer_scrollport_clips(layer, artifact, scroll_info).into_iter().map(to_rect)
        .fold(viewport, |left, right| Rect::new(left.left.max(right.left), left.top.max(right.top),
            left.right.min(right.right), left.bottom.min(right.bottom)));
    if clip.is_empty() { return Some(Rect::default()); }
    let inverse = compositor_layer_matrix(layer, artifact, scroll_info, overrides, scale).invert()?;
    if inverse.has_perspective() { return None; }
    let local = inverse.map_rect(clip).0;
    local.is_finite().then_some(local)
}

fn compositor_layer_matrix(
    layer: &crate::retained_layers::CompositorLayer,
    artifact: &PaintArtifact,
    scroll_info: &[Option<(f32, f32, LayoutRect)>],
    overrides: &CompositorOverrides,
    scale_factor: f32,
) -> Matrix {
    let (scroll_x, scroll_y, _) = layer_scroll_translation(layer, scroll_info);
    let css = layer_css_transform(layer, artifact, overrides);
    let mut matrix = Matrix::new_identity();
    if !css.is_identity() {
        let origin = (layer.bounds.x * scale_factor, layer.bounds.y * scale_factor);
        matrix.post_translate((-origin.0, -origin.1));
        if (css.scale_x - 1.0).abs() > f32::EPSILON || (css.scale_y - 1.0).abs() > f32::EPSILON {
            matrix.post_scale((css.scale_x, css.scale_y), None);
        }
        if css.rotate_deg.abs() > f32::EPSILON {
            matrix.post_rotate(css.rotate_deg, None);
        }
        matrix.post_translate((
            origin.0 + css.translate_x * scale_factor,
            origin.1 + css.translate_y * scale_factor,
        ));
    }
    matrix.post_translate((scroll_x, scroll_y));
    matrix
}

fn composite_skia_layers(
    canvas: &Canvas,
    pictures: &[Picture],
    layers: &[crate::retained_layers::CompositorLayer],
    artifact: &PaintArtifact,
    scroll_info: &[Option<(f32, f32, LayoutRect)>],
    overrides: &CompositorOverrides,
    background: w3cos_std::color::Color,
    scale_factor: f32,
) {
    canvas.clear(to_skia_color(background, 1.0));
    paint_canvas_background_image(canvas, Some(artifact), scale_factor);
    for (layer, picture) in layers.iter().zip(pictures) {
        let matrix = compositor_layer_matrix(layer, artifact, scroll_info, overrides, scale_factor);
        let opacity = compositor_layer_opacity(layer, artifact, overrides);
        let save = canvas.save();
        if let Some(&index) = layer.client_indices.first() {
            let (sx, sy) = artifact.viewport_scroll_for(index);
            canvas.translate((-sx * scale_factor, -sy * scale_factor));
        }
        for clip_rect in layer_scrollport_clips(layer, artifact, scroll_info) {
            canvas.clip_rect(to_rect(clip_rect), None, Some(false));
        }
        let isolates_surface = artifact.properties.effects.get(layer.properties.effect)
            .is_some_and(|effect| effect.isolates_surface);
        if opacity < 0.999 || isolates_surface {
            // CSS opacity uses the same normalized SrcOver rounding as
            // ordinary paint, not Skia's legacy integer layer shortcut.
            let paint = opacity_layer_paint(opacity);
            canvas.save_layer(&SaveLayerRec::default().paint(&paint));
        }
        canvas.draw_picture(picture, Some(&matrix), None);
        canvas.restore_to_count(save);
    }
}

fn paint_canvas_background_image(
    canvas: &Canvas,
    artifact: Option<&PaintArtifact>,
    scale_factor: f32,
) {
    let Some(style) = artifact
        .and_then(|artifact| artifact.canvas_background_style.as_ref())
        .filter(|style| style.background_image.is_some())
    else {
        return;
    };
    let scale_factor = if scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    let image_info = canvas.image_info();
    let save = canvas.save();
    canvas.scale((scale_factor, scale_factor));
    draw_canvas_background_image(
        canvas,
        LayoutRect {
            x: 0.0,
            y: 0.0,
            width: image_info.width() as f32 / scale_factor,
            height: image_info.height() as f32 / scale_factor,
        },
        0.0,
        style,
        artifact.and_then(|artifact| artifact.canvas_background_positioning_rect.map(|rect| {
            LayoutRect {
                x: rect.x - artifact.viewport_scroll.0,
                y: rect.y - artifact.viewport_scroll.1,
                ..rect
            }
        })),
        1.0,
    );
    canvas.restore_to_count(save);
}

#[cfg(target_os = "ios")]
pub struct SkiaMetalPresenter {
    layer: objc2_06::rc::Retained<objc2_quartz_core::CAMetalLayer>,
    command_queue:
        objc2_06::rc::Retained<objc2_06::runtime::ProtocolObject<dyn objc2_metal::MTLCommandQueue>>,
    context: skia_safe::gpu::DirectContext,
    typeface: Typeface,
    retained: RetainedSkiaCache,
}

#[cfg(target_os = "ios")]
impl SkiaMetalPresenter {
    pub fn new(window: &winit::window::Window, font_bytes: &[u8]) -> Option<Self> {
        Self::new_with_typeface(window, primary_typeface(font_bytes)?)
    }

    pub fn new_host(window: &winit::window::Window) -> Option<Self> {
        Self::new_with_typeface(window, host_typeface()?)
    }

    fn new_with_typeface(window: &winit::window::Window, typeface: Typeface) -> Option<Self> {
        use objc2_metal::{MTLCreateSystemDefaultDevice, MTLDevice};
        use objc2_quartz_core::CALayer;
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

        let device = MTLCreateSystemDefaultDevice()?;
        let layer = objc2_quartz_core::CAMetalLayer::new();
        layer.setDevice(Some(&device));
        layer.setPixelFormat(objc2_metal::MTLPixelFormat::BGRA8Unorm);
        layer.setPresentsWithTransaction(false);
        layer.setFramebufferOnly(false);

        let handle = window.window_handle().ok()?;
        let RawWindowHandle::UiKit(handle) = handle.as_raw() else {
            return None;
        };
        let view = unsafe {
            (handle.ui_view.as_ptr() as *mut objc2_ui_kit::UIView)
                .as_ref()
                .expect("winit UiKit view")
        };
        let parent_layer = view.layer();
        layer.setFrame(parent_layer.bounds());
        parent_layer.addSublayer(&layer);

        let command_queue = device.newCommandQueue()?;
        let backend = unsafe {
            skia_safe::gpu::mtl::BackendContext::new(
                objc2_06::rc::Retained::as_ptr(&device) as skia_safe::gpu::mtl::Handle,
                objc2_06::rc::Retained::as_ptr(&command_queue) as skia_safe::gpu::mtl::Handle,
            )
        };
        let context = skia_safe::gpu::direct_contexts::make_metal(&backend, None)?;
        Some(Self {
            layer,
            command_queue,
            context,
            typeface,
            retained: RetainedSkiaCache::default(),
        })
    }

    pub fn invalidate_recordings(&mut self) {
        self.retained.invalidate_recordings();
    }

    #[allow(clippy::too_many_arguments)]
    pub fn render_frame(
        &mut self,
        width: u32,
        height: u32,
        nodes: &[(usize, LayoutRect, &ComponentKind, &Style)],
        metrics_font: &fontdue::Font,
        scroll_info: &[Option<(f32, f32, LayoutRect)>],
        text_input_values: &HashMap<usize, String>,
        focused_index: Option<usize>,
        background: w3cos_std::color::Color,
        artifact: Option<&PaintArtifact>,
        compositor_overrides: Option<&CompositorOverrides>,
        scale_factor: f32,
    ) -> bool {
        use objc2_06::rc::Retained;
        use objc2_06::runtime::ProtocolObject;
        use objc2_core_foundation::CGSize;
        use objc2_metal::{MTLCommandBuffer, MTLCommandQueue};
        use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer};
        use skia_safe::gpu::{SurfaceOrigin, backend_render_targets, mtl};

        self.layer
            .setDrawableSize(CGSize::new(width as f64, height as f64));
        objc2_06::rc::autoreleasepool(|_| {
            let Some(drawable) = self.layer.nextDrawable() else {
                return false;
            };
            let texture_info = unsafe {
                mtl::TextureInfo::new(Retained::as_ptr(&drawable.texture()) as mtl::Handle)
            };
            let target =
                backend_render_targets::make_mtl((width as i32, height as i32), &texture_info);
            let Some(mut surface) = skia_safe::gpu::surfaces::wrap_backend_render_target(
                &mut self.context,
                &target,
                SurfaceOrigin::TopLeft,
                ColorType::BGRA8888,
                None,
                None,
            ) else {
                return false;
            };
            replay_frame(
                surface.canvas(),
                &self.typeface,
                ReplayFrame {
                    nodes,
                    metrics_font,
                    scroll_info,
                    text_input_values,
                    focused_index,
                    background,
                    artifact,
                    retained: Some(&mut self.retained),
                    compositor_overrides,
                    scale_factor,
                },
            );
            self.context.flush_and_submit();
            drop(surface);

            let Some(command_buffer) = self.command_queue.commandBuffer() else {
                return false;
            };
            let drawable: Retained<ProtocolObject<dyn objc2_metal::MTLDrawable>> =
                (&drawable).into();
            command_buffer.presentDrawable(&drawable);
            command_buffer.commit();
            true
        })
    }
}

fn inline_background_groups(artifact: &crate::paint_artifact::PaintArtifact)
    -> HashMap<usize, Vec<usize>>
{
    let mut sources: HashMap<(usize, &str), Vec<usize>> = HashMap::new();
    for (index, node) in artifact.nodes.iter().enumerate() {
        let Some(parent) = node.parent else { continue; };
        let Some(source) = node.style.custom_properties.as_ref().and_then(|properties|
            properties.get("--w3cos-internal-inline-background-group")) else { continue; };
        if node.style.display == Display::Inline && node.style.background.a > 0
            && matches!(node.kind, ComponentKind::Text { .. })
            && artifact.rect_by_index[index].is_some()
        {
            sources.entry((parent, source.as_str())).or_default().push(index);
        }
    }
    sources.into_values().filter(|group| group.len() > 1)
        .map(|group| (group[0], group)).collect()
}

fn plain_inline_background(style: &Style) -> bool {
    style.background.a > 0
        && style.padding_lengths().top == 0.0
        && style.padding_lengths().bottom == 0.0
        // Horizontal padding changes the inline's width, not its em height.
        && style.border_width == 0.0
        && style.border_top_width.unwrap_or(0.0) == 0.0
        && style.border_right_width.unwrap_or(0.0) == 0.0
        && style.border_bottom_width.unwrap_or(0.0) == 0.0
        && style.border_left_width.unwrap_or(0.0) == 0.0
}

/// A preserved terminal break closes the text fragment, but an inline's
/// trailing padding still occupies the following line. It must not remain
/// attached to the preceding line's principal background rectangle.
fn terminal_inline_padding_fragment(artifact: &PaintArtifact, index: usize, rect: LayoutRect) -> Option<LayoutRect> {
    use w3cos_std::style::{TextDirection, WhiteSpace};
    let node = artifact.nodes.get(index)?;
    if node.style.display != Display::Inline || !plain_inline_background(&node.style)
        || node.style.direction != TextDirection::Ltr || node.style.padding_lengths().right <= 0.0
        || !matches!(node.kind, ComponentKind::Row | ComponentKind::Box)
    { return None; }
    let children = artifact.nodes.iter().filter(|child| child.parent == Some(index)).collect::<Vec<_>>();
    if children.iter().any(|child| !matches!(child.kind, ComponentKind::Text { .. })
        || child.style.display != Display::Inline || child.style.position != w3cos_std::style::Position::Static
        || child.style.float != w3cos_std::style::Float::None) { return None; }
    let last = children.last()?;
    let ComponentKind::Text { content } = &last.kind else { return None; };
    let prepared = text_layout::prepare_text_for_white_space(content, last.style.white_space);
    if !matches!(last.style.white_space, WhiteSpace::Pre | WhiteSpace::PreWrap | WhiteSpace::PreLine)
        || !content.ends_with(['\n', '\r', '\u{2028}'])
        || prepared.chars().filter(|character| matches!(character, '\n' | '\u{2028}')).count() != 1
        || children.iter().any(|child| child.style.font_size != node.style.font_size
            || child.style.line_height != node.style.line_height)
        || children[..children.len() - 1].iter().any(|child| {
            let ComponentKind::Text { content } = &child.kind else { return true; };
            text_layout::prepare_text_for_white_space(content, child.style.white_space)
                .contains(['\n', '\u{2028}'])
        })
    { return None; }
    let original = artifact.rect_by_index.get(index).copied().flatten()?;
    let mut ancestor = node.parent;
    while let Some(owner) = ancestor {
        let parent = artifact.nodes.get(owner)?;
        if !matches!(parent.style.display, Display::Inline | Display::Contents) {
            let line = artifact.rect_by_index.get(owner).copied().flatten()?;
            return Some(LayoutRect {
                x: line.x + parent.style.padding_lengths().left
                    + parent.style.border_left_width.unwrap_or(parent.style.border_width)
                    + rect.x - original.x,
                y: rect.y + crate::layout::inline_style_line_height(&node.style),
                width: node.style.padding_lengths().right,
                height: rect.height,
            });
        }
        ancestor = parent.parent;
    }
    None
}

fn inline_background_union_rect(
    artifact: Option<&crate::paint_artifact::PaintArtifact>,
    index: usize,
    rect: LayoutRect,
    kind: &ComponentKind,
    style: &Style,
) -> LayoutRect {
    if artifact.is_none()
        || style.display != Display::Inline
        || !matches!(kind, ComponentKind::Row | ComponentKind::Box)
        || (style.background.a == 0
            && style.border_width == 0.0
            && style.border_top_width.unwrap_or(0.0) == 0.0
            && style.border_right_width.unwrap_or(0.0) == 0.0
            && style.border_bottom_width.unwrap_or(0.0) == 0.0
            && style.border_left_width.unwrap_or(0.0) == 0.0
            && style.padding_lengths().top == 0.0
            && style.padding_lengths().right == 0.0
            && style.padding_lengths().bottom == 0.0
            && style.padding_lengths().left == 0.0)
        || style.position != w3cos_std::style::Position::Static
        // Normal inline vertical edge geometry is already measured by layout.
        // Unioning it with the strut shifts a wrapper relative to an
        // equivalent merged decorated text leaf.
        || top_aligned_inline(style)
        // A plain inline's background is its own em box. Descendant and
        // paragraph leading must not enlarge it, regardless of paint color.
        || plain_inline_background(style)
        || (style.line_height_is_normal
            && (style.border_top_width.unwrap_or(style.border_width) > 0.0
                || style.border_left_width.unwrap_or(style.border_width) > 0.0
                || style.border_right_width.unwrap_or(style.border_width) > 0.0
                || style.border_bottom_width.unwrap_or(style.border_width) > 0.0
                || style.padding_lengths().top > 0.0
                || style.padding_lengths().bottom > 0.0))
    {
        return rect;
    }
    let artifact = artifact.expect("checked above");
    let mut union = rect;
    let has_inline_border = style.border_top_width.unwrap_or(style.border_width) > 0.0
        || style.border_right_width.unwrap_or(style.border_width) > 0.0
        || style.border_bottom_width.unwrap_or(style.border_width) > 0.0
        || style.border_left_width.unwrap_or(style.border_width) > 0.0;
    let decorated_descendant = artifact.nodes.iter().enumerate().find_map(|(descendant, node)| {
        let mut parent = node.parent;
        let mut is_descendant = false;
        while let Some(candidate) = parent {
            if candidate == index {
                is_descendant = true;
                break;
            }
            parent = artifact.nodes.get(candidate).and_then(|node| node.parent);
        }
        (is_descendant
            && (node.style.background.a > 0
                || node.style.background_image.is_some()
                || node.style.padding_lengths().top > 0.0
                || node.style.padding_lengths().right > 0.0
                || node.style.padding_lengths().bottom > 0.0
                || node.style.padding_lengths().left > 0.0
                || node.style.border_width > 0.0
                || node.style.border_top_width.is_some_and(|width| width > 0.0)
                || node.style.border_right_width.is_some_and(|width| width > 0.0)
                || node.style.border_bottom_width.is_some_and(|width| width > 0.0)
                || node.style.border_left_width.is_some_and(|width| width > 0.0)))
            .then(|| artifact.rect_by_index.get(descendant).copied().flatten())
            .flatten()
    });
    let direct_parent = artifact.nodes.get(index).and_then(|node| node.parent);
    let mut line_parent = (has_inline_border || decorated_descendant.is_some())
        .then_some(direct_parent)
        .flatten();
    let mut line_rect = None;
    while let Some(parent_index) = line_parent {
        let parent = artifact.nodes.get(parent_index);
        let is_line_row = parent
            .and_then(|node| node.style.custom_properties.as_ref())
            .is_some_and(|properties| {
                properties.contains_key("--w3cos-internal-anonymous-block-line")
            });
        if is_line_row {
            line_rect = artifact.rect_by_index.get(parent_index).copied().flatten();
            break;
        }
        line_parent = parent.and_then(|node| node.parent);
    }
    if line_rect.is_none() {
        line_rect = direct_parent
            .and_then(|parent_index| artifact.rect_by_index.get(parent_index).copied().flatten());
    }
    if let Some(parent_rect) = line_rect {
        let top_delta = union.y - parent_rect.y;
        if top_delta.abs() <= 2.0 && parent_rect.height > 0.0 {
            let bottom = (union.y + union.height).max(parent_rect.y + parent_rect.height);
            union.y = parent_rect.y;
            union.height = bottom - union.y;
        }
    }
    if decorated_descendant.is_some() {
        return union;
    }
    for (descendant, node) in artifact.nodes.iter().enumerate() {
        let mut parent = node.parent;
        let mut is_descendant = false;
        while let Some(candidate) = parent {
            if candidate == index {
                is_descendant = true;
                break;
            }
            parent = artifact.nodes.get(candidate).and_then(|node| node.parent);
        }
        // Mixed-size descendants contribute to the line box, not to the
        // ancestor's own em-sized background. Same-size continuation fragments
        // still extend the decoration across wrapped lines.
        if !is_descendant || (node.style.font_size - style.font_size).abs() > 0.01 {
            continue;
        }
        let Some(descendant_rect) = artifact.rect_by_index.get(descendant).copied().flatten() else {
            continue;
        };
        let max_x = (union.x + union.width).max(descendant_rect.x + descendant_rect.width);
        let max_y = (union.y + union.height).max(descendant_rect.y + descendant_rect.height);
        union.x = union.x.min(descendant_rect.x);
        union.y = union.y.min(descendant_rect.y);
        union.width = max_x - union.x;
        union.height = max_y - union.y;
    }
    union
}

fn inline_has_content(artifact: &crate::paint_artifact::PaintArtifact, index: usize) -> bool {
    // Paint nodes are in preorder. A descendant's parent stays at or after
    // this index; the first following sibling ends the contiguous subtree.
    artifact
        .nodes
        .iter()
        .skip(index + 1)
        .take_while(|node| node.parent.is_some_and(|parent| parent >= index))
        .any(|node| match &node.kind {
            ComponentKind::Text { content } => !content.is_empty(),
            ComponentKind::Root | ComponentKind::Row | ComponentKind::Column | ComponentKind::Box => {
                node.style.display == Display::InlineBlock
            }
            _ => true,
        })
}

fn inline_background_fragment_width(
    fragment: LayoutRect, content: &str, typeface: &Typeface, style: &Style,
) -> f32 {
    if style.white_space == w3cos_std::style::WhiteSpace::Pre
        && content.contains('\t') && !content.contains(['\r', '\n', '\u{2028}'])
    {
        // Preserved tabs have already been resolved against their block's
        // stops and line offset. Remeasuring from zero changes decoration.
        fragment.width
    } else {
        measure_skia_text_advance(content, typeface, style)
    }
}

fn inline_text_is_in_first_strut(text: LayoutRect, line: LayoutRect, strut_height: f32) -> bool {
    // Continuation text already has a line-local origin from layout. The
    // anonymous block may enclose several lines; centering the glyph in its
    // whole height would add their leading for a second time.
    strut_height <= 0.0 || text.y < line.y + strut_height - 0.01
}

fn top_aligned_inline(style: &Style) -> bool {
    // Layout already positions this principal em/edge box at the line top.
    // Its decoration must not snap to a parent block or absorb the strut.
    style.custom_properties.as_ref().is_some_and(|properties| {
            properties
                .get("--w3cos-internal-vertical-align-keyword")
                .is_some_and(|value| value == "top")
        })
}

fn render_node_with_line_context(
    canvas: &Canvas,
    client_index: usize,
    rect: LayoutRect,
    kind: &ComponentKind,
    style: &Style,
    typeface: &Typeface,
    metrics_font: &fontdue::Font,
    text_input_value: Option<&str>,
    focused: bool,
    bake_compositor_props: bool,
    line_context: Option<crate::paint_artifact::InlineLineContext>,
    inline_decoration_line_box: Option<LayoutRect>,
    inline_text_leading: bool,
    inline_has_content: bool,
    extra_inline_background_rects: &[(LayoutRect, String)],
    shaped_fragment: Option<&crate::inline_shaping::Fragment>,
) {
    render_node_with_applied_decorations(canvas, client_index, rect, kind, style,
        typeface, metrics_font, text_input_value, focused, bake_compositor_props,
        line_context, inline_decoration_line_box, inline_text_leading, inline_has_content,
        extra_inline_background_rects, shaped_fragment, &[], None);
}

fn paint_html_auto_control(canvas: &Canvas, rect: LayoutRect, kind: &ComponentKind, style: &Style) -> bool {
    let part = style.custom_properties.as_ref()
        .and_then(|p| p.get("--w3cos-internal-html-control-appearance"))
        .map(String::as_str);
    let button = match (part, kind) {
        (Some("button"), ComponentKind::Button { .. }) => true,
        (Some("textfield"), ComponentKind::TextInput { .. }) => false,
        _ => return false,
    };
    // NativeThemeBase receives a snapped integer control rectangle, not CSS
    // border edges. Its normal/light theme paints a1px stroke inside that box.
    let bounds = skia_safe::Rect::new(rect.x.round() + 0.5, rect.y.round() + 0.5,
        (rect.x + rect.width).round() - 0.5, (rect.y + rect.height).round() - 0.5);
    let fill = if button { w3cos_std::Color::rgb(239, 239, 239) } else { w3cos_std::Color::WHITE };
    let mut paint = color_paint(fill, style.opacity);
    paint.set_anti_alias(button);
    if !analytic_round_rect::draw(canvas, bounds, 2.0, &paint) {
        canvas.draw_round_rect(bounds, 2.0, 2.0, &paint);
    }
    paint.set_color(to_skia_color(w3cos_std::Color::rgb(118, 118, 118), style.opacity))
        .set_style(paint::Style::Stroke).set_stroke_width(1.0);
    if !analytic_round_rect::draw(canvas, bounds, 2.0, &paint) {
        canvas.draw_round_rect(bounds, 2.0, 2.0, &paint);
    }
    true
}

fn render_node_with_applied_decorations(
    canvas: &Canvas,
    client_index: usize,
    rect: LayoutRect,
    kind: &ComponentKind,
    style: &Style,
    typeface: &Typeface,
    metrics_font: &fontdue::Font,
    text_input_value: Option<&str>,
    focused: bool,
    bake_compositor_props: bool,
    line_context: Option<crate::paint_artifact::InlineLineContext>,
    inline_decoration_line_box: Option<LayoutRect>,
    inline_text_leading: bool,
    inline_has_content: bool,
    extra_inline_background_rects: &[(LayoutRect, String)],
    shaped_fragment: Option<&crate::inline_shaping::Fragment>,
    applied_decorations: &[crate::paint_artifact::AppliedTextDecoration<'_>],
    terminal_inline_padding: Option<LayoutRect>,
) {
    let transform = if bake_compositor_props {
        style.transform
    } else {
        Transform2D::IDENTITY
    };
    let line_context = line_context.map(|mut context| {
        for line in [&mut context.line_box, &mut context.first_line_box] {
            line.x = rect.x + (line.x - rect.x) * transform.scale_x + transform.translate_x;
            line.width *= transform.scale_x;
        }
        context
    });
    let rect = LayoutRect {
        x: rect.x + transform.translate_x,
        y: rect.y + transform.translate_y,
        width: rect.width * transform.scale_x,
        height: rect.height * transform.scale_y,
    };
    let inline_decoration_line_box = inline_decoration_line_box.map(|mut line| {
        line.y += if bake_compositor_props {
            style.transform.translate_y
        } else {
            0.0
        };
        line.height *= if bake_compositor_props {
            style.transform.scale_y
        } else {
            1.0
        };
        line
    });
    let rect = if matches!(kind, ComponentKind::Text { .. })
        && line_context.is_none()
        && inline_text_leading
        // Explicit line-height has already positioned the font box around
        // the resolved baseline. A taller ancestor strut must not add leading
        // again at paint time (including when the inline has min-height).
        && style.line_height_is_normal
        && !top_aligned_inline(style)
    {
        inline_decoration_line_box
            .filter(|line| line.height > rect.height + 0.01)
            .map(|line| LayoutRect {
                y: rect.y + (line.height - rect.height) * 0.5,
                ..rect
            })
            .unwrap_or(rect)
    } else {
        rect
    };
    let rect = crate::paint_artifact::table_grid_paint_rect(style, rect);
    let plain_background = plain_inline_background(style);
    let top_aligned_inline = top_aligned_inline(style);
    let mut inline_decoration_rect = if style.display == Display::Inline
        && matches!(kind, ComponentKind::Row | ComponentKind::Box)
        && style.position == w3cos_std::style::Position::Static
        && (style.background.a > 0
            || style.padding_lengths().top > 0.0
            || style.padding_lengths().right > 0.0
            || style.padding_lengths().bottom > 0.0
            || style.padding_lengths().left > 0.0
            || style.border_width > 0.0
            || style.border_top_width.is_some_and(|width| width > 0.0)
            || style.border_right_width.is_some_and(|width| width > 0.0)
            || style.border_bottom_width.is_some_and(|width| width > 0.0)
            || style.border_left_width.is_some_and(|width| width > 0.0))
    {
        let vertical_edges = style.padding_lengths().top
            + style.padding_lengths().bottom
            + style.border_top_width.unwrap_or(style.border_width)
            + style.border_bottom_width.unwrap_or(style.border_width);
        let normal_side_only_border = style.line_height_is_normal
            && vertical_edges == 0.0
            && (style.border_left_width.unwrap_or(style.border_width) > 0.0
                || style.border_right_width.unwrap_or(style.border_width) > 0.0);
        // The authored inline's computed line-height can be smaller than the
        // actual line box when a nested fragment contributes half-leading.
        // Paint decorations against that line box so borders/backgrounds
        // cover the same vertical extent as the browser's inline formatting
        // context.
        let authored_line_height = crate::layout::inline_style_line_height(&style);
        let line_height = line_context
            .as_ref()
            .map(|context| context.line_box.height + vertical_edges)
            .filter(|height| height.is_finite() && *height > 0.0)
            .unwrap_or(rect.height.max(authored_line_height));
        let split_inline_edge = style
            .custom_properties
            .as_ref()
            .is_some_and(|properties| {
                properties.contains_key("--w3cos-internal-split-inline-edge")
            });
        let empty_split_inline_edge = style
            .custom_properties
            .as_ref()
            .and_then(|properties| properties.get("--w3cos-internal-split-inline-edge"))
            .is_some_and(|value| value == "empty");
        let line_height = if split_inline_edge {
            style.font_size + vertical_edges
        } else {
            line_height
        };
        let centered = inline_decoration_line_box
            .filter(|line| !normal_side_only_border && !top_aligned_inline && line.height > rect.height + 0.01)
            .map(|line| LayoutRect {
                y: line.y + (line.height - rect.height) * 0.5,
                ..rect
            })
            .unwrap_or(rect);
        LayoutRect {
            // Non-empty normal inlines and their split fragments share the
            // same device-pixel border origin. Empty inlines keep the existing
            // rounding rule; their em box is not a non-empty fragment.
            y: (split_inline_edge
                || (style.line_height_is_normal
                    && !top_aligned_inline
                    && vertical_edges > 0.0
                    && inline_has_content))
                .then(|| {
                    if empty_split_inline_edge || (split_inline_edge && !inline_has_content) {
                        centered.y.round()
                    } else {
                        centered.y.floor()
                    }
                })
                .unwrap_or(centered.y),
            // The measured non-empty fragment already includes its painted
            // height; the font-size fallback would add an extra border row.
            height: if plain_background
                || top_aligned_inline
                || normal_side_only_border
                || (split_inline_edge && !empty_split_inline_edge)
            {
                centered.height
            } else {
                centered.height.max(line_height)
            },
            x: centered.x,
            width: centered.width,
        }
    } else {
        rect
    };
    if transform.rotate_deg != 0.0 {
        canvas.rotate(
            transform.rotate_deg,
            Some((rect.x + rect.width * 0.5, rect.y + rect.height * 0.5).into()),
        );
    }

    if let Some(shadow) = style.box_shadow.filter(|shadow| !shadow.inset) {
        let spread = shadow.spread_radius;
        let shadow_rect = LayoutRect {
            x: rect.x + shadow.offset_x - spread,
            y: rect.y + shadow.offset_y - spread,
            width: rect.width + spread * 2.0,
            height: rect.height + spread * 2.0,
        };
        let mut paint = color_paint(shadow.color, style.opacity);
        if shadow.blur_radius > 0.0 {
            paint.set_mask_filter(MaskFilter::blur(
                BlurStyle::Normal,
                shadow.blur_radius * 0.5,
                false,
            ));
        }
        draw_rounded_rect(
            canvas,
            shadow_rect,
            style.border_corner_radii().map(|radius| radius + spread),
            &paint,
        );
    }

    let themed_control = paint_html_auto_control(canvas, rect, kind, style);
    let bg = style.background;
    // A plain inline container may have an em-sized layout rect while its
    // background spans the anonymous line. Positioned inline wrappers can
    // fragment around block descendants and keep their own paint geometry.
    let background_box = if style.display == Display::Inline
        && matches!(kind, ComponentKind::Row | ComponentKind::Box)
        && bg.a > 0
    {
        let mut background_box = inline_decoration_rect;
        if terminal_inline_padding.is_none() && let Some((first, content)) = extra_inline_background_rects
            .iter()
            .min_by(|(left, _), (right, _)| left.y.total_cmp(&right.y))
        {
            let single_plain_line = plain_background && extra_inline_background_rects
                .iter().all(|(fragment, _)| (fragment.y - first.y).abs() < 0.01);
            if !single_plain_line {
                background_box.x = first.x;
                background_box.width = inline_background_fragment_width(*first, content, typeface, style);
            }
        }
        background_box
    } else {
        rect
    };
    let mut background_rects =
        if themed_control || (style.display == Display::Inline && matches!(kind, ComponentKind::Text { .. })) {
            Vec::new() // Inline decorations follow shaped fragments below.
        } else {
            crate::paint_artifact::box_background_paint_rects(style, background_box)
        };
    if let Some(terminal) = terminal_inline_padding {
        for fragment in &mut background_rects {
            fragment.width = (fragment.width - terminal.width).max(0.0);
        }
        background_rects.push(terminal);
    }
    for background_rect in background_rects.iter().copied() {
        if bg.a > 0 {
            draw_rounded_rect(
                canvas,
                background_rect,
                style.border_corner_radii(),
                &color_paint(bg, style.opacity),
            );
        }
        if style.background_image.is_some() {
            draw_background_image(
                canvas,
                background_rect,
                rect,
                style.border_radius,
                style,
                style.opacity,
            );
        }
    }
    if bg.a > 0 && terminal_inline_padding.is_none() {
        for (fragment, content) in extra_inline_background_rects {
            let fragment = LayoutRect {
                width: inline_background_fragment_width(*fragment, content, typeface, style),
                ..*fragment
            };
            // The principal background may already paint the first text
            // fragment. Repainting it is invisible for opaque colors but
            // compounds alpha. Containment is safe only for square corners;
            // identical geometry has identical rounded coverage as well.
            let square = style.border_corner_radii().iter().all(|radius| *radius == 0.0);
            if background_rects.iter().any(|painted| *painted == fragment
                || (square && fragment.x >= painted.x && fragment.y >= painted.y
                    && fragment.x + fragment.width <= painted.x + painted.width
                    && fragment.y + fragment.height <= painted.y + painted.height))
            {
                continue;
            }
            draw_rounded_rect(
                canvas,
                fragment,
                style.border_corner_radii(),
                &color_paint(bg, style.opacity),
            );
        }
    }
    if !themed_control && !(style.display == Display::Inline && matches!(kind, ComponentKind::Text { .. })) {
        draw_box_border(canvas, inline_decoration_rect, style);
    }

    if style.custom_properties.as_ref().is_some_and(|p|
        p.get("--w3cos-internal-media-controls-layer").map(String::as_str) == Some("no-source")) {
        media_controls_paint::draw(canvas, rect, style);
    }

    match kind {
        ComponentKind::Text { .. } if crate::list_marker::inside_kind(style).is_some() => {
            let mut origin = text_paint_box(rect, style);
            if let Some(context) = line_context {
                origin.x = context.fragment_box(style, true, true).x;
            }
            if let Some(band) = style.custom_properties.as_ref()
                .and_then(|p| p.get("--w3cos-internal-float-line-bands"))
                .and_then(|bands| bands.split(';').next()) {
                let values = band.split_ascii_whitespace()
                    .filter_map(|v| v.parse::<f32>().ok()).collect::<Vec<_>>();
                if values.len() == 3 {
                    origin.x = text_paint_box(rect, style).x + values[0]
                        + style.margin_lengths().left;
                }
            }
            let symbol = crate::list_marker::inside_symbol_rect(origin, style);
            let kind = crate::list_marker::inside_kind(style).unwrap();
            let mut paint = color_paint(style.color, style.opacity);
            if kind == "square" { canvas.draw_rect(to_rect(symbol), &paint); }
            else if kind == "circle" {
                paint.set_style(skia_safe::paint::Style::Stroke).set_stroke_width(1.0);
                canvas.draw_oval(to_rect(symbol), &paint);
            } else {
                draw_rounded_rect(canvas, symbol, [style.font_size; 4], &paint);
            }
        }
        ComponentKind::Text { content } => {
            let own_decoration = (shaped_fragment.is_some()
                && style.text_decoration != w3cos_std::style::TextDecoration::None)
                .then(|| crate::paint_artifact::AppliedTextDecoration {
                    style: std::borrow::Cow::Borrowed(style), baseline_shift: 0.0 });
            for decoration in applied_decorations.iter().chain(own_decoration.iter()).filter(|decoration|
                decoration.style.text_decoration != w3cos_std::style::TextDecoration::LineThrough)
            {
                text_decoration::paint_applied_in_rect(canvas, rect, content, style,
                    typeface, metrics_font, line_context, shaped_fragment, decoration);
            }
            if let Some(fragment) = shaped_fragment {
                // The ordinary inline text path paints decorations as it
                // shapes each line. Shared glyph slices bypass that path,
                // but retain their own solid background box.
                if bg.a > 0 {
                    draw_rounded_rect(
                        canvas,
                        rect,
                        style.border_corner_radii(),
                        &color_paint(bg, style.opacity),
                    );
                }
                fragment.paint(canvas, rect, style);
            } else {
                draw_text_in_rect_with_line_context(
                    canvas,
                    rect,
                    content,
                    style,
                    typeface,
                    metrics_font,
                    line_context,
                );
            }
            for decoration in applied_decorations.iter().chain(own_decoration.iter()).filter(|decoration|
                decoration.style.text_decoration == w3cos_std::style::TextDecoration::LineThrough)
            {
                text_decoration::paint_applied_in_rect(canvas, rect, content, style,
                    typeface, metrics_font, line_context, shaped_fragment, decoration);
            }
        }
        ComponentKind::Button { label } => {
            draw_centered_text(canvas, rect, label, style, typeface, metrics_font);
        }
        ComponentKind::TextInput {
            value,
            placeholder,
            secure,
        } => {
            let value = text_input_value.unwrap_or(value);
            let masked_value = secure.then(|| "•".repeat(value.chars().count()));
            let text = if value.is_empty() {
                placeholder.as_str()
            } else if let Some(masked) = masked_value.as_deref() {
                masked
            } else {
                value
            };
            let color = if value.is_empty() {
                w3cos_std::color::Color::rgb(107, 114, 128)
            } else {
                style.color
            };
            let content = text_content_box(rect, style);
            let ink = measure_skia_text_ink_bounds(
                text,
                style.font_size,
                typeface,
                style.font_weight,
                Some(style),
            );
            let y = content.y + (content.height - ink.height) * 0.5 - ink.top;
            let save = canvas.save();
            canvas.clip_rect(to_rect(content), None, Some(false));
            let text_width = draw_text_line(
                canvas,
                content.x,
                y,
                text,
                style.font_size,
                color,
                style.opacity,
                typeface,
                style,
            );
            if focused {
                let cursor_x = content.x + if value.is_empty() { 0.0 } else { text_width };
                let cursor_width = (style.font_size * 0.1).max(2.0);
                let cursor_height = style.font_size.max(1.0).min(content.height);
                let cursor_y = content.y + (content.height - cursor_height) * 0.5;
                let cursor = LayoutRect {
                    x: cursor_x.min(content.x + content.width - cursor_width),
                    y: cursor_y,
                    width: cursor_width,
                    height: cursor_height,
                };
                canvas.draw_rect(to_rect(cursor), &color_paint(style.color, style.opacity));
            }
            canvas.restore_to_count(save);
        }
        ComponentKind::Image { src } => {
            draw_image(canvas, text_content_box(rect, style), src, style.opacity)
        }
        ComponentKind::Canvas { .. } => draw_canvas(canvas, client_index, rect, style.opacity),
        ComponentKind::SvgPath {
            commands,
            fill,
            stroke,
            stroke_width,
        } => draw_svg_path(
            canvas,
            rect,
            commands,
            *fill,
            *stroke,
            *stroke_width,
            style.opacity,
        ),
        ComponentKind::SvgDocument { source, .. } => {
            let rect = crate::svg_renderer::content_box(rect, style);
            if let Some(raster) = crate::svg_renderer::get_or_render_inline(
                source, rect.width.ceil().max(1.0) as u32, rect.height.ceil().max(1.0) as u32) {
                draw_decoded_image(canvas, rect, &raster, style.opacity, false, (false, false));
            }
        }
        ComponentKind::Root
        | ComponentKind::Column
        | ComponentKind::Row
        | ComponentKind::Box
        | ComponentKind::VirtualList { .. } => {}
    }
    draw_box_outline(canvas, inline_decoration_rect, style);
}

fn draw_box_outline(canvas: &Canvas, rect: LayoutRect, style: &Style) {
    use w3cos_std::style::{OutlineStyle, BorderLineStyle};
    let width = style.outline_width;
    if !width.is_finite() || width <= 0.0 || style.outline_color.a == 0
        || matches!(style.display,Display::None | Display::Contents
            | Display::TableColumn | Display::TableColumnGroup)
        || style.outline_style == OutlineStyle::None { return; }
    let band = |inner: f32, outer: f32, dashed: Option<(f32,f32)>| {
        let center = (inner + outer) * 0.5;
        let mut paint = color_paint(style.outline_color,style.opacity);
        paint.set_style(paint::Style::Stroke);
        paint.set_stroke_width(outer-inner);
        if let Some((on,off)) = dashed {
            paint.set_path_effect(skia_safe::PathEffect::dash(&[on,off],0.0));
        }
        draw_rounded_rect(canvas,LayoutRect { x:rect.x-center,y:rect.y-center,
            width:rect.width+center*2.0,height:rect.height+center*2.0 },
            style.border_corner_radii().map(|r| if r>0.0 {r+center} else {0.0}),&paint);
    };
    match style.outline_style {
        OutlineStyle::None => {},
        OutlineStyle::Solid => band(0.0,width,None),
        OutlineStyle::Dashed => band(0.0,width,Some((width*3.0,width*3.0))),
        OutlineStyle::Dotted => band(0.0,width,Some((width,width))),
        OutlineStyle::Double if width>=3.0 => {
            let stripe=(width/3.0).round();
            band(0.0,stripe,None);band(width-stripe,width,None);
        },
        OutlineStyle::Double => band(0.0,width,None),
        line_style => {
            let line_style=match line_style {
                OutlineStyle::Groove => BorderLineStyle::Groove,
                OutlineStyle::Ridge => BorderLineStyle::Ridge,
                OutlineStyle::Inset => BorderLineStyle::Inset,
                OutlineStyle::Outset => BorderLineStyle::Outset,
                _ => unreachable!(),
            };
            let border=Style { border_width:width,border_color:style.outline_color,
                border_styles:[Some(line_style);4],border_top_width:Some(width),
                border_right_width:Some(width),border_bottom_width:Some(width),
                border_left_width:Some(width),opacity:style.opacity,..Style::default() };
            draw_box_border(canvas,LayoutRect {x:rect.x-width,y:rect.y-width,
                width:rect.width+width*2.0,height:rect.height+width*2.0},&border);
        }
    }
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn render_node(
    canvas: &Canvas,
    client_index: usize,
    rect: LayoutRect,
    kind: &ComponentKind,
    style: &Style,
    typeface: &Typeface,
    metrics_font: &fontdue::Font,
    text_input_value: Option<&str>,
    focused: bool,
    bake_compositor_props: bool,
) {
    render_node_with_line_context(
        canvas,
        client_index,
        rect,
        kind,
        style,
        typeface,
        metrics_font,
        text_input_value,
        focused,
        bake_compositor_props,
        None,
        None,
        false,
        false,
        &[],
        None,
    );
}

thread_local! {
    static SOLID_BORDER_MITER: Option<skia_safe::RuntimeEffect> = skia_safe::RuntimeEffect::make_for_shader(r#"
        uniform float4 corner;
        uniform float4 sizes;
        uniform float4 horizontal_rgba;
        uniform float4 vertical_rgba;
        float sq(float v) { v=max(v,0); return v*v; }
        half4 main(float2 xy) {
            float2 q=(xy-corner.xy)*corner.zw;
            float w=sizes.x,h=sizes.y,d=h*q.x-w*q.y,e=(w+h)*.5;
            float coverage=d>=e?1:(d<=-e?0:
                clamp((sq(d+e)-sq(d+(h-w)*.5)-sq(d+(w-h)*.5)+sq(d-e))/(2*w*h),0,1));
            return half4(floor(mix(vertical_rgba,horizontal_rgba,coverage)+.5)/255);
        }
    "#,None).ok();
}

/// Integrate the shared diagonal over a device pixel once. Independently
/// rasterizing adjacent quads quantizes their complementary coverages and
/// biases midpoint colors. Keep unsupported transforms/fractional corner
/// extents on the ordinary path; this shader does not alter glyph/opacity AA.
fn draw_opaque_border_miter(canvas:&Canvas, rect:LayoutRect, corner:usize,
    width:f32,height:f32,horizontal:w3cos_std::Color,vertical:w3cos_std::Color) -> bool {
    if width<=0.0 || height<=0.0 || horizontal==vertical {return false;}
    let matrix=canvas.local_to_device_as_3x3();
    if !matrix.is_scale_translate() || matrix.scale_x()<=0.0 || matrix.scale_y()<=0.0 {return false;}
    let right=matches!(corner,1|2);
    let bottom=corner>=2;
    let bounds=Rect::from_xywh(if right {rect.x+rect.width-width} else {rect.x},
        if bottom {rect.y+rect.height-height} else {rect.y},width,height);
    let Some(bounds)=matrix.map_rect_scale_translate(bounds) else {return false;};
    if !bounds.is_finite() || [bounds.left,bounds.top,bounds.right,bounds.bottom]
        .iter().any(|edge|edge.fract()!=0.0) {return false;}
    SOLID_BORDER_MITER.with(|effect| {
        let Some(effect)=effect else {return false;};
        let values=[if right {bounds.right} else {bounds.left},if bottom {bounds.bottom} else {bounds.top},
            if right {-1.0} else {1.0},if bottom {-1.0} else {1.0},
            bounds.width(),bounds.height(),0.0,0.0,
            horizontal.r as f32,horizontal.g as f32,horizontal.b as f32,255.0,
            vertical.r as f32,vertical.g as f32,vertical.b as f32,255.0];
        let bytes=values.iter().flat_map(|value|value.to_ne_bytes()).collect::<Vec<_>>();
        let Some(shader)=effect.make_shader(skia_safe::Data::new_copy(&bytes),&[],None) else {return false;};
        let mut paint=Paint::default();
        paint.set_anti_alias(false).set_color(Color::WHITE).set_shader(shader)
            .set_blend_mode(skia_safe::BlendMode::Src);
        let save=canvas.save();
        canvas.reset_matrix();
        canvas.draw_rect(bounds,&paint);
        canvas.restore_to_count(save);
        true
    })
}

fn draw_box_border(canvas: &Canvas, rect: LayoutRect, style: &Style) {
    let has_edge_border = style.border_top_width.is_some()
        || style.border_right_width.is_some()
        || style.border_bottom_width.is_some()
        || style.border_left_width.is_some()
        || style.border_top_color.is_some()
        || style.border_right_color.is_some()
        || style.border_bottom_color.is_some()
        || style.border_left_color.is_some()
        || (style.border_collapse
            && matches!(
                style.display,
                Display::TableColumn | Display::TableColumnGroup | Display::TableCell
            ));
    let has_edge_border = has_edge_border
        || (0..4).any(|side| crate::border_paint::is_three_dimensional(style, side));
    if !has_edge_border && style.border_width > 0.0 && style.border_color.a > 0 {
        let mut border = color_paint(style.border_color, style.opacity);
        border.set_style(paint::Style::Stroke);
        border.set_stroke_width(style.border_width);
        let inset = style.border_width * 0.5;
        draw_rounded_rect(
            canvas,
            LayoutRect {
                x: rect.x + inset,
                y: rect.y + inset,
                width: (rect.width - style.border_width).max(0.0),
                height: (rect.height - style.border_width).max(0.0),
            },
            style
                .border_corner_radii()
                .map(|radius| (radius - inset).max(0.0)),
            &border,
        );
    } else if has_edge_border {
        let widths = [
            style.border_top_width.unwrap_or(style.border_width),
            style.border_right_width.unwrap_or(style.border_width),
            style.border_bottom_width.unwrap_or(style.border_width),
            style.border_left_width.unwrap_or(style.border_width),
        ];
        let colors = [
            style.border_top_color.unwrap_or(style.border_color),
            style.border_right_color.unwrap_or(style.border_color),
            style.border_bottom_color.unwrap_or(style.border_color),
            style.border_left_color.unwrap_or(style.border_color),
        ];
        let edges = crate::paint_artifact::border_edge_paint_rects(style, rect, widths);
        if (0..4).any(|side| crate::border_paint::is_three_dimensional(style, side)) {
            let layer_paint = color_paint(w3cos_std::Color::rgb(255, 255, 255), style.opacity);
            canvas.save_layer(&SaveLayerRec::default().paint(&layer_paint));
            for layer in crate::border_paint::three_dimensional_layers(style, rect, widths, colors)
            {
                let mut builder = PathBuilder::new();
                for points in layer.polygons {
                    builder.move_to(points[0]);
                    for point in &points[1..] {
                        builder.line_to(*point);
                    }
                    builder.close();
                }
                let mut color = layer.color;
                if layer.shadow {
                    color.a = 255;
                }
                let mut paint = color_paint(color, 1.0);
                if layer.shadow {
                    paint.set_blend_mode(skia_safe::BlendMode::SrcATop);
                }
                canvas.draw_path(&builder.detach(), &paint);
            }
            canvas.restore();
        } else if !style.border_collapse
            && colors.iter().all(|color| color.a == 255)
            && style.border_corner_radii().iter().all(|radius| *radius == 0.0)
            && style.border_styles.iter().all(|line| matches!(line,
                None | Some(w3cos_std::style::BorderLineStyle::Solid)))
        {
            // Opaque adjacent sides must meet at a miter. Fill the side
            // underpaint first, then overlay top/bottom quads: AA on a shared
            // diagonal blends the two border colors, not an uncovered backdrop.
            // Apply element opacity once, after resolving those opaque joins.
            let snap = |value:f32| (value + 0.5).floor();
            let left = snap(rect.x);
            let top = snap(rect.y);
            let right = snap(rect.x + rect.width);
            let bottom = snap(rect.y + rect.height);
            let rect = LayoutRect {x:left,y:top,width:(right-left).max(0.0),height:(bottom-top).max(0.0)};
            let edges = crate::paint_artifact::border_edge_paint_rects(style, rect, widths);
            let save = canvas.save_count();
            if style.opacity != 1.0 {
                let opacity = color_paint(w3cos_std::Color::rgb(255,255,255),style.opacity);
                canvas.save_layer(&SaveLayerRec::default().paint(&opacity));
            }
            for side in [0,2,3,1] {
                if widths[side] > 0.0 {
                    draw_round_rect(canvas,edges[side],0.0,&color_paint(colors[side],1.0));
                }
            }
            let outer=[(left,top),(right,top),(right,bottom),(left,bottom)];
            let inner=[(left+widths[3],top+widths[0]),(right-widths[1],top+widths[0]),
                (right-widths[1],bottom-widths[2]),(left+widths[3],bottom-widths[2])];
            for side in [0,2] {
                if widths[side] <= 0.0 {continue;}
                let next=(side+1)%4;
                let mut path=PathBuilder::new();
                path.move_to(outer[side]).line_to(outer[next]).line_to(inner[next])
                    .line_to(inner[side]).close();
                canvas.draw_path(&path.detach(),&color_paint(colors[side],1.0));
            }
            if widths[1]+widths[3]<=rect.width && widths[0]+widths[2]<=rect.height {
                for corner in 0..4 {
                    let horizontal=if corner<2 {0} else {2};
                    let vertical=if matches!(corner,1|2) {1} else {3};
                    draw_opaque_border_miter(canvas,rect,corner,widths[vertical],widths[horizontal],
                        colors[horizontal],colors[vertical]);
                }
            }
            canvas.restore_to_count(save);
        } else {
            for ((edge, width), color) in edges.into_iter().zip(widths).zip(colors) {
                if width > 0.0 && color.a > 0 {
                    draw_round_rect(canvas, edge, 0.0, &color_paint(color, style.opacity));
                }
            }
        }
    }
}

fn draw_svg_path(
    canvas: &Canvas,
    rect: LayoutRect,
    commands: &[SvgPathCommand],
    fill: w3cos_std::color::Color,
    stroke: Option<w3cos_std::color::Color>,
    stroke_width: f32,
    opacity: f32,
) {
    let mut builder = PathBuilder::new();
    for command in commands {
        match *command {
            SvgPathCommand::MoveTo(x, y) => {
                builder.move_to((rect.x + x, rect.y + y));
            }
            SvgPathCommand::LineTo(x, y) => {
                builder.line_to((rect.x + x, rect.y + y));
            }
            SvgPathCommand::QuadTo(cx, cy, x, y) => {
                builder.quad_to((rect.x + cx, rect.y + cy), (rect.x + x, rect.y + y));
            }
            SvgPathCommand::CubicTo(c1x, c1y, c2x, c2y, x, y) => {
                builder.cubic_to(
                    (rect.x + c1x, rect.y + c1y),
                    (rect.x + c2x, rect.y + c2y),
                    (rect.x + x, rect.y + y),
                );
            }
            SvgPathCommand::Close => {
                builder.close();
            }
        }
    }
    let path = builder.detach();
    if fill.a > 0 {
        canvas.draw_path(&path, &color_paint(fill, opacity));
    }
    if let Some(stroke) = stroke.filter(|color| color.a > 0)
        && stroke_width > 0.0
    {
        let mut paint = color_paint(stroke, opacity);
        paint.set_style(paint::Style::Stroke);
        paint.set_stroke_width(stroke_width);
        canvas.draw_path(&path, &paint);
    }
}

fn effect_path(
    artifact: Option<&PaintArtifact>,
    client_index: usize,
    bake_compositor_props: bool,
) -> Vec<usize> {
    let Some(artifact) = artifact else {
        return Vec::new();
    };
    let mut current = artifact
        .node_properties
        .get(client_index)
        .map(|properties| properties.effect)
        .unwrap_or_default();
    let mut path = Vec::new();
    while current != 0 {
        let Some(effect) = artifact.properties.effects.get(current) else {
            break;
        };
        if effect.filter.is_some()
            || (bake_compositor_props && (effect.opacity < 0.999 || effect.isolates_surface)) {
            path.push(current);
        }
        if effect.parent == current {
            break;
        }
        current = effect.parent;
    }
    path.reverse();
    path
}

fn small_blur_input_bounds(frame: &ReplayFrame<'_>, bake: bool) -> HashMap<usize, Rect> {
    let mut bounds = HashMap::<usize, Rect>::new();
    let Some(artifact) = frame.artifact else { return bounds; };
    let candidates: std::collections::HashSet<_> = artifact.properties.effects.iter().enumerate()
        .filter_map(|(index, effect)| effect.filter.as_deref().and_then(parse_css_filter)
            .filter(|chain| matches!(chain.ops.as_slice(), [FilterOp::Blur(sigma)] if *sigma > 0.0 && *sigma <= 1.0))
            .map(|_| index)).collect();
    if candidates.is_empty() { return bounds; }
    let mut unsupported = std::collections::HashSet::new();
    for &(index, rect, kind, style) in frame.nodes {
        let path = effect_path(frame.artifact, index, bake);
        for (position, &effect) in path.iter().enumerate() {
            if !candidates.contains(&effect) { continue; }
            // Keep the established general filter path for geometry requiring
            // transformed/fragmented or expanded ink bounds, until those bounds
            // are derived in the same coordinate system as the filter input.
            if frame.scale_factor != 1.0 || style.transform != Transform2D::IDENTITY
                || style.box_shadow.is_some() || style.outline_width > 0.0
                || path[position+1..].iter().any(|id| artifact.properties.effects[*id].filter.is_some())
                || artifact.column_fragments.get(index).is_some_and(|fragments| !fragments.is_empty()) {
                unsupported.insert(effect);
                continue;
            }
            let paints_content = !matches!(kind, ComponentKind::Row | ComponentKind::Column)
                || style.background.a > 0 || style.border_width > 0.0
                || style.background_image.as_deref().is_some_and(|image|
                    !image.trim().eq_ignore_ascii_case("none"));
            if !paints_content || style.opacity <= 0.0 || rect.width <= 0.0 || rect.height <= 0.0 {
                continue;
            }
            let mut rect = rect;
            if let Some((x, y, _)) = frame.scroll_info.get(index).copied().flatten() {
                rect.x -= x; rect.y -= y;
            }
            if bake {
                let (x, y) = artifact.viewport_scroll_for(index);
                rect.x -= x; rect.y -= y;
            }
            let logical = Rect::new(rect.x.floor(), rect.y.floor(),
                (rect.x + rect.width).ceil(), (rect.y + rect.height).ceil());
            bounds.entry(effect).and_modify(|existing| {
                *existing = Rect::new(existing.left().min(logical.left()), existing.top().min(logical.top()),
                    existing.right().max(logical.right()), existing.bottom().max(logical.bottom()));
            }).or_insert(logical);
        }
    }
    bounds.retain(|effect, _| !unsupported.contains(effect));
    bounds
}

fn clip_path(
    artifact: Option<&PaintArtifact>,
    client_index: usize,
    recording_layer: bool,
) -> Vec<LayoutRect> {
    let Some(artifact) = artifact else {
        return Vec::new();
    };
    // A box that clips its own overflow paints its background and border under
    // the chain it inherited, not under the overflow clip it hands to its
    // contents (CSS 2.1 11.1.1 clips "the contents of an element"). The
    // artifact records both chains; this is the one the box itself uses.
    let mut current = artifact
        .self_clip
        .get(client_index)
        .copied()
        .unwrap_or_default();
    let mut path = Vec::new();
    while current != 0 {
        let Some(clip) = artifact.properties.clips.get(current) else {
            break;
        };
        let scrollport_clip = recording_layer && {
            let mut scroll = artifact.node_properties[client_index].scroll;
            let mut found = false;
            while scroll != 0 {
                let Some(node) = artifact.properties.scrolls.get(scroll) else {
                    break;
                };
                if node.clip == Some(current) {
                    found = true;
                    break;
                }
                if node.parent == scroll {
                    break;
                }
                scroll = node.parent;
            }
            found
        };
        // The scrollport clip is applied after the layer's scroll transform.
        // Baking it into a reusable picture would permanently discard content
        // that starts below the viewport but later scrolls into view.
        if let Some(rect) = clip.rect.filter(|_| !scrollport_clip) {
            path.push(rect);
        }
        if clip.parent == current {
            break;
        }
        current = clip.parent;
    }
    path.reverse();
    path
}

fn draw_image(canvas: &Canvas, rect: LayoutRect, src: &str, opacity: f32) {
    if src.is_empty() {
        // An img without a resource paints a one-pixel frame inside its
        // content rectangle; this does not change its CSS box dimensions.
        let frame=Style { border_width:1.0,
            border_color:w3cos_std::Color::rgb(192,192,192),opacity,..Style::default() };
        draw_box_border(canvas,rect,&frame);
        return;
    }
    // Untransformed replaced images own their pixel coverage just like a
    // zero-radius CSS background. Browser rasterizers snap those outer edges
    // instead of blending the image with the page at fractional layout
    // coordinates; interpolation remains inside the destination rectangle.
    draw_image_with_edge_antialiasing(canvas, rect, src, opacity, false, (true, true));
}

fn draw_image_with_edge_antialiasing(
    canvas: &Canvas,
    rect: LayoutRect,
    src: &str,
    opacity: f32,
    anti_alias: bool,
    snap_axes: (bool, bool),
) {
    let Some(decoded) = crate::image_loader::get_or_load(src) else {
        return;
    };
    draw_decoded_image(canvas, rect, &decoded, opacity, anti_alias, snap_axes);
}

fn draw_decoded_image(
    canvas: &Canvas,
    rect: LayoutRect,
    decoded: &crate::image_loader::DecodedImage,
    opacity: f32,
    anti_alias: bool,
    snap_axes: (bool, bool),
) {
    let viewport = decoded.svg_viewport(rect.width, rect.height);
    let mut rect = rect;
    if viewport.is_none() && !anti_alias {
        if snap_axes.0 {
            let right = (rect.x + rect.width).round();
            rect.x = rect.x.round();
            rect.width = right - rect.x;
        }
        if snap_axes.1 {
            let bottom = (rect.y + rect.height).round();
            rect.y = rect.y.round();
            rect.height = bottom - rect.y;
        }
    }
    let decoded = viewport.as_ref().unwrap_or(decoded);
    let Some(image) = cached_skia_image(decoded) else {
        return;
    };
    let mut paint = Paint::default();
    paint.set_anti_alias(anti_alias);
    paint.set_alpha_f(opacity.clamp(0.0, 1.0));
    // External SVGs already resolve vector coverage at the used CSS viewport.
    // Raster images still need interpolation within their crisp outer edges.
    let sampling = if viewport.is_some() {
        skia_safe::SamplingOptions::default()
    } else {
        skia_safe::SamplingOptions::from(skia_safe::FilterMode::Linear)
    };
    canvas.draw_image_rect_with_sampling_options(image, None, to_rect(rect), sampling, &paint);
}

fn draw_canvas(canvas: &Canvas, client_index: usize, rect: LayoutRect, opacity: f32) {
    let Some(snapshot) = crate::canvas2d::surface_snapshot(client_index) else {
        return;
    };
    // Reuse the Skia image while the published Arc identity is unchanged.
    // Mutating 2D APIs force a fresh Arc on the next publish.
    let decoded = crate::image_loader::DecodedImage {
        width: snapshot.width,
        height: snapshot.height,
        intrinsic_width: snapshot.width,
        intrinsic_height: snapshot.height,
        svg_intrinsic_size: None,
        svg_source: None,
        data: snapshot.pixels,
    };
    draw_decoded_image(canvas, rect, &decoded, opacity, true, (false, false));
}

fn skia_filter_chain(chain: &FilterChain) -> Option<ImageFilter> {
    let mut input = None;
    for op in &chain.ops {
        input = match op {
            FilterOp::Blur(radius) => image_filters::blur(
                (*radius, *radius),
                None,
                input,
                image_filters::CropRect::NO_CROP_RECT,
            ),
            FilterOp::DropShadow(shadow) => image_filters::drop_shadow(
                (shadow.offset_x, shadow.offset_y),
                (shadow.blur_radius * 0.5, shadow.blur_radius * 0.5),
                Color4f::new(
                    shadow.color.r as f32 / 255.0,
                    shadow.color.g as f32 / 255.0,
                    shadow.color.b as f32 / 255.0,
                    shadow.color.a as f32 / 255.0,
                ),
                None,
                input,
                image_filters::CropRect::NO_CROP_RECT,
            ),
            color_op => {
                let matrix = css_color_matrix(color_op)?;
                image_filters::color_filter(
                    color_filters::matrix_row_major(&matrix, None),
                    input,
                    image_filters::CropRect::NO_CROP_RECT,
                )
            }
        };
    }
    input
}

fn css_color_matrix(op: &FilterOp) -> Option<[f32; 20]> {
    let identity = || {
        [
            1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0,
            0.0, 1.0, 0.0,
        ]
    };
    match *op {
        FilterOp::Brightness(value) => Some([
            value, 0.0, 0.0, 0.0, 0.0, 0.0, value, 0.0, 0.0, 0.0, 0.0, 0.0, value, 0.0, 0.0, 0.0,
            0.0, 0.0, 1.0, 0.0,
        ]),
        FilterOp::Contrast(value) => {
            // Skia's high-level color-filter matrix operates on normalized
            // color components, so CSS's midpoint is 0.5 rather than 127.5.
            let offset = 0.5 * (1.0 - value);
            Some([
                value, 0.0, 0.0, 0.0, offset, 0.0, value, 0.0, 0.0, offset, 0.0, 0.0, value, 0.0,
                offset, 0.0, 0.0, 0.0, 1.0, 0.0,
            ])
        }
        FilterOp::Grayscale(amount) => {
            let t = amount.clamp(0.0, 1.0);
            Some([
                1.0 - 0.787 * t,
                0.715 * t,
                0.072 * t,
                0.0,
                0.0,
                0.213 * t,
                1.0 - 0.285 * t,
                0.072 * t,
                0.0,
                0.0,
                0.213 * t,
                0.715 * t,
                1.0 - 0.928 * t,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                1.0,
                0.0,
            ])
        }
        FilterOp::Sepia(amount) => {
            let t = amount.clamp(0.0, 1.0);
            Some([
                1.0 - 0.607 * t,
                0.769 * t,
                0.189 * t,
                0.0,
                0.0,
                0.349 * t,
                1.0 - 0.314 * t,
                0.168 * t,
                0.0,
                0.0,
                0.272 * t,
                0.534 * t,
                1.0 - 0.869 * t,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                1.0,
                0.0,
            ])
        }
        FilterOp::Invert(amount) => {
            let scale = 1.0 - 2.0 * amount;
            let offset = amount;
            Some([
                scale, 0.0, 0.0, 0.0, offset, 0.0, scale, 0.0, 0.0, offset, 0.0, 0.0, scale, 0.0,
                offset, 0.0, 0.0, 0.0, 1.0, 0.0,
            ])
        }
        FilterOp::Saturate(amount) => Some([
            0.213 + 0.787 * amount,
            0.715 - 0.715 * amount,
            0.072 - 0.072 * amount,
            0.0,
            0.0,
            0.213 - 0.213 * amount,
            0.715 + 0.285 * amount,
            0.072 - 0.072 * amount,
            0.0,
            0.0,
            0.213 - 0.213 * amount,
            0.715 - 0.715 * amount,
            0.072 + 0.928 * amount,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            1.0,
            0.0,
        ]),
        FilterOp::HueRotate(degrees) => {
            let radians = degrees.to_radians();
            let cosine = radians.cos();
            let sine = radians.sin();
            Some([
                0.213 + cosine * 0.787 - sine * 0.213,
                0.715 - cosine * 0.715 - sine * 0.715,
                0.072 - cosine * 0.072 + sine * 0.928,
                0.0,
                0.0,
                0.213 - cosine * 0.213 + sine * 0.143,
                0.715 + cosine * 0.285 + sine * 0.140,
                0.072 - cosine * 0.072 - sine * 0.283,
                0.0,
                0.0,
                0.213 - cosine * 0.213 - sine * 0.787,
                0.715 - cosine * 0.715 + sine * 0.715,
                0.072 + cosine * 0.928 + sine * 0.072,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                1.0,
                0.0,
            ])
        }
        FilterOp::Opacity(amount) => {
            let mut matrix = identity();
            matrix[18] = amount.clamp(0.0, 1.0);
            Some(matrix)
        }
        FilterOp::Blur(_) | FilterOp::DropShadow(_) => None,
    }
}

fn draw_text_in_rect(
    canvas: &Canvas,
    rect: LayoutRect,
    text: &str,
    style: &Style,
    typeface: &Typeface,
    metrics_font: &fontdue::Font,
) {
    draw_text_in_rect_with_line_context(
        canvas,
        rect,
        text,
        style,
        typeface,
        metrics_font,
        None,
    );
}

fn draw_text_in_rect_with_line_context(
    canvas: &Canvas,
    rect: LayoutRect,
    text: &str,
    style: &Style,
    typeface: &Typeface,
    _metrics_font: &fontdue::Font,
    line_context: Option<crate::paint_artifact::InlineLineContext>,
) {
    draw_text_in_rect_with_line_painter(canvas, rect, text, style, typeface,
        _metrics_font, line_context, true, &mut |canvas, x, top, text, _advance, style, expansion| {
            if let Some(expansion) = expansion {
                draw_justified_text_line(canvas, x, top, text, style.font_size, style.color,
                    style.opacity, typeface, style, expansion)
            } else {
                draw_text_line(canvas, x, top, text, style.font_size, style.color,
                    style.opacity, typeface, style)
            }
        });
}

type TextLinePainter<'a> = dyn FnMut(&Canvas, f32, f32, &str, f32, &Style,
    Option<text_layout::JustificationExpansion>) -> f32 + 'a;

fn draw_text_in_rect_with_line_painter(
    canvas: &Canvas,
    rect: LayoutRect,
    text: &str,
    style: &Style,
    typeface: &Typeface,
    _metrics_font: &fontdue::Font,
    line_context: Option<crate::paint_artifact::InlineLineContext>,
    paint_inline_box: bool,
    paint_line: &mut TextLinePainter<'_>,
) {
    if style.display == Display::Inline
        && rect.width <= 0.0
        && matches!(
            style.white_space,
            w3cos_std::style::WhiteSpace::Normal | w3cos_std::style::WhiteSpace::PreLine
        )
        && !text.is_empty()
        && text
            .chars()
            .all(|character| matches!(character, ' ' | '\t' | '\r' | '\n'))
        && style.padding_lengths().left == 0.0
        && style.padding_lengths().right == 0.0
        && style.border_left_width.unwrap_or(style.border_width) == 0.0
        && style.border_right_width.unwrap_or(style.border_width) == 0.0
    {
        return;
    }
    let content = text_paint_box(rect, style);
    let first_content = line_context
        .map(|context| LayoutRect {
            y: content.y,
            height: content.height,
            ..context.fragment_box(style, true, false)
        })
        .unwrap_or(content);
    let continuation_content = line_context
        .map(|context| LayoutRect {
            y: content.y,
            height: content.height,
            ..context.fragment_box(style, false, false)
        })
        .unwrap_or_else(|| text_continuation_paint_box(rect, style));
    let image_info = canvas.image_info();
    let indent = style.resolved_text_indent(
        line_context.map_or(content.width, |context| context.line_box.width),
        image_info.width() as f32,
        image_info.height() as f32,
    );
    let mut first_line_content =
        match line_context.map_or(style.direction, |context| context.direction) {
            w3cos_std::style::TextDirection::Ltr => LayoutRect {
                x: first_content.x + indent,
                width: (first_content.width - indent).max(1.0),
                ..first_content
            },
            w3cos_std::style::TextDirection::Rtl => LayoutRect {
                // A negative RTL indent is already represented by the block's
                // logical-start geometry in the portable line-box lowering.
                // Only contract the paintable first line for a positive indent.
                width: (first_content.width - indent.max(0.0)).max(1.0),
                ..first_content
            },
        };
    if line_context.is_none() && style.display == Display::Inline {
        // A split inline reserves its trailing padding and border on the
        // final fragment, not while deciding whether the first word fits.
        let trailing = if style.direction == w3cos_std::style::TextDirection::Rtl {
            style.padding_lengths().left + style.border_left_width.unwrap_or(style.border_width)
        } else {
            style.padding_lengths().right + style.border_right_width.unwrap_or(style.border_width)
        };
        first_line_content.width += trailing;
    }
    let float_bands: Vec<LayoutRect> = style
        .custom_properties
        .as_ref()
        .and_then(|properties| properties.get("--w3cos-internal-float-line-bands"))
        .map(|value| {
            value
                .split(';')
                .filter_map(|band| {
                    let values: Vec<f32> = band
                        .split_ascii_whitespace()
                        .filter_map(|value| value.parse().ok())
                        .collect();
                    (values.len() == 3
                        && values.iter().all(|value| value.is_finite())
                        && values[2] > 0.0)
                        .then(|| LayoutRect {
                            x: content.x + values[0],
                            y: content.y + values[1],
                            width: values[2],
                            ..content
                        })
                })
                .collect()
        })
        .unwrap_or_default();
    if let Some(first) = float_bands.first() {
        first_line_content = *first;
    }
    let line_widths: Vec<_> = if float_bands.is_empty() {
        vec![first_line_content.width]
    } else {
        float_bands.iter().map(|band| band.width).collect()
    };
    // An overflow clip belongs to the element itself as well as its
    // descendants. The retained prepaint clip chain only carries ancestor
    // clips, so a leaf text node must clip its own glyph paint explicitly.
    // Without this, a correctly shrunken `white-space:nowrap` flex title still
    // paints across adjacent controls.
    let own_clip = matches!(
        style.resolved_overflow_x(),
        w3cos_std::style::Overflow::Hidden
            | w3cos_std::style::Overflow::Scroll
            | w3cos_std::style::Overflow::Auto
    ) || matches!(
        style.resolved_overflow_y(),
        w3cos_std::style::Overflow::Hidden
            | w3cos_std::style::Overflow::Scroll
            | w3cos_std::style::Overflow::Auto
    );
    let clip_save = own_clip.then(|| {
        let save = canvas.save();
        canvas.clip_rect(to_rect(rect), None, Some(false));
        save
    });
    let registry = crate::font_face::FontRegistry::global();
    let tab_context = (style.white_space == w3cos_std::style::WhiteSpace::Pre
        && style.direction == w3cos_std::style::TextDirection::Ltr
        && text.contains('\t') && !text.contains(['\r', '\n', '\u{2028}']))
        .then(|| line_context.map_or_else(
            || (tab_stops_for_style(style), 0.0),
            |context| (context.tab_stops, content.x - context.line_box.x),
        ));
    if tab_context.is_some() {
        // An outer inline can contain earlier siblings of this text. Its
        // first-line box is paragraph geometry, not this run's resolved start.
        let end = first_line_content.x + first_line_content.width;
        first_line_content.x = content.x;
        first_line_content.width = (end - content.x).max(1.0);
    }
    let measure_line = |line: &str| tab_runs_for_context(line, typeface, style, tab_context).map_or_else(
        || measure_skia_text_advance(line, typeface, style), |(_, advance)| advance);
    let measure_ink = |line: &str| tab_runs_for_context(line, typeface, style, tab_context).map_or_else(
        || measure_skia_text_ink_bounds(line, style.font_size, typeface, style.font_weight, Some(style)),
        |(runs, _)| tab_run_ink_bounds(&runs, style.font_size, typeface, style.font_weight, style),
    );
    // A single preserved line cannot soft-wrap. Keep its tabs as shifts until
    // positioning; preparing them as spaces would destroy block-stop context.
    let layout = if tab_context.is_some() {
        std::rc::Rc::new(text_layout::TextPaintLayout {
            lines: vec![text.to_string()], ink_bounds: vec![measure_ink(text)],
        })
    } else { text_layout::retained_text_paint_layout_with_run_width_and_line_widths(
        text,
        continuation_content.width,
        &line_widths,
        style.font_size,
        style.white_space,
        registry.cascade_cache_key(style, text) ^ 0x534b_4941_5445_5801
            ^ w3cos_std::inline_text::metadata_cache_key(style),
        |line| measure_line(line),
        |line| {
            measure_skia_text_ink_bounds(
                line,
                style.font_size,
                typeface,
                style.font_weight,
                Some(style),
            )
        },
    ) };
    let authored_styles = text_layout::authored_line_styles(text, style, &layout.lines);
    if paint_inline_box && style.display == Display::Inline
        && (style.background.a > 0
            || style.background_image.is_some()
            || text_layout::inline_fragment_border_widths(style, true, true)
                .into_iter()
                .any(|width| width > 0.0))
    {
        // InlineLineContext carries the paragraph's available geometry,
        // whose height can enclose several lines. Decoration must advance
        // by the same line spacing as the glyph loop below, not that height.
        let line_height = line_context.and_then(|context| context.line_advance)
            .unwrap_or(crate::layout::inline_style_line_height(&style));
        let top = content.y
            + text_vertical_offset(
                style,
                content.height,
                layout.lines.len() as f32 * line_height,
            );
        let paragraph_ends =
            text_layout::paragraph_terminal_lines(text, style.white_space, &layout.lines);
        for (index, line) in layout.lines.iter().enumerate() {
            let line_content = float_bands.get(index).copied().unwrap_or_else(|| {
                if index == 0 {
                    first_line_content
                } else {
                    line_context
                        .map(|context| {
                            context.fragment_box(style, false, index + 1 == layout.lines.len())
                        })
                        .unwrap_or(continuation_content)
                }
            });
            let align = line_context.map_or_else(
                || effective_text_align(style),
                |context| context.alignment(),
            );
            let justify = align == TextAlign::Justify
                && !paragraph_ends[index]
                && matches!(
                    style.white_space,
                    w3cos_std::style::WhiteSpace::Normal | w3cos_std::style::WhiteSpace::PreLine
                );
            let advance = if justify {
                line_content.width
            } else {
                measure_line(line)
            };
            let line_align = if paragraph_ends[index] {
                effective_text_align_last(style).unwrap_or_else(|| {
                    if align == TextAlign::Justify
                        && style.direction == w3cos_std::style::TextDirection::Rtl
                    {
                        TextAlign::Right
                    } else {
                        align
                    }
                })
            } else {
                align
            };
            let x = aligned_text_x(line_content, line_align, 0.0, advance);
            // Inline decoration follows the font em box, not the line-height
            // strut; half-leading belongs to the ancestor's background.
            let group_font_box = style.custom_properties.as_ref()
                .and_then(|p| p.get("--w3cos-internal-inline-background-font-box"))
                .and_then(|value| {
                    let mut parts = value.split_ascii_whitespace();
                    Some((parts.next()?.parse::<f32>().ok()?, parts.next()?.parse::<f32>().ok()?))
                })
                .filter(|(y, height)| y.is_finite() && height.is_finite() && *height > 0.0);
            let decoration_height = group_font_box.map(|(_, height)| height).unwrap_or_else(|| resolved_font_geometry(style)
                .map_or(style.font_size, ResolvedFontGeometry::height));
            let fragment = text_layout::inline_fragment_background_box(
                LayoutRect {
                    x,
                    y: group_font_box.map_or(top, |(y, _)| y) + index as f32 * line_height,
                    height: decoration_height,
                    ..line_content
                },
                advance,
                style,
                index == 0,
                index + 1 == layout.lines.len(),
            );
            // A merged decorated text leaf and a wrapper around the same
            // text must rasterize their non-empty normal inline edges alike.
            let fragment = if style.line_height_is_normal
                    && !top_aligned_inline(style)
                    && (style.padding_lengths().top > 0.0
                        || style.padding_lengths().bottom > 0.0
                        || style.border_top_width.unwrap_or(style.border_width) > 0.0
                        || style.border_bottom_width.unwrap_or(style.border_width) > 0.0)
                {
                    floor_inline_decoration_origin(fragment)
                } else {
                    fragment
                };
            if style.background.a > 0 {
                draw_rounded_rect(
                    canvas,
                    fragment,
                    style.border_corner_radii(),
                    &color_paint(style.background, style.opacity),
                );
            }
            if style.background_image.is_some() {
                draw_background_image(
                    canvas,
                    fragment,
                    rect,
                    style.border_radius,
                    style,
                    style.opacity,
                );
            }
            let widths = text_layout::inline_fragment_border_widths(
                style,
                index == 0,
                index + 1 == layout.lines.len(),
            );
            if widths.into_iter().any(|width| width > 0.0) {
                let mut fragment_style = style.clone();
                fragment_style.border_width = 0.0;
                fragment_style.border_top_width = Some(widths[0]);
                fragment_style.border_right_width = Some(widths[1]);
                fragment_style.border_bottom_width = Some(widths[2]);
                fragment_style.border_left_width = Some(widths[3]);
                draw_box_border(canvas, fragment, &fragment_style);
            }
        }
    }
    if layout.lines.len() == 1 {
        let style = authored_styles.as_ref().map_or(style, |styles| &styles[0]);
        let ink = measure_ink(&layout.lines[0]);
        let advance = measure_line(&layout.lines[0]);
        let alignment_ink_left =
            alignment_ink_left(&layout.lines[0], ink.left, style.font_size, typeface, style);
        let mut align =
            effective_text_align_last(style).unwrap_or_else(|| effective_text_align(style));
        if line_context.is_none()
            && style.display == Display::Inline
            && style.direction == w3cos_std::style::TextDirection::Rtl
            && style.text_align == TextAlign::Start
            && advance > first_line_content.width
        {
            align = TextAlign::Right;
        }
        let x = aligned_text_x(first_line_content, align, alignment_ink_left, advance);
        // CSS text shares its font baseline in both block and inline leaves.
        // Short line-height permits ink overflow; the current string's ink
        // bounds must not relocate its baseline upward.
        let top = content.y
            + text_vertical_offset(style, content.height, crate::layout::inline_style_line_height(&style))
            + line_box_half_leading(style);
        if paint_inline_box && let Some((runs, _)) = tab_runs_for_context(&layout.lines[0], typeface, style, tab_context) {
            for (offset, run) in runs {
                paint_line(canvas, x + offset, top, run,
                    measure_skia_text_advance(run, typeface, style), style, None);
            }
        } else { paint_line(
            canvas,
            x,
            top,
            &layout.lines[0],
            advance,
            style,
            None,
        ); }
        if let Some(save) = clip_save {
            canvas.restore_to_count(save);
        }
        return;
    }
    let line_height = line_context.and_then(|context| context.line_advance)
        .unwrap_or(crate::layout::inline_style_line_height(&style));
    let line_heights = layout.lines.iter().enumerate().map(|(index, line)| {
        let line_style = authored_styles.as_ref().map_or(style, |styles| &styles[index]);
        line_height.max(resolved_text_line_height(line, line_style))
    }).collect::<Vec<_>>();
    let mut next_line_top = 0.0;
    let line_tops = line_heights.iter().map(|height| {
        let top = next_line_top;
        next_line_top += height;
        top
    }).collect::<Vec<_>>();
    let text_height = next_line_top;
    let top = content.y + text_vertical_offset(style, content.height, text_height);
    let paragraph_ends =
        text_layout::paragraph_terminal_lines(text, style.white_space, &layout.lines);
    for (index, line) in layout.lines.iter().enumerate() {
        let style = authored_styles.as_ref().map_or(style, |styles| &styles[index]);
        let ink = measure_skia_text_ink_bounds(
            line,
            style.font_size,
            typeface,
            style.font_weight,
            Some(style),
        );
        let advance = measure_line(line);
        let alignment_ink_left =
            alignment_ink_left(line, ink.left, style.font_size, typeface, style);
        let line_content = float_bands.get(index).copied().unwrap_or_else(|| {
            if index == 0 {
                first_line_content
            } else {
                line_context
                    .map(|context| {
                        context.fragment_box(style, false, index + 1 == layout.lines.len())
                    })
                    .unwrap_or(continuation_content)
            }
        });
        let align = line_context.map_or_else(
            || effective_text_align(style),
            |context| context.alignment(),
        );
        if align == TextAlign::Justify
            && !paragraph_ends[index]
            && matches!(
                style.white_space,
                w3cos_std::style::WhiteSpace::Normal | w3cos_std::style::WhiteSpace::PreLine
            )
        {
            if !paint_inline_box {
                let expansion = (!style_uses_ahem(style)).then(||
                    text_layout::justification_expansion(line, line_content.width,
                        measure_skia_text_advance(line, typeface, style))).flatten();
                paint_line(canvas, line_content.x,
                    top + line_tops[index] + line_box_half_leading(style),
                    line, line_content.width, style, expansion);
                continue;
            }
            if !style_uses_ahem(style)
                && let Some(expansion) = text_layout::justification_expansion(
                    line, line_content.width, measure_skia_text_advance(line, typeface, style))
            {
                // Preserve contextual shaping and the precise cursor across
                // word boundaries. Per-word origins add an extra float
                // conversion before Skia positions the contextual glyphs.
                paint_line(canvas, line_content.x,
                    top + line_tops[index] + line_box_half_leading(style),
                    line, line_content.width, style, Some(expansion));
                continue;
            }
            if let Some(words) =
                text_layout::justified_word_positions(line, line_content.width, |word| {
                    measure_skia_text_advance(word, typeface, style)
                })
            {
                for (word, offset) in words {
                    paint_line(
                        canvas,
                        line_content.x + offset,
                        top + line_tops[index] + line_box_half_leading(style),
                        word,
                        measure_skia_text_advance(word, typeface, style),
                        style,
                        None,
                    );
                }
                continue;
            }
        }
        let line_align = if paragraph_ends[index] {
            effective_text_align_last(style).unwrap_or_else(|| {
                if align == TextAlign::Justify
                    && style.direction == w3cos_std::style::TextDirection::Rtl
                {
                    TextAlign::Right
                } else {
                    align
                }
            })
        } else {
            align
        };
        let x = aligned_text_x(line_content, line_align, alignment_ink_left, advance);
        paint_line(
            canvas,
            x,
            top + line_tops[index] + line_box_half_leading(style),
            line,
            advance,
            style,
            None,
        );
    }
    if let Some(save) = clip_save {
        canvas.restore_to_count(save);
    }
}

fn text_vertical_offset(style: &Style, content_height: f32, text_height: f32) -> f32 {
    let is_extended_inline_fragment = style.custom_properties.as_ref().is_some_and(|properties| {
        properties.contains_key("--w3cos-internal-vertical-align-length")
    });
    // An in-flow inline text run's box *is* its em box, not a container to
    // centre within: the line box already placed it, negative half-leading
    // included (CSS 2.1 10.8.1). Centring it here would add back exactly the
    // `(font_size - line_height) / 2` that a short `line-height` overflows by,
    // pinning the glyph to the line box top instead of letting it bleed above.
    // Blockified inline runs (absolute, fixed or floated) keep their own line
    // box, so they are excluded from this case.
    let is_in_flow_inline = style.display == Display::Inline
        && !matches!(
            style.position,
            w3cos_std::style::Position::Absolute | w3cos_std::style::Position::Fixed
        )
        && style.float == w3cos_std::style::Float::None;
    if is_extended_inline_fragment
        || is_in_flow_inline
        || (matches!(style.display, Display::Block | Display::InlineBlock)
            && style.justify_content != JustifyContent::Center)
    {
        0.0
    } else {
        (content_height - text_height).max(0.0) * 0.5
    }
}

fn line_box_half_leading(style: &Style) -> f32 {
    if style.display == Display::Inline
        && !matches!(
            style.position,
            w3cos_std::style::Position::Absolute | w3cos_std::style::Position::Fixed
        )
        && style.float == w3cos_std::style::Float::None
    {
        0.0
    } else {
        let font_height = resolved_font_geometry(style)
            .map_or(style.font_size, ResolvedFontGeometry::height);
        // Use the same upper-leading allocation as inline line metrics.
        // A half-pixel (including negative leading) is assigned below the
        // baseline, not rounded again by glyph painting of block text.
        ((crate::layout::inline_used_line_height(crate::layout::inline_style_line_height(&style))
            - font_height) * 0.5).floor()
    }
}

fn draw_centered_text(
    canvas: &Canvas,
    rect: LayoutRect,
    text: &str,
    style: &Style,
    typeface: &Typeface,
    _metrics_font: &fontdue::Font,
) {
    let content = text_paint_box(rect, style);
    let ink = measure_skia_text_ink_bounds(
        text,
        style.font_size,
        typeface,
        style.font_weight,
        Some(style),
    );
    let html_control = style.custom_properties.as_ref()
        .is_some_and(|p| p.contains_key("--w3cos-internal-html-control-appearance"));
    let (x, y) = if html_control {
        // HTML labels are inline content: align the glyph advance and shared
        // font baseline, rather than moving the line for each label's ink.
        // This also applies when author decoration disables theme painting.
        let advance = measure_skia_text_advance(text, typeface, style);
        let font_height = resolved_font_geometry(style)
            .map_or(style.font_size, ResolvedFontGeometry::height);
        (content.x + (content.width - advance) * 0.5,
            content.y + (content.height - font_height) * 0.5)
    } else {
        (content.x + (content.width - ink.width) * 0.5 - ink.left,
            content.y + (content.height - ink.height) * 0.5 - ink.top)
    };
    draw_text_line(
        canvas,
        x,
        y,
        text,
        style.font_size,
        style.color,
        style.opacity,
        typeface,
        style,
    );
}

fn effective_text_align(style: &Style) -> TextAlign {
    // `text-align` positions an inline formatting context inside its block
    // container; it does not realign every inherited inline fragment inside
    // its own margin box. The DOM lowering represents those fragments as
    // Text leaves, so keep their glyphs at the content-box start and let the
    // anonymous line row perform the authored alignment.
    if style.display == Display::Inline
        && style.width != w3cos_std::style::Dimension::Percent(100.0)
        && !style
            .custom_properties
            .as_ref()
            .is_some_and(|properties| properties.contains_key("--w3cos-internal-float-line-bands"))
    {
        return TextAlign::Left;
    }
    // DOM text content is lowered into the host Text component instead of an
    // anonymous flex child. Preserve the browser behavior of centering that
    // anonymous child when the host itself is a centered flex container.
    if matches!(style.justify_content, JustifyContent::Center) {
        TextAlign::Center
    } else {
        match (style.text_align, style.direction) {
            (TextAlign::Start, w3cos_std::style::TextDirection::Ltr)
            | (TextAlign::End, w3cos_std::style::TextDirection::Rtl) => TextAlign::Left,
            (TextAlign::Start, w3cos_std::style::TextDirection::Rtl)
            | (TextAlign::End, w3cos_std::style::TextDirection::Ltr) => TextAlign::Right,
            (align, _) => align,
        }
    }
}

fn effective_text_align_last(style: &Style) -> Option<TextAlign> {
    let value = style
        .custom_properties
        .as_ref()?
        .get("--w3cos-internal-text-align-last")?;
    Some(match value.as_str() {
        "left" => TextAlign::Left,
        "right" => TextAlign::Right,
        "center" => TextAlign::Center,
        "start" if style.direction == w3cos_std::style::TextDirection::Rtl => TextAlign::Right,
        "end" if style.direction == w3cos_std::style::TextDirection::Ltr => TextAlign::Right,
        "auto" => effective_text_align(style),
        _ => TextAlign::Left,
    })
}

fn aligned_text_x(rect: LayoutRect, align: TextAlign, ink_left: f32, advance_width: f32) -> f32 {
    match align {
        // The right-aligned box uses ShapeResult's snapped inline width;
        // glyph positions inside it retain raw shaping precision. Subtracting
        // raw width can move a final glyph across a subpixel raster phase.
        TextAlign::Right => rect.x + rect.width - text_layout::inline_layout_advance(advance_width),
        TextAlign::Center => rect.x + (rect.width - advance_width) * 0.5,
        TextAlign::Left | TextAlign::Justify | TextAlign::Start | TextAlign::End => {
            rect.x - ink_left.min(0.0)
        }
    }
}

fn alignment_ink_left(
    text: &str,
    ink_left: f32,
    font_size: f32,
    typeface: &Typeface,
    style: &Style,
) -> f32 {
    // CSS text starts at its glyph-advance origin, whether represented by an
    // inline run, an atomic inline wrapper or a lowered block leaf. Negative
    // ink bearings may overflow that origin; compensating them changes
    // otherwise identical CSS text.
    //
    // Every display that generates a box owns the line its text is laid out
    // in, so all of them share that advance origin. A `display: table` or
    // `display: table-row` box starts its text exactly like a `display: block`
    // box (css/CSS2/generated-content/after-content-display-006.xht and
    // -011.xht). `display: none` and `display: contents` generate no box of
    // their own and never paint text, so they are the only displays that may
    // still need the ink compensation below.
    if !matches!(style.display, Display::None | Display::Contents)
        || style_uses_generic_monospace(style)
    {
        return 0.0;
    }
    let rendered = text_layout::font_render_text_for_style(text, style);
    if !rendered.chars().next().is_some_and(char::is_whitespace) {
        return ink_left;
    }
    let trimmed = rendered.trim_start_matches(char::is_whitespace);
    let mut visual_style = style.clone();
    visual_style
        .custom_properties
        .get_or_insert_with(Default::default)
        .insert(
            "--w3cos-internal-bidi-visual-order".to_string(),
            "1".to_string(),
        );
    measure_skia_text_ink_bounds(
        trimmed,
        font_size,
        typeface,
        style.font_weight,
        Some(&visual_style),
    )
    .left
}

pub(crate) fn draw_text_line(
    canvas: &Canvas,
    x: f32,
    top: f32,
    text: &str,
    font_size: f32,
    color: w3cos_std::color::Color,
    opacity: f32,
    typeface: &Typeface,
    style: &Style,
) -> f32 {
    let advance = draw_text_glyph_line(
        canvas, x, top, text, font_size, color, opacity, typeface, style,
    );
    text_decoration::paint(
        canvas, x, top, text, advance, font_size, color, opacity, typeface, style,
    );
    advance
}

fn draw_justified_text_line(
    canvas: &Canvas, x: f32, top: f32, text: &str, font_size: f32,
    color: w3cos_std::color::Color, opacity: f32, typeface: &Typeface, style: &Style,
    expansion: text_layout::JustificationExpansion,
) -> f32 {
    let advance = draw_text_glyph_line_with_expansion(canvas, x, top, text, font_size,
        color, opacity, typeface, style, Some(expansion));
    text_decoration::paint_with_expansion(canvas, x, top, text, advance, font_size,
        color, opacity, typeface, style, Some(expansion));
    advance
}

fn draw_text_glyph_line(
    canvas: &Canvas,
    x: f32,
    top: f32,
    text: &str,
    font_size: f32,
    color: w3cos_std::color::Color,
    opacity: f32,
    typeface: &Typeface,
    style: &Style,
) -> f32 {
    draw_text_glyph_line_with_expansion(canvas, x, top, text, font_size, color,
        opacity, typeface, style, None)
}

fn floor_inline_decoration_origin(rect: LayoutRect) -> LayoutRect {
    // Align the top without translating the unsnapped bottom edge. Pixel
    // coverage resolves both endpoints; moving only the origin loses the
    // last border row when fractional padding crosses the bottom midpoint.
    let y=rect.y.floor();
    LayoutRect {y,height:rect.height+(rect.y-y),..rect}
}

fn draw_text_glyph_line_with_expansion(
    canvas: &Canvas, x: f32, top: f32, text: &str, font_size: f32,
    color: w3cos_std::color::Color, opacity: f32, typeface: &Typeface, style: &Style,
    expansion: Option<text_layout::JustificationExpansion>,
) -> f32 {
    if let Some((runs, advance)) = positioned_tab_runs(text, typeface, style) {
        for (offset, run) in runs {
            draw_text_glyph_line(canvas, x + offset, top, run, font_size, color, opacity, typeface, style);
        }
        return advance;
    }
    let paint = color_paint(color, opacity);
    let (font_text, bidi_boundaries) = text_layout::font_render_text_for_style_with_boundaries(text, style);
    if style_uses_ahem(style) {
        // Ahem paints one deterministic em cell per character it covers, but
        // `font-family` still applies per character: a stack such as
        // `"Ahem", "Times New Roman"` keeps Times for every character Ahem
        // has no glyph for.
        let mut cursor_x = x;
        let mut baseline = None;
        for segment in ahem_segments(font_text.as_ref(), style) {
            cursor_x += match segment {
                AhemSegment::Cell(cells) => {
                    draw_ahem_cells(canvas, cursor_x, top, cells, font_size, style, &paint)
                }
                AhemSegment::Stack(stack) => {
                    let baseline = *baseline.get_or_insert_with(|| {
                        text_baseline(top, font_size, typeface, style, stack)
                    });
                    draw_font_stack_runs(
                        canvas, cursor_x, baseline, stack, font_size, typeface, style, &paint,
                    )
                }
                AhemSegment::EnQuad(spaces) => ahem_en_quad_advance(spaces, style),
            };
        }
        return cursor_x - x;
    }
    let baseline = text_baseline(top, font_size, typeface, style, font_text.as_ref());
    draw_font_stack_runs_with_expansion(
        canvas,
        x,
        baseline,
        font_text.as_ref(),
        font_size,
        typeface,
        style,
        &paint,
        expansion.map(|expansion| expansion.for_text(font_text.as_ref())),
        Some(&bidi_boundaries),
    )
}

/// How a slice of a line has to be painted.
#[derive(Debug)]
enum AhemSegment<'a> {
    /// Characters the Ahem face covers: one deterministic em cell each.
    Cell(&'a str),
    /// Characters Ahem does not cover: painted by the CSS font stack.
    Stack(&'a str),
    /// U+2000 has a specified half-em advance even when Ahem lacks its glyph.
    EnQuad(&'a str),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AhemSegmentKind {
    Cell,
    Stack,
    EnQuad,
}

fn ahem_en_quad_advance(spaces: &str, style: &Style) -> f32 {
    spaces.chars().count() as f32 * (style.font_size * 0.5 + style.letter_spacing)
}

/// Split `text` where the Ahem face stops covering characters.
///
/// The deterministic cell is only known to be wrong once a registered Ahem
/// face actually lacks the character, so a stack without one keeps cells for
/// the whole line and every existing Ahem case is untouched.
fn ahem_segments<'a>(text: &'a str, style: &Style) -> Vec<AhemSegment<'a>> {
    let Some(ahem) = registered_ahem_face(style) else {
        return vec![AhemSegment::Cell(text)];
    };
    ahem_segments_for_face(text, &ahem)
}

/// Split `text` at the characters `ahem` does not cover.
fn ahem_segments_for_face<'a>(
    text: &'a str,
    ahem: &crate::font_face::LoadedFont,
) -> Vec<AhemSegment<'a>> {
    let mut segments = Vec::new();
    let mut start = 0;
    let mut kind_here = None;
    for (index, character) in text.char_indices() {
        let kind = if character == '\u{2000}' {
            AhemSegmentKind::EnQuad
        } else if ahem.supports_character(character) {
            AhemSegmentKind::Cell
        } else {
            AhemSegmentKind::Stack
        };
        if kind_here == Some(kind) {
            continue;
        }
        if let Some(previous) = kind_here {
            segments.push(ahem_segment(previous, &text[start..index]));
        }
        start = index;
        kind_here = Some(kind);
    }
    if let Some(last) = kind_here {
        segments.push(ahem_segment(last, &text[start..]));
    }
    segments
}

fn ahem_segment<'a>(kind: AhemSegmentKind, text: &'a str) -> AhemSegment<'a> {
    match kind {
        AhemSegmentKind::Cell => AhemSegment::Cell(text),
        AhemSegmentKind::EnQuad => AhemSegment::EnQuad(text),
        AhemSegmentKind::Stack => AhemSegment::Stack(text),
    }
}

/// Whether `font` has a glyph for every character of `text`.
fn font_covers_text(font: &crate::font_face::LoadedFont, text: &str) -> bool {
    text.chars()
        .all(|character| font.supports_character(character))
}

/// The Ahem face, but only when a parsed one can prove character coverage.
///
/// A placeholder registration carries no bytes, and an unparsed face reports
/// every character as missing, so coverage may only be trusted with a parsed
/// face present.
fn registered_ahem_face(style: &Style) -> Option<crate::font_face::LoadedFont> {
    if !style_uses_ahem(style) {
        return None;
    }
    let face_style = match style.font_style {
        w3cos_std::style::FontStyle::Normal => crate::font_face::FontFaceStyle::Normal,
        w3cos_std::style::FontStyle::Italic => crate::font_face::FontFaceStyle::Italic,
        w3cos_std::style::FontStyle::Oblique => crate::font_face::FontFaceStyle::Oblique,
    };
    let ahem = crate::font_face::FontRegistry::global().resolve(
        "Ahem",
        crate::font_face::FontWeight(style.font_weight),
        face_style,
    )?;
    ahem.parsed()?;
    Some(ahem)
}

/// Use registered font raster geometry; deterministic em cells are only a
/// fallback when the embedder has not supplied the Ahem face.
fn draw_ahem_cells(
    canvas: &Canvas,
    x: f32,
    top: f32,
    text: &str,
    font_size: f32,
    style: &Style,
    paint: &Paint,
) -> f32 {
    if let Some((_, face)) = registered_typeface_covering(style, text) {
        return draw_font_stack_runs(canvas, x,
            text_baseline(top, font_size, &face, style, text), text, font_size,
            &face, style, paint);
    }
    let mut cursor_x = x;
    let snapped_top = top.round();
    for character in text.chars() {
        if !character.is_whitespace() {
            let (glyph_top, glyph_height) = ahem_glyph_vertical_bounds(character, font_size);
            canvas.draw_rect(
                Rect::from_xywh(
                    cursor_x.round(),
                    snapped_top + glyph_top.round(),
                    font_size.round(),
                    glyph_height.round(),
                ),
                paint,
            );
        }
        cursor_x += font_size;
        cursor_x += style.letter_spacing;
        if is_word_spacing_character(character) {
            cursor_x += style.word_spacing;
        }
    }
    cursor_x - x
}

/// Distance from the line top to the alphabetic baseline, taken from the face
/// that establishes the line's metrics.
pub(crate) fn text_baseline(top: f32, font_size: f32, typeface: &Typeface, style: &Style, _text: &str) -> f32 {
    // Fallback glyphs still share the registered primary font box's baseline.
    // Coverage decides which face paints, not which embedding supplies a strut.
    let registered = registered_typeface(style);
    let generic = generic_serif_typeface(style);
    let baseline_typeface = registered
        .as_ref()
        .map(|(_, typeface)| typeface)
        .or(generic.as_ref())
        .unwrap_or(typeface);
    if registered.is_some() || generic.is_some() {
        let face = if registered.is_none() {
            typeface_for_character(baseline_typeface, 'x', style.font_weight)
        } else { baseline_typeface.clone() };
        return top + typeface_font_geometry(&face, font_size).ascent;
    }
    let (_, metrics) = crate::skia_text_run::css_font(baseline_typeface, font_size).metrics();
    let metric_height = (metrics.descent - metrics.ascent).max(f32::EPSILON);
    top + font_size * (-metrics.ascent / metric_height).clamp(0.0, 1.0)
}

/// Paint `text` with the CSS font stack and return the advance.
fn glyph_run_intersects_clip(
    canvas: &Canvas,
    font: &skia_safe::Font,
    run: &crate::skia_text_run::GlyphRun,
    x: f32,
    baseline: f32,
) -> bool {
    glyph_slice_intersects_clip(canvas, font, &run.glyphs, &run.positions, &run.glyph_font_sizes, x, baseline)
}

pub(crate) fn glyph_slice_intersects_clip(
    canvas: &Canvas, font: &skia_safe::Font,
    glyphs: &[skia_safe::GlyphId], positions: &[skia_safe::Point],
    glyph_font_sizes: &[f32],
    x: f32, baseline: f32,
) -> bool {
    let Some(clip) = canvas.device_clip_bounds() else { return false; };
    let matrix = canvas.local_to_device_as_3x3();
    if matrix.has_perspective() { return true; }
    let Some(ink) = crate::skia_text_run::geometric_ink_bounds_with_sizes(
        font, glyphs, positions, glyph_font_sizes) else { return true; };
    if ink.is_empty() { return false; }
    let bounds = matrix.map_rect(Rect::new(ink.left + x, ink.top + baseline,
        ink.right + x, ink.bottom + baseline)).0;
    if !bounds.is_finite() { return true; }
    let late_visible = GLYPH_CLIP_RECORDING.with(|recording| {
        let mut recording = recording.borrow_mut();
        let Some(recording) = recording.as_mut().filter(|recording| recording.clip.is_some())
            else { return true; };
        let visible = glyph_ink_visible(bounds, recording.clip);
        recording.decisions.push(GlyphClipDecision { ink: bounds, visible });
        visible
    });
    // Do not let a raster mask's antialias fringe paint when the vector ink
    // lies wholly outside the clip. Actual negative bearings remain included.
    late_visible && bounds.left < clip.right as f32 && bounds.right > clip.left as f32
        && bounds.top < clip.bottom as f32 && bounds.bottom > clip.top as f32
}

fn draw_font_stack_runs(
    canvas: &Canvas,
    x: f32,
    baseline: f32,
    text: &str,
    font_size: f32,
    typeface: &Typeface,
    style: &Style,
    paint: &Paint,
) -> f32 {
    draw_font_stack_runs_with_expansion(canvas, x, baseline, text, font_size,
        typeface, style, paint, None, None)
}

fn draw_font_stack_runs_with_expansion(
    canvas: &Canvas, x: f32, baseline: f32, text: &str, font_size: f32,
    typeface: &Typeface, style: &Style, paint: &Paint,
    expansion: Option<text_layout::JustificationExpansion>,
    resolved_boundaries: Option<&[usize]>,
) -> f32 {
    let mut cursor_x = x;
    let adjustments = authored_fragment_adjustments(text, style);
    let fallback_boundaries;
    let bidi_boundaries = if let Some(boundaries) = resolved_boundaries { boundaries } else {
        fallback_boundaries = unresolved_bidi_segment_boundaries(text, style);
        &fallback_boundaries
    };
    let mut byte_offset = 0;
    for (index, run) in css_font_runs_with_boundaries(text, typeface, style, bidi_boundaries).into_iter().enumerate() {
        // Bidi segment boxes use LayoutUnit starts. A same-direction font
        // fallback is still inside that box and retains raw shaping precision.
        if index > 0 && bidi_boundaries.contains(&byte_offset) {
            cursor_x = x + text_layout::inline_layout_advance(cursor_x - x);
        }
        let font = crate::skia_text_run::css_font_for_style(&run.typeface, font_size, style);
        let shaped = if let Some(expansion) = expansion {
            crate::skia_text_run::shape_visual_run_with_expansion(run.text,
                &run.typeface, style, adjustments.is_some(), expansion, byte_offset)
        } else if adjustments.is_some() {
            crate::skia_text_run::shape_visual_run_with_clusters(run.text, &run.typeface, style)
        } else { crate::skia_text_run::shape_visual_run(run.text, &run.typeface, style) };
        if let Some(mut shaped) = shaped
        {
            if let Some((ends, offsets, _)) = &adjustments {
                for (position, cluster) in shaped.positions.iter_mut().zip(&shaped.glyph_clusters) {
                    let fragment = ends.partition_point(|end| *end <= byte_offset + cluster);
                    position.x += offsets.get(fragment).copied().unwrap_or(0.0);
                }
            }
            if glyph_run_intersects_clip(canvas, &font, &shaped, cursor_x, baseline) {
                shaped.draw(canvas, (cursor_x, baseline), &font, paint);
            }
            cursor_x += shaped.advance;
        } else {
            canvas.draw_str(run.text, (cursor_x, baseline), &font, paint);
            cursor_x += font.measure_str(run.text, Some(paint)).0;
        }
        byte_offset += run.text.len();
    }
    cursor_x - x + adjustments.map_or(0.0, |(_, _, remainder)| remainder)
}

fn authored_fragment_adjustments(text: &str, style: &Style) -> Option<(Vec<usize>, Vec<f32>, f32)> {
    let ends = w3cos_std::inline_text::fragment_ends(text, style)?;
    let mut start = 0;
    let fragments = ends.iter().map(|end| {
        let fragment = &text[start..*end];
        start = *end;
        fragment
    }).collect::<Vec<_>>();
    // Reuse contextual shaping validation: never cut ligatures, reorder
    // clusters or independently reshape each authored fragment.
    let advances = measure_skia_inline_fragment_advances(&fragments, style)?;
    let mut remainder = 0.0;
    let offsets = advances.iter().map(|advance| {
        let offset = remainder;
        remainder += text_layout::inline_layout_advance(*advance) - advance;
        offset
    }).collect();
    Some((ends, offsets, remainder))
}

fn tab_run_ink_bounds(
    runs: &[(f32, &str)], font_size: f32, typeface: &Typeface, font_weight: u16, style: &Style,
) -> text_layout::InkBounds {
    let mut bounds = None::<text_layout::InkBounds>;
    for &(offset, run) in runs {
        let ink = measure_skia_text_ink_bounds(run, font_size, typeface, font_weight, Some(style));
        if ink.width <= 0.0 && ink.height <= 0.0 { continue; }
        let ink = text_layout::InkBounds { left: ink.left + offset, ..ink };
        bounds = Some(match bounds {
            None => ink,
            Some(previous) => {
                let left = previous.left.min(ink.left);
                let top = previous.top.min(ink.top);
                text_layout::InkBounds {
                    left, top,
                    width: (previous.left + previous.width).max(ink.left + ink.width) - left,
                    height: (previous.top + previous.height).max(ink.top + ink.height) - top,
                }
            }
        });
    }
    bounds.unwrap_or_else(text_layout::InkBounds::empty)
}

fn measure_skia_text_ink_bounds(
    text: &str,
    font_size: f32,
    typeface: &Typeface,
    font_weight: u16,
    style: Option<&Style>,
) -> text_layout::InkBounds {
    if let Some(style) = style
        && let Some((runs, _)) = positioned_tab_runs(text, typeface, style)
    {
        let mut bounds = None::<text_layout::InkBounds>;
        for (offset, run) in runs {
            let ink = measure_skia_text_ink_bounds(run, font_size, typeface, font_weight, Some(style));
            if ink.width <= 0.0 && ink.height <= 0.0 { continue; }
            let ink = text_layout::InkBounds { left: ink.left + offset, ..ink };
            bounds = Some(match bounds {
                None => ink,
                Some(previous) => {
                    let left = previous.left.min(ink.left);
                    let top = previous.top.min(ink.top);
                    text_layout::InkBounds {
                        left, top,
                        width: (previous.left + previous.width).max(ink.left + ink.width) - left,
                        height: (previous.top + previous.height).max(ink.top + ink.height) - top,
                    }
                }
            });
        }
        return bounds.unwrap_or_else(text_layout::InkBounds::empty);
    }
    if style.is_some_and(style_uses_ahem) {
        let style = style.expect("Ahem style checked above");
        let render_text = text_layout::font_render_text_for_style(text, style);
        let mut cursor = 0.0_f32;
        let mut left = f32::MAX;
        let mut top = f32::MAX;
        let mut right = f32::MIN;
        let mut bottom = f32::MIN;
        let mut saw_ink = false;
        for segment in ahem_segments(render_text.as_ref(), style) {
            let (advance, ink) = match segment {
                AhemSegment::Cell(cells) => ahem_cell_ink_bounds(cells, font_size, style),
                AhemSegment::Stack(stack) => {
                    font_stack_ink_bounds(stack, font_size, typeface, font_weight, Some(style))
                }
                AhemSegment::EnQuad(spaces) => (
                    ahem_en_quad_advance(spaces, style),
                    text_layout::InkBounds::empty(),
                ),
            };
            if ink.width > 0.0 || ink.height > 0.0 {
                saw_ink = true;
                left = left.min(cursor + ink.left);
                top = top.min(ink.top);
                right = right.max(cursor + ink.left + ink.width);
                bottom = bottom.max(ink.top + ink.height);
            }
            cursor += advance;
        }
        if !saw_ink {
            return text_layout::InkBounds::empty();
        }
        return text_layout::InkBounds {
            left,
            top,
            width: (right - left).max(0.0),
            height: (bottom - top).max(0.0),
        };
    }
    font_stack_ink_bounds(text, font_size, typeface, font_weight, style).1
}

/// Ink and advance of the deterministic Ahem cells covering `text`.
fn ahem_cell_ink_bounds(
    text: &str,
    font_size: f32,
    style: &Style,
) -> (f32, text_layout::InkBounds) {
    if let Some((_, face)) = registered_typeface_covering(style, text) {
        return font_stack_ink_bounds(text, font_size, &face, style.font_weight, Some(style));
    }
    let character_count = text.chars().count();
    let mut cursor = 0.0_f32;
    let mut left = None::<f32>;
    let mut right = 0.0_f32;
    let mut top = f32::MAX;
    let mut bottom = f32::MIN;
    for (index, character) in text.chars().enumerate() {
        if !character.is_whitespace() {
            left.get_or_insert(cursor);
            right = cursor + font_size;
            let (glyph_top, glyph_height) = ahem_glyph_vertical_bounds(character, font_size);
            top = top.min(glyph_top);
            bottom = bottom.max(glyph_top + glyph_height);
        }
        cursor += font_size;
        if index + 1 < character_count {
            cursor += style.letter_spacing;
        }
        if is_word_spacing_character(character) {
            cursor += style.word_spacing;
        }
    }
    let bounds = match left {
        Some(left) => text_layout::InkBounds {
            left,
            top,
            width: right - left,
            height: bottom - top,
        },
        _ => text_layout::InkBounds::empty(),
    };
    (cursor, bounds)
}

/// Ink and advance of `text` measured through the CSS font stack.
fn font_stack_ink_bounds(
    text: &str,
    font_size: f32,
    typeface: &Typeface,
    font_weight: u16,
    style: Option<&Style>,
) -> (f32, text_layout::InkBounds) {
    let mut cursor_x = 0.0_f32;
    let mut left = f32::MAX;
    let mut top = f32::MAX;
    let mut right = f32::MIN;
    let mut bottom = f32::MIN;
    let mut saw_ink = false;

    let (font_text, bidi_boundaries) = style.map_or_else(
        || (text_layout::font_render_text(text, w3cos_std::style::TextDirection::Ltr), Vec::new()),
        |style| text_layout::font_render_text_for_style_with_boundaries(text, style),
    );
    let runs = style.map_or_else(
        || fallback_font_runs(font_text.as_ref(), typeface, font_weight),
        |style| css_font_runs_with_boundaries(font_text.as_ref(), typeface, style, &bidi_boundaries),
    );
    let mut byte_offset = 0;
    for (index, run) in runs.into_iter().enumerate() {
        if index > 0 && bidi_boundaries.contains(&byte_offset) {
            cursor_x = text_layout::inline_layout_advance(cursor_x);
        }
        let font = style.map_or_else(|| crate::skia_text_run::css_font(&run.typeface, font_size),
            |style| crate::skia_text_run::css_font_for_style(&run.typeface, font_size, style));
        let default_style = Style {
            font_size,
            ..Style::default()
        };
        let shaping_style = style.unwrap_or(&default_style);
        let (advance, bounds) = if let Some(shaped) =
            crate::skia_text_run::shape_visual_run(run.text, &run.typeface, shaping_style)
        {
            (shaped.advance, shaped.ink_bounds(&font).unwrap_or_default())
        } else {
            font.measure_str(run.text, None)
        };
        if bounds.width() > 0.0 || bounds.height() > 0.0 {
            saw_ink = true;
            left = left.min(cursor_x + bounds.left);
            top = top.min(font_size + bounds.top);
            right = right.max(cursor_x + bounds.right);
            bottom = bottom.max(font_size + bounds.bottom);
        }
        cursor_x += advance;
        byte_offset += run.text.len();
    }

    if !saw_ink {
        return (cursor_x, text_layout::InkBounds::empty());
    }

    (
        cursor_x,
        text_layout::InkBounds {
            left,
            top,
            width: (right - left).max(0.0),
            height: (bottom - top).max(0.0),
        },
    )
}

fn ahem_glyph_vertical_bounds(character: char, font_size: f32) -> (f32, f32) {
    match character {
        // The Ahem face exposes an 0.2em descender-only `p`. CSS painting
        // order tests use it to prove that the glyph covers an underline.
        'p' => (font_size * 0.8, font_size * 0.2),
        // Capital E-acute occupies the top 0.8em of its cell. Reftests use
        // the exposed lower 0.2em for inline background/baseline checks.
        '\u{00c9}' => (0.0, font_size * 0.8),
        _ => (0.0, font_size),
    }
}

#[cfg(test)]
mod ahem_segment_tests {
    use super::{AhemSegment, ahem_en_quad_advance, ahem_segments_for_face, font_covers_text};
    use crate::font_face::{FontFace, FontFaceStyle, FontRegistry, FontSource, FontWeight};

    /// Ahem covers Latin-1 but stops short of Latin Extended-A, which is the
    /// gap `font-family-013` builds `"Ahem", "Times New Roman"` on.
    fn pinned_ahem() -> Option<crate::font_face::LoadedFont> {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../wpt/fonts/Ahem.ttf");
        let bytes = std::fs::read(path).ok()?;
        let registry = FontRegistry::new();
        registry
            .register(FontFace {
                family: "Ahem".into(),
                src: FontSource::Bytes(bytes),
                ..Default::default()
            })
            .expect("Ahem registers");
        registry.resolve("Ahem", FontWeight::NORMAL, FontFaceStyle::Normal)
    }

    #[test]
    fn ahem_covers_ascii_but_not_latin_extended_a() {
        let Some(ahem) = pinned_ahem() else {
            eprintln!("skipped: no pinned WPT checkout");
            return;
        };
        assert!(font_covers_text(&ahem, "T"));
        assert!(!font_covers_text(&ahem, "\u{0162}"));
        assert!(!font_covers_text(&ahem, "T\u{0162}"));
    }

    #[test]
    fn cells_stop_at_the_coverage_boundary() {
        let Some(ahem) = pinned_ahem() else {
            eprintln!("skipped: no pinned WPT checkout");
            return;
        };
        let segments = ahem_segments_for_face("T\u{0162}T", &ahem);
        assert_eq!(segments.len(), 3, "{segments:?}");
        assert!(matches!(segments[0], AhemSegment::Cell(t) if t == "T"));
        assert!(matches!(segments[1], AhemSegment::Stack(t) if t == "\u{0162}"));
        assert!(matches!(segments[2], AhemSegment::Cell(t) if t == "T"));
    }

    /// `font-family-013` and `fonts-013` are four uncovered characters in a
    /// row; they must stay one run so the stack shapes them together.
    #[test]
    fn uncovered_text_stays_one_stack_run() {
        let Some(ahem) = pinned_ahem() else {
            eprintln!("skipped: no pinned WPT checkout");
            return;
        };
        let segments = ahem_segments_for_face("\u{0162}\u{0119}\u{015f}\u{0163}", &ahem);
        assert_eq!(segments.len(), 1, "{segments:?}");
        assert!(
            matches!(segments[0], AhemSegment::Stack(t) if t == "\u{0162}\u{0119}\u{015f}\u{0163}")
        );
    }

    #[test]
    fn en_quad_keeps_its_half_em_advance_between_ahem_cells() {
        let Some(ahem) = pinned_ahem() else {
            eprintln!("skipped: no pinned WPT checkout");
            return;
        };
        let segments = ahem_segments_for_face("XX\u{2000}\u{2000}\u{2000}XX", &ahem);
        assert_eq!(segments.len(), 3, "{segments:?}");
        assert!(matches!(segments[0], AhemSegment::Cell("XX")));
        let AhemSegment::EnQuad(spaces) = segments[1] else {
            panic!("en quads must use their own advance: {segments:?}");
        };
        assert_eq!(
            ahem_en_quad_advance(
                spaces,
                &w3cos_std::Style {
                    font_size: 16.0,
                    ..w3cos_std::Style::default()
                }
            ),
            24.0
        );
        assert!(matches!(segments[2], AhemSegment::Cell("XX")));
    }
}

fn measure_skia_text_advance(text: &str, typeface: &Typeface, style: &Style) -> f32 {
    if let Some((_, advance)) = positioned_tab_runs(text, typeface, style) {
        return advance;
    }
    let (render_text, bidi_boundaries) = text_layout::font_render_text_for_style_with_boundaries(text, style);
    if style_uses_ahem(style) {
        return ahem_segments(render_text.as_ref(), style)
            .into_iter()
            .map(|segment| match segment {
                AhemSegment::Cell(cells) => ahem_cell_advance(cells, style),
                AhemSegment::Stack(stack) => font_stack_advance(stack, typeface, style),
                AhemSegment::EnQuad(spaces) => ahem_en_quad_advance(spaces, style),
            })
            .sum();
    }
    font_stack_advance_with_boundaries(render_text.as_ref(), typeface, style, &bidi_boundaries)
}

/// Position preserved tabs before glyph shaping. A tab is a shift, not a
/// glyph or a synthetic sequence of word-spacing-bearing spaces. The same
/// offsets must drive advance, ink bounds and glyph paint.
/// This entry point resolves a standalone LTR line from its content start;
/// paragraph callers still need to supply the block-container tab context.
fn positioned_tab_runs<'a>(
    text: &'a str,
    typeface: &Typeface,
    style: &Style,
) -> Option<(Vec<(f32, &'a str)>, f32)> {
    positioned_tab_runs_in_context(text, typeface, style, None, 0.0)
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct TabStops {
    width: f32,
    minimum_gap: f32,
}

fn tab_runs_for_context<'a>(
    text: &'a str, typeface: &Typeface, style: &Style, context: Option<(TabStops, f32)>,
) -> Option<(Vec<(f32, &'a str)>, f32)> {
    context.and_then(|(stops, offset)|
        positioned_tab_runs_in_context(text, typeface, style, Some(stops), offset))
}

fn with_text_typeface<T>(text: &str, style: &Style, callback: impl FnOnce(&Typeface) -> T) -> T {
    let registered = registered_typeface_covering(style, text);
    let generic = generic_serif_typeface(style);
    INTRINSIC_PRIMARY_TYPEFACE.with(|intrinsic| {
        callback(registered.as_ref().map(|(_, face)| face).or(generic.as_ref()).unwrap_or(intrinsic))
    })
}

/// HTML text field content size, before padding and borders. Mirrors the
/// browser's character-count sizing rather than a fixed native control size.
pub(crate) fn html_text_input_content_size(style: &Style, size: u32) -> (f32, f32) {
    with_text_typeface("x0", style, |face| {
        let font = crate::skia_text_run::css_font(face, style.font_size);
        let (_, raw) = font.metrics();
        let geometry = typeface_font_geometry(face, style.font_size);
        let zero = font.measure_str("0", None).0;
        let average = if cfg!(target_os = "macos") {
            font.measure_str("x", None).0
        } else { raw.avg_char_width };
        // The browser rejects families with incompatible average-width data,
        // including metrics reporting a CJK cell for Latin text controls.
        let invalid = ["American Typewriter", "Arial Hebrew", "Chalkboard", "Cochin",
            "Corsiva Hebrew", "Courier", "Euphemia UCAS", "Geneva", "Gill Sans",
            "Hei", "Helvetica", "Hoefler Text", "InaiMathi", "Kai", "Lucida Grande",
            "Marker Felt", "Monaco", "Mshtakan", "New Peninim MT", "Osaka", "Raanana",
            "STHeiti", "Symbol", "Times", "Apple Braille", "Apple LiGothic", "Apple LiSung",
            "Apple Symbols", "AppleGothic", "AppleMyungjo", "#GungSeo", "#HeadLineA",
            "#PCMyungjo", "#PilGi"].contains(&face.family_name().as_str());
        let valid = average.is_finite() && average > 0.0
            && average <= zero * 1.7 && !invalid;
        let character = if valid { average.max(average.round()) } else { zero };
        let maximum = if !valid { character } else if cfg!(target_os = "macos") {
            geometry.ascent
        } else { raw.max_char_width.round() };
        ((character * size as f32 + (maximum - character).max(0.0)).ceil(),
            geometry.line_spacing())
    })
}

pub(crate) fn tab_stops_for_style(style: &Style) -> TabStops {
    with_text_typeface(" 0", style, |face| {
        let unspaced = Style { word_spacing: 0.0, letter_spacing: 0.0, ..style.clone() };
        TabStops {
            width: 8.0 * measure_skia_text_advance(" ", face, style).max(0.0),
            minimum_gap: 0.5 * measure_skia_text_advance("0", face, &unspaced).max(0.0),
        }
    })
}

pub(crate) fn preserved_tab_advance_in_block(
    text: &str, style: &Style, block_style: &Style, line_offset: f32,
) -> Option<f32> {
    let stops = tab_stops_for_style(block_style);
    with_text_typeface(text, style, |face|
        positioned_tab_runs_in_context(text, face, style, Some(stops), line_offset)
            .map(|(_, advance)| advance)
    )
}

fn positioned_tab_runs_in_context<'a>(
    text: &'a str,
    typeface: &Typeface,
    style: &Style,
    block_stops: Option<TabStops>,
    line_offset: f32,
) -> Option<(Vec<(f32, &'a str)>, f32)> {
    if !text.contains('\t')
        || style.direction != w3cos_std::style::TextDirection::Ltr
        || !matches!(style.white_space,
            w3cos_std::style::WhiteSpace::Pre | w3cos_std::style::WhiteSpace::PreWrap)
    {
        return None;
    }
    let stop_width = block_stops.map_or_else(
        || 8.0 * measure_skia_text_advance(" ", typeface, style).max(0.0),
        |stops| stops.width,
    );
    let unspaced = Style { word_spacing: 0.0, letter_spacing: 0.0, ..style.clone() };
    let minimum_gap = block_stops.map_or_else(
        || 0.5 * measure_skia_text_advance("0", typeface, &unspaced).max(0.0),
        |stops| stops.minimum_gap,
    );
    let mut cursor = line_offset;
    let mut runs = Vec::new();
    let mut parts = text.split('\t').peekable();
    while let Some(run) = parts.next() {
        if !run.is_empty() {
            runs.push((cursor - line_offset, run));
            cursor += measure_skia_text_advance(run, typeface, style);
        }
        if parts.peek().is_some() && stop_width > 0.0 {
            let mut next = ((cursor / stop_width).floor() + 1.0) * stop_width;
            if next - cursor < minimum_gap { next += stop_width; }
            cursor = next;
        }
    }
    Some((runs, cursor - line_offset))
}

/// Advance of the deterministic Ahem cells covering `text`.
fn ahem_cell_advance(text: &str, style: &Style) -> f32 {
    if style.font_variant == w3cos_std::style::FontVariant::SmallCaps {
        if let Some((_, face)) = registered_typeface_covering(style, text) {
            return font_stack_advance(text, &face, style);
        }
    }
    let character_count = text.chars().count();
    let word_spacing = text
        .chars()
        .filter(|character| is_word_spacing_character(*character))
        .count() as f32
        * style.word_spacing;
    character_count as f32 * style.font_size
        + character_count as f32 * style.letter_spacing
        + word_spacing
}

/// Advance of `text` measured through the CSS font stack.
fn font_stack_advance(text: &str, typeface: &Typeface, style: &Style) -> f32 {
    let bidi_boundaries = unresolved_bidi_segment_boundaries(text, style);
    font_stack_advance_with_boundaries(text, typeface, style, &bidi_boundaries)
}

fn font_stack_advance_with_boundaries(
    text: &str, typeface: &Typeface, style: &Style, bidi_boundaries: &[usize],
) -> f32 {
    let mut byte_offset = 0;
    css_font_runs_with_boundaries(text, typeface, style, bidi_boundaries)
        .into_iter()
        .enumerate()
        .fold(0.0, |cursor, (index, run)| {
            let cursor = if index > 0 && bidi_boundaries.contains(&byte_offset) {
                text_layout::inline_layout_advance(cursor)
            } else { cursor };
            byte_offset += run.text.len();
            cursor + crate::skia_text_run::shape_visual_run(run.text, &run.typeface, style)
                .map(|shaped| shaped.advance)
                .unwrap_or_else(|| {
                    crate::skia_text_run::css_font(&run.typeface, style.font_size)
                        .measure_str(run.text, None)
                        .0
                        + run
                            .text
                            .chars()
                            .filter(|character| is_word_spacing_character(*character))
                            .count() as f32
                            * style.word_spacing
                })
        })
}

fn unresolved_bidi_segment_boundaries(text: &str, style: &Style) -> Vec<usize> {
    // Prepared inline source runs already carry their segment geometry. Do
    // not quantize that shared shaping context again in the glyph painter.
    if style.custom_properties.as_ref().is_some_and(|properties|
        properties.get("--w3cos-internal-bidi-visual-order").is_some_and(|value| value == "1"))
        || !text.chars().any(|character| matches!(unicode_bidi::bidi_class(character),
            unicode_bidi::BidiClass::R | unicode_bidi::BidiClass::AL)) { return Vec::new(); }
    // This input is already in left-to-right visual paint order, including
    // mirrored punctuation. Reapplying the authored paragraph base changes
    // neutral boundaries a second time and loses fallback-font LayoutUnit
    // starts. The paragraph direction was consumed by font_render_text_for_style.
    let base = unicode_bidi::Level::ltr();
    let bidi = unicode_bidi::BidiInfo::new(text, Some(base));
    bidi.levels.windows(2).enumerate().filter_map(|(index, levels)|
        (levels[0].is_rtl() != levels[1].is_rtl()).then_some(index + 1)).collect()
}

fn is_word_spacing_character(character: char) -> bool {
    matches!(character, ' ' | '\u{00a0}')
}

fn style_uses_ahem(style: &Style) -> bool {
    style.font_family.as_deref().is_some_and(|families| {
        families.split(',').any(|family| {
            family
                .trim()
                .trim_matches(['"', '\''])
                .eq_ignore_ascii_case("ahem")
        })
    })
}

fn style_uses_generic_monospace(style: &Style) -> bool {
    style.font_family.as_deref().is_some_and(|families| {
        families.split(',').any(|family| {
            family
                .trim()
                .trim_matches(['"', '\''])
                .eq_ignore_ascii_case("monospace")
        })
    })
}


/// Partition one shaped font-stack run at existing fragment boundaries.
/// Never split a glyph cluster: ligatures or reordered clusters require a
/// shared paint run instead of independently painted word fragments.
pub(crate) fn measure_skia_inline_fragment_advances(
    fragments: &[&str],
    style: &Style,
) -> Option<Vec<f32>> {
    if fragments.is_empty() || style_uses_ahem(style) {
        return None;
    }
    let text = fragments.concat();
    if text_layout::font_render_text_for_style(&text, style).as_ref() != text {
        return None;
    }
    let mut boundaries = vec![0];
    for fragment in fragments {
        boundaries.push(boundaries.last()? + fragment.len());
    }
    let registered = registered_typeface_covering(style, &text);
    let generic = generic_serif_typeface(style);
    INTRINSIC_PRIMARY_TYPEFACE.with(|intrinsic| {
        let primary = registered.as_ref().map(|(_, face)| face)
            .or(generic.as_ref()).unwrap_or(intrinsic);
        let mut advances = vec![0.0; fragments.len()];
        let mut run_offset = 0;
        for run in css_font_runs(&text, primary, style) {
            let shaped = crate::skia_text_run::shape_visual_run_with_clusters(run.text, &run.typeface, style)?;
            if shaped.clusters.first()?.0 != 0
                || shaped.clusters.windows(2).any(|pair| pair[0].0 >= pair[1].0)
            { return None; }
            for (index, &(start, advance)) in shaped.clusters.iter().enumerate() {
                let end = shaped.clusters.get(index + 1).map_or(run.text.len(), |next| next.0);
                let fragment = boundaries.partition_point(|boundary| *boundary <= run_offset + start)
                    .checked_sub(1)?;
                if fragment >= fragments.len() || run_offset + end > boundaries[fragment + 1]
                    || !advance.is_finite()
                { return None; }
                advances[fragment] += advance;
            }
            run_offset += run.text.len();
        }
        (run_offset == text.len() && advances.iter().all(|width| *width >= 0.0))
            .then_some(advances)
    })
}

fn resolved_text_line_height(text: &str, style: &Style) -> f32 {
    let authored = crate::layout::inline_style_line_height(style);
    if style.line_height_is_normal {
        resolved_text_font_geometry(text, style).map_or(authored,
            |metrics| authored.max(metrics.line_spacing()))
    } else { authored }
}

pub(crate) fn measure_skia_text_intrinsic_size(text: &str, style: &Style) -> (f32, f32) {
    let registered = registered_typeface_covering(style, text);
    let generic = generic_serif_typeface(style);
    INTRINSIC_PRIMARY_TYPEFACE.with(|intrinsic| {
        let primary = registered
            .as_ref()
            .map(|(_, typeface)| typeface)
            .or(generic.as_ref())
            .unwrap_or(intrinsic);
        let lines = text_layout::wrap_text_with_run_width(
            text,
            f32::MAX / 4.0,
            style.white_space,
            |line| measure_skia_text_advance(line, primary, style),
        );
        let authored_styles = text_layout::authored_line_styles(text, style, &lines);
        let width = lines
            .iter()
            .enumerate()
            .map(|(index, line)| {
                let style = authored_styles.as_ref().map_or(style, |styles| &styles[index]);
                measure_skia_text_advance(line, primary, style)
                    + authored_fragment_adjustments(line, style).map_or(0.0, |(_, _, extra)| extra)
            })
            .fold(0.0_f32, f32::max);
        let padding = style.padding_lengths();
        // Skia's macOS fallback typeface exposes a 32.55px top-to-bottom
        // metric at 16px, but that is a glyph coverage bound, not the used
        // CSS line box. Browser block/inline layout advances by the computed
        // line-height; fallback-specific CJK expansion is applied centrally
        // by `browser_normal_cjk_height` in layout.rs.
        let used = text_layout::used_text_line_count(text, style, &lines);
        let content_height = if used > 1 {
            lines.iter().take(used).map(|line| resolved_text_line_height(line, style)).sum()
        } else { used as f32 * crate::layout::inline_style_line_height(style) };
        (
            width + padding.left + padding.right,
            content_height + padding.top + padding.bottom,
        )
    })
}

pub(crate) fn measure_skia_wrapped_text_height(text: &str, width: f32, style: &Style) -> f32 {
    let registered = registered_typeface_covering(style, text);
    let generic = generic_serif_typeface(style);
    INTRINSIC_PRIMARY_TYPEFACE.with(|intrinsic| {
        let primary = registered
            .as_ref()
            .map(|(_, typeface)| typeface)
            .or(generic.as_ref())
            .unwrap_or(intrinsic);
        let padding = style.padding_lengths();
        let inner_width = (width - padding.left - padding.right).max(1.0);
        let first_line_width =
            (inner_width - style.resolved_text_indent(inner_width, 0.0, 0.0)).max(1.0);
        // A retained paragraph's inline start margin belongs to its first
        // fragment, not every line. Measure with the same continuation box
        // used by paint, or following blocks advance by an extra wrapped line.
        let continuation_width = if style.display == Display::Inline
            && style.custom_properties.as_ref().is_some_and(|properties| {
                properties.contains_key("--w3cos-internal-text-line-width")
            })
        {
            text_layout::inline_text_continuation_box(LayoutRect {
                x: 0.0, y: 0.0, width: inner_width, height: 0.0,
            }, style).width.max(1.0)
        } else { inner_width };
        let lines = text_layout::wrap_text_with_run_width_and_first_line(
            text,
            continuation_width,
            first_line_width,
            style.white_space,
            |line| measure_skia_text_advance(line, primary, style),
        );
        let used_line_count = text_layout::used_text_line_count(text, style, &lines);
        let content_height = if used_line_count == 1 {
            measure_skia_text_intrinsic_size(&lines[0], style).1 - padding.top - padding.bottom
        } else {
            lines.iter().take(used_line_count).map(|line| resolved_text_line_height(line, style)).sum()
        };
        content_height + padding.top + padding.bottom
    })
}

pub(crate) struct FallbackFontRun<'a> {
    pub(crate) text: &'a str,
    pub(crate) typeface: Typeface,
}

fn css_font_runs_with_boundaries<'a>(
    text: &'a str, primary: &Typeface, style: &Style, boundaries: &[usize],
) -> Vec<FallbackFontRun<'a>> {
    // Font fallback and resolved bidi segments are independent partitions.
    // A neutral/Latin direction transition can occur inside one font run;
    // keeping it merged skips the segment's LayoutUnit origin and shaping
    // boundary. Preserve raw precision only inside each resolved segment.
    let mut result=Vec::new();
    let mut offset=0;
    for run in css_font_runs(text,primary,style) {
        let end=offset+run.text.len();
        let mut start=offset;
        for boundary in boundaries.iter().copied().filter(|boundary|*boundary>offset&&*boundary<end)
            .chain(std::iter::once(end)) {
            result.push(FallbackFontRun {text:&text[start..boundary],typeface:run.typeface.clone()});
            start=boundary;
        }
        offset=end;
    }
    result
}

pub(crate) fn css_font_runs<'a>(text: &'a str, primary: &Typeface, style: &Style) -> Vec<FallbackFontRun<'a>> {
    let generic = generic_serif_typeface(style);
    let primary = generic.as_ref().unwrap_or(primary);
    let mut runs = Vec::new();
    for resolved in crate::font_face::FontRegistry::global().resolve_style_runs(style, text) {
        let run_text = &text[resolved.byte_range];
        if let Some(typeface) = resolved.font.as_ref().and_then(|font| font.skia_typeface()) {
            runs.push(FallbackFontRun {
                text: run_text,
                typeface,
            });
        } else {
            runs.extend(fallback_font_runs(run_text, primary, style.font_weight));
        }
    }
    runs
}

fn fallback_font_runs<'a>(
    text: &'a str,
    primary: &Typeface,
    font_weight: u16,
) -> Vec<FallbackFontRun<'a>> {
    let mut runs = Vec::new();
    let mut run_start = 0;
    let mut run_typeface = primary.clone();
    let mut run_typeface_id = primary.unique_id();

    for (offset, character) in text.char_indices() {
        let typeface = typeface_for_character(primary, character, font_weight);
        let typeface_id = typeface.unique_id();
        if offset > run_start && typeface_id != run_typeface_id {
            runs.push(FallbackFontRun {
                text: &text[run_start..offset],
                typeface: run_typeface,
            });
            run_start = offset;
            run_typeface = typeface;
            run_typeface_id = typeface_id;
        } else if offset == run_start {
            run_typeface = typeface;
            run_typeface_id = typeface_id;
        }
    }
    if run_start < text.len() {
        runs.push(FallbackFontRun {
            text: &text[run_start..],
            typeface: run_typeface,
        });
    }
    runs
}

fn typeface_for_character(primary: &Typeface, character: char, font_weight: u16) -> Typeface {
    if font_weight == 400 && primary.unichar_to_glyph(character as i32) != 0 {
        return primary.clone();
    }

    let key = (primary.unique_id(), character, font_weight);
    let cached = FONT_FALLBACK_CACHE.with(|cache| cache.borrow().get(&key).cloned());
    let fallback = match cached {
        Some(cached) => cached,
        None => {
            let style = FontStyle::new(
                (font_weight.clamp(1, 1000) as i32).into(),
                skia_safe::font_style::Width::NORMAL, primary.font_style().slant());
            // A missing nominal cmap entry does not imply a missing shaped
            // cluster: HarfBuzz may canonically decompose it into base/mark
            // glyphs already present in this face. Only retain multi-glyph
            // coverage here: a single replacement glyph can be HarfBuzz's
            // synthetic space fallback, not coverage of the requested character.
            // Keep the original UTF-8
            // text and clusters; do not rewrite the DOM or force NFD globally.
            let probe_style = Style { font_size: 16.0, font_weight,
                ..Style::default() };
            let covered = font_weight == 400
                && crate::skia_text_run::shape_visual_run(&character.to_string(), primary, &probe_style)
                    .is_some_and(|run| run.glyphs.len() > 1
                        && run.glyphs.iter().all(|glyph| *glyph != 0));
            let matched = if covered { Some(primary.clone()) } else {
                FontMgr::default().match_family_style_character(
                    &primary.family_name(), style, &["en", "zh-Hans"], character as i32,
                )
            };
            FONT_FALLBACK_CACHE.with(|cache| {
                let mut cache = cache.borrow_mut();
                if cache.len() >= FONT_FALLBACK_CACHE_CAPACITY {
                    cache.clear();
                }
                cache.insert(key, matched.clone());
            });
            matched
        }
    };
    fallback.unwrap_or_else(|| primary.clone())
}

fn text_content_box(rect: LayoutRect, style: &Style) -> LayoutRect {
    let border_top = style.border_top_width.unwrap_or(style.border_width);
    let (border_left, border_right) = crate::paint_artifact::paint_inline_border_widths(style);
    let border_bottom = style.border_bottom_width.unwrap_or(style.border_width);
    let padding = style.padding_lengths();
    LayoutRect {
        x: rect.x + padding.left + border_left,
        y: rect.y + padding.top + border_top,
        width: (rect.width - padding.left - padding.right - border_left - border_right).max(1.0),
        height: (rect.height - padding.top - padding.bottom - border_top - border_bottom).max(0.0),
    }
}

fn text_paint_box(rect: LayoutRect, style: &Style) -> LayoutRect {
    text_content_box(rect, style)
}

fn text_continuation_paint_box(rect: LayoutRect, style: &Style) -> LayoutRect {
    text_layout::inline_text_continuation_box(text_paint_box(rect, style), style)
}

fn draw_background_image(
    canvas: &Canvas,
    rect: LayoutRect,
    positioning_area: LayoutRect,
    _radius: f32,
    style: &Style,
    opacity: f32,
) {
    draw_background_layers(
        canvas,
        crate::background_image::background_paint_layers_with_overrides(
            style,
            positioning_area,
            crate::paint_artifact::box_background_positioning_rect(style, positioning_area),
            Some(rect),
        ),
        opacity,
    );
}

fn draw_canvas_background_image(
    canvas: &Canvas,
    canvas_rect: LayoutRect,
    _radius: f32,
    style: &Style,
    positioning_area: Option<LayoutRect>,
    opacity: f32,
) {
    draw_background_layers(
        canvas,
        crate::background_image::canvas_background_paint_layers(
            style,
            canvas_rect,
            positioning_area,
        ),
        opacity,
    );
}

fn draw_background_layers(
    canvas: &Canvas,
    layers: Vec<crate::background_image::BackgroundPaintLayer>,
    opacity: f32,
) {
    // CSS paints the first listed background on top of the following layers.
    for layer in layers.into_iter().rev() {
        let clip = match &layer {
            crate::background_image::BackgroundPaintLayer::Raster(layer) => layer.clip,
            crate::background_image::BackgroundPaintLayer::Gradient(layer) => layer.geometry.clip,
        };
        let save = canvas.save();
        if clip.radius > 0.0 {
            canvas.clip_rrect(
                RRect::new_rect_xy(to_rect(clip.rect), clip.radius, clip.radius),
                None,
                Some(true),
            );
        } else {
            // Skia's zero-radius RRect clip uses subtly different coverage
            // from a CSS rectangle. Use the rectangular primitive so raster
            // backgrounds and solid/image reference boxes share exact edges.
            canvas.clip_rect(to_rect(clip.rect), None, Some(false));
        }
        let blend_mode = match &layer {
            crate::background_image::BackgroundPaintLayer::Raster(layer) => layer.blend_mode,
            crate::background_image::BackgroundPaintLayer::Gradient(layer) => layer.blend_mode,
        };
        if blend_mode != crate::background_image::BackgroundBlendMode::Normal {
            let mut layer_paint = Paint::default();
            layer_paint.set_blend_mode(background_skia_blend(blend_mode));
            canvas.save_layer(&SaveLayerRec::default().paint(&layer_paint));
        }
        match layer {
            crate::background_image::BackgroundPaintLayer::Raster(layer) => {
                let shader = layer.repeat_shader_tile.and_then(|tile| {
                    let decoded = crate::image_loader::get_or_load(&layer.source)?;
                    let viewport = decoded.svg_viewport(tile.width, tile.height);
                    let decoded = viewport.as_ref().unwrap_or(&decoded);
                    let image = cached_skia_image(decoded)?;
                    let matrix = Matrix::new_all(
                        tile.width / image.width() as f32,
                        0.0,
                        tile.x,
                        0.0,
                        tile.height / image.height() as f32,
                        tile.y,
                        0.0,
                        0.0,
                        1.0,
                    );
                    image.to_shader(
                        Some((TileMode::Repeat, TileMode::Repeat)),
                        skia_safe::SamplingOptions::default(),
                        Some(&matrix),
                    )
                });
                if let Some(shader) = shader {
                    let mut paint = Paint::default();
                    paint.set_anti_alias(false);
                    paint.set_alpha_f(opacity.clamp(0.0, 1.0));
                    paint.set_shader(shader);
                    canvas.draw_rect(to_rect(layer.clip.rect), &paint);
                } else {
                    for tile in &layer.tiles {
                        // The background painting area owns edge coverage.
                        draw_image_with_edge_antialiasing(
                            canvas,
                            *tile,
                            &layer.source,
                            opacity,
                            false,
                            layer.snap_raster_axes,
                        );
                    }
                }
            }
            crate::background_image::BackgroundPaintLayer::Gradient(layer) => {
                for tile in &layer.geometry.tiles {
                    if let Some(shader) =
                        gradient_shader_for_layer(*tile, &layer.kind, &layer.stops)
                    {
                        let mut paint = Paint::default();
                        paint.set_anti_alias(true);
                        paint.set_alpha_f(opacity.clamp(0.0, 1.0));
                        paint.set_shader(shader);
                        canvas.draw_rect(to_rect(*tile), &paint);
                    }
                }
            }
        }
        canvas.restore_to_count(save);
    }
}

fn background_skia_blend(
    mode: crate::background_image::BackgroundBlendMode,
) -> skia_safe::BlendMode {
    use crate::background_image::BackgroundBlendMode as Mode;
    match mode {
        Mode::Normal => skia_safe::BlendMode::SrcOver,
        Mode::Multiply => skia_safe::BlendMode::Multiply,
        Mode::Screen => skia_safe::BlendMode::Screen,
        Mode::Overlay => skia_safe::BlendMode::Overlay,
        Mode::Darken => skia_safe::BlendMode::Darken,
        Mode::Lighten => skia_safe::BlendMode::Lighten,
        Mode::ColorDodge => skia_safe::BlendMode::ColorDodge,
        Mode::ColorBurn => skia_safe::BlendMode::ColorBurn,
        Mode::HardLight => skia_safe::BlendMode::HardLight,
        Mode::SoftLight => skia_safe::BlendMode::SoftLight,
        Mode::Difference => skia_safe::BlendMode::Difference,
        Mode::Exclusion => skia_safe::BlendMode::Exclusion,
    }
}

fn gradient_shader_for_layer(
    rect: LayoutRect,
    kind: &crate::background_image::GradientKind,
    stops: &[crate::background_image::GradientStop],
) -> Option<skia_safe::Shader> {
    let colors = stops
        .iter()
        .map(|stop| to_skia_color(stop.color, 1.0))
        .collect::<Vec<_>>();
    let positions = stops.iter().map(|stop| stop.position).collect::<Vec<_>>();
    match kind {
        crate::background_image::GradientKind::Linear { angle_degrees } => {
            let (start, end) =
                crate::background_image::linear_gradient_points(rect, *angle_degrees);
            gradient_shader::linear(
                (start, end),
                colors.as_slice(),
                positions.as_slice(),
                TileMode::Clamp,
                None,
                None,
            )
        }
        crate::background_image::GradientKind::Radial {
            center_x,
            center_y,
            shape,
        } => {
            let (center, (radius_x, radius_y)) =
                crate::background_image::radial_gradient_axes(rect, *center_x, *center_y, *shape);
            let radius = radius_x.max(radius_y);
            gradient_shader::radial(
                center,
                radius,
                colors.as_slice(),
                positions.as_slice(),
                TileMode::Clamp,
                None,
                None,
            )
        }
    }
}

fn draw_round_rect(canvas: &Canvas, rect: LayoutRect, radius: f32, paint: &Paint) {
    if radius <= 0.0 {
        let mut crisp = paint.clone();
        crisp.set_anti_alias(false);
        canvas.draw_rect(to_rect(rect), &crisp);
    } else {
        canvas.draw_round_rect(to_rect(rect), radius, radius, paint);
    }
}

fn draw_rounded_rect(canvas: &Canvas, rect: LayoutRect, radii: [f32; 4], paint: &Paint) {
    if radii.iter().all(|radius| *radius <= 0.0) {
        let mut crisp = paint.clone();
        crisp.set_anti_alias(false);
        canvas.draw_rect(to_rect(rect), &crisp);
        return;
    }
    let radii = radii.map(|radius| {
        let radius = radius.max(0.0);
        Vector::new(radius, radius)
    });
    let rounded = RRect::new_rect_radii(to_rect(rect), &radii);
    if !analytic_ellipse::draw(canvas, &rounded, paint) {
        canvas.draw_rrect(rounded, paint);
    }
}

fn to_rect(rect: LayoutRect) -> Rect {
    Rect::from_xywh(rect.x, rect.y, rect.width.max(0.0), rect.height.max(0.0))
}

thread_local! {
    // Preserve the N32 framebuffer, but avoid its legacy 256-denominator
    // integer SrcOver shortcut. Browser compositing rounds the normalized
    // premultiplied result, including grayscale glyph coverage, to 8 bits.
    // Compile once per paint thread; the effect has no uniforms or children.
    static CSS_SRC_OVER: Option<skia_safe::Blender> = skia_safe::RuntimeEffect::make_for_blender(
        "half4 main(half4 src, half4 dst) { return src + dst * (1 - src.a); }", None,
    ).ok().and_then(|effect| effect.make_blender(skia_safe::Data::new_empty(), None));
    // Opacity layers have floating-point alpha until the surface restore.
    // Preserve float32 SrcOver, then convert to UNORM8 without rounding a
    // near-half 255*x product to a false exact tie. 256*x is an exact binary
    // scale; splitting its integer part keeps the residual subtraction small.
    static CSS_OPACITY_SRC_OVER: Option<skia_safe::Blender> = skia_safe::RuntimeEffect::make_for_blender(
        "half4 main(half4 src, half4 dst) { float4 color = src + dst * (1 - src.a); float4 scaled = color * 256; float4 whole = floor(scaled); return half4((whole + floor(scaled - whole - color + .5)) / 255); }", None,
    ).ok().and_then(|effect| effect.make_blender(skia_safe::Data::new_empty(), None));
}

fn opacity_layer_paint(opacity: f32) -> Paint {
    let mut paint = color_paint(w3cos_std::Color::WHITE, 1.0);
    let opacity = opacity.clamp(0.0, 1.0);
    if opacity < 1.0 {
        CSS_OPACITY_SRC_OVER.with(|blender| {
            if let Some(blender) = blender { paint.set_blender(blender.clone()); }
        });
    }
    paint.set_alpha_f(opacity);
    paint
}

pub(crate) fn color_paint(color: w3cos_std::color::Color, opacity: f32) -> Paint {
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    CSS_SRC_OVER.with(|blender| {
        if let Some(blender) = blender { paint.set_blender(blender.clone()); }
    });
    paint.set_color(Color::from_argb(
        (color.a as f32 * opacity.clamp(0.0, 1.0)).round() as u8,
        color.r,
        color.g,
        color.b,
    ));
    paint
}

fn to_skia_color(color: w3cos_std::color::Color, opacity: f32) -> Color {
    Color::from_argb(
        (color.a as f32 * opacity.clamp(0.0, 1.0)).round() as u8,
        color.r,
        color.g,
        color.b,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_FONT: &[u8] = include_bytes!("../assets/Inter-Regular.ttf");

    #[test]
    fn fractional_square_three_dimensional_border_matches_snapped_paint_box() {
        for line_style in [w3cos_std::style::BorderLineStyle::Inset,w3cos_std::style::BorderLineStyle::Outset] {
            let render=|width:f32| {
                let mut surface=Surface::new_raster_n32_premul((80,220)).unwrap();
                surface.canvas().clear(Color::WHITE);
                let style=Style {border_width:1.0,border_color:w3cos_std::Color::rgb(128,128,128),
                    border_styles:[Some(line_style);4],..Style::default()};
                draw_box_border(surface.canvas(),LayoutRect {x:8.0,y:8.0,width,height:200.0},&style);
                let mut pixels=vec![0_u8;80*220*4];
                let info=ImageInfo::new((80,220),ColorType::RGBA8888,AlphaType::Premul,None);
                assert!(surface.read_pixels(&info,&mut pixels,80*4,(0,0)));
                pixels
            };
            let expected=render(55.0);
            for width in [55.328125,55.359375] {
                let actual=render(width);
                assert_eq!(actual.chunks_exact(4).zip(expected.chunks_exact(4)).filter(|(a,b)|a!=b).count(),0,
                    "{line_style:?} border paint endpoints must match the original browser command log");
            }
        }
    }

    #[test]
    fn relative_zero_outline_width_overrides_shorthand_medium() {
        for width in ["0em","-0em","+0em","0ex","-0ex","+0ex"] {
            let mut declaration=w3cos_dom::css_style::CSSStyleDeclaration::new();
            declaration.set_property("outline","solid red");
            declaration.set_property("outline-width",width);
            assert_eq!(declaration.inner.outline_width,0.0,"{width} must override medium");
        }
    }

    #[test]
    fn opaque_four_color_solid_border_joins_diagonally_without_a_white_seam() {
        let mut surface=Surface::new_raster_n32_premul((24,24)).unwrap();
        surface.canvas().clear(Color::WHITE);
        let style=Style {border_width:5.0,
            border_top_color:Some(w3cos_std::Color::rgb(255,165,0)),
            border_right_color:Some(w3cos_std::Color::rgb(128,0,128)),
            border_bottom_color:Some(w3cos_std::Color::rgb(0,128,128)),
            border_left_color:Some(w3cos_std::Color::rgb(255,255,0)),..Style::default()};
        draw_box_border(surface.canvas(),LayoutRect {x:2.0,y:2.0,width:20.0,height:20.0},&style);
        let info=ImageInfo::new((24,24),ColorType::RGBA8888,AlphaType::Premul,None);
        let mut pixels=vec![0u8;24*24*4];
        assert!(surface.read_pixels(&info,&mut pixels,24*4,(0,0)));
        let pixel=|x:usize,y:usize| &pixels[(y*24+x)*4..(y*24+x+1)*4];
        assert_eq!(pixel(3,2),[255,165,0,255],"top owns the corner above the miter");
        assert_eq!(pixel(2,3),[255,255,0,255],"left owns the corner below the miter");
        assert_eq!(pixel(2,2),[255,210,0,255],"AA join blends the two sides, not the white background");
        assert_eq!(pixel(2,21),[128,192,64,255],
            "Chromium141 rounds the 50% yellow/teal miter coverage to the nearest channel");
    }

    #[test]
    fn structural_table_columns_do_not_paint_outlines() {
        for display in [Display::TableColumn,Display::TableColumnGroup] {
            let mut surface=Surface::new_raster_n32_premul((40,30)).unwrap();
            surface.canvas().clear(Color::WHITE);
            let style=Style {display,outline_width:4.0,
                outline_style:w3cos_std::style::OutlineStyle::Solid,
                outline_color:w3cos_std::Color::rgb(128,0,128),..Style::default()};
            draw_box_outline(surface.canvas(),LayoutRect {x:8.0,y:8.0,width:24.0,height:16.0},&style);
            let info=ImageInfo::new((40,30),ColorType::RGBA8888,AlphaType::Premul,None);
            let mut pixels=vec![0_u8;40*30*4];
            assert!(surface.read_pixels(&info,&mut pixels,40*4,(0,0)));
            assert!(pixels.chunks_exact(4).all(|pixel|pixel==[255,255,255,255]));
        }
    }

    #[test]
    fn outline_shorthand_reaches_runtime_style() {
        let mut declaration=w3cos_dom::css_style::CSSStyleDeclaration::new();
        declaration.set_property("outline","solid purple 4px");
        assert_eq!(declaration.inner.outline_width,4.0);
        assert_eq!(declaration.inner.outline_style,w3cos_std::style::OutlineStyle::Solid);
        assert_eq!(declaration.inner.outline_color,w3cos_std::Color::rgb(128,0,128));
    }

    #[test]
    fn zero_height_block_paints_solid_outline_without_layout_growth() {
        let face = primary_typeface(TEST_FONT).unwrap();
        let metrics = test_font();
        let mut surface = Surface::new_raster_n32_premul((40, 30)).unwrap();
        surface.canvas().clear(Color::WHITE);
        let style = Style { outline_width: 4.0,
            outline_style: w3cos_std::style::OutlineStyle::Solid,
            outline_color: w3cos_std::Color::rgb(128,0,128), ..Style::default() };
        render_node(surface.canvas(),0,LayoutRect { x:8.0,y:12.0,width:24.0,height:0.0 },
            &ComponentKind::Box,&style,&face,&metrics,None,false,true);
        let info = ImageInfo::new((40,30),ColorType::RGBA8888,AlphaType::Premul,None);
        let mut pixels=vec![0_u8;40*30*4];
        assert!(surface.read_pixels(&info,&mut pixels,40*4,(0,0)));
        let purple=pixels.chunks_exact(4).filter(|p|*p==[128,0,128,255]).count();
        assert_eq!(purple,32*8,"empty border box retains outward outline ink");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn multiline_normal_text_includes_each_lines_resolved_fallback_metrics() {
        let style=Style {font_size:15.6,font_weight:700,font_family:Some("monospace".into()),
            white_space:w3cos_std::style::WhiteSpace::Pre,
            line_height:18.0/15.6,line_height_is_normal:true,..Style::default()};
        let text="א + - × ÷ \u{a0}\n\u{a0} + - × ÷ ת";
        assert_eq!(resolved_text_font_geometry("א + - × ÷ \u{a0}",&style).unwrap().line_spacing(),19.0);
        assert_eq!(measure_skia_text_intrinsic_size(text,&style).1,38.0,
            "Chromium141: two normal lines containing the fallback face occupy 38px");
        assert_eq!(measure_skia_wrapped_text_height(text,200.0,&style),38.0);
        let explicit=Style {line_height_is_normal:false,..style.clone()};
        assert_eq!(measure_skia_text_intrinsic_size(text,&explicit).1,
            2.0 * crate::layout::inline_style_line_height(&explicit),
            "explicit line-height must not expand to fallback metrics");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn trailing_bidi_control_keeps_a_fitting_complete_run_on_one_line() {
        let style=Style { font_size:15.6,font_weight:700,font_family:Some("monospace".into()),
            line_height:18.0/15.6,line_height_is_normal:true,..Style::default() };
        let text="ת + - × ÷ \u{a0}\u{200f}";
        let (width,height)=measure_skia_text_intrinsic_size(text,&style);
        assert_eq!(measure_skia_wrapped_text_height(text,
            text_layout::inline_layout_advance(width),&style),height,
            "prefix shaping must not break a complete run that fits after its trailing control");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn rtl_marked_fallback_line_preserves_one_resolved_segment_advance() {
        let style = Style { font_size:15.6, font_weight:700,
            font_family:Some("monospace".into()), ..Style::default() };
        let mut visual = style.clone();
        visual.custom_properties.get_or_insert_with(Default::default)
            .insert("--w3cos-internal-bidi-visual-order".into(),"1".into());
        let logical="\u{200f}\u{a0} + - × ÷ א";
        let rendered=text_layout::font_render_text_for_style(logical,&style);
        assert_eq!(rendered,"א ÷ × - +  ");
        let natural=crate::layout::text_intrinsic_size(logical,&style).0;
        // Keep the authored NBSP until CSS white-space processing, just as
        // the override control does; rendered ordinary spaces would collapse.
        let override_width=crate::layout::text_intrinsic_size("א ÷ × - + \u{a0}",&visual).0;
        assert_eq!(natural,override_width,
            "removing RLM must not invent a second bidi segment boundary");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mirrored_rtl_paragraph_matches_equivalent_ltr_glyph_pixels() {
        let primary=host_typeface().unwrap();
        let mut captures=Vec::new();
        for (text,direction) in [("(a b א) c d",w3cos_std::style::TextDirection::Ltr),
            ("c d (א a b)",w3cos_std::style::TextDirection::Rtl)] {
            let style=Style {font_size:16.0,direction,..Style::default()};
            let mut surface=Surface::new_raster_n32_premul((160,40)).unwrap();
            surface.canvas().clear(Color::WHITE);
            let advance=draw_text_glyph_line(surface.canvas(),8.0,8.0,text,16.0,
                w3cos_std::Color::BLACK,1.0,&primary,&style);
            let info=ImageInfo::new((160,40),ColorType::RGBA8888,AlphaType::Premul,None);
            let mut pixels=vec![0;160*40*4];
            assert!(surface.read_pixels(&info,&mut pixels,160*4,(0,0)));
            captures.push((advance,pixels));
        }
        assert_eq!(captures[0].1.iter().zip(&captures[1].1).filter(|(a,b)|a!=b).count(),0,
            "Chromium bidi-glyph-mirroring-002 renders both equivalent paragraphs identically; advances {:?}",
            [captures[0].0,captures[1].0]);
    }

    #[test]
    fn fractional_normal_inline_origin_keeps_the_browser_bottom_border_row() {
        let rect=floor_inline_decoration_origin(LayoutRect {
            x:151.4375,y:83.40625,width:120.421875,height:41.1875});
        let style=Style {border_width:3.0,border_color:w3cos_std::Color::rgb(255,165,0),
            background:w3cos_std::Color::rgb(255,255,0),..Style::default()};
        let mut surface=Surface::new_raster_n32_premul((300,140)).unwrap();
        surface.canvas().clear(Color::WHITE);
        draw_rounded_rect(surface.canvas(),rect,[0.0;4],&color_paint(style.background,1.0));
        draw_box_border(surface.canvas(),rect,&style);
        let info=ImageInfo::new((300,140),ColorType::RGBA8888,AlphaType::Premul,None);
        let mut pixels=vec![0;300*140*4];
        assert!(surface.read_pixels(&info,&mut pixels,300*4,(0,0)));
        let pixel=|x:usize,y:usize|&pixels[(y*300+x)*4..(y*300+x)*4+4];
        assert_eq!(pixel(160,121),[255,255,0,255],"Chromium bidi011: interior above bottom border");
        assert_eq!(pixel(160,124),[255,165,0,255],"Chromium bidi011: last bottom border row");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn right_aligned_standard_text_keeps_browser_final_glyph_coverage() {
        let face=host_typeface().unwrap();
        let mut style=Style {display:Display::Block,text_align:TextAlign::Right,color:w3cos_std::Color::BLACK,
            font_size:16.0,line_height:1.375,line_height_is_normal:true,..Style::default()};
        style.custom_properties.get_or_insert_with(Default::default)
            .insert(w3cos_dom::user_agent::HTML_STANDARD_FONT_PROPERTY.into(),"1".into());
        let mut surface=Surface::new_raster_n32_premul((800,100)).unwrap();
        surface.canvas().clear(Color::WHITE);
        draw_text_in_rect(surface.canvas(),LayoutRect {x:8.0,y:54.0,width:784.0,height:22.0},
            "PASS PASS",&style,&face,crate::layout::layout_font());
        let info=ImageInfo::new((800,100),ColorType::RGBA8888,AlphaType::Premul,None);
        let mut pixels=vec![0;800*100*4];
        assert!(surface.read_pixels(&info,&mut pixels,800*4,(0,0)));
        assert_eq!(&pixels[(58*800+785)*4..(58*800+785)*4+4],&[241,241,241,255],
            "Chromium unicode-bidi-applies-to009 final S; advance={}",
            measure_skia_text_advance("PASS PASS",&face,&style));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn hebrew_fallback_keeps_browser_following_run_origin() {
        let primary = FontMgr::default().match_family_style("PingFang SC", FontStyle::normal()).unwrap();
        let style = Style { font_size: 16.0, font_weight: 400, ..Style::default() };
        let prefix = "There should be a perfect 6×6 grid of squares below. (Force bidi: א";
        let cursor = font_stack_advance(prefix, &primary, &style);
        assert_eq!(text_layout::inline_layout_advance(cursor), 503.125,
            "Chromium141 v2028: following parenthesis starts at 511.125 from body origin 8px");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn browser_times_bold_normal_metrics_keep_the_same_16px_line_height() {
        for weight in [400, 700] {
            let primary = FontMgr::default().match_family_style("Times", FontStyle::normal()).unwrap();
            let face = typeface_for_character(&primary, 'x', weight);
            let metrics = typeface_font_geometry(&face, 16.0);
            assert_eq!(metrics.line_spacing(), 18.0,
                "Chromium141 v1996: Times regular/bold share the 18px normal line");
            let style = Style { font_size: 16.0, font_weight: weight,
                font_family: Some("serif".into()), line_height_is_normal: true,
                ..Style::default() };
            assert_eq!(resolved_text_font_geometry(". (Force bidi: א)", &style).unwrap().line_spacing(), 18.0,
                "Chromium141 v1996: the Hebrew fallback preserves the 18px paragraph");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn font_fallback_retains_primary_canonical_decomposition() {
        let primary = FontMgr::default().match_family_style("PingFang SC", FontStyle::normal()).unwrap();
        let style = Style { font_size: 16.0, font_weight: 400,
            font_family: Some("PingFang SC".into()), ..Style::default() };
        for character in ['Ć','Ĉ','Č','Ď'] {
            assert_eq!(primary.unichar_to_glyph(character as i32), 0,
                "fixture requires a character absent from the nominal cmap");
            let text = character.to_string();
            let run = crate::skia_text_run::shape_visual_run(&text, &primary, &style).unwrap();
            assert!(run.glyphs.len()>1 && run.glyphs.iter().all(|glyph| *glyph!=0),
                "primary must cover the canonical base/mark cluster for {character}");
            let actual = typeface_for_character(&primary, character, 400);
            assert_eq!(actual.unique_id(), primary.unique_id(),
                "{character}: successful shaping must precede system fallback");
        }
    }

    #[test]
    fn html_control_round_corners_match_browser_coverage() {
        // Browser141 actual/reference captures in v1862, not a tolerance.
        let typeface = FontMgr::default().new_from_data(TEST_FONT, None).unwrap();
        for (part, kind, fill, grays) in [
            ("textfield", ComponentKind::TextInput { value: String::new(), placeholder: String::new(), secure: false }, w3cos_std::Color::WHITE, [232,148,118]),
            ("button", ComponentKind::Button { label: String::new() }, w3cos_std::Color::rgb(239,239,239), [232,147,118]),
        ] {
            for (x, width) in [(3.0, 30.0), (7.0, 37.0)] {
                let mut style = Style { background: fill, border_width: 2.0,
                    border_color: w3cos_std::Color::rgb(118,118,118), ..Style::default() };
                style.custom_properties.get_or_insert_with(Default::default)
                    .insert("--w3cos-internal-html-control-appearance".into(), part.into());
                let mut surface = Surface::new_raster_n32_premul((60,30)).unwrap();
                surface.canvas().clear(Color::WHITE);
                render_node(surface.canvas(), 0, LayoutRect { x, y: 3.0, width, height: 21.0 },
                    &kind, &style, &typeface, &test_font(), None, false, false);
                let info = ImageInfo::new((60,30), ColorType::RGBA8888, AlphaType::Premul, None);
                let mut pixels = vec![0_u8; 60*30*4];
                assert!(surface.read_pixels(&info, &mut pixels, 60*4, (0,0)));
                for (dx, gray) in grays.into_iter().enumerate() {
                    let offset = (3*60+x as usize+dx)*4;
                    assert_eq!(&pixels[offset..offset+4], &[gray,gray,gray,255],
                        "{part}: x={x}, width={width}, corner column={dx}");
                }
            }
        }
    }

    #[test]
    fn html_button_centers_font_line_not_label_ink() {
        let typeface = FontMgr::default().new_from_data(TEST_FONT, None).unwrap();
        let metrics = test_font();
        let mut style = Style { font_size: 16.0, line_height: 1.5, color: w3cos_std::Color::BLACK,
            font_family: Some("Inter".into()), padding: w3cos_std::style::Edges::all(2.0),
            border_width: 2.0, ..Style::default() };
        style.custom_properties.get_or_insert_with(Default::default)
            .insert("--w3cos-internal-html-control-appearance".into(), "button".into());
        let rect = LayoutRect { x: 3.0, y: 3.0, width: 100.0, height: 42.0 };
        let content = text_paint_box(rect, &style);
        let font_height = resolved_font_geometry(&style).unwrap().height();
        for text in ["A", "gy", "Av"] {
            let mut actual = Surface::new_raster_n32_premul((110, 50)).unwrap();
            let mut expected = Surface::new_raster_n32_premul((110, 50)).unwrap();
            actual.canvas().clear(Color::WHITE);
            expected.canvas().clear(Color::WHITE);
            draw_centered_text(actual.canvas(), rect, text, &style, &typeface, &metrics);
            let advance = measure_skia_text_advance(text, &typeface, &style);
            draw_text_line(expected.canvas(), content.x + (content.width - advance) * 0.5,
                content.y + (content.height - font_height) * 0.5,
                text, style.font_size, style.color, style.opacity, &typeface, &style);
            let info = ImageInfo::new((110, 50), ColorType::RGBA8888, AlphaType::Premul, None);
            let mut a = vec![0_u8; 110 * 50 * 4];
            let mut b = a.clone();
            assert!(actual.read_pixels(&info, &mut a, 110 * 4, (0, 0)));
            assert!(expected.read_pixels(&info, &mut b, 110 * 4, (0, 0)));
            assert!(a.chunks_exact(4).any(|pixel| pixel[..3] != [255, 255, 255]),
                "{text}: a blank raster cannot prove label alignment");
            assert_eq!(a.iter().zip(&b).filter(|(a,b)| a != b).count(), 0,
                "{text}: label ink must not move the shared font baseline");
        }
    }

    #[test]
    fn html_auto_control_theme_keeps_layout_border_but_paints_one_pixel() {
        let typeface = FontMgr::default().new_from_data(TEST_FONT, None).unwrap();
        for (part, kind, fill) in [
            ("textfield", ComponentKind::TextInput { value: String::new(), placeholder: String::new(), secure: false }, w3cos_std::Color::WHITE),
            ("button", ComponentKind::Button { label: String::new() }, w3cos_std::Color::rgb(239, 239, 239)),
        ] {
            let mut style = Style { background: fill, border_width: 2.0,
                border_color: w3cos_std::Color::rgb(118, 118, 118), ..Style::default() };
            style.custom_properties.get_or_insert_with(Default::default)
                .insert("--w3cos-internal-html-control-appearance".into(), part.into());
            let mut surface = Surface::new_raster_n32_premul((40, 30)).unwrap();
            surface.canvas().clear(Color::WHITE);
            render_node(surface.canvas(), 0, LayoutRect { x: 3.0, y: 3.0, width: 30.0, height: 21.0 },
                &kind, &style, &typeface, &test_font(), None, false, false);
            let info = ImageInfo::new((40, 30), ColorType::RGBA8888, AlphaType::Premul, None);
            let mut pixels = vec![0_u8; 40 * 30 * 4];
            assert!(surface.read_pixels(&info, &mut pixels, 40 * 4, (0, 0)));
            assert_eq!(&pixels[(4 * 40 + 15) * 4..(4 * 40 + 15) * 4 + 4],
                &[fill.r, fill.g, fill.b, 255], "{part}: second border row must be control fill");
            assert_eq!(style.border_width, 2.0, "theme painting must not mutate layout");
        }
    }

    #[test]
    fn filled_circle_edges_match_default_graphite_browser() {
        // V605 DEFAULT Chromium141 capture, V613 backend attestation.
        // All channels are exact, not a tolerance around legacy raster AA.
        for (size, x, y, gray) in [(4.0, 3, 3, 158u8), (4.0, 4, 3, 7),
                                  (8.0, 4, 3, 205), (8.0, 5, 3, 77)] {
            let mut surface = Surface::new_raster_n32_premul((16, 16)).unwrap();
            surface.canvas().clear(Color::WHITE);
            draw_rounded_rect(surface.canvas(),
                LayoutRect { x: 3.0, y: 3.0, width: size, height: size },
                [size * 0.5; 4],
                &color_paint(w3cos_std::Color { r: 0, g: 0, b: 0, a: 255 }, 1.0));
            let info = skia_safe::ImageInfo::new((16, 16), skia_safe::ColorType::RGBA8888,
                skia_safe::AlphaType::Premul, None);
            let mut pixels = [0u8; 16 * 16 * 4];
            assert!(surface.read_pixels(&info, &mut pixels, 16 * 4, (0, 0)));
            let offset = (y * 16 + x) * 4;
            assert_eq!(&pixels[offset..offset + 4], &[gray, gray, gray, 255],
                "DEFAULT Graphite circle size={size}, pixel=({x},{y})");
        }
    }

    #[test]
    fn composited_oval_preserves_background_layer_boundary() {
        use crate::paint_artifact::PaintNode;
        let host = LayoutRect { x: 0.0, y: 0.0, width: 16.0, height: 16.0 };
        let oval = LayoutRect { x: 3.0, y: 3.0, width: 6.0, height: 6.0 };
        // V745 DEFAULT browser: a separately composited oval quantizes its
        // transparent surface before blend; an opaque parent shared with the
        // background does not quantize the intermediate coverage.
        for owner in [None, Some(1), Some(0)] {
            let mut host_style = Style { background: w3cos_std::Color::rgb(255, 165, 0),
                ..Style::default() };
            let mut oval_style = Style { background: w3cos_std::Color::BLACK,
                border_radius: 3.0, ..Style::default() };
            if owner == Some(0) { host_style.will_change.transform = true; }
            if owner == Some(1) { oval_style.will_change.transform = true; }
            let artifact = PaintArtifact::build([
                PaintNode { kind: ComponentKind::Root, style: Style::default(), parent: None, sticky_counter_signal: None },
                PaintNode { kind: ComponentKind::Column, style: host_style, parent: Some(0), sticky_counter_signal: None },
                PaintNode { kind: ComponentKind::Box, style: oval_style, parent: Some(1), sticky_counter_signal: None },
            ], &[(host, 0), (host, 1), (oval, 2)], 1);
            let nodes = artifact.nodes.iter().enumerate().map(|(i, n)|
                (i, artifact.rect_by_index[i].unwrap(), &n.kind, &n.style)).collect::<Vec<_>>();
            let metrics = fontdue::Font::from_bytes(TEST_FONT, fontdue::FontSettings::default()).unwrap();
            let mut rasterizer = SkiaRasterizer::new(TEST_FONT).unwrap();
            let pixels = rasterizer.render_frame(16, 16, &nodes, &metrics, &[None, None, None],
                &HashMap::new(), None, w3cos_std::Color::WHITE, Some(&artifact), None, 1.0).unwrap();
            let green = if owner == Some(1) { [69, 1] } else { [68, 2] };
            for (x, red, expected_green) in [(4, 106, green[0]), (5, 2, green[1])] {
                let offset = (3 * 16 + x) * 4;
                assert_eq!(&pixels[offset..offset + 4], &[red, expected_green, 0, 255],
                    "browser surface owner={owner:?}, pixel=({x},3)");
            }
            let expected = pixels.to_vec();
            let replayed = rasterizer.render_frame(16, 16, &nodes, &metrics, &[None, None, None],
                &HashMap::new(), None, w3cos_std::Color::WHITE, Some(&artifact), None, 1.0).unwrap();
            assert_eq!(replayed, expected, "retained surface replay owner={owner:?}");
            assert_eq!(rasterizer.retained_replays(), 1);
            let mut surface = Surface::new_raster_n32_premul((16, 16)).unwrap();
            paint_display_list(surface.canvas(), &primary_typeface(TEST_FONT).unwrap(), ReplayFrame {
                nodes: &nodes, metrics_font: &metrics, scroll_info: &[None, None, None],
                text_input_values: &HashMap::new(), focused_index: None,
                background: w3cos_std::Color::WHITE, artifact: Some(&artifact),
                retained: None, compositor_overrides: None, scale_factor: 1.0,
            }, true);
            let info = ImageInfo::new((16, 16), ColorType::RGBA8888, AlphaType::Premul, None);
            let mut baked = vec![0u8; 16 * 16 * 4];
            assert!(surface.read_pixels(&info, &mut baked, 16 * 4, (0, 0)));
            assert_eq!(baked, expected, "baked surface owner={owner:?}");
        }
    }

    #[test]
    fn glyph_clip_keeps_filtered_ink_conservative() {
        use crate::paint_artifact::PaintNode;
        let rect = LayoutRect { x: 0.0, y: 0.0, width: 20.0, height: 20.0 };
        let mut artifact = PaintArtifact::build([
            PaintNode { kind: ComponentKind::Column, style: Style::default(), parent: None, sticky_counter_signal: None },
            PaintNode { kind: ComponentKind::Column, style: Style { filter: Some("blur(4px)".into()),
                ..Style::default() }, parent: Some(0), sticky_counter_signal: None },
            PaintNode { kind: ComponentKind::Text { content: "H".into() }, style: Style::default(),
                parent: Some(1), sticky_counter_signal: None },
        ], &[(rect, 0), (rect, 1), (rect, 2)], 1);
        let layer = crate::retained_layers::CompositorLayer {
            id: 0, bounds: rect, properties: artifact.node_properties[2], chunk_ids: Vec::new(),
            client_indices: vec![2], sticky_owner: None, effect_owner: None,
            transform_owner: None, scroll_host: None,
        };
        let viewport = Rect::from_xywh(0.0, 0.0, 20.0, 20.0);
        let overrides = CompositorOverrides::default();
        assert!(layer_glyph_clip(&layer, &artifact, &[], &overrides, 1.0, viewport).is_none(),
            "inherited filters can spread glyph ink into the viewport");
        for effect in &mut artifact.properties.effects { effect.filter = None; }
        assert_eq!(layer_glyph_clip(&layer, &artifact, &[], &overrides, 1.0, viewport), Some(viewport));
    }

    #[test]
    fn glyph_clip_recording_preserves_reentry_and_reuses_unchanged_visibility() {
        let face = primary_typeface(TEST_FONT).unwrap();
        let style = Style { font_size: 20.0, ..Style::default() };
        let font = crate::skia_text_run::css_font(&face, 20.0);
        let run = crate::skia_text_run::shape_visual_run("H", &face, &style).unwrap();
        let ink = run.geometric_ink_bounds(&font).unwrap();
        let mut surface = Surface::new_raster_n32_premul((160, 64)).unwrap();
        let clip = Rect::new(13.0, 0.0, 113.0, 64.0);
        let x = 113.0 - ink.left;
        let recording = GlyphClipRecordingScope::new(Some(clip));
        assert!(!glyph_run_intersects_clip(surface.canvas(), &font, &run, x, 32.0));
        let decisions = recording.finish();
        assert_eq!(decisions.len(), 1);
        assert!(glyph_clip_decisions_match(&decisions, Some(clip)));
        assert!(glyph_clip_decisions_match(&decisions, Some(Rect::new(0.0, 0.0, 100.0, 64.0))));
        let shifted = Rect::new(13.0, 0.0, 123.0, 64.0);
        assert!(!glyph_clip_decisions_match(&decisions, Some(shifted)), "entering ink must invalidate the recording");
        let recording = GlyphClipRecordingScope::new(Some(shifted));
        assert!(glyph_run_intersects_clip(surface.canvas(), &font, &run, x, 32.0));
        let visible = recording.finish();
        assert!(!glyph_clip_decisions_match(&visible, Some(clip)), "leaving ink must invalidate the recording");
        assert!(glyph_run_intersects_clip(surface.canvas(), &font, &run, x, 32.0), "scope must restore ordinary drawing");
    }

    #[test]
    fn glyph_clip_retained_scroll_reentry_never_loses_text() {
        use crate::paint_artifact::PaintNode;
        let host = LayoutRect { x: 0.0, y: 0.0, width: 8.0, height: 8.0 };
        let text = LayoutRect { x: 1.0, y: 10.0, width: 6.0, height: 4.0 };
        let artifact = PaintArtifact::build([
            PaintNode { kind: ComponentKind::Column, style: Style::default(), parent: None, sticky_counter_signal: None },
            PaintNode { kind: ComponentKind::Column, style: Style { overflow: w3cos_std::style::Overflow::Hidden,
                ..Style::default() }, parent: Some(0), sticky_counter_signal: None },
            PaintNode { kind: ComponentKind::Text { content: "H".into() }, style: Style { display: Display::Block,
                font_size: 4.0, line_height: 1.0, color: w3cos_std::Color::rgb(255, 0, 0), ..Style::default() },
                parent: Some(1), sticky_counter_signal: None },
        ], &[(host, 0), (host, 1), (text, 2)], 1);
        let nodes = artifact.nodes.iter().enumerate().filter_map(|(index, node)|
            Some((index, artifact.rect_by_index[index]?, &node.kind, &node.style))).collect::<Vec<_>>();
        let metrics = fontdue::Font::from_bytes(TEST_FONT, fontdue::FontSettings::default()).unwrap();
        let mut rasterizer = SkiaRasterizer::new(TEST_FONT).unwrap();
        let mut render = |offset| rasterizer.render_frame(8, 8, &nodes, &metrics,
            &[None, None, Some((0.0, offset, host))], &HashMap::new(), None,
            w3cos_std::Color::WHITE, Some(&artifact), None, 1.0).unwrap().to_vec();
        let hidden = render(0.0);
        assert!(hidden.chunks_exact(4).all(|pixel| pixel == [255, 255, 255, 255]));
        let entered = render(6.0);
        assert!(entered.chunks_exact(4).any(|pixel| pixel != [255, 255, 255, 255]));
        render(6.125);
        assert_eq!(render(0.0), hidden);
        assert_eq!(render(6.0), entered, "repeated scroll reentry must restore the exact glyphs");
        assert_eq!(rasterizer.retained_replays(), 1, "unchanged visibility reuses the recording");
        assert_eq!(rasterizer.retained_rebuilds(), 4, "only visibility transitions rerecord");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn html_standard_light_weight_selects_real_platform_face() {
        let mut style = w3cos_dom::user_agent::html_default_style("html");
        style.font_weight = 100;
        style.custom_properties.get_or_insert_with(Default::default)
            .insert(w3cos_dom::user_agent::TEXT_LANGUAGE_PROPERTY.into(), "zh-CN".into());
        let base = generic_serif_typeface(&style).unwrap();
        let expected = FontMgr::default().match_family_style(&base.family_name(),
            FontStyle::new(100.into(), skia_safe::font_style::Width::NORMAL,
                skia_safe::font_style::Slant::Upright)).unwrap();
        assert!(expected.font_style().weight() < base.font_style().weight());
        for run in css_font_runs("Filler Text", &base, &style) {
            assert_eq!(run.typeface.font_style(), expected.font_style(),
                "light text must use the installed light face across measurement and paint");
        }
        let metrics = resolved_font_geometry(&style).unwrap();
        let expected_metrics = typeface_font_geometry(&expected, style.font_size);
        assert_eq!((metrics.ascent, metrics.descent),
            (expected_metrics.ascent, expected_metrics.descent));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn generic_italic_uses_installed_slanted_face_before_synthesis() {
        for family in [None, Some("serif"), Some("sans-serif"), Some("monospace")] {
            for weight in [400, 700] {
                let mut style = w3cos_dom::user_agent::html_default_style("html");
                style.font_family = family.map(str::to_string);
                style.font_style = w3cos_std::style::FontStyle::Italic;
                style.font_weight = weight;
                style.custom_properties.get_or_insert_with(Default::default)
                    .insert(w3cos_dom::user_agent::TEXT_LANGUAGE_PROPERTY.into(), "es".into());
                let base = generic_serif_typeface(&style).unwrap();
                let expected = FontMgr::default().match_family_style(&base.family_name(),
                    FontStyle::new((weight as i32).into(), skia_safe::font_style::Width::NORMAL,
                        skia_safe::font_style::Slant::Italic)).unwrap();
                let runs = css_font_runs("and this should be green", &base, &style);
                assert!(!runs.is_empty());
                for run in runs {
                    assert_eq!(run.typeface.font_style(), expected.font_style(),
                        "generic={family:?}, weight={weight}: use the installed italic, not a sheared normal face");
                    assert_eq!(crate::skia_text_run::css_font_for_style(&run.typeface,
                        16.0, &style).skew_x(), 0.0);
                }
            }
        }
    }

    #[test]
    fn explicit_sans_uses_host_browser_metrics_for_short_line_height() {
        let style = Style { font_family: Some("sans-serif".into()), font_size: 32.0,
            font_weight: 900, line_height: 1.0, display: Display::Block, ..Style::default() };
        let face = HTML_STANDARD_TYPEFACE.with(Clone::clone).expect("host browser font");
        assert_eq!(generic_serif_typeface(&style).unwrap().unique_id(), face.unique_id());
        let bold = typeface_for_character(&face, 'x', style.font_weight);
        let expected = typeface_font_geometry(&bold, style.font_size);
        let actual = resolved_font_geometry(&style).expect("explicit sans font metrics");
        assert_eq!(actual.ascent, expected.ascent);
        assert_eq!(actual.descent, expected.descent);
        assert_eq!(text_baseline(0.0, 32.0, &face, &style, "PASS"), expected.ascent);
        assert_eq!(line_box_half_leading(&style), ((32.0 - expected.height()) * 0.5).floor());
    }

    #[test]
    fn opacity_layer_composition_matches_browser_pixels_and_retained_replay() {
        CSS_OPACITY_SRC_OVER.with(|blender| assert!(blender.is_some()));
        use crate::paint_artifact::PaintNode;
        let rect = LayoutRect { x: 1.0, y: 1.0, width: 4.0, height: 4.0 };
        for (opacity, color, expected) in [
            (0.5, w3cos_std::Color::rgb(0,128,0), [128,191,128,255]),
            (0.9, w3cos_std::Color::rgb(0,0,255), [26,26,255,255]),
            (1.0, w3cos_std::Color::rgb(0,128,0), [0,128,0,255]),
        ] {
            let artifact = PaintArtifact::build([
                PaintNode { kind: ComponentKind::Box, style: Style { opacity,
                    ..Style::default() }, parent: None, sticky_counter_signal: None },
                PaintNode { kind: ComponentKind::Box, style: Style { background: color,
                    ..Style::default() }, parent: Some(0), sticky_counter_signal: None },
            ], &[(rect,0),(rect,1)], 1);
            let nodes = artifact.nodes.iter().enumerate().map(|(i,n)|
                (i,rect,&n.kind,&n.style)).collect::<Vec<_>>();
            let mut rasterizer = SkiaRasterizer::new(TEST_FONT).unwrap();
            for _ in 0..2 {
                let pixels = rasterizer.render_frame(8,8,&nodes,&test_font(),&[],
                    &HashMap::new(),None,w3cos_std::Color::WHITE,Some(&artifact),None,1.0).unwrap();
                assert_eq!(&pixels[(2*8+2)*4..(2*8+2)*4+4], &expected,
                    "browser opacity layer {opacity}");
            }
            assert_eq!(rasterizer.retained_replays(),1);
            let mut surface = Surface::new_raster_n32_premul((8,8)).unwrap();
            paint_display_list(surface.canvas(), &primary_typeface(TEST_FONT).unwrap(), ReplayFrame {
                nodes:&nodes,metrics_font:&test_font(),scroll_info:&[],text_input_values:&HashMap::new(),
                focused_index:None,background:w3cos_std::Color::WHITE,artifact:Some(&artifact),
                retained:None,compositor_overrides:None,scale_factor:1.0,
            },true);
            let mut pixels = [0u8;8*8*4];
            let info = ImageInfo::new((8,8),ColorType::RGBA8888,AlphaType::Premul,None);
            assert!(surface.read_pixels(&info,&mut pixels,8*4,(0,0)));
            assert_eq!(&pixels[(2*8+2)*4..(2*8+2)*4+4],&expected,
                "baked opacity layer {opacity}");
        }
    }

    #[test]
    fn opacity_layer_calibration_matches_browser_solid_colors() {
        use crate::paint_artifact::PaintNode;
        // Chromium141 V2647: four colors, two destinations, five opacities.
        // These independent screenshot bytes are not generated by the shader.
        let expected = [
            [229,242,229],[191,223,191],[128,191,128],[102,179,102],[26,141,26],
            [12,51,78],[10,64,65],[7,85,44],[5,94,35],[1,119,9],
            [229,229,255],[191,191,255],[128,128,255],[102,102,255],[26,26,255],
            [12,38,104],[10,32,129],[7,21,171],[5,17,188],[1,4,238],
            [233,239,251],[200,216,244],[146,177,233],[124,161,229],[59,115,215],
            [15,48,99],[19,56,118],[25,71,149],[27,76,161],[35,93,199],
            [255,246,229],[255,233,191],[255,210,128],[255,201,102],[255,174,26],
            [37,54,78],[74,73,65],[134,104,44],[158,116,35],[231,153,9],
        ];
        let rect = LayoutRect { x:1.0,y:1.0,width:4.0,height:4.0 };
        let mut case = 0;
        for color in [[0,128,0],[0,0,255],[37,99,211],[255,165,0]] {
            for bg in [[255,255,255],[13,42,87]] {
                for opacity in [0.1,0.25,0.5,0.6,0.9] {
                    let artifact = PaintArtifact::build([
                        PaintNode { kind:ComponentKind::Box,style:Style { opacity,
                            ..Style::default() },parent:None,sticky_counter_signal:None },
                        PaintNode { kind:ComponentKind::Box,style:Style {
                            background:w3cos_std::Color::rgb(color[0],color[1],color[2]),
                            ..Style::default() },parent:Some(0),sticky_counter_signal:None },
                    ],&[(rect,0),(rect,1)],1);
                    let nodes = artifact.nodes.iter().enumerate().map(|(i,n)|
                        (i,rect,&n.kind,&n.style)).collect::<Vec<_>>();
                    let mut rasterizer = SkiaRasterizer::new(TEST_FONT).unwrap();
                    let pixels = rasterizer.render_frame(8,8,&nodes,&test_font(),&[],
                        &HashMap::new(),None,w3cos_std::Color::rgb(bg[0],bg[1],bg[2]),
                        Some(&artifact),None,1.0).unwrap();
                    assert_eq!(&pixels[(2*8+2)*4..(2*8+2)*4+3],&expected[case],
                        "browser calibration {case}: {color:?} over {bg:?}, opacity={opacity}");
                    case += 1;
                }
            }
        }
        assert_eq!(case,40);
    }

    #[test]
    fn css_src_over_rounds_normalized_alpha_without_a_float_framebuffer() {
        CSS_SRC_OVER.with(|blender| assert!(blender.is_some()));
        let mut surface = Surface::new_raster_n32_premul((256, 1)).unwrap();
        surface.canvas().clear(Color::from_rgb(255, 165, 0));
        for alpha in 0..256 {
            surface.canvas().draw_rect(Rect::from_xywh(alpha as f32, 0.0, 1.0, 1.0),
                &color_paint(w3cos_std::Color::rgba(0, 0, 0, alpha as u8), 1.0));
        }
        let info = ImageInfo::new((256, 1), ColorType::RGBA8888, AlphaType::Premul, None);
        let mut pixels = vec![0_u8; 1024];
        assert!(surface.read_pixels(&info, &mut pixels, 1024, (0, 0)));
        for (alpha, pixel) in pixels.chunks_exact(4).enumerate() {
            let green = (165.0 * (255 - alpha) as f32 / 255.0).round() as u8;
            assert_eq!(pixel, &[255 - alpha as u8, green, 0, 255], "alpha={alpha}");
        }
    }

    #[test]
    fn glyph_clip_rejects_vector_ink_outside_but_keeps_partial_overlap() {
        let face = primary_typeface(TEST_FONT).unwrap();
        let style = Style { font_size: 20.0, ..Style::default() };
        let font = crate::skia_text_run::css_font(&face, 20.0);
        let run = crate::skia_text_run::shape_visual_run("H", &face, &style).unwrap();
        let ink = run.geometric_ink_bounds(&font).unwrap();
        let mut surface = Surface::new_raster_n32_premul((160, 64)).unwrap();
        let canvas = surface.canvas();
        canvas.clear(Color::WHITE);
        canvas.clip_rect(Rect::new(13.0, 0.0, 113.0, 64.0), None, Some(false));
        let boundary = 113.0 - ink.left;
        assert!(!glyph_run_intersects_clip(canvas, &font, &run, boundary, 32.0));
        assert!(glyph_run_intersects_clip(canvas, &font, &run, boundary - 0.5, 32.0));
        // A translated canvas must compare device-space ink and clip bounds.
        canvas.translate((10.0, 0.0));
        assert!(!glyph_run_intersects_clip(canvas, &font, &run, boundary, 32.0));
        assert!(glyph_run_intersects_clip(canvas, &font, &run, boundary - 10.5, 32.0));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn glyph_clip_preserves_a_real_negative_bearing() {
        let face = FontMgr::default().match_family_style("Times", FontStyle::italic()).unwrap();
        let style = Style { font_size: 20.0, ..Style::default() };
        let font = crate::skia_text_run::css_font(&face, 20.0);
        let run = crate::skia_text_run::shape_visual_run("j", &face, &style).unwrap();
        assert!(run.geometric_ink_bounds(&font).unwrap().left < 0.0);
        let mut surface = Surface::new_raster_n32_premul((160, 64)).unwrap();
        let canvas = surface.canvas();
        canvas.clip_rect(Rect::new(13.0, 0.0, 113.0, 64.0), None, Some(false));
        assert!(glyph_run_intersects_clip(canvas, &font, &run, 113.0, 32.0),
            "a real overhang is ink inside the clip even when its advance origin lies outside");
    }

    #[test]
    fn html_standard_face_is_shared_by_glyphs_and_metrics_not_explicit_serif() {
        let mut style = w3cos_dom::user_agent::html_default_style("html");
        let standard = generic_serif_typeface(&style).expect("HTML standard face");
        let host = host_typeface().unwrap();
        assert_eq!(standard.family_name(), host.family_name());
        let actual = resolved_font_geometry(&style).expect("HTML standard metrics");
        let expected = typeface_font_geometry(&host, style.font_size);
        assert_eq!(actual.ascent, expected.ascent);
        assert_eq!(actual.descent, expected.descent);
        style.font_family = Some("serif".into());
        let explicit = generic_serif_typeface(&style).expect("explicit serif face");
        let expected_serif = GENERIC_SERIF_TYPEFACE.with(Clone::clone).unwrap();
        assert_eq!(explicit.family_name(), expected_serif.family_name());
        assert!(generic_serif_typeface(&Style::default()).is_none(),
            "native embeddings retain their supplied primary font");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn language_generic_faces_share_measurement_and_metrics() {
        use browser_font_defaults::GenericFamily;
        for language in ["en", "zh-CN", "zh-Hant", "ja", "ko", "zh-Latn"] {
            for (css, generic) in [(None, GenericFamily::Standard),
                (Some("serif"), GenericFamily::Serif), (Some("sans-serif"), GenericFamily::Sans),
                (Some("monospace"), GenericFamily::Monospace)] {
                let mut style = w3cos_dom::user_agent::html_default_style("html");
                style.font_family = css.map(str::to_string);
                style.custom_properties.get_or_insert_with(Default::default)
                    .insert(w3cos_dom::user_agent::TEXT_LANGUAGE_PROPERTY.to_string(), language.to_string());
                let wanted = browser_font_defaults::family(language, generic).unwrap();
                let expected = FontMgr::default().match_family_style(wanted, FontStyle::normal()).unwrap();
                let face = generic_serif_typeface(&style).expect("language generic face");
                assert_eq!(face.family_name(), expected.family_name(), "{language}: {css:?}");
                let actual = resolved_font_geometry(&style).unwrap();
                let metrics = typeface_font_geometry(&expected, style.font_size);
                assert_eq!(actual.ascent, metrics.ascent);
                assert_eq!(actual.descent, metrics.descent);
                assert_eq!(font_stack_advance("This text should be green.", &face, &style),
                    font_stack_advance("This text should be green.", &expected, &style));
                style.font_family = Some("Arial".into());
                assert!(generic_serif_typeface(&style).is_none(), "authored named font keeps its cascade");
            }
        }
    }

    #[test]
    fn registered_ahem_normal_line_includes_uncovered_font_metrics() {
        use crate::font_face::{FontFace, FontRegistry, FontSource};
        const OWNER: u64 = 202610080112;
        let bytes = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"),
            "/../../../wpt/fonts/Ahem.ttf")).expect("pinned Ahem font");
        let registry = FontRegistry::global();
        registry.register_for_owner(OWNER, FontFace { family: "Ahem".into(),
            src: FontSource::Bytes(bytes), ..Default::default() }).unwrap();
        let mut results = Vec::new();
        for family in ["Times New Roman", "Arial", "Courier New"] {
            let style = Style { font_family: Some(format!("Ahem, \"{family}\"")),
                font_size: 64.0, line_height_is_normal: true, ..Style::default() };
            let primary = registered_typeface(&style).unwrap().1;
            let mut expected = resolved_font_geometry(&style).unwrap().normal_line_box();
            for run in css_font_runs("Ţęşţ", &primary, &style) {
                let fallback = typeface_font_geometry(&run.typeface, 64.0).normal_line_box();
                expected.ascent = expected.ascent.max(fallback.ascent);
                expected.descent = expected.descent.max(fallback.descent);
            }
            let embedding = primary_typeface(TEST_FONT).unwrap();
            results.push((family, resolved_text_font_geometry("Ţęşţ", &style).unwrap(), expected,
                text_baseline(0.0, 64.0, &embedding, &style, "Ţęşţ"),
                typeface_font_geometry(&primary, 64.0).ascent));
        }
        registry.clear_owner(OWNER);
        for (family, actual, expected, painted_baseline, primary_ascent) in results {
            assert_eq!((actual.ascent, actual.descent, actual.line_gap),
                (expected.ascent, expected.descent, expected.line_gap),
                "normal Ahem stack must include uncovered {family} line metrics");
            assert_eq!(painted_baseline, primary_ascent,
                "uncovered {family} glyphs share the registered primary strut baseline, not embedding metrics");
        }
    }

    #[test]
    fn mirrored_rtl_font_fallback_matches_equivalent_ltr_visual_glyphs() {
        let mut style = Style { font_size: 16.0, ..Style::default() };
        style.custom_properties = Some(std::collections::HashMap::from([
            (w3cos_dom::user_agent::HTML_STANDARD_FONT_PROPERTY.to_string(), "1".to_string()),
        ]));
        let face = primary_typeface(TEST_FONT).unwrap();
        let mut captures = Vec::new();
        for (direction, text) in [
            (w3cos_std::style::TextDirection::Ltr, "(a b א) c d"),
            (w3cos_std::style::TextDirection::Rtl, "c d (א a b)"),
        ] {
            style.direction = direction;
            let mut surface = Surface::new_raster_n32_premul((128, 40)).unwrap();
            surface.canvas().clear(Color::WHITE);
            draw_text_glyph_line(surface.canvas(), 8.0, 8.0, text, 16.0,
                w3cos_std::Color::BLACK, 1.0, &face, &style);
            let info = ImageInfo::new((128, 40), ColorType::RGBA8888, AlphaType::Premul, None);
            let mut pixels = vec![0_u8; 128 * 40 * 4];
            assert!(surface.read_pixels(&info, &mut pixels, 128 * 4, (0, 0)));
            captures.push(pixels);
        }
        let different = captures[0].chunks_exact(4).zip(captures[1].chunks_exact(4))
            .filter(|(left, right)| left != right).count();
        assert_eq!(different, 0,
            "equivalent visual glyphs must retain the same fallback-font origin");
    }

    #[test]
    fn registered_ahem_inline_glyph_covers_its_integer_em_background() {
        use crate::font_face::{FontFace, FontRegistry, FontSource};
        const OWNER: u64 = 2026100802290;
        let bytes = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"),
            "/../../../wpt/fonts/Ahem.ttf")).expect("pinned Ahem font");
        let registry = FontRegistry::global();
        registry.register_for_owner(OWNER, FontFace { family: "Ahem".into(),
            src: FontSource::Bytes(bytes), ..Default::default() }).unwrap();
        let style = Style { display: Display::Inline, font_family: Some("Ahem".into()),
            font_size: 20.0, line_height: 1.0, line_height_is_normal: false,
            color: w3cos_std::Color::WHITE, ..Style::default() };
        let face = primary_typeface(TEST_FONT).unwrap();
        let mut captures = Vec::new();
        for pipeline in [false, true] {
            let mut surface = Surface::new_raster_n32_premul((40, 90)).unwrap();
            surface.canvas().clear(Color::WHITE);
            surface.canvas().draw_rect(Rect::from_xywh(8.0, 55.0, 20.0, 20.0),
                &color_paint(w3cos_std::Color::rgb(255, 0, 0), 1.0));
            let rect = LayoutRect { x: 8.0, y: 55.0, width: 20.0, height: 20.0 };
            if pipeline {
                render_node_with_line_context(surface.canvas(), 0, rect,
                    &ComponentKind::Text { content: "A".into() }, &style, &face,
                    crate::layout::layout_font(), None, false, true, None,
                    Some(LayoutRect { x: 8.0, y: 54.0, width: 784.0, height: 22.0 }),
                    true, true, &[], None);
            } else {
                draw_text_in_rect(surface.canvas(), rect, "A", &style, &face,
                    crate::layout::layout_font());
            }
            let info = ImageInfo::new((40, 90), ColorType::RGBA8888, AlphaType::Premul, None);
            let mut pixels = vec![0_u8; 40 * 90 * 4];
            assert!(surface.read_pixels(&info, &mut pixels, 40 * 4, (0, 0)));
            captures.push((pipeline, pixels));
        }
        registry.clear_owner(OWNER);
        for (pipeline, pixels) in captures {
            let red: Vec<_> = (55..75).flat_map(|y| (8..28).map(move |x| (x, y)))
                .filter(|(x, y)| pixels[(y * 40 + x) * 4..(y * 40 + x) * 4 + 4] != [255; 4])
                .collect();
            assert!(red.is_empty(), "explicit inline baseline must not be centered again: pipeline={pipeline}, red={red:?}");
        }
    }

    #[test]
    fn registered_ahem_normal_preserves_font_raster_and_fractional_origin() {
        use crate::font_face::{FontFace, FontRegistry, FontSource};
        const OWNER: u64 = 202610040048;
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../wpt/fonts/Ahem.ttf");
        let Ok(bytes) = std::fs::read(path) else {
            eprintln!("skipped: no pinned WPT checkout at {path}");
            return;
        };
        let registry = FontRegistry::global();
        registry.register_for_owner(OWNER, FontFace { family: "Ahem".into(),
            src: FontSource::Bytes(bytes), ..Default::default() }).unwrap();
        let mut captures = Vec::new();
        for origin in [23.0, 23.375] {
            let style = Style { font_family: Some("Ahem".into()), font_size: 15.0,
                font_weight: 400, ..Style::default() };
            let (_, face) = registered_typeface_covering(&style, "xx xx").unwrap();
            let mut images = Vec::new();
            for reference in [false, true] {
                let mut surface = Surface::new_raster_n32_premul((128, 40)).unwrap();
                surface.canvas().clear(Color::WHITE);
                let paint = color_paint(w3cos_std::Color::BLACK, 1.0);
                let advance = if reference {
                    draw_font_stack_runs(surface.canvas(), origin,
                        text_baseline(8.0, 15.0, &face, &style, "xx xx"),
                        "xx xx", 15.0, &face, &style, &paint)
                } else {
                    draw_ahem_cells(surface.canvas(), origin, 8.0, "xx xx", 15.0,
                        &style, &paint)
                };
                let info = ImageInfo::new((128, 40), ColorType::RGBA8888, AlphaType::Premul, None);
                let mut pixels = vec![0_u8; 128 * 40 * 4];
                assert!(surface.read_pixels(&info, &mut pixels, 128 * 4, (0, 0)));
                images.push((advance, pixels));
            }
            captures.push((origin, images));
        }
        registry.clear_owner(OWNER);
        for (origin, images) in captures {
            assert_eq!(images[0].0, images[1].0, "registered Ahem advance");
            assert_eq!(images[0].1.iter().zip(&images[1].1).filter(|(a, b)| a != b).count(), 0,
                "registered normal face must not substitute snapped rectangles, x={origin}");
        }
    }

    #[test]
    fn registered_ahem_bold_changes_ink_without_changing_advance() {
        use crate::font_face::{FontFace, FontRegistry, FontSource};
        const OWNER: u64 = 202610040029;
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../wpt/fonts/Ahem.ttf");
        let Ok(bytes) = std::fs::read(path) else {
            eprintln!("skipped: no pinned WPT checkout at {path}");
            return;
        };
        let registry = FontRegistry::global();
        registry.register_for_owner(OWNER, FontFace { family: "Ahem".into(),
            src: FontSource::Bytes(bytes), ..Default::default() }).unwrap();
        let face = FontMgr::default().new_from_data(TEST_FONT, None).unwrap();
        let mut images = Vec::new();
        let mut advances = Vec::new();
        for weight in [400, 900] {
            let style = Style { font_family: Some("Ahem".into()), font_size: 24.0,
                font_weight: weight, ..Style::default() };
            let mut surface = Surface::new_raster_n32_premul((96, 64)).unwrap();
            surface.canvas().clear(Color::WHITE);
            advances.push(draw_text_line(surface.canvas(), 8.375, 8.0, "XX", 24.0,
                w3cos_std::Color::BLACK, 1.0, &face, &style));
            let info = ImageInfo::new((96, 64), ColorType::RGBA8888, AlphaType::Premul, None);
            let mut pixels = vec![255_u8; 96 * 64 * 4];
            assert!(surface.read_pixels(&info, &mut pixels, 96 * 4, (0, 0)));
            images.push(pixels);
        }
        registry.clear_owner(OWNER);
        assert_eq!(advances[0], advances[1], "synthetic bold must not expand the text advance");
        assert_ne!(images[0], images[1], "registered normal face ignored requested synthetic bold");
    }

    #[test]
    fn ancestor_text_decoration_uses_owner_metrics_and_respects_isolation() {
        use w3cos_std::style::{Float, Position, TextDecoration};
        for boundary in ["flow", "inline-block", "float", "absolute"] {
            let owner = Style {
                display: Display::Block, font_size: 16.0,
                color: w3cos_std::Color::rgb(0, 128, 0),
                text_decoration: TextDecoration::Underline,
                ..Style::default()
            };
            let mut child = Style {
                display: Display::Inline, font_size: 24.0,
                color: w3cos_std::Color::rgb(0, 0, 255),
                ..Style::default()
            };
            match boundary {
                "inline-block" => child.display = Display::InlineBlock,
                "float" => child.float = Float::Left,
                "absolute" => child.position = Position::Absolute,
                _ => {}
            }
            let kinds = [ComponentKind::Box, ComponentKind::Text { content: "    ".into() }];
            let styles = [owner, child];
            let rects = [
                LayoutRect { x: 8.0, y: 8.0, width: 100.0, height: 40.0 },
                LayoutRect { x: 8.0, y: 8.0, width: 80.0, height: 28.8 },
            ];
            let layouts = [(rects[0], 0), (rects[1], 1)];
            let artifact = PaintArtifact::build((0..2).map(|i| crate::paint_artifact::PaintNode {
                kind: kinds[i].clone(), style: styles[i].clone(),
                parent: if i == 0 { None } else { Some(0) }, sticky_counter_signal: None,
            }), &layouts, 1);
            let nodes = [(0, rects[0], &kinds[0], &styles[0]), (1, rects[1], &kinds[1], &styles[1])];
            let mut rasterizer = SkiaRasterizer::new(TEST_FONT).unwrap();
            let pixels = rasterizer.render_frame(128, 64, &nodes, &test_font(), &[],
                &HashMap::new(), None, w3cos_std::Color::WHITE, Some(&artifact), None, 1.0).unwrap();
            let rows = (0..64).filter(|y| (0..128).any(|x| {
                let pixel = &pixels[(y * 128 + x) * 4..(y * 128 + x) * 4 + 3];
                pixel == [0, 128, 0]
            })).count();
            assert_eq!(rows, if boundary == "flow" { 1 } else { 0 },
                "owner underline thickness/color or isolation lost for {boundary}");
        }
    }

    #[test]
    fn text_decoration_paints_whitespace_without_changing_advance() {
        use w3cos_std::style::TextDecoration;

        let typeface = FontMgr::default().new_from_data(TEST_FONT, None).unwrap();
        let mut undecorated_advance = None;
        for decoration in [
            TextDecoration::None,
            TextDecoration::Underline,
            TextDecoration::Overline,
            TextDecoration::LineThrough,
        ] {
            let style = Style {
                font_size: 32.0,
                text_decoration: decoration,
                ..Style::default()
            };
            let mut surface = Surface::new_raster_n32_premul((128, 80)).unwrap();
            surface.canvas().clear(Color::WHITE);
            let advance = draw_text_line(
                surface.canvas(), 8.0, 16.0, "    ", 32.0,
                w3cos_std::Color::BLACK, 1.0, &typeface, &style,
            );
            let expected_advance = *undecorated_advance.get_or_insert(advance);
            assert_eq!(advance, expected_advance, "decoration changed text advance");
            let info = ImageInfo::new(
                (128, 80), ColorType::RGBA8888, AlphaType::Premul, None,
            );
            let mut pixels = vec![255_u8; 128 * 80 * 4];
            assert!(surface.read_pixels(&info, &mut pixels, 128 * 4, (0, 0)));
            let painted = pixels.chunks_exact(4)
                .filter(|pixel| pixel[..3] != [255, 255, 255]).count();
            if decoration == TextDecoration::None {
                assert_eq!(painted, 0, "spaces unexpectedly painted glyphs");
            } else {
                assert!(painted > 0, "{decoration:?} did not paint its line");
            }
        }
    }

    fn test_font() -> fontdue::Font {
        fontdue::Font::from_bytes(TEST_FONT, fontdue::FontSettings::default()).unwrap()
    }

    #[test]
    fn fixed_auto_multicol_skia_slices_borders_and_replays_all_columns() {
        use crate::paint_artifact::PaintNode;
        use w3cos_std::style::{ColumnFill, Dimension};
        let kind = ComponentKind::Column;
        let root_style = Style { display: Display::Block, width: Dimension::Px(300.0),
            height: Dimension::Px(100.0), column_width: Dimension::Px(100.0),
            column_gap: Some(0.0), column_fill: ColumnFill::Auto, ..Style::default() };
        let child_style = Style { display: Display::Block,
            background: w3cos_std::Color::rgb(0, 255, 255),
            border_bottom_width: Some(3.0), border_color: w3cos_std::Color::rgb(255, 128, 0),
            ..Style::default() };
        let root = LayoutRect { x: 0.0, y: 0.0, width: 300.0, height: 100.0 };
        let child = LayoutRect { x: 0.0, y: 0.0, width: 15.0, height: 250.0 };
        let artifact = PaintArtifact::build([
            PaintNode { kind: kind.clone(), style: root_style.clone(), parent: None, sticky_counter_signal: None },
            PaintNode { kind: kind.clone(), style: child_style.clone(), parent: Some(0), sticky_counter_signal: None },
        ], &[(root, 0), (child, 1)], 1);
        let nodes = [(0, root, &kind, &root_style), (1, child, &kind, &child_style)];
        let font = test_font();
        let values = HashMap::new();
        let mut rasterizer = SkiaRasterizer::new(TEST_FONT).unwrap();
        for _ in 0..2 {
            let rgba = rasterizer.render_frame(300, 300, &nodes, &font, &[], &values,
                None, w3cos_std::Color::WHITE, Some(&artifact), None, 1.0).unwrap();
            let pixel = |x: usize, y: usize| &rgba[(y * 300 + x) * 4..(y * 300 + x) * 4 + 4];
            for x in [5, 105, 205] { assert_eq!(pixel(x, 25), &[0, 255, 255, 255]); }
            for x in [5, 105] { assert_eq!(pixel(x, 99), &[0, 255, 255, 255], "border repeated at column break"); }
            assert_eq!(pixel(205, 49), &[255, 128, 0, 255], "bottom border must appear only at final slice");
            assert_eq!(pixel(205, 75), &[255, 255, 255, 255]);
            assert_eq!(pixel(5, 150), &[255, 255, 255, 255], "unfragmented paint leaked below column");
        }
    }

    #[test]
    fn first_line_background_uses_principal_font_box_across_large_glyph() {
        use crate::paint_artifact::PaintNode;
        for background_image in [None, Some("none")] {
        let text_style = |size| Style { display: Display::Inline,
            font_family: Some("Ahem".into()), font_size: size, line_height: 1.0,
            background_image: background_image.map(str::to_owned),
            line_height_is_normal: false, background: w3cos_std::Color::rgb(255, 0, 0),
            color: w3cos_std::Color::rgb(0, 128, 0),
            custom_properties: Some(HashMap::from([
                ("--w3cos-internal-inline-background-group".into(), "first-line".into()),
            ])), ..Style::default() };
        let kinds = [ComponentKind::Box, ComponentKind::Text { content: "X".into() },
            ComponentKind::Text { content: "p".into() }, ComponentKind::Text { content: "X".into() }];
        let styles = [Style { display: Display::Block, font_family: Some("Ahem".into()),
            font_size: 20.0, ..Style::default() }, text_style(20.0), text_style(100.0), text_style(20.0)];
        let rects = [LayoutRect { x: 8.0, y: 0.0, width: 140.0, height: 100.0 },
            LayoutRect { x: 8.0, y: 80.0, width: 20.0, height: 20.0 },
            LayoutRect { x: 28.0, y: 0.0, width: 100.0, height: 100.0 },
            LayoutRect { x: 128.0, y: 80.0, width: 20.0, height: 20.0 }];
        let layouts = rects.iter().copied().enumerate().map(|(i,r)| (r,i)).collect::<Vec<_>>();
        let artifact = PaintArtifact::build((0..4).map(|i| PaintNode {
            kind: kinds[i].clone(), style: styles[i].clone(),
            parent: (i != 0).then_some(0), sticky_counter_signal: None,
        }), &layouts, 1);
        let nodes = (0..4).map(|i| (i, rects[i], &kinds[i], &styles[i])).collect::<Vec<_>>();
        let mut rasterizer = SkiaRasterizer::new(TEST_FONT).unwrap();
        let pixels = rasterizer.render_frame(160, 110, &nodes, &test_font(), &[],
            &HashMap::new(), None, w3cos_std::Color::WHITE, Some(&artifact), None, 1.0).unwrap();
        let pixel = |x: usize,y: usize| &pixels[(y*160+x)*4..(y*160+x)*4+4];
        assert_eq!(pixel(30, 10), &[255,255,255,255], "large glyph must not enlarge pseudo background");
        assert_eq!(pixel(30, 90), &[0,128,0,255], "large glyph keeps its independent ink position");
        }
    }

    #[test]
    fn inline_background_does_not_expand_to_larger_descendant_font() {
        use crate::paint_artifact::PaintNode;
        let style = Style {
            display: Display::Inline,
            font_size: 20.0,
            line_height: 1.0,
            line_height_is_normal: false,
            background: w3cos_std::Color::rgb(255, 0, 0),
            ..Style::default()
        };
        let child_style = Style {
            font_size: 100.0,
            background: w3cos_std::Color::TRANSPARENT,
            ..style.clone()
        };
        let kind = ComponentKind::Row;
        let rect = LayoutRect { x: 0.0, y: 80.0, width: 140.0, height: 20.0 };
        let child_rect = LayoutRect { x: 20.0, y: 0.0, width: 100.0, height: 100.0 };
        let artifact = crate::paint_artifact::PaintArtifact::build(vec![
            PaintNode { kind: kind.clone(), style: style.clone(), parent: None, sticky_counter_signal: None },
            PaintNode { kind: ComponentKind::Text { content: "p".into() }, style: child_style, parent: Some(0), sticky_counter_signal: None },
        ], &[(rect, 0), (child_rect, 1)], 1);
        assert_eq!(inline_background_union_rect(Some(&artifact), 0, rect, &kind, &style), rect);
    }

    #[test]
    fn overlapping_absolute_text_keeps_ink_under_later_whitespace() {
        use crate::paint_artifact::PaintNode;
        let parent = ComponentKind::Box;
        let earlier = ComponentKind::Text { content: "X".into() };
        let parent_style = Style::default();
        let style = Style {
            position: w3cos_std::style::Position::Absolute,
            font_size: 20.0,
            line_height: 1.0,
            color: w3cos_std::Color::BLACK,
            // Preserve the gap so the later glyph does not overlap the first.
            white_space: w3cos_std::style::WhiteSpace::Pre,
            ..Style::default()
        };
        let rect = LayoutRect { x: 0.0, y: 0.0, width: 160.0, height: 40.0 };
        let font = test_font();
        let render = |later: Option<&ComponentKind>| {
            let mut paint_nodes = vec![
                PaintNode { kind: parent.clone(), style: parent_style.clone(), parent: None, sticky_counter_signal: None },
                PaintNode { kind: earlier.clone(), style: style.clone(), parent: Some(0), sticky_counter_signal: None },
            ];
            let mut layouts = vec![(rect, 0), (rect, 1)];
            let mut nodes = vec![(0, rect, &parent, &parent_style), (1, rect, &earlier, &style)];
            if let Some(later) = later {
                paint_nodes.push(PaintNode { kind: later.clone(), style: style.clone(), parent: Some(0), sticky_counter_signal: None });
                layouts.push((rect, 2));
                nodes.push((2, rect, later, &style));
            }
            let artifact = PaintArtifact::build(paint_nodes, &layouts, 1);
            SkiaRasterizer::new(TEST_FONT).unwrap().render_frame(
                160, 40, &nodes, &font, &[], &HashMap::new(), None,
                w3cos_std::Color::WHITE, Some(&artifact), None, 1.0,
            ).unwrap().to_vec()
        };
        let earlier_pixels = render(None);
        assert!(earlier_pixels.chunks_exact(4).any(|pixel| pixel[0] < 200));
        for content in ["          ", "          X"] {
            let later = ComponentKind::Text { content: content.into() };
            let combined = render(Some(&later));
            for y in 0..40 {
                for x in 0..20 {
                    let offset = (y * 160 + x) * 4;
                    assert_eq!(&combined[offset..offset + 4], &earlier_pixels[offset..offset + 4],
                        "earlier ink must survive transparent later whitespace at ({x},{y}), later={content:?}");
                }
            }
        }
    }

    #[test]
    fn css_filter_matrix_matches_web_invert_and_opacity() {
        let invert = css_color_matrix(&FilterOp::Invert(1.0)).unwrap();
        assert_eq!(invert[0], -1.0);
        assert_eq!(invert[4], 1.0);
        assert_eq!(invert[6], -1.0);
        assert_eq!(invert[9], 1.0);

        let opacity = css_color_matrix(&FilterOp::Opacity(0.25)).unwrap();
        assert_eq!(opacity[0], 1.0);
        assert_eq!(opacity[18], 0.25);
    }

    #[test]
    fn parses_layered_css_gradients_without_splitting_rgba() {
        let value = "radial-gradient(circle at 85% 8%, rgba(22, 119, 255, 0.18), transparent 34%), linear-gradient(160deg, #f7faff 0%, #eef3fb 100%)";
        let style = Style {
            background_image: Some(value.to_string()),
            ..Style::default()
        };
        let layers = crate::background_image::gradient_background_layers(
            &style,
            LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            },
        );
        assert_eq!(layers.len(), 2);
        assert_eq!(
            layers[0].stops[0].color,
            w3cos_std::color::Color::rgba(22, 119, 255, 46)
        );
        assert_eq!(
            layers[1]
                .stops
                .iter()
                .map(|stop| stop.position)
                .collect::<Vec<_>>(),
            vec![0.0, 1.0]
        );
    }

    #[test]
    fn centered_flex_host_centers_lowered_text_content() {
        let mut style = Style::default();
        style.justify_content = JustifyContent::Center;
        assert_eq!(effective_text_align(&style), TextAlign::Center);
    }

    #[test]
    fn inherited_text_align_does_not_realign_an_inline_fragment() {
        let mut style = Style::default();
        style.display = Display::Inline;
        style.text_align = TextAlign::Right;
        assert_eq!(effective_text_align(&style), TextAlign::Left);
    }

    #[test]
    fn float_exclusion_line_uses_the_containing_blocks_text_alignment() {
        for (align, direction, expected) in [
            (
                TextAlign::Right,
                w3cos_std::style::TextDirection::Ltr,
                TextAlign::Right,
            ),
            (
                TextAlign::Start,
                w3cos_std::style::TextDirection::Rtl,
                TextAlign::Right,
            ),
            (
                TextAlign::Center,
                w3cos_std::style::TextDirection::Ltr,
                TextAlign::Center,
            ),
        ] {
            let mut style = Style {
                display: Display::Inline,
                text_align: align,
                direction,
                ..Style::default()
            };
            assert_eq!(effective_text_align(&style), TextAlign::Left);
            style
                .custom_properties
                .get_or_insert_with(Default::default)
                .insert(
                    "--w3cos-internal-float-line-bands".into(),
                    "50 0 350".into(),
                );
            assert_eq!(effective_text_align(&style), expected);
        }
    }

    #[test]
    fn text_align_last_overrides_the_final_inline_line() {
        let mut style = Style::default();
        style
            .custom_properties
            .get_or_insert_with(Default::default)
            .insert(
                "--w3cos-internal-text-align-last".to_string(),
                "right".to_string(),
            );
        assert_eq!(effective_text_align_last(&style), Some(TextAlign::Right));
    }

    #[test]
    fn fragmented_inline_text_uses_parent_line_alignment_not_its_own_direction() {
        let mut line_style = Style {
            display: Display::Flex,
            font_size: 20.0,
            line_height: 1.0,
            direction: w3cos_std::style::TextDirection::Rtl,
            ..Style::default()
        };
        line_style
            .custom_properties
            .get_or_insert_with(Default::default)
            .insert(
                "--w3cos-internal-inline-formatting-context".into(),
                "1".into(),
            );
        let text_style = Style {
            display: Display::Inline,
            font_family: Some("Ahem".into()),
            font_size: 20.0,
            line_height: 1.0,
            direction: w3cos_std::style::TextDirection::Ltr,
            border_width: 2.0,
            border_color: w3cos_std::Color::BLACK,
            padding: w3cos_std::style::Edges {
                left: w3cos_std::style::Spacing::Px(5.0),
                right: w3cos_std::style::Spacing::Px(10.0),
                ..w3cos_std::style::Edges::ZERO
            },
            margin: w3cos_std::style::Edges {
                left: w3cos_std::style::Spacing::Px(30.0),
                right: w3cos_std::style::Spacing::Px(60.0),
                ..w3cos_std::style::Edges::ZERO
            },
            ..Style::default()
        };
        let line_kind = ComponentKind::Row;
        let text_kind = ComponentKind::Text {
            content: "p\u{2028}p".into(),
        };
        let layouts = [
            (
                LayoutRect {
                    x: 8.0,
                    y: 8.0,
                    width: 200.0,
                    height: 40.0,
                },
                0,
            ),
            (
                LayoutRect {
                    x: 109.0,
                    y: 6.0,
                    width: 39.0,
                    height: 24.0,
                },
                1,
            ),
        ];
        let artifact = PaintArtifact::build(
            [
                crate::paint_artifact::PaintNode {
                    kind: line_kind.clone(),
                    style: line_style.clone(),
                    parent: None,
                    sticky_counter_signal: None,
                },
                crate::paint_artifact::PaintNode {
                    kind: text_kind.clone(),
                    style: text_style.clone(),
                    parent: Some(0),
                    sticky_counter_signal: None,
                },
            ],
            &layouts,
            1,
        );
        let nodes = [
            (0, layouts[0].0, &line_kind, &line_style),
            (1, layouts[1].0, &text_kind, &text_style),
        ];
        let mut rasterizer = SkiaRasterizer::new(TEST_FONT).unwrap();
        let font = test_font();
        let inputs = HashMap::new();
        let pixels = rasterizer
            .render_frame(
                256,
                80,
                &nodes,
                &font,
                &[],
                &inputs,
                None,
                w3cos_std::Color::WHITE,
                Some(&artifact),
                None,
                1.0,
            )
            .unwrap();
        for (x, y, expected) in [
            (181, 15, 0),
            (109, 15, 255),
            (207, 15, 255),
            (147, 35, 0),
            (109, 35, 255),
        ] {
            assert_eq!(
                pixels[(y * 256 + x) * 4],
                expected,
                "parent-line-aligned border sample ({x},{y})"
            );
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn resolved_system_font_geometry_matches_browser_normal_box() {
        for line_height in [1.0, 1.2, 2.0] {
            let style = Style {
                font_family: Some("serif".into()), font_size: 16.0,
                line_height, ..Style::default()
            };
            let metrics = resolved_font_geometry(&style).expect("system Times face");
            assert_eq!(metrics.ascent, 14.0);
            assert_eq!(metrics.descent, 4.0);
            assert_eq!(metrics.height(), 18.0);
            assert_eq!(metrics.line_spacing(), 18.0);
        }
        assert!(resolved_font_geometry(&Style { font_size: 0.0, ..Style::default() }).is_none());
    }

    #[test]
    fn continuation_inline_text_preserves_its_projected_line_origin() {
        let line = LayoutRect { x: 0.0, y: 50.0, width: 200.0, height: 54.0 };
        let text = |y| LayoutRect { x: 0.0, y, width: 40.0, height: 18.0 };
        assert!(inline_text_is_in_first_strut(text(50.0), line, 18.0));
        assert!(inline_text_is_in_first_strut(text(51.0), line, 18.0));
        assert!(!inline_text_is_in_first_strut(text(68.0), line, 18.0));
        assert!(!inline_text_is_in_first_strut(text(86.0), line, 18.0));
        assert!(inline_text_is_in_first_strut(text(50.0), line, 0.0));
    }

    #[test]
    fn top_aligned_inline_glyph_does_not_absorb_ancestor_half_leading() {
        let typeface = FontMgr::default().new_from_data(TEST_FONT, None).unwrap();
        for (normal, top_aligned) in [(false, false), (false, true), (true, false), (true, true)] {
            let mut style = Style {
                display: Display::Inline, font_family: Some("Ahem".into()),
                font_size: 16.0, line_height: 1.0, color: w3cos_std::Color::BLACK,
                line_height_is_normal: normal,
                ..Style::default()
            };
            if top_aligned {
                style.custom_properties.get_or_insert_with(Default::default).insert(
                    "--w3cos-internal-vertical-align-keyword".into(), "top".into(),
                );
            }
            let mut surface = Surface::new_raster_n32_premul((64, 48)).unwrap();
            surface.canvas().clear(Color::WHITE);
            render_node_with_line_context(
                surface.canvas(), 0,
                LayoutRect { x: 4.0, y: 7.0, width: 16.0, height: 16.0 },
                &ComponentKind::Text { content: "X".into() }, &style, &typeface,
                crate::layout::layout_font(), None, false, true, None,
                Some(LayoutRect { x: 0.0, y: 0.0, width: 64.0, height: 40.0 }),
                true, true, &[], None,
            );
            let info = ImageInfo::new((64, 48), ColorType::RGBA8888, AlphaType::Premul, None);
            let mut pixels = vec![0_u8; 64 * 48 * 4];
            assert!(surface.read_pixels(&info, &mut pixels, 64 * 4, (0, 0)));
            // Explicit-height rectangles are projected font boxes, not boxes
            // awaiting ancestor strut centering. Keep the legacy normal strut
            // branch covered separately rather than asserting it for both.
            let top = if normal && !top_aligned { 19 } else { 7 };
            for (y, red) in [(top - 1, 255), (top, 0), (top + 15, 0), (top + 16, 255)] {
                assert_eq!(pixels[(y * 64 + 10) * 4], red,
                    "normal={normal}, top_aligned={top_aligned}, y={y}");
            }
        }
    }

    #[test]
    fn top_aligned_inline_decoration_keeps_its_own_box_for_explicit_line_height() {
        let typeface = FontMgr::default().new_from_data(TEST_FONT, None).unwrap();
        for normal in [false, true] {
            let mut style = Style {
                display: Display::Inline,
                font_size: 16.0,
                line_height: 1.0,
                line_height_is_normal: normal,
                background: w3cos_std::Color::rgb(0, 128, 0),
                ..Style::default()
            };
            style.custom_properties.get_or_insert_with(Default::default).insert(
                "--w3cos-internal-vertical-align-keyword".into(), "top".into(),
            );
            let mut surface = Surface::new_raster_n32_premul((64, 48)).unwrap();
            surface.canvas().clear(Color::WHITE);
            render_node_with_line_context(
                surface.canvas(), 0,
                LayoutRect { x: 4.0, y: 7.0, width: 40.0, height: 16.0 },
                &ComponentKind::Row, &style, &typeface, crate::layout::layout_font(),
                None, false, true, None,
                Some(LayoutRect { x: 0.0, y: 0.0, width: 64.0, height: 40.0 }),
                false, true, &[], None,
            );
            let info = ImageInfo::new((64, 48), ColorType::RGBA8888, AlphaType::Premul, None);
            let mut pixels = vec![0_u8; 64 * 48 * 4];
            assert!(surface.read_pixels(&info, &mut pixels, 64 * 4, (0, 0)));
            for (y, expected) in [
                (6, [255, 255, 255, 255]),
                (7, [0, 128, 0, 255]),
                (22, [0, 128, 0, 255]),
                (23, [255, 255, 255, 255]),
                (39, [255, 255, 255, 255]),
            ] {
                assert_eq!(&pixels[(y * 64 + 10) * 4..(y * 64 + 10) * 4 + 4], &expected,
                    "top-aligned sample y={y}, normal={normal}");
            }
        }
    }

    #[test]
    fn normal_inline_side_border_does_not_add_horizontal_caps_or_absorb_strut() {
        let typeface = FontMgr::default().new_from_data(TEST_FONT, None).unwrap();
        let style = Style {
            display: Display::Inline,
            font_size: 16.0,
            line_height: 1.2,
            line_height_is_normal: true,
            border_left_width: Some(5.0),
            border_left_color: Some(w3cos_std::Color::rgb(0, 0, 255)),
            ..Style::default()
        };
        for has_content in [false, true] {
            let mut surface = Surface::new_raster_n32_premul((64, 48)).unwrap();
            surface.canvas().clear(Color::WHITE);
            render_node_with_line_context(
                surface.canvas(), 0,
                LayoutRect { x: 4.0, y: 12.0, width: 40.0, height: 16.0 },
                &ComponentKind::Row, &style, &typeface, crate::layout::layout_font(),
                None, false, true, None,
                Some(LayoutRect { x: 0.0, y: 0.0, width: 64.0, height: 40.0 }),
                false, has_content, &[], None,
            );
            let info = ImageInfo::new((64, 48), ColorType::RGBA8888, AlphaType::Premul, None);
            let mut pixels = vec![0_u8; 64 * 48 * 4];
            assert!(surface.read_pixels(&info, &mut pixels, 64 * 4, (0, 0)));
            for (x, y, expected) in [
                (5, 11, [255, 255, 255, 255]),
                (5, 12, [0, 0, 255, 255]),
                (5, 27, [0, 0, 255, 255]),
                (5, 28, [255, 255, 255, 255]),
                (20, 12, [255, 255, 255, 255]),
                (20, 27, [255, 255, 255, 255]),
            ] {
                assert_eq!(&pixels[(y * 64 + x) * 4..(y * 64 + x) * 4 + 4], &expected,
                    "side-border sample ({x},{y}), content={has_content}");
            }
        }
    }

    #[test]
    fn forced_inline_break_slices_border_edges_instead_of_enclosing_both_lines() {
        let typeface = FontMgr::default().new_from_data(TEST_FONT, None).unwrap();
        let style = Style {
            display: Display::Inline,
            font_family: Some("Ahem".into()),
            font_size: 20.0,
            line_height: 1.0,
            line_height_is_normal: false,
            border_width: 2.0,
            border_color: w3cos_std::Color::BLACK,
            padding: w3cos_std::style::Edges {
                left: w3cos_std::style::Spacing::Px(5.0),
                right: w3cos_std::style::Spacing::Px(10.0),
                ..w3cos_std::style::Edges::ZERO
            },
            margin: w3cos_std::style::Edges {
                left: w3cos_std::style::Spacing::Px(30.0),
                ..w3cos_std::style::Edges::ZERO
            },
            ..Style::default()
        };
        let mut surface = Surface::new_raster_n32_premul((128, 80)).unwrap();
        surface.canvas().clear(Color::WHITE);
        draw_text_in_rect(
            surface.canvas(),
            LayoutRect {
                x: 38.0,
                y: 6.0,
                width: 39.0,
                height: 44.0,
            },
            "p\u{2028}p",
            &style,
            &typeface,
            crate::layout::layout_font(),
        );
        let info = ImageInfo::new((128, 80), ColorType::RGBA8888, AlphaType::Premul, None);
        let mut pixels = vec![0_u8; 128 * 80 * 4];
        assert!(surface.read_pixels(&info, &mut pixels, 128 * 4, (0, 0)));
        for (x, y, expected) in [
            (38, 15, 0),
            (64, 15, 255),
            (75, 15, 255),
            (8, 35, 255),
            (39, 35, 0),
        ] {
            assert_eq!(
                pixels[(y * 128 + x) * 4],
                expected,
                "border sample ({x},{y})"
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn generic_serif_matches_the_macos_browser_default_face() {
        let style = Style {
            font_family: Some("serif".into()),
            ..Style::default()
        };
        let expected = FontMgr::default()
            .match_family_style("Times", FontStyle::normal())
            .expect("macOS system Times face");
        let actual = generic_serif_typeface(&style).expect("generic serif face");
        assert_eq!(actual.family_name(), expected.family_name());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn html_unresolved_font_stack_uses_standard_context_not_explicit_serif() {
        let mut standard = w3cos_dom::user_agent::html_default_style("html");
        standard.custom_properties.get_or_insert_with(Default::default)
            .insert(w3cos_dom::user_agent::TEXT_LANGUAGE_PROPERTY.into(), "zh-CN".into());
        let expected = generic_serif_typeface(&standard).expect("HTML standard face");
        // DEFAULT Chromium141 original unresolved-family pages and references,
        // screenshot-bound V2088: PingFang SC with a22px normal16px line box.
        assert_eq!(expected.family_name(), "PingFang SC");
        for stack in ["Definitely Missing Font Family", "Missing Font One, Missing Font Two"] {
            let style = Style { font_family: Some(stack.into()), ..standard.clone() };
            let actual = generic_serif_typeface(&style).expect("unresolved stack fallback");
            assert_eq!(actual.family_name(), expected.family_name(), "{stack}");
            let geometry = resolved_font_geometry(&style).unwrap().normal_line_box();
            let expected_geometry = resolved_font_geometry(&standard).unwrap().normal_line_box();
            assert_eq!(geometry.ascent, expected_geometry.ascent);
            assert_eq!(geometry.descent, expected_geometry.descent);
            assert_eq!(measure_skia_text_intrinsic_size("X", &style),
                measure_skia_text_intrinsic_size("X", &standard));
        }
        let explicit_serif = Style { font_family: Some("Missing Font One, serif".into()), ..standard };
        assert_eq!(generic_serif_typeface(&explicit_serif).unwrap().family_name(), "Times",
            "an explicit generic serif is not the HTML standard default");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn unknown_font_family_falls_back_to_the_browser_default_serif() {
        let style = Style {
            font_family: Some("Definitely Missing Font Family".into()),
            ..Style::default()
        };
        let expected = generic_serif_typeface(&Style {
            font_family: Some("serif".into()),
            ..Style::default()
        })
        .expect("generic serif face");
        let actual = generic_serif_typeface(&style).expect("unknown family fallback");
        assert_eq!(actual.family_name(), expected.family_name());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn letter_spacing_keeps_the_last_character_advance_across_inline_boundaries() {
        let typeface = FontMgr::default()
            .match_family_style("Times", FontStyle::normal())
            .unwrap();
        for family in ["serif", "Ahem", "monospace"] {
            let style = Style {
                font_family: Some(family.into()),
                font_size: 32.0,
                letter_spacing: 32.0,
                ..Style::default()
            };
            let font = Font::new(&typeface, style.font_size);
            for text in ["a", "ab", "abc"] {
                let expected = text
                    .chars()
                    .map(|character| {
                        let advance = match family {
                            "Ahem" => style.font_size,
                            "monospace" => font.measure_str("0", None).0,
                            _ => font.measure_str(character.to_string(), None).0,
                        };
                        advance + style.letter_spacing
                    })
                    .sum::<f32>();
                let actual = measure_skia_text_advance(text, &typeface, &style);
                assert!(
                    (actual - expected).abs() < 0.01,
                    "{family}/{text:?}: the inline advance must retain trailing character spacing: {actual} != {expected}"
                );
            }
        }
    }

    #[test]
    fn multiline_inline_background_uses_line_advance_not_paragraph_height() {
        let typeface = FontMgr::default().new_from_data(TEST_FONT, None).unwrap();
        let style = Style {
            display: Display::Inline,
            font_family: Some("serif".into()),
            font_size: 16.0,
            line_height: 1.125,
            background: w3cos_std::Color::rgb(255, 255, 0),
            color: w3cos_std::Color::rgba(0, 0, 0, 0),
            ..Style::default()
        };
        for paragraph_height in [36.0, 72.0] {
            let mut surface = Surface::new_raster_n32_premul((80, 100)).unwrap();
            surface.canvas().clear(Color::WHITE);
            let rect = LayoutRect { x: 8.0, y: 8.0, width: 40.0, height: paragraph_height };
            let context = crate::paint_artifact::InlineLineContext {
                line_box: rect, first_line_box: rect,
                direction: style.direction, text_align: style.text_align,
                line_advance: None,
                tab_stops: tab_stops_for_style(&style),
            };
            render_node_with_line_context(surface.canvas(), 0, rect,
                &ComponentKind::Text { content: "a\u{2028}a".into() },
                &style, &typeface, crate::layout::layout_font(), None, false, false,
                Some(context), None, false, true, &[], None);
            let info = ImageInfo::new((80, 100), ColorType::RGBA8888, AlphaType::Premul, None);
            let mut pixels = vec![0_u8; 80 * 100 * 4];
            assert!(surface.read_pixels(&info, &mut pixels, 80 * 4, (0, 0)));
            let pixel = |x: usize, y: usize| &pixels[(y * 80 + x) * 4..(y * 80 + x) * 4 + 4];
            assert_eq!(pixel(10, 26), &[255, 255, 0, 255],
                "second fragment follows the18px glyph advance, paragraph={paragraph_height}");
            assert_eq!(pixel(10, 44), &[255, 255, 255, 255],
                "no phantom decoration at the paragraph-height advance");
        }
    }

    #[test]
    fn inline_background_uses_first_and_continuation_fragments() {
        let typeface = FontMgr::default().new_from_data(TEST_FONT, None).unwrap();
        let style = Style {
            display: Display::Inline,
            font_family: Some("serif".into()),
            font_size: 16.0,
            line_height: 1.25,
            text_indent: w3cos_std::style::Dimension::Px(16.0),
            background: w3cos_std::color::Color::rgb(255, 255, 0),
            color: w3cos_std::color::Color::rgba(0, 0, 0, 0),
            ..Style::default()
        };
        let mut surface = Surface::new_raster_n32_premul((80, 120)).unwrap();
        surface.canvas().clear(Color::WHITE);
        let kind = ComponentKind::Text {
            content: "a a a a a a a a a a a a".into(),
        };
        render_node(
            surface.canvas(),
            0,
            LayoutRect {
                x: 8.0,
                y: 8.0,
                width: 40.0,
                height: 16.0,
            },
            &kind,
            &style,
            &typeface,
            crate::layout::layout_font(),
            None,
            false,
            false,
        );
        let info = ImageInfo::new((80, 120), ColorType::RGBA8888, AlphaType::Premul, None);
        let mut pixels = vec![0_u8; 80 * 120 * 4];
        assert!(surface.read_pixels(&info, &mut pixels, 80 * 4, (0, 0)));
        let pixel = |x: usize, y: usize| &pixels[(y * 80 + x) * 4..(y * 80 + x) * 4 + 4];
        assert_eq!(
            pixel(8, 8),
            &[255, 255, 255, 255],
            "indent is not background"
        );
        assert_eq!(
            pixel(25, 6),
            &[255, 255, 255, 255],
            "inline decoration excludes half-leading"
        );
        assert_eq!(pixel(25, 8), &[255, 255, 0, 255], "first fragment");
        assert_eq!(pixel(8, 28), &[255, 255, 0, 255], "continuation fragment");
    }

    #[test]
    fn wrapped_inline_first_word_uses_space_before_trailing_edges() {
        let typeface = FontMgr::default().new_from_data(TEST_FONT, None).unwrap();
        let style = Style {
            display: Display::Inline,
            font_family: Some("Ahem".into()),
            font_size: 20.0,
            line_height: 1.0,
            border_width: 10.0,
            border_color: w3cos_std::Color::rgb(0, 0, 255),
            padding: w3cos_std::style::Edges {
                top: w3cos_std::style::Spacing::Px(20.0),
                right: w3cos_std::style::Spacing::Px(20.0),
                bottom: w3cos_std::style::Spacing::Px(20.0),
                left: w3cos_std::style::Spacing::Px(20.0),
            },
            color: w3cos_std::Color::rgb(0, 0, 255),
            ..Style::default()
        };
        let mut surface = Surface::new_raster_n32_premul((160, 130)).unwrap();
        surface.canvas().clear(Color::WHITE);
        draw_text_in_rect(
            surface.canvas(),
            LayoutRect {
                x: 8.0,
                y: 45.0,
                width: 130.0,
                height: 80.0,
            },
            "XXXXX XXXXX",
            &style,
            &typeface,
            crate::layout::layout_font(),
        );
        let info = ImageInfo::new((160, 130), ColorType::RGBA8888, AlphaType::Premul, None);
        let mut pixels = vec![0_u8; 160 * 130 * 4];
        assert!(surface.read_pixels(&info, &mut pixels, 160 * 4, (0, 0)));
        let pixel = |x: usize, y: usize| &pixels[(y * 160 + x) * 4..(y * 160 + x) * 4 + 4];
        assert_eq!(
            pixel(100, 46),
            &[0, 0, 255, 255],
            "the first word must not be deferred behind an empty inline fragment"
        );
    }

    #[test]
    fn short_line_height_inline_background_keeps_the_font_em_box() {
        let typeface = FontMgr::default().new_from_data(TEST_FONT, None).unwrap();
        let style = Style {
            display: Display::Inline,
            font_family: Some("serif".into()),
            font_size: 50.0,
            line_height: 0.2,
            padding: w3cos_std::style::Edges {
                top: w3cos_std::style::Spacing::Px(10.0),
                ..w3cos_std::style::Edges::ZERO
            },
            background: w3cos_std::color::Color::rgb(0, 128, 0),
            color: w3cos_std::color::Color::rgba(0, 0, 0, 0),
            ..Style::default()
        };
        let mut surface = Surface::new_raster_n32_premul((160, 120)).unwrap();
        surface.canvas().clear(Color::WHITE);
        render_node(
            surface.canvas(),
            0,
            LayoutRect {
                x: 8.0,
                y: 41.2,
                width: 115.0,
                height: 60.0,
            },
            &ComponentKind::Text {
                content: "PASS".into(),
            },
            &style,
            &typeface,
            crate::layout::layout_font(),
            None,
            false,
            false,
        );
        let info = ImageInfo::new((160, 120), ColorType::RGBA8888, AlphaType::Premul, None);
        let mut pixels = vec![0_u8; 160 * 120 * 4];
        assert!(surface.read_pixels(&info, &mut pixels, 160 * 4, (0, 0)));
        let pixel = |x: usize, y: usize| &pixels[(y * 160 + x) * 4..(y * 160 + x) * 4 + 4];
        assert_eq!(pixel(10, 50), &[0, 128, 0, 255]);
        assert_eq!(pixel(10, 90), &[0, 128, 0, 255]);
    }

    #[test]
    fn long_line_height_inline_background_excludes_half_leading() {
        let typeface = FontMgr::default().new_from_data(TEST_FONT, None).unwrap();
        let style = Style {
            display: Display::Inline,
            font_family: Some("Ahem".into()),
            font_size: 25.0,
            line_height: 51.0 / 25.0,
            background: w3cos_std::Color::rgb(255, 0, 0),
            color: w3cos_std::Color::rgba(0, 0, 0, 0),
            ..Style::default()
        };
        let mut surface = Surface::new_raster_n32_premul((100, 90)).unwrap();
        surface.canvas().clear(Color::from_argb(255, 0, 128, 0));
        render_node(
            surface.canvas(),
            0,
            LayoutRect {
                x: 8.0,
                y: 21.0,
                width: 75.0,
                height: 25.0,
            },
            &ComponentKind::Text {
                content: "xxx".into(),
            },
            &style,
            &typeface,
            crate::layout::layout_font(),
            None,
            false,
            false,
        );
        let info = ImageInfo::new((100, 90), ColorType::RGBA8888, AlphaType::Premul, None);
        let mut pixels = vec![0_u8; 100 * 90 * 4];
        assert!(surface.read_pixels(&info, &mut pixels, 100 * 4, (0, 0)));
        let pixel = |x: usize, y: usize| &pixels[(y * 100 + x) * 4..(y * 100 + x) * 4 + 4];
        assert_eq!(pixel(20, 13), &[0, 128, 0, 255], "top half-leading");
        assert_eq!(pixel(20, 33), &[255, 0, 0, 255], "font em box");
        assert_eq!(pixel(20, 53), &[0, 128, 0, 255], "bottom half-leading");
    }

    #[test]
    fn split_inline_background_keeps_coincident_first_text_fragment() {
        assert_split_inline_background(255);
    }

    #[test]
    fn split_inline_translucent_background_paints_each_fragment_once() {
        assert_split_inline_background(128);
    }

    fn assert_split_inline_background(alpha: u8) {
        use crate::paint_artifact::{PaintArtifact, PaintNode};
        use w3cos_std::style::Position;
        let blue = w3cos_std::Color::rgba(0, 0, 255, alpha);
        let orange = w3cos_std::Color::rgb(255, 165, 0);
        let inline = Style { display: Display::Inline, position: Position::Relative,
            font_family: Some("serif".into()), font_size: 16.0,
            line_height: 1.125, line_height_is_normal: true,
            background: blue, ..Style::default() };
        let text = Style { position: Position::Static, background: w3cos_std::Color::TRANSPARENT,
            ..inline.clone() };
        let block = Style { display: Display::Block, background: orange, ..text.clone() };
        let kinds = [ComponentKind::Row,
            ComponentKind::Text { content: "Filler Text".into() },
            ComponentKind::Row,
            ComponentKind::Text { content: "Filler Text".into() },
            ComponentKind::Text { content: "Filler Text".into() }];
        let styles = [inline, text.clone(), block, text.clone(), text];
        let parents = [None, Some(0), Some(0), Some(2), Some(0)];
        let rects = [0.0, 0.0, 18.0, 18.0, 36.0].map(|y|
            LayoutRect { x: 0.0, y, width: 192.0, height: 18.0 });
        let layouts = rects.iter().enumerate().map(|(i,r)| (*r,i)).collect::<Vec<_>>();
        let artifact = PaintArtifact::build((0..5).map(|i| PaintNode {
            kind: kinds[i].clone(), style: styles[i].clone(), parent: parents[i],
            sticky_counter_signal: None,
        }), &layouts, 1);
        let nodes = (0..5).map(|i| (i,rects[i],&kinds[i],&styles[i])).collect::<Vec<_>>();
        let mut rasterizer = SkiaRasterizer::new(TEST_FONT).unwrap();
        let pixels = rasterizer.render_frame(200,60,&nodes,&test_font(),&[],
            &HashMap::new(),None,w3cos_std::Color::WHITE,Some(&artifact),None,1.0).unwrap();
        let pixel = |x: usize,y: usize| &pixels[(y*200+x)*4..(y*200+x)*4+4];
        assert_eq!(pixel(100,8), &[255,255,255,255], "first inline fragment excludes unused line width");
        assert_eq!(pixel(100,26), &[255,165,0,255], "intervening block keeps its full background");
        assert_eq!(pixel(100,44), &[255,255,255,255], "continuation fragment remains short");
        assert_eq!(pixel(5,0), &[255-alpha,255-alpha,255,255], "first fragment paints exactly once");
        assert_eq!(pixel(5,36), &[255-alpha,255-alpha,255,255], "continuation paints exactly once");
    }

    #[test]
    fn borderless_inline_container_background_covers_the_line_band() {
        let typeface = FontMgr::default().new_from_data(TEST_FONT, None).unwrap();
        let style = Style {
            display: Display::Inline,
            font_size: 16.0,
            line_height: 1.2,
            background: w3cos_std::color::Color::rgb(0, 0, 255),
            ..Style::default()
        };
        let mut surface = Surface::new_raster_n32_premul((100, 60)).unwrap();
        surface.canvas().clear(Color::WHITE);
        render_node(
            surface.canvas(),
            0,
            LayoutRect {
                x: 8.0,
                y: 21.6,
                width: 80.0,
                height: 16.0,
            },
            &ComponentKind::Row,
            &style,
            &typeface,
            crate::layout::layout_font(),
            None,
            false,
            false,
        );
        let info = ImageInfo::new((100, 60), ColorType::RGBA8888, AlphaType::Premul, None);
        let mut pixels = vec![0_u8; 100 * 60 * 4];
        assert!(surface.read_pixels(&info, &mut pixels, 100 * 4, (0, 0)));
        let pixel = |x: usize, y: usize| &pixels[(y * 100 + x) * 4..(y * 100 + x) * 4 + 4];
        assert_eq!(pixel(10, 20), &[0, 0, 255, 255]);
        assert_eq!(pixel(10, 38), &[0, 0, 255, 255]);
    }

    #[test]
    fn short_inline_line_height_keeps_the_font_baseline_independent_of_ink() {
        let typeface = FontMgr::default().new_from_data(TEST_FONT, None).unwrap();
        let style = Style {
            display: Display::Inline,
            font_family: Some("serif".to_string()),
            font_size: 24.0,
            line_height: 0.4,
            white_space: w3cos_std::style::WhiteSpace::NoWrap,
            color: w3cos_std::color::Color::BLACK,
            ..Style::default()
        };
        let rect = LayoutRect {
            x: 10.0,
            y: 40.0,
            width: 180.0,
            height: 9.6,
        };
        for text in ["abcde", "fghij", "abcdefghijklmno"] {
            let mut actual = Surface::new_raster_n32_premul((200, 100)).unwrap();
            let mut expected = Surface::new_raster_n32_premul((200, 100)).unwrap();
            actual.canvas().clear(Color::WHITE);
            expected.canvas().clear(Color::WHITE);
            draw_text_in_rect(
                actual.canvas(),
                rect,
                text,
                &style,
                &typeface,
                crate::layout::layout_font(),
            );
            let ink = measure_skia_text_ink_bounds(
                text,
                24.0,
                &typeface,
                style.font_weight,
                Some(&style),
            );
            let x = aligned_text_x(
                rect,
                TextAlign::Left,
                alignment_ink_left(text, ink.left, 24.0, &typeface, &style),
                measure_skia_text_advance(text, &typeface, &style),
            );
            draw_text_line(
                expected.canvas(),
                x,
                rect.y,
                text,
                24.0,
                style.color,
                style.opacity,
                &typeface,
                &style,
            );
            let info = ImageInfo::new((200, 100), ColorType::RGBA8888, AlphaType::Premul, None);
            let mut actual_pixels = vec![0; 200 * 100 * 4];
            let mut expected_pixels = actual_pixels.clone();
            assert!(actual.read_pixels(&info, &mut actual_pixels, 800, (0, 0)));
            assert!(expected.read_pixels(&info, &mut expected_pixels, 800, (0, 0)));
            assert_eq!(actual_pixels, expected_pixels, "{text}");
        }
    }

    #[test]
    fn logical_start_alignment_follows_rtl_direction() {
        let mut style = Style::default();
        style.direction = w3cos_std::style::TextDirection::Rtl;
        assert_eq!(effective_text_align(&style), TextAlign::Right);
    }

    #[test]
    fn rtl_inline_text_overflow_paints_toward_the_left() {
        let typeface = FontMgr::default().new_from_data(TEST_FONT, None).unwrap();
        let style = Style {
            display: Display::Inline,
            direction: w3cos_std::style::TextDirection::Rtl,
            font_family: Some("Ahem".into()),
            font_size: 20.0,
            line_height: 1.0,
            color: w3cos_std::Color::BLACK,
            ..Style::default()
        };
        let mut surface = Surface::new_raster_n32_premul((160, 80)).unwrap();
        surface.canvas().clear(Color::WHITE);
        draw_text_in_rect(
            surface.canvas(),
            LayoutRect {
                x: 80.0,
                y: 10.0,
                width: 40.0,
                height: 40.0,
            },
            "1234",
            &style,
            &typeface,
            crate::layout::layout_font(),
        );
        let info = ImageInfo::new((160, 80), ColorType::RGBA8888, AlphaType::Premul, None);
        let mut pixels = vec![0_u8; 160 * 80 * 4];
        assert!(surface.read_pixels(&info, &mut pixels, 160 * 4, (0, 0)));
        let painted = |x: usize| (0..80).any(|y| pixels[(y * 160 + x) * 4] < 128);
        let painted_xs: Vec<_> = (0..160).filter(|&x| painted(x)).collect();
        assert!(
            painted(50),
            "RTL overflow must extend left of the inline box: {painted_xs:?}"
        );
        assert!(painted(110), "the line remains anchored at the right edge");
        assert!(!painted(130), "RTL overflow must not extend to the right");
    }

    #[test]
    fn text_with_background_still_paints_inside_css_padding() {
        let style = Style {
            background: w3cos_std::color::Color::WHITE,
            border_width: 1.0,
            padding: w3cos_std::style::Edges::xy(14.0, 11.0),
            ..Style::default()
        };
        let content = text_paint_box(
            LayoutRect {
                x: 20.0,
                y: 30.0,
                width: 200.0,
                height: 80.0,
            },
            &style,
        );

        assert_eq!(content.x, 35.0);
        assert_eq!(content.y, 42.0);
        assert_eq!(content.width, 170.0);
        assert_eq!(content.height, 56.0);
    }

    #[test]
    fn continued_inline_lines_restore_the_inline_start_edge() {
        let style = Style {
            display: Display::Inline,
            border_left_width: Some(20.0),
            padding: w3cos_std::style::Edges {
                left: w3cos_std::style::Spacing::Px(20.0),
                ..w3cos_std::style::Edges::ZERO
            },
            ..Style::default()
        };
        let rect = LayoutRect {
            x: 8.0,
            y: 60.0,
            width: 200.0,
            height: 200.0,
        };

        assert_eq!(text_paint_box(rect, &style).x, 48.0);
        assert_eq!(text_paint_box(rect, &style).width, 160.0);
        assert_eq!(text_continuation_paint_box(rect, &style).x, 8.0);
        assert_eq!(text_continuation_paint_box(rect, &style).width, 200.0);
    }

    #[test]
    fn text_content_box_uses_each_resolved_border_edge() {
        let style = Style {
            border_top_width: Some(30.0),
            border_right_width: Some(4.0),
            border_bottom_width: Some(6.0),
            border_left_width: Some(8.0),
            padding: w3cos_std::style::Edges::all(10.0),
            ..Style::default()
        };
        let content = text_content_box(
            LayoutRect {
                x: 0.0,
                y: 20.0,
                width: 200.0,
                height: 100.0,
            },
            &style,
        );

        assert_eq!(content.x, 18.0);
        assert_eq!(content.y, 60.0);
        assert_eq!(content.width, 168.0);
        assert_eq!(content.height, 44.0);
    }

    #[test]
    fn groove_and_ridge_default_borders_paint_opposite_half_bands() {
        use w3cos_std::style::BorderLineStyle;
        let typeface = FontMgr::default().new_from_data(TEST_FONT, None).unwrap();
        for (line_style, outer, inner) in [
            (BorderLineStyle::Groove, 154, 238),
            (BorderLineStyle::Ridge, 238, 154),
        ] {
            let style = Style {
                border_width: 10.0,
                border_color: w3cos_std::color::Color::BLACK,
                border_top_width: Some(10.0),
                border_right_width: Some(10.0),
                border_bottom_width: Some(10.0),
                border_left_width: Some(10.0),
                border_styles: [Some(line_style); 4],
                border_current_color: Some([true; 4]),
                ..Style::default()
            };
            let mut surface = Surface::new_raster_n32_premul((120, 120)).unwrap();
            surface.canvas().clear(Color::WHITE);
            render_node(
                surface.canvas(),
                0,
                LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: 120.0,
                    height: 120.0,
                },
                &ComponentKind::Column,
                &style,
                &typeface,
                crate::layout::layout_font(),
                None,
                false,
                false,
            );
            let info = ImageInfo::new((120, 120), ColorType::RGBA8888, AlphaType::Premul, None);
            let mut pixels = vec![0_u8; 120 * 120 * 4];
            assert!(surface.read_pixels(&info, &mut pixels, 120 * 4, (0, 0)));
            let pixel = |x: usize, y: usize| &pixels[(y * 120 + x) * 4..(y * 120 + x) * 4 + 4];
            assert_eq!(
                pixel(60, 2),
                &[outer, outer, outer, 255],
                "{line_style:?} outer top band"
            );
            assert_eq!(
                pixel(60, 7),
                &[inner, inner, inner, 255],
                "{line_style:?} inner top band"
            );
            assert_eq!(
                pixel(60, 112),
                &[outer, outer, outer, 255],
                "{line_style:?} inner bottom band"
            );
            assert_eq!(
                pixel(60, 117),
                &[inner, inner, inner, 255],
                "{line_style:?} outer bottom band"
            );
            assert_eq!(
                pixel(117, 2),
                &[196, 196, 196, 255],
                "{line_style:?} outer miter coverage"
            );
            assert_eq!(
                pixel(112, 7),
                &[196, 196, 196, 255],
                "{line_style:?} inner miter coverage"
            );
        }
    }

    #[test]
    fn zero_radius_css_box_fill_uses_crisp_edge_coverage() {
        let mut surface = Surface::new_raster_n32_premul((8, 8)).unwrap();
        surface.canvas().clear(Color::WHITE);
        draw_rounded_rect(
            surface.canvas(),
            LayoutRect {
                x: 0.5,
                y: 0.5,
                width: 4.0,
                height: 4.0,
            },
            [0.0; 4],
            &color_paint(w3cos_std::color::Color::rgb(0, 128, 0), 1.0),
        );

        let info = ImageInfo::new((8, 8), ColorType::RGBA8888, AlphaType::Premul, None);
        let mut pixels = vec![0_u8; 8 * 8 * 4];
        assert!(surface.read_pixels(&info, &mut pixels, 8 * 4, (0, 0)));
        assert_eq!(&pixels[..4], &[255, 255, 255, 255]);
        assert_eq!(
            &pixels[(1 * 8 + 1) * 4..(1 * 8 + 1) * 4 + 4],
            &[0, 128, 0, 255]
        );
        assert!(
            pixels
                .chunks_exact(4)
                .all(|pixel| { pixel == [255, 255, 255, 255] || pixel == [0, 128, 0, 255] })
        );
    }

    #[test]
    fn replaced_image_uses_crisp_edge_coverage_at_fractional_layout_coordinates() {
        crate::image_loader::clear_cache();
        let image = image::RgbaImage::from_pixel(2, 2, image::Rgba([0, 128, 0, 255]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        crate::image_loader::decode_and_install("fractional-image.png", &bytes.into_inner())
            .unwrap();

        let mut surface = Surface::new_raster_n32_premul((8, 8)).unwrap();
        surface.canvas().clear(Color::WHITE);
        draw_image(
            surface.canvas(),
            LayoutRect {
                x: 0.5,
                y: 0.5,
                width: 4.0,
                height: 4.0,
            },
            "fractional-image.png",
            1.0,
        );

        let info = ImageInfo::new((8, 8), ColorType::RGBA8888, AlphaType::Premul, None);
        let mut pixels = vec![0_u8; 8 * 8 * 4];
        assert!(surface.read_pixels(&info, &mut pixels, 8 * 4, (0, 0)));
        assert!(
            pixels
                .chunks_exact(4)
                .all(|pixel| { pixel == [255, 255, 255, 255] || pixel == [0, 128, 0, 255] })
        );
        crate::image_loader::clear_cache();
    }

    fn assert_fractional_svg_viewport(view_box: &str, width: f32, height: f32, edge: [u8; 4]) {
        // Original Chromium141 controls V391: CSS viewport quantization and
        // preserveAspectRatio produce translucent vector edges inside the
        // image. They must not be replaced by a stretched intrinsic raster.
        let source = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"{view_box}\"><rect width=\"100%\" height=\"100%\" fill=\"green\"/></svg>"
        );
        crate::image_loader::clear_cache();
        crate::image_loader::decode_and_install("viewport.svg", source.as_bytes()).unwrap();
        for background in [false, true] {
            let mut surface = Surface::new_raster_n32_premul((80, 100)).unwrap();
            surface.canvas().clear(Color::RED);
            if background {
                let rect = LayoutRect {
                    x: 0.0, y: 0.0, width: 80.0, height: 100.0,
                };
                draw_background_image(surface.canvas(), rect, rect, 0.0, &Style {
                    background_image: Some("url(viewport.svg)".into()),
                    background_repeat: Some("no-repeat".into()),
                    ..Style::default()
                }, 1.0);
            } else {
                draw_image(surface.canvas(), LayoutRect {
                    x: 0.0, y: 0.0, width, height,
                }, "viewport.svg", 1.0);
            }
            let info = ImageInfo::new((80, 100), ColorType::RGBA8888, AlphaType::Premul, None);
            let mut pixels = vec![0_u8; 80 * 100 * 4];
            assert!(surface.read_pixels(&info, &mut pixels, 80 * 4, (0, 0)));
            assert_eq!(&pixels[..4], &edge, "background={background}");
            assert_eq!(&pixels[(1 * 80 + 1) * 4..(1 * 80 + 1) * 4 + 4], &[0, 128, 0, 255]);
        }
        crate::image_loader::clear_cache();
    }

    #[test]
    fn portrait_svg_preserves_fractional_css_viewport_edges() {
        assert_fractional_svg_viewport("0 0 4 6", 200.0 / 3.0, 100.0, [2, 127, 0, 255]);
    }

    #[test]
    fn landscape_svg_preserves_fractional_css_viewport_edges() {
        assert_fractional_svg_viewport("0 0 6 4", 80.0, 160.0 / 3.0, [1, 127, 0, 255]);
    }

    #[test]
    fn raster_background_uses_crisp_clip_at_fractional_layout_coordinates() {
        crate::image_loader::clear_cache();
        let image = image::RgbaImage::from_pixel(2, 2, image::Rgba([0, 128, 0, 255]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        crate::image_loader::decode_and_install("fractional-background.png", &bytes.into_inner())
            .unwrap();

        let mut surface = Surface::new_raster_n32_premul((8, 8)).unwrap();
        surface.canvas().clear(Color::WHITE);
        draw_background_image(
            surface.canvas(),
            LayoutRect {
                x: 0.5,
                y: 0.5,
                width: 4.0,
                height: 4.0,
            },
            LayoutRect {
                x: 0.5,
                y: 0.5,
                width: 4.0,
                height: 4.0,
            },
            0.0,
            &Style {
                background_image: Some("url(\"fractional-background.png\")".to_string()),
                ..Style::default()
            },
            1.0,
        );

        let info = ImageInfo::new((8, 8), ColorType::RGBA8888, AlphaType::Premul, None);
        let mut pixels = vec![0_u8; 8 * 8 * 4];
        assert!(surface.read_pixels(&info, &mut pixels, 8 * 4, (0, 0)));
        assert!(
            pixels
                .chunks_exact(4)
                .all(|pixel| { pixel == [255, 255, 255, 255] || pixel == [0, 128, 0, 255] })
        );
        crate::image_loader::clear_cache();
    }

    #[test]
    fn fractional_background_translation_interpolates_internal_texels() {
        crate::image_loader::clear_cache();
        let mut image = image::RgbaImage::new(2, 1);
        image.put_pixel(0, 0, image::Rgba([0, 128, 128, 255]));
        image.put_pixel(1, 0, image::Rgba([0, 255, 255, 255]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        crate::image_loader::decode_and_install("fractional-texels.png", &bytes.into_inner())
            .unwrap();
        let mut surface = Surface::new_raster_n32_premul((8, 2)).unwrap();
        surface.canvas().clear(Color::WHITE);
        let rect = LayoutRect { x: 0.0, y: 0.0, width: 8.0, height: 2.0 };
        draw_background_image(surface.canvas(), rect, rect, 0.0, &Style {
            background_image: Some("url(fractional-texels.png)".into()),
            background_position: Some("0.5px 0".into()),
            ..Style::default()
        }, 1.0);
        let info = ImageInfo::new((8, 2), ColorType::RGBA8888, AlphaType::Premul, None);
        let mut pixels = vec![0_u8; 8 * 2 * 4];
        assert!(surface.read_pixels(&info, &mut pixels, 8 * 4, (0, 0)));
        assert_eq!(&pixels[4..8], &[0, 191, 191, 255]);
        crate::image_loader::clear_cache();
    }

    #[test]
    fn non_repeating_raster_images_snap_fractional_destination() {
        crate::image_loader::clear_cache();
        let mut image = image::RgbaImage::new(2, 1);
        image.put_pixel(0, 0, image::Rgba([0, 128, 128, 255]));
        image.put_pixel(1, 0, image::Rgba([0, 255, 255, 255]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        crate::image_loader::decode_and_install("snapped-raster.png", &bytes.into_inner()).unwrap();
        for background in [false, true] {
            let mut surface = Surface::new_raster_n32_premul((8, 2)).unwrap();
            surface.canvas().clear(Color::WHITE);
            if background {
                let rect = LayoutRect { x: 0.0, y: 0.0, width: 8.0, height: 2.0 };
                draw_background_image(surface.canvas(), rect, rect, 0.0, &Style {
                    background_image: Some("url(snapped-raster.png)".into()),
                    background_position: Some("0.5px 0".into()),
                    background_repeat: Some("no-repeat".into()),
                    ..Style::default()
                }, 1.0);
            } else {
                draw_image(surface.canvas(), LayoutRect { x: 0.5, y: 0.0, width: 2.0, height: 1.0 }, "snapped-raster.png", 1.0);
            }
            let info = ImageInfo::new((8, 2), ColorType::RGBA8888, AlphaType::Premul, None);
            let mut pixels = [0_u8; 64];
            assert!(surface.read_pixels(&info, &mut pixels, 32, (0, 0)));
            assert_eq!(&pixels[..12], &[255, 255, 255, 255, 0, 128, 128, 255, 0, 255, 255, 255], "background={background}");
        }
        crate::image_loader::clear_cache();
    }

    #[test]
    fn repeating_one_pixel_background_covers_paint_area_beyond_tile_cap() {
        crate::image_loader::clear_cache();
        let image = image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 255, 0, 255]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        crate::image_loader::decode_and_install("one-pixel-repeat.png", &bytes.into_inner())
            .unwrap();

        let mut surface = Surface::new_raster_n32_premul((128, 40)).unwrap();
        surface.canvas().clear(Color::WHITE);
        let rect = LayoutRect {
            x: 0.0,
            y: 0.0,
            width: 128.0,
            height: 40.0,
        };
        draw_background_image(
            surface.canvas(),
            rect,
            rect,
            0.0,
            &Style {
                background_image: Some("url(one-pixel-repeat.png)".into()),
                ..Style::default()
            },
            1.0,
        );
        let info = ImageInfo::new((128, 40), ColorType::RGBA8888, AlphaType::Premul, None);
        let mut pixels = vec![0_u8; 128 * 40 * 4];
        assert!(surface.read_pixels(&info, &mut pixels, 128 * 4, (0, 0)));
        for (x, y) in [(0, 0), (127, 20), (127, 39)] {
            let offset = (y * 128 + x) * 4;
            assert_eq!(
                &pixels[offset..offset + 4],
                &[0, 255, 0, 255],
                "pixel {x},{y}"
            );
        }
        crate::image_loader::clear_cache();
    }

    #[test]
    fn repeated_background_shader_preserves_tile_phase_and_clip() {
        crate::image_loader::clear_cache();
        let mut image = image::RgbaImage::new(2, 2);
        for (x, y, color) in [
            (0, 0, [255, 0, 0, 255]),
            (1, 0, [0, 255, 0, 255]),
            (0, 1, [0, 0, 255, 255]),
            (1, 1, [255, 255, 0, 255]),
        ] {
            image.put_pixel(x, y, image::Rgba(color));
        }
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        crate::image_loader::decode_and_install("two-pixel-repeat.png", &bytes.into_inner())
            .unwrap();

        let mut surface = Surface::new_raster_n32_premul((140, 140)).unwrap();
        surface.canvas().clear(Color::WHITE);
        let rect = LayoutRect {
            x: 5.0,
            y: 7.0,
            width: 128.0,
            height: 130.0,
        };
        draw_background_image(
            surface.canvas(),
            rect,
            rect,
            0.0,
            &Style {
                background_image: Some("url(two-pixel-repeat.png)".into()),
                ..Style::default()
            },
            1.0,
        );
        let info = ImageInfo::new((140, 140), ColorType::RGBA8888, AlphaType::Premul, None);
        let mut pixels = vec![0_u8; 140 * 140 * 4];
        assert!(surface.read_pixels(&info, &mut pixels, 140 * 4, (0, 0)));
        for (x, y, color) in [
            (5, 7, [255, 0, 0, 255]),
            (6, 7, [0, 255, 0, 255]),
            (5, 8, [0, 0, 255, 255]),
            (6, 8, [255, 255, 0, 255]),
            (132, 136, [255, 255, 0, 255]),
            (4, 7, [255, 255, 255, 255]),
            (133, 136, [255, 255, 255, 255]),
        ] {
            let offset = (y * 140 + x) * 4;
            assert_eq!(&pixels[offset..offset + 4], &color, "pixel {x},{y}");
        }
        crate::image_loader::clear_cache();
    }

    #[test]
    fn assigned_inline_separator_keeps_background_advance() {
        let mut surface = Surface::new_raster_n32_premul((40, 30)).unwrap();
        surface.canvas().clear(Color::WHITE);
        let font = test_font();
        let typeface = FontMgr::default().new_from_data(TEST_FONT, None).unwrap();
        let style = Style {
            display: Display::Inline,
            font_size: 15.0,
            line_height: 1.0,
            background: w3cos_std::color::Color::rgb(0, 0, 255),
            ..Style::default()
        };
        draw_text_in_rect(surface.canvas(), LayoutRect {
            x: 8.0, y: 5.0, width: 15.0, height: 15.0,
        }, " ", &style, &typeface, &font);
        let info = ImageInfo::new((40, 30), ColorType::RGBA8888, AlphaType::Premul, None);
        let mut pixels = vec![0_u8; 40 * 30 * 4];
        assert!(surface.read_pixels(&info, &mut pixels, 40 * 4, (0, 0)));
        assert_eq!(&pixels[(10 * 40 + 8) * 4..(10 * 40 + 8) * 4 + 4],
            &[0, 0, 255, 255], "a laid-out separator must paint its assigned background");
        assert_eq!(&pixels[(10 * 40 + 23) * 4..(10 * 40 + 23) * 4 + 4],
            &[255, 255, 255, 255], "separator must not expand beyond assigned advance");
    }

    #[test]
    fn split_inline_background_precedes_all_its_glyph_fragments() {
        // FontRegistry is process-global. Keep this real-font regression out
        // of tests that deliberately exercise the unregistered Ahem fallback.
        const CHILD: &str = "W3COS_TEST_INLINE_BACKGROUND_FONT_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "render_skia::tests::split_inline_background_precedes_all_its_glyph_fragments", "--nocapture"])
                .env(CHILD, "1").output().unwrap();
            assert!(output.status.success(), "isolated registered-font regression failed:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
            assert!(!String::from_utf8_lossy(&output.stderr).contains("skipped:"),
                "registered-font regression needs the pinned WPT font");
            return;
        }
        use crate::font_face::{FontFace, FontRegistry, FontSource};
        use crate::html_parser_host::InertParserScriptHost;
        use crate::html_parser_state::StreamingDocumentParser;
        use std::rc::Rc;
        const OWNER: u64 = 202610040049;
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../wpt/fonts/Ahem.ttf");
        let Ok(bytes) = std::fs::read(path) else {
            eprintln!("skipped: no pinned WPT checkout at {path}");
            return;
        };
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let mut parser = StreamingDocumentParser::new_with_script_host(
            Rc::new(InertParserScriptHost), "https://example.test/inline-separator.html",
        ).unwrap();
        parser.write("<!doctype html><body style='margin:0'><div style='margin-left:23px;margin-top:50px;width:180px;font:15px/1 Ahem'><p style='margin:0;background:yellow;color:lime'>xx <span style='margin-right:-60px;background:blue;color:orange'>xx xx xx </span> xx</p></div>").unwrap();
        parser.finish().unwrap();
        let registry = FontRegistry::global();
        registry.register_for_owner(OWNER, FontFace { family: "Ahem".into(),
            src: FontSource::Bytes(bytes), ..Default::default() }).unwrap();
        let frame = crate::headless::render_document_rgba(240, 90).unwrap();
        let mut expected = Surface::new_raster_n32_premul((240, 90)).unwrap();
        expected.canvas().clear(Color::WHITE);
        expected.canvas().draw_rect(Rect::from_xywh(68.0, 50.0, 135.0, 15.0),
            &color_paint(w3cos_std::Color::rgb(0, 0, 255), 1.0));
        let style = Style { font_family: Some("Ahem".into()), font_size: 15.0,
            line_height: 1.0, color: w3cos_std::Color::rgb(255, 165, 0),
            ..Style::default() };
        let (_, face) = registered_typeface_covering(&style, "xx xx xx ").unwrap();
        draw_text_line(expected.canvas(), 68.0, 50.0, "xx xx xx ", 15.0,
            style.color, 1.0, &face, &style);
        let info = ImageInfo::new((240, 90), ColorType::RGBA8888, AlphaType::Premul, None);
        let mut expected_pixels = vec![0_u8; 240 * 90 * 4];
        assert!(expected.read_pixels(&info, &mut expected_pixels, 240 * 4, (0, 0)));
        registry.clear_owner(OWNER);
        for x in [98, 188] {
            assert_eq!(&frame.rgba[(55 * 240 + x) * 4..(55 * 240 + x) * 4 + 4],
                &expected_pixels[(55 * 240 + x) * 4..(55 * 240 + x) * 4 + 4],
                "one inline background must precede all its glyph fragments at {x}");
        }
    }

    #[test]
    fn collapsed_decorated_inline_space_has_no_painted_advance() {
        let mut surface = Surface::new_raster_n32_premul((40, 60)).unwrap();
        surface.canvas().clear(Color::WHITE);
        let font = test_font();
        let typeface = FontMgr::default().new_from_data(TEST_FONT, None).unwrap();
        let style = Style {
            display: Display::Inline,
            font_size: 10.0,
            border_color: w3cos_std::color::Color::rgb(0, 0, 255),
            border_top_width: Some(10.0),
            border_right_width: Some(0.0),
            border_bottom_width: Some(10.0),
            border_left_width: Some(0.0),
            ..Style::default()
        };
        draw_text_in_rect(
            surface.canvas(),
            LayoutRect {
                x: 5.0,
                y: 10.0,
                width: 0.0,
                height: 34.0,
            },
            " ",
            &style,
            &typeface,
            &font,
        );
        let info = ImageInfo::new((40, 60), ColorType::RGBA8888, AlphaType::Premul, None);
        let mut pixels = vec![0_u8; 40 * 60 * 4];
        assert!(surface.read_pixels(&info, &mut pixels, 40 * 4, (0, 0)));
        assert!(
            pixels
                .chunks_exact(4)
                .all(|pixel| pixel == [255, 255, 255, 255])
        );

        let mut terminal = style;
        terminal.border_right_width = Some(10.0);
        draw_text_in_rect(
            surface.canvas(),
            LayoutRect {
                x: 5.0,
                y: 10.0,
                width: 0.0,
                height: 34.0,
            },
            " ",
            &terminal,
            &typeface,
            &font,
        );
        assert!(surface.read_pixels(&info, &mut pixels, 40 * 4, (0, 0)));
        assert!(
            pixels
                .chunks_exact(4)
                .any(|pixel| pixel == [0, 0, 255, 255]),
            "a logical end border must still paint"
        );
    }

    #[test]
    fn text_leaf_clips_its_own_hidden_overflow() {
        let mut surface = Surface::new_raster_n32_premul((96, 32)).unwrap();
        surface.canvas().clear(Color::TRANSPARENT);
        let typeface = FontMgr::default()
            .new_from_data(TEST_FONT, None)
            .expect("Skia test typeface");
        let metrics_font = test_font();
        let style = Style {
            color: w3cos_std::color::Color::rgba(0, 0, 0, 255),
            font_size: 20.0,
            white_space: w3cos_std::style::WhiteSpace::NoWrap,
            overflow_x: Some(w3cos_std::style::Overflow::Hidden),
            overflow_y: Some(w3cos_std::style::Overflow::Hidden),
            ..Style::default()
        };
        let rect = LayoutRect {
            x: 2.0,
            y: 2.0,
            width: 24.0,
            height: 26.0,
        };

        draw_text_in_rect(
            surface.canvas(),
            rect,
            "MMMMMMMM",
            &style,
            &typeface,
            &metrics_font,
        );

        let mut pixels = vec![0_u8; 96 * 32 * 4];
        let info = ImageInfo::new((96, 32), ColorType::RGBA8888, AlphaType::Premul, None);
        assert!(surface.read_pixels(&info, &mut pixels, 96 * 4, (0, 0)));
        let has_ink_inside = pixels
            .chunks_exact(4)
            .enumerate()
            .any(|(index, pixel)| index % 96 < 26 && pixel[3] != 0);
        let has_ink_after_clip = pixels
            .chunks_exact(4)
            .enumerate()
            .any(|(index, pixel)| index % 96 >= 26 && pixel[3] != 0);

        assert!(has_ink_inside, "test text should paint inside its own box");
        assert!(
            !has_ink_after_clip,
            "nowrap glyphs must not paint beyond the text leaf overflow clip"
        );
    }

    #[test]
    fn single_line_descender_stays_within_nominal_line_height() {
        let mut surface = Surface::new_raster_n32_premul((32, 32)).unwrap();
        surface.canvas().clear(Color::TRANSPARENT);
        let typeface = FontMgr::default()
            .new_from_data(TEST_FONT, None)
            .expect("Skia test typeface");
        let metrics_font = test_font();
        let style = Style {
            color: w3cos_std::color::Color::BLACK,
            font_size: 20.0,
            line_height: 1.2,
            ..Style::default()
        };

        draw_text_in_rect(
            surface.canvas(),
            LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 32.0,
                height: 24.0,
            },
            "p",
            &style,
            &typeface,
            &metrics_font,
        );

        let info = ImageInfo::new((32, 32), ColorType::RGBA8888, AlphaType::Premul, None);
        let mut pixels = vec![0_u8; 32 * 32 * 4];
        assert!(surface.read_pixels(&info, &mut pixels, 32 * 4, (0, 0)));
        assert!(
            pixels[24 * 32 * 4..]
                .chunks_exact(4)
                .all(|pixel| pixel[3] == 0),
            "fallback glyph ink must not leak below its nominal line box"
        );
    }

    #[test]
    fn single_line_and_first_explicit_multiline_run_share_the_same_baseline() {
        let mut single = Surface::new_raster_n32_premul((64, 40)).unwrap();
        let mut multiline = Surface::new_raster_n32_premul((64, 40)).unwrap();
        single.canvas().clear(Color::TRANSPARENT);
        multiline.canvas().clear(Color::TRANSPARENT);
        let typeface = FontMgr::default()
            .new_from_data(TEST_FONT, None)
            .expect("Skia test typeface");
        let metrics_font = test_font();
        let style = Style {
            color: w3cos_std::color::Color::BLACK,
            font_size: 16.0,
            line_height: 1.2,
            ..Style::default()
        };
        draw_text_in_rect(
            single.canvas(),
            LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 64.0,
                height: 19.2,
            },
            "i",
            &style,
            &typeface,
            &metrics_font,
        );
        draw_text_in_rect(
            multiline.canvas(),
            LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 64.0,
                height: 38.4,
            },
            "i\u{2028}i",
            &style,
            &typeface,
            &metrics_font,
        );

        let info = ImageInfo::new((64, 40), ColorType::RGBA8888, AlphaType::Premul, None);
        let mut single_pixels = vec![0_u8; 64 * 40 * 4];
        let mut multiline_pixels = vec![0_u8; 64 * 40 * 4];
        assert!(single.read_pixels(&info, &mut single_pixels, 64 * 4, (0, 0)));
        assert!(multiline.read_pixels(&info, &mut multiline_pixels, 64 * 4, (0, 0)));
        assert_eq!(
            &single_pixels[..64 * 19 * 4],
            &multiline_pixels[..64 * 19 * 4]
        );
    }

    #[test]
    fn block_text_starts_at_the_content_box_top_unless_explicitly_centered() {
        let block = Style {
            display: Display::Block,
            ..Style::default()
        };
        assert_eq!(text_vertical_offset(&block, 84.0, 19.2), 0.0);

        let inline_block = Style {
            display: Display::InlineBlock,
            ..Style::default()
        };
        assert_eq!(text_vertical_offset(&inline_block, 84.0, 19.2), 0.0);

        let centered = Style {
            display: Display::Block,
            justify_content: JustifyContent::Center,
            ..Style::default()
        };
        assert!((text_vertical_offset(&centered, 84.0, 19.2) - 32.4).abs() < 0.01);
    }

    #[test]
    fn inline_text_keeps_its_line_box_instead_of_centring_in_its_em_box() {
        // A short `line-height` overflows the line box symmetrically. The
        // painter must not add that half-leading back as a centring offset, or
        // the glyph lands on the line box top instead of bleeding above it.
        let inline = Style {
            display: Display::Inline,
            font_size: 20.0,
            line_height: 0.0,
            ..Style::default()
        };
        assert_eq!(text_vertical_offset(&inline, 20.0, 0.0), 0.0);
        assert_eq!(text_vertical_offset(&inline, 20.0, 10.0), 0.0);

        // A blockified inline run owns its own line box and still centres.
        for position in [
            w3cos_std::style::Position::Absolute,
            w3cos_std::style::Position::Fixed,
        ] {
            let blockified = Style {
                position,
                ..inline.clone()
            };
            assert!((text_vertical_offset(&blockified, 20.0, 0.0) - 10.0).abs() < 0.01);
        }
        let floated = Style {
            float: w3cos_std::style::Float::Left,
            ..inline.clone()
        };
        assert!((text_vertical_offset(&floated, 20.0, 0.0) - 10.0).abs() < 0.01);
    }

    #[test]
    fn text_origin_does_not_depend_on_the_box_display() {
        // Every display that generates a box owns the line its text is laid out
        // in, so a negative ink bearing must not move it: `display: table` and
        // `display: table-row` start their text exactly like `display: block`.
        // A whitelist here silently regressed the table parts and `flex` (which
        // is `Display`'s `#[default]`, so it also catches an unresolved
        // `display: inherit`). See
        // css/CSS2/generated-content/after-content-display-006.xht and -011.xht.
        let typeface = FontMgr::default().new_from_data(TEST_FONT, None).unwrap();
        let ink_left = -1.0;
        for display in [
            Display::Block,
            Display::FlowRoot,
            Display::Flex,
            Display::Grid,
            Display::Inline,
            Display::InlineBlock,
            Display::InlineFlex,
            Display::Table,
            Display::InlineTable,
            Display::TableRowGroup,
            Display::TableHeaderGroup,
            Display::TableFooterGroup,
            Display::TableRow,
            Display::TableColumnGroup,
            Display::TableColumn,
            Display::TableCell,
            Display::TableCaption,
            Display::ListItem,
        ] {
            let style = Style {
                display,
                ..Style::default()
            };
            assert_eq!(
                alignment_ink_left("Filler text", ink_left, 16.0, &typeface, &style),
                0.0,
                "{display:?} owns its line box and must keep the advance origin"
            );
        }
        // Only the displays that generate no box of their own keep the ink
        // compensation, and those never paint text.
        for display in [Display::None, Display::Contents] {
            let style = Style {
                display,
                ..Style::default()
            };
            assert_eq!(
                alignment_ink_left("Filler text", ink_left, 16.0, &typeface, &style),
                ink_left,
                "{display:?} generates no box and keeps the compensation"
            );
        }
    }

    #[test]
    fn line_box_centers_the_em_box_with_half_leading() {
        let style = Style {
            font_size: 30.0,
            line_height: 4.0,
            ..Style::default()
        };
        assert_eq!(line_box_half_leading(&style), 45.0);

        let inline = Style {
            display: Display::Inline,
            ..style
        };
        assert_eq!(line_box_half_leading(&inline), 0.0);
    }

    #[test]
    fn line_box_block_upper_leading_matches_inline_baseline() {
        for position in [w3cos_std::style::Position::Static,
            w3cos_std::style::Position::Absolute, w3cos_std::style::Position::Fixed] {
            let style = Style { display: Display::Block, position,
                font_family: Some("monospace".into()), font_size: 32.0,
                font_weight: 700, line_height: 1.0, line_height_is_normal: false,
                ..Style::default() };
            let metrics = resolved_font_geometry(&style).unwrap();
            assert_eq!(metrics.ascent + line_box_half_leading(&style),
                crate::layout::inline_font_baseline_from_line_top(&style, 32.0),
                "block/absolute text and the anonymous inline strut need one baseline");
        }
    }

    #[test]
    fn blockified_inline_text_retains_its_line_box_half_leading() {
        for position in [
            w3cos_std::style::Position::Absolute,
            w3cos_std::style::Position::Fixed,
        ] {
            let style = Style {
                display: Display::Inline,
                position,
                font_size: 30.0,
                line_height: 4.0,
                ..Style::default()
            };
            assert_eq!(line_box_half_leading(&style), 45.0);
        }
        let floated = Style {
            display: Display::Inline,
            float: w3cos_std::style::Float::Left,
            font_size: 30.0,
            line_height: 4.0,
            ..Style::default()
        };
        assert_eq!(line_box_half_leading(&floated), 45.0);
    }

    #[test]
    fn text_alignment_uses_the_inline_advance_not_the_ink_width() {
        let rect = LayoutRect {
            x: 10.0,
            y: 0.0,
            width: 100.0,
            height: 20.0,
        };
        assert_eq!(aligned_text_x(rect, TextAlign::Right, 1.0, 40.0), 70.0);
        assert_eq!(aligned_text_x(rect, TextAlign::Center, 1.0, 40.0), 40.0);
        assert_eq!(aligned_text_x(rect, TextAlign::Left, 1.0, 40.0), 10.0);
        assert_eq!(aligned_text_x(rect, TextAlign::Left, -0.25, 40.0), 10.25);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn serif_text_advance_applies_font_positioning() {
        // CSS2 inline-box cases use the browser's generic serif face. Its
        // OpenType positioning adjusts Tw even though Skia's legacy pair
        // adjustment API reports no adjustments for this face.
        let style = Style {
            font_family: Some("serif".to_string()),
            font_size: 16.0,
            ..Style::default()
        };
        let typeface = generic_serif_typeface(&style).expect("macOS generic serif face");
        let separate = ["T", "w", "o"]
            .into_iter()
            .map(|text| measure_skia_text_advance(text, &typeface, &style))
            .sum::<f32>();
        let together = measure_skia_text_advance("Two", &typeface, &style);
        assert!(
            together < separate - 0.5,
            "font positioning must affect the inline advance: together={together}, separate={separate}"
        );
    }

    #[test]
    fn ahem_text_uses_one_square_em_per_character() {
        let typeface = FontMgr::default()
            .new_from_data(TEST_FONT, None)
            .expect("Skia test typeface");
        let style = Style {
            font_family: Some("Ahem".to_string()),
            font_size: 20.0,
            ..Style::default()
        };

        assert_eq!(measure_skia_text_advance(" A ", &typeface, &style), 60.0);
        let spaced = Style {
            word_spacing: 30.0,
            ..style.clone()
        };
        assert_eq!(measure_skia_text_advance("A A", &typeface, &spaced), 90.0);
        let letter_spaced = Style {
            letter_spacing: 96.0,
            ..style.clone()
        };
        assert_eq!(
            measure_skia_text_advance("xx", &typeface, &letter_spaced),
            232.0 // Advance includes the final spacing; ink below does not.
        );
        assert_eq!(
            measure_skia_text_ink_bounds("xx", 20.0, &typeface, 400, Some(&letter_spaced)),
            text_layout::InkBounds {
                left: 0.0,
                top: 0.0,
                width: 136.0,
                height: 20.0,
            }
        );
        assert_eq!(
            measure_skia_text_advance("\u{202e} A \u{202c}", &typeface, &style),
            60.0,
            "bidi formatting controls affect order but consume no Ahem cell"
        );
        assert_eq!(
            measure_skia_text_ink_bounds(" A ", 20.0, &typeface, 400, Some(&style)),
            text_layout::InkBounds {
                left: 20.0,
                top: 0.0,
                width: 20.0,
                height: 20.0,
            }
        );
        assert_eq!(
            measure_skia_text_ink_bounds("pp", 20.0, &typeface, 400, Some(&style)),
            text_layout::InkBounds {
                left: 0.0,
                top: 16.0,
                width: 40.0,
                height: 4.0,
            }
        );
    }

    #[test]
    fn registered_css_font_supplies_skia_typeface_and_releases_with_owner() {
        const OWNER: u64 = 0x534b_4941_464f_4e54;
        const FAMILY: &str = "W3COS Skia Font Test";
        let style = Style {
            font_family: Some(FAMILY.to_string()),
            ..Style::default()
        };
        assert!(registered_typeface(&style).is_none());
        crate::font_face::FontRegistry::global()
            .register_for_owner(
                OWNER,
                crate::font_face::FontFace {
                    family: FAMILY.to_string(),
                    src: crate::font_face::FontSource::Bytes(TEST_FONT.to_vec()),
                    ..crate::font_face::FontFace::default()
                },
            )
            .expect("register Skia font");
        let (loaded, typeface) = registered_typeface(&style).expect("registered Skia typeface");
        assert_eq!(loaded.family, FAMILY);
        assert_ne!(typeface.unichar_to_glyph('W' as i32), 0);

        drop(loaded);
        drop(typeface);
        crate::font_face::FontRegistry::global().clear_owner(OWNER);
        assert!(registered_typeface(&style).is_none());
    }

    #[test]
    fn generic_serif_selects_a_system_serif_typeface() {
        let style = Style {
            font_family: Some("serif".to_string()),
            ..Style::default()
        };
        let serif = generic_serif_typeface(&style).expect("system serif typeface");
        assert_ne!(serif.family_name(), "Inter");
    }

    #[test]
    fn block_and_inline_text_share_the_same_baseline_with_descenders() {
        let typeface = FontMgr::default().new_from_data(TEST_FONT, None).unwrap();
        let style = Style {
            display: Display::Block,
            color: w3cos_std::color::Color::BLACK,
            font_family: Some("serif".into()),
            ..Style::default()
        };
        let kind = ComponentKind::Text {
            content:
                "There should be a single fuchsia diamond at the bottom right of the viewport."
                    .into(),
        };
        let raster = |display, y, height| {
            let mut surface = Surface::new_raster_n32_premul((800, 80)).unwrap();
            surface.canvas().clear(Color::WHITE);
            render_node(
                surface.canvas(),
                0,
                LayoutRect {
                    x: 32.0,
                    y,
                    width: 736.0,
                    height,
                },
                &kind,
                &Style {
                    display,
                    ..style.clone()
                },
                &typeface,
                crate::layout::layout_font(),
                None,
                false,
                false,
            );
            let info = ImageInfo::new((800, 80), ColorType::RGBA8888, AlphaType::Premul, None);
            let mut pixels = vec![0_u8; 800 * 80 * 4];
            assert!(surface.read_pixels(&info, &mut pixels, 800 * 4, (0, 0)));
            pixels
        };
        let block = raster(Display::Block, 32.0, 19.2);
        let font_height = resolved_font_geometry(&style).unwrap().height();
        let inline = raster(Display::Inline, 32.0 + (19.2 - font_height) * 0.5, font_height);
        assert!(
            block
                .chunks_exact(4)
                .any(|pixel| pixel[..3] != [255, 255, 255])
        );
        assert!(
            inline
                .chunks_exact(4)
                .any(|pixel| pixel[..3] != [255, 255, 255])
        );
        let different_pixels = block
            .chunks_exact(4)
            .zip(inline.chunks_exact(4))
            .filter(|(left, right)| left != right)
            .count();
        assert_eq!(
            different_pixels, 0,
            "block and inline must share their font baseline"
        );
    }

    #[test]
    fn skia_uses_regular_space_metrics_for_non_breaking_space() {
        let primary = FontMgr::default().new_from_data(TEST_FONT, None).unwrap();
        let style = Style::default();
        let space = measure_skia_text_advance(" ", &primary, &style);
        let non_breaking_space = measure_skia_text_advance("\u{00a0}", &primary, &style);
        assert!((space - non_breaking_space).abs() < 0.01);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn justified_line_paint_matches_contextual_word_spacing() {
        let primary = FontMgr::default().match_family_style("Times",
            skia_safe::FontStyle::normal()).unwrap();
        let text = "Some words justified";
        let style = Style { font_family: Some("serif".into()), font_size: 20.0,
            ..Style::default() };
        let available = text_layout::inline_layout_advance(
            measure_skia_text_advance(text, &primary, &style)) + 20.0;
        let expansion = text_layout::justification_expansion(text, available,
            measure_skia_text_advance(text, &primary, &style)).unwrap();
        let raster = |expanded| {
            let mut surface = Surface::new_raster_n32_premul((400, 60)).unwrap();
            surface.canvas().clear(Color::WHITE);
            if expanded {
                draw_justified_text_line(surface.canvas(), 8.375, 4.0, text, 20.0,
                    w3cos_std::Color::BLACK, 1.0, &primary, &style, expansion);
            } else {
                let reference = Style { word_spacing: 10.0, ..style.clone() };
                draw_text_line(surface.canvas(), 8.375, 4.0, text, 20.0,
                    w3cos_std::Color::BLACK, 1.0, &primary, &reference);
            }
            let info = ImageInfo::new((400, 60), ColorType::RGBA8888, AlphaType::Premul, None);
            let mut pixels = vec![0; 400 * 60 * 4];
            assert!(surface.read_pixels(&info, &mut pixels, 400 * 4, (0, 0)));
            pixels
        };
        assert_eq!(raster(true), raster(false));
    }

    #[test]
    fn inline_background_keeps_the_resolved_tab_fragment_extent() {
        let primary = FontMgr::default().new_from_data(TEST_FONT, None).unwrap();
        let style = Style {
            display: Display::Inline,
            font_family: Some("Ahem".into()), font_size: 20.0,
            white_space: w3cos_std::style::WhiteSpace::Pre,
            background: w3cos_std::Color::rgb(0, 0, 255),
            ..Style::default()
        };
        let fragment = LayoutRect { x: 80.0, y: 10.0, width: 80.0, height: 20.0 };
        let mut surface = Surface::new_raster_n32_premul((300, 60)).unwrap();
        surface.canvas().clear(Color::WHITE);
        render_node_with_line_context(surface.canvas(), 0, fragment,
            &ComponentKind::Row, &style, &primary, crate::layout::layout_font(),
            None, false, false, None, None, false, true,
            &[(fragment, "\tB".into())], None);
        let info = ImageInfo::new((300, 60), ColorType::RGBA8888, AlphaType::Premul, None);
        let mut pixels = vec![0; 300 * 60 * 4];
        assert!(surface.read_pixels(&info, &mut pixels, 300 * 4, (0, 0)));
        let pixel = |x: usize| &pixels[(15 * 300 + x) * 4..(15 * 300 + x) * 4 + 4];
        assert_eq!(pixel(80), &[0, 0, 255, 255]);
        assert_eq!(pixel(159), &[0, 0, 255, 255]);
        assert_eq!(pixel(160), &[255, 255, 255, 255],
            "background must stop at the resolved fragment, not remeasure a tab from zero");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn authored_coalesced_text_paints_the_layout_unit_fragment_origins() {
        let primary = FontMgr::default().new_from_data(TEST_FONT, None).unwrap();
        let text = "All the words are aligned on the same baseline.";
        let mut style = Style { font_family: Some("serif".into()), font_size: 15.0,
            white_space: w3cos_std::style::WhiteSpace::NoWrap, ..Style::default() };
        w3cos_std::inline_text::set_fragment_ends(&mut style, text, &[8, 13, text.len()]);
        let render = |coalesced| {
            let mut surface = Surface::new_raster_n32_premul((400, 40)).unwrap();
            surface.canvas().clear(Color::WHITE);
            if coalesced {
                let advance = draw_text_line(surface.canvas(), 8.0, 4.0, text, 15.0,
                    w3cos_std::Color::BLACK, 1.0, &primary, &style);
                assert!((advance - 283.6875).abs() < 0.001, "advance={advance}");
            } else {
                let mut plain = style.clone();
                plain.custom_properties.as_mut().unwrap().remove(w3cos_std::inline_text::FRAGMENT_ENDS);
                for (x, part) in [(8.0, "All the "), (53.0, "words"),
                    (89.671875, " are aligned on the same baseline.")] {
                    draw_text_line(surface.canvas(), x, 4.0, part, 15.0,
                        w3cos_std::Color::BLACK, 1.0, &primary, &plain);
                }
            }
            let info = ImageInfo::new((400, 40), ColorType::RGBA8888, AlphaType::Premul, None);
            let mut pixels = vec![0; 400 * 40 * 4];
            assert!(surface.read_pixels(&info, &mut pixels, 400 * 4, (0, 0)));
            pixels
        };
        assert_eq!(render(true), render(false));
    }

    #[test]
    fn preserved_tabs_use_positioned_stops_in_measurement_ink_and_paint() {
        let primary = FontMgr::default().new_from_data(TEST_FONT, None).unwrap();
        for family in ["Ahem", "monospace", "serif"] {
            for spacing in [0.0, 12.0] {
                let style = Style {
                    font_family: Some(family.into()),
                    font_size: 20.0,
                    white_space: w3cos_std::style::WhiteSpace::Pre,
                    word_spacing: spacing,
                    ..Style::default()
                };
                let stop = 8.0 * measure_skia_text_advance(" ", &primary, &style);
                let text = "A\tB\tC";
                let expected = 2.0 * stop + measure_skia_text_advance("C", &primary, &style);
                let actual = measure_skia_text_advance(text, &primary, &style);
                assert!((actual - expected).abs() < 0.01,
                    "family={family}, spacing={spacing}, measured={actual}, expected={expected}");
                let ink = measure_skia_text_ink_bounds(text, 20.0, &primary, 400, Some(&style));
                let last = measure_skia_text_ink_bounds("C", 20.0, &primary, 400, Some(&style));
                assert!((ink.left + ink.width - (2.0 * stop + last.left + last.width)).abs() < 0.01);
                let raster = |tabbed| {
                    let mut surface = Surface::new_raster_n32_premul((900, 40)).unwrap();
                    surface.canvas().clear(Color::WHITE);
                    if tabbed {
                        let advance = draw_text_line(surface.canvas(), 8.0, 4.0, text, 20.0,
                            w3cos_std::Color::BLACK, 1.0, &primary, &style);
                        assert!((advance - expected).abs() < 0.01);
                    } else {
                        for (offset, text) in [(0.0, "A"), (stop, "B"), (2.0 * stop, "C")] {
                            draw_text_line(surface.canvas(), 8.0 + offset, 4.0, text, 20.0,
                                w3cos_std::Color::BLACK, 1.0, &primary, &style);
                        }
                    }
                    let info = ImageInfo::new((900, 40), ColorType::RGBA8888, AlphaType::Premul, None);
                    let mut pixels = vec![0; 900 * 40 * 4];
                    assert!(surface.read_pixels(&info, &mut pixels, 900 * 4, (0, 0)));
                    pixels
                };
                assert_eq!(raster(true), raster(false), "family={family}, spacing={spacing}");
            }
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn generic_monospace_resolves_platform_face_for_shared_metrics() {
        let primary = FontMgr::default().new_from_data(TEST_FONT, None).unwrap();
        let courier = FontMgr::default()
            .match_family_style("Courier", FontStyle::normal()).unwrap();
        for size in [13.0, 16.0] {
            let style = Style {
                font_family: Some("monospace".into()),
                font_size: size,
                ..Style::default()
            };
            let runs = css_font_runs("0W", &primary, &style);
            assert!(runs.iter().all(|run| run.typeface.family_name() == "Courier"),
                "generic fixed family must not use the embedding's primary face");
            let expected = crate::skia_text_run::css_font(&courier, size).measure_str("0W", None).0;
            let measured = measure_skia_text_intrinsic_size("0W", &style).0;
            assert!((measured - expected).abs() < 0.01, "{measured} != {expected}");
            let geometry = resolved_font_geometry(&style).expect("resolved fixed font metrics");
            assert_eq!(geometry.line_spacing(), if size == 13.0 { 15.0 } else { 18.0 });
        }
    }

    #[test]
    fn monospace_overlays_keep_a_shared_baseline_independent_of_glyphs() {
        let typeface = FontMgr::default().new_from_data(TEST_FONT, None).unwrap();
        let style = Style {
            display: Display::Inline,
            font_family: Some("monospace".to_string()),
            font_size: 32.0,
            font_weight: 700,
            line_height: 1.0,
            color: w3cos_std::color::Color::BLACK,
            ..Style::default()
        };
        let render = |lines: &[&str]| {
            let mut surface = Surface::new_raster_n32_premul((240, 96)).unwrap();
            surface.canvas().clear(Color::WHITE);
            for line in lines {
                draw_text_in_rect(
                    surface.canvas(),
                    LayoutRect {
                        x: 8.0,
                        y: 40.0,
                        width: 220.0,
                        height: 32.0,
                    },
                    line,
                    &style,
                    &typeface,
                    crate::layout::layout_font(),
                );
            }
            let info = ImageInfo::new((240, 96), ColorType::RGBA8888, AlphaType::Premul, None);
            let mut pixels = vec![0_u8; 240 * 96 * 4];
            assert!(surface.read_pixels(&info, &mut pixels, 240 * 4, (0, 0)));
            pixels
        };
        let actual = render(&[
            "FAIL \u{a0}\u{a0}\u{a0}\u{a0}",
            "#\u{a0}\u{a0}\u{a0} P\u{a0}\u{a0}\u{a0}",
            "\u{a0}##\u{a0} \u{a0}A\u{a0}\u{a0}",
            "\u{a0}\u{a0}\u{a0}# \u{a0}\u{a0}SS",
        ]);
        let expected = render(&["FAIL PASS", "####"]);
        assert_eq!(
            actual.iter().zip(&expected).filter(|(a, b)| a != b).count(),
            0
        );
    }

    #[test]
    fn block_and_inline_text_share_the_same_glyph_origin() {
        let typeface = FontMgr::default().new_from_data(TEST_FONT, None).unwrap();
        let render = |display| {
            let style = Style {
                display,
                font_family: Some("serif".into()),
                font_size: 16.0,
                line_height: 1.2,
                color: w3cos_std::color::Color::BLACK,
                ..Style::default()
            };
            let mut surface = Surface::new_raster_n32_premul((96, 40)).unwrap();
            surface.canvas().clear(Color::WHITE);
            let leading = (style.font_size * style.line_height - style.font_size) * 0.5;
            let rect = if display == Display::Inline {
                LayoutRect {
                    x: 8.0,
                    y: 8.0 + leading,
                    width: 80.0,
                    height: 16.0,
                }
            } else {
                LayoutRect {
                    x: 8.0,
                    y: 8.0,
                    width: 80.0,
                    height: 19.2,
                }
            };
            draw_text_in_rect(
                surface.canvas(),
                rect,
                "Filler Text",
                &style,
                &typeface,
                crate::layout::layout_font(),
            );
            let info = ImageInfo::new((96, 40), ColorType::RGBA8888, AlphaType::Premul, None);
            let mut pixels = vec![0_u8; 96 * 40 * 4];
            assert!(surface.read_pixels(&info, &mut pixels, 96 * 4, (0, 0)));
            pixels
        };
        let expected = render(Display::Inline);
        for display in [
            Display::Block,
            Display::ListItem,
            Display::TableCell,
            Display::TableCaption,
            Display::InlineBlock,
            Display::InlineFlex,
            Display::InlineTable,
        ] {
            let actual = render(display);
            assert_eq!(
                actual.iter().zip(&expected).filter(|(a, b)| a != b).count(),
                0,
                "glyph origin for {display:?}"
            );
        }
    }

    #[test]
    fn serif_glyph_advances_are_invariant_across_inline_runs() {
        let typeface = FontMgr::default().new_from_data(TEST_FONT, None).unwrap();
        let style = Style {
            display: Display::Inline,
            font_family: Some("serif".into()),
            color: w3cos_std::color::Color::BLACK,
            ..Style::default()
        };
        let render = |fragments: &[(f32, &str)]| {
            let mut surface = Surface::new_raster_n32_premul((96, 32)).unwrap();
            surface.canvas().clear(Color::WHITE);
            for (x, text) in fragments {
                let width = measure_skia_text_advance(text, &typeface, &style);
                draw_text_in_rect(
                    surface.canvas(),
                    LayoutRect {
                        x: *x,
                        y: 4.0,
                        width,
                        height: 16.0,
                    },
                    text,
                    &style,
                    &typeface,
                    crate::layout::layout_font(),
                );
            }
            let info = ImageInfo::new((96, 32), ColorType::RGBA8888, AlphaType::Premul, None);
            let mut pixels = vec![0_u8; 96 * 32 * 4];
            assert!(surface.read_pixels(&info, &mut pixels, 96 * 4, (0, 0)));
            pixels
        };
        let actual = render(&[
            (8.0, "a"),
            (15.1015625, " b "),
            (31.1015625, "c"),
            (38.203125, " d"),
        ]);
        let expected = render(&[(8.0, "a b c d")]);
        assert_eq!(
            actual.iter().zip(&expected).filter(|(a, b)| a != b).count(),
            0
        );
    }

    #[test]
    fn default_ascii_text_is_pixel_invariant_across_inline_fragments() {
        let typeface = FontMgr::default().new_from_data(TEST_FONT, None).unwrap();
        let continuous_style = Style {
            display: Display::Block,
            font_family: Some("serif".to_string()),
            color: w3cos_std::color::Color::BLACK,
            ..Style::default()
        };
        let fragment_style = Style {
            display: Display::Inline,
            ..continuous_style.clone()
        };
        let mut continuous = Surface::new_raster_n32_premul((96, 32)).unwrap();
        let mut fragmented = Surface::new_raster_n32_premul((96, 32)).unwrap();
        continuous.canvas().clear(Color::WHITE);
        fragmented.canvas().clear(Color::WHITE);
        draw_text_in_rect(
            continuous.canvas(),
            LayoutRect {
                x: 11.0,
                y: 4.0,
                width: 46.179688,
                height: 19.2,
            },
            "abcde",
            &continuous_style,
            &typeface,
            crate::layout::layout_font(),
        );
        for (x, width, fragment) in [
            (11.0, 8.8515625, "a"),
            (19.851563, 28.117188, "bcd"),
            (47.96875, 9.2109375, "e"),
        ] {
            draw_text_in_rect(
                fragmented.canvas(),
                LayoutRect {
                    x,
                    y: 4.0,
                    width,
                    height: 19.2,
                },
                fragment,
                &fragment_style,
                &typeface,
                crate::layout::layout_font(),
            );
        }
        let info = ImageInfo::new((96, 32), ColorType::RGBA8888, AlphaType::Premul, None);
        let mut continuous_pixels = vec![0_u8; 96 * 32 * 4];
        let mut fragmented_pixels = vec![0_u8; 96 * 32 * 4];
        assert!(continuous.read_pixels(&info, &mut continuous_pixels, 96 * 4, (0, 0)));
        assert!(fragmented.read_pixels(&info, &mut fragmented_pixels, 96 * 4, (0, 0)));
        assert_eq!(continuous_pixels, fragmented_pixels);
    }

    #[test]
    #[cfg(target_os = "macos")]
    #[ignore = "diagnostic only: requires source-bound browser/native PNG inputs"]
    fn fractional_monospace_raster_policy_diagnostic() {
        let browser = image::open(std::env::var("W3COS_FONT_RASTER_BROWSER_PNG").unwrap())
            .unwrap().to_rgba8();
        let native = image::open(std::env::var("W3COS_FONT_RASTER_NATIVE_PNG").unwrap())
            .unwrap().to_rgba8();
        assert_eq!(browser.dimensions(), (800, 600));
        assert_eq!(native.dimensions(), (800, 600));
        let face = FontMgr::default().match_family_style("Courier", FontStyle::normal()).unwrap();
        let style = Style { font_family: Some("monospace".into()),
            font_size: (10.0_f64 * 96.0 / 72.0) as f32,
            color: w3cos_std::color::Color::BLACK, ..Style::default() };
        let text = " The   spacing  on      these         two  sentences need to be the    same!  ";
        let shaped = crate::skia_text_run::shape_visual_run(text, &face, &style).unwrap();
        let geometry = typeface_font_geometry(&face, style.font_size);
        let baseline = 54.0 + geometry.ascent;
        let info = ImageInfo::new((800, 600), ColorType::RGBA8888, AlphaType::Premul, None);
        let mut rows = Vec::new();
        for hinting in [skia_safe::FontHinting::None, skia_safe::FontHinting::Slight,
            skia_safe::FontHinting::Normal, skia_safe::FontHinting::Full] {
            for edging in [skia_safe::font::Edging::AntiAlias, skia_safe::font::Edging::SubpixelAntiAlias] {
                for subpixel in [false, true] {
                    for linear in [false, true] {
                        for precise_blend in [false, true] {
                            let mut font = crate::skia_text_run::css_font(&face, style.font_size);
                            font.set_hinting(hinting).set_edging(edging)
                                .set_subpixel(subpixel).set_linear_metrics(linear);
                            let paint = if precise_blend { color_paint(style.color, 1.0) } else {
                                let mut paint = Paint::default();
                                paint.set_anti_alias(true).set_color(Color::BLACK);
                                paint
                            };
                            let mut surface = Surface::new_raster_n32_premul((800, 600)).unwrap();
                            surface.canvas().clear(Color::WHITE);
                            surface.canvas().draw_glyphs_at(&shaped.glyphs,
                                shaped.positions.as_slice(), (8.0, baseline), &font, &paint);
                            let mut rgba = vec![0; 800 * 600 * 4];
                            assert!(surface.read_pixels(&info, &mut rgba, 800 * 4, (0, 0)));
                            let mut browser_pixels = 0;
                            let mut native_pixels = 0;
                            let mut max_difference = 0;
                            for y in 54..69 {
                                for x in 0..800 {
                                    let offset = (y * 800 + x) as usize * 4;
                                    let actual = &rgba[offset..offset + 4];
                                    let expected = browser.get_pixel(x, y).0;
                                    let original = native.get_pixel(x, y).0;
                                    browser_pixels += usize::from(actual != expected);
                                    native_pixels += usize::from(actual != original);
                                    for channel in 0..4 {
                                        max_difference = max_difference.max(actual[channel].abs_diff(expected[channel]));
                                    }
                                }
                            }
                            rows.push(serde_json::json!({"hinting":format!("{hinting:?}"),
                                "edging":format!("{edging:?}"),"subpixel":subpixel,"linear":linear,
                                "precise_blend":precise_blend,"browser_pixels":browser_pixels,
                                "native_pixels":native_pixels,"max_difference":max_difference}));
                        }
                    }
                }
            }
        }
        assert!(rows.iter().any(|row| row["native_pixels"] == 0),
            "diagnostic must reproduce the original native row before comparing alternatives");
        eprintln!("MONOSPACE_RASTER_DIAGNOSTIC {}", serde_json::json!({
            "baseline":baseline,"font_size":style.font_size,"advance":shaped.advance,
            "glyph_positions":shaped.positions.iter().map(|point| point.x).collect::<Vec<_>>(),"rows":rows}));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn macos_css_text_preserves_fractional_glyph_origins() {
        let face = FontMgr::default().match_family_style("Times", FontStyle::normal()).unwrap();
        for size in [16.0, 24.0, 96.0] {
            let style = Style { font_family: Some("serif".into()), font_size: size,
                color: w3cos_std::color::Color::rgb(0, 128, 0), ..Style::default() };
            let shaped = crate::skia_text_run::shape_visual_run("Filler Text", &face, &style).unwrap();
            // Chromium 141 FontPlatformData::CreateSkFont on macOS. Glyphs
            // and shaping are identical: only the raster font policy differs.
            let mut expected_font = Font::new(&face, size);
            expected_font.set_subpixel(true).set_linear_metrics(true)
                .set_embedded_bitmaps(false)
                .set_edging(skia_safe::font::Edging::SubpixelAntiAlias);
            for x in [8.0, 8.25, 8.5] {
                let mut actual = Surface::new_raster_n32_premul((440, 130)).unwrap();
                let mut expected = Surface::new_raster_n32_premul((440, 130)).unwrap();
                actual.canvas().clear(Color::WHITE);
                expected.canvas().clear(Color::WHITE);
                let paint = color_paint(style.color, style.opacity);
                draw_font_stack_runs(actual.canvas(), x, 100.0, "Filler Text", size,
                    &face, &style, &paint);
                expected.canvas().draw_glyphs_at(&shaped.glyphs, shaped.positions.as_slice(),
                    (x, 100.0), &expected_font, &paint);
                let info = ImageInfo::new((440, 130), ColorType::RGBA8888, AlphaType::Premul, None);
                let mut a = vec![0; 440 * 130 * 4];
                let mut b = a.clone();
                assert!(actual.read_pixels(&info, &mut a, 440 * 4, (0, 0)));
                assert!(expected.read_pixels(&info, &mut b, 440 * 4, (0, 0)));
                let pixels = a.chunks_exact(4).zip(b.chunks_exact(4)).filter(|(a,b)| a != b).count();
                assert_eq!(pixels, 0, "macOS fractional glyph positions: size={size}, x={x}");
            }
        }
    }

    #[test]
    fn css_font_runs_follow_unicode_range_subsets() {
        const OWNER: u64 = 0x534b_4941_5355_4253;
        const FAMILY: &str = "W3COS Skia Subset Test";
        let style = Style {
            font_family: Some(FAMILY.to_string()),
            ..Style::default()
        };
        for unicode_range in ["U+0057", "U+0030-0039"] {
            crate::font_face::FontRegistry::global()
                .register_for_owner(
                    OWNER,
                    crate::font_face::FontFace {
                        family: FAMILY.to_string(),
                        src: crate::font_face::FontSource::Bytes(TEST_FONT.to_vec()),
                        unicode_range: Some(unicode_range.to_string()),
                        ..crate::font_face::FontFace::default()
                    },
                )
                .expect("register Skia subset");
        }
        let primary = FontMgr::default().new_from_data(TEST_FONT, None).unwrap();
        let runs = css_font_runs("W3W", &primary, &style);
        assert_eq!(
            runs.iter().map(|run| run.text).collect::<Vec<_>>(),
            ["W", "3", "W"]
        );
        assert!(runs.iter().all(|run| run.typeface.unique_id() != 0));

        crate::font_face::FontRegistry::global().clear_owner(OWNER);
    }

    #[cfg(any(target_os = "ios", target_os = "macos"))]
    #[test]
    fn missing_cjk_glyphs_use_one_cached_system_font_run() {
        let primary = FontMgr::default().new_from_data(TEST_FONT, None).unwrap();
        assert_eq!(primary.unichar_to_glyph('丹' as i32), 0);

        let fallback = typeface_for_character(&primary, '丹', 400);
        assert_ne!(fallback.unichar_to_glyph('丹' as i32), 0);

        let runs = fallback_font_runs("A丹丹B", &primary, 400);
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0].text, "A");
        assert_eq!(runs[1].text, "丹丹");
        assert_eq!(runs[2].text, "B");
        assert_eq!(runs[1].typeface.unique_id(), fallback.unique_id());
    }

    #[cfg(any(target_os = "ios", target_os = "macos"))]
    #[test]
    fn bold_cjk_uses_a_weighted_system_face() {
        let primary = FontMgr::default()
            .match_family_style("PingFang SC", FontStyle::normal())
            .expect("PingFang regular");
        let regular = typeface_for_character(&primary, '入', 400);
        let bold = typeface_for_character(&primary, '入', 700);

        assert_ne!(regular.unique_id(), bold.unique_id());
        assert!(bold.font_style().weight() > regular.font_style().weight());
    }

    #[cfg(any(target_os = "ios", target_os = "macos"))]
    #[test]
    fn skia_ink_bounds_follow_the_actual_fallback_typeface() {
        let primary = FontMgr::default().new_from_data(TEST_FONT, None).unwrap();
        let ink = measure_skia_text_ink_bounds("✦首次入驻", 17.0, &primary, 400, None);
        assert!(ink.width > 0.0);
        assert!(ink.height > 0.0);
        assert!(ink.top.is_finite());
        assert!(ink.height <= 24.0);
    }

    #[test]
    fn replay_uploads_canvas_pixels_and_applies_filter_chain() {
        let mut context = crate::canvas2d::CanvasRenderingContext2D::new(8, 8);
        context.set_fill_style("#ff0000");
        context.fill_rect(0.0, 0.0, 8.0, 8.0);
        context.publish_to_surface(7);
        assert_eq!(
            &crate::canvas2d::surface_snapshot(7).unwrap().pixels[..4],
            &[255, 0, 0, 255]
        );

        let kind = ComponentKind::Canvas {
            width: 8,
            height: 8,
        };
        let style = Style::default();
        let nodes = [(
            7,
            LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 8.0,
                height: 8.0,
            },
            &kind,
            &style,
        )];
        let font = test_font();
        let mut rasterizer = SkiaRasterizer::new(TEST_FONT).unwrap();
        let plain = rasterizer
            .render_frame(
                8,
                8,
                &nodes,
                &font,
                &[],
                &HashMap::new(),
                None,
                w3cos_std::color::Color::WHITE,
                None,
                None,
                1.0,
            )
            .unwrap();
        let center = (4 * 8 + 4) * 4;
        assert_eq!(&plain[center..center + 4], &[255, 0, 0, 255]);

        let mut filtered_style = style.clone();
        filtered_style.filter = Some("invert(1)".into());
        let filtered_nodes = [(
            7,
            LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 8.0,
                height: 8.0,
            },
            &kind,
            &filtered_style,
        )];
        let pixels = rasterizer
            .render_frame(
                8,
                8,
                &filtered_nodes,
                &font,
                &[],
                &HashMap::new(),
                None,
                w3cos_std::color::Color::WHITE,
                None,
                None,
                1.0,
            )
            .unwrap();
        let pixel = &pixels[center..center + 4];
        assert!(pixel[0] < 8, "red should be inverted: {pixel:?}");
        assert!(pixel[1] > 247, "green should be inverted: {pixel:?}");
        assert!(pixel[2] > 247, "blue should be inverted: {pixel:?}");
        assert_eq!(pixel[3], 255);
        crate::canvas2d::remove_surface(7);
    }

    #[test]
    fn replay_applies_ancestor_effect_to_the_whole_subtree() {
        use crate::paint_artifact::PaintNode;

        let mut parent_style = Style::default();
        parent_style.filter = Some("invert(1)".into());
        let mut red_style = Style::default();
        red_style.background = w3cos_std::color::Color::rgb(255, 0, 0);
        let mut blue_style = Style::default();
        blue_style.background = w3cos_std::color::Color::rgb(0, 0, 255);
        let parent_kind = ComponentKind::Box;
        let red_kind = ComponentKind::Box;
        let blue_kind = ComponentKind::Box;
        let rects = [
            LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 8.0,
                height: 4.0,
            },
            LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 4.0,
                height: 4.0,
            },
            LayoutRect {
                x: 4.0,
                y: 0.0,
                width: 4.0,
                height: 4.0,
            },
        ];
        let artifact = PaintArtifact::build(
            [
                PaintNode {
                    kind: parent_kind.clone(),
                    style: parent_style.clone(),
                    parent: None,
                    sticky_counter_signal: None,
                },
                PaintNode {
                    kind: red_kind.clone(),
                    style: red_style.clone(),
                    parent: Some(0),
                    sticky_counter_signal: None,
                },
                PaintNode {
                    kind: blue_kind.clone(),
                    style: blue_style.clone(),
                    parent: Some(0),
                    sticky_counter_signal: None,
                },
            ],
            &[(rects[0], 0), (rects[1], 1), (rects[2], 2)],
            1,
        );
        let nodes = [
            (0, rects[0], &parent_kind, &parent_style),
            (1, rects[1], &red_kind, &red_style),
            (2, rects[2], &blue_kind, &blue_style),
        ];
        let font = test_font();
        let mut rasterizer = SkiaRasterizer::new(TEST_FONT).unwrap();
        let pixels = rasterizer
            .render_frame(
                8,
                4,
                &nodes,
                &font,
                &[],
                &HashMap::new(),
                None,
                w3cos_std::color::Color::WHITE,
                Some(&artifact),
                None,
                1.0,
            )
            .unwrap();
        let left = &pixels[(2 * 8 + 2) * 4..(2 * 8 + 2) * 4 + 4];
        let right = &pixels[(2 * 8 + 6) * 4..(2 * 8 + 6) * 4 + 4];
        assert_eq!(left, &[0, 255, 255, 255]);
        assert_eq!(right, &[255, 255, 0, 255]);
    }

    #[test]
    fn empty_source_image_paints_browser_missing_resource_frame() {
        let mut surface=skia_safe::surfaces::raster_n32_premul((32,24)).unwrap();
        surface.canvas().clear(skia_safe::Color::WHITE);
        draw_image(surface.canvas(),LayoutRect {x:4.0,y:4.0,width:24.0,height:16.0},"",1.0);
        let mut pixels=vec![0u8;32*24*4];
        let info=skia_safe::ImageInfo::new((32,24),skia_safe::ColorType::RGBA8888,skia_safe::AlphaType::Premul,None);
        assert!(surface.read_pixels(&info,&mut pixels,32*4,(0,0)));
        assert_eq!(pixels.chunks_exact(4).filter(|p|*p==[192,192,192,255]).count(),24*16-22*14);
        assert_eq!(&pixels[(8*32+8)*4..(8*32+8)*4+4],&[255,255,255,255]);
    }

    #[test]
    fn replay_decodes_and_draws_image_resources() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let image = image::RgbaImage::from_pixel(2, 2, image::Rgba([10, 20, 240, 255]));
        image
            .save_with_format(file.path(), image::ImageFormat::Png)
            .unwrap();

        let kind = ComponentKind::Image {
            src: file.path().to_string_lossy().into_owned(),
        };
        let style = Style::default();
        let nodes = [(
            3,
            LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 2.0,
                height: 2.0,
            },
            &kind,
            &style,
        )];
        let font = test_font();
        let mut rasterizer = SkiaRasterizer::new(TEST_FONT).unwrap();
        let pixels = rasterizer
            .render_frame(
                2,
                2,
                &nodes,
                &font,
                &[],
                &HashMap::new(),
                None,
                w3cos_std::color::Color::WHITE,
                None,
                None,
                1.0,
            )
            .unwrap();
        assert!((pixels[0] as i16 - 10).abs() <= 2);
        assert!((pixels[1] as i16 - 20).abs() <= 2);
        assert!(pixels[2] >= 238);
        assert_eq!(pixels[3], 255);
    }

    #[test]
    fn image_paints_inside_its_content_box() {
        let file = tempfile::NamedTempFile::new().unwrap();
        image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 255, 255]))
            .save_with_format(file.path(), image::ImageFormat::Png)
            .unwrap();

        let kind = ComponentKind::Image {
            src: file.path().to_string_lossy().into_owned(),
        };
        let style = Style {
            padding: w3cos_std::style::Edges {
                left: w3cos_std::style::Spacing::Px(2.0),
                ..w3cos_std::style::Edges::ZERO
            },
            ..Style::default()
        };
        let nodes = [(
            3,
            LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 3.0,
                height: 1.0,
            },
            &kind,
            &style,
        )];
        let font = test_font();
        let mut rasterizer = SkiaRasterizer::new(TEST_FONT).unwrap();
        let pixels = rasterizer
            .render_frame(
                3,
                1,
                &nodes,
                &font,
                &[],
                &HashMap::new(),
                None,
                w3cos_std::color::Color::WHITE,
                None,
                None,
                1.0,
            )
            .unwrap();

        assert_eq!(&pixels[0..4], &[255, 255, 255, 255]);
        assert_eq!(&pixels[4..8], &[255, 255, 255, 255]);
        assert_eq!(&pixels[8..12], &[0, 0, 255, 255]);
    }

    #[test]
    fn cached_image_premultiplies_without_mutating_decoded_pixels() {
        crate::image_loader::clear_cache();
        let image = image::RgbaImage::from_pixel(1, 1, image::Rgba([128, 64, 0, 128]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        let decoded = crate::image_loader::decode_and_install("premul-cache.png", &bytes.into_inner()).unwrap();
        let texture = super::cached_skia_image(&decoded).unwrap();
        assert_eq!(texture.alpha_type(), AlphaType::Premul);
        assert_eq!(decoded.data.as_slice(), &[128, 64, 0, 128]);
        let mut surface = Surface::new_raster_n32_premul((1, 1)).unwrap();
        surface.canvas().clear(Color::TRANSPARENT);
        draw_decoded_image(surface.canvas(), LayoutRect { x: 0.0, y: 0.0, width: 1.0, height: 1.0 }, &decoded, 1.0, false, (false, false));
        let info = ImageInfo::new((1, 1), ColorType::RGBA8888, AlphaType::Premul, None);
        let mut pixel = [0_u8; 4];
        assert!(surface.read_pixels(&info, &mut pixel, 4, (0, 0)));
        assert_eq!(pixel, [64, 32, 0, 128]);
        crate::image_loader::clear_cache();
    }

    #[test]
    fn skia_image_is_reused_for_unchanged_decoded_pixels() {
        crate::image_loader::clear_cache();
        crate::image_loader::reset_cache_stats();
        let image = image::RgbaImage::from_pixel(2, 1, image::Rgba([12, 34, 56, 255]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        let decoded =
            crate::image_loader::decode_and_install("skia-cache.png", &bytes.into_inner()).unwrap();
        let first = super::cached_skia_image(&decoded).expect("skia image");
        let second = super::cached_skia_image(&decoded).expect("skia image");
        assert_eq!(first.unique_id(), second.unique_id());
        assert_eq!(skia_image_upload_count(), 1);
        assert_eq!(skia_image_reuse_count(), 1);
        crate::image_loader::clear_cache();
        assert!(super::SKIA_IMAGES.with(|cache| cache.borrow().is_empty()));
    }

    #[test]
    fn canvas_snapshot_skia_image_reused_until_dirtied() {
        crate::image_loader::reset_cache_stats();
        super::clear_image_texture_cache();
        crate::canvas2d::remove_surface(11);

        let mut context = crate::canvas2d::CanvasRenderingContext2D::new(4, 4);
        context.set_fill_style("#00ff00");
        context.fill_rect(0.0, 0.0, 4.0, 4.0);
        context.publish_to_surface(11);

        let kind = ComponentKind::Canvas {
            width: 4,
            height: 4,
        };
        let style = Style::default();
        let nodes = [(
            11,
            LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 4.0,
                height: 4.0,
            },
            &kind,
            &style,
        )];
        let font = test_font();
        let mut rasterizer = SkiaRasterizer::new(TEST_FONT).unwrap();

        let _ = rasterizer
            .render_frame(
                4,
                4,
                &nodes,
                &font,
                &[],
                &HashMap::new(),
                None,
                w3cos_std::color::Color::WHITE,
                None,
                None,
                1.0,
            )
            .unwrap();
        assert_eq!(skia_image_upload_count(), 1);
        assert_eq!(skia_image_reuse_count(), 0);

        // Unchanged canvas: republish must keep Arc identity and skip upload.
        context.publish_to_surface(11);
        let _ = rasterizer
            .render_frame(
                4,
                4,
                &nodes,
                &font,
                &[],
                &HashMap::new(),
                None,
                w3cos_std::color::Color::WHITE,
                None,
                None,
                1.0,
            )
            .unwrap();
        assert_eq!(skia_image_upload_count(), 1);
        assert_eq!(skia_image_reuse_count(), 1);

        context.fill_rect(0.0, 0.0, 1.0, 1.0);
        context.publish_to_surface(11);
        let _ = rasterizer
            .render_frame(
                4,
                4,
                &nodes,
                &font,
                &[],
                &HashMap::new(),
                None,
                w3cos_std::color::Color::WHITE,
                None,
                None,
                1.0,
            )
            .unwrap();
        assert_eq!(skia_image_upload_count(), 2);
        assert_eq!(skia_image_reuse_count(), 1);

        crate::canvas2d::remove_surface(11);
        super::clear_image_texture_cache();
    }
}
