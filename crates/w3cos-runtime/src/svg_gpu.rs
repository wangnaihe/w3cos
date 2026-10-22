//! Direct GPU vector path for SVG on the wgpu/Vello backend.
//!
//! The CPU tile raster in [`crate::svg_renderer`] stays the fallback. Each
//! painted element is either encoded as Vello geometry or rasterized on its
//! own to a pixmap. The choice is recorded with a reason. `W3COS_SVG_CPU_TILES=1`
//! forces the full-document 32px tile upload even when Vello is active.
//!
//! Painting follows the usvg tree, which is the SVG2 geometry resvg paints:
//! paint order, fill rule, object-bounding-box gradients, `clip-path`
//! intersection, group opacity, and `mix-blend-mode`.

use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::Arc;

use resvg::tiny_skia::Pixmap;
use resvg::usvg::{self, Node, PaintOrder};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Cap, Join, Rect, Stroke};
use vello::peniko::{
    BlendMode, Blob, Color, Compose, Extend, Fill, Gradient, ImageAlphaType, ImageBrush, ImageData,
    ImageFormat, ImageQuality, Mix,
};

use crate::layout::LayoutRect;

const FALLBACK_PAD: f32 = 2.0;
const MAX_FALLBACK_EDGE: u32 = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SvgPaintMode {
    /// Vector draws unless `W3COS_SVG_CPU_TILES=1`.
    Auto,
    /// Encode supported elements as GPU vectors.
    GpuVector,
    /// Rasterize the whole document to CPU pixmap tiles and upload them.
    CpuTiles,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SvgGpuRoute {
    Vector,
    CpuPixmap,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SvgElementDecision {
    pub element: String,
    pub route: SvgGpuRoute,
    pub reason: &'static str,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SvgGpuFrame {
    pub vector_elements: u64,
    pub fallback_elements: u64,
    pub upload_bytes: u64,
    pub decisions: Vec<SvgElementDecision>,
}

thread_local! {
    static LOGGED: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

/// Draw inline SVG into `scene` for the Vello backend.
pub(crate) fn draw_inline_svg(
    scene: &mut Scene,
    source: &str,
    rect: LayoutRect,
    dpi: Affine,
) -> SvgGpuFrame {
    draw_inline_svg_mode(scene, source, rect, dpi, SvgPaintMode::Auto)
}

pub(crate) fn draw_inline_svg_mode(
    scene: &mut Scene,
    source: &str,
    rect: LayoutRect,
    dpi: Affine,
    mode: SvgPaintMode,
) -> SvgGpuFrame {
    if !rect.width.is_finite()
        || !rect.height.is_finite()
        || rect.width <= 0.0
        || rect.height <= 0.0
    {
        return SvgGpuFrame::default();
    }
    let width = rect.width.ceil().max(1.0) as u32;
    let height = rect.height.ceil().max(1.0) as u32;
    if !vector_mode(mode) {
        return draw_cpu_tiles(scene, source, rect, dpi, width, height);
    }
    let Some(mut frame) = crate::svg_renderer::with_inline_tree(source, width, height, |tree| {
        paint_tree(scene, tree, rect, dpi)
    }) else {
        let mut frame = SvgGpuFrame::default();
        push_decision(
            &mut frame,
            "svg",
            SvgGpuRoute::CpuPixmap,
            "SVG subtree could not be parsed",
        );
        log_frame(&frame);
        return frame;
    };
    if frame.decisions.is_empty() && frame.vector_elements == 0 && frame.fallback_elements == 0 {
        push_decision(
            &mut frame,
            "svg",
            SvgGpuRoute::Vector,
            "SVG tree has no painted elements",
        );
    }
    log_frame(&frame);
    frame
}

fn vector_mode(mode: SvgPaintMode) -> bool {
    match mode {
        SvgPaintMode::CpuTiles => false,
        SvgPaintMode::GpuVector => true,
        SvgPaintMode::Auto => std::env::var("W3COS_SVG_CPU_TILES").ok().as_deref() != Some("1"),
    }
}

fn draw_cpu_tiles(
    scene: &mut Scene,
    source: &str,
    rect: LayoutRect,
    dpi: Affine,
    width: u32,
    height: u32,
) -> SvgGpuFrame {
    let mut frame = SvgGpuFrame::default();
    let Some(image) = crate::svg_renderer::get_or_render_inline(source, width, height) else {
        push_decision(
            &mut frame,
            "svg",
            SvgGpuRoute::CpuPixmap,
            "SVG subtree could not be parsed",
        );
        log_frame(&frame);
        return frame;
    };
    frame.upload_bytes = image.data.len() as u64;
    frame.fallback_elements = 1;
    push_decision(
        &mut frame,
        "svg",
        SvgGpuRoute::CpuPixmap,
        "CPU pixmap tiles are the selected SVG path",
    );
    draw_straight_image(
        scene,
        dpi * Affine::translate((rect.x as f64, rect.y as f64))
            * Affine::scale_non_uniform(
                rect.width as f64 / image.width.max(1) as f64,
                rect.height as f64 / image.height.max(1) as f64,
            ),
        image.width,
        image.height,
        image.data,
        ImageQuality::Low,
    );
    log_frame(&frame);
    frame
}

fn paint_tree(scene: &mut Scene, tree: &usvg::Tree, rect: LayoutRect, dpi: Affine) -> SvgGpuFrame {
    let canvas_w = tree.size().width().max(1.0);
    let canvas_h = tree.size().height().max(1.0);
    let viewport = dpi
        * Affine::translate((rect.x as f64, rect.y as f64))
        * Affine::scale_non_uniform(
            rect.width as f64 / canvas_w as f64,
            rect.height as f64 / canvas_h as f64,
        );
    let mut walk = Walk {
        scene,
        viewport,
        frame: SvgGpuFrame::default(),
        index: 0,
        silent: false,
        space: TreeSpace {
            abs_to_outer: Affine::IDENTITY,
            canvas_w,
            canvas_h,
        },
    };
    let clip = Rect::new(0.0, 0.0, canvas_w as f64, canvas_h as f64);
    walk.scene.push_clip_layer(Fill::NonZero, viewport, &clip);
    walk.paint_group(tree.root(), Affine::IDENTITY, None);
    walk.scene.pop_layer();
    walk.frame
}

#[derive(Clone, Copy)]
struct TreeSpace {
    /// Maps this tree's absolute canvas (post-viewBox) into the outer canvas.
    abs_to_outer: Affine,
    canvas_w: f32,
    canvas_h: f32,
}

struct Walk<'a> {
    scene: &'a mut Scene,
    viewport: Affine,
    frame: SvgGpuFrame,
    index: u32,
    silent: bool,
    space: TreeSpace,
}

impl Walk<'_> {
    fn draw_group(&mut self, group: &usvg::Group, to_canvas: Affine) {
        for child in group.children() {
            self.draw_node(child, to_canvas);
        }
    }

    fn draw_node(&mut self, node: &Node, to_canvas: Affine) {
        match node {
            Node::Group(group) => self.draw_group_node(node, group, to_canvas),
            Node::Path(path) => self.draw_path_node(node, path, to_canvas),
            Node::Image(image) => self.draw_image_node(node, image, to_canvas),
            Node::Text(text) => self.draw_text_node(node, text, to_canvas),
        }
    }

    fn draw_group_node(&mut self, node: &Node, group: &usvg::Group, to_canvas: Affine) {
        self.paint_group(group, to_canvas, Some(node));
    }

    fn paint_group(&mut self, group: &usvg::Group, to_canvas: Affine, node: Option<&Node>) {
        if group.opacity().get() <= 0.0 {
            self.record(
                "group",
                group.id(),
                SvgGpuRoute::Vector,
                "group opacity is zero, so nothing is painted",
            );
            return;
        }
        if group.mask().is_some() {
            self.fallback_group(node, group.id(), "SVG mask is not a GPU vector primitive");
            return;
        }
        if !group.filters().is_empty() {
            self.fallback_group(
                node,
                group.id(),
                "SVG filter primitives are not GPU vector draws",
            );
            return;
        }
        let child_space = to_canvas * affine_from(group.transform());
        let clips = if let Some(clip) = group.clip_path() {
            match encode_clip(clip, child_space) {
                Ok(clips) => clips,
                Err(reason) => {
                    self.fallback_group(node, group.id(), reason);
                    return;
                }
            }
        } else {
            Vec::new()
        };
        let needs_layer = group.opacity().get() < 1.0
            || group.blend_mode() != usvg::BlendMode::Normal
            || group.isolate();
        let interesting = needs_layer || !clips.is_empty() || !is_identity(group.transform());
        if interesting {
            self.record(
                "group",
                group.id(),
                SvgGpuRoute::Vector,
                group_reason(group, !clips.is_empty()),
            );
        }
        for (fill, path, transform) in &clips {
            self.scene
                .push_clip_layer(*fill, self.viewport * *transform, path);
        }
        if needs_layer {
            let bounds = Rect::new(
                0.0,
                0.0,
                self.space.canvas_w as f64,
                self.space.canvas_h as f64,
            );
            self.scene.push_layer(
                Fill::NonZero,
                BlendMode::new(mix_of(group.blend_mode()), Compose::SrcOver),
                group.opacity().get(),
                self.viewport * self.space.abs_to_outer,
                &bounds,
            );
        }
        self.draw_group(group, child_space);
        if needs_layer {
            self.scene.pop_layer();
        }
        for _ in &clips {
            self.scene.pop_layer();
        }
    }

    fn draw_path_node(&mut self, node: &Node, path: &usvg::Path, to_canvas: Affine) {
        if !path.is_visible() {
            return;
        }
        if path
            .stroke()
            .is_some_and(|stroke| matches!(stroke.linejoin(), usvg::LineJoin::MiterClip))
        {
            self.fallback(
                node,
                path.id(),
                "path",
                "stroke-linejoin miter-clip is not a GPU stroke",
            );
            return;
        }
        if paint_is_pattern(path.fill().map(|fill| fill.paint()))
            || paint_is_pattern(path.stroke().map(|stroke| stroke.paint()))
        {
            self.fallback(
                node,
                path.id(),
                "path",
                "pattern paint is not a direct GPU vector brush",
            );
            return;
        }
        self.record("path", path.id(), SvgGpuRoute::Vector, path_reason(path));
        let geometry = bez_path(path.data());
        let transform = self.viewport * to_canvas;
        let fill_first = path.paint_order() == PaintOrder::FillAndStroke;
        if fill_first {
            self.paint_fill(path, &geometry, transform);
            self.paint_stroke(path, &geometry, transform);
        } else {
            self.paint_stroke(path, &geometry, transform);
            self.paint_fill(path, &geometry, transform);
        }
    }

    fn paint_fill(&mut self, path: &usvg::Path, geometry: &BezPath, transform: Affine) {
        let Some(fill) = path.fill() else {
            return;
        };
        let bounds = path.data().bounds();
        if bounds.width() == 0.0 || bounds.height() == 0.0 {
            return;
        }
        let Some(paint) = gpu_paint(fill.paint(), fill.opacity().get()) else {
            return;
        };
        let rule = match fill.rule() {
            usvg::FillRule::NonZero => Fill::NonZero,
            usvg::FillRule::EvenOdd => Fill::EvenOdd,
        };
        apply_fill(self.scene, rule, transform, &paint, geometry);
    }

    fn paint_stroke(&mut self, path: &usvg::Path, geometry: &BezPath, transform: Affine) {
        let Some(stroke) = path.stroke() else {
            return;
        };
        let Some(paint) = gpu_paint(stroke.paint(), stroke.opacity().get()) else {
            return;
        };
        let style = stroke_style(stroke);
        apply_stroke(self.scene, &style, transform, &paint, geometry);
    }

    fn draw_image_node(&mut self, node: &Node, image: &usvg::Image, to_canvas: Affine) {
        if !image.is_visible() {
            return;
        }
        match image.kind() {
            usvg::ImageKind::SVG(tree) => {
                self.record(
                    "image",
                    image.id(),
                    SvgGpuRoute::Vector,
                    "nested SVG image is drawn as GPU vectors",
                );
                let saved = self.space;
                let saved_silent = self.silent;
                self.space = TreeSpace {
                    abs_to_outer: to_canvas,
                    canvas_w: tree.size().width().max(1.0),
                    canvas_h: tree.size().height().max(1.0),
                };
                self.silent = false;
                self.draw_group(tree.root(), to_canvas);
                self.space = saved;
                self.silent = saved_silent;
            }
            kind => match decode_raster(kind) {
                Some((width, height, pixels)) => {
                    let bytes = pixels.len() as u64;
                    self.frame.upload_bytes = self.frame.upload_bytes.saturating_add(bytes);
                    self.record(
                        "image",
                        image.id(),
                        SvgGpuRoute::Vector,
                        "embedded raster image is sampled as a GPU image brush",
                    );
                    let sx = image.size().width() / width.max(1) as f32;
                    let sy = image.size().height() / height.max(1) as f32;
                    draw_straight_image(
                        self.scene,
                        self.viewport * to_canvas * Affine::scale_non_uniform(sx as f64, sy as f64),
                        width,
                        height,
                        Arc::new(pixels),
                        ImageQuality::Medium,
                    );
                }
                None => self.fallback(
                    node,
                    image.id(),
                    "image",
                    "embedded image could not be decoded as a GPU image brush",
                ),
            },
        }
    }

    fn draw_text_node(&mut self, node: &Node, text: &usvg::Text, to_canvas: Affine) {
        let flat = text.flattened();
        if !flat.has_children() {
            let bounds = text.stroke_bounding_box();
            if bounds.width() > 0.0 && bounds.height() > 0.0 {
                self.fallback(node, text.id(), "text", "text glyphs are not outline paths");
            }
            return;
        }
        self.record(
            "text",
            text.id(),
            SvgGpuRoute::Vector,
            "text is drawn from GPU vector glyph outlines",
        );
        let saved = self.silent;
        self.silent = true;
        self.paint_group(flat, to_canvas, None);
        self.silent = saved;
    }

    fn fallback_group(&mut self, node: Option<&Node>, id: &str, reason: &'static str) {
        let Some(node) = node else {
            self.record("group", id, SvgGpuRoute::CpuPixmap, reason);
            return;
        };
        self.fallback(node, id, "group", reason);
    }

    fn fallback(&mut self, node: &Node, id: &str, kind: &str, reason: &'static str) {
        self.record(kind, id, SvgGpuRoute::CpuPixmap, reason);
        let Some((pixmap, origin_x, origin_y)) = rasterize_node(node) else {
            return;
        };
        let bytes = pixmap.data().len() as u64;
        self.frame.upload_bytes = self.frame.upload_bytes.saturating_add(bytes);
        let abs_w = pixmap.width() as f64;
        let abs_h = pixmap.height() as f64;
        let place = self.viewport
            * self.space.abs_to_outer
            * Affine::translate((f64::from(origin_x), f64::from(origin_y)))
            * Affine::scale_non_uniform(
                (pixmap.width() as f64) / abs_w.max(1.0),
                (pixmap.height() as f64) / abs_h.max(1.0),
            );
        let pixels = Arc::new(unpremultiply_rgba(pixmap.data()));
        draw_straight_image(
            self.scene,
            place,
            pixmap.width(),
            pixmap.height(),
            pixels,
            ImageQuality::Low,
        );
    }

    fn record(&mut self, kind: &str, id: &str, route: SvgGpuRoute, reason: &'static str) {
        if self.silent {
            return;
        }
        self.index = self.index.saturating_add(1);
        let element = if id.is_empty() {
            format!("{kind}#{}", self.index)
        } else {
            format!("{kind}#{id}")
        };
        match route {
            SvgGpuRoute::Vector => {
                self.frame.vector_elements = self.frame.vector_elements.saturating_add(1);
            }
            SvgGpuRoute::CpuPixmap => {
                self.frame.fallback_elements = self.frame.fallback_elements.saturating_add(1);
            }
        }
        self.frame.decisions.push(SvgElementDecision {
            element,
            route,
            reason,
        });
    }
}

fn group_reason(group: &usvg::Group, has_clip: bool) -> &'static str {
    if has_clip && group.opacity().get() < 1.0 {
        "group clip-path and opacity are GPU scene state"
    } else if has_clip {
        "clip-path is a GPU vector clip"
    } else if group.blend_mode() != usvg::BlendMode::Normal {
        "group blend mode is a GPU blend layer"
    } else if group.opacity().get() < 1.0 {
        "group opacity is a GPU blend layer"
    } else if group.isolate() {
        "group isolation is a GPU blend layer"
    } else {
        "group transform is a GPU affine"
    }
}

