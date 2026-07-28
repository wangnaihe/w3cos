//! Process-local Web Locks API compatibility layer.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};

use crate::jsdom::realm_function;
use w3cos_core::Value;

#[derive(Clone, Copy, PartialEq, Eq)]
enum LockMode {
    Exclusive,
    Shared,
}

impl LockMode {
    fn parse(value: &Value) -> Option<Self> {
        match value.to_js_string().as_str() {
            "exclusive" | "undefined" => Some(Self::Exclusive),
            "shared" => Some(Self::Shared),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Exclusive => "exclusive",
            Self::Shared => "shared",
        }
    }
}

struct HeldLock {
    id: u64,
    name: String,
    mode: LockMode,
    reject: Value,
}

struct PendingLock {
    id: u64,
    generation: u32,
    name: String,
    mode: LockMode,
    callback: Value,
    resolve: Value,
    reject: Value,
    if_available: bool,
}

struct LockState {
    next_id: u64,
    held: Vec<HeldLock>,
    pending: VecDeque<PendingLock>,
}

thread_local! {
    static STATE: RefCell<LockState> = const { RefCell::new(LockState {
        next_id: 1,
        held: Vec::new(),
        pending: VecDeque::new(),
    }) };
    static LOCK_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static LOCK_MANAGER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static LOCK_MANAGER_VALUE: RefCell<Option<Value>> = const { RefCell::new(None) };
    static LOCK_WARNING_EMITTED: RefCell<bool> = const { RefCell::new(false) };
}

fn exception(name: &str, message: &str) -> Value {
    Value::object(HashMap::from([
        ("name".into(), Value::string(name)),
        ("message".into(), Value::string(message)),
    ]))
}

fn abort_error(message: &str) -> Value {
    exception("AbortError", message)
}

fn lock_value(name: &str, mode: LockMode) -> Value {
    let value = Value::object(HashMap::from([
        ("name".into(), Value::string(name)),
        ("mode".into(), Value::string(mode.as_str())),
    ]));
    w3cos_core::class::set_prototype_of(&value, &lock_class().get_property("prototype"));
    value
}

fn lock_info(name: &str, mode: LockMode) -> Value {
    Value::object(HashMap::from([
        ("name".into(), Value::string(name)),
        ("mode".into(), Value::string(mode.as_str())),
        ("clientId".into(), Value::string("w3cos-main")),
    ]))
}

fn available(state: &LockState, name: &str, mode: LockMode) -> bool {
    let mut held = state.held.iter().filter(|lock| lock.name == name);
    match mode {
        LockMode::Exclusive => held.count() == 0,
        LockMode::Shared => held.all(|lock| lock.mode == LockMode::Shared),
    }
}

fn schedule_process(generation: u32) {
    w3cos_core::promise::resolve(vec![Value::Undefined]).call_method(
        "then",
        vec![realm_function(generation, move |_, _| {
            process_queue(generation);
            Value::Undefined
        })],
    );
}

fn settle_callback(pending: PendingLock, lock: Value, releases_lock: bool) {
    let callback = pending.callback.clone();
    let callback_lock = lock.clone();
    let result = w3cos_core::promise::resolve(vec![Value::Undefined]).call_method(
        "then",
        vec![Value::function(move |_, _| {
            callback.call(Value::Undefined, vec![callback_lock.clone()])
        })],
    );
    let resolve = pending.resolve.clone();
    let id = pending.id;
    let generation = pending.generation;
    result.call_method(
        "then",
        vec![
            Value::function(move |_, args| {
                if releases_lock {
                    release(id, generation);
                }
                resolve.call(
                    Value::Undefined,
                    vec![args.first().cloned().unwrap_or(Value::Undefined)],
                );
                Value::Undefined
            }),
            {
                let reject = pending.reject;
                Value::function(move |_, args| {
                    if releases_lock {
                        release(id, generation);
                    }
                    reject.call(
                        Value::Undefined,
                        vec![args.first().cloned().unwrap_or(Value::Undefined)],
                    );
                    Value::Undefined
                })
            },
        ],
    );
}

fn release(id: u64, generation: u32) {
    if crate::jsdom::realm_generation() != generation {
        return;
    }
    let removed = STATE.with(|state| {
        let mut state = state.borrow_mut();
        let before = state.held.len();
        state.held.retain(|lock| lock.id != id);
        state.held.len() != before
    });
    if removed {
        schedule_process(generation);
    }
}

