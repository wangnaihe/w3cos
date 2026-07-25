//! Chromium Navigation API compatibility over the existing session history.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::Once;

use w3cos_core::Value;

#[derive(Clone)]
struct Entry {
    id: String,
    key: String,
    url: String,
    state: Value,
}

struct NavigationState {
    entries: Vec<Entry>,
    index: usize,
}

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static NAVIGATION: RefCell<Option<Value>> = const { RefCell::new(None) };
    static STATE: RefCell<NavigationState> = RefCell::new(NavigationState {
        entries: vec![Entry {
            id: String::new(),
            key: String::new(),
            url: String::new(),
            state: Value::Undefined,
        }],
        index: 0,
    });
    static NEXT_KEY: Cell<u64> = const { Cell::new(1) };
}

static HISTORY_SYNC_WARNING: Once = Once::new();

fn promise(value: Value) -> Value {
    w3cos_core::promise::resolve(vec![value])
}

fn error(name: &str, message: &str) -> Value {
    w3cos_core::class::construct(
        &crate::unsupported::dom_exception_class(),
        vec![Value::string(message), Value::string(name)],
    )
}

fn result(committed: Value, finished: Value) -> Value {
    Value::object(HashMap::from([
        ("committed".into(), committed),
        ("finished".into(), finished),
    ]))
}

fn rejected_result(name: &str, message: &str) -> Value {
    let error = error(name, message);
    result(
        w3cos_core::promise::reject(vec![error.clone()]),
        w3cos_core::promise::reject(vec![error]),
    )
}

fn next_identity() -> (String, String) {
    NEXT_KEY.with(|next| {
        let value = next.get();
        next.set(value.saturating_add(1));
        (
            format!("w3cos-navigation-{value}"),
            format!("entry-{value}"),
        )
    })
}

fn ensure_initial_state() {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        if state.entries[0].id.is_empty() {
            let (id, key) = next_identity();
            state.entries[0].id = id;
            state.entries[0].key = key;
            state.entries[0].url = crate::history::get_href();
            state.entries[0].state = crate::history::get_state()
                .map(|value| Value::string(&value))
                .unwrap_or(Value::Undefined);
        } else if state.entries.len() == 1 && state.entries[0].url != crate::history::get_href() {
            HISTORY_SYNC_WARNING.call_once(|| {
                eprintln!(
                    "[w3cos] warning: Navigation API tracks entries created through `navigation`; \
                     direct History/location writes update current URL but do not preserve \
                     Navigation entry keys until unified session-history storage lands"
                );
            });
            state.entries[0].url = crate::history::get_href();
        }
    });
}

fn class(name: &'static str, event: bool) -> Value {
    CLASSES.with(|classes| {
        if let Some(value) = classes.borrow().get(name).cloned() {
            return value;
        }
        let constructor = if event {
            Value::function(move |this, args| {
                crate::web_events::event_class().call(this.clone(), args.clone());
                let init = args.get(1).cloned().unwrap_or(Value::Undefined);
                for property in match name {
                    "NavigateEvent" => vec![
                        "navigationType",
                        "destination",
                        "canIntercept",
                        "userInitiated",
                        "hashChange",
                        "signal",
                        "formData",
                        "downloadRequest",
                        "info",
                        "hasUAVisualTransition",
                        "sourceElement",
                    ],
                    _ => vec!["navigationType", "from"],
                } {
                    let value = init.get_property(property);
                    this.set_property(
                        property,
                        if value.is_undefined() {
                            match property {
                                "canIntercept"
                                | "userInitiated"
                                | "hashChange"
                                | "hasUAVisualTransition" => Value::Bool(false),
                                "navigationType" => Value::string("push"),
                                _ => Value::Null,
                            }
                        } else {
                            value
                        },
                    );
                }
                if name == "NavigateEvent" {
                    this.set_property(
                        "intercept",
                        Value::function(|this, args| {
                            let options = args.first().cloned().unwrap_or(Value::Undefined);
                            let handler = options.get_property("handler");
                            if handler.is_function() {
                                handler.call(Value::Undefined, vec![]);
                            }
                            this.set_property("__w3cos_intercepted", Value::Bool(true));
                            Value::Undefined
                        }),
                    );
                    this.set_property("scroll", Value::function(|_, _| Value::Undefined));
                }
                Value::Undefined
            })
        } else {
            Value::function(move |_, _| {
                w3cos_core::throw_value(error("TypeError", &format!("Illegal constructor: {name}")))
            })
        };
        constructor.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::from([("constructor".into(), constructor.clone())]));
        if event {
            w3cos_core::class::set_prototype_of(
                &prototype,
                &crate::web_events::event_class().get_property("prototype"),
            );
            for property in if name == "NavigateEvent" {
                vec![
                    "navigationType",
                    "destination",
                    "canIntercept",
                    "userInitiated",
                    "hashChange",
                    "signal",
                    "formData",
                    "downloadRequest",
                    "info",
                    "hasUAVisualTransition",
                    "sourceElement",
                ]
            } else {
                vec!["navigationType", "from"]
            } {
                prototype.set_property(property, Value::Undefined);
            }
            if name == "NavigateEvent" {
                prototype.set_property("intercept", Value::function(|_, _| Value::Undefined));
                prototype.set_property("scroll", Value::function(|_, _| Value::Undefined));
            }
        }
        constructor.set_property("prototype", prototype);
        classes
            .borrow_mut()
            .insert(name.to_string(), constructor.clone());
        constructor
    })
}

