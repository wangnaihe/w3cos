//! View Transitions API lifecycle without renderer snapshot compositing.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Once;

use w3cos_core::Value;

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static ACTIVE: RefCell<Option<Value>> = const { RefCell::new(None) };
}

static VISUAL_WARNING: Once = Once::new();

fn illegal_class(name: &'static str) -> Value {
    CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        let class = Value::function(move |_, _| {
            w3cos_core::throw_value(w3cos_core::class::construct(
                &crate::unsupported::dom_exception_class(),
                vec![
                    Value::string(&format!("Illegal constructor: {name}")),
                    Value::string("TypeError"),
                ],
            ))
        });
        class.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::from([("constructor".into(), class.clone())]));
        class.set_property("prototype", prototype);
        classes.borrow_mut().insert(name.into(), class.clone());
        class
    })
}

pub fn view_transition_class() -> Value {
    let class = illegal_class("ViewTransition");
    for member in [
        "finished",
        "ready",
        "skipTransition",
        "transitionRoot",
        "types",
        "updateCallbackDone",
        "waitUntil",
    ] {
        class
            .get_property("prototype")
            .set_property(member, Value::Undefined);
    }
    class
}
pub fn type_set_class() -> Value {
    let class = illegal_class("ViewTransitionTypeSet");
    for member in [
        "add", "clear", "delete", "entries", "forEach", "has", "keys", "size", "values",
    ] {
        class
            .get_property("prototype")
            .set_property(member, Value::Undefined);
    }
    class
}

fn event_class(name: &'static str, fields: &'static [&'static str]) -> Value {
    CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        let class = Value::function(move |this, args| {
            crate::web_events::event_class().call(this.clone(), args.clone());
            let init = args.get(1).cloned().unwrap_or(Value::Undefined);
            for field in fields {
                let value = init.get_property(field);
                this.set_property(
                    field,
                    if value.is_undefined() {
                        Value::Null
                    } else {
                        value
                    },
                );
            }
            Value::Undefined
        });
        class.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::from([("constructor".into(), class.clone())]));
        for field in fields {
            prototype.set_property(field, Value::Undefined);
        }
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::web_events::event_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        classes.borrow_mut().insert(name.into(), class.clone());
        class
    })
}

pub fn page_reveal_event_class() -> Value {
    event_class("PageRevealEvent", &["viewTransition"])
}
pub fn page_swap_event_class() -> Value {
    event_class("PageSwapEvent", &["activation", "viewTransition"])
}

fn deferred() -> (Value, Value, Value) {
    let resolve = Rc::new(RefCell::new(Value::Undefined));
    let reject = Rc::new(RefCell::new(Value::Undefined));
    let resolve_for_executor = Rc::clone(&resolve);
    let reject_for_executor = Rc::clone(&reject);
    let promise = w3cos_core::promise::new(vec![Value::function(move |_, args| {
        *resolve_for_executor.borrow_mut() = args.first().cloned().unwrap_or(Value::Undefined);
        *reject_for_executor.borrow_mut() = args.get(1).cloned().unwrap_or(Value::Undefined);
        Value::Undefined
    })]);
    let resolve_value = resolve.borrow().clone();
    let reject_value = reject.borrow().clone();
    (promise, resolve_value, reject_value)
}

fn panic_value(payload: Box<dyn std::any::Any + Send>) -> Value {
    if let Some(value) = payload.downcast_ref::<w3cos_core::PanicValue>() {
        return value.0.clone();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return Value::object(HashMap::from([
            ("name".into(), Value::string("Error")),
            ("message".into(), Value::string(message)),
        ]));
    }
    Value::object(HashMap::from([
        ("name".into(), Value::string("Error")),
        (
            "message".into(),
            Value::string("View transition update callback failed"),
        ),
    ]))
}

fn iterator(items: Vec<Value>) -> Value {
    Value::array(items).call_method("__w3cos_symbol_iterator", vec![])
}

