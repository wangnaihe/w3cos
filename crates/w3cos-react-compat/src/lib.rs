//! React hooks compatibility layer for W3C OS.
//!
//! Mirrors the React Hooks API on top of the [`w3cos_core`] signal system,
//! so a function-component style render API can be authored verbatim:
//!
//! ```ignore
//! use w3cos_react_compat::*;
//!
//! fn Counter() {
//!     let count = use_state::<i64>(|| 0);
//!     let label = use_memo(|| format!("count={}", count.get()), &[count.deps()]);
//!
//!     use_effect(
//!         || println!("rendered: {}", label.get()),
//!         &[count.deps()],
//!     );
//!
//!     let inc = use_callback(
//!         {
//!             let count = count.clone();
//!             move || count.set(count.get() + 1)
//!         },
//!         &[count.deps()],
//!     );
//!     inc();
//! }
//! ```
//!
//! ## Hook semantics
//!
//! React tracks hooks by their *call order* within a component body. We do
//! the same: each render frame the runtime calls
//! [`begin_render(component_id)`](begin_render) and the hook calls allocate
//! slot `0`, `1`, `2`, … from the per-component slot table. The slot table
//! survives across renders — that's how `use_state` keeps its value.
//!
//! When a `useState` setter is invoked, the corresponding `Signal` notifies
//! its subscribers, which in turn schedule the component for re-render via
//! [`mark_dirty`]. The host loop drives that re-render by calling
//! [`begin_render`] again with the same `component_id` and replaying the
//! component body.

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use w3cos_core::{batch, Effect, Signal};

// ---------------------------------------------------------------------------
// Internal slot model
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ComponentId(pub u64);

enum HookSlot {
    State(Box<dyn Any>),
    Effect {
        deps: Vec<u64>,
        cleanup: Option<Box<dyn FnOnce()>>,
    },
    Memo {
        deps: Vec<u64>,
        value: Box<dyn Any>,
    },
    Ref(Rc<RefCell<Box<dyn Any>>>),
    Reducer(Box<dyn Any>),
    Context,
}

#[derive(Default)]
struct ComponentInstance {
    slots: Vec<HookSlot>,
    /// Effects whose cleanup should run when the component is dropped.
    pending_cleanups: Vec<Box<dyn FnOnce()>>,
}

#[derive(Default)]
struct HostState {
    components: HashMap<ComponentId, ComponentInstance>,
    /// Components whose dependencies changed and that should be re-rendered
    /// on the next host tick.
    dirty: Vec<ComponentId>,
    /// Stack of currently-rendering components (for nested rendering).
    active: Vec<RenderFrame>,
    /// Globally provided context values keyed by `(component_root, type-name)`.
    contexts: HashMap<&'static str, Box<dyn Any>>,
}

struct RenderFrame {
    id: ComponentId,
    cursor: usize,
}

thread_local! {
    static HOST: RefCell<HostState> = RefCell::new(HostState::default());
}

fn with_host<R>(f: impl FnOnce(&mut HostState) -> R) -> R {
    HOST.with(|h| f(&mut h.borrow_mut()))
}

// ---------------------------------------------------------------------------
// Render lifecycle
// ---------------------------------------------------------------------------

/// Allocate (or reuse) state for `id` and start a render frame. Hook calls
/// inside this scope read from `id`'s slot table in declaration order.
///
/// Must be paired with [`end_render`].
pub fn begin_render(id: ComponentId) {
    with_host(|host| {
        host.components.entry(id).or_default();
        host.active.push(RenderFrame { id, cursor: 0 });
    });
}

/// Close the current render frame. Asserts that `id` matches the
/// most-recent [`begin_render`].
pub fn end_render(id: ComponentId) {
    with_host(|host| {
        let frame = host
            .active
            .pop()
            .expect("end_render without begin_render");
        debug_assert_eq!(frame.id, id, "render frame id mismatch");
    });
}

