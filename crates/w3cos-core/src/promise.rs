//! JavaScript `Promise` for the ESM compile pipeline.
//!
//! A promise is a `Value::Object` whose own properties are the instance
//! methods `then` / `catch` / `finally` (plain function values, so compiled
//! property access finds them without prototype plumbing) plus the hidden
//! numeric key `__w3cos_promise`. That key is an id into a thread-local
//! registry mapping it to the shared `Rc<RefCell<PromiseState>>` — `Value`
//! has no native-resource slot, so the state itself cannot live inside a
//! property directly.
//!
//! Reactions never run synchronously: settlement only enqueues reaction
//! jobs onto the thread-local microtask queue ([`PROMISE_MICROTASKS`]),
//! which the embedder drains via [`drain_microtasks`]. Compiled JS `throw`
//! uses [`crate::throw_value`] (`panic_any` of a `Send` wrapper — a
//! bare `Value` is not `Send`); every callback boundary here
//! (`new`'s executor, reaction handlers) catches such panics with
//! `catch_unwind` and turns the payload back into a rejection.
//!
//! v1 limitations: unhandled rejections are tracked as a no-op (nothing
//! reports them), the state registry never reclaims ids of dropped
//! promises, and `finally` does not await a promise returned from its
//! callback.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use crate::Value;
use crate::value::js_error;

/// Hidden own property holding the promise's state-registry id.
const STATE_KEY: &str = "__w3cos_promise";

/// Internal promise state machine.
enum PromiseState {
    Pending { callbacks: Vec<Reaction> },
    Fulfilled(Value),
    Rejected(Value),
}

/// A `then` subscription: the two handlers plus the state of the promise
/// that `then` returned (settled when the matching handler runs).
struct Reaction {
    on_fulfilled: Value,
    on_rejected: Value,
    next_state: Rc<RefCell<PromiseState>>,
}

thread_local! {
    /// Microtask queue: jobs are `Value::Function`s that run one reaction.
    static PROMISE_MICROTASKS: RefCell<Vec<Value>> = const { RefCell::new(Vec::new()) };
    /// State registry keyed by the id stored under [`STATE_KEY`].
    static PROMISE_STATES: RefCell<HashMap<u64, Rc<RefCell<PromiseState>>>> =
        RefCell::new(HashMap::new());
    static NEXT_PROMISE_ID: Cell<u64> = const { Cell::new(1) };
}

fn register_state(state: Rc<RefCell<PromiseState>>) -> u64 {
    let id = NEXT_PROMISE_ID.with(|counter| {
        let id = counter.get();
        counter.set(id + 1);
        id
    });
    PROMISE_STATES.with(|registry| registry.borrow_mut().insert(id, state));
    id
}

/// The shared state behind `value`, when `value` is one of our promises.
fn state_of(value: &Value) -> Option<Rc<RefCell<PromiseState>>> {
    if let Value::Object(object) = value {
        if let Value::Number(id) = object.borrow().get_direct(STATE_KEY) {
            return PROMISE_STATES.with(|registry| registry.borrow().get(&(id as u64)).cloned());
        }
    }
    None
}

fn is_pending(state: &Rc<RefCell<PromiseState>>) -> bool {
    matches!(&*state.borrow(), PromiseState::Pending { .. })
}

fn pending_state() -> Rc<RefCell<PromiseState>> {
    Rc::new(RefCell::new(PromiseState::Pending {
        callbacks: Vec::new(),
    }))
}

/// Turn a panic payload back into a JS value: compiled `throw` panics with
/// a [`PanicValue`] (bare `Value` cannot satisfy `panic_any`'s `Send`
/// bound), Rust code with a string message.
pub(crate) fn payload_to_value(payload: Box<dyn Any + Send>) -> Value {
    if let Some(value) = payload.downcast_ref::<Value>() {
        return value.clone();
    }
    if let Some(wrapped) = payload.downcast_ref::<crate::PanicValue>() {
        return wrapped.0.clone();
    }
    if let Some(message) = payload.downcast_ref::<&str>() {
        return js_error(message);
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return js_error(message);
    }
    js_error("non-JS panic crossed a promise boundary")
}

// ── Settlement ─────────────────────────────────────────────────────────