fn process_queue(generation: u32) {
    if crate::jsdom::realm_generation() != generation {
        return;
    }
    loop {
        enum Action {
            Grant(PendingLock),
            Unavailable(PendingLock),
            Stop,
        }
        let action = STATE.with(|state| {
            let mut state = state.borrow_mut();
            let Some(front) = state.pending.front() else {
                return Action::Stop;
            };
            if available(&state, &front.name, front.mode) {
                let pending = state.pending.pop_front().unwrap();
                state.held.push(HeldLock {
                    id: pending.id,
                    name: pending.name.clone(),
                    mode: pending.mode,
                    reject: pending.reject.clone(),
                });
                Action::Grant(pending)
            } else if front.if_available {
                Action::Unavailable(state.pending.pop_front().unwrap())
            } else {
                Action::Stop
            }
        });
        match action {
            Action::Grant(pending) => {
                let lock = lock_value(&pending.name, pending.mode);
                settle_callback(pending, lock, true);
            }
            Action::Unavailable(pending) => settle_callback(pending, Value::Null, false),
            Action::Stop => break,
        }
    }
}

pub fn lock_class() -> Value {
    LOCK_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let generation = crate::jsdom::realm_generation();
        let class = realm_function(generation, |_, _| Value::Undefined);
        class.set_property("name", Value::string("Lock"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property("mode", Value::Undefined);
        prototype.set_property("name", Value::Undefined);
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn lock_manager_class() -> Value {
    LOCK_MANAGER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let generation = crate::jsdom::realm_generation();
        let class = realm_function(generation, |_, _| lock_manager_value());
        class.set_property("name", Value::string("LockManager"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for method in ["query", "request"] {
            prototype.set_property(method, Value::function(|_, _| Value::Undefined));
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn lock_manager_value() -> Value {
    LOCK_MANAGER_VALUE.with(|slot| {
        if let Some(manager) = slot.borrow().clone() {
            return manager;
        }
        LOCK_WARNING_EMITTED.with(|warned| {
            if !warned.replace(true) {
                eprintln!(
                    "[w3cos] warning: navigator.locks coordinates this runtime process; \
                     cross-process and cross-device lock arbitration remain pending"
                );
            }
        });
        let generation = crate::jsdom::realm_generation();
        let manager = Value::object(HashMap::from([
            (
                "request".into(),
                realm_function(generation, move |_, args| {
                    let name = args
                        .first()
                        .cloned()
                        .unwrap_or(Value::Undefined)
                        .to_js_string();
                    let (options, callback) = if args.get(1).is_some_and(Value::is_function) {
                        (Value::Undefined, args[1].clone())
                    } else {
                        (
                            args.get(1).cloned().unwrap_or(Value::Undefined),
                            args.get(2).cloned().unwrap_or(Value::Undefined),
                        )
                    };
                    if name.is_empty() || name.starts_with('-') {
                        return w3cos_core::promise::reject(vec![exception(
                            "NotSupportedError",
                            "lock names must be non-empty and must not start with '-'",
                        )]);
                    }
                    if !callback.is_function() {
                        return w3cos_core::promise::reject(vec![exception(
                            "TypeError",
                            "navigator.locks.request requires a callback",
                        )]);
                    }
                    let mode_value = options.get_property("mode");
                    let Some(mode) = LockMode::parse(&mode_value) else {
                        return w3cos_core::promise::reject(vec![exception(
                            "TypeError",
                            "lock mode must be \"exclusive\" or \"shared\"",
                        )]);
                    };
                    let signal = options.get_property("signal");
                    if signal.get_property("aborted").to_bool() {
                        let reason = signal.get_property("reason");
                        return w3cos_core::promise::reject(vec![if reason.is_undefined() {
                            abort_error("the lock request was aborted")
                        } else {
                            reason
                        }]);
                    }
                    let if_available = options.get_property("ifAvailable").to_bool();
                    let steal = options.get_property("steal").to_bool();
                    if steal && (if_available || mode == LockMode::Shared) {
                        return w3cos_core::promise::reject(vec![exception(
                            "NotSupportedError",
                            "steal cannot be combined with ifAvailable or shared mode",
                        )]);
                    }

                    w3cos_core::promise::new(vec![Value::function(move |_, promise_args| {
                        let resolve = promise_args.first().cloned().unwrap_or(Value::Undefined);
                        let reject = promise_args.get(1).cloned().unwrap_or(Value::Undefined);
                        let id = STATE.with(|state| {
                            let mut state = state.borrow_mut();
                            let id = state.next_id;
                            state.next_id += 1;
                            if steal {
                                let stolen = state
                                    .held
                                    .iter()
                                    .filter(|held| held.name == name)
                                    .map(|held| held.reject.clone())
                                    .collect::<Vec<_>>();
                                state.held.retain(|held| held.name != name);
                                for reject in stolen {
                                    reject.call(
                                        Value::Undefined,
                                        vec![abort_error("the lock was stolen by another request")],
                                    );
                                }
                            }
                            let pending = PendingLock {
                                id,
                                generation,
                                name: name.clone(),
                                mode,
                                callback: callback.clone(),
                                resolve,
                                reject: reject.clone(),
                                if_available,
                            };
                            if steal {
                                state.pending.push_front(pending);
                            } else {
                                state.pending.push_back(pending);
                            }
                            id
                        });
                        if !signal.is_undefined() {
                            let reject_for_abort = reject.clone();
                            signal.call_method(
                                "addEventListener",
                                vec![
                                    Value::string("abort"),
                                    realm_function(generation, move |_, _| {
                                        let removed = STATE.with(|state| {
                                            let mut state = state.borrow_mut();
                                            let before = state.pending.len();
                                            state.pending.retain(|pending| pending.id != id);
                                            state.pending.len() != before
                                        });
                                        if removed {
                                            reject_for_abort.call(
                                                Value::Undefined,
                                                vec![abort_error("the lock request was aborted")],
                                            );
                                        }
                                        Value::Undefined
                                    }),
                                ],
                            );
                        }
                        schedule_process(generation);
                        Value::Undefined
                    })])
                }),
            ),
            (
                "query".into(),
                realm_function(generation, |_, _| {
                    let snapshot = STATE.with(|state| {
                        let state = state.borrow();
                        Value::object(HashMap::from([
                            (
                                "held".into(),
                                Value::array(
                                    state
                                        .held
                                        .iter()
                                        .map(|lock| lock_info(&lock.name, lock.mode))
                                        .collect(),
                                ),
                            ),
                            (
                                "pending".into(),
                                Value::array(
                                    state
                                        .pending
                                        .iter()
                                        .map(|lock| lock_info(&lock.name, lock.mode))
                                        .collect(),
                                ),
                            ),
                        ]))
                    });
                    w3cos_core::promise::resolve(vec![snapshot])
                }),
            ),
        ]));
        w3cos_core::class::set_prototype_of(
            &manager,
            &lock_manager_class().get_property("prototype"),
        );
        *slot.borrow_mut() = Some(manager.clone());
        manager
    })
}

pub fn reset() {
    STATE.with(|state| {
        *state.borrow_mut() = LockState {
            next_id: 1,
            held: Vec::new(),
            pending: VecDeque::new(),
        }
    });
    LOCK_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
    LOCK_MANAGER_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
    LOCK_MANAGER_VALUE.with(|slot| {
        slot.borrow_mut().take();
    });
    LOCK_WARNING_EMITTED.with(|warned| *warned.borrow_mut() = false);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn exclusive_queue_if_available_and_query_follow_promise_lifecycle() {
        reset();
        let manager = lock_manager_value();
        let release = Rc::new(RefCell::new(Value::Undefined));
        let release_for_callback = Rc::clone(&release);
        manager.call_method(
            "request",
            vec![
                Value::string("db"),
                Value::function(move |_, args| {
                    let lock = args[0].clone();
                    assert_eq!(lock.get_property("name"), Value::string("db"));
                    w3cos_core::promise::new(vec![Value::function({
                        let release_for_callback = Rc::clone(&release_for_callback);
                        move |_, args| {
                            *release_for_callback.borrow_mut() =
                                args.first().cloned().unwrap_or(Value::Undefined);
                            Value::Undefined
                        }
                    })])
                }),
            ],
        );
        w3cos_core::promise::drain_microtasks();

        let unavailable = Rc::new(Cell::new(false));
        let unavailable_for_callback = Rc::clone(&unavailable);
        manager.call_method(
            "request",
            vec![
                Value::string("db"),
                Value::object(HashMap::from([("ifAvailable".into(), Value::Bool(true))])),
                Value::function(move |_, args| {
                    unavailable_for_callback.set(args[0].is_null());
                    Value::Undefined
                }),
            ],
        );
        let second_ran = Rc::new(Cell::new(false));
        let second_for_callback = Rc::clone(&second_ran);
        manager.call_method(
            "request",
            vec![
                Value::string("db"),
                Value::function(move |_, _| {
                    second_for_callback.set(true);
                    Value::string("done")
                }),
            ],
        );
        w3cos_core::promise::drain_microtasks();
        assert!(unavailable.get());
        assert!(!second_ran.get());

        let snapshot = Rc::new(RefCell::new(Value::Undefined));
        let snapshot_for_callback = Rc::clone(&snapshot);
        manager.call_method("query", vec![]).call_method(
            "then",
            vec![Value::function(move |_, args| {
                *snapshot_for_callback.borrow_mut() = args[0].clone();
                Value::Undefined
            })],
        );
        w3cos_core::promise::drain_microtasks();
        assert_eq!(
            snapshot
                .borrow()
                .get_property("held")
                .get_property("length"),
            Value::Number(1.0)
        );
        assert_eq!(
            snapshot
                .borrow()
                .get_property("pending")
                .get_property("length"),
            Value::Number(1.0)
        );

        release
            .borrow()
            .call(Value::Undefined, vec![Value::Undefined]);
        w3cos_core::promise::drain_microtasks();
        assert!(second_ran.get());
    }

    #[test]
    fn shared_locks_run_together_and_abort_removes_queued_exclusive_request() {
        reset();
        let manager = lock_manager_value();
        let running = Rc::new(Cell::new(0_u32));
        let releases = Rc::new(RefCell::new(Vec::<Value>::new()));
        for _ in 0..2 {
            let running_for_callback = Rc::clone(&running);
            let releases_for_callback = Rc::clone(&releases);
            manager.call_method(
                "request",
                vec![
                    Value::string("shared"),
                    Value::object(HashMap::from([("mode".into(), Value::string("shared"))])),
                    Value::function(move |_, _| {
                        running_for_callback.set(running_for_callback.get() + 1);
                        w3cos_core::promise::new(vec![Value::function({
                            let releases_for_callback = Rc::clone(&releases_for_callback);
                            move |_, args| {
                                releases_for_callback
                                    .borrow_mut()
                                    .push(args.first().cloned().unwrap_or(Value::Undefined));
                                Value::Undefined
                            }
                        })])
                    }),
                ],
            );
        }
        w3cos_core::promise::drain_microtasks();
        assert_eq!(running.get(), 2);

        let controller =
            w3cos_core::class::construct(&crate::fetch::abort_controller_class(), vec![]);
        let rejected = Rc::new(Cell::new(false));
        let rejected_for_callback = Rc::clone(&rejected);
        manager
            .call_method(
                "request",
                vec![
                    Value::string("shared"),
                    Value::object(HashMap::from([(
                        "signal".into(),
                        controller.get_property("signal"),
                    )])),
                    Value::function(|_, _| Value::Undefined),
                ],
            )
            .call_method(
                "catch",
                vec![Value::function(move |_, args| {
                    rejected_for_callback
                        .set(args[0].get_property("name").to_js_string() == "AbortError");
                    Value::Undefined
                })],
            );
        w3cos_core::promise::drain_microtasks();
        controller.call_method("abort", vec![]);
        w3cos_core::promise::drain_microtasks();
        assert!(rejected.get());
        STATE.with(|state| {
            assert_eq!(state.borrow().held.len(), 2);
            assert!(state.borrow().pending.is_empty());
        });

        for release in std::mem::take(&mut *releases.borrow_mut()) {
            release.call(Value::Undefined, vec![Value::Undefined]);
        }
        w3cos_core::promise::drain_microtasks();
        STATE.with(|state| assert!(state.borrow().held.is_empty()));
    }

    #[test]
    fn steal_rejects_the_previous_holder_and_grants_the_new_request() {
        reset();
        let manager = lock_manager_value();
        let first = manager.call_method(
            "request",
            vec![
                Value::string("exclusive"),
                Value::function(|_, _| {
                    w3cos_core::promise::new(vec![Value::function(|_, _| Value::Undefined)])
                }),
            ],
        );
        let stolen = Rc::new(Cell::new(false));
        let stolen_for_callback = Rc::clone(&stolen);
        first.call_method(
            "catch",
            vec![Value::function(move |_, args| {
                stolen_for_callback
                    .set(args[0].get_property("name").to_js_string() == "AbortError");
                Value::Undefined
            })],
        );
        w3cos_core::promise::drain_microtasks();

        let replacement_ran = Rc::new(Cell::new(false));
        let replacement_for_callback = Rc::clone(&replacement_ran);
        manager.call_method(
            "request",
            vec![
                Value::string("exclusive"),
                Value::object(HashMap::from([("steal".into(), Value::Bool(true))])),
                Value::function(move |_, _| {
                    replacement_for_callback.set(true);
                    Value::Undefined
                }),
            ],
        );
        w3cos_core::promise::drain_microtasks();
        assert!(stolen.get());
        assert!(replacement_ran.get());
    }

    #[test]
    fn manager_requests_and_abort_listeners_are_realm_owned() {
        reset();
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        let old_manager = lock_manager_value();
        let old_manager_class = lock_manager_class();
        let old_lock_class = lock_class();
        old_manager_class
            .get_property("prototype")
            .set_property("oldRealmMarker", Value::Bool(true));

        let old_release = Rc::new(RefCell::new(Value::Undefined));
        old_manager.call_method(
            "request",
            vec![
                Value::string("realm-lock"),
                Value::function({
                    let old_release = Rc::clone(&old_release);
                    move |_, _| {
                        w3cos_core::promise::new(vec![Value::function({
                            let old_release = Rc::clone(&old_release);
                            move |_, args| {
                                *old_release.borrow_mut() =
                                    args.first().cloned().unwrap_or(Value::Undefined);
                                Value::Undefined
                            }
                        })])
                    }
                }),
            ],
        );
        let old_controller =
            w3cos_core::class::construct(&crate::fetch::abort_controller_class(), vec![]);
        old_manager.call_method(
            "request",
            vec![
                Value::string("realm-lock"),
                Value::object(HashMap::from([(
                    "signal".into(),
                    old_controller.get_property("signal"),
                )])),
                Value::function(|_, _| Value::Undefined),
            ],
        );
        w3cos_core::promise::drain_microtasks();
        STATE.with(|state| {
            assert_eq!(state.borrow().held.len(), 1);
            assert_eq!(state.borrow().pending.len(), 1);
        });

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        let new_manager = lock_manager_value();
        let new_manager_class = lock_manager_class();
        let new_lock_class = lock_class();
        assert!(old_manager != new_manager);
        assert!(old_manager_class != new_manager_class);
        assert!(old_lock_class != new_lock_class);
        assert!(
            new_manager_class
                .get_property("prototype")
                .get_property("oldRealmMarker")
                .is_undefined()
        );

        let new_release = Rc::new(RefCell::new(Value::Undefined));
        new_manager.call_method(
            "request",
            vec![
                Value::string("realm-lock"),
                Value::function({
                    let new_release = Rc::clone(&new_release);
                    move |_, _| {
                        w3cos_core::promise::new(vec![Value::function({
                            let new_release = Rc::clone(&new_release);
                            move |_, args| {
                                *new_release.borrow_mut() =
                                    args.first().cloned().unwrap_or(Value::Undefined);
                                Value::Undefined
                            }
                        })])
                    }
                }),
            ],
        );
        new_manager.call_method(
            "request",
            vec![
                Value::string("realm-lock"),
                Value::function(|_, _| Value::Undefined),
            ],
        );
        w3cos_core::promise::drain_microtasks();
        STATE.with(|state| {
            assert_eq!(state.borrow().held.len(), 1);
            assert_eq!(state.borrow().pending.len(), 1);
        });

        old_controller.call_method("abort", vec![]);
        old_release
            .borrow()
            .call(Value::Undefined, vec![Value::Undefined]);
        w3cos_core::promise::drain_microtasks();
        STATE.with(|state| {
            assert_eq!(state.borrow().held.len(), 1);
            assert_eq!(state.borrow().pending.len(), 1);
        });
        assert!(old_manager.call_method("request", vec![]).is_undefined());
        assert!(
            old_manager_class
                .call(Value::Undefined, vec![])
                .is_undefined()
        );

        new_release
            .borrow()
            .call(Value::Undefined, vec![Value::Undefined]);
        w3cos_core::promise::drain_microtasks();
        STATE.with(|state| {
            assert!(state.borrow().held.is_empty());
            assert!(state.borrow().pending.is_empty());
        });
        reset();
    }
}
