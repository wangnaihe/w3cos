//! Small browser compatibility interfaces that share no larger subsystem.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use w3cos_core::Value;

use crate::jsdom::{
    WeakRealmObject, disconnect_realm_class, realm_function, register_weak_realm_object,
    upgrade_realm_object,
};

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static VALUES: RefCell<Vec<WeakRealmObject>> = const { RefCell::new(Vec::new()) };
    static COMPAT_WARNING_EMITTED: Cell<bool> = const { Cell::new(false) };
}

const CLASS_NAMES: &[&str] = &[
    "External",
    "FeaturePolicy",
    "DocumentPictureInPicture",
    "MediaError",
    "Origin",
    "NavigatorUAData",
    "PictureInPictureWindow",
    "QuotaExceededError",
    "RadioNodeList",
    "ReportBody",
    "RemotePlayback",
    "TimeRanges",
    "WebSocketError",
];

fn realm_compat_function(f: impl Fn(Value, Vec<Value>) -> Value + 'static) -> Value {
    realm_function(crate::jsdom::realm_generation(), f)
}

fn register_compat_value(value: &Value) {
    register_weak_realm_object(&VALUES, value);
}

fn warn_once() {
    COMPAT_WARNING_EMITTED.with(|warned| {
        if !warned.replace(true) {
            eprintln!(
                "[w3cos] warning: legacy browser integration surfaces return compatible local \
                 values; browser UI, policy enforcement and storage quota telemetry require \
                 host adapters"
            );
        }
    });
}

fn illegal_constructor(name: &'static str) -> Value {
    w3cos_core::throw_value(w3cos_core::error_instance(
        "TypeError",
        vec![Value::string(&format!("Illegal constructor: {name}"))],
    ))
}

fn build_class(name: &'static str) -> Value {
    let class = match name {
        "QuotaExceededError" => realm_compat_function(|_, args| {
            let message = args.first().map(Value::to_js_string).unwrap_or_default();
            let options = args.get(1).cloned().unwrap_or(Value::Undefined);
            let value = w3cos_core::web::dom_exception_instance(&message, "QuotaExceededError");
            value.set_property("quota", options.get_property("quota"));
            value.set_property("requested", options.get_property("requested"));
            w3cos_core::class::set_prototype_of(
                &value,
                &class("QuotaExceededError").get_property("prototype"),
            );
            register_compat_value(&value);
            value
        }),
        "Origin" => realm_compat_function(|_, args| origin_value(args.first().cloned())),
        "WebSocketError" => realm_compat_function(|_, args| {
            let message = args.first().map(Value::to_js_string).unwrap_or_default();
            let options = args.get(1).cloned().unwrap_or(Value::Undefined);
            let value = w3cos_core::web::dom_exception_instance(&message, "WebSocketError");
            value.set_property(
                "closeCode",
                Value::Number(options.get_property("closeCode").to_u32() as f64),
            );
            value.set_property(
                "reason",
                Value::string(&options.get_property("reason").to_js_string()),
            );
            w3cos_core::class::set_prototype_of(
                &value,
                &class("WebSocketError").get_property("prototype"),
            );
            register_compat_value(&value);
            value
        }),
        _ => realm_compat_function(move |_, _| illegal_constructor(name)),
    };
    class.set_property("name", Value::string(name));
    let prototype = Value::object(HashMap::new());
    prototype.set_property("constructor", class.clone());
    for member in class_members(name) {
        prototype.set_property(member, Value::Undefined);
    }
    if matches!(
        name,
        "DocumentPictureInPicture" | "PictureInPictureWindow" | "RemotePlayback"
    ) {
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::web_events::event_target_class().get_property("prototype"),
        );
    } else if matches!(name, "QuotaExceededError" | "WebSocketError") {
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::unsupported::dom_exception_class().get_property("prototype"),
        );
    } else if name == "RadioNodeList" {
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::dom_constructors::prototype("NodeList"),
        );
    }
    class.set_property("prototype", prototype);
    if name == "MediaError" {
        for (constant, value) in [
            ("MEDIA_ERR_ABORTED", 1),
            ("MEDIA_ERR_NETWORK", 2),
            ("MEDIA_ERR_DECODE", 3),
            ("MEDIA_ERR_SRC_NOT_SUPPORTED", 4),
        ] {
            class.set_property(constant, Value::Number(value as f64));
            class
                .get_property("prototype")
                .set_property(constant, Value::Number(value as f64));
        }
    }
    if name == "Origin" {
        class.set_property(
            "from",
            realm_compat_function(|_, args| origin_value(args.first().cloned())),
        );
    }
    class
}

