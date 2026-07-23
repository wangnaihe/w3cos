//! Skia replay backend for the retained W3COS paint artifact.
//!
//! This module intentionally consumes the same pre-painted node stream as the
//! Vello and tiny-skia backends. It does not perform layout or invent native
//! widget defaults: CSS-derived geometry and style remain the source of truth.

use std::collections::HashMap;

use skia_safe::canvas::SaveLayerRec;
use skia_safe::{
    AlphaType, BlurStyle, Canvas, Color, Color4f, ColorType, Data, Font, FontMgr, ImageFilter,
    ImageInfo, MaskFilter, Paint, Rect, Surface, TileMode, Typeface, color_filters,
    gradient_shader, image_filters, images, paint,
};
use w3cos_std::component::ComponentKind;
use w3cos_std::style::{JustifyContent, Style, TextAlign};

use crate::filter::{FilterChain, FilterOp, parse_css_filter};
use crate::layout::LayoutRect;
use crate::paint_artifact::PaintArtifact;
use crate::text_layout;

pub(crate) struct ReplayFrame<'a> {
    pub nodes: &'a [(usize, LayoutRect, &'a ComponentKind, &'a Style)],
    pub metrics_font: &'a fontdue::Font,
    pub scroll_info: &'a [Option<(f32, f32, LayoutRect)>],
    pub text_input_values: &'a HashMap<usize, String>,
    pub focused_index: Option<usize>,
    pub background: w3cos_std::color::Color,
    pub artifact: Option<&'a PaintArtifact>,
}

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
        background: w3cos_std::color::Color,
        artifact: Option<&PaintArtifact>,
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

#[allow(clippy::too_many_arguments)]
pub(crate) fn replay_frame(canvas: &Canvas, typeface: &Typeface, frame: ReplayFrame<'_>) {
    canvas.clear(to_skia_color(frame.background, 1.0));
    let mut active_filters = Vec::new();
    for &(idx, rect, kind, style) in frame.nodes {
        let filter_path = effect_path(frame.artifact, idx);
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
            let mut paint = Paint::default();
            paint.set_alpha_f(effect.opacity.clamp(0.0, 1.0));
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

        let save = canvas.save();
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
        let normalized_style = (frame.artifact.is_some() && style.opacity < 0.999).then(|| {
            let mut normalized = style.clone();
            normalized.opacity = 1.0;
            normalized
        });
        render_node(
            canvas,
            idx,
            rect,
            kind,
            normalized_style.as_ref().unwrap_or(style),
            typeface,
            frame.metrics_font,
            frame.text_input_values.get(&idx).map(String::as_str),
            frame.focused_index == Some(idx),
        );
        if matches!(local_filter, Some(Some(_))) {
            canvas.restore();
        }
        canvas.restore_to_count(save);
    }
    for _ in 0..active_filters.len() {
        canvas.restore();
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
        background: w3cos_std::color::Color,
        artifact: Option<&PaintArtifact>,
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
    _focused: bool,
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
    if let Some(background_image) = style.background_image.as_deref() {
        draw_background_image(
            canvas,
            rect,
            style.border_radius,
            background_image,
            style.opacity,
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
                style.font_weight,
            );
        }
        ComponentKind::Image { src } => draw_image(canvas, rect, src, style.opacity),
        ComponentKind::Canvas { .. } => draw_canvas(canvas, client_index, rect, style.opacity),
        ComponentKind::Root
        | ComponentKind::Column
        | ComponentKind::Row
        | ComponentKind::Box
        | ComponentKind::VirtualList { .. } => {}
    }
}

