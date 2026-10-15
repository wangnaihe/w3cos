//! Separable gaussian blur for backdrop filters, box-shadows, and text-shadows.
//!
//! The kernel is normalized and cut off at 3σ (capped). Callers skip it when
//! the radius is ~0 so the hard-edge fill stays the fast path. Box-shadow σ
//! matches the GPU path (`blur_radius / 2`). CSS `blur()` passes σ through.

use std::cell::RefCell;

use w3cos_std::color::Color;
use w3cos_std::style::BoxShadow;

use crate::layout::LayoutRect;

/// Largest standard deviation we will convolve. Larger requests clamp here so
/// a `blur(60px)` shadow cannot allocate a kernel hundreds of taps wide.
pub const MAX_SIGMA: f32 = 24.0;
const MAX_RADIUS: i32 = 72;

thread_local! {
    static SCRATCH: RefCell<Vec<f32>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone, Copy)]
pub enum BlurEdge {
    /// Samples outside the buffer are transparent. Used for shadow masks.
    Zero,
    /// Samples outside repeat the edge pixel. Used for backdrop snapshots.
    Clamp,
}

pub fn box_shadow_sigma(blur_radius: f32) -> f32 {
    (blur_radius.max(0.0) * 0.5).min(MAX_SIGMA)
}

pub fn kernel_radius(sigma: f32) -> i32 {
    if sigma < 0.15 {
        return 0;
    }
    (((sigma.min(MAX_SIGMA)) * 3.0).ceil() as i32).clamp(1, MAX_RADIUS)
}

fn kernel(sigma: f32) -> Vec<f32> {
    let radius = kernel_radius(sigma);
    let mut weights = Vec::with_capacity((radius * 2 + 1) as usize);
    if radius == 0 {
        weights.push(1.0);
        return weights;
    }
    let denom = 2.0 * sigma * sigma;
    let mut sum = 0.0_f32;
    for offset in -radius..=radius {
        let weight = (-((offset * offset) as f32) / denom).exp();
        weights.push(weight);
        sum += weight;
    }
    if sum > 0.0 {
        for weight in &mut weights {
            *weight /= sum;
        }
    }
    weights
}

/// Blur one alpha plane in place. `alpha.len()` is `width * height`.
pub fn blur_alpha(alpha: &mut [u8], width: u32, height: u32, sigma: f32, edge: BlurEdge) {
    if kernel_radius(sigma) == 0 || width == 0 || height == 0 {
        return;
    }
    // Large σ is low-frequency. Blurring a 2× or 4× smaller plane keeps the
    // same shape and cuts the separable pass by roughly scale² × scale.
    let scale = downsample_scale(sigma);
    if scale == 1 {
        blur_alpha_full(alpha, width, height, sigma, edge);
        return;
    }
    let (small_w, small_h, mut small) = downsample_plane(alpha, width, height, scale);
    blur_alpha_full(&mut small, small_w, small_h, sigma / scale as f32, edge);
    upsample_plane(&small, small_w, small_h, alpha, width, height, scale);
}

fn blur_alpha_full(alpha: &mut [u8], width: u32, height: u32, sigma: f32, edge: BlurEdge) {
    if kernel_radius(sigma) == 0 || width == 0 || height == 0 {
        return;
    }
    let n = (width as usize).saturating_mul(height as usize);
    if alpha.len() < n {
        return;
    }
    let weights = kernel(sigma);
    SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        scratch.resize(n * 2, 0.0);
        let (src, dst) = scratch.split_at_mut(n);
        for (slot, pixel) in src.iter_mut().zip(alpha.iter()) {
            *slot = f32::from(*pixel);
        }
        convolve_plane(src, dst, width, height, &weights, true, edge);
        convolve_plane(dst, src, width, height, &weights, false, edge);
        for (pixel, sample) in alpha.iter_mut().zip(src.iter()) {
            *pixel = sample.round().clamp(0.0, 255.0) as u8;
        }
    });
}

/// Blur premultiplied RGBA in place (`pixels.len()` is `width * height * 4`).
pub fn blur_premultiplied(pixels: &mut [u8], width: u32, height: u32, sigma: f32, edge: BlurEdge) {
    if kernel_radius(sigma) == 0 || width == 0 || height == 0 {
        return;
    }
    let scale = downsample_scale(sigma);
    if scale == 1 {
        blur_premultiplied_full(pixels, width, height, sigma, edge);
        return;
    }
    let (small_w, small_h, mut small) = downsample_rgba(pixels, width, height, scale);
    blur_premultiplied_full(&mut small, small_w, small_h, sigma / scale as f32, edge);
    upsample_rgba(&small, small_w, small_h, pixels, width, height, scale);
}

fn downsample_scale(sigma: f32) -> u32 {
    if sigma >= 16.0 {
        4
    } else if sigma >= 8.0 {
        2
    } else {
        1
    }
}

fn downsample_plane(src: &[u8], width: u32, height: u32, scale: u32) -> (u32, u32, Vec<u8>) {
    let small_w = width.div_ceil(scale).max(1);
    let small_h = height.div_ceil(scale).max(1);
    let mut dst = vec![0_u8; (small_w * small_h) as usize];
    for y in 0..small_h {
        for x in 0..small_w {
            let mut sum = 0_u32;
            let mut count = 0_u32;
            for dy in 0..scale {
                for dx in 0..scale {
                    let sx = x * scale + dx;
                    let sy = y * scale + dy;
                    if sx < width && sy < height {
                        sum += u32::from(src[(sy * width + sx) as usize]);
                        count += 1;
                    }
                }
            }
            dst[(y * small_w + x) as usize] = (sum / count.max(1)) as u8;
        }
    }
    (small_w, small_h, dst)
}

