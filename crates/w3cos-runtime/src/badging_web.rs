//! Badging API compatibility state and host fallback.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Once;

use w3cos_core::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Badge {
    Flag,
    Number(u64),
}

thread_local! {
    static APP_BADGE: RefCell<Option<Badge>> = const { RefCell::new(None) };
}

fn error(name: &str, message: &str) -> Value {
    Value::object(HashMap::from([
        ("name".into(), Value::string(name)),
        ("message".into(), Value::string(message)),
    ]))
}

fn warn_host_adapter() {
    static WARNING: Once = Once::new();
    WARNING.call_once(|| {
        eprintln!(
            "[w3cos] warning: the Badging API updates compatibility state only; \
             displaying a badge on the host application icon requires a platform adapter"
        );
    });
}

pub fn set_app_badge_value() -> Value {
    Value::function(|_, args| {
        let number = args
            .first()
            .cloned()
            .unwrap_or(Value::Number(0.0))
            .to_number();
        if !number.is_finite() || number < 0.0 {
            return w3cos_core::promise::reject(vec![error(
                "TypeError",
                "badge contents must be a non-negative finite number",
            )]);
        }
        let badge = if number < 1.0 {
            Badge::Flag
        } else {
            Badge::Number(number.trunc().min(u64::MAX as f64) as u64)
        };
        APP_BADGE.with(|state| *state.borrow_mut() = Some(badge));
        warn_host_adapter();
        w3cos_core::promise::resolve(vec![Value::Undefined])
    })
}

pub fn clear_app_badge_value() -> Value {
    Value::function(|_, _| {
        APP_BADGE.with(|state| *state.borrow_mut() = None);
        warn_host_adapter();
        w3cos_core::promise::resolve(vec![Value::Undefined])
    })
}

pub fn current_badge() -> Option<Badge> {
    APP_BADGE.with(|state| *state.borrow())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn badge_state_and_promise_lifecycle_are_compatible() {
        clear_app_badge_value().call(Value::Undefined, vec![]);
        assert_eq!(current_badge(), None);

        let resolved = Rc::new(Cell::new(false));
        let resolved_for_then = Rc::clone(&resolved);
        set_app_badge_value()
            .call(Value::Undefined, vec![Value::Number(7.8)])
            .call_method(
                "then",
                vec![Value::function(move |_, _| {
                    resolved_for_then.set(true);
                    Value::Undefined
                })],
            );
        w3cos_core::promise::drain_microtasks();
        assert!(resolved.get());
        assert_eq!(current_badge(), Some(Badge::Number(7)));

        set_app_badge_value().call(Value::Undefined, vec![]);
        assert_eq!(current_badge(), Some(Badge::Flag));

        let rejected = Rc::new(Cell::new(false));
        let rejected_for_catch = Rc::clone(&rejected);
        set_app_badge_value()
            .call(Value::Undefined, vec![Value::Number(-1.0)])
            .call_method(
                "catch",
                vec![Value::function(move |_, args| {
                    rejected_for_catch
                        .set(args[0].get_property("name").to_js_string() == "TypeError");
                    Value::Undefined
                })],
            );
        w3cos_core::promise::drain_microtasks();
        assert!(rejected.get());

        clear_app_badge_value().call(Value::Undefined, vec![]);
        assert_eq!(current_badge(), None);
    }
}