/// Check whether the component is currently mid-render. Useful as a guard
/// for hook calls outside the render lifecycle.
pub fn is_rendering() -> bool {
    with_host(|h| !h.active.is_empty())
}

/// Mark `id` for re-render. Hosts poll [`take_dirty`] each tick.
pub fn mark_dirty(id: ComponentId) {
    with_host(|host| {
        if !host.dirty.contains(&id) {
            host.dirty.push(id);
        }
    });
}

/// Drain components flagged for re-render. Intended for the runtime's
/// reactive loop.
pub fn take_dirty() -> Vec<ComponentId> {
    with_host(|host| std::mem::take(&mut host.dirty))
}

/// Unmount a component — runs all pending effect cleanups and frees state.
pub fn unmount(id: ComponentId) {
    let cleanups = with_host(|host| {
        host.dirty.retain(|d| *d != id);
        host.components
            .remove(&id)
            .map(|c| c.pending_cleanups)
            .unwrap_or_default()
    });
    for cleanup in cleanups {
        cleanup();
    }
}

/// Total number of mounted components — exposed for diagnostics.
pub fn mounted_count() -> usize {
    with_host(|h| h.components.len())
}

// ---------------------------------------------------------------------------
// Hook helpers
// ---------------------------------------------------------------------------

fn current_frame_cursor() -> (ComponentId, usize) {
    with_host(|host| {
        let frame = host
            .active
            .last_mut()
            .expect("hook called outside of begin_render/end_render scope");
        let cursor = frame.cursor;
        frame.cursor += 1;
        (frame.id, cursor)
    })
}

fn ensure_slot<F: FnOnce() -> HookSlot>(id: ComponentId, idx: usize, init: F) {
    with_host(|host| {
        let comp = host.components.get_mut(&id).expect("component missing");
        if comp.slots.len() <= idx {
            comp.slots.push(init());
        }
    });
}

fn with_slot<R, F>(id: ComponentId, idx: usize, f: F) -> R
where
    F: FnOnce(&mut HookSlot) -> R,
{
    with_host(|host| {
        let comp = host.components.get_mut(&id).expect("component missing");
        let slot = comp.slots.get_mut(idx).expect("hook slot missing");
        f(slot)
    })
}

// ---------------------------------------------------------------------------
// useState
// ---------------------------------------------------------------------------

/// `useState` — returns a [`StateHook`] wrapping a [`Signal`].
pub fn use_state<T>(initial: impl FnOnce() -> T) -> StateHook<T>
where
    T: Clone + PartialEq + 'static,
{
    let (id, idx) = current_frame_cursor();
    ensure_slot(id, idx, || HookSlot::State(Box::new(Signal::new(initial()))));
    let signal = with_slot(id, idx, |slot| {
        if let HookSlot::State(boxed) = slot {
            boxed
                .downcast_ref::<Signal<T>>()
                .expect("useState slot type mismatch")
                .clone()
        } else {
            panic!("hook slot at index {idx} was not useState");
        }
    });

    // Subscribe the host re-render to value changes for this component.
    let dirty_id = id;
    signal.subscribe(Rc::new(move || mark_dirty(dirty_id)));

    StateHook { signal }
}

/// Handle returned from [`use_state`]. Mirrors `[value, setValue]` from React.
pub struct StateHook<T: Clone + PartialEq + 'static> {
    signal: Signal<T>,
}

impl<T: Clone + PartialEq + 'static> StateHook<T> {
    pub fn get(&self) -> T {
        self.signal.get_untracked()
    }

    pub fn set(&self, value: T) {
        self.signal.set(value);
    }

    pub fn update(&self, f: impl FnOnce(&T) -> T) {
        self.signal.update(f);
    }

    /// Stable identity used by [`use_effect`] / [`use_memo`] / [`use_callback`]
    /// when listing this state as a dependency.
    pub fn deps(&self) -> u64 {
        self.signal.id() as u64
    }

    pub fn signal(&self) -> Signal<T> {
        self.signal.clone()
    }
}