fn upsample_plane(
    src: &[u8],
    small_w: u32,
    small_h: u32,
    dst: &mut [u8],
    width: u32,
    height: u32,
    scale: u32,
) {
    for y in 0..height {
        let fy = (y as f32 + 0.5) / scale as f32 - 0.5;
        let y0 = fy.floor().clamp(0.0, small_h as f32 - 1.0);
        let y1 = (y0 + 1.0).min(small_h as f32 - 1.0);
        let ty = fy - y0;
        for x in 0..width {
            let fx = (x as f32 + 0.5) / scale as f32 - 0.5;
            let x0 = fx.floor().clamp(0.0, small_w as f32 - 1.0);
            let x1 = (x0 + 1.0).min(small_w as f32 - 1.0);
            let tx = fx - x0;
            let index = (y * width + x) as usize;
            let sample =
                |sx: f32, sy: f32| f32::from(src[(sy as u32 * small_w + sx as u32) as usize]);
            let value = sample(x0, y0) * (1.0 - tx) * (1.0 - ty)
                + sample(x1, y0) * tx * (1.0 - ty)
                + sample(x0, y1) * (1.0 - tx) * ty
                + sample(x1, y1) * tx * ty;
            dst[index] = value.round().clamp(0.0, 255.0) as u8;
        }
    }
}

fn downsample_rgba(src: &[u8], width: u32, height: u32, scale: u32) -> (u32, u32, Vec<u8>) {
    let small_w = width.div_ceil(scale).max(1);
    let small_h = height.div_ceil(scale).max(1);
    let mut dst = vec![0_u8; (small_w * small_h * 4) as usize];
    for y in 0..small_h {
        for x in 0..small_w {
            let mut sum = [0_u32; 4];
            let mut count = 0_u32;
            for dy in 0..scale {
                for dx in 0..scale {
                    let sx = x * scale + dx;
                    let sy = y * scale + dy;
                    if sx < width && sy < height {
                        let index = ((sy * width + sx) * 4) as usize;
                        for channel in 0..4 {
                            sum[channel] += u32::from(src[index + channel]);
                        }
                        count += 1;
                    }
                }
            }
            let index = ((y * small_w + x) * 4) as usize;
            for channel in 0..4 {
                dst[index + channel] = (sum[channel] / count.max(1)) as u8;
            }
        }
    }
    (small_w, small_h, dst)
}