fn path_reason(path: &usvg::Path) -> &'static str {
    let gradient = |paint: Option<&usvg::Paint>| {
        matches!(
            paint,
            Some(usvg::Paint::LinearGradient(_) | usvg::Paint::RadialGradient(_))
        )
    };
    if gradient(path.fill().map(|fill| fill.paint()))
        || gradient(path.stroke().map(|stroke| stroke.paint()))
    {
        "path fill and stroke use GPU solid or gradient brushes"
    } else if path.fill().is_some() && path.stroke().is_some() {
        "path fill and stroke are GPU vector draws"
    } else if path.stroke().is_some() {
        "path stroke is a GPU vector draw"
    } else {
        "path fill is a GPU vector draw"
    }
}

fn paint_is_pattern(paint: Option<&usvg::Paint>) -> bool {
    matches!(paint, Some(usvg::Paint::Pattern(_)))
}

struct GpuPaint {
    gradient: Option<(Gradient, Affine)>,
    solid: Option<Color>,
}

fn gpu_paint(paint: &usvg::Paint, opacity: f32) -> Option<GpuPaint> {
    if opacity <= 0.0 {
        return None;
    }
    match paint {
        usvg::Paint::Color(color) => Some(GpuPaint {
            gradient: None,
            solid: Some(svg_color(*color, opacity)),
        }),
        usvg::Paint::LinearGradient(gradient) => {
            let brush = linear_gradient(gradient, opacity)?;
            let transform = affine_from(gradient.transform());
            Some(GpuPaint {
                gradient: Some((brush, transform)),
                solid: None,
            })
        }
        usvg::Paint::RadialGradient(gradient) => {
            let brush = radial_gradient(gradient, opacity)?;
            let transform = affine_from(gradient.transform());
            Some(GpuPaint {
                gradient: Some((brush, transform)),
                solid: None,
            })
        }
        usvg::Paint::Pattern(_) => None,
    }
}

