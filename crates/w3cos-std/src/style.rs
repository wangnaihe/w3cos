use crate::color::Color;
use serde::{Deserialize, Serialize};

pub use crate::safe_area::{SafeAreaEdge, SafeAreaInsets};

/// Per-thread renderer adapter for the final font's zero-glyph advance.
pub type ChAdvanceProvider = fn(&Style) -> Option<f32>;
thread_local! {
    static CH_ADVANCE_PROVIDER: std::cell::Cell<Option<ChAdvanceProvider>> = const {
        std::cell::Cell::new(None)
    };
}

pub fn set_ch_advance_provider(provider: Option<ChAdvanceProvider>) {
    CH_ADVANCE_PROVIDER.with(|slot| slot.set(provider));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ColumnFill {
    Auto,
    #[default]
    Balance,
    BalanceAll,
}

/// Authored line style retained independently from the used border width.
/// In collapsed tables, hidden suppresses competitors while none does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BorderLineStyle {
    None,
    Hidden,
    Dotted,
    Dashed,
    Solid,
    Double,
    Groove,
    Ridge,
    Inset,
    Outset,
}

impl BorderLineStyle {
    pub fn is_visible(self) -> bool {
        !matches!(self, Self::None | Self::Hidden)
    }
}

/// Resolve CSS absolute lengths to canonical CSS pixels (96 px per inch).
/// Unitless values remain accepted for the runtime's existing native-style
/// compatibility; CSS declaration validation decides where only zero is legal.
pub fn parse_absolute_length_px(value: &str) -> Option<f32> {
    let value = value.trim();
    for (suffix, pixels_per_unit) in [
        ("px", 1.0_f64),
        ("cm", 96.0 / 2.54),
        ("mm", 96.0 / 25.4),
        ("q", 96.0 / 101.6),
        ("in", 96.0),
        ("pt", 96.0 / 72.0),
        ("pc", 16.0),
    ] {
        if let Some(number) = value.strip_suffix(suffix)
            && let Ok(number) = number.trim().parse::<f64>()
        {
            // CSS numeric literals and canonical-unit factors are doubles.
            // Round once at the used f32 boundary, not before conversion:
            // e.g. 10pt otherwise gains one ULP and changes host glyph masks.
            return Some((number * pixels_per_unit) as f32);
        }
    }
    value.parse().ok()
}

/// Parse a CSS `<integer>` into the runtime's supported range.
///
/// CSS integers are not restricted to the host integer width. Keeping an
/// out-of-range value valid and clamping it preserves its ordering relative
/// to every representable value instead of dropping the declaration.
pub fn parse_css_integer_clamped(value: &str) -> Option<i32> {
    let value = value.trim();
    if let Ok(parsed) = value.parse::<i128>() {
        return Some(parsed.clamp(i32::MIN as i128, i32::MAX as i128) as i32);
    }

    let (negative, digits) = if let Some(digits) = value.strip_prefix('-') {
        (true, digits)
    } else if let Some(digits) = value.strip_prefix('+') {
        (false, digits)
    } else {
        (false, value)
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(if negative { i32::MIN } else { i32::MAX })
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CssClipRect {
    pub top: Option<Dimension>,
    pub right: Option<Dimension>,
    pub bottom: Option<Dimension>,
    pub left: Option<Dimension>,
}

/// Custom (and internal `--w3cos-*`) properties, shared copy-on-write
/// between styles like Blink's DataRef<StyleRareInheritedData>: cloning a
/// computed style for inheritance no longer deep-copies the map; the first
/// mutation through `DerefMut` unshares it.
#[derive(Clone, Default)]
pub struct CustomProperties(std::sync::Arc<CustomPropertyMap>);

/// FxHash like Blink's StringHash-keyed HashMap: keys are short CSS names, so a
/// SipHash per lookup (layout probes `--w3cos-internal-*` markers per box) is
/// pure overhead.
pub type CustomPropertyMap = crate::fxhash::FxHashMap<String, String>;

impl CustomProperties {
    pub fn new() -> Self {
        Self::default()
    }

    /// Copy-on-write aware: a no-op removal never unshares the map (Blink's
    /// DataRef::Access() is likewise only taken when a field really changes).
    pub fn remove<Q>(&mut self, key: &Q) -> Option<String>
    where
        String: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        if !self.0.contains_key(key) {
            return None;
        }
        std::sync::Arc::make_mut(&mut self.0).remove(key)
    }

    /// Like `HashMap::retain`, but only unshares when something is dropped.
    pub fn retain(&mut self, mut keep: impl FnMut(&String, &String) -> bool) {
        if self.0.iter().all(|(key, value)| keep(key, value)) {
            return;
        }
        std::sync::Arc::make_mut(&mut self.0).retain(|key, value| keep(key, value));
    }

    /// Like `HashMap::insert`, but storing an identical value keeps sharing.
    pub fn insert(&mut self, key: String, value: String) -> Option<String> {
        if self.0.get(&key) == Some(&value) {
            return Some(value);
        }
        std::sync::Arc::make_mut(&mut self.0).insert(key, value)
    }
}

impl std::ops::Deref for CustomProperties {
    type Target = CustomPropertyMap;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for CustomProperties {
    fn deref_mut(&mut self) -> &mut Self::Target {
        std::sync::Arc::make_mut(&mut self.0)
    }
}

impl PartialEq for CustomProperties {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0) || *self.0 == *other.0
    }
}

impl Eq for CustomProperties {}