impl<T: Clone + PartialEq + 'static> Clone for StateHook<T> {
    fn clone(&self) -> Self {
        Self {
            signal: self.signal.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// useEffect
// ---------------------------------------------------------------------------

/// `useEffect(fn, deps)` — runs `fn` after each render whose dependency list
/// changed. Returning a cleanup closure mirrors React's cleanup semantics.
pub fn use_effect<F>(effect: F, deps: &[u64])
where
    F: FnOnce() -> Option<Box<dyn FnOnce()>> + 'static,
{
    let (id, idx) = current_frame_cursor();
    let mut should_run = false;

    ensure_slot(id, idx, || {
        should_run = true;
        HookSlot::Effect {
            deps: deps.to_vec(),
            cleanup: None,
        }
    });

    if !should_run {
        let prev_deps = with_slot(id, idx, |slot| {
            if let HookSlot::Effect { deps: prev, .. } = slot {
                prev.clone()
            } else {
                panic!("hook slot at index {idx} was not useEffect");
            }
        });
        if prev_deps != deps {
            should_run = true;
        }
    }

    if should_run {
        // Run cleanup from prior invocation first.
        let prior_cleanup = with_slot(id, idx, |slot| {
            if let HookSlot::Effect { deps: prev, cleanup } = slot {
                *prev = deps.to_vec();
                cleanup.take()
            } else {
                None
            }
        });
        if let Some(c) = prior_cleanup {
            c();
        }
        let cleanup = effect();
        with_slot(id, idx, |slot| {
            if let HookSlot::Effect {
                cleanup: stored, ..
            } = slot
            {
                *stored = cleanup;
            }
        });
    }
}

// ---------------------------------------------------------------------------
// useMemo
// ---------------------------------------------------------------------------

/// `useMemo(compute, deps)` — caches the value produced by `compute` and
/// re-runs it whenever any dependency changes.
pub fn use_memo<T, F>(compute: F, deps: &[u64]) -> MemoHook<T>
where
    F: FnOnce() -> T,
    T: Clone + 'static,
{
    let (id, idx) = current_frame_cursor();

    let mut needs_compute = false;
    ensure_slot(id, idx, || {
        needs_compute = true;
        HookSlot::Memo {
            deps: deps.to_vec(),
            value: Box::new(()), // placeholder — replaced immediately below
        }
    });

    if !needs_compute {
        let prev_deps = with_slot(id, idx, |slot| {
            if let HookSlot::Memo { deps: prev, .. } = slot {
                prev.clone()
            } else {
                panic!("hook slot at index {idx} was not useMemo");
            }
        });
        if prev_deps != deps {
            needs_compute = true;
        }
    }

    if needs_compute {
        let new_value = compute();
        with_slot(id, idx, |slot| {
            if let HookSlot::Memo {
                deps: stored_deps,
                value,
            } = slot
            {
                *stored_deps = deps.to_vec();
                *value = Box::new(new_value);
            }
        });
    }

    let value = with_slot(id, idx, |slot| {
        if let HookSlot::Memo { value, .. } = slot {
            value
                .downcast_ref::<T>()
                .expect("useMemo slot type mismatch")
                .clone()
        } else {
            panic!("hook slot at index {idx} was not useMemo");
        }
    });

    MemoHook { value }
}

pub struct MemoHook<T> {
    value: T,
}

impl<T: Clone> MemoHook<T> {
    pub fn get(&self) -> T {
        self.value.clone()
    }
}

// ---------------------------------------------------------------------------
// useCallback (memoised closure container)
// ---------------------------------------------------------------------------

/// `useCallback(fn, deps)` — memoises a closure. Identity is preserved
/// across renders unless `deps` changes. Internally backed by
/// `Rc<RefCell<...>>` so callers can call it multiple times.
pub fn use_callback<F>(callback: F, deps: &[u64]) -> Rc<RefCell<F>>
where
    F: 'static,
{
    let (id, idx) = current_frame_cursor();
    let mut needs_replace = false;
    ensure_slot(id, idx, || {
        needs_replace = true;
        HookSlot::Memo {
            deps: deps.to_vec(),
            value: Box::new(()),
        }
    });

    if !needs_replace {
        let prev_deps = with_slot(id, idx, |slot| {
            if let HookSlot::Memo { deps: prev, .. } = slot {
                prev.clone()
            } else {
                panic!("hook slot at index {idx} was not useCallback");
            }
        });
        if prev_deps != deps {
            needs_replace = true;
        }
    }

    if needs_replace {
        let cell = Rc::new(RefCell::new(callback));
        with_slot(id, idx, |slot| {
            if let HookSlot::Memo {
                deps: stored,
                value,
            } = slot
            {
                *stored = deps.to_vec();
                *value = Box::new(cell.clone());
            }
        });
    }

    with_slot(id, idx, |slot| {
        if let HookSlot::Memo { value, .. } = slot {
            value
                .downcast_ref::<Rc<RefCell<F>>>()
                .expect("useCallback slot type mismatch")
                .clone()
        } else {
            panic!("hook slot at index {idx} was not useCallback");
        }
    })
}

// ---------------------------------------------------------------------------
// useRef
// ---------------------------------------------------------------------------

/// `useRef(initial)` — returns a stable mutable reference. Mutating the
/// ref does NOT trigger a re-render.
pub fn use_ref<T>(initial: impl FnOnce() -> T) -> RefHook<T>
where
    T: 'static,
{
    let (id, idx) = current_frame_cursor();
    ensure_slot(id, idx, || {
        HookSlot::Ref(Rc::new(RefCell::new(Box::new(initial()))))
    });
    let inner = with_slot(id, idx, |slot| {
        if let HookSlot::Ref(rc) = slot {
            Rc::clone(rc)
        } else {
            panic!("hook slot at index {idx} was not useRef");
        }
    });
    RefHook {
        inner,
        _phantom: std::marker::PhantomData,
    }
}

pub struct RefHook<T: 'static> {
    inner: Rc<RefCell<Box<dyn Any>>>,
    _phantom: std::marker::PhantomData<T>,
}

impl<T: 'static> RefHook<T> {
    /// `ref.current` (mutable).
    pub fn with_mut<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let mut guard = self.inner.borrow_mut();
        let current = guard
            .downcast_mut::<T>()
            .expect("useRef type mismatch");
        f(current)
    }

    /// `ref.current` (read-only).
    pub fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        let guard = self.inner.borrow();
        let current = guard
            .downcast_ref::<T>()
            .expect("useRef type mismatch");
        f(current)
    }
}

