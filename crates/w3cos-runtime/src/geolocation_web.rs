//! Browser-shaped Geolocation facade.
//!
//! `navigator.geolocation` stays on the standard object. Permission, a fresh
//! cached position, and `timeout: 0` are decided on the JavaScript thread.
//! A real fix is acquired on the provider thread and delivered back through
//! the DOM event loop. Hosts can still inject a position with
//! [`update_position`]; that path never asks the platform for coordinates,
//! and the platform never invents them.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use w3cos_core::Value;

use crate::geolocation_provider::{self, Fix, FixError};

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
    /// Spec `[[isHighAccuracy]]`: the `enableHighAccuracy` hint of the request
    /// that produced this reading, not a claim that the hardware was GPS.
    pub high_accuracy: bool,
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
            high_accuracy: false,
        }
    }
}

#[derive(Clone)]
struct Watch {
    id: u32,
    job: Option<u64>,
    success: Value,
    error: Value,
}

struct OneShot {
    success: Value,
    error: Value,
}

struct PositionOptions {
    enable_high_accuracy: bool,
    timeout_ms: u32,
    maximum_age_ms: u32,
}

thread_local! {
    static CURRENT_POSITION: RefCell<Option<GeoPosition>> = const { RefCell::new(None) };
    static PERMISSION_DENIED: Cell<bool> = const { Cell::new(false) };
    static NEXT_WATCH_ID: Cell<u32> = const { Cell::new(1) };
    static NEXT_JOB_ID: Cell<u64> = const { Cell::new(1) };
    static WATCHES: RefCell<Vec<Watch>> = const { RefCell::new(Vec::new()) };
    static ONESHOTS: RefCell<HashMap<u64, OneShot>> = RefCell::new(HashMap::new());
    static JOB_WATCH: RefCell<HashMap<u64, u32>> = RefCell::new(HashMap::new());
    static GEOLOCATION_CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
}

fn class(name: &'static str, properties: &'static [&'static str]) -> Value {
    GEOLOCATION_CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        let class = Value::function(move |_, _| {
            w3cos_core::throw_value(Value::object(HashMap::from([
                ("name".into(), Value::string("TypeError")),
                (
                    "message".into(),
                    Value::string(&format!("Illegal constructor: {name}")),
                ),
            ])))
        });
        class.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for property in properties {
            prototype.set_property(property, Value::Undefined);
        }
        if name == "GeolocationPositionError" {
            for (constant, code) in [
                ("PERMISSION_DENIED", 1.0),
                ("POSITION_UNAVAILABLE", 2.0),
                ("TIMEOUT", 3.0),
            ] {
                class.set_property(constant, Value::Number(code));
                prototype.set_property(constant, Value::Number(code));
            }
        }
        class.set_property("prototype", prototype);
        classes.borrow_mut().insert(name.into(), class.clone());
        class
    })
}

pub fn geolocation_class() -> Value {
    class(
        "Geolocation",
        &["clearWatch", "getCurrentPosition", "watchPosition"],
    )
}

pub fn coordinates_class() -> Value {
    class(
        "GeolocationCoordinates",
        &[
            "accuracy",
            "altitude",
            "altitudeAccuracy",
            "heading",
            "latitude",
            "longitude",
            "speed",
            "toJSON",
        ],
    )
}

pub fn position_class() -> Value {
    class("GeolocationPosition", &["coords", "timestamp", "toJSON"])
}

pub fn position_error_class() -> Value {
    class(
        "GeolocationPositionError",
        &[
            "PERMISSION_DENIED",
            "POSITION_UNAVAILABLE",
            "TIMEOUT",
            "code",
            "message",
        ],
    )
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
    coords.set_property("toJSON", Value::function(|this, _| this));
    w3cos_core::class::set_prototype_of(&coords, &coordinates_class().get_property("prototype"));
    let value = Value::object(HashMap::from([
        ("coords".to_string(), coords),
        (
            "timestamp".to_string(),
            Value::Number(position.timestamp_ms),
        ),
    ]));
    value.set_property("toJSON", Value::function(|this, _| this));
    w3cos_core::class::set_prototype_of(&value, &position_class().get_property("prototype"));
    value
}

