use anyhow::Result;
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::time::Instant;
use tiny_skia::Pixmap;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowAttributes, WindowId};

use crate::layout::{self, LayoutRect, ScrollExtent};
use crate::render;
use crate::state;
use w3cos_std::color::Color;
use w3cos_std::style::{Easing, TransitionProperty};
use w3cos_std::{Component, ComponentKind, EventAction};

static EMBEDDED_FONT: &[u8] = include_bytes!("../assets/Inter-Regular.ttf");

/// Target ~60fps for animation frames.
const ANIMATION_FRAME_INTERVAL_MS: u64 = 16;

struct HitNode {
    rect: LayoutRect,
    index: usize,
    is_interactive: bool,
    is_focusable: bool,
    on_click: EventAction,
}

/// A single active CSS transition animation for a specific property on a node.
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

struct App {
    builder: Option<fn() -> Component>,
    root: Component,
    window: Option<Window>,
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
    animations: Vec<ActiveAnimation>,
    last_frame_time: Option<Instant>,
    modifiers: ModifiersState,
}

impl App {
    fn new_reactive(builder: fn() -> Component) -> Self {
        let root = builder();
        let font = fontdue::Font::from_bytes(EMBEDDED_FONT, fontdue::FontSettings::default())
            .expect("failed to load embedded font");
        Self {
            builder: Some(builder),
            root,
            window: None,
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
            animations: Vec::new(),
            last_frame_time: None,
            modifiers: ModifiersState::default(),
        }
    }

    fn new_static(root: Component) -> Self {
        let font = fontdue::Font::from_bytes(EMBEDDED_FONT, fontdue::FontSettings::default())
            .expect("failed to load embedded font");
        Self {
            builder: None,
            root,
            window: None,
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
            animations: Vec::new(),
            last_frame_time: None,
            modifiers: ModifiersState::default(),
        }
    }

    fn rebuild_if_dirty(&mut self) {
        if !state::is_dirty() {
            return;
        }
        let old_root = self.root.clone();
        state::clear_dirty();
        if let Some(builder) = self.builder {
            self.root = builder();
            self.needs_layout = true;
            self.hovered_index = None;
            self.pressed_index = None;
            self.collect_transition_animations(&old_root);
        }
    }