fn linear_gradient(gradient: &usvg::LinearGradient, opacity: f32) -> Option<Gradient> {
    if (gradient.x1() - gradient.x2()).abs() < 1.0e-4
        && (gradient.y1() - gradient.y2()).abs() < 1.0e-4
    {
        return None;
    }
    let stops = gradient_stops(gradient.stops(), opacity)?;
    Some(
        Gradient::new_linear(
            (gradient.x1(), gradient.y1()),
            (gradient.x2(), gradient.y2()),
        )
        .with_extend(extend_of(gradient.spread_method()))
        .with_stops(stops.as_slice()),
    )
}

fn radial_gradient(gradient: &usvg::RadialGradient, opacity: f32) -> Option<Gradient> {
    let stops = gradient_stops(gradient.stops(), opacity)?;
    let center = (gradient.cx(), gradient.cy());
    let focal = (gradient.fx(), gradient.fy());
    let radius = gradient.r().get();
    let brush = if (focal.0 - center.0).abs() < 1.0e-3 && (focal.1 - center.1).abs() < 1.0e-3 {
        Gradient::new_radial(center, radius)
    } else {
        Gradient::new_two_point_radial(focal, 0.0, center, radius)
    };
    Some(
        brush
            .with_extend(extend_of(gradient.spread_method()))
            .with_stops(stops.as_slice()),
    )
}

