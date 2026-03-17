use tiny_skia::{Color as SkColor, FillRule, Paint, PathBuilder, Pixmap, Rect, Transform};
use w3cos_std::color::Color;
use w3cos_std::component::ComponentKind;
use w3cos_std::style::Style;

use crate::layout::LayoutRect;

/// Render a flat list of (layout, component_index) into a pixel buffer.
pub fn render_frame(
    pixmap: &mut Pixmap,
    nodes: &[(LayoutRect, &ComponentKind, &Style)],
    font: &fontdue::Font,
) {
    pixmap.fill(SkColor::from_rgba8(18, 18, 24, 255));

    for &(rect, kind, style) in nodes {
        render_node(pixmap, rect, kind, style, font);
    }
}

fn render_node(
    pixmap: &mut Pixmap,
    rect: LayoutRect,
    kind: &ComponentKind,
    style: &Style,
    font: &fontdue::Font,
) {
    if style.opacity <= 0.0 {
        return;
    }

    // Apply transform offset
    let tx = style.transform.translate_x;
    let ty = style.transform.translate_y;
    let rect = LayoutRect {
        x: rect.x + tx,
        y: rect.y + ty,
        width: rect.width * style.transform.scale_x,
        height: rect.height * style.transform.scale_y,
    };

    // Box shadow (render before the element)
    if let Some(ref shadow) = style.box_shadow {
        draw_box_shadow(pixmap, rect, shadow, style.border_radius, style.opacity);
    }

    let opacity = style.opacity;
    let bg = apply_opacity(style.background, opacity);

    if bg.a > 0 {
        draw_rect(pixmap, rect, bg, style.border_radius);
    }

    if style.border_width > 0.0 && style.border_color.a > 0 {
        draw_border(pixmap, rect, apply_opacity(style.border_color, opacity), style.border_width, style.border_radius);
    }

    let text_color = apply_opacity(style.color, opacity);

    match kind {
        ComponentKind::Text { content } => {
            draw_text(pixmap, rect.x, rect.y, content, style.font_size, text_color, font);
        }
        ComponentKind::Button { label } => {
            let btn_bg = if bg.a == 0 { apply_opacity(Color::rgb(55, 65, 81), opacity) } else { bg };
            draw_rect(pixmap, rect, btn_bg, style.border_radius.max(6.0));
            let text_x = rect.x + 16.0;
            let text_y = rect.y + 8.0;
            draw_text(pixmap, text_x, text_y, label, style.font_size, text_color, font);
        }
        _ => {}
    }
}

fn apply_opacity(c: Color, opacity: f32) -> Color {
    Color::rgba(c.r, c.g, c.b, (c.a as f32 * opacity) as u8)
}

fn draw_box_shadow(pixmap: &mut Pixmap, rect: LayoutRect, shadow: &w3cos_std::style::BoxShadow, radius: f32, opacity: f32) {
    let spread = shadow.spread_radius;
    let shadow_rect = LayoutRect {
        x: rect.x + shadow.offset_x - spread,
        y: rect.y + shadow.offset_y - spread,
        width: rect.width + spread * 2.0,
        height: rect.height + spread * 2.0,
    };
    let color = apply_opacity(shadow.color, opacity);

    // Approximate blur by drawing multiple expanding rectangles with decreasing alpha
    let steps = (shadow.blur_radius / 2.0).max(1.0) as u32;
    for i in 0..steps {
        let t = i as f32 / steps as f32;
        let expand = shadow.blur_radius * t;
        let alpha = ((1.0 - t) * color.a as f32 / steps as f32) as u8;
        if alpha == 0 { continue; }
        let c = Color::rgba(color.r, color.g, color.b, alpha);
        let r = LayoutRect {
            x: shadow_rect.x - expand,
            y: shadow_rect.y - expand,
            width: shadow_rect.width + expand * 2.0,
            height: shadow_rect.height + expand * 2.0,
        };
        draw_rect(pixmap, r, c, radius + expand);
    }
}

