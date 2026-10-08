//! Browser-compatible small Gaussian blur on byte-backed Skia filter inputs.
use skia_safe::{AlphaType, Canvas, Color, ColorType, Data, Image, ImageInfo, Paint,
    Point, Rect, RuntimeEffect, TileMode};
#[cfg(test)]
use skia_safe::Surface;

const SAMPLER: &str = r#"
    uniform shader image;
    uniform float4 offsets;
    uniform float4 weights;
    uniform float2 direction;
    float4 sample(float2 p, float offset) {
        float low = floor(offset);
        float t = floor((offset - low) * 256 + 0.5);
        float4 a = floor(float4(image.eval(p + low * direction)) * 255 + 0.5);
        float4 b = floor(float4(image.eval(p + (low + 1) * direction)) * 255 + 0.5);
        return floor((a * (256 - t) + b * t + 8) / 16) / 4080;
    }
    half4 main(float2 p) {
        float4 sum = sample(p, offsets.x) * weights.x;
        sum = sum + sample(p, offsets.y) * weights.y;
        sum = sum + sample(p, offsets.z) * weights.z;
        sum = sum + sample(p, offsets.w) * weights.w;
        return half4(floor(sum * 255 + 0.5) / 255);
    }
"#;

thread_local! {
    static EFFECT: Option<RuntimeEffect> = RuntimeEffect::make_for_shader(SAMPLER, None).ok();
}

