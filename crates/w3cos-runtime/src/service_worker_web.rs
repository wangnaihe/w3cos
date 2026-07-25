//! Service Worker container compatibility surface.
//!
//! Isolated worker realms and script execution are not available in the
//! compact runtime yet, so registration is explicitly rejected while
//! discovery APIs return truthful empty results.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Once;

use w3cos_core::Value;

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static CONTAINER: RefCell<Option<Value>> = const { RefCell::new(None) };
}

fn error(name: &str, message: &str) -> Value {
    Value::object(HashMap::from([
        ("name".into(), Value::string(name)),
        ("message".into(), Value::string(message)),
    ]))
}

fn illegal_class(name: &'static str) -> Value {
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
        let members: &[&str] = match name {
            "ServiceWorker" => &[
                "scriptURL",
                "state",
                "onstatechange",
                "onerror",
                "postMessage",
            ],
            "ServiceWorkerContainer" => &[
                "controller",
                "ready",
                "oncontrollerchange",
                "onmessage",
                "onmessageerror",
                "register",
                "getRegistration",
                "getRegistrations",
                "startMessages",
            ],
            "ServiceWorkerRegistration" => &[
                "installing",
                "waiting",
                "active",
                "backgroundFetch",
                "cookies",
                "navigationPreload",
                "paymentManager",
                "periodicSync",
                "pushManager",
                "scope",
                "sync",
                "updateViaCache",
                "onupdatefound",
                "update",
                "unregister",
                "showNotification",
                "getNotifications",
            ],
            "BackgroundFetchManager" => &["fetch", "get", "getIds"],
            "BackgroundFetchRecord" => &["request", "responseReady"],
            "BackgroundFetchRegistration" => &[
                "abort",
                "downloadTotal",
                "downloaded",
                "failureReason",
                "id",
                "match",
                "matchAll",
                "onprogress",
                "recordsAvailable",
                "result",
                "uploadTotal",
                "uploaded",
            ],
            "CookieStoreManager" => &["getSubscriptions", "subscribe", "unsubscribe"],
            "NavigationPreloadManager" => &["disable", "enable", "getState", "setHeaderValue"],
            "PeriodicSyncManager" => &["getTags", "register", "unregister"],
            "SyncManager" => &["getTags", "register"],
            _ => &[],
        };
        for member in members {
            prototype.set_property(member, Value::Undefined);
        }
        if name != "BackgroundFetchRecord" {
            w3cos_core::class::set_prototype_of(
                &prototype,
                &crate::web_events::event_target_class().get_property("prototype"),
            );
        }
        class.set_property("prototype", prototype);
        classes.borrow_mut().insert(name.into(), class.clone());
        class
    })
}

pub fn service_worker_class() -> Value {
    illegal_class("ServiceWorker")
}

pub fn service_worker_container_class() -> Value {
    illegal_class("ServiceWorkerContainer")
}

pub fn service_worker_registration_class() -> Value {
    illegal_class("ServiceWorkerRegistration")
}

pub fn companion_manager_class(name: &'static str) -> Value {
    illegal_class(name)
}

pub fn background_fetch_class(name: &'static str) -> Value {
    illegal_class(name)
}

pub fn companion_manager_value(name: &'static str) -> Value {
    let value = Value::object(HashMap::new());
    w3cos_core::class::set_prototype_of(
        &value,
        &companion_manager_class(name).get_property("prototype"),
    );
    let empty_array = || w3cos_core::promise::resolve(vec![Value::array(Vec::new())]);
    match name {
        "BackgroundFetchManager" => {
            value.set_property(
                "get",
                Value::function(|_, _| w3cos_core::promise::resolve(vec![Value::Undefined])),
            );
            value.set_property("getIds", Value::function(move |_, _| empty_array()));
            value.set_property("fetch", Value::function(|_, _| unavailable()));
        }
        "CookieStoreManager" => {
            value.set_property(
                "getSubscriptions",
                Value::function(move |_, _| empty_array()),
            );
            for method in ["subscribe", "unsubscribe"] {
                value.set_property(method, Value::function(|_, _| unavailable()));
            }
        }
        "NavigationPreloadManager" => {
            value.set_property(
                "getState",
                Value::function(|_, _| {
                    w3cos_core::promise::resolve(vec![Value::object(HashMap::from([
                        ("enabled".into(), Value::Bool(false)),
                        ("headerValue".into(), Value::string("true")),
                    ]))])
                }),
            );
            for method in ["disable", "enable", "setHeaderValue"] {
                value.set_property(method, Value::function(|_, _| unavailable()));
            }
        }
        "PeriodicSyncManager" => {
            value.set_property("getTags", Value::function(move |_, _| empty_array()));
            value.set_property("register", Value::function(|_, _| unavailable()));
            value.set_property(
                "unregister",
                Value::function(|_, _| w3cos_core::promise::resolve(vec![Value::Undefined])),
            );
        }
        "SyncManager" => {
            value.set_property("getTags", Value::function(move |_, _| empty_array()));
            value.set_property("register", Value::function(|_, _| unavailable()));
        }
        _ => {}
    }
    value
}