fn class_members(name: &str) -> &'static [&'static str] {
    match name {
        "External" => &["AddSearchProvider", "IsSearchProviderInstalled"],
        "FeaturePolicy" => &[
            "allowedFeatures",
            "allowsFeature",
            "features",
            "getAllowlistForFeature",
        ],
        "DocumentPictureInPicture" => &["onenter", "requestWindow", "window"],
        "MediaError" => &[
            "MEDIA_ERR_ABORTED",
            "MEDIA_ERR_DECODE",
            "MEDIA_ERR_NETWORK",
            "MEDIA_ERR_SRC_NOT_SUPPORTED",
            "code",
            "message",
        ],
        "Origin" => &["isSameOrigin", "isSameSite", "opaque"],
        "NavigatorUAData" => &[
            "brands",
            "getHighEntropyValues",
            "mobile",
            "platform",
            "toJSON",
        ],
        "PictureInPictureWindow" => &["height", "onresize", "width"],
        "QuotaExceededError" => &["quota", "requested"],
        "RadioNodeList" => &["value"],
        "ReportBody" => &["toJSON"],
        "RemotePlayback" => &[
            "cancelWatchAvailability",
            "onconnect",
            "onconnecting",
            "ondisconnect",
            "prompt",
            "state",
            "watchAvailability",
        ],
        "TimeRanges" => &["end", "length", "start"],
        "WebSocketError" => &["closeCode", "reason"],
        _ => &[],
    }
}

pub fn class(name: &'static str) -> Value {
    CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        let class = build_class(name);
        classes.borrow_mut().insert(name.to_string(), class.clone());
        class
    })
}

fn origin_text(input: Option<Value>) -> String {
    let input = input.map(|value| value.to_js_string()).unwrap_or_default();
    let candidate = input
        .split_once("://")
        .and_then(|(scheme, rest)| {
            let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
            (!scheme.is_empty() && !authority.is_empty())
                .then(|| format!("{}://{}", scheme.to_ascii_lowercase(), authority))
        })
        .unwrap_or_default();
    if candidate.is_empty() {
        "null".into()
    } else {
        candidate
    }
}

fn origin_value(input: Option<Value>) -> Value {
    let serialized = origin_text(input);
    let opaque = serialized == "null";
    let value = Value::object(HashMap::from([
        ("opaque".into(), Value::Bool(opaque)),
        (
            "isSameOrigin".into(),
            realm_compat_function({
                let serialized = serialized.clone();
                move |_, args| {
                    Value::Bool(
                        !opaque
                            && origin_text(args.first().cloned()).as_str() == serialized.as_str(),
                    )
                }
            }),
        ),
        (
            "isSameSite".into(),
            realm_compat_function({
                let serialized = serialized.clone();
                move |_, args| {
                    warn_once();
                    Value::Bool(
                        !opaque
                            && origin_text(args.first().cloned()).as_str() == serialized.as_str(),
                    )
                }
            }),
        ),
    ]));
    w3cos_core::class::set_prototype_of(&value, &class("Origin").get_property("prototype"));
    register_compat_value(&value);
    value
}

