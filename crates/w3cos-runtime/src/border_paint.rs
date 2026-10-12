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
    // Square CSS border paint boxes snap their endpoints in local CSS space
    // before transforms. Keep layout and rounded/collapsed geometry separate.
    // Original browser command logs retain fractional table layout widths but
    // paint these outer edges at integer coordinates.
    let rect = if !style.border_collapse
        && style.border_corner_radii().iter().all(|radius| *radius <= 0.0)
    {
        let snap = |value:f32| (value+0.5).floor();
        let left=snap(rect.x);
        let top=snap(rect.y);
        LayoutRect {x:left,y:top,width:(snap(rect.x+rect.width)-left).max(0.0),
            height:(snap(rect.y+rect.height)-top).max(0.0)}
    } else {rect};
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
    let full = if style.border_collapse
        && style.display == w3cos_std::style::Display::TableCell
    {
        let edge = crate::paint_artifact::border_edge_paint_rects(style, rect, widths)[side];
        // Collapsed table ink uses pixel-snapped edges, unlike the fractional
        // layout grid. Snap endpoints, not the width in isolation, so adjacent
        // tracks retain a common painted boundary (Blink TablePainter).
        let snap = |value: f32| (value + 0.5).floor();
        let left = snap(edge.x);
        let top = snap(edge.y);
        let right = snap(edge.x + edge.width);
        let bottom = snap(edge.y + edge.height);
        let points = [(left, top), (right, top), (right, bottom), (left, bottom)];
        // Orient each band from its outer edge toward the cell interior.
        std::array::from_fn(|index| points[(side + index) % 4])
    } else {
        [outer[side], outer[next], inner[next], inner[side]]
    };
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
    let line_style = match (style.border_collapse, style.border_styles[side].unwrap()) {
        (true, BorderLineStyle::Inset) => BorderLineStyle::Ridge,
        (true, BorderLineStyle::Outset) => BorderLineStyle::Groove,
        (_, line_style) => line_style,
    };
    // Keep keyword provenance separate from the resolved computed RGB color.
    // Blink resolves initial 3D border color against actual currentColor for
    // table display types; other boxes retain the historical #eee fallback.
    let table_display = matches!(style.display,
        w3cos_std::style::Display::Table | w3cos_std::style::Display::InlineTable
        | w3cos_std::style::Display::TableRowGroup
        | w3cos_std::style::Display::TableHeaderGroup
        | w3cos_std::style::Display::TableFooterGroup
        | w3cos_std::style::Display::TableRow
        | w3cos_std::style::Display::TableColumnGroup
        | w3cos_std::style::Display::TableColumn
        | w3cos_std::style::Display::TableCell
        | w3cos_std::style::Display::TableCaption);
    let base = if !table_display && style.border_current_color.is_some_and(|mask| mask[side]) {
        Color::rgb(238, 238, 238)
    } else {
        colors[side]
    };
    if base.a == 0 {
        return Vec::new();
    }
    // Chromium's current inset/outset softening matches WebKit. Very dark
    // colors need two lightening steps to distinguish both bevel faces.
    let brightness = luminance(base);
    let (dark, light) = if brightness <= 0.014443844 {
        let dark=shade(base,false);
        (dark,shade(dark,false))
    } else {
        (shade(base,true),if brightness > 0.83077 {base}else{shade(base,false)})
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
    let first = interpolate(full[0], full[3]);
    let second = interpolate(full[1], full[2]);
    let points = if outer_dark {
        [full[0], full[1], second, first]
    } else {
        [first, second, full[2], full[3]]
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
    if style.border_collapse && style.display == w3cos_std::style::Display::TableCell {
        // Each collapsed grid edge is an independent filled rectangle. Its
        // shadow belongs to that edge, not to a shared light/shadow ring:
        // otherwise a losing perpendicular shadow can darken winning ink.
        return [3, 0, 1, 2].into_iter()
            .flat_map(|side| border_bands(style, rect, widths, colors, side))
            .map(|band| BorderLayer {
                polygons: vec![band.points], color: band.color, shadow: band.shadow,
            }).collect();
    }
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
    fn collapsed_three_dimensional_edges_use_browser_light_color() {
        // V4040 original cell-001: Chrome's collapsed green edge is (0,212,0),
        // while the prior box-border contrast heuristic kept (0,128,0).
        let rect = LayoutRect { x: 43.0, y: 35.0, width: 42.0, height: 36.0 };
        for line in [BorderLineStyle::Inset, BorderLineStyle::Outset,
            BorderLineStyle::Groove, BorderLineStyle::Ridge] {
            let style = Style { border_collapse: true,
                display: w3cos_std::style::Display::TableCell,
                border_styles: [Some(line); 4], ..Style::default() };
            for side in 0..4 {
                let bands = border_bands(&style, rect, [10.0; 4], [Color::rgb(0,128,0); 4], side);
                assert_eq!(bands[0].color, Color::rgb(0,212,0), "{line:?}, side={side}");
            }
        }
    }

    #[test]
    fn separate_three_dimensional_border_uses_browser_light_color() {
        let style = Style { border_styles: [Some(BorderLineStyle::Groove);4],
            ..Style::default() };
        let rect = LayoutRect {x:20.0,y:30.0,width:40.0,height:30.0};
        for side in 0..4 {
            let bands=border_bands(&style,rect,[10.0;4],[Color::rgb(0,128,0);4],side);
            assert_eq!(bands[0].color,Color::rgb(0,212,0));
        }
    }

    #[test]
    fn table_current_color_border_softens_black_without_box_fallback() {
        // Chrome 155 V4084: table outset top=168 and right=84. Ordinary
        // currentColor outset retains the legacy 238/154 paint fallback.
        let rect=LayoutRect {x:8.0,y:8.0,width:100.0,height:100.0};
        for display in [w3cos_std::style::Display::Table,
            w3cos_std::style::Display::InlineTable,
            w3cos_std::style::Display::TableCell] {
            let style=Style {display,border_current_color:Some([true;4]),
                border_styles:[Some(BorderLineStyle::Outset);4],..Style::default()};
            let top=border_bands(&style,rect,[1.0;4],[Color::BLACK;4],0);
            let right=border_bands(&style,rect,[1.0;4],[Color::BLACK;4],1);
            assert_eq!(top[0].color,Color::rgb(168,168,168));
            assert_eq!(right.last().unwrap().color,Color::rgb(84,84,84));
        }
        let style=Style {border_current_color:Some([true;4]),
            border_styles:[Some(BorderLineStyle::Outset);4],..Style::default()};
        assert_eq!(border_bands(&style,rect,[10.0;4],[Color::BLACK;4],0)[0].color,
            Color::rgb(238,238,238));
    }

    #[test]
    fn collapsed_layers_keep_each_edges_shading_together() {
        let style = Style { border_collapse: true,
            display: w3cos_std::style::Display::TableCell,
            border_styles: [Some(BorderLineStyle::Groove); 4], ..Style::default() };
        let rect = LayoutRect { x: 43.0, y: 35.0, width: 42.0, height: 36.0 };
        let layers = three_dimensional_layers(&style, rect, [10.0; 4], [Color::rgb(0, 128, 0); 4]);
        assert_eq!(layers.len(), 8, "collapsed edges paint independently, not as one shared shadow ring");
        for edge in layers.chunks_exact(2) {
            assert!(!edge[0].shadow);
            assert!(edge[1].shadow);
        }
        assert_eq!(layers[2].polygons[0],
            [(43.0, 30.0), (85.0, 30.0), (85.0, 40.0), (43.0, 40.0)],
            "grid order paints the left edge, horizontal edge, then the right edge");
    }

    #[test]
    fn collapsed_three_dimensional_band_snaps_shared_edge_pixels() {
        let style = Style { border_collapse: true,
            display: w3cos_std::style::Display::TableCell,
            border_styles: [Some(BorderLineStyle::Groove); 4], ..Style::default() };
        let rect = LayoutRect { x: 43.0, y: 35.0, width: 41.828125, height: 36.0 };
        let bands = border_bands(&style, rect, [10.0; 4], [Color::rgb(0, 128, 0); 4], 1);
        assert_eq!(bands[0].points,
            [(90.0, 30.0), (90.0, 76.0), (80.0, 76.0), (80.0, 30.0)],
            "collapsed ink snaps its endpoints without changing layout geometry");
        assert_eq!(bands[1].points,
            [(85.0, 30.0), (85.0, 76.0), (80.0, 76.0), (80.0, 30.0)]);
    }

    #[test]
    fn collapsed_three_dimensional_band_uses_shared_edge_geometry() {
        let style = Style { border_collapse: true,
            display: w3cos_std::style::Display::TableCell,
            border_styles: [Some(BorderLineStyle::Groove); 4], ..Style::default() };
        let rect = LayoutRect { x: 20.0, y: 30.0, width: 40.0, height: 30.0 };
        let bands = border_bands(&style, rect, [10.0; 4], [Color::rgb(0, 128, 0); 4], 0);
        assert_eq!(bands[0].points,
            [(20.0, 25.0), (60.0, 25.0), (60.0, 35.0), (20.0, 35.0)],
            "a collapsed edge is centered on its grid line and has no cell-box miter");
    }

    #[test]
    fn collapsed_inset_outset_use_ridge_groove_shading() {
        let rect = LayoutRect { x: 20.0, y: 30.0, width: 40.0, height: 30.0 };
        for (authored, effective) in [(BorderLineStyle::Outset, BorderLineStyle::Groove),
            (BorderLineStyle::Inset, BorderLineStyle::Ridge)] {
            let style = Style { border_collapse: true,
                display: w3cos_std::style::Display::TableCell,
                border_styles: [Some(authored); 4], ..Style::default() };
            let expected_style = Style { border_styles: [Some(effective); 4], ..style.clone() };
            for side in 0..4 {
                let actual = border_bands(&style, rect, [10.0; 4], [Color::rgb(0, 128, 0); 4], side);
                let expected = border_bands(&expected_style, rect, [10.0; 4], [Color::rgb(0, 128, 0); 4], side);
                assert_eq!(actual.len(), expected.len());
                for (actual, expected) in actual.iter().zip(&expected) {
                    assert_eq!(actual.points, expected.points, "{authored:?}, side={side}");
                    assert_eq!(actual.color, expected.color);
                    assert_eq!(actual.shadow, expected.shadow);
                }
            }
        }
    }

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
