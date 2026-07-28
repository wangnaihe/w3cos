//! Chromium Observable API compatibility.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Once;

use crate::jsdom::realm_function;
use w3cos_core::Value;

struct SubscriberState {
    value: Value,
    active: Rc<Cell<bool>>,
    teardowns: Rc<RefCell<Vec<Value>>>,
}

thread_local! {
    static OBSERVABLE_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static SUBSCRIBER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static SUBSCRIBERS: RefCell<Vec<SubscriberState>> = const { RefCell::new(Vec::new()) };
}

fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or(Value::Undefined)
}

fn observer_callback(observer: &Value, name: &str) -> Value {
    if name == "next" && observer.is_function() {
        observer.clone()
    } else {
        observer.get_property(name)
    }
}

fn run_teardowns(active: &Rc<Cell<bool>>, teardowns: &Rc<RefCell<Vec<Value>>>) {
    if !active.replace(false) {
        return;
    }
    for teardown in teardowns.borrow_mut().drain(..).rev() {
        teardown.call(Value::Undefined, vec![]);
    }
}

fn unregister_subscriber(value: &Value) {
    SUBSCRIBERS.with(|subscribers| {
        subscribers
            .borrow_mut()
            .retain(|state| !state.value.strict_eq(value));
    });
}

fn subscriber_value(observer: Value) -> Value {
    let generation = crate::jsdom::realm_generation();
    let value = Value::object(HashMap::new());
    let active = Rc::new(Cell::new(true));
    let teardowns = Rc::new(RefCell::new(Vec::new()));
    let controller = w3cos_core::class::construct(&crate::fetch::abort_controller_class(), vec![]);
    value.set_property("active", Value::Bool(true));
    value.set_property("signal", controller.get_property("signal"));

    let next_active = Rc::clone(&active);
    let next_observer = observer.clone();
    value.set_property(
        "next",
        realm_function(generation, move |_, args| {
            if next_active.get() {
                let callback = observer_callback(&next_observer, "next");
                if callback.is_function() {
                    callback.call(next_observer.clone(), vec![arg(&args, 0)]);
                }
            }
            Value::Undefined
        }),
    );

    let complete_active = Rc::clone(&active);
    let complete_teardowns = Rc::clone(&teardowns);
    let complete_observer = observer.clone();
    value.set_property(
        "complete",
        realm_function(generation, move |this, _| {
            if complete_active.get() {
                let callback = observer_callback(&complete_observer, "complete");
                if callback.is_function() {
                    callback.call(complete_observer.clone(), vec![]);
                }
                run_teardowns(&complete_active, &complete_teardowns);
                this.set_property("active", Value::Bool(false));
                unregister_subscriber(&this);
            }
            Value::Undefined
        }),
    );

    let error_active = Rc::clone(&active);
    let error_teardowns = Rc::clone(&teardowns);
    let error_observer = observer;
    value.set_property(
        "error",
        realm_function(generation, move |this, args| {
            if error_active.get() {
                let callback = observer_callback(&error_observer, "error");
                if callback.is_function() {
                    callback.call(error_observer.clone(), vec![arg(&args, 0)]);
                } else {
                    eprintln!(
                        "[W3C OS][compat warning] unhandled Observable error: {}",
                        arg(&args, 0).to_js_string()
                    );
                }
                run_teardowns(&error_active, &error_teardowns);
                this.set_property("active", Value::Bool(false));
                unregister_subscriber(&this);
            }
            Value::Undefined
        }),
    );

    let teardown_active = Rc::clone(&active);
    let teardown_values = Rc::clone(&teardowns);
    value.set_property(
        "addTeardown",
        realm_function(generation, move |_, args| {
            let teardown = arg(&args, 0);
            if !teardown.is_function() {
                return Value::Undefined;
            }
            if teardown_active.get() {
                teardown_values.borrow_mut().push(teardown);
            } else {
                teardown.call(Value::Undefined, vec![]);
            }
            Value::Undefined
        }),
    );
    w3cos_core::class::set_prototype_of(&value, &subscriber_class().get_property("prototype"));
    SUBSCRIBERS.with(|subscribers| {
        subscribers.borrow_mut().push(SubscriberState {
            value: value.clone(),
            active,
            teardowns,
        });
    });
    value
}