fn error_value(code: u32, message: &str) -> Value {
    let value = Value::object(HashMap::from([
        ("code".to_string(), Value::Number(code as f64)),
        ("message".to_string(), Value::string(message)),
        ("PERMISSION_DENIED".to_string(), Value::Number(1.0)),
        ("POSITION_UNAVAILABLE".to_string(), Value::Number(2.0)),
        ("TIMEOUT".to_string(), Value::Number(3.0)),
    ]));
    w3cos_core::class::set_prototype_of(&value, &position_error_class().get_property("prototype"));
    value
}

fn error_parts(error: FixError) -> (u32, &'static str) {
    match error {
        FixError::PermissionDenied => (1, "Geolocation permission denied"),
        FixError::Unavailable => (2, "Geolocation is unavailable on this w3cos target"),
        FixError::Timeout => (3, "Geolocation request timed out"),
    }
}

fn permission_blocked() -> bool {
    PERMISSION_DENIED.with(Cell::get) || !crate::capability_gate::is_granted("geolocation")
}

fn parse_options(value: &Value) -> PositionOptions {
    if !value.is_object() {
        return PositionOptions {
            enable_high_accuracy: false,
            timeout_ms: u32::MAX,
            maximum_age_ms: 0,
        };
    }
    let high = value.get_property("enableHighAccuracy");
    let timeout = value.get_property("timeout");
    let maximum_age = value.get_property("maximumAge");
    PositionOptions {
        enable_high_accuracy: !high.is_undefined() && high.to_bool(),
        timeout_ms: if timeout.is_undefined() {
            u32::MAX
        } else {
            timeout.to_u32()
        },
        maximum_age_ms: if maximum_age.is_undefined() {
            0
        } else {
            maximum_age.to_u32()
        },
    }
}

/// `timeout` of 0 must not arm a spinning provider deadline. The WebIDL
/// default (`0xFFFFFFFF`) waits until the platform answers.
fn provider_timeout(timeout_ms: u32) -> Option<Duration> {
    if timeout_ms == 0 || timeout_ms == u32::MAX {
        None
    } else {
        Some(Duration::from_millis(u64::from(timeout_ms)))
    }
}

fn fresh_cache(options: &PositionOptions) -> Option<GeoPosition> {
    if options.maximum_age_ms == 0 {
        return None;
    }
    let position = CURRENT_POSITION.with(|current| current.borrow().clone())?;
    if position.high_accuracy != options.enable_high_accuracy {
        return None;
    }
    let age = now_ms() - position.timestamp_ms;
    (age < f64::from(options.maximum_age_ms)).then_some(position)
}

fn alloc_watch_id() -> u32 {
    NEXT_WATCH_ID.with(|next| {
        let id = next.get();
        next.set(id.saturating_add(1));
        id
    })
}

fn alloc_job_id() -> u64 {
    NEXT_JOB_ID.with(|next| {
        let id = next.get();
        next.set(id.saturating_add(1));
        id
    })
}

fn register_watch(success: Value, error: Value, job: Option<u64>) -> u32 {
    let id = alloc_watch_id();
    if let Some(job) = job {
        JOB_WATCH.with(|jobs| jobs.borrow_mut().insert(job, id));
    }
    WATCHES.with(|watches| {
        watches.borrow_mut().push(Watch {
            id,
            job,
            success,
            error,
        });
    });
    id
}

fn watch_active(id: u32) -> bool {
    WATCHES.with(|watches| watches.borrow().iter().any(|watch| watch.id == id))
}

fn queue_callback(callback: Value, argument: Value) {
    if !callback.is_function() {
        return;
    }
    crate::jsdom::queue_microtask_value(Value::function(move |_, _| {
        callback.call(Value::Undefined, vec![argument.clone()]);
        Value::Undefined
    }));
}

fn queue_success(callback: Value, position: GeoPosition) {
    queue_callback(callback, position_value(&position));
}

fn queue_error(callback: Value, code: u32, message: &str) {
    queue_callback(callback, error_value(code, message));
}

fn queue_watch_success(id: u32, callback: Value, position: GeoPosition) {
    if !callback.is_function() {
        return;
    }
    crate::jsdom::queue_microtask_value(Value::function(move |_, _| {
        if watch_active(id) {
            callback.call(Value::Undefined, vec![position_value(&position)]);
        }
        Value::Undefined
    }));
}

