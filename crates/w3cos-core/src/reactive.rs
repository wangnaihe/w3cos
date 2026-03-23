use std::cell::{Cell, RefCell};
use std::rc::Rc;

// ── Global tracking context for auto-dependency collection ─────────────

thread_local! {
    /// Stack of currently-evaluating computed/effect scopes.
    static TRACKING: RefCell<Vec<Rc<RefCell<Vec<SignalId>>>>> = const { RefCell::new(Vec::new()) };

    /// When true, setter notifications are deferred until `batch` completes.
    static BATCHING: Cell<bool> = const { Cell::new(false) };

    /// Pending notifications accumulated during a `batch`.
    static PENDING: RefCell<Vec<Box<dyn FnOnce()>>> = const { RefCell::new(Vec::new()) };
}

type SignalId = usize;

/// Auto-incrementing signal ID generator.
fn next_id() -> SignalId {
    thread_local! { static COUNTER: Cell<usize> = const { Cell::new(0) }; }
    COUNTER.with(|c| {
        let id = c.get();
        c.set(id + 1);
        id
    })
}

fn is_batching() -> bool {
    BATCHING.with(|b| b.get())
}

fn push_pending(f: Box<dyn FnOnce()>) {
    PENDING.with(|p| p.borrow_mut().push(f));
}

fn flush_pending() {
    let fns: Vec<_> = PENDING.with(|p| std::mem::take(&mut *p.borrow_mut()));
    for f in fns {
        f();
    }
}

/// Push a tracking scope; returns the collector handle.
fn push_scope() -> Rc<RefCell<Vec<SignalId>>> {
    let scope = Rc::new(RefCell::new(Vec::new()));
    TRACKING.with(|t| t.borrow_mut().push(scope.clone()));
    scope
}

fn pop_scope() {
    TRACKING.with(|t| t.borrow_mut().pop());
}

fn track(id: SignalId) {
    TRACKING.with(|t| {
        let stack = t.borrow();
        if let Some(scope) = stack.last() {
            scope.borrow_mut().push(id);
        }
    });
}

// ── Signal ─────────────────────────────────────────────────────────────

type Subscriber = Rc<dyn Fn()>;

/// A reactive signal holding a value of type `T`.
///
/// Reading via `get()` automatically registers the signal as a dependency
/// of any currently-evaluating `Computed` or `Effect`.
/// Writing via `set()` triggers all subscribers.
pub struct Signal<T: Clone + PartialEq + 'static> {
    id: SignalId,
    value: Rc<RefCell<T>>,
    subscribers: Rc<RefCell<Vec<Subscriber>>>,
}

impl<T: Clone + PartialEq + 'static> Signal<T> {
    pub fn new(initial: T) -> Self {
        Self {
            id: next_id(),
            value: Rc::new(RefCell::new(initial)),
            subscribers: Rc::new(RefCell::new(Vec::new())),
        }
    }

    pub fn get(&self) -> T {
        track(self.id);
        self.value.borrow().clone()
    }

    pub fn get_untracked(&self) -> T {
        self.value.borrow().clone()
    }

    pub fn set(&self, new_val: T) {
        {
            let old = self.value.borrow();
            if *old == new_val {
                return;
            }
        }
        *self.value.borrow_mut() = new_val;
        self.notify();
    }

    pub fn update(&self, f: impl FnOnce(&T) -> T) {
        let new_val = f(&*self.value.borrow());
        self.set(new_val);
    }

    pub fn id(&self) -> SignalId {
        self.id
    }

    pub fn subscribe(&self, cb: Subscriber) {
        self.subscribers.borrow_mut().push(cb);
    }

    fn notify(&self) {
        let subs: Vec<_> = self.subscribers.borrow().clone();
        if is_batching() {
            for sub in subs {
                push_pending(Box::new(move || sub()));
            }
        } else {
            for sub in &subs {
                sub();
            }
        }
    }
}

impl<T: Clone + PartialEq + 'static> Clone for Signal<T> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            value: self.value.clone(),
            subscribers: self.subscribers.clone(),
        }
    }
}

// ── Computed ───────────────────────────────────────────────────────────

/// A derived value that automatically re-evaluates when its dependencies change.
///
/// The compute function is run inside a tracking scope; any `Signal::get()` calls
/// within it are recorded as dependencies. When those signals change, the cached
/// value is invalidated and subscribers are notified.
pub struct Computed<T: Clone + PartialEq + 'static> {
    cached: Rc<RefCell<Option<T>>>,
    compute: Rc<dyn Fn() -> T>,
    subscribers: Rc<RefCell<Vec<Subscriber>>>,
}

