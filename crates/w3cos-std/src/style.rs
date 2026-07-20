use crate::color::Color;
use serde::{Deserialize, Serialize};

pub use crate::safe_area::{SafeAreaEdge, SafeAreaInsets};

/// CSS Modern Subset — Flexbox, Grid, Block, Inline, and positioning.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// CSS Overscroll Behavior Level 1, block-axis subset.
    #[serde(default)]
    pub overscroll_behavior: OverscrollBehavior,
    /// CSS Scroll Snap Level 2 `scroll-initial-target`.
    #[serde(default)]
    pub scroll_initial_target: ScrollInitialTarget,
    /// CSS `overflow-anchor`; `false` excludes this subtree from UA scroll anchoring.
    #[serde(default = "default_overflow_anchor")]
    pub overflow_anchor: bool,

    // Visual
    pub background: Color,
    pub color: Color,
    pub font_size: f32,
    pub font_weight: u16,
    pub border_radius: f32,
    pub border_width: f32,
    pub border_color: Color,
    pub opacity: f32,

    // CSS Text (#31)
    pub text_align: TextAlign,
    pub white_space: WhiteSpace,
    pub line_height: f32,
    pub letter_spacing: f32,
    pub text_decoration: TextDecoration,
    pub text_overflow: TextOverflow,
    pub font_family: Option<String>,
    pub font_style: FontStyle,
    pub word_break: WordBreak,

    // CSS Custom Properties (#34)
    pub custom_properties: Option<std::collections::HashMap<String, String>>,

    // CSS Containment — layout isolation boundaries (Chrome-inspired)
    pub contain: Contain,

    /// CSS `will-change` — UA compositor layer promotion hint.
    pub will_change: WillChange,

    /// CSS `filter` — stored raw; non-none values promote compositor layers.
    pub filter: Option<String>,

    // Box Shadow
    pub box_shadow: Option<BoxShadow>,

    // Transform
    pub transform: Transform2D,

    // Transition (property, duration_ms, easing)
    pub transition: Option<Transition>,

    // CSS Animation (#11)
    pub animation: Option<Animation>,

    // Additional layout properties
    pub flex_basis: Dimension,
    pub order: i32,
    pub align_self: AlignSelf,
    pub align_content: AlignContent,

    // Interaction
    pub cursor: Cursor,
    pub pointer_events: PointerEvents,
    pub user_select: UserSelect,

    // Visibility
    pub visibility: Visibility,

    // Outline
    pub outline_width: f32,
    pub outline_color: Color,
    pub outline_style: OutlineStyle,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            display: Display::Flex,
            position: Position::Static,
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
            overscroll_behavior: OverscrollBehavior::Auto,
            scroll_initial_target: ScrollInitialTarget::None,
            overflow_anchor: true,
            background: Color::TRANSPARENT,
            color: Color::WHITE,
            font_size: 16.0,
            font_weight: 400,
            border_radius: 0.0,
            border_width: 0.0,
            border_color: Color::TRANSPARENT,
            opacity: 1.0,
            text_align: TextAlign::Left,
            white_space: WhiteSpace::Normal,
            line_height: 1.2,
            letter_spacing: 0.0,
            text_decoration: TextDecoration::None,
            text_overflow: TextOverflow::Clip,
            font_family: None,
            font_style: FontStyle::Normal,
            word_break: WordBreak::Normal,
            custom_properties: None,
            contain: Contain::None,
            will_change: WillChange::default(),
            filter: None,
            box_shadow: None,
            transform: Transform2D::IDENTITY,
            transition: None,
            animation: None,
            flex_basis: Dimension::Auto,
            order: 0,
            align_self: AlignSelf::Auto,
            align_content: AlignContent::Stretch,
            cursor: Cursor::Default,
            pointer_events: PointerEvents::Auto,
            user_select: UserSelect::Auto,
            visibility: Visibility::Visible,
            outline_width: 0.0,
            outline_color: Color::TRANSPARENT,
            outline_style: OutlineStyle::None,
        }
    }
}

