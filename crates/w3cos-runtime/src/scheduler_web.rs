//! Prioritized Task Scheduling API compatibility layer.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use crate::jsdom::realm_function;
use w3cos_core::Value;

#[derive(Clone, Copy, PartialEq, Eq)]
enum TaskPriority {
    UserBlocking,
    UserVisible,
    Background,
}

impl TaskPriority {
    fn parse(value: &Value) -> Option<Self> {
        match value.to_js_string().as_str() {
            "user-blocking" => Some(Self::UserBlocking),
            "user-visible" | "undefined" => Some(Self::UserVisible),
            "background" => Some(Self::Background),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::UserBlocking => "user-blocking",
            Self::UserVisible => "user-visible",
            Self::Background => "background",
        }
    }
}

struct TaskSignalState {
    priority: Cell<TaskPriority>,
    aborted: Cell<bool>,
    reason: RefCell<Value>,
    abort_listeners: RefCell<Vec<Value>>,
    priority_listeners: RefCell<Vec<Value>>,
}

thread_local! {
    static SCHEDULER_VALUE: RefCell<Option<Value>> = const { RefCell::new(None) };
    static SCHEDULER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static TASK_CONTROLLER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static TASK_SIGNAL_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static SCHEDULER_WARNING_EMITTED: Cell<bool> = const { Cell::new(false) };
}