impl<T: 'static> Clone for RefHook<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Rc::clone(&self.inner),
            _phantom: std::marker::PhantomData,
        }
    }
}

// ---------------------------------------------------------------------------
// useReducer
// ---------------------------------------------------------------------------

/// `useReducer(reducer, initial)` — Redux-style state container.
pub fn use_reducer<S, A, R>(reducer: R, initial: impl FnOnce() -> S) -> ReducerHook<S, A>
where
    S: Clone + PartialEq + 'static,
    A: 'static,
    R: Fn(S, A) -> S + 'static,
{
    let (id, idx) = current_frame_cursor();
    ensure_slot(id, idx, || {
        let state = Signal::new(initial());
        let pair = ReducerPair::<S, A> {
            state,
            reducer: Rc::new(reducer),
        };
        HookSlot::Reducer(Box::new(pair))
    });
    let pair = with_slot(id, idx, |slot| {
        if let HookSlot::Reducer(boxed) = slot {
            boxed
                .downcast_ref::<ReducerPair<S, A>>()
                .expect("useReducer slot type mismatch")
                .clone()
        } else {
            panic!("hook slot at index {idx} was not useReducer");
        }
    });

    let dirty_id = id;
    pair.state.subscribe(Rc::new(move || mark_dirty(dirty_id)));

    ReducerHook { pair }
}

struct ReducerPair<S, A>
where
    S: Clone + PartialEq + 'static,
    A: 'static,
{
    state: Signal<S>,
    reducer: Rc<dyn Fn(S, A) -> S>,
}

