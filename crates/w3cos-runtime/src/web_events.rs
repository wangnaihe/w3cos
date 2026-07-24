//! Value-level implementations of the core Web Events constructors.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use w3cos_core::Value;

thread_local! {
    static EVENT_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static CUSTOM_EVENT_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static EVENT_TARGET_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct Listener {
    type_name: String,
    callback: Value,
    capture: bool,
    once: bool,
}

fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or(Value::Undefined)
}

fn bool_option(options: &Value, name: &str) -> bool {
    match options {
        Value::Bool(value) if name == "capture" => *value,
        Value::Object(_) => options.get_property(name).to_bool(),
        _ => false,
    }
}

fn listener_is_callable(listener: &Value) -> bool {
    listener.is_function() || listener.get_property("handleEvent").is_function()
}

fn invoke_listener(listener: &Value, target: &Value, event: Value) {
    if listener.is_function() {
        listener.call(target.clone(), vec![event]);
    } else {
        listener.call_method("handleEvent", vec![event]);
    }
}

fn install_event(this: &Value, args: &[Value], custom: bool) {
    let type_name = arg(args, 0).to_js_string();
    let init = arg(args, 1);
    this.set_property("type", Value::string(&type_name));
    this.set_property("bubbles", Value::Bool(bool_option(&init, "bubbles")));
    this.set_property("cancelable", Value::Bool(bool_option(&init, "cancelable")));
    this.set_property("composed", Value::Bool(bool_option(&init, "composed")));
    this.set_property("target", Value::Null);
    this.set_property("currentTarget", Value::Null);
    this.set_property("srcElement", Value::Null);
    this.set_property("relatedTarget", Value::Null);
    this.set_property("eventPhase", Value::Number(0.0));
    this.set_property(
        "timeStamp",
        Value::Number(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64()
                * 1000.0,
        ),
    );
    this.set_property("isTrusted", Value::Bool(false));
    this.set_property("__pd", Value::Bool(false));
    this.set_property("__sp", Value::Bool(false));
    this.set_property("__sip", Value::Bool(false));
    this.set_property("returnValue", Value::Bool(true));
    if custom {
        let detail = if init.is_object() {
            init.get_property("detail")
        } else {
            Value::Null
        };
        this.set_property("detail", detail);
    }

    for (name, value) in [
        ("NONE", 0.0),
        ("CAPTURING_PHASE", 1.0),
        ("AT_TARGET", 2.0),
        ("BUBBLING_PHASE", 3.0),
    ] {
        this.set_property(name, Value::Number(value));
    }

    this.set_property(
        "preventDefault",
        Value::function(|this, _| {
            if this.get_property("cancelable").to_bool() {
                this.set_property("__pd", Value::Bool(true));
                this.set_property("returnValue", Value::Bool(false));
            }
            Value::Undefined
        }),
    );
    this.set_property(
        "stopPropagation",
        Value::function(|this, _| {
            this.set_property("__sp", Value::Bool(true));
            Value::Undefined
        }),
    );
    this.set_property(
        "stopImmediatePropagation",
        Value::function(|this, _| {
            this.set_property("__sp", Value::Bool(true));
            this.set_property("__sip", Value::Bool(true));
            Value::Undefined
        }),
    );
    this.set_property(
        "composedPath",
        Value::function(|this, _| {
            let target = this.get_property("target");
            if target.is_nullish() {
                Value::array(vec![])
            } else {
                Value::array(vec![target])
            }
        }),
    );
    this.set_property(
        "initEvent",
        Value::function(|this, args| {
            this.set_property("type", Value::string(&arg(&args, 0).to_js_string()));
            this.set_property("bubbles", Value::Bool(arg(&args, 1).to_bool()));
            this.set_property("cancelable", Value::Bool(arg(&args, 2).to_bool()));
            this.set_property("__pd", Value::Bool(false));
            this.set_property("__sp", Value::Bool(false));
            this.set_property("__sip", Value::Bool(false));
            this.set_property("returnValue", Value::Bool(true));
            Value::Undefined
        }),
    );
    this.set_property(
        "__w3cos_getter_defaultPrevented",
        Value::function(|this, _| this.get_property("__pd")),
    );
    this.set_property(
        "__w3cos_getter_cancelBubble",
        Value::function(|this, _| this.get_property("__sp")),
    );
    this.set_property(
        "__w3cos_setter_cancelBubble",
        Value::function(|this, args| {
            if arg(&args, 0).to_bool() {
                this.set_property("__sp", Value::Bool(true));
            }
            Value::Undefined
        }),
    );
}

fn make_event_constructor(custom: bool) -> Value {
    let constructor = Value::function(move |this, args| {
        install_event(&this, &args, custom);
        if custom {
            this.set_property(
                "initCustomEvent",
                Value::function(|this, args| {
                    this.call_method("initEvent", args.iter().take(3).cloned().collect());
                    this.set_property("detail", arg(&args, 3));
                    Value::Undefined
                }),
            );
        }
        Value::Undefined
    });
    let prototype = Value::object(HashMap::new());
    prototype.set_property("constructor", constructor.clone());
    if custom {
        w3cos_core::class::set_prototype_of(&prototype, &event_class().get_property("prototype"));
    }
    constructor.set_property("prototype", prototype);
    for (name, value) in [
        ("NONE", 0.0),
        ("CAPTURING_PHASE", 1.0),
        ("AT_TARGET", 2.0),
        ("BUBBLING_PHASE", 3.0),
    ] {
        constructor.set_property(name, Value::Number(value));
    }
    constructor
}

