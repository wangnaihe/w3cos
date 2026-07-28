use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Read as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use image::AnimationDecoder;

#[derive(Clone)]
pub struct DecodedImage {
    /// Physical texture width in decoded pixels.
    pub width: u32,
    /// Physical texture height in decoded pixels.
    pub height: u32,
    /// Density-corrected CSS intrinsic width used by layout and DOM APIs.
    pub intrinsic_width: u32,
    /// Density-corrected CSS intrinsic height used by layout and DOM APIs.
    pub intrinsic_height: u32,
    pub data: Arc<Vec<u8>>,
}

thread_local! {
    static CACHE: RefCell<HashMap<String, Option<DecodedImage>>> = RefCell::new(HashMap::new());
    static ANIMATIONS: RefCell<HashMap<String, AnimatedImage>> = RefCell::new(HashMap::new());
}

#[derive(Clone)]
struct AnimatedFrame {
    duration: Duration,
    data: Arc<Vec<u8>>,
}

struct AnimatedImage {
    started: Instant,
    width: u32,
    height: u32,
    frames: Vec<AnimatedFrame>,
    cycle: Duration,
}

pub fn get_or_load(src: &str) -> Option<DecodedImage> {
    if let Some(frame) = current_animation_frame(src) {
        return Some(frame);
    }
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(entry) = cache.get(src) {
            return entry.clone();
        }
        let result = load_from_source(src);
        cache.insert(src.to_string(), result.clone());
        result
    })
}

/// Decode bytes fetched by the Browser subresource loader and publish them to
/// the same cache consumed by every renderer. `src` is the DOM-facing source
/// string, so relative image URLs do not trigger a second renderer-owned
/// network request after the Browser loader has resolved them.
pub(crate) fn decode_and_install(src: &str, bytes: &[u8]) -> Result<DecodedImage, String> {
    if looks_like_svg(bytes) {
        let decoded = decode_svg(bytes)?;
        install_decoded(src, decoded.clone());
        return Ok(decoded);
    }
    if let Ok(frames) = decode_gif_frames(bytes)
        && frames.len() > 1
    {
        let width = frames[0].0.width();
        let height = frames[0].0.height();
        let animation_frames = frames
            .into_iter()
            .map(|(rgba, duration)| AnimatedFrame {
                duration,
                data: Arc::new(rgba.into_raw()),
            })
            .collect::<Vec<_>>();
        let cycle = animation_frames
            .iter()
            .map(|frame| frame.duration)
            .sum::<Duration>();
        let decoded = DecodedImage {
            width,
            height,
            intrinsic_width: width,
            intrinsic_height: height,
            data: Arc::clone(&animation_frames[0].data),
        };
        ANIMATIONS.with(|animations| {
            animations.borrow_mut().insert(
                src.to_string(),
                AnimatedImage {
                    started: Instant::now(),
                    width,
                    height,
                    frames: animation_frames,
                    cycle,
                },
            );
        });
        install_decoded(src, decoded.clone());
        return Ok(decoded);
    }
    let image = image::load_from_memory(bytes).map_err(|error| error.to_string())?;
    let rgba = image.to_rgba8();
    if rgba.as_raw().len() > 256 * 1024 * 1024 {
        return Err("decoded image exceeds the 256 MiB safety limit".to_string());
    }
    let decoded = DecodedImage {
        width: rgba.width(),
        height: rgba.height(),
        intrinsic_width: rgba.width(),
        intrinsic_height: rgba.height(),
        data: Arc::new(rgba.into_raw()),
    };
    install_decoded(src, decoded.clone());
    Ok(decoded)
}

fn install_decoded(src: &str, decoded: DecodedImage) {
    CACHE.with(|cache| {
        cache.borrow_mut().insert(src.to_string(), Some(decoded));
    });
}

fn looks_like_svg(bytes: &[u8]) -> bool {
    let prefix = String::from_utf8_lossy(&bytes[..bytes.len().min(1024)]);
    prefix
        .trim_start_matches('\u{feff}')
        .trim_start()
        .starts_with("<svg")
        || prefix.contains("<svg ")
}

fn decode_svg(bytes: &[u8]) -> Result<DecodedImage, String> {
    let options = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(bytes, &options)
        .map_err(|error| format!("invalid SVG image document: {error}"))?;
    let size = tree.size();
    let width = size.width().ceil().clamp(1.0, 16_384.0) as u32;
    let height = size.height().ceil().clamp(1.0, 16_384.0) as u32;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| "SVG image document exceeds raster limits".to_string())?;
    let transform = resvg::tiny_skia::Transform::from_scale(
        width as f32 / size.width(),
        height as f32 / size.height(),
    );
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    let mut rgba = pixmap.data().to_vec();
    unpremultiply_rgba(&mut rgba);
    Ok(DecodedImage {
        width,
        height,
        intrinsic_width: width,
        intrinsic_height: height,
        data: Arc::new(rgba),
    })
}

