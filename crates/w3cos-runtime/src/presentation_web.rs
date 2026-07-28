//! Presentation API compatibility surface.
//!
//! Availability truthfully reports false without a remote-display adapter.
//! Start/reconnect requests reject explicitly after validating their inputs.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Once;

use crate::jsdom::realm_function;
use w3cos_core::Value;

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
}

fn error(name: &str, message: &str) -> Value {
    w3cos_core::class::construct(
        &crate::unsupported::dom_exception_class(),
        vec![Value::string(message), Value::string(name)],
    )
}

fn warn_unavailable() {
    static WARNING: Once = Once::new();
    WARNING.call_once(|| {
        eprintln!(
            "[w3cos] warning: Presentation API discovery, user selection and remote-display \
             transport require a platform presentation adapter"
        );
    });
}

fn cached_class(name: &'static str, build: impl FnOnce() -> Value) -> Value {
    CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        let class = build();
        classes.borrow_mut().insert(name.into(), class.clone());
        class
    })
}

fn illegal_event_target_class(name: &'static str, members: &[(&str, Value)]) -> Value {
    cached_class(name, || {
        let generation = crate::jsdom::realm_generation();
        let class = realm_function(generation, move |_, _| {
            w3cos_core::throw_value(error("TypeError", &format!("Illegal constructor: {name}")))
        });
        class.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for (member, value) in members {
            prototype.set_property(member, value.clone());
        }
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::web_events::event_target_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        class
    })
}

fn rejected_connection(operation: &str) -> Value {
    warn_unavailable();
    w3cos_core::promise::reject(vec![error(
        "NotFoundError",
        &format!("{operation} could not find an available presentation display"),
    )])
}

pub fn presentation_class() -> Value {
    illegal_event_target_class(
        "Presentation",
        &[("defaultRequest", Value::Null), ("receiver", Value::Null)],
    )
}

pub fn presentation_value() -> Value {
    let value = Value::object(HashMap::from([
        ("defaultRequest".into(), Value::Null),
        ("receiver".into(), Value::Null),
    ]));
    w3cos_core::class::set_prototype_of(&value, &presentation_class().get_property("prototype"));
    value
}

pub fn presentation_availability_class() -> Value {
    illegal_event_target_class(
        "PresentationAvailability",
        &[("value", Value::Bool(false)), ("onchange", Value::Null)],
    )
}

fn availability_value() -> Value {
    let value = Value::object(HashMap::from([
        ("value".into(), Value::Bool(false)),
        ("onchange".into(), Value::Null),
    ]));
    crate::web_events::event_target_class().call(value.clone(), vec![]);
    w3cos_core::class::set_prototype_of(
        &value,
        &presentation_availability_class().get_property("prototype"),
    );
    value
}

fn request_urls(value: Value) -> Result<Vec<Value>, Value> {
    let values = if matches!(value, Value::Array(_)) {
        value.iter().collect::<Vec<_>>()
    } else if value.is_undefined() || value.is_null() {
        vec![]
    } else {
        vec![value]
    };
    if values.is_empty()
        || values
            .iter()
            .any(|value| value.to_js_string().trim().is_empty())
    {
        return Err(error(
            "TypeError",
            "PresentationRequest requires at least one non-empty URL",
        ));
    }
    Ok(values
        .into_iter()
        .map(|value| Value::string(&value.to_js_string()))
        .collect())
}

pub fn presentation_request_class() -> Value {
    cached_class("PresentationRequest", || {
        let generation = crate::jsdom::realm_generation();
        let class = realm_function(generation, move |this, args| {
            let urls = match request_urls(args.first().cloned().unwrap_or(Value::Undefined)) {
                Ok(urls) => urls,
                Err(reason) => w3cos_core::throw_value(reason),
            };
            crate::web_events::event_target_class().call(this.clone(), vec![]);
            this.set_property("__urls", Value::array(urls));
            this.set_property("onconnectionavailable", Value::Null);
            this.set_property(
                "getAvailability",
                realm_function(generation, |_, _| {
                    warn_unavailable();
                    w3cos_core::promise::resolve(vec![availability_value()])
                }),
            );
            this.set_property(
                "start",
                realm_function(generation, |_, _| {
                    rejected_connection("PresentationRequest.start")
                }),
            );
            this.set_property(
                "reconnect",
                realm_function(generation, |_, args| {
                    let id = args.first().cloned().unwrap_or(Value::Undefined);
                    if id.is_undefined() || id.is_null() || id.to_js_string().trim().is_empty() {
                        return w3cos_core::promise::reject(vec![error(
                            "TypeError",
                            "PresentationRequest.reconnect requires a connection id",
                        )]);
                    }
                    rejected_connection("PresentationRequest.reconnect")
                }),
            );
            Value::Undefined
        });
        class.set_property("name", Value::string("PresentationRequest"));
        let prototype = Value::object(HashMap::from([
            ("constructor".into(), class.clone()),
            ("onconnectionavailable".into(), Value::Null),
            (
                "getAvailability".into(),
                realm_function(generation, |_, _| {
                    warn_unavailable();
                    w3cos_core::promise::resolve(vec![availability_value()])
                }),
            ),
            (
                "start".into(),
                realm_function(generation, |_, _| {
                    rejected_connection("PresentationRequest.start")
                }),
            ),
            (
                "reconnect".into(),
                realm_function(generation, |_, args| {
                    let id = args.first().cloned().unwrap_or(Value::Undefined);
                    if id.is_undefined() || id.is_null() || id.to_js_string().trim().is_empty() {
                        return w3cos_core::promise::reject(vec![error(
                            "TypeError",
                            "PresentationRequest.reconnect requires a connection id",
                        )]);
                    }
                    rejected_connection("PresentationRequest.reconnect")
                }),
            ),
        ]));
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::web_events::event_target_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        class
    })
}