pub fn scheduler_class() -> Value {
    SCHEDULER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let generation = crate::jsdom::realm_generation();
        let class = realm_function(generation, |_, _| {
            w3cos_core::throw_value(Value::object(HashMap::from([
                ("name".into(), Value::string("TypeError")),
                (
                    "message".into(),
                    Value::string("Illegal constructor: Scheduler"),
                ),
            ])))
        });
        class.set_property("name", Value::string("Scheduler"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for property in ["postTask", "yield"] {
            prototype.set_property(property, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn exception(name: &str, message: &str) -> Value {
    Value::object(HashMap::from([
        ("name".into(), Value::string(name)),
        ("message".into(), Value::string(message)),
    ]))
}

fn abort_error() -> Value {
    exception("AbortError", "the scheduled task was aborted")
}

fn dispatch(signal: &Value, state: &TaskSignalState, event_type: &str) {
    let event = Value::object(HashMap::from([
        ("type".into(), Value::string(event_type)),
        ("target".into(), signal.clone()),
        ("currentTarget".into(), signal.clone()),
    ]));
    let handler = signal.get_property(&format!("on{event_type}"));
    if handler.is_function() {
        handler.call(signal.clone(), vec![event.clone()]);
    }
    let listeners = if event_type == "abort" {
        state.abort_listeners.borrow().clone()
    } else {
        state.priority_listeners.borrow().clone()
    };
    for listener in listeners {
        listener.call(signal.clone(), vec![event.clone()]);
    }
}

fn task_signal_value(state: Rc<TaskSignalState>) -> Value {
    let generation = crate::jsdom::realm_generation();
    let signal = Value::object(HashMap::from([
        ("onabort".into(), Value::Null),
        ("onprioritychange".into(), Value::Null),
    ]));
    let priority_state = Rc::clone(&state);
    signal.set_property(
        "__w3cos_getter_priority",
        realm_function(generation, move |_, _| {
            Value::string(priority_state.priority.get().as_str())
        }),
    );
    let aborted_state = Rc::clone(&state);
    signal.set_property(
        "__w3cos_getter_aborted",
        realm_function(generation, move |_, _| {
            Value::Bool(aborted_state.aborted.get())
        }),
    );
    let reason_state = Rc::clone(&state);
    signal.set_property(
        "__w3cos_getter_reason",
        realm_function(generation, move |_, _| reason_state.reason.borrow().clone()),
    );
    let add_state = Rc::clone(&state);
    signal.set_property(
        "addEventListener",
        realm_function(generation, move |_, args| {
            let event_type = args
                .first()
                .cloned()
                .unwrap_or(Value::Undefined)
                .to_js_string();
            let listener = args.get(1).cloned().unwrap_or(Value::Undefined);
            if listener.is_function() {
                match event_type.as_str() {
                    "abort" => add_state.abort_listeners.borrow_mut().push(listener),
                    "prioritychange" => add_state.priority_listeners.borrow_mut().push(listener),
                    _ => {}
                }
            }
            Value::Undefined
        }),
    );
    let remove_state = Rc::clone(&state);
    signal.set_property(
        "removeEventListener",
        realm_function(generation, move |_, args| {
            let event_type = args
                .first()
                .cloned()
                .unwrap_or(Value::Undefined)
                .to_js_string();
            let listener = args.get(1).cloned().unwrap_or(Value::Undefined);
            let listeners = match event_type.as_str() {
                "abort" => Some(&remove_state.abort_listeners),
                "prioritychange" => Some(&remove_state.priority_listeners),
                _ => None,
            };
            if let Some(listeners) = listeners {
                listeners
                    .borrow_mut()
                    .retain(|candidate| !candidate.same_value_zero(&listener));
            }
            Value::Undefined
        }),
    );
    let throw_state = Rc::clone(&state);
    signal.set_property(
        "throwIfAborted",
        realm_function(generation, move |_, _| {
            if throw_state.aborted.get() {
                w3cos_core::throw_value(throw_state.reason.borrow().clone());
            }
            Value::Undefined
        }),
    );
    w3cos_core::class::set_prototype_of(&signal, &task_signal_class().get_property("prototype"));
    signal
}

pub fn task_signal_class() -> Value {
    TASK_SIGNAL_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let generation = crate::jsdom::realm_generation();
        let class = realm_function(generation, |_, _| Value::Undefined);
        class.set_property("name", Value::string("TaskSignal"));
        class.set_property(
            "any",
            realm_function(generation, |_, args| {
                let signals = args
                    .first()
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .iter()
                    .collect::<Vec<_>>();
                let priority = signals
                    .first()
                    .map(|signal| signal.get_property("priority"))
                    .filter(|priority| !priority.is_undefined())
                    .unwrap_or_else(|| Value::string("user-visible"));
                let options = Value::object(HashMap::from([("priority".into(), priority)]));
                let controller =
                    w3cos_core::class::construct(&task_controller_class(), vec![options]);
                let aggregate = controller.get_property("signal");
                for signal in signals {
                    if signal.get_property("aborted").to_bool() {
                        controller.call_method("abort", vec![signal.get_property("reason")]);
                        break;
                    }
                    let controller_for_abort = controller.clone();
                    let signal_for_reason = signal.clone();
                    signal.call_method(
                        "addEventListener",
                        vec![
                            Value::string("abort"),
                            Value::function(move |_, _| {
                                controller_for_abort.call_method(
                                    "abort",
                                    vec![signal_for_reason.get_property("reason")],
                                )
                            }),
                        ],
                    );
                }
                aggregate
            }),
        );
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property("onprioritychange", Value::Undefined);
        prototype.set_property("priority", Value::Undefined);
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::fetch::abort_signal_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn task_controller_class() -> Value {
    TASK_CONTROLLER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let generation = crate::jsdom::realm_generation();
        let class = realm_function(generation, move |_, args| {
            let options = args.first().cloned().unwrap_or(Value::Undefined);
            let priority_value = options.get_property("priority");
            let priority = if priority_value.is_undefined() {
                TaskPriority::UserVisible
            } else if let Some(priority) = TaskPriority::parse(&priority_value) {
                priority
            } else {
                w3cos_core::throw_value(exception(
                    "TypeError",
                    "task priority must be user-blocking, user-visible, or background",
                ));
            };
            let state = Rc::new(TaskSignalState {
                priority: Cell::new(priority),
                aborted: Cell::new(false),
                reason: RefCell::new(Value::Undefined),
                abort_listeners: RefCell::new(Vec::new()),
                priority_listeners: RefCell::new(Vec::new()),
            });
            let signal = task_signal_value(Rc::clone(&state));
            let signal_for_abort = signal.clone();
            let abort_state = Rc::clone(&state);
            let signal_for_priority = signal.clone();
            let priority_state = Rc::clone(&state);
            let controller_generation = generation;
            let controller = Value::object(HashMap::from([
                ("signal".into(), signal),
                (
                    "abort".into(),
                    realm_function(controller_generation, move |_, args| {
                        if abort_state.aborted.replace(true) {
                            return Value::Undefined;
                        }
                        *abort_state.reason.borrow_mut() =
                            args.first().cloned().unwrap_or_else(abort_error);
                        dispatch(&signal_for_abort, &abort_state, "abort");
                        Value::Undefined
                    }),
                ),
                (
                    "setPriority".into(),
                    realm_function(controller_generation, move |_, args| {
                        let value = args.first().cloned().unwrap_or(Value::Undefined);
                        let Some(priority) = TaskPriority::parse(&value) else {
                            w3cos_core::throw_value(exception(
                                "TypeError",
                                "task priority must be user-blocking, user-visible, or background",
                            ));
                        };
                        if priority_state.priority.replace(priority) != priority {
                            dispatch(&signal_for_priority, &priority_state, "prioritychange");
                        }
                        Value::Undefined
                    }),
                ),
            ]));
            w3cos_core::class::set_prototype_of(
                &controller,
                &task_controller_class().get_property("prototype"),
            );
            controller
        });
        class.set_property("name", Value::string("TaskController"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property("setPriority", Value::function(|_, _| Value::Undefined));
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::fetch::abort_controller_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn post_task(args: Vec<Value>) -> Value {
    let callback = args.first().cloned().unwrap_or(Value::Undefined);
    let options = args.get(1).cloned().unwrap_or(Value::Undefined);
    if !callback.is_function() {
        return w3cos_core::promise::reject(vec![exception(
            "TypeError",
            "scheduler.postTask requires a callback",
        )]);
    }
    if TaskPriority::parse(&options.get_property("priority")).is_none() {
        return w3cos_core::promise::reject(vec![exception(
            "TypeError",
            "task priority must be user-blocking, user-visible, or background",
        )]);
    }
    let signal = options.get_property("signal");
    if signal.get_property("aborted").to_bool() {
        let reason = signal.get_property("reason");
        return w3cos_core::promise::reject(vec![if reason.is_undefined() {
            abort_error()
        } else {
            reason
        }]);
    }
    let delay = options.get_property("delay").to_number().max(0.0);
    w3cos_core::promise::new(vec![Value::function(move |_, promise_args| {
        let resolve = promise_args.first().cloned().unwrap_or(Value::Undefined);
        let reject = promise_args.get(1).cloned().unwrap_or(Value::Undefined);
        if !signal.is_undefined() {
            let reject_for_abort = reject.clone();
            let signal_for_reason = signal.clone();
            signal.call_method(
                "addEventListener",
                vec![
                    Value::string("abort"),
                    Value::function(move |_, _| {
                        let reason = signal_for_reason.get_property("reason");
                        reject_for_abort.call(
                            Value::Undefined,
                            vec![if reason.is_undefined() {
                                abort_error()
                            } else {
                                reason
                            }],
                        );
                        Value::Undefined
                    }),
                ],
            );
        }
        let signal_for_task = signal.clone();
        let reject_for_task = reject.clone();
        let callback_for_task = callback.clone();
        let task = Value::function(move |_, _| {
            if signal_for_task.get_property("aborted").to_bool() {
                let reason = signal_for_task.get_property("reason");
                reject_for_task.call(
                    Value::Undefined,
                    vec![if reason.is_undefined() {
                        abort_error()
                    } else {
                        reason
                    }],
                );
                return Value::Undefined;
            }
            let callback = callback_for_task.clone();
            let result = w3cos_core::promise::resolve(vec![Value::Undefined]).call_method(
                "then",
                vec![Value::function(move |_, _| {
                    callback.call(Value::Undefined, vec![])
                })],
            );
            result.call_method("then", vec![resolve.clone(), reject_for_task.clone()]);
            Value::Undefined
        });
        crate::jsdom::window_value()
            .get_property("setTimeout")
            .call(Value::Undefined, vec![task, Value::Number(delay)]);
        Value::Undefined
    })])
}

pub fn scheduler_value() -> Value {
    SCHEDULER_VALUE.with(|slot| {
        if let Some(scheduler) = slot.borrow().clone() {
            return scheduler;
        }
        SCHEDULER_WARNING_EMITTED.with(|warned| {
            if !warned.replace(true) {
                eprintln!(
                    "[w3cos] warning: scheduler exposes compatible priorities and timing, \
                     but native priority-aware task execution remains pending"
                );
            }
        });
        let generation = crate::jsdom::realm_generation();
        let scheduler = Value::object(HashMap::from([
            (
                "postTask".into(),
                realm_function(generation, |_, args| post_task(args)),
            ),
            (
                "yield".into(),
                realm_function(generation, |_, _| {
                    post_task(vec![Value::function(|_, _| Value::Undefined)])
                }),
            ),
        ]));
        w3cos_core::class::set_prototype_of(
            &scheduler,
            &scheduler_class().get_property("prototype"),
        );
        *slot.borrow_mut() = Some(scheduler.clone());
        scheduler
    })
}

pub fn reset() {
    SCHEDULER_VALUE.with(|slot| {
        slot.borrow_mut().take();
    });
    SCHEDULER_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
    TASK_CONTROLLER_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
    TASK_SIGNAL_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
    SCHEDULER_WARNING_EMITTED.with(|warned| warned.set(false));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_controller_changes_priority_and_aborts_posted_tasks() {
        crate::jsdom::reset_bridge();
        let controller = w3cos_core::class::construct(
            &task_controller_class(),
            vec![Value::object(HashMap::from([(
                "priority".into(),
                Value::string("background"),
            )]))],
        );
        let signal = controller.get_property("signal");
        assert_eq!(signal.get_property("priority"), Value::string("background"));
        assert!(w3cos_core::class::instance_of(
            &signal,
            &task_signal_class()
        ));

        let priority_changes = Rc::new(Cell::new(0_u32));
        let changes_for_listener = Rc::clone(&priority_changes);
        signal.call_method(
            "addEventListener",
            vec![
                Value::string("prioritychange"),
                Value::function(move |_, _| {
                    changes_for_listener.set(changes_for_listener.get() + 1);
                    Value::Undefined
                }),
            ],
        );
        controller.call_method("setPriority", vec![Value::string("user-blocking")]);
        assert_eq!(
            signal.get_property("priority"),
            Value::string("user-blocking")
        );
        assert_eq!(priority_changes.get(), 1);

        let ran = Rc::new(Cell::new(false));
        let ran_for_task = Rc::clone(&ran);
        let rejected = Rc::new(Cell::new(false));
        let rejected_for_catch = Rc::clone(&rejected);
        scheduler_value()
            .call_method(
                "postTask",
                vec![
                    Value::function(move |_, _| {
                        ran_for_task.set(true);
                        Value::Undefined
                    }),
                    Value::object(HashMap::from([("signal".into(), signal)])),
                ],
            )
            .call_method(
                "catch",
                vec![Value::function(move |_, args| {
                    rejected_for_catch
                        .set(args[0].get_property("name").to_js_string() == "AbortError");
                    Value::Undefined
                })],
            );
        controller.call_method("abort", vec![]);
        crate::jsdom::tick_timers();
        crate::jsdom::drain_microtasks();
        assert!(!ran.get());
        assert!(rejected.get());
    }

    #[test]
    fn scheduler_and_task_constructors_are_realm_owned() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        let old_scheduler = scheduler_value();
        let old_scheduler_class = scheduler_class();
        let old_controller_class = task_controller_class();
        let old_signal_class = task_signal_class();
        old_scheduler_class
            .get_property("prototype")
            .set_property("realmMarker", Value::Bool(true));

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        let new_scheduler = scheduler_value();
        let new_scheduler_class = scheduler_class();
        let new_controller_class = task_controller_class();
        let new_signal_class = task_signal_class();
        assert!(!old_scheduler.strict_eq(&new_scheduler));
        assert!(!old_scheduler_class.strict_eq(&new_scheduler_class));
        assert!(!old_controller_class.strict_eq(&new_controller_class));
        assert!(!old_signal_class.strict_eq(&new_signal_class));
        assert!(
            !new_scheduler_class
                .get_property("prototype")
                .get_property("realmMarker")
                .to_bool()
        );

        let stale_ran = Rc::new(Cell::new(false));
        let stale_ran_for_task = Rc::clone(&stale_ran);
        assert!(matches!(
            old_scheduler.call_method(
                "postTask",
                vec![Value::function(move |_, _| {
                    stale_ran_for_task.set(true);
                    Value::Undefined
                })],
            ),
            Value::Undefined
        ));
        assert!(matches!(
            old_controller_class.call(Value::Undefined, vec![]),
            Value::Undefined
        ));
        assert!(matches!(
            old_signal_class.call(Value::Undefined, vec![]),
            Value::Undefined
        ));
        crate::jsdom::tick_timers();
        crate::jsdom::drain_microtasks();
        assert!(!stale_ran.get());

        let fresh_ran = Rc::new(Cell::new(false));
        let fresh_ran_for_task = Rc::clone(&fresh_ran);
        new_scheduler.call_method(
            "postTask",
            vec![Value::function(move |_, _| {
                fresh_ran_for_task.set(true);
                Value::Undefined
            })],
        );
        crate::jsdom::tick_timers();
        crate::jsdom::drain_microtasks();
        assert!(fresh_ran.get());
    }

    #[test]
    fn post_task_and_yield_are_async_and_adopt_callback_results() {
        crate::jsdom::reset_bridge();
        let result = Rc::new(RefCell::new(Value::Undefined));
        let result_for_then = Rc::clone(&result);
        scheduler_value()
            .call_method(
                "postTask",
                vec![Value::function(|_, _| {
                    w3cos_core::promise::resolve(vec![Value::string("complete")])
                })],
            )
            .call_method(
                "then",
                vec![Value::function(move |_, args| {
                    *result_for_then.borrow_mut() = args[0].clone();
                    Value::Undefined
                })],
            );
        assert!(result.borrow().is_undefined());
        assert_eq!(crate::jsdom::tick_timers(), 1);
        crate::jsdom::drain_microtasks();
        assert_eq!(*result.borrow(), Value::string("complete"));

        let yielded = Rc::new(Cell::new(false));
        let yielded_for_then = Rc::clone(&yielded);
        scheduler_value().call_method("yield", vec![]).call_method(
            "then",
            vec![Value::function(move |_, _| {
                yielded_for_then.set(true);
                Value::Undefined
            })],
        );
        assert!(!yielded.get());
        assert_eq!(crate::jsdom::tick_timers(), 1);
        crate::jsdom::drain_microtasks();
        assert!(yielded.get());
    }
}
