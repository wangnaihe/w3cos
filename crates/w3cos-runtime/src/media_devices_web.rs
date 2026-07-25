//! Browser-shaped MediaDevices, MediaStream, and MediaStreamTrack facades.
//!
//! Native hosts register devices through [`set_devices`]. The facade remains
//! standards-shaped when no adapter is present by rejecting `getUserMedia`
//! with a named DOM-style error.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Once;

use w3cos_core::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaDevice {
    pub device_id: String,
    pub kind: String,
    pub label: String,
    pub group_id: String,
}

struct TrackProcessorState {
    controller: RefCell<Value>,
    total_frames: Cell<u64>,
    discarded_frames: Cell<u64>,
}

#[derive(Default)]
struct TrackGeneratorState {
    processors: Vec<Rc<TrackProcessorState>>,
}

thread_local! {
    static DEVICES: RefCell<Vec<MediaDevice>> = const { RefCell::new(Vec::new()) };
    static PERMISSION_DENIED: Cell<bool> = const { Cell::new(false) };
    static NEXT_TRACK_ID: Cell<u64> = const { Cell::new(1) };
    static MEDIA_STREAM_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static MEDIA_STREAM_TRACK_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static MEDIA_DEVICE_INFO_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static INPUT_DEVICE_INFO_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static MEDIA_DEVICES_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static OVERCONSTRAINED_ERROR_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static MEDIA_STREAM_TRACK_GENERATOR_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static MEDIA_STREAM_TRACK_PROCESSOR_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static MEDIA_STATS_CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static TRACK_GENERATORS: RefCell<HashMap<String, Rc<RefCell<TrackGeneratorState>>>> =
        RefCell::new(HashMap::new());
}

fn media_stats_members(name: &str) -> &'static [&'static str] {
    match name {
        "MediaStreamTrackAudioStats" => &[
            "averageLatency",
            "deliveredFrames",
            "deliveredFramesDuration",
            "latency",
            "maximumLatency",
            "minimumLatency",
            "resetLatency",
            "totalFrames",
            "totalFramesDuration",
        ],
        "MediaStreamTrackVideoStats" => &["deliveredFrames", "discardedFrames", "totalFrames"],
        "AudioPlaybackStats" => &[
            "averageLatency",
            "maximumLatency",
            "minimumLatency",
            "resetLatency",
            "totalDuration",
            "underrunDuration",
            "underrunEvents",
        ],
        _ => &[],
    }
}

pub fn media_stats_class(name: &str) -> Value {
    MEDIA_STATS_CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        let Some(name) = [
            "AudioPlaybackStats",
            "MediaStreamTrackAudioStats",
            "MediaStreamTrackVideoStats",
        ]
        .into_iter()
        .find(|candidate| candidate == &name) else {
            return Value::Undefined;
        };
        let class = Value::function(move |_, _| {
            w3cos_core::throw_value(named_error(
                "TypeError",
                &format!("Illegal constructor: {name}"),
            ))
        });
        class.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in media_stats_members(name) {
            prototype.set_property(member, Value::Undefined);
        }
        prototype.set_property("toJSON", Value::Undefined);
        class.set_property("prototype", prototype);
        classes.borrow_mut().insert(name.to_string(), class.clone());
        class
    })
}

pub fn media_stats_value(name: &str) -> Value {
    let stats = Value::object(
        media_stats_members(name)
            .iter()
            .filter(|member| **member != "resetLatency")
            .map(|member| ((*member).to_string(), Value::Number(0.0)))
            .collect(),
    );
    if media_stats_members(name).contains(&"resetLatency") {
        stats.set_property(
            "resetLatency",
            Value::function(|this, _| {
                for member in [
                    "averageLatency",
                    "latency",
                    "maximumLatency",
                    "minimumLatency",
                ] {
                    if !this.get_property(member).is_undefined() {
                        this.set_property(member, Value::Number(0.0));
                    }
                }
                Value::Undefined
            }),
        );
    }
    let stats_for_json = stats.clone();
    let class_name = name.to_string();
    stats.set_property(
        "toJSON",
        Value::function(move |_, _| {
            Value::object(
                media_stats_members(&class_name)
                    .iter()
                    .filter(|member| **member != "resetLatency")
                    .map(|member| ((*member).to_string(), stats_for_json.get_property(member)))
                    .collect(),
            )
        }),
    );
    w3cos_core::class::set_prototype_of(&stats, &media_stats_class(name).get_property("prototype"));
    stats
}

