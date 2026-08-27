use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
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
    /// SVG preserves CSS intrinsic sizing separately from the raster surface.
    /// Missing and percentage root dimensions must not be replaced by usvg's
    /// fallback pixel size when resolving `background-size: auto`.
    pub(crate) svg_intrinsic_size: Option<SvgIntrinsicSize>,
    pub data: Arc<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SvgIntrinsicSize {
    pub width: SvgIntrinsicLength,
    pub height: SvgIntrinsicLength,
    pub ratio: Option<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum SvgIntrinsicLength {
    Auto,
    Px(f32),
    Percent(f32),
}

impl DecodedImage {
    /// Identity of the decoded pixel buffer. GPU/Skia texture caches key off
    /// this so a reused `Arc` keeps a stable Vello blob id / Skia image.
    pub(crate) fn pixels_id(&self) -> usize {
        Arc::as_ptr(&self.data) as usize
    }
}

thread_local! {
    static CACHE: RefCell<HashMap<String, Option<DecodedImage>>> = RefCell::new(HashMap::new());
    static FINGERPRINTS: RefCell<HashMap<String, u64>> = RefCell::new(HashMap::new());
    static ANIMATIONS: RefCell<HashMap<String, AnimatedImage>> = RefCell::new(HashMap::new());
    static DECODE_COUNT: Cell<u64> = const { Cell::new(0) };
    static CACHE_HITS: Cell<u64> = const { Cell::new(0) };
}

fn record_decode() {
    DECODE_COUNT.with(|count| count.set(count.get().saturating_add(1)));
}

fn record_hit() {
    CACHE_HITS.with(|count| count.set(count.get().saturating_add(1)));
}

pub(crate) fn decode_count() -> u64 {
    DECODE_COUNT.with(Cell::get)
}

pub(crate) fn cache_hit_count() -> u64 {
    CACHE_HITS.with(Cell::get)
}

pub(crate) fn reset_cache_stats() {
    DECODE_COUNT.with(|count| count.set(0));
    CACHE_HITS.with(|count| count.set(0));
    #[cfg(feature = "gpu")]
    crate::render_gpu::reset_image_texture_stats();
    #[cfg(feature = "skia")]
    crate::render_skia::reset_image_texture_stats();
}

fn source_fingerprint(bytes: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

fn cached_decoded_if_fingerprint(src: &str, fingerprint: u64) -> Option<DecodedImage> {
    let matches = FINGERPRINTS.with(|fps| fps.borrow().get(src).copied() == Some(fingerprint));
    if !matches {
        return None;
    }
    CACHE.with(|cache| cache.borrow().get(src).and_then(Option::clone))
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
        record_hit();
        return Some(frame);
    }
    if let Some(entry) = CACHE.with(|cache| cache.borrow().get(src).cloned()) {
        if entry.is_some() {
            record_hit();
        }
        return entry;
    }
    let result = load_from_source(src);
    CACHE.with(|cache| {
        cache.borrow_mut().insert(src.to_string(), result.clone());
    });
    result
}

/// Decode bytes fetched by the Browser subresource loader and publish them to
/// the same cache consumed by every renderer. `src` is the DOM-facing source
/// string, so relative image URLs do not trigger a second renderer-owned
/// network request after the Browser loader has resolved them.
pub(crate) fn decode_and_install(src: &str, bytes: &[u8]) -> Result<DecodedImage, String> {
    let fingerprint = source_fingerprint(bytes);
    if let Some(decoded) = cached_decoded_if_fingerprint(src, fingerprint) {
        record_hit();
        return Ok(decoded);
    }
    record_decode();
    if looks_like_svg(bytes) {
        let decoded = decode_svg(bytes)?;
        install_decoded_bytes(src, decoded.clone(), fingerprint);
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
            svg_intrinsic_size: None,
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
        install_decoded_bytes(src, decoded.clone(), fingerprint);
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
        svg_intrinsic_size: None,
        data: Arc::new(rgba.into_raw()),
    };
    install_decoded_bytes(src, decoded.clone(), fingerprint);
    Ok(decoded)
}

fn install_decoded(src: &str, decoded: DecodedImage) {
    CACHE.with(|cache| {
        cache.borrow_mut().insert(src.to_string(), Some(decoded));
    });
}

fn install_decoded_bytes(src: &str, decoded: DecodedImage, fingerprint: u64) {
    install_decoded(src, decoded);
    FINGERPRINTS.with(|fps| {
        fps.borrow_mut().insert(src.to_string(), fingerprint);
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
    let intrinsic_size = parse_svg_intrinsic_size(bytes);
    let normalized_bytes = complete_svg_intrinsic_axis(bytes, intrinsic_size);
    let options = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(&normalized_bytes, &options)
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
        svg_intrinsic_size: Some(intrinsic_size),
        data: Arc::new(rgba),
    })
}

fn complete_svg_intrinsic_axis(bytes: &[u8], size: SvgIntrinsicSize) -> std::borrow::Cow<'_, [u8]> {
    let missing_attribute = match (size.width, size.height, size.ratio) {
        (SvgIntrinsicLength::Px(width), SvgIntrinsicLength::Auto, Some(ratio)) if ratio > 0.0 => {
            Some(format!(" height=\"{}\"", width / ratio))
        }
        (SvgIntrinsicLength::Auto, SvgIntrinsicLength::Px(height), Some(ratio)) if ratio > 0.0 => {
            Some(format!(" width=\"{}\"", height * ratio))
        }
        _ => None,
    };
    let Some(attribute) = missing_attribute else {
        return std::borrow::Cow::Borrowed(bytes);
    };
    let Ok(source) = std::str::from_utf8(bytes) else {
        return std::borrow::Cow::Borrowed(bytes);
    };
    let Some(svg_start) = source.find("<svg") else {
        return std::borrow::Cow::Borrowed(bytes);
    };
    let insertion = svg_start + "<svg".len();
    let mut normalized = String::with_capacity(source.len() + attribute.len());
    normalized.push_str(&source[..insertion]);
    normalized.push_str(&attribute);
    normalized.push_str(&source[insertion..]);
    std::borrow::Cow::Owned(normalized.into_bytes())
}

