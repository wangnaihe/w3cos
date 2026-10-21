//! Generic Sensor API core for host-injectable vector sensors.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
#[cfg(not(feature = "web-sensors"))]
use std::sync::Once;
use std::sync::atomic::{AtomicU64, Ordering};

use w3cos_core::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SensorKind {
    Accelerometer,
    Gravity,
    LinearAcceleration,
    Gyroscope,
    Magnetometer,
    AmbientLight,
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
    illuminance: Cell<Option<f64>>,
    generation: Cell<u64>,
    platform_id: u64,
    frequency: f64,
}

struct OrientationState {
    kind: OrientationKind,
    sensor: RefCell<Value>,
    activated: Cell<bool>,
    has_reading: Cell<bool>,
    timestamp: Cell<Option<f64>>,
    quaternion: Cell<Option<[f64; 4]>>,
    generation: Cell<u64>,
    platform_id: u64,
    frequency: f64,
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

#[cfg(not(feature = "web-sensors"))]
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

fn alloc_platform_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn kind_permissions(kind: SensorKind) -> &'static [&'static str] {
    match kind {
        SensorKind::Accelerometer | SensorKind::Gravity | SensorKind::LinearAcceleration => {
            &["accelerometer"]
        }
        SensorKind::Gyroscope => &["gyroscope"],
        SensorKind::Magnetometer => &["magnetometer"],
        SensorKind::AmbientLight => &["ambient-light-sensor"],
    }
}

fn orientation_permissions(kind: OrientationKind) -> &'static [&'static str] {
    match kind {
        OrientationKind::Absolute => &["accelerometer", "magnetometer"],
        OrientationKind::Relative => &["accelerometer", "gyroscope"],
    }
}

fn sensor_frequency(options: &Value) -> f64 {
    validate_options(options);
    if !options.is_object() {
        return 60.0;
    }
    let frequency = options.get_property("frequency");
    if frequency.is_undefined() {
        60.0
    } else {
        frequency.to_number()
    }
}

fn end_live(id: u64) {
    #[cfg(feature = "web-sensors")]
    crate::sensor_iio::close_source(id);
    #[cfg(not(feature = "web-sensors"))]
    let _ = id;
}

fn begin_vector(id: u64, kind: SensorKind, frequency_hz: f64) -> Result<(), &'static str> {
    #[cfg(feature = "web-sensors")]
    {
        let source = match kind {
            SensorKind::Accelerometer => crate::sensor_iio::Source::Accelerometer,
            SensorKind::Gravity => crate::sensor_iio::Source::Gravity,
            SensorKind::LinearAcceleration => crate::sensor_iio::Source::LinearAcceleration,
            SensorKind::Gyroscope => crate::sensor_iio::Source::Gyroscope,
            SensorKind::Magnetometer => crate::sensor_iio::Source::Magnetometer,
            SensorKind::AmbientLight => crate::sensor_iio::Source::AmbientLight,
        };
        return match crate::sensor_iio::open_source(id, source, frequency_hz) {
            crate::sensor_iio::OpenResult::Missing(message) => Err(message),
            crate::sensor_iio::OpenResult::HostOnly | crate::sensor_iio::OpenResult::Live => Ok(()),
        };
    }
    #[cfg(not(feature = "web-sensors"))]
    {
        let _ = (id, kind, frequency_hz);
        Ok(())
    }
}

fn begin_orientation(
    id: u64,
    kind: OrientationKind,
    frequency_hz: f64,
) -> Result<(), &'static str> {
    #[cfg(feature = "web-sensors")]
    {
        let source = match kind {
            OrientationKind::Absolute => crate::sensor_iio::Source::AbsoluteOrientation,
            OrientationKind::Relative => crate::sensor_iio::Source::RelativeOrientation,
        };
        return match crate::sensor_iio::open_source(id, source, frequency_hz) {
            crate::sensor_iio::OpenResult::Missing(message) => Err(message),
            crate::sensor_iio::OpenResult::HostOnly | crate::sensor_iio::OpenResult::Live => Ok(()),
        };
    }
    #[cfg(not(feature = "web-sensors"))]
    {
        let _ = (id, kind, frequency_hz);
        Ok(())
    }
}

