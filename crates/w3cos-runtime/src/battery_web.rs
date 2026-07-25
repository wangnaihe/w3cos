//! Battery Status API with host-injectable state.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Once;

use w3cos_core::Value;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BatteryState {
    pub charging: bool,
    pub charging_time: f64,
    pub discharging_time: f64,
    pub level: f64,
}

impl Default for BatteryState {
    fn default() -> Self {
        Self {
            charging: true,
            charging_time: 0.0,
            discharging_time: f64::INFINITY,
            level: 1.0,
        }
    }
}

thread_local! {
    static BATTERY_STATE: RefCell<BatteryState> = RefCell::new(BatteryState::default());
    static BATTERY_MANAGER: RefCell<Option<Value>> = const { RefCell::new(None) };
    static BATTERY_MANAGER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
}

pub fn battery_manager_class() -> Value {
    BATTERY_MANAGER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            w3cos_core::throw_value(Value::object(HashMap::from([
                ("name".into(), Value::string("TypeError")),
                (
                    "message".into(),
                    Value::string("Illegal constructor: BatteryManager"),
                ),
            ])))
        });
        class.set_property("name", Value::string("BatteryManager"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for property in [
            "charging",
            "chargingTime",
            "dischargingTime",
            "level",
            "onchargingchange",
            "onchargingtimechange",
            "ondischargingtimechange",
            "onlevelchange",
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

pub fn battery_manager_value() -> Value {
    BATTERY_MANAGER.with(|slot| {
        if let Some(manager) = slot.borrow().clone() {
            return manager;
        }
        let manager =
            w3cos_core::class::construct(&crate::web_events::event_target_class(), vec![]);
        w3cos_core::class::set_prototype_of(
            &manager,
            &battery_manager_class().get_property("prototype"),
        );
        for (name, event_handler, getter) in [
            (
                "charging",
                "onchargingchange",
                Value::function(|_, _| {
                    BATTERY_STATE.with(|state| Value::Bool(state.borrow().charging))
                }),
            ),
            (
                "chargingTime",
                "onchargingtimechange",
                Value::function(|_, _| {
                    BATTERY_STATE.with(|state| Value::Number(state.borrow().charging_time))
                }),
            ),
            (
                "dischargingTime",
                "ondischargingtimechange",
                Value::function(|_, _| {
                    BATTERY_STATE.with(|state| Value::Number(state.borrow().discharging_time))
                }),
            ),
            (
                "level",
                "onlevelchange",
                Value::function(|_, _| {
                    BATTERY_STATE.with(|state| Value::Number(state.borrow().level))
                }),
            ),
        ] {
            manager.set_property(&format!("__w3cos_getter_{name}"), getter);
            manager.set_property(event_handler, Value::Null);
        }
        *slot.borrow_mut() = Some(manager.clone());
        manager
    })
}

pub fn get_battery_value() -> Value {
    Value::function(|_, _| {
        static WARNING: Once = Once::new();
        WARNING.call_once(|| {
            eprintln!(
                "[w3cos] warning: navigator.getBattery returns a host-injectable compatibility \
                 snapshot; live battery telemetry requires a platform power adapter"
            );
        });
        w3cos_core::promise::resolve(vec![battery_manager_value()])
    })
}

fn dispatch_change(manager: &Value, event_type: &str) {
    let event = w3cos_core::class::construct(
        &crate::web_events::event_class(),
        vec![Value::string(event_type)],
    );
    manager.call_method("dispatchEvent", vec![event]);
}

/// Update battery telemetry from a platform power adapter.
pub fn update_battery_state(mut state: BatteryState) {
    state.level = state.level.clamp(0.0, 1.0);
    let previous = BATTERY_STATE.with(|current| {
        let previous = *current.borrow();
        *current.borrow_mut() = state;
        previous
    });
    let manager = battery_manager_value();
    if previous.charging != state.charging {
        dispatch_change(&manager, "chargingchange");
    }
    if previous.charging_time != state.charging_time {
        dispatch_change(&manager, "chargingtimechange");
    }
    if previous.discharging_time != state.discharging_time {
        dispatch_change(&manager, "dischargingtimechange");
    }
    if previous.level != state.level {
        dispatch_change(&manager, "levelchange");
    }
}

pub fn reset() {
    BATTERY_STATE.with(|state| *state.borrow_mut() = BatteryState::default());
    BATTERY_MANAGER.with(|manager| *manager.borrow_mut() = None);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn host_updates_getters_and_dispatches_each_changed_event() {
        reset();
        let manager = battery_manager_value();
        assert!(w3cos_core::class::instance_of(
            &manager,
            &battery_manager_class()
        ));
        let changes = Rc::new(Cell::new(0));
        for event_type in [
            "chargingchange",
            "chargingtimechange",
            "dischargingtimechange",
            "levelchange",
        ] {
            let changes_for_listener = Rc::clone(&changes);
            manager.call_method(
                "addEventListener",
                vec![
                    Value::string(event_type),
                    Value::function(move |_, _| {
                        changes_for_listener.set(changes_for_listener.get() + 1);
                        Value::Undefined
                    }),
                ],
            );
        }
        update_battery_state(BatteryState {
            charging: false,
            charging_time: f64::INFINITY,
            discharging_time: 7200.0,
            level: 0.42,
        });
        assert_eq!(changes.get(), 4);
        assert!(!manager.get_property("charging").to_bool());
        assert_eq!(
            manager.get_property("chargingTime").to_number(),
            f64::INFINITY
        );
        assert_eq!(manager.get_property("dischargingTime").to_number(), 7200.0);
        assert_eq!(manager.get_property("level").to_number(), 0.42);
    }
}