fn parse_svg_intrinsic_size(bytes: &[u8]) -> SvgIntrinsicSize {
    let mut reader = quick_xml::Reader::from_reader(bytes);
    loop {
        match reader.read_event() {
            Ok(quick_xml::events::Event::Start(element))
            | Ok(quick_xml::events::Event::Empty(element))
                if element.local_name().as_ref() == b"svg" =>
            {
                let mut width = SvgIntrinsicLength::Auto;
                let mut height = SvgIntrinsicLength::Auto;
                let mut ratio = None;
                for attribute in element.attributes().flatten() {
                    let Ok(name) = std::str::from_utf8(attribute.key.as_ref()) else {
                        continue;
                    };
                    let Ok(value) = attribute.decode_and_unescape_value(reader.decoder()) else {
                        continue;
                    };
                    match name {
                        "width" => width = parse_svg_intrinsic_length(&value),
                        "height" => height = parse_svg_intrinsic_length(&value),
                        "viewBox" => {
                            let values = value
                                .split(|character: char| {
                                    character.is_ascii_whitespace() || character == ','
                                })
                                .filter(|value| !value.is_empty())
                                .filter_map(|value| value.parse::<f32>().ok())
                                .collect::<Vec<_>>();
                            if values.len() == 4 && values[2] > 0.0 && values[3] > 0.0 {
                                ratio = Some(values[2] / values[3]);
                            }
                        }
                        _ => {}
                    }
                }
                return SvgIntrinsicSize {
                    width,
                    height,
                    ratio,
                };
            }
            Ok(quick_xml::events::Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    SvgIntrinsicSize {
        width: SvgIntrinsicLength::Auto,
        height: SvgIntrinsicLength::Auto,
        ratio: None,
    }
}

fn parse_svg_intrinsic_length(value: &str) -> SvgIntrinsicLength {
    let value = value.trim();
    if let Some(percent) = value.strip_suffix('%') {
        return percent
            .trim()
            .parse::<f32>()
            .map(|value| SvgIntrinsicLength::Percent(value / 100.0))
            .unwrap_or(SvgIntrinsicLength::Auto);
    }
    w3cos_std::style::parse_absolute_length_px(value)
        .map(SvgIntrinsicLength::Px)
        .unwrap_or(SvgIntrinsicLength::Auto)
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
            svg_intrinsic_size: None,
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

/// Whether the Browser loader owns this source but has no decoded intrinsic
/// dimensions yet. This covers both an in-flight request and a terminally
/// broken image, neither of which should fall back to the legacy 200x200
/// renderer placeholder.
pub(crate) fn is_reserved_browser_source(src: &str) -> bool {
    CACHE.with(|cache| matches!(cache.borrow().get(src), Some(None)))
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
    let pixels_ids = cached_pixels_ids(src);
    CACHE.with(|cache| {
        cache.borrow_mut().remove(src);
    });
    FINGERPRINTS.with(|fps| {
        fps.borrow_mut().remove(src);
    });
    ANIMATIONS.with(|animations| {
        animations.borrow_mut().remove(src);
    });
    for pixels_id in pixels_ids {
        drop_renderer_texture(pixels_id);
    }
}

fn cached_pixels_ids(src: &str) -> Vec<usize> {
    let mut ids = Vec::new();
    CACHE.with(|cache| {
        if let Some(Some(image)) = cache.borrow().get(src) {
            ids.push(image.pixels_id());
        }
    });
    ANIMATIONS.with(|animations| {
        if let Some(animation) = animations.borrow().get(src) {
            for frame in &animation.frames {
                ids.push(Arc::as_ptr(&frame.data) as usize);
            }
        }
    });
    ids
}

fn drop_renderer_texture(pixels_id: usize) {
    #[cfg(feature = "gpu")]
    crate::render_gpu::invalidate_image_texture(pixels_id);
    #[cfg(feature = "skia")]
    crate::render_skia::invalidate_image_texture(pixels_id);
}

fn drop_all_renderer_textures() {
    #[cfg(feature = "gpu")]
    crate::render_gpu::clear_image_texture_cache();
    #[cfg(feature = "skia")]
    crate::render_skia::clear_image_texture_cache();
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
    FINGERPRINTS.with(|fps| fps.borrow_mut().clear());
    ANIMATIONS.with(|animations| animations.borrow_mut().clear());
    drop_all_renderer_textures();
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use w3cos_core::Value;

    fn png_2x1() -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(2, 1, image::Rgba([12, 34, 56, 255]));
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }

    #[test]
    fn blob_object_url_images_load_without_dynamic_js() {
        let bytes = w3cos_core::binary::typed_array_value(
            png_2x1()
                .into_iter()
                .map(|byte| Value::Number(f64::from(byte)))
                .collect(),
        );
        let blob = w3cos_core::class::construct(
            &w3cos_core::web::blob_class(),
            vec![Value::array(vec![bytes])],
        );
        let url = w3cos_core::web::url_class()
            .call_method("createObjectURL", vec![blob])
            .to_js_string();

        let decoded = get_or_load(&url).expect("blob image should decode");
        assert_eq!((decoded.width, decoded.height), (2, 1));
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
    fn svg_intrinsic_metadata_preserves_auto_percent_and_view_box_ratio() {
        let size = parse_svg_intrinsic_size(
            br#"<svg xmlns="http://www.w3.org/2000/svg" width="40%" viewBox="0 0 4 6"/>"#,
        );
        assert_eq!(size.width, SvgIntrinsicLength::Percent(0.4));
        assert_eq!(size.height, SvgIntrinsicLength::Auto);
        assert_eq!(size.ratio, Some(4.0 / 6.0));

        let size = parse_svg_intrinsic_size(br#"<svg xmlns="http://www.w3.org/2000/svg"/>"#);
        assert_eq!(size.width, SvgIntrinsicLength::Auto);
        assert_eq!(size.height, SvgIntrinsicLength::Auto);
        assert_eq!(size.ratio, None);
    }

    #[test]
    fn svg_view_box_with_one_intrinsic_axis_fills_its_raster_surface() {
        let decoded = decode_svg(
            br#"<svg xmlns="http://www.w3.org/2000/svg" width="40" viewBox="0 0 4 6"><rect width="100%" height="100%" fill="green"/></svg>"#,
        )
        .unwrap();
        assert_eq!((decoded.width, decoded.height), (40, 60));
        assert!(decoded.data.chunks_exact(4).all(|pixel| pixel[3] == 255));

        let decoded = decode_svg(
            br#"<svg xmlns="http://www.w3.org/2000/svg" height="60" viewBox="0 0 4 6"><rect width="100%" height="100%" fill="green"/></svg>"#,
        )
        .unwrap();
        assert_eq!((decoded.width, decoded.height), (40, 60));
        assert!(decoded.data.chunks_exact(4).all(|pixel| pixel[3] == 255));
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

    #[test]
    fn decode_once_is_reused_across_paints_of_the_same_src() {
        clear_cache();
        reset_cache_stats();
        let bytes = png_2x1();
        decode_and_install("cache/pixel.png", &bytes).unwrap();
        assert_eq!(decode_count(), 1);
        assert_eq!(cache_hit_count(), 0);

        let first = get_or_load("cache/pixel.png").expect("decoded");
        let second = get_or_load("cache/pixel.png").expect("decoded");
        assert_eq!(decode_count(), 1);
        assert_eq!(cache_hit_count(), 2);
        assert_eq!(first.pixels_id(), second.pixels_id());
        assert!(std::sync::Arc::ptr_eq(&first.data, &second.data));

        decode_and_install("cache/pixel.png", &bytes).unwrap();
        assert_eq!(decode_count(), 1);
        assert!(cache_hit_count() >= 3);

        let other = {
            let image = image::RgbaImage::from_pixel(1, 1, image::Rgba([1, 2, 3, 255]));
            let mut bytes = Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(image)
                .write_to(&mut bytes, image::ImageFormat::Png)
                .unwrap();
            bytes.into_inner()
        };
        decode_and_install("cache/pixel.png", &other).unwrap();
        assert_eq!(decode_count(), 2);
        clear_cache();
    }
}

fn load_from_source(src: &str) -> Option<DecodedImage> {
    let bytes = if src.starts_with("blob:w3cos/") {
        match w3cos_core::web::object_url_resource(src) {
            Some((bytes, _)) => bytes,
            None => {
                eprintln!("[W3C OS] Failed to resolve image object URL {src}");
                return None;
            }
        }
    } else if src.starts_with("http://") || src.starts_with("https://") {
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

    match decode_and_install(src, &bytes) {
        Ok(decoded) => Some(decoded),
        Err(e) => {
            eprintln!("[W3C OS] Failed to decode image {src}: {e}");
            None
        }
    }
}
