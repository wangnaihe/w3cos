use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use skrifa::MetadataProvider;
use vello::kurbo::{Affine, Rect, RoundedRect, Stroke};
use vello::peniko::{
    Blob, Color, Fill, FontData, ImageAlphaType, ImageBrush, ImageData, ImageFormat,
};
use vello::{Glyph, Scene};
use w3cos_std::color::Color as AppColor;
use w3cos_std::component::ComponentKind;
use w3cos_std::style::Style;

use crate::layout::LayoutRect;

// ---------------------------------------------------------------------------
// GlyphCache — avoid repeated font parsing, charmap lookup, and rasterization
// ---------------------------------------------------------------------------

pub struct GlyphCache {
    entries: HashMap<(char, u32), GlyphEntry>,
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
                let advance = glyph_metrics.advance_width(glyph_id).unwrap_or_else(|| {
                    let (metrics, _) = fontdue_font.rasterize(ch, font_size);
                    metrics.advance_width
                });
                GlyphEntry {
                    glyph_id: Some(glyph_id.to_u32()),
                    advance,
                }
            } else {
                let (metrics, _) = fontdue_font.rasterize(ch, font_size);
                GlyphEntry {
                    glyph_id: None,
                    advance: metrics.advance_width,
                }
            }
        })
    }
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
) {
    let vw = width as f32;
    let vh = height as f32;

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
        scene.push_clip_layer(Fill::NonZero, Affine::IDENTITY, &clip_shape);
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

    let needs_opacity_layer = opacity < 1.0;
    if needs_opacity_layer {
        let bounds = Rect::new(
            rect.x as f64,
            rect.y as f64,
            (rect.x + rect.width) as f64,
            (rect.y + rect.height) as f64,
        );
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::Mix::Normal,
            opacity,
            Affine::IDENTITY,
            &bounds,
        );
    }

    if let Some(ref shadow) = style.box_shadow {
        draw_box_shadow(scene, rect, shadow, style.border_radius);
    }

    let bg = style.background;

    if bg.a > 0 {
        draw_rect(scene, rect, bg, style.border_radius);
    }

    if style.border_width > 0.0 && style.border_color.a > 0 {
        draw_border(
            scene,
            rect,
            style.border_color,
            style.border_width,
            style.border_radius,
        );
    }

    let text_color = style.color;

    match kind {
        ComponentKind::Text { content } => {
            draw_text(
                scene,
                rect.x,
                rect.y,
                content,
                style.font_size,
                text_color,
                font_data,
                font,
                glyph_cache,
            );
        }
        ComponentKind::Button { label } => {
            let btn_bg = if bg.a == 0 {
                AppColor::rgb(55, 65, 81)
            } else {
                bg
            };
            draw_rect(scene, rect, btn_bg, style.border_radius.max(6.0));
            let text_x = rect.x + 16.0;
            let text_y = rect.y + 8.0;
            draw_text(
                scene,
                text_x,
                text_y,
                label,
                style.font_size,
                text_color,
                font_data,
                font,
                glyph_cache,
            );
        }
        ComponentKind::Image { src } => {
            if let Some(decoded) = crate::image_loader::get_or_load(src) {
                let blob = Blob::new(
                    decoded.data.clone() as Arc<dyn AsRef<[u8]> + Send + Sync>,
                );
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
                draw_rect(scene, rect, placeholder_bg, style.border_radius);
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
                );
            }
        }
        ComponentKind::TextInput { value, placeholder } => {
            let display_value = text_input_value.unwrap_or(value.as_str());
            let (display_text, text_color_final) = if display_value.is_empty() {
                (placeholder.as_str(), AppColor::rgb(107, 114, 128))
            } else {
                (display_value, text_color)
            };
            let input_bg = if bg.a == 0 {
                AppColor::rgb(30, 30, 40)
            } else {
                bg
            };
            draw_rect(scene, rect, input_bg, style.border_radius.max(4.0));
            let border_color = if is_focused {
                AppColor::rgb(108, 92, 231)
            } else if style.border_color.a > 0 {
                style.border_color
            } else {
                AppColor::rgb(75, 85, 99)
            };
            let border_w = if is_focused {
                style.border_width.max(2.0)
            } else {
                style.border_width.max(1.0)
            };
            draw_border(
                scene,
                rect,
                border_color,
                border_w,
                style.border_radius.max(4.0),
            );
            let text_x = rect.x + 12.0;
            let text_y = rect.y + (rect.height - style.font_size) / 2.0 + style.font_size;
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
            );
            if is_focused {
                draw_blinking_cursor(
                    scene,
                    rect,
                    display_value,
                    style.font_size,
                    text_color,
                    font_data,
                    font,
                    glyph_cache,
                );
            }
        }
        _ => {}
    }

    if needs_opacity_layer {
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
    scene.draw_blurred_rounded_rect(Affine::IDENTITY, shadow_rect, color, r, std_dev);
}

