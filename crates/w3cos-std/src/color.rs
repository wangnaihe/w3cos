use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    pub fn from_hex(hex: &str) -> Self {
        let bare_hex = hex.trim_start_matches('#');
        let is_hex = matches!(bare_hex.len(), 3 | 4 | 6 | 8)
            && bare_hex.chars().all(|c| c.is_ascii_hexdigit());
        if !is_hex {
            return Self::from_named(hex).unwrap_or(Self::BLACK);
        }
        let hex = bare_hex;
        let hex = if matches!(hex.len(), 3 | 4) {
            hex.chars()
                .flat_map(|character| [character, character])
                .collect()
        } else {
            hex.to_string()
        };
        let len = hex.len();
        match len {
            6 => {
                let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0);
                let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0);
                let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0);
                Self::rgb(r, g, b)
            }
            8 => {
                let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0);
                let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0);
                let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0);
                let a = u8::from_str_radix(&hex[6..8], 16).unwrap_or(255);
                Self::rgba(r, g, b, a)
            }
            _ => Self::rgb(0, 0, 0),
        }
    }

    pub fn from_named(name: &str) -> Option<Self> {
        // All opaque sRGB keywords from CSS Color 4 section 6.1:
        // https://www.w3.org/TR/css-color-4/#named-colors
        let rgb = match name.to_ascii_lowercase().as_str() {
            "transparent" => return Some(Self::TRANSPARENT),
            "aliceblue" => 0xf0f8ff,
            "antiquewhite" => 0xfaebd7,
            "aqua" => 0x00ffff,
            "aquamarine" => 0x7fffd4,
            "azure" => 0xf0ffff,
            "beige" => 0xf5f5dc,
            "bisque" => 0xffe4c4,
            "black" => 0x000000,
            "blanchedalmond" => 0xffebcd,
            "blue" => 0x0000ff,
            "blueviolet" => 0x8a2be2,
            "brown" => 0xa52a2a,
            "burlywood" => 0xdeb887,
            "cadetblue" => 0x5f9ea0,
            "chartreuse" => 0x7fff00,
            "chocolate" => 0xd2691e,
            "coral" => 0xff7f50,
            "cornflowerblue" => 0x6495ed,
            "cornsilk" => 0xfff8dc,
            "crimson" => 0xdc143c,
            "cyan" => 0x00ffff,
            "darkblue" => 0x00008b,
            "darkcyan" => 0x008b8b,
            "darkgoldenrod" => 0xb8860b,
            "darkgray" => 0xa9a9a9,
            "darkgreen" => 0x006400,
            "darkgrey" => 0xa9a9a9,
            "darkkhaki" => 0xbdb76b,
            "darkmagenta" => 0x8b008b,
            "darkolivegreen" => 0x556b2f,
            "darkorange" => 0xff8c00,
            "darkorchid" => 0x9932cc,
            "darkred" => 0x8b0000,
            "darksalmon" => 0xe9967a,
            "darkseagreen" => 0x8fbc8f,
            "darkslateblue" => 0x483d8b,
            "darkslategray" => 0x2f4f4f,
            "darkslategrey" => 0x2f4f4f,
            "darkturquoise" => 0x00ced1,
            "darkviolet" => 0x9400d3,
            "deeppink" => 0xff1493,
            "deepskyblue" => 0x00bfff,
            "dimgray" => 0x696969,
            "dimgrey" => 0x696969,
            "dodgerblue" => 0x1e90ff,
            "firebrick" => 0xb22222,
            "floralwhite" => 0xfffaf0,
            "forestgreen" => 0x228b22,
            "fuchsia" => 0xff00ff,
            "gainsboro" => 0xdcdcdc,
            "ghostwhite" => 0xf8f8ff,
            "gold" => 0xffd700,
            "goldenrod" => 0xdaa520,
            "gray" => 0x808080,
            "green" => 0x008000,
            "greenyellow" => 0xadff2f,
            "grey" => 0x808080,
            "honeydew" => 0xf0fff0,
            "hotpink" => 0xff69b4,
            "indianred" => 0xcd5c5c,
            "indigo" => 0x4b0082,
            "ivory" => 0xfffff0,
            "khaki" => 0xf0e68c,
            "lavender" => 0xe6e6fa,
            "lavenderblush" => 0xfff0f5,
            "lawngreen" => 0x7cfc00,
            "lemonchiffon" => 0xfffacd,
            "lightblue" => 0xadd8e6,
            "lightcoral" => 0xf08080,
            "lightcyan" => 0xe0ffff,
            "lightgoldenrodyellow" => 0xfafad2,
            "lightgray" => 0xd3d3d3,
            "lightgreen" => 0x90ee90,
            "lightgrey" => 0xd3d3d3,
            "lightpink" => 0xffb6c1,
            "lightsalmon" => 0xffa07a,
            "lightseagreen" => 0x20b2aa,
            "lightskyblue" => 0x87cefa,
            "lightslategray" => 0x778899,
            "lightslategrey" => 0x778899,
            "lightsteelblue" => 0xb0c4de,
            "lightyellow" => 0xffffe0,
            "lime" => 0x00ff00,
            "limegreen" => 0x32cd32,
            "linen" => 0xfaf0e6,
            "magenta" => 0xff00ff,
            "maroon" => 0x800000,
            "mediumaquamarine" => 0x66cdaa,
            "mediumblue" => 0x0000cd,
            "mediumorchid" => 0xba55d3,
            "mediumpurple" => 0x9370db,
            "mediumseagreen" => 0x3cb371,
            "mediumslateblue" => 0x7b68ee,
            "mediumspringgreen" => 0x00fa9a,
            "mediumturquoise" => 0x48d1cc,
            "mediumvioletred" => 0xc71585,
            "midnightblue" => 0x191970,
            "mintcream" => 0xf5fffa,
            "mistyrose" => 0xffe4e1,
            "moccasin" => 0xffe4b5,
            "navajowhite" => 0xffdead,
            "navy" => 0x000080,
            "oldlace" => 0xfdf5e6,
            "olive" => 0x808000,
            "olivedrab" => 0x6b8e23,
            "orange" => 0xffa500,
            "orangered" => 0xff4500,
            "orchid" => 0xda70d6,
            "palegoldenrod" => 0xeee8aa,
            "palegreen" => 0x98fb98,
            "paleturquoise" => 0xafeeee,
            "palevioletred" => 0xdb7093,
            "papayawhip" => 0xffefd5,
            "peachpuff" => 0xffdab9,
            "peru" => 0xcd853f,
            "pink" => 0xffc0cb,
            "plum" => 0xdda0dd,
            "powderblue" => 0xb0e0e6,
            "purple" => 0x800080,
            "rebeccapurple" => 0x663399,
            "red" => 0xff0000,
            "rosybrown" => 0xbc8f8f,
            "royalblue" => 0x4169e1,
            "saddlebrown" => 0x8b4513,
            "salmon" => 0xfa8072,
            "sandybrown" => 0xf4a460,
            "seagreen" => 0x2e8b57,
            "seashell" => 0xfff5ee,
            "sienna" => 0xa0522d,
            "silver" => 0xc0c0c0,
            "skyblue" => 0x87ceeb,
            "slateblue" => 0x6a5acd,
            "slategray" => 0x708090,
            "slategrey" => 0x708090,
            "snow" => 0xfffafa,
            "springgreen" => 0x00ff7f,
            "steelblue" => 0x4682b4,
            "tan" => 0xd2b48c,
            "teal" => 0x008080,
            "thistle" => 0xd8bfd8,
            "tomato" => 0xff6347,
            "turquoise" => 0x40e0d0,
            "violet" => 0xee82ee,
            "wheat" => 0xf5deb3,
            "white" => 0xffffff,
            "whitesmoke" => 0xf5f5f5,
            "yellow" => 0xffff00,
            "yellowgreen" => 0x9acd32,
            _ => return None,
        };
        Some(Self::rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8))
    }

    pub fn from_css(value: &str) -> Option<Self> {
        let value = value.trim().to_ascii_lowercase();
        if value.starts_with('#') {
            let hex = value.strip_prefix('#').expect("prefix checked");
            return (matches!(hex.len(), 3 | 4 | 6 | 8)
                && hex.chars().all(|character| character.is_ascii_hexdigit()))
            .then(|| Self::from_hex(&value));
        }
        if let Some(color) = Self::from_named(&value) {
            return Some(color);
        }
        if let Some(arguments) = value
            .strip_prefix("rgb(")
            .and_then(|value| value.strip_suffix(')'))
        {
            let channels = arguments.split(',').map(str::trim).collect::<Vec<_>>();
            return (channels.len() == 3 && rgb_channels_use_one_unit(&channels)).then(|| {
                Some(Self::rgb(
                    parse_css_rgb_channel(channels[0])?,
                    parse_css_rgb_channel(channels[1])?,
                    parse_css_rgb_channel(channels[2])?,
                ))
            })?;
        }
        if let Some(arguments) = value
            .strip_prefix("rgba(")
            .and_then(|value| value.strip_suffix(')'))
        {
            let channels = arguments.split(',').map(str::trim).collect::<Vec<_>>();
            return (channels.len() == 4 && rgb_channels_use_one_unit(&channels[..3])).then(
                || {
                    Some(Self::rgba(
                        parse_css_rgb_channel(channels[0])?,
                        parse_css_rgb_channel(channels[1])?,
                        parse_css_rgb_channel(channels[2])?,
                        parse_css_alpha_channel(channels[3])?,
                    ))
                },
            )?;
        }
        None
    }

    pub const WHITE: Self = Self::rgb(255, 255, 255);
    pub const BLACK: Self = Self::rgb(0, 0, 0);
    pub const TRANSPARENT: Self = Self::rgba(0, 0, 0, 0);

    pub fn to_u32(self) -> u32 {
        (self.a as u32) << 24 | (self.r as u32) << 16 | (self.g as u32) << 8 | self.b as u32
    }
}