fn queue_watch_error(id: u32, callback: Value, code: u32, message: &'static str) {
    if !callback.is_function() {
        return;
    }
    crate::jsdom::queue_microtask_value(Value::function(move |_, _| {
        if watch_active(id) {
            callback.call(Value::Undefined, vec![error_value(code, message)]);
        }
        Value::Undefined
    }));
}

fn remember(fix: &Fix, high_accuracy: bool) -> GeoPosition {
    let position = GeoPosition {
        latitude: fix.latitude,
        longitude: fix.longitude,
        accuracy: fix.accuracy,
        altitude: fix.altitude,
        altitude_accuracy: fix.altitude_accuracy,
        heading: fix.heading,
        speed: fix.speed,
        timestamp_ms: now_ms(),
        high_accuracy,
    };
    CURRENT_POSITION.with(|current| *current.borrow_mut() = Some(position.clone()));
    position
}

fn start_job(success: Value, error: Value, options: &PositionOptions, watch: bool) -> Value {
    let job = alloc_job_id();
    let timeout = provider_timeout(options.timeout_ms);
    if watch {
        let id = register_watch(success, error, Some(job));
        geolocation_provider::submit(job, true, options.enable_high_accuracy, timeout);
        Value::Number(f64::from(id))
    } else {
        ONESHOTS.with(|shots| {
            shots.borrow_mut().insert(job, OneShot { success, error });
        });
        geolocation_provider::submit(job, false, options.enable_high_accuracy, timeout);
        Value::Undefined
    }
}

fn acquire(success: Value, error: Value, options_value: Value, watch: bool) -> Value {
    let options = parse_options(&options_value);
    if permission_blocked() {
        queue_error(error, 1, "Geolocation permission denied");
        return if watch {
            Value::Number(f64::from(alloc_watch_id()))
        } else {
            Value::Undefined
        };
    }

    if let Some(position) = fresh_cache(&options) {
        if watch {
            let id = if geolocation_provider::available() {
                let job = alloc_job_id();
                let id = register_watch(success.clone(), error, Some(job));
                geolocation_provider::submit(
                    job,
                    true,
                    options.enable_high_accuracy,
                    provider_timeout(options.timeout_ms),
                );
                id
            } else {
                register_watch(success.clone(), error, None)
            };
            queue_watch_success(id, success, position);
            return Value::Number(f64::from(id));
        }
        queue_success(success, position);
        return Value::Undefined;
    }

    if options.timeout_ms == 0 {
        if watch {
            let id = register_watch(success, error.clone(), None);
            queue_watch_error(id, error, 3, "Geolocation request timed out");
            return Value::Number(f64::from(id));
        }
        queue_error(error, 3, "Geolocation request timed out");
        return Value::Undefined;
    }

    if geolocation_provider::available() {
        return start_job(success, error, &options, watch);
    }

    if watch {
        let id = register_watch(success, error.clone(), None);
        queue_watch_error(
            id,
            error,
            2,
            "Geolocation is unavailable on this w3cos target",
        );
        return Value::Number(f64::from(id));
    }
    queue_error(error, 2, "Geolocation is unavailable on this w3cos target");
    Value::Undefined
}

pub fn geolocation_value() -> Value {
    let value = Value::object(HashMap::new());
    value.set_property(
        "getCurrentPosition",
        Value::function(|_, args| {
            acquire(
                args.first().cloned().unwrap_or_default(),
                args.get(1).cloned().unwrap_or_default(),
                args.get(2).cloned().unwrap_or_default(),
                false,
            )
        }),
    );
    value.set_property(
        "watchPosition",
        Value::function(|_, args| {
            acquire(
                args.first().cloned().unwrap_or_default(),
                args.get(1).cloned().unwrap_or_default(),
                args.get(2).cloned().unwrap_or_default(),
                true,
            )
        }),
    );
    value.set_property(
        "clearWatch",
        Value::function(|_, args| {
            let id = args.first().cloned().unwrap_or_default().to_u32();
            let job = WATCHES.with(|watches| {
                let mut watches = watches.borrow_mut();
                let job = watches
                    .iter()
                    .find(|watch| watch.id == id)
                    .and_then(|watch| watch.job);
                watches.retain(|watch| watch.id != id);
                job
            });
            if let Some(job) = job {
                geolocation_provider::cancel(job);
                JOB_WATCH.with(|jobs| {
                    jobs.borrow_mut().remove(&job);
                });
            }
            Value::Undefined
        }),
    );
    w3cos_core::class::set_prototype_of(&value, &geolocation_class().get_property("prototype"));
    value
}

