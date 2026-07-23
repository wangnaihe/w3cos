//! Shared CSS value parsing for compiler codegen.

use w3cos_std::style::{SafeAreaEdge, Spacing};

/// Split `calc()` terms on `+` / `-` outside parentheses.
pub fn split_calc_terms(inner: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut current = String::new();
    let mut depth = 0i32;
    for ch in inner.chars() {
        match ch {
            '(' => {
                depth += 1;
                current.push(ch);
            }
            ')' => {
                depth -= 1;
                current.push(ch);
            }
            '+' if depth == 0 => {
                if !current.trim().is_empty() {
                    terms.push(current.trim().to_string());
                }
                current.clear();
            }
            '-' if depth == 0 && !current.trim().is_empty() => {
                terms.push(current.trim().to_string());
                current = "-".to_string();
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        terms.push(current.trim().to_string());
    }
    terms
}

/// Evaluate `calc()` containing only `px` literals.
pub fn css_parse_calc_px(value: &str) -> Option<f32> {
    let inner = value.trim().strip_prefix("calc(")?.strip_suffix(')')?;
    let mut sum = 0.0f32;
    for term in split_calc_terms(inner) {
        let negative = term.starts_with('-');
        let t = term
            .trim()
            .trim_start_matches('-')
            .trim_start_matches('+')
            .trim();
        let v = parse_plain_px(t)?;
        sum += if negative { -v } else { v };
    }
    Some(sum)
}

pub fn parse_plain_px(value: &str) -> Option<f32> {
    let trimmed = value.trim();
    if trimmed.starts_with("calc(") {
        return css_parse_calc_px(trimmed);
    }
    let v = trimmed.trim_end_matches("px");
    v.parse().ok()
}

/// Parse spacing: `px`, `env()`, or `calc(px + env())`.
pub fn css_parse_spacing_value(value: &str) -> Option<Spacing> {
    let trimmed = value.trim().trim_end_matches(';');
    if let Some(inner) = trimmed
        .strip_prefix("calc(")
        .and_then(|s| s.strip_suffix(')'))
    {
        let mut px_sum = 0.0f32;
        let mut safe_area = None;
        let mut keyboard_inset = false;
        for term in split_calc_terms(inner) {
            let negative = term.starts_with('-');
            let t = term
                .trim()
                .trim_start_matches('-')
                .trim_start_matches('+')
                .trim();
            if let Some(inner_env) = t.strip_prefix("env(").and_then(|s| s.strip_suffix(')')) {
                let name = inner_env.split(',').next()?.trim();
                match name {
                    "safe-area-inset-top" => safe_area = Some(SafeAreaEdge::Top),
                    "safe-area-inset-right" => safe_area = Some(SafeAreaEdge::Right),
                    "safe-area-inset-bottom" => safe_area = Some(SafeAreaEdge::Bottom),
                    "safe-area-inset-left" => safe_area = Some(SafeAreaEdge::Left),
                    "keyboard-inset-height" => keyboard_inset = true,
                    _ => return None,
                }
            } else if let Some(v) = parse_plain_px(t) {
                px_sum += if negative { -v } else { v };
            } else {
                return None;
            }
        }
        if px_sum.abs() < f32::EPSILON && safe_area.is_none() && !keyboard_inset {
            return None;
        }
        return Some(Spacing::Composite {
            px: px_sum,
            safe_area,
            keyboard_inset,
        });
    }
    if let Some(inner) = trimmed
        .strip_prefix("env(")
        .and_then(|s| s.strip_suffix(')'))
    {
        let name = inner.split(',').next()?.trim();
        return match name {
            "safe-area-inset-top" => Some(Spacing::SafeAreaInset(SafeAreaEdge::Top)),
            "safe-area-inset-right" => Some(Spacing::SafeAreaInset(SafeAreaEdge::Right)),
            "safe-area-inset-bottom" => Some(Spacing::SafeAreaInset(SafeAreaEdge::Bottom)),
            "safe-area-inset-left" => Some(Spacing::SafeAreaInset(SafeAreaEdge::Left)),
            "keyboard-inset-height" => Some(Spacing::KeyboardInsetHeight),
            _ => None,
        };
    }
    parse_plain_px(trimmed).map(Spacing::Px)
}

#[derive(Debug, Clone, Default)]
pub struct ParsedTransform {
    pub translate_x: f32,
    pub translate_y: f32,
    pub scale_x: f32,
    pub scale_y: f32,
    pub rotate_deg: f32,
}

impl ParsedTransform {
    pub fn identity() -> Self {
        Self {
            scale_x: 1.0,
            scale_y: 1.0,
            ..Default::default()
        }
    }
}

pub fn parse_transform(value: &str) -> ParsedTransform {
    let mut t = ParsedTransform::identity();
    let v = value.trim();

    if let Some(inner) = extract_fn(v, "translateX") {
        t.translate_x = parse_plain_px(inner).unwrap_or(0.0);
    }
    if let Some(inner) = extract_fn(v, "translateY") {
        t.translate_y = parse_plain_px(inner).unwrap_or(0.0);
    }
    if let Some(inner) = extract_fn(v, "translate") {
        let parts: Vec<&str> = inner.split(',').collect();
        if let Some(x) = parts.first().and_then(|s| parse_plain_px(s.trim())) {
            t.translate_x = x;
        }
        if let Some(y) = parts.get(1).and_then(|s| parse_plain_px(s.trim())) {
            t.translate_y = y;
        }
    }
    if let Some(inner) = extract_fn(v, "scale") {
        let parts: Vec<&str> = inner.split(',').collect();
        if let Ok(sx) = parts[0].trim().parse::<f32>() {
            t.scale_x = sx;
            t.scale_y = parts
                .get(1)
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(sx);
        }
    }
    if let Some(inner) = extract_fn(v, "scaleX")
        && let Ok(s) = inner.trim().parse::<f32>()
    {
        t.scale_x = s;
    }
    if let Some(inner) = extract_fn(v, "scaleY")
        && let Ok(s) = inner.trim().parse::<f32>()
    {
        t.scale_y = s;
    }
    if let Some(inner) = extract_fn(v, "rotate") {
        let deg_str = inner.trim().trim_end_matches("deg").trim_end_matches("rad");
        if let Ok(d) = deg_str.parse::<f32>() {
            t.rotate_deg = if inner.contains("rad") {
                d.to_degrees()
            } else {
                d
            };
        }
    }
    t
}

#[derive(Debug, Clone)]
pub struct ParsedBoxShadow {
    pub offset_x: f32,
    pub offset_y: f32,
    pub blur: f32,
    pub spread: f32,
    pub color: String,
}

pub fn parse_box_shadow(value: &str) -> Option<ParsedBoxShadow> {
    let parts = split_css_whitespace(value);
    if parts.len() < 3 {
        return None;
    }
    let ox = parse_plain_px(&parts[0])?;
    let oy = parse_plain_px(&parts[1])?;
    let blur = parse_plain_px(&parts[2])?;
    let (spread, color_index) = parts
        .get(3)
        .and_then(|part| parse_plain_px(part))
        .map_or((0.0, 3), |spread| (spread, 4));
    let color = parts
        .get(color_index)
        .cloned()
        .unwrap_or_else(|| "rgba(0,0,0,0.3)".to_string());
    Some(ParsedBoxShadow {
        offset_x: ox,
        offset_y: oy,
        blur,
        spread,
        color,
    })
}

fn split_css_whitespace(value: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut start = None;
    let mut depth = 0_u32;
    for (index, ch) in value.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ => {}
        }
        if ch.is_whitespace() && depth == 0 {
            if let Some(from) = start.take() {
                parts.push(value[from..index].to_string());
            }
        } else if start.is_none() {
            start = Some(index);
        }
    }
    if let Some(from) = start {
        parts.push(value[from..].to_string());
    }
    parts
}

