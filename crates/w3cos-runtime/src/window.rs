use anyhow::Result;
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

#[cfg(feature = "cpu-render")]
use std::rc::Rc;

#[cfg(feature = "gpu")]
use std::sync::Arc;
use winit::application::ApplicationHandler;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
use winit::dpi::LogicalSize;
use winit::event::{
    ElementState, MouseButton, MouseScrollDelta, StartCause, TouchPhase, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowAttributes, WindowId};

/// Desktop startup size. Mobile windows must use the platform's native
/// fullscreen bounds; forcing a reference-device size clips other devices.
#[cfg(not(any(target_os = "ios", target_os = "android")))]
fn default_logical_size() -> LogicalSize<f64> {
    LogicalSize::new(1200.0, 800.0)
}

fn default_window_attributes() -> WindowAttributes {
    let attributes = WindowAttributes::default().with_title("W3C OS");
    #[cfg(any(target_os = "ios", target_os = "android"))]
    {
        attributes
    }
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    {
        attributes.with_inner_size(default_logical_size())
    }
}

/// Physical backing-store size for the platform view.
///
/// winit's iOS `inner_size()` reports the safe-area rectangle, while wgpu's
/// raw UIKit handle targets the full-screen UIView. A safe-area-sized Metal
/// drawable is therefore stretched by Core Animation and distorts the scene.
/// Safe areas belong to CSS env(), not the GPU backing store.
fn window_backing_size(window: &Window) -> winit::dpi::PhysicalSize<u32> {
    #[cfg(target_os = "ios")]
    {
        window.outer_size()
    }
    #[cfg(not(target_os = "ios"))]
    {
        window.inner_size()
    }
}

#[cfg(feature = "gpu")]
fn gpu_aa_config() -> AaConfig {
    #[cfg(any(target_os = "ios", target_os = "android"))]
    {
        AaConfig::Area
    }
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    {
        AaConfig::Msaa16
    }
}

#[cfg(target_os = "ios")]
const IOS_CONTENT_INSET_TOP: f32 = 47.0;

#[cfg(not(target_os = "ios"))]
const IOS_CONTENT_INSET_TOP: f32 = 0.0;

#[cfg(target_os = "ios")]
fn update_safe_area_from_window(window: &Window, scale: f32) {
    if !w3cos_std::safe_area::is_enabled() {
        return;
    }
    if let Some(insets) = crate::ios_input::safe_area_insets(window) {
        w3cos_std::safe_area::set_insets(insets);
        return;
    }
    let outer = window.outer_size();
    let inner = window.inner_size();
    let pos = window
        .inner_position()
        .unwrap_or(winit::dpi::PhysicalPosition::new(0, 0));
    let top = pos.y as f32 / scale;
    let left = pos.x as f32 / scale;
    let bottom = (outer
        .height
        .saturating_sub(pos.y as u32)
        .saturating_sub(inner.height)) as f32
        / scale;
    let right = (outer
        .width
        .saturating_sub(pos.x as u32)
        .saturating_sub(inner.width)) as f32
        / scale;
    w3cos_std::safe_area::set_insets(w3cos_std::safe_area::SafeAreaInsets {
        top,
        right,
        bottom,
        left,
    });
}

#[cfg(not(target_os = "ios"))]
fn update_safe_area_from_window(_window: &Window, _scale: f32) {}

/// Layout viewport derived from the platform window (Visual Viewport semantics).
#[derive(Clone, Copy, Debug, PartialEq)]
struct ViewportLayout {
    layout_w: f32,
    layout_h: f32,
    offset_y: f32,
    keyboard_inset_bottom: f32,
}

/// Estimated soft-keyboard height when the platform does not shrink `content_rect`
/// (common with `NativeActivity` + pan). ~260 CSS px ≈ typical Android IME.
const ANDROID_IME_FALLBACK_INSET: f32 = 260.0;

impl ViewportLayout {
    fn from_window(window: &Window, scale: f32, inset_top: f32, _ime_open: bool) -> Self {
        let size = window_backing_size(window);
        let full_w = size.width as f32 / scale;
        let full_h = size.height as f32 / scale;
        let mut keyboard_inset_bottom = 0.0_f32;
        let mut layout_w = full_w;
        let mut layout_h = (full_h - inset_top).max(1.0);
        let mut offset_y = inset_top;

        #[cfg(target_os = "android")]
        {
            use winit::platform::android::WindowExtAndroid;
            let rect = window.content_rect();
            let rw = rect.right - rect.left;
            let rh = rect.bottom - rect.top;
            if rw > 0 && rh > 0 {
                layout_w = rw as f32 / scale;
                let visible_h = rh as f32 / scale;
                offset_y = inset_top + rect.top as f32 / scale;
                keyboard_inset_bottom = (full_h - visible_h - rect.top as f32 / scale).max(0.0);
            }
            if _ime_open && keyboard_inset_bottom < 8.0 {
                keyboard_inset_bottom = ANDROID_IME_FALLBACK_INSET;
            }
        }

        #[cfg(target_os = "ios")]
        {
            keyboard_inset_bottom = crate::ios_input::keyboard_inset_bottom(window).unwrap_or(0.0);
        }

        if w3cos_std::viewport::interactive_widget().resizes_layout_viewport()
            && keyboard_inset_bottom > 0.0
        {
            layout_h = (full_h - inset_top - keyboard_inset_bottom).max(1.0);
            #[cfg(target_os = "android")]
            if offset_y > inset_top + 0.5 {
                offset_y = inset_top;
            }
        } else if keyboard_inset_bottom > 0.0 {
            layout_h = (full_h - inset_top).max(1.0);
            offset_y = inset_top;
        } else {
            #[cfg(target_os = "android")]
            {
                use winit::platform::android::WindowExtAndroid;
                let rect = window.content_rect();
                let rh = rect.bottom - rect.top;
                if rh > 0 {
                    layout_h = (rh as f32 / scale - inset_top).max(1.0);
                    offset_y = inset_top + rect.top as f32 / scale;
                }
            }
        }

        w3cos_std::keyboard_inset::set_bottom(keyboard_inset_bottom);
        Self {
            layout_w,
            layout_h,
            offset_y,
            keyboard_inset_bottom,
        }
    }

    fn ime_open_for_app(app: &App) -> bool {
        app.focused_index.is_some_and(|idx| {
            matches!(app.get_kind_at(idx), Some(ComponentKind::TextInput { .. }))
        })
    }
}

#[cfg(feature = "cpu-render")]
use std::num::NonZeroU32;
#[cfg(feature = "cpu-render")]
use tiny_skia::Pixmap;

#[cfg(feature = "gpu")]
use vello::peniko::FontData;
#[cfg(feature = "gpu")]
use vello::util::{RenderContext, RenderSurface};
#[cfg(feature = "gpu")]
use vello::wgpu;
#[cfg(feature = "gpu")]
use vello::{AaConfig, Renderer, RendererOptions, Scene};

use crate::compositor::lerp_transform;
use crate::fling::MobileFlingCurve;
use crate::layout::{self, LayoutEngine, LayoutRect, ScrollExtent};
use crate::overscroll::OverscrollState;
#[cfg(feature = "cpu-render")]
use crate::render_cpu;
#[cfg(feature = "gpu")]
use crate::render_gpu;
use crate::state;
use crate::virtual_list::{KeyedVirtualList, VirtualListConfig, VisibleWindow};
use w3cos_std::color::Color;
use w3cos_std::style::{
    Dimension, Easing, OverscrollBehavior, Position, Transform2D, TransitionProperty,
};
use w3cos_std::{Component, ComponentKind, EventAction};

#[cfg(any(target_os = "ios", target_os = "android"))]
static EMBEDDED_FONT: &[u8] = include_bytes!("../assets/CJK-Subset.ttf");
#[cfg(not(any(target_os = "ios", target_os = "android")))]
static EMBEDDED_FONT: &[u8] = include_bytes!("../assets/Inter-Regular.ttf");

const ANIMATION_FRAME_INTERVAL_MS: u64 = 16;
const TOUCH_SCROLL_SLOP: f32 = 8.0;
const KINETIC_SCROLL_MIN_VELOCITY: f32 = 80.0;

#[cfg(target_os = "ios")]
const IOS_IME_RETRY_INTERVAL_MS: u64 = 16;
#[cfg(target_os = "ios")]
const IOS_IME_RETRY_LIMIT: u8 = 24;
#[cfg(target_os = "ios")]
const IOS_IME_VIEWPORT_POLL_LIMIT: u8 = 24;
#[cfg(target_os = "ios")]
const IOS_IME_IDLE_POLL_INTERVAL_MS: u64 = 33;

#[cfg(target_os = "ios")]
#[derive(Clone, Copy)]
struct IosImeRetry {
    deadline: Instant,
    attempts: u8,
}
const KINETIC_SCROLL_STOP_VELOCITY: f32 = 12.0;
// At large virtual-list offsets f32's representable step can exceed 0.001px.
// A sub-0.05px remainder is numerical noise, not an unconsumed boundary delta.
const SCROLL_CHAIN_EPSILON: f32 = 0.05;
const KINETIC_SCROLL_MAX_VELOCITY: f32 = 6_000.0;
const KINETIC_VELOCITY_WINDOW: Duration = Duration::from_millis(150);

// ---------------------------------------------------------------------------
// HitNode — interactive region for click/focus
// ---------------------------------------------------------------------------

struct HitNode {
    rect: LayoutRect,
    index: usize,
    is_interactive: bool,
    is_focusable: bool,
    on_click: EventAction,
}

#[derive(Clone, Copy)]
struct ScrollDamage {
    index: usize,
    delta_y: f32,
}

#[derive(Clone, Default)]
enum RepaintMode {
    Full,
    ScrollOnly(Vec<ScrollDamage>),
    #[default]
    Clean,
}

impl RepaintMode {
    fn queue_scroll_damage(&mut self, index: usize, delta_y: f32) {
        match self {
            RepaintMode::ScrollOnly(damages) => {
                if let Some(damage) = damages.iter_mut().find(|damage| damage.index == index) {
                    damage.delta_y += delta_y;
                } else {
                    damages.push(ScrollDamage { index, delta_y });
                }
            }
            RepaintMode::Clean => {
                *self = RepaintMode::ScrollOnly(vec![ScrollDamage { index, delta_y }]);
            }
            // Layout/style/React tree invalidation already requires a complete
            // repaint. A later scroll event in the same frame must not
            // downgrade it to retained framebuffer strip-copying.
            RepaintMode::Full => {}
        }
    }
}

struct KineticScroll {
    index: usize,
    curve: MobileFlingCurve,
    started_at: Instant,
    last_offset: f32,
}

fn estimate_touch_velocity(samples: &VecDeque<(Instant, f32)>, now: Instant) -> Option<f32> {
    let &(latest_time, latest_y) = samples.back()?;
    if now.duration_since(latest_time) > KINETIC_VELOCITY_WINDOW {
        return None;
    }
    // The queue deliberately retains one sample immediately before the
    // window boundary. Including it avoids iOS's tiny terminal Move events
    // collapsing an otherwise fast fling to zero velocity.
    let &(earliest_time, earliest_y) = samples.front()?;
    let elapsed = latest_time.duration_since(earliest_time).as_secs_f32();
    (elapsed >= 0.008).then(|| {
        ((earliest_y - latest_y) / elapsed)
            .clamp(-KINETIC_SCROLL_MAX_VELOCITY, KINETIC_SCROLL_MAX_VELOCITY)
    })
}

fn bounded_scroll_step(stored_offset: f32, delta: f32, max_offset: f32) -> (f32, f32, f32) {
    let base_offset = stored_offset.clamp(0.0, max_offset);
    let next_offset = (base_offset + delta).clamp(0.0, max_offset);
    (base_offset, next_offset, next_offset - base_offset)
}

fn scroll_damage_crosses_stacking_context(
    damages: &[ScrollDamage],
    paint_z: &[i32],
    scrollable: &[(usize, LayoutRect, ScrollExtent)],
    painted_rects: &[(usize, LayoutRect)],
) -> bool {
    damages.iter().any(|damage| {
        let target_z = paint_z.get(damage.index).copied().unwrap_or_default();
        if target_z != 0 {
            return true;
        }
        let Some(scroll_rect) = scrollable
            .iter()
            .find(|(idx, _, _)| *idx == damage.index)
            .map(|(_, rect, _)| *rect)
        else {
            return true;
        };
        painted_rects.iter().any(|(idx, rect)| {
            paint_z.get(*idx).copied().unwrap_or_default() > target_z
                && rect.x < scroll_rect.x + scroll_rect.width
                && rect.x + rect.width > scroll_rect.x
                && rect.y < scroll_rect.y + scroll_rect.height
                && rect.y + rect.height > scroll_rect.y
        })
    })
}

// ---------------------------------------------------------------------------
// SpatialGrid — O(1) hit testing via grid-based spatial hash
// ---------------------------------------------------------------------------

struct SpatialGrid {
    cell_size: f32,
    cols: usize,
    cells: Vec<Vec<usize>>,
}

impl SpatialGrid {
    fn empty() -> Self {
        Self {
            cell_size: 64.0,
            cols: 1,
            cells: Vec::new(),
        }
    }

    fn build(hit_nodes: &[HitNode], viewport_w: f32, viewport_h: f32) -> Self {
        let cell_size = 64.0f32;
        let cols = ((viewport_w / cell_size).ceil() as usize).max(1);
        let rows = ((viewport_h / cell_size).ceil() as usize).max(1);
        let mut cells = vec![Vec::new(); cols * rows];

        for (i, hit) in hit_nodes.iter().enumerate() {
            if !hit.is_interactive {
                continue;
            }
            let x0 = (hit.rect.x / cell_size).floor().max(0.0) as usize;
            let y0 = (hit.rect.y / cell_size).floor().max(0.0) as usize;
            let x1 = ((hit.rect.x + hit.rect.width) / cell_size)
                .ceil()
                .min(cols as f32) as usize;
            let y1 = ((hit.rect.y + hit.rect.height) / cell_size)
                .ceil()
                .min(rows as f32) as usize;

            for cy in y0..y1 {
                for cx in x0..x1 {
                    if cy < rows && cx < cols {
                        cells[cy * cols + cx].push(i);
                    }
                }
            }
        }

        Self {
            cell_size,
            cols,
            cells,
        }
    }

