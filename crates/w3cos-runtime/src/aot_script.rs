//! `<script src>` insertion for AOT builds (no `dynamic-js` feature).
//!
//! An AOT artifact cannot fetch and compile JavaScript at runtime, but it may
//! contain the script ahead of time: the compiler registers precompiled
//! classic scripts in the module registry under their absolute file path.
//! When a connected `<script src>` is inserted, this runs the precompiled
//! script in a task and fires `load`; a script that is not in the artifact
//! fires `error` instead, so loaders awaiting either event never hang.

use crate::dom;
use crate::jsdom;
use w3cos_core::Value;

const STARTED: &str = "__w3cos_aot_script_started";

/// Map a script URL to the registry specifier of its precompiled module.
pub(crate) fn script_src_to_specifier(src: &str) -> Option<String> {
    let src = src.split(['?', '#']).next().unwrap_or(src);
    for prefix in ["vscode-file://vscode-app", "file://"] {
        if let Some(path) = src.strip_prefix(prefix)
            && path.starts_with('/')
        {
            return Some(percent_decode(path));
        }
    }
    src.starts_with('/').then(|| percent_decode(src))
}

fn percent_decode(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(&path[i + 1..i + 3], 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| path.to_string())
}

/// Post-insertion steps for `node` (the inserted root, not its subtree).
pub(crate) fn notify_inserted(node: u32) {
    if dom::node_type(node) != 1 || !dom::tag_name(node).eq_ignore_ascii_case("script") {
        return;
    }
    if !dom::is_connected(node) || !dom::has_attribute(node, "src") {
        return;
    }
    if jsdom::get_expando(node, STARTED).is_some() {
        return;
    }
    jsdom::set_expando(node, STARTED, Value::Bool(true));
    let src = dom::get_attribute(node, "src").unwrap_or_default();
    let task = Value::function(move |_, _| {
        run_script(node, &src);
        Value::Undefined
    });
    jsdom::window_value().call_method("setTimeout", vec![task, Value::Number(0.0)]);
}

fn run_script(node: u32, src: &str) {
    let specifier = script_src_to_specifier(src)
        .filter(|specifier| w3cos_core::module_registry::contains(specifier));
    let Some(specifier) = specifier else {
        jsdom::dispatch_element_lifecycle_event(node, "error");
        return;
    };
    let window = jsdom::window_value();
    let amd = AmdBridge::install(&window);
    // A classic script that throws still "ran": the exception is reported
    // and `load` fires, as in a browser.
    let evaluated = w3cos_core::catch_js(|| {
        w3cos_core::module_registry::evaluate(&specifier).unwrap_or(Value::Undefined)
    });
    let report = {
        let src = src.to_string();
        move |error: &Value| eprintln!("[aot_script] {src}: {}", error.to_js_string())
    };
    let finish = move || {
        amd.finish(&window, &specifier);
        jsdom::dispatch_element_lifecycle_event(node, "load");
    };
    let evaluation = match evaluated {
        Ok(evaluation) => evaluation,
        Err(error) => {
            report(&error);
            finish();
            return;
        }
    };
    match w3cos_core::promise::status(&evaluation) {
        Some(w3cos_core::promise::PromiseStatus::Pending) => {
            let finish = std::rc::Rc::new(std::cell::RefCell::new(Some(finish)));
            let on_fulfilled = {
                let finish = finish.clone();
                Value::function(move |_, _| {
                    if let Some(finish) = finish.borrow_mut().take() {
                        finish();
                    }
                    Value::Undefined
                })
            };
            let on_rejected = Value::function(move |_, arguments| {
                report(&arguments.first().cloned().unwrap_or(Value::Undefined));
                if let Some(finish) = finish.borrow_mut().take() {
                    finish();
                }
                Value::Undefined
            });
            evaluation.call_method("then", vec![on_fulfilled, on_rejected]);
        }
        Some(w3cos_core::promise::PromiseStatus::Rejected(error)) => {
            report(&error);
            finish();
        }
        _ => finish(),
    }
}

/// UMD bundles (xterm.js and its addons) pick their CommonJS branch when
/// `module`/`exports` exist, which they always do in a precompiled record,
/// so they never call the page's AMD `define`. An AMD loader (VS Code's
/// amdX) waits for that call after `load`. When the page has `define.amd`
/// and the script did not call it, call `define([], () => exports)` for it.
struct AmdBridge {
    original: Value,
    called: std::rc::Rc<std::cell::Cell<bool>>,
}

impl AmdBridge {
    fn install(window: &Value) -> Option<Self> {
        let original = window.get_property("define");
        if !original.is_function() || !original.get_property("amd").to_bool() {
            return None;
        }
        let called = std::rc::Rc::new(std::cell::Cell::new(false));
        let spy = {
            let original = original.clone();
            let called = called.clone();
            Value::function(move |this, arguments| {
                called.set(true);
                original.call(this, arguments)
            })
        };
        spy.set_property("amd", original.get_property("amd"));
        window.set_property("define", spy);
        Some(Self { original, called })
    }
}

trait FinishAmd {
    fn finish(&self, window: &Value, specifier: &str);
}

