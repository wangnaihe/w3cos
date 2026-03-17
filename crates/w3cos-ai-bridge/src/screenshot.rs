use serde::{Deserialize, Serialize};
use tiny_skia::{Color, Paint, PathBuilder, Pixmap, Rect, Transform};
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

/// Font metrics for annotation rendering.
const CIRCLE_RADIUS: f32 = 12.0;
const CIRCLE_BG: Color = Color::from_rgba8(220, 50, 47, 230);

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
            // Fallback: return raw PNG without annotations
            let png_data = encode_raw_png(pixels, width, height);
            return AnnotatedScreenshot {
                png_data,
                width,
                height,
                annotations,
            };
        }
    };

    // Copy source pixels into the pixmap
    let dst = pixmap.pixels_mut();
    let copy_len = (width as usize * height as usize * 4).min(pixels.len());
    dst[..copy_len].copy_from_slice(&pixels[..copy_len]);

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
    // Load embedded font (monospace fallback bitmap)
    let font_data = include_bytes!("../../w3cos-runtime/assets/Inter-Regular.ttf");
    let font = Font::from_bytes(font_data as &[u8], fontdue::FontSettings::default())
        .expect("failed to load Inter font");

    for ann in annotations.iter().filter(|a| a.interactive) {
        let cx = ann.bounds[0] + ann.bounds[2] / 2.0;
        let cy = ann.bounds[1] + ann.bounds[3] / 2.0;
        let r = CIRCLE_RADIUS;

        // Draw filled circle (background)
        let mut paint = Paint::default();
        paint.set_color(CIRCLE_BG);
        paint.anti_alias = true;

        let mut pb = PathBuilder::new();
        pb.push_circle(cx, cy, r);
        let path = pb.finish().unwrap();
        pixmap.fill_path(&path, &paint, Transform::identity(), None);

        // Render number text
        let text = ann.index.to_string();
        draw_text(pixmap, &font, &text, cx, cy, r);
    }
}

/// Draw text centered in a circle using fontdue rasterization.
/// Uses the same pixel blending approach as w3cos-runtime/src/render.rs.
fn draw_text(pixmap: &mut Pixmap, font: &Font, text: &str, cx: f32, cy: f32, circle_r: f32) {
    let font_size = circle_r * 1.2;
    let (metrics, bitmap) = font.rasterize(text, font_size);

    let px_w = pixmap.width() as i32;
    let px_h = pixmap.height() as i32;

    let offset_x = cx - metrics.advance_width / 2.0 - metrics.xmin as f32;
    let offset_y = cy - metrics.height as f32 / 2.0 - metrics.ymin as f32;

    // Source color (white text)
    let sr: u8 = 255;
    let sg: u8 = 255;
    let sb: u8 = 255;

    let pixels = pixmap.pixels_mut();
    for row in 0..metrics.height {
        for col in 0..metrics.width {
            let px = (offset_x + col as f32) as i32;
            let py = (offset_y + row as f32) as i32;
            if px < 0 || py < 0 || px >= px_w || py >= px_h {
                continue;
            }
            let coverage = bitmap[row * metrics.width + col];
            if coverage == 0 {
                continue;
            }
            let idx = (py as usize * px_w as usize + px as usize) * 4;
            let dst = tiny_skia::PremultipliedColorU8::from_rgba(
                pixels[idx],
                pixels[idx + 1],
                pixels[idx + 2],
                pixels[idx + 3],
            ).unwrap();

            let blended = blend_pixel(dst, sr, sg, sb, coverage);
            let out = blended.to_bytes();
            pixels[idx] = out[0];
            pixels[idx + 1] = out[1];
            pixels[idx + 2] = out[2];
            pixels[idx + 3] = out[3];
        }
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
