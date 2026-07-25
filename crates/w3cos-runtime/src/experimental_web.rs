//! Truthful compatibility surfaces for browser experiments without native adapters.
//!
//! Stateful, process-local operations (Shared Storage modifiers and viewport
//! segments) work. APIs requiring privileged browser services reject with a
//! standard error and emit a compatibility warning.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use w3cos_core::Value;

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static SHARED_STORAGE: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    static SINGLETONS: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static WARNING_EMITTED: Cell<bool> = const { Cell::new(false) };
}

fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or(Value::Undefined)
}

fn error(name: &str, message: &str) -> Value {
    if matches!(name, "TypeError" | "RangeError") {
        w3cos_core::error_instance(name, vec![Value::string(message)])
    } else {
        w3cos_core::web::dom_exception_instance(message, name)
    }
}

fn throw(name: &str, message: &str) -> ! {
    w3cos_core::throw_value(error(name, message))
}

fn warn_once() {
    WARNING_EMITTED.with(|warned| {
        if !warned.replace(true) {
            eprintln!(
                "[w3cos] warning: experimental browser APIs preserve compatible records and \
                 process-local state; privileged browser services, AI models, local-font \
                 enumeration, ink presentation, profiling and privacy-sandbox delivery require \
                 host adapters"
            );
        }
    });
}

fn unavailable(api: &str) -> Value {
    warn_once();
    w3cos_core::promise::reject(vec![error(
        "NotSupportedError",
        &format!("{api} requires a host browser-service adapter"),
    )])
}

fn illegal(name: &str) -> ! {
    throw("TypeError", &format!("Illegal constructor: {name}"))
}

fn prototype_members(name: &str) -> &'static [&'static str] {
    match name {
        "CrashReportContext" => &["delete", "initialize", "set"],
        "CreateMonitor" => &["ondownloadprogress"],
        "DelegatedInkTrailPresenter" => &["presentationArea", "updateInkTrailStartPoint"],
        "Fence" => &[
            "getNestedConfigs",
            "reportEvent",
            "setReportEventDataForAutomaticBeacons",
        ],
        "FencedFrameConfig" => &["setSharedStorageContext"],
        "FetchLaterResult" => &["activated"],
        "FontData" => &["blob", "family", "fullName", "postscriptName", "style"],
        "Ink" => &["requestPresenter"],
        "LanguageDetector" => &[
            "destroy",
            "detect",
            "expectedInputLanguages",
            "inputQuota",
            "measureInputUsage",
        ],
        "LanguageModel" => &[
            "append",
            "clone",
            "contextUsage",
            "contextWindow",
            "destroy",
            "measureContextUsage",
            "oncontextoverflow",
            "prompt",
            "promptStreaming",
        ],
        "NavigationPrecommitController" => &["addHandler", "redirect"],
        "Profiler" => &["sampleInterval", "stop", "stopped"],
        "ProtectedAudience" => &["queryFeatureSupport"],
        "SharedStorage" => &[
            "append",
            "batchUpdate",
            "clear",
            "createWorklet",
            "delete",
            "run",
            "selectURL",
            "set",
            "worklet",
        ],
        "SharedStorageWorklet" => &["addModule", "run", "selectURL"],
        "Summarizer" => &[
            "destroy",
            "expectedContextLanguages",
            "expectedInputLanguages",
            "format",
            "inputQuota",
            "length",
            "measureInputUsage",
            "outputLanguage",
            "sharedContext",
            "summarize",
            "summarizeStreaming",
            "type",
        ],
        "Translator" => &[
            "destroy",
            "inputQuota",
            "measureInputUsage",
            "sourceLanguage",
            "targetLanguage",
            "translate",
            "translateStreaming",
        ],
        "Viewport" => &["segments"],
        "WGSLLanguageFeatures" => &["entries", "forEach", "has", "keys", "size", "values"],
        _ => &[],
    }
}

