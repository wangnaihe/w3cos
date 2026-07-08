//! UA compositor layer promotion — mirrors browser heuristics for CSS-driven GPU layers.
//!
//! Applications express intent via standard CSS (`transform`, `opacity`, `will-change`,
//! `filter`, `contain: paint`, animations). The runtime decides when to promote layers.

use w3cos_std::style::{Contain, Style, Transform2D, WillChange};

const OPACITY_EPSILON: f32 = 0.999;

/// Whether this element should be drawn inside a dedicated compositor layer (GPU path).
pub fn promotes_compositor_layer(style: &Style) -> bool {
    style.opacity < OPACITY_EPSILON
        || !style.transform.is_identity()
        || style.will_change.promotes_layer()
        || style.filter.as_ref().is_some_and(|f| crate::filter::filter_promotes_layer(f))
        || style.contain.has_paint_containment()
        || style.animation.is_some()
}

pub fn layer_opacity(style: &Style) -> f32 {
    if style.opacity < OPACITY_EPSILON {
        style.opacity
    } else {
        1.0
    }
}

pub fn lerp_transform(a: Transform2D, b: Transform2D, t: f32) -> Transform2D {
    let t = t.clamp(0.0, 1.0);
    Transform2D {
        translate_x: a.translate_x + (b.translate_x - a.translate_x) * t,
        translate_y: a.translate_y + (b.translate_y - a.translate_y) * t,
        scale_x: a.scale_x + (b.scale_x - a.scale_x) * t,
        scale_y: a.scale_y + (b.scale_y - a.scale_y) * t,
        rotate_deg: a.rotate_deg + (b.rotate_deg - a.rotate_deg) * t,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use w3cos_std::style::WillChange;

    #[test]
    fn will_change_promotes() {
        let mut style = Style::default();
        style.will_change = WillChange::from_css("transform, opacity");
        assert!(promotes_compositor_layer(&style));
    }

    #[test]
    fn transform_promotes() {
        let mut style = Style::default();
        style.transform.translate_y = 8.0;
        assert!(promotes_compositor_layer(&style));
    }

    #[test]
    fn filter_none_does_not_promote() {
        let mut style = Style::default();
        style.filter = Some("none".into());
        assert!(!promotes_compositor_layer(&style));
    }
}
