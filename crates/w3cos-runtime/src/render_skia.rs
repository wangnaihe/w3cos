//! Skia replay backend for the retained W3COS paint artifact.
//!
//! This module intentionally consumes the same pre-painted node stream as the
//! Vello and tiny-skia backends. It does not perform layout or invent native
//! widget defaults: CSS-derived geometry and style remain the source of truth.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use skia_safe::canvas::SaveLayerRec;
use skia_safe::{
    AlphaType, BlurStyle, Canvas, Color, Color4f, ColorType, Data, Font, FontMgr, FontStyle, Image,
    ImageFilter, ImageInfo, MaskFilter, Matrix, Paint, PathBuilder, Picture, PictureRecorder,
    RRect, Rect, Surface, TileMode, Typeface, Vector, color_filters, gradient_shader,
    image_filters, images, paint,
};
use w3cos_std::SvgPathCommand;
use w3cos_std::component::ComponentKind;
use w3cos_std::style::{JustifyContent, Style, TextAlign, Transform2D};

use crate::filter::{FilterChain, FilterOp, parse_css_filter};
use crate::layout::LayoutRect;
use crate::paint_artifact::PaintArtifact;
use crate::retained_layers::{
    CompositorOverrides, LayerPaintAction, RetainedLayerTree, layer_css_transform,
    layer_opacity as compositor_layer_opacity, layer_scroll_translation,
};
use crate::text_layout;

const FONT_FALLBACK_CACHE_CAPACITY: usize = 2048;

const IMAGE_TEXTURE_CACHE_LIMIT: usize = 256;

thread_local! {
    static SKIA_IMAGES: RefCell<HashMap<usize, Image>> = RefCell::new(HashMap::new());
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
            host_typeface()
                .or_else(|| primary_typeface(include_bytes!("../assets/Inter-Regular.ttf")))
                .expect("embedded Skia fallback font")
        }
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
            AlphaType::Unpremul,
            None,
        );
        let image = images::raster_from_data(&info, Data::new_copy(pixels), width as usize * 4)?;
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
    pub retained: Option<&'a mut RetainedSkiaCache>,
    pub compositor_overrides: Option<&'a CompositorOverrides>,
    pub scale_factor: f32,
}