/// Supply a fresh host position and notify active JavaScript watchers.
///
/// This is an emulated significant change. It does not consult the platform
/// provider and does not re-apply `maximumAge`.
pub fn update_position(position: GeoPosition) {
    CURRENT_POSITION.with(|current| *current.borrow_mut() = Some(position.clone()));
    let watches = WATCHES.with(|watches| watches.borrow().clone());
    for watch in watches {
        if permission_blocked() {
            queue_watch_error(watch.id, watch.error, 1, "Geolocation permission denied");
        } else {
            queue_watch_success(watch.id, watch.success, position.clone());
        }
    }
}

pub fn set_permission_denied(denied: bool) {
    PERMISSION_DENIED.with(|value| value.set(denied));
}

pub fn reset() {
    CURRENT_POSITION.with(|current| *current.borrow_mut() = None);
    PERMISSION_DENIED.with(|value| value.set(false));
    WATCHES.with(|watches| watches.borrow_mut().clear());
    ONESHOTS.with(|shots| shots.borrow_mut().clear());
    JOB_WATCH.with(|jobs| jobs.borrow_mut().clear());
    NEXT_WATCH_ID.with(|next| next.set(1));
    NEXT_JOB_ID.with(|next| next.set(1));
    geolocation_provider::reset();
}

/// Deliver provider fixes onto the microtask queue. Called from the DOM drain.
pub(crate) fn poll_platform_events() -> usize {
    let events = geolocation_provider::take_events();
    let count = events.len();
    for event in events {
        match event.result {
            Ok(fix) => {
                let position = remember(&fix, event.high_accuracy);
                if event.watch {
                    deliver_watch_success(event.id, position);
                } else if let Some(shot) = take_oneshot(event.id) {
                    queue_success(shot.success, position);
                }
            }
            Err(error) => {
                let (code, message) = error_parts(error);
                if event.watch {
                    deliver_watch_error(event.id, code, message);
                } else if let Some(shot) = take_oneshot(event.id) {
                    queue_error(shot.error, code, message);
                }
            }
        }
    }
    count
}

fn take_oneshot(job: u64) -> Option<OneShot> {
    ONESHOTS.with(|shots| shots.borrow_mut().remove(&job))
}

fn watch_callbacks(job: u64) -> Option<(u32, Value, Value)> {
    let watch_id = JOB_WATCH.with(|jobs| jobs.borrow().get(&job).copied())?;
    WATCHES.with(|watches| {
        watches
            .borrow()
            .iter()
            .find(|watch| watch.id == watch_id)
            .map(|watch| (watch.id, watch.success.clone(), watch.error.clone()))
    })
}

fn deliver_watch_success(job: u64, position: GeoPosition) {
    if let Some((id, success, _)) = watch_callbacks(job) {
        queue_watch_success(id, success, position);
    }
}

fn deliver_watch_error(job: u64, code: u32, message: &'static str) {
    if let Some((id, _, error)) = watch_callbacks(job) {
        queue_watch_error(id, error, code, message);
    }
}