pub fn event_class() -> Value {
    EVENT_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = make_event_constructor(false);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn custom_event_class() -> Value {
    CUSTOM_EVENT_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = make_event_constructor(true);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn event_target_class() -> Value {
    EVENT_TARGET_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = make_event_target_class();
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn make_event_target_class() -> Value {
    let constructor = Value::function(|this, _| {
        let listeners: Rc<RefCell<Vec<Listener>>> = Rc::new(RefCell::new(Vec::new()));

        let state = listeners.clone();
        this.set_property(
            "addEventListener",
            Value::function(move |_, args| {
                let type_name = arg(&args, 0).to_js_string();
                let callback = arg(&args, 1);
                let options = arg(&args, 2);
                if type_name.is_empty() || !listener_is_callable(&callback) {
                    return Value::Undefined;
                }
                let capture = bool_option(&options, "capture");
                let mut listeners = state.borrow_mut();
                if !listeners.iter().any(|listener| {
                    listener.type_name == type_name
                        && listener.capture == capture
                        && listener.callback.strict_eq(&callback)
                }) {
                    listeners.push(Listener {
                        type_name,
                        callback,
                        capture,
                        once: bool_option(&options, "once"),
                    });
                }
                Value::Undefined
            }),
        );

        let state = listeners.clone();
        this.set_property(
            "removeEventListener",
            Value::function(move |_, args| {
                let type_name = arg(&args, 0).to_js_string();
                let callback = arg(&args, 1);
                let capture = bool_option(&arg(&args, 2), "capture");
                state.borrow_mut().retain(|listener| {
                    listener.type_name != type_name
                        || listener.capture != capture
                        || !listener.callback.strict_eq(&callback)
                });
                Value::Undefined
            }),
        );

        let state = listeners;
        this.set_property(
            "dispatchEvent",
            Value::function(move |this, args| {
                let event = arg(&args, 0);
                let type_name = event.get_property("type").to_js_string();
                if type_name.is_empty() || type_name == "undefined" {
                    return Value::Bool(true);
                }

                event.set_property("target", this.clone());
                event.set_property("srcElement", this.clone());
                event.set_property("currentTarget", this.clone());
                event.set_property("eventPhase", Value::Number(2.0));
                event.set_property("__sp", Value::Bool(false));
                event.set_property("__sip", Value::Bool(false));

                let property_handler = this.get_property(&format!("on{type_name}"));
                if listener_is_callable(&property_handler) {
                    invoke_listener(&property_handler, &this, event.clone());
                }

                let snapshot = state.borrow().clone();
                for listener in snapshot
                    .iter()
                    .filter(|listener| listener.type_name == type_name)
                {
                    if event.get_property("__sip").to_bool() {
                        break;
                    }
                    if listener.once {
                        state.borrow_mut().retain(|registered| {
                            registered.type_name != listener.type_name
                                || registered.capture != listener.capture
                                || !registered.callback.strict_eq(&listener.callback)
                        });
                    }
                    invoke_listener(&listener.callback, &this, event.clone());
                }

                event.set_property("currentTarget", Value::Null);
                event.set_property("eventPhase", Value::Number(0.0));
                Value::Bool(!event.get_property("__pd").to_bool())
            }),
        );
        Value::Undefined
    });
    let prototype = Value::object(HashMap::new());
    prototype.set_property("constructor", constructor.clone());
    constructor.set_property("prototype", prototype);
    constructor
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn event_target_dispatches_cancelable_custom_events_and_once_listeners() {
        let target_class = event_target_class();
        let target = w3cos_core::class::construct(&target_class, vec![]);
        let custom_event_class = custom_event_class();
        let event = w3cos_core::class::construct(
            &custom_event_class,
            vec![
                Value::string("ready"),
                Value::object(HashMap::from([
                    ("detail".to_string(), Value::string("payload")),
                    ("cancelable".to_string(), Value::Bool(true)),
                ])),
            ],
        );

        let calls = Rc::new(Cell::new(0));
        let calls_for_listener = calls.clone();
        let target_for_listener = target.clone();
        target.call_method(
            "addEventListener",
            vec![
                Value::string("ready"),
                Value::function(move |this, args| {
                    calls_for_listener.set(calls_for_listener.get() + 1);
                    assert!(this.strict_eq(&target_for_listener));
                    assert_eq!(
                        arg(&args, 0).get_property("detail").to_js_string(),
                        "payload"
                    );
                    arg(&args, 0).call_method("preventDefault", vec![]);
                    Value::Undefined
                }),
                Value::object(HashMap::from([("once".to_string(), Value::Bool(true))])),
            ],
        );

        assert!(
            !target
                .call_method("dispatchEvent", vec![event.clone()])
                .to_bool()
        );
        assert!(event.get_property("defaultPrevented").to_bool());
        assert!(event.get_property("target").strict_eq(&target));
        assert_eq!(calls.get(), 1);
        assert!(!target.call_method("dispatchEvent", vec![event]).to_bool());
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn remove_event_listener_uses_callback_identity() {
        let target = w3cos_core::class::construct(&event_target_class(), vec![]);
        let calls = Rc::new(Cell::new(0));
        let calls_for_listener = calls.clone();
        let listener = Value::function(move |_, _| {
            calls_for_listener.set(calls_for_listener.get() + 1);
            Value::Undefined
        });
        target.call_method(
            "addEventListener",
            vec![Value::string("tick"), listener.clone()],
        );
        target.call_method("removeEventListener", vec![Value::string("tick"), listener]);
        let event = w3cos_core::class::construct(&event_class(), vec![Value::string("tick")]);
        assert!(target.call_method("dispatchEvent", vec![event]).to_bool());
        assert_eq!(calls.get(), 0);
    }
}
