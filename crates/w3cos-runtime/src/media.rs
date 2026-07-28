use serde::{Deserialize, Serialize};

/// Viewport information for evaluating @media queries.
#[derive(Debug, Clone, Copy)]
pub struct Viewport {
    pub width: f32,
    pub height: f32,
    pub device_pixel_ratio: f32,
}

impl Viewport {
    pub fn new(width: f32, height: f32, dpr: f32) -> Self {
        Self {
            width,
            height,
            device_pixel_ratio: dpr,
        }
    }

    pub fn orientation(&self) -> Orientation {
        if self.width >= self.height {
            Orientation::Landscape
        } else {
            Orientation::Portrait
        }
    }

    /// Classify viewport into a size class (like SwiftUI/HarmonyOS breakpoints).
    pub fn size_class(&self) -> SizeClass {
        if self.width < 600.0 {
            SizeClass::Compact
        } else if self.width < 1024.0 {
            SizeClass::Medium
        } else {
            SizeClass::Expanded
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Orientation {
    Portrait,
    Landscape,
}

/// Breakpoint size classes (similar to HarmonyOS breakpoints / SwiftUI SizeClass).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SizeClass {
    Compact,  // < 600px (phone)
    Medium,   // 600-1024px (tablet)
    Expanded, // > 1024px (desktop)
}

/// A CSS @media query condition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MediaCondition {
    Always,
    Never,
    MinWidth(f32),
    MaxWidth(f32),
    MinHeight(f32),
    MaxHeight(f32),
    Orientation(Orientation),
    MinResolution(f32),
    PrefersColorScheme(ColorScheme),
    And(Vec<MediaCondition>),
    Or(Vec<MediaCondition>),
    Not(Box<MediaCondition>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColorScheme {
    Light,
    Dark,
}

/// A CSS @media rule: condition + associated style overrides.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaRule {
    pub condition: MediaCondition,
    pub styles: Vec<(String, Vec<(String, String)>)>,
}

/// A CSS container query condition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ContainerCondition {
    MinWidth(f32),
    MaxWidth(f32),
    MinHeight(f32),
    MaxHeight(f32),
    And(Vec<ContainerCondition>),
}

/// Evaluate a @media condition against the current viewport.
pub fn matches_media(condition: &MediaCondition, viewport: &Viewport) -> bool {
    match condition {
        MediaCondition::Always => true,
        MediaCondition::Never => false,
        MediaCondition::MinWidth(w) => viewport.width >= *w,
        MediaCondition::MaxWidth(w) => viewport.width <= *w,
        MediaCondition::MinHeight(h) => viewport.height >= *h,
        MediaCondition::MaxHeight(h) => viewport.height <= *h,
        MediaCondition::Orientation(o) => viewport.orientation() == *o,
        MediaCondition::MinResolution(dpr) => viewport.device_pixel_ratio >= *dpr,
        MediaCondition::PrefersColorScheme(_scheme) => {
            // Default to dark for W3C OS
            matches!(_scheme, ColorScheme::Dark)
        }
        MediaCondition::And(conditions) => conditions.iter().all(|c| matches_media(c, viewport)),
        MediaCondition::Or(conditions) => conditions.iter().any(|c| matches_media(c, viewport)),
        MediaCondition::Not(c) => !matches_media(c, viewport),
    }
}

/// Evaluate a container query against a container's actual size.
pub fn matches_container(condition: &ContainerCondition, width: f32, height: f32) -> bool {
    match condition {
        ContainerCondition::MinWidth(w) => width >= *w,
        ContainerCondition::MaxWidth(w) => width <= *w,
        ContainerCondition::MinHeight(h) => height >= *h,
        ContainerCondition::MaxHeight(h) => height <= *h,
        ContainerCondition::And(conditions) => conditions
            .iter()
            .all(|c| matches_container(c, width, height)),
    }
}

/// Parse a simple @media query string into a condition.
///
/// Supports:
///   (min-width: 600px)
///   (max-width: 1024px)
///   (orientation: portrait)
///   (min-width: 600px) and (max-width: 1024px)
pub fn parse_media_query(query: &str) -> Option<MediaCondition> {
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return Some(MediaCondition::Always);
    }
    parse_media_expression(&query)
}

fn parse_media_expression(query: &str) -> Option<MediaCondition> {
    if let Some(inner) = strip_outer_parentheses(query)
        && parse_single_condition(inner).is_none()
    {
        return parse_media_expression(inner);
    }
    let alternatives = split_media_top_level(query, ',')
        .into_iter()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(parse_media_alternative)
        .collect::<Option<Vec<_>>>()?;
    match alternatives.as_slice() {
        [] => None,
        [condition] => Some(condition.clone()),
        _ => Some(MediaCondition::Or(alternatives)),
    }
}

