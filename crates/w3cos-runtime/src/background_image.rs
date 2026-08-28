use crate::layout::LayoutRect;
use w3cos_std::background::split_top_level;
use w3cos_std::style::Style;

const MAX_BACKGROUND_TILES_PER_LAYER: usize = 4096;

#[derive(Debug, Clone)]
pub(crate) struct RasterBackgroundLayer {
    pub layer_index: usize,
    pub source: String,
    pub clip: BackgroundClip,
    pub tiles: Vec<LayoutRect>,
    pub blend_mode: BackgroundBlendMode,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct BackgroundClip {
    pub rect: LayoutRect,
    pub radius: f32,
}

#[derive(Debug, Clone)]
pub(crate) struct BackgroundGeometry {
    pub clip: BackgroundClip,
    pub tiles: Vec<LayoutRect>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct GradientStop {
    pub color: w3cos_std::color::Color,
    pub position: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RadialShape {
    Circle,
    Ellipse,
}

#[derive(Debug, Clone)]
pub(crate) enum GradientKind {
    Linear {
        angle_degrees: f32,
    },
    Radial {
        center_x: f32,
        center_y: f32,
        shape: RadialShape,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct GradientBackgroundLayer {
    pub layer_index: usize,
    pub kind: GradientKind,
    pub stops: Vec<GradientStop>,
    pub geometry: BackgroundGeometry,
    pub blend_mode: BackgroundBlendMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BackgroundBlendMode {
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
}

#[derive(Debug, Clone)]
pub(crate) enum BackgroundPaintLayer {
    Raster(RasterBackgroundLayer),
    Gradient(GradientBackgroundLayer),
}

impl BackgroundPaintLayer {
    pub fn layer_index(&self) -> usize {
        match self {
            Self::Raster(layer) => layer.layer_index,
            Self::Gradient(layer) => layer.layer_index,
        }
    }
}

#[derive(Clone, Copy)]
enum BoxKind {
    Border,
    Padding,
    Content,
}

#[derive(Clone, Copy)]
enum Length {
    Auto,
    Px(f32),
    Percent(f32),
}

#[derive(Clone, Copy)]
struct IntrinsicSize {
    width: Length,
    height: Length,
    ratio: Option<f32>,
}

#[derive(Clone, Copy)]
enum Repeat {
    Repeat,
    NoRepeat,
    Round,
    Space,
}

pub(crate) fn raster_background_layers(
    style: &Style,
    border_box: LayoutRect,
) -> Vec<RasterBackgroundLayer> {
    raster_background_layers_with_positioning_area(style, border_box, None)
}

fn raster_background_layers_with_positioning_area(
    style: &Style,
    border_box: LayoutRect,
    positioning_area: Option<LayoutRect>,
) -> Vec<RasterBackgroundLayer> {
    let Some(images) = style.background_image.as_deref() else {
        return Vec::new();
    };
    let image_layers = split_top_level(images, ',');
    image_layers
        .iter()
        .enumerate()
        .filter_map(|(index, layer)| {
            let source = crate::image_loader::css_image_urls(layer)
                .into_iter()
                .next()?;
            let decoded = crate::image_loader::get_or_load(&source)?;
            let intrinsic = decoded.svg_intrinsic_size.map_or(
                IntrinsicSize {
                    width: Length::Px(decoded.intrinsic_width as f32),
                    height: Length::Px(decoded.intrinsic_height as f32),
                    ratio: Some(decoded.intrinsic_width as f32 / decoded.intrinsic_height as f32),
                },
                |svg| IntrinsicSize {
                    width: svg_intrinsic_length(svg.width),
                    height: svg_intrinsic_length(svg.height),
                    ratio: svg.ratio,
                },
            );
            let geometry =
                layer_geometry(style, border_box, index, Some(intrinsic), positioning_area)?;
            Some(RasterBackgroundLayer {
                layer_index: index,
                source,
                clip: geometry.clip,
                tiles: geometry.tiles,
                blend_mode: layer_blend_mode(style, index),
            })
        })
        .collect()
}

pub(crate) fn gradient_background_layers(
    style: &Style,
    border_box: LayoutRect,
) -> Vec<GradientBackgroundLayer> {
    gradient_background_layers_with_positioning_area(style, border_box, None)
}

fn gradient_background_layers_with_positioning_area(
    style: &Style,
    border_box: LayoutRect,
    positioning_area: Option<LayoutRect>,
) -> Vec<GradientBackgroundLayer> {
    let Some(images) = style.background_image.as_deref() else {
        return Vec::new();
    };
    split_top_level(images, ',')
        .into_iter()
        .enumerate()
        .filter_map(|(index, layer)| {
            let (kind, stops) = parse_gradient(layer)?;
            let geometry = layer_geometry(style, border_box, index, None, positioning_area)?;
            Some(GradientBackgroundLayer {
                layer_index: index,
                kind,
                stops,
                geometry,
                blend_mode: layer_blend_mode(style, index),
            })
        })
        .collect()
}

pub(crate) fn background_paint_layers(
    style: &Style,
    border_box: LayoutRect,
) -> Vec<BackgroundPaintLayer> {
    let mut layers = raster_background_layers(style, border_box)
        .into_iter()
        .map(BackgroundPaintLayer::Raster)
        .chain(
            gradient_background_layers(style, border_box)
                .into_iter()
                .map(BackgroundPaintLayer::Gradient),
        )
        .collect::<Vec<_>>();
    layers.sort_by_key(BackgroundPaintLayer::layer_index);
    layers
}

pub(crate) fn canvas_background_paint_layers(
    style: &Style,
    canvas_box: LayoutRect,
    positioning_area: Option<LayoutRect>,
) -> Vec<BackgroundPaintLayer> {
    let mut layers =
        raster_background_layers_with_positioning_area(style, canvas_box, positioning_area)
            .into_iter()
            .map(BackgroundPaintLayer::Raster)
            .chain(
                gradient_background_layers_with_positioning_area(
                    style,
                    canvas_box,
                    positioning_area,
                )
                .into_iter()
                .map(BackgroundPaintLayer::Gradient),
            )
            .collect::<Vec<_>>();
    layers.sort_by_key(BackgroundPaintLayer::layer_index);
    layers
}

pub(crate) fn linear_gradient_points(
    rect: LayoutRect,
    angle_degrees: f32,
) -> ((f32, f32), (f32, f32)) {
    let radians = angle_degrees.to_radians();
    let direction = (radians.sin(), -radians.cos());
    let center = (rect.x + rect.width * 0.5, rect.y + rect.height * 0.5);
    let extent = direction.0.abs() * rect.width * 0.5 + direction.1.abs() * rect.height * 0.5;
    (
        (
            center.0 - direction.0 * extent,
            center.1 - direction.1 * extent,
        ),
        (
            center.0 + direction.0 * extent,
            center.1 + direction.1 * extent,
        ),
    )
}

pub(crate) fn radial_gradient_circle(
    rect: LayoutRect,
    center_x: f32,
    center_y: f32,
) -> ((f32, f32), f32) {
    let center = (
        rect.x + center_x * rect.width,
        rect.y + center_y * rect.height,
    );
    let radius = [
        (center.0 - rect.x).hypot(center.1 - rect.y),
        (center.0 - (rect.x + rect.width)).hypot(center.1 - rect.y),
        (center.0 - rect.x).hypot(center.1 - (rect.y + rect.height)),
        (center.0 - (rect.x + rect.width)).hypot(center.1 - (rect.y + rect.height)),
    ]
    .into_iter()
    .fold(0.0_f32, f32::max);
    (center, radius.max(1.0))
}

pub(crate) fn radial_gradient_axes(
    rect: LayoutRect,
    center_x: f32,
    center_y: f32,
    shape: RadialShape,
) -> ((f32, f32), (f32, f32)) {
    let center = (
        rect.x + center_x * rect.width,
        rect.y + center_y * rect.height,
    );
    if shape == RadialShape::Circle {
        let (_, radius) = radial_gradient_circle(rect, center_x, center_y);
        return (center, (radius, radius));
    }
    let radius_x = (center.0 - rect.x)
        .max(rect.x + rect.width - center.0)
        .max(1.0);
    let radius_y = (center.1 - rect.y)
        .max(rect.y + rect.height - center.1)
        .max(1.0);
    (center, (radius_x, radius_y))
}

fn layer_geometry(
    style: &Style,
    border_box: LayoutRect,
    index: usize,
    intrinsic: Option<IntrinsicSize>,
    positioning_override: Option<LayoutRect>,
) -> Option<BackgroundGeometry> {
    let origin = layer_value(style.background_origin.as_deref(), index, "padding-box");
    let clip_value = layer_value(style.background_clip.as_deref(), index, "border-box");
    let mut positioning_area = positioning_override
        .unwrap_or_else(|| background_box(style, border_box, parse_box(origin)));
    if layer_value(style.background_attachment.as_deref(), index, "scroll")
        .eq_ignore_ascii_case("fixed")
    {
        // Layout rects are already expressed in viewport coordinates. Anchoring
        // the positioning area at the viewport origin keeps the image phase
        // stable while the element clip moves during scrolling.
        positioning_area.x = 0.0;
        positioning_area.y = 0.0;
    }
    let clip_kind = parse_box(clip_value);
    let clip_rect = background_box(style, border_box, clip_kind);
    let clip = BackgroundClip {
        rect: clip_rect,
        radius: background_radius(style, clip_kind),
    };
    if positioning_area.width <= 0.0
        || positioning_area.height <= 0.0
        || clip.rect.width <= 0.0
        || clip.rect.height <= 0.0
    {
        return None;
    }
    let size = layer_value(style.background_size.as_deref(), index, "auto");
    let (auto_width, auto_height) = auto_size_axes(size);
    let intrinsic_ratio = intrinsic.and_then(|intrinsic| {
        intrinsic.ratio.or_else(|| {
            resolve_length(intrinsic.width, positioning_area.width)
                .zip(resolve_length(intrinsic.height, positioning_area.height))
                .filter(|(width, height)| *width > 0.0 && *height > 0.0)
                .map(|(width, height)| width / height)
        })
    });
    let (mut width, mut height) = resolve_size(size, positioning_area, intrinsic);
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    let repeat = layer_value(style.background_repeat.as_deref(), index, "repeat");
    let (repeat_x, repeat_y) = parse_repeat(repeat);
    if matches!(repeat_x, Repeat::Round) {
        width = round_tile_size(positioning_area.width, width);
        if auto_height
            && !matches!(repeat_y, Repeat::Round)
            && let Some(ratio) = intrinsic_ratio
        {
            height = width / ratio;
        }
    }
    if matches!(repeat_y, Repeat::Round) {
        height = round_tile_size(positioning_area.height, height);
        if auto_width
            && !matches!(repeat_x, Repeat::Round)
            && let Some(ratio) = intrinsic_ratio
        {
            width = height * ratio;
        }
    }
    let position = layer_value(style.background_position.as_deref(), index, "0% 0%");
    let (position_x, position_y) = resolve_position_with_ex(
        position,
        positioning_area,
        width,
        height,
        style.font_size,
        style_ex_size(style),
    );
    let xs = axis_tiles(
        positioning_area.x,
        positioning_area.width,
        clip.rect.x,
        clip.rect.width,
        position_x,
        width,
        repeat_x,
    );
    let ys = axis_tiles(
        positioning_area.y,
        positioning_area.height,
        clip.rect.y,
        clip.rect.height,
        position_y,
        height,
        repeat_y,
    );
    let mut tiles = Vec::with_capacity(
        xs.len()
            .saturating_mul(ys.len())
            .min(MAX_BACKGROUND_TILES_PER_LAYER),
    );
    'rows: for y in ys {
        for x in &xs {
            if tiles.len() >= MAX_BACKGROUND_TILES_PER_LAYER {
                break 'rows;
            }
            tiles.push(LayoutRect {
                x: *x,
                y,
                width,
                height,
            });
        }
    }
    Some(BackgroundGeometry { clip, tiles })
}

fn layer_blend_mode(style: &Style, index: usize) -> BackgroundBlendMode {
    match layer_value(style.background_blend_mode.as_deref(), index, "normal")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "multiply" => BackgroundBlendMode::Multiply,
        "screen" => BackgroundBlendMode::Screen,
        "overlay" => BackgroundBlendMode::Overlay,
        "darken" => BackgroundBlendMode::Darken,
        "lighten" => BackgroundBlendMode::Lighten,
        "color-dodge" => BackgroundBlendMode::ColorDodge,
        "color-burn" => BackgroundBlendMode::ColorBurn,
        "hard-light" => BackgroundBlendMode::HardLight,
        "soft-light" => BackgroundBlendMode::SoftLight,
        "difference" => BackgroundBlendMode::Difference,
        "exclusion" => BackgroundBlendMode::Exclusion,
        _ => BackgroundBlendMode::Normal,
    }
}

fn layer_value<'a>(value: Option<&'a str>, index: usize, initial: &'a str) -> &'a str {
    let Some(value) = value else {
        return initial;
    };
    let layers = split_top_level(value, ',');
    if layers.is_empty() {
        initial
    } else {
        layers[index % layers.len()]
    }
}

fn parse_box(value: &str) -> BoxKind {
    match value.trim().to_ascii_lowercase().as_str() {
        "content-box" => BoxKind::Content,
        "padding-box" => BoxKind::Padding,
        _ => BoxKind::Border,
    }
}

fn background_box(style: &Style, rect: LayoutRect, kind: BoxKind) -> LayoutRect {
    let borders = [
        style
            .border_top_width
            .unwrap_or(style.border_width)
            .max(0.0),
        style
            .border_right_width
            .unwrap_or(style.border_width)
            .max(0.0),
        style
            .border_bottom_width
            .unwrap_or(style.border_width)
            .max(0.0),
        style
            .border_left_width
            .unwrap_or(style.border_width)
            .max(0.0),
    ];
    let padding = style.padding_lengths();
    let padding_edges = [
        padding.top.max(0.0),
        padding.right.max(0.0),
        padding.bottom.max(0.0),
        padding.left.max(0.0),
    ];
    let inset = match kind {
        BoxKind::Border => [0.0; 4],
        BoxKind::Padding => borders,
        BoxKind::Content => [
            borders[0] + padding_edges[0],
            borders[1] + padding_edges[1],
            borders[2] + padding_edges[2],
            borders[3] + padding_edges[3],
        ],
    };
    LayoutRect {
        x: rect.x + inset[3],
        y: rect.y + inset[0],
        width: (rect.width - inset[1] - inset[3]).max(0.0),
        height: (rect.height - inset[0] - inset[2]).max(0.0),
    }
}

fn background_radius(style: &Style, kind: BoxKind) -> f32 {
    let borders = [
        style
            .border_top_width
            .unwrap_or(style.border_width)
            .max(0.0),
        style
            .border_right_width
            .unwrap_or(style.border_width)
            .max(0.0),
        style
            .border_bottom_width
            .unwrap_or(style.border_width)
            .max(0.0),
        style
            .border_left_width
            .unwrap_or(style.border_width)
            .max(0.0),
    ];
    let padding = style.padding_lengths();
    let inset = match kind {
        BoxKind::Border => 0.0,
        BoxKind::Padding => borders.into_iter().fold(0.0_f32, f32::max),
        BoxKind::Content => [
            borders[0] + padding.top.max(0.0),
            borders[1] + padding.right.max(0.0),
            borders[2] + padding.bottom.max(0.0),
            borders[3] + padding.left.max(0.0),
        ]
        .into_iter()
        .fold(0.0_f32, f32::max),
    };
    (style.border_radius - inset).max(0.0)
}

fn resolve_size(value: &str, area: LayoutRect, intrinsic: Option<IntrinsicSize>) -> (f32, f32) {
    let intrinsic = intrinsic.unwrap_or(IntrinsicSize {
        width: Length::Auto,
        height: Length::Auto,
        ratio: None,
    });
    let intrinsic_width = resolve_length(intrinsic.width, area.width);
    let intrinsic_height = resolve_length(intrinsic.height, area.height);
    let ratio = intrinsic.ratio.or_else(|| {
        intrinsic_width
            .zip(intrinsic_height)
            .filter(|(width, height)| *width > 0.0 && *height > 0.0)
            .map(|(width, height)| width / height)
    });
    match value.trim().to_ascii_lowercase().as_str() {
        "cover" => {
            if let Some(ratio) = ratio {
                return size_for_ratio(area, ratio, true);
            }
            return (area.width, area.height);
        }
        "contain" => {
            if let Some(ratio) = ratio {
                return size_for_ratio(area, ratio, false);
            }
            return (area.width, area.height);
        }
        _ => {}
    }
    let parts = value.split_ascii_whitespace().collect::<Vec<_>>();
    let width = parse_length(parts.first().copied().unwrap_or("auto"));
    let height = parse_length(parts.get(1).copied().unwrap_or("auto"));
    match (
        resolve_length(width, area.width),
        resolve_length(height, area.height),
    ) {
        (Some(width), Some(height)) => (width, height),
        (Some(width), None) => ratio
            .map(|ratio| (width, width / ratio))
            .or_else(|| intrinsic_height.map(|height| (width, height)))
            .unwrap_or((width, area.height)),
        (None, Some(height)) => ratio
            .map(|ratio| (height * ratio, height))
            .or_else(|| intrinsic_width.map(|width| (width, height)))
            .unwrap_or((area.width, height)),
        (None, None) => match (intrinsic_width, intrinsic_height, ratio) {
            (Some(width), Some(height), _) => (width, height),
            (Some(width), None, Some(ratio)) => (width, width / ratio),
            (None, Some(height), Some(ratio)) => (height * ratio, height),
            (Some(width), None, None) => (width, area.height),
            (None, Some(height), None) => (area.width, height),
            (None, None, Some(ratio)) => size_for_ratio(area, ratio, false),
            (None, None, None) => (area.width, area.height),
        },
    }
}

fn size_for_ratio(area: LayoutRect, ratio: f32, cover: bool) -> (f32, f32) {
    let ratio = ratio.max(f32::EPSILON);
    let width_from_height = area.height * ratio;
    let choose_width = if cover {
        width_from_height >= area.width
    } else {
        width_from_height <= area.width
    };
    if choose_width {
        (width_from_height, area.height)
    } else {
        (area.width, area.width / ratio)
    }
}

fn svg_intrinsic_length(value: crate::image_loader::SvgIntrinsicLength) -> Length {
    match value {
        crate::image_loader::SvgIntrinsicLength::Auto => Length::Auto,
        crate::image_loader::SvgIntrinsicLength::Px(value) => Length::Px(value),
        crate::image_loader::SvgIntrinsicLength::Percent(value) => Length::Percent(value),
    }
}

fn parse_gradient(value: &str) -> Option<(GradientKind, Vec<GradientStop>)> {
    let (kind, arguments, repeating) =
        if let Some(arguments) = function_arguments(value, "linear-gradient") {
            (
                GradientKind::Linear {
                    angle_degrees: 180.0,
                },
                arguments,
                false,
            )
        } else if let Some(arguments) = function_arguments(value, "repeating-linear-gradient") {
            (
                GradientKind::Linear {
                    angle_degrees: 180.0,
                },
                arguments,
                true,
            )
        } else if let Some(arguments) = function_arguments(value, "radial-gradient") {
            (
                GradientKind::Radial {
                    center_x: 0.5,
                    center_y: 0.5,
                    shape: RadialShape::Ellipse,
                },
                arguments,
                false,
            )
        } else if let Some(arguments) = function_arguments(value, "repeating-radial-gradient") {
            (
                GradientKind::Radial {
                    center_x: 0.5,
                    center_y: 0.5,
                    shape: RadialShape::Ellipse,
                },
                arguments,
                true,
            )
        } else {
            return None;
        };
    let mut parts = split_top_level(arguments, ',');
    let kind = match kind {
        GradientKind::Linear { .. } => {
            let angle = parts.first().and_then(|part| parse_linear_direction(part));
            if angle.is_some() {
                parts.remove(0);
            }
            GradientKind::Linear {
                angle_degrees: angle.unwrap_or(180.0),
            }
        }
        GradientKind::Radial { .. } => {
            let mut center = (0.5, 0.5);
            let mut shape = RadialShape::Ellipse;
            if let Some(header) = parts.first().copied()
                && parse_gradient_stop(header).is_none()
            {
                if header
                    .split_ascii_whitespace()
                    .any(|part| part.eq_ignore_ascii_case("circle"))
                {
                    shape = RadialShape::Circle;
                }
                if let Some(at) = header.find(" at ") {
                    let coords = header[at + 4..].split_whitespace().collect::<Vec<_>>();
                    if coords.len() >= 2 {
                        center.0 = gradient_position(coords[0], true).unwrap_or(0.5);
                        center.1 = gradient_position(coords[1], false).unwrap_or(0.5);
                    }
                }
                parts.remove(0);
            }
            GradientKind::Radial {
                center_x: center.0,
                center_y: center.1,
                shape,
            }
        }
    };
    let mut stops = parts
        .iter()
        .flat_map(|part| parse_gradient_stops(part))
        .collect::<Vec<_>>();
    if stops.len() < 2 {
        return None;
    }
    normalize_gradient_stops(&mut stops);
    if repeating {
        stops = expand_repeating_stops(stops);
    }
    Some((
        kind,
        stops
            .into_iter()
            .map(|(color, position)| GradientStop {
                color,
                position: position.unwrap_or(0.0),
            })
            .collect(),
    ))
}

fn parse_gradient_stop(value: &str) -> Option<(w3cos_std::color::Color, Option<f32>)> {
    parse_gradient_stops(value).into_iter().next()
}

fn parse_gradient_stops(value: &str) -> Vec<(w3cos_std::color::Color, Option<f32>)> {
    let parts = split_css_whitespace(value.trim());
    let Some(color) = parts
        .first()
        .and_then(|value| w3cos_std::color::Color::from_css(value))
    else {
        return Vec::new();
    };
    let position = parts.get(1).and_then(|value| parse_percent(value));
    let second_position = parts.get(2).and_then(|value| parse_percent(value));
    let mut result = vec![(color, position)];
    if let Some(position) = second_position {
        result.push((color, Some(position)));
    }
    result
}

fn parse_linear_direction(value: &str) -> Option<f32> {
    let value = value.trim().to_ascii_lowercase();
    if let Some(value) = value.strip_suffix("deg") {
        return value.trim().parse().ok();
    }
    let direction = value
        .strip_prefix("to ")?
        .split_ascii_whitespace()
        .collect::<Vec<_>>();
    match direction.as_slice() {
        ["top"] => Some(0.0),
        ["right"] => Some(90.0),
        ["bottom"] => Some(180.0),
        ["left"] => Some(270.0),
        ["top", "right"] | ["right", "top"] => Some(45.0),
        ["bottom", "right"] | ["right", "bottom"] => Some(135.0),
        ["bottom", "left"] | ["left", "bottom"] => Some(225.0),
        ["top", "left"] | ["left", "top"] => Some(315.0),
        _ => None,
    }
}

fn gradient_position(value: &str, horizontal: bool) -> Option<f32> {
    match value.trim().to_ascii_lowercase().as_str() {
        "left" | "top" => Some(0.0),
        "center" => Some(0.5),
        "right" if horizontal => Some(1.0),
        "bottom" if !horizontal => Some(1.0),
        value => parse_percent(value),
    }
}

fn expand_repeating_stops(
    stops: Vec<(w3cos_std::color::Color, Option<f32>)>,
) -> Vec<(w3cos_std::color::Color, Option<f32>)> {
    let start = stops.first().and_then(|stop| stop.1).unwrap_or(0.0);
    let end = stops.last().and_then(|stop| stop.1).unwrap_or(1.0);
    let period = end - start;
    if period <= f32::EPSILON || period >= 1.0 {
        return stops;
    }
    let mut expanded = Vec::new();
    let first_cycle = ((0.0 - start) / period).floor() as i32;
    let last_cycle = ((1.0 - start) / period).ceil() as i32;
    for cycle in first_cycle..=last_cycle {
        let offset = cycle as f32 * period;
        for (color, position) in &stops {
            let position = position.unwrap_or(start) + offset;
            if (-f32::EPSILON..=1.0 + f32::EPSILON).contains(&position) {
                expanded.push((*color, Some(position.clamp(0.0, 1.0))));
            }
        }
    }
    expanded.sort_by(|left, right| {
        left.1
            .partial_cmp(&right.1)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    expanded
}

fn normalize_gradient_stops(stops: &mut [(w3cos_std::color::Color, Option<f32>)]) {
    if stops[0].1.is_none() {
        stops[0].1 = Some(0.0);
    }
    let last = stops.len() - 1;
    if stops[last].1.is_none() {
        stops[last].1 = Some(1.0);
    }
    let mut anchor = 0;
    while anchor < last {
        let next = (anchor + 1..=last)
            .find(|&index| stops[index].1.is_some())
            .unwrap_or(last);
        let from = stops[anchor].1.unwrap_or(0.0);
        let to = stops[next].1.unwrap_or(1.0).max(from);
        for (index, stop) in stops.iter_mut().enumerate().take(next).skip(anchor + 1) {
            let t = (index - anchor) as f32 / (next - anchor) as f32;
            stop.1 = Some(from + (to - from) * t);
        }
        anchor = next;
    }
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

fn auto_size_axes(value: &str) -> (bool, bool) {
    if matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "cover" | "contain"
    ) {
        return (false, false);
    }
    let parts = value.split_ascii_whitespace().collect::<Vec<_>>();
    (
        parts
            .first()
            .is_none_or(|value| value.eq_ignore_ascii_case("auto")),
        parts
            .get(1)
            .is_none_or(|value| value.eq_ignore_ascii_case("auto")),
    )
}

fn parse_length(value: &str) -> Length {
    let value = value.trim();
    if value.eq_ignore_ascii_case("auto") {
        Length::Auto
    } else if let Some(value) = value.strip_suffix('%') {
        value
            .trim()
            .parse::<f32>()
            .map(|value| Length::Percent(value / 100.0))
            .unwrap_or(Length::Auto)
    } else {
        w3cos_std::style::parse_absolute_length_px(value)
            .map(Length::Px)
            .unwrap_or(Length::Auto)
    }
}

fn resolve_length(length: Length, reference: f32) -> Option<f32> {
    match length {
        Length::Auto => None,
        Length::Px(value) => Some(value.max(0.0)),
        Length::Percent(value) => Some((reference * value).max(0.0)),
    }
}

fn parse_repeat(value: &str) -> (Repeat, Repeat) {
    let value = value.trim().to_ascii_lowercase();
    match value.as_str() {
        "repeat-x" => (Repeat::Repeat, Repeat::NoRepeat),
        "repeat-y" => (Repeat::NoRepeat, Repeat::Repeat),
        _ => {
            let mut parts = value.split_ascii_whitespace();
            let x = parse_repeat_axis(parts.next().unwrap_or("repeat"));
            let y = parts.next().map(parse_repeat_axis).unwrap_or(x);
            (x, y)
        }
    }
}

fn parse_repeat_axis(value: &str) -> Repeat {
    match value {
        "no-repeat" => Repeat::NoRepeat,
        "round" => Repeat::Round,
        "space" => Repeat::Space,
        _ => Repeat::Repeat,
    }
}

fn round_tile_size(area: f32, tile: f32) -> f32 {
    let count = (area / tile).round().max(1.0);
    area / count
}

fn resolve_position(
    value: &str,
    area: LayoutRect,
    width: f32,
    height: f32,
    font_size: f32,
) -> (f32, f32) {
    resolve_position_with_ex(value, area, width, height, font_size, font_size * 0.5)
}

fn resolve_position_with_ex(
    value: &str,
    area: LayoutRect,
    width: f32,
    height: f32,
    font_size: f32,
    ex_size: f32,
) -> (f32, f32) {
    let normalized = value.trim().to_ascii_lowercase();
    let parts = normalized.split_ascii_whitespace().collect::<Vec<_>>();
    if parts.len() >= 3 {
        let (x, y) = parse_edge_position(&parts, font_size, ex_size);
        return (
            area.x + resolve_axis_position(x, area.width - width),
            area.y + resolve_axis_position(y, area.height - height),
        );
    }
    let (x, y) = match parts.as_slice() {
        [] => ("0%", "0%"),
        [one] if matches!(*one, "top" | "bottom") => ("50%", *one),
        [one] if matches!(*one, "left" | "right") => (*one, "50%"),
        [one] => (*one, "50%"),
        [first, second, ..] if matches!(*first, "top" | "bottom") => (*second, *first),
        [first, second, ..] if matches!(*second, "left" | "right") => (*second, *first),
        [first, second, ..] => (*first, *second),
    };
    (
        area.x + position_offset(x, area.width - width, font_size, ex_size),
        area.y + position_offset(y, area.height - height, font_size, ex_size),
    )
}

#[derive(Clone, Copy)]
enum AxisEdge {
    Start,
    Center,
    End,
}

#[derive(Clone, Copy)]
struct AxisPosition {
    edge: AxisEdge,
    offset: Length,
}

fn parse_edge_position(
    parts: &[&str],
    font_size: f32,
    ex_size: f32,
) -> (AxisPosition, AxisPosition) {
    let center = AxisPosition {
        edge: AxisEdge::Center,
        offset: Length::Px(0.0),
    };
    let mut x = center;
    let mut y = center;
    let mut index = 0;
    while index < parts.len() {
        let keyword = parts[index];
        let offset = parts
            .get(index + 1)
            .filter(|value| !is_position_keyword(value))
            .map(|value| parse_position_length(value, font_size, ex_size))
            .unwrap_or(Length::Px(0.0));
        let consumed_offset = parts
            .get(index + 1)
            .is_some_and(|value| !is_position_keyword(value));
        match keyword {
            "left" => {
                x = AxisPosition {
                    edge: AxisEdge::Start,
                    offset,
                }
            }
            "right" => {
                x = AxisPosition {
                    edge: AxisEdge::End,
                    offset,
                }
            }
            "top" => {
                y = AxisPosition {
                    edge: AxisEdge::Start,
                    offset,
                }
            }
            "bottom" => {
                y = AxisPosition {
                    edge: AxisEdge::End,
                    offset,
                }
            }
            "center" => {}
            _ => {}
        }
        index += 1 + usize::from(consumed_offset);
    }
    (x, y)
}

fn is_position_keyword(value: &&str) -> bool {
    matches!(*value, "left" | "right" | "top" | "bottom" | "center")
}

fn resolve_axis_position(position: AxisPosition, free_space: f32) -> f32 {
    let offset = match position.offset {
        Length::Auto => 0.0,
        Length::Px(value) => value,
        Length::Percent(value) => free_space * value,
    };
    match position.edge {
        AxisEdge::Start => offset,
        AxisEdge::Center => free_space * 0.5 + offset,
        AxisEdge::End => free_space - offset,
    }
}

fn position_offset(value: &str, free_space: f32, font_size: f32, ex_size: f32) -> f32 {
    match value.trim().to_ascii_lowercase().as_str() {
        "left" | "top" => 0.0,
        "center" => free_space * 0.5,
        "right" | "bottom" => free_space,
        value if value.ends_with('%') => value[..value.len() - 1]
            .trim()
            .parse::<f32>()
            .map(|value| free_space * value / 100.0)
            .unwrap_or(0.0),
        value => match parse_position_length(value, font_size, ex_size) {
            Length::Px(value) => value,
            Length::Percent(value) => free_space * value,
            Length::Auto => 0.0,
        },
    }
}

fn parse_position_length(value: &str, font_size: f32, ex_size: f32) -> Length {
    let value = value.trim();
    if let Some(value) = value.strip_suffix("rem") {
        return value
            .trim()
            .parse::<f32>()
            .map(|value| Length::Px(value * 16.0))
            .unwrap_or(Length::Auto);
    }
    if let Some(value) = value.strip_suffix("em") {
        return value
            .trim()
            .parse::<f32>()
            .map(|value| Length::Px(value * font_size))
            .unwrap_or(Length::Auto);
    }
    if let Some(value) = value.strip_suffix("ex") {
        return value
            .trim()
            .parse::<f32>()
            .map(|value| Length::Px(value * ex_size))
            .unwrap_or(Length::Auto);
    }
    parse_length(value)
}

fn style_ex_size(style: &Style) -> f32 {
    #[cfg(feature = "skia")]
    if let Some(typeface) = crate::font_face::FontRegistry::global()
        .resolve_style(style)
        .and_then(|font| font.skia_typeface())
    {
        let (_, metrics) = skia_safe::Font::new(typeface, style.font_size).metrics();
        if metrics.x_height > 0.0 {
            return metrics.x_height;
        }
    }

    let ahem = style.font_family.as_deref().is_some_and(|family| {
        family.split(',').any(|name| {
            name.trim()
                .trim_matches(['"', '\''])
                .eq_ignore_ascii_case("ahem")
        })
    });
    style.font_size * if ahem { 0.8 } else { 0.5 }
}

fn axis_tiles(
    area_start: f32,
    area_length: f32,
    paint_start: f32,
    paint_length: f32,
    positioned_start: f32,
    tile_length: f32,
    repeat: Repeat,
) -> Vec<f32> {
    if tile_length <= 0.0 {
        return Vec::new();
    }
    match repeat {
        Repeat::NoRepeat => vec![positioned_start],
        Repeat::Space => {
            let count = (area_length / tile_length).floor() as usize;
            if count < 2 {
                vec![positioned_start]
            } else {
                let gap = (area_length - count as f32 * tile_length) / (count - 1) as f32;
                (0..count)
                    .map(|index| area_start + index as f32 * (tile_length + gap))
                    .collect()
            }
        }
        Repeat::Repeat | Repeat::Round => {
            let paint_end = paint_start + paint_length;
            let mut start = positioned_start;
            while start > paint_start {
                start -= tile_length;
            }
            while start + tile_length <= paint_start {
                start += tile_length;
            }
            let mut values = Vec::new();
            while start < paint_end && values.len() < MAX_BACKGROUND_TILES_PER_LAYER {
                values.push(start);
                start += tile_length;
            }
            values
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn install_image(source: &str, width: u32, height: u32) {
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            width,
            height,
            image::Rgba([1, 2, 3, 255]),
        ))
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
        crate::image_loader::decode_and_install(source, &bytes.into_inner()).unwrap();
    }

    #[test]
    fn cover_center_and_no_repeat_produce_one_shared_tile() {
        crate::image_loader::clear_cache();
        install_image("hero.png", 100, 50);
        let style = Style {
            background_image: Some("url(hero.png)".to_string()),
            background_size: Some("cover".to_string()),
            background_position: Some("center".to_string()),
            background_repeat: Some("no-repeat".to_string()),
            ..Style::default()
        };
        let layers = raster_background_layers(
            &style,
            LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            },
        );
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0].tiles.len(), 1);
        let tile = layers[0].tiles[0];
        assert_eq!(
            (tile.x, tile.y, tile.width, tile.height),
            (-50.0, 0.0, 200.0, 100.0)
        );
    }

    #[test]
    fn canvas_negative_em_position_can_move_a_root_tile_to_the_viewport_origin() {
        crate::image_loader::clear_cache();
        install_image("diamond.png", 10, 10);
        let style = Style {
            background_image: Some("url(diamond.png)".to_string()),
            background_position: Some("-2em -2em".to_string()),
            background_repeat: Some("no-repeat".to_string()),
            font_size: 16.0,
            ..Style::default()
        };
        let layers = canvas_background_paint_layers(
            &style,
            LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 800.0,
                height: 600.0,
            },
            Some(LayoutRect {
                x: 32.0,
                y: 32.0,
                width: 736.0,
                height: 200.0,
            }),
        );

        assert_eq!(layers.len(), 1);
        let BackgroundPaintLayer::Raster(layer) = &layers[0] else {
            panic!("expected raster layer");
        };
        assert_eq!(layer.tiles[0].x, 0.0);
        assert_eq!(layer.tiles[0].y, 0.0);
    }

    #[test]
    fn repeat_and_content_clip_use_box_model_edges() {
        crate::image_loader::clear_cache();
        install_image("tile.png", 10, 10);
        let style = Style {
            background_image: Some("url(tile.png)".to_string()),
            background_repeat: Some("repeat".to_string()),
            background_origin: Some("content-box".to_string()),
            background_clip: Some("content-box".to_string()),
            border_width: 2.0,
            padding: w3cos_std::style::Edges::all(3.0),
            ..Style::default()
        };
        let layers = raster_background_layers(
            &style,
            LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 30.0,
                height: 30.0,
            },
        );
        assert_eq!(layers[0].clip.rect.x, 5.0);
        assert_eq!(layers[0].clip.rect.y, 5.0);
        assert_eq!(layers[0].clip.rect.width, 20.0);
        assert_eq!(layers[0].tiles.len(), 4);
    }