fn rgb_channels_use_one_unit(channels: &[&str]) -> bool {
    channels.first().is_none_or(|first| {
        channels
            .iter()
            .all(|channel| channel.ends_with('%') == first.ends_with('%'))
    })
}

fn parse_css_rgb_channel(value: &str) -> Option<u8> {
    let (number, maximum) = match value.strip_suffix('%') {
        Some(percentage) => (percentage.parse::<f32>().ok()?, 100.0),
        None => (value.parse::<f32>().ok()?, 255.0),
    };
    number
        .is_finite()
        .then(|| (number.clamp(0.0, maximum) * 255.0 / maximum).round() as u8)
}

fn parse_css_alpha_channel(value: &str) -> Option<u8> {
    let (number, maximum) = match value.strip_suffix('%') {
        Some(percentage) => (percentage.parse::<f32>().ok()?, 100.0),
        None => (value.parse::<f32>().ok()?, 1.0),
    };
    number
        .is_finite()
        .then(|| (number.clamp(0.0, maximum) * 255.0 / maximum).round() as u8)
}

#[cfg(test)]
mod named_color_tests {
    use super::Color;

    #[test]
    fn extended_css_named_colors_use_the_standard_srgb_values() {
        for (name, expected) in [
            ("pink", Color::rgb(255, 192, 203)),
            ("aliceblue", Color::rgb(240, 248, 255)),
            ("darkslategrey", Color::rgb(47, 79, 79)),
            ("lightgoldenrodyellow", Color::rgb(250, 250, 210)),
            ("rebeccapurple", Color::rgb(102, 51, 153)),
            ("yellowgreen", Color::rgb(154, 205, 50)),
        ] {
            assert_eq!(Color::from_named(name), Some(expected), "named color {name}");
            assert_eq!(Color::from_css(&name.to_ascii_uppercase()), Some(expected));
            assert_eq!(Color::from_hex(name), expected);
        }
        assert_eq!(Color::from_named("currentcolor"), None);
        assert_eq!(Color::from_named("not-a-color"), None);
        assert_eq!(Color::from_named("transparent"), Some(Color::TRANSPARENT));
    }
}
