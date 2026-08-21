//! Latest-frame snapshot cache.
//!
//! Both renderers (CPU `tiny-skia` and GPU `vello`) call [`store`] after
//! presenting a frame so consumers — most notably the AI Bridge `/screenshot`
//! endpoint — can grab a PNG snapshot without coordinating with the render
//! loop or running a separate offscreen pass.
//!
//! The cache holds raw RGBA bytes plus the framebuffer dimensions, and a
//! monotonically-increasing generation counter so consumers can detect
//! whether a new frame has been produced since the last poll.

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

/// Replace the cached frame. `rgba` must be `width*height*4` bytes
/// in non-premultiplied RGBA order.
pub fn store(width: u32, height: u32, rgba: Vec<u8>) {
    store_from_slice(width, height, &rgba);
}

/// Copy `rgba` into the cache, reusing the previous allocation when the
/// framebuffer size is unchanged. The paint loop should call this instead of
/// `pixmap.data().to_vec()` so a static-size window does not allocate a
/// second full-screen buffer every frame.
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
    encode_rgba_to_png(width, height, &rgba).ok()
}

fn encode_rgba_to_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    use image::ImageEncoder;
    let mut out = Vec::with_capacity(rgba.len() / 4);
    let mut cursor = std::io::Cursor::new(&mut out);
    image::codecs::png::PngEncoder::new(&mut cursor)
        .write_image(rgba, width, height, image::ExtendedColorType::Rgba8)
        .map_err(|e| format!("png encode failed: {e}"))?;
    Ok(out)
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

    #[test]
    fn encode_png_returns_signature() {
        let _g = FC_TEST_LOCK.lock().unwrap();
        reset();
        store(1, 1, vec![10, 20, 30, 255]);
        let png = encode_png().unwrap();
        // PNG magic number = 89 50 4E 47
        assert_eq!(&png[..4], &[0x89, 0x50, 0x4E, 0x47]);
    }
}
