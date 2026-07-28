//! JavaScript facades over the native worker and message-channel engines.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::{Rc, Weak};

use w3cos_core::Value;

use crate::jsdom::realm_function;
use crate::worker::{Worker, WorkerEvent, WorkerOptions};

#[derive(Clone)]
struct JsWorker {
    native: Rc<RefCell<Option<Worker>>>,
    value: Value,
}

#[derive(Clone)]
struct BroadcastEntry {
    id: u64,
    name: String,
    value: Value,
    closed: Rc<Cell<bool>>,
}

thread_local! {
    static WORKERS: RefCell<Vec<JsWorker>> = const { RefCell::new(Vec::new()) };
    static WORKER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static SHARED_WORKER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static MESSAGE_PORT_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static MESSAGE_CHANNEL_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static BROADCAST_CHANNEL_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static BROADCAST_CHANNELS: RefCell<Vec<BroadcastEntry>> = const { RefCell::new(Vec::new()) };
    static MESSAGE_PORTS: RefCell<Vec<Weak<RefCell<PortState>>>> = const { RefCell::new(Vec::new()) };
    static NEXT_BROADCAST_CHANNEL_ID: Cell<u64> = const { Cell::new(1) };
    static WORKER_PORT_TRANSFER_WARNED: Cell<bool> = const { Cell::new(false) };
}

fn core_to_json(value: Value) -> serde_json::Value {
    crate::indexed_db_web::value_to_json(&value).unwrap_or_else(|error| {
        w3cos_core::throw_value(w3cos_core::web::dom_exception_instance(
            &error.message,
            &error.name,
        ))
    })
}

fn json_to_core(value: serde_json::Value) -> Value {
    crate::indexed_db_web::json_to_value(value)
}

fn event_with_data(event_type: &str, data: Value) -> Value {
    let event = w3cos_core::class::construct(
        &crate::web_events::event_subclass_class("MessageEvent"),
        vec![
            Value::string(event_type),
            Value::object(HashMap::from([("data".to_string(), data)])),
        ],
    );
    event
}

struct PortState {
    value: Value,
    handler: Value,
    queued: VecDeque<Value>,
    started: bool,
    closed: bool,
    peer: Option<Weak<RefCell<PortState>>>,
}

fn dispatch_port_message(target: &Value, data: Value) {
    target.call_method("dispatchEvent", vec![event_with_data("message", data)]);
}

fn enqueue_port_message(state: &Rc<RefCell<PortState>>, args: &[Value]) {
    let data = args.first().cloned().unwrap_or(Value::Undefined);
    let transfer = args.get(1).cloned().unwrap_or(Value::Undefined);
    let data = w3cos_core::web::structured_clone(vec![
        data,
        Value::object(HashMap::from([("transfer".to_string(), transfer)])),
    ]);
    let target = {
        let mut state = state.borrow_mut();
        if state.closed {
            return;
        }
        if !state.started {
            state.queued.push_back(data);
            return;
        }
        state.value.clone()
    };
    dispatch_port_message(&target, data);
}

fn start_port(state: &Rc<RefCell<PortState>>) {
    let (target, queued) = {
        let mut state = state.borrow_mut();
        if state.closed {
            return;
        }
        state.started = true;
        (
            state.value.clone(),
            state.queued.drain(..).collect::<Vec<_>>(),
        )
    };
    for data in queued {
        dispatch_port_message(&target, data);
    }
}

