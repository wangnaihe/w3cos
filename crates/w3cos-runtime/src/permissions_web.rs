//! Permissions API read-only compatibility snapshots.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Once;

use w3cos_core::Value;

thread_local! {
    static PERMISSIONS_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static PERMISSION_STATUS_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
}

fn error(name: &str, message: &str) -> Value {
    Value::object(HashMap::from([
        ("name".into(), Value::string(name)),
        ("message".into(), Value::string(message)),
    ]))
}

pub fn permission_status_class() -> Value {
    PERMISSION_STATUS_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            w3cos_core::throw_value(error("TypeError", "Illegal constructor: PermissionStatus"))
        });
        class.set_property("name", Value::string("PermissionStatus"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for property in ["name", "onchange", "state"] {
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

pub fn permissions_class() -> Value {
    PERMISSIONS_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            w3cos_core::throw_value(error("TypeError", "Illegal constructor: Permissions"))
        });
        class.set_property("name", Value::string("Permissions"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property("query", Value::Undefined);
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn state_for(name: &str) -> Option<String> {
    match name {
        "notifications" => Some(
            crate::notification_web::notification_class()
                .get_property("permission")
                .to_js_string(),
        ),
        "clipboard-read" | "clipboard-write" => Some("granted".into()),
        "accelerometer" | "gyroscope" | "magnetometer" => Some(
            if crate::orientation_web::permission_granted() {
                "granted"
            } else {
                "prompt"
            }
            .into(),
        ),
        "camera"
        | "microphone"
        | "geolocation"
        | "midi"
        | "push"
        | "persistent-storage"
        | "background-sync"
        | "ambient-light-sensor"
        | "screen-wake-lock"
        | "payment-handler"
        | "idle-detection"
        | "storage-access"
        | "top-level-storage-access"
        | "window-management"
        | "local-fonts"
        | "speaker-selection" => Some("prompt".into()),
        _ => None,
    }
}

fn permission_status(name: &str, state: &str) -> Value {
    let status = w3cos_core::class::construct(&crate::web_events::event_target_class(), vec![]);
    w3cos_core::class::set_prototype_of(
        &status,
        &permission_status_class().get_property("prototype"),
    );
    status.set_property("state", Value::string(state));
    status.set_property("name", Value::string(name));
    status.set_property("onchange", Value::Null);
    status
}

pub fn permissions_value() -> Value {
    let permissions = Value::object(HashMap::new());
    permissions.set_property(
        "query",
        Value::function(|_, args| {
            let descriptor = args.first().cloned().unwrap_or(Value::Undefined);
            if !descriptor.is_object() {
                return w3cos_core::promise::reject(vec![error(
                    "TypeError",
                    "permissions.query requires a permission descriptor",
                )]);
            }
            let name = descriptor.get_property("name").to_js_string();
            let Some(state) = state_for(&name) else {
                return w3cos_core::promise::reject(vec![error(
                    "TypeError",
                    &format!("unsupported permission name: {name}"),
                )]);
            };
            static WARNING: Once = Once::new();
            WARNING.call_once(|| {
                eprintln!(
                    "[w3cos] warning: navigator.permissions returns host-aware compatibility \
                     snapshots; live operating-system policy change notifications require a \
                     platform permission adapter"
                );
            });
            w3cos_core::promise::resolve(vec![permission_status(&name, &state)])
        }),
    );
    w3cos_core::class::set_prototype_of(
        &permissions,
        &permissions_class().get_property("prototype"),
    );
    permissions
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn query_returns_event_target_status_and_rejects_unknown_names() {
        let permissions = permissions_value();
        let status_slot = Rc::new(RefCell::new(Value::Undefined));
        let status_for_then = Rc::clone(&status_slot);
        permissions
            .call_method(
                "query",
                vec![Value::object(HashMap::from([(
                    "name".into(),
                    Value::string("geolocation"),
                )]))],
            )
            .call_method(
                "then",
                vec![Value::function(move |_, args| {
                    *status_for_then.borrow_mut() = args[0].clone();
                    Value::Undefined
                })],
            );
        w3cos_core::promise::drain_microtasks();
        let status = status_slot.borrow().clone();
        assert!(w3cos_core::class::instance_of(
            &status,
            &permission_status_class()
        ));
        assert_eq!(status.get_property("state").to_js_string(), "prompt");
        assert!(status.get_property("addEventListener").is_function());

        let error_name = Rc::new(RefCell::new(String::new()));
        let error_name_for_catch = Rc::clone(&error_name);
        permissions
            .call_method(
                "query",
                vec![Value::object(HashMap::from([(
                    "name".into(),
                    Value::string("unknown-capability"),
                )]))],
            )
            .call_method(
                "catch",
                vec![Value::function(move |_, args| {
                    *error_name_for_catch.borrow_mut() =
                        args[0].get_property("name").to_js_string();
                    Value::Undefined
                })],
            );
        w3cos_core::promise::drain_microtasks();
        assert_eq!(&*error_name.borrow(), "TypeError");
    }
}
