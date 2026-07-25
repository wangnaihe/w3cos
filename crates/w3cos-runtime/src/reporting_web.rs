//! Reporting API and `reportError()` compatibility surfaces.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use w3cos_core::Value;

struct ObserverState {
    callback: Value,
    observer: RefCell<Value>,
    types: Vec<String>,
    records: RefCell<Vec<Value>>,
    active: Cell<bool>,
    scheduled: Cell<bool>,
    buffered: bool,
}

thread_local! {
    static OBSERVERS: RefCell<Vec<Rc<ObserverState>>> = const { RefCell::new(Vec::new()) };
    static BUFFERED_REPORTS: RefCell<Vec<Value>> = const { RefCell::new(Vec::new()) };
    static REPORT_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static REPORTING_OBSERVER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static REPORT_BODY_CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static REPORT_ERROR_WARNING_EMITTED: Cell<bool> = const { Cell::new(false) };
}

fn report_body_members(name: &str) -> &'static [&'static str] {
    match name {
        "CSPViolationReportBody" => &[
            "blockedURL",
            "columnNumber",
            "disposition",
            "documentURL",
            "effectiveDirective",
            "lineNumber",
            "originalPolicy",
            "referrer",
            "sample",
            "sourceFile",
            "statusCode",
        ],
        "IntegrityViolationReportBody" => {
            &["blockedURL", "destination", "documentURL", "reportOnly"]
        }
        _ => &[],
    }
}

pub fn report_body_class(name: &str) -> Value {
    REPORT_BODY_CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        let Some(name) = ["CSPViolationReportBody", "IntegrityViolationReportBody"]
            .into_iter()
            .find(|candidate| candidate == &name)
        else {
            return Value::Undefined;
        };
        let class = Value::function(move |_, _| {
            w3cos_core::throw_value(w3cos_core::error_instance(
                "TypeError",
                vec![Value::string(&format!("Illegal constructor: {name}"))],
            ))
        });
        class.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in report_body_members(name) {
            prototype.set_property(member, Value::Undefined);
        }
        prototype.set_property("toJSON", Value::Undefined);
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::compat_web::class("ReportBody").get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        classes.borrow_mut().insert(name.to_string(), class.clone());
        class
    })
}

fn structured_report_body(name: &str, input: Value) -> Value {
    let mut properties = HashMap::new();
    for member in report_body_members(name) {
        let supplied = input.get_property(member);
        let value = if !supplied.is_undefined() {
            supplied
        } else if matches!(*member, "columnNumber" | "lineNumber" | "statusCode") {
            Value::Number(0.0)
        } else if *member == "reportOnly" {
            Value::Bool(false)
        } else {
            Value::string("")
        };
        properties.insert((*member).to_string(), value);
    }
    let body = Value::object(properties);
    let body_for_json = body.clone();
    let body_name = name.to_string();
    body.set_property(
        "toJSON",
        Value::function(move |_, _| {
            let mut snapshot = HashMap::new();
            for member in report_body_members(&body_name) {
                snapshot.insert((*member).to_string(), body_for_json.get_property(member));
            }
            Value::object(snapshot)
        }),
    );
    w3cos_core::class::set_prototype_of(&body, &report_body_class(name).get_property("prototype"));
    body
}

fn report_value(report_type: &str, url: &str, body: Value) -> Value {
    let body = match report_type {
        "csp-violation" => structured_report_body("CSPViolationReportBody", body),
        "integrity-violation" => structured_report_body("IntegrityViolationReportBody", body),
        _ => body,
    };
    let report = Value::object(HashMap::from([
        ("type".into(), Value::string(report_type)),
        ("url".into(), Value::string(url)),
        ("body".into(), body),
    ]));
    let report_for_json = report.clone();
    report.set_property(
        "toJSON",
        Value::function(move |_, _| {
            Value::object(HashMap::from([
                ("type".into(), report_for_json.get_property("type")),
                ("url".into(), report_for_json.get_property("url")),
                ("body".into(), report_for_json.get_property("body")),
            ]))
        }),
    );
    w3cos_core::class::set_prototype_of(&report, &report_class().get_property("prototype"));
    report
}

fn accepts(state: &ObserverState, report: &Value) -> bool {
    state.types.is_empty()
        || state
            .types
            .iter()
            .any(|kind| kind == &report.get_property("type").to_js_string())
}

