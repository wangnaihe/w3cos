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
    let pad = kernel_radius(sigma) as f32;
    if shadow.inset {
        raster_inset(rect, radius, shadow, sigma, pad)
    } else {
        raster_outer(rect, radius, shadow, sigma, pad)
    }
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