impl std::fmt::Debug for CustomProperties {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl From<CustomPropertyMap> for CustomProperties {
    fn from(map: CustomPropertyMap) -> Self {
        Self(std::sync::Arc::new(map))
    }
}

impl From<std::collections::HashMap<String, String>> for CustomProperties {
    fn from(map: std::collections::HashMap<String, String>) -> Self {
        Self(std::sync::Arc::new(map.into_iter().collect()))
    }
}

impl From<CustomProperties> for std::collections::HashMap<String, String> {
    fn from(props: CustomProperties) -> Self {
        std::sync::Arc::try_unwrap(props.0)
            .unwrap_or_else(|shared| (*shared).clone())
            .into_iter()
            .collect()
    }
}

impl FromIterator<(String, String)> for CustomProperties {
    fn from_iter<I: IntoIterator<Item = (String, String)>>(iter: I) -> Self {
        Self::from(iter.into_iter().collect::<CustomPropertyMap>())
    }
}

impl<'a> IntoIterator for &'a CustomProperties {
    type Item = (&'a String, &'a String);
    type IntoIter = std::collections::hash_map::Iter<'a, String, String>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl Serialize for CustomProperties {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        (*self.0).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CustomProperties {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        CustomPropertyMap::deserialize(deserializer).map(Self::from)
    }
}

/// LayoutNG-only grid cascade. Kept off `Style` until a grid property is
/// set, so recursive tree walks do not copy unused strings on the stack.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GridNg {
    /// `grid-template-rows`. Legacy track sizing does not read this.
    #[serde(default)]
    pub grid_template_rows: Option<String>,
    /// `grid-row`. Legacy placement does not read this.
    #[serde(default)]
    pub grid_row: Option<String>,
    /// Raw `grid-template` shorthand, after the cascade.
    #[serde(default)]
    pub grid_template: Option<String>,
    /// Raw `grid` shorthand, after the cascade.
    #[serde(default)]
    pub grid_shorthand: Option<String>,
    #[serde(default)]
    pub grid_auto_flow: Option<String>,
    #[serde(default)]
    pub grid_auto_rows: Option<String>,
    #[serde(default)]
    pub grid_auto_columns: Option<String>,
    #[serde(default)]
    pub grid_template_areas: Option<String>,
    /// Raw `grid-area`. Not copied into `grid_column`, which Taffy reads.
    #[serde(default)]
    pub grid_area: Option<String>,
    /// Cascaded column template for LayoutNG. `None` with
    /// `grid_columns_cascaded` is an explicit `none` or a shorthand that
    /// reset the columns. Legacy keeps reading `grid_template_columns`.
    #[serde(default)]
    pub grid_columns_used: Option<String>,
    #[serde(default)]
    pub grid_columns_cascaded: bool,
    /// Cascaded `grid-column` / `grid-row` for LayoutNG, including
    /// `grid-area`. Absent until one of those properties is set.
    #[serde(default)]
    pub grid_column_used: Option<String>,
    #[serde(default)]
    pub grid_row_used: Option<String>,
    #[serde(default)]
    pub grid_placement_cascaded: bool,
}