fn unavailable() -> Value {
    static WARNING: Once = Once::new();
    WARNING.call_once(|| {
        eprintln!(
            "[w3cos] warning: Service Worker registration requires isolated worker realms, \
             persistent origin storage and a fetch interception adapter"
        );
    });
    w3cos_core::promise::reject(vec![error(
        "NotSupportedError",
        "Service Worker execution is not available in this runtime",
    )])
}

pub fn service_worker_container_value() -> Value {
    CONTAINER.with(|slot| {
        if let Some(container) = slot.borrow().clone() {
            return container;
        }
        let container =
            w3cos_core::class::construct(&crate::web_events::event_target_class(), vec![]);
        w3cos_core::class::set_prototype_of(
            &container,
            &service_worker_container_class().get_property("prototype"),
        );
        for name in ["oncontrollerchange", "onmessage", "onmessageerror"] {
            container.set_property(name, Value::Null);
        }
        container.set_property("controller", Value::Null);
        container.set_property("ready", unavailable());
        container.set_property(
            "register",
            Value::function(|_, args| {
                let script_url = args
                    .first()
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .to_js_string();
                if script_url.trim().is_empty() {
                    return w3cos_core::promise::reject(vec![error(
                        "TypeError",
                        "Service Worker script URL must be non-empty",
                    )]);
                }
                unavailable()
            }),
        );
        container.set_property(
            "getRegistration",
            Value::function(|_, _| w3cos_core::promise::resolve(vec![Value::Undefined])),
        );
        container.set_property(
            "getRegistrations",
            Value::function(|_, _| w3cos_core::promise::resolve(vec![Value::array(Vec::new())])),
        );
        container.set_property("startMessages", Value::function(|_, _| Value::Undefined));
        *slot.borrow_mut() = Some(container.clone());
        container
    })
}

pub fn reset() {
    CONTAINER.with(|container| *container.borrow_mut() = None);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn container_queries_are_empty_and_registration_is_explicitly_unavailable() {
        reset();
        let container = service_worker_container_value();
        assert!(w3cos_core::class::instance_of(
            &container,
            &service_worker_container_class()
        ));
        assert!(container.get_property("controller").is_null());

        let log = Rc::new(RefCell::new(Vec::<String>::new()));
        let log_for_registrations = Rc::clone(&log);
        container
            .call_method("getRegistrations", vec![])
            .call_method(
                "then",
                vec![Value::function(move |_, args| {
                    log_for_registrations
                        .borrow_mut()
                        .push(args[0].get_property("length").to_js_string());
                    Value::Undefined
                })],
            );
        let log_for_register = Rc::clone(&log);
        container
            .call_method("register", vec![Value::string("/service-worker.js")])
            .call_method(
                "catch",
                vec![Value::function(move |_, args| {
                    log_for_register
                        .borrow_mut()
                        .push(args[0].get_property("name").to_js_string());
                    Value::Undefined
                })],
            );
        crate::jsdom::drain_microtasks();
        assert_eq!(&*log.borrow(), &["0", "NotSupportedError"]);
    }

    #[test]
    fn companion_managers_return_empty_discovery_results() {
        let sync = companion_manager_value("SyncManager");
        let tags = Rc::new(RefCell::new(Value::Undefined));
        let tags_for_callback = Rc::clone(&tags);
        sync.call_method("getTags", Vec::new()).call_method(
            "then",
            vec![Value::function(move |_, args| {
                *tags_for_callback.borrow_mut() = args[0].clone();
                Value::Undefined
            })],
        );
        crate::jsdom::drain_microtasks();
        assert_eq!(tags.borrow().get_property("length").to_u32(), 0);
    }

    #[test]
    fn background_fetch_types_keep_browser_shape_while_manager_rejects() {
        let record = background_fetch_class("BackgroundFetchRecord");
        assert!(
            record
                .get_property("prototype")
                .get_property("responseReady")
                .is_undefined()
        );
        let registration = background_fetch_class("BackgroundFetchRegistration");
        for member in ["abort", "downloadTotal", "matchAll", "onprogress", "result"] {
            assert!(
                registration
                    .get_property("prototype")
                    .get_property(member)
                    .is_undefined()
            );
        }
        assert_eq!(
            w3cos_core::class::get_prototype_of(&registration.get_property("prototype")),
            crate::web_events::event_target_class().get_property("prototype")
        );

        let rejection = Rc::new(RefCell::new(String::new()));
        let rejection_for_callback = Rc::clone(&rejection);
        companion_manager_value("BackgroundFetchManager")
            .call_method(
                "fetch",
                vec![Value::string("job"), Value::array(vec![Value::string("/")])],
            )
            .call_method(
                "catch",
                vec![Value::function(move |_, args| {
                    *rejection_for_callback.borrow_mut() =
                        args[0].get_property("name").to_js_string();
                    Value::Undefined
                })],
            );
        crate::jsdom::drain_microtasks();
        assert_eq!(&*rejection.borrow(), "NotSupportedError");
    }
}
