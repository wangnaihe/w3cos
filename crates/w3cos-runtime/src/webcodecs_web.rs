//! WebCodecs data-container primitives independent of platform codecs.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Once;

use w3cos_core::Value;

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
}

fn type_error(message: &str) -> ! {
    w3cos_core::throw_value(w3cos_core::error_instance(
        "TypeError",
        vec![Value::string(message)],
    ))
}

fn chunk_value(name: &'static str, init: Value) -> Value {
    let data = init.get_property("data");
    let Some(bytes) = w3cos_core::binary::bytes_of(&data) else {
        type_error("Encoded chunk data must be a BufferSource");
    };
    let kind = init.get_property("type").to_js_string();
    let valid_type = if name == "EncodedAudioChunk" {
        matches!(kind.as_str(), "key" | "delta")
    } else {
        matches!(kind.as_str(), "key" | "delta")
    };
    if !valid_type {
        type_error("Encoded chunk type must be 'key' or 'delta'");
    }
    let bytes = Rc::new(bytes);
    let value = Value::object(HashMap::from([
        ("byteLength".into(), Value::Number(bytes.len() as f64)),
        ("duration".into(), init.get_property("duration")),
        ("timestamp".into(), init.get_property("timestamp")),
        ("type".into(), Value::string(&kind)),
    ]));
    let copy_bytes = Rc::clone(&bytes);
    value.set_property(
        "copyTo",
        Value::function(move |_, args| {
            let destination = args.first().cloned().unwrap_or(Value::Undefined);
            let capacity = w3cos_core::binary::bytes_of(&destination)
                .map(|bytes| bytes.len())
                .unwrap_or_default();
            if capacity < copy_bytes.len() {
                w3cos_core::throw_value(w3cos_core::web::dom_exception_instance(
                    "Destination is too small",
                    "DataError",
                ));
            }
            let source = w3cos_core::binary::typed_array_value(
                copy_bytes
                    .iter()
                    .map(|byte| Value::Number(*byte as f64))
                    .collect(),
            );
            destination.call_method("set", vec![source]);
            Value::Undefined
        }),
    );
    w3cos_core::class::set_prototype_of(&value, &class_for(name).get_property("prototype"));
    value
}