impl FinishAmd for Option<AmdBridge> {
    fn finish(&self, window: &Value, specifier: &str) {
        let Some(bridge) = self else {
            return;
        };
        window.set_property("define", bridge.original.clone());
        if bridge.called.get() {
            return;
        }
        let Some(namespace) = w3cos_core::module_registry::namespace(specifier) else {
            return;
        };
        let exports = namespace.get_property("default");
        let exports = if exports.is_undefined() {
            namespace
        } else {
            exports
        };
        let factory = Value::function(move |_, _| exports.clone());
        bridge
            .original
            .call(Value::Undefined, vec![Value::array(Vec::new()), factory]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    fn reset() {
        dom::reset_document();
        jsdom::reset_bridge();
        w3cos_core::module_registry::clear();
    }

    /// Append `<script src>` to `<head>` and return the fired event types.
    fn insert_script(src: &str) -> Rc<RefCell<Vec<String>>> {
        let document = jsdom::document_value();
        let script = document.call_method("createElement", vec![Value::string("script")]);
        let events = Rc::new(RefCell::new(Vec::new()));
        for kind in ["load", "error"] {
            let events = events.clone();
            script.call_method(
                "addEventListener",
                vec![
                    Value::string(kind),
                    Value::function(move |_, _| {
                        events.borrow_mut().push(kind.to_string());
                        Value::Undefined
                    }),
                ],
            );
        }
        script.call_method(
            "setAttribute",
            vec![Value::string("src"), Value::string(src)],
        );
        document
            .get_property("head")
            .call_method("appendChild", vec![script]);
        events
    }

    fn run_tasks() {
        for _ in 0..4 {
            jsdom::tick_timers();
            w3cos_core::promise::drain_microtasks();
        }
    }

    #[test]
    fn maps_vscode_file_and_file_urls_to_registry_paths() {
        assert_eq!(
            script_src_to_specifier("vscode-file://vscode-app/a/node_modules/x%20y/lib/x.js?v=1")
                .as_deref(),
            Some("/a/node_modules/x y/lib/x.js")
        );
        assert_eq!(
            script_src_to_specifier("file:///a/b.js").as_deref(),
            Some("/a/b.js")
        );
        assert_eq!(script_src_to_specifier("https://example.com/b.js"), None);
    }

    #[test]
    fn precompiled_script_runs_in_a_task_then_fires_load() {
        reset();
        let ran = Rc::new(RefCell::new(0));
        let counter = ran.clone();
        w3cos_core::module_registry::register_runtime(
            "/app/node_modules/pkg/lib/pkg.js",
            HashMap::new(),
            Some(Value::function(move |_, _| {
                *counter.borrow_mut() += 1;
                Value::Undefined
            })),
        );
        let events = insert_script("vscode-file://vscode-app/app/node_modules/pkg/lib/pkg.js");
        // Insertion only queues the script; it runs in a later task.
        assert_eq!(*ran.borrow(), 0);
        assert!(events.borrow().is_empty());
        run_tasks();
        assert_eq!(*ran.borrow(), 1);
        assert_eq!(*events.borrow(), vec!["load".to_string()]);
    }

    #[test]
    fn script_missing_from_the_artifact_fires_error() {
        reset();
        let events = insert_script("vscode-file://vscode-app/app/missing.js");
        run_tasks();
        assert_eq!(*events.borrow(), vec!["error".to_string()]);
    }

    #[test]
    fn script_without_src_or_reinserted_does_not_fire() {
        reset();
        let document = jsdom::document_value();
        let inline = document.call_method("createElement", vec![Value::string("script")]);
        document
            .get_property("head")
            .call_method("appendChild", vec![inline]);
        let events = insert_script("file:///app/missing.js");
        run_tasks();
        assert_eq!(events.borrow().len(), 1);
    }

    #[test]
    fn commonjs_umd_record_is_handed_to_the_pages_amd_define() {
        reset();
        let exports = Value::object(HashMap::new());
        exports.set_property("Terminal", Value::Number(7.0));
        let module_exports = exports.clone();
        let mut bindings = HashMap::new();
        bindings.insert(
            "default".to_string(),
            w3cos_core::module_registry::ExportBinding::new(
                Value::function(move |_, _| module_exports.clone()),
                Value::Undefined,
            ),
        );
        w3cos_core::module_registry::register_runtime(
            "/app/node_modules/umd/lib/umd.js",
            bindings,
            Some(Value::function(|_, _| Value::Undefined)),
        );
        let defined = Rc::new(RefCell::new(Vec::new()));
        let sink = defined.clone();
        let define = Value::function(move |_, arguments| {
            let factory = arguments.get(1).cloned().unwrap_or(Value::Undefined);
            sink.borrow_mut()
                .push(factory.call(Value::Undefined, Vec::new()));
            Value::Undefined
        });
        define.set_property("amd", Value::Bool(true));
        let window = jsdom::window_value();
        window.set_property("define", define.clone());
        let events = insert_script("vscode-file://vscode-app/app/node_modules/umd/lib/umd.js");
        run_tasks();
        assert_eq!(*events.borrow(), vec!["load".to_string()]);
        let defined = defined.borrow();
        assert_eq!(defined.len(), 1);
        assert_eq!(defined[0].get_property("Terminal").to_js_string(), "7");
        // The page's own `define` is restored.
        assert!(window.get_property("define").get_property("amd").to_bool());
        window.set_property("define", Value::Undefined);
    }
}
