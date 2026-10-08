//! Graphite's device-space 8x8 UNORM lookup, independent of raster/tile origin.
use skia_safe::{Canvas, Data, RuntimeEffect, Shader};

const SOURCE: &str = r#"
        uniform shader image;
        uniform float4 background;
        uniform float2 origin;
        uniform float2 axis_x;
        uniform float2 axis_y;
        float bit(float n, float place) { return mod(floor(n / place), 2.0); }
        half4 main(float2 p) {
            float2 xy = floor(origin + p.x * axis_x + p.y * axis_y);
            float m = bit(xy.y, 1.0) * 32 + bit(xy.x, 1.0) * 16
                    + bit(xy.y, 2.0) * 8 + bit(xy.x, 2.0) * 4
                    + bit(xy.y, 4.0) * 2 + bit(xy.x, 4.0);
            float lut = floor((m / 64 + 1.0 / 128) * 255 + 0.5) / 255;
            float value = (lut - 0.5) / 255;
            half4 color = image.eval(p);
            float3 rgb = clamp(color.rgb + value, 0.0, color.a);
            return half4(rgb + background.rgb * (1 - color.a),
                color.a + background.a * (1 - color.a));
        }
    "#;

thread_local! {
    static EFFECT: Option<RuntimeEffect> = RuntimeEffect::make_for_shader(SOURCE, None).ok();
}

#[cfg(test)]
#[test]
fn graphite_dither_shader_compiles() {
    RuntimeEffect::make_for_shader(SOURCE, None).expect("Graphite dither shader");
}

/// Composite against a known solid background before RGBA8 storage; CPU
/// SrcOver otherwise quantizes the tiny dither contribution before blending.
pub(super) fn shader(canvas: &Canvas, input: Shader, background: skia_safe::Color4f) -> Option<Shader> {
    let matrix = canvas.local_to_device_as_3x3();
    if matrix.has_perspective() { return None; }
    let o = matrix.map_point((0.0, 0.0));
    let x = matrix.map_point((1.0, 0.0));
    let y = matrix.map_point((0.0, 1.0));
    let uniforms = [background.r * background.a, background.g * background.a,
        background.b * background.a, background.a,
        o.x, o.y, x.x - o.x, x.y - o.y, y.x - o.x, y.y - o.y];
    let bytes = uniforms.iter().flat_map(|v| v.to_ne_bytes()).collect::<Vec<_>>();
    EFFECT.with(|effect| effect.as_ref()?.make_shader(Data::new_copy(&bytes), &[input.into()], None))
}