pub fn observable_from_producer(producer: Value) -> Value {
    let value = Value::object(HashMap::from([("__w3cosProducer".to_string(), producer)]));
    w3cos_core::class::set_prototype_of(&value, &observable_class().get_property("prototype"));
    value
}

fn subscribe(this: Value, args: Vec<Value>) -> Value {
    let observer = arg(&args, 0);
    let subscriber = subscriber_value(observer);
    let producer = this.get_property("__w3cosProducer");
    if producer.is_function() {
        producer.call(Value::Undefined, vec![subscriber]);
    }
    Value::Undefined
}

fn transform(source: Value, callback: Value, kind: &'static str, amount: u32) -> Value {
    let generation = crate::jsdom::realm_generation();
    observable_from_producer(realm_function(generation, move |_, args| {
        let downstream = arg(&args, 0);
        let index = Rc::new(Cell::new(0_u32));
        let next_index = Rc::clone(&index);
        let next_downstream = downstream.clone();
        let next_callback = callback.clone();
        let observer = Value::object(HashMap::new());
        observer.set_property(
            "next",
            realm_function(generation, move |_, args| {
                let value = arg(&args, 0);
                let current = next_index.get();
                next_index.set(current.saturating_add(1));
                match kind {
                    "map" => {
                        let mapped = next_callback
                            .call(Value::Undefined, vec![value, Value::Number(current as f64)]);
                        next_downstream.call_method("next", vec![mapped]);
                    }
                    "filter" => {
                        if next_callback
                            .call(
                                Value::Undefined,
                                vec![value.clone(), Value::Number(current as f64)],
                            )
                            .to_bool()
                        {
                            next_downstream.call_method("next", vec![value]);
                        }
                    }
                    "take" if current < amount => {
                        next_downstream.call_method("next", vec![value]);
                        if current + 1 == amount {
                            next_downstream.call_method("complete", vec![]);
                        }
                    }
                    "drop" if current >= amount => {
                        next_downstream.call_method("next", vec![value]);
                    }
                    _ => {}
                }
                Value::Undefined
            }),
        );
        let error_downstream = downstream.clone();
        observer.set_property(
            "error",
            realm_function(generation, move |_, args| {
                error_downstream.call_method("error", vec![arg(&args, 0)]);
                Value::Undefined
            }),
        );
        observer.set_property(
            "complete",
            realm_function(generation, move |_, _| {
                downstream.call_method("complete", vec![]);
                Value::Undefined
            }),
        );
        source.call_method("subscribe", vec![observer]);
        Value::Undefined
    }))
}

fn collect_promise(source: Value) -> Value {
    let generation = crate::jsdom::realm_generation();
    w3cos_core::promise::new(vec![realm_function(generation, move |_, args| {
        let resolve = arg(&args, 0);
        let reject = arg(&args, 1);
        let values = Rc::new(RefCell::new(Vec::new()));
        let next_values = Rc::clone(&values);
        let observer = Value::object(HashMap::new());
        observer.set_property(
            "next",
            realm_function(generation, move |_, args| {
                next_values.borrow_mut().push(arg(&args, 0));
                Value::Undefined
            }),
        );
        observer.set_property(
            "error",
            realm_function(generation, move |_, args| {
                reject.call(Value::Undefined, vec![arg(&args, 0)]);
                Value::Undefined
            }),
        );
        observer.set_property(
            "complete",
            realm_function(generation, move |_, _| {
                resolve.call(
                    Value::Undefined,
                    vec![Value::array(values.borrow().clone())],
                );
                Value::Undefined
            }),
        );
        source.call_method("subscribe", vec![observer]);
        Value::Undefined
    })])
}