fn modifier_constructor(name: &'static str, args: &[Value], this: &Value) {
    let required = match name {
        "SharedStorageClearMethod" => 0,
        "SharedStorageDeleteMethod" => 1,
        "SharedStorageAppendMethod" | "SharedStorageSetMethod" => 2,
        _ => return illegal(name),
    };
    if args.len() < required {
        throw(
            "TypeError",
            &format!("{name} requires {required} argument(s)"),
        );
    }
    this.set_property("__w3cos_kind", Value::string(name));
    if required >= 1 {
        this.set_property("__w3cos_key", Value::string(&arg(args, 0).to_js_string()));
    }
    if required >= 2 {
        this.set_property("__w3cos_value", Value::string(&arg(args, 1).to_js_string()));
    }
    if name == "SharedStorageSetMethod" {
        let options = arg(args, 2);
        this.set_property(
            "__w3cos_ignore_if_present",
            Value::Bool(options.get_property("ignoreIfPresent").to_bool()),
        );
    }
}

fn build_class(name: &'static str) -> Value {
    let constructor = match name {
        "SharedStorageAppendMethod"
        | "SharedStorageClearMethod"
        | "SharedStorageDeleteMethod"
        | "SharedStorageSetMethod" => Value::function(move |this, args| {
            modifier_constructor(name, &args, &this);
            Value::Undefined
        }),
        "Profiler" => Value::function(|_, args| {
            if args.is_empty() {
                throw("TypeError", "Profiler requires 1 argument");
            }
            warn_once();
            throw(
                "NotAllowedError",
                "JS profiling is disabled because no host profiler adapter is configured",
            )
        }),
        _ => Value::function(move |_, _| illegal(name)),
    };
    constructor.set_property("name", Value::string(name));
    let prototype = Value::object(HashMap::from([("constructor".into(), constructor.clone())]));
    for member in prototype_members(name) {
        prototype.set_property(member, Value::Undefined);
    }
    if matches!(name, "CreateMonitor" | "Profiler") {
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::web_events::event_target_class().get_property("prototype"),
        );
    }
    constructor.set_property("prototype", prototype);
    if matches!(
        name,
        "LanguageDetector" | "LanguageModel" | "Summarizer" | "Translator"
    ) {
        constructor.set_property(
            "availability",
            Value::function(move |_, args| {
                if name == "Translator" {
                    let options = arg(&args, 0);
                    if options.get_property("sourceLanguage").is_undefined()
                        || options.get_property("targetLanguage").is_undefined()
                    {
                        return w3cos_core::promise::reject(vec![error(
                            "TypeError",
                            "Translator availability requires sourceLanguage and targetLanguage",
                        )]);
                    }
                }
                warn_once();
                w3cos_core::promise::resolve(vec![Value::string("unavailable")])
            }),
        );
        constructor.set_property(
            "create",
            Value::function(move |_, args| {
                if name == "Translator" {
                    let options = arg(&args, 0);
                    if options.get_property("sourceLanguage").is_undefined()
                        || options.get_property("targetLanguage").is_undefined()
                    {
                        return w3cos_core::promise::reject(vec![error(
                            "TypeError",
                            "Translator creation requires sourceLanguage and targetLanguage",
                        )]);
                    }
                }
                unavailable(&format!("{name}.create"))
            }),
        );
    }
    constructor
}

pub fn class_for(name: &'static str) -> Value {
    CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        let class = build_class(name);
        classes.borrow_mut().insert(name.into(), class.clone());
        class
    })
}

fn set_prototype(value: &Value, name: &'static str) {
    w3cos_core::class::set_prototype_of(value, &class_for(name).get_property("prototype"));
}

fn singleton(name: &'static str, build: impl FnOnce() -> Value) -> Value {
    SINGLETONS.with(|values| {
        if let Some(value) = values.borrow().get(name).cloned() {
            return value;
        }
        let value = build();
        values.borrow_mut().insert(name.into(), value.clone());
        value
    })
}

fn apply_modifier(method: Value) {
    let kind = method.get_property("__w3cos_kind").to_js_string();
    let key = method.get_property("__w3cos_key").to_js_string();
    let value = method.get_property("__w3cos_value").to_js_string();
    SHARED_STORAGE.with(|storage| {
        let mut storage = storage.borrow_mut();
        match kind.as_str() {
            "SharedStorageAppendMethod" => storage.entry(key).or_default().push_str(&value),
            "SharedStorageClearMethod" => storage.clear(),
            "SharedStorageDeleteMethod" => {
                storage.remove(&key);
            }
            "SharedStorageSetMethod" => {
                let ignore = method.get_property("__w3cos_ignore_if_present").to_bool();
                if !ignore || !storage.contains_key(&key) {
                    storage.insert(key, value);
                }
            }
            _ => {}
        }
    });
}

