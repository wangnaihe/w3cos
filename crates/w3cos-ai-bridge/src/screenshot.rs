use serde::{Deserialize, Serialize};
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Rect, Transform};
use fontdue::Font;

/// Layer 3: Annotated screenshot API.
/// For compatibility with external AI tools (Claude Computer Use, UI-TARS, etc.)
/// that expect visual input.

#[derive(Debug, Serialize, Deserialize)]
pub struct AnnotatedScreenshot {
    /// Raw PNG image bytes.
    pub png_data: Vec<u8>,
    /// Width in physical pixels.
    pub width: u32,
    /// Height in physical pixels.
    pub height: u32,
    /// Element annotations: numbered markers on the screenshot.
    pub annotations: Vec<ElementAnnotation>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ElementAnnotation {
    /// Sequential number displayed on the screenshot (e.g., "1", "2", "3").
    pub index: u32,
    /// DOM node ID.
    pub node_id: u32,
    /// Element description (role + name).
    pub label: String,
    /// Bounding box [x, y, width, height] in physical pixels.
    pub bounds: [f32; 4],
    /// Whether the element is interactive.
    pub interactive: bool,
}

/// Configuration for screenshot capture.
#[derive(Debug, Default)]
pub struct CaptureConfig {
    /// Draw numbered markers on interactive elements.
    pub annotate_interactive: bool,
    /// Draw outlines around all elements.
    pub show_outlines: bool,
    /// Include element index map in response.
    pub include_map: bool,
}

/// Circle radius for annotation markers.
const CIRCLE_RADIUS: f32 = 12.0;

/// Returns the background color for annotation circles.
fn circle_bg() -> Color {
    Color::from_rgba8(220, 50, 47, 230)
}

/// Capture an annotated screenshot from a rendered pixmap.
///
/// This takes the raw pixel buffer + layout info and produces:
/// 1. A PNG with numbered annotations drawn on top
/// 2. A mapping from annotation numbers to DOM elements
pub fn capture(
    pixels: &[u8],
    width: u32,
    height: u32,
    annotations: Vec<ElementAnnotation>,
    config: &CaptureConfig,
) -> AnnotatedScreenshot {
    let mut pixmap = match Pixmap::new(width, height) {
        Some(p) => p,
        None => {
            let png_data = encode_raw_png(pixels, width, height);
            return AnnotatedScreenshot {
                png_data,
                width,
                height,
                annotations,
            };
        }
    };

    // Copy source pixels into the pixmap.
    // Pixmap stores PremultipliedColorU8 internally, but new pixmap starts zeroed,
    // so we need to set each pixel from the raw RGBA source data.
    let dst = pixmap.pixels_mut();
    for i in 0..pixels.len().min(dst.len() / 4) {
        dst[i] = tiny_skia::PremultipliedColorU8::from_rgba(
            pixels[i * 4],
            pixels[i * 4 + 1],
            pixels[i * 4 + 2],
            pixels[i * 4 + 3],
        ).unwrap();
    }

    if config.annotate_interactive {
        draw_annotations(&mut pixmap, &annotations);
    }

    if config.show_outlines {
        draw_outlines(&mut pixmap, &annotations);
    }

    let png_data = encode_pixmap(&pixmap);

    AnnotatedScreenshot {
        png_data,
        width,
        height,
        annotations,
    }
}

/// Draw numbered circle markers on interactive elements.
fn draw_annotations(pixmap: &mut Pixmap, annotations: &[ElementAnnotation]) {
    let font_data = include_bytes!("../../w3cos-runtime/assets/Inter-Regular.ttf");
    let font = Font::from_bytes(font_data as &[u8], fontdue::FontSettings::default())
        .expect("failed to load Inter font");

    for ann in annotations.iter().filter(|a| a.interactive) {
        let cx = ann.bounds[0] + ann.bounds[2] / 2.0;
        let cy = ann.bounds[1] + ann.bounds[3] / 2.0;
        let r = CIRCLE_RADIUS;

        // Draw filled circle
        let mut paint = Paint::default();
        paint.set_color(circle_bg());
        paint.anti_alias = true;

        let mut pb = PathBuilder::new();
        pb.push_circle(cx, cy, r);
        let path = pb.finish().unwrap();
        // fill_path signature: (path, paint, transform, mask, fill_rule)
        pixmap.fill_path(&path, &paint, Transform::identity(), None, FillRule::Winding);

        // Render number text
        let text = ann.index.to_string();
        draw_text(pixmap, &font, &text, cx, cy, r);
    }
}

/// Draw text centered in a circle using fontdue rasterization.
/// Uses the same pixel blending approach as w3cos-runtime/src/render.rs.
fn draw_text(pixmap: &mut Pixmap, font: &Font, text: &str, cx: f32, cy: f32, circle_r: f32) {
    let font_size = circle_r * 1.2;
    // fontdue rasterize takes each char separately
    let mut total_width = 0.0f32;
    let mut glyphs: Vec<(fontdue::GlyphLayout, Vec<u8>)> = Vec::new();

    for ch in text.chars() {
        let (metrics, bitmap) = font.rasterize(ch, font_size);
        glyphs.push((metrics, bitmap));
        total_width += metrics.advance_width;
    }

    let px_w = pixmap.width() as i32;
    let px_h = pixmap.height() as i32;
    let total_height = glyphs.first().map(|(m, _)| m.height).unwrap_or(0) as f32;
    let y_min = glyphs.first().map(|(m, _)| m.ymin).unwrap_or(0) as f32;

    let start_x = cx - total_width / 2.0;
    let start_y = cy - total_height / 2.0 - y_min;

    // Source color (white text)
    let sr: u8 = 255;
    let sg: u8 = 255;
    let sb: u8 = 255;

    let mut cursor_x = start_x;

    for (metrics, bitmap) in &glyphs {
        for row in 0..metrics.height {
            for col in 0..metrics.width {
                let px = (cursor_x + col as f32) as i32;
                let py = (start_y + row as f32) as i32;
                if px < 0 || py < 0 || px >= px_w || py >= px_h {
                    continue;
                }
                let coverage = bitmap[row * metrics.width + col];
                if coverage == 0 {
                    continue;
                }
                let idx = (py as usize * px_w as usize + px as usize);
                let dst = pixmap.pixels_mut()[idx];

                let blended = blend_pixel(dst, sr, sg, sb, coverage);
                pixmap.pixels_mut()[idx] = blended;
            }
        }
        cursor_x += metrics.advance_width;
    }
}

/// Blend a source color with alpha coverage over a destination pixel.
/// Same approach as w3cos-runtime/src/render.rs::blend_pixel.
fn blend_pixel(
    dst: tiny_skia::PremultipliedColorU8,
    sr: u8,
    sg: u8,
    sb: u8,
    sa: u8,
) -> tiny_skia::PremultipliedColorU8 {
    let da = dst.alpha() as u16;
    let dr = dst.red() as u16;
    let dg = dst.green() as u16;
    let db = dst.blue() as u16;
    let sa16 = sa as u16;
    let inv = 255 - sa16;

    let out_a = (sa16 + da * inv / 255).min(255) as u8;
    let out_r = (sr as u16 * sa16 / 255 + dr * inv / 255).min(255) as u8;
    let out_g = (sg as u16 * sa16 / 255 + dg * inv / 255).min(255) as u8;
    let out_b = (sb as u16 * sa16 / 255 + db * inv / 255).min(255) as u8;

    tiny_skia::PremultipliedColorU8::from_rgba(out_r, out_g, out_b, out_a).unwrap()
}

/// Draw rectangular outlines around elements.
fn draw_outlines(pixmap: &mut Pixmap, annotations: &[ElementAnnotation]) {
    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba8(108, 92, 231, 80));
    paint.anti_alias = true;

    let stroke = tiny_skia::Stroke {
        width: 1.0,
        ..Default::default()
    };

    for ann in annotations {
        if let Some(rect) = Rect::from_xywh(ann.bounds[0], ann.bounds[1], ann.bounds[2], ann.bounds[3])
        {
            let mut pb = PathBuilder::new();
            pb.push_rect(rect);
            if let Some(path) = pb.finish() {
                let _ = pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
            }
        }
    }
}

fn encode_pixmap(pixmap: &Pixmap) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut buf, pixmap.width(), pixmap.height());
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        if let Ok(mut writer) = encoder.write_header() {
            let _ = writer.write_image_data(pixmap.data());
        }
    }
    buf
}

fn encode_raw_png(pixels: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut buf, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        if let Ok(mut writer) = encoder.write_header() {
            let _ = writer.write_image_data(pixels);
        }
    }
    buf
}