fn audio_data_value(init: Value) -> Value {
    let data = init.get_property("data");
    let Some(bytes) = w3cos_core::binary::bytes_of(&data) else {
        type_error("AudioData data must be a BufferSource");
    };
    let format = init.get_property("format").to_js_string();
    if !matches!(
        format.as_str(),
        "u8" | "s16" | "s32" | "f32" | "u8-planar" | "s16-planar" | "s32-planar" | "f32-planar"
    ) {
        type_error("AudioData format is not supported");
    }
    let sample_rate = init.get_property("sampleRate").to_number();
    let frames = init.get_property("numberOfFrames").to_u32();
    let channels = init.get_property("numberOfChannels").to_u32();
    if !sample_rate.is_finite() || sample_rate <= 0.0 || frames == 0 || channels == 0 {
        type_error("AudioData requires positive sampleRate, numberOfFrames and numberOfChannels");
    }
    let timestamp = init.get_property("timestamp").to_number();
    let duration = frames as f64 * 1_000_000.0 / sample_rate;
    let bytes = Rc::new(bytes);
    let closed = Rc::new(Cell::new(false));
    let value = Value::object(HashMap::from([
        ("duration".into(), Value::Number(duration)),
        ("format".into(), Value::string(&format)),
        ("numberOfChannels".into(), Value::Number(channels as f64)),
        ("numberOfFrames".into(), Value::Number(frames as f64)),
        ("sampleRate".into(), Value::Number(sample_rate)),
        ("timestamp".into(), Value::Number(timestamp)),
    ]));
    let allocation_bytes = Rc::clone(&bytes);
    let allocation_closed = Rc::clone(&closed);
    value.set_property(
        "allocationSize",
        Value::function(move |_, _| {
            if allocation_closed.get() {
                w3cos_core::throw_value(w3cos_core::web::dom_exception_instance(
                    "AudioData is closed",
                    "InvalidStateError",
                ));
            }
            Value::Number(allocation_bytes.len() as f64)
        }),
    );
    let copy_bytes = Rc::clone(&bytes);
    let copy_closed = Rc::clone(&closed);
    value.set_property(
        "copyTo",
        Value::function(move |_, args| {
            if copy_closed.get() {
                w3cos_core::throw_value(w3cos_core::web::dom_exception_instance(
                    "AudioData is closed",
                    "InvalidStateError",
                ));
            }
            let destination = args.first().cloned().unwrap_or(Value::Undefined);
            let capacity = w3cos_core::binary::bytes_of(&destination)
                .map(|bytes| bytes.len())
                .unwrap_or_default();
            if capacity < copy_bytes.len() {
                w3cos_core::throw_value(w3cos_core::web::dom_exception_instance(
                    "Destination is too small",
                    "RangeError",
                ));
            }
            destination.call_method(
                "set",
                vec![w3cos_core::binary::typed_array_value(
                    copy_bytes
                        .iter()
                        .map(|byte| Value::Number(*byte as f64))
                        .collect(),
                )],
            );
            Value::Undefined
        }),
    );
    let clone_bytes = Rc::clone(&bytes);
    let clone_closed = Rc::clone(&closed);
    let clone_format = format.clone();
    value.set_property(
        "clone",
        Value::function(move |_, _| {
            if clone_closed.get() {
                w3cos_core::throw_value(w3cos_core::web::dom_exception_instance(
                    "AudioData is closed",
                    "InvalidStateError",
                ));
            }
            audio_data_value(Value::object(HashMap::from([
                (
                    "data".into(),
                    w3cos_core::binary::array_buffer_value((*clone_bytes).clone()),
                ),
                ("format".into(), Value::string(&clone_format)),
                ("numberOfChannels".into(), Value::Number(channels as f64)),
                ("numberOfFrames".into(), Value::Number(frames as f64)),
                ("sampleRate".into(), Value::Number(sample_rate)),
                ("timestamp".into(), Value::Number(timestamp)),
            ])))
        }),
    );
    value.set_property(
        "close",
        Value::function(move |_, _| {
            closed.set(true);
            Value::Undefined
        }),
    );
    w3cos_core::class::set_prototype_of(&value, &class_for("AudioData").get_property("prototype"));
    value
}