fn schedule_start<M, A, B, S, E>(
    generation_matches: M,
    allowed: A,
    begin: B,
    on_success: S,
    on_error: E,
    platform_id: u64,
) where
    M: Fn() -> bool + 'static,
    A: Fn() -> bool + 'static,
    B: Fn() -> Result<(), &'static str> + 'static,
    S: Fn() + 'static,
    E: Fn(&str, &str) + 'static,
{
    crate::jsdom::queue_microtask_value(Value::function(move |_, _| {
        if !generation_matches() {
            return Value::Undefined;
        }
        if !allowed() {
            on_error(
                "NotAllowedError",
                "sensor access requires platform user consent",
            );
            return Value::Undefined;
        }
        if let Err(message) = begin() {
            if generation_matches() {
                on_error("NotReadableError", message);
            } else {
                end_live(platform_id);
            }
            return Value::Undefined;
        }
        if !generation_matches() {
            end_live(platform_id);
            return Value::Undefined;
        }
        on_success();
        Value::Undefined
    }));
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
    let frequency = sensor_frequency(&args.first().cloned().unwrap_or(Value::Undefined));
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
        illuminance: Cell::new(None),
        generation: Cell::new(0),
        platform_id: alloc_platform_id(),
        frequency,
    });
    let mut getters = vec![
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
    ];
    if kind == SensorKind::AmbientLight {
        let state = Rc::clone(&state);
        getters.push((
            "illuminance",
            Value::function(move |_, _| optional_number(state.illuminance.get())),
        ));
    } else {
        getters.extend([
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
        ]);
    }
    for (name, getter) in getters {
        sensor.set_property(&format!("__w3cos_getter_{name}"), getter);
    }
    for name in ["onactivate", "onreading", "onerror"] {
        sensor.set_property(name, Value::Null);
    }
    let start_state = Rc::clone(&state);
    sensor.set_property(
        "start",
        Value::function(move |_, _| {
            #[cfg(not(feature = "web-sensors"))]
            warn_host_adapter();
            if start_state.activated.get() {
                return Value::Undefined;
            }
            let generation = start_state.generation.get().wrapping_add(1);
            start_state.generation.set(generation);
            let state = Rc::clone(&start_state);
            let permissions = kind_permissions(state.kind);
            let platform_id = state.platform_id;
            schedule_start(
                {
                    let state = Rc::clone(&state);
                    move || state.generation.get() == generation
                },
                move || crate::orientation_web::sensor_names_allowed(permissions),
                {
                    let state = Rc::clone(&state);
                    move || begin_vector(state.platform_id, state.kind, state.frequency)
                },
                {
                    let state = Rc::clone(&state);
                    move || {
                        state.activated.set(true);
                        dispatch_event(&state.sensor.borrow(), "activate");
                    }
                },
                {
                    let state = Rc::clone(&state);
                    move |name, message| {
                        dispatch_error(&state.sensor.borrow(), name, message);
                    }
                },
                platform_id,
            );
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
            stop_state.illuminance.set(None);
            end_live(stop_state.platform_id);
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

pub fn ambient_light_sensor_class() -> Value {
    SENSOR_CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get("AmbientLightSensor").cloned() {
            return class;
        }
        let class = Value::function(move |this, args| {
            install_sensor(&this, &args, SensorKind::AmbientLight);
            Value::Undefined
        });
        class.set_property("name", Value::string("AmbientLightSensor"));
        let prototype = Value::object(HashMap::from([
            ("constructor".into(), class.clone()),
            ("illuminance".into(), Value::Undefined),
        ]));
        w3cos_core::class::set_prototype_of(&prototype, &sensor_class().get_property("prototype"));
        class.set_property("prototype", prototype);
        classes
            .borrow_mut()
            .insert("AmbientLightSensor".into(), class.clone());
        class
    })
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
    let frequency = sensor_frequency(&args.first().cloned().unwrap_or(Value::Undefined));
    crate::web_events::event_target_class().call(sensor.clone(), Vec::new());
    let state = Rc::new(OrientationState {
        kind,
        sensor: RefCell::new(sensor.clone()),
        activated: Cell::new(false),
        has_reading: Cell::new(false),
        timestamp: Cell::new(None),
        quaternion: Cell::new(None),
        generation: Cell::new(0),
        platform_id: alloc_platform_id(),
        frequency,
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
            #[cfg(not(feature = "web-sensors"))]
            warn_host_adapter();
            if start_state.activated.get() {
                return Value::Undefined;
            }
            let generation = start_state.generation.get().wrapping_add(1);
            start_state.generation.set(generation);
            let state = Rc::clone(&start_state);
            let permissions = orientation_permissions(state.kind);
            let platform_id = state.platform_id;
            schedule_start(
                {
                    let state = Rc::clone(&state);
                    move || state.generation.get() == generation
                },
                move || crate::orientation_web::sensor_names_allowed(permissions),
                {
                    let state = Rc::clone(&state);
                    move || begin_orientation(state.platform_id, state.kind, state.frequency)
                },
                {
                    let state = Rc::clone(&state);
                    move || {
                        state.activated.set(true);
                        dispatch_event(&state.sensor.borrow(), "activate");
                    }
                },
                {
                    let state = Rc::clone(&state);
                    move |name, message| {
                        dispatch_error(&state.sensor.borrow(), name, message);
                    }
                },
                platform_id,
            );
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
            end_live(stop_state.platform_id);
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
    if !crate::orientation_web::sensor_names_allowed(kind_permissions(kind)) {
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
    if !crate::orientation_web::sensor_names_allowed(orientation_permissions(kind))
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

/// Push one lux reading into activated `AmbientLightSensor` instances.
pub fn update_illuminance(lux: f64, timestamp: f64) {
    if !lux.is_finite()
        || !crate::orientation_web::sensor_names_allowed(kind_permissions(SensorKind::AmbientLight))
    {
        return;
    }
    SENSOR_INSTANCES.with(|instances| {
        for state in instances.borrow().iter() {
            if state.kind != SensorKind::AmbientLight || !state.activated.get() {
                continue;
            }
            state.illuminance.set(Some(lux));
            state.timestamp.set(Some(timestamp.max(0.0)));
            state.has_reading.set(true);
            dispatch_event(&state.sensor.borrow(), "reading");
        }
    });
}

pub(crate) fn poll_platform_samples() -> usize {
    #[cfg(feature = "web-sensors")]
    {
        let samples = crate::sensor_iio::take_samples();
        let count = samples.len();
        for sample in samples {
            apply_sample(sample);
        }
        return count;
    }
    #[cfg(not(feature = "web-sensors"))]
    {
        0
    }
}

pub(crate) fn next_sample_deadline() -> Option<std::time::Instant> {
    #[cfg(feature = "web-sensors")]
    {
        return crate::sensor_iio::next_deadline();
    }
    #[cfg(not(feature = "web-sensors"))]
    {
        None
    }
}

#[cfg(feature = "web-sensors")]
fn apply_sample(sample: crate::sensor_iio::Sample) {
    use crate::sensor_iio::{Sample, Source};
    match sample {
        Sample::Vector {
            source,
            x,
            y,
            z,
            timestamp,
        } => {
            let Some(kind) = vector_kind(source) else {
                return;
            };
            update_vector(kind, Some(x), Some(y), Some(z), timestamp);
        }
        Sample::Illuminance { lux, timestamp } => update_illuminance(lux, timestamp),
        Sample::Quaternion {
            source,
            quaternion,
            timestamp,
        } => {
            let kind = match source {
                Source::AbsoluteOrientation => OrientationKind::Absolute,
                Source::RelativeOrientation => OrientationKind::Relative,
                _ => return,
            };
            update_orientation(kind, quaternion, timestamp);
        }
        Sample::Motion {
            acceleration,
            including_gravity,
            rotation_alpha,
            rotation_beta,
            rotation_gamma,
            interval_ms,
        } => {
            crate::orientation_web::dispatch_device_motion(
                crate::orientation_web::DeviceMotionState {
                    acceleration: acceleration.map(vector3),
                    acceleration_including_gravity: including_gravity.map(vector3),
                    rotation_rate: Some(crate::orientation_web::RotationRate {
                        alpha: rotation_alpha,
                        beta: rotation_beta,
                        gamma: rotation_gamma,
                    }),
                    interval: interval_ms,
                },
            );
        }
        Sample::Orientation {
            alpha,
            beta,
            gamma,
            absolute,
        } => {
            let state = crate::orientation_web::DeviceOrientationState {
                alpha,
                beta: Some(beta),
                gamma: Some(gamma),
                absolute,
            };
            crate::orientation_web::dispatch_device_orientation(state);
            if absolute {
                crate::orientation_web::dispatch_device_orientation_absolute(state);
            }
        }
        Sample::Unavailable { source } => fail_source(source),
    }
}

#[cfg(feature = "web-sensors")]
fn vector3(value: [f64; 3]) -> crate::orientation_web::Vector3 {
    crate::orientation_web::Vector3 {
        x: Some(value[0]),
        y: Some(value[1]),
        z: Some(value[2]),
    }
}

#[cfg(feature = "web-sensors")]
fn vector_kind(source: crate::sensor_iio::Source) -> Option<SensorKind> {
    use crate::sensor_iio::Source;
    match source {
        Source::Accelerometer => Some(SensorKind::Accelerometer),
        Source::Gravity => Some(SensorKind::Gravity),
        Source::LinearAcceleration => Some(SensorKind::LinearAcceleration),
        Source::Gyroscope => Some(SensorKind::Gyroscope),
        Source::Magnetometer => Some(SensorKind::Magnetometer),
        _ => None,
    }
}

#[cfg(feature = "web-sensors")]
fn fail_source(source: crate::sensor_iio::Source) {
    use crate::sensor_iio::Source;
    if let Some(kind) = vector_kind(source) {
        SENSOR_INSTANCES.with(|instances| {
            for state in instances.borrow().iter() {
                if state.kind != kind || !state.activated.get() {
                    continue;
                }
                clear_vector(state);
                dispatch_error(
                    &state.sensor.borrow(),
                    "NotReadableError",
                    "sensor is not exposed by the host",
                );
            }
        });
        return;
    }
    if source == Source::AmbientLight {
        SENSOR_INSTANCES.with(|instances| {
            for state in instances.borrow().iter() {
                if state.kind != SensorKind::AmbientLight || !state.activated.get() {
                    continue;
                }
                clear_vector(state);
                dispatch_error(
                    &state.sensor.borrow(),
                    "NotReadableError",
                    "sensor is not exposed by the host",
                );
            }
        });
        return;
    }
    let kind = match source {
        Source::AbsoluteOrientation => Some(OrientationKind::Absolute),
        Source::RelativeOrientation => Some(OrientationKind::Relative),
        _ => None,
    };
    let Some(kind) = kind else {
        return;
    };
    ORIENTATION_INSTANCES.with(|instances| {
        for state in instances.borrow().iter() {
            if state.kind != kind || !state.activated.get() {
                continue;
            }
            state.activated.set(false);
            state.has_reading.set(false);
            state.timestamp.set(None);
            state.quaternion.set(None);
            end_live(state.platform_id);
            dispatch_error(
                &state.sensor.borrow(),
                "NotReadableError",
                "sensor is not exposed by the host",
            );
        }
    });
}

#[cfg(feature = "web-sensors")]
fn clear_vector(state: &SensorState) {
    state.activated.set(false);
    state.has_reading.set(false);
    state.timestamp.set(None);
    state.x.set(None);
    state.y.set(None);
    state.z.set(None);
    state.illuminance.set(None);
    end_live(state.platform_id);
}

pub fn reset() {
    SENSOR_INSTANCES.with(|instances| instances.borrow_mut().clear());
    ORIENTATION_INSTANCES.with(|instances| instances.borrow_mut().clear());
    #[cfg(feature = "web-sensors")]
    crate::sensor_iio::reset();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn permission_errors_and_host_readings_follow_sensor_lifecycle() {
        reset();
        crate::capability_gate::reset();
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
        crate::capability_gate::reset();
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

    #[cfg(feature = "web-sensors")]
    mod live {
        use super::*;
        use std::cell::RefCell;
        use std::fs;
        use std::path::Path;
        use std::rc::Rc;
        use std::time::Duration;

        fn isolate() {
            reset();
            crate::capability_gate::reset();
            crate::orientation_web::set_permission_granted(false);
        }

        fn write_device(root: &Path, files: &[(&str, &str)]) {
            let device = root.join("iio:device0");
            fs::create_dir_all(&device).unwrap();
            for (name, body) in files {
                fs::write(device.join(name), body).unwrap();
            }
            crate::sensor_iio::set_sysfs_root(Some(root.to_path_buf()));
        }

        fn accel_flat() -> Vec<(&'static str, &'static str)> {
            vec![
                ("in_accel_x_raw", "0\n"),
                ("in_accel_y_raw", "0\n"),
                ("in_accel_z_raw", "981\n"),
                ("in_accel_scale", "0.01\n"),
            ]
        }

        fn pump_until(mut ready: impl FnMut() -> bool) -> bool {
            for _ in 0..40 {
                crate::jsdom::drain_microtasks();
                if ready() {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            false
        }

        fn error_name(sensor: &Value) -> Rc<RefCell<String>> {
            let name = Rc::new(RefCell::new(String::new()));
            let slot = Rc::clone(&name);
            sensor.call_method(
                "addEventListener",
                vec![
                    Value::string("error"),
                    Value::function(move |_, args| {
                        *slot.borrow_mut() = args[0]
                            .get_property("error")
                            .get_property("name")
                            .to_js_string();
                        Value::Undefined
                    }),
                ],
            );
            name
        }

        #[test]
        fn denied_permission_does_not_read_a_present_sensor() {
            isolate();
            let dir = tempfile::tempdir().unwrap();
            write_device(dir.path(), &accel_flat());
            let sensor = w3cos_core::class::construct(&accelerometer_class(), vec![]);
            let error = error_name(&sensor);
            sensor.call_method("start", vec![]);
            assert!(pump_until(|| !error.borrow().as_str().is_empty()));
            assert_eq!(error.borrow().as_str(), "NotAllowedError");
            assert!(!sensor.get_property("activated").to_bool());
            isolate();
        }

        #[test]
        fn missing_channel_is_not_readable_after_grant() {
            isolate();
            crate::orientation_web::set_permission_granted(true);
            let dir = tempfile::tempdir().unwrap();
            fs::create_dir_all(dir.path()).unwrap();
            crate::sensor_iio::set_sysfs_root(Some(dir.path().to_path_buf()));
            let sensor = w3cos_core::class::construct(&gyroscope_class(), vec![]);
            let error = error_name(&sensor);
            sensor.call_method("start", vec![]);
            assert!(pump_until(|| !error.borrow().as_str().is_empty()));
            assert_eq!(error.borrow().as_str(), "NotReadableError");
            assert!(!sensor.get_property("activated").to_bool());
            isolate();
        }

        #[test]
        fn fake_sysfs_scale_reaches_an_activated_accelerometer() {
            isolate();
            crate::orientation_web::set_permission_granted(true);
            let dir = tempfile::tempdir().unwrap();
            write_device(dir.path(), &accel_flat());
            let sensor = w3cos_core::class::construct(
                &accelerometer_class(),
                vec![Value::object(HashMap::from([(
                    "frequency".into(),
                    Value::Number(50.0),
                )]))],
            );
            sensor.call_method("start", vec![]);
            assert!(pump_until(|| sensor.get_property("hasReading").to_bool()));
            assert!(sensor.get_property("activated").to_bool());
            assert!((sensor.get_property("z").to_number() - 9.81).abs() < 1e-6);
            assert_eq!(sensor.get_property("x").to_number(), 0.0);
            sensor.call_method("stop", vec![]);
            assert!(!sensor.get_property("hasReading").to_bool());
            isolate();
        }

        #[test]
        fn capability_decisions_override_the_legacy_permission_flag() {
            isolate();
            crate::orientation_web::set_permission_granted(true);
            crate::capability_gate::deny("gyroscope");
            let gyro = w3cos_core::class::construct(&gyroscope_class(), vec![]);
            let error = error_name(&gyro);
            gyro.call_method("start", vec![]);
            assert!(pump_until(|| !error.borrow().as_str().is_empty()));
            assert_eq!(error.borrow().as_str(), "NotAllowedError");

            isolate();
            crate::capability_gate::grant("accelerometer");
            let sensor = w3cos_core::class::construct(&accelerometer_class(), vec![]);
            sensor.call_method("start", vec![]);
            crate::jsdom::drain_microtasks();
            assert!(sensor.get_property("activated").to_bool());
            let gyro = w3cos_core::class::construct(&gyroscope_class(), vec![]);
            let error = error_name(&gyro);
            gyro.call_method("start", vec![]);
            assert!(pump_until(|| !error.borrow().as_str().is_empty()));
            assert_eq!(error.borrow().as_str(), "NotAllowedError");
            isolate();
        }

        #[test]
        fn orientation_requires_the_channels_it_can_derive() {
            isolate();
            crate::orientation_web::set_permission_granted(true);
            let dir = tempfile::tempdir().unwrap();
            write_device(dir.path(), &accel_flat());
            let absolute =
                w3cos_core::class::construct(&absolute_orientation_sensor_class(), vec![]);
            let error = error_name(&absolute);
            absolute.call_method("start", vec![]);
            assert!(pump_until(|| !error.borrow().as_str().is_empty()));
            assert_eq!(error.borrow().as_str(), "NotReadableError");

            let relative =
                w3cos_core::class::construct(&relative_orientation_sensor_class(), vec![]);
            relative.call_method("start", vec![]);
            assert!(pump_until(|| {
                relative.get_property("hasReading").to_bool()
            }));
            let w = relative
                .get_property("quaternion")
                .get_property("3")
                .to_number();
            assert!(w > 0.99, "{w}");

            let mut files = accel_flat();
            files.extend([
                ("in_magn_x_raw", "0\n"),
                ("in_magn_y_raw", "0.4\n"),
                ("in_magn_z_raw", "0\n"),
                ("in_magn_scale", "1\n"),
            ]);
            write_device(dir.path(), &files);
            let absolute =
                w3cos_core::class::construct(&absolute_orientation_sensor_class(), vec![]);
            absolute.call_method("start", vec![]);
            assert!(pump_until(|| {
                absolute.get_property("hasReading").to_bool()
            }));
            let w = absolute
                .get_property("quaternion")
                .get_property("3")
                .to_number();
            assert!(w > 0.99, "{w}");
            isolate();
        }

        #[test]
        fn ambient_light_follows_its_own_grant_and_lux_channel() {
            isolate();
            let dark = w3cos_core::class::construct(&ambient_light_sensor_class(), vec![]);
            assert!(dark.get_property("x").is_undefined());
            let error = error_name(&dark);
            dark.call_method("start", vec![]);
            assert!(pump_until(|| !error.borrow().as_str().is_empty()));
            assert_eq!(error.borrow().as_str(), "NotAllowedError");

            crate::capability_gate::grant("ambient-light-sensor");
            let host = w3cos_core::class::construct(&ambient_light_sensor_class(), vec![]);
            host.call_method("start", vec![]);
            crate::jsdom::drain_microtasks();
            update_illuminance(12.0, 3.0);
            assert_eq!(host.get_property("illuminance").to_number(), 12.0);

            let dir = tempfile::tempdir().unwrap();
            write_device(
                dir.path(),
                &[
                    ("in_illuminance_raw", "25\n"),
                    ("in_illuminance_offset", "1\n"),
                    ("in_illuminance_scale", "2\n"),
                ],
            );
            let sensor = w3cos_core::class::construct(&ambient_light_sensor_class(), vec![]);
            sensor.call_method("start", vec![]);
            assert!(pump_until(|| sensor.get_property("hasReading").to_bool()));
            assert!((sensor.get_property("illuminance").to_number() - 52.0).abs() < 1e-6);
            isolate();
        }

        #[test]
        fn window_motion_events_follow_sysfs_channels() {
            isolate();
            crate::orientation_web::set_permission_granted(true);
            let dir = tempfile::tempdir().unwrap();
            let mut files = accel_flat();
            files.extend([
                ("in_anglvel_x_raw", "0\n"),
                ("in_anglvel_y_raw", "0\n"),
                ("in_anglvel_z_raw", "314.159265359\n"),
                ("in_anglvel_scale", "0.01\n"),
            ]);
            write_device(dir.path(), &files);
            let motion = Rc::new(RefCell::new(String::new()));
            let orientation = Rc::new(RefCell::new(String::new()));
            let motion_slot = Rc::clone(&motion);
            let orientation_slot = Rc::clone(&orientation);
            crate::jsdom::window_value().call_method(
                "addEventListener",
                vec![
                    Value::string("devicemotion"),
                    Value::function(move |_, args| {
                        let event = &args[0];
                        *motion_slot.borrow_mut() = format!(
                            "{}:{}:{}",
                            event
                                .get_property("accelerationIncludingGravity")
                                .get_property("z")
                                .to_number(),
                            event
                                .get_property("rotationRate")
                                .get_property("alpha")
                                .to_number(),
                            event.get_property("interval").to_number()
                        );
                        Value::Undefined
                    }),
                ],
            );
            crate::jsdom::window_value().call_method(
                "addEventListener",
                vec![
                    Value::string("deviceorientation"),
                    Value::function(move |_, args| {
                        let event = &args[0];
                        *orientation_slot.borrow_mut() = format!(
                            "{}:{}:{}",
                            event.get_property("beta").to_number(),
                            event.get_property("gamma").to_number(),
                            event.get_property("absolute").to_bool()
                        );
                        Value::Undefined
                    }),
                ],
            );
            assert!(pump_until(|| !motion.borrow().as_str().is_empty()
                && !orientation.borrow().as_str().is_empty()));
            let motion_text = motion.borrow().clone();
            let mut parts = motion_text.split(':');
            let z: f64 = parts.next().unwrap().parse().unwrap();
            let alpha: f64 = parts.next().unwrap().parse().unwrap();
            let interval: f64 = parts.next().unwrap().parse().unwrap();
            assert!((z - 9.81).abs() < 1e-6, "{motion_text}");
            assert!((alpha - 180.0).abs() < 0.05, "{motion_text}");
            assert!(interval > 0.0);
            let orientation_text = orientation.borrow().clone();
            let mut parts = orientation_text.split(':');
            let beta: f64 = parts.next().unwrap().parse().unwrap();
            let gamma: f64 = parts.next().unwrap().parse().unwrap();
            assert!(
                beta.abs() < 0.05 && gamma.abs() < 0.05,
                "{orientation_text}"
            );
            assert!(orientation_text.ends_with(":false"));
            crate::jsdom::reset_bridge();
        }
    }
}