impl<S, A> Clone for ReducerPair<S, A>
where
    S: Clone + PartialEq + 'static,
    A: 'static,
{
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            reducer: Rc::clone(&self.reducer),
        }
    }
}

pub struct ReducerHook<S, A>
where
    S: Clone + PartialEq + 'static,
    A: 'static,
{
    pair: ReducerPair<S, A>,
}

impl<S, A> ReducerHook<S, A>
where
    S: Clone + PartialEq + 'static,
    A: 'static,
{
    pub fn get(&self) -> S {
        self.pair.state.get_untracked()
    }

    pub fn dispatch(&self, action: A) {
        let next = (self.pair.reducer)(self.pair.state.get_untracked(), action);
        self.pair.state.set(next);
    }

    pub fn deps(&self) -> u64 {
        self.pair.state.id() as u64
    }
}

// ---------------------------------------------------------------------------
// useContext
// ---------------------------------------------------------------------------

/// `provide_context::<T>(value)` — sets a context value visible to subsequent
/// [`use_context`] calls. Mirrors `<Context.Provider>`.
pub fn provide_context<T: Clone + 'static>(value: T) {
    let key = std::any::type_name::<T>();
    with_host(|host| {
        host.contexts.insert(key, Box::new(value));
    });
}

/// `useContext::<T>()` — returns the most-recently-provided value of type
/// `T`, or the supplied default.
pub fn use_context<T: Clone + 'static>(default: impl FnOnce() -> T) -> T {
    let (id, idx) = current_frame_cursor();
    ensure_slot(id, idx, || HookSlot::Context);
    let _slot_taken = with_slot(id, idx, |_| ());

    let key = std::any::type_name::<T>();
    let from_provider = with_host(|host| {
        host.contexts
            .get(key)
            .and_then(|boxed| boxed.downcast_ref::<T>().cloned())
    });
    from_provider.unwrap_or_else(default)
}

// ---------------------------------------------------------------------------
// Utility re-exports
// ---------------------------------------------------------------------------

pub use w3cos_core::{Effect as ReactiveEffect, Signal as ReactiveSignal};

/// `flushSync(fn)` — run `fn` inside a reactive batch so that multiple
/// state updates collapse into a single re-render notification.
pub fn flush_sync(f: impl FnOnce()) {
    batch(f);
}

/// Convenience: pretend to be a top-level component for ad-hoc tests / demos.
pub fn render<F: FnOnce()>(id: u64, f: F) {
    let cid = ComponentId(id);
    begin_render(cid);
    f();
    end_render(cid);
}

/// Reset all hook state (tests).
pub fn reset_all() {
    let cleanups = with_host(|host| {
        host.dirty.clear();
        host.active.clear();
        host.contexts.clear();
        host.components
            .drain()
            .flat_map(|(_, c)| c.pending_cleanups.into_iter())
            .collect::<Vec<_>>()
    });
    for cleanup in cleanups {
        cleanup();
    }
}