fn video_frame_value(source: Value, init: Value) -> Value {
    let Some(bytes) = w3cos_core::binary::bytes_of(&source) else {
        type_error(
            "VideoFrame currently requires BufferSource input; canvas/image sources need a renderer adapter",
        );
    };
    let format = init.get_property("format").to_js_string();
    if format.is_empty() || format == "undefined" {
        type_error("VideoFrame BufferSource input requires a pixel format");
    }
    let coded_width = init.get_property("codedWidth").to_u32();
    let coded_height = init.get_property("codedHeight").to_u32();
    if coded_width == 0 || coded_height == 0 {
        type_error("VideoFrame requires positive codedWidth and codedHeight");
    }
    let timestamp = init.get_property("timestamp").to_number();
    let duration = init.get_property("duration");
    let duration = if duration.is_undefined() {
        Value::Null
    } else {
        duration
    };
    let display_width = {
        let value = init.get_property("displayWidth");
        if value.is_undefined() {
            coded_width
        } else {
            value.to_u32()
        }
    };
    let display_height = {
        let value = init.get_property("displayHeight");
        if value.is_undefined() {
            coded_height
        } else {
            value.to_u32()
        }
    };
    let visible_init = init.get_property("visibleRect");
    let visible = if visible_init.is_undefined() {
        crate::geometry_web::rect(0.0, 0.0, coded_width as f64, coded_height as f64)
    } else {
        crate::geometry_web::rect(
            visible_init.get_property("x").to_number(),
            visible_init.get_property("y").to_number(),
            visible_init.get_property("width").to_number(),
            visible_init.get_property("height").to_number(),
        )
    };
    let coded = crate::geometry_web::rect(0.0, 0.0, coded_width as f64, coded_height as f64);
    let color_space = color_space_value(init.get_property("colorSpace"));
    let rotation = {
        let value = init.get_property("rotation");
        if value.is_undefined() {
            0
        } else {
            value.to_u32()
        }
    };
    if !matches!(rotation, 0 | 90 | 180 | 270) {
        type_error("VideoFrame rotation must be 0, 90, 180 or 270");
    }
    let flip = init.get_property("flip").to_bool();
    let bytes = Rc::new(bytes);
    let closed = Rc::new(Cell::new(false));
    let value = Value::object(HashMap::from([
        ("codedHeight".into(), Value::Number(coded_height as f64)),
        ("codedRect".into(), coded),
        ("codedWidth".into(), Value::Number(coded_width as f64)),
        ("colorSpace".into(), color_space),
        ("displayHeight".into(), Value::Number(display_height as f64)),
        ("displayWidth".into(), Value::Number(display_width as f64)),
        ("duration".into(), duration.clone()),
        ("flip".into(), Value::Bool(flip)),
        ("format".into(), Value::string(&format)),
        ("rotation".into(), Value::Number(rotation as f64)),
        ("timestamp".into(), Value::Number(timestamp)),
        ("visibleRect".into(), visible),
    ]));
    let allocation_bytes = Rc::clone(&bytes);
    let allocation_closed = Rc::clone(&closed);
    value.set_property(
        "allocationSize",
        Value::function(move |_, _| {
            if allocation_closed.get() {
                w3cos_core::throw_value(w3cos_core::web::dom_exception_instance(
                    "VideoFrame is closed",
                    "InvalidStateError",
                ));
            }
            Value::Number(allocation_bytes.len() as f64)
        }),
    );
    let copy_bytes = Rc::clone(&bytes);
    let copy_closed = Rc::clone(&closed);
    value.set_property(
        "copyTo",
        Value::function(move |_, args| {
            if copy_closed.get() {
                return w3cos_core::promise::reject(vec![w3cos_core::web::dom_exception_instance(
                    "VideoFrame is closed",
                    "InvalidStateError",
                )]);
            }
            let destination = args.first().cloned().unwrap_or(Value::Undefined);
            let capacity = w3cos_core::binary::bytes_of(&destination)
                .map(|bytes| bytes.len())
                .unwrap_or_default();
            if capacity < copy_bytes.len() {
                return w3cos_core::promise::reject(vec![w3cos_core::web::dom_exception_instance(
                    "Destination is too small",
                    "RangeError",
                )]);
            }
            if !args
                .get(1)
                .cloned()
                .unwrap_or(Value::Undefined)
                .is_undefined()
            {
                static WARNING: Once = Once::new();
                WARNING.call_once(|| {
                    eprintln!(
                        "[w3cos] warning: VideoFrame.copyTo preserves the stored raw plane bytes; \
                         format conversion, crop and custom layout require a host image adapter"
                    );
                });
            }
            destination.call_method(
                "set",
                vec![w3cos_core::binary::typed_array_value(
                    copy_bytes
                        .iter()
                        .map(|byte| Value::Number(*byte as f64))
                        .collect(),
                )],
            );
            w3cos_core::promise::resolve(vec![Value::array(vec![Value::object(HashMap::from([
                ("offset".into(), Value::Number(0.0)),
                (
                    "stride".into(),
                    Value::Number(copy_bytes.len() as f64 / coded_height as f64),
                ),
            ]))])])
        }),
    );
    let clone_bytes = Rc::clone(&bytes);
    let clone_closed = Rc::clone(&closed);
    let clone_format = format.clone();
    value.set_property(
        "clone",
        Value::function(move |_, _| {
            if clone_closed.get() {
                w3cos_core::throw_value(w3cos_core::web::dom_exception_instance(
                    "VideoFrame is closed",
                    "InvalidStateError",
                ));
            }
            video_frame_value(
                w3cos_core::binary::array_buffer_value((*clone_bytes).clone()),
                Value::object(HashMap::from([
                    ("codedHeight".into(), Value::Number(coded_height as f64)),
                    ("codedWidth".into(), Value::Number(coded_width as f64)),
                    ("displayHeight".into(), Value::Number(display_height as f64)),
                    ("displayWidth".into(), Value::Number(display_width as f64)),
                    ("duration".into(), duration.clone()),
                    ("flip".into(), Value::Bool(flip)),
                    ("format".into(), Value::string(&clone_format)),
                    ("rotation".into(), Value::Number(rotation as f64)),
                    ("timestamp".into(), Value::Number(timestamp)),
                ])),
            )
        }),
    );
    value.set_property(
        "close",
        Value::function(move |_, _| {
            closed.set(true);
            Value::Undefined
        }),
    );
    let metadata_format = format;
    value.set_property(
        "metadata",
        Value::function(move |_, _| {
            Value::object(HashMap::from([(
                "format".into(),
                Value::string(&metadata_format),
            )]))
        }),
    );
    w3cos_core::class::set_prototype_of(&value, &class_for("VideoFrame").get_property("prototype"));
    value
}

