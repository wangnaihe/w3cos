//! Shape Detection API barcode facade.
//!
//! The Web-facing contract is available even when the host has not registered
//! a native detector. Detection then resolves to an empty result and emits a
//! one-time compatibility warning.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Once;

use crate::jsdom::realm_function;
use w3cos_core::Value;

thread_local! {
    static BARCODE_DETECTOR_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
}

const FORMATS: &[&str] = &[
    "aztec",
    "code_128",
    "code_39",
    "code_93",
    "codabar",
    "data_matrix",
    "ean_13",
    "ean_8",
    "itf",
    "pdf417",
    "qr_code",
    "unknown",
    "upc_a",
    "upc_e",
];

fn error(message: &str) -> Value {
    Value::object(HashMap::from([
        ("name".into(), Value::string("TypeError")),
        ("message".into(), Value::string(message)),
    ]))
}

fn parse_formats(options: &Value) -> Result<Vec<Value>, Value> {
    if options.is_undefined() {
        return Ok(Vec::new());
    }
    if !options.is_object() {
        return Err(error("BarcodeDetector options must be an object"));
    }
    let formats = options.get_property("formats");
    if formats.is_undefined() {
        return Ok(Vec::new());
    }
    let mut parsed = Vec::new();
    for format in formats.iter() {
        let name = format.to_js_string();
        if !FORMATS.contains(&name.as_str()) {
            return Err(error(&format!("Unknown barcode format: {name}")));
        }
        if !parsed
            .iter()
            .any(|value: &Value| value.to_js_string() == name)
        {
            parsed.push(Value::string(&name));
        }
    }
    Ok(parsed)
}

pub fn barcode_detector_class() -> Value {
    BARCODE_DETECTOR_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let generation = crate::jsdom::realm_generation();
        let class = realm_function(generation, |this, args| {
            let options = args.first().cloned().unwrap_or(Value::Undefined);
            let formats = match parse_formats(&options) {
                Ok(formats) => formats,
                Err(error) => w3cos_core::throw_value(error),
            };
            this.set_property("__w3cos_formats", Value::array(formats));
            Value::Undefined
        });
        class.set_property("name", Value::string("BarcodeDetector"));
        class.set_property(
            "getSupportedFormats",
            realm_function(generation, |_, _| {
                w3cos_core::promise::resolve(vec![Value::array(Vec::new())])
            }),
        );
        let prototype = Value::object(HashMap::from([("constructor".into(), class.clone())]));
        prototype.set_property(
            "detect",
            realm_function(generation, |_, args| {
                let source = args.first().cloned().unwrap_or(Value::Undefined);
                if source.is_undefined() || source.is_null() {
                    return w3cos_core::promise::reject(vec![error(
                        "BarcodeDetector.detect requires an image source",
                    )]);
                }
                static WARNING: Once = Once::new();
                WARNING.call_once(|| {
                    eprintln!(
                        "[w3cos] warning: BarcodeDetector has no native image-analysis adapter; \
                         detect() returns an empty compatible result"
                    );
                });
                w3cos_core::promise::resolve(vec![Value::array(Vec::new())])
            }),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn reset_realm() {
    BARCODE_DETECTOR_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;

    #[test]
    fn detector_exposes_promises_and_rejects_missing_sources() {
        reset_realm();
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let class = barcode_detector_class();
        let detector = w3cos_core::class::construct(
            &class,
            vec![Value::object(HashMap::from([(
                "formats".into(),
                Value::array(vec![Value::string("qr_code")]),
            )]))],
        );
        assert!(w3cos_core::class::instance_of(&detector, &class));

        let log = Rc::new(RefCell::new(Vec::new()));
        let log_for_detect = Rc::clone(&log);
        detector
            .call_method("detect", vec![Value::object(HashMap::new())])
            .call_method(
                "then",
                vec![Value::function(move |_, args| {
                    log_for_detect
                        .borrow_mut()
                        .push(args[0].get_property("length").to_js_string());
                    Value::Undefined
                })],
            );
        let log_for_error = Rc::clone(&log);
        detector.call_method("detect", vec![]).call_method(
            "catch",
            vec![Value::function(move |_, args| {
                log_for_error
                    .borrow_mut()
                    .push(args[0].get_property("name").to_js_string());
                Value::Undefined
            })],
        );
        crate::jsdom::drain_microtasks();
        assert_eq!(&*log.borrow(), &["0", "TypeError"]);
        reset_realm();
    }

    #[test]
    fn detector_entry_points_are_realm_owned() {
        reset_realm();
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let old_class = barcode_detector_class();
        let detector = w3cos_core::class::construct(&old_class, vec![]);
        old_class
            .get_property("prototype")
            .set_property("oldRealmMarker", Value::Bool(true));

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let new_class = barcode_detector_class();
        assert!(!old_class.strict_eq(&new_class));
        assert!(
            new_class
                .get_property("prototype")
                .get_property("oldRealmMarker")
                .is_undefined()
        );
        assert!(old_class.call(Value::Undefined, vec![]).is_undefined());
        assert!(
            old_class
                .get_property("getSupportedFormats")
                .call(Value::Undefined, vec![])
                .is_undefined()
        );
        assert!(
            detector
                .call_method("detect", vec![Value::object(HashMap::new())])
                .is_undefined()
        );
        assert!(
            w3cos_core::class::construct(&new_class, vec![])
                .call_method("detect", vec![Value::object(HashMap::new())])
                .is_object()
        );
        reset_realm();
    }
}