pub fn shared_storage_value() -> Value {
    singleton("SharedStorage", || {
        let value = Value::object(HashMap::new());
        value.set_property(
            "set",
            Value::function(|_, args| {
                let key = arg(&args, 0).to_js_string();
                let text = arg(&args, 1).to_js_string();
                let ignore = arg(&args, 2).get_property("ignoreIfPresent").to_bool();
                SHARED_STORAGE.with(|storage| {
                    let mut storage = storage.borrow_mut();
                    if !ignore || !storage.contains_key(&key) {
                        storage.insert(key, text);
                    }
                });
                w3cos_core::promise::resolve(vec![Value::Undefined])
            }),
        );
        value.set_property(
            "append",
            Value::function(|_, args| {
                let key = arg(&args, 0).to_js_string();
                let text = arg(&args, 1).to_js_string();
                SHARED_STORAGE
                    .with(|storage| storage.borrow_mut().entry(key).or_default().push_str(&text));
                w3cos_core::promise::resolve(vec![Value::Undefined])
            }),
        );
        value.set_property(
            "delete",
            Value::function(|_, args| {
                SHARED_STORAGE.with(|storage| {
                    storage.borrow_mut().remove(&arg(&args, 0).to_js_string());
                });
                w3cos_core::promise::resolve(vec![Value::Undefined])
            }),
        );
        value.set_property(
            "clear",
            Value::function(|_, _| {
                SHARED_STORAGE.with(|storage| storage.borrow_mut().clear());
                w3cos_core::promise::resolve(vec![Value::Undefined])
            }),
        );
        value.set_property(
            "batchUpdate",
            Value::function(|_, args| {
                for method in arg(&args, 0).iter() {
                    apply_modifier(method);
                }
                w3cos_core::promise::resolve(vec![Value::Undefined])
            }),
        );
        for operation in ["createWorklet", "run", "selectURL"] {
            value.set_property(
                operation,
                Value::function(move |_, _| unavailable(&format!("SharedStorage.{operation}"))),
            );
        }
        value.set_property("worklet", shared_storage_worklet_value());
        set_prototype(&value, "SharedStorage");
        value
    })
}

pub fn shared_storage_worklet_value() -> Value {
    singleton("SharedStorageWorklet", || {
        let value = Value::object(HashMap::new());
        for operation in ["addModule", "run", "selectURL"] {
            value.set_property(
                operation,
                Value::function(move |_, _| {
                    unavailable(&format!("SharedStorageWorklet.{operation}"))
                }),
            );
        }
        set_prototype(&value, "SharedStorageWorklet");
        value
    })
}

pub fn ink_value() -> Value {
    singleton("Ink", || {
        let value = Value::object(HashMap::new());
        value.set_property(
            "requestPresenter",
            Value::function(|_, _| unavailable("Ink.requestPresenter")),
        );
        set_prototype(&value, "Ink");
        value
    })
}

pub fn protected_audience_value() -> Value {
    singleton("ProtectedAudience", || {
        let value = Value::object(HashMap::new());
        value.set_property(
            "queryFeatureSupport",
            Value::function(|_, _| {
                warn_once();
                Value::string("unsupported")
            }),
        );
        set_prototype(&value, "ProtectedAudience");
        value
    })
}

pub fn viewport_value() -> Value {
    singleton("Viewport", || {
        let value = Value::object(HashMap::new());
        value.set_property(
            "__w3cos_getter_segments",
            Value::function(|_, _| {
                let (width, height, _) = crate::jsdom::viewport();
                Value::array(vec![Value::object(HashMap::from([
                    ("x".into(), Value::Number(0.0)),
                    ("y".into(), Value::Number(0.0)),
                    ("left".into(), Value::Number(0.0)),
                    ("top".into(), Value::Number(0.0)),
                    ("right".into(), Value::Number(width)),
                    ("bottom".into(), Value::Number(height)),
                    ("width".into(), Value::Number(width)),
                    ("height".into(), Value::Number(height)),
                ]))])
            }),
        );
        set_prototype(&value, "Viewport");
        value
    })
}

