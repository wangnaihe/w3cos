//! Window/input environment compatibility APIs discovered from Chromium.
//!
//! The values expose browser identities and host-injectable snapshots. Native
//! keyboard locking and virtual-keyboard control warn when no adapter exists.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::Once;

use w3cos_core::Value;

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static VIRTUAL_KEYBOARD: RefCell<Option<Value>> = const { RefCell::new(None) };
    static DEVICE_POSTURE: RefCell<Option<Value>> = const { RefCell::new(None) };
    static WINDOW_CONTROLS_OVERLAY: RefCell<Option<Value>> = const { RefCell::new(None) };
    static INPUT_PENDING: Cell<bool> = const { Cell::new(false) };
}

fn error(name: &str, message: &str) -> Value {
    w3cos_core::class::construct(
        &crate::unsupported::dom_exception_class(),
        vec![Value::string(message), Value::string(name)],
    )
}

fn illegal_class(name: &'static str, event_target: bool) -> Value {
    CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        let class = Value::function(move |_, _| {
            w3cos_core::throw_value(error("TypeError", &format!("Illegal constructor: {name}")))
        });
        class.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        if name == "KeyboardLayoutMap" {
            for member in ["size", "get", "has", "entries", "keys", "values", "forEach"] {
                prototype.set_property(member, Value::Undefined);
            }
        }
        if event_target {
            w3cos_core::class::set_prototype_of(
                &prototype,
                &crate::web_events::event_target_class().get_property("prototype"),
            );
        }
        class.set_property("prototype", prototype);
        classes.borrow_mut().insert(name.into(), class.clone());
        class
    })
}

fn iterator(values: Vec<Value>) -> Value {
    Value::array(values).call_method("__w3cos_symbol_iterator", vec![])
}

fn mirror_prototype(value: &Value, class: &Value, names: &[&str]) {
    let prototype = class.get_property("prototype");
    for name in names {
        prototype.set_property(name, value.get_property(name));
    }
}

fn layout_map_value(entries: Vec<(String, String)>) -> Value {
    let entries = std::rc::Rc::new(entries);
    let map = Value::object(HashMap::new());
    let entries_for_size = std::rc::Rc::clone(&entries);
    map.set_property(
        "__w3cos_getter_size",
        Value::function(move |_, _| Value::Number(entries_for_size.len() as f64)),
    );
    let entries_for_get = std::rc::Rc::clone(&entries);
    map.set_property(
        "get",
        Value::function(move |_, args| {
            let key = args.first().cloned().unwrap_or_default().to_js_string();
            entries_for_get
                .iter()
                .find_map(|(code, value)| (code == &key).then(|| Value::string(value)))
                .unwrap_or(Value::Undefined)
        }),
    );
    let entries_for_has = std::rc::Rc::clone(&entries);
    map.set_property(
        "has",
        Value::function(move |_, args| {
            let key = args.first().cloned().unwrap_or_default().to_js_string();
            Value::Bool(entries_for_has.iter().any(|(code, _)| code == &key))
        }),
    );
    let entries_for_entries = std::rc::Rc::clone(&entries);
    let entries_method = Value::function(move |_, _| {
        iterator(
            entries_for_entries
                .iter()
                .map(|(key, value)| Value::array(vec![Value::string(key), Value::string(value)]))
                .collect(),
        )
    });
    map.set_property("entries", entries_method.clone());
    map.set_property("__w3cos_symbol_iterator", entries_method);
    let entries_for_keys = std::rc::Rc::clone(&entries);
    map.set_property(
        "keys",
        Value::function(move |_, _| {
            iterator(
                entries_for_keys
                    .iter()
                    .map(|(key, _)| Value::string(key))
                    .collect(),
            )
        }),
    );
    let entries_for_values = std::rc::Rc::clone(&entries);
    map.set_property(
        "values",
        Value::function(move |_, _| {
            iterator(
                entries_for_values
                    .iter()
                    .map(|(_, value)| Value::string(value))
                    .collect(),
            )
        }),
    );
    let entries_for_each = entries;
    let map_for_each = map.clone();
    map.set_property(
        "forEach",
        Value::function(move |_, args| {
            let callback = args.first().cloned().unwrap_or(Value::Undefined);
            let this_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
            if !callback.is_function() {
                w3cos_core::throw_value(error(
                    "TypeError",
                    "KeyboardLayoutMap.forEach requires a callback",
                ));
            }
            for (key, value) in entries_for_each.iter() {
                callback.call(
                    this_arg.clone(),
                    vec![
                        Value::string(value),
                        Value::string(key),
                        map_for_each.clone(),
                    ],
                );
            }
            Value::Undefined
        }),
    );
    w3cos_core::class::set_prototype_of(
        &map,
        &keyboard_layout_map_class().get_property("prototype"),
    );
    mirror_prototype(
        &map,
        &keyboard_layout_map_class(),
        &[
            "__w3cos_getter_size",
            "get",
            "has",
            "entries",
            "keys",
            "values",
            "forEach",
            "__w3cos_symbol_iterator",
        ],
    );
    map
}