    fn query(
        &self,
        x: f32,
        y: f32,
        hit_nodes: &[HitNode],
        parents: &[Option<usize>],
    ) -> Option<usize> {
        if self.cells.is_empty() {
            return None;
        }
        let cx = (x / self.cell_size).floor() as usize;
        let cy = (y / self.cell_size).floor() as usize;
        let cell_idx = cy * self.cols + cx;

        if cell_idx >= self.cells.len() {
            return None;
        }

        for &hit_idx in self.cells[cell_idx].iter().rev() {
            let hit = &hit_nodes[hit_idx];
            if x >= hit.rect.x
                && x <= hit.rect.x + hit.rect.width
                && y >= hit.rect.y
                && y <= hit.rect.y + hit.rect.height
            {
                let mut cur = Some(hit.index);
                while let Some(idx) = cur {
                    if let Some(h) = hit_nodes.iter().find(|h| h.index == idx) {
                        if h.is_interactive {
                            return Some(idx);
                        }
                    }
                    cur = parents.get(idx).copied().flatten();
                }
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// ActiveAnimation
// ---------------------------------------------------------------------------

enum ActiveAnimation {
    LayoutHeight {
        target_id: u64,
        node_index: usize,
        from: f32,
        to: f32,
        start: Instant,
        duration_ms: f64,
        delay_ms: f64,
        easing: Easing,
    },
    Opacity {
        target_id: u64,
        node_index: usize,
        from: f32,
        to: f32,
        start: Instant,
        duration_ms: f64,
        delay_ms: f64,
        easing: Easing,
    },
    Background {
        target_id: u64,
        node_index: usize,
        from: Color,
        to: Color,
        start: Instant,
        duration_ms: f64,
        delay_ms: f64,
        easing: Easing,
    },
    Transform {
        target_id: u64,
        node_index: usize,
        from: Transform2D,
        to: Transform2D,
        start: Instant,
        duration_ms: f64,
        delay_ms: f64,
        easing: Easing,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnimatedProperty {
    LayoutHeight,
    Opacity,
    Background,
    Transform,
}

impl ActiveAnimation {
    fn target_id(&self) -> u64 {
        match self {
            ActiveAnimation::LayoutHeight { target_id, .. }
            | ActiveAnimation::Opacity { target_id, .. }
            | ActiveAnimation::Background { target_id, .. }
            | ActiveAnimation::Transform { target_id, .. } => *target_id,
        }
    }

    fn property(&self) -> AnimatedProperty {
        match self {
            ActiveAnimation::LayoutHeight { .. } => AnimatedProperty::LayoutHeight,
            ActiveAnimation::Opacity { .. } => AnimatedProperty::Opacity,
            ActiveAnimation::Background { .. } => AnimatedProperty::Background,
            ActiveAnimation::Transform { .. } => AnimatedProperty::Transform,
        }
    }

    fn node_index(&self) -> usize {
        match self {
            ActiveAnimation::LayoutHeight { node_index, .. } => *node_index,
            ActiveAnimation::Opacity { node_index, .. } => *node_index,
            ActiveAnimation::Background { node_index, .. } => *node_index,
            ActiveAnimation::Transform { node_index, .. } => *node_index,
        }
    }

    fn progress(&self, now: Instant) -> f32 {
        let elapsed_ms = now
            .duration_since(match self {
                ActiveAnimation::LayoutHeight { start, .. } => *start,
                ActiveAnimation::Opacity { start, .. } => *start,
                ActiveAnimation::Background { start, .. } => *start,
                ActiveAnimation::Transform { start, .. } => *start,
            })
            .as_secs_f64()
            * 1000.0;
        let delay_ms = match self {
            ActiveAnimation::LayoutHeight { delay_ms, .. } => *delay_ms,
            ActiveAnimation::Opacity { delay_ms, .. } => *delay_ms,
            ActiveAnimation::Background { delay_ms, .. } => *delay_ms,
            ActiveAnimation::Transform { delay_ms, .. } => *delay_ms,
        };
        let duration_ms = match self {
            ActiveAnimation::LayoutHeight { duration_ms, .. } => *duration_ms,
            ActiveAnimation::Opacity { duration_ms, .. } => *duration_ms,
            ActiveAnimation::Background { duration_ms, .. } => *duration_ms,
            ActiveAnimation::Transform { duration_ms, .. } => *duration_ms,
        };
        let effective_elapsed = elapsed_ms - delay_ms;
        if effective_elapsed <= 0.0 {
            return 0.0;
        }
        (effective_elapsed / duration_ms).min(1.0) as f32
    }

    fn is_complete(&self, now: Instant) -> bool {
        self.progress(now) >= 1.0
    }

    fn eased_progress(&self, now: Instant) -> f32 {
        let progress = self.progress(now);
        match self {
            ActiveAnimation::LayoutHeight { easing, .. }
            | ActiveAnimation::Opacity { easing, .. }
            | ActiveAnimation::Background { easing, .. }
            | ActiveAnimation::Transform { easing, .. } => easing.interpolate(progress),
        }
    }

    fn sampled_height(&self, now: Instant) -> Option<f32> {
        let ActiveAnimation::LayoutHeight { from, to, .. } = self else {
            return None;
        };
        Some(*from + self.eased_progress(now) * (*to - *from))
    }

    fn sampled_opacity(&self, now: Instant) -> Option<f32> {
        let ActiveAnimation::Opacity { from, to, .. } = self else {
            return None;
        };
        Some(*from + self.eased_progress(now) * (*to - *from))
    }

    fn sampled_background(&self, now: Instant) -> Option<Color> {
        let ActiveAnimation::Background { from, to, .. } = self else {
            return None;
        };
        let progress = self.eased_progress(now);
        Some(Color::rgba(
            lerp_u8(from.r, to.r, progress),
            lerp_u8(from.g, to.g, progress),
            lerp_u8(from.b, to.b, progress),
            lerp_u8(from.a, to.a, progress),
        ))
    }

    fn sampled_transform(&self, now: Instant) -> Option<Transform2D> {
        let ActiveAnimation::Transform { from, to, .. } = self else {
            return None;
        };
        Some(lerp_transform(*from, *to, self.eased_progress(now)))
    }
}

fn transition_pair_id(parent_id: u64, old_id: u64, new_id: u64) -> u64 {
    let (first, second) = if old_id <= new_id {
        (old_id, new_id)
    } else {
        (new_id, old_id)
    };
    parent_id.rotate_left(13) ^ first.rotate_left(29) ^ second.rotate_left(47)
}

fn animated_layout_cache(
    layout_cache: &[(LayoutRect, usize)],
    animations: &[ActiveAnimation],
    now: Instant,
) -> Option<Vec<(LayoutRect, usize)>> {
    if !animations
        .iter()
        .any(|animation| matches!(animation, ActiveAnimation::LayoutHeight { .. }))
    {
        return None;
    }

    let mut animated = layout_cache.to_vec();
    for animation in animations {
        let ActiveAnimation::LayoutHeight {
            node_index,
            from,
            to,
            ..
        } = animation
        else {
            continue;
        };
        let eased = animation.eased_progress(now);
        if let Some((rect, _)) = animated.iter_mut().find(|(_, idx)| idx == node_index) {
            rect.height = from + eased * (to - from);
        }
    }
    Some(animated)
}

fn animated_clip_nodes(
    clip_nodes: &[(usize, LayoutRect)],
    animations: &[ActiveAnimation],
    now: Instant,
) -> Option<Vec<(usize, LayoutRect)>> {
    if !animations
        .iter()
        .any(|animation| matches!(animation, ActiveAnimation::LayoutHeight { .. }))
    {
        return None;
    }

    let mut animated = clip_nodes.to_vec();
    for animation in animations {
        let ActiveAnimation::LayoutHeight {
            node_index,
            from,
            to,
            ..
        } = animation
        else {
            continue;
        };
        let eased = animation.eased_progress(now);
        if let Some((_, rect)) = animated.iter_mut().find(|(idx, _)| idx == node_index) {
            rect.height = from + eased * (to - from);
        }
    }
    Some(animated)
}

// ---------------------------------------------------------------------------
// GPU render state
// ---------------------------------------------------------------------------
#[cfg(feature = "gpu")]
enum GpuState {
    Active {
        surface: RenderSurface<'static>,
        window: Arc<Window>,
    },
    Suspended(Option<Arc<Window>>),
}

// ---------------------------------------------------------------------------
// AI Bridge screenshot provider — reads the cached framebuffer
// ---------------------------------------------------------------------------
#[cfg(feature = "ai-bridge")]
struct FrameCacheScreenshot;

#[cfg(feature = "ai-bridge")]
impl w3cos_ai_bridge::server::ScreenshotProvider for FrameCacheScreenshot {
    fn capture_png(&self) -> Option<Vec<u8>> {
        crate::frame_cache::encode_png()
    }
}

// ---------------------------------------------------------------------------
// CPU presenter — softbuffer context/surface must be created once, not per frame
// ---------------------------------------------------------------------------

#[cfg(feature = "cpu-render")]
struct CpuPresenter {
    window: Rc<Window>,
    context: softbuffer::Context<winit::event_loop::OwnedDisplayHandle>,
    surface: softbuffer::Surface<winit::event_loop::OwnedDisplayHandle, Rc<Window>>,
    framebuffer: Option<Pixmap>,
    clip_masks: render_cpu::ClipMaskCache,
    buffer_size: (u32, u32),
}

struct ComponentVirtualList {
    engine: KeyedVirtualList<usize, Component, HashMap<usize, String>, ()>,
    template: Component,
    window: VisibleWindow,
    scroll_offset: f32,
}

/// Retained PrePaint data. Blink keeps this information beside the layout
/// tree and only rebuilds it after layout/style invalidation; scroll frames
/// then consume it without walking the component tree again.
#[derive(Clone)]
struct RetainedPaintNode {
    kind: ComponentKind,
    style: w3cos_std::style::Style,
    parent: Option<usize>,
}

// ---------------------------------------------------------------------------
// App struct
// ---------------------------------------------------------------------------
struct App {
    builder: Option<fn() -> Component>,
    dom_setup: Option<fn()>,
    dom_mode: bool,
    root: Component,
    font: fontdue::Font,
    mouse_x: f32,
    mouse_y: f32,
    scale_factor: f64,
    hovered_index: Option<usize>,
    pressed_index: Option<usize>,
    focused_index: Option<usize>,
    #[cfg(target_os = "ios")]
    ios_ime_retry: Option<IosImeRetry>,
    #[cfg(target_os = "ios")]
    ios_ime_viewport_poll: Option<IosImeRetry>,
    text_input_values: HashMap<usize, String>,
    hit_nodes: Vec<HitNode>,
    focusable_indices: Vec<usize>,
    layout_cache: Vec<(LayoutRect, usize)>,
    scrollable_nodes: Vec<(usize, LayoutRect, ScrollExtent)>,
    clip_only_nodes: Vec<(usize, LayoutRect)>,
    scroll_offsets: HashMap<usize, (f32, f32)>,
    overscroll_states: HashMap<usize, OverscrollState>,
    last_overscroll_tick: Option<Instant>,
    initialized_scroll_targets: HashSet<usize>,
    user_scrolled_nodes: HashSet<usize>,
    sticky_counter_bases: HashMap<usize, i64>,
    sticky_marker_index: HashMap<usize, HashMap<usize, Vec<f32>>>,
    pending_sticky_scrolls: HashSet<usize>,
    virtual_lists: HashMap<usize, ComponentVirtualList>,
    virtual_scroll_indices: HashMap<usize, usize>,
    needs_layout: bool,
    needs_tree_rebuild: bool,
    needs_style_refresh: bool,
    animations: Vec<ActiveAnimation>,
    last_frame_time: Option<Instant>,
    modifiers: ModifiersState,
    last_touch_y: Option<f32>,
    touch_samples: VecDeque<(Instant, f32)>,
    touch_drag_y: f32,
    touch_scroll_active: bool,
    touch_scroll_index: Option<usize>,
    kinetic_scroll: Option<KineticScroll>,
    content_inset_top: f32,
    viewport: ViewportLayout,
    repaint_mode: RepaintMode,
    recent_scroll_damage: Vec<ScrollDamage>,

    /// UA presenter selection when both GPU and CPU backends are compiled in.
    #[cfg(all(feature = "gpu", feature = "cpu-render"))]
    using_gpu: bool,

    // Performance: persistent layout engine (avoids TaffyTree rebuild on resize)
    layout_engine: LayoutEngine,
    // Performance: scroll ancestor map (avoids O(n*depth) parent walk)
    scroll_ancestor: Vec<Option<usize>>,
    flat_parents: Vec<Option<usize>>,
    retained_paint_nodes: Vec<RetainedPaintNode>,
    retained_paint_z: Vec<i32>,
    retained_sticky_owner: Vec<Option<usize>>,
    retained_rect_by_index: Vec<Option<LayoutRect>>,
    // Performance: spatial grid for O(1) hit testing
    spatial_grid: SpatialGrid,
    // Performance: dirty frame detection
    paint_generation: u64,
    layout_generation: u64,

    // DevTools integration (Chrome DevTools Protocol)
    #[cfg(feature = "devtools")]
    devtools_handle: Option<crate::devtools::DevToolsHandle>,
    #[cfg(feature = "devtools")]
    devtools_highlight: Option<i64>,

    // AI Bridge HTTP server
    #[cfg(feature = "ai-bridge")]
    ai_bridge_handle: Option<w3cos_ai_bridge::AiBridgeHandle>,

    // GPU-specific
    #[cfg(feature = "gpu")]
    render_cx: RenderContext,
    #[cfg(feature = "gpu")]
    renderers: Vec<Option<Renderer>>,
    #[cfg(feature = "gpu")]
    gpu_state: GpuState,
    #[cfg(feature = "gpu")]
    scene: Scene,
    #[cfg(feature = "gpu")]
    font_data: FontData,
    #[cfg(feature = "gpu")]
    glyph_cache: render_gpu::GlyphCache,
    #[cfg(feature = "gpu")]
    gpu_filter_pipelines: Option<crate::gpu_filter::GpuFilterPipelines>,
    #[cfg(feature = "gpu")]
    gpu_layer_textures: Option<crate::gpu_filter::GpuLayerTextures>,
    #[cfg(feature = "gpu")]
    gpu_output_texture_pool: crate::gpu_filter::GpuOutputTexturePool,

    // CPU presenter — softbuffer path; also used as GPU fallback.
    #[cfg(feature = "cpu-render")]
    cpu: Option<CpuPresenter>,
}

impl App {
    fn new_reactive(builder: fn() -> Component) -> Self {
        let root = builder();
        Self::create(Some(builder), None, false, root)
    }

    fn new_static(root: Component) -> Self {
        Self::create(None, None, false, root)
    }

    fn new_dom(setup: fn()) -> Self {
        crate::dom::reset_document();
        setup();
        let root = crate::dom::to_component_tree();
        crate::dom::clear_document_dirty();
        Self::create(None, Some(setup), true, root)
    }

    fn create(
        builder: Option<fn() -> Component>,
        dom_setup: Option<fn()>,
        dom_mode: bool,
        root: Component,
    ) -> Self {
        let font = fontdue::Font::from_bytes(EMBEDDED_FONT, fontdue::FontSettings::default())
            .expect("failed to load embedded font");

        Self {
            builder,
            dom_setup,
            dom_mode,
            root,
            font,
            mouse_x: 0.0,
            mouse_y: 0.0,
            scale_factor: 1.0,
            hovered_index: None,
            pressed_index: None,
            focused_index: None,
            #[cfg(target_os = "ios")]
            ios_ime_retry: None,
            #[cfg(target_os = "ios")]
            ios_ime_viewport_poll: None,
            text_input_values: HashMap::new(),
            hit_nodes: Vec::new(),
            focusable_indices: Vec::new(),
            layout_cache: Vec::new(),
            scrollable_nodes: Vec::new(),
            clip_only_nodes: Vec::new(),
            scroll_offsets: HashMap::new(),
            overscroll_states: HashMap::new(),
            last_overscroll_tick: None,
            initialized_scroll_targets: HashSet::new(),
            user_scrolled_nodes: HashSet::new(),
            sticky_counter_bases: HashMap::new(),
            sticky_marker_index: HashMap::new(),
            pending_sticky_scrolls: HashSet::new(),
            virtual_lists: HashMap::new(),
            virtual_scroll_indices: HashMap::new(),
            needs_layout: true,
            needs_tree_rebuild: true,
            needs_style_refresh: false,
            animations: Vec::new(),
            last_frame_time: None,
            modifiers: ModifiersState::default(),
            last_touch_y: None,
            touch_samples: VecDeque::new(),
            touch_drag_y: 0.0,
            touch_scroll_active: false,
            touch_scroll_index: None,
            kinetic_scroll: None,
            content_inset_top: if w3cos_std::safe_area::is_enabled() {
                0.0
            } else {
                IOS_CONTENT_INSET_TOP
            },
            viewport: ViewportLayout {
                layout_w: 1.0,
                layout_h: 1.0,
                offset_y: 0.0,
                keyboard_inset_bottom: 0.0,
            },
            repaint_mode: RepaintMode::Full,
            recent_scroll_damage: Vec::new(),
            #[cfg(all(feature = "gpu", feature = "cpu-render"))]
            using_gpu: true,

            layout_engine: LayoutEngine::new(),
            scroll_ancestor: Vec::new(),
            flat_parents: Vec::new(),
            retained_paint_nodes: Vec::new(),
            retained_paint_z: Vec::new(),
            retained_sticky_owner: Vec::new(),
            retained_rect_by_index: Vec::new(),
            spatial_grid: SpatialGrid::empty(),
            paint_generation: 0,
            layout_generation: 0,

            #[cfg(feature = "devtools")]
            devtools_handle: None,
            #[cfg(feature = "devtools")]
            devtools_highlight: None,

            #[cfg(feature = "ai-bridge")]
            ai_bridge_handle: None,

            #[cfg(feature = "gpu")]
            render_cx: RenderContext::new(),
            #[cfg(feature = "gpu")]
            renderers: vec![],
            #[cfg(feature = "gpu")]
            gpu_state: GpuState::Suspended(None),
            #[cfg(feature = "gpu")]
            scene: Scene::new(),
            #[cfg(feature = "gpu")]
            font_data: render_gpu::make_font_data(EMBEDDED_FONT),
            #[cfg(feature = "gpu")]
            glyph_cache: render_gpu::GlyphCache::new(),
            #[cfg(feature = "gpu")]
            gpu_filter_pipelines: None,
            #[cfg(feature = "gpu")]
            gpu_layer_textures: None,
            #[cfg(feature = "gpu")]
            gpu_output_texture_pool: crate::gpu_filter::GpuOutputTexturePool::new(),

            #[cfg(feature = "cpu-render")]
            cpu: None,
        }
    }

    fn paint(&mut self) {
        #[cfg(all(feature = "gpu", feature = "cpu-render"))]
        {
            if self.using_gpu {
                self.paint_gpu();
            } else {
                self.paint_cpu();
            }
            crate::uitest::write_snapshot();
            return;
        }
        #[cfg(all(feature = "gpu", not(feature = "cpu-render")))]
        self.paint_gpu();
        #[cfg(all(feature = "cpu-render", not(feature = "gpu")))]
        self.paint_cpu();
        #[cfg(not(all(feature = "gpu", feature = "cpu-render")))]
        crate::uitest::write_snapshot();
    }

    fn get_window_gpu(&self) -> Option<&Window> {
        #[cfg(feature = "gpu")]
        {
            return match &self.gpu_state {
                GpuState::Active { window, .. } => Some(window.as_ref()),
                GpuState::Suspended(Some(w)) => Some(w.as_ref()),
                GpuState::Suspended(None) => None,
            };
        }
        #[cfg(not(feature = "gpu"))]
        {
            None
        }
    }

    fn get_window(&self) -> Option<&Window> {
        #[cfg(all(feature = "gpu", feature = "cpu-render"))]
        {
            if self.using_gpu {
                return self.get_window_gpu();
            }
            return self.cpu.as_ref().map(|cpu| cpu.window.as_ref());
        }
        #[cfg(all(feature = "gpu", not(feature = "cpu-render")))]
        {
            return self.get_window_gpu();
        }
        #[cfg(all(feature = "cpu-render", not(feature = "gpu")))]
        {
            return self.cpu.as_ref().map(|cpu| cpu.window.as_ref());
        }
        #[cfg(not(any(feature = "gpu", feature = "cpu-render")))]
        {
            None
        }
    }

    #[cfg(feature = "cpu-render")]
    fn ensure_cpu_presenter(&mut self, event_loop: &ActiveEventLoop) {
        if self.cpu.is_some() {
            return;
        }
        let attrs = default_window_attributes();
        let window = Rc::new(event_loop.create_window(attrs).unwrap());
        self.scale_factor = window.scale_factor();
        let context = softbuffer::Context::new(event_loop.owned_display_handle())
            .expect("softbuffer context");
        let surface =
            softbuffer::Surface::new(&context, window.clone()).expect("softbuffer surface");
        self.cpu = Some(CpuPresenter {
            window,
            context,
            surface,
            framebuffer: None,
            clip_masks: render_cpu::ClipMaskCache::default(),
            buffer_size: (0, 0),
        });
        self.needs_layout = true;
    }

    fn rebuild_if_dirty(&mut self) {
        let react_dirty = w3cos_react_compat::aot::has_pending_render();
        let signal_dirty = state::is_dirty() || react_dirty;
        let dom_dirty = self.dom_mode && crate::dom::is_document_dirty();

        if !signal_dirty && !dom_dirty {
            return;
        }

        let old_root = self.root.clone();

        if signal_dirty {
            state::clear_dirty();
        }
        if react_dirty {
            // Drain before calling the builder. Ref callbacks and effects may
            // enqueue a follow-up render while the new tree is being built;
            // that work must remain pending for the next event-loop turn.
            w3cos_react_compat::aot::clear_pending_render();
        }
        if dom_dirty {
            crate::dom::clear_document_dirty();
        }

        if self.dom_mode {
            self.root = crate::dom::to_component_tree();
            self.needs_layout = true;
            self.needs_tree_rebuild = true;
            self.repaint_mode = RepaintMode::Full;
            self.hovered_index = None;
            self.pressed_index = None;
            self.collect_transition_animations(&old_root);
        } else if let Some(builder) = self.builder {
            let old_flat = layout::pre_flatten(&old_root);
            self.root = builder();
            let new_flat = layout::pre_flatten(&self.root);
            let display_changed = !layout::layout_display_unchanged(&old_flat, &new_flat);
            self.needs_layout = true;
            // Stable Show slots already exist in the persistent Taffy tree.
            // Patch their display styles and let Taffy dirty only affected
            // ancestors; rebuilding the entire tree makes a local card toggle
            // proportional to a long conversation's total node count.
            self.needs_tree_rebuild = !layout::layout_shape_unchanged(&old_flat, &new_flat);
            self.needs_style_refresh = display_changed && !self.needs_tree_rebuild;
            self.repaint_mode = if react_dirty {
                match std::mem::take(&mut self.repaint_mode) {
                    // A fixed-size virtual window only unmounts rows that have
                    // already left the viewport and mounts rows in the newly
                    // exposed strip. Keep the accumulated raster-copy damage;
                    // the explicit Clean state guarantees that a later scroll
                    // event can no longer overwrite a pre-existing Full paint.
                    RepaintMode::ScrollOnly(damages) => RepaintMode::ScrollOnly(damages),
                    RepaintMode::Clean if !self.recent_scroll_damage.is_empty() => {
                        RepaintMode::Clean
                    }
                    RepaintMode::Full | RepaintMode::Clean => RepaintMode::Full,
                }
            } else {
                RepaintMode::Full
            };
            if react_dirty {
                self.recent_scroll_damage.clear();
            }
            self.hovered_index = None;
            self.pressed_index = None;
            self.collect_transition_animations(&old_root);
        }
    }

    fn materialize_virtual_list(
        &mut self,
        ordinal: usize,
        viewport_extent: f32,
        scroll_offset: f32,
    ) -> bool {
        let Some(node) = nth_virtual_list_mut(&mut self.root, ordinal) else {
            return false;
        };
        let ComponentKind::VirtualList {
            item_count,
            estimated_item_height,
            overscan,
            total_extent,
        } = node.kind
        else {
            return false;
        };
        if !self.virtual_lists.contains_key(&ordinal) {
            let Some(template) = node.children.first().cloned() else {
                return false;
            };
            self.virtual_lists.insert(
                ordinal,
                ComponentVirtualList {
                    engine: KeyedVirtualList::new(VirtualListConfig::new(
                        item_count,
                        estimated_item_height,
                        overscan,
                    )),
                    template,
                    window: VisibleWindow {
                        start: 0,
                        end: 0,
                        before_extent: 0.0,
                        visible_extent: 0.0,
                        after_extent: 0.0,
                    },
                    scroll_offset: 0.0,
                },
            );
        }

        let host = self.virtual_lists.get_mut(&ordinal).unwrap();
        host.engine.resize(item_count);
        let anchor_index = host.engine.index_at_offset(scroll_offset);
        host.engine.set_anchor(
            anchor_index,
            anchor_index,
            host.engine.offset_of(anchor_index) - scroll_offset,
        );
        let window = host.engine.visible_window(scroll_offset, viewport_extent);
        if host.window == window
            && (host.scroll_offset - scroll_offset).abs() <= 0.01
            && (host.engine.total_extent() - total_extent).abs() <= 0.01
        {
            return false;
        }
        let template = host.template.clone();
        host.engine.reconcile(
            window,
            |index| index,
            |_, index| virtual_item_from_template(&template, index),
            |component, _, index| *component = virtual_item_from_template(&template, index),
            |_| None,
            |_, _| {},
        );
        host.window = window;
        host.scroll_offset = scroll_offset;

        let mut children = Vec::with_capacity(host.engine.mounted_len() + 2);
        children.push(virtual_spacer(window.before_extent));
        children.extend(host.engine.mounted().map(|item| item.node.clone()));
        children.push(virtual_spacer(window.after_extent));
        node.children = children;
        if let ComponentKind::VirtualList { total_extent, .. } = &mut node.kind {
            *total_extent = host.engine.total_extent();
        }
        true
    }

    fn materialize_all_virtual_lists(&mut self, viewport_extent: f32) -> bool {
        let count = count_virtual_lists(&self.root);
        let mut changed = false;
        for ordinal in 0..count {
            let offset = self
                .virtual_lists
                .get(&ordinal)
                .map(|host| host.scroll_offset)
                .unwrap_or(0.0);
            changed |= self.materialize_virtual_list(ordinal, viewport_extent, offset);
        }
        changed
    }

    fn collect_transition_animations(&mut self, old_root: &Component) {
        let old_flat = layout::pre_flatten(old_root);
        let new_flat = layout::pre_flatten(&self.root);
        let now = Instant::now();
        let old_by_id: HashMap<u64, (usize, &layout::FlatNodeInfo<'_>)> = old_flat
            .iter()
            .enumerate()
            .map(|(idx, node)| (node.stable_id, (idx, node)))
            .collect();

        for (idx, new_node) in new_flat.iter().enumerate() {
            let Some(transition) = &new_node.style.transition else {
                continue;
            };
            let target_id = new_node.stable_id;
            let duration_ms = transition.duration_ms as f64;
            let delay_ms = transition.delay_ms as f64;
            let easing = transition.easing;

            let animates_opacity = matches!(
                transition.property,
                TransitionProperty::All | TransitionProperty::Opacity
            );
            let animates_background = matches!(
                transition.property,
                TransitionProperty::All | TransitionProperty::Background
            );
            let animates_transform = matches!(
                transition.property,
                TransitionProperty::All | TransitionProperty::Transform
            );
            let Some((old_idx, old_node)) = old_by_id.get(&target_id).copied() else {
                let is_side_panel = matches!(
                    new_node.style.width,
                    Dimension::Percent(percent) if percent >= 50.0
                ) && matches!(
                    new_node.style.height,
                    Dimension::Percent(percent) if percent >= 90.0
                );
                let is_overlay = matches!(
                    new_node.style.position,
                    Position::Absolute | Position::Fixed
                );
                // React host ids are rebuilt on a state update and virtual
                // windows move later siblings between flat-tree indices. Only
                // overlay-shaped nodes receive an implicit enter transition;
                // ordinary content must provide two concrete style states.
                if !is_side_panel && !is_overlay {
                    continue;
                }
                // React conditionals insert the entering subtree instead of
                // keeping a display:none wrapper around it. CSS transitions
                // still need an initial paint value, just as a browser gets
                // one from an enter class/keyframe before committing the
                // final style on the next frame.
                if !layout::is_node_visible(&new_flat, idx) {
                    continue;
                }
                if animates_opacity && new_node.style.opacity > 0.0 {
                    self.animations.push(ActiveAnimation::Opacity {
                        target_id,
                        node_index: idx,
                        from: 0.0,
                        to: new_node.style.opacity,
                        start: now,
                        duration_ms,
                        delay_ms,
                        easing,
                    });
                }
                if animates_transform {
                    let mut from = new_node.style.transform;
                    if is_side_panel || is_overlay {
                        let panel_width = match new_node.style.width {
                            Dimension::Percent(percent) => self.viewport.layout_w * percent / 100.0,
                            Dimension::Px(width) => width,
                            _ => self.viewport.layout_w * 0.8,
                        };
                        from.translate_x -= panel_width.max(48.0);
                    } else {
                        from.translate_y += 10.0;
                    }
                    self.animations.push(ActiveAnimation::Transform {
                        target_id,
                        node_index: idx,
                        from,
                        to: new_node.style.transform,
                        start: now,
                        duration_ms,
                        delay_ms,
                        easing,
                    });
                }
                continue;
            };
            // A Show/conditional normally toggles display on a stable wrapper,
            // while the transitioning child keeps display:flex in both trees.
            // CSS transitions on that child still need to observe the effective
            // visibility change through its ancestors.
            let became_visible = !layout::is_node_visible(&old_flat, old_idx)
                && layout::is_node_visible(&new_flat, idx);

            if animates_opacity
                && (old_node.style.opacity != new_node.style.opacity || became_visible)
            {
                let from = self
                    .animations
                    .iter()
                    .rev()
                    .find(|animation| {
                        animation.target_id() == target_id
                            && animation.property() == AnimatedProperty::Opacity
                    })
                    .and_then(|animation| animation.sampled_opacity(now))
                    .unwrap_or_else(|| {
                        if became_visible {
                            0.0
                        } else {
                            old_node.style.opacity
                        }
                    });
                self.animations.retain(|animation| {
                    animation.target_id() != target_id
                        || animation.property() != AnimatedProperty::Opacity
                });
                self.animations.push(ActiveAnimation::Opacity {
                    target_id,
                    node_index: idx,
                    from,
                    to: new_node.style.opacity,
                    start: now,
                    duration_ms,
                    delay_ms,
                    easing,
                });
            }
            if animates_background
                && (old_node.style.background.r != new_node.style.background.r
                    || old_node.style.background.g != new_node.style.background.g
                    || old_node.style.background.b != new_node.style.background.b
                    || old_node.style.background.a != new_node.style.background.a)
            {
                let from = self
                    .animations
                    .iter()
                    .rev()
                    .find(|animation| {
                        animation.target_id() == target_id
                            && animation.property() == AnimatedProperty::Background
                    })
                    .and_then(|animation| animation.sampled_background(now))
                    .unwrap_or(old_node.style.background);
                self.animations.retain(|animation| {
                    animation.target_id() != target_id
                        || animation.property() != AnimatedProperty::Background
                });
                self.animations.push(ActiveAnimation::Background {
                    target_id,
                    node_index: idx,
                    from,
                    to: new_node.style.background,
                    start: now,
                    duration_ms,
                    delay_ms,
                    easing,
                });
            }
            if animates_transform
                && (old_node.style.transform != new_node.style.transform || became_visible)
            {
                let from = self
                    .animations
                    .iter()
                    .rev()
                    .find(|animation| {
                        animation.target_id() == target_id
                            && animation.property() == AnimatedProperty::Transform
                    })
                    .and_then(|animation| animation.sampled_transform(now))
                    .unwrap_or_else(|| {
                        let mut from = old_node.style.transform;
                        if became_visible {
                            from = new_node.style.transform;
                            let is_side_panel = matches!(
                                new_node.style.width,
                                Dimension::Percent(percent) if percent >= 50.0
                            ) && matches!(
                                new_node.style.height,
                                Dimension::Percent(percent) if percent >= 90.0
                            );
                            if is_side_panel
                                || matches!(
                                    new_node.style.position,
                                    Position::Absolute | Position::Fixed
                                )
                            {
                                let panel_width = match new_node.style.width {
                                    Dimension::Percent(percent) => {
                                        self.viewport.layout_w * percent / 100.0
                                    }
                                    Dimension::Px(width) => width,
                                    _ => self.viewport.layout_w * 0.8,
                                };
                                from.translate_x -= panel_width.max(48.0);
                            } else {
                                from.translate_y += 10.0;
                            }
                        }
                        from
                    });
                self.animations.retain(|animation| {
                    animation.target_id() != target_id
                        || animation.property() != AnimatedProperty::Transform
                });
                self.animations.push(ActiveAnimation::Transform {
                    target_id,
                    node_index: idx,
                    from,
                    to: new_node.style.transform,
                    start: now,
                    duration_ms,
                    delay_ms,
                    easing,
                });
            }
        }
    }

    fn ensure_layout(&mut self) {
        let window = match self.get_window() {
            Some(w) => w,
            None => return,
        };
        let scale = self.scale_factor as f32;
        let size = window_backing_size(window);
        if size.width == 0 || size.height == 0 {
            return;
        }
        update_safe_area_from_window(&window, scale);
        let inset_top = if w3cos_std::safe_area::is_enabled() {
            0.0
        } else {
            self.content_inset_top
        };
        let viewport = ViewportLayout::from_window(
            &window,
            scale,
            inset_top,
            ViewportLayout::ime_open_for_app(self),
        );
        #[cfg(target_os = "ios")]
        let input_window_offset = window
            .outer_position()
            .map(|position| (position.x as f32 / scale, position.y as f32 / scale))
            .unwrap_or((0.0, 0.0));
        #[cfg(not(target_os = "ios"))]
        let input_window_offset = (0.0, 0.0);

        if std::env::var_os("W3COS_INPUT_TRACE").is_some() && self.viewport != viewport {
            eprintln!(
                "[W3C OS][VIEWPORT] physical={}x{} scale={:.3} logical={:.1}x{:.1} offsetY={:.1} keyboard={:.1}",
                size.width,
                size.height,
                scale,
                viewport.layout_w,
                viewport.layout_h,
                viewport.offset_y,
                viewport.keyboard_inset_bottom
            );
        }

        if !self.needs_layout && !self.layout_cache.is_empty() && self.viewport == viewport {
            return;
        }
        let previous_viewport = self.viewport;
        self.viewport = viewport;

        let w = viewport.layout_w;
        let layout_h = viewport.layout_h;
        let layout_offset_y = viewport.offset_y;

        if self.materialize_all_virtual_lists(layout_h) {
            self.needs_tree_rebuild = true;
        }

        let flat = layout::pre_flatten(&self.root);
        if self.needs_tree_rebuild {
            self.layout_engine.invalidate();
            self.needs_tree_rebuild = false;
            self.needs_style_refresh = false;
        } else if self.needs_style_refresh && self.layout_engine.tree_valid() {
            let _ = self.layout_engine.patch_display_styles(&flat);
            self.needs_style_refresh = false;
        }

        let old_layout_cache = self.layout_cache.clone();
        let results = self
            .layout_engine
            .compute(&self.root, &flat, w, layout_h)
            .unwrap_or_else(|_| layout::LayoutResults::empty());

        self.layout_cache = results.layout_cache;
        self.scrollable_nodes = results.scrollable_nodes;
        for (idx, _, extent) in &self.scrollable_nodes {
            if let Some((x, y)) = self.scroll_offsets.get_mut(idx) {
                *x = (*x).clamp(0.0, extent.max_x);
                *y = (*y).clamp(0.0, extent.max_y);
            }
        }
        self.overscroll_states.retain(|idx, _| {
            self.scrollable_nodes
                .iter()
                .any(|(scroll_idx, _, _)| scroll_idx == idx)
        });
        if self.overscroll_states.is_empty() {
            self.last_overscroll_tick = None;
        }
        self.clip_only_nodes = results.clip_only_nodes;
        self.scroll_ancestor = results.scroll_ancestor;
        self.flat_parents = flat.iter().map(|n| n.parent).collect();
        self.virtual_scroll_indices.clear();
        let mut virtual_ordinal = 0;
        for (idx, node) in flat.iter().enumerate() {
            if matches!(node.kind, ComponentKind::VirtualList { .. }) {
                self.virtual_scroll_indices.insert(idx, virtual_ordinal);
                virtual_ordinal += 1;
            }
        }
        offset_layout_y(
            layout_offset_y,
            &mut self.layout_cache,
            &mut self.scrollable_nodes,
            &mut self.clip_only_nodes,
        );
        apply_initial_scroll_targets(
            &flat,
            &self.layout_cache,
            &self.scrollable_nodes,
            &self.scroll_ancestor,
            &mut self.scroll_offsets,
            &mut self.initialized_scroll_targets,
            &mut self.user_scrolled_nodes,
        );
        self.sticky_marker_index =
            build_sticky_marker_index(&flat, &self.layout_cache, &self.scroll_ancestor);
        Self::collect_layout_transition_animations(
            &mut self.animations,
            &flat,
            &self.layout_cache,
            &old_layout_cache,
            previous_viewport,
            viewport,
        );

        self.hit_nodes.clear();
        self.focusable_indices.clear();
        for &(rect, idx) in &self.layout_cache {
            if let Some(node) = flat.get(idx) {
                if !layout::is_node_visible(&flat, idx) {
                    continue;
                }
                let is_interactive = matches!(node.kind, ComponentKind::Button { .. })
                    || matches!(node.kind, ComponentKind::TextInput { .. })
                    || !node.on_click.is_none();
                let is_focusable = matches!(node.kind, ComponentKind::Button { .. })
                    || matches!(node.kind, ComponentKind::TextInput { .. });
                if is_focusable {
                    self.focusable_indices.push(idx);
                }
                self.hit_nodes.push(HitNode {
                    rect,
                    index: idx,
                    is_interactive,
                    is_focusable,
                    on_click: node.on_click.clone(),
                });
            }
        }

        self.spatial_grid = SpatialGrid::build(&self.hit_nodes, w, layout_h + layout_offset_y);
        crate::uitest::set_input_targets(
            self.hit_nodes
                .iter()
                .filter(|hit| {
                    matches!(
                        self.get_kind_at(hit.index),
                        Some(ComponentKind::TextInput { .. })
                    )
                })
                .map(|hit| crate::uitest::UiInputTarget {
                    index: hit.index,
                    x: hit.rect.x + input_window_offset.0,
                    y: hit.rect.y + input_window_offset.1,
                    width: hit.rect.width,
                    height: hit.rect.height,
                })
                .collect(),
        );

        let virtual_heights_changed = measure_virtual_list_rows(
            &flat,
            &self.layout_cache,
            &self.virtual_scroll_indices,
            &mut self.virtual_lists,
            &mut self.scroll_offsets,
        );
        let (paint_nodes, paint_z, sticky_owner, rect_by_index) =
            build_retained_prepaint(&flat, &self.layout_cache);
        self.retained_paint_nodes = paint_nodes;
        self.retained_paint_z = paint_z;
        self.retained_sticky_owner = sticky_owner;
        self.retained_rect_by_index = rect_by_index;
        self.needs_layout = virtual_heights_changed;
        if virtual_heights_changed {
            self.needs_tree_rebuild = true;
            self.repaint_mode = RepaintMode::Full;
            self.request_repaint();
        }
        self.layout_generation += 1;
        self.ensure_focused_input_visible();
    }

    /// FLIP layout transitions: hit testing moves to the final geometry immediately,
    /// while painting interpolates from the previous position. Text metrics stay at
    /// their final size, so viewport and IME reflow does not stretch glyphs.
    fn collect_layout_transition_animations(
        animations: &mut Vec<ActiveAnimation>,
        flat: &[layout::FlatNodeInfo<'_>],
        layout_cache: &[(LayoutRect, usize)],
        old_layout_cache: &[(LayoutRect, usize)],
        old_viewport: ViewportLayout,
        new_viewport: ViewportLayout,
    ) {
        if old_layout_cache.is_empty() {
            return;
        }
        let old_rects: HashMap<usize, LayoutRect> = old_layout_cache
            .iter()
            .map(|(rect, idx)| (*idx, *rect))
            .collect();
        let new_indices: HashSet<usize> = layout_cache.iter().map(|(_, idx)| *idx).collect();
        let now = Instant::now();
        let before_count = animations.len();
        let viewport_changed = (old_viewport.layout_w - new_viewport.layout_w).abs() >= 0.5
            || (old_viewport.layout_h - new_viewport.layout_h).abs() >= 0.5
            || (old_viewport.offset_y - new_viewport.offset_y).abs() >= 0.5;
        for &(new_rect, idx) in layout_cache {
            let Some(node) = flat.get(idx) else {
                continue;
            };
            let is_root_bottom_anchor = node.parent == Some(0)
                && new_rect.y + new_rect.height
                    >= new_viewport.layout_h + new_viewport.offset_y - 96.0;
            let (transition_property, duration_ms, delay_ms, easing) =
                if let Some(transition) = &node.style.transition {
                    (
                        transition.property.clone(),
                        transition.duration_ms,
                        transition.delay_ms,
                        transition.easing,
                    )
                } else if viewport_changed && is_root_bottom_anchor {
                    // Visual Viewport resize is a UA interaction. Keep the
                    // bottom composer visually attached to the UIKit keyboard
                    // even when application CSS does not opt into transition.
                    (TransitionProperty::Transform, 280, 0, Easing::EaseOut)
                } else {
                    continue;
                };
            let belongs_to_sticky_subtree = std::iter::successors(Some(idx), |current| {
                flat.get(*current)
                    .and_then(|current_node| current_node.parent)
            })
            .any(|current| {
                flat.get(current).is_some_and(|current_node| {
                    matches!(current_node.style.position, Position::Sticky)
                })
            });
            // Compiled conditional branches remain sibling nodes and switch
            // `display`, so the entering branch has a different flat index.
            // Pair it with the nearest transitioned sibling that left layout;
            // this preserves the visual box across a Show/conditional swap.
            let old_entry = old_rects
                .get(&idx)
                .copied()
                .map(|rect| (rect, idx))
                .or_else(|| {
                    old_layout_cache
                        .iter()
                        .filter(|(_, old_idx)| !new_indices.contains(old_idx))
                        .filter(|(_, old_idx)| {
                            flat.get(*old_idx).is_some_and(|old_node| {
                                old_node.parent == node.parent
                                    && old_node.style.transition.is_some()
                            })
                        })
                        .min_by_key(|(_, old_idx)| old_idx.abs_diff(idx))
                        .map(|(rect, old_idx)| (*rect, *old_idx))
                });
            let Some((old_rect, old_idx)) = old_entry else {
                continue;
            };
            if old_rect.width <= 0.0
                || old_rect.height <= 0.0
                || new_rect.width <= 0.0
                || new_rect.height <= 0.0
            {
                continue;
            }
            let animates_height = matches!(transition_property, TransitionProperty::All)
                || matches!(
                    &transition_property,
                    TransitionProperty::Custom(property) if property.eq_ignore_ascii_case("height")
                );
            // `display:none` sibling replacement is discrete in Blink. Pairing
            // two Show branches inside a sticky container makes the entering
            // child paint with the leaving branch's 55vh height while its
            // parent already owns the compact final layout, exposing a large
            // blank panel. Same-node height transitions remain supported.
            let can_animate_height = old_idx == idx || !belongs_to_sticky_subtree;
            if can_animate_height
                && animates_height
                && (old_rect.height - new_rect.height).abs() >= 0.5
            {
                let target_id = if old_idx == idx {
                    node.stable_id
                } else {
                    let parent_id = node
                        .parent
                        .and_then(|parent| flat.get(parent))
                        .map(|parent| parent.stable_id)
                        .unwrap_or(0);
                    let old_id = flat
                        .get(old_idx)
                        .map(|old_node| old_node.stable_id)
                        .unwrap_or(old_idx as u64);
                    transition_pair_id(parent_id, old_id, node.stable_id)
                };
                let from = animations
                    .iter()
                    .rev()
                    .find(|animation| {
                        animation.target_id() == target_id
                            && animation.property() == AnimatedProperty::LayoutHeight
                    })
                    .and_then(|animation| animation.sampled_height(now))
                    .unwrap_or(old_rect.height);
                animations.retain(|animation| {
                    animation.target_id() != target_id
                        || animation.property() != AnimatedProperty::LayoutHeight
                });
                animations.push(ActiveAnimation::LayoutHeight {
                    target_id,
                    node_index: idx,
                    from,
                    to: new_rect.height,
                    start: now,
                    duration_ms: duration_ms as f64,
                    delay_ms: delay_ms as f64,
                    easing,
                });
            }
            if !matches!(
                transition_property,
                TransitionProperty::All | TransitionProperty::Transform
            ) {
                continue;
            }
            if viewport_changed && belongs_to_sticky_subtree {
                // Sticky geometry is resolved against the scrollport during
                // painting. Applying a flow-space FLIP translation on top of
                // that resolved position makes sticky content drift while the
                // software keyboard resizes the viewport.
                animations.retain(|animation| {
                    animation.target_id() != node.stable_id
                        || animation.property() != AnimatedProperty::Transform
                });
                continue;
            }
            let delta_x = old_rect.x - new_rect.x;
            let mut delta_y = old_rect.y - new_rect.y;
            let viewport_height_delta = old_viewport.layout_h - new_viewport.layout_h;
            let is_bottom_anchored = new_rect.y + new_rect.height
                >= new_viewport.layout_h + new_viewport.offset_y - 96.0;
            if delta_y.abs() < 0.5 && viewport_height_delta.abs() >= 0.5 && is_bottom_anchored {
                delta_y = viewport_height_delta;
            }
            if delta_x.abs() < 0.5 && delta_y.abs() < 0.5 {
                continue;
            }
            let mut from = node.style.transform;
            from.translate_x += delta_x;
            from.translate_y += delta_y;
            let target_id = node.stable_id;
            let from = animations
                .iter()
                .rev()
                .find(|animation| {
                    animation.target_id() == target_id
                        && animation.property() == AnimatedProperty::Transform
                })
                .and_then(|animation| animation.sampled_transform(now))
                .unwrap_or(from);
            animations.retain(|animation| {
                animation.target_id() != target_id
                    || animation.property() != AnimatedProperty::Transform
            });
            animations.push(ActiveAnimation::Transform {
                target_id,
                node_index: idx,
                from,
                to: node.style.transform,
                start: now,
                duration_ms: duration_ms as f64,
                delay_ms: delay_ms as f64,
                easing,
            });
        }
        if std::env::var_os("W3COS_INPUT_TRACE").is_some()
            && (old_viewport.layout_h - new_viewport.layout_h).abs() >= 0.5
        {
            eprintln!(
                "[W3C OS][IME] layout transition viewport {:.1}->{:.1} animations={}",
                old_viewport.layout_h,
                new_viewport.layout_h,
                animations.len().saturating_sub(before_count)
            );
        }
    }

    /// Scroll focused `TextInput` into view (HTML `scrollIntoView` semantics).
    fn ensure_focused_input_visible(&mut self) {
        let focus_idx = match self.focused_index {
            Some(i) => i,
            None => return,
        };
        if !matches!(
            self.get_kind_at(focus_idx),
            Some(ComponentKind::TextInput { .. })
        ) {
            return;
        }
        let focus_rect = match self
            .layout_cache
            .iter()
            .find(|(_, idx)| *idx == focus_idx)
            .map(|(r, _)| *r)
        {
            Some(r) => r,
            None => return,
        };
        let scroll_idx = self.scroll_ancestor.get(focus_idx).copied().flatten();
        let scroll_idx = match scroll_idx {
            Some(i) => i,
            None => return,
        };
        let (scroll_rect, extent) = match self
            .scrollable_nodes
            .iter()
            .find(|(i, _, _)| *i == scroll_idx)
            .map(|(_, r, e)| (*r, *e))
        {
            Some(v) => v,
            None => return,
        };

        let (ox, oy) = self
            .scroll_offsets
            .get(&scroll_idx)
            .copied()
            .unwrap_or((0.0, 0.0));
        let mut new_oy = oy;
        let margin = 12.0;
        let focus_bottom = focus_rect.y + focus_rect.height;
        let visible_bottom = scroll_rect.y + scroll_rect.height;
        if focus_bottom > visible_bottom - margin {
            new_oy = (new_oy + (focus_bottom - visible_bottom + margin)).min(extent.max_y);
        }
        if focus_rect.y < scroll_rect.y + margin {
            new_oy = (new_oy + (focus_rect.y - scroll_rect.y - margin)).max(0.0);
        }
        if (new_oy - oy).abs() > 0.001 {
            self.scroll_offsets.insert(scroll_idx, (ox, new_oy));
            self.queue_scroll_damage(scroll_idx, new_oy - oy);
        }
    }

    fn poll_viewport_inset(&mut self) -> bool {
        let window = match self.get_window() {
            Some(w) => w,
            None => return false,
        };
        let scale = self.scale_factor as f32;
        let inset_top = if w3cos_std::safe_area::is_enabled() {
            0.0
        } else {
            self.content_inset_top
        };
        let viewport = ViewportLayout::from_window(
            &window,
            scale,
            inset_top,
            ViewportLayout::ime_open_for_app(self),
        );
        if viewport != self.viewport {
            // Commit the new viewport inside `ensure_layout`. Keeping the
            // previous value here is required for FLIP transitions to retain
            // both ends of an IME-driven viewport resize.
            self.needs_layout = true;
            self.request_repaint();
            true
        } else {
            false
        }
    }

    #[cfg(target_os = "ios")]
    fn poll_native_text_input(&mut self) -> bool {
        let Some(focus_idx) = self.focused_index else {
            return false;
        };
        if !matches!(
            self.get_kind_at(focus_idx),
            Some(ComponentKind::TextInput { .. })
        ) {
            return false;
        }
        let state = {
            let Some(window) = self.get_window() else {
                return false;
            };
            crate::ios_input::text_input_state(window)
        };
        let Some(state) = state else {
            return false;
        };
        let current = self
            .text_input_values
            .entry(focus_idx)
            .or_insert_with(String::new);
        if *current == state.text {
            return false;
        }
        *current = state.text;
        if std::env::var_os("W3COS_INPUT_TRACE").is_some() {
            eprintln!(
                "[W3C OS][IME] native text changed composing={}",
                state.is_composing
            );
        }
        // Marked text participates in painting immediately so Pinyin is
        // visible while UIKit still owns candidate selection. DOM input/
        // composition events remain separated at the adapter boundary.
        self.request_repaint();
        true
    }

    // -----------------------------------------------------------------------
    // GPU paint — zero-copy via style overrides (no root.clone())
    // -----------------------------------------------------------------------
    #[cfg(feature = "gpu")]
    fn paint_gpu(&mut self) {
        crate::perf::begin_frame();
        self.sync_gpu_surface_to_window();
        let layout_started = Instant::now();
        self.ensure_layout();
        crate::perf::record_layout(layout_started.elapsed());
        let paint_started = Instant::now();

        let (dev_id, width, height) = match &self.gpu_state {
            GpuState::Active { surface, .. } => {
                (surface.dev_id, surface.config.width, surface.config.height)
            }
            _ => return,
        };
        if width == 0 || height == 0 {
            return;
        }

        let now = Instant::now();

        let flat = self.retained_paint_nodes.as_slice();
        // CSS canvas background propagation: the platform surface continues
        // behind translucent system UI such as the rounded iOS keyboard. Use
        // the root (or its document-element child) instead of exposing the
        // renderer's historical dark debug clear color.
        let canvas_background = flat
            .first()
            .filter(|node| node.style.background.a > 0)
            .or_else(|| {
                flat.iter()
                    .find(|node| node.parent == Some(0) && node.style.background.a > 0)
            })
            .map(|node| node.style.background)
            .unwrap_or(Color::WHITE);

        // A CSS transform establishes a transformed subtree, so animated
        // translation must affect descendants as well as the panel box.
        let mut style_overrides = animated_style_overrides(flat, &self.animations, now);

        if let Some(hover_idx) = self.hovered_index {
            if hover_idx < flat.len() {
                let entry = style_overrides
                    .entry(hover_idx)
                    .or_insert_with(|| flat[hover_idx].style.clone());
                if self.pressed_index == Some(hover_idx) {
                    entry.opacity = 0.6;
                } else if entry.background.a > 0 {
                    entry.background.r = entry.background.r.saturating_add(25);
                    entry.background.g = entry.background.g.saturating_add(25);
                    entry.background.b = entry.background.b.saturating_add(25);
                }
            }
        }

        let animated_layout = animated_layout_cache(&self.layout_cache, &self.animations, now);
        let layout_cache = animated_layout
            .as_deref()
            .unwrap_or(self.layout_cache.as_slice());
        let animated_clips = animated_clip_nodes(&self.clip_only_nodes, &self.animations, now);
        let clip_only_nodes = animated_clips
            .as_deref()
            .unwrap_or(self.clip_only_nodes.as_slice());

        let scroll_info = build_scroll_info_fast(
            &self.scroll_ancestor,
            &self.scrollable_nodes,
            clip_only_nodes,
            &self.scroll_offsets,
            &self.overscroll_states,
            layout_cache,
            flat,
            Some((&self.retained_sticky_owner, &self.retained_rect_by_index)),
            self.viewport.layout_w,
            self.viewport.layout_h,
        );
        // Blink computes cull rects during PrePaint. Do the equivalent before
        // building Vello display items so offscreen list nodes never enter the
        // scene or its z-sort. Keep a small overscan for shadows/transforms.
        let mut render_nodes: Vec<(usize, LayoutRect, &ComponentKind, &w3cos_std::style::Style)> =
            layout_cache
                .iter()
                .filter_map(|&(rect, idx)| {
                    if !node_intersects_paint_cull(
                        idx,
                        rect,
                        &scroll_info,
                        self.viewport.layout_w,
                        self.viewport.layout_h,
                        64.0,
                    ) {
                        return None;
                    }
                    let node = flat.get(idx)?;
                    let style = style_overrides.get(&idx).unwrap_or(&node.style);
                    Some((idx, rect, &node.kind, style))
                })
                .collect();
        let paint_z = &self.retained_paint_z;
        render_nodes.sort_by_key(|(idx, _, _, _)| paint_z[*idx]);

        let scale = self.scale_factor as f32;

        let device_handle = &self.render_cx.devices[dev_id];
        if self.gpu_filter_pipelines.is_none() {
            self.gpu_filter_pipelines = Some(crate::gpu_filter::GpuFilterPipelines::new(
                &device_handle.device,
            ));
        }

        self.scene.reset();
        {
            let pipelines = self.gpu_filter_pipelines.as_mut().unwrap();
            let layer_pool = &mut self.gpu_layer_textures;
            let output_pool = &mut self.gpu_output_texture_pool;
            let renderer = self
                .renderers
                .get_mut(dev_id)
                .and_then(|r| r.as_mut())
                .expect("gpu renderer");
            let mut filter_ctx = crate::gpu_filter::GpuFilterCtx {
                device: &device_handle.device,
                queue: &device_handle.queue,
                renderer,
                pipelines,
                layer_pool,
                output_pool,
                scale_factor: scale,
            };
            render_gpu::render_frame(
                &mut self.scene,
                width,
                height,
                &render_nodes,
                &self.font_data,
                &self.font,
                &scroll_info,
                &self.text_input_values,
                self.focused_index,
                &mut self.glyph_cache,
                scale,
                Some(&mut filter_ctx),
            );
        }

        // Draw hover outline (logical pixels — render function handles DPI)
        if let Some(hover_idx) = self.hovered_index {
            if let Some(hit) = self
                .hit_nodes
                .iter()
                .find(|h| h.index == hover_idx && h.is_interactive)
            {
                render_gpu::draw_hover_outline(&mut self.scene, hit.rect, scale);
            }
        }

        // Draw focus ring for focused buttons
        if let Some(focus_idx) = self.focused_index {
            if self.hovered_index != Some(focus_idx) {
                if let Some(node) = flat.get(focus_idx) {
                    if matches!(&node.kind, ComponentKind::Button { .. }) {
                        if let Some(hit) = self
                            .hit_nodes
                            .iter()
                            .find(|h| h.index == focus_idx && h.is_focusable)
                        {
                            render_gpu::draw_focus_ring(&mut self.scene, hit.rect, scale);
                        }
                    }
                }
            }
        }

        drop(render_nodes);
        drop(style_overrides);

        // Cleanup animations
        self.animations.retain(|a| !a.is_complete(now));
        self.last_frame_time = Some(now);
        self.paint_generation += 1;

        if !self.animations.is_empty() {
            self.request_repaint();
        }

        // GPU submit
        let GpuState::Active { surface, .. } = &self.gpu_state else {
            return;
        };

        let device_handle = &self.render_cx.devices[dev_id];
        if let Some(renderer) = self.renderers.get_mut(dev_id).and_then(|r| r.as_mut()) {
            let render_result = renderer.render_to_texture(
                &device_handle.device,
                &device_handle.queue,
                &self.scene,
                &surface.target_view,
                &vello::RenderParams {
                    base_color: vello::peniko::Color::new([
                        canvas_background.r as f32 / 255.0,
                        canvas_background.g as f32 / 255.0,
                        canvas_background.b as f32 / 255.0,
                        canvas_background.a as f32 / 255.0,
                    ]),
                    width,
                    height,
                    antialiasing_method: gpu_aa_config(),
                },
            );
            if let Err(e) = render_result {
                self.gpu_output_texture_pool.end_frame(renderer);
                eprintln!("[W3C OS] GPU render error: {e}");
                return;
            }

            self.gpu_output_texture_pool.end_frame(renderer);

            let surface_texture = match surface.surface.get_current_texture() {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("[W3C OS] Failed to get surface texture: {e}");
                    return;
                }
            };
            let mut encoder =
                device_handle
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("Surface Blit"),
                    });
            surface.blitter.copy(
                &device_handle.device,
                &mut encoder,
                &surface.target_view,
                &surface_texture
                    .texture
                    .create_view(&wgpu::TextureViewDescriptor::default()),
            );
            device_handle.queue.submit([encoder.finish()]);
            surface_texture.present();
            let _ = device_handle.device.poll(wgpu::PollType::Poll);
        }
        crate::perf::record_paint(paint_started.elapsed());
    }

    /// iOS can resize the native `UIWindow` after winit has created the Metal
    /// surface without delivering a later `Resized` event. Keep the drawable
    /// and layout viewport sourced from the same current window bounds before
    /// every frame so a launch-time size (for example 375 pt) cannot clip a
    /// wider runtime window (for example 402 pt).
    #[cfg(feature = "gpu")]
    fn sync_gpu_surface_to_window(&mut self) {
        let (window, configured_width, configured_height) = match &self.gpu_state {
            GpuState::Active { surface, window } => {
                (window.clone(), surface.config.width, surface.config.height)
            }
            _ => return,
        };
        let size = window_backing_size(&window);
        if size.width == 0 || size.height == 0 {
            return;
        }

        self.scale_factor = window.scale_factor();
        if size.width == configured_width && size.height == configured_height {
            return;
        }

        if let GpuState::Active { surface, .. } = &mut self.gpu_state {
            self.render_cx
                .resize_surface(surface, size.width, size.height);
        }
        self.needs_layout = true;
    }

    // -----------------------------------------------------------------------
    // CPU paint — same zero-copy pattern
    // -----------------------------------------------------------------------
    #[cfg(feature = "cpu-render")]
    fn paint_cpu(&mut self) {
        crate::perf::begin_frame();
        let layout_started = Instant::now();
        self.ensure_layout();
        crate::perf::record_layout(layout_started.elapsed());
        let paint_started = Instant::now();

        let Some(cpu_ref) = self.cpu.as_ref() else {
            return;
        };
        let window = cpu_ref.window.clone();
        let size = window_backing_size(&window);
        let (w, h) = (size.width, size.height);
        let scale = self.scale_factor as f32;
        if w == 0 || h == 0 {
            return;
        }

        let mut pixmap = match self.cpu.as_mut().unwrap().framebuffer.take() {
            Some(existing) if existing.width() == w && existing.height() == h => existing,
            _ => match Pixmap::new(w, h) {
                Some(p) => p,
                None => return,
            },
        };

        let now = Instant::now();
        let flat = layout::pre_flatten(&self.root);

        let mut style_overrides = animated_style_overrides(&flat, &self.animations, now);
        if let Some(hover_idx) = self.hovered_index {
            if hover_idx < flat.len() {
                let entry = style_overrides
                    .entry(hover_idx)
                    .or_insert_with(|| flat[hover_idx].style.clone());
                if self.pressed_index == Some(hover_idx) {
                    entry.opacity = 0.6;
                } else if entry.background.a > 0 {
                    entry.background.r = entry.background.r.saturating_add(25);
                    entry.background.g = entry.background.g.saturating_add(25);
                    entry.background.b = entry.background.b.saturating_add(25);
                }
            }
        }

        let animated_layout = animated_layout_cache(&self.layout_cache, &self.animations, now);
        let layout_cache = animated_layout
            .as_deref()
            .unwrap_or(self.layout_cache.as_slice());
        let animated_clips = animated_clip_nodes(&self.clip_only_nodes, &self.animations, now);
        let clip_only_nodes = animated_clips
            .as_deref()
            .unwrap_or(self.clip_only_nodes.as_slice());

        let scroll_info_raw = build_scroll_info_fast(
            &self.scroll_ancestor,
            &self.scrollable_nodes,
            clip_only_nodes,
            &self.scroll_offsets,
            &self.overscroll_states,
            layout_cache,
            &flat,
            None,
            self.viewport.layout_w,
            self.viewport.layout_h,
        );

        // Cull before cloning/scaling styles. The previous raster path skipped
        // offscreen nodes only after allocating a Style and render tuple for
        // every node, which made long lists O(total nodes) in expensive work.
        let visible_layout: Vec<(LayoutRect, usize)> = layout_cache
            .iter()
            .copied()
            .filter(|&(rect, idx)| {
                node_intersects_paint_cull(
                    idx,
                    rect,
                    &scroll_info_raw,
                    self.viewport.layout_w,
                    self.viewport.layout_h,
                    64.0,
                )
            })
            .collect();

        // Scale visible layout rects (logical) → physical Pixmap pixels and
        // clone only the styles that can contribute to this frame.
        let scaled_styles: Vec<w3cos_std::style::Style> = visible_layout
            .iter()
            .filter_map(|&(_, idx)| {
                let node = flat.get(idx)?;
                let base = style_overrides.get(&idx).unwrap_or(node.style);
                let mut s = base.clone();
                s.font_size *= scale;
                s.border_radius *= scale;
                s.border_width *= scale;
                Some(s)
            })
            .collect();

        let mut render_nodes: Vec<(usize, LayoutRect, &ComponentKind, &w3cos_std::style::Style)> =
            visible_layout
                .iter()
                .enumerate()
                .filter_map(|(i, &(rect, idx))| {
                    let node = flat.get(idx)?;
                    let scaled_rect = LayoutRect {
                        x: rect.x * scale,
                        y: rect.y * scale,
                        width: rect.width * scale,
                        height: rect.height * scale,
                    };
                    Some((idx, scaled_rect, node.kind, scaled_styles.get(i)?))
                })
                .collect();
        let mut paint_z = vec![0; flat.len()];
        for (idx, node) in flat.iter().enumerate() {
            let inherited = node.parent.map(|parent| paint_z[parent]).unwrap_or(0);
            paint_z[idx] = if node.style.z_index == 0 {
                inherited
            } else {
                node.style.z_index
            };
        }
        render_nodes.sort_by_key(|(idx, _, _, _)| paint_z[*idx]);

        let scroll_info: Vec<Option<(f32, f32, LayoutRect)>> = scroll_info_raw
            .iter()
            .map(|si| {
                si.map(|(sx, sy, clip)| {
                    (
                        sx * scale,
                        sy * scale,
                        LayoutRect {
                            x: clip.x * scale,
                            y: clip.y * scale,
                            width: clip.width * scale,
                            height: clip.height * scale,
                        },
                    )
                })
            })
            .collect();

        let scaled_scrollable: Vec<(usize, LayoutRect, ScrollExtent)> = self
            .scrollable_nodes
            .iter()
            .map(|(i, r, e)| {
                (
                    *i,
                    LayoutRect {
                        x: r.x * scale,
                        y: r.y * scale,
                        width: r.width * scale,
                        height: r.height * scale,
                    },
                    *e,
                )
            })
            .collect();
        match std::mem::take(&mut self.repaint_mode) {
            RepaintMode::ScrollOnly(damages) => {
                let scaled_damages: Vec<(usize, f32)> = damages
                    .iter()
                    .map(|damage| (damage.index, damage.delta_y * scale))
                    .collect();
                let painted_rects: Vec<(usize, LayoutRect)> = render_nodes
                    .iter()
                    .map(|(idx, rect, _, _)| (*idx, *rect))
                    .collect();
                if scroll_damage_crosses_stacking_context(
                    &damages,
                    &paint_z,
                    &scaled_scrollable,
                    &painted_rects,
                ) {
                    // Raster-copying moves already-composited pixels. Inside
                    // an overlay stacking context this can copy the page below
                    // through translucent list gaps. CPU layers do not yet own
                    // independent backing stores, so repaint the composed
                    // overlay frame for correctness.
                    render_cpu::render_frame(
                        &mut pixmap,
                        &render_nodes,
                        &self.font,
                        &scroll_info,
                        &self.text_input_values,
                        self.focused_index,
                        &mut self.cpu.as_mut().unwrap().clip_masks,
                    );
                } else {
                    render_cpu::render_scroll_damage(
                        &mut pixmap,
                        &render_nodes,
                        &self.font,
                        &scroll_info,
                        &self.text_input_values,
                        self.focused_index,
                        &scaled_damages,
                        &scaled_scrollable,
                        &self.scroll_ancestor,
                        &mut self.cpu.as_mut().unwrap().clip_masks,
                    );
                }
            }
            RepaintMode::Full => {
                render_cpu::render_frame(
                    &mut pixmap,
                    &render_nodes,
                    &self.font,
                    &scroll_info,
                    &self.text_input_values,
                    self.focused_index,
                    &mut self.cpu.as_mut().unwrap().clip_masks,
                );
            }
            RepaintMode::Clean => {}
        }
        if let Some(hover_idx) = self.hovered_index {
            if let Some(hit) = self
                .hit_nodes
                .iter()
                .find(|h| h.index == hover_idx && h.is_interactive)
            {
                draw_hover_outline_cpu(&mut pixmap, hit.rect);
            }
        }
        if let Some(focus_idx) = self.focused_index {
            if self.hovered_index != Some(focus_idx) {
                if let Some(node) = flat.get(focus_idx) {
                    if matches!(node.kind, ComponentKind::Button { .. }) {
                        if let Some(hit) = self
                            .hit_nodes
                            .iter()
                            .find(|h| h.index == focus_idx && h.is_focusable)
                        {
                            draw_focus_ring_cpu(&mut pixmap, hit.rect);
                        }
                    }
                }
            }
        }

        drop(render_nodes);
        drop(style_overrides);
        drop(flat);

        self.animations.retain(|a| !a.is_complete(now));
        self.last_frame_time = Some(now);
        self.paint_generation += 1;

        let needs_anim_repaint = !self.animations.is_empty();

        #[cfg(any(feature = "devtools", feature = "ai-bridge"))]
        {
            crate::frame_cache::store(w, h, pixmap.data().to_vec());
        }

        if let Some(cpu) = self.cpu.as_mut() {
            cpu.present(&pixmap, w, h);
            cpu.framebuffer = Some(pixmap);
        }

        if needs_anim_repaint {
            self.request_repaint();
        }
        crate::perf::record_paint(paint_started.elapsed());
    }

    fn set_pointer_logical(&mut self, physical_x: f64, physical_y: f64) {
        let scale = self.scale_factor as f32;
        self.mouse_x = physical_x as f32 / scale;
        self.mouse_y = physical_y as f32 / scale;
    }

    fn update_hover_at_pointer(&mut self) {
        #[cfg(any(target_os = "ios", target_os = "android"))]
        {
            // Touch-only mobile surfaces do not have a persistent hover
            // state. Some simulator/device event bridges still emit
            // CursorMoved after a touch, which previously left the desktop
            // debug hover rectangle painted over text until the next frame.
            if self.hovered_index.take().is_some() {
                self.request_repaint();
            }
        }

        #[cfg(not(any(target_os = "ios", target_os = "android")))]
        {
            self.ensure_layout();
            let new_hover = self.hit_test(self.mouse_x, self.mouse_y);
            if new_hover != self.hovered_index {
                self.hovered_index = new_hover;
                if let Some(window) = self.get_window() {
                    if new_hover.is_some() {
                        window.set_cursor(winit::window::Cursor::Icon(
                            winit::window::CursorIcon::Pointer,
                        ));
                    } else {
                        window.set_cursor(winit::window::Cursor::Icon(
                            winit::window::CursorIcon::Default,
                        ));
                    }
                }
                self.request_repaint();
            }
        }
    }

    fn pointer_pressed(&mut self) {
        self.ensure_layout();
        let hit = self.hit_test(self.mouse_x, self.mouse_y);
        if std::env::var_os("W3COS_INPUT_TRACE").is_some() {
            eprintln!(
                "[W3C OS][TOUCH] down x={:.1} y={:.1} hit={hit:?} focused={:?}",
                self.mouse_x, self.mouse_y, self.focused_index
            );
        }
        crate::uitest::set_pointer_hit(self.mouse_x, self.mouse_y, hit);
        if let Some(idx) = hit {
            self.pressed_index = Some(idx);
            #[cfg(target_os = "ios")]
            if matches!(self.get_kind_at(idx), Some(ComponentKind::TextInput { .. })) {
                // Keep keyboard activation inside the native touch-down user
                // gesture. Small real-finger movement can promote the gesture
                // to scrolling before touch-up and would otherwise drop focus.
                self.focus_text_input(idx);
            }
            #[cfg(not(any(target_os = "ios", target_os = "android")))]
            self.request_repaint();
        } else {
            self.focused_index = None;
            crate::uitest::set_focused_index(self.focused_index);
            #[cfg(any(target_os = "ios", target_os = "android"))]
            self.sync_soft_keyboard();
            #[cfg(not(any(target_os = "ios", target_os = "android")))]
            self.request_repaint();
        }
    }

    fn pointer_released(&mut self) {
        if std::env::var_os("W3COS_INPUT_TRACE").is_some() {
            eprintln!(
                "[W3C OS][TOUCH] up x={:.1} y={:.1} pressed={:?} scrolled={}",
                self.mouse_x, self.mouse_y, self.pressed_index, self.touch_scroll_active
            );
        }
        if let Some(pressed_idx) = self.pressed_index.take() {
            #[cfg(any(target_os = "ios", target_os = "android"))]
            {
                // Mobile touch end coordinates can be rounded or shifted by the
                // platform compositor. A gesture that was not promoted to scroll
                // should activate the target captured on touch start.
                self.handle_click(pressed_idx);
                return;
            }
            #[cfg(not(any(target_os = "ios", target_os = "android")))]
            {
                let current_hover = self.hit_test(self.mouse_x, self.mouse_y);
                if current_hover == Some(pressed_idx) {
                    self.handle_click(pressed_idx);
                } else {
                    self.repaint_after_interaction();
                }
            }
        }
    }

    fn hit_test(&self, x: f32, y: f32) -> Option<usize> {
        let flat = layout::pre_flatten(&self.root);
        if flat
            .iter()
            .any(|node| matches!(node.style.position, w3cos_std::style::Position::Sticky))
        {
            let scroll_info = build_scroll_info_fast(
                &self.scroll_ancestor,
                &self.scrollable_nodes,
                &self.clip_only_nodes,
                &self.scroll_offsets,
                &self.overscroll_states,
                &self.layout_cache,
                &flat,
                None,
                self.viewport.layout_w,
                self.viewport.layout_h,
            );
            // Hit testing must follow the same stacking order as painting.
            // A sticky subtree can be earlier in DOM order while its z-index
            // deliberately places it above later conversation content.
            let mut hit_order: Vec<&HitNode> = self.hit_nodes.iter().collect();
            let mut paint_z = vec![0; flat.len()];
            for (idx, node) in flat.iter().enumerate() {
                let inherited = node.parent.map(|parent| paint_z[parent]).unwrap_or(0);
                paint_z[idx] = if node.style.z_index == 0 {
                    inherited
                } else {
                    node.style.z_index
                };
            }
            hit_order.sort_by_key(|hit| paint_z[hit.index]);
            for hit in hit_order.into_iter().rev() {
                let (rect, clip) = match scroll_info.get(hit.index).copied().flatten() {
                    Some((sx, sy, clip)) => (
                        LayoutRect {
                            x: hit.rect.x - sx,
                            y: hit.rect.y - sy,
                            ..hit.rect
                        },
                        Some(clip),
                    ),
                    None => (hit.rect, None),
                };
                let inside_clip = clip.is_none_or(|clip| {
                    x >= clip.x
                        && x <= clip.x + clip.width
                        && y >= clip.y
                        && y <= clip.y + clip.height
                });
                if inside_clip
                    && x >= rect.x
                    && x <= rect.x + rect.width
                    && y >= rect.y
                    && y <= rect.y + rect.height
                {
                    let mut current = Some(hit.index);
                    while let Some(idx) = current {
                        if self
                            .hit_nodes
                            .iter()
                            .find(|candidate| candidate.index == idx)
                            .is_some_and(|candidate| candidate.is_interactive)
                        {
                            return Some(idx);
                        }
                        current = self.flat_parents.get(idx).copied().flatten();
                    }
                }
            }
        }

        let direct = self
            .spatial_grid
            .query(x, y, &self.hit_nodes, &self.flat_parents);
        if direct.is_some_and(|idx| {
            self.scroll_ancestor
                .get(idx)
                .and_then(|ancestor| *ancestor)
                .is_none_or(|ancestor| {
                    let (sx, sy) = self
                        .scroll_offsets
                        .get(&ancestor)
                        .copied()
                        .unwrap_or((0.0, 0.0));
                    sx.abs() <= 0.001
                        && sy.abs() <= 0.001
                        && overscroll_displacement_y(&self.overscroll_states, ancestor).abs()
                            <= 0.001
                })
        }) {
            return direct;
        }

        #[cfg(any(target_os = "ios", target_os = "android"))]
        if direct.is_none() {
            const TEXT_INPUT_HIT_SLOP: f32 = 24.0;
            if let Some(hit) = self.hit_nodes.iter().rev().find(|hit| {
                matches!(
                    self.get_kind_at(hit.index),
                    Some(ComponentKind::TextInput { .. })
                ) && x >= hit.rect.x - TEXT_INPUT_HIT_SLOP
                    && x <= hit.rect.x + hit.rect.width + TEXT_INPUT_HIT_SLOP
                    && y >= hit.rect.y - TEXT_INPUT_HIT_SLOP
                    && y <= hit.rect.y + hit.rect.height + TEXT_INPUT_HIT_SLOP
            }) {
                return Some(hit.index);
            }
        }

        let (lx, ly) = self.viewport_to_layout(x, y);
        self.spatial_grid
            .query(lx, ly, &self.hit_nodes, &self.flat_parents)
            .or(direct)
    }

    fn viewport_to_layout(&self, x: f32, y: f32) -> (f32, f32) {
        for (idx, rect, _) in self.scrollable_nodes.iter().rev() {
            let (sx, sy) = self.scroll_offsets.get(idx).copied().unwrap_or((0.0, 0.0));
            let visual_sy = sy - overscroll_displacement_y(&self.overscroll_states, *idx);
            // The scroll viewport is stationary; only its contents move. Using
            // an offset viewport makes adjacent fixed controls (for example a
            // composer below a feed) hit-test as scrolled content.
            if x >= rect.x && x <= rect.x + rect.width && y >= rect.y && y <= rect.y + rect.height {
                return (x + sx, y + visual_sy);
            }
        }
        (x, y)
    }

    fn hit_test_scroll(&self, x: f32, y: f32) -> Option<usize> {
        let flat = layout::pre_flatten(&self.root);
        let scroll_info = build_scroll_info_fast(
            &self.scroll_ancestor,
            &self.scrollable_nodes,
            &self.clip_only_nodes,
            &self.scroll_offsets,
            &self.overscroll_states,
            &self.layout_cache,
            &flat,
            None,
            self.viewport.layout_w,
            self.viewport.layout_h,
        );
        let mut paint_z = vec![0; flat.len()];
        for (idx, node) in flat.iter().enumerate() {
            let inherited = node.parent.map(|parent| paint_z[parent]).unwrap_or(0);
            paint_z[idx] = if node.style.z_index == 0 {
                inherited
            } else {
                node.style.z_index
            };
        }
        let overlay_blockers: Vec<(usize, LayoutRect)> = self
            .layout_cache
            .iter()
            .filter_map(|&(rect, idx)| {
                let node = flat.get(idx)?;
                matches!(
                    node.style.position,
                    w3cos_std::style::Position::Absolute | w3cos_std::style::Position::Fixed
                )
                .then_some((idx, rect))
            })
            .collect();
        topmost_scroll_node_at(
            x,
            y,
            &self.scrollable_nodes,
            &scroll_info,
            &paint_z,
            &overlay_blockers,
        )
    }

    fn scroll_at_pointer(&mut self, dy: f32) {
        if dy == 0.0 {
            return;
        }
        self.ensure_layout();
        let Some(idx) = self.hit_test_scroll(self.mouse_x, self.mouse_y) else {
            return;
        };
        self.scroll_node_by(idx, dy);
        self.flush_pending_sticky_counters();
    }

    fn scroll_node_by(&mut self, idx: usize, dy: f32) -> f32 {
        let Some(max_y) = self
            .scrollable_nodes
            .iter()
            .find(|(i, _, _)| *i == idx)
            .map(|(_, _, extent)| extent.max_y)
        else {
            return 0.0;
        };
        let (ox, stored_oy) = self.scroll_offsets.get(&idx).copied().unwrap_or((0.0, 0.0));
        let (oy, new_oy, applied) = bounded_scroll_step(stored_oy, dy, max_y);
        if (stored_oy - oy).abs() > 0.001 {
            self.scroll_offsets.insert(idx, (ox, oy));
            crate::uitest::set_scroll_offset(idx, oy);
        }
        if applied.abs() > 0.001 {
            self.user_scrolled_nodes.insert(idx);
            self.scroll_offsets.insert(idx, (ox, new_oy));
            crate::uitest::set_scroll_offset(idx, new_oy);
            let native_scroll =
                layout::pre_flatten(&self.root)
                    .get(idx)
                    .and_then(|node| match node.on_click {
                        EventAction::NativeScroll(id) => Some(*id),
                        _ => None,
                    });
            if let Some(host_id) = native_scroll {
                w3cos_react_compat::aot::dispatch_scroll(host_id, new_oy);
                self.rebuild_if_dirty();
            }
            if let Some(ordinal) = self.virtual_scroll_indices.get(&idx).copied() {
                let viewport_extent = self
                    .scrollable_nodes
                    .iter()
                    .find(|(index, _, _)| *index == idx)
                    .map(|(_, rect, _)| rect.height)
                    .unwrap_or(self.viewport.layout_h);
                self.materialize_virtual_list(ordinal, viewport_extent, new_oy);
                self.needs_layout = true;
                self.needs_tree_rebuild = true;
                self.repaint_mode = RepaintMode::Full;
            } else {
                self.queue_scroll_damage(idx, applied);
            }
            if self.sticky_marker_index.contains_key(&idx) {
                self.pending_sticky_scrolls.insert(idx);
            }
            self.request_repaint();
        }
        applied
    }

    fn overscroll_behavior(&self, idx: usize) -> OverscrollBehavior {
        self.retained_paint_nodes
            .get(idx)
            .map(|node| node.style.overscroll_behavior)
            .unwrap_or_default()
    }

    fn scrollport_height(&self, idx: usize) -> f32 {
        self.scrollable_nodes
            .iter()
            .find(|(node_idx, _, _)| *node_idx == idx)
            .map(|(_, rect, _)| rect.height)
            .unwrap_or(self.viewport.layout_h)
    }

    /// Return the next user-scrollable container in the scroll chain.
    ///
    /// `scroll_ancestor` also contains `overflow: hidden` clip owners. Those
    /// establish a clip/scroll container for painting, but they cannot consume
    /// a direct-manipulation gesture. Handing boundary delta to one of them
    /// used to attach rubber-band state to an unpainted node and made both the
    /// bounce and the remaining momentum disappear.
    fn scroll_chain_parent(&self, idx: usize) -> Option<usize> {
        direct_scroll_chain_parent(idx, &self.scroll_ancestor, &self.scrollable_nodes)
    }

    /// Apply direct-manipulation scrolling with CSS scroll chaining. Any
    /// unconsumed boundary delta becomes a compositor-only rubber-band unless
    /// `overscroll-behavior: none` suppresses the local affordance.
    fn apply_touch_scroll(&mut self, start_idx: usize, dy: f32) -> usize {
        let mut remaining = if let Some(state) = self.overscroll_states.get_mut(&start_idx) {
            state.consume_restoring_drag(dy)
        } else {
            dy
        };
        if remaining.abs() <= SCROLL_CHAIN_EPSILON {
            self.repaint_mode = RepaintMode::Full;
            self.request_repaint();
            return start_idx;
        }

        let mut current = start_idx;
        loop {
            remaining -= self.scroll_node_by(current, remaining);
            if remaining.abs() <= SCROLL_CHAIN_EPSILON {
                return current;
            }

            let behavior = self.overscroll_behavior(current);
            if behavior == OverscrollBehavior::Auto
                && let Some(parent) = self.scroll_chain_parent(current)
            {
                current = parent;
                continue;
            }

            if behavior != OverscrollBehavior::None {
                let viewport_height = self.scrollport_height(current);
                let state = self.overscroll_states.entry(current).or_default();
                state.drag_past_boundary(remaining, viewport_height);
                if std::env::var_os("W3COS_INPUT_TRACE").is_some() {
                    eprintln!(
                        "[W3C OS][SCROLL] boundary index={} remaining={:.1} visual={:.1}",
                        current, remaining, state.displacement_y
                    );
                }
                self.last_overscroll_tick = None;
                self.repaint_mode = RepaintMode::Full;
                self.request_repaint();
            }
            return current;
        }
    }

    fn release_active_overscroll(&mut self, visual_velocity_y: f32) -> bool {
        let mut released = false;
        for state in self.overscroll_states.values_mut() {
            if state.displacement_y.abs() > 0.001 {
                state.release(visual_velocity_y);
                released = true;
            }
        }
        if released {
            self.last_overscroll_tick = Some(Instant::now());
            self.repaint_mode = RepaintMode::Full;
            self.request_repaint();
        }
        released
    }

    /// Advance inertial scrolling through the same CSS scroll chain as direct
    /// manipulation. When momentum reaches the terminal boundary, transfer
    /// its remaining velocity into the rubber-band spring instead of dropping
    /// the gesture abruptly.
    fn apply_kinetic_scroll(&mut self, start_idx: usize, dy: f32, velocity_y: f32) -> bool {
        let mut current = start_idx;
        let mut remaining = dy;
        loop {
            let requested = remaining;
            let applied = self.scroll_node_by(current, requested);
            remaining -= applied;
            let offset = self.scroll_offsets.get(&current).map(|(_, y)| *y);
            let extent = self
                .scrollable_nodes
                .iter()
                .find(|(idx, _, _)| *idx == current)
                .map(|(_, _, extent)| extent.max_y);
            crate::uitest::set_kinetic_attempt(current, extent, offset, applied);
            if remaining.abs() <= SCROLL_CHAIN_EPSILON {
                return true;
            }

            let behavior = self.overscroll_behavior(current);
            if behavior == OverscrollBehavior::Auto
                && let Some(parent) = self.scroll_chain_parent(current)
            {
                current = parent;
                continue;
            }

            if behavior != OverscrollBehavior::None {
                let viewport_height = self.scrollport_height(current);
                let state = self.overscroll_states.entry(current).or_default();
                state.drag_past_boundary(remaining, viewport_height);
                state.release(-velocity_y * 0.25);
                self.last_overscroll_tick = Some(Instant::now());
                self.repaint_mode = RepaintMode::Full;
                self.request_repaint();
            }
            return false;
        }
    }

    fn tick_overscroll(&mut self) {
        if self.overscroll_states.is_empty() || self.touch_scroll_active {
            return;
        }
        let now = Instant::now();
        let elapsed = self
            .last_overscroll_tick
            .replace(now)
            .map(|last| now.duration_since(last).as_secs_f32())
            .unwrap_or(0.0);
        if elapsed <= 0.0 {
            return;
        }
        self.overscroll_states
            .retain(|_, state| state.tick(elapsed));
        if self.overscroll_states.is_empty() {
            self.last_overscroll_tick = None;
        }
        self.repaint_mode = RepaintMode::Full;
        self.request_repaint();
    }

    fn update_sticky_counters(&mut self, scroll_idx: usize) {
        let Some((_, scroll_rect, _)) = self
            .scrollable_nodes
            .iter()
            .find(|(idx, _, _)| *idx == scroll_idx)
            .copied()
        else {
            return;
        };
        let scroll_y = self
            .scroll_offsets
            .get(&scroll_idx)
            .map(|(_, y)| *y)
            .unwrap_or(0.0);
        let counts = sticky_marker_counts(
            &self.sticky_marker_index,
            scroll_idx,
            scroll_rect.y,
            scroll_y,
        );
        let mut changed = false;
        for (signal, passed) in counts {
            let base = *self
                .sticky_counter_bases
                .entry(signal)
                .or_insert_with(|| state::get_signal(signal));
            let next = base.saturating_add(passed as i64);
            if state::get_signal(signal) != next {
                state::set_signal(signal, next);
                changed = true;
            }
        }
        if changed {
            self.rebuild_if_dirty();
        }
    }

    fn flush_pending_sticky_counters(&mut self) {
        let pending = std::mem::take(&mut self.pending_sticky_scrolls);
        for scroll_idx in pending {
            self.update_sticky_counters(scroll_idx);
        }
    }

    fn tick_kinetic_scroll(&mut self) {
        let Some(mut kinetic) = self.kinetic_scroll.take() else {
            return;
        };
        let now = Instant::now();
        let elapsed = now.duration_since(kinetic.started_at).as_secs_f32();
        let sample = kinetic.curve.sample(elapsed);
        let delta = sample.offset - kinetic.last_offset;
        if delta.abs() < 0.001 && sample.active {
            self.kinetic_scroll = Some(kinetic);
            return;
        }
        let continued = self.apply_kinetic_scroll(kinetic.index, delta, sample.velocity);
        kinetic.last_offset = sample.offset;
        let remains_active =
            continued && sample.active && sample.velocity.abs() >= KINETIC_SCROLL_STOP_VELOCITY;
        crate::uitest::set_kinetic_tick(
            remains_active,
            elapsed,
            delta,
            sample.velocity,
            sample.active,
            continued,
        );
        if remains_active {
            self.kinetic_scroll = Some(kinetic);
        } else {
            self.flush_pending_sticky_counters();
        }
    }

    fn queue_scroll_damage(&mut self, index: usize, delta_y: f32) {
        if let Some(damage) = self
            .recent_scroll_damage
            .iter_mut()
            .find(|damage| damage.index == index)
        {
            damage.delta_y += delta_y;
        } else {
            self.recent_scroll_damage
                .push(ScrollDamage { index, delta_y });
        }
        self.repaint_mode.queue_scroll_damage(index, delta_y);
    }

    fn sync_soft_keyboard(&mut self) {
        #[cfg(any(target_os = "android", target_os = "ios"))]
        {
            use winit::dpi::{PhysicalPosition, PhysicalSize};

            let Some(window) = self.get_window() else {
                return;
            };
            let Some(focus_idx) = self.focused_index else {
                #[cfg(target_os = "ios")]
                crate::ios_input::resign_text_input(window);
                window.set_ime_allowed(false);
                #[cfg(target_os = "ios")]
                {
                    self.ios_ime_retry = None;
                    self.ios_ime_viewport_poll = Some(IosImeRetry {
                        deadline: Instant::now()
                            + std::time::Duration::from_millis(IOS_IME_RETRY_INTERVAL_MS),
                        attempts: 0,
                    });
                }
                return;
            };
            let Some(kind) = self.get_kind_at(focus_idx) else {
                #[cfg(target_os = "ios")]
                crate::ios_input::resign_text_input(window);
                window.set_ime_allowed(false);
                #[cfg(target_os = "ios")]
                {
                    self.ios_ime_retry = None;
                    self.ios_ime_viewport_poll = Some(IosImeRetry {
                        deadline: Instant::now()
                            + std::time::Duration::from_millis(IOS_IME_RETRY_INTERVAL_MS),
                        attempts: 0,
                    });
                }
                return;
            };
            if !matches!(kind, ComponentKind::TextInput { .. }) {
                #[cfg(target_os = "ios")]
                crate::ios_input::resign_text_input(window);
                window.set_ime_allowed(false);
                #[cfg(target_os = "ios")]
                {
                    self.ios_ime_retry = None;
                    self.ios_ime_viewport_poll = Some(IosImeRetry {
                        deadline: Instant::now()
                            + std::time::Duration::from_millis(IOS_IME_RETRY_INTERVAL_MS),
                        attempts: 0,
                    });
                }
                return;
            }
            let initial_value = self
                .text_input_values
                .get(&focus_idx)
                .cloned()
                .or_else(|| match kind {
                    ComponentKind::TextInput { value, .. } => Some(value.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            #[cfg(target_os = "ios")]
            crate::uitest::set_native_key_window(crate::ios_input::ensure_key_window(window));
            #[cfg(not(target_os = "ios"))]
            window.set_ime_allowed(true);
            #[cfg(target_os = "ios")]
            let is_first_responder =
                crate::ios_input::ensure_text_input_first_responder(window, &initial_value);
            #[cfg(target_os = "ios")]
            crate::uitest::set_native_first_responder(is_first_responder);
            if let Some(&(rect, _)) = self.layout_cache.iter().find(|(_, i)| *i == focus_idx) {
                let scale = self.scale_factor as f32;
                let x = (rect.x * scale) as i32;
                let y = ((rect.y + rect.height) * scale) as i32;
                let w = (rect.width * scale).max(1.0) as u32;
                let h = (rect.height * scale).max(1.0) as u32;
                window.set_ime_cursor_area(PhysicalPosition::new(x, y), PhysicalSize::new(w, h));
            }
            #[cfg(target_os = "ios")]
            {
                // UIKit can accept first responder before keyboard presentation
                // actually starts. Keep the event loop awake until the keyboard
                // layout guide confirms a visible inset; otherwise a real finger
                // tap can require a later scroll event while XCUITest still passes.
                let keyboard_visible = crate::ios_input::keyboard_inset_bottom(window)
                    .is_some_and(|inset| inset > 8.0);
                if is_first_responder == Some(true) && keyboard_visible {
                    self.ios_ime_retry = None;
                } else {
                    let attempts = self.ios_ime_retry.map(|retry| retry.attempts).unwrap_or(0);
                    self.ios_ime_retry = (attempts < IOS_IME_RETRY_LIMIT).then(|| IosImeRetry {
                        deadline: Instant::now()
                            + std::time::Duration::from_millis(IOS_IME_RETRY_INTERVAL_MS),
                        attempts: attempts + 1,
                    });
                }
            }
        }
    }

    fn focus_text_input(&mut self, idx: usize) {
        if std::env::var_os("W3COS_INPUT_TRACE").is_some() {
            eprintln!("[W3C OS][IME] TextInput focus index={idx}");
        }
        self.focused_index = Some(idx);
        crate::uitest::set_focused_index(self.focused_index);
        self.sync_soft_keyboard();
        #[cfg(target_os = "ios")]
        {
            self.ios_ime_viewport_poll = Some(IosImeRetry {
                deadline: Instant::now()
                    + std::time::Duration::from_millis(IOS_IME_RETRY_INTERVAL_MS),
                attempts: 0,
            });
        }
        self.needs_layout = true;
        self.repaint_after_interaction();
    }

    fn handle_click(&mut self, idx: usize) {
        if let Some(hit) = self.hit_nodes.iter().find(|h| h.index == idx) {
            let kind_is_text_input =
                matches!(self.get_kind_at(idx), Some(ComponentKind::TextInput { .. }));
            let kind_is_button =
                matches!(self.get_kind_at(idx), Some(ComponentKind::Button { .. }));

            if kind_is_text_input {
                self.focus_text_input(idx);
                return;
            }
            if kind_is_button {
                let action = hit.on_click.clone();
                if self.focused_index.take().is_some() {
                    crate::uitest::set_focused_index(self.focused_index);
                    self.sync_soft_keyboard();
                    self.needs_layout = true;
                }
                if !action.is_none() {
                    state::execute_action(&action);
                    self.rebuild_if_dirty();
                } else {
                    eprintln!("[W3C OS] Click → Button (no action)");
                }
                self.repaint_after_interaction();
                return;
            }
            if !hit.on_click.is_none() {
                state::execute_action(&hit.on_click);
                self.rebuild_if_dirty();
                self.repaint_after_interaction();
                return;
            }
        }
        self.focused_index = None;
        crate::uitest::set_focused_index(self.focused_index);
        self.sync_soft_keyboard();
        self.repaint_after_interaction();
    }

    fn repaint_after_interaction(&mut self) {
        self.request_repaint();
    }

    fn get_kind_at(&self, idx: usize) -> Option<&ComponentKind> {
        get_kind_recursive(&self.root, idx, &mut 0)
    }

    fn request_repaint(&self) {
        if let Some(window) = self.get_window() {
            window.request_redraw();
        }
    }

    fn focus_next(&mut self, backward: bool) {
        if self.focusable_indices.is_empty() {
            return;
        }
        let current_pos = self
            .focused_index
            .and_then(|idx| self.focusable_indices.iter().position(|&i| i == idx));
        let next_pos = match (current_pos, backward) {
            (None, false) => Some(0),
            (None, true) => Some(self.focusable_indices.len().saturating_sub(1)),
            (Some(p), false) => {
                if p + 1 < self.focusable_indices.len() {
                    Some(p + 1)
                } else {
                    Some(0)
                }
            }
            (Some(p), true) => {
                if p > 0 {
                    Some(p - 1)
                } else {
                    Some(self.focusable_indices.len().saturating_sub(1))
                }
            }
        };
        if let Some(pos) = next_pos {
            self.focused_index = Some(self.focusable_indices[pos]);
            crate::uitest::set_focused_index(self.focused_index);
            self.sync_soft_keyboard();
        }
    }

    #[cfg(feature = "ai-bridge")]
    fn poll_ai_bridge(&mut self) {
        let handle = match &self.ai_bridge_handle {
            Some(h) => h,
            None => return,
        };

        if self.dom_mode {
            crate::dom::with_document_mut(|doc| {
                handle.poll_and_respond(doc);
            });
        }
    }

    #[cfg(feature = "devtools")]
    fn poll_devtools(&mut self) {
        use crate::devtools::server::{DevToolsToMain, DomSnapshot, SerializedDocument};

        let handle = match &self.devtools_handle {
            Some(h) => h,
            None => return,
        };

        for msg in handle.poll_messages() {
            match msg {
                DevToolsToMain::RequestSnapshot => {
                    let serialized_doc = if self.dom_mode {
                        crate::dom::with_document(|doc| SerializedDocument::from_document(doc))
                    } else {
                        let mut nodes = Vec::new();
                        Self::serialize_component_tree(&self.root, None, &mut nodes);
                        SerializedDocument { nodes }
                    };
                    let snapshot = DomSnapshot {
                        serialized_doc,
                        layout_rects: self.layout_cache.clone(),
                    };
                    handle.send_snapshot(snapshot);
                }
                DevToolsToMain::HighlightNode(node_id) => {
                    self.devtools_highlight = node_id;
                    self.request_repaint();
                }
            }
        }
    }

    #[cfg(feature = "devtools")]
    fn serialize_component_tree(
        comp: &Component,
        parent_id: Option<u32>,
        nodes: &mut Vec<crate::devtools::server::SerializedNode>,
    ) {
        use crate::devtools::server::SerializedNode;

        let my_id = nodes.len() as u32;

        let (tag, text, attrs) = match &comp.kind {
            ComponentKind::Root | ComponentKind::Column | ComponentKind::VirtualList { .. } => {
                ("div", None, vec![])
            }
            ComponentKind::Row => ("div", None, vec![]),
            ComponentKind::Box => ("div", None, vec![]),
            ComponentKind::Text { content } => ("#text", Some(content.clone()), vec![]),
            ComponentKind::Button { label } => ("button", Some(label.clone()), vec![]),
            ComponentKind::Image { src } => ("img", None, vec![("src".to_string(), src.clone())]),
            ComponentKind::TextInput { value, placeholder } => (
                "input",
                Some(value.clone()),
                vec![("placeholder".to_string(), placeholder.clone())],
            ),
            ComponentKind::Canvas { width, height } => (
                "canvas",
                None,
                vec![
                    ("width".to_string(), width.to_string()),
                    ("height".to_string(), height.to_string()),
                ],
            ),
        };

        let node_type = if tag == "#text" { 3u8 } else { 1u8 };
        if my_id == 0 {
            // Root node is the document node
            nodes.push(SerializedNode {
                id: 0,
                node_type: 9,
                tag: "#document".to_string(),
                text_content: None,
                parent: None,
                children: vec![1],
                attributes: vec![],
                class_list: vec![],
                style: w3cos_std::style::Style::default(),
            });

            nodes.push(SerializedNode {
                id: 1,
                node_type: 1,
                tag: "body".to_string(),
                text_content: None,
                parent: Some(0),
                children: vec![],
                attributes: vec![],
                class_list: vec![],
                style: comp.style.clone(),
            });

            let body_idx = 1u32;
            let mut child_ids = Vec::new();
            for child in &comp.children {
                let child_id = nodes.len() as u32;
                child_ids.push(child_id);
                Self::serialize_component_tree(child, Some(body_idx), nodes);
            }
            nodes[1].children = child_ids;
            return;
        }

        nodes.push(SerializedNode {
            id: my_id,
            node_type,
            tag: tag.to_string(),
            text_content: text,
            parent: parent_id,
            children: vec![],
            attributes: attrs,
            class_list: vec![],
            style: comp.style.clone(),
        });

        let mut child_ids = Vec::new();
        for child in &comp.children {
            let child_id = nodes.len() as u32;
            child_ids.push(child_id);
            Self::serialize_component_tree(child, Some(my_id), nodes);
        }
        nodes[my_id as usize].children = child_ids;
    }

    #[cfg(feature = "gpu")]
    fn try_init_gpu(&mut self, event_loop: &ActiveEventLoop) -> bool {
        if matches!(self.gpu_state, GpuState::Active { .. }) {
            return true;
        }
        let window = match &self.gpu_state {
            GpuState::Suspended(cached) => cached.clone().unwrap_or_else(|| {
                let attrs = default_window_attributes();
                Arc::new(event_loop.create_window(attrs).unwrap())
            }),
            GpuState::Active { .. } => return true,
        };

        self.scale_factor = window.scale_factor();
        let size = window_backing_size(&window);

        let surface = match pollster::block_on(self.render_cx.create_surface(
            window.clone(),
            size.width.max(1),
            size.height.max(1),
            wgpu::PresentMode::AutoVsync,
        )) {
            Ok(surface) => surface,
            Err(error) => {
                eprintln!("[W3C OS] GPU surface initialization failed: {error}");
                return false;
            }
        };

        while self.renderers.len() <= surface.dev_id {
            self.renderers.push(None);
        }
        if self.renderers[surface.dev_id].is_none() {
            let dev = &self.render_cx.devices[surface.dev_id];
            let downlevel = dev.adapter().get_downlevel_capabilities();
            if !downlevel
                .flags
                .contains(wgpu::DownlevelFlags::INDIRECT_EXECUTION)
            {
                eprintln!("[W3C OS] GPU adapter lacks indirect execution; using CPU renderer");
                return false;
            }
            let init_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                Renderer::new(&dev.device, RendererOptions::default())
            }));
            match init_result {
                Ok(Ok(renderer)) => self.renderers[surface.dev_id] = Some(renderer),
                Ok(Err(error)) => {
                    eprintln!("[W3C OS] GPU renderer initialization failed: {error}");
                    return false;
                }
                Err(_) => {
                    eprintln!("[W3C OS] GPU renderer initialization panicked; using CPU renderer");
                    return false;
                }
            }
        }

        self.gpu_state = GpuState::Active { surface, window };
        self.needs_layout = true;
        true
    }

    #[cfg(not(feature = "gpu"))]
    fn try_init_gpu(&mut self, _event_loop: &ActiveEventLoop) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn count_virtual_lists(component: &Component) -> usize {
    usize::from(matches!(component.kind, ComponentKind::VirtualList { .. }))
        + component
            .children
            .iter()
            .map(count_virtual_lists)
            .sum::<usize>()
}

fn nth_virtual_list_mut(component: &mut Component, target: usize) -> Option<&mut Component> {
    fn visit<'a>(
        component: &'a mut Component,
        target: usize,
        cursor: &mut usize,
    ) -> Option<&'a mut Component> {
        if matches!(component.kind, ComponentKind::VirtualList { .. }) {
            if *cursor == target {
                return Some(component);
            }
            *cursor += 1;
        }
        for child in &mut component.children {
            if let Some(found) = visit(child, target, cursor) {
                return Some(found);
            }
        }
        None
    }
    visit(component, target, &mut 0)
}

fn virtual_spacer(height: f32) -> Component {
    let mut style = w3cos_std::style::Style::default();
    style.height = Dimension::Px(height.max(0.0));
    style.flex_shrink = 0.0;
    Component::boxed(style, vec![])
}

fn virtual_item_from_template(template: &Component, index: usize) -> Component {
    let mut item = template.clone();
    replace_virtual_index(&mut item, index);
    item
}

fn measure_virtual_list_rows(
    flat: &[layout::FlatNodeInfo<'_>],
    layout_cache: &[(LayoutRect, usize)],
    virtual_scroll_indices: &HashMap<usize, usize>,
    virtual_lists: &mut HashMap<usize, ComponentVirtualList>,
    scroll_offsets: &mut HashMap<usize, (f32, f32)>,
) -> bool {
    let rects: HashMap<usize, LayoutRect> = layout_cache
        .iter()
        .map(|(rect, index)| (*index, *rect))
        .collect();
    let mut changed = false;
    for (&root_index, &ordinal) in virtual_scroll_indices {
        let Some(host) = virtual_lists.get_mut(&ordinal) else {
            continue;
        };
        let direct_children: Vec<usize> = flat
            .iter()
            .enumerate()
            .filter_map(|(index, node)| (node.parent == Some(root_index)).then_some(index))
            .collect();
        if direct_children.len() < 2 {
            continue;
        }
        let logical_indices: Vec<usize> = host.engine.mounted().map(|item| item.index).collect();
        let before_total = host.engine.total_extent();
        let mut anchor_correction = 0.0;
        for (&node_index, logical_index) in direct_children[1..direct_children.len() - 1]
            .iter()
            .zip(logical_indices)
        {
            if let Some(rect) = rects.get(&node_index) {
                anchor_correction += host.engine.measure(logical_index, rect.height);
            }
        }
        changed |= (host.engine.total_extent() - before_total).abs() > 0.01;
        if anchor_correction.abs() > 0.01 {
            host.scroll_offset = (host.scroll_offset + anchor_correction).max(0.0);
            let (x, y) = scroll_offsets
                .get(&root_index)
                .copied()
                .unwrap_or((0.0, 0.0));
            scroll_offsets.insert(root_index, (x, (y + anchor_correction).max(0.0)));
        }
    }
    changed
}

fn replace_virtual_index(component: &mut Component, index: usize) {
    let replacement = index.to_string();
    match &mut component.kind {
        ComponentKind::Text { content } => *content = content.replace("{index}", &replacement),
        ComponentKind::Button { label } => *label = label.replace("{index}", &replacement),
        ComponentKind::TextInput { value, placeholder } => {
            *value = value.replace("{index}", &replacement);
            *placeholder = placeholder.replace("{index}", &replacement);
        }
        _ => {}
    }
    for child in &mut component.children {
        replace_virtual_index(child, index);
    }
}

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    let t = t.clamp(0.0, 1.0);
    (a as f32 + (b as f32 - a as f32) * t).round() as u8
}

trait PaintNodeView {
    fn paint_style(&self) -> &w3cos_std::style::Style;
    fn paint_parent(&self) -> Option<usize>;
}

impl PaintNodeView for layout::FlatNodeInfo<'_> {
    fn paint_style(&self) -> &w3cos_std::style::Style {
        self.style
    }

    fn paint_parent(&self) -> Option<usize> {
        self.parent
    }
}

impl PaintNodeView for RetainedPaintNode {
    fn paint_style(&self) -> &w3cos_std::style::Style {
        &self.style
    }

    fn paint_parent(&self) -> Option<usize> {
        self.parent
    }
}

fn animated_style_overrides<T: PaintNodeView>(
    flat: &[T],
    animations: &[ActiveAnimation],
    now: Instant,
) -> HashMap<usize, w3cos_std::style::Style> {
    let mut overrides = HashMap::new();
    let mut subtree_translations = Vec::new();

    for animation in animations {
        let idx = animation.node_index();
        if idx >= flat.len() {
            continue;
        }
        let eased = animation.eased_progress(now);
        let entry = overrides
            .entry(idx)
            .or_insert_with(|| flat[idx].paint_style().clone());
        match animation {
            ActiveAnimation::LayoutHeight { .. } => {}
            ActiveAnimation::Opacity { from, to, .. } => {
                entry.opacity = *from + eased * (to - from);
            }
            ActiveAnimation::Background { from, to, .. } => {
                entry.background = Color::rgba(
                    lerp_u8(from.r, to.r, eased),
                    lerp_u8(from.g, to.g, eased),
                    lerp_u8(from.b, to.b, eased),
                    lerp_u8(from.a, to.a, eased),
                );
            }
            ActiveAnimation::Transform { from, to, .. } => {
                let sampled = lerp_transform(*from, *to, eased);
                entry.transform = sampled;
                subtree_translations.push((
                    idx,
                    sampled.translate_x - flat[idx].paint_style().transform.translate_x,
                    sampled.translate_y - flat[idx].paint_style().transform.translate_y,
                ));
            }
        }
    }

    // Flat nodes are depth-first. Once the ancestor chain no longer reaches
    // the animated node, its contiguous subtree has ended.
    for (ancestor, dx, dy) in subtree_translations {
        for idx in (ancestor + 1)..flat.len() {
            let mut parent = flat[idx].paint_parent();
            let mut belongs_to_subtree = false;
            while let Some(parent_idx) = parent {
                if parent_idx == ancestor {
                    belongs_to_subtree = true;
                    break;
                }
                parent = flat[parent_idx].paint_parent();
            }
            if !belongs_to_subtree {
                break;
            }
            let entry = overrides
                .entry(idx)
                .or_insert_with(|| flat[idx].paint_style().clone());
            entry.transform.translate_x += dx;
            entry.transform.translate_y += dy;
        }
    }

    overrides
}

fn get_kind_recursive<'a>(
    comp: &'a Component,
    target: usize,
    counter: &mut usize,
) -> Option<&'a ComponentKind> {
    let my_idx = *counter;
    *counter += 1;
    if my_idx == target {
        return Some(&comp.kind);
    }
    for child in &comp.children {
        if let Some(k) = get_kind_recursive(child, target, counter) {
            return Some(k);
        }
    }
    None
}

fn node_intersects_paint_cull(
    idx: usize,
    rect: LayoutRect,
    scroll_info: &[Option<(f32, f32, LayoutRect)>],
    viewport_w: f32,
    viewport_h: f32,
    overscan: f32,
) -> bool {
    let (paint_rect, clip) = match scroll_info.get(idx).copied().flatten() {
        Some((sx, sy, clip)) => (
            LayoutRect {
                x: rect.x - sx,
                y: rect.y - sy,
                width: rect.width,
                height: rect.height,
            },
            Some(clip),
        ),
        None => (rect, None),
    };
    let intersects = |a: LayoutRect, b: LayoutRect| {
        a.width > 0.0
            && a.height > 0.0
            && b.width > 0.0
            && b.height > 0.0
            && a.x < b.x + b.width
            && a.x + a.width > b.x
            && a.y < b.y + b.height
            && a.y + a.height > b.y
    };
    let viewport = LayoutRect {
        x: -overscan,
        y: -overscan,
        width: viewport_w + overscan * 2.0,
        height: viewport_h + overscan * 2.0,
    };
    if !intersects(paint_rect, viewport) {
        return false;
    }
    clip.is_none_or(|clip| {
        intersects(
            paint_rect,
            LayoutRect {
                x: clip.x - overscan,
                y: clip.y - overscan,
                width: clip.width + overscan * 2.0,
                height: clip.height + overscan * 2.0,
            },
        )
    })
}

fn build_sticky_marker_index(
    flat: &[layout::FlatNodeInfo<'_>],
    layout_cache: &[(LayoutRect, usize)],
    scroll_ancestor: &[Option<usize>],
) -> HashMap<usize, HashMap<usize, Vec<f32>>> {
    let mut index: HashMap<usize, HashMap<usize, Vec<f32>>> = HashMap::new();
    for &(rect, idx) in layout_cache {
        let Some(signal) = flat.get(idx).and_then(|node| node.sticky_counter_signal) else {
            continue;
        };
        let Some(scroll_idx) = scroll_ancestor.get(idx).copied().flatten() else {
            continue;
        };
        index
            .entry(scroll_idx)
            .or_default()
            .entry(signal)
            .or_default()
            .push(rect.y);
    }
    for signals in index.values_mut() {
        for positions in signals.values_mut() {
            positions.sort_by(f32::total_cmp);
        }
    }
    index
}

fn sticky_marker_counts(
    index: &HashMap<usize, HashMap<usize, Vec<f32>>>,
    scroll_idx: usize,
    threshold_y: f32,
    scroll_y: f32,
) -> HashMap<usize, usize> {
    let Some(signals) = index.get(&scroll_idx) else {
        return HashMap::new();
    };
    let crossing_y = threshold_y + scroll_y;
    let mut counts = HashMap::with_capacity(signals.len());
    for (&signal, positions) in signals {
        counts.insert(
            signal,
            positions.partition_point(|position| *position <= crossing_y),
        );
    }
    counts
}

fn topmost_scroll_node_at(
    x: f32,
    y: f32,
    scrollable: &[(usize, LayoutRect, ScrollExtent)],
    scroll_info: &[Option<(f32, f32, LayoutRect)>],
    paint_z: &[i32],
    overlay_blockers: &[(usize, LayoutRect)],
) -> Option<usize> {
    let candidate = scrollable
        .iter()
        .filter_map(|(idx, rect, _)| {
            let (visual_rect, clip) = match scroll_info.get(*idx).copied().flatten() {
                Some((sx, sy, clip)) => (
                    LayoutRect {
                        x: rect.x - sx,
                        y: rect.y - sy,
                        ..*rect
                    },
                    Some(clip),
                ),
                None => (*rect, None),
            };
            let inside_rect = x >= visual_rect.x
                && x <= visual_rect.x + visual_rect.width
                && y >= visual_rect.y
                && y <= visual_rect.y + visual_rect.height;
            let inside_clip = clip.is_none_or(|clip| {
                x >= clip.x && x <= clip.x + clip.width && y >= clip.y && y <= clip.y + clip.height
            });
            (inside_rect && inside_clip).then_some((paint_z.get(*idx).copied().unwrap_or(0), *idx))
        })
        .max()
        .map(|(z, idx)| (z, idx));
    let candidate_z = candidate.map(|(z, _)| z).unwrap_or(i32::MIN);
    let blocked = overlay_blockers.iter().any(|(idx, rect)| {
        paint_z.get(*idx).copied().unwrap_or_default() > candidate_z
            && x >= rect.x
            && x <= rect.x + rect.width
            && y >= rect.y
            && y <= rect.y + rect.height
    });
    (!blocked).then(|| candidate.map(|(_, idx)| idx)).flatten()
}

fn direct_scroll_chain_parent(
    idx: usize,
    scroll_ancestor: &[Option<usize>],
    scrollable: &[(usize, LayoutRect, ScrollExtent)],
) -> Option<usize> {
    let parent = scroll_ancestor.get(idx).copied().flatten()?;
    scrollable
        .iter()
        .any(|(scroll_idx, _, _)| *scroll_idx == parent)
        .then_some(parent)
}

fn build_retained_prepaint(
    flat: &[layout::FlatNodeInfo<'_>],
    layout_cache: &[(LayoutRect, usize)],
) -> (
    Vec<RetainedPaintNode>,
    Vec<i32>,
    Vec<Option<usize>>,
    Vec<Option<LayoutRect>>,
) {
    let mut paint_z = vec![0; flat.len()];
    let mut sticky_owner = vec![None; flat.len()];
    let paint_nodes = flat
        .iter()
        .enumerate()
        .map(|(idx, node)| {
            let inherited_z = node.parent.map(|parent| paint_z[parent]).unwrap_or(0);
            paint_z[idx] = if node.style.z_index == 0 {
                inherited_z
            } else {
                node.style.z_index
            };
            sticky_owner[idx] = if matches!(node.style.position, Position::Sticky) {
                Some(idx)
            } else {
                node.parent.and_then(|parent| sticky_owner[parent])
            };
            RetainedPaintNode {
                kind: node.kind.clone(),
                style: node.style.clone(),
                parent: node.parent,
            }
        })
        .collect();
    let mut rect_by_index = vec![None; flat.len()];
    for &(rect, idx) in layout_cache {
        if idx < rect_by_index.len() {
            rect_by_index[idx] = Some(rect);
        }
    }
    (paint_nodes, paint_z, sticky_owner, rect_by_index)
}

/// Build scroll info using pre-computed scroll ancestors and optionally the
/// retained PrePaint ownership/geometry produced by the last layout pass.
/// O(n) instead of O(n * tree_depth), with no tree walk on compositor scroll.
fn build_scroll_info_fast<T: PaintNodeView>(
    scroll_ancestor: &[Option<usize>],
    scrollable: &[(usize, LayoutRect, ScrollExtent)],
    clip_only: &[(usize, LayoutRect)],
    offsets: &HashMap<usize, (f32, f32)>,
    overscroll_states: &HashMap<usize, OverscrollState>,
    layout_cache: &[(LayoutRect, usize)],
    flat: &[T],
    retained_prepaint: Option<(&[Option<usize>], &[Option<LayoutRect>])>,
    viewport_w: f32,
    viewport_h: f32,
) -> Vec<Option<(f32, f32, LayoutRect)>> {
    if scroll_ancestor.is_empty() {
        return Vec::new();
    }

    let scrollable_rect: HashMap<usize, LayoutRect> =
        scrollable.iter().map(|(i, r, _)| (*i, *r)).collect();
    let clip_only_rect: HashMap<usize, LayoutRect> =
        clip_only.iter().map(|(i, r)| (*i, *r)).collect();
    let mut owned_rect_by_index = Vec::new();
    let mut owned_sticky_owner = Vec::new();
    let (sticky_owner, rect_by_index) = if let Some(retained) = retained_prepaint {
        retained
    } else {
        owned_rect_by_index.resize(flat.len(), None);
        for &(rect, idx) in layout_cache {
            if idx < owned_rect_by_index.len() {
                owned_rect_by_index[idx] = Some(rect);
            }
        }
        owned_sticky_owner.resize(flat.len(), None);
        for (idx, node) in flat.iter().enumerate() {
            owned_sticky_owner[idx] = if matches!(node.paint_style().position, Position::Sticky) {
                Some(idx)
            } else {
                node.paint_parent()
                    .and_then(|parent| owned_sticky_owner[parent])
            };
        }
        (
            owned_sticky_owner.as_slice(),
            owned_rect_by_index.as_slice(),
        )
    };

    scroll_ancestor
        .iter()
        .enumerate()
        .map(|(idx, ancestor)| match ancestor {
            Some(anc_idx) => {
                if let Some(&clip) = scrollable_rect.get(anc_idx) {
                    let (mut sx, mut sy) = offsets.get(anc_idx).copied().unwrap_or((0.0, 0.0));
                    let mut effective_clip = clip;
                    if let Some(owner) = sticky_owner.get(idx).copied().flatten()
                        && let Some(owner_rect) = rect_by_index.get(owner).copied().flatten()
                    {
                        let style = flat[owner].paint_style();
                        if let Some(sticky_scroll_idx) = scroll_ancestor[owner]
                            && let Some(&sticky_clip) = scrollable_rect.get(&sticky_scroll_idx)
                        {
                            let (sticky_sx, sticky_sy) = offsets
                                .get(&sticky_scroll_idx)
                                .copied()
                                .unwrap_or((0.0, 0.0));
                            let top = style
                                .top
                                .resolve(
                                    sticky_clip.height,
                                    16.0,
                                    style.font_size,
                                    viewport_w,
                                    viewport_h,
                                )
                                .unwrap_or(0.0);
                            let sticky_effective_y = clamp_sticky_scroll_offset(
                                owner_rect.y,
                                sticky_clip.y,
                                top,
                                sticky_sy,
                            );
                            if sticky_scroll_idx == *anc_idx {
                                sy = sticky_effective_y;
                            } else {
                                // A scrollable list inside a sticky panel needs
                                // both its own scroll and the panel's clamped
                                // outer-scroll transform.
                                sx += sticky_sx;
                                let sticky_visual_y = sticky_effective_y
                                    - overscroll_displacement_y(
                                        overscroll_states,
                                        sticky_scroll_idx,
                                    );
                                sy += sticky_visual_y;
                                effective_clip.x -= sticky_sx;
                                effective_clip.y -= sticky_visual_y;
                            }
                        }
                    }
                    sy -= overscroll_displacement_y(overscroll_states, *anc_idx);
                    Some((sx, sy, effective_clip))
                } else if let Some(&clip) = clip_only_rect.get(anc_idx) {
                    let mut sx = 0.0;
                    let mut sy = 0.0;
                    let mut effective_clip = clip;
                    if let Some(owner) = sticky_owner.get(idx).copied().flatten()
                        && let Some(owner_rect) = rect_by_index.get(owner).copied().flatten()
                    {
                        let style = flat[owner].paint_style();
                        if let Some(sticky_scroll_idx) = scroll_ancestor[owner]
                            && let Some(&sticky_clip) = scrollable_rect.get(&sticky_scroll_idx)
                        {
                            let (sticky_sx, sticky_sy) = offsets
                                .get(&sticky_scroll_idx)
                                .copied()
                                .unwrap_or((0.0, 0.0));
                            let top = style
                                .top
                                .resolve(
                                    sticky_clip.height,
                                    16.0,
                                    style.font_size,
                                    viewport_w,
                                    viewport_h,
                                )
                                .unwrap_or(0.0);
                            sx = sticky_sx;
                            sy =
                                clamp_sticky_scroll_offset(
                                    owner_rect.y,
                                    sticky_clip.y,
                                    top,
                                    sticky_sy,
                                ) - overscroll_displacement_y(overscroll_states, sticky_scroll_idx);
                            effective_clip.x -= sx;
                            effective_clip.y -= sy;
                        }
                    }
                    Some((sx, sy, effective_clip))
                } else {
                    None
                }
            }
            None => None,
        })
        .collect()
}

fn overscroll_displacement_y(states: &HashMap<usize, OverscrollState>, scroll_idx: usize) -> f32 {
    states
        .get(&scroll_idx)
        .map(|state| state.displacement_y)
        .unwrap_or(0.0)
}

fn clamp_sticky_scroll_offset(
    owner_y: f32,
    scrollport_y: f32,
    sticky_top: f32,
    scroll_y: f32,
) -> f32 {
    scroll_y.min((owner_y - scrollport_y - sticky_top).max(0.0))
}

// ---------------------------------------------------------------------------
// CPU-only drawing helpers
// ---------------------------------------------------------------------------

#[cfg(feature = "cpu-render")]
impl CpuPresenter {
    fn present(&mut self, pixmap: &Pixmap, w: u32, h: u32) {
        if self.buffer_size != (w, h) {
            if let (Some(w_nz), Some(h_nz)) = (NonZeroU32::new(w), NonZeroU32::new(h)) {
                let _ = self.surface.resize(w_nz, h_nz);
                self.buffer_size = (w, h);
            }
        }
        let mut buffer = match self.surface.buffer_mut() {
            Ok(b) => b,
            Err(_) => return,
        };
        for (dst, chunk) in buffer.iter_mut().zip(pixmap.data().chunks_exact(4)) {
            *dst = (chunk[0] as u32) << 16 | (chunk[1] as u32) << 8 | (chunk[2] as u32);
        }
        let _ = buffer.present();
    }
}

#[cfg(feature = "cpu-render")]
fn draw_hover_outline_cpu(pixmap: &mut Pixmap, rect: LayoutRect) {
    let color = tiny_skia::Color::from_rgba8(108, 92, 231, 100);
    let mut paint = tiny_skia::Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;

    let w = 2.0;
    for r in [
        tiny_skia::Rect::from_xywh(rect.x, rect.y, rect.width, w),
        tiny_skia::Rect::from_xywh(rect.x, rect.y + rect.height - w, rect.width, w),
        tiny_skia::Rect::from_xywh(rect.x, rect.y, w, rect.height),
        tiny_skia::Rect::from_xywh(rect.x + rect.width - w, rect.y, w, rect.height),
    ]
    .into_iter()
    .flatten()
    {
        pixmap.fill_rect(r, &paint, tiny_skia::Transform::identity(), None);
    }
}

#[cfg(feature = "cpu-render")]
fn draw_focus_ring_cpu(pixmap: &mut Pixmap, rect: LayoutRect) {
    let color = tiny_skia::Color::from_rgba8(108, 92, 231, 180);
    let mut paint = tiny_skia::Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;

    let w = 3.0;
    for r in [
        tiny_skia::Rect::from_xywh(rect.x, rect.y, rect.width, w),
        tiny_skia::Rect::from_xywh(rect.x, rect.y + rect.height - w, rect.width, w),
        tiny_skia::Rect::from_xywh(rect.x, rect.y, w, rect.height),
        tiny_skia::Rect::from_xywh(rect.x + rect.width - w, rect.y, w, rect.height),
    ]
    .into_iter()
    .flatten()
    {
        pixmap.fill_rect(r, &paint, tiny_skia::Transform::identity(), None);
    }
}

// ---------------------------------------------------------------------------
// ApplicationHandler
// ---------------------------------------------------------------------------

impl ApplicationHandler for App {
    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        self.tick_kinetic_scroll();
        self.tick_overscroll();

        // React refs/effects can enqueue work during the initial build without
        // an input event. Pump that work before sleeping so components such as
        // react-window can install their native scroll listener immediately.
        if state::is_dirty() || w3cos_react_compat::aot::has_pending_render() {
            self.rebuild_if_dirty();
            self.request_repaint();
        }

        if crate::speech::poll() {
            self.rebuild_if_dirty();
            self.request_repaint();
        }

        #[cfg(target_os = "ios")]
        if self
            .ios_ime_retry
            .is_some_and(|retry| retry.deadline <= Instant::now())
        {
            self.sync_soft_keyboard();
        }

        #[cfg(target_os = "ios")]
        if self
            .ios_ime_viewport_poll
            .is_some_and(|poll| poll.deadline <= Instant::now())
        {
            self.poll_native_text_input();
            let previous_attempts = self
                .ios_ime_viewport_poll
                .map(|poll| poll.attempts)
                .unwrap_or(IOS_IME_VIEWPORT_POLL_LIMIT);
            let changed = self.poll_viewport_inset();
            let stable_attempts = if changed {
                0
            } else {
                previous_attempts
                    .saturating_add(1)
                    .min(IOS_IME_VIEWPORT_POLL_LIMIT)
            };
            let should_continue =
                ViewportLayout::ime_open_for_app(self) || self.viewport.keyboard_inset_bottom > 0.0;
            let interval_ms = if stable_attempts < IOS_IME_VIEWPORT_POLL_LIMIT {
                IOS_IME_RETRY_INTERVAL_MS
            } else {
                IOS_IME_IDLE_POLL_INTERVAL_MS
            };
            self.ios_ime_viewport_poll = should_continue.then(|| IosImeRetry {
                deadline: Instant::now() + std::time::Duration::from_millis(interval_ms),
                attempts: stable_attempts,
            });
        }

        if matches!(cause, StartCause::ResumeTimeReached { .. }) {
            let timer_actions = crate::timers::tick();
            for action in &timer_actions {
                state::execute_action(action);
            }
            if !timer_actions.is_empty() {
                self.rebuild_if_dirty();
            }

            if !self.animations.is_empty() || !timer_actions.is_empty() {
                self.request_repaint();
            }
        }

        #[cfg(feature = "devtools")]
        self.poll_devtools();

        #[cfg(feature = "ai-bridge")]
        self.poll_ai_bridge();
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // winit on Android can drop touch wakeups under Wait; keep polling input.
        #[cfg(target_os = "android")]
        {
            let _ = self.poll_viewport_inset();
            event_loop.set_control_flow(ControlFlow::Poll);
            return;
        }

        let has_animations = !self.animations.is_empty()
            || self.kinetic_scroll.is_some()
            || !self.overscroll_states.is_empty();
        let now = Instant::now();
        let animation_deadline = has_animations.then(|| {
            self.last_frame_time.unwrap_or(now)
                + std::time::Duration::from_millis(ANIMATION_FRAME_INTERVAL_MS)
        });

        // Keep animation cadence anchored to the previous frame start. Using
        // `now + 16ms` here adds layout/paint time to every interval and can
        // turn a 60 Hz transition into a visibly uneven 15–30 Hz sequence.
        // An overdue frame is requested once, then the next paint advances
        // `last_frame_time`, so Poll cannot become an idle busy loop.
        if animation_deadline.is_some_and(|deadline| deadline <= now) {
            self.request_repaint();
            event_loop.set_control_flow(ControlFlow::Poll);
            return;
        }
        let mut timer_deadline = crate::timers::next_deadline();
        #[cfg(target_os = "ios")]
        if let Some(retry) = self.ios_ime_retry {
            timer_deadline = Some(
                timer_deadline
                    .map(|deadline| deadline.min(retry.deadline))
                    .unwrap_or(retry.deadline),
            );
        }
        #[cfg(target_os = "ios")]
        if let Some(poll) = self.ios_ime_viewport_poll {
            timer_deadline = Some(
                timer_deadline
                    .map(|deadline| deadline.min(poll.deadline))
                    .unwrap_or(poll.deadline),
            );
        }
        if let Some(speech_deadline) = crate::speech::next_deadline() {
            timer_deadline = Some(
                timer_deadline
                    .map(|deadline| deadline.min(speech_deadline))
                    .unwrap_or(speech_deadline),
            );
        }

        #[cfg(feature = "devtools")]
        let has_devtools = self.devtools_handle.is_some();
        #[cfg(not(feature = "devtools"))]
        let has_devtools = false;

        #[cfg(feature = "ai-bridge")]
        let has_devtools = has_devtools || self.ai_bridge_handle.is_some();

        match (has_animations, timer_deadline) {
            (false, None) => {
                if has_devtools {
                    event_loop.set_control_flow(ControlFlow::WaitUntil(
                        Instant::now() + std::time::Duration::from_millis(100),
                    ));
                } else {
                    event_loop.set_control_flow(ControlFlow::Wait);
                }
            }
            (true, None) => {
                event_loop.set_control_flow(ControlFlow::WaitUntil(
                    animation_deadline.expect("active animation has a deadline"),
                ));
            }
            (false, Some(deadline)) => {
                if has_devtools {
                    let devtools_deadline = Instant::now() + std::time::Duration::from_millis(100);
                    event_loop
                        .set_control_flow(ControlFlow::WaitUntil(deadline.min(devtools_deadline)));
                } else {
                    event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
                }
            }
            (true, Some(deadline)) => {
                let anim_deadline = animation_deadline.expect("active animation has a deadline");
                event_loop.set_control_flow(ControlFlow::WaitUntil(deadline.min(anim_deadline)));
            }
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        #[cfg(target_os = "ios")]
        crate::uitest::maybe_start_server();

        #[cfg(all(feature = "gpu", feature = "cpu-render"))]
        {
            // Android NativeActivity main thread must stay responsive — defer GPU probe.
            #[cfg(target_os = "android")]
            {
                self.ensure_cpu_presenter(event_loop);
                self.using_gpu = false;
                crate::perf::set_backend("cpu");
            }
            #[cfg(not(target_os = "android"))]
            if self.try_init_gpu(event_loop) {
                self.using_gpu = true;
                crate::perf::set_backend("gpu");
            } else {
                self.ensure_cpu_presenter(event_loop);
                self.using_gpu = false;
                crate::perf::set_backend("cpu");
            }
        }

        #[cfg(all(feature = "gpu", not(feature = "cpu-render")))]
        {
            let _ = self.try_init_gpu(event_loop);
        }

        #[cfg(feature = "cpu-render")]
        {
            #[cfg(feature = "gpu")]
            if !self.using_gpu {
                self.ensure_cpu_presenter(event_loop);
            }
            #[cfg(not(feature = "gpu"))]
            self.ensure_cpu_presenter(event_loop);
        }
        #[cfg(all(feature = "cpu-render", not(feature = "gpu")))]
        crate::perf::set_backend("cpu");

        #[cfg(feature = "devtools")]
        {
            if self.devtools_handle.is_none() {
                let port = std::env::var("W3COS_DEVTOOLS_PORT")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(9229u16);
                self.devtools_handle = Some(crate::devtools::DevToolsServer::start(port));
            }
        }

        #[cfg(feature = "ai-bridge")]
        {
            if self.ai_bridge_handle.is_none() {
                if let Ok(port_str) = std::env::var("W3COS_AI_PORT") {
                    if let Ok(port) = port_str.parse::<u16>() {
                        let provider: std::sync::Arc<
                            dyn w3cos_ai_bridge::server::ScreenshotProvider,
                        > = std::sync::Arc::new(FrameCacheScreenshot);
                        self.ai_bridge_handle =
                            Some(w3cos_ai_bridge::server::start_with_provider(port, provider));
                    }
                }
            }
        }

        #[cfg(target_os = "android")]
        self.request_repaint();
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        #[cfg(feature = "gpu")]
        {
            if let GpuState::Active { window, .. } =
                std::mem::replace(&mut self.gpu_state, GpuState::Suspended(None))
            {
                self.gpu_state = GpuState::Suspended(Some(window));
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::ModifiersChanged(mods) => {
                self.modifiers = mods.state();
            }

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.scale_factor = scale_factor;
                self.needs_layout = true;
                self.request_repaint();
            }

            WindowEvent::RedrawRequested => {
                self.paint();
            }

            WindowEvent::Resized(_size) => {
                #[cfg(feature = "gpu")]
                {
                    let resize_gpu = {
                        #[cfg(all(feature = "gpu", feature = "cpu-render"))]
                        {
                            self.using_gpu
                        }
                        #[cfg(all(feature = "gpu", not(feature = "cpu-render")))]
                        {
                            true
                        }
                        #[cfg(not(feature = "gpu"))]
                        {
                            false
                        }
                    };
                    let surface_size = self
                        .get_window_gpu()
                        .map(window_backing_size)
                        .unwrap_or(_size);
                    if resize_gpu && surface_size.width > 0 && surface_size.height > 0 {
                        if let GpuState::Active {
                            ref mut surface, ..
                        } = self.gpu_state
                        {
                            self.render_cx.resize_surface(
                                surface,
                                surface_size.width,
                                surface_size.height,
                            );
                        }
                    }
                }
                self.needs_layout = true;
                self.request_repaint();
            }

            WindowEvent::CursorMoved { position, .. } => {
                // winit gives physical pixels; convert to logical for hit testing
                self.set_pointer_logical(position.x, position.y);
                self.update_hover_at_pointer();
            }

            WindowEvent::Touch(touch) => {
                self.set_pointer_logical(touch.location.x, touch.location.y);
                match touch.phase {
                    TouchPhase::Started => {
                        self.kinetic_scroll = None;
                        self.last_overscroll_tick = None;
                        let now = Instant::now();
                        self.last_touch_y = Some(self.mouse_y);
                        self.touch_samples.clear();
                        self.touch_samples.push_back((now, self.mouse_y));
                        self.touch_drag_y = 0.0;
                        self.touch_scroll_active = false;
                        self.touch_scroll_index = self.hit_test_scroll(self.mouse_x, self.mouse_y);
                        self.pointer_pressed();
                    }
                    TouchPhase::Moved => {
                        if let Some(last_y) = self.last_touch_y {
                            let now = Instant::now();
                            let dy = last_y - self.mouse_y;
                            self.touch_drag_y += dy.abs();
                            self.touch_samples.push_back((now, self.mouse_y));
                            while self.touch_samples.len() > 2
                                && now.duration_since(self.touch_samples[1].0)
                                    > KINETIC_VELOCITY_WINDOW
                            {
                                self.touch_samples.pop_front();
                            }
                            if !self.touch_scroll_active
                                && self.touch_drag_y > TOUCH_SCROLL_SLOP
                                && self.touch_scroll_index.is_some()
                            {
                                self.touch_scroll_active = true;
                                self.pressed_index = None;
                            }
                            if self.touch_scroll_active {
                                if let Some(index) = self.touch_scroll_index {
                                    self.touch_scroll_index =
                                        Some(self.apply_touch_scroll(index, dy));
                                }
                            } else {
                                self.update_hover_at_pointer();
                            }
                            self.last_touch_y = Some(self.mouse_y);
                        }
                    }
                    TouchPhase::Ended => {
                        let release_velocity =
                            estimate_touch_velocity(&self.touch_samples, Instant::now())
                                .unwrap_or(0.0);
                        let recent_velocity = release_velocity.abs() >= KINETIC_SCROLL_MIN_VELOCITY;
                        let released_overscroll = self.touch_scroll_active
                            && self.release_active_overscroll(-release_velocity * 0.35);
                        if std::env::var_os("W3COS_INPUT_TRACE").is_some() {
                            let displacement = self
                                .overscroll_states
                                .values()
                                .map(|state| state.displacement_y.abs())
                                .fold(0.0_f32, f32::max);
                            eprintln!(
                                "[W3C OS][SCROLL] end active={} recent={} velocity={:.1} overscroll={:.1} released={}",
                                self.touch_scroll_active,
                                recent_velocity,
                                release_velocity,
                                displacement,
                                released_overscroll
                            );
                        }
                        let mut started_kinetic = false;
                        if self.touch_scroll_active && recent_velocity {
                            if let Some(index) = self.touch_scroll_index {
                                crate::uitest::set_kinetic_started(release_velocity);
                                self.kinetic_scroll = Some(KineticScroll {
                                    index,
                                    curve: MobileFlingCurve::new(release_velocity),
                                    started_at: Instant::now(),
                                    last_offset: 0.0,
                                });
                                started_kinetic = true;
                                self.request_repaint();
                            }
                        }
                        self.last_touch_y = None;
                        self.touch_samples.clear();
                        self.touch_drag_y = 0.0;
                        self.touch_scroll_index = None;
                        if self.touch_scroll_active {
                            self.touch_scroll_active = false;
                        } else {
                            self.pointer_released();
                        }
                        // A sticky-counter commit can rebuild the flattened
                        // tree and invalidate the scroll-node index captured by
                        // the fling. Treat drag + inertia as one gesture and
                        // defer that rebuild until kinetic scrolling settles.
                        if !started_kinetic {
                            self.flush_pending_sticky_counters();
                        }
                    }
                    TouchPhase::Cancelled => {
                        self.release_active_overscroll(0.0);
                        self.last_touch_y = None;
                        self.touch_samples.clear();
                        self.touch_drag_y = 0.0;
                        self.touch_scroll_active = false;
                        self.touch_scroll_index = None;
                        self.pressed_index = None;
                        self.flush_pending_sticky_counters();
                        self.request_repaint();
                    }
                }
            }

            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => match state {
                ElementState::Pressed => self.pointer_pressed(),
                ElementState::Released => self.pointer_released(),
            },

            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } => {
                if event.state != ElementState::Pressed {
                    return;
                }
                if let Some(focus_idx) = self.focused_index {
                    if let Some(kind) = self.get_kind_at(focus_idx) {
                        match kind {
                            ComponentKind::TextInput { value, .. } => {
                                let value = value.clone();
                                if let Key::Named(NamedKey::Backspace) = event.logical_key {
                                    let current = self
                                        .text_input_values
                                        .entry(focus_idx)
                                        .or_insert_with(|| value.clone());
                                    if !current.is_empty() {
                                        let mut chars: Vec<char> = current.chars().collect();
                                        chars.pop();
                                        *current = chars.into_iter().collect();
                                        self.request_repaint();
                                    }
                                    return;
                                }
                                if let Key::Named(NamedKey::Tab) = event.logical_key {
                                    self.focus_next(self.modifiers.shift_key());
                                    self.request_repaint();
                                    return;
                                }
                                if let Some(ref text) = event.text {
                                    if !text.is_empty() && !text.chars().all(|c| c.is_control()) {
                                        let current = self
                                            .text_input_values
                                            .entry(focus_idx)
                                            .or_insert_with(|| value.clone());
                                        current.push_str(text);
                                        self.request_repaint();
                                        return;
                                    }
                                }
                            }
                            ComponentKind::Button { .. } => {
                                if let Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Space) =
                                    event.logical_key
                                {
                                    if let Some(hit) =
                                        self.hit_nodes.iter().find(|h| h.index == focus_idx)
                                    {
                                        if !hit.on_click.is_none() {
                                            state::execute_action(&hit.on_click);
                                            self.rebuild_if_dirty();
                                        }
                                    }
                                    self.request_repaint();
                                    return;
                                }
                                if let Key::Named(NamedKey::Tab) = event.logical_key {
                                    self.focus_next(self.modifiers.shift_key());
                                    self.request_repaint();
                                    return;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                if let Key::Named(NamedKey::Tab) = event.logical_key {
                    self.focus_next(self.modifiers.shift_key());
                    self.request_repaint();
                }
            }

            WindowEvent::Ime(ime) => {
                if let Some(focus_idx) = self.focused_index {
                    if let Some(kind) = self.get_kind_at(focus_idx) {
                        if let ComponentKind::TextInput { value, .. } = kind {
                            let value = value.clone();
                            match ime {
                                winit::event::Ime::Commit(commit) => {
                                    let current = self
                                        .text_input_values
                                        .entry(focus_idx)
                                        .or_insert_with(|| value);
                                    current.push_str(&commit);
                                    self.request_repaint();
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => -y * 24.0,
                    MouseScrollDelta::PixelDelta(pos) => -pos.y as f32,
                };
                self.scroll_at_pointer(dy);
            }

            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

fn offset_layout_y(
    inset_top: f32,
    layout_cache: &mut [(LayoutRect, usize)],
    scrollable: &mut [(usize, LayoutRect, ScrollExtent)],
    clip_only: &mut [(usize, LayoutRect)],
) {
    if inset_top <= 0.0 {
        return;
    }
    for (rect, _) in layout_cache.iter_mut() {
        rect.y += inset_top;
    }
    for (_, rect, _) in scrollable.iter_mut() {
        rect.y += inset_top;
    }
    for (_, rect) in clip_only.iter_mut() {
        rect.y += inset_top;
    }
}

/// CSS Scroll Snap Level 2 `scroll-initial-target`: the first rendered target
/// in tree order establishes the initial position of its nearest scrollport.
/// A late target is honored until the user manually scrolls that container.
fn apply_initial_scroll_targets(
    flat: &[layout::FlatNodeInfo<'_>],
    layout_cache: &[(LayoutRect, usize)],
    scrollable_nodes: &[(usize, LayoutRect, ScrollExtent)],
    scroll_ancestor: &[Option<usize>],
    scroll_offsets: &mut HashMap<usize, (f32, f32)>,
    initialized: &mut HashSet<usize>,
    user_scrolled: &mut HashSet<usize>,
) {
    let rects: HashMap<usize, LayoutRect> = layout_cache
        .iter()
        .map(|(rect, index)| (*index, *rect))
        .collect();
    let scrollports: HashMap<usize, (LayoutRect, ScrollExtent)> = scrollable_nodes
        .iter()
        .map(|(index, rect, extent)| (*index, (*rect, *extent)))
        .collect();
    let active_scrollports: HashSet<usize> = scrollports.keys().copied().collect();
    initialized.retain(|index| active_scrollports.contains(index));
    user_scrolled.retain(|index| active_scrollports.contains(index));

    let mut claimed_scrollports = HashSet::new();
    for (target_index, node) in flat.iter().enumerate() {
        if node.style.scroll_initial_target != w3cos_std::style::ScrollInitialTarget::Nearest
            || !layout::is_node_visible(flat, target_index)
        {
            continue;
        }
        let Some(scroll_index) = scroll_ancestor.get(target_index).copied().flatten() else {
            continue;
        };
        let (Some(target_rect), Some((scroll_rect, extent))) = (
            rects.get(&target_index).copied(),
            scrollports.get(&scroll_index).copied(),
        ) else {
            continue;
        };
        if !claimed_scrollports.insert(scroll_index)
            || initialized.contains(&scroll_index)
            || user_scrolled.contains(&scroll_index)
        {
            continue;
        }
        let y = initial_scroll_target_offset(target_rect.y, scroll_rect.y, extent.max_y);
        let x = scroll_offsets
            .get(&scroll_index)
            .map(|(x, _)| *x)
            .unwrap_or(0.0);
        scroll_offsets.insert(scroll_index, (x, y));
        initialized.insert(scroll_index);
    }
}

fn initial_scroll_target_offset(target_y: f32, scrollport_y: f32, max_y: f32) -> f32 {
    (target_y - scrollport_y).clamp(0.0, max_y)
}

#[cfg(test)]
mod scroll_physics_tests {
    use super::*;

    #[test]
    fn touch_velocity_uses_recent_motion_window_instead_of_last_delta() {
        let now = Instant::now();
        let samples = VecDeque::from([
            (now - Duration::from_millis(120), 300.0),
            (now - Duration::from_millis(40), 200.0),
            (now - Duration::from_millis(5), 198.0),
        ]);

        let velocity = estimate_touch_velocity(&samples, now).unwrap();
        assert!(velocity > 800.0);
    }

    #[test]
    fn stale_touch_motion_does_not_start_kinetic_scroll() {
        let now = Instant::now();
        let samples = VecDeque::from([
            (now - Duration::from_millis(300), 300.0),
            (now - Duration::from_millis(200), 200.0),
        ]);

        assert_eq!(estimate_touch_velocity(&samples, now), None);
    }

    #[test]
    fn stale_offset_above_new_extent_does_not_consume_kinetic_delta() {
        let (base, next, applied) = bounded_scroll_step(140.0, -20.0, 100.0);
        assert_eq!(base, 100.0);
        assert_eq!(next, 80.0);
        assert_eq!(applied, -20.0);
    }

    #[test]
    fn large_offset_rounding_is_not_treated_as_a_scroll_boundary() {
        let requested = -23.666_f32;
        let (_, _, applied) = bounded_scroll_step(78_142.68, requested, 78_466.0);
        assert!((requested - applied).abs() <= SCROLL_CHAIN_EPSILON);
    }

    #[test]
    fn hidden_clip_owner_does_not_receive_scroll_chain_delta() {
        let rect = LayoutRect {
            x: 0.0,
            y: 0.0,
            width: 375.0,
            height: 600.0,
        };
        let scrollable = vec![(
            2,
            rect,
            ScrollExtent {
                max_x: 0.0,
                max_y: 1_000.0,
            },
        )];
        let ancestors = vec![None, None, Some(1)];

        assert_eq!(direct_scroll_chain_parent(2, &ancestors, &scrollable), None);
    }

    #[test]
    fn direct_scrollable_owner_receives_scroll_chain_delta() {
        let rect = LayoutRect {
            x: 0.0,
            y: 0.0,
            width: 375.0,
            height: 600.0,
        };
        let scrollable = vec![
            (
                1,
                rect,
                ScrollExtent {
                    max_x: 0.0,
                    max_y: 500.0,
                },
            ),
            (
                2,
                rect,
                ScrollExtent {
                    max_x: 0.0,
                    max_y: 1_000.0,
                },
            ),
        ];
        let ancestors = vec![None, None, Some(1)];

        assert_eq!(
            direct_scroll_chain_parent(2, &ancestors, &scrollable),
            Some(1)
        );
    }

    #[test]
    fn initial_scroll_target_aligns_block_start() {
        assert_eq!(initial_scroll_target_offset(640.0, 80.0, 2_000.0), 560.0);
    }

    #[test]
    fn initial_scroll_target_clamps_tail_to_scroll_end() {
        assert_eq!(
            initial_scroll_target_offset(2_400.0, 80.0, 2_000.0),
            2_000.0
        );
    }

    #[test]
    fn first_initial_scroll_target_in_tree_order_wins() {
        let target_style = w3cos_std::style::Style {
            scroll_initial_target: w3cos_std::style::ScrollInitialTarget::Nearest,
            ..w3cos_std::style::Style::default()
        };
        let root = Component::root(vec![Component::column(
            w3cos_std::style::Style {
                overflow: w3cos_std::style::Overflow::Scroll,
                ..w3cos_std::style::Style::default()
            },
            vec![
                Component::boxed(target_style.clone(), vec![]),
                Component::boxed(target_style, vec![]),
            ],
        )]);
        let flat = layout::pre_flatten(&root);
        let layout_cache = vec![
            (
                LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: 375.0,
                    height: 500.0,
                },
                1,
            ),
            (
                LayoutRect {
                    x: 0.0,
                    y: 700.0,
                    width: 1.0,
                    height: 1.0,
                },
                2,
            ),
            (
                LayoutRect {
                    x: 0.0,
                    y: 1_400.0,
                    width: 1.0,
                    height: 1.0,
                },
                3,
            ),
        ];
        let scrollable = vec![(
            1,
            layout_cache[0].0,
            ScrollExtent {
                max_x: 0.0,
                max_y: 1_000.0,
            },
        )];
        let ancestors = vec![None, None, Some(1), Some(1)];
        let mut offsets = HashMap::new();
        let mut initialized = HashSet::new();
        let mut user_scrolled = HashSet::new();

        apply_initial_scroll_targets(
            &flat,
            &layout_cache,
            &scrollable,
            &ancestors,
            &mut offsets,
            &mut initialized,
            &mut user_scrolled,
        );

        assert_eq!(offsets.get(&1), Some(&(0.0, 700.0)));
        assert!(initialized.contains(&1));
    }

    #[test]
    fn late_initial_scroll_target_does_not_override_user_scroll() {
        let target_style = w3cos_std::style::Style {
            scroll_initial_target: w3cos_std::style::ScrollInitialTarget::Nearest,
            ..w3cos_std::style::Style::default()
        };
        let root = Component::root(vec![Component::column(
            w3cos_std::style::Style::default(),
            vec![Component::boxed(target_style, vec![])],
        )]);
        let flat = layout::pre_flatten(&root);
        let viewport = LayoutRect {
            x: 0.0,
            y: 0.0,
            width: 375.0,
            height: 500.0,
        };
        let layout_cache = vec![(viewport, 1), (viewport, 2)];
        let scrollable = vec![(
            1,
            viewport,
            ScrollExtent {
                max_x: 0.0,
                max_y: 1_000.0,
            },
        )];
        let ancestors = vec![None, None, Some(1)];
        let mut offsets = HashMap::from([(1, (0.0, 120.0))]);
        let mut initialized = HashSet::new();
        let mut user_scrolled = HashSet::from([1]);

        apply_initial_scroll_targets(
            &flat,
            &layout_cache,
            &scrollable,
            &ancestors,
            &mut offsets,
            &mut initialized,
            &mut user_scrolled,
        );

        assert_eq!(offsets.get(&1), Some(&(0.0, 120.0)));
        assert!(!initialized.contains(&1));
    }

    #[test]
    fn sticky_scroll_follows_flow_then_clamps_to_top_inset() {
        assert_eq!(clamp_sticky_scroll_offset(180.0, 80.0, 12.0, 40.0), 40.0);
        assert_eq!(clamp_sticky_scroll_offset(180.0, 80.0, 12.0, 140.0), 88.0);
    }

    #[test]
    fn sticky_nested_scroller_wins_over_feed_at_visual_position() {
        let extent = ScrollExtent {
            max_x: 0.0,
            max_y: 1_000.0,
        };
        let scrollable = vec![
            (
                0,
                LayoutRect {
                    x: 0.0,
                    y: 80.0,
                    width: 375.0,
                    height: 700.0,
                },
                extent,
            ),
            (
                1,
                LayoutRect {
                    x: 20.0,
                    y: 600.0,
                    width: 335.0,
                    height: 400.0,
                },
                extent,
            ),
        ];
        let scroll_info = vec![
            None,
            Some((
                0.0,
                500.0,
                LayoutRect {
                    x: 20.0,
                    y: 100.0,
                    width: 335.0,
                    height: 400.0,
                },
            )),
        ];

        assert_eq!(
            topmost_scroll_node_at(100.0, 180.0, &scrollable, &scroll_info, &[0, 20], &[]),
            Some(1)
        );
        assert_eq!(
            topmost_scroll_node_at(10.0, 180.0, &scrollable, &scroll_info, &[0, 20], &[]),
            Some(0)
        );
    }

    #[test]
    fn overlay_without_scroll_extent_blocks_page_scroll_below() {
        let extent = ScrollExtent {
            max_x: 0.0,
            max_y: 1_000.0,
        };
        let feed = vec![(
            0,
            LayoutRect {
                x: 0.0,
                y: 80.0,
                width: 375.0,
                height: 700.0,
            },
            extent,
        )];
        let drawer = [(
            1,
            LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 300.0,
                height: 800.0,
            },
        )];

        assert_eq!(
            topmost_scroll_node_at(150.0, 400.0, &feed, &[None], &[0, 100], &drawer),
            None
        );
        assert_eq!(
            topmost_scroll_node_at(350.0, 400.0, &feed, &[None], &[0, 100], &drawer),
            Some(0)
        );
    }

    #[test]
    fn clip_only_descendants_follow_their_sticky_owner() {
        let mut feed_style = w3cos_std::Style::default();
        feed_style.overflow = w3cos_std::style::Overflow::Scroll;
        let mut sticky_style = w3cos_std::Style::default();
        sticky_style.position = w3cos_std::style::Position::Sticky;
        sticky_style.top = w3cos_std::style::Dimension::Px(0.0);
        let mut clip_style = w3cos_std::Style::default();
        clip_style.overflow = w3cos_std::style::Overflow::Hidden;
        let root = Component::root(vec![Component::column(
            feed_style,
            vec![Component::column(
                sticky_style,
                vec![Component::column(
                    clip_style,
                    vec![Component::column(w3cos_std::Style::default(), vec![])],
                )],
            )],
        )]);
        let flat = layout::pre_flatten(&root);
        let feed_rect = LayoutRect {
            x: 0.0,
            y: 100.0,
            width: 375.0,
            height: 600.0,
        };
        let sticky_rect = LayoutRect {
            x: 10.0,
            y: 300.0,
            width: 355.0,
            height: 180.0,
        };
        let clip_rect = sticky_rect;
        let layout_cache = vec![
            (feed_rect, 1),
            (sticky_rect, 2),
            (clip_rect, 3),
            (clip_rect, 4),
        ];
        let scrollable = vec![(
            1,
            feed_rect,
            ScrollExtent {
                max_x: 0.0,
                max_y: 1_000.0,
            },
        )];
        let scroll_ancestor = vec![None, None, Some(1), Some(1), Some(3)];
        let scroll_info = build_scroll_info_fast(
            &scroll_ancestor,
            &scrollable,
            &[(3, clip_rect)],
            &HashMap::from([(1, (0.0, 250.0))]),
            &HashMap::new(),
            &layout_cache,
            &flat,
            None,
            375.0,
            700.0,
        );

        let (_, sy, visual_clip) = scroll_info[4].unwrap();
        assert_eq!(sy, 200.0);
        assert_eq!(visual_clip.y, 100.0);

        let bouncing_scroll_info = build_scroll_info_fast(
            &scroll_ancestor,
            &scrollable,
            &[(3, clip_rect)],
            &HashMap::from([(1, (0.0, 250.0))]),
            &HashMap::from([(
                1,
                OverscrollState {
                    displacement_y: 30.0,
                    velocity_y: 0.0,
                },
            )]),
            &layout_cache,
            &flat,
            None,
            375.0,
            700.0,
        );
        let (_, bouncing_sy, bouncing_clip) = bouncing_scroll_info[4].unwrap();
        assert_eq!(bouncing_sy, 170.0);
        assert_eq!(bouncing_clip.y, 130.0);
    }

    #[test]
    fn flip_transition_starts_at_previous_layout_position() {
        let mut style = w3cos_std::Style::default();
        style.transition = Some(w3cos_std::style::Transition {
            property: TransitionProperty::Transform,
            duration_ms: 260,
            easing: Easing::EaseOut,
            delay_ms: 0,
        });
        let root = Component::column(style, vec![]);
        let flat = layout::pre_flatten(&root);
        let old = vec![(
            LayoutRect {
                x: 10.0,
                y: 500.0,
                width: 355.0,
                height: 60.0,
            },
            0,
        )];
        let new = vec![(
            LayoutRect {
                x: 10.0,
                y: 200.0,
                width: 355.0,
                height: 60.0,
            },
            0,
        )];
        let mut animations = Vec::new();

        let viewport = ViewportLayout {
            layout_w: 375.0,
            layout_h: 700.0,
            offset_y: 0.0,
            keyboard_inset_bottom: 0.0,
        };
        App::collect_layout_transition_animations(
            &mut animations,
            &flat,
            &new,
            &old,
            viewport,
            viewport,
        );

        assert_eq!(animations.len(), 1);
        match &animations[0] {
            ActiveAnimation::Transform { from, to, .. } => {
                assert_eq!(from.translate_y, 300.0);
                assert_eq!(to.translate_y, 0.0);
            }
            _ => panic!("expected FLIP transform animation"),
        }
    }

    #[test]
    fn animated_parent_translation_moves_entire_subtree() {
        let root = Component::root(vec![Component::column(
            w3cos_std::Style::default(),
            vec![Component::text("drawer child", w3cos_std::Style::default())],
        )]);
        let flat = layout::pre_flatten(&root);
        let now = Instant::now();
        let animations = vec![ActiveAnimation::Transform {
            target_id: flat[1].stable_id,
            node_index: 1,
            from: Transform2D {
                translate_x: -300.0,
                ..Transform2D::IDENTITY
            },
            to: Transform2D::IDENTITY,
            start: now,
            duration_ms: 280.0,
            delay_ms: 0.0,
            easing: Easing::Linear,
        }];

        let overrides = animated_style_overrides(&flat, &animations, now);

        assert_eq!(overrides[&1].transform.translate_x, -300.0);
        assert_eq!(overrides[&2].transform.translate_x, -300.0);
    }

    #[test]
    fn layout_transition_interpolates_height_without_scaling_content() {
        let mut style = w3cos_std::Style::default();
        style.transition = Some(w3cos_std::style::Transition {
            property: TransitionProperty::All,
            duration_ms: 240,
            easing: Easing::Linear,
            delay_ms: 0,
        });
        let root = Component::column(style, vec![]);
        let flat = layout::pre_flatten(&root);
        let old = vec![(
            LayoutRect {
                x: 10.0,
                y: 100.0,
                width: 355.0,
                height: 180.0,
            },
            0,
        )];
        let new = vec![(
            LayoutRect {
                x: 10.0,
                y: 100.0,
                width: 355.0,
                height: 52.0,
            },
            0,
        )];
        let viewport = ViewportLayout {
            layout_w: 375.0,
            layout_h: 700.0,
            offset_y: 0.0,
            keyboard_inset_bottom: 0.0,
        };
        let mut animations = Vec::new();

        App::collect_layout_transition_animations(
            &mut animations,
            &flat,
            &new,
            &old,
            viewport,
            viewport,
        );

        assert_eq!(animations.len(), 1);
        match &animations[0] {
            ActiveAnimation::LayoutHeight { from, to, .. } => {
                assert_eq!(*from, 180.0);
                assert_eq!(*to, 52.0);
            }
            _ => panic!("expected layout height animation"),
        }

        let now = Instant::now();
        if let ActiveAnimation::LayoutHeight { start, .. } = &mut animations[0] {
            *start = now - std::time::Duration::from_millis(120);
        }
        let animated = animated_layout_cache(&new, &animations, now).unwrap();
        assert!((animated[0].0.height - 116.0).abs() < 0.01);
    }

    #[test]
    fn layout_transition_retargets_from_current_sample() {
        let mut style = w3cos_std::Style::default();
        style.transition = Some(w3cos_std::style::Transition {
            property: TransitionProperty::All,
            duration_ms: 240,
            easing: Easing::Linear,
            delay_ms: 0,
        });
        let root = Component::column(style, vec![]);
        let flat = layout::pre_flatten(&root);
        let expanded = vec![(
            LayoutRect {
                x: 10.0,
                y: 100.0,
                width: 355.0,
                height: 180.0,
            },
            0,
        )];
        let compact = vec![(
            LayoutRect {
                x: 10.0,
                y: 100.0,
                width: 355.0,
                height: 52.0,
            },
            0,
        )];
        let viewport = ViewportLayout {
            layout_w: 375.0,
            layout_h: 700.0,
            offset_y: 0.0,
            keyboard_inset_bottom: 0.0,
        };
        let now = Instant::now();
        let mut animations = Vec::new();

        App::collect_layout_transition_animations(
            &mut animations,
            &flat,
            &compact,
            &expanded,
            viewport,
            viewport,
        );
        if let ActiveAnimation::LayoutHeight { start, .. } = &mut animations[0] {
            *start = now - std::time::Duration::from_millis(120);
        }
        App::collect_layout_transition_animations(
            &mut animations,
            &flat,
            &expanded,
            &compact,
            viewport,
            viewport,
        );

        assert_eq!(animations.len(), 1);
        match animations[0] {
            ActiveAnimation::LayoutHeight { from, to, .. } => {
                assert!((from - 116.0).abs() < 1.0, "retargeted from={from}");
                assert_eq!(to, 180.0);
            }
            _ => panic!("expected retargeted layout height animation"),
        }
    }

    #[test]
    fn layout_transition_pairs_conditional_sibling_replacement() {
        let transition = Some(w3cos_std::style::Transition {
            property: TransitionProperty::All,
            duration_ms: 520,
            easing: Easing::EaseInOut,
            delay_ms: 0,
        });
        let mut compact_style = w3cos_std::Style::default();
        compact_style.height = Dimension::Px(52.0);
        compact_style.transition = transition.clone();
        let mut card_style = w3cos_std::Style::default();
        card_style.display = w3cos_std::style::Display::None;
        card_style.transition = transition;
        let root = Component::column(
            Default::default(),
            vec![
                Component::row(compact_style, vec![]),
                Component::column(card_style, vec![]),
            ],
        );
        let flat = layout::pre_flatten(&root);
        let old = vec![(
            LayoutRect {
                x: 10.0,
                y: 100.0,
                width: 355.0,
                height: 180.0,
            },
            2,
        )];
        let new = vec![(
            LayoutRect {
                x: 10.0,
                y: 100.0,
                width: 355.0,
                height: 52.0,
            },
            1,
        )];
        let viewport = ViewportLayout {
            layout_w: 375.0,
            layout_h: 700.0,
            offset_y: 0.0,
            keyboard_inset_bottom: 0.0,
        };
        let mut animations = Vec::new();

        App::collect_layout_transition_animations(
            &mut animations,
            &flat,
            &new,
            &old,
            viewport,
            viewport,
        );

        assert!(matches!(
            animations.as_slice(),
            [ActiveAnimation::LayoutHeight {
                node_index: 1,
                from: 180.0,
                to: 52.0,
                ..
            }]
        ));
    }

    #[test]
    fn sticky_show_collapse_does_not_retain_leaving_branch_height() {
        let transition = Some(w3cos_std::style::Transition {
            property: TransitionProperty::All,
            duration_ms: 280,
            easing: Easing::EaseOut,
            delay_ms: 0,
        });
        let mut compact_style = w3cos_std::Style::default();
        compact_style.height = Dimension::Px(52.0);
        compact_style.transition = transition.clone();
        let mut expanded_style = w3cos_std::Style::default();
        expanded_style.display = w3cos_std::style::Display::None;
        expanded_style.transition = transition;
        let mut sticky_style = w3cos_std::Style::default();
        sticky_style.position = Position::Sticky;
        let root = Component::column(
            Default::default(),
            vec![Component::column(
                sticky_style,
                vec![
                    Component::row(compact_style, vec![]),
                    Component::column(expanded_style, vec![]),
                ],
            )],
        );
        let flat = layout::pre_flatten(&root);
        let old = vec![(
            LayoutRect {
                x: 10.0,
                y: 100.0,
                width: 355.0,
                height: 520.0,
            },
            3,
        )];
        let new = vec![(
            LayoutRect {
                x: 10.0,
                y: 100.0,
                width: 355.0,
                height: 52.0,
            },
            2,
        )];
        let viewport = ViewportLayout {
            layout_w: 375.0,
            layout_h: 700.0,
            offset_y: 0.0,
            keyboard_inset_bottom: 0.0,
        };
        let mut animations = Vec::new();

        App::collect_layout_transition_animations(
            &mut animations,
            &flat,
            &new,
            &old,
            viewport,
            viewport,
        );

        assert!(
            animations
                .iter()
                .all(|animation| animation.property() != AnimatedProperty::LayoutHeight),
            "sticky Show replacement must use its compact final height immediately"
        );
    }

    #[test]
    fn flip_transition_uses_viewport_delta_for_bottom_anchored_ime_ui() {
        let mut style = w3cos_std::Style::default();
        style.transition = Some(w3cos_std::style::Transition {
            property: TransitionProperty::Transform,
            duration_ms: 260,
            easing: Easing::EaseOut,
            delay_ms: 0,
        });
        let root = Component::column(style, vec![]);
        let flat = layout::pre_flatten(&root);
        let rect = LayoutRect {
            x: 10.0,
            y: 630.0,
            width: 355.0,
            height: 60.0,
        };
        let cache = vec![(rect, 0)];
        let old_viewport = ViewportLayout {
            layout_w: 375.0,
            layout_h: 800.0,
            offset_y: 0.0,
            keyboard_inset_bottom: 0.0,
        };
        let new_viewport = ViewportLayout {
            layout_w: 375.0,
            layout_h: 700.0,
            offset_y: 0.0,
            keyboard_inset_bottom: 100.0,
        };
        let mut animations = Vec::new();

        App::collect_layout_transition_animations(
            &mut animations,
            &flat,
            &cache,
            &cache,
            old_viewport,
            new_viewport,
        );

        match &animations[0] {
            ActiveAnimation::Transform { from, .. } => assert_eq!(from.translate_y, 100.0),
            _ => panic!("expected viewport FLIP transform animation"),
        }
    }

    #[test]
    fn keyboard_viewport_change_does_not_flip_sticky_subtree() {
        let mut sticky_style = w3cos_std::Style::default();
        sticky_style.position = Position::Sticky;
        sticky_style.top = Dimension::Px(0.0);
        let mut card_style = w3cos_std::Style::default();
        card_style.transition = Some(w3cos_std::style::Transition {
            property: TransitionProperty::All,
            duration_ms: 280,
            easing: Easing::EaseOut,
            delay_ms: 0,
        });
        let root = Component::root(vec![Component::column(
            sticky_style,
            vec![Component::column(card_style, vec![])],
        )]);
        let flat = layout::pre_flatten(&root);
        let old = vec![(
            LayoutRect {
                x: 16.0,
                y: 280.0,
                width: 343.0,
                height: 180.0,
            },
            2,
        )];
        let new = vec![(
            LayoutRect {
                x: 16.0,
                y: 120.0,
                width: 343.0,
                height: 180.0,
            },
            2,
        )];
        let old_viewport = ViewportLayout {
            layout_w: 375.0,
            layout_h: 812.0,
            offset_y: 0.0,
            keyboard_inset_bottom: 0.0,
        };
        let new_viewport = ViewportLayout {
            layout_w: 375.0,
            layout_h: 479.0,
            offset_y: 0.0,
            keyboard_inset_bottom: 333.0,
        };
        let mut animations = vec![ActiveAnimation::Transform {
            target_id: flat[2].stable_id,
            node_index: 2,
            from: Transform2D {
                translate_y: 24.0,
                ..Default::default()
            },
            to: Transform2D::default(),
            start: Instant::now(),
            duration_ms: 280.0,
            delay_ms: 0.0,
            easing: Easing::EaseOut,
        }];

        App::collect_layout_transition_animations(
            &mut animations,
            &flat,
            &new,
            &old,
            old_viewport,
            new_viewport,
        );

        assert!(animations.is_empty());
    }

    #[test]
    fn sticky_markers_accumulate_after_crossing_scrollport_top() {
        let mut first = Component::column(Default::default(), vec![]);
        first.sticky_counter_signal = Some(7);
        let mut second = Component::column(Default::default(), vec![]);
        second.sticky_counter_signal = Some(7);
        let root = Component::column(Default::default(), vec![first, second]);
        let flat = layout::pre_flatten(&root);
        let layout_cache = vec![
            (
                LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: 375.0,
                    height: 700.0,
                },
                0,
            ),
            (
                LayoutRect {
                    x: 0.0,
                    y: 100.0,
                    width: 335.0,
                    height: 80.0,
                },
                1,
            ),
            (
                LayoutRect {
                    x: 0.0,
                    y: 300.0,
                    width: 335.0,
                    height: 80.0,
                },
                2,
            ),
        ];
        let scroll_ancestor = vec![None, Some(0), Some(0)];
        let index = build_sticky_marker_index(&flat, &layout_cache, &scroll_ancestor);

        let before = sticky_marker_counts(&index, 0, 0.0, 50.0);
        assert_eq!(before.get(&7), Some(&0));

        let after_first = sticky_marker_counts(&index, 0, 0.0, 150.0);
        assert_eq!(after_first.get(&7), Some(&1));

        let after_both = sticky_marker_counts(&index, 0, 0.0, 350.0);
        assert_eq!(after_both.get(&7), Some(&2));
    }

    #[test]
    fn paint_cull_applies_scroll_offset_before_viewport_test() {
        let scroll_info = vec![Some((
            0.0,
            900.0,
            LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 375.0,
                height: 700.0,
            },
        ))];
        assert!(node_intersects_paint_cull(
            0,
            LayoutRect {
                x: 20.0,
                y: 1_000.0,
                width: 335.0,
                height: 80.0,
            },
            &scroll_info,
            375.0,
            812.0,
            0.0,
        ));
    }

    #[test]
    fn paint_cull_rejects_offscreen_scrolled_node() {
        let scroll_info = vec![Some((
            0.0,
            900.0,
            LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 375.0,
                height: 700.0,
            },
        ))];
        assert!(!node_intersects_paint_cull(
            0,
            LayoutRect {
                x: 20.0,
                y: 2_000.0,
                width: 335.0,
                height: 80.0,
            },
            &scroll_info,
            375.0,
            812.0,
            0.0,
        ));
    }

    #[test]
    fn paint_cull_respects_scrollport_clip() {
        let scroll_info = vec![Some((
            0.0,
            0.0,
            LayoutRect {
                x: 0.0,
                y: 200.0,
                width: 375.0,
                height: 300.0,
            },
        ))];
        assert!(!node_intersects_paint_cull(
            0,
            LayoutRect {
                x: 20.0,
                y: 40.0,
                width: 335.0,
                height: 80.0,
            },
            &scroll_info,
            375.0,
            812.0,
            0.0,
        ));
    }

    #[test]
    fn overlay_scroll_damage_requires_composed_repaint() {
        let damage = ScrollDamage {
            index: 2,
            delta_y: 24.0,
        };
        let scrollable = [(
            2,
            LayoutRect {
                x: 0.0,
                y: 0.0,
                width: 300.0,
                height: 600.0,
            },
            ScrollExtent {
                max_x: 0.0,
                max_y: 1_000.0,
            },
        )];
        assert!(!scroll_damage_crosses_stacking_context(
            &[damage],
            &[0, 0, 0],
            &scrollable,
            &[(2, scrollable[0].1)]
        ));
        assert!(scroll_damage_crosses_stacking_context(
            &[damage],
            &[0, 100, 0],
            &scrollable,
            &[(
                1,
                LayoutRect {
                    x: 0.0,
                    y: 0.0,
                    width: 240.0,
                    height: 600.0,
                }
            )]
        ));
    }

    #[test]
    fn component_virtual_list_materializes_only_viewport_window() {
        let mut list_style = w3cos_std::style::Style::default();
        list_style.height = Dimension::Px(500.0);
        list_style.overflow = w3cos_std::style::Overflow::Scroll;
        let mut item_style = w3cos_std::style::Style::default();
        item_style.height = Dimension::Px(50.0);
        item_style.flex_shrink = 0.0;
        let virtual_list = Component::virtual_list(
            1_000,
            50.0,
            100.0,
            list_style,
            Component::text("row-{index}", item_style),
        );
        let root = Component::root(vec![virtual_list]);
        let mut app = App::new_static(root);

        assert!(app.materialize_virtual_list(0, 500.0, 20_000.0));
        let flat = layout::pre_flatten(&app.root);
        assert!(flat.len() < 25, "only the overscanned window is mounted");
        assert_eq!(app.virtual_lists[&0].engine.mounted_len(), 15);
        assert_eq!(app.virtual_lists[&0].engine.total_extent(), 50_000.0);
        assert!(flat.iter().any(|node| matches!(
            node.kind,
            ComponentKind::Text { content } if content == "row-398"
        )));

        let results = layout::compute_with_scroll(&app.root, 375.0, 500.0).unwrap();
        assert!(!results.1.is_empty(), "layout={:?}", results.0);
        assert!(results.1[0].2.max_y > 49_000.0);
    }

    #[test]
    fn react_tree_full_repaint_is_not_downgraded_by_scroll_damage() {
        let mut invalidated = RepaintMode::Full;
        invalidated.queue_scroll_damage(7, 84.0);
        assert!(matches!(invalidated, RepaintMode::Full));

        let mut clean = RepaintMode::Clean;
        clean.queue_scroll_damage(7, 84.0);
        assert!(matches!(
            clean,
            RepaintMode::ScrollOnly(ref damages)
                if damages.len() == 1 && damages[0].index == 7 && damages[0].delta_y == 84.0
        ));
    }
}

pub fn run_reactive(builder: fn() -> Component) -> Result<()> {
    let event_loop = EventLoop::new()?;
    let mut app = App::new_reactive(builder);
    event_loop.run_app(&mut app)?;
    Ok(())
}

pub fn run_static(root: Component) -> Result<()> {
    let event_loop = EventLoop::new()?;
    let mut app = App::new_static(root);
    event_loop.run_app(&mut app)?;
    Ok(())
}

pub fn run_dom(setup: fn()) -> Result<()> {
    let event_loop = EventLoop::new()?;
    let mut app = App::new_dom(setup);
    event_loop.run_app(&mut app)?;
    Ok(())
}

#[cfg(target_os = "android")]
pub fn run_reactive_android(
    android_app: winit::platform::android::activity::AndroidApp,
    builder: fn() -> Component,
) -> Result<()> {
    use winit::platform::android::EventLoopBuilderExtAndroid;
    let event_loop = EventLoop::builder().with_android_app(android_app).build()?;
    let mut app = App::new_reactive(builder);
    event_loop.run_app(&mut app)?;
    Ok(())
}
