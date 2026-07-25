//! Generic Sensor API core for host-injectable vector sensors.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Once;

use w3cos_core::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SensorKind {
    Accelerometer,
    Gravity,
    LinearAcceleration,
    Gyroscope,
    Magnetometer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrientationKind {
    Absolute,
    Relative,
}

struct SensorState {
    kind: SensorKind,
    sensor: RefCell<Value>,
    activated: Cell<bool>,
    has_reading: Cell<bool>,
    timestamp: Cell<Option<f64>>,
    x: Cell<Option<f64>>,
    y: Cell<Option<f64>>,
    z: Cell<Option<f64>>,
    generation: Cell<u64>,
}

struct OrientationState {
    kind: OrientationKind,
    sensor: RefCell<Value>,
    activated: Cell<bool>,
    has_reading: Cell<bool>,
    timestamp: Cell<Option<f64>>,
    quaternion: Cell<Option<[f64; 4]>>,
    generation: Cell<u64>,
}

thread_local! {
    static SENSOR_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static SENSOR_ERROR_EVENT_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static SENSOR_CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static SENSOR_INSTANCES: RefCell<Vec<Rc<SensorState>>> = const { RefCell::new(Vec::new()) };
    static ORIENTATION_INSTANCES: RefCell<Vec<Rc<OrientationState>>> = const { RefCell::new(Vec::new()) };
}

fn error(name: &str, message: &str) -> Value {
    Value::object(HashMap::from([
        ("name".into(), Value::string(name)),
        ("message".into(), Value::string(message)),
    ]))
}

fn optional_number(value: Option<f64>) -> Value {
    value.map(Value::Number).unwrap_or(Value::Null)
}

fn warn_host_adapter() {
    static WARNING: Once = Once::new();
    WARNING.call_once(|| {
        eprintln!(
            "[w3cos] warning: Generic Sensor lifecycle is available, but live motion, gravity, \
             gyroscope, magnetometer and orientation readings require a platform sensor adapter"
        );
    });
}

pub fn sensor_class() -> Value {
    SENSOR_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            w3cos_core::throw_value(error("TypeError", "Illegal constructor: Sensor"))
        });
        class.set_property("name", Value::string("Sensor"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for method in ["start", "stop"] {
            prototype.set_property(method, Value::function(|_, _| Value::Undefined));
        }
        for property in [
            "activated",
            "hasReading",
            "onactivate",
            "onerror",
            "onreading",
            "timestamp",
        ] {
            prototype.set_property(property, Value::Undefined);
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

pub fn sensor_error_event_class() -> Value {
    SENSOR_ERROR_EVENT_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|this, args| {
            crate::web_events::event_class().call(this.clone(), args.clone());
            let init = args.get(1).cloned().unwrap_or(Value::Undefined);
            this.set_property("error", init.get_property("error"));
            Value::Undefined
        });
        class.set_property("name", Value::string("SensorErrorEvent"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property("error", Value::Undefined);
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::web_events::event_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn dispatch_event(sensor: &Value, event_type: &str) {
    let event = w3cos_core::class::construct(
        &crate::web_events::event_class(),
        vec![Value::string(event_type)],
    );
    sensor.call_method("dispatchEvent", vec![event]);
}

fn dispatch_error(sensor: &Value, name: &str, message: &str) {
    let event = w3cos_core::class::construct(
        &sensor_error_event_class(),
        vec![
            Value::string("error"),
            Value::object(HashMap::from([("error".into(), error(name, message))])),
        ],
    );
    sensor.call_method("dispatchEvent", vec![event]);
}

fn validate_options(options: &Value) {
    if !options.is_object() {
        return;
    }
    let frequency = options.get_property("frequency");
    if !frequency.is_undefined() {
        let frequency = frequency.to_number();
        if !frequency.is_finite() || frequency <= 0.0 {
            w3cos_core::throw_value(error(
                "TypeError",
                "sensor frequency must be finite and greater than zero",
            ));
        }
    }
    let reference_frame = options.get_property("referenceFrame");
    if !reference_frame.is_undefined()
        && !matches!(reference_frame.to_js_string().as_str(), "device" | "screen")
    {
        w3cos_core::throw_value(error(
            "TypeError",
            "sensor referenceFrame must be device or screen",
        ));
    }
}

fn install_sensor(sensor: &Value, args: &[Value], kind: SensorKind) {
    validate_options(&args.first().cloned().unwrap_or(Value::Undefined));
    crate::web_events::event_target_class().call(sensor.clone(), vec![]);
    let state = Rc::new(SensorState {
        kind,
        sensor: RefCell::new(sensor.clone()),
        activated: Cell::new(false),
        has_reading: Cell::new(false),
        timestamp: Cell::new(None),
        x: Cell::new(None),
        y: Cell::new(None),
        z: Cell::new(None),
        generation: Cell::new(0),
    });
    for (name, getter) in [
        ("activated", {
            let state = Rc::clone(&state);
            Value::function(move |_, _| Value::Bool(state.activated.get()))
        }),
        ("hasReading", {
            let state = Rc::clone(&state);
            Value::function(move |_, _| Value::Bool(state.has_reading.get()))
        }),
        ("timestamp", {
            let state = Rc::clone(&state);
            Value::function(move |_, _| optional_number(state.timestamp.get()))
        }),
        ("x", {
            let state = Rc::clone(&state);
            Value::function(move |_, _| optional_number(state.x.get()))
        }),
        ("y", {
            let state = Rc::clone(&state);
            Value::function(move |_, _| optional_number(state.y.get()))
        }),
        ("z", {
            let state = Rc::clone(&state);
            Value::function(move |_, _| optional_number(state.z.get()))
        }),
    ] {
        sensor.set_property(&format!("__w3cos_getter_{name}"), getter);
    }
    for name in ["onactivate", "onreading", "onerror"] {
        sensor.set_property(name, Value::Null);
    }
    let start_state = Rc::clone(&state);
    sensor.set_property(
        "start",
        Value::function(move |_, _| {
            warn_host_adapter();
            if start_state.activated.get() {
                return Value::Undefined;
            }
            let generation = start_state.generation.get().wrapping_add(1);
            start_state.generation.set(generation);
            let state = Rc::clone(&start_state);
            crate::jsdom::queue_microtask_value(Value::function(move |_, _| {
                if state.generation.get() != generation {
                    return Value::Undefined;
                }
                let sensor = state.sensor.borrow().clone();
                if !crate::orientation_web::permission_granted() {
                    dispatch_error(
                        &sensor,
                        "NotAllowedError",
                        "sensor access requires platform user consent",
                    );
                    return Value::Undefined;
                }
                state.activated.set(true);
                dispatch_event(&sensor, "activate");
                Value::Undefined
            }));
            Value::Undefined
        }),
    );
    let stop_state = Rc::clone(&state);
    sensor.set_property(
        "stop",
        Value::function(move |_, _| {
            stop_state
                .generation
                .set(stop_state.generation.get().wrapping_add(1));
            stop_state.activated.set(false);
            stop_state.has_reading.set(false);
            stop_state.timestamp.set(None);
            stop_state.x.set(None);
            stop_state.y.set(None);
            stop_state.z.set(None);
            Value::Undefined
        }),
    );
    SENSOR_INSTANCES.with(|instances| instances.borrow_mut().push(state));
}

fn vector_sensor_class(name: &'static str, kind: SensorKind) -> Value {
    SENSOR_CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        let class = Value::function(move |this, args| {
            install_sensor(&this, &args, kind);
            Value::Undefined
        });
        class.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for property in ["x", "y", "z"] {
            prototype.set_property(property, Value::Undefined);
        }
        w3cos_core::class::set_prototype_of(&prototype, &sensor_class().get_property("prototype"));
        class.set_property("prototype", prototype);
        classes.borrow_mut().insert(name.into(), class.clone());
        class
    })
}

pub fn accelerometer_class() -> Value {
    vector_sensor_class("Accelerometer", SensorKind::Accelerometer)
}

pub fn gyroscope_class() -> Value {
    vector_sensor_class("Gyroscope", SensorKind::Gyroscope)
}

pub fn magnetometer_class() -> Value {
    vector_sensor_class("Magnetometer", SensorKind::Magnetometer)
}

fn derived_accelerometer_class(name: &'static str, kind: SensorKind) -> Value {
    SENSOR_CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        let class = Value::function(move |this, args| {
            install_sensor(&this, &args, kind);
            Value::Undefined
        });
        class.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::from([("constructor".into(), class.clone())]));
        w3cos_core::class::set_prototype_of(
            &prototype,
            &accelerometer_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        classes.borrow_mut().insert(name.into(), class.clone());
        class
    })
}

pub fn gravity_sensor_class() -> Value {
    derived_accelerometer_class("GravitySensor", SensorKind::Gravity)
}

pub fn linear_acceleration_sensor_class() -> Value {
    derived_accelerometer_class("LinearAccelerationSensor", SensorKind::LinearAcceleration)
}

pub fn orientation_sensor_class() -> Value {
    SENSOR_CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get("OrientationSensor").cloned() {
            return class;
        }
        let class = Value::function(|_, _| {
            w3cos_core::throw_value(error("TypeError", "Illegal constructor: OrientationSensor"))
        });
        class.set_property("name", Value::string("OrientationSensor"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property("quaternion", Value::Undefined);
        prototype.set_property("populateMatrix", Value::Undefined);
        w3cos_core::class::set_prototype_of(&prototype, &sensor_class().get_property("prototype"));
        class.set_property("prototype", prototype);
        classes
            .borrow_mut()
            .insert("OrientationSensor".into(), class.clone());
        class
    })
}

fn quaternion_matrix([x, y, z, w]: [f64; 4]) -> [f64; 16] {
    [
        1.0 - 2.0 * (y * y + z * z),
        2.0 * (x * y + z * w),
        2.0 * (x * z - y * w),
        0.0,
        2.0 * (x * y - z * w),
        1.0 - 2.0 * (x * x + z * z),
        2.0 * (y * z + x * w),
        0.0,
        2.0 * (x * z + y * w),
        2.0 * (y * z - x * w),
        1.0 - 2.0 * (x * x + y * y),
        0.0,
        0.0,
        0.0,
        0.0,
        1.0,
    ]
}

fn install_orientation_sensor(sensor: &Value, args: &[Value], kind: OrientationKind) {
    validate_options(&args.first().cloned().unwrap_or(Value::Undefined));
    crate::web_events::event_target_class().call(sensor.clone(), Vec::new());
    let state = Rc::new(OrientationState {
        kind,
        sensor: RefCell::new(sensor.clone()),
        activated: Cell::new(false),
        has_reading: Cell::new(false),
        timestamp: Cell::new(None),
        quaternion: Cell::new(None),
        generation: Cell::new(0),
    });
    for name in ["onactivate", "onreading", "onerror"] {
        sensor.set_property(name, Value::Null);
    }
    for (name, getter) in [
        ("activated", {
            let state = Rc::clone(&state);
            Value::function(move |_, _| Value::Bool(state.activated.get()))
        }),
        ("hasReading", {
            let state = Rc::clone(&state);
            Value::function(move |_, _| Value::Bool(state.has_reading.get()))
        }),
        ("timestamp", {
            let state = Rc::clone(&state);
            Value::function(move |_, _| optional_number(state.timestamp.get()))
        }),
        ("quaternion", {
            let state = Rc::clone(&state);
            Value::function(move |_, _| {
                state
                    .quaternion
                    .get()
                    .map(|quaternion| {
                        Value::array(quaternion.into_iter().map(Value::Number).collect())
                    })
                    .unwrap_or(Value::Null)
            })
        }),
    ] {
        sensor.set_property(&format!("__w3cos_getter_{name}"), getter);
    }
    let matrix_state = Rc::clone(&state);
    sensor.set_property(
        "populateMatrix",
        Value::function(move |_, args| {
            let destination = args.first().cloned().unwrap_or(Value::Undefined);
            let Some(quaternion) = matrix_state.quaternion.get() else {
                w3cos_core::throw_value(error(
                    "NotReadableError",
                    "orientation sensor has no reading",
                ));
            };
            if destination.get_property("length").to_u32() < 16 {
                w3cos_core::throw_value(error(
                    "TypeError",
                    "populateMatrix requires a 16-element typed array",
                ));
            }
            destination.call_method(
                "set",
                vec![Value::array(
                    quaternion_matrix(quaternion)
                        .into_iter()
                        .map(Value::Number)
                        .collect(),
                )],
            );
            Value::Undefined
        }),
    );
    let start_state = Rc::clone(&state);
    sensor.set_property(
        "start",
        Value::function(move |_, _| {
            warn_host_adapter();
            if start_state.activated.get() {
                return Value::Undefined;
            }
            let generation = start_state.generation.get().wrapping_add(1);
            start_state.generation.set(generation);
            let state = Rc::clone(&start_state);
            crate::jsdom::queue_microtask_value(Value::function(move |_, _| {
                if state.generation.get() != generation {
                    return Value::Undefined;
                }
                let sensor = state.sensor.borrow().clone();
                if !crate::orientation_web::permission_granted() {
                    dispatch_error(
                        &sensor,
                        "NotAllowedError",
                        "sensor access requires platform user consent",
                    );
                    return Value::Undefined;
                }
                state.activated.set(true);
                dispatch_event(&sensor, "activate");
                Value::Undefined
            }));
            Value::Undefined
        }),
    );
    let stop_state = Rc::clone(&state);
    sensor.set_property(
        "stop",
        Value::function(move |_, _| {
            stop_state
                .generation
                .set(stop_state.generation.get().wrapping_add(1));
            stop_state.activated.set(false);
            stop_state.has_reading.set(false);
            stop_state.timestamp.set(None);
            stop_state.quaternion.set(None);
            Value::Undefined
        }),
    );
    ORIENTATION_INSTANCES.with(|instances| instances.borrow_mut().push(state));
}

fn concrete_orientation_class(name: &'static str, kind: OrientationKind) -> Value {
    SENSOR_CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        let class = Value::function(move |this, args| {
            install_orientation_sensor(&this, &args, kind);
            Value::Undefined
        });
        class.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::from([("constructor".into(), class.clone())]));
        w3cos_core::class::set_prototype_of(
            &prototype,
            &orientation_sensor_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        classes.borrow_mut().insert(name.into(), class.clone());
        class
    })
}

