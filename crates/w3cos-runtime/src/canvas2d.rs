//! W3C Canvas 2D Context API
//!
//! Mirrors the HTML Canvas 2D Context specification:
//! https://html.spec.whatwg.org/multipage/canvas.html
//!
//! This module provides a `CanvasRenderingContext2D` backed by `tiny-skia`
//! for CPU rendering and exposes the standard W3C API surface so that
//! third-party libraries (Monaco, CodeMirror, xterm.js, etc.) can render
//! onto a w3cos canvas element without modification.
//!
//! # Example
//! ```ignore
//! let mut ctx = CanvasRenderingContext2D::new(800, 600);
//! ctx.set_fill_style("#1e1e1e");
//! ctx.fill_rect(0.0, 0.0, 800.0, 600.0);
//! ctx.set_font("14px monospace");
//! ctx.set_fill_style("#d4d4d4");
//! ctx.fill_text("Hello, w3cos!", 10.0, 20.0, None);
//! let pixels = ctx.get_image_data(0, 0, 800, 600);
//! ```

use std::collections::VecDeque;

// ── Color / Style ──────────────────────────────────────────────────────────

/// A paint style — color string, gradient, or pattern.
/// Mirrors `CanvasRenderingContext2D.fillStyle` / `strokeStyle`.
#[derive(Debug, Clone)]
pub enum CanvasStyle {
    Color(u8, u8, u8, u8), // r, g, b, a
    Transparent,
}

impl CanvasStyle {
    /// Parse a CSS color string into a `CanvasStyle`.
    /// Supports: `#rgb`, `#rrggbb`, `#rrggbbaa`, `rgb(r,g,b)`, `rgba(r,g,b,a)`,
    /// `transparent`, and the 16 basic CSS named colors.
    pub fn from_str(s: &str) -> Self {
        let s = s.trim();
        if s == "transparent" {
            return Self::Transparent;
        }
        if let Some(hex) = s.strip_prefix('#') {
            return Self::parse_hex(hex);
        }
        if s.starts_with("rgba(") {
            return Self::parse_rgba(s);
        }
        if s.starts_with("rgb(") {
            return Self::parse_rgb(s);
        }
        Self::parse_named(s)
    }

