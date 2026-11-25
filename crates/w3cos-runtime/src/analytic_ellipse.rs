//! Analytic filled-oval coverage matching Graphite's corner outset geometry.
//! Layout, border strokes and non-oval rounded rectangles stay in their own paths.
use skia_safe::{Canvas, Color, Data, Paint, RRect, Rect, RuntimeEffect, paint};

// Fixed Skia mesh topology (5eefbe51d17d2e379fa2d7353827e0ccb1e1f601).
// Interpolate original edge distances over device vertices on the rasterizer's
// 1/256-pixel grid; snapping the varyings too changes analytic curve coverage.
pub(super) const OVAL_SHADER: &str = r#"
uniform float4 bounds; uniform float4 rgba;
uniform float4 vertices[36]; uniform float4 edges[36];
float cross2(float2 a,float2 b){return a.x*b.y-a.y*b.x;}
bool triangle(float2 p,float4 a,float4 b,float4 c,float4 ea,float4 eb,float4 ec,inout float4 distances,inout float outset){
 float determinant=cross2(b.xy-a.xy,c.xy-a.xy);
 if(abs(determinant)<1e-8)return false;
 float wb=cross2(p-a.xy,c.xy-a.xy)/determinant,wc=cross2(b.xy-a.xy,p-a.xy)/determinant,wa=1-wb-wc;
 if(min(wa,min(wb,wc))<-.000001)return false;
 distances=wa*ea+wb*eb+wc*ec;outset=wa*a.z+wb*b.z+wc*c.z;return true;
}
half4 main(float2 p){
 float4 distances=float4(p-bounds.xy,bounds.zw-p);float outset=0;bool hit=false;
 if(!hit)hit=triangle(p,vertices[0],vertices[4],vertices[1],edges[0],edges[4],edges[1],distances,outset);
 if(!hit)hit=triangle(p,vertices[4],vertices[1],vertices[5],edges[4],edges[1],edges[5],distances,outset);
 if(!hit)hit=triangle(p,vertices[1],vertices[5],vertices[2],edges[1],edges[5],edges[2],distances,outset);
 if(!hit)hit=triangle(p,vertices[5],vertices[2],vertices[3],edges[5],edges[2],edges[3],distances,outset);
 if(!hit)hit=triangle(p,vertices[2],vertices[3],vertices[5],edges[2],edges[3],edges[5],distances,outset);
 if(!hit)hit=triangle(p,vertices[3],vertices[5],vertices[9],edges[3],edges[5],edges[9],distances,outset);
 if(!hit)hit=triangle(p,vertices[5],vertices[9],vertices[13],edges[5],edges[9],edges[13],distances,outset);
 if(!hit)hit=triangle(p,vertices[9],vertices[13],vertices[10],edges[9],edges[13],edges[10],distances,outset);
 if(!hit)hit=triangle(p,vertices[13],vertices[10],vertices[14],edges[13],edges[10],edges[14],distances,outset);
 if(!hit)hit=triangle(p,vertices[10],vertices[14],vertices[11],edges[10],edges[14],edges[11],distances,outset);
 if(!hit)hit=triangle(p,vertices[14],vertices[11],vertices[12],edges[14],edges[11],edges[12],distances,outset);
 if(!hit)hit=triangle(p,vertices[11],vertices[12],vertices[14],edges[11],edges[12],edges[14],distances,outset);
 if(!hit)hit=triangle(p,vertices[12],vertices[14],vertices[18],edges[12],edges[14],edges[18],distances,outset);
 if(!hit)hit=triangle(p,vertices[14],vertices[18],vertices[22],edges[14],edges[18],edges[22],distances,outset);
 if(!hit)hit=triangle(p,vertices[18],vertices[22],vertices[19],edges[18],edges[22],edges[19],distances,outset);
 if(!hit)hit=triangle(p,vertices[22],vertices[19],vertices[23],edges[22],edges[19],edges[23],distances,outset);
 if(!hit)hit=triangle(p,vertices[19],vertices[23],vertices[20],edges[19],edges[23],edges[20],distances,outset);
 if(!hit)hit=triangle(p,vertices[23],vertices[20],vertices[21],edges[23],edges[20],edges[21],distances,outset);
 if(!hit)hit=triangle(p,vertices[20],vertices[21],vertices[23],edges[20],edges[21],edges[23],distances,outset);
 if(!hit)hit=triangle(p,vertices[21],vertices[23],vertices[27],edges[21],edges[23],edges[27],distances,outset);
 if(!hit)hit=triangle(p,vertices[23],vertices[27],vertices[31],edges[23],edges[27],edges[31],distances,outset);
 if(!hit)hit=triangle(p,vertices[27],vertices[31],vertices[28],edges[27],edges[31],edges[28],distances,outset);
 if(!hit)hit=triangle(p,vertices[31],vertices[28],vertices[32],edges[31],edges[28],edges[32],distances,outset);
 if(!hit)hit=triangle(p,vertices[28],vertices[32],vertices[29],edges[28],edges[32],edges[29],distances,outset);
 if(!hit)hit=triangle(p,vertices[32],vertices[29],vertices[30],edges[32],edges[29],edges[30],distances,outset);
 if(!hit)hit=triangle(p,vertices[29],vertices[30],vertices[32],edges[29],edges[30],edges[32],distances,outset);
 if(!hit)hit=triangle(p,vertices[30],vertices[32],vertices[0],edges[30],edges[32],edges[0],distances,outset);
 if(!hit)hit=triangle(p,vertices[32],vertices[0],vertices[4],edges[32],edges[0],edges[4],distances,outset);
 if(!hit)hit=triangle(p,vertices[4],vertices[6],vertices[5],edges[4],edges[6],edges[5],distances,outset);
 if(!hit)hit=triangle(p,vertices[6],vertices[5],vertices[7],edges[6],edges[5],edges[7],distances,outset);
 if(!hit)hit=triangle(p,vertices[5],vertices[7],vertices[13],edges[5],edges[7],edges[13],distances,outset);
 if(!hit)hit=triangle(p,vertices[7],vertices[13],vertices[15],edges[7],edges[13],edges[15],distances,outset);
 if(!hit)hit=triangle(p,vertices[13],vertices[15],vertices[14],edges[13],edges[15],edges[14],distances,outset);
 if(!hit)hit=triangle(p,vertices[15],vertices[14],vertices[16],edges[15],edges[14],edges[16],distances,outset);
 if(!hit)hit=triangle(p,vertices[14],vertices[16],vertices[22],edges[14],edges[16],edges[22],distances,outset);
 if(!hit)hit=triangle(p,vertices[16],vertices[22],vertices[24],edges[16],edges[22],edges[24],distances,outset);
 if(!hit)hit=triangle(p,vertices[22],vertices[24],vertices[23],edges[22],edges[24],edges[23],distances,outset);
 if(!hit)hit=triangle(p,vertices[24],vertices[23],vertices[25],edges[24],edges[23],edges[25],distances,outset);
 if(!hit)hit=triangle(p,vertices[23],vertices[25],vertices[31],edges[23],edges[25],edges[31],distances,outset);
 if(!hit)hit=triangle(p,vertices[25],vertices[31],vertices[33],edges[25],edges[31],edges[33],distances,outset);
 if(!hit)hit=triangle(p,vertices[31],vertices[33],vertices[32],edges[31],edges[33],edges[32],distances,outset);
 if(!hit)hit=triangle(p,vertices[33],vertices[32],vertices[34],edges[33],edges[32],edges[34],distances,outset);
 if(!hit)hit=triangle(p,vertices[32],vertices[34],vertices[4],edges[32],edges[34],edges[4],distances,outset);
 if(!hit)hit=triangle(p,vertices[34],vertices[4],vertices[6],edges[34],edges[4],edges[6],distances,outset);
 if(!hit)hit=triangle(p,vertices[6],vertices[8],vertices[7],edges[6],edges[8],edges[7],distances,outset);
 if(!hit)hit=triangle(p,vertices[7],vertices[17],vertices[15],edges[7],edges[17],edges[15],distances,outset);
 if(!hit)hit=triangle(p,vertices[15],vertices[17],vertices[16],edges[15],edges[17],edges[16],distances,outset);
 if(!hit)hit=triangle(p,vertices[16],vertices[26],vertices[24],edges[16],edges[26],edges[24],distances,outset);
 if(!hit)hit=triangle(p,vertices[24],vertices[26],vertices[25],edges[24],edges[26],edges[25],distances,outset);
 if(!hit)hit=triangle(p,vertices[25],vertices[35],vertices[33],edges[25],edges[35],edges[33],distances,outset);
 if(!hit)hit=triangle(p,vertices[33],vertices[35],vertices[34],edges[33],edges[35],edges[34],distances,outset);
 if(!hit)hit=triangle(p,vertices[34],vertices[8],vertices[6],edges[34],edges[8],edges[6],distances,outset);
 float2 r=(bounds.zw-bounds.xy)*.5,uv=r-min(distances.xy,distances.zw);
 float2 invR2=1/(r*r),normUV=invR2*uv;
 float invGradLength=inversesqrt(max(dot(normUV,normUV),1.1755e-38));
 float ellipseDistance=-.5*invGradLength*(dot(uv,normUV)-1);
 float d=min(min(distances.x,distances.y),min(distances.z,distances.w));
 float coverage=clamp(.5+min(d,ellipseDistance)-outset,0,1);
 return half4(rgba*coverage);
}"#;