fn gradient_stops(stops: &[usvg::Stop], opacity: f32) -> Option<Vec<(f32, Color)>> {
    if stops.len() < 2 {
        return None;
    }
    Some(
        stops
            .iter()
            .map(|stop| {
                (
                    stop.offset().get(),
                    svg_color(stop.color(), stop.opacity().get() * opacity),
                )
            })
            .collect(),
    )
}

fn extend_of(method: usvg::SpreadMethod) -> Extend {
    match method {
        usvg::SpreadMethod::Pad => Extend::Pad,
        usvg::SpreadMethod::Repeat => Extend::Repeat,
        usvg::SpreadMethod::Reflect => Extend::Reflect,
    }
}

fn apply_fill(scene: &mut Scene, rule: Fill, transform: Affine, paint: &GpuPaint, shape: &BezPath) {
    if let Some(color) = paint.solid {
        scene.fill(rule, transform, color, None, shape);
    } else if let Some((gradient, brush_transform)) = paint.gradient.as_ref() {
        scene.fill(rule, transform, gradient, Some(*brush_transform), shape);
    }
}

fn apply_stroke(
    scene: &mut Scene,
    style: &Stroke,
    transform: Affine,
    paint: &GpuPaint,
    shape: &BezPath,
) {
    if let Some(color) = paint.solid {
        scene.stroke(style, transform, color, None, shape);
    } else if let Some((gradient, brush_transform)) = paint.gradient.as_ref() {
        scene.stroke(style, transform, gradient, Some(*brush_transform), shape);
    }
}

fn stroke_style(stroke: &usvg::Stroke) -> Stroke {
    let cap = match stroke.linecap() {
        usvg::LineCap::Butt => Cap::Butt,
        usvg::LineCap::Round => Cap::Round,
        usvg::LineCap::Square => Cap::Square,
    };
    let join = match stroke.linejoin() {
        usvg::LineJoin::Miter | usvg::LineJoin::MiterClip => Join::Miter,
        usvg::LineJoin::Round => Join::Round,
        usvg::LineJoin::Bevel => Join::Bevel,
    };
    let mut style = Stroke::new(f64::from(stroke.width().get()))
        .with_caps(cap)
        .with_join(join)
        .with_miter_limit(f64::from(stroke.miterlimit().get()));
    if let Some(dash) = stroke.dasharray() {
        style = style.with_dashes(
            f64::from(stroke.dashoffset()),
            dash.iter().map(|value| f64::from(*value)),
        );
    }
    style
}

fn encode_clip(
    clip: &usvg::ClipPath,
    canvas_from_group: Affine,
) -> Result<Vec<(Fill, BezPath, Affine)>, &'static str> {
    let mut layers = Vec::new();
    encode_clip_layer(clip, canvas_from_group, &mut layers)?;
    Ok(layers)
}

fn encode_clip_layer(
    clip: &usvg::ClipPath,
    canvas_from_group: Affine,
    layers: &mut Vec<(Fill, BezPath, Affine)>,
) -> Result<(), &'static str> {
    let mut combined: Option<(usvg::FillRule, BezPath)> = None;
    collect_clip_paths(clip.root(), Affine::IDENTITY, &mut combined)?;
    if let Some((rule, path)) = combined {
        let fill = match rule {
            usvg::FillRule::NonZero => Fill::NonZero,
            usvg::FillRule::EvenOdd => Fill::EvenOdd,
        };
        layers.push((
            fill,
            path,
            canvas_from_group * affine_from(clip.transform()),
        ));
    }
    if let Some(nested) = clip.clip_path() {
        encode_clip_layer(nested, canvas_from_group, layers)?;
    }
    Ok(())
}

fn collect_clip_paths(
    group: &usvg::Group,
    incoming: Affine,
    combined: &mut Option<(usvg::FillRule, BezPath)>,
) -> Result<(), &'static str> {
    if group.opacity().get() < 1.0
        || group.blend_mode() != usvg::BlendMode::Normal
        || group.mask().is_some()
        || !group.filters().is_empty()
        || group.clip_path().is_some()
    {
        return Err("clip-path child is not a plain vector shape");
    }
    let local = incoming * affine_from(group.transform());
    for child in group.children() {
        match child {
            Node::Path(path) => {
                if !path.is_visible() {
                    continue;
                }
                let Some(fill) = path.fill() else {
                    continue;
                };
                let bounds = path.data().bounds();
                if bounds.width() == 0.0 || bounds.height() == 0.0 {
                    continue;
                }
                let baked = transform_path(&bez_path(path.data()), local);
                match combined {
                    None => *combined = Some((fill.rule(), baked)),
                    Some((rule, dest)) if *rule == fill.rule() => dest.extend(baked),
                    Some(_) => return Err("clip-path mixes fill rules"),
                }
            }
            Node::Group(group) => collect_clip_paths(group, local, combined)?,
            Node::Text(text) => collect_clip_paths(text.flattened(), local, combined)?,
            Node::Image(_) => return Err("clip-path contains an image"),
        }
    }
    Ok(())
}

fn rasterize_node(node: &Node) -> Option<(Pixmap, f32, f32)> {
    let bbox = node.abs_layer_bounding_box()?;
    let rendered_to_canvas = rendered_to_canvas(node)?;
    // Crop on the canvas pixel grid. A fractional bbox shift changes resvg's
    // filter-layer rounding, so the pixmap would no longer match the full
    // document raster that the CPU tile path produces.
    let x0 = (bbox.x() - FALLBACK_PAD).floor();
    let y0 = (bbox.y() - FALLBACK_PAD).floor();
    let x1 = (bbox.right() + FALLBACK_PAD).ceil();
    let y1 = (bbox.bottom() + FALLBACK_PAD).ceil();
    let width = (x1 - x0).round().max(0.0) as u32;
    let height = (y1 - y0).round().max(0.0) as u32;
    if width == 0 || height == 0 || width > MAX_FALLBACK_EDGE || height > MAX_FALLBACK_EDGE {
        return None;
    }
    let mut pixmap = Pixmap::new(width, height)?;
    let caller = resvg::tiny_skia::Transform::from_translate(-x0, -y0)
        .pre_concat(rendered_to_canvas)
        .pre_concat(resvg::tiny_skia::Transform::from_translate(
            bbox.x(),
            bbox.y(),
        ));
    resvg::render_node(node, caller, &mut pixmap.as_mut())?;
    Some((pixmap, x0, y0))
}