    #[test]
    fn repeat_extends_from_padding_origin_across_border_clip() {
        crate::image_loader::clear_cache();
        install_image("tile-border.png", 10, 10);
        let style = Style {
            background_image: Some("url(tile-border.png)".to_string()),
            background_repeat: Some("repeat".to_string()),
            background_origin: Some("padding-box".to_string()),
            background_clip: Some("border-box".to_string()),
            border_width: 2.0,
            ..Style::default()
        };
        let layers = raster_background_layers(
            &style,
            LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 30.0,
                height: 30.0,
            },
        );
        assert_eq!(layers[0].clip.rect.x, 0.0);
        assert_eq!(layers[0].tiles[0].x, -8.0);
        assert_eq!(layers[0].tiles[0].y, -8.0);
    }

    #[test]
    fn round_preserves_aspect_ratio_on_the_auto_axis() {
        crate::image_loader::clear_cache();
        install_image("round.png", 30, 10);
        let style = Style {
            background_image: Some("url(round.png)".to_string()),
            background_size: Some("auto".to_string()),
            background_repeat: Some("round no-repeat".to_string()),
            ..Style::default()
        };
        let layers = raster_background_layers(
            &style,
            LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 80.0,
            },
        );
        let tile = layers[0].tiles[0];
        assert!((tile.width - (100.0 / 3.0)).abs() < 0.001);
        assert!((tile.height - (100.0 / 9.0)).abs() < 0.001);
    }

    #[test]
    fn edge_offset_position_is_resolved_inward_from_named_edges() {
        let area = LayoutRect {
            x: 10.0,
            y: 20.0,
            width: 200.0,
            height: 100.0,
        };
        assert_eq!(
            resolve_position("right 30px bottom 10px", area, 50.0, 20.0, 16.0),
            (130.0, 90.0)
        );
        assert_eq!(
            resolve_position("bottom 25% left 5px", area, 50.0, 20.0, 16.0),
            (15.0, 80.0)
        );
    }

    #[test]
    fn two_keyword_position_accepts_vertical_then_horizontal_order() {
        let area = LayoutRect {
            x: 8.0,
            y: 51.0,
            width: 288.0,
            height: 288.0,
        };

        assert_eq!(
            resolve_position("center left", area, 96.0, 96.0, 16.0),
            (8.0, 147.0)
        );
    }

    #[test]
    fn absolute_units_are_resolved_for_two_axis_background_positions() {
        let area = LayoutRect {
            x: 8.0,
            y: 12.0,
            width: 200.0,
            height: 100.0,
        };
        assert_eq!(
            resolve_position("0.5in 0.5in", area, 15.0, 15.0, 16.0),
            (56.0, 60.0)
        );
    }

    #[test]
    fn font_relative_units_are_resolved_for_background_positions() {
        let area = LayoutRect {
            x: 8.0,
            y: 12.0,
            width: 200.0,
            height: 100.0,
        };
        assert_eq!(
            resolve_position("1em 0", area, 64.0, 64.0, 16.0),
            (24.0, 12.0)
        );
        assert_eq!(
            resolve_position("1.5rem 0", area, 64.0, 64.0, 20.0),
            (32.0, 12.0)
        );
        assert_eq!(
            resolve_position_with_ex("0 6.25ex", area, 1.0, 1.0, 40.0, 32.0),
            (8.0, 212.0)
        );
    }

    #[test]
    fn svg_auto_background_size_uses_intrinsic_dimension_semantics() {
        let area = LayoutRect {
            x: 0.0,
            y: 0.0,
            width: 80.0,
            height: 100.0,
        };
        let auto = Length::Auto;
        assert_eq!(
            resolve_size(
                "auto",
                area,
                Some(IntrinsicSize {
                    width: auto,
                    height: auto,
                    ratio: None,
                }),
            ),
            (80.0, 100.0)
        );
        let portrait = resolve_size(
            "auto",
            area,
            Some(IntrinsicSize {
                width: auto,
                height: auto,
                ratio: Some(4.0 / 6.0),
            }),
        );
        assert!((portrait.0 - 100.0 * 4.0 / 6.0).abs() < 0.001);
        assert_eq!(portrait.1, 100.0);
        let percentages = resolve_size(
            "auto",
            area,
            Some(IntrinsicSize {
                width: Length::Percent(0.4),
                height: Length::Percent(0.6),
                ratio: None,
            }),
        );
        assert!((percentages.0 - 32.0).abs() < 0.001);
        assert!((percentages.1 - 60.0).abs() < 0.001);
    }

    #[test]
    fn content_clip_reduces_shared_corner_radius() {
        let style = Style {
            border_radius: 12.0,
            border_width: 2.0,
            padding: w3cos_std::style::Edges::all(3.0),
            ..Style::default()
        };
        assert_eq!(background_radius(&style, BoxKind::Border), 12.0);
        assert_eq!(background_radius(&style, BoxKind::Padding), 10.0);
        assert_eq!(background_radius(&style, BoxKind::Content), 7.0);
    }

    #[test]
    fn gradients_share_layer_size_position_repeat_and_clip_geometry() {
        let style = Style {
            background_image: Some("linear-gradient(90deg, red 0%, blue 100%)".to_string()),
            background_size: Some("20px 10px".to_string()),
            background_position: Some("right 5px bottom 3px".to_string()),
            background_repeat: Some("no-repeat".to_string()),
            background_clip: Some("padding-box".to_string()),
            border_width: 2.0,
            border_radius: 9.0,
            ..Style::default()
        };
        let layers = gradient_background_layers(
            &style,
            LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 50.0,
            },
        );
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0].geometry.tiles.len(), 1);
        let tile = layers[0].geometry.tiles[0];
        assert_eq!(
            (tile.x, tile.y, tile.width, tile.height),
            (73.0, 35.0, 20.0, 10.0)
        );
        assert_eq!(layers[0].geometry.clip.rect.x, 2.0);
        assert_eq!(layers[0].geometry.clip.radius, 7.0);
    }

    #[test]
    fn layered_gradient_parser_preserves_function_colors_and_normalizes_stops() {
        let style = Style {
            background_image: Some(
                "radial-gradient(circle at 85% 8%, rgba(22, 119, 255, 0.18), transparent 34%), linear-gradient(160deg, #f7faff 0%, #eef3fb 100%)"
                    .to_string(),
            ),
            ..Style::default()
        };
        let layers = gradient_background_layers(
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
    fn repeating_directional_and_shaped_gradients_normalize_once() {
        let style = Style {
            background_image: Some(
                "repeating-linear-gradient(to right, red 0% 10%, blue 20%), \
                 radial-gradient(circle at right bottom, white, black)"
                    .to_string(),
            ),
            background_blend_mode: Some("multiply, screen".to_string()),
            ..Style::default()
        };
        let layers = gradient_background_layers(
            &style,
            LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 50.0,
            },
        );
        assert_eq!(layers.len(), 2);
        assert!(layers[0].stops.len() > 4);
        assert!(matches!(
            layers[0].kind,
            GradientKind::Linear {
                angle_degrees: 90.0
            }
        ));
        assert!(matches!(
            layers[1].kind,
            GradientKind::Radial {
                center_x: 1.0,
                center_y: 1.0,
                shape: RadialShape::Circle
            }
        ));
        assert_eq!(layers[0].blend_mode, BackgroundBlendMode::Multiply);
        assert_eq!(layers[1].blend_mode, BackgroundBlendMode::Screen);
    }

    #[test]
    fn fixed_attachment_anchors_tile_phase_to_viewport_origin() {
        let style = Style {
            background_image: Some("linear-gradient(red, blue)".to_string()),
            background_size: Some("20px 20px".to_string()),
            background_repeat: Some("repeat".to_string()),
            background_attachment: Some("fixed".to_string()),
            ..Style::default()
        };
        let layers = gradient_background_layers(
            &style,
            LayoutRect {
                x: 37.0,
                y: 53.0,
                width: 100.0,
                height: 100.0,
            },
        );
        assert_eq!(layers[0].geometry.tiles[0].x, 20.0);
        assert_eq!(layers[0].geometry.tiles[0].y, 40.0);
    }
}