pub fn absolute_orientation_sensor_class() -> Value {
    concrete_orientation_class("AbsoluteOrientationSensor", OrientationKind::Absolute)
}

pub fn relative_orientation_sensor_class() -> Value {
    concrete_orientation_class("RelativeOrientationSensor", OrientationKind::Relative)
}

/// Push one vector reading from a platform sensor adapter.
pub fn update_vector(
    kind: SensorKind,
    x: Option<f64>,
    y: Option<f64>,
    z: Option<f64>,
    timestamp: f64,
) {
    if !crate::orientation_web::permission_granted() {
        return;
    }
    SENSOR_INSTANCES.with(|instances| {
        for state in instances.borrow().iter() {
            if state.kind != kind || !state.activated.get() {
                continue;
            }
            state.x.set(x);
            state.y.set(y);
            state.z.set(z);
            state.timestamp.set(Some(timestamp.max(0.0)));
            state.has_reading.set(true);
            dispatch_event(&state.sensor.borrow(), "reading");
        }
    });
}

/// Push one normalized quaternion reading from a platform orientation adapter.
pub fn update_orientation(kind: OrientationKind, quaternion: [f64; 4], timestamp: f64) {
    if !crate::orientation_web::permission_granted()
        || quaternion.iter().any(|value| !value.is_finite())
    {
        return;
    }
    let magnitude = quaternion
        .iter()
        .map(|value| value * value)
        .sum::<f64>()
        .sqrt();
    if magnitude == 0.0 {
        return;
    }
    let quaternion = quaternion.map(|value| value / magnitude);
    ORIENTATION_INSTANCES.with(|instances| {
        for state in instances.borrow().iter() {
            if state.kind != kind || !state.activated.get() {
                continue;
            }
            state.quaternion.set(Some(quaternion));
            state.timestamp.set(Some(timestamp.max(0.0)));
            state.has_reading.set(true);
            dispatch_event(&state.sensor.borrow(), "reading");
        }
    });
}