/// CSS Modern Subset — Flexbox, Grid, Block, Inline, and positioning.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Style {
    // Layout mode
    pub display: Display,
    pub position: Position,
    #[serde(default)]
    pub float: Float,
    #[serde(default)]
    pub clear: Clear,

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
    #[serde(default)]
    pub column_width: Dimension,
    /// None is the CSS initial `auto`, not zero columns.
    #[serde(default)]
    pub column_count: Option<u32>,
    #[serde(default)]
    pub column_fill: ColumnFill,
    #[serde(default)]
    pub border_spacing_x: f32,
    #[serde(default)]
    pub border_spacing_y: f32,
    #[serde(default)]
    pub border_collapse: bool,
    #[serde(default)]
    pub table_layout_fixed: bool,
    #[serde(default)]
    pub empty_cells_hide: bool,
    #[serde(default)]
    pub caption_side_bottom: bool,
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
    /// CSS2 `clip: rect(...)` for absolutely positioned boxes. `None` means
    /// `auto`; individual `None` sides represent the `auto` side keyword.
    #[serde(default)]
    pub clip: Option<CssClipRect>,
    #[serde(default)]
    pub overflow_x: Option<Overflow>,
    #[serde(default)]
    pub overflow_y: Option<Overflow>,
    /// CSS Overflow 3 `overflow: clip` per axis. Clip is not a scroll
    /// container and lays out like `visible`, so the axis keeps its
    /// `Overflow::Visible` value for layout; these flags only add a paint clip.
    #[serde(default)]
    pub overflow_clip_x: bool,
    #[serde(default)]
    pub overflow_clip_y: bool,
    /// `overflow-clip-margin`: the reference box (0 padding-box, 1
    /// content-box, 2 border-box) and the distance the `overflow: clip` edge
    /// is moved outward from it.
    #[serde(default)]
    pub overflow_clip_margin: (u8, f32),
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
    /// Unquantized CSS foreground RGBA, in normalized sRGB channels. The
    /// integer color remains the computed/legacy value; paint retains precision.
    #[serde(default)]
    pub color_rgba: Option<[f32; 4]>,
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
    /// Computed keyword provenance, top/right/bottom/left. CSS currentColor
    /// is inherited as a keyword, not as the parent's resolved RGBA value.
    /// None leaves native numeric styles without CSS provenance unchanged.
    #[serde(default)]
    pub border_current_color: Option<[bool; 4]>,
    /// Physical edges in top/right/bottom/left order. None preserves older
    /// native numeric styles without inventing an authored CSS declaration.
    #[serde(default)]
    pub border_styles: [Option<BorderLineStyle>; 4],
    /// Authored/computed widths before none or hidden reduces the used width.
    /// None means the CSS initial medium width when explicitly inherited.
    #[serde(default)]
    pub computed_border_widths: [Option<f32>; 4],
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
    /// `white-space: break-spaces`: line breaking allows a break after every
    /// space (Blink BreakSpaceType::kAfterEverySpace).
    #[serde(default)]
    pub break_spaces: bool,
    pub line_height: f32,
    /// Retained CSS keyword; old wire styles and native numeric builders default to false.
    #[serde(default)]
    pub line_height_is_normal: bool,
    /// Computed CSS length for an authored length or percentage. Unitless
    /// line-height stays `None` so descendants inherit its multiplier.
    #[serde(default)]
    pub line_height_computed_px: Option<f32>,
    #[serde(default = "default_text_indent")]
    pub text_indent: Dimension,
    #[serde(default)]
    pub text_transform: TextTransform,
    pub letter_spacing: f32,
    #[serde(default)]
    pub word_spacing: f32,
    pub text_decoration: TextDecoration,
    pub text_overflow: TextOverflow,
    pub font_family: Option<String>,
    pub font_style: FontStyle,
    /// CSS2 `font-variant`; glyph selection does not alter the CSS font size.
    #[serde(default)]
    pub font_variant: FontVariant,
    /// Whether OpenType `kern` is enabled for this inherited text style.
    #[serde(default = "default_font_kerning")]
    pub font_kerning: bool,
    /// Low-level `font-feature-settings` override for OpenType `kern`.
    #[serde(default)]
    pub font_feature_kern: Option<bool>,
    pub word_break: WordBreak,
    /// CSS `line-break` (inherited).
    #[serde(default)]
    pub line_break: LineBreak,
    /// `hanging-punctuation: last` (inherited).
    #[serde(default)]
    pub hanging_punctuation_last: bool,
    #[serde(default)]
    pub direction: TextDirection,
    /// CSS `writing-mode` (inherited). Blink keeps it in ComputedStyle and
    /// derives the logical axes from it; the cascade maps logical sizes to
    /// the physical axes only for `horizontal-tb` (CSS Writing Modes 4).
    #[serde(default)]
    pub writing_mode: WritingMode,
    /// Physical values the element's logical properties (`inline-size`,
    /// `block-size`, `margin-block-start`, ...) resolve to for the layout
    /// engine that models them. The fields above keep the declarations
    /// exactly as the engine that ignores them reads them; see
    /// [`LogicalOverrides`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logical_overrides: Option<Box<LogicalOverrides>>,
    #[serde(default)]
    pub unicode_bidi: UnicodeBidi,

    // CSS Custom Properties (#34)
    pub custom_properties: Option<CustomProperties>,

    // CSS Containment — layout isolation boundaries (Chrome-inspired)
    pub contain: Contain,

    /// CSS `will-change` — UA compositor layer promotion hint.
    pub will_change: WillChange,

    /// CSS `filter` — stored raw; non-none values promote compositor layers.
    pub filter: Option<String>,

    // Box Shadow. `box_shadow` is the first layer so existing callers stay
    // valid. Further comma-separated layers live in `box_shadows`.
    pub box_shadow: Option<BoxShadow>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub box_shadows: Vec<BoxShadow>,
    /// CSS `backdrop-filter: blur()` standard deviation, in CSS pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backdrop_blur: Option<f32>,
    /// CSS `text-shadow` layers, first painted on top.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub text_shadows: Vec<TextShadow>,

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
    /// LayoutNG grid cascade. One heap box so the eleven strings are not
    /// copied onto the stack of every recursive style walk. Legacy track
    /// sizing still reads only `grid_template_columns` and `grid_column`.
    #[serde(default)]
    pub grid_ng: Option<Box<GridNg>>,

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
            clear: Clear::None,
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
            column_width: Dimension::Auto,
            column_count: None,
            column_fill: ColumnFill::Balance,
            border_spacing_x: 0.0,
            border_spacing_y: 0.0,
            border_collapse: false,
            table_layout_fixed: false,
            empty_cells_hide: false,
            caption_side_bottom: false,
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
            clip: None,
            overflow_x: None,
            overflow_y: None,
            overflow_clip_x: false,
            overflow_clip_y: false,
            overflow_clip_margin: (0, 0.0),
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
            color_rgba: None,
            font_size: 16.0,
            font_weight: 400,
            border_radius: 0.0,
            border_top_left_radius: None,
            border_top_right_radius: None,
            border_bottom_right_radius: None,
            border_bottom_left_radius: None,
            border_width: 0.0,
            border_color: Color::TRANSPARENT,
            border_current_color: None,
            border_styles: [None; 4],
            computed_border_widths: [None; 4],
            border_top_width: None,
            border_right_width: None,
            border_bottom_width: None,
            border_left_width: None,
            border_top_color: None,
            border_right_color: None,
            border_bottom_color: None,
            border_left_color: None,
            opacity: 1.0,
            text_align: TextAlign::Start,
            white_space: WhiteSpace::Normal,
            break_spaces: false,
            line_height: 1.2,
            line_height_is_normal: false,
            line_height_computed_px: None,
            text_indent: Dimension::Px(0.0),
            text_transform: TextTransform::None,
            letter_spacing: 0.0,
            word_spacing: 0.0,
            text_decoration: TextDecoration::None,
            text_overflow: TextOverflow::Clip,
            font_family: None,
            font_style: FontStyle::Normal,
            font_variant: FontVariant::Normal,
            font_kerning: true,
            font_feature_kern: None,
            word_break: WordBreak::Normal,
            line_break: LineBreak::Auto,
            hanging_punctuation_last: false,
            direction: TextDirection::Ltr,
            writing_mode: WritingMode::HorizontalTb,
            logical_overrides: None,
            unicode_bidi: UnicodeBidi::Normal,
            custom_properties: None,
            contain: Contain::None,
            will_change: WillChange::default(),
            filter: None,
            box_shadow: None,
            box_shadows: Vec::new(),
            backdrop_blur: None,
            text_shadows: Vec::new(),
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
            grid_ng: None,
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
    pub fn has_box_shadow(&self) -> bool {
        self.box_shadow.is_some() || !self.box_shadows.is_empty()
    }

    /// First declared shadow, then the rest. Painters that need CSS order
    /// (first on top) reverse this after splitting inset from outer.
    pub fn box_shadow_layers(&self) -> impl Iterator<Item = &BoxShadow> {
        self.box_shadow.iter().chain(self.box_shadows.iter())
    }

    /// Resolve only an owned layout/paint snapshot. The source computed style
    /// must retain its lengths so explicit inheritance can copy them.
    pub fn resolve_used_border_widths(&mut self) {
        for (line_style, width) in self.border_styles.iter().zip([
            &mut self.border_top_width,
            &mut self.border_right_width,
            &mut self.border_bottom_width,
            &mut self.border_left_width,
        ]) {
            if line_style.is_some_and(|style| !style.is_visible()) {
                *width = Some(0.0);
            }
        }
        if self
            .border_styles
            .iter()
            .all(|style| style.is_some_and(|style| !style.is_visible()))
        {
            self.border_width = 0.0;
        }
    }

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
            clear,
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
            column_width,
            column_count,
            column_fill,
            border_spacing_x,
            border_spacing_y,
            border_collapse,
            table_layout_fixed,
            empty_cells_hide,
            caption_side_bottom,
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
            clip,
            overflow_x,
            overflow_y,
            overflow_clip_x,
            overflow_clip_y,
            overflow_clip_margin,
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
            color_rgba,
            font_size,
            font_weight,
            border_radius,
            border_top_left_radius,
            border_top_right_radius,
            border_bottom_right_radius,
            border_bottom_left_radius,
            border_width,
            border_color,
            border_current_color,
            border_styles,
            computed_border_widths,
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
            line_height_is_normal,
            line_height_computed_px,
            text_indent,
            text_transform,
            letter_spacing,
            word_spacing,
            text_decoration,
            text_overflow,
            font_family,
            font_style,
            font_variant,
            font_kerning,
            font_feature_kern,
            word_break,
            line_break,
            hanging_punctuation_last,
            break_spaces,
            direction,
            unicode_bidi,
            writing_mode,
            logical_overrides,
            custom_properties,
            contain,
            will_change,
            filter,
            box_shadow,
            box_shadows,
            backdrop_blur,
            text_shadows,
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
            grid_ng,
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
            clear: clear_b,
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
            column_width: column_width_b,
            column_count: column_count_b,
            column_fill: column_fill_b,
            border_spacing_x: border_spacing_x_b,
            border_spacing_y: border_spacing_y_b,
            border_collapse: border_collapse_b,
            table_layout_fixed: table_layout_fixed_b,
            empty_cells_hide: empty_cells_hide_b,
            caption_side_bottom: caption_side_bottom_b,
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
            clip: clip_b,
            overflow_x: overflow_x_b,
            overflow_y: overflow_y_b,
            overflow_clip_x: overflow_clip_x_b,
            overflow_clip_y: overflow_clip_y_b,
            overflow_clip_margin: overflow_clip_margin_b,
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
            color_rgba: color_rgba_b,
            font_size: font_size_b,
            font_weight: font_weight_b,
            border_radius: border_radius_b,
            border_top_left_radius: border_top_left_radius_b,
            border_top_right_radius: border_top_right_radius_b,
            border_bottom_right_radius: border_bottom_right_radius_b,
            border_bottom_left_radius: border_bottom_left_radius_b,
            border_width: border_width_b,
            border_color: border_color_b,
            border_current_color: border_current_color_b,
            border_styles: border_styles_b,
            computed_border_widths: computed_border_widths_b,
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
            line_height_is_normal: line_height_is_normal_b,
            line_height_computed_px: line_height_computed_px_b,
            text_indent: text_indent_b,
            text_transform: text_transform_b,
            letter_spacing: letter_spacing_b,
            word_spacing: word_spacing_b,
            text_decoration: text_decoration_b,
            text_overflow: text_overflow_b,
            font_family: font_family_b,
            font_style: font_style_b,
            font_variant: font_variant_b,
            font_kerning: font_kerning_b,
            font_feature_kern: font_feature_kern_b,
            word_break: word_break_b,
            line_break: line_break_b,
            hanging_punctuation_last: hanging_punctuation_last_b,
            break_spaces: break_spaces_b,
            direction: direction_b,
            unicode_bidi: unicode_bidi_b,
            writing_mode: writing_mode_b,
            logical_overrides: logical_overrides_b,
            custom_properties: custom_properties_b,
            contain: contain_b,
            will_change: will_change_b,
            filter: filter_b,
            box_shadow: box_shadow_b,
            box_shadows: box_shadows_b,
            backdrop_blur: backdrop_blur_b,
            text_shadows: text_shadows_b,
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
            grid_ng: grid_ng_b,
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
            && clear == clear_b
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
            && column_width == column_width_b
            && column_count == column_count_b
            && column_fill == column_fill_b
            && border_spacing_x == border_spacing_x_b
            && border_spacing_y == border_spacing_y_b
            && border_collapse == border_collapse_b
            && table_layout_fixed == table_layout_fixed_b
            && empty_cells_hide == empty_cells_hide_b
            && caption_side_bottom == caption_side_bottom_b
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
            && clip == clip_b
            && overflow_x == overflow_x_b
            && overflow_y == overflow_y_b
            && overflow_clip_x == overflow_clip_x_b
            && overflow_clip_y == overflow_clip_y_b
            && overflow_clip_margin == overflow_clip_margin_b
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
            && color_rgba == color_rgba_b
            && font_size == font_size_b
            && font_weight == font_weight_b
            && border_radius == border_radius_b
            && border_top_left_radius == border_top_left_radius_b
            && border_top_right_radius == border_top_right_radius_b
            && border_bottom_right_radius == border_bottom_right_radius_b
            && border_bottom_left_radius == border_bottom_left_radius_b
            && border_width == border_width_b
            && border_color == border_color_b
            && border_current_color == border_current_color_b
            && border_styles == border_styles_b
            && computed_border_widths == computed_border_widths_b
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
            && line_height_is_normal == line_height_is_normal_b
            && line_height_computed_px == line_height_computed_px_b
            && text_indent == text_indent_b
            && text_transform == text_transform_b
            && letter_spacing == letter_spacing_b
            && word_spacing == word_spacing_b
            && text_decoration == text_decoration_b
            && text_overflow == text_overflow_b
            && font_family == font_family_b
            && font_style == font_style_b
            && font_variant == font_variant_b
            && font_kerning == font_kerning_b
            && font_feature_kern == font_feature_kern_b
            && word_break == word_break_b
            && line_break == line_break_b
            && hanging_punctuation_last == hanging_punctuation_last_b
            && break_spaces == break_spaces_b
            && direction == direction_b
            && unicode_bidi == unicode_bidi_b
            && writing_mode == writing_mode_b
            && logical_overrides == logical_overrides_b
            && custom_properties == custom_properties_b
            && contain == contain_b
            && will_change == will_change_b
            && filter == filter_b
            && box_shadow == box_shadow_b
            && box_shadows == box_shadows_b
            && backdrop_blur == backdrop_blur_b
            && text_shadows == text_shadows_b
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
            && grid_ng == grid_ng_b
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

