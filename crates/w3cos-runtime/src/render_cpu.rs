use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use tiny_skia::{Color as SkColor, FillRule, Mask, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};
use w3cos_std::color::Color;
use w3cos_std::component::ComponentKind;
use w3cos_std::style::{Style, TextAlign};

use crate::filter::{self, CssFilter};
use crate::layout::LayoutRect;
use crate::text_layout;

fn page_bg() -> SkColor {
    SkColor::from_rgba8(11, 18, 32, 255)
}

/// Full-frame paint (content dirty or first frame).
pub fn render_frame(
    pixmap: &mut Pixmap,
    nodes: &[(usize, LayoutRect, &ComponentKind, &Style)],
    font: &fontdue::Font,
    scroll_info: &[Option<(f32, f32, LayoutRect)>],
    text_input_values: &HashMap<usize, String>,
    focused_index: Option<usize>,
) {
    pixmap.fill(page_bg());
    paint_nodes(
        pixmap,
        nodes,
        font,
        scroll_info,
        text_input_values,
        focused_index,
        None,
        None,
    );
}

/// Chromium-style scroll damage: repaint scroll container + descendants only.
pub fn render_scroll_damage(
    pixmap: &mut Pixmap,
    nodes: &[(usize, LayoutRect, &ComponentKind, &Style)],
    font: &fontdue::Font,
    scroll_info: &[Option<(f32, f32, LayoutRect)>],
    text_input_values: &HashMap<usize, String>,
    focused_index: Option<usize>,
    scroll_targets: &[usize],
    scrollable: &[(usize, LayoutRect, crate::layout::ScrollExtent)],
    scroll_ancestor: &[Option<usize>],
) {
    for &scroll_idx in scroll_targets {
        if let Some((_, rect, _)) = scrollable.iter().find(|(i, _, _)| *i == scroll_idx) {
            clear_rect(pixmap, *rect);
        }
    }
    paint_nodes(
        pixmap,
        nodes,
        font,
        scroll_info,
        text_input_values,
        focused_index,
        Some(scroll_targets),
        Some(scroll_ancestor),
    );
}

fn node_in_scroll_damage(
    idx: usize,
    scroll_targets: &[usize],
    scroll_ancestor: &[Option<usize>],
) -> bool {
    scroll_targets.iter().any(|&scroll_idx| {
        idx == scroll_idx || scroll_ancestor.get(idx).and_then(|a| *a) == Some(scroll_idx)
    })
}

fn clear_rect(pixmap: &mut Pixmap, rect: LayoutRect) {
    let Some(sk) = Rect::from_xywh(rect.x, rect.y, rect.width, rect.height) else {
        return;
    };
    let mut paint = Paint::default();
    paint.set_color(page_bg());
    pixmap.fill_rect(sk, &paint, Transform::identity(), None);
}

