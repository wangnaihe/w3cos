//! Value-level implementations of the core Web Events constructors.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::jsdom::realm_function;
use w3cos_core::Value;

thread_local! {
    static EVENT_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static CUSTOM_EVENT_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static EVENT_TARGET_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static EVENT_SUBCLASSES: RefCell<Option<HashMap<String, Value>>> = const { RefCell::new(None) };
    static TOUCH_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static TOUCH_LIST_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static INPUT_DEVICE_CAPABILITIES_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static EVENT_TARGET_INSTANCES: RefCell<Vec<EventTargetBinding>> = const { RefCell::new(Vec::new()) };
}

fn realm_event_function(f: impl Fn(Value, Vec<Value>) -> Value + 'static) -> Value {
    realm_function(crate::jsdom::realm_generation(), f)
}

pub const EVENT_SUBCLASS_NAMES: &[&str] = &[
    "UIEvent",
    "MouseEvent",
    "KeyboardEvent",
    "PointerEvent",
    "WheelEvent",
    "FocusEvent",
    "InputEvent",
    "CompositionEvent",
    "ClipboardEvent",
    "DragEvent",
    "TouchEvent",
    "AnimationEvent",
    "TransitionEvent",
    "ErrorEvent",
    "ProgressEvent",
    "MessageEvent",
    "HashChangeEvent",
    "PopStateEvent",
    "CloseEvent",
    "BlobEvent",
    "SubmitEvent",
    "FormDataEvent",
    "ToggleEvent",
    "CommandEvent",
    "PageTransitionEvent",
    "PromiseRejectionEvent",
    "SecurityPolicyViolationEvent",
    "TrackEvent",
    "MediaStreamTrackEvent",
    "StorageEvent",
    "MediaQueryListEvent",
    "AnimationPlaybackEvent",
    "AudioProcessingEvent",
    "BeforeInstallPromptEvent",
    "BeforeUnloadEvent",
    "CharacterBoundsUpdateEvent",
    "ClipboardChangeEvent",
    "ContentVisibilityAutoStateChangeEvent",
    "DocumentPictureInPictureEvent",
    "FontFaceSetLoadEvent",
    "GPUUncapturedErrorEvent",
    "HIDConnectionEvent",
    "InterestEvent",
    "MediaEncryptedEvent",
    "MediaStreamEvent",
    "OfflineAudioCompletionEvent",
    "PaymentMethodChangeEvent",
    "PaymentRequestUpdateEvent",
    "PictureInPictureEvent",
    "RTCDTMFToneChangeEvent",
    "RTCDataChannelEvent",
    "RTCErrorEvent",
    "RTCPeerConnectionIceErrorEvent",
    "RTCPeerConnectionIceEvent",
    "RTCTrackEvent",
    "SnapEvent",
    "SpeechRecognitionErrorEvent",
    "SpeechRecognitionEvent",
    "SpeechSynthesisErrorEvent",
    "SpeechSynthesisEvent",
    "TaskPriorityChangeEvent",
    "TextEvent",
    "TextFormatUpdateEvent",
    "TextUpdateEvent",
    "VirtualKeyboardGeometryChangeEvent",
    "WebGLContextEvent",
    "WindowControlsOverlayGeometryChangeEvent",
    "XRInputSourceEvent",
    "XRInputSourcesChangeEvent",
    "XRLayerEvent",
    "XRReferenceSpaceEvent",
    "XRSessionEvent",
    "XRVisibilityMaskChangeEvent",
];

