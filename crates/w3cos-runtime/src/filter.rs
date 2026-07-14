//! CSS `filter` parsing and application — standard W3C filter functions.
//!
//! ## Algorithm choices (industry-standard, not ad-hoc)
//!
//! | Effect | Optimal approach | Complexity |
//! |--------|------------------|------------|
//! | brightness/contrast/opacity/invert | per-pixel linear | O(n) |
//! | grayscale/sepia/saturate/hue-rotate | 4×4 color matrix (SVG/CSS spec) | O(n) |
//! | blur | separable **stack blur** or triple box-blur ≈ Gaussian | O(n) per pass |
//! | drop-shadow | alpha extract → blur → tint → composite | O(n) |
//! | full layer | offscreen raster → filter chain → blit | O(n × passes) |
//!
//! Naive 2D Gaussian convolution O(n·r²) is **not** used.

use w3cos_std::color::Color;
use w3cos_std::style::BoxShadow;

/// One step in a CSS `filter` chain (order matches author syntax).
#[derive(Debug, Clone)]
pub enum FilterOp {
    Blur(f32),
    Brightness(f32),
    Contrast(f32),
    Grayscale(f32),
    Sepia(f32),
    Invert(f32),
    Saturate(f32),
    HueRotate(f32),
    Opacity(f32),
    DropShadow(BoxShadow),
}

/// Parsed filter chain — `filter: a() b()` applies `a` then `b`.
#[derive(Debug, Clone, Default)]
pub struct FilterChain {
    pub ops: Vec<FilterOp>,
}

/// Back-compat alias used by render paths.
pub type CssFilter = FilterChain;

impl FilterChain {
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    pub fn has_blur(&self) -> bool {
        self.ops
            .iter()
            .any(|op| matches!(op, FilterOp::Blur(r) if *r > 0.0))
    }

    pub fn max_blur_px(&self) -> f32 {
        self.ops
            .iter()
            .filter_map(|op| match op {
                FilterOp::Blur(r) => Some(*r),
                _ => None,
            })
            .fold(0.0_f32, f32::max)
    }

    pub fn drop_shadow(&self) -> Option<&BoxShadow> {
        self.ops.iter().find_map(|op| match op {
            FilterOp::DropShadow(s) => Some(s),
            _ => None,
        })
    }
}

/// Whether a raw CSS filter value should promote a compositor layer.
pub fn filter_promotes_layer(raw: &str) -> bool {
    let v = raw.trim().to_lowercase();
    !v.is_empty() && v != "none"
}

pub fn parse_css_filter(value: &str) -> Option<FilterChain> {
    let v = value.trim();
    if v.is_empty() || v.eq_ignore_ascii_case("none") {
        return None;
    }

    let mut chain = FilterChain::default();
    for part in split_filter_parts(v) {
        if let Some(inner) = extract_fn(&part, "blur") {
            chain
                .ops
                .push(FilterOp::Blur(parse_length_px(inner).unwrap_or(0.0)));
        } else if let Some(inner) = extract_fn(&part, "brightness") {
            chain.ops.push(FilterOp::Brightness(
                parse_filter_amount(inner).unwrap_or(1.0),
            ));
        } else if let Some(inner) = extract_fn(&part, "contrast") {
            chain.ops.push(FilterOp::Contrast(
                parse_filter_amount(inner).unwrap_or(1.0),
            ));
        } else if let Some(inner) = extract_fn(&part, "grayscale") {
            chain.ops.push(FilterOp::Grayscale(
                parse_filter_amount(inner).unwrap_or(0.0).clamp(0.0, 1.0),
            ));
        } else if let Some(inner) = extract_fn(&part, "sepia") {
            chain.ops.push(FilterOp::Sepia(
                parse_filter_amount(inner).unwrap_or(0.0).clamp(0.0, 1.0),
            ));
        } else if let Some(inner) = extract_fn(&part, "invert") {
            chain.ops.push(FilterOp::Invert(
                parse_filter_amount(inner).unwrap_or(0.0).clamp(0.0, 1.0),
            ));
        } else if let Some(inner) = extract_fn(&part, "saturate") {
            chain.ops.push(FilterOp::Saturate(
                parse_filter_amount(inner).unwrap_or(1.0),
            ));
        } else if let Some(inner) = extract_fn(&part, "hue-rotate") {
            chain
                .ops
                .push(FilterOp::HueRotate(parse_angle_deg(inner).unwrap_or(0.0)));
        } else if let Some(inner) = extract_fn(&part, "opacity") {
            chain.ops.push(FilterOp::Opacity(
                parse_filter_amount(inner).unwrap_or(1.0).clamp(0.0, 1.0),
            ));
        } else if let Some(inner) = extract_fn(&part, "drop-shadow") {
            if let Some(shadow) = parse_drop_shadow(inner) {
                chain.ops.push(FilterOp::DropShadow(shadow));
            }
        }
    }

    if chain.is_empty() { None } else { Some(chain) }
}