fn paint_nodes(
    pixmap: &mut Pixmap,
    nodes: &[(usize, LayoutRect, &ComponentKind, &Style)],
    font: &fontdue::Font,
    scroll_info: &[Option<(f32, f32, LayoutRect)>],
    text_input_values: &HashMap<usize, String>,
    focused_index: Option<usize>,
    scroll_damage: Option<&[usize]>,
    scroll_ancestor: Option<&[Option<usize>]>,
) {
    for &(idx, rect, kind, style) in nodes {
        if let (Some(targets), Some(ancestor)) = (scroll_damage, scroll_ancestor) {
            if !node_in_scroll_damage(idx, targets, ancestor) {
                continue;
            }
        }
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
            pixmap,
            offset_rect,
            kind,
            style,
            font,
            clip,
            pixmap.width(),
            pixmap.height(),
            text_value,
            is_focused,
            false,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn render_node(
    pixmap: &mut Pixmap,
    rect: LayoutRect,
    kind: &ComponentKind,
    style: &Style,
    font: &fontdue::Font,
    clip_rect: Option<LayoutRect>,
    pixmap_w: u32,
    pixmap_h: u32,
    text_input_value: Option<&str>,
    is_focused: bool,
    in_layer: bool,
) {
    if style.opacity <= 0.0 {
        return;
    }

    let clip_mask = clip_rect.and_then(|cr| {
        let mut mask = Mask::new(pixmap_w, pixmap_h)?;
        mask.clear();
        let path = rounded_rect_path(cr.x, cr.y, cr.width, cr.height, 0.0)?;
        mask.fill_path(&path, FillRule::Winding, true, Transform::identity());
        Some(mask)
    });
    let clip_mask = clip_mask.as_ref();

    // Apply transform offset
    let tx = style.transform.translate_x;
    let ty = style.transform.translate_y;
    let rect = LayoutRect {
        x: rect.x + tx,
        y: rect.y + ty,
        width: rect.width * style.transform.scale_x,
        height: rect.height * style.transform.scale_y,
    };

    // Box shadow (render before the element; shadows stay outside filtered layer)
    let css_filter = style.filter.as_deref().and_then(filter::parse_css_filter);
    if !in_layer {
        if let Some(ref chain) = css_filter {
            if let Some(shadow) = chain.drop_shadow() {
                draw_box_shadow(
                    pixmap,
                    rect,
                    shadow,
                    style.border_radius,
                    style.opacity,
                    clip_mask,
                );
            }
        }
        if let Some(ref shadow) = style.box_shadow {
            draw_box_shadow(
                pixmap,
                rect,
                shadow,
                style.border_radius,
                style.opacity,
                clip_mask,
            );
        }

        if css_filter.as_ref().is_some_and(|c| c.has_blur()) {
            if let Some(ref chain) = css_filter {
                render_node_layer_blur(
                    pixmap,
                    rect,
                    kind,
                    style,
                    font,
                    clip_mask,
                    text_input_value,
                    is_focused,
                    chain,
                );
            }
            return;
        }
    }

    let opacity = style.opacity;
    let color_chain = if in_layer {
        None
    } else {
        css_filter.as_ref()
    };
    let bg = node_color(style.background, opacity, color_chain);

    if bg.a > 0 {
        draw_rect(pixmap, rect, bg, style.border_radius, clip_mask);
    }

    if style.border_width > 0.0 && style.border_color.a > 0 {
        draw_border(
            pixmap,
            rect,
            node_color(style.border_color, opacity, color_chain),
            style.border_width,
            style.border_radius,
            clip_mask,
        );
    }

    let text_color = node_color(style.color, opacity, color_chain);

    match kind {
        ComponentKind::Text { content } => {
            draw_text_in_rect(
                pixmap,
                rect,
                content,
                style,
                text_color,
                font,
                clip_mask,
            );
        }
        ComponentKind::Button { label } => {
            let btn_bg = if bg.a == 0 {
                node_color(Color::rgb(55, 65, 81), opacity, color_chain)
            } else {
                bg
            };
            draw_rect(
                pixmap,
                rect,
                btn_bg,
                style.border_radius.max(6.0),
                clip_mask,
            );
            draw_text_centered_in_rect(
                pixmap,
                rect,
                label,
                style,
                text_color,
                font,
                clip_mask,
            );
        }
        ComponentKind::Image { src } => {
            if let Some(decoded) = crate::image_loader::get_or_load(src) {
                draw_image_pixels(
                    pixmap,
                    rect,
                    decoded.width,
                    decoded.height,
                    &decoded.data,
                    opacity,
                    clip_mask,
                );
            } else {
                let placeholder_bg = if bg.a == 0 {
                    apply_opacity(Color::rgb(40, 40, 50), opacity)
                } else {
                    bg
                };
                draw_rect(pixmap, rect, placeholder_bg, style.border_radius, clip_mask);
                let border_color = if style.border_width > 0.0 && style.border_color.a > 0 {
                    apply_opacity(style.border_color, opacity)
                } else {
                    apply_opacity(Color::rgb(100, 100, 120), opacity)
                };
                draw_border(
                    pixmap,
                    rect,
                    border_color,
                    style.border_width.max(1.0),
                    style.border_radius,
                    clip_mask,
                );
                let label = format!("[Image: {}]", src);
                draw_text_line(
                    pixmap,
                    rect.x + 8.0,
                    rect.y + 8.0,
                    &label,
                    style.font_size,
                    text_color,
                    font,
                    clip_mask,
                );
            }
        }
        ComponentKind::TextInput { value, placeholder } => {
            let display_value = text_input_value.unwrap_or(value.as_str());
            let (display_text, text_color_override) = if display_value.is_empty() {
                (
                    placeholder.as_str(),
                    Some(apply_opacity(Color::rgb(107, 114, 128), opacity)),
                )
            } else {
                (display_value, None)
            };
            let input_bg = if bg.a == 0 {
                apply_opacity(Color::rgb(30, 30, 40), opacity)
            } else {
                bg
            };
            draw_rect(
                pixmap,
                rect,
                input_bg,
                style.border_radius.max(4.0),
                clip_mask,
            );
            let border_color = if is_focused {
                apply_opacity(Color::rgb(108, 92, 231), opacity)
            } else if style.border_color.a > 0 {
                apply_opacity(style.border_color, opacity)
            } else {
                apply_opacity(Color::rgb(75, 85, 99), opacity)
            };
            let border_w = if is_focused {
                style.border_width.max(2.0)
            } else {
                style.border_width.max(1.0)
            };
            draw_border(
                pixmap,
                rect,
                border_color,
                border_w,
                style.border_radius.max(4.0),
                clip_mask,
            );
            let text_x = rect.x + 12.0;
            let text_y = rect.y + (rect.height - style.font_size) / 2.0 + style.font_size;
            let tc = text_color_override.unwrap_or(text_color);
            draw_text_line(
                pixmap,
                text_x,
                text_y,
                display_text,
                style.font_size,
                tc,
                font,
                clip_mask,
            );
            if is_focused {
                draw_blinking_cursor(
                    pixmap,
                    rect,
                    display_value,
                    style.font_size,
                    text_color,
                    font,
                    clip_mask,
                );
            }
        }
        _ => {}
    }
}

fn apply_opacity(c: Color, opacity: f32) -> Color {
    Color::rgba(c.r, c.g, c.b, (c.a as f32 * opacity) as u8)
}

fn node_color(c: Color, opacity: f32, chain: Option<&CssFilter>) -> Color {
    let c = chain
        .map(|f| filter::apply_filter_to_color(c, f))
        .unwrap_or(c);
    apply_opacity(c, opacity)
}

#[allow(clippy::too_many_arguments)]
fn render_node_layer_blur(
    pixmap: &mut Pixmap,
    rect: LayoutRect,
    kind: &ComponentKind,
    style: &Style,
    font: &fontdue::Font,
    clip_mask: Option<&Mask>,
    text_input_value: Option<&str>,
    is_focused: bool,
    chain: &CssFilter,
) {
    let pad = chain.max_blur_px().ceil() as u32 + 2;
    let lw = (rect.width as u32 + pad * 2).max(1);
    let lh = (rect.height as u32 + pad * 2).max(1);
    let Some(mut layer) = Pixmap::new(lw, lh) else {
        return;
    };
    layer.fill(SkColor::TRANSPARENT);

    let inner = LayoutRect {
        x: pad as f32,
        y: pad as f32,
        width: rect.width,
        height: rect.height,
    };
    render_node(
        &mut layer,
        inner,
        kind,
        style,
        font,
        None,
        lw,
        lh,
        text_input_value,
        is_focused,
        true,
    );
    filter::apply_chain_to_rgba(layer.data_mut(), lw, lh, chain);

    let paint = tiny_skia::PixmapPaint::default();
    pixmap.draw_pixmap(
        (rect.x - pad as f32) as i32,
        (rect.y - pad as f32) as i32,
        layer.as_ref(),
        &paint,
        Transform::identity(),
        clip_mask,
    );
}

fn resolved_color(c: Color, opacity: f32, filter: Option<&CssFilter>) -> Color {
    node_color(c, opacity, filter)
}

fn draw_box_shadow(
    pixmap: &mut Pixmap,
    rect: LayoutRect,
    shadow: &w3cos_std::style::BoxShadow,
    radius: f32,
    opacity: f32,
    clip_mask: Option<&Mask>,
) {
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
        if alpha == 0 {
            continue;
        }
        let c = Color::rgba(color.r, color.g, color.b, alpha);
        let r = LayoutRect {
            x: shadow_rect.x - expand,
            y: shadow_rect.y - expand,
            width: shadow_rect.width + expand * 2.0,
            height: shadow_rect.height + expand * 2.0,
        };
        draw_rect(pixmap, r, c, radius + expand, clip_mask);
    }
}

fn draw_rect(
    pixmap: &mut Pixmap,
    r: LayoutRect,
    color: Color,
    radius: f32,
    clip_mask: Option<&Mask>,
) {
    let mut paint = Paint::default();
    paint.set_color(SkColor::from_rgba8(color.r, color.g, color.b, color.a));
    paint.anti_alias = true;

    if let Some(sk_rect) = Rect::from_xywh(r.x, r.y, r.width, r.height) {
        if radius > 0.0 {
            if let Some(path) = rounded_rect_path(r.x, r.y, r.width, r.height, radius) {
                pixmap.fill_path(
                    &path,
                    &paint,
                    FillRule::Winding,
                    Transform::identity(),
                    clip_mask,
                );
            }
        } else {
            pixmap.fill_rect(sk_rect, &paint, Transform::identity(), clip_mask);
        }
    }
}

fn draw_border(
    pixmap: &mut Pixmap,
    r: LayoutRect,
    color: Color,
    width: f32,
    radius: f32,
    clip_mask: Option<&Mask>,
) {
    if width <= 0.0 {
        return;
    }
    let mut paint = Paint::default();
    paint.set_color(SkColor::from_rgba8(color.r, color.g, color.b, color.a));
    paint.anti_alias = true;

    if radius > 0.5 {
        let inset = width * 0.5;
        let inner = LayoutRect {
            x: r.x + inset,
            y: r.y + inset,
            width: (r.width - width).max(0.0),
            height: (r.height - width).max(0.0),
        };
        if let Some(path) = rounded_rect_path(inner.x, inner.y, inner.width, inner.height, radius) {
            let stroke = Stroke {
                width,
                ..Stroke::default()
            };
            pixmap.stroke_path(
                &path,
                &paint,
                &stroke,
                Transform::identity(),
                clip_mask,
            );
        }
        return;
    }

    let rects = [
        Rect::from_xywh(r.x, r.y, r.width, width),
        Rect::from_xywh(r.x, r.y + r.height - width, r.width, width),
        Rect::from_xywh(r.x, r.y, width, r.height),
        Rect::from_xywh(r.x + r.width - width, r.y, width, r.height),
    ];
    for rect in rects.into_iter().flatten() {
        pixmap.fill_rect(rect, &paint, Transform::identity(), clip_mask);
    }
}

#[allow(clippy::too_many_arguments)]
fn measure_text_width(text: &str, font_size: f32, font: &fontdue::Font) -> f32 {
    text_layout::measure_text_width_font(text, font_size, font)
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

fn draw_text_ink_in_box(
    pixmap: &mut Pixmap,
    box_rect: LayoutRect,
    text: &str,
    font_size: f32,
    color: Color,
    font: &fontdue::Font,
    align: TextAlign,
    clip_mask: Option<&Mask>,
) {
    let ink = text_layout::measure_text_ink_bounds(text, font_size, font, 0.0, 0.0);
    if ink.width <= 0.0 && ink.height <= 0.0 {
        draw_text_line(pixmap, box_rect.x, box_rect.y, text, font_size, color, font, clip_mask);
        return;
    }

    let x = match align {
        TextAlign::Right => box_rect.x + box_rect.width - ink.width - ink.left,
        TextAlign::Center => box_rect.x + (box_rect.width - ink.width) * 0.5 - ink.left,
        TextAlign::Left | TextAlign::Justify => box_rect.x - ink.left,
    };
    let y = box_rect.y + (box_rect.height - ink.height) * 0.5 - ink.top;
    draw_text_line(pixmap, x, y, text, font_size, color, font, clip_mask);
}

fn single_line_h_align(style: &Style, box_w: f32, ink_w: f32) -> TextAlign {
    match style.text_align {
        TextAlign::Center | TextAlign::Right => style.text_align,
        TextAlign::Left if style.background.a > 0 => TextAlign::Center,
        TextAlign::Left
            if matches!(style.white_space, w3cos_std::style::WhiteSpace::NoWrap | w3cos_std::style::WhiteSpace::Pre)
                && box_w > ink_w + 1.5 =>
        {
            TextAlign::Center
        }
        TextAlign::Left | TextAlign::Justify => TextAlign::Left,
    }
}

fn draw_text_in_rect(
    pixmap: &mut Pixmap,
    rect: LayoutRect,
    text: &str,
    style: &Style,
    color: Color,
    font: &fontdue::Font,
    clip_mask: Option<&Mask>,
) {
    let content = text_paint_box(rect, style);
    let line_h = style.font_size * style.line_height;
    let lines = text_layout::wrap_text_font(
        text,
        content.width,
        style.font_size,
        font,
        style.white_space,
    );

    if lines.len() == 1 {
        let align = single_line_h_align(
            style,
            content.width,
            text_layout::measure_text_ink_bounds(&lines[0], style.font_size, font, 0.0, 0.0).width,
        );
        draw_text_ink_in_box(
            pixmap,
            content,
            &lines[0],
            style.font_size,
            color,
            font,
            align,
            clip_mask,
        );
        return;
    }

    let block_h = lines.len() as f32 * line_h;
    let block_top = content.y + (content.height - block_h).max(0.0) * 0.5;

    for (i, line) in lines.iter().enumerate() {
        let ink = text_layout::measure_text_ink_bounds(line, style.font_size, font, 0.0, 0.0);
        let align = single_line_h_align(style, content.width, ink.width);
        let x = match align {
            TextAlign::Right => content.x + content.width - ink.width - ink.left,
            TextAlign::Center => content.x + (content.width - ink.width) * 0.5 - ink.left,
            TextAlign::Left | TextAlign::Justify => content.x - ink.left,
        };
        let y = block_top + i as f32 * line_h;
        draw_text_line(
            pixmap,
            x,
            y,
            line,
            style.font_size,
            color,
            font,
            clip_mask,
        );
    }
}

fn draw_text_centered_in_rect(
    pixmap: &mut Pixmap,
    rect: LayoutRect,
    text: &str,
    style: &Style,
    color: Color,
    font: &fontdue::Font,
    clip_mask: Option<&Mask>,
) {
    draw_text_ink_in_box(
        pixmap,
        text_paint_box(rect, style),
        text,
        style.font_size,
        color,
        font,
        TextAlign::Center,
        clip_mask,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_text_line(
    pixmap: &mut Pixmap,
    x: f32,
    y: f32,
    text: &str,
    font_size: f32,
    color: Color,
    font: &fontdue::Font,
    clip_mask: Option<&Mask>,
) {
    let mut cursor_x = x;
    let cursor_y = y + font_size;

    let px_w = pixmap.width() as i32;
    let px_h = pixmap.height() as i32;

    let in_clip = |px: i32, py: i32| -> bool {
        if let Some(mask) = clip_mask {
            if px < 0 || py < 0 || px >= mask.width() as i32 || py >= mask.height() as i32 {
                return false;
            }
            let idx = (py * mask.width() as i32 + px) as usize;
            mask.data().get(idx).copied().unwrap_or(0) > 0
        } else {
            true
        }
    };

    for ch in text.chars() {
        let (metrics, bitmap) = font.rasterize(ch, font_size);
        let advance = if metrics.advance_width > 0.0 {
            metrics.advance_width
        } else {
            text_layout::estimated_char_width(ch, font_size)
        };
        if metrics.width == 0 || metrics.height == 0 {
            cursor_x += advance;
            continue;
        }

        let (gx, gy) = text_layout::glyph_pixel_origin(cursor_x, cursor_y, &metrics);

        paint_glyph_bitmap(
            pixmap,
            gx,
            gy,
            &metrics,
            &bitmap,
            color,
            px_w,
            px_h,
            &in_clip,
        );

        cursor_x += advance;
    }
}

fn paint_glyph_bitmap(
    pixmap: &mut Pixmap,
    gx: i32,
    gy: i32,
    metrics: &fontdue::Metrics,
    bitmap: &[u8],
    color: Color,
    px_w: i32,
    px_h: i32,
    in_clip: &dyn Fn(i32, i32) -> bool,
) {
    let pixels = pixmap.pixels_mut();
    for row in 0..metrics.height {
        for col in 0..metrics.width {
            let px = gx + col as i32;
            let py = gy + row as i32;
            if px < 0 || py < 0 || px >= px_w || py >= px_h {
                continue;
            }
            if !in_clip(px, py) {
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
}

fn draw_blinking_cursor(
    pixmap: &mut Pixmap,
    rect: LayoutRect,
    text: &str,
    font_size: f32,
    color: Color,
    font: &fontdue::Font,
    clip_mask: Option<&Mask>,
) {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    if (ms / 500).is_multiple_of(2) {
        return;
    }
    let mut cursor_x = rect.x + 12.0;
    let cursor_y = rect.y + (rect.height - font_size) / 2.0 + font_size;

    for ch in text.chars() {
        let (metrics, _) = font.rasterize(ch, font_size);
        cursor_x += metrics.advance_width;
    }
    let px_w = pixmap.width() as i32;
    let px_h = pixmap.height() as i32;
    let in_clip = |px: i32, py: i32| -> bool {
        if let Some(mask) = clip_mask {
            if px < 0 || py < 0 || px >= mask.width() as i32 || py >= mask.height() as i32 {
                return false;
            }
            let idx = (py * mask.width() as i32 + px) as usize;
            mask.data().get(idx).copied().unwrap_or(0) > 0
        } else {
            true
        }
    };
    let cw = 2.0f32.max(font_size * 0.1);
    let gx = cursor_x as i32;
    let gy = (cursor_y - font_size) as i32;
    let gw = cw.ceil() as i32;
    let gh = font_size.ceil() as i32;
    let pixels = pixmap.pixels_mut();
    for row in 0..gh {
        for col in 0..gw {
            let px = gx + col;
            let py = gy + row;
            if px < 0 || py < 0 || px >= px_w || py >= px_h {
                continue;
            }
            if !in_clip(px, py) {
                continue;
            }
            let idx = (py * px_w + px) as usize;
            let dst = pixels[idx];
            let blended = blend_pixel(dst, color.r, color.g, color.b, 255);
            pixels[idx] = blended;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_image_pixels(
    pixmap: &mut Pixmap,
    rect: LayoutRect,
    img_w: u32,
    img_h: u32,
    rgba: &[u8],
    opacity: f32,
    clip_mask: Option<&Mask>,
) {
    let dest_w = rect.width.ceil() as u32;
    let dest_h = rect.height.ceil() as u32;
    if dest_w == 0 || dest_h == 0 || img_w == 0 || img_h == 0 {
        return;
    }
    let px_w = pixmap.width() as i32;
    let px_h = pixmap.height() as i32;
    let pixels = pixmap.pixels_mut();

    for dy in 0..dest_h {
        for dx in 0..dest_w {
            let px = rect.x as i32 + dx as i32;
            let py = rect.y as i32 + dy as i32;
            if px < 0 || py < 0 || px >= px_w || py >= px_h {
                continue;
            }
            if let Some(mask) = clip_mask {
                if px >= mask.width() as i32 || py >= mask.height() as i32 {
                    continue;
                }
                let mask_idx = (py * mask.width() as i32 + px) as usize;
                if mask.data().get(mask_idx).copied().unwrap_or(0) == 0 {
                    continue;
                }
            }
            let src_x = ((dx as f32 / dest_w as f32) * img_w as f32) as u32;
            let src_y = ((dy as f32 / dest_h as f32) * img_h as f32) as u32;
            let src_x = src_x.min(img_w - 1);
            let src_y = src_y.min(img_h - 1);
            let src_idx = ((src_y * img_w + src_x) * 4) as usize;

            let r = rgba[src_idx];
            let g = rgba[src_idx + 1];
            let b = rgba[src_idx + 2];
            let a = (rgba[src_idx + 3] as f32 * opacity) as u8;
            if a == 0 {
                continue;
            }

            let dst_idx = (py * px_w + px) as usize;
            let dst = pixels[dst_idx];
            pixels[dst_idx] = blend_pixel(dst, r, g, b, a);
        }
    }
}

fn blend_pixel(
    dst: tiny_skia::PremultipliedColorU8,
    sr: u8,
    sg: u8,
    sb: u8,
    sa: u8,
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

#[cfg(test)]
mod font_cjk_tests {
    use super::*;

    #[test]
    fn cjk_subset_font_loads() {
        let data = include_bytes!("../assets/CJK-Subset.ttf");
        let font = fontdue::Font::from_bytes(data as &[u8], fontdue::FontSettings::default()).unwrap();
        let (m, bmp) = font.rasterize('话', 16.0);
        assert!(m.width > 0 && !bmp.is_empty());
        let (m2, _) = font.rasterize('A', 16.0);
        assert!(m2.advance_width > 0.0);
    }
}
