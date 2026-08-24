use crate::color::Color;
use serde::{Deserialize, Serialize};

pub use crate::safe_area::{SafeAreaEdge, SafeAreaInsets};

/// Resolve CSS absolute lengths to canonical CSS pixels (96 px per inch).
/// Unitless values remain accepted for the runtime's existing native-style
/// compatibility; CSS declaration validation decides where only zero is legal.
pub fn parse_absolute_length_px(value: &str) -> Option<f32> {
    let value = value.trim();
    for (suffix, pixels_per_unit) in [
        ("px", 1.0_f32),
        ("cm", 96.0 / 2.54),
        ("mm", 96.0 / 25.4),
        ("q", 96.0 / 101.6),
        ("in", 96.0),
        ("pt", 96.0 / 72.0),
        ("pc", 16.0),
    ] {
        if let Some(number) = value.strip_suffix(suffix)
            && let Ok(number) = number.trim().parse::<f32>()
        {
            return Some(number * pixels_per_unit);
        }
    }
    value.parse().ok()
}

/// CSS Modern Subset — Flexbox, Grid, Block, Inline, and positioning.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Style {
    // Layout mode
    pub display: Display,
    pub position: Position,
    #[serde(default)]
    pub float: Float,

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
    #[serde(default)]
    pub row_gap: Option<f32>,
    #[serde(default)]
    pub column_gap: Option<f32>,
    pub padding: Edges,
    pub margin: Edges,

    // Sizing
    #[serde(default)]
    pub box_sizing: BoxSizing,
    pub width: Dimension,
    pub height: Dimension,
    pub min_width: Dimension,
    pub min_height: Dimension,
    pub max_width: Dimension,
    pub max_height: Dimension,

    // Overflow
    pub overflow: Overflow,
    #[serde(default)]
    pub overflow_x: Option<Overflow>,
    #[serde(default)]
    pub overflow_y: Option<Overflow>,
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
    /// Raw CSS image/gradient layers. Solid backgrounds continue to use `background`.
    #[serde(default)]
    pub background_image: Option<String>,
    #[serde(default)]
    pub background_size: Option<String>,
    #[serde(default)]
    pub background_position: Option<String>,
    #[serde(default)]
    pub background_repeat: Option<String>,
    #[serde(default)]
    pub background_origin: Option<String>,
    #[serde(default)]
    pub background_clip: Option<String>,
    #[serde(default)]
    pub background_attachment: Option<String>,
    #[serde(default)]
    pub background_blend_mode: Option<String>,
    pub color: Color,
    pub font_size: f32,
    pub font_weight: u16,
    pub border_radius: f32,
    #[serde(default)]
    pub border_top_left_radius: Option<f32>,
    #[serde(default)]
    pub border_top_right_radius: Option<f32>,
    #[serde(default)]
    pub border_bottom_right_radius: Option<f32>,
    #[serde(default)]
    pub border_bottom_left_radius: Option<f32>,
    pub border_width: f32,
    pub border_color: Color,
    #[serde(default)]
    pub border_top_width: Option<f32>,
    #[serde(default)]
    pub border_right_width: Option<f32>,
    #[serde(default)]
    pub border_bottom_width: Option<f32>,
    #[serde(default)]
    pub border_left_width: Option<f32>,
    #[serde(default)]
    pub border_top_color: Option<Color>,
    #[serde(default)]
    pub border_right_color: Option<Color>,
    #[serde(default)]
    pub border_bottom_color: Option<Color>,
    #[serde(default)]
    pub border_left_color: Option<Color>,
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
    #[serde(default)]
    pub justify_self: AlignSelf,
    #[serde(default)]
    pub justify_items: AlignItems,
    #[serde(default)]
    pub grid_template_columns: Option<String>,
    #[serde(default)]
    pub grid_column: Option<String>,

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
            float: Float::None,
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
            row_gap: None,
            column_gap: None,
            padding: Edges::ZERO,
            margin: Edges::ZERO,
            box_sizing: BoxSizing::ContentBox,
            width: Dimension::Auto,
            height: Dimension::Auto,
            min_width: Dimension::Auto,
            min_height: Dimension::Auto,
            max_width: Dimension::Auto,
            max_height: Dimension::Auto,
            overflow: Overflow::Visible,
            overflow_x: None,
            overflow_y: None,
            overscroll_behavior: OverscrollBehavior::Auto,
            scroll_initial_target: ScrollInitialTarget::None,
            overflow_anchor: true,
            background: Color::TRANSPARENT,
            background_image: None,
            background_size: None,
            background_position: None,
            background_repeat: None,
            background_origin: None,
            background_clip: None,
            background_attachment: None,
            background_blend_mode: None,
            color: Color::WHITE,
            font_size: 16.0,
            font_weight: 400,
            border_radius: 0.0,
            border_top_left_radius: None,
            border_top_right_radius: None,
            border_bottom_right_radius: None,
            border_bottom_left_radius: None,
            border_width: 0.0,
            border_color: Color::TRANSPARENT,
            border_top_width: None,
            border_right_width: None,
            border_bottom_width: None,
            border_left_width: None,
            border_top_color: None,
            border_right_color: None,
            border_bottom_color: None,
            border_left_color: None,
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
            justify_self: AlignSelf::Auto,
            justify_items: AlignItems::Stretch,
            grid_template_columns: None,
            grid_column: None,
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

