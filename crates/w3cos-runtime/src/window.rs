use anyhow::Result;
use std::collections::HashMap;
use std::time::Instant;

#[cfg(feature = "cpu-render")]
use std::rc::Rc;

#[cfg(feature = "gpu")]
use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::event::{
    ElementState, MouseButton, MouseScrollDelta, StartCause, TouchPhase, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::dpi::LogicalSize;
use winit::window::{Window, WindowAttributes, WindowId};

/// Logical viewport for layout — matches compare page (iPhone 17 Pro).
fn default_logical_size() -> LogicalSize<f64> {
    #[cfg(any(target_os = "ios", target_os = "android"))]
    {
        LogicalSize::new(402.0, 874.0)
    }
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    {
        LogicalSize::new(1200.0, 800.0)
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
    fn from_window(window: &Window, scale: f32, inset_top: f32, ime_open: bool) -> Self {
        let size = window.inner_size();
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
                keyboard_inset_bottom =
                    (full_h - visible_h - rect.top as f32 / scale).max(0.0);
            }
            if ime_open && keyboard_inset_bottom < 8.0 {
                keyboard_inset_bottom = ANDROID_IME_FALLBACK_INSET;
            }
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
            matches!(
                app.get_kind_at(idx),
                Some(ComponentKind::TextInput { .. })
            )
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
use vello::{AaConfig, Renderer, RendererOptions, Scene};
#[cfg(feature = "gpu")]
use vello::wgpu;

use crate::compositor::lerp_transform;
use crate::layout::{self, LayoutEngine, LayoutRect, ScrollExtent};
#[cfg(feature = "gpu")]
use crate::render_gpu;
#[cfg(feature = "cpu-render")]
use crate::render_cpu;
use crate::state;
use w3cos_std::color::Color;
use w3cos_std::style::{Easing, Transform2D, TransitionProperty};
use w3cos_std::{Component, ComponentKind, EventAction};

#[cfg(any(target_os = "ios", target_os = "android"))]
static EMBEDDED_FONT: &[u8] = include_bytes!("../assets/CJK-Subset.ttf");
#[cfg(not(any(target_os = "ios", target_os = "android")))]
static EMBEDDED_FONT: &[u8] = include_bytes!("../assets/Inter-Regular.ttf");

const ANIMATION_FRAME_INTERVAL_MS: u64 = 16;

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

#[derive(Clone, Default)]
enum RepaintMode {
    #[default]
    Full,
    ScrollOnly(Vec<usize>),
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

    fn query(&self, x: f32, y: f32, hit_nodes: &[HitNode], parents: &[Option<usize>]) -> Option<usize> {
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
    Opacity {
        node_index: usize,
        from: f32,
        to: f32,
        start: Instant,
        duration_ms: f64,
        delay_ms: f64,
        easing: Easing,
    },
    Background {
        node_index: usize,
        from: Color,
        to: Color,
        start: Instant,
        duration_ms: f64,
        delay_ms: f64,
        easing: Easing,
    },
    Transform {
        node_index: usize,
        from: Transform2D,
        to: Transform2D,
        start: Instant,
        duration_ms: f64,
        delay_ms: f64,
        easing: Easing,
    },
}

impl ActiveAnimation {
    fn node_index(&self) -> usize {
        match self {
            ActiveAnimation::Opacity { node_index, .. } => *node_index,
            ActiveAnimation::Background { node_index, .. } => *node_index,
            ActiveAnimation::Transform { node_index, .. } => *node_index,
        }
    }

    fn progress(&self, now: Instant) -> f32 {
        let elapsed_ms = now
            .duration_since(match self {
                ActiveAnimation::Opacity { start, .. } => *start,
                ActiveAnimation::Background { start, .. } => *start,
                ActiveAnimation::Transform { start, .. } => *start,
            })
            .as_secs_f64()
            * 1000.0;
        let delay_ms = match self {
            ActiveAnimation::Opacity { delay_ms, .. } => *delay_ms,
            ActiveAnimation::Background { delay_ms, .. } => *delay_ms,
            ActiveAnimation::Transform { delay_ms, .. } => *delay_ms,
        };
        let duration_ms = match self {
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
    buffer_size: (u32, u32),
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
    text_input_values: HashMap<usize, String>,
    hit_nodes: Vec<HitNode>,
    focusable_indices: Vec<usize>,
    layout_cache: Vec<(LayoutRect, usize)>,
    scrollable_nodes: Vec<(usize, LayoutRect, ScrollExtent)>,
    clip_only_nodes: Vec<(usize, LayoutRect)>,
    scroll_offsets: HashMap<usize, (f32, f32)>,
    needs_layout: bool,
    needs_tree_rebuild: bool,
    needs_style_refresh: bool,
    animations: Vec<ActiveAnimation>,
    last_frame_time: Option<Instant>,
    modifiers: ModifiersState,
    last_touch_y: Option<f32>,
    touch_drag_y: f32,
    touch_scroll_active: bool,
    content_inset_top: f32,
    viewport: ViewportLayout,
    repaint_mode: RepaintMode,

    /// UA presenter selection when both GPU and CPU backends are compiled in.
    #[cfg(all(feature = "gpu", feature = "cpu-render"))]
    using_gpu: bool,

    // Performance: persistent layout engine (avoids TaffyTree rebuild on resize)
    layout_engine: LayoutEngine,
    // Performance: scroll ancestor map (avoids O(n*depth) parent walk)
    scroll_ancestor: Vec<Option<usize>>,
    flat_parents: Vec<Option<usize>>,
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
            text_input_values: HashMap::new(),
            hit_nodes: Vec::new(),
            focusable_indices: Vec::new(),
            layout_cache: Vec::new(),
            scrollable_nodes: Vec::new(),
            clip_only_nodes: Vec::new(),
            scroll_offsets: HashMap::new(),
            needs_layout: true,
            needs_tree_rebuild: true,
            needs_style_refresh: false,
            animations: Vec::new(),
            last_frame_time: None,
            modifiers: ModifiersState::default(),
            last_touch_y: None,
            touch_drag_y: 0.0,
            touch_scroll_active: false,
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
            #[cfg(all(feature = "gpu", feature = "cpu-render"))]
            using_gpu: true,

            layout_engine: LayoutEngine::new(),
            scroll_ancestor: Vec::new(),
            flat_parents: Vec::new(),
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
            return;
        }
        #[cfg(all(feature = "gpu", not(feature = "cpu-render")))]
        self.paint_gpu();
        #[cfg(all(feature = "cpu-render", not(feature = "gpu")))]
        self.paint_cpu();
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
        let attrs = WindowAttributes::default()
            .with_title("W3C OS")
            .with_inner_size(default_logical_size());
        let window = Rc::new(event_loop.create_window(attrs).unwrap());
        self.scale_factor = window.scale_factor();
        let context =
            softbuffer::Context::new(event_loop.owned_display_handle()).expect("softbuffer context");
        let surface =
            softbuffer::Surface::new(&context, window.clone()).expect("softbuffer surface");
        self.cpu = Some(CpuPresenter {
            window,
            context,
            surface,
            framebuffer: None,
            buffer_size: (0, 0),
        });
        self.needs_layout = true;
    }

    fn rebuild_if_dirty(&mut self) {
        let signal_dirty = state::is_dirty();
        let dom_dirty = self.dom_mode && crate::dom::is_document_dirty();

        if !signal_dirty && !dom_dirty {
            return;
        }

        let old_root = self.root.clone();

        if signal_dirty {
            state::clear_dirty();
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
            self.needs_layout = true;
            self.needs_tree_rebuild = !layout::layout_shape_unchanged(&old_flat, &new_flat);
            self.needs_style_refresh = !self.needs_tree_rebuild
                && !layout::layout_display_unchanged(&old_flat, &new_flat);
            self.repaint_mode = RepaintMode::Full;
            self.hovered_index = None;
            self.pressed_index = None;
            self.collect_transition_animations(&old_root);
        }
    }

    fn collect_transition_animations(&mut self, old_root: &Component) {
        let old_flat = layout::pre_flatten(old_root);
        let new_flat = layout::pre_flatten(&self.root);
        let now = Instant::now();

        for (idx, (old_node, new_node)) in old_flat.iter().zip(new_flat.iter()).enumerate() {
            let Some(transition) = &new_node.style.transition else {
                continue;
            };
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

            if animates_opacity && old_node.style.opacity != new_node.style.opacity {
                self.animations.push(ActiveAnimation::Opacity {
                    node_index: idx,
                    from: old_node.style.opacity,
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
                self.animations.push(ActiveAnimation::Background {
                    node_index: idx,
                    from: old_node.style.background,
                    to: new_node.style.background,
                    start: now,
                    duration_ms,
                    delay_ms,
                    easing,
                });
            }
            if animates_transform && old_node.style.transform != new_node.style.transform {
                self.animations.push(ActiveAnimation::Transform {
                    node_index: idx,
                    from: old_node.style.transform,
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
        let size = window.inner_size();
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

        if !self.needs_layout
            && !self.layout_cache.is_empty()
            && self.viewport == viewport
        {
            return;
        }
        self.viewport = viewport;

        let w = viewport.layout_w;
        let layout_h = viewport.layout_h;
        let layout_offset_y = viewport.offset_y;

        let flat = layout::pre_flatten(&self.root);

        if self.needs_tree_rebuild {
            self.layout_engine.invalidate();
            self.needs_tree_rebuild = false;
            self.needs_style_refresh = false;
        } else if self.needs_style_refresh && self.layout_engine.tree_valid() {
            let _ = self.layout_engine.patch_display_styles(&flat);
            self.needs_style_refresh = false;
        }

        let results = self
            .layout_engine
            .compute(&self.root, &flat, w, layout_h)
            .unwrap_or_else(|_| layout::LayoutResults::empty());

        self.layout_cache = results.layout_cache;
        self.scrollable_nodes = results.scrollable_nodes;
        self.clip_only_nodes = results.clip_only_nodes;
        self.scroll_ancestor = results.scroll_ancestor;
        self.flat_parents = flat.iter().map(|n| n.parent).collect();
        offset_layout_y(layout_offset_y, &mut self.layout_cache, &mut self.scrollable_nodes, &mut self.clip_only_nodes);

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

        self.needs_layout = false;
        self.layout_generation += 1;
        self.ensure_focused_input_visible();
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
        let scroll_idx = self
            .scroll_ancestor
            .get(focus_idx)
            .copied()
            .flatten();
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

        let (ox, oy) = self.scroll_offsets.get(&scroll_idx).copied().unwrap_or((0.0, 0.0));
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
            self.repaint_mode = RepaintMode::ScrollOnly(vec![scroll_idx]);
        }
    }

    fn poll_viewport_inset(&mut self) {
        let window = match self.get_window() {
            Some(w) => w,
            None => return,
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
            self.viewport = viewport;
            self.needs_layout = true;
            self.request_repaint();
        }
    }

    // -----------------------------------------------------------------------
    // GPU paint — zero-copy via style overrides (no root.clone())
    // -----------------------------------------------------------------------
    #[cfg(feature = "gpu")]
    fn paint_gpu(&mut self) {
        self.ensure_layout();

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

        // Pre-flatten once for this frame (borrows self.root)
        let flat = layout::pre_flatten(&self.root);

        // Compute style overrides for animated/hovered nodes (only clones 0-2 styles)
        let mut style_overrides: HashMap<usize, w3cos_std::style::Style> = HashMap::new();

        for anim in &self.animations {
            let idx = anim.node_index();
            if idx >= flat.len() {
                continue;
            }
            let t = anim.progress(now);
            let eased = match anim {
                ActiveAnimation::Opacity { easing, .. } => easing.interpolate(t),
                ActiveAnimation::Background { easing, .. } => easing.interpolate(t),
                ActiveAnimation::Transform { easing, .. } => easing.interpolate(t),
            };
            let entry = style_overrides
                .entry(idx)
                .or_insert_with(|| flat[idx].style.clone());
            match anim {
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
                    entry.transform = lerp_transform(*from, *to, eased);
                }
            }
        }

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

        // Build render nodes — all in logical (CSS) pixels.
        // DPI scaling is handled by Vello's Affine::scale inside render_frame.
        let render_nodes: Vec<(usize, LayoutRect, &ComponentKind, &w3cos_std::style::Style)> = self
            .layout_cache
            .iter()
            .filter_map(|&(rect, idx)| {
                let node = flat.get(idx)?;
                let style = style_overrides.get(&idx).unwrap_or(node.style);
                Some((idx, rect, node.kind, style))
            })
            .collect();

        let scroll_info = build_scroll_info_fast(
            &self.scroll_ancestor,
            &self.scrollable_nodes,
            &self.clip_only_nodes,
            &self.scroll_offsets,
        );

        let scale = self.scale_factor as f32;

        let device_handle = &self.render_cx.devices[dev_id];
        if self.gpu_filter_pipelines.is_none() {
            self.gpu_filter_pipelines =
                Some(crate::gpu_filter::GpuFilterPipelines::new(&device_handle.device));
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
                    if matches!(node.kind, ComponentKind::Button { .. }) {
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

        // Drop borrows on self.root (via flat/render_nodes/style_overrides)
        drop(render_nodes);
        drop(style_overrides);
        drop(flat);

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
                        18.0 / 255.0,
                        18.0 / 255.0,
                        24.0 / 255.0,
                        1.0,
                    ]),
                    width,
                    height,
                    antialiasing_method: AaConfig::Msaa16,
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
    }

    // -----------------------------------------------------------------------
    // CPU paint — same zero-copy pattern
    // -----------------------------------------------------------------------
    #[cfg(feature = "cpu-render")]
    fn paint_cpu(&mut self) {
        self.ensure_layout();

        let Some(cpu_ref) = self.cpu.as_ref() else {
            return;
        };
        let window = cpu_ref.window.clone();
        let size = window.inner_size();
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

        // Compute style overrides
        let mut style_overrides: HashMap<usize, w3cos_std::style::Style> = HashMap::new();
        for anim in &self.animations {
            let idx = anim.node_index();
            if idx >= flat.len() {
                continue;
            }
            let t = anim.progress(now);
            let eased = match anim {
                ActiveAnimation::Opacity { easing, .. } => easing.interpolate(t),
                ActiveAnimation::Background { easing, .. } => easing.interpolate(t),
                ActiveAnimation::Transform { easing, .. } => easing.interpolate(t),
            };
            let entry = style_overrides
                .entry(idx)
                .or_insert_with(|| flat[idx].style.clone());
            match anim {
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
                    entry.transform = lerp_transform(*from, *to, eased);
                }
            }
        }
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

        // Scale layout rects (logical) → physical pixels for the Pixmap.
        // Also scale font_size so text renders at correct physical resolution.
        let scaled_styles: Vec<w3cos_std::style::Style> = self
            .layout_cache
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

        let render_nodes: Vec<(usize, LayoutRect, &ComponentKind, &w3cos_std::style::Style)> = self
            .layout_cache
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

        let scroll_info_raw = build_scroll_info_fast(
            &self.scroll_ancestor,
            &self.scrollable_nodes,
            &self.clip_only_nodes,
            &self.scroll_offsets,
        );
        let scroll_info: Vec<Option<(f32, f32, LayoutRect)>> = scroll_info_raw
            .iter()
            .map(|si| {
                si.map(|(sx, sy, clip)| {
                    (sx * scale, sy * scale, LayoutRect {
                        x: clip.x * scale, y: clip.y * scale,
                        width: clip.width * scale, height: clip.height * scale,
                    })
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
            RepaintMode::ScrollOnly(targets) => {
                render_cpu::render_scroll_damage(
                    &mut pixmap,
                    &render_nodes,
                    &self.font,
                    &scroll_info,
                    &self.text_input_values,
                    self.focused_index,
                    &targets,
                    &scaled_scrollable,
                    &self.scroll_ancestor,
                );
            }
            RepaintMode::Full => {
                render_cpu::render_frame(
                    &mut pixmap,
                    &render_nodes,
                    &self.font,
                    &scroll_info,
                    &self.text_input_values,
                    self.focused_index,
                );
            }
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
    }

    fn set_pointer_logical(&mut self, physical_x: f64, physical_y: f64) {
        let scale = self.scale_factor as f32;
        self.mouse_x = physical_x as f32 / scale;
        self.mouse_y = physical_y as f32 / scale;
    }

    fn update_hover_at_pointer(&mut self) {
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

    fn pointer_pressed(&mut self) {
        self.ensure_layout();
        let hit = self.hit_test(self.mouse_x, self.mouse_y);
        if let Some(idx) = hit {
            self.pressed_index = Some(idx);
            #[cfg(not(any(target_os = "ios", target_os = "android")))]
            self.request_repaint();
        } else {
            self.focused_index = None;
            #[cfg(any(target_os = "ios", target_os = "android"))]
            self.sync_soft_keyboard();
            #[cfg(not(any(target_os = "ios", target_os = "android")))]
            self.request_repaint();
        }
    }

    fn pointer_released(&mut self) {
        if let Some(pressed_idx) = self.pressed_index.take() {
            let current_hover = self.hit_test(self.mouse_x, self.mouse_y);
            if current_hover == Some(pressed_idx) {
                self.handle_click(pressed_idx);
            } else {
                self.repaint_after_interaction();
            }
        }
    }

    fn hit_test(&self, x: f32, y: f32) -> Option<usize> {
        let (lx, ly) = self.viewport_to_layout(x, y);
        self.spatial_grid
            .query(lx, ly, &self.hit_nodes, &self.flat_parents)
    }

    fn viewport_to_layout(&self, x: f32, y: f32) -> (f32, f32) {
        for (idx, rect, _) in self.scrollable_nodes.iter().rev() {
            let (sx, sy) = self.scroll_offsets.get(idx).copied().unwrap_or((0.0, 0.0));
            let vx = rect.x - sx;
            let vy = rect.y - sy;
            if x >= vx && x <= vx + rect.width && y >= vy && y <= vy + rect.height {
                return (x + sx, y + sy);
            }
        }
        (x, y)
    }

    fn hit_test_scroll(&self, x: f32, y: f32) -> Option<usize> {
        for (idx, rect, _) in self.scrollable_nodes.iter().rev() {
            if x >= rect.x && x <= rect.x + rect.width && y >= rect.y && y <= rect.y + rect.height
            {
                return Some(*idx);
            }
        }
        None
    }

    fn scroll_at_pointer(&mut self, dy: f32) {
        if dy == 0.0 {
            return;
        }
        self.ensure_layout();
        let Some(idx) = self.hit_test_scroll(self.mouse_x, self.mouse_y) else {
            return;
        };
        let Some((_rect, extent)) = self
            .scrollable_nodes
            .iter()
            .find(|(i, _, _)| *i == idx)
            .map(|(_, r, e)| (r, e))
        else {
            return;
        };
        let (ox, oy) = self.scroll_offsets.get(&idx).copied().unwrap_or((0.0, 0.0));
        let new_oy = (oy + dy).clamp(0.0, extent.max_y);
        if (new_oy - oy).abs() > 0.001 {
            self.scroll_offsets.insert(idx, (ox, new_oy));
            self.repaint_mode = RepaintMode::ScrollOnly(vec![idx]);
            self.request_repaint();
        }
    }

    fn sync_soft_keyboard(&self) {
        #[cfg(any(target_os = "android", target_os = "ios"))]
        {
            use winit::dpi::{PhysicalPosition, PhysicalSize};

            let Some(window) = self.get_window() else {
                return;
            };
            let Some(focus_idx) = self.focused_index else {
                window.set_ime_allowed(false);
                return;
            };
            let Some(kind) = self.get_kind_at(focus_idx) else {
                window.set_ime_allowed(false);
                return;
            };
            if !matches!(kind, ComponentKind::TextInput { .. }) {
                window.set_ime_allowed(false);
                return;
            }
            window.set_ime_allowed(true);
            if let Some(&(rect, idx)) = self.layout_cache.iter().find(|(_, i)| *i == focus_idx) {
                let scale = self.scale_factor as f32;
                let x = (rect.x * scale) as i32;
                let y = ((rect.y + rect.height) * scale) as i32;
                let w = (rect.width * scale).max(1.0) as u32;
                let h = (rect.height * scale).max(1.0) as u32;
                window.set_ime_cursor_area(PhysicalPosition::new(x, y), PhysicalSize::new(w, h));
            }
        }
    }

    fn handle_click(&mut self, idx: usize) {
        if let Some(hit) = self.hit_nodes.iter().find(|h| h.index == idx) {
            let kind_is_text_input = matches!(
                self.get_kind_at(idx),
                Some(ComponentKind::TextInput { .. })
            );
            let kind_is_button = matches!(
                self.get_kind_at(idx),
                Some(ComponentKind::Button { .. })
            );

            if kind_is_text_input {
                self.focused_index = Some(idx);
                self.sync_soft_keyboard();
                self.needs_layout = true;
                self.repaint_after_interaction();
                return;
            }
            if kind_is_button {
                if !hit.on_click.is_none() {
                    state::execute_action(&hit.on_click);
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
        self.sync_soft_keyboard();
        self.repaint_after_interaction();
    }

    fn repaint_after_interaction(&mut self) {
        #[cfg(target_os = "ios")]
        {
            self.paint();
            return;
        }
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
                        crate::dom::with_document(|doc| {
                            SerializedDocument::from_document(doc)
                        })
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
            ComponentKind::Root | ComponentKind::Column => ("div", None, vec![]),
            ComponentKind::Row => ("div", None, vec![]),
            ComponentKind::Box => ("div", None, vec![]),
            ComponentKind::Text { content } => ("#text", Some(content.clone()), vec![]),
            ComponentKind::Button { label } => ("button", Some(label.clone()), vec![]),
            ComponentKind::Image { src } => {
                ("img", None, vec![("src".to_string(), src.clone())])
            }
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
                let attrs = WindowAttributes::default()
                    .with_title("W3C OS")
                    .with_inner_size(default_logical_size());
                Arc::new(event_loop.create_window(attrs).unwrap())
            }),
            GpuState::Active { .. } => return true,
        };

        self.scale_factor = window.scale_factor();
        let size = window.inner_size();

        let surface = match pollster::block_on(self.render_cx.create_surface(
            window.clone(),
            size.width.max(1),
            size.height.max(1),
            wgpu::PresentMode::AutoVsync,
        )) {
            Ok(surface) => surface,
            Err(_) => return false,
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
                return false;
            }
            let init_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                Renderer::new(&dev.device, RendererOptions::default())
            }));
            match init_result {
                Ok(Ok(renderer)) => self.renderers[surface.dev_id] = Some(renderer),
                Ok(Err(_)) | Err(_) => return false,
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

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    let t = t.clamp(0.0, 1.0);
    (a as f32 + (b as f32 - a as f32) * t).round() as u8
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

/// Build scroll info using pre-computed scroll_ancestor map.
/// O(n) instead of O(n * tree_depth).
fn build_scroll_info_fast(
    scroll_ancestor: &[Option<usize>],
    scrollable: &[(usize, LayoutRect, ScrollExtent)],
    clip_only: &[(usize, LayoutRect)],
    offsets: &HashMap<usize, (f32, f32)>,
) -> Vec<Option<(f32, f32, LayoutRect)>> {
    if scroll_ancestor.is_empty() {
        return Vec::new();
    }

    let scrollable_rect: HashMap<usize, LayoutRect> =
        scrollable.iter().map(|(i, r, _)| (*i, *r)).collect();
    let clip_only_rect: HashMap<usize, LayoutRect> =
        clip_only.iter().map(|(i, r)| (*i, *r)).collect();

    scroll_ancestor
        .iter()
        .map(|ancestor| match ancestor {
            Some(anc_idx) => {
                if let Some(&clip) = scrollable_rect.get(anc_idx) {
                    let (sx, sy) = offsets.get(anc_idx).copied().unwrap_or((0.0, 0.0));
                    Some((sx, sy, clip))
                } else if let Some(&clip) = clip_only_rect.get(anc_idx) {
                    Some((0.0, 0.0, clip))
                } else {
                    None
                }
            }
            None => None,
        })
        .collect()
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
            self.poll_viewport_inset();
            event_loop.set_control_flow(ControlFlow::Poll);
            return;
        }

        let has_animations = !self.animations.is_empty();
        let timer_deadline = crate::timers::next_deadline();

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
                    Instant::now() + std::time::Duration::from_millis(ANIMATION_FRAME_INTERVAL_MS),
                ));
            }
            (false, Some(deadline)) => {
                if has_devtools {
                    let devtools_deadline =
                        Instant::now() + std::time::Duration::from_millis(100);
                    event_loop
                        .set_control_flow(ControlFlow::WaitUntil(deadline.min(devtools_deadline)));
                } else {
                    event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
                }
            }
            (true, Some(deadline)) => {
                let anim_deadline =
                    Instant::now() + std::time::Duration::from_millis(ANIMATION_FRAME_INTERVAL_MS);
                event_loop.set_control_flow(ControlFlow::WaitUntil(deadline.min(anim_deadline)));
            }
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        #[cfg(all(feature = "gpu", feature = "cpu-render"))]
        {
            // Android NativeActivity main thread must stay responsive — defer GPU probe.
            #[cfg(target_os = "android")]
            {
                self.ensure_cpu_presenter(event_loop);
                self.using_gpu = false;
            }
            #[cfg(not(target_os = "android"))]
            if self.try_init_gpu(event_loop) {
                self.using_gpu = true;
            } else {
                self.ensure_cpu_presenter(event_loop);
                self.using_gpu = false;
            }
        }

        #[cfg(all(feature = "gpu", not(feature = "cpu-render")))]
        {
            let _ = self.try_init_gpu(event_loop);
        }

        #[cfg(feature = "cpu-render")]
        {
            self.ensure_cpu_presenter(event_loop);
        }

        #[cfg(feature = "devtools")]
        {
            if self.devtools_handle.is_none() {
                let port = std::env::var("W3COS_DEVTOOLS_PORT")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(9229u16);
                self.devtools_handle =
                    Some(crate::devtools::DevToolsServer::start(port));
            }
        }

        #[cfg(feature = "ai-bridge")]
        {
            if self.ai_bridge_handle.is_none() {
                if let Ok(port_str) = std::env::var("W3COS_AI_PORT") {
                    if let Ok(port) = port_str.parse::<u16>() {
                        let provider: std::sync::Arc<dyn w3cos_ai_bridge::server::ScreenshotProvider> =
                            std::sync::Arc::new(FrameCacheScreenshot);
                        self.ai_bridge_handle = Some(
                            w3cos_ai_bridge::server::start_with_provider(port, provider),
                        );
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
                        { self.using_gpu }
                        #[cfg(all(feature = "gpu", not(feature = "cpu-render")))]
                        { true }
                        #[cfg(not(feature = "gpu"))]
                        { false }
                    };
                    if resize_gpu && _size.width > 0 && _size.height > 0 {
                        if let GpuState::Active {
                            ref mut surface, ..
                        } = self.gpu_state
                        {
                            self.render_cx
                                .resize_surface(surface, _size.width, _size.height);
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
                        self.last_touch_y = Some(self.mouse_y);
                        self.touch_drag_y = 0.0;
                        self.touch_scroll_active = false;
                        self.pointer_pressed();
                    }
                    TouchPhase::Moved => {
                        if let Some(last_y) = self.last_touch_y {
                            let dy = last_y - self.mouse_y;
                            self.touch_drag_y += dy.abs();
                            if !self.touch_scroll_active
                                && self.touch_drag_y > 8.0
                                && self.hit_test_scroll(self.mouse_x, self.mouse_y).is_some()
                            {
                                self.touch_scroll_active = true;
                                self.pressed_index = None;
                            }
                            if self.touch_scroll_active {
                                self.scroll_at_pointer(dy);
                            } else {
                                self.update_hover_at_pointer();
                            }
                            self.last_touch_y = Some(self.mouse_y);
                        }
                    }
                    TouchPhase::Ended => {
                        self.last_touch_y = None;
                        self.touch_drag_y = 0.0;
                        if self.touch_scroll_active {
                            self.touch_scroll_active = false;
                        } else {
                            self.pointer_released();
                        }
                    }
                    TouchPhase::Cancelled => {
                        self.last_touch_y = None;
                        self.touch_drag_y = 0.0;
                        self.touch_scroll_active = false;
                        self.pressed_index = None;
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
                                    if !text.is_empty()
                                        && !text.chars().all(|c| c.is_control())
                                    {
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
    let event_loop = EventLoop::builder()
        .with_android_app(android_app)
        .build()?;
    let mut app = App::new_reactive(builder);
    event_loop.run_app(&mut app)?;
    Ok(())
}