fn schedule_delivery(state: Rc<ObserverState>) {
    if state.scheduled.replace(true) {
        return;
    }
    crate::jsdom::queue_microtask_value(Value::function(move |_, _| {
        state.scheduled.set(false);
        if !state.active.get() {
            return Value::Undefined;
        }
        let records = std::mem::take(&mut *state.records.borrow_mut());
        if records.is_empty() {
            return Value::Undefined;
        }
        state.callback.call(
            Value::Undefined,
            vec![Value::array(records), state.observer.borrow().clone()],
        );
        Value::Undefined
    }));
}

/// Queue a browser-generated report for active observers.
pub fn queue_report(report_type: &str, url: &str, body: Value) {
    let report = report_value(report_type, url, body);
    BUFFERED_REPORTS.with(|reports| {
        let mut reports = reports.borrow_mut();
        reports.push(report.clone());
        if reports.len() > 100 {
            reports.remove(0);
        }
    });
    OBSERVERS.with(|observers| {
        for state in observers.borrow().iter() {
            if state.active.get() && accepts(state, &report) {
                state.records.borrow_mut().push(report.clone());
                schedule_delivery(Rc::clone(state));
            }
        }
    });
}

pub fn report_class() -> Value {
    REPORT_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            w3cos_core::throw_value(Value::object(HashMap::from([
                ("name".into(), Value::string("TypeError")),
                (
                    "message".into(),
                    Value::string("Illegal constructor: Report"),
                ),
            ])))
        });
        class.set_property("name", Value::string("Report"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn reporting_observer_class() -> Value {
    REPORTING_OBSERVER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, args| {
            let callback = args.first().cloned().unwrap_or(Value::Undefined);
            if !callback.is_function() {
                w3cos_core::throw_value(Value::object(HashMap::from([
                    ("name".into(), Value::string("TypeError")),
                    (
                        "message".into(),
                        Value::string("ReportingObserver requires a callback"),
                    ),
                ])));
            }
            let options = args.get(1).cloned().unwrap_or(Value::Undefined);
            let types = options
                .get_property("types")
                .iter()
                .map(|value| value.to_js_string())
                .collect();
            let state = Rc::new(ObserverState {
                callback,
                observer: RefCell::new(Value::Undefined),
                types,
                records: RefCell::new(Vec::new()),
                active: Cell::new(false),
                scheduled: Cell::new(false),
                buffered: options.get_property("buffered").to_bool(),
            });
            let observe_state = Rc::clone(&state);
            let disconnect_state = Rc::clone(&state);
            let take_state = Rc::clone(&state);
            let observer = Value::object(HashMap::from([
                (
                    "observe".into(),
                    Value::function(move |_, _| {
                        let was_active = observe_state.active.replace(true);
                        if !was_active && observe_state.buffered {
                            BUFFERED_REPORTS.with(|reports| {
                                observe_state.records.borrow_mut().extend(
                                    reports
                                        .borrow()
                                        .iter()
                                        .filter(|report| accepts(&observe_state, report))
                                        .cloned(),
                                );
                            });
                            if !observe_state.records.borrow().is_empty() {
                                schedule_delivery(Rc::clone(&observe_state));
                            }
                        }
                        Value::Undefined
                    }),
                ),
                (
                    "disconnect".into(),
                    Value::function(move |_, _| {
                        disconnect_state.active.set(false);
                        disconnect_state.records.borrow_mut().clear();
                        Value::Undefined
                    }),
                ),
                (
                    "takeRecords".into(),
                    Value::function(move |_, _| {
                        Value::array(std::mem::take(&mut *take_state.records.borrow_mut()))
                    }),
                ),
            ]));
            w3cos_core::class::set_prototype_of(
                &observer,
                &reporting_observer_class().get_property("prototype"),
            );
            *state.observer.borrow_mut() = observer.clone();
            OBSERVERS.with(|observers| observers.borrow_mut().push(state));
            observer
        });
        class.set_property("name", Value::string("ReportingObserver"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for method in ["disconnect", "observe", "takeRecords"] {
            prototype.set_property(method, Value::function(|_, _| Value::Undefined));
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn report_error_value() -> Value {
    Value::function(|_, args| {
        REPORT_ERROR_WARNING_EMITTED.with(|warned| {
            if !warned.replace(true) {
                eprintln!(
                    "[w3cos] warning: reportError dispatches ErrorEvent within this runtime; \
                     host crash and native console reporting remain pending"
                );
            }
        });
        let error = args.first().cloned().unwrap_or(Value::Undefined);
        let message = if error.get_property("message").is_undefined() {
            error.to_js_string()
        } else {
            error.get_property("message").to_js_string()
        };
        let window = crate::jsdom::window_value();
        let event = w3cos_core::class::construct(
            &window.get_property("ErrorEvent"),
            vec![
                Value::string("error"),
                Value::object(HashMap::from([
                    ("message".into(), Value::string(&message)),
                    ("error".into(), error),
                    ("filename".into(), Value::string("")),
                    ("lineno".into(), Value::Number(0.0)),
                    ("colno".into(), Value::Number(0.0)),
                ])),
            ],
        );
        window.call_method("dispatchEvent", vec![event]);
        Value::Undefined
    })
}

pub fn reset() {
    OBSERVERS.with(|observers| observers.borrow_mut().clear());
    BUFFERED_REPORTS.with(|reports| reports.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observer_filters_buffers_delivers_and_takes_records() {
        crate::jsdom::reset_bridge();
        queue_report(
            "deprecation",
            "w3cos://app",
            Value::object(HashMap::from([("id".into(), Value::string("legacy-api"))])),
        );
        let delivered = Rc::new(RefCell::new(Vec::<Value>::new()));
        let delivered_for_callback = Rc::clone(&delivered);
        let observer = w3cos_core::class::construct(
            &reporting_observer_class(),
            vec![
                Value::function(move |_, args| {
                    delivered_for_callback.borrow_mut().extend(args[0].iter());
                    Value::Undefined
                }),
                Value::object(HashMap::from([
                    (
                        "types".into(),
                        Value::array(vec![Value::string("deprecation")]),
                    ),
                    ("buffered".into(), Value::Bool(true)),
                ])),
            ],
        );
        assert!(w3cos_core::class::instance_of(
            &observer,
            &reporting_observer_class()
        ));
        observer.call_method("observe", vec![]);
        crate::jsdom::drain_microtasks();
        assert_eq!(delivered.borrow().len(), 1);
        assert!(w3cos_core::class::instance_of(
            &delivered.borrow()[0],
            &report_class()
        ));

        queue_report("intervention", "w3cos://app", Value::Null);
        queue_report("deprecation", "w3cos://app", Value::Null);
        let records = observer.call_method("takeRecords", vec![]);
        assert_eq!(records.get_property("length"), Value::Number(1.0));
        observer.call_method("disconnect", vec![]);
        crate::jsdom::drain_microtasks();
        assert_eq!(delivered.borrow().len(), 1);
    }

    #[test]
    fn violation_reports_expose_structured_body_identity_and_json() {
        crate::jsdom::reset_bridge();
        let observer = w3cos_core::class::construct(
            &reporting_observer_class(),
            vec![
                Value::function(|_, _| Value::Undefined),
                Value::object(HashMap::from([(
                    "types".into(),
                    Value::array(vec![
                        Value::string("csp-violation"),
                        Value::string("integrity-violation"),
                    ]),
                )])),
            ],
        );
        observer.call_method("observe", vec![]);
        queue_report(
            "csp-violation",
            "https://example.test/",
            Value::object(HashMap::from([
                (
                    "blockedURL".into(),
                    Value::string("https://cdn.example.test/script.js"),
                ),
                ("lineNumber".into(), Value::Number(42.0)),
            ])),
        );
        queue_report(
            "integrity-violation",
            "https://example.test/",
            Value::object(HashMap::from([(
                "destination".into(),
                Value::string("script"),
            )])),
        );

        let records = observer.call_method("takeRecords", vec![]);
        let csp = records.get_property("0").get_property("body");
        assert!(w3cos_core::class::instance_of(
            &csp,
            &report_body_class("CSPViolationReportBody")
        ));
        assert!(w3cos_core::class::instance_of(
            &csp,
            &crate::compat_web::class("ReportBody")
        ));
        assert_eq!(csp.get_property("lineNumber").to_number(), 42.0);
        assert_eq!(
            csp.call_method("toJSON", vec![])
                .get_property("blockedURL")
                .to_js_string(),
            "https://cdn.example.test/script.js"
        );

        let integrity = records.get_property("1").get_property("body");
        assert!(w3cos_core::class::instance_of(
            &integrity,
            &report_body_class("IntegrityViolationReportBody")
        ));
        assert_eq!(
            integrity.get_property("destination").to_js_string(),
            "script"
        );
        assert!(!integrity.get_property("reportOnly").to_bool());
    }
}