    fn collect_transition_animations(&mut self, old_root: &Component) {
        let old_flat = flatten_styles(old_root);
        let new_flat = flatten_styles(&self.root);
        let now = Instant::now();

        for (idx, (old_style, new_style)) in old_flat.iter().zip(new_flat.iter()).enumerate() {
            let Some(transition) = &new_style.transition else {
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

            if animates_opacity && old_style.opacity != new_style.opacity {
                self.animations.push(ActiveAnimation::Opacity {
                    node_index: idx,
                    from: old_style.opacity,
                    to: new_style.opacity,
                    start: now,
                    duration_ms,
                    delay_ms,
                    easing,
                });
            }
            if animates_background
                && (old_style.background.r != new_style.background.r
                    || old_style.background.g != new_style.background.g
                    || old_style.background.b != new_style.background.b
                    || old_style.background.a != new_style.background.a)
            {
                self.animations.push(ActiveAnimation::Background {
                    node_index: idx,
                    from: old_style.background,
                    to: new_style.background,
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
        let window = match self.window.as_ref() {
            Some(w) => w,
            None => return,
        };
        let size = window.inner_size();
        let (w, h) = (size.width as f32, size.height as f32);
        if w == 0.0 || h == 0.0 {
            return;
        }

        let (cache, scrollable, clip_only) = layout::compute_with_scroll(&self.root, w, h)
            .unwrap_or_else(|_| (Vec::new(), Vec::new(), Vec::new()));
        self.layout_cache = cache;
        self.scrollable_nodes = scrollable;
        self.clip_only_nodes = clip_only;

        let flat = flatten_tree(&self.root);
        self.hit_nodes.clear();
        self.focusable_indices.clear();
        for &(rect, idx) in &self.layout_cache {
            if let Some(&(kind, _, on_click)) = flat.get(idx) {
                let is_interactive = matches!(kind, ComponentKind::Button { .. })
                    || matches!(kind, ComponentKind::TextInput { .. })
                    || !on_click.is_none();
                let is_focusable = matches!(kind, ComponentKind::Button { .. })
                    || matches!(kind, ComponentKind::TextInput { .. });
                if is_focusable {
                    self.focusable_indices.push(idx);
                }
                self.hit_nodes.push(HitNode {
                    rect,
                    index: idx,
                    is_interactive,
                    is_focusable,
                    on_click: on_click.clone(),
                });
            }
        }
        self.needs_layout = false;
    }

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

        let mut render_root = self.root.clone();
        let now = Instant::now();
        apply_animations(&mut render_root, &self.animations, now);
        if let Some(hover_idx) = self.hovered_index {
            apply_hover(
                &mut render_root,
                hover_idx,
                self.pressed_index == Some(hover_idx),
            );
        }

        self.animations.retain(|a| !a.is_complete(now));
        self.last_frame_time = Some(now);

        if !self.animations.is_empty() {
            self.request_repaint();
        }

        let flat = flatten_tree(&render_root);
        let render_nodes: Vec<(usize, LayoutRect, &ComponentKind, &w3cos_std::style::Style)> = self
            .layout_cache
            .iter()
            .filter_map(|&(rect, idx)| {
                flat.get(idx)
                    .map(|&(kind, style, _)| (idx, rect, kind, style))
            })
            .collect();

        let scroll_info = build_scroll_info(
            &self.root,
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

        if let Some(hover_idx) = self.hovered_index
            && let Some(hit) = self
                .hit_nodes
                .iter()
                .find(|h| h.index == hover_idx && h.is_interactive)
        {
            draw_hover_outline(&mut pixmap, hit.rect);
        }
        if let Some(focus_idx) = self.focused_index
            && (self.hovered_index != Some(focus_idx))
            && let Some(&(kind, _, _)) = flatten_tree(&self.root).get(focus_idx)
            && matches!(kind, ComponentKind::Button { .. })
            && let Some(hit) = self
                .hit_nodes
                .iter()
                .find(|h| h.index == focus_idx && h.is_focusable)
        {
            draw_focus_ring(&mut pixmap, hit.rect);
        }

        present_pixels(window, &pixmap, w, h);
    }

    fn hit_test(&self, x: f32, y: f32) -> Option<usize> {
        for hit in self.hit_nodes.iter().rev() {
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

    /// Find the topmost scrollable node that contains the given point.
    fn hit_test_scroll(&self, x: f32, y: f32) -> Option<usize> {
        for (idx, rect, _) in self.scrollable_nodes.iter().rev() {
            if x >= rect.x && x <= rect.x + rect.width && y >= rect.y && y <= rect.y + rect.height {
                return Some(*idx);
            }
        }
        None
    }

    fn handle_click(&mut self, idx: usize) {
        let flat = flatten_tree(&self.root);
        if let Some(&(kind, _, _)) = flat.get(idx) {
            match kind {
                ComponentKind::TextInput { .. } => {
                    self.focused_index = Some(idx);
                    self.request_repaint();
                    return;
                }
                ComponentKind::Button { .. } => {
                    if let Some(hit) = self.hit_nodes.iter().find(|h| h.index == idx)
                        && !hit.on_click.is_none()
                    {
                        state::execute_action(&hit.on_click);
                        self.rebuild_if_dirty();
                    } else {
                        eprintln!("[W3C OS] Click → Button (no action)");
                    }
                    self.request_repaint();
                    return;
                }
                _ => {}
            }
        }
        if let Some(hit) = self.hit_nodes.iter().find(|h| h.index == idx)
            && !hit.on_click.is_none()
        {
            state::execute_action(&hit.on_click);
            self.rebuild_if_dirty();
            self.request_repaint();
            return;
        }
        self.focused_index = None;
        self.request_repaint();
    }

    fn request_repaint(&self) {
        if let Some(ref window) = self.window {
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

fn apply_animations(root: &mut Component, animations: &[ActiveAnimation], now: Instant) {
    for anim in animations {
        let t = anim.progress(now);
        let node_index = anim.node_index();
        apply_animation_to_node(root, node_index, anim, t, &mut 0);
    }
}

fn apply_animation_to_node(
    comp: &mut Component,
    target_idx: usize,
    anim: &ActiveAnimation,
    t: f32,
    counter: &mut usize,
) {
    let my_idx = *counter;
    *counter += 1;

    if my_idx == target_idx {
        let eased = match anim {
            ActiveAnimation::Opacity { easing, .. } => easing.interpolate(t),
            ActiveAnimation::Background { easing, .. } => easing.interpolate(t),
        };
        match anim {
            ActiveAnimation::Opacity { from, to, .. } => {
                comp.style.opacity = *from + eased * (to - from);
            }
            ActiveAnimation::Background { from, to, .. } => {
                comp.style.background = Color::rgba(
                    lerp_u8(from.r, to.r, eased),
                    lerp_u8(from.g, to.g, eased),
                    lerp_u8(from.b, to.b, eased),
                    lerp_u8(from.a, to.a, eased),
                );
            }
        }
    }

    for child in &mut comp.children {
        apply_animation_to_node(child, target_idx, anim, t, counter);
    }
}

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    let t = t.clamp(0.0, 1.0);
    (a as f32 + (b as f32 - a as f32) * t).round() as u8
}

fn apply_hover(root: &mut Component, target_idx: usize, is_pressed: bool) {
    let mut counter = 0usize;
    apply_hover_recursive(root, target_idx, is_pressed, &mut counter);
}

fn apply_hover_recursive(
    comp: &mut Component,
    target_idx: usize,
    is_pressed: bool,
    counter: &mut usize,
) {
    let my_idx = *counter;
    *counter += 1;

    if my_idx == target_idx {
        if is_pressed {
            comp.style.opacity = 0.6;
        } else {
            let bg = &mut comp.style.background;
            if bg.a > 0 {
                bg.r = bg.r.saturating_add(25);
                bg.g = bg.g.saturating_add(25);
                bg.b = bg.b.saturating_add(25);
            }
        }
    }

    for child in &mut comp.children {
        apply_hover_recursive(child, target_idx, is_pressed, counter);
    }
}

fn draw_hover_outline(pixmap: &mut Pixmap, rect: LayoutRect) {
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

fn draw_focus_ring(pixmap: &mut Pixmap, rect: LayoutRect) {
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

impl ApplicationHandler for App {
    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        if matches!(cause, StartCause::ResumeTimeReached { .. }) && !self.animations.is_empty() {
            self.request_repaint();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.animations.is_empty() {
            event_loop.set_control_flow(ControlFlow::Wait);
        } else {
            event_loop.set_control_flow(ControlFlow::WaitUntil(
                Instant::now() + std::time::Duration::from_millis(ANIMATION_FRAME_INTERVAL_MS),
            ));
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
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

            WindowEvent::Resized(_) => {
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
                    if let Some(ref window) = self.window {
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
                    let flat = flatten_tree(&self.root);
                    if let Some(&(kind, _, _)) = flat.get(focus_idx) {
                        match kind {
                            ComponentKind::TextInput { value, .. } => {
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
                                if let Some(ref text) = event.text
                                    && !text.is_empty()
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
                            ComponentKind::Button { .. } => {
                                if let Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Space) =
                                    event.logical_key
                                {
                                    if let Some(hit) =
                                        self.hit_nodes.iter().find(|h| h.index == focus_idx)
                                        && !hit.on_click.is_none()
                                    {
                                        state::execute_action(&hit.on_click);
                                        self.rebuild_if_dirty();
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
                if let Some(focus_idx) = self.focused_index
                    && let Some(&(kind, _, _)) = flatten_tree(&self.root).get(focus_idx)
                    && let ComponentKind::TextInput { value, .. } = kind
                    && let winit::event::Ime::Commit(commit) = ime
                {
                    let current = self
                        .text_input_values
                        .entry(focus_idx)
                        .or_insert_with(|| value.clone());
                    current.push_str(&commit);
                    self.request_repaint();
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                self.ensure_layout();
                if let Some(idx) = self.hit_test_scroll(self.mouse_x, self.mouse_y)
                    && let Some((_rect, extent)) = self
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
                        let (ox, oy) = self.scroll_offsets.get(&idx).copied().unwrap_or((0.0, 0.0));
                        let new_oy = (oy + dy).clamp(0.0, extent.max_y);
                        if (new_oy - oy).abs() > 0.001 {
                            self.scroll_offsets.insert(idx, (ox, new_oy));
                            self.request_repaint();
                        }
                    }
                }
            }

            _ => {}
        }
    }
}

/// For each node index, returns (scroll_x, scroll_y, clip_rect) if the node is inside a scroll or clip container.
fn build_scroll_info(
    root: &Component,
    scrollable: &[(usize, LayoutRect, ScrollExtent)],
    clip_only: &[(usize, LayoutRect)],
    offsets: &HashMap<usize, (f32, f32)>,
) -> Vec<Option<(f32, f32, LayoutRect)>> {
    let parent_map = build_parent_map(root);
    let scrollable_set: std::collections::HashSet<usize> =
        scrollable.iter().map(|(i, _, _)| *i).collect();
    let scrollable_rect: HashMap<usize, LayoutRect> =
        scrollable.iter().map(|(i, r, _)| (*i, *r)).collect();
    let clip_only_set: std::collections::HashSet<usize> =
        clip_only.iter().map(|(i, _)| *i).collect();
    let clip_only_rect: HashMap<usize, LayoutRect> =
        clip_only.iter().map(|(i, r)| (*i, *r)).collect();
    let n = parent_map.len();
    let mut out = vec![None; n];
    #[allow(clippy::needless_range_loop)]
    for idx in 0..n {
        let mut cur = Some(idx);
        let mut scroll_container = None;
        let mut clip_container = None;
        while let Some(i) = cur {
            if scrollable_set.contains(&i) {
                scroll_container = Some(i);
                break;
            }
            if clip_only_set.contains(&i) {
                clip_container = Some(i);
                break;
            }
            cur = parent_map.get(i).copied().flatten();
        }
        if let Some(sc_id) = scroll_container {
            let (sx, sy) = offsets.get(&sc_id).copied().unwrap_or((0.0, 0.0));
            if let Some(&clip) = scrollable_rect.get(&sc_id) {
                out[idx] = Some((sx, sy, clip));
            }
        } else if let Some(cl_id) = clip_container
            && let Some(&clip) = clip_only_rect.get(&cl_id)
        {
            out[idx] = Some((0.0, 0.0, clip));
        }
    }
    out
}

fn build_parent_map(root: &Component) -> Vec<Option<usize>> {
    let mut out = Vec::new();
    build_parent_map_recursive(root, None, &mut out);
    out
}

fn build_parent_map_recursive(
    comp: &Component,
    parent: Option<usize>,
    out: &mut Vec<Option<usize>>,
) {
    let my_idx = out.len();
    out.push(parent);
    for child in &comp.children {
        build_parent_map_recursive(child, Some(my_idx), out);
    }
}

fn flatten_styles(comp: &Component) -> Vec<w3cos_std::style::Style> {
    let mut out = Vec::new();
    flatten_styles_recursive(comp, &mut out);
    out
}

fn flatten_styles_recursive(comp: &Component, out: &mut Vec<w3cos_std::style::Style>) {
    out.push(comp.style.clone());
    for child in &comp.children {
        flatten_styles_recursive(child, out);
    }
}

fn flatten_tree(comp: &Component) -> Vec<(&ComponentKind, &w3cos_std::style::Style, &EventAction)> {
    let mut out = Vec::new();
    flatten_recursive(comp, &mut out);
    out
}

fn flatten_recursive<'a>(
    comp: &'a Component,
    out: &mut Vec<(
        &'a ComponentKind,
        &'a w3cos_std::style::Style,
        &'a EventAction,
    )>,
) {
    out.push((&comp.kind, &comp.style, &comp.on_click));
    for child in &comp.children {
        flatten_recursive(child, out);
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
