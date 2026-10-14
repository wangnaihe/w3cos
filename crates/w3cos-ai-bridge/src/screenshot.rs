use fontdue::Font;
use serde::{Deserialize, Serialize};
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Rect, Transform};

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

    let expected_len = (width * height * 4) as usize;
    let dst = pixmap.data_mut();
    if pixels.len() >= expected_len && dst.len() >= expected_len {
        dst[..expected_len].copy_from_slice(&pixels[..expected_len]);
    } else {
        let n = pixels.len().min(dst.len());
        dst[..n].copy_from_slice(&pixels[..n]);
    }

    if config.annotate_interactive {
        draw_annotations(&mut pixmap, &annotations);
    }

    if config.show_outlines {
        draw_outlines(&mut pixmap, &annotations);
    }

    // Encode pixmap to PNG bytes.
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
        pixmap.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );

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
    let mut glyphs: Vec<(fontdue::Metrics, Vec<u8>)> = Vec::new();

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
                let idx = py as usize * px_w as usize + px as usize;
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
        if let Some(rect) =
            Rect::from_xywh(ann.bounds[0], ann.bounds[1], ann.bounds[2], ann.bounds[3])
        {
            let mut pb = PathBuilder::new();
            pb.push_rect(rect);
            if let Some(path) = pb.finish() {
                pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
            }
        }
    }
}

/// JSON body for `GET /screenshot/annotated`. Drawing happens here, when a
/// request is answered, not while a frame is painted.
pub fn annotated_json(shot: &AnnotatedScreenshot) -> String {
    serde_json::json!({
        "png_base64": base64_encode(&shot.png_data),
        "width": shot.width,
        "height": shot.height,
        "annotations": shot.annotations,
    })
    .to_string()
}

fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    let mut index = 0;
    while index + 3 <= data.len() {
        let chunk = u32::from_be_bytes([0, data[index], data[index + 1], data[index + 2]]);
        for shift in [18, 12, 6, 0] {
            out.push(TABLE[((chunk >> shift) & 0x3f) as usize] as char);
        }
        index += 3;
    }
    let rest = data.len() - index;
    if rest == 1 {
        let chunk = u32::from(data[index]) << 16;
        out.push(TABLE[((chunk >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((chunk >> 12) & 0x3f) as usize] as char);
        out.push('=');
        out.push('=');
    } else if rest == 2 {
        let chunk = (u32::from(data[index]) << 16) | (u32::from(data[index + 1]) << 8);
        out.push(TABLE[((chunk >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((chunk >> 12) & 0x3f) as usize] as char);
        out.push(TABLE[((chunk >> 6) & 0x3f) as usize] as char);
        out.push('=');
    }
    out
}

fn encode_pixmap(pixmap: &Pixmap) -> Vec<u8> {
    pixmap.encode_png().unwrap_or_default()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_pixmap_draws_two_interactive_markers() {
        let width = 48u32;
        let height = 32u32;
        let pixels = vec![255u8; (width * height * 4) as usize];
        let annotations = vec![
            ElementAnnotation {
                index: 1,
                node_id: 4,
                label: "button: Go".into(),
                bounds: [2.0, 2.0, 12.0, 12.0],
                interactive: true,
            },
            ElementAnnotation {
                index: 2,
                node_id: 5,
                label: "button: Stop".into(),
                bounds: [20.0, 8.0, 12.0, 12.0],
                interactive: true,
            },
        ];
        let shot = capture(
            &pixels,
            width,
            height,
            annotations,
            &CaptureConfig {
                annotate_interactive: true,
                show_outlines: false,
                include_map: true,
            },
        );
        assert!(shot.png_data.starts_with(b"\x89PNG"));
        assert_eq!(shot.annotations.len(), 2);
        assert_eq!(shot.annotations[0].index, 1);
        assert_eq!(shot.annotations[1].index, 2);
        let json = annotated_json(&shot);
        assert!(json.contains("\"index\":1"));
        assert!(json.contains("png_base64"));
        assert!(!json.contains("\"annotations\":[]"));
    }
}