fn parse_media_alternative(query: &str) -> Option<MediaCondition> {
    let (negated, query) = query
        .strip_prefix("not ")
        .map(|query| (true, query.trim()))
        .unwrap_or((false, query));
    let query = query.strip_prefix("only ").unwrap_or(query).trim();
    let conditions = split_media_and(query)
        .into_iter()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(parse_media_term)
        .collect::<Option<Vec<_>>>()?;
    let condition = match conditions.as_slice() {
        [] => return None,
        [condition] => condition.clone(),
        _ => MediaCondition::And(conditions),
    };
    Some(if negated {
        MediaCondition::Not(Box::new(condition))
    } else {
        condition
    })
}

fn parse_media_term(term: &str) -> Option<MediaCondition> {
    match term {
        "all" | "screen" => return Some(MediaCondition::Always),
        "print" | "speech" => return Some(MediaCondition::Never),
        _ => {}
    }
    if let Some(clean) = strip_outer_parentheses(term) {
        return parse_single_condition(clean).or_else(|| parse_media_expression(clean));
    }
    parse_single_condition(term.trim())
}

fn strip_outer_parentheses(query: &str) -> Option<&str> {
    let query = query.trim();
    if !query.starts_with('(') || !query.ends_with(')') {
        return None;
    }
    let mut depth = 0_i32;
    for (index, byte) in query.bytes().enumerate() {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 && index + 1 != query.len() {
                    return None;
                }
            }
            _ => {}
        }
        if depth < 0 {
            return None;
        }
    }
    (depth == 0).then(|| query[1..query.len() - 1].trim())
}

fn split_media_top_level(query: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0_i32;
    let mut start = 0;
    for (index, character) in query.char_indices() {
        match character {
            '(' => depth += 1,
            ')' => depth = (depth - 1).max(0),
            _ if character == separator && depth == 0 => {
                parts.push(&query[start..index]);
                start = index + character.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&query[start..]);
    parts
}

fn split_media_and(query: &str) -> Vec<&str> {
    let bytes = query.as_bytes();
    let mut parts = Vec::new();
    let mut depth = 0_i32;
    let mut start = 0;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'(' => depth += 1,
            b')' => depth = (depth - 1).max(0),
            _ => {}
        }
        if depth == 0 && bytes[index..].starts_with(b" and ") {
            parts.push(&query[start..index]);
            index += " and ".len();
            start = index;
            continue;
        }
        index += 1;
    }
    parts.push(&query[start..]);
    parts
}

fn parse_single_condition(s: &str) -> Option<MediaCondition> {
    let parts: Vec<&str> = s.splitn(2, ':').collect();
    if parts.len() != 2 {
        return None;
    }
    let prop = parts[0].trim();
    let val = parts[1].trim();

    match prop {
        "min-width" => parse_px(val).map(MediaCondition::MinWidth),
        "max-width" => parse_px(val).map(MediaCondition::MaxWidth),
        "min-height" => parse_px(val).map(MediaCondition::MinHeight),
        "max-height" => parse_px(val).map(MediaCondition::MaxHeight),
        "orientation" => match val {
            "portrait" => Some(MediaCondition::Orientation(Orientation::Portrait)),
            "landscape" => Some(MediaCondition::Orientation(Orientation::Landscape)),
            _ => None,
        },
        "min-resolution" => val
            .strip_suffix("dppx")
            .or_else(|| val.strip_suffix("x"))
            .and_then(|n| n.trim().parse::<f32>().ok())
            .map(MediaCondition::MinResolution),
        "prefers-color-scheme" => match val {
            "dark" => Some(MediaCondition::PrefersColorScheme(ColorScheme::Dark)),
            "light" => Some(MediaCondition::PrefersColorScheme(ColorScheme::Light)),
            _ => None,
        },
        _ => None,
    }
}

