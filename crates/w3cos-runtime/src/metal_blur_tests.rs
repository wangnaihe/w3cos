//! Explicit, source-bound GPU diagnostics; never silently replace the WPT oracle.
use super::*;
use std::ffi::{c_char, c_void};

#[link(name = "Metal", kind = "framework")]
unsafe extern "C" {
    fn MTLCreateSystemDefaultDevice() -> *mut c_void;
}
#[link(name = "objc")]
unsafe extern "C" {
    fn sel_registerName(name: *const c_char) -> *mut c_void;
    #[link_name = "objc_msgSend"]
    fn object_message(object: *mut c_void, selector: *mut c_void) -> *mut c_void;
    fn objc_release(object: *mut c_void);
}

struct Object(*mut c_void);
impl Drop for Object {
    fn drop(&mut self) {
        unsafe { objc_release(self.0); }
    }
}

// Diagnostic bridge from the calibrated sampler to Skia's actual shader
// execution. Keep this test-only until the retained effect input/output bounds
// and arbitrary filter chains have their own regression coverage.
fn calibrated_small_blur(context: &mut skia_safe::gpu::DirectContext,
    input: &Image, info: &ImageInfo, origin: (f32, f32)) -> Image {
    let effect = skia_safe::RuntimeEffect::make_for_shader(r#"
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
    "#, None).expect("calibrated blur shader compiles");
    let mut kernel = [0_f32; 7];
    for (i, value) in kernel.iter_mut().enumerate() {
        *value = (-((i as f64 - 3.0).powi(2)) / 2.0).exp() as f32;
    }
    let total = kernel.iter().copied().fold(0_f32, |sum, value| sum + value);
    for value in &mut kernel { *value /= total; }
    let center_weight = kernel[3] * 0.5 + kernel[4];
    let outer_weight = kernel[5] + kernel[6];
    let center_offset = kernel[4] / center_weight;
    let outer_offset = 2.0 + kernel[6] / outer_weight;
    let mut image = input.clone();
    for (pass, direction) in [[1_f32, 0_f32], [0_f32, 1_f32]].iter().enumerate() {
        let matrix = Matrix::translate(if pass == 0 { origin } else { (0.0, 0.0) });
        let child = image.to_shader((TileMode::Decal, TileMode::Decal),
            skia_safe::FilterMode::Nearest, &matrix).unwrap();
        let uniforms = [-outer_offset, -center_offset, center_offset, outer_offset,
            outer_weight, center_weight, center_weight, outer_weight,
            direction[0], direction[1]];
        let bytes = uniforms.iter().flat_map(|value| value.to_ne_bytes()).collect::<Vec<_>>();
        let shader = effect.make_shader(Data::new_copy(&bytes), &[child.into()], None).unwrap();
        let mut surface = diagnostic_surface(context, info);
        surface.canvas().clear(Color::TRANSPARENT);
        let mut paint = Paint::default();
        paint.set_blend_mode(skia_safe::BlendMode::Src);
        paint.set_shader(shader);
        surface.canvas().draw_paint(&paint);
        image = surface.image_snapshot();
    }
    image
}

fn diagnostic_surface(context: &mut skia_safe::gpu::DirectContext, info: &ImageInfo) -> Surface {
    if std::env::var_os("W3COS_BLUR_RASTER").is_some() {
        skia_safe::surfaces::raster(info, None, None).unwrap()
    } else {
        skia_safe::gpu::surfaces::render_target(context,
            skia_safe::gpu::Budgeted::Yes, info, None, None, None, None, None).unwrap()
    }
}

#[test]
#[ignore = "requires explicit bound unfiltered frame and unchanged Chromium PNG"]
fn metal_small_blur_matches_the_bound_browser_capture() {
    let bytes = std::fs::read(std::env::var("W3COS_BLUR_INPUT_FRAME").unwrap()).unwrap();
    let width = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
    let height = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    assert_eq!(bytes.len(), 8 + width * height * 4);
    let expected = image::open(std::env::var("W3COS_BLUR_BROWSER_PNG").unwrap())
        .unwrap().to_rgba8();
    assert_eq!(expected.dimensions(), (width as u32, height as u32));
    // The isolated input is black glyph coverage over white. Restore its
    // transparent premultiplied layer without changing any glyph samples.
    let mut coverage = bytes[8..].to_vec();
    for pixel in coverage.chunks_exact_mut(4) {
        assert_eq!(pixel[0], pixel[1]);
        assert_eq!(pixel[1], pixel[2]);
        assert_eq!(pixel[3], 255);
        pixel[3] = 255 - pixel[0];
        pixel[..3].fill(0);
    }
    let device = Object(unsafe { MTLCreateSystemDefaultDevice() });
    assert!(!device.0.is_null(), "Metal device unavailable");
    let queue = Object(unsafe { object_message(device.0, sel_registerName(c"newCommandQueue".as_ptr())) });
    assert!(!queue.0.is_null(), "Metal queue unavailable");
    let backend = unsafe { skia_safe::gpu::mtl::BackendContext::new(device.0, queue.0) };
    let mut context = skia_safe::gpu::direct_contexts::make_metal(&backend, None).unwrap();
    let info = ImageInfo::new((width as i32, height as i32), ColorType::RGBA8888,
        AlphaType::Premul, None);
    let bounds = std::env::var("W3COS_BLUR_INPUT_BOUNDS").ok().map(|value|
        value.split(',').map(|n| n.parse::<usize>().unwrap()).collect::<Vec<_>>())
        .unwrap_or_else(|| vec![0,0,width,height]);
    assert_eq!(bounds.len(), 4);
    let (x,y,w,h) = (bounds[0],bounds[1],bounds[2],bounds[3]);
    assert!(w > 0 && h > 0 && x+w <= width && y+h <= height);
    let cropped = (y..y+h).flat_map(|row| coverage[
        (row*width+x)*4..(row*width+x+w)*4].iter().copied()).collect::<Vec<_>>();
    let source_info = ImageInfo::new((w as i32,h as i32), ColorType::RGBA8888,
        AlphaType::Premul, None);
    let input = images::raster_from_data(&source_info, Data::new_copy(&cropped), w * 4).unwrap();
    let mut surface = diagnostic_surface(&mut context, &info);
    surface.canvas().clear(Color::WHITE);
    let mut paint = Paint::default();
    if std::env::var_os("W3COS_BLUR_FILTER_GRAPH").is_some() {
        paint = color_paint(w3cos_std::Color::WHITE, 1.0);
        let (filtered, origin) = browser_blur::rasterize_small_blur(1.0,
            Rect::from_xywh(x as f32,y as f32,w as f32,h as f32), |canvas| {
                canvas.draw_image(&input, (x as f32,y as f32), None);
            }).unwrap();
        surface.canvas().draw_image(&filtered, origin, Some(&paint));
    } else if std::env::var_os("W3COS_BLUR_CALIBRATED_SHADER").is_some() {
        let filtered = calibrated_small_blur(&mut context, &input, &info, (x as f32,y as f32));
        let composite = skia_safe::RuntimeEffect::make_for_blender(
            "half4 main(half4 src, half4 dst) { return src + dst * (1 - src.a); }", None
        ).unwrap().make_blender(Data::new_empty(), None).unwrap();
        paint.set_blender(composite);
        surface.canvas().draw_image(&filtered, (0.0, 0.0), Some(&paint));
    } else {
        paint.set_image_filter(skia_filter_chain(&parse_css_filter("blur(1px)").unwrap()));
        surface.canvas().draw_image(&input, (x as f32, y as f32), Some(&paint));
    }
    context.flush_and_submit();
    let mut actual = vec![0; width * height * 4];
    assert!(surface.read_pixels(&info, &mut actual, width * 4, (0, 0)));
    if let Ok(path) = std::env::var("W3COS_BLUR_GPU_OUTPUT") {
        image::save_buffer(path, &actual, width as u32, height as u32,
            image::ColorType::Rgba8).unwrap();
    }
    let differing = actual.chunks_exact(4).zip(expected.as_raw().chunks_exact(4))
        .filter(|(a,b)| a != b).count();
    assert_eq!(differing, 0, "native Metal diagnostic is not default-browser pixel-exact");
}