pub(crate) fn next_deadline() -> Option<Instant> {
    geolocation_provider::next_deadline()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    fn isolate() {
        crate::jsdom::reset_bridge();
        reset();
        crate::capability_gate::reset();
    }

    fn grant() {
        crate::capability_gate::grant("geolocation");
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

    fn capture_code() -> (Rc<RefCell<f64>>, Value) {
        let code = Rc::new(RefCell::new(0.0));
        let slot = Rc::clone(&code);
        let error = Value::function(move |_, args| {
            *slot.borrow_mut() = args
                .first()
                .map(|value| value.get_property("code").to_number())
                .unwrap_or(0.0);
            Value::Undefined
        });
        (code, error)
    }

    fn capture_fix() -> (Rc<RefCell<Vec<String>>>, Value) {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let slot = Rc::clone(&seen);
        let success = Value::function(move |_, args| {
            let position = args.first().cloned().unwrap_or_default();
            let coords = position.get_property("coords");
            slot.borrow_mut().push(format!(
                "{}:{}:{}",
                coords.get_property("latitude").to_number(),
                coords.get_property("longitude").to_number(),
                coords.get_property("accuracy").to_number()
            ));
            Value::Undefined
        });
        (seen, success)
    }

    fn options(entries: Vec<(&str, Value)>) -> Value {
        Value::object(
            entries
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
        )
    }

    fn sample_fix(latitude: f64) -> Fix {
        Fix {
            latitude,
            longitude: 121.47,
            accuracy: 8.0,
            altitude: None,
            altitude_accuracy: None,
            heading: None,
            speed: None,
        }
    }

    #[test]
    fn current_position_and_watch_have_browser_shapes() {
        isolate();
        grant();
        update_position(GeoPosition::new(31.23, 121.47, 8.0));
        let geolocation = geolocation_value();
        let seen = Rc::new(RefCell::new(Vec::new()));
        let capture = Rc::clone(&seen);
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
            .call_method(
                "watchPosition",
                vec![
                    success,
                    Value::Undefined,
                    options(vec![("maximumAge", Value::Number(10_000.0))]),
                ],
            )
            .to_u32();
        crate::jsdom::drain_microtasks();
        assert_eq!(seen.borrow().as_slice(), ["31.23:8:null"]);
        geolocation.call_method("clearWatch", vec![Value::Number(f64::from(id))]);
        update_position(GeoPosition::new(0.0, 0.0, 1.0));
        crate::jsdom::drain_microtasks();
        assert_eq!(seen.borrow().len(), 1);
    }

    #[test]
    fn permission_timeout_and_unavailable_errors_are_distinct() {
        isolate();
        let geolocation = geolocation_value();
        let (code, error) = capture_code();
        geolocation.call_method(
            "getCurrentPosition",
            vec![Value::Undefined, error, Value::Undefined],
        );
        crate::jsdom::drain_microtasks();
        assert_eq!(*code.borrow(), 1.0, "missing grant");

        isolate();
        grant();
        set_permission_denied(true);
        update_position(GeoPosition::new(1.0, 2.0, 3.0));
        let geolocation = geolocation_value();
        let (code, error) = capture_code();
        geolocation.call_method(
            "getCurrentPosition",
            vec![
                Value::Undefined,
                error,
                options(vec![("maximumAge", Value::Number(10_000.0))]),
            ],
        );
        crate::jsdom::drain_microtasks();
        assert_eq!(*code.borrow(), 1.0, "host denial overrides grant and cache");

        isolate();
        grant();
        let geolocation = geolocation_value();
        let (code, error) = capture_code();
        geolocation.call_method(
            "getCurrentPosition",
            vec![
                Value::Undefined,
                error,
                options(vec![("timeout", Value::Number(0.0))]),
            ],
        );
        crate::jsdom::drain_microtasks();
        assert_eq!(*code.borrow(), 3.0, "timeout 0 without a fresh cache");

        isolate();
        grant();
        if !geolocation_provider::platform_enabled() {
            let geolocation = geolocation_value();
            let (code, error) = capture_code();
            geolocation.call_method(
                "getCurrentPosition",
                vec![Value::Undefined, error, Value::Undefined],
            );
            crate::jsdom::drain_microtasks();
            assert_eq!(*code.borrow(), 2.0, "no provider");
        }
    }

    #[test]
    fn permission_is_checked_before_cache_and_the_provider() {
        isolate();
        update_position(GeoPosition::new(31.23, 121.47, 8.0));
        geolocation_provider::push_mock(Duration::ZERO, Ok(sample_fix(9.0)));
        let (code, error) = capture_code();
        geolocation_value().call_method(
            "getCurrentPosition",
            vec![
                Value::Undefined,
                error,
                options(vec![("maximumAge", Value::Number(60_000.0))]),
            ],
        );
        crate::jsdom::drain_microtasks();
        assert_eq!(*code.borrow(), 1.0);
        assert_eq!(geolocation_provider::mock_calls(), 0);
    }

    #[test]
    fn timeout_zero_uses_a_fresh_cache_and_does_not_touch_the_provider() {
        isolate();
        grant();
        update_position(GeoPosition::new(31.23, 121.47, 8.0));
        geolocation_provider::push_mock(Duration::ZERO, Ok(sample_fix(9.0)));
        let (seen, success) = capture_fix();
        let (code, error) = capture_code();
        geolocation_value().call_method(
            "getCurrentPosition",
            vec![
                success,
                error,
                options(vec![
                    ("timeout", Value::Number(0.0)),
                    ("maximumAge", Value::Number(60_000.0)),
                ]),
            ],
        );
        crate::jsdom::drain_microtasks();
        assert_eq!(seen.borrow().as_slice(), ["31.23:121.47:8"]);
        assert_eq!(*code.borrow(), 0.0);
        assert_eq!(geolocation_provider::mock_calls(), 0);
    }

    #[test]
    fn maximum_age_cache_respects_age_and_accuracy() {
        isolate();
        grant();
        update_position(GeoPosition::new(31.23, 121.47, 8.0));
        geolocation_provider::push_mock(Duration::ZERO, Ok(sample_fix(9.0)));
        let (seen, success) = capture_fix();
        geolocation_value().call_method(
            "getCurrentPosition",
            vec![
                success,
                Value::Undefined,
                options(vec![("maximumAge", Value::Number(60_000.0))]),
            ],
        );
        crate::jsdom::drain_microtasks();
        assert_eq!(seen.borrow().as_slice(), ["31.23:121.47:8"]);
        assert_eq!(geolocation_provider::mock_calls(), 0);
        // The unused scripted step must not leak into the next acquisition.
        geolocation_provider::reset();

        let mut stale = GeoPosition::new(31.23, 121.47, 8.0);
        stale.timestamp_ms -= 10_000.0;
        update_position(stale);
        geolocation_provider::push_mock(Duration::ZERO, Ok(sample_fix(4.0)));
        let (seen, success) = capture_fix();
        geolocation_value().call_method(
            "getCurrentPosition",
            vec![
                success,
                Value::Undefined,
                options(vec![
                    ("maximumAge", Value::Number(1_000.0)),
                    ("timeout", Value::Number(2_000.0)),
                ]),
            ],
        );
        assert!(pump_until(|| !seen.borrow().is_empty()));
        assert_eq!(seen.borrow().as_slice(), ["4:121.47:8"]);
        assert_eq!(geolocation_provider::mock_calls(), 1);

        update_position(GeoPosition::new(31.23, 121.47, 8.0));
        geolocation_provider::push_mock(Duration::ZERO, Ok(sample_fix(7.0)));
        let (seen, success) = capture_fix();
        geolocation_value().call_method(
            "getCurrentPosition",
            vec![
                success,
                Value::Undefined,
                options(vec![
                    ("enableHighAccuracy", Value::Bool(true)),
                    ("maximumAge", Value::Number(60_000.0)),
                    ("timeout", Value::Number(2_000.0)),
                ]),
            ],
        );
        assert!(pump_until(|| !seen.borrow().is_empty()));
        assert_eq!(seen.borrow().as_slice(), ["7:121.47:8"]);
        assert_eq!(
            geolocation_provider::mock_accuracies().last().copied(),
            Some(true)
        );
    }

    #[test]
    fn mock_provider_honors_timeout_success_and_unavailable() {
        isolate();
        grant();
        geolocation_provider::push_mock(Duration::from_millis(400), Ok(sample_fix(1.0)));
        let (code, error) = capture_code();
        geolocation_value().call_method(
            "getCurrentPosition",
            vec![
                Value::Undefined,
                error,
                options(vec![("timeout", Value::Number(40.0))]),
            ],
        );
        assert!(pump_until(|| *code.borrow() != 0.0));
        assert_eq!(*code.borrow(), 3.0);

        geolocation_provider::push_mock(Duration::ZERO, Ok(sample_fix(2.0)));
        let (seen, success) = capture_fix();
        geolocation_value().call_method(
            "getCurrentPosition",
            vec![
                success,
                Value::Undefined,
                options(vec![
                    ("enableHighAccuracy", Value::Bool(false)),
                    ("timeout", Value::Number(2_000.0)),
                ]),
            ],
        );
        assert!(pump_until(|| !seen.borrow().is_empty()));
        assert_eq!(seen.borrow().as_slice(), ["2:121.47:8"]);
        assert_eq!(
            geolocation_provider::mock_accuracies().last().copied(),
            Some(false)
        );

        geolocation_provider::push_mock(Duration::ZERO, Err(FixError::Unavailable));
        let (code, error) = capture_code();
        geolocation_value().call_method(
            "getCurrentPosition",
            vec![
                Value::Undefined,
                error,
                options(vec![("timeout", Value::Number(2_000.0))]),
            ],
        );
        assert!(pump_until(|| *code.borrow() != 0.0));
        assert_eq!(*code.borrow(), 2.0);
    }

    #[test]
    fn watch_receives_mock_fixes_until_clear_watch() {
        isolate();
        grant();
        geolocation_provider::push_mock(Duration::ZERO, Ok(sample_fix(1.0)));
        geolocation_provider::push_mock(Duration::from_millis(250), Ok(sample_fix(2.0)));
        let (seen, success) = capture_fix();
        let id = geolocation_value().call_method(
            "watchPosition",
            vec![
                success,
                Value::Undefined,
                options(vec![("timeout", Value::Number(2_000.0))]),
            ],
        );
        assert!(pump_until(|| seen.borrow().len() == 1));
        geolocation_value().call_method("clearWatch", vec![id]);
        std::thread::sleep(Duration::from_millis(80));
        crate::jsdom::drain_microtasks();
        assert_eq!(seen.borrow().as_slice(), ["1:121.47:8"]);
    }

    #[test]
    fn clear_watch_before_drain_suppresses_the_cached_watch_callback() {
        isolate();
        grant();
        update_position(GeoPosition::new(31.23, 121.47, 8.0));
        let (seen, success) = capture_fix();
        let geolocation = geolocation_value();
        geolocation.call_method(
            "getCurrentPosition",
            vec![
                success.clone(),
                Value::Undefined,
                options(vec![("maximumAge", Value::Number(1_000.0))]),
            ],
        );
        let id = geolocation.call_method(
            "watchPosition",
            vec![
                success,
                Value::Undefined,
                options(vec![("maximumAge", Value::Number(1_000.0))]),
            ],
        );
        geolocation.call_method("clearWatch", vec![id]);
        crate::jsdom::drain_microtasks();
        assert_eq!(seen.borrow().as_slice(), ["31.23:121.47:8"]);
    }

    #[test]
    fn hardware_provider_returns_a_real_fix_or_an_honest_error() {
        if std::env::var_os("W3COS_GEOLOCATION_DEVICE_TESTS").is_none_or(|value| value != "1") {
            return;
        }
        isolate();
        if !geolocation_provider::platform_enabled() {
            return;
        }
        grant();
        let (seen, success) = capture_fix();
        let (code, error) = capture_code();
        geolocation_value().call_method(
            "getCurrentPosition",
            vec![
                success,
                error,
                options(vec![
                    ("timeout", Value::Number(8_000.0)),
                    ("maximumAge", Value::Number(0.0)),
                ]),
            ],
        );
        let started = std::time::Instant::now();
        while started.elapsed() < Duration::from_millis(8_500) {
            crate::jsdom::drain_microtasks();
            if !seen.borrow().is_empty() || *code.borrow() != 0.0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        let honest = !seen.borrow().is_empty() || matches!(*code.borrow(), 1.0 | 2.0 | 3.0);
        assert!(
            honest,
            "expected a finite fix or error 1/2/3, saw {:?} code {}",
            seen.borrow(),
            code.borrow()
        );
        if let Some(reading) = seen.borrow().first() {
            let mut parts = reading.split(':');
            let latitude: f64 = parts.next().unwrap().parse().unwrap();
            let longitude: f64 = parts.next().unwrap().parse().unwrap();
            let accuracy: f64 = parts.next().unwrap().parse().unwrap();
            assert!(latitude.is_finite() && (-90.0..=90.0).contains(&latitude));
            assert!(longitude.is_finite() && (-180.0..=180.0).contains(&longitude));
            assert!(accuracy.is_finite() && accuracy >= 0.0);
        }
    }
}