fn upsample_rgba(
    src: &[u8],
    small_w: u32,
    small_h: u32,
    dst: &mut [u8],
    width: u32,
    height: u32,
    scale: u32,
) {
    for y in 0..height {
        let fy = (y as f32 + 0.5) / scale as f32 - 0.5;
        let y0 = fy.floor().clamp(0.0, small_h as f32 - 1.0);
        let y1 = (y0 + 1.0).min(small_h as f32 - 1.0);
        let ty = fy - y0;
        for x in 0..width {
            let fx = (x as f32 + 0.5) / scale as f32 - 0.5;
            let x0 = fx.floor().clamp(0.0, small_w as f32 - 1.0);
            let x1 = (x0 + 1.0).min(small_w as f32 - 1.0);
            let tx = fx - x0;
            let index = ((y * width + x) * 4) as usize;
            for channel in 0..4 {
                let sample = |sx: f32, sy: f32| -> f32 {
                    f32::from(
                        src[((sy as u32 * small_w + sx as u32) * 4 + channel as u32) as usize],
                    )
                };
                let value = sample(x0, y0) * (1.0 - tx) * (1.0 - ty)
                    + sample(x1, y0) * tx * (1.0 - ty)
                    + sample(x0, y1) * (1.0 - tx) * ty
                    + sample(x1, y1) * tx * ty;
                dst[index + channel] = value.round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

fn blur_premultiplied_full(pixels: &mut [u8], width: u32, height: u32, sigma: f32, edge: BlurEdge) {
    if kernel_radius(sigma) == 0 || width == 0 || height == 0 {
        return;
    }
    let n = (width as usize).saturating_mul(height as usize);
    if pixels.len() < n * 4 {
        return;
    }
    let weights = kernel(sigma);
    SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        scratch.resize(n * 8, 0.0);
        let (src, dst) = scratch.split_at_mut(n * 4);
        for (slot, pixel) in src.iter_mut().zip(pixels.iter()) {
            *slot = f32::from(*pixel);
        }
        convolve_rgba(src, dst, width, height, &weights, true, edge);
        convolve_rgba(dst, src, width, height, &weights, false, edge);
        for (pixel, sample) in pixels.iter_mut().zip(src.iter()) {
            *pixel = sample.round().clamp(0.0, 255.0) as u8;
        }
    });
}

fn convolve_plane(
    src: &[f32],
    dst: &mut [f32],
    width: u32,
    height: u32,
    weights: &[f32],
    horizontal: bool,
    edge: BlurEdge,
) {
    let radius = (weights.len() / 2) as i32;
    let width = width as i32;
    let height = height as i32;
    for y in 0..height {
        for x in 0..width {
            let mut acc = 0.0_f32;
            for (tap, weight) in weights.iter().enumerate() {
                let offset = tap as i32 - radius;
                let (sx, sy) = if horizontal {
                    (x + offset, y)
                } else {
                    (x, y + offset)
                };
                acc += sample_plane(src, width, height, sx, sy, edge) * weight;
            }
            dst[(y * width + x) as usize] = acc;
        }
    }
}

fn convolve_rgba(
    src: &[f32],
    dst: &mut [f32],
    width: u32,
    height: u32,
    weights: &[f32],
    horizontal: bool,
    edge: BlurEdge,
) {
    let radius = (weights.len() / 2) as i32;
    let width = width as i32;
    let height = height as i32;
    for y in 0..height {
        for x in 0..width {
            let mut acc = [0.0_f32; 4];
            for (tap, weight) in weights.iter().enumerate() {
                let offset = tap as i32 - radius;
                let (sx, sy) = if horizontal {
                    (x + offset, y)
                } else {
                    (x, y + offset)
                };
                let sample = sample_rgba(src, width, height, sx, sy, edge);
                for channel in 0..4 {
                    acc[channel] += sample[channel] * weight;
                }
            }
            let index = ((y * width + x) * 4) as usize;
            dst[index..index + 4].copy_from_slice(&acc);
        }
    }
}

fn sample_plane(src: &[f32], width: i32, height: i32, x: i32, y: i32, edge: BlurEdge) -> f32 {
    let (x, y) = match edge {
        BlurEdge::Zero => {
            if x < 0 || y < 0 || x >= width || y >= height {
                return 0.0;
            }
            (x, y)
        }
        BlurEdge::Clamp => (x.clamp(0, width - 1), y.clamp(0, height - 1)),
    };
    src[(y * width + x) as usize]
}

fn sample_rgba(src: &[f32], width: i32, height: i32, x: i32, y: i32, edge: BlurEdge) -> [f32; 4] {
    let (x, y) = match edge {
        BlurEdge::Zero => {
            if x < 0 || y < 0 || x >= width || y >= height {
                return [0.0; 4];
            }
            (x, y)
        }
        BlurEdge::Clamp => (x.clamp(0, width - 1), y.clamp(0, height - 1)),
    };
    let index = ((y * width + x) * 4) as usize;
    [src[index], src[index + 1], src[index + 2], src[index + 3]]
}

/// Premultiplied RGBA sprite in CSS pixels.
pub struct PremulSprite {
    pub origin_x: f32,
    pub origin_y: f32,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Gaussian shadow sprite. `None` when blur is ~0 so the caller keeps the
/// hard-edge fill (no kernel, no extra buffer).
pub fn raster_box_shadow(
    rect: LayoutRect,
    radius: f32,
    shadow: &BoxShadow,
) -> Option<PremulSprite> {
    let sigma = box_shadow_sigma(shadow.blur_radius);
    if kernel_radius(sigma) == 0 || rect.width < 0.5 || rect.height < 0.5 {
        return None;
    }
    let key = shadow_key(rect, radius, shadow);
    let cached = CACHES.with(|slot| {
        let mut caches = slot.borrow_mut();
        let found = caches.shadows.iter().position(|entry| entry.key == key);
        let Some(index) = found else {
            return None;
        };
        let stamp = caches.bump();
        caches.shadows[index].stamp = stamp;
        caches.hits += 1;
        Some(place_shadow(&caches.shadows[index], rect))
    });
    if cached.is_some() {
        return cached;
    }
    let pad = kernel_radius(sigma) as f32;
    let sprite = if shadow.inset {
        raster_inset(rect, radius, shadow, sigma, pad)
    } else {
        raster_outer(rect, radius, shadow, sigma, pad)
    }?;
    remember_shadow(key, &sprite, rect);
    Some(sprite)
}

fn raster_outer(
    rect: LayoutRect,
    radius: f32,
    shadow: &BoxShadow,
    sigma: f32,
    pad: f32,
) -> Option<PremulSprite> {
    let spread = shadow.spread_radius;
    let shape = LayoutRect {
        x: rect.x + shadow.offset_x - spread,
        y: rect.y + shadow.offset_y - spread,
        width: (rect.width + spread * 2.0).max(0.0),
        height: (rect.height + spread * 2.0).max(0.0),
    };
    let origin_x = (shape.x - pad).floor();
    let origin_y = (shape.y - pad).floor();
    let width = (shape.x + shape.width + pad - origin_x).ceil().max(1.0) as u32;
    let height = (shape.y + shape.height + pad - origin_y).ceil().max(1.0) as u32;
    if width > 4096 || height > 4096 {
        return None;
    }
    let mut alpha = vec![0_u8; (width as usize) * (height as usize)];
    fill_round_rect(
        &mut alpha,
        width,
        height,
        origin_x,
        origin_y,
        shape,
        radius + spread,
        255,
    );
    blur_alpha(&mut alpha, width, height, sigma, BlurEdge::Zero);
    Some(PremulSprite {
        origin_x,
        origin_y,
        width,
        height,
        pixels: colorize(&alpha, shadow.color),
    })
}

fn raster_inset(
    rect: LayoutRect,
    radius: f32,
    shadow: &BoxShadow,
    sigma: f32,
    pad: f32,
) -> Option<PremulSprite> {
    let origin_x = (rect.x - pad).floor();
    let origin_y = (rect.y - pad).floor();
    let width = (rect.x + rect.width + pad - origin_x).ceil().max(1.0) as u32;
    let height = (rect.y + rect.height + pad - origin_y).ceil().max(1.0) as u32;
    if width > 4096 || height > 4096 {
        return None;
    }
    // The pre-blur mask is everything except the inner hole, including the
    // padding outside the element. Blur then spreads that ink inward; the
    // element coverage below throws away whatever falls outside the box.
    let mut alpha = vec![255_u8; (width as usize) * (height as usize)];
    let shrink = shadow.spread_radius;
    let hole = LayoutRect {
        x: rect.x + shadow.offset_x + shrink,
        y: rect.y + shadow.offset_y + shrink,
        width: (rect.width - shrink * 2.0).max(0.0),
        height: (rect.height - shrink * 2.0).max(0.0),
    };
    if hole.width > 0.5 && hole.height > 0.5 {
        fill_round_rect(
            &mut alpha,
            width,
            height,
            origin_x,
            origin_y,
            hole,
            (radius - shrink).max(0.0),
            0,
        );
    }
    blur_alpha(&mut alpha, width, height, sigma, BlurEdge::Zero);
    // The blur must fall inward. Drop coverage outside the element's box.
    for y in 0..height {
        for x in 0..width {
            let px = origin_x + x as f32 + 0.5;
            let py = origin_y + y as f32 + 0.5;
            let cover = round_rect_coverage(px, py, rect, radius);
            let index = (y * width + x) as usize;
            alpha[index] = (f32::from(alpha[index]) * cover).round() as u8;
        }
    }
    Some(PremulSprite {
        origin_x,
        origin_y,
        width,
        height,
        pixels: colorize(&alpha, shadow.color),
    })
}

fn colorize(alpha: &[u8], color: Color) -> Vec<u8> {
    let mut pixels = Vec::with_capacity(alpha.len() * 4);
    for coverage in alpha {
        let a = (u16::from(*coverage) * u16::from(color.a) / 255) as u8;
        pixels.push((u16::from(color.r) * u16::from(a) / 255) as u8);
        pixels.push((u16::from(color.g) * u16::from(a) / 255) as u8);
        pixels.push((u16::from(color.b) * u16::from(a) / 255) as u8);
        pixels.push(a);
    }
    pixels
}

fn fill_round_rect(
    alpha: &mut [u8],
    width: u32,
    height: u32,
    origin_x: f32,
    origin_y: f32,
    shape: LayoutRect,
    radius: f32,
    value: u8,
) {
    let x0 = ((shape.x - origin_x).floor() as i32 - 1).clamp(0, width as i32) as u32;
    let y0 = ((shape.y - origin_y).floor() as i32 - 1).clamp(0, height as i32) as u32;
    let x1 = ((shape.x + shape.width - origin_x).ceil() as i32 + 1).clamp(0, width as i32) as u32;
    let y1 = ((shape.y + shape.height - origin_y).ceil() as i32 + 1).clamp(0, height as i32) as u32;
    for y in y0..y1 {
        for x in x0..x1 {
            let px = origin_x + x as f32 + 0.5;
            let py = origin_y + y as f32 + 0.5;
            let cover = round_rect_coverage(px, py, shape, radius);
            if cover <= 0.0 {
                continue;
            }
            let index = (y * width + x) as usize;
            let sample = (cover * f32::from(value)).round() as u8;
            // A later hole (`value == 0`) punches; a fill takes the max so
            // overlapping coverage does not darken the interior.
            if value == 0 {
                alpha[index] = (f32::from(alpha[index]) * (1.0 - cover)).round() as u8;
            } else {
                alpha[index] = alpha[index].max(sample);
            }
        }
    }
}

/// Signed-distance coverage of a rounded rectangle. 1 inside, 0 outside.
pub fn round_rect_coverage(x: f32, y: f32, rect: LayoutRect, radius: f32) -> f32 {
    if rect.width <= 0.0 || rect.height <= 0.0 {
        return 0.0;
    }
    let radius = radius.min(rect.width * 0.5).min(rect.height * 0.5).max(0.0);
    let px = x - rect.x;
    let py = y - rect.y;
    let half_w = rect.width * 0.5;
    let half_h = rect.height * 0.5;
    let qx = (px - half_w).abs() - (half_w - radius);
    let qy = (py - half_h).abs() - (half_h - radius);
    let outside = qx.max(0.0).hypot(qy.max(0.0));
    let inside = qx.max(qy).min(0.0);
    let distance = outside + inside - radius;
    (0.5 - distance).clamp(0.0, 1.0)
}

/// Local matrix that maps a canvas point into a unit circle of radii `(rx, ry)`.
/// Coefficients are Skia order: scaleX, skewX, transX, skewY, scaleY, transY.
pub fn radial_local_coeffs(cx: f32, cy: f32, rx: f32, ry: f32) -> [f32; 6] {
    let rx = rx.max(1.0);
    let ry = ry.max(1.0);
    [1.0 / rx, 0.0, -cx / rx, 0.0, 1.0 / ry, -cy / ry]
}

pub fn apply_affine6(m: [f32; 6], x: f32, y: f32) -> (f32, f32) {
    (m[0] * x + m[1] * y + m[2], m[3] * x + m[4] * y + m[5])
}

// --- frame caches -------------------------------------------------------
//
// A gaussian over a dock-sized strip is too slow to redo every animation
// frame. Two caches keep a 1200×800 CPU frame interactive:
//
// * Backdrop tiles remember the blurred strip. Unchanged tiles are copied.
//   A damage rect reblurs only the dirty tiles plus the kernel margin.
// * Shadow sprites are independent of the pixels underneath, so they are
//   keyed by shape and subpixel phase, not by absolute position.
// * Chrome snapshots remember the framebuffer after backdrop, shadows, and
//   the element's own background. The next frame with the same underlay is
//   a copy instead of another composite.

const TILE: u32 = 64;
const MAX_BACKDROP_SLOTS: usize = 4;
const MAX_SHADOW_SLOTS: usize = 8;
const MAX_CHROME_SLOTS: usize = 4;

fn fnv64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    let mut chunks = bytes.chunks_exact(8);
    for chunk in chunks.by_ref() {
        let word = u64::from_le_bytes([
            chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
        ]);
        hash ^= word;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    for byte in chunks.remainder() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn mix64(hash: u64, value: u64) -> u64 {
    hash ^ value.wrapping_mul(0x9E3779B97F4A7C15)
}

fn quantize(value: f32) -> i32 {
    (value * 4.0).round() as i32
}

struct BackdropSlot {
    width: u32,
    height: u32,
    sigma_bits: u32,
    edge: u8,
    tile_hashes: Vec<u64>,
    pixels: Vec<u8>,
    stamp: u64,
}

struct ShadowSlot {
    key: u64,
    origin_dx: f32,
    origin_dy: f32,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    stamp: u64,
}

struct ChromeSlot {
    tag: u64,
    underlay: u64,
    origin_x: i32,
    origin_y: i32,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    stamp: u64,
}

struct FrameCaches {
    backdrops: Vec<BackdropSlot>,
    shadows: Vec<ShadowSlot>,
    chrome: Vec<ChromeSlot>,
    stamp: u64,
    hits: u64,
    misses: u64,
}

impl FrameCaches {
    fn new() -> Self {
        Self {
            backdrops: Vec::new(),
            shadows: Vec::new(),
            chrome: Vec::new(),
            stamp: 1,
            hits: 0,
            misses: 0,
        }
    }

    fn bump(&mut self) -> u64 {
        self.stamp = self.stamp.wrapping_add(1);
        self.stamp
    }
}

thread_local! {
    static CACHES: RefCell<FrameCaches> = RefCell::new(FrameCaches::new());
}

fn tile_hashes(pixels: &[u8], width: u32, height: u32) -> Vec<u64> {
    let tiles_x = width.div_ceil(TILE).max(1);
    let tiles_y = height.div_ceil(TILE).max(1);
    let mut hashes = Vec::with_capacity((tiles_x * tiles_y) as usize);
    for ty in 0..tiles_y {
        for tx in 0..tiles_x {
            let x0 = tx * TILE;
            let y0 = ty * TILE;
            let x1 = (x0 + TILE).min(width);
            let y1 = (y0 + TILE).min(height);
            let mut hash = 0xcbf29ce484222325u64;
            for y in y0..y1 {
                let start = ((y * width + x0) * 4) as usize;
                let end = ((y * width + x1) * 4) as usize;
                hash = mix64(hash, fnv64(&pixels[start..end]));
            }
            hashes.push(hash);
        }
    }
    hashes
}

fn dirty_tile_bounds(
    before: &[u64],
    after: &[u64],
    width: u32,
    height: u32,
) -> Option<(u32, u32, u32, u32)> {
    if before.len() != after.len() {
        return None;
    }
    let tiles_x = width.div_ceil(TILE).max(1);
    let mut any = false;
    let mut x0 = tiles_x;
    let mut y0 = height.div_ceil(TILE).max(1);
    let mut x1 = 0u32;
    let mut y1 = 0u32;
    for (index, (left, right)) in before.iter().zip(after.iter()).enumerate() {
        if left == right {
            continue;
        }
        any = true;
        let tx = index as u32 % tiles_x;
        let ty = index as u32 / tiles_x;
        x0 = x0.min(tx);
        y0 = y0.min(ty);
        x1 = x1.max(tx + 1);
        y1 = y1.max(ty + 1);
    }
    if !any {
        return Some((0, 0, 0, 0));
    }
    let px0 = x0 * TILE;
    let py0 = y0 * TILE;
    let px1 = (x1 * TILE).min(width);
    let py1 = (y1 * TILE).min(height);
    Some((px0, py0, px1, py1))
}

fn blur_influence_pad(sigma: f32) -> i32 {
    let scale = downsample_scale(sigma).max(1) as i32;
    let small = kernel_radius(sigma / scale as f32) + 2;
    (small * scale).max(kernel_radius(sigma))
}

fn reblur_dirty(
    source: &[u8],
    cached: &[u8],
    width: u32,
    height: u32,
    sigma: f32,
    edge: BlurEdge,
    dirty: (u32, u32, u32, u32),
) -> Option<Vec<u8>> {
    let (x0, y0, x1, y1) = dirty;
    if x1 <= x0 || y1 <= y0 {
        return Some(cached.to_vec());
    }
    let pad = blur_influence_pad(sigma).max(0) as u32;
    let sx0 = x0.saturating_sub(pad);
    let sy0 = y0.saturating_sub(pad);
    let sx1 = (x1 + pad).min(width);
    let sy1 = (y1 + pad).min(height);
    let sub_w = sx1 - sx0;
    let sub_h = sy1 - sy0;
    if sub_w == 0 || sub_h == 0 {
        return None;
    }
    // A dirty region that covers most of the strip is cheaper to blur whole.
    if u64::from(sub_w) * u64::from(sub_h) * 2 > u64::from(width) * u64::from(height) {
        return None;
    }
    let mut sub = vec![0u8; (sub_w as usize) * (sub_h as usize) * 4];
    for row in 0..sub_h {
        let src = (((sy0 + row) * width + sx0) * 4) as usize;
        let dst = (row * sub_w * 4) as usize;
        let len = (sub_w * 4) as usize;
        sub[dst..dst + len].copy_from_slice(&source[src..src + len]);
    }
    blur_premultiplied(&mut sub, sub_w, sub_h, sigma, edge);
    let mut out = cached.to_vec();
    // Only the inner dirty rect is valid. The pad exists so the kernel does
    // not see a hard edge that the full-frame blur never had.
    let local_x0 = x0 - sx0;
    let local_y0 = y0 - sy0;
    for row in y0..y1 {
        let local_row = local_y0 + (row - y0);
        let src = ((local_row * sub_w + local_x0) * 4) as usize;
        let dst = ((row * width + x0) * 4) as usize;
        let len = ((x1 - x0) * 4) as usize;
        out[dst..dst + len].copy_from_slice(&sub[src..src + len]);
    }
    Some(out)
}

/// Blur premultiplied RGBA, reusing an identical or partly-identical strip.
pub fn blur_premultiplied_cached(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    sigma: f32,
    edge: BlurEdge,
) {
    if kernel_radius(sigma) == 0 || width == 0 || height == 0 {
        return;
    }
    let n = (width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(4);
    if pixels.len() < n {
        return;
    }
    let pixels = &mut pixels[..n];
    let sigma_bits = (sigma.clamp(0.0, MAX_SIGMA) * 100.0).round() as u32;
    let edge_id = match edge {
        BlurEdge::Zero => 0u8,
        BlurEdge::Clamp => 1u8,
    };
    let hashes = tile_hashes(pixels, width, height);
    let reused = CACHES.with(|slot| {
        let mut caches = slot.borrow_mut();
        let found = caches.backdrops.iter().position(|entry| {
            entry.width == width
                && entry.height == height
                && entry.sigma_bits == sigma_bits
                && entry.edge == edge_id
        });
        let Some(index) = found else {
            caches.misses += 1;
            return None;
        };
        let dirty = dirty_tile_bounds(&caches.backdrops[index].tile_hashes, &hashes, width, height);
        let Some(dirty) = dirty else {
            caches.misses += 1;
            return None;
        };
        if dirty.2 <= dirty.0 || dirty.3 <= dirty.1 {
            pixels.copy_from_slice(&caches.backdrops[index].pixels);
            let stamp = caches.bump();
            caches.backdrops[index].stamp = stamp;
            caches.hits += 1;
            return Some(());
        }
        let cached = caches.backdrops[index].pixels.clone();
        let updated = reblur_dirty(pixels, &cached, width, height, sigma, edge, dirty)?;
        pixels.copy_from_slice(&updated);
        let stamp = caches.bump();
        let entry = &mut caches.backdrops[index];
        entry.tile_hashes = hashes.clone();
        entry.pixels = updated;
        entry.stamp = stamp;
        caches.hits += 1;
        Some(())
    });
    if reused.is_some() {
        return;
    }
    let mut blurred = pixels.to_vec();
    blur_premultiplied(&mut blurred, width, height, sigma, edge);
    pixels.copy_from_slice(&blurred);
    CACHES.with(|slot| {
        let mut caches = slot.borrow_mut();
        let stamp = caches.bump();
        if let Some(entry) = caches.backdrops.iter_mut().find(|entry| {
            entry.width == width
                && entry.height == height
                && entry.sigma_bits == sigma_bits
                && entry.edge == edge_id
        }) {
            entry.tile_hashes = hashes;
            entry.pixels = blurred;
            entry.stamp = stamp;
            return;
        }
        if caches.backdrops.len() >= MAX_BACKDROP_SLOTS {
            let oldest = caches
                .backdrops
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| entry.stamp)
                .map(|(index, _)| index);
            if let Some(index) = oldest {
                caches.backdrops.remove(index);
            }
        }
        caches.backdrops.push(BackdropSlot {
            width,
            height,
            sigma_bits,
            edge: edge_id,
            tile_hashes: hashes,
            pixels: blurred,
            stamp,
        });
    });
}

fn shadow_key(rect: LayoutRect, radius: f32, shadow: &BoxShadow) -> u64 {
    let mut hash = 0x6a09e667f3bcc909u64;
    hash = mix64(hash, quantize(rect.width) as u64);
    hash = mix64(hash, quantize(rect.height) as u64);
    hash = mix64(hash, quantize(rect.x.fract()) as u64);
    hash = mix64(hash, quantize(rect.y.fract()) as u64);
    hash = mix64(hash, quantize(radius) as u64);
    hash = mix64(hash, quantize(shadow.offset_x) as u64);
    hash = mix64(hash, quantize(shadow.offset_y) as u64);
    hash = mix64(hash, quantize(shadow.blur_radius) as u64);
    hash = mix64(hash, quantize(shadow.spread_radius) as u64);
    hash = mix64(
        hash,
        u64::from(shadow.color.r)
            | (u64::from(shadow.color.g) << 8)
            | (u64::from(shadow.color.b) << 16)
            | (u64::from(shadow.color.a) << 24),
    );
    hash = mix64(hash, u64::from(u8::from(shadow.inset)));
    hash
}

fn place_shadow(slot: &ShadowSlot, rect: LayoutRect) -> PremulSprite {
    PremulSprite {
        origin_x: rect.x + slot.origin_dx,
        origin_y: rect.y + slot.origin_dy,
        width: slot.width,
        height: slot.height,
        pixels: slot.pixels.clone(),
    }
}

fn remember_shadow(key: u64, sprite: &PremulSprite, rect: LayoutRect) {
    CACHES.with(|slot| {
        let mut caches = slot.borrow_mut();
        let stamp = caches.bump();
        if caches.shadows.len() >= MAX_SHADOW_SLOTS {
            let oldest = caches
                .shadows
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| entry.stamp)
                .map(|(index, _)| index);
            if let Some(index) = oldest {
                caches.shadows.remove(index);
            }
        }
        caches.shadows.push(ShadowSlot {
            key,
            origin_dx: sprite.origin_x - rect.x,
            origin_dy: sprite.origin_y - rect.y,
            width: sprite.width,
            height: sprite.height,
            pixels: sprite.pixels.clone(),
            stamp,
        });
    });
}

/// Style bits that change the painted chrome. Position is included so a
/// moved dock does not reuse another dock's pixels.
pub fn chrome_tag(
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    radius: f32,
    sigma: f32,
    background: u32,
    border: u32,
    shadows: u64,
) -> u64 {
    let mut hash = 0xbb67ae8584caa73bu64;
    hash = mix64(hash, quantize(x) as u64);
    hash = mix64(hash, quantize(y) as u64);
    hash = mix64(hash, quantize(width) as u64);
    hash = mix64(hash, quantize(height) as u64);
    hash = mix64(hash, quantize(radius) as u64);
    hash = mix64(hash, quantize(sigma) as u64);
    hash = mix64(hash, u64::from(background));
    hash = mix64(hash, u64::from(border));
    mix64(hash, shadows)
}

fn rect_sample_bounds(
    bounds: LayoutRect,
    stride_w: u32,
    height: u32,
) -> Option<(i32, i32, u32, u32)> {
    let x0 = bounds.x.floor() as i32;
    let y0 = bounds.y.floor() as i32;
    let x1 = (bounds.x + bounds.width).ceil() as i32;
    let y1 = (bounds.y + bounds.height).ceil() as i32;
    let x0 = x0.clamp(0, stride_w as i32);
    let y0 = y0.clamp(0, height as i32);
    let x1 = x1.clamp(0, stride_w as i32);
    let y1 = y1.clamp(0, height as i32);
    let w = (x1 - x0) as u32;
    let h = (y1 - y0) as u32;
    if w == 0 || h == 0 {
        None
    } else {
        Some((x0, y0, w, h))
    }
}

pub fn hash_pixmap_rect(data: &[u8], stride_w: u32, height: u32, bounds: LayoutRect) -> u64 {
    let Some((x0, y0, w, h)) = rect_sample_bounds(bounds, stride_w, height) else {
        return 0;
    };
    let mut hash = 0xcbf29ce484222325u64;
    let row_bytes = stride_w as usize * 4;
    for row in 0..h {
        let start = ((y0 as u32 + row) as usize) * row_bytes + x0 as usize * 4;
        let end = start + (w as usize) * 4;
        if end <= data.len() {
            hash = mix64(hash, fnv64(&data[start..end]));
        }
    }
    hash
}

fn copy_rect(data: &[u8], stride_w: u32, x0: i32, y0: i32, w: u32, h: u32) -> Vec<u8> {
    let mut out = vec![0u8; (w as usize) * (h as usize) * 4];
    let row_bytes = stride_w as usize * 4;
    for row in 0..h {
        let start = ((y0 as u32 + row) as usize) * row_bytes + x0 as usize * 4;
        let dst = (row * w * 4) as usize;
        let len = (w * 4) as usize;
        if start + len <= data.len() {
            out[dst..dst + len].copy_from_slice(&data[start..start + len]);
        }
    }
    out
}

fn paste_rect(data: &mut [u8], stride_w: u32, x0: i32, y0: i32, w: u32, h: u32, src: &[u8]) {
    let row_bytes = stride_w as usize * 4;
    for row in 0..h {
        let start = ((y0 as u32 + row) as usize) * row_bytes + x0 as usize * 4;
        let src_off = (row * w * 4) as usize;
        let len = (w * 4) as usize;
        if start + len <= data.len() && src_off + len <= src.len() {
            data[start..start + len].copy_from_slice(&src[src_off..src_off + len]);
        }
    }
}

/// Copy a previously painted dock/intent chrome when the pixels underneath
/// still match. Returns false when the caller must paint and then `store_chrome`.
pub fn restore_chrome(
    data: &mut [u8],
    stride_w: u32,
    _height: u32,
    bounds: LayoutRect,
    tag: u64,
    underlay: u64,
) -> bool {
    let _ = bounds;
    CACHES.with(|slot| {
        let mut caches = slot.borrow_mut();
        let found = caches
            .chrome
            .iter()
            .position(|entry| entry.tag == tag && entry.underlay == underlay);
        let Some(index) = found else {
            caches.misses += 1;
            return false;
        };
        let stamp = caches.bump();
        let origin_x = caches.chrome[index].origin_x;
        let origin_y = caches.chrome[index].origin_y;
        let width = caches.chrome[index].width;
        let height_px = caches.chrome[index].height;
        let pixels = caches.chrome[index].pixels.clone();
        caches.chrome[index].stamp = stamp;
        caches.hits += 1;
        drop(caches);
        paste_rect(
            data, stride_w, origin_x, origin_y, width, height_px, &pixels,
        );
        true
    })
}

pub fn store_chrome(
    data: &[u8],
    stride_w: u32,
    height: u32,
    bounds: LayoutRect,
    tag: u64,
    underlay: u64,
) {
    let Some((x0, y0, w, h)) = rect_sample_bounds(bounds, stride_w, height) else {
        return;
    };
    let pixels = copy_rect(data, stride_w, x0, y0, w, h);
    CACHES.with(|slot| {
        let mut caches = slot.borrow_mut();
        let stamp = caches.bump();
        if let Some(entry) = caches
            .chrome
            .iter_mut()
            .find(|entry| entry.tag == tag && entry.underlay == underlay)
        {
            entry.origin_x = x0;
            entry.origin_y = y0;
            entry.width = w;
            entry.height = h;
            entry.pixels = pixels;
            entry.stamp = stamp;
            return;
        }
        if caches.chrome.len() >= MAX_CHROME_SLOTS {
            let oldest = caches
                .chrome
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| entry.stamp)
                .map(|(index, _)| index);
            if let Some(index) = oldest {
                caches.chrome.remove(index);
            }
        }
        caches.chrome.push(ChromeSlot {
            tag,
            underlay,
            origin_x: x0,
            origin_y: y0,
            width: w,
            height: h,
            pixels,
            stamp,
        });
    });
}

pub fn cache_hit_count() -> u64 {
    CACHES.with(|slot| slot.borrow().hits)
}

pub fn clear_frame_caches() {
    CACHES.with(|slot| *slot.borrow_mut() = FrameCaches::new());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gaussian_kernel_is_normalized_and_peaks_at_center() {
        let weights = kernel(2.0);
        let sum: f32 = weights.iter().sum();
        assert!((sum - 1.0).abs() < 1e-4);
        let mid = weights.len() / 2;
        assert!(weights[mid] > weights[0]);
        assert!((weights[mid - 1] - weights[mid + 1]).abs() < 1e-5);
    }

    #[test]
    fn separable_blur_spreads_an_impulse_and_keeps_it_brightest() {
        let width = 17_u32;
        let height = 17_u32;
        let mut alpha = vec![0_u8; (width * height) as usize];
        alpha[(8 * width + 8) as usize] = 255;
        blur_alpha(&mut alpha, width, height, 1.6, BlurEdge::Zero);
        let center = alpha[(8 * width + 8) as usize];
        let neighbor = alpha[(8 * width + 10) as usize];
        let far = alpha[(8 * width + 16) as usize];
        assert!(center > neighbor);
        assert!(neighbor > 0);
        assert_eq!(far, 0);
        assert!(center < 255);
    }

    #[test]
    fn blurred_outer_shadow_fades_outside_the_box() {
        let rect = LayoutRect {
            x: 20.0,
            y: 20.0,
            width: 24.0,
            height: 24.0,
        };
        let shadow = BoxShadow::new(0.0, 0.0, 8.0, 0.0, Color::rgb(0, 0, 255));
        let sprite = raster_box_shadow(rect, 0.0, &shadow).expect("blurred shadow");
        let alpha_at = |x: f32, y: f32| {
            let sx = (x - sprite.origin_x).floor() as i32;
            let sy = (y - sprite.origin_y).floor() as i32;
            if sx < 0 || sy < 0 || sx >= sprite.width as i32 || sy >= sprite.height as i32 {
                return 0;
            }
            sprite.pixels[((sy as u32 * sprite.width + sx as u32) * 4 + 3) as usize]
        };
        let edge = alpha_at(rect.x + rect.width + 1.0, rect.y + rect.height * 0.5);
        let further = alpha_at(rect.x + rect.width + 4.0, rect.y + rect.height * 0.5);
        let outside = alpha_at(rect.x + rect.width + 30.0, rect.y + rect.height * 0.5);
        assert!(edge > further);
        assert!(further > 0);
        assert_eq!(outside, 0);
        assert!(edge < 250);
    }

    #[test]
    fn blurred_inset_shadow_stays_inside_the_box() {
        let rect = LayoutRect {
            x: 16.0,
            y: 16.0,
            width: 40.0,
            height: 40.0,
        };
        let shadow = BoxShadow::inset(0.0, 0.0, 6.0, 0.0, Color::rgb(255, 0, 0));
        let sprite = raster_box_shadow(rect, 4.0, &shadow).expect("inset");
        let alpha_at = |x: f32, y: f32| {
            let sx = (x - sprite.origin_x).floor() as i32;
            let sy = (y - sprite.origin_y).floor() as i32;
            sprite.pixels[((sy as u32 * sprite.width + sx as u32) * 4 + 3) as usize]
        };
        assert_eq!(alpha_at(rect.x - 2.0, rect.y + 8.0), 0);
        assert!(alpha_at(rect.x + 2.0, rect.y + 8.0) > 0);
        assert!(alpha_at(rect.x + 20.0, rect.y + 20.0) < alpha_at(rect.x + 2.0, rect.y + 8.0));
    }

    #[test]
    fn dock_sized_backdrop_and_window_shadow_stay_interactive() {
        let mut backdrop = vec![80_u8; 1200 * 120 * 4];
        let backdrop_started = std::time::Instant::now();
        blur_premultiplied(&mut backdrop, 1200, 120, 18.0, BlurEdge::Clamp);
        let backdrop_elapsed = backdrop_started.elapsed();
        let rect = LayoutRect {
            x: 40.0,
            y: 40.0,
            width: 640.0,
            height: 400.0,
        };
        let shadow = BoxShadow::new(0.0, 24.0, 60.0, 0.0, Color::rgba(0, 0, 0, 115));
        let shadow_started = std::time::Instant::now();
        let sprite = raster_box_shadow(rect, 14.0, &shadow).expect("window shadow");
        let shadow_elapsed = shadow_started.elapsed();
        assert!(
            backdrop_elapsed.as_millis() < 750,
            "backdrop {backdrop_elapsed:?}"
        );
        assert!(
            shadow_elapsed.as_millis() < 750,
            "shadow {shadow_elapsed:?}"
        );
        assert!(sprite.pixels.iter().any(|pixel| *pixel > 0));
    }

    #[test]
    fn unchanged_backdrop_is_a_copy_and_a_dirty_tile_matches_a_full_blur() {
        clear_frame_caches();
        let width = 320u32;
        let height = 96u32;
        let mut source = vec![0u8; (width * height * 4) as usize];
        for (index, pixel) in source.chunks_exact_mut(4).enumerate() {
            pixel[0] = (index % 251) as u8;
            pixel[1] = 40;
            pixel[2] = 180;
            pixel[3] = 255;
        }
        let mut first = source.clone();
        blur_premultiplied_cached(&mut first, width, height, 12.0, BlurEdge::Clamp);
        let hits = cache_hit_count();
        let mut second = source.clone();
        blur_premultiplied_cached(&mut second, width, height, 12.0, BlurEdge::Clamp);
        assert!(cache_hit_count() > hits);
        assert_eq!(first, second);

        source[16] = source[16].wrapping_add(90);
        let mut partial = source.clone();
        blur_premultiplied_cached(&mut partial, width, height, 12.0, BlurEdge::Clamp);
        let mut reference = source.clone();
        blur_premultiplied(&mut reference, width, height, 12.0, BlurEdge::Clamp);
        assert_eq!(partial, reference);
    }

    #[test]
    fn a_translated_shadow_reuses_the_sprite() {
        clear_frame_caches();
        let rect = LayoutRect {
            x: 20.25,
            y: 30.25,
            width: 180.0,
            height: 48.0,
        };
        let shadow = BoxShadow::new(0.0, 8.0, 16.0, 0.0, Color::rgba(0, 0, 0, 90));
        let first = raster_box_shadow(rect, 12.0, &shadow).expect("shadow");
        let moved = LayoutRect {
            x: rect.x + 40.0,
            ..rect
        };
        let second = raster_box_shadow(moved, 12.0, &shadow).expect("cached");
        assert_eq!(first.pixels, second.pixels);
        assert!((second.origin_x - first.origin_x - 40.0).abs() < 0.02);
    }

    #[test]
    fn ellipse_local_matrix_maps_the_rim_to_the_unit_circle() {
        let coeffs = radial_local_coeffs(10.0, 20.0, 40.0, 15.0);
        let (x, y) = apply_affine6(coeffs, 50.0, 20.0);
        assert!((x - 1.0).abs() < 1e-4);
        assert!(y.abs() < 1e-4);
        let (x, y) = apply_affine6(coeffs, 10.0, 35.0);
        assert!(x.abs() < 1e-4);
        assert!((y - 1.0).abs() < 1e-4);
    }
}
