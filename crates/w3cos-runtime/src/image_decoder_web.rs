//! WebCodecs ImageDecoder backed by the runtime's `image` codecs.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::{BufReader, Cursor};
use std::rc::Rc;
use std::sync::Arc;

use image::metadata::LoopCount;
use image::{AnimationDecoder, ImageFormat};
use w3cos_core::Value;

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
}

#[derive(Clone)]
struct DecodedFrame {
    width: u32,
    height: u32,
    rgba: Arc<Vec<u8>>,
    timestamp: f64,
    duration: Option<f64>,
}

fn error(name: &str, message: &str) -> Value {
    if name == "TypeError" || name == "RangeError" {
        w3cos_core::error_instance(name, vec![Value::string(message)])
    } else {
        w3cos_core::web::dom_exception_instance(message, name)
    }
}

fn throw(name: &str, message: &str) -> ! {
    w3cos_core::throw_value(error(name, message))
}

fn mime_format(type_name: &str) -> Option<ImageFormat> {
    ImageFormat::from_mime_type(type_name).filter(ImageFormat::reading_enabled)
}

fn repetition_count(loop_count: LoopCount, includes_initial_play: bool) -> f64 {
    match loop_count {
        LoopCount::Infinite => f64::INFINITY,
        LoopCount::Finite(count) => {
            if includes_initial_play {
                count.get().saturating_sub(1) as f64
            } else {
                count.get() as f64
            }
        }
    }
}

fn frames_from_animation(
    frames: image::Frames<'_>,
    desired_width: Option<u32>,
    desired_height: Option<u32>,
) -> Result<Vec<DecodedFrame>, String> {
    let frames = frames.collect_frames().map_err(|error| error.to_string())?;
    if frames.len() > 512 {
        return Err("animated image exceeds the 512-frame safety limit".into());
    }
    let mut timestamp = 0.0;
    let mut total_bytes = 0_usize;
    frames
        .into_iter()
        .map(|frame| {
            let (numerator, denominator) = frame.delay().numer_denom_ms();
            let duration = numerator as f64 / denominator.max(1) as f64 * 1000.0;
            let mut buffer = frame.into_buffer();
            let width = desired_width.unwrap_or(buffer.width());
            let height = desired_height.unwrap_or(buffer.height());
            if width != buffer.width() || height != buffer.height() {
                buffer = image::imageops::resize(
                    &buffer,
                    width,
                    height,
                    image::imageops::FilterType::Triangle,
                );
            }
            let rgba = buffer.into_raw();
            total_bytes = total_bytes.saturating_add(rgba.len());
            if total_bytes > 256 * 1024 * 1024 {
                return Err("decoded animation exceeds the 256 MiB safety limit".into());
            }
            let decoded = DecodedFrame {
                width,
                height,
                rgba: Arc::new(rgba),
                timestamp,
                duration: Some(duration),
            };
            timestamp += duration;
            Ok(decoded)
        })
        .collect()
}

fn decode_image(
    bytes: &[u8],
    format: ImageFormat,
    desired_width: Option<u32>,
    desired_height: Option<u32>,
) -> Result<(Vec<DecodedFrame>, f64), String> {
    match format {
        ImageFormat::Gif => {
            let decoder =
                image::codecs::gif::GifDecoder::new(BufReader::new(Cursor::new(bytes.to_vec())))
                    .map_err(|error| error.to_string())?;
            let repeats = repetition_count(decoder.loop_count(), false);
            let frames =
                frames_from_animation(decoder.into_frames(), desired_width, desired_height)?;
            Ok((frames, repeats))
        }
        ImageFormat::WebP => {
            let decoder =
                image::codecs::webp::WebPDecoder::new(BufReader::new(Cursor::new(bytes.to_vec())))
                    .map_err(|error| error.to_string())?;
            let repeats = repetition_count(decoder.loop_count(), true);
            let frames =
                frames_from_animation(decoder.into_frames(), desired_width, desired_height)?;
            Ok((frames, repeats))
        }
        ImageFormat::Png => {
            let decoder =
                image::codecs::png::PngDecoder::new(BufReader::new(Cursor::new(bytes.to_vec())))
                    .map_err(|error| error.to_string())?;
            if decoder.is_apng().map_err(|error| error.to_string())? {
                let decoder = decoder.apng().map_err(|error| error.to_string())?;
                let repeats = repetition_count(decoder.loop_count(), true);
                let frames =
                    frames_from_animation(decoder.into_frames(), desired_width, desired_height)?;
                return Ok((frames, repeats));
            }
            decode_static(bytes, format, desired_width, desired_height)
        }
        _ => decode_static(bytes, format, desired_width, desired_height),
    }
}