thread_local! {
    static OVAL: Option<RuntimeEffect> = RuntimeEffect::make_for_shader(OVAL_SHADER, None).ok();
}

/// The mesh triangles in `OVAL_SHADER`'s test order (vertex and edge indices).
const OVAL_TRIANGLES: [[usize; 3]; 52] = [
    [0, 4, 1],
    [4, 1, 5],
    [1, 5, 2],
    [5, 2, 3],
    [2, 3, 5],
    [3, 5, 9],
    [5, 9, 13],
    [9, 13, 10],
    [13, 10, 14],
    [10, 14, 11],
    [14, 11, 12],
    [11, 12, 14],
    [12, 14, 18],
    [14, 18, 22],
    [18, 22, 19],
    [22, 19, 23],
    [19, 23, 20],
    [23, 20, 21],
    [20, 21, 23],
    [21, 23, 27],
    [23, 27, 31],
    [27, 31, 28],
    [31, 28, 32],
    [28, 32, 29],
    [32, 29, 30],
    [29, 30, 32],
    [30, 32, 0],
    [32, 0, 4],
    [4, 6, 5],
    [6, 5, 7],
    [5, 7, 13],
    [7, 13, 15],
    [13, 15, 14],
    [15, 14, 16],
    [14, 16, 22],
    [16, 22, 24],
    [22, 24, 23],
    [24, 23, 25],
    [23, 25, 31],
    [25, 31, 33],
    [31, 33, 32],
    [33, 32, 34],
    [32, 34, 4],
    [34, 4, 6],
    [6, 8, 7],
    [7, 17, 15],
    [15, 17, 16],
    [16, 26, 24],
    [24, 26, 25],
    [25, 35, 33],
    [33, 35, 34],
    [34, 8, 6],
];

