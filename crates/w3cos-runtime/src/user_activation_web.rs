//! User Activation API state shared with trusted native input dispatch.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use crate::jsdom::realm_function;
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
    USER_ACTIVATION_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
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
        let generation = crate::jsdom::realm_generation();
        let class = realm_function(generation, |_, _| {
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
    let generation = crate::jsdom::realm_generation();
    let activation = Value::object(HashMap::from([
        (
            "__w3cos_getter_isActive".into(),
            realm_function(generation, |_, _| Value::Bool(is_active())),
        ),
        (
            "__w3cos_getter_hasBeenActive".into(),
            realm_function(generation, |_, _| Value::Bool(has_been_active())),
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
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
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

    #[test]
    fn activation_getters_and_class_are_realm_owned() {
        reset();
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let old_activation = user_activation_value();
        let old_class = user_activation_class();
        old_class
            .get_property("prototype")
            .set_property("oldRealmMarker", Value::Bool(true));

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let new_activation = user_activation_value();
        let new_class = user_activation_class();
        assert!(!old_class.strict_eq(&new_class));
        assert!(
            new_class
                .get_property("prototype")
                .get_property("oldRealmMarker")
                .is_undefined()
        );
        assert!(old_class.call(Value::Undefined, vec![]).is_undefined());
        let guard = begin_transient_activation();
        assert!(old_activation.get_property("isActive").is_undefined());
        assert!(new_activation.get_property("isActive").to_bool());
        drop(guard);
        reset();
    }
}