fn color_space_value(init: Value) -> Value {
    let value = Value::object(HashMap::new());
    for name in ["fullRange", "matrix", "primaries", "transfer"] {
        let property = init.get_property(name);
        value.set_property(
            name,
            if property.is_undefined() {
                Value::Null
            } else {
                property
            },
        );
    }
    let json = value.clone();
    value.set_property(
        "toJSON",
        Value::function(move |_, _| {
            Value::object(HashMap::from([
                ("fullRange".into(), json.get_property("fullRange")),
                ("matrix".into(), json.get_property("matrix")),
                ("primaries".into(), json.get_property("primaries")),
                ("transfer".into(), json.get_property("transfer")),
            ]))
        }),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &class_for("VideoColorSpace").get_property("prototype"),
    );
    value
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CodecKind {
    AudioDecoder,
    AudioEncoder,
    VideoDecoder,
    VideoEncoder,
}

impl CodecKind {
    fn name(self) -> &'static str {
        match self {
            Self::AudioDecoder => "AudioDecoder",
            Self::AudioEncoder => "AudioEncoder",
            Self::VideoDecoder => "VideoDecoder",
            Self::VideoEncoder => "VideoEncoder",
        }
    }

    fn queue_property(self) -> &'static str {
        match self {
            Self::AudioDecoder | Self::VideoDecoder => "decodeQueueSize",
            Self::AudioEncoder | Self::VideoEncoder => "encodeQueueSize",
        }
    }

    fn input_class(self) -> &'static str {
        match self {
            Self::AudioDecoder => "EncodedAudioChunk",
            Self::AudioEncoder => "AudioData",
            Self::VideoDecoder => "EncodedVideoChunk",
            Self::VideoEncoder => "VideoFrame",
        }
    }

    fn operation(self) -> &'static str {
        match self {
            Self::AudioDecoder | Self::VideoDecoder => "decode",
            Self::AudioEncoder | Self::VideoEncoder => "encode",
        }
    }
}

fn codec_kind(name: &str) -> Option<CodecKind> {
    match name {
        "AudioDecoder" => Some(CodecKind::AudioDecoder),
        "AudioEncoder" => Some(CodecKind::AudioEncoder),
        "VideoDecoder" => Some(CodecKind::VideoDecoder),
        "VideoEncoder" => Some(CodecKind::VideoEncoder),
        _ => None,
    }
}

fn codec_error(name: &str, message: &str) -> Value {
    w3cos_core::web::dom_exception_instance(message, name)
}

fn validate_codec_config(config: &Value) -> Result<(), Value> {
    if !config.is_object() {
        return Err(w3cos_core::error_instance(
            "TypeError",
            vec![Value::string("WebCodecs configuration must be an object")],
        ));
    }
    let codec = config.get_property("codec").to_js_string();
    if codec.trim().is_empty() || codec == "undefined" {
        return Err(w3cos_core::error_instance(
            "TypeError",
            vec![Value::string(
                "WebCodecs configuration requires a non-empty codec",
            )],
        ));
    }
    Ok(())
}

fn dispatch_dequeue(target: &Value) {
    let event = w3cos_core::class::construct(
        &crate::web_events::event_class(),
        vec![Value::string("dequeue")],
    );
    target.call_method("dispatchEvent", vec![event]);
}

