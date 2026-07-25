//! Gamepad API state and platform input bridge.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::sync::Once;

use w3cos_core::Value;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GamepadButtonState {
    pub pressed: bool,
    pub touched: bool,
    pub value: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GamepadState {
    pub id: String,
    pub index: u32,
    pub connected: bool,
    pub timestamp: f64,
    pub mapping: String,
    pub axes: Vec<f64>,
    pub buttons: Vec<GamepadButtonState>,
}

thread_local! {
    static GAMEPADS: RefCell<BTreeMap<u32, Value>> = const { RefCell::new(BTreeMap::new()) };
    static GAMEPAD_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static GAMEPAD_BUTTON_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static GAMEPAD_EVENT_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static GAMEPAD_HAPTIC_ACTUATOR_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
}

fn illegal_class(name: &str, parent: Option<Value>) -> Value {
    let class_name = name.to_string();
    let class = Value::function(move |_, _| {
        w3cos_core::throw_value(Value::object(HashMap::from([
            ("name".into(), Value::string("TypeError")),
            (
                "message".into(),
                Value::string(&format!("Illegal constructor: {class_name}")),
            ),
        ])))
    });
    class.set_property("name", Value::string(name));
    let prototype = Value::object(HashMap::new());
    prototype.set_property("constructor", class.clone());
    if let Some(parent) = parent {
        w3cos_core::class::set_prototype_of(&prototype, &parent.get_property("prototype"));
    }
    class.set_property("prototype", prototype);
    class
}

pub fn gamepad_class() -> Value {
    GAMEPAD_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = illegal_class("Gamepad", None);
        for property in [
            "axes",
            "buttons",
            "connected",
            "id",
            "index",
            "mapping",
            "timestamp",
            "vibrationActuator",
        ] {
            class
                .get_property("prototype")
                .set_property(property, Value::Undefined);
        }
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn gamepad_button_class() -> Value {
    GAMEPAD_BUTTON_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = illegal_class("GamepadButton", None);
        for property in ["pressed", "touched", "value"] {
            class
                .get_property("prototype")
                .set_property(property, Value::Undefined);
        }
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn gamepad_event_class() -> Value {
    GAMEPAD_EVENT_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|this, args| {
            crate::web_events::event_class().call(this.clone(), args.clone());
            let init = args.get(1).cloned().unwrap_or(Value::Undefined);
            this.set_property("gamepad", init.get_property("gamepad"));
            Value::Undefined
        });
        class.set_property("name", Value::string("GamepadEvent"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property("gamepad", Value::Undefined);
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::web_events::event_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn gamepad_haptic_actuator_class() -> Value {
    GAMEPAD_HAPTIC_ACTUATOR_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = illegal_class("GamepadHapticActuator", None);
        for property in ["effects", "playEffect", "reset", "type"] {
            class
                .get_property("prototype")
                .set_property(property, Value::Undefined);
        }
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn haptic_actuator_value() -> Value {
    let actuator = Value::object(HashMap::from([
        (
            "effects".into(),
            Value::array(vec![Value::string("dual-rumble")]),
        ),
        ("type".into(), Value::string("dual-rumble")),
        (
            "playEffect".into(),
            Value::function(|_, _| {
                static WARNING: Once = Once::new();
                WARNING.call_once(|| {
                    eprintln!(
                        "[w3cos] warning: GamepadHapticActuator returns compatible completion \
                         results; physical vibration requires a platform gamepad haptics adapter"
                    );
                });
                w3cos_core::promise::resolve(vec![Value::string("complete")])
            }),
        ),
        (
            "reset".into(),
            Value::function(|_, _| w3cos_core::promise::resolve(vec![Value::string("complete")])),
        ),
    ]));
    w3cos_core::class::set_prototype_of(
        &actuator,
        &gamepad_haptic_actuator_class().get_property("prototype"),
    );
    actuator
}

fn button_value(state: GamepadButtonState) -> Value {
    let button = Value::object(HashMap::from([
        ("pressed".into(), Value::Bool(state.pressed)),
        ("touched".into(), Value::Bool(state.touched)),
        ("value".into(), Value::Number(state.value.clamp(0.0, 1.0))),
    ]));
    w3cos_core::class::set_prototype_of(&button, &gamepad_button_class().get_property("prototype"));
    button
}

fn apply_state(gamepad: &Value, state: &GamepadState) {
    gamepad.set_property("id", Value::string(&state.id));
    gamepad.set_property("index", Value::Number(state.index as f64));
    gamepad.set_property("connected", Value::Bool(state.connected));
    gamepad.set_property("timestamp", Value::Number(state.timestamp.max(0.0)));
    gamepad.set_property("mapping", Value::string(&state.mapping));
    gamepad.set_property(
        "axes",
        Value::array(
            state
                .axes
                .iter()
                .map(|axis| Value::Number(axis.clamp(-1.0, 1.0)))
                .collect(),
        ),
    );
    gamepad.set_property(
        "buttons",
        Value::array(state.buttons.iter().copied().map(button_value).collect()),
    );
}

fn gamepad_value(state: &GamepadState) -> Value {
    let gamepad = Value::object(HashMap::new());
    w3cos_core::class::set_prototype_of(&gamepad, &gamepad_class().get_property("prototype"));
    gamepad.set_property("vibrationActuator", haptic_actuator_value());
    apply_state(&gamepad, state);
    gamepad
}

fn dispatch_connection_event(event_type: &str, gamepad: Value) {
    let event = w3cos_core::class::construct(
        &gamepad_event_class(),
        vec![
            Value::string(event_type),
            Value::object(HashMap::from([("gamepad".into(), gamepad)])),
        ],
    );
    event.set_property("isTrusted", Value::Bool(true));
    crate::jsdom::window_value().call_method("dispatchEvent", vec![event]);
}

/// Insert, update, or disconnect a controller from the platform input adapter.
pub fn update_gamepad(state: GamepadState) {
    let existing = GAMEPADS.with(|gamepads| gamepads.borrow().get(&state.index).cloned());
    let was_connected = existing
        .as_ref()
        .is_some_and(|gamepad| gamepad.get_property("connected").to_bool());
    let gamepad = existing.unwrap_or_else(|| gamepad_value(&state));
    apply_state(&gamepad, &state);
    if state.connected {
        GAMEPADS.with(|gamepads| {
            gamepads.borrow_mut().insert(state.index, gamepad.clone());
        });
        if !was_connected {
            dispatch_connection_event("gamepadconnected", gamepad);
        }
    } else {
        if was_connected {
            dispatch_connection_event("gamepaddisconnected", gamepad.clone());
        }
        GAMEPADS.with(|gamepads| {
            gamepads.borrow_mut().remove(&state.index);
        });
    }
}

pub fn get_gamepads_value() -> Value {
    Value::function(|_, _| {
        static WARNING: Once = Once::new();
        WARNING.call_once(|| {
            eprintln!(
                "[w3cos] warning: navigator.getGamepads exposes host-injectable controller \
                 snapshots; detecting physical controllers requires a platform gamepad adapter"
            );
        });
        GAMEPADS.with(|gamepads| {
            let gamepads = gamepads.borrow();
            let Some(max_index) = gamepads.keys().next_back().copied() else {
                return Value::array(vec![]);
            };
            let mut values = vec![Value::Null; max_index as usize + 1];
            for (&index, gamepad) in gamepads.iter() {
                values[index as usize] = gamepad.clone();
            }
            Value::array(values)
        })
    })
}

pub fn reset() {
    GAMEPADS.with(|gamepads| gamepads.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn controller(connected: bool) -> GamepadState {
        GamepadState {
            id: "W3COS Controller".into(),
            index: 1,
            connected,
            timestamp: 42.0,
            mapping: "standard".into(),
            axes: vec![-0.5, 0.25],
            buttons: vec![GamepadButtonState {
                pressed: true,
                touched: true,
                value: 0.75,
            }],
        }
    }

    #[test]
    fn host_updates_stable_index_snapshot_and_connection_events() {
        reset();
        let event_log = Rc::new(RefCell::new(Vec::<String>::new()));
        for event_type in ["gamepadconnected", "gamepaddisconnected"] {
            let event_log_for_listener = Rc::clone(&event_log);
            crate::jsdom::window_value().call_method(
                "addEventListener",
                vec![
                    Value::string(event_type),
                    Value::function(move |_, args| {
                        event_log_for_listener.borrow_mut().push(format!(
                            "{}:{}",
                            args[0].get_property("type").to_js_string(),
                            args[0]
                                .get_property("gamepad")
                                .get_property("index")
                                .to_js_string()
                        ));
                        Value::Undefined
                    }),
                ],
            );
        }

        update_gamepad(controller(true));
        let gamepads = get_gamepads_value().call(Value::Undefined, vec![]);
        assert_eq!(gamepads.get_property("length").to_u32(), 2);
        assert!(gamepads.get_property("0").is_null());
        let gamepad = gamepads.get_property("1");
        assert!(w3cos_core::class::instance_of(&gamepad, &gamepad_class()));
        assert!(w3cos_core::class::instance_of(
            &gamepad.get_property("buttons").get_property("0"),
            &gamepad_button_class()
        ));
        assert_eq!(
            gamepad.get_property("axes").get_property("0").to_number(),
            -0.5
        );
        assert_eq!(
            gamepad
                .get_property("buttons")
                .get_property("0")
                .get_property("value")
                .to_number(),
            0.75
        );
        let actuator = gamepad.get_property("vibrationActuator");
        assert!(w3cos_core::class::instance_of(
            &actuator,
            &gamepad_haptic_actuator_class()
        ));
        assert_eq!(
            actuator
                .get_property("effects")
                .get_property("0")
                .to_js_string(),
            "dual-rumble"
        );
        let haptic_result = Rc::new(RefCell::new(String::new()));
        let result_for_callback = Rc::clone(&haptic_result);
        actuator
            .call_method(
                "playEffect",
                vec![Value::string("dual-rumble"), Value::object(HashMap::new())],
            )
            .call_method(
                "then",
                vec![Value::function(move |_, args| {
                    *result_for_callback.borrow_mut() = args[0].to_js_string();
                    Value::Undefined
                })],
            );
        crate::jsdom::drain_microtasks();
        assert_eq!(&*haptic_result.borrow(), "complete");

        update_gamepad(controller(false));
        assert_eq!(
            get_gamepads_value()
                .call(Value::Undefined, vec![])
                .get_property("length")
                .to_u32(),
            0
        );
        assert_eq!(
            &*event_log.borrow(),
            &["gamepadconnected:1", "gamepaddisconnected:1"]
        );
    }
}
