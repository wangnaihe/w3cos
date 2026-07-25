//! Encrypted Media Extensions compatibility surface.
//!
//! A content-decryption module is deliberately required before access can be
//! granted. The default runtime validates requests and rejects them explicitly
//! instead of advertising DRM playback that cannot be performed.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Once;

use w3cos_core::Value;

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
}

fn error(name: &str, message: &str) -> Value {
    Value::object(HashMap::from([
        ("name".into(), Value::string(name)),
        ("message".into(), Value::string(message)),
    ]))
}

fn illegal_class(name: &'static str, parent: Option<Value>) -> Value {
    CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        let class = Value::function(move |_, _| {
            w3cos_core::throw_value(error("TypeError", &format!("Illegal constructor: {name}")))
        });
        class.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        if let Some(parent) = parent {
            w3cos_core::class::set_prototype_of(&prototype, &parent.get_property("prototype"));
        }
        class.set_property("prototype", prototype);
        classes.borrow_mut().insert(name.into(), class.clone());
        class
    })
}

pub fn media_key_session_class() -> Value {
    let class = illegal_class(
        "MediaKeySession",
        Some(crate::web_events::event_target_class()),
    );
    for member in [
        "close",
        "closed",
        "expiration",
        "generateRequest",
        "keyStatuses",
        "load",
        "onkeystatuseschange",
        "onmessage",
        "remove",
        "sessionId",
        "update",
    ] {
        class
            .get_property("prototype")
            .set_property(member, Value::Undefined);
    }
    class
}

pub fn media_key_status_map_class() -> Value {
    let class = illegal_class("MediaKeyStatusMap", None);
    for member in ["entries", "forEach", "get", "has", "keys", "size", "values"] {
        class
            .get_property("prototype")
            .set_property(member, Value::Undefined);
    }
    class
}

pub fn media_key_system_access_class() -> Value {
    let class = illegal_class("MediaKeySystemAccess", None);
    for member in ["createMediaKeys", "getConfiguration", "keySystem"] {
        class
            .get_property("prototype")
            .set_property(member, Value::Undefined);
    }
    class
}

pub fn media_keys_class() -> Value {
    let class = illegal_class("MediaKeys", None);
    for member in [
        "createSession",
        "getStatusForPolicy",
        "setServerCertificate",
    ] {
        class
            .get_property("prototype")
            .set_property(member, Value::Undefined);
    }
    class
}

pub fn media_key_message_event_class() -> Value {
    CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get("MediaKeyMessageEvent").cloned() {
            return class;
        }
        let class = Value::function(|this, args| {
            crate::web_events::event_class().call(this.clone(), args.clone());
            let init = args.get(1).cloned().unwrap_or(Value::Undefined);
            let message_type = init.get_property("messageType").to_js_string();
            if !matches!(
                message_type.as_str(),
                "license-request"
                    | "license-renewal"
                    | "license-release"
                    | "individualization-request"
            ) {
                w3cos_core::throw_value(error(
                    "TypeError",
                    "MediaKeyMessageEvent requires a valid messageType",
                ));
            }
            let message = init.get_property("message");
            if message.is_undefined() || message.is_null() {
                w3cos_core::throw_value(error(
                    "TypeError",
                    "MediaKeyMessageEvent requires message data",
                ));
            }
            this.set_property("messageType", Value::string(&message_type));
            this.set_property("message", message);
            Value::Undefined
        });
        class.set_property("name", Value::string("MediaKeyMessageEvent"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property("message", Value::Undefined);
        prototype.set_property("messageType", Value::Undefined);
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::web_events::event_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        classes
            .borrow_mut()
            .insert("MediaKeyMessageEvent".into(), class.clone());
        class
    })
}

pub fn request_media_key_system_access_value() -> Value {
    Value::function(|_, args| {
        let key_system = args
            .first()
            .cloned()
            .unwrap_or(Value::Undefined)
            .to_js_string();
        let configurations = args.get(1).cloned().unwrap_or(Value::Undefined);
        if key_system.trim().is_empty() {
            return w3cos_core::promise::reject(vec![error(
                "TypeError",
                "keySystem must be a non-empty string",
            )]);
        }
        if configurations.get_property("length").to_u32() == 0 {
            return w3cos_core::promise::reject(vec![error(
                "TypeError",
                "supportedConfigurations must contain at least one configuration",
            )]);
        }
        static WARNING: Once = Once::new();
        WARNING.call_once(|| {
            eprintln!(
                "[w3cos] warning: requestMediaKeySystemAccess is unavailable until a platform \
                 CDM, secure decoder and license-policy adapter are configured"
            );
        });
        w3cos_core::promise::reject(vec![error(
            "NotSupportedError",
            "no content-decryption module is configured for this runtime",
        )])
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn validates_requests_and_reports_missing_cdm_explicitly() {
        let names = Rc::new(RefCell::new(Vec::<String>::new()));
        for args in [
            vec![
                Value::string(""),
                Value::array(vec![Value::object(HashMap::new())]),
            ],
            vec![Value::string("org.example.cdm"), Value::array(vec![])],
            vec![
                Value::string("org.example.cdm"),
                Value::array(vec![Value::object(HashMap::new())]),
            ],
        ] {
            let names_for_handler = Rc::clone(&names);
            request_media_key_system_access_value()
                .call(Value::Undefined, args)
                .call_method(
                    "catch",
                    vec![Value::function(move |_, args| {
                        names_for_handler
                            .borrow_mut()
                            .push(args[0].get_property("name").to_js_string());
                        Value::Undefined
                    })],
                );
        }
        crate::jsdom::drain_microtasks();
        assert_eq!(
            &*names.borrow(),
            &["TypeError", "TypeError", "NotSupportedError"]
        );
        assert!(media_key_message_event_class().is_function());
        assert!(media_key_session_class().is_function());
    }
}