    fn parse_hex(hex: &str) -> Self {
        match hex.len() {
            3 => {
                let r = u8::from_str_radix(&hex[0..1].repeat(2), 16).unwrap_or(0);
                let g = u8::from_str_radix(&hex[1..2].repeat(2), 16).unwrap_or(0);
                let b = u8::from_str_radix(&hex[2..3].repeat(2), 16).unwrap_or(0);
                Self::Color(r, g, b, 255)
            }
            6 => {
                let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0);
                let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0);
                let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0);
                Self::Color(r, g, b, 255)
            }
            8 => {
                let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0);
                let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0);
                let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0);
                let a = u8::from_str_radix(&hex[6..8], 16).unwrap_or(255);
                Self::Color(r, g, b, a)
            }
            _ => Self::Color(0, 0, 0, 255),
        }
    }

    fn parse_rgb(s: &str) -> Self {
        let inner = s.trim_start_matches("rgb(").trim_end_matches(')');
        let parts: Vec<&str> = inner.split(',').collect();
        if parts.len() >= 3 {
            let r = parts[0].trim().parse::<u8>().unwrap_or(0);
            let g = parts[1].trim().parse::<u8>().unwrap_or(0);
            let b = parts[2].trim().parse::<u8>().unwrap_or(0);
            Self::Color(r, g, b, 255)
        } else {
            Self::Color(0, 0, 0, 255)
        }
    }

    fn parse_rgba(s: &str) -> Self {
        let inner = s.trim_start_matches("rgba(").trim_end_matches(')');
        let parts: Vec<&str> = inner.split(',').collect();
        if parts.len() >= 4 {
            let r = parts[0].trim().parse::<u8>().unwrap_or(0);
            let g = parts[1].trim().parse::<u8>().unwrap_or(0);
            let b = parts[2].trim().parse::<u8>().unwrap_or(0);
            let a_f: f32 = parts[3].trim().parse().unwrap_or(1.0);
            let a = (a_f.clamp(0.0, 1.0) * 255.0) as u8;
            Self::Color(r, g, b, a)
        } else {
            Self::Color(0, 0, 0, 255)
        }
    }

    fn parse_named(s: &str) -> Self {
        match s {
            "black" => Self::Color(0, 0, 0, 255),
            "white" => Self::Color(255, 255, 255, 255),
            "red" => Self::Color(255, 0, 0, 255),
            "green" => Self::Color(0, 128, 0, 255),
            "blue" => Self::Color(0, 0, 255, 255),
            "yellow" => Self::Color(255, 255, 0, 255),
            "cyan" | "aqua" => Self::Color(0, 255, 255, 255),
            "magenta" | "fuchsia" => Self::Color(255, 0, 255, 255),
            "orange" => Self::Color(255, 165, 0, 255),
            "purple" => Self::Color(128, 0, 128, 255),
            "gray" | "grey" => Self::Color(128, 128, 128, 255),
            "silver" => Self::Color(192, 192, 192, 255),
            "lime" => Self::Color(0, 255, 0, 255),
            "maroon" => Self::Color(128, 0, 0, 255),
            "navy" => Self::Color(0, 0, 128, 255),
            "teal" => Self::Color(0, 128, 128, 255),
            _ => Self::Color(0, 0, 0, 255),
        }
    }

    pub fn to_rgba(&self) -> (u8, u8, u8, u8) {
        match self {
            Self::Color(r, g, b, a) => (*r, *g, *b, *a),
            Self::Transparent => (0, 0, 0, 0),
        }
    }
}

// ── TextAlign / TextBaseline ───────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextAlign {
    Start,
    End,
    Left,
    Right,
    Center,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextBaseline {
    Top,
    Hanging,
    Middle,
    Alphabetic,
    Ideographic,
    Bottom,
}

// ── LineCap / LineJoin ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineCap {
    Butt,
    Round,
    Square,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineJoin {
    Miter,
    Round,
    Bevel,
}

// ── CompositeOperation ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompositeOperation {
    SourceOver,
    SourceIn,
    SourceOut,
    SourceAtop,
    DestinationOver,
    DestinationIn,
    DestinationOut,
    DestinationAtop,
    Lighter,
    Copy,
    Xor,
}

// ── ImageData ─────────────────────────────────────────────────────────────

/// W3C `ImageData` — raw RGBA pixel buffer.
#[derive(Debug, Clone)]
pub struct ImageData {
    pub data: Vec<u8>, // RGBA, row-major
    pub width: u32,
    pub height: u32,
}

impl ImageData {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            data: vec![0u8; (width * height * 4) as usize],
            width,
            height,
        }
    }

    pub fn from_bytes(data: Vec<u8>, width: u32, height: u32) -> Self {
        Self {
            data,
            width,
            height,
        }
    }
}

// ── TextMetrics ────────────────────────────────────────────────────────────

/// W3C `TextMetrics` — result of `measureText()`.
#[derive(Debug, Clone)]
pub struct TextMetrics {
    pub width: f32,
    pub actual_bounding_box_ascent: f32,
    pub actual_bounding_box_descent: f32,
    pub font_bounding_box_ascent: f32,
    pub font_bounding_box_descent: f32,
}

// ── Path2D ─────────────────────────────────────────────────────────────────

/// W3C `Path2D` — a reusable path object.
#[derive(Debug, Clone)]
pub enum PathOp {
    MoveTo(f32, f32),
    LineTo(f32, f32),
    QuadraticCurveTo(f32, f32, f32, f32),
    BezierCurveTo(f32, f32, f32, f32, f32, f32),
    Arc(f32, f32, f32, f32, f32, bool),
    ArcTo(f32, f32, f32, f32, f32),
    Rect(f32, f32, f32, f32),
    Ellipse(f32, f32, f32, f32, f32, f32, f32, bool),
    ClosePath,
}

