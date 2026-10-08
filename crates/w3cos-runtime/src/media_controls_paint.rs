//! Compact, no-source video UA controls. Geometry is host-relative in CSS px.
//! Loaded media state and controls interaction remain with the media runtime.
use super::*;

pub(super) fn draw(canvas: &Canvas, rect: LayoutRect, style: &Style) {
    let w3cos_std::style::Dimension::Px(width) = style.width else { return; };
    let w3cos_std::style::Dimension::Px(height) = style.height else { return; };
    if width <= 0.0 || height <= 0.0 { return; }
    let save = canvas.save();
    canvas.translate((rect.x, rect.y));
    canvas.scale((rect.width / width, rect.height / height));
    canvas.clip_rect(Rect::from_wh(width, height), None, Some(true));

    // Authored UA gradient stops, bottom-aligned at 112 CSS px. Applying the
    // device scale to the canvas keeps every control token in the same space.
    // Computed-style serialization quantizes alpha to bytes; the actual
    // browser gradient retains the authored floating-point color stops.
    let alphas = [0.0, 0.013, 0.049, 0.104, 0.175, 0.259, 0.352, 0.45,
        0.55, 0.648, 0.741, 0.825, 0.896, 0.951, 0.987, 1.0];
    let colors = alphas.map(|alpha| Color4f::new(0.0, 0.0, 0.0, alpha));
    let positions = [0.0, 0.081, 0.155, 0.225, 0.29, 0.353, 0.412, 0.471,
        0.529, 0.588, 0.647, 0.71, 0.775, 0.845, 0.919, 1.0];
    if let Some(shader) = gradient_shader::linear(
        ((0.0, height - 112.0), (0.0, height)), colors.as_slice(), positions.as_slice(), TileMode::Clamp, None, None,
    ) {
        let mut gradient = Paint::default();
        let calibrated = browser_dither::shader(canvas, shader.clone(), Color4f::new(0.2, 0.2, 0.2, 1.0));
        gradient.set_shader(calibrated.clone().unwrap_or(shader)).set_anti_alias(true).set_dither(calibrated.is_none());
        canvas.draw_rect(Rect::from_wh(width, height), &gradient);
    }

    // The disabled overflow button is 48px; its 24px SVG is centered at 20px.
    let button = Rect::from_xywh(width - 48.0, height - 72.0, 48.0, 48.0);
    let button_save = canvas.save();
    canvas.clip_rect(button, None, Some(true));
    let layer = color_paint(w3cos_std::Color::BLACK, 77.0 / 255.0);
    canvas.save_layer(&SaveLayerRec::default().bounds(&button).paint(&layer));
    canvas.translate((width - 34.0, height - 58.0));
    canvas.scale((20.0 / 24.0, 20.0 / 24.0));
    let white = color_paint(w3cos_std::Color::WHITE, 1.0);
    for top in [4.0, 10.0, 16.0] {
        let oval = RRect::new_oval(Rect::from_xywh(10.0, top, 4.0, 4.0));
        if !analytic_ellipse::draw(canvas, &oval, &white) { canvas.draw_oval(oval.rect(), &white); }
    }
    canvas.restore_to_count(button_save);

    let track_rect = Rect::from_xywh(16.0, height - 24.0, (width - 32.0).max(0.0), 4.0);
    let track = RRect::new_rect_xy(track_rect, 2.0, 2.0);
    let shadow_save = canvas.save();
    canvas.clip_rrect(track, skia_safe::ClipOp::Difference, Some(true));
    canvas.translate((0.0, 2.0));
    // UA track box-shadow: rgba(0,0,0,0.5) 0px 2px 10px. Command logs
    // serialize its color but omit the mask filter and computed opacity.
    let mut shadow = color_paint(w3cos_std::Color::BLACK, 0.5);
    shadow.set_mask_filter(MaskFilter::blur(BlurStyle::Normal, 5.0, false));
    canvas.draw_rrect(track, &shadow);
    canvas.restore_to_count(shadow_save);
    let fill = color_paint(w3cos_std::Color::WHITE, 77.0 / 255.0);
    if !analytic_round_rect::draw(canvas, track_rect, 2.0, &fill) { canvas.draw_rrect(track, &fill); }
    canvas.restore_to_count(save);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_track_shadow_preserves_original_browser_background() {
        let mut surface = skia_safe::surfaces::raster_n32_premul((120, 120)).unwrap();
        let canvas = surface.canvas();
        canvas.clear(Color::WHITE);
        let rect = LayoutRect { x: 8.0, y: 13.0, width: 95.0, height: 95.0 };
        let style = Style { width: w3cos_std::style::Dimension::Px(95.0),
            height: w3cos_std::style::Dimension::Px(95.0), ..Style::default() };
        canvas.draw_rect(to_rect(rect), &color_paint(w3cos_std::Color::rgb(51,51,51), 1.0));
        draw(canvas, rect, &style);
        let info = ImageInfo::new((120,120), ColorType::RGBA8888, AlphaType::Premul, None);
        let mut pixels = vec![0u8; 120*120*4];
        assert!(surface.read_pixels(&info, &mut pixels, 120*4, (0,0)));
        // Bound original-browser samples from V2411, beneath the track.
        for (x,y,gray) in [(30,88,3),(50,88,3),(30,89,3),(50,89,3)] {
            assert_eq!(&pixels[(y*120+x)*4..(y*120+x)*4+4], &[gray,gray,gray,255], "{x},{y}");
        }
    }

    #[test]
    fn video_gradient_matches_original_browser_dither_samples() {
        let mut surface = skia_safe::surfaces::raster_n32_premul((120, 120)).unwrap();
        let canvas = surface.canvas();
        canvas.clear(Color::WHITE);
        let rect = LayoutRect { x: 8.0, y: 13.0, width: 95.0, height: 95.0 };
        let style = Style { width: w3cos_std::style::Dimension::Px(95.0),
            height: w3cos_std::style::Dimension::Px(95.0), ..Style::default() };
        canvas.draw_rect(to_rect(rect), &color_paint(w3cos_std::Color::rgb(51, 51, 51), 1.0));
        draw(canvas, rect, &style);
        let info = ImageInfo::new((120, 120), ColorType::RGBA8888, AlphaType::Premul, None);
        let mut pixels = vec![0u8; 120 * 120 * 4];
        assert!(surface.read_pixels(&info, &mut pixels, 120 * 4, (0, 0)));
        // V2401 binds these samples to unchanged original browser captures.
        for (x, y, gray) in [(8,13,48),(9,13,49),(8,14,48),(9,14,48),
            (8,16,47),(9,16,47),(8,19,46),(9,19,47)] {
            assert_eq!(&pixels[(y * 120 + x) * 4..(y * 120 + x) * 4 + 4], &[gray,gray,gray,255], "{x},{y}");
        }
    }
}