fn codec_value(kind: CodecKind, init: Value) -> Value {
    if !init.is_object() {
        type_error(&format!("{} requires an init object", kind.name()));
    }
    let output = init.get_property("output");
    let error = init.get_property("error");
    if !output.is_function() || !error.is_function() {
        type_error(&format!(
            "{} requires callable output and error callbacks",
            kind.name()
        ));
    }

    let value = w3cos_core::class::construct(&crate::web_events::event_target_class(), Vec::new());
    w3cos_core::class::set_prototype_of(&value, &class_for(kind.name()).get_property("prototype"));
    value.set_property("state", Value::string("unconfigured"));
    value.set_property(kind.queue_property(), Value::Number(0.0));
    value.set_property("ondequeue", Value::Null);

    let generation = Rc::new(Cell::new(0_u64));

    let configure_generation = Rc::clone(&generation);
    value.set_property(
        "configure",
        Value::function(move |this, args| {
            if this.get_property("state").to_js_string() == "closed" {
                w3cos_core::throw_value(codec_error(
                    "InvalidStateError",
                    "Cannot configure a closed codec",
                ));
            }
            let config = args.first().cloned().unwrap_or(Value::Undefined);
            if let Err(error) = validate_codec_config(&config) {
                w3cos_core::throw_value(error);
            }
            configure_generation.set(configure_generation.get().wrapping_add(1));
            this.set_property(kind.queue_property(), Value::Number(0.0));
            this.set_property("state", Value::string("configured"));
            this.set_property("__w3cos_codec_config", config);
            Value::Undefined
        }),
    );

    let operation_generation = Rc::clone(&generation);
    let error_callback = error.clone();
    value.set_property(
        kind.operation(),
        Value::function(move |this, args| {
            if this.get_property("state").to_js_string() != "configured" {
                w3cos_core::throw_value(codec_error(
                    "InvalidStateError",
                    &format!("{} requires a configured codec", kind.operation()),
                ));
            }
            let input = args.first().cloned().unwrap_or(Value::Undefined);
            if !w3cos_core::class::instance_of(&input, &class_for(kind.input_class())) {
                type_error(&format!(
                    "{}.{} requires a {}",
                    kind.name(),
                    kind.operation(),
                    kind.input_class()
                ));
            }
            let queue_size = this.get_property(kind.queue_property()).to_u32() + 1;
            this.set_property(kind.queue_property(), Value::Number(queue_size as f64));
            let operation_id = operation_generation.get();
            let generation_for_task = Rc::clone(&operation_generation);
            let callback_for_task = error_callback.clone();
            let target = this.clone();
            crate::jsdom::queue_microtask_value(Value::function(move |_, _| {
                if generation_for_task.get() != operation_id
                    || target.get_property("state").to_js_string() == "closed"
                {
                    return Value::Undefined;
                }
                let remaining = target
                    .get_property(kind.queue_property())
                    .to_u32()
                    .saturating_sub(1);
                target.set_property(kind.queue_property(), Value::Number(remaining as f64));
                dispatch_dequeue(&target);
                static WARNING: Once = Once::new();
                WARNING.call_once(|| {
                    eprintln!(
                        "[w3cos] warning: WebCodecs codec controllers preserve configuration, \
                         queue and error lifecycles, but encoded media processing requires a \
                         registered native codec adapter"
                    );
                });
                target.set_property(kind.queue_property(), Value::Number(0.0));
                target.set_property("state", Value::string("closed"));
                generation_for_task.set(generation_for_task.get().wrapping_add(1));
                callback_for_task.call(
                    Value::Undefined,
                    vec![codec_error(
                        "NotSupportedError",
                        "No native WebCodecs codec adapter is registered",
                    )],
                );
                Value::Undefined
            }));
            Value::Undefined
        }),
    );

    value.set_property(
        "flush",
        Value::function(move |this, _| {
            if this.get_property("state").to_js_string() == "closed" {
                return w3cos_core::promise::reject(vec![codec_error(
                    "InvalidStateError",
                    "Cannot flush a closed codec",
                )]);
            }
            if this.get_property("state").to_js_string() != "configured" {
                return w3cos_core::promise::reject(vec![codec_error(
                    "InvalidStateError",
                    "Cannot flush an unconfigured codec",
                )]);
            }
            if this.get_property(kind.queue_property()).to_u32() > 0 {
                return w3cos_core::promise::reject(vec![codec_error(
                    "NotSupportedError",
                    "Queued media cannot be processed without a native codec adapter",
                )]);
            }
            w3cos_core::promise::resolve(vec![Value::Undefined])
        }),
    );

    let reset_generation = Rc::clone(&generation);
    value.set_property(
        "reset",
        Value::function(move |this, _| {
            if this.get_property("state").to_js_string() == "closed" {
                w3cos_core::throw_value(codec_error(
                    "InvalidStateError",
                    "Cannot reset a closed codec",
                ));
            }
            reset_generation.set(reset_generation.get().wrapping_add(1));
            this.set_property(kind.queue_property(), Value::Number(0.0));
            this.set_property("state", Value::string("unconfigured"));
            Value::Undefined
        }),
    );

    value.set_property(
        "close",
        Value::function(move |this, _| {
            generation.set(generation.get().wrapping_add(1));
            this.set_property(kind.queue_property(), Value::Number(0.0));
            this.set_property("state", Value::string("closed"));
            Value::Undefined
        }),
    );
    value
}