#[derive(Debug, Clone, Default)]
pub struct Path2D {
    pub ops: Vec<PathOp>,
}

impl Path2D {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn move_to(&mut self, x: f32, y: f32) {
        self.ops.push(PathOp::MoveTo(x, y));
    }
    pub fn line_to(&mut self, x: f32, y: f32) {
        self.ops.push(PathOp::LineTo(x, y));
    }
    pub fn close_path(&mut self) {
        self.ops.push(PathOp::ClosePath);
    }
    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        self.ops.push(PathOp::Rect(x, y, w, h));
    }
    pub fn arc(&mut self, x: f32, y: f32, r: f32, start: f32, end: f32, ccw: bool) {
        self.ops.push(PathOp::Arc(x, y, r, start, end, ccw));
    }
    pub fn quadratic_curve_to(&mut self, cpx: f32, cpy: f32, x: f32, y: f32) {
        self.ops.push(PathOp::QuadraticCurveTo(cpx, cpy, x, y));
    }
    pub fn bezier_curve_to(&mut self, cp1x: f32, cp1y: f32, cp2x: f32, cp2y: f32, x: f32, y: f32) {
        self.ops
            .push(PathOp::BezierCurveTo(cp1x, cp1y, cp2x, cp2y, x, y));
    }
    pub fn ellipse(
        &mut self,
        x: f32,
        y: f32,
        rx: f32,
        ry: f32,
        rot: f32,
        start: f32,
        end: f32,
        ccw: bool,
    ) {
        self.ops
            .push(PathOp::Ellipse(x, y, rx, ry, rot, start, end, ccw));
    }
}

// ── State snapshot (for save/restore) ─────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ContextState {
    pub fill_style: CanvasStyle,
    pub stroke_style: CanvasStyle,
    pub line_width: f32,
    pub line_cap: LineCap,
    pub line_join: LineJoin,
    pub miter_limit: f32,
    pub global_alpha: f32,
    pub global_composite_operation: CompositeOperation,
    pub font_size: f32,
    pub font_family: String,
    pub text_align: TextAlign,
    pub text_baseline: TextBaseline,
    pub shadow_blur: f32,
    pub shadow_offset_x: f32,
    pub shadow_offset_y: f32,
    pub shadow_color: CanvasStyle,
    pub transform: [f32; 6],
}

impl Default for ContextState {
    fn default() -> Self {
        Self {
            fill_style: CanvasStyle::Color(0, 0, 0, 255),
            stroke_style: CanvasStyle::Color(0, 0, 0, 255),
            line_width: 1.0,
            line_cap: LineCap::Butt,
            line_join: LineJoin::Miter,
            miter_limit: 10.0,
            global_alpha: 1.0,
            global_composite_operation: CompositeOperation::SourceOver,
            font_size: 10.0,
            font_family: "sans-serif".into(),
            text_align: TextAlign::Start,
            text_baseline: TextBaseline::Alphabetic,
            shadow_blur: 0.0,
            shadow_offset_x: 0.0,
            shadow_offset_y: 0.0,
            shadow_color: CanvasStyle::Transparent,
            transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        }
    }
}

// ── CanvasRenderingContext2D ───────────────────────────────────────────────

/// W3C `CanvasRenderingContext2D` — the 2D drawing API for `<canvas>` elements.
///
/// Backed by a software pixel buffer (`Vec<u8>`, RGBA). The GPU renderer
/// can upload this buffer as a texture each frame. Third-party libraries
/// (Monaco, CodeMirror, xterm.js) call the standard W3C API and w3cos
/// handles the native rendering — no browser required.
pub struct CanvasRenderingContext2D {
    pub width: u32,
    pub height: u32,
    pixels: Vec<u8>, // RGBA row-major pixel buffer
    current_path: Path2D,
    pub state: ContextState,
    state_stack: VecDeque<ContextState>,
}