/// `W3COS_PAINT_OVAL_LOOP`: `OVAL_SHADER` with its 52 unrolled triangle tests
/// as one loop over per-triangle uniforms. Each draw builds the raster
/// pipeline of the shader's program anew, and the unrolled program made that
/// setup cost far more than the pixels; the loop runs the same arithmetic in
/// the same order, stopping at the first hit as the chain of `if(!hit)` did.
pub(super) const OVAL_LOOP_SHADER: &str = r#"
uniform float4 bounds; uniform float4 rgba;
uniform float4 ta[52]; uniform float4 tb[52]; uniform float4 tc[52];
uniform float4 ea[52]; uniform float4 eb[52]; uniform float4 ec[52];
float cross2(float2 a,float2 b){return a.x*b.y-a.y*b.x;}
bool triangle(float2 p,float4 a,float4 b,float4 c,float4 ea,float4 eb,float4 ec,inout float4 distances,inout float outset){
 float determinant=cross2(b.xy-a.xy,c.xy-a.xy);
 if(abs(determinant)<1e-8)return false;
 float wb=cross2(p-a.xy,c.xy-a.xy)/determinant,wc=cross2(b.xy-a.xy,p-a.xy)/determinant,wa=1-wb-wc;
 if(min(wa,min(wb,wc))<-.000001)return false;
 distances=wa*ea+wb*eb+wc*ec;outset=wa*a.z+wb*b.z+wc*c.z;return true;
}
half4 main(float2 p){
 float4 distances=float4(p-bounds.xy,bounds.zw-p);float outset=0;
 for(int i=0;i<52;i++){if(triangle(p,ta[i],tb[i],tc[i],ea[i],eb[i],ec[i],distances,outset))break;}
 float2 r=(bounds.zw-bounds.xy)*.5,uv=r-min(distances.xy,distances.zw);
 float2 invR2=1/(r*r),normUV=invR2*uv;
 float invGradLength=inversesqrt(max(dot(normUV,normUV),1.1755e-38));
 float ellipseDistance=-.5*invGradLength*(dot(uv,normUV)-1);
 float d=min(min(distances.x,distances.y),min(distances.z,distances.w));
 float coverage=clamp(.5+min(d,ellipseDistance)-outset,0,1);
 return half4(rgba*coverage);
}"#;

