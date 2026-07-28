//! URL Fragment Text Directives API identity and document entry point.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Once;

use w3cos_core::Value;

thread_local! {
    static CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static VALUE: RefCell<Option<Value>> = const { RefCell::new(None) };
}

pub fn fragment_directive_class() -> Value {
    let class = CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            w3cos_core::throw_value(Value::object(HashMap::from([
                ("name".into(), Value::string("TypeError")),
                (
                    "message".into(),
                    Value::string("Illegal constructor: FragmentDirective"),
                ),
            ])))
        });
        class.set_property("name", Value::string("FragmentDirective"));
        class.set_property(
            "prototype",
            Value::object(HashMap::from([("constructor".into(), class.clone())])),
        );
        *slot.borrow_mut() = Some(class.clone());
        class
    });
    // `window.document` is lazy. Install the Document prototype member when
    // the global constructor is exposed as well as when the singleton is
    // first materialized, so surface inventories see the browser shape.
    crate::dom_constructors::prototype("Document")
        .set_property("fragmentDirective", Value::Undefined);
    class
}

fn fragment_directive_value() -> Value {
    VALUE.with(|slot| {
        if let Some(value) = slot.borrow().clone() {
            return value;
        }
        let value = Value::object(HashMap::new());
        w3cos_core::class::set_prototype_of(
            &value,
            &fragment_directive_class().get_property("prototype"),
        );
        *slot.borrow_mut() = Some(value.clone());
        value
    })
}

pub fn install_document(document: &Value) {
    document.set_property(
        "__w3cos_getter_fragmentDirective",
        Value::function(|_, _| {
            static WARNING: Once = Once::new();
            WARNING.call_once(|| {
                eprintln!(
                    "[w3cos] warning: FragmentDirective identity is available, but parsing \
                     :~:text directives, automatic scrolling and document highlighting are pending"
                );
            });
            fragment_directive_value()
        }),
    );
    crate::dom_constructors::prototype("Document")
        .set_property("fragmentDirective", Value::Undefined);
}

/// Release the document-scoped fragment-directive wrapper.
pub fn reset_realm() {
    VALUE.with(|slot| {
        slot.borrow_mut().take();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_exposes_stable_fragment_directive_identity() {
        let document = crate::jsdom::document_value();
        install_document(&document);
        let first = document.get_property("fragmentDirective");
        let second = document.get_property("fragmentDirective");
        assert_eq!(first, second);
        assert!(w3cos_core::class::instance_of(
            &first,
            &fragment_directive_class()
        ));
    }
}
