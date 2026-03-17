use serde::{Deserialize, Serialize};

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
    _config: &CaptureConfig,
) -> AnnotatedScreenshot {
    // For Phase 1: return raw pixels as PNG without drawing annotations.
    // Phase 2 will draw numbered circles on the image.
    let png_data = encode_png(pixels, width, height);

    AnnotatedScreenshot {
        png_data,
        width,
        height,
        annotations,
    }
}

fn encode_png(pixels: &[u8], width: u32, height: u32) -> Vec<u8> {
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