/// This four-tap specialization covers support radii up to three pixels.
/// Larger sigma values must retain the general Skia path, not truncate a kernel.
pub(super) fn rasterize_small_blur(sigma: f32, bounds: Rect,
    draw_input: impl FnOnce(&Canvas)) -> Option<(Image, Point)> {
    if !sigma.is_finite() || sigma <= 0.0 || sigma > 1.0 || bounds.is_empty() {
        return None;
    }
    let radius = (3.0 * sigma).ceil() as i32;
    let output = Rect::new(bounds.left().floor() - radius as f32, bounds.top().floor() - radius as f32,
        bounds.right().ceil() + radius as f32, bounds.bottom().ceil() + radius as f32);
    let info = ImageInfo::new((output.width() as i32, output.height() as i32),
        ColorType::RGBA8888, AlphaType::Premul, None);
    let mut source = skia_safe::surfaces::raster(&info, None, None)?;
    source.canvas().clear(Color::TRANSPARENT);
    source.canvas().save();
    source.canvas().translate((-output.left(), -output.top()));
    source.canvas().clip_rect(bounds, None, Some(false));
    {
        // Glyph visibility recorded for the destination tile is in a different
        // coordinate system and must not cull the blur's offscreen input halo.
        let _recording = super::GlyphClipRecordingScope::new(None);
        draw_input(source.canvas());
    }
    source.canvas().restore();
    let mut image = source.image_snapshot();
    let mut kernel = [0_f32; 7];
    for (i, value) in kernel.iter_mut().enumerate() {
        let distance = i as i32 - 3;
        if distance.abs() <= radius {
            *value = (-(distance as f64).powi(2) / (2.0 * (sigma as f64).powi(2))).exp() as f32;
        }
    }
    let total = kernel.iter().copied().fold(0_f32, |sum, value| sum + value);
    for value in &mut kernel { *value /= total; }
    let center_weight = kernel[3] * 0.5 + kernel[4];
    let outer_weight = kernel[5] + kernel[6];
    let center_offset = kernel[4] / center_weight;
    let outer_offset = if outer_weight > 0.0 { 2.0 + kernel[6] / outer_weight } else { 2.0 };
    EFFECT.with(|effect| {
        let effect = effect.as_ref()?;
        // Materialize the complete logical input and its halo independently of
        // the destination clip. Runtime image filters without an exposed sample
        // radius restrict child sampling to the requested output and are unsafe
        // for partial/tiled replay. Image shaders retain the full backing here.
        for direction in [[1.0, 0.0], [0.0, 1.0]] {
            let child = image.to_shader((TileMode::Decal, TileMode::Decal),
                skia_safe::FilterMode::Nearest, None)?;
            let uniforms = [-outer_offset, -center_offset, center_offset, outer_offset,
                outer_weight, center_weight, center_weight, outer_weight, direction[0], direction[1]];
            let bytes = uniforms.iter().flat_map(|value| value.to_ne_bytes()).collect::<Vec<_>>();
            let shader = effect.make_shader(Data::new_copy(&bytes), &[child.into()], None)?;
            let mut surface = skia_safe::surfaces::raster(&info, None, None)?;
            surface.canvas().clear(Color::TRANSPARENT);
            let mut paint = Paint::default();
            paint.set_blend_mode(skia_safe::BlendMode::Src);
            paint.set_shader(shader);
            surface.canvas().draw_paint(&paint);
            image = surface.image_snapshot();
        }
        Some((image, Point::new(output.left(), output.top())))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blur_input_does_not_inherit_destination_glyph_clip() {
        let clip = Rect::from_xywh(10.0, 10.0, 4.0, 4.0);
        let recording = super::super::GlyphClipRecordingScope::new(Some(clip));
        rasterize_small_blur(1.0, Rect::from_xywh(8.0, 8.0, 20.0, 20.0), |_| {
            super::super::GLYPH_CLIP_RECORDING.with(|state| {
                assert!(state.borrow().as_ref().unwrap().clip.is_none());
            });
        }).unwrap();
        super::super::GLYPH_CLIP_RECORDING.with(|state| {
            assert_eq!(state.borrow().as_ref().unwrap().clip, Some(clip));
        });
        assert!(recording.finish().is_empty());
    }

    #[test]
    fn clipped_small_blur_matches_the_same_region_of_full_output() {
        let bounds = Rect::from_xywh(8.0, 8.0, 20.0, 20.0);
        let render = |clip: Option<Rect>| {
            let mut surface = Surface::new_raster_n32_premul((40, 40)).unwrap();
            surface.canvas().clear(Color::WHITE);
            if let Some(clip) = clip { surface.canvas().clip_rect(clip, None, Some(false)); }
            let (image, origin) = rasterize_small_blur(1.0, bounds, |canvas| {
                let mut paint = Paint::default();
                paint.set_color(Color::from_argb(180, 180, 40, 100));
                canvas.draw_rect(bounds, &paint);
            }).unwrap();
            surface.canvas().draw_image(&image, origin, None);
            let info = ImageInfo::new((40, 40), ColorType::RGBA8888, AlphaType::Premul, None);
            let mut pixels = vec![0_u8; 40 * 40 * 4];
            assert!(surface.read_pixels(&info, &mut pixels, 40 * 4, (0, 0)));
            pixels
        };
        let full = render(None);
        for clip in [Rect::new(10.0, 10.0, 14.0, 14.0), Rect::new(6.0, 7.0, 11.0, 12.0),
            Rect::new(26.0, 26.0, 31.0, 31.0)] {
            let cropped = render(Some(clip));
            for y in clip.top() as usize..clip.bottom() as usize {
                for x in clip.left() as usize..clip.right() as usize {
                    let i = (y * 40 + x) * 4;
                    assert_eq!(&cropped[i..i+4], &full[i..i+4], "clip={clip:?} pixel=({x},{y})");
                }
            }
        }
    }

    #[test]
    fn small_blur_specialization_never_truncates_a_larger_kernel() {
        let bounds = Rect::from_xywh(0.0, 0.0, 20.0, 20.0);
        for sigma in [0.0, -1.0, f32::NAN, f32::INFINITY, 1.01, 8.0] {
            assert!(rasterize_small_blur(sigma, bounds, |_| {}).is_none());
        }
        for sigma in [0.1, 0.5, 1.0] {
            assert!(rasterize_small_blur(sigma, bounds, |_| {}).is_some());
        }
    }
}
