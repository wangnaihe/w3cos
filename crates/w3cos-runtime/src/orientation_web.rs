//! Device Orientation and Motion event APIs with host permission gating.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::Once;

use w3cos_core::Value;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeviceOrientationState {
    pub alpha: Option<f64>,
    pub beta: Option<f64>,
    pub gamma: Option<f64>,
    pub absolute: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vector3 {
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub z: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RotationRate {
    pub alpha: Option<f64>,
    pub beta: Option<f64>,
    pub gamma: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeviceMotionState {
    pub acceleration: Option<Vector3>,
    pub acceleration_including_gravity: Option<Vector3>,
    pub rotation_rate: Option<RotationRate>,
    pub interval: f64,
}

thread_local! {
    static PERMISSION_GRANTED: Cell<bool> = const { Cell::new(false) };
    static DEVICE_ORIENTATION_EVENT_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static DEVICE_MOTION_EVENT_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static DEVICE_MOTION_ACCELERATION_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static DEVICE_MOTION_ROTATION_RATE_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
}

fn optional_number(value: Value) -> Value {
    if value.is_undefined() || value.is_null() {
        Value::Null
    } else {
        Value::Number(value.to_number())
    }
}

fn vector_value(value: Option<Vector3>) -> Value {
    let Some(value) = value else {
        return Value::Null;
    };
    let value = Value::object(HashMap::from([
        (
            "x".into(),
            value.x.map(Value::Number).unwrap_or(Value::Null),
        ),
        (
            "y".into(),
            value.y.map(Value::Number).unwrap_or(Value::Null),
        ),
        (
            "z".into(),
            value.z.map(Value::Number).unwrap_or(Value::Null),
        ),
    ]));
    w3cos_core::class::set_prototype_of(
        &value,
        &device_motion_acceleration_class().get_property("prototype"),
    );
    value
}

fn rotation_value(value: Option<RotationRate>) -> Value {
    let Some(value) = value else {
        return Value::Null;
    };
    let value = Value::object(HashMap::from([
        (
            "alpha".into(),
            value.alpha.map(Value::Number).unwrap_or(Value::Null),
        ),
        (
            "beta".into(),
            value.beta.map(Value::Number).unwrap_or(Value::Null),
        ),
        (
            "gamma".into(),
            value.gamma.map(Value::Number).unwrap_or(Value::Null),
        ),
    ]));
    w3cos_core::class::set_prototype_of(
        &value,
        &device_motion_rotation_rate_class().get_property("prototype"),
    );
    value
}

fn readonly_motion_value(class: Value, fields: &[(&str, Value)]) -> Value {
    let value = Value::object(
        fields
            .iter()
            .map(|(name, value)| ((*name).to_string(), value.clone()))
            .collect(),
    );
    w3cos_core::class::set_prototype_of(&value, &class.get_property("prototype"));
    value
}

fn motion_value_class(
    slot: &'static std::thread::LocalKey<RefCell<Option<Value>>>,
    name: &'static str,
    fields: &'static [&'static str],
) -> Value {
    slot.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(move |_, _| {
            w3cos_core::throw_value(w3cos_core::error_instance(
                "TypeError",
                vec![Value::string(&format!("Illegal constructor: {name}"))],
            ))
        });
        class.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for field in fields {
            prototype.set_property(field, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn device_motion_acceleration_class() -> Value {
    motion_value_class(
        &DEVICE_MOTION_ACCELERATION_CLASS,
        "DeviceMotionEventAcceleration",
        &["x", "y", "z"],
    )
}

pub fn device_motion_rotation_rate_class() -> Value {
    motion_value_class(
        &DEVICE_MOTION_ROTATION_RATE_CLASS,
        "DeviceMotionEventRotationRate",
        &["alpha", "beta", "gamma"],
    )
}

fn request_permission_value() -> Value {
    Value::function(|_, _| {
        static WARNING: Once = Once::new();
        WARNING.call_once(|| {
            eprintln!(
                "[w3cos] warning: device orientation/motion permission is denied until a \
                 platform sensor-consent adapter grants access"
            );
        });
        w3cos_core::promise::resolve(vec![Value::string(if permission_granted() {
            "granted"
        } else {
            "denied"
        })])
    })
}

pub fn device_orientation_event_class() -> Value {
    DEVICE_ORIENTATION_EVENT_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|this, args| {
            crate::web_events::event_class().call(this.clone(), args.clone());
            let init = args.get(1).cloned().unwrap_or(Value::Undefined);
            this.set_property("alpha", optional_number(init.get_property("alpha")));
            this.set_property("beta", optional_number(init.get_property("beta")));
            this.set_property("gamma", optional_number(init.get_property("gamma")));
            this.set_property(
                "absolute",
                Value::Bool(init.get_property("absolute").to_bool()),
            );
            Value::Undefined
        });
        class.set_property("name", Value::string("DeviceOrientationEvent"));
        class.set_property("requestPermission", request_permission_value());
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for property in ["absolute", "alpha", "beta", "gamma"] {
            prototype.set_property(property, Value::Undefined);
        }
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::web_events::event_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn device_motion_event_class() -> Value {
    DEVICE_MOTION_EVENT_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|this, args| {
            crate::web_events::event_class().call(this.clone(), args.clone());
            let init = args.get(1).cloned().unwrap_or(Value::Undefined);
            for name in ["acceleration", "accelerationIncludingGravity"] {
                let source = init.get_property(name);
                this.set_property(
                    name,
                    if source.is_object() {
                        readonly_motion_value(
                            device_motion_acceleration_class(),
                            &[
                                ("x", optional_number(source.get_property("x"))),
                                ("y", optional_number(source.get_property("y"))),
                                ("z", optional_number(source.get_property("z"))),
                            ],
                        )
                    } else {
                        Value::Null
                    },
                );
            }
            let rotation = init.get_property("rotationRate");
            this.set_property(
                "rotationRate",
                if rotation.is_object() {
                    readonly_motion_value(
                        device_motion_rotation_rate_class(),
                        &[
                            ("alpha", optional_number(rotation.get_property("alpha"))),
                            ("beta", optional_number(rotation.get_property("beta"))),
                            ("gamma", optional_number(rotation.get_property("gamma"))),
                        ],
                    )
                } else {
                    Value::Null
                },
            );
            let interval = init.get_property("interval");
            this.set_property(
                "interval",
                Value::Number(if interval.is_undefined() {
                    0.0
                } else {
                    interval.to_number().max(0.0)
                }),
            );
            Value::Undefined
        });
        class.set_property("name", Value::string("DeviceMotionEvent"));
        class.set_property("requestPermission", request_permission_value());
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for property in [
            "acceleration",
            "accelerationIncludingGravity",
            "interval",
            "rotationRate",
        ] {
            prototype.set_property(property, Value::Undefined);
        }
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::web_events::event_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn permission_granted() -> bool {
    PERMISSION_GRANTED.with(Cell::get)
}

/// Update sensor permission after platform user consent.
pub fn set_permission_granted(granted: bool) {
    PERMISSION_GRANTED.with(|permission| permission.set(granted));
}

pub fn dispatch_device_orientation(state: DeviceOrientationState) -> bool {
    if !permission_granted() {
        return false;
    }
    let event = w3cos_core::class::construct(
        &device_orientation_event_class(),
        vec![
            Value::string("deviceorientation"),
            Value::object(HashMap::from([
                (
                    "alpha".into(),
                    state.alpha.map(Value::Number).unwrap_or(Value::Null),
                ),
                (
                    "beta".into(),
                    state.beta.map(Value::Number).unwrap_or(Value::Null),
                ),
                (
                    "gamma".into(),
                    state.gamma.map(Value::Number).unwrap_or(Value::Null),
                ),
                ("absolute".into(), Value::Bool(state.absolute)),
            ])),
        ],
    );
    event.set_property("isTrusted", Value::Bool(true));
    crate::jsdom::window_value().call_method("dispatchEvent", vec![event]);
    true
}

pub fn dispatch_device_motion(state: DeviceMotionState) -> bool {
    if !permission_granted() {
        return false;
    }
    let event = w3cos_core::class::construct(
        &device_motion_event_class(),
        vec![
            Value::string("devicemotion"),
            Value::object(HashMap::from([
                ("acceleration".into(), vector_value(state.acceleration)),
                (
                    "accelerationIncludingGravity".into(),
                    vector_value(state.acceleration_including_gravity),
                ),
                ("rotationRate".into(), rotation_value(state.rotation_rate)),
                ("interval".into(), Value::Number(state.interval.max(0.0))),
            ])),
        ],
    );
    event.set_property("isTrusted", Value::Bool(true));
    crate::jsdom::window_value().call_method("dispatchEvent", vec![event]);
    true
}

pub fn reset() {
    set_permission_granted(false);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn permission_gates_trusted_orientation_and_motion_events() {
        reset();
        assert!(!dispatch_device_orientation(DeviceOrientationState {
            alpha: Some(1.0),
            beta: Some(2.0),
            gamma: Some(3.0),
            absolute: true,
        }));
        let log = Rc::new(RefCell::new(Vec::<String>::new()));
        for event_type in ["deviceorientation", "devicemotion"] {
            let log_for_listener = Rc::clone(&log);
            crate::jsdom::window_value().call_method(
                "addEventListener",
                vec![
                    Value::string(event_type),
                    Value::function(move |_, args| {
                        let event = args[0].clone();
                        assert!(event.get_property("isTrusted").to_bool());
                        log_for_listener.borrow_mut().push(format!(
                            "{}:{}",
                            event.get_property("type").to_js_string(),
                            if event_type == "deviceorientation" {
                                event.get_property("alpha").to_js_string()
                            } else {
                                event
                                    .get_property("acceleration")
                                    .get_property("x")
                                    .to_js_string()
                            }
                        ));
                        Value::Undefined
                    }),
                ],
            );
        }
        set_permission_granted(true);
        assert!(dispatch_device_orientation(DeviceOrientationState {
            alpha: Some(10.0),
            beta: Some(20.0),
            gamma: None,
            absolute: true,
        }));
        assert!(dispatch_device_motion(DeviceMotionState {
            acceleration: Some(Vector3 {
                x: Some(0.5),
                y: Some(1.0),
                z: None,
            }),
            acceleration_including_gravity: None,
            rotation_rate: Some(RotationRate {
                alpha: Some(1.0),
                beta: Some(2.0),
                gamma: Some(3.0),
            }),
            interval: 16.0,
        }));
        assert_eq!(
            &*log.borrow(),
            &["deviceorientation:10", "devicemotion:0.5"]
        );
    }
}
