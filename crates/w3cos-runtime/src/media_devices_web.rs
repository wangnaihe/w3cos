//! Browser-shaped MediaDevices, MediaStream, and MediaStreamTrack facades.
//!
//! Native hosts register devices through [`set_devices`]. The facade remains
//! standards-shaped when no adapter is present by rejecting `getUserMedia`
//! with a named DOM-style error.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use w3cos_core::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaDevice {
    pub device_id: String,
    pub kind: String,
    pub label: String,
    pub group_id: String,
}

thread_local! {
    static DEVICES: RefCell<Vec<MediaDevice>> = const { RefCell::new(Vec::new()) };
    static PERMISSION_DENIED: Cell<bool> = const { Cell::new(false) };
    static NEXT_TRACK_ID: Cell<u64> = const { Cell::new(1) };
    static MEDIA_STREAM_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static MEDIA_STREAM_TRACK_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static MEDIA_DEVICE_INFO_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
}

fn named_error(name: &str, message: &str) -> Value {
    let error = w3cos_core::class::construct(
        &crate::unsupported::dom_exception_class(),
        vec![Value::string(message), Value::string(name)],
    );
    error.set_property("code", Value::Number(0.0));
    error
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

pub fn media_stream_track_class() -> Value {
    simple_class(&MEDIA_STREAM_TRACK_CLASS, "MediaStreamTrack")
}

pub fn media_device_info_class() -> Value {
    simple_class(&MEDIA_DEVICE_INFO_CLASS, "MediaDeviceInfo")
}

fn next_track_id() -> String {
    NEXT_TRACK_ID.with(|next| {
        let id = next.get();
        next.set(id.saturating_add(1));
        format!("w3cos-track-{id}")
    })
}

fn track_value(kind: &str, label: &str) -> Value {
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

fn stream_value(tracks: Vec<Value>) -> Value {
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
        &media_device_info_class().get_property("prototype"),
    );
    value
}

fn requested(constraints: &Value, name: &str) -> bool {
    let value = constraints.get_property(name);
    !value.is_undefined() && !value.is_null() && value.to_bool()
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
        let Some(device) = devices.iter().find(|device| device.kind == device_kind) else {
            return w3cos_core::promise::reject(vec![named_error(
                "NotFoundError",
                &format!("No {track_kind} input device is available"),
            )]);
        };
        tracks.push(track_value(track_kind, &device.label));
    }
    w3cos_core::promise::resolve(vec![stream_value(tracks)])
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
}