fn rendered_to_canvas(node: &Node) -> Option<resvg::tiny_skia::Transform> {
    match node {
        Node::Group(group) => Some(
            group
                .abs_transform()
                .pre_concat(group.transform().invert()?),
        ),
        Node::Path(path) => Some(path.abs_transform()),
        Node::Image(image) => Some(image.abs_transform()),
        Node::Text(text) => {
            let group = text.flattened();
            Some(
                group
                    .abs_transform()
                    .pre_concat(group.transform().invert()?),
            )
        }
    }
}

fn decode_raster(kind: &usvg::ImageKind) -> Option<(u32, u32, Vec<u8>)> {
    let bytes = match kind {
        usvg::ImageKind::JPEG(data)
        | usvg::ImageKind::PNG(data)
        | usvg::ImageKind::GIF(data)
        | usvg::ImageKind::WEBP(data) => data.as_slice(),
        usvg::ImageKind::SVG(_) => return None,
    };
    let image = image::load_from_memory(bytes).ok()?.into_rgba8();
    let width = image.width();
    let height = image.height();
    Some((width, height, image.into_raw()))
}

fn draw_straight_image(
    scene: &mut Scene,
    transform: Affine,
    width: u32,
    height: u32,
    pixels: Arc<Vec<u8>>,
    quality: ImageQuality,
) {
    if width == 0 || height == 0 || pixels.len() < (width as usize) * (height as usize) * 4 {
        return;
    }
    let image = ImageData {
        data: Blob::new(pixels),
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width,
        height,
    };
    scene.draw_image(&ImageBrush::new(image).with_quality(quality), transform);
}

fn bez_path(path: &resvg::tiny_skia::Path) -> BezPath {
    let mut bez = BezPath::new();
    for segment in path.segments() {
        match segment {
            resvg::tiny_skia::PathSegment::MoveTo(point) => {
                bez.move_to((f64::from(point.x), f64::from(point.y)));
            }
            resvg::tiny_skia::PathSegment::LineTo(point) => {
                bez.line_to((f64::from(point.x), f64::from(point.y)));
            }
            resvg::tiny_skia::PathSegment::QuadTo(control, point) => {
                bez.quad_to(
                    (f64::from(control.x), f64::from(control.y)),
                    (f64::from(point.x), f64::from(point.y)),
                );
            }
            resvg::tiny_skia::PathSegment::CubicTo(control1, control2, point) => {
                bez.curve_to(
                    (f64::from(control1.x), f64::from(control1.y)),
                    (f64::from(control2.x), f64::from(control2.y)),
                    (f64::from(point.x), f64::from(point.y)),
                );
            }
            resvg::tiny_skia::PathSegment::Close => bez.close_path(),
        }
    }
    bez
}

fn transform_path(path: &BezPath, transform: Affine) -> BezPath {
    let mut out = BezPath::new();
    for element in path {
        out.push(transform * element);
    }
    out
}

fn affine_from(transform: usvg::Transform) -> Affine {
    Affine::new([
        f64::from(transform.sx),
        f64::from(transform.ky),
        f64::from(transform.kx),
        f64::from(transform.sy),
        f64::from(transform.tx),
        f64::from(transform.ty),
    ])
}

fn is_identity(transform: usvg::Transform) -> bool {
    transform.sx == 1.0
        && transform.sy == 1.0
        && transform.kx == 0.0
        && transform.ky == 0.0
        && transform.tx == 0.0
        && transform.ty == 0.0
}

fn svg_color(color: usvg::Color, alpha: f32) -> Color {
    Color::new([
        f32::from(color.red) / 255.0,
        f32::from(color.green) / 255.0,
        f32::from(color.blue) / 255.0,
        alpha.clamp(0.0, 1.0),
    ])
}

fn mix_of(mode: usvg::BlendMode) -> Mix {
    match mode {
        usvg::BlendMode::Normal => Mix::Normal,
        usvg::BlendMode::Multiply => Mix::Multiply,
        usvg::BlendMode::Screen => Mix::Screen,
        usvg::BlendMode::Overlay => Mix::Overlay,
        usvg::BlendMode::Darken => Mix::Darken,
        usvg::BlendMode::Lighten => Mix::Lighten,
        usvg::BlendMode::ColorDodge => Mix::ColorDodge,
        usvg::BlendMode::ColorBurn => Mix::ColorBurn,
        usvg::BlendMode::HardLight => Mix::HardLight,
        usvg::BlendMode::SoftLight => Mix::SoftLight,
        usvg::BlendMode::Difference => Mix::Difference,
        usvg::BlendMode::Exclusion => Mix::Exclusion,
        usvg::BlendMode::Hue => Mix::Hue,
        usvg::BlendMode::Saturation => Mix::Saturation,
        usvg::BlendMode::Color => Mix::Color,
        usvg::BlendMode::Luminosity => Mix::Luminosity,
    }
}

fn unpremultiply_rgba(data: &[u8]) -> Vec<u8> {
    let mut rgba = data.to_vec();
    for pixel in rgba.chunks_exact_mut(4) {
        let alpha = pixel[3];
        if alpha == 0 || alpha == 255 {
            continue;
        }
        for channel in &mut pixel[..3] {
            *channel = ((u32::from(*channel) * 255 + u32::from(alpha) / 2) / u32::from(alpha))
                .min(255) as u8;
        }
    }
    rgba
}

fn push_decision(frame: &mut SvgGpuFrame, element: &str, route: SvgGpuRoute, reason: &'static str) {
    frame.decisions.push(SvgElementDecision {
        element: element.to_string(),
        route,
        reason,
    });
}

