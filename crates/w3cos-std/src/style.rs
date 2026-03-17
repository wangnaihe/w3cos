use serde::{Deserialize, Serialize};
use crate::color::Color;

/// CSS Modern Subset — Flexbox, Grid, Block, Inline, and positioning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Style {
    // Layout mode
    pub display: Display,
    pub position: Position,

    // Flexbox
    pub flex_direction: FlexDirection,
    pub justify_content: JustifyContent,
    pub align_items: AlignItems,
    pub flex_wrap: FlexWrap,
    pub flex_grow: f32,
    pub flex_shrink: f32,

    // Position offsets (for relative/absolute/fixed)
    pub top: Dimension,
    pub right: Dimension,
    pub bottom: Dimension,
    pub left: Dimension,
    pub z_index: i32,

    // Spacing
    pub gap: f32,
    pub padding: Edges,
    pub margin: Edges,

    // Sizing
    pub width: Dimension,
    pub height: Dimension,
    pub min_width: Dimension,
    pub min_height: Dimension,
    pub max_width: Dimension,
    pub max_height: Dimension,

    // Overflow
    pub overflow: Overflow,

    // Visual
    pub background: Color,
    pub color: Color,
    pub font_size: f32,
    pub font_weight: u16,
    pub border_radius: f32,
    pub border_width: f32,
    pub border_color: Color,
    pub opacity: f32,

    // Box Shadow
    pub box_shadow: Option<BoxShadow>,

    // Transform
    pub transform: Transform2D,

    // Transition (property, duration_ms, easing)
    pub transition: Option<Transition>,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            display: Display::Flex,
            position: Position::Relative,
            flex_direction: FlexDirection::Column,
            justify_content: JustifyContent::FlexStart,
            align_items: AlignItems::Stretch,
            flex_wrap: FlexWrap::NoWrap,
            flex_grow: 0.0,
            flex_shrink: 1.0,
            top: Dimension::Auto,
            right: Dimension::Auto,
            bottom: Dimension::Auto,
            left: Dimension::Auto,
            z_index: 0,
            gap: 0.0,
            padding: Edges::ZERO,
            margin: Edges::ZERO,
            width: Dimension::Auto,
            height: Dimension::Auto,
            min_width: Dimension::Auto,
            min_height: Dimension::Auto,
            max_width: Dimension::Auto,
            max_height: Dimension::Auto,
            overflow: Overflow::Visible,
            background: Color::TRANSPARENT,
            color: Color::WHITE,
            font_size: 16.0,
            font_weight: 400,
            border_radius: 0.0,
            border_width: 0.0,
            border_color: Color::TRANSPARENT,
            opacity: 1.0,
            box_shadow: None,
            transform: Transform2D::IDENTITY,
            transition: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub enum Display {
    Block,
    #[default]
    Flex,
    Grid,
    Inline,
    InlineBlock,
    None,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub enum Position {
    #[default]
    Relative,
    Absolute,
    Fixed,
    Sticky,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub enum Overflow {
    #[default]
    Visible,
    Hidden,
    Scroll,
    Auto,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub enum FlexDirection {
    Row,
    #[default]
    Column,
    RowReverse,
    ColumnReverse,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub enum JustifyContent {
    #[default]
    FlexStart,
    FlexEnd,
    Center,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub enum AlignItems {
    FlexStart,
    FlexEnd,
    Center,
    #[default]
    Stretch,
    Baseline,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub enum FlexWrap {
    #[default]
    NoWrap,
    Wrap,
    WrapReverse,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum Dimension {
    Auto,
    Px(f32),
    Percent(f32),
    Rem(f32),
    Em(f32),
    Vw(f32),
    Vh(f32),
}

impl Default for Dimension {
    fn default() -> Self {
        Self::Auto
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Edges {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl Edges {
    pub const ZERO: Self = Self { top: 0.0, right: 0.0, bottom: 0.0, left: 0.0 };

    pub const fn all(v: f32) -> Self {
        Self { top: v, right: v, bottom: v, left: v }
    }

    pub const fn xy(x: f32, y: f32) -> Self {
        Self { top: y, right: x, bottom: y, left: x }
    }
}

impl Default for Edges {
    fn default() -> Self {
        Self::ZERO
    }
}

// --- Box Shadow ---

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct BoxShadow {
    pub offset_x: f32,
    pub offset_y: f32,
    pub blur_radius: f32,
    pub spread_radius: f32,
    pub color: Color,
    pub inset: bool,
}

impl BoxShadow {
    pub fn new(ox: f32, oy: f32, blur: f32, spread: f32, color: Color) -> Self {
        Self { offset_x: ox, offset_y: oy, blur_radius: blur, spread_radius: spread, color, inset: false }
    }
}

// --- Transform ---

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Transform2D {
    pub translate_x: f32,
    pub translate_y: f32,
    pub scale_x: f32,
    pub scale_y: f32,
    pub rotate_deg: f32,
}

impl Transform2D {
    pub const IDENTITY: Self = Self {
        translate_x: 0.0, translate_y: 0.0,
        scale_x: 1.0, scale_y: 1.0,
        rotate_deg: 0.0,
    };

    pub fn is_identity(&self) -> bool {
        self.translate_x == 0.0 && self.translate_y == 0.0
            && self.scale_x == 1.0 && self.scale_y == 1.0
            && self.rotate_deg == 0.0
    }
}

impl Default for Transform2D {
    fn default() -> Self { Self::IDENTITY }
}

// --- Transition ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transition {
    pub property: TransitionProperty,
    pub duration_ms: u32,
    pub easing: Easing,
    pub delay_ms: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TransitionProperty {
    All,
    Opacity,
    Transform,
    Background,
    Color,
    Custom(String),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub enum Easing {
    #[default]
    Ease,
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
    CubicBezier(f32, f32, f32, f32),
}

impl Easing {
    pub fn interpolate(&self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Easing::Linear => t,
            Easing::Ease => cubic_bezier(0.25, 0.1, 0.25, 1.0, t),
            Easing::EaseIn => cubic_bezier(0.42, 0.0, 1.0, 1.0, t),
            Easing::EaseOut => cubic_bezier(0.0, 0.0, 0.58, 1.0, t),
            Easing::EaseInOut => cubic_bezier(0.42, 0.0, 0.58, 1.0, t),
            Easing::CubicBezier(x1, y1, x2, y2) => cubic_bezier(*x1, *y1, *x2, *y2, t),
        }
    }
}

fn cubic_bezier(_x1: f32, y1: f32, _x2: f32, y2: f32, t: f32) -> f32 {
    // Simple approximation: sample the curve
    let ct = 1.0 - t;
    let ct2 = ct * ct;
    let t2 = t * t;
    3.0 * ct2 * t * y1 + 3.0 * ct * t2 * y2 + t2 * t
}

// --- Dimension resolution ---

impl Dimension {
    pub fn resolve(&self, parent_size: f32, root_font_size: f32, local_font_size: f32, viewport_w: f32, viewport_h: f32) -> Option<f32> {
        match self {
            Dimension::Auto => None,
            Dimension::Px(v) => Some(*v),
            Dimension::Percent(v) => Some(parent_size * v / 100.0),
            Dimension::Rem(v) => Some(*v * root_font_size),
            Dimension::Em(v) => Some(*v * local_font_size),
            Dimension::Vw(v) => Some(*v * viewport_w / 100.0),
            Dimension::Vh(v) => Some(*v * viewport_h / 100.0),
        }
    }
}
