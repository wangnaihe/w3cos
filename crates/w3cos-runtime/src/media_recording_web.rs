//! MediaRecorder and ImageCapture compatibility surfaces.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use w3cos_core::Value;

thread_local! {
    static MEDIA_RECORDER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static IMAGE_CAPTURE_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static CAPTURE_CONTROLLER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static CROP_TARGET_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static RESTRICTION_TARGET_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static BROWSER_CAPTURE_TRACK_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static WARNING_EMITTED: Cell<bool> = const { Cell::new(false) };
}

fn warning() {
    WARNING_EMITTED.with(|warned| {
        if !warned.replace(true) {
            eprintln!(
                "[w3cos] warning: media recording/capture lifecycle is available, but encoded \
                 audio/video and camera frame extraction require a host codec/capture adapter"
            );
        }
    });
}

fn type_error(message: &str) -> ! {
    w3cos_core::throw_value(w3cos_core::error_instance(
        "TypeError",
        vec![Value::string(message)],
    ))
}

fn invalid_state(message: &str) -> ! {
    w3cos_core::throw_value(w3cos_core::web::dom_exception_instance(
        message,
        "InvalidStateError",
    ))
}

fn dispatch(target: &Value, event_type: &str, data: Option<Value>) {
    let event = if let Some(data) = data {
        w3cos_core::class::construct(
            &crate::web_events::event_subclass_class("BlobEvent"),
            vec![
                Value::string(event_type),
                Value::object(HashMap::from([("data".into(), data)])),
            ],
        )
    } else {
        w3cos_core::class::construct(
            &crate::web_events::event_class(),
            vec![Value::string(event_type)],
        )
    };
    target.call_method("dispatchEvent", vec![event]);
}

fn empty_blob(mime_type: &str) -> Value {
    w3cos_core::class::construct(
        &crate::files::blob_class(),
        vec![
            Value::array(Vec::new()),
            Value::object(HashMap::from([("type".into(), Value::string(mime_type))])),
        ],
    )
}