pub fn navigation_class() -> Value {
    let class = class("Navigation", false);
    for property in [
        "activation",
        "canGoBack",
        "canGoForward",
        "currentEntry",
        "transition",
    ] {
        class
            .get_property("prototype")
            .set_property(property, Value::Undefined);
    }
    class
}
pub fn history_entry_class() -> Value {
    let class = class("NavigationHistoryEntry", false);
    for property in [
        "getState",
        "id",
        "index",
        "key",
        "ondispose",
        "sameDocument",
        "url",
    ] {
        class
            .get_property("prototype")
            .set_property(property, Value::Undefined);
    }
    class
}
pub fn destination_class() -> Value {
    let class = class("NavigationDestination", false);
    let prototype = class.get_property("prototype");
    for name in ["id", "key", "url", "index", "sameDocument", "getState"] {
        prototype.set_property(name, Value::Undefined);
    }
    class
}
pub fn navigate_event_class() -> Value {
    class("NavigateEvent", true)
}
pub fn current_entry_change_event_class() -> Value {
    class("NavigationCurrentEntryChangeEvent", true)
}
pub fn transition_class() -> Value {
    let class = class("NavigationTransition", false);
    for property in ["committed", "finished", "from", "navigationType", "to"] {
        class
            .get_property("prototype")
            .set_property(property, Value::Undefined);
    }
    class
}
pub fn activation_class() -> Value {
    let class = class("NavigationActivation", false);
    for property in ["entry", "from", "navigationType"] {
        class
            .get_property("prototype")
            .set_property(property, Value::Undefined);
    }
    class
}

fn entry_value(entry: &Entry, index: usize) -> Value {
    let value = Value::object(HashMap::from([
        ("id".into(), Value::string(&entry.id)),
        ("key".into(), Value::string(&entry.key)),
        ("url".into(), Value::string(&entry.url)),
        ("index".into(), Value::Number(index as f64)),
        ("sameDocument".into(), Value::Bool(true)),
        ("ondispose".into(), Value::Null),
        ("__w3cos_state".into(), entry.state.clone()),
        (
            "getState".into(),
            Value::function(|this, _| this.get_property("__w3cos_state")),
        ),
    ]));
    w3cos_core::class::set_prototype_of(&value, &history_entry_class().get_property("prototype"));
    let prototype = history_entry_class().get_property("prototype");
    for name in [
        "id",
        "key",
        "url",
        "index",
        "sameDocument",
        "ondispose",
        "getState",
    ] {
        prototype.set_property(name, value.get_property(name));
    }
    value
}

fn current_entry() -> Value {
    ensure_initial_state();
    STATE.with(|state| {
        let state = state.borrow();
        entry_value(&state.entries[state.index], state.index)
    })
}

fn destination_value(url: &str, state: Value) -> Value {
    let destination = Value::object(HashMap::from([
        ("id".into(), Value::string("")),
        ("key".into(), Value::string("")),
        ("url".into(), Value::string(url)),
        ("index".into(), Value::Number(-1.0)),
        ("sameDocument".into(), Value::Bool(true)),
        ("__w3cos_state".into(), state),
        (
            "getState".into(),
            Value::function(|this, _| this.get_property("__w3cos_state")),
        ),
    ]));
    w3cos_core::class::set_prototype_of(
        &destination,
        &destination_class().get_property("prototype"),
    );
    destination
}