fn unpremultiply_rgba(rgba: &mut [u8]) {
    for pixel in rgba.chunks_exact_mut(4) {
        let alpha = pixel[3];
        if alpha == 0 || alpha == 255 {
            continue;
        }
        for channel in &mut pixel[..3] {
            *channel = ((*channel as u32 * 255 + alpha as u32 / 2) / alpha as u32).min(255) as u8;
        }
    }
}

fn decode_gif_frames(bytes: &[u8]) -> Result<Vec<(image::RgbaImage, Duration)>, image::ImageError> {
    let decoder = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(bytes))?;
    decoder.into_frames().collect_frames().map(|frames| {
        frames
            .into_iter()
            .map(|frame| {
                let (numerator, denominator) = frame.delay().numer_denom_ms();
                let millis = (u64::from(numerator) / u64::from(denominator).max(1)).max(10);
                (frame.into_buffer(), Duration::from_millis(millis))
            })
            .collect()
    })
}

fn current_animation_frame(src: &str) -> Option<DecodedImage> {
    ANIMATIONS.with(|animations| {
        let animations = animations.borrow();
        let animation = animations.get(src)?;
        let cycle_ms = animation.cycle.as_millis().max(1);
        let elapsed_ms = animation.started.elapsed().as_millis() % cycle_ms;
        let mut boundary = 0_u128;
        let frame = animation
            .frames
            .iter()
            .find(|frame| {
                boundary += frame.duration.as_millis().max(1);
                elapsed_ms < boundary
            })
            .unwrap_or(&animation.frames[0]);
        Some(DecodedImage {
            width: animation.width,
            height: animation.height,
            intrinsic_width: animation.width,
            intrinsic_height: animation.height,
            data: Arc::clone(&frame.data),
        })
    })
}

pub(crate) fn has_active_animations() -> bool {
    ANIMATIONS.with(|animations| !animations.borrow().is_empty())
}

pub(crate) fn set_density(src: &str, density: f64) {
    if !density.is_finite() || density <= 0.0 {
        return;
    }
    CACHE.with(|cache| {
        if let Some(image) = cache.borrow_mut().get_mut(src).and_then(Option::as_mut) {
            image.intrinsic_width = ((image.width as f64 / density).round() as u32).max(1);
            image.intrinsic_height = ((image.height as f64 / density).round() as u32).max(1);
        }
    });
}

pub(crate) fn dimensions(src: &str) -> Option<(u32, u32)> {
    CACHE.with(|cache| {
        cache
            .borrow()
            .get(src)
            .and_then(Option::as_ref)
            .map(|image| (image.intrinsic_width, image.intrinsic_height))
    })
}

/// Reserve a Browser-owned source while its asynchronous fetch is pending (or
/// after it has failed), preventing paint from falling back to the legacy
/// synchronous source loader and issuing a duplicate network request.
pub(crate) fn reserve_browser_source(src: &str) {
    CACHE.with(|cache| {
        cache.borrow_mut().insert(src.to_string(), None);
    });
}

pub(crate) fn invalidate(src: &str) {
    CACHE.with(|cache| {
        cache.borrow_mut().remove(src);
    });
    ANIMATIONS.with(|animations| {
        animations.borrow_mut().remove(src);
    });
}

/// Return the sources from CSS `url(...)` image layers in paint order.
///
/// This intentionally leaves URL resolution to the Browser document loader:
/// renderers consume the exact source key installed in the shared cache.
pub(crate) fn css_image_urls(value: &str) -> Vec<String> {
    let bytes = value.as_bytes();
    let mut urls = Vec::new();
    let mut cursor = 0;
    while cursor + 4 <= bytes.len() {
        if !value[cursor..].to_ascii_lowercase().starts_with("url(") {
            cursor += value[cursor..]
                .chars()
                .next()
                .map(char::len_utf8)
                .unwrap_or(1);
            continue;
        }
        let start = cursor + 4;
        let mut end = start;
        let mut quote = None;
        while end < bytes.len() {
            let ch = bytes[end] as char;
            if let Some(active) = quote {
                if ch == '\\' {
                    end = (end + 2).min(bytes.len());
                    continue;
                }
                if ch == active {
                    quote = None;
                }
            } else if ch == '\'' || ch == '"' {
                quote = Some(ch);
            } else if ch == ')' {
                break;
            }
            end += 1;
        }
        if end >= bytes.len() {
            break;
        }
        let raw = value[start..end].trim();
        let source = if raw.len() >= 2
            && ((raw.starts_with('"') && raw.ends_with('"'))
                || (raw.starts_with('\'') && raw.ends_with('\'')))
        {
            &raw[1..raw.len() - 1]
        } else {
            raw
        };
        if !source.is_empty() {
            urls.push(source.replace("\\\"", "\"").replace("\\'", "'"));
        }
        cursor = end + 1;
    }
    urls
}