const fn default_font_kerning() -> bool {
    true
}

const fn default_text_indent() -> Dimension {
    Dimension::Px(0.0)
}

impl Style {
    /// The NG grid cascade, created on the first write.
    pub fn grid_ng_mut(&mut self) -> &mut GridNg {
        self.grid_ng
            .get_or_insert_with(|| Box::new(GridNg::default()))
    }

    fn ng(&self) -> Option<&GridNg> {
        self.grid_ng.as_deref()
    }

    pub fn grid_rows(&self) -> Option<&str> {
        self.ng()
            .and_then(|grid| grid.grid_template_rows.as_deref())
    }

    pub fn grid_flow(&self) -> Option<&str> {
        self.ng().and_then(|grid| grid.grid_auto_flow.as_deref())
    }

    pub fn grid_auto_row_spec(&self) -> Option<&str> {
        self.ng().and_then(|grid| grid.grid_auto_rows.as_deref())
    }

    pub fn grid_auto_column_spec(&self) -> Option<&str> {
        self.ng().and_then(|grid| grid.grid_auto_columns.as_deref())
    }

    pub fn grid_areas(&self) -> Option<&str> {
        self.ng()
            .and_then(|grid| grid.grid_template_areas.as_deref())
    }

    pub fn grid_columns_cascaded(&self) -> bool {
        self.ng().is_some_and(|grid| grid.grid_columns_cascaded)
    }

