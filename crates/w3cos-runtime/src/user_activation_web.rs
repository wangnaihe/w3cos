//! User Activation API state shared with trusted native input dispatch.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use w3cos_core::Value;

thread_local! {
    static ACTIVE_DEPTH: Cell<u32> = const { Cell::new(0) };
    static HAS_BEEN_ACTIVE: Cell<bool> = const { Cell::new(false) };
    static USER_ACTIVATION_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
}

pub(crate) struct TransientActivationGuard;

pub(crate) fn begin_transient_activation() -> TransientActivationGuard {
    ACTIVE_DEPTH.with(|depth| depth.set(depth.get().saturating_add(1)));
    HAS_BEEN_ACTIVE.with(|active| active.set(true));
    TransientActivationGuard
}

impl Drop for TransientActivationGuard {
    fn drop(&mut self) {
        ACTIVE_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

pub(crate) fn reset() {
    ACTIVE_DEPTH.with(|depth| depth.set(0));
    HAS_BEEN_ACTIVE.with(|active| active.set(false));
}

fn is_active() -> bool {
    ACTIVE_DEPTH.with(|depth| depth.get() > 0)
}

fn has_been_active() -> bool {
    HAS_BEEN_ACTIVE.with(Cell::get)
}

pub fn user_activation_class() -> Value {
    USER_ACTIVATION_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            w3cos_core::throw_value(Value::object(HashMap::from([
                ("name".into(), Value::string("TypeError")),
                (
                    "message".into(),
                    Value::string("Illegal constructor: UserActivation"),
                ),
            ])))
        });
        class.set_property("name", Value::string("UserActivation"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property("hasBeenActive", Value::Undefined);
        prototype.set_property("isActive", Value::Undefined);
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn user_activation_value() -> Value {
    let activation = Value::object(HashMap::from([
        (
            "__w3cos_getter_isActive".into(),
            Value::function(|_, _| Value::Bool(is_active())),
        ),
        (
            "__w3cos_getter_hasBeenActive".into(),
            Value::function(|_, _| Value::Bool(has_been_active())),
        ),
    ]));
    w3cos_core::class::set_prototype_of(
        &activation,
        &user_activation_class().get_property("prototype"),
    );
    activation
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_and_sticky_activation_lifecycle() {
        reset();
        let activation = user_activation_value();
        assert!(w3cos_core::class::instance_of(
            &activation,
            &user_activation_class()
        ));
        assert!(!activation.get_property("isActive").to_bool());
        assert!(!activation.get_property("hasBeenActive").to_bool());

        let first = begin_transient_activation();
        assert!(activation.get_property("isActive").to_bool());
        assert!(activation.get_property("hasBeenActive").to_bool());
        let second = begin_transient_activation();
        drop(second);
        assert!(activation.get_property("isActive").to_bool());
        drop(first);
        assert!(!activation.get_property("isActive").to_bool());
        assert!(activation.get_property("hasBeenActive").to_bool());

        reset();
        assert!(!activation.get_property("hasBeenActive").to_bool());
    }
}