pub(crate) fn absolutize_css_urls(source: &str, base: &url::Url) -> String {
    let bytes = source.as_bytes();
    let mut output = String::with_capacity(source.len());
    let mut cursor = 0;
    while cursor + 4 <= bytes.len() {
        let Some(relative) = source[cursor..].to_ascii_lowercase().find("url(") else {
            break;
        };
        let function_start = cursor + relative;
        output.push_str(&source[cursor..function_start + 4]);
        let value_start = function_start + 4;
        let mut end = value_start;
        let mut quote = None;
        while end < bytes.len() {
            let ch = bytes[end] as char;
            if let Some(active) = quote {
                if ch == '\\' {
                    end = (end + 2).min(bytes.len());
                    continue;
                }
                if ch == active {
                    quote = None;
                }
            } else if ch == '\'' || ch == '"' {
                quote = Some(ch);
            } else if ch == ')' {
                break;
            }
            end += 1;
        }
        if end >= bytes.len() {
            output.push_str(&source[value_start..]);
            return output;
        }
        let raw = source[value_start..end].trim();
        let (prefix, value, suffix) = if raw.len() >= 2
            && ((raw.starts_with('"') && raw.ends_with('"'))
                || (raw.starts_with('\'') && raw.ends_with('\'')))
        {
            (&raw[..1], &raw[1..raw.len() - 1], &raw[raw.len() - 1..])
        } else {
            ("", raw, "")
        };
        if let Ok(url) = base.join(value) {
            output.push_str(prefix);
            output.push_str(url.as_str());
            output.push_str(suffix);
        } else {
            output.push_str(raw);
        }
        output.push(')');
        cursor = end + 1;
    }
    output.push_str(&source[cursor..]);
    output
}

/// Drop decoded images that can be recreated from their source.
pub fn clear_cache() {
    CACHE.with(|cache| cache.borrow_mut().clear());
    ANIMATIONS.with(|animations| animations.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn png_2x1() -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(2, 1, image::Rgba([12, 34, 56, 255]));
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }

    #[test]
    fn browser_bytes_install_into_the_shared_renderer_cache() {
        clear_cache();
        let decoded = decode_and_install("images/pixel.png", &png_2x1()).unwrap();
        assert_eq!((decoded.width, decoded.height), (2, 1));
        assert_eq!(dimensions("images/pixel.png"), Some((2, 1)));
        set_density("images/pixel.png", 2.0);
        assert_eq!(dimensions("images/pixel.png"), Some((1, 1)));
        assert_eq!(
            get_or_load("images/pixel.png").map(|image| (image.width, image.height)),
            Some((2, 1))
        );
        invalidate("images/pixel.png");
        assert_eq!(dimensions("images/pixel.png"), None);
    }

    #[test]
    fn svg_image_document_uses_shared_raster_cache() {
        clear_cache();
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="12" height="7"><rect width="12" height="7" fill="red"/></svg>"#;
        let decoded = decode_and_install("icon.svg", svg).unwrap();
        assert_eq!((decoded.width, decoded.height), (12, 7));
        assert_eq!(dimensions("icon.svg"), Some((12, 7)));
        assert!(decoded.data.iter().any(|channel| *channel != 0));
    }

    #[test]
    fn extracts_quoted_and_layered_css_image_urls() {
        assert_eq!(
            css_image_urls(
                "linear-gradient(red, blue), url(\"images/a.png\"), url('icons/b.webp')"
            ),
            vec!["images/a.png", "icons/b.webp"]
        );
        assert!(css_image_urls("none").is_empty());
    }

    #[test]
    fn resolves_css_urls_against_the_fragment_base() {
        let base = url::Url::parse("https://example.test/css/theme/main.css").unwrap();
        assert_eq!(
            absolutize_css_urls(
                ".hero { background-image: url('../images/hero.png'); }",
                &base
            ),
            ".hero { background-image: url('https://example.test/css/images/hero.png'); }"
        );
    }
}

fn load_from_source(src: &str) -> Option<DecodedImage> {
    let bytes = if src.starts_with("http://") || src.starts_with("https://") {
        let resp = match ureq::get(src).call() {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[W3C OS] Failed to fetch image {src}: {e}");
                return None;
            }
        };
        let mut buf = Vec::new();
        if resp.into_body().as_reader().read_to_end(&mut buf).is_err() {
            eprintln!("[W3C OS] Failed to read image response body for {src}");
            return None;
        }
        buf
    } else {
        match std::fs::read(src) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("[W3C OS] Failed to read image file {src}: {e}");
                return None;
            }
        }
    };

    match image::load_from_memory(&bytes) {
        Ok(img) => {
            let rgba = img.to_rgba8();
            let (w, h) = (rgba.width(), rgba.height());
            Some(DecodedImage {
                width: w,
                height: h,
                intrinsic_width: w,
                intrinsic_height: h,
                data: Arc::new(rgba.into_raw()),
            })
        }
        Err(e) => {
            eprintln!("[W3C OS] Failed to decode image {src}: {e}");
            None
        }
    }
}
