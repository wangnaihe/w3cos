//! Backend-independent geometry and shading for CSS three-dimensional borders.

use crate::layout::LayoutRect;
use w3cos_std::color::Color;
use w3cos_std::style::{BorderLineStyle, Style};

pub(crate) struct BorderLayer {
    pub polygons: Vec<[(f32, f32); 4]>,
    pub color: Color,
    pub shadow: bool,
}

struct BorderBand {
    points: [(f32, f32); 4],
    color: Color,
    shadow: bool,
    side: usize,
    outer: Option<bool>,
}

pub(crate) fn is_three_dimensional(style: &Style, side: usize) -> bool {
    matches!(
        style.border_styles[side],
        Some(
            BorderLineStyle::Groove
                | BorderLineStyle::Ridge
                | BorderLineStyle::Inset
                | BorderLineStyle::Outset
        )
    )
}

fn shade(color: Color, dark: bool) -> Color {
    // Chromium 141 Color::Dark/Light operates on the maximum sRGB channel.
    if dark && color == Color::rgb(255, 255, 255) {
        return Color::rgb(171, 171, 171);
    }
    let maximum = color.r.max(color.g).max(color.b) as f32 / 255.0;
    let scale = if dark {
        if maximum == 0.0 {
            0.0
        } else {
            (maximum - 0.33).max(0.0) / maximum
        }
    } else {
        if maximum == 0.0 {
            return Color::rgba(84, 84, 84, color.a);
        }
        (maximum + 0.33).min(1.0) / maximum
    };
    // Blink QuantizeTo8Bit truncates after multiplying by nextafter(256, 0).
    let channel = |value: u8| {
        (value as f32 / 255.0 * scale * f32::from_bits(0x437fffff))
            .floor()
            .clamp(0.0, 255.0) as u8
    };
    Color::rgba(
        channel(color.r),
        channel(color.g),
        channel(color.b),
        color.a,
    )
}

fn luminance(color: Color) -> f32 {
    let linear = |value: u8| {
        let value = value as f32 / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
}

fn border_bands(
    style: &Style,
    rect: LayoutRect,
    widths: [f32; 4],
    colors: [Color; 4],
    side: usize,
) -> Vec<BorderBand> {
    if widths[side] <= 0.0 {
        return Vec::new();
    }
    let corners = |insets: [f32; 4]| {
        [
            (rect.x + insets[3], rect.y + insets[0]),
            (rect.x + rect.width - insets[1], rect.y + insets[0]),
            (
                rect.x + rect.width - insets[1],
                rect.y + rect.height - insets[2],
            ),
            (rect.x + insets[3], rect.y + rect.height - insets[2]),
        ]
    };
    let outer = corners([0.0; 4]);
    let inner = corners(widths);
    let next = (side + 1) % 4;
    let full = [outer[side], outer[next], inner[next], inner[side]];
    if !is_three_dimensional(style, side) {
        return if colors[side].a == 0 {
            Vec::new()
        } else {
            vec![BorderBand {
                points: full,
                color: colors[side],
                shadow: false,
                side,
                outer: None,
            }]
        };
    }
    let line_style = style.border_styles[side].unwrap();
    // Keep keyword provenance separate from the resolved computed RGB color.
    let base = if style.border_current_color.is_some_and(|mask| mask[side]) {
        Color::rgb(238, 238, 238)
    } else {
        colors[side]
    };
    if base.a == 0 {
        return Vec::new();
    }
    let dark = shade(base, true);
    let contrast = (luminance(base) + 0.05) / (luminance(dark) + 0.05);
    let light = if base.r < 150 && base.g < 92 && contrast < 1.75 {
        shade(base, false)
    } else {
        base
    };
    let top_or_left = side == 0 || side == 3;
    let outer_dark = match line_style {
        BorderLineStyle::Groove | BorderLineStyle::Inset => top_or_left,
        _ => !top_or_left,
    };
    let background = BorderBand {
        points: full,
        color: light,
        shadow: false,
        side,
        outer: None,
    };
    if matches!(line_style, BorderLineStyle::Inset | BorderLineStyle::Outset) {
        return if outer_dark {
            vec![
                background,
                BorderBand {
                    points: full,
                    color: dark,
                    shadow: true,
                    side,
                    outer: None,
                },
            ]
        } else {
            vec![background]
        };
    }
    let half = if top_or_left {
        (widths[side] / 2.0).ceil()
    } else {
        (widths[side] / 2.0).floor()
    };
    let fraction = half / widths[side];
    let interpolate =
        |a: (f32, f32), b: (f32, f32)| (a.0 + (b.0 - a.0) * fraction, a.1 + (b.1 - a.1) * fraction);
    let first = interpolate(outer[side], inner[side]);
    let second = interpolate(outer[next], inner[next]);
    let points = if outer_dark {
        [outer[side], outer[next], second, first]
    } else {
        [first, second, inner[next], inner[side]]
    };
    vec![
        background,
        BorderBand {
            points,
            color: dark,
            shadow: true,
            side,
            outer: Some(outer_dark),
        },
    ]
}

/// Fill the complete light ring before convex shadow bands. Shared same-color
/// corners overlap opaquely; differing-color miters retain convex AA coverage.
pub(crate) fn three_dimensional_layers(
    style: &Style,
    rect: LayoutRect,
    widths: [f32; 4],
    colors: [Color; 4],
) -> Vec<BorderLayer> {
    let mut bands: Vec<_> = (0..4)
        .flat_map(|side| border_bands(style, rect, widths, colors, side))
        .collect();
    let joins: Vec<_> = bands
        .iter()
        .map(|band| {
            [((band.side + 3) % 4), ((band.side + 1) % 4)].map(|neighbor| {
                band.shadow
                    && bands.iter().any(|other| {
                        other.side == neighbor
                            && other.shadow
                            && other.outer == band.outer
                            && other.color == band.color
                    })
            })
        })
        .collect();
    for (band, joins) in bands.iter_mut().zip(joins) {
        for (join, inner_point, outer_point) in [(joins[0], 3, 0), (joins[1], 2, 1)] {
            if join {
                if band.side == 0 || band.side == 2 {
                    band.points[inner_point].0 = band.points[outer_point].0;
                } else {
                    band.points[inner_point].1 = band.points[outer_point].1;
                }
            }
        }
    }
    let mut layers: Vec<BorderLayer> = Vec::new();
    for shadow in [false, true] {
        let start = layers.len();
        for band in bands.iter().filter(|band| band.shadow == shadow) {
            if let Some(layer) = layers[start..]
                .iter_mut()
                .find(|layer| !shadow && layer.color == band.color)
            {
                layer.polygons.push(band.points);
            } else {
                layers.push(BorderLayer {
                    color: band.color,
                    polygons: vec![band.points],
                    shadow,
                });
            }
        }
    }
    layers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_dimensional_shades_use_blink_srgb_quantization() {
        assert_eq!(
            shade(Color::rgb(238, 238, 238), true),
            Color::rgb(154, 154, 154)
        );
        assert_eq!(
            shade(Color::rgb(100, 100, 100), true),
            Color::rgb(15, 15, 15)
        );
        assert_eq!(
            shade(Color::rgb(255, 255, 255), true),
            Color::rgb(171, 171, 171)
        );
        assert_eq!(shade(Color::BLACK, false), Color::rgb(84, 84, 84));
        assert_eq!(
            shade(Color::rgba(0, 0, 0, 128), false),
            Color::rgba(84, 84, 84, 128)
        );
    }
}