pub fn time_ranges_value(ranges: Vec<(f64, f64)>) -> Value {
    let ranges = std::rc::Rc::new(ranges);
    let value = Value::object(HashMap::from([(
        "length".into(),
        Value::Number(ranges.len() as f64),
    )]));
    for (method, end) in [("start", false), ("end", true)] {
        let ranges = std::rc::Rc::clone(&ranges);
        value.set_property(
            method,
            realm_compat_function(move |_, args| {
                let index = args.first().map(Value::to_u32).unwrap_or_default() as usize;
                let Some(range) = ranges.get(index) else {
                    w3cos_core::throw_value(w3cos_core::web::dom_exception_instance(
                        "The index is outside the TimeRanges object",
                        "IndexSizeError",
                    ));
                };
                Value::Number(if end { range.1 } else { range.0 })
            }),
        );
    }
    w3cos_core::class::set_prototype_of(&value, &class("TimeRanges").get_property("prototype"));
    register_compat_value(&value);
    value
}

pub fn external_value() -> Value {
    let value = Value::object(HashMap::from([
        (
            "AddSearchProvider".into(),
            realm_compat_function(|_, _| {
                warn_once();
                Value::Undefined
            }),
        ),
        (
            "IsSearchProviderInstalled".into(),
            realm_compat_function(|_, _| {
                warn_once();
                Value::Number(0.0)
            }),
        ),
    ]));
    w3cos_core::class::set_prototype_of(&value, &class("External").get_property("prototype"));
    register_compat_value(&value);
    value
}

pub fn feature_policy_value() -> Value {
    let empty = || Value::array(Vec::new());
    let value = Value::object(HashMap::from([
        (
            "features".into(),
            realm_compat_function(move |_, _| empty()),
        ),
        (
            "allowedFeatures".into(),
            realm_compat_function(|_, _| Value::array(Vec::new())),
        ),
        (
            "getAllowlistForFeature".into(),
            realm_compat_function(|_, _| Value::array(Vec::new())),
        ),
        (
            "allowsFeature".into(),
            realm_compat_function(|_, _| {
                warn_once();
                Value::Bool(false)
            }),
        ),
    ]));
    w3cos_core::class::set_prototype_of(&value, &class("FeaturePolicy").get_property("prototype"));
    register_compat_value(&value);
    value
}

pub fn navigator_ua_data_value() -> Value {
    let brands = Value::array(vec![Value::object(HashMap::from([
        ("brand".into(), Value::string("W3COS")),
        ("version".into(), Value::string("0")),
    ]))]);
    let value = Value::object(HashMap::from([
        ("brands".into(), brands.clone()),
        ("mobile".into(), Value::Bool(false)),
        ("platform".into(), Value::string("w3cos")),
    ]));
    let json_value = value.clone();
    value.set_property(
        "toJSON",
        realm_compat_function(move |_, _| {
            Value::object(HashMap::from([
                ("brands".into(), json_value.get_property("brands")),
                ("mobile".into(), json_value.get_property("mobile")),
                ("platform".into(), json_value.get_property("platform")),
            ]))
        }),
    );
    let entropy_value = value.clone();
    value.set_property(
        "getHighEntropyValues",
        realm_compat_function(move |_, args| {
            warn_once();
            let result = entropy_value.call_method("toJSON", Vec::new());
            for hint in args.first().cloned().unwrap_or(Value::Undefined).iter() {
                let name = hint.to_js_string();
                let neutral = match name.as_str() {
                    "architecture" | "bitness" | "model" | "platformVersion" => Value::string(""),
                    "fullVersionList" => Value::array(Vec::new()),
                    "wow64" => Value::Bool(false),
                    _ => continue,
                };
                result.set_property(&name, neutral);
            }
            w3cos_core::promise::resolve(vec![result])
        }),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &class("NavigatorUAData").get_property("prototype"),
    );
    register_compat_value(&value);
    value
}