fn codec_class(kind: CodecKind) -> Value {
    let class = Value::function(move |_, args| {
        codec_value(kind, args.first().cloned().unwrap_or(Value::Undefined))
    });
    class.set_property("name", Value::string(kind.name()));
    class.set_property(
        "isConfigSupported",
        Value::function(move |_, args| {
            let config = args.first().cloned().unwrap_or(Value::Undefined);
            if let Err(error) = validate_codec_config(&config) {
                return w3cos_core::promise::reject(vec![error]);
            }
            static WARNING: Once = Once::new();
            WARNING.call_once(|| {
                eprintln!(
                    "[w3cos] warning: {}.isConfigSupported returns supported=false until a \
                     native codec adapter is registered",
                    kind.name()
                );
            });
            w3cos_core::promise::resolve(vec![Value::object(HashMap::from([
                ("config".into(), config),
                ("supported".into(), Value::Bool(false)),
            ]))])
        }),
    );
    let prototype = Value::object(HashMap::new());
    prototype.set_property("constructor", class.clone());
    for member in match kind {
        CodecKind::AudioDecoder | CodecKind::VideoDecoder => &[
            "close",
            "configure",
            "decode",
            "decodeQueueSize",
            "flush",
            "ondequeue",
            "reset",
            "state",
        ][..],
        CodecKind::AudioEncoder | CodecKind::VideoEncoder => &[
            "close",
            "configure",
            "encode",
            "encodeQueueSize",
            "flush",
            "ondequeue",
            "reset",
            "state",
        ][..],
    } {
        prototype.set_property(member, Value::Undefined);
    }
    w3cos_core::class::set_prototype_of(
        &prototype,
        &crate::web_events::event_target_class().get_property("prototype"),
    );
    class.set_property("prototype", prototype);
    class
}

