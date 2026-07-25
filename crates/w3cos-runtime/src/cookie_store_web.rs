//! Session-backed Cookie Store API compatibility layer.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use w3cos_core::Value;

thread_local! {
    static COOKIES: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
    static COOKIE_STORE: RefCell<Option<Value>> = const { RefCell::new(None) };
    static COOKIE_STORE_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static COOKIE_CHANGE_EVENT_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static WARNING_EMITTED: Cell<bool> = const { Cell::new(false) };
}

fn cookie_value(name: &str, value: &str) -> Value {
    Value::object(HashMap::from([
        ("name".into(), Value::string(name)),
        ("value".into(), Value::string(value)),
        ("domain".into(), Value::Null),
        ("path".into(), Value::string("/")),
        ("expires".into(), Value::Null),
        ("secure".into(), Value::Bool(false)),
        ("sameSite".into(), Value::string("strict")),
        ("partitioned".into(), Value::Bool(false)),
    ]))
}

fn change_event(changed: Vec<Value>, deleted: Vec<Value>) -> Value {
    let event = w3cos_core::class::construct(
        &crate::web_events::event_class(),
        vec![Value::string("change")],
    );
    event.set_property("changed", Value::array(changed));
    event.set_property("deleted", Value::array(deleted));
    w3cos_core::class::set_prototype_of(
        &event,
        &cookie_change_event_class().get_property("prototype"),
    );
    event
}

fn dispatch_change(changed: Vec<Value>, deleted: Vec<Value>) {
    let store = cookie_store_value();
    crate::jsdom::queue_microtask_value(Value::function(move |_, _| {
        store.call_method(
            "dispatchEvent",
            vec![change_event(changed.clone(), deleted.clone())],
        );
        Value::Undefined
    }));
}

pub fn document_cookie() -> String {
    COOKIES.with(|cookies| {
        cookies
            .borrow()
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ")
    })
}

pub fn set_document_cookie(assignment: &str) {
    let pair = assignment.split(';').next().unwrap_or_default();
    let Some((name, value)) = pair.split_once('=') else {
        return;
    };
    let name = name.trim();
    if name.is_empty() {
        return;
    }
    set_cookie(name, value.trim());
}

fn set_cookie(name: &str, value: &str) {
    let previous = COOKIES.with(|cookies| {
        let mut cookies = cookies.borrow_mut();
        let previous = cookies
            .iter()
            .find(|(candidate, _)| candidate == name)
            .cloned();
        cookies.retain(|(candidate, _)| candidate != name);
        cookies.push((name.to_string(), value.to_string()));
        previous
    });
    if previous.as_ref().is_none_or(|(_, old)| old != value) {
        dispatch_change(vec![cookie_value(name, value)], vec![]);
    }
}

fn delete_cookie(name: &str) {
    let deleted = COOKIES.with(|cookies| {
        let mut cookies = cookies.borrow_mut();
        let deleted = cookies
            .iter()
            .find(|(candidate, _)| candidate == name)
            .cloned();
        cookies.retain(|(candidate, _)| candidate != name);
        deleted
    });
    if let Some((name, value)) = deleted {
        dispatch_change(vec![], vec![cookie_value(&name, &value)]);
    }
}

fn option_name(value: &Value) -> Option<String> {
    if matches!(value, Value::String(_)) {
        Some(value.to_js_string())
    } else {
        let name = value.get_property("name");
        (!name.is_undefined()).then(|| name.to_js_string())
    }
}