fn named_error(name: &str, message: &str) -> Value {
    let error = w3cos_core::class::construct(
        &crate::unsupported::dom_exception_class(),
        vec![Value::string(message), Value::string(name)],
    );
    error.set_property("code", Value::Number(0.0));
    error
}

pub fn overconstrained_error_class() -> Value {
    OVERCONSTRAINED_ERROR_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|this, args| {
            let constraint = args.first().map(Value::to_js_string).unwrap_or_default();
            let message = args.get(1).map(Value::to_js_string).unwrap_or_default();
            crate::unsupported::dom_exception_class().call(
                this.clone(),
                vec![
                    Value::string(&message),
                    Value::string("OverconstrainedError"),
                ],
            );
            this.set_property("constraint", Value::string(&constraint));
            Value::Undefined
        });
        class.set_property("name", Value::string("OverconstrainedError"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property("constraint", Value::Undefined);
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::unsupported::dom_exception_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn overconstrained_error(constraint: &str, message: &str) -> Value {
    w3cos_core::class::construct(
        &overconstrained_error_class(),
        vec![Value::string(constraint), Value::string(message)],
    )
}

fn simple_class(
    slot: &'static std::thread::LocalKey<RefCell<Option<Value>>>,
    name: &'static str,
) -> Value {
    slot.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| Value::Undefined);
        class.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn media_devices_class() -> Value {
    MEDIA_DEVICES_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            w3cos_core::throw_value(named_error(
                "TypeError",
                "Illegal constructor: MediaDevices",
            ))
        });
        class.set_property("name", Value::string("MediaDevices"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for property in [
            "enumerateDevices",
            "getDisplayMedia",
            "getSupportedConstraints",
            "getUserMedia",
            "ondevicechange",
            "setCaptureHandleConfig",
        ] {
            prototype.set_property(property, Value::Undefined);
        }
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::web_events::event_target_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn media_stream_track_class() -> Value {
    let class = simple_class(&MEDIA_STREAM_TRACK_CLASS, "MediaStreamTrack");
    for member in [
        "applyConstraints",
        "clone",
        "contentHint",
        "enabled",
        "getCapabilities",
        "getCaptureHandle",
        "getConstraints",
        "getSettings",
        "id",
        "kind",
        "label",
        "muted",
        "oncapturehandlechange",
        "onended",
        "onmute",
        "onunmute",
        "readyState",
        "stats",
        "stop",
    ] {
        class
            .get_property("prototype")
            .set_property(member, Value::Undefined);
    }
    class
}

fn media_stream_track_generator_value(options: Value) -> Value {
    let kind = options.get_property("kind").to_js_string();
    if !matches!(kind.as_str(), "audio" | "video") {
        w3cos_core::throw_value(named_error(
            "TypeError",
            "MediaStreamTrackGenerator kind must be 'audio' or 'video'",
        ));
    }
    let track = track_value(&kind, "MediaStreamTrackGenerator");
    let id = track.get_property("id").to_js_string();
    let state = Rc::new(RefCell::new(TrackGeneratorState::default()));
    TRACK_GENERATORS.with(|generators| {
        generators.borrow_mut().insert(id, Rc::clone(&state));
    });
    let write_state = Rc::clone(&state);
    let write_stats = track.get_property("stats");
    let write_kind = kind;
    let sink = Value::object(HashMap::from([(
        "write".into(),
        Value::function(move |_, args| {
            let frame = args.first().cloned().unwrap_or(Value::Undefined);
            let expected = if write_kind == "audio" {
                crate::webcodecs_web::class_for("AudioData")
            } else {
                crate::webcodecs_web::class_for("VideoFrame")
            };
            if !w3cos_core::class::instance_of(&frame, &expected) {
                w3cos_core::throw_value(named_error(
                    "TypeError",
                    if write_kind == "audio" {
                        "Audio track generators accept AudioData"
                    } else {
                        "Video track generators accept VideoFrame"
                    },
                ));
            }
            for member in ["totalFrames", "deliveredFrames"] {
                write_stats.set_property(
                    member,
                    Value::Number(write_stats.get_property(member).to_number() + 1.0),
                );
            }
            if write_kind == "audio" {
                let duration = frame.get_property("duration").to_number() / 1_000_000.0;
                for member in ["totalFramesDuration", "deliveredFramesDuration"] {
                    write_stats.set_property(
                        member,
                        Value::Number(write_stats.get_property(member).to_number() + duration),
                    );
                }
            }
            for processor in &write_state.borrow().processors {
                let controller = processor.controller.borrow().clone();
                if controller.is_undefined() {
                    processor
                        .discarded_frames
                        .set(processor.discarded_frames.get().saturating_add(1));
                } else {
                    controller.call_method("enqueue", vec![frame.clone()]);
                    processor
                        .total_frames
                        .set(processor.total_frames.get().saturating_add(1));
                }
            }
            Value::Undefined
        }),
    )]));
    track.set_property(
        "writable",
        w3cos_core::class::construct(&crate::streams_web::writable_stream_class(), vec![sink]),
    );
    w3cos_core::class::set_prototype_of(
        &track,
        &media_stream_track_generator_class().get_property("prototype"),
    );
    track
}

pub fn media_stream_track_generator_class() -> Value {
    MEDIA_STREAM_TRACK_GENERATOR_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, args| {
            media_stream_track_generator_value(args.first().cloned().unwrap_or(Value::Undefined))
        });
        class.set_property("name", Value::string("MediaStreamTrackGenerator"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property("writable", Value::Undefined);
        w3cos_core::class::set_prototype_of(
            &prototype,
            &media_stream_track_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn media_stream_track_processor_value(options: Value) -> Value {
    let track = options.get_property("track");
    if !w3cos_core::class::instance_of(&track, &media_stream_track_class()) {
        w3cos_core::throw_value(named_error(
            "TypeError",
            "MediaStreamTrackProcessor requires a MediaStreamTrack",
        ));
    }
    let state = Rc::new(TrackProcessorState {
        controller: RefCell::new(Value::Undefined),
        total_frames: Cell::new(0),
        discarded_frames: Cell::new(0),
    });
    let start_state = Rc::clone(&state);
    let source = Value::object(HashMap::from([(
        "start".into(),
        Value::function(move |_, args| {
            *start_state.controller.borrow_mut() =
                args.first().cloned().unwrap_or(Value::Undefined);
            Value::Undefined
        }),
    )]));
    let readable =
        w3cos_core::class::construct(&crate::streams_web::readable_stream_class(), vec![source]);
    let id = track.get_property("id").to_js_string();
    let registered = TRACK_GENERATORS.with(|generators| {
        let generator = generators.borrow().get(&id).cloned();
        if let Some(generator) = generator {
            generator.borrow_mut().processors.push(Rc::clone(&state));
            true
        } else {
            false
        }
    });
    if !registered {
        static WARNING: Once = Once::new();
        WARNING.call_once(|| {
            eprintln!(
                "[w3cos] warning: MediaStreamTrackProcessor exposes an empty readable stream for \
                 host capture tracks until a native decoded-frame adapter is connected"
            );
        });
    }
    let total_state = Rc::clone(&state);
    let discarded_state = Rc::clone(&state);
    let processor = Value::object(HashMap::from([
        ("readable".into(), readable),
        (
            "__w3cos_getter_totalFrames".into(),
            Value::function(move |_, _| Value::Number(total_state.total_frames.get() as f64)),
        ),
        (
            "__w3cos_getter_discardedFrames".into(),
            Value::function(move |_, _| {
                Value::Number(discarded_state.discarded_frames.get() as f64)
            }),
        ),
    ]));
    w3cos_core::class::set_prototype_of(
        &processor,
        &media_stream_track_processor_class().get_property("prototype"),
    );
    processor
}

pub fn media_stream_track_processor_class() -> Value {
    MEDIA_STREAM_TRACK_PROCESSOR_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, args| {
            media_stream_track_processor_value(args.first().cloned().unwrap_or(Value::Undefined))
        });
        class.set_property("name", Value::string("MediaStreamTrackProcessor"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in ["discardedFrames", "readable", "totalFrames"] {
            prototype.set_property(member, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn media_device_info_class() -> Value {
    MEDIA_DEVICE_INFO_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            w3cos_core::throw_value(named_error(
                "TypeError",
                "Illegal constructor: MediaDeviceInfo",
            ))
        });
        class.set_property("name", Value::string("MediaDeviceInfo"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for property in ["deviceId", "groupId", "kind", "label"] {
            prototype.set_property(property, Value::Undefined);
        }
        prototype.set_property(
            "toJSON",
            Value::function(|this, _| {
                Value::object(HashMap::from([
                    ("deviceId".to_string(), this.get_property("deviceId")),
                    ("kind".to_string(), this.get_property("kind")),
                    ("label".to_string(), this.get_property("label")),
                    ("groupId".to_string(), this.get_property("groupId")),
                ]))
            }),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn input_device_info_class() -> Value {
    INPUT_DEVICE_INFO_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            w3cos_core::throw_value(named_error(
                "TypeError",
                "Illegal constructor: InputDeviceInfo",
            ))
        });
        class.set_property("name", Value::string("InputDeviceInfo"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property(
            "getCapabilities",
            Value::function(|_, _| Value::object(HashMap::new())),
        );
        w3cos_core::class::set_prototype_of(
            &prototype,
            &media_device_info_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn next_track_id() -> String {
    NEXT_TRACK_ID.with(|next| {
        let id = next.get();
        next.set(id.saturating_add(1));
        format!("w3cos-track-{id}")
    })
}

pub(crate) fn track_value(kind: &str, label: &str) -> Value {
    let stats = media_stats_value(if kind == "audio" {
        "MediaStreamTrackAudioStats"
    } else {
        "MediaStreamTrackVideoStats"
    });
    let value = Value::object(HashMap::from([
        ("kind".to_string(), Value::string(kind)),
        ("id".to_string(), Value::string(&next_track_id())),
        ("label".to_string(), Value::string(label)),
        ("enabled".to_string(), Value::Bool(true)),
        ("muted".to_string(), Value::Bool(false)),
        ("readyState".to_string(), Value::string("live")),
        ("contentHint".to_string(), Value::string("")),
        ("onended".to_string(), Value::Null),
        ("onmute".to_string(), Value::Null),
        ("onunmute".to_string(), Value::Null),
        ("stats".to_string(), stats),
    ]));
    crate::web_events::event_target_class().call(value.clone(), vec![]);
    value.set_property(
        "stop",
        Value::function(|this, _| {
            if this.get_property("readyState").to_js_string() != "ended" {
                this.set_property("readyState", Value::string("ended"));
                let event = w3cos_core::class::construct(
                    &crate::web_events::event_class(),
                    vec![Value::string("ended")],
                );
                this.call_method("dispatchEvent", vec![event]);
            }
            Value::Undefined
        }),
    );
    value.set_property(
        "clone",
        Value::function(|this, _| {
            track_value(
                &this.get_property("kind").to_js_string(),
                &this.get_property("label").to_js_string(),
            )
        }),
    );
    value.set_property(
        "getCapabilities",
        Value::function(|_, _| Value::object(HashMap::new())),
    );
    value.set_property(
        "getConstraints",
        Value::function(|_, _| Value::object(HashMap::new())),
    );
    value.set_property(
        "getSettings",
        Value::function(|this, _| {
            Value::object(HashMap::from([(
                "deviceId".to_string(),
                this.get_property("id"),
            )]))
        }),
    );
    value.set_property(
        "applyConstraints",
        Value::function(|_, _| w3cos_core::promise::resolve(vec![Value::Undefined])),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &media_stream_track_class().get_property("prototype"),
    );
    value
}

fn tracks_from(value: &Value) -> Vec<Value> {
    value
        .get_property("__tracks")
        .iter()
        .collect::<Vec<Value>>()
}

pub(crate) fn stream_value(tracks: Vec<Value>) -> Value {
    let value = Value::object(HashMap::from([
        (
            "id".to_string(),
            Value::string(&format!("w3cos-stream-{}", next_track_id())),
        ),
        ("onaddtrack".to_string(), Value::Null),
        ("onremovetrack".to_string(), Value::Null),
        ("__tracks".to_string(), Value::array(tracks)),
    ]));
    crate::web_events::event_target_class().call(value.clone(), vec![]);
    value.set_property(
        "__w3cos_getter_active",
        Value::function(|this, _| {
            Value::Bool(
                tracks_from(&this)
                    .iter()
                    .any(|track| track.get_property("readyState").to_js_string() == "live"),
            )
        }),
    );
    value.set_property(
        "getTracks",
        Value::function(|this, _| Value::array(tracks_from(&this))),
    );
    for (method, kind) in [("getAudioTracks", "audio"), ("getVideoTracks", "video")] {
        value.set_property(
            method,
            Value::function(move |this, _| {
                Value::array(
                    tracks_from(&this)
                        .into_iter()
                        .filter(|track| track.get_property("kind").to_js_string() == kind)
                        .collect(),
                )
            }),
        );
    }
    value.set_property(
        "getTrackById",
        Value::function(|this, args| {
            let id = args.first().cloned().unwrap_or_default().to_js_string();
            tracks_from(&this)
                .into_iter()
                .find(|track| track.get_property("id").to_js_string() == id)
                .unwrap_or(Value::Null)
        }),
    );
    value.set_property(
        "addTrack",
        Value::function(|this, args| {
            let track = args.first().cloned().unwrap_or_default();
            let mut tracks = tracks_from(&this);
            if !tracks.iter().any(|existing| existing.strict_eq(&track)) {
                tracks.push(track.clone());
                this.set_property("__tracks", Value::array(tracks));
                let event = w3cos_core::class::construct(
                    &crate::web_events::event_class(),
                    vec![Value::string("addtrack")],
                );
                event.set_property("track", track);
                this.call_method("dispatchEvent", vec![event]);
            }
            Value::Undefined
        }),
    );
    value.set_property(
        "removeTrack",
        Value::function(|this, args| {
            let track = args.first().cloned().unwrap_or_default();
            let mut tracks = tracks_from(&this);
            let before = tracks.len();
            tracks.retain(|existing| !existing.strict_eq(&track));
            if tracks.len() != before {
                this.set_property("__tracks", Value::array(tracks));
                let event = w3cos_core::class::construct(
                    &crate::web_events::event_class(),
                    vec![Value::string("removetrack")],
                );
                event.set_property("track", track);
                this.call_method("dispatchEvent", vec![event]);
            }
            Value::Undefined
        }),
    );
    value.set_property(
        "clone",
        Value::function(|this, _| {
            stream_value(
                tracks_from(&this)
                    .into_iter()
                    .map(|track| track.call_method("clone", vec![]))
                    .collect(),
            )
        }),
    );
    w3cos_core::class::set_prototype_of(&value, &media_stream_class().get_property("prototype"));
    value
}

pub fn media_stream_class() -> Value {
    MEDIA_STREAM_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, args| {
            let tracks = args
                .first()
                .map(|value| value.iter().collect())
                .unwrap_or_default();
            stream_value(tracks)
        });
        class.set_property("name", Value::string("MediaStream"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in [
            "active",
            "addTrack",
            "clone",
            "getAudioTracks",
            "getTrackById",
            "getTracks",
            "getVideoTracks",
            "id",
            "onactive",
            "onaddtrack",
            "oninactive",
            "onremovetrack",
            "removeTrack",
        ] {
            prototype.set_property(member, Value::Undefined);
        }
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::web_events::event_target_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn device_value(device: &MediaDevice) -> Value {
    let value = Value::object(HashMap::from([
        ("deviceId".to_string(), Value::string(&device.device_id)),
        ("kind".to_string(), Value::string(&device.kind)),
        ("label".to_string(), Value::string(&device.label)),
        ("groupId".to_string(), Value::string(&device.group_id)),
    ]));
    value.set_property(
        "toJSON",
        Value::function(|this, _| {
            Value::object(HashMap::from([
                ("deviceId".to_string(), this.get_property("deviceId")),
                ("kind".to_string(), this.get_property("kind")),
                ("label".to_string(), this.get_property("label")),
                ("groupId".to_string(), this.get_property("groupId")),
            ]))
        }),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &if matches!(device.kind.as_str(), "audioinput" | "videoinput") {
            value.set_property(
                "getCapabilities",
                Value::function(|_, _| Value::object(HashMap::new())),
            );
            input_device_info_class().get_property("prototype")
        } else {
            media_device_info_class().get_property("prototype")
        },
    );
    value
}

fn requested(constraints: &Value, name: &str) -> bool {
    let value = constraints.get_property(name);
    !value.is_undefined() && !value.is_null() && value.to_bool()
}

fn exact_device_id(constraint: Value) -> Option<String> {
    if !constraint.is_object() {
        return None;
    }
    let device_id = constraint.get_property("deviceId");
    if device_id.is_object() {
        let exact = device_id.get_property("exact");
        (!exact.is_undefined()).then(|| exact.to_js_string())
    } else {
        None
    }
}

fn get_user_media(constraints: Value) -> Value {
    if PERMISSION_DENIED.with(Cell::get) {
        return w3cos_core::promise::reject(vec![named_error(
            "NotAllowedError",
            "Camera or microphone permission denied",
        )]);
    }
    let wants_audio = requested(&constraints, "audio");
    let wants_video = requested(&constraints, "video");
    if !wants_audio && !wants_video {
        return w3cos_core::promise::reject(vec![named_error(
            "TypeError",
            "At least one of audio or video must be requested",
        )]);
    }
    let devices = DEVICES.with(|devices| devices.borrow().clone());
    let mut tracks = Vec::new();
    for (requested, device_kind, track_kind) in [
        (wants_audio, "audioinput", "audio"),
        (wants_video, "videoinput", "video"),
    ] {
        if !requested {
            continue;
        }
        let constraint = constraints.get_property(track_kind);
        let exact_id = exact_device_id(constraint);
        let device = devices.iter().find(|device| {
            device.kind == device_kind
                && exact_id
                    .as_ref()
                    .is_none_or(|device_id| &device.device_id == device_id)
        });
        let Some(device) = device else {
            if exact_id.is_some() {
                return w3cos_core::promise::reject(vec![overconstrained_error(
                    "deviceId",
                    &format!("No {track_kind} input satisfies the exact deviceId constraint"),
                )]);
            }
            return w3cos_core::promise::reject(vec![named_error(
                "NotFoundError",
                &format!("No {track_kind} input device is available"),
            )]);
        };
        tracks.push(track_value(track_kind, &device.label));
    }
    w3cos_core::promise::resolve(vec![stream_value(tracks)])
}

fn supported_constraints() -> Value {
    Value::object(
        [
            "aspectRatio",
            "autoGainControl",
            "channelCount",
            "deviceId",
            "displaySurface",
            "echoCancellation",
            "facingMode",
            "frameRate",
            "groupId",
            "height",
            "latency",
            "noiseSuppression",
            "sampleRate",
            "sampleSize",
            "width",
        ]
        .into_iter()
        .map(|name| (name.to_string(), Value::Bool(true)))
        .collect(),
    )
}

pub fn media_devices_value() -> Value {
    let value = Value::object(HashMap::from([("ondevicechange".to_string(), Value::Null)]));
    crate::web_events::event_target_class().call(value.clone(), vec![]);
    value.set_property(
        "enumerateDevices",
        Value::function(|_, _| {
            let devices = DEVICES.with(|devices| {
                devices
                    .borrow()
                    .iter()
                    .map(device_value)
                    .collect::<Vec<_>>()
            });
            w3cos_core::promise::resolve(vec![Value::array(devices)])
        }),
    );
    value.set_property(
        "getUserMedia",
        Value::function(|_, args| get_user_media(args.first().cloned().unwrap_or_default())),
    );
    value.set_property(
        "getSupportedConstraints",
        Value::function(|_, _| supported_constraints()),
    );
    value.set_property(
        "getDisplayMedia",
        Value::function(|_, args| {
            let constraints = args.first().cloned().unwrap_or(Value::Undefined);
            if constraints
                .get_property("video")
                .strict_eq(&Value::Bool(false))
            {
                return w3cos_core::promise::reject(vec![named_error(
                    "TypeError",
                    "getDisplayMedia requires a video track",
                )]);
            }
            static WARNING: Once = Once::new();
            WARNING.call_once(|| {
                eprintln!(
                    "[w3cos] warning: getDisplayMedia requires a host screen-capture picker and \
                     capture-stream adapter"
                );
            });
            w3cos_core::promise::reject(vec![named_error(
                "NotSupportedError",
                "Screen capture is unavailable without a host adapter",
            )])
        }),
    );
    value.set_property(
        "selectAudioOutput",
        Value::function(|_, _| {
            static WARNING: Once = Once::new();
            WARNING.call_once(|| {
                eprintln!(
                    "[w3cos] warning: selectAudioOutput requires a host audio-output picker and \
                     permission adapter"
                );
            });
            w3cos_core::promise::reject(vec![named_error(
                "NotFoundError",
                "No selectable audio-output device adapter is configured",
            )])
        }),
    );
    value.set_property(
        "setCaptureHandleConfig",
        Value::function(|_, _| {
            static WARNING: Once = Once::new();
            WARNING.call_once(|| {
                eprintln!(
                    "[w3cos] warning: MediaDevices.setCaptureHandleConfig() is accepted for \
                     compatibility; capture-handle publication requires a host adapter"
                );
            });
            Value::Undefined
        }),
    );
    w3cos_core::class::set_prototype_of(&value, &media_devices_class().get_property("prototype"));
    value
}

pub fn set_devices(devices: Vec<MediaDevice>) {
    DEVICES.with(|current| *current.borrow_mut() = devices);
}

pub fn set_permission_denied(denied: bool) {
    PERMISSION_DENIED.with(|permission| permission.set(denied));
}

pub fn reset() {
    DEVICES.with(|devices| devices.borrow_mut().clear());
    TRACK_GENERATORS.with(|generators| generators.borrow_mut().clear());
    PERMISSION_DENIED.with(|permission| permission.set(false));
    NEXT_TRACK_ID.with(|next| next.set(1));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn devices() -> Vec<MediaDevice> {
        vec![
            MediaDevice {
                device_id: "mic".into(),
                kind: "audioinput".into(),
                label: "Virtual microphone".into(),
                group_id: "virtual".into(),
            },
            MediaDevice {
                device_id: "camera".into(),
                kind: "videoinput".into(),
                label: "Virtual camera".into(),
                group_id: "virtual".into(),
            },
        ]
    }

    #[test]
    fn media_device_info_exposes_browser_prototype_shape() {
        let class = media_device_info_class();
        let prototype = class.get_property("prototype");
        for property in ["deviceId", "groupId", "kind", "label"] {
            assert!(prototype.get_property(property).is_undefined());
        }
        assert!(prototype.get_property("toJSON").is_function());
        let input = device_value(&devices()[0]);
        assert!(w3cos_core::class::instance_of(
            &input,
            &input_device_info_class()
        ));
        assert!(w3cos_core::class::instance_of(
            &input,
            &media_device_info_class()
        ));
        assert!(input.call_method("getCapabilities", vec![]).is_object());
    }

    #[test]
    fn generated_video_frames_flow_through_track_processors() {
        reset();
        let generator = w3cos_core::class::construct(
            &media_stream_track_generator_class(),
            vec![Value::object(HashMap::from([(
                "kind".into(),
                Value::string("video"),
            )]))],
        );
        assert!(w3cos_core::class::instance_of(
            &generator,
            &media_stream_track_class()
        ));
        let processor = w3cos_core::class::construct(
            &media_stream_track_processor_class(),
            vec![Value::object(HashMap::from([(
                "track".into(),
                generator.clone(),
            )]))],
        );
        let reader = processor
            .get_property("readable")
            .call_method("getReader", vec![]);
        let writer = generator
            .get_property("writable")
            .call_method("getWriter", vec![]);
        let frame = w3cos_core::class::construct(
            &crate::webcodecs_web::class_for("VideoFrame"),
            vec![
                w3cos_core::binary::typed_array_value(
                    (0..4).map(|value| Value::Number(value as f64)).collect(),
                ),
                Value::object(HashMap::from([
                    ("codedHeight".into(), Value::Number(1.0)),
                    ("codedWidth".into(), Value::Number(1.0)),
                    ("format".into(), Value::string("RGBA")),
                    ("timestamp".into(), Value::Number(0.0)),
                ])),
            ],
        );
        writer.call_method("write", vec![frame.clone()]);
        let delivered = Rc::new(RefCell::new(Value::Undefined));
        let delivered_for_callback = Rc::clone(&delivered);
        reader.call_method("read", vec![]).call_method(
            "then",
            vec![Value::function(move |_, args| {
                *delivered_for_callback.borrow_mut() = args[0].get_property("value");
                Value::Undefined
            })],
        );
        crate::jsdom::drain_microtasks();
        assert!(*delivered.borrow() == frame);
        assert_eq!(processor.get_property("totalFrames").to_number(), 1.0);
        assert_eq!(processor.get_property("discardedFrames").to_number(), 0.0);
        let stats = generator.get_property("stats");
        assert!(w3cos_core::class::instance_of(
            &stats,
            &media_stats_class("MediaStreamTrackVideoStats")
        ));
        assert_eq!(stats.get_property("totalFrames").to_number(), 1.0);
        assert_eq!(
            stats
                .call_method("toJSON", vec![])
                .get_property("deliveredFrames")
                .to_number(),
            1.0
        );
    }

    #[test]
    fn audio_and_playback_stats_use_neutral_resettable_records() {
        let track = track_value("audio", "microphone");
        let stats = track.get_property("stats");
        assert!(w3cos_core::class::instance_of(
            &stats,
            &media_stats_class("MediaStreamTrackAudioStats")
        ));
        stats.set_property("latency", Value::Number(0.25));
        stats.set_property("maximumLatency", Value::Number(0.5));
        stats.call_method("resetLatency", vec![]);
        assert_eq!(stats.get_property("latency").to_number(), 0.0);
        assert_eq!(stats.get_property("maximumLatency").to_number(), 0.0);

        let playback = media_stats_value("AudioPlaybackStats");
        assert!(w3cos_core::class::instance_of(
            &playback,
            &media_stats_class("AudioPlaybackStats")
        ));
        assert_eq!(
            playback
                .call_method("toJSON", vec![])
                .get_property("underrunEvents")
                .to_number(),
            0.0
        );
    }

    #[test]
    fn user_media_resolves_stream_and_tracks_can_stop() {
        reset();
        set_devices(devices());
        let result = Rc::new(RefCell::new(Value::Undefined));
        let capture = result.clone();
        media_devices_value()
            .call_method(
                "getUserMedia",
                vec![Value::object(HashMap::from([
                    ("audio".to_string(), Value::Bool(true)),
                    ("video".to_string(), Value::Bool(true)),
                ]))],
            )
            .call_method(
                "then",
                vec![Value::function(move |_, args| {
                    *capture.borrow_mut() = args.first().cloned().unwrap_or_default();
                    Value::Undefined
                })],
            );
        w3cos_core::promise::drain_microtasks();
        let stream = result.borrow().clone();
        assert_eq!(
            stream
                .call_method("getTracks", vec![])
                .get_property("length"),
            2.into()
        );
        assert!(stream.get_property("active").to_bool());
        for track in stream.call_method("getTracks", vec![]).iter() {
            track.call_method("stop", vec![]);
        }
        assert!(!stream.get_property("active").to_bool());
    }

    #[test]
    fn exposes_constraints_and_explicit_host_dependent_capture_failures() {
        let media = media_devices_value();
        let constraints = media.call_method("getSupportedConstraints", vec![]);
        assert!(constraints.get_property("width").to_bool());
        assert!(constraints.get_property("echoCancellation").to_bool());

        let errors = Rc::new(RefCell::new(Vec::new()));
        for (method, args) in [
            (
                "getDisplayMedia",
                vec![Value::object(HashMap::from([(
                    "video".into(),
                    Value::Bool(false),
                )]))],
            ),
            ("getDisplayMedia", vec![Value::object(HashMap::new())]),
            ("selectAudioOutput", vec![]),
        ] {
            let errors_for_handler = Rc::clone(&errors);
            media.call_method(method, args).call_method(
                "catch",
                vec![Value::function(move |_, args| {
                    errors_for_handler
                        .borrow_mut()
                        .push(args[0].get_property("name").to_js_string());
                    Value::Undefined
                })],
            );
        }
        crate::jsdom::drain_microtasks();
        assert_eq!(
            &*errors.borrow(),
            &["TypeError", "NotSupportedError", "NotFoundError"]
        );
    }

    #[test]
    fn missing_adapter_rejects_with_not_found_error() {
        reset();
        let error = Rc::new(RefCell::new(String::new()));
        let capture = error.clone();
        media_devices_value()
            .call_method(
                "getUserMedia",
                vec![Value::object(HashMap::from([(
                    "video".to_string(),
                    Value::Bool(true),
                )]))],
            )
            .call_method(
                "catch",
                vec![Value::function(move |_, args| {
                    *capture.borrow_mut() = args[0].get_property("name").to_js_string();
                    Value::Undefined
                })],
            );
        w3cos_core::promise::drain_microtasks();
        assert_eq!(&*error.borrow(), "NotFoundError");
    }

    #[test]
    fn exact_device_constraint_rejects_with_overconstrained_error() {
        reset();
        set_devices(devices());
        let error = Rc::new(RefCell::new(Value::Undefined));
        let capture = Rc::clone(&error);
        media_devices_value()
            .call_method(
                "getUserMedia",
                vec![Value::object(HashMap::from([(
                    "audio".to_string(),
                    Value::object(HashMap::from([(
                        "deviceId".to_string(),
                        Value::object(HashMap::from([(
                            "exact".to_string(),
                            Value::string("missing-microphone"),
                        )])),
                    )])),
                )]))],
            )
            .call_method(
                "catch",
                vec![Value::function(move |_, args| {
                    *capture.borrow_mut() = args[0].clone();
                    Value::Undefined
                })],
            );
        w3cos_core::promise::drain_microtasks();
        let error = error.borrow().clone();
        assert!(w3cos_core::class::instance_of(
            &error,
            &overconstrained_error_class()
        ));
        assert_eq!(
            error.get_property("name").to_js_string(),
            "OverconstrainedError"
        );
        assert_eq!(error.get_property("constraint").to_js_string(), "deviceId");
    }
}
