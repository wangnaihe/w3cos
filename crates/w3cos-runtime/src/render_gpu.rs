use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use skrifa::MetadataProvider;
use vello::kurbo::{Affine, Rect, RoundedRect, Stroke};
use vello::peniko::{
    Blob, Color, Fill, FontData, ImageAlphaType, ImageBrush, ImageData, ImageFormat,
};
use vello::{Glyph, Scene};
use w3cos_std::color::Color as AppColor;
use w3cos_std::component::ComponentKind;
use w3cos_std::style::{Style, TextAlign};

use crate::compositor::{layer_opacity, promotes_compositor_layer};
use crate::filter::{self, CssFilter};
#[cfg(feature = "gpu")]
use crate::gpu_filter::{self, GpuFilterCtx};

use crate::layout::LayoutRect;

// ---------------------------------------------------------------------------
// GlyphCache — avoid repeated font parsing, charmap lookup, and rasterization
// ---------------------------------------------------------------------------

pub struct GlyphCache {
    entries: HashMap<(char, u32), GlyphEntry>,
    display_chunks: HashMap<u64, CachedDisplayChunk>,
    display_chunk_bytes: usize,
    display_chunk_clock: u64,
}

struct CachedDisplayChunk {
    scene: Scene,
    estimated_bytes: usize,
    last_used: u64,
}

#[cfg(any(target_os = "ios", target_os = "android"))]
const DISPLAY_CHUNK_BUDGET_BYTES: usize = 24 * 1024 * 1024;

#[cfg(not(any(target_os = "ios", target_os = "android")))]
const DISPLAY_CHUNK_BUDGET_BYTES: usize = 64 * 1024 * 1024;

pub struct DisplayChunkPrepaintRequest {
    pub kind: ComponentKind,
    pub style: Style,
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Copy)]
struct GlyphEntry {
    glyph_id: Option<u32>,
    advance: f32,
}

impl GlyphCache {
    pub fn new() -> Self {
        Self {
            entries: HashMap::with_capacity(256),
            display_chunks: HashMap::with_capacity(256),
            display_chunk_bytes: 0,
            display_chunk_clock: 0,
        }
    }

    fn quantize(font_size: f32) -> u32 {
        (font_size * 4.0).round() as u32
    }

    fn lookup_or_insert(
        &mut self,
        ch: char,
        font_size: f32,
        charmap: &skrifa::charmap::Charmap,
        glyph_metrics: &skrifa::metrics::GlyphMetrics,
        fontdue_font: &fontdue::Font,
    ) -> GlyphEntry {
        let key = (ch, Self::quantize(font_size));
        *self.entries.entry(key).or_insert_with(|| {
            if let Some(glyph_id) = charmap.map(ch) {
                let advance = glyph_metrics
                    .advance_width(glyph_id)
                    .unwrap_or_else(|| fontdue_font.metrics(ch, font_size).advance_width);
                GlyphEntry {
                    glyph_id: Some(glyph_id.to_u32()),
                    advance,
                }
            } else {
                GlyphEntry {
                    glyph_id: None,
                    advance: fontdue_font.metrics(ch, font_size).advance_width,
                }
            }
        })
    }

    pub fn prepaint_interest_rect(
        &mut self,
        requests: &[crate::text_layout::TextPrepaintRequest],
        font_data: &FontData,
        fontdue_font: &fontdue::Font,
        budget: Duration,
    ) -> usize {
        let Ok(font_ref) = skrifa::FontRef::from_index(font_data.data.as_ref().as_ref(), 0) else {
            return 0;
        };
        let charmap = font_ref.charmap();
        let started = Instant::now();
        let mut prepared = 0;
        for request in requests {
            if prepared > 0 && started.elapsed() >= budget {
                break;
            }
            let layout = crate::text_layout::retained_text_paint_layout(
                &request.text,
                request.width.max(1.0),
                request.font_size,
                fontdue_font,
                request.white_space,
            );
            let glyph_metrics = font_ref.glyph_metrics(
                skrifa::instance::Size::new(request.font_size),
                skrifa::instance::LocationRef::default(),
            );
            for line in &layout.lines {
                for ch in line.chars() {
                    self.lookup_or_insert(
                        ch,
                        request.font_size,
                        &charmap,
                        &glyph_metrics,
                        fontdue_font,
                    );
                }
            }
            prepared += 1;
        }
        prepared
    }

