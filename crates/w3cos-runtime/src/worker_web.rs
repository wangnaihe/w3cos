//! JavaScript facades over the native worker and message-channel engines.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use w3cos_core::Value;

use crate::worker::{Worker, WorkerEvent, WorkerOptions};

#[derive(Clone)]
struct JsWorker {
    native: Rc<RefCell<Option<Worker>>>,
    value: Value,
}

thread_local! {
    static WORKERS: RefCell<Vec<JsWorker>> = const { RefCell::new(Vec::new()) };
    static WORKER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static SHARED_WORKER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static MESSAGE_PORT_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static MESSAGE_CHANNEL_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
}

fn core_to_json(value: Value) -> serde_json::Value {
    let text = w3cos_core::json::stringify(vec![value]).to_js_string();
    serde_json::from_str(&text).unwrap_or(serde_json::Value::Null)
}

fn json_to_core(value: serde_json::Value) -> Value {
    let text = serde_json::to_string(&value).unwrap_or_else(|_| "null".to_string());
    w3cos_core::json::parse(vec![Value::string(&text)])
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

pub fn worker_class() -> Value {
    WORKER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, args| {
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
                Value::function(move |_, args| {
                    if let Some(worker) = native_for_post.borrow().as_ref() {
                        let _ = worker
                            .post_message(core_to_json(args.first().cloned().unwrap_or_default()));
                    }
                    Value::Undefined
                }),
            );
            let native_for_terminate = Rc::clone(&native);
            value.set_property(
                "terminate",
                Value::function(move |_, _| {
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

fn message_port_value() -> Value {
    let value = Value::object(HashMap::from([
        ("onmessage".to_string(), Value::Null),
        ("onmessageerror".to_string(), Value::Null),
    ]));
    crate::web_events::event_target_class().call(value.clone(), vec![]);
    value.set_property("start", Value::function(|_, _| Value::Undefined));
    value.set_property("close", Value::function(|_, _| Value::Undefined));
    w3cos_core::class::set_prototype_of(&value, &message_port_class().get_property("prototype"));
    value
}

pub fn message_port_class() -> Value {
    MESSAGE_PORT_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| message_port_value());
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
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
    let port1 = message_port_value();
    let port2 = message_port_value();
    let target2 = port2.clone();
    port1.set_property(
        "postMessage",
        Value::function(move |_, args| {
            target2.call_method(
                "dispatchEvent",
                vec![event_with_data(
                    "message",
                    args.first().cloned().unwrap_or_default(),
                )],
            );
            Value::Undefined
        }),
    );
    let target1 = port1.clone();
    port2.set_property(
        "postMessage",
        Value::function(move |_, args| {
            target1.call_method(
                "dispatchEvent",
                vec![event_with_data(
                    "message",
                    args.first().cloned().unwrap_or_default(),
                )],
            );
            Value::Undefined
        }),
    );
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
        let class = Value::function(|_, _| channel_value());
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
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
        let class = Value::function(|_, _| {
            let channel = channel_value();
            let public_port = channel.get_property("port1");
            let worker_port = channel.get_property("port2");
            let echo_port = worker_port.clone();
            worker_port.set_property(
                "onmessage",
                Value::function(move |_, args| {
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
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}
