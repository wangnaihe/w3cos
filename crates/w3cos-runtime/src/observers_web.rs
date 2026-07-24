//! Browser-facing observer constructors.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use w3cos_core::Value;

thread_local! {
    static RESIZE_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static MUTATION_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static INTERSECTION_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static PERFORMANCE_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
}

fn finish_class(class: Value) -> Value {
    let prototype = Value::object(HashMap::new());
    prototype.set_property("constructor", class.clone());
    class.set_property("prototype", prototype);
    class
}

pub fn resize_observer_class() -> Value {
    RESIZE_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = finish_class(Value::function(|_, args| {
            let observer = w3cos_core::ResizeObserver::new(args);
            w3cos_core::class::set_prototype_of(
                &observer,
                &resize_observer_class().get_property("prototype"),
            );
            observer
        }));
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn mutation_observer_class() -> Value {
    MUTATION_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = finish_class(Value::function(|_, args| {
            let callback = args.first().cloned().unwrap_or_default();
            let records = Rc::new(RefCell::new(Vec::<Value>::new()));
            let value = Value::object(HashMap::new());
            value.set_property("observe", Value::function(|_, _| Value::Undefined));
            let disconnect_records = Rc::clone(&records);
            value.set_property(
                "disconnect",
                Value::function(move |_, _| {
                    disconnect_records.borrow_mut().clear();
                    Value::Undefined
                }),
            );
            let take_records = Rc::clone(&records);
            value.set_property(
                "takeRecords",
                Value::function(move |_, _| {
                    Value::array(std::mem::take(&mut *take_records.borrow_mut()))
                }),
            );
            let enqueue_records = records;
            value.set_property(
                "__w3cosEnqueue",
                Value::function(move |this, args| {
                    let record = args.first().cloned().unwrap_or_default();
                    enqueue_records.borrow_mut().push(record);
                    let pending = std::mem::take(&mut *enqueue_records.borrow_mut());
                    callback.call(Value::Undefined, vec![Value::array(pending), this.clone()]);
                    Value::Undefined
                }),
            );
            w3cos_core::class::set_prototype_of(
                &value,
                &mutation_observer_class().get_property("prototype"),
            );
            value
        }));
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn intersection_observer_class() -> Value {
    INTERSECTION_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = finish_class(Value::function(|_, args| {
            let callback = args.first().cloned().unwrap_or_default();
            let options = args.get(1).cloned().unwrap_or_default();
            let root = if options.get_property("root").is_undefined() {
                Value::Null
            } else {
                options.get_property("root")
            };
            let threshold = options.get_property("threshold");
            let thresholds = if threshold.is_undefined() {
                Value::array(vec![Value::Number(0.0)])
            } else if matches!(threshold, Value::Array(_)) {
                threshold
            } else {
                Value::array(vec![threshold])
            };
            let value = Value::object(HashMap::from([
                ("root".to_string(), root),
                (
                    "rootMargin".to_string(),
                    if options.get_property("rootMargin").is_undefined() {
                        Value::string("0px 0px 0px 0px")
                    } else {
                        options.get_property("rootMargin")
                    },
                ),
                ("thresholds".to_string(), thresholds),
            ]));
            let callback_for_observe = callback;
            value.set_property(
                "observe",
                Value::function(move |this, args| {
                    let target = args.first().cloned().unwrap_or_default();
                    let rect = target.call_method("getBoundingClientRect", vec![]);
                    let entry = Value::object(HashMap::from([
                        ("target".to_string(), target),
                        ("time".to_string(), Value::Number(0.0)),
                        ("rootBounds".to_string(), Value::Null),
                        ("boundingClientRect".to_string(), rect.clone()),
                        ("intersectionRect".to_string(), rect),
                        ("intersectionRatio".to_string(), Value::Number(1.0)),
                        ("isIntersecting".to_string(), Value::Bool(true)),
                    ]));
                    callback_for_observe
                        .call(Value::Undefined, vec![Value::array(vec![entry]), this]);
                    Value::Undefined
                }),
            );
            value.set_property("unobserve", Value::function(|_, _| Value::Undefined));
            value.set_property("disconnect", Value::function(|_, _| Value::Undefined));
            value.set_property("takeRecords", Value::function(|_, _| Value::array(vec![])));
            w3cos_core::class::set_prototype_of(
                &value,
                &intersection_observer_class().get_property("prototype"),
            );
            value
        }));
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn performance_observer_class() -> Value {
    PERFORMANCE_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = finish_class(Value::function(|_, args| {
            let callback = args.first().cloned().unwrap_or_default();
            let value = Value::object(HashMap::new());
            value.set_property(
                "observe",
                Value::function(move |this, _| {
                    let entries = Value::array(vec![]);
                    let list = Value::object(HashMap::from([
                        ("getEntries".to_string(), {
                            let entries = entries.clone();
                            Value::function(move |_, _| entries.clone())
                        }),
                        (
                            "getEntriesByName".to_string(),
                            Value::function(|_, _| Value::array(vec![])),
                        ),
                        (
                            "getEntriesByType".to_string(),
                            Value::function(|_, _| Value::array(vec![])),
                        ),
                    ]));
                    callback.call(Value::Undefined, vec![list, this]);
                    Value::Undefined
                }),
            );
            value.set_property("disconnect", Value::function(|_, _| Value::Undefined));
            value.set_property("takeRecords", Value::function(|_, _| Value::array(vec![])));
            w3cos_core::class::set_prototype_of(
                &value,
                &performance_observer_class().get_property("prototype"),
            );
            value
        }));
        class.set_property(
            "supportedEntryTypes",
            Value::array(
                ["mark", "measure", "navigation", "resource"]
                    .into_iter()
                    .map(Value::string)
                    .collect(),
            ),
        );
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}