pub fn remote_playback_value() -> Value {
    let value = w3cos_core::class::construct(&crate::web_events::event_target_class(), Vec::new());
    for (name, property) in [
        ("state", Value::string("disconnected")),
        ("onconnect", Value::Null),
        ("onconnecting", Value::Null),
        ("ondisconnect", Value::Null),
    ] {
        value.set_property(name, property);
    }
    value.set_property(
        "watchAvailability",
        realm_compat_function(|_, args| {
            warn_once();
            let callback = args.first().cloned().unwrap_or(Value::Undefined);
            if callback.is_function() {
                callback.call(Value::Undefined, vec![Value::Bool(false)]);
            }
            w3cos_core::promise::resolve(vec![Value::Number(1.0)])
        }),
    );
    value.set_property(
        "cancelWatchAvailability",
        realm_compat_function(|_, _| w3cos_core::promise::resolve(vec![Value::Undefined])),
    );
    value.set_property(
        "prompt",
        realm_compat_function(|_, _| {
            warn_once();
            w3cos_core::promise::reject(vec![w3cos_core::web::dom_exception_instance(
                "No remote playback device is available",
                "NotFoundError",
            )])
        }),
    );
    w3cos_core::class::set_prototype_of(&value, &class("RemotePlayback").get_property("prototype"));
    register_compat_value(&value);
    value
}

pub fn document_picture_in_picture_value() -> Value {
    let value = w3cos_core::class::construct(&crate::web_events::event_target_class(), Vec::new());
    value.set_property("onenter", Value::Null);
    value.set_property("window", Value::Null);
    value.set_property(
        "requestWindow",
        realm_compat_function(|_, _| {
            warn_once();
            w3cos_core::promise::reject(vec![w3cos_core::web::dom_exception_instance(
                "Document Picture-in-Picture requires native multi-window integration",
                "NotSupportedError",
            )])
        }),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &class("DocumentPictureInPicture").get_property("prototype"),
    );
    register_compat_value(&value);
    value
}

pub fn reset() {
    VALUES.with(|values| {
        for value in values
            .borrow_mut()
            .drain(..)
            .filter_map(|value| upgrade_realm_object(&value))
        {
            for name in CLASS_NAMES {
                for member in class_members(name) {
                    value.set_property(member, Value::Undefined);
                }
            }
        }
    });
    let classes = CLASSES.with(|classes| std::mem::take(&mut *classes.borrow_mut()));
    for class in classes.into_values() {
        for property in [
            "from",
            "MEDIA_ERR_ABORTED",
            "MEDIA_ERR_NETWORK",
            "MEDIA_ERR_DECODE",
            "MEDIA_ERR_SRC_NOT_SUPPORTED",
        ] {
            class.set_property(property, Value::Undefined);
        }
        disconnect_realm_class(class);
    }
    COMPAT_WARNING_EMITTED.with(|warned| warned.set(false));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_ranges_and_origins_have_browser_behavior() {
        reset();
        let ranges = time_ranges_value(vec![(1.5, 4.0)]);
        assert_eq!(ranges.get_property("length").to_number(), 1.0);
        assert_eq!(
            ranges
                .call_method("start", vec![Value::Number(0.0)])
                .to_number(),
            1.5
        );
        let origin = class("Origin").call(
            Value::Undefined,
            vec![Value::string("https://example.com/path")],
        );
        assert!(!origin.get_property("opaque").to_bool());
        assert!(
            origin
                .call_method(
                    "isSameOrigin",
                    vec![Value::string("https://example.com/other")]
                )
                .to_bool()
        );
    }

    #[test]
    fn values_methods_callbacks_and_classes_are_realm_owned() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        let old_origin_class = class("Origin");
        let old_remote_class = class("RemotePlayback");
        let origin = origin_value(Some(Value::string("https://example.test/path")));
        let ua = navigator_ua_data_value();
        let remote = remote_playback_value();

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        assert!(old_origin_class.get_property("prototype").is_undefined());
        assert!(old_origin_class.get_property("from").is_undefined());
        assert!(old_remote_class.get_property("prototype").is_undefined());
        assert!(!old_origin_class.strict_eq(&class("Origin")));
        assert!(origin.get_property("isSameOrigin").is_undefined());
        assert!(ua.get_property("brands").is_undefined());
        assert!(ua.get_property("toJSON").is_undefined());
        assert!(remote.get_property("watchAvailability").is_undefined());
        assert!(remote.get_property("onconnect").is_undefined());
    }
}