fn navigation_event(url: &str, navigation_type: &str, state: Value) -> Value {
    let destination = destination_value(url, state);
    w3cos_core::class::construct(
        &navigate_event_class(),
        vec![
            Value::string("navigate"),
            Value::object(HashMap::from([
                ("cancelable".into(), Value::Bool(true)),
                ("navigationType".into(), Value::string(navigation_type)),
                ("destination".into(), destination),
                ("canIntercept".into(), Value::Bool(true)),
            ])),
        ],
    )
}

fn changed_event(from: Value, navigation_type: &str) -> Value {
    w3cos_core::class::construct(
        &current_entry_change_event_class(),
        vec![
            Value::string("currententrychange"),
            Value::object(HashMap::from([
                ("from".into(), from),
                ("navigationType".into(), Value::string(navigation_type)),
            ])),
        ],
    )
}

fn navigate(this: &Value, args: &[Value]) -> Value {
    let input = args
        .first()
        .cloned()
        .unwrap_or(Value::Undefined)
        .to_js_string();
    let url = w3cos_core::web::url_new(vec![
        Value::string(&input),
        Value::string(&crate::history::get_href()),
    ])
    .get_property("href")
    .to_js_string();
    let options = args.get(1).cloned().unwrap_or(Value::Undefined);
    let history = options.get_property("history").to_js_string();
    let navigation_type = if history == "replace" {
        "replace"
    } else {
        "push"
    };
    let state_value = options.get_property("state");
    let event = navigation_event(&url, navigation_type, state_value.clone());
    if !this.call_method("dispatchEvent", vec![event]).to_bool() {
        return rejected_result("AbortError", "Navigation was canceled");
    }
    let from = current_entry();
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        if navigation_type == "replace" {
            let index = state.index;
            state.entries[index].url = url.clone();
            state.entries[index].state = state_value.clone();
            crate::history::replace_state(
                (!state_value.is_undefined())
                    .then(|| state_value.to_js_string())
                    .as_deref(),
                "",
                &url,
            );
        } else {
            let keep = state.index + 1;
            state.entries.truncate(keep);
            let (id, key) = next_identity();
            state.entries.push(Entry {
                id,
                key,
                url: url.clone(),
                state: state_value.clone(),
            });
            state.index = state.entries.len() - 1;
            crate::history::push_state(
                (!state_value.is_undefined())
                    .then(|| state_value.to_js_string())
                    .as_deref(),
                "",
                &url,
            );
        }
    });
    let entry = current_entry();
    this.call_method("dispatchEvent", vec![changed_event(from, navigation_type)]);
    this.call_method(
        "dispatchEvent",
        vec![w3cos_core::class::construct(
            &crate::web_events::event_class(),
            vec![Value::string("navigatesuccess")],
        )],
    );
    result(promise(entry.clone()), promise(entry))
}

fn traverse(this: &Value, target: usize, navigation_type: &str) -> Value {
    let from = current_entry();
    let found = STATE.with(|state| target < state.borrow().entries.len());
    if !found {
        return rejected_result("InvalidStateError", "Navigation entry is unavailable");
    }
    let delta = STATE.with(|state| target as i32 - state.borrow().index as i32);
    crate::history::go(delta);
    STATE.with(|state| state.borrow_mut().index = target);
    let entry = current_entry();
    this.call_method("dispatchEvent", vec![changed_event(from, navigation_type)]);
    result(promise(entry.clone()), promise(entry))
}