const fn default_overflow_anchor() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Display {
    Block,
    #[default]
    Flex,
    Grid,
    Inline,
    InlineBlock,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum Position {
    #[default]
    Static,
    Relative,
    Absolute,
    Fixed,
    Sticky,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum Overflow {
    #[default]
    Visible,
    Hidden,
    Scroll,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum OverscrollBehavior {
    #[default]
    Auto,
    Contain,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ScrollInitialTarget {
    #[default]
    None,
    Nearest,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum FlexDirection {
    Row,
    #[default]
    Column,
    RowReverse,
    ColumnReverse,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum JustifyContent {
    #[default]
    FlexStart,
    FlexEnd,
    Center,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum AlignItems {
    FlexStart,
    FlexEnd,
    Center,
    #[default]
    Stretch,
    Baseline,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum FlexWrap {
    #[default]
    NoWrap,
    Wrap,
    WrapReverse,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub enum Dimension {
    #[default]
    Auto,
    Px(f32),
    Percent(f32),
    Rem(f32),
    Em(f32),
    Vw(f32),
    Vh(f32),
}

/// CSS length for padding/margin — `px`, `env()`, or `calc(px + env())`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Spacing {
    Px(f32),
    SafeAreaInset(SafeAreaEdge),
    /// `env(keyboard-inset-height)` — virtual keyboard occlusion (logical px).
    KeyboardInsetHeight,
    /// `calc(Npx + env(...))` — one optional `safe-area` and/or `keyboard-inset` term.
    Composite {
        px: f32,
        #[serde(default)]
        safe_area: Option<SafeAreaEdge>,
        #[serde(default)]
        keyboard_inset: bool,
    },
}

impl Spacing {
    pub fn resolve(&self, insets: &SafeAreaInsets) -> f32 {
        self.resolve_env(insets, crate::keyboard_inset::bottom())
    }

    pub fn resolve_env(&self, insets: &SafeAreaInsets, keyboard_bottom: f32) -> f32 {
        match self {
            Spacing::Px(v) => *v,
            Spacing::SafeAreaInset(edge) => insets.value(*edge),
            Spacing::KeyboardInsetHeight => keyboard_bottom,
            Spacing::Composite {
                px,
                safe_area,
                keyboard_inset,
            } => {
                *px + safe_area.map(|e| insets.value(e)).unwrap_or(0.0)
                    + if *keyboard_inset {
                        keyboard_bottom
                    } else {
                        0.0
                    }
            }
        }
    }
}

impl From<f32> for Spacing {
    fn from(v: f32) -> Self {
        Spacing::Px(v)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EdgeLengths {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Edges {
    pub top: Spacing,
    pub right: Spacing,
    pub bottom: Spacing,
    pub left: Spacing,
}

impl Edges {
    pub const ZERO: Self = Self {
        top: Spacing::Px(0.0),
        right: Spacing::Px(0.0),
        bottom: Spacing::Px(0.0),
        left: Spacing::Px(0.0),
    };

    pub const fn all(v: f32) -> Self {
        Self {
            top: Spacing::Px(v),
            right: Spacing::Px(v),
            bottom: Spacing::Px(v),
            left: Spacing::Px(v),
        }
    }

    pub const fn xy(x: f32, y: f32) -> Self {
        Self {
            top: Spacing::Px(y),
            right: Spacing::Px(x),
            bottom: Spacing::Px(y),
            left: Spacing::Px(x),
        }
    }

    pub fn resolve_lengths(&self, insets: &SafeAreaInsets) -> EdgeLengths {
        EdgeLengths {
            top: self.top.resolve(insets),
            right: self.right.resolve(insets),
            bottom: self.bottom.resolve(insets),
            left: self.left.resolve(insets),
        }
    }
}

impl Style {
    pub fn padding_lengths(&self) -> EdgeLengths {
        self.padding.resolve_lengths(&crate::safe_area::current())
    }

    pub fn margin_lengths(&self) -> EdgeLengths {
        self.margin.resolve_lengths(&crate::safe_area::current())
    }
}

impl Default for Edges {
    fn default() -> Self {
        Self::ZERO
    }
}

// --- CSS Containment ---

/// CSS `contain` property — creates layout isolation boundaries.
/// Enables incremental layout: changes inside a contained subtree
/// cannot affect layout outside it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum Contain {
    #[default]
    None,
    /// Layout isolation: child layout cannot affect parent.
    Layout,
    /// Size isolation: element has intrinsic size, children don't affect it.
    Size,
    /// Both layout and paint containment.
    Content,
    /// Layout + size + paint + style containment (strongest).
    Strict,
}

/// CSS `will-change` — hints the UA to promote a compositor layer early.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct WillChange {
    pub transform: bool,
    pub opacity: bool,
    pub filter: bool,
    pub scroll_position: bool,
}

impl WillChange {
    pub fn from_css(value: &str) -> Self {
        let v = value.trim().to_lowercase();
        if v.is_empty() || v == "auto" {
            return Self::default();
        }
        let mut wc = Self::default();
        for part in v.split(',') {
            match part.trim() {
                "transform" => wc.transform = true,
                "opacity" => wc.opacity = true,
                "filter" => wc.filter = true,
                "scroll-position" => wc.scroll_position = true,
                _ => {}
            }
        }
        wc
    }

    pub fn promotes_layer(&self) -> bool {
        self.transform || self.opacity || self.filter
    }
}

impl Contain {
    pub fn from_css(value: &str) -> Self {
        let v = value.trim().to_lowercase();
        if v.contains("strict") {
            Self::Strict
        } else if v.contains("content") {
            Self::Content
        } else if v.contains("layout") && v.contains("size") {
            Self::Strict
        } else if v.contains("layout") {
            Self::Layout
        } else if v.contains("size") {
            Self::Size
        } else if v.contains("paint") {
            Self::Content
        } else {
            Self::None
        }
    }

    pub fn has_paint_containment(&self) -> bool {
        matches!(self, Self::Content | Self::Strict)
    }
}

// --- CSS Text (#31) ---

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
    Justify,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum WhiteSpace {
    #[default]
    Normal,
    NoWrap,
    Pre,
    PreWrap,
    PreLine,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum TextDecoration {
    #[default]
    None,
    Underline,
    LineThrough,
    Overline,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum TextOverflow {
    #[default]
    Clip,
    Ellipsis,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum FontStyle {
    #[default]
    Normal,
    Italic,
    Oblique,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum WordBreak {
    #[default]
    Normal,
    BreakAll,
    BreakWord,
    KeepAll,
}

// --- CSS Animation (#11) ---

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Animation {
    pub name: String,
    pub duration_ms: u32,
    pub easing: Easing,
    pub delay_ms: u32,
    pub iteration_count: AnimationIterationCount,
    pub direction: AnimationDirection,
    pub fill_mode: AnimationFillMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum AnimationIterationCount {
    #[default]
    Once,
    Count(u32),
    Infinite,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum AnimationDirection {
    #[default]
    Normal,
    Reverse,
    Alternate,
    AlternateReverse,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum AnimationFillMode {
    #[default]
    None,
    Forwards,
    Backwards,
    Both,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Keyframe {
    pub offset: f32,
    pub style: KeyframeStyle,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KeyframeStyle {
    pub opacity: Option<f32>,
    pub background: Option<Color>,
    pub transform: Option<Transform2D>,
    pub color: Option<Color>,
}

// --- Box Shadow ---

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
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
        Self {
            offset_x: ox,
            offset_y: oy,
            blur_radius: blur,
            spread_radius: spread,
            color,
            inset: false,
        }
    }
}

// --- Transform ---

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Transform2D {
    pub translate_x: f32,
    pub translate_y: f32,
    pub scale_x: f32,
    pub scale_y: f32,
    pub rotate_deg: f32,
}

impl Transform2D {
    pub const IDENTITY: Self = Self {
        translate_x: 0.0,
        translate_y: 0.0,
        scale_x: 1.0,
        scale_y: 1.0,
        rotate_deg: 0.0,
    };

    pub fn is_identity(&self) -> bool {
        self.translate_x == 0.0
            && self.translate_y == 0.0
            && self.scale_x == 1.0
            && self.scale_y == 1.0
            && self.rotate_deg == 0.0
    }
}

impl Default for Transform2D {
    fn default() -> Self {
        Self::IDENTITY
    }
}

// --- Transition ---

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    pub property: TransitionProperty,
    pub duration_ms: u32,
    pub easing: Easing,
    pub delay_ms: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TransitionProperty {
    All,
    Opacity,
    Transform,
    Background,
    Color,
    Custom(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
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

fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, x: f32) -> f32 {
    fn sample(a1: f32, a2: f32, t: f32) -> f32 {
        ((1.0 - 3.0 * a2 + 3.0 * a1) * t + (3.0 * a2 - 6.0 * a1)) * t * t + 3.0 * a1 * t
    }

    fn slope(a1: f32, a2: f32, t: f32) -> f32 {
        3.0 * (1.0 - 3.0 * a2 + 3.0 * a1) * t * t + 2.0 * (3.0 * a2 - 6.0 * a1) * t + 3.0 * a1
    }

    // CSS timing functions map time through the curve's x axis; evaluating
    // y directly at `t` ignores both x control points. Follow browser engines:
    // Newton iteration for the common case, with bisection for flat slopes.
    let mut curve_t = x;
    for _ in 0..8 {
        let error = sample(x1, x2, curve_t) - x;
        if error.abs() < 1.0e-6 {
            return sample(y1, y2, curve_t);
        }
        let derivative = slope(x1, x2, curve_t);
        if derivative.abs() < 1.0e-6 {
            break;
        }
        curve_t = (curve_t - error / derivative).clamp(0.0, 1.0);
    }

    let (mut low, mut high) = (0.0, 1.0);
    for _ in 0..12 {
        curve_t = (low + high) * 0.5;
        if sample(x1, x2, curve_t) < x {
            low = curve_t;
        } else {
            high = curve_t;
        }
    }
    sample(y1, y2, curve_t)
}

#[cfg(test)]
mod easing_tests {
    use super::Easing;

    #[test]
    fn css_easing_solves_the_curve_x_axis() {
        let midpoint = Easing::Ease.interpolate(0.5);
        assert!((midpoint - 0.802).abs() < 0.002, "midpoint={midpoint}");
        assert_eq!(Easing::Ease.interpolate(0.0), 0.0);
        assert_eq!(Easing::Ease.interpolate(1.0), 1.0);
    }
}

// --- New enums for Phase 3 ---

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum AlignSelf {
    #[default]
    Auto,
    FlexStart,
    FlexEnd,
    Center,
    Baseline,
    Stretch,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum AlignContent {
    FlexStart,
    FlexEnd,
    Center,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
    #[default]
    Stretch,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum Cursor {
    #[default]
    Default,
    Pointer,
    Text,
    Move,
    Grab,
    Grabbing,
    NotAllowed,
    Crosshair,
    Help,
    Wait,
    Progress,
    ColResize,
    RowResize,
    NResize,
    EResize,
    SResize,
    WResize,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum PointerEvents {
    #[default]
    Auto,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum UserSelect {
    #[default]
    Auto,
    None,
    Text,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum Visibility {
    #[default]
    Visible,
    Hidden,
    Collapse,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum OutlineStyle {
    #[default]
    None,
    Solid,
    Dashed,
    Dotted,
    Double,
    Groove,
    Ridge,
    Inset,
    Outset,
}

// --- Dimension resolution ---

impl Dimension {
    pub fn resolve(
        &self,
        parent_size: f32,
        root_font_size: f32,
        local_font_size: f32,
        viewport_w: f32,
        viewport_h: f32,
    ) -> Option<f32> {
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