fn draw_rect(scene: &mut Scene, r: LayoutRect, color: AppColor, radius: f32) {
    let vc = color_to_vello(color);
    if radius > 0.0 {
        let rr = RoundedRect::new(
            r.x as f64,
            r.y as f64,
            (r.x + r.width) as f64,
            (r.y + r.height) as f64,
            radius as f64,
        );
        scene.fill(Fill::NonZero, Affine::IDENTITY, vc, None, &rr);
    } else {
        let rect = Rect::new(
            r.x as f64,
            r.y as f64,
            (r.x + r.width) as f64,
            (r.y + r.height) as f64,
        );
        scene.fill(Fill::NonZero, Affine::IDENTITY, vc, None, &rect);
    }
}

fn draw_border(scene: &mut Scene, r: LayoutRect, color: AppColor, width: f32, radius: f32) {
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
        scene.stroke(&stroke, Affine::IDENTITY, vc, None, &rr);
    } else {
        let rect = Rect::new(
            r.x as f64 + half,
            r.y as f64 + half,
            (r.x + r.width) as f64 - half,
            (r.y + r.height) as f64 - half,
        );
        scene.stroke(&stroke, Affine::IDENTITY, vc, None, &rect);
    }
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
            .brush(vc)
            .draw(Fill::NonZero, glyphs.into_iter());
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_blinking_cursor(
    scene: &mut Scene,
    rect: LayoutRect,
    text: &str,
    font_size: f32,
    color: AppColor,
    font_data: &FontData,
    fontdue_font: &fontdue::Font,
    glyph_cache: &mut GlyphCache,
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

    let mut cursor_x = rect.x + 12.0;
    for ch in text.chars() {
        let entry =
            glyph_cache.lookup_or_insert(ch, font_size, &charmap, &glyph_metrics, fontdue_font);
        cursor_x += entry.advance;
    }

    let cursor_w = 2.0f32.max(font_size * 0.1);
    let cursor_y = rect.y + (rect.height - font_size) / 2.0;
    let cursor_rect = Rect::new(
        cursor_x as f64,
        cursor_y as f64,
        (cursor_x + cursor_w) as f64,
        (cursor_y + font_size) as f64,
    );
    let vc = color_to_vello(color);
    scene.fill(Fill::NonZero, Affine::IDENTITY, vc, None, &cursor_rect);
}

pub fn draw_hover_outline(scene: &mut Scene, rect: LayoutRect) {
    let color = Color::new([108.0 / 255.0, 92.0 / 255.0, 231.0 / 255.0, 100.0 / 255.0]);
    let stroke = Stroke::new(2.0);
    let r = Rect::new(
        rect.x as f64,
        rect.y as f64,
        (rect.x + rect.width) as f64,
        (rect.y + rect.height) as f64,
    );
    scene.stroke(&stroke, Affine::IDENTITY, color, None, &r);
}

pub fn draw_focus_ring(scene: &mut Scene, rect: LayoutRect) {
    let color = Color::new([108.0 / 255.0, 92.0 / 255.0, 231.0 / 255.0, 180.0 / 255.0]);
    let stroke = Stroke::new(3.0);
    let r = Rect::new(
        rect.x as f64,
        rect.y as f64,
        (rect.x + rect.width) as f64,
        (rect.y + rect.height) as f64,
    );
    scene.stroke(&stroke, Affine::IDENTITY, color, None, &r);
}