    pub fn prepaint_display_chunks(
        &mut self,
        requests: &[DisplayChunkPrepaintRequest],
        font_data: &FontData,
        font: &fontdue::Font,
        budget: Duration,
    ) -> usize {
        let started = Instant::now();
        let mut prepared = 0;
        for request in requests {
            if prepared > 0 && started.elapsed() >= budget {
                break;
            }
            let _ = self.ensure_display_chunk(
                &request.kind,
                &request.style,
                request.width,
                request.height,
                font_data,
                font,
            );
            prepared += 1;
        }
        prepared
    }

    fn ensure_display_chunk(
        &mut self,
        kind: &ComponentKind,
        style: &Style,
        width: f32,
        height: f32,
        font_data: &FontData,
        font: &fontdue::Font,
    ) -> Option<u64> {
        let key = display_chunk_key(kind, style, width, height)?;
        self.display_chunk_clock = self.display_chunk_clock.wrapping_add(1);
        if let Some(chunk) = self.display_chunks.get_mut(&key) {
            chunk.last_used = self.display_chunk_clock;
            return Some(key);
        }
        let mut chunk = Scene::new();
        render_node(
            &mut chunk,
            LayoutRect {
                x: 0.0,
                y: 0.0,
                width,
                height,
            },
            kind,
            style,
            font_data,
            font,
            None,
            None,
            false,
            self,
            Affine::IDENTITY,
            true,
            #[cfg(feature = "gpu")]
            None,
        );
        let estimated_bytes = (width.max(1.0).ceil() as usize)
            .saturating_mul(height.max(1.0).ceil() as usize)
            .saturating_mul(4)
            .max(256);
        self.evict_display_chunks(estimated_bytes);
        self.display_chunk_bytes = self.display_chunk_bytes.saturating_add(estimated_bytes);
        self.display_chunks.insert(
            key,
            CachedDisplayChunk {
                scene: chunk,
                estimated_bytes,
                last_used: self.display_chunk_clock,
            },
        );
        Some(key)
    }

    fn evict_display_chunks(&mut self, incoming_bytes: usize) {
        while !self.display_chunks.is_empty()
            && self.display_chunk_bytes.saturating_add(incoming_bytes) > DISPLAY_CHUNK_BUDGET_BYTES
        {
            let Some(victim) = self
                .display_chunks
                .iter()
                .min_by_key(|(_, chunk)| chunk.last_used)
                .map(|(key, _)| *key)
            else {
                break;
            };
            if let Some(removed) = self.display_chunks.remove(&victim) {
                self.display_chunk_bytes = self
                    .display_chunk_bytes
                    .saturating_sub(removed.estimated_bytes);
            }
        }
    }
}