fn decode_static(
    bytes: &[u8],
    format: ImageFormat,
    desired_width: Option<u32>,
    desired_height: Option<u32>,
) -> Result<(Vec<DecodedFrame>, f64), String> {
    let mut rgba = image::load_from_memory_with_format(bytes, format)
        .map_err(|error| error.to_string())?
        .to_rgba8();
    let width = desired_width.unwrap_or(rgba.width());
    let height = desired_height.unwrap_or(rgba.height());
    if width != rgba.width() || height != rgba.height() {
        rgba = image::imageops::resize(&rgba, width, height, image::imageops::FilterType::Triangle);
    }
    if rgba.as_raw().len() > 256 * 1024 * 1024 {
        return Err("decoded image exceeds the 256 MiB safety limit".into());
    }
    Ok((
        vec![DecodedFrame {
            width,
            height,
            rgba: Arc::new(rgba.into_raw()),
            timestamp: 0.0,
            duration: None,
        }],
        0.0,
    ))
}

fn track_value(frame_count: usize, repetition_count: f64) -> Value {
    let value = Value::object(HashMap::from([
        ("animated".into(), Value::Bool(frame_count > 1)),
        ("frameCount".into(), Value::Number(frame_count as f64)),
        ("repetitionCount".into(), Value::Number(repetition_count)),
        ("selected".into(), Value::Bool(true)),
    ]));
    w3cos_core::class::set_prototype_of(&value, &class_for("ImageTrack").get_property("prototype"));
    value
}

fn track_list_value(track: Value) -> Value {
    let value = Value::object(HashMap::from([
        ("0".into(), track.clone()),
        ("length".into(), Value::Number(1.0)),
    ]));
    let selected_track = track.clone();
    value.set_property(
        "__w3cos_getter_selectedIndex",
        Value::function(move |_, _| {
            Value::Number(if selected_track.get_property("selected").to_bool() {
                0.0
            } else {
                -1.0
            })
        }),
    );
    let selected_track = track;
    value.set_property(
        "__w3cos_getter_selectedTrack",
        Value::function(move |_, _| {
            if selected_track.get_property("selected").to_bool() {
                selected_track.clone()
            } else {
                Value::Null
            }
        }),
    );
    value.set_property("ready", w3cos_core::promise::resolve(vec![value.clone()]));
    w3cos_core::class::set_prototype_of(
        &value,
        &class_for("ImageTrackList").get_property("prototype"),
    );
    value
}

fn video_frame(frame: &DecodedFrame) -> Value {
    let mut init = HashMap::from([
        ("codedWidth".into(), Value::Number(frame.width as f64)),
        ("codedHeight".into(), Value::Number(frame.height as f64)),
        ("format".into(), Value::string("RGBA")),
        ("timestamp".into(), Value::Number(frame.timestamp)),
    ]);
    if let Some(duration) = frame.duration {
        init.insert("duration".into(), Value::Number(duration));
    }
    w3cos_core::class::construct(
        &crate::webcodecs_web::class_for("VideoFrame"),
        vec![
            w3cos_core::binary::array_buffer_value((*frame.rgba).clone()),
            Value::object(init),
        ],
    )
}

