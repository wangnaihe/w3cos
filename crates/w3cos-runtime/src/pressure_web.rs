//! Compute Pressure API with host-injectable CPU pressure records.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Once;

use crate::jsdom::realm_function;
use w3cos_core::Value;

thread_local! {
    static PRESSURE_OBSERVER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static PRESSURE_RECORD_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static OBSERVERS: RefCell<Vec<Value>> = const { RefCell::new(Vec::new()) };
}

fn error(name: &str, message: &str) -> Value {
    Value::object(HashMap::from([
        ("name".into(), Value::string(name)),
        ("message".into(), Value::string(message)),
    ]))
}

pub fn pressure_record_class() -> Value {
    PRESSURE_RECORD_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let generation = crate::jsdom::realm_generation();
        let class = realm_function(generation, |_, _| {
            w3cos_core::throw_value(error("TypeError", "Illegal constructor: PressureRecord"))
        });
        class.set_property("name", Value::string("PressureRecord"));
        let prototype = Value::object(HashMap::from([("constructor".into(), class.clone())]));
        for name in ["source", "state", "time"] {
            prototype.set_property(name, Value::Undefined);
        }
        prototype.set_property(
            "toJSON",
            realm_function(generation, |this, _| {
                Value::object(HashMap::from([
                    ("source".into(), this.get_property("source")),
                    ("state".into(), this.get_property("state")),
                    ("time".into(), this.get_property("time")),
                ]))
            }),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn pressure_record(source: &str, state: &str, time: f64) -> Value {
    let record = Value::object(HashMap::from([
        ("source".into(), Value::string(source)),
        ("state".into(), Value::string(state)),
        ("time".into(), Value::Number(time)),
    ]));
    w3cos_core::class::set_prototype_of(
        &record,
        &pressure_record_class().get_property("prototype"),
    );
    record
}

fn take_records(observer: &Value) -> Value {
    let records = observer.get_property("__w3cos_records");
    observer.set_property("__w3cos_records", Value::array(Vec::new()));
    records
}

fn schedule_delivery(observer: Value) {
    if observer.get_property("__w3cos_scheduled").to_bool() {
        return;
    }
    observer.set_property("__w3cos_scheduled", Value::Bool(true));
    let generation = crate::jsdom::realm_generation();
    crate::jsdom::queue_microtask_value(realm_function(generation, move |_, _| {
        observer.set_property("__w3cos_scheduled", Value::Bool(false));
        let records = take_records(&observer);
        if records.get_property("length").to_number() > 0.0 {
            observer
                .get_property("__w3cos_callback")
                .call(Value::Undefined, vec![records, observer.clone()]);
        }
        Value::Undefined
    }));
}

pub fn pressure_observer_class() -> Value {
    PRESSURE_OBSERVER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let generation = crate::jsdom::realm_generation();
        let class = realm_function(generation, |this, args| {
            let callback = args.first().cloned().unwrap_or(Value::Undefined);
            if !callback.is_function() {
                w3cos_core::throw_value(error(
                    "TypeError",
                    "PressureObserver requires a callable callback",
                ));
            }
            let options = args.get(1).cloned().unwrap_or(Value::Undefined);
            let sample_interval = options.get_property("sampleInterval").to_number();
            this.set_property("__w3cos_callback", callback);
            this.set_property("__w3cos_sources", Value::array(Vec::new()));
            this.set_property("__w3cos_records", Value::array(Vec::new()));
            this.set_property("__w3cos_scheduled", Value::Bool(false));
            this.set_property(
                "__w3cos_sample_interval",
                Value::Number(if sample_interval.is_finite() && sample_interval >= 0.0 {
                    sample_interval
                } else {
                    0.0
                }),
            );
            OBSERVERS.with(|observers| observers.borrow_mut().push(this));
            Value::Undefined
        });
        class.set_property("name", Value::string("PressureObserver"));
        class.set_property("knownSources", Value::array(vec![Value::string("cpu")]));
        let prototype = Value::object(HashMap::from([("constructor".into(), class.clone())]));
        prototype.set_property(
            "observe",
            realm_function(generation, |this, args| {
                let source = args
                    .first()
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .to_js_string();
                if source != "cpu" {
                    return w3cos_core::promise::reject(vec![error(
                        "NotSupportedError",
                        "Only the cpu pressure source is defined",
                    )]);
                }
                static WARNING: Once = Once::new();
                WARNING.call_once(|| {
                    eprintln!(
                        "[w3cos] warning: PressureObserver is active but receives records only \
                         when a host CPU-pressure telemetry adapter injects them"
                    );
                });
                let sources = this.get_property("__w3cos_sources");
                if !sources.iter().any(|item| item.to_js_string() == source) {
                    sources.call_method("push", vec![Value::string(&source)]);
                }
                w3cos_core::promise::resolve(vec![Value::Undefined])
            }),
        );
        prototype.set_property(
            "unobserve",
            realm_function(generation, |this, args| {
                let source = args
                    .first()
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .to_js_string();
                let retained = this
                    .get_property("__w3cos_sources")
                    .iter()
                    .filter(|item| item.to_js_string() != source)
                    .collect();
                this.set_property("__w3cos_sources", Value::array(retained));
                Value::Undefined
            }),
        );
        prototype.set_property(
            "disconnect",
            realm_function(generation, |this, _| {
                this.set_property("__w3cos_sources", Value::array(Vec::new()));
                this.set_property("__w3cos_records", Value::array(Vec::new()));
                Value::Undefined
            }),
        );
        prototype.set_property(
            "takeRecords",
            realm_function(generation, |this, _| take_records(&this)),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn reset_realm() {
    PRESSURE_OBSERVER_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
    PRESSURE_RECORD_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
    OBSERVERS.with(|observers| observers.borrow_mut().clear());
}

/// Inject a platform pressure reading. Returns false for unknown sources or
/// states and true when the record was accepted for observer delivery.
pub fn update_pressure(source: &str, state: &str, time: f64) -> bool {
    if source != "cpu"
        || !matches!(state, "nominal" | "fair" | "serious" | "critical")
        || !time.is_finite()
        || time < 0.0
    {
        return false;
    }
    OBSERVERS.with(|observers| {
        observers
            .borrow_mut()
            .retain(|observer| observer.is_object());
        for observer in observers.borrow().iter() {
            let observed = observer
                .get_property("__w3cos_sources")
                .iter()
                .any(|item| item.to_js_string() == source);
            if !observed {
                continue;
            }
            observer
                .get_property("__w3cos_records")
                .call_method("push", vec![pressure_record(source, state, time)]);
            schedule_delivery(observer.clone());
        }
    });
    true
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;

    #[test]
    fn host_records_are_batched_and_delivered_asynchronously() {
        reset_realm();
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let log = Rc::new(RefCell::new(Vec::new()));
        let log_for_callback = Rc::clone(&log);
        let observer = w3cos_core::class::construct(
            &pressure_observer_class(),
            vec![Value::function(move |_, args| {
                let record = args[0].get_property("0");
                log_for_callback.borrow_mut().push(format!(
                    "{}:{}:{}",
                    record.get_property("source").to_js_string(),
                    record.get_property("state").to_js_string(),
                    w3cos_core::class::instance_of(&record, &pressure_record_class())
                ));
                Value::Undefined
            })],
        );
        observer.call_method("observe", vec![Value::string("cpu")]);
        assert!(update_pressure("cpu", "serious", 42.0));
        assert!(log.borrow().is_empty());
        crate::jsdom::drain_microtasks();
        assert_eq!(&*log.borrow(), &["cpu:serious:true"]);
        assert_eq!(
            observer
                .call_method("takeRecords", vec![])
                .get_property("length")
                .to_number(),
            0.0
        );
        reset_realm();
    }

    #[test]
    fn observers_and_pending_delivery_are_realm_owned() {
        reset_realm();
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let deliveries = Rc::new(RefCell::new(0));
        let deliveries_for_callback = Rc::clone(&deliveries);
        let observer = w3cos_core::class::construct(
            &pressure_observer_class(),
            vec![Value::function(move |_, _| {
                *deliveries_for_callback.borrow_mut() += 1;
                Value::Undefined
            })],
        );
        observer.call_method("observe", vec![Value::string("cpu")]);
        assert!(update_pressure("cpu", "serious", 42.0));
        let old_class = pressure_observer_class();
        old_class
            .get_property("prototype")
            .set_property("oldRealmMarker", Value::Bool(true));

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::jsdom::drain_microtasks();
        assert_eq!(*deliveries.borrow(), 0);
        assert!(
            observer
                .call_method("observe", vec![Value::string("cpu")])
                .is_undefined()
        );
        assert!(old_class.call(Value::Undefined, vec![]).is_undefined());
        assert!(update_pressure("cpu", "critical", 43.0));
        crate::jsdom::drain_microtasks();
        assert_eq!(*deliveries.borrow(), 0);

        let new_class = pressure_observer_class();
        assert!(!old_class.strict_eq(&new_class));
        assert!(
            new_class
                .get_property("prototype")
                .get_property("oldRealmMarker")
                .is_undefined()
        );
        reset_realm();
    }
}