pub fn reset() {
    SENSOR_INSTANCES.with(|instances| instances.borrow_mut().clear());
    ORIENTATION_INSTANCES.with(|instances| instances.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn permission_errors_and_host_readings_follow_sensor_lifecycle() {
        reset();
        crate::orientation_web::set_permission_granted(false);
        let denied = w3cos_core::class::construct(&gyroscope_class(), vec![]);
        let errors = Rc::new(Cell::new(0));
        let errors_for_listener = Rc::clone(&errors);
        denied.call_method(
            "addEventListener",
            vec![
                Value::string("error"),
                Value::function(move |_, args| {
                    assert_eq!(
                        args[0]
                            .get_property("error")
                            .get_property("name")
                            .to_js_string(),
                        "NotAllowedError"
                    );
                    errors_for_listener.set(errors_for_listener.get() + 1);
                    Value::Undefined
                }),
            ],
        );
        denied.call_method("start", vec![]);
        crate::jsdom::drain_microtasks();
        assert_eq!(errors.get(), 1);
        assert!(!denied.get_property("activated").to_bool());

        crate::orientation_web::set_permission_granted(true);
        let sensor = w3cos_core::class::construct(
            &accelerometer_class(),
            vec![Value::object(HashMap::from([(
                "frequency".into(),
                Value::Number(60.0),
            )]))],
        );
        assert!(w3cos_core::class::instance_of(&sensor, &sensor_class()));
        let activations = Rc::new(Cell::new(0));
        let readings = Rc::new(Cell::new(0));
        for (event_type, counter) in [
            ("activate", Rc::clone(&activations)),
            ("reading", Rc::clone(&readings)),
        ] {
            sensor.call_method(
                "addEventListener",
                vec![
                    Value::string(event_type),
                    Value::function(move |_, _| {
                        counter.set(counter.get() + 1);
                        Value::Undefined
                    }),
                ],
            );
        }
        sensor.call_method("start", vec![]);
        assert!(!sensor.get_property("activated").to_bool());
        crate::jsdom::drain_microtasks();
        assert!(sensor.get_property("activated").to_bool());
        assert_eq!(activations.get(), 1);

        update_vector(
            SensorKind::Accelerometer,
            Some(0.25),
            Some(-0.5),
            None,
            12.0,
        );
        assert_eq!(readings.get(), 1);
        assert!(sensor.get_property("hasReading").to_bool());
        assert_eq!(sensor.get_property("x").to_number(), 0.25);
        assert!(sensor.get_property("z").is_null());
        sensor.call_method("stop", vec![]);
        assert!(!sensor.get_property("activated").to_bool());
        assert!(!sensor.get_property("hasReading").to_bool());
        assert!(sensor.get_property("timestamp").is_null());
    }

    #[test]
    fn orientation_and_gravity_subclasses_reuse_sensor_lifecycle() {
        reset();
        crate::orientation_web::set_permission_granted(true);
        let orientation =
            w3cos_core::class::construct(&absolute_orientation_sensor_class(), Vec::new());
        assert!(w3cos_core::class::instance_of(
            &orientation,
            &orientation_sensor_class()
        ));
        orientation.call_method("start", Vec::new());
        crate::jsdom::drain_microtasks();
        update_orientation(OrientationKind::Absolute, [0.0, 0.0, 0.0, 2.0], 5.0);
        assert_eq!(
            orientation
                .get_property("quaternion")
                .get_property("3")
                .to_number(),
            1.0
        );
        let matrix =
            w3cos_core::binary::typed_array_value((0..16).map(|_| Value::Number(0.0)).collect());
        orientation.call_method("populateMatrix", vec![matrix.clone()]);
        assert_eq!(matrix.get_property("0").to_number(), 1.0);
        assert_eq!(matrix.get_property("15").to_number(), 1.0);

        let gravity = w3cos_core::class::construct(&gravity_sensor_class(), Vec::new());
        assert!(w3cos_core::class::instance_of(
            &gravity,
            &accelerometer_class()
        ));
        gravity.call_method("start", Vec::new());
        crate::jsdom::drain_microtasks();
        update_vector(SensorKind::Gravity, Some(0.0), Some(9.81), Some(0.0), 6.0);
        assert_eq!(gravity.get_property("y").to_number(), 9.81);
    }
}
