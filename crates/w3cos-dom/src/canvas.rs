use serde::{Deserialize, Serialize};
use w3cos_std::color::Color;

/// CanvasRenderingContext2D — a pixel-level drawing surface.
///
/// Maps Canvas 2D API calls to an internal command buffer.
/// The runtime drains the command buffer and executes the drawing operations
/// on a `Pixmap` (tiny-skia) or `Scene` (Vello) during `render_frame()`.
#[derive(Debug, Clone)]
pub struct CanvasRenderingContext2D {
    pub width: u32,
    pub height: u32,
    pub commands: Vec<CanvasCommand>,
    state_stack: Vec<CanvasState>,
    pub(crate) state: CanvasState,
    current_path: Vec<PathSegment>,
}

#[derive(Debug, Clone)]
pub(crate) struct CanvasState {
    pub fill_style: CanvasColor,
    pub stroke_style: CanvasColor,
    pub line_width: f32,
    pub line_cap: LineCap,
    pub line_join: LineJoin,
    pub font: String,
    pub font_size: f32,
    pub text_align: TextAlign,
    pub text_baseline: TextBaseline,
    pub global_alpha: f32,
    pub global_composite_operation: CompositeOp,
    pub transform: [f32; 6],
}

impl Default for CanvasState {
    fn default() -> Self {
        Self {
            fill_style: CanvasColor::Rgba(0, 0, 0, 255),
            stroke_style: CanvasColor::Rgba(0, 0, 0, 255),
            line_width: 1.0,
            line_cap: LineCap::Butt,
            line_join: LineJoin::Miter,
            font: "16px sans-serif".to_string(),
            font_size: 16.0,
            text_align: TextAlign::Start,
            text_baseline: TextBaseline::Alphabetic,
            global_alpha: 1.0,
            global_composite_operation: CompositeOp::SourceOver,
            transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CanvasColor {
    Rgba(u8, u8, u8, u8),
}

impl CanvasColor {
    pub fn from_css(s: &str) -> Self {
        let c = Color::from_hex(s);
        Self::Rgba(c.r, c.g, c.b, c.a)
    }

    pub fn to_rgba(&self) -> (u8, u8, u8, u8) {
        match self {
            Self::Rgba(r, g, b, a) => (*r, *g, *b, *a),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum LineCap {
    Butt,
    Round,
    Square,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum LineJoin {
    Miter,
    Round,
    Bevel,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum TextAlign {
    Start,
    End,
    Left,
    Right,
    Center,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum TextBaseline {
    Top,
    Hanging,
    Middle,
    Alphabetic,
    Ideographic,
    Bottom,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum CompositeOp {
    SourceOver,
    SourceAtop,
    SourceIn,
    SourceOut,
    DestinationOver,
    DestinationAtop,
    DestinationIn,
    DestinationOut,
    Lighter,
    Copy,
    Xor,
}

/// Each Canvas 2D API call is recorded as a command.
/// The runtime processes these during rendering.
#[derive(Debug, Clone)]
pub enum CanvasCommand {
    FillRect { x: f32, y: f32, w: f32, h: f32, color: CanvasColor, alpha: f32 },
    StrokeRect { x: f32, y: f32, w: f32, h: f32, color: CanvasColor, line_width: f32, alpha: f32 },
    ClearRect { x: f32, y: f32, w: f32, h: f32 },
    FillPath { segments: Vec<PathSegment>, color: CanvasColor, alpha: f32 },
    StrokePath { segments: Vec<PathSegment>, color: CanvasColor, line_width: f32, alpha: f32 },
    FillText { text: String, x: f32, y: f32, color: CanvasColor, font_size: f32, alpha: f32 },
    StrokeText { text: String, x: f32, y: f32, color: CanvasColor, font_size: f32, line_width: f32, alpha: f32 },
    DrawImage { image_data: Vec<u8>, dx: f32, dy: f32, dw: Option<f32>, dh: Option<f32> },
    SetTransform { a: f32, b: f32, c: f32, d: f32, e: f32, f: f32 },
}

#[derive(Debug, Clone)]
pub enum PathSegment {
    MoveTo(f32, f32),
    LineTo(f32, f32),
    Arc { cx: f32, cy: f32, r: f32, start_angle: f32, end_angle: f32, counter_clockwise: bool },
    ClosePath,
}

impl CanvasRenderingContext2D {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            commands: Vec::new(),
            state_stack: Vec::new(),
            state: CanvasState::default(),
            current_path: Vec::new(),
        }
    }

    // --- Drawing rectangles ---

    pub fn fill_rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        self.commands.push(CanvasCommand::FillRect {
            x, y, w, h,
            color: self.state.fill_style.clone(),
            alpha: self.state.global_alpha,
        });
    }

    pub fn stroke_rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        self.commands.push(CanvasCommand::StrokeRect {
            x, y, w, h,
            color: self.state.stroke_style.clone(),
            line_width: self.state.line_width,
            alpha: self.state.global_alpha,
        });
    }

    pub fn clear_rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        self.commands.push(CanvasCommand::ClearRect { x, y, w, h });
    }

    // --- Path operations ---

    pub fn begin_path(&mut self) {
        self.current_path.clear();
    }

    pub fn move_to(&mut self, x: f32, y: f32) {
        self.current_path.push(PathSegment::MoveTo(x, y));
    }

    pub fn line_to(&mut self, x: f32, y: f32) {
        self.current_path.push(PathSegment::LineTo(x, y));
    }

    pub fn arc(&mut self, cx: f32, cy: f32, r: f32, start_angle: f32, end_angle: f32, counter_clockwise: bool) {
        self.current_path.push(PathSegment::Arc {
            cx, cy, r, start_angle, end_angle, counter_clockwise,
        });
    }

    pub fn close_path(&mut self) {
        self.current_path.push(PathSegment::ClosePath);
    }

    pub fn fill(&mut self) {
        let segments = std::mem::take(&mut self.current_path);
        self.commands.push(CanvasCommand::FillPath {
            segments,
            color: self.state.fill_style.clone(),
            alpha: self.state.global_alpha,
        });
    }

    pub fn stroke(&mut self) {
        let segments = std::mem::take(&mut self.current_path);
        self.commands.push(CanvasCommand::StrokePath {
            segments,
            color: self.state.stroke_style.clone(),
            line_width: self.state.line_width,
            alpha: self.state.global_alpha,
        });
    }

    // --- Text ---

    pub fn fill_text(&mut self, text: &str, x: f32, y: f32) {
        self.commands.push(CanvasCommand::FillText {
            text: text.to_string(),
            x, y,
            color: self.state.fill_style.clone(),
            font_size: self.state.font_size,
            alpha: self.state.global_alpha,
        });
    }

    pub fn stroke_text(&mut self, text: &str, x: f32, y: f32) {
        self.commands.push(CanvasCommand::StrokeText {
            text: text.to_string(),
            x, y,
            color: self.state.stroke_style.clone(),
            font_size: self.state.font_size,
            line_width: self.state.line_width,
            alpha: self.state.global_alpha,
        });
    }

    pub fn measure_text(&self, text: &str) -> TextMetrics {
        let approx_width = text.len() as f32 * self.state.font_size * 0.6;
        TextMetrics { width: approx_width }
    }

    // --- Style setters ---

    pub fn set_fill_style(&mut self, color: &str) {
        self.state.fill_style = CanvasColor::from_css(color);
    }

    pub fn set_stroke_style(&mut self, color: &str) {
        self.state.stroke_style = CanvasColor::from_css(color);
    }

    pub fn set_line_width(&mut self, width: f32) {
        self.state.line_width = width;
    }

    pub fn set_font(&mut self, font: &str) {
        self.state.font = font.to_string();
        if let Some(size) = parse_font_size(font) {
            self.state.font_size = size;
        }
    }

    pub fn set_text_align(&mut self, align: TextAlign) {
        self.state.text_align = align;
    }

    pub fn set_text_baseline(&mut self, baseline: TextBaseline) {
        self.state.text_baseline = baseline;
    }

    pub fn set_global_alpha(&mut self, alpha: f32) {
        self.state.global_alpha = alpha.clamp(0.0, 1.0);
    }

    // --- Transform ---

    pub fn save(&mut self) {
        self.state_stack.push(self.state.clone());
    }

    pub fn restore(&mut self) {
        if let Some(state) = self.state_stack.pop() {
            self.state = state;
        }
    }

    pub fn translate(&mut self, x: f32, y: f32) {
        self.state.transform[4] += x;
        self.state.transform[5] += y;
        self.commands.push(CanvasCommand::SetTransform {
            a: self.state.transform[0],
            b: self.state.transform[1],
            c: self.state.transform[2],
            d: self.state.transform[3],
            e: self.state.transform[4],
            f: self.state.transform[5],
        });
    }

    pub fn scale(&mut self, sx: f32, sy: f32) {
        self.state.transform[0] *= sx;
        self.state.transform[3] *= sy;
        self.commands.push(CanvasCommand::SetTransform {
            a: self.state.transform[0],
            b: self.state.transform[1],
            c: self.state.transform[2],
            d: self.state.transform[3],
            e: self.state.transform[4],
            f: self.state.transform[5],
        });
    }

    pub fn rotate(&mut self, angle: f32) {
        let cos = angle.cos();
        let sin = angle.sin();
        let a = self.state.transform[0];
        let b = self.state.transform[1];
        let c = self.state.transform[2];
        let d = self.state.transform[3];
        self.state.transform[0] = a * cos + c * sin;
        self.state.transform[1] = b * cos + d * sin;
        self.state.transform[2] = a * -sin + c * cos;
        self.state.transform[3] = b * -sin + d * cos;
        self.commands.push(CanvasCommand::SetTransform {
            a: self.state.transform[0],
            b: self.state.transform[1],
            c: self.state.transform[2],
            d: self.state.transform[3],
            e: self.state.transform[4],
            f: self.state.transform[5],
        });
    }

    // --- Image data ---

    pub fn get_image_data(&self) -> ImageData {
        ImageData {
            width: self.width,
            height: self.height,
            data: vec![0u8; (self.width * self.height * 4) as usize],
        }
    }

    pub fn create_image_data(&self, width: u32, height: u32) -> ImageData {
        ImageData {
            width,
            height,
            data: vec![0u8; (width * height * 4) as usize],
        }
    }

    /// Take and reset the command buffer. Called by the renderer after processing.
    pub fn drain_commands(&mut self) -> Vec<CanvasCommand> {
        std::mem::take(&mut self.commands)
    }

}

#[derive(Debug, Clone)]
pub struct TextMetrics {
    pub width: f32,
}

#[derive(Debug, Clone)]
pub struct ImageData {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

fn parse_font_size(font: &str) -> Option<f32> {
    for part in font.split_whitespace() {
        if let Some(stripped) = part.strip_suffix("px") {
            if let Ok(size) = stripped.parse::<f32>() {
                return Some(size);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_canvas_context() {
        let ctx = CanvasRenderingContext2D::new(800, 600);
        assert_eq!(ctx.width, 800);
        assert_eq!(ctx.height, 600);
        assert!(ctx.commands.is_empty());
    }

    #[test]
    fn fill_rect_adds_command() {
        let mut ctx = CanvasRenderingContext2D::new(100, 100);
        ctx.fill_rect(10.0, 20.0, 30.0, 40.0);
        assert_eq!(ctx.commands.len(), 1);
        assert!(matches!(ctx.commands[0], CanvasCommand::FillRect { .. }));
    }

    #[test]
    fn stroke_rect_adds_command() {
        let mut ctx = CanvasRenderingContext2D::new(100, 100);
        ctx.stroke_rect(0.0, 0.0, 50.0, 50.0);
        assert_eq!(ctx.commands.len(), 1);
    }

    #[test]
    fn clear_rect_adds_command() {
        let mut ctx = CanvasRenderingContext2D::new(100, 100);
        ctx.clear_rect(0.0, 0.0, 100.0, 100.0);
        assert_eq!(ctx.commands.len(), 1);
    }

    #[test]
    fn fill_text_adds_command() {
        let mut ctx = CanvasRenderingContext2D::new(200, 200);
        ctx.fill_text("Hello", 10.0, 50.0);
        assert_eq!(ctx.commands.len(), 1);
    }

    #[test]
    fn measure_text_returns_width() {
        let ctx = CanvasRenderingContext2D::new(200, 200);
        let metrics = ctx.measure_text("Hello");
        assert!(metrics.width > 0.0);
    }

    #[test]
    fn set_fill_style() {
        let mut ctx = CanvasRenderingContext2D::new(100, 100);
        ctx.set_fill_style("#ff0000");
        let (r, g, b, a) = ctx.state.fill_style.to_rgba();
        assert_eq!(r, 255);
        assert_eq!(g, 0);
        assert_eq!(b, 0);
        assert_eq!(a, 255);
    }

    #[test]
    fn save_restore_state() {
        let mut ctx = CanvasRenderingContext2D::new(100, 100);
        ctx.set_fill_style("#ff0000");
        ctx.set_global_alpha(0.5);
        ctx.save();
        ctx.set_fill_style("#00ff00");
        ctx.set_global_alpha(1.0);
        ctx.restore();
        let (r, _, _, _) = ctx.state.fill_style.to_rgba();
        assert_eq!(r, 255);
        assert_eq!(ctx.state.global_alpha, 0.5);
    }

    #[test]
    fn drain_commands_clears() {
        let mut ctx = CanvasRenderingContext2D::new(100, 100);
        ctx.fill_rect(0.0, 0.0, 10.0, 10.0);
        ctx.fill_rect(20.0, 20.0, 10.0, 10.0);
        let cmds = ctx.drain_commands();
        assert_eq!(cmds.len(), 2);
        assert!(ctx.commands.is_empty());
    }

    #[test]
    fn translate_updates_transform() {
        let mut ctx = CanvasRenderingContext2D::new(100, 100);
        ctx.translate(50.0, 30.0);
        assert_eq!(ctx.state.transform[4], 50.0);
        assert_eq!(ctx.state.transform[5], 30.0);
    }

    #[test]
    fn scale_updates_transform() {
        let mut ctx = CanvasRenderingContext2D::new(100, 100);
        ctx.scale(2.0, 3.0);
        assert_eq!(ctx.state.transform[0], 2.0);
        assert_eq!(ctx.state.transform[3], 3.0);
    }

    #[test]
    fn parse_font_size_px() {
        assert_eq!(parse_font_size("16px sans-serif"), Some(16.0));
        assert_eq!(parse_font_size("24px Arial"), Some(24.0));
        assert_eq!(parse_font_size("bold 14px monospace"), Some(14.0));
        assert_eq!(parse_font_size("no-size-here"), None);
    }

    #[test]
    fn image_data_size() {
        let ctx = CanvasRenderingContext2D::new(100, 50);
        let img = ctx.get_image_data();
        assert_eq!(img.width, 100);
        assert_eq!(img.height, 50);
        assert_eq!(img.data.len(), 100 * 50 * 4);
    }
}