fn promise_operator(source: Value, args: Vec<Value>, kind: &'static str) -> Value {
    let generation = crate::jsdom::realm_generation();
    let callback = arg(&args, 0);
    let initial = arg(&args, 1);
    collect_promise(source).call_method(
        "then",
        vec![realm_function(generation, move |_, args| {
            let values = arg(&args, 0).iter().collect::<Vec<_>>();
            match kind {
                "first" => values.first().cloned().unwrap_or(Value::Undefined),
                "last" => values.last().cloned().unwrap_or(Value::Undefined),
                "toArray" => Value::array(values),
                "forEach" => {
                    for (index, value) in values.into_iter().enumerate() {
                        callback.call(Value::Undefined, vec![value, Value::Number(index as f64)]);
                    }
                    Value::Undefined
                }
                "every" => Value::Bool(values.into_iter().enumerate().all(|(index, value)| {
                    callback
                        .call(Value::Undefined, vec![value, Value::Number(index as f64)])
                        .to_bool()
                })),
                "some" => Value::Bool(values.into_iter().enumerate().any(|(index, value)| {
                    callback
                        .call(Value::Undefined, vec![value, Value::Number(index as f64)])
                        .to_bool()
                })),
                "find" => values
                    .into_iter()
                    .enumerate()
                    .find_map(|(index, value)| {
                        callback
                            .call(
                                Value::Undefined,
                                vec![value.clone(), Value::Number(index as f64)],
                            )
                            .to_bool()
                            .then_some(value)
                    })
                    .unwrap_or(Value::Undefined),
                "reduce" => {
                    let mut values = values.into_iter();
                    let mut accumulator = if initial.is_undefined() {
                        values.next().unwrap_or(Value::Undefined)
                    } else {
                        initial.clone()
                    };
                    for (index, value) in values.enumerate() {
                        accumulator = callback.call(
                            Value::Undefined,
                            vec![accumulator, value, Value::Number(index as f64)],
                        );
                    }
                    accumulator
                }
                _ => Value::Undefined,
            }
        })],
    )
}

fn unsupported_operator(this: Value, name: &'static str) -> Value {
    static WARNING: Once = Once::new();
    WARNING.call_once(|| {
        eprintln!(
            "[W3C OS][compat warning] advanced Observable composition operators preserve \
             compatible return shapes but flatMap/switchMap/catch/takeUntil cancellation \
             semantics are pending"
        );
    });
    let _ = name;
    this
}

