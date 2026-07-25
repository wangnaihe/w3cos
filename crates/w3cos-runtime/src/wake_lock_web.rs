//! Screen Wake Lock API compatibility surface.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Once;

use w3cos_core::Value;

thread_local! {
    static WAKE_LOCK_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static WAKE_LOCK_SENTINEL_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
}

fn error(name: &str, message: &str) -> Value {
    Value::object(HashMap::from([
        ("name".into(), Value::string(name)),
        ("message".into(), Value::string(message)),
    ]))
}

fn illegal_constructor(name: &str) -> Value {
    w3cos_core::throw_value(error("TypeError", &format!("Illegal constructor: {name}")))
}

pub fn wake_lock_sentinel_class() -> Value {
    WAKE_LOCK_SENTINEL_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| illegal_constructor("WakeLockSentinel"));
        class.set_property("name", Value::string("WakeLockSentinel"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property("release", Value::function(|_, _| Value::Undefined));
        for property in ["onrelease", "released", "type"] {
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

fn sentinel_value() -> Value {
    let sentinel = w3cos_core::class::construct(&crate::web_events::event_target_class(), vec![]);
    w3cos_core::class::set_prototype_of(
        &sentinel,
        &wake_lock_sentinel_class().get_property("prototype"),
    );
    sentinel.set_property("type", Value::string("screen"));
    sentinel.set_property("released", Value::Bool(false));
    sentinel.set_property("onrelease", Value::Null);

    let released = Rc::new(Cell::new(false));
    let released_for_call = Rc::clone(&released);
    sentinel.set_property(
        "release",
        Value::function(move |this, _| {
            if !released_for_call.replace(true) {
                this.set_property("released", Value::Bool(true));
                let event = w3cos_core::class::construct(
                    &crate::web_events::event_class(),
                    vec![Value::string("release")],
                );
                this.call_method("dispatchEvent", vec![event]);
            }
            w3cos_core::promise::resolve(vec![Value::Undefined])
        }),
    );
    sentinel
}

pub fn wake_lock_class() -> Value {
    WAKE_LOCK_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| illegal_constructor("WakeLock"));
        class.set_property("name", Value::string("WakeLock"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property(
            "request",
            Value::function(|_, _| {
                w3cos_core::promise::reject(vec![error(
                    "InvalidStateError",
                    "WakeLock.request requires a WakeLock instance",
                )])
            }),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn wake_lock_value() -> Value {
    let wake_lock = Value::object(HashMap::new());
    w3cos_core::class::set_prototype_of(&wake_lock, &wake_lock_class().get_property("prototype"));
    wake_lock.set_property(
        "request",
        Value::function(|_, args| {
            let lock_type = args
                .first()
                .cloned()
                .unwrap_or(Value::Undefined)
                .to_js_string();
            if lock_type != "screen" {
                return w3cos_core::promise::reject(vec![error(
                    "NotSupportedError",
                    "only the screen wake lock type is supported",
                )]);
            }
            static WARNING: Once = Once::new();
            WARNING.call_once(|| {
                eprintln!(
                    "[w3cos] warning: navigator.wakeLock returns a compatibility sentinel; \
                     preventing host display sleep requires a platform power adapter"
                );
            });
            w3cos_core::promise::resolve(vec![sentinel_value()])
        }),
    );
    wake_lock
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_request_resolves_and_release_is_idempotent() {
        let wake_lock = wake_lock_value();
        assert!(w3cos_core::class::instance_of(
            &wake_lock,
            &wake_lock_class()
        ));

        let sentinel_slot = Rc::new(RefCell::new(Value::Undefined));
        let sentinel_for_then = Rc::clone(&sentinel_slot);
        wake_lock
            .call_method("request", vec![Value::string("screen")])
            .call_method(
                "then",
                vec![Value::function(move |_, args| {
                    *sentinel_for_then.borrow_mut() = args[0].clone();
                    Value::Undefined
                })],
            );
        w3cos_core::promise::drain_microtasks();

        let sentinel = sentinel_slot.borrow().clone();
        assert!(w3cos_core::class::instance_of(
            &sentinel,
            &wake_lock_sentinel_class()
        ));
        assert_eq!(sentinel.get_property("type").to_js_string(), "screen");
        assert!(!sentinel.get_property("released").to_bool());

        let releases = Rc::new(Cell::new(0));
        let releases_for_listener = Rc::clone(&releases);
        sentinel.call_method(
            "addEventListener",
            vec![
                Value::string("release"),
                Value::function(move |_, _| {
                    releases_for_listener.set(releases_for_listener.get() + 1);
                    Value::Undefined
                }),
            ],
        );
        sentinel.call_method("release", vec![]);
        sentinel.call_method("release", vec![]);
        assert!(sentinel.get_property("released").to_bool());
        assert_eq!(releases.get(), 1);

        let error_name = Rc::new(RefCell::new(String::new()));
        let error_name_for_catch = Rc::clone(&error_name);
        wake_lock
            .call_method("request", vec![Value::string("system")])
            .call_method(
                "catch",
                vec![Value::function(move |_, args| {
                    *error_name_for_catch.borrow_mut() =
                        args[0].get_property("name").to_js_string();
                    Value::Undefined
                })],
            );
        w3cos_core::promise::drain_microtasks();
        assert_eq!(&*error_name.borrow(), "NotSupportedError");
    }
}