pub fn touch_class() -> Value {
    TOUCH_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = realm_event_function(|this, args| {
            let init = arg(&args, 0);
            install_fields(
                &this,
                &init,
                &[
                    ("identifier", Value::Number(0.0)),
                    ("target", Value::Null),
                    ("screenX", Value::Number(0.0)),
                    ("screenY", Value::Number(0.0)),
                    ("clientX", Value::Number(0.0)),
                    ("clientY", Value::Number(0.0)),
                    ("pageX", Value::Number(0.0)),
                    ("pageY", Value::Number(0.0)),
                    ("radiusX", Value::Number(0.0)),
                    ("radiusY", Value::Number(0.0)),
                    ("rotationAngle", Value::Number(0.0)),
                    ("force", Value::Number(0.0)),
                ],
            );
            Value::Undefined
        });
        class.set_property("name", Value::string("Touch"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in [
            "clientX",
            "clientY",
            "force",
            "identifier",
            "pageX",
            "pageY",
            "radiusX",
            "radiusY",
            "rotationAngle",
            "screenX",
            "screenY",
            "target",
        ] {
            prototype.set_property(member, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn touch_list_class() -> Value {
    TOUCH_LIST_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = realm_event_function(|_, _| {
            w3cos_core::throw_value(Value::object(HashMap::from([
                ("name".into(), Value::string("TypeError")),
                (
                    "message".into(),
                    Value::string("Illegal constructor: TouchList"),
                ),
            ])))
        });
        class.set_property("name", Value::string("TouchList"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property("item", Value::Undefined);
        prototype.set_property("length", Value::Undefined);
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn touch_list_value(input: Value) -> Value {
    if w3cos_core::class::instance_of(&input, &touch_list_class()) {
        return input;
    }
    let items = input.iter().collect::<Vec<_>>();
    let mut properties = HashMap::from([("length".to_string(), Value::Number(items.len() as f64))]);
    for (index, item) in items.iter().enumerate() {
        properties.insert(index.to_string(), item.clone());
    }
    properties.insert(
        "item".into(),
        realm_event_function(move |_, args| {
            let index = args.first().cloned().unwrap_or_default().to_u32() as usize;
            items.get(index).cloned().unwrap_or(Value::Null)
        }),
    );
    let value = Value::object(properties);
    w3cos_core::class::set_prototype_of(&value, &touch_list_class().get_property("prototype"));
    value
}

#[derive(Clone)]
struct Listener {
    type_name: String,
    callback: Value,
    capture: bool,
    once: bool,
}

struct EventTargetBinding {
    value: Value,
    listeners: Rc<RefCell<Vec<Listener>>>,
}

fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or(Value::Undefined)
}

fn bool_option(options: &Value, name: &str) -> bool {
    if name == "capture" {
        if let Some(value) = options.as_bool() {
            return value;
        }
    }
    if options.is_object() {
        options.get_property(name).to_bool()
    } else {
        false
    }
}

fn listener_is_callable(listener: &Value) -> bool {
    listener.is_function() || listener.get_property("handleEvent").is_function()
}

fn invoke_listener(listener: &Value, target: &Value, event: Value) {
    if listener.is_function() {
        listener.call(target.clone(), vec![event]);
    } else {
        listener.call_method("handleEvent", vec![event]);
    }
}

fn install_event(this: &Value, args: &[Value], custom: bool) {
    let type_name = arg(args, 0).to_js_string();
    let init = arg(args, 1);
    this.set_property("type", Value::string(&type_name));
    this.set_property("bubbles", Value::Bool(bool_option(&init, "bubbles")));
    this.set_property("cancelable", Value::Bool(bool_option(&init, "cancelable")));
    this.set_property("composed", Value::Bool(bool_option(&init, "composed")));
    this.set_property("target", Value::Null);
    this.set_property("currentTarget", Value::Null);
    this.set_property("srcElement", Value::Null);
    this.set_property("relatedTarget", Value::Null);
    this.set_property("eventPhase", Value::Number(0.0));
    this.set_property(
        "timeStamp",
        Value::Number(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64()
                * 1000.0,
        ),
    );
    this.set_property("isTrusted", Value::Bool(false));
    this.set_property("__pd", Value::Bool(false));
    this.set_property("__sp", Value::Bool(false));
    this.set_property("__sip", Value::Bool(false));
    this.set_property("returnValue", Value::Bool(true));
    if custom {
        let detail = if init.is_object() {
            init.get_property("detail")
        } else {
            Value::Null
        };
        this.set_property("detail", detail);
    }

    for (name, value) in [
        ("NONE", 0.0),
        ("CAPTURING_PHASE", 1.0),
        ("AT_TARGET", 2.0),
        ("BUBBLING_PHASE", 3.0),
    ] {
        this.set_property(name, Value::Number(value));
    }

    this.set_property(
        "preventDefault",
        realm_event_function(|this, _| {
            if this.get_property("cancelable").to_bool() {
                this.set_property("__pd", Value::Bool(true));
                this.set_property("returnValue", Value::Bool(false));
            }
            Value::Undefined
        }),
    );
    this.set_property(
        "stopPropagation",
        realm_event_function(|this, _| {
            this.set_property("__sp", Value::Bool(true));
            Value::Undefined
        }),
    );
    this.set_property(
        "stopImmediatePropagation",
        realm_event_function(|this, _| {
            this.set_property("__sp", Value::Bool(true));
            this.set_property("__sip", Value::Bool(true));
            Value::Undefined
        }),
    );
    this.set_property(
        "composedPath",
        realm_event_function(|this, _| {
            let path = this.get_property("__w3cos_path");
            if !path.is_undefined() {
                return path;
            }
            let target = this.get_property("target");
            if target.is_nullish() {
                Value::array(vec![])
            } else {
                Value::array(vec![target])
            }
        }),
    );
    this.set_property(
        "initEvent",
        realm_event_function(|this, args| {
            this.set_property("type", Value::string(&arg(&args, 0).to_js_string()));
            this.set_property("bubbles", Value::Bool(arg(&args, 1).to_bool()));
            this.set_property("cancelable", Value::Bool(arg(&args, 2).to_bool()));
            this.set_property("__pd", Value::Bool(false));
            this.set_property("__sp", Value::Bool(false));
            this.set_property("__sip", Value::Bool(false));
            this.set_property("returnValue", Value::Bool(true));
            Value::Undefined
        }),
    );
    this.set_property(
        "__w3cos_getter_defaultPrevented",
        realm_event_function(|this, _| this.get_property("__pd")),
    );
    this.set_property(
        "__w3cos_getter_cancelBubble",
        realm_event_function(|this, _| this.get_property("__sp")),
    );
    this.set_property(
        "__w3cos_setter_cancelBubble",
        realm_event_function(|this, args| {
            if arg(&args, 0).to_bool() {
                this.set_property("__sp", Value::Bool(true));
            }
            Value::Undefined
        }),
    );
}

fn init_value(init: &Value, name: &str, default: Value) -> Value {
    if !init.is_object() {
        return default;
    }
    let value = init.get_property(name);
    if value.is_undefined() { default } else { value }
}

fn install_fields(this: &Value, init: &Value, fields: &[(&str, Value)]) {
    for (name, default) in fields {
        this.set_property(name, init_value(init, name, default.clone()));
    }
}

fn install_ui_fields(this: &Value, init: &Value) {
    let source = init_value(init, "sourceCapabilities", Value::Null);
    let source = if source.is_object()
        && !w3cos_core::class::instance_of(&source, &input_device_capabilities_class())
    {
        w3cos_core::class::construct(&input_device_capabilities_class(), vec![source])
    } else {
        source
    };
    install_fields(
        this,
        init,
        &[
            ("detail", Value::Number(0.0)),
            ("view", Value::Null),
            ("which", Value::Number(0.0)),
            ("pseudoTarget", Value::Null),
        ],
    );
    this.set_property("sourceCapabilities", source);
}

pub fn input_device_capabilities_class() -> Value {
    INPUT_DEVICE_CAPABILITIES_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = realm_event_function(|this, args| {
            let init = arg(&args, 0);
            this.set_property(
                "firesTouchEvents",
                Value::Bool(init.get_property("firesTouchEvents").to_bool()),
            );
            this.set_property(
                "pointerMovementScrolls",
                Value::Bool(init.get_property("pointerMovementScrolls").to_bool()),
            );
            Value::Undefined
        });
        class.set_property("name", Value::string("InputDeviceCapabilities"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for property in ["firesTouchEvents", "pointerMovementScrolls"] {
            prototype.set_property(property, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn install_modifier_fields(this: &Value, init: &Value) {
    install_fields(
        this,
        init,
        &[
            ("ctrlKey", Value::Bool(false)),
            ("shiftKey", Value::Bool(false)),
            ("altKey", Value::Bool(false)),
            ("metaKey", Value::Bool(false)),
        ],
    );
    this.set_property(
        "getModifierState",
        realm_event_function(|this, args| {
            let property = match arg(&args, 0).to_js_string().as_str() {
                "Alt" => "altKey",
                "Control" => "ctrlKey",
                "Meta" => "metaKey",
                "Shift" => "shiftKey",
                _ => return Value::Bool(false),
            };
            Value::Bool(this.get_property(property).to_bool())
        }),
    );
}

fn install_mouse_fields(this: &Value, init: &Value) {
    install_ui_fields(this, init);
    install_modifier_fields(this, init);
    install_fields(
        this,
        init,
        &[
            ("screenX", Value::Number(0.0)),
            ("screenY", Value::Number(0.0)),
            ("clientX", Value::Number(0.0)),
            ("clientY", Value::Number(0.0)),
            ("pageX", Value::Number(0.0)),
            ("pageY", Value::Number(0.0)),
            ("offsetX", Value::Number(0.0)),
            ("offsetY", Value::Number(0.0)),
            ("movementX", Value::Number(0.0)),
            ("movementY", Value::Number(0.0)),
            ("button", Value::Number(0.0)),
            ("buttons", Value::Number(0.0)),
            ("relatedTarget", Value::Null),
            ("fromElement", Value::Null),
            ("toElement", Value::Null),
            ("layerX", Value::Number(0.0)),
            ("layerY", Value::Number(0.0)),
            ("x", Value::Number(0.0)),
            ("y", Value::Number(0.0)),
        ],
    );
}

#[derive(Clone, Copy)]
enum EventSubclass {
    Generic,
    Ui,
    Mouse,
    Keyboard,
    Pointer,
    Wheel,
    Focus,
    Input,
    Composition,
    Clipboard,
    Drag,
    Touch,
    Animation,
    Transition,
    Error,
    Progress,
    Message,
    HashChange,
    PopState,
    Close,
    Blob,
    Submit,
    FormData,
    Toggle,
    Command,
    PageTransition,
    PromiseRejection,
    SecurityPolicyViolation,
    Track,
    MediaStreamTrack,
    Storage,
    MediaQueryList,
}

fn require_init_field(init: &Value, name: &str, event_name: &str) {
    if init.get_property(name).is_undefined() {
        w3cos_core::throw_value(Value::object(HashMap::from([
            ("name".into(), Value::string("TypeError")),
            (
                "message".into(),
                Value::string(&format!("{event_name} requires {name}")),
            ),
        ])));
    }
}

fn install_subclass(this: &Value, args: &[Value], kind: EventSubclass) {
    install_event(this, args, false);
    let init = arg(args, 1);
    match kind {
        EventSubclass::Generic => {}
        EventSubclass::Ui => install_ui_fields(this, &init),
        EventSubclass::Mouse => install_mouse_fields(this, &init),
        EventSubclass::Keyboard => {
            install_ui_fields(this, &init);
            install_modifier_fields(this, &init);
            install_fields(
                this,
                &init,
                &[
                    ("key", Value::string("")),
                    ("code", Value::string("")),
                    ("location", Value::Number(0.0)),
                    ("repeat", Value::Bool(false)),
                    ("isComposing", Value::Bool(false)),
                    ("charCode", Value::Number(0.0)),
                    ("keyCode", Value::Number(0.0)),
                ],
            );
        }
        EventSubclass::Pointer => {
            install_mouse_fields(this, &init);
            install_fields(
                this,
                &init,
                &[
                    ("pointerId", Value::Number(0.0)),
                    ("width", Value::Number(1.0)),
                    ("height", Value::Number(1.0)),
                    ("pressure", Value::Number(0.0)),
                    ("tangentialPressure", Value::Number(0.0)),
                    ("tiltX", Value::Number(0.0)),
                    ("tiltY", Value::Number(0.0)),
                    ("twist", Value::Number(0.0)),
                    ("pointerType", Value::string("")),
                    ("isPrimary", Value::Bool(false)),
                    ("altitudeAngle", Value::Number(0.0)),
                    ("azimuthAngle", Value::Number(0.0)),
                    ("persistentDeviceId", Value::Number(0.0)),
                ],
            );
            for method in ["getCoalescedEvents", "getPredictedEvents"] {
                this.set_property(method, realm_event_function(|_, _| Value::array(vec![])));
            }
        }
        EventSubclass::Wheel => {
            install_mouse_fields(this, &init);
            install_fields(
                this,
                &init,
                &[
                    ("deltaX", Value::Number(0.0)),
                    ("deltaY", Value::Number(0.0)),
                    ("deltaZ", Value::Number(0.0)),
                    ("deltaMode", Value::Number(0.0)),
                    ("wheelDelta", Value::Number(0.0)),
                    ("wheelDeltaX", Value::Number(0.0)),
                    ("wheelDeltaY", Value::Number(0.0)),
                ],
            );
            for (name, value) in [
                ("DOM_DELTA_PIXEL", 0.0),
                ("DOM_DELTA_LINE", 1.0),
                ("DOM_DELTA_PAGE", 2.0),
            ] {
                this.set_property(name, Value::Number(value));
            }
        }
        EventSubclass::Focus => {
            install_ui_fields(this, &init);
            install_fields(this, &init, &[("relatedTarget", Value::Null)]);
        }
        EventSubclass::Input => {
            install_ui_fields(this, &init);
            install_fields(
                this,
                &init,
                &[
                    ("data", Value::Null),
                    ("isComposing", Value::Bool(false)),
                    ("inputType", Value::string("")),
                    ("dataTransfer", Value::Null),
                ],
            );
            this.set_property(
                "getTargetRanges",
                realm_event_function(|_, _| Value::array(vec![])),
            );
        }
        EventSubclass::Composition => {
            install_ui_fields(this, &init);
            install_fields(this, &init, &[("data", Value::string(""))]);
            this.set_property(
                "initCompositionEvent",
                realm_event_function(|_, _| Value::Undefined),
            );
        }
        EventSubclass::Clipboard => {
            install_fields(this, &init, &[("clipboardData", Value::Null)]);
        }
        EventSubclass::Drag => {
            install_mouse_fields(this, &init);
            install_fields(this, &init, &[("dataTransfer", Value::Null)]);
        }
        EventSubclass::Touch => {
            install_ui_fields(this, &init);
            install_modifier_fields(this, &init);
            install_fields(
                this,
                &init,
                &[
                    ("touches", Value::array(vec![])),
                    ("targetTouches", Value::array(vec![])),
                    ("changedTouches", Value::array(vec![])),
                ],
            );
            for property in ["touches", "targetTouches", "changedTouches"] {
                this.set_property(property, touch_list_value(this.get_property(property)));
            }
        }
        EventSubclass::Animation => install_fields(
            this,
            &init,
            &[
                ("animationName", Value::string("")),
                ("elapsedTime", Value::Number(0.0)),
                ("pseudoElement", Value::string("")),
                ("pseudoTarget", Value::Null),
            ],
        ),
        EventSubclass::Transition => install_fields(
            this,
            &init,
            &[
                ("propertyName", Value::string("")),
                ("elapsedTime", Value::Number(0.0)),
                ("pseudoElement", Value::string("")),
                ("pseudoTarget", Value::Null),
            ],
        ),
        EventSubclass::Error => install_fields(
            this,
            &init,
            &[
                ("message", Value::string("")),
                ("filename", Value::string("")),
                ("lineno", Value::Number(0.0)),
                ("colno", Value::Number(0.0)),
                ("error", Value::Null),
            ],
        ),
        EventSubclass::Progress => install_fields(
            this,
            &init,
            &[
                ("lengthComputable", Value::Bool(false)),
                ("loaded", Value::Number(0.0)),
                ("total", Value::Number(0.0)),
            ],
        ),
        EventSubclass::Message => install_fields(
            this,
            &init,
            &[
                ("data", Value::Null),
                ("origin", Value::string("")),
                ("lastEventId", Value::string("")),
                ("source", Value::Null),
                ("ports", Value::array(vec![])),
                ("userActivation", Value::Null),
            ],
        ),
        EventSubclass::HashChange => install_fields(
            this,
            &init,
            &[("oldURL", Value::string("")), ("newURL", Value::string(""))],
        ),
        EventSubclass::PopState => {
            install_fields(
                this,
                &init,
                &[
                    ("state", Value::Null),
                    ("hasUAVisualTransition", Value::Bool(false)),
                ],
            );
        }
        EventSubclass::Close => install_fields(
            this,
            &init,
            &[
                ("wasClean", Value::Bool(false)),
                ("code", Value::Number(0.0)),
                ("reason", Value::string("")),
            ],
        ),
        EventSubclass::Blob => {
            require_init_field(&init, "data", "BlobEvent");
            install_fields(
                this,
                &init,
                &[("data", Value::Null), ("timecode", Value::Number(0.0))],
            );
        }
        EventSubclass::Submit => {
            install_fields(this, &init, &[("submitter", Value::Null)]);
        }
        EventSubclass::FormData => {
            require_init_field(&init, "formData", "FormDataEvent");
            install_fields(this, &init, &[("formData", Value::Null)]);
        }
        EventSubclass::Toggle => install_fields(
            this,
            &init,
            &[
                ("oldState", Value::string("")),
                ("newState", Value::string("")),
                ("source", Value::Null),
            ],
        ),
        EventSubclass::Command => install_fields(
            this,
            &init,
            &[("source", Value::Null), ("command", Value::string(""))],
        ),
        EventSubclass::PageTransition => {
            install_fields(this, &init, &[("persisted", Value::Bool(false))]);
        }
        EventSubclass::PromiseRejection => {
            require_init_field(&init, "promise", "PromiseRejectionEvent");
            install_fields(
                this,
                &init,
                &[("promise", Value::Undefined), ("reason", Value::Undefined)],
            );
        }
        EventSubclass::SecurityPolicyViolation => install_fields(
            this,
            &init,
            &[
                ("documentURI", Value::string("")),
                ("referrer", Value::string("")),
                ("blockedURI", Value::string("")),
                ("violatedDirective", Value::string("")),
                ("effectiveDirective", Value::string("")),
                ("originalPolicy", Value::string("")),
                ("sourceFile", Value::string("")),
                ("sample", Value::string("")),
                ("disposition", Value::string("report")),
                ("statusCode", Value::Number(0.0)),
                ("lineNumber", Value::Number(0.0)),
                ("columnNumber", Value::Number(0.0)),
            ],
        ),
        EventSubclass::Track => {
            install_fields(this, &init, &[("track", Value::Null)]);
        }
        EventSubclass::MediaStreamTrack => {
            require_init_field(&init, "track", "MediaStreamTrackEvent");
            install_fields(this, &init, &[("track", Value::Null)]);
        }
        EventSubclass::Storage => {
            install_fields(
                this,
                &init,
                &[
                    ("key", Value::Null),
                    ("oldValue", Value::Null),
                    ("newValue", Value::Null),
                    ("url", Value::string("")),
                    ("storageArea", Value::Null),
                ],
            );
            this.set_property(
                "initStorageEvent",
                realm_event_function(|this, args| {
                    this.call_method("initEvent", args.iter().take(3).cloned().collect());
                    for (index, name) in ["key", "oldValue", "newValue", "url", "storageArea"]
                        .iter()
                        .enumerate()
                    {
                        if let Some(value) = args.get(index + 3) {
                            this.set_property(name, value.clone());
                        }
                    }
                    Value::Undefined
                }),
            );
        }
        EventSubclass::MediaQueryList => {
            install_fields(
                this,
                &init,
                &[
                    ("matches", Value::Bool(false)),
                    ("media", Value::string("")),
                ],
            );
        }
    }
}

fn subclass_kind(name: &str) -> EventSubclass {
    match name {
        "UIEvent" => EventSubclass::Ui,
        "MouseEvent" => EventSubclass::Mouse,
        "KeyboardEvent" => EventSubclass::Keyboard,
        "PointerEvent" => EventSubclass::Pointer,
        "WheelEvent" => EventSubclass::Wheel,
        "FocusEvent" => EventSubclass::Focus,
        "InputEvent" => EventSubclass::Input,
        "CompositionEvent" => EventSubclass::Composition,
        "ClipboardEvent" => EventSubclass::Clipboard,
        "DragEvent" => EventSubclass::Drag,
        "TouchEvent" => EventSubclass::Touch,
        "AnimationEvent" => EventSubclass::Animation,
        "TransitionEvent" => EventSubclass::Transition,
        "ErrorEvent" => EventSubclass::Error,
        "ProgressEvent" => EventSubclass::Progress,
        "MessageEvent" => EventSubclass::Message,
        "HashChangeEvent" => EventSubclass::HashChange,
        "PopStateEvent" => EventSubclass::PopState,
        "CloseEvent" => EventSubclass::Close,
        "BlobEvent" => EventSubclass::Blob,
        "SubmitEvent" => EventSubclass::Submit,
        "FormDataEvent" => EventSubclass::FormData,
        "ToggleEvent" => EventSubclass::Toggle,
        "CommandEvent" => EventSubclass::Command,
        "PageTransitionEvent" => EventSubclass::PageTransition,
        "PromiseRejectionEvent" => EventSubclass::PromiseRejection,
        "SecurityPolicyViolationEvent" => EventSubclass::SecurityPolicyViolation,
        "TrackEvent" => EventSubclass::Track,
        "MediaStreamTrackEvent" => EventSubclass::MediaStreamTrack,
        "StorageEvent" => EventSubclass::Storage,
        "MediaQueryListEvent" => EventSubclass::MediaQueryList,
        _ => EventSubclass::Generic,
    }
}

fn subclass_parent(name: &str) -> &'static str {
    match name {
        "MouseEvent" | "KeyboardEvent" | "FocusEvent" | "InputEvent" | "CompositionEvent"
        | "TouchEvent" => "UIEvent",
        "PointerEvent" | "WheelEvent" | "DragEvent" => "MouseEvent",
        _ => "Event",
    }
}

fn generic_event_members(name: &str) -> &'static str {
    match name {
        "AnimationPlaybackEvent" => "currentTime timelineTime",
        "AudioProcessingEvent" => "inputBuffer outputBuffer playbackTime",
        "BeforeInstallPromptEvent" => "platforms prompt userChoice",
        "BeforeUnloadEvent" => "returnValue",
        "CharacterBoundsUpdateEvent" => "rangeEnd rangeStart",
        "ClipboardChangeEvent" => "changeId types",
        "ContentVisibilityAutoStateChangeEvent" => "skipped",
        "DocumentPictureInPictureEvent" => "window",
        "FontFaceSetLoadEvent" => "fontfaces",
        "GPUUncapturedErrorEvent" => "error",
        "HIDConnectionEvent" => "device",
        "InterestEvent" => "source",
        "MediaEncryptedEvent" => "initData initDataType",
        "MediaStreamEvent" => "stream",
        "OfflineAudioCompletionEvent" => "renderedBuffer",
        "PaymentMethodChangeEvent" => "methodDetails methodName",
        "PaymentRequestUpdateEvent" => "updateWith",
        "PictureInPictureEvent" => "pictureInPictureWindow",
        "RTCDTMFToneChangeEvent" => "tone",
        "RTCDataChannelEvent" => "channel",
        "RTCErrorEvent" => "error",
        "RTCPeerConnectionIceErrorEvent" => "address errorCode errorText hostCandidate port url",
        "RTCPeerConnectionIceEvent" => "candidate",
        "RTCTrackEvent" => "receiver streams track transceiver",
        "SnapEvent" => "snapTargetBlock snapTargetInline",
        "SpeechRecognitionErrorEvent" => "error message",
        "SpeechRecognitionEvent" => "resultIndex results",
        "SpeechSynthesisErrorEvent" => "error",
        "SpeechSynthesisEvent" => "charIndex charLength elapsedTime name utterance",
        "TaskPriorityChangeEvent" => "previousPriority",
        "TextEvent" => "data initTextEvent",
        "TextFormatUpdateEvent" => "getTextFormats",
        "TextUpdateEvent" => "selectionEnd selectionStart text updateRangeEnd updateRangeStart",
        "WebGLContextEvent" => "statusMessage",
        "WindowControlsOverlayGeometryChangeEvent" => "titlebarAreaRect visible",
        "XRInputSourceEvent" => "frame inputSource",
        "XRInputSourcesChangeEvent" => "added removed session",
        "XRLayerEvent" => "layer",
        "XRReferenceSpaceEvent" => "referenceSpace transform",
        "XRSessionEvent" => "session",
        "XRVisibilityMaskChangeEvent" => "eye index indices session vertices",
        _ => "",
    }
}

fn subclass_prototype_members(name: &str) -> (&'static [&'static str], &'static [&'static str]) {
    match name {
        "UIEvent" => (
            &["initUIEvent"],
            &[
                "detail",
                "pseudoTarget",
                "sourceCapabilities",
                "view",
                "which",
            ],
        ),
        "MouseEvent" => (
            &["getModifierState", "initMouseEvent"],
            &[
                "altKey",
                "button",
                "buttons",
                "clientX",
                "clientY",
                "ctrlKey",
                "fromElement",
                "layerX",
                "layerY",
                "metaKey",
                "movementX",
                "movementY",
                "offsetX",
                "offsetY",
                "pageX",
                "pageY",
                "relatedTarget",
                "screenX",
                "screenY",
                "shiftKey",
                "toElement",
                "x",
                "y",
            ],
        ),
        "KeyboardEvent" => (
            &["getModifierState", "initKeyboardEvent"],
            &[
                "DOM_KEY_LOCATION_LEFT",
                "DOM_KEY_LOCATION_NUMPAD",
                "DOM_KEY_LOCATION_RIGHT",
                "DOM_KEY_LOCATION_STANDARD",
                "altKey",
                "charCode",
                "code",
                "ctrlKey",
                "isComposing",
                "key",
                "keyCode",
                "location",
                "metaKey",
                "repeat",
                "shiftKey",
            ],
        ),
        "PointerEvent" => (
            &["getCoalescedEvents", "getPredictedEvents"],
            &[
                "altitudeAngle",
                "azimuthAngle",
                "height",
                "isPrimary",
                "persistentDeviceId",
                "pointerId",
                "pointerType",
                "pressure",
                "tangentialPressure",
                "tiltX",
                "tiltY",
                "twist",
                "width",
            ],
        ),
        "WheelEvent" => (
            &[],
            &[
                "DOM_DELTA_LINE",
                "DOM_DELTA_PAGE",
                "DOM_DELTA_PIXEL",
                "deltaMode",
                "deltaX",
                "deltaY",
                "deltaZ",
                "wheelDelta",
                "wheelDeltaX",
                "wheelDeltaY",
            ],
        ),
        "FocusEvent" => (&[], &["relatedTarget"]),
        "InputEvent" => (
            &["getTargetRanges"],
            &["data", "dataTransfer", "inputType", "isComposing"],
        ),
        "CompositionEvent" => (&["initCompositionEvent"], &["data"]),
        "ClipboardEvent" => (&[], &["clipboardData"]),
        "DragEvent" => (&[], &["dataTransfer"]),
        "TouchEvent" => (
            &[],
            &[
                "altKey",
                "changedTouches",
                "ctrlKey",
                "metaKey",
                "shiftKey",
                "targetTouches",
                "touches",
            ],
        ),
        "AnimationEvent" => (
            &[],
            &[
                "animationName",
                "elapsedTime",
                "pseudoElement",
                "pseudoTarget",
            ],
        ),
        "TransitionEvent" => (
            &[],
            &[
                "elapsedTime",
                "propertyName",
                "pseudoElement",
                "pseudoTarget",
            ],
        ),
        "ErrorEvent" => (&[], &["colno", "error", "filename", "lineno", "message"]),
        "ProgressEvent" => (&[], &["lengthComputable", "loaded", "total"]),
        "MessageEvent" => (
            &["initMessageEvent"],
            &[
                "data",
                "lastEventId",
                "origin",
                "ports",
                "source",
                "userActivation",
            ],
        ),
        "HashChangeEvent" => (&[], &["newURL", "oldURL"]),
        "PopStateEvent" => (&[], &["hasUAVisualTransition", "state"]),
        "CloseEvent" => (&[], &["code", "reason", "wasClean"]),
        "BlobEvent" => (&[], &["data", "timecode"]),
        "SubmitEvent" => (&[], &["submitter"]),
        "FormDataEvent" => (&[], &["formData"]),
        "ToggleEvent" => (&[], &["newState", "oldState", "source"]),
        "CommandEvent" => (&[], &["command", "source"]),
        "PageTransitionEvent" => (&[], &["persisted"]),
        "PromiseRejectionEvent" => (&[], &["promise", "reason"]),
        "SecurityPolicyViolationEvent" => (
            &[],
            &[
                "blockedURI",
                "columnNumber",
                "disposition",
                "documentURI",
                "effectiveDirective",
                "lineNumber",
                "originalPolicy",
                "referrer",
                "sample",
                "sourceFile",
                "statusCode",
                "violatedDirective",
            ],
        ),
        "TrackEvent" | "MediaStreamTrackEvent" => (&[], &["track"]),
        "StorageEvent" => (
            &["initStorageEvent"],
            &["key", "newValue", "oldValue", "storageArea", "url"],
        ),
        "MediaQueryListEvent" => (&[], &["matches", "media"]),
        _ => (&[], &[]),
    }
}

fn install_prototype_members(prototype: &Value, methods: &[&str], properties: &[&str]) {
    for method in methods {
        let returns_array = matches!(
            *method,
            "getCoalescedEvents" | "getPredictedEvents" | "getTargetRanges"
        );
        prototype.set_property(
            method,
            realm_event_function(move |_, _| {
                if returns_array {
                    Value::array(vec![])
                } else {
                    Value::Undefined
                }
            }),
        );
    }
    for property in properties {
        prototype.set_property(property, Value::Undefined);
    }
}

fn build_event_subclasses() -> HashMap<String, Value> {
    let mut constructors = HashMap::new();
    for name in EVENT_SUBCLASS_NAMES {
        let kind = subclass_kind(name);
        let event_name = *name;
        let constructor = realm_event_function(move |this, args| {
            install_subclass(&this, &args, kind);
            let init = arg(&args, 1);
            for member in generic_event_members(event_name).split_whitespace() {
                let value = init_value(&init, member, Value::Undefined);
                this.set_property(member, value);
            }
            Value::Undefined
        });
        constructor.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", constructor.clone());
        let (methods, properties) = subclass_prototype_members(name);
        install_prototype_members(&prototype, methods, properties);
        for member in generic_event_members(name).split_whitespace() {
            prototype.set_property(member, Value::Undefined);
        }
        constructor.set_property("prototype", prototype);
        if *name == "WheelEvent" {
            for (constant, value) in [
                ("DOM_DELTA_PIXEL", 0.0),
                ("DOM_DELTA_LINE", 1.0),
                ("DOM_DELTA_PAGE", 2.0),
            ] {
                constructor.set_property(constant, Value::Number(value));
            }
        }
        if *name == "KeyboardEvent" {
            for (constant, value) in [
                ("DOM_KEY_LOCATION_STANDARD", 0.0),
                ("DOM_KEY_LOCATION_LEFT", 1.0),
                ("DOM_KEY_LOCATION_RIGHT", 2.0),
                ("DOM_KEY_LOCATION_NUMPAD", 3.0),
            ] {
                constructor.set_property(constant, Value::Number(value));
            }
        }
        constructors.insert((*name).to_string(), constructor);
    }
    for name in EVENT_SUBCLASS_NAMES {
        let parent_prototype = if subclass_parent(name) == "Event" {
            event_class().get_property("prototype")
        } else {
            constructors[subclass_parent(name)].get_property("prototype")
        };
        w3cos_core::class::set_prototype_of(
            &constructors[*name].get_property("prototype"),
            &parent_prototype,
        );
    }
    constructors
}

pub fn event_subclass_class(name: &str) -> Value {
    EVENT_SUBCLASSES.with(|slot| {
        if slot.borrow().is_none() {
            *slot.borrow_mut() = Some(build_event_subclasses());
        }
        slot.borrow()
            .as_ref()
            .and_then(|constructors| constructors.get(name))
            .cloned()
            .unwrap_or(Value::Undefined)
    })
}

fn make_event_constructor(custom: bool) -> Value {
    let constructor = realm_event_function(move |this, args| {
        install_event(&this, &args, custom);
        if custom {
            this.set_property(
                "initCustomEvent",
                realm_event_function(|this, args| {
                    this.call_method("initEvent", args.iter().take(3).cloned().collect());
                    this.set_property("detail", arg(&args, 3));
                    Value::Undefined
                }),
            );
        }
        Value::Undefined
    });
    constructor.set_property(
        "name",
        Value::string(if custom { "CustomEvent" } else { "Event" }),
    );
    let prototype = Value::object(HashMap::new());
    prototype.set_property("constructor", constructor.clone());
    if custom {
        install_prototype_members(&prototype, &["initCustomEvent"], &["detail"]);
        w3cos_core::class::set_prototype_of(&prototype, &event_class().get_property("prototype"));
    } else {
        install_prototype_members(
            &prototype,
            &[
                "composedPath",
                "initEvent",
                "preventDefault",
                "stopImmediatePropagation",
                "stopPropagation",
            ],
            &[
                "AT_TARGET",
                "BUBBLING_PHASE",
                "CAPTURING_PHASE",
                "NONE",
                "bubbles",
                "cancelBubble",
                "cancelable",
                "composed",
                "currentTarget",
                "defaultPrevented",
                "eventPhase",
                "returnValue",
                "srcElement",
                "target",
                "timeStamp",
                "type",
            ],
        );
    }
    constructor.set_property("prototype", prototype);
    for (name, value) in [
        ("NONE", 0.0),
        ("CAPTURING_PHASE", 1.0),
        ("AT_TARGET", 2.0),
        ("BUBBLING_PHASE", 3.0),
    ] {
        constructor.set_property(name, Value::Number(value));
    }
    constructor
}

pub fn event_class() -> Value {
    EVENT_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = make_event_constructor(false);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn custom_event_class() -> Value {
    CUSTOM_EVENT_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = make_event_constructor(true);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn event_target_class() -> Value {
    EVENT_TARGET_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = make_event_target_class();
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn make_event_target_class() -> Value {
    let constructor = realm_event_function(|this, _| {
        let listeners: Rc<RefCell<Vec<Listener>>> = Rc::new(RefCell::new(Vec::new()));

        let state = listeners.clone();
        this.set_property(
            "addEventListener",
            realm_event_function(move |_, args| {
                let type_name = arg(&args, 0).to_js_string();
                let callback = arg(&args, 1);
                let options = arg(&args, 2);
                if type_name.is_empty() || !listener_is_callable(&callback) {
                    return Value::Undefined;
                }
                let capture = bool_option(&options, "capture");
                let mut listeners = state.borrow_mut();
                if !listeners.iter().any(|listener| {
                    listener.type_name == type_name
                        && listener.capture == capture
                        && listener.callback.strict_eq(&callback)
                }) {
                    listeners.push(Listener {
                        type_name,
                        callback,
                        capture,
                        once: bool_option(&options, "once"),
                    });
                }
                Value::Undefined
            }),
        );

        let state = listeners.clone();
        this.set_property(
            "removeEventListener",
            realm_event_function(move |_, args| {
                let type_name = arg(&args, 0).to_js_string();
                let callback = arg(&args, 1);
                let capture = bool_option(&arg(&args, 2), "capture");
                state.borrow_mut().retain(|listener| {
                    listener.type_name != type_name
                        || listener.capture != capture
                        || !listener.callback.strict_eq(&callback)
                });
                Value::Undefined
            }),
        );

        let state = listeners.clone();
        this.set_property(
            "dispatchEvent",
            realm_event_function(move |this, args| {
                let event = arg(&args, 0);
                let type_name = event.get_property("type").to_js_string();
                if type_name.is_empty() || type_name == "undefined" {
                    return Value::Bool(true);
                }

                event.set_property("target", this.clone());
                event.set_property("srcElement", this.clone());
                event.set_property("currentTarget", this.clone());
                event.set_property("eventPhase", Value::Number(2.0));
                event.set_property("__sp", Value::Bool(false));
                event.set_property("__sip", Value::Bool(false));

                let property_handler = this.get_property(&format!("on{type_name}"));
                if listener_is_callable(&property_handler) {
                    invoke_listener(&property_handler, &this, event.clone());
                }

                let snapshot = state.borrow().clone();
                for listener in snapshot
                    .iter()
                    .filter(|listener| listener.type_name == type_name)
                {
                    if event.get_property("__sip").to_bool() {
                        break;
                    }
                    if listener.once {
                        state.borrow_mut().retain(|registered| {
                            registered.type_name != listener.type_name
                                || registered.capture != listener.capture
                                || !registered.callback.strict_eq(&listener.callback)
                        });
                    }
                    invoke_listener(&listener.callback, &this, event.clone());
                }

                event.set_property("currentTarget", Value::Null);
                event.set_property("eventPhase", Value::Number(0.0));
                Value::Bool(!event.get_property("__pd").to_bool())
            }),
        );
        this.set_property(
            "when",
            realm_event_function(|this, args| {
                let type_name = arg(&args, 0).to_js_string();
                let options = arg(&args, 1);
                crate::observable_web::observable_from_producer(realm_event_function(
                    move |_, args| {
                        let subscriber = arg(&args, 0);
                        let subscriber_for_event = subscriber.clone();
                        let listener = realm_event_function(move |_, args| {
                            subscriber_for_event.call_method("next", vec![arg(&args, 0)]);
                            Value::Undefined
                        });
                        this.call_method(
                            "addEventListener",
                            vec![Value::string(&type_name), listener.clone(), options.clone()],
                        );
                        let target = this.clone();
                        let teardown_type = type_name.clone();
                        subscriber.call_method(
                            "addTeardown",
                            vec![realm_event_function(move |_, _| {
                                target.call_method(
                                    "removeEventListener",
                                    vec![Value::string(&teardown_type), listener.clone()],
                                );
                                Value::Undefined
                            })],
                        );
                        Value::Undefined
                    },
                ))
            }),
        );
        EVENT_TARGET_INSTANCES.with(|instances| {
            instances.borrow_mut().push(EventTargetBinding {
                value: this,
                listeners,
            });
        });
        Value::Undefined
    });
    constructor.set_property("name", Value::string("EventTarget"));
    let prototype = Value::object(HashMap::new());
    prototype.set_property("constructor", constructor.clone());
    install_prototype_members(
        &prototype,
        &[
            "addEventListener",
            "dispatchEvent",
            "removeEventListener",
            "when",
        ],
        &[],
    );
    constructor.set_property("prototype", prototype);
    constructor
}

pub(crate) fn reset_realm() {
    let bindings =
        EVENT_TARGET_INSTANCES.with(|instances| std::mem::take(&mut *instances.borrow_mut()));
    for binding in bindings {
        binding.listeners.borrow_mut().clear();
        let properties = if let Some(object) = binding.value.as_object() {
            object.borrow().keys()
        } else if let Some(function) = binding.value.as_function() {
            function.keys()
        } else {
            Vec::new()
        };
        for property in properties {
            if property.starts_with("on")
                && listener_is_callable(&binding.value.get_property(&property))
            {
                binding.value.set_property(&property, Value::Null);
            }
        }
        for method in [
            "addEventListener",
            "removeEventListener",
            "dispatchEvent",
            "when",
        ] {
            binding.value.set_property(method, Value::Undefined);
        }
    }

    EVENT_SUBCLASSES.with(|slot| {
        slot.borrow_mut().take();
    });
    for slot in [
        &EVENT_CLASS,
        &CUSTOM_EVENT_CLASS,
        &EVENT_TARGET_CLASS,
        &TOUCH_CLASS,
        &TOUCH_LIST_CLASS,
        &INPUT_DEVICE_CAPABILITIES_CLASS,
    ] {
        slot.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn event_classes_expose_browser_prototype_members() {
        for method in [
            "composedPath",
            "initEvent",
            "preventDefault",
            "stopImmediatePropagation",
            "stopPropagation",
        ] {
            assert!(
                event_class()
                    .get_property("prototype")
                    .get_property(method)
                    .is_function()
            );
        }
        for (name, method) in [
            ("InputEvent", "getTargetRanges"),
            ("KeyboardEvent", "getModifierState"),
            ("MessageEvent", "initMessageEvent"),
            ("MouseEvent", "initMouseEvent"),
            ("PointerEvent", "getCoalescedEvents"),
            ("StorageEvent", "initStorageEvent"),
            ("UIEvent", "initUIEvent"),
        ] {
            assert!(
                event_subclass_class(name)
                    .get_property("prototype")
                    .get_property(method)
                    .is_function(),
                "{name}.{method} should be exposed on the prototype"
            );
        }
        assert!(
            event_target_class()
                .get_property("prototype")
                .get_property("when")
                .is_function()
        );
    }

    #[test]
    fn event_target_when_returns_observable_and_subscribes() {
        let target = w3cos_core::class::construct(&event_target_class(), vec![]);
        let observable = target.call_method("when", vec![Value::string("tick")]);
        assert!(w3cos_core::class::instance_of(
            &observable,
            &crate::observable_web::observable_class()
        ));
        let calls = Rc::new(Cell::new(0));
        let calls_for_next = Rc::clone(&calls);
        let result = observable.call_method(
            "subscribe",
            vec![Value::function(move |_, _| {
                calls_for_next.set(calls_for_next.get() + 1);
                Value::Undefined
            })],
        );
        assert!(result.is_undefined());
        let event = w3cos_core::class::construct(&event_class(), vec![Value::string("tick")]);
        target.call_method("dispatchEvent", vec![event]);
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn event_target_dispatches_cancelable_custom_events_and_once_listeners() {
        let target_class = event_target_class();
        let target = w3cos_core::class::construct(&target_class, vec![]);
        let custom_event_class = custom_event_class();
        let event = w3cos_core::class::construct(
            &custom_event_class,
            vec![
                Value::string("ready"),
                Value::object(HashMap::from([
                    ("detail".to_string(), Value::string("payload")),
                    ("cancelable".to_string(), Value::Bool(true)),
                ])),
            ],
        );

        let calls = Rc::new(Cell::new(0));
        let calls_for_listener = calls.clone();
        let target_for_listener = target.clone();
        target.call_method(
            "addEventListener",
            vec![
                Value::string("ready"),
                Value::function(move |this, args| {
                    calls_for_listener.set(calls_for_listener.get() + 1);
                    assert!(this.strict_eq(&target_for_listener));
                    assert_eq!(
                        arg(&args, 0).get_property("detail").to_js_string(),
                        "payload"
                    );
                    arg(&args, 0).call_method("preventDefault", vec![]);
                    Value::Undefined
                }),
                Value::object(HashMap::from([("once".to_string(), Value::Bool(true))])),
            ],
        );

        assert!(
            !target
                .call_method("dispatchEvent", vec![event.clone()])
                .to_bool()
        );
        assert!(event.get_property("defaultPrevented").to_bool());
        assert!(event.get_property("target").strict_eq(&target));
        assert_eq!(calls.get(), 1);
        assert!(!target.call_method("dispatchEvent", vec![event]).to_bool());
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn remove_event_listener_uses_callback_identity() {
        let target = w3cos_core::class::construct(&event_target_class(), vec![]);
        let calls = Rc::new(Cell::new(0));
        let calls_for_listener = calls.clone();
        let listener = Value::function(move |_, _| {
            calls_for_listener.set(calls_for_listener.get() + 1);
            Value::Undefined
        });
        target.call_method(
            "addEventListener",
            vec![Value::string("tick"), listener.clone()],
        );
        target.call_method("removeEventListener", vec![Value::string("tick"), listener]);
        let event = w3cos_core::class::construct(&event_class(), vec![Value::string("tick")]);
        assert!(target.call_method("dispatchEvent", vec![event]).to_bool());
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn event_subclasses_expose_fields_and_prototype_hierarchy() {
        let keyboard_class = event_subclass_class("KeyboardEvent");
        let keyboard = w3cos_core::class::construct(
            &keyboard_class,
            vec![
                Value::string("keydown"),
                Value::object(HashMap::from([
                    ("key".to_string(), Value::string("Enter")),
                    ("code".to_string(), Value::string("Enter")),
                    ("ctrlKey".to_string(), Value::Bool(true)),
                    ("repeat".to_string(), Value::Bool(true)),
                ])),
            ],
        );
        assert_eq!(keyboard.get_property("key").to_js_string(), "Enter");
        assert_eq!(keyboard.get_property("code").to_js_string(), "Enter");
        assert!(keyboard.get_property("repeat").to_bool());
        assert!(
            keyboard
                .call_method("getModifierState", vec![Value::string("Control")])
                .to_bool()
        );
        assert!(w3cos_core::class::instance_of(
            &keyboard,
            &event_subclass_class("UIEvent")
        ));
        assert!(w3cos_core::class::instance_of(&keyboard, &event_class()));

        let pointer = w3cos_core::class::construct(
            &event_subclass_class("PointerEvent"),
            vec![
                Value::string("pointerdown"),
                Value::object(HashMap::from([
                    ("clientX".to_string(), Value::Number(12.0)),
                    ("pointerId".to_string(), Value::Number(7.0)),
                    ("pointerType".to_string(), Value::string("pen")),
                    ("pressure".to_string(), Value::Number(0.5)),
                ])),
            ],
        );
        assert_eq!(pointer.get_property("clientX").to_number(), 12.0);
        assert_eq!(pointer.get_property("pointerId").to_number(), 7.0);
        assert_eq!(pointer.get_property("pointerType").to_js_string(), "pen");
        assert_eq!(pointer.get_property("pressure").to_number(), 0.5);
        assert!(w3cos_core::class::instance_of(
            &pointer,
            &event_subclass_class("MouseEvent")
        ));
        assert!(w3cos_core::class::instance_of(&pointer, &event_class()));

        let input = w3cos_core::class::construct(
            &event_subclass_class("InputEvent"),
            vec![
                Value::string("input"),
                Value::object(HashMap::from([
                    ("data".to_string(), Value::string("x")),
                    ("inputType".to_string(), Value::string("insertText")),
                    ("isComposing".to_string(), Value::Bool(true)),
                ])),
            ],
        );
        assert_eq!(input.get_property("data").to_js_string(), "x");
        assert_eq!(input.get_property("inputType").to_js_string(), "insertText");
        assert!(input.get_property("isComposing").to_bool());
        assert_eq!(
            input
                .call_method("getTargetRanges", vec![])
                .get_property("length")
                .to_number(),
            0.0
        );
    }

    #[test]
    fn extended_event_subclasses_validate_and_expose_standard_fields() {
        let close = w3cos_core::class::construct(
            &event_subclass_class("CloseEvent"),
            vec![
                Value::string("close"),
                Value::object(HashMap::from([
                    ("wasClean".into(), Value::Bool(true)),
                    ("code".into(), Value::Number(1000.0)),
                    ("reason".into(), Value::string("done")),
                ])),
            ],
        );
        assert!(close.get_property("wasClean").to_bool());
        assert_eq!(close.get_property("code"), 1000.into());
        assert_eq!(close.get_property("reason"), Value::string("done"));
        assert!(w3cos_core::class::instance_of(&close, &event_class()));

        let toggle = w3cos_core::class::construct(
            &event_subclass_class("ToggleEvent"),
            vec![
                Value::string("toggle"),
                Value::object(HashMap::from([
                    ("oldState".into(), Value::string("closed")),
                    ("newState".into(), Value::string("open")),
                ])),
            ],
        );
        assert_eq!(toggle.get_property("oldState"), Value::string("closed"));
        assert_eq!(toggle.get_property("newState"), Value::string("open"));

        let storage = w3cos_core::class::construct(
            &event_subclass_class("StorageEvent"),
            vec![Value::string("storage")],
        );
        storage.call_method(
            "initStorageEvent",
            vec![
                Value::string("storage"),
                Value::Bool(false),
                Value::Bool(false),
                Value::string("key"),
                Value::string("old"),
                Value::string("new"),
                Value::string("https://example.test"),
                Value::Null,
            ],
        );
        assert_eq!(storage.get_property("key"), Value::string("key"));
        assert_eq!(storage.get_property("newValue"), Value::string("new"));

        for (name, field) in [
            ("BlobEvent", "data"),
            ("FormDataEvent", "formData"),
            ("PromiseRejectionEvent", "promise"),
            ("MediaStreamTrackEvent", "track"),
        ] {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                w3cos_core::class::construct(
                    &event_subclass_class(name),
                    vec![Value::string("event")],
                )
            }));
            assert!(result.is_err(), "{name} should require {field}");
        }
    }

    #[test]
    fn ui_events_brand_source_capabilities() {
        let event = w3cos_core::class::construct(
            &event_subclass_class("MouseEvent"),
            vec![
                Value::string("click"),
                Value::object(HashMap::from([(
                    "sourceCapabilities".to_string(),
                    Value::object(HashMap::from([
                        ("firesTouchEvents".to_string(), Value::Bool(true)),
                        ("pointerMovementScrolls".to_string(), Value::Bool(false)),
                    ])),
                )])),
            ],
        );
        let capabilities = event.get_property("sourceCapabilities");
        assert!(w3cos_core::class::instance_of(
            &capabilities,
            &input_device_capabilities_class()
        ));
        assert!(capabilities.get_property("firesTouchEvents").to_bool());
        assert!(
            !capabilities
                .get_property("pointerMovementScrolls")
                .to_bool()
        );
    }

    #[test]
    fn event_classes_targets_and_callbacks_are_realm_owned() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        let old_event_class = event_class();
        let old_custom_event_class = custom_event_class();
        let old_target_class = event_target_class();
        let old_keyboard_class = event_subclass_class("KeyboardEvent");
        let old_touch_class = touch_class();
        let old_touch_list_class = touch_list_class();
        let old_capabilities_class = input_device_capabilities_class();

        let target = w3cos_core::class::construct(&old_target_class, vec![]);
        let listener_marker = Rc::new(());
        let listener_marker_weak = Rc::downgrade(&listener_marker);
        target.call_method(
            "addEventListener",
            vec![
                Value::string("tick"),
                Value::function(move |_, _| {
                    let _ = &listener_marker;
                    Value::Undefined
                }),
            ],
        );
        let handler_marker = Rc::new(());
        let handler_marker_weak = Rc::downgrade(&handler_marker);
        target.set_property(
            "ontick",
            Value::function(move |_, _| {
                let _ = &handler_marker;
                Value::Undefined
            }),
        );
        let event = w3cos_core::class::construct(&old_event_class, vec![Value::string("tick")]);
        assert!(
            target
                .call_method("dispatchEvent", vec![event.clone()])
                .to_bool()
        );

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        assert!(!old_event_class.strict_eq(&event_class()));
        assert!(!old_custom_event_class.strict_eq(&custom_event_class()));
        assert!(!old_target_class.strict_eq(&event_target_class()));
        assert!(!old_keyboard_class.strict_eq(&event_subclass_class("KeyboardEvent")));
        assert!(!old_touch_class.strict_eq(&touch_class()));
        assert!(!old_touch_list_class.strict_eq(&touch_list_class()));
        assert!(!old_capabilities_class.strict_eq(&input_device_capabilities_class()));
        assert!(
            old_target_class
                .call(Value::Undefined, vec![])
                .is_undefined()
        );
        for method in [
            "addEventListener",
            "removeEventListener",
            "dispatchEvent",
            "when",
        ] {
            assert!(target.call_method(method, vec![]).is_undefined());
        }
        assert!(target.get_property("ontick").is_null());
        assert!(event.call_method("preventDefault", vec![]).is_undefined());
        assert!(listener_marker_weak.upgrade().is_none());
        assert!(handler_marker_weak.upgrade().is_none());

        let fresh = w3cos_core::class::construct(&event_target_class(), vec![]);
        assert!(fresh.get_property("dispatchEvent").is_function());
        reset_realm();
    }
}