thread_local! {
    static OVAL_LOOP: Option<RuntimeEffect> = RuntimeEffect::make_for_shader(OVAL_LOOP_SHADER, None).ok();
    /// (loop draws, verify mismatches).
    static OVAL_LOOP_STATS: std::cell::Cell<(u64, u64)> = const { std::cell::Cell::new((0, 0)) };
}

#[cfg(test)]
thread_local! {
    pub(crate) static OVAL_LOOP_OVERRIDE: std::cell::Cell<Option<u8>> = const { std::cell::Cell::new(None) };
}

/// `W3COS_PAINT_OVAL_LOOP=1|verify` (off by default): draw analytic ovals
/// with `OVAL_LOOP_SHADER`. `verify` also evaluates `OVAL_SHADER` over the
/// draw and compares every shaded value (float32) before drawing.
pub(crate) fn oval_loop_mode() -> u8 {
    #[cfg(test)]
    if let Some(mode) = OVAL_LOOP_OVERRIDE.with(std::cell::Cell::get) {
        return mode;
    }
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(
        || match std::env::var("W3COS_PAINT_OVAL_LOOP").ok().as_deref() {
            Some("1") => 1,
            Some("verify") => 2,
            _ => 0,
        },
    )
}

/// Ovals drawn with the loop shader on this thread: (draws, verify mismatches).
pub(crate) fn oval_loop_stats() -> (u64, u64) {
    OVAL_LOOP_STATS.with(std::cell::Cell::get)
}

/// `mesh_uniforms` regrouped per triangle for `OVAL_LOOP_SHADER`.
fn loop_uniforms(mesh: &[f32]) -> Vec<f32> {
    let (head, rest) = mesh.split_at(8);
    let (vertices, edges) = rest.split_at(36 * 4);
    let mut uniforms = Vec::with_capacity(8 + 6 * 52 * 4);
    uniforms.extend_from_slice(head);
    for table in [vertices, edges] {
        for corner in 0..3 {
            for triangle in OVAL_TRIANGLES {
                let at = triangle[corner] * 4;
                uniforms.extend_from_slice(&table[at..at + 4]);
            }
        }
    }
    uniforms
}

/// Every value `shader` yields over `rect` (device pixels), as float32 bits.
fn shaded_values(shader: &skia_safe::Shader, rect: skia_safe::IRect) -> Option<Vec<u8>> {
    let info = skia_safe::ImageInfo::new(
        (rect.width(), rect.height()),
        skia_safe::ColorType::RGBAF32,
        skia_safe::AlphaType::Premul,
        None,
    );
    let mut surface = skia_safe::surfaces::raster(&info, None, None)?;
    let canvas = surface.canvas();
    canvas.translate((-rect.left as f32, -rect.top as f32));
    let mut paint = Paint::default();
    paint
        .set_color(Color::WHITE)
        .set_shader(shader.clone())
        .set_anti_alias(false)
        .set_blend_mode(skia_safe::BlendMode::Src);
    canvas.draw_rect(Rect::from(rect), &paint);
    let mut bytes = vec![0u8; rect.width() as usize * rect.height() as usize * 16];
    surface
        .read_pixels(&info, &mut bytes, rect.width() as usize * 16, (0, 0))
        .then_some(bytes)
}

