use crate::history::History;
use crate::location::Location;

pub struct MediaQueryResult {
    pub matches: bool,
    pub media: String,
}

/// W3C Window API — global scope for the application.
pub struct Window {
    pub inner_width: f32,
    pub inner_height: f32,
    pub device_pixel_ratio: f32,
    pub history: History,
    pub location: Location,
    pub prefers_dark: bool,
    animation_frame_callbacks: Vec<Box<dyn FnOnce(f64)>>,
}

impl Window {
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            inner_width: width,
            inner_height: height,
            device_pixel_ratio: 1.0,
            history: History::new(),
            location: Location::new("/"),
            prefers_dark: false,
            animation_frame_callbacks: Vec::new(),
        }
    }

    pub fn request_animation_frame(&mut self, callback: Box<dyn FnOnce(f64)>) -> u32 {
        self.animation_frame_callbacks.push(callback);
        self.animation_frame_callbacks.len() as u32
    }

    pub fn flush_animation_frames(&mut self, timestamp: f64) {
        let callbacks = std::mem::take(&mut self.animation_frame_callbacks);
        for cb in callbacks {
            cb(timestamp);
        }
    }

    pub fn resize(&mut self, width: f32, height: f32) {
        self.inner_width = width;
        self.inner_height = height;
    }

    pub fn match_media(&self, query: &str) -> MediaQueryResult {
        let query_trimmed = query.trim();
        let matches = Self::eval_media_query(query_trimmed, self.inner_width, self.prefers_dark);
        MediaQueryResult {
            matches,
            media: query_trimmed.to_string(),
        }
    }

    fn eval_media_query(query: &str, width: f32, prefers_dark: bool) -> bool {
        let inner = query
            .strip_prefix('(')
            .and_then(|s| s.strip_suffix(')'))
            .unwrap_or(query)
            .trim();

        if let Some(rest) = inner.strip_prefix("min-width:") {
            if let Some(px) = Self::parse_px(rest) {
                return width >= px;
            }
        }
        if let Some(rest) = inner.strip_prefix("max-width:") {
            if let Some(px) = Self::parse_px(rest) {
                return width <= px;
            }
        }
        if let Some(rest) = inner.strip_prefix("prefers-color-scheme:") {
            let scheme = rest.trim();
            return match scheme {
                "dark" => prefers_dark,
                "light" => !prefers_dark,
                _ => false,
            };
        }
        false
    }

    fn parse_px(s: &str) -> Option<f32> {
        let s = s.trim();
        s.strip_suffix("px")
            .and_then(|n| n.trim().parse::<f32>().ok())
    }
}

impl Default for Window {
    fn default() -> Self {
        Self::new(960.0, 640.0)
    }
}