fn display_chunk_key(kind: &ComponentKind, style: &Style, width: f32, height: f32) -> Option<u64> {
    if !matches!(
        kind,
        ComponentKind::Text { .. } | ComponentKind::Button { .. }
    ) || style.filter.is_some()
        || style.box_shadow.is_some()
        || style.transform != w3cos_std::style::Transform2D::IDENTITY
    {
        return None;
    }
    let mut hasher = DefaultHasher::new();
    serde_json::to_vec(kind).ok()?.hash(&mut hasher);
    serde_json::to_vec(style).ok()?.hash(&mut hasher);
    width.to_bits().hash(&mut hasher);
    height.to_bits().hash(&mut hasher);
    Some(hasher.finish())
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn color_to_vello(c: AppColor) -> Color {
    Color::new([
        c.r as f32 / 255.0,
        c.g as f32 / 255.0,
        c.b as f32 / 255.0,
        c.a as f32 / 255.0,
    ])
}

fn resolved_color(c: AppColor, opacity: f32, chain: Option<&CssFilter>) -> AppColor {
    let c = chain
        .map(|f| filter::apply_filter_to_color(c, f))
        .unwrap_or(c);
    AppColor::rgba(
        c.r,
        c.g,
        c.b,
        (c.a as f32 * opacity).clamp(0.0, 255.0) as u8,
    )
}

fn node_color(c: AppColor, opacity: f32, chain: Option<&CssFilter>) -> AppColor {
    resolved_color(c, opacity, chain)
}

#[cfg(feature = "gpu")]
#[allow(clippy::too_many_arguments)]
fn render_node_gpu_layer(
    scene: &mut Scene,
    filter_ctx: &mut GpuFilterCtx<'_>,
    rect: LayoutRect,
    kind: &ComponentKind,
    style: &Style,
    font_data: &FontData,
    font: &fontdue::Font,
    text_input_value: Option<&str>,
    is_focused: bool,
    glyph_cache: &mut GlyphCache,
    dpi: Affine,
    chain: &CssFilter,
) {
    let pad = chain.max_blur_px().ceil() as u32 + 2;
    let lw = (rect.width as u32 + pad * 2).max(1);
    let lh = (rect.height as u32 + pad * 2).max(1);
    let mut layer_scene = Scene::new();
    let inner = LayoutRect {
        x: pad as f32,
        y: pad as f32,
        width: rect.width,
        height: rect.height,
    };
    render_node(
        &mut layer_scene,
        inner,
        kind,
        style,
        font_data,
        font,
        None,
        text_input_value,
        is_focused,
        glyph_cache,
        dpi,
        true,
        None,
    );
    if let Some(layer) = filter_ctx.rasterize_filtered_layer(&layer_scene, lw, lh, chain) {
        gpu_filter::draw_filtered_image(
            scene,
            rect.x - pad as f32,
            rect.y - pad as f32,
            &layer,
            dpi,
        );
    }
}

pub fn make_font_data(font_bytes: &'static [u8]) -> FontData {
    let blob = Blob::new(Arc::new(font_bytes.to_vec()));
    FontData::new(blob, 0)
}

#[allow(clippy::too_many_arguments)]
pub fn render_frame(
    scene: &mut Scene,
    width: u32,
    height: u32,
    nodes: &[(usize, LayoutRect, &ComponentKind, &Style)],
    font_data: &FontData,
    font: &fontdue::Font,
    scroll_info: &[Option<(f32, f32, LayoutRect)>],
    text_input_values: &HashMap<usize, String>,
    focused_index: Option<usize>,
    glyph_cache: &mut GlyphCache,
    scale_factor: f32,
    #[cfg(feature = "gpu")] mut gpu_filter: Option<&mut GpuFilterCtx<'_>>,
) {
    let vw = width as f32 / scale_factor;
    let vh = height as f32 / scale_factor;

    let dpi = Affine::scale(scale_factor as f64);

    for &(idx, rect, kind, style) in nodes {
        let (offset_rect, clip) = match scroll_info.get(idx) {
            Some(Some((sx, sy, clip_rect))) => {
                let offset_rect = LayoutRect {
                    x: rect.x - sx,
                    y: rect.y - sy,
                    width: rect.width,
                    height: rect.height,
                };
                (offset_rect, Some(*clip_rect))
            }
            _ => (rect, None),
        };

        // Viewport culling: skip nodes entirely outside the visible area
        if offset_rect.x + offset_rect.width < 0.0
            || offset_rect.y + offset_rect.height < 0.0
            || offset_rect.x > vw
            || offset_rect.y > vh
        {
            continue;
        }

        let text_value = match kind {
            ComponentKind::TextInput { value, .. } => Some(
                text_input_values
                    .get(&idx)
                    .map(|s| s.as_str())
                    .unwrap_or_else(|| value.as_str()),
            ),
            _ => None,
        };
        let is_focused = focused_index == Some(idx);
        if !is_focused
            && let Some(key) = glyph_cache.ensure_display_chunk(
                kind,
                style,
                offset_rect.width,
                offset_rect.height,
                font_data,
                font,
            )
            && let Some(chunk) = glyph_cache.display_chunks.get(&key)
        {
            if let Some(clip_rect) = clip {
                let clip_shape = Rect::new(
                    clip_rect.x as f64,
                    clip_rect.y as f64,
                    (clip_rect.x + clip_rect.width) as f64,
                    (clip_rect.y + clip_rect.height) as f64,
                );
                scene.push_clip_layer(Fill::NonZero, dpi, &clip_shape);
            }
            scene.append(
                &chunk.scene,
                Some(dpi * Affine::translate((offset_rect.x as f64, offset_rect.y as f64))),
            );
            if clip.is_some() {
                scene.pop_layer();
            }
            continue;
        }
        render_node(
            scene,
            offset_rect,
            kind,
            style,
            font_data,
            font,
            clip,
            text_value,
            is_focused,
            glyph_cache,
            dpi,
            false,
            #[cfg(feature = "gpu")]
            gpu_filter.as_deref_mut(),
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn render_node(
    scene: &mut Scene,
    rect: LayoutRect,
    kind: &ComponentKind,
    style: &Style,
    font_data: &FontData,
    font: &fontdue::Font,
    clip_rect: Option<LayoutRect>,
    text_input_value: Option<&str>,
    is_focused: bool,
    glyph_cache: &mut GlyphCache,
    dpi: Affine,
    in_layer: bool,
    #[cfg(feature = "gpu")] mut gpu_filter: Option<&mut GpuFilterCtx<'_>>,
) {
    if style.opacity <= 0.0 {
        return;
    }

    let has_clip = clip_rect.is_some();
    if let Some(cr) = clip_rect {
        let clip_shape = Rect::new(
            cr.x as f64,
            cr.y as f64,
            (cr.x + cr.width) as f64,
            (cr.y + cr.height) as f64,
        );
        scene.push_clip_layer(Fill::NonZero, dpi, &clip_shape);
    }

    let tx = style.transform.translate_x;
    let ty = style.transform.translate_y;
    let rect = LayoutRect {
        x: rect.x + tx,
        y: rect.y + ty,
        width: rect.width * style.transform.scale_x,
        height: rect.height * style.transform.scale_y,
    };

    let opacity = style.opacity;
    let css_filter = style.filter.as_deref().and_then(filter::parse_css_filter);

    let needs_compositor_layer = promotes_compositor_layer(style);
    if needs_compositor_layer {
        let bounds = Rect::new(
            rect.x as f64,
            rect.y as f64,
            (rect.x + rect.width) as f64,
            (rect.y + rect.height) as f64,
        );
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::Mix::Normal,
            layer_opacity(style),
            dpi,
            &bounds,
        );
    }

    #[cfg(feature = "gpu")]
    if !in_layer {
        if let (Some(ref chain), Some(ctx)) = (css_filter.as_ref(), gpu_filter.as_deref_mut()) {
            if chain.has_blur() {
                if let Some(shadow) = chain.drop_shadow() {
                    draw_box_shadow(scene, rect, shadow, style.border_radius, dpi);
                }
                if let Some(ref shadow) = style.box_shadow {
                    draw_box_shadow(scene, rect, shadow, style.border_radius, dpi);
                }
                render_node_gpu_layer(
                    scene,
                    ctx,
                    rect,
                    kind,
                    style,
                    font_data,
                    font,
                    text_input_value,
                    is_focused,
                    glyph_cache,
                    dpi,
                    chain,
                );
                if needs_compositor_layer {
                    scene.pop_layer();
                }
                if has_clip {
                    scene.pop_layer();
                }
                return;
            }
        }
    }

    if !in_layer {
        if let Some(ref chain) = css_filter {
            if let Some(shadow) = chain.drop_shadow() {
                draw_box_shadow(scene, rect, shadow, style.border_radius, dpi);
            }
        }
        if let Some(ref shadow) = style.box_shadow {
            draw_box_shadow(scene, rect, shadow, style.border_radius, dpi);
        }
    }

    let color_chain = if in_layer { None } else { css_filter.as_ref() };
    let bg = node_color(style.background, opacity, color_chain);

    if bg.a > 0 {
        draw_rect(scene, rect, bg, style.border_radius, dpi);
    }

    if style.border_width > 0.0 && style.border_color.a > 0 {
        let border = node_color(style.border_color, opacity, color_chain);
        draw_border(
            scene,
            rect,
            border,
            style.border_width,
            style.border_radius,
            dpi,
        );
    }

    let text_color = node_color(style.color, opacity, color_chain);

    match kind {
        ComponentKind::Text { content } => {
            draw_text_in_rect(
                scene,
                rect,
                content,
                style,
                text_color,
                font_data,
                font,
                glyph_cache,
                dpi,
            );
        }
        ComponentKind::Button { label } => {
            draw_text_centered_in_rect(
                scene,
                rect,
                label,
                style.font_size,
                text_color,
                font_data,
                font,
                glyph_cache,
                dpi,
            );
        }
        ComponentKind::Image { src } => {
            if let Some(decoded) = crate::image_loader::get_or_load(src) {
                let blob = Blob::new(decoded.data.clone() as Arc<dyn AsRef<[u8]> + Send + Sync>);
                let image_data = ImageData {
                    data: blob,
                    format: ImageFormat::Rgba8,
                    alpha_type: ImageAlphaType::Alpha,
                    width: decoded.width,
                    height: decoded.height,
                };
                let image_brush = ImageBrush::new(image_data);
                let scale_x = rect.width as f64 / decoded.width as f64;
                let scale_y = rect.height as f64 / decoded.height as f64;
                let transform = Affine::translate((rect.x as f64, rect.y as f64))
                    * Affine::scale_non_uniform(scale_x, scale_y);
                scene.draw_image(image_brush.as_ref(), transform);
            } else {
                let placeholder_bg = if bg.a == 0 {
                    AppColor::rgb(40, 40, 50)
                } else {
                    bg
                };
                draw_rect(scene, rect, placeholder_bg, style.border_radius, dpi);
                let border_color = if style.border_width > 0.0 && style.border_color.a > 0 {
                    style.border_color
                } else {
                    AppColor::rgb(100, 100, 120)
                };
                draw_border(
                    scene,
                    rect,
                    border_color,
                    style.border_width.max(1.0),
                    style.border_radius,
                    dpi,
                );
                let label = format!("[Image: {}]", src);
                draw_text(
                    scene,
                    rect.x + 8.0,
                    rect.y + 8.0,
                    &label,
                    style.font_size,
                    text_color,
                    font_data,
                    font,
                    glyph_cache,
                    dpi,
                );
            }
        }
        ComponentKind::TextInput {
            value,
            placeholder,
            secure,
        } => {
            let display_value = text_input_value.unwrap_or(value.as_str());
            let masked_value = secure.then(|| "•".repeat(display_value.chars().count()));
            let (display_text, text_color_final) = if display_value.is_empty() {
                (placeholder.as_str(), AppColor::rgb(107, 114, 128))
            } else if let Some(masked) = masked_value.as_deref() {
                (masked, text_color)
            } else {
                (display_value, text_color)
            };
            let border = style.border_width;
            let padding = style.padding_lengths();
            let content = LayoutRect {
                x: rect.x + border + padding.left,
                y: rect.y + border + padding.top,
                width: (rect.width - border * 2.0 - padding.left - padding.right).max(1.0),
                height: (rect.height - border * 2.0 - padding.top - padding.bottom).max(0.0),
            };
            let text_x = content.x;
            let text_y = crate::text_layout::y_for_draw_text_line_centered(
                display_text,
                style.font_size,
                font,
                content.y,
                content.height,
            );
            draw_text(
                scene,
                text_x,
                text_y,
                display_text,
                style.font_size,
                text_color_final,
                font_data,
                font,
                glyph_cache,
                dpi,
            );
            if is_focused {
                draw_blinking_cursor(
                    scene,
                    content,
                    display_value,
                    style.font_size,
                    text_color,
                    font_data,
                    font,
                    glyph_cache,
                    dpi,
                );
            }
        }
        _ => {}
    }

    if needs_compositor_layer {
        scene.pop_layer();
    }

    if has_clip {
        scene.pop_layer();
    }
}

fn draw_box_shadow(
    scene: &mut Scene,
    rect: LayoutRect,
    shadow: &w3cos_std::style::BoxShadow,
    radius: f32,
    dpi: Affine,
) {
    let spread = shadow.spread_radius;
    let shadow_rect = Rect::new(
        (rect.x + shadow.offset_x - spread) as f64,
        (rect.y + shadow.offset_y - spread) as f64,
        (rect.x + shadow.offset_x - spread + rect.width + spread * 2.0) as f64,
        (rect.y + shadow.offset_y - spread + rect.height + spread * 2.0) as f64,
    );
    let color = color_to_vello(shadow.color);
    let r = (radius + spread) as f64;
    let std_dev = (shadow.blur_radius / 2.0) as f64;
    scene.draw_blurred_rounded_rect(dpi, shadow_rect, color, r, std_dev);
}

fn draw_rect(scene: &mut Scene, r: LayoutRect, color: AppColor, radius: f32, dpi: Affine) {
    let vc = color_to_vello(color);
    if radius > 0.0 {
        let rr = RoundedRect::new(
            r.x as f64,
            r.y as f64,
            (r.x + r.width) as f64,
            (r.y + r.height) as f64,
            radius as f64,
        );
        scene.fill(Fill::NonZero, dpi, vc, None, &rr);
    } else {
        let rect = Rect::new(
            r.x as f64,
            r.y as f64,
            (r.x + r.width) as f64,
            (r.y + r.height) as f64,
        );
        scene.fill(Fill::NonZero, dpi, vc, None, &rect);
    }
}

fn draw_border(
    scene: &mut Scene,
    r: LayoutRect,
    color: AppColor,
    width: f32,
    radius: f32,
    dpi: Affine,
) {
    let vc = color_to_vello(color);
    let stroke = Stroke::new(width as f64);
    let half = width as f64 / 2.0;
    if radius > 0.0 {
        let rr = RoundedRect::new(
            r.x as f64 + half,
            r.y as f64 + half,
            (r.x + r.width) as f64 - half,
            (r.y + r.height) as f64 - half,
            radius as f64,
        );
        scene.stroke(&stroke, dpi, vc, None, &rr);
    } else {
        let rect = Rect::new(
            r.x as f64 + half,
            r.y as f64 + half,
            (r.x + r.width) as f64 - half,
            (r.y + r.height) as f64 - half,
        );
        scene.stroke(&stroke, dpi, vc, None, &rect);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_text_centered_in_rect(
    scene: &mut Scene,
    rect: LayoutRect,
    text: &str,
    font_size: f32,
    color: AppColor,
    font_data: &FontData,
    fontdue_font: &fontdue::Font,
    glyph_cache: &mut GlyphCache,
    dpi: Affine,
) {
    let text_w: f32 = text
        .chars()
        .map(|ch| fontdue_font.metrics(ch, font_size).advance_width)
        .sum();
    let text_h = font_size * 1.2;
    let x = rect.x + (rect.width - text_w).max(0.0) * 0.5;
    let y = rect.y + (rect.height - text_h).max(0.0) * 0.5;
    draw_text(
        scene,
        x,
        y,
        text,
        font_size,
        color,
        font_data,
        fontdue_font,
        glyph_cache,
        dpi,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_text(
    scene: &mut Scene,
    x: f32,
    y: f32,
    text: &str,
    font_size: f32,
    color: AppColor,
    font_data: &FontData,
    fontdue_font: &fontdue::Font,
    glyph_cache: &mut GlyphCache,
    dpi: Affine,
) {
    if text.is_empty() {
        return;
    }

    let vc = color_to_vello(color);
    let font_ref = skrifa::FontRef::from_index(font_data.data.as_ref().as_ref(), 0);
    let font_ref = match font_ref {
        Ok(f) => f,
        Err(_) => return,
    };
    let charmap = font_ref.charmap();
    let glyph_metrics = font_ref.glyph_metrics(
        skrifa::instance::Size::new(font_size),
        skrifa::instance::LocationRef::default(),
    );

    let baseline_y = y + font_size;
    let mut cursor_x = x;
    let mut glyphs = Vec::new();

    for ch in text.chars() {
        let entry =
            glyph_cache.lookup_or_insert(ch, font_size, &charmap, &glyph_metrics, fontdue_font);
        if let Some(gid) = entry.glyph_id {
            glyphs.push(Glyph {
                id: gid,
                x: cursor_x,
                y: baseline_y,
            });
        }
        cursor_x += entry.advance;
    }

    if !glyphs.is_empty() {
        scene
            .draw_glyphs(font_data)
            .font_size(font_size)
            .transform(dpi)
            .brush(vc)
            .draw(Fill::NonZero, glyphs.into_iter());
    }
}

fn text_content_box(rect: LayoutRect, style: &Style) -> LayoutRect {
    let border = style.border_width;
    let pad = style.padding_lengths();
    LayoutRect {
        x: rect.x + pad.left + border,
        y: rect.y + pad.top + border,
        width: (rect.width - pad.left - pad.right - border * 2.0).max(1.0),
        height: (rect.height - pad.top - pad.bottom - border * 2.0).max(0.0),
    }
}

fn text_paint_box(rect: LayoutRect, style: &Style) -> LayoutRect {
    if style.background.a > 0 {
        let border = style.border_width;
        LayoutRect {
            x: rect.x + border,
            y: rect.y + border,
            width: (rect.width - border * 2.0).max(1.0),
            height: (rect.height - border * 2.0).max(0.0),
        }
    } else {
        text_content_box(rect, style)
    }
}

fn single_line_h_align(style: &Style, box_w: f32, ink_w: f32) -> TextAlign {
    match style.text_align {
        TextAlign::Center | TextAlign::Right => style.text_align,
        TextAlign::Left if style.background.a > 0 => TextAlign::Center,
        TextAlign::Left
            if matches!(
                style.white_space,
                w3cos_std::style::WhiteSpace::NoWrap | w3cos_std::style::WhiteSpace::Pre
            ) && box_w > ink_w + 1.5 =>
        {
            TextAlign::Center
        }
        TextAlign::Left | TextAlign::Justify => TextAlign::Left,
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_text_in_rect(
    scene: &mut Scene,
    rect: LayoutRect,
    text: &str,
    style: &Style,
    color: AppColor,
    font_data: &FontData,
    font: &fontdue::Font,
    glyph_cache: &mut GlyphCache,
    dpi: Affine,
) {
    let content = text_paint_box(rect, style);
    let line_h = style.font_size * style.line_height;
    let layout = crate::text_layout::retained_text_paint_layout(
        text,
        content.width,
        style.font_size,
        font,
        style.white_space,
    );
    let lines = &layout.lines;
    let block_h = if lines.len() == 1 {
        crate::text_layout::single_line_content_height(
            &lines[0],
            style.font_size,
            style.line_height,
            font,
        )
    } else {
        lines.len() as f32 * line_h
    };
    let block_top = content.y + (content.height - block_h).max(0.0) * 0.5;

    for (index, line) in lines.iter().enumerate() {
        let ink = layout.ink_bounds[index];
        let align = single_line_h_align(style, content.width, ink.width);
        let x = match align {
            TextAlign::Right => content.x + content.width - ink.width - ink.left,
            TextAlign::Center => content.x + (content.width - ink.width) * 0.5 - ink.left,
            TextAlign::Left | TextAlign::Justify => content.x - ink.left,
        };
        let y = if lines.len() == 1 {
            content.y + (content.height - ink.height).max(0.0) * 0.5 - ink.top
        } else {
            block_top + index as f32 * line_h
        };
        draw_text(
            scene,
            x,
            y,
            line,
            style.font_size,
            color,
            font_data,
            font,
            glyph_cache,
            dpi,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_blinking_cursor(
    scene: &mut Scene,
    content: LayoutRect,
    text: &str,
    font_size: f32,
    color: AppColor,
    font_data: &FontData,
    fontdue_font: &fontdue::Font,
    glyph_cache: &mut GlyphCache,
    dpi: Affine,
) {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    if (ms / 500) % 2 == 0 {
        return;
    }

    let font_ref = skrifa::FontRef::from_index(font_data.data.as_ref().as_ref(), 0);
    let font_ref = match font_ref {
        Ok(f) => f,
        Err(_) => return,
    };
    let charmap = font_ref.charmap();
    let glyph_metrics = font_ref.glyph_metrics(
        skrifa::instance::Size::new(font_size),
        skrifa::instance::LocationRef::default(),
    );

    let mut cursor_x = content.x;
    for ch in text.chars() {
        let entry =
            glyph_cache.lookup_or_insert(ch, font_size, &charmap, &glyph_metrics, fontdue_font);
        cursor_x += entry.advance;
    }

    let cursor_w = 2.0f32.max(font_size * 0.1);
    let cursor_y = content.y + (content.height - font_size) / 2.0;
    let cursor_rect = Rect::new(
        cursor_x as f64,
        cursor_y as f64,
        (cursor_x + cursor_w) as f64,
        (cursor_y + font_size) as f64,
    );
    let vc = color_to_vello(color);
    scene.fill(Fill::NonZero, dpi, vc, None, &cursor_rect);
}

pub fn draw_hover_outline(scene: &mut Scene, rect: LayoutRect, scale_factor: f32) {
    let dpi = Affine::scale(scale_factor as f64);
    let color = Color::new([108.0 / 255.0, 92.0 / 255.0, 231.0 / 255.0, 100.0 / 255.0]);
    let stroke = Stroke::new(2.0);
    let r = Rect::new(
        rect.x as f64,
        rect.y as f64,
        (rect.x + rect.width) as f64,
        (rect.y + rect.height) as f64,
    );
    scene.stroke(&stroke, dpi, color, None, &r);
}

pub fn draw_focus_ring(scene: &mut Scene, rect: LayoutRect, scale_factor: f32) {
    let dpi = Affine::scale(scale_factor as f64);
    let color = Color::new([108.0 / 255.0, 92.0 / 255.0, 231.0 / 255.0, 180.0 / 255.0]);
    let stroke = Stroke::new(3.0);
    let r = Rect::new(
        rect.x as f64,
        rect.y as f64,
        (rect.x + rect.width) as f64,
        (rect.y + rect.height) as f64,
    );
    scene.stroke(&stroke, dpi, color, None, &r);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_chunk_key_tracks_payload_style_and_geometry() {
        let style = Style::default();
        let first = ComponentKind::Text {
            content: "prepared".to_string(),
        };
        let second = ComponentKind::Text {
            content: "changed".to_string(),
        };
        let key = display_chunk_key(&first, &style, 180.0, 24.0).unwrap();
        assert_eq!(key, display_chunk_key(&first, &style, 180.0, 24.0).unwrap());
        assert_ne!(
            key,
            display_chunk_key(&second, &style, 180.0, 24.0).unwrap()
        );
        assert_ne!(key, display_chunk_key(&first, &style, 181.0, 24.0).unwrap());
    }
}
