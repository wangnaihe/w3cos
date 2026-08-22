//! Process-local live ESM module registry shared by native AOT and W3VM hosts.
//!
//! Records expose bindings through getter/setter `Value` callables rather than
//! either backend's private storage type. This keeps Core as the ABI boundary:
//! an AOT cell and a W3VM `BindingSlot` can point at each other without linking
//! the compiler, W3IR, or W3VM into ordinary applications.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use crate::Value;

/// Core-owned storage for one ECMAScript module binding.
///
/// Native AOT and W3VM adapters can expose the same state through `Value`
/// getter/setter callables without sharing either backend's private binding
/// representation. A lexical binding starts uninitialized and therefore
/// preserves the temporal dead zone across cyclic module graphs.
#[derive(Clone)]
pub struct BindingCell {
    state: Rc<RefCell<Option<Value>>>,
}

impl BindingCell {
    pub fn uninitialized() -> Self {
        Self {
            state: Rc::new(RefCell::new(None)),
        }
    }

    pub fn initialized(value: Value) -> Self {
        Self {
            state: Rc::new(RefCell::new(Some(value))),
        }
    }

    pub fn is_initialized(&self) -> bool {
        self.state.borrow().is_some()
    }

    pub fn read(&self) -> Value {
        crate::unwrap_or_throw(self.try_read())
    }

    pub fn read_named(&self, name: &str) -> Value {
        crate::unwrap_or_throw(self.try_read_named(name))
    }

    /// TDZ-aware read that returns a Throw completion instead of unwinding.
    pub fn try_read(&self) -> Result<Value, Value> {
        self.try_read_named("module binding")
    }

    pub fn try_read_named(&self, name: &str) -> Result<Value, Value> {
        match self.state.borrow().clone() {
            Some(value) => Ok(value),
            None => Err(crate::intrinsics::reference_error(&format!(
                "{name} is not initialized"
            ))),
        }
    }

    /// Complete declaration initialization, or update an already initialized
    /// live binding. Declaration mutability remains enforced by the lowering
    /// metadata; this cell owns only instantiation and TDZ state.
    pub fn initialize_or_set(&self, value: Value) -> Value {
        *self.state.borrow_mut() = Some(value.clone());
        value
    }
}

#[derive(Clone)]
pub struct ExportBinding {
    getter: Value,
    setter: Value,
}

impl ExportBinding {
    pub fn new(getter: Value, setter: Value) -> Self {
        Self { getter, setter }
    }

    pub fn read(&self) -> Value {
        crate::unwrap_or_throw(self.try_read())
    }

    pub fn write(&self, value: Value) -> Value {
        crate::unwrap_or_throw(self.try_write(value))
    }

    pub fn try_read(&self) -> Result<Value, Value> {
        crate::catch_js(|| self.getter.call(Value::Undefined, Vec::new()))
    }

    pub fn try_write(&self, value: Value) -> Result<Value, Value> {
        if !self.setter.is_callable() {
            return Err(Value::string(
                "TypeError: imported module binding is immutable",
            ));
        }
        crate::catch_js(|| self.setter.call(Value::Undefined, vec![value]))
    }

    pub fn getter(&self) -> Value {
        self.getter.clone()
    }

    pub fn setter(&self) -> Value {
        self.setter.clone()
    }

    pub fn from_cell(cell: BindingCell, writable: bool) -> Self {
        let getter_cell = cell.clone();
        let getter = Value::function(move |_, _| getter_cell.read());
        let setter = if writable {
            Value::function(move |_, arguments| {
                cell.initialize_or_set(arguments.first().cloned().unwrap_or(Value::Undefined))
            })
        } else {
            Value::Undefined
        };
        Self::new(getter, setter)
    }
}

