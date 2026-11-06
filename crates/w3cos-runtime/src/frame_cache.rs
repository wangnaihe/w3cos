//! Optional screenshot cache.
//!
//! The vsync present path must not copy the framebuffer here. Call
//! [`request_screenshot`] so the next present stores once for the AI Bridge
//! `/screenshot` endpoint.
//!
//! The cache holds raw RGBA bytes plus the framebuffer dimensions, and a
//! monotonically-increasing generation counter so consumers can detect
//! whether a new frame has been produced since the last poll.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Default, Clone)]
struct Frame {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    generation: u64,
}

fn cache() -> &'static Mutex<Frame> {
    static CACHE: OnceLock<Mutex<Frame>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Frame::default()))
}

static SCREENSHOT_REQUESTED: AtomicBool = AtomicBool::new(false);

/// One interactive component from the last layout pass.
///
/// Published only when a bridge request asks for an index or an annotated
/// screenshot. Painting does not read or draw this list.
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutMark {
    pub index: u32,
    pub node_id: u32,
    pub role: String,
    pub name: String,
    pub bounds: [f32; 4],
}

fn layout_marks_slot() -> &'static Mutex<Vec<LayoutMark>> {
    static MARKS: OnceLock<Mutex<Vec<LayoutMark>>> = OnceLock::new();
    MARKS.get_or_init(|| Mutex::new(Vec::new()))
}

fn pending_clicks() -> &'static Mutex<Vec<u32>> {
    static CLICKS: OnceLock<Mutex<Vec<u32>>> = OnceLock::new();
    CLICKS.get_or_init(|| Mutex::new(Vec::new()))
}

pub fn store_layout_marks(marks: Vec<LayoutMark>) {
    if let Ok(mut slot) = layout_marks_slot().lock() {
        *slot = marks;
    }
}

pub fn layout_marks() -> Vec<LayoutMark> {
    layout_marks_slot()
        .lock()
        .map(|slot| slot.clone())
        .unwrap_or_default()
}

pub fn enqueue_layout_click(node_id: u32) {
    if let Ok(mut slot) = pending_clicks().lock() {
        slot.push(node_id);
    }
}

pub fn take_layout_clicks() -> Vec<u32> {
    pending_clicks()
        .lock()
        .map(|mut slot| std::mem::take(&mut *slot))
        .unwrap_or_default()
}

/// Ask the next vsync present to copy the framebuffer for a screenshot.
pub fn request_screenshot() {
    SCREENSHOT_REQUESTED.store(true, Ordering::Release);
}

/// Whether a screenshot copy is pending. Does not consume the request.
pub fn screenshot_request_pending() -> bool {
    SCREENSHOT_REQUESTED.load(Ordering::Acquire)
}

/// Consume a pending screenshot request. Present copies only when this is true.
pub fn take_screenshot_request() -> bool {
    SCREENSHOT_REQUESTED.swap(false, Ordering::AcqRel)
}

/// Replace the cached frame. `rgba` must be `width*height*4` bytes
/// in non-premultiplied RGBA order.
pub fn store(width: u32, height: u32, rgba: Vec<u8>) {
    store_from_slice(width, height, &rgba);
}

/// Copy `rgba` into the cache, reusing the previous allocation when the
/// framebuffer size is unchanged. Call this only when a screenshot was
/// requested — never from the vsync present hot path.
pub fn store_from_slice(width: u32, height: u32, rgba: &[u8]) {
    let expected = (width as usize) * (height as usize) * 4;
    if rgba.len() != expected {
        // Don't store a malformed frame; leave the previous snapshot intact.
        return;
    }
    if let Ok(mut frame) = cache().lock() {
        if frame.rgba.len() == expected {
            frame.rgba.copy_from_slice(rgba);
        } else {
            frame.rgba = rgba.to_vec();
        }
        frame.width = width;
        frame.height = height;
        frame.generation = frame.generation.wrapping_add(1);
    }
}

/// Whether a frame has ever been stored.
pub fn has_frame() -> bool {
    cache().lock().map(|f| !f.rgba.is_empty()).unwrap_or(false)
}

pub fn clear() {
    if let Ok(mut frame) = cache().lock() {
        *frame = Frame::default();
    }
}

/// Snapshot the cached buffer (`width`, `height`, RGBA bytes, generation).
pub fn snapshot() -> Option<(u32, u32, Vec<u8>, u64)> {
    let frame = cache().lock().ok()?;
    if frame.rgba.is_empty() {
        return None;
    }
    Some((
        frame.width,
        frame.height,
        frame.rgba.clone(),
        frame.generation,
    ))
}