pub fn keyboard_class() -> Value {
    illegal_class("Keyboard", true)
}

pub fn keyboard_layout_map_class() -> Value {
    illegal_class("KeyboardLayoutMap", false)
}

pub fn keyboard_value() -> Value {
    let keyboard = Value::object(HashMap::new());
    crate::web_events::event_target_class().call(keyboard.clone(), vec![]);
    keyboard.set_property(
        "getLayoutMap",
        Value::function(|_, _| {
            static WARNING: Once = Once::new();
            WARNING.call_once(|| {
                eprintln!(
                    "[w3cos] warning: Keyboard.getLayoutMap returns an empty compatibility map \
                     until a host keyboard-layout adapter is configured"
                );
            });
            w3cos_core::promise::resolve(vec![layout_map_value(vec![])])
        }),
    );
    keyboard.set_property(
        "lock",
        Value::function(|_, _| {
            static WARNING: Once = Once::new();
            WARNING.call_once(|| {
                eprintln!(
                    "[w3cos] warning: Keyboard.lock requires a host exclusive-keyboard adapter"
                );
            });
            w3cos_core::promise::reject(vec![error(
                "NotSupportedError",
                "Keyboard locking is unavailable without a host adapter",
            )])
        }),
    );
    keyboard.set_property(
        "unlock",
        Value::function(|_, _| {
            static WARNING: Once = Once::new();
            WARNING.call_once(|| {
                eprintln!(
                    "[w3cos] warning: Keyboard.unlock has no native effect without a host adapter"
                );
            });
            Value::Undefined
        }),
    );
    w3cos_core::class::set_prototype_of(&keyboard, &keyboard_class().get_property("prototype"));
    mirror_prototype(
        &keyboard,
        &keyboard_class(),
        &["getLayoutMap", "lock", "unlock"],
    );
    keyboard
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> Value {
    w3cos_core::class::construct(
        &crate::geometry_web::class("DOMRect"),
        vec![
            Value::Number(x),
            Value::Number(y),
            Value::Number(width),
            Value::Number(height),
        ],
    )
}

pub fn virtual_keyboard_class() -> Value {
    illegal_class("VirtualKeyboard", true)
}

pub fn virtual_keyboard_value() -> Value {
    VIRTUAL_KEYBOARD.with(|slot| {
        if let Some(value) = slot.borrow().clone() {
            return value;
        }
        let value = Value::object(HashMap::from([
            ("overlaysContent".into(), Value::Bool(false)),
            ("boundingRect".into(), rect(0.0, 0.0, 0.0, 0.0)),
            ("ongeometrychange".into(), Value::Null),
        ]));
        crate::web_events::event_target_class().call(value.clone(), vec![]);
        for method in ["show", "hide"] {
            value.set_property(
                method,
                Value::function(move |_, _| {
                    static WARNING: Once = Once::new();
                    WARNING.call_once(|| {
                        eprintln!(
                            "[w3cos] warning: VirtualKeyboard show/hide requires a host IME adapter"
                        );
                    });
                    Value::Undefined
                }),
            );
        }
        w3cos_core::class::set_prototype_of(
            &value,
            &virtual_keyboard_class().get_property("prototype"),
        );
        mirror_prototype(
            &value,
            &virtual_keyboard_class(),
            &[
                "boundingRect",
                "overlaysContent",
                "ongeometrychange",
                "show",
                "hide",
            ],
        );
        *slot.borrow_mut() = Some(value.clone());
        value
    })
}

pub fn device_posture_class() -> Value {
    illegal_class("DevicePosture", true)
}

pub fn device_posture_value() -> Value {
    DEVICE_POSTURE.with(|slot| {
        if let Some(value) = slot.borrow().clone() {
            return value;
        }
        let value = Value::object(HashMap::from([
            ("type".into(), Value::string("continuous")),
            ("onchange".into(), Value::Null),
        ]));
        crate::web_events::event_target_class().call(value.clone(), vec![]);
        w3cos_core::class::set_prototype_of(
            &value,
            &device_posture_class().get_property("prototype"),
        );
        mirror_prototype(&value, &device_posture_class(), &["type", "onchange"]);
        *slot.borrow_mut() = Some(value.clone());
        value
    })
}

pub fn window_controls_overlay_class() -> Value {
    illegal_class("WindowControlsOverlay", true)
}

pub fn window_controls_overlay_value() -> Value {
    WINDOW_CONTROLS_OVERLAY.with(|slot| {
        if let Some(value) = slot.borrow().clone() {
            return value;
        }
        let value = Value::object(HashMap::from([
            ("visible".into(), Value::Bool(false)),
            ("ongeometrychange".into(), Value::Null),
        ]));
        crate::web_events::event_target_class().call(value.clone(), vec![]);
        value.set_property(
            "getTitlebarAreaRect",
            Value::function(|this, _| {
                let stored = this.get_property("__titlebarRect");
                if stored.is_undefined() {
                    rect(0.0, 0.0, 0.0, 0.0)
                } else {
                    stored
                }
            }),
        );
        w3cos_core::class::set_prototype_of(
            &value,
            &window_controls_overlay_class().get_property("prototype"),
        );
        mirror_prototype(
            &value,
            &window_controls_overlay_class(),
            &["visible", "ongeometrychange", "getTitlebarAreaRect"],
        );
        *slot.borrow_mut() = Some(value.clone());
        value
    })
}

pub fn scheduling_class() -> Value {
    illegal_class("Scheduling", false)
}

pub fn scheduling_value() -> Value {
    let value = Value::object(HashMap::from([(
        "isInputPending".into(),
        Value::function(|_, _| Value::Bool(INPUT_PENDING.with(Cell::get))),
    )]));
    w3cos_core::class::set_prototype_of(&value, &scheduling_class().get_property("prototype"));
    mirror_prototype(&value, &scheduling_class(), &["isInputPending"]);
    value
}

fn dispatch_change(target: &Value, event_name: &str) {
    let event = w3cos_core::class::construct(
        &crate::web_events::event_class(),
        vec![Value::string(event_name)],
    );
    target.call_method("dispatchEvent", vec![event]);
}

pub fn set_virtual_keyboard_geometry(x: f64, y: f64, width: f64, height: f64) {
    let value = virtual_keyboard_value();
    value.set_property("boundingRect", rect(x, y, width, height));
    dispatch_change(&value, "geometrychange");
}

pub fn set_device_posture(posture: &str) {
    let posture = if posture == "folded" {
        "folded"
    } else {
        "continuous"
    };
    let value = device_posture_value();
    if value.get_property("type").to_js_string() != posture {
        value.set_property("type", Value::string(posture));
        dispatch_change(&value, "change");
    }
}

pub fn set_window_controls_overlay(visible: bool, x: f64, y: f64, width: f64, height: f64) {
    let value = window_controls_overlay_value();
    value.set_property("visible", Value::Bool(visible));
    value.set_property("__titlebarRect", rect(x, y, width, height));
    dispatch_change(&value, "geometrychange");
}

pub fn set_input_pending(pending: bool) {
    INPUT_PENDING.with(|state| state.set(pending));
}

/// Release mutable EventTarget wrappers owned by the current Window Realm.
/// Host input/posture state remains available to seed replacements.
pub fn reset_realm() {
    VIRTUAL_KEYBOARD.with(|slot| {
        slot.borrow_mut().take();
    });
    DEVICE_POSTURE.with(|slot| {
        slot.borrow_mut().take();
    });
    WINDOW_CONTROLS_OVERLAY.with(|slot| {
        slot.borrow_mut().take();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn exposes_empty_keyboard_layout_and_explicit_lock_failure() {
        let keyboard = keyboard_value();
        let log = std::rc::Rc::new(RefCell::new(Vec::new()));
        let log_for_layout = std::rc::Rc::clone(&log);
        keyboard.call_method("getLayoutMap", vec![]).call_method(
            "then",
            vec![Value::function(move |_, args| {
                let map = args[0].clone();
                log_for_layout
                    .borrow_mut()
                    .push(map.get_property("size").to_js_string());
                Value::Undefined
            })],
        );
        let log_for_lock = std::rc::Rc::clone(&log);
        keyboard.call_method("lock", vec![]).call_method(
            "catch",
            vec![Value::function(move |_, args| {
                log_for_lock
                    .borrow_mut()
                    .push(args[0].get_property("name").to_js_string());
                Value::Undefined
            })],
        );
        crate::jsdom::drain_microtasks();
        assert_eq!(&*log.borrow(), &["0", "NotSupportedError"]);
    }

    #[test]
    fn host_updates_environment_snapshots_and_dispatches_events() {
        let keyboard = virtual_keyboard_value();
        let keyboard_events = Rc::new(Cell::new(0));
        let keyboard_events_for_handler = Rc::clone(&keyboard_events);
        keyboard.call_method(
            "addEventListener",
            vec![
                Value::string("geometrychange"),
                Value::function(move |_, _| {
                    keyboard_events_for_handler.set(keyboard_events_for_handler.get() + 1);
                    Value::Undefined
                }),
            ],
        );
        set_virtual_keyboard_geometry(0.0, 500.0, 800.0, 300.0);
        assert_eq!(
            keyboard.get_property("boundingRect").get_property("height"),
            300.into()
        );
        assert_eq!(keyboard_events.get(), 1);

        let posture = device_posture_value();
        set_device_posture("folded");
        assert_eq!(posture.get_property("type"), Value::string("folded"));

        let overlay = window_controls_overlay_value();
        set_window_controls_overlay(true, 10.0, 0.0, 600.0, 30.0);
        assert!(overlay.get_property("visible").to_bool());
        assert_eq!(
            overlay
                .call_method("getTitlebarAreaRect", vec![])
                .get_property("width"),
            600.into()
        );

        let scheduling = scheduling_value();
        assert!(!scheduling.call_method("isInputPending", vec![]).to_bool());
        set_input_pending(true);
        assert!(scheduling.call_method("isInputPending", vec![]).to_bool());
        set_input_pending(false);
    }
}