#[derive(Default)]
pub struct RetainedSkiaCache {
    tree: RetainedLayerTree,
    pictures: Vec<Picture>,
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
    // Layer recordings must not bake a background clear; composite applies
    // the canvas background, then opacity / transform / scroll.
    if bake_compositor_props {
        canvas.clear(to_skia_color(frame.background, 1.0));
    }
    let mut active_filters = Vec::new();
    for &(idx, rect, kind, style) in frame.nodes {
        let filter_path = effect_path(frame.artifact, idx, bake_compositor_props);
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
            if bake_compositor_props {
                paint.set_alpha_f(effect.opacity.clamp(0.0, 1.0));
            }
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
            bake_compositor_props,
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
        let can_replay = matches!(action, LayerPaintAction::Replay)
            && cache.tree.recordings_valid()
            && cache.pictures.len() == cache.tree.layers.len();
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
        for layer in &cache.tree.layers {
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
            if let Some(picture) = recorder.finish_recording_as_picture(None) {
                pictures.push(picture);
            }
        }
        cache.pictures = pictures;
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
    for (layer, picture) in layers.iter().zip(pictures) {
        let (_, _, clip) = layer_scroll_translation(layer, scroll_info);
        let matrix = compositor_layer_matrix(layer, artifact, scroll_info, overrides, scale_factor);
        let opacity = compositor_layer_opacity(layer, artifact, overrides);
        let save = canvas.save();
        if let Some(clip_rect) = clip {
            canvas.clip_rect(to_rect(clip_rect), None, Some(false));
        }
        if opacity < 0.999 {
            let mut paint = Paint::default();
            paint.set_alpha_f(opacity);
            canvas.save_layer(&SaveLayerRec::default().paint(&paint));
        }
        canvas.draw_picture(picture, Some(&matrix), None);
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
    let transform = if bake_compositor_props {
        style.transform
    } else {
        Transform2D::IDENTITY
    };
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
        draw_rounded_rect(
            canvas,
            shadow_rect,
            style.border_corner_radii().map(|radius| radius + spread),
            &paint,
        );
    }

    let bg = style.background;
    if bg.a > 0 {
        draw_rounded_rect(
            canvas,
            rect,
            style.border_corner_radii(),
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
                draw_decoded_image(canvas, rect, &raster, style.opacity, true);
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
        if effect.filter.is_some() || (bake_compositor_props && effect.opacity < 0.999) {
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
    // Untransformed replaced images own their pixel coverage just like a
    // zero-radius CSS background. Browser rasterizers snap those outer edges
    // instead of blending the image with the page at fractional layout
    // coordinates; interpolation remains inside the destination rectangle.
    draw_image_with_edge_antialiasing(canvas, rect, src, opacity, false);
}

fn draw_image_with_edge_antialiasing(
    canvas: &Canvas,
    rect: LayoutRect,
    src: &str,
    opacity: f32,
    anti_alias: bool,
) {
    let Some(decoded) = crate::image_loader::get_or_load(src) else {
        return;
    };
    draw_decoded_image(canvas, rect, &decoded, opacity, anti_alias);
}

fn draw_decoded_image(
    canvas: &Canvas,
    rect: LayoutRect,
    decoded: &crate::image_loader::DecodedImage,
    opacity: f32,
    anti_alias: bool,
) {
    let Some(image) = cached_skia_image(decoded) else {
        return;
    };
    let mut paint = Paint::default();
    paint.set_anti_alias(anti_alias);
    paint.set_alpha_f(opacity.clamp(0.0, 1.0));
    canvas.draw_image_rect(image, None, to_rect(rect), &paint);
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
        data: snapshot.pixels,
    };
    draw_decoded_image(canvas, rect, &decoded, opacity, true);
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
    _metrics_font: &fontdue::Font,
) {
    let content = text_paint_box(rect, style);
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
    let layout = text_layout::retained_text_paint_layout_with_run_width(
        text,
        content.width,
        style.font_size,
        style.white_space,
        registry.cascade_cache_key(style, text) ^ 0x534b_4941_5445_5801,
        |line| measure_skia_text_advance(line, typeface, style),
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
        let line_height = style.font_size * style.line_height;
        let x = aligned_text_x(content, effective_text_align(style), ink.left, ink.width);
        let top = content.y + (content.height - line_height).max(0.0) * 0.5;
        draw_text_line(
            canvas,
            x,
            top,
            &layout.lines[0],
            style.font_size,
            style.color,
            style.opacity,
            typeface,
            style,
        );
        if let Some(save) = clip_save {
            canvas.restore_to_count(save);
        }
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
    if let Some(save) = clip_save {
        canvas.restore_to_count(save);
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
    let font_text = text_layout::font_render_text(text);
    for run in css_font_runs(font_text.as_ref(), typeface, style) {
        let font = Font::new(run.typeface, font_size);
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

    let font_text = text_layout::font_render_text(text);
    let runs = style.map_or_else(
        || fallback_font_runs(font_text.as_ref(), typeface, font_weight),
        |style| css_font_runs(font_text.as_ref(), typeface, style),
    );
    for run in runs {
        let font = Font::new(run.typeface, font_size);
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

fn measure_skia_text_advance(text: &str, typeface: &Typeface, style: &Style) -> f32 {
    let font_text = text_layout::font_render_text(text);
    css_font_runs(font_text.as_ref(), typeface, style)
        .into_iter()
        .map(|run| {
            Font::new(run.typeface, style.font_size)
                .measure_str(run.text, None)
                .0
        })
        .sum()
}

pub(crate) fn measure_skia_text_intrinsic_size(text: &str, style: &Style) -> (f32, f32) {
    let registered = registered_typeface(style);
    INTRINSIC_PRIMARY_TYPEFACE.with(|intrinsic| {
        let primary = registered
            .as_ref()
            .map(|(_, typeface)| typeface)
            .unwrap_or(intrinsic);
        let lines = text_layout::wrap_text_with_run_width(
            text,
            f32::MAX / 4.0,
            style.white_space,
            |line| measure_skia_text_advance(line, primary, style),
        );
        let width = lines
            .iter()
            .map(|line| measure_skia_text_advance(line, primary, style))
            .fold(0.0_f32, f32::max);
        let padding = style.padding_lengths();
        // Skia's macOS fallback typeface exposes a 32.55px top-to-bottom
        // metric at 16px, but that is a glyph coverage bound, not the used
        // CSS line box. Browser block/inline layout advances by the computed
        // line-height; fallback-specific CJK expansion is applied centrally
        // by `browser_normal_cjk_height` in layout.rs.
        let content_height = text_layout::used_text_line_count(text, style, &lines) as f32
            * style.font_size
            * style.line_height;
        (
            width + padding.left + padding.right,
            content_height + padding.top + padding.bottom,
        )
    })
}

pub(crate) fn measure_skia_wrapped_text_height(text: &str, width: f32, style: &Style) -> f32 {
    let registered = registered_typeface(style);
    INTRINSIC_PRIMARY_TYPEFACE.with(|intrinsic| {
        let primary = registered
            .as_ref()
            .map(|(_, typeface)| typeface)
            .unwrap_or(intrinsic);
        let padding = style.padding_lengths();
        let inner_width = (width - padding.left - padding.right).max(1.0);
        let lines =
            text_layout::wrap_text_with_run_width(text, inner_width, style.white_space, |line| {
                measure_skia_text_advance(line, primary, style)
            });
        let used_line_count = text_layout::used_text_line_count(text, style, &lines);
        let content_height = if used_line_count == 1 {
            measure_skia_text_intrinsic_size(&lines[0], style).1 - padding.top - padding.bottom
        } else {
            used_line_count as f32 * style.font_size * style.line_height
        };
        content_height + padding.top + padding.bottom
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
    if font_weight < 600 && primary.unichar_to_glyph(character as i32) != 0 {
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
                &primary.family_name(),
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
    text_content_box(rect, style)
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
                for tile in &layer.tiles {
                    // The background painting area owns edge coverage. Keeping
                    // tile edges non-AA avoids double-blended outer edges and
                    // seams between repeated tiles.
                    draw_image_with_edge_antialiasing(canvas, *tile, &layer.source, opacity, false);
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
    canvas.draw_rrect(RRect::new_rect_radii(to_rect(rect), &radii), paint);
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
    fn skia_uses_regular_space_metrics_for_non_breaking_space() {
        let primary = FontMgr::default().new_from_data(TEST_FONT, None).unwrap();
        let style = Style::default();
        let space = measure_skia_text_advance(" ", &primary, &style);
        let non_breaking_space = measure_skia_text_advance("\u{00a0}", &primary, &style);
        assert!((space - non_breaking_space).abs() < 0.01);
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
