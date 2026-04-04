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
    Compact,   // < 600px (phone)
    Medium,    // 600-1024px (tablet)
    Expanded,  // > 1024px (desktop)
}

/// A CSS @media query condition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MediaCondition {
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
        ContainerCondition::And(conditions) => {
            conditions.iter().all(|c| matches_container(c, width, height))
        }
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
    let query = query.trim();

    if query.contains(") and (") {
        let parts: Vec<&str> = query.split(") and (").collect();
        let conditions: Vec<MediaCondition> = parts
            .iter()
            .filter_map(|p| {
                let clean = p.trim_matches(|c| c == '(' || c == ')');
                parse_single_condition(clean)
            })
            .collect();
        if conditions.is_empty() {
            return None;
        }
        return Some(MediaCondition::And(conditions));
    }

    let clean = query.trim_matches(|c: char| c == '(' || c == ')');
    parse_single_condition(clean)
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
        "min-resolution" => {
            val.strip_suffix("dppx")
                .or_else(|| val.strip_suffix("x"))
                .and_then(|n| n.trim().parse::<f32>().ok())
                .map(MediaCondition::MinResolution)
        }
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
        assert!(matches_media(&cond, &tablet()));    // 768 in range
        assert!(!matches_media(&cond, &phone()));    // 375 < 600
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
        assert!(matches_media(&cond, &tablet()));    // 2x
        assert!(matches_media(&cond, &phone()));     // 3x
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