fn effect_path(artifact: Option<&PaintArtifact>, client_index: usize) -> Vec<usize> {
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
        if effect.opacity < 0.999 || effect.filter.is_some() {
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

fn draw_image(canvas: &Canvas, rect: LayoutRect, src: &str, opacity: f32) {
    let Some(decoded) = crate::image_loader::get_or_load(src) else {
        return;
    };
    draw_rgba_pixels(
        canvas,
        rect,
        decoded.width,
        decoded.height,
        decoded.data.as_slice(),
        opacity,
    );
}

fn draw_canvas(canvas: &Canvas, client_index: usize, rect: LayoutRect, opacity: f32) {
    let Some(snapshot) = crate::canvas2d::surface_snapshot(client_index) else {
        return;
    };
    draw_rgba_pixels(
        canvas,
        rect,
        snapshot.width,
        snapshot.height,
        snapshot.pixels.as_slice(),
        opacity,
    );
}

fn draw_rgba_pixels(
    canvas: &Canvas,
    rect: LayoutRect,
    width: u32,
    height: u32,
    pixels: &[u8],
    opacity: f32,
) {
    if width == 0 || height == 0 || pixels.len() != width as usize * height as usize * 4 {
        return;
    }
    let info = ImageInfo::new(
        (width as i32, height as i32),
        ColorType::RGBA8888,
        AlphaType::Unpremul,
        None,
    );
    let Some(image) = images::raster_from_data(&info, Data::new_copy(pixels), width as usize * 4)
    else {
        return;
    };
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_alpha_f(opacity.clamp(0.0, 1.0));
    canvas.draw_image_rect(image, None, to_rect(rect), &paint);
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
        let x = aligned_text_x(content, effective_text_align(style), ink.left, ink.width);
        draw_text_line(
            canvas,
            x,
            top + index as f32 * line_height,
            line,
            style.font_size,
            style.color,
            style.opacity,
            typeface,
            style.font_weight,
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
        style.font_weight,
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
    let x = aligned_text_x(rect, effective_text_align(style), ink.left, ink.width);
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
        style.font_weight,
    );
}

fn effective_text_align(style: &Style) -> TextAlign {
    // DOM text content is lowered into the host Text component instead of an
    // anonymous flex child. Preserve the browser behavior of centering that
    // anonymous child when the host itself is a centered flex container.
    if matches!(style.justify_content, JustifyContent::Center) {
        TextAlign::Center
    } else {
        style.text_align
    }
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
    font_weight: u16,
) {
    let mut font = Font::new(typeface.clone(), font_size);
    font.set_embolden(font_weight >= 600);
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
    if style.background.a > 0 || style.background_image.is_some() {
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

#[derive(Clone, Copy)]
struct GradientStop {
    color: w3cos_std::color::Color,
    position: Option<f32>,
}

fn draw_background_image(
    canvas: &Canvas,
    rect: LayoutRect,
    radius: f32,
    value: &str,
    opacity: f32,
) {
    // CSS paints the first listed background on top of the following layers.
    for layer in split_top_level(value, ',').into_iter().rev() {
        let mut paint = Paint::default();
        paint.set_anti_alias(true);
        paint.set_alpha_f(opacity.clamp(0.0, 1.0));
        let shader = if let Some(arguments) = function_arguments(layer, "linear-gradient") {
            linear_gradient_shader(rect, arguments)
        } else if let Some(arguments) = function_arguments(layer, "radial-gradient") {
            radial_gradient_shader(rect, arguments)
        } else {
            None
        };
        if let Some(shader) = shader {
            paint.set_shader(shader);
            draw_round_rect(canvas, rect, radius, &paint);
        }
    }
}

fn linear_gradient_shader(rect: LayoutRect, arguments: &str) -> Option<skia_safe::Shader> {
    let mut parts = split_top_level(arguments, ',');
    let has_angle = parts
        .first()
        .is_some_and(|part| part.trim().ends_with("deg"));
    let angle = parts
        .first()
        .and_then(|part| part.trim().strip_suffix("deg"))
        .and_then(|value| value.trim().parse::<f32>().ok())
        .unwrap_or(180.0);
    if has_angle {
        parts.remove(0);
    }
    let (colors, positions) = gradient_colors_and_positions(&parts)?;
    let radians = angle.to_radians();
    let direction = (radians.sin(), -radians.cos());
    let center = (rect.x + rect.width * 0.5, rect.y + rect.height * 0.5);
    let extent = direction.0.abs() * rect.width * 0.5 + direction.1.abs() * rect.height * 0.5;
    let start = (
        center.0 - direction.0 * extent,
        center.1 - direction.1 * extent,
    );
    let end = (
        center.0 + direction.0 * extent,
        center.1 + direction.1 * extent,
    );
    gradient_shader::linear(
        (start, end),
        colors.as_slice(),
        positions.as_slice(),
        TileMode::Clamp,
        None,
        None,
    )
}

fn radial_gradient_shader(rect: LayoutRect, arguments: &str) -> Option<skia_safe::Shader> {
    let mut parts = split_top_level(arguments, ',');
    let mut center = (rect.x + rect.width * 0.5, rect.y + rect.height * 0.5);
    if let Some(header) = parts.first().copied()
        && !parse_gradient_stop(header).is_some()
    {
        if let Some(at) = header.find(" at ") {
            let coords: Vec<&str> = header[at + 4..].split_whitespace().collect();
            if coords.len() >= 2 {
                center.0 = rect.x + parse_percent(coords[0]).unwrap_or(0.5) * rect.width;
                center.1 = rect.y + parse_percent(coords[1]).unwrap_or(0.5) * rect.height;
            }
        }
        parts.remove(0);
    }
    let (colors, positions) = gradient_colors_and_positions(&parts)?;
    let radius = [
        (center.0 - rect.x).hypot(center.1 - rect.y),
        (center.0 - (rect.x + rect.width)).hypot(center.1 - rect.y),
        (center.0 - rect.x).hypot(center.1 - (rect.y + rect.height)),
        (center.0 - (rect.x + rect.width)).hypot(center.1 - (rect.y + rect.height)),
    ]
    .into_iter()
    .fold(0.0_f32, f32::max);
    gradient_shader::radial(
        center,
        radius.max(1.0),
        colors.as_slice(),
        positions.as_slice(),
        TileMode::Clamp,
        None,
        None,
    )
}

fn gradient_colors_and_positions(parts: &[&str]) -> Option<(Vec<Color>, Vec<f32>)> {
    let stops: Vec<GradientStop> = parts
        .iter()
        .filter_map(|part| parse_gradient_stop(part))
        .collect();
    if stops.len() < 2 {
        return None;
    }
    let colors = stops
        .iter()
        .map(|stop| to_skia_color(stop.color, 1.0))
        .collect();
    let mut positions: Vec<Option<f32>> = stops.iter().map(|stop| stop.position).collect();
    if positions.first().is_some_and(Option::is_none) {
        positions[0] = Some(0.0);
    }
    let last = positions.len() - 1;
    if positions[last].is_none() {
        positions[last] = Some(1.0);
    }
    let mut anchor = 0;
    while anchor < last {
        let next = (anchor + 1..=last)
            .find(|&index| positions[index].is_some())
            .unwrap_or(last);
        let from = positions[anchor].unwrap_or(0.0);
        let to = positions[next].unwrap_or(1.0).max(from);
        for index in anchor + 1..next {
            let t = (index - anchor) as f32 / (next - anchor) as f32;
            positions[index] = Some(from + (to - from) * t);
        }
        anchor = next;
    }
    Some((colors, positions.into_iter().map(Option::unwrap).collect()))
}

fn parse_gradient_stop(value: &str) -> Option<GradientStop> {
    let parts = split_css_whitespace(value.trim());
    let color = w3cos_std::color::Color::from_css(parts.first()?)?;
    let position = parts.get(1).and_then(|value| parse_percent(value));
    Some(GradientStop { color, position })
}

fn parse_percent(value: &str) -> Option<f32> {
    value
        .trim()
        .strip_suffix('%')?
        .trim()
        .parse::<f32>()
        .ok()
        .map(|value| (value / 100.0).clamp(0.0, 1.0))
}

fn function_arguments<'a>(value: &'a str, name: &str) -> Option<&'a str> {
    value
        .trim()
        .strip_prefix(name)?
        .strip_prefix('(')?
        .strip_suffix(')')
}

fn split_top_level(value: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut depth = 0_u32;
    for (index, ch) in value.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ if ch == separator && depth == 0 => {
                parts.push(value[start..index].trim());
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(value[start..].trim());
    parts
}

fn split_css_whitespace(value: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = None;
    let mut depth = 0_u32;
    for (index, ch) in value.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ => {}
        }
        if ch.is_whitespace() && depth == 0 {
            if let Some(from) = start.take() {
                parts.push(&value[from..index]);
            }
        } else if start.is_none() {
            start = Some(index);
        }
    }
    if let Some(from) = start {
        parts.push(&value[from..]);
    }
    parts
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

    fn test_font() -> fontdue::Font {
        fontdue::Font::from_bytes(TEST_FONT, fontdue::FontSettings::default()).unwrap()
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
        let layers = split_top_level(value, ',');
        assert_eq!(layers.len(), 2);

        let radial = function_arguments(layers[0], "radial-gradient").unwrap();
        let radial_parts = split_top_level(radial, ',');
        assert_eq!(radial_parts.len(), 3);
        let stop = parse_gradient_stop(radial_parts[1]).unwrap();
        assert_eq!(stop.color, w3cos_std::color::Color::rgba(22, 119, 255, 46));

        let linear = function_arguments(layers[1], "linear-gradient").unwrap();
        let linear_parts = split_top_level(linear, ',');
        let (_, positions) = gradient_colors_and_positions(&linear_parts[1..]).unwrap();
        assert_eq!(positions, vec![0.0, 1.0]);
    }

    #[test]
    fn centered_flex_host_centers_lowered_text_content() {
        let mut style = Style::default();
        style.justify_content = JustifyContent::Center;
        assert_eq!(effective_text_align(&style), TextAlign::Center);
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
                },
                PaintNode {
                    kind: red_kind.clone(),
                    style: red_style.clone(),
                    parent: Some(0),
                },
                PaintNode {
                    kind: blue_kind.clone(),
                    style: blue_style.clone(),
                    parent: Some(0),
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
            )
            .unwrap();
        let left = &pixels[(2 * 8 + 2) * 4..(2 * 8 + 2) * 4 + 4];
        let right = &pixels[(2 * 8 + 6) * 4..(2 * 8 + 6) * 4 + 4];
        assert_eq!(left, &[0, 255, 255, 255]);
        assert_eq!(right, &[255, 255, 0, 255]);
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
            )
            .unwrap();
        assert!((pixels[0] as i16 - 10).abs() <= 2);
        assert!((pixels[1] as i16 - 20).abs() <= 2);
        assert!(pixels[2] >= 238);
        assert_eq!(pixels[3], 255);
    }
}
