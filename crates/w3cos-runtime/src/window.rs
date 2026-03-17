use anyhow::Result;
use std::num::NonZeroU32;
use tiny_skia::Pixmap;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowAttributes, WindowId};

use crate::animation::AnimationManager;
use crate::layout::{self, LayoutRect};
use crate::render;
use w3cos_std::{Component, ComponentKind};

static EMBEDDED_FONT: &[u8] = include_bytes!("../assets/Inter-Regular.ttf");

struct HitNode {
    rect: LayoutRect,
    index: usize,
    is_interactive: bool,
}

struct App {
    root: Component,
    window: Option<Window>,
    font: fontdue::Font,
    mouse_x: f32,
    mouse_y: f32,
    scale_factor: f64,
    hovered_index: Option<usize>,
    pressed_index: Option<usize>,
    hit_nodes: Vec<HitNode>,
    layout_cache: Vec<(LayoutRect, usize)>,
    needs_layout: bool,
    animation_manager: AnimationManager,
}

impl App {
    fn new(root: Component) -> Self {
        let font = fontdue::Font::from_bytes(EMBEDDED_FONT, fontdue::FontSettings::default())
            .expect("failed to load embedded font");
        Self {
            root,
            window: None,
            font,
            mouse_x: 0.0,
            mouse_y: 0.0,
            scale_factor: 1.0,
            hovered_index: None,
            pressed_index: None,
            hit_nodes: Vec::new(),
            layout_cache: Vec::new(),
            needs_layout: true,
            animation_manager: AnimationManager::new(),
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

        // Layout uses physical pixels
        self.layout_cache = layout::compute(&self.root, w, h).unwrap_or_default();

        let flat = flatten_tree(&self.root);
        self.hit_nodes.clear();
        for &(rect, idx) in &self.layout_cache {
            if let Some(&(kind, _)) = flat.get(idx) {
                let is_interactive = matches!(kind, ComponentKind::Button { .. });
                self.hit_nodes.push(HitNode {
                    rect,
                    index: idx,
                    is_interactive,
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
        if let Some(hover_idx) = self.hovered_index {
            apply_hover(
                &mut render_root,
                hover_idx,
                self.pressed_index == Some(hover_idx),
            );
        }

        // Tick animations and apply interpolated styles to render tree
        let still_animating = self.animation_manager.tick_and_apply(&mut render_root);
        if still_animating {
            self.request_repaint();
        }

        let flat = flatten_tree(&render_root);
        let render_nodes: Vec<(LayoutRect, &ComponentKind, &w3cos_std::style::Style)> = self
            .layout_cache
            .iter()
            .filter_map(|&(rect, idx)| flat.get(idx).map(|&(kind, style)| (rect, kind, style)))
            .collect();

        render::render_frame(&mut pixmap, &render_nodes, &self.font);

        if let Some(hover_idx) = self.hovered_index
            && let Some(hit) = self
                .hit_nodes
                .iter()
                .find(|h| h.index == hover_idx && h.is_interactive)
        {
            draw_hover_outline(&mut pixmap, hit.rect);
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

    fn request_repaint(&self) {
        if let Some(ref window) = self.window {
            window.request_redraw();
        }
    }
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
                // position is in logical coordinates on macOS
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
                    if hit.is_some() {
                        self.pressed_index = hit;
                        self.request_repaint();
                    }
                }
                ElementState::Released => {
                    if let Some(pressed_idx) = self.pressed_index.take() {
                        let current_hover = self.hit_test(self.mouse_x, self.mouse_y);
                        if current_hover == Some(pressed_idx) {
                            let flat = flatten_tree(&self.root);
                            if let Some(&(kind, _)) = flat.get(pressed_idx) {
                                match kind {
                                    ComponentKind::Button { label } => {
                                        eprintln!("[W3C OS] Click → Button(\"{}\")", label);
                                    }
                                    ComponentKind::Text { content } => {
                                        eprintln!("[W3C OS] Click → Text(\"{}\")", content);
                                    }
                                    _ => {
                                        eprintln!("[W3C OS] Click → node #{}", pressed_idx);
                                    }
                                }
                            }
                        }
                        self.request_repaint();
                    }
                }
            },

            _ => {}
        }
    }
}

fn flatten_tree(comp: &Component) -> Vec<(&ComponentKind, &w3cos_std::style::Style)> {
    let mut out = Vec::new();
    flatten_recursive(comp, &mut out);
    out
}

fn flatten_recursive<'a>(
    comp: &'a Component,
    out: &mut Vec<(&'a ComponentKind, &'a w3cos_std::style::Style)>,
) {
    out.push((&comp.kind, &comp.style));
    for child in &comp.children {
        flatten_recursive(child, out);
    }
}

pub fn run(root: Component) -> Result<()> {
    let event_loop = EventLoop::new()?;
    let mut app = App::new(root);
    event_loop.run_app(&mut app)?;
    Ok(())
}