/// `[[Resolve]](state, value)` — with thenable assimilation.
fn fulfill_state(state: &Rc<RefCell<PromiseState>>, value: Value) {
    if !is_pending(state) {
        return;
    }
    // One of our own promises: adopt its state (this also implements the
    // one-level unwrap of promises returned from `then` handlers).
    if let Some(other) = state_of(&value) {
        if Rc::ptr_eq(&other, state) {
            reject_state(
                state,
                js_error("TypeError: chaining cycle detected for promise"),
            );
            return;
        }
        let on_fulfilled = {
            let state = state.clone();
            Value::function(move |_, args| {
                fulfill_state(&state, args.first().cloned().unwrap_or(Value::Undefined));
                Value::Undefined
            })
        };
        let on_rejected = {
            let state = state.clone();
            Value::function(move |_, args| {
                reject_state(&state, args.first().cloned().unwrap_or(Value::Undefined));
                Value::Undefined
            })
        };
        subscribe(&other, on_fulfilled, on_rejected);
        return;
    }
    // Foreign thenable: best-effort — call its `then` with our resolve/reject.
    if value.is_object() {
        let then = value.get_property("then");
        if then.is_function() {
            let resolve_fn = make_resolve(state.clone());
            let reject_fn = make_reject(state.clone());
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                then.call(value.clone(), vec![resolve_fn, reject_fn])
            }));
            if let Err(payload) = outcome {
                reject_state(state, payload_to_value(payload));
            }
            return;
        }
    }
    settle(state, Ok(value));
}

fn reject_state(state: &Rc<RefCell<PromiseState>>, reason: Value) {
    if !is_pending(state) {
        return;
    }
    settle(state, Err(reason));
}

/// Transition out of `Pending` and schedule every queued reaction.
fn settle(state: &Rc<RefCell<PromiseState>>, outcome: Result<Value, Value>) {
    let callbacks = {
        let next = match &outcome {
            Ok(value) => PromiseState::Fulfilled(value.clone()),
            Err(reason) => PromiseState::Rejected(reason.clone()),
        };
        match std::mem::replace(&mut *state.borrow_mut(), next) {
            PromiseState::Pending { callbacks } => callbacks,
            _ => Vec::new(),
        }
    };
    let (is_fulfilled, value) = match outcome {
        Ok(value) => (true, value),
        Err(reason) => (false, reason),
    };
    for reaction in callbacks {
        schedule_reaction(reaction, is_fulfilled, value.clone());
    }
}

/// Enqueue one reaction run as a microtask job.
fn schedule_reaction(reaction: Reaction, is_fulfilled: bool, value: Value) {
    let job = Value::function(move |_, _| {
        run_reaction(&reaction, is_fulfilled, value.clone());
        Value::Undefined
    });
    PROMISE_MICROTASKS.with(|queue| queue.borrow_mut().push(job));
}

/// The microtask body: invoke the matching handler (or pass the settlement
/// through when it is missing) and settle the derived promise accordingly.
fn run_reaction(reaction: &Reaction, is_fulfilled: bool, value: Value) {
    let handler = if is_fulfilled {
        &reaction.on_fulfilled
    } else {
        &reaction.on_rejected
    };
    if !handler.is_function() {
        if is_fulfilled {
            fulfill_state(&reaction.next_state, value);
        } else {
            reject_state(&reaction.next_state, value);
        }
        return;
    }
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        handler.call(Value::Undefined, vec![value.clone()])
    }));
    match outcome {
        Ok(result) => fulfill_state(&reaction.next_state, result),
        Err(payload) => reject_state(&reaction.next_state, payload_to_value(payload)),
    }
}

/// Subscribe to `state`; returns the state of the derived promise.
fn subscribe(
    state: &Rc<RefCell<PromiseState>>,
    on_fulfilled: Value,
    on_rejected: Value,
) -> Rc<RefCell<PromiseState>> {
    let next_state = pending_state();
    let reaction = Reaction {
        on_fulfilled,
        on_rejected,
        next_state: next_state.clone(),
    };
    let settled = {
        let borrowed = state.borrow();
        match &*borrowed {
            PromiseState::Pending { .. } => None,
            PromiseState::Fulfilled(value) => Some((true, value.clone())),
            PromiseState::Rejected(reason) => Some((false, reason.clone())),
        }
    };
    match settled {
        Some((is_fulfilled, value)) => schedule_reaction(reaction, is_fulfilled, value),
        None => {
            if let PromiseState::Pending { callbacks } = &mut *state.borrow_mut() {
                callbacks.push(reaction);
            }
        }
    }
    next_state
}