fn mesh_uniforms(bounds: Rect, color: skia_safe::Color4f) -> Vec<f32> {
    let mut uniforms = vec![
        bounds.left,
        bounds.top,
        bounds.right,
        bounds.bottom,
        color.r * color.a,
        color.g * color.a,
        color.b * color.a,
        color.a,
    ];
    let lx = bounds.left;
    let ly = bounds.top;
    let w = bounds.width();
    let h = bounds.height();
    let mut verts = Vec::new();
    let mut distances = Vec::new();
    let rx = w * 0.5;
    let ry = h * 0.5;
    // Use the vertex shader's literal, not a separately rounded sqrt(2)-1.
    let k = 0.41421356237_f32;
    for corner in 0..4 {
        let sx = if corner == 0 || corner == 3 {
            -1.0
        } else {
            1.0
        };
        let sy = if corner < 2 { -1.0 } else { 1.0 };
        let mut anchors = [[rx, k * ry], [k * rx, ry]];
        let mut inner = [[rx - 1.0, 0.0], [0.0, ry - 1.0]];
        let mut axis = [[1.0, 0.0], [0.0, 1.0]];
        if rx.min(ry) <= 2.0 {
            inner = [[0.0, 0.0]; 2];
        }
        if corner % 2 != 0 {
            anchors.swap(0, 1);
            inner.swap(0, 1);
            axis.swap(0, 1);
        }
        let normal = [ry / rx.hypot(ry), rx / rx.hypot(ry)];
        for id in 0..9 {
            let uv = if id < 6 {
                anchors[if id == 2 || id == 3 || id == 5 { 1 } else { 0 }]
            } else if id == 8 {
                [0.0, 0.0]
            } else {
                inner[id - 6]
            };
            let out = if id < 4 {
                if id == 0 {
                    axis[0]
                } else if id == 3 {
                    axis[1]
                } else {
                    normal
                }
            } else {
                [0.0, 0.0]
            };
            let px = lx + rx + sx * uv[0];
            let py = ly + ry + sy * uv[1];
            let dx = px + sx * out[0];
            let dy = py + sy * out[1];
            verts.extend_from_slice(&[
                (dx * 256.0).round() / 256.0,
                (dy * 256.0).round() / 256.0,
                if id < 4 { 1.0 } else { 0.0 },
                0.0,
            ]);
            distances.extend_from_slice(&[px - lx, py - ly, lx + w - px, ly + h - py]);
        }
    }
    uniforms.extend(verts);
    uniforms.extend(distances);
    uniforms
}