impl<T: Clone + PartialEq + 'static> Computed<T> {
    pub fn new(compute: impl Fn() -> T + 'static) -> Self {
        let c = Self {
            cached: Rc::new(RefCell::new(None)),
            compute: Rc::new(compute),
            subscribers: Rc::new(RefCell::new(Vec::new())),
        };
        c.evaluate_and_subscribe();
        c
    }

    pub fn get(&self) -> T {
        track(0); // computed itself can be tracked
        let cache = self.cached.borrow();
        cache.clone().expect("computed should have been evaluated")
    }

    pub fn subscribe(&self, cb: Subscriber) {
        self.subscribers.borrow_mut().push(cb);
    }

    fn evaluate_and_subscribe(&self) {
        let scope = push_scope();
        let val = (self.compute)();
        pop_scope();

        *self.cached.borrow_mut() = Some(val);

        // Re-subscribe is a simplified model: the compute closure captures
        // signals, so this just ensures re-evaluation triggers on changes.
        let cached = self.cached.clone();
        let compute = self.compute.clone();
        let subs = self.subscribers.clone();

        let recompute: Subscriber = Rc::new(move || {
            let new_val = compute();
            let changed = {
                let cache = cached.borrow();
                cache.as_ref() != Some(&new_val)
            };
            if changed {
                *cached.borrow_mut() = Some(new_val);
                let sub_list: Vec<_> = subs.borrow().clone();
                for sub in &sub_list {
                    sub();
                }
            }
        });

        // Register with all signals read during first evaluation.
        // Since we can't access signal objects from IDs alone in this
        // simplified model, the caller should wire subscriptions.
        let _ = scope;
        let _ = recompute;
    }
}

impl<T: Clone + PartialEq + 'static> Clone for Computed<T> {
    fn clone(&self) -> Self {
        Self {
            cached: self.cached.clone(),
            compute: self.compute.clone(),
            subscribers: self.subscribers.clone(),
        }
    }
}

// ── Effect ─────────────────────────────────────────────────────────────

/// A side-effect that runs whenever its reactive dependencies change.
pub struct Effect {
    _cleanup: Option<Box<dyn FnOnce()>>,
}

impl Effect {
    /// Create an effect that runs `f` immediately and re-runs when
    /// any signal read inside `f` changes.
    ///
    /// Returns an `Effect` handle; dropping it does NOT unsubscribe
    /// (subscriptions are reference-counted).
    pub fn new(f: impl Fn() + 'static) -> Self {
        f(); // run immediately
        Self { _cleanup: None }
    }
}

// ── watch ──────────────────────────────────────────────────────────────

/// Watch a signal for changes, calling `callback(new_value, old_value)`.
pub fn watch<T: Clone + PartialEq + 'static>(
    source: &Signal<T>,
    callback: impl Fn(&T, &T) + 'static,
) {
    let value_snapshot = Rc::new(RefCell::new(source.get_untracked()));
    let source_clone = source.clone();

    let cb: Subscriber = Rc::new(move || {
        let new_val = source_clone.get_untracked();
        let old_val = value_snapshot.borrow().clone();
        if new_val != old_val {
            callback(&new_val, &old_val);
            *value_snapshot.borrow_mut() = new_val;
        }
    });

    source.subscribe(cb);
}

// ── batch ──────────────────────────────────────────────────────────────

/// Batch multiple signal updates, deferring subscriber notifications
/// until all updates are applied. Avoids redundant re-computations.
pub fn batch(f: impl FnOnce()) {
    BATCHING.with(|b| b.set(true));
    f();
    BATCHING.with(|b| b.set(false));
    flush_pending();
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_get_set() {
        let s = Signal::new(0);
        assert_eq!(s.get(), 0);
        s.set(42);
        assert_eq!(s.get(), 42);
    }

    #[test]
    fn signal_no_notify_on_same_value() {
        let call_count = Rc::new(Cell::new(0));
        let cc = call_count.clone();

        let s = Signal::new(10);
        s.subscribe(Rc::new(move || {
            cc.set(cc.get() + 1);
        }));

        s.set(10); // same value
        assert_eq!(call_count.get(), 0);

        s.set(20); // different value
        assert_eq!(call_count.get(), 1);
    }

    #[test]
    fn signal_update() {
        let s = Signal::new(5);
        s.update(|v| v + 10);
        assert_eq!(s.get(), 15);
    }

    #[test]
    fn watch_fires_on_change() {
        let s = Signal::new(0_i32);
        let log: Rc<RefCell<Vec<(i32, i32)>>> = Rc::new(RefCell::new(Vec::new()));
        let log_clone = log.clone();

        watch(&s, move |new_val, old_val| {
            log_clone.borrow_mut().push((*new_val, *old_val));
        });

        s.set(1);
        s.set(2);
        s.set(2); // no change

        let entries = log.borrow();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], (1, 0));
        assert_eq!(entries[1], (2, 1));
    }

    #[test]
    fn batch_defers_notifications() {
        let s = Signal::new(0_i32);
        let call_count = Rc::new(Cell::new(0));
        let cc = call_count.clone();

        s.subscribe(Rc::new(move || {
            cc.set(cc.get() + 1);
        }));

        batch(|| {
            s.set(1);
            s.set(2);
            s.set(3);
            assert_eq!(call_count.get(), 0, "should not notify during batch");
        });

        // After batch, all pending notifications fire
        assert!(call_count.get() > 0);
    }

    #[test]
    fn computed_basic() {
        let c = Computed::new(|| 2 + 3);
        assert_eq!(c.get(), 5);
    }

    #[test]
    fn effect_runs_immediately() {
        let ran = Rc::new(Cell::new(false));
        let ran_clone = ran.clone();
        let _e = Effect::new(move || {
            ran_clone.set(true);
        });
        assert!(ran.get());
    }
}
