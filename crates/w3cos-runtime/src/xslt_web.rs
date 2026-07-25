//! XSLTProcessor compatibility using an explicit identity-transform fallback.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use w3cos_core::Value;

thread_local! {
    static XSLT_PROCESSOR_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static WARNING_EMITTED: Cell<bool> = const { Cell::new(false) };
}

fn warning() {
    WARNING_EMITTED.with(|warned| {
        if !warned.replace(true) {
            eprintln!(
                "[w3cos] warning: XSLTProcessor preserves parameters and returns an identity \
                 transform; executing XSLT stylesheets requires an XML/XSLT engine adapter"
            );
        }
    });
}

fn source_node(source: Value) -> Value {
    if source.get_property("nodeType").to_u32() == 9 {
        source.get_property("documentElement")
    } else {
        source
    }
}

pub fn xslt_processor_class() -> Value {
    XSLT_PROCESSOR_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|this, _| {
            let parameters = Rc::new(RefCell::new(HashMap::<String, Value>::new()));
            this.set_property("__w3cos_stylesheet", Value::Null);
            let import_target = this.clone();
            this.set_property(
                "importStylesheet",
                Value::function(move |_, args| {
                    import_target.set_property(
                        "__w3cos_stylesheet",
                        args.first().cloned().unwrap_or(Value::Null),
                    );
                    Value::Undefined
                }),
            );
            let set_parameters = Rc::clone(&parameters);
            this.set_property(
                "setParameter",
                Value::function(move |_, args| {
                    let namespace = args.first().map(Value::to_js_string).unwrap_or_default();
                    let name = args.get(1).map(Value::to_js_string).unwrap_or_default();
                    set_parameters.borrow_mut().insert(
                        format!("{namespace}\0{name}"),
                        args.get(2).cloned().unwrap_or(Value::Undefined),
                    );
                    Value::Undefined
                }),
            );
            let get_parameters = Rc::clone(&parameters);
            this.set_property(
                "getParameter",
                Value::function(move |_, args| {
                    let namespace = args.first().map(Value::to_js_string).unwrap_or_default();
                    let name = args.get(1).map(Value::to_js_string).unwrap_or_default();
                    get_parameters
                        .borrow()
                        .get(&format!("{namespace}\0{name}"))
                        .cloned()
                        .unwrap_or(Value::Null)
                }),
            );
            let remove_parameters = Rc::clone(&parameters);
            this.set_property(
                "removeParameter",
                Value::function(move |_, args| {
                    let namespace = args.first().map(Value::to_js_string).unwrap_or_default();
                    let name = args.get(1).map(Value::to_js_string).unwrap_or_default();
                    remove_parameters
                        .borrow_mut()
                        .remove(&format!("{namespace}\0{name}"));
                    Value::Undefined
                }),
            );
            let clear_parameters = Rc::clone(&parameters);
            this.set_property(
                "clearParameters",
                Value::function(move |_, _| {
                    clear_parameters.borrow_mut().clear();
                    Value::Undefined
                }),
            );
            let reset_parameters = parameters;
            let reset_target = this.clone();
            this.set_property(
                "reset",
                Value::function(move |_, _| {
                    reset_parameters.borrow_mut().clear();
                    reset_target.set_property("__w3cos_stylesheet", Value::Null);
                    Value::Undefined
                }),
            );
            this.set_property(
                "transformToFragment",
                Value::function(|_, args| {
                    warning();
                    let source = source_node(args.first().cloned().unwrap_or(Value::Undefined));
                    let document = args
                        .get(1)
                        .cloned()
                        .unwrap_or_else(crate::jsdom::document_value);
                    let fragment = document.call_method("createDocumentFragment", Vec::new());
                    if source.get_property("nodeType").to_u32() != 0 {
                        let clone =
                            document.call_method("importNode", vec![source, Value::Bool(true)]);
                        fragment.call_method("appendChild", vec![clone]);
                    }
                    fragment
                }),
            );
            this.set_property(
                "transformToDocument",
                Value::function(|_, args| {
                    warning();
                    let source = source_node(args.first().cloned().unwrap_or(Value::Undefined));
                    let name = source.get_property("localName").to_js_string();
                    let document = crate::jsdom::document_value();
                    let result = document.get_property("implementation").call_method(
                        "createDocument",
                        vec![
                            Value::string(""),
                            Value::string(if name.is_empty() { "result" } else { &name }),
                            Value::Null,
                        ],
                    );
                    result
                        .get_property("documentElement")
                        .set_property("textContent", source.get_property("textContent"));
                    result
                }),
            );
            Value::Undefined
        });
        class.set_property("name", Value::string("XSLTProcessor"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in [
            "clearParameters",
            "getParameter",
            "importStylesheet",
            "removeParameter",
            "reset",
            "setParameter",
            "transformToDocument",
            "transformToFragment",
        ] {
            prototype.set_property(member, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn reset() {
    WARNING_EMITTED.with(|warned| warned.set(false));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameters_and_identity_fragment_are_usable() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let processor = w3cos_core::class::construct(&xslt_processor_class(), Vec::new());
        processor.call_method(
            "setParameter",
            vec![Value::Null, Value::string("answer"), Value::Number(42.0)],
        );
        assert_eq!(
            processor
                .call_method("getParameter", vec![Value::Null, Value::string("answer")])
                .to_number(),
            42.0
        );
        let document = crate::jsdom::document_value();
        let source = document.call_method("createElement", vec![Value::string("item")]);
        source.set_property("textContent", Value::string("value"));
        let fragment = processor.call_method("transformToFragment", vec![source, document]);
        assert_eq!(fragment.get_property("textContent").to_js_string(), "value");
    }
}
