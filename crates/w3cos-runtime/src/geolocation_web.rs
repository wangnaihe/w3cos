//! Browser-shaped Geolocation facade with a host-injectable position source.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use w3cos_core::Value;

#[derive(Clone, Debug)]
pub struct GeoPosition {
    pub latitude: f64,
    pub longitude: f64,
    pub accuracy: f64,
    pub altitude: Option<f64>,
    pub altitude_accuracy: Option<f64>,
    pub heading: Option<f64>,
    pub speed: Option<f64>,
    pub timestamp_ms: f64,
}

impl GeoPosition {
    pub fn new(latitude: f64, longitude: f64, accuracy: f64) -> Self {
        Self {
            latitude,
            longitude,
            accuracy,
            altitude: None,
            altitude_accuracy: None,
            heading: None,
            speed: None,
            timestamp_ms: now_ms(),
        }
    }
}

#[derive(Clone)]
struct Watch {
    id: u32,
    success: Value,
    error: Value,
    options: Value,
}

thread_local! {
    static CURRENT_POSITION: RefCell<Option<GeoPosition>> = const { RefCell::new(None) };
    static PERMISSION_DENIED: Cell<bool> = const { Cell::new(false) };
    static NEXT_WATCH_ID: Cell<u32> = const { Cell::new(1) };
    static WATCHES: RefCell<Vec<Watch>> = const { RefCell::new(Vec::new()) };
}

fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
        * 1000.0
}

fn nullable_number(value: Option<f64>) -> Value {
    value.map(Value::Number).unwrap_or(Value::Null)
}

fn position_value(position: &GeoPosition) -> Value {
    let coords = Value::object(HashMap::from([
        ("latitude".to_string(), Value::Number(position.latitude)),
        ("longitude".to_string(), Value::Number(position.longitude)),
        ("accuracy".to_string(), Value::Number(position.accuracy)),
        ("altitude".to_string(), nullable_number(position.altitude)),
        (
            "altitudeAccuracy".to_string(),
            nullable_number(position.altitude_accuracy),
        ),
        ("heading".to_string(), nullable_number(position.heading)),
        ("speed".to_string(), nullable_number(position.speed)),
    ]));
    Value::object(HashMap::from([
        ("coords".to_string(), coords),
        (
            "timestamp".to_string(),
            Value::Number(position.timestamp_ms),
        ),
    ]))
}

fn error_value(code: u32, message: &str) -> Value {
    Value::object(HashMap::from([
        ("code".to_string(), Value::Number(code as f64)),
        ("message".to_string(), Value::string(message)),
        ("PERMISSION_DENIED".to_string(), Value::Number(1.0)),
        ("POSITION_UNAVAILABLE".to_string(), Value::Number(2.0)),
        ("TIMEOUT".to_string(), Value::Number(3.0)),
    ]))
}

fn option_number(options: &Value, name: &str) -> Option<f64> {
    if !options.is_object() {
        return None;
    }
    let value = options.get_property(name);
    (!value.is_undefined()).then(|| value.to_number().max(0.0))
}

fn deliver(success: Value, error: Value, options: Value) {
    crate::jsdom::queue_microtask_value(Value::function(move |_, _| {
        if PERMISSION_DENIED.with(Cell::get) {
            if error.is_function() {
                error.call(
                    Value::Undefined,
                    vec![error_value(1, "Geolocation permission denied")],
                );
            }
            return Value::Undefined;
        }
        if option_number(&options, "timeout") == Some(0.0) {
            if error.is_function() {
                error.call(
                    Value::Undefined,
                    vec![error_value(3, "Geolocation request timed out")],
                );
            }
            return Value::Undefined;
        }
        let position = CURRENT_POSITION.with(|current| current.borrow().clone());
        if let Some(position) = position {
            let maximum_age = option_number(&options, "maximumAge").unwrap_or(f64::INFINITY);
            if now_ms() - position.timestamp_ms <= maximum_age {
                if success.is_function() {
                    success.call(Value::Undefined, vec![position_value(&position)]);
                }
                return Value::Undefined;
            }
        }
        if error.is_function() {
            error.call(
                Value::Undefined,
                vec![error_value(
                    2,
                    "Geolocation is unavailable on this w3cos target",
                )],
            );
        }
        Value::Undefined
    }));
}