pub fn worker_class() -> Value {
    WORKER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let generation = crate::jsdom::realm_generation();
        let class = realm_function(generation, move |_, args| {
            let url = args.first().cloned().unwrap_or_default().to_js_string();
            let options = args.get(1).cloned().unwrap_or_default();
            let name = options.get_property("name").to_js_string();
            let native = Worker::spawn(
                if name.is_empty() {
                    WorkerOptions::default()
                } else {
                    WorkerOptions::named(name)
                },
                |scope| {
                    while let Some(message) = scope.recv() {
                        let _ = scope.post_message(message);
                    }
                },
            );
            let native = Rc::new(RefCell::new(Some(native)));
            let value = Value::object(HashMap::from([
                ("onmessage".to_string(), Value::Null),
                ("onmessageerror".to_string(), Value::Null),
                ("onerror".to_string(), Value::Null),
                ("url".to_string(), Value::string(&url)),
            ]));
            crate::web_events::event_target_class().call(value.clone(), vec![]);
            let native_for_post = Rc::clone(&native);
            value.set_property(
                "postMessage",
                realm_function(generation, move |_, args| {
                    if let Some(worker) = native_for_post.borrow().as_ref() {
                        let data = args.first().cloned().unwrap_or_default();
                        let transfer = args.get(1).cloned().unwrap_or(Value::Undefined);
                        if transfer.iter().any(|item| {
                            w3cos_core::class::instance_of(&item, &message_port_class())
                        }) {
                            WORKER_PORT_TRANSFER_WARNED.with(|warned| {
                                if !warned.replace(true) {
                                    eprintln!(
                                        "W3COS warning: Worker MessagePort transfer is not \
                                         available until worker script realms are implemented"
                                    );
                                }
                            });
                            w3cos_core::throw_value(w3cos_core::web::dom_exception_instance(
                                "MessagePort cannot yet be transferred into a Worker realm.",
                                "DataCloneError",
                            ));
                        }
                        let cloned = w3cos_core::web::structured_clone(vec![
                            data,
                            Value::object(HashMap::from([("transfer".to_string(), transfer)])),
                        ]);
                        let _ = worker.post_message(core_to_json(cloned));
                    }
                    Value::Undefined
                }),
            );
            let native_for_terminate = Rc::clone(&native);
            value.set_property(
                "terminate",
                realm_function(generation, move |_, _| {
                    if let Some(worker) = native_for_terminate.borrow_mut().take() {
                        worker.terminate();
                    }
                    Value::Undefined
                }),
            );
            w3cos_core::class::set_prototype_of(&value, &worker_class().get_property("prototype"));
            WORKERS.with(|workers| {
                workers.borrow_mut().push(JsWorker {
                    native,
                    value: value.clone(),
                })
            });
            value
        });
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in ["onerror", "onmessage", "postMessage", "terminate"] {
            prototype.set_property(member, Value::Undefined);
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

pub fn poll_js_events() -> usize {
    let workers = WORKERS.with(|workers| workers.borrow().clone());
    let mut dispatched = 0;
    for worker in workers {
        let events = worker
            .native
            .borrow()
            .as_ref()
            .map(Worker::poll_events)
            .unwrap_or_default();
        for event in events {
            let (event_type, event) = match event {
                WorkerEvent::Message(data) => {
                    ("message", event_with_data("message", json_to_core(data)))
                }
                WorkerEvent::Error(message) => {
                    let event = event_with_data("error", Value::Undefined);
                    event.set_property("message", Value::string(&message));
                    ("error", event)
                }
                WorkerEvent::Exit => continue,
            };
            worker.value.call_method("dispatchEvent", vec![event]);
            if worker
                .value
                .get_property(&format!("on{event_type}"))
                .is_function()
            {
                dispatched += 1;
            }
        }
    }
    dispatched
}

fn port_value_for_state(state: Rc<RefCell<PortState>>) -> Value {
    let generation = crate::jsdom::realm_generation();
    let value = Value::object(HashMap::from([("onmessageerror".to_string(), Value::Null)]));
    crate::web_events::event_target_class().call(value.clone(), vec![]);
    let active = Rc::new(Cell::new(true));
    let state_for_getter = Rc::clone(&state);
    let active_for_getter = Rc::clone(&active);
    value.set_property(
        "__w3cos_getter_onmessage",
        realm_function(generation, move |_, _| {
            if active_for_getter.get() {
                state_for_getter.borrow().handler.clone()
            } else {
                Value::Null
            }
        }),
    );
    let state_for_setter = Rc::clone(&state);
    let active_for_setter = Rc::clone(&active);
    value.set_property(
        "__w3cos_setter_onmessage",
        realm_function(generation, move |_, args| {
            if !active_for_setter.get() {
                return Value::Undefined;
            }
            state_for_setter.borrow_mut().handler = args.first().cloned().unwrap_or(Value::Null);
            start_port(&state_for_setter);
            Value::Undefined
        }),
    );
    let state_for_start = Rc::clone(&state);
    let active_for_start = Rc::clone(&active);
    value.set_property(
        "start",
        realm_function(generation, move |_, _| {
            if active_for_start.get() {
                start_port(&state_for_start);
            }
            Value::Undefined
        }),
    );
    let state_for_close = Rc::clone(&state);
    let active_for_close = Rc::clone(&active);
    value.set_property(
        "close",
        realm_function(generation, move |this, _| {
            if !active_for_close.get() {
                return Value::Undefined;
            }
            w3cos_core::web::unregister_host_transferable(&this);
            let mut state = state_for_close.borrow_mut();
            state.closed = true;
            state.queued.clear();
            Value::Undefined
        }),
    );
    let state_for_post = Rc::clone(&state);
    let active_for_post = Rc::clone(&active);
    value.set_property(
        "postMessage",
        realm_function(generation, move |_, args| {
            if !active_for_post.get() {
                return Value::Undefined;
            }
            let peer = {
                let state = state_for_post.borrow();
                if state.closed {
                    return Value::Undefined;
                }
                state.peer.as_ref().and_then(Weak::upgrade)
            };
            if let Some(peer) = peer {
                enqueue_port_message(&peer, &args);
            }
            Value::Undefined
        }),
    );
    let pending_transfer = Rc::new(RefCell::new(None::<Value>));
    let state_for_prepare = Rc::clone(&state);
    let active_for_prepare = Rc::clone(&active);
    let pending_for_prepare = Rc::clone(&pending_transfer);
    let prepare = realm_function(generation, move |_, _| {
        if !active_for_prepare.get() || state_for_prepare.borrow().closed {
            w3cos_core::throw_value(w3cos_core::web::dom_exception_instance(
                "MessagePort is already detached or closed.",
                "DataCloneError",
            ));
        }
        if let Some(stale) = pending_for_prepare.borrow_mut().take() {
            w3cos_core::web::unregister_host_transferable(&stale);
        }
        let transferred = port_value_for_state(Rc::clone(&state_for_prepare));
        *pending_for_prepare.borrow_mut() = Some(transferred.clone());
        transferred
    });
    let state_for_finalize = Rc::clone(&state);
    let active_for_finalize = Rc::clone(&active);
    let pending_for_finalize = Rc::clone(&pending_transfer);
    let finalize = realm_function(generation, move |this, _| {
        let Some(transferred) = pending_for_finalize.borrow_mut().take() else {
            return Value::Undefined;
        };
        w3cos_core::web::unregister_host_transferable(&this);
        active_for_finalize.set(false);
        let mut state = state_for_finalize.borrow_mut();
        state.value = transferred;
        state.handler = Value::Null;
        state.started = false;
        Value::Undefined
    });
    w3cos_core::class::set_prototype_of(&value, &message_port_class().get_property("prototype"));
    w3cos_core::web::register_host_transferable(&value, prepare, finalize);
    value
}

fn message_port_value() -> (Value, Rc<RefCell<PortState>>) {
    let state = Rc::new(RefCell::new(PortState {
        value: Value::Undefined,
        handler: Value::Null,
        queued: VecDeque::new(),
        started: false,
        closed: false,
        peer: None,
    }));
    let value = port_value_for_state(Rc::clone(&state));
    state.borrow_mut().value = value.clone();
    MESSAGE_PORTS.with(|ports| ports.borrow_mut().push(Rc::downgrade(&state)));
    (value, state)
}

pub fn message_port_class() -> Value {
    MESSAGE_PORT_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let generation = crate::jsdom::realm_generation();
        let class = realm_function(generation, |_, _| message_port_value().0);
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in [
            "close",
            "onmessage",
            "onmessageerror",
            "postMessage",
            "start",
        ] {
            prototype.set_property(member, Value::Undefined);
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

fn channel_value() -> Value {
    let (port1, state1) = message_port_value();
    let (port2, state2) = message_port_value();
    state1.borrow_mut().peer = Some(Rc::downgrade(&state2));
    state2.borrow_mut().peer = Some(Rc::downgrade(&state1));
    Value::object(HashMap::from([
        ("port1".to_string(), port1),
        ("port2".to_string(), port2),
    ]))
}

pub fn message_channel_class() -> Value {
    MESSAGE_CHANNEL_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let generation = crate::jsdom::realm_generation();
        let class = realm_function(generation, |_, _| channel_value());
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property("port1", Value::Undefined);
        prototype.set_property("port2", Value::Undefined);
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn broadcast_channel_class() -> Value {
    BROADCAST_CHANNEL_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let generation = crate::jsdom::realm_generation();
        let class = realm_function(generation, move |_, args| {
            let name = args
                .first()
                .cloned()
                .unwrap_or(Value::Undefined)
                .to_js_string();
            let id = NEXT_BROADCAST_CHANNEL_ID.with(|next| {
                let id = next.get();
                next.set(id + 1);
                id
            });
            let closed = Rc::new(Cell::new(false));
            let value = Value::object(HashMap::from([
                ("name".to_string(), Value::string(&name)),
                ("onmessage".to_string(), Value::Null),
                ("onmessageerror".to_string(), Value::Null),
            ]));
            crate::web_events::event_target_class().call(value.clone(), vec![]);

            let name_for_post = name.clone();
            let closed_for_post = Rc::clone(&closed);
            value.set_property(
                "postMessage",
                realm_function(generation, move |_, args| {
                    if closed_for_post.get() {
                        w3cos_core::throw_value(w3cos_core::web::dom_exception_instance(
                            "BroadcastChannel is closed.",
                            "InvalidStateError",
                        ));
                    }
                    let data = args.first().cloned().unwrap_or(Value::Undefined);
                    let snapshot = w3cos_core::web::structured_clone(vec![data]);
                    let recipients = BROADCAST_CHANNELS.with(|channels| {
                        channels
                            .borrow()
                            .iter()
                            .filter(|entry| {
                                entry.id != id && entry.name == name_for_post && !entry.closed.get()
                            })
                            .cloned()
                            .collect::<Vec<_>>()
                    });
                    for recipient in recipients {
                        let delivery = w3cos_core::web::structured_clone(vec![snapshot.clone()]);
                        crate::jsdom::queue_microtask_value(realm_function(
                            generation,
                            move |_, _| {
                                if !recipient.closed.get() {
                                    recipient.value.call_method(
                                        "dispatchEvent",
                                        vec![event_with_data("message", delivery.clone())],
                                    );
                                }
                                Value::Undefined
                            },
                        ));
                    }
                    Value::Undefined
                }),
            );

            let closed_for_close = Rc::clone(&closed);
            value.set_property(
                "close",
                realm_function(generation, move |_, _| {
                    closed_for_close.set(true);
                    BROADCAST_CHANNELS.with(|channels| {
                        channels.borrow_mut().retain(|entry| entry.id != id);
                    });
                    Value::Undefined
                }),
            );
            w3cos_core::class::set_prototype_of(
                &value,
                &broadcast_channel_class().get_property("prototype"),
            );
            BROADCAST_CHANNELS.with(|channels| {
                channels.borrow_mut().push(BroadcastEntry {
                    id,
                    name,
                    value: value.clone(),
                    closed,
                });
            });
            value
        });
        class.set_property("name", Value::string("BroadcastChannel"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in [
            "close",
            "name",
            "onmessage",
            "onmessageerror",
            "postMessage",
        ] {
            prototype.set_property(member, Value::Undefined);
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

pub fn shared_worker_class() -> Value {
    SHARED_WORKER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let generation = crate::jsdom::realm_generation();
        let class = realm_function(generation, move |_, _| {
            let channel = channel_value();
            let public_port = channel.get_property("port1");
            let worker_port = channel.get_property("port2");
            let echo_port = worker_port.clone();
            worker_port.set_property(
                "onmessage",
                realm_function(generation, move |_, args| {
                    let event = args.first().cloned().unwrap_or_default();
                    echo_port.call_method("postMessage", vec![event.get_property("data")]);
                    Value::Undefined
                }),
            );
            let value = Value::object(HashMap::from([("port".to_string(), public_port)]));
            w3cos_core::class::set_prototype_of(
                &value,
                &shared_worker_class().get_property("prototype"),
            );
            value
        });
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property("onerror", Value::Undefined);
        prototype.set_property("port", Value::Undefined);
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn clear_event_target(value: &Value) {
    for member in [
        "addEventListener",
        "removeEventListener",
        "dispatchEvent",
        "onmessage",
        "onmessageerror",
        "onerror",
    ] {
        value.set_property(member, Value::Undefined);
    }
}

pub fn reset_realm() {
    WORKERS.with(|workers| {
        for worker in workers.borrow_mut().drain(..) {
            if let Some(native) = worker.native.borrow_mut().take() {
                native.terminate();
            }
            clear_event_target(&worker.value);
            for member in ["postMessage", "terminate"] {
                worker.value.set_property(member, Value::Undefined);
            }
        }
    });
    BROADCAST_CHANNELS.with(|channels| {
        for channel in channels.borrow_mut().drain(..) {
            channel.closed.set(true);
            clear_event_target(&channel.value);
            for member in ["postMessage", "close"] {
                channel.value.set_property(member, Value::Undefined);
            }
        }
    });
    MESSAGE_PORTS.with(|ports| {
        for state in ports
            .borrow_mut()
            .drain(..)
            .filter_map(|state| state.upgrade())
        {
            let value = {
                let mut state = state.borrow_mut();
                state.handler = Value::Undefined;
                state.queued.clear();
                state.started = false;
                state.closed = true;
                state.peer = None;
                state.value.clone()
            };
            w3cos_core::web::unregister_host_transferable(&value);
            clear_event_target(&value);
            for member in [
                "__w3cos_getter_onmessage",
                "__w3cos_setter_onmessage",
                "postMessage",
                "start",
                "close",
            ] {
                value.set_property(member, Value::Undefined);
            }
        }
    });
    for slot in [
        &WORKER_CLASS,
        &SHARED_WORKER_CLASS,
        &MESSAGE_PORT_CLASS,
        &MESSAGE_CHANNEL_CLASS,
        &BROADCAST_CHANNEL_CLASS,
    ] {
        slot.with(|slot| {
            slot.borrow_mut().take();
        });
    }
    NEXT_BROADCAST_CHANNEL_ID.with(|next| next.set(1));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    #[test]
    fn broadcast_channel_clones_asynchronously_filters_names_and_closes() {
        let sender = w3cos_core::class::construct(
            &broadcast_channel_class(),
            vec![Value::string("runtime-test")],
        );
        let receiver = w3cos_core::class::construct(
            &broadcast_channel_class(),
            vec![Value::string("runtime-test")],
        );
        let other = w3cos_core::class::construct(
            &broadcast_channel_class(),
            vec![Value::string("runtime-test-other")],
        );
        let received = Rc::new(RefCell::new(Vec::new()));
        let received_for_handler = Rc::clone(&received);
        receiver.set_property(
            "onmessage",
            Value::function(move |_, args| {
                received_for_handler.borrow_mut().push(
                    args[0]
                        .get_property("data")
                        .get_property("value")
                        .to_js_string(),
                );
                Value::Undefined
            }),
        );
        let other_fired = Rc::new(Cell::new(false));
        let other_fired_for_handler = Rc::clone(&other_fired);
        other.set_property(
            "onmessage",
            Value::function(move |_, _| {
                other_fired_for_handler.set(true);
                Value::Undefined
            }),
        );

        let message = Value::object(HashMap::from([("value".into(), Value::string("snapshot"))]));
        sender.call_method("postMessage", vec![message.clone()]);
        message.set_property("value", Value::string("mutated"));
        assert!(
            received.borrow().is_empty(),
            "delivery must be asynchronous"
        );
        crate::jsdom::drain_microtasks();
        assert_eq!(received.borrow().as_slice(), &["snapshot"]);
        assert!(!other_fired.get());
        assert_eq!(sender.get_property("name"), Value::string("runtime-test"));
        assert!(w3cos_core::class::instance_of(
            &receiver,
            &broadcast_channel_class()
        ));

        receiver.call_method("close", vec![]);
        sender.call_method(
            "postMessage",
            vec![Value::object(HashMap::from([(
                "value".into(),
                Value::string("closed"),
            )]))],
        );
        crate::jsdom::drain_microtasks();
        assert_eq!(received.borrow().as_slice(), &["snapshot"]);

        sender.call_method("close", vec![]);
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            sender.call_method("postMessage", vec![Value::string("invalid")])
        }));
        let error = outcome
            .expect_err("posting to a closed BroadcastChannel must throw")
            .downcast::<w3cos_core::PanicValue>()
            .expect("exception should contain a JavaScript value");
        assert_eq!(
            error.0.get_property("name").to_js_string(),
            "InvalidStateError"
        );
        other.call_method("close", vec![]);
    }

    #[test]
    fn message_port_queues_starts_clones_and_closes() {
        let channel = channel_value();
        let port1 = channel.get_property("port1");
        let port2 = channel.get_property("port2");
        let received = Rc::new(RefCell::new(Vec::new()));

        let first = Value::object(HashMap::from([(
            "value".to_string(),
            Value::string("queued"),
        )]));
        port1.call_method("postMessage", vec![first.clone()]);
        first.set_property("value", Value::string("mutated"));
        assert!(received.borrow().is_empty());

        let received_for_handler = Rc::clone(&received);
        port2.set_property(
            "onmessage",
            Value::function(move |_, args| {
                received_for_handler.borrow_mut().push(
                    args[0]
                        .get_property("data")
                        .get_property("value")
                        .to_js_string(),
                );
                Value::Undefined
            }),
        );
        assert_eq!(received.borrow().as_slice(), &["queued"]);

        port1.call_method(
            "postMessage",
            vec![Value::object(HashMap::from([(
                "value".to_string(),
                Value::string("live"),
            )]))],
        );
        assert_eq!(received.borrow().as_slice(), &["queued", "live"]);

        port2.call_method("close", vec![]);
        port1.call_method(
            "postMessage",
            vec![Value::object(HashMap::from([(
                "value".to_string(),
                Value::string("closed"),
            )]))],
        );
        assert_eq!(received.borrow().as_slice(), &["queued", "live"]);
    }

    #[test]
    fn message_port_start_flushes_listener_queue() {
        let channel = channel_value();
        let port1 = channel.get_property("port1");
        let port2 = channel.get_property("port2");
        let received = Rc::new(RefCell::new(String::new()));
        let received_for_listener = Rc::clone(&received);
        port2.call_method(
            "addEventListener",
            vec![
                Value::string("message"),
                Value::function(move |_, args| {
                    *received_for_listener.borrow_mut() =
                        args[0].get_property("data").to_js_string();
                    Value::Undefined
                }),
            ],
        );
        port1.call_method("postMessage", vec![Value::string("queued")]);
        assert!(received.borrow().is_empty());
        port2.call_method("start", vec![]);
        assert_eq!(&*received.borrow(), "queued");
    }

    #[test]
    fn message_port_uses_structured_clone_cycles_and_errors() {
        let channel = channel_value();
        let port1 = channel.get_property("port1");
        let port2 = channel.get_property("port2");
        let received = Rc::new(RefCell::new(Value::Undefined));
        let received_for_handler = Rc::clone(&received);
        port2.set_property(
            "onmessage",
            Value::function(move |_, args| {
                *received_for_handler.borrow_mut() = args[0].get_property("data");
                Value::Undefined
            }),
        );

        let cyclic = Value::object(HashMap::new());
        cyclic.set_property("self", cyclic.clone());
        port1.call_method("postMessage", vec![cyclic]);
        let cloned = received.borrow().clone();
        assert_eq!(cloned.get_property("self"), cloned);

        let buffer = w3cos_core::class::construct(
            &w3cos_core::binary::array_buffer_class(),
            vec![Value::Number(2.0)],
        );
        let bytes = w3cos_core::class::construct(
            &w3cos_core::binary::typed_array_class("Uint8Array"),
            vec![buffer.clone()],
        );
        bytes.set_property("0", Value::Number(23.0));
        port1.call_method(
            "postMessage",
            vec![buffer.clone(), Value::array(vec![buffer.clone()])],
        );
        assert_eq!(buffer.get_property("byteLength").to_number(), 0.0);
        let received_buffer = received.borrow().clone();
        let received_bytes = w3cos_core::class::construct(
            &w3cos_core::binary::typed_array_class("Uint8Array"),
            vec![received_buffer],
        );
        assert_eq!(received_bytes.get_property("0").to_number(), 23.0);

        let outcome = catch_unwind(AssertUnwindSafe(|| {
            port1.call_method(
                "postMessage",
                vec![Value::function(|_, _| Value::Undefined)],
            )
        }));
        assert!(outcome.is_err(), "functions must raise DataCloneError");
    }

    #[test]
    fn message_port_transfer_moves_entanglement_and_detaches_the_source_wrapper() {
        let delivery = channel_value();
        let delivery_sender = delivery.get_property("port1");
        let delivery_receiver = delivery.get_property("port2");
        let transferred = Rc::new(RefCell::new(Value::Undefined));
        let transferred_for_handler = Rc::clone(&transferred);
        delivery_receiver.set_property(
            "onmessage",
            Value::function(move |_, args| {
                *transferred_for_handler.borrow_mut() =
                    args[0].get_property("data").get_property("port");
                Value::Undefined
            }),
        );

        let channel = channel_value();
        let peer = channel.get_property("port1");
        let source = channel.get_property("port2");
        delivery_sender.call_method(
            "postMessage",
            vec![
                Value::object(HashMap::from([("port".into(), source.clone())])),
                Value::array(vec![source.clone()]),
            ],
        );
        let moved = transferred.borrow().clone();
        assert!(w3cos_core::class::instance_of(
            &moved,
            &message_port_class()
        ));
        assert_ne!(moved, source);

        let received = Rc::new(RefCell::new(Vec::new()));
        let received_from_peer = Rc::clone(&received);
        moved.set_property(
            "onmessage",
            Value::function(move |_, args| {
                received_from_peer
                    .borrow_mut()
                    .push(args[0].get_property("data").to_js_string());
                Value::Undefined
            }),
        );
        peer.call_method("postMessage", vec![Value::string("to-moved")]);
        assert_eq!(received.borrow().as_slice(), &["to-moved"]);

        let received_by_peer = Rc::clone(&received);
        peer.set_property(
            "onmessage",
            Value::function(move |_, args| {
                received_by_peer
                    .borrow_mut()
                    .push(args[0].get_property("data").to_js_string());
                Value::Undefined
            }),
        );
        moved.call_method("postMessage", vec![Value::string("from-moved")]);
        source.call_method("postMessage", vec![Value::string("from-detached")]);
        assert_eq!(
            received.borrow().as_slice(),
            &["to-moved", "from-moved"],
            "the transferred wrapper remains entangled while the source is inert"
        );

        let duplicate = catch_unwind(AssertUnwindSafe(|| {
            delivery_sender.call_method(
                "postMessage",
                vec![
                    Value::Null,
                    Value::array(vec![moved.clone(), moved.clone()]),
                ],
            )
        }));
        assert!(duplicate.is_err(), "duplicate transfer entries must fail");
    }

    #[test]
    fn worker_echo_crosses_the_thread_with_structured_clone_types_and_cycles() {
        let worker = w3cos_core::class::construct(&worker_class(), vec![Value::string("echo")]);
        let received = Rc::new(RefCell::new(Value::Undefined));
        let received_for_handler = Rc::clone(&received);
        worker.set_property(
            "onmessage",
            Value::function(move |_, args| {
                *received_for_handler.borrow_mut() = args[0].get_property("data");
                Value::Undefined
            }),
        );

        let cyclic = Value::object(HashMap::new());
        cyclic.set_property("self", cyclic.clone());
        let map = w3cos_core::class::construct(&w3cos_core::collections::map_class(), vec![]);
        map.call_method("set", vec![Value::string("cyclic"), cyclic.clone()]);
        let error = w3cos_core::class::construct(
            &w3cos_core::error_class("TypeError"),
            vec![Value::string("worker")],
        );
        error.set_property("cause", error.clone());
        let blob = w3cos_core::class::construct(
            &w3cos_core::web::blob_class(),
            vec![Value::array(vec![Value::string("bytes")])],
        );
        let buffer = w3cos_core::class::construct(
            &w3cos_core::binary::array_buffer_class(),
            vec![Value::Number(12.0)],
        );
        let words = w3cos_core::class::construct(
            &w3cos_core::binary::typed_array_class("Uint16Array"),
            vec![buffer.clone(), Value::Number(2.0), Value::Number(3.0)],
        );
        words.set_property("0", Value::Number(0x1234 as f64));
        let data_view = w3cos_core::class::construct(
            &w3cos_core::binary::data_view_class(),
            vec![buffer.clone(), Value::Number(2.0), Value::Number(6.0)],
        );
        let shared_buffer = w3cos_core::class::construct(
            &w3cos_core::binary::shared_array_buffer_class(),
            vec![Value::Number(8.0)],
        );
        let shared_words = w3cos_core::class::construct(
            &w3cos_core::binary::typed_array_class("Int32Array"),
            vec![shared_buffer.clone()],
        );
        shared_words.set_property("0", Value::Number(41.0));
        worker.call_method(
            "postMessage",
            vec![Value::object(HashMap::from([
                ("cyclic".into(), cyclic),
                ("map".into(), map),
                ("error".into(), error),
                ("blob".into(), blob),
                ("buffer".into(), buffer),
                ("words".into(), words),
                ("dataView".into(), data_view),
                ("sharedBuffer".into(), shared_buffer),
                ("sharedWords".into(), shared_words),
            ]))],
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while received.borrow().is_undefined() && std::time::Instant::now() < deadline {
            poll_js_events();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let cloned = received.borrow().clone();
        assert!(!cloned.is_undefined(), "worker echo should arrive");
        assert_eq!(
            cloned.get_property("cyclic").get_property("self"),
            cloned.get_property("cyclic")
        );
        assert_eq!(
            cloned
                .get_property("map")
                .call_method("get", vec![Value::string("cyclic")]),
            cloned.get_property("cyclic")
        );
        let cloned_error = cloned.get_property("error");
        assert!(w3cos_core::class::instance_of(
            &cloned_error,
            &w3cos_core::error_class("TypeError")
        ));
        assert_eq!(cloned_error.get_property("cause"), cloned_error);
        assert_eq!(
            cloned
                .get_property("blob")
                .call_method("text", vec![])
                .to_js_string(),
            "bytes"
        );
        let cloned_buffer = cloned.get_property("buffer");
        let cloned_words = cloned.get_property("words");
        let cloned_data_view = cloned.get_property("dataView");
        assert!(w3cos_core::class::instance_of(
            &cloned_buffer,
            &w3cos_core::binary::array_buffer_class()
        ));
        assert!(w3cos_core::class::instance_of(
            &cloned_words,
            &w3cos_core::binary::typed_array_class("Uint16Array")
        ));
        assert!(w3cos_core::class::instance_of(
            &cloned_data_view,
            &w3cos_core::binary::data_view_class()
        ));
        assert_eq!(cloned_words.get_property("buffer"), cloned_buffer);
        assert_eq!(cloned_data_view.get_property("buffer"), cloned_buffer);
        assert_eq!(cloned_words.get_property("byteOffset").to_number(), 2.0);
        assert_eq!(cloned_words.get_property("length").to_number(), 3.0);
        assert_eq!(cloned_words.get_property("0").to_number(), 0x1234 as f64);
        let cloned_shared_buffer = cloned.get_property("sharedBuffer");
        let cloned_shared_words = cloned.get_property("sharedWords");
        assert!(w3cos_core::class::instance_of(
            &cloned_shared_buffer,
            &w3cos_core::binary::shared_array_buffer_class()
        ));
        assert_eq!(
            cloned_shared_words.get_property("buffer"),
            cloned_shared_buffer
        );
        assert_eq!(cloned_shared_words.get_property("0").to_number(), 41.0);

        let channel = channel_value();
        let source = channel.get_property("port1");
        let rejected_port = channel.get_property("port2");
        let rejected = catch_unwind(AssertUnwindSafe(|| {
            worker.call_method(
                "postMessage",
                vec![
                    Value::object(HashMap::from([("port".into(), rejected_port.clone())])),
                    Value::array(vec![rejected_port.clone()]),
                ],
            )
        }));
        assert!(rejected.is_err());
        let still_entangled = Rc::new(Cell::new(false));
        let still_entangled_for_handler = Rc::clone(&still_entangled);
        rejected_port.set_property(
            "onmessage",
            Value::function(move |_, _| {
                still_entangled_for_handler.set(true);
                Value::Undefined
            }),
        );
        source.call_method("postMessage", vec![Value::string("still-live")]);
        assert!(
            still_entangled.get(),
            "unsupported cross-thread port transfer must not detach the source"
        );
        worker.call_method("terminate", vec![]);
    }

    #[test]
    fn workers_ports_and_broadcast_deliveries_are_realm_owned() {
        reset_realm();
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let old_worker_class = worker_class();
        let old_shared_worker_class = shared_worker_class();
        let old_port_class = message_port_class();
        let old_channel_class = message_channel_class();
        let old_broadcast_class = broadcast_channel_class();
        let worker =
            w3cos_core::class::construct(&old_worker_class, vec![Value::string("echo.js")]);
        let channel = w3cos_core::class::construct(&old_channel_class, vec![]);
        let port1 = channel.get_property("port1");
        let port2 = channel.get_property("port2");
        let sender =
            w3cos_core::class::construct(&old_broadcast_class, vec![Value::string("old-page")]);
        let receiver =
            w3cos_core::class::construct(&old_broadcast_class, vec![Value::string("old-page")]);
        let deliveries = Rc::new(Cell::new(0_u32));
        let deliveries_for_receiver = Rc::clone(&deliveries);
        receiver.set_property(
            "onmessage",
            Value::function(move |_, _| {
                deliveries_for_receiver.set(deliveries_for_receiver.get() + 1);
                Value::Undefined
            }),
        );
        sender.call_method("postMessage", vec![Value::string("queued")]);

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        assert_eq!(crate::jsdom::drain_microtasks(), 0);
        assert_eq!(poll_js_events(), 0);
        assert_eq!(deliveries.get(), 0);
        assert!(!old_worker_class.strict_eq(&worker_class()));
        assert!(!old_shared_worker_class.strict_eq(&shared_worker_class()));
        assert!(!old_port_class.strict_eq(&message_port_class()));
        assert!(!old_channel_class.strict_eq(&message_channel_class()));
        assert!(!old_broadcast_class.strict_eq(&broadcast_channel_class()));
        assert!(
            old_worker_class
                .call(Value::Undefined, vec![])
                .is_undefined()
        );
        assert!(worker.call_method("postMessage", vec![]).is_undefined());
        assert!(port1.call_method("postMessage", vec![]).is_undefined());
        assert!(port2.call_method("start", vec![]).is_undefined());
        assert!(sender.call_method("postMessage", vec![]).is_undefined());
        assert!(receiver.call_method("close", vec![]).is_undefined());
        let current_channel = w3cos_core::class::construct(&message_channel_class(), vec![]);
        assert!(
            current_channel
                .get_property("port1")
                .get_property("postMessage")
                .is_function()
        );
        reset_realm();
    }
}
