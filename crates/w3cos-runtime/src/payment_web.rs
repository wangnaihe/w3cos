//! Payment Request API with conservative capability results.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use w3cos_core::Value;

use crate::jsdom::{
    WeakRealmObject, disconnect_realm_class, realm_function, register_weak_realm_object,
    upgrade_realm_object,
};

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static REQUESTS: RefCell<Vec<WeakRealmObject>> = const { RefCell::new(Vec::new()) };
    static WARNING_EMITTED: Cell<bool> = const { Cell::new(false) };
}

const CLASS_NAMES: &[&str] = &[
    "PaymentAddress",
    "PaymentManager",
    "PaymentRequest",
    "PaymentResponse",
];

fn realm_payment_function(f: impl Fn(Value, Vec<Value>) -> Value + 'static) -> Value {
    realm_function(crate::jsdom::realm_generation(), f)
}

fn warning() {
    WARNING_EMITTED.with(|warned| {
        if !warned.replace(true) {
            eprintln!(
                "[w3cos] warning: PaymentRequest capability queries are available, but payment \
                 handler discovery, secure UI and authorization require a platform adapter"
            );
        }
    });
}

fn type_error(message: &str) -> ! {
    w3cos_core::throw_value(w3cos_core::error_instance(
        "TypeError",
        vec![Value::string(message)],
    ))
}

fn illegal(name: &'static str) -> Value {
    type_error(&format!("Illegal constructor: {name}"))
}

fn build_class(name: &'static str) -> Value {
    let class = if name == "PaymentRequest" {
        realm_payment_function(|this, args| {
            crate::web_events::event_target_class().call(this.clone(), Vec::new());
            let methods = args.first().cloned().unwrap_or(Value::Undefined);
            let details = args.get(1).cloned().unwrap_or(Value::Undefined);
            if methods.iter().next().is_none() {
                type_error("PaymentRequest requires at least one payment method");
            }
            if !details.is_object() || details.get_property("total").is_undefined() {
                type_error("PaymentRequest details.total is required");
            }
            let options = args.get(2).cloned().unwrap_or(Value::Undefined);
            this.set_property(
                "id",
                Value::string(&format!(
                    "w3cos-payment-{}",
                    crate::jsdom::performance_now()
                )),
            );
            this.set_property("shippingAddress", Value::Null);
            this.set_property("shippingOption", Value::Null);
            this.set_property(
                "shippingType",
                if options.get_property("requestShipping").to_bool() {
                    let kind = options.get_property("shippingType").to_js_string();
                    Value::string(if kind.is_empty() { "shipping" } else { &kind })
                } else {
                    Value::Null
                },
            );
            for event in [
                "onpaymentmethodchange",
                "onshippingaddresschange",
                "onshippingoptionchange",
            ] {
                this.set_property(event, Value::Null);
            }
            for method in ["canMakePayment", "hasEnrolledInstrument"] {
                this.set_property(
                    method,
                    realm_payment_function(|_, _| {
                        warning();
                        w3cos_core::promise::resolve(vec![Value::Bool(false)])
                    }),
                );
            }
            this.set_property(
                "show",
                realm_payment_function(|_, _| {
                    warning();
                    w3cos_core::promise::reject(vec![w3cos_core::web::dom_exception_instance(
                        "No platform payment handler is registered",
                        "NotSupportedError",
                    )])
                }),
            );
            this.set_property(
                "abort",
                realm_payment_function(|_, _| {
                    w3cos_core::promise::reject(vec![w3cos_core::web::dom_exception_instance(
                        "The payment request is not being shown",
                        "InvalidStateError",
                    )])
                }),
            );
            register_weak_realm_object(&REQUESTS, &this);
            Value::Undefined
        })
    } else {
        realm_payment_function(move |_, _| illegal(name))
    };
    class.set_property("name", Value::string(name));
    let prototype = Value::object(HashMap::new());
    prototype.set_property("constructor", class.clone());
    for member in class_members(name) {
        prototype.set_property(member, Value::Undefined);
    }
    if matches!(name, "PaymentRequest" | "PaymentResponse") {
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::web_events::event_target_class().get_property("prototype"),
        );
    }
    class.set_property("prototype", prototype);
    if name == "PaymentRequest" {
        class.set_property(
            "getSecurePaymentConfirmationCapabilities",
            realm_payment_function(|_, _| {
                warning();
                w3cos_core::promise::resolve(vec![Value::object(HashMap::new())])
            }),
        );
        class.set_property(
            "securePaymentConfirmationAvailability",
            realm_payment_function(|_, _| {
                warning();
                w3cos_core::promise::resolve(vec![Value::string("unavailable")])
            }),
        );
    }
    class
}