fn build_class(name: &'static str) -> Value {
    if let Some(kind) = codec_kind(name) {
        return codec_class(kind);
    }
    let class = match name {
        "AudioData" => Value::function(|_, args| {
            audio_data_value(args.first().cloned().unwrap_or(Value::Undefined))
        }),
        "VideoFrame" => Value::function(|_, args| {
            video_frame_value(
                args.first().cloned().unwrap_or(Value::Undefined),
                args.get(1).cloned().unwrap_or(Value::Undefined),
            )
        }),
        "EncodedAudioChunk" | "EncodedVideoChunk" => Value::function(move |_, args| {
            chunk_value(name, args.first().cloned().unwrap_or(Value::Undefined))
        }),
        "VideoColorSpace" => Value::function(|_, args| {
            color_space_value(args.first().cloned().unwrap_or(Value::Undefined))
        }),
        _ => unreachable!(),
    };
    class.set_property("name", Value::string(name));
    let prototype = Value::object(HashMap::new());
    prototype.set_property("constructor", class.clone());
    let members: &[&str] = match name {
        "AudioData" => &[
            "allocationSize",
            "clone",
            "close",
            "copyTo",
            "duration",
            "format",
            "numberOfChannels",
            "numberOfFrames",
            "sampleRate",
            "timestamp",
        ],
        "VideoFrame" => &[
            "allocationSize",
            "clone",
            "close",
            "codedHeight",
            "codedRect",
            "codedWidth",
            "colorSpace",
            "copyTo",
            "displayHeight",
            "displayWidth",
            "duration",
            "flip",
            "format",
            "metadata",
            "rotation",
            "timestamp",
            "visibleRect",
        ],
        "EncodedAudioChunk" | "EncodedVideoChunk" => {
            &["byteLength", "copyTo", "duration", "timestamp", "type"]
        }
        "VideoColorSpace" => &["fullRange", "matrix", "primaries", "toJSON", "transfer"],
        _ => &[],
    };
    for member in members {
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
    use super::*;

    #[test]
    fn encoded_chunks_copy_bytes_without_a_codec() {
        let chunk = w3cos_core::class::construct(
            &class_for("EncodedVideoChunk"),
            vec![Value::object(HashMap::from([
                ("type".into(), Value::string("key")),
                ("timestamp".into(), Value::Number(10.0)),
                (
                    "data".into(),
                    w3cos_core::binary::typed_array_value(vec![
                        Value::Number(1.0),
                        Value::Number(2.0),
                    ]),
                ),
            ]))],
        );
        let destination =
            w3cos_core::binary::typed_array_value(vec![Value::Number(0.0), Value::Number(0.0)]);
        chunk.call_method("copyTo", vec![destination.clone()]);
        assert_eq!(
            w3cos_core::binary::bytes_of(&destination).unwrap(),
            vec![1, 2]
        );
    }

    #[test]
    fn audio_data_copies_clones_and_closes_independently() {
        let audio = w3cos_core::class::construct(
            &class_for("AudioData"),
            vec![Value::object(HashMap::from([
                (
                    "data".into(),
                    w3cos_core::binary::typed_array_value(vec![
                        Value::Number(1.0),
                        Value::Number(2.0),
                        Value::Number(3.0),
                        Value::Number(4.0),
                    ]),
                ),
                ("format".into(), Value::string("u8")),
                ("numberOfChannels".into(), Value::Number(1.0)),
                ("numberOfFrames".into(), Value::Number(4.0)),
                ("sampleRate".into(), Value::Number(8_000.0)),
                ("timestamp".into(), Value::Number(25.0)),
            ]))],
        );
        assert_eq!(audio.call_method("allocationSize", vec![]).to_number(), 4.0);
        assert_eq!(audio.get_property("duration").to_number(), 500.0);
        let clone = audio.call_method("clone", vec![]);
        assert!(w3cos_core::class::instance_of(
            &clone,
            &class_for("AudioData")
        ));
        audio.call_method("close", vec![]);
        let destination = w3cos_core::binary::typed_array_value(vec![
            Value::Number(0.0),
            Value::Number(0.0),
            Value::Number(0.0),
            Value::Number(0.0),
        ]);
        clone.call_method("copyTo", vec![destination.clone()]);
        assert_eq!(
            w3cos_core::binary::bytes_of(&destination).unwrap(),
            vec![1, 2, 3, 4]
        );
    }

    #[test]
    fn video_frame_preserves_raw_pixels_geometry_and_clone_lifecycle() {
        let frame = w3cos_core::class::construct(
            &class_for("VideoFrame"),
            vec![
                w3cos_core::binary::typed_array_value(
                    (0..16).map(|value| Value::Number(value as f64)).collect(),
                ),
                Value::object(HashMap::from([
                    ("codedHeight".into(), Value::Number(2.0)),
                    ("codedWidth".into(), Value::Number(2.0)),
                    ("format".into(), Value::string("RGBA")),
                    ("timestamp".into(), Value::Number(1_000.0)),
                ])),
            ],
        );
        assert!(w3cos_core::class::instance_of(
            &frame,
            &class_for("VideoFrame")
        ));
        assert_eq!(
            frame
                .get_property("codedRect")
                .get_property("width")
                .to_number(),
            2.0
        );
        assert_eq!(
            frame.call_method("allocationSize", vec![]).to_number(),
            16.0
        );
        let clone = frame.call_method("clone", vec![]);
        frame.call_method("close", vec![]);
        let destination =
            w3cos_core::binary::typed_array_value((0..16).map(|_| Value::Number(0.0)).collect());
        let layouts = Rc::new(RefCell::new(Value::Undefined));
        let layouts_for_callback = Rc::clone(&layouts);
        clone
            .call_method("copyTo", vec![destination.clone()])
            .call_method(
                "then",
                vec![Value::function(move |_, args| {
                    *layouts_for_callback.borrow_mut() = args[0].clone();
                    Value::Undefined
                })],
            );
        crate::jsdom::drain_microtasks();
        assert_eq!(
            w3cos_core::binary::bytes_of(&destination).unwrap(),
            (0..16).collect::<Vec<_>>()
        );
        assert_eq!(
            layouts
                .borrow()
                .get_property("0")
                .get_property("stride")
                .to_number(),
            8.0
        );
    }

    #[test]
    fn codec_controllers_validate_config_and_report_missing_native_adapter() {
        let errors = Rc::new(RefCell::new(Vec::<String>::new()));
        let errors_for_callback = Rc::clone(&errors);
        let decoder = w3cos_core::class::construct(
            &class_for("VideoDecoder"),
            vec![Value::object(HashMap::from([
                ("output".into(), Value::function(|_, _| Value::Undefined)),
                (
                    "error".into(),
                    Value::function(move |_, args| {
                        errors_for_callback.borrow_mut().push(
                            args.first()
                                .cloned()
                                .unwrap_or(Value::Undefined)
                                .get_property("name")
                                .to_js_string(),
                        );
                        Value::Undefined
                    }),
                ),
            ]))],
        );
        assert!(w3cos_core::class::instance_of(
            &decoder,
            &class_for("VideoDecoder")
        ));
        assert!(w3cos_core::class::instance_of(
            &decoder,
            &crate::web_events::event_target_class()
        ));
        assert_eq!(decoder.get_property("state").to_js_string(), "unconfigured");

        decoder.call_method(
            "configure",
            vec![Value::object(HashMap::from([(
                "codec".into(),
                Value::string("vp09.00.10.08"),
            )]))],
        );
        let chunk = w3cos_core::class::construct(
            &class_for("EncodedVideoChunk"),
            vec![Value::object(HashMap::from([
                ("type".into(), Value::string("key")),
                ("timestamp".into(), Value::Number(0.0)),
                (
                    "data".into(),
                    w3cos_core::binary::typed_array_value(vec![Value::Number(1.0)]),
                ),
            ]))],
        );
        decoder.call_method("decode", vec![chunk]);
        assert_eq!(decoder.get_property("decodeQueueSize").to_number(), 1.0);
        crate::jsdom::drain_microtasks();
        assert_eq!(decoder.get_property("decodeQueueSize").to_number(), 0.0);
        assert_eq!(decoder.get_property("state").to_js_string(), "closed");
        assert_eq!(&*errors.borrow(), &["NotSupportedError"]);
    }

    #[test]
    fn codec_support_query_is_truthful_without_an_adapter() {
        let result = Rc::new(RefCell::new(Value::Undefined));
        let result_for_callback = Rc::clone(&result);
        class_for("AudioEncoder")
            .call_method(
                "isConfigSupported",
                vec![Value::object(HashMap::from([(
                    "codec".into(),
                    Value::string("opus"),
                )]))],
            )
            .call_method(
                "then",
                vec![Value::function(move |_, args| {
                    *result_for_callback.borrow_mut() =
                        args.first().cloned().unwrap_or(Value::Undefined);
                    Value::Undefined
                })],
            );
        crate::jsdom::drain_microtasks();
        assert!(!result.borrow().get_property("supported").to_bool());
        assert_eq!(
            result
                .borrow()
                .get_property("config")
                .get_property("codec")
                .to_js_string(),
            "opus"
        );
    }
}
