//! Skia replay backend for the retained W3COS paint artifact.
//!
//! This module intentionally consumes the same pre-painted node stream as the
//! Vello and tiny-skia backends. It does not perform layout or invent native
//! widget defaults: CSS-derived geometry and style remain the source of truth.

use std::cell::RefCell;
use std::collections::HashMap;

use skia_safe::canvas::SaveLayerRec;
use skia_safe::{
    AlphaType, BlurStyle, Canvas, Color, Color4f, ColorType, Data, Font, FontMgr, FontStyle,
    ImageFilter, ImageInfo, MaskFilter, Paint, PathBuilder, RRect, Rect, Surface, TileMode,
    Typeface, color_filters, gradient_shader, image_filters, images, paint,
};
use w3cos_std::SvgPathCommand;
use w3cos_std::component::ComponentKind;
use w3cos_std::style::{JustifyContent, Style, TextAlign};

use crate::filter::{FilterChain, FilterOp, parse_css_filter};
use crate::layout::LayoutRect;
use crate::paint_artifact::PaintArtifact;
use crate::text_layout;

const FONT_FALLBACK_CACHE_CAPACITY: usize = 2048;

thread_local! {
    /// System font matching is comparatively expensive on Apple platforms.
    /// Cache Skia typeface references for characters missing from the primary
    /// face; this does not copy the underlying system font into application memory.
    static FONT_FALLBACK_CACHE: RefCell<HashMap<(u32, char, u16), Option<Typeface>>> =
        RefCell::new(HashMap::new());
    static INTRINSIC_PRIMARY_TYPEFACE: Typeface = {
        #[cfg(test)]
        {
            primary_typeface(include_bytes!("../assets/Inter-Regular.ttf"))
                .expect("Skia test font")
        }
        #[cfg(not(test))]
        {
            host_typeface().expect("host Skia font")
        }
    };
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
    FontMgr::default().new_from_data(host.data.as_slice(), Some(host.index as usize))
}

