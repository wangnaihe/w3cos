//! JavaScript `Promise` for the ESM compile pipeline.
//!
//! A promise is a `Value::Object` whose own properties are the instance
//! methods `then` / `catch` / `finally` (plain function values, so compiled
//! property access finds them without prototype plumbing) plus the hidden
//! numeric key `__w3cos_promise`. That key is an id into a thread-local
//! weak registry mapping it to the shared `Rc<RefCell<PromiseState>>`.
//! Promise methods and resolver/reaction closures own the state; the weak
//! index lets host code recover it without retaining settled values forever.
//!
//! Reactions never run synchronously: settlement only enqueues reaction
//! jobs onto the thread-local microtask queue ([`PROMISE_MICROTASKS`]),
//! which the embedder drains via [`drain_microtasks`]. Compiled JS `throw`
//! uses [`crate::throw_value`] (`panic_any` of a `Send` wrapper — a
//! bare `Value` is not `Send`); every callback boundary here
//! (`new`'s executor, reaction handlers) catches such panics with
//! `catch_unwind` and turns the payload back into a rejection.
//!
//! Unhandled rejections follow HTML's HostPromiseRejectionTracker shape:
//! a promise rejected with no reactions joins an "about to be notified"
//! list, a later `then` removes it, and the microtask checkpoint reports
//! what is left. Reporting is the `W3COS_JS_CONSOLE=1` debug channel
//! (`[js.unhandledrejection] ...` on stderr, like `[js.throw]`); tracking
//! is off (zero cost) otherwise. v1 limitation: `finally` does not await a
//! promise returned from its callback.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::{Rc, Weak};

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

/// Observable settlement state for embedders that must bridge a JavaScript
/// promise into a synchronous host API.
#[derive(Clone)]
pub enum PromiseStatus {
    Pending,
    Fulfilled(Value),
    Rejected(Value),
}

/// A `then` subscription: the two handlers plus the state of the promise
/// that `then` returned (settled when the matching handler runs).
struct Reaction {
    on_fulfilled: Value,
    on_rejected: Value,
    next_state: Rc<RefCell<PromiseState>>,
    realm_generation: u64,
}

struct MicrotaskJob {
    callback: Value,
    realm_generation: u64,
}