pub fn wgsl_language_features_value() -> Value {
    singleton("WGSLLanguageFeatures", || {
        let value = Value::object(HashMap::new());
        value.set_property(
            "__w3cos_getter_size",
            Value::function(|_, _| Value::Number(0.0)),
        );
        value.set_property("has", Value::function(|_, _| Value::Bool(false)));
        value.set_property("forEach", Value::function(|_, _| Value::Undefined));
        for operation in ["entries", "keys", "values"] {
            value.set_property(operation, Value::function(|_, _| Value::array(Vec::new())));
        }
        value.set_property("__w3cos_symbol_iterator", value.get_property("values"));
        set_prototype(&value, "WGSLLanguageFeatures");
        value
    })
}

pub fn query_local_fonts_value() -> Value {
    Value::function(|_, _| {
        warn_once();
        w3cos_core::promise::resolve(vec![Value::array(Vec::new())])
    })
}

pub fn fetch_later_value() -> Value {
    Value::function(|_, args| {
        if args.is_empty() {
            throw("TypeError", "fetchLater requires 1 argument");
        }
        warn_once();
        let value = Value::object(HashMap::from([("activated".into(), Value::Bool(false))]));
        set_prototype(&value, "FetchLaterResult");
        value
    })
}

pub const INTERFACES: &[&str] = &[
    "CrashReportContext",
    "CreateMonitor",
    "DelegatedInkTrailPresenter",
    "Fence",
    "FencedFrameConfig",
    "FetchLaterResult",
    "FontData",
    "Ink",
    "LanguageDetector",
    "LanguageModel",
    "NavigationPrecommitController",
    "Profiler",
    "ProtectedAudience",
    "SharedStorage",
    "SharedStorageAppendMethod",
    "SharedStorageClearMethod",
    "SharedStorageDeleteMethod",
    "SharedStorageModifierMethod",
    "SharedStorageSetMethod",
    "SharedStorageWorklet",
    "Summarizer",
    "Translator",
    "Viewport",
    "WGSLLanguageFeatures",
];

pub fn reset() {
    CLASSES.with(|classes| classes.borrow_mut().clear());
    SHARED_STORAGE.with(|storage| storage.borrow_mut().clear());
    SINGLETONS.with(|values| values.borrow_mut().clear());
    WARNING_EMITTED.with(|warned| warned.set(false));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_storage_modifiers_preserve_process_local_state() {
        reset();
        let storage = shared_storage_value();
        storage.call_method("set", vec![Value::string("key"), Value::string("a")]);
        let append = w3cos_core::class::construct(
            &class_for("SharedStorageAppendMethod"),
            vec![Value::string("key"), Value::string("b")],
        );
        storage.call_method("batchUpdate", vec![Value::array(vec![append])]);
        SHARED_STORAGE.with(|state| {
            assert_eq!(state.borrow().get("key").map(String::as_str), Some("ab"));
        });
    }

    #[test]
    fn unavailable_model_apis_report_capability_without_fake_output() {
        reset();
        let detector = class_for("LanguageDetector");
        let availability =
            detector.call_method("availability", vec![Value::object(HashMap::new())]);
        assert!(availability.is_object());
        let created = detector.call_method("create", vec![Value::object(HashMap::new())]);
        assert!(created.is_object());
        assert_eq!(
            class_for("Translator")
                .get_property("prototype")
                .get_property("translate")
                .is_undefined(),
            true
        );
    }

    #[test]
    fn viewport_segments_are_live_and_fetch_later_is_inactive() {
        reset();
        crate::jsdom::set_viewport(800.0, 600.0);
        let viewport = viewport_value();
        let segment = viewport.get_property("segments").get_property("0");
        assert_eq!(segment.get_property("width").to_number(), 800.0);
        let fetch_later = fetch_later_value();
        let result = fetch_later.call(Value::Undefined, vec![Value::string("/ping")]);
        assert!(!result.get_property("activated").to_bool());
        assert!(w3cos_core::class::instance_of(
            &result,
            &class_for("FetchLaterResult")
        ));
    }
}