/// Apply only color-matrix ops (no blur) — used when content is not rasterized.
pub fn apply_filter_to_color(color: Color, chain: &FilterChain) -> Color {
    if chain.is_empty() {
        return color;
    }
    let mut px = Pixel::from_color(color);
    for op in &chain.ops {
        match op {
            FilterOp::Blur(_) | FilterOp::DropShadow(_) => {}
            other => apply_color_op(&mut px, other),
        }
    }
    px.to_color()
}

/// Apply the full ordered chain to an RGBA buffer (layer offscreen path).
pub fn apply_chain_to_rgba(data: &mut [u8], width: u32, height: u32, chain: &FilterChain) {
    if chain.is_empty() || width == 0 || height == 0 {
        return;
    }
    for op in &chain.ops {
        match op {
            FilterOp::Blur(radius) if *radius > 0.0 => {
                stack_blur_rgba(data, width, height, (*radius / 2.0).max(1.0) as u32);
            }
            FilterOp::DropShadow(_) => {}
            color_op => {
                for chunk in data.chunks_exact_mut(4) {
                    let mut px = Pixel::from_rgba(chunk[0], chunk[1], chunk[2], chunk[3]);
                    apply_color_op(&mut px, color_op);
                    let c = px.to_color();
                    chunk[0] = c.r;
                    chunk[1] = c.g;
                    chunk[2] = c.b;
                    chunk[3] = c.a;
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
struct Pixel {
    r: f32,
    g: f32,
    b: f32,
    a: f32,
}

impl Pixel {
    fn from_color(c: Color) -> Self {
        Self {
            r: c.r as f32,
            g: c.g as f32,
            b: c.b as f32,
            a: c.a as f32,
        }
    }

    fn from_rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self {
            r: r as f32,
            g: g as f32,
            b: b as f32,
            a: a as f32,
        }
    }

    fn to_color(self) -> Color {
        Color::rgba(
            self.r.clamp(0.0, 255.0) as u8,
            self.g.clamp(0.0, 255.0) as u8,
            self.b.clamp(0.0, 255.0) as u8,
            self.a.clamp(0.0, 255.0) as u8,
        )
    }
}

fn apply_color_op(px: &mut Pixel, op: &FilterOp) {
    match op {
        FilterOp::Brightness(v) => {
            px.r *= v;
            px.g *= v;
            px.b *= v;
        }
        FilterOp::Contrast(v) => {
            px.r = (px.r - 128.0) * v + 128.0;
            px.g = (px.g - 128.0) * v + 128.0;
            px.b = (px.b - 128.0) * v + 128.0;
        }
        FilterOp::Grayscale(t) => {
            let gray = 0.299 * px.r + 0.587 * px.g + 0.114 * px.b;
            px.r = px.r + (gray - px.r) * t;
            px.g = px.g + (gray - px.g) * t;
            px.b = px.b + (gray - px.b) * t;
        }
        FilterOp::Sepia(t) => {
            let r = px.r;
            let g = px.g;
            let b = px.b;
            let sr = (r * 0.393 + g * 0.769 + b * 0.189).clamp(0.0, 255.0);
            let sg = (r * 0.349 + g * 0.686 + b * 0.168).clamp(0.0, 255.0);
            let sb = (r * 0.272 + g * 0.534 + b * 0.131).clamp(0.0, 255.0);
            px.r = r + (sr - r) * t;
            px.g = g + (sg - g) * t;
            px.b = b + (sb - b) * t;
        }
        FilterOp::Invert(t) => {
            px.r = px.r + (255.0 - 2.0 * px.r) * t;
            px.g = px.g + (255.0 - 2.0 * px.g) * t;
            px.b = px.b + (255.0 - 2.0 * px.b) * t;
        }
        FilterOp::Saturate(s) => {
            let m = saturate_matrix(*s);
            apply_matrix(px, &m);
        }
        FilterOp::HueRotate(deg) => {
            let m = hue_rotate_matrix(*deg);
            apply_matrix(px, &m);
        }
        FilterOp::Opacity(o) => {
            px.a *= o;
        }
        FilterOp::Blur(_) | FilterOp::DropShadow(_) => {}
    }
}

fn apply_matrix(px: &mut Pixel, m: &[[f32; 3]; 3]) {
    let r = px.r;
    let g = px.g;
    let b = px.b;
    px.r = (r * m[0][0] + g * m[0][1] + b * m[0][2]).clamp(0.0, 255.0);
    px.g = (r * m[1][0] + g * m[1][1] + b * m[1][2]).clamp(0.0, 255.0);
    px.b = (r * m[2][0] + g * m[2][1] + b * m[2][2]).clamp(0.0, 255.0);
}

/// SVG/CSS `saturate()` matrix — public for GPU color pass uniforms.
pub fn saturate_matrix_public(s: f32) -> [[f32; 3]; 3] {
    saturate_matrix(s)
}

/// SVG/CSS `hue-rotate()` matrix — public for GPU color pass uniforms.
pub fn hue_rotate_matrix_public(deg: f32) -> [[f32; 3]; 3] {
    hue_rotate_matrix(deg)
}

/// SVG/CSS `saturate()` matrix.
fn saturate_matrix(s: f32) -> [[f32; 3]; 3] {
    [
        [0.213 + 0.787 * s, 0.715 - 0.715 * s, 0.072 - 0.072 * s],
        [0.213 - 0.213 * s, 0.715 + 0.285 * s, 0.072 - 0.072 * s],
        [0.213 - 0.213 * s, 0.715 - 0.715 * s, 0.072 + 0.928 * s],
    ]
}

/// SVG/CSS `hue-rotate()` matrix (angle in degrees).
fn hue_rotate_matrix(deg: f32) -> [[f32; 3]; 3] {
    let rad = deg.to_radians();
    let c = rad.cos();
    let s = rad.sin();
    [
        [
            0.213 + c * 0.787 - s * 0.213,
            0.715 - c * 0.715 - s * 0.715,
            0.072 - c * 0.072 + s * 0.928,
        ],
        [
            0.213 - c * 0.213 + s * 0.143,
            0.715 + c * 0.285 + s * 0.140,
            0.072 - c * 0.072 - s * 0.283,
        ],
        [
            0.213 - c * 0.213 - s * 0.787,
            0.715 - c * 0.715 + s * 0.715,
            0.072 + c * 0.928 + s * 0.072,
        ],
    ]
}

/// Mario Klingemann **stack blur** — O(n) per axis, radius-independent cost factor.
pub fn stack_blur_rgba(data: &mut [u8], width: u32, height: u32, radius: u32) {
    if radius == 0 || width == 0 || height == 0 {
        return;
    }
    let w = width as usize;
    let h = height as usize;
    let r = radius as usize;
    let div = 2 * r + 1;

    // Horizontal
    for y in 0..h {
        let row = y * w * 4;
        let mut rs = 0u32;
        let mut gs = 0u32;
        let mut bs = 0u32;
        let mut as_ = 0u32;
        for i in 0..div {
            let x = i.min(w - 1);
            let idx = row + x * 4;
            rs += data[idx] as u32;
            gs += data[idx + 1] as u32;
            bs += data[idx + 2] as u32;
            as_ += data[idx + 3] as u32;
        }
        for x in 0..w {
            let idx = row + x * 4;
            data[idx] = (rs / div as u32) as u8;
            data[idx + 1] = (gs / div as u32) as u8;
            data[idx + 2] = (bs / div as u32) as u8;
            data[idx + 3] = (as_ / div as u32) as u8;
            let add_x = (x + r + 1).min(w - 1);
            let sub_x = x.saturating_sub(r);
            let add = row + add_x * 4;
            let sub = row + sub_x * 4;
            rs += data[add] as u32;
            gs += data[add + 1] as u32;
            bs += data[add + 2] as u32;
            as_ += data[add + 3] as u32;
            rs -= data[sub] as u32;
            gs -= data[sub + 1] as u32;
            bs -= data[sub + 2] as u32;
            as_ -= data[sub + 3] as u32;
        }
    }

    // Vertical
    for x in 0..w {
        let mut rs = 0u32;
        let mut gs = 0u32;
        let mut bs = 0u32;
        let mut as_ = 0u32;
        for i in 0..div {
            let y = i.min(h - 1);
            let idx = (y * w + x) * 4;
            rs += data[idx] as u32;
            gs += data[idx + 1] as u32;
            bs += data[idx + 2] as u32;
            as_ += data[idx + 3] as u32;
        }
        for y in 0..h {
            let idx = (y * w + x) * 4;
            data[idx] = (rs / div as u32) as u8;
            data[idx + 1] = (gs / div as u32) as u8;
            data[idx + 2] = (bs / div as u32) as u8;
            data[idx + 3] = (as_ / div as u32) as u8;
            let add_y = (y + r + 1).min(h - 1);
            let sub_y = y.saturating_sub(r);
            let add = (add_y * w + x) * 4;
            let sub = (sub_y * w + x) * 4;
            rs += data[add] as u32;
            gs += data[add + 1] as u32;
            bs += data[add + 2] as u32;
            as_ += data[add + 3] as u32;
            rs -= data[sub] as u32;
            gs -= data[sub + 1] as u32;
            bs -= data[sub + 2] as u32;
            as_ -= data[sub + 3] as u32;
        }
    }
}

fn split_filter_parts(value: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth: u32 = 0;
    let mut start = 0;
    for (i, ch) in value.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ' ' if depth == 0 => {
                let slice = value[start..i].trim();
                if !slice.is_empty() {
                    parts.push(slice.to_string());
                }
                start = i + 1;
            }
            _ => {}
        }
    }
    let tail = value[start..].trim();
    if !tail.is_empty() {
        parts.push(tail.to_string());
    }
    parts
}

fn extract_fn<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    let start = s.find(name)?;
    let rest = &s[start + name.len()..];
    let open = rest.find('(')?;
    let close = rest.rfind(')')?;
    Some(&rest[open + 1..close])
}

fn parse_length_px(value: &str) -> Option<f32> {
    let v = value.trim().trim_end_matches("px");
    v.parse().ok()
}

fn parse_angle_deg(value: &str) -> Option<f32> {
    let v = value.trim();
    if let Some(deg) = v.strip_suffix("deg") {
        return deg.parse().ok();
    }
    if let Some(rad) = v.strip_suffix("rad") {
        return rad.parse::<f32>().ok().map(f32::to_degrees);
    }
    v.parse().ok()
}

fn parse_filter_amount(value: &str) -> Option<f32> {
    let v = value.trim();
    if let Some(pct) = v.strip_suffix('%') {
        return pct.parse::<f32>().ok().map(|n| n / 100.0);
    }
    v.parse().ok()
}

fn parse_drop_shadow(value: &str) -> Option<BoxShadow> {
    let parts: Vec<&str> = value.trim().splitn(5, ' ').collect();
    if parts.len() < 3 {
        return None;
    }
    let ox = parse_length_px(parts[0])?;
    let oy = parse_length_px(parts[1])?;
    let blur = parse_length_px(parts[2])?;
    let spread = parts.get(3).and_then(|s| parse_length_px(s)).unwrap_or(0.0);
    let color = if let Some(c) = parts.get(4) {
        Color::from_hex(c)
    } else if parts.len() >= 4 && !parts[3].starts_with('#') && !parts[3].starts_with("rgb") {
        Color::rgba(0, 0, 0, 128)
    } else if let Some(c) = parts.get(3) {
        Color::from_hex(c)
    } else {
        Color::rgba(0, 0, 0, 128)
    };
    Some(BoxShadow::new(ox, oy, blur, spread, color))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_blur_and_brightness() {
        let f = parse_css_filter("blur(4px) brightness(1.2)").unwrap();
        assert!(matches!(f.ops[0], FilterOp::Blur(4.0)));
        assert!(matches!(f.ops[1], FilterOp::Brightness(v) if (v - 1.2).abs() < 0.001));
    }

    #[test]
    fn parse_hue_rotate_and_saturate() {
        let f = parse_css_filter("saturate(2) hue-rotate(90deg)").unwrap();
        assert!(matches!(f.ops[0], FilterOp::Saturate(2.0)));
        assert!(matches!(f.ops[1], FilterOp::HueRotate(90.0)));
    }

    #[test]
    fn parse_none_returns_none() {
        assert!(parse_css_filter("none").is_none());
    }

    #[test]
    fn grayscale_darkens_to_gray() {
        let f = parse_css_filter("grayscale(100%)").unwrap();
        let c = apply_filter_to_color(Color::rgb(255, 0, 0), &f);
        assert_eq!(c.r, c.g);
        assert_eq!(c.g, c.b);
    }

    #[test]
    fn filter_promotes_layer_excludes_none() {
        assert!(!filter_promotes_layer("none"));
        assert!(filter_promotes_layer("blur(2px)"));
    }

    #[test]
    fn stack_blur_softens_hard_edge() {
        let mut data = vec![0u8; 9 * 9 * 4];
        for y in 0..9 {
            for x in 0..9 {
                let idx = (y * 9 + x) * 4;
                if x == 4 && y == 4 {
                    data[idx] = 255;
                    data[idx + 1] = 255;
                    data[idx + 2] = 255;
                    data[idx + 3] = 255;
                }
            }
        }
        stack_blur_rgba(&mut data, 9, 9, 2);
        let side = data[(4 * 9 + 3) * 4 + 3];
        assert!(side > 0 && side < 255);
    }
}
