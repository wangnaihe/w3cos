//! Lightweight @media query parsing/evaluation for compile-time style resolution.

#[derive(Debug, Clone)]
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

    fn orientation(&self) -> Orientation {
        if self.width >= self.height {
            Orientation::Landscape
        } else {
            Orientation::Portrait
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Portrait,
    Landscape,
}

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorScheme {
    Light,
    Dark,
}

impl MediaCondition {
    pub fn matches(&self, vp: &Viewport) -> bool {
        match self {
            MediaCondition::MinWidth(w) => vp.width >= *w,
            MediaCondition::MaxWidth(w) => vp.width <= *w,
            MediaCondition::MinHeight(h) => vp.height >= *h,
            MediaCondition::MaxHeight(h) => vp.height <= *h,
            MediaCondition::Orientation(o) => vp.orientation() == *o,
            MediaCondition::MinResolution(r) => vp.device_pixel_ratio >= *r,
            MediaCondition::PrefersColorScheme(_) => true,
            MediaCondition::And(conds) => conds.iter().all(|c| c.matches(vp)),
            MediaCondition::Or(conds) => conds.iter().any(|c| c.matches(vp)),
            MediaCondition::Not(inner) => !inner.matches(vp),
        }
    }
}

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
        "min-resolution" => val
            .strip_suffix("dppx")
            .or_else(|| val.strip_suffix('x'))
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

    #[test]
    fn min_width_matches() {
        let cond = parse_media_query("(min-width: 600px)").unwrap();
        assert!(!cond.matches(&Viewport::new(402.0, 874.0, 1.0)));
        assert!(cond.matches(&Viewport::new(800.0, 600.0, 1.0)));
    }
}