pub fn cookie_store_value() -> Value {
    COOKIE_STORE.with(|slot| {
        if let Some(value) = slot.borrow().clone() {
            return value;
        }
        WARNING_EMITTED.with(|warned| {
            if !warned.replace(true) {
                eprintln!(
                    "[w3cos] warning: cookieStore uses the per-session origin cookie backend; \
                     persistence, partitioning, expiry and service-worker delivery remain pending"
                );
            }
        });
        let store = w3cos_core::class::construct(&crate::web_events::event_target_class(), vec![]);
        store.set_property("onchange", Value::Null);
        store.set_property(
            "get",
            Value::function(|_, args| {
                let name = option_name(&args.first().cloned().unwrap_or(Value::Undefined));
                let found = name.and_then(|name| {
                    COOKIES.with(|cookies| {
                        cookies
                            .borrow()
                            .iter()
                            .find(|(candidate, _)| candidate == &name)
                            .map(|(name, value)| cookie_value(name, value))
                    })
                });
                w3cos_core::promise::resolve(vec![found.unwrap_or(Value::Null)])
            }),
        );
        store.set_property(
            "getAll",
            Value::function(|_, args| {
                let selector = args.first().cloned().unwrap_or(Value::Undefined);
                let name = (!selector.is_undefined())
                    .then(|| option_name(&selector))
                    .flatten();
                let cookies = COOKIES.with(|cookies| {
                    cookies
                        .borrow()
                        .iter()
                        .filter(|(candidate, _)| name.as_ref().is_none_or(|name| candidate == name))
                        .map(|(name, value)| cookie_value(name, value))
                        .collect()
                });
                w3cos_core::promise::resolve(vec![Value::array(cookies)])
            }),
        );
        store.set_property(
            "set",
            Value::function(|_, args| {
                let first = args.first().cloned().unwrap_or(Value::Undefined);
                let (name, value) = if matches!(first, Value::String(_)) {
                    (
                        first.to_js_string(),
                        args.get(1)
                            .cloned()
                            .unwrap_or(Value::Undefined)
                            .to_js_string(),
                    )
                } else {
                    (
                        first.get_property("name").to_js_string(),
                        first.get_property("value").to_js_string(),
                    )
                };
                if !name.is_empty() && name != "undefined" {
                    set_cookie(&name, &value);
                }
                w3cos_core::promise::resolve(vec![Value::Undefined])
            }),
        );
        store.set_property(
            "delete",
            Value::function(|_, args| {
                if let Some(name) = option_name(&args.first().cloned().unwrap_or(Value::Undefined))
                {
                    delete_cookie(&name);
                }
                w3cos_core::promise::resolve(vec![Value::Undefined])
            }),
        );
        w3cos_core::class::set_prototype_of(
            &store,
            &cookie_store_class().get_property("prototype"),
        );
        *slot.borrow_mut() = Some(store.clone());
        store
    })
}

pub fn cookie_store_class() -> Value {
    COOKIE_STORE_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| Value::Undefined);
        class.set_property("name", Value::string("CookieStore"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for method in ["delete", "get", "getAll", "set"] {
            prototype.set_property(method, Value::function(|_, _| Value::Undefined));
        }
        prototype.set_property("onchange", Value::Undefined);
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn cookie_change_event_class() -> Value {
    COOKIE_CHANGE_EVENT_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|this, args| {
            let init = args.get(1).cloned().unwrap_or(Value::Undefined);
            this.set_property("type", Value::string("change"));
            this.set_property("changed", init.get_property("changed"));
            this.set_property("deleted", init.get_property("deleted"));
            Value::Undefined
        });
        class.set_property("name", Value::string("CookieChangeEvent"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property("changed", Value::Undefined);
        prototype.set_property("deleted", Value::Undefined);
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::web_events::event_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn reset() {
    COOKIES.with(|cookies| cookies.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    #[test]
    fn cookie_store_and_document_cookie_share_state_and_emit_changes() {
        crate::jsdom::reset_bridge();
        let store = cookie_store_value();
        assert!(w3cos_core::class::instance_of(
            &store,
            &cookie_store_class()
        ));
        let changes = Rc::new(RefCell::new(Vec::<Value>::new()));
        let changes_for_listener = Rc::clone(&changes);
        store.call_method(
            "addEventListener",
            vec![
                Value::string("change"),
                Value::function(move |_, args| {
                    changes_for_listener.borrow_mut().push(args[0].clone());
                    Value::Undefined
                }),
            ],
        );

        set_document_cookie("theme=dark; Path=/");
        crate::jsdom::drain_microtasks();
        assert_eq!(document_cookie(), "theme=dark");
        assert_eq!(changes.borrow().len(), 1);
        assert!(w3cos_core::class::instance_of(
            &changes.borrow()[0],
            &cookie_change_event_class()
        ));
        assert_eq!(
            changes.borrow()[0]
                .get_property("changed")
                .get_property("length"),
            Value::Number(1.0)
        );

        let found = Rc::new(RefCell::new(Value::Undefined));
        let found_for_then = Rc::clone(&found);
        store
            .call_method("get", vec![Value::string("theme")])
            .call_method(
                "then",
                vec![Value::function(move |_, args| {
                    *found_for_then.borrow_mut() = args[0].clone();
                    Value::Undefined
                })],
            );
        w3cos_core::promise::drain_microtasks();
        assert_eq!(found.borrow().get_property("value"), Value::string("dark"));

        store.call_method("delete", vec![Value::string("theme")]);
        crate::jsdom::drain_microtasks();
        assert_eq!(document_cookie(), "");
        assert_eq!(
            changes.borrow()[1]
                .get_property("deleted")
                .get_property("length"),
            Value::Number(1.0)
        );
    }
}