fn class_members(name: &str) -> &'static [&'static str] {
    match name {
        "PaymentAddress" => &[
            "addressLine",
            "city",
            "country",
            "dependentLocality",
            "organization",
            "phone",
            "postalCode",
            "recipient",
            "region",
            "sortingCode",
            "toJSON",
        ],
        "PaymentManager" => &["enableDelegations", "userHint"],
        "PaymentRequest" => &[
            "abort",
            "canMakePayment",
            "hasEnrolledInstrument",
            "id",
            "onpaymentmethodchange",
            "onshippingaddresschange",
            "onshippingoptionchange",
            "shippingAddress",
            "shippingOption",
            "shippingType",
            "show",
        ],
        "PaymentResponse" => &[
            "complete",
            "details",
            "methodName",
            "onpayerdetailchange",
            "payerEmail",
            "payerName",
            "payerPhone",
            "requestId",
            "retry",
            "shippingAddress",
            "shippingOption",
            "toJSON",
        ],
        _ => &[],
    }
}

pub fn class_for(name: &'static str) -> Value {
    CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        let class = build_class(name);
        classes.borrow_mut().insert(name.to_string(), class.clone());
        class
    })
}

pub fn reset() {
    REQUESTS.with(|requests| {
        for request in requests
            .borrow_mut()
            .drain(..)
            .filter_map(|request| upgrade_realm_object(&request))
        {
            for name in CLASS_NAMES {
                for member in class_members(name) {
                    request.set_property(member, Value::Undefined);
                }
            }
        }
    });
    let classes = CLASSES.with(|classes| std::mem::take(&mut *classes.borrow_mut()));
    for class in classes.into_values() {
        class.set_property("getSecurePaymentConfirmationCapabilities", Value::Undefined);
        class.set_property("securePaymentConfirmationAvailability", Value::Undefined);
        disconnect_realm_class(class);
    }
    WARNING_EMITTED.with(|warned| warned.set(false));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    #[test]
    fn capability_query_is_false_and_show_rejects() {
        let request = w3cos_core::class::construct(
            &class_for("PaymentRequest"),
            vec![
                Value::array(vec![Value::object(HashMap::from([(
                    "supportedMethods".into(),
                    Value::string("basic-card"),
                )]))]),
                Value::object(HashMap::from([(
                    "total".into(),
                    Value::object(HashMap::new()),
                )])),
            ],
        );
        let values = Rc::new(RefCell::new(Vec::<String>::new()));
        for (promise, rejected) in [
            (request.call_method("canMakePayment", Vec::new()), false),
            (request.call_method("show", Vec::new()), true),
        ] {
            let values = Rc::clone(&values);
            promise.call_method(
                if rejected { "catch" } else { "then" },
                vec![Value::function(move |_, args| {
                    values.borrow_mut().push(if rejected {
                        args[0].get_property("name").to_js_string()
                    } else {
                        args[0].to_js_string()
                    });
                    Value::Undefined
                })],
            );
        }
        w3cos_core::promise::drain_microtasks();
        assert_eq!(&*values.borrow(), &["false", "NotSupportedError"]);
    }

    #[test]
    fn requests_methods_callbacks_and_classes_are_realm_owned() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        let old_class = class_for("PaymentRequest");
        let request = w3cos_core::class::construct(
            &old_class,
            vec![
                Value::array(vec![Value::object(HashMap::from([(
                    "supportedMethods".into(),
                    Value::string("basic-card"),
                )]))]),
                Value::object(HashMap::from([(
                    "total".into(),
                    Value::object(HashMap::new()),
                )])),
            ],
        );

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        assert!(old_class.get_property("prototype").is_undefined());
        assert!(!old_class.strict_eq(&class_for("PaymentRequest")));
        assert!(request.get_property("show").is_undefined());
        assert!(request.get_property("abort").is_undefined());
        assert!(request.get_property("onpaymentmethodchange").is_undefined());
    }
}