fn draw_rect(pixmap: &mut Pixmap, r: LayoutRect, color: Color, radius: f32) {
    let mut paint = Paint::default();
    paint.set_color(SkColor::from_rgba8(color.r, color.g, color.b, color.a));
    paint.anti_alias = true;

    if let Some(sk_rect) = Rect::from_xywh(r.x, r.y, r.width, r.height) {
        if radius > 0.0 {
            if let Some(path) = rounded_rect_path(r.x, r.y, r.width, r.height, radius) {
                pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
            }
        } else {
            pixmap.fill_rect(sk_rect, &paint, Transform::identity(), None);
        }
    }
}

fn draw_border(pixmap: &mut Pixmap, r: LayoutRect, color: Color, width: f32, _radius: f32) {
    let mut paint = Paint::default();
    paint.set_color(SkColor::from_rgba8(color.r, color.g, color.b, color.a));

    let rects = [
        Rect::from_xywh(r.x, r.y, r.width, width),
        Rect::from_xywh(r.x, r.y + r.height - width, r.width, width),
        Rect::from_xywh(r.x, r.y, width, r.height),
        Rect::from_xywh(r.x + r.width - width, r.y, width, r.height),
    ];
    for rect in rects.into_iter().flatten() {
        pixmap.fill_rect(rect, &paint, Transform::identity(), None);
    }
}

fn draw_text(
    pixmap: &mut Pixmap,
    x: f32,
    y: f32,
    text: &str,
    font_size: f32,
    color: Color,
    font: &fontdue::Font,
) {
    let mut cursor_x = x;
    let cursor_y = y + font_size;

    for ch in text.chars() {
        let (metrics, bitmap) = font.rasterize(ch, font_size);
        if metrics.width == 0 || metrics.height == 0 {
            cursor_x += metrics.advance_width;
            continue;
        }

        let px_w = pixmap.width() as i32;
        let px_h = pixmap.height() as i32;
        let gx = cursor_x as i32;
        let gy = cursor_y as i32 - metrics.height as i32 - metrics.ymin as i32;

        let pixels = pixmap.pixels_mut();
        for row in 0..metrics.height {
            for col in 0..metrics.width {
                let px = gx + col as i32;
                let py = gy + row as i32;
                if px < 0 || py < 0 || px >= px_w || py >= px_h {
                    continue;
                }
                let alpha = bitmap[row * metrics.width + col];
                if alpha == 0 {
                    continue;
                }
                let idx = (py * px_w + px) as usize;
                let a = (alpha as u16 * color.a as u16 / 255) as u8;
                let dst = pixels[idx];
                let blended = blend_pixel(dst, color.r, color.g, color.b, a);
                pixels[idx] = blended;
            }
        }

        cursor_x += metrics.advance_width;
    }
}

fn blend_pixel(
    dst: tiny_skia::PremultipliedColorU8,
    sr: u8, sg: u8, sb: u8, sa: u8,
) -> tiny_skia::PremultipliedColorU8 {
    let da = dst.alpha() as u16;
    let dr = dst.red() as u16;
    let dg = dst.green() as u16;
    let db = dst.blue() as u16;
    let sa16 = sa as u16;
    let inv = 255 - sa16;

    let out_a = (sa16 + da * inv / 255).min(255) as u8;
    let out_r = (sr as u16 * sa16 / 255 + dr * inv / 255).min(255) as u8;
    let out_g = (sg as u16 * sa16 / 255 + dg * inv / 255).min(255) as u8;
    let out_b = (sb as u16 * sa16 / 255 + db * inv / 255).min(255) as u8;

    tiny_skia::PremultipliedColorU8::from_rgba(out_r, out_g, out_b, out_a).unwrap()
}

fn rounded_rect_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    let r = r.min(w / 2.0).min(h / 2.0);
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.quad_to(x + w, y, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.quad_to(x + w, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.quad_to(x, y + h, x, y + h - r);
    pb.line_to(x, y + r);
    pb.quad_to(x, y, x + r, y);
    pb.close();
    pb.finish()
}