// ── Value construction ─────────────────────────────────────────────────

/// Wrap a state in its JS-facing `Value::Object` (methods as own props).
fn promise_value_from_state(state: &Rc<RefCell<PromiseState>>) -> Value {
    let id = register_state(state.clone());
    let promise = Value::object(HashMap::new());
    promise.set_property(STATE_KEY, Value::Number(id as f64));
    promise.set_property("then", make_then(state.clone()));
    promise.set_property("catch", make_catch(state.clone()));
    promise.set_property("finally", make_finally(state.clone()));
    promise
}

fn make_resolve(state: Rc<RefCell<PromiseState>>) -> Value {
    Value::function(move |_, args| {
        fulfill_state(&state, args.first().cloned().unwrap_or(Value::Undefined));
        Value::Undefined
    })
}

fn make_reject(state: Rc<RefCell<PromiseState>>) -> Value {
    Value::function(move |_, args| {
        reject_state(&state, args.first().cloned().unwrap_or(Value::Undefined));
        Value::Undefined
    })
}

fn make_then(state: Rc<RefCell<PromiseState>>) -> Value {
    Value::function(move |_, args| {
        let on_fulfilled = args.first().cloned().unwrap_or(Value::Undefined);
        let on_rejected = args.get(1).cloned().unwrap_or(Value::Undefined);
        let next_state = subscribe(&state, on_fulfilled, on_rejected);
        promise_value_from_state(&next_state)
    })
}

fn make_catch(state: Rc<RefCell<PromiseState>>) -> Value {
    Value::function(move |_, args| {
        let on_rejected = args.first().cloned().unwrap_or(Value::Undefined);
        let next_state = subscribe(&state, Value::Undefined, on_rejected);
        promise_value_from_state(&next_state)
    })
}

fn make_finally(state: Rc<RefCell<PromiseState>>) -> Value {
    Value::function(move |_, args| {
        let callback = args.first().cloned().unwrap_or(Value::Undefined);
        // On both paths run the callback and then pass the original
        // settlement through; a throwing callback rejects via the reaction
        // runner's `catch_unwind`.
        let on_fulfilled = {
            let callback = callback.clone();
            Value::function(move |_, args| {
                callback.call(Value::Undefined, vec![]);
                args.first().cloned().unwrap_or(Value::Undefined)
            })
        };
        let on_rejected = Value::function(move |_, args| {
            callback.call(Value::Undefined, vec![]);
            crate::throw_value(args.first().cloned().unwrap_or(Value::Undefined));
        });
        let next_state = subscribe(&state, on_fulfilled, on_rejected);
        promise_value_from_state(&next_state)
    })
}

// ── Public API (called from generated code) ────────────────────────────

/// `new Promise(executor)` — executor runs synchronously; a throwing
/// executor rejects the promise.
pub fn new(args: Vec<Value>) -> Value {
    let executor = args.first().cloned().unwrap_or(Value::Undefined);
    let state = pending_state();
    let promise = promise_value_from_state(&state);
    let resolve_fn = make_resolve(state.clone());
    let reject_fn = make_reject(state.clone());
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        executor.call(Value::Undefined, vec![resolve_fn, reject_fn])
    }));
    if let Err(payload) = outcome {
        reject_state(&state, payload_to_value(payload));
    }
    promise
}

/// `Promise.resolve(v)` — returns `v` itself when it is one of our
/// promises, otherwise a fulfilled promise wrapping (assimilating) `v`.
pub fn resolve(args: Vec<Value>) -> Value {
    let value = args.first().cloned().unwrap_or(Value::Undefined);
    if state_of(&value).is_some() {
        return value;
    }
    let state = pending_state();
    let promise = promise_value_from_state(&state);
    fulfill_state(&state, value);
    promise
}

/// `Promise.reject(e)`.
pub fn reject(args: Vec<Value>) -> Value {
    let reason = args.first().cloned().unwrap_or(Value::Undefined);
    let state = pending_state();
    let promise = promise_value_from_state(&state);
    reject_state(&state, reason);
    promise
}

