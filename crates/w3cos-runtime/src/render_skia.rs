//! Skia replay backend for the retained W3COS paint artifact.
//!
//! This module intentionally consumes the same pre-painted node stream as the
//! Vello and tiny-skia backends. It does not perform layout or invent native
//! widget defaults: CSS-derived geometry and style remain the source of truth.

use std::collections::HashMap;

use skia_safe::{
    AlphaType, BlurStyle, Canvas, Color, ColorType, Font, FontMgr, ImageInfo, MaskFilter, Paint,
    Rect, Surface, Typeface, paint,
};
use w3cos_std::component::ComponentKind;
use w3cos_std::style::{Style, TextAlign};

use crate::layout::LayoutRect;
use crate::text_layout;

pub struct SkiaRasterizer {
    surface: Option<Surface>,
    size: (u32, u32),
    rgba: Vec<u8>,
    typeface: Typeface,
}

impl SkiaRasterizer {
    pub fn new(font_bytes: &[u8]) -> Option<Self> {
        let typeface = FontMgr::default().new_from_data(font_bytes, None)?;
        Some(Self {
            surface: None,
            size: (0, 0),
            rgba: Vec::new(),
            typeface,
        })
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
    ) -> Option<&[u8]> {
        self.ensure_surface(width, height)?;
        let surface = self.surface.as_mut()?;
        replay_frame(
            surface.canvas(),
            nodes,
            &self.typeface,
            metrics_font,
            scroll_info,
            text_input_values,
            focused_index,
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

#[allow(clippy::too_many_arguments)]
fn replay_frame(
    canvas: &Canvas,
    nodes: &[(usize, LayoutRect, &ComponentKind, &Style)],
    typeface: &Typeface,
    metrics_font: &fontdue::Font,
    scroll_info: &[Option<(f32, f32, LayoutRect)>],
    text_input_values: &HashMap<usize, String>,
    focused_index: Option<usize>,
) {
    canvas.clear(Color::from_argb(255, 11, 18, 32));
    for &(idx, rect, kind, style) in nodes {
        if style.opacity <= 0.0 {
            continue;
        }
        let (rect, clip) = match scroll_info.get(idx).copied().flatten() {
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

        let save = canvas.save();
        if let Some(clip) = clip {
            canvas.clip_rect(to_rect(clip), None, Some(false));
        }
        render_node(
            canvas,
            rect,
            kind,
            style,
            typeface,
            metrics_font,
            text_input_values.get(&idx).map(String::as_str),
            focused_index == Some(idx),
        );
        canvas.restore_to_count(save);
    }
}

#[cfg(target_os = "ios")]
pub struct SkiaMetalPresenter {
    layer: objc2_06::rc::Retained<objc2_quartz_core::CAMetalLayer>,
    command_queue:
        objc2_06::rc::Retained<objc2_06::runtime::ProtocolObject<dyn objc2_metal::MTLCommandQueue>>,
    context: skia_safe::gpu::DirectContext,
    typeface: Typeface,
}

#[cfg(target_os = "ios")]
impl SkiaMetalPresenter {
    pub fn new(window: &winit::window::Window, font_bytes: &[u8]) -> Option<Self> {
        use objc2_metal::{MTLCreateSystemDefaultDevice, MTLDevice};
        use objc2_quartz_core::CALayer;
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

        let typeface = FontMgr::default().new_from_data(font_bytes, None)?;
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
        })
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
                nodes,
                &self.typeface,
                metrics_font,
                scroll_info,
                text_input_values,
                focused_index,
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

#[allow(clippy::too_many_arguments)]
fn render_node(
    canvas: &Canvas,
    rect: LayoutRect,
    kind: &ComponentKind,
    style: &Style,
    typeface: &Typeface,
    metrics_font: &fontdue::Font,
    text_input_value: Option<&str>,
    focused: bool,
) {
    let transform = style.transform;
    let rect = LayoutRect {
        x: rect.x + transform.translate_x,
        y: rect.y + transform.translate_y,
        width: rect.width * transform.scale_x,
        height: rect.height * transform.scale_y,
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
        draw_round_rect(canvas, shadow_rect, style.border_radius + spread, &paint);
    }

    let bg = style.background;
    if bg.a > 0 {
        draw_round_rect(
            canvas,
            rect,
            style.border_radius,
            &color_paint(bg, style.opacity),
        );
    }
    if style.border_width > 0.0 && style.border_color.a > 0 {
        let mut border = color_paint(style.border_color, style.opacity);
        border.set_style(paint::Style::Stroke);
        border.set_stroke_width(style.border_width);
        let inset = style.border_width * 0.5;
        draw_round_rect(
            canvas,
            LayoutRect {
                x: rect.x + inset,
                y: rect.y + inset,
                width: (rect.width - style.border_width).max(0.0),
                height: (rect.height - style.border_width).max(0.0),
            },
            style.border_radius,
            &border,
        );
    }

    match kind {
        ComponentKind::Text { content } => {
            draw_text_in_rect(canvas, rect, content, style, typeface, metrics_font);
        }
        ComponentKind::Button { label } => {
            if bg.a == 0 {
                draw_round_rect(
                    canvas,
                    rect,
                    style.border_radius.max(6.0),
                    &color_paint(w3cos_std::color::Color::rgb(55, 65, 81), style.opacity),
                );
            }
            draw_centered_text(canvas, rect, label, style, typeface, metrics_font);
        }
        ComponentKind::TextInput { value, placeholder } => {
            let value = text_input_value.unwrap_or(value);
            let text = if value.is_empty() { placeholder } else { value };
            if bg.a == 0 {
                draw_round_rect(
                    canvas,
                    rect,
                    style.border_radius.max(4.0),
                    &color_paint(w3cos_std::color::Color::rgb(30, 30, 40), style.opacity),
                );
            }
            if focused && style.border_width > 0.0 {
                let mut focus =
                    color_paint(w3cos_std::color::Color::rgb(108, 92, 231), style.opacity);
                focus.set_style(paint::Style::Stroke);
                focus.set_stroke_width(style.border_width.max(2.0));
                draw_round_rect(canvas, rect, style.border_radius.max(4.0), &focus);
            }
            let color = if value.is_empty() {
                w3cos_std::color::Color::rgb(107, 114, 128)
            } else {
                style.color
            };
            let content = text_content_box(rect, style);
            let y = text_layout::y_for_draw_text_line_centered(
                text,
                style.font_size,
                metrics_font,
                content.y,
                content.height,
            );
            draw_text_line(
                canvas,
                content.x,
                y,
                text,
                style.font_size,
                color,
                style.opacity,
                typeface,
            );
        }
        ComponentKind::Image { .. }
        | ComponentKind::Root
        | ComponentKind::Column
        | ComponentKind::Row
        | ComponentKind::Box
        | ComponentKind::Canvas { .. }
        | ComponentKind::VirtualList { .. } => {}
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
    let content = text_paint_box(rect, style);
    let layout = text_layout::retained_text_paint_layout(
        text,
        content.width,
        style.font_size,
        metrics_font,
        style.white_space,
    );
    if layout.lines.len() == 1 {
        draw_text_ink_in_box(
            canvas,
            content,
            &layout.lines[0],
            layout.ink_bounds[0],
            style,
            typeface,
        );
        return;
    }
    let line_height = style.font_size * style.line_height;
    let top = content.y + (content.height - layout.lines.len() as f32 * line_height).max(0.0) * 0.5;
    for (index, line) in layout.lines.iter().enumerate() {
        let ink = layout.ink_bounds[index];
        let x = aligned_text_x(content, style.text_align, ink.left, ink.width);
        draw_text_line(
            canvas,
            x,
            top + index as f32 * line_height,
            line,
            style.font_size,
            style.color,
            style.opacity,
            typeface,
        );
    }
}

fn draw_centered_text(
    canvas: &Canvas,
    rect: LayoutRect,
    text: &str,
    style: &Style,
    typeface: &Typeface,
    metrics_font: &fontdue::Font,
) {
    let content = text_paint_box(rect, style);
    let ink = text_layout::measure_text_ink_bounds(text, style.font_size, metrics_font, 0.0, 0.0);
    let x = content.x + (content.width - ink.width) * 0.5 - ink.left;
    let y = content.y + (content.height - ink.height) * 0.5 - ink.top;
    draw_text_line(
        canvas,
        x,
        y,
        text,
        style.font_size,
        style.color,
        style.opacity,
        typeface,
    );
}

fn draw_text_ink_in_box(
    canvas: &Canvas,
    rect: LayoutRect,
    text: &str,
    ink: text_layout::InkBounds,
    style: &Style,
    typeface: &Typeface,
) {
    let x = aligned_text_x(rect, style.text_align, ink.left, ink.width);
    let y = rect.y + (rect.height - ink.height) * 0.5 - ink.top;
    draw_text_line(
        canvas,
        x,
        y,
        text,
        style.font_size,
        style.color,
        style.opacity,
        typeface,
    );
}

fn aligned_text_x(rect: LayoutRect, align: TextAlign, ink_left: f32, ink_width: f32) -> f32 {
    match align {
        TextAlign::Right => rect.x + rect.width - ink_width - ink_left,
        TextAlign::Center => rect.x + (rect.width - ink_width) * 0.5 - ink_left,
        TextAlign::Left | TextAlign::Justify => rect.x - ink_left,
    }
}

fn draw_text_line(
    canvas: &Canvas,
    x: f32,
    top: f32,
    text: &str,
    font_size: f32,
    color: w3cos_std::color::Color,
    opacity: f32,
    typeface: &Typeface,
) {
    let font = Font::new(typeface.clone(), font_size);
    canvas.draw_str(
        text,
        (x, top + font_size),
        &font,
        &color_paint(color, opacity),
    );
}

fn text_content_box(rect: LayoutRect, style: &Style) -> LayoutRect {
    let border = style.border_width;
    let padding = style.padding_lengths();
    LayoutRect {
        x: rect.x + padding.left + border,
        y: rect.y + padding.top + border,
        width: (rect.width - padding.left - padding.right - border * 2.0).max(1.0),
        height: (rect.height - padding.top - padding.bottom - border * 2.0).max(0.0),
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

fn draw_round_rect(canvas: &Canvas, rect: LayoutRect, radius: f32, paint: &Paint) {
    canvas.draw_round_rect(to_rect(rect), radius.max(0.0), radius.max(0.0), paint);
}

fn to_rect(rect: LayoutRect) -> Rect {
    Rect::from_xywh(rect.x, rect.y, rect.width.max(0.0), rect.height.max(0.0))
}

fn color_paint(color: w3cos_std::color::Color, opacity: f32) -> Paint {
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_color(Color::from_argb(
        (color.a as f32 * opacity.clamp(0.0, 1.0)).round() as u8,
        color.r,
        color.g,
        color.b,
    ));
    paint
}