pub(super) fn draw(canvas: &Canvas, oval: &RRect, paint: &Paint) -> bool {
    if !oval.is_oval()
        || paint.style() != paint::Style::Fill
        || paint.shader().is_some()
        || paint.mask_filter().is_some()
        || paint.image_filter().is_some()
        || paint.color_filter().is_some()
        || paint.path_effect().is_some()
    {
        return false;
    }
    let matrix = canvas.local_to_device_as_3x3();
    if !matrix.is_scale_translate() {
        return false;
    }
    let mut local = *oval.rect();
    if matrix.is_identity() {
        local = Rect::new(
            local.left.round(),
            local.top.round(),
            local.right.round(),
            local.bottom.round(),
        );
    }
    let Some(bounds) = matrix.map_rect_scale_translate(local) else {
        return false;
    };
    if !bounds.is_finite() || bounds.width() <= 0.0 || bounds.height() <= 0.0 {
        return false;
    }
    let device = Rect::new(
        bounds.left.floor(),
        bounds.top.floor(),
        bounds.right.ceil(),
        bounds.bottom.ceil(),
    );
    let mode = oval_loop_mode();
    let make = |values: &[f32], effect: &RuntimeEffect| {
        let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_ne_bytes()).collect();
        effect.make_shader(Data::new_copy(&bytes), &[], None)
    };
    let looped = (mode != 0)
        .then(|| {
            OVAL_LOOP.with(|effect| {
                let effect = effect.as_ref()?;
                make(
                    &loop_uniforms(&mesh_uniforms(bounds, paint.color4f())),
                    effect,
                )
            })
        })
        .flatten();
    OVAL.with(|effect| {
        let Some(effect) = effect else {
            return false;
        };
        let shader = match looped {
            Some(looped) => {
                let mut mismatch = false;
                if mode == 2 {
                    let unrolled = make(&mesh_uniforms(bounds, paint.color4f()), effect);
                    let rect = skia_safe::IRect::new(
                        device.left as i32,
                        device.top as i32,
                        device.right as i32,
                        device.bottom as i32,
                    );
                    let same = unrolled.is_some_and(|unrolled| {
                        let a = shaded_values(&unrolled, rect);
                        a.is_some() && a == shaded_values(&looped, rect)
                    });
                    if !same {
                        mismatch = true;
                        eprintln!(
                            "[paint-oval-loop-verify] mismatch: oval {bounds:?} shades differently"
                        );
                    }
                }
                OVAL_LOOP_STATS.with(|stats| {
                    let (draws, mismatches) = stats.get();
                    stats.set((draws + 1, mismatches + mismatch as u64));
                });
                looped
            }
            None => {
                let Some(shader) = make(&mesh_uniforms(bounds, paint.color4f()), effect) else {
                    return false;
                };
                shader
            }
        };
        let mut analytic = paint.clone();
        // Alpha is already premultiplied in the shader; do not apply it twice.
        analytic
            .set_color(Color::WHITE)
            .set_shader(shader)
            .set_anti_alias(false);
        let save = canvas.save();
        canvas.reset_matrix();
        canvas.draw_rect(device, &analytic);
        canvas.restore_to_count(save);
        true
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use skia_safe::{AlphaType, ColorType, ImageInfo, surfaces};

    fn assert_browser_edge(
        size: f32,
        background: Color,
        color: w3cos_std::Color,
        x: usize,
        expected: [u8; 4],
    ) {
        let mut surface = surfaces::raster_n32_premul((32, 32)).unwrap();
        surface.canvas().clear(background);
        let paint = crate::render_skia::color_paint(color, 1.0);
        let oval = RRect::new_oval(Rect::from_xywh(3.0, 3.0, size, size));
        assert!(draw(surface.canvas(), &oval, &paint));
        let info = ImageInfo::new((32, 32), ColorType::RGBA8888, AlphaType::Premul, None);
        let mut bytes = vec![0u8; 32 * 32 * 4];
        assert!(surface.read_pixels(&info, &mut bytes, 32 * 4, (0, 0)));
        let offset = (3 * 32 + x) * 4;
        assert_eq!(
            &bytes[offset..offset + 4],
            &expected,
            "DEFAULT-browser oval {size}x{size} edge ({x},3)"
        );
    }

    #[test]
    fn oval_loop_takes_the_loop_and_matches_the_unrolled_shader() {
        let render = |mode: u8| {
            OVAL_LOOP_OVERRIDE.with(|value| value.set(Some(mode)));
            let mut surface = surfaces::raster_n32_premul((96, 64)).unwrap();
            surface.canvas().clear(Color::from_rgb(255, 165, 0));
            for (x, y, w, h, color) in [
                (3.0, 3.0, 6.0, 6.0, w3cos_std::Color::rgba(0, 0, 0, 128)),
                (12.25, 4.5, 8.0, 8.0, w3cos_std::Color::rgb(37, 99, 211)),
                (24.0, 2.0, 18.0, 18.0, w3cos_std::Color::BLACK),
                (
                    44.5,
                    3.75,
                    40.0,
                    22.5,
                    w3cos_std::Color::rgba(200, 30, 90, 200),
                ),
                (2.0, 30.0, 3.0, 2.0, w3cos_std::Color::WHITE),
                (
                    10.0,
                    28.0,
                    60.0,
                    33.0,
                    w3cos_std::Color::rgba(20, 160, 60, 255),
                ),
            ] {
                let paint = crate::render_skia::color_paint(color, 1.0);
                let oval = RRect::new_oval(Rect::from_xywh(x, y, w, h));
                surface.canvas().save();
                surface.canvas().translate((0.375, 0.0));
                assert!(draw(surface.canvas(), &oval, &paint));
                surface.canvas().restore();
                assert!(draw(surface.canvas(), &oval, &paint));
            }
            let info = ImageInfo::new((96, 64), ColorType::RGBA8888, AlphaType::Premul, None);
            let mut bytes = vec![0u8; 96 * 64 * 4];
            assert!(surface.read_pixels(&info, &mut bytes, 96 * 4, (0, 0)));
            bytes
        };
        let unrolled = render(0);
        let before = oval_loop_stats();
        let verified = render(2);
        let after = oval_loop_stats();
        let looped = render(1);
        OVAL_LOOP_OVERRIDE.with(|value| value.set(None));
        assert_eq!(after.0 - before.0, 12, "every oval took the loop shader");
        assert_eq!(
            after.1, before.1,
            "the loop shades every value as the unrolled one"
        );
        assert!(oval_loop_stats().0 >= after.0 + 12);
        assert!(unrolled == verified && unrolled == looped, "same pixels");
    }

    #[test]
    fn browser_mesh_translucent_circle_on_orange() {
        // V742/V771: continuous direct DOM paint, not an isolated surface.
        assert_browser_edge(
            6.0,
            Color::from_rgb(255, 165, 0),
            w3cos_std::Color::rgba(0, 0, 0, 128),
            4,
            [180, 117, 0, 255],
        );
    }

    #[test]
    fn browser_mesh_colored_circle_on_white() {
        assert_browser_edge(
            8.0,
            Color::WHITE,
            w3cos_std::Color::rgb(37, 99, 211),
            4,
            [212, 225, 246, 255],
        );
    }

    #[test]
    fn browser_mesh_large_circle_inner_triangle() {
        // V775 extends the browser oracle beyond the original five sizes.
        assert_browser_edge(
            18.0,
            Color::WHITE,
            w3cos_std::Color::BLACK,
            10,
            [32, 32, 32, 255],
        );
    }
}