fn deliver_watch(id: u32) {
    let watch = WATCHES.with(|watches| {
        watches
            .borrow()
            .iter()
            .find(|watch| watch.id == id)
            .cloned()
    });
    if let Some(watch) = watch {
        let success = watch.success;
        let error = watch.error;
        let options = watch.options;
        crate::jsdom::queue_microtask_value(Value::function(move |_, _| {
            let still_active =
                WATCHES.with(|watches| watches.borrow().iter().any(|watch| watch.id == id));
            if still_active {
                deliver(success.clone(), error.clone(), options.clone());
            }
            Value::Undefined
        }));
    }
}

pub fn geolocation_value() -> Value {
    let value = Value::object(HashMap::new());
    value.set_property(
        "getCurrentPosition",
        Value::function(|_, args| {
            deliver(
                args.first().cloned().unwrap_or_default(),
                args.get(1).cloned().unwrap_or_default(),
                args.get(2).cloned().unwrap_or_default(),
            );
            Value::Undefined
        }),
    );
    value.set_property(
        "watchPosition",
        Value::function(|_, args| {
            let id = NEXT_WATCH_ID.with(|next| {
                let id = next.get();
                next.set(id.saturating_add(1));
                id
            });
            WATCHES.with(|watches| {
                watches.borrow_mut().push(Watch {
                    id,
                    success: args.first().cloned().unwrap_or_default(),
                    error: args.get(1).cloned().unwrap_or_default(),
                    options: args.get(2).cloned().unwrap_or_default(),
                });
            });
            deliver_watch(id);
            Value::Number(id as f64)
        }),
    );
    value.set_property(
        "clearWatch",
        Value::function(|_, args| {
            let id = args.first().cloned().unwrap_or_default().to_u32();
            WATCHES.with(|watches| watches.borrow_mut().retain(|watch| watch.id != id));
            Value::Undefined
        }),
    );
    value
}

/// Supply a fresh host position and notify active JavaScript watchers.
pub fn update_position(position: GeoPosition) {
    CURRENT_POSITION.with(|current| *current.borrow_mut() = Some(position));
    let ids = WATCHES.with(|watches| {
        watches
            .borrow()
            .iter()
            .map(|watch| watch.id)
            .collect::<Vec<_>>()
    });
    for id in ids {
        deliver_watch(id);
    }
}

pub fn set_permission_denied(denied: bool) {
    PERMISSION_DENIED.with(|value| value.set(denied));
}

pub fn reset() {
    CURRENT_POSITION.with(|current| *current.borrow_mut() = None);
    PERMISSION_DENIED.with(|value| value.set(false));
    WATCHES.with(|watches| watches.borrow_mut().clear());
    NEXT_WATCH_ID.with(|next| next.set(1));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn current_position_and_watch_have_browser_shapes() {
        reset();
        update_position(GeoPosition::new(31.23, 121.47, 8.0));
        let geolocation = geolocation_value();
        let seen = Rc::new(RefCell::new(Vec::new()));
        let capture = seen.clone();
        let success = Value::function(move |_, args| {
            let position = args.first().cloned().unwrap_or_default();
            capture.borrow_mut().push(format!(
                "{}:{}:{}",
                position.get_property("coords").get_property("latitude"),
                position.get_property("coords").get_property("accuracy"),
                position.get_property("coords").get_property("altitude")
            ));
            Value::Undefined
        });
        let id = geolocation
            .call_method("watchPosition", vec![success, Value::Undefined])
            .to_u32();
        crate::jsdom::drain_microtasks();
        assert_eq!(seen.borrow().as_slice(), ["31.23:8:null"]);
        geolocation.call_method("clearWatch", vec![Value::Number(id as f64)]);
        update_position(GeoPosition::new(0.0, 0.0, 1.0));
        crate::jsdom::drain_microtasks();
        assert_eq!(seen.borrow().len(), 1);
    }

    #[test]
    fn unavailable_denied_and_timeout_errors_are_distinct() {
        for (denied, timeout, expected) in [
            (false, None, 2.0),
            (true, None, 1.0),
            (false, Some(0.0), 3.0),
        ] {
            reset();
            set_permission_denied(denied);
            let code = Rc::new(RefCell::new(0.0));
            let capture = code.clone();
            geolocation_value().call_method(
                "getCurrentPosition",
                vec![
                    Value::Undefined,
                    Value::function(move |_, args| {
                        *capture.borrow_mut() = args[0].get_property("code").to_number();
                        Value::Undefined
                    }),
                    timeout
                        .map(|timeout| {
                            Value::object(HashMap::from([(
                                "timeout".to_string(),
                                Value::Number(timeout),
                            )]))
                        })
                        .unwrap_or_default(),
                ],
            );
            crate::jsdom::drain_microtasks();
            assert_eq!(*code.borrow(), expected);
        }
    }
}