/// `Promise.all(iterable)` — fulfilled with the results array once every
/// item fulfills, or rejected with the first rejection. Empty → `[]`.
pub fn all(args: Vec<Value>) -> Value {
    let items = match args.first() {
        None | Some(Value::Undefined) | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items.borrow().clone(),
        Some(_) => {
            return reject(vec![js_error("TypeError: Promise.all expects an array")]);
        }
    };
    let state = pending_state();
    let promise = promise_value_from_state(&state);
    if items.is_empty() {
        fulfill_state(&state, Value::array(Vec::new()));
        return promise;
    }
    let results = Rc::new(RefCell::new(vec![Value::Undefined; items.len()]));
    let remaining = Rc::new(Cell::new(items.len()));
    for (index, item) in items.into_iter().enumerate() {
        let item_state = pending_state();
        fulfill_state(&item_state, item);
        let on_fulfilled = {
            let results = results.clone();
            let remaining = remaining.clone();
            let state = state.clone();
            Value::function(move |_, args| {
                results.borrow_mut()[index] = args.first().cloned().unwrap_or(Value::Undefined);
                remaining.set(remaining.get() - 1);
                if remaining.get() == 0 {
                    fulfill_state(&state, Value::array(results.borrow().clone()));
                }
                Value::Undefined
            })
        };
        let on_rejected = {
            let state = state.clone();
            Value::function(move |_, args| {
                reject_state(&state, args.first().cloned().unwrap_or(Value::Undefined));
                Value::Undefined
            })
        };
        subscribe(&item_state, on_fulfilled, on_rejected);
    }
    promise
}

/// `Promise.race(iterable)` — settles with the first item that settles.
pub fn race(args: Vec<Value>) -> Value {
    let items = match args.first() {
        Some(Value::Array(items)) => items.borrow().clone(),
        _ => Vec::new(),
    };
    let state = pending_state();
    let promise = promise_value_from_state(&state);
    for item in items {
        let item_state = pending_state();
        fulfill_state(&item_state, item);
        let on_fulfilled = {
            let state = state.clone();
            Value::function(move |_, args| {
                fulfill_state(&state, args.first().cloned().unwrap_or(Value::Undefined));
                Value::Undefined
            })
        };
        let on_rejected = {
            let state = state.clone();
            Value::function(move |_, args| {
                reject_state(&state, args.first().cloned().unwrap_or(Value::Undefined));
                Value::Undefined
            })
        };
        subscribe(&item_state, on_fulfilled, on_rejected);
    }
    promise
}

// ── Microtask queue ────────────────────────────────────────────────────

/// Run every queued reaction job until the queue is empty (jobs may
/// enqueue more jobs). Returns the number of jobs run. Panics crossing a
/// job boundary are contained so one bad job cannot abort the drain.
pub fn drain_microtasks() -> usize {
    let mut ran = 0;
    loop {
        let batch: Vec<Value> =
            PROMISE_MICROTASKS.with(|queue| std::mem::take(&mut *queue.borrow_mut()));
        if batch.is_empty() {
            break;
        }
        for job in batch {
            let _ = catch_unwind(AssertUnwindSafe(|| job.call(Value::Undefined, vec![])));
            ran += 1;
        }
    }
    ran
}

