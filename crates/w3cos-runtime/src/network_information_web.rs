//! Network Information API static compatibility snapshot.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Once;

use w3cos_core::Value;

thread_local! {
    static NETWORK_INFORMATION_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
}

pub fn network_information_class() -> Value {
    NETWORK_INFORMATION_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            w3cos_core::throw_value(Value::object(HashMap::from([
                ("name".into(), Value::string("TypeError")),
                (
                    "message".into(),
                    Value::string("Illegal constructor: NetworkInformation"),
                ),
            ])))
        });
        class.set_property("name", Value::string("NetworkInformation"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for property in ["downlink", "effectiveType", "onchange", "rtt", "saveData"] {
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

pub fn network_information_value() -> Value {
    static WARNING: Once = Once::new();
    WARNING.call_once(|| {
        eprintln!(
            "[w3cos] warning: navigator.connection exposes a static compatibility snapshot; \
             live connection type, quality estimates and change events require a host \
             network-information adapter"
        );
    });
    let information =
        w3cos_core::class::construct(&crate::web_events::event_target_class(), vec![]);
    w3cos_core::class::set_prototype_of(
        &information,
        &network_information_class().get_property("prototype"),
    );
    for (name, value) in [
        ("type", Value::string("unknown")),
        ("effectiveType", Value::string("4g")),
        ("downlink", Value::Number(10.0)),
        ("downlinkMax", Value::Number(f64::INFINITY)),
        ("rtt", Value::Number(0.0)),
        ("saveData", Value::Bool(false)),
        ("onchange", Value::Null),
    ] {
        information.set_property(name, value);
    }
    information
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_has_standard_identity_and_event_target_shape() {
        let information = network_information_value();
        assert!(w3cos_core::class::instance_of(
            &information,
            &network_information_class()
        ));
        assert_eq!(information.get_property("type").to_js_string(), "unknown");
        assert_eq!(
            information.get_property("effectiveType").to_js_string(),
            "4g"
        );
        assert_eq!(information.get_property("downlink").to_number(), 10.0);
        assert_eq!(information.get_property("rtt").to_number(), 0.0);
        assert!(!information.get_property("saveData").to_bool());
        assert!(information.get_property("addEventListener").is_function());
    }
}