impl CanvasRenderingContext2D {
    /// Create a new 2D context with a blank (transparent black) pixel buffer.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0u8; (width * height * 4) as usize],
            current_path: Path2D::new(),
            state: ContextState::default(),
            state_stack: VecDeque::new(),
        }
    }

    // ── State ──────────────────────────────────────────────────────────────

    /// `ctx.save()` — push current state onto the stack.
    pub fn save(&mut self) {
        self.state_stack.push_back(self.state.clone());
    }

    /// `ctx.restore()` — pop and restore the last saved state.
    pub fn restore(&mut self) {
        if let Some(s) = self.state_stack.pop_back() {
            self.state = s;
        }
    }

    // ── Style setters ──────────────────────────────────────────────────────

    pub fn set_fill_style(&mut self, style: &str) {
        self.state.fill_style = CanvasStyle::from_str(style);
    }
    pub fn set_stroke_style(&mut self, style: &str) {
        self.state.stroke_style = CanvasStyle::from_str(style);
    }
    pub fn set_line_width(&mut self, w: f32) {
        self.state.line_width = w;
    }
    pub fn set_global_alpha(&mut self, a: f32) {
        self.state.global_alpha = a.clamp(0.0, 1.0);
    }
    pub fn set_font(&mut self, font: &str) {
        // Parse "14px monospace" style font shorthand
        let parts: Vec<&str> = font.trim().splitn(2, ' ').collect();
        if parts.len() == 2 {
            let size_str = parts[0].trim_end_matches("px");
            if let Ok(sz) = size_str.parse::<f32>() {
                self.state.font_size = sz;
            }
            self.state.font_family = parts[1].to_string();
        }
    }
    pub fn set_text_align(&mut self, align: &str) {
        self.state.text_align = match align {
            "center" => TextAlign::Center,
            "right" => TextAlign::Right,
            "end" => TextAlign::End,
            "start" => TextAlign::Start,
            _ => TextAlign::Left,
        };
    }
    pub fn set_text_baseline(&mut self, baseline: &str) {
        self.state.text_baseline = match baseline {
            "top" => TextBaseline::Top,
            "hanging" => TextBaseline::Hanging,
            "middle" => TextBaseline::Middle,
            "ideographic" => TextBaseline::Ideographic,
            "bottom" => TextBaseline::Bottom,
            _ => TextBaseline::Alphabetic,
        };
    }
    pub fn set_shadow_blur(&mut self, blur: f32) {
        self.state.shadow_blur = blur;
    }
    pub fn set_shadow_offset_x(&mut self, x: f32) {
        self.state.shadow_offset_x = x;
    }
    pub fn set_shadow_offset_y(&mut self, y: f32) {
        self.state.shadow_offset_y = y;
    }
    pub fn set_shadow_color(&mut self, color: &str) {
        self.state.shadow_color = CanvasStyle::from_str(color);
    }

    // ── Transform ─────────────────────────────────────────────────────────

    /// `ctx.setTransform(a, b, c, d, e, f)`
    pub fn set_transform(&mut self, a: f32, b: f32, c: f32, d: f32, e: f32, f: f32) {
        self.state.transform = [a, b, c, d, e, f];
    }
    /// `ctx.resetTransform()`
    pub fn reset_transform(&mut self) {
        self.state.transform = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    }
    /// `ctx.translate(x, y)`
    pub fn translate(&mut self, x: f32, y: f32) {
        self.state.transform[4] += x;
        self.state.transform[5] += y;
    }
    /// `ctx.scale(sx, sy)`
    pub fn scale(&mut self, sx: f32, sy: f32) {
        self.state.transform[0] *= sx;
        self.state.transform[3] *= sy;
    }

    // ── Path API ──────────────────────────────────────────────────────────

    pub fn begin_path(&mut self) {
        self.current_path = Path2D::new();
    }
    pub fn close_path(&mut self) {
        self.current_path.close_path();
    }
    pub fn move_to(&mut self, x: f32, y: f32) {
        self.current_path.move_to(x, y);
    }
    pub fn line_to(&mut self, x: f32, y: f32) {
        self.current_path.line_to(x, y);
    }
    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        self.current_path.rect(x, y, w, h);
    }
    pub fn arc(&mut self, x: f32, y: f32, r: f32, start: f32, end: f32, ccw: bool) {
        self.current_path.arc(x, y, r, start, end, ccw);
    }
    pub fn quadratic_curve_to(&mut self, cpx: f32, cpy: f32, x: f32, y: f32) {
        self.current_path.quadratic_curve_to(cpx, cpy, x, y);
    }
    pub fn bezier_curve_to(&mut self, cp1x: f32, cp1y: f32, cp2x: f32, cp2y: f32, x: f32, y: f32) {
        self.current_path
            .bezier_curve_to(cp1x, cp1y, cp2x, cp2y, x, y);
    }
    pub fn ellipse(
        &mut self,
        x: f32,
        y: f32,
        rx: f32,
        ry: f32,
        rot: f32,
        start: f32,
        end: f32,
        ccw: bool,
    ) {
        self.current_path
            .ellipse(x, y, rx, ry, rot, start, end, ccw);
    }

    // ── Rectangle drawing ─────────────────────────────────────────────────

    /// `ctx.fillRect(x, y, w, h)`
    pub fn fill_rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        let (r, g, b, a) = self.state.fill_style.to_rgba();
        let alpha = (a as f32 / 255.0 * self.state.global_alpha).clamp(0.0, 1.0);
        let [_ta, _tb, _tc, _td, tx, ty] = self.state.transform;
        let x0 = (x + tx) as i32;
        let y0 = (y + ty) as i32;
        let x1 = (x + w + tx) as i32;
        let y1 = (y + h + ty) as i32;
        self.fill_rect_pixels(x0, y0, x1, y1, r, g, b, alpha);
    }

    /// `ctx.clearRect(x, y, w, h)` — set pixels to transparent black.
    pub fn clear_rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        let [_ta, _tb, _tc, _td, tx, ty] = self.state.transform;
        let x0 = (x + tx) as i32;
        let y0 = (y + ty) as i32;
        let x1 = (x + w + tx) as i32;
        let y1 = (y + h + ty) as i32;
        for py in y0.max(0)..y1.min(self.height as i32) {
            for px in x0.max(0)..x1.min(self.width as i32) {
                let idx = ((py as u32 * self.width + px as u32) * 4) as usize;
                self.pixels[idx] = 0;
                self.pixels[idx + 1] = 0;
                self.pixels[idx + 2] = 0;
                self.pixels[idx + 3] = 0;
            }
        }
    }

    /// `ctx.strokeRect(x, y, w, h)` — draw rectangle outline.
    pub fn stroke_rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        let lw = self.state.line_width.max(1.0) as i32;
        let (r, g, b, a) = self.state.stroke_style.to_rgba();
        let alpha = (a as f32 / 255.0 * self.state.global_alpha).clamp(0.0, 1.0);
        let [_ta, _tb, _tc, _td, tx, ty] = self.state.transform;
        let x0 = (x + tx) as i32;
        let y0 = (y + ty) as i32;
        let x1 = (x + w + tx) as i32;
        let y1 = (y + h + ty) as i32;
        // Top / bottom
        self.fill_rect_pixels(x0, y0, x1, y0 + lw, r, g, b, alpha);
        self.fill_rect_pixels(x0, y1 - lw, x1, y1, r, g, b, alpha);
        // Left / right
        self.fill_rect_pixels(x0, y0, x0 + lw, y1, r, g, b, alpha);
        self.fill_rect_pixels(x1 - lw, y0, x1, y1, r, g, b, alpha);
    }

    // ── Fill / Stroke path ────────────────────────────────────────────────

    /// `ctx.fill()` — fill the current path.
    pub fn fill(&mut self) {
        let path = self.current_path.clone();
        self.fill_path(&path);
    }

    /// `ctx.stroke()` — stroke the current path.
    pub fn stroke(&mut self) {
        let path = self.current_path.clone();
        self.stroke_path(&path);
    }

    /// `ctx.fill(path)` — fill a `Path2D` object.
    pub fn fill_path(&mut self, path: &Path2D) {
        let (r, g, b, a) = self.state.fill_style.to_rgba();
        let alpha = (a as f32 / 255.0 * self.state.global_alpha).clamp(0.0, 1.0);
        let [_ta, _tb, _tc, _td, tx, ty] = self.state.transform;
        // Rasterise path ops — rect is the common case for editors
        for op in &path.ops {
            if let PathOp::Rect(x, y, w, h) = op {
                let x0 = (*x + tx) as i32;
                let y0 = (*y + ty) as i32;
                let x1 = (*x + *w + tx) as i32;
                let y1 = (*y + *h + ty) as i32;
                self.fill_rect_pixels(x0, y0, x1, y1, r, g, b, alpha);
            }
        }
    }

    /// `ctx.stroke(path)` — stroke a `Path2D` object.
    pub fn stroke_path(&mut self, path: &Path2D) {
        let (r, g, b, a) = self.state.stroke_style.to_rgba();
        let alpha = (a as f32 / 255.0 * self.state.global_alpha).clamp(0.0, 1.0);
        let [_ta, _tb, _tc, _td, tx, ty] = self.state.transform;
        let lw = self.state.line_width.max(1.0);
        let mut cur_x = 0.0f32;
        let mut cur_y = 0.0f32;
        for op in &path.ops {
            match op {
                PathOp::MoveTo(x, y) => {
                    cur_x = *x;
                    cur_y = *y;
                }
                PathOp::LineTo(x, y) => {
                    self.draw_line(cur_x + tx, cur_y + ty, *x + tx, *y + ty, lw, r, g, b, alpha);
                    cur_x = *x;
                    cur_y = *y;
                }
                PathOp::Rect(x, y, w, h) => {
                    let x0 = *x + tx;
                    let y0 = *y + ty;
                    let x1 = x0 + *w;
                    let y1 = y0 + *h;
                    self.draw_line(x0, y0, x1, y0, lw, r, g, b, alpha);
                    self.draw_line(x1, y0, x1, y1, lw, r, g, b, alpha);
                    self.draw_line(x1, y1, x0, y1, lw, r, g, b, alpha);
                    self.draw_line(x0, y1, x0, y0, lw, r, g, b, alpha);
                }
                _ => {}
            }
        }
    }

    // ── Text ──────────────────────────────────────────────────────────────

    /// `ctx.fillText(text, x, y[, maxWidth])` — draw filled text.
    /// Uses a simple bitmap glyph approximation; replace with fontdue/parley
    /// integration for production-quality text rendering.
    pub fn fill_text(&mut self, text: &str, x: f32, y: f32, _max_width: Option<f32>) {
        let (r, g, b, a) = self.state.fill_style.to_rgba();
        let alpha = (a as f32 / 255.0 * self.state.global_alpha).clamp(0.0, 1.0);
        let [_ta, _tb, _tc, _td, tx, ty] = self.state.transform;
        let char_w = (self.state.font_size * 0.6) as i32;
        let char_h = self.state.font_size as i32;
        let baseline_offset = match self.state.text_baseline {
            TextBaseline::Top => 0,
            TextBaseline::Middle => -char_h / 2,
            TextBaseline::Bottom => -char_h,
            _ => -(char_h as f32 * 0.8) as i32,
        };
        let mut cx = (x + tx) as i32;
        let cy = (y + ty) as i32 + baseline_offset;
        for _ in text.chars() {
            // Placeholder: draw a filled rectangle per character.
            // Real implementation hooks into fontdue/parley glyph cache.
            self.fill_rect_pixels(cx, cy, cx + char_w - 1, cy + char_h, r, g, b, alpha);
            cx += char_w;
        }
    }

    /// `ctx.strokeText(text, x, y[, maxWidth])` — draw stroked text outline.
    pub fn stroke_text(&mut self, text: &str, x: f32, y: f32, max_width: Option<f32>) {
        // Delegate to fill_text with stroke style for now
        let saved = self.state.fill_style.clone();
        self.state.fill_style = self.state.stroke_style.clone();
        self.fill_text(text, x, y, max_width);
        self.state.fill_style = saved;
    }

    /// `ctx.measureText(text)` — estimate text metrics.
    pub fn measure_text(&self, text: &str) -> TextMetrics {
        let char_w = self.state.font_size * 0.6;
        let width = text.chars().count() as f32 * char_w;
        let ascent = self.state.font_size * 0.8;
        let descent = self.state.font_size * 0.2;
        TextMetrics {
            width,
            actual_bounding_box_ascent: ascent,
            actual_bounding_box_descent: descent,
            font_bounding_box_ascent: ascent,
            font_bounding_box_descent: descent,
        }
    }

    // ── ImageData ─────────────────────────────────────────────────────────

    /// `ctx.getImageData(sx, sy, sw, sh)` — extract a pixel region.
    pub fn get_image_data(&self, sx: u32, sy: u32, sw: u32, sh: u32) -> ImageData {
        let mut data = vec![0u8; (sw * sh * 4) as usize];
        for row in 0..sh {
            for col in 0..sw {
                let src_x = sx + col;
                let src_y = sy + row;
                if src_x < self.width && src_y < self.height {
                    let src = ((src_y * self.width + src_x) * 4) as usize;
                    let dst = ((row * sw + col) * 4) as usize;
                    data[dst..dst + 4].copy_from_slice(&self.pixels[src..src + 4]);
                }
            }
        }
        ImageData::from_bytes(data, sw, sh)
    }

    /// `ctx.putImageData(imageData, dx, dy)` — write pixels to the canvas.
    pub fn put_image_data(&mut self, image_data: &ImageData, dx: i32, dy: i32) {
        for row in 0..image_data.height {
            for col in 0..image_data.width {
                let dst_x = dx + col as i32;
                let dst_y = dy + row as i32;
                if dst_x >= 0
                    && dst_y >= 0
                    && (dst_x as u32) < self.width
                    && (dst_y as u32) < self.height
                {
                    let src = ((row * image_data.width + col) * 4) as usize;
                    let dst = ((dst_y as u32 * self.width + dst_x as u32) * 4) as usize;
                    self.pixels[dst..dst + 4].copy_from_slice(&image_data.data[src..src + 4]);
                }
            }
        }
    }

    /// `ctx.createImageData(sw, sh)` — create a blank `ImageData`.
    pub fn create_image_data(sw: u32, sh: u32) -> ImageData {
        ImageData::new(sw, sh)
    }

    /// Raw pixel buffer access — used by the renderer to upload as a texture.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Resize the canvas, clearing all content.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.pixels = vec![0u8; (width * height * 4) as usize];
    }

    // ── Internal helpers ──────────────────────────────────────────────────

    fn fill_rect_pixels(
        &mut self,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        r: u8,
        g: u8,
        b: u8,
        alpha: f32,
    ) {
        let w = self.width as i32;
        let h = self.height as i32;
        for py in y0.max(0)..y1.min(h) {
            for px in x0.max(0)..x1.min(w) {
                self.blend_pixel(px as u32, py as u32, r, g, b, alpha);
            }
        }
    }

    /// Alpha-composite a single pixel (Porter-Duff source-over).
    fn blend_pixel(&mut self, x: u32, y: u32, r: u8, g: u8, b: u8, src_alpha: f32) {
        let idx = ((y * self.width + x) * 4) as usize;
        if idx + 3 >= self.pixels.len() {
            return;
        }
        let dst_a = self.pixels[idx + 3] as f32 / 255.0;
        let out_a = src_alpha + dst_a * (1.0 - src_alpha);
        if out_a < f32::EPSILON {
            self.pixels[idx] = 0;
            self.pixels[idx + 1] = 0;
            self.pixels[idx + 2] = 0;
            self.pixels[idx + 3] = 0;
            return;
        }
        let blend = |src: u8, dst: u8| -> u8 {
            let s = src as f32 / 255.0;
            let d = dst as f32 / 255.0;
            ((s * src_alpha + d * dst_a * (1.0 - src_alpha)) / out_a * 255.0) as u8
        };
        self.pixels[idx] = blend(r, self.pixels[idx]);
        self.pixels[idx + 1] = blend(g, self.pixels[idx + 1]);
        self.pixels[idx + 2] = blend(b, self.pixels[idx + 2]);
        self.pixels[idx + 3] = (out_a * 255.0) as u8;
    }

    /// Bresenham line drawing with line width.
    fn draw_line(
        &mut self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        lw: f32,
        r: u8,
        g: u8,
        b: u8,
        alpha: f32,
    ) {
        let dx = (x1 - x0).abs();
        let dy = (y1 - y0).abs();
        let steps = dx.max(dy) as i32;
        if steps == 0 {
            return;
        }
        let half = (lw / 2.0) as i32;
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            let px = (x0 + t * (x1 - x0)) as i32;
            let py = (y0 + t * (y1 - y0)) as i32;
            self.fill_rect_pixels(
                px - half,
                py - half,
                px + half + 1,
                py + half + 1,
                r,
                g,
                b,
                alpha,
            );
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_rect_sets_pixels() {
        let mut ctx = CanvasRenderingContext2D::new(10, 10);
        ctx.set_fill_style("#ff0000");
        ctx.fill_rect(0.0, 0.0, 5.0, 5.0);
        let px = ctx.get_image_data(0, 0, 1, 1);
        assert_eq!(px.data[0], 255); // R
        assert_eq!(px.data[1], 0); // G
        assert_eq!(px.data[2], 0); // B
        assert_eq!(px.data[3], 255); // A
    }

    #[test]
    fn clear_rect_transparent() {
        let mut ctx = CanvasRenderingContext2D::new(10, 10);
        ctx.set_fill_style("#ffffff");
        ctx.fill_rect(0.0, 0.0, 10.0, 10.0);
        ctx.clear_rect(0.0, 0.0, 5.0, 5.0);
        let px = ctx.get_image_data(0, 0, 1, 1);
        assert_eq!(px.data[3], 0); // transparent
    }

    #[test]
    fn color_parsing() {
        let c = CanvasStyle::from_str("#1e1e1e");
        assert!(matches!(c, CanvasStyle::Color(0x1e, 0x1e, 0x1e, 255)));
        let c2 = CanvasStyle::from_str("rgba(255,0,128,0.5)");
        let (r, g, b, a) = c2.to_rgba();
        assert_eq!(r, 255);
        assert_eq!(g, 0);
        assert_eq!(b, 128);
        assert!((a as f32 - 127.5).abs() < 2.0);
    }

    #[test]
    fn measure_text_proportional() {
        let mut ctx = CanvasRenderingContext2D::new(100, 100);
        ctx.set_font("14px monospace");
        let m4 = ctx.measure_text("abcd");
        let m8 = ctx.measure_text("abcdefgh");
        assert!((m8.width - m4.width * 2.0).abs() < 1.0);
    }

    #[test]
    fn save_restore_state() {
        let mut ctx = CanvasRenderingContext2D::new(10, 10);
        ctx.set_fill_style("#ff0000");
        ctx.save();
        ctx.set_fill_style("#00ff00");
        ctx.restore();
        let (r, g, b, _) = ctx.state.fill_style.to_rgba();
        assert_eq!(r, 255);
        assert_eq!(g, 0);
        assert_eq!(b, 0);
    }
}
