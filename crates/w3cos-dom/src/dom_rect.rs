/// W3C `DOMRect` — axis-aligned bounding rectangle.
/// https://www.w3.org/TR/geometry-1/#domrect
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DOMRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl DOMRect {
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn zero() -> Self {
        Self::default()
    }

    /// `DOMRect.top` — same as `y` for standard (non-inverted) rects.
    pub fn top(&self) -> f32 {
        self.y
    }

    /// `DOMRect.left` — same as `x`.
    pub fn left(&self) -> f32 {
        self.x
    }

    /// `DOMRect.bottom` — `y + height`.
    pub fn bottom(&self) -> f32 {
        self.y + self.height
    }

    /// `DOMRect.right` — `x + width`.
    pub fn right(&self) -> f32 {
        self.x + self.width
    }

    /// Returns true if the rect has non-zero area.
    pub fn is_empty(&self) -> bool {
        self.width <= 0.0 || self.height <= 0.0
    }

    /// Returns true if the point (px, py) is inside this rect.
    pub fn contains_point(&self, px: f32, py: f32) -> bool {
        px >= self.left() && px < self.right() && py >= self.top() && py < self.bottom()
    }
}

impl From<(f32, f32, f32, f32)> for DOMRect {
    fn from((x, y, w, h): (f32, f32, f32, f32)) -> Self {
        Self::new(x, y, w, h)
    }
}

impl From<DOMRect> for (f32, f32, f32, f32) {
    fn from(r: DOMRect) -> Self {
        (r.x, r.y, r.width, r.height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dom_rect_edges() {
        let r = DOMRect::new(10.0, 20.0, 100.0, 50.0);
        assert_eq!(r.top(), 20.0);
        assert_eq!(r.left(), 10.0);
        assert_eq!(r.bottom(), 70.0);
        assert_eq!(r.right(), 110.0);
    }

    #[test]
    fn dom_rect_contains_point() {
        let r = DOMRect::new(0.0, 0.0, 100.0, 100.0);
        assert!(r.contains_point(50.0, 50.0));
        assert!(!r.contains_point(150.0, 50.0));
    }
}