/// Encode the cached frame to PNG. `None` if no frame has been rendered yet
/// or if encoding fails.
pub fn encode_png() -> Option<Vec<u8>> {
    // Clone under the lock, encode after release so a screenshot cannot stall
    // the paint thread's store_from_slice. Screenshots are not a per-frame path.
    let (width, height, rgba) = {
        let frame = cache().lock().ok()?;
        if frame.rgba.is_empty() {
            return None;
        }
        (frame.width, frame.height, frame.rgba.clone())
    };
    encode_rgba_png(width, height, &rgba).ok()
}

/// Encode straight RGBA as a PNG. Alpha is written as 255.
///
/// A framebuffer can carry real RGB in a pixel whose alpha byte is 0 (the
/// X11 present format keeps that byte unused). Viewers then treat the whole
/// image as blank. Screenshot PNGs stay opaque and keep the RGB bytes.
pub fn encode_rgba_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    #[cfg(not(feature = "image-png"))]
    {
        let _ = (width, height, rgba);
        return Err("PNG encoding requires the image-png feature".into());
    }
    #[cfg(feature = "image-png")]
    {
        use image::ImageEncoder;
        let expected = (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(4);
        if rgba.len() != expected {
            return Err(format!(
                "png encode failed: expected {expected} bytes, got {}",
                rgba.len()
            ));
        }
        let mut opaque = Vec::with_capacity(expected);
        for pixel in rgba.chunks_exact(4) {
            opaque.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
        }
        let mut out = Vec::with_capacity(expected / 2);
        let mut cursor = std::io::Cursor::new(&mut out);
        image::codecs::png::PngEncoder::new(&mut cursor)
            .write_image(&opaque, width, height, image::ExtendedColorType::Rgba8)
            .map_err(|e| format!("png encode failed: {e}"))?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static FC_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn reset() {
        if let Ok(mut frame) = cache().lock() {
            *frame = Frame::default();
        }
    }

    #[test]
    fn store_and_snapshot() {
        let _g = FC_TEST_LOCK.lock().unwrap();
        reset();
        let pixels = vec![0xFFu8; 4 * 4 * 4]; // 4x4 RGBA white
        store(4, 4, pixels.clone());
        let (w, h, snap, generation) = snapshot().unwrap();
        assert_eq!((w, h), (4, 4));
        assert_eq!(snap, pixels);
        assert!(generation > 0);
    }

    #[test]
    fn rejects_mismatched_buffer() {
        let _g = FC_TEST_LOCK.lock().unwrap();
        reset();
        store(2, 2, vec![0u8; 7]);
        assert!(snapshot().is_none());
    }

    #[test]
    fn store_from_slice_updates_without_changing_generation_on_reject() {
        let _g = FC_TEST_LOCK.lock().unwrap();
        reset();
        store_from_slice(1, 1, &[1, 2, 3, 4]);
        let (w, h, snap, generation) = snapshot().unwrap();
        assert_eq!((w, h), (1, 1));
        assert_eq!(snap, vec![1, 2, 3, 4]);
        store_from_slice(1, 1, &[9, 8, 7, 6]);
        let (_, _, snap2, gen2) = snapshot().unwrap();
        assert_eq!(snap2, vec![9, 8, 7, 6]);
        assert!(gen2 > generation);
        store_from_slice(2, 2, &[0u8; 3]);
        let (_, _, snap3, gen3) = snapshot().unwrap();
        assert_eq!(snap3, vec![9, 8, 7, 6]);
        assert_eq!(gen3, gen2);
    }

    #[cfg(feature = "image-png")]
    #[test]
    fn encode_png_returns_signature() {
        let _g = FC_TEST_LOCK.lock().unwrap();
        reset();
        store(1, 1, vec![10, 20, 30, 255]);
        let png = encode_png().unwrap();
        // PNG magic number = 89 50 4E 47
        assert_eq!(&png[..4], &[0x89, 0x50, 0x4E, 0x47]);
    }

    #[cfg(feature = "image-png")]
    #[test]
    fn encode_png_keeps_rgb_and_writes_opaque_alpha() {
        let _g = FC_TEST_LOCK.lock().unwrap();
        reset();
        store(1, 1, vec![10, 20, 30, 0]);
        let png = encode_png().unwrap();
        let image = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(image.get_pixel(0, 0).0, [10, 20, 30, 255]);
    }

    #[test]
    fn screenshot_request_is_consumed_by_present() {
        let _g = FC_TEST_LOCK.lock().unwrap();
        let _ = take_screenshot_request();
        assert!(!screenshot_request_pending());
        request_screenshot();
        assert!(screenshot_request_pending());
        assert!(take_screenshot_request());
        assert!(!screenshot_request_pending());
        assert!(!take_screenshot_request());
    }
}
