use crate::Color;

#[derive(Debug, Clone, PartialEq)]
pub struct BackgroundShorthand {
    pub color: Option<Color>,
    pub images: Vec<String>,
    pub sizes: Vec<String>,
    pub positions: Vec<String>,
    pub repeats: Vec<String>,
    pub origins: Vec<String>,
    pub clips: Vec<String>,
    pub attachments: Vec<String>,
}

pub fn parse_shorthand(value: &str) -> BackgroundShorthand {
    let mut result = BackgroundShorthand {
        color: None,
        images: Vec::new(),
        sizes: Vec::new(),
        positions: Vec::new(),
        repeats: Vec::new(),
        origins: Vec::new(),
        clips: Vec::new(),
        attachments: Vec::new(),
    };
    for layer in split_top_level(value, ',') {
        let tokens = split_tokens(layer);
        let mut image = "none".to_string();
        let mut position = Vec::new();
        let mut size = Vec::new();
        let mut repeat = Vec::new();
        let mut boxes = Vec::new();
        let mut attachment = "scroll".to_string();
        let mut after_slash = false;
        for token in tokens {
            let lower = token.to_ascii_lowercase();
            if token == "/" {
                after_slash = true;
            } else if lower == "none"
                || lower.starts_with("url(")
                || lower.starts_with("linear-gradient(")
                || lower.starts_with("radial-gradient(")
                || lower.starts_with("repeating-linear-gradient(")
                || lower.starts_with("repeating-radial-gradient(")
            {
                image = token;
            } else if matches!(lower.as_str(), "scroll" | "fixed" | "local") {
                attachment = lower;
            } else if matches!(
                lower.as_str(),
                "repeat" | "no-repeat" | "repeat-x" | "repeat-y" | "round" | "space"
            ) {
                repeat.push(token);
            } else if matches!(lower.as_str(), "border-box" | "padding-box" | "content-box") {
                boxes.push(token);
            } else if let Some(color) = Color::from_css(&token) {
                result.color = Some(color);
            } else if after_slash {
                size.push(token);
            } else {
                position.push(token);
            }
        }
        result.images.push(image);
        result.sizes.push(if size.is_empty() {
            "auto".to_string()
        } else {
            size.join(" ")
        });
        result.positions.push(if position.is_empty() {
            "0% 0%".to_string()
        } else {
            position.join(" ")
        });
        result.repeats.push(if repeat.is_empty() {
            "repeat".to_string()
        } else {
            repeat.join(" ")
        });
        result.origins.push(
            boxes
                .first()
                .cloned()
                .unwrap_or_else(|| "padding-box".to_string()),
        );
        result.clips.push(
            boxes
                .get(1)
                .or_else(|| boxes.first())
                .cloned()
                .unwrap_or_else(|| "border-box".to_string()),
        );
        result.attachments.push(attachment);
    }
    result
}

pub fn is_valid_image_list(value: &str) -> bool {
    let layers = split_top_level(value, ',');
    !layers.is_empty()
        && layers.into_iter().all(|layer| {
            let tokens = split_tokens(layer);
            if tokens.len() != 1 {
                return false;
            }
            let token = tokens[0].trim().to_ascii_lowercase();
            token == "none"
                || [
                    "url(",
                    "linear-gradient(",
                    "radial-gradient(",
                    "repeating-linear-gradient(",
                    "repeating-radial-gradient(",
                ]
                .into_iter()
                .any(|prefix| token.starts_with(prefix) && token.ends_with(')'))
        })
}

pub fn split_top_level(value: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0_i32;
    let mut quote = None;
    let mut start = 0;
    let mut escaped = false;
    for (index, ch) in value.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if let Some(active) = quote {
            if ch == active {
                quote = None;
            }
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            continue;
        }
        match ch {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = (depth - 1).max(0),
            _ if ch == separator && depth == 0 => {
                parts.push(value[start..index].trim());
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(value[start..].trim());
    parts
}

fn split_tokens(value: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut depth = 0_u32;
    let mut quote = None;
    let mut escaped = false;
    for ch in value.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' {
            current.push(ch);
            escaped = true;
            continue;
        }
        if let Some(active) = quote {
            current.push(ch);
            if ch == active {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => {
                quote = Some(ch);
                current.push(ch);
            }
            '(' => {
                depth += 1;
                current.push(ch);
            }
            ')' => {
                depth = depth.saturating_sub(1);
                current.push(ch);
            }
            '/' if depth == 0 => {
                if !current.trim().is_empty() {
                    tokens.push(current.trim().to_string());
                }
                current.clear();
                tokens.push("/".to_string());
            }
            ch if ch.is_whitespace() && depth == 0 => {
                if !current.trim().is_empty() {
                    tokens.push(current.trim().to_string());
                    current.clear();
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        tokens.push(current.trim().to_string());
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_common_raster_background_shorthand() {
        let parsed = parse_shorthand(
            "#123456 url('map/tile.png') center / cover no-repeat content-box padding-box",
        );
        assert_eq!(parsed.images, ["url('map/tile.png')"]);
        assert_eq!(parsed.positions, ["center"]);
        assert_eq!(parsed.sizes, ["cover"]);
        assert_eq!(parsed.repeats, ["no-repeat"]);
        assert_eq!(parsed.origins, ["content-box"]);
        assert_eq!(parsed.clips, ["padding-box"]);
        assert_eq!(parsed.attachments, ["scroll"]);
        assert_eq!(parsed.color, Some(Color::from_hex("#123456")));
    }

    #[test]
    fn none_and_color_still_expand_all_reset_longhands() {
        let none = parse_shorthand("none");
        assert_eq!(none.images, ["none"]);
        assert_eq!(none.positions, ["0% 0%"]);
        assert_eq!(none.sizes, ["auto"]);
        assert_eq!(none.repeats, ["repeat"]);
        assert_eq!(none.origins, ["padding-box"]);
        assert_eq!(none.clips, ["border-box"]);
        assert_eq!(none.attachments, ["scroll"]);
        assert_eq!(none.color, None);

        let color = parse_shorthand("red");
        assert_eq!(color.images, ["none"]);
        assert_eq!(color.color, Color::from_css("red"));
    }

    #[test]
    fn preserves_repeating_gradients_and_attachment_per_layer() {
        let parsed = parse_shorthand(
            "repeating-linear-gradient(to right, red 0 10%, blue 10% 20%) fixed, \
             repeating-radial-gradient(circle, white, black 12px) local",
        );
        assert!(parsed.images[0].starts_with("repeating-linear-gradient("));
        assert!(parsed.images[1].starts_with("repeating-radial-gradient("));
        assert_eq!(parsed.attachments, ["fixed", "local"]);
    }

    #[test]
    fn background_image_longhand_rejects_shorthand_tokens() {
        assert!(is_valid_image_list("url('tile.png'), none"));
        assert!(is_valid_image_list("linear-gradient(red, blue)"));
        assert!(!is_valid_image_list("url('tile.png') repeat"));
        assert!(!is_valid_image_list("red"));
    }
}