fn registered_typeface(style: &Style) -> Option<(crate::font_face::LoadedFont, Typeface)> {
    let loaded = crate::font_face::FontRegistry::global().resolve_style(style)?;
    loaded.parsed()?;
    let typeface = loaded.skia_typeface()?;
    Some((loaded, typeface))
}

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
        let typeface = primary_typeface(font_bytes)?;
        Some(Self {
            surface: None,
            size: (0, 0),
            rgba: Vec::new(),
            typeface,
        })
    }

    pub fn new_host() -> Option<Self> {
        let typeface = host_typeface()?;
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
    if style.background_image.is_some() {
        draw_background_image(canvas, rect, style.border_radius, style, style.opacity);
    }
    let has_edge_border = style.border_top_width.is_some()
        || style.border_right_width.is_some()
        || style.border_bottom_width.is_some()
        || style.border_left_width.is_some()
        || style.border_top_color.is_some()
        || style.border_right_color.is_some()
        || style.border_bottom_color.is_some()
        || style.border_left_color.is_some();
    if !has_edge_border && style.border_width > 0.0 && style.border_color.a > 0 {
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
        let edges = [
            LayoutRect {
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: widths[0],
            },
            LayoutRect {
                x: rect.x + rect.width - widths[1],
                y: rect.y,
                width: widths[1],
                height: rect.height,
            },
            LayoutRect {
                x: rect.x,
                y: rect.y + rect.height - widths[2],
                width: rect.width,
                height: widths[2],
            },
            LayoutRect {
                x: rect.x,
                y: rect.y,
                width: widths[3],
                height: rect.height,
            },
        ];
        for ((edge, width), color) in edges.into_iter().zip(widths).zip(colors) {
            if width > 0.0 && color.a > 0 {
                draw_round_rect(canvas, edge, 0.0, &color_paint(color, style.opacity));
            }
        }
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
        ComponentKind::Image { src } => draw_image(canvas, rect, src, style.opacity),
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
        ComponentKind::SvgDocument {
            source,
            width,
            height,
            ..
        } => {
            if let Some(raster) = crate::svg_renderer::get_or_render(source, *width, *height) {
                draw_rgba_pixels(
                    canvas,
                    rect,
                    raster.width,
                    raster.height,
                    raster.data.as_slice(),
                    style.opacity,
                );
            }
        }
        ComponentKind::Root
        | ComponentKind::Column
        | ComponentKind::Row
        | ComponentKind::Box
        | ComponentKind::VirtualList { .. } => {}
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
    let registry = crate::font_face::FontRegistry::global();
    let layout = text_layout::retained_text_paint_layout_with(
        text,
        content.width,
        style.font_size,
        style.white_space,
        registry.cascade_cache_key(style, text) ^ 0x534b_4941_5445_5801,
        |character| registry.style_char_advance(style, character, style.font_size, metrics_font),
        |line| {
            measure_skia_text_ink_bounds(
                line,
                style.font_size,
                typeface,
                style.font_weight,
                Some(style),
            )
        },
    );
    if layout.lines.len() == 1 {
        let ink = measure_skia_text_ink_bounds(
            &layout.lines[0],
            style.font_size,
            typeface,
            style.font_weight,
            Some(style),
        );
        draw_text_ink_in_box(canvas, content, &layout.lines[0], ink, style, typeface);
        return;
    }
    let line_height = style.font_size * style.line_height;
    let top = content.y + (content.height - layout.lines.len() as f32 * line_height).max(0.0) * 0.5;
    for (index, line) in layout.lines.iter().enumerate() {
        let ink = measure_skia_text_ink_bounds(
            line,
            style.font_size,
            typeface,
            style.font_weight,
            Some(style),
        );
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
            style,
        );
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
        style,
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
        style,
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
    style: &Style,
) -> f32 {
    let paint = color_paint(color, opacity);
    let mut cursor_x = x;
    for run in css_font_runs(text, typeface, style) {
        let mut font = Font::new(run.typeface, font_size);
        font.set_embolden(style.font_weight >= 600);
        canvas.draw_str(run.text, (cursor_x, top + font_size), &font, &paint);
        cursor_x += font.measure_str(run.text, Some(&paint)).0;
    }
    cursor_x - x
}

fn measure_skia_text_ink_bounds(
    text: &str,
    font_size: f32,
    typeface: &Typeface,
    font_weight: u16,
    style: Option<&Style>,
) -> text_layout::InkBounds {
    let mut cursor_x = 0.0_f32;
    let mut left = f32::MAX;
    let mut top = f32::MAX;
    let mut right = f32::MIN;
    let mut bottom = f32::MIN;
    let mut saw_ink = false;

    let runs = style.map_or_else(
        || fallback_font_runs(text, typeface, font_weight),
        |style| css_font_runs(text, typeface, style),
    );
    for run in runs {
        let mut font = Font::new(run.typeface, font_size);
        font.set_embolden(font_weight >= 600);
        let (advance, bounds) = font.measure_str(run.text, None);
        if bounds.width() > 0.0 || bounds.height() > 0.0 {
            saw_ink = true;
            left = left.min(cursor_x + bounds.left);
            top = top.min(font_size + bounds.top);
            right = right.max(cursor_x + bounds.right);
            bottom = bottom.max(font_size + bounds.bottom);
        }
        cursor_x += advance;
    }

    if !saw_ink {
        return text_layout::InkBounds::empty();
    }

    text_layout::InkBounds {
        left,
        top,
        width: (right - left).max(0.0),
        height: (bottom - top).max(0.0),
    }
}

pub(crate) fn measure_skia_text_intrinsic_size(text: &str, style: &Style) -> (f32, f32) {
    let registered = registered_typeface(style);
    INTRINSIC_PRIMARY_TYPEFACE.with(|intrinsic| {
        let primary = registered
            .as_ref()
            .map(|(_, typeface)| typeface)
            .unwrap_or(intrinsic);
        let mut width = 0.0_f32;
        let mut line_spacing = 0.0_f32;
        for run in css_font_runs(text, primary, style) {
            let mut font = Font::new(run.typeface, style.font_size);
            font.set_embolden(style.font_weight >= 600);
            width += font.measure_str(run.text, None).0;
            let (spacing, metrics) = font.metrics();
            // CSS `normal` uses the font's full em line box, not only Skia's
            // baseline spacing. `BOUNDS_INVALID` only describes the aggregate
            // glyph bounds; top/bottom line metrics are still populated.
            let normal_line_height = (metrics.bottom - metrics.top).max(spacing);
            line_spacing = line_spacing.max(normal_line_height);
        }
        if text.is_empty() {
            let font = Font::new(primary.clone(), style.font_size);
            let (spacing, metrics) = font.metrics();
            line_spacing = (metrics.bottom - metrics.top).max(spacing);
        }
        let padding = style.padding_lengths();
        (
            width + padding.left + padding.right,
            line_spacing.max(style.font_size * style.line_height) + padding.top + padding.bottom,
        )
    })
}

struct FallbackFontRun<'a> {
    text: &'a str,
    typeface: Typeface,
}

fn css_font_runs<'a>(text: &'a str, primary: &Typeface, style: &Style) -> Vec<FallbackFontRun<'a>> {
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
    if primary.unichar_to_glyph(character as i32) != 0 {
        return primary.clone();
    }

    let key = (primary.unique_id(), character, font_weight);
    let cached = FONT_FALLBACK_CACHE.with(|cache| cache.borrow().get(&key).cloned());
    let fallback = match cached {
        Some(cached) => cached,
        None => {
            let style = if font_weight >= 600 {
                FontStyle::bold()
            } else {
                FontStyle::normal()
            };
            let matched = FontMgr::default().match_family_style_character(
                "",
                style,
                &["en", "zh-Hans"],
                character as i32,
            );
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

fn draw_background_image(
    canvas: &Canvas,
    rect: LayoutRect,
    _radius: f32,
    style: &Style,
    opacity: f32,
) {
    // CSS paints the first listed background on top of the following layers.
    for layer in crate::background_image::background_paint_layers(style, rect)
        .into_iter()
        .rev()
    {
        let clip = match &layer {
            crate::background_image::BackgroundPaintLayer::Raster(layer) => layer.clip,
            crate::background_image::BackgroundPaintLayer::Gradient(layer) => layer.geometry.clip,
        };
        let save = canvas.save();
        canvas.clip_rrect(
            RRect::new_rect_xy(to_rect(clip.rect), clip.radius, clip.radius),
            None,
            Some(true),
        );
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
                for tile in &layer.tiles {
                    draw_image(canvas, *tile, &layer.source, opacity);
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