#[derive(Clone)]
struct ModuleRecord {
    exports: HashMap<String, ExportBinding>,
    evaluator: Option<Value>,
    namespace: RefCell<Option<Value>>,
    native: bool,
    evaluation_state: Cell<EvaluationState>,
    evaluation_result: RefCell<Option<Value>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EvaluationState {
    New,
    Evaluating,
    Evaluated,
    Failed,
}

thread_local! {
    static MODULES: RefCell<HashMap<String, ModuleRecord>> = RefCell::new(HashMap::new());
    static ALIASES: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    static EVALUATION_STACK: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn canonical_specifier(specifier: &str) -> String {
    ALIASES
        .try_with(|aliases| {
            let aliases = aliases.borrow();
            let mut current = specifier.to_string();
            let mut visited = std::collections::HashSet::new();
            while visited.insert(current.clone()) {
                let Some(next) = aliases.get(&current) else {
                    break;
                };
                current = next.clone();
            }
            current
        })
        .unwrap_or_else(|_| specifier.to_string())
}

/// Map a deployment-visible URL/specifier to the canonical module record.
///
/// Hosts can install aliases after generated AOT modules register their
/// build-time paths, allowing W3VM imports to use application, CDN or
/// redirect-final URLs without cloning module state.
pub fn register_alias(alias: impl Into<String>, canonical: impl Into<String>) {
    ALIASES.with(|aliases| {
        aliases.borrow_mut().insert(alias.into(), canonical.into());
    });
}

pub fn register(
    specifier: impl Into<String>,
    exports: HashMap<String, ExportBinding>,
    evaluator: Option<Value>,
) {
    register_with_kind(specifier, exports, evaluator, true);
}

pub fn register_runtime(
    specifier: impl Into<String>,
    exports: HashMap<String, ExportBinding>,
    evaluator: Option<Value>,
) {
    register_with_kind(specifier, exports, evaluator, false);
}

fn register_with_kind(
    specifier: impl Into<String>,
    exports: HashMap<String, ExportBinding>,
    evaluator: Option<Value>,
    native: bool,
) {
    let specifier = canonical_specifier(&specifier.into());
    MODULES.with(|modules| {
        modules.borrow_mut().insert(
            specifier,
            ModuleRecord {
                exports,
                evaluator,
                namespace: RefCell::new(None),
                native,
                evaluation_state: Cell::new(EvaluationState::New),
                evaluation_result: RefCell::new(None),
            },
        );
    });
}

pub fn contains(specifier: &str) -> bool {
    let specifier = canonical_specifier(specifier);
    MODULES.with(|modules| modules.borrow().contains_key(&specifier))
}

pub fn contains_native(specifier: &str) -> bool {
    let specifier = canonical_specifier(specifier);
    MODULES.with(|modules| {
        modules
            .borrow()
            .get(&specifier)
            .is_some_and(|record| record.native)
    })
}

pub fn export(specifier: &str, name: &str) -> Option<ExportBinding> {
    let specifier = canonical_specifier(specifier);
    MODULES.with(|modules| {
        modules
            .borrow()
            .get(&specifier)
            .and_then(|record| record.exports.get(name))
            .cloned()
    })
}

pub fn export_names(specifier: &str, include_default: bool) -> Vec<String> {
    let specifier = canonical_specifier(specifier);
    MODULES.with(|modules| {
        let modules = modules.borrow();
        let Some(record) = modules.get(&specifier) else {
            return Vec::new();
        };
        let mut names = record
            .exports
            .keys()
            .filter(|name| include_default || name.as_str() != "default")
            .cloned()
            .collect::<Vec<_>>();
        names.sort();
        names
    })
}

pub fn namespace(specifier: &str) -> Option<Value> {
    let specifier = canonical_specifier(specifier);
    MODULES.with(|modules| {
        let modules = modules.borrow();
        let record = modules.get(&specifier)?;
        if let Some(namespace) = record.namespace.borrow().clone() {
            return Some(namespace);
        }
        let namespace = Value::object(HashMap::new());
        let mut exports: Vec<_> = record.exports.iter().collect();
        exports.sort_by(|(left, _), (right, _)| left.cmp(right));
        for (name, binding) in exports {
            let binding = binding.clone();
            namespace.set_property(
                &format!("__w3cos_getter_{name}"),
                Value::function(move |_, _| binding.read()),
            );
        }
        *record.namespace.borrow_mut() = Some(namespace.clone());
        Some(namespace)
    })
}

pub fn evaluate(specifier: &str) -> Option<Value> {
    let specifier = canonical_specifier(specifier);
    enum EvaluationAction {
        Run(Option<Value>),
        Cached(Value),
        BackEdge,
    }
    let action = MODULES.with(|modules| {
        let modules = modules.borrow();
        let record = modules.get(&specifier)?;
        Some(match record.evaluation_state.get() {
            EvaluationState::New => {
                record.evaluation_state.set(EvaluationState::Evaluating);
                EvaluationAction::Run(record.evaluator.clone())
            }
            EvaluationState::Evaluating => {
                let is_back_edge = EVALUATION_STACK
                    .try_with(|stack| stack.borrow().contains(&specifier))
                    .unwrap_or(false);
                if is_back_edge {
                    EvaluationAction::BackEdge
                } else {
                    EvaluationAction::Cached(
                        record
                            .evaluation_result
                            .borrow()
                            .clone()
                            .unwrap_or_else(|| crate::promise::resolve(vec![Value::Undefined])),
                    )
                }
            }
            EvaluationState::Evaluated | EvaluationState::Failed => EvaluationAction::Cached(
                record
                    .evaluation_result
                    .borrow()
                    .clone()
                    .unwrap_or_else(|| crate::promise::resolve(vec![Value::Undefined])),
            ),
        })
    })?;

    match action {
        EvaluationAction::Cached(value) => Some(value),
        // Instantiation has already allocated every live export cell. A
        // dependency edge back into the active DFS must not await itself.
        EvaluationAction::BackEdge => Some(crate::promise::resolve(vec![Value::Undefined])),
        EvaluationAction::Run(None) => {
            let result = crate::promise::resolve(vec![Value::Undefined]);
            settle_evaluation(&specifier, EvaluationState::Evaluated, &result);
            Some(result)
        }
        EvaluationAction::Run(Some(evaluator)) => {
            let stack_specifier = specifier.clone();
            let _ = EVALUATION_STACK.try_with(|stack| {
                stack.borrow_mut().push(stack_specifier);
            });
            let execution = crate::promise::new(vec![Value::function(move |_, arguments| {
                let resolve = arguments.first().cloned().unwrap_or(Value::Undefined);
                let value = evaluator.call(Value::Undefined, Vec::new());
                resolve.call(Value::Undefined, vec![value]);
                Value::Undefined
            })]);
            let _ = EVALUATION_STACK.try_with(|stack| {
                stack.borrow_mut().pop();
            });

            MODULES.with(|modules| {
                if let Some(record) = modules.borrow().get(&specifier) {
                    *record.evaluation_result.borrow_mut() = Some(execution.clone());
                }
            });
            match crate::promise::status(&execution) {
                Some(crate::promise::PromiseStatus::Fulfilled(_)) => {
                    settle_evaluation(&specifier, EvaluationState::Evaluated, &execution);
                }
                Some(crate::promise::PromiseStatus::Rejected(_)) => {
                    settle_evaluation(&specifier, EvaluationState::Failed, &execution);
                }
                Some(crate::promise::PromiseStatus::Pending) | None => {
                    let fulfilled_specifier = specifier.clone();
                    let on_fulfilled = Value::function(move |_, _| {
                        settle_evaluation(
                            &fulfilled_specifier,
                            EvaluationState::Evaluated,
                            &Value::Undefined,
                        );
                        Value::Undefined
                    });
                    let rejected_specifier = specifier.clone();
                    let on_rejected = Value::function(move |_, arguments| {
                        settle_evaluation(
                            &rejected_specifier,
                            EvaluationState::Failed,
                            &Value::Undefined,
                        );
                        arguments.first().cloned().unwrap_or(Value::Undefined)
                    });
                    execution.call_method("then", vec![on_fulfilled, on_rejected]);
                }
            }
            Some(execution)
        }
    }
}

fn settle_evaluation(specifier: &str, state: EvaluationState, result: &Value) {
    let _ = MODULES.try_with(|modules| {
        if let Some(record) = modules.borrow().get(specifier) {
            record.evaluation_state.set(state);
            if !result.is_undefined() {
                *record.evaluation_result.borrow_mut() = Some(result.clone());
            }
        }
    });
}

pub fn unregister(specifier: &str) {
    let canonical = canonical_specifier(specifier);
    let _ = ALIASES.try_with(|aliases| {
        aliases.borrow_mut().remove(specifier);
    });
    let _ = MODULES.try_with(|modules| {
        modules.borrow_mut().remove(&canonical);
    });
}

pub fn clear() {
    let _ = MODULES.try_with(|modules| modules.borrow_mut().clear());
    let _ = ALIASES.try_with(|aliases| aliases.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::rc::Rc;

    #[test]
    fn binding_cell_preserves_tdz_then_exposes_live_updates() {
        let cell = BindingCell::uninitialized();
        let binding = ExportBinding::from_cell(cell.clone(), true);

        let completion = cell
            .try_read_named("alpha")
            .expect_err("TDZ try_read must be Err without unwinding");
        assert_eq!(completion.get_property("name").to_js_string(), "ReferenceError");
        assert!(
            completion
                .get_property("message")
                .to_js_string()
                .contains("alpha is not initialized"),
            "{}",
            completion.get_property("message").to_js_string()
        );

        let thrown = catch_unwind(AssertUnwindSafe(|| binding.read()))
            .expect_err("an uninitialized module binding must throw");
        let error = thrown
            .downcast_ref::<crate::PanicValue>()
            .expect("TDZ reads must use the shared JavaScript exception ABI")
            .0
            .clone();
        assert_eq!(error.get_property("name").to_js_string(), "ReferenceError");
        assert!(!cell.is_initialized());

        assert_eq!(binding.write(Value::Number(1.0)), Value::Number(1.0));
        assert_eq!(binding.read(), Value::Number(1.0));
        assert_eq!(binding.try_read().expect("initialized"), Value::Number(1.0));
        assert_eq!(binding.write(Value::Number(2.0)), Value::Number(2.0));
        assert_eq!(cell.read(), Value::Number(2.0));
        assert_eq!(cell.try_read().expect("initialized"), Value::Number(2.0));
    }

    #[test]
    fn initialized_binding_cell_models_var_instantiation() {
        let cell = BindingCell::initialized(Value::Undefined);
        assert!(cell.is_initialized());
        assert_eq!(cell.read(), Value::Undefined);
    }

    #[test]
    fn namespace_properties_follow_registered_live_bindings() {
        clear();
        let cell = Rc::new(RefCell::new(Value::Number(1.0)));
        let getter_cell = Rc::clone(&cell);
        let setter_cell = Rc::clone(&cell);
        register(
            "app:///counter.js",
            HashMap::from([(
                "count".into(),
                ExportBinding::new(
                    Value::function(move |_, _| getter_cell.borrow().clone()),
                    Value::function(move |_, arguments| {
                        let value = arguments.first().cloned().unwrap_or(Value::Undefined);
                        *setter_cell.borrow_mut() = value.clone();
                        value
                    }),
                ),
            )]),
            None,
        );

        let counter_namespace = namespace("app:///counter.js").unwrap();
        assert_eq!(counter_namespace.get_property("count"), Value::Number(1.0));
        export("app:///counter.js", "count")
            .unwrap()
            .write(Value::Number(2.0));
        assert_eq!(counter_namespace.get_property("count"), Value::Number(2.0));
        register_alias(
            "https://cdn.example.test/assets/counter.js",
            "app:///counter.js",
        );
        assert!(contains_native(
            "https://cdn.example.test/assets/counter.js"
        ));
        assert_eq!(
            namespace("https://cdn.example.test/assets/counter.js")
                .unwrap()
                .get_property("count"),
            Value::Number(2.0)
        );
        unregister("https://cdn.example.test/assets/counter.js");
        assert!(!contains("https://cdn.example.test/assets/counter.js"));
        assert!(!contains("app:///counter.js"));
        clear();
    }

    #[test]
    fn evaluation_is_cached_and_mixed_cycle_back_edges_do_not_deadlock() {
        clear();
        let events = Rc::new(RefCell::new(Vec::new()));
        let a_events = Rc::clone(&events);
        register(
            "app:///a.js",
            HashMap::new(),
            Some(Value::function(move |_, _| {
                a_events.borrow_mut().push("a:start");
                let dependency = evaluate("app:///b.js").unwrap();
                assert!(matches!(
                    crate::promise::status(&dependency),
                    Some(crate::promise::PromiseStatus::Fulfilled(_))
                ));
                a_events.borrow_mut().push("a:end");
                Value::Undefined
            })),
        );
        let b_events = Rc::clone(&events);
        register_runtime(
            "app:///b.js",
            HashMap::new(),
            Some(Value::function(move |_, _| {
                b_events.borrow_mut().push("b:start");
                let back_edge = evaluate("app:///a.js").unwrap();
                assert!(matches!(
                    crate::promise::status(&back_edge),
                    Some(crate::promise::PromiseStatus::Fulfilled(_))
                ));
                b_events.borrow_mut().push("b:end");
                Value::Undefined
            })),
        );

        let first = evaluate("app:///a.js").unwrap();
        let second = evaluate("app:///a.js").unwrap();
        assert!(matches!(
            crate::promise::status(&first),
            Some(crate::promise::PromiseStatus::Fulfilled(_))
        ));
        assert!(matches!(
            crate::promise::status(&second),
            Some(crate::promise::PromiseStatus::Fulfilled(_))
        ));
        assert_eq!(
            events.borrow().as_slice(),
            &["a:start", "b:start", "b:end", "a:end"]
        );
        clear();
    }

    #[test]
    fn cached_native_evaluation_promise_accepts_new_realm_subscriptions() {
        clear();
        let evaluations = Rc::new(Cell::new(0));
        let evaluator_calls = Rc::clone(&evaluations);
        register(
            "app:///shared.js",
            HashMap::new(),
            Some(Value::function(move |_, _| {
                evaluator_calls.set(evaluator_calls.get() + 1);
                Value::Undefined
            })),
        );

        let first = evaluate("app:///shared.js").unwrap();
        assert!(matches!(
            crate::promise::status(&first),
            Some(crate::promise::PromiseStatus::Fulfilled(_))
        ));
        assert_eq!(evaluations.get(), 1);

        crate::promise::advance_realm_generation();
        let cached = evaluate("app:///shared.js").unwrap();
        let observed = Rc::new(Cell::new(false));
        let subscription_observed = Rc::clone(&observed);
        cached.call_method(
            "then",
            vec![Value::function(move |_, _| {
                subscription_observed.set(true);
                Value::Undefined
            })],
        );

        assert_eq!(crate::promise::drain_microtasks(), 1);
        assert!(observed.get());
        assert_eq!(evaluations.get(), 1);
        clear();
    }

    #[test]
    fn rejected_evaluation_is_cached() {
        clear();
        let calls = Rc::new(Cell::new(0));
        let evaluator_calls = Rc::clone(&calls);
        register(
            "app:///failed.js",
            HashMap::new(),
            Some(Value::function(move |_, _| {
                evaluator_calls.set(evaluator_calls.get() + 1);
                crate::throw_value(Value::string("failed"))
            })),
        );

        for _ in 0..2 {
            let result = evaluate("app:///failed.js").unwrap();
            assert!(matches!(
                crate::promise::status(&result),
                Some(crate::promise::PromiseStatus::Rejected(reason))
                    if reason == Value::string("failed")
            ));
        }
        assert_eq!(calls.get(), 1);
        clear();
    }
}