fn log_frame(frame: &SvgGpuFrame) {
    let mut suppressed = 0_u32;
    for decision in &frame.decisions {
        let route = match decision.route {
            SvgGpuRoute::Vector => "gpu-vector",
            SvgGpuRoute::CpuPixmap => "cpu-pixmap",
        };
        let line = format!(
            "W3COS SVG GPU: {} -> {route} ({})",
            decision.element, decision.reason
        );
        let fresh = LOGGED.with(|seen| seen.borrow_mut().insert(line.clone()));
        if !fresh {
            continue;
        }
        if LOGGED.with(|seen| seen.borrow().len()) > 64 {
            suppressed = suppressed.saturating_add(1);
            continue;
        }
        eprintln!("{line}");
    }
    if suppressed > 0 {
        eprintln!("W3COS SVG GPU: {suppressed} further element decisions suppressed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    use vello::Renderer;
    use vello::wgpu;

    fn svg(body: &str, size: u32) -> String {
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="{size}" height="{size}" viewBox="0 0 {size} {size}">{body}</svg>"#
        )
    }

    fn paint(source: &str, size: u32, mode: SvgPaintMode) -> SvgGpuFrame {
        crate::svg_renderer::clear_cache();
        let mut scene = Scene::new();
        let rect = LayoutRect {
            x: 0.0,
            y: 0.0,
            width: size as f32,
            height: size as f32,
        };
        draw_inline_svg_mode(&mut scene, source, rect, Affine::IDENTITY, mode)
    }

    fn has_route(frame: &SvgGpuFrame, route: SvgGpuRoute, reason_part: &str) -> bool {
        frame
            .decisions
            .iter()
            .any(|decision| decision.route == route && decision.reason.contains(reason_part))
    }

    #[test]
    fn vector_path_records_a_reason_for_each_supported_element() {
        let source = svg(
            r##"
            <defs>
              <linearGradient id="g" x1="0" y1="0" x2="64" y2="0">
                <stop offset="0" stop-color="#ff0000"/>
                <stop offset="1" stop-color="#0000ff"/>
              </linearGradient>
              <clipPath id="c"><circle cx="32" cy="32" r="20"/></clipPath>
            </defs>
            <g transform="translate(4 4)" opacity="0.5">
              <rect id="box" width="40" height="24" fill="url(#g)" stroke="#111111" stroke-width="2"/>
            </g>
            <circle cx="32" cy="40" r="12" fill="none" stroke="#00aa00" stroke-width="3" stroke-dasharray="2 2"/>
            <path id="hole" fill-rule="evenodd" fill="#222222" d="M8 48 H56 V60 H8 Z M20 50 H44 V58 H20 Z"/>
            <g clip-path="url(#c)"><rect width="64" height="64" fill="#ff8800"/></g>
            <text id="label" x="6" y="20" font-size="14" fill="#102030">Ag</text>
            "##,
            64,
        );
        let frame = paint(&source, 64, SvgPaintMode::GpuVector);
        assert!(
            has_route(&frame, SvgGpuRoute::Vector, "gradient"),
            "{:?}",
            frame.decisions
        );
        assert!(has_route(&frame, SvgGpuRoute::Vector, "group opacity"));
        assert!(has_route(&frame, SvgGpuRoute::Vector, "clip-path"));
        assert!(has_route(&frame, SvgGpuRoute::Vector, "path stroke"));
        assert!(has_route(&frame, SvgGpuRoute::Vector, "text is drawn"));
        assert_eq!(frame.fallback_elements, 0, "{:?}", frame.decisions);
        assert_eq!(frame.upload_bytes, 0);
        assert!(frame.vector_elements >= 5);
    }

    #[test]
    fn unsupported_elements_fall_back_per_element_with_a_reason() {
        let source = svg(
            r##"
            <defs>
              <filter id="blur"><feGaussianBlur stdDeviation="1"/></filter>
              <mask id="m"><rect width="64" height="64" fill="white"/></mask>
              <pattern id="p" width="8" height="8" patternUnits="userSpaceOnUse">
                <rect width="8" height="8" fill="#00ff00"/>
              </pattern>
            </defs>
            <rect id="plain" x="2" y="2" width="10" height="10" fill="#ff0000"/>
            <g id="filtered" filter="url(#blur)"><rect x="20" y="4" width="12" height="12" fill="#0000ff"/></g>
            <rect id="masked" x="4" y="24" width="16" height="16" fill="#00ff00" mask="url(#m)"/>
            <rect id="patterned" x="28" y="28" width="20" height="20" fill="url(#p)"/>
            "##,
            64,
        );
        let frame = paint(&source, 64, SvgPaintMode::GpuVector);
        assert!(
            has_route(&frame, SvgGpuRoute::Vector, "path fill"),
            "{:?}",
            frame.decisions
        );
        assert!(
            has_route(&frame, SvgGpuRoute::CpuPixmap, "filter"),
            "{:?}",
            frame.decisions
        );
        assert!(
            has_route(&frame, SvgGpuRoute::CpuPixmap, "mask"),
            "{:?}",
            frame.decisions
        );
        assert!(
            has_route(&frame, SvgGpuRoute::CpuPixmap, "pattern"),
            "{:?}",
            frame.decisions
        );
        assert!(frame.fallback_elements >= 3);
        assert!(frame.vector_elements >= 1);
        assert!(frame.upload_bytes > 0);
        assert!(frame.upload_bytes < 64 * 64 * 4);
    }

    #[test]
    fn cpu_tile_mode_uploads_the_full_pixmap() {
        let source = svg(r##"<rect width="32" height="32" fill="#336699"/>"##, 32);
        let frame = paint(&source, 32, SvgPaintMode::CpuTiles);
        assert_eq!(frame.fallback_elements, 1);
        assert_eq!(frame.vector_elements, 0);
        assert_eq!(frame.upload_bytes, 32 * 32 * 4);
        assert!(has_route(
            &frame,
            SvgGpuRoute::CpuPixmap,
            "CPU pixmap tiles are the selected SVG path"
        ));
    }

    struct PixelDiff {
        max_delta: u8,
        mean_abs: f32,
        outlier_frac: f32,
        hard_frac: f32,
    }

    fn premultiply(pixel: &[u8]) -> [u8; 4] {
        let alpha = u16::from(pixel[3]);
        [
            ((u16::from(pixel[0]) * alpha + 127) / 255) as u8,
            ((u16::from(pixel[1]) * alpha + 127) / 255) as u8,
            ((u16::from(pixel[2]) * alpha + 127) / 255) as u8,
            pixel[3],
        ]
    }

    fn compare_rgba(cpu: &[u8], gpu: &[u8]) -> PixelDiff {
        assert_eq!(cpu.len(), gpu.len());
        let mut max_delta = 0_u8;
        let mut sum = 0_u64;
        let mut outliers = 0_u64;
        let mut hard = 0_u64;
        let pixels = (cpu.len() / 4) as u64;
        for (left, right) in cpu.chunks_exact(4).zip(gpu.chunks_exact(4)) {
            // Straight-alpha blue-255 with alpha 3 is invisible next to
            // transparent. Compare the premultiplied color a compositor shows.
            let left = premultiply(left);
            let right = premultiply(right);
            let mut pixel_max = 0_u8;
            for index in 0..4 {
                let delta = left[index].abs_diff(right[index]);
                sum += u64::from(delta);
                pixel_max = pixel_max.max(delta);
            }
            max_delta = max_delta.max(pixel_max);
            if pixel_max > 40 {
                outliers += 1;
            }
            if pixel_max > 96 {
                hard += 1;
            }
        }
        PixelDiff {
            max_delta,
            mean_abs: sum as f32 / (pixels.max(1) * 4) as f32,
            outlier_frac: outliers as f32 / pixels.max(1) as f32,
            hard_frac: hard as f32 / pixels.max(1) as f32,
        }
    }

    struct GpuKit {
        device: wgpu::Device,
        queue: wgpu::Queue,
        renderer: Renderer,
    }

    fn lavapipe_env() {
        if std::env::var_os("VK_DRIVER_FILES").is_some() {
            return;
        }
        let Ok(entries) = std::fs::read_dir("/usr/share/vulkan/icd.d") else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if name.starts_with("lvp_icd") {
                // Tests select lavapipe before the Vulkan instance is created.
                // set_var is process-global; this only runs when the ICD is unset.
                unsafe {
                    std::env::set_var("VK_DRIVER_FILES", entry.path());
                    std::env::set_var("VK_ICD_FILENAMES", entry.path());
                }
                return;
            }
        }
    }

    fn gpu_kit() -> Option<&'static Mutex<GpuKit>> {
        static KIT: OnceLock<Option<Mutex<GpuKit>>> = OnceLock::new();
        KIT.get_or_init(|| {
            lavapipe_env();
            let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
                backends: wgpu::Backends::VULKAN,
                ..Default::default()
            });
            let adapter =
                pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::LowPower,
                    force_fallback_adapter: true,
                    ..Default::default()
                }))
                .ok()
                .or_else(|| {
                    pollster::block_on(
                        instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
                    )
                    .ok()
                })?;
            let (device, queue) =
                pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                    label: Some("w3cos-svg-gpu"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                    ..Default::default()
                }))
                .ok()?;
            let renderer = vello::Renderer::new(
                &device,
                vello::RendererOptions {
                    use_cpu: false,
                    antialiasing_support: vello::AaSupport::all(),
                    num_init_threads: std::num::NonZeroUsize::new(1),
                    pipeline_cache: None,
                },
            )
            .ok()?;
            eprintln!(
                "[w3cos] SVG GPU adapter: {} ({:?})",
                adapter.get_info().name,
                adapter.get_info().backend
            );
            Some(Mutex::new(GpuKit {
                device,
                queue,
                renderer,
            }))
        })
        .as_ref()
    }

    fn require_gpu() -> &'static Mutex<GpuKit> {
        match gpu_kit() {
            Some(kit) => kit,
            None if std::env::var("W3COS_GPU_TESTS").ok().as_deref() == Some("1") => {
                panic!("W3COS_GPU_TESTS=1 but no Vulkan adapter is available for SVG GPU compares");
            }
            None => {
                eprintln!("skip SVG GPU pixel compare; no Vulkan adapter");
                panic!("SKIP");
            }
        }
    }

    fn render_scene(scene: &Scene, width: u32, height: u32) -> Vec<u8> {
        let kit = require_gpu();
        let mut kit = kit.lock().expect("svg gpu kit");
        let texture = kit.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("svg-gpu-target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let GpuKit {
            device,
            queue,
            renderer,
        } = &mut *kit;
        renderer
            .render_to_texture(
                device,
                queue,
                scene,
                &view,
                &vello::RenderParams {
                    base_color: Color::new([0.0, 0.0, 0.0, 0.0]),
                    width,
                    height,
                    antialiasing_method: vello::AaConfig::Area,
                },
            )
            .expect("vello render");
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        crate::render_gpu::copy_texture_view_to_cpu(device, queue, &view, width, height)
            .expect("readback")
    }

    fn gpu_pixels(source: &str, size: u32) -> (Vec<u8>, SvgGpuFrame) {
        crate::svg_renderer::clear_cache();
        let mut scene = Scene::new();
        let rect = LayoutRect {
            x: 0.0,
            y: 0.0,
            width: size as f32,
            height: size as f32,
        };
        let frame = draw_inline_svg_mode(
            &mut scene,
            source,
            rect,
            Affine::IDENTITY,
            SvgPaintMode::GpuVector,
        );
        let pixels = render_scene(&scene, size, size);
        (pixels, frame)
    }

    fn cpu_pixels(source: &str, size: u32) -> Vec<u8> {
        crate::svg_renderer::clear_cache();
        crate::svg_renderer::get_or_render_inline(source, size, size)
            .expect("cpu svg")
            .data
            .as_ref()
            .clone()
    }

    fn expect_gpu() -> bool {
        if gpu_kit().is_some() {
            return true;
        }
        if std::env::var("W3COS_GPU_TESTS").ok().as_deref() == Some("1") {
            panic!("W3COS_GPU_TESTS=1 but no Vulkan adapter is available for SVG GPU compares");
        }
        eprintln!("skip SVG GPU pixel compare; no Vulkan adapter");
        false
    }

    #[test]
    fn gpu_vector_pixels_match_cpu_tiles_within_tolerance() {
        if !expect_gpu() {
            return;
        }
        let fixtures = [
            (
                "solid",
                svg(
                    r##"<rect x="8" y="8" width="48" height="48" fill="#ff0000"/>"##,
                    64,
                ),
            ),
            (
                "stroke",
                svg(
                    r##"<circle cx="32" cy="32" r="18" fill="none" stroke="#111827" stroke-width="4"/>"##,
                    64,
                ),
            ),
            (
                "gradient",
                svg(
                    r##"<defs><linearGradient id="g" x1="0" y1="0" x2="64" y2="0"><stop offset="0" stop-color="#ff0000"/><stop offset="1" stop-color="#0000ff"/></linearGradient></defs><rect width="64" height="64" fill="url(#g)"/>"##,
                    64,
                ),
            ),
            (
                "radial",
                svg(
                    r##"<defs><radialGradient id="g" cx="32" cy="32" r="24"><stop offset="0" stop-color="#ffffff"/><stop offset="1" stop-color="#0f766e"/></radialGradient></defs><rect width="64" height="64" fill="url(#g)"/>"##,
                    64,
                ),
            ),
            (
                "transform",
                svg(
                    r##"<g transform="translate(12 8) scale(1.5) rotate(15)"><rect width="16" height="12" fill="#7c3aed"/></g>"##,
                    64,
                ),
            ),
            (
                "clip",
                svg(
                    r##"<clipPath id="c"><circle cx="32" cy="32" r="16"/></clipPath><rect width="64" height="64" fill="#ea580c" clip-path="url(#c)"/>"##,
                    64,
                ),
            ),
            (
                "opacity",
                svg(
                    r##"<g opacity="0.5"><rect x="8" y="8" width="48" height="48" fill="#dc2626"/></g>"##,
                    64,
                ),
            ),
            (
                "text",
                svg(
                    r##"<text x="4" y="40" font-size="28" fill="#0f172a">Ag</text>"##,
                    64,
                ),
            ),
            (
                "mixed-fallback",
                svg(
                    r##"<defs><filter id="f"><feGaussianBlur stdDeviation="1.5"/></filter></defs><rect x="4" y="4" width="20" height="20" fill="#16a34a"/><g transform="translate(28 24)" filter="url(#f)"><rect width="18" height="18" fill="#2563eb"/></g>"##,
                    64,
                ),
            ),
        ];
        let mut failures = Vec::new();
        for (label, source) in fixtures {
            let cpu = cpu_pixels(&source, 64);
            let (gpu, frame) = gpu_pixels(&source, 64);
            if label == "mixed-fallback" {
                if frame.vector_elements < 1 || frame.fallback_elements < 1 {
                    failures.push(format!("{label} routing {:?}", frame.decisions));
                }
            } else if frame.fallback_elements != 0 || frame.upload_bytes != 0 {
                failures.push(format!(
                    "{label} expected a pure vector path, got {:?}",
                    frame.decisions
                ));
            }
            let diff = compare_rgba(&cpu, &gpu);
            eprintln!(
                "SVG_GPU_PIXEL {label} mean={:.2} outliers={:.3} hard={:.3} max={}",
                diff.mean_abs, diff.outlier_frac, diff.hard_frac, diff.max_delta
            );
            // Vello area AA and resvg can fully disagree on a few rotated-edge
            // pixels. Mean error and the fraction of those pixels still have to
            // stay small, which fails a shifted or recolored shape.
            if !(diff.mean_abs < 8.0 && diff.outlier_frac < 0.02 && diff.hard_frac < 0.01) {
                failures.push(format!(
                    "{label} gpu vs cpu mean {:.2} outliers {:.3} hard {:.3} max {}",
                    diff.mean_abs, diff.outlier_frac, diff.hard_frac, diff.max_delta
                ));
            }
            let cpu_ink = cpu.chunks_exact(4).filter(|pixel| pixel[3] > 16).count();
            let gpu_ink = gpu.chunks_exact(4).filter(|pixel| pixel[3] > 16).count();
            if cpu_ink > 8 {
                let ratio = gpu_ink as f32 / cpu_ink as f32;
                if !(0.75..=1.25).contains(&ratio) {
                    failures.push(format!("{label} coverage {gpu_ink} vs {cpu_ink}"));
                }
            }
            if label == "solid" {
                let pixel = 32 * 64 + 32;
                let cpu_px = &cpu[pixel * 4..pixel * 4 + 4];
                let gpu_px = &gpu[pixel * 4..pixel * 4 + 4];
                for index in 0..4 {
                    assert!(
                        cpu_px[index].abs_diff(gpu_px[index]) <= 8,
                        "solid center cpu {cpu_px:?} gpu {gpu_px:?}"
                    );
                }
            }
        }
        assert!(failures.is_empty(), "{failures:?}");
    }

    fn heavy_svg() -> String {
        let mut body = String::from(
            r##"<defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#0ea5e9"/><stop offset="1" stop-color="#f43f5e"/></linearGradient><clipPath id="c"><rect width="28" height="18" rx="3"/></clipPath></defs>"##,
        );
        for index in 0..160 {
            let x = (index % 16) * 40;
            let y = (index / 16) * 48;
            let angle = (index % 9) * 4;
            body.push_str(&format!(
                r##"<g transform="translate({x} {y}) rotate({angle})" opacity="0.9"><g clip-path="url(#c)"><rect width="28" height="18" fill="url(#g)" stroke="#111827" stroke-width="1"/><circle cx="14" cy="9" r="5" fill="#ffffff" fill-opacity="0.4"/></g></g>"##
            ));
        }
        svg(&body, 640)
    }

    fn timed_frames(source: &str, size: u32, mode: SvgPaintMode, frames: u32) -> (Duration, u64) {
        crate::svg_renderer::clear_cache();
        let kit = require_gpu();
        let mut cold_upload = 0_u64;
        let mut elapsed = Duration::ZERO;
        for frame_index in 0..frames {
            let started = Instant::now();
            let mut scene = Scene::new();
            let rect = LayoutRect {
                x: 0.0,
                y: 0.0,
                width: size as f32,
                height: size as f32,
            };
            let stats = draw_inline_svg_mode(&mut scene, source, rect, Affine::IDENTITY, mode);
            if frame_index == 0 {
                cold_upload = stats.upload_bytes;
            }
            {
                let mut kit = kit.lock().expect("svg gpu kit");
                let texture = kit.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("svg-bench-target"),
                    size: wgpu::Extent3d {
                        width: size,
                        height: size,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::STORAGE_BINDING,
                    view_formats: &[],
                });
                let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                let GpuKit {
                    device,
                    queue,
                    renderer,
                } = &mut *kit;
                renderer
                    .render_to_texture(
                        device,
                        queue,
                        &scene,
                        &view,
                        &vello::RenderParams {
                            base_color: Color::new([1.0, 1.0, 1.0, 1.0]),
                            width: size,
                            height: size,
                            antialiasing_method: vello::AaConfig::Area,
                        },
                    )
                    .expect("bench render");
                device
                    .poll(wgpu::PollType::wait_indefinitely())
                    .expect("bench poll");
            }
            elapsed += started.elapsed();
        }
        (elapsed / frames, cold_upload)
    }

    #[test]
    fn svg_heavy_scene_benchmark_reports_frame_time_and_upload_bytes() {
        if !expect_gpu() {
            return;
        }
        let source = heavy_svg();
        let (cpu_frame, cpu_upload) = timed_frames(&source, 640, SvgPaintMode::CpuTiles, 3);
        let (gpu_frame, gpu_upload) = timed_frames(&source, 640, SvgPaintMode::GpuVector, 3);
        eprintln!(
            "SVG_GPU_BENCH cpu_frame_ms={:.3} gpu_frame_ms={:.3} cpu_upload_bytes={cpu_upload} gpu_upload_bytes={gpu_upload}",
            cpu_frame.as_secs_f64() * 1_000.0,
            gpu_frame.as_secs_f64() * 1_000.0,
        );
        assert!(
            gpu_upload < cpu_upload,
            "vector path should upload fewer pixmap bytes ({gpu_upload} vs {cpu_upload})"
        );
        assert_eq!(gpu_upload, 0);
        assert_eq!(cpu_upload, 640 * 640 * 4);
    }
}
