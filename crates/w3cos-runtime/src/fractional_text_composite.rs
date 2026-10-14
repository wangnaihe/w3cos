//! Fractional CSS foregrounds with coverage applied before SrcOver.
//! Keep glyph-mask luminance calibration independent of framebuffer quantization.
use skia_safe::RoundOut;
use skia_safe::{
    Canvas, Color, Data, ImageInfo, Matrix, Paint, Rect, RuntimeEffect, SamplingOptions, TileMode,
};
use w3cos_std::Style;

thread_local! {
    static MASK_EFFECT: Option<RuntimeEffect> = RuntimeEffect::make_for_shader(
        "uniform shader mask; uniform float4 color; uniform float divisor; half4 main(float2 p) { float coverage = floor(mask.eval(p).a * 255 + .5) / divisor; return half4(color.rgb * color.a * coverage, color.a * coverage); }",None).ok();
}

pub(super) fn draw(
    canvas: &Canvas,
    style: &Style,
    paint: &Paint,
    ink: Option<Rect>,
    draw_glyphs: impl Fn(&Canvas, &Paint),
) {
    let Some([r, g, b, a]) = style.color_rgba else {
        draw_glyphs(canvas, paint);
        return;
    };
    let color = paint.color4f();
    let channels = [r, g, b, a];
    let bytes = [style.color.r, style.color.g, style.color.b, style.color.a];
    let valid = channels.iter().zip(bytes).all(|(channel, byte)| {
        channel.is_finite()
            && (0.0..=1.0).contains(channel)
            && (*channel * 255.0).round() as u8 == byte
    });
    let fractional = [r, g, b]
        .into_iter()
        .zip(bytes)
        .any(|(channel, byte)| channel != f32::from(byte) / 255.0);
    if !valid
        || !fractional
        || [color.r, color.g, color.b] != [r, g, b]
        || paint.shader().is_some()
        || paint.color_filter().is_some()
        || paint.mask_filter().is_some()
    {
        draw_glyphs(canvas, paint);
        return;
    }
    let Some(clip) = canvas.device_clip_bounds() else {
        return;
    };
    let matrix = canvas.local_to_device_as_3x3();
    if matrix.has_perspective() {
        draw_glyphs(canvas, paint);
        return;
    }
    let mut bounds = Rect::from(clip);
    if let Some(ink) = ink {
        let mut device_ink = matrix.map_rect(ink).0;
        device_ink.outset((1.0, 1.0));
        if !bounds.intersect(device_ink) {
            return;
        }
    }
    let bounds: skia_safe::IRect = bounds.round_out();
    let info = ImageInfo::new(
        (bounds.width(), bounds.height()),
        skia_safe::ColorType::Alpha8,
        skia_safe::AlphaType::Premul,
        None,
    );
    let Some(mut mask) = skia_safe::surfaces::raster(&info, None, None) else {
        draw_glyphs(canvas, paint);
        return;
    };
    mask.canvas().clear(Color::TRANSPARENT);
    mask.canvas()
        .translate((-(bounds.left as f32), -(bounds.top as f32)));
    mask.canvas().concat(&matrix);
    let mut mask_paint = paint.clone();
    mask_paint.set_alpha_f(1.0);
    draw_glyphs(mask.canvas(), &mask_paint);
    let image = mask.image_snapshot();
    let placement = Matrix::translate((bounds.left as f32, bounds.top as f32));
    let Some(mask_shader) = image.to_shader(
        (TileMode::Decal, TileMode::Decal),
        SamplingOptions::default(),
        Some(&placement),
    ) else {
        draw_glyphs(canvas, paint);
        return;
    };
    let uniforms = [color.r, color.g, color.b, color.a, 255.0]
        .into_iter()
        .flat_map(f32::to_ne_bytes)
        .collect::<Vec<_>>();
    let shader = MASK_EFFECT.with(|effect| {
        effect.as_ref().and_then(|effect| {
            effect.make_shader(Data::new_copy(&uniforms), &[mask_shader.into()], None)
        })
    });
    let Some(shader) = shader else {
        draw_glyphs(canvas, paint);
        return;
    };
    let mut composite = Paint::default();
    composite.set_shader(shader);
    let blender = super::CSS_OPACITY_SRC_OVER.with(Clone::clone);
    let Some(blender) = blender else {
        draw_glyphs(canvas, paint);
        return;
    };
    composite.set_blender(blender);
    let save = canvas.save();
    canvas.reset_matrix();
    canvas.draw_rect(Rect::from(bounds), &composite);
    canvas.restore_to_count(save);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style() -> Style {
        Style {
            color: w3cos_std::Color::rgb(77, 128, 179),
            color_rgba: Some([0.3, 0.5, 0.7, 1.0]),
            ..Style::default()
        }
    }

    fn pixels(surface: &mut skia_safe::Surface) -> Vec<u8> {
        let info = ImageInfo::new(
            (24, 24),
            skia_safe::ColorType::RGBA8888,
            skia_safe::AlphaType::Premul,
            None,
        );
        let mut pixels = vec![0; 24 * 24 * 4];
        assert!(surface.read_pixels(&info, &mut pixels, 24 * 4, (0, 0)));
        pixels
    }

    #[test]
    fn fractional_mask_composes_color_and_alpha_over_existing_backgrounds() {
        for background in [
            Color::WHITE,
            Color::from_rgb(21, 43, 97),
            Color::TRANSPARENT,
        ] {
            for alpha in [0.0_f32, 0.25, 0.5, 1.0] {
                let mut surface = skia_safe::surfaces::raster_n32_premul((24, 24)).unwrap();
                surface.canvas().clear(background);
                let before = pixels(&mut surface);
                let style = style();
                let paint = super::super::text_color_paint(&style, alpha);
                let rect = Rect::from_xywh(3.0, 4.0, 8.0, 8.0);
                draw(
                    surface.canvas(),
                    &style,
                    &paint,
                    Some(rect),
                    |canvas, paint| {
                        canvas.draw_rect(rect, paint);
                    },
                );
                let after = pixels(&mut surface);
                let center = (6 * 24 + 6) * 4;
                for (channel, source) in [0.3_f32, 0.5, 0.7, 1.0].into_iter().enumerate() {
                    let destination = f32::from(before[center + channel]) / 255.0;
                    let scaled = source * alpha;
                    let remaining = destination * (1.0 - alpha);
                    let expected = (f64::from(scaled + remaining) * 255.0).round() as u8;
                    assert_eq!(
                        after[center + channel],
                        expected,
                        "{background:?}/{alpha}/channel{channel}"
                    );
                }
                assert_eq!(
                    &after[..4],
                    &before[..4],
                    "outside glyph ink remains untouched"
                );
            }
        }
    }

    #[test]
    fn fractional_mask_keeps_transform_clip_and_canvas_state() {
        let mut surface = skia_safe::surfaces::raster_n32_premul((24, 24)).unwrap();
        let canvas = surface.canvas();
        canvas.clear(Color::WHITE);
        canvas.translate((3.0, 4.0));
        canvas.scale((2.0, 2.0));
        canvas.clip_rect(Rect::from_xywh(0.0, 0.0, 4.0, 4.0), None, Some(false));
        let matrix = canvas.local_to_device_as_3x3();
        let clip = canvas.device_clip_bounds();
        let style = style();
        let paint = super::super::text_color_paint(&style, 1.0);
        let rect = Rect::from_xywh(0.0, 0.0, 8.0, 8.0);
        draw(canvas, &style, &paint, Some(rect), |canvas, paint| {
            canvas.draw_rect(rect, paint);
        });
        assert_eq!(canvas.local_to_device_as_3x3(), matrix);
        assert_eq!(canvas.device_clip_bounds(), clip);
        let pixels = pixels(&mut surface);
        let at = |x: usize, y: usize| &pixels[(y * 24 + x) * 4..(y * 24 + x) * 4 + 4];
        let foreground =
            [0.3_f32, 0.5, 0.7, 1.0].map(|channel| (f64::from(channel) * 255.0).round() as u8);
        assert_eq!(at(4, 5), foreground);
        assert_eq!(at(11, 5), [255, 255, 255, 255]);
        assert_eq!(at(4, 12), [255, 255, 255, 255]);
    }

    #[test]
    fn byte_exact_and_stale_foregrounds_keep_original_paint_path() {
        let mut surface = skia_safe::surfaces::raster_n32_premul((24, 24)).unwrap();
        for mut style in [
            Style {
                color: w3cos_std::Color::rgb(77, 128, 179),
                color_rgba: Some([77.0 / 255.0, 128.0 / 255.0, 179.0 / 255.0, 1.0]),
                ..Style::default()
            },
            style(),
        ] {
            if style.color_rgba == Some([0.3, 0.5, 0.7, 1.0]) {
                style.color = w3cos_std::Color::BLACK;
            }
            let paint = super::super::text_color_paint(&style, 0.5);
            let called = std::cell::Cell::new(false);
            let canvas = surface.canvas();
            draw(canvas, &style, &paint, None, |target, target_paint| {
                assert!(std::ptr::eq(canvas, target));
                assert_eq!(target_paint.color4f(), paint.color4f());
                called.set(true);
            });
            assert!(called.get());
        }
    }
}