fn type_set(initial: Vec<String>) -> Value {
    let values = Rc::new(RefCell::new(initial));
    let set = Value::object(HashMap::new());
    let values_for_size = Rc::clone(&values);
    set.set_property(
        "__w3cos_getter_size",
        Value::function(move |_, _| Value::Number(values_for_size.borrow().len() as f64)),
    );
    let values_for_has = Rc::clone(&values);
    set.set_property(
        "has",
        Value::function(move |_, args| {
            let value = args
                .first()
                .cloned()
                .unwrap_or(Value::Undefined)
                .to_js_string();
            Value::Bool(values_for_has.borrow().contains(&value))
        }),
    );
    let values_for_add = Rc::clone(&values);
    let set_for_add = set.clone();
    set.set_property(
        "add",
        Value::function(move |_, args| {
            let value = args
                .first()
                .cloned()
                .unwrap_or(Value::Undefined)
                .to_js_string();
            let mut values = values_for_add.borrow_mut();
            if !values.contains(&value) {
                values.push(value);
            }
            set_for_add.clone()
        }),
    );
    let values_for_delete = Rc::clone(&values);
    set.set_property(
        "delete",
        Value::function(move |_, args| {
            let value = args
                .first()
                .cloned()
                .unwrap_or(Value::Undefined)
                .to_js_string();
            let mut values = values_for_delete.borrow_mut();
            let before = values.len();
            values.retain(|item| item != &value);
            Value::Bool(before != values.len())
        }),
    );
    let values_for_clear = Rc::clone(&values);
    set.set_property(
        "clear",
        Value::function(move |_, _| {
            values_for_clear.borrow_mut().clear();
            Value::Undefined
        }),
    );
    for name in ["keys", "values"] {
        let values = Rc::clone(&values);
        set.set_property(
            name,
            Value::function(move |_, _| {
                iterator(
                    values
                        .borrow()
                        .iter()
                        .map(|value| Value::string(value))
                        .collect(),
                )
            }),
        );
    }
    let values_for_entries = Rc::clone(&values);
    set.set_property(
        "entries",
        Value::function(move |_, _| {
            iterator(
                values_for_entries
                    .borrow()
                    .iter()
                    .map(|value| Value::array(vec![Value::string(value), Value::string(value)]))
                    .collect(),
            )
        }),
    );
    let values_for_each = values;
    let set_for_each = set.clone();
    set.set_property(
        "forEach",
        Value::function(move |_, args| {
            let callback = args.first().cloned().unwrap_or(Value::Undefined);
            let this_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
            for value in values_for_each.borrow().iter() {
                callback.call(
                    this_arg.clone(),
                    vec![
                        Value::string(value),
                        Value::string(value),
                        set_for_each.clone(),
                    ],
                );
            }
            Value::Undefined
        }),
    );
    set.set_property("__w3cos_symbol_iterator", set.get_property("values"));
    let prototype = type_set_class().get_property("prototype");
    for name in [
        "__w3cos_getter_size",
        "add",
        "clear",
        "delete",
        "entries",
        "forEach",
        "has",
        "keys",
        "values",
        "__w3cos_symbol_iterator",
    ] {
        prototype.set_property(name, set.get_property(name));
    }
    w3cos_core::class::set_prototype_of(&set, &prototype);
    set
}

