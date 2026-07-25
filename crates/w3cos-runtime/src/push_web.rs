//! Push API capability facade.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use w3cos_core::Value;

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static WARNING_EMITTED: Cell<bool> = const { Cell::new(false) };
}

fn warning() {
    WARNING_EMITTED.with(|warned| {
        if !warned.replace(true) {
            eprintln!(
                "[w3cos] warning: Push API discovery is available, but subscription requires a \
                 Service Worker realm, user permission and a platform push-service adapter"
            );
        }
    });
}

fn build_class(name: &'static str) -> Value {
    let class = Value::function(move |_, _| {
        w3cos_core::throw_value(w3cos_core::error_instance(
            "TypeError",
            vec![Value::string(&format!("Illegal constructor: {name}"))],
        ))
    });
    class.set_property("name", Value::string(name));
    let prototype = Value::object(HashMap::new());
    prototype.set_property("constructor", class.clone());
    let members: &[&str] = match name {
        "PushManager" => &["getSubscription", "permissionState", "subscribe"],
        "PushSubscription" => &[
            "endpoint",
            "expirationTime",
            "getKey",
            "options",
            "toJSON",
            "unsubscribe",
        ],
        "PushSubscriptionOptions" => &["applicationServerKey", "userVisibleOnly"],
        _ => &[],
    };
    for member in members {
        prototype.set_property(member, Value::Undefined);
    }
    class.set_property("prototype", prototype);
    if name == "PushManager" {
        class.set_property(
            "supportedContentEncodings",
            Value::array(vec![Value::string("aes128gcm")]),
        );
    }
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

pub fn push_manager_value() -> Value {
    let manager = Value::object(HashMap::from([
        (
            "getSubscription".into(),
            Value::function(|_, _| {
                warning();
                w3cos_core::promise::resolve(vec![Value::Null])
            }),
        ),
        (
            "permissionState".into(),
            Value::function(|_, _| {
                warning();
                w3cos_core::promise::resolve(vec![Value::string("denied")])
            }),
        ),
        (
            "subscribe".into(),
            Value::function(|_, _| {
                warning();
                w3cos_core::promise::reject(vec![w3cos_core::web::dom_exception_instance(
                    "No platform push service is registered",
                    "NotSupportedError",
                )])
            }),
        ),
    ]));
    w3cos_core::class::set_prototype_of(
        &manager,
        &class_for("PushManager").get_property("prototype"),
    );
    manager
}

pub fn reset() {
    CLASSES.with(|classes| classes.borrow_mut().clear());
    WARNING_EMITTED.with(|warned| warned.set(false));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    #[test]
    fn push_manager_reports_no_subscription_and_denied_permission() {
        let manager = push_manager_value();
        let values = Rc::new(RefCell::new(Vec::<String>::new()));
        for method in ["getSubscription", "permissionState"] {
            let values = Rc::clone(&values);
            manager.call_method(method, Vec::new()).call_method(
                "then",
                vec![Value::function(move |_, args| {
                    values.borrow_mut().push(if args[0].is_null() {
                        "null".into()
                    } else {
                        args[0].to_js_string()
                    });
                    Value::Undefined
                })],
            );
        }
        w3cos_core::promise::drain_microtasks();
        assert_eq!(&*values.borrow(), &["null", "denied"]);
    }
}