#[derive(Debug, Clone)]
pub struct ParsedAnimation {
    pub name: String,
    pub duration_ms: u32,
    pub easing: String,
    pub delay_ms: u32,
}

pub fn parse_animation(value: &str) -> Option<ParsedAnimation> {
    let parts: Vec<&str> = value.split_whitespace().collect();
    if parts.is_empty() {
        return None;
    }
    let name = parts[0].to_string();
    let mut duration_ms = 300u32;
    let mut easing = "ease".to_string();
    let mut delay_ms = 0u32;
    for part in parts.iter().skip(1) {
        if let Some(ms) = part.strip_suffix("ms") {
            if let Ok(v) = ms.parse() {
                duration_ms = v;
            }
        } else if let Some(sec) = part.strip_suffix('s') {
            if let Ok(v) = sec.parse::<f32>() {
                duration_ms = (v * 1000.0) as u32;
            }
        } else if matches!(
            *part,
            "ease" | "linear" | "ease-in" | "ease-out" | "ease-in-out"
        ) {
            easing = (*part).to_string();
        }
    }
    Some(ParsedAnimation {
        name,
        duration_ms,
        easing,
        delay_ms,
    })
}

fn extract_fn<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    let start = s.find(name)?;
    let rest = &s[start + name.len()..];
    let open = rest.find('(')?;
    let close = rest.find(')')?;
    Some(&rest[open + 1..close])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_shadow_supports_omitted_spread_and_spaced_rgba() {
        let shadow = parse_box_shadow("0 18px 56px rgba(28, 55, 90, 0.12)").expect("parsed shadow");
        assert_eq!(shadow.offset_x, 0.0);
        assert_eq!(shadow.offset_y, 18.0);
        assert_eq!(shadow.blur, 56.0);
        assert_eq!(shadow.spread, 0.0);
        assert_eq!(shadow.color, "rgba(28, 55, 90, 0.12)");
    }
}
