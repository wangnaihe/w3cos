//! User-mediated Idle Detection and EyeDropper compatibility APIs.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::Once;

use w3cos_core::Value;

thread_local! {
    static IDLE_DETECTOR_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static EYE_DROPPER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static IDLE_PERMISSION: Cell<bool> = const { Cell::new(false) };
    static IDLE_DETECTORS: RefCell<Vec<Value>> = const { RefCell::new(Vec::new()) };
}

fn error(name: &str, message: &str) -> Value {
    w3cos_core::class::construct(
        &crate::unsupported::dom_exception_class(),
        vec![Value::string(message), Value::string(name)],
    )
}

fn warn_idle() {
    static WARNING: Once = Once::new();
    WARNING.call_once(|| {
        eprintln!(
            "[w3cos] warning: IdleDetector requires host idle-time, screen-lock and permission \
             adapters; the default permission is denied"
        );
    });
}

fn idle_start(this: Value, args: Vec<Value>) -> Value {
    let options = args.first().cloned().unwrap_or(Value::Undefined);
    let threshold = options.get_property("threshold");
    if !threshold.is_undefined() && threshold.to_number() < 60_000.0 {
        return w3cos_core::promise::reject(vec![error(
            "TypeError",
            "IdleDetector threshold must be at least 60000 milliseconds",
        )]);
    }
    let signal = options.get_property("signal");
    if !signal.is_undefined() && signal.get_property("aborted").to_bool() {
        return w3cos_core::promise::reject(vec![signal.get_property("reason")]);
    }
    if !IDLE_PERMISSION.with(Cell::get) {
        warn_idle();
        return w3cos_core::promise::reject(vec![error(
            "NotAllowedError",
            "Idle detection permission is not granted",
        )]);
    }
    this.set_property("userState", Value::string("active"));
    this.set_property("screenState", Value::string("unlocked"));
    w3cos_core::promise::resolve(vec![Value::Undefined])
}

pub fn idle_detector_class() -> Value {
    IDLE_DETECTOR_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|this, _| {
            crate::web_events::event_target_class().call(this.clone(), vec![]);
            this.set_property("userState", Value::Null);
            this.set_property("screenState", Value::Null);
            this.set_property("onchange", Value::Null);
            this.set_property(
                "start",
                Value::function(|this, args| idle_start(this, args)),
            );
            IDLE_DETECTORS.with(|detectors| detectors.borrow_mut().push(this));
            Value::Undefined
        });
        class.set_property("name", Value::string("IdleDetector"));
        class.set_property(
            "requestPermission",
            Value::function(|_, _| {
                if !IDLE_PERMISSION.with(Cell::get) {
                    warn_idle();
                }
                w3cos_core::promise::resolve(vec![Value::string(
                    if IDLE_PERMISSION.with(Cell::get) {
                        "granted"
                    } else {
                        "denied"
                    },
                )])
            }),
        );
        let prototype = Value::object(HashMap::from([
            ("constructor".into(), class.clone()),
            ("userState".into(), Value::Null),
            ("screenState".into(), Value::Null),
            ("onchange".into(), Value::Null),
            (
                "start".into(),
                Value::function(|this, args| idle_start(this, args)),
            ),
        ]));
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::web_events::event_target_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn eye_dropper_class() -> Value {
    EYE_DROPPER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let open = Value::function(|_, args| {
            let signal = args
                .first()
                .cloned()
                .unwrap_or(Value::Undefined)
                .get_property("signal");
            if !signal.is_undefined() && signal.get_property("aborted").to_bool() {
                return w3cos_core::promise::reject(vec![signal.get_property("reason")]);
            }
            static WARNING: Once = Once::new();
            WARNING.call_once(|| {
                eprintln!(
                    "[w3cos] warning: EyeDropper.open requires user activation and a host \
                     screen-color sampler"
                );
            });
            w3cos_core::promise::reject(vec![error(
                "NotSupportedError",
                "EyeDropper is unavailable without a host screen-color sampler",
            )])
        });
        let open_for_instance = open.clone();
        let class = Value::function(move |this, _| {
            this.set_property("open", open_for_instance.clone());
            Value::Undefined
        });
        class.set_property("name", Value::string("EyeDropper"));
        let prototype = Value::object(HashMap::from([
            ("constructor".into(), class.clone()),
            ("open".into(), open),
        ]));
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn set_idle_permission(granted: bool) {
    IDLE_PERMISSION.with(|permission| permission.set(granted));
}

pub fn update_idle_state(user_state: &str, screen_state: &str) {
    let user_state = if user_state == "idle" {
        "idle"
    } else {
        "active"
    };
    let screen_state = if screen_state == "locked" {
        "locked"
    } else {
        "unlocked"
    };
    IDLE_DETECTORS.with(|detectors| {
        for detector in detectors.borrow().iter() {
            if detector.get_property("userState").is_null() {
                continue;
            }
            detector.set_property("userState", Value::string(user_state));
            detector.set_property("screenState", Value::string(screen_state));
            let event = w3cos_core::class::construct(
                &crate::web_events::event_class(),
                vec![Value::string("change")],
            );
            detector.call_method("dispatchEvent", vec![event]);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    #[test]
    fn idle_detector_permission_and_host_updates_are_explicit() {
        set_idle_permission(false);
        let detector = w3cos_core::class::construct(&idle_detector_class(), vec![]);
        let log = Rc::new(RefCell::new(Vec::new()));
        let log_for_permission = Rc::clone(&log);
        idle_detector_class()
            .call_method("requestPermission", vec![])
            .call_method(
                "then",
                vec![Value::function(move |_, args| {
                    log_for_permission.borrow_mut().push(args[0].to_js_string());
                    Value::Undefined
                })],
            );
        let log_for_start = Rc::clone(&log);
        detector.call_method("start", vec![]).call_method(
            "catch",
            vec![Value::function(move |_, args| {
                log_for_start
                    .borrow_mut()
                    .push(args[0].get_property("name").to_js_string());
                Value::Undefined
            })],
        );
        crate::jsdom::drain_microtasks();
        assert_eq!(&*log.borrow(), &["denied", "NotAllowedError"]);

        set_idle_permission(true);
        detector.call_method("start", vec![]);
        crate::jsdom::drain_microtasks();
        update_idle_state("idle", "locked");
        assert_eq!(detector.get_property("userState"), Value::string("idle"));
        assert_eq!(
            detector.get_property("screenState"),
            Value::string("locked")
        );
        set_idle_permission(false);
    }

    #[test]
    fn eye_dropper_rejects_without_host_sampler() {
        let name = Rc::new(RefCell::new(String::new()));
        let name_for_handler = Rc::clone(&name);
        w3cos_core::class::construct(&eye_dropper_class(), vec![])
            .call_method("open", vec![])
            .call_method(
                "catch",
                vec![Value::function(move |_, args| {
                    *name_for_handler.borrow_mut() = args[0].get_property("name").to_js_string();
                    Value::Undefined
                })],
            );
        crate::jsdom::drain_microtasks();
        assert_eq!(&*name.borrow(), "NotSupportedError");
    }
}