pub fn navigation_value() -> Value {
    NAVIGATION.with(|slot| {
        if let Some(value) = slot.borrow().clone() {
            return value;
        }
        ensure_initial_state();
        let navigation = Value::object(HashMap::new());
        crate::web_events::event_target_class().call(navigation.clone(), vec![]);
        for name in [
            "onnavigate",
            "onnavigatesuccess",
            "onnavigateerror",
            "oncurrententrychange",
        ] {
            navigation.set_property(name, Value::Null);
        }
        navigation.set_property(
            "__w3cos_getter_activation",
            Value::function(|_, _| Value::Null),
        );
        navigation.set_property(
            "__w3cos_getter_transition",
            Value::function(|_, _| Value::Null),
        );
        navigation.set_property(
            "__w3cos_getter_currentEntry",
            Value::function(|_, _| current_entry()),
        );
        navigation.set_property(
            "__w3cos_getter_canGoBack",
            Value::function(|_, _| STATE.with(|state| Value::Bool(state.borrow().index > 0))),
        );
        navigation.set_property(
            "__w3cos_getter_canGoForward",
            Value::function(|_, _| {
                STATE.with(|state| {
                    let state = state.borrow();
                    Value::Bool(state.index + 1 < state.entries.len())
                })
            }),
        );
        navigation.set_property(
            "entries",
            Value::function(|_, _| {
                STATE.with(|state| {
                    Value::array(
                        state
                            .borrow()
                            .entries
                            .iter()
                            .enumerate()
                            .map(|(index, entry)| entry_value(entry, index))
                            .collect(),
                    )
                })
            }),
        );
        navigation.set_property(
            "navigate",
            Value::function(|this, args| navigate(&this, &args)),
        );
        navigation.set_property(
            "reload",
            Value::function(|this, args| {
                let url = crate::history::get_href();
                let options = args.first().cloned().unwrap_or(Value::Undefined);
                navigate(
                    &this,
                    &[
                        Value::string(&url),
                        Value::object(HashMap::from([
                            ("history".into(), Value::string("replace")),
                            ("state".into(), options.get_property("state")),
                        ])),
                    ],
                )
            }),
        );
        navigation.set_property(
            "back",
            Value::function(|this, _| {
                let target = STATE.with(|state| state.borrow().index.checked_sub(1));
                target
                    .map(|target| traverse(&this, target, "traverse"))
                    .unwrap_or_else(|| {
                        rejected_result("InvalidStateError", "No previous navigation entry")
                    })
            }),
        );
        navigation.set_property(
            "forward",
            Value::function(|this, _| {
                let target = STATE.with(|state| state.borrow().index + 1);
                traverse(&this, target, "traverse")
            }),
        );
        navigation.set_property(
            "traverseTo",
            Value::function(|this, args| {
                let key = args
                    .first()
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .to_js_string();
                let target = STATE.with(|state| {
                    state
                        .borrow()
                        .entries
                        .iter()
                        .position(|entry| entry.key == key)
                });
                target
                    .map(|target| traverse(&this, target, "traverse"))
                    .unwrap_or_else(|| {
                        rejected_result("InvalidStateError", "Navigation key was not found")
                    })
            }),
        );
        navigation.set_property(
            "updateCurrentEntry",
            Value::function(|_, args| {
                let state_value = args
                    .first()
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .get_property("state");
                STATE.with(|state| {
                    let mut state = state.borrow_mut();
                    let index = state.index;
                    state.entries[index].state = state_value.clone();
                    let url = state.entries[index].url.clone();
                    crate::history::replace_state(
                        (!state_value.is_undefined())
                            .then(|| state_value.to_js_string())
                            .as_deref(),
                        "",
                        &url,
                    );
                });
                Value::Undefined
            }),
        );
        let prototype = navigation_class().get_property("prototype");
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::web_events::event_target_class().get_property("prototype"),
        );
        for name in [
            "__w3cos_getter_activation",
            "__w3cos_getter_transition",
            "__w3cos_getter_currentEntry",
            "__w3cos_getter_canGoBack",
            "__w3cos_getter_canGoForward",
            "entries",
            "navigate",
            "reload",
            "back",
            "forward",
            "traverseTo",
            "updateCurrentEntry",
            "onnavigate",
            "onnavigatesuccess",
            "onnavigateerror",
            "oncurrententrychange",
        ] {
            prototype.set_property(name, navigation.get_property(name));
        }
        w3cos_core::class::set_prototype_of(&navigation, &prototype);
        *slot.borrow_mut() = Some(navigation.clone());
        navigation
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigate_and_traverse_preserve_entries_and_state() {
        crate::history::reset();
        STATE.with(|state| {
            state.borrow_mut().entries[0].id.clear();
        });
        let navigation = navigation_value();
        let first_key = navigation
            .get_property("currentEntry")
            .get_property("key")
            .to_js_string();
        navigation.call_method(
            "navigate",
            vec![
                Value::string("/page"),
                Value::object(HashMap::from([("state".into(), Value::string("saved"))])),
            ],
        );
        assert_eq!(navigation.call_method("entries", vec![]).iter().len(), 2);
        assert_eq!(
            navigation
                .get_property("currentEntry")
                .get_property("index")
                .to_u32(),
            1
        );
        navigation.call_method("traverseTo", vec![Value::string(&first_key)]);
        assert_eq!(
            navigation
                .get_property("currentEntry")
                .get_property("index")
                .to_u32(),
            0
        );
    }
}
