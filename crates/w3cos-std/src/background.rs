use crate::Color;

#[derive(Debug, Clone, PartialEq)]
pub struct BackgroundShorthand {
    pub valid: bool,
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
        valid: true,
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
            } else if lower == "none" {
                image = token;
            } else if let Some(normalized) = normalize_background_image_token(&token) {
                image = normalized;
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
            } else if after_slash && is_background_size_token(&token) {
                size.push(token);
            } else if is_background_position_token(&token) {
                position.push(token);
            } else {
                result.valid = false;
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

fn is_background_image_token(token: &str) -> bool {
    normalize_background_image_token(token).is_some()
}

fn normalize_background_image_token(token: &str) -> Option<String> {
    let Some(open) = token.find('(') else {
        return None;
    };
    let function = css_unescape_identifier(token[..open].trim()).to_ascii_lowercase();
    if function == "url" {
        if !token.ends_with(')') {
            return None;
        }
        let Some(inner) = token.get(open + 1..token.len() - 1) else {
            return None;
        };
        let inner = inner.trim();
        if let Some(quote) = inner.chars().next().filter(|ch| matches!(ch, '\'' | '"')) {
            return (inner.len() >= 2 && inner.ends_with(quote)).then(|| format!("url({inner})"));
        }
        let mut escaped = false;
        for ch in inner.chars() {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
            } else if ch.is_whitespace() || matches!(ch, '\'' | '"' | '(' | ')') {
                return None;
            }
        }
        return (!escaped).then(|| format!("url({inner})"));
    }
    (matches!(
        function.as_str(),
        "linear-gradient"
            | "radial-gradient"
            | "repeating-linear-gradient"
            | "repeating-radial-gradient"
    ) && token.ends_with(')'))
    .then(|| format!("{function}{}", &token[open..]))
}

fn css_unescape_identifier(value: &str) -> String {
    let chars = value.chars().collect::<Vec<_>>();
    let mut output = String::new();
    let mut pos = 0;
    while pos < chars.len() {
        if chars[pos] != '\\' || pos + 1 == chars.len() {
            output.push(chars[pos]);
            pos += 1;
            continue;
        }
        pos += 1;
        if chars[pos].is_ascii_hexdigit() {
            let start = pos;
            while pos < chars.len() && pos < start + 6 && chars[pos].is_ascii_hexdigit() {
                pos += 1;
            }
            let digits = chars[start..pos].iter().collect::<String>();
            output.push(
                u32::from_str_radix(&digits, 16)
                    .ok()
                    .and_then(char::from_u32)
                    .unwrap_or('\u{fffd}'),
            );
            if pos < chars.len() && chars[pos].is_whitespace() {
                pos += 1;
            }
        } else if !matches!(chars[pos], '\n' | '\r' | '\u{000c}') {
            output.push(chars[pos]);
            pos += 1;
        }
    }
    output
}

fn is_background_position_token(token: &str) -> bool {
    matches!(
        token.to_ascii_lowercase().as_str(),
        "left" | "right" | "top" | "bottom" | "center"
    ) || is_background_length(token)
}

fn is_background_size_token(token: &str) -> bool {
    matches!(
        token.to_ascii_lowercase().as_str(),
        "auto" | "cover" | "contain"
    ) || is_background_length(token)
}

fn is_background_length(token: &str) -> bool {
    let token = token.trim().to_ascii_lowercase();
    if token == "0"
        || token.starts_with("calc(")
        || token.starts_with("min(")
        || token.starts_with("max(")
        || token.starts_with("clamp(")
        || token.starts_with("var(")
    {
        return true;
    }
    [
        "%", "px", "em", "rem", "ex", "ch", "vw", "vh", "vmin", "vmax", "cm", "mm", "q", "in",
        "pt", "pc",
    ]
    .into_iter()
    .any(|unit| {
        token
            .strip_suffix(unit)
            .is_some_and(|number| number.trim().parse::<f32>().is_ok())
    })
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
            token == "none" || is_background_image_token(&tokens[0])
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
    let mut characters = value.chars().peekable();
    while let Some(ch) = characters.next() {
        if ch == '\\' {
            current.push(ch);
            if characters.peek().is_some_and(char::is_ascii_hexdigit) {
                for _ in 0..6 {
                    let Some(next) = characters.peek().copied() else {
                        break;
                    };
                    if !next.is_ascii_hexdigit() {
                        break;
                    }
                    current.push(next);
                    characters.next();
                }
                if characters.peek().is_some_and(|next| next.is_whitespace()) {
                    characters.next();
                }
            } else if let Some(next) = characters.next() {
                current.push(next);
            }
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

    #[test]
    fn invalid_background_tokens_do_not_form_a_shorthand() {
        assert!(!parse_shorthand("\"red\"").valid);
        assert!(!parse_shorthand("red;").valid);
        assert!(!parse_shorthand("\\0020red").valid);
        assert!(!parse_shorthand("red url( { test )").valid);
        assert!(parse_shorthand("green center / cover no-repeat").valid);
        let escaped_url = parse_shorthand(r#"red U\r\4c ("green.png")"#);
        assert!(escaped_url.valid);
        assert_eq!(escaped_url.images, [r#"url("green.png")"#]);
        let bracket_url = parse_shorthand("url([) green");
        assert!(bracket_url.valid);
        assert_eq!(bracket_url.color, Color::from_css("green"));
    }
}
