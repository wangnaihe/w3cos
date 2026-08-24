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
        Some(match name.to_lowercase().as_str() {
            "white" => Self::WHITE,
            "black" => Self::BLACK,
            "transparent" => Self::TRANSPARENT,
            "red" => Self::rgb(255, 0, 0),
            "green" => Self::rgb(0, 128, 0),
            "blue" => Self::rgb(0, 0, 255),
            "yellow" => Self::rgb(255, 255, 0),
            "aqua" | "cyan" => Self::rgb(0, 255, 255),
            "fuchsia" | "magenta" => Self::rgb(255, 0, 255),
            "gray" => Self::rgb(128, 128, 128),
            "lime" => Self::rgb(0, 255, 0),
            "maroon" => Self::rgb(128, 0, 0),
            "navy" => Self::rgb(0, 0, 128),
            "olive" => Self::rgb(128, 128, 0),
            "orange" => Self::rgb(255, 165, 0),
            "purple" => Self::rgb(128, 0, 128),
            "silver" => Self::rgb(192, 192, 192),
            "teal" => Self::rgb(0, 128, 128),
            _ => return None,
        })
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
            return (channels.len() == 3).then(|| {
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
            return (channels.len() == 4).then(|| {
                Some(Self::rgba(
                    parse_css_rgb_channel(channels[0])?,
                    parse_css_rgb_channel(channels[1])?,
                    parse_css_rgb_channel(channels[2])?,
                    parse_css_alpha_channel(channels[3])?,
                ))
            })?;
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