/// Number of reaction jobs currently queued.
pub fn queue_count() -> usize {
    PROMISE_MICROTASKS.with(|queue| queue.borrow().len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    fn recorder(log: Rc<RefCell<Vec<String>>>) -> Value {
        Value::function(move |_, args| {
            log.borrow_mut().push(
                args.first()
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .to_js_string(),
            );
            Value::Undefined
        })
    }

    fn recording_promise(log: Rc<RefCell<Vec<String>>>, tag: &'static str) -> Value {
        Value::function(move |_, args| {
            log.borrow_mut().push(format!(
                "{tag}:{}",
                args.first()
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .to_js_string()
            ));
            Value::Undefined
        })
    }

    #[test]
    fn executor_resolve_then_async() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let promise = new(vec![Value::function(|_, args| {
            args[0].call(Value::Undefined, vec![Value::Number(42.0)]);
            Value::Undefined
        })]);
        promise.call_method("then", vec![recorder(log.clone())]);
        // Reactions are microtasks: nothing ran yet even though `promise`
        // settled synchronously inside the executor.
        assert!(log.borrow().is_empty());
        assert_eq!(drain_microtasks(), 1);
        assert_eq!(log.borrow().as_slice(), &["42".to_string()]);
        assert_eq!(drain_microtasks(), 0);
    }

    #[test]
    fn executor_reject_caught_by_catch() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let promise = new(vec![Value::function(|_, args| {
            args[1].call(Value::Undefined, vec![Value::string("nope")]);
            Value::Undefined
        })]);
        promise.call_method("catch", vec![recorder(log.clone())]);
        drain_microtasks();
        assert_eq!(log.borrow().as_slice(), &["nope".to_string()]);
    }

    #[test]
    fn throwing_executor_rejects() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let promise = new(vec![Value::function(|_, _| {
            crate::throw_value(Value::string("boom"));
        })]);
        promise.call_method("catch", vec![recorder(log.clone())]);
        drain_microtasks();
        assert_eq!(log.borrow().as_slice(), &["boom".to_string()]);
    }

    #[test]
    fn then_chains_in_order() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let promise = resolve(vec![Value::Number(1.0)]);
        let plus_one = Value::function(|_, args| Value::Number(args[0].to_number() + 1.0));
        let chained = promise
            .call_method("then", vec![plus_one.clone()])
            .call_method("then", vec![plus_one])
            .call_method("then", vec![recorder(log.clone())]);
        let _ = chained;
        // Each level is one reaction job; jobs enqueue the next level.
        assert_eq!(drain_microtasks(), 3);
        assert_eq!(log.borrow().as_slice(), &["3".to_string()]);
    }

    #[test]
    fn then_unwraps_returned_promises() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let promise = resolve(vec![Value::Number(7.0)]);
        promise
            .call_method(
                "then",
                vec![Value::function(|_, args| {
                    resolve(vec![resolve(vec![args[0].clone()])])
                })],
            )
            .call_method("then", vec![recorder(log.clone())]);
        drain_microtasks();
        assert_eq!(log.borrow().as_slice(), &["7".to_string()]);
    }

    #[test]
    fn missing_handlers_pass_settlement_through() {
        let log = Rc::new(RefCell::new(Vec::new()));
        resolve(vec![Value::string("v")])
            .call_method("then", vec![])
            .call_method("then", vec![recorder(log.clone())]);
        drain_microtasks();
        assert_eq!(log.borrow().as_slice(), &["v".to_string()]);

        let log = Rc::new(RefCell::new(Vec::new()));
        reject(vec![Value::string("r")])
            .call_method(
                "then",
                vec![Value::function(|_, _| Value::string("unused"))],
            )
            .call_method("catch", vec![recorder(log.clone())]);
        drain_microtasks();
        assert_eq!(log.borrow().as_slice(), &["r".to_string()]);
    }

    #[test]
    fn finally_runs_and_passes_through() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let hit_final = {
            let log = log.clone();
            Value::function(move |_, _| {
                log.borrow_mut().push("finally".to_string());
                Value::Undefined
            })
        };
        resolve(vec![Value::string("kept")])
            .call_method("finally", vec![hit_final.clone()])
            .call_method("then", vec![recorder(log.clone())]);
        drain_microtasks();
        assert_eq!(
            log.borrow().as_slice(),
            &["finally".to_string(), "kept".to_string()]
        );

        let log = Rc::new(RefCell::new(Vec::new()));
        reject(vec![Value::string("still-rejected")])
            .call_method("finally", vec![Value::function(|_, _| Value::Undefined)])
            .call_method("catch", vec![recorder(log.clone())]);
        drain_microtasks();
        assert_eq!(log.borrow().as_slice(), &["still-rejected".to_string()]);
    }

    #[test]
    fn throwing_then_callback_rejects_derived_promise() {
        let log = Rc::new(RefCell::new(Vec::new()));
        resolve(vec![Value::Number(1.0)])
            .call_method(
                "then",
                vec![Value::function(|_, _| {
                    crate::throw_value(Value::string("handler-failed"));
                })],
            )
            .call_method("catch", vec![recorder(log.clone())]);
        drain_microtasks();
        assert_eq!(log.borrow().as_slice(), &["handler-failed".to_string()]);
    }

    #[test]
    fn promise_resolve_returns_same_promise() {
        let promise = resolve(vec![Value::Number(9.0)]);
        let again = resolve(vec![promise.clone()]);
        assert_eq!(promise, again);
    }

    #[test]
    fn all_collects_in_order() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let combined = all(vec![Value::array(vec![
            resolve(vec![Value::Number(1.0)]),
            Value::Number(2.0),
            resolve(vec![resolve(vec![Value::Number(3.0)])]),
        ])]);
        combined.call_method("then", vec![recorder(log.clone())]);
        drain_microtasks();
        assert_eq!(log.borrow().as_slice(), &["1,2,3".to_string()]);
    }

    #[test]
    fn all_rejects_on_first_rejection() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let combined = all(vec![Value::array(vec![
            resolve(vec![Value::Number(1.0)]),
            reject(vec![Value::string("bad")]),
        ])]);
        combined.call_method("catch", vec![recorder(log.clone())]);
        drain_microtasks();
        assert_eq!(log.borrow().as_slice(), &["bad".to_string()]);
    }

    #[test]
    fn all_empty_is_fulfilled_with_empty_array() {
        let log = Rc::new(RefCell::new(Vec::new()));
        all(vec![Value::array(Vec::new())]).call_method("then", vec![recorder(log.clone())]);
        drain_microtasks();
        assert_eq!(log.borrow().as_slice(), &["".to_string()]);
    }

    #[test]
    fn race_takes_first_settlement() {
        let log = Rc::new(RefCell::new(Vec::new()));
        // The never-settling promise must not block the race.
        let pending = new(vec![Value::function(|_, _| Value::Undefined)]);
        race(vec![Value::array(vec![
            pending,
            resolve(vec![Value::string("winner")]),
        ])])
        .call_method("then", vec![recording_promise(log.clone(), "w")]);
        drain_microtasks();
        assert_eq!(log.borrow().as_slice(), &["w:winner".to_string()]);

        let log = Rc::new(RefCell::new(Vec::new()));
        race(vec![Value::array(vec![
            reject(vec![Value::string("first-error")]),
            resolve(vec![Value::Number(1.0)]),
        ])])
        .call_method("catch", vec![recorder(log.clone())]);
        drain_microtasks();
        assert_eq!(log.borrow().as_slice(), &["first-error".to_string()]);
    }

    #[test]
    fn queue_count_tracks_pending_jobs() {
        assert_eq!(queue_count(), 0);
        let promise = resolve(vec![Value::Number(1.0)]);
        promise.call_method("then", vec![Value::function(|_, _| Value::Undefined)]);
        assert_eq!(queue_count(), 1);
        assert_eq!(drain_microtasks(), 1);
        assert_eq!(queue_count(), 0);
    }

    #[test]
    fn executor_panicking_with_string_becomes_error_object() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let log2 = log.clone();
        let promise = new(vec![Value::function(|_, _| panic!("rust-style panic"))]);
        promise.call_method(
            "catch",
            vec![Value::function(move |_, args| {
                log2.borrow_mut()
                    .push(args[0].get_property("message").to_js_string());
                Value::Undefined
            })],
        );
        drain_microtasks();
        assert_eq!(log.borrow().as_slice(), &["rust-style panic".to_string()]);
    }

    #[test]
    fn resolve_is_noop_after_settle() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let promise = new(vec![Value::function(|_, args| {
            args[0].call(Value::Undefined, vec![Value::Number(1.0)]);
            args[0].call(Value::Undefined, vec![Value::Number(2.0)]);
            args[1].call(Value::Undefined, vec![Value::string("late")]);
            Value::Undefined
        })]);
        promise.call_method("then", vec![recorder(log.clone()), recorder(log.clone())]);
        drain_microtasks();
        assert_eq!(log.borrow().as_slice(), &["1".to_string()]);
    }

    #[test]
    fn foreign_thenable_is_assimilated_best_effort() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let thenable = Value::object(HashMap::new());
        thenable.set_property(
            "then",
            Value::function(|_, args| {
                args[0].call(Value::Undefined, vec![Value::string("assimilated")]);
                Value::Undefined
            }),
        );
        resolve(vec![thenable]).call_method("then", vec![recorder(log.clone())]);
        drain_microtasks();
        assert_eq!(log.borrow().as_slice(), &["assimilated".to_string()]);
    }

    #[test]
    fn drain_contains_job_panics() {
        // A job whose handler throws a non-Value payload must not abort the
        // drain (the reaction runner turns it into a rejection instead).
        let promise = resolve(vec![Value::Number(1.0)]);
        promise.call_method("then", vec![Value::function(|_, _| panic!("explode"))]);
        let ran = catch_unwind(AssertUnwindSafe(drain_microtasks));
        assert_eq!(ran.ok(), Some(1));
    }
}