#[allow(unused)]
fn _capture_unused_imports() {
    // Silences unused-import warnings while keeping the explicit `Effect`
    // re-export documented above.
    let _ = std::mem::size_of::<Effect>();
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static TEST_GUARD: Mutex<()> = Mutex::new(());

    fn fresh() -> std::sync::MutexGuard<'static, ()> {
        let guard = TEST_GUARD.lock().unwrap();
        reset_all();
        guard
    }

    #[test]
    fn use_state_persists_across_renders() {
        let _g = fresh();
        let id = ComponentId(1);

        begin_render(id);
        let s = use_state::<i32>(|| 0);
        s.set(42);
        end_render(id);

        begin_render(id);
        let s2 = use_state::<i32>(|| 999);
        let val: i32 = s2.get();
        assert_eq!(val, 42, "useState must keep its value across renders");
        end_render(id);
    }

    #[test]
    fn setting_state_marks_dirty() {
        let _g = fresh();
        let id = ComponentId(2);
        begin_render(id);
        let s = use_state::<i32>(|| 0);
        end_render(id);
        let _ = take_dirty();

        s.set(10);
        let dirty = take_dirty();
        assert!(dirty.contains(&id));
    }

    #[test]
    fn use_effect_runs_on_dep_change() {
        let _g = fresh();
        let id = ComponentId(3);
        let runs: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));

        let logs1 = runs.clone();
        begin_render(id);
        let s = use_state::<i32>(|| 1);
        let v: i32 = s.get();
        use_effect(
            move || {
                logs1.borrow_mut().push("first".into());
                None
            },
            &[s.deps(), v as u64],
        );
        end_render(id);

        let logs2 = runs.clone();
        begin_render(id);
        let s = use_state::<i32>(|| 1);
        let v: i32 = s.get();
        use_effect(
            move || {
                logs2.borrow_mut().push("second".into());
                None
            },
            &[s.deps(), v as u64],
        );
        end_render(id);

        // Same deps → effect should NOT re-run.
        assert_eq!(runs.borrow().len(), 1);

        // Change a dep value: now we expect the effect to fire again.
        s.set(2);

        let logs3 = runs.clone();
        begin_render(id);
        let s = use_state::<i32>(|| 1);
        let v: i32 = s.get();
        use_effect(
            move || {
                logs3.borrow_mut().push("third".into());
                None
            },
            &[s.deps(), v as u64],
        );
        end_render(id);
        assert_eq!(runs.borrow().len(), 2);
    }

    #[test]
    fn use_memo_skips_when_deps_unchanged() {
        let _g = fresh();
        let id = ComponentId(4);
        let calls = Rc::new(RefCell::new(0usize));

        for _ in 0..3 {
            let calls = calls.clone();
            begin_render(id);
            let _ = use_state::<i32>(|| 0);
            let m = use_memo(
                move || {
                    *calls.borrow_mut() += 1;
                    7 * 6
                },
                &[1, 2, 3],
            );
            assert_eq!(m.get(), 42);
            end_render(id);
        }
        assert_eq!(*calls.borrow(), 1, "memo recomputed despite stable deps");
    }

    #[test]
    fn use_ref_is_stable() {
        let _g = fresh();
        let id = ComponentId(5);

        begin_render(id);
        let r = use_ref::<Vec<i32>>(Vec::new);
        r.with_mut(|v| v.push(7));
        end_render(id);

        begin_render(id);
        let r2 = use_ref::<Vec<i32>>(Vec::new);
        let len = r2.with(|v| v.len());
        assert_eq!(len, 1);
        end_render(id);
    }

    #[test]
    fn reducer_dispatch() {
        let _g = fresh();
        let id = ComponentId(6);
        begin_render(id);
        let r = use_reducer::<i32, i32, _>(|state, action| state + action, || 10);
        r.dispatch(5);
        r.dispatch(2);
        assert_eq!(r.get(), 17);
        end_render(id);
    }

    #[test]
    fn context_propagation() {
        let _g = fresh();
        provide_context::<&'static str>("dark-theme");

        let id = ComponentId(7);
        begin_render(id);
        let theme = use_context::<&'static str>(|| "default");
        assert_eq!(theme, "dark-theme");
        end_render(id);
    }

    #[test]
    fn unmount_cleans_state() {
        let _g = fresh();
        let id = ComponentId(8);
        begin_render(id);
        let _s = use_state::<u8>(|| 1);
        end_render(id);
        assert_eq!(mounted_count(), 1);
        unmount(id);
        assert_eq!(mounted_count(), 0);
    }

    #[test]
    fn flush_sync_batches_updates() {
        let _g = fresh();
        let id = ComponentId(9);
        begin_render(id);
        let s = use_state::<i32>(|| 0);
        end_render(id);

        flush_sync(|| {
            s.set(1);
            s.set(2);
            s.set(3);
        });
        let final_val: i32 = s.get();
        assert_eq!(final_val, 3);
    }
}