fn decoder_value(init: Value) -> Value {
    if !init.is_object() {
        throw("TypeError", "ImageDecoder requires an init object");
    }
    let type_name = init.get_property("type").to_js_string();
    let Some(format) = mime_format(&type_name) else {
        throw(
            "NotSupportedError",
            "ImageDecoder MIME type is not supported",
        );
    };
    let data = init.get_property("data");
    let Some(bytes) = w3cos_core::binary::bytes_of(&data) else {
        static WARNING: std::sync::Once = std::sync::Once::new();
        WARNING.call_once(|| {
            eprintln!(
                "[w3cos] warning: ImageDecoder currently accepts complete BufferSource data; \
                 incremental ReadableStream decoding requires a streaming image adapter"
            );
        });
        throw(
            "NotSupportedError",
            "Streaming ImageDecoder input is unavailable",
        );
    };
    let desired_width = {
        let value = init.get_property("desiredWidth");
        if value.is_undefined() {
            None
        } else {
            let width = value.to_u32();
            if width == 0 {
                throw("RangeError", "ImageDecoder desiredWidth must be positive");
            }
            Some(width)
        }
    };
    let desired_height = {
        let value = init.get_property("desiredHeight");
        if value.is_undefined() {
            None
        } else {
            let height = value.to_u32();
            if height == 0 {
                throw("RangeError", "ImageDecoder desiredHeight must be positive");
            }
            Some(height)
        }
    };
    let (frames, repetitions) = decode_image(&bytes, format, desired_width, desired_height)
        .unwrap_or_else(|message| {
            throw(
                "EncodingError",
                &format!("ImageDecoder could not decode data: {message}"),
            )
        });
    if frames.is_empty() {
        throw("EncodingError", "ImageDecoder produced no frames");
    }
    let frames = Rc::new(frames);
    let closed = Rc::new(Cell::new(false));
    let generation = Rc::new(Cell::new(0_u64));
    let track = track_value(frames.len(), repetitions);
    let tracks = track_list_value(track.clone());
    let value = Value::object(HashMap::from([
        ("complete".into(), Value::Bool(true)),
        ("tracks".into(), tracks),
        ("type".into(), Value::string(&type_name)),
    ]));
    value.set_property(
        "completed",
        w3cos_core::promise::resolve(vec![Value::Undefined]),
    );
    let decode_frames = Rc::clone(&frames);
    let decode_closed = Rc::clone(&closed);
    let decode_track = track;
    value.set_property(
        "decode",
        Value::function(move |_, args| {
            if decode_closed.get() {
                return w3cos_core::promise::reject(vec![error(
                    "InvalidStateError",
                    "ImageDecoder is closed",
                )]);
            }
            if !decode_track.get_property("selected").to_bool() {
                return w3cos_core::promise::reject(vec![error(
                    "InvalidStateError",
                    "ImageDecoder has no selected track",
                )]);
            }
            let options = args.first().cloned().unwrap_or(Value::Undefined);
            let index = options.get_property("frameIndex").to_u32() as usize;
            let Some(frame) = decode_frames.get(index) else {
                return w3cos_core::promise::reject(vec![error(
                    "RangeError",
                    "ImageDecoder frameIndex is out of range",
                )]);
            };
            w3cos_core::promise::resolve(vec![Value::object(HashMap::from([
                ("complete".into(), Value::Bool(true)),
                ("image".into(), video_frame(frame)),
            ]))])
        }),
    );
    let reset_generation = Rc::clone(&generation);
    let reset_closed = Rc::clone(&closed);
    value.set_property(
        "reset",
        Value::function(move |_, _| {
            if reset_closed.get() {
                throw("InvalidStateError", "ImageDecoder is closed");
            }
            reset_generation.set(reset_generation.get().wrapping_add(1));
            Value::Undefined
        }),
    );
    value.set_property(
        "close",
        Value::function(move |_, _| {
            generation.set(generation.get().wrapping_add(1));
            closed.set(true);
            Value::Undefined
        }),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &class_for("ImageDecoder").get_property("prototype"),
    );
    value
}

