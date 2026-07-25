//! Sanitizer API compatibility layer backed by the inert HTML fragment parser.

use std::cell::RefCell;
use std::collections::HashMap;

use w3cos_core::Value;

thread_local! {
    static SANITIZER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
}

pub fn sanitizer_class() -> Value {
    SANITIZER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, args| {
            let config = args.first().cloned().unwrap_or(Value::Undefined);
            let sanitizer = Value::object(HashMap::from([(
                "get".into(),
                Value::function(move |_, _| {
                    if config.is_undefined() {
                        Value::object(HashMap::new())
                    } else {
                        config.clone()
                    }
                }),
            )]));
            sanitizer.set_property(
                "sanitize",
                Value::function(|_, args| {
                    crate::jsdom::sanitized_fragment_value(
                        &args
                            .first()
                            .cloned()
                            .unwrap_or(Value::Undefined)
                            .to_js_string(),
                    )
                }),
            );
            sanitizer.set_property(
                "sanitizeFor",
                Value::function(|_, args| {
                    let tag_name = args
                        .first()
                        .cloned()
                        .unwrap_or(Value::Undefined)
                        .to_js_string();
                    let source = args.get(1).cloned().unwrap_or(Value::Undefined);
                    crate::jsdom::sanitized_element_value(&tag_name, &source.to_js_string())
                }),
            );
            w3cos_core::class::set_prototype_of(
                &sanitizer,
                &sanitizer_class().get_property("prototype"),
            );
            sanitizer
        });
        class.set_property("name", Value::string("Sanitizer"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in [
            "allowAttribute",
            "allowElement",
            "allowProcessingInstruction",
            "get",
            "removeAttribute",
            "removeElement",
            "removeProcessingInstruction",
            "removeUnsafe",
            "replaceElementWithChildren",
            "setComments",
            "setDataAttributes",
            "sanitize",
            "sanitizeFor",
        ] {
            prototype.set_property(member, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizer_removes_active_content_and_unsafe_attributes() {
        crate::jsdom::reset_bridge();
        let sanitizer = w3cos_core::class::construct(&sanitizer_class(), vec![]);
        let element = sanitizer.call_method(
            "sanitizeFor",
            vec![
                Value::string("div"),
                Value::string(
                    "<script>alert(1)</script><a onclick='bad()' href='javascript:bad()'>ok</a>",
                ),
            ],
        );
        assert!(w3cos_core::class::instance_of(
            &sanitizer,
            &sanitizer_class()
        ));
        assert!(
            element
                .call_method("querySelector", vec![Value::string("script")])
                .is_null()
        );
        let anchor = element.call_method("querySelector", vec![Value::string("a")]);
        assert!(
            anchor
                .call_method("getAttribute", vec![Value::string("onclick")])
                .is_null()
        );
        assert!(
            anchor
                .call_method("getAttribute", vec![Value::string("href")])
                .is_null()
        );
        assert_eq!(anchor.get_property("textContent"), Value::string("ok"));

        element.call_method(
            "setHTML",
            vec![Value::string("<img src='x' onerror='bad()'><b>safe</b>")],
        );
        let image = element.call_method("querySelector", vec![Value::string("img")]);
        assert!(
            image
                .call_method("getAttribute", vec![Value::string("onerror")])
                .is_null()
        );
        assert_eq!(
            element
                .call_method("querySelector", vec![Value::string("b")])
                .get_property("textContent"),
            Value::string("safe")
        );

        let document = crate::jsdom::window_value()
            .get_property("Document")
            .call_method(
                "parseHTML",
                vec![Value::string(
                    "<main><script>bad()</script><p onclick='bad()'>parsed</p></main>",
                )],
            );
        assert!(
            document
                .call_method("querySelector", vec![Value::string("script")])
                .is_null()
        );
        assert!(
            document
                .call_method("querySelector", vec![Value::string("p")])
                .call_method("getAttribute", vec![Value::string("onclick")])
                .is_null()
        );
    }
}