pub fn presentation_connection_class() -> Value {
    let generation = crate::jsdom::realm_generation();
    illegal_event_target_class(
        "PresentationConnection",
        &[
            ("id", Value::string("")),
            ("url", Value::string("")),
            ("state", Value::string("closed")),
            ("binaryType", Value::string("arraybuffer")),
            ("onconnect", Value::Null),
            ("onclose", Value::Null),
            ("onterminate", Value::Null),
            ("onmessage", Value::Null),
            ("close", realm_function(generation, |_, _| Value::Undefined)),
            (
                "terminate",
                realm_function(generation, |_, _| Value::Undefined),
            ),
            (
                "send",
                realm_function(generation, |_, _| {
                    w3cos_core::throw_value(error(
                        "InvalidStateError",
                        "PresentationConnection is not connected",
                    ))
                }),
            ),
        ],
    )
}

pub fn presentation_connection_list_class() -> Value {
    illegal_event_target_class(
        "PresentationConnectionList",
        &[
            ("connections", Value::array(vec![])),
            ("onconnectionavailable", Value::Null),
        ],
    )
}

pub fn presentation_receiver_class() -> Value {
    illegal_event_target_class(
        "PresentationReceiver",
        &[(
            "connectionList",
            w3cos_core::promise::resolve(vec![Value::object(HashMap::new())]),
        )],
    )
}

fn event_class(name: &'static str, fields: &'static [&'static str]) -> Value {
    cached_class(name, || {
        let generation = crate::jsdom::realm_generation();
        let class = realm_function(generation, move |this, args| {
            crate::web_events::event_class().call(this.clone(), args.clone());
            let init = args.get(1).cloned().unwrap_or(Value::Undefined);
            for field in fields {
                this.set_property(field, init.get_property(field));
            }
            Value::Undefined
        });
        class.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for field in fields {
            prototype.set_property(field, Value::Undefined);
        }
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::web_events::event_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        class
    })
}

pub fn reset_realm() {
    CLASSES.with(|classes| classes.borrow_mut().clear());
}

pub fn presentation_connection_available_event_class() -> Value {
    event_class("PresentationConnectionAvailableEvent", &["connection"])
}

pub fn presentation_connection_close_event_class() -> Value {
    event_class("PresentationConnectionCloseEvent", &["reason", "message"])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    #[test]
    fn availability_is_false_and_connection_requests_fail_explicitly() {
        reset_realm();
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let request = w3cos_core::class::construct(
            &presentation_request_class(),
            vec![Value::string("https://example.test/presentation")],
        );
        let log = Rc::new(RefCell::new(Vec::new()));
        let log_for_availability = Rc::clone(&log);
        request.call_method("getAvailability", vec![]).call_method(
            "then",
            vec![Value::function(move |_, args| {
                log_for_availability.borrow_mut().push(format!(
                    "{}:{}",
                    w3cos_core::class::instance_of(&args[0], &presentation_availability_class()),
                    args[0].get_property("value").to_bool()
                ));
                Value::Undefined
            })],
        );
        for (method, args) in [
            ("start", vec![]),
            ("reconnect", vec![]),
            ("reconnect", vec![Value::string("missing")]),
        ] {
            let log_for_error = Rc::clone(&log);
            request.call_method(method, args).call_method(
                "catch",
                vec![Value::function(move |_, args| {
                    log_for_error
                        .borrow_mut()
                        .push(args[0].get_property("name").to_js_string());
                    Value::Undefined
                })],
            );
        }
        crate::jsdom::drain_microtasks();
        assert_eq!(
            &*log.borrow(),
            &["true:false", "NotFoundError", "TypeError", "NotFoundError"]
        );
        reset_realm();
    }

    #[test]
    fn constructors_and_request_methods_are_realm_owned() {
        reset_realm();
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let old_class = presentation_request_class();
        let old_event_class = presentation_connection_close_event_class();
        let request = w3cos_core::class::construct(
            &old_class,
            vec![Value::string("https://example.test/presentation")],
        );
        old_class
            .get_property("prototype")
            .set_property("oldRealmMarker", Value::Bool(true));

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let new_class = presentation_request_class();
        let new_event_class = presentation_connection_close_event_class();
        assert!(!old_class.strict_eq(&new_class));
        assert!(!old_event_class.strict_eq(&new_event_class));
        assert!(
            new_class
                .get_property("prototype")
                .get_property("oldRealmMarker")
                .is_undefined()
        );
        assert!(old_class.call(Value::Undefined, vec![]).is_undefined());
        assert!(
            old_event_class
                .call(Value::Undefined, vec![])
                .is_undefined()
        );
        for method in ["getAvailability", "start", "reconnect"] {
            assert!(request.call_method(method, vec![]).is_undefined());
        }
        assert!(
            w3cos_core::class::construct(
                &new_class,
                vec![Value::string("https://example.test/presentation")]
            )
            .call_method("getAvailability", vec![])
            .is_object()
        );
        reset_realm();
    }
}