fn build_class(name: &'static str) -> Value {
    let class = match name {
        "ImageDecoder" => Value::function(|_, args| {
            decoder_value(args.first().cloned().unwrap_or(Value::Undefined))
        }),
        _ => {
            Value::function(move |_, _| throw("TypeError", &format!("Illegal constructor: {name}")))
        }
    };
    class.set_property("name", Value::string(name));
    if name == "ImageDecoder" {
        class.set_property(
            "isTypeSupported",
            Value::function(|_, args| {
                let type_name = args.first().map(Value::to_js_string).unwrap_or_default();
                w3cos_core::promise::resolve(vec![Value::Bool(mime_format(&type_name).is_some())])
            }),
        );
    }
    let prototype = Value::object(HashMap::new());
    prototype.set_property("constructor", class.clone());
    for member in match name {
        "ImageDecoder" => &[
            "close",
            "complete",
            "completed",
            "decode",
            "reset",
            "tracks",
            "type",
        ][..],
        "ImageTrack" => &["animated", "frameCount", "repetitionCount", "selected"][..],
        "ImageTrackList" => &["length", "ready", "selectedIndex", "selectedTrack"][..],
        _ => &[][..],
    } {
        prototype.set_property(member, Value::Undefined);
    }
    class.set_property("prototype", prototype);
    class
}

pub fn class_for(name: &'static str) -> Value {
    CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        let class = build_class(name);
        classes.borrow_mut().insert(name.to_string(), class.clone());
        class
    })
}

pub fn reset() {
    CLASSES.with(|classes| classes.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn png_bytes() -> Vec<u8> {
        let image = image::RgbaImage::from_fn(2, 1, |x, _| {
            if x == 0 {
                image::Rgba([255, 0, 0, 255])
            } else {
                image::Rgba([0, 255, 0, 255])
            }
        });
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut bytes, ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }

    #[test]
    fn static_png_exposes_track_and_decodes_to_video_frame() {
        let decoder = w3cos_core::class::construct(
            &class_for("ImageDecoder"),
            vec![Value::object(HashMap::from([
                (
                    "data".into(),
                    w3cos_core::binary::array_buffer_value(png_bytes()),
                ),
                ("type".into(), Value::string("image/png")),
            ]))],
        );
        assert_eq!(
            decoder
                .get_property("tracks")
                .get_property("selectedTrack")
                .get_property("frameCount")
                .to_number(),
            1.0
        );
        let result = Rc::new(RefCell::new(Value::Undefined));
        let result_for_callback = Rc::clone(&result);
        decoder
            .call_method("decode", vec![Value::object(HashMap::new())])
            .call_method(
                "then",
                vec![Value::function(move |_, args| {
                    *result_for_callback.borrow_mut() =
                        args.first().cloned().unwrap_or(Value::Undefined);
                    Value::Undefined
                })],
            );
        crate::jsdom::drain_microtasks();
        assert!(result.borrow().get_property("complete").to_bool());
        let frame = result.borrow().get_property("image");
        assert!(w3cos_core::class::instance_of(
            &frame,
            &crate::webcodecs_web::class_for("VideoFrame")
        ));
        assert_eq!(frame.get_property("codedWidth").to_number(), 2.0);
        assert_eq!(frame.get_property("codedHeight").to_number(), 1.0);
    }

    #[test]
    fn type_support_matches_the_native_image_codec_table() {
        let supported = Rc::new(Cell::new(false));
        let supported_for_callback = Rc::clone(&supported);
        class_for("ImageDecoder")
            .call_method("isTypeSupported", vec![Value::string("image/png")])
            .call_method(
                "then",
                vec![Value::function(move |_, args| {
                    supported_for_callback.set(args.first().is_some_and(Value::to_bool));
                    Value::Undefined
                })],
            );
        crate::jsdom::drain_microtasks();
        assert!(supported.get());
    }
}