fn start(args: Vec<Value>) -> Value {
    VISUAL_WARNING.call_once(|| {
        eprintln!(
            "[w3cos] warning: View Transition callback and Promise lifecycle are active, but \
             renderer old/new snapshot capture and pseudo-element compositing are pending"
        );
    });
    let options = args.first().cloned().unwrap_or(Value::Undefined);
    let update = if options.is_function() {
        options.clone()
    } else {
        options.get_property("update")
    };
    let types = if options.is_object() {
        options
            .get_property("types")
            .iter()
            .map(|value| value.to_js_string())
            .collect()
    } else {
        Vec::new()
    };
    let (ready, ready_resolve, ready_reject) = deferred();
    let (updated, updated_resolve, updated_reject) = deferred();
    let (finished, finished_resolve, finished_reject) = deferred();
    let transition = Value::object(HashMap::from([
        ("ready".into(), ready),
        ("updateCallbackDone".into(), updated.clone()),
        ("finished".into(), finished),
        ("types".into(), type_set(types)),
        ("transitionRoot".into(), crate::jsdom::document_value()),
        ("__w3cos_waits".into(), Value::array(Vec::new())),
    ]));
    let transition_for_skip = transition.clone();
    let finished_resolve_for_skip = finished_resolve.clone();
    transition.set_property(
        "skipTransition",
        Value::function(move |_, _| {
            transition_for_skip.set_property("__w3cos_skipped", Value::Bool(true));
            finished_resolve_for_skip.call(Value::Undefined, vec![Value::Undefined]);
            ACTIVE.with(|active| *active.borrow_mut() = None);
            Value::Undefined
        }),
    );
    let transition_for_wait = transition.clone();
    transition.set_property(
        "waitUntil",
        Value::function(move |_, args| {
            transition_for_wait
                .get_property("__w3cos_waits")
                .call_method(
                    "push",
                    vec![args.first().cloned().unwrap_or(Value::Undefined)],
                );
            Value::Undefined
        }),
    );
    let prototype = view_transition_class().get_property("prototype");
    for name in [
        "ready",
        "updateCallbackDone",
        "finished",
        "types",
        "transitionRoot",
        "skipTransition",
        "waitUntil",
    ] {
        prototype.set_property(name, transition.get_property(name));
    }
    w3cos_core::class::set_prototype_of(&transition, &prototype);
    ACTIVE.with(|active| *active.borrow_mut() = Some(transition.clone()));

    let transition_for_job = transition.clone();
    crate::jsdom::queue_microtask_value(Value::function(move |_, _| {
        ready_resolve.call(Value::Undefined, vec![Value::Undefined]);
        let result = if update.is_function() {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                update.call(Value::Undefined, vec![])
            }))
            .map_err(panic_value)
        } else {
            Ok(Value::Undefined)
        };
        match result {
            Ok(value) => updated_resolve.call(Value::Undefined, vec![value]),
            Err(error) => {
                ready_reject.call(Value::Undefined, vec![error.clone()]);
                updated_reject.call(Value::Undefined, vec![error.clone()]);
                finished_reject.call(Value::Undefined, vec![error]);
                ACTIVE.with(|active| *active.borrow_mut() = None);
                return Value::Undefined;
            }
        };
        let mut waits = vec![updated.clone()];
        waits.extend(transition_for_job.get_property("__w3cos_waits").iter());
        let all = w3cos_core::promise::all(vec![Value::array(waits)]);
        let finish = finished_resolve.clone();
        let fail = finished_reject.clone();
        all.call_method(
            "then",
            vec![
                Value::function(move |_, _| {
                    finish.call(Value::Undefined, vec![Value::Undefined]);
                    ACTIVE.with(|active| *active.borrow_mut() = None);
                    Value::Undefined
                }),
                Value::function(move |_, args| {
                    fail.call(
                        Value::Undefined,
                        vec![args.first().cloned().unwrap_or(Value::Undefined)],
                    );
                    ACTIVE.with(|active| *active.borrow_mut() = None);
                    Value::Undefined
                }),
            ],
        );
        Value::Undefined
    }));
    transition
}

pub fn install_document(document: &Value) {
    let start_method = Value::function(|_, args| start(args));
    document.set_property("startViewTransition", start_method.clone());
    document.set_property(
        "__w3cos_getter_activeViewTransition",
        Value::function(|_, _| {
            ACTIVE.with(|active| active.borrow().clone().unwrap_or(Value::Null))
        }),
    );
    let prototype = crate::dom_constructors::prototype("Document");
    prototype.set_property("startViewTransition", start_method);
    prototype.set_property(
        "__w3cos_getter_activeViewTransition",
        document.get_property("__w3cos_getter_activeViewTransition"),
    );
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn callback_and_waited_promises_settle_asynchronously() {
        let document = crate::jsdom::document_value();
        install_document(&document);
        let called = Rc::new(Cell::new(false));
        let called_for_update = Rc::clone(&called);
        let transition = document.call_method(
            "startViewTransition",
            vec![Value::function(move |_, _| {
                called_for_update.set(true);
                Value::Undefined
            })],
        );
        assert!(!called.get());
        assert!(w3cos_core::class::instance_of(
            &transition,
            &view_transition_class()
        ));
        crate::jsdom::drain_microtasks();
        assert!(called.get());
        assert!(document.get_property("activeViewTransition").is_null());
    }
}
