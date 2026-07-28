//! Push API capability facade.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use w3cos_core::Value;

use crate::jsdom::{
    WeakRealmObject, disconnect_realm_class, realm_function, register_weak_realm_object,
    upgrade_realm_object,
};

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static MANAGERS: RefCell<Vec<WeakRealmObject>> = const { RefCell::new(Vec::new()) };
    static WARNING_EMITTED: Cell<bool> = const { Cell::new(false) };
}

const CLASS_NAMES: &[&str] = &["PushManager", "PushSubscription", "PushSubscriptionOptions"];

fn realm_push_function(f: impl Fn(Value, Vec<Value>) -> Value + 'static) -> Value {
    realm_function(crate::jsdom::realm_generation(), f)
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
    let class = realm_push_function(move |_, _| {
        w3cos_core::throw_value(w3cos_core::error_instance(
            "TypeError",
            vec![Value::string(&format!("Illegal constructor: {name}"))],
        ))
    });
    class.set_property("name", Value::string(name));
    let prototype = Value::object(HashMap::new());
    prototype.set_property("constructor", class.clone());
    for member in class_members(name) {
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

fn class_members(name: &str) -> &'static [&'static str] {
    match name {
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
    }
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
            realm_push_function(|_, _| {
                warning();
                w3cos_core::promise::resolve(vec![Value::Null])
            }),
        ),
        (
            "permissionState".into(),
            realm_push_function(|_, _| {
                warning();
                w3cos_core::promise::resolve(vec![Value::string("denied")])
            }),
        ),
        (
            "subscribe".into(),
            realm_push_function(|_, _| {
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
    register_weak_realm_object(&MANAGERS, &manager);
    manager
}

pub fn reset() {
    MANAGERS.with(|managers| {
        for manager in managers
            .borrow_mut()
            .drain(..)
            .filter_map(|manager| upgrade_realm_object(&manager))
        {
            for name in CLASS_NAMES {
                for member in class_members(name) {
                    manager.set_property(member, Value::Undefined);
                }
            }
        }
    });
    let classes = CLASSES.with(|classes| std::mem::take(&mut *classes.borrow_mut()));
    for class in classes.into_values() {
        class.set_property("supportedContentEncodings", Value::Undefined);
        disconnect_realm_class(class);
    }
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

    #[test]
    fn manager_methods_and_classes_are_realm_owned() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        let old_class = class_for("PushManager");
        let manager = push_manager_value();

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        assert!(old_class.get_property("prototype").is_undefined());
        assert!(
            old_class
                .get_property("supportedContentEncodings")
                .is_undefined()
        );
        assert!(!old_class.strict_eq(&class_for("PushManager")));
        assert!(manager.get_property("subscribe").is_undefined());
        assert!(manager.get_property("permissionState").is_undefined());
    }
}