impl Style {
    /// Equality that ignores `display`.
    ///
    /// Layout's Show-slot path used to `clone` both styles, zero `display`,
    /// then `PartialEq`. `Style` owns `Option<String>` layers and an optional
    /// `HashMap`, so that clone ran on every reactive rebuild even when only
    /// visibility toggled. Destructuring so a newly added field fails to
    /// compile here instead of silently dropping out of the comparison.
    pub fn eq_except_display(&self, other: &Self) -> bool {
        if std::ptr::eq(self, other) {
            return true;
        }
        if self.display == other.display {
            return self == other;
        }
        let Style {
            display: _,
            position,
            float,
            flex_direction,
            justify_content,
            align_items,
            flex_wrap,
            flex_grow,
            flex_shrink,
            top,
            right,
            bottom,
            left,
            z_index,
            gap,
            row_gap,
            column_gap,
            padding,
            margin,
            box_sizing,
            width,
            height,
            min_width,
            min_height,
            max_width,
            max_height,
            overflow,
            overflow_x,
            overflow_y,
            overscroll_behavior,
            scroll_initial_target,
            overflow_anchor,
            background,
            background_image,
            background_size,
            background_position,
            background_repeat,
            background_origin,
            background_clip,
            background_attachment,
            background_blend_mode,
            color,
            font_size,
            font_weight,
            border_radius,
            border_top_left_radius,
            border_top_right_radius,
            border_bottom_right_radius,
            border_bottom_left_radius,
            border_width,
            border_color,
            border_top_width,
            border_right_width,
            border_bottom_width,
            border_left_width,
            border_top_color,
            border_right_color,
            border_bottom_color,
            border_left_color,
            opacity,
            text_align,
            white_space,
            line_height,
            letter_spacing,
            text_decoration,
            text_overflow,
            font_family,
            font_style,
            word_break,
            custom_properties,
            contain,
            will_change,
            filter,
            box_shadow,
            transform,
            transition,
            animation,
            flex_basis,
            order,
            align_self,
            align_content,
            justify_self,
            justify_items,
            grid_template_columns,
            grid_column,
            cursor,
            pointer_events,
            user_select,
            visibility,
            outline_width,
            outline_color,
            outline_style,
        } = self;
        let Style {
            display: _,
            position: position_b,
            float: float_b,
            flex_direction: flex_direction_b,
            justify_content: justify_content_b,
            align_items: align_items_b,
            flex_wrap: flex_wrap_b,
            flex_grow: flex_grow_b,
            flex_shrink: flex_shrink_b,
            top: top_b,
            right: right_b,
            bottom: bottom_b,
            left: left_b,
            z_index: z_index_b,
            gap: gap_b,
            row_gap: row_gap_b,
            column_gap: column_gap_b,
            padding: padding_b,
            margin: margin_b,
            box_sizing: box_sizing_b,
            width: width_b,
            height: height_b,
            min_width: min_width_b,
            min_height: min_height_b,
            max_width: max_width_b,
            max_height: max_height_b,
            overflow: overflow_b,
            overflow_x: overflow_x_b,
            overflow_y: overflow_y_b,
            overscroll_behavior: overscroll_behavior_b,
            scroll_initial_target: scroll_initial_target_b,
            overflow_anchor: overflow_anchor_b,
            background: background_b,
            background_image: background_image_b,
            background_size: background_size_b,
            background_position: background_position_b,
            background_repeat: background_repeat_b,
            background_origin: background_origin_b,
            background_clip: background_clip_b,
            background_attachment: background_attachment_b,
            background_blend_mode: background_blend_mode_b,
            color: color_b,
            font_size: font_size_b,
            font_weight: font_weight_b,
            border_radius: border_radius_b,
            border_top_left_radius: border_top_left_radius_b,
            border_top_right_radius: border_top_right_radius_b,
            border_bottom_right_radius: border_bottom_right_radius_b,
            border_bottom_left_radius: border_bottom_left_radius_b,
            border_width: border_width_b,
            border_color: border_color_b,
            border_top_width: border_top_width_b,
            border_right_width: border_right_width_b,
            border_bottom_width: border_bottom_width_b,
            border_left_width: border_left_width_b,
            border_top_color: border_top_color_b,
            border_right_color: border_right_color_b,
            border_bottom_color: border_bottom_color_b,
            border_left_color: border_left_color_b,
            opacity: opacity_b,
            text_align: text_align_b,
            white_space: white_space_b,
            line_height: line_height_b,
            letter_spacing: letter_spacing_b,
            text_decoration: text_decoration_b,
            text_overflow: text_overflow_b,
            font_family: font_family_b,
            font_style: font_style_b,
            word_break: word_break_b,
            custom_properties: custom_properties_b,
            contain: contain_b,
            will_change: will_change_b,
            filter: filter_b,
            box_shadow: box_shadow_b,
            transform: transform_b,
            transition: transition_b,
            animation: animation_b,
            flex_basis: flex_basis_b,
            order: order_b,
            align_self: align_self_b,
            align_content: align_content_b,
            justify_self: justify_self_b,
            justify_items: justify_items_b,
            grid_template_columns: grid_template_columns_b,
            grid_column: grid_column_b,
            cursor: cursor_b,
            pointer_events: pointer_events_b,
            user_select: user_select_b,
            visibility: visibility_b,
            outline_width: outline_width_b,
            outline_color: outline_color_b,
            outline_style: outline_style_b,
        } = other;
        position == position_b
            && float == float_b
            && flex_direction == flex_direction_b
            && justify_content == justify_content_b
            && align_items == align_items_b
            && flex_wrap == flex_wrap_b
            && flex_grow == flex_grow_b
            && flex_shrink == flex_shrink_b
            && top == top_b
            && right == right_b
            && bottom == bottom_b
            && left == left_b
            && z_index == z_index_b
            && gap == gap_b
            && row_gap == row_gap_b
            && column_gap == column_gap_b
            && padding == padding_b
            && margin == margin_b
            && box_sizing == box_sizing_b
            && width == width_b
            && height == height_b
            && min_width == min_width_b
            && min_height == min_height_b
            && max_width == max_width_b
            && max_height == max_height_b
            && overflow == overflow_b
            && overflow_x == overflow_x_b
            && overflow_y == overflow_y_b
            && overscroll_behavior == overscroll_behavior_b
            && scroll_initial_target == scroll_initial_target_b
            && overflow_anchor == overflow_anchor_b
            && background == background_b
            && background_image == background_image_b
            && background_size == background_size_b
            && background_position == background_position_b
            && background_repeat == background_repeat_b
            && background_origin == background_origin_b
            && background_clip == background_clip_b
            && background_attachment == background_attachment_b
            && background_blend_mode == background_blend_mode_b
            && color == color_b
            && font_size == font_size_b
            && font_weight == font_weight_b
            && border_radius == border_radius_b
            && border_top_left_radius == border_top_left_radius_b
            && border_top_right_radius == border_top_right_radius_b
            && border_bottom_right_radius == border_bottom_right_radius_b
            && border_bottom_left_radius == border_bottom_left_radius_b
            && border_width == border_width_b
            && border_color == border_color_b
            && border_top_width == border_top_width_b
            && border_right_width == border_right_width_b
            && border_bottom_width == border_bottom_width_b
            && border_left_width == border_left_width_b
            && border_top_color == border_top_color_b
            && border_right_color == border_right_color_b
            && border_bottom_color == border_bottom_color_b
            && border_left_color == border_left_color_b
            && opacity == opacity_b
            && text_align == text_align_b
            && white_space == white_space_b
            && line_height == line_height_b
            && letter_spacing == letter_spacing_b
            && text_decoration == text_decoration_b
            && text_overflow == text_overflow_b
            && font_family == font_family_b
            && font_style == font_style_b
            && word_break == word_break_b
            && custom_properties == custom_properties_b
            && contain == contain_b
            && will_change == will_change_b
            && filter == filter_b
            && box_shadow == box_shadow_b
            && transform == transform_b
            && transition == transition_b
            && animation == animation_b
            && flex_basis == flex_basis_b
            && order == order_b
            && align_self == align_self_b
            && align_content == align_content_b
            && justify_self == justify_self_b
            && justify_items == justify_items_b
            && grid_template_columns == grid_template_columns_b
            && grid_column == grid_column_b
            && cursor == cursor_b
            && pointer_events == pointer_events_b
            && user_select == user_select_b
            && visibility == visibility_b
            && outline_width == outline_width_b
            && outline_color == outline_color_b
            && outline_style == outline_style_b
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
    InlineFlex,
    Table,
    InlineTable,
    TableRowGroup,
    TableHeaderGroup,
    TableFooterGroup,
    TableRow,
    TableColumnGroup,
    TableColumn,
    TableCell,
    TableCaption,
    ListItem,
    Contents,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Float {
    #[default]
    None,
    Left,
    Right,
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
pub enum BoxSizing {
    #[default]
    ContentBox,
    BorderBox,
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

/// CSS length for padding/margin.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Spacing {
    Px(f32),
    Percent(f32),
    Rem(f32),
    Em(f32),
    Vw(f32),
    Vh(f32),
    Auto,
    SafeAreaInset(SafeAreaEdge),
    /// `max(Npx, env(safe-area-inset-*))` — CSS safe-area fallback semantics.
    Maximum {
        px: f32,
        safe_area: SafeAreaEdge,
    },
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
            Spacing::Percent(_) | Spacing::Auto => 0.0,
            Spacing::Rem(v) => *v * 16.0,
            Spacing::Em(v) => *v * 16.0,
            Spacing::Vw(_) | Spacing::Vh(_) => 0.0,
            Spacing::SafeAreaInset(edge) => insets.value(*edge),
            Spacing::Maximum { px, safe_area } => px.max(insets.value(*safe_area)),
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
    /// CSS corner radii in top-left, top-right, bottom-right, bottom-left order.
    pub fn border_corner_radii(&self) -> [f32; 4] {
        [
            self.border_top_left_radius.unwrap_or(self.border_radius),
            self.border_top_right_radius.unwrap_or(self.border_radius),
            self.border_bottom_right_radius
                .unwrap_or(self.border_radius),
            self.border_bottom_left_radius.unwrap_or(self.border_radius),
        ]
    }

    pub fn padding_lengths(&self) -> EdgeLengths {
        self.padding.resolve_lengths(&crate::safe_area::current())
    }

    pub fn margin_lengths(&self) -> EdgeLengths {
        self.margin.resolve_lengths(&crate::safe_area::current())
    }

    pub fn resolved_overflow_x(&self) -> Overflow {
        self.overflow_x.unwrap_or(self.overflow)
    }

    pub fn resolved_overflow_y(&self) -> Overflow {
        self.overflow_y.unwrap_or(self.overflow)
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
    Steps(u32, StepPosition),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum StepPosition {
    JumpStart,
    #[default]
    JumpEnd,
    JumpNone,
    JumpBoth,
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
            Easing::Steps(steps, position) => {
                let steps = (*steps).max(1) as f32;
                match position {
                    StepPosition::JumpStart => ((t * steps).floor() + 1.0).min(steps) / steps,
                    StepPosition::JumpEnd => (t * steps).floor() / steps,
                    StepPosition::JumpNone => {
                        if steps <= 1.0 {
                            t
                        } else {
                            ((t * steps).floor()).min(steps - 1.0) / (steps - 1.0)
                        }
                    }
                    StepPosition::JumpBoth => {
                        ((t * steps).floor() + 1.0).min(steps + 1.0) / (steps + 1.0)
                    }
                }
            }
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

    #[test]
    fn css_steps_jump_both_has_the_extra_midpoint_step() {
        assert_eq!(
            Easing::Steps(1, super::StepPosition::JumpBoth).interpolate(0.25),
            0.5
        );
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

#[cfg(test)]
mod eq_except_display_tests {
    use super::*;

    #[test]
    fn ignores_display_and_does_not_require_equal_display() {
        let mut a = Style::default();
        let mut b = Style::default();
        a.display = Display::None;
        b.display = Display::Flex;
        assert!(a.eq_except_display(&b));
        b.font_size = 18.0;
        assert!(!a.eq_except_display(&b));
    }

    #[test]
    fn pointer_equal_styles_are_equal() {
        let a = Style::default();
        assert!(a.eq_except_display(&a));
    }

    #[test]
    fn corner_radii_participate_in_display_independent_equality() {
        let mut a = Style::default();
        let mut b = Style::default();
        a.display = Display::None;
        b.display = Display::Flex;
        b.border_top_left_radius = Some(4.0);
        assert!(!a.eq_except_display(&b));
    }
}