fn parse_px(val: &str) -> Option<f32> {
    val.strip_suffix("px")
        .and_then(|n| n.trim().parse::<f32>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desktop() -> Viewport {
        Viewport::new(1920.0, 1080.0, 1.0)
    }
    fn tablet() -> Viewport {
        Viewport::new(768.0, 1024.0, 2.0)
    }
    fn phone() -> Viewport {
        Viewport::new(375.0, 812.0, 3.0)
    }

    #[test]
    fn size_class_breakpoints() {
        assert_eq!(phone().size_class(), SizeClass::Compact);
        assert_eq!(tablet().size_class(), SizeClass::Medium);
        assert_eq!(desktop().size_class(), SizeClass::Expanded);
    }

    #[test]
    fn orientation_detection() {
        assert_eq!(desktop().orientation(), Orientation::Landscape);
        assert_eq!(phone().orientation(), Orientation::Portrait);
    }

    #[test]
    fn min_width_query() {
        let cond = MediaCondition::MinWidth(600.0);
        assert!(matches_media(&cond, &desktop()));
        assert!(matches_media(&cond, &tablet()));
        assert!(!matches_media(&cond, &phone()));
    }

    #[test]
    fn max_width_query() {
        let cond = MediaCondition::MaxWidth(600.0);
        assert!(!matches_media(&cond, &desktop()));
        assert!(!matches_media(&cond, &tablet()));
        assert!(matches_media(&cond, &phone()));
    }

    #[test]
    fn orientation_query() {
        let portrait = MediaCondition::Orientation(Orientation::Portrait);
        assert!(!matches_media(&portrait, &desktop()));
        assert!(matches_media(&portrait, &phone()));
    }

    #[test]
    fn and_query() {
        let cond = MediaCondition::And(vec![
            MediaCondition::MinWidth(600.0),
            MediaCondition::MaxWidth(1024.0),
        ]);
        assert!(!matches_media(&cond, &desktop())); // 1920 > 1024
        assert!(matches_media(&cond, &tablet())); // 768 in range
        assert!(!matches_media(&cond, &phone())); // 375 < 600
    }

    #[test]
    fn not_query() {
        let cond = MediaCondition::Not(Box::new(MediaCondition::MinWidth(600.0)));
        assert!(!matches_media(&cond, &desktop()));
        assert!(matches_media(&cond, &phone()));
    }

    #[test]
    fn resolution_query() {
        let cond = MediaCondition::MinResolution(2.0);
        assert!(!matches_media(&cond, &desktop())); // 1x
        assert!(matches_media(&cond, &tablet())); // 2x
        assert!(matches_media(&cond, &phone())); // 3x
    }

    #[test]
    fn parse_simple_min_width() {
        let cond = parse_media_query("(min-width: 600px)").unwrap();
        assert!(matches_media(&cond, &desktop()));
        assert!(!matches_media(&cond, &phone()));
    }

    #[test]
    fn parse_and_query() {
        let cond = parse_media_query("(min-width: 600px) and (max-width: 1024px)").unwrap();
        assert!(matches_media(&cond, &tablet()));
        assert!(!matches_media(&cond, &desktop()));
    }

    #[test]
    fn parse_orientation() {
        let cond = parse_media_query("(orientation: portrait)").unwrap();
        assert!(matches_media(&cond, &phone()));
        assert!(!matches_media(&cond, &desktop()));
    }

    #[test]
    fn parse_color_scheme() {
        let cond = parse_media_query("(prefers-color-scheme: dark)").unwrap();
        assert!(matches_media(&cond, &desktop()));
    }

    #[test]
    fn parses_media_types_query_lists_and_not() {
        let responsive =
            parse_media_query("screen and (min-width: 800px), (orientation: portrait)").unwrap();
        assert!(matches_media(&responsive, &desktop()));
        assert!(matches_media(&responsive, &phone()));
        assert!(matches_media(&responsive, &tablet()));
        assert!(!matches_media(
            &responsive,
            &Viewport::new(700.0, 500.0, 1.0)
        ));

        assert!(matches_media(
            &parse_media_query("not print").unwrap(),
            &desktop()
        ));
        assert!(!matches_media(
            &parse_media_query("print").unwrap(),
            &desktop()
        ));
        assert!(matches_media(
            &parse_media_query("only screen").unwrap(),
            &desktop()
        ));
    }

    #[test]
    fn container_query_basic() {
        let cond = ContainerCondition::MinWidth(300.0);
        assert!(matches_container(&cond, 400.0, 200.0));
        assert!(!matches_container(&cond, 200.0, 200.0));
    }

    #[test]
    fn container_query_and() {
        let cond = ContainerCondition::And(vec![
            ContainerCondition::MinWidth(300.0),
            ContainerCondition::MaxWidth(800.0),
        ]);
        assert!(matches_container(&cond, 500.0, 400.0));
        assert!(!matches_container(&cond, 900.0, 400.0));
        assert!(!matches_container(&cond, 200.0, 400.0));
    }
}
