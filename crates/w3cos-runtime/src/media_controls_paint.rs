//! Compact, no-source video UA controls. Geometry is host-relative in CSS px.
//! Loaded media state and controls interaction remain with the media runtime.
use super::*;

thread_local! {
    // Graphite's half-precision disabled-control opacity is the nearest
    // binary16 value of authored 0.3. CPU SkSL treats half as float, so retain
    // that value explicitly during composition instead of RGBA8 multiplication.
    static DISABLED_BLEND: Option<skia_safe::Blender> = skia_safe::RuntimeEffect::make_for_blender(
        "half4 main(half4 src, half4 dst) { float4 s = float4(src) * 0.300048828125; return half4(s + float4(dst) * (1.0 - s.a)); }", None,
    ).ok().and_then(|effect| effect.make_blender(Data::new_empty(),None));
    // Keep the A8 blur mask independent of color opacity, then round the
    // final RGBA8 shadow composition. A mask byte 51 over gray5 at alpha0.5
    // produces exactly4.5; intermediate paint alpha / mask lerp loses the tie.
    static SHADOW_BLEND: Option<skia_safe::Blender> = skia_safe::RuntimeEffect::make_for_blender(
        "half4 main(half4 src, half4 dst) { float a = float(src.a) * 0.5; float3 rgb = floor(float3(dst.rgb) * 255.0 * (1.0 - a) + 0.5) / 255.0; return half4(rgb, a + float(dst.a) * (1.0 - a)); }", None,
    ).ok().and_then(|effect| effect.make_blender(Data::new_empty(),None));
}

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
    let mut layer = color_paint(w3cos_std::Color::BLACK, 1.0);
    DISABLED_BLEND.with(|blender| {
        if let Some(blender) = blender { layer.set_blender(blender.clone()); }
        else { layer.set_alpha_f(0.3); }
    });
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
    SHADOW_BLEND.with(|blender| {
        if let Some(blender) = blender {
            let mut composite = Paint::default();
            composite.set_blender(blender.clone());
            canvas.save_layer(&SaveLayerRec::default().paint(&composite));
            let mut mask = color_paint(w3cos_std::Color::WHITE,1.0);
            mask.set_mask_filter(MaskFilter::blur(BlurStyle::Normal,5.0,false));
            canvas.draw_rrect(track,&mask);
        } else {
            let mut shadow = color_paint(w3cos_std::Color::BLACK,0.5);
            shadow.set_mask_filter(MaskFilter::blur(BlurStyle::Normal,5.0,false));
            canvas.draw_rrect(track,&shadow);
        }
    });
    canvas.restore_to_count(shadow_save);
    let fill = color_paint(w3cos_std::Color::WHITE, 77.0 / 255.0);
    if !analytic_round_rect::draw(canvas, track_rect, 2.0, &fill) { canvas.draw_rrect(track, &fill); }
    canvas.restore_to_count(save);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_shadow_mask_preserves_coverage_before_opacity() {
        let info = ImageInfo::new((120,120), ColorType::RGBA8888, AlphaType::Premul, None);
        {
            let mut surface = skia_safe::surfaces::raster_n32_premul((120,120)).unwrap();
            let canvas = surface.canvas();
            canvas.clear(Color::TRANSPARENT);
            let track = RRect::new_rect_xy(Rect::from_xywh(24.0,84.0,63.0,4.0),2.0,2.0);
            canvas.clip_rrect(track,skia_safe::ClipOp::Difference,Some(true));
            canvas.translate((0.0,2.0));
            let mut paint = color_paint(w3cos_std::Color::WHITE,1.0);
            paint.set_mask_filter(MaskFilter::blur(BlurStyle::Normal,5.0,false));
            canvas.draw_rrect(track,&paint);
            let mut pixels=vec![0;120*120*4];
            assert!(surface.read_pixels(&info,&mut pixels,120*4,(0,0)));
            for (x,y,coverage) in [(30,83,51),(80,83,51),(50,83,56),(30,88,75)] {
                assert_eq!(&pixels[(y*120+x)*4..(y*120+x)*4+4],&[coverage;4]);
            }
        }
        // V2448 isolates native mask coverage, not a browser mask oracle.
    }

    #[test]
    fn video_timeline_capsule_matches_browser_end_coverage() {
        let mut surface = skia_safe::surfaces::raster_n32_premul((120,120)).unwrap();
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
        // Original browser V2430, both ends of the 4px UA timeline capsule.
        for (x,y,gray) in [(24,84,34),(25,84,79),(85,84,79),(86,84,34),
            (24,85,78),(86,85,78),(24,87,33),(25,87,78),(85,87,78),(86,87,33)] {
            assert_eq!(&pixels[(y*120+x)*4..(y*120+x)*4+4], &[gray,gray,gray,255], "{x},{y}");
        }
    }

    #[test]
    fn disabled_video_overflow_preserves_authored_opacity() {
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
        // Original Chromium capture V2422; disabled UA opacity is 0.3,
        // not the rounded computed-style byte 77/255.
        for (x,y,gray) in [(78,59,91),(79,59,91),(78,54,93),(79,54,93),(78,58,61),(80,59,61),(78,66,57)] {
            assert_eq!(&pixels[(y*120+x)*4..(y*120+x)*4+4], &[gray,gray,gray,255], "{x},{y}");
        }
    }

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
        for (x,y,gray) in [(30,83,5),(80,83,5),(30,88,3),(50,88,3),(30,89,3),(50,89,3)] {
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
