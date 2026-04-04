use anyhow::Result;
use std::collections::HashMap;
use std::time::Instant;

#[cfg(feature = "gpu")]
use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowAttributes, WindowId};

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

use crate::layout::{self, LayoutEngine, LayoutRect, ScrollExtent};
use crate::render;
use crate::state;
use w3cos_std::color::Color;
use w3cos_std::style::{Easing, TransitionProperty};
use w3cos_std::{Component, ComponentKind, EventAction};

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

    fn query(&self, x: f32, y: f32, hit_nodes: &[HitNode]) -> Option<usize> {
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
            if hit.is_interactive
                && x >= hit.rect.x
                && x <= hit.rect.x + hit.rect.width
                && y >= hit.rect.y
                && y <= hit.rect.y + hit.rect.height
            {
                return Some(hit.index);
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
}

impl ActiveAnimation {
    fn node_index(&self) -> usize {
        match self {
            ActiveAnimation::Opacity { node_index, .. } => *node_index,
            ActiveAnimation::Background { node_index, .. } => *node_index,
        }
    }

    fn progress(&self, now: Instant) -> f32 {
        let elapsed_ms = now
            .duration_since(match self {
                ActiveAnimation::Opacity { start, .. } => *start,
                ActiveAnimation::Background { start, .. } => *start,
            })
            .as_secs_f64()
            * 1000.0;
        let delay_ms = match self {
            ActiveAnimation::Opacity { delay_ms, .. } => *delay_ms,
            ActiveAnimation::Background { delay_ms, .. } => *delay_ms,
        };
        let duration_ms = match self {
            ActiveAnimation::Opacity { duration_ms, .. } => *duration_ms,
            ActiveAnimation::Background { duration_ms, .. } => *duration_ms,
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
    animations: Vec<ActiveAnimation>,
    last_frame_time: Option<Instant>,
    modifiers: ModifiersState,

    // Performance: persistent layout engine (avoids TaffyTree rebuild on resize)
    layout_engine: LayoutEngine,
    // Performance: scroll ancestor map (avoids O(n*depth) parent walk)
    scroll_ancestor: Vec<Option<usize>>,
    // Performance: spatial grid for O(1) hit testing
    spatial_grid: SpatialGrid,
    // Performance: dirty frame detection
    paint_generation: u64,
    layout_generation: u64,

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
    glyph_cache: render::GlyphCache,

    // CPU-specific
    #[cfg(feature = "cpu-render")]
    window: Option<Window>,
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
            animations: Vec::new(),
            last_frame_time: None,
            modifiers: ModifiersState::default(),

            layout_engine: LayoutEngine::new(),
            scroll_ancestor: Vec::new(),
            spatial_grid: SpatialGrid::empty(),
            paint_generation: 0,
            layout_generation: 0,

            #[cfg(feature = "gpu")]
            render_cx: RenderContext::new(),
            #[cfg(feature = "gpu")]
            renderers: vec![],
            #[cfg(feature = "gpu")]
            gpu_state: GpuState::Suspended(None),
            #[cfg(feature = "gpu")]
            scene: Scene::new(),
            #[cfg(feature = "gpu")]
            font_data: render::make_font_data(EMBEDDED_FONT),
            #[cfg(feature = "gpu")]
            glyph_cache: render::GlyphCache::new(),

            #[cfg(feature = "cpu-render")]
            window: None,
        }
    }

    fn get_window(&self) -> Option<&Window> {
        #[cfg(feature = "gpu")]
        {
            match &self.gpu_state {
                GpuState::Active { window, .. } => Some(window.as_ref()),
                GpuState::Suspended(Some(w)) => Some(w.as_ref()),
                GpuState::Suspended(None) => None,
            }
        }
        #[cfg(feature = "cpu-render")]
        {
            self.window.as_ref()
        }
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
            self.hovered_index = None;
            self.pressed_index = None;
            self.collect_transition_animations(&old_root);
        } else if let Some(builder) = self.builder {
            self.root = builder();
            self.needs_layout = true;
            self.needs_tree_rebuild = true;
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
        }
    }

    fn ensure_layout(&mut self) {
        if !self.needs_layout && !self.layout_cache.is_empty() {
            return;
        }
        let window = match self.get_window() {
            Some(w) => w,
            None => return,
        };
        let size = window.inner_size();
        let (w, h) = (size.width as f32, size.height as f32);
        if w == 0.0 || h == 0.0 {
            return;
        }

        let flat = layout::pre_flatten(&self.root);

        if self.needs_tree_rebuild {
            self.layout_engine.invalidate();
            self.needs_tree_rebuild = false;
        }

        let results = self
            .layout_engine
            .compute(&self.root, &flat, w, h)
            .unwrap_or_else(|_| layout::LayoutResults::empty());

        self.layout_cache = results.layout_cache;
        self.scrollable_nodes = results.scrollable_nodes;
        self.clip_only_nodes = results.clip_only_nodes;
        self.scroll_ancestor = results.scroll_ancestor;

        self.hit_nodes.clear();
        self.focusable_indices.clear();
        for &(rect, idx) in &self.layout_cache {
            if let Some(node) = flat.get(idx) {
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

        self.spatial_grid = SpatialGrid::build(&self.hit_nodes, w, h);
        self.needs_layout = false;
        self.layout_generation += 1;
    }

    // -----------------------------------------------------------------------
    // GPU paint — zero-copy via style overrides (no root.clone())
    // -----------------------------------------------------------------------
    #[cfg(feature = "gpu")]
    fn paint(&mut self) {
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

        // Build render nodes using flat array + overrides (no flatten_tree needed)
        let render_nodes: Vec<(usize, LayoutRect, &ComponentKind, &w3cos_std::style::Style)> = self
            .layout_cache
            .iter()
            .filter_map(|&(rect, idx)| {
                let node = flat.get(idx)?;
                let style = style_overrides.get(&idx).unwrap_or(node.style);
                Some((idx, rect, node.kind, style))
            })
            .collect();

        // Build scroll info using pre-computed scroll_ancestor (O(n) instead of O(n*depth))
        let scroll_info = build_scroll_info_fast(
            &self.scroll_ancestor,
            &self.scrollable_nodes,
            &self.clip_only_nodes,
            &self.scroll_offsets,
        );

        // Render (split borrows: scene is separate from root/layout_cache/etc.)
        self.scene.reset();
        render::render_frame(
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
        );

        // Draw hover outline
        if let Some(hover_idx) = self.hovered_index {
            if let Some(hit) = self
                .hit_nodes
                .iter()
                .find(|h| h.index == hover_idx && h.is_interactive)
            {
                render::draw_hover_outline(&mut self.scene, hit.rect);
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
                            render::draw_focus_ring(&mut self.scene, hit.rect);
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
                eprintln!("[W3C OS] GPU render error: {e}");
                return;
            }

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
    fn paint(&mut self) {
        self.ensure_layout();

        let window = match self.window.as_ref() {
            Some(w) => w,
            None => return,
        };
        let size = window.inner_size();
        let (w, h) = (size.width, size.height);
        if w == 0 || h == 0 {
            return;
        }

        let mut pixmap = match Pixmap::new(w, h) {
            Some(p) => p,
            None => return,
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
        render::render_frame(
            &mut pixmap,
            &render_nodes,
            &self.font,
            &scroll_info,
            &self.text_input_values,
            self.focused_index,
        );

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

        if !self.animations.is_empty() {
            self.request_repaint();
        }

        present_pixels(window, &pixmap, w, h);
    }

    fn hit_test(&self, x: f32, y: f32) -> Option<usize> {
        self.spatial_grid.query(x, y, &self.hit_nodes)
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
                self.request_repaint();
                return;
            }
            if kind_is_button {
                if !hit.on_click.is_none() {
                    state::execute_action(&hit.on_click);
                    self.rebuild_if_dirty();
                } else {
                    eprintln!("[W3C OS] Click → Button (no action)");
                }
                self.request_repaint();
                return;
            }
            if !hit.on_click.is_none() {
                state::execute_action(&hit.on_click);
                self.rebuild_if_dirty();
                self.request_repaint();
                return;
            }
        }
        self.focused_index = None;
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
        }
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

#[cfg(feature = "cpu-render")]
fn present_pixels(window: &Window, pixmap: &Pixmap, w: u32, h: u32) {
    let context = match softbuffer::Context::new(window) {
        Ok(c) => c,
        Err(_) => return,
    };
    let mut surface = match softbuffer::Surface::new(&context, window) {
        Ok(s) => s,
        Err(_) => return,
    };
    if surface
        .resize(NonZeroU32::new(w).unwrap(), NonZeroU32::new(h).unwrap())
        .is_err()
    {
        return;
    }
    let mut buffer = match surface.buffer_mut() {
        Ok(b) => b,
        Err(_) => return,
    };
    for (i, px) in pixmap.pixels().iter().enumerate() {
        buffer[i] = (px.red() as u32) << 16 | (px.green() as u32) << 8 | px.blue() as u32;
    }
    let _ = buffer.present();
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
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let has_animations = !self.animations.is_empty();
        let timer_deadline = crate::timers::next_deadline();

        match (has_animations, timer_deadline) {
            (false, None) => {
                event_loop.set_control_flow(ControlFlow::Wait);
            }
            (true, None) => {
                event_loop.set_control_flow(ControlFlow::WaitUntil(
                    Instant::now() + std::time::Duration::from_millis(ANIMATION_FRAME_INTERVAL_MS),
                ));
            }
            (false, Some(deadline)) => {
                event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
            }
            (true, Some(deadline)) => {
                let anim_deadline =
                    Instant::now() + std::time::Duration::from_millis(ANIMATION_FRAME_INTERVAL_MS);
                event_loop.set_control_flow(ControlFlow::WaitUntil(deadline.min(anim_deadline)));
            }
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        #[cfg(feature = "gpu")]
        {
            let window = match &self.gpu_state {
                GpuState::Suspended(cached) => cached.clone().unwrap_or_else(|| {
                    let attrs = WindowAttributes::default()
                        .with_title("W3C OS")
                        .with_inner_size(winit::dpi::LogicalSize::new(1200, 800));
                    Arc::new(event_loop.create_window(attrs).unwrap())
                }),
                GpuState::Active { .. } => return,
            };

            self.scale_factor = window.scale_factor();
            let size = window.inner_size();

            let surface = pollster::block_on(self.render_cx.create_surface(
                window.clone(),
                size.width.max(1),
                size.height.max(1),
                wgpu::PresentMode::AutoVsync,
            ))
            .expect("failed to create GPU surface");

            while self.renderers.len() <= surface.dev_id {
                self.renderers.push(None);
            }
            if self.renderers[surface.dev_id].is_none() {
                let dev = &self.render_cx.devices[surface.dev_id];
                self.renderers[surface.dev_id] = Some(
                    Renderer::new(&dev.device, RendererOptions::default())
                        .expect("failed to create vello renderer"),
                );
            }

            self.gpu_state = GpuState::Active { surface, window };
            self.needs_layout = true;
        }

        #[cfg(feature = "cpu-render")]
        {
            if self.window.is_none() {
                let attrs = WindowAttributes::default()
                    .with_title("W3C OS")
                    .with_inner_size(winit::dpi::LogicalSize::new(1200, 800));
                let window = event_loop.create_window(attrs).unwrap();
                self.scale_factor = window.scale_factor();
                self.window = Some(window);
                self.needs_layout = true;
            }
        }
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
                    if _size.width > 0 && _size.height > 0 {
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
                self.mouse_x = position.x as f32;
                self.mouse_y = position.y as f32;

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

            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => match state {
                ElementState::Pressed => {
                    self.ensure_layout();
                    let hit = self.hit_test(self.mouse_x, self.mouse_y);
                    if let Some(idx) = hit {
                        self.pressed_index = Some(idx);
                        self.request_repaint();
                    } else {
                        self.focused_index = None;
                        self.request_repaint();
                    }
                }
                ElementState::Released => {
                    if let Some(pressed_idx) = self.pressed_index.take() {
                        let current_hover = self.hit_test(self.mouse_x, self.mouse_y);
                        if current_hover == Some(pressed_idx) {
                            self.handle_click(pressed_idx);
                        }
                        self.request_repaint();
                    }
                }
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
                            if let winit::event::Ime::Commit(commit) = ime {
                                let current = self
                                    .text_input_values
                                    .entry(focus_idx)
                                    .or_insert_with(|| value);
                                current.push_str(&commit);
                                self.request_repaint();
                            }
                        }
                    }
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                self.ensure_layout();
                if let Some(idx) = self.hit_test_scroll(self.mouse_x, self.mouse_y) {
                    if let Some((_rect, extent)) = self
                        .scrollable_nodes
                        .iter()
                        .find(|(i, _, _)| *i == idx)
                        .map(|(_, r, e)| (r, e))
                    {
                        let dy = match delta {
                            MouseScrollDelta::LineDelta(_, y) => -y * 24.0,
                            MouseScrollDelta::PixelDelta(pos) => -pos.y as f32,
                        };
                        if dy != 0.0 {
                            let (ox, oy) =
                                self.scroll_offsets.get(&idx).copied().unwrap_or((0.0, 0.0));
                            let new_oy = (oy + dy).clamp(0.0, extent.max_y);
                            if (new_oy - oy).abs() > 0.001 {
                                self.scroll_offsets.insert(idx, (ox, new_oy));
                                self.request_repaint();
                            }
                        }
                    }
                }
            }

            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

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