pub fn observable_class() -> Value {
    OBSERVABLE_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let generation = crate::jsdom::realm_generation();
        let class = realm_function(generation, |_, args| {
            let producer = arg(&args, 0);
            if !producer.is_function() {
                w3cos_core::throw_value(w3cos_core::error_instance(
                    "TypeError",
                    vec![Value::string("Observable callback must be a function")],
                ));
            }
            observable_from_producer(producer)
        });
        class.set_property("name", Value::string("Observable"));
        class.set_property(
            "from",
            realm_function(generation, move |_, args| {
                let input = arg(&args, 0);
                if w3cos_core::class::instance_of(&input, &observable_class()) {
                    return input;
                }
                observable_from_producer(realm_function(generation, move |_, args| {
                    let subscriber = arg(&args, 0);
                    for value in input.iter() {
                        subscriber.call_method("next", vec![value]);
                    }
                    subscriber.call_method("complete", vec![]);
                    Value::Undefined
                }))
            }),
        );
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property(
            "subscribe",
            realm_function(generation, |this, args| subscribe(this, args)),
        );
        for (name, kind) in [("map", "map"), ("filter", "filter")] {
            prototype.set_property(
                name,
                realm_function(generation, move |this, args| {
                    transform(this, arg(&args, 0), kind, 0)
                }),
            );
        }
        for (name, kind) in [("take", "take"), ("drop", "drop")] {
            prototype.set_property(
                name,
                realm_function(generation, move |this, args| {
                    transform(this, Value::Undefined, kind, arg(&args, 0).to_u32())
                }),
            );
        }
        for name in [
            "toArray", "forEach", "every", "first", "last", "find", "some", "reduce",
        ] {
            prototype.set_property(
                name,
                realm_function(generation, move |this, args| {
                    promise_operator(this, args, name)
                }),
            );
        }
        prototype.set_property(
            "inspect",
            realm_function(generation, move |this, args| {
                let inspector = arg(&args, 0);
                let callback = if inspector.is_function() {
                    inspector
                } else {
                    inspector.get_property("next")
                };
                transform(
                    this,
                    realm_function(generation, move |_, args| {
                        callback.call(Value::Undefined, vec![arg(&args, 0)]);
                        arg(&args, 0)
                    }),
                    "map",
                    0,
                )
            }),
        );
        prototype.set_property(
            "finally",
            realm_function(generation, move |this, args| {
                let callback = arg(&args, 0);
                transform(
                    this,
                    realm_function(generation, move |_, args| {
                        let _ = &callback;
                        arg(&args, 0)
                    }),
                    "map",
                    0,
                )
            }),
        );
        for name in ["catch", "flatMap", "switchMap", "takeUntil"] {
            prototype.set_property(
                name,
                realm_function(generation, move |this, _| unsupported_operator(this, name)),
            );
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn subscriber_class() -> Value {
    SUBSCRIBER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let generation = crate::jsdom::realm_generation();
        let class = realm_function(generation, |_, _| {
            w3cos_core::throw_value(w3cos_core::error_instance(
                "TypeError",
                vec![Value::string("Illegal constructor: Subscriber")],
            ))
        });
        class.set_property("name", Value::string("Subscriber"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for method in ["addTeardown", "complete", "error", "next"] {
            prototype.set_property(method, realm_function(generation, |_, _| Value::Undefined));
        }
        for property in ["active", "signal"] {
            prototype.set_property(property, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn reset_realm() {
    OBSERVABLE_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
    SUBSCRIBER_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
    SUBSCRIBERS.with(|subscribers| {
        for state in subscribers.borrow_mut().drain(..) {
            state.active.set(false);
            state.teardowns.borrow_mut().clear();
            state.value.set_property("active", Value::Bool(false));
            state.value.set_property("signal", Value::Undefined);
            for method in ["addTeardown", "complete", "error", "next"] {
                state.value.set_property(method, Value::Undefined);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observable_from_map_filter_and_collect() {
        reset_realm();
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let source = observable_class().call_method(
            "from",
            vec![Value::array(vec![1.into(), 2.into(), 3.into()])],
        );
        let result = Rc::new(RefCell::new(Value::Undefined));
        let capture = Rc::clone(&result);
        source
            .call_method(
                "map",
                vec![Value::function(|_, args| {
                    Value::Number(arg(&args, 0).to_number() * 2.0)
                })],
            )
            .call_method(
                "filter",
                vec![Value::function(|_, args| {
                    Value::Bool(arg(&args, 0).to_number() > 2.0)
                })],
            )
            .call_method("toArray", vec![])
            .call_method(
                "then",
                vec![Value::function(move |_, args| {
                    *capture.borrow_mut() = arg(&args, 0);
                    Value::Undefined
                })],
            );
        w3cos_core::promise::drain_microtasks();
        assert_eq!(
            result
                .borrow()
                .iter()
                .map(|value| value.to_number())
                .collect::<Vec<_>>(),
            vec![4.0, 6.0]
        );
        reset_realm();
    }

    #[test]
    fn subscriptions_and_callbacks_are_realm_owned() {
        reset_realm();
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let subscriber_slot = Rc::new(RefCell::new(Value::Undefined));
        let subscriber_for_producer = Rc::clone(&subscriber_slot);
        let source = observable_from_producer(Value::function(move |_, args| {
            *subscriber_for_producer.borrow_mut() = arg(&args, 0);
            Value::Undefined
        }));
        let deliveries = Rc::new(Cell::new(0));
        let deliveries_for_observer = Rc::clone(&deliveries);
        source.call_method(
            "subscribe",
            vec![Value::function(move |_, _| {
                deliveries_for_observer.set(deliveries_for_observer.get() + 1);
                Value::Undefined
            })],
        );
        let subscriber = subscriber_slot.borrow().clone();
        subscriber.call_method("next", vec![Value::Number(1.0)]);
        assert_eq!(deliveries.get(), 1);
        let old_class = observable_class();
        let old_subscriber_class = subscriber_class();
        old_class
            .get_property("prototype")
            .set_property("oldRealmMarker", Value::Bool(true));

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let new_class = observable_class();
        let new_subscriber_class = subscriber_class();
        assert!(!old_class.strict_eq(&new_class));
        assert!(!old_subscriber_class.strict_eq(&new_subscriber_class));
        assert!(
            new_class
                .get_property("prototype")
                .get_property("oldRealmMarker")
                .is_undefined()
        );
        assert!(old_class.call(Value::Undefined, vec![]).is_undefined());
        assert!(
            source
                .call_method("subscribe", vec![Value::function(|_, _| Value::Undefined)])
                .is_undefined()
        );
        assert!(
            subscriber
                .call_method("next", vec![Value::Number(2.0)])
                .is_undefined()
        );
        assert_eq!(deliveries.get(), 1);
        assert!(!subscriber.get_property("active").to_bool());
        assert!(subscriber.get_property("signal").is_undefined());
        assert!(
            new_class
                .call_method("from", vec![Value::array(vec![Value::Number(1.0)])])
                .is_object()
        );
        reset_realm();
    }
}