pub fn media_recorder_class() -> Value {
    MEDIA_RECORDER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|this, args| {
            crate::web_events::event_target_class().call(this.clone(), Vec::new());
            let stream = args.first().cloned().unwrap_or(Value::Undefined);
            if !w3cos_core::class::instance_of(
                &stream,
                &crate::media_devices_web::media_stream_class(),
            ) {
                type_error("MediaRecorder requires a MediaStream");
            }
            let options = args.get(1).cloned().unwrap_or(Value::Undefined);
            let mime_type = options.get_property("mimeType").to_js_string();
            for (name, value) in [
                ("stream", stream),
                ("mimeType", Value::string(&mime_type)),
                ("state", Value::string("inactive")),
                ("audioBitrateMode", Value::string("variable")),
                (
                    "audioBitsPerSecond",
                    Value::Number(options.get_property("audioBitsPerSecond").to_u32() as f64),
                ),
                (
                    "videoBitsPerSecond",
                    Value::Number(options.get_property("videoBitsPerSecond").to_u32() as f64),
                ),
                ("ondataavailable", Value::Null),
                ("onerror", Value::Null),
                ("onpause", Value::Null),
                ("onresume", Value::Null),
                ("onstart", Value::Null),
                ("onstop", Value::Null),
            ] {
                this.set_property(name, value);
            }
            let start_target = this.clone();
            this.set_property(
                "start",
                Value::function(move |_, args| {
                    if start_target.get_property("state").to_js_string() != "inactive" {
                        invalid_state("MediaRecorder is already recording");
                    }
                    start_target.set_property("state", Value::string("recording"));
                    if args.first().is_some_and(|slice| slice.to_number() > 0.0) {
                        warning();
                    }
                    dispatch(&start_target, "start", None);
                    Value::Undefined
                }),
            );
            let pause_target = this.clone();
            this.set_property(
                "pause",
                Value::function(move |_, _| {
                    if pause_target.get_property("state").to_js_string() != "recording" {
                        invalid_state("MediaRecorder is not recording");
                    }
                    pause_target.set_property("state", Value::string("paused"));
                    dispatch(&pause_target, "pause", None);
                    Value::Undefined
                }),
            );
            let resume_target = this.clone();
            this.set_property(
                "resume",
                Value::function(move |_, _| {
                    if resume_target.get_property("state").to_js_string() != "paused" {
                        invalid_state("MediaRecorder is not paused");
                    }
                    resume_target.set_property("state", Value::string("recording"));
                    dispatch(&resume_target, "resume", None);
                    Value::Undefined
                }),
            );
            let request_target = this.clone();
            this.set_property(
                "requestData",
                Value::function(move |_, _| {
                    if request_target.get_property("state").to_js_string() == "inactive" {
                        invalid_state("MediaRecorder is inactive");
                    }
                    warning();
                    dispatch(
                        &request_target,
                        "dataavailable",
                        Some(empty_blob(
                            &request_target.get_property("mimeType").to_js_string(),
                        )),
                    );
                    Value::Undefined
                }),
            );
            let stop_target = this.clone();
            this.set_property(
                "stop",
                Value::function(move |_, _| {
                    if stop_target.get_property("state").to_js_string() == "inactive" {
                        invalid_state("MediaRecorder is inactive");
                    }
                    warning();
                    stop_target.set_property("state", Value::string("inactive"));
                    dispatch(
                        &stop_target,
                        "dataavailable",
                        Some(empty_blob(
                            &stop_target.get_property("mimeType").to_js_string(),
                        )),
                    );
                    dispatch(&stop_target, "stop", None);
                    Value::Undefined
                }),
            );
            Value::Undefined
        });
        class.set_property("name", Value::string("MediaRecorder"));
        class.set_property(
            "isTypeSupported",
            Value::function(|_, args| {
                let mime_type = args.first().map(Value::to_js_string).unwrap_or_default();
                if !mime_type.is_empty() {
                    warning();
                }
                Value::Bool(mime_type.is_empty())
            }),
        );
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in [
            "audioBitrateMode",
            "audioBitsPerSecond",
            "mimeType",
            "ondataavailable",
            "onerror",
            "onpause",
            "onresume",
            "onstart",
            "onstop",
            "pause",
            "requestData",
            "resume",
            "start",
            "state",
            "stop",
            "stream",
            "videoBitsPerSecond",
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

pub fn image_capture_class() -> Value {
    IMAGE_CAPTURE_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|this, args| {
            let track = args.first().cloned().unwrap_or(Value::Undefined);
            if !w3cos_core::class::instance_of(
                &track,
                &crate::media_devices_web::media_stream_track_class(),
            ) || track.get_property("kind").to_js_string() != "video"
            {
                type_error("ImageCapture requires a video MediaStreamTrack");
            }
            this.set_property("track", track);
            for method in ["getPhotoCapabilities", "getPhotoSettings"] {
                this.set_property(
                    method,
                    Value::function(|_, _| {
                        warning();
                        w3cos_core::promise::resolve(vec![Value::object(HashMap::new())])
                    }),
                );
            }
            this.set_property(
                "takePhoto",
                Value::function(|_, _| {
                    warning();
                    w3cos_core::promise::resolve(vec![empty_blob("image/png")])
                }),
            );
            this.set_property(
                "grabFrame",
                Value::function(|_, _| {
                    warning();
                    w3cos_core::promise::reject(vec![w3cos_core::web::dom_exception_instance(
                        "No native video frame provider is registered",
                        "NotSupportedError",
                    )])
                }),
            );
            Value::Undefined
        });
        class.set_property("name", Value::string("ImageCapture"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in [
            "getPhotoCapabilities",
            "getPhotoSettings",
            "grabFrame",
            "takePhoto",
            "track",
        ] {
            prototype.set_property(member, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn element_target_class(
    slot: &'static std::thread::LocalKey<RefCell<Option<Value>>>,
    name: &'static str,
) -> Value {
    slot.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class =
            Value::function(move |_, _| type_error(&format!("Illegal constructor: {name}")));
        class.set_property("name", Value::string(name));
        class.set_property(
            "fromElement",
            Value::function(move |_, args| {
                let element = args.first().cloned().unwrap_or(Value::Undefined);
                if element.get_property("nodeType").to_u32() != 1 {
                    return w3cos_core::promise::reject(vec![w3cos_core::error_instance(
                        "TypeError",
                        vec![Value::string("fromElement requires an Element")],
                    )]);
                }
                let target = Value::object(HashMap::from([("__w3cos_element".into(), element)]));
                w3cos_core::class::set_prototype_of(
                    &target,
                    &if name == "CropTarget" {
                        crop_target_class()
                    } else {
                        restriction_target_class()
                    }
                    .get_property("prototype"),
                );
                w3cos_core::promise::resolve(vec![target])
            }),
        );
        let prototype = Value::object(HashMap::from([("constructor".into(), class.clone())]));
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn crop_target_class() -> Value {
    element_target_class(&CROP_TARGET_CLASS, "CropTarget")
}

pub fn restriction_target_class() -> Value {
    element_target_class(&RESTRICTION_TARGET_CLASS, "RestrictionTarget")
}

pub fn capture_controller_class() -> Value {
    CAPTURE_CONTROLLER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|this, _| {
            crate::web_events::event_target_class().call(this.clone(), Vec::new());
            this.set_property("zoomLevel", Value::Number(100.0));
            this.set_property("onzoomlevelchange", Value::Null);
            for method in [
                "decreaseZoomLevel",
                "forwardWheel",
                "increaseZoomLevel",
                "resetZoomLevel",
                "setFocusBehavior",
            ] {
                this.set_property(
                    method,
                    Value::function(|_, _| {
                        warning();
                        w3cos_core::promise::reject(vec![w3cos_core::web::dom_exception_instance(
                            "Display capture control requires a host adapter",
                            "NotSupportedError",
                        )])
                    }),
                );
            }
            this.set_property(
                "getSupportedZoomLevels",
                Value::function(|_, _| {
                    warning();
                    w3cos_core::promise::resolve(vec![Value::array(Vec::new())])
                }),
            );
            Value::Undefined
        });
        class.set_property("name", Value::string("CaptureController"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in [
            "decreaseZoomLevel",
            "forwardWheel",
            "getSupportedZoomLevels",
            "increaseZoomLevel",
            "onzoomlevelchange",
            "resetZoomLevel",
            "setFocusBehavior",
            "zoomLevel",
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

pub fn browser_capture_media_stream_track_class() -> Value {
    BROWSER_CAPTURE_TRACK_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            type_error("Illegal constructor: BrowserCaptureMediaStreamTrack")
        });
        class.set_property("name", Value::string("BrowserCaptureMediaStreamTrack"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for method in ["cropTo", "restrictTo"] {
            prototype.set_property(method, Value::Undefined);
        }
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::media_devices_web::media_stream_track_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn reset() {
    WARNING_EMITTED.with(|warned| warned.set(false));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorder_lifecycle_emits_compatible_empty_data() {
        let stream = crate::media_devices_web::stream_value(Vec::new());
        let recorder = w3cos_core::class::construct(&media_recorder_class(), vec![stream]);
        let chunks = std::rc::Rc::new(Cell::new(0usize));
        let chunks_for_listener = std::rc::Rc::clone(&chunks);
        recorder.call_method(
            "addEventListener",
            vec![
                Value::string("dataavailable"),
                Value::function(move |_, args| {
                    assert_eq!(
                        args[0].get_property("data").get_property("size").to_u32(),
                        0
                    );
                    chunks_for_listener.set(chunks_for_listener.get() + 1);
                    Value::Undefined
                }),
            ],
        );
        recorder.call_method("start", Vec::new());
        assert_eq!(recorder.get_property("state").to_js_string(), "recording");
        recorder.call_method("stop", Vec::new());
        assert_eq!(recorder.get_property("state").to_js_string(), "inactive");
        assert_eq!(chunks.get(), 1);
    }

    #[test]
    fn crop_targets_are_branded_from_real_elements() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let element =
            crate::jsdom::document_value().call_method("createElement", vec![Value::string("div")]);
        let target = std::rc::Rc::new(RefCell::new(Value::Undefined));
        let target_for_callback = std::rc::Rc::clone(&target);
        crop_target_class()
            .call_method("fromElement", vec![element])
            .call_method(
                "then",
                vec![Value::function(move |_, args| {
                    *target_for_callback.borrow_mut() = args[0].clone();
                    Value::Undefined
                })],
            );
        w3cos_core::promise::drain_microtasks();
        assert!(w3cos_core::class::instance_of(
            &target.borrow(),
            &crop_target_class()
        ));
    }
}