thread_local! {
    /// Microtask queue shared by AOT and W3VM Promise reactions.
    static PROMISE_MICROTASKS: RefCell<Vec<MicrotaskJob>> = const { RefCell::new(Vec::new()) };
    /// Page/Realm generation. Reactions keep the generation in which `.then`
    /// subscribed so old pending Promises cannot schedule stale page code.
    static MICROTASK_REALM_GENERATION: Cell<u64> = const { Cell::new(0) };
    /// State registry keyed by the id stored under [`STATE_KEY`].
    static PROMISE_STATES: RefCell<HashMap<u64, Weak<RefCell<PromiseState>>>> =
        RefCell::new(HashMap::new());
    static NEXT_PROMISE_ID: Cell<u64> = const { Cell::new(1) };
    /// Rejected promises with no reaction yet, with their reasons
    /// (HTML's "about to be notified rejected promises list").
    static UNHANDLED_REJECTIONS: RefCell<Vec<(Weak<RefCell<PromiseState>>, Value)>> =
        const { RefCell::new(Vec::new()) };
    /// Whether rejection tracking is on: `W3COS_JS_CONSOLE` (read once).
    static TRACK_REJECTIONS: Cell<Option<bool>> = const { Cell::new(None) };
    #[cfg(test)]
    static REPORTED_REJECTIONS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn tracking_rejections() -> bool {
    TRACK_REJECTIONS.with(|flag| match flag.get() {
        Some(on) => on,
        None => {
            let on = std::env::var_os("W3COS_JS_CONSOLE").is_some();
            flag.set(Some(on));
            on
        }
    })
}

/// A reaction was added to `state`: it is no longer unhandled.
fn mark_rejection_handled(state: &Rc<RefCell<PromiseState>>) {
    if !tracking_rejections() {
        return;
    }
    UNHANDLED_REJECTIONS.with(|list| {
        list.borrow_mut()
            .retain(|(tracked, _)| !std::ptr::eq(tracked.as_ptr(), Rc::as_ptr(state)))
    });
}

/// Microtask checkpoint: report rejections that are still unhandled.
fn report_unhandled_rejections() {
    if !tracking_rejections() {
        return;
    }
    let pending = UNHANDLED_REJECTIONS.with(|list| std::mem::take(&mut *list.borrow_mut()));
    for (_, reason) in pending {
        let stack = if reason.is_object() {
            reason.get_property("stack")
        } else {
            Value::Undefined
        };
        let message = if reason.is_object() {
            reason.get_property("message")
        } else {
            Value::Undefined
        };
        let detail = if stack.is_string() {
            stack.to_js_string()
        } else if !message.is_undefined() {
            message.to_js_string()
        } else if reason.is_object() && !reason.is_function() {
            // A plain object reason (`reject({ code })`): show its fields,
            // not "[object Object]".
            let json = crate::value::catch_js(|| crate::json::stringify(vec![reason.clone()]));
            match json {
                Ok(json) if json.is_string() => json.to_js_string(),
                _ => reason.to_js_string(),
            }
        } else {
            reason.to_js_string()
        };
        eprintln!("[js.unhandledrejection] Uncaught (in promise) {detail}");
        #[cfg(test)]
        REPORTED_REJECTIONS.with(|reported| reported.borrow_mut().push(detail));
    }
}

fn register_state(state: Rc<RefCell<PromiseState>>) -> u64 {
    let id = NEXT_PROMISE_ID.with(|counter| {
        let id = counter.get();
        counter.set(id + 1);
        id
    });
    PROMISE_STATES.with(|registry| {
        let mut registry = registry.borrow_mut();
        registry.retain(|_, state| state.strong_count() != 0);
        registry.insert(id, Rc::downgrade(&state));
    });
    id
}

/// The shared state behind `value`, when `value` is one of our promises.
fn state_of(value: &Value) -> Option<Rc<RefCell<PromiseState>>> {
    if let Some(object) = value.as_object() {
        if let Some(id) = object.borrow().get_direct(STATE_KEY).as_number() {
            return PROMISE_STATES
                .with(|registry| registry.borrow().get(&(id as u64)).and_then(Weak::upgrade));
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
    if !is_fulfilled && callbacks.is_empty() && tracking_rejections() {
        UNHANDLED_REJECTIONS.with(|list| {
            list.borrow_mut()
                .push((Rc::downgrade(state), value.clone()))
        });
    }
    for reaction in callbacks {
        schedule_reaction(reaction, is_fulfilled, value.clone());
    }
}

/// Enqueue one reaction run as a microtask job.
fn schedule_reaction(reaction: Reaction, is_fulfilled: bool, value: Value) {
    let current_generation = MICROTASK_REALM_GENERATION.with(Cell::get);
    if reaction.realm_generation != current_generation {
        return;
    }
    let realm_generation = reaction.realm_generation;
    let job = Value::function(move |_, _| {
        run_reaction(&reaction, is_fulfilled, value.clone());
        Value::Undefined
    });
    queue_microtask_for_generation(job, realm_generation);
}

fn queue_microtask_for_generation(callback: Value, realm_generation: u64) {
    PROMISE_MICROTASKS.with(|queue| {
        queue.borrow_mut().push(MicrotaskJob {
            callback,
            realm_generation,
        })
    });
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
        realm_generation: MICROTASK_REALM_GENERATION.with(Cell::get),
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
        Some((is_fulfilled, value)) => {
            if !is_fulfilled {
                mark_rejection_handled(state);
            }
            schedule_reaction(reaction, is_fulfilled, value)
        }
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
    let items = match combinator_items(&args, "Promise.all expects an iterable") {
        Ok(items) => items,
        Err(error) => return reject(vec![error]),
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
    let items = match combinator_items(&args, "Promise.race expects an iterable") {
        Ok(items) => items,
        Err(error) => return reject(vec![error]),
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

/// The items of a `Promise.<combinator>(array)` argument (`None` when it is
/// not an array; nullish counts as empty, like [`all`]).
/// Items of a combinator's `iterable` argument: arrays read their slots
/// directly; any other iterable (Set, Map, generators, typed arrays,
/// strings, `__w3cos_symbol_iterator` objects) is iterated, as
/// `Promise.all(new Set(promises))` does in Node.
///
/// An abrupt completion while iterating (a throwing `next` or `done`
/// getter) is returned as `Err` so the combinator rejects instead of
/// throwing synchronously (IfAbruptRejectPromise); the iterator is not
/// closed, as the step itself failed. Non-iterables give a `TypeError`.
fn combinator_items(args: &[Value], not_iterable: &str) -> Result<Vec<Value>, Value> {
    match args.first() {
        None => Ok(Vec::new()),
        Some(v) if v.is_nullish() => Ok(Vec::new()),
        Some(items) if items.as_array().is_some() => Ok(items
            .as_array()
            .expect("array")
            .borrow()
            .iter()
            .cloned()
            .map(crate::value::array_slot_value)
            .collect()),
        Some(items) if items.is_iterable() => {
            crate::value::catch_js_result(|| Ok(items.iter().collect()))
        }
        Some(_) => Err(crate::value::type_error(not_iterable)),
    }
}

fn settled_record(status: &str, key: &str, value: Value) -> Value {
    let record = Value::object(HashMap::new());
    record.set_property("status", Value::string(status));
    record.set_property(key, value);
    record
}

/// `Promise.allSettled(iterable)` — fulfilled, once every item settles,
/// with `{ status: "fulfilled", value }` / `{ status: "rejected", reason }`
/// records in input order. Never rejects for settled inputs.
pub fn all_settled(args: Vec<Value>) -> Value {
    let items = match combinator_items(&args, "Promise.allSettled expects an iterable") {
        Ok(items) => items,
        Err(error) => return reject(vec![error]),
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
        let record = |status: &'static str, key: &'static str| {
            let results = results.clone();
            let remaining = remaining.clone();
            let state = state.clone();
            Value::function(move |_, args| {
                let value = args.first().cloned().unwrap_or(Value::Undefined);
                results.borrow_mut()[index] = settled_record(status, key, value);
                remaining.set(remaining.get() - 1);
                if remaining.get() == 0 {
                    fulfill_state(&state, Value::array(results.borrow().clone()));
                }
                Value::Undefined
            })
        };
        let on_fulfilled = record("fulfilled", "value");
        let on_rejected = record("rejected", "reason");
        subscribe(&item_state, on_fulfilled, on_rejected);
    }
    promise
}

fn all_rejected_error(errors: Vec<Value>) -> Value {
    crate::builtins::error_instance(
        "AggregateError",
        vec![
            Value::array(errors),
            Value::string("All promises were rejected"),
        ],
    )
}

/// `Promise.any(iterable)` — fulfilled with the first item that fulfills;
/// rejected with an `AggregateError` of every reason (input order) when all
/// of them reject, or immediately for an empty input.
pub fn any(args: Vec<Value>) -> Value {
    let items = match combinator_items(&args, "Promise.any expects an iterable") {
        Ok(items) => items,
        Err(error) => return reject(vec![error]),
    };
    let state = pending_state();
    let promise = promise_value_from_state(&state);
    if items.is_empty() {
        reject_state(&state, all_rejected_error(Vec::new()));
        return promise;
    }
    let errors = Rc::new(RefCell::new(vec![Value::Undefined; items.len()]));
    let remaining = Rc::new(Cell::new(items.len()));
    for (index, item) in items.into_iter().enumerate() {
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
            let errors = errors.clone();
            let remaining = remaining.clone();
            let state = state.clone();
            Value::function(move |_, args| {
                errors.borrow_mut()[index] = args.first().cloned().unwrap_or(Value::Undefined);
                remaining.set(remaining.get() - 1);
                if remaining.get() == 0 {
                    reject_state(&state, all_rejected_error(errors.borrow().clone()));
                }
                Value::Undefined
            })
        };
        subscribe(&item_state, on_fulfilled, on_rejected);
    }
    promise
}

/// `Promise.withResolvers()` — `{ promise, resolve, reject }`.
pub fn with_resolvers(_args: Vec<Value>) -> Value {
    let state = pending_state();
    let result = Value::object(HashMap::new());
    result.set_property("promise", promise_value_from_state(&state));
    result.set_property("resolve", make_resolve(state.clone()));
    result.set_property("reject", make_reject(state));
    result
}

// ── Microtask queue ────────────────────────────────────────────────────

/// Enqueue a host or JavaScript microtask in the same FIFO used by Promise
/// reactions. Browsers expose one microtask queue, so embedder jobs such as
/// MutationObserver delivery must not be drained in a separate phase.
pub fn queue_microtask(callback: Value) {
    if callback.is_function() {
        let realm_generation = MICROTASK_REALM_GENERATION.with(Cell::get);
        queue_microtask_for_generation(callback, realm_generation);
    }
}

/// Run every queued reaction job until the queue is empty (jobs may
/// enqueue more jobs), then any pending `FinalizationRegistry` cleanups
/// (see [`crate::weak`] for ordering). Returns the number of jobs run. Panics crossing a
/// job boundary are contained so one bad job cannot abort the drain.
pub fn drain_microtasks() -> usize {
    let mut ran = 0;
    loop {
        let batch: Vec<MicrotaskJob> =
            PROMISE_MICROTASKS.with(|queue| std::mem::take(&mut *queue.borrow_mut()));
        if batch.is_empty() {
            // Job checkpoint: deliver FinalizationRegistry cleanups for
            // targets released so far, then drain whatever they queued.
            let finalized = crate::weak::run_pending_finalizers();
            if finalized == 0 {
                report_unhandled_rejections();
                break;
            }
            ran += finalized;
            continue;
        }
        let current_generation = MICROTASK_REALM_GENERATION.with(Cell::get);
        for job in batch {
            if job.realm_generation != current_generation {
                continue;
            }
            let _ = catch_unwind(AssertUnwindSafe(|| {
                job.callback.call(Value::Undefined, vec![])
            }));
            ran += 1;
        }
    }
    ran
}

/// Number of reaction jobs currently queued.
pub fn queue_count() -> usize {
    let current_generation = MICROTASK_REALM_GENERATION.with(Cell::get);
    PROMISE_MICROTASKS.with(|queue| {
        queue
            .borrow()
            .iter()
            .filter(|job| job.realm_generation == current_generation)
            .count()
    })
}

/// Start a fresh page/Realm microtask generation.
///
/// Existing Promise states remain valid so a new Realm can subscribe to an
/// already-settled shared AOT module Promise. Queued reactions and subscriptions
/// created by the previous Realm can no longer execute.
pub fn advance_realm_generation() {
    MICROTASK_REALM_GENERATION.with(|generation| {
        generation.set(generation.get().wrapping_add(1));
    });
    PROMISE_MICROTASKS.with(|queue| queue.borrow_mut().clear());
}

/// Returns the current settlement state when `value` is one of the promises
/// owned by this runtime.
pub fn status(value: &Value) -> Option<PromiseStatus> {
    let state = state_of(value)?;
    let status = match &*state.borrow() {
        PromiseState::Pending { .. } => PromiseStatus::Pending,
        PromiseState::Fulfilled(value) => PromiseStatus::Fulfilled(value.clone()),
        PromiseState::Rejected(reason) => PromiseStatus::Rejected(reason.clone()),
    };
    Some(status)
}

#[cfg(test)]
mod tests {
    #[test]
    fn unhandled_rejections_are_reported_at_the_microtask_checkpoint() {
        TRACK_REJECTIONS.with(|flag| flag.set(Some(true)));
        REPORTED_REJECTIONS.with(|reported| reported.borrow_mut().clear());
        let call = |target: &Value, method: &str, args: Vec<Value>| {
            crate::intrinsics::call_method(target, &Value::string(method), args)
        };
        // Unhandled: rejected and never observed.
        let _lost = reject(vec![Value::string("lost")]);
        // Handled before the checkpoint: no report.
        let caught = reject(vec![Value::string("caught")]);
        let _ = call(
            &caught,
            "catch",
            vec![Value::function(|_, _| Value::Undefined)],
        );
        // `then` without a rejection handler hands the rejection on to the
        // derived promise, which is the unhandled one.
        let passed = reject(vec![Value::string("passed on")]);
        let _derived = call(
            &passed,
            "then",
            vec![Value::function(|_, _| Value::Undefined)],
        );
        // A handler throwing inside a reaction rejects its derived promise.
        let thrown = resolve(vec![Value::Number(1.0)]);
        let _thrown_derived = call(
            &thrown,
            "then",
            vec![Value::function(|_, _| {
                crate::throw_value(Value::string("thrown"))
            })],
        );
        // Objects without a stack show their message, or else their JSON.
        let with_message = Value::object(HashMap::new());
        with_message.set_property("message", Value::string("has message"));
        let _message = reject(vec![with_message]);
        let plain = Value::object(HashMap::new());
        plain.set_property("code", Value::string("E1"));
        let _plain = reject(vec![plain]);
        drain_microtasks();
        let mut reported = REPORTED_REJECTIONS.with(|reported| reported.take());
        reported.sort();
        assert_eq!(
            reported,
            [
                "has message",
                "lost",
                "passed on",
                "thrown",
                r#"{"code":"E1"}"#
            ]
        );
        TRACK_REJECTIONS.with(|flag| flag.set(None));
    }

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
        assert_eq!(
            status(&promise).and_then(|status| match status {
                PromiseStatus::Fulfilled(value) => value.as_number(),
                _ => None,
            }),
            Some(42.0)
        );
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
    fn host_microtasks_share_fifo_order_with_promise_reactions() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let before_log = Rc::clone(&log);
        queue_microtask(Value::function(move |_, _| {
            before_log.borrow_mut().push("host-before".to_string());
            Value::Undefined
        }));
        let promise_log = Rc::clone(&log);
        resolve(vec![]).call_method(
            "then",
            vec![Value::function(move |_, _| {
                promise_log.borrow_mut().push("promise".to_string());
                Value::Undefined
            })],
        );
        let after_log = Rc::clone(&log);
        queue_microtask(Value::function(move |_, _| {
            after_log.borrow_mut().push("host-after".to_string());
            Value::Undefined
        }));

        assert_eq!(drain_microtasks(), 3);
        assert_eq!(
            log.borrow().as_slice(),
            &["host-before", "promise", "host-after"]
        );
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
    fn combinators_reject_when_iteration_throws() {
        // test262 Promise/*/iter-step-err-no-close: a throwing `next`
        // rejects the combinator's promise instead of throwing.
        let poisoned = || {
            let iterable = Value::object(HashMap::new());
            iterable.set_property(
                "__w3cos_symbol_iterator",
                Value::function(|_, _| {
                    let iterator = Value::object(HashMap::new());
                    iterator.set_property(
                        "next",
                        Value::function(|_, _| crate::throw_value(Value::string("step"))),
                    );
                    iterator
                }),
            );
            iterable
        };
        let log = Rc::new(RefCell::new(Vec::new()));
        for promise in [
            all(vec![poisoned()]),
            race(vec![poisoned()]),
            any(vec![poisoned()]),
            all_settled(vec![poisoned()]),
        ] {
            let log = log.clone();
            promise.call_method(
                "catch",
                vec![Value::function(move |_, args| {
                    log.borrow_mut().push(
                        args.first()
                            .cloned()
                            .unwrap_or(Value::Undefined)
                            .to_js_string(),
                    );
                    Value::Undefined
                })],
            );
        }
        drain_microtasks();
        assert_eq!(*log.borrow(), vec!["step"; 4]);
    }

    #[test]
    fn combinators_accept_non_array_iterables() {
        let set = || {
            crate::intrinsics::construct(
                &crate::collections::set_class(),
                vec![Value::array(vec![Value::Number(1.0), Value::Number(2.0)])],
            )
        };
        let log = Rc::new(RefCell::new(Vec::new()));
        all(vec![set()]).call_method("then", vec![recorder(log.clone())]);
        race(vec![set()]).call_method("then", vec![recorder(log.clone())]);
        any(vec![set()]).call_method("then", vec![recorder(log.clone())]);
        let settled = all_settled(vec![set()]);
        settled.call_method(
            "then",
            vec![{
                let log = log.clone();
                Value::function(move |_, args| {
                    let records = args.first().cloned().unwrap_or(Value::Undefined);
                    let first = records.get_property("0");
                    log.borrow_mut().push(format!(
                        "settled {} {}",
                        records.get_property("length").to_js_string(),
                        first.get_property("status").to_js_string()
                    ));
                    Value::Undefined
                })
            }],
        );
        all(vec![Value::Number(3.0)]).call_method(
            "catch",
            vec![{
                let log = log.clone();
                Value::function(move |_, args| {
                    let error = args.first().cloned().unwrap_or(Value::Undefined);
                    log.borrow_mut()
                        .push(error.get_property("message").to_js_string());
                    Value::Undefined
                })
            }],
        );
        drain_microtasks();
        let mut got = log.borrow().clone();
        got.sort();
        assert_eq!(
            got,
            vec![
                "1".to_string(),
                "1".to_string(),
                "1,2".to_string(),
                "Promise.all expects an iterable".to_string(),
                "settled 2 fulfilled".to_string(),
            ]
        );
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
    fn realm_advance_discards_already_queued_reactions() {
        let log = Rc::new(RefCell::new(Vec::new()));
        resolve(vec![Value::string("stale")]).call_method("then", vec![recorder(Rc::clone(&log))]);
        assert_eq!(queue_count(), 1);

        advance_realm_generation();

        assert_eq!(queue_count(), 0);
        assert_eq!(drain_microtasks(), 0);
        assert!(log.borrow().is_empty());
    }

    #[test]
    fn old_pending_subscriptions_cannot_enqueue_after_realm_advance() {
        let resolve_slot = Rc::new(RefCell::new(Value::Undefined));
        let executor_slot = Rc::clone(&resolve_slot);
        let promise = new(vec![Value::function(move |_, arguments| {
            *executor_slot.borrow_mut() = arguments.first().cloned().unwrap_or(Value::Undefined);
            Value::Undefined
        })]);
        let log = Rc::new(RefCell::new(Vec::new()));
        promise.call_method("then", vec![recorder(Rc::clone(&log))]);

        advance_realm_generation();
        resolve_slot
            .borrow()
            .call(Value::Undefined, vec![Value::string("stale")]);

        assert_eq!(queue_count(), 0);
        assert_eq!(drain_microtasks(), 0);
        assert!(log.borrow().is_empty());
    }

    #[test]
    fn new_realm_can_subscribe_to_an_already_settled_shared_promise() {
        let shared = resolve(vec![Value::string("cached-aot")]);
        advance_realm_generation();
        let log = Rc::new(RefCell::new(Vec::new()));

        shared.call_method("then", vec![recorder(Rc::clone(&log))]);

        assert_eq!(drain_microtasks(), 1);
        assert_eq!(log.borrow().as_slice(), &["cached-aot".to_string()]);
    }

    #[test]
    fn dropped_promises_do_not_retain_settled_values_in_the_state_index() {
        let marker = Rc::new(());
        let marker_weak = Rc::downgrade(&marker);
        {
            let settled_marker = Rc::clone(&marker);
            let settled_value = Value::function(move |_, _| {
                let _ = &settled_marker;
                Value::Undefined
            });
            let promise = resolve(vec![settled_value]);
            assert!(matches!(
                status(&promise),
                Some(PromiseStatus::Fulfilled(value)) if value.is_function()
            ));
        }
        crate::page_arena::reset();
        drop(marker);

        assert!(marker_weak.upgrade().is_none());
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