    pub fn grid_columns_used(&self) -> Option<&str> {
        self.ng().and_then(|grid| grid.grid_columns_used.as_deref())
    }

    pub fn grid_placement_cascaded(&self) -> bool {
        self.ng().is_some_and(|grid| grid.grid_placement_cascaded)
    }

    pub fn grid_row_used(&self) -> Option<&str> {
        self.ng().and_then(|grid| grid.grid_row_used.as_deref())
    }

    pub fn grid_column_used(&self) -> Option<&str> {
        self.ng().and_then(|grid| grid.grid_column_used.as_deref())
    }

    pub fn grid_row_spec(&self) -> Option<&str> {
        self.ng().and_then(|grid| grid.grid_row.as_deref())
    }

    /// Resolve `text-indent` against the containing block and viewport.
    pub fn resolved_text_indent(
        &self,
        containing_width: f32,
        viewport_width: f32,
        viewport_height: f32,
    ) -> f32 {
        match self.text_indent {
            Dimension::Px(value) => value,
            Dimension::Percent(value) => containing_width * value / 100.0,
            Dimension::Rem(value) => value * 16.0,
            Dimension::Em(value) => value * self.font_size,
            Dimension::Ch(value) => value * self.ch_advance(),
            Dimension::Vw(value) => viewport_width * value / 100.0,
            Dimension::Vh(value) => viewport_height * value / 100.0,
            Dimension::Auto | Dimension::Calc { .. } => 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Display {
    Block,
    FlowRoot,
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

impl Display {
    /// CSS 2.1 overflow applies to block containers, not inline boxes or
    /// non-cell internal table boxes.
    pub fn establishes_overflow_clip(self) -> bool {
        !matches!(
            self,
            Self::Inline
                | Self::TableRow
                | Self::TableRowGroup
                | Self::TableHeaderGroup
                | Self::TableFooterGroup
                | Self::TableColumn
                | Self::TableColumnGroup
        )
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Clear {
    #[default]
    None,
    Left,
    Right,
    Both,
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
    Ch(f32),
    Vw(f32),
    Vh(f32),
    /// `calc()` of a percentage and a px length: `percent`% of the basis
    /// plus `px`. Only LayoutNG resolves it; legacy layout treats it as
    /// `auto`, as it did when the declaration was dropped.
    Calc {
        percent: f32,
        px: f32,
    },
}

/// CSS length for padding/margin.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Spacing {
    Px(f32),
    Percent(f32),
    Rem(f32),
    Em(f32),
    Ch(f32),
    Vw(f32),
    Vh(f32),
    Auto,
    SafeAreaInset(SafeAreaEdge),
    /// `max(Npx, env(safe-area-inset-*))` — CSS safe-area fallback semantics.
    Maximum {
        px: f32,
        safe_area: SafeAreaEdge,
    },
    /// `calc(P% + Xpx)`, a margin that mixes a percentage and a length.
    /// Only LayoutNG resolves it; legacy layout reads it as zero.
    Calc {
        percent: f32,
        px: f32,
    },
    /// `calc(P% + Xpx)` on a padding side: the same sum, floored at zero
    /// (padding cannot be negative). Only LayoutNG resolves it; legacy
    /// layout reads it as zero, as it did when the declaration was dropped.
    PaddingCalc {
        percent: f32,
        px: f32,
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
            Spacing::Percent(_)
            | Spacing::Calc { .. }
            | Spacing::PaddingCalc { .. }
            | Spacing::Auto => 0.0,
            Spacing::Rem(v) => *v * 16.0,
            Spacing::Em(v) => *v * 16.0,
            Spacing::Ch(v) => *v * 8.0,
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

/// Quantize a resolved margin/padding length at the used box-value boundary.
/// Computed CSS lengths and glyph/letter-spacing advances stay unmodified.
pub fn used_box_spacing(value: f32) -> f32 {
    if value.is_finite() {
        (value * 64.0).trunc() / 64.0
    } else {
        value
    }
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

    fn resolve_lengths_with_style(&self, insets: &SafeAreaInsets, style: &Style) -> EdgeLengths {
        let resolve = |value: Spacing| {
            used_box_spacing(match value {
                Spacing::Em(value) => value * style.font_size,
                Spacing::Ch(value) => value * style.ch_advance(),
                // A calc() with a zero percentage does not depend on the
                // percentage basis (inline-intrinsic-size-calc).
                Spacing::Calc { percent, px } if percent == 0.0 => px,
                Spacing::PaddingCalc { percent, px } if percent == 0.0 => px.max(0.0),
                other => other.resolve(insets),
            })
        };
        EdgeLengths {
            top: resolve(self.top),
            right: resolve(self.right),
            bottom: resolve(self.bottom),
            left: resolve(self.left),
        }
    }
}

impl Style {
    /// Resolve against the current font, never cached pixels from an ancestor.
    pub fn ch_advance(&self) -> f32 {
        CH_ADVANCE_PROVIDER
            .with(|slot| slot.get())
            .and_then(|provider| provider(self))
            .filter(|advance| advance.is_finite() && *advance >= 0.0)
            .unwrap_or(self.font_size * 0.5)
    }

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
        self.padding
            .resolve_lengths_with_style(&crate::safe_area::current(), self)
    }

    pub fn margin_lengths(&self) -> EdgeLengths {
        self.margin
            .resolve_lengths_with_style(&crate::safe_area::current(), self)
    }

    pub fn resolved_overflow_x(&self) -> Overflow {
        self.table_overflow(self.overflow_x.unwrap_or(self.overflow))
    }

    pub fn resolved_overflow_y(&self) -> Overflow {
        self.table_overflow(self.overflow_y.unwrap_or(self.overflow))
    }

    fn table_overflow(&self, overflow: Overflow) -> Overflow {
        if matches!(self.display, Display::Table | Display::InlineTable)
            && matches!(overflow, Overflow::Auto | Overflow::Scroll)
        {
            Overflow::Visible
        } else {
            overflow
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Display, Edges, Overflow, Spacing, Style};

    #[test]
    fn absolute_length_conversion_rounds_only_the_final_pixel_value() {
        for (unit, scale) in [
            ("pt", 96.0_f64 / 72.0),
            ("cm", 96.0 / 2.54),
            ("mm", 96.0 / 25.4),
            ("q", 96.0 / 101.6),
            ("in", 96.0),
            ("pc", 16.0),
            ("px", 1.0),
        ] {
            for number in ["10", "1.1", "-1.2345678901", "0.000017", "123.456789"] {
                let expected = (number.parse::<f64>().unwrap() * scale) as f32;
                let actual = super::parse_absolute_length_px(&format!("{number}{unit}")).unwrap();
                assert_eq!(actual.to_bits(), expected.to_bits(), "{number}{unit}");
            }
        }
        assert_eq!(super::parse_absolute_length_px(" 12 "), Some(12.0));
        assert_eq!(super::parse_absolute_length_px("invalid"), None);
    }

    #[test]
    fn table_auto_and_scroll_overflow_are_visible() {
        for display in [Display::Table, Display::InlineTable] {
            for overflow in [Overflow::Auto, Overflow::Scroll] {
                let style = Style {
                    display,
                    overflow,
                    ..Style::default()
                };
                assert_eq!(style.resolved_overflow_x(), Overflow::Visible);
                assert_eq!(style.resolved_overflow_y(), Overflow::Visible);
            }
        }
        let block = Style {
            display: Display::Block,
            overflow: Overflow::Auto,
            ..Style::default()
        };
        assert_eq!(block.resolved_overflow_x(), Overflow::Auto);
    }

    #[test]
    fn normal_line_height_is_not_compatible_with_the_same_numeric_ratio() {
        let numeric = Style::default();
        let normal = Style {
            line_height_is_normal: true,
            ..numeric.clone()
        };
        assert!(!numeric.eq_except_display(&normal));
        assert_ne!(numeric, normal);
    }

    #[test]
    fn style_edge_lengths_resolve_em_against_local_font_size() {
        let style = Style {
            font_size: 28.0,
            padding: Edges {
                top: Spacing::Em(1.0),
                right: Spacing::Em(0.5),
                bottom: Spacing::Em(2.0),
                left: Spacing::Px(3.0),
            },
            margin: Edges {
                top: Spacing::Em(1.0),
                right: Spacing::Px(0.0),
                bottom: Spacing::Px(0.0),
                left: Spacing::Em(0.5),
            },
            ..Style::default()
        };

        let padding = style.padding_lengths();
        assert_eq!(padding.top, 28.0);
        assert_eq!(padding.right, 14.0);
        assert_eq!(padding.bottom, 56.0);
        assert_eq!(padding.left, 3.0);

        let margin = style.margin_lengths();
        assert_eq!(margin.top, 28.0);
        assert_eq!(margin.left, 14.0);
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
    Start,
    End,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TextTransform {
    #[default]
    None,
    Capitalize,
    Uppercase,
    Lowercase,
}

pub fn transformed_text<'a>(
    text: &'a str,
    transform: TextTransform,
    language: Option<&str>,
    continues_word: bool,
) -> std::borrow::Cow<'a, str> {
    let turkic = language.is_some_and(|language| {
        language == "tr"
            || language.starts_with("tr-")
            || language == "az"
            || language.starts_with("az-")
    });
    match transform {
        TextTransform::None => std::borrow::Cow::Borrowed(text),
        TextTransform::Uppercase => {
            let mut output = String::with_capacity(text.len());
            for character in text.chars() {
                if turkic && character == 'i' {
                    output.push('\u{0130}');
                } else if turkic && character == '\u{0131}' {
                    output.push('I');
                } else {
                    push_css_uppercase(&mut output, character);
                }
            }
            std::borrow::Cow::Owned(output)
        }
        TextTransform::Lowercase => {
            let mut output = String::with_capacity(text.len());
            for character in text.chars() {
                if turkic && character == 'I' {
                    output.push('\u{0131}');
                } else if turkic && character == '\u{0130}' {
                    output.push('i');
                } else {
                    output.extend(character.to_lowercase());
                }
            }
            std::borrow::Cow::Owned(output)
        }
        TextTransform::Capitalize => {
            let mut output = String::with_capacity(text.len());
            let mut at_word_start = !continues_word;
            for character in text.chars() {
                if character.is_alphanumeric() {
                    if at_word_start {
                        push_css_titlecase(&mut output, character);
                    } else {
                        output.push(character);
                    }
                    at_word_start = false;
                } else {
                    output.push(character);
                    at_word_start = true;
                }
            }
            std::borrow::Cow::Owned(output)
        }
    }
}

fn push_css_uppercase(output: &mut String, character: char) {
    let codepoint = character as u32;
    if (0x10d0..=0x10ff).contains(&codepoint) {
        // CSS 2.1's bicameral mapping baseline treats Georgian Mkhedruli as
        // unicase; do not apply the newer Unicode Mtavruli case mapping.
        output.push(character);
        return;
    }
    output.extend(character.to_uppercase());
}

fn push_css_titlecase(output: &mut String, character: char) {
    // Greek titlecase keeps ypogegrammeni, while full uppercase expands it
    // to a separate capital iota. Do not use an old reftest's simple casing
    // baseline as the uppercase rendering rule.
    let codepoint = character as u32;
    let greek_titlecase = match codepoint {
        0x1f80..=0x1f87 | 0x1f90..=0x1f97 | 0x1fa0..=0x1fa7 => char::from_u32(codepoint + 8),
        0x1fb3 => Some('\u{1fbc}'),
        0x1fc3 => Some('\u{1fcc}'),
        0x1ff3 => Some('\u{1ffc}'),
        _ => None,
    };
    if let Some(character) = greek_titlecase {
        output.push(character);
    } else {
        push_css_uppercase(output, character);
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum FontVariant {
    #[default]
    Normal,
    SmallCaps,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum WordBreak {
    #[default]
    Normal,
    BreakAll,
    BreakWord,
    KeepAll,
}

/// CSS Text 3 `line-break`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LineBreak {
    #[default]
    Auto,
    Loose,
    Normal,
    Strict,
    Anywhere,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TextDirection {
    #[default]
    Ltr,
    Rtl,
}

/// CSS `writing-mode` (CSS Writing Modes 4). The sideways values are the
/// vertical modes whose lines run top to bottom with rotated glyphs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum WritingMode {
    #[default]
    HorizontalTb,
    VerticalRl,
    VerticalLr,
    SidewaysRl,
    SidewaysLr,
}

impl WritingMode {
    /// Block flow is top to bottom and lines are horizontal: the inline size
    /// is the physical width.
    pub fn is_horizontal(self) -> bool {
        self == WritingMode::HorizontalTb
    }

    /// Parse a computed `writing-mode` keyword, including the SVG 1.1 names
    /// Blink still accepts (`lr`, `lr-tb`, `rl`, `rl-tb` are horizontal-tb;
    /// `tb` and `tb-rl` are vertical-rl). `None` for anything else.
    pub fn from_keyword(value: &str) -> Option<WritingMode> {
        Some(match value.trim().to_ascii_lowercase().as_str() {
            "horizontal-tb" | "lr" | "lr-tb" | "rl" | "rl-tb" => WritingMode::HorizontalTb,
            "vertical-rl" | "tb" | "tb-rl" => WritingMode::VerticalRl,
            "vertical-lr" => WritingMode::VerticalLr,
            "sideways-rl" => WritingMode::SidewaysRl,
            "sideways-lr" => WritingMode::SidewaysLr,
            _ => return None,
        })
    }
}

/// The physical values an element's logical properties resolve to, kept
/// beside the declared style. The declared fields stay what the engine that
/// ignores logical properties reads, so a reftest whose test and reference
/// both relied on that keeps rendering the same; the engine that models them
/// applies these on top (`apply`) and falls back, with both sides of a
/// reftest, for the features it does not lay out.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LogicalOverrides {
    pub width: Option<Dimension>,
    pub height: Option<Dimension>,
    pub min_width: Option<Dimension>,
    pub min_height: Option<Dimension>,
    pub max_width: Option<Dimension>,
    pub max_height: Option<Dimension>,
    /// Top, right, bottom, left.
    pub margin: [Option<Spacing>; 4],
    pub padding: [Option<Spacing>; 4],
    /// Internal marker properties that differ: `None` removes the marker.
    pub markers: Vec<(String, Option<String>)>,
}

impl LogicalOverrides {
    /// The style as the engine that models logical properties reads it.
    pub fn apply(&self, style: &mut Style) {
        style.logical_overrides = None;
        if let Some(v) = self.width {
            style.width = v;
        }
        if let Some(v) = self.height {
            style.height = v;
        }
        if let Some(v) = self.min_width {
            style.min_width = v;
        }
        if let Some(v) = self.min_height {
            style.min_height = v;
        }
        if let Some(v) = self.max_width {
            style.max_width = v;
        }
        if let Some(v) = self.max_height {
            style.max_height = v;
        }
        for (edge, value) in self.margin.iter().enumerate() {
            let Some(v) = *value else { continue };
            match edge {
                0 => style.margin.top = v,
                1 => style.margin.right = v,
                2 => style.margin.bottom = v,
                _ => style.margin.left = v,
            }
        }
        for (edge, value) in self.padding.iter().enumerate() {
            let Some(v) = *value else { continue };
            match edge {
                0 => style.padding.top = v,
                1 => style.padding.right = v,
                2 => style.padding.bottom = v,
                _ => style.padding.left = v,
            }
        }
        for (key, value) in &self.markers {
            match value {
                Some(value) => {
                    style
                        .custom_properties
                        .get_or_insert_with(Default::default)
                        .insert(key.clone(), value.clone());
                }
                None => {
                    if let Some(properties) = style.custom_properties.as_mut() {
                        properties.remove(key);
                    }
                }
            }
        }
    }
}

/// The physical edge a logical block-start edge resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhysicalSide {
    Top,
    Right,
    Left,
}

impl WritingMode {
    /// Block-start edge (CSS Writing Modes): top in horizontal-tb, right in
    /// vertical-rl and sideways-rl, left in vertical-lr and sideways-lr. The
    /// block-end edge is the opposite one.
    pub fn block_start_side(self) -> PhysicalSide {
        match self {
            WritingMode::HorizontalTb => PhysicalSide::Top,
            WritingMode::VerticalRl | WritingMode::SidewaysRl => PhysicalSide::Right,
            WritingMode::VerticalLr | WritingMode::SidewaysLr => PhysicalSide::Left,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum UnicodeBidi {
    #[default]
    Normal,
    Embed,
    BidiOverride,
    Isolate,
    IsolateOverride,
    Plaintext,
}

// --- CSS Animation (#11) ---

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Animation {
    pub name: String,
    pub duration_ms: u32,
    pub easing: Easing,
    /// Signed: a negative animation-delay starts part-way through.
    pub delay_ms: i32,
    pub iteration_count: AnimationIterationCount,
    pub direction: AnimationDirection,
    pub fill_mode: AnimationFillMode,
    /// `animation-play-state: paused`.
    #[serde(default)]
    pub paused: bool,
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

    pub fn inset(ox: f32, oy: f32, blur: f32, spread: f32, color: Color) -> Self {
        Self {
            inset: true,
            ..Self::new(ox, oy, blur, spread, color)
        }
    }
}

/// CSS `text-shadow` (no spread, no inset).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TextShadow {
    pub offset_x: f32,
    pub offset_y: f32,
    pub blur_radius: f32,
    pub color: Color,
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
            Dimension::Ch(v) => Some(*v * local_font_size * 0.5),
            Dimension::Vw(v) => Some(*v * viewport_w / 100.0),
            Dimension::Vh(v) => Some(*v * viewport_h / 100.0),
            Dimension::Calc { .. } => None,
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
